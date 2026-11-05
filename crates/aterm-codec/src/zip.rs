// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only ZIP central-directory reader: the workspace's one ZIP reader.
//!
//! [`Archive`] works over any `Read + Seek` (a `File` for atpkg's `zip` vendor
//! payload lane, a `Cursor<&[u8]>` for an in-memory OOXML part). It locates the
//! end-of-central-directory record (ZIP64 included), walks the central directory
//! record by record with every span checked against its bounds, and opens a
//! member's body as a reader of its uncompressed bytes (store or deflate).
//! Every refusal is an `InvalidData` [`io::Error`] whose text starts `zip: `.
//!
//! # Examples
//!
//! ```rust
//! # fn main() -> std::io::Result<()> {
//! // An empty archive: just the 22-byte end-of-central-directory record.
//! let mut bytes = vec![0x50, 0x4b, 0x05, 0x06];
//! bytes.resize(22, 0);
//! let mut zip = aterm_codec::zip::Archive::open(std::io::Cursor::new(&bytes[..]))?;
//! assert!(zip.is_empty());
//! assert!(zip.find(b"xl/workbook.xml")?.is_none());
//! # Ok(())
//! # }
//! ```

use std::io::{self, Read, Seek, SeekFrom};

const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_EOCD64_LOCATOR: u32 = 0x0706_4b50;
const SIG_EOCD64: u32 = 0x0606_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_LOCAL: u32 = 0x0403_4b50;
const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
const FLAG_ENCRYPTED: u16 = 0x0001;
const FLAG_STRONG_ENCRYPTION: u16 = 0x0040;
const HOST_UNIX: u8 = 3;
const HOST_DARWIN: u8 = 19;
const ZIP64_EXTRA_ID: u16 = 0x0001;
/// The end record is 22 bytes plus a comment of at most 65535 bytes.
const EOCD_LEN: u64 = 22;
const EOCD_SEARCH: u64 = EOCD_LEN + 65_535;
const CENTRAL_FIXED: u64 = 46;
const LOCAL_FIXED: u64 = 30;
/// The most [`Archive::read`] reserves up front, whatever the record declares.
const READ_RESERVE_CAP: u64 = 1 << 20;

/// `zip: <what>` as an `InvalidData` error.
fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("zip: {what}"))
}

fn u16_at(b: &[u8], off: usize) -> io::Result<u16> {
    let s = b
        .get(off..off.saturating_add(2))
        .ok_or_else(|| bad("truncated record"))?;
    let a = <[u8; 2]>::try_from(s).map_err(|_| bad("truncated record"))?;
    Ok(u16::from_le_bytes(a))
}

fn u32_at(b: &[u8], off: usize) -> io::Result<u32> {
    let s = b
        .get(off..off.saturating_add(4))
        .ok_or_else(|| bad("truncated record"))?;
    let a = <[u8; 4]>::try_from(s).map_err(|_| bad("truncated record"))?;
    Ok(u32::from_le_bytes(a))
}

fn u64_at(b: &[u8], off: usize) -> io::Result<u64> {
    let s = b
        .get(off..off.saturating_add(8))
        .ok_or_else(|| bad("truncated record"))?;
    let a = <[u8; 8]>::try_from(s).map_err(|_| bad("truncated record"))?;
    Ok(u64::from_le_bytes(a))
}

/// Where the central directory is and how many records it holds.
#[derive(Debug, Clone, Copy)]
struct CentralDirectory {
    entries: u64,
    offset: u64,
    size: u64,
}

impl CentralDirectory {
    /// One past the directory's last byte; [`locate`] proved the sum does not overflow.
    fn end(self) -> u64 {
        self.offset.saturating_add(self.size)
    }
}

