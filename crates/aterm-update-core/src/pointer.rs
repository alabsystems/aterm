// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resolves the newest published release tag via one unmetered HEAD request to
//! `/releases/latest/download/{name}`, with a fresh query per lookup so an
//! intermediary cannot reuse a redirect for an older release.
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
    if !path_segment_safe(owner) || !path_segment_safe(repo) || !path_segment_safe(name) {
        return Err(refused("the release source is not URL-safe"));
    }
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
    let mut url = latest_download_url(owner, repo, name).ok_or(PointerError::UnsafeName)?;
    let mut nonce = [0u8; 16];
    aterm_uds::rand::fill(&mut nonce)
        .map_err(|error| PointerError::Transport(format!("release lookup freshness: {error}")))?;
    url.push_str(&format!(
        "?aterm_lookup={:032x}",
        u128::from_be_bytes(nonce)
    ));
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
#[path = "pointer_tests.rs"]
mod tests;
