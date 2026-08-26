// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A READ-ONLY tar reader for signed bundles — the parser that retired the
//! `tar` crate (and, with it, `xattr` and `filetime`) from the shipped graph.
//!
//! # Scope, deliberately small
//!
//! aterm NEVER WRITES a tar archive. It reads exactly one kind: the
//! `.tar.zst` payload of a signed package bundle, produced by the system
//! `tar` on the publisher's machine (bsdtar on macOS, GNU tar on Linux) and
//! opened by [`crate::extract`]. So this is a parser, not a library: no
//! builder, no `unpack`, no permissions/xattr/mtime restoration — the
//! extractor sets its own sanitized mode on every file it writes, which is
//! why the retired crate's `xattr` and `filetime` dependencies had no
//! customer here either.
//!
//! # What it must understand
//!
//! * **USTAR** — the 512-byte header, POSIX octal fields, the `prefix`/`name`
//!   split for paths over 100 bytes.
//! * **GNU long names** — typeflag `L` (path) and `K` (link target), whose
//!   body is the real name and whose *next* header is the entry it names. GNU
//!   tar emits these by default for a long path.
//! * **PAX extended headers** — typeflag `x`, `"<len> <key>=<value>\n"`
//!   records. bsdtar's default "restricted pax" emits them for long paths,
//!   high ids and sub-second times. Three keys are read: `path`, `linkpath`
//!   and `size`. A `g` (global) header is NOT an extension header here — see
//!   below.
//! * **GNU base-256 numbers** — a numeric field whose top bit is set is
//!   big-endian binary, not octal. That is how a file over 8 GiB states its
//!   size. Only the fields that DEFINE the extension accept it (`size`);
//!   `mode` and `chksum` are octal-only.
//!
//! # Where two tar readers stop agreeing
//!
//! Everything in this section is a rule that only exists because a *second*
//! reader would answer differently, and a bundle that two readers describe
//! differently is a bundle whose file list can be substituted after vetting.
//! Each of these is pinned by a case in `tests/tar_oracle.rs`.
//!
//! * **A PAX `size` record OVERRIDES the header's `size` field.** Without
//!   that, an `x` header saying `size=4` in front of a ustar header saying
//!   `size=512` makes the two readers disagree about where the NEXT header
//!   starts, and from there about the entire rest of the archive.
//! * **The FIRST record of a repeated key wins**, for `path`, `linkpath` and
//!   `size` alike. Last-wins and first-wins are both defensible; being the
//!   only reader that picks one of them is not.
//! * **A malformed PAX record is SKIPPED, not fatal, and does not end the
//!   scan** — later records still apply. (For `size` alone the lookup gives
//!   up at the first malformed record, because that is what the retired
//!   reader's `pax_extensions_value` did.)
//! * **A `g` (PAX global) header is an ORDINARY entry**, classified
//!   [`EntryType::Other`] and refused upstream as a disallowed kind. It is
//!   never allowed to supply a `path`. A global header is a default for every
//!   following member, which is not a thing this extractor restores, and
//!   honouring one as a rename primitive would hand a crafted archive a name
//!   override that `vet_entry` has never been exercised against.
//! * **`L`/`K`/`x` are extension headers only with ustar or GNU MAGIC.** A
//!   header carrying the typeflag but neither magic is an ordinary entry —
//!   again `Other`, again refused. Otherwise a magic-less `L` turns an archive
//!   that every other reader rejects into one that installs under an
//!   attacker-supplied long name.
//! * **A second `L`, `K` or `x` before the same member is a REJECTION.** Two
//!   long names for one member is two answers to "what is this file called".
//! * **`prefix` is read only from a POSIX ustar header** (magic `"ustar\0"`
//!   AND version `"00"`). In the GNU layout those same bytes are
//!   `atime`/`ctime`/`offset`, so reading them as a path prefix invents a
//!   directory component out of a timestamp.
//!
//! # Security posture
//!
//! This parses ATTACKER-INFLUENCED bytes (a downloaded bundle) before its
//! signature has bought anything, so the rules are hard:
//!
//! * **It never panics.** Every field is bounds-checked, every arithmetic step
//!   is checked or saturating, and a malformed field is an `io::Error`, never
//!   an index out of range. `#![forbid(unsafe_code)]` is inherited from the
//!   crate.
//! * **It never allocates on an attacker's word.** A declared size is a `u64`
//!   the archive chose; nothing here reserves memory for it. Long-name and PAX
//!   bodies — the only variable-length reads — are read through the CALLER's
//!   reader, which is [`crate::extract::extract_tar_zst`]'s budget-capped
//!   wrapper, and are additionally refused outright past
//!   [`MAX_EXTENSION_BODY`].
//! * **It reads strictly forward.** Every skip is a bounded `read` loop into a
//!   fixed stack buffer, so a huge declared size costs time, not memory — and
//!   the caller's budget stops the time too.
//! * **It does not interpret paths.** `..`, absolute paths, symlink targets
//!   and disallowed entry kinds are the extractor's `vet_entry` /
//!   `vet_hardlink` decision, unchanged; this layer hands them the exact bytes
//!   the archive carried, so that vetting still sees the truth.
//!
//! # Evidence
//!
//! `tests/tar_oracle.rs` keeps the retired `tar` crate as a dev-dependency and
//! runs both readers over the same bytes — hand-built adversarial headers,
//! real system-`tar` archives, and tens of thousands of mutated ones —
//! asserting they AGREE on accept-or-reject and, where both accept, on every
//! field the extractor reads.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// One tar block. Every header is exactly this, and every entry's content is
/// padded up to a multiple of it.
const BLOCK: usize = 512;

/// The largest GNU-longname / PAX extension body this reader will read.
///
/// An extension header declares its body length in the same `size` field a
/// file uses, so without a ceiling a 2^64 declaration would ask the reader to
/// stream forever. 1 MiB dwarfs any real `PATH_MAX` name or PAX record set,
/// and the extractor's own `TAR_ENTRY_STRUCTURAL_BUDGET` caps it again from
/// outside — belt and braces, because this is the one place the parser holds
/// attacker-chosen bytes in memory at all.
const MAX_EXTENSION_BODY: u64 = 1024 * 1024;

/// The tar typeflag byte, as the entry kinds this reader distinguishes.
///
/// Only the variants [`crate::extract`] classifies are named; everything else
/// — FIFOs, devices, GNU sparse files, unknown vendor flags — lands in
/// [`EntryType::Other`], which the extractor refuses as a disallowed kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    /// A regular file (`0`, or the historical NUL).
    Regular,
    /// A "continuation" of a regular file (`7`). Treated as regular, as every
    /// reader does — no tar in living memory splits files this way.
    Continuous,
    /// A directory (`5`).
    Directory,
    /// A symbolic link (`2`); its target is the header's link name.
    Symlink,
    /// A hard link (`1`); its target is the header's link name.
    Link,
    /// Anything else — refused upstream.
    Other,
}

impl EntryType {
    /// Classify a raw typeflag byte.
    fn from_byte(b: u8) -> Self {
        match b {
            b'0' | 0 => Self::Regular,
            b'7' => Self::Continuous,
            b'5' => Self::Directory,
            b'2' => Self::Symlink,
            b'1' => Self::Link,
            _ => Self::Other,
        }
    }
}

/// The 512-byte header of one entry, kept raw so every accessor reads the
/// bytes the archive actually carried.
pub struct Header {
    raw: [u8; BLOCK],
}

/// Byte offsets of the POSIX ustar header fields. Named rather than inlined,
/// because an off-by-one in this table is the whole class of bug that makes a
/// tar parser read someone else's field.
mod field {
    /// `name`: 100 bytes.
    pub const NAME: (usize, usize) = (0, 100);
    /// `mode`: 8 bytes, octal.
    pub const MODE: (usize, usize) = (100, 8);
    /// `size`: 12 bytes, octal or base-256.
    pub const SIZE: (usize, usize) = (124, 12);
    /// `chksum`: 8 bytes, octal — excluded from its own sum.
    pub const CHKSUM: (usize, usize) = (148, 8);
    /// `typeflag`: 1 byte.
    pub const TYPEFLAG: usize = 156;
    /// `linkname`: 100 bytes.
    pub const LINKNAME: (usize, usize) = (157, 100);
    /// `magic`: 6 bytes — `"ustar\0"` (POSIX) or `"ustar "` (GNU).
    pub const MAGIC: (usize, usize) = (257, 6);
    /// `version`: 2 bytes — `"00"` (POSIX) or `" \0"` (GNU). Read TOGETHER with
    /// the magic: the two formats differ by one byte of magic and both bytes of
    /// version, and the fields that follow are laid out differently in each.
    pub const VERSION: (usize, usize) = (263, 2);
    /// `prefix`: 155 bytes, prepended to `name` with a `/` when non-empty.
    /// POSIX ustar ONLY — the GNU header puts `atime`/`ctime`/`offset` here.
    pub const PREFIX: (usize, usize) = (345, 155);
}

