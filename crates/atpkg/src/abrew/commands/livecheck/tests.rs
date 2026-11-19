// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// r405: plist field extraction in both encodings, and
/// the no-block item version assembly
/// (`[short, bundle]` present parts, deduplicated).
#[test]
fn plist_fields_and_item_versions() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>CFBundleShortVersionString</key>
	<string>2.5.0</string>
	<key>Nested</key>
	<dict>
		<key>CFBundleVersion</key>
		<string>inner</string>
	</dict>
	<key>CFBundleVersion</key>
	<string>250</string>
	<key>Count</key>
	<integer>7</integer>
</dict>
</plist>"#;
    assert_eq!(
        plist_string_fields(xml).unwrap(),
        vec![
            (
                "CFBundleShortVersionString".to_string(),
                "2.5.0".to_string()
            ),
            ("CFBundleVersion".to_string(), "250".to_string()),
        ]
    );
    // r454: a dict ending in self-closing values
    // (`<true/>`) still terminates the walk (the skip
    // arm continues past the old loop-end check).
    let xml_tail = br#"<plist version="1.0">
<dict>
	<key>CFBundleVersion</key>
	<string>0.1</string>
	<key>CFBundleAllowMixedLocalizations</key>
	<true/>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
</dict>
</plist>"#;
    assert_eq!(
        plist_string_fields(xml_tail).unwrap(),
        vec![("CFBundleVersion".to_string(), "0.1".to_string())]
    );
    // Hand-built bplist00: {A: "1.2", B: "12", N: {x: "y"}}.
    let bin: &[u8] = &[
        98, 112, 108, 105, 115, 116, 48, 48, 211, 1, 2, 3, 4, 5, 6, 81, 65, 81, 66, 81, 78, 83, 49,
        46, 50, 82, 49, 50, 209, 7, 8, 81, 120, 81, 121, 8, 15, 17, 19, 21, 25, 28, 31, 33, 0, 0,
        0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 35,
    ];
    assert_eq!(
        plist_string_fields(bin).unwrap(),
        vec![
            ("A".to_string(), "1.2".to_string()),
            ("B".to_string(), "12".to_string()),
        ]
    );
    assert_eq!(plist_string_fields(b"bplist00junk"), None);
    // Mutations of a valid document never panic (the
    // parser runs on downloaded bytes): every truncation
    // and single-byte flip returns or parses cleanly.
    for cut in 0..bin.len() {
        let _ = plist_string_fields(&bin[..cut]);
    }
    for i in 0..bin.len() {
        for delta in [1u8, 0x80, 0xff] {
            let mut mutated = bin.to_vec();
            mutated[i] = mutated[i].wrapping_add(delta);
            let _ = plist_string_fields(&mutated);
        }
    }
    for cut in 0..xml.len() {
        let _ = plist_string_fields(&xml[..cut]);
    }
    let item = |short: Option<&str>, bundle: Option<&str>| {
        PlistItem {
            bundle_id: None,
            short: short.map(str::to_string),
            bundle: bundle.map(str::to_string),
        }
        .version()
    };
    assert_eq!(
        item(Some("2.5.0"), Some("250")).as_deref(),
        Some("2.5.0,250")
    );
    assert_eq!(item(Some("4.0"), Some("4.0")).as_deref(), Some("4.0"));
    // r454 `nice_parts`: a build number already
    // dotted into the short version is dropped, a
    // bundle version extending the short one wins,
    // and a trailing `(version)` is stripped first.
    assert_eq!(item(Some("9.2.2"), Some("2")).as_deref(), Some("9.2.2"));
    assert_eq!(item(Some("1.2"), Some("1.2.3")).as_deref(), Some("1.2.3"));
    assert_eq!(item(Some("1.2.3"), Some("1.2")).as_deref(), Some("1.2.3"));
    assert_eq!(
        item(Some("5.0 (123)"), Some("123")).as_deref(),
        Some("5.0,123")
    );
    assert_eq!(item(Some("3.1"), None).as_deref(), Some("3.1"));
    assert_eq!(item(None, Some("777")).as_deref(), Some("777"));
    assert_eq!(item(None, None), None);
}

#[test]
fn binary_plist_utf16_fields_preserve_surrogate_pairs() {
    let mut bytes = b"bplist00".to_vec();
    // Three objects: a one-entry dictionary, its ASCII key, and three UTF-16 units.
    bytes.extend([
        0xd1, 1, 2, 0x51, b'K', 0x63, 0x00, 0xe9, 0xd8, 0x3d, 0xde, 0x00,
    ]);
    let offsets_at = bytes.len() as u64;
    bytes.extend([8, 11, 13]);
    let mut trailer = [0u8; 32];
    trailer[6] = 1;
    trailer[7] = 1;
    trailer[8..16].copy_from_slice(&3u64.to_be_bytes());
    trailer[24..32].copy_from_slice(&offsets_at.to_be_bytes());
    bytes.extend(trailer);
    assert_eq!(
        plist_string_fields(&bytes),
        Some(vec![("K".into(), "é😀".into())])
    );
}

/// r402: GitHub API failures take brew's raised texts
/// (bare exception class for 404s, prefixed HTTP
/// explanation otherwise).
#[test]
fn github_api_error_texts() {
    let not_found = agh::Error::new(agh::Kind::NotFound, "GitHub answered 404: Not Found");
    assert_eq!(
        github_api_error(&not_found),
        "GitHub::API::HTTPNotFoundError"
    );
    let not_visible = agh::Error::new(agh::Kind::NotVisible, "as dev, answers 404");
    assert_eq!(
        github_api_error(&not_visible),
        "GitHub::API::HTTPNotFoundError"
    );
    let limited = agh::Error::new(agh::Kind::RateLimited, "GitHub answered 403: slow down");
    assert_eq!(
        github_api_error(&limited),
        "GitHub API Error: GitHub answered 403: slow down"
    );
}

/// r399: the mini YAML parser walks block mappings
/// (pnpm-lock shape) and sequences, and scalar
/// typing follows YAML 1.1 resolution.
#[test]
fn yaml_mini_parser_walks_maps_and_seqs() {
    let dig = |doc: &str, path: &[&str]| -> Option<String> {
        let root = yaml_parse(doc).ok()?;
        let mut cur = &root;
        for step in path {
            match cur {
                YNode::Map(entries) => {
                    cur = &entries.iter().find(|(k, _)| k == step)?.1;
                }
                _ => return None,
            }
        }
        match cur {
            YNode::Scalar(s, _) => Some(s.clone()),
            _ => None,
        }
    };
    let lock =
        "importers:\n  .:\n    dependencies:\n      '@napi-rs/keyring':\n        version: 2.3.1\n";
    assert_eq!(
        dig(
            lock,
            &[
                "importers",
                ".",
                "dependencies",
                "@napi-rs/keyring",
                "version"
            ]
        )
        .as_deref(),
        Some("2.3.1")
    );
    assert_eq!(dig(lock, &["importers", "nope"]), None);
    // Same-indent sequences under a key.
    let seq = yaml_parse("items:\n- \"1.9\"\n- \"2.9\"\n").ok();
    match seq {
        Some(YNode::Map(entries)) => match &entries[0].1 {
            YNode::Seq(items) => assert_eq!(items.len(), 2),
            _ => panic!("expected sequence"),
        },
        _ => panic!("expected map"),
    }
    assert!(matches!(yaml_scalar_kind("2.3.1"), YScalarKind::Str));
    assert!(matches!(yaml_scalar_kind("3.1"), YScalarKind::Float));
    assert!(matches!(yaml_scalar_kind("42"), YScalarKind::Int));
    assert!(matches!(yaml_scalar_kind("yes"), YScalarKind::Bool(true)));
    assert!(yaml_parse("a: 1\n\tb: 2\n").is_err());
}

/// r390: the `--debug` matched-versions list dedupes
/// in discovery order (the oracle's trealla output
/// shows each matched version once, unsorted by
/// version semantics).
#[test]
fn debug_matched_dedupes_in_discovery_order() {
    let versions: Vec<String> = ["1.10", "1.2", "1.10", "0.9", "1.2"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(debug_matched(&versions), "1.10, 1.2, 0.9");
    assert_eq!(debug_matched(&[]), "");
}

/// r413: API core records carry the bare leaf as
/// `full_name` and must read as from-API (the
/// agreement gate); tap-loaded records are qualified.
#[test]
fn formula_from_api_discriminates() {
    let mk = |full: &str| crate::abrew::formula::Formula {
        name: "immich-cli".to_string(),
        full_name: full.to_string(),
        ..Default::default()
    };
    assert!(formula_from_api(&mk("immich-cli")));
    assert!(formula_from_api(&mk("homebrew/core/immich-cli")));
    assert!(!formula_from_api(&mk("homebrew/cask/immich-cli")));
    assert!(!formula_from_api(&mk("probeuser/probe/immich-cli")));
}

/// r418: a lazy capture group takes the SHORTEST
/// feasible split (Ruby order) — Hackage's derived
/// `(.*?)/?` must leave a trailing slash to the
/// suffix (shelltestrunner reported `1.11/` before).
#[test]
fn lazy_group_split_is_shortest() {
    let re = lc_regex("<h3>shelltestrunner-(.*?)/?</h3>", "i").unwrap();
    assert_eq!(
        scan_versions(
            &re,
            "<h3>shelltestrunner-1.11/</h3><h3>shelltestrunner-1.10</h3>"
        ),
        ["1.11", "1.10"]
    );
    // Greedy groups are unaffected.
    let greedy = lc_regex(r"v(\d+(?:\.\d+)*)\.tar", "").unwrap();
    assert_eq!(scan_versions(&greedy, "v1.2.3.tar"), ["1.2.3"]);
    // r459: a lazy prefix with a fixed tail
    // (`href=.*?v?`) still splits shortest-first
    // (dosbox-x read group 1 as `0.01` before).
    let re = lc_regex(
        r"href=.*?v?(\d+(?:\.\d+)+)/dosbox-x-macosx?-arm64[._-]v?(\d+(?:\.\d+)+)\.zip",
        "i",
    )
    .unwrap();
    let m = re
        .whole
        .find("href=\"https://example.com/dosbox-x-v2026.10.01/dosbox-x-macosx-arm64-2026.10.01.zip\"")
        .unwrap();
    assert_eq!(
        extract_groups(&re, m.as_str()),
        [
            Some("2026.10.01".to_string()),
            Some("2026.10.01".to_string())
        ]
    );
    // r462: a greedy repeated group yields its
    // LAST iteration (menuwhere/witch `(\.\d+)*`),
    // nil when it never iterated.
    let re = lc_regex(r"^v?(\d+\.\d+)(\.\d+)*$", "i").unwrap();
    let groups = |s: &str| {
        let m = re.whole.find(s).unwrap();
        extract_groups(&re, m.as_str())
    };
    assert_eq!(groups("1.2"), [Some("1.2".to_string()), None]);
    assert_eq!(
        groups("1.2.3"),
        [Some("1.2".to_string()), Some(".3".to_string())]
    );
    assert_eq!(
        groups("v1.2.3.4"),
        [Some("1.2".to_string()), Some(".4".to_string())]
    );
    // r468: `\h` inside a character class
    // inlines its ranges (dorico `[\h-]+`).
    let re = lc_regex(r"/([\h-]+)/Dorico[._-]v?(\d+(?:\.\d+)*)", "i").unwrap();
    let m = re.whole.find("/27a07da9-fcfa/Dorico_6.2").unwrap();
    assert_eq!(
        extract_groups(&re, m.as_str()),
        [Some("27a07da9-fcfa".to_string()), Some("6.2".to_string())]
    );
    // `+` needs only its single iteration.
    let re = lc_regex(r"^(\d+)(\.\d+)+$", "").unwrap();
    let m = re.whole.find("1.2").unwrap();
    assert_eq!(
        extract_groups(&re, m.as_str()),
        [Some("1".to_string()), Some(".2".to_string())]
    );
}

/// r418: Hackage match/values, pinned against the
/// oracle over every haskell.org URL in
/// homebrew-core (75/75, 73 matching).
#[test]
fn hackage_match_and_values_oracle() {
    assert!(hackage_match(
        "https://downloads.haskell.org/~ghc/9.12.2/ghc-9.12.2-src.tar.xz"
    ));
    assert_eq!(
        hackage_values("https://hackage.haskell.org/package/happy-2.1.7/happy-2.1.7.tar.gz"),
        Some((
            "https://hackage.haskell.org/package/happy/src/".to_string(),
            "<h3>happy-(.*?)/?</h3>".to_string()
        ))
    );
    assert!(!hackage_match(
        "https://hackage.haskell.org/package/git-annex"
    ));
    assert_eq!(
        hackage_values("https://hackage.haskell.org/package/git-annex"),
        None
    );
    assert!(!hackage_match("https://example.com/package/foo-1.0.tar.gz"));
}

/// r417: crate package/index derivation, pinned
/// against the oracle's `generate_input_values`
/// over every static.crates.io URL in homebrew-core
/// (31/31), incl. the sparse-index shard rules.
#[test]
fn crate_package_and_index_url() {
    assert_eq!(
        crate_package("https://static.crates.io/crates/serde/serde-1.0.219.crate").as_deref(),
        Some("serde")
    );
    assert_eq!(
        crate_package("https://static.crates.io/crates/serde/"),
        None
    );
    assert_eq!(
        crate_package("https://example.com/crates/serde/serde-1.0.crate"),
        None
    );
    assert_eq!(crate_index_url("ab"), "https://index.crates.io/2/ab");
    assert_eq!(crate_index_url("abc"), "https://index.crates.io/3/a/abc");
    assert_eq!(
        crate_index_url("serde"),
        "https://index.crates.io/se/rd/serde"
    );
    assert_eq!(
        crate_index_url("Tokio"),
        "https://index.crates.io/to/ki/tokio"
    );
}

/// r416: Cpan values, pinned against the oracle's
/// own `generate_input_values` over every
/// cpan.metacpan.org / www.cpan.org URL in
/// homebrew-core (457/457, 455 matching).
#[test]
fn cpan_values_match_oracle() {
    assert_eq!(
        cpan_values("https://cpan.metacpan.org/authors/id/A/AM/AMBS/btparse/btparse-0.35.tar.gz"),
        Some((
            "https://www.cpan.org/authors/id/A/AM/AMBS/btparse/".to_string(),
            r"href=.*?btparse[._-]v?(\d+(?:\.\d+)*)\.t".to_string()
        ))
    );
    assert_eq!(
        cpan_values("https://cpan.metacpan.org/authors/id/R/RJ/RJBS/App-Cmd-0.340.tar.gz")
            .map(|(u, _)| u)
            .as_deref(),
        Some("https://www.cpan.org/authors/id/R/RJ/RJBS/")
    );
    assert_eq!(
        cpan_values("https://www.cpan.org/src/5.0/perl-5.44.0.tar.xz"),
        None
    );
    assert_eq!(
        cpan_values("https://example.com/authors/id/A/AB/ABC/x-1.0.tar.gz"),
        None
    );
}

/// r415: Xorg match/values, pinned against the
/// oracle over every x.org-ish URL in homebrew-core
/// (291/291, 78 matching). Generated values exist
/// for non-matching URLs too (explicit `:xorg`).
#[test]
fn xorg_match_and_values_oracle() {
    assert!(xorg_match("https://archive.mesa3d.org/mesa-26.2.6.tar.xz"));
    assert_eq!(
        xorg_values("https://archive.mesa3d.org/mesa-26.2.6.tar.xz"),
        Some((
            "https://archive.mesa3d.org/".to_string(),
            r"href=.*?mesa[._-]v?(\d+(?:\.\d+)+)\.t".to_string()
        ))
    );
    assert!(xorg_match(
        "https://www.x.org/archive/individual/lib/libX11-1.8.12.tar.xz"
    ));
    assert_eq!(
        xorg_values("https://www.x.org/pub/individual/lib/libX11-1.8.12.tar.xz")
            .map(|(u, _)| u)
            .as_deref(),
        Some("https://www.x.org/archive/individual/lib/")
    );
    // freedesktop /releases/ does not derive-match,
    // but values still generate for explicit use.
    assert!(!xorg_match(
        "https://xorg.freedesktop.org/releases/individual/util/imake-1.0.11.tar.xz"
    ));
    assert!(
        xorg_values("https://xorg.freedesktop.org/releases/individual/util/imake-1.0.11.tar.xz")
            .is_some()
    );
    assert!(!xorg_match("https://example.com/mesa-1.2.tar.xz"));
}

/// r414: Gnome package derivation (106/106 vs the
/// oracle generator) and the old-scheme unstable
/// filter rule (rows confirmed against brew's own
/// Version comparisons).
#[test]
fn gnome_package_and_stable_filter() {
    assert_eq!(
        gnome_package("https://download.gnome.org/sources/glib/2.86/glib-2.86.3.tar.xz").as_deref(),
        Some("glib")
    );
    assert_eq!(
        gnome_package("https://download.gnome.org/sources/glib"),
        None
    );
    assert_eq!(
        gnome_package("https://example.com/sources/glib/2.86/x.tar.xz"),
        None
    );
    for (v, stable) in [
        ("48.1", true),
        ("40.alpha", true),
        ("2", true),
        ("3.38.9", true),
        ("3.37.1", false),
        ("2.99.4", false),
        ("3.38.91", false),
        ("1.90", false),
        ("1.89.89", false),
        ("39.0", true),
    ] {
        assert_eq!(gnome_stable(v), stable, "{v}");
    }
}

/// r413: npm package derivation, pinned against the
/// oracle's own `generate_input_values` over every
/// registry.npmjs.org URL in homebrew-core (292/292).
#[test]
fn npm_package_matches_oracle() {
    let rows: &[(&str, Option<(&str, &str)>)] = &[
        (
            "https://registry.npmjs.org/typescript/-/typescript-5.7.3.tgz",
            Some(("typescript", "typescript")),
        ),
        (
            "https://registry.npmjs.org/@babel/core/-/core-7.26.0.tgz",
            Some(("@babel/core", "%40babel%2Fcore")),
        ),
        ("https://registry.npmjs.org/@napi-rs/nice/latest", None),
        ("https://registry.npmjs.org/", None),
        ("https://example.com/pkg/-/pkg-1.0.tgz", None),
    ];
    for (url, want) in rows {
        assert_eq!(
            npm_package(url).map(|p| {
                let enc = form_encode_component(&p);
                (p, enc)
            }),
            want.map(|(p, e)| (p.to_string(), e.to_string())),
            "{url}"
        );
    }
}

/// r412: Sourceforge strategy inputs, pinned against
/// the oracle's own `generate_input_values` over
/// every sourceforge-ish URL in homebrew-core
/// (900/900, 683 matching). Bare project homepages
/// deliberately do NOT match (brew falls through).
#[test]
fn sourceforge_values_match_oracle() {
    type Row<'a> = (&'a str, Option<(Option<&'a str>, &'a str)>);
    let rows: &[Row] = &[
        (
            "https://downloads.sourceforge.net/bbmap/BBMap_40.05.tar.gz",
            Some((
                Some("https://sourceforge.net/projects/bbmap/rss"),
                r"url=.*?/bbmap/files/.*?[-_/](\d+(?:[-.]\d+)+)[-_/%.]",
            )),
        ),
        (
            "https://downloads.sourceforge.net/project/lzmautils/xz-5.8.4.tar.gz",
            Some((
                Some("https://sourceforge.net/projects/lzmautils/rss"),
                r"url=.*?/lzmautils/files/.*?[-_/](\d+(?:[-.]\d+)+)[-_/%.]",
            )),
        ),
        (
            "https://sourceforge.net/projects/bbmap/rss",
            Some((
                None,
                r"url=.*?/bbmap/files/.*?[-_/](\d+(?:[-.]\d+)+)[-_/%.]",
            )),
        ),
        ("https://matio.sourceforge.net/", None),
        ("https://sourceforge.net/", None),
    ];
    for (url, want) in rows {
        assert_eq!(
            sourceforge_values(url),
            want.map(|(u, r)| (u.map(str::to_string), r.to_string())),
            "{url}"
        );
    }
}

