// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Safe, bounded admission for explicit font-file paths.
//!
//! A config string is not proof that its target is a normal finite file: a FIFO
//! can park `open`, a device can produce an unbounded stream, and a pathname can
//! be replaced between a metadata probe and a later read. This module opens once,
//! makes that open non-blocking where the platform exposes the flag, validates
//! the opened handle itself, and reads at most the font budget plus one sentinel
//! byte. Unix and Windows refuse final-component links/reparse points; config
//! authors should name the actual font file.
//!
//! Two ways to admit a file share that one open: [`read_bounded_font_file`]
//! copies it into a `Vec`, and [`admit_font_file`] MAPS it read-only and
//! private ([`MappedFontFile`]) when the path lies under one of the font
//! directories the resolver walks, so a 192 MB emoji collection the seal
//! must admit is file-backed, shared with every other process through the
//! page cache, evictable, and faulted in per glyph instead of copied whole
//! into anonymous heap at every launch.

use std::io::{self, Read as _};
use std::path::Path;
use std::sync::Arc;

/// Largest external font blob admitted by aterm.
///
/// Apple Color Emoji is currently about 192 MiB, so 256 MiB retains the largest
/// useful shipping system collection while bounding hostile input. Ordinary
/// terminal and styled faces are generally well below 16 MiB. Reads happen only
/// on cold construction/config paths (and Settings semantic previews use their
/// parked worker), never per glyph.
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

/// Open once and prove that the handle which will be read is a regular file.
/// `O_NONBLOCK` prevents a planted writerless FIFO from parking `open`, while
/// `O_NOFOLLOW` closes the final-component replacement/link ambiguity.
#[cfg(unix)]
fn open_regular(path: &Path) -> io::Result<std::fs::File> {
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
/// The attribute is checked on the opened handle before any byte is consumed.
#[cfg(windows)]
fn open_regular(path: &Path) -> io::Result<std::fs::File> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let file = std::fs::OpenOptions::new()
        .read(true)
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

/// Portable fallback. Targets without a non-blocking filesystem-open flag use
/// a no-link precheck and still verify the opened handle before reading.
#[cfg(not(any(unix, windows)))]
fn open_regular(path: &Path) -> io::Result<std::fs::File> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(not_regular());
    }
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

fn check_observed_size(file: &std::fs::File, max_bytes: usize) -> io::Result<u64> {
    let length = file.metadata()?.len();
    if length > u64::try_from(max_bytes).unwrap_or(u64::MAX) {
        return Err(too_large(max_bytes));
    }
    Ok(length)
}

/// Validate an explicit path without consuming or parsing its bytes.
///
/// This is suitable for diagnostics and resolution. It shares the exact
/// open/handle/type/metadata-size rules with [`read_bounded_font_file`], so a
/// writerless FIFO returns immediately and an already-oversized regular file has
/// an exact diagnostic. The eventual read repeats admission because the path may
/// legitimately change between validation and application.
pub fn validate_bounded_font_file(path: &Path, max_bytes: usize) -> io::Result<()> {
    let file = open_regular(path)?;
    let _ = check_observed_size(&file, max_bytes)?;
    Ok(())
}

/// Read one same-handle regular font file into a bounded byte vector.
///
/// Metadata is an early rejection only. The `max + 1` sentinel is authoritative,
/// so a regular file which grows after `fstat` cannot exceed the parser budget or
/// be accepted as a silently truncated font.
pub fn read_bounded_font_file(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    read_bounded_font_file_into(path, max_bytes, &mut bytes)?;
    Ok(bytes)
}