/// A malformed-header error, spelled once so every rejection reads the same.
fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("tar: {what}"))
}

impl Header {
    /// A field's raw bytes. Returns an empty slice rather than panicking if
    /// the offsets ever went wrong — this parser has no path to an index
    /// panic, by construction rather than by review.
    fn field(&self, (off, len): (usize, usize)) -> &[u8] {
        self.raw.get(off..off + len).unwrap_or(&[])
    }

    /// A field's bytes with the trailing NUL/space padding removed — the form
    /// every text field is stored in.
    fn trimmed(&self, spec: (usize, usize)) -> &[u8] {
        let f = self.field(spec);
        let end = f.iter().position(|&b| b == 0).unwrap_or(f.len());
        f.get(..end).unwrap_or(&[])
    }

    /// Is this a POSIX ustar header — i.e. does `prefix` mean anything?
    ///
    /// Magic AND version, both exact. `starts_with(b"ustar")` is TRUE for a GNU
    /// header (magic `"ustar "`), and in the GNU layout bytes 345..500 are
    /// `atime`/`ctime`/`offset`, not `prefix`: reading them as a path prefix
    /// manufactures a directory component out of a timestamp, which is a
    /// different path from the one every other reader produces for the same
    /// bytes. The retired reader required both fields exactly, and so does
    /// this.
    fn is_ustar(&self) -> bool {
        self.field(field::MAGIC) == b"ustar\0" && self.field(field::VERSION) == b"00"
    }

    /// Is this a GNU header (magic `"ustar "`, version `" \0"`)?
    ///
    /// GNU is a different layout, not a dialect of ustar: it shares the first
    /// 345 bytes and diverges after them. Nothing here reads a GNU-only field —
    /// the format matters only because GNU is one of the two magics that make
    /// an `L`/`K`/`x` typeflag mean "extension header".
    fn is_gnu(&self) -> bool {
        self.field(field::MAGIC) == b"ustar " && self.field(field::VERSION) == b" \0"
    }

    /// Is this a header whose EXTENSION typeflags are meaningful?
    ///
    /// `L`, `K` and `x` are extensions defined by GNU tar and POSIX.1-2001; a
    /// header carrying one of those bytes with neither magic is not speaking
    /// either format, so the byte is just a vendor typeflag and the block is an
    /// ordinary entry — which [`EntryType::Other`] then gets refused for. The
    /// retired reader gated all four the same way
    /// (`as_gnu().is_some() || as_ustar().is_some()`), and the gap matters:
    /// without it a magic-less `L` header renames the entry after it, turning
    /// an archive every other reader refuses into one that installs.
    fn is_recognized(&self) -> bool {
        self.is_ustar() || self.is_gnu()
    }

    /// The entry's kind.
    #[must_use]
    pub fn entry_type(&self) -> EntryType {
        EntryType::from_byte(self.raw.get(field::TYPEFLAG).copied().unwrap_or(0))
    }

    /// The archive's recorded permission bits.
    ///
    /// # Errors
    /// When the field is not a valid number. The extractor treats a mode as a
    /// hint anyway — it writes its own sanitized `0o755`/`0o644` — but a
    /// malformed field still means a malformed archive.
    pub fn mode(&self) -> io::Result<u32> {
        let v = self.octal(field::MODE, "mode")?;
        // Modes live in 12 bits; anything wider is noise from a hostile or
        // broken writer and is not worth carrying into `safe_mode`.
        Ok(u32::try_from(v & 0o7777).unwrap_or(0o644))
    }

    /// The number of CONTENT bytes this entry occupies in the archive.
    ///
    /// # Errors
    /// When the size field is not a valid number.
    pub fn entry_size(&self) -> io::Result<u64> {
        self.numeric(field::SIZE, "size")
    }

    /// The stored checksum field. OCTAL ONLY — see [`Header::octal`].
    fn stored_checksum(&self) -> io::Result<u64> {
        self.octal(field::CHKSUM, "checksum")
    }

    /// Parse a header field as NUL-terminated, whitespace-padded OCTAL, and
    /// nothing else.
    ///
    /// This is the parser for every field that POSIX defines as octal and the
    /// GNU base-256 extension does not cover: `mode` and `chksum`. The retired
    /// reader used its octal-only path for exactly these two (`num_field_wrapper`
    /// — the base-256-aware one — was reserved for `size`/`uid`/`gid`/`mtime`),
    /// and the difference is not cosmetic on either field:
    ///
    /// * `mode`: a high-bit mode made the retired reader return `Err`, and
    ///   `extract.rs` falls back to `0o644` on `Err` — non-executable. Accepting
    ///   base-256 there would let an attacker-chosen number carry the `0o111`
    ///   bits that drive `safe_mode` to `0o755` instead.
    /// * `chksum`: a high-bit checksum field was a hard reject there. Accepting
    ///   base-256 makes a header constructible whose base-256 checksum equals
    ///   its own byte sum — a whole extra family of accepted headers, on the one
    ///   field whose entire job is to refuse them.
    ///
    /// The permissiveness that IS kept is the permissiveness every reader has
    /// to have for forty years of real archives: cut at the first NUL, trim the
    /// surrounding whitespace, then parse what is left. Writers pad with spaces
    /// on either side, and a stray `\n` in a checksum field is legal.
    ///
    /// # Errors
    /// On a field that is not octal text.
    fn octal(&self, spec: (usize, usize), what: &str) -> io::Result<u64> {
        let f = self.field(spec);
        if f.is_empty() {
            return Err(bad("truncated header field"));
        }
        let end = f.iter().position(|&b| b == 0).unwrap_or(f.len());
        let digits = f.get(..end).unwrap_or(&[]);
        let text =
            std::str::from_utf8(digits).map_err(|_| bad(&format!("non-text {what} field")))?;
        u64::from_str_radix(text.trim(), 8)
            .map_err(|_| bad(&format!("{what} field is not an octal number")))
    }

    /// Parse a numeric header field that MAY use the GNU base-256 extension:
    /// big-endian binary when the top bit of the first byte is set, otherwise
    /// [`octal`](Header::octal).
    ///
    /// Only `size` is read through here, and `size` is one of the fields the
    /// extension was defined for (a file over 8 GiB cannot state its length in
    /// 11 octal digits).
    ///
    /// The base-256 arm reproduces the retired reader's reading EXACTLY,
    /// including the part that looks like a bug: for a field WIDER than 8 bytes
    /// — `size` is 12 — only the LAST 8 bytes are read, and the leading bytes,
    /// flag byte included, are ignored. So `0x80 0x01 0x00…0x00` is size ZERO,
    /// not 2^80. Reading all 12 bytes and rejecting the overflow would be the
    /// stricter rule, and stricter is usually right — but not here: it makes
    /// this the only reader in the world that refuses that archive, which is
    /// the same disagreement class the rest of this module exists to close.
    /// The value cannot overflow, because 8 bytes is exactly a `u64`.
    ///
    /// # Errors
    /// On a field that is neither base-256 nor octal text.
    fn numeric(&self, spec: (usize, usize), what: &str) -> io::Result<u64> {
        let f = self.field(spec);
        let Some(&first) = f.first() else {
            return Err(bad("truncated header field"));
        };
        if first & 0x80 == 0 {
            return self.octal(spec, what);
        }
        // The last 8 bytes, big-endian — with the flag bit stripped only when
        // the field is exactly 8 wide and the flag byte IS one of the 8 read.
        // (A field narrower than 8 cannot occur in this table; the arm is
        // written total anyway rather than left to an underflow.)
        let mut acc: u64 = 0;
        let skip = if f.len() > 8 {
            f.len().saturating_sub(8)
        } else {
            acc = u64::from(first & 0x7f);
            1
        };
        for &b in f.iter().skip(skip) {
            // `acc` holds at most 7 bytes' worth before each step (8 bytes are
            // consumed in total), so neither operation can overflow.
            acc = acc.wrapping_mul(256).wrapping_add(u64::from(b));
        }
        Ok(acc)
    }

