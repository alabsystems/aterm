// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The EVERGREEN POINTER: learning which release is newest from one unmetered HEAD.
//!
//! # What it is
//!
//! GitHub serves `https://github.com/{owner}/{repo}/releases/latest/download/{name}`
//! as a 302 whose `Location` is `…/releases/download/<tag>/{name}` for the newest
//! published release — drafts and prereleases excluded BY CONSTRUCTION, on the server.
//! MEASURED 2026-09-03 against the shipped channel: `curl -sI --max-redirs 0` answers
//! `HTTP/2 302` with `location: https://github.com/alabsystems/aterm/releases/download/
//! v0.74.0/aterm-appcast.toml`, carries no `x-ratelimit-*` header at all, and the
//! private publish repo answers 404 on the same shape. So the question the releases
//! LIST used to answer — "what is the channel head?" — is answered by the web host in
//! one request that costs nothing against any budget, and a check whose head has not
//! moved needs no second request.
//!
//! # What it is NOT
//!
//! It is not trust, and nothing here is. The pointer chooses WHICH tag the tag-specific
//! GETs are addressed to; every byte those GETs return still meets the Ed25519 appcast
//! signature, the master-signed roster, the version/tag/URL binds and the build-number
//! and floor gates exactly as before. What this module guarantees is narrower and
//! structural: the tag is taken from the `Location` ONLY when the whole URL is exactly
//! `https://github.com/{owner}/{repo}/releases/download/<tag>/{name}` for the very
//! owner, repo and name the caller asked about, with `<tag>` passing the caller's
//! predicate and the strict [`crate::cdn::path_segment_safe`] charset. Any other
//! `Location` — another host, another repo, a query, an escape, a tag the caller does
//! not accept — is REFUSED, never "interpreted". A moved-during-check pointer cannot mix
//! two releases because nothing after the HEAD goes through `latest` again: the tag is
//! the address of every later GET.
//!
//! # What it does NOT promise: that the head carries an app build
//!
//! `latest` is the newest PUBLISHED release, and the publication train publishes the
//! shared `vX.Y.0` release from the source side before the app cut attaches its
//! assets (measured 2026-09-10: v0.80.0 carried only `SHA256SUMS` and a roster copy
//! for a whole night, and every client 404'd on `…/v0.80.0/aterm-appcast.toml`). So a
//! pointer that resolves is not a release that can be installed from; the check lane
//! treats a 404 on the head's appcast as the distinct "channel head has no app
//! manifest" state and elects the newest release that does carry one
//! (`aterm-update`'s `web_head_fallback`) — never as a broken download pipeline.

use crate::cdn::path_segment_safe;
use crate::http::{HeadAnswer, HttpError};

/// The evergreen URL for asset `name` of the NEWEST published release in `owner/repo`,
/// or `None` when any of the three is not a safe path segment (the same predicate the
/// tag-specific builder applies, so the two can never disagree about a name).
#[must_use]
pub fn latest_download_url(owner: &str, repo: &str, name: &str) -> Option<String> {
    if !(path_segment_safe(owner) && path_segment_safe(repo) && path_segment_safe(name)) {
        return None;
    }
    // Manual concat rather than `format!`: this crate's Trust-gated paths avoid the
    // `fmt::Arguments` construction the strict gate cannot lower.
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

/// Why the pointer did not yield a tag — split into the classes a check lane treats
/// differently, and nothing finer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerError {
    /// `owner`, `repo` or `name` is not a safe path segment: no URL was built and no
    /// request was made.
    UnsafeName,
    /// The host answered 404: this repository has no published (non-draft,
    /// non-prerelease) release carrying anything — or is private/nonexistent, which
    /// GitHub renders identically on the web host. A STANDING state, not weather.
    NoRelease { url: String },
    /// The host asked us to slow down (429) or is having a bad moment (5xx). Weather:
    /// defer and retry on the next cycle. `code` is the status.
    Transient { code: u16, url: String },
    /// curl itself failed (DNS, TLS, timeout) or the answer was unparseable.
    Transport(String),
    /// The host answered a status the pointer has no reading for (a 200 with no
    /// redirect, a 403 from a filtering proxy, …). A verdict about THIS host.
    Unexpected { code: u16, url: String },
    /// The 302 carried a `Location` that is not exactly the tag-specific download URL
    /// of `name` in this very repository under an acceptable tag. The value is NOT
    /// echoed (it is server-controlled text); the `why` is a fixed literal.
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
            Self::Refused { why } => write!(
                f,
                "the evergreen release pointer redirected somewhere this client refuses to \
                 follow ({why})"
            ),
        }
    }
}

