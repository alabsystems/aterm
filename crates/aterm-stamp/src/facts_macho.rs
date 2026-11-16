// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Named, file-backed compile-facts sections in supported macOS executable slices.
//! Bounds and section checks follow the release identity writer's `postlink` reader.

use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

use super::{LEN, SECTION, decode};

const MAX_COMMANDS: u64 = 1024 * 1024;
const MAX_SLICES: usize = 16;

fn word(bytes: &[u8], at: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or("truncated Mach-O word")?
            .try_into()
            .map_err(|_| "Mach-O word")?,
    ))
}

fn wide(bytes: &[u8], at: usize) -> Result<u64, String> {
    Ok(u64::from_le_bytes(
        bytes
            .get(at..at + 8)
            .ok_or("truncated Mach-O wide word")?
            .try_into()
            .map_err(|_| "Mach-O wide word")?,
    ))
}

fn range(offset: u64, size: u64, limit: u64) -> Result<Range<u64>, String> {
    let end = offset.checked_add(size).ok_or("Mach-O range overflow")?;
    if end > limit {
        return Err("Mach-O range outside file".into());
    }
    Ok(offset..end)
}

fn named(bytes: &[u8], name: &[u8]) -> bool {
    bytes.get(..name.len()) == Some(name)
        && bytes
            .get(name.len()..)
            .is_some_and(|tail| tail.iter().all(|b| *b == 0))
}

fn read_at(reader: &mut (impl Read + Seek), at: u64, out: &mut [u8]) -> Result<(), String> {
    reader
        .seek(SeekFrom::Start(at))
        .and_then(|_| reader.read_exact(out))
        .map_err(|e| format!("cannot read Mach-O compile facts: {e}"))
}

/// Read the one compile-facts section of every supported executable architecture, without
/// running it. Thin 64-bit macOS images and big-endian 32/64-bit universal tables are supported;
/// all other formats refuse. Load commands are bounded to 1 MiB and slices to sixteen.
/// # Errors
/// The image cannot be read, its object structure or section is malformed/unsupported, a
/// compile-facts section is absent/duplicated, or its facts contradict the architecture.
pub fn macho_records(reader: &mut (impl Read + Seek)) -> Result<Vec<[u8; LEN]>, String> {
    let len = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    let mut header = [0; 8];
    read_at(reader, 0, &mut header)?;
    let magic = u32::from_be_bytes(header[..4].try_into().map_err(|_| "Mach-O header")?);
    if !matches!(magic, 0xcafe_babe | 0xcafe_babf) {
        return thin(reader, 0..len, None).map(|record| vec![record]);
    }
    let count = u32::from_be_bytes(header[4..].try_into().map_err(|_| "fat count")?) as usize;
    if count == 0 || count > MAX_SLICES {
        return Err("invalid universal architecture count".into());
    }
    let stride = if magic == 0xcafe_babf { 32 } else { 20 };
    let table_end = 8 + count * stride;
    range(0, table_end as u64, len)?;
    let mut table = vec![0; count * stride];
    read_at(reader, 8, &mut table)?;
    let be32 =
        |bytes: &[u8]| u32::from_be_bytes(bytes.try_into().expect("a bounded four-byte field"));
    let be64 =
        |bytes: &[u8]| u64::from_be_bytes(bytes.try_into().expect("a bounded eight-byte field"));
    let mut slices: Vec<((u32, u32), Range<u64>)> = Vec::new();
    for row in table.chunks_exact(stride) {
        let cpu = be32(&row[..4]);
        let subtype = be32(&row[4..8]);
        let (offset, size) = if stride == 32 {
            (be64(&row[8..16]), be64(&row[16..24]))
        } else {
            (u64::from(be32(&row[8..12])), u64::from(be32(&row[12..16])))
        };
        let slice = range(offset, size, len)?;
        let align = if stride == 32 {
            be32(&row[24..28])
        } else {
            be32(&row[16..20])
        };
        if slice.start < table_end as u64
            || slice.is_empty()
            || align > 63
            || offset % (1_u64 << align.min(63)) != 0
            || stride == 32 && be32(&row[28..32]) != 0
            || slices
                .iter()
                .any(|((other, _), r)| *other == cpu || slice.start < r.end && r.start < slice.end)
        {
            return Err("duplicate, overlapping or invalid universal slice".into());
        }
        slices.push(((cpu, subtype), slice));
    }
    slices
        .into_iter()
        .map(|(cpu, slice)| thin(reader, slice, Some(cpu)))
        .collect()
}

