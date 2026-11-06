// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! RFC 3986 encoders and dot-segment guards for URL path segments and query values, and the
//! shape checks of the words agh puts in a path or an argv: digits, lowercase hex, a sha and a
//! git ref name.

use crate::error::{Error, Result};

const UPPER: &[u8; 16] = b"0123456789ABCDEF";

pub(crate) fn encode_into(out: &mut String, text: &str) {
    if text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
    {
        out.push_str(text);
        return;
    }
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(UPPER[usize::from(b >> 4)]));
            out.push(char::from(UPPER[usize::from(b & 0x0f)]));
        }
    }
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    encode_into(&mut out, text);
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
    let mut out = String::with_capacity(path.len());
    for part in path.split('/').filter(|part| !part.is_empty()) {
        if !out.is_empty() {
            out.push('/');
        }
        encode_into(&mut out, part);
    }
    out
}

/// A ref (`heads/feature`, a tag): each part encoded, every `/` kept.
#[must_use]
pub fn ref_path(reference: &str) -> String {
    let mut out = String::with_capacity(reference.len());
    for (i, part) in reference.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        encode_into(&mut out, part);
    }
    out
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
    if !text.contains('%') {
        return text
            .split(['/', '\\'])
            .any(|part| part == "." || part == "..");
    }
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
    let mut out = String::with_capacity(pairs.iter().map(|(k, v)| k.len() + v.len() + 2).sum());
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        encode_into(&mut out, k);
        out.push('=');
        encode_into(&mut out, v);
    }
    out
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

/// Whether `text` is a full sha: 40 lowercase hex digits.
#[inline]
pub(crate) fn is_sha(text: &str) -> bool {
    aterm_codec::hex::is_lower(text, 40)
}

/// The one refusal of a value [`is_sha`] rejects, the value redacted by shape: every caller
/// (`agh ref`, `Refs`, the git runner) adds only its own context.
pub(crate) fn sha_refusal(sha: &str) -> String {
    format!(
        "{}: a sha is 40 lowercase hex digits",
        crate::redact::Redactor::shapes(sha)
    )
}

/// The one refusal of a reference [`reference_ok`] rejects (plan §9 body 14), the reference
/// redacted by shape: every caller adds only its own context.
pub(crate) fn reference_refusal(reference: &str) -> String {
    format!(
        "{}: agh reads and writes refs/heads/… and refs/tags/… only, by names git accepts",
        crate::redact::Redactor::shapes(reference)
    )
}

/// Whether `name` is a ref git's `check-ref-format` accepts under `refs/`: one or more
/// components after `refs/`, none empty, starting with `.` or ending with `.lock`; no `..`,
/// `@{`, ASCII control or DEL, space, `~`, `^`, `:`, `?`, `*`, `[` or `\`; and no trailing `.`.
pub(crate) fn ref_name_ok(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("refs/") else {
        return false;
    };
    if rest.is_empty() || name.ends_with('.') || name.contains("..") || name.contains("@{") {
        return false;
    }
    let bad_byte = |b: u8| {
        b.is_ascii_control() || matches!(b, b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
    };
    if name.bytes().any(bad_byte) {
        return false;
    }
    name.split('/')
        .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

/// The reference rule (plan §9 body 14): `refs/heads/…` or `refs/tags/…`, by a name
/// [`ref_name_ok`] accepts.
pub(crate) fn reference_ok(reference: &str) -> bool {
    let branch_or_tag = reference
        .strip_prefix("refs/heads/")
        .or_else(|| reference.strip_prefix("refs/tags/"))
        .is_some_and(|rest| !rest.is_empty());
    branch_or_tag && ref_name_ok(reference)
}
