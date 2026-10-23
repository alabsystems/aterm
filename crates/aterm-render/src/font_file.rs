// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Safe, bounded admission for explicit font-file paths.

use std::borrow::Cow;
use std::io::{self, Read as _};
use std::path::Path;
use std::sync::Arc;

/// Largest external font blob admitted.
pub const MAX_FONT_FILE_BYTES: usize = 256 * 1024 * 1024;

fn limit_plus_sentinel(max_bytes: usize) -> io::Result<usize> {
    max_bytes.checked_add(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "font-file byte limit cannot be represented",
        )
    })
}

fn too_large(max_bytes: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("font file exceeds the {max_bytes}-byte limit"),
    )
}

fn not_regular() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "font path is not a regular non-link file",
    )
}

/// Open once and prove that the handle which will be read is a regular file,
/// returning it with the path that was actually opened.
#[cfg(unix)]
fn open_regular(path: &Path) -> io::Result<(std::fs::File, Cow<'_, Path>)> {
    match open_regular_no_follow(path) {
        Ok(file) => Ok((file, Cow::Borrowed(path))),
        Err(error) => {
            // Only a final-component LINK is retried; every other refusal
            if !std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(error);
            }
            let target = std::fs::canonicalize(path)?;
            let file = open_regular_no_follow(&target)?;
            Ok((file, Cow::Owned(target)))
        }
    }
}

/// The one `open` [`open_regular`] performs per path: never through a
/// final-component link, never blocking, and only a regular file is kept.
#[cfg(unix)]
fn open_regular_no_follow(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

/// Windows' reparse-point flag is the closest equivalent of `O_NOFOLLOW`.
#[cfg(windows)]
fn open_regular(path: &Path) -> io::Result<(std::fs::File, Cow<'_, Path>)> {
    let file = open_regular_shared(
        path,
        win::FILE_SHARE_READ | win::FILE_SHARE_WRITE | win::FILE_SHARE_DELETE,
    )?;
    Ok((file, Cow::Borrowed(path)))
}

/// [`open_regular`] with an explicit share mode: the copy path shares everything (std's
/// default); the mapping path ([`open_for_admission`]) refuses write sharing.
#[cfg(windows)]
fn open_regular_shared(path: &Path, share: u32) -> io::Result<std::fs::File> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(share)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(not_regular());
    }
    Ok(file)
}

/// Portable fallback.
#[cfg(not(any(unix, windows)))]
fn open_regular(path: &Path) -> io::Result<(std::fs::File, Cow<'_, Path>)> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(not_regular());
    }
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(not_regular());
    }
    Ok((file, Cow::Borrowed(path)))
}

fn check_observed_size(file: &std::fs::File, max_bytes: usize) -> io::Result<u64> {
    let length = file.metadata()?.len();
    if length > u64::try_from(max_bytes).unwrap_or(u64::MAX) {
        return Err(too_large(max_bytes));
    }
    Ok(length)
}

/// Validate an explicit path without reading or parsing its bytes, under the same open/handle/type/size rules as [`read_bounded_font_file`].
pub fn validate_bounded_font_file(path: &Path, max_bytes: usize) -> io::Result<()> {
    let (file, _) = open_regular(path)?;
    let _ = check_observed_size(&file, max_bytes)?;
    Ok(())
}

/// Read one same-handle regular font file into a bounded byte vector.
pub fn read_bounded_font_file(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    read_bounded_font_file_into(path, max_bytes, &mut bytes)?;
    Ok(bytes)
}

/// [`read_bounded_font_file`] into a buffer the caller supplies and reuses.
pub fn read_bounded_font_file_into(
    path: &Path,
    max_bytes: usize,
    buf: &mut Vec<u8>,
) -> io::Result<()> {
    buf.clear();
    let (file, _) = open_regular(path)?;
    let length = check_observed_size(&file, max_bytes)?;
    read_admitted_handle(file, length, max_bytes, buf)
}

/// The read half of [`read_bounded_font_file_into`], over a handle
/// [`open_regular`] + [`check_observed_size`] already admitted, so the mapping
/// path can fall back to the copy without reopening the pathname.
fn read_admitted_handle(
    file: std::fs::File,
    length: u64,
    max_bytes: usize,
    buf: &mut Vec<u8>,
) -> io::Result<()> {
    buf.clear();
    let read_limit = limit_plus_sentinel(max_bytes)?;
    let want = usize::try_from(length).unwrap_or(max_bytes).min(max_bytes);
    if buf.capacity() < want {
        buf.try_reserve_exact(want).map_err(|error| {
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!("font-file allocation refused: {error}"),
            )
        })?;
    }
    file.take(read_limit as u64).read_to_end(buf)?;
    if buf.len() > max_bytes {
        return Err(too_large(max_bytes));
    }
    Ok(())
}