    /// Does the header's own checksum match its bytes?
    ///
    /// The sum is the UNSIGNED total of all 512 bytes with the checksum field
    /// itself replaced by eight spaces.
    ///
    /// Unsigned ONLY, deliberately. A handful of pre-POSIX writers summed the
    /// bytes as signed, and a reader that accepts both readings accepts every
    /// header whose 128-or-above bytes happen to cancel — which is a second,
    /// weaker acceptance rule on attacker-influenced bytes, bought for the
    /// benefit of writers that predate this project by decades. The retired
    /// reader accepted only the unsigned sum, aterm's bundles are written by
    /// today's bsdtar and GNU tar, and `tests/tar_oracle.rs` keeps the two
    /// answers identical — a mutated header that the signed rule would have
    /// waved through is exactly what found this.
    ///
    /// The sum cannot overflow: 512 bytes of at most 255 is 130 560.
    fn checksum_ok(&self) -> bool {
        let Ok(stored) = self.stored_checksum() else {
            return false;
        };
        let (cs_off, cs_len) = field::CHKSUM;
        let mut unsigned: u64 = 0;
        for (i, &b) in self.raw.iter().enumerate() {
            let b = if i >= cs_off && i < cs_off + cs_len {
                b' '
            } else {
                b
            };
            unsigned += u64::from(b);
        }
        unsigned == stored
    }

    /// Is this block entirely zero — the archive's end marker?
    fn is_zero_block(&self) -> bool {
        self.raw.iter().all(|&b| b == 0)
    }

    /// The path bytes this header alone declares: `prefix/name` on a ustar
    /// header with a non-empty prefix, otherwise `name`.
    fn path_bytes(&self) -> Vec<u8> {
        let name = self.trimmed(field::NAME);
        if self.is_ustar() {
            let prefix = self.trimmed(field::PREFIX);
            if !prefix.is_empty() {
                let mut out = Vec::with_capacity(prefix.len() + 1 + name.len());
                out.extend_from_slice(prefix);
                out.push(b'/');
                out.extend_from_slice(name);
                return out;
            }
        }
        name.to_vec()
    }

    /// The link-target bytes this header alone declares.
    fn link_bytes(&self) -> Vec<u8> {
        self.trimmed(field::LINKNAME).to_vec()
    }
}

/// Turn raw archive bytes into a path.
///
/// On unix any byte string is a legal path, so this is a reinterpretation, not
/// a decode — which is the point: the extractor's vetting must see exactly
/// what the archive carried, not a lossily-repaired version of it. On Windows
/// there is no such thing as a non-UTF-8 path, so invalid UTF-8 is refused
/// rather than mangled.
#[cfg(unix)]
fn bytes_to_path(bytes: &[u8]) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

/// Windows arm of [`bytes_to_path`].
///
/// # Errors
/// When the bytes are not UTF-8 — unrepresentable as a Windows path.
#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8]) -> io::Result<PathBuf> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(PathBuf::from(s)),
        Err(_) => Err(bad("path is not valid unicode")),
    }
}

/// A tar archive being read forward from a stream.
pub struct Archive<R> {
    inner: R,
}

impl<R: Read> Archive<R> {
    /// Wrap a reader positioned at the first header block.
    pub const fn new(reader: R) -> Self {
        Self { inner: reader }
    }

    /// Begin walking the entries.
    ///
    /// # Errors
    /// Never at this point — the signature mirrors the retired crate's so the
    /// extractor's error plumbing is unchanged, and so a future seek-based
    /// variant can fail here without a call-site change.
    pub fn entries(&mut self) -> io::Result<Entries<'_, R>> {
        Ok(Entries {
            inner: &mut self.inner,
            skip: 0,
            finished: false,
        })
    }
}

/// A forward walk over an archive's entries.
///
/// Deliberately NOT an `Iterator`: each entry borrows the walk (it reads its
/// content straight from the same stream), which is a lending iterator, and
/// spelling it as an inherent [`next_entry`](Entries::next_entry) keeps that
/// honest instead of reaching for interior mutability to fake the trait.
pub struct Entries<'a, R> {
    inner: &'a mut R,
    /// Bytes of the previous entry (unread content + its block padding) that
    /// must be discarded before the next header can be read.
    skip: u64,
    /// Set once the end marker or a clean EOF has been seen; the walk then
    /// yields nothing forever, so a caller that keeps asking cannot re-enter
    /// the parser on trailing bytes.
    finished: bool,
}