/// Find the end-of-central-directory record by a bounded backward scan and read the
/// central directory's span from it (or from the ZIP64 record it points at).
fn locate<R: Read + Seek>(file: &mut R, len: u64) -> io::Result<CentralDirectory> {
    if len < EOCD_LEN {
        return Err(bad("file too short for an end-of-central-directory record"));
    }
    let window = len.min(EOCD_SEARCH);
    file.seek(SeekFrom::Start(len.saturating_sub(window)))?;
    let mut tail = vec![0u8; window as usize];
    file.read_exact(&mut tail)?;
    let mut pos = tail.len().saturating_sub(EOCD_LEN as usize);
    loop {
        if u32_at(&tail, pos)? == SIG_EOCD {
            break;
        }
        if pos == 0 {
            return Err(bad("no end-of-central-directory record"));
        }
        pos = pos.saturating_sub(1);
    }
    let comment_len = usize::from(u16_at(&tail, pos.saturating_add(20))?);
    if pos
        .saturating_add(EOCD_LEN as usize)
        .saturating_add(comment_len)
        != tail.len()
    {
        return Err(bad(
            "trailing bytes after the end-of-central-directory record",
        ));
    }
    let disk = u16_at(&tail, pos.saturating_add(4))?;
    let cd_disk = u16_at(&tail, pos.saturating_add(6))?;
    let entries = u16_at(&tail, pos.saturating_add(10))?;
    let size = u32_at(&tail, pos.saturating_add(12))?;
    let offset = u32_at(&tail, pos.saturating_add(16))?;
    let eocd_abs = len.saturating_sub(window).saturating_add(pos as u64);
    let cd = if disk == 0xFFFF
        || cd_disk == 0xFFFF
        || entries == 0xFFFF
        || size == 0xFFFF_FFFF
        || offset == 0xFFFF_FFFF
    {
        locate_zip64(file, eocd_abs)?
    } else {
        if disk != 0 || cd_disk != 0 {
            return Err(bad("multi-disk archives are not supported"));
        }
        CentralDirectory {
            entries: u64::from(entries),
            offset: u64::from(offset),
            size: u64::from(size),
        }
    };
    let end = cd
        .offset
        .checked_add(cd.size)
        .ok_or_else(|| bad("central directory span overflows"))?;
    if end > eocd_abs {
        return Err(bad("central directory overruns the end record"));
    }
    Ok(cd)
}

/// The ZIP64 locator sits immediately before the end record and names the ZIP64 end
/// record, which carries the 64-bit count/size/offset.
fn locate_zip64<R: Read + Seek>(file: &mut R, eocd_abs: u64) -> io::Result<CentralDirectory> {
    if eocd_abs < 20 {
        return Err(bad(
            "a ZIP64 field is saturated but there is no room for a locator",
        ));
    }
    let locator_at = eocd_abs.saturating_sub(20);
    file.seek(SeekFrom::Start(locator_at))?;
    let mut loc = [0u8; 20];
    file.read_exact(&mut loc)?;
    if u32_at(&loc, 0)? != SIG_EOCD64_LOCATOR {
        return Err(bad(
            "a ZIP64 field is saturated but no ZIP64 locator precedes the end record",
        ));
    }
    if u32_at(&loc, 4)? != 0 || u32_at(&loc, 16)? != 1 {
        return Err(bad("multi-disk archives are not supported"));
    }
    let record_at = u64_at(&loc, 8)?;
    if record_at.saturating_add(56) > locator_at {
        return Err(bad("ZIP64 end record overruns its locator"));
    }
    file.seek(SeekFrom::Start(record_at))?;
    let mut rec = [0u8; 56];
    file.read_exact(&mut rec)?;
    if u32_at(&rec, 0)? != SIG_EOCD64 {
        return Err(bad("ZIP64 end record has a bad signature"));
    }
    if u32_at(&rec, 16)? != 0 || u32_at(&rec, 20)? != 0 {
        return Err(bad("multi-disk archives are not supported"));
    }
    Ok(CentralDirectory {
        entries: u64_at(&rec, 32)?,
        offset: u64_at(&rec, 48)?,
        size: u64_at(&rec, 40)?,
    })
}

