// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! D-Bus validation rules for object paths, interface, error, member, and bus names.

/// The longest name the specification allows.
pub const MAX_NAME: usize = 255;

#[inline]
const fn is_element_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `/`, or `/` followed by `/`-separated non-empty elements of `[A-Za-z0-9_]`.
#[must_use]
pub fn is_object_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes == b"/" {
        return true;
    }
    match bytes.split_first() {
        Some((b'/', rest)) => rest
            .split(|&b| b == b'/')
            .all(|element| !element.is_empty() && element.iter().all(|&b| is_element_char(b))),
        _ => false,
    }
}

/// Two or more dot-separated `[A-Za-z0-9_]` elements not starting with a digit (at most 255 bytes); error names follow the same rule.
#[must_use]
pub fn is_interface(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() <= MAX_NAME
        && bytes.split(|&b| b == b'.').count() >= 2
        && bytes
            .split(|&b| b == b'.')
            .all(|element| match element.split_first() {
                Some((first, rest)) => {
                    !first.is_ascii_digit()
                        && is_element_char(*first)
                        && rest.iter().all(|&b| is_element_char(b))
                }
                None => false,
            })
}

/// One `[A-Za-z0-9_]` element that does not start with a digit, at most 255 bytes.
#[must_use]
pub fn is_member(name: &str) -> bool {
    let bytes = name.as_bytes();
    match bytes.split_first() {
        Some((first, rest)) => {
            bytes.len() <= MAX_NAME
                && !first.is_ascii_digit()
                && is_element_char(*first)
                && rest.iter().all(|&b| is_element_char(b))
        }
        None => false,
    }
}

/// A unique (`:`-prefixed) or well-known (non-digit-leading) bus name of two or more dot-separated `[A-Za-z0-9_-]` elements, at most 255 bytes.
#[must_use]
pub fn is_bus_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() > MAX_NAME {
        return false;
    }
    let (unique, body) = match bytes.split_first() {
        Some((b':', rest)) => (true, rest),
        _ => (false, bytes),
    };
    let ok = |b: u8| is_element_char(b) || b == b'-';
    body.split(|&b| b == b'.').count() >= 2
        && body
            .split(|&b| b == b'.')
            .all(|element| match element.split_first() {
                Some((first, rest)) => {
                    (unique || !first.is_ascii_digit()) && ok(*first) && rest.iter().all(|&b| ok(b))
                }
                None => false,
            })
}

#[cfg(test)]
#[path = "names_tests.rs"]
mod tests;
