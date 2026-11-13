// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn optional_index_walk_shares_one_deadline_and_stops_on_failure() {
    use std::cell::Cell;
    use std::time::Instant;
    let deadline = Instant::now() + INDEX_HINT_BOUND;
    let calls = Cell::new(0);
    let got = published_index_run_with(deadline, &|url, bound| {
        assert_eq!(bound, deadline, "each probe inherits the original deadline");
        calls.set(calls.get() + 1);
        if walked_build(url) <= 8 {
            published()
        } else {
            absent()
        }
    })
    .unwrap();
    assert_eq!(
        got,
        IndexHint {
            low: Some(1),
            newest: 8
        }
    );
    assert!(calls.get() > 1);
    calls.set(0);
    let refused = published_index_run_with(deadline, &|_, bound| {
        assert_eq!(bound, deadline);
        calls.set(calls.get() + 1);
        Err(aterm_update_core::HttpError::Transport("HTTP 504".into()))
    })
    .unwrap_err();
    assert!(refused.contains("HTTP 504"));
    assert_eq!(
        calls.get(),
        1,
        "no other probe round follows an unavailable hint"
    );
}

#[test]
fn a_repository_override_has_a_distinct_durable_artifact_namespace() {
    let official = GithubFetcher::new("alabsystems".into());
    let official_source =
        crate::flow::Fetcher::package_source_id(&official, "aterm", "cpython").unwrap();
    let mut overrides = std::collections::BTreeMap::new();
    overrides.insert("cpython".into(), "private/tools".into());
    let overridden = GithubFetcher::new("alabsystems".into()).with_overrides(overrides);
    let overridden_source =
        crate::flow::Fetcher::package_source_id(&overridden, "aterm", "cpython").unwrap();
    assert_eq!(official_source, "github:alabsystems/aterm");
    assert_eq!(overridden_source, "github:private/tools");
    assert_ne!(official_source, overridden_source);
}

#[test]
fn a_newest_exact_quad_transport_failure_never_selects_an_older_release() {
    use aterm_update_core::roster::{ROSTER_ASSET, ROSTER_SIG_ASSET};

    let slug = "alabsystems/aterm";
    let mut first_error = None;
    let fetch = |url: &str, _cap: u64| {
        if url.contains("/atpkg-index-2/") {
            Err(format!(
                "curl asset download failed with HTTP 504 for {url}"
            ))
        } else {
            Ok(url.as_bytes().to_vec())
        }
    };
    let error = fetch_run(slug, (2, 1), &by_url(&fetch), None, None, &mut first_error).unwrap_err();
    assert!(error.contains("HTTP 504"), "{error}");

    for unavailable in [false, true] {
        for (failed_asset, earlier_absent) in [
            ("index.toml", false),
            ("index.toml.sig", false),
            (ROSTER_ASSET, false),
            (ROSTER_SIG_ASSET, false),
            ("index.toml.sig", true),
            (ROSTER_ASSET, true),
            (ROSTER_SIG_ASSET, true),
        ] {
            let mut first_error = None;
            let detail = if unavailable { "HTTP 504" } else { "HTTP 403" };
            let fetch = |asset: &Named, _cap: u64| {
                if asset.tag == "atpkg-index-2" {
                    if asset.name == failed_asset {
                        return Err(if unavailable {
                            AssetError::Unavailable {
                                web: detail.into(),
                                api: "the API route did not deliver".into(),
                            }
                        } else {
                            AssetError::Refused(format!("{detail}: a proxy refused the asset"))
                        });
                    }
                    if earlier_absent && asset.name == "index.toml" {
                        return Err(AssetError::Absent("HTTP 404: no index asset".into()));
                    }
                }
                Ok(asset.web_url().into_bytes())
            };
            let error = fetch_run(slug, (2, 1), &fetch, None, None, &mut first_error)
                .expect_err("a newest quad acquisition failure must not select an older index");
            assert!(
                error.contains(detail),
                "{failed_asset}, earlier absence={earlier_absent}: {error}"
            );
        }
    }

    let mut first_error = None;
    let absent = |url: &str, _cap: u64| {
        if url.contains("/atpkg-index-2/") {
            Err(format!("HTTP 404 for {url}: release carries no asset"))
        } else {
            Ok(url.as_bytes().to_vec())
        }
    };
    let candidates =
        fetch_run(slug, (2, 1), &by_url(&absent), None, None, &mut first_error).unwrap();
    assert_eq!(
        candidates
            .iter()
            .map(|c| c.label.as_str())
            .collect::<Vec<_>>(),
        ["atpkg-index-1"],
        "negative control: an answered missing release is not a transport outage"
    );
}

#[test]
fn direct_urls_match_the_publishing_tag_convention() {
    let (toml, sig) = super::manifest_assets("alabsystems/ty", "ty", 2973).unwrap();
    let (toml, sig) = (toml.web_url(), sig.web_url());
    assert_eq!(
        toml,
        "https://github.com/alabsystems/ty/releases/download/atpkg-ty-2973/pkg-ty-2973.toml"
    );
    assert_eq!(
        sig,
        "https://github.com/alabsystems/ty/releases/download/atpkg-ty-2973/pkg-ty-2973.toml.sig"
    );
    assert_eq!(
        super::unit_asset("alabsystems/ty", "ty-2973.tar.zst")
            .unwrap()
            .web_url(),
        "https://github.com/alabsystems/ty/releases/download/atpkg-ty-2973/ty-2973.tar.zst"
    );
    assert_eq!(
        super::unit_asset("alabsystems/trust-mc", "trust-mc-20011.tar.zst")
            .unwrap()
            .web_url(),
        "https://github.com/alabsystems/trust-mc/releases/download/atpkg-trust-mc-20011/trust-mc-20011.tar.zst"
    );
    assert!(
        !super::unit_asset("alabsystems/ty", "ty-2973.tar.zst")
            .unwrap()
            .web_url()
            .contains(".tar/")
    );
}

#[test]
fn a_triple_suffixed_asset_derives_the_shared_build_tag() {
    for (slug, asset, tag) in [
        (
            "alabsystems/trust",
            "trust-4821-x86_64-unknown-linux-gnu.tar.zst",
            "atpkg-trust-4821",
        ),
        (
            "alabsystems/trust",
            "trust-4821-aarch64-apple-darwin.tar.zst",
            "atpkg-trust-4821",
        ),
        (
            "alabsystems/trust",
            "trust-4821.tar.zst",
            "atpkg-trust-4821",
        ),
        (
            "alabsystems/trust-mc",
            "trust-mc-20011-x86_64-unknown-linux-gnu.tar.zst",
            "atpkg-trust-mc-20011",
        ),
        (
            "alabsystems/ty",
            "ty-2973-aarch64-unknown-linux-musl.tar.zst",
            "atpkg-ty-2973",
        ),
    ] {
        assert_eq!(
            super::unit_asset(slug, asset)
                .map(|a| a.web_url())
                .as_deref(),
            Some(format!("https://github.com/{slug}/releases/download/{tag}/{asset}").as_str()),
            "{asset}"
        );
    }
    assert!(super::unit_asset("alabsystems/ty", "ty-latest.tar.zst").is_none());
}

#[test]
fn direct_urls_refuse_unsafe_slugs_and_names() {
    for bad in [
        "alabsystems",
        "alabsystems/ty/extra",
        "alabsystems/",
        "/ty",
        "alabsystems/..",
        "evil.com/x/../../alabsystems/ty",
        "https://evil.com/a",
    ] {
        assert!(
            super::manifest_assets(bad, "ty", 1).is_none(),
            "manifest URL built from unsafe slug {bad:?}"
        );
        assert!(
            super::unit_asset(bad, "ty-1.tar.zst").is_none(),
            "asset URL built from unsafe slug {bad:?}"
        );
    }
    assert!(super::manifest_assets("a/b", "../ty", 1).is_none());
    assert!(super::manifest_assets("a/b", "", 1).is_none());
    assert!(super::unit_asset("a/b", "../x.tar.zst").is_none());
    assert!(super::unit_asset("a/b", "noextension").is_none());
    for tag in ["v1?x", "v1#x", "v1%2f", "v 1", "", ".."] {
        assert!(
            super::Named::new("a/b", tag, "index.toml").is_none(),
            "{tag:?}"
        );
    }
    assert_eq!(
        super::Named::new("alabsystems/atpkg-index", "atpkg-index-41", "index.toml")
            .map(|a| a.web_url())
            .as_deref(),
        Some(
            "https://github.com/alabsystems/atpkg-index/releases/download/atpkg-index-41/index.toml"
        )
    );
}

#[test]
fn artifact_cap_is_the_signed_size_clamped_by_the_ceiling() {
    assert_eq!(super::artifact_cap(30_000_000), 30_000_000);
    assert_eq!(super::artifact_cap(0), super::ARTIFACT_CAP);
    assert_eq!(
        super::artifact_cap(super::ARTIFACT_CAP + 1),
        super::ARTIFACT_CAP
    );
    assert_eq!(super::artifact_cap(u64::MAX), super::ARTIFACT_CAP);
    assert_eq!(
        super::artifact_cap(super::ARTIFACT_CAP),
        super::ARTIFACT_CAP
    );
}

