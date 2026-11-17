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
    let mk = |full: &str| {
        let mut f = crate::abrew::formula::Formula::default();
        f.name = "immich-cli".to_string();
        f.full_name = full.to_string();
        f
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
    let rows: &[(&str, Option<(Option<&str>, &str)>)] = &[
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
    match plan_url(None, flit, "").unwrap() {
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
        let Ok(UrlPlan::Run(rp)) = github_plan(strat, url, None, Vec::new()) else {
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
        github_latest_versions_with("https://github.com/livecheck-private/pkg", &re, |slug| client.releases(slug).latest()),
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
            github_latest_versions_with("https://github.com/sample/tool.git", &re, |slug| {
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
        github_latest_versions_with("https://github.com/sample/private", &re, |slug| {
            github.releases(slug).latest()
        }),
        Found::Raised(_)
    ));
    assert!(matches!(
        github_latest_versions_with("https://github.com/sample/missing", &re, |slug| {
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
        name_skip: Some(("^api".into(), "i".into())),
        source: GhRelSource::TagName,
        then: GhRelThen::Tr("-".into(), ".".into()),
    };
    let found = github_releases_versions_with(
        "https://github.com/sample/tool/releases/download/v1/tool.tgz",
        &re,
        Some(&block),
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
