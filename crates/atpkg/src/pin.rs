// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Local pin state for programs frozen against `update` and `sync`, and, apart from it, which
//! of those pins a named harness install made ([`set_held`]): a hold that a named update
//! releases, where a pin the person set stays until they unpin it.

use std::collections::BTreeSet;
use std::fs;
use std::io;

use crate::store::Layout;

const MAX_PIN_FILE_BYTES: usize = 256 * 1024;
const MAX_PINNED_PROGRAMS: usize = 2048;

/// The person's pins.
const PINS: &str = "pins";
/// The pins `aterm pkg install <id>-alab --build <n>` made ([`set_held`]).
const HOLDS: &str = "harness-holds";

#[cfg(test)]
fn pins_path(layout: &Layout) -> std::path::PathBuf {
    layout.prefix.join(PINS)
}

/// Read the local pins. Only a confirmed missing file means an empty set.
///
/// # Errors
/// Refuses unreadable, nonregular, oversized, malformed or over-count pin state.
pub fn pinned_set(layout: &Layout) -> io::Result<BTreeSet<String>> {
    set_of(layout, PINS)
}

/// The names in the pin-format file `file` under the prefix ([`pinned_set`]).
fn set_of(layout: &Layout, file: &str) -> io::Result<BTreeSet<String>> {
    let path = layout.prefix.join(file);
    let body = match crate::metadata_io::read_bounded_regular_utf8(&path, MAX_PIN_FILE_BYTES) {
        Ok(body) => body,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && fs::symlink_metadata(&path)
                    .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(BTreeSet::new());
        }
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!("local pins at {}: {error}", path.display()),
            ));
        }
    };
    let mut out = BTreeSet::new();
    for line in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if !valid_name(line) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "local pins contain an invalid program name",
            ));
        }
        if out.insert(line.to_string()) && out.len() > MAX_PINNED_PROGRAMS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "local pins exceed the program count limit",
            ));
        }
    }
    Ok(out)
}

/// Whether `program` is locally pinned (held against update/sync).
///
/// # Errors
/// As [`pinned_set`]; an unreadable file establishes no answer.
pub fn is_pinned(layout: &Layout, program: &str) -> io::Result<bool> {
    pinned_set(layout).map(|pins| pins.contains(program))
}

/// Whether unattended update work must hold: a known pin or an unknown pin file.
#[must_use]
pub fn holds_update(layout: &Layout, program: &str) -> bool {
    is_pinned(layout, program).unwrap_or(true)
}

fn valid_name(program: &str) -> bool {
    !program.is_empty()
        && program != "."
        && program != ".."
        && program.trim() == program
        && !program.contains(['/', '\\', '\n', '\r', '\0'])
}

/// Sets or clears the local pin for `program`, returning `Ok(changed)`.
///
/// # Errors
/// Refuses invalid names, lock failures, unreadable prior state, bounds violations and publication failures.
pub fn set_pinned(layout: &Layout, program: &str, pinned: bool) -> io::Result<bool> {
    set_in(layout, PINS, program, pinned)
}

/// Whether `program`'s pin is a hold a named harness install made ([`set_held`]).
///
/// # Errors
/// As [`pinned_set`], for the holds file.
pub fn is_held(layout: &Layout, program: &str) -> io::Result<bool> {
    set_of(layout, HOLDS).map(|holds| holds.contains(program))
}

/// Records or forgets that `program`'s pin is a named harness install's hold, returning
/// `Ok(changed)`. It pins nothing by itself: the pin is [`set_pinned`]'s.
///
/// # Errors
/// As [`set_pinned`].
pub fn set_held(layout: &Layout, program: &str, held: bool) -> io::Result<bool> {
    set_in(layout, HOLDS, program, held)
}

/// Add `program` to, or take it from, the pin-format file `file` ([`set_pinned`]).
fn set_in(layout: &Layout, file: &str, program: &str, pinned: bool) -> io::Result<bool> {
    if !valid_name(program) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program name must be a single safe path component (no separators, `.`, `..`, \
             outer whitespace, CR/LF, or NUL)",
        ));
    }
    layout.ensure_dir(&layout.prefix)?;
    let _writer = crate::lock::lock_metadata(&layout.prefix.join("pins.lock"))?;
    let mut set = set_of(layout, file)?;
    let changed = if pinned {
        set.insert(program.to_string())
    } else {
        set.remove(program)
    };
    if set.len() > MAX_PINNED_PROGRAMS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot pin more than {MAX_PINNED_PROGRAMS} programs"),
        ));
    }
    let body: String = set.iter().map(|p| format!("{p}\n")).collect();
    if body.len() > MAX_PIN_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("pin file exceeds {MAX_PIN_FILE_BYTES}-byte limit"),
        ));
    }
    #[cfg(test)]
    tests::before_publish();
    aterm_uds::atomic_file::write(&layout.prefix.join(file), body.as_bytes(), 0o600)?;
    Ok(changed)
}

#[cfg(test)]
#[path = "pin_tests.rs"]
mod tests;