#[test]
fn the_fetcher_reads_through_the_api_route_when_the_web_route_answers_504() {
    use crate::flow::Fetcher as _;
    use agh_fake::{AssetSpec, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec};
    let fake = FakeGithub::start();
    fake.add_repo("alabsystems/ty", RepoSpec::public().push("alabsystems-bot"));
    let id = fake.add_release(
        "alabsystems/ty",
        ReleaseSpec::published("atpkg-ty-2973", "2026-10-01T00:00:00Z"),
    );
    for (name, bytes) in [
        ("pkg-ty-2973.toml", &b"manifest"[..]),
        ("pkg-ty-2973.toml.sig", b"signature"),
        ("ty-2973.tar.zst", b"archive"),
    ] {
        fake.add_asset(id, AssetSpec::uploaded(name, bytes));
    }
    let gateway_timeout = || Fault::Status {
        status: 504,
        body: String::new(),
        headers: Vec::new(),
    };
    fake.fault_always(Match::web("/alabsystems/ty/releases/"), gateway_timeout());
    let p = fake.ports();
    let origins = aterm_update_core::github_rest::Origins::loopback(p.api, p.uploads, p.web, p.cdn);
    let f = GithubFetcher::new("alabsystems".into()).with_reader(Reader::loopback(
        origins.clone(),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    ));
    assert_eq!(
        f.pkg_manifest("ty", "ty", 2973),
        Ok((b"manifest".to_vec(), b"signature".to_vec()))
    );
    let dir = crate::testdir::dir("atpkg-api-route-");
    let dest = dir.join("ty-2973.tar.zst");
    f.download_for("ty", "ty", "ty-2973.tar.zst", &dest, 7)
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"archive");
    // A vendor URL naming a release asset is read the same way.
    let vendor = dir.join("vendor.tar.zst");
    let url = "https://github.com/alabsystems/ty/releases/download/atpkg-ty-2973/ty-2973.tar.zst";
    f.download_url(url, &vendor, 7).unwrap();
    assert_eq!(std::fs::read(&vendor).unwrap(), b"archive");
    // Negative control: a URL that is not github.com's release route stays the vendor's
    // own (and plain http is refused before any request).
    let asked = fake.seen().len();
    let plain = dir.join("plain");
    assert!(
        f.download_url(&url.replacen("https", "http", 1), &plain, 7)
            .is_err()
    );
    assert_eq!(fake.seen().len(), asked, "the reader was not asked");
    let seen = fake.seen();
    assert!(
        seen.iter().any(|s| s.origin == FakeOrigin::Web),
        "the web route was asked first"
    );
    assert!(
        seen.iter().all(|s| !s.had_authorization),
        "atpkg reads anonymously on every route"
    );
    assert_eq!(
        seen.iter()
            .filter(|s| s.path_and_query == "/repos/alabsystems/ty/releases/tags/atpkg-ty-2973")
            .count(),
        1,
        "one listing of the release served the pass"
    );
    fake.fault_always(
        Match::api("/repos/alabsystems/ty/releases/"),
        gateway_timeout(),
    );
    let both_down = GithubFetcher::new("alabsystems".into()).with_reader(Reader::loopback(
        origins,
        None,
        std::time::Duration::from_millis(200),
    ));
    let err = both_down.pkg_manifest("ty", "ty", 2973).unwrap_err();
    assert!(
        err.contains("web route") && err.contains("API route"),
        "with both routes down the read names both: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_packages_share_one_release_of_api_recovery() {
    use crate::flow::Fetcher as _;
    use agh_fake::{AssetSpec, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec};
    let fake = FakeGithub::start();
    for program in ["ty", "ay"] {
        let slug = format!("alabsystems/{program}");
        fake.add_repo(&slug, RepoSpec::public().push("alabsystems-bot"));
        let id = fake.add_release(
            &slug,
            ReleaseSpec::published(&format!("atpkg-{program}-7"), "2026-10-01T00:00:00Z"),
        );
        fake.add_asset(
            id,
            AssetSpec::uploaded(&format!("pkg-{program}-7.toml"), b"m"),
        );
        fake.add_asset(
            id,
            AssetSpec::uploaded(&format!("pkg-{program}-7.toml.sig"), b"s"),
        );
        fake.fault_always(
            Match::web(&format!("/{slug}/releases/")),
            Fault::Status {
                status: 504,
                body: String::new(),
                headers: Vec::new(),
            },
        );
    }
    let p = fake.ports();
    let origins = aterm_update_core::github_rest::Origins::loopback(p.api, p.uploads, p.web, p.cdn);
    let f = GithubFetcher::new("alabsystems".into()).with_reader(Reader::loopback(
        origins,
        None,
        std::time::Duration::from_secs(2),
    ));
    let got: Vec<Result<_, String>> = std::thread::scope(|s| {
        let both = ["ty", "ay"].map(|program| {
            let f = &f;
            s.spawn(move || f.pkg_manifest(program, program, 7))
        });
        both.map(|h| h.join().expect("no panic")).into()
    });
    assert_eq!(
        got.iter().filter(|r| r.is_ok()).count(),
        1,
        "one package recovered: {got:?}"
    );
    let refused = got
        .iter()
        .find_map(|r| r.as_ref().err())
        .expect("one refused");
    assert!(refused.contains("is spent"), "{refused}");
    let listings = fake
        .seen()
        .into_iter()
        .filter(|s| s.origin == FakeOrigin::Api && s.path_and_query.contains("/releases/tags/"))
        .count();
    assert_eq!(listings, 1, "one release listed for the pass");
}

