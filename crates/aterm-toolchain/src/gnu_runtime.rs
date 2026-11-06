// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GNU libc requirements read from ELF's dynamic version-needs table, without executing it.
//! Program headers own these reads: a stripped object may carry no section headers at all.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// A dotted GNU libc version, compared numerically, including its optional patch component.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version([u32; 3]);

impl Version {
    /// Admit the dotted version returned by GNU libc or required by an ELF symbol.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let parts: Vec<_> = text.split('.').collect();
        if !(2..=3).contains(&parts.len()) {
            return None;
        }
        let mut version = [0; 3];
        for (at, part) in parts.iter().enumerate() {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            version[at] = part.parse().ok()?;
        }
        Some(Self(version))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.0[0], self.0[1])?;
        if self.0[2] != 0 {
            write!(f, ".{}", self.0[2])?;
        }
        Ok(())
    }
}

/// The native GNU host whose runtime a ready build must be able to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Host {
    /// ELF's machine identifier: 62 for x86_64, 183 for AArch64.
    pub machine: u16,
    /// The GNU libc loaded in the admitting process.
    pub glibc: Version,
}

/// The GNU host, or none on a different operating system/runtime. No child is started.
///
/// # Errors
/// The GNU host's architecture or public libc version cannot be read.
pub fn host() -> Result<Option<Host>, String> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn gnu_get_libc_version() -> *const std::ffi::c_char;
        }
        let machine = machine_for_arch(std::env::consts::ARCH)
            .ok_or("this GNU host's ELF architecture is unsupported")?;
        // SAFETY: GNU libc documents a pointer to its static, NUL-terminated version string.
        let pointer = unsafe { gnu_get_libc_version() };
        if pointer.is_null() {
            return Err("GNU libc returned no runtime version".into());
        }
        // SAFETY: the non-null pointer belongs to the loaded GNU libc, as above.
        let version = unsafe { std::ffi::CStr::from_ptr(pointer) }
            .to_str()
            .ok()
            .and_then(Version::parse)
            .ok_or("GNU libc returned an unrecognized runtime version")?;
        Ok(Some(Host {
            machine,
            glibc: version,
        }))
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        Ok(None)
    }
}

/// ELF's machine identifier for the two GNU targets the toolset publishes.
#[must_use]
pub fn machine_for_arch(arch: &str) -> Option<u16> {
    match arch {
        "x86_64" => Some(62),
        "aarch64" => Some(183),
        _ => None,
    }
}

/// One runtime ELF's architecture and highest required numeric GNU libc symbol version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirements {
    /// ELF's machine identifier.
    pub machine: u16,
    /// None for an object with no GNU libc version requirements, including a static program.
    pub glibc: Option<Version>,
}

impl Requirements {
    /// Require this object to fit the native host, without starting the program or loader.
    ///
    /// # Errors
    /// Its machine differs or it needs a newer GNU libc.
    pub fn admit(self, host: Host) -> Result<(), String> {
        if self.machine != host.machine {
            return Err("the ELF object is for another CPU architecture".into());
        }
        if let Some(required) = self.glibc.filter(|required| *required > host.glibc) {
            return Err(format!(
                "requires GNU libc {required}; this host has {}",
                host.glibc
            ));
        }
        Ok(())
    }
}

struct Reader<'a, R> {
    input: &'a mut R,
    size: u64,
}

impl<R: Read + Seek> Reader<'_, R> {
    fn bytes(&mut self, at: u64, count: usize) -> Result<Vec<u8>, String> {
        if at
            .checked_add(count as u64)
            .is_none_or(|end| end > self.size)
        {
            return Err("ELF metadata extends past the file".into());
        }
        let mut bytes = vec![0; count];
        self.input
            .seek(SeekFrom::Start(at))
            .map_err(|_| "cannot seek ELF metadata")?;
        self.input
            .read_exact(&mut bytes)
            .map_err(|_| "cannot read complete ELF metadata")?;
        Ok(bytes)
    }
}

fn half(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().expect("bounded ELF structure"))
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("bounded ELF structure"))
}

fn wide(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("bounded ELF structure"))
}

#[derive(Clone, Copy)]
struct Load {
    address: u64,
    offset: u64,
    size: u64,
    memory: u64,
}

