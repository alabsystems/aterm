// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Browser release-asset download URLs and API-host detection, paired with
//! [`crate::pointer`] so credential-less update checks avoid metered `api.github.com` calls.

/// Whether `segment` is a non-empty `[A-Za-z0-9._-]` path segment other than `.` or `..`.
#[inline]
#[must_use]
pub fn path_segment_safe(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// Returns the browser download URL for asset `name` of release `tag` in `owner/repo`, or
/// `None` when any component fails [`path_segment_safe`].
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

/// Whether `url` targets `https://api.github.com/` (where HTTP 403 indicates rate limiting).
#[inline]
#[must_use]
pub fn is_api_host(url: &str) -> bool {
    url.starts_with("https://api.github.com/")
}

#[cfg(test)]
#[path = "cdn_tests.rs"]
mod tests;
