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
/// returning it with its metadata and the path that was actually opened.
#[cfg(unix)]
fn open_regular(path: &Path) -> io::Result<(std::fs::File, std::fs::Metadata, Cow<'_, Path>)> {
    match open_regular_no_follow(path) {
        Ok((file, metadata)) => Ok((file, metadata, Cow::Borrowed(path))),
        Err(error) => {
            // Only a final-component LINK is retried; every other refusal
            // (absent, FIFO, device, oversize) keeps its own error.
            if !std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(error);
            }
            let target = std::fs::canonicalize(path)?;
            let (file, metadata) = open_regular_no_follow(&target)?;
            Ok((file, metadata, Cow::Owned(target)))
        }
    }
}

/// The one `open` [`open_regular`] performs per path: never through a
/// final-component link, never blocking, and only a regular file is kept.
#[cfg(unix)]
fn open_regular_no_follow(path: &Path) -> io::Result<(std::fs::File, std::fs::Metadata)> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(not_regular());
    }
    Ok((file, metadata))
}

/// Windows' reparse-point flag is the closest equivalent of `O_NOFOLLOW`.
#[cfg(windows)]
fn open_regular(path: &Path) -> io::Result<(std::fs::File, std::fs::Metadata, Cow<'_, Path>)> {
    let (file, metadata) = open_regular_shared(
        path,
        win::FILE_SHARE_READ | win::FILE_SHARE_WRITE | win::FILE_SHARE_DELETE,
    )?;
    Ok((file, metadata, Cow::Borrowed(path)))
}

/// [`open_regular`] with an explicit share mode: the copy path shares everything (std's
/// default); the mapping path ([`open_for_admission`]) refuses write sharing.
#[cfg(windows)]
fn open_regular_shared(path: &Path, share: u32) -> io::Result<(std::fs::File, std::fs::Metadata)> {
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
    Ok((file, metadata))
}

/// Portable fallback for targets without a non-blocking filesystem-open flag.
#[cfg(not(any(unix, windows)))]
fn open_regular(path: &Path) -> io::Result<(std::fs::File, std::fs::Metadata, Cow<'_, Path>)> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(not_regular());
    }
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(not_regular());
    }
    Ok((file, metadata, Cow::Borrowed(path)))
}

fn check_observed_size(metadata: &std::fs::Metadata, max_bytes: usize) -> io::Result<u64> {
    let length = metadata.len();
    if length > u64::try_from(max_bytes).unwrap_or(u64::MAX) {
        return Err(too_large(max_bytes));
    }
    Ok(length)
}

/// Validate an explicit path without reading or parsing its bytes, under the same open/handle/type/size rules as [`read_bounded_font_file`].
pub fn validate_bounded_font_file(path: &Path, max_bytes: usize) -> io::Result<()> {
    let (_, metadata, _) = open_regular(path)?;
    let _ = check_observed_size(&metadata, max_bytes)?;
    Ok(())
}

/// Whether `path` opens as an admitted regular font file starting with the `ttcf` TrueType Collection magic.
#[must_use]
pub(crate) fn is_ttc_font_file(path: &Path) -> bool {
    let Ok((mut file, _, _)) = open_regular(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).is_ok() && &magic == b"ttcf"
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
    let (file, metadata, _) = open_regular(path)?;
    let length = check_observed_size(&metadata, max_bytes)?;
    read_admitted_handle(file, length, max_bytes, buf)
}

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

// SAFETY: the mapping is read-only (unix: PROT_READ and MAP_PRIVATE; Windows: a PAGE_READONLY
// section viewed FILE_MAP_READ) — nothing writes through it, and the kernel owns the pages — so
// sharing or moving the handle between threads is sharing an immutable byte slice. `Drop` runs
// once, on the last `Arc` holder. On Windows, CreateFileW's FILE_SHARE_WRITE rule covers mappings
// as well as open handles, so the no-write-share admission open also excludes an existing writable
// mapping, and the retained handle refuses later writable opens and mappings.
#[cfg(any(unix, windows))]
unsafe impl Send for MappedFontFile {}
#[cfg(any(unix, windows))]
unsafe impl Sync for MappedFontFile {}