fn file_offset(loads: &[Load], address: u64, size: usize) -> Result<u64, String> {
    let offset = loads
        .iter()
        .find_map(|load| {
            let within = address.checked_sub(load.address)?;
            (within.checked_add(size as u64)? <= load.size)
                .then(|| load.offset.checked_add(within))
                .flatten()
        })
        .ok_or("ELF runtime metadata is outside its load segments")?;
    let end = address
        .checked_add(size as u64)
        .ok_or("ELF runtime address overflow")?;
    for load in loads {
        let first = address.max(load.address);
        let last = end.min(
            load.address
                .checked_add(load.memory)
                .ok_or("ELF load range overflow")?,
        );
        if first >= last {
            continue;
        }
        let within = first - load.address;
        if last - load.address > load.size
            || load.offset.checked_add(within) != offset.checked_add(first - address)
        {
            return Err("ELF runtime metadata has conflicting load segments".into());
        }
    }
    Ok(offset)
}

fn name(strings: &[u8], at: u32) -> Result<&str, String> {
    let suffix = strings
        .get(at as usize..)
        .ok_or("ELF version string is outside its table")?;
    let end = suffix
        .iter()
        .position(|byte| *byte == 0)
        .ok_or("ELF version string is unterminated")?;
    std::str::from_utf8(&suffix[..end]).map_err(|_| "ELF version string is not UTF-8".into())
}

fn unique(slot: &mut Option<u64>, value: u64) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err("ELF dynamic metadata has duplicate declarations".into());
    }
    Ok(())
}

