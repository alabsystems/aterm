// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const PAGE_WITH_A_TAGLESS_DRAFT: &str = "9\t\ttrue\tmain\n12\tv0.3.0\tfalse\tabc123\n";

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

#[test]
fn a_captured_row_that_is_malformed_still_fails_closed() {
    let wanted = std::collections::BTreeSet::from([9, 12]);
    let selected = captured_identity_rows(PAGE_WITH_A_TAGLESS_DRAFT, &wanted);
    publish::parse_release_object_identity_rows(&selected)
        .expect_err("an empty tag on a CAPTURED release is still fatal");
}

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
