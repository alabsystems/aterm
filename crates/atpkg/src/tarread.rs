// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A read-only tar reader for the `.tar.zst` payload of a signed package bundle, written by the publisher's system `tar` (bsdtar on macOS, GNU tar on Linux) and opened by [`crate::extract`].

use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// One tar block. Every header is exactly this, and every entry's content is
/// padded up to a multiple of it.
const BLOCK: usize = 512;

/// The largest GNU-longname / PAX extension body this reader will read. Its length is
/// declared in the same `size` field a file uses, so without a ceiling a 2^64 declaration
/// would stream forever; 1 MiB dwarfs any real `PATH_MAX` name or PAX record set, and the
/// extractor's `TAR_ENTRY_STRUCTURAL_BUDGET` caps it again from outside.
const MAX_EXTENSION_BODY: u64 = 1024 * 1024;

/// The tar typeflag byte, as the entry kinds this reader distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    /// A regular file (`0`, or the historical NUL).
    Regular,
    /// A "continuation" of a regular file (`7`), treated as regular, as every reader does.
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
    #[inline]
    const fn from_byte(b: u8) -> Self {
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

/// The 512-byte header of one entry, kept raw so every accessor reads the bytes the archive actually carried.
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
    /// `version`: 2 bytes — `"00"` (POSIX) or `" \0"` (GNU).
    pub const VERSION: (usize, usize) = (263, 2);
    /// `prefix`: 155 bytes, prepended to `name` with a `/` when non-empty.
    pub const PREFIX: (usize, usize) = (345, 155);
}

/// A malformed-header error, spelled once so every rejection reads the same.
fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("tar: {what}"))
}

impl Header {
    /// A field's raw bytes, or an empty slice if the offsets were ever wrong: never an
    /// index panic.
    #[inline]
    fn field(&self, (off, len): (usize, usize)) -> &[u8] {
        self.raw.get(off..off + len).unwrap_or(&[])
    }

    /// A field's bytes with the trailing NUL/space padding removed — the form
    /// every text field is stored in.
    #[inline]
    fn trimmed(&self, spec: (usize, usize)) -> &[u8] {
        let f = self.field(spec);
        let end = f.iter().position(|&b| b == 0).unwrap_or(f.len());
        f.get(..end).unwrap_or(&[])
    }

    /// Is this a POSIX ustar header, i.e. does `prefix` mean anything? Magic and version,
    /// both exact: `starts_with(b"ustar")` is also true of a GNU header, whose bytes
    /// 345..500 are `atime`/`ctime`/`offset`.
    #[inline]
    fn is_ustar(&self) -> bool {
        self.field(field::MAGIC) == b"ustar\0" && self.field(field::VERSION) == b"00"
    }

    /// Is this a GNU header (magic `"ustar "`, version `" \0"`)? It shares the first 345
    /// bytes with ustar and matters here only as one of the two magics that make an
    /// `L`/`K`/`x` typeflag an extension header.
    #[inline]
    fn is_gnu(&self) -> bool {
        self.field(field::MAGIC) == b"ustar " && self.field(field::VERSION) == b" \0"
    }

    /// Is this a header whose extension typeflags (`L`, `K`, `x`) are meaningful? With
    /// neither magic the byte is just a vendor typeflag and the block an ordinary entry,
    /// refused as [`EntryType::Other`]; otherwise a magic-less `L` would rename the entry
    /// after it.
    #[inline]
    fn is_recognized(&self) -> bool {
        self.is_ustar() || self.is_gnu()
    }

    /// The entry's kind.
    #[inline]
    #[must_use]
    pub fn entry_type(&self) -> EntryType {
        EntryType::from_byte(self.raw.get(field::TYPEFLAG).copied().unwrap_or(0))
    }

    /// A recognized PAX global header, still yielded as an ordinary `Other` entry.
    #[inline]
    pub(crate) fn is_global_pax(&self) -> bool {
        self.is_recognized() && self.raw.get(field::TYPEFLAG) == Some(&b'g')
    }

    /// The archive's recorded permission bits.
    /// # Errors
    /// When the field is not a valid number.
    #[inline]
    pub fn mode(&self) -> io::Result<u32> {
        let v = self.octal(field::MODE, "mode")?;
        Ok(u32::try_from(v & 0o7777).unwrap_or(0o644))
    }

    /// The number of content bytes this entry occupies in the archive.
    /// # Errors
    /// When the size field is not a valid number.
    #[inline]
    pub fn entry_size(&self) -> io::Result<u64> {
        self.numeric(field::SIZE, "size")
    }

