// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const OWNER: &str = "alabsystems";
const REPO: &str = "aterm";
const NAME: &str = "aterm-appcast.toml";

fn measured() -> String {
    "https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-appcast.toml".into()
}

#[test]
fn the_evergreen_url_matches_githubs_shape_and_refuses_unsafe_segments() {
    assert_eq!(
        latest_download_url(OWNER, REPO, NAME).as_deref(),
        Some("https://github.com/alabsystems/aterm/releases/latest/download/aterm-appcast.toml")
    );
    for bad in ["", ".", "..", "a/b", "a?b", "a#b", "a%2fb", "a b"] {
        assert!(
            latest_download_url(bad, REPO, NAME).is_none(),
            "owner {bad:?}"
        );
        assert!(
            latest_download_url(OWNER, bad, NAME).is_none(),
            "repo {bad:?}"
        );
        assert!(
            latest_download_url(OWNER, REPO, bad).is_none(),
            "name {bad:?}"
        );
    }
}

#[test]
fn the_measured_redirect_names_the_tag() {
    let p = parse_location(OWNER, REPO, NAME, &measured(), &canonical_app_tag).unwrap();
    assert_eq!(p.tag, "v0.74.0");
    assert_eq!(
        Some(p.location.as_str()),
        crate::cdn::release_download_url(OWNER, REPO, "v0.74.0", NAME).as_deref()
    );
}

#[test]
fn every_other_location_is_refused_not_interpreted() {
    let refused = |loc: &str| match parse_location(OWNER, REPO, NAME, loc, &canonical_app_tag) {
        Err(PointerError::Refused { .. }) => {}
        other => panic!("{loc}: expected a refusal, got {other:?}"),
    };
    refused("https://evil.example/alabsystems/aterm/releases/download/v0.74.0/aterm-appcast.toml");
    refused("https://github.com/someone/aterm/releases/download/v0.74.0/aterm-appcast.toml");
    refused("https://github.com/alabsystems/other/releases/download/v0.74.0/aterm-appcast.toml");
    refused("https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-machines.toml");
    refused("http://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-appcast.toml");
    refused(
        "https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-appcast.toml?x=1",
    );
    refused("https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-appcast.toml#f");
    refused("https://github.com/alabsystems/aterm/releases/download/v0%2e74.0/aterm-appcast.toml");
    refused("https://github.com/alabsystems/aterm/releases/download/v0.74.0/x/aterm-appcast.toml");
    refused("https://github.com/alabsystems/aterm/releases/download/../aterm-appcast.toml");
    refused("https://github.com/alabsystems/aterm/releases/latest/download/aterm-appcast.toml");
    refused("https://github.com/alabsystems/aterm/releases/tag/v0.74.0");
    refused("https://github.com/alabsystems/aterm/releases/download//aterm-appcast.toml");
    for tag in [
        "v0.74",
        "v01.74.0",
        "v0.74.0-rc1",
        "atpkg-index-41",
        "0.74.0",
    ] {
        let mut loc = String::from("https://github.com/alabsystems/aterm/releases/download/");
        loc.push_str(tag);
        loc.push('/');
        loc.push_str(NAME);
        match parse_location(OWNER, REPO, NAME, &loc, &canonical_app_tag) {
            Err(PointerError::OtherTag { tag: named }) => assert_eq!(named, tag),
            other => panic!("{loc}: expected OtherTag, got {other:?}"),
        }
    }
    let p = parse_location(
        "alabsystems",
        "atpkg-index",
        "index.toml",
        "https://github.com/alabsystems/atpkg-index/releases/download/atpkg-index-41/index.toml",
        &|tag| tag.starts_with("atpkg-index-"),
    )
    .unwrap();
    assert_eq!(p.tag, "atpkg-index-41");
    assert!(matches!(
        parse_location(
            "bad/owner",
            REPO,
            NAME,
            "https://github.com/bad/owner/aterm/releases/download/not-a-tag/aterm-appcast.toml",
            &canonical_app_tag,
        ),
        Err(PointerError::Refused { .. })
    ));
}

#[test]
fn head_answers_are_classified_into_exactly_the_check_lanes_classes() {
    let url = latest_download_url(OWNER, REPO, NAME).unwrap();
    let class = |answer: Result<HeadAnswer, HttpError>| {
        classify(OWNER, REPO, NAME, &url, answer, &canonical_app_tag)
    };
    let ok = |code| {
        Ok(HeadAnswer {
            code,
            location: Some(measured()),
        })
    };
    assert_eq!(class(ok(302)).unwrap().tag, "v0.74.0");
    assert_eq!(class(ok(301)).unwrap().tag, "v0.74.0");
    assert!(matches!(
        class(Ok(HeadAnswer {
            code: 302,
            location: None
        })),
        Err(PointerError::Refused { .. })
    ));
    assert!(matches!(
        class(Ok(HeadAnswer {
            code: 404,
            location: None
        })),
        Err(PointerError::NoRelease { .. })
    ));
    for code in [429, 500, 502, 503, 504] {
        assert!(
            matches!(
                class(Ok(HeadAnswer {
                    code,
                    location: None
                })),
                Err(PointerError::Transient { .. })
            ),
            "{code}"
        );
    }
    for code in [200, 403, 401] {
        assert!(
            matches!(
                class(Ok(HeadAnswer {
                    code,
                    location: Some(measured())
                })),
                Err(PointerError::Unexpected { .. })
            ),
            "{code}: a non-redirect is never read as a pointer, Location or not"
        );
    }
    assert!(matches!(
        class(Err(HttpError::Transport("curl HEAD x failed".into()))),
        Err(PointerError::Transport(_))
    ));
}

#[test]
fn each_lookup_escapes_an_intermediarys_cached_release_pointer() {
    let mut cached = std::collections::HashMap::new();
    let mut published = "v0.74.0";
    let mut lookup = |published: &str| {
        resolve_with(OWNER, REPO, NAME, &canonical_app_tag, &mut |url| {
            let tag = cached
                .entry(url.to_owned())
                .or_insert_with(|| published.to_owned());
            Ok(HeadAnswer {
                code: 302,
                location: Some(crate::cdn::release_download_url(OWNER, REPO, tag, NAME).unwrap()),
            })
        })
        .unwrap()
    };
    assert_eq!(lookup(published).tag, published);
    published = "v0.75.0";
    assert_eq!(lookup(published).tag, published);
}

#[test]
fn resolve_makes_one_head_and_none_for_an_unsafe_source() {
    let mut asked: Vec<String> = Vec::new();
    let mut head = |url: &str| {
        asked.push(url.to_string());
        Ok(HeadAnswer {
            code: 302,
            location: Some(measured()),
        })
    };
    let p = resolve_with(OWNER, REPO, NAME, &canonical_app_tag, &mut head).unwrap();
    assert_eq!(p.tag, "v0.74.0");
    assert_eq!(asked.len(), 1, "one HEAD");
    let (base, nonce) = asked[0].split_once("?aterm_lookup=").unwrap();
    assert_eq!(base, latest_download_url(OWNER, REPO, NAME).unwrap());
    assert_eq!(nonce.len(), 32);
    assert!(nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(!crate::cdn::is_api_host(&asked[0]));
    let mut none = |_: &str| panic!("no request may be made for an unsafe source");
    assert_eq!(
        resolve_with("a/b", REPO, NAME, &canonical_app_tag, &mut none),
        Err(PointerError::UnsafeName)
    );
}