/// Production-font-budget wrapper used by renderer and GUI config seams.
#[inline]
pub fn read_font_file(path: &Path) -> io::Result<Vec<u8>> {
    read_bounded_font_file(path, MAX_FONT_FILE_BYTES)
}

/// One font file mapped whole and read-only, unmapped on drop.
#[cfg(any(unix, windows))]
pub struct MappedFontFile {
    ptr: std::ptr::NonNull<u8>,
    len: usize,
    identity: (u64, u64),
    /// A duplicate of the admission handle, held so its no-write-share mode lasts as long as
    /// the bytes are addressable.
    #[cfg(windows)]
    _file: std::fs::File,
}

/// Elsewhere (neither unix nor Windows) the type is uninhabited.
#[cfg(not(any(unix, windows)))]
pub struct MappedFontFile {
    never: core::convert::Infallible,
}

// SAFETY: the mapping is read-only (unix: PROT_READ and MAP_PRIVATE; Windows: a PAGE_READONLY section viewed FILE_MAP_READ).
#[cfg(any(unix, windows))]
unsafe impl Send for MappedFontFile {}
#[cfg(any(unix, windows))]
unsafe impl Sync for MappedFontFile {}

#[cfg(unix)]
impl MappedFontFile {
    /// Map `len` bytes of an already-admitted handle.
    fn map(file: &std::fs::File, len: u64) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        use std::os::unix::io::AsRawFd as _;

        let Ok(len_usize) = usize::try_from(len) else {
            return Err(too_large(MAX_FONT_FILE_BYTES));
        };
        if len_usize == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "font file is empty",
            ));
        }
        let metadata = file.metadata()?;
        let identity = (metadata.dev(), metadata.ino());
        // SAFETY: `fd` is the caller's open, admitted handle.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len_usize,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let Some(ptr) = std::ptr::NonNull::new(raw.cast::<u8>()) else {
            // A NULL success is impossible with a NULL hint on every unix
            return Err(io::Error::other("mmap returned a null mapping"));
        };
        Ok(Self {
            ptr,
            len: len_usize,
            identity,
        })
    }
}

#[cfg(windows)]
impl MappedFontFile {
    /// Map `len` bytes of an already-admitted handle, read-only, as the unix `map` does: `len`
    /// is the size [`check_observed_size`] bounded, and an empty file (which
    /// `CreateFileMappingW` refuses) is refused up front.
    fn map(file: &std::fs::File, len: u64) -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle as _;

        let Ok(len_usize) = usize::try_from(len) else {
            return Err(too_large(MAX_FONT_FILE_BYTES));
        };
        if len_usize == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "font file is empty",
            ));
        }
        let identity = win::file_identity(file)?;
        let held = file.try_clone()?;
        // SAFETY: `file` is the caller's open, admitted handle.
        let section = unsafe {
            win::CreateFileMappingW(
                file.as_raw_handle(),
                std::ptr::null_mut(),
                win::PAGE_READONLY,
                0,
                0,
                std::ptr::null(),
            )
        };
        if section.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `section` is live; the view covers exactly the `len_usize` bytes the file was observed to hold, from offset 0.
        let view = unsafe { win::MapViewOfFile(section, win::FILE_MAP_READ, 0, 0, len_usize) };
        let failed = view.is_null().then(io::Error::last_os_error);
        // SAFETY: `section` is live and no longer needed: a view keeps its section alive until it is unmapped.
        unsafe { win::CloseHandle(section) };
        if let Some(error) = failed {
            return Err(error);
        }
        let Some(ptr) = std::ptr::NonNull::new(view.cast::<u8>()) else {
            return Err(io::Error::other("MapViewOfFile returned a null view"));
        };
        Ok(Self {
            ptr,
            len: len_usize,
            identity,
            _file: held,
        })
    }
}

#[cfg(any(unix, windows))]
impl MappedFontFile {
    /// Whether `self` and `other` map the same file — the same bytes, without touching a page of either.
    #[inline]
    #[must_use]
    pub const fn same_file(&self, other: &Self) -> bool {
        self.identity.0 == other.identity.0 && self.identity.1 == other.identity.1
    }
}

#[cfg(not(any(unix, windows)))]
impl MappedFontFile {
    /// No mapping on this target: [`admit_font_file`] never asks for one
    /// ([`open_for_admission`] answers `false`), and this says why if it did.
    fn map(_file: &std::fs::File, _len: u64) -> io::Result<Self> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Uninhabited: see the type's `cfg(not(any(unix, windows)))` doc.
    #[must_use]
    pub fn same_file(&self, _other: &Self) -> bool {
        match self.never {}
    }
}

impl core::ops::Deref for MappedFontFile {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        #[cfg(any(unix, windows))]
        // SAFETY: `ptr` is a live `len`-byte read-only mapping owned by `self` until `Drop`.
        unsafe {
            std::slice::from_raw_parts(self.ptr.as_ptr(), self.len)
        }
        #[cfg(not(any(unix, windows)))]
        match self.never {}
    }
}

