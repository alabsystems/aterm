// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RELEASE CHANNEL: policy and pure decisions for the `publish` pipeline step,
//! which puts each cut onto the one release object installed copies read.
//!
//! ## One release object per version
//!
//! Releases are cut from the private dev repository — the ledger claim, the
//! lease/fence protocol and the annotated tag all live on `[workspace.package]
//! repository` — but they are PUBLISHED once, straight onto the public channel named
//! by one tracked key, `[workspace.metadata.aterm] update_channel`: the same key
//! `crates/aterm-update-core/build.rs` compiles into every client as
//! `DEFAULT_OWNER`/`DEFAULT_REPO`, so publisher and client read one value (the test
//! `the_channel_is_exactly_the_one_clients_compiled_in` proves they agree). The engine
//! (`pub publish`) creates that version's release first, as a SOURCE prerelease; the
//! cut adopts it and completes it (owner ruling R4, 2026-09-23). Until 2026-09-26 the
//! cut published a second object first — a draft on the private origin, flipped,
//! archived and verified there — and then MIRRORED it here; that leg is gone.
//!
//! ## What the release must get exactly right
//!
//! The updater's election rule is unforgiving, and every one of its requirements
//! is a NAME requirement. It keeps non-draft releases whose tag is canonical
//! `vMAJOR.MINOR.PATCH`, needs EXACTLY ONE asset named `aterm-appcast.toml`, and
//! then needs exactly one asset named `aterm-<version>.dmg` where `<version>` is
//! derived from the TAG and cross-checked against the manifest's own `dmg` field,
//! plus exactly one `aterm-<version>-mac.zip` (the container it actually stages
//! from) cross-checked against the manifest's `zip` field the same way.
//! A release that carried, say, `aterm.dmg` alone, or two appcasts, or sat under
//! a two-component tag, would produce a channel that is live, plausible, and
//! permanently unelectable — the exact silent-never-updates failure this whole
//! effort exists to remove. [`required_asset_names`] is that rule as data, and
//! [`validate_channel_asset_set`] is enforced against the REAL remote listing
//! before the release is ever made the head.
//!
//! ## What is deliberately NOT on the channel
//!
//! Exactly the client-required set plus the human-required `.sha256` sidecars (and the
//! native Linux executables a cut declares) goes onto the public release. The owner's
//! debugging aids — the provenance record ([`provenance_asset_name`]) and the dSYM
//! archive (`aterm-<version>-dSYM.zip`, the build's) — stay in the cutting machine's
//! `dist/`: no client reads either, a public channel carries the smallest surface that
//! satisfies the updater, and the dSYM's DWARF names the build host's absolute source
//! and compile directories, home directory included. Keeping them out also keeps
//! [`validate_channel_asset_set`] a total check with no "and maybe some extras" hole:
//! either name on the channel is refused as an unexpected object.
//!
//! RETIRED 2026-08-26 (owner direction: ONE lean self-provisioning macOS
//! download): the batteries-included seed, the Intel `aterm-<v>-x86_64.dmg`
//! variant, the `aterm-<v>-lite.dmg` lean twin and the `aterm-offline.dmg`
//! alias. The bare `aterm-<v>.dmg` name survives because the deployed updater
//! binds it exactly (`aterm-update/src/github.rs` `authoritative_dmg_index`),
//! but the bytes behind it are now the lean app — nothing ever required them
//! to be seeded.

use std::path::{Path, PathBuf};

use crate::ledger::{Error, Result};
use crate::manifest_out;
use crate::publish::{DurablePostDecision, durable_post_decision};

/// The workspace-manifest table holding release-channel policy.
pub const CHANNEL_TABLE: &str = "[workspace.metadata.aterm]";

/// The key inside [`CHANNEL_TABLE`] naming the release channel.
pub const CHANNEL_KEY: &str = "update_channel";

/// `OWNER/REPO` of the release channel, from `[workspace.metadata.aterm]
/// update_channel` in the WORKSPACE manifest.
///
/// `Ok(None)` means the key is absent, which is a legal configuration: clients then
/// fall back to `[workspace.package] repository`, and so does the cut
/// (`publish::channel_slug`). A key that is PRESENT but not a clean `OWNER/REPO` is
/// an error, not a fall-through — a typo here would silently ship binaries pointed at
/// one channel while the cutter publishes to another, and the whole point of the
/// single key is that it cannot drift.
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
    /// The channel has no readable workspace manifest — an empty repo, or a
    /// source-less channel. There is nothing to disagree with, so this is not a
    /// failure; the caller reports it and proceeds.
    NoManifest,
}

