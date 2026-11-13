// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Mach-O header detection, dylib load-command inspection, and staged-tree executable checks.

use std::path::{Path, PathBuf};

use crate::install::StageError;

/// Thin Mach-O, as the first four bytes read big-endian: 32/64-bit, either byte order.
const THIN_MAGICS: [u32; 4] = [0xFEED_FACE, 0xFEED_FACF, 0xCEFA_EDFE, 0xCFFA_EDFE];

/// Universal (fat) headers, 32- and 64-bit arch tables, in either byte order.
const FAT_MAGICS: [u32; 4] = [0xCAFE_BABE, 0xCAFE_BABF, 0xBEBA_FECA, 0xBFBA_FECA];

/// Returns whether `head` starts with a thin Mach-O or non-empty universal header magic.
#[must_use]
pub fn is_macho_header(head: &[u8]) -> bool {
    let Some(word) = head.get(..4) else {
        return false;
    };
    let magic = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    if THIN_MAGICS.contains(&magic) {
        return true;
    }
    FAT_MAGICS.contains(&magic) && head.get(4..8).is_some_and(|n| n != [0; 4])
}

/// `LC_LOAD_WEAK_DYLIB`: a dylib the loader maps only if present;
/// brew's linkage checker excludes these (`DYLIB_USE_WEAK_LINK`).
const WEAK_DYLIB: u32 = 0x8000_0018;

/// The load commands that name a dylib the loader maps (`<mach-o/loader.h>`): `LC_LOAD_DYLIB`, `LC_LOAD_WEAK_DYLIB`, `LC_REEXPORT_DYLIB`, `LC_LAZY_LOAD_DYLIB` and `LC_LOAD_UPWARD_DYLIB`.
const DYLIB_LOADS: [u32; 5] = [0xC, WEAK_DYLIB, 0x8000_001F, 0x20, 0x8000_0023];

/// `LC_ID_DYLIB`: a dylib's own install name, which a program linked against it records.
const DYLIB_ID: u32 = 0xD;

/// `LC_RPATH`: one run path an `@rpath/` name is searched under.
const RPATH: u32 = 0x8000_001C;

/// The fixed part of an `rpath_command`: cmd, cmdsize, then the path's offset.
const RPATH_COMMAND: usize = 12;

/// The most slices a universal file may name before the reader refuses it: a real one
/// carries two or three.
const MAX_SLICES: u32 = 16;

/// The largest Mach-O [`load_dylibs_at`] reads whole.
const MAX_MACHO_BYTES: usize = 1 << 30;

/// A 64-bit Mach-O header's size, which its load commands follow.
const HEADER_64: usize = 32;

/// A 32-bit Mach-O header's size (no reserved word), which its load commands follow.
const HEADER_32: usize = 28;

/// What a static archive opens with (`ar`'s magic): a universal file's slice may be one,
/// which carries object files and loads no dylib.
const ARCHIVE_MAGIC: &[u8] = b"!<arch>\n";

/// The fixed part of a `dylib_command`: cmd, cmdsize, then the `dylib` struct (name
/// offset, timestamp, current and compatibility versions).
const DYLIB_COMMAND: usize = 24;

/// Error returned when reading a Mach-O's load commands.
#[derive(Debug)]
pub enum LoadError {
    /// The file could not be read (not a regular file, too large, or I/O).
    Io(std::io::Error),
    /// The bytes are not a well-formed Mach-O: what is wrong, and where.
    Malformed(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot read the Mach-O: {e}"),
            Self::Malformed(why) => write!(f, "malformed Mach-O: {why}"),
        }
    }
}

impl std::error::Error for LoadError {}

const fn malformed(why: String) -> LoadError {
    LoadError::Malformed(why)
}

/// The 4-byte word at `at` in `bytes`, little- or big-endian.
fn word(bytes: &[u8], at: usize, little: bool, what: &str) -> Result<u32, LoadError> {
    let w = at
        .checked_add(4)
        .and_then(|end| bytes.get(at..end))
        .ok_or_else(|| malformed(format!("{what} is cut short at byte {at}")))?;
    let w = [w[0], w[1], w[2], w[3]];
    Ok(if little {
        u32::from_le_bytes(w)
    } else {
        u32::from_be_bytes(w)
    })
}