impl<'a, R: Read> Entries<'a, R> {
    /// Discard exactly `n` bytes from the stream.
    ///
    /// A fixed stack buffer, in a loop: a declared size is an attacker's
    /// number, so skipping it must cost time proportional to the bytes that
    /// actually exist, never memory proportional to the claim. A short read
    /// before `n` is a truncated archive.
    fn discard(&mut self, mut n: u64) -> io::Result<()> {
        let mut buf = [0u8; BLOCK];
        while n > 0 {
            let want = usize::try_from(n.min(BLOCK as u64)).unwrap_or(BLOCK);
            let slice = buf.get_mut(..want).ok_or_else(|| bad("skip overflow"))?;
            let read = self.inner.read(slice)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "tar: archive ends inside an entry",
                ));
            }
            // `read <= want <= n` by the `Read` contract; saturating carries
            // the no-underflow proof without trusting the callee's return.
            n = n.saturating_sub(read as u64);
        }
        Ok(())
    }

    /// Fill `buf` completely; `Ok(false)` means a clean EOF at offset zero.
    fn read_exact_or_eof(&mut self, buf: &mut [u8]) -> io::Result<bool> {
        let mut filled = 0usize;
        while filled < buf.len() {
            let Some(rest) = buf.get_mut(filled..) else {
                return Err(bad("header read overflow"));
            };
            let n = self.inner.read(rest)?;
            if n == 0 {
                if filled == 0 {
                    return Ok(false);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "tar: truncated header block",
                ));
            }
            filled = filled.saturating_add(n);
        }
        Ok(true)
    }

    /// Read one raw 512-byte header block.
    fn read_header(&mut self) -> io::Result<Option<Header>> {
        let mut raw = [0u8; BLOCK];
        if !self.read_exact_or_eof(&mut raw)? {
            return Ok(None);
        }
        Ok(Some(Header { raw }))
    }

    /// Read an extension header's body (`size` bytes plus its block padding).
    ///
    /// The body is the ONLY variable-length thing this parser holds in memory,
    /// so it is streamed in fixed blocks and GROWN — never pre-allocated from
    /// the declared size, which is a number the archive chose and may be 2^64.
    /// The ceiling is checked as it grows, so a hostile declaration costs at
    /// most [`MAX_EXTENSION_BODY`] of memory whatever it claims, and the
    /// caller's own reader (the extractor's budget-capped wrapper, which is
    /// charged for the header block too) reaches its limit first in the
    /// shipped path — so a bomb still surfaces as the extractor's `TooLarge`,
    /// not as this parser's rejection.
    fn read_extension_body(&mut self, size: u64) -> io::Result<Vec<u8>> {
        let mut body: Vec<u8> = Vec::new();
        let mut buf = [0u8; BLOCK];
        let mut left = size;
        while left > 0 {
            if body.len() as u64 >= MAX_EXTENSION_BODY {
                return Err(bad("extension header body is implausibly large"));
            }
            let want = usize::try_from(left.min(BLOCK as u64)).unwrap_or(BLOCK);
            let slice = buf
                .get_mut(..want)
                .ok_or_else(|| bad("extension body read overflow"))?;
            let n = self.inner.read(slice)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "tar: truncated extension header",
                ));
            }
            body.extend_from_slice(slice.get(..n).unwrap_or(&[]));
            // `n <= want <= left` by the `Read` contract; saturating carries
            // the no-underflow proof without trusting the callee's return.
            left = left.saturating_sub(n as u64);
        }
        self.discard(padding_for(size))?;
        Ok(body)
    }

    /// Advance to the next entry, or `Ok(None)` at the end of the archive.
    ///
    /// GNU `L`/`K` and PAX `x` headers are consumed here and folded into the
    /// entry they describe, so a caller only ever sees real entries — and,
    /// crucially, their bodies are read through the SAME reader the caller
    /// supplied, which is how the extractor's per-entry budget gets to bound
    /// them. A PAX `g` (global) header is NOT consumed: it falls through as an
    /// ordinary entry, which [`EntryType::Other`] and the extractor's
    /// `DisallowedKind` then refuse.
    ///
    /// # Errors
    /// On a truncated stream, a header whose checksum does not match, a
    /// malformed numeric field, a size that cannot be advanced past, a repeated
    /// extension header for one member, or an extension header describing an
    /// entry that never arrives.
    pub fn next_entry(&mut self) -> io::Result<Option<Entry<'_, 'a, R>>> {
        if self.finished {
            return Ok(None);
        }
        let pending = self.skip;
        self.skip = 0;
        self.discard(pending)?;

        // Overrides accumulated from extension headers preceding this entry.
        //
        // GNU and PAX are tracked SEPARATELY rather than overwriting one
        // another, because when an archive carries both for the same entry the
        // GNU long name wins whatever order they appeared in — that is the
        // precedence the retired reader had, and an entry's path is not a
        // detail to resolve differently: it is what `vet_entry` judges and
        // what lands on disk. Order-of-arrival precedence would let a crafted
        // archive show one reader one path and another reader another.
        let mut gnu_path: Option<Vec<u8>> = None;
        let mut gnu_link: Option<Vec<u8>> = None;
        let mut pax: Option<PaxRecords> = None;
        // Whether an extension header has been consumed and is still waiting
        // for the entry it describes. An archive that ENDS there — at EOF or
        // at the zero-block marker — is truncated, not finished: something
        // announced a name for an entry that never arrived. Ending the walk
        // cleanly would silently drop that entry, so it is a rejection.
        let mut saw_extension = false;

        loop {
            let Some(header) = self.read_header()? else {
                // A clean EOF with no end marker. Real archives always carry
                // the marker, but a stream that simply stops between entries
                // is not on its own a reason to fail: the extractor's
                // tree_root comparison is what decides whether the payload was
                // complete. Ending on a DANGLING extension header is a
                // different thing — see `saw_extension`.
                if saw_extension {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "tar: extension header names an entry that never arrives",
                    ));
                }
                self.finished = true;
                return Ok(None);
            };
            if header.is_zero_block() {
                if saw_extension {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "tar: extension header names an entry that never arrives",
                    ));
                }
                self.finished = true;
                return Ok(None);
            }
            if !header.checksum_ok() {
                return Err(bad("header checksum mismatch"));
            }
            let size = header.entry_size()?;
            let flag = header.raw.get(field::TYPEFLAG).copied().unwrap_or(0);
            // GNU SPARSE (`S`) is REFUSED, not parsed.
            //
            // A sparse entry's content is not the bytes that follow it: the
            // header carries a map of (offset, length) chunks, optionally
            // continued into further blocks, and the real file is those chunks
            // interleaved with implicit zero runs. Two readers that disagree
            // about that map disagree about the file's bytes AND about where
            // the next header starts — the same class this whole module is
            // about, with a much larger surface.
            //
            // The retired reader implemented it, and errored on essentially
            // every sparse header that is not perfectly consistent (its
            // `real_size` alone rejects an all-zero field, because an empty
            // string is not an octal number). Refusing outright is therefore
            // the same answer for every archive either reader is likely to
            // meet, and it is the RIGHT answer for a bundle regardless: aterm's
            // bundles are ordinary files written by `tar -c`, a sparse entry is
            // classified `Other`, and `vet_entry` aborts the whole staged group
            // on an `Other` entry anyway. So refusing costs nothing and removes
            // a parser. `tests/tar_oracle.rs` pins that both readers refuse
            // every sparse archive it can build.
            if flag == b'S' {
                return Err(bad("GNU sparse entries are not supported"));
            }
            // An extension typeflag only MEANS an extension on a header that
            // speaks ustar or GNU. Without this gate a magic-less `L` renames
            // the entry after it — an archive every other reader refuses,
            // installed under an attacker-supplied name. See
            // [`Header::is_recognized`].
            let extension = header.is_recognized();
            match flag {
                // GNU long name / long link: the body IS the name, and the
                // next header is the entry it belongs to.
                b'L' | b'K' if extension => {
                    // A SECOND long name for one member is two answers to
                    // "what is this file called", and last-wins would make the
                    // answer depend on which parser you asked. The retired
                    // reader rejected the archive; so does this — and it
                    // decides BEFORE reading the body, so a duplicate costs the
                    // extractor's budget nothing.
                    let slot = if flag == b'L' {
                        &mut gnu_path
                    } else {
                        &mut gnu_link
                    };
                    if slot.is_some() {
                        return Err(bad("two long name entries describing the same member"));
                    }
                    // Charge the advance BEFORE reading, so an unadvanceable
                    // size is refused on an extension header exactly as it is
                    // on an ordinary one.
                    advance_for(size)?;
                    let mut body = self.read_extension_body(size)?;
                    saw_extension = true;
                    // GNU writes the name NUL-terminated, and exactly ONE
                    // trailing NUL comes off. Truncating at the FIRST NUL
                    // instead would silently shorten a name that carries an
                    // interior NUL — a different path than the archive
                    // declared, and therefore a different thing for
                    // `vet_entry` to judge than the one that will be written.
                    // The retired reader dropped only the terminator, and the
                    // oracle holds this to that.
                    if body.last() == Some(&0) {
                        body.pop();
                    }
                    *slot = Some(body);
                }
                // PAX extended header. `x` applies to the next entry; only
                // `path`, `linkpath` and `size` are read — every other record
                // (times, ids, xattrs) describes metadata this extractor
                // deliberately does not restore.
                //
                // `g` is DELIBERATELY ABSENT from this arm. A global header is
                // a default for every following member, and honouring one here
                // would make it a rename primitive; falling through leaves it
                // an ordinary entry, classified `Other` and refused upstream,
                // which is what the retired reader did.
                b'x' if extension => {
                    if pax.is_some() {
                        return Err(bad("two pax extensions entries describing the same member"));
                    }
                    advance_for(size)?;
                    let body = self.read_extension_body(size)?;
                    saw_extension = true;
                    pax = Some(parse_pax(&body));
                }
                _ => {
                    let pax = pax.unwrap_or_default();
                    // A PAX `size` record OVERRIDES the header's size field —
                    // otherwise the two readers disagree about where the next
                    // header begins, and from there about the whole archive.
                    //
                    // …except on a header whose TYPEFLAG is an extension flag.
                    // An extension header's own body length is its own
                    // business, and the retired reader suppressed the override
                    // on the TYPEFLAG ALONE — before, and independently of, the
                    // magic check that decides whether the block is actually
                    // treated as an extension header. So a `K` header with a
                    // bad magic reaches this arm as an ordinary entry AND keeps
                    // its own size field, while still taking the `path`
                    // override (which that reader applied unconditionally).
                    // Two independent gates on two nearly-identical conditions
                    // is not a design anyone would choose; it is a shape a
                    // crafted archive can aim at, which is exactly why it is
                    // reproduced here instead of tidied.
                    let extension_flag = matches!(flag, b'L' | b'K' | b'x' | b'g');
                    let size = if extension_flag {
                        size
                    } else {
                        pax.size.unwrap_or(size)
                    };
                    let advance = advance_for(size)?;
                    let path_bytes = gnu_path.or(pax.path).unwrap_or_else(|| header.path_bytes());
                    // An override that EXISTS but is empty is still an
                    // override: `Some("")`, not `None`. Only the header's own
                    // `linkname` field collapses an empty value to "no link
                    // target", because there the emptiness is the absence. A
                    // zero-length `K` body or an empty `linkpath` record is an
                    // archive SAYING the link target is the empty string, and
                    // flattening the two makes a hardlink entry look
                    // target-less to one reader and empty-targeted to the
                    // other — different `vet_hardlink` inputs from one archive.
                    let link_bytes = gnu_link.or(pax.link).or_else(|| {
                        let raw = header.link_bytes();
                        (!raw.is_empty()).then_some(raw)
                    });
                    let path = bytes_to_path(&path_bytes)?;
                    let link_name = match link_bytes {
                        Some(bytes) => Some(bytes_to_path(&bytes)?),
                        None => None,
                    };
                    // What the NEXT `next_entry` must discard if the caller
                    // reads none of this entry's content.
                    self.skip = advance;
                    return Ok(Some(Entry {
                        parent: self,
                        header,
                        path,
                        link_name,
                        size,
                        remaining: size,
                    }));
                }
            }
        }
    }
}

/// Bytes to advance past an entry of `size` content bytes: the size itself plus
/// its zero padding, rounded up to a whole block.
///
/// # Errors
/// When the rounding would overflow a `u64` — a `size` within 511 of `u64::MAX`.
/// The retired reader refused those with its own `checked_add` before yielding
/// the entry, and refusing at the same MOMENT matters: saturating instead would
/// hand the caller one entry the other reader never produced before failing on
/// the following read.
fn advance_for(size: u64) -> io::Result<u64> {
    let block = BLOCK as u64;
    let rounded = size
        .checked_add(block - 1)
        .ok_or_else(|| bad("entry size overflows the block rounding"))?;
    Ok(rounded & !(block - 1))
}

/// Bytes of zero padding after `size` content bytes, rounding up to a block.
fn padding_for(size: u64) -> u64 {
    let rem = size % BLOCK as u64;
    if rem == 0 { 0 } else { BLOCK as u64 - rem }
}