/// Refuse a cut whose version the public channel's source does not carry.
///
/// This is the reconciliation the two-publisher model was missing. Source is
/// published by `pub` (staging -> `alabsystems/<repo>` main + annotated tag) and
/// binaries by `targo --unverified ship cut`, and until this gate NOTHING compared the two.
/// The observed consequence: `v0.6.0`'s tag came to rest on a tree still
/// carrying `0.5.0`, and its appcast named a commit that does not exist in the
/// public repository at all. A user who trusts the tag downloads source that
/// cannot have produced the binary beside it.
///
/// So the ordering is now enforced, not merely documented: **promote the source
/// first, then cut**. The gate runs pre-claim, so a mismatch costs seconds and
/// burns no ledger number.
///
/// Deliberately an EQUALITY check, not `>=`. A channel ahead of the cut is just
/// as broken as one behind — it means source for a version that has no binary,
/// and it is the exact state a half-finished publish leaves behind.
pub fn check_channel_version(
    cut_version: &str,
    channel_cargo_toml: &str,
) -> Result<ChannelVersion> {
    let trimmed = channel_cargo_toml.trim();
    if trimmed.is_empty() {
        return Ok(ChannelVersion::NoManifest);
    }
    // A missing/unparseable `[workspace.package] version` in a manifest that DOES
    // exist is a real disagreement, not a skip: the channel is carrying something
    // this cutter cannot reason about, and silently proceeding is what produced
    // the drift in the first place.
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

/// Read one `key = "value"` string out of one line-oriented `[table]`.
///
/// Same minimal shape as the sibling parser in `aterm-update-core/build.rs`
/// (which cannot share code with this crate — it runs before it). Both accept
/// only `key = "value"` on its own line inside the header, so the two readers
/// cannot disagree about which spelling of the key counts.
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

/// Exactly two non-empty GitHub-name segments. Mirrors `is_valid_slug` in
/// `aterm-update-core::source`, which re-validates the same value at runtime in
/// the client: a slug that would be rejected there must never be published to.
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

/// The canonical DMG asset name for a release version. The deployed client
/// derives this same string from the TAG and refuses the release if the
/// manifest's `dmg` field disagrees, so it is a name the cut cannot vary.
///
/// Since 2026-08-26 the bytes under this name are the LEAN app image — the
/// same signed, notarized bundle the updater zip carries, in the drag-install
/// DMG gesture. The name is fleet-load-bearing; the seed behind it never was.
#[must_use]
pub fn dmg_asset_name(version: &str) -> String {
    format!("aterm-{version}.dmg")
}

/// The stable, version-independent twin of the DMG every release also carries,
/// so `releases/latest/download/aterm.dmg` is a permanent direct-download URL.
/// NO client elects this name: install.sh and the in-app updater bind to the
/// manifest's version-bound `dmg` field, so the twin exists purely for the
/// browser download lane. Byte-identical to the [`dmg_asset_name`] asset of
/// the same cut, with its `.sha256` sidecar restating that digest under the
/// alias filename.
#[must_use]
pub fn stable_dmg_asset_name() -> String {
    "aterm.dmg".to_string()
}

/// The stable, version-independent twin of the updater zip, for bookmarked and
/// printed links to `releases/latest/download/aterm-mac.zip`, the lightweight
/// app-only container (the app installs its toolchain itself on first launch).
/// The alab.systems homepage button is NOT this link: `publish/site-sync.py`
/// points it at the versioned DMG of the newest release that carries one.
/// Byte-identical to the [`zip_asset_name`] asset of the same cut, exactly as
/// the DMG twin is to its canonical DMG, and like it elected by NO client —
/// the in-app updater stages from the manifest's version-bound `zip` field —
/// so it exists purely for the browser download lane.
#[must_use]
pub fn stable_zip_asset_name() -> String {
    "aterm-mac.zip".to_string()
}

/// The canonical updater-container (zip) asset name for a release version. Same
/// contract as [`dmg_asset_name`]: the client re-derives this string from the TAG
/// and refuses a manifest whose `zip` field disagrees.
#[must_use]
pub fn zip_asset_name(version: &str) -> String {
    format!("aterm-{version}-mac.zip")
}

/// The `.sha256` sidecar name for a container asset — the same `<asset>.sha256`
/// shape the Linux tarball already ships, so one verification instruction covers
/// every download on the release.
#[must_use]
pub fn sha256_sidecar_name(asset: &str) -> String {
    format!("{asset}.sha256")
}

/// The `.sha256` sidecar's entire content: `<hash>  <filename>` — TWO spaces,
/// newline-terminated — the exact record `shasum -a 256 -c` accepts. The digest
/// is the in-process one computed at packaging time (dmg.rs), so the sidecar can
/// never state anything the manifest does not.
#[must_use]
pub fn sha256_sidecar_contents(sha256: &str, asset: &str) -> String {
    format!("{sha256}  {asset}\n")
}

/// The EXACT client asset set a release must carry, sorted, as the deployed
/// updater requires it: the appcast, the version-bound DMG, the version-bound
/// updater zip, the stable `aterm.dmg` + `aterm-mac.zip` download twins, the
/// `.sha256` sidecars a human verifies every one of those containers with, and
/// — only when the cut is signed — the detached signature the pinned client
/// demands.
///
/// The zip is unconditional because every manifest this cutter emits names one,
/// and a manifest naming an asset the channel does not carry is exactly the
/// live-but-unelectable state this module exists to prevent. The stable twins
/// are unconditional for the inverse reason: every bookmarked or printed
/// `releases/latest/download/aterm-mac.zip` or `.../aterm.dmg` link 404s on any
/// release that drops them. (The homepage button links the versioned DMG, which
/// `publish/site-sync.py` picks.) The sidecars are unconditional for the humans, not
/// the updater: the containers are the manual downloads, their digests
/// otherwise live only inside the appcast TOML nobody opens, and a download
/// nobody can check is a funnel that trains people not to check. The TWIN
/// sidecars restate the same digests under the ALIAS filenames, because
/// `shasum -a 256 -c` matches on the embedded name — a versioned sidecar can
/// never verify the file a button-click actually saves.
///
/// `rostered` adds the master-signed machine roster and its signature. It belongs in
/// the CLIENT-REQUIRED set rather than the private-debugging set for a blunt reason:
/// a client with the paper master pinned refuses, structurally and before any
/// artifact crypto, a release that does not carry both
/// (`aterm_update::github::authorize_by_roster` step 1). Publishing the appcast
/// without the roster would publish a channel head the whole armed fleet declines —
/// the exact failure this module exists to prevent, one tier along. It stays
/// conditional because with an unpinned master no client ever looks for them, and
/// the published set must not change by one byte while that is true.
///
/// There is exactly ONE macOS DMG in this set (RETIRED 2026-08-26: the Intel
/// `-x86_64` pair, the `-lite` twin and the `aterm-offline.dmg` alias). A
/// channel head that still carries one of those names is refused as an
/// unexpected object — a human inspects rather than the cut converging
/// `aterm.dmg` onto bytes a dead publisher minted under a different contract.
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

/// The SOURCE publish's attestation pair, allowed to share the release with the
/// binary asset set. Since the source/binary tag unification (v0.65.0), the
/// enforced order — `pub publish` first, `ship cut` second — means the channel's
/// vX.Y.0 release ALREADY carries these when the cut arrives. They are not
/// updater-elected (election binds the exact binary names), so their presence
/// cannot change what a client resolves; each is allowed AT MOST ONCE, and any
/// other foreign name is still refused outright.
pub const SOURCE_ATTESTATION_ASSETS: [&str; 2] = ["SHA256SUMS", "SHA256SUMS.sig"];

/// The provenance record's file name in `dist/` — `bundle::write_provenance`'s file.
/// Never published (see the module note).
#[must_use]
pub fn provenance_asset_name(version: &str) -> String {
    format!("aterm-{version}-build.txt")
}

/// Prove a release's REAL remote asset listing is exactly the set this cut
/// publishes — no missing name, no duplicate name (the client's
/// `unique_asset_index` refuses a duplicate outright), no extra object: the client
/// set ([`required_asset_names`]) and the native Linux executables (`linux`, each a
/// canonical name for `version`), beside at most one copy of each source attestation
/// asset.
///
/// Called against a fresh listing of the bound release before it is made the head,
/// and again after: a channel head is only useful if the election rule can actually
/// resolve it.
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
    // Split off the source-attestation pair before the exactness check: at most
    // one of each, and what remains must match the required set EXACTLY.
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
/// Every arm that is not provably ours refuses rather than mutating: the channel is
/// what the whole fleet reads, and a wrong object there is a fleet-wide event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindPlan {
    /// No exact object and no durable intent: create the draft.
    CreateDraft,
    /// A draft with our tag exists — converge assets onto it, then make it the head.
    ConvergeDraft,
    /// A visible release under our tag: the engine's source release to adopt, or our
    /// own from a previous pass. Converge onto it, then make it the head.
    ConvergeVisible,
    /// A create POST was durably issued but nothing is visible yet. Never POST
    /// again — a duplicate draft on the channel is exactly the ambiguity the
    /// one-shot protocol exists to prevent.
    AwaitVisibility,
}