#[cfg(unix)]
impl MappedFontFile {
    fn map(file: &std::fs::File, metadata: &std::fs::Metadata, len: u64) -> io::Result<Self> {
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
        let identity = (metadata.dev(), metadata.ino());
        // SAFETY: `fd` is the caller's open, admitted handle; a NULL hint with
        // PROT_READ|MAP_PRIVATE asks the kernel for a fresh read-only private
        // region of exactly `len_usize` bytes at offset 0, which the fstat
        // above proved the file has. MAP_FAILED is checked before the pointer
        // is used.
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
    fn map(file: &std::fs::File, _metadata: &std::fs::Metadata, len: u64) -> io::Result<Self> {
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
        // SAFETY: `file` is the caller's open, admitted handle. A NULL name and
        // zero maximum size ask for an anonymous read-only section over the
        // whole current file, which the size check above proved non-empty.
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
    fn map(_file: &std::fs::File, _metadata: &std::fs::Metadata, _len: u64) -> io::Result<Self> {
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
        // SAFETY: `ptr` is a live `len`-byte read-only mapping owned by
        // `self` until `Drop`; nothing writes through it, so handing out
        // shared slices for `&self`'s lifetime is sound.
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
        // The held handle (`_file`) closes after this body, so the write-sharing refusal lasts
        // exactly as long as the view.
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
    let (file, metadata, may_map) = open_for_admission(path, maps_in_place)?;
    let length = check_observed_size(&metadata, MAX_FONT_FILE_BYTES)?;
    if may_map && let Ok(mapped) = MappedFontFile::map(&file, &metadata, length) {
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

const MAX_TABLE_RECORDS: usize = 1024;

/// Read tables `tags` of face `face_index` at `path` via [`open_regular`].
///
/// # Errors
/// Admission error, read error, or `InvalidData` for a non-`sfnt` or missing `face_index`.
pub fn read_face_tables(
    path: &Path,
    face_index: u32,
    tags: &[[u8; 4]],
    max_table_bytes: usize,
) -> io::Result<FaceTables> {
    use std::io::{Seek as _, SeekFrom};

    let malformed = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
    let (mut file, metadata, _) = open_regular(path)?;
    let length = check_observed_size(&metadata, MAX_FONT_FILE_BYTES)?;
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
) -> io::Result<(std::fs::File, std::fs::Metadata, bool)> {
    let (file, metadata, opened) = open_regular(path)?;
    let may_map = mappable(&opened);
    Ok((file, metadata, may_map))
}

/// Windows: a file [`maps_in_place`] would map is opened without write sharing, which the
/// mapping keeps ([`MappedFontFile`]), so no process can open it for writing while its bytes
/// are a live `&[u8]`.
#[cfg(windows)]
fn open_for_admission(
    path: &Path,
    mappable: fn(&Path) -> bool,
) -> io::Result<(std::fs::File, std::fs::Metadata, bool)> {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    if mappable(path) {
        match open_regular_shared(path, win::FILE_SHARE_READ | win::FILE_SHARE_DELETE) {
            Ok((file, metadata)) => return Ok((file, metadata, true)),
            Err(error) if error.raw_os_error() != Some(ERROR_SHARING_VIOLATION) => {
                return Err(error);
            }
            Err(_) => {}
        }
    }
    let (file, metadata, _) = open_regular(path)?;
    Ok((file, metadata, false))
}

/// No mapping on this target: every admission is a copy.
#[cfg(not(any(unix, windows)))]
fn open_for_admission(
    path: &Path,
    _mappable: fn(&Path) -> bool,
) -> io::Result<(std::fs::File, std::fs::Metadata, bool)> {
    let (file, metadata, _) = open_regular(path)?;
    Ok((file, metadata, false))
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
#[path = "font_file_tests.rs"]
mod tests;
