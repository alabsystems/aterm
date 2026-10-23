// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resolves the newest published release tag via one unmetered HEAD request to
//! `/releases/latest/download/{name}`.
//!
//! Only a redirect to the exact tag-specific download URL for `(owner, repo, name)` under a
//! [`crate::cdn::path_segment_safe`] tag accepted by the caller is trusted to name the tag;
//! all subsequent GETs use that derived tag URL directly.

use crate::cdn::path_segment_safe;
use crate::http::{HeadAnswer, HttpError};

/// Returns the `/releases/latest/download/{name}` URL for `owner/repo`, or `None` if any segment is unsafe.
#[must_use]
pub fn latest_download_url(owner: &str, repo: &str, name: &str) -> Option<String> {
    if !(path_segment_safe(owner) && path_segment_safe(repo) && path_segment_safe(name)) {
        return None;
    }
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

/// Reason why the evergreen pointer did not yield a release tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerError {
    /// `owner`, `repo` or `name` is not a safe path segment.
    UnsafeName,
    /// HTTP 404: no published release (or a private or nonexistent repository).
    NoRelease { url: String },
    /// Safe redirect within this repository and asset whose tag `accept_tag` rejected.
    OtherTag { tag: String },
    /// HTTP 429 or 5xx transient error.
    Transient { code: u16, url: String },
    /// Transport failure or unparseable response.
    Transport(String),
    /// Non-redirect HTTP status other than 404, 429, or 5xx.
    Unexpected { code: u16, url: String },
    /// Redirect `Location` does not match the exact tag-specific download URL of `name` in `owner/repo`.
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

/// Verified release tag and derived download URL from the evergreen pointer redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    pub tag: String,
    pub location: String,
}

/// Strictly parses a redirect `Location` into `https://github.com/{owner}/{repo}/releases/download/<tag>/{name}`
/// with a [`path_segment_safe`] `<tag>` accepted by `accept_tag`.
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
        return Err(PointerError::OtherTag {
            tag: tag.to_string(),
        });
    }
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

/// Classifies a redirect-refusing HEAD response for `url` under `accept_tag`.
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

/// [`resolve`] over an injected HEAD transport.
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

/// Resolves the newest published release tag in `owner/repo` via one HEAD request for `name`.
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

/// Returns `true` if `tag` is a canonical `vMAJOR.MINOR.PATCH` release tag.
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
}