/// [`read_bounded_font_file`] into a buffer the caller supplies and REUSES.
///
/// Identical admission: same single `open_regular` handle, same metadata
/// pre-check, same `max + 1` sentinel, same over-limit refusal — this is the one
/// implementation, and [`read_bounded_font_file`] is it with a fresh `Vec`.
///
/// WHY A REUSED BUFFER IS A DIFFERENT THING FROM A FRESH ONE. A caller that
/// reads MANY files and keeps almost nothing — the cmap-coverage index
/// (`font_coverage_index`) reads every system font on the box, 358 files and
/// 174 MB on this Linux host, and retains ~214 kB of range tables — hands the
/// allocator 358 alloc/free pairs whose sizes swing from a few kB to 26 MB.
/// glibc frees the first large block by `munmap` and then RAISES its dynamic
/// `mmap` threshold to that block's size, so every read after it is served from
/// the arena instead, and the arena's high-water mark never comes back. MEASURED
/// on this host: the index build left **27.5 MB** of RSS behind it, on the main
/// thread and on the spawned warm thread alike, in every aterm process. One
/// buffer that only ever grows is one block, and it is returned when it drops:
/// the same build leaves **1.4 MB**. Same files, same bytes, same index.
///
/// On success `buf` holds exactly the file's bytes; its CAPACITY is whatever the
/// largest file so far needed, which is the point.
///
/// ON FAILURE THE BUFFER IS EMPTY OR PARTIAL, NEVER THE PREVIOUS FONT. The
/// clear happens BEFORE the first fallible step, and the ordering is the whole
/// point: admission refuses a path (unopenable, non-regular, already over the
/// limit) before one byte is read, so clearing afterwards would leave a REUSED
/// buffer holding the last file's complete, parseable face. A caller that
/// mishandles the error would then read a font it never asked for and could not
/// tell from a success. Cleared-first, the same mistake yields an empty buffer;
/// a failure part-way through the read leaves a partial one, which at least
/// cannot pass for a font. Callers that skip the file — every caller today —
/// are unaffected either way.
pub fn read_bounded_font_file_into(
    path: &Path,
    max_bytes: usize,
    buf: &mut Vec<u8>,
) -> io::Result<()> {
    buf.clear();
    let file = open_regular(path)?;
    let length = check_observed_size(&file, max_bytes)?;
    read_admitted_handle(file, length, max_bytes, buf)
}