impl AsRef<[u8]> for MappedFontFile {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl std::fmt::Debug for MappedFontFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedFontFile")
            .field("len", &self.len())
            .finish()
    }
}

#[cfg(unix)]
impl Drop for MappedFontFile {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`len` are exactly what `mmap` returned for this handle, and `Drop` runs once.
        unsafe {
            let _ = libc::munmap(self.ptr.as_ptr().cast(), self.len);
        }
    }
}

#[cfg(windows)]
impl Drop for MappedFontFile {
    fn drop(&mut self) {
        // SAFETY: `ptr` is the base `MapViewOfFile` returned for this handle, and `Drop` runs once.
        unsafe {
            let _ = win::UnmapViewOfFile(self.ptr.as_ptr().cast_const().cast());
        }
    }
}

/// The directories whose files [`admit_font_file`] may hold as mappings rather than copies.
pub const MAPPABLE_FONT_ROOTS: &[&str] = &[
    #[cfg(target_os = "macos")]
    "/System/Library/Fonts",
    #[cfg(windows)]
    "C:\\Windows\\Fonts",
];

/// Whether [`admit_font_file`] may map `path` (macOS, Windows) rather than copying it.
#[must_use]
pub fn maps_in_place(path: &Path) -> bool {
    !path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
        && MAPPABLE_FONT_ROOTS
            .iter()
            .any(|root| lies_under(path, Path::new(root)))
}