/// The big-endian 8-byte word at `at` (a 64-bit universal arch table's offset and size).
fn wide(bytes: &[u8], at: usize, what: &str) -> Result<u64, LoadError> {
    let hi = word(bytes, at, false, what)?;
    let lo = word(bytes, at + 4, false, what)?;
    Ok((u64::from(hi) << 32) | u64::from(lo))
}

/// Whether `bytes` open as a Java class file rather than a universal Mach-O: both start
/// `CAFEBABE`, and a class file's minor and major versions read as an arch count of at
/// least 45 (Java 1.1's major), more slices than any universal file carries.
#[must_use]
pub fn is_java_class(bytes: &[u8]) -> bool {
    bytes.get(..4) == Some(&[0xCA, 0xFE, 0xBA, 0xBE][..])
        && bytes
            .get(4..8)
            .is_some_and(|n| u32::from_be_bytes([n[0], n[1], n[2], n[3]]) >= 45)
}

/// The names a Mach-O's load commands carry, each list in load-command order, slice after
/// slice for a universal file (a name both slices carry appears once per slice): what
/// `otool -D`, `otool -L` and `otool -l`'s `LC_RPATH` rows print, read without a process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadNames {
    /// The dylib's own install name (`LC_ID_DYLIB`), once per slice; empty for a program.
    pub id: Vec<String>,
    /// Every dylib it loads ([`DYLIB_LOADS`]).
    pub loads: Vec<String>,
    /// The loads named ONLY by `LC_LOAD_WEAK_DYLIB` commands (a name
    /// also loaded by a strong command is not weak): the loader
    /// tolerates their absence, and brew's linkage checker skips
    /// them entirely.
    pub weak_loads: Vec<String>,
    /// Its run paths (`LC_RPATH`).
    pub rpaths: Vec<String>,
}

impl LoadNames {
    /// Each list with every name once, first appearance first: the names of a universal
    /// file's slices merged.
    #[must_use]
    pub fn once(&self) -> LoadNames {
        let once = |list: &[String]| -> Vec<String> {
            let mut seen: Vec<String> = Vec::new();
            for name in list {
                if !seen.contains(name) {
                    seen.push(name.clone());
                }
            }
            seen
        };
        LoadNames {
            id: once(&self.id),
            loads: once(&self.loads),
            weak_loads: once(&self.weak_loads),
            rpaths: once(&self.rpaths),
        }
    }
}

/// The install name, the loaded dylibs and the run paths of the Mach-O `bytes`
/// ([`LoadNames`]).
///
/// Reads thin 32- and 64-bit Mach-Os in either byte order and universal files with 32- or
/// 64-bit arch tables; a universal file's static-archive slice (`!<arch>`, object files)
/// names nothing, as `otool` lists no dylib for one. Refuses ([`LoadError::Malformed`]) a
/// byte-swapped or nested universal header, an empty or oversized arch table, a slice
/// outside the file, load commands that overrun `sizeofcmds` or the file, a `cmdsize`
/// under 8 or not a multiple of the header's alignment (8 for 64-bit, 4 for 32-bit), and a
/// dylib or rpath command whose name offset falls outside it or whose name is empty,
/// unterminated or not UTF-8.
///
/// # Errors
/// [`LoadError::Malformed`] as above.
pub fn load_names(bytes: &[u8]) -> Result<LoadNames, LoadError> {
    let magic = word(bytes, 0, false, "the header")?;
    let mut out = LoadNames::default();
    let mut strong: Vec<String> = Vec::new();
    match magic {
        0xCAFE_BABE | 0xCAFE_BABF => {
            let is_wide = magic == 0xCAFE_BABF;
            let count = word(bytes, 4, false, "the universal header")?;
            if count == 0 || count > MAX_SLICES {
                return Err(malformed(format!(
                    "a universal header naming {count} slices (1 to {MAX_SLICES} are read)"
                )));
            }
            let entry = if is_wide { 32 } else { 20 };
            for i in 0..count as usize {
                let base = 8 + i * entry;
                let what = format!("arch entry {i}");
                let (offset, size) = if is_wide {
                    (
                        wide(bytes, base + 8, &what)?,
                        wide(bytes, base + 16, &what)?,
                    )
                } else {
                    (
                        u64::from(word(bytes, base + 8, false, &what)?),
                        u64::from(word(bytes, base + 12, false, &what)?),
                    )
                };
                let slice = usize::try_from(offset)
                    .ok()
                    .zip(usize::try_from(size).ok())
                    .and_then(|(start, len)| Some((start, start.checked_add(len)?)))
                    .filter(|(start, _)| *start >= 8 + count as usize * entry)
                    .and_then(|(start, end)| bytes.get(start..end))
                    .ok_or_else(|| {
                        malformed(format!(
                            "slice {i} (offset {offset}, size {size}) lies outside the file"
                        ))
                    })?;
                if slice.starts_with(ARCHIVE_MAGIC) {
                    continue;
                }
                thin_loads(slice, &format!("slice {i}"), &mut out, &mut strong)?;
            }
        }
        0xBEBA_FECA | 0xBFBA_FECA => {
            return Err(malformed(
                "a byte-swapped universal header, which no Apple tool writes".to_owned(),
            ));
        }
        _ => thin_loads(bytes, "the file", &mut out, &mut strong)?,
    }
    out.weak_loads.retain(|name| !strong.contains(name));
    Ok(out)
}