/// Inspect a bounded, seekable file. Non-ELF and relocatable object files return none.
/// Runtime ELF64 little-endian objects are read through PT_DYNAMIC, including without sections.
///
/// # Errors
/// A runtime ELF has malformed, ambiguous, oversized or unrecognized requirement metadata.
pub fn inspect(input: &mut (impl Read + Seek), size: u64) -> Result<Option<Requirements>, String> {
    let mut reader = Reader { input, size };
    if size < 4 || reader.bytes(0, 4)? != b"\x7fELF" {
        return Ok(None);
    }
    let ident = reader.bytes(0, 16)?;
    let (header_size, size_at) = match ident[4] {
        1 => (52, 40),
        2 => (64, 52),
        _ => return Err("unsupported ELF class".into()),
    };
    let header = reader.bytes(0, header_size)?;
    let (kind, version, declared_size) = match ident[5] {
        1 => (half(&header, 16), word(&header, 20), half(&header, size_at)),
        2 => (
            u16::from_be_bytes(header[16..18].try_into().expect("bounded ELF header")),
            u32::from_be_bytes(header[20..24].try_into().expect("bounded ELF header")),
            u16::from_be_bytes(
                header[size_at..size_at + 2]
                    .try_into()
                    .expect("bounded ELF header"),
            ),
        ),
        _ => return Err("unsupported ELF byte order".into()),
    };
    if ident[6] != 1 || version != 1 || usize::from(declared_size) != header_size {
        return Err("malformed ELF header".into());
    }
    if kind == 1 {
        return Ok(None); // ET_REL is not a runtime-loaded object, including cross-target inputs.
    }
    if ident[4..6] != [2, 1] {
        return Err("unsupported or malformed ELF64 little-endian header".into());
    }
    match kind {
        2 | 3 => {}
        _ => return Err("unsupported ELF object type".into()),
    }
    let mut requirements = Requirements {
        machine: half(&header, 18),
        glibc: None,
    };
    let count = usize::from(half(&header, 56));
    if count > 8192 || (count != 0 && half(&header, 54) != 56) {
        return Err("unsupported ELF program-header table".into());
    }
    let headers = reader.bytes(wide(&header, 32), count * 56)?;
    let mut loads = Vec::new();
    let mut dynamic = None;
    for entry in headers.as_chunks::<56>().0 {
        if word(entry, 0) == 0 {
            continue; // PT_NULL's remaining fields are undefined (gABI program headers).
        }
        let offset = wide(entry, 8);
        let size = wide(entry, 32);
        if offset.checked_add(size).is_none_or(|end| end > reader.size) {
            return Err("ELF program segment extends past the file".into());
        }
        match word(entry, 0) {
            1 => {
                if wide(entry, 40) < size || wide(entry, 16).checked_add(wide(entry, 40)).is_none()
                {
                    return Err("ELF load segment has an invalid memory range".into());
                }
                loads.push(Load {
                    address: wide(entry, 16),
                    offset,
                    size,
                    memory: wide(entry, 40),
                });
            }
            2 if dynamic.replace((offset, wide(entry, 16), size)).is_some() => {
                return Err("duplicate ELF dynamic segment".into());
            }
            _ => {}
        }
    }
    let Some((offset, address, size)) = dynamic else {
        return Ok(Some(requirements));
    };
    let size = usize::try_from(size)
        .ok()
        .filter(|size| *size <= 1024 * 1024 && size % 16 == 0)
        .ok_or("ELF dynamic table is oversized or misaligned")?;
    if file_offset(&loads, address, size)? != offset {
        return Err("ELF dynamic segment disagrees with its mapped address".into());
    }
    let dynamic = reader.bytes(offset, size)?;
    let (mut strings_at, mut strings_size, mut needs_at, mut needs_count) =
        (None, None, None, None);
    let mut ended = false;
    for entry in dynamic.as_chunks::<16>().0 {
        let value = wide(entry, 8);
        match wide(entry, 0) {
            0 => {
                ended = true;
                break;
            }
            5 => unique(&mut strings_at, value)?,
            10 => unique(&mut strings_size, value)?,
            0x6fff_fffe => unique(&mut needs_at, value)?,
            0x6fff_ffff => unique(&mut needs_count, value)?,
            _ => {}
        }
    }
    if !ended {
        return Err("ELF dynamic table has no terminator".into());
    }
    let (needs_at, needs_count) = match (needs_at, needs_count) {
        (None, None) => return Ok(Some(requirements)),
        (Some(at), Some(count)) if (1..=4096).contains(&count) => (at, count),
        _ => return Err("ELF version-needs declarations are incomplete".into()),
    };
    let (Some(strings_at), Some(strings_size)) = (strings_at, strings_size) else {
        return Err("ELF version needs have no complete string table".into());
    };
    let strings_size = usize::try_from(strings_size)
        .ok()
        .filter(|size| *size <= 16 * 1024 * 1024)
        .ok_or("ELF dynamic string table is oversized")?;
    let strings = reader.bytes(file_offset(&loads, strings_at, strings_size)?, strings_size)?;
    let mut at = needs_at;
    let mut total = 0_u32;
    for index in 0..needs_count {
        let need = reader.bytes(file_offset(&loads, at, 16)?, 16)?;
        let count = u32::from(half(&need, 2));
        if half(&need, 0) != 1 || count == 0 || total.checked_add(count).is_none_or(|n| n > 65_536)
        {
            return Err("unsupported or oversized ELF version-needs list".into());
        }
        total += count;
        if name(&strings, word(&need, 4))?.is_empty() {
            return Err("ELF version dependency has no library name".into());
        }
        let auxiliary = word(&need, 8);
        if auxiliary < 16 {
            return Err("ELF version-needs auxiliary record overlaps its header".into());
        }
        let mut aux_at = at
            .checked_add(u64::from(auxiliary))
            .ok_or("ELF version-needs address overflow")?;
        for auxiliary_index in 0..count {
            let aux = reader.bytes(file_offset(&loads, aux_at, 16)?, 16)?;
            let version = name(&strings, word(&aux, 8))?;
            // VER_FLG_WEAK is a warning rather than a loader requirement (LSB symbol
            // versioning). Do not invent a hard host floor from an optional dependency.
            if let Some(version) = version
                .strip_prefix("GLIBC_")
                .filter(|_| half(&aux, 4) & 2 == 0)
            {
                let version = Version::parse(version)
                    .filter(|parsed| parsed.to_string() == version)
                    .ok_or_else(|| {
                        format!("unrecognized GNU libc ABI requirement GLIBC_{version}")
                    })?;
                requirements.glibc =
                    Some(requirements.glibc.map_or(version, |old| old.max(version)));
            }
            let next = word(&aux, 12);
            if auxiliary_index + 1 == count {
                if next != 0 {
                    return Err("ELF version-needs auxiliary count disagrees with its chain".into());
                }
            } else if next < 16 {
                return Err("ELF version-needs auxiliary chain ends early or overlaps".into());
            } else {
                aux_at = aux_at
                    .checked_add(u64::from(next))
                    .ok_or("ELF auxiliary address overflow")?;
            }
        }
        let next = word(&need, 12);
        if index + 1 == needs_count {
            if next != 0 {
                return Err("ELF version-needs count disagrees with its chain".into());
            }
        } else if next < 16 {
            return Err("ELF version-needs chain ends early or overlaps".into());
        } else {
            at = at
                .checked_add(u64::from(next))
                .ok_or("ELF version-needs address overflow")?;
        }
    }
    Ok(Some(requirements))
}