#[test]
fn dir_fetcher_rejects_traversal_names() {
    use crate::flow::Fetcher as _;
    let d = crate::testdir::dir("atpkg-dirfetch-");
    let f = DirFetcher::new(d.clone());
    let dest = d.join("out");
    assert!(f.download("r", "../../etc/passwd", &dest).is_err());
    assert!(f.pkg_manifest("r", "../x", 1).is_err());
    assert!(!dest.exists(), "no file read/written outside dir");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn override_redirects_only_the_named_programs_slug() {
    use crate::flow::Fetcher as _;
    let mut overrides = std::collections::BTreeMap::new();
    overrides.insert("orc".to_string(), "alabsystems/orc-private".to_string());
    let f = GithubFetcher::new("alabsystems".into()).with_overrides(overrides);
    assert_eq!(f.slug_for("orc", "orc"), "alabsystems/orc-private");
    assert_eq!(f.slug_for("orc", "some-repo"), "alabsystems/orc-private");
    assert_eq!(f.slug_for("ay", "ay"), "alabsystems/ay");
    let plain = GithubFetcher::new("alabsystems".into());
    assert_eq!(plain.slug_for("orc", "orc"), "alabsystems/orc");
    assert_ne!(
        f.package_source_id("orc", "orc"),
        plain.package_source_id("orc", "orc")
    );
    assert_eq!(
        f.package_source_id("ay", "ay"),
        plain.package_source_id("ay", "ay")
    );
}

#[cfg(unix)]
#[test]
fn dir_manifest_fifo_and_symlink_return_without_blocking() {
    use crate::flow::Fetcher as _;
    use std::os::unix::ffi::OsStrExt as _;

    let d = crate::testdir::dir("atpkg-dirfetch-special-");
    let index = d.join("index.toml");
    let index_c = std::ffi::CString::new(index.as_os_str().as_bytes()).unwrap();
    // SAFETY: `index_c` is a live NUL-terminated path in our private fixture.
    assert_eq!(unsafe { libc::mkfifo(index_c.as_ptr(), 0o600) }, 0);
    std::fs::write(d.join("index.toml.sig"), b"sig").unwrap();
    std::fs::write(d.join("aterm-machines.toml"), b"roster").unwrap();
    std::fs::write(d.join("aterm-machines.toml.sig"), b"sig").unwrap();
    let fetcher = DirFetcher::new(d.clone());
    assert!(fetcher.index_candidates().unwrap().is_empty());

    std::fs::remove_file(&index).unwrap();
    let target = d.join("index-target.toml");
    std::fs::write(&target, b"index").unwrap();
    std::os::unix::fs::symlink(&target, &index).unwrap();
    assert!(fetcher.index_candidates().unwrap().is_empty());

    let pkg = d.join("pkg-ay-1.toml");
    std::os::unix::fs::symlink(&target, &pkg).unwrap();
    std::fs::write(d.join("pkg-ay-1.toml.sig"), b"sig").unwrap();
    assert!(fetcher.pkg_manifest("ignored", "ay", 1).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_stale_staging_hardlink_never_truncates_the_registry_file() {
    use crate::flow::Fetcher as _;
    let dir = crate::testdir::dir("atpkg-hl-");
    std::fs::create_dir_all(dir.join("reg")).unwrap();
    std::fs::create_dir_all(dir.join("staging")).unwrap();
    let payload = b"the registry bytes that must survive";
    let src = dir.join("reg/ay-18.tar.zst");
    std::fs::write(&src, payload).unwrap();
    let dest = dir.join("staging/ay-18.tar.zst");

    let f = DirFetcher::new(dir.join("reg"));
    f.download("r", "ay-18.tar.zst", &dest).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    f.download("r", "ay-18.tar.zst", &dest).unwrap();
    assert_eq!(
        std::fs::read(&src).unwrap(),
        payload,
        "the registry file must be byte-intact after a retry"
    );
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        payload,
        "and the staged copy must hold the real bytes, not an empty file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_github_fetcher_fetches_vendor_documents_only_under_the_pins() {
    use crate::flow::Fetcher as _;
    let fetcher = GithubFetcher::new("alabsystems".into());
    let dest = std::env::temp_dir().join("atpkg-vendor-unpinned-never-written");
    for (program, url) in [
        ("claude", "https://evil.example/claude-code-releases/latest"),
        (
            "claude",
            "https://github.com/alabsystems/aterm/releases/download/v1/x",
        ),
        ("claude", "https://downloads.claude.ai/other/latest"),
        (
            "claude",
            "http://downloads.claude.ai/claude-code-releases/latest",
        ),
        (
            "claude",
            "https://downloads.claude.ai/claude-code-releases/latest?x=1",
        ),
        (
            "claude",
            "https://releases.openai.com/codex/channels/latest",
        ),
        (
            "codex",
            "https://downloads.claude.ai/claude-code-releases/latest",
        ),
        (
            "ay",
            "https://downloads.claude.ai/claude-code-releases/latest",
        ),
    ] {
        let refused = |e: VendorFetchError| match e {
            VendorFetchError::Refused(_) => {}
            other => panic!("{program} {url}: {other:?}"),
        };
        refused(fetcher.vendor_get(program, url, 64, None).unwrap_err());
        refused(fetcher.vendor_content_length(program, url).unwrap_err());
        refused(
            fetcher
                .vendor_download(program, url, &dest, 64)
                .unwrap_err(),
        );
    }
    assert!(!dest.exists(), "a refusal writes nothing");
    assert_eq!(
        vendor_get_from(aterm_update_core::VendorResponse::Body {
            bytes: b"2.1.280\n".to_vec(),
            etag: Some("\"e\"".into()),
            effective_url: "https://downloads.claude.ai/claude-code-releases/latest".into(),
        }),
        crate::flow::VendorGet::Body {
            bytes: b"2.1.280\n".to_vec(),
            etag: Some("\"e\"".into()),
            effective_url: "https://downloads.claude.ai/claude-code-releases/latest".into(),
        }
    );
    assert_eq!(
        vendor_get_from(aterm_update_core::VendorResponse::NotModified {
            etag: "\"e\"".into()
        }),
        crate::flow::VendorGet::NotModified {
            etag: "\"e\"".into()
        }
    );
}

#[test]
fn a_vendor_transport_error_is_unreachable_only_for_the_network() {
    use aterm_update_core::HttpError;
    const URL: &str = "https://downloads.claude.ai/claude-code-releases/latest";
    let status = |code: u16| HttpError::VendorStatus {
        code,
        url: URL.into(),
    };
    for e in [
        HttpError::Transport("curl GET x failed (exit 6): dns".into()),
        status(408),
        status(429),
        status(500),
        status(503),
    ] {
        let text = e.to_string();
        assert_eq!(vendor_error(e), VendorFetchError::Unreachable(text));
    }
    for e in [
        HttpError::VendorRefused("vendor document x exceeds its 64-byte cap".into()),
        HttpError::Malformed("no usable Content-Length".into()),
        status(304),
        status(403),
        status(404),
    ] {
        let text = e.to_string();
        assert_eq!(vendor_error(e), VendorFetchError::Refused(text));
    }
}

#[test]
fn an_underivable_name_is_refused_with_no_fallback() {
    use crate::flow::Fetcher as _;
    let f = GithubFetcher::new("alabsystems".into());
    let dest = std::env::temp_dir().join(format!(
        "atpkg-underivable-never-written-{}",
        std::process::id()
    ));
    assert!(f.pkg_manifest("ty", "../ty", 1).is_err());
    assert!(f.download("ty", "ty-latest.tar.zst", &dest).is_err());
    assert!(
        f.download_for("ty", "ty", "ty-latest.tar.zst", &dest, 1024)
            .is_err()
    );
    assert!(!dest.exists(), "nothing was fetched");
    assert_eq!(
        unit_asset(&f.slug_for("ty", "ty"), "ty-2973.tar.zst")
            .map(|a| a.web_url())
            .as_deref(),
        Some("https://github.com/alabsystems/ty/releases/download/atpkg-ty-2973/ty-2973.tar.zst"),
        "a published name derives its URL"
    );
}

fn walked_build(url: &str) -> u64 {
    url.strip_prefix("https://github.com/alabsystems/aterm/releases/download/atpkg-index-")
        .and_then(|rest| rest.strip_suffix("/index.toml"))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("not an index tag HEAD: {url}"))
}

fn published() -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
    Ok(aterm_update_core::HeadAnswer {
        code: 302,
        location: Some("https://release-assets.githubusercontent.com/object".into()),
    })
}

fn absent() -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
    Ok(aterm_update_core::HeadAnswer {
        code: 404,
        location: None,
    })
}

fn channel(
    first: u64,
    last: u64,
) -> impl Fn(u64) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
    move |build| {
        if (first..=last).contains(&build) {
            published()
        } else {
            absent()
        }
    }
}

struct Walked {
    /// The candidates' labels, newest first, or the discovery's error.
    labels: Result<Vec<String>, String>,
    /// The builds the walk asked, in its order: the [`sequential_walk`] reference's HEADs.
    heads: Vec<u64>,
    /// Every build the fetcher HEADed (the walk's own and the ones asked ahead), sorted.
    headed: Vec<u64>,
    /// Every URL the fetcher asked: the HEADs, then the asset downloads.
    asked: Vec<String>,
    /// The asset downloads alone, sorted: the quads are fetched at once, in no order.
    fetched: Vec<String>,
    /// Whether the fetcher now reports the link down.
    link_down: bool,
    /// How many rounds the fetcher's HEADs went out in.
    rounds: u32,
}

type SequentialHead<'a> =
    dyn FnMut(&str) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> + 'a;

type SequentialFetch<'a> = dyn FnMut(&str, u64) -> Result<Vec<u8>, String> + 'a;

