// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The public update-channel MIRROR: policy and pure decisions for the `mirror`
//! pipeline step, which copies each finished, verified release from the private
//! publish repo to the public channel installed copies actually read.
//!
//! ## Why a mirror at all
//!
//! Releases are cut PRIVATELY — the ledger claim, the lease/fence protocol, the
//! draft-first upload and the whole verify chain all run against
//! `[workspace.package] repository`. But a private repo's Releases API needs a
//! credential, and nothing provisions one on a fresh machine, so a shipped build
//! pointed at the private repo can never see an update. The channel installed
//! copies read is therefore a separate, PUBLIC repo named by one tracked key,
//! `[workspace.metadata.aterm] update_channel` — the same key
//! `crates/aterm-update-core/build.rs` compiles into every client as
//! `DEFAULT_OWNER`/`DEFAULT_REPO`. Publisher and client read one value; the test
//! `mirror_target_is_exactly_the_channel_clients_compiled_in` proves they agree.
//!
//! ## What the mirror must get exactly right
//!
//! The updater's election rule is unforgiving, and every one of its requirements
//! is a NAME requirement. It keeps non-draft releases whose tag is canonical
//! `vMAJOR.MINOR.PATCH`, needs EXACTLY ONE asset named `aterm-appcast.toml`, and
//! then needs exactly one asset named `aterm-<version>.dmg` where `<version>` is
//! derived from the TAG and cross-checked against the manifest's own `dmg` field,
//! plus exactly one `aterm-<version>-mac.zip` (the container it actually stages
//! from) cross-checked against the manifest's `zip` field the same way.
//! A mirror that uploaded, say, `aterm.dmg`, or two appcasts, or a release under
//! a two-component tag, would produce a channel that is live, plausible, and
//! permanently unelectable — the exact silent-never-updates failure this whole
//! effort exists to remove. [`required_asset_names`] is that rule as data, and
//! [`validate_mirror_asset_set`] is enforced against the REAL remote listing
//! before the mirrored draft is ever flipped visible.
//!
//! ## What is deliberately NOT mirrored
//!
//! Exactly the client-required set plus the human-required `.sha256` sidecars
//! crosses over: the manifest, the DMG (the per-arch pair, when the manifest
//! names an Intel DMG), the updater zip, the lite DMG and its `aterm-offline.dmg`
//! companion alias (on a cut that produced one — see [`dmg_lite_asset_name`]),
//! the stable download twins (`aterm.dmg`, `aterm-mac.zip`), every container's
//! and twin's sidecar, and the detached signature when the cut is signed. The
//! provenance text and the dSYM archive stay private — they are debugging aids
//! for the owner, the client never reads them, and a public channel should carry
//! the smallest surface that still satisfies the updater. Keeping the set exact
//! also makes [`validate_mirror_asset_set`] a total check (no "and maybe some
//! extras" hole).

use crate::ledger::{Error, Result};
use crate::manifest_out;
use crate::publish::{DurablePostDecision, durable_post_decision};

/// The workspace-manifest table holding release-channel policy.
pub const CHANNEL_TABLE: &str = "[workspace.metadata.aterm]";

/// The key inside [`CHANNEL_TABLE`] naming the public update channel.
pub const CHANNEL_KEY: &str = "update_channel";

/// `OWNER/REPO` of the public update channel, from `[workspace.metadata.aterm]
/// update_channel` in the WORKSPACE manifest.
///
/// `Ok(None)` means the key is absent, which is a legal configuration: there is
/// no public mirror, clients fall back to `[workspace.package] repository`, and
/// the `mirror` step announces itself as a no-op. A key that is PRESENT but not a
/// clean `OWNER/REPO` is an error, not a fall-through — a typo here would
/// silently ship binaries pointed at one channel while the cutter mirrors to
/// another, and the whole point of the single key is that it cannot drift.
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
/// binaries by `cargo ship cut`, and until this gate NOTHING compared the two.
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
         pub stage aterm && pub promote aterm\n\
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
/// the client: a slug that would be rejected there must never be mirrored to.
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
/// manifest's `dmg` field disagrees, so it is a name the mirror cannot vary.
#[must_use]
pub fn dmg_asset_name(version: &str) -> String {
    format!("aterm-{version}.dmg")
}

