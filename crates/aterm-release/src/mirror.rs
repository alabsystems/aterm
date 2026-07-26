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
//! derived from the TAG and cross-checked against the manifest's own `dmg` field.
//! A mirror that uploaded, say, `aterm.dmg`, or two appcasts, or a release under
//! a two-component tag, would produce a channel that is live, plausible, and
//! permanently unelectable — the exact silent-never-updates failure this whole
//! effort exists to remove. [`required_asset_names`] is that rule as data, and
//! [`validate_mirror_asset_set`] is enforced against the REAL remote listing
//! before the mirrored draft is ever flipped visible.
//!
//! ## What is deliberately NOT mirrored
//!
//! Exactly the client-required set crosses over: the manifest, the DMG, and the
//! detached signature when the cut is signed. The provenance text and the dSYM
//! archive stay private — they are debugging aids for the owner, the client never
//! reads them, and a public channel should carry the smallest surface that still
//! satisfies the updater. Keeping the set exact also makes
//! [`validate_mirror_asset_set`] a total check (no "and maybe some extras" hole).

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

/// The EXACT asset set a mirrored release must carry, sorted, as the deployed
/// updater requires it: the appcast, the version-bound DMG, and — only when the
/// cut is signed — the detached signature the pinned client demands.
#[must_use]
pub fn required_asset_names(version: &str, signed: bool) -> Vec<String> {
    let mut names = vec![
        manifest_out::MANIFEST_ASSET.to_string(),
        dmg_asset_name(version),
    ];
    if signed {
        names.push(manifest_out::MANIFEST_SIG_ASSET.to_string());
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
pub fn validate_mirror_asset_set(names: &[String], version: &str, signed: bool) -> Result<()> {
    let required = required_asset_names(version, signed);
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
        // a PUBLIC source snapshot that rewrites `alabsystems` -> `alabsystems`
        // throughout, so in that tree `repository` and `update_channel` legally
        // coincide — one public repo serving both source and releases, which is
        // the documented "no separate mirror" configuration, not a regression.
        let channel = update_channel_slug(REAL_MANIFEST).unwrap().unwrap();
        let publish = crate::publish::repo_slug(REAL_MANIFEST).unwrap();
        if publish.starts_with("alabsystems/") {
            assert_ne!(
                channel, publish,
                "the private staging repo must never be the update channel"
            );
        }
    }

    #[test]
    fn absent_key_means_no_mirror_but_a_present_bad_key_is_an_error() {
        assert_eq!(update_channel_slug("[workspace]\nmembers = []\n").unwrap(), None);
        // Right key, wrong table — must not be picked up.
        assert_eq!(
            update_channel_slug("[workspace.metadata.atpkg]\nupdate_channel = \"a/b\"\n").unwrap(),
            None
        );
        for bad in [
            "alabsystems",             // no repo segment
            "alabsystems/aterm/extra", // three segments
            "alabsystems/",            // empty repo
            "/aterm",                  // empty owner
            "alab systems/aterm",      // space
            "alabsystems/../aterm",    // traversal shape
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
        // Unsigned (Tier REPO, the default): appcast + version-bound DMG.
        assert_eq!(
            required_asset_names("0.5.0", false),
            vec!["aterm-0.5.0.dmg".to_string(), "aterm-appcast.toml".to_string()]
        );
        // Signed (Tier SIG): a pinned client REFUSES a head with no .sig.
        assert_eq!(
            required_asset_names("0.5.0", true),
            vec![
                "aterm-0.5.0.dmg".to_string(),
                "aterm-appcast.toml".to_string(),
                "aterm-appcast.toml.sig".to_string(),
            ]
        );
        // The names are the client's literals, not a lookalike.
        assert_eq!(manifest_out::MANIFEST_ASSET, "aterm-appcast.toml");
        assert_eq!(manifest_out::MANIFEST_SIG_ASSET, "aterm-appcast.toml.sig");
        assert_eq!(dmg_asset_name("1.2.3"), "aterm-1.2.3.dmg");
    }

    #[test]
    fn mirrored_asset_set_must_match_the_client_rules_exactly() {
        let ok = vec![
            "aterm-appcast.toml".to_string(),
            "aterm-0.5.0.dmg".to_string(),
        ];
        validate_mirror_asset_set(&ok, "0.5.0", false).unwrap();
        // Order is irrelevant — GitHub does not promise listing order.
        let reordered = vec![
            "aterm-0.5.0.dmg".to_string(),
            "aterm-appcast.toml".to_string(),
        ];
        validate_mirror_asset_set(&reordered, "0.5.0", false).unwrap();

        // Every way a plausible-looking mirror silently never updates:
        let cases: Vec<(Vec<&str>, &str, bool, &str)> = vec![
            // no appcast at all -> the release is skipped by selection
            (vec!["aterm-0.5.0.dmg"], "0.5.0", false, "aterm-appcast.toml"),
            // two appcasts -> `unique_asset_index` refuses the release
            (
                vec!["aterm-appcast.toml", "aterm-appcast.toml", "aterm-0.5.0.dmg"],
                "0.5.0",
                false,
                "duplicated",
            ),
            // DMG named for the WRONG version -> manifest/tag disagreement
            (
                vec!["aterm-appcast.toml", "aterm-0.61.0.dmg"],
                "0.5.0",
                false,
                "aterm-0.5.0.dmg",
            ),
            // generic DMG name -> no asset matches manifest.dmg
            (
                vec!["aterm-appcast.toml", "aterm.dmg"],
                "0.5.0",
                false,
                "aterm-0.5.0.dmg",
            ),
            // signed cut whose signature never crossed -> pinned clients refuse
            (
                vec!["aterm-appcast.toml", "aterm-0.5.0.dmg"],
                "0.5.0",
                true,
                "aterm-appcast.toml.sig",
            ),
            // private-only artifacts leaking into the public set
            (
                vec![
                    "aterm-appcast.toml",
                    "aterm-0.5.0.dmg",
                    "aterm-0.5.0-dSYM.zip",
                ],
                "0.5.0",
                false,
                "aterm-0.5.0-dSYM.zip",
            ),
        ];
        for (names, version, signed, needle) in cases {
            let names: Vec<String> = names.into_iter().map(str::to_string).collect();
            let err = validate_mirror_asset_set(&names, version, signed)
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
        assert_eq!(mirror_plan(false, Some(false)), MirrorPlan::ConvergePublished);
        // The whole point: intent issued + nothing visible is NOT a retry.
        assert_eq!(mirror_plan(true, None), MirrorPlan::AwaitVisibility);
        assert_eq!(mirror_plan(true, Some(true)), MirrorPlan::ConvergeDraft);
        assert_eq!(mirror_plan(true, Some(false)), MirrorPlan::ConvergePublished);
    }
}