fn sequential_walk(
    slug: &str,
    floor: u64,
    head: &mut SequentialHead<'_>,
    fetch: &mut SequentialFetch<'_>,
    link_down: &mut bool,
) -> Result<Vec<Candidate>, String> {
    let mut heads = 0u32;
    let mut probe = |build: u64| -> Result<bool, String> {
        heads += 1;
        if heads > WALK_HEAD_BUDGET {
            return Err(format!(
                "index discovery on {slug} found no end within {WALK_HEAD_BUDGET} HEADs"
            ));
        }
        let url = Named::new(slug, &index_tag(build), "index.toml")
            .expect("derivable")
            .web_url();
        let answer = head(&url).map_err(|e| {
            *link_down |= matches!(e, aterm_update_core::HttpError::Transport(_));
            format!("HEAD {url}: {e}")
        })?;
        let code = answer.code;
        match crate::index_probe::classify(build, Ok(answer)) {
            crate::index_probe::Probe::Published(_) => Ok(true),
            crate::index_probe::Probe::Missing => Ok(false),
            _ => Err(format!(
                "HEAD {url} answered {code}, not GitHub's redirect to a release asset"
            )),
        }
    };
    let mut fetch_run = |(newest, low): (u64, u64), first_error: &mut Option<String>| {
        let oldest = newest.saturating_sub(INDEX_CANDIDATE_CAP - 1).max(low);
        let mut out = Vec::new();
        for build in (oldest..=newest).rev() {
            let quad = index_quad(slug, &index_tag(build)).expect("a derivable tag");
            let mut fetch_quad = || -> Result<Candidate, String> {
                Ok(Candidate {
                    label: quad.label.clone(),
                    index_bytes: fetch(&quad.index.web_url(), MANIFEST_CAP)?,
                    sig: fetch(&quad.index_sig.web_url(), SIG_CAP)?,
                    roster_bytes: fetch(&quad.roster.web_url(), ROSTER_CAP)?,
                    roster_sig: fetch(&quad.roster_sig.web_url(), SIG_CAP)?,
                })
            };
            match fetch_quad() {
                Ok(candidate) => out.push(candidate),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        out
    };
    let run = newest_index(floor, &mut probe)?;
    let mut first_error = None;
    let mut out = fetch_run(run, &mut first_error);
    if out.is_empty()
        && floor > 0
        && run.0 == floor
        && matches!(probe(floor), Ok(false))
        && let Ok(found) = newest_index(0, &mut probe)
        && found.0 < floor - 1
    {
        out = fetch_run(found, &mut first_error);
    }
    if out.is_empty() {
        return Err(first_error.unwrap_or_else(|| underivable(slug, "the index")));
    }
    Ok(out)
}

fn walk(
    floor: u64,
    answer: &(
         dyn Fn(u64) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> + Sync
     ),
    missing: &[&str],
) -> Walked {
    walk_with(floor, None, answer, missing)
}

fn walk_with(
    floor: u64,
    hint: Option<IndexHint>,
    answer: &(
         dyn Fn(u64) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> + Sync
     ),
    missing: &[&str],
) -> Walked {
    use crate::flow::Fetcher as _;
    let download = |url: &str| -> Result<Vec<u8>, String> {
        if missing.iter().any(|m| url.ends_with(m)) {
            let mut msg = String::from("HTTP 404 for ");
            msg.push_str(url);
            return Err(msg);
        }
        Ok(url.as_bytes().to_vec())
    };
    let f = GithubFetcher::new("alabsystems".into())
        .with_index_floor(floor)
        .with_index_hint(hint);
    let headed = std::sync::Mutex::new(Vec::<String>::new());
    let fetched = std::sync::Mutex::new(Vec::<String>::new());
    let labels = f
        .walk_candidates(
            &f.index_slug(),
            &|url: &str| {
                headed.lock().unwrap().push(url.to_string());
                answer(walked_build(url))
            },
            &by_url(&|url: &str, _cap: u64| {
                fetched.lock().unwrap().push(url.to_string());
                download(url)
            }),
        )
        .map(|candidates| candidates.into_iter().map(|c| c.label).collect::<Vec<_>>());
    let mut heads = Vec::new();
    let mut reference_down = false;
    let reference = sequential_walk(
        &f.index_slug(),
        floor,
        &mut |url: &str| {
            heads.push(walked_build(url));
            answer(walked_build(url))
        },
        &mut |url: &str, _cap: u64| download(url),
        &mut reference_down,
    )
    .map(|candidates| candidates.into_iter().map(|c| c.label).collect::<Vec<_>>());
    assert_eq!(
        labels, reference,
        "from {floor}: the rounds land the reference's set"
    );
    assert_eq!(
        f.index_link_down(),
        reference_down,
        "from {floor}: the link verdict"
    );
    let headed = headed.into_inner().unwrap();
    let mut builds: Vec<u64> = headed.iter().map(|url| walked_build(url)).collect();
    builds.sort_unstable();
    assert!(
        builds.windows(2).all(|pair| pair[0] < pair[1]),
        "from {floor}: no build is HEADed twice: {builds:?}"
    );
    for build in &heads {
        assert!(
            builds.contains(build),
            "from {floor}: the walk's {build} was asked"
        );
    }
    assert!(
        builds.len() <= WALK_HEAD_BUDGET as usize + WALK_AHEAD_BUDGET,
        "from {floor}: {} HEADs",
        builds.len()
    );
    let mut fetched = fetched.into_inner().unwrap();
    let mut asked = headed;
    asked.extend(fetched.iter().cloned());
    fetched.sort();
    Walked {
        labels,
        heads,
        headed: builds,
        asked,
        fetched,
        link_down: f.index_link_down(),
        rounds: f.walk_cost[1].load(std::sync::atomic::Ordering::Relaxed),
    }
}

fn labels(builds: &[u64]) -> Result<Vec<String>, String> {
    Ok(builds.iter().map(|b| index_tag(*b)).collect())
}

fn quad_assets(builds: &[u64]) -> Vec<String> {
    let mut urls: Vec<String> = builds
        .iter()
        .flat_map(|b| {
            let q = index_quad("alabsystems/aterm", &index_tag(*b)).unwrap();
            [q.index, q.index_sig, q.roster, q.roster_sig].map(|a| a.web_url())
        })
        .collect();
    urls.sort();
    urls
}

#[test]
fn the_walk_resolves_the_next_index_with_no_api_request() {
    assert!(aterm_update_core::cdn::is_api_host(
        "https://api.github.com/repos/alabsystems/aterm/releases?per_page=100&page=1"
    ));
    let w = walk(44, &channel(6, 45), &[]);
    assert_eq!(w.labels, labels(&[45, 44]));
    assert_eq!(w.heads, vec![45, 46, 47]);
    assert_eq!(
        w.headed,
        vec![45, 46, 47],
        "nothing asked past the walk's own"
    );
    assert_eq!(
        w.asked.len(),
        3 + 8,
        "three HEADs, four assets per candidate"
    );
    assert_eq!(w.fetched, quad_assets(&[45, 44]), "both quads, whole");
    for url in &w.asked {
        assert!(!aterm_update_core::cdn::is_api_host(url), "{url}");
        assert!(
            url.starts_with("https://github.com/alabsystems/aterm/releases/download/"),
            "{url}"
        );
    }
    assert!(!w.link_down);
}

#[test]
fn the_walk_finds_the_newest_from_any_floor_and_from_none() {
    let steady = walk(45, &channel(6, 45), &[]);
    assert_eq!(steady.labels, labels(&[45]));
    assert_eq!(steady.heads, vec![46, 47], "two HEADs in the steady state");
    assert_eq!(steady.headed, vec![46, 47], "and none asked ahead of them");

    let behind = walk(40, &channel(6, 45), &[]);
    assert_eq!(
        behind.labels,
        labels(&[45, 44, 43, 42]),
        "newest first, capped"
    );

    let fresh = walk(0, &channel(6, 45), &[]);
    assert_eq!(fresh.labels, labels(&[45, 44, 43, 42]));

    let empty = walk(0, &|_| absent(), &[]);
    assert!(empty.labels.is_err());
    assert_eq!(
        empty.headed, empty.heads,
        "the search asks the powers alone"
    );
    assert!(
        empty
            .asked
            .iter()
            .all(|u| !aterm_update_core::cdn::is_api_host(u))
    );
}

#[test]
fn the_search_lands_the_newest_of_every_contiguous_channel() {
    for first in 1..=9u64 {
        for newest in first..=80u64 {
            let floors = std::iter::once(0).chain(first..=newest);
            for floor in floors {
                if floor == 0 && !(first..=newest).any(u64::is_power_of_two) {
                    continue;
                }
                let mut probes = 0u32;
                let found = newest_index(floor, &mut |build| {
                    probes += 1;
                    Ok((first..=newest).contains(&build))
                });
                let (hit, low) = found
                    .unwrap_or_else(|e| panic!("channel {first}..={newest} from {floor}: {e}"));
                assert_eq!(hit, newest, "channel {first}..={newest} from {floor}");
                assert!(
                    (first..=newest).contains(&low),
                    "{first}..={newest} from {floor}"
                );
                let distance = u32::try_from(newest - low + 1).unwrap();
                assert!(
                    probes <= 2 * (distance.ilog2() + 1) + 22,
                    "{probes} HEADs for channel {first}..={newest} from {floor}"
                );
            }
        }
    }
}

#[test]
fn the_search_steps_over_a_missing_number_and_stops_at_two() {
    for first in 1..=3u64 {
        for newest in first..=first + 16 {
            let interior = (newest - first).saturating_sub(1);
            for gaps in 0..(1u64 << interior) {
                if gaps & (gaps >> 1) != 0 {
                    continue;
                }
                let published = |build: u64| {
                    (first..=newest).contains(&build)
                        && (build == first
                            || build == newest
                            || gaps >> (build - first - 1) & 1 == 0)
                };
                let floors = std::iter::once(0).chain((first..=newest).filter(|&b| published(b)));
                for floor in floors {
                    if floor == 0 && !(first..=newest).any(|b| b.is_power_of_two() && published(b))
                    {
                        continue;
                    }
                    let mut probes = 0u32;
                    let found = newest_index(floor, &mut |build| {
                        probes += 1;
                        Ok(published(build))
                    });
                    let (hit, low) = found.unwrap_or_else(|e| {
                        panic!("channel {first}..={newest} gaps {gaps:b} from {floor}: {e}")
                    });
                    assert_eq!(
                        hit, newest,
                        "channel {first}..={newest} gaps {gaps:b} from {floor}"
                    );
                    assert!(
                        published(low),
                        "{first}..={newest} gaps {gaps:b} from {floor}"
                    );
                    assert!(probes <= WALK_HEAD_BUDGET, "{probes} HEADs");
                }
            }
        }
    }
    let gap = |build: u64| {
        if build == 45 {
            absent()
        } else {
            channel(6, 50)(build)
        }
    };
    let w = walk(44, &gap, &[]);
    assert_eq!(w.labels, labels(&[50, 49, 48, 47]));
    assert_eq!(&w.heads[..2], &[45, 46], "the step past the missing number");
    let w = walk(
        44,
        &|b| if b == 48 { absent() } else { channel(6, 50)(b) },
        &["atpkg-index-48/index.toml"],
    );
    assert_eq!(
        w.labels,
        labels(&[50, 49, 47]),
        "a missing number inside the window"
    );
    let double = |build: u64| {
        if build == 45 || build == 46 {
            absent()
        } else {
            channel(6, 50)(build)
        }
    };
    let w = walk(44, &double, &[]);
    assert_eq!(w.labels, labels(&[44]));
    assert_eq!(w.heads, vec![45, 46]);
    for first in 1..=3u64 {
        for newest in first + 3..=first + 16 {
            for floor in first..newest - 2 {
                let found = newest_index(floor, &mut |build| {
                    Ok((first..=newest).contains(&build)
                        && build != floor + 1
                        && build != floor + 2)
                });
                assert_eq!(found, Ok((floor, floor)), "{first}..={newest} from {floor}");
            }
        }
    }
}

#[test]
fn an_answer_that_says_nothing_fails_the_discovery() {
    let answer = |code: u16, location: Option<&str>| {
        let location = location.map(str::to_string);
        move |_: u64| {
            Ok(aterm_update_core::HeadAnswer {
                code,
                location: location.clone(),
            })
        }
    };
    for (why, reply) in [
        ("a portal page", answer(200, None)),
        (
            "a renamed repo",
            answer(
                301,
                Some("https://github.com/new/aterm/releases/download/x/index.toml"),
            ),
        ),
        (
            "a login redirect",
            answer(302, Some("https://sso.example.com/login")),
        ),
        ("a rate limit", answer(429, None)),
        ("an outage", answer(503, None)),
    ] {
        let w = walk(44, &reply, &[]);
        assert!(w.labels.is_err(), "{why}");
        assert_eq!(w.heads, vec![45], "{why}");
        assert!(!w.link_down, "{why}: a host answered");
    }
    let offline = walk(
        44,
        &|_| {
            Err(aterm_update_core::HttpError::Transport(
                "curl: (6) Could not resolve host".into(),
            ))
        },
        &[],
    );
    assert!(
        offline
            .labels
            .unwrap_err()
            .contains("Could not resolve host")
    );
    assert_eq!(offline.heads, vec![45]);
    assert_eq!(offline.asked.len(), offline.headed.len(), "nothing fetched");
    assert!(offline.link_down, "no host answered");
    use crate::flow::Fetcher as _;
    let f = GithubFetcher::new("alabsystems".into()).with_index_floor(44);
    let slug = f.index_slug();
    let fetch = |url: &str, _: u64| Ok(url.as_bytes().to_vec());
    let dead = |_: &str| {
        Err(aterm_update_core::HttpError::Transport(
            "curl: (6) Could not resolve host".into(),
        ))
    };
    assert!(f.walk_candidates(&slug, &dead, &by_url(&fetch)).is_err());
    assert!(f.index_link_down());
    let busy = |_: &str| {
        Ok(aterm_update_core::HeadAnswer {
            code: 503,
            location: None,
        })
    };
    assert!(f.walk_candidates(&slug, &busy, &by_url(&fetch)).is_err());
    assert!(!f.index_link_down(), "a host answered this walk");
}

#[test]
fn a_release_that_does_not_arrive_whole_is_left_out() {
    let w = walk(44, &channel(6, 46), &["atpkg-index-46/index.toml.sig"]);
    assert_eq!(w.labels, labels(&[45, 44]));
    assert_eq!(
        w.fetched,
        quad_assets(&[46, 45, 44]),
        "every quad of the window asked whole, the incomplete one too"
    );
    let w = walk(44, &channel(6, 46), &["atpkg-index-46/aterm-machines.toml"]);
    assert_eq!(w.labels, labels(&[45, 44]), "or its roster");
    let w = walk(44, &channel(6, 46), &["atpkg-index-45/index.toml.sig"]);
    assert_eq!(w.labels, labels(&[46, 44]), "an older one likewise");
    let w = walk(45, &channel(6, 45), &["/index.toml"]);
    let err = w.labels.unwrap_err();
    assert!(err.contains("atpkg-index-45/index.toml"), "{err}");
}

#[test]
fn a_floor_above_the_published_run_is_found_again_from_first_contact() {
    let poisoned = i64::MAX.unsigned_abs();
    let unpublished = format!("atpkg-index-{poisoned}/index.toml");
    let w = walk(poisoned, &channel(6, 46), &[unpublished.as_str()]);
    assert_eq!(w.labels, labels(&[46, 45, 44, 43]));
    let w = walk(45, &channel(6, 44), &["atpkg-index-45/index.toml"]);
    let err = w.labels.unwrap_err();
    assert!(err.contains("atpkg-index-45/index.toml"), "{err}");
}

/// What [`SignedChannel`] publishes for one pass.
#[derive(Clone)]
struct Publication {
    /// The builds whose releases are up; `None` is the link down.
    run: Option<std::ops::RangeInclusive<u64>>,
    /// The first build published beside the next roster generation.
    newer_roster_from: u64,
    /// The first build whose index declares that generation.
    newer_index_from: u64,
    /// The first build whose index a key on no roster signed.
    outsider_from: u64,
}

struct SignedChannel {
    fetcher: GithubFetcher,
    publication: std::cell::RefCell<Publication>,
    /// The builds up to which an index's freshness window has already lapsed.
    lapsed_through: std::cell::Cell<u64>,
    head_reads: std::sync::atomic::AtomicUsize,
    asset_reads: std::sync::atomic::AtomicUsize,
    assets_fail: std::sync::atomic::AtomicBool,
}

impl crate::flow::Fetcher for SignedChannel {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        use crate::sig::testkit;
        let p = self.publication.borrow().clone();
        let lapsed_through = self.lapsed_through.get();
        let head_reads = &self.head_reads;
        let asset_reads = &self.asset_reads;
        let assets_fail = &self.assets_fail;
        let up = |build: u64| p.run.as_ref().is_some_and(|run| run.contains(&build));
        let generation =
            |newer_from: u64, build: u64| testkit::SEQ + u64::from(build >= newer_from);
        self.fetcher.walk_candidates(
            &self.fetcher.index_slug(),
            &|url: &str| {
                head_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if p.run.is_none() {
                    return Err(aterm_update_core::HttpError::Transport(
                        "curl: (6) Could not resolve host".into(),
                    ));
                }
                if up(walked_build(url)) {
                    published()
                } else {
                    absent()
                }
            },
            &by_url(&|url: &str, _cap: u64| {
                asset_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if assets_fail.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err("exact release transport is blocked".into());
                }
                let (build, asset) = url
                    .split_once("/atpkg-index-")
                    .and_then(|(_, rest)| rest.split_once('/'))
                    .and_then(|(n, asset)| Some((n.parse::<u64>().ok()?, asset)))
                    .unwrap_or_else(|| panic!("not an index asset: {url}"));
                if !up(build) {
                    return Err(format!("HTTP 404 for {url}"));
                }
                let roster = aterm_update_core::roster::Roster {
                    roster_seq: generation(p.newer_roster_from, build),
                    ..testkit::roster()
                }
                .to_toml()
                .expect("a valid roster emits")
                .into_bytes();
                let valid_until = if build <= lapsed_through {
                    "2020-01-01T00:00:00Z"
                } else {
                    "2099-01-01T00:00:00Z"
                };
                let index = format!(
                    "schema = 2\nindex_build = {build}\nvalid_until = \"{valid_until}\"\n\
                     machine_id = \"{}\"\nroster_seq = {}\n[programs.ay]\nrepo = \"ay\"\n",
                    testkit::MACHINE_ID,
                    generation(p.newer_index_from, build),
                )
                .into_bytes();
                Ok(match asset {
                    "index.toml" => index,
                    "index.toml.sig" if build >= p.outsider_from => {
                        testkit::sign(&testkit::OUTSIDER_SEED, &index)
                    }
                    "index.toml.sig" => testkit::sign(&testkit::MACHINE_SEED, &index),
                    "aterm-machines.toml" => roster,
                    _ => testkit::sign(&testkit::MASTER_SEED, &roster),
                })
            }),
        )
    }
    fn index_link_down(&self) -> bool {
        self.fetcher.index_link_down()
    }
    fn cache_verified_index(&self, build: u64, candidate: &Candidate) {
        crate::flow::Fetcher::cache_verified_index(&self.fetcher, build, candidate);
    }
    fn pkg_manifest(&self, _: &str, _: &str, _: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
        Err("not asked".into())
    }
    fn download(&self, _: &str, _: &str, _: &std::path::Path) -> Result<(), String> {
        Err("not asked".into())
    }
    fn source_id(&self) -> String {
        github_source_id("alabsystems")
    }
}