/// The three PAX records this reader acts on.
///
/// `Default` is "no override at all", which is what an entry with no `x`
/// header in front of it gets.
#[derive(Default)]
struct PaxRecords {
    /// A `path` record — the entry's real name.
    path: Option<Vec<u8>>,
    /// A `linkpath` record — the entry's real link target.
    link: Option<Vec<u8>>,
    /// A `size` record — the entry's real content length, OVERRIDING the
    /// header's `size` field.
    size: Option<u64>,
}

/// Split one PAX record — `"<len> <key>=<value>"`, newline already removed —
/// into its key and value.
///
/// The declared `<len>` counts the WHOLE record including its own digits and
/// the newline, so it is a self-describing length a hostile archive can make
/// inconsistent; a record whose declared length does not match the line it was
/// actually split from is malformed and yields `None`. That single check is
/// also what makes an embedded newline malformed for free: the newline split
/// the line early, so the declared length no longer matches.
///
/// # Returns
/// `None` for any malformed record — no space, a non-numeric length, a length
/// that disagrees with the line, or no `=`.
fn pax_record(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let sp = line.iter().position(|&b| b == b' ')?;
    let digits = line.get(..sp)?;
    let reported: usize = std::str::from_utf8(digits).ok()?.parse().ok()?;
    // `+ 1` for the newline the split consumed.
    if line.len().checked_add(1)? != reported {
        return None;
    }
    let kv = line.get(sp.checked_add(1)?..)?;
    let eq = kv.iter().position(|&b| b == b'=')?;
    Some((kv.get(..eq)?, kv.get(eq.checked_add(1)?..)?))
}

/// Parse a PAX extended-header body into the records this reader acts on.
///
/// The body is a sequence of newline-terminated records, and it is parsed as
/// exactly that: SPLIT ON NEWLINES, one record per line. Three rules come out
/// of that shape, and all three are the retired reader's, because each one is a
/// place where picking the other answer would make two readers describe one
/// archive differently:
///
/// * **An EMPTY line ends the scan.** The body's trailing newline produces one,
///   which is the normal termination; a blank line in the middle stops it too.
/// * **The FIRST record of a repeated key wins.** A body carrying
///   `path=a.txt` then `path=../evil` resolves to `a.txt`.
/// * **A malformed record is SKIPPED, not fatal, and does not end the scan.**
///   Later records still apply, so a junk record cannot suppress a real `path`
///   that follows it. Nothing is lost by the leniency: a record that does not
///   parse supplies no override, so the entry falls back to the name in its own
///   header, which the extractor vets exactly as it vets every other name.
///
/// `size` alone has a fourth rule: its lookup GIVES UP at the first malformed
/// record, rather than skipping past it. That asymmetry is not a design choice
/// here, it is the retired reader's `pax_extensions_value` (which returns `None`
/// on the first `Err` the record iterator yields) as against its `path_bytes`
/// (which filters errors out and takes the first match) — and since the whole
/// point of the `size` override is that both readers agree on where the next
/// header starts, matching it exactly is the only version worth having.
///
/// Termination is structural: the scan is a `split` over a finite slice.
fn parse_pax(body: &[u8]) -> PaxRecords {
    let mut out = PaxRecords::default();
    // Set once the `size` lookup has stopped — at the first malformed record,
    // or at the first `size` record, whichever comes first.
    let mut size_settled = false;
    for line in body.split(|&b| b == b'\n') {
        if line.is_empty() {
            break;
        }
        let Some((key, value)) = pax_record(line) else {
            size_settled = true;
            continue;
        };
        match key {
            b"path" if out.path.is_none() => out.path = Some(value.to_vec()),
            b"linkpath" if out.link.is_none() => out.link = Some(value.to_vec()),
            b"size" if !size_settled => {
                size_settled = true;
                // A `size` value that is not a plain decimal `u64` supplies no
                // override — the retired reader's `value.parse::<u64>()`, whose
                // failure it turns into `None` rather than an error.
                out.size = std::str::from_utf8(value).ok().and_then(|v| v.parse().ok());
            }
            // Every other record is metadata this extractor does not restore
            // (atime/mtime/uid/gid/xattrs/sparse maps), so it is read past
            // rather than interpreted — as is a repeat of a key already taken.
            _ => {}
        }
    }
    out
}

/// One entry of an archive: its header, its resolved names, and a [`Read`]
/// over its content.
///
/// Borrows the walk it came from, so reading content advances the same stream
/// the next header will be read from — there is no second cursor to get out of
/// step with.
pub struct Entry<'e, 'a, R> {
    parent: &'e mut Entries<'a, R>,
    header: Header,
    path: PathBuf,
    link_name: Option<PathBuf>,
    /// The entry's EFFECTIVE content length — the header's `size` field, or the
    /// PAX `size` record where one overrode it. This is the number that decides
    /// where the next header begins, so it is the number every caller must use.
    size: u64,
    /// Content bytes not yet handed to the caller.
    remaining: u64,
}

impl<R: Read> Entry<'_, '_, R> {
    /// The entry's raw header.
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    /// The entry's EFFECTIVE content length.
    ///
    /// Use this, never `header().entry_size()`. A PAX `size` record overrides
    /// the header field, and the override is exactly the thing a crafted
    /// archive uses to make two readers see two different file lists: the
    /// header says 512 and the `x` record says 4, and a caller that trusts the
    /// header is describing bytes that are actually the next entry's. This is
    /// the number the walk itself advances by.
    #[must_use]
    pub const fn entry_size(&self) -> u64 {
        self.size
    }

    /// The entry's path, as the archive declared it — including any GNU
    /// longname or PAX `path` override.
    ///
    /// # Errors
    /// Never; the fallible decode happened when the entry was read. The
    /// signature mirrors the retired crate's so the extractor's `?` plumbing
    /// is unchanged.
    pub fn path(&self) -> io::Result<std::borrow::Cow<'_, Path>> {
        Ok(std::borrow::Cow::Borrowed(self.path.as_path()))
    }

    /// The entry's link target for a symlink or hard link, `None` otherwise.
    ///
    /// # Errors
    /// Never, for the same reason as [`path`](Self::path).
    pub fn link_name(&self) -> io::Result<Option<std::borrow::Cow<'_, Path>>> {
        Ok(self.link_name.as_deref().map(std::borrow::Cow::Borrowed))
    }
}

impl<R: Read> Read for Entry<'_, '_, R> {
    /// Hand out content bytes, and NEVER more than the header declared: the
    /// entry ends at its own size, so a caller reading to EOF cannot walk into
    /// the next header. The bytes the caller does take are debited from what
    /// the walk must skip afterwards.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let want = usize::try_from(self.remaining.min(buf.len() as u64)).unwrap_or(0);
        let Some(slice) = buf.get_mut(..want) else {
            return Ok(0);
        };
        let n = self.parent.inner.read(slice)?;
        // `n <= want <= remaining` by the `Read` contract; saturating carries
        // the no-underflow proof without trusting the callee's return value.
        self.remaining = self.remaining.saturating_sub(n as u64);
        self.parent.skip = self.parent.skip.saturating_sub(n as u64);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::{Archive, EntryType, MAX_EXTENSION_BODY};
    use std::io::Read;