/// r410: Apache strategy inputs, pinned against the
/// oracle's own `generate_input_values` over every
/// apache.org URL in homebrew-core (233/233); these
/// rows span the dyn/closer.lua, archive, and
/// non-matching forms.
#[test]
fn apache_values_match_oracle() {
    let rows: &[(&str, Option<(&str, &str)>)] = &[
        (
            "https://www.apache.org/dyn/closer.lua?path=flink/flink-cdc-3.7.0/flink-cdc-3.7.0-2.2-bin.tar.gz",
            Some((
                "https://archive.apache.org/dist/flink/",
                r#"href=["']?flink-cdc-v?(\d+(?:\.\d+)+)/"#,
            )),
        ),
        (
            "https://archive.apache.org/dist/ant/binaries/apache-ant-1.10.18-bin.tar.xz",
            Some((
                "https://archive.apache.org/dist/ant/binaries/",
                r#"href=["']?apache-ant-v?(\d+(?:\.\d+)+)-bin\.t"#,
            )),
        ),
        (
            "https://archive.apache.org/dist/activemq/6.4.0/apache-activemq-6.4.0-bin.tar.gz",
            Some((
                "https://archive.apache.org/dist/activemq/",
                r#"href=["']?v?(\d+(?:\.\d+)+)/"#,
            )),
        ),
        ("https://www.apache.org/licenses/LICENSE-2.0", None),
        ("https://example.com/dist/foo-1.2.tar.gz", None),
    ];
    for (url, want) in rows {
        assert_eq!(
            apache_values(url),
            want.map(|(u, r)| (u.to_string(), r.to_string())),
            "{url}"
        );
    }
}

/// r409: resource currents — explicit version wins,
/// a git tag detects in place of the URL, and an
/// undetectable URL is stamped with the formula's
/// version (brew's SoftwareSpec#owner=), which is
/// how e.g. wzprof's `simple.wasm` test resource
/// reports the parent's 0.2.0.
#[test]
fn resource_current_derivation() {
    let wasm = "https://github.com/dispatchrun/wzprof/raw/c2e9f22/testdata/c/simple.wasm";
    assert_eq!(
        resource_current(None, None, wasm, "0.2.0").as_deref(),
        Some("0.2.0")
    );
    assert_eq!(
        resource_current(Some("9.9".into()), None, wasm, "0.2.0").as_deref(),
        Some("9.9")
    );
    assert_eq!(
        resource_current(
            None,
            Some("v0.14.0".into()),
            "https://github.com/rggen/rggen-verilog.git",
            "0.36.1"
        )
        .as_deref(),
        Some("0.14.0")
    );
    // An undetectable tag does not fall back to the
    // URL; the stamp applies instead.
    assert_eq!(
        resource_current(
            None,
            Some("release".into()),
            "https://example.com/x/foo-1.2.3.tar.gz",
            "4.0"
        )
        .as_deref(),
        Some("4.0")
    );
    // No version anywhere: stays undetected.
    assert_eq!(resource_current(None, None, wasm, ""), None);
}

/// r384/r407: PyPI project derivation, as shown by
/// `brew livecheck -r --verbose` on python-shaped
/// resources and pinned against the oracle strategy's
/// own `FILENAME_REGEX` (run under Ruby): the name is
/// greedy to the last `-` before a trailing
/// `.<alnum>+` suffix, so a dotted-post version file
/// (`python-dateutil-2.9.0.post0.tar.gz`, ansible's
/// resource) derives `python-dateutil`, and a wheel
/// keeps its tags in the name, as the oracle does.
#[test]
fn pypi_project_and_version_parse() {
    let flit = "https://files.pythonhosted.org/packages/e7/91/flit_core-4.1.0.tar.gz";
    assert_eq!(pypi_project(flit).as_deref(), Some("flit-core"));
    let whl = "https://files.pythonhosted.org/packages/a/packaging-26.3-py3-none-any.whl";
    assert_eq!(
        pypi_project(whl).as_deref(),
        Some("packaging-26.3-py3-none")
    );
    let zope = "https://files.pythonhosted.org/packages/b/zope_interface-7.0.3.tar.gz";
    assert_eq!(pypi_project(zope).as_deref(), Some("zope-interface"));
    assert_eq!(
        pypi_project(
            "https://files.pythonhosted.org/packages/66/c0/python-dateutil-2.9.0.post0.tar.gz"
        )
        .as_deref(),
        Some("python-dateutil")
    );
    assert_eq!(
        pypi_project("https://files.pythonhosted.org/packages/66/c0/my%20pkg-1.0.zip").as_deref(),
        Some("my-pkg")
    );
    assert_eq!(
        pypi_project("https://files.pythonhosted.org/packages/66/c0/pkg-1.0.README").as_deref(),
        Some("pkg")
    );
    for none in [
        "https://example.com/packaging-26.3.tar.gz",
        "https://files.pythonhosted.org/packages/a/nover.xz",
        "https://files.pythonhosted.org/packages/a/-1.0.tar.gz",
        "https://files.pythonhosted.org/packages/a/pkg.TAR.GZ",
        "https://files.pythonhosted.org/packages/a/pkg-",
    ] {
        assert_eq!(pypi_project(none), None, "{none}");
    }
    // Path-segment fallback (influxdb@2 ui-assets).
    assert_eq!(
        version_parse(
            "https://github.com/influxdata/ui/releases/download/OSS-v2.10.0/build.tar.gz"
        )
        .as_deref(),
        Some("2.10.0")
    );
    // r389/r403/r408: resource-current detection across
    // the oracle-probed filename shapes. Since r408
    // these run the faithful Version.parse port, so
    // every row is the oracle's own reported version
    // (None only where the oracle detects none).
    let shapes: &[(&str, Option<&str>)] = &[
        ("foo-1.0rc1.tar.gz", Some("1.0rc1")),
        ("foo-2.0-beta.3.tar.gz", Some("2.0-beta.3")),
        ("foo-1.2.3b1.tar.gz", Some("1.2.3b1")),
        ("foo1.2.tar.gz", Some("1.2")),
        ("foo_1.2.3_alpha.tar.gz", Some("1.2.3_alpha")),
        ("foo-2024.01.tar.gz", Some("2024.01")),
        // r407: the oracle's `.post\d+` forms (probed
        // via `brew ruby` Version.parse), including the
        // digit-less `.post` tail the oracle also keeps.
        ("python-dateutil-2.9.0.post0.tar.gz", Some("2.9.0.post0")),
        ("foo-1.2.3.post12.tar.gz", Some("1.2.3.post12")),
        ("foo-1.2-post3.tar.gz", Some("1.2-post3")),
        ("foo-1.2_post3.tar.gz", Some("1.2_post3")),
        ("foo-1.2.post3-linux.tar.gz", Some("1.2.post3")),
        ("foo-1.2xpost3.tar.gz", Some("1.2xpost3")),
        ("foo-1.2.POST3.tar.gz", Some("3")),
        ("foo-1.2.dev3.tar.gz", Some("3")),
        ("foo-1.2.post.tar.gz", Some("1.2.post")),
        ("foo-r16.tar.gz", Some("16")),
        ("foo-1.0.0-linux.tar.gz", Some("1.0.0")),
        ("foo-1.2.3.4.5.tar.gz", Some("1.2.3.4.5")),
        ("foo-3.0a.tar.gz", Some("3.0a")),
        ("foo-1.1.1j.tar.gz", Some("1.1.1j")),
        ("foo-2.4.60-1.tar.gz", Some("2.4.60-1")),
        ("foo2bar-1.0.tar.gz", Some("1.0")),
        ("lib2to3-4.5.tar.gz", Some("4.5")),
        ("foo-1.2-3.4.tar.gz", Some("3.4")),
        ("foo128.tar.gz", Some("128")),
        ("foo-ver1.2.tar.gz", Some("1.2")),
        ("foo-1.2.3-rc.1+build5.tar.gz", Some("1.2.3-rc.1")),
        ("foo-1.2.3+build5.tar.gz", Some("5")),
        ("foo-1.0_amd64.tar.gz", Some("64")),
        ("foo-1.0-src.tar.gz", Some("1.0")),
        ("foo-1.0.RELEASE.tar.gz", Some("1.0.RELEASE")),
        ("foo-1.0.0.GA.tar.gz", Some("1.0.0.GA")),
        ("foo-1.0.0.Final.tar.gz", Some("1.0.0")),
        ("foo-9.4p1.tar.gz", Some("1")),
        ("foo-1.0.1-2.el8.tar.gz", Some("8")),
        ("boost_1_87_0.tar.gz", Some("1.87.0")),
        ("foo-1.0.0-1-linux.tar.gz", Some("1.0.0")),
        ("foo-1.0.0-rc1-linux.tar.gz", Some("1.0.0-rc1")),
        ("foo-1.0.0-snapshot.tar.gz", Some("1.0.0")),
        ("foo-1.0.0-x64.tar.gz", Some("1.0.0")),
        ("foo-1.0.0-arm64.tar.gz", Some("64")),
        ("foo-1.0_1.tar.gz", Some("0.1")),
        ("packaging-26.3-py3-none-any.whl", Some("26.3")),
        ("zope_interface-7.0.3-py3-none-any.whl", Some("7.0.3")),
    ];
    for (name, want) in shapes {
        assert_eq!(
            version_parse(&format!("https://example.com/x/{name}")).as_deref(),
            *want,
            "shape {name}"
        );
    }
    // The plan for a hosted URL is the PyPI JSON API.
    match plan_url(None, flit, "", "").unwrap() {
        UrlPlan::Run(rp) => {
            assert_eq!(strat_name(rp.strat), "Pypi");
            assert_eq!(
                rp.strategy_url.as_deref(),
                Some("https://pypi.org/pypi/flit-core/json")
            );
            assert_eq!(rp.candidates, ["Pypi"]);
        }
        _ => panic!("expected a Pypi run plan"),
    }
}

#[test]
fn github_strategy_url_is_the_releases_endpoint_brew_names() {
    let url = "https://github.com/owner/pkg/releases/download/v1.2/pkg.tgz";
    for (strat, want) in [
        (
            Strat::GithubLatest,
            "https://api.github.com/repos/owner/pkg/releases/latest",
        ),
        (
            Strat::GithubReleases,
            "https://api.github.com/repos/owner/pkg/releases",
        ),
    ] {
        let Ok(UrlPlan::Run(rp)) = github_plan(strat, url, None, Vec::new(), "1.0") else {
            panic!("{} plans no run for {url}", strat_name(strat));
        };
        assert_eq!(rp.strategy_url.as_deref(), Some(want));
    }
}

#[test]
#[cfg(unix)]
fn private_latest_release_uses_the_repository_routed_identity() {
    use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _, sync::Arc};
    let fake = agh_fake::FakeGithub::start();
    let routed = agh_fake::token("livecheck-routed");
    fake.add_token(&routed, "reader");
    fake.add_repo(
        "livecheck-private/pkg",
        agh_fake::RepoSpec::private().pull("reader"),
    );
    fake.add_release(
        "livecheck-private/pkg",
        agh_fake::ReleaseSpec::published("v3.2", "2026-01-01T00:00:00Z"),
    );
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dirs = agh::config::Dirs::under(&scratch.path().canonicalize().unwrap());
    // The routed token is the vault entry `reader`, read in place from the scratch home's store;
    // agh.toml names it by its `~/.secrets/reader` spelling and no file there is read.
    agh::identity::vault::put_entry_for_tests(&dirs.home, "reader", Some(routed.as_bytes()));
    let token_file = dirs.home.join(".secrets").join("reader");
    let text = format!(
        "[profiles.reader]\ntoken_file={:?}\ntier='dev'\nlogin='reader'\n[[owners]]\nname='livecheck-private'\ntier='dev'\nread=['reader']\n",
        token_file.to_str().unwrap()
    );
    std::fs::create_dir_all(dirs.config.parent().unwrap()).unwrap();
    aterm_uds::private_file::create_private_dir(&dirs.config).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dirs.config_file())
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
    drop(file);
    let ports = fake.ports();
    let env = Arc::new(
        agh::identity::MapEnv::new()
            .with("HOMEBREW_GITHUB_API_TOKEN", &agh_fake::token("unrouted")),
    );
    let base = agh::GithubBuilder::new(agh::policy::Router::empty())
        .env(env)
        .endpoints(agh::wire::Endpoints::loopback(
            ports.api,
            ports.uploads,
            ports.web,
            ports.cdn,
        ))
        .build();
    let client = bind_latest_routing(&base, Some(&dirs)).unwrap();
    let re = lc_regex(r"v(\d+\.\d+)", "").unwrap();
    assert!(matches!(
        github_latest_versions_with("https://github.com/livecheck-private/pkg", &re, None, |slug| client.releases(slug).latest()),
        Found::Versions(versions, _) if versions == ["3.2"]
    ));
    let reads = fake
        .seen()
        .into_iter()
        .filter(|row| row.path_and_query.ends_with("/releases/latest"))
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].as_login.as_deref(), Some("reader"));
    assert!(reads[0].had_authorization);
    let before = fake.seen().len();
    std::fs::write(dirs.config_file(), "[broken").unwrap();
    assert!(bind_latest_routing(&base, Some(&dirs)).is_err());
    assert_eq!(
        fake.seen().len(),
        before,
        "invalid routing must not fall back to a send"
    );
}

#[test]
fn github_latest_reads_the_routed_repository_and_refuses_nonstable_releases() {
    let fake = agh_fake::FakeGithub::start();
    fake.add_repo("sample/tool", agh_fake::RepoSpec::public());
    fake.add_repo("sample/private", agh_fake::RepoSpec::private());
    fake.add_release(
        "sample/private",
        agh_fake::ReleaseSpec::published("v1.2.3", "2026-11-11T00:00:00Z"),
    );
    let id = fake.add_release(
        "sample/tool",
        agh_fake::ReleaseSpec::published("v1.2.3", "2026-11-11T00:00:00Z"),
    );
    fake.set_latest("sample/tool", Some(id));
    let ports = fake.ports();
    let github = agh::GithubBuilder::new(agh::policy::Router::empty())
        .endpoints(agh::wire::Endpoints::loopback(
            ports.api,
            ports.uploads,
            ports.web,
            ports.cdn,
        ))
        .build();
    let re = lc_regex(r"v(\d+(?:\.\d+)+)", "").unwrap();
    for reject in [None, Some("draft"), Some("prerelease")] {
        let found =
            github_latest_versions_with("https://github.com/sample/tool.git", &re, None, |slug| {
                assert_eq!(slug.to_string(), "sample/tool");
                let mut release = github.releases(slug).latest()?;
                release.draft = reject == Some("draft");
                release.prerelease = reject == Some("prerelease");
                Ok(release)
            });
        let Found::Versions(versions, _) = found else {
            panic!("the release read did not return versions");
        };
        assert_eq!(
            versions,
            if reject.is_none() {
                vec!["1.2.3"]
            } else {
                vec![]
            }
        );
    }
    assert!(matches!(
        github_latest_versions_with("https://github.com/sample/private", &re, None, |slug| {
            github.releases(slug).latest()
        }),
        Found::Raised(_)
    ));
    assert!(matches!(
        github_latest_versions_with("https://github.com/sample/missing", &re, None, |slug| {
            github.releases(slug).latest()
        }),
        Found::Raised(_)
    ));
}

#[test]
fn regex_scan_and_search_extract_group1() {
    let re = lc_regex(r"^(?:jq[._-])?v?(\d+(?:\.\d+)+)$", "i").unwrap();
    assert_eq!(search_group1(&re, "jq-1.8.2").as_deref(), Some("1.8.2"));
    assert_eq!(search_group1(&re, "v1.7").as_deref(), Some("1.7"));
    assert_eq!(search_group1(&re, "other"), None);
    // Git's default regex: the greedy \D* prefix must
    // win over the capture (Ruby backtracking order).
    let git_default = lc_regex(r"\D*(.+)", "").unwrap();
    assert_eq!(
        search_group1(&git_default, "v3.15.1").as_deref(),
        Some("3.15.1")
    );
    assert_eq!(
        search_group1(&git_default, "release-2.0").as_deref(),
        Some("2.0")
    );
    let page = lc_regex(r"href=.*?foo[._-]v?(\d+(?:\.\d+)*)(?:\.[a-z]+|/)", "i").unwrap();
    let got = scan_versions(&page, "<a href=\"foo-1.2.tar.gz\">x</a> foo-9.9.zip");
    assert_eq!(got, ["1.2"]);
    // No capture: whole matches are the versions.
    let plain = lc_regex(r"\d+\.\d+", "").unwrap();
    assert_eq!(scan_versions(&plain, "a 1.2 b 3.4"), ["1.2", "3.4"]);
    assert_eq!(search_group1(&plain, "1.2"), None);
    // A quantified first group is outside the subset.
    assert!(lc_regex(r"(\d)+", "").is_err());
    // Ruby line anchors on a page.
    let anchored = lc_regex(r"^version (\d+)$", "").unwrap();
    assert_eq!(scan_versions(&anchored, "x\nversion 3\ny"), ["3"]);
    // r385 regression (ruby@3.3): a lazy prefix whose
    // first reach of the literal fails the rest, so
    // the match spans several candidate stretches.
    let ruby = lc_regex(r"href=.*?ruby[._-]v?(3\.3(?:\.\d+)+)\.t", "i").unwrap();
    assert_eq!(
        scan_versions(
            &ruby,
            "<li><a href=\"https://cache.ruby-lang.org/pub/ruby/3.3/ruby-3.3.13.tar.gz\">Ruby 3.3.13</a>"
        ),
        ["3.3.13"]
    );
}

#[test]
fn git_url_shapes() {
    assert_eq!(
        git_match("https://github.com/jqlang/jq/releases/download/jq-1.8.2/jq.tar.gz").as_deref(),
        Some("https://github.com/jqlang/jq.git")
    );
    assert_eq!(git_match("file:///tmp/repo.git"), None);
    assert_eq!(
        git_match("https://gitlab.com/x/y/-/archive/1.0/y.tar.gz").as_deref(),
        Some("https://gitlab.com/x/y.git")
    );
    assert_eq!(git_match("https://example.com/foo-1.0.tar.gz"), None);
}

#[test]
fn gnu_and_github_url_shapes() {
    assert_eq!(
        gnu_project("https://ftpmirror.gnu.org/wget/wget-1.25.0.tar.gz").as_deref(),
        Some("wget")
    );
    assert_eq!(gnu_project("https://ftpmirror.gnu.org/non-gnu/x/"), None);
    assert_eq!(
        github_repo("https://github.com/jqlang/jq.git"),
        Some(("jqlang".to_string(), "jq".to_string()))
    );
    assert_eq!(github_repo("https://example.com/a/b"), None);
}

#[test]
fn sparkle_newest_item_wins() {
    let cast = r#"<?xml version="1.0"?>
<rss><channel>
<item><title>1.0</title><pubDate>Wed, 01 Jan 2025 10:00:00 +0000</pubDate>
<enclosure url="https://example.com/a.zip" sparkle:shortVersionString="1.0" sparkle:version="100"/></item>
<item><title>2.0 (200)</title><pubDate>Thu, 02 Jan 2025 10:00:00 +0000</pubDate>
<enclosure url="https://example.com/b.zip" sparkle:shortVersionString="2.0" sparkle:version="200"/></item>
<item><title>win</title><pubDate>Fri, 03 Jan 2025 10:00:00 +0000</pubDate>
<enclosure url="https://example.com/c.zip" sparkle:shortVersionString="9.9" sparkle:os="windows"/></item>
</channel></rss>"#;
    assert_eq!(sparkle_newest(cast).as_deref(), Some("2.0,200"));
    assert_eq!(
        bundle_nice(Some("1.2"), Some("1.2.3")).as_deref(),
        Some("1.2.3")
    );
    assert_eq!(
        bundle_nice(Some("1.2 (34)"), Some("34")).as_deref(),
        Some("1.2,34")
    );
}