/// The INTEL batteries-included DMG for a release version — the same signed,
/// notarized universal app with the toolchain seed filtered to
/// `x86_64-apple-darwin` artifacts. ADDITIVE beside [`dmg_asset_name`]: the
/// bare `aterm-<version>.dmg` spelling stays the canonical (arm64-seeded)
/// asset because the deployed fleet binds that exact name
/// (`aterm-update/src/github.rs` `authoritative_dmg_index`, `install.sh`'s
/// identity bind) — renaming it would 404 or refuse every installed client,
/// which is why the split is spelled "bare = arm64, suffixed = x86_64" and
/// never a symmetric `-arm64`/`-x86_64` pair.
#[must_use]
pub fn dmg_x86_64_asset_name(version: &str) -> String {
    format!("aterm-{version}-x86_64.dmg")
}

/// The stable, version-independent twin of the DMG every release also carries,
/// so `releases/latest/download/aterm.dmg` is a permanent direct-download URL.
/// NO client elects this name: install.sh and the in-app updater bind to the
/// manifest's version-bound `dmg` field, so the twin exists purely for the
/// browser download lane.
///
/// WHICH bytes it aliases changed once, deliberately (2026-08, product-owner
/// approved). On a cut that produces a lite DMG — every cut this cutter makes
/// — the twin is byte-identical to the [`dmg_lite_asset_name`] artifact: the
/// ~28 MB seed-stripped drag-install image, because a browser click on an
/// evergreen link is a person who wants the app, not a ~1.07 GB offline
/// toolchain payload their machine will fetch fresh anyway. The seeded image
/// keeps its version-bound [`dmg_asset_name`] spelling UNCHANGED (that name is
/// fleet-load-bearing) and gains its own explicit evergreen alias,
/// [`stable_offline_dmg_asset_name`]. On a pre-lite cut (a recovered old
/// release) the twin stays what it always was: a byte copy of the canonical
/// seeded DMG.
#[must_use]
pub fn stable_dmg_asset_name() -> String {
    "aterm.dmg".to_string()
}

/// The stable, version-independent alias of the SEEDED (offline,
/// batteries-included) DMG, carried exactly when the cut also carries a lite
/// DMG — the two repoints travel together: the moment `aterm.dmg` starts
/// serving the lean image, the offline image needs an evergreen spelling of
/// its own (`releases/latest/download/aterm-offline.dmg`), or the ~1.07 GB
/// no-network install lane loses its only unversioned URL. Byte-identical to
/// the [`dmg_asset_name`] asset of the same cut, elected by NO client, sidecar
/// under the alias name — the same rules as every other twin here.
#[must_use]
pub fn stable_offline_dmg_asset_name() -> String {
    "aterm-offline.dmg".to_string()
}

/// The stable, version-independent twin of the updater zip — the PRIMARY
/// download: the alab.systems homepage is a single evergreen button pointed at
/// `releases/latest/download/aterm-mac.zip`, the lightweight app-only
/// container (`dmg::create_zip` strips the toolchain seed; the app installs
/// its toolchain itself on first launch). Byte-identical to the
/// [`zip_asset_name`] asset of the same cut, exactly as the DMG twin is to its
/// canonical DMG, and like it elected by NO client — the in-app updater stages
/// from the manifest's version-bound `zip` field — so it exists purely for the
/// browser download lane.
///
/// The LIGHTWEIGHT DMG ([`dmg_lite_asset_name`]) landed beside these two twins
/// exactly as this doc once predicted — a name helper, membership in
/// [`required_asset_names`], and the byte-copy + alias-sidecar staging
/// `step_build`/`step_selfcheck` give the twins — with ONE deliberate
/// deviation from the prediction: membership is keyed on the cut carrying a
/// lite digest record rather than unconditional, because the signed manifest
/// names no lite container and a recovered pre-lite release must keep its
/// published byte set exactly (see [`required_asset_names`]).
#[must_use]
pub fn stable_zip_asset_name() -> String {
    "aterm-mac.zip".to_string()
}

