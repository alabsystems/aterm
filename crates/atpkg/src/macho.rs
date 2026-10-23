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

/// The load commands that name a dylib the loader maps (`<mach-o/loader.h>`): `LC_LOAD_DYLIB`, `LC_LOAD_WEAK_DYLIB`, `LC_REEXPORT_DYLIB`, `LC_LAZY_LOAD_DYLIB` and `LC_LOAD_UPWARD_DYLIB`.
const DYLIB_LOADS: [u32; 5] = [0xC, 0x8000_0018, 0x8000_001F, 0x20, 0x8000_0023];

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
            rpaths: once(&self.rpaths),
        }
    }
}

/// Every dylib the Mach-O `bytes` loads, in load-command order, slice after slice for a
/// universal file (a name both slices load appears once per slice): [`load_names`]'
/// `loads`.
///
/// # Errors
/// [`LoadError::Malformed`] as [`load_names`] refuses.
pub fn load_dylibs(bytes: &[u8]) -> Result<Vec<String>, LoadError> {
    load_names(bytes).map(|names| names.loads)
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
                thin_loads(slice, &format!("slice {i}"), &mut out)?;
            }
        }
        0xBEBA_FECA | 0xBFBA_FECA => {
            return Err(malformed(
                "a byte-swapped universal header, which no Apple tool writes".to_owned(),
            ));
        }
        _ => thin_loads(bytes, "the file", &mut out)?,
    }
    Ok(out)
}

