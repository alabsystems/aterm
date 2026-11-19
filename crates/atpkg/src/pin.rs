// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Local pin state for programs frozen against `update` and `sync`, and, apart from it, which
//! of those pins a named harness install made ([`set_held`]): a hold that a named update
//! releases, where a pin the person set stays until they unpin it.
//!
//! Unattended work holds only for what it read ([`holds_update`], [`is_held`]). A pin-format
//! file that does not read is set aside beside itself as `<name>.unread-<unix seconds>`, kept
//! for a person to read, and holds nothing ([`set_aside_files`]). A read this process could not
//! make just now (out of descriptors or memory) says nothing of the file: it sets nothing aside
//! and holds every program for that call. A person's own verb (`aterm pkg pin`,
//! `unpin`, `list`, `update <harness>`) is told why a file does not read ([`pinned_set`],
//! [`is_pinned`], [`marks_hold`]), and a diagnostic reads without changing anything
//! ([`would_hold_update`]).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::store::Layout;

const MAX_PIN_FILE_BYTES: usize = 256 * 1024;
const MAX_PINNED_PROGRAMS: usize = 2048;

/// The lock every writer of a pin-format file holds ([`set_in`], [`set_aside`]).
const LOCK: &str = "pins.lock";
/// What a set-aside file's name puts between the file it was and the second it was set aside.
const UNREAD: &str = ".unread-";
/// How many set-aside names one second may take (`<name>.unread-<n>`, then `-2`, `-3`, ...).
const MAX_SET_ASIDE_NAMES: u32 = 64;

/// A pin-format file under the prefix: one program name per line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinFile {
    /// The person's pins.
    Pins,
    /// The pins `aterm pkg install <id>-alab --build <n>` made ([`set_held`]).
    Holds,
}

impl PinFile {
    /// Its name under the prefix.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pins => "pins",
            Self::Holds => "harness-holds",
        }
    }

    /// What it holds, in words.
    const fn what(self) -> &'static str {
        match self {
            Self::Pins => "local pins",
            Self::Holds => "harness holds",
        }
    }
}

#[cfg(test)]
fn pins_path(layout: &Layout) -> PathBuf {
    layout.prefix.join(PinFile::Pins.name())
}

/// Read the local pins. Only a confirmed missing file means an empty set.
///
/// # Errors
/// Refuses unreadable, nonregular, oversized, malformed or over-count pin state.
pub fn pinned_set(layout: &Layout) -> io::Result<BTreeSet<String>> {
    set_of(layout, PinFile::Pins)
}

/// The names in the pin-format `file` under the prefix ([`pinned_set`]).
fn set_of(layout: &Layout, file: PinFile) -> io::Result<BTreeSet<String>> {
    read_of(layout, file).map_err(Unread::into_error)
}

/// Why a pin-format file gave no names ([`read_of`]).
#[derive(Debug)]
enum Unread {
    /// The file does not read: it cannot be opened or is no regular file, is over its size or
    /// count bound, is not UTF-8, or holds a name no pin can have. Unattended work sets it aside.
    File(io::Error),
    /// This process could not read it just now ([`passing`]); nothing of the file is known.
    Now(io::Error),
}

impl Unread {
    fn into_error(self) -> io::Error {
        match self {
            Self::File(error) | Self::Now(error) => error,
        }
    }
}

/// The names in the pin-format `file` under the prefix, none for a confirmed missing file, or
/// why it gave none, in words that name the file.
fn read_of(layout: &Layout, file: PinFile) -> Result<BTreeSet<String>, Unread> {
    let path = layout.prefix.join(file.name());
    let said = |error: &dyn std::fmt::Display, kind: io::ErrorKind| {
        io::Error::new(
            kind,
            format!("{} at {}: {error}", file.what(), path.display()),
        )
    };
    let read = crate::metadata_io::read_bounded_regular_utf8(&path, MAX_PIN_FILE_BYTES);
    #[cfg(test)]
    let read = tests::instead_of_read(read);
    let body = match read {
        Ok(body) => body,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && fs::symlink_metadata(&path)
                    .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(BTreeSet::new());
        }
        Err(error) if passing(&error) => return Err(Unread::Now(said(&error, error.kind()))),
        Err(error) => return Err(Unread::File(said(&error, error.kind()))),
    };
    let mut out = BTreeSet::new();
    for line in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if !valid_name(line) {
            return Err(Unread::File(said(
                &"it holds an invalid program name",
                io::ErrorKind::InvalidData,
            )));
        }
        if out.insert(line.to_string()) && out.len() > MAX_PINNED_PROGRAMS {
            return Err(Unread::File(said(
                &format!("it names more than {MAX_PINNED_PROGRAMS} programs"),
                io::ErrorKind::InvalidData,
            )));
        }
    }
    Ok(out)
}