    /// The stored checksum field. Octal only: see [`Header::octal`].
    #[inline]
    fn stored_checksum(&self) -> io::Result<u64> {
        self.octal(field::CHKSUM, "checksum")
    }

    /// Parse a header field as NUL-terminated, whitespace-padded octal, and nothing else:
    /// the parser for `mode` and `chksum`, which the GNU base-256 extension does not cover
    /// (the `tar` crate reads them octal-only too).
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

    /// Parse a numeric header field that may use the GNU base-256 extension: big-endian
    /// binary when the first byte's top bit is set, otherwise [`octal`](Header::octal).
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
        let mut acc: u64 = 0;
        let skip = if f.len() > 8 {
            f.len().saturating_sub(8)
        } else {
            acc = u64::from(first & 0x7f);
            1
        };
        for &b in f.iter().skip(skip) {
            // `acc` holds at most 7 bytes' worth before each step (8 bytes are
            acc = acc.wrapping_mul(256).wrapping_add(u64::from(b));
        }
        Ok(acc)
    }

    /// Does the header's own checksum match its bytes? The sum is the unsigned total of all
    /// 512 bytes with the checksum field read as eight spaces.
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
    #[inline]
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
#[cfg(unix)]
fn bytes_to_path(bytes: &[u8]) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

/// Windows arm of [`bytes_to_path`].
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
    #[inline]
    pub const fn new(reader: R) -> Self {
        Self { inner: reader }
    }

    /// Begin walking the entries.
    /// # Errors
    /// Never at this point; the signature leaves room for a variant that can fail here.
    #[inline]
    pub const fn entries(&mut self) -> io::Result<Entries<'_, R>> {
        Ok(Entries {
            inner: &mut self.inner,
            skip: 0,
            finished: false,
        })
    }

    /// The reader, positioned wherever the walk stopped: after the end marker's first block once [`Entries::next_entry`] has answered `None`.
    #[inline]
    pub fn into_inner(self) -> R {
        self.inner
    }
}

