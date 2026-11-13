// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Local pin state for programs frozen against `update` and `sync`.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::store::Layout;

const MAX_PIN_FILE_BYTES: usize = 256 * 1024;
const MAX_PINNED_PROGRAMS: usize = 2048;

fn pins_path(layout: &Layout) -> PathBuf {
    layout.prefix.join("pins")
}

/// Read the local pins. Only a confirmed missing file means an empty set.
///
/// # Errors
/// Refuses unreadable, nonregular, oversized, malformed or over-count pin state.
pub fn pinned_set(layout: &Layout) -> io::Result<BTreeSet<String>> {
    let path = pins_path(layout);
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
/// Refuses invalid names, unreadable prior state, bounds violations and publication failures.
pub fn set_pinned(layout: &Layout, program: &str, pinned: bool) -> io::Result<bool> {
    if !valid_name(program) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program name must be a single safe path component (no separators, `.`, `..`, \
             outer whitespace, CR/LF, or NUL)",
        ));
    }
    let mut set = pinned_set(layout)?;
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
    layout.ensure_dir(&layout.prefix)?;
    aterm_uds::atomic_file::write(&pins_path(layout), body.as_bytes(), 0o600)?;
    Ok(changed)
}

#[cfg(test)]
#[path = "pin_tests.rs"]
mod tests;