/// One central-directory record, with any ZIP64 extra field already folded in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The member name's raw bytes, as the record stores them.
    pub name: Vec<u8>,
    /// The general-purpose bit flags.
    pub flags: u16,
    /// The compression method (0 store, 8 deflate; [`Archive::body`] refuses others).
    pub method: u16,
    /// The body's size as stored.
    pub comp_size: u64,
    /// The body's size once decompressed, as declared.
    pub uncomp_size: u64,
    /// Where the member's local header starts.
    pub local_offset: u64,
    /// The "version made by" host byte (3 Unix, 19 Darwin).
    pub host: u8,
    /// The external file attributes.
    pub external: u32,
}

impl Entry {
    /// Whether the record flags traditional or strong encryption.
    #[must_use]
    pub const fn encrypted(&self) -> bool {
        self.flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION) != 0
    }

    /// The Unix mode in the external attributes' high half when a Unix or Darwin
    /// host wrote the record, else 0 (no type or permission bits supplied).
    #[must_use]
    pub const fn unix_mode(&self) -> u32 {
        if self.host == HOST_UNIX || self.host == HOST_DARWIN {
            self.external >> 16
        } else {
            0
        }
    }
}

/// Returns the record and the offset of the next one.
fn read_record<R: Read + Seek>(file: &mut R, at: u64, end: u64) -> io::Result<(Entry, u64)> {
    let fixed_end = at
        .checked_add(CENTRAL_FIXED)
        .ok_or_else(|| bad("central directory record overruns its span"))?;
    if fixed_end > end {
        return Err(bad("central directory record overruns its span"));
    }
    file.seek(SeekFrom::Start(at))?;
    let mut h = [0u8; 46];
    file.read_exact(&mut h)?;
    if u32_at(&h, 0)? != SIG_CENTRAL {
        return Err(bad("central directory record has a bad signature"));
    }
    let made_by = u16_at(&h, 4)?;
    let host = (made_by >> 8) as u8;
    let flags = u16_at(&h, 8)?;
    let method = u16_at(&h, 10)?;
    let comp = u32_at(&h, 20)?;
    let uncomp = u32_at(&h, 24)?;
    let name_len = u16_at(&h, 28)?;
    let extra_len = u16_at(&h, 30)?;
    let comment_len = u16_at(&h, 32)?;
    let disk_start = u16_at(&h, 34)?;
    let external = u32_at(&h, 38)?;
    let lho = u32_at(&h, 42)?;
    let var = u64::from(name_len)
        .saturating_add(u64::from(extra_len))
        .saturating_add(u64::from(comment_len));
    let next = fixed_end
        .checked_add(var)
        .ok_or_else(|| bad("central directory record overruns its span"))?;
    if next > end {
        return Err(bad("central directory record overruns its span"));
    }
    let mut name = vec![0u8; usize::from(name_len)];
    file.read_exact(&mut name)?;
    let mut extra = vec![0u8; usize::from(extra_len)];
    file.read_exact(&mut extra)?;
    let mut comp_size = u64::from(comp);
    let mut uncomp_size = u64::from(uncomp);
    let mut local_offset = u64::from(lho);
    let mut disk = u32::from(disk_start);
    if comp == 0xFFFF_FFFF || uncomp == 0xFFFF_FFFF || lho == 0xFFFF_FFFF || disk_start == 0xFFFF {
        let mut p: usize = 0;
        let mut found = false;
        while p.saturating_add(4) <= extra.len() {
            let id = u16_at(&extra, p)?;
            let sz = usize::from(u16_at(&extra, p.saturating_add(2))?);
            let body_start = p.saturating_add(4);
            let body_end = body_start.saturating_add(sz);
            let body = extra
                .get(body_start..body_end)
                .ok_or_else(|| bad("extra field overruns the record"))?;
            if id == ZIP64_EXTRA_ID {
                let mut q: usize = 0;
                if uncomp == 0xFFFF_FFFF {
                    uncomp_size = u64_at(body, q)?;
                    q = q.saturating_add(8);
                }
                if comp == 0xFFFF_FFFF {
                    comp_size = u64_at(body, q)?;
                    q = q.saturating_add(8);
                }
                if lho == 0xFFFF_FFFF {
                    local_offset = u64_at(body, q)?;
                    q = q.saturating_add(8);
                }
                if disk_start == 0xFFFF {
                    disk = u32_at(body, q)?;
                }
                found = true;
                break;
            }
            p = body_end;
        }
        if !found {
            return Err(bad(
                "a ZIP64 field is saturated but the record carries no ZIP64 extra field",
            ));
        }
    }
    if disk != 0 {
        return Err(bad("multi-disk archives are not supported"));
    }
    Ok((
        Entry {
            name,
            flags,
            method,
            comp_size,
            uncomp_size,
            local_offset,
            host,
            external,
        },
        next,
    ))
}

