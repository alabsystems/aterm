// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The evergreen pointer: learning which release is newest from one unmetered HEAD.
//!
//! GitHub answers `https://github.com/{owner}/{repo}/releases/latest/download/{name}` with a
//! 302 to `…/releases/download/<tag>/{name}` for the newest published release (drafts and
//! prereleases excluded by the server), with no `x-ratelimit-*` header; a private repo answers
//! 404. So the channel head costs one request against no budget, and a check whose head has
//! not moved needs no second one.
//!
//! The pointer is not trust: it only chooses which tag later GETs address, and every byte they
//! return still meets the appcast signature, the roster, the version/tag/URL binds and the
//! build and floor gates. The tag is taken only from a `Location` that is exactly the
//! tag-specific URL for the caller's owner, repo and name, with a tag that passes the caller's
//! predicate and [`crate::cdn::path_segment_safe`]; any other `Location` is refused, never
//! interpreted. Nothing after the HEAD goes through `latest` again, so a pointer that moves
//! mid-check cannot mix two releases.
//!
//! `latest` need not carry an app build: the shared `vX.Y.0` release can be published before
//! the app cut attaches its assets, so the check lane treats a 404 on the head's appcast as
//! "channel head has no app manifest yet", a healthy end to the check.

use crate::cdn::path_segment_safe;
use crate::http::{HeadAnswer, HttpError};

/// The evergreen URL for asset `name` of the newest published release in `owner/repo`, or
/// `None` when any of the three is not a safe path segment.
#[must_use]
pub fn latest_download_url(owner: &str, repo: &str, name: &str) -> Option<String> {
    if !(path_segment_safe(owner) && path_segment_safe(repo) && path_segment_safe(name)) {
        return None;
    }
    // Manual concat, as in `cdn::release_download_url`.
    let mut url = String::with_capacity(
        "https://github.com/".len()
            + owner.len()
            + repo.len()
            + "/releases/latest/download/".len()
            + name.len()
            + 2,
    );
    url.push_str("https://github.com/");
    url.push_str(owner);
    url.push('/');
    url.push_str(repo);
    url.push_str("/releases/latest/download/");
    url.push_str(name);
    Some(url)
}

/// Why the pointer did not yield a tag, in the classes a check lane treats differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerError {
    /// `owner`, `repo` or `name` is not a safe path segment: no URL was built and no
    /// request was made.
    UnsafeName,
    /// 404: no published (non-draft, non-prerelease) release, or a private or nonexistent
    /// repository, which the web host renders identically. A standing state, not weather.
    NoRelease { url: String },
    /// The pointer names this repository and asset under a safe tag this client does not
    /// install from (an `atpkg-index-<n>` cut published as a normal release captures
    /// `/releases/latest`). The channel is intact but its head is not an app release; the
    /// caller records that and ends the check. Nothing at the location was fetched.
    OtherTag { tag: String },
    /// 429 or 5xx: weather, retried on the next cycle.
    Transient { code: u16, url: String },
    /// curl itself failed (DNS, TLS, timeout) or the answer was unparseable.
    Transport(String),
    /// A status the pointer has no reading for (a 200 with no redirect, a 403 from a filtering
    /// proxy, …): a verdict about this host.
    Unexpected { code: u16, url: String },
    /// The redirect's `Location` is not exactly the tag-specific download URL of `name` in this
    /// repository. The value is not echoed (it is server-controlled); `why` is a fixed literal.
    Refused { why: &'static str },
}

impl std::fmt::Display for PointerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsafeName => f.write_str(
                "the release source is not URL-safe; no evergreen pointer URL was built",
            ),
            Self::NoRelease { url } => write!(
                f,
                "HTTP 404 for {url}: the channel has no published release (or is private or \
                 does not exist — GitHub answers identically for all three on this host)"
            ),
            Self::Transient { code, url } => write!(
                f,
                "the release host answered HTTP {code} to HEAD {url}; transient — backing off, \
                 will retry on the next check"
            ),
            Self::Transport(message) => f.write_str(message),
            Self::Unexpected { code, url } => write!(
                f,
                "the release host answered HTTP {code} to HEAD {url}, which is not a release \
                 pointer (a blocked host or proxy?)"
            ),
            Self::OtherTag { tag } => write!(
                f,
                "the evergreen release pointer names {tag}, a release of this channel that is \
                 not an app release"
            ),
            Self::Refused { why } => write!(
                f,
                "the evergreen release pointer redirected somewhere this client refuses to \
                 follow ({why})"
            ),
        }
    }
}

/// The parsed pointer: the newest published release's tag, and the tag-specific URL the
/// `Location` named (proved equal to the derived one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    pub tag: String,
    pub location: String,
}