/// A forward walk over an archive's entries.
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
    /// Discard exactly `n` bytes through a fixed stack buffer, so an attacker's declared
    /// size costs time proportional to the bytes that exist, never memory.
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

    /// Read an extension header's body (`size` bytes plus its block padding): the only
    /// variable-length thing this parser holds in memory, so it is streamed in fixed
    /// blocks and grown, never pre-allocated from the declared size, and capped at
    /// [`MAX_EXTENSION_BODY`].
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
            left = left.saturating_sub(n as u64);
        }
        self.discard(padding_for(size))?;
        Ok(body)
    }

    /// Advance to the next entry, or `Ok(None)` at the end of the archive.
    /// # Errors
    /// On a truncated stream, a header whose checksum does not match, a malformed numeric field, a size that cannot be advanced past, a repeated extension header for one member, or an extension header describing an entry that never arrives.
    pub fn next_entry(&mut self) -> io::Result<Option<Entry<'_, 'a, R>>> {
        if self.finished {
            return Ok(None);
        }
        let pending = self.skip;
        self.skip = 0;
        self.discard(pending)?;

        let mut gnu_path: Option<Vec<u8>> = None;
        let mut gnu_link: Option<Vec<u8>> = None;
        let mut pax: Option<PaxRecords> = None;
        let mut saw_extension = false;

        loop {
            let Some(header) = self.read_header()? else {
                // A clean EOF with no end marker is not on its own a failure (the
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
            if flag == b'S' {
                return Err(bad("GNU sparse entries are not supported"));
            }
            let extension = header.is_recognized();
            match flag {
                b'L' | b'K' if extension => {
                    let slot = if flag == b'L' {
                        &mut gnu_path
                    } else {
                        &mut gnu_link
                    };
                    if slot.is_some() {
                        return Err(bad("two long name entries describing the same member"));
                    }
                    advance_for(size)?;
                    let mut body = self.read_extension_body(size)?;
                    saw_extension = true;
                    if body.last() == Some(&0) {
                        body.pop();
                    }
                    *slot = Some(body);
                }
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
                    let extension_flag = matches!(flag, b'L' | b'K' | b'x' | b'g');
                    let size = if extension_flag {
                        size
                    } else {
                        pax.size.unwrap_or(size)
                    };
                    let advance = advance_for(size)?;
                    let path_bytes = gnu_path.or(pax.path).unwrap_or_else(|| header.path_bytes());
                    let link_bytes = gnu_link.or(pax.link).or_else(|| {
                        let raw = header.link_bytes();
                        (!raw.is_empty()).then_some(raw)
                    });
                    let path = bytes_to_path(&path_bytes)?;
                    let link_name = match link_bytes {
                        Some(bytes) => Some(bytes_to_path(&bytes)?),
                        None => None,
                    };
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

/// Bytes to advance past an entry of `size` content bytes: the size plus its zero padding,
/// rounded up to a whole block.
///
/// # Errors
/// When the rounding would overflow a `u64` (a `size` within 511 of `u64::MAX`). Refused
/// before the entry is yielded, as the `tar` crate does: saturating would hand the caller
/// one entry another reader never produced.
#[inline]
fn advance_for(size: u64) -> io::Result<u64> {
    let block = BLOCK as u64;
    let rounded = size
        .checked_add(block - 1)
        .ok_or_else(|| bad("entry size overflows the block rounding"))?;
    Ok(rounded & !(block - 1))
}

/// Bytes of zero padding after `size` content bytes, rounding up to a block.
#[inline]
const fn padding_for(size: u64) -> u64 {
    let rem = size % BLOCK as u64;
    if rem == 0 { 0 } else { BLOCK as u64 - rem }
}

/// The three PAX records this reader acts on.
#[derive(Default)]
struct PaxRecords {
    /// A `path` record — the entry's real name.
    path: Option<Vec<u8>>,
    /// A `linkpath` record — the entry's real link target.
    link: Option<Vec<u8>>,
    /// A `size` record — the entry's real content length, overriding the
    /// header's `size` field.
    size: Option<u64>,
}

/// Split one PAX record (`"<len> <key>=<value>"`, newline already removed) into key and
/// value.
fn pax_record(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let sp = line.iter().position(|&b| b == b' ')?;
    let digits = line.get(..sp)?;
    let reported: usize = std::str::from_utf8(digits).ok()?.parse().ok()?;
    if line.len().checked_add(1)? != reported {
        return None;
    }
    let kv = line.get(sp.checked_add(1)?..)?;
    let eq = kv.iter().position(|&b| b == b'=')?;
    Some((kv.get(..eq)?, kv.get(eq.checked_add(1)?..)?))
}

/// Parse a PAX extended-header body into the records this reader acts on: records split on
/// newlines, one per line.
fn parse_pax(body: &[u8]) -> PaxRecords {
    let mut out = PaxRecords::default();
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
                out.size = std::str::from_utf8(value).ok().and_then(|v| v.parse().ok());
            }
            _ => {}
        }
    }
    out
}

/// One entry of an archive: its header, its resolved names, and a [`Read`] over its content.
pub struct Entry<'e, 'a, R> {
    parent: &'e mut Entries<'a, R>,
    header: Header,
    path: PathBuf,
    link_name: Option<PathBuf>,
    /// The entry's effective content length: the header's `size` field, or the PAX `size`
    /// record where one overrode it ([`Entry::entry_size`]).
    size: u64,
    /// Content bytes not yet handed to the caller.
    remaining: u64,
}

impl<R: Read> Entry<'_, '_, R> {
    /// The entry's raw header.
    #[inline]
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    /// The entry's effective content length.
    #[inline]
    #[must_use]
    pub const fn entry_size(&self) -> u64 {
        self.size
    }

    /// The entry's path as the archive declared it, including any GNU longname or PAX `path` override.
    /// # Errors
    /// Never; the fallible decode happened when the entry was read.
    #[inline]
    pub fn path(&self) -> io::Result<std::borrow::Cow<'_, Path>> {
        Ok(std::borrow::Cow::Borrowed(self.path.as_path()))
    }

    /// The entry's link target for a symlink or hard link, `None` otherwise.
    /// # Errors
    /// Never, for the same reason as [`path`](Self::path).
    #[inline]
    pub fn link_name(&self) -> io::Result<Option<std::borrow::Cow<'_, Path>>> {
        Ok(self.link_name.as_deref().map(std::borrow::Cow::Borrowed))
    }

    /// Consume bounded global metadata without applying it to later entries.
    pub(crate) fn read_descriptive_global_pax(&mut self) -> io::Result<()> {
        if !self.header.is_global_pax() {
            return Err(bad("not a recognized global PAX header"));
        }
        if self.size > MAX_EXTENSION_BODY {
            return Err(bad("global PAX body is implausibly large"));
        }
        let mut body = Vec::new();
        self.read_to_end(&mut body)?;
        if body.len() as u64 != self.size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "tar: truncated global PAX body",
            ));
        }
        for record in body.split_inclusive(|&byte| byte == b'\n') {
            let line = record
                .strip_suffix(b"\n")
                .ok_or_else(|| bad("unterminated global PAX record"))?;
            let (key, _) = pax_record(line).ok_or_else(|| bad("malformed global PAX record"))?;
            if !matches!(
                key,
                b"comment" | b"uid" | b"gid" | b"uname" | b"gname" | b"mtime" | b"atime" | b"ctime"
            ) {
                return Err(bad("unsupported global PAX key"));
            }
        }
        Ok(())
    }
}

