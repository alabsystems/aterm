// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The specification's rules for object paths, interface, error and member
//! names, and bus names (unique `:1.42` or well-known `org.a11y.Bus`).

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

/// Two or more `.`-separated elements of `[A-Za-z0-9_]` that do not start
/// with a digit; at most 255 bytes. Error names follow the same rule.
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

/// One element of `[A-Za-z0-9_]` that does not start with a digit; at most
/// 255 bytes.
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

/// A unique name (`:` then two or more `.`-separated elements of
/// `[A-Za-z0-9_-]`) or a well-known one (two or more elements of
/// `[A-Za-z0-9_-]` that do not start with a digit); at most 255 bytes.
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
mod tests {
    use super::*;

    #[test]
    fn object_paths() {
        for good in ["/", "/org", "/org/a11y/atspi/accessible/root", "/a_1/B2"] {
            assert!(is_object_path(good), "{good}");
        }
        for bad in ["", "org", "/org/", "//", "/org//a11y", "/org.a11y", "/é"] {
            assert!(!is_object_path(bad), "{bad}");
        }
    }

    #[test]
    fn interfaces_members_and_bus_names() {
        assert!(is_interface("org.a11y.atspi.Event.Object"));
        assert!(is_interface("org.freedesktop.DBus.Error.UnknownMethod"));
        for bad in [
            "org",
            ".org.a11y",
            "org..a11y",
            "org.a11y.",
            "org.1a11y",
            "org.a-11y",
            "",
        ] {
            assert!(!is_interface(bad), "{bad}");
        }
        assert!(!is_interface(&format!("a.{}", "b".repeat(254))));
        assert!(is_member("GetRole") && is_member("_x1"));
        for bad in ["", "1x", "Get.Role", "Get-Role"] {
            assert!(!is_member(bad), "{bad}");
        }
        for good in [
            ":1.42",
            ":1.0",
            "org.a11y.Bus",
            "org.freedesktop.DBus",
            "a-b.c_d",
            ":x-1.2y",
        ] {
            assert!(is_bus_name(good), "{good}");
        }
        for bad in [
            "",
            ":",
            ":1",
            "org",
            "org.",
            "1org.a11y",
            ":1..2",
            "org.a11y/Bus",
        ] {
            assert!(!is_bus_name(bad), "{bad}");
        }
    }
}
