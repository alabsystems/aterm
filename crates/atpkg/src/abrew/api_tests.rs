// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn ghcr_repo_with_query_delimiters_is_refused() {
    assert!(ghcr_authorization("homebrew/core/wget&scope=admin").is_err());
    assert!(ghcr_authorization("").is_err());
    assert_eq!(percent_encode("repository:a/b:pull"), "repository:a/b:pull");
    assert_eq!(percent_encode("a&b"), "a%26b");
}

#[test]
#[cfg(unix)]
fn formula_metadata_must_name_the_request() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure_api_cache().unwrap();
    let api = Api::new(prefix.clone());
    write_metadata(
        &api.cache_path("formula-demo.json"),
        br#"{"name":"other","versions":{"stable":"1"}}"#,
    )
    .unwrap();
    assert!(api.formula("demo").is_err());
    write_metadata(
        &api.cache_path("formula-demo.json"),
        br#"{"name":"real","aliases":["demo"],"versions":{"stable":"1"}}"#,
    )
    .unwrap();
    assert_eq!(api.formula("demo").unwrap().name, "real");
    write_metadata(
        &api.cache_path("cask-caffeine.json"),
        br#"{"token":"caffeine","name":["Caffeine"],"version":"1.1.4","url":"https://example.com/Caffeine.dmg","sha256":"1ad34c3299a0c866873c4feb432c3e11186375adb6f43c5f481ae9d46d06d723","artifacts":[{"app":["Caffeine.app"]}]}"#,
    )
    .unwrap();
    assert_eq!(api.cask("caffiene").unwrap().token, "caffeine");
}

#[test]
fn dep_tag_filter_matches_brew_predicates() {
    // abook→gettext is in both runtime and build lists; brew's
    // JSON `dependencies` list is exactly `required?`, so the
    // edge passes unconditionally. abseil→cmake is build+test:
    // needs either include. Oracle-verified shapes.
    let abook = DepRow {
        name: "abook".to_string(),
        deps: vec!["readline".to_string(), "gettext".to_string()],
        build_deps: vec!["autoconf".to_string(), "gettext".to_string()],
        ..DepRow::default()
    };
    let plain = DepFilter::default();
    assert!(abook.edge_passes("gettext", &plain));
    let abseil = DepRow {
        name: "abseil".to_string(),
        build_deps: vec!["cmake".to_string()],
        test_deps: vec!["cmake".to_string()],
        ..DepRow::default()
    };
    assert!(!abseil.edge_passes("cmake", &plain));
    assert!(abseil.edge_passes(
        "cmake",
        &DepFilter {
            include_test: true,
            ..DepFilter::default()
        }
    ));
    assert!(abseil.edge_passes(
        "cmake",
        &DepFilter {
            include_build: true,
            ..DepFilter::default()
        }
    ));
    let reco = DepRow {
        name: "x".to_string(),
        recommended_deps: vec!["y".to_string()],
        ..DepRow::default()
    };
    assert!(reco.edge_passes("y", &plain));
    assert!(!reco.edge_passes(
        "y",
        &DepFilter {
            skip_recommended: true,
            ..DepFilter::default()
        }
    ));
    assert!(!abook.edge_passes("nope", &plain));
}

#[test]
#[cfg(unix)]
fn cached_documents_use_real_bounded_metadata_and_preserve_link_targets() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure_api_cache().unwrap();
    let api = Api::new(prefix.clone());
    let path = api.cache_path("formula-demo.json");
    let owner = tmp.path().join("owner-note");
    std::fs::write(&owner, b"keep").unwrap();
    std::os::unix::fs::symlink(&owner, &path).unwrap();
    let document = br#"{"name":"demo","versions":{"stable":"1"}}"#;
    write_metadata(&path, document).unwrap();
    assert!(!path.is_symlink());
    assert_eq!(std::fs::read(&owner).unwrap(), b"keep");
    assert_eq!(api.formula("demo").unwrap().version, "1");
    assert!(!prefix.cellar().exists());

    let outside = tmp.path().join("outside");
    std::fs::rename(prefix.cache().join("api"), &outside).unwrap();
    std::os::unix::fs::symlink(&outside, prefix.cache().join("api")).unwrap();
    assert!(
        api.formula("demo")
            .unwrap_err()
            .contains("not a real directory")
    );
    api.refresh();
    assert_eq!(
        std::fs::read(outside.join("formula-demo.json")).unwrap(),
        document
    );
    assert_eq!(std::fs::read(&owner).unwrap(), b"keep");
}

#[test]
fn ghcr_repo_parse() {
    assert_eq!(
        ghcr_repo_of("https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:abc").as_deref(),
        Some("homebrew/core/wget")
    );
    assert_eq!(ghcr_repo_of("https://example.com/x.tar.gz"), None);
    assert!(ghcr_authorization("homebrew/../evil").is_err());
    assert!(ghcr_authorization("homebrew//wget").is_err());
    let tmp = aterm_tempfile::tempdir().unwrap();
    let api = Api::new(Prefix::at(tmp.path()));
    assert_eq!(
        api.formula("bad?query").unwrap_err(),
        "abrew: no such formula: bad?query"
    );
    assert_eq!(
        api.cask("bad?query").unwrap_err(),
        "abrew: no such cask: bad?query"
    );
}

#[test]
fn options_record_sorts_flags_and_sees_head() {
    let v: Value = aterm_json::from_slice(
        br#"{"options":[{"option":"with-z","description":"Zed"},{"option":"with-a","description":"Ay"},{"option":"","description":"skip"}],"urls":{"head":{"url":"https://example.com/x.git"}}}"#,
    )
    .unwrap();
    let (options, head) = parse_options_record(&v);
    assert_eq!(
        options,
        [
            ("with-a".to_string(), "Ay".to_string()),
            ("with-z".to_string(), "Zed".to_string())
        ]
    );
    assert!(head);
    let bare: Value = aterm_json::from_slice(br#"{"options":[],"urls":{"stable":{}}}"#).unwrap();
    assert_eq!(parse_options_record(&bare), (vec![], false));
}

#[test]
fn registry_header_keeps_the_requested_pull_scope_and_token_refusal() {
    let header = ghcr_authorization_with("homebrew/core/demo", |repo| {
        assert_eq!(repo, "homebrew/core/demo");
        Ok("fixture-pull-token".into())
    })
    .unwrap();
    assert_eq!(header, "Authorization: Bearer fixture-pull-token");
    assert_eq!(
        ghcr_authorization_with("homebrew/core/demo", |_| Err("registry unavailable".into())),
        Err("registry unavailable".into())
    );
}