/// The OS error codes for a process, or the system, out of file descriptors.
#[cfg(unix)]
const OUT_OF_DESCRIPTORS: &[i32] = &[libc::EMFILE, libc::ENFILE];
/// `ERROR_TOO_MANY_OPEN_FILES`.
#[cfg(windows)]
const OUT_OF_DESCRIPTORS: &[i32] = &[4];
#[cfg(not(any(unix, windows)))]
const OUT_OF_DESCRIPTORS: &[i32] = &[];

/// Whether `error`, a read's, says this process could not read just now rather than anything of
/// the file: it, or the system, is out of file descriptors or memory, or a call was interrupted,
/// would block or found its object busy. Every other error is the file's, a permission refused
/// included.
fn passing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::OutOfMemory
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::ResourceBusy
    ) || error
        .raw_os_error()
        .is_some_and(|code| OUT_OF_DESCRIPTORS.contains(&code))
}

/// Whether `program` is locally pinned (held against update/sync).
///
/// # Errors
/// As [`pinned_set`]; an unreadable file establishes no answer.
pub fn is_pinned(layout: &Layout, program: &str) -> io::Result<bool> {
    pinned_set(layout).map(|pins| pins.contains(program))
}

/// Whether unattended update work must hold `program`: only a pin that was read holds. A pins
/// file that does not read (unreadable, not a regular file, over its size or count bound, or
/// holding a name no pin can have) holds nothing: it is set aside beside itself as
/// `pins.unread-<unix seconds>`, kept with its bytes for a person to read, and the packages log
/// names it and why; later calls find no pins file. `aterm pkg doctor` names the kept file
/// ([`set_aside_files`]), and `aterm pkg pin <program>` makes a pin again. A read this process
/// could not make just now (out of descriptors or memory) says nothing of the file, so every
/// program holds for that call, and nothing is moved or logged; a later call reads it.
#[must_use]
pub fn holds_update(layout: &Layout, program: &str) -> bool {
    unattended(layout, PinFile::Pins).is_none_or(|pins| pins.contains(program))
}

/// What [`holds_update`] would answer for `program`, read without changing anything, for a
/// diagnostic (`aterm pkg doctor`, `aterm doctor --read-only`): a pins file that does not read
/// holds nothing here too, and stays where it is for unattended work to set aside.
#[must_use]
pub fn would_hold_update(layout: &Layout, program: &str) -> bool {
    match read_of(layout, PinFile::Pins) {
        Ok(pins) => pins.contains(program),
        Err(Unread::Now(_)) => true,
        Err(Unread::File(_)) => false,
    }
}

/// What unattended work reads of `file`: its names, none for a confirmed missing file, none for
/// one that does not read, which is set aside first ([`set_aside`]), and `None` where this
/// process could not read it just now ([`passing`]), which moves and logs nothing; each caller
/// answers that in its own safe direction for the one call.
fn unattended(layout: &Layout, file: PinFile) -> Option<BTreeSet<String>> {
    match read_of(layout, file) {
        Ok(set) => Some(set),
        Err(Unread::Now(_)) => None,
        Err(Unread::File(error)) => {
            #[cfg(test)]
            tests::before_set_aside();
            set_aside(layout, file, &error, crate::flow::now_unix())
        }
    }
}

/// Set aside the pin-format `file`, which did not read (`first`), under the lock [`set_in`]
/// takes. It reads the file again there, since another process may have set it aside, or a pin
/// edit replaced it, while this one waited, and answers what that read finds: its names, or
/// `None` where this process could not read it just now ([`passing`]), which moves nothing.
/// Where it still does not read it is renamed beside itself to `<name>.unread-<now>` (`-2`,
/// `-3`, ... where a name is taken), never over another entry and never deleted, and the
/// packages log names it, why it did not read and where it is kept; the answer is no names.
/// Where it cannot be moved (a read-only prefix, a lock that does not open) the answer is no
/// names too, and the log says so once per process.
fn set_aside(
    layout: &Layout,
    file: PinFile,
    first: &io::Error,
    now: i64,
) -> Option<BTreeSet<String>> {
    let path = layout.prefix.join(file.name());
    let _writer = match crate::lock::lock_metadata(&layout.prefix.join(LOCK)) {
        Ok(writer) => writer,
        Err(why) => {
            note_unmoved(layout, &path, first, &why);
            return Some(BTreeSet::new());
        }
    };
    let error = match read_of(layout, file) {
        Ok(set) => return Some(set),
        Err(Unread::Now(_)) => return None,
        Err(Unread::File(error)) => error,
    };
    match move_aside(&path, file, now) {
        Ok(kept) => crate::packages_log::append(
            layout,
            &crate::packages_log::Event::SetAside {
                file: &path.display().to_string(),
                error: &error.to_string(),
                kept: &kept.display().to_string(),
                unmoved: "",
            },
        ),
        Err(why) => note_unmoved(layout, &path, &error, &why),
    }
    Some(BTreeSet::new())
}