    /// Write `bytes` at `off` in a header block, clipped at the block's end.
    fn put(h: &mut [u8; 512], off: usize, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            if let Some(slot) = h.get_mut(off + i) {
                *slot = *b;
            }
        }
    }

    /// Fill in a header's checksum field the way every writer does: the
    /// UNSIGNED sum of all 512 bytes with the field itself read as spaces.
    fn seal(h: &mut [u8; 512]) {
        h[148..156].fill(b' ');
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        put(h, 148, format!("{sum:06o}\0 ").as_bytes());
    }

    /// A POSIX ustar header.
    fn header(name: &str, flag: u8, size: u64) -> [u8; 512] {
        header_magic(name, flag, size, b"ustar\0", b"00")
    }

    /// A header with the magic and version chosen by the caller.
    fn header_magic(name: &str, flag: u8, size: u64, magic: &[u8], version: &[u8]) -> [u8; 512] {
        let mut h = [0u8; 512];
        put(&mut h, 0, name.as_bytes());
        put(&mut h, 100, b"0000644\0");
        put(&mut h, 108, b"0000000\0");
        put(&mut h, 116, b"0000000\0");
        put(&mut h, 124, format!("{size:011o}\0").as_bytes());
        put(&mut h, 136, b"00000000000\0");
        h[156] = flag;
        put(&mut h, 257, magic);
        put(&mut h, 263, version);
        seal(&mut h);
        h
    }

    /// Content padded up to the next block.
    fn padded(body: &[u8]) -> Vec<u8> {
        let mut v = body.to_vec();
        let rem = v.len() % 512;
        if rem != 0 {
            v.resize(v.len() + (512 - rem), 0);
        }
        v
    }

    /// The two zero blocks that end an archive.
    fn end() -> Vec<u8> {
        vec![0u8; 1024]
    }

    /// One PAX `"<len> key=value\n"` record, with its self-describing length.
    fn pax(key: &str, value: &str) -> Vec<u8> {
        let payload = format!("{key}={value}\n");
        let mut len = payload.len() + 2;
        loop {
            let candidate = format!("{len} {payload}");
            if candidate.len() == len {
                return candidate.into_bytes();
            }
            len = candidate.len();
        }
    }

    /// One entry, reduced to what the extractor reads.
    #[derive(Debug, PartialEq, Eq)]
    struct Seen {
        path: String,
        kind: EntryType,
        size: u64,
        link: Option<String>,
        content: Vec<u8>,
    }

    /// Walk an archive to the end, or to the first error.
    fn walk(bytes: &[u8]) -> Result<Vec<Seen>, String> {
        walk_reader(bytes)
    }

    /// [`walk`] over any reader, so the short-read paths can be driven too.
    fn walk_reader<R: Read>(reader: R) -> Result<Vec<Seen>, String> {
        let mut archive = Archive::new(reader);
        let mut entries = archive.entries().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        loop {
            let Some(mut entry) = entries.next_entry().map_err(|e| e.to_string())? else {
                return Ok(out);
            };
            let path = entry
                .path()
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned();
            let kind = entry.header().entry_type();
            let size = entry.entry_size();
            let link = entry
                .link_name()
                .map_err(|e| e.to_string())?
                .map(|l| l.to_string_lossy().into_owned());
            let mut content = Vec::new();
            entry.read_to_end(&mut content).map_err(|e| e.to_string())?;
            out.push(Seen {
                path,
                kind,
                size,
                link,
                content,
            });
        }
    }

    /// A reader that hands out at most `chunk` bytes per `read` call.
    ///
    /// Every loop in this module is written against the `Read` contract rather
    /// than against "one call fills the buffer", and a reader that satisfies
    /// the contract while never filling a buffer is the only thing that proves
    /// it. A zstd decoder behaves exactly like this in the shipped path.
    struct Dribble<'a> {
        bytes: &'a [u8],
        chunk: usize,
    }

    impl Read for Dribble<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.bytes.len().min(buf.len()).min(self.chunk);
            buf[..n].copy_from_slice(&self.bytes[..n]);
            self.bytes = &self.bytes[n..];
            Ok(n)
        }
    }

    // -----------------------------------------------------------------------
    // Golden evidence, held IN TREE
    //
    // The differential oracle lives in `tests/tar_oracle.rs` and depends on the
    // retired crate still being a dev-dependency. These do not: they are the
    // evidence that survives the day someone drops that dev-dep, in the same
    // shape `crates/aterm-hash` pins golden vectors for the hash it replaced.
    // -----------------------------------------------------------------------

    /// The shapes a real bundle is made of, decoded to exact expected values.
    #[test]
    fn golden_walk_of_an_ordinary_archive() {
        let mut v = Vec::new();
        v.extend_from_slice(&header("bin/aterm", b'0', 7));
        v.extend_from_slice(&padded(b"payload"));
        v.extend_from_slice(&header("share/", b'5', 0));
        v.extend_from_slice(&header("alias", b'2', 0));
        // A symlink's target lives in the header's linkname field.
        let mut sym = header("alias", b'2', 0);
        put(&mut sym, 157, b"bin/aterm");
        seal(&mut sym);
        v.truncate(v.len() - 512);
        v.extend_from_slice(&sym);
        v.extend_from_slice(&header("empty.txt", b'0', 0));
        v.extend_from_slice(&end());

        let seen = walk(&v).expect("walk");
        assert_eq!(
            seen,
            vec![
                Seen {
                    path: "bin/aterm".into(),
                    kind: EntryType::Regular,
                    size: 7,
                    link: None,
                    content: b"payload".to_vec(),
                },
                Seen {
                    path: "share/".into(),
                    kind: EntryType::Directory,
                    size: 0,
                    link: None,
                    content: Vec::new(),
                },
                Seen {
                    path: "alias".into(),
                    kind: EntryType::Symlink,
                    size: 0,
                    link: Some("bin/aterm".into()),
                    content: Vec::new(),
                },
                Seen {
                    path: "empty.txt".into(),
                    kind: EntryType::Regular,
                    size: 0,
                    link: None,
                    content: Vec::new(),
                },
            ]
        );
    }

    /// A ustar `prefix` joins to `name` with a slash — and ONLY on a POSIX
    /// header. On a GNU header those bytes are `atime`/`ctime`/`offset`.
    #[test]
    fn golden_prefix_is_posix_ustar_only() {
        for (magic, version, expected) in [
            (&b"ustar\0"[..], &b"00"[..], "some/prefix/tail.txt"),
            (&b"ustar "[..], &b" \0"[..], "tail.txt"),
            (&b"ustar "[..], &b"00"[..], "tail.txt"),
            (&b"xxxxx\0"[..], &b"00"[..], "tail.txt"),
        ] {
            let mut h = header_magic("tail.txt", b'0', 0, magic, version);
            put(&mut h, 345, b"some/prefix");
            seal(&mut h);
            let seen = walk(&[&h[..], &end()].concat()).expect("walk");
            assert_eq!(seen.len(), 1);
            assert_eq!(
                seen[0].path, expected,
                "magic {magic:?} version {version:?} must resolve to {expected}"
            );
        }
    }

    /// A GNU long name replaces the header's, and exactly ONE trailing NUL
    /// comes off — an interior NUL is part of the name.
    #[test]
    fn golden_gnu_long_name_drops_one_trailing_nul() {
        for (label, body, expected) in [
            ("terminated", &b"real/name.txt\0"[..], "real/name.txt"),
            ("unterminated", &b"real/name.txt"[..], "real/name.txt"),
            ("two nuls", &b"real/name.txt\0\0"[..], "real/name.txt\u{0}"),
            (
                "interior nul",
                &b"real/\0name.txt\0"[..],
                "real/\u{0}name.txt",
            ),
        ] {
            let mut v = Vec::new();
            v.extend_from_slice(&header("././@LongLink", b'L', body.len() as u64));
            v.extend_from_slice(&padded(body));
            v.extend_from_slice(&header("decoy.txt", b'0', 0));
            v.extend_from_slice(&end());
            let seen = walk(&v).expect("walk");
            assert_eq!(seen.len(), 1, "{label}");
            assert_eq!(seen[0].path, expected, "{label}");
        }
    }

    /// A PAX `size` record decides the entry's length and therefore where the
    /// NEXT header begins.
    #[test]
    fn golden_pax_size_overrides_the_header_field() {
        let rec = pax("size", "4");
        let mut v = Vec::new();
        v.extend_from_slice(&header("PaxHeader", b'x', rec.len() as u64));
        v.extend_from_slice(&padded(&rec));
        v.extend_from_slice(&header("f.txt", b'0', 512));
        v.extend_from_slice(&padded(&vec![b'Z'; 512]));
        v.extend_from_slice(&header("g.txt", b'0', 3));
        v.extend_from_slice(&padded(b"xyz"));
        v.extend_from_slice(&end());

        let seen = walk(&v).expect("walk");
        // Reading the header's 512 instead would swallow g.txt's header whole.
        assert_eq!(seen.len(), 2, "the size override moved the next header");
        assert_eq!(seen[0].size, 4);
        assert_eq!(seen[0].content, b"ZZZZ");
        assert_eq!(seen[1].path, "g.txt");
        assert_eq!(seen[1].content, b"xyz");
    }

    /// `Entry::entry_size` is the EFFECTIVE size; `header().entry_size()` is
    /// the raw field. The extractor's directory guard reads the former.
    #[test]
    fn golden_effective_and_raw_sizes_are_both_reachable() {
        let rec = pax("size", "4");
        let mut v = Vec::new();
        v.extend_from_slice(&header("PaxHeader", b'x', rec.len() as u64));
        v.extend_from_slice(&padded(&rec));
        v.extend_from_slice(&header("f.txt", b'0', 512));
        v.extend_from_slice(&padded(&vec![b'Z'; 512]));
        v.extend_from_slice(&end());

        let mut archive = Archive::new(&v[..]);
        let mut entries = archive.entries().expect("entries");
        let entry = entries.next_entry().expect("ok").expect("some");
        assert_eq!(entry.entry_size(), 4, "effective size");
        assert_eq!(
            entry.header().entry_size().expect("raw size"),
            512,
            "raw header field"
        );
    }

    /// The FIRST record of a repeated key wins, and a malformed record neither
    /// ends the scan nor suppresses what follows it.
    #[test]
    fn golden_pax_precedence_and_recovery() {
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            (
                "first path wins",
                {
                    let mut b = pax("path", "FIRST.txt");
                    b.extend_from_slice(&pax("path", "SECOND.txt"));
                    b
                },
                "FIRST.txt",
            ),
            (
                "malformed then valid",
                {
                    let mut b = b"999 linkpath=y\n".to_vec();
                    b.extend_from_slice(&pax("path", "GOOD.txt"));
                    b
                },
                "GOOD.txt",
            ),
            (
                "valid then malformed",
                {
                    let mut b = pax("path", "GOOD.txt");
                    b.extend_from_slice(b"999 linkpath=y\n");
                    b
                },
                "GOOD.txt",
            ),
            (
                "blank line stops the scan",
                {
                    let mut b = pax("mtime", "0");
                    b.push(b'\n');
                    b.extend_from_slice(&pax("path", "AFTER.txt"));
                    b
                },
                "hdr.txt",
            ),
            (
                "embedded newline is malformed",
                pax("path", "a\nb"),
                "hdr.txt",
            ),
        ];
        for (label, body, expected) in cases {
            let mut v = Vec::new();
            v.extend_from_slice(&header("PaxHeader", b'x', body.len() as u64));
            v.extend_from_slice(&padded(&body));
            v.extend_from_slice(&header("hdr.txt", b'0', 0));
            v.extend_from_slice(&end());
            let seen = walk(&v).expect("walk");
            assert_eq!(seen.len(), 1, "{label}");
            assert_eq!(seen[0].path, expected, "{label}");
        }
    }

    /// A GNU long name beats a PAX `path` whatever order they arrive in.
    #[test]
    fn golden_gnu_long_name_beats_a_pax_path_in_both_orders() {
        let gnu = b"gnu.txt\0";
        let px = pax("path", "pax.txt");
        for order in [true, false] {
            let mut v = Vec::new();
            let emit_gnu = |v: &mut Vec<u8>| {
                v.extend_from_slice(&header("././@LongLink", b'L', gnu.len() as u64));
                v.extend_from_slice(&padded(gnu));
            };
            let emit_pax = |v: &mut Vec<u8>| {
                v.extend_from_slice(&header("PaxHeader", b'x', px.len() as u64));
                v.extend_from_slice(&padded(&px));
            };
            if order {
                emit_gnu(&mut v);
                emit_pax(&mut v);
            } else {
                emit_pax(&mut v);
                emit_gnu(&mut v);
            }
            v.extend_from_slice(&header("hdr.txt", b'0', 0));
            v.extend_from_slice(&end());
            let seen = walk(&v).expect("walk");
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].path, "gnu.txt", "gnu long name must win");
        }
    }

    /// An override that EXISTS but is empty is `Some("")`, not `None`.
    #[test]
    fn golden_empty_link_override_is_not_absence() {
        let mut v = Vec::new();
        v.extend_from_slice(&header("././@LongLink", b'K', 0));
        v.extend_from_slice(&header("hard", b'1', 0));
        v.extend_from_slice(&end());
        let seen = walk(&v).expect("walk");
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].link,
            Some(String::new()),
            "empty K body is a target"
        );

        // …while a header with no linkname at all really has none.
        let mut v = Vec::new();
        v.extend_from_slice(&header("hard", b'1', 0));
        v.extend_from_slice(&end());
        let seen = walk(&v).expect("walk");
        assert_eq!(seen[0].link, None);
    }

    /// A base-256 `size` reads the LOW EIGHT BYTES of the 12-byte field and
    /// ignores everything above them.
    #[test]
    fn golden_base256_size_reads_the_low_eight_bytes() {
        let mut h = header("x.txt", b'0', 0);
        h[124] = 0x80;
        h[125] = 0x01; // above the low 8 bytes: ignored
        h[126..135].fill(0);
        h[135] = 4;
        seal(&mut h);
        let seen = walk(&[&h[..], &padded(b"data"), &end()].concat()).expect("walk");
        assert_eq!(seen[0].size, 4, "leading bytes above the low 8 are ignored");
        assert_eq!(seen[0].content, b"data");
    }

    // -----------------------------------------------------------------------
    // Deliberate deviations — behaviour the differential oracle CANNOT assert,
    // because asserting it would mean asserting a disagreement.
    // -----------------------------------------------------------------------

    /// The [`MAX_EXTENSION_BODY`] ceiling. The retired crate had no such limit,
    /// so any input that reaches this refusal is by definition one the oracle
    /// corpus cannot contain — which is exactly why it is tested here.
    #[test]
    fn extension_body_ceiling_refuses_an_oversized_long_name() {
        let over = MAX_EXTENSION_BODY + 512;
        let mut v = Vec::new();
        v.extend_from_slice(&header("././@LongLink", b'L', over));
        v.extend_from_slice(&padded(&vec![b'n'; usize::try_from(over).expect("fits")]));
        v.extend_from_slice(&header("decoy.txt", b'0', 0));
        v.extend_from_slice(&end());
        let err = walk(&v).expect_err("an oversized extension body must be refused");
        assert!(err.contains("implausibly large"), "unexpected error: {err}");

        // A body one block under the ceiling is still read.
        let under = MAX_EXTENSION_BODY - 512;
        let mut body = vec![b'n'; usize::try_from(under).expect("fits")];
        body.push(0);
        body.truncate(usize::try_from(under).expect("fits"));
        let mut v = Vec::new();
        v.extend_from_slice(&header("././@LongLink", b'L', under));
        v.extend_from_slice(&padded(&body));
        v.extend_from_slice(&header("decoy.txt", b'0', 0));
        v.extend_from_slice(&end());
        let seen = walk(&v).expect("a body under the ceiling must be read");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].path.len(), usize::try_from(under).expect("fits"));
    }

    /// GNU sparse (`S`) is refused outright rather than parsed.
    #[test]
    fn gnu_sparse_entries_are_refused() {
        for (magic, version) in [(&b"ustar "[..], &b" \0"[..]), (&b"ustar\0"[..], &b"00"[..])] {
            let mut v = Vec::new();
            v.extend_from_slice(&header("good.txt", b'0', 4));
            v.extend_from_slice(&padded(b"good"));
            v.extend_from_slice(&header_magic("sparse.bin", b'S', 0, magic, version));
            v.extend_from_slice(&end());
            let err = walk(&v).expect_err("a sparse entry must be refused");
            assert!(err.contains("sparse"), "unexpected error: {err}");
        }
    }

    /// The checksum is the UNSIGNED byte sum and nothing else.
    ///
    /// A handful of pre-POSIX writers summed the bytes as SIGNED, and a reader
    /// that accepts both readings accepts every header whose ≥128 bytes happen
    /// to cancel — a second, weaker acceptance rule on attacker-influenced
    /// bytes. This builds a header that passes the signed rule and fails the
    /// unsigned one, and requires the refusal.
    #[test]
    fn a_signed_only_checksum_is_refused() {
        let mut h = header("x.txt", b'0', 0);
        // A high byte in the name field makes the signed and unsigned sums
        // differ by 256; seal to the SIGNED total.
        h[5] = 0x80;
        h[148..156].fill(b' ');
        let signed: i64 = h.iter().map(|&b| i64::from(b as i8)).sum();
        let unsigned: i64 = h.iter().map(|&b| i64::from(b)).sum();
        assert_ne!(
            signed, unsigned,
            "the fixture must distinguish the two rules"
        );
        put(
            &mut h,
            148,
            format!("{:06o}\0 ", u64::try_from(signed).expect("positive")).as_bytes(),
        );
        let err = walk(&[&h[..], &end()].concat()).expect_err("signed-only checksum");
        assert!(err.contains("checksum"), "unexpected error: {err}");
    }

    // -----------------------------------------------------------------------
    // Streaming, truncation and the short-read paths
    // -----------------------------------------------------------------------

    /// Every loop honours the `Read` contract rather than assuming a full
    /// buffer: the same archive through a one-byte-at-a-time reader must decode
    /// identically.
    #[test]
    fn a_dribbling_reader_decodes_identically() {
        let rec = pax("size", "4");
        let mut v = Vec::new();
        v.extend_from_slice(&header("PaxHeader", b'x', rec.len() as u64));
        v.extend_from_slice(&padded(&rec));
        v.extend_from_slice(&header("f.txt", b'0', 512));
        v.extend_from_slice(&padded(&vec![b'Z'; 512]));
        v.extend_from_slice(&header("././@LongLink", b'L', 14));
        v.extend_from_slice(&padded(b"real/name.txt\0"));
        v.extend_from_slice(&header("decoy.txt", b'0', 3));
        v.extend_from_slice(&padded(b"abc"));
        v.extend_from_slice(&end());

        let whole = walk(&v).expect("walk");
        for chunk in [1usize, 3, 7, 511, 512, 513] {
            let dribbled = walk_reader(Dribble { bytes: &v, chunk }).expect("dribbled walk");
            assert_eq!(dribbled, whole, "chunk size {chunk} changed the decode");
        }
    }

    /// A stream that stops mid-archive is an error, not a short archive —
    /// except between entries, where it is a clean end.
    #[test]
    fn truncation_is_an_error_inside_an_entry_and_clean_between_them() {
        let mut v = Vec::new();
        v.extend_from_slice(&header("a.txt", b'0', 600));
        v.extend_from_slice(&padded(&vec![b'A'; 600]));
        v.extend_from_slice(&header("b.txt", b'0', 4));
        v.extend_from_slice(&padded(b"BBBB"));
        v.extend_from_slice(&end());

        // Cut inside a.txt's content.
        assert!(walk(&v[..900]).is_err(), "cut inside content");
        // Cut inside b.txt's header.
        assert!(walk(&v[..1800]).is_err(), "cut inside a header");
        // Cut exactly between entries: a clean end, one entry.
        let seen = walk(&v[..1536]).expect("cut between entries");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].path, "a.txt");
        // Nothing at all.
        assert_eq!(walk(&[]).expect("empty"), Vec::new());
    }

    /// An extension header describing an entry that never arrives is a
    /// rejection, not a silently shorter archive — at EOF and at the end
    /// marker alike.
    #[test]
    fn a_dangling_extension_header_is_refused() {
        for (label, tail) in [("eof", Vec::new()), ("end marker", end())] {
            for flag in [b'L', b'K', b'x'] {
                let body = b"dangling\0".to_vec();
                let mut v = Vec::new();
                v.extend_from_slice(&header("././@LongLink", flag, body.len() as u64));
                v.extend_from_slice(&padded(&body));
                v.extend_from_slice(&tail);
                let err = walk(&v).expect_err("dangling extension");
                assert!(
                    err.contains("never arrives"),
                    "{label}/{}: unexpected error: {err}",
                    flag as char
                );
            }
        }
    }

    /// A second `L`, `K` or `x` before one member is a rejection.
    #[test]
    fn duplicate_extension_headers_are_refused() {
        for flag in [b'L', b'K', b'x'] {
            let body = if flag == b'x' {
                pax("path", "p.txt")
            } else {
                b"n.txt\0".to_vec()
            };
            let mut v = Vec::new();
            for _ in 0..2 {
                v.extend_from_slice(&header("././@LongLink", flag, body.len() as u64));
                v.extend_from_slice(&padded(&body));
            }
            v.extend_from_slice(&header("decoy.txt", b'0', 0));
            v.extend_from_slice(&end());
            let err = walk(&v).expect_err("duplicate extension header");
            assert!(
                err.contains("same member"),
                "{}: unexpected error: {err}",
                flag as char
            );
        }
    }

    /// An extension typeflag with neither magic is an ordinary entry, and a
    /// `g` header is an ordinary entry whatever its magic.
    #[test]
    fn unrecognized_and_global_headers_are_ordinary_entries() {
        // Magic-less `L`: two entries, the first classified `Other`.
        let body = b"real/name.txt\0".to_vec();
        let mut v = Vec::new();
        let mut h = header_magic("././@LongLink", b'L', body.len() as u64, b"xxxxx\0", b"00");
        seal(&mut h);
        v.extend_from_slice(&h);
        v.extend_from_slice(&padded(&body));
        v.extend_from_slice(&header("decoy.txt", b'0', 5));
        v.extend_from_slice(&padded(b"decoy"));
        v.extend_from_slice(&end());
        let seen = walk(&v).expect("walk");
        assert_eq!(seen.len(), 2, "a magic-less L must not be consumed");
        assert_eq!(seen[0].kind, EntryType::Other);
        assert_eq!(seen[1].path, "decoy.txt", "the name must NOT be overridden");

        // `g`, with every magic: an ordinary `Other` entry that supplies no
        // override to the entry after it.
        for (magic, version) in [
            (&b"ustar\0"[..], &b"00"[..]),
            (&b"ustar "[..], &b" \0"[..]),
            (&b"xxxxx\0"[..], &b"00"[..]),
        ] {
            let body = pax("path", "global/override.txt");
            let mut v = Vec::new();
            v.extend_from_slice(&header_magic(
                "GlobalHead",
                b'g',
                body.len() as u64,
                magic,
                version,
            ));
            v.extend_from_slice(&padded(&body));
            v.extend_from_slice(&header("plain.txt", b'0', 0));
            v.extend_from_slice(&end());
            let seen = walk(&v).expect("walk");
            assert_eq!(seen.len(), 2, "a g header is an entry, not an extension");
            assert_eq!(seen[0].kind, EntryType::Other);
            assert_eq!(seen[1].path, "plain.txt", "a g header must not rename");
        }
    }

    /// `mode` and `chksum` are octal-only; a high-bit value in either is a
    /// malformed field, not a base-256 number.
    #[test]
    fn mode_and_checksum_reject_the_base256_form() {
        // A base-256 mode: the header is otherwise valid, so the walk succeeds
        // and only the mode read fails — which is what `extract.rs`'s
        // `unwrap_or(0o644)` turns into a non-executable file.
        let mut h = header("x.txt", b'0', 0);
        h[100] = 0x80;
        h[101..107].fill(0);
        h[107] = 0o355;
        seal(&mut h);
        let bytes = [&h[..], &end()].concat();
        let mut archive = Archive::new(&bytes[..]);
        let mut entries = archive.entries().expect("entries");
        let entry = entries.next_entry().expect("ok").expect("some");
        assert!(
            entry.header().mode().is_err(),
            "a base-256 mode must not parse as a number"
        );

        // A base-256 checksum, made numerically equal to the header's own byte
        // sum: accepted by a base-256-aware reader, refused here.
        let mut h = header("x.txt", b'0', 0);
        h[148..156].fill(b' ');
        let sum: u64 = h.iter().map(|&b| u64::from(b)).sum();
        h[148] = 0x80;
        for (i, shift) in (0..7).enumerate() {
            h[155 - i] = ((sum >> (8 * shift)) & 0xff) as u8;
        }
        let err = walk(&[&h[..], &end()].concat()).expect_err("base-256 checksum");
        assert!(err.contains("checksum"), "unexpected error: {err}");
    }

    /// A size within a block of `u64::MAX` cannot be advanced past, and that is
    /// a refusal BEFORE the entry is yielded — not one entry followed by a
    /// failure, which would be a different entry list from any other reader's.
    #[test]
    fn an_unadvanceable_size_is_refused_before_the_entry() {
        let mut h = header("x.txt", b'0', 0);
        h[124] = 0x80;
        h[125..136].fill(0xff);
        seal(&mut h);
        let err = walk(&[&h[..], &end()].concat()).expect_err("size overflow");
        assert!(err.contains("overflow"), "unexpected error: {err}");
    }

    /// An entry hands out exactly its declared bytes and never reads into the
    /// next header, and an entry the caller never reads is still skipped
    /// correctly — the shape the extractor uses for directories and hardlinks.
    #[test]
    fn content_is_bounded_and_unread_content_is_skipped() {
        let mut v = Vec::new();
        v.extend_from_slice(&header("skipme.bin", b'0', 1000));
        v.extend_from_slice(&padded(&vec![7u8; 1000]));
        v.extend_from_slice(&header("after.txt", b'0', 5));
        v.extend_from_slice(&padded(b"after"));
        v.extend_from_slice(&end());

        let mut archive = Archive::new(&v[..]);
        let mut entries = archive.entries().expect("entries");
        let first = entries.next_entry().expect("ok").expect("some");
        assert_eq!(first.entry_size(), 1000);
        drop(first);
        let mut second = entries.next_entry().expect("ok").expect("some");
        let mut body = Vec::new();
        second.read_to_end(&mut body).expect("read");
        assert_eq!(body, b"after");
        drop(second);
        assert!(entries.next_entry().expect("ok").is_none());
        // Once finished, the walk stays finished even on trailing bytes.
        assert!(entries.next_entry().expect("ok").is_none());
    }
}