/// Where a record's body starts: past its local header, whose name/extra lengths are
/// the only thing read from it. The body must end before the central directory.
fn body_offset<R: Read + Seek>(file: &mut R, rec: &Entry, cd_offset: u64) -> io::Result<u64> {
    let hdr_end = rec
        .local_offset
        .checked_add(LOCAL_FIXED)
        .ok_or_else(|| bad("local header overruns the file"))?;
    if hdr_end > cd_offset {
        return Err(bad("local header overruns the central directory"));
    }
    file.seek(SeekFrom::Start(rec.local_offset))?;
    let mut h = [0u8; 30];
    file.read_exact(&mut h)?;
    if u32_at(&h, 0)? != SIG_LOCAL {
        return Err(bad("local header has a bad signature"));
    }
    let name_len = u64::from(u16_at(&h, 26)?);
    let extra_len = u64::from(u16_at(&h, 28)?);
    let data = hdr_end
        .checked_add(name_len)
        .and_then(|d| d.checked_add(extra_len))
        .ok_or_else(|| bad("local header overruns the file"))?;
    let data_end = data
        .checked_add(rec.comp_size)
        .ok_or_else(|| bad("member body overruns the file"))?;
    if data_end > cd_offset {
        return Err(bad("member body overruns the central directory"));
    }
    Ok(data)
}

/// A ZIP archive opened over `R`, with its central directory located and a cursor
/// over its records.
#[derive(Debug)]
pub struct Archive<R> {
    reader: R,
    cd: CentralDirectory,
    /// Where [`Archive::next_entry`] reads the next record.
    at: u64,
    /// How many records [`Archive::next_entry`] has read.
    seen: u64,
}

impl<R: Read + Seek> Archive<R> {
    /// Opens the archive `reader` holds: its length comes from seeking to its end, and
    /// the end-of-central-directory record (or the ZIP64 record it points at) must sit
    /// in the last 64 KiB plus 22 bytes, with nothing after its comment.
    ///
    /// # Errors
    ///
    /// The reader's own seek or read error, or `InvalidData` for an archive too short
    /// for an end record, with none, with trailing bytes, multi-disk, with a ZIP64
    /// locator or record missing or misplaced, or whose central directory overflows or
    /// overruns the end record.
    pub fn open(mut reader: R) -> io::Result<Self> {
        let len = reader.seek(SeekFrom::End(0))?;
        let cd = locate(&mut reader, len)?;
        Ok(Self {
            reader,
            at: cd.offset,
            seen: 0,
            cd,
        })
    }