fn thin(
    reader: &mut (impl Read + Seek),
    slice: Range<u64>,
    expected_cpu: Option<(u32, u32)>,
) -> Result<[u8; LEN], String> {
    range(0, 32, slice.end - slice.start)?;
    let mut header = [0; 32];
    read_at(reader, slice.start, &mut header)?;
    let cpu = word(&header, 4)?;
    let arch = match cpu {
        0x0100_000c => "aarch64",
        0x0100_0007 => "x86_64",
        _ => return Err("unsupported Mach-O architecture".into()),
    };
    if word(&header, 0)? != 0xfeed_facf
        || word(&header, 12)? != 2
        || expected_cpu.is_some_and(|c| c != (cpu, word(&header, 8).unwrap_or(u32::MAX)))
    {
        return Err("compile facts require a matching 64-bit macOS executable slice".into());
    }
    let size = u64::from(word(&header, 20)?);
    if size > MAX_COMMANDS {
        return Err("Mach-O load commands exceed compile-facts bound".into());
    }
    let commands = range(32, size, slice.end - slice.start)?;
    let count = word(&header, 16)? as usize;
    if count > size as usize / 8 {
        return Err("Mach-O command count exceeds table".into());
    }
    let mut bytes = vec![0; size as usize];
    read_at(reader, slice.start + 32, &mut bytes)?;
    let mut at = 0;
    let mut found = None;
    for _ in 0..count {
        let kind = word(&bytes, at)?;
        let size = word(&bytes, at + 4)? as usize;
        if size < 8 || size > bytes.len().saturating_sub(at) {
            return Err("invalid Mach-O command size".into());
        }
        if kind == 0x19 {
            if size < 72 {
                return Err("short Mach-O segment".into());
            }
            let segment = range(
                wide(&bytes, at + 40)?,
                wide(&bytes, at + 48)?,
                slice.end - slice.start,
            )?;
            let sections = word(&bytes, at + 64)? as usize;
            if sections > (size - 72) / 80 {
                return Err("Mach-O sections exceed segment command".into());
            }
            for i in 0..sections {
                let section = at + 72 + i * 80;
                if !named(&bytes[section..section + 16], SECTION) {
                    continue;
                }
                if !named(&bytes[at + 8..at + 24], b"__TEXT")
                    || !named(&bytes[section + 16..section + 32], b"__TEXT")
                    || wide(&bytes, section + 40)? != LEN as u64
                    || word(&bytes, section + 64)? & 0xff != 0
                {
                    return Err("compile facts must be fixed-size __TEXT S_REGULAR".into());
                }
                let protection = word(&bytes, at + 60)?;
                if protection & 2 != 0 || protection & 1 == 0 {
                    return Err("compile-facts segment must be readable and not writable".into());
                }
                let record = range(
                    u64::from(word(&bytes, section + 48)?),
                    LEN as u64,
                    slice.end - slice.start,
                )?;
                if record.start < commands.end
                    || record.start < segment.start
                    || record.end > segment.end
                    || found.replace(record).is_some()
                {
                    return Err("duplicate or non-file-backed compile-facts section".into());
                }
            }
        } else if kind == 1 {
            return Err("32-bit segment in 64-bit image".into());
        }
        at += size;
    }
    if at != bytes.len() {
        return Err("unclaimed Mach-O load commands".into());
    }
    let found = found.ok_or("missing __TEXT,__aterm_facts")?;
    let mut record = [0; LEN];
    read_at(reader, slice.start + found.start, &mut record)?;
    if decode(&record)?.target_arch != arch {
        return Err("compile facts contradict their executable architecture".into());
    }
    Ok(record)
}