/// The aggregate inspected runtime requirements of a package tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    /// Runtime ELF files inspected, including shared libraries without execute bits.
    pub objects: u64,
    /// The highest numeric GNU libc requirement found.
    pub glibc: Option<Version>,
}

/// Inspect all regular files in a tree for the declared machine, following no symlink.
/// This is publication inspection, not permission to write a host's ready marker.
///
/// # Errors
/// The tree or an ELF file cannot be read consistently, has another machine, or is malformed.
pub fn inspect_tree(root: &Path, machine: u16) -> Result<Tree, String> {
    let before = walk(root)?;
    let tree = inspect_files(&before.files, machine)?;
    if walk(root)?.digest != before.digest {
        return Err("runtime tree changed while reading".into());
    }
    Ok(tree)
}

fn inspect_files(files: &[PathBuf], machine: u16) -> Result<Tree, String> {
    let mut tree = Tree::default();
    for path in files {
        if let Some(required) = inspect_file(path)? {
            if required.machine != machine {
                return Err(format!(
                    "{} is for another ELF architecture",
                    path.display()
                ));
            }
            tree.objects += 1;
            tree.glibc = match (tree.glibc, required.glibc) {
                (Some(old), Some(new)) => Some(old.max(new)),
                (old, new) => old.or(new),
            };
        }
    }
    Ok(tree)
}

struct Walk {
    files: Vec<PathBuf>,
    digest: [u8; 32],
    settled: bool,
}

fn walk(root: &Path) -> Result<Walk, String> {
    use crate::stat_digest::{StatDigest, StatKey};
    let begun = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "runtime status clock is before the Unix epoch")?;
    let begun = i128::try_from(begun.as_nanos()).map_err(|_| "runtime status clock overflow")?;
    let mut digest = StatDigest::new(root.as_os_str().as_encoded_bytes());
    let mut files = Vec::new();
    let mut pending = vec![root.to_owned()];
    let mut count = 0_u64;
    while let Some(path) = pending.pop() {
        count += 1;
        if count > 4_000_000 {
            return Err("runtime inspection has too many entries".into());
        }
        let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "runtime path escaped its root")?;
        digest.entry(relative.as_os_str().as_encoded_bytes(), &StatKey::of(&meta));
        if meta.file_type().is_symlink() {
            if path == root {
                return Err("runtime inspection root is a symlink".into());
            }
            let target = std::fs::read_link(&path).map_err(|e| e.to_string())?;
            digest.link(target.as_os_str().as_encoded_bytes(), None);
        } else if meta.is_dir() {
            let mut names = std::fs::read_dir(&path)
                .map_err(|e| e.to_string())?
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            names.sort();
            digest.names(names.iter().map(|name| name.as_encoded_bytes()));
            pending.extend(names.into_iter().rev().map(|name| path.join(name)));
        } else if meta.is_file() {
            if path == root {
                return Err("runtime inspection root is no directory".into());
            }
            files.push(path);
        } else {
            return Err("runtime tree contains a special file".into());
        }
    }
    let settled = cfg!(unix)
        && digest
            .newest_change_ns()
            .is_some_and(|newest| begun.saturating_sub(newest) >= crate::stat_digest::SETTLE_NS);
    Ok(Walk {
        files,
        digest: digest.finish().0,
        settled,
    })
}

#[derive(Clone)]
struct Cached {
    root: PathBuf,
    host: Host,
    digest: [u8; 32],
}

