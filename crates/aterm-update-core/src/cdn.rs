// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The UNMETERED asset lane: deriving a release asset's browser download URL from the
//! four things that name it, and telling that host apart from the API host.
//!
//! # Why a derived URL, and why it lives here
//!
//! GitHub meters `api.github.com` at ~60 requests/hour per IP for an anonymous caller,
//! and that budget is shared by every machine (and every other tool) behind one NAT.
//! Measured 2026-09-02 against the shipped channel: a conditional releases LIST that
//! answers 304 still moves `x-ratelimit-used` (22 → 23), and each asset fetched through
//! the assets API costs one more unit. A steady-state anonymous check therefore spent
//! five metered requests, and on a saturated IP lived in "update check deferred".
//!
//! `https://github.com/{owner}/{repo}/releases/download/{tag}/{name}` is a different
//! host. Measured the same day: for a PUBLIC repo it answers 200 anonymously via a 302 to
//! `release-assets.githubusercontent.com` with NO `x-ratelimit-*` headers at all; a
//! missing asset answers 404; a PRIVATE repo answers 404 for the same URL shape (no
//! credential is accepted there). So every asset byte can move unmetered — and, with
//! the tag discovered from the evergreen pointer on the same host ([`crate::pointer`]),
//! the app's credential-less check makes no API request at all.
//!
//! The URL is a PURE FUNCTION of `(owner, repo, tag, name)` and is never read out of a
//! server response. `tag` comes from the evergreen pointer's strictly-parsed `Location`
//! (the app updater's web lane) or from a release listing's `tag_name` (the token lane,
//! and atpkg's listing lane), and `name` is a publisher-convention asset name, so
//! nothing about which bytes are fetched can be steered by a memo or a listing field.
//! Selection and every trust check stay exactly where they were — the same bytes reach
//! the same verifiers.
//!
//! Two clients build this URL — the app updater (`aterm-update`) and the toolchain
//! package manager (`atpkg`) — and a drift between two spellings of the publisher's
//! convention would silently send every fetch to a 404. One builder, one predicate.

/// Whether `segment` may be spliced into a synthesized URL path as-is.
///
/// Deliberately strict: non-empty, not `.`/`..`, and only `[A-Za-z0-9._-]` — so no
/// `/` (an extra path segment), no `?`/`#` (a query or fragment), no `%` (an escape
/// that could decode into any of those), and no whitespace or control byte. A refusal
/// costs the caller at most a fallback (see [`release_download_url`]); an acceptance of
/// any of the above would point the URL somewhere else entirely. Lifted verbatim from
/// atpkg's slug check so both clients refuse the same strings.
#[must_use]
pub fn path_segment_safe(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// The browser download URL for asset `name` of release `tag` in `owner/repo`, or
/// `None` when any of the four is not a safe path segment.
///
/// What `None` means is the caller's: on the app updater's web lane it is a REFUSAL (no
/// listing exists to fall back to, and nothing off this host is ever fetched there); on
/// atpkg's listing lane the caller falls back to the asset API URL the listing handed
/// it (metered, but proven to work). Every name the publisher emits — `aterm-appcast.toml`, its `.sig`,
/// `aterm-machines.toml`, its `.sig`, `aterm-<ver>-mac.zip`, `aterm-<ver>.dmg`, and the
/// `vMAJOR.MINOR.PATCH` tag — passes the predicate; atpkg's `atpkg-<program>-<build>`
/// tags and `pkg-<program>-<build>.toml` / `<program>-<build>.tar.zst` names do too.
#[must_use]
pub fn release_download_url(owner: &str, repo: &str, tag: &str, name: &str) -> Option<String> {
    if !(path_segment_safe(owner)
        && path_segment_safe(repo)
        && path_segment_safe(tag)
        && path_segment_safe(name))
    {
        return None;
    }
    // Manual concat rather than `format!`: this crate's Trust-gated paths avoid the
    // `fmt::Arguments` construction the strict gate cannot lower.
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

/// The metered host. A 403 means "rate limit" ONLY here; the same status from
/// `github.com` is a blocked host or proxy (a private or unpublished asset answers 404
/// there, never 403), and GitHub's own throttle on the web host is a 429.
#[must_use]
pub fn is_api_host(url: &str) -> bool {
    url.starts_with("https://api.github.com/")
}

#[cfg(test)]
mod tests {
    use super::{is_api_host, path_segment_safe, release_download_url};

    /// The exact strings are the contract with the publisher's convention: the DMG URL
    /// `aterm-release` writes into the appcast (`manifest_out.rs`) and the roster URL
    /// measured live on 2026-09-02 (200 via a 302 to release-assets, no rate-limit
    /// headers).
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

    /// Anything that could splice the synthesized URL onto another host, path, query
    /// or fragment must decline — never emit a URL built from it.
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
