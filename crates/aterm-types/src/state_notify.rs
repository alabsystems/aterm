// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Darwin notification aterm posts about a file under one state home, named for that home.
//!
//! A command line that changed a state file posts a notification, and the running app that
//! shows that state hears it and reads the file again (`aterm_macos_ui::darwin_notify`). A
//! Darwin notification name is heard by every process of the machine registered for it, so one
//! name for every state home would wake each app on the Mac, the person's own included,
//! whenever a test or a lab instance changed a file in its scratch home. [`name`] carries the
//! home in the name instead: only a listener of the same home hears the post.

use std::ffi::{CStr, CString};
use std::path::{Component, Path, PathBuf};

/// How many bytes [`name`] adds to its base: a dot and 16 hex digits.
pub const KEY_LEN: usize = 17;

/// `base`'s notification about a file under the state home `home`: `base`, a dot, and the 16
/// lowercase hex digits of FNV-1a/64 (`aterm_hash::fnv1a_64`) over `home` as [`resolved`]
/// spells it, so two processes that spell one home two ways (through a link, say) name one
/// notification. Its length is `base`'s and [`KEY_LEN`] bytes more whatever the home's path:
/// notify(3) documents no limit on a name's length, and no name this makes grows with the path.
#[must_use]
pub fn name(base: &CStr, home: &Path) -> CString {
    let key = aterm_hash::fnv1a_64_to_hex16(aterm_hash::fnv1a_64(
        resolved(home).as_os_str().as_encoded_bytes(),
    ));
    let mut bytes = Vec::with_capacity(base.count_bytes() + KEY_LEN);
    bytes.extend_from_slice(base.to_bytes());
    bytes.push(b'.');
    bytes.extend_from_slice(&key);
    // Never the default: a C string's bytes before its end, a dot and hex digits hold no NUL.
    CString::new(bytes).unwrap_or_default()
}

/// `home` as [`name`] hashes it: canonicalized (`std::fs::canonicalize`: absolute, every link
/// resolved) where it exists. A home not made yet is its nearest existing ancestor canonicalized,
/// with the parts of `home` after it as spelled, each `..` among them dropping the part before
/// it (a folder the home's making makes, which `canonicalize` steps back out of once it is made)
/// or, with no such part before it, the ancestor's last part; so a home that `create_dir_all` of
/// its spelling makes names what it named before, unless one of those parts is made a link. A
/// home no part of which resolves (a relative path naming nothing, which no state home is:
/// `crate::dirs::resolve_state_dir` refuses a relative root) is hashed as spelled. Two spellings
/// `canonicalize` keeps apart, such as macOS's firmlink `/System/Volumes/Data/Users/<user>` for
/// `/Users/<user>`, name two notifications.
fn resolved(home: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = home;
    loop {
        if let Ok(mut real) = std::fs::canonicalize(at) {
            let mut made = Vec::new();
            for part in rest.into_iter().rev() {
                match part {
                    Component::ParentDir => {
                        if made.pop().is_none() {
                            real.pop();
                        }
                    }
                    Component::CurDir => {}
                    part => made.push(part),
                }
            }
            return made.iter().fold(real, |path, part| path.join(part));
        }
        // `parent` answers only where the last component is a name, `.` or `..`.
        match (at.parent(), at.components().next_back()) {
            (Some(parent), Some(part)) => {
                rest.push(part);
                at = parent;
            }
            _ => return home.to_path_buf(),
        }
    }
}

#[cfg(test)]
#[path = "state_notify_tests.rs"]
mod tests;