/// The read half of [`read_bounded_font_file_into`], over a handle
/// [`open_regular`] + [`check_observed_size`] already admitted — so the mapping
/// path can fall back to the copy WITHOUT reopening the pathname.
fn read_admitted_handle(
    file: std::fs::File,
    length: u64,
    max_bytes: usize,
    buf: &mut Vec<u8>,
) -> io::Result<()> {
    buf.clear();
    let read_limit = limit_plus_sentinel(max_bytes)?;
    // `try_reserve_exact` on an EMPTY vec asks for exactly the observed length,
    // which is what keeps a fresh buffer's capacity slack-free (`read_to_end`
    // probes for EOF with a stack array rather than doubling an exact-fit
    // buffer). A reused buffer already big enough asks for nothing.
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
pub fn read_font_file(path: &Path) -> io::Result<Vec<u8>> {
    read_bounded_font_file(path, MAX_FONT_FILE_BYTES)
}

/// One font file mapped whole, read-only and private (`PROT_READ`,
/// `MAP_PRIVATE`), unmapped on drop.
///
/// This is the handle [`admit_font_file`] hands back for a file on the system
/// font volume, and it is what turns the seal's ~240 MB of system-font
/// admission on a Mac — Apple Color Emoji 192 MB, Hiragino Sans GB 23.5 MB,
/// Arial Unicode 23.3 MB, STIX Two Math 0.8 MB, every one read whole into
/// anonymous heap at every windowed launch and held for the process's life
/// — into file-backed pages: shared across every aterm process through the
/// unified buffer cache, evictable under pressure, and faulted in only for
/// the tables and glyphs actually touched (an ASCII-only session touches a
/// few pages of the emoji collection's table directory and nothing else).
///
/// # The truncation caveat, on the record
///
/// A page of a `MAP_PRIVATE` file mapping that lies PAST the file's current
/// end faults with `SIGBUS`, so a file truncated after it was mapped crashes
/// the reader on its next cold glyph — and bytes changing under a live
/// `&[u8]` is undefined behaviour besides. That is why [`admit_font_file`]
/// maps ONLY paths under the sealed system font volume
/// ([`MAPPABLE_FONT_ROOTS`] — `/System/Library/Fonts`, which is SIP-protected
/// and mounted read-only; an OS update replaces its files as new inodes, so
/// an old mapping stays valid) and copies everything else. Every byte the
/// residency finding measured lives on that volume, so the exclusion costs
/// nothing; and every user-writable directory — `~/Library/Fonts`, `~/.fonts`,
/// `~/.local/share/fonts`, `/Library/Fonts`, the Linux package roots, a
/// download directory named in the config — keeps the bounded copy, because
/// `cp new.ttf ~/Library/Fonts/Font.ttf` truncates and rewrites the SAME
/// inode while a terminal may hold it, and a face reachable through
/// `fallback_fonts` / `$ATERM_FALLBACK_FONT` / `$ATERM_SYMBOL_FONT` /
/// `$ATERM_EMOJI_FONT` can live exactly there. [`maps_in_place`] is the one
/// definition of the rule.
///
/// The bytes are validated exactly as a copy would be: same `open_regular`
/// (regular file, `O_NOFOLLOW`, `O_NONBLOCK`), same `fstat` size bound
/// ([`MAX_FONT_FILE_BYTES`]), and the mapping's length IS that observed
/// size, so no byte past the admission bound is ever addressable.
///
/// `identity` is the file's `(st_dev, st_ino)`: two mappings of the same
/// inode are the same bytes, and the discovery intern uses that to converge
/// on one mapping without comparing 192 MB of pages.
#[cfg(unix)]
pub struct MappedFontFile {
    ptr: std::ptr::NonNull<u8>,
    len: usize,
    identity: (u64, u64),
}

/// Off unix there is no mapping constructor, so the type is uninhabited: the
/// `FaceBytes::Mapped` arm exists on every target and can be built on none of
/// these, which keeps every `match` over the handle cfg-free.
#[cfg(not(unix))]
pub struct MappedFontFile {
    never: core::convert::Infallible,
}

// SAFETY: the mapping is PROT_READ and MAP_PRIVATE — nothing writes through
// it, and the kernel owns the pages — so sharing or moving the handle between
// threads is sharing an immutable byte slice. `Drop` runs once, on the last
// `Arc` holder.
#[cfg(unix)]
unsafe impl Send for MappedFontFile {}
#[cfg(unix)]
unsafe impl Sync for MappedFontFile {}

#[cfg(unix)]
impl MappedFontFile {
    /// Map `len` bytes of an already-admitted handle. `len` is the `fstat`
    /// size [`check_observed_size`] bounded, so the mapping never addresses a
    /// byte past the admission limit. An empty file cannot be mapped
    /// (`mmap` refuses a zero length) and is refused here the same way.
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
            // A NULL success is impossible with a NULL hint on every unix
            // aterm builds for; treat it as a failed map rather than trust it.
            return Err(io::Error::other("mmap returned a null mapping"));
        };
        Ok(Self {
            ptr,
            len: len_usize,
            identity,
        })
    }

    /// Whether `self` and `other` map the same inode — the same bytes, without
    /// touching a page of either.
    #[must_use]
    pub fn same_file(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

#[cfg(not(unix))]
impl MappedFontFile {
    /// Uninhabited: see the type's `cfg(not(unix))` doc.
    #[must_use]
    pub fn same_file(&self, _other: &Self) -> bool {
        match self.never {}
    }
}

impl core::ops::Deref for MappedFontFile {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        #[cfg(unix)]
        // SAFETY: `ptr` is a live `len`-byte PROT_READ mapping owned by
        // `self` until `Drop`; nothing writes through it, so handing out
        // shared slices for `&self`'s lifetime is sound.
        unsafe {
            std::slice::from_raw_parts(self.ptr.as_ptr(), self.len)
        }
        #[cfg(not(unix))]
        match self.never {}
    }
}

impl AsRef<[u8]> for MappedFontFile {
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
        // SAFETY: `ptr`/`len` are exactly what `mmap` returned for this handle,
        // and `Drop` runs once. The result is ignored on purpose: munmap of a
        // valid mapping cannot fail, and there is nothing to do if it did.
        unsafe {
            let _ = libc::munmap(self.ptr.as_ptr().cast(), self.len);
        }
    }
}