/// Decide the bind from the two facts that matter: whether this journal holds the
/// durable release intent, and what is visible now.
///
/// Reuses [`durable_post_decision`], the audited one-shot rule — a lost response can
/// be discovered, never re-POSTed.
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

/// Where an asset goes in the upload order onto the channel release: every other
/// asset first (rank 0), then the appcast's detached signature (1), then the appcast
/// itself (2).
///
/// The appcast is what makes a release a channel head a client can elect, so it
/// lands after every byte it names; its signature lands before it, so no client can
/// ever read an appcast whose signature is not there yet (a client with the channel
/// key pinned refuses exactly that head). On a draft nobody can see either, and the
/// order costs nothing; on an adopted release it is the only thing standing between
/// a client and a half-published head.
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

/// May this cut adopt a visible release under its tag that its journal holds NO intent
/// for (`publish`'s `ConvergeVisible` arm without a durable release intent)? Only as the
/// engine's SOURCE release: nothing on it but the source shapes ([`is_source_shape`] —
/// the attestation pair and the roster pair the source publish uploads too), which
/// `pub publish` creates before every cut.
///
/// Anything else is some other cut's release, never this one's: every asset this cut
/// uploads goes onto a release its journal bound first, under the durable intent; a lost
/// journal's recovery reconstructs that intent (or withdraws); and a recut claims a
/// fresh build, so no earlier cut's bytes can be its. Adopting such a release would
/// publish someone else's bytes as this cut.
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

/// Which of `names` — the assets on the engine's SOURCE release this cut adopted — a
/// withdrawal takes, leaving the source release as the engine made it. With this
/// cut's journal, exactly the assets it uploaded (`uploaded`); with a lost journal,
/// every asset this version publishes. Source shapes are never taken. A lost journal
/// meeting a name this version does not publish refuses: that release is not the
/// source release this cut adopted.
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