// Cache only successful, settled reads. Each look still records every entry's inode,
// size, nanosecond mtime/ctime, type, names and symlink target. A fresh tree gets no sleep:
// it is inspected again until status stamps can safely distinguish subsequent writes.
fn admissions() -> &'static std::sync::Mutex<Vec<Cached>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Vec<Cached>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

fn inspect_file(path: &Path) -> Result<Option<Requirements>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    inspect_open_file(&file).map_err(|why| format!("{}: {why}", path.display()))
}

fn inspect_open_file(file: &std::fs::File) -> Result<Option<Requirements>, String> {
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() {
        return Err("runtime input is no regular file".into());
    }
    let mut input = Positioned { file, at: 0 };
    let inspected = inspect(&mut input, before.len())?;
    let after = file.metadata().map_err(|e| e.to_string())?;
    if !crate::stat_digest::StatKey::of(&before).unchanged(&crate::stat_digest::StatKey::of(&after))
    {
        return Err("runtime input changed while reading".into());
    }
    Ok(inspected)
}

struct Positioned<'a> {
    file: &'a std::fs::File,
    at: u64,
}

impl Read for Positioned<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        #[cfg(unix)]
        let count = {
            use std::os::unix::fs::FileExt as _;
            self.file.read_at(bytes, self.at)?
        };
        #[cfg(windows)]
        let count = {
            use std::os::windows::fs::FileExt as _;
            self.file.seek_read(bytes, self.at)?
        };
        #[cfg(not(any(unix, windows)))]
        let count = {
            let mut file = self.file.try_clone()?;
            file.seek(SeekFrom::Start(self.at))?;
            file.read(bytes)?
        };
        self.at = self
            .at
            .checked_add(count as u64)
            .ok_or(std::io::ErrorKind::InvalidInput)?;
        Ok(count)
    }
}

impl Seek for Positioned<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let at = match position {
            SeekFrom::Start(at) => i128::from(at),
            SeekFrom::Current(offset) => i128::from(self.at) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.file.metadata()?.len()) + i128::from(offset),
        };
        self.at = u64::try_from(at).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        Ok(self.at)
    }
}

/// Admit an already-held file, without resolving its path again. Used by descriptor-held
/// local publication after its own byte gate and before it becomes a ready build.
///
/// # Errors
/// [`inspect`]'s failures, a changing file, a different machine or a newer GNU libc.
pub fn admit_open_file(file: &std::fs::File, host: Host) -> Result<(), String> {
    if let Some(required) = inspect_open_file(file)? {
        required.admit(host)?;
    }
    Ok(())
}

/// Admit a package tree to this GNU host; other runtimes have no GNU libc requirement.
/// No executable or dynamic loader is started.
///
/// # Errors
/// An ELF requirement is unavailable, malformed, for another machine, or newer than this host.
pub fn admit_tree(root: &Path) -> Result<(), String> {
    let Some(host) = host()? else {
        return Ok(());
    };
    admit_tree_on(root, host)
}

