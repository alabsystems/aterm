// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn asset(name: &str, size: Option<u64>) -> Asset {
    Asset {
        name: name.into(),
        size,
    }
}

fn rel(tag: &str, assets: &[(&str, Option<u64>)]) -> Release {
    Release {
        tag: tag.into(),
        prerelease: false,
        draft: false,
        date: String::new(),
        body: String::new(),
        assets: assets.iter().map(|(n, s)| asset(n, *s)).collect(),
    }
}

/// A complete app release of `v` whose DMG weighs `size`.
fn app(v: &str, size: Option<u64>) -> Release {
    let mut r = rel(&format!("v{v}"), &[]);
    r.assets = vec![
        asset(&dmg_name(v), size),
        asset(&format!("aterm-{v}-mac.zip"), Some(1)),
        asset("aterm-appcast.toml", Some(1)),
        asset("aterm-appcast.toml.sig", Some(64)),
    ];
    r
}

struct NoProver;
impl LinuxProver for NoProver {
    fn prove(
        &mut self,
        r: &Release,
        _: &'static str,
        _: &Release,
    ) -> Result<LinuxDownload, String> {
        Err(format!("no prover for {}", r.tag))
    }
}

const SLUG: &str = "alabsystems/aterm";

#[test]
fn the_size_is_the_finders_decimal_units() {
    let label = |n| size_label(Some(n)).unwrap();
    assert_eq!(label(1_067_044_990), "1.07 GB");
    assert_eq!(label(28_219_832), "28 MB");
    assert_eq!(label(999_400_000), "999 MB");
    assert_eq!(label(999_499_999), "999 MB");
    assert_eq!(label(999_500_000), "1.00 GB");
    assert_eq!(label(999_999_999), "1.00 GB");
    assert_eq!(label(1_000_000_000), "1.00 GB");
    assert_eq!(label(12_345_678_901), "12.35 GB");
    assert_eq!(size_label(None), None);
}

#[test]
fn the_newest_version_wins_not_githubs_latest_and_the_url_is_versioned() {
    let releases = vec![
        app("0.63.0", Some(1_067_044_990)),
        app("0.64.0", Some(31_457_280)),
    ];
    let (mac, _) = elect_mac(&releases, SLUG, &mut Vec::new()).unwrap();
    assert_eq!(mac.label, "aterm-0.64.0.dmg");
    assert_eq!(
        mac.url,
        "https://github.com/alabsystems/aterm/releases/download/v0.64.0/aterm-0.64.0.dmg"
    );
    assert_eq!(mac.size_label().as_deref(), Some("31 MB"));
}

#[test]
fn calendar_releases_keep_full_order_and_reject_zero_counters_and_invalid_dates() {
    let releases = vec![
        app("0.194.0", Some(1)),
        app("1.20261109.1", Some(1)),
        app("1.20261109.2", Some(1)),
        app("1.20261110.0", Some(1)),
        app("1.20261301.1", Some(1)),
    ];
    let stable = stable_product_releases(&releases);
    assert_eq!(
        stable.iter().map(|r| r.version()).collect::<Vec<_>>(),
        ["1.20261109.2", "1.20261109.1", "0.194.0"]
    );
    let (mac, _) = elect_mac(&releases, SLUG, &mut Vec::new()).unwrap();
    assert_eq!(mac.tag, "v1.20261109.2");
    assert_eq!(
        mac.url,
        "https://github.com/alabsystems/aterm/releases/download/v1.20261109.2/aterm-1.20261109.2.dmg"
    );
}

#[test]
fn a_source_release_atop_an_app_cut_is_skipped_and_named() {
    let releases = vec![
        rel("v0.64.0", &[("SHA256SUMS", Some(273_281))]),
        app("0.63.0", Some(1_067_044_990)),
    ];
    let mut notes = Vec::new();
    let (mac, _) = elect_mac(&releases, SLUG, &mut notes).unwrap();
    assert_eq!(mac.tag, "v0.63.0");
    assert_eq!(
        notes,
        vec![Note::NoDmg {
            tag: "v0.64.0".into(),
            name: "aterm-0.64.0.dmg".into()
        }]
    );
}

#[test]
fn an_incomplete_app_upload_never_wins() {
    let mut half = app("0.64.0", Some(1));
    half.assets.retain(|a| a.name != "aterm-appcast.toml.sig");
    let releases = vec![half, app("0.63.0", Some(2))];
    let mut notes = Vec::new();
    let (mac, _) = elect_mac(&releases, SLUG, &mut notes).unwrap();
    assert_eq!(mac.tag, "v0.63.0");
    assert_eq!(
        notes,
        vec![Note::IncompleteApp {
            tag: "v0.64.0".into()
        }]
    );
}

#[test]
fn a_stream_with_no_versioned_dmg_anywhere_is_refused() {
    let releases = vec![rel(
        "v0.65.0",
        &[
            ("aterm.dmg", Some(31_457_280)),
            ("aterm-0.65.0-mac.zip", Some(1)),
        ],
    )];
    let mut notes = Vec::new();
    assert_eq!(
        elect_mac(&releases, SLUG, &mut notes).unwrap_err(),
        Refused::NothingInstallable
    );
    assert_eq!(
        notes,
        vec![Note::NoDmg {
            tag: "v0.65.0".into(),
            name: "aterm-0.65.0.dmg".into()
        }]
    );
    assert_eq!(
        elect_mac(&[], SLUG, &mut notes).unwrap_err(),
        Refused::NoStable { slug: SLUG.into() }
    );
    assert_eq!(
        elect_mac(&releases, "no slug", &mut notes).unwrap_err(),
        Refused::BadSlug
    );
}

