// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The unmetered asset lane: a release asset's browser download URL, and telling that host
//! apart from the API host.
//!
//! Anonymous `api.github.com` is metered (~60 requests/hour per IP, shared behind a NAT; even
//! a 304 counts). `https://github.com/{owner}/{repo}/releases/download/{tag}/{name}` is not:
//! for a public repo it answers 200 via a 302 to `release-assets.githubusercontent.com` with no
//! `x-ratelimit-*` headers, and 404 for a missing asset or a private repo (no credential is
//! accepted there). With the tag from the evergreen pointer on the same host
//! ([`crate::pointer`]), the credential-less check makes no API request at all.
//!
//! The URL is a pure function of `(owner, repo, tag, name)`, never read from a server
//! response: `tag` comes from the pointer's strictly parsed `Location` or from signed data the
//! caller holds, and `name` is a publisher-convention asset name. `aterm-update` and `atpkg`
//! both build it here, so the two cannot drift onto a 404.

/// Whether `segment` may be spliced into a synthesized URL path as-is: non-empty, not
/// `.`/`..`, and only `[A-Za-z0-9._-]`, so no extra segment, query, fragment, `%` escape,
/// whitespace or control byte.
#[must_use]
pub fn path_segment_safe(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// The browser download URL for asset `name` of release `tag` in `owner/repo`, or `None`
/// (a refusal: there is no listing to fall back to) when any of the four is not a safe path
/// segment. Every asset name and tag either publisher emits passes.
#[must_use]
pub fn release_download_url(owner: &str, repo: &str, tag: &str, name: &str) -> Option<String> {
    if !(path_segment_safe(owner)
        && path_segment_safe(repo)
        && path_segment_safe(tag)
        && path_segment_safe(name))
    {
        return None;
    }
    // Manual concat rather than `format!`: the Trust lane cannot lower `fmt::Arguments`.
    let mut url = String::with_capacity(
        "https://github.com/".len()
            + owner.len()
            + repo.len()
            + "/releases/download/".len()
            + tag.len()
            + name.len()
            + 3,
    );
    url.push_str("https://github.com/");
    url.push_str(owner);
    url.push('/');
    url.push_str(repo);
    url.push_str("/releases/download/");
    url.push_str(tag);
    url.push('/');
    url.push_str(name);
    Some(url)
}

/// The metered host. A 403 means rate limit only here; from `github.com` it is a blocked host
/// or proxy (a private or unpublished asset answers 404 there), and GitHub's web-host throttle
/// is a 429.
#[must_use]
pub fn is_api_host(url: &str) -> bool {
    url.starts_with("https://api.github.com/")
}

#[cfg(test)]
mod tests {
    use super::{is_api_host, path_segment_safe, release_download_url};

    /// The exact strings are the contract with the publisher's convention (the DMG URL
    /// `aterm-release` writes into the appcast in `manifest_out.rs`, and the roster URL).
    #[test]
    fn derived_url_matches_the_publisher_convention() {
        assert_eq!(
            release_download_url("alabsystems", "aterm", "v0.73.0", "aterm-0.73.0.dmg").as_deref(),
            Some("https://github.com/alabsystems/aterm/releases/download/v0.73.0/aterm-0.73.0.dmg")
        );
        assert_eq!(
            release_download_url("alabsystems", "aterm", "v0.73.0", "aterm-machines.toml")
                .as_deref(),
            Some(
                "https://github.com/alabsystems/aterm/releases/download/v0.73.0/aterm-machines.toml"
            )
        );
        // Every asset name the two publishers emit passes the predicate.
        for name in [
            "aterm-appcast.toml",
            "aterm-appcast.toml.sig",
            "aterm-machines.toml",
            "aterm-machines.toml.sig",
            "aterm-0.74.0-mac.zip",
            "aterm-0.74.0.dmg",
            "pkg-trust-mc-20011.toml",
            "ty-2973.tar.zst",
        ] {
            assert!(path_segment_safe(name), "{name}");
        }
        for tag in ["v0.74.0", "atpkg-ty-2973", "atpkg-trust-mc-20011"] {
            assert!(path_segment_safe(tag), "{tag}");
        }
    }

    /// Anything that could splice the URL onto another host, path, query or fragment declines.
    #[test]
    fn derived_url_refuses_unsafe_owner_repo_tag_and_name() {
        let ok = ("alabsystems", "aterm", "v0.1.0", "aterm-appcast.toml");
        assert!(release_download_url(ok.0, ok.1, ok.2, ok.3).is_some());
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a?b",
            "a#b",
            "a%2fb",
            "a b",
            "a\nb",
            "a\tb",
            "https://evil.com",
            "..\\x",
        ] {
            assert!(
                release_download_url(bad, ok.1, ok.2, ok.3).is_none(),
                "owner {bad:?}"
            );
            assert!(
                release_download_url(ok.0, bad, ok.2, ok.3).is_none(),
                "repo {bad:?}"
            );
            assert!(
                release_download_url(ok.0, ok.1, bad, ok.3).is_none(),
                "tag {bad:?}"
            );
            assert!(
                release_download_url(ok.0, ok.1, ok.2, bad).is_none(),
                "name {bad:?}"
            );
        }
    }

    #[test]
    fn api_and_web_hosts_are_told_apart() {
        assert!(is_api_host(
            "https://api.github.com/repos/alabsystems/aterm/releases/assets/541859636"
        ));
        assert!(!is_api_host(
            "https://github.com/alabsystems/aterm/releases/download/v0.73.0/aterm-machines.toml"
        ));
        assert!(!is_api_host(
            "https://release-assets.githubusercontent.com/github-production-release-asset/x"
        ));
        // A look-alike host is not the API host.
        assert!(!is_api_host("https://api.github.com.evil.com/x"));
        assert!(!is_api_host("http://api.github.com/x"));
    }
}