impl<R: Read> Read for Entry<'_, '_, R> {
    /// Hand out content bytes, and never more than the header declared: the
    /// entry ends at its own size, so a caller reading to EOF cannot walk into
    /// the next header.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let want = usize::try_from(self.remaining.min(buf.len() as u64)).unwrap_or(0);
        let Some(slice) = buf.get_mut(..want) else {
            return Ok(0);
        };
        let n = self.parent.inner.read(slice)?;
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
    /// unsigned sum of all 512 bytes with the field itself read as spaces.
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

    /// A reader that hands out at most `chunk` bytes per `read` call, as a zstd decoder
    /// does in the shipped path: the proof that every loop honours the `Read` contract.
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

    /// The shapes a real bundle is made of, decoded to exact expected values.
    #[test]
    fn golden_walk_of_an_ordinary_archive() {
        let mut v = Vec::new();
        v.extend_from_slice(&header("bin/aterm", b'0', 7));
        v.extend_from_slice(&padded(b"payload"));
        v.extend_from_slice(&header("share/", b'5', 0));
        v.extend_from_slice(&header("alias", b'2', 0));
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

    /// A ustar `prefix` joins to `name` with a slash — and only on a POSIX
    /// header.
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

    /// A GNU long name replaces the header's, and exactly one trailing NUL
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
    /// next header begins.
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
        assert_eq!(seen.len(), 2, "the size override moved the next header");
        assert_eq!(seen[0].size, 4);
        assert_eq!(seen[0].content, b"ZZZZ");
        assert_eq!(seen[1].path, "g.txt");
        assert_eq!(seen[1].content, b"xyz");
    }

    /// `Entry::entry_size` is the effective size; `header().entry_size()` is
    /// the raw field.
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

    /// The first record of a repeated key wins, and a malformed record neither
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

    /// An override that exists but is empty is `Some("")`, not `None`.
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

        let mut v = Vec::new();
        v.extend_from_slice(&header("hard", b'1', 0));
        v.extend_from_slice(&end());
        let seen = walk(&v).expect("walk");
        assert_eq!(seen[0].link, None);
    }

    /// A base-256 `size` reads the low eight bytes of the 12-byte field and
    /// ignores everything above them.
    #[test]
    fn golden_base256_size_reads_the_low_eight_bytes() {
        let mut h = header("x.txt", b'0', 0);
        h[124] = 0x80;
        h[125] = 0x01;
        h[126..135].fill(0);
        h[135] = 4;
        seal(&mut h);
        let seen = walk(&[&h[..], &padded(b"data"), &end()].concat()).expect("walk");
        assert_eq!(seen[0].size, 4, "leading bytes above the low 8 are ignored");
        assert_eq!(seen[0].content, b"data");
    }

    /// The [`MAX_EXTENSION_BODY`] ceiling.
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

    /// The checksum is the unsigned byte sum only: a header that passes the signed rule and
    /// fails the unsigned one is refused.
    #[test]
    fn a_signed_only_checksum_is_refused() {
        let mut h = header("x.txt", b'0', 0);
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

        assert!(walk(&v[..900]).is_err(), "cut inside content");
        assert!(walk(&v[..1800]).is_err(), "cut inside a header");
        let seen = walk(&v[..1536]).expect("cut between entries");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].path, "a.txt");
        assert_eq!(walk(&[]).expect("empty"), Vec::new());
    }

    /// An extension header describing an entry that never arrives is a
    /// rejection, not a silently shorter archive — at EOF and at the end
    /// marker alike.
    #[test]
    fn a_dangling_extension_header_is_refused() {
        for (label, tail) in [("eof", Vec::new()), ("end marker", end())] {
            for flag in *b"LKx" {
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

    /// An extension typeflag with neither magic is an ordinary entry, and a
    /// `g` header is an ordinary entry whatever its magic.
    #[test]
    fn unrecognized_and_global_headers_are_ordinary_entries() {
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
    /// a refusal before the entry is yielded — not one entry followed by a
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
        assert!(entries.next_entry().expect("ok").is_none());
    }
}