/// The LEAN drag-install DMG for a release version: the seed-stripped app —
/// the very bundle the updater zip carries — imaged with the `/Applications`
/// symlink, signed, notarized and stapled through the same lane as the seeded
/// DMG (`dmg::create_lite`). ADDITIVE beside [`dmg_asset_name`] for the same
/// reason the Intel variant is: the bare `aterm-<version>.dmg` spelling is
/// fleet-load-bearing — the deployed updater's `authoritative_dmg_index` and
/// install.sh's identity bind (`manifest dmg != aterm-$version.dmg` is a hard
/// refusal, and its asset allowlist admits only the manifest-named container
/// shapes) both elect the versioned name as the BATTERIES-INCLUDED container
/// through the signed manifest. Flipping that name's meaning to "lean" would
/// hand every toolchain-included install a seedless image while passing every
/// digest check — so the seeded image keeps the bare name, and the lean image
/// takes this suffix, which no deployed script's allowlist matches and no
/// client ever looks up.
///
/// Deliberately NOT a manifest field: no client elects this container, and the
/// shared `Manifest` type is compiled into every deployed client — its byte
/// authority is instead the cut's own in-process post-hook digest, journaled
/// (`Journal::lite_dmg_sha256`) and restated by the `.sha256` sidecars. That is
/// also why its membership in [`required_asset_names`] is a parameter rather
/// than unconditional.
#[must_use]
pub fn dmg_lite_asset_name(version: &str) -> String {
    format!("aterm-{version}-lite.dmg")
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

/// The EXACT asset set a mirrored release must carry, sorted, as the deployed
/// updater requires it: the appcast, the version-bound DMG, the version-bound
/// updater zip, the stable `aterm.dmg` + `aterm-mac.zip` download twins, the
/// `.sha256` sidecars a human verifies every one of those containers with, and
/// — only when the cut is signed — the detached signature the pinned client
/// demands.
///
/// The zip is unconditional because every manifest this cutter emits names one,
/// and a manifest naming an asset the channel does not carry is exactly the
/// live-but-unelectable state this module exists to prevent. The stable twins
/// are unconditional for the inverse reason: the website's evergreen
/// `releases/latest/download/aterm-mac.zip` button (the homepage's ONLY
/// download) and every printed/bookmarked `.../aterm.dmg` link 404 on any
/// release that drops them. The sidecars are unconditional for the humans, not
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
/// (`aterm_update::github::authorize_by_roster` step 1). Mirroring the appcast
/// without the roster would publish a channel head the whole armed fleet declines —
/// the exact failure this module exists to prevent, one tier along. It stays
/// conditional because with an unpinned master no client ever looks for them, and
/// the mirrored set must not change by one byte while that is true.
///
/// `x86_dmg` adds the Intel DMG and its `.sha256` sidecar. It is a parameter,
/// not unconditional, for the zip's inverse reason: the pair exists exactly when
/// the cut's manifest names one (`dmg_x86_64`), and every caller derives the
/// flag FROM the manifest — so a manifest that names the asset while the channel
/// lacks it fails this exact-set check, and a channel carrying one the manifest
/// never named fails it too (an asset no manifest names is an asset no install
/// can verify a digest for).
///
/// `lite_dmg` adds the lean drag-install DMG, its `.sha256` sidecar, and the
/// `aterm-offline.dmg` alias + sidecar for the seeded image (the four names
/// travel together — see [`stable_offline_dmg_asset_name`] for why). A
/// parameter like `x86_dmg`, but keyed on a different authority: the signed
/// manifest deliberately names no lite container ([`dmg_lite_asset_name`]),
/// so every caller derives the flag from the CUT's own lite digest record
/// (`Journal::lite_dmg_sha256`). Every cut this cutter builds carries one;
/// `false` is the pre-lite shape — an old journal resumed past selfcheck, or a
/// recovery of a release published before the lane existed — whose mirrored
/// byte set must not change by one name, exactly as for the x86 pair.
#[must_use]
pub fn required_asset_names(
    version: &str,
    signed: bool,
    rostered: bool,
    x86_dmg: bool,
    lite_dmg: bool,
) -> Vec<String> {
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
    if x86_dmg {
        names.push(dmg_x86_64_asset_name(version));
        names.push(sha256_sidecar_name(&dmg_x86_64_asset_name(version)));
    }
    if lite_dmg {
        names.push(dmg_lite_asset_name(version));
        names.push(sha256_sidecar_name(&dmg_lite_asset_name(version)));
        names.push(stable_offline_dmg_asset_name());
        names.push(sha256_sidecar_name(&stable_offline_dmg_asset_name()));
    }
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

/// Prove a mirrored release's REAL remote asset listing is exactly the set the
/// client requires — no missing name, no duplicate name (the client's
/// `unique_asset_index` refuses a duplicate outright), no extra object.
///
/// Called against a fresh listing of the mirrored draft before it is flipped
/// visible, and again after the flip: a channel head is only useful if the
/// election rule can actually resolve it.
pub fn validate_mirror_asset_set(
    names: &[String],
    version: &str,
    signed: bool,
    rostered: bool,
    x86_dmg: bool,
    lite_dmg: bool,
) -> Result<()> {
    let required = required_asset_names(version, signed, rostered, x86_dmg, lite_dmg);
    let mut observed: Vec<String> = names.to_vec();
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
        "mirrored release asset set does not match what the updater elects \
         (required exactly {required:?}): missing {missing:?}, unexpected {extra:?}, \
         duplicated {duplicated:?}"
    )))
}