/// Rename `path`, the pin-format `file`, to the first free `<name>.unread-<now>[-<n>]` beside it.
fn move_aside(path: &Path, file: PinFile, now: i64) -> io::Result<PathBuf> {
    let base = format!("{}{UNREAD}{now}", file.name());
    for n in 1..=MAX_SET_ASIDE_NAMES {
        let kept = path.with_file_name(if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        });
        match crate::platform::rename_noreplace(path, &kept) {
            Ok(()) => return Ok(kept),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{MAX_SET_ASIDE_NAMES} names beside it for second {now} are taken"),
    ))
}

/// Log, once per process for each file, that `path` did not read (`error`) and stayed where it
/// was (`why`).
fn note_unmoved(layout: &Layout, path: &Path, error: &io::Error, why: &io::Error) {
    static NOTED: std::sync::Mutex<BTreeSet<PathBuf>> = std::sync::Mutex::new(BTreeSet::new());
    let first = NOTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path.to_path_buf());
    if first {
        crate::packages_log::append(
            layout,
            &crate::packages_log::Event::SetAside {
                file: &path.display().to_string(),
                error: &error.to_string(),
                kept: "",
                unmoved: &why.to_string(),
            },
        );
    }
}

/// The pin-format files unattended work set aside under the prefix ([`holds_update`],
/// [`is_held`]), each with the file it was, in path order; none where the prefix does not list.
#[must_use]
pub fn set_aside_files(layout: &Layout) -> Vec<(PinFile, PathBuf)> {
    let Ok(entries) = fs::read_dir(&layout.prefix) else {
        return Vec::new();
    };
    let mut out: Vec<(PinFile, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            [PinFile::Pins, PinFile::Holds]
                .into_iter()
                .find(|file| {
                    name.strip_prefix(file.name())
                        .and_then(|rest| rest.strip_prefix(UNREAD))
                        .is_some_and(|stamp| !stamp.is_empty())
                })
                .map(|file| (file, entry.path()))
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
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
    set_in(layout, PinFile::Pins, program, pinned)
}

/// Whether `program`'s pin is a hold a named harness install made ([`set_held`]), as that install
/// reads it. A holds file that does not read marks no hold: it is set aside as
/// `harness-holds.unread-<unix seconds>`, as [`holds_update`] sets the pins aside, and the pins
/// it marked then read as the person's own, which `aterm pkg unpin` releases. A read this
/// process could not make just now (out of descriptors or memory) marks no hold for that call
/// and moves nothing.
#[must_use]
pub fn is_held(layout: &Layout, program: &str) -> bool {
    unattended(layout, PinFile::Holds).is_some_and(|holds| holds.contains(program))
}

/// Whether `program`'s pin is a hold a named harness install made, for a person's own verb
/// (`aterm pkg update <harness>`, which releases such a hold): a holds file that does not read
/// is not taken to mark no hold, which would make a hold the person's own pin, nor moved.
///
/// # Errors
/// As [`pinned_set`], for the holds file.
pub fn marks_hold(layout: &Layout, program: &str) -> io::Result<bool> {
    set_of(layout, PinFile::Holds).map(|holds| holds.contains(program))
}

/// Records or forgets that `program`'s pin is a named harness install's hold, returning
/// `Ok(changed)`. It pins nothing by itself: the pin is [`set_pinned`]'s.
///
/// # Errors
/// As [`set_pinned`].
pub fn set_held(layout: &Layout, program: &str, held: bool) -> io::Result<bool> {
    set_in(layout, PinFile::Holds, program, held)
}

/// Add `program` to, or take it from, the pin-format `file` ([`set_pinned`]).
fn set_in(layout: &Layout, file: PinFile, program: &str, pinned: bool) -> io::Result<bool> {
    if !valid_name(program) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program name must be a single safe path component (no separators, `.`, `..`, \
             outer whitespace, CR/LF, or NUL)",
        ));
    }
    layout.ensure_dir(&layout.prefix)?;
    let _writer = crate::lock::lock_metadata(&layout.prefix.join(LOCK))?;
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
    aterm_uds::atomic_file::write(&layout.prefix.join(file.name()), body.as_bytes(), 0o600)?;
    Ok(changed)
}

#[cfg(test)]
#[path = "pin_tests.rs"]
mod tests;