#[test]
fn pubdate_epoch_handles_rfc2822_and_iso() {
    let rfc = pubdate_epoch("Wed, 01 Jan 2025 10:00:00 +0000");
    assert_eq!(rfc, 1_735_725_600);
    assert_eq!(pubdate_epoch("2025-01-01T10:00:00Z"), rfc);
    assert_eq!(pubdate_epoch("2025-01-01T12:00:00+02:00"), rfc);
    assert_eq!(pubdate_epoch("2025-01-01"), 1_735_689_600);
    assert_eq!(pubdate_epoch("not a date"), 0);
    assert!(pubdate_epoch("2025-06-01T00:00:00Z") > rfc);
}

#[test]
fn header_merge_last_value_wins() {
    let text = "HTTP/2 302\r\nlocation: https://x.test/a-1.0.tgz\r\n\r\nHTTP/2 200\r\ncontent-disposition: attachment; filename=\"a-2.0.tgz\"\r\n";
    let merged = merge_response_headers(text);
    assert_eq!(merged["location"], "https://x.test/a-1.0.tgz");
    assert!(merged["content-disposition"].contains("a-2.0.tgz"));
}

#[test]
fn cask_comma_versions_compare_segmentwise() {
    assert_eq!(lc_cmp(true, "1.2,300", "1.2,400"), Ordering::Less);
    assert_eq!(lc_cmp(true, "1.3,1", "1.2,999"), Ordering::Greater);
    assert_eq!(lc_cmp(false, "1.0", "1.0.1"), Ordering::Less);
}

/// r419: the RubyGems gem name, greedy to the last
/// versioned hyphen before `.gem` (329/329 vs the
/// oracle generator corpus), and /gems/<x>/versions/.
#[test]
fn ruby_gem_name_mirrors_the_oracle_generator() {
    let g = |u: &str| ruby_gem_name(u);
    assert_eq!(
        g("https://rubygems.org/downloads/bundler-2.5.9.gem").as_deref(),
        Some("bundler")
    );
    assert_eq!(
        g("https://rubygems.org/downloads/foo-bar-1.0-x64-mingw32.gem").as_deref(),
        Some("foo-bar")
    );
    assert_eq!(
        g("https://rubygems.org/downloads/activesupport-7.1.3.2.gem").as_deref(),
        Some("activesupport")
    );
    assert_eq!(
        g("https://rubygems.org/gems/rake/versions/rake-13.2.1.gem").as_deref(),
        Some("rake")
    );
    assert_eq!(g("https://rubygems.org/downloads/noversion.gem"), None);
    assert_eq!(g("https://rubygems.org/api/v1/gems/rake.json"), None);
}

/// r420: the Launchpad project is the first path
/// segment on any launchpad.net host (27/27 vs the
/// oracle generator corpus).
#[test]
fn launchpad_project_mirrors_the_oracle_generator() {
    let g = |u: &str| launchpad_project(u);
    assert_eq!(
        g("https://launchpad.net/intltool/trunk/0.51.0/+download/intltool-0.51.0.tar.gz")
            .as_deref(),
        Some("intltool")
    );
    assert_eq!(
        g("https://bugs.launchpad.net/ubuntu/+source/ncurses/+bug/1").as_deref(),
        Some("ubuntu")
    );
    assert_eq!(
        g("https://launchpad.net/libmetalink/").as_deref(),
        Some("libmetalink")
    );
    assert_eq!(g("https://launchpad.net"), None);
    assert_eq!(g("https://launchpad.net/"), None);
    assert_eq!(g("https://notlaunchpad.net/foo/"), None);
    assert_eq!(g("https://example.com/launchpad.net/foo"), None);
}

/// r421: Bitbucket generated values (30/30 vs the
/// oracle generator corpus, regex source included):
/// lazy prefix, dotted version, tarball suffix
/// collapse; `/get/` checks the tags tab.
#[test]
fn bitbucket_values_mirror_the_oracle_generator() {
    let v = |u: &str| bitbucket_values(u);
    assert_eq!(
        v("https://bitbucket.org/agalanin/fuse-zip/downloads/fuse-zip-0.7.2.tar.gz"),
        Some((
            "https://bitbucket.org/agalanin/fuse-zip/downloads/?iframe=true&spa=0".to_string(),
            r"href=.*?fuse-zip-v?(\d+(?:\.\d+)+)\.t".to_string(),
            "i"
        ))
    );
    assert_eq!(
        v("https://bitbucket.org/amc-nicemice/par/get/1.53.0.tar.bz2"),
        Some((
            "https://bitbucket.org/amc-nicemice/par/downloads/?tab=tags&iframe=true&spa=0"
                .to_string(),
            r#"<td[^>]*?class="name"[^>]*?>\s*v?(\d+(?:\.\d+)+)\s*?<"#.to_string(),
            "im"
        ))
    );
    // The version stops at the first hyphen after a
    // dotted run; the rest is suffix (mecab shape).
    assert_eq!(
        v("https://bitbucket.org/eunjeon/mecab-ko/downloads/mecab-0.996-ko-0.9.2.tar.gz")
            .map(|x| x.1),
        Some(r"href=.*?mecab-v?(\d+(?:\.\d+)+)-ko-0\.9\.2\.t".to_string())
    );
    assert_eq!(v("https://bitbucket.org/foo/bar"), None);
    assert_eq!(v("https://example.com/foo/downloads/x-1.2.tar.gz"), None);
}

/// r422: ElectronBuilder URL match edges, as probed
/// against the oracle's `URL_MATCH_REGEX` (explicit
/// only, priority 0).
#[test]
fn electron_builder_match_mirrors_the_oracle() {
    let m = electron_builder_match;
    assert!(m("https://x.io/a/b.yaml"));
    assert!(m("https://x.io/a/b.yml?x=1"));
    assert!(m("https://x.io/b.yml"));
    assert!(m("https://x.io/a/b.YML"));
    assert!(m("http://x.io/a/b.yml"));
    assert!(!m("https://x.io/"));
    assert!(!m("https://x.io/a/b.yml/"));
    assert!(!m("https://x.io/a/.yml"));
    assert!(!m("https://x.io/a/b.yml?x=1?y=2"));
    assert!(!m("ftp://x.io/a/b.yml"));
}

/// r423: Ruby `String#tr` semantics for the ScanTr
/// page-block shape (sets, ranges, short-to padding,
/// empty-to deletion).
#[test]
fn ruby_tr_mirrors_ruby() {
    assert_eq!(ruby_tr("1_2_3", "_", "."), "1.2.3");
    assert_eq!(ruby_tr("1/2", "/", "."), "1.2");
    assert_eq!(ruby_tr("2_8R1", "_R", ".r"), "2.8r1");
    assert_eq!(ruby_tr("abc", "a-c", "x"), "xxx");
    assert_eq!(ruby_tr("a-b", "-", "."), "a.b");
    assert_eq!(ruby_tr("a_b", "_", ""), "ab");
    assert_eq!(ruby_tr("hello", "el", "ip"), "hippo");
}

/// r424: `Date.parse` ("%Y%m%d") for month-name
/// captures, calendar-validated like Ruby.
#[test]
fn date_ymd_mirrors_ruby_date_parse() {
    assert_eq!(date_ymd("09Mar2016").as_deref(), Some("20160309"));
    assert_eq!(date_ymd("March 9, 2016").as_deref(), Some("20160309"));
    assert_eq!(date_ymd("September 30, 2024").as_deref(), Some("20240930"));
    assert_eq!(date_ymd("2016Mar09").as_deref(), Some("20160309"));
    assert_eq!(date_ymd("Feb 29, 2024").as_deref(), Some("20240229"));
    assert_eq!(date_ymd("Feb 29, 2023"), None);
    assert_eq!(date_ymd("Smarch 9, 2016"), None);
    assert_eq!(date_ymd("March 2016"), None);
    // r436: all-numeric forms as the oracle's
    // Date.parse reads them (probed): year-first,
    // day-first with a trailing 4-digit year,
    // two-digit-year-first with the 69 pivot, and
    // the compact forms.
    assert_eq!(date_ymd("2016-03-09").as_deref(), Some("20160309"));
    assert_eq!(date_ymd("2016/3/9").as_deref(), Some("20160309"));
    assert_eq!(date_ymd("09-03-2016").as_deref(), Some("20160309"));
    assert_eq!(date_ymd("03/09/2016").as_deref(), Some("20160903"));
    assert_eq!(date_ymd("97/11/13").as_deref(), Some("19971113"));
    assert_eq!(date_ymd("68/01/02").as_deref(), Some("20680102"));
    assert_eq!(date_ymd("69/01/02").as_deref(), Some("19690102"));
    assert_eq!(date_ymd("20160309").as_deref(), Some("20160309"));
    assert_eq!(date_ymd("970313").as_deref(), Some("19970313"));
    assert_eq!(date_ymd("11/13/1997"), None);
    assert_eq!(date_ymd("2024-13-01"), None);
    // Regexp#inspect orders flags m, i, x.
    assert_eq!(
        regex_inspect(&lc_regex("a", "im").unwrap()),
        "/a/mi".to_string()
    );
}

/// r425: the gsub/sub/reject page shapes apply like
/// their Ruby originals.
#[test]
fn page_shape_gsub_sub_reject() {
    use super::super::super::formula::PageShape;
    let apply = |shape: &PageShape, v: &str, re: Option<&LcRegex>| page_shape_apply(shape, v, re);
    let g = PageShape::ScanGsub("_rev_".into(), ".".into());
    assert_eq!(apply(&g, "23_rev_9", None), Ok(Some("23.9".to_string())));
    let sub_re = lc_regex("%2Bdbg", "i").ok();
    let sub = PageShape::ScanSub("%2Bdbg".into(), "i".into(), "".into());
    assert_eq!(
        apply(&sub, "1.2%2Bdbg-3", sub_re.as_ref()),
        Ok(Some("1.2-3".to_string()))
    );
    assert_eq!(
        apply(&sub, "1.2", sub_re.as_ref()),
        Ok(Some("1.2".to_string()))
    );
    let rej = PageShape::ScanRejectEq("2.22".into());
    assert_eq!(apply(&rej, "2.22", None), Ok(None));
    assert_eq!(apply(&rej, "2.30", None), Ok(Some("2.30".to_string())));
}

/// r426: a reduced `:github_releases` block applies
/// its guards/source/transform over release objects.
#[test]
fn github_releases_block_applies() {
    use super::super::super::formula::{GhRelBlock, GhRelSource, GhRelThen};
    fn rel(tag: &str, name: Option<&str>, draft: bool, pre: bool) -> agh::model::ReleaseObject {
        agh::model::ReleaseObject {
            id: 1,
            tag_name: tag.to_string(),
            name: name.map(str::to_string),
            draft,
            prerelease: pre,
            immutable: false,
            target_commitish: String::new(),
            created_at: String::new(),
            published_at: None,
            html_url: String::new(),
            body: None,
            assets: Vec::new(),
            raw: aterm_json::Value::Null,
        }
    }
    let re = lc_regex(r"^v?(\d+(?:[.-]\d+)+)$", "i").unwrap();
    let releases = vec![
        rel("v1-2", Some("API v1-2"), false, false),
        rel("v1.3", Some("Tool 1.3"), false, true),
        rel("v1.4", Some("Tool 1.4"), false, false),
        rel("v9.9", Some("Tool 9.9"), true, false),
    ];
    let block = GhRelBlock {
        skip_draft: true,
        skip_prerelease: false,
        require_prerelease: false,
        tag_ne: None,
        tag_prefix: None,
        asset_any: None,
        name_skip: Some(("^api".into(), "i".into())),
        source: GhRelSource::TagName,
        pre_capture: None,
        then: GhRelThen::Tr("-".into(), ".".into()),
        fallback_tag: None,
        blank_emit_local: false,
        limit: None,
        own_guards: Vec::new(),
    };
    let found = github_releases_versions_with(
        "https://github.com/sample/tool/releases/download/v1/tool.tgz",
        &re,
        Some(&block),
        "1.0",
        |_| Ok(releases),
    );
    assert!(
        matches!(found, Found::Versions(v, _) if v == ["1.3", "1.4"]),
        "draft skipped, prerelease kept, API-named skipped"
    );
    assert!(version_patch_nonzero("3.5.1"));
    assert!(!version_patch_nonzero("3.5.0"));
    assert!(!version_patch_nonzero("3.5"));
}

/// r427: YAML block scalars (`>-` folded appcast
/// values) parse; clip vs strip chomping.
#[test]
fn yaml_block_scalars_parse() {
    let doc = "version: 2.8.55\nfiles:\n  - url: >-\n      https://example.com/app.zip\n    size: 12\nnotes: |\n  line1\n  line2\n";
    let root = yaml_parse(doc).unwrap();
    let YNode::Map(entries) = &root else {
        panic!("map root")
    };
    let get = |k: &str| entries.iter().find(|(ek, _)| ek == k).map(|(_, v)| v);
    assert!(matches!(get("version"), Some(YNode::Scalar(v, _)) if v == "2.8.55"));
    let Some(YNode::Seq(items)) = get("files") else {
        panic!("files seq")
    };
    let Some(YNode::Map(first)) = items.first() else {
        panic!("file map")
    };
    let url = first.iter().find(|(k, _)| k == "url").map(|(_, v)| v);
    assert!(matches!(url, Some(YNode::Scalar(v, _)) if v == "https://example.com/app.zip"));
    assert!(matches!(get("notes"), Some(YNode::Scalar(v, _)) if v == "line1\nline2\n"));
}

/// r428: regex-less HeaderMatch detects versions
/// from raw header values (the oracle's
/// `Version.parse` fallback on content-disposition
/// and location).
#[test]
fn header_values_detect_versions() {
    assert_eq!(
        version_parse("https://downloads.example.org/gui/monero-gui-mac-armv8-v0.18.5.4.dmg")
            .as_deref(),
        Some("0.18.5.4")
    );
    // The trailing quote keeps ".tar" in the stem —
    // the oracle detects the same string.
    assert_eq!(
        version_parse("attachment; filename=\"spidermonkey-140.11.0.tar.xz\"").as_deref(),
        Some("140.11.0.tar")
    );
}

/// r430: multi-capture extraction mirrors Ruby
/// `scan` arrays (pythia's adjacent groups, jasp's
/// optional nested second group).
#[test]
fn extract_groups_mirror_ruby_scan() {
    let re = lc_regex(r"href=.*?pythia(\d)(\d{3})\.t", "i").unwrap();
    assert_eq!(re.group_count, 2);
    let m = re.whole.find("xx href=\"pythia8315.tar.gz\" yy").unwrap();
    assert_eq!(
        extract_groups(&re, m.as_str()),
        vec![Some("8".to_string()), Some("315".to_string())]
    );
    let re = lc_regex(
        r"href=.*?JASP[._-]v?(\d+(?:\.\d+)+)[._-]macOS[._-]arm64(?:_(\d+))?\.dmg",
        "i",
    )
    .unwrap();
    assert_eq!(re.group_count, 2);
    let content =
        "href=\"x/JASP-0.99.0.0-macOS-arm64.dmg\" href=\"y/JASP-0.99.1-macOS-arm64_2.dmg\"";
    let got: Vec<Vec<Option<String>>> = re
        .whole
        .find_iter(content)
        .map(|mm| extract_groups(&re, mm.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            vec![Some("0.99.0.0".to_string()), None],
            vec![Some("0.99.1".to_string()), Some("2".to_string())],
        ]
    );
}

/// r430: group extraction on a page-wide match
/// (>512 chars) finds the split Ruby's backtracking
/// lands on, not the first version-shaped text in
/// the match (the old re-search shortcut).
#[test]
fn extract_group_long_match_finds_real_split() {
    let re = lc_regex(
        r"href=.*?JASP[._-]v?(\d+(?:\.\d+)+)[._-]macOS[._-]arm64(?:_(\d+))?\.dmg",
        "im",
    )
    .unwrap();
    let filler = " filler 9.9 and 1.0 text".repeat(40);
    let page =
        format!("href=\"/x\"{filler} content=\"0.05;url=z\" JASP-0.99.0.0-MacOS-arm64.dmg end");
    let m = re.whole.find(&page).unwrap();
    assert!(m.as_str().len() > 512);
    assert_eq!(
        extract_groups(&re, m.as_str()),
        vec![Some("0.99.0.0".to_string()), None]
    );
}

/// r482: a long section capture needs the larger
/// exhaustive-split budget (inkscape's table rows).
#[test]
fn extract_group_limited_long_section() {
    let re = lc_regex(r"<tr[^>]*?>(.*?)</tr>", "im").unwrap();
    let section = " release row".repeat(80);
    let page = format!("<tr class=\"even\">{section}</tr>");
    let m = re.whole.find(&page).unwrap();
    assert!(m.as_str().len() > 512);
    assert_eq!(
        extract_group_limited(&re, m.as_str(), 65_536).as_deref(),
        Some(section.as_str())
    );
}

/// r432: URI.join for directory-walk fetches.
#[test]
fn uri_join_directory_forms() {
    assert_eq!(
        uri_join("https://example.org/util-linux/", "v2.41/"),
        "https://example.org/util-linux/v2.41/"
    );
    assert_eq!(
        uri_join("https://example.org/texlive/", "2024"),
        "https://example.org/texlive/2024"
    );
    assert_eq!(
        uri_join("https://example.org/a/page.html", "b/"),
        "https://example.org/a/b/"
    );
    assert_eq!(version_patch_value("4.3.2"), 2);
    assert_eq!(version_patch_value("4.3"), 0);
}

/// r433: reduced `:git` block transforms over a tag
/// capture (oracle `String#tr`/`#gsub` semantics).
#[test]
fn git_op_transforms() {
    use super::super::super::formula::GitOp;
    assert_eq!(git_op_apply(&GitOp::Identity, "1_2", None), "1_2");
    assert_eq!(
        git_op_apply(&GitOp::Tr("_-".into(), ".".into()), "1_2-3", None),
        "1.2.3"
    );
    assert_eq!(
        git_op_apply(&GitOp::GsubLit("8u".into(), "1.8.0+".into()), "8u432", None),
        "1.8.0+432"
    );
    // libtrace: trailing `-1` stripped, other text kept.
    let re = lc_regex("-1$", "").unwrap();
    let op = GitOp::GsubRe("-1$".into(), String::new(), String::new());
    assert_eq!(git_op_apply(&op, "4.0.1-1", Some(&re)), "4.0.1");
    assert_eq!(git_op_apply(&op, "4.0.1", Some(&re)), "4.0.1");
    // tmpwatch: every separator replaced.
    let re = lc_regex("[_-]", "").unwrap();
    let op = GitOp::GsubRe("[_-]".into(), String::new(), ".".into());
    assert_eq!(git_op_apply(&op, "1_2-3_4", Some(&re)), "1.2.3.4");
    // json_spirit: backreference regrouping.
    let re = lc_regex(r"(\d+)\.(\d+)\.(\d+)", "").unwrap();
    let op = GitOp::GsubRe(
        r"(\d+)\.(\d+)\.(\d+)".into(),
        String::new(),
        r"\1.\2\3".into(),
    );
    assert_eq!(git_op_apply(&op, "4.0.8", Some(&re)), "4.08");
    assert_eq!(git_op_apply(&op, "4.08", Some(&re)), "4.08");
}