/// `path.starts_with(root)`, component-wise.
#[cfg(not(windows))]
fn lies_under(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

/// `path.starts_with(root)`, component-wise and ASCII-case-insensitive.
#[cfg(windows)]
fn lies_under(path: &Path, root: &Path) -> bool {
    let mut components = path.components();
    root.components().all(|want| {
        components
            .next()
            .is_some_and(|have| have.as_os_str().eq_ignore_ascii_case(want.as_os_str()))
    })
}

/// Admit one font file under the production budget.
pub fn admit_font_file(path: &Path) -> io::Result<crate::font::FaceBytes> {
    let (file, may_map) = open_for_admission(path, maps_in_place)?;
    let length = check_observed_size(&file, MAX_FONT_FILE_BYTES)?;
    if may_map && let Ok(mapped) = MappedFontFile::map(&file, length) {
        return Ok(crate::font::FaceBytes::Mapped(Arc::new(mapped)));
    }
    let mut bytes = Vec::new();
    read_admitted_handle(file, length, MAX_FONT_FILE_BYTES, &mut bytes)?;
    Ok(crate::font::FaceBytes::Vec(Arc::new(bytes)))
}

/// The `sfnt` tables of ONE face of a font file, read by position — never the whole file.
#[derive(Debug)]
pub struct FaceTables {
    pub faces: u32,
    pub tables: Vec<Option<Vec<u8>>>,
}

/// Most table records a face directory may declare before it is refused as
/// malformed.
const MAX_TABLE_RECORDS: usize = 1024;

/// Read tables `tags` of face `face_index` at `path` (each at most `max_table_bytes`) via [`open_regular`].
///
/// # Errors
/// The admission's error, a read error, or `InvalidData` for a file that is not an `sfnt` (or a collection without face `face_index`).
pub fn read_face_tables(
    path: &Path,
    face_index: u32,
    tags: &[[u8; 4]],
    max_table_bytes: usize,
) -> io::Result<FaceTables> {
    use std::io::{Seek as _, SeekFrom};

    let malformed = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
    let (mut file, _) = open_regular(path)?;
    let length = check_observed_size(&file, MAX_FONT_FILE_BYTES)?;
    let mut read_at = |offset: u64, len: usize| -> io::Result<Vec<u8>> {
        if offset
            .checked_add(len as u64)
            .is_none_or(|end| end > length)
        {
            return Err(malformed("font table lies past the end of the file"));
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0; len];
        file.read_exact(&mut buf)?;
        Ok(buf)
    };
    let be32 = |b: &[u8], at: usize| u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    let header = read_at(0, 12)?;
    let (faces, sfnt_offset) = if &header[..4] == b"ttcf" {
        let faces = be32(&header, 8);
        if face_index >= faces || faces > 4096 {
            return Err(malformed("collection has no such face"));
        }
        let entry = read_at(12 + 4 * u64::from(face_index), 4)?;
        (faces, u64::from(be32(&entry, 0)))
    } else if face_index == 0 {
        (1, 0)
    } else {
        return Err(malformed("a single-face font has no face past 0"));
    };
    let directory = read_at(sfnt_offset, 12)?;
    let version = be32(&directory, 0);
    if !matches!(version, 0x0001_0000 | 0x4F54_544F | 0x7472_7565) {
        return Err(malformed("not an sfnt face"));
    }
    let records = usize::from(u16::from_be_bytes([directory[4], directory[5]]));
    if records > MAX_TABLE_RECORDS {
        return Err(malformed("face declares too many tables"));
    }
    let records = read_at(sfnt_offset + 12, 16 * records)?;
    let mut tables = Vec::with_capacity(tags.len());
    for tag in tags {
        let table = records
            .as_chunks::<16>()
            .0
            .iter()
            .find(|record| &record[..4] == tag)
            .map(|record| (u64::from(be32(record, 8)), be32(record, 12) as usize))
            .filter(|&(_, len)| len <= max_table_bytes)
            .and_then(|(offset, len)| read_at(offset, len).ok());
        tables.push(table);
    }
    Ok(FaceTables { faces, tables })
}

/// [`admit_font_file`]'s one open, and whether its handle may be MAPPED:
/// `mappable` ([`maps_in_place`] in production) judges the path the handle
/// was opened at, which for a link is its canonical target ([`open_regular`]),
/// so a link on the system font volume to a user-writable file is copied.
#[cfg(unix)]
fn open_for_admission(
    path: &Path,
    mappable: fn(&Path) -> bool,
) -> io::Result<(std::fs::File, bool)> {
    let (file, opened) = open_regular(path)?;
    let may_map = mappable(&opened);
    Ok((file, may_map))
}

/// Windows: a file [`maps_in_place`] would map is opened without write sharing, which the
/// mapping keeps ([`MappedFontFile`]), so no process can open it for writing while its bytes
/// are a live `&[u8]`.
#[cfg(windows)]
fn open_for_admission(
    path: &Path,
    mappable: fn(&Path) -> bool,
) -> io::Result<(std::fs::File, bool)> {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    if mappable(path) {
        match open_regular_shared(path, win::FILE_SHARE_READ | win::FILE_SHARE_DELETE) {
            Ok(file) => return Ok((file, true)),
            Err(error) if error.raw_os_error() != Some(ERROR_SHARING_VIOLATION) => {
                return Err(error);
            }
            Err(_) => {}
        }
    }
    Ok((open_regular(path)?.0, false))
}

/// No mapping on this target: every admission is a copy.
#[cfg(not(any(unix, windows)))]
fn open_for_admission(
    path: &Path,
    _mappable: fn(&Path) -> bool,
) -> io::Result<(std::fs::File, bool)> {
    Ok((open_regular(path)?.0, false))
}

/// The kernel32 calls the Windows mapping arm makes, declared directly.
#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::io::AsRawHandle as _;

    pub(super) const FILE_SHARE_READ: u32 = 0x1;
    pub(super) const FILE_SHARE_WRITE: u32 = 0x2;
    pub(super) const FILE_SHARE_DELETE: u32 = 0x4;
    pub(super) const PAGE_READONLY: u32 = 0x02;
    pub(super) const FILE_MAP_READ: u32 = 0x0004;

    /// `BY_HANDLE_FILE_INFORMATION` (52 bytes): the attributes and three
    /// `FILETIME`s, the volume serial number, the size and link count, then
    /// the file index.
    #[repr(C)]
    #[derive(Default)]
    struct ByHandleFileInformation {
        _attributes_and_times: [u32; 7],
        volume_serial_number: u32,
        _size_and_links: [u32; 3],
        index_high: u32,
        index_low: u32,
    }

    // SAFETY: standard kernel32 declarations, signatures per the Win32 docs.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub(super) fn CreateFileMappingW(
            file: *mut c_void,
            attributes: *mut c_void,
            protect: u32,
            max_size_high: u32,
            max_size_low: u32,
            name: *const u16,
        ) -> *mut c_void;
        pub(super) fn MapViewOfFile(
            mapping: *mut c_void,
            access: u32,
            offset_high: u32,
            offset_low: u32,
            bytes: usize,
        ) -> *mut c_void;
        pub(super) fn UnmapViewOfFile(base: *const c_void) -> i32;
        pub(super) fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetFileInformationByHandle(
            file: *mut c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    /// The file's `(volume serial number, file index)`.
    pub(super) fn file_identity(file: &std::fs::File) -> io::Result<(u64, u64)> {
        let mut info = ByHandleFileInformation::default();
        // SAFETY: `file` is an open handle and `info` is a writable BY_HANDLE_FILE_INFORMATION, which is all the call writes.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((
            u64::from(info.volume_serial_number),
            (u64::from(info.index_high) << 32) | u64::from(info.index_low),
        ))
    }
}

/// Production-font-budget twin of [`read_bounded_font_file_into`], for a caller that reads the whole font tree and keeps only a summary of it.
#[inline]
pub fn read_font_file_into(path: &Path, buf: &mut Vec<u8>) -> io::Result<()> {
    read_bounded_font_file_into(path, MAX_FONT_FILE_BYTES, buf)
}

