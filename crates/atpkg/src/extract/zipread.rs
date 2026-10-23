// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only ZIP central-directory parser and extractor for the `zip` vendor payload lane.

use std::cell::Cell;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::{
    EntryKind, ExtractError, ExtractOptions, ExtractReject, Layer, TAR_ENTRY_STRUCTURAL_BUDGET,
    TooLargeReason, TreeAccumulator, folded,
};

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
const S_IFMT: u32 = 0o170_000;
const S_IFLNK: u32 = 0o120_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
/// The end record is 22 bytes plus a comment of at most 65535 bytes.
const EOCD_LEN: u64 = 22;
const EOCD_SEARCH: u64 = EOCD_LEN + 65_535;
const CENTRAL_FIXED: u64 = 46;
const LOCAL_FIXED: u64 = 30;
/// A symlink target longer than `PATH_MAX` names no valid link.
const MAX_LINK_TARGET: u64 = 4096;

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
struct CentralDirectory {
    entries: u64,
    offset: u64,
    size: u64,
}

/// central directory's span from it (or from the ZIP64 record it points at).
fn locate(file: &mut File, len: u64) -> io::Result<CentralDirectory> {
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

/// record, which carries the 64-bit count/size/offset.
fn locate_zip64(file: &mut File, eocd_abs: u64) -> io::Result<CentralDirectory> {
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
struct Record {
    name: Vec<u8>,
    flags: u16,
    method: u16,
    comp_size: u64,
    uncomp_size: u64,
    local_offset: u64,
    host: u8,
    external: u32,
}

/// Returns the record and the offset of the next one.
fn read_record(file: &mut File, at: u64, end: u64) -> io::Result<(Record, u64)> {
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
        Record {
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

/// the only thing read from it. The body must end before the central directory.
fn body_offset(file: &mut File, rec: &Record, cd_offset: u64) -> io::Result<u64> {
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

/// type bits (when the host supplied them).
fn classify(name: &[u8], unix_mode: u32) -> EntryKind {
    if name.last() == Some(&b'/') {
        return EntryKind::Directory;
    }
    match unix_mode & S_IFMT {
        0 | S_IFREG => EntryKind::Regular,
        S_IFDIR => EntryKind::Directory,
        S_IFLNK => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

/// The member name as a path — raw bytes on Unix, UTF-8 or refused elsewhere.
fn name_to_path(bytes: &[u8]) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| bad("member name is not UTF-8"))
    }
}

/// caller's cap.
fn open_body<'f>(
    file: &'f mut File,
    rec: &Record,
    cd_offset: u64,
) -> io::Result<Box<dyn Read + 'f>> {
    match rec.method {
        METHOD_STORE => {
            if rec.comp_size != rec.uncomp_size {
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
    let data = body_offset(file, rec, cd_offset)?;
    file.seek(SeekFrom::Start(data))?;
    let raw = file.take(rec.comp_size);
    if rec.method == METHOD_STORE {
        Ok(Box::new(raw))
    } else {
        Ok(Box::new(aterm_codec::inflate::stream::DeflateReader::new(
            raw,
        )))
    }
}

/// Extracts the ZIP archive at `archive` into `dest_root` through a [`Layer`].
pub(super) fn extract(
    archive: &Path,
    dest_root: &Path,
    max_total_bytes: u64,
    max_entries: u64,
    opts: ExtractOptions,
) -> Result<TreeAccumulator, ExtractError> {
    let mut file = File::open(archive)?;
    let len = file.metadata()?.len();
    let cd = locate(&mut file, len)?;
    let budget = Rc::new(Cell::new(TAR_ENTRY_STRUCTURAL_BUDGET));
    let mut layer = Layer::open(dest_root, max_total_bytes, max_entries, true, opts, budget)?;
    let end = cd.offset.saturating_add(cd.size);
    let mut at = cd.offset;
    let mut seen: u64 = 0;
    while seen < cd.entries {
        seen = seen.saturating_add(1);
        layer.next_entry()?;
        let (rec, next) = read_record(&mut file, at, end)?;
        at = next;
        if rec.flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION) != 0 {
            return Err(bad("encrypted members are not supported").into());
        }
        if rec.name.contains(&0) || rec.name.contains(&b'\\') {
            return Err(bad("member name contains NUL or a backslash").into());
        }
        let raw = name_to_path(&rec.name)?;
        let unix_mode = if rec.host == HOST_UNIX || rec.host == HOST_DARWIN {
            rec.external >> 16
        } else {
            0
        };
        match classify(&rec.name, unix_mode) {
            EntryKind::Directory => {
                layer.directory(&raw, rec.uncomp_size, unix_mode & 0o7777)?;
            }
            EntryKind::Regular => {
                let body = open_body(&mut file, &rec, cd.offset)?;
                let consumed = layer.regular(&raw, unix_mode & 0o7777, body)?;
                if consumed != rec.uncomp_size {
                    return Err(bad("member body disagrees with its declared size").into());
                }
            }
            EntryKind::Symlink => {
                if opts.symlinks == super::SymlinkPolicy::Refuse {
                    return Err(ExtractError::Rejected(ExtractReject::DisallowedKind, raw));
                }
                if rec.uncomp_size > MAX_LINK_TARGET {
                    return Err(ExtractError::TooLarge(TooLargeReason::LinkTargetTooLong(
                        raw.clone(),
                    )));
                }
                let body = open_body(&mut file, &rec, cd.offset)?;
                let mut target = Vec::new();
                body.take(MAX_LINK_TARGET.saturating_add(1))
                    .read_to_end(&mut target)?;
                if target.len() as u64 != rec.uncomp_size {
                    return Err(bad("symlink target disagrees with its declared size").into());
                }
                let target = name_to_path(&target)?;
                layer.symlink(&raw, &target, 0)?;
            }
            EntryKind::Hardlink | EntryKind::Other => {
                return Err(ExtractError::Rejected(ExtractReject::DisallowedKind, raw));
            }
        }
    }
    if at != end {
        return Err(bad("central directory size disagrees with its records").into());
    }
    folded(layer.finish())
}