/// The names one thin slice's load commands carry, appended to `out`
/// (strongly loaded names also to `strong`, for the weak-only split).
fn thin_loads(
    slice: &[u8],
    what: &str,
    out: &mut LoadNames,
    strong: &mut Vec<String>,
) -> Result<(), LoadError> {
    let (little, header, align) = match word(slice, 0, false, what)? {
        0xCFFA_EDFE => (true, HEADER_64, 8),
        0xFEED_FACF => (false, HEADER_64, 8),
        0xCEFA_EDFE => (true, HEADER_32, 4),
        0xFEED_FACE => (false, HEADER_32, 4),
        0xCAFE_BABE | 0xCAFE_BABF | 0xBEBA_FECA | 0xBFBA_FECA => {
            return Err(malformed(format!(
                "{what} is a universal file inside a universal file"
            )));
        }
        other => {
            return Err(malformed(format!(
                "{what} opens with {other:#010x}, not a Mach-O magic"
            )));
        }
    };
    if slice.len() < header {
        return Err(malformed(format!("{what} is shorter than a Mach-O header")));
    }
    let ncmds = word(slice, 16, little, what)?;
    let sizeofcmds = word(slice, 20, little, what)? as usize;
    let end = header
        .checked_add(sizeofcmds)
        .filter(|end| *end <= slice.len())
        .ok_or_else(|| {
            malformed(format!(
                "{what}'s load commands ({sizeofcmds} bytes) run past its end"
            ))
        })?;
    let mut at = header;
    for i in 0..ncmds {
        if at + 8 > end {
            return Err(malformed(format!(
                "{what}'s load command {i} of {ncmds} starts past sizeofcmds"
            )));
        }
        let cmd = word(slice, at, little, what)?;
        let size = word(slice, at + 4, little, what)? as usize;
        if size < 8 || !size.is_multiple_of(align) {
            return Err(malformed(format!(
                "{what}'s load command {i} has cmdsize {size} (a multiple of {align}, at least 8)"
            )));
        }
        let next = at.checked_add(size).filter(|n| *n <= end).ok_or_else(|| {
            malformed(format!(
                "{what}'s load command {i} (cmdsize {size}) runs past sizeofcmds"
            ))
        })?;
        let (list, fixed, kind) = if DYLIB_LOADS.contains(&cmd) {
            (&mut out.loads, DYLIB_COMMAND, "dylib")
        } else if cmd == DYLIB_ID {
            (&mut out.id, DYLIB_COMMAND, "dylib id")
        } else if cmd == RPATH {
            (&mut out.rpaths, RPATH_COMMAND, "rpath")
        } else {
            at = next;
            continue;
        };
        let command = Command {
            slice,
            at,
            next,
            fixed,
            little,
        };
        let name = command.name(&format!("{what}'s {kind} command {i}"))?;
        if DYLIB_LOADS.contains(&cmd) {
            if cmd == WEAK_DYLIB {
                out.weak_loads.push(name.clone());
            } else {
                strong.push(name.clone());
            }
        }
        list.push(name);
        at = next;
    }
    Ok(())
}

