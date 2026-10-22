// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! RFC 3986 encoders for path segments and query values: every byte outside the unreserved
//! set (`A-Z a-z 0-9 - . _ ~`) becomes `%XX`, uppercase. `+` is `%2B`, `/` is `%2F` and a
//! space is `%20`.

use crate::error::{Error, Result};

/// The uppercase hex digits, by nibble.
const UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// `text`'s bytes, each outside the unreserved set as `%XX`.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(UPPER[usize::from(b >> 4)]));
            out.push(char::from(UPPER[usize::from(b & 0x0f)]));
        }
    }
    out
}

/// One segment.
#[must_use]
pub fn segment(text: &str) -> String {
    encode(text)
}

/// A file path: each `/`-separated part encoded, empty parts dropped, joined with `/`.
#[must_use]
pub fn file_path(path: &str) -> String {
    path.split('/')
        .filter(|part| !part.is_empty())
        .map(encode)
        .collect::<Vec<_>>()
        .join("/")
}

/// A ref (`heads/feature`, a tag): each part encoded, every `/` kept.
#[must_use]
pub fn ref_path(reference: &str) -> String {
    reference
        .split('/')
        .map(encode)
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether any `/`-separated part of `text` is exactly `.` or `..`. Such a part is a
/// dot-segment, which `segment`, `file_path` and `ref_path` keep as it is, and which a server
/// that normalises paths would resolve, so a request would leave the route it was decided
/// for. Their callers refuse such a text first; percent-encoding is no cure, because URL
/// parsers read `%2e%2e` as a dot-segment too.
#[must_use]
pub fn has_dot_segment(text: &str) -> bool {
    text.split('/').any(|part| part == "." || part == "..")
}

/// Whether `text`, percent-decoded as a whole, has a part between `/` or `\` that is exactly
/// `.` or `..`. A typed path is judged this way: decoding before splitting catches a
/// dot-segment hidden behind an encoded separator (`..%2F..%2Fx`), which a front end that
/// decodes `%2F` before it removes dot-segments would resolve.
#[must_use]
pub(crate) fn has_encoded_dot_segment(text: &str) -> bool {
    aterm_codec::pct::decode(text)
        .split(['/', '\\'])
        .any(|part| part == "." || part == "..")
}

/// Usage `{what} "{text}" holds a . or .. part: agh sends no dot-segment`.
pub(crate) fn dot_segment_error(what: &str, text: &str) -> Error {
    Error::usage(format!(
        "{what} \"{text}\" holds a . or .. part: agh sends no dot-segment"
    ))
}

/// The one refusal of a text with a dot-segment ([`has_dot_segment`]), given before any
/// request; `what` names the text (`tag`, `ref`, `path`).
///
/// # Errors
///
/// Usage, as above.
pub(crate) fn refuse_dot_segment(what: &str, text: &str) -> Result<()> {
    if has_dot_segment(text) {
        return Err(dot_segment_error(what, text));
    }
    Ok(())
}

/// [`refuse_dot_segment`] for a typed, already-encoded path, judged by
/// [`has_encoded_dot_segment`].
///
/// # Errors
///
/// Usage, as above.
pub(crate) fn refuse_encoded_dot_segment(what: &str, text: &str) -> Result<()> {
    if has_encoded_dot_segment(text) {
        return Err(dot_segment_error(what, text));
    }
    Ok(())
}

/// Whether `tag` cannot be named in a release path: empty, with an empty `/`-part (`v1/`,
/// `a//b`, which a slash-merging server reads as another tag), or with a dot-segment. Git
/// refuses all three as a ref name, so no release has such a tag.
#[must_use]
pub(crate) fn bad_tag(tag: &str) -> bool {
    tag.split('/').any(str::is_empty) || has_dot_segment(tag)
}

/// The one refusal of a tag [`bad_tag`] names, given before any request.
///
/// # Errors
///
/// Usage `tag "{tag}" is empty or holds an empty, . or .. part: no release can have it`.
pub(crate) fn refuse_bad_tag(tag: &str) -> Result<()> {
    if bad_tag(tag) {
        return Err(Error::usage(format!(
            "tag \"{tag}\" is empty or holds an empty, . or .. part: no release can have it"
        )));
    }
    Ok(())
}

/// One query value (the same encoding as `segment`).
#[must_use]
pub fn query_value(text: &str) -> String {
    encode(text)
}

/// `k=v&k2=v2` in the given order, keys and values encoded; `""` for none.
#[must_use]
pub fn query(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Whether `text` is one or more ASCII digits and nothing else.
pub(crate) fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// `text` as a `T` when it is one or more ASCII digits and nothing else (`str::parse` alone
/// would take a leading `+`); `None` otherwise, or when it does not fit `T`.
pub(crate) fn digits<T: std::str::FromStr>(text: &str) -> Option<T> {
    if is_digits(text) {
        text.parse().ok()
    } else {
        None
    }
}