    /// How many records the end record declares.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.cd.entries
    }

    /// Whether the end record declares no records.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.cd.entries == 0
    }

    /// The next central-directory record, or `None` once every declared record is read,
    /// which first checks that those records fill the directory's declared size
    /// exactly. A caller that counts to [`Archive::len`] itself calls this once more at
    /// the end to make that check.
    ///
    /// # Errors
    ///
    /// The reader's own error, or `InvalidData` for a record that overruns the
    /// directory, has a bad signature, a saturated field with no ZIP64 extra field, an
    /// extra field that overruns it or a nonzero start disk; or, at the end, a
    /// directory size that disagrees with its records.
    pub fn next_entry(&mut self) -> io::Result<Option<Entry>> {
        if self.seen >= self.cd.entries {
            if self.at != self.cd.end() {
                return Err(bad("central directory size disagrees with its records"));
            }
            return Ok(None);
        }
        self.seen = self.seen.saturating_add(1);
        let (entry, next) = read_record(&mut self.reader, self.at, self.cd.end())?;
        self.at = next;
        Ok(Some(entry))
    }

    /// Every record from the first, as an iterator that ends after the first error.
    /// It restarts the [`Archive::next_entry`] cursor.
    pub fn entries(&mut self) -> Entries<'_, R> {
        self.at = self.cd.offset;
        self.seen = 0;
        Entries {
            archive: self,
            done: false,
        }
    }

    /// The first record named exactly `name`, leaving the [`Archive::next_entry`]
    /// cursor where it was. It reads every record before the match, so a malformed
    /// earlier record refuses the search.
    ///
    /// # Errors
    ///
    /// As [`Archive::next_entry`].
    pub fn find(&mut self, name: &[u8]) -> io::Result<Option<Entry>> {
        let end = self.cd.end();
        let mut at = self.cd.offset;
        let mut seen: u64 = 0;
        while seen < self.cd.entries {
            seen = seen.saturating_add(1);
            let (entry, next) = read_record(&mut self.reader, at, end)?;
            if entry.name == name {
                return Ok(Some(entry));
            }
            at = next;
        }
        if at != end {
            return Err(bad("central directory size disagrees with its records"));
        }
        Ok(None)
    }

    /// The body of `entry` as a reader of its uncompressed bytes, bounded on the
    /// compressed side by `Take` (a body never reads past its own span); the caller
    /// bounds the uncompressed side and compares the count with
    /// [`Entry::uncomp_size`].
    ///
    /// # Errors
    ///
    /// The reader's own error, or `InvalidData` for an encrypted member, a method other
    /// than store or deflate, a stored member whose two sizes disagree, or a local
    /// header or body that overruns the central directory or has a bad signature.
    /// Reading the body yields a deflate stream's own errors.
    pub fn body(&mut self, entry: &Entry) -> io::Result<Box<dyn Read + '_>> {
        if entry.encrypted() {
            return Err(bad("encrypted members are not supported"));
        }
        match entry.method {
            METHOD_STORE => {
                if entry.comp_size != entry.uncomp_size {
                    return Err(bad(
                        "stored member's compressed and uncompressed sizes disagree",
                    ));
                }
            }
            METHOD_DEFLATE => {}
            _ => {
                return Err(bad(
                    "unsupported compression method (only store and deflate)",
                ));
            }
        }
        let data = body_offset(&mut self.reader, entry, self.cd.offset)?;
        self.reader.seek(SeekFrom::Start(data))?;
        let raw = (&mut self.reader).take(entry.comp_size);
        if entry.method == METHOD_STORE {
            Ok(Box::new(raw))
        } else {
            Ok(Box::new(crate::inflate::stream::DeflateReader::new(raw)))
        }
    }

    /// The whole uncompressed body of `entry`.
    ///
    /// # Errors
    ///
    /// As [`Archive::body`], or `InvalidData` for a member that declares or yields more
    /// than `max` bytes, or whose body disagrees with its declared size.
    pub fn read(&mut self, entry: &Entry, max: u64) -> io::Result<Vec<u8>> {
        if entry.uncomp_size > max {
            return Err(bad("member body exceeds the caller's cap"));
        }
        let reserve = usize::try_from(entry.uncomp_size.min(READ_RESERVE_CAP)).unwrap_or(0);
        let mut out = Vec::with_capacity(reserve);
        self.body(entry)?
            .take(max.saturating_add(1))
            .read_to_end(&mut out)?;
        let got = out.len() as u64;
        if got > max {
            return Err(bad("member body exceeds the caller's cap"));
        }
        if got != entry.uncomp_size {
            return Err(bad("member body disagrees with its declared size"));
        }
        Ok(out)
    }
}

/// The records of an [`Archive`], from [`Archive::entries`].
#[derive(Debug)]
pub struct Entries<'a, R> {
    archive: &'a mut Archive<R>,
    done: bool,
}

impl<R: Read + Seek> Iterator for Entries<'_, R> {
    type Item = io::Result<Entry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let got = self.archive.next_entry().transpose();
        if !matches!(got, Some(Ok(_))) {
            self.done = true;
        }
        got
    }
}