#[test]
fn a_verified_exact_index_survives_blocked_asset_transport_but_not_corruption() {
    use crate::sig::testkit;
    use std::sync::atomic::Ordering;

    let store = crate::testdir::absent_layout("atpkg-net-signed-index-cache-");
    let f = SignedChannel {
        fetcher: GithubFetcher::new("alabsystems".into())
            .with_index_floor(47)
            .with_artifact_layout(&store),
        publication: std::cell::RefCell::new(Publication {
            run: Some(47..=47),
            newer_roster_from: u64::MAX,
            newer_index_from: u64::MAX,
            outsider_from: u64::MAX,
        }),
        lapsed_through: std::cell::Cell::new(0),
        head_reads: std::sync::atomic::AtomicUsize::new(0),
        asset_reads: std::sync::atomic::AtomicUsize::new(0),
        assets_fail: std::sync::atomic::AtomicBool::new(false),
    };
    let resolve = || {
        crate::flow::resolve_verified_index(
            &f,
            &store,
            &testkit::anchor(),
            crate::sig::BuildFloor::default(),
            testkit::NOW,
        )
    };

    assert_eq!(resolve().unwrap().index_build, 47);
    assert!(f.head_reads.load(Ordering::SeqCst) > 0);
    assert_eq!(f.asset_reads.load(Ordering::SeqCst), 4);

    f.head_reads.store(0, Ordering::SeqCst);
    f.asset_reads.store(0, Ordering::SeqCst);
    f.assets_fail.store(true, Ordering::SeqCst);
    assert_eq!(resolve().unwrap().index_build, 47);
    assert!(
        f.head_reads.load(Ordering::SeqCst) > 0,
        "freshness HEADs remain live"
    );
    assert_eq!(
        f.asset_reads.load(Ordering::SeqCst),
        0,
        "the verified exact quad needs no transport"
    );

    let source = github_source_id("alabsystems");
    let key =
        crate::signed_artifact_cache::SignedIndexKey::release(&source, "atpkg-index-47").unwrap();
    let cache = crate::signed_artifact_cache::SignedArtifactCache::for_layout(&store);
    let object = cache
        .object_path_for_index(&key)
        .expect("cached exact quad");
    std::fs::write(object, b"changed after verification").unwrap();
    let _ = std::fs::remove_file(store.prefix.join("index-cache.toml"));
    f.head_reads.store(0, Ordering::SeqCst);
    f.asset_reads.store(0, Ordering::SeqCst);
    f.assets_fail.store(false, Ordering::SeqCst);
    f.publication.borrow_mut().outsider_from = 47;
    assert!(
        resolve().is_err(),
        "the mismatched network signature is refused"
    );
    assert!(f.head_reads.load(Ordering::SeqCst) > 0);
    assert_eq!(
        f.asset_reads.load(Ordering::SeqCst),
        4,
        "corruption turns the local object into a miss"
    );
}