/// The directories whose files [`admit_font_file`] holds as file MAPPINGS
/// rather than copies: the sealed, read-only system font volume and nothing
/// user-writable (see [`MappedFontFile`]'s truncation caveat). macOS-shaped on
/// purpose: the Linux package roots are root-owned but are rewritten in place
/// by package managers, and the finding this serves measured a Mac; widening
/// the list is an owner ruling, not a patch.
pub const MAPPABLE_FONT_ROOTS: &[&str] = &["/System/Library/Fonts"];

/// Whether [`admit_font_file`] MAPS `path` (unix) rather than copying it:
/// the path lies lexically under one of [`MAPPABLE_FONT_ROOTS`] and contains
/// no `..` component. The `..` refusal is load-bearing — `Path::starts_with`
/// is component-wise, so `/System/Library/Fonts/../../../Users//x/y.ttf`
/// would otherwise pass — and it keeps the check free of pathname I/O, which
/// matters because the seal's callers admit paths with the promise that no
/// pathname is touched afterwards.
#[must_use]
pub fn maps_in_place(path: &Path) -> bool {
    !path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
        && MAPPABLE_FONT_ROOTS
            .iter()
            .any(|root| path.starts_with(root))
}

/// Admit one font file under the production budget, MAPPING it when the path
/// lies under the system font volume ([`maps_in_place`]) and COPYING it
/// otherwise.
///
/// One `open_regular`, one `fstat` bound, for either outcome: the copy arm
/// reads the very handle the mapping arm would have mapped, so a path that
/// changes underneath us after admission cannot swap in a different file.
/// A mapping failure (`mmap` refused, an empty file) falls back to the copy
/// on the same handle rather than failing the admission — the mapping is a
/// residency choice, never a correctness one.
pub fn admit_font_file(path: &Path) -> io::Result<crate::font::FaceBytes> {
    let file = open_regular(path)?;
    let length = check_observed_size(&file, MAX_FONT_FILE_BYTES)?;
    #[cfg(unix)]
    if maps_in_place(path)
        && let Ok(mapped) = MappedFontFile::map(&file, length)
    {
        return Ok(crate::font::FaceBytes::Mapped(Arc::new(mapped)));
    }
    let mut bytes = Vec::new();
    read_admitted_handle(file, length, MAX_FONT_FILE_BYTES, &mut bytes)?;
    Ok(crate::font::FaceBytes::Vec(Arc::new(bytes)))
}

/// Production-font-budget twin of [`read_bounded_font_file_into`], for the one
/// caller that reads the whole font tree and keeps only a summary of it.
pub fn read_font_file_into(path: &Path, buf: &mut Vec<u8>) -> io::Result<()> {
    read_bounded_font_file_into(path, MAX_FONT_FILE_BYTES, buf)
}