/// One load command that names a path: `slice[at..next]`, whose first `fixed` bytes are
/// its fixed part, the path's offset the word at `at + 8` (`lc_str`).
struct Command<'a> {
    slice: &'a [u8],
    at: usize,
    next: usize,
    fixed: usize,
    little: bool,
}

impl Command<'_> {
    /// The path the command names, `what` naming the command in a refusal.
    fn name(&self, what: &str) -> Result<String, LoadError> {
        let size = self.next - self.at;
        if size < self.fixed {
            return Err(malformed(format!(
                "{what} is {size} bytes, under its fixed {} bytes",
                self.fixed
            )));
        }
        let offset = word(self.slice, self.at + 8, self.little, what)? as usize;
        if !(self.fixed..size).contains(&offset) {
            return Err(malformed(format!(
                "{what} names its path at offset {offset}, outside the command ({}..{size})",
                self.fixed
            )));
        }
        let body = &self.slice[self.at + offset..self.next];
        let name = body
            .iter()
            .position(|b| *b == 0)
            .map(|nul| &body[..nul])
            .ok_or_else(|| malformed(format!("{what} has no NUL-terminated path")))?;
        std::str::from_utf8(name)
            .ok()
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| malformed(format!("{what} names an empty or non-UTF-8 path")))
    }
}

/// Every dylib the Mach-O at `path` loads ([`load_names`]' `loads`), read whole through a
/// no-follow open of a regular file.
///
/// # Errors
/// [`LoadError::Io`] when it is not a regular file, is over 1 GiB or cannot be read;
/// [`LoadError::Malformed`] as [`load_names`] refuses.
pub fn load_dylibs_at(path: &Path) -> Result<Vec<String>, LoadError> {
    load_names_at(path).map(|names| names.loads)
}

/// [`load_names`] of the Mach-O at `path`, read whole through a no-follow open of a
/// regular file.
///
/// # Errors
/// As [`load_dylibs_at`].
pub fn load_names_at(path: &Path) -> Result<LoadNames, LoadError> {
    let bytes =
        crate::metadata_io::read_bounded_regular(path, MAX_MACHO_BYTES).map_err(LoadError::Io)?;
    load_names(&bytes)
}

/// Returns whether `name` is an absolute path under `/usr/lib/` or `/System/` without `.` or `..`.
#[must_use]
pub fn is_system_dylib(name: &str) -> bool {
    (name.starts_with("/usr/lib/") || name.starts_with("/System/"))
        && !name.split('/').any(|part| part == "." || part == "..")
}

/// Returns sorted, deduplicated non-system dylibs loaded by the Mach-O at `path`.
///
/// # Errors
/// As [`load_dylibs_at`].
pub fn foreign_dylibs(path: &Path) -> Result<Vec<String>, LoadError> {
    let mut foreign: Vec<String> = load_dylibs_at(path)?
        .into_iter()
        .filter(|name| !is_system_dylib(name))
        .collect();
    foreign.sort();
    foreign.dedup();
    Ok(foreign)
}

/// Whether `head` opens an interpreter script (`#!`), which the kernel runs without any
/// code signature.
fn is_script_header(head: &[u8]) -> bool {
    head.starts_with(b"#!")
}

/// What the regular file at `path` opens as.
fn sniff(path: &Path) -> std::io::Result<Sniffed> {
    let head = crate::metadata_io::read_head(path, 8)?;
    Ok(if is_macho_header(&head) {
        Sniffed::MachO
    } else if is_script_header(&head) {
        Sniffed::Script
    } else {
        Sniffed::Other
    })
}

enum Sniffed {
    MachO,
    Script,
    Other,
}