#[test]
fn a_run_found_below_the_floor_keeps_serving_the_cached_floor_index() {
    use crate::sig::testkit;
    let store = crate::testdir::absent_layout("atpkg-net-below-floor-");
    let f = SignedChannel {
        fetcher: GithubFetcher::new("alabsystems".into()).with_index_floor(47),
        publication: std::cell::RefCell::new(Publication {
            run: None,
            newer_roster_from: u64::MAX,
            newer_index_from: u64::MAX,
            outsider_from: u64::MAX,
        }),
        lapsed_through: std::cell::Cell::new(0),
        head_reads: std::sync::atomic::AtomicUsize::new(0),
        asset_reads: std::sync::atomic::AtomicUsize::new(0),
        assets_fail: std::sync::atomic::AtomicBool::new(false),
    };
    let pass = |run: Option<std::ops::RangeInclusive<u64>>,
                roster_from: u64,
                index_from: u64,
                outsider_from| {
        *f.publication.borrow_mut() = Publication {
            run,
            newer_roster_from: roster_from,
            newer_index_from: index_from,
            outsider_from,
        };
        let floor = crate::sig::BuildFloor {
            index_build: 47,
            roster_seq: testkit::SEQ,
        };
        let index = crate::flow::resolve_verified_index(
            &f,
            &store,
            &testkit::anchor(),
            floor,
            testkit::NOW,
        );
        let cached = crate::cache::IndexCache::for_layout(&store)
            .labels(&github_source_id("alabsystems"))
            .unwrap_or_default();
        (
            index.map(|i| i.index_build).map_err(|e| e.to_string()),
            crate::flow::last_resolve().is_some_and(|r| r.reached),
            cached,
        )
    };
    let cached = |builds: &[u64]| labels(builds).unwrap();
    let never = u64::MAX;
    assert_eq!(
        pass(Some(6..=47), never, never, never),
        (Ok(47), true, cached(&[47]))
    );
    assert_eq!(
        crate::sig::Floor::new(store.roster_floor()).current(),
        testkit::SEQ
    );
    let from_cache = (Ok(47), false, cached(&[47]));
    assert_eq!(pass(Some(6..=46), never, never, never), from_cache);
    assert_eq!(pass(None, never, never, never), from_cache);
    let beside_run = (Ok(47), false, cached(&[45, 44, 43, 42, 47]));
    assert_eq!(pass(Some(6..=45), never, never, never), beside_run);
    assert_eq!(pass(None, never, never, never), beside_run);
    assert_eq!(
        pass(Some(6..=47), never, never, never),
        (Ok(47), true, cached(&[47]))
    );
    let beside_refused = (Ok(47), false, cached(&[51, 50, 49, 48, 47]));
    assert_eq!(pass(Some(6..=51), never, never, 48), beside_refused);
    assert_eq!(pass(None, never, never, 48), beside_refused);
    f.lapsed_through.set(47);
    let stale = |builds: &[u64], reached| {
        (
            Err(crate::flow::FlowError::Stale.to_string()),
            reached,
            cached(builds),
        )
    };
    assert_eq!(pass(Some(6..=47), never, never, never), stale(&[47], true));
    assert_eq!(
        pass(Some(6..=51), never, never, 48),
        stale(&[51, 50, 49, 48, 47], false)
    );
    f.lapsed_through.set(0);
    assert_eq!(
        pass(Some(6..=47), never, never, never),
        (Ok(47), true, cached(&[47]))
    );
    let refused = |builds: &[u64]| {
        (
            Err(crate::flow::FlowError::NoIndex.to_string()),
            true,
            cached(builds),
        )
    };
    assert_eq!(pass(Some(6..=48), 48, never, 48), refused(&[48, 47]));
    assert_eq!(
        pass(Some(6..=45), 0, never, never),
        refused(&[45, 44, 43, 42])
    );
    assert_eq!(
        pass(Some(6..=45), 45, 45, never),
        (Ok(45), true, cached(&[45, 44, 43, 42]))
    );
}

#[test]
fn the_rounds_land_what_the_sequential_walk_lands() {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = |bound: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % bound
    };
    for _ in 0..300 {
        let first = 1 + next(40);
        let last = first + next(80);
        let holes: Vec<u64> = (0..next(4))
            .map(|_| first + next(last - first + 1))
            .collect();
        let floor = match next(4) {
            0 => 0,
            1 => first + next(last - first + 1),
            2 => last,
            _ => last + 1 + next(1000),
        };
        let missing = format!(
            "atpkg-index-{}/aterm-machines.toml",
            first + next(last - first + 1)
        );
        let up = move |b: u64| (first..=last).contains(&b) && !holes.contains(&b);
        let newest = match next(5) {
            0 => None,
            1 => Some(first + next(last - first + 1)),
            2 => Some(last),
            3 => Some(first.saturating_sub(next(3))),
            _ => Some(last + 1 + next(50)),
        };
        let low = match next(3) {
            0 => None,
            1 => Some(first.next_power_of_two()),
            _ => Some(1 + next(last + 10)),
        };
        let hint = newest.map(|newest| IndexHint { low, newest });
        walk_with(
            floor,
            hint,
            &move |b| if up(b) { published() } else { absent() },
            &[missing.as_str()],
        );
    }
}

#[test]
fn a_stamped_index_hint_reads_back_or_is_no_hint() {
    let run = IndexHint {
        low: Some(8),
        newest: 72,
    };
    assert_eq!(IndexHint::parse(&run.to_string()), Some(run));
    assert_eq!(
        IndexHint::parse(" 72\n"),
        Some(IndexHint {
            low: None,
            newest: 72
        })
    );
    for refused in [
        "",
        "0",
        "x",
        "9..8",
        "0..8",
        "+8..72",
        "08..72",
        "8..+72",
        "8..",
        "..8",
        "-1..8",
        "8..72..80",
    ] {
        assert_eq!(IndexHint::parse(refused), None, "{refused:?}");
    }
}

#[test]
fn discovery_takes_a_few_rounds_where_the_walk_takes_many_heads() {
    fn hinted_rounds(floor: u64, newest: u64, hint: Option<IndexHint>) -> (usize, usize) {
        let mut known = std::collections::BTreeMap::<u64, HeadVerdict>::new();
        let mut rounds = 0;
        loop {
            let mut need = None;
            let found = newest_index(floor, &mut |b| match known.get(&b) {
                Some(verdict) => verdict.clone().map_err(|(msg, _, _)| msg),
                None => {
                    need = Some(b);
                    Err(String::new())
                }
            });
            let Some(build) = need else {
                assert_eq!(found, Ok((newest, if floor == 0 { 8 } else { floor })));
                return (rounds, known.len());
            };
            rounds += 1;
            for b in ahead_of_walk(floor, build, &known, FETCH_CONCURRENCY, hint) {
                known.insert(b, Ok((6..=newest).contains(&b)));
            }
        }
    }
    let rounds = |floor: u64, newest: u64| hinted_rounds(floor, newest, None);
    assert_eq!(rounds(71, 71), (1, 2), "the steady state");
    assert_eq!(rounds(70, 71).0, 2, "one index behind");
    assert_eq!(
        rounds(0, 71).0,
        3,
        "first contact with the channel as it stands"
    );
    let mut worst = 0;
    for newest in 8..1000_u64 {
        for floor in [0, newest, newest - 1, newest.saturating_sub(5).max(8), 8] {
            worst = worst.max(rounds(floor, newest).0);
        }
    }
    assert!(worst <= 5, "{worst} rounds");
    assert_eq!(
        walk(71, &channel(6, 71), &[]).rounds,
        1,
        "the fetcher, steady"
    );
    assert_eq!(
        walk(0, &channel(6, 71), &[]).rounds,
        3,
        "the fetcher, first contact"
    );
    let seen = |newest| Some(IndexHint { low: None, newest });
    assert_eq!(
        hinted_rounds(0, 71, seen(71)).0,
        2,
        "first contact, cache current"
    );
    assert_eq!(walk_with(0, seen(71), &channel(6, 71), &[]).rounds, 2);
    for (newest, hint) in [(72, 71), (75, 71), (140, 71), (71, 90), (71, 3), (300, 299)] {
        assert!(
            hinted_rounds(0, newest, seen(hint)).0 <= 5,
            "{newest} hinted {hint}"
        );
    }
    let run = |newest| {
        Some(IndexHint {
            low: Some(8),
            newest,
        })
    };
    assert_eq!(
        hinted_rounds(0, 71, run(71)),
        (1, 20),
        "first contact, release-cut run current"
    );
    assert_eq!(walk_with(0, run(71), &channel(6, 71), &[]).rounds, 1);
    for newest in 72..=103 {
        assert_eq!(hinted_rounds(0, newest, run(71)).0, 2, "moved to {newest}");
    }
    for newest in 71..=103 {
        let want = if newest <= 76 { 2 } else { 3 };
        assert_eq!(
            hinted_rounds(0, newest, seen(71)).0,
            want,
            "cache, {newest}"
        );
    }
    assert_eq!(hinted_rounds(70, 71, seen(71)).0, 1, "one behind, woken");
    assert_eq!(walk_with(70, seen(71), &channel(6, 71), &[]).rounds, 1);
    for hint in [71, 70, 3] {
        assert_eq!(
            hinted_rounds(71, 71, seen(hint)),
            (1, 2),
            "floor 71, hint {hint}"
        );
    }
    assert_eq!(
        walk_with(71, seen(71), &channel(6, 71), &[]).headed,
        [72, 73]
    );
}