/// The same admission for an explicitly observed GNU host, also used by inert fixtures.
///
/// # Errors
/// [`admit_tree`]'s failures for the supplied host.
pub fn admit_tree_on(root: &Path, host: Host) -> Result<(), String> {
    let before = walk(root)?;
    if before.settled
        && admissions()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|cached| {
                cached.root == root && cached.host == host && cached.digest == before.digest
            })
    {
        return Ok(());
    }
    let tree = inspect_files(&before.files, host.machine)?;
    if let Some(required) = tree.glibc.filter(|required| *required > host.glibc) {
        return Err(format!(
            "requires GNU libc {required}; this host has {}",
            host.glibc
        ));
    }
    let after = walk(root)?;
    if after.digest != before.digest {
        return Err("runtime tree changed while reading".into());
    }
    if before.settled && after.settled {
        let mut cache = admissions()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.retain(|cached| cached.root != root);
        if cache.len() == 32 {
            cache.remove(0);
        }
        cache.push(Cached {
            root: root.to_owned(),
            host,
            digest: after.digest,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn put16(bytes: &mut [u8], at: usize, value: u16) {
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn put32(bytes: &mut [u8], at: usize, value: u32) {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn put64(bytes: &mut [u8], at: usize, value: u64) {
        bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
    fn header(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0; 64];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        put16(&mut bytes, 16, 2);
        put16(&mut bytes, 18, machine);
        put32(&mut bytes, 20, 1);
        put16(&mut bytes, 52, 64);
        put16(&mut bytes, 54, 56);
        bytes
    }
    fn dynamic(version: &str) -> Vec<u8> {
        let mut bytes = header(62);
        bytes.resize(1024, 0);
        put16(&mut bytes, 16, 3);
        put64(&mut bytes, 32, 64);
        put16(&mut bytes, 56, 2);
        put32(&mut bytes, 64, 1); // PT_LOAD covers all runtime metadata.
        put64(&mut bytes, 80, 0x40_0000);
        put64(&mut bytes, 96, 1024);
        put64(&mut bytes, 104, 1024);
        put32(&mut bytes, 120, 2); // PT_DYNAMIC, with no section table at all.
        put64(&mut bytes, 128, 256);
        put64(&mut bytes, 136, 0x40_0100);
        put64(&mut bytes, 152, 80);
        let strings = format!("\0libc.so.6\0{version}\0").into_bytes();
        for (index, (tag, value)) in [
            (5, 0x40_0200),
            (10, strings.len() as u64),
            (0x6fff_fffe, 0x40_0280),
            (0x6fff_ffff, 1),
            (0, 0),
        ]
        .into_iter()
        .enumerate()
        {
            put64(&mut bytes, 256 + index * 16, tag);
            put64(&mut bytes, 264 + index * 16, value);
        }
        bytes[512..512 + strings.len()].copy_from_slice(&strings);
        put16(&mut bytes, 640, 1);
        put16(&mut bytes, 642, 1);
        put32(&mut bytes, 644, 1);
        put32(&mut bytes, 648, 16);
        put16(&mut bytes, 662, 2);
        put32(&mut bytes, 664, 11);
        bytes
    }
    fn parsed(bytes: &[u8]) -> Result<Option<Requirements>, String> {
        inspect(&mut Cursor::new(bytes), bytes.len() as u64)
    }
    fn host(version: &str) -> Host {
        Host {
            machine: 62,
            glibc: Version::parse(version).unwrap(),
        }
    }

    #[test]
    fn dynamic_needs_without_sections_refuse_an_older_host_and_accept_a_newer_one() {
        let mut bytes = dynamic("GLIBC_2.39");
        bytes[900..910].copy_from_slice(b"GLIBC_99.0");
        let requirements = parsed(&bytes).unwrap().unwrap();
        assert_eq!(requirements.glibc, Version::parse("2.39"));
        assert!(
            requirements
                .admit(host("2.35"))
                .unwrap_err()
                .contains("2.39")
        );
        assert_eq!(requirements.admit(host("2.39")), Ok(()));
        assert_eq!(requirements.admit(host("2.40")), Ok(()));
        assert!(
            requirements
                .admit(Host {
                    machine: 183,
                    ..host("2.40")
                })
                .is_err()
        );
        assert!(Version::parse("2.9") < Version::parse("2.35"));
    }

    #[test]
    fn ordinary_data_static_programs_and_other_libraries_do_not_invent_libc_requirements() {
        assert_eq!(parsed(b"README contains GLIBC_99.0"), Ok(None));
        let static_elf = parsed(&header(62)).unwrap().unwrap();
        assert_eq!(static_elf.glibc, None);
        assert_eq!(static_elf.admit(host("2.17")), Ok(()));
        let mut with_null = dynamic("GLIBC_2.17");
        put16(&mut with_null, 56, 3);
        with_null[180..232].fill(0xff);
        assert_eq!(
            parsed(&with_null).unwrap().unwrap().admit(host("2.17")),
            Ok(())
        );
        assert_eq!(
            parsed(&dynamic("OPENSSL_3.0.0")).unwrap().unwrap().glibc,
            None
        );
        let mut object = header(183);
        put16(&mut object, 16, 1);
        assert_eq!(parsed(&object), Ok(None));
        let mut object32 = vec![0; 52];
        object32[..7].copy_from_slice(b"\x7fELF\x01\x02\x01");
        object32[16..18].copy_from_slice(&1_u16.to_be_bytes());
        object32[20..24].copy_from_slice(&1_u32.to_be_bytes());
        object32[40..42].copy_from_slice(&52_u16.to_be_bytes());
        assert_eq!(parsed(&object32), Ok(None));
        object32[16..18].copy_from_slice(&2_u16.to_be_bytes());
        assert!(
            parsed(&object32).is_err(),
            "foreign runtime ELF stays refused"
        );
        let mut optional = dynamic("GLIBC_2.39");
        put16(&mut optional, 660, 2);
        assert_eq!(
            parsed(&optional).unwrap().unwrap().admit(host("2.17")),
            Ok(())
        );
    }

    #[test]
    fn incomplete_ambiguous_or_unrecognized_runtime_metadata_never_means_compatible() {
        let good = dynamic("GLIBC_2.39");
        for cut in [4, 63, 120, 256, 645, 1023] {
            assert!(parsed(&good[..cut]).is_err(), "truncated at {cut}");
        }
        let mut variants = Vec::new();
        let mut duplicate = good.clone();
        put64(&mut duplicate, 288, 5);
        variants.push(duplicate);
        let mut early = good.clone();
        put16(&mut early, 642, 2);
        variants.push(early);
        let mut past = good.clone();
        put32(&mut past, 664, 65_535);
        variants.push(past);
        let mut incomplete = good.clone();
        put64(&mut incomplete, 304, 0);
        variants.push(incomplete);
        let mut overlap = good.clone();
        overlap.resize(2048, 0);
        put16(&mut overlap, 56, 3);
        let load = overlap[64..120].to_vec();
        overlap[176..232].copy_from_slice(&load);
        put64(&mut overlap, 184, 1024);
        variants.push(overlap);
        let mut misleading_dynamic = good.clone();
        put64(&mut misleading_dynamic, 136, 0x40_0300);
        variants.push(misleading_dynamic);
        variants.push(dynamic("GLIBC_PRIVATE"));
        variants.push(dynamic("GLIBC_2.39.invalid"));
        variants.push(dynamic("GLIBC_02.39"));
        variants.push(dynamic("GLIBC_2.39.0"));
        for bytes in variants {
            assert!(parsed(&bytes).is_err());
        }
    }

    #[test]
    fn even_a_partial_load_overlap_must_preserve_metadata_byte_authority() {
        let mut matching = dynamic("GLIBC_2.39");
        matching.resize(2048, 0);
        put16(&mut matching, 56, 3);
        put32(&mut matching, 176, 1);
        put64(&mut matching, 184, 525);
        put64(&mut matching, 192, 0x40_020d);
        put64(&mut matching, 208, 2);
        put64(&mut matching, 216, 2);
        assert_eq!(
            parsed(&matching).unwrap().unwrap().glibc,
            Version::parse("2.39")
        );
        let mut conflicting = matching.clone();
        put64(&mut conflicting, 184, 1024);
        assert!(
            parsed(&conflicting).is_err(),
            "partial different-file alias"
        );
        let mut zero_fill = matching;
        put64(&mut zero_fill, 208, 0);
        assert!(parsed(&zero_fill).is_err(), "partial zero-filled alias");
    }

    #[test]
    fn shared_libraries_without_execute_bits_are_admitted_with_the_whole_tree() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        std::fs::create_dir(scratch.path().join("lib")).unwrap();
        let library = scratch.path().join("lib/runtime.so");
        std::fs::write(&library, dynamic("GLIBC_2.39")).unwrap();
        assert!(admit_tree_on(scratch.path(), host("2.35")).is_err());
        assert_eq!(admit_tree_on(scratch.path(), host("2.39")), Ok(()));
        assert_eq!(inspect_tree(scratch.path(), 62).unwrap().objects, 1);
        let mut file = std::fs::File::open(&library).unwrap();
        file.seek(SeekFrom::Start(5)).unwrap();
        assert!(admit_open_file(&file, host("2.35")).is_err());
        assert_eq!(admit_open_file(&file, host("2.39")), Ok(()));
        assert_eq!(
            file.stream_position().unwrap(),
            5,
            "inspection preserves the retained descriptor's cursor"
        );
    }
}