/// r434: a reduced `:github_latest` block reads its
/// field (no draft/prerelease filter), scans or
/// captures, applies `tr`, and dedupes.
#[test]
fn github_latest_block_applies() {
    use super::super::super::formula::{LatestBlock, LatestRender, LatestSource};
    fn asset(name: &str) -> agh::model::AssetObject {
        agh::model::AssetObject {
            id: 1,
            name: name.to_string(),
            size: 0,
            digest: None,
            state: String::new(),
            content_type: String::new(),
            updated_at: String::new(),
            browser_download_url: String::new(),
            download_count: 0,
            raw: aterm_json::Value::Null,
        }
    }
    let release = agh::model::ReleaseObject {
        id: 1,
        tag_name: "R_2_7_1".to_string(),
        name: Some("Tool 2.7 and 2.8".to_string()),
        draft: true,
        prerelease: true,
        immutable: false,
        target_commitish: String::new(),
        created_at: String::new(),
        published_at: Some("2026-11-03T00:00:00Z".to_string()),
        html_url: String::new(),
        body: None,
        assets: vec![
            asset("tool-3.0.tgz"),
            asset("notes.txt"),
            asset("tool-3.0.zip"),
        ],
        raw: aterm_json::Value::Null,
    };
    let base = LatestBlock {
        needs_spec_regex: false,
        source: LatestSource::TagName,
        scan: false,
        tr: None,
        render: LatestRender::Capture,
        two_b: None,
        pre_capture: None,
        match_pre: false,
        asset_pick: None,
    };
    // Scan + tr over tag_name (draft/prerelease kept:
    // the block path has no filter).
    let re = lc_regex(r"^R_(\d+(?:_\d+)+)$", "").unwrap();
    let block = LatestBlock {
        scan: true,
        tr: Some(("_".into(), ".".into())),
        ..base.clone()
    };
    assert_eq!(
        latest_block_versions(&release, &re, &block),
        vec!["2.7.1".to_string()]
    );
    // Scan over name finds every match.
    let re = lc_regex(r"(\d+\.\d+)", "").unwrap();
    let block = LatestBlock {
        source: LatestSource::Name,
        scan: true,
        ..base.clone()
    };
    assert_eq!(
        latest_block_versions(&release, &re, &block),
        vec!["2.7".to_string(), "2.8".to_string()]
    );
    // First capture over published_at with deleting tr.
    let re = lc_regex(r"^(\d{4}-\d{2}-\d{2})", "").unwrap();
    let block = LatestBlock {
        source: LatestSource::PublishedAt,
        tr: Some(("-".into(), String::new())),
        ..base.clone()
    };
    assert_eq!(
        latest_block_versions(&release, &re, &block),
        vec!["20261103".to_string()]
    );
    // Per-asset captures, deduplicated.
    let re = lc_regex(r"^tool-(\d+(?:\.\d+)+)\.", "").unwrap();
    let block = LatestBlock {
        source: LatestSource::AssetName,
        ..base.clone()
    };
    assert_eq!(
        latest_block_versions(&release, &re, &block),
        vec!["3.0".to_string()]
    );
    // A missing optional field yields nothing.
    let block = LatestBlock {
        source: LatestSource::Body,
        ..base
    };
    assert!(latest_block_versions(&release, &re, &block).is_empty());
}