/// `path` relative to `root` for a message, or whole when it is not under it.
fn shown(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Returns all regular Mach-O files under `root`, sorted, without following symlinks.
///
/// # Errors
/// [`StageError::SignerRefused`] on scripts, symlinked Mach-Os, or special files;
/// [`StageError::Io`] on I/O failure.
pub fn find_macho(root: &Path) -> Result<Vec<PathBuf>, StageError> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(StageError::Io)? {
            let entry = entry.map_err(StageError::Io)?;
            let path = entry.path();
            let kind = entry.file_type().map_err(StageError::Io)?;
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                match sniff(&path).map_err(StageError::Io)? {
                    Sniffed::MachO => found.push(path),
                    Sniffed::Script => {
                        let mut m = shown(root, &path);
                        m.push_str(" is an interpreter script, which runs unsigned");
                        return Err(StageError::SignerRefused(m));
                    }
                    Sniffed::Other => {}
                }
            } else if kind.is_symlink() {
                let Ok(target) = std::fs::canonicalize(&path) else {
                    continue;
                };
                let target_is_file =
                    std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_file());
                if target_is_file
                    && matches!(sniff(&target).map_err(StageError::Io)?, Sniffed::MachO)
                {
                    let mut m = shown(root, &path);
                    m.push_str(" is a symlink to a Mach-O; only a regular file is verified");
                    return Err(StageError::SignerRefused(m));
                }
            } else {
                let mut m = shown(root, &path);
                m.push_str(" is neither a regular file, a directory nor a symlink");
                return Err(StageError::SignerRefused(m));
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Returns whether the regular file at `path` is a Mach-O or ELF object.
#[must_use]
pub(crate) fn is_native_object(path: &Path) -> bool {
    crate::metadata_io::read_head(path, 8)
        .is_ok_and(|head| is_macho_header(&head) || head.starts_with(b"\x7fELF"))
}

/// Recognize the native Unix image header: Mach-O on macOS, ELF elsewhere.
/// This is a format check, not proof of a runnable image, ownership or signature.
#[must_use]
pub fn is_native_executable(head: &[u8]) -> bool {
    if cfg!(target_os = "macos") {
        is_macho_header(head)
    } else {
        head.starts_with(b"\x7fELF")
    }
}

/// Returns all regular files with an execute bit under `root`, sorted.
///
/// # Errors
/// [`StageError::Io`] when the tree cannot be read.
pub(crate) fn executable_files(root: &Path) -> Result<Vec<PathBuf>, StageError> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(StageError::Io)? {
            let entry = entry.map_err(StageError::Io)?;
            let kind = entry.file_type().map_err(StageError::Io)?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                let meta = entry.metadata().map_err(StageError::Io)?;
                if crate::platform::permission_mode(&meta) & 0o111 != 0 {
                    found.push(entry.path());
                }
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Returns the regular files under `root` with an execute bit that are not this platform's
/// native executables, sorted: what [`demote_foreign_executables`] demotes.
///
/// # Errors
/// [`StageError::Io`] when the tree or a file's head cannot be read.
pub(crate) fn foreign_executables(root: &Path) -> Result<Vec<PathBuf>, StageError> {
    let mut foreign = Vec::new();
    for path in executable_files(root)? {
        let head = crate::metadata_io::read_head(&path, 8).map_err(StageError::Io)?;
        if !is_native_executable(&head) {
            foreign.push(path);
        }
    }
    Ok(foreign)
}

/// Clears execute bits on non-native regular files under `root`.
///
/// # Errors
/// [`StageError::Io`] when the tree cannot be read or a mode cannot be set.
pub(crate) fn demote_foreign_executables(root: &Path) -> Result<bool, StageError> {
    let mut demoted = false;
    for path in executable_files(root)? {
        let (head, meta) =
            crate::metadata_io::read_head_with_meta(&path, 8).map_err(StageError::Io)?;
        if !is_native_executable(&head) {
            let mode = crate::platform::permission_mode(&meta) & 0o7777;
            crate::platform::set_mode(&path, mode & !0o111).map_err(StageError::Io)?;
            demoted = true;
        }
    }
    Ok(demoted)
}

/// Mach-O fixtures built from their load commands, for this reader's tests and for the
/// tests of what reads through it (abrew's linkage scan and relocation).
#[cfg(test)]
#[path = "macho_fixtures_tests.rs"]
pub(crate) mod fixtures;

#[cfg(test)]
#[path = "macho_tests.rs"]
mod tests;