struct ScriptedHeads<'a> {
    first: &'a (dyn Fn(u64) -> aterm_update_core::FirstHead + Sync),
    rest: &'a (
            dyn Fn(u64) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>
                + Sync
        ),
    firsts: std::sync::Mutex<Vec<u64>>,
    agains: std::sync::Mutex<Vec<u64>>,
    wholes: std::sync::Mutex<Vec<u64>>,
    downs: std::sync::atomic::AtomicU32,
}

impl<'a> ScriptedHeads<'a> {
    fn new(
        first: &'a (dyn Fn(u64) -> aterm_update_core::FirstHead + Sync),
        rest: &'a (
                dyn Fn(u64) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>
                    + Sync
            ),
    ) -> Self {
        Self {
            first,
            rest,
            firsts: std::sync::Mutex::default(),
            agains: std::sync::Mutex::default(),
            wholes: std::sync::Mutex::default(),
            downs: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn sorted(calls: &std::sync::Mutex<Vec<u64>>) -> Vec<u64> {
        let mut calls = calls.lock().unwrap().clone();
        calls.sort_unstable();
        calls
    }
}

impl Heads for ScriptedHeads<'_> {
    fn whole(
        &self,
        url: &str,
    ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
        self.wholes.lock().unwrap().push(walked_build(url));
        (self.rest)(walked_build(url))
    }

    fn first(&self, url: &str) -> aterm_update_core::FirstHead {
        self.firsts.lock().unwrap().push(walked_build(url));
        (self.first)(walked_build(url))
    }

    fn again(
        &self,
        url: &str,
        _cause: aterm_types::http_retry::Cause,
    ) -> Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError> {
        self.agains.lock().unwrap().push(walked_build(url));
        (self.rest)(walked_build(url))
    }

    fn link_down(&self) {
        self.downs
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[test]
fn a_walk_whose_first_round_reaches_no_host_is_offline_at_once() {
    use crate::flow::Fetcher as _;
    use aterm_update_core::{FirstHead, HttpError};
    let refused = || HttpError::Transport("curl: (7) Failed to connect to 127.0.0.1 port 9".into());
    let fetch = |url: &str, _: u64| Ok(url.as_bytes().to_vec());
    let up = channel(6, 71);
    for floor in [0, 71] {
        let f = GithubFetcher::new("alabsystems".into()).with_index_floor(floor);
        let slug = f.index_slug();
        let unreached = |_| FirstHead::Again {
            answer: Err(refused()),
            unreached: true,
            cause: aterm_types::http_retry::Cause::Reset,
        };
        let dead = |_| Err(refused());
        let heads = ScriptedHeads::new(&unreached, &dead);
        let err = f
            .walk_candidates(&slug, &heads, &by_url(&fetch))
            .unwrap_err();
        assert!(err.contains("Failed to connect"), "{err}");
        assert!(f.index_link_down(), "from {floor}");
        assert_eq!(heads.downs.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(!heads.firsts.lock().unwrap().is_empty());
        assert!(
            heads.agains.lock().unwrap().is_empty() && heads.wholes.lock().unwrap().is_empty(),
            "from {floor}: no HEAD retried a link that is not there"
        );
        let lone = if floor == 0 { 8 } else { 72 };
        let one_unreached = |b: u64| {
            if b == lone {
                FirstHead::Again {
                    answer: Err(refused()),
                    unreached: true,
                    cause: aterm_types::http_retry::Cause::Reset,
                }
            } else {
                FirstHead::Settled(up(b))
            }
        };
        let heads = ScriptedHeads::new(&one_unreached, &up);
        let labels = f
            .walk_candidates(&slug, &heads, &by_url(&fetch))
            .map(|c| c.into_iter().map(|c| c.label).collect::<Vec<_>>());
        assert_eq!(labels, walk(floor, &up, &[]).labels, "from {floor}");
        assert_eq!(ScriptedHeads::sorted(&heads.agains), vec![lone]);
        assert_eq!(heads.downs.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert!(!f.index_link_down());
        let timed_out = |_| FirstHead::Again {
            answer: Err(HttpError::Transport(
                "curl: (28) Operation timed out".into(),
            )),
            unreached: false,
            cause: aterm_types::http_retry::Cause::Timeout,
        };
        let heads = ScriptedHeads::new(&timed_out, &up);
        assert!(f.walk_candidates(&slug, &heads, &by_url(&fetch)).is_ok());
        assert_eq!(
            ScriptedHeads::sorted(&heads.agains),
            ScriptedHeads::sorted(&heads.firsts),
            "from {floor}"
        );
        assert_eq!(heads.downs.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}

/// The scratch directory [`probe_a_first_round_over_counted_curl_if_asked`] runs in, set only for
/// the child [`a_first_rounds_curls_are_one_per_head_offline_and_whole_online`] starts.
#[cfg(unix)]
const LINK_PROBE_ENV: &str = "ATPKG_TEST_LINK_PROBE_DIR";
/// That probe's name, as `--exact` wants it.
#[cfg(unix)]
const LINK_PROBE: &str =
    "net::tests::a_first_rounds_curls_are_one_per_head_offline_and_whole_online";

/// The child's half: a first round of three HEADs over the PRODUCTION transport ([`Wire`],
/// [`aterm_update_core::head_no_redirect_first`]), its curl the counting wrapper the
/// parent laid ([`aterm_update_core::CURL_SEAM`]), then one whole HEAD of a fourth build —
/// what the round's link verdict leaves a request after it.
#[cfg(unix)]
fn probe_a_first_round_over_counted_curl_if_asked() -> bool {
    let Some(dir) = std::env::var_os(LINK_PROBE_ENV) else {
        return false;
    };
    let slug = "alabsystems/aterm";
    let round = probe_round(slug, &[1, 2, 4], &Wire);
    let after = aterm_update_core::head_no_redirect(&index_head_url(slug, 8).unwrap());
    let said = |verdict: &HeadVerdict| match verdict {
        Ok(published) => format!("ok {published}"),
        Err((_, link_down, transport_failed)) => {
            format!("err link_down={link_down} transport_failed={transport_failed}")
        }
    };
    let mut out: Vec<String> = round.iter().map(said).collect();
    out.push(match after {
        Ok(answer) => format!("after {}", answer.code),
        Err(aterm_update_core::HttpError::Transport(_)) => "after err transport".into(),
        Err(e) => format!("after err {e:?}"),
    });
    std::fs::write(PathBuf::from(dir).join("verdicts"), out.join("\n")).unwrap();
    true
}

/// PKG-4'S LINK PROBE, COUNTED IN PROCESSES. The first round runs in a child of this test
/// binary ([`probe_a_first_round_over_counted_curl_if_asked`]) whose every curl is a wrapper that
/// logs its URL and answers as the scenario says.
#[cfg(unix)]
#[test]
fn a_first_rounds_curls_are_one_per_head_offline_and_whole_online() {
    if probe_a_first_round_over_counted_curl_if_asked() {
        return;
    }
    use std::os::unix::fs::PermissionsExt as _;
    let run = |scenario: &str| -> (std::collections::BTreeMap<u64, usize>, Vec<String>) {
        let dir = aterm_tempfile::tempdir().unwrap();
        let wrapper = dir.path().join("curl");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nfor a; do url=$a; done\necho \"$url\" >> '{log}'\n\
                 case {scenario}:$url in\n\
                 offline:*|online:*/atpkg-index-4/*) echo 'curl: (7) Failed to connect' >&2; exit 7 ;;\n\
                 esac\nprintf 'HTTP/2 404\\r\\n\\r\\n\\n404 0.042'\n",
                log = dir.path().join("calls").display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args(["--exact", "--nocapture", LINK_PROBE])
            .env(LINK_PROBE_ENV, dir.path())
            .env(aterm_update_core::CURL_SEAM, &wrapper)
            .env(aterm_types::domain::ENV_DEV_FAST_TIMERS, "1")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{scenario}: {}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let mut curls = std::collections::BTreeMap::new();
        for url in std::fs::read_to_string(dir.path().join("calls"))
            .unwrap()
            .lines()
        {
            *curls.entry(walked_build(url)).or_insert(0) += 1;
        }
        let verdicts = std::fs::read_to_string(dir.path().join("verdicts")).unwrap();
        (curls, verdicts.lines().map(str::to_string).collect())
    };
    let (curls, verdicts) = run("offline");
    assert_eq!(
        curls,
        [(1, 1), (2, 1), (4, 1), (8, 1)].into(),
        "offline: one curl per HEAD, then the link is held down"
    );
    assert_eq!(
        verdicts,
        [
            "err link_down=true transport_failed=true",
            "err link_down=true transport_failed=true",
            "err link_down=true transport_failed=true",
            "after err transport",
        ]
    );
    let (curls, verdicts) = run("online");
    assert_eq!(
        curls,
        [(1, 1), (2, 1), (4, 1), (8, 1)].into(),
        "online: connection-refused curl exit 7 is final under the narrow retry policy"
    );
    assert_eq!(
        verdicts,
        [
            "ok false",
            "ok false",
            "err link_down=true transport_failed=true",
            "after 404",
        ]
    );
}

#[test]
fn a_manifest_cache_miss_fetches_both_halves_in_one_wave() {
    let f = GithubFetcher::new("alabsystems".into());
    let arrived = (std::sync::Mutex::new(0usize), std::sync::Condvar::new());
    let wire = |url: &str, cap: u64| -> Result<Vec<u8>, String> {
        assert_eq!(
            cap,
            if url.ends_with(".sig") {
                SIG_CAP
            } else {
                MANIFEST_CAP
            }
        );
        let (count, ready) = &arrived;
        let mut n = count.lock().unwrap();
        *n += 1;
        ready.notify_all();
        let (_arrived, timeout) = ready
            .wait_timeout_while(n, aterm_spec::wait::MUST_HAPPEN, |n| *n < 2)
            .unwrap();
        if timeout.timed_out() {
            Err("the other request never started".into())
        } else {
            Ok(url.as_bytes().to_vec())
        }
    };
    let pair = f.manifest_pair("ay", "ay", 8256, &by_url(&wire)).unwrap();
    let (toml, sig) = manifest_assets("alabsystems/ay", "ay", 8256).unwrap();
    assert_eq!(
        pair,
        (toml.web_url().into_bytes(), sig.web_url().into_bytes())
    );
    let hit = f
        .manifest_pair("ay", "ay", 8256, &|_: &Named, _| {
            panic!("a complete pair must be memoized")
        })
        .unwrap();
    assert_eq!(hit, pair);
    assert_eq!(*arrived.0.lock().unwrap(), 2);
}

#[test]
fn a_failed_half_names_its_release_and_asset_and_an_invalid_identity_never_fetches() {
    let mut overrides = std::collections::BTreeMap::new();
    overrides.insert("cpython".into(), "alabsystems/aterm".into());
    let f = GithubFetcher::new("fixture".into()).with_overrides(overrides);
    let web = |url: &str, _: u64| {
        if url.ends_with(".sig") {
            Ok(b"sig".to_vec())
        } else {
            Err("HTTP 504".to_string())
        }
    };
    let error = f
        .manifest_pair("ignored", "cpython", 1, &by_url(&web))
        .unwrap_err();
    assert!(error.contains("HTTP 504"), "{error}");
    assert!(
        error.contains("alabsystems/aterm tag atpkg-cpython-1 asset pkg-cpython-1.toml"),
        "{error}"
    );
    assert!(f.manifests.lock().unwrap().is_empty());
    let no_web = |_: &Named, _: u64| panic!("invalid identity must not fetch");
    assert!(f.manifest_pair("aterm", "../cpython", 1, &no_web).is_err());
}

#[test]
fn the_manifest_prefetch_fills_the_memo_with_bytes_not_trust() {
    use crate::sig::testkit;
    let f = GithubFetcher::new("alabsystems".into());
    let pins: Vec<(String, String, u64)> = [("ay", 8256), ("ty", 3007), ("nn", 108)]
        .iter()
        .map(|(p, b)| ((*p).to_string(), (*p).to_string(), *b))
        .collect();
    let asked = std::sync::Mutex::new(Vec::<(String, u64)>::new());
    let wire = |url: &str, cap: u64| -> Result<Vec<u8>, String> {
        asked.lock().unwrap().push((url.to_string(), cap));
        match url.strip_suffix(".sig") {
            Some(_) if url.contains("/pkg-nn-") => Err(format!("HTTP 503 for {url}")),
            Some(toml) => {
                let mut sig = testkit::sign(&testkit::MACHINE_SEED, toml.as_bytes());
                if url.contains("/pkg-ty-") {
                    sig[0] ^= 1;
                }
                Ok(sig)
            }
            None => Ok(url.as_bytes().to_vec()),
        }
    };
    f.prefetch_with(&pins, &by_url(&wire));
    let mut first: Vec<(String, u64)> = std::mem::take(&mut *asked.lock().unwrap());
    first.sort();
    let mut expected: Vec<(String, u64)> = pins
        .iter()
        .flat_map(|(repo, program, build)| {
            let (toml, sig) =
                manifest_assets(&format!("alabsystems/{repo}"), program, *build).unwrap();
            [(toml.web_url(), MANIFEST_CAP), (sig.web_url(), SIG_CAP)]
        })
        .collect();
    expected.sort();
    assert_eq!(
        first, expected,
        "both halves of every pair, each under its cap"
    );

    let index = testkit::trusted_index_at(
        testkit::SEQ,
        format!(
            "schema = 2\nindex_build = 47\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
             machine_id = \"{}\"\nroster_seq = {}\n[programs.ay]\nrepo = \"ay\"\n",
            testkit::MACHINE_ID,
            testkit::SEQ,
        )
        .into_bytes(),
    );
    let (raw, sig) = f.manifest_pair("ay", "ay", 8256, &by_url(&wire)).unwrap();
    assert!(
        index.verify_pkg(raw, &sig).is_ok(),
        "the genuine pair verifies"
    );
    let (raw, sig) = f.manifest_pair("ty", "ty", 3007, &by_url(&wire)).unwrap();
    assert!(
        index.verify_pkg(raw, &sig).is_err(),
        "a tampered signature the prefetch carried is refused"
    );
    assert!(asked.lock().unwrap().is_empty(), "both read from the memo");
    let err = f
        .manifest_pair("nn", "nn", 108, &by_url(&wire))
        .unwrap_err();
    assert!(err.contains("HTTP 503"), "{err}");
    assert_eq!(asked.lock().unwrap().len(), 2, "fetched again on the read");
    asked.lock().unwrap().clear();
    f.prefetch_with(&pins[..2], &by_url(&wire));
    assert!(asked.lock().unwrap().is_empty());
}

#[test]
fn the_manifest_prefetch_asks_every_pair_in_one_wave() {
    let f = GithubFetcher::new("alabsystems".into());
    let pins: Vec<(String, String, u64)> = (0..=FETCH_CONCURRENCY / 2)
        .map(|i| (format!("p{i}"), format!("p{i}"), 1))
        .collect();
    let want = 2 * pins.len();
    let arrived = (std::sync::Mutex::new(0usize), std::sync::Condvar::new());
    let wire = |url: &str, _cap: u64| -> Result<Vec<u8>, String> {
        let (count, all_in) = &arrived;
        let mut n = count.lock().unwrap();
        *n += 1;
        all_in.notify_all();
        let timed_out = all_in
            .wait_timeout_while(n, aterm_spec::wait::MUST_HAPPEN, |n| *n < want)
            .unwrap()
            .1
            .timed_out();
        if timed_out {
            Err(format!(
                "{url}: answered before all {want} requests were out"
            ))
        } else {
            Ok(url.as_bytes().to_vec())
        }
    };
    f.prefetch_with(&pins, &by_url(&wire));
    assert_eq!(
        f.manifests.lock().unwrap().len(),
        pins.len(),
        "every pair of {want} requests in flight at once"
    );
}

#[test]
fn the_walk_stops_at_its_head_budget() {
    let w = walk(44, &|_| published(), &[]);
    assert!(w.labels.is_err());
    assert_eq!(w.heads.len(), usize::try_from(WALK_HEAD_BUDGET).unwrap());
    assert!(w.fetched.is_empty(), "no asset was fetched");
}

#[test]
fn the_index_quad_is_the_four_derived_download_urls() {
    let q = index_quad("alabsystems/aterm", "atpkg-index-41").unwrap();
    let base = "https://github.com/alabsystems/aterm/releases/download/atpkg-index-41/";
    assert_eq!(q.label, "atpkg-index-41");
    assert_eq!(q.index.web_url(), format!("{base}index.toml"));
    assert_eq!(q.index_sig.web_url(), format!("{base}index.toml.sig"));
    assert_eq!(q.roster.web_url(), format!("{base}aterm-machines.toml"));
    assert_eq!(
        q.roster_sig.web_url(),
        format!("{base}aterm-machines.toml.sig")
    );
    assert!(index_quad("a/b/c", "atpkg-index-41").is_none());
    assert!(index_quad("alabsystems/aterm", "atpkg-index-4/1").is_none());
    const { assert!(INDEX_CANDIDATE_CAP < 24) };
}