/// r435: the `none?` filename-match guard (go's
/// stable-release filter) evaluates against the
/// block regex as a search.
#[test]
fn json_none_match_guard() {
    use super::super::super::formula::JsonCond;
    let rp = RunPlan {
        strat: Strat::Json,
        url: String::new(),
        re: Some(lc_regex(r"^go[._-]?v?(\d+(?:\.\d+)+)[._-]src\.t.+$", "i").unwrap()),
        candidates: Vec::new(),
        strategy_url: None,
        ua: None,
        ua_option: None,
        ua_symbol: false,
        gnome_filter: false,
        json: None,
        xml: None,
        yaml: None,
        page_block: None,
        ghrel_block: None,
        git: None,
        sparkle_block: None,
        header_block: None,
        latest: None,
        post_form: None,
        post_json: None,
        headers: Vec::new(),
        major_minor: String::new(),
        own_version: String::new(),
        plist: None,
        apps: Vec::new(),
        uncompressed: false,
    };
    let cond = JsonCond::NoneMatch("files".into(), "filename".into());
    let item = |json: &str| aterm_json::from_slice::<aterm_json::Value>(json.as_bytes()).unwrap();
    // A src tarball among the files: not none.
    assert!(!eval_json_cond(
        &cond,
        &item(
            r#"{"files":[{"filename":"go1.27.4.darwin-arm64.tar.gz"},{"filename":"go1.27.4.src.tar.gz"}]}"#
        ),
        &rp,
        None
    ));
    // No src tarball: none match (the item is skipped).
    assert!(eval_json_cond(
        &cond,
        &item(r#"{"files":[{"filename":"go1.27.4.darwin-arm64.tar.gz"}]}"#),
        &rp,
        None
    ));
    // Missing/empty file list: none match.
    assert!(eval_json_cond(&cond, &item(r#"{"files":[]}"#), &rp, None));
    assert!(eval_json_cond(&cond, &item(r#"{}"#), &rp, None));
}

/// r437: `URI.encode_www_form` — alphanumerics and
/// `*-._` pass, space is `+`, the rest percent-escapes
/// (oracle-probed byte forms).
#[test]
fn www_form_encoding() {
    let pairs = vec![
        ("action".to_string(), "percona_downloads".to_string()),
        ("product_id".to_string(), "Percona-Server-9.7".to_string()),
    ];
    assert_eq!(
        www_form_encode(&pairs),
        "action=percona_downloads&product_id=Percona-Server-9.7"
    );
    let pairs = vec![
        ("a b".to_string(), "x+y&z".to_string()),
        ("k".to_string(), "~ok*".to_string()),
    ];
    assert_eq!(www_form_encode(&pairs), "a+b=x%2By%26z&k=%7Eok*");
}

/// r438: document-order object keys at a dig path
/// (the parsed map sorts; Ruby hashes do not).
#[test]
fn json_key_order_document() {
    use super::super::super::formula::JsonKey;
    let doc = r#"{"releases":{"3.4":{"children":[1,2]},"3.3":{"children":[]},"we\"ird":{}},"other":[true,null]}"#;
    assert_eq!(
        json_object_key_order(doc, &[JsonKey::Key("releases".into())]),
        Some(vec![
            "3.4".to_string(),
            "3.3".to_string(),
            "we\"ird".to_string()
        ])
    );
    assert_eq!(
        json_object_key_order(doc, &[]),
        Some(vec!["releases".to_string(), "other".to_string()])
    );
    assert_eq!(
        json_object_key_order(doc, &[JsonKey::Key("nope".into())]),
        None
    );
    assert_eq!(
        json_object_key_order("[1,2]", &[JsonKey::Key("a".into())]),
        None
    );
}

/// r441: a capture inside an alternation branch
/// splits via the rebalanced prefix/suffix (the
/// elfutils directory/tarball regex); the other
/// branch's whole match yields no group 1.
#[test]
fn alternation_branch_group1_extracts() {
    let re = lc_regex(
        r#"href=(?:["']?v?(\d+(?:\.\d+)+)/?["' >]|.*?elfutils[._-]v?(\d+(?:\.\d+)+)\.t)"#,
        "i",
    )
    .unwrap();
    assert_eq!(re.group_count, 2);
    assert_eq!(
        search_group1(&re, "href=\"0.197/\"").as_deref(),
        Some("0.197")
    );
    assert_eq!(search_group1(&re, "href=\"elfutils-0.197.tar.xz\""), None);
}

/// r442: Ruby pattern syntax translated for the
/// engine — `{,n}` open-below quantifiers (gyb) and
/// `x`-mode free whitespace/comments (neo4j).
#[test]
fn ruby_pattern_translations() {
    let re = lc_regex(r"^v?(\d{,3}(?:\.\d+)+)$", "i").unwrap();
    assert_eq!(search_group1(&re, "v12.345").as_deref(), Some("12.345"));
    let re = lc_regex(
        "href=.*?rel=v?(\\d+(?:\\.\\d+)+) # the release\n |other",
        "ix",
    )
    .unwrap();
    assert_eq!(
        search_group1(&re, "href=xrel=1.2.3").as_deref(),
        Some("1.2.3")
    );
}

/// r443: reduced `:sparkle` blocks over an appcast
/// — channel-nil find skips the newer beta item;
/// a match template joins fields and captures.
#[test]
fn sparkle_block_versions_eval() {
    use super::super::super::formula::{SpChannel, SpExpr, SpField, SpPart, SparkleBlock};
    let appcast = "<rss><channel>\
        <item><title>3.0</title><pubDate>Mon, 02 Nov 2026 09:34:27 +0000</pubDate>\
        <sparkle:channel>beta</sparkle:channel>\
        <enclosure url=\"https://example.com/v3-99.dmg\" sparkle:shortVersionString=\"3.0\" sparkle:version=\"300\"/></item>\
        <item><title>2.0</title><pubDate>Sat, 15 Aug 2026 07:20:14 +0000</pubDate>\
        <enclosure url=\"https://example.com/v2-42.dmg\" sparkle:shortVersionString=\"2.0\" sparkle:version=\"200\"/></item>\
        </channel></rss>";
    let find = SparkleBlock {
        all: false,
        find: false,
        channel: Some(SpChannel::Nil),
        match_guard: None,
        present: Vec::new(),
        needs_spec_regex: false,
        own_if: None,
        expr: SpExpr::Field(SpField::Short),
    };
    assert_eq!(
        sparkle_block_versions(appcast, &find, None, "1.0"),
        vec!["2.0"]
    );
    let tmpl = SparkleBlock {
        all: false,
        find: false,
        channel: None,
        match_guard: None,
        present: Vec::new(),
        needs_spec_regex: true,
        own_if: None,
        expr: SpExpr::Template {
            from: Some(SpField::Url),
            local: None,
            parts: vec![
                SpPart::Field(SpField::Short),
                SpPart::Lit(",".to_string()),
                SpPart::Cap(1),
            ],
        },
    };
    let re = lc_regex(r"-(\d+)\.dmg", "").unwrap();
    assert_eq!(
        sparkle_block_versions(appcast, &tmpl, Some(&re), "1.0"),
        vec!["3.0,99"]
    );
}

/// r444: scan captures index from 0 (bartender's
/// `match[0].tr("-", ".")` over element-form
/// appcast items).
#[test]
fn sparkle_scan_capture_zero_is_first_group() {
    use super::super::super::formula::{SpExpr, SpField, SpPart, SparkleBlock};
    let appcast = "<rss><channel><item><title>Version 7.2.0</title>        <sparkle:shortVersionString>7.2.0</sparkle:shortVersionString>        <pubDate>Thu, 05 Nov 2026 17:41:58 GMT</pubDate>        <enclosure url=\"https://example.com/updates/7-2-0/App%207.zip\" /></item></channel></rss>";
    let block = SparkleBlock {
        all: false,
        find: false,
        channel: None,
        match_guard: None,
        present: Vec::new(),
        needs_spec_regex: true,
        own_if: None,
        expr: SpExpr::Scan {
            from: SpField::Url,
            parts: vec![SpPart::CapTr(0, "-".into(), ".".into())],
        },
    };
    let re = lc_regex(r"/v?(\d+(?:[.-]\d+)+)/App%207\.zip", "i").unwrap();
    assert_eq!(
        sparkle_block_versions(appcast, &block, Some(&re), "1.0"),
        vec!["7.2.0"]
    );
}

/// r444: cask version chains substitute with
/// `Cask::DSL::Version` semantics.
#[test]
fn cask_version_chain_substitution() {
    assert_eq!(
        substitute_version_tokens(
            "v#{version.csv.first.major}/x#{version.no_dots}",
            "2.10.1,406",
            true
        )
        .as_deref(),
        Some("v2/x2101,406")
    );
    assert_eq!(
        substitute_version_tokens("#{version.major_minor}", "2.10.1,406", true).as_deref(),
        Some("2.10")
    );
    assert_eq!(
        substitute_version_tokens("#{version.csv.second}", "1.2", true).as_deref(),
        Some("")
    );
    // Formula semantics reject cask-only chains.
    assert_eq!(
        substitute_version_tokens("#{version.no_dots}", "1.2.3", false),
        None
    );
}

/// r445: a json template body over items (locals,
/// blank guards, CSV render), and a match template.
#[test]
fn json_template_eval() {
    use super::super::super::formula::{JsonKey, JsonLocal, JsonPart, JsonRender, JsonTemplate};
    let rp = RunPlan {
        strat: Strat::Json,
        url: String::new(),
        re: Some(lc_regex(r"/v(\d+)-(\d+)", "").unwrap()),
        candidates: Vec::new(),
        strategy_url: None,
        ua: None,
        ua_option: None,
        ua_symbol: false,
        gnome_filter: false,
        json: None,
        xml: None,
        yaml: None,
        page_block: None,
        ghrel_block: None,
        git: None,
        sparkle_block: None,
        header_block: None,
        latest: None,
        post_form: None,
        post_json: None,
        headers: Vec::new(),
        major_minor: String::new(),
        own_version: String::new(),
        plist: None,
        apps: Vec::new(),
        uncompressed: false,
    };
    let root = aterm_json::from_slice::<aterm_json::Value>(
        br#"{"releases":[{"version":"1.2","build":34},{"version":"1.3"}],"url":"https://x/v9-8"}"#,
    )
    .unwrap();
    let map_tmpl = JsonTemplate {
        path: vec![JsonKey::Key("releases".into())],
        map: true,
        values: false,
        locals: vec![
            JsonLocal::Dig(vec![JsonKey::Key("version".into())]),
            JsonLocal::Dig(vec![JsonKey::Key("build".into())]),
        ],
        require: vec![0, 1],
        match_src: None,
        local_match_guard: None,
        guards: Vec::new(),
        finds: Vec::new(),
        require_find: Vec::new(),
        render: JsonRender::Template(vec![
            JsonPart::Local(0),
            JsonPart::Lit(",".into()),
            JsonPart::Local(1),
        ]),
    };
    // Numeric build interpolates; the item without
    // a build is skipped by the blank guard.
    let Ok(versions) = json_template_versions(&map_tmpl, &root, &rp) else {
        panic!("template eval failed");
    };
    assert_eq!(versions, vec!["1.2,34"]);
    let match_tmpl = JsonTemplate {
        path: Vec::new(),
        map: false,
        values: false,
        locals: Vec::new(),
        require: Vec::new(),
        match_src: Some(vec![JsonKey::Key("url".into())]),
        local_match_guard: None,
        guards: Vec::new(),
        finds: Vec::new(),
        require_find: Vec::new(),
        render: JsonRender::Template(vec![
            JsonPart::Cap(2),
            JsonPart::Lit(",".into()),
            JsonPart::Cap(1),
        ]),
    };
    let Ok(versions) = json_template_versions(&match_tmpl, &root, &rp) else {
        panic!("template eval failed");
    };
    assert_eq!(versions, vec!["8,9"]);
}

/// r447: xml template evaluation over a mini DOM
/// (plist key/next-element locals + CSV render;
/// attr predicate map).
#[test]
fn xml_template_eval() {
    use super::super::super::formula::{
        XmlLocal, XmlMap, XmlPart, XmlPathQ, XmlPred, XmlProj, XmlRender, XmlStep, XmlTemplate,
    };
    let doc = "<plist><dict><key>version</key><string> 7.1 </string><key>build</key><string>99</string></dict></plist>";
    let key = |v: &str| XmlLocal {
        path: XmlPathQ {
            absolute: false,
            relative: false,
            steps: vec![
                XmlStep {
                    tag: "dict".into(),
                    pred: None,
                },
                XmlStep {
                    tag: "key".into(),
                    pred: Some(XmlPred::TextEq(v.into())),
                },
            ],
        },
        next: true,
        from: None,
    };
    let rp = RunPlan {
        strat: Strat::Xml,
        url: String::new(),
        re: None,
        candidates: Vec::new(),
        strategy_url: None,
        ua: None,
        ua_option: None,
        ua_symbol: false,
        gnome_filter: false,
        json: None,
        xml: None,
        yaml: None,
        page_block: None,
        ghrel_block: None,
        git: None,
        sparkle_block: None,
        header_block: None,
        latest: None,
        post_form: None,
        post_json: None,
        headers: Vec::new(),
        major_minor: String::new(),
        own_version: String::new(),
        plist: None,
        apps: Vec::new(),
        uncompressed: false,
    };
    let tmpl = XmlTemplate {
        locals: vec![key("version"), key("build")],
        elems: Vec::new(),
        require: vec![0, 1],
        require_elems: Vec::new(),
        match_local: None,
        or_pairs: Vec::new(),
        map: None,
        render: XmlRender::Template(vec![
            XmlPart::LocalStrip(0),
            XmlPart::Lit(",".into()),
            XmlPart::LocalStrip(1),
        ]),
    };
    assert_eq!(
        xml_template_versions(&tmpl, doc, &rp).unwrap(),
        vec!["7.1,99"]
    );
    let doc = "<feed><entry v=\"1.0\"/><entry v=\"2.0\"/></feed>";
    let tmpl = XmlTemplate {
        locals: Vec::new(),
        elems: Vec::new(),
        require: Vec::new(),
        require_elems: Vec::new(),
        match_local: None,
        or_pairs: Vec::new(),
        map: Some(XmlMap {
            path: XmlPathQ {
                absolute: false,
                relative: false,
                steps: vec![XmlStep {
                    tag: "entry".into(),
                    pred: None,
                }],
            },
            proj: XmlProj::Attr("v".into()),
            grep: false,
            locals: Vec::new(),
        }),
        render: XmlRender::Projection,
    };
    assert_eq!(
        xml_template_versions(&tmpl, doc, &rp).unwrap(),
        vec!["1.0", "2.0"]
    );
}

/// r468: an empty prefix/suffix pattern pins the
/// split boundary (imagej's `(\d)(\d+)` sub read
/// "154" as groups ("5","54") before).
#[test]
fn empty_edge_splits_are_pinned() {
    let re = lc_regex(r"(\d)(\d+)", "").unwrap();
    assert_eq!(
        extract_groups(&re, "154"),
        [Some("1".to_string()), Some("54".to_string())]
    );
    assert_eq!(sub_backref_apply("154", "\\1.\\2", Some(&re)), "1.54");
    assert_eq!(sub_backref_apply("x154y", "\\1.\\2", Some(&re)), "x1.54y");
    // A lone group spans the whole match.
    let re = lc_regex(r"(\d+)", "").unwrap();
    assert_eq!(extract_group(&re, "154").as_deref(), Some("154"));
    // Greedy first group still backtracks for the last.
    let re = lc_regex(r"(\d+)(\d)", "").unwrap();
    assert_eq!(
        extract_groups(&re, "154"),
        [Some("15".to_string()), Some("4".to_string())]
    );
}

/// r488 fixtures: canon's contains() header hop
/// and turbotax's max-by second-XML hop reduce to
/// data.
#[test]
fn r488_xml_header_and_doc_hops() {
    use crate::abrew::formula::{XmlPred, XmlShape};
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    let spec = parse(
        "cask \"canon-eos-utility\" do\n  version \"3.21.11.2,0200007754,4\"\n  sha256 \"099f546215d3797cad5928300d061770393a49336ba7278d4e4154e1edf54f12\"\n\n  url \"https://gdlp01.c-wss.com/gds/#{version.csv.third}/#{version.csv.second}/01/EU#{version.csv.first.major}Installer-M#{version.csv.first}.zip\"\n  name \"Canon EOS Utility\"\n  desc \"Communication with Canon EOS cameras\"\n  homepage \"https://app.ssw.imaging-saas.canon/app/en/eu.html\"\n\n  # The upstream data for the in-app updater uses redirecting URLs, so we have\n  # to find and fetch the URL for macOS to be able to match the cask `version`\n  # parts from the file URL in the `location` header of the response.\n  livecheck do\n    url \"https://gdlp01.c-wss.com/rmds/ic/autoupdate/common/tls_eu_updater_url.xml\"\n    regex(%r{/(\\d+)/(\\d+)/\\d+/EU\\d*[._-]?Install(?:er|set)[._-]v?M?(\\d+(?:\\.\\d+)+)\\.zip}i)\n    strategy :xml do |xml, regex|\n      # NOTE: The macOS identifier will need to be manually updated when\n      # releases become available for newer macOS versions.\n      url = xml.elements[\"//Component[contains(@ID,'mac_15')]\"]&.text&.strip\n      next if url.blank?\n\n      merged_headers = Homebrew::Livecheck::Strategy.page_headers(url).reduce(&:merge)\n\n      match = merged_headers[\"location\"]&.match(regex)\n      next if match.blank?\n\n      \"#{match[3]},#{match[2]},#{match[1]}\"\n    end\n  end\n\n  auto_updates true\n  depends_on :macos\n\n  pkg \"EU3Installer-M#{version.csv.first}/EU3Installer-M#{version.csv.first}.pkg\"\n\n  uninstall pkgutil: [\n    \"com.canon.EOS-Lens-Registration-Tool\",\n    \"com.canon.EOS-Network-Setting-Tool\",\n    \"com.canon.EOS-Utility\",\n    \"com.canon.EOS-Utility-3\",\n    \"com.canon.EOS-Web-Service-Registration-Tool\",\n    \"jp.co.canon.ic.CameraSurveyProgram\",\n  ]\n\n  zap trash: [\n    \"~/Library/Caches/com.canon.EOS-Lens-Registration-Tool\",\n    \"~/Library/Caches/com.canon.UpdateChecker\",\n    \"~/Library/Preferences/com.canon.EOS Utility 2.plist\",\n    \"~/Library/Preferences/com.canon.EOS-Utility-3.plist\",\n    \"~/Library/Preferences/com.canon.EOS-Utility.plist\",\n    \"~/Library/Preferences/jp.co.canon.ic.camerasurveyprogram.plist\",\n  ]\nend\n",
    );
    let XmlShape::HeaderHop { path, header, .. } = &spec.xml_block.as_ref().unwrap().shape else {
        panic!("canon shape");
    };
    assert_eq!(header, "location");
    assert_eq!(
        path.steps[0].pred,
        Some(XmlPred::AttrContains(
            "ID".to_string(),
            "mac_15".to_string()
        ))
    );
    let spec = parse(
        "cask \"turbotax-2024\" do\n  version \"2024.000.0193,D66513534AF33728FE66809C667F9565\"\n  sha256 \"e8d7ba016efe2e654fc4cdb27d0102b96e3c670ef3c339fbbb5ecb454c9defb3\"\n\n  url \"https://downloadpatch.esd.intuit.com/ESD/Files/CTG/comp/#{version.csv.second}/v1/Content/Mac_TurboTax_Download2024.dmg\"\n  name \"TurboTax 2024\"\n  desc \"Tax declaration for the fiscal year 2024\"\n  homepage \"https://turbotax.intuit.com/personal-taxes/cd-download/\"\n\n  livecheck do\n    url \"https://downloadpatch.esd.intuit.com/requestadapter/ctg/comp/2024/turbotaxpersonal/mcusi0000pp00/mac/deliverytarget_live.xml\"\n    regex(%r{/ESD/Files/CTG/comp/([^/]+)/}i)\n    strategy :xml do |xml, regex|\n      latest_package_info =\n        xml.get_elements(\"//DeliveryPackages/CoreDeliveryPackageInfo\")\n           .max_by { |item| item.elements[\"InternalName\"]&.text }\n      next unless latest_package_info\n\n      version = latest_package_info.elements[\"InternalName\"]&.text&.[](/v?(\\d+(?:\\.\\d+)+)/i, 1)\n      latest_xml_url = latest_package_info.elements[\"BaseUri\"]&.text&.strip\n      next if !version || !latest_xml_url\n\n      latest_xml_page = Homebrew::Livecheck::Strategy.page_content(latest_xml_url)\n      next if (latest_xml_content = latest_xml_page[:content]).blank?\n\n      latest_xml = Homebrew::Livecheck::Strategy::Xml.parse_xml(latest_xml_content)\n      latest_url = latest_xml.elements[\"//DeliveryPackageData/FileContent/ContentNetworkBaseUri\"]&.text&.strip\n      match = latest_url&.match(regex)\n      next if match.blank?\n\n      \"#{version},#{match[1]}\"\n    end\n  end\n\n  auto_updates true\n  depends_on macos: :ventura\n\n  app \"TurboTax 2024.app\"\n\n  uninstall quit: \"com.intuit.turbotax.2024\"\n\n  zap trash: [\n    \"~/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/com.intuit.turbotax.2024.sfl*\",\n    \"~/Library/Application Support/TurboTax 2024\",\n    \"~/Library/Caches/com.intuit.turbotax.2024\",\n    \"~/Library/Caches/KSCrash/TurboTax\",\n    \"~/Library/Caches/TurboTax 2024\",\n    \"~/Library/HTTPStorages/com.intuit.turbotax.2024\",\n    \"~/Library/HTTPStorages/com.intuit.turbotax.2024.binarycookies\",\n    \"~/Library/Logs/com.intuit.turbotax.2024*.log\",\n    \"~/Library/Preferences/com.intuit.turbotax.2024.plist\",\n    \"~/Library/WebKit/com.intuit.turbotax.2024\",\n  ]\n\n  caveats do\n    requires_rosetta\n  end\nend\n",
    );
    let XmlShape::HopMatch {
        max_child,
        version_child,
        url_child,
        ..
    } = &spec.xml_block.as_ref().unwrap().shape
    else {
        panic!("turbotax shape");
    };
    assert_eq!(max_child, "InternalName");
    assert_eq!(version_child, "InternalName");
    assert_eq!(url_child, "BaseUri");
}

/// r487 fixtures: plistedit-pro's find + literal
/// match, soundsource@test's blank-match own fork,
/// moom's dotless fork, and commander's patch-bucket
/// throttle reduce to data.
#[test]
fn r487_sparkle_litmatch_blankown_dotless_throttle() {
    use crate::abrew::formula::SpExpr;
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    let block = parse("cask \"plistedit-pro\" do\n  version \"1.10.0\"\n  sha256 \"0ad2f7a4066945b5a67db0667e946748cc4f33858f786554779c54f1a6608c4e\"\n\n  url \"https://www.fatcatsoftware.com/plisteditpro/downloads/PlistEditPro_#{version.no_dots}.zip\"\n  name \"PlistEdit Pro\"\n  desc \"Property list and JSON editor\"\n  homepage \"https://www.fatcatsoftware.com/plisteditpro/\"\n\n  # The Sparkle feed can contain items on the \"beta\" channel, so we restrict\n  # matching to the default channel.\n  livecheck do\n    url \"https://www.fatcatsoftware.com/plisteditpro/downloads/appcast.xml\"\n    strategy :sparkle do |items|\n      newest_item = items.find { |item| item.channel.nil? }&.short_version\n      match = newest_item&.match(/v?(\\d+\\.\\d+)((?:\\.\\d+)*)/i)\n      next if match.blank?\n\n      # The dotless version in the file name uses major/minor/patch but the\n      # appcast `shortVersionString` may omit patch for new major/minor\n      # versions (e.g. 1100 and 1.10 respectively). This adds `.0` to versions\n      # without a patch to avoid this mismatch.\n      \"#{match[1]}#{match[2].presence || \".0\"}\"\n    end\n  end\n\n  auto_updates true\n  depends_on macos: :sonoma\n\n  app \"PlistEdit Pro.app\"\n  binary \"#{appdir}/PlistEdit Pro.app/Contents/MacOS/pledit\"\n\n  zap trash: [\n    \"~/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/com.fatcatsoftware.pledpro.sfl*\",\n    \"~/Library/Application Support/com.fatcatsoftware.pledpro\",\n    \"~/Library/Application Support/PlistEdit Pro\",\n    \"~/Library/Caches/com.apple.helpd/Generated/com.fatcatsoftware.pledpro.help*\",\n    \"~/Library/Caches/com.fatcatsoftware.pledpro\",\n    \"~/Library/Preferences/com.fatcatsoftware.pledpro.plist\",\n  ]\nend\n").sparkle_block.unwrap();
    assert!(matches!(block.expr, SpExpr::LitMatch { .. }));
    let appcast = r#"<rss><channel><item><enclosure url="https://e/a.dmg" sparkle:shortVersionString="1.10" sparkle:version="1100"/></item></channel></rss>"#;
    assert_eq!(
        sparkle_block_versions(appcast, &block, None, "1.9"),
        vec!["1.10.0"]
    );
    let appcast = r#"<rss><channel><item><enclosure url="https://e/a.dmg" sparkle:shortVersionString="1.9.3" sparkle:version="193"/></item></channel></rss>"#;
    assert_eq!(
        sparkle_block_versions(appcast, &block, None, "1.9"),
        vec!["1.9.3"]
    );
    let block = parse("cask \"soundsource@test\" do\n  version \"6.2.0,0,20261112,0933,6208000\"\n  sha256 \"e0d63ca156d1606127b2cda84e6a47702830859712b48ccce5395172c00663bf\"\n\n  url \"https://download.rogueamoeba.com/builds/SoundSource/SoundSource_#{version.csv.fifth}_#{version.csv.third}_#{version.csv.fourth}.zip\"\n  name \"SoundSource\"\n  desc \"Sound and audio controller\"\n  homepage \"https://rogueamoeba.com/soundsource/\"\n\n  # The livecheck uses a hard-coded system version number in the url that corresponds to the latest macOS version\n  livecheck do\n    url \"https://rogueamoeba.net/ping/versionCheck.cgi?format=sparkle&bundleid=com.rogueamoeba.soundsource&system=270&platform=osx&arch=arm64&version=#{version.csv.fifth}&wantsTestReleases=true\"\n    regex(/SoundSource[._-]v?(\\h+)[._-](\\d+)[._-](\\d+)\\.zip/i)\n    strategy :sparkle do |item, regex|\n      match = item.url&.match(regex)\n      if match.blank?\n        # Release items link to an unversioned download instead of the build\n        next version if item.version == version.csv.first\n\n        next item.version\n      end\n\n      \"#{item.version.sub(\"fc\", \",\")},#{match[2]},#{match[3]},#{match[1]}\"\n    end\n  end\n\n  auto_updates true\n  conflicts_with cask: \"soundsource\"\n  depends_on macos: :sequoia\n\n  app \"SoundSource.app\"\n\n  uninstall quit: \"com.rogueamoeba.soundsource\"\n\n  zap trash: [\n    \"~/Library/Application Support/SoundSource\",\n    \"~/Library/Caches/com.rogueamoeba.soundsource\",\n    \"~/Library/HTTPStorages/com.rogueamoeba.soundsource\",\n    \"~/Library/Preferences/com.rogueamoeba.soundsource.plist\",\n    \"~/Library/WebKit/com.rogueamoeba.soundsource\",\n  ]\nend\n").sparkle_block.unwrap();
    assert!(matches!(block.expr, SpExpr::BlankOwnElse { .. }));
    let re = lc_regex(r"SoundSource[._-]v?(\h+)[._-](\d+)[._-](\d+)\.zip", "i").unwrap();
    let appcast = r#"<rss><channel><item><enclosure url="https://e/SoundSource.zip" sparkle:version="1.0"/></item></channel></rss>"#;
    assert_eq!(
        sparkle_block_versions(appcast, &block, Some(&re), "1.0,9"),
        vec!["1.0,9"]
    );
    let appcast = r#"<rss><channel><item><enclosure url="https://e/SoundSource.zip" sparkle:version="2.0"/></item></channel></rss>"#;
    assert_eq!(
        sparkle_block_versions(appcast, &block, Some(&re), "1.0,9"),
        vec!["2.0"]
    );
    let appcast = r#"<rss><channel><item><enclosure url="https://e/SoundSource-abc123-45-67.zip" sparkle:version="2.0fc3"/></item></channel></rss>"#;
    assert_eq!(
        sparkle_block_versions(appcast, &block, Some(&re), "1.0,9"),
        vec!["2.0,3,45,67,abc123"]
    );
    let block = parse("cask \"moom\" do\n  version \"4.6.0\"\n  sha256 \"45217254124fce56c48c84687c9abf78507e84e2df4a860ad3801433357b87e9\"\n\n  url \"https://manytricks.com/download/_do_not_hotlink_/moom#{version.no_dots}.dmg\"\n  name \"Moom\"\n  desc \"Utility to move and zoom windows—on one display\"\n  homepage \"https://manytricks.com/moom/\"\n\n  # Upstream has published releases where the version in the filename differs\n  # from the Sparkle shortVersion (e.g. moom400.dmg for 4.0), so we have to\n  # account for this mismatch.\n  livecheck do\n    url \"https://manytricks.com/moom/appcast/\"\n    regex(/moom[._-]?v?(\\d+(?:\\.\\d+)*)\\.dmg/i)\n    strategy :sparkle do |item, regex|\n      dotless_short_version = item.short_version.tr(\".\", \"\")\n      next if dotless_short_version.blank?\n\n      file_version = item.url[regex, 1]&.tr(\".\", \"\")\n      next if file_version.blank?\n\n      if dotless_short_version == file_version\n        item.short_version\n      elsif dotless_short_version.length < file_version.length\n        # Naively add extra digits from the dotless file version to the end of\n        # the shortVersion\n        \"#{item.short_version}.#{file_version.delete_prefix(dotless_short_version)}\"\n      else\n        # TODO: Account for a filename version with fewer digits than the\n        # shortVersion if/when necessary in the future\n        next\n      end\n    end\n  end\n\n  auto_updates true\n  depends_on :macos\n\n  app \"Moom.app\"\n\n  zap trash: [\n    \"~/Library/Application Support/Many Tricks\",\n    \"~/Library/Preferences/com.manytricks.Moom.plist\",\n  ]\nend\n").sparkle_block.unwrap();
    assert!(matches!(block.expr, SpExpr::DotlessFork { .. }));
    let re = lc_regex(r"moom[._-]?v?(\d+(?:\.\d+)*)\.dmg", "i").unwrap();
    let appcast = r#"<rss><channel><item><enclosure url="https://e/moom431.dmg" sparkle:shortVersionString="4.3"/></item></channel></rss>"#;
    assert_eq!(
        sparkle_block_versions(appcast, &block, Some(&re), "4.2"),
        vec!["4.3.1"]
    );
    let appcast = r#"<rss><channel><item><enclosure url="https://e/moom431.dmg" sparkle:shortVersionString="4.3.1"/></item></channel></rss>"#;
    assert_eq!(
        sparkle_block_versions(appcast, &block, Some(&re), "4.2"),
        vec!["4.3.1"]
    );
    let appcast = r#"<rss><channel><item><enclosure url="https://e/moom43.dmg" sparkle:shortVersionString="4.3.1"/></item></channel></rss>"#;
    assert_eq!(
        sparkle_block_versions(appcast, &block, Some(&re), "4.2"),
        Vec::<String>::new()
    );
    let block = parse("cask \"commander\" do\n  version \"0.7.1055\"\n  sha256 \"66ce8aa7fa4b6e82ee6413fe76a32ec121f90396433eb3f71ffa566360f133e7\"\n\n  url \"https://download.thecommander.app/release/Commander-#{version}.zip\"\n  name \"Commander\"\n  desc \"AI agent operator\"\n  homepage \"https://thecommander.app/\"\n\n  # Upstream typically creates several releases per day and there isn't always\n  # a release for every version increase. This manually throttles versions to\n  # one in every five versions (not releases), aiming for roughly one update per\n  # day. This may need to be adjusted if the release cadence changes.\n  livecheck do\n    url \"https://softwareupdate.thecommander.app/appcast/commanderai-appcast.xml\"\n    strategy :sparkle do |items|\n      versions = items.filter_map do |item|\n        short_version = item.short_version\n        next unless short_version\n\n        Version.new(short_version)\n      end.uniq.sort\n\n      throttled_patch = nil\n      versions.map do |short_version|\n        patch = short_version.patch.to_i\n        cur_throttled_patch = patch - (patch % 5)\n        next if cur_throttled_patch == throttled_patch\n\n        throttled_patch = cur_throttled_patch\n        short_version\n      end\n    end\n  end\n\n  auto_updates true\n  depends_on macos: :sequoia\n\n  app \"Commander.app\"\n\n  zap trash: [\n    \"~/Library/Application Support/Commander\",\n    \"~/Library/Caches/Commander\",\n    \"~/Library/Commander\",\n    \"~/Library/Preferences/com.krzyzanowskim.Commander.plist\",\n  ]\nend\n").sparkle_block.unwrap();
    assert!(matches!(block.expr, SpExpr::ThrottleUniq { .. }));
    let appcast = r#"<rss><channel><item><enclosure url="https://e/a.dmg" sparkle:shortVersionString="1.0.11"/></item><item><enclosure url="https://e/a.dmg" sparkle:shortVersionString="1.0.3"/></item><item><enclosure url="https://e/a.dmg" sparkle:shortVersionString="1.0.6"/></item><item><enclosure url="https://e/a.dmg" sparkle:shortVersionString="1.0.6"/></item><item><enclosure url="https://e/a.dmg" sparkle:shortVersionString="1.0.1"/></item></channel></rss>"#;
    assert_eq!(
        sparkle_block_versions(appcast, &block, None, "1.0.0"),
        vec!["1.0.1", "1.0.6", "1.0.11"]
    );
}

/// r486 fixtures: adobe's JSON-in-XML text,
/// silentknight's own-major XPath predicate, and
/// endnote's own-version map fork reduce to data.
#[test]
fn r486_xml_embed_predinterp_ownfork() {
    use crate::abrew::formula::{XmlPred, XmlShape};
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    let spec = parse(
        "cask \"adobe-creative-cloud\" do\n  arch arm: \"macarm64\", intel: \"osx10\"\n\n  version \"6.11.0.478.6\"\n  sha256 arm:   \"724c1e76ff4886cb135988d1bf46d28bf33e784840e9b160b3c6932cac62c541\",\n         intel: \"addca3ba7819bb0fdbe1394b8d0ff6a2614e599ded91a3ba5839672c996ec750\"\n\n  # If url breaks you can find the latest static urls - https://helpx.adobe.com/download-install/kb/creative-cloud-desktop-app-download.html\n  url \"https://ccmdls.adobe.com/AdobeProducts/StandaloneBuilds/ACCC/ESD/#{version.major_minor_patch}/#{version.split(\".\")[3..4].join(\".\")}/#{arch}/ACCCx#{version.dots_to_underscores}.dmg\"\n  name \"Adobe Creative Cloud\"\n  desc \"Collection of apps and services for photography, design, video, web, and UX\"\n  homepage \"https://www.adobe.com/creativecloud.html\"\n\n  livecheck do\n    url \"https://ffc-static-cdn.oobesaas.adobe.com/features/v3/#{arch}/ccdConfig.xml\"\n    strategy :xml do |xml|\n      item = xml.elements[\"//feature-entry[@id='ccd.fw.update.greenline.latest']/data\"]&.text&.strip\n      next if item.blank?\n\n      json = Homebrew::Livecheck::Strategy::Json.parse_json(item)\n      json[\"version\"]\n    end\n  end\n\n  auto_updates true\n  depends_on :macos\n\n  installer script: {\n    executable:   \"#{staged_path}/Install.app/Contents/MacOS/Install\",\n    args:         [\"--mode=silent\"],\n    sudo:         true,\n    print_stderr: false,\n  }\n\n  uninstall early_script: {\n              executable:   \"/usr/bin/pluginkit\",\n              args:         [\n                \"-r\", \"/Applications/Utilities/Adobe Sync/CoreSync/Core Sync.app/Contents/PlugIns/ACCFinderSync.appex\"\n              ],\n              must_succeed: false,\n              print_stderr: false,\n            },\n            launchctl:    [\n              \"Adobe_Genuine_Software_Integrity_Service\",\n              \"com.adobe.acc.installer\",\n              \"com.adobe.acc.installer.v2\",\n              \"com.adobe.AdobeCreativeCloud\",\n              \"com.adobe.AdobeDesktopService\",\n              \"com.adobe.ccxprocess\",\n              \"com.adobe.CCXProcess.*\",\n            ],\n            quit:         \"com.adobe.acc.AdobeCreativeCloud\",\n            signal:       [\"QUIT\", \"com.adobe.accmac\"],\n            script:       {\n              executable:   \"/usr/bin/pkill\",\n              args:         [\"Adobe Desktop Service\", \"AdobeIPCBroker\", \"AdobeCRDaemon\"],\n              must_succeed: false,\n            },\n            delete:       [\n              \"#{appdir}/Adobe Creative Cloud/*Adobe Creative Cloud\",\n              \"#{appdir}/Adobe Creative Cloud/.Uninstall*\",\n              \"#{appdir}/Adobe Creative Cloud/Icon?\",\n              \"/Applications/Utilities/Adobe Application Manager\",\n              \"/Applications/Utilities/Adobe Creative Cloud*\",\n              \"/Applications/Utilities/Adobe Installers/.Uninstall*\",\n              \"/Applications/Utilities/Adobe Installers/Uninstall Adobe Creative Cloud\",\n              \"/Applications/Utilities/Adobe Sync\",\n              \"/Library/Internet Plug-Ins/AdobeAAMDetect.plugin\",\n              \"/Library/LaunchDaemons/com.adobe.agsservice.plist\",\n            ],\n            rmdir:        [\n              \"#{appdir}/Adobe Creative Cloud\",\n              \"/Applications/Utilities/Adobe Installers\",\n              \"/Library/*/Adobe\",\n              \"/Library/Application Support/Adobe\",\n              \"/Library/Application Support/Adobe{/CEP{/extensions,},}\",\n              \"/Library/Logs/Adobe\",\n            ]\n\n  zap trash: [\n        \"/Library/*/com.adobe.acc*\",\n        \"/Library/Application Support/Adobe/ADCRefs\",\n        \"/Library/Application Support/Adobe/Adobe Desktop Common\",\n        \"/Library/Application Support/Adobe/*[Ii]nstall*\",\n        \"/Library/Application Support/Adobe/Adobe PCD\",\n        \"/Library/Application Support/Adobe/AdobeApplicationManager\",\n        \"/Library/Application Support/Adobe/AdobeGC*\",\n        \"/Library/Application Support/Adobe/caps\",\n        \"/Library/Application Support/Adobe/CEP/extensions/CC_*\",\n        \"/Library/Application Support/Adobe/CEP/extensions/com.adobe.ccx.*\",\n        \"/Library/Application Support/Adobe/Creative Cloud Libraries\",\n        \"/Library/Application Support/Adobe/Extension Manager CC\",\n        \"/Library/Application Support/Adobe/OOBE\",\n        \"/Library/Application Support/Adobe/PCF\",\n        \"/Library/Application Support/Adobe/SL*\",\n        \"/Library/Application Support/Adobe/Vulcan\",\n        \"/Library/Application Support/regid.*.com.adobe\",\n        \"/Library/Logs/CreativeCloud\",\n        \"/Library/Preferences/com.adobe.headlights*.plist\",\n        \"/Users//Shared/Adobe/Installer\",\n        \"/Users//Shared/Adobe/OOBE\",\n        \"~/Creative Cloud Files\",\n        \"~/Creative Cloud Files/Icon?\",\n        \"~/Library/*/Adobe/CoreSync\",\n        \"~/Library/*/com.adobe.acc*\",\n        \"~/Library/Application Scripts/com.adobe.accmac.ACCFinderSync\",\n        \"~/Library/Application Support/Adobe/.adobelicnotification\",\n        \"~/Library/Application Support/Adobe/AAMUpdater\",\n        \"~/Library/Application Support/Adobe/ExtensibilityLibrary\",\n        \"~/Library/Application Support/Adobe/Extension Manager CC\",\n        \"~/Library/Application Support/Adobe/FloodGate\",\n        \"~/Library/Application Support/Adobe/OOBE\",\n        \"~/Library/Application Support/CrashReporter/AdobeCRDaemon_*.plist\",\n        \"~/Library/Group Containers/Adobe-Hub-App\",\n        \"~/Library/LaunchAgents/com.adobe.ccxprocess.plist\",\n        \"~/Library/Logs/ACC*.log\",\n        \"~/Library/Logs/AdobeDownload.log\",\n        \"~/Library/Logs/AdobeIPCBroker*.log\",\n        \"~/Library/Logs/CoreSyncInstall.log\",\n        \"~/Library/Logs/CreativeCloud\",\n        \"~/Library/Logs/PDApp*.log\",\n        \"~/Library/Preferences/Adobe/.[A-Z0-9]???????????\",\n        \"~/Library/Preferences/com.adobe.crashreporter.plist\",\n      ],\n      rmdir: [\n        \"/Users//Shared/Adobe\",\n        \"~/Creative Cloud Files\",\n        \"~/Library/*/Adobe\",\n        \"~/Library/Application Support/Adobe\",\n        \"~/Library/Logs/Adobe\",\n      ]\nend\n",
    );
    let XmlShape::TextJsonKey { path, key } = &spec.xml_block.as_ref().unwrap().shape else {
        panic!("adobe shape");
    };
    assert_eq!(key, "version");
    assert_eq!(
        xml_text_json_versions(
            path,
            key,
            "<features><feature-entry id=\"ccd.fw.update.greenline.latest\"><data>{\"version\":\"6.9.0\"}</data></feature-entry></features>",
        )
        .unwrap(),
        vec!["6.9.0"]
    );
    let mut block = parse("cask \"silentknight\" do\n  version \"3.02,2026.07\"\n  sha256 \"41be69cccdcedb325edbe825b86bdea24b1bcfbbcde8bea9e57e7a82326ea157\"\n\n  # Upstream zero-pads the minor version in the no-dot filename version to two\n  # digits (e.g. 2.9 is 209). We only need this workaround while the minor\n  # version is less than two digits, so we should be able to switch back to\n  # `version.csv.first.no_dots` in the filename with version 2.10+.`\n  no_dot_version = version.csv.first.split(\".\").each_with_index.map do |n, i|\n    (i < 1 || n.length > 1) ? n : n.rjust(2, \"0\")\n  end.join\n\n  url \"https://eclecticlight.co/wp-content/uploads/#{version.csv.second.major}/#{version.csv.second.minor}/sk#{no_dot_version}.zip\"\n  name \"SilentKnight\"\n  desc \"Automatically checks computer's security\"\n  homepage \"https://eclecticlight.co/lockrattler-systhist/\"\n\n  livecheck do\n    url \"https://raw.githubusercontent.com/hoakleyelc/updates/master/eclecticapps.plist\"\n    regex(%r{/(\\d+)/(\\d+)/[^/]+?$}i)\n    strategy :xml do |xml, regex|\n      item = xml.elements[\"//dict[key[text()='AppName']/following-sibling::*[1][text()='SilentKnight#{version.major}']]\"]\n      next unless item\n\n      version = item.elements[\"key[text()='Version']\"]&.next_element&.text\n      url = item.elements[\"key[text()='URL']\"]&.next_element&.text\n      match = url.strip.match(regex) if url\n      next if version.blank? || match.blank?\n\n      \"#{version.strip},#{match[1]}.#{match[2]}\"\n    end\n  end\n\n  depends_on arch: :arm64\n  depends_on macos: :sequoia\n\n  app \"sk#{no_dot_version}/SilentKnight#{version.major}.app\"\n\n  zap trash: [\n    \"~/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/co.eclecticlight.silentknight.sfl*\",\n    \"~/Library/Caches/co.eclecticlight.SilentKnight\",\n    \"~/Library/Caches/co.eclecticlight.SilentKnight#{version.major}\",\n    \"~/Library/HTTPStorages/co.eclecticlight.SilentKnight\",\n    \"~/Library/HTTPStorages/co.eclecticlight.SilentKnight#{version.major}\",\n    \"~/Library/Preferences/co.eclecticlight.SilentKnight#{version.major}.plist\",\n    \"~/Library/Preferences/co.eclecticlight.SilentKnight.plist\",\n    \"~/Library/Saved Application State/co.eclecticlight.SilentKnight.savedState\",\n  ]\nend\n").xml_block.unwrap();
    let XmlShape::Template(tmpl) = &block.shape else {
        panic!("silentknight shape");
    };
    let pred_text = |t: &crate::abrew::formula::XmlTemplate| match &t.elems[0].path.steps[0].pred {
        Some(XmlPred::SiblingAfterChild { text, .. }) => text.clone(),
        other => panic!("pred {other:?}"),
    };
    assert_eq!(pred_text(tmpl), "SilentKnight#{version.major}");
    xml_block_substitute_versions(&mut block, "3.02,2026.07", true).unwrap();
    let XmlShape::Template(tmpl) = &block.shape else {
        unreachable!()
    };
    assert_eq!(pred_text(tmpl), "SilentKnight3");
    let spec = parse(
        "cask \"endnote\" do\n  # The second part of the `version` is the product version (e.g. X9, 2025).\n  # Until we can identify this using livecheck, we will need to manually find\n  # the new product version on each major version update. See:\n  # https://support.clarivate.com/Endnote/s/article/Download-EndNote\n  version \"22.3.0.22265,2025\"\n  sha256 :no_check # required as upstream package is updated in-place\n\n  url \"https://download.endnote.com/downloads/#{version.csv.second}/EndNote#{version.csv.second}Installer.dmg\"\n  name \"EndNote\"\n  desc \"Reference manager\"\n  homepage \"https://endnote.com/\"\n\n  # The XML file doesn't provide any update information for new major versions\n  # (e.g. 22.0.0.0) until a subsequent version is released. The check will fail\n  # when the cask is updated to a new major version but it should automatically\n  # work again when the next version is released (i.e. livecheck will need to\n  # be skipped on CI for major version updates).\n  livecheck do\n    url \"https://download.endnote.com/updates/#{version.csv.first.major}.0/EN#{version.csv.first.major}MacUpdates.xml\"\n    regex(/^v?(\\d+(?:\\.\\d+)+)$/i)\n    strategy :xml do |xml, regex|\n      current_major = version.csv.first.major\n      current_product_version = version.csv.second\n\n      xml.get_elements(\"//updates/build\").map do |item|\n        update_to = item.elements[\"updateTo\"]&.text&.strip\n        match = update_to.match(regex) if update_to\n        next if match.blank?\n\n        major = match[1][/^(\\d+)/, 1]\n        if major == current_major && current_product_version\n          \"#{match[1]},#{current_product_version}\"\n        else\n          # This does not add the current product version to any versions in\n          # the upstream XML file that don't use the current major. This is\n          # intended to ensure that a major version update won't work until\n          # someone manually identifies the new product version (e.g. X9, 2025)\n          # and adds it as the second part of the cask `version` (e.g.\n          # `22.0.0.0,2025`). This `strategy` block setup can be reworked\n          # if/when upstream provides a way of identifying product versions\n          # that can be reliably checked by livecheck.\n          match[1]\n        end\n      end\n    end\n  end\n\n  depends_on :macos\n  container nested: \"Install EndNote #{version.csv.second}.app/Contents/Resources/EndNote.zip\"\n\n  suite \"EndNote\"\n\n  zap trash: [\n    \"/Library/Application Support/ResearchSoft/EndNote\",\n    \"~/Library/Application Support/EndNote\",\n    \"~/Library/Caches/com.ThomsonResearchSoft.EndNote\",\n    \"~/Library/Cookies/com.ThomsonResearchSoft.EndNote.binarycookies\",\n    \"~/Library/HTTPStorages/com.ThomsonResearchSoft.EndNote\",\n    \"~/Library/HTTPStorages/com.ThomsonResearchSoft.EndNote.binarycookies\",\n    \"~/Library/Preferences/com.ThomsonResearchSoft.EndNote.plist\",\n    \"~/Library/Preferences/com.ThomsonReuters.EndNoteCustomizer.plist\",\n    \"~/Library/Services/ENService.app\",\n    \"~/Library/Spotlight/EndNote.mdimporter\",\n    \"~/Library/WebKit/com.ThomsonResearchSoft.EndNote\",\n  ]\nend\n",
    );
    let XmlShape::ChildMatchOwnFork { path, child } = &spec.xml_block.as_ref().unwrap().shape
    else {
        panic!("endnote shape");
    };
    assert_eq!(child, "updateTo");
    let rp = RunPlan {
        strat: Strat::Xml,
        url: String::new(),
        re: Some(lc_regex(r"^v?(\d+(?:\.\d+)+)$", "i").unwrap()),
        candidates: Vec::new(),
        strategy_url: None,
        ua: None,
        ua_option: None,
        ua_symbol: false,
        gnome_filter: false,
        json: None,
        xml: None,
        yaml: None,
        page_block: None,
        ghrel_block: None,
        git: None,
        sparkle_block: None,
        header_block: None,
        latest: None,
        post_form: None,
        post_json: None,
        headers: Vec::new(),
        major_minor: String::new(),
        own_version: "22.0.0.0,2025".to_string(),
        plist: None,
        apps: Vec::new(),
        uncompressed: false,
    };
    assert_eq!(
        xml_own_fork_versions(
            path,
            child,
            "<updates><build><updateTo>22.1.0</updateTo></build><build><updateTo>23.0.0</updateTo></build></updates>",
            &rp,
        )
        .unwrap(),
        vec!["22.1.0,2025", "23.0.0"]
    );
    // The agreement gate's file URLs: adobe's
    // split-range join and silentknight's indexed
    // zero-pad local interpolate natively.
    assert_eq!(
        crate::abrew::formula::cask_version_token(
            "6.11.0.478.6",
            "version.split(\".\")[3..4].join(\".\")",
        )
        .as_deref(),
        Some("478.6")
    );
    let c = crate::abrew::tap::parse_cask_rb_as_data(
        "cask \"x\" do\n  version \"3.2,2026.07\"\n  no_dot_version = version.csv.first.split(\".\").each_with_index.map do |n, i|\n    (i < 1 || n.length > 1) ? n : n.rjust(2, \"0\")\n  end.join\n  url \"https://e/sk#{no_dot_version}.zip\"\nend\n",
        "x",
    );
    assert_eq!(c.url, "https://e/sk302.zip");
}

/// r485 fixtures: konica's any-match OS guard and
/// split-rows dmg MD5 reduce to data.
#[test]
fn r485_json_konica_split_rows_md5() {
    use crate::abrew::formula::JsonShape;
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    assert!(matches!(parse("  livecheck do\n    url \"https://dl.konicaminolta.eu/en?tx_kmdownloadcenter_dlajaxservice[action]=getDocuments&tx_kmdownloadcenter_dlajaxservice[controller]=AjaxService&tx_kmdownloadcenter_dlajaxservice[productId]=102314&tx_kmdownloadcenter_dlajaxservice[system]=KonicaMinolta&cHash=dd72618a38434b6cb3edfc20595d58c5&type=1527583889\"\n    strategy :json do |json|\n      json.map do |item|\n        next if item[\"TypeOfApplicationId_textS\"] != \"1\"\n        next unless item[\"OperatingSystemsNames_textM\"]&.any? { |os| os =~ /macOS/i }\n\n        version = item[\"Version_textS\"]\n        document_id = item[\"AnacondaId_textS\"]\n        next if version.blank? || document_id.blank?\n\n        files = item[\"DownloadFiles_textS\"]&.split(\"\\n\")&.map { |file| file.split(\"|\") }\n        dmg_file = files.find { |file| file.first.end_with?(\".dmg\") } if files\n        next if dmg_file.blank?\n\n        \"#{version},#{Digest::MD5.hexdigest(dmg_file[2])},#{document_id}\"\n      end\n    end\n  end\n").json_block.unwrap().shape, JsonShape::Template(..)));
    assert!(matches!(parse("  livecheck do\n    url \"https://dl.konicaminolta.eu/en?tx_kmdownloadcenter_dlajaxservice[action]=getDocuments&tx_kmdownloadcenter_dlajaxservice[controller]=AjaxService&tx_kmdownloadcenter_dlajaxservice[productId]=103745&tx_kmdownloadcenter_dlajaxservice[system]=KonicaMinolta&cHash=dd72618a38434b6cb3edfc20595d58c5&type=1527583889\"\n    strategy :json do |json|\n      json.map do |item|\n        next if item[\"TypeOfApplicationId_textS\"] != \"1\"\n        next unless item[\"OperatingSystemsNames_textM\"]&.any? { |os| os =~ /macOS/i }\n\n        version = item[\"Version_textS\"]\n        document_id = item[\"AnacondaId_textS\"]\n        next if version.blank? || document_id.blank?\n\n        files = item[\"DownloadFiles_textS\"]&.split(\"\\n\")&.map { |file| file.split(\"|\") }\n        dmg_file = files.find { |file| file.first.end_with?(\".dmg\") } if files\n        next if dmg_file.blank?\n\n        \"#{version},#{Digest::MD5.hexdigest(dmg_file[2])},#{document_id}\"\n      end\n    end\n  end\n").json_block.unwrap().shape, JsonShape::Template(..)));
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(
        md5_hex(b"The quick brown fox jumps over the lazy dog"),
        "9e107d9d372bb6826bd81d3542a419d6"
    );
}

/// r484 fixtures: gimp's nested release/build map
/// with an outer-version fallback reduces to data.
#[test]
fn r484_json_nested_template_gimp() {
    use crate::abrew::formula::JsonShape;
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    assert!(matches!(parse("cask \"gimp\" do\n  arch arm: on_system_conditional(macos: \"arm64\", linux: \"aarch64\"), intel: \"x86_64\"\n  os macos: \"macos\", linux: \"linux\"\n  livecheck_os = on_system_conditional macos: \"macos\", linux: \"appimage\"\n  name_start = on_system_conditional macos: \"gimp\", linux: \"GIMP\"\n  url_end = on_system_conditional macos: \"dmg\", linux: \"AppImage\"\n\n  version \"3.2.8\"\n  sha256 arm:          \"06404a375106b7613ba3a14c7d8d9a7cbe18b9b9e2b2050bb72f3162cbcfc395\",\n         intel:        \"7746487985d7143b190692500b58d5317b6ce916a60f8139a76df5eea1e920db\",\n         arm64_linux:  \"3fd61dbc40b760f7806ffc04c29dc0d93021b799b9d0e6977571c4425d0323b0\",\n         x86_64_linux: \"8b43dc23e58a1b66ace9dd6d4d5f7e8532f3398e6109f95a1ec0b6ad036aa250\"\n\n  on_macos do\n    app \"GIMP.app\"\n    command_wrapper \"gimp\", executable: \"#{appdir}/GIMP.app/Contents/MacOS/gimp\"\n\n    zap trash: [\n      \"~/Library/Application Support/Gimp\",\n      \"~/Library/Caches/GIMP\",\n      \"~/Library/Preferences/org.gimp.gimp-#{version.major_minor}.plist\",\n      \"~/Library/Preferences/org.gimp.gimp.plist\",\n      \"~/Library/Saved Application State/org.gimp.gimp-#{version.major_minor}.savedState\",\n    ]\n  end\n  on_linux do\n    app_image \"GIMP-#{version.csv.first}-#{arch}#{\"-#{version.csv.second}\" if version.csv.second}.AppImage\",\n              target: \"GIMP.AppImage\"\n\n    zap trash: \"~/.config/GIMP/#{version.major_minor}\"\n  end\n\n  url \"https://download.gimp.org/gimp/v#{version.major_minor}/#{os}/#{name_start}-#{version.csv.first}-#{arch}#{\"-#{version.csv.second}\" if version.csv.second}.#{url_end}\"\n  name \"GIMP\"\n  name \"GNU Image Manipulation Program\"\n  desc \"Free and open-source image editor\"\n  homepage \"https://www.gimp.org/\"\n\n  livecheck do\n    url \"https://www.gimp.org/gimp_versions.json\"\n    strategy :json do |json|\n      json[\"STABLE\"]&.map do |release|\n        release[livecheck_os]&.map do |build|\n          next unless build[\"filename\"]&.match?(/#{arch}/i)\n          next release[\"version\"] unless build[\"revision\"]\n\n          \"#{release[\"version\"]},#{build[\"revision\"]}\"\n        end\n      end&.flatten\n    end\n  end\n\n  conflicts_with cask: \"gimp@dev\"\nend\n").json_block.unwrap().shape, JsonShape::NestedTemplate { .. }));
    assert!(matches!(parse("cask \"gimp@dev\" do\n  arch arm: on_system_conditional(macos: \"arm64\", linux: \"aarch64\"), intel: \"x86_64\"\n  os macos: \"macos\", linux: \"linux\"\n  livecheck_os = on_system_conditional macos: \"macos\", linux: \"appimage\"\n  name_start = on_system_conditional macos: \"gimp\", linux: \"GIMP\"\n  url_end = on_system_conditional macos: \"dmg\", linux: \"AppImage\"\n\n  version \"3.2.0-RC3\"\n  sha256 arm:          \"93baffa8e41517b2cb6902176a851a7a473f2957aa3eadfcc38b9dd361fa6601\",\n         intel:        \"3b69163e475f8aee71565f2df8dbc35fde730f18ff6b9fde98b726ff41f2ab32\",\n         arm64_linux:  \"59a97a7a46df7f77a076d4f2b431949f2667be79bb6094babfb7f8bcd86ff7fc\",\n         x86_64_linux: \"3712a67061474c363e0c2ed8f5d26398e2d708cdf6b0778368a514c9d134d88d\"\n\n  on_macos do\n    app \"GIMP.app\"\n    command_wrapper \"gimp\", executable: \"#{appdir}/GIMP.app/Contents/MacOS/gimp\"\n\n    zap trash: [\n      \"~/Library/Application Support/Gimp\",\n      \"~/Library/Preferences/org.gimp.gimp-#{version.major_minor}.plist\",\n      \"~/Library/Saved Application State/org.gimp.gimp-#{version.major_minor}.savedState\",\n    ]\n  end\n  on_linux do\n    app_image \"GIMP-#{version.csv.first}-#{arch}#{\"-#{version.csv.second}\" if version.csv.second}.AppImage\",\n              target: \"GIMP.AppImage\"\n\n    zap trash: \"~/.config/GIMP/#{version.major_minor}\"\n  end\n\n  url \"https://download.gimp.org/gimp/v#{version.major_minor}/#{os}/#{name_start}-#{version.csv.first}-#{arch}#{\"-#{version.csv.second}\" if version.csv.second}.#{url_end}\"\n  name \"GIMP development version\"\n  desc \"Free and open-source image editor\"\n  homepage \"https://www.gimp.org/\"\n\n  livecheck do\n    url \"https://www.gimp.org/gimp_versions.json\"\n    strategy :json do |json|\n      json[\"DEVELOPMENT\"]&.map do |release|\n        release[livecheck_os]&.map do |build|\n          next unless build[\"filename\"]&.match?(/#{arch}/i)\n          next release[\"version\"] unless build[\"revision\"]\n\n          \"#{release[\"version\"]},#{build[\"revision\"]}\"\n        end\n      end&.flatten\n    end\n  end\n\n  conflicts_with cask: \"gimp\"\nend\n").json_block.unwrap().shape, JsonShape::NestedTemplate { .. }));
}

#[test]
fn json_nested_template_keeps_truthiness_distinct_from_blankness() {
    use crate::abrew::formula::{JsonShape, LivecheckUrl};

    let source = |predicate: &str| {
        format!(
            r##"  livecheck do
    url "https://example.invalid/not-fetched.json"
    strategy :json do |json|
      json["releases"].map do |release|
        release["builds"].map do |build|
          next release["version"] {predicate}
          "#{{release["version"]}},#{{build["revision"]}}"
        end
      end.flatten
    end
  end
"##
        )
    };
    let permitted = source("unless build[\"revision\"]");
    let spec = crate::abrew::tap::parse_livecheck_block(&permitted).unwrap();
    assert!(spec.strategy_block);
    assert!(matches!(
        spec.json_block.as_ref().unwrap().shape,
        JsonShape::NestedTemplate { .. }
    ));
    let Some(LivecheckUrl::Literal(url)) = &spec.url else {
        panic!("fixture URL was not parsed as a literal");
    };
    let UrlPlan::Run(mut plan) = plan_url(Some(&spec), url, "", "").unwrap() else {
        panic!("supported nested template did not produce an execution plan");
    };

    // Exercise page_fetch and the actual evaluator using private local bytes; this does not
    // establish HTTP URL admission. Ruby treats empty strings, whitespace and zero as truthy.
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("releases.json");
    std::fs::write(
        &path,
        br#"{"releases":[
          {"version":"1","builds":[{"revision":""}]},
          {"version":"2","builds":[{"revision":null}]},
          {"version":"3","builds":[{"revision":false}]},
          {"version":"4","builds":[{}]},
          {"version":"5","builds":[{"revision":0}]},
          {"version":"6","builds":[{"revision":" "}]},
          {"version":"7","builds":[{"revision":2}]}
        ]}"#,
    )
    .unwrap();
    plan.url = format!("file://{}", path.display());
    plan.ua = Some(String::new());
    plan.uncompressed = true;
    match json_versions(&plan) {
        Found::Versions(versions, _) => {
            assert_eq!(versions, ["1,", "2", "3", "4", "5,0", "6, ", "7,2"]);
        }
        Found::Messages(messages) => panic!("local JSON evaluation refused: {messages:?}"),
        Found::Raised(message) => panic!("local JSON evaluation raised: {message}"),
    }

    // Both blankness spellings differ on the empty/whitespace values above. Without a
    // predicate in the native shape, the complete block must remain unsupported.
    for predicate in [
        "if build[\"revision\"].blank?",
        "if build[\"revision\"]&.blank?",
    ] {
        let text = source(predicate);
        let spec = crate::abrew::tap::parse_livecheck_block(&text).unwrap();
        assert!(
            spec.strategy_block && spec.json_block.is_none(),
            "{predicate}"
        );
        let Some(LivecheckUrl::Literal(url)) = &spec.url else {
            panic!("fixture URL was not parsed as a literal");
        };
        assert!(
            plan_url(Some(&spec), url, "", "").is_err(),
            "unsupported predicate must not fall back to an execution plan: {predicate}"
        );
        assert!(spec_outside(&Some(spec)), "{predicate}");
    }
}

/// r483 fixtures: insta360's find/select/max_by
/// pipeline with a capture tr and comma-collapsing
/// gsub reduces to data.
#[test]
fn r483_json_find_select_max_by() {
    use crate::abrew::formula::JsonShape;
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    assert!(matches!(parse("  livecheck do\n    url \"https://openapi.insta360.com/app/appDownload/getGroupApp?group=insta360-go2&X-Language=en-us\"\n    regex(%r{/(\\h+)/Insta360(?:[._-]|%20)?Studio(?:[._-]|%20)v?(\\d+(?:\\.\\d+)+)[._-](.+)\\.(?:pkg|zip)}i)\n    strategy :json do |json, regex|\n      # Find the Insta360 Studio app\n      app = json.dig(\"data\", \"apps\")&.find { |item| item[\"app_id\"] == 38 }\n      next if app.blank?\n\n      # Find the newest macOS version\n      newest_release = app[\"items\"]&.select { |item| item[\"platform\"] == \"mac\" }\n                                   &.max_by { |item| Version.new(item[\"version\"]) }\n      next if newest_release.blank?\n\n      # Find the channel item (there's likely only one object in the array)\n      channel = newest_release[\"channels\"]&.find { |item| item[\"channel\"] == \"official\" }\n      next if channel.blank?\n\n      match = channel[\"download_url\"]&.match(regex)\n      next if match.blank?\n\n      \"#{match[2]},#{match[3].tr(\"()\", \",\")},#{match[1]}\"\n    end\n  end\n").json_block.unwrap().shape, JsonShape::Template(..)));
    assert!(matches!(parse("  livecheck do\n    url \"https://openapi.insta360.com/app/appDownload/getGroupApp?group=insta360-link&X-Language=en-us\"\n    regex(%r{/(\\h+)/Insta360(?:[._-]|%20)?LinkController(?:[._-]|%20)v?(\\d+(?:\\.\\d+)+)[._-]?(.+?)\\.(?:pkg|zip)}i)\n    strategy :json do |json, regex|\n      # Find the Insta360 Link Controller app\n      app = json.dig(\"data\", \"apps\")&.find { |item| item[\"app_id\"] == 100 }\n      next if app.blank?\n\n      # Find the newest macOS version\n      newest_release = app[\"items\"]&.select { |item| item[\"platform\"] == \"mac\" }\n                                   &.max_by { |item| Version.new(item[\"version\"]) }\n      next if newest_release.blank?\n\n      # Find the channel item (there's likely only one object in the array)\n      channel = newest_release[\"channels\"]&.find { |item| item[\"channel\"] == \"official\" }\n      next if channel.blank?\n\n      match = channel[\"download_url\"]&.match(regex)\n      next unless match\n\n      \"#{match[2]},#{match[3].tr(\"()\", \",\")},#{match[1]}\".gsub(/,{2,}/, \",\")\n    end\n  end\n").json_block.unwrap().shape, JsonShape::Template(..)));
}

/// r482 fixtures: dosbox's split-length scan fork,
/// asix-ax88179's split-section scan, and
/// inkscape's scanned-section match reduce to data.
#[test]
fn r482_page_section_scans() {
    use crate::abrew::formula::PageShape;
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    assert!(matches!(parse("  livecheck do\n    url \"https://sourceforge.net/projects/dosbox/rss?path=/dosbox\"\n    regex(%r{<link>.*/DOSBox-(\\d+(?:[.-]\\d+)*)\\.dmg}i)\n    strategy :page_match do |page, regex|\n      page.scan(regex).map do |matches|\n        versions = matches[0].split(\"-\")\n        version = \"#{versions[0]}-#{versions[1]},#{versions[2]}\" if versions.length == 3\n        version = \"#{versions[0]}-#{versions[1]}\" if versions.length == 2\n        version\n      end\n    end\n  end\n").page_block.unwrap().shape, PageShape::ScanSplitFork { .. }));
    assert!(matches!(parse("  livecheck do\n    url \"https://www.asix.com.tw/en/support/download/step2/11/2/3\"\n    regex(%r{data-href=.*?/download/file/(\\d+).*?macOS\\s*12.*?(?:<br>)?\\s*?(\\d+(?:\\.\\d+)+)<}mi)\n    strategy :page_match do |page, regex|\n      page.split(/class=['\"]?list__item['\"]?/).map do |list_item|\n        list_item.scan(regex).map { |match| \"#{match[1]},#{match[0]}\" }\n      end.flatten\n    end\n  end\n").page_block.unwrap().shape, PageShape::SplitScan { .. }));
    assert!(matches!(parse("cask \"inkscape\" do\n  arch arm: \"_arm64\", intel: on_system_conditional(macos: \"_x86_64\")\n  livecheck_folder = on_system_conditional macos: \"mac-os-x\", linux: \"gnulinux/appimage\"\n  url_end = on_system_conditional macos: \"dmg\", linux: \"AppImage\"\n\n  version \"1.4.4\"\n  sha256 arm:          \"eacca94ab01e59467cdd5452a2867c3734450f9a4cfb3346956cc9b07f34f3f1\",\n         intel:        \"17ff9f49b60115237cad9b60605950e26b11b04600e726a4513224aa373aee11\",\n         x86_64_linux: \"d217fe05c92faaed5c3af46856b6299c98933dc722819cb5bc2ca052c5522166\"\n\n  on_macos do\n    app \"Inkscape.app\"\n    command_wrapper \"inkscape\", executable: \"#{appdir}/Inkscape.app/Contents/MacOS/inkscape\"\n\n    zap trash: [\n      \"~/.config/inkscape\",\n      \"~/Library/Application Support/Inkscape\",\n      \"~/Library/Application Support/org.inkscape.Inkscape\",\n      \"~/Library/Caches/org.inkscape.Inkscape*\",\n      \"~/Library/Preferences/org.inkscape.Inkscape.plist\",\n      \"~/Library/Saved Application State/org.inkscape.Inkscape.savedState\",\n    ]\n  end\n  on_linux do\n    depends_on arch: :x86_64\n\n    app_image \"Inkscape-#{version.csv.first}.AppImage\", target: \"Inkscape.AppImage\"\n\n    zap trash: \"~/.config/inkscape\"\n  end\n\n  url \"https://media.inkscape.org/dl/resources/file/Inkscape-#{version.csv.second || version.csv.first}#{arch}.#{url_end}\"\n  name \"Inkscape\"\n  desc \"Vector graphics editor\"\n  homepage \"https://inkscape.org/\"\n\n  # Inkscape releases use a version format like 1.4, 1.4.1, etc. but the file\n  # name version can sometimes use a longer number (e.g. 1.4.230579 for 1.4.2,\n  # 1.3.0 for 1.3, etc.).\n  livecheck do\n    url \"https://inkscape.org/release/all/#{livecheck_folder}/\"\n    regex(%r{/inkscape[._-]v?(\\d+(?:\\.\\d+)+)/?[\"' >].*?Inkscape[._-]v?(\\d+(?:\\.\\d+)+)#{arch}\\.#{url_end}}im)\n    strategy :page_match do |page, regex|\n      # Match within individual row elements to ensure the regex doesn't capture\n      # a release version and file name version from separate releases\n      page.scan(%r{<tr[^>]*?>(.*?)</tr>}im).filter_map do |row|\n        match = row[0].match(regex)\n        next unless match\n\n        (match[1] == match[2]) ? match[1] : \"#{match[1]},#{match[2]}\"\n      end\n    end\n  end\nend\n").page_block.unwrap().shape, PageShape::SectionMatch { .. }));
}

/// r481 fixtures: oxygen's two-list extremes,
/// macgamestore's guarded child-attribute match,
/// and output-factory's following-sibling step
/// reduce to data.
#[test]
fn r481_xml_two_list_child_attr_following_sibling() {
    use crate::abrew::formula::XmlShape;
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    assert!(matches!(parse("  livecheck do\n    url \"https://www.oxygenxml.com/rssBuildID.xml\"\n    regex(/Oxygen\\s+XML\\s+Editor\\s+(?:version\\s+)?v?(\\d+(?:\\.\\d+)+)/i)\n    strategy :xml do |xml, regex|\n      versions = xml.get_elements(\"//description\").filter_map do |item|\n        match = item.text&.strip&.match(regex)\n        next if match.blank?\n\n        match[1]\n      end\n      builds = xml.get_elements(\"//guid\").map { |item| item.text&.strip }\n\n      version = versions.max_by { |v| Version.new(v) }\n      build = builds.max\n      next if version.blank? || build.blank?\n\n      \"#{version},#{build}\"\n    end\n  end\n").xml_block.unwrap().shape, XmlShape::TwoListExtreme { .. }));
    assert!(matches!(parse("  livecheck do\n    url \"https://www.macgamestore.com/api_clientapp/clientupdates/public/update-2021.xml\"\n    regex(/MacGameStore[._-]v?(\\d+(?:\\.\\d+)+)[._-](\\d+)\\.zip/i)\n    strategy :xml do |xml, regex|\n      xml.get_elements(\"//item\").map do |item|\n        next if item.elements[\"sparkle:isBetaBuild\"]&.text&.include?(\"1\")\n\n        url = item.elements[\"enclosure\"]&.attributes&.[](\"url\")\n        match = url.match(regex) if url\n        next if match.blank?\n\n        \"#{match[1]},#{match[2]}\"\n      end\n    end\n  end\n").xml_block.unwrap().shape, XmlShape::Template(..)));
    assert!(matches!(parse("  livecheck do\n    url \"https://zevrix.com/zsversioncontrol/ZSVersionControl.xml\"\n    strategy :xml do |xml|\n      xml.elements[\n        \"//dict/key[text()='Output Factory']/following-sibling::dict[1]/key[text()='version']\",\n      ]&.next_element&.text\n    end\n  end\n").xml_block.unwrap().shape, XmlShape::Template(..)));
}

/// r480 fixtures: wetype's fallback dig match,
/// font-oppo-sans's two named dig matches, mega's
/// OR-guarded map template, and synology-drive's
/// nested guarded map reduce to data.
#[test]
fn r480_json_multi_match_and_or_guards() {
    use crate::abrew::formula::JsonShape;
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    assert!(matches!(parse("  livecheck do\n    url \"https://z.weixin.qq.com/web/api/app_info\"\n    regex(/WeTypeInstaller[._-]v?(\\d+(?:\\.\\d+)+)[._-](\\d+)\\.zip/i)\n    strategy :json do |json, regex|\n      match = json.dig(\"data\", \"mac\", \"download_link\")&.match(regex)\n\n      # Try to match the version from the `InstallInfo` file name if the\n      # Mac installer file name doesn't include a version\n      match ||= json.dig(\"data\", \"mac\", \"InstallInfo\")&.match(/v?(\\d+(?:\\.\\d+)+)[_-](\\d+)/i)\n      next unless match\n\n      \"#{match[1]},#{match[2]}\"\n    end\n  end\n").json_block.unwrap().shape, JsonShape::MultiMatch { .. }));
    assert!(matches!(parse("  livecheck do\n    url \"https://open.oppomobile.com/wiki/new-doc/detail.json?doc_id=13223\"\n    regex(/Oppo\\s+Sans\\s+v?(\\d+(?:\\.\\d+)+)/i)\n    strategy :json do |json, regex|\n      url_regex = %r{oop/(\\d+)/(\\d+)/(\\h+)\\.zip}i\n\n      version_match = json.dig(\"data\", \"title\")&.match(regex)\n      next if version_match.blank?\n\n      url_match = json.dig(\"data\", \"content\")&.match(url_regex)\n      next if url_match.blank?\n\n      \"#{version_match[1]},#{url_match[1]},#{url_match[2]},#{url_match[3]}\"\n    end\n  end\n").json_block.unwrap().shape, JsonShape::MultiMatch { .. }));
    assert!(matches!(parse("  livecheck do\n    url \"https://www.megasoftware.net/current_release/\",\n        user_agent: :browser\n    strategy :json do |json|\n      json.map do |item|\n        next if item[\"operating_system\"] != \"mac\" ||\n                item[\"release_type\"] != \"stable\" ||\n                !item[\"interface\"].match?(/Graphical|GUI/i)\n\n        major = item[\"major\"]\n        minor = item[\"minor\"]\n        release = item[\"release\"]\n        next if major.blank? || minor.blank? || release.blank?\n\n        \"#{major}.#{minor}.#{release}\"\n      end\n    end\n  end\n").json_block.unwrap().shape, JsonShape::Template { .. }));
    assert!(matches!(parse("  livecheck do\n    url \"https://www.synology.com/api/support/findDownloadInfo?lang=en-us&product=DS923%2B\"\n    strategy :json do |json|\n      json.dig(\"info\", \"utilities\", \"detail\")&.map do |group|\n        group[\"items\"]&.map do |item|\n          next if item[\"identify\"] != \"SynologyDriveClient\"\n          next if item.dig(\"files\", \"Mac\").blank?\n\n          item[\"version\"]&.tr(\"-\", \",\")\n        end\n      end&.flatten\n    end\n  end\n").json_block.unwrap().shape, JsonShape::NestedMap { .. }));
}

#[test]
fn json_match_fallback_must_precede_its_required_match_guard() {
    use crate::abrew::formula::JsonShape;

    // Keep the parser and the actual JSON evaluator in the same control. A private file
    // supplies page_fetch's bytes; this makes no claim about HTTP strategy URL admission.
    fn versions(text: &str, url: &str) -> Option<Vec<String>> {
        let spec = crate::abrew::tap::parse_livecheck_block(text).unwrap();
        assert!(spec.strategy_block);
        let block = spec.json_block?;
        assert!(matches!(block.shape, JsonShape::MultiMatch { .. }));
        let plan = RunPlan {
            strat: Strat::Json,
            url: url.into(),
            re: Some(lc_regex(spec.regex.as_deref().unwrap(), &spec.regex_flags).unwrap()),
            candidates: Vec::new(),
            strategy_url: None,
            ua: Some(String::new()),
            ua_option: None,
            ua_symbol: false,
            gnome_filter: false,
            json: Some(block),
            xml: None,
            yaml: None,
            page_block: None,
            ghrel_block: None,
            git: None,
            sparkle_block: None,
            header_block: None,
            latest: None,
            post_form: None,
            post_json: None,
            headers: Vec::new(),
            major_minor: String::new(),
            own_version: String::new(),
            plist: None,
            apps: Vec::new(),
            uncompressed: true,
        };
        Some(match json_versions(&plan) {
            Found::Versions(versions, _) => versions,
            Found::Messages(messages) => panic!("local JSON evaluation refused: {messages:?}"),
            Found::Raised(message) => panic!("local JSON evaluation raised: {message}"),
        })
    }

    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("matches.json");
    let url = format!("file://{}", path.display());
    let source = |statements: &str| {
        format!(
            r##"  livecheck do
    url "https://example.invalid/not-fetched.json"
    regex(/v(\d+)/)
    strategy :json do |json, regex|
      matched = json.dig("stable")&.match(regex)
{statements}
      "#{{matched[1]}}"
    end
  end
"##
        )
    };
    for guard in ["next unless matched", "next if matched.blank?"] {
        let fallback = "matched ||= json.dig(\"fallback\")&.match(regex)";
        let permitted = source(&format!("      {fallback}\n      {guard}"));
        let skipped = source(&format!("      {guard}\n      {fallback}"));
        for (json, expected) in [
            (r#"{"stable":"none","fallback":"v2"}"#, vec!["2"]),
            (r#"{"stable":"v1","fallback":"v2"}"#, vec!["1"]),
            (r#"{"stable":"none","fallback":"none"}"#, Vec::new()),
        ] {
            std::fs::write(&path, json).unwrap();
            assert_eq!(
                versions(&permitted, &url).unwrap(),
                expected,
                "{guard}: {json}"
            );
        }

        // Ruby skips before it can reach the fallback. The flat native attempts cannot
        // express that order, so the top-level parser must leave the block unsupported.
        // Without that boundary, the real evaluator would read this payload and return 2.
        std::fs::write(&path, r#"{"stable":"none","fallback":"v2"}"#).unwrap();
        assert_eq!(versions(&skipped, &url), None, "{guard}");
        let top = crate::abrew::tap::parse_livecheck_block(&skipped).unwrap();
        assert!(top.strategy_block && top.json_block.is_none(), "{guard}");
    }
}

/// r479: server bytes appended after a zip's end
/// record trim in scratch, then the strict extractor
/// validates the archive as usual, including an
/// in-root app-bundle symlink.
#[test]
fn r479_zip_appended_server_bytes_trim() {
    use crate::extract::fixtures::{ZipMember, zip_bytes};
    let bytes = zip_bytes(
        &[
            ZipMember {
                name: "Probe.app/Contents/Info.plist",
                mode: 0o100_644,
                data: b"probe",
                deflate: false,
            },
            ZipMember {
                name: "Probe.app/Contents/Current",
                mode: 0o120_777,
                data: b"Info.plist",
                deflate: false,
            },
        ],
        false,
    );
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let exact = tmp.path().join("exact.zip");
    std::fs::write(&exact, &bytes).unwrap();
    assert_eq!(zip_trimmed_end(&exact), None);

    let mut appended = bytes.clone();
    appended.extend_from_slice(b"\nWarning: generated after the archive\n");
    let path = tmp.path().join("appended.zip");
    std::fs::write(&path, appended).unwrap();
    assert_eq!(zip_trimmed_end(&path), Some(bytes.len() as u64));
    zip_trim_appended_bytes(&path);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let root = tmp.path().join("extracted");
    crate::extract::extract_zip_tree(
        &path,
        &root,
        crate::install::UNSIGNED_SIZE_CAP,
        crate::install::MAX_ENTRIES,
        plist_zip_options(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(root.join("Probe.app/Contents/Info.plist")).unwrap(),
        b"probe"
    );
    assert_eq!(
        std::fs::read_link(root.join("Probe.app/Contents/Current")).unwrap(),
        std::path::Path::new("Info.plist")
    );
}

/// r479 fixture: scribus's sourceforge early-next
/// equality fork parses as a MatchFork.
#[test]
fn r479_scribus_early_next_fork() {
    let text = "  livecheck do\n    url \"https://sourceforge.net/projects/scribus/rss?path=/scribus\"\n    regex %r{url=.*?/v?(\\d+(?:\\.\\d+)+)/scribus[._-]v?(\\d+(?:[._]\\d+)+)(?:-x86_64)?\\.dmg}i\n    strategy :sourceforge do |page, regex|\n      match = page.match(regex)\n      next if match.blank?\n\n      next match[1] if match[1] == match[2]\n\n      \"#{match[1]},#{match[2]}\"\n    end\n  end\n";
    let spec = crate::abrew::tap::parse_livecheck_block(text).unwrap();
    assert!(matches!(
        spec.page_block.unwrap().shape,
        crate::abrew::formula::PageShape::MatchFork {
            cond: crate::abrew::formula::PageCond::CapEq(1, 2),
            ..
        }
    ));
}

/// r478 fixtures: quarkclouddrive's fallback dig
/// match, expressvpn's entries first match and
/// rnote's nested collect parse to data shapes.
#[test]
fn r478_json_residue_shapes() {
    use crate::abrew::formula::JsonShape;
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    let quark = "  livecheck do\n    url \"https://pan.quark.cn/api/client_version\"\n    regex(%r{QuarkCloudDriveMac[._-]v?(\\d+(?:\\.\\d+)+)[^/]*\\(Build(\\d+)\\)\\.dmg}i)\n    strategy :json do |json, regex|\n      url = json.dig(\"data\", \"origin_macDmgForArmUrl\") || json.dig(\"data\", \"macDmgForArmUrl\")\n      match = url&.match(regex)\n      next if match.blank?\n\n      \"#{match[1]},#{match[2]}\"\n    end\n  end\n";
    assert!(matches!(
        parse(quark).json_block.unwrap().shape,
        JsonShape::DigMatch { .. }
    ));
    let express = "  livecheck do\n    url \"https://example.com/api/v2/installers\"\n    regex(/expressvpn[._-]macos[._-]universal[._-]v?(\\d+(?:\\.\\d+)+)[._-]release\\.zip/i)\n    strategy :json do |json, regex|\n      version = nil\n      json[\"installers\"].filter_map do |_key, item|\n        match = item.dig(\"locations\", \"default\")&.match(regex)\n        next unless match\n\n        version = match[1]\n        break\n      end\n      version\n    end\n  end\n";
    assert!(matches!(
        parse(express).json_block.unwrap().shape,
        JsonShape::EntriesFirstMatch { .. }
    ));
    let rnote = "  livecheck do\n    url \"https://example.com/releases.json\"\n    regex(/rnote-v?(\\d+(?:\\.\\d+)+)-/i)\n    strategy :json do |json, regex|\n      json.map do |item|\n        item.dig(\"assets\", \"links\")&.filter_map do |asset|\n          asset[\"direct_asset_url\"]&.[](regex, 1)\n        end\n      end.flatten\n    end\n  end\n";
    assert!(matches!(
        parse(rnote).json_block.unwrap().shape,
        JsonShape::NestedCollect { .. }
    ));
}

/// r477 fixtures: the github_latest residue —
/// ultimaker-cura's match over a transformed tag,
/// clock-bar's second field capture, creality's
/// version-component fork and recaf's asset pick.
#[test]
fn r477_github_latest_residue_shapes() {
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    let ultimaker = "  livecheck do\n    url :url\n    regex(/^(\\d+(?:\\.\\d+)+)/i)\n    strategy :github_latest do |json, regex|\n      tag = json[\"tag_name\"]&.sub(/^\\D+/, \"\")\n      match = tag&.match(regex)\n      next if match.blank?\n\n      (match[1] == tag) ? match[1] : \"#{match[1]},#{tag}\"\n    end\n  end\n";
    let blk = parse(ultimaker).latest_block.unwrap();
    assert!(blk.match_pre);
    let clock = "  livecheck do\n    url :url\n    regex(/^v?(\\d+(?:\\.\\d+)+)$/i)\n    strategy :github_latest do |json, regex|\n      version_match = json[\"tag_name\"]&.match(regex)\n      next if version_match.blank?\n\n      id_match = json[\"body\"]&.match(%r{/(\\d+)/Clock\\.Bar\\.app\\.zip}i)\n      next if id_match.blank?\n\n      \"#{version_match[1]},#{id_match[1]}\"\n    end\n  end\n";
    let spec = parse(clock);
    let blk = spec.latest_block.as_ref().expect("clock-bar latest_block");
    assert!(blk.pre_capture.is_some());
    let creality = "  livecheck do\n    url :url\n    regex(%r{/v?(\\d+(?:\\.\\d+)+)/Creality[._-]?Print[._-]v?(\\d+(?:\\.\\d+)+)[._-]macx[._-]demo[._-]Release\\.dmg}i)\n    strategy :github_latest do |json, regex|\n      json[\"assets\"]&.map do |asset|\n        match = asset[\"browser_download_url\"]&.match(regex)\n        next unless match\n\n        if Version.new(match[2]).major_minor_patch.to_s == match[1]\n          match[2]\n        else\n          \"#{match[2]},#{match[1]}\"\n        end\n      end\n    end\n  end\n";
    let spec = parse(creality);
    assert!(
        format!("{:?}", spec.latest_block.unwrap().render).contains("CapMmp"),
        "creality must reduce to a component fork"
    );
    let recaf = "  livecheck do\n    url :url\n    regex(/^v?(\\d+(?:\\.\\d+)+(?:-alpha)?)$/i)\n    strategy :github_latest do |json, regex|\n      match = json[\"tag_name\"]&.match(regex)\n      next if match.blank?\n\n      assets = json[\"assets\"]&.select do |asset|\n        %w[recaf-4x-alpha-mac-aarch.jar recaf-4x-alpha-mac-86-x64.jar].include?(asset[\"name\"])\n      end\n      next if assets.blank? || assets.length != 2\n\n      \"#{match[1]},#{assets.map { |asset| asset[\"updated_at\"] }.max.gsub(/\\D/, \"\")}\"\n    end\n  end\n";
    let blk = parse(recaf).latest_block.unwrap();
    assert_eq!(blk.asset_pick.as_ref().map(|(_, n)| *n), Some(2));
    assert!(
        format!("{:?}", blk.render).contains("AssetsMaxUpdatedDigits"),
        "recaf render must read the picked assets' newest update"
    );
}

/// r475 fixtures: insomnia's ternary check URL,
/// mindforger's split-index URL terminal, recut's
/// reserved `version` local + all-headers first
/// match, and gzdoom's tr-equal latest fork.
#[test]
fn r475_token_and_block_shapes() {
    use crate::abrew::formula::cask_version_token;
    let ternary = "version.end_with?(\".0.0\") ? (version.major.to_i - 1) : version.major";
    assert_eq!(cask_version_token("13.4.0", ternary).as_deref(), Some("13"));
    assert_eq!(cask_version_token("13.0.0", ternary).as_deref(), Some("12"));
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    let insomnia = "  livecheck do\n    url \"https://updates.insomnia.rest/builds/check/mac?v=#{version.end_with?(\".0.0\") ? (version.major.to_i - 1) : version.major}.0.0&app=com.insomnia.app&channel=stable\"\n    strategy :json do |json|\n      json[\"name\"]\n    end\n  end\n";
    let spec = parse(insomnia);
    assert!(spec.json_block.is_some());
    // insomnia@alpha: `include?` predicate + a
    // quoted conditional-string token.
    assert_eq!(
        cask_version_token(
            "13.4.0-beta.1",
            "version.include?(\".0.0\") ? (version.major.to_i - 1) : version.major"
        )
        .as_deref(),
        Some("13")
    );
    assert_eq!(
        cask_version_token("13.4.0-beta.1", "\"-beta.0\" if version.include?(\"-\")").as_deref(),
        Some("-beta.0")
    );
    assert_eq!(
        cask_version_token("13.4.0", "\"-beta.0\" if version.include?(\"-\")").as_deref(),
        Some("")
    );
    let alpha = "  livecheck do\n    url \"https://updates.insomnia.rest/builds/check/mac?v=#{version.include?(\".0.0\") ? (version.major.to_i - 1) : version.major}.0.0#{\"-beta.0\" if version.include?(\"-\")}&app=com.insomnia.app&channel=beta\"\n    strategy :json do |json|\n      json[\"name\"]\n    end\n  end\n";
    assert!(parse(alpha).json_block.is_some());
    assert!(
        format!("{:?}", spec.url).contains("version.major.to_i - 1"),
        "the ternary stays verbatim for plan-time substitution"
    );

    assert_eq!(
        cask_version_token(
            "2.0.0-20240217.170738,5152",
            "version.csv.first.split(\"-\").first"
        )
        .as_deref(),
        Some("2.0.0")
    );
    assert_eq!(
        cask_version_token(
            "2.0.0-20240217.170738,5152",
            "version.csv.first.split(\"-\")[1]"
        )
        .as_deref(),
        Some("20240217.170738")
    );

    // A strategy-block `version = "…"` assignment
    // must not shadow `#{version.csv.second}` in
    // the download URL.
    let recut = "cask \"recut\" do\n  version \"4.5.8,blob123\"\n  url \"https://updates.getrecut.com/redirect/#{version.csv.second}/Recut_#{version.csv.first}_universal.dmg\"\n  livecheck do\n    url \"https://updates.getrecut.com/latest-mac\"\n    regex(%r{/redirect/([^/]+)/Recut[._-]v?(\\d+(?:\\.\\d+)+)}i)\n    strategy :header_match do |all_headers, regex|\n      version = nil\n      all_headers.each do |headers|\n        match = headers[\"location\"]&.match(regex)\n        next unless match\n\n        version = \"#{match[2]},#{match[1]}\"\n        break\n      end\n      version\n    end\n  end\nend\n";
    let c = crate::abrew::tap::parse_cask_rb_as_data(recut, "recut");
    assert_eq!(
        c.url,
        "https://updates.getrecut.com/redirect/blob123/Recut_4.5.8_universal.dmg"
    );
    let spec = parse(recut);
    assert!(matches!(
        spec.header_block.unwrap().shape,
        crate::abrew::formula::HeaderShape::AllTemplate(_)
    ));

    let gzdoom = "  livecheck do\n    url :url\n    regex(%r{/g?(\\d+(?:\\.\\d+)+)/gzdoom(?:-bin)?[._-]v?(\\d+(?:[.-]\\d+)+[a-z]?)(?:(?:-macOS)?\\.dmg|-macOS\\.zip)}i)\n    strategy :github_latest do |json, regex|\n      json[\"assets\"]&.map do |asset|\n        match = asset[\"browser_download_url\"]&.match(regex)\n        next if match.blank?\n\n        (match[2].tr(\"-\", \".\") == match[1]) ? match[1] : \"#{match[2].tr(\"-\", \".\")},#{match[1]}\"\n      end\n    end\n  end\n";
    let spec = parse(gzdoom);
    assert!(
        format!("{:?}", spec.latest_block.unwrap().render).contains("CapTr"),
        "gzdoom must reduce to a tr-equal fork"
    );
}

/// r474 fixtures: jpki's normalized capture,
/// fujifilm's bracket-capture two-match,
/// libreoffice-still's version minimum and
/// unity's nested first match parse; the scan
/// minimum and two-match render through the
/// genuine evaluators.
#[test]
fn r474_page_and_json_shapes() {
    use super::super::super::formula::PageShape;
    let parse = |text: &str| crate::abrew::tap::parse_livecheck_block(text).unwrap();
    // jpki
    let jpki = r##"  livecheck do
    url "https://www.jpki.go.jp/download/mac.html"
    regex(/href=.*?JPKIMac[._-]v?(\d+(?:[.-]\d+)+)(?:_(\d+(?:[.-]\d+)+))?\.dmg/i)
    strategy :page_match do |page, regex|
      page.scan(regex).map do |match|
        ver = match[0].tr("-", ".").split(".").map(&:to_i).join(".")
        match[1].present? ? "#{ver},#{match[1]}" : ver
      end
    end
  end
"##;
    let spec = parse(jpki);
    assert!(
        format!("{:?}", spec.page_block.unwrap().shape).contains("CapNormInts"),
        "jpki must reduce to a normalized-int capture part"
    );
    // fujifilm
    let fuji = r##"  livecheck do
    url "https://www.fujifilm-x.com/global/support/download/software/x-raw-studio/"
    regex(/Mac\s*Version\s*:\s*v?(\d+(?:\.\d+)+)/i)
    strategy :page_match do |page, regex|
      version = page[regex, 1]
      next if version.blank?

      dir = page[%r{href=.*?x-raw-studio-mac(?:\d+)-([^/]+)/XRawStudio(?:\d+)\.dmg}i, 1]
      next if dir.blank?

      "#{version},#{dir}"
    end
  end
"##;
    assert!(matches!(
        parse(fuji).page_block.unwrap().shape,
        PageShape::TwoMatch { .. }
    ));
    // libreoffice-still
    let lo = r##"cask "libreoffice-still" do
  arch arm: "aarch64", intel: "x86-64"
  folder = on_arch_conditional arm: "aarch64", intel: "x86_64"

  version "26.2.7"
  sha256 arm:   "58601808ae2b919c82dc7c4d6a26389de0e167f344ca48e5b6c9189218eb95ae",
         intel: "26a830205c617cc624b2a83783ef46434a4d6c337c3a873f79c43012171f83de"

  url "https://download.documentfoundation.org/libreoffice/stable/#{version}/mac/#{folder}/LibreOffice_#{version}_MacOS_#{arch}.dmg"
  name "LibreOffice Still"
  desc "Free cross-platform office suite, stable version recommended for enterprises"
  homepage "https://www.libreoffice.org/"

  # This checks the same source of version information as the `libreoffice`
  # cask, so we need to make sure that the former always checks a page that
  # provides the latest versions for both Fresh and Still.
  livecheck do
    url "https://www.libreoffice.org/download/"
    regex(%r{href=["']?[^"' >]*/stable/[^"' >]*LibreOffice[._-]v?(\d+(?:\.\d+)+)(?:[._-]MacOS)?[._-]#{arch}\.dmg}i)
    strategy :page_match do |page, regex|
      # Sort versions from lowest to highest, using the lowest (or only) version
      page.scan(regex).map(&:first).uniq.min_by { |v| Version.new(v) }
    end
  end
"##;
    assert!(matches!(
        parse(lo).page_block.unwrap().shape,
        PageShape::ScanExtreme(true)
    ));
    // unity
    let unity = r##"cask "unity" do
  arch arm: "Arm64"
  livecheck_arch = on_arch_conditional arm: "ARM64", intel: "X86_64"

  version "6000.6.9f1,8c45277fb9c5"
  sha256 arm:   "7d73299e668f25c21109e93fb3c5329c43ff36297a824e80763c3d520b3af4bc",
         intel: "0037a8ef93e66e2452aef4cf7430dcad5502d76c4d1eb1397a50d6b6c9d3d476"

  url "https://download.unity3d.com/download_unity/#{version.csv.second}/MacEditorInstaller#{arch}/Unity-#{version.csv.first}.pkg"
  name "Unity Editor"
  desc "Platform for 3D content"
  homepage "https://unity.com/"

  livecheck do
    url "https://services.api.unity.com/unity/editor/release/v1/releases?platform=MAC_OS&stream=SUPPORTED&architecture=#{livecheck_arch}&limit=1&offset=0"
    regex(%r{/(\h+)/MacEditorInstaller#{arch}/Unity[._-]v?(\d+(?:\.\d+)+(?:f\d+)?)\.pkg}i)
    strategy :json do |json, regex|
      match = nil
      json["results"]&.each do |item|
        item["downloads"]&.each do |download|
          match = download["url"]&.match(regex)
          break if match
        end
        break if match
      end
      next unless match

      "#{match[2]},#{match[1]}"
    end
  end
"##;
    assert!(matches!(
        parse(unity).json_block.unwrap().shape,
        crate::abrew::formula::JsonShape::NestedFirstMatch { .. }
    ));

    // jpki's download URL pipeline zero-pads each
    // dotted part of the first csv field.
    assert_eq!(
        crate::abrew::formula::cask_version_token(
            "3.9,01-01",
            "version.csv.first.split(\".\").map { |str| str.rjust(2, \"0\") }.join(\"-\")"
        )
        .as_deref(),
        Some("03-09")
    );

    let re = lc_regex(r"v([\d.]+)", "").unwrap();
    let content = "v26.1 v25.8 v26.1";
    let vals: Vec<String> = re
        .whole
        .find_iter(content)
        .filter_map(|m| extract_groups(&re, m.as_str()).into_iter().next().flatten())
        .collect();
    assert_eq!(
        vals.into_iter()
            .min_by(|a, b| lc_cmp(false, a, b))
            .as_deref(),
        Some("25.8")
    );
}
