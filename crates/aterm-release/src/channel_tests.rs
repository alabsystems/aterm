// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    assert_eq!(
        update_channel_slug("[workspace.metadata.atpkg]\nupdate_channel = \"a/b\"\n").unwrap(),
        None
    );
    for bad in [
        "alabsystems",
        "alabsystems/aterm/extra",
        "alabsystems/",
        "/aterm",
        "alab systems/aterm",
        "alabsystems/../aterm",
        "https://github.com/alabsystems/aterm",
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
        let err = validate_channel_asset_set(&names, "0.5.0", signed, false, &[]).expect_err(case);
        assert!(err.to_string().contains(needle), "{case}: {err}");
    }
}

#[test]
fn bind_plan_never_reissues_a_durable_post() {
    assert_eq!(bind_plan(false, None), BindPlan::CreateDraft);
    assert_eq!(bind_plan(false, Some(true)), BindPlan::ConvergeDraft);
    assert_eq!(bind_plan(false, Some(false)), BindPlan::ConvergeVisible);
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

    let uploaded = vec![
        dmg.clone(),
        aterm_update_core::roster::ROSTER_ASSET.to_string(),
    ];
    assert_eq!(
        withdrawable_assets(&adopted, v, Some(&uploaded)).unwrap(),
        vec![dmg.clone()]
    );
    assert_eq!(
        withdrawable_assets(&adopted, v, None).unwrap(),
        vec![dmg.clone(), zip.clone()]
    );
    let mut foreign = adopted.clone();
    foreign.push("aterm-0.94.0.dmg".to_string());
    withdrawable_assets(&foreign, v, None).unwrap_err();
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