/// Production-font-budget validation wrapper used for exact diagnostics before committing a config path to the live renderer.
#[inline]
pub fn validate_font_file(path: &Path) -> io::Result<()> {
    validate_bounded_font_file(path, MAX_FONT_FILE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aterm-font-file-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create font-file fixture directory");
        root
    }

    #[test]
    fn exact_limit_is_accepted_and_one_byte_over_is_rejected() {
        let root = fixture_dir("limit");
        let exact = root.join("exact.ttf");
        let over = root.join("over.ttf");
        std::fs::write(&exact, [0x5a; 64]).expect("write exact-limit fixture");
        std::fs::write(&over, [0x5a; 65]).expect("write over-limit fixture");
        assert_eq!(read_bounded_font_file(&exact, 64).unwrap(), [0x5a; 64]);
        let error = read_bounded_font_file(&over, 64).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    /// A reused buffer reads what a fresh one does, including after a larger file (the stale
    /// tail a missing `clear()` would leave).
    #[test]
    fn a_reused_buffer_reads_the_same_bytes_a_fresh_one_does() {
        let root = fixture_dir("reuse");
        let sizes = [4096usize, 17, 900, 1, 8192];
        let paths: Vec<_> = sizes
            .iter()
            .enumerate()
            .map(|(i, &n)| {
                let path = root.join(format!("face{i}.ttf"));
                std::fs::write(&path, vec![u8::try_from(i).unwrap(); n]).expect("write fixture");
                path
            })
            .collect();

        let mut buf = Vec::new();
        for path in &paths {
            read_font_file_into(path, &mut buf).expect("reused read");
            assert_eq!(buf, read_font_file(path).expect("fresh read"));
        }
        assert!(buf.capacity() >= *sizes.iter().max().unwrap());
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    /// A refused path must not leave the previous font in a reused buffer.
    #[test]
    fn a_refused_path_leaves_no_stale_font_in_a_reused_buffer() {
        let root = fixture_dir("stale");
        let previous = root.join("previous.ttf");
        let over = root.join("over.ttf");
        let absent = root.join("absent.ttf");
        std::fs::write(&previous, [0xa5; 512]).expect("write previous-face fixture");
        std::fs::write(&over, [0x11; 4096]).expect("write over-limit fixture");

        let cases: [(&Path, usize, &str); 3] = [
            (absent.as_path(), 1024, "open_regular"),
            (over.as_path(), 1024, "check_observed_size"),
            (previous.as_path(), usize::MAX, "limit_plus_sentinel"),
        ];

        let mut buf = Vec::new();
        for (path, max_bytes, step) in cases {
            read_bounded_font_file_into(&previous, 1024, &mut buf).expect("previous face reads");
            assert_eq!(buf.len(), 512, "the reused buffer holds the previous face");
            read_bounded_font_file_into(path, max_bytes, &mut buf)
                .expect_err("a refused path must not be admitted");
            assert!(
                buf.is_empty(),
                "{step} refused the path and left {} of the PREVIOUS font's bytes behind",
                buf.len()
            );
        }
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    /// A one-shot read allocates no slack: the blob is interned for the process's life, so
    /// over-reserved capacity would stay resident.
    #[test]
    fn a_fresh_read_allocates_no_slack() {
        let root = fixture_dir("exact-capacity");
        let path = root.join("face.ttf");
        std::fs::write(&path, vec![0x5a; 100_003]).expect("write fixture");
        let bytes = read_font_file(&path).expect("fresh read");
        assert_eq!(bytes.len(), 100_003);
        assert_eq!(bytes.capacity(), bytes.len());
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    #[test]
    fn same_open_handle_supplies_bytes_after_path_replacement() {
        let root = fixture_dir("same-handle");
        let path = root.join("face.ttf");
        let moved = root.join("opened.ttf");
        std::fs::write(&path, b"original-font-bytes").expect("write original fixture");

        let (mut opened, _) = open_regular(&path).expect("open admitted handle");
        std::fs::rename(&path, &moved).expect("retain opened inode under another name");
        std::fs::write(&path, b"replacement-font").expect("replace logical path");

        let mut bytes = Vec::new();
        opened
            .read_to_end(&mut bytes)
            .expect("read admitted handle");
        assert_eq!(bytes, b"original-font-bytes");
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement-font");
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    /// A file under the system font directory maps; one under any user-writable directory (the
    /// per-user `$HOME` dirs, `/Library/Fonts`) copies, as does a `..` traversal that begins in
    /// the system directory.
    #[test]
    fn maps_in_place_admits_the_system_font_volume_and_nothing_user_writable() {
        #[cfg(target_os = "macos")]
        {
            assert!(maps_in_place(Path::new("/System/Library/Fonts/Menlo.ttc")));
            assert!(maps_in_place(Path::new(
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"
            )));
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            assert!(!maps_in_place(Path::new("/System/Library/Fonts/Menlo.ttc")));
            assert!(!maps_in_place(Path::new(
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"
            )));
        }
        assert!(!maps_in_place(Path::new("/Library/Fonts/Custom.ttf")));
        assert!(!maps_in_place(Path::new(
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
        )));
        assert!(!maps_in_place(Path::new(
            "/System/Library/Fonts/../../../Users//x/y.ttf"
        )));
        assert!(!maps_in_place(Path::new("System/Library/Fonts/Menlo.ttc")));
        #[cfg(windows)]
        {
            assert!(maps_in_place(Path::new("C:\\Windows\\Fonts\\msyh.ttc")));
            assert!(maps_in_place(Path::new("C:\\WINDOWS\\Fonts\\SegUIVar.ttf")));
            assert!(maps_in_place(Path::new("c:/windows/fonts/seguisb.ttf")));
            assert!(!maps_in_place(Path::new("C:\\Windows\\FontsX\\a.ttf")));
            assert!(!maps_in_place(Path::new("C:\\Windows")));
            assert!(!maps_in_place(Path::new("/System/Library/Fonts/Menlo.ttc")));
            assert!(!maps_in_place(Path::new("C:\\font-fixture\\café.ttf")));
            assert!(!maps_in_place(Path::new(
                "C:\\Users\\x\\AppData\\Local\\Microsoft\\Windows\\Fonts\\Font.ttf"
            )));
            assert!(!maps_in_place(Path::new("C:\\Windows\\Temp\\Font.ttf")));
            assert!(!maps_in_place(Path::new(
                "C:\\Windows\\Fonts\\..\\Temp\\Font.ttf"
            )));
        }
        #[cfg(not(windows))]
        assert!(!maps_in_place(Path::new("C:\\Windows\\Fonts\\msyh.ttc")));
        if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
            for user_dir in ["Library/Fonts", ".fonts", ".local/share/fonts"] {
                assert!(
                    !maps_in_place(&home.join(user_dir).join("Font.ttf")),
                    "~/{user_dir} is user-writable and must be COPIED"
                );
            }
        }
    }

    /// The residency policy is [`maps_in_place`]: a file outside the system
    /// font volume — here a fixture directory, standing in for every
    /// user-writable path — is COPIED, empty or not, with exactly its bytes.
    #[test]
    fn admit_font_file_copies_a_file_off_the_system_font_volume() {
        let root = fixture_dir("admit-policy");
        let outside = root.join("outside.ttf");
        std::fs::write(&outside, [0x42; 4099]).expect("write outside fixture");
        assert!(!maps_in_place(&outside));
        let admitted = admit_font_file(&outside).expect("a regular file admits");
        assert!(
            matches!(admitted, crate::font::FaceBytes::Vec(_)),
            "a path off the system font volume must be COPIED, got {admitted:?}"
        );
        assert_eq!(&admitted[..], &read_font_file(&outside).unwrap()[..]);

        let empty = root.join("empty.ttf");
        std::fs::write(&empty, []).expect("write empty fixture");
        let admitted = admit_font_file(&empty).expect("an empty regular file still admits");
        assert!(
            admitted.is_empty(),
            "the copy arm carries the empty file as-is"
        );

        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    /// The mapping arm answers the SAME bytes the copy arm does: a file on the
    /// admitted system font volume is MAPPED (macOS, Windows; elsewhere nothing
    /// is).
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn admit_font_file_maps_a_file_on_the_system_font_volume() {
        let candidate = MAPPABLE_FONT_ROOTS
            .iter()
            .filter_map(|dir| std::fs::read_dir(dir).ok())
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| matches!(e, "ttf" | "otf" | "ttc"))
                    && std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
            });
        let Some(system_font) = candidate else {
            aterm_spec::host::could_not_run(
                "no font file on the system font volume on this host to map",
            );
        };
        assert!(maps_in_place(&system_font));
        let admitted = admit_font_file(&system_font).expect("a system font admits");
        assert!(
            matches!(admitted, crate::font::FaceBytes::Mapped(_)),
            "{}: a file on the system font volume must be MAPPED, got {admitted:?}",
            system_font.display()
        );
        assert_eq!(
            &admitted[..],
            &read_font_file(&system_font).unwrap()[..],
            "the mapping must present exactly the file's bytes"
        );
        let again = admit_font_file(&system_font).expect("a system font admits twice");
        match (&admitted, &again) {
            (crate::font::FaceBytes::Mapped(a), crate::font::FaceBytes::Mapped(b)) => {
                assert!(a.same_file(b));
                assert!(admitted.same_bytes(&again));
            }
            _ => unreachable!("both admissions were asserted mapped"),
        }
    }

    /// The Windows stand-in for a read-only volume, checked against the OS: while a view is live,
    /// opening the file for writing fails with a sharing violation (the mapping keeps the
    /// no-write-share handle), readers still open it, and the refusal lifts with the view.
    #[cfg(windows)]
    #[test]
    fn a_mapped_font_refuses_writers_until_it_is_dropped() {
        const ERROR_SHARING_VIOLATION: i32 = 32;
        let root = fixture_dir("win-map-share");
        let path = root.join("face.ttf");
        std::fs::write(&path, b"mapped-font-bytes").expect("write fixture");

        let file = open_regular_shared(&path, win::FILE_SHARE_READ | win::FILE_SHARE_DELETE)
            .expect("the mapping open succeeds with no writer present");
        let length = check_observed_size(&file, MAX_FONT_FILE_BYTES).expect("size");
        let mapped = MappedFontFile::map(&file, length).expect("map the fixture");
        drop(file);
        assert_eq!(&mapped[..], b"mapped-font-bytes");

        let writer = std::fs::OpenOptions::new().write(true).open(&path);
        assert_eq!(
            writer.as_ref().err().and_then(io::Error::raw_os_error),
            Some(ERROR_SHARING_VIOLATION),
            "a writer opened the file while its bytes were a live mapping: {writer:?}"
        );
        assert_eq!(
            read_font_file(&path).expect("readers are not refused"),
            b"mapped-font-bytes"
        );

        drop(mapped);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("the write refusal must lift with the mapping");
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    /// The servicing path the mapping keeps open, checked against the OS: the
    /// admission grants `FILE_SHARE_DELETE`, so while a view is live the file
    /// can still be RENAMED and its name reused (a font replaced while aterm
    /// runs), and the view, bound to the file object rather than to its name,
    /// still reads the original bytes.
    #[cfg(windows)]
    #[test]
    fn a_mapped_font_can_be_renamed_and_replaced_and_its_view_keeps_the_original() {
        let root = fixture_dir("win-map-rename");
        let path = root.join("face.ttf");
        let renamed = root.join("face-old.ttf");
        std::fs::write(&path, b"original-font-bytes").expect("write fixture");

        let file = open_regular_shared(&path, win::FILE_SHARE_READ | win::FILE_SHARE_DELETE)
            .expect("the mapping open succeeds with no writer present");
        let length = check_observed_size(&file, MAX_FONT_FILE_BYTES).expect("size");
        let mapped = MappedFontFile::map(&file, length).expect("map the fixture");
        drop(file);

        std::fs::rename(&path, &renamed).expect("a mapped font must still be renameable");
        std::fs::write(&path, b"replacement").expect("and its name reusable");
        assert_eq!(
            &mapped[..],
            b"original-font-bytes",
            "the view must still read the ORIGINAL bytes after its name was replaced"
        );
        assert_eq!(
            read_font_file(&path).expect("the replacement reads"),
            b"replacement"
        );

        drop(mapped);
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    /// A mappable file a writer already holds is not mapped (its bytes could change under the
    /// view) nor refused: it is copied through the fully-shared open.
    #[cfg(windows)]
    #[test]
    fn a_mappable_font_a_writer_holds_is_copied_not_refused() {
        let root = fixture_dir("win-map-writer");
        let path = root.join("face.ttf");
        std::fs::write(&path, b"held-by-a-writer").expect("write fixture");
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open the writer");

        let (file, may_map) =
            open_for_admission(&path, |_| true).expect("a held file still admits");
        assert!(!may_map, "a file a writer holds must not be mapped");
        let length = check_observed_size(&file, MAX_FONT_FILE_BYTES).expect("size");
        let mut bytes = Vec::new();
        read_admitted_handle(file, length, MAX_FONT_FILE_BYTES, &mut bytes).expect("copy");
        assert_eq!(bytes, b"held-by-a-writer");

        drop(writer);
        let (_, may_map) = open_for_admission(&path, |_| true).expect("admits once released");
        assert!(may_map, "with no writer the same file is mappable again");
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    /// A writable view keeps its section alive even after both handles close.
    #[cfg(windows)]
    #[test]
    fn a_writable_view_without_its_handles_is_copied_not_mapped() {
        use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

        struct WritableView(std::ptr::NonNull<std::ffi::c_void>);

        impl Drop for WritableView {
            fn drop(&mut self) {
                // SAFETY: this guard owns the live view returned below and unmaps it once, including when an assertion unwinds.
                unsafe { win::UnmapViewOfFile(self.0.as_ptr()) };
            }
        }

        const PAGE_READWRITE: u32 = 0x04;
        const FILE_MAP_WRITE: u32 = 0x0002;
        let root = fixture_dir("win-map-prior-view");
        let path = root.join("face.ttf");
        let expected = b"held-by-a-writable-view";
        std::fs::write(&path, expected).expect("write fixture");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open the writable mapping file");
        // SAFETY: the file is open for reading/writing and is nonempty; an unnamed PAGE_READWRITE section covers its existing contents.
        let raw_section = unsafe {
            win::CreateFileMappingW(
                file.as_raw_handle(),
                std::ptr::null_mut(),
                PAGE_READWRITE,
                0,
                0,
                std::ptr::null(),
            )
        };
        assert!(!raw_section.is_null(), "{}", io::Error::last_os_error());
        // SAFETY: CreateFileMappingW returned a live handle, transferred to this guard so a later failure still closes it exactly once.
        let section = unsafe { OwnedHandle::from_raw_handle(raw_section) };
        // SAFETY: the section is live and contains expected.len() bytes.
        let raw_view = unsafe {
            win::MapViewOfFile(
                section.as_raw_handle(),
                FILE_MAP_WRITE,
                0,
                0,
                expected.len(),
            )
        };
        let view = WritableView(
            std::ptr::NonNull::new(raw_view)
                .unwrap_or_else(|| panic!("{}", io::Error::last_os_error())),
        );
        drop(section);
        drop(file);

        let (file, may_map) =
            open_for_admission(&path, |_| true).expect("a live view still admits");
        assert!(
            !may_map,
            "a writable view without handles must prevent mapping"
        );
        let length = check_observed_size(&file, MAX_FONT_FILE_BYTES).expect("size");
        let mut bytes = Vec::new();
        read_admitted_handle(file, length, MAX_FONT_FILE_BYTES, &mut bytes).expect("copy");
        assert_eq!(bytes, expected);

        drop(view);
        let (file, may_map) = open_for_admission(&path, |_| true).expect("admits after unmapping");
        assert!(
            may_map,
            "the same file is mappable after its writable view closes"
        );
        let mapped = MappedFontFile::map(&file, length).expect("map the released fixture");
        assert_eq!(&mapped[..], expected);
        drop(mapped);
        drop(file);
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    #[cfg(unix)]
    #[test]
    fn writerless_fifo_and_links_to_it_return_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let root = fixture_dir("special");
        let fifo = root.join("face.fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path");
        // SAFETY: `fifo_c` is a live NUL-terminated pathname and mkfifo retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let error = read_bounded_font_file(&fifo, 64)
            .expect_err("writerless FIFO must fail without blocking");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        let to_fifo = root.join("fifo-link.ttf");
        std::os::unix::fs::symlink(&fifo, &to_fifo).expect("create link to FIFO");
        let error = read_bounded_font_file(&to_fifo, 64)
            .expect_err("a link to a writerless FIFO must fail without blocking");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let to_device = root.join("zero-link.ttf");
        std::os::unix::fs::symlink("/dev/zero", &to_device).expect("create link to device");
        let error = read_bounded_font_file(&to_device, 64)
            .expect_err("a link to a device must not be read");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    /// A font installed as a LINK (Ubuntu's `UbuntuMono-R.ttf`, a stow or
    /// home-manager font in `~/.local/share/fonts`) admits as the file it
    /// names, through every entry point; a link loop and a dangling link do
    /// not; and residency is decided on the file opened, so a copy is a copy.
    #[cfg(unix)]
    #[test]
    fn a_link_to_a_regular_font_admits_as_its_target() {
        let root = fixture_dir("links");
        let store = root.join("store");
        std::fs::create_dir_all(&store).expect("create link-target directory");
        let regular = store.join("Face[wght].ttf");
        std::fs::write(&regular, b"font-bytes").expect("write link target");
        let linked = root.join("Face-R.ttf");
        std::os::unix::fs::symlink(&regular, &linked).expect("create final link");
        let chained = root.join("Face-B.ttf");
        std::os::unix::fs::symlink("Face-R.ttf", &chained).expect("create chained link");

        for path in [&linked, &chained] {
            validate_font_file(path).expect("a link to a regular file validates");
            assert_eq!(read_font_file(path).expect("and reads"), b"font-bytes");
            let mut buf = Vec::new();
            read_font_file_into(path, &mut buf).expect("and reads into a buffer");
            assert_eq!(buf, b"font-bytes");
            let admitted = admit_font_file(path).expect("and admits");
            assert_eq!(&admitted[..], b"font-bytes");
            assert!(matches!(admitted, crate::font::FaceBytes::Vec(_)));
        }

        let a = root.join("loop-a.ttf");
        let b = root.join("loop-b.ttf");
        std::os::unix::fs::symlink(&b, &a).expect("create loop link a");
        std::os::unix::fs::symlink(&a, &b).expect("create loop link b");
        assert!(read_font_file(&a).is_err(), "a link loop must stay refused");
        let dangling = root.join("dangling.ttf");
        std::os::unix::fs::symlink(root.join("absent.ttf"), &dangling)
            .expect("create dangling link");
        assert_eq!(
            read_font_file(&dangling).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }
}