/// Parse a 302's `Location` into the tag it names, strictly: it must be byte for byte
/// `https://github.com/{owner}/{repo}/releases/download/<tag>/{name}`, with `<tag>` one
/// [`path_segment_safe`] segment. A tag `accept_tag` refuses is [`PointerError::OtherTag`];
/// anything else is [`PointerError::Refused`]. The URL rebuilt by
/// [`crate::cdn::release_download_url`] must equal the `Location`, so the caller always fetches
/// the derived URL, never a server string with a hidden difference.
pub fn parse_location(
    owner: &str,
    repo: &str,
    name: &str,
    location: &str,
    accept_tag: &dyn Fn(&str) -> bool,
) -> Result<Pointer, PointerError> {
    let refused = |why: &'static str| PointerError::Refused { why };
    let mut prefix = String::from("https://github.com/");
    prefix.push_str(owner);
    prefix.push('/');
    prefix.push_str(repo);
    prefix.push_str("/releases/download/");
    let rest = location
        .strip_prefix(prefix.as_str())
        .ok_or_else(|| refused("not a release download URL of this repository"))?;
    let mut suffix = String::from("/");
    suffix.push_str(name);
    let tag = rest
        .strip_suffix(suffix.as_str())
        .ok_or_else(|| refused("the redirect does not name the asset that was asked for"))?;
    if !path_segment_safe(tag) {
        return Err(refused(
            "the redirect's tag segment is not a safe path segment",
        ));
    }
    if !accept_tag(tag) {
        // This repository, this asset, a safe segment: the one benign refusal.
        return Err(PointerError::OtherTag {
            tag: tag.to_string(),
        });
    }
    // Every later GET uses the derived URL, so it must be the very string the server named.
    let derived = crate::cdn::release_download_url(owner, repo, tag, name)
        .ok_or_else(|| refused("the release source is not URL-safe"))?;
    if derived != location {
        return Err(refused(
            "the redirect differs from the derived download URL",
        ));
    }
    Ok(Pointer {
        tag: tag.to_string(),
        location: derived,
    })
}

/// Classify a redirect-refusing HEAD's answer for `url` (from [`latest_download_url`]) into
/// the tag it names, under `accept_tag`. Pure: the caller supplies the transport.
pub fn classify(
    owner: &str,
    repo: &str,
    name: &str,
    url: &str,
    answer: Result<HeadAnswer, HttpError>,
    accept_tag: &dyn Fn(&str) -> bool,
) -> Result<Pointer, PointerError> {
    let answer = answer.map_err(|e| PointerError::Transport(e.to_string()))?;
    match answer.code {
        301 | 302 | 303 | 307 | 308 => {
            let location = answer.location.ok_or(PointerError::Refused {
                why: "the redirect carried no Location header",
            })?;
            parse_location(owner, repo, name, &location, accept_tag)
        }
        404 => Err(PointerError::NoRelease {
            url: url.to_string(),
        }),
        code @ (429 | 500..=599) => Err(PointerError::Transient {
            code,
            url: url.to_string(),
        }),
        code => Err(PointerError::Unexpected {
            code,
            url: url.to_string(),
        }),
    }
}

/// [`resolve`] over an injected HEAD transport, which tests count requests through.
pub fn resolve_with(
    owner: &str,
    repo: &str,
    name: &str,
    accept_tag: &dyn Fn(&str) -> bool,
    head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>,
) -> Result<Pointer, PointerError> {
    let url = latest_download_url(owner, repo, name).ok_or(PointerError::UnsafeName)?;
    let answer = head(&url);
    classify(owner, repo, name, &url, answer, accept_tag)
}

/// The newest published release's tag in `owner/repo`, from one anonymous unmetered HEAD of
/// the evergreen URL for `name`.
pub fn resolve(
    owner: &str,
    repo: &str,
    name: &str,
    accept_tag: &dyn Fn(&str) -> bool,
) -> Result<Pointer, PointerError> {
    resolve_with(
        owner,
        repo,
        name,
        accept_tag,
        &mut crate::http::head_no_redirect,
    )
}