/// The parsed pointer: the tag the newest published release carries, and the exact
/// tag-specific URL the `Location` named (already proved equal to the derived one, so a
/// caller may use either).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    pub tag: String,
    pub location: String,
}

/// Parse a 302's `Location` into the tag it names — STRICTLY.
///
/// The `Location` must be byte-for-byte `https://github.com/{owner}/{repo}/releases/
/// download/<tag>/{name}`: same host, same owner and repo, same asset name, exactly one
/// path segment for the tag, and that segment both [`path_segment_safe`] and accepted by
/// `accept_tag` (the caller's grammar — canonical `vMAJOR.MINOR.PATCH` for the app
/// channel). Everything else is [`PointerError::Refused`]. The derived
/// [`crate::cdn::release_download_url`] for the recovered tag is then re-built and
/// compared to the `Location`, so the value the caller fetches from is always the
/// derived one and never a server string with a hidden difference.
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
        return Err(refused(
            "the redirect's tag is not a release tag this client installs from",
        ));
    }
    // Re-derive and compare: the URL every later GET uses is the DERIVED one, and it
    // must be the very string the server named, or the two would fetch different things.
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

/// Classify a redirect-refusing HEAD's answer for `url` (built by
/// [`latest_download_url`]) into the tag it names, under `accept_tag`.
///
/// Pure: the transport is the caller's ([`resolve_with`] injects it), so every arm is
/// testable without a network.
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

/// [`resolve`] over an injected HEAD transport — the seam the update crates' tests
/// count requests through, so "no `api.github.com` request on the web lane" is a
/// measured fact rather than a claim.
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

/// The newest published release's tag in `owner/repo`, learned from ONE anonymous
/// unmetered HEAD of the evergreen URL for `name`. See the module doc.
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

/// The app channel's tag grammar for the pointer: exactly a canonical
/// `vMAJOR.MINOR.PATCH` candidate ([`crate::tag::parse_release_tag`] +
/// [`crate::tag::canonical_version`]) — the same rule the LIST-based selection applied,
/// so the two lanes can never install from differently-spelled tags.
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
        // The exact `location:` measured 2026-09-03.
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

    /// The measured answer parses to the measured tag, and the location handed back is
    /// the DERIVED URL (byte-equal to the server's, proved rather than assumed).
    #[test]
    fn the_measured_redirect_names_the_tag() {
        let p = parse_location(OWNER, REPO, NAME, &measured(), &canonical_app_tag).unwrap();
        assert_eq!(p.tag, "v0.74.0");
        assert_eq!(
            Some(p.location.as_str()),
            crate::cdn::release_download_url(OWNER, REPO, "v0.74.0", NAME).as_deref()
        );
    }

    /// Invariant (d): the pointer cannot choose a release outside the grammar, and
    /// invariant (a)/(b)-adjacent: it cannot point the later GETs off this repository.
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
        // Tags outside the grammar: legacy two-component, non-canonical spelling,
        // prerelease suffix, atpkg's tags, an empty tag.
        for tag in [
            "v0.74",
            "v01.74.0",
            "v0.74.0-rc1",
            "atpkg-index-41",
            "0.74.0",
            "",
        ] {
            let mut loc = String::from("https://github.com/alabsystems/aterm/releases/download/");
            loc.push_str(tag);
            loc.push('/');
            loc.push_str(NAME);
            refused(&loc);
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

    /// The status classes a check lane acts on: a redirect is the answer, a 404 is the
    /// loud standing state, 429/5xx is weather, anything else is a host verdict, and a
    /// redirect without a Location is refused.
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
        // The wording an operator meets for the standing state names the causes.
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

    /// `resolve_with` makes exactly ONE request, to the evergreen URL, and an unsafe
    /// source makes none.
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
