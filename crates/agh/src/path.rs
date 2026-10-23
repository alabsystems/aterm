// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! RFC 3986 encoders and dot-segment guards for URL path segments and query values.

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

/// Percent-encodes a single URL path segment.
#[inline]
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

/// Returns whether any `/`-separated segment of `text` is `.` or `..`.
#[inline]
#[must_use]
pub fn has_dot_segment(text: &str) -> bool {
    text.split('/').any(|part| part == "." || part == "..")
}

/// Returns whether `text`, after percent-decoding, contains a `.` or `..` path segment.
#[must_use]
pub(crate) fn has_encoded_dot_segment(text: &str) -> bool {
    aterm_codec::pct::decode(text)
        .split(['/', '\\'])
        .any(|part| part == "." || part == "..")
}

/// Builds the usage error for an input containing a `.` or `..` segment.
pub(crate) fn dot_segment_error(what: &str, text: &str) -> Error {
    Error::usage(format!(
        "{what} \"{text}\" holds a . or .. part: agh sends no dot-segment"
    ))
}

/// Rejects `text` if it contains a `.` or `..` segment.
///
/// # Errors
///
/// Returns a usage error if [`has_dot_segment`] is true.
pub(crate) fn refuse_dot_segment(what: &str, text: &str) -> Result<()> {
    if has_dot_segment(text) {
        return Err(dot_segment_error(what, text));
    }
    Ok(())
}

/// Rejects a pre-encoded `text` if percent-decoding reveals a `.` or `..` segment.
///
/// # Errors
///
/// Returns a usage error if [`has_encoded_dot_segment`] is true.
pub(crate) fn refuse_encoded_dot_segment(what: &str, text: &str) -> Result<()> {
    if has_encoded_dot_segment(text) {
        return Err(dot_segment_error(what, text));
    }
    Ok(())
}

/// Returns whether `tag` is empty, has an empty `/`-separated part, or has a dot-segment.
#[inline]
#[must_use]
pub(crate) fn bad_tag(tag: &str) -> bool {
    tag.split('/').any(str::is_empty) || has_dot_segment(tag)
}

/// Rejects `tag` if [`bad_tag`] is true.
///
/// # Errors
///
/// Returns a usage error when `tag` cannot be used in a release path.
pub(crate) fn refuse_bad_tag(tag: &str) -> Result<()> {
    if bad_tag(tag) {
        return Err(Error::usage(format!(
            "tag \"{tag}\" is empty or holds an empty, . or .. part: no release can have it"
        )));
    }
    Ok(())
}

/// One query value (the same encoding as `segment`).
#[inline]
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
#[inline]
pub(crate) fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// Parses `text` as `T` when it consists solely of one or more ASCII digits.
#[inline]
pub(crate) fn digits<T: std::str::FromStr>(text: &str) -> Option<T> {
    if is_digits(text) {
        text.parse().ok()
    } else {
        None
    }
}