/// The app channel's tag grammar for the pointer: a canonical `vMAJOR.MINOR.PATCH` candidate
/// ([`crate::tag::parse_release_tag`] + [`crate::tag::canonical_version`]).
#[must_use]
pub fn canonical_app_tag(tag: &str) -> bool {
    match crate::tag::parse_release_tag(tag) {
        Ok(crate::tag::TagKind::Candidate(numeric)) => {
            crate::tag::canonical_version(tag, &numeric).is_some()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "alabsystems";
    const REPO: &str = "aterm";
    const NAME: &str = "aterm-appcast.toml";

    fn measured() -> String {
        // A real `location:` from the shipped channel.
        "https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-appcast.toml".into()
    }

    #[test]
    fn the_evergreen_url_matches_githubs_shape_and_refuses_unsafe_segments() {
        assert_eq!(
            latest_download_url(OWNER, REPO, NAME).as_deref(),
            Some(
                "https://github.com/alabsystems/aterm/releases/latest/download/aterm-appcast.toml"
            )
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

    /// GitHub's answer parses to its tag, and the location handed back is the derived URL.
    #[test]
    fn the_measured_redirect_names_the_tag() {
        let p = parse_location(OWNER, REPO, NAME, &measured(), &canonical_app_tag).unwrap();
        assert_eq!(p.tag, "v0.74.0");
        assert_eq!(
            Some(p.location.as_str()),
            crate::cdn::release_download_url(OWNER, REPO, "v0.74.0", NAME).as_deref()
        );
    }

    /// The pointer cannot choose a tag outside the grammar or point later GETs off this
    /// repository.
    #[test]
    fn every_other_location_is_refused_not_interpreted() {
        let refused = |loc: &str| match parse_location(OWNER, REPO, NAME, loc, &canonical_app_tag) {
            Err(PointerError::Refused { .. }) => {}
            other => panic!("{loc}: expected a refusal, got {other:?}"),
        };
        // Another host, another repo, another asset.
        refused(
            "https://evil.example/alabsystems/aterm/releases/download/v0.74.0/aterm-appcast.toml",
        );
        refused("https://github.com/someone/aterm/releases/download/v0.74.0/aterm-appcast.toml");
        refused(
            "https://github.com/alabsystems/other/releases/download/v0.74.0/aterm-appcast.toml",
        );
        refused(
            "https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-machines.toml",
        );
        // http, a query, a fragment, an escape, an extra segment, a traversal.
        refused("http://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-appcast.toml");
        refused(
            "https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-appcast.toml?x=1",
        );
        refused(
            "https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-appcast.toml#f",
        );
        refused(
            "https://github.com/alabsystems/aterm/releases/download/v0%2e74.0/aterm-appcast.toml",
        );
        refused(
            "https://github.com/alabsystems/aterm/releases/download/v0.74.0/x/aterm-appcast.toml",
        );
        refused("https://github.com/alabsystems/aterm/releases/download/../aterm-appcast.toml");
        // The `latest` alias itself, and the tag page: neither is a download URL.
        refused("https://github.com/alabsystems/aterm/releases/latest/download/aterm-appcast.toml");
        refused("https://github.com/alabsystems/aterm/releases/tag/v0.74.0");
        // An empty tag never reaches the grammar (it is not a safe segment).
        refused("https://github.com/alabsystems/aterm/releases/download//aterm-appcast.toml");
        // A tag outside the grammar on this repository, for this asset, is the one benign
        // case: `OtherTag`, named and never fetched from.
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
        // …but a caller with a different grammar (atpkg) accepts its own tags.
        let p = parse_location(
            "alabsystems",
            "atpkg-index",
            "index.toml",
            "https://github.com/alabsystems/atpkg-index/releases/download/atpkg-index-41/index.toml",
            &|tag| tag.starts_with("atpkg-index-"),
        )
        .unwrap();
        assert_eq!(p.tag, "atpkg-index-41");
    }

    /// A redirect is the answer, 404 the standing state, 429/5xx weather, anything else a host
    /// verdict, and a redirect without a Location is refused.
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
        // The standing state's wording names its causes.
        let text = PointerError::NoRelease { url: url.clone() }.to_string();
        assert!(
            text.contains("no published release") && text.contains("private"),
            "{text}"
        );
        // And no refusal ever echoes the server's string.
        let text = PointerError::Refused {
            why: "the redirect differs from the derived download URL",
        }
        .to_string();
        assert!(!text.contains("github.com"), "{text}");
    }

    /// One HEAD, to the evergreen URL; none for an unsafe source.
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
        assert_eq!(
            asked,
            vec![latest_download_url(OWNER, REPO, NAME).unwrap()],
            "one HEAD, to the evergreen URL"
        );
        assert!(!crate::cdn::is_api_host(&asked[0]));
        let mut none = |_: &str| panic!("no request may be made for an unsafe source");
        assert_eq!(
            resolve_with("a/b", REPO, NAME, &canonical_app_tag, &mut none),
            Err(PointerError::UnsafeName)
        );
    }

    #[test]
    fn the_app_grammar_is_the_shared_tag_rule() {
        assert!(canonical_app_tag("v0.74.0"));
        assert!(canonical_app_tag("v1.2.3"));
        for bad in [
            "v0.74",
            "v01.74.0",
            "v0.74.0-rc1",
            "V0.74.0",
            "0.74.0",
            "atpkg-ty-2973",
        ] {
            assert!(!canonical_app_tag(bad), "{bad}");
        }
    }
}