/// THE HEAD PATCH BODY — `PATCH /repos/{channel}/releases/{id}`, the one request that
/// makes this cut's release the channel head ([`ChannelRelease::make_head`]). Every
/// field is load-bearing:
///
/// * `draft=false` publishes a draft this cut created;
/// * `prerelease=false` turns the engine's source PRERELEASE into a full release —
///   GitHub never lets a prerelease hold `latest`, so without it `make_latest` moves
///   nothing on the adopt path and the pointer gate refuses the cut;
/// * `make_latest="true"` moves `latest` to this release (a STRING enum on GitHub's
///   API, `"true" | "false" | "legacy"`, hence `-f`).
///
/// The live PATCH ([`head_patch_argv`]) and the fake GitHub in
/// `tests/it/channel_latest.rs` both read this table, so a field dropped here is dropped
/// from the tests too — and the adopt-path proof fails at the pointer gate.
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

/// Both halves of putting one cut onto its release on the public channel — the local
/// proof of `dist/` and every call that talks to GitHub — so that [`publish_on_channel`]
/// can fix their ORDER in one place and a fake GitHub can prove it
/// (`tests/it/channel_latest.rs`).
pub trait ChannelRelease {
    /// Has THIS cut's head PATCH already landed? Asked first, and it only reads: the
    /// release the journal bound is the channel's PUBLISHED app release (not a draft, not
    /// a prerelease, carrying its appcast — the state only the head PATCH produces, and
    /// the one `--abandon` refuses). When it answers yes it has bound that release.
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
    /// The HEAD half of the floors alone, read from what the channel serves NOW: the
    /// channel head (the release the evergreen pointer names) is this cut or OLDER than
    /// it. A cut owns `latest`, and never takes it from a newer release.
    fn ratchet_head(&mut self) -> Result<()>;
    /// Every floor: [`Self::ratchet_head`], then the fleet's — the carried apply floor,
    /// the monotonic build, and a machine-roster generation this cut's roster covers. A
    /// cut never makes a head of a roster the fleet has moved past.
    fn ratchet(&mut self) -> Result<()>;
    /// Bind the release object this cut publishes onto — the engine's source release,
    /// adopted, or a draft created under a durable one-shot intent — and record it in
    /// the journal. Called only after the floors passed (see [`publish_on_channel`]).
    fn bind(&mut self) -> Result<()>;
    /// THE one PATCH that makes the release the channel head: [`HEAD_PATCH`], guarded
    /// by the release capability and the release lease. Idempotent, and sent on every
    /// path — a draft this cut created, and a release it adopted.
    fn make_head(&mut self) -> Result<()>;
    /// What a credential-less client now sees: the release elected by the client's
    /// replay, the evergreen pointer naming it and serving its appcast, and every
    /// required asset downloading.
    fn prove_head(&mut self) -> Result<()>;
}

