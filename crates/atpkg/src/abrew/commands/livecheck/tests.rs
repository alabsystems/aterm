// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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

/// r384: file-name version tokens and PyPI project
/// derivation, as shown by `brew livecheck -r
/// --verbose` on python-shaped resources.
#[test]
fn pypi_project_and_file_version_tokens() {
    let flit = "https://files.pythonhosted.org/packages/e7/91/flit_core-4.1.0.tar.gz";
    assert_eq!(pypi_project(flit).as_deref(), Some("flit-core"));
    assert_eq!(file_version_token(flit), Some((2, "4.1.0".to_string())));
    let whl = "https://files.pythonhosted.org/packages/a/packaging-26.3-py3-none-any.whl";
    assert_eq!(pypi_project(whl).as_deref(), Some("packaging"));
    assert_eq!(file_version_token(whl), Some((1, "26.3".to_string())));
    let zope = "https://files.pythonhosted.org/packages/b/zope_interface-7.0.3.tar.gz";
    assert_eq!(pypi_project(zope).as_deref(), Some("zope-interface"));
    assert_eq!(
        pypi_project("https://example.com/packaging-26.3.tar.gz"),
        None
    );
    // Path-segment fallback (influxdb@2 ui-assets).
    assert_eq!(
        url_version_token(
            "https://github.com/influxdata/ui/releases/download/OSS-v2.10.0/build.tar.gz"
        )
        .as_deref(),
        Some("2.10.0")
    );
    // Letter-suffixed tokens stay undetected (the run
    // delegates rather than guess).
    assert_eq!(
        file_version_token("https://files.pythonhosted.org/x/foo-1.0rc1.tar.gz"),
        None
    );
    // r389: resource-current detection across the
    // oracle-probed filename shapes (None = delegate;
    // the oracle's own answers for the delegated rows
    // are noted in the round's probes).
    let shapes: &[(&str, Option<&str>)] = &[
        ("foo-1.0rc1.tar.gz", Some("1.0rc1")),
        ("foo-2.0-beta.3.tar.gz", Some("2.0-beta.3")),
        ("foo-1.2.3b1.tar.gz", Some("1.2.3b1")),
        ("foo1.2.tar.gz", Some("1.2")),
        ("foo_1.2.3_alpha.tar.gz", Some("1.2.3_alpha")),
        ("foo-2024.01.tar.gz", Some("2024.01")),
        ("foo-r16.tar.gz", Some("16")),
        ("foo-1.0.0-linux.tar.gz", None),
        ("foo-1.2.3.4.5.tar.gz", Some("1.2.3.4.5")),
        ("foo-3.0a.tar.gz", Some("3.0a")),
        ("foo-1.1.1j.tar.gz", Some("1.1.1j")),
        ("foo-2.4.60-1.tar.gz", Some("2.4.60-1")),
        ("foo2bar-1.0.tar.gz", Some("1.0")),
        ("lib2to3-4.5.tar.gz", Some("4.5")),
        ("foo-1.2-3.4.tar.gz", None),
        ("foo128.tar.gz", Some("128")),
        ("foo-ver1.2.tar.gz", Some("1.2")),
        ("foo-1.2.3-rc.1+build5.tar.gz", None),
        ("foo-1.0_amd64.tar.gz", None),
        ("foo-1.0-src.tar.gz", None),
        ("foo-1.0.RELEASE.tar.gz", Some("1.0.RELEASE")),
        ("foo-1.0.0.Final.tar.gz", None),
        ("foo-9.4p1.tar.gz", Some("1")),
        ("foo-1.0.1-2.el8.tar.gz", None),
        ("packaging-26.3-py3-none-any.whl", Some("26.3")),
        ("zope_interface-7.0.3-py3-none-any.whl", Some("7.0.3")),
    ];
    for (name, want) in shapes {
        assert_eq!(
            url_version_token(&format!("https://example.com/x/{name}")).as_deref(),
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
    let text = "[profiles.reader]\nenv='LIVE_READER'\ntier='dev'\nlogin='reader'\n[[owners]]\nname='livecheck-private'\ntier='dev'\nread=['reader']\n";
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dirs = agh::config::Dirs::under(&scratch.path().canonicalize().unwrap());
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
            .with("LIVE_READER", &routed)
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
