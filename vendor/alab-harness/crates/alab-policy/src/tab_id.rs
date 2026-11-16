// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE GRAMMAR OF A TAB ID'S TEXT, the one every reader of a tab ID takes: `t` and 10 lower-case
//! Crockford base32 characters (`tqn5r6e0148`), shown as its first 5 (`tqn5r`). It is strict:
//! lower case only, none of Crockford's aliases (`i`, `l`, `o`, `u`), exact lengths. The `s-`
//! and 20 lower-case hex digits a tab opened before the `t` form keeps are no tab ID by it
//! ([`is_tab_id`] refuses them): aterm reads such an ID apart, from its own shells
//! (`aterm_types::tab_id::is_legacy`), and the go-ahead, which reads another session's name in
//! a reply by this grammar and its shown form, also takes a legacy one named there by 6 to 20
//! of its hex digits (`go_ahead::session_id`). aterm's tab ID type
//! (`aterm_types::tab_id::TabId`), which mints, parses and shows an ID, is built on it.

/// The 32 characters of a tab ID after its `t`, in value order: Crockford's base32 alphabet in
/// lower case (`0-9` and `a-z` without `i`, `l`, `o` and `u`). It ascends in ASCII, so two IDs
/// order as text exactly as their values do.
pub const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// The bytes of a minted tab ID: `t` and 10 characters.
pub const LEN: usize = 11;

/// The bytes a minted tab ID is shown as: its first 5 (`t` and 4 characters).
pub const SHORT_LEN: usize = 5;

/// The random bits a tab ID carries: 10 characters of 5 bits.
pub const BITS: u32 = 50;

/// The value of one ID character, or `None` for a byte outside [`ALPHABET`].
#[must_use]
pub const fn digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'h' => Some(b - b'a' + 10),
        b'j' | b'k' => Some(b - b'j' + 18),
        b'm' | b'n' => Some(b - b'm' + 20),
        b'p'..=b't' => Some(b - b'p' + 22),
        b'v'..=b'z' => Some(b - b'v' + 27),
        _ => None,
    }
}

/// Whether `s` is a tab ID: exactly `t` and 10 characters of [`ALPHABET`].
#[must_use]
pub const fn is_tab_id(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != LEN || b[0] != b't' {
        return false;
    }
    let mut i = 1;
    while i < LEN {
        if digit(b[i]).is_none() {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
#[path = "tab_id_tests.rs"]
mod tests;