/// The names one thin slice's load commands carry, appended to `out`.
fn thin_loads(slice: &[u8], what: &str, out: &mut LoadNames) -> Result<(), LoadError> {
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
        list.push(command.name(&format!("{what}'s {kind} command {i}"))?);
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

/// Every dylib the Mach-O at `path` loads ([`load_dylibs`]), read whole through a
/// no-follow open of a regular file.
///
/// # Errors
/// [`LoadError::Io`] when it is not a regular file, is over 1 GiB or cannot be read;
/// [`LoadError::Malformed`] as [`load_dylibs`] refuses.
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

/// Whether `head` opens this platform's native executable format: Mach-O on macOS, ELF on every other unix.
fn is_native_executable(head: &[u8]) -> bool {
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

/// Clears execute bits on non-native regular files under `root`.
///
/// # Errors
/// [`StageError::Io`] when the tree cannot be read or a mode cannot be set.
pub(crate) fn demote_foreign_executables(root: &Path) -> Result<bool, StageError> {
    let mut changed = false;
    for path in executable_files(root)? {
        let head = crate::metadata_io::read_head(&path, 8).map_err(StageError::Io)?;
        if is_native_executable(&head) {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path).map_err(StageError::Io)?;
        let mode = crate::platform::permission_mode(&meta) & 0o7777;
        crate::platform::set_mode(&path, mode & !0o111).map_err(StageError::Io)?;
        changed = true;
    }
    Ok(changed)
}

/// Mach-O fixtures built from their load commands, for this reader's tests and for the
/// tests of what reads through it (abrew's linkage scan and relocation).
#[cfg(test)]
pub(crate) mod fixtures {
    use super::{DYLIB_COMMAND, RPATH, RPATH_COMMAND};

    /// `LC_LOAD_DYLIB`.
    pub(crate) const LOAD_DYLIB: u32 = 0xC;
    /// `LC_ID_DYLIB`.
    pub(crate) const ID_DYLIB: u32 = super::DYLIB_ID;

    fn words(ws: &[u32], little: bool) -> Vec<u8> {
        ws.iter()
            .flat_map(|w| {
                if little {
                    w.to_le_bytes()
                } else {
                    w.to_be_bytes()
                }
            })
            .collect()
    }

    /// One `dylib_command` naming `name`, padded to a multiple of 8 (a 64-bit file's).
    pub(crate) fn dylib_command(cmd: u32, name: &str, little: bool) -> Vec<u8> {
        dylib_command_aligned(cmd, name, little, 8)
    }

    /// One `dylib_command` naming `name`, padded to a multiple of 4 only (a 32-bit
    /// file's alignment).
    pub(crate) fn dylib_command32(cmd: u32, name: &str, little: bool) -> Vec<u8> {
        dylib_command_aligned(cmd, name, little, 4)
    }

    fn dylib_command_aligned(cmd: u32, name: &str, little: bool, align: usize) -> Vec<u8> {
        let size = (DYLIB_COMMAND + name.len() + 1).div_ceil(align) * align;
        let fixed = [
            cmd,
            size as u32,
            DYLIB_COMMAND as u32,
            2,
            0x1_0000,
            0x1_0000,
        ];
        let mut c = words(&fixed, little);
        c.extend_from_slice(name.as_bytes());
        c.resize(size, 0);
        c
    }

    /// One `rpath_command` naming `path`, padded to a multiple of 8.
    pub(crate) fn rpath_command(path: &str, little: bool) -> Vec<u8> {
        let size = (RPATH_COMMAND + path.len() + 1).div_ceil(8) * 8;
        let mut c = words(&[RPATH, size as u32, RPATH_COMMAND as u32], little);
        c.extend_from_slice(path.as_bytes());
        c.resize(size, 0);
        c
    }

    /// A `cmd`/`cmdsize` pair padded to `size` bytes: a load command the reader skips.
    pub(crate) fn other_command(cmd: u32, size: u32, little: bool) -> Vec<u8> {
        let mut c = words(&[cmd, size], little);
        c.resize(size as usize, 0);
        c
    }

    /// A thin 64-bit arm64 executable whose load commands are `commands`, in that order.
    pub(crate) fn thin(commands: &[Vec<u8>], little: bool) -> Vec<u8> {
        let body: Vec<u8> = commands.concat();
        let magic = if little { 0xCFFA_EDFEu32 } else { 0xFEED_FACF };
        let mut m = magic.to_be_bytes().to_vec();
        let header = [
            0x0100_000C,
            0,
            2,
            commands.len() as u32,
            body.len() as u32,
            0,
            0,
        ];
        m.extend(words(&header, little));
        m.extend_from_slice(&body);
        m.extend_from_slice(&[0u8; 64]);
        m
    }

    /// A thin 32-bit arm64_32 dylib (a watchOS slice's shape) whose load commands are
    /// `commands`, in that order: a 28-byte header, no reserved word.
    pub(crate) fn thin32(commands: &[Vec<u8>], little: bool) -> Vec<u8> {
        let body: Vec<u8> = commands.concat();
        let magic = if little { 0xCEFA_EDFEu32 } else { 0xFEED_FACE };
        let mut m = magic.to_be_bytes().to_vec();
        let header = [
            0x0200_000C,
            1,
            6,
            commands.len() as u32,
            body.len() as u32,
            0,
        ];
        m.extend(words(&header, little));
        m.extend_from_slice(&body);
        m.extend_from_slice(&[0u8; 64]);
        m
    }

    /// A static archive (`ar`'s magic, then one member header): what a universal
    /// file's archive slice holds, object files that load no dylib.
    pub(crate) fn archive() -> Vec<u8> {
        let mut a = super::ARCHIVE_MAGIC.to_vec();
        a.extend_from_slice(b"absvdi2.c.o/    0           0     0     644     8         `\n");
        a.extend_from_slice(&[0xCF, 0xFA, 0xED, 0xFE, 0, 0, 0, 0]);
        a
    }

    /// A universal file of `slices`, with a 32- or 64-bit arch table.
    pub(crate) fn fat(slices: &[Vec<u8>], is_wide: bool) -> Vec<u8> {
        let entry = if is_wide { 32 } else { 20 };
        let mut offset = (8 + entry * slices.len()).div_ceil(16) * 16;
        let magic = if is_wide { 0xCAFE_BABFu32 } else { 0xCAFE_BABE };
        let mut f = magic.to_be_bytes().to_vec();
        f.extend_from_slice(&(slices.len() as u32).to_be_bytes());
        for s in slices {
            f.extend_from_slice(&0x0100_000Cu32.to_be_bytes());
            f.extend_from_slice(&0u32.to_be_bytes());
            if is_wide {
                f.extend_from_slice(&(offset as u64).to_be_bytes());
                f.extend_from_slice(&(s.len() as u64).to_be_bytes());
                f.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 0]);
            } else {
                f.extend_from_slice(&(offset as u32).to_be_bytes());
                f.extend_from_slice(&(s.len() as u32).to_be_bytes());
                f.extend_from_slice(&4u32.to_be_bytes());
            }
            offset += s.len().div_ceil(16) * 16;
        }
        for s in slices {
            f.resize(f.len().div_ceil(16) * 16, 0);
            f.extend_from_slice(s);
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{
        archive, dylib_command, dylib_command32, fat, other_command, rpath_command, thin, thin32,
    };
    use super::*;

    fn tmp(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-macho-{label}-"))
    }

    fn with_tail(head: &[u8]) -> Vec<u8> {
        let mut bytes = head.to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        bytes
    }

    /// `magic` (big-endian) then `nfat` in the given byte order.
    fn fat_header(magic: u32, nfat: u32, big_endian: bool) -> Vec<u8> {
        let mut h = magic.to_be_bytes().to_vec();
        if big_endian {
            h.extend_from_slice(&nfat.to_be_bytes());
        } else {
            h.extend_from_slice(&nfat.to_le_bytes());
        }
        h
    }

    #[test]
    fn thin_and_fat_headers_are_recognised_at_any_arch_count() {
        for magic in THIN_MAGICS {
            assert!(is_macho_header(&magic.to_be_bytes()), "{magic:#x}");
        }
        for magic in FAT_MAGICS {
            for nfat in [1u32, 2, 44, 45, 52, 200, 818, u32::MAX] {
                for big_endian in [true, false] {
                    assert!(
                        is_macho_header(&fat_header(magic, nfat, big_endian)),
                        "{magic:#x} nfat={nfat} big_endian={big_endian}"
                    );
                }
            }
        }
        let java8 = [0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x00, 0x00, 0x34];
        assert!(
            is_macho_header(&java8),
            "a class file is checked, not trusted"
        );
    }

    #[test]
    fn empty_fat_headers_and_other_files_are_not() {
        for magic in FAT_MAGICS {
            assert!(!is_macho_header(&fat_header(magic, 0, true)), "{magic:#x}");
        }
        assert!(!is_macho_header(&[0xCA, 0xFE, 0xBA, 0xBE]));
        assert!(!is_macho_header(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 1]));
        assert!(!is_macho_header(&[0xFE, 0xED]));
        assert!(!is_macho_header(b"\x7fELF\x02\x01\x01\x00"));
        assert!(!is_macho_header(b"#!/bin/sh\n"));
        assert!(!is_macho_header(b""));
    }

    #[test]
    fn the_walk_finds_thin_and_fat_files_by_content() {
        let d = tmp("walk");
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::create_dir_all(d.join("libexec/deep")).unwrap();
        std::fs::write(d.join("bin/tool"), with_tail(&0xCFFA_EDFEu32.to_be_bytes())).unwrap();
        let fat = fat_header(0xCAFE_BABE, 2, true);
        std::fs::write(d.join("libexec/deep/helper.txt"), with_tail(&fat)).unwrap();
        let fat45 = fat_header(0xCAFE_BABE, 45, true);
        std::fs::write(d.join("libexec/rg"), with_tail(&fat45)).unwrap();
        let fat200 = fat_header(0xCAFE_BABE, 200, true);
        std::fs::write(d.join("libexec/deep/zsh"), with_tail(&fat200)).unwrap();
        std::fs::write(d.join("README"), b"not code").unwrap();
        std::fs::write(d.join("NOTICE.md"), b"# Notices\n").unwrap();
        std::fs::write(d.join("bin/elf"), with_tail(b"\x7fELF")).unwrap();
        std::fs::write(d.join("empty"), b"").unwrap();
        assert_eq!(
            find_macho(&d).unwrap(),
            vec![
                d.join("bin/tool"),
                d.join("libexec/deep/helper.txt"),
                d.join("libexec/deep/zsh"),
                d.join("libexec/rg"),
            ]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_interpreter_script_is_refused() {
        let d = tmp("script");
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::create_dir_all(d.join("codex-path")).unwrap();
        std::fs::write(
            d.join("bin/codex"),
            with_tail(&0xCFFA_EDFEu32.to_be_bytes()),
        )
        .unwrap();
        std::fs::write(d.join("codex-path/rg"), b"#!/bin/sh\nexec evil\n").unwrap();
        match find_macho(&d) {
            Err(StageError::SignerRefused(m)) => {
                let path = Path::new("codex-path").join("rg").display().to_string();
                assert!(m.starts_with(&path), "{m}");
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A symlink resolving to a Mach-O is refused by name — inside the tree or out of it —
    /// while a symlink to anything else, or to nothing, is left alone.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_macho_is_refused_and_other_links_are_not() {
        let d = tmp("links");
        let outside = tmp("links-outside");
        std::fs::write(
            outside.join("real"),
            with_tail(&0xFEED_FACFu32.to_be_bytes()),
        )
        .unwrap();
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::write(d.join("notes"), b"text").unwrap();
        std::os::unix::fs::symlink("../notes", d.join("bin/notes")).unwrap();
        std::os::unix::fs::symlink("../missing", d.join("bin/dangling")).unwrap();
        assert_eq!(find_macho(&d).unwrap(), Vec::<PathBuf>::new());

        std::os::unix::fs::symlink(outside.join("real"), d.join("bin/claude")).unwrap();
        match find_macho(&d) {
            Err(StageError::SignerRefused(m)) => {
                assert!(m.starts_with("bin/claude "), "{m}");
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_file(d.join("bin/claude")).unwrap();

        std::fs::write(d.join("real"), with_tail(&0xFEED_FACFu32.to_be_bytes())).unwrap();
        std::os::unix::fs::symlink("../real", d.join("bin/claude")).unwrap();
        assert!(matches!(find_macho(&d), Err(StageError::SignerRefused(_))));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// The fish row's shape: libSystem, an `LC_LOAD_DYLINKER` and an `LC_UUID` beside the
    /// dylib a Homebrew-linked build loads.
    fn homebrew_pcre2_fish(little: bool) -> Vec<u8> {
        thin(
            &[
                other_command(0xE, 32, little),
                other_command(0x1B, 24, little),
                dylib_command(0xC, "/usr/lib/libSystem.B.dylib", little),
                dylib_command(0xC, "/opt/homebrew/lib/libpcre2-32.dylib", little),
            ],
            little,
        )
    }

    #[test]
    fn a_homebrew_pcre2_link_is_foreign_and_libsystem_alone_is_not() {
        let d = tmp("pcre2");
        let clean = thin(
            &[
                other_command(0xE, 32, true),
                dylib_command(0xC, "/usr/lib/libSystem.B.dylib", true),
                dylib_command(
                    0x8000_0018,
                    "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation",
                    true,
                ),
            ],
            true,
        );
        std::fs::write(d.join("clean"), &clean).unwrap();
        assert_eq!(
            foreign_dylibs(&d.join("clean")).unwrap(),
            Vec::<String>::new()
        );
        let pcre2 = "/opt/homebrew/lib/libpcre2-32.dylib".to_owned();
        for little in [true, false] {
            std::fs::write(d.join("fish"), homebrew_pcre2_fish(little)).unwrap();
            assert_eq!(
                load_dylibs_at(&d.join("fish")).unwrap(),
                vec!["/usr/lib/libSystem.B.dylib".to_owned(), pcre2.clone()],
                "little={little}"
            );
            assert_eq!(
                foreign_dylibs(&d.join("fish")).unwrap(),
                vec![pcre2.clone()]
            );
        }
        for is_wide in [false, true] {
            let both = fat(&[clean.clone(), homebrew_pcre2_fish(true)], is_wide);
            std::fs::write(d.join("universal"), both).unwrap();
            assert_eq!(
                foreign_dylibs(&d.join("universal")).unwrap(),
                vec![pcre2.clone()]
            );
            std::fs::write(
                d.join("universal"),
                fat(std::slice::from_ref(&clean), is_wide),
            )
            .unwrap();
            assert_eq!(
                foreign_dylibs(&d.join("universal")).unwrap(),
                Vec::<String>::new()
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn every_loading_command_is_read_and_the_id_is_not() {
        let mut commands: Vec<Vec<u8>> = DYLIB_LOADS
            .iter()
            .map(|cmd| dylib_command(*cmd, &format!("/opt/x/{cmd:x}.dylib"), true))
            .collect();
        commands.push(dylib_command(0xD, "/opt/x/self.dylib", true));
        let names = load_dylibs(&thin(&commands, true)).unwrap();
        assert_eq!(names.len(), DYLIB_LOADS.len(), "{names:?}");
        assert!(
            !names.iter().any(|n| n.ends_with("self.dylib")),
            "{names:?}"
        );
    }

    /// A Homebrew `:any` dylib before relocation: its own name, what it loads and its run
    /// paths all name the placeholder prefix, beside libSystem and an `LC_UUID`.
    fn placeholder_dylib(little: bool) -> Vec<u8> {
        thin(
            &[
                dylib_command(
                    DYLIB_ID,
                    "@@HOMEBREW_PREFIX@@/opt/x/lib/libx.1.dylib",
                    little,
                ),
                other_command(0x1B, 24, little),
                dylib_command(0xC, "@@HOMEBREW_CELLAR@@/xz/1/lib/liblzma.5.dylib", little),
                rpath_command("@@HOMEBREW_PREFIX@@/lib", little),
                dylib_command(0xC, "/usr/lib/libSystem.B.dylib", little),
                rpath_command("@loader_path/../lib", little),
            ],
            little,
        )
    }

    #[test]
    fn the_id_the_loads_and_the_rpaths_are_each_read() {
        let want = LoadNames {
            id: vec!["@@HOMEBREW_PREFIX@@/opt/x/lib/libx.1.dylib".into()],
            loads: vec![
                "@@HOMEBREW_CELLAR@@/xz/1/lib/liblzma.5.dylib".into(),
                "/usr/lib/libSystem.B.dylib".into(),
            ],
            rpaths: vec![
                "@@HOMEBREW_PREFIX@@/lib".into(),
                "@loader_path/../lib".into(),
            ],
        };
        for little in [true, false] {
            assert_eq!(load_names(&placeholder_dylib(little)).unwrap(), want);
        }
        let both = load_names(&fat(
            &[placeholder_dylib(true), placeholder_dylib(true)],
            true,
        ))
        .unwrap();
        assert_eq!(both.id, [want.id.clone(), want.id.clone()].concat());
        assert_eq!(both.rpaths.len(), 4);
        assert_eq!(both.once(), want, "the slices' names merge to one of each");
        let program = load_names(&homebrew_pcre2_fish(true)).unwrap();
        assert!(
            program.id.is_empty() && program.rpaths.is_empty(),
            "{program:?}"
        );
        assert_eq!(program.loads.len(), 2);
    }

    #[test]
    fn a_malformed_rpath_command_is_refused() {
        let good = thin(&[rpath_command("@loader_path/../lib", true)], true);
        assert!(load_names(&good).is_ok(), "the unmutated fixture reads");
        let at = HEADER_64;
        let size = u32::from_le_bytes(good[at + 4..at + 8].try_into().unwrap()) as usize;
        let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
        let mut low = good.clone();
        low[at + 8..at + 12].copy_from_slice(&4u32.to_le_bytes());
        cases.push(("offset inside the fixed part", low));
        for offset in [8u32, 11] {
            let mut word = good.clone();
            word[at + 8..at + 12].copy_from_slice(&offset.to_le_bytes());
            cases.push(("offset inside the path's own offset word", word));
        }
        let mut high = good.clone();
        high[at + 8..at + 12].copy_from_slice(&(size as u32).to_le_bytes());
        cases.push(("offset at the command's end", high));
        let mut unterminated = good.clone();
        for b in &mut unterminated[at + RPATH_COMMAND..at + size] {
            *b = b'a';
        }
        cases.push(("no NUL", unterminated));
        let mut empty = good.clone();
        for b in &mut empty[at + RPATH_COMMAND..at + size] {
            *b = 0;
        }
        cases.push(("empty path", empty));
        cases.push((
            "under its fixed part",
            thin(&[other_command(RPATH, 8, true)], true),
        ));
        for (what, bytes) in cases {
            match load_names(&bytes) {
                Err(LoadError::Malformed(_)) => {}
                other => panic!("{what}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_32_bit_mach_o_reads_and_an_archive_slice_names_nothing() {
        let watch = |little: bool| {
            thin32(
                &[
                    dylib_command32(DYLIB_ID, "@rpath/libswiftCore.dylib", little),
                    other_command(0x1B, 24, little),
                    dylib_command32(0xC, "/usr/lib/libSystem.B.dylib", little),
                    rpath_command("@loader_path/../lib", little),
                ],
                little,
            )
        };
        assert_ne!(
            dylib_command32(DYLIB_ID, "@rpath/libswiftCore.dylib", true).len() % 8,
            0,
            "the fixture's command is 4- and not 8-aligned"
        );
        let want = LoadNames {
            id: vec!["@rpath/libswiftCore.dylib".into()],
            loads: vec!["/usr/lib/libSystem.B.dylib".into()],
            rpaths: vec!["@loader_path/../lib".into()],
        };
        for little in [true, false] {
            assert_eq!(load_names(&watch(little)).unwrap(), want, "little={little}");
        }
        for is_wide in [false, true] {
            let universal = fat(&[watch(true), archive(), placeholder_dylib(true)], is_wide);
            let read = load_names(&universal).unwrap();
            assert_eq!(
                read.id,
                [
                    "@rpath/libswiftCore.dylib",
                    "@@HOMEBREW_PREFIX@@/opt/x/lib/libx.1.dylib"
                ]
            );
            assert_eq!(read.loads.len(), 3, "{read:?}");
            let archives = load_names(&fat(&[archive(), archive()], is_wide)).unwrap();
            assert_eq!(archives, LoadNames::default(), "a universal static archive");
        }
        let mut odd = watch(true);
        odd[HEADER_32 + 4..HEADER_32 + 8].copy_from_slice(&42u32.to_le_bytes());
        let four = thin(
            &[dylib_command32(0xC, "/usr/lib/libSystem.B.dylib", true)],
            true,
        );
        for (what, bytes) in [
            ("a 32-bit cmdsize not a multiple of 4", odd),
            ("a 64-bit cmdsize a multiple of 4 only", four),
            ("an archive alone", archive()),
            ("a 32-bit header cut short", watch(true)[..20].to_vec()),
        ] {
            match load_names(&bytes) {
                Err(LoadError::Malformed(_)) => {}
                other => panic!("{what}: {other:?}"),
            }
        }
    }

    /// The reader agrees with Apple's own `otool` on real files: a Command Line Tools
    /// dylib with an install name and a run path, Apple's universal `/bin/ls` and this test
    /// binary, every slice (`-arch all`), and where the developer tools carry them a thin
    /// 32-bit dylib (Swift 5.0's arm64_32 `libswiftCore.dylib`), a universal dylib with a
    /// 32-bit slice and a universal static archive (clang's runtime libraries, whose
    /// archive `otool` lists no dylib for). The `otool` run is the developer directory's own
    /// (read off `/var/db/xcode_select_link`, no process), never `/usr/bin/otool`, the shim
    /// that raises Apple's install dialog where no developer tools are; with none, the
    /// comparison ends COULD NOT RUN once the files it needs no tool for were read.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_reader_agrees_with_the_developer_tools_otool() {
        let developer = std::fs::read_link("/var/db/xcode_select_link")
            .unwrap_or_else(|_| PathBuf::from("/Library/Developer/CommandLineTools"));
        let me = std::env::current_exe().unwrap();
        let mut files = vec![PathBuf::from("/bin/ls"), me];
        for root in ["", "Toolchains/XcodeDefault.xctoolchain/"] {
            let lib = developer.join(root).join("usr/lib");
            let mut libs = vec![
                lib.join("libLTO.dylib"),
                lib.join("swift-5.0/watchos/libswiftCore.dylib"),
            ];
            for version in std::fs::read_dir(lib.join("clang")).into_iter().flatten() {
                let darwin = version.map(|v| v.path().join("lib/darwin"));
                if let Ok(darwin) = darwin {
                    libs.push(darwin.join("libclang_rt.asan_watchos_dynamic.dylib"));
                    libs.push(darwin.join("libclang_rt.osx.a"));
                }
            }
            files.extend(libs.into_iter().filter(|p| p.is_file()));
        }
        for file in &files {
            load_names_at(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        }
        let Some(otool) = [
            "usr/bin/otool",
            "Toolchains/XcodeDefault.xctoolchain/usr/bin/otool",
        ]
        .iter()
        .map(|rel| developer.join(rel))
        .find(|p| p.is_file()) else {
            aterm_uds::host::could_not_run_without(&["otool"]);
            return;
        };
        let run = |flag: &str, file: &Path| -> Vec<String> {
            let out = aterm_uds::spawnfd::command(&otool)
                .args(["-arch", "all", flag])
                .arg(file)
                .output()
                .expect("the developer tools' otool runs");
            assert!(out.status.success(), "otool {flag} {}", file.display());
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter_map(|line| match flag {
                    "-l" => line.strip_prefix("path "),
                    _ => (!line.ends_with(':') && !line.starts_with("Archive : ")).then_some(line),
                })
                .map(|name| name.split(" (").next().unwrap_or(name).to_owned())
                .collect()
        };
        let set = |names: Vec<String>| -> Vec<String> {
            let mut names = names;
            names.sort();
            names.dedup();
            names
        };
        let mut rpaths_seen = 0;
        for file in &files {
            let names = load_names_at(file).unwrap();
            let shown = file.display();
            assert_eq!(set(names.id.clone()), set(run("-D", file)), "-D {shown}");
            let listed = [names.id.clone(), names.loads.clone()].concat();
            assert_eq!(set(listed), set(run("-L", file)), "-L {shown}");
            let rpaths = run("-l", file);
            assert_eq!(set(names.rpaths.clone()), set(rpaths), "-l {shown}");
            rpaths_seen += names.rpaths.len();
        }
        assert!(
            files.len() < 3 || rpaths_seen > 0,
            "libLTO.dylib carries a run path: the -l comparison compared something"
        );
    }

    #[test]
    fn only_the_os_roots_are_system_dylibs() {
        for ok in [
            "/usr/lib/libSystem.B.dylib",
            "/System/Library/Frameworks/Security.framework/Versions/A/Security",
        ] {
            assert!(is_system_dylib(ok), "{ok}");
        }
        for foreign in [
            "/opt/homebrew/lib/libpcre2-32.dylib",
            "/usr/local/lib/libpcre2-32.dylib",
            "@rpath/libpcre2-32.dylib",
            "@loader_path/../lib/x.dylib",
            "@executable_path/x.dylib",
            "/usr/lib/../../opt/homebrew/lib/libpcre2-32.dylib",
            "/System/./x.dylib",
            "/usr/libexec/x.dylib",
            "usr/lib/libSystem.B.dylib",
        ] {
            assert!(!is_system_dylib(foreign), "{foreign}");
        }
    }

    #[test]
    fn malformed_load_commands_are_refused() {
        let good = homebrew_pcre2_fish(true);
        let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
        let mut zero = good.clone();
        zero[36..40].copy_from_slice(&0u32.to_le_bytes());
        cases.push(("cmdsize 0", zero));
        let mut odd = good.clone();
        odd[36..40].copy_from_slice(&12u32.to_le_bytes());
        cases.push(("cmdsize not a multiple of 8", odd));
        let mut long = good.clone();
        long[36..40].copy_from_slice(&4096u32.to_le_bytes());
        cases.push(("cmdsize past sizeofcmds", long));
        let mut count = good.clone();
        count[16..20].copy_from_slice(&5u32.to_le_bytes());
        cases.push(("ncmds past sizeofcmds", count));
        let mut sizeof = good.clone();
        sizeof[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        cases.push(("sizeofcmds past the file", sizeof));
        cases.push(("truncated header", good[..20].to_vec()));
        let dylib_at = HEADER_64 + 32 + 24;
        let mut low = good.clone();
        low[dylib_at + 8..dylib_at + 12].copy_from_slice(&8u32.to_le_bytes());
        cases.push(("name offset inside the fixed part", low));
        let mut high = good.clone();
        high[dylib_at + 8..dylib_at + 12].copy_from_slice(&4096u32.to_le_bytes());
        cases.push(("name offset past the command", high));
        let mut unterminated = good.clone();
        let size =
            u32::from_le_bytes(good[dylib_at + 4..dylib_at + 8].try_into().unwrap()) as usize;
        for b in &mut unterminated[dylib_at + DYLIB_COMMAND..dylib_at + size] {
            *b = b'a';
        }
        cases.push(("name without a NUL", unterminated));
        let mut empty = good.clone();
        for b in &mut empty[dylib_at + DYLIB_COMMAND..dylib_at + size] {
            *b = 0;
        }
        cases.push(("empty name", empty));
        let mut short_dylib = thin(&[other_command(0xC, 16, true)], true);
        short_dylib.truncate(HEADER_64 + 16 + 8);
        cases.push(("dylib command under its fixed part", short_dylib));
        cases.push(("not a Mach-O", b"#!/bin/sh\necho hi\n".to_vec()));
        cases.push(("empty arch table", fat(&[], false)));
        let mut many = fat(std::slice::from_ref(&good), false);
        many[4..8].copy_from_slice(&(MAX_SLICES + 1).to_be_bytes());
        cases.push(("too many slices", many));
        let mut outside = fat(std::slice::from_ref(&good), false);
        outside[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        cases.push(("slice outside the file", outside));
        let mut overlap = fat(std::slice::from_ref(&good), true);
        overlap[16..24].copy_from_slice(&0u64.to_be_bytes());
        cases.push(("slice over the header", overlap));
        cases.push((
            "universal inside universal",
            fat(&[fat(std::slice::from_ref(&good), false)], false),
        ));
        let mut swapped = fat(std::slice::from_ref(&good), false);
        swapped[..4].copy_from_slice(&0xBEBA_FECAu32.to_be_bytes());
        cases.push(("byte-swapped universal header", swapped));
        cases.push((
            "a Java class file",
            vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 0x34, 0, 0],
        ));
        for (what, bytes) in cases {
            match load_dylibs(&bytes) {
                Err(LoadError::Malformed(_)) => {}
                other => panic!("{what}: {other:?}"),
            }
        }
        assert!(load_dylibs(&good).is_ok(), "the unmutated fixture reads");
    }

    /// A real binary reads: the test binary itself and Apple's universal `/bin/ls` each
    /// load libSystem and nothing outside the OS.
    #[cfg(target_os = "macos")]
    #[test]
    fn real_binaries_read_as_self_contained() {
        let me = std::env::current_exe().unwrap();
        for path in [me.as_path(), Path::new("/bin/ls")] {
            let names = load_dylibs_at(path).unwrap();
            assert!(
                names.iter().any(|n| n == "/usr/lib/libSystem.B.dylib"),
                "{}: {names:?}",
                path.display()
            );
        }
        assert_eq!(
            foreign_dylibs(Path::new("/bin/ls")).unwrap(),
            Vec::<String>::new()
        );
        assert!(matches!(
            load_dylibs_at(Path::new("/bin")),
            Err(LoadError::Io(_))
        ));
    }

    /// A FIFO is refused without being opened — reading one would block the stage.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_being_read() {
        let d = tmp("fifo");
        let fifo = d.join("claude");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(matches!(find_macho(&d), Err(StageError::SignerRefused(_))));
        let _ = std::fs::remove_dir_all(&d);
    }
}
