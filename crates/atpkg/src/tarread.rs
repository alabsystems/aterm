// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A read-only tar reader for the `.tar.zst` payload of a signed package bundle, written by the publisher's system `tar` (bsdtar on macOS, GNU tar on Linux) and opened by [`crate::extract`].

use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// One tar block (512 bytes).
const BLOCK: usize = 512;

/// The largest GNU-longname / PAX extension body this reader will read (1 MiB).
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

/// Byte offsets of the POSIX ustar header fields.
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
    /// A field's raw bytes, or an empty slice if the offsets were ever wrong.
    #[inline]
    fn field(&self, (off, len): (usize, usize)) -> &[u8] {
        self.raw.get(off..off + len).unwrap_or(&[])
    }

    /// A field's bytes with the trailing NUL padding removed.
    #[inline]
    fn trimmed(&self, spec: (usize, usize)) -> &[u8] {
        let f = self.field(spec);
        let end = f.iter().position(|&b| b == 0).unwrap_or(f.len());
        f.get(..end).unwrap_or(&[])
    }

    /// Is this a POSIX ustar header (`"ustar\0"` and `"00"`)?
    #[inline]
    fn is_ustar(&self) -> bool {
        self.field(field::MAGIC) == b"ustar\0" && self.field(field::VERSION) == b"00"
    }

    /// Is this a GNU header (magic `"ustar "`, version `" \0"`)?
    #[inline]
    fn is_gnu(&self) -> bool {
        self.field(field::MAGIC) == b"ustar " && self.field(field::VERSION) == b" \0"
    }

    /// Is this a header whose extension typeflags (`L`, `K`, `x`) are meaningful?
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

    /// Parse a header field as NUL-terminated, whitespace-padded octal (`mode` and `chksum`).
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
        let trimmed = text.trim();
        if !trimmed.bytes().all(|b| matches!(b, b'0'..=b'7')) {
            return Err(bad(&format!("{what} field is not an octal number")));
        }
        u64::from_str_radix(trimmed, 8)
            .map_err(|_| bad(&format!("{what} field is not an octal number")))
    }

    /// Parse a numeric header field that may use the GNU base-256 extension.
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
            acc = acc.wrapping_mul(256).wrapping_add(u64::from(b));
        }
        Ok(acc)
    }

    /// Does the header's own checksum match its bytes?
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

    /// The path bytes this header alone declares (`prefix/name` on ustar, else `name`).
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
    /// Bytes of the previous entry (unread content + padding) to discard before the next header.
    skip: u64,
    /// Set once the end marker or a clean EOF has been seen.
    finished: bool,
}

impl<'a, R: Read> Entries<'a, R> {
    /// Discard `n` bytes through a fixed stack buffer.
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

    /// Read an extension header's body (`size` bytes plus padding), capped at [`MAX_EXTENSION_BODY`].
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
                // signed digest and tree_root the stager checks decide whether the
                // payload was complete), unless an extension header is dangling.
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

/// Bytes to advance past an entry of `size` content bytes, rounded up to a whole block.
///
/// # Errors
/// When the rounding would overflow a `u64` (a `size` within 511 of `u64::MAX`).
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
    /// A `size` record — the entry's real content length, overriding the header's `size` field.
    size: Option<u64>,
}

/// Split one PAX record (`"<len> <key>=<value>"`, newline already removed) into key and value.
fn pax_record(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let sp = line.iter().position(|&b| b == b' ')?;
    let digits = line.get(..sp)?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let reported: usize = std::str::from_utf8(digits).ok()?.parse().ok()?;
    if line.len().checked_add(1)? != reported {
        return None;
    }
    let kv = line.get(sp.checked_add(1)?..)?;
    let eq = kv.iter().position(|&b| b == b'=')?;
    Some((kv.get(..eq)?, kv.get(eq.checked_add(1)?..)?))
}

/// Parse a PAX extended-header body into the records this reader acts on.
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
                out.size = (!value.is_empty() && value.iter().all(u8::is_ascii_digit))
                    .then(|| std::str::from_utf8(value).ok()?.parse().ok())
                    .flatten();
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
    /// The entry's effective content length ([`Entry::entry_size`]).
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
        let mut body =
            Vec::with_capacity(usize::try_from(self.size.saturating_add(1)).unwrap_or(0));
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
    /// Hand out content bytes, bounded by the entry's declared size.
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
#[path = "tarread_tests.rs"]
mod tests;