#[test]
fn an_unknown_size_decides_nothing() {
    for json in [
        r#"[{"tag_name":"v0.63.0","assets":[{"name":"aterm-0.63.0.dmg"}]}]"#,
        r#"[{"tag_name":"v0.63.0","assets":[{"name":"aterm-0.63.0.dmg","size":"big"}]}]"#,
        r#"[{"tag_name":"v0.63.0","assets":[{"name":"aterm-0.63.0.dmg","size":null}]}]"#,
    ] {
        let mut r = parse_releases(json).unwrap();
        r[0].assets
            .extend(app("0.63.0", None).assets.into_iter().skip(1));
        let (mac, _) = elect_mac(&r, SLUG, &mut Vec::new()).unwrap();
        assert_eq!(
            (mac.label.as_str(), mac.size_bytes),
            ("aterm-0.63.0.dmg", None),
            "{json}"
        );
    }
}

#[test]
fn every_json_shape_the_api_and_gh_print_is_read() {
    let one =
        r#"{"tagName":"v0.64.0","isPrerelease":false,"assets":[{"name":"x","size":3,"url":"u"}]}"#;
    let pages = r#"[{"tag_name":"v0.2.0"}]
            [{"tag_name":"v0.1.0","draft":true}]"#;
    let slurped = r#"[[{"tag_name":"v0.2.0"}],[{"tag_name":"v0.1.0"}]]"#;
    assert_eq!(
        parse_releases(one).unwrap()[0].assets,
        vec![asset("x", Some(3))]
    );
    let paged = parse_releases(pages).unwrap();
    assert_eq!(
        paged
            .iter()
            .map(|r| (r.tag.as_str(), r.draft))
            .collect::<Vec<_>>(),
        [("v0.2.0", false), ("v0.1.0", true)]
    );
    assert_eq!(parse_releases(slurped).unwrap().len(), 2);
    assert!(parse_releases("3").is_err());
    assert!(parse_releases("[{").is_err());
}

#[test]
fn stable_is_published_canonical_tags_newest_first() {
    let mut rc = rel("v0.64.0-rc.1", &[]);
    rc.prerelease = true;
    let releases = vec![
        rel("v0.61.0", &[]),
        rc,
        rel("atpkg-index-7", &[]),
        rel("v0.63.0", &[]),
        rel("v0.010.0", &[]),
    ];
    let tags: Vec<&str> = stable(&releases).iter().map(|r| r.tag.as_str()).collect();
    assert_eq!(tags, ["v0.63.0", "v0.61.0"]);
}

#[test]
fn linux_follows_the_newest_release_that_carries_its_tarball() {
    let mut old = app("0.63.0", Some(1));
    old.assets
        .push(asset("aterm-0.63.0-linux-x86_64.tar.gz", Some(15_887_638)));
    old.assets
        .push(asset("aterm-0.63.0-linux-x86_64.tar.gz.sha256", Some(99)));
    let releases = vec![app("0.64.0", Some(1)), old];
    let mut notes = Vec::new();
    let facts = elect(&releases, SLUG, &mut NoProver, &mut notes).unwrap();
    assert_eq!(facts.head, "v0.64.0");
    let x86 = facts.linux("x86_64").unwrap();
    assert_eq!(
        (x86.tag.as_str(), x86.label.as_str(), &x86.kind),
        (
            "v0.63.0",
            "aterm-0.63.0-linux-x86_64.tar.gz",
            &LinuxKind::LegacyTar
        )
    );
    assert_eq!(facts.linux("aarch64"), None);
    assert!(notes.contains(&Note::LinuxLags {
        tag: "v0.64.0".into(),
        name: "aterm-0.64.0-linux-x86_64.tar.gz".into(),
        arch: "x86_64",
        newest: "v0.63.0".into()
    }));
    assert!(notes.contains(&Note::NoLinux { arch: "aarch64" }));
    let mut bare = app("0.64.0", Some(1));
    bare.assets
        .push(asset("aterm-0.64.0-linux-x86_64.tar.gz", Some(1)));
    let facts = elect(&[bare], SLUG, &mut NoProver, &mut Vec::new()).unwrap();
    assert_eq!(facts.linux("x86_64"), None);
}

#[test]
fn a_raw_linux_executable_is_proved_or_the_election_refuses() {
    let mut r = app("0.64.0", Some(1));
    r.assets
        .push(asset("aterm-0.64.0-linux-aarch64", Some(9000)));
    let err = elect(&[r], SLUG, &mut NoProver, &mut Vec::new()).unwrap_err();
    assert_eq!(
        err,
        Refused::Linux {
            tag: "v0.64.0".into(),
            arch: "aarch64",
            why: "no prover for v0.64.0".into()
        }
    );
}

#[test]
fn the_intel_proof_is_the_chosen_releases_own_record() {
    let mut r = app("0.64.0", Some(1));
    r.assets
        .push(asset("aterm-0.64.0-x86_64-probe.txt", Some(900)));
    let facts = elect(&[r], SLUG, &mut NoProver, &mut Vec::new()).unwrap();
    assert_eq!(
        facts.intel_proof.as_deref(),
        Some(
            "https://github.com/alabsystems/aterm/releases/download/v0.64.0/\
             aterm-0.64.0-x86_64-probe.txt"
        )
    );
    let mut r = app("0.64.0", Some(1));
    r.assets
        .push(asset("aterm-0.63.0-x86_64-probe.txt", Some(900)));
    let mut notes = Vec::new();
    let facts = elect(&[r], SLUG, &mut NoProver, &mut notes).unwrap();
    assert_eq!(facts.intel_proof, None);
    assert!(notes.contains(&Note::NoIntelProof {
        tag: "v0.64.0".into(),
        name: "aterm-0.64.0-x86_64-probe.txt".into()
    }));
}