/// Production-font-budget validation wrapper used for exact diagnostics before
/// committing a config path to the live renderer.
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
        assert_eq!(error.to_string(), "font file exceeds the 64-byte limit");
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    /// A REUSED buffer must answer exactly what a fresh one does, including when
    /// the previous file was LARGER — the case a missing `clear()` turns into a
    /// font whose tail is the previous font's bytes. Sizes go big → small → big
    /// so both the stale-tail and the grow-again paths are exercised.
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
        // The buffer really was reused: it ends holding the capacity the largest
        // file needed, not a fresh allocation per file.
        assert!(buf.capacity() >= *sizes.iter().max().unwrap());
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    /// A REFUSED path must not leave the PREVIOUS font's bytes in a reused
    /// buffer. Every admission failure happens before a single byte is read, so
    /// a clear placed after them leaves the buffer holding the last file whole —
    /// a complete, parseable face, indistinguishable from a success to a caller
    /// that mishandles the `Err`. One case per fallible step, in source order.
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
            // 512 bytes is inside `usize::MAX`, so admission reaches the third
            // step and only the `max + 1` sentinel arithmetic refuses it.
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

    /// The delegation must not cost the ONE-SHOT callers any slack: a font blob
    /// read here is interned for the life of the process, so an over-reserved
    /// `Vec` would be permanently resident waste.
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

        let mut opened = open_regular(&path).expect("open admitted handle");
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

    /// The residency rule has ONE definition, and it is the system font
    /// volume: a file there maps; a file under any user-writable directory
    /// — every `$HOME`-joined search dir and `/Library/Fonts` included — is
    /// copied; and a `..` traversal that lexically begins on the volume is
    /// refused rather than mapped.
    #[test]
    fn maps_in_place_admits_the_system_font_volume_and_nothing_user_writable() {
        assert!(maps_in_place(Path::new("/System/Library/Fonts/Menlo.ttc")));
        assert!(maps_in_place(Path::new(
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"
        )));
        assert!(!maps_in_place(Path::new("/Library/Fonts/Custom.ttf")));
        assert!(!maps_in_place(Path::new(
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
        )));
        assert!(!maps_in_place(Path::new(
            "/System/Library/Fonts/../../../Users//x/y.ttf"
        )));
        assert!(!maps_in_place(Path::new("System/Library/Fonts/Menlo.ttc")));
        for dir in crate::font_search_dirs() {
            let candidate = dir.join("Font.ttf");
            let on_volume = MAPPABLE_FONT_ROOTS.iter().any(|root| dir.starts_with(root));
            assert_eq!(
                maps_in_place(&candidate),
                on_volume,
                "{}: a search dir maps only when it IS the system font volume",
                candidate.display()
            );
        }
        // The per-user directories in particular: they are what a user
        // rewrites in place, and they are the ones the resolver prefers.
        if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
            for user_dir in ["Library/Fonts", ".fonts", ".local/share/fonts"] {
                assert!(
                    !maps_in_place(&home.join(user_dir).join("Font.ttf")),
                    "~/{user_dir} is user-writable and must be COPIED"
                );
            }
        }
    }

    /// The mapping arm answers the SAME bytes the copy arm does, and the
    /// residency policy is [`maps_in_place`]: a file outside the system font
    /// volume — here a fixture directory, standing in for every user-writable
    /// path — is copied, one on the volume is mapped (unix). The on-volume
    /// case uses a real system font when the host has one; there is no
    /// fixture path on a read-only volume.
    #[test]
    fn admit_font_file_maps_only_the_system_font_volume_and_matches_the_copy() {
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

        #[cfg(unix)]
        {
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
                        // `symlink_metadata`: admission is O_NOFOLLOW, so a
                        // symlinked font is (rightly) refused and is not the
                        // regular file this half wants.
                        && std::fs::symlink_metadata(path)
                            .is_ok_and(|m| m.is_file() && m.len() > 0)
                });
            let Some(system_font) = candidate else {
                eprintln!(
                    "SKIP (mapping half): no font file on the system font volume on this host"
                );
                std::fs::remove_dir_all(root).expect("remove font-file fixtures");
                return;
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
            // Two mappings of one inode know they are the same file without a
            // byte compare — the discovery intern's convergence test.
            let again = admit_font_file(&system_font).expect("a system font admits twice");
            match (&admitted, &again) {
                (crate::font::FaceBytes::Mapped(a), crate::font::FaceBytes::Mapped(b)) => {
                    assert!(a.same_file(b));
                    assert!(admitted.same_bytes(&again));
                }
                _ => unreachable!("both admissions were asserted mapped"),
            }
        }
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }

    #[cfg(unix)]
    #[test]
    fn writerless_fifo_and_final_symlink_return_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let root = fixture_dir("special");
        let fifo = root.join("face.fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path");
        // SAFETY: `fifo_c` is a live NUL-terminated pathname and mkfifo retains
        // no pointer. The private test directory makes the name unique.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let error = read_bounded_font_file(&fifo, 64)
            .expect_err("writerless FIFO must fail without blocking");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        let regular = root.join("regular.ttf");
        let linked = root.join("linked.ttf");
        std::fs::write(&regular, b"font").expect("write symlink target");
        std::os::unix::fs::symlink(&regular, &linked).expect("create final symlink");
        assert!(read_bounded_font_file(&linked, 64).is_err());
        std::fs::remove_dir_all(root).expect("remove font-file fixtures");
    }
}