/// What the `mirror` step may do to the public channel for this cut.
///
/// The private release is already live and verified by the time this is
/// consulted, so the mirror never re-decides whether to ship — only how to
/// converge the copy. Every arm that is not provably ours refuses rather than
/// mutating: the public channel is what the whole fleet reads, and a wrong
/// object there is a fleet-wide event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorPlan {
    /// No exact object and no durable create intent: create the draft.
    CreateDraft,
    /// A draft with our tag exists — converge assets onto it, then flip.
    ConvergeDraft,
    /// Already published under our tag: the flip landed before the journal
    /// mark. Re-prove it carries our build, then converge silently.
    ConvergePublished,
    /// A create POST was durably issued but nothing is visible yet. Never POST
    /// again — a duplicate draft on the channel is exactly the ambiguity the
    /// one-shot protocol exists to prevent.
    AwaitVisibility,
}

/// Decide the mirror convergence action from the two facts that matter: whether
/// this journal durably issued a create POST, and what is visible now.
///
/// Reuses [`durable_post_decision`] so the mirror inherits the private side's
/// audited rule — a lost response can be discovered, never re-POSTed.
#[must_use]
pub fn mirror_plan(create_issued: bool, observed_draft: Option<bool>) -> MirrorPlan {
    match durable_post_decision(create_issued, observed_draft.is_some()) {
        DurablePostDecision::PersistIntentThenPost => MirrorPlan::CreateDraft,
        DurablePostDecision::AwaitVisibility => MirrorPlan::AwaitVisibility,
        DurablePostDecision::ConvergeVisible => match observed_draft {
            Some(true) => MirrorPlan::ConvergeDraft,
            // `ConvergeVisible` is only reachable with `Some(..)`; a published
            // object means our own flip landed before its journal mark.
            _ => MirrorPlan::ConvergePublished,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_MANIFEST: &str = include_str!("../../../Cargo.toml");

    /// PINS THE PER-ARCH DMG NAME SHAPES AND SET MEMBERSHIP. The bare
    /// `aterm-<v>.dmg` spelling is fleet-load-bearing (the deployed updater and
    /// install.sh bind it exactly), so the split MUST be "bare = arm64,
    /// `-x86_64` suffix = Intel" — a symmetric rename would 404 every installed
    /// client. The x86 pair (DMG + sidecar) joins the required set exactly when
    /// the flag says the manifest names one, and never otherwise: the mirrored
    /// byte set of an x86-less cut must not change by one name.
    #[test]
    fn per_arch_dmg_names_and_required_set_membership() {
        assert_eq!(dmg_asset_name("0.47.0"), "aterm-0.47.0.dmg");
        assert_eq!(dmg_x86_64_asset_name("0.47.0"), "aterm-0.47.0-x86_64.dmg");
        assert_eq!(stable_dmg_asset_name(), "aterm.dmg");
        assert_eq!(stable_zip_asset_name(), "aterm-mac.zip");

        let without = required_asset_names("0.47.0", true, false, false, false);
        assert!(!without.iter().any(|n| n.contains("x86_64")), "{without:?}");

        let with = required_asset_names("0.47.0", true, false, true, false);
        assert!(with.contains(&"aterm-0.47.0-x86_64.dmg".to_string()), "{with:?}");
        assert!(
            with.contains(&"aterm-0.47.0-x86_64.dmg.sha256".to_string()),
            "{with:?}"
        );
        // Exactly the pair, nothing else, joins the set.
        let extra: Vec<&String> = with.iter().filter(|n| !without.contains(n)).collect();
        assert_eq!(extra.len(), 2, "{extra:?}");

        // The exact-set check inherits the flag in both directions.
        validate_mirror_asset_set(&with, "0.47.0", true, false, true, false).expect("exact set");
        assert!(validate_mirror_asset_set(&with, "0.47.0", true, false, false, false).is_err());
        assert!(validate_mirror_asset_set(&without, "0.47.0", true, false, true, false).is_err());
    }

    /// PINS THE LITE-DMG NAME SHAPES AND SET MEMBERSHIP. The bare versioned DMG
    /// name is fleet-load-bearing AS the batteries-included container —
    /// install.sh's identity bind and the deployed updater elect it through the
    /// signed manifest — so the lean image is strictly ADDITIVE (`-lite`
    /// suffix), and the only names that repoint are the unversioned browser
    /// aliases: `aterm.dmg` serves the lean bytes and the new
    /// `aterm-offline.dmg` serves the seeded bytes. Exactly four names join the
    /// mirrored set with the flag, and none without it: a pre-lite cut's
    /// mirrored byte set must not change by one name.
    #[test]
    fn lite_dmg_names_and_required_set_membership() {
        assert_eq!(dmg_lite_asset_name("0.50.0"), "aterm-0.50.0-lite.dmg");
        assert_eq!(stable_offline_dmg_asset_name(), "aterm-offline.dmg");
        assert_eq!(
            sha256_sidecar_name(&dmg_lite_asset_name("0.50.0")),
            "aterm-0.50.0-lite.dmg.sha256"
        );
        assert_eq!(
            sha256_sidecar_name(&stable_offline_dmg_asset_name()),
            "aterm-offline.dmg.sha256"
        );

        let without = required_asset_names("0.50.0", true, false, false, false);
        assert!(
            !without.iter().any(|n| n.contains("lite") || n.contains("offline")),
            "{without:?}"
        );

        let with = required_asset_names("0.50.0", true, false, false, true);
        for name in [
            "aterm-0.50.0-lite.dmg",
            "aterm-0.50.0-lite.dmg.sha256",
            "aterm-offline.dmg",
            "aterm-offline.dmg.sha256",
        ] {
            assert!(with.contains(&name.to_string()), "{name} missing: {with:?}");
        }
        // Exactly the four, nothing else, join the set — and the versioned
        // seeded DMG stays exactly where it was.
        let extra: Vec<&String> = with.iter().filter(|n| !without.contains(n)).collect();
        assert_eq!(extra.len(), 4, "{extra:?}");
        assert!(with.contains(&"aterm-0.50.0.dmg".to_string()), "{with:?}");
        assert!(with.contains(&"aterm.dmg".to_string()), "{with:?}");

        // The exact-set check inherits the flag in both directions: a lite cut
        // whose lean assets never crossed is refused, and a pre-lite release
        // judged as a lite one is refused too.
        validate_mirror_asset_set(&with, "0.50.0", true, false, false, true).expect("exact set");
        let err = validate_mirror_asset_set(&without, "0.50.0", true, false, false, true)
            .expect_err("a lite cut's channel head without the lean DMG is refused");
        assert!(err.to_string().contains("aterm-0.50.0-lite.dmg"), "{err}");
        let err = validate_mirror_asset_set(&with, "0.50.0", true, false, false, false)
            .expect_err("lean assets on a pre-lite release are foreign objects");
        assert!(err.to_string().contains("aterm-0.50.0-lite.dmg"), "{err}");

        // Both flags compose: an x86-split lite cut carries all six additions.
        let both = required_asset_names("0.50.0", true, false, true, true);
        validate_mirror_asset_set(&both, "0.50.0", true, false, true, true).expect("exact set");
        assert_eq!(
            both.iter().filter(|n| !without.contains(n)).count(),
            6,
            "{both:?}"
        );
    }

    /// The drift this gate exists to stop, in its exact observed form: the tag
    /// `v0.6.0` came to rest on a public tree still carrying `0.5.0`.
    #[test]
    fn a_channel_behind_the_cut_is_refused_with_the_promote_remedy() {
        let channel = "[workspace.package]\nversion = \"0.5.0\"\n";
        let err = check_channel_version("0.6.0", channel).expect_err("must refuse");
        let msg = err.to_string();
        assert!(msg.contains("0.6.0") && msg.contains("0.5.0"), "{msg}");
        // The refusal is only useful if it names the fix.
        assert!(
            msg.contains("pub stage aterm && pub promote aterm"),
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
    fn mirror_target_is_exactly_the_channel_clients_compiled_in() {
        // THE binding this whole design rests on. `aterm-update-core/build.rs`
        // stamps `[workspace.metadata.aterm] update_channel` into every client
        // as DEFAULT_OWNER/DEFAULT_REPO; the cutter reads the same key to pick
        // its mirror target. If these ever disagree, the pipeline would publish
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
        // The mirror exists precisely because the publish repo is private and
        // unreadable without a credential. Pointing the channel back at it
        // would restore the "fresh machine never updates" bug AND make the
        // mirror step a silent no-op, hiding the regression.
        //
        // Scoped to the private staging namespace on purpose: `publish/` exports
        // a PUBLIC source snapshot that rewrites the staging owner into the
        // public org throughout, so in that tree `repository` and
        // `update_channel` legally coincide — one public repo serving both
        // source and releases, which is the documented "no separate mirror"
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
    fn absent_key_means_no_mirror_but_a_present_bad_key_is_an_error() {
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
            !aterm_update_core::pins::update_channel_signing_pubkey().is_empty(),
            "the public channel is pinned in pins.rs"
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
            required_asset_names("0.5.0", false, false, false, false),
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
            required_asset_names("0.5.0", true, false, false, false),
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
    fn mirrored_asset_set_must_match_the_client_rules_exactly() {
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
        validate_mirror_asset_set(&ok, "0.5.0", false, false, false, false).unwrap();
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
        validate_mirror_asset_set(&reordered, "0.5.0", false, false, false, false).unwrap();

        // Every way a plausible-looking mirror silently never updates:
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
            let err = validate_mirror_asset_set(&names, version, signed, false, false, false)
                .expect_err(&format!("{names:?} must be refused"));
            assert!(
                err.to_string().contains(needle),
                "error for {names:?} should name {needle:?}, got: {err}"
            );
        }
    }

    #[test]
    fn mirror_plan_never_reissues_a_durable_post() {
        assert_eq!(mirror_plan(false, None), MirrorPlan::CreateDraft);
        assert_eq!(mirror_plan(false, Some(true)), MirrorPlan::ConvergeDraft);
        assert_eq!(
            mirror_plan(false, Some(false)),
            MirrorPlan::ConvergePublished
        );
        // The whole point: intent issued + nothing visible is NOT a retry.
        assert_eq!(mirror_plan(true, None), MirrorPlan::AwaitVisibility);
        assert_eq!(mirror_plan(true, Some(true)), MirrorPlan::ConvergeDraft);
        assert_eq!(
            mirror_plan(true, Some(false)),
            MirrorPlan::ConvergePublished
        );
    }
}
