// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! The key-text string type.
//!
//! Added by the aterm project in 2026; see the repository NOTICE. Upstream winit re-exports
//! `smol_str::SmolStr` here; this fork defines its own (2026-10-15,
//! docs/AUDIT-third-party-2026-10-15.md, winit-core step 2), which retired the vendored
//! `smol_str` 0.2.2 fork. Every caller, in this fork and in aterm, only builds one with
//! [`SmolStr::new`] and reads it as a `str`, so the type is that and the traits the key and
//! event types derive, with no `unsafe`: the inline arm is read back through a checked
//! `str::from_utf8`.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

/// The longest text kept inline. Twenty-two bytes, a length byte and the tag fit in the 24
/// bytes the shared arm needs anyway, so a `SmolStr` is no bigger than upstream's.
const INLINE_CAP: usize = 22;

/// An immutable string whose clone is `O(1)`: text up to 22 bytes (what a key types) is held
/// inline with no allocation, longer text in one shared allocation.
#[derive(Clone)]
pub struct SmolStr(Repr);

#[derive(Clone)]
enum Repr {
    Inline { len: u8, buf: [u8; INLINE_CAP] },
    Shared(Arc<str>),
}

impl SmolStr {
    /// `text` as a `SmolStr`.
    #[inline]
    pub fn new<T: AsRef<str>>(text: T) -> SmolStr {
        let text = text.as_ref();
        match u8::try_from(text.len()) {
            Ok(len) if text.len() <= INLINE_CAP => {
                let mut buf = [0; INLINE_CAP];
                buf[..text.len()].copy_from_slice(text.as_bytes());
                SmolStr(Repr::Inline { len, buf })
            },
            _ => SmolStr(Repr::Shared(Arc::from(text))),
        }
    }

    /// The text.
    #[inline]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            // `new` copied exactly `len` bytes of a `str` here, so this never fails; the check
            // is what keeps the type free of `unsafe`, and a failure is a bug in `new`.
            Repr::Inline { len, buf } => std::str::from_utf8(&buf[..usize::from(*len)])
                .expect("SmolStr::new stores whole UTF-8 text inline"),
            Repr::Shared(text) => text,
        }
    }
}

impl Default for SmolStr {
    #[inline]
    fn default() -> Self {
        SmolStr(Repr::Inline { len: 0, buf: [0; INLINE_CAP] })
    }
}

impl Deref for SmolStr {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for SmolStr {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for SmolStr {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for SmolStr {
    #[inline]
    fn from(text: &str) -> Self {
        SmolStr::new(text)
    }
}

impl From<String> for SmolStr {
    #[inline]
    fn from(text: String) -> Self {
        SmolStr::new(text)
    }
}

// Equality, order and hash are the text's, as `Borrow<str>` requires.
impl PartialEq for SmolStr {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for SmolStr {}

impl PartialEq<str> for SmolStr {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for SmolStr {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialOrd for SmolStr {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SmolStr {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl Hash for SmolStr {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl fmt::Debug for SmolStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for SmolStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips_on_both_sides_of_the_inline_limit() {
        for text in ["", "a", "é", "\u{1F600}", "abcdefghijklmnopqrstuv", "abcdefghijklmnopqrstuvw"]
        {
            let s = SmolStr::new(text);
            assert_eq!(s.as_str(), text);
            assert_eq!(s.clone(), s);
            assert_eq!(format!("{s:?}"), format!("{text:?}"));
        }
        assert!(matches!(SmolStr::new("abcdefghijklmnopqrstuv").0, Repr::Inline { .. }));
        assert!(matches!(SmolStr::new("abcdefghijklmnopqrstuvw").0, Repr::Shared(_)));
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<SmolStr>(), 24);
    }
}
