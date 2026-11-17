// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The record a root step leaves when its apply fails for good, so setup does not ask the Mac
//! password for it again by itself.
//!
//! Setup's one elevation runs each root step, and a step whose apply cannot succeed (a helper
//! clone the signature check refuses, a job launchd will not run, a developer mode that will
//! not turn on) reads owed again afterwards. Asked for again by itself, it brings the password
//! back each time the person answers it, to no end. The step's root half therefore records a
//! failure it can tell is the step's own (a definite refusal of the clone, a published job that
//! served in no round, a tool of Apple's that answered with a failure; never one that may be
//! the Mac's passing state, such as a check or a tool that gave no answer) in a file of its own
//! beside Spotlight's ([`super::spotlight_scope::REFUSED`]): the **keys** of the inputs it
//! failed with, one a line. A run nobody asked for does not ask the
//! password for a step whose key stands there ([`stands`]); the step still reads owed, and a
//! person's own run (Finish setup, a typed `aterm init`) asks for it and tries it again. Every
//! root run that carries the step tries it too, whatever it ran for, and a success removes the
//! record.
//!
//! A key names what the apply consumed, so other inputs are asked for afresh: another build or
//! bundle for the helper ([`broker_key`]), another reading for developer mode
//! ([`developer_mode_key`], which names no build: a new build does not ask for it again by
//! itself). A record that holds [`KEYS`] keys stands for every key: a step that
//! failed for that many builds is not asked for by itself again.
//!
//! Only root's own file counts ([`super::spotlight_scope::listed`]): a missing, unreadable or
//! planted one says nothing, and the step is asked for.

use super::spotlight_scope::{Reads, listed};
use std::path::Path;

/// The LAN broker's record: root's own file, one key a line ([`broker_key`]).
pub const LAN_BROKER: &str = "/Library/Application Support/aterm/lan-broker.refused";

/// Developer mode's record: root's own file, one key a line ([`developer_mode_key`]).
pub const DEVELOPER_MODE: &str = "/Library/Application Support/aterm/developer-mode.refused";

/// The most keys one record holds; one that holds this many stands for every key.
pub const KEYS: usize = 8;

/// Whether the record at `file` stands for the step's inputs as they read now: root's own file
/// lists [`KEYS`] keys, or lists `key()`. The key is read only where a record lists something
/// (`None` where the inputs cannot be read), so a Mac with no record pays nothing for it.
#[must_use]
pub fn stands(reads: &dyn Reads, file: &str, key: impl FnOnce() -> Option<String>) -> bool {
    let Ok(keys) = listed(reads, file) else {
        return false;
    };
    if keys.is_empty() {
        return false;
    }
    keys.len() >= KEYS || key().is_some_and(|key| keys.contains(Path::new(&key)))
}

/// The key of a developer mode that reads owed: `off` where the mode is off (`on` false), which
/// no account's password changes on a Mac that refuses it, else the console account `uid`
/// whose authorization is missing.
#[must_use]
pub fn developer_mode_key(on: bool, uid: u32) -> String {
    if on {
        format!("member {uid}")
    } else {
        "off".to_owned()
    }
}

/// The key of a broker install from the bundle `source`, whose executable's code is `code`: a
/// digest of every CodeDirectory (each slice's CPU type, length and bytes), then the bundle's
/// path. `None` for a path a line cannot hold.
#[cfg(unix)]
#[must_use]
pub fn broker_key(
    source: &Path,
    code: &aterm_uds::lan::install::CodeDirectories,
) -> Option<String> {
    let source = source.to_str().filter(|path| !path.contains('\n'))?;
    let mut digest = aterm_hash::FNV128_OFFSET;
    for (cpu, directory) in code.slices() {
        digest = aterm_hash::fnv1a_128_extend(digest, &cpu.to_be_bytes());
        digest = aterm_hash::fnv1a_128_extend(digest, &(directory.len() as u64).to_be_bytes());
        digest = aterm_hash::fnv1a_128_extend(digest, directory);
    }
    Some(format!("{digest:032x} {source}"))
}

/// Whether the broker's record stands for the bundle this process runs from, on this Mac
/// ([`stands`], [`broker_key`]): `false` off macOS.
#[must_use]
pub fn broker_stands_here() -> bool {
    #[cfg(target_os = "macos")]
    {
        use aterm_uds::lan::install::{BUNDLE_EXECUTABLE, CodeDirectories, source_here};
        stands(&super::spotlight_scope::Live, LAN_BROKER, || {
            let source = source_here()?;
            let code = CodeDirectories::of_file(&source.join(BUNDLE_EXECUTABLE)).ok()?;
            broker_key(&source, &code)
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Whether developer mode's record stands for `key` on this Mac ([`stands`],
/// [`developer_mode_key`]): `false` off macOS.
#[must_use]
pub fn developer_mode_stands_here(key: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        stands(&super::spotlight_scope::Live, DEVELOPER_MODE, || {
            Some(key.to_owned())
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = key;
        false
    }
}

#[cfg(test)]
#[path = "refused_tests.rs"]
mod tests;