/// THE CHANNEL PUBLISH SEQUENCE, one order for every path: prove `dist/`, check the
/// fleet's floors, bind the release, upload everything with the appcast pair last
/// ([`channel_upload_rank`]), prove the asset set, check the floors again, make the
/// release the head, prove what a stranger sees — unless the head is already made.
///
/// A HEAD THE CUT MADE IS FINISHED, NEVER REFUSED (2026-09-26). The step can fail after
/// its PATCH landed — the pointer gate's five-minute bound ran out on v0.93.0 — and the
/// journal then stays at `publish` with the release already the fleet's head. Re-entering
/// the write half there was wrong twice over: a roster join since the PATCH re-dresses
/// that head with a generation this cut's signed appcast does not carry, so the floors
/// read again refuse a release every client already runs — permanently, and with no
/// exit, because `--abandon` refuses a published release and `yank` wants a finished
/// journal — and an upload would have put the older roster pair back over the join's.
/// So [`ChannelRelease::head_made`] is asked first, by the same predicate `--abandon`
/// refuses on: every state abandon refuses, the resume finishes — with only what
/// cannot refuse a head the fleet already runs: the head half of the ratchet (still
/// never over a newer release), the idempotent PATCH, and the read side. The PATCH is
/// re-sent because GitHub hands `latest` to any release published as a full release in
/// between — an `atpkg-index-<n>` cut included — and the pointer gate would otherwise
/// never pass. Nothing is uploaded, and no roster floor is read.
///
/// The floors are read BEFORE THE BIND. A cut they refuse there has touched nothing
/// and recorded nothing: no draft created, no adoption intent persisted, no channel
/// release ID in the journal — so `--abandon` finds nothing on the channel to withdraw.
///
/// The floors are read AGAIN after the proof, before the PATCH, because a roster join is
/// not lease-gated and can land while this cut uploads; the release, not yet the head,
/// is refused there and `--abandon` withdraws exactly its uploads. A join landing between
/// that read and the PATCH itself is the one window no read closes: GitHub offers no
/// conditional PATCH.
///
/// WHY THE HEAD PATCH IS ON EVERY PATH (2026-09-23). The cut OWNS GitHub's `latest`
/// pointer — the one request every credential-less updater makes, and the target of
/// the evergreen `releases/latest/download/aterm.dmg` / `aterm-mac.zip` links.
/// `pub publish` creates the same vX.Y.0 release first, as a source release, and the
/// cut ADOPTS it. The adopt path used to flip nothing ("already visible"), and passed
/// its pointer gate only because that source release had already STOLEN `latest`
/// from the previous app release — about twenty minutes before any app asset
/// existed, during which every updater 302'd to a release with no appcast. The
/// engine now creates that source release as a prerelease, which can never hold
/// `latest`; without this PATCH on the adopt path the pointer would stay on the
/// previous release and the gate would refuse every cut.
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

    /// PINS THE SHARED TAG. The source publish and the binary cut meet on one
    /// vX.Y.0 release since v0.65.0, so the pub attestation pair rides beside
    /// the elected set — at most once each, with every other foreign name (and
    /// any duplicate) still refused. The first shared-tag cut failed exactly
    /// here, with the release complete and the cut refusing its own tag.
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

    /// PINS THE ONE-DMG SHAPE. The bare `aterm-<v>.dmg` spelling is
    /// fleet-load-bearing (the deployed updater and install.sh bind it
    /// exactly) and it is the ONLY macOS DMG a cut publishes: the retired
    /// Intel `-x86_64` variant, the `-lite` twin and the `aterm-offline.dmg`
    /// alias must never re-enter the required set under any flag, and a
    /// channel head still carrying one of them is refused as a foreign
    /// object rather than converged.
    #[test]
    fn the_required_set_carries_exactly_one_dmg_and_no_retired_names() {
        assert_eq!(dmg_asset_name("0.62.0"), "aterm-0.62.0.dmg");
        assert_eq!(stable_dmg_asset_name(), "aterm.dmg");
        assert_eq!(stable_zip_asset_name(), "aterm-mac.zip");

        for (signed, rostered) in [(false, false), (true, false), (true, true)] {
            let set = required_asset_names("0.62.0", signed, rostered);
            let dmgs: Vec<&String> = set.iter().filter(|n| n.ends_with(".dmg")).collect();
            assert_eq!(
                dmgs,
                vec!["aterm-0.62.0.dmg", "aterm.dmg"],
                "exactly the canonical DMG and its evergreen twin: {set:?}"
            );
            assert!(
                !set.iter().any(|n| {
                    n.contains("x86_64") || n.contains("lite") || n.contains("offline")
                }),
                "a retired container name re-entered the set: {set:?}"
            );
        }

        let ok = required_asset_names("0.62.0", true, false);
        for retired in [
            "aterm-0.62.0-x86_64.dmg",
            "aterm-0.62.0-x86_64.dmg.sha256",
            "aterm-0.62.0-lite.dmg",
            "aterm-0.62.0-lite.dmg.sha256",
            "aterm-offline.dmg",
            "aterm-offline.dmg.sha256",
        ] {
            let mut stale = ok.clone();
            stale.push(retired.to_string());
            let err = validate_channel_asset_set(&stale, "0.62.0", true, false, &[])
                .expect_err("a retired container on the channel head is a foreign object");
            assert!(err.to_string().contains(retired), "{err}");
        }
    }

    /// The drift this gate exists to stop, in its exact observed form: the tag
    /// `v0.6.0` came to rest on a public tree still carrying `0.5.0`.
    #[test]
    fn a_channel_behind_the_cut_is_refused_with_the_publish_remedy() {
        let channel = "[workspace.package]\nversion = \"0.5.0\"\n";
        let err = check_channel_version("0.6.0", channel).expect_err("must refuse");
        let msg = err.to_string();
        assert!(msg.contains("0.6.0") && msg.contains("0.5.0"), "{msg}");
        // The refusal is only useful if it names the fix.
        assert!(
            msg.contains("pub stage aterm && pub publish aterm"),
            "{msg}"
        );
    }

    /// Equality, not `>=`. A channel AHEAD means source exists for a version with
    /// no binary — the residue of a half-finished publish, and just as wrong.
    #[test]
    fn a_channel_ahead_of_the_cut_is_refused_too() {
        let channel = "[workspace.package]\nversion = \"0.9.0\"\n";
        assert!(check_channel_version("0.8.0", channel).is_err());
    }

    #[test]
    fn agreement_is_exact_string_equality_of_the_workspace_version() {
        let channel = "[workspace.package]\nversion = \"0.8.0\"\nedition = \"2024\"\n";
        assert_eq!(
            check_channel_version("0.8.0", channel).unwrap(),
            ChannelVersion::Agrees
        );
    }

    /// An EMPTY body is the only skip. Distinguishing this from "unreadable" is the
    /// point: an empty channel legitimately has nothing to compare, whereas a
    /// manifest we cannot parse is a channel whose version is unknown — and
    /// proceeding on an unknown version is precisely the original bug.
    #[test]
    fn an_empty_channel_manifest_skips_but_an_unparseable_one_refuses() {
        assert_eq!(
            check_channel_version("0.8.0", "   \n\n").unwrap(),
            ChannelVersion::NoManifest
        );
        assert!(
            check_channel_version("0.8.0", "[workspace]\nmembers = []\n").is_err(),
            "a real manifest with no [workspace.package] version must NOT be treated as absent"
        );
    }

    /// The gate compares the channel against the version being cut, so it must read
    /// the CHANNEL's manifest — not fall back to this repo's. Feeding it our own
    /// manifest and a deliberately different version has to fail.
    #[test]
    fn the_gate_reads_the_channel_manifest_not_the_local_one() {
        let ours = crate::publish::workspace_version(REAL_MANIFEST).unwrap();
        let bumped = format!(
            "{}99.0",
            ours.trim_end_matches(|c: char| c.is_ascii_digit())
        );
        assert_ne!(bumped, ours);
        assert!(check_channel_version(&bumped, REAL_MANIFEST).is_err());
        assert_eq!(
            check_channel_version(&ours, REAL_MANIFEST).unwrap(),
            ChannelVersion::Agrees
        );
    }

    #[test]
    fn the_channel_is_exactly_the_one_clients_compiled_in() {
        // THE binding this whole design rests on. `aterm-update-core/build.rs`
        // stamps `[workspace.metadata.aterm] update_channel` into every client
        // as DEFAULT_OWNER/DEFAULT_REPO; the cutter reads the same key to pick
        // its release channel. If these ever disagree, the pipeline would publish
        // to a channel no shipped binary reads — live, plausible, and silently
        // never updating. `aterm-release` depends on `aterm-update-core`, so
        // the constants below are the ACTUAL compiled-in values, not a copy.
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
    fn the_channel_is_never_pointed_back_at_the_private_staging_repo() {
        // The channel is a separate repository precisely because the dev repo is
        // private and unreadable without a credential. Pointing the channel back at
        // it would restore the "fresh machine never updates" bug — and the cut's
        // own preflight would refuse the private channel only at cut time.
        //
        // Scoped to the private staging namespace on purpose: `publish/` exports
        // a PUBLIC source snapshot that rewrites the staging owner into the
        // public org throughout, so in that tree `repository` and
        // `update_channel` legally coincide — one public repo serving both
        // source and releases, which is the documented "no separate channel"
        // configuration, not a regression.
        //
        // The scope test must therefore contain NO rewritable literal: guarding
        // on the staging owner's spelling would be rewritten into the public
        // org, flip to always-true in the exported tree, and deterministically
        // fail there. "alabsystems" is a FIXED POINT of that rewrite, so
        // "publish owner is not the public org" identifies the private staging
        // tree in both snapshots. Not `DEFAULT_OWNER` on purpose: build.rs
        // stamps that from `update_channel`, so it MOVES WITH the exact
        // regression this test names (channel repointed at the private repo
        // recompiles DEFAULT_OWNER to the private owner and would skip the
        // guard); the fixed literal keeps the tripwire live in that world.
        let channel = update_channel_slug(REAL_MANIFEST).unwrap().unwrap();
        let publish = crate::publish::repo_slug(REAL_MANIFEST).unwrap();
        let publish_owner = publish.split('/').next().unwrap_or("");
        if publish_owner != "alabsystems" {
            assert_ne!(
                channel, publish,
                "the private staging repo must never be the update channel"
            );
        }
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

    /// REGRESSION: the channel anchor has exactly ONE home.
    ///
    /// It used to live in BOTH `[workspace.metadata.aterm] update_channel_pubkey`
    /// and (after the pins refactor) `aterm_update_core::pins`. Two separately
    /// edited committed values that nothing compared: editing one and not the other
    /// yields releases signed by a key no client accepts, silently. The manifest key
    /// is gone; this test fails if it comes back.
    #[test]
    fn the_channel_anchor_lives_only_in_pins() {
        assert!(
            !REAL_MANIFEST.contains("update_channel_pubkey"),
            "the channel anchor must live only in aterm_update_core::pins, \
             never also in Cargo.toml"
        );
        assert!(
            aterm_update_core::pins::roster_tier_armed(),
            "the public channel's anchor — the paper master — is pinned in pins.rs"
        );
    }

    #[test]
    fn required_asset_set_is_exactly_what_the_updater_elects() {
        // Unsigned (Tier REPO, the default): appcast + version-bound DMG + the
        // version-bound zip the in-app updater actually stages from + the two
        // stable download twins the evergreen website URLs point at + the
        // `.sha256` sidecars a human verifies every one of those with (the
        // twin sidecars embed the ALIAS names, so `shasum -c` accepts them
        // against what a button-click actually saves).
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
        // Signed (Tier SIG): a pinned client REFUSES a head with no .sig.
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
        // The names are the client's literals, not a lookalike.
        assert_eq!(manifest_out::MANIFEST_ASSET, "aterm-appcast.toml");
        assert_eq!(manifest_out::MANIFEST_SIG_ASSET, "aterm-appcast.toml.sig");
        assert_eq!(dmg_asset_name("1.2.3"), "aterm-1.2.3.dmg");
        assert_eq!(zip_asset_name("1.2.3"), "aterm-1.2.3-mac.zip");
        assert_eq!(
            sha256_sidecar_name(&dmg_asset_name("1.2.3")),
            "aterm-1.2.3.dmg.sha256"
        );
        assert_eq!(stable_dmg_asset_name(), "aterm.dmg");
        // The twin names are VERSION-FREE by construction — that is the whole
        // evergreen-URL property — and their sidecars embed exactly them.
        assert_eq!(stable_zip_asset_name(), "aterm-mac.zip");
        assert_eq!(
            sha256_sidecar_name(&stable_zip_asset_name()),
            "aterm-mac.zip.sha256"
        );
        assert_eq!(
            sha256_sidecar_name(&stable_dmg_asset_name()),
            "aterm.dmg.sha256"
        );
    }

    /// The sidecar is only worth publishing if `shasum -a 256 -c` accepts it:
    /// hex digest, TWO spaces, exact asset name, one trailing newline.
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

        // Every way a plausible-looking release silently never updates:
        let cases: Vec<(Vec<&str>, &str, bool, &str)> = vec![
            // no appcast at all -> the release is skipped by selection
            (
                vec![
                    "aterm-0.5.0.dmg",
                    "aterm-0.5.0.dmg.sha256",
                    "aterm-0.5.0-mac.zip",
                    "aterm-0.5.0-mac.zip.sha256",
                    "aterm.dmg",
                    "aterm.dmg.sha256",
                    "aterm-mac.zip",
                    "aterm-mac.zip.sha256",
                ],
                "0.5.0",
                false,
                "aterm-appcast.toml",
            ),
            // two appcasts -> `unique_asset_index` refuses the release
            (
                vec![
                    "aterm-appcast.toml",
                    "aterm-appcast.toml",
                    "aterm-0.5.0.dmg",
                    "aterm-0.5.0.dmg.sha256",
                    "aterm-0.5.0-mac.zip",
                    "aterm-0.5.0-mac.zip.sha256",
                    "aterm.dmg",
                    "aterm.dmg.sha256",
                    "aterm-mac.zip",
                    "aterm-mac.zip.sha256",
                ],
                "0.5.0",
                false,
                "duplicated",
            ),
            // DMG named for the WRONG version -> manifest/tag disagreement
            (
                vec![
                    "aterm-appcast.toml",
                    "aterm-0.61.0.dmg",
                    "aterm-0.5.0.dmg.sha256",
                    "aterm-0.5.0-mac.zip",
                    "aterm-0.5.0-mac.zip.sha256",
                    "aterm.dmg",
                    "aterm.dmg.sha256",
                    "aterm-mac.zip",
                    "aterm-mac.zip.sha256",
                ],
                "0.5.0",
                false,
                "aterm-0.5.0.dmg",
            ),
            // the stable twin alone cannot substitute for the version-bound
            // name the manifest elects -> the canonical DMG is still missing
            (
                vec![
                    "aterm-appcast.toml",
                    "aterm.dmg",
                    "aterm.dmg.sha256",
                    "aterm-0.5.0.dmg.sha256",
                    "aterm-0.5.0-mac.zip",
                    "aterm-0.5.0-mac.zip.sha256",
                    "aterm-mac.zip",
                    "aterm-mac.zip.sha256",
                ],
                "0.5.0",
                false,
                "aterm-0.5.0.dmg",
            ),
            // the stable download twin never crossed -> every printed/bookmarked
            // releases/latest/download/aterm.dmg link 404s on this head
            (
                vec![
                    "aterm-appcast.toml",
                    "aterm-0.5.0.dmg",
                    "aterm-0.5.0-mac.zip",
                ],
                "0.5.0",
                false,
                "aterm.dmg",
            ),
            // the PRIMARY evergreen download never crossed -> the alab.systems
            // homepage's single releases/latest/download/aterm-mac.zip button
            // 404s on this head
            (
                vec![
                    "aterm-appcast.toml",
                    "aterm-0.5.0.dmg",
                    "aterm-0.5.0.dmg.sha256",
                    "aterm-0.5.0-mac.zip",
                    "aterm-0.5.0-mac.zip.sha256",
                    "aterm.dmg",
                    "aterm.dmg.sha256",
                    "aterm-mac.zip.sha256",
                ],
                "0.5.0",
                false,
                "aterm-mac.zip",
            ),
            // a twin without its ALIAS sidecar -> the documented shasum -c
            // one-liner has no record naming the file the button actually saved
            (
                vec![
                    "aterm-appcast.toml",
                    "aterm-0.5.0.dmg",
                    "aterm-0.5.0.dmg.sha256",
                    "aterm-0.5.0-mac.zip",
                    "aterm-0.5.0-mac.zip.sha256",
                    "aterm.dmg",
                    "aterm.dmg.sha256",
                    "aterm-mac.zip",
                ],
                "0.5.0",
                false,
                "aterm-mac.zip.sha256",
            ),
            // the updater container never crossed -> the manifest names a zip the
            // channel does not carry, and every in-app stage 404s
            (
                vec![
                    "aterm-appcast.toml",
                    "aterm-0.5.0.dmg",
                    "aterm-0.5.0.dmg.sha256",
                    "aterm-0.5.0-mac.zip.sha256",
                    "aterm.dmg",
                    "aterm.dmg.sha256",
                    "aterm-mac.zip",
                    "aterm-mac.zip.sha256",
                ],
                "0.5.0",
                false,
                "aterm-0.5.0-mac.zip",
            ),
            // zip named for the WRONG version -> same, with a plausible decoy
            (
                vec![
                    "aterm-appcast.toml",
                    "aterm-0.5.0.dmg",
                    "aterm-0.5.0.dmg.sha256",
                    "aterm-0.61.0-mac.zip",
                    "aterm-0.5.0-mac.zip.sha256",
                    "aterm.dmg",
                    "aterm.dmg.sha256",
                    "aterm-mac.zip",
                    "aterm-mac.zip.sha256",
                ],
                "0.5.0",
                false,
                "aterm-0.5.0-mac.zip",
            ),
            // a container without its sidecar -> the human download the release
            // page advertises cannot be verified the way the notes instruct
            (
                vec![
                    "aterm-appcast.toml",
                    "aterm-0.5.0.dmg",
                    "aterm-0.5.0-mac.zip",
                    "aterm-0.5.0-mac.zip.sha256",
                    "aterm.dmg",
                    "aterm.dmg.sha256",
                    "aterm-mac.zip",
                    "aterm-mac.zip.sha256",
                ],
                "0.5.0",
                false,
                "aterm-0.5.0.dmg.sha256",
            ),
            // signed cut whose signature never crossed -> pinned clients refuse
            (
                vec![
                    "aterm-appcast.toml",
                    "aterm-0.5.0.dmg",
                    "aterm-0.5.0.dmg.sha256",
                    "aterm-0.5.0-mac.zip",
                    "aterm-0.5.0-mac.zip.sha256",
                    "aterm.dmg",
                    "aterm.dmg.sha256",
                    "aterm-mac.zip",
                    "aterm-mac.zip.sha256",
                ],
                "0.5.0",
                true,
                "aterm-appcast.toml.sig",
            ),
            // private-only artifacts leaking into the public set
            (
                vec![
                    "aterm-appcast.toml",
                    "aterm-0.5.0.dmg",
                    "aterm-0.5.0.dmg.sha256",
                    "aterm-0.5.0-mac.zip",
                    "aterm-0.5.0-mac.zip.sha256",
                    "aterm.dmg",
                    "aterm.dmg.sha256",
                    "aterm-mac.zip",
                    "aterm-mac.zip.sha256",
                    "aterm-0.5.0-dSYM.zip",
                ],
                "0.5.0",
                false,
                "aterm-0.5.0-dSYM.zip",
            ),
        ];
        for (names, version, signed, needle) in cases {
            let names: Vec<String> = names.into_iter().map(str::to_string).collect();
            let err = validate_channel_asset_set(&names, version, signed, false, &[])
                .expect_err(&format!("{names:?} must be refused"));
            assert!(
                err.to_string().contains(needle),
                "error for {names:?} should name {needle:?}, got: {err}"
            );
        }
    }

    #[test]
    fn bind_plan_never_reissues_a_durable_post() {
        assert_eq!(bind_plan(false, None), BindPlan::CreateDraft);
        assert_eq!(bind_plan(false, Some(true)), BindPlan::ConvergeDraft);
        assert_eq!(bind_plan(false, Some(false)), BindPlan::ConvergeVisible);
        // The whole point: intent issued + nothing visible is NOT a retry.
        assert_eq!(bind_plan(true, None), BindPlan::AwaitVisibility);
        assert_eq!(bind_plan(true, Some(true)), BindPlan::ConvergeDraft);
        assert_eq!(bind_plan(true, Some(false)), BindPlan::ConvergeVisible);
    }

    /// THE DEBUGGING AIDS NEVER JOIN THE PUBLIC SET. The provenance record and the dSYM
    /// archive (whose DWARF names the build host's paths) are refused on the channel as
    /// unexpected objects, and no reading of the version — not even a lost journal's
    /// widest — calls either name this version's to publish.
    #[test]
    fn the_debugging_aids_never_join_the_channel_set() {
        let aids = [
            provenance_asset_name("0.5.0"),
            "aterm-0.5.0-dSYM.zip".to_string(),
        ];
        assert_eq!(
            aids[0], "aterm-0.5.0-build.txt",
            "the name bundle.rs writes into dist/"
        );
        let publishable = publishable_asset_names("0.5.0");
        for aid in &aids {
            assert!(!publishable.contains(aid), "{aid} is publishable");
            let mut set = required_asset_names("0.5.0", true, true);
            set.push(aid.clone());
            let err = validate_channel_asset_set(&set, "0.5.0", true, true, &[])
                .expect_err("an aid on the channel is an unexpected object");
            assert!(err.to_string().contains(aid.as_str()), "{err}");
        }
    }

    /// What a lost journal can still decide: a release is this version's when it holds
    /// nothing a cut of this version does not publish, and a withdrawal takes exactly
    /// what the cut put there — the journal's uploads, or every app asset of this
    /// version — and never a source shape.
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

        // With the journal: exactly its uploads, minus source shapes (a roster the cut
        // replaced stays — the source release keeps a roster pair).
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
        let why = withdrawable_assets(&foreign, v, None).unwrap_err();
        assert!(why.contains("aterm-0.94.0.dmg"), "{why}");
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
