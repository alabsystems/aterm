// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Identity is patched in a file-backed Mach-O section, then covered by a fresh signature.

use std::ops::Range;
use std::path::Path;

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    let word = bytes.get(at..at + 4).ok_or("truncated Mach-O word")?;
    Ok(u32::from_le_bytes(
        word.try_into().map_err(|_| "Mach-O word")?,
    ))
}

fn u64_at(bytes: &[u8], at: usize) -> Result<u64, String> {
    let word = bytes.get(at..at + 8).ok_or("truncated Mach-O word")?;
    Ok(u64::from_le_bytes(
        word.try_into().map_err(|_| "Mach-O word")?,
    ))
}

fn range(offset: u64, size: u64, limit: usize) -> Result<Range<usize>, String> {
    let end = offset.checked_add(size).ok_or("Mach-O range overflow")?;
    if end > limit as u64 {
        return Err("Mach-O range outside file".into());
    }
    Ok(offset as usize..end as usize)
}

fn named(bytes: &[u8], name: &[u8]) -> bool {
    bytes.get(..name.len()) == Some(name)
        && bytes
            .get(name.len()..)
            .is_some_and(|tail| tail.iter().all(|b| *b == 0))
}

struct Layout {
    stamp: Range<usize>,
    signature: Range<usize>,
    signature_command: Range<usize>,
    linkedit_sizes: [Range<usize>; 2],
}

fn layout(bytes: &[u8]) -> Result<Layout, String> {
    if u32_at(bytes, 0)? != 0xfeed_facf
        || !matches!(u32_at(bytes, 4)?, 0x0100_0007 | 0x0100_000c)
        || u32_at(bytes, 12)? != 2
    {
        return Err("stamp requires a thin 64-bit macOS executable".into());
    }
    let commands = range(32, u64::from(u32_at(bytes, 20)?), bytes.len())?;
    let count = u32_at(bytes, 16)?;
    if count as usize > commands.len() / 8 {
        return Err("Mach-O command count exceeds table".into());
    }
    let mut at = commands.start;
    let mut stamp = None;
    let mut signature = None;
    let mut linkedit_sizes = None;
    for _ in 0..count {
        if at + 8 > commands.end {
            return Err("truncated Mach-O command".into());
        }
        let kind = u32_at(bytes, at)?;
        let size = u32_at(bytes, at + 4)? as usize;
        if size < 8 || size > commands.end - at {
            return Err("invalid Mach-O command size".into());
        }
        if kind == 0x19 {
            if size < 72 {
                return Err("short Mach-O segment".into());
            }
            let segment = range(
                u64_at(bytes, at + 40)?,
                u64_at(bytes, at + 48)?,
                bytes.len(),
            )?;
            let sections = u32_at(bytes, at + 64)? as usize;
            if sections > (size - 72) / 80 {
                return Err("Mach-O sections exceed segment command".into());
            }
            if named(&bytes[at + 8..at + 24], b"__LINKEDIT")
                && linkedit_sizes
                    .replace([at + 32..at + 40, at + 48..at + 56])
                    .is_some()
            {
                return Err("duplicate __LINKEDIT".into());
            }
            for i in 0..sections {
                let section = at + 72 + i * 80;
                if !named(&bytes[section..section + 16], b"__aterm_stamp") {
                    continue;
                }
                if !named(&bytes[at + 8..at + 24], b"__DATA")
                    || !named(&bytes[section + 16..section + 32], b"__DATA")
                    || u64_at(bytes, section + 40)? != aterm_stamp::LEN as u64
                    || u32_at(bytes, section + 64)? & 0xff != 0
                {
                    return Err("stamp must be fixed-size __DATA S_REGULAR".into());
                }
                let record = range(
                    u64::from(u32_at(bytes, section + 48)?),
                    aterm_stamp::LEN as u64,
                    bytes.len(),
                )?;
                if record.start < commands.end
                    || record.start < segment.start
                    || record.end > segment.end
                {
                    return Err("stamp is outside file-backed segment data".into());
                }
                if stamp.replace(record).is_some() {
                    return Err("duplicate stamp section".into());
                }
            }
        } else if kind == 0x1d {
            if size != 16 {
                return Err("invalid code signature command".into());
            }
            let blob = range(
                u64::from(u32_at(bytes, at + 8)?),
                u64::from(u32_at(bytes, at + 12)?),
                bytes.len(),
            )?;
            if blob.is_empty() || blob.start < commands.end || blob.end != bytes.len() {
                return Err("signature must be a nonempty trailing blob".into());
            }
            if signature.replace((blob, at + 8..at + 16)).is_some() {
                return Err("duplicate code signature".into());
            }
        } else if kind == 1 {
            return Err("32-bit segment in 64-bit image".into());
        }
        at += size;
    }
    if at != commands.end {
        return Err("unclaimed Mach-O load commands".into());
    }
    let stamp = stamp.ok_or("missing __DATA,__aterm_stamp")?;
    let (signature, signature_command) = signature.ok_or("missing code signature")?;
    if stamp.end > signature.start {
        return Err("stamp overlaps code signature".into());
    }
    Ok(Layout {
        stamp,
        signature,
        signature_command,
        linkedit_sizes: linkedit_sizes.ok_or("missing __LINKEDIT")?,
    })
}

/// Normalize identity and signature storage, retaining every other product byte.
pub fn normalized(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbe]) {
        let be = |at: usize| -> Result<u32, String> {
            Ok(u32::from_be_bytes(
                bytes
                    .get(at..at + 4)
                    .ok_or("truncated fat header")?
                    .try_into()
                    .map_err(|_| "fat word")?,
            ))
        };
        let count = be(4)? as usize;
        if count == 0 || count > bytes.len().saturating_sub(8) / 20 {
            return Err("invalid fat architecture table".into());
        }
        let mut result = b"aterm-fat-content-v1\0".to_vec();
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..count {
            let at = 8 + 20 * i;
            let cpu = be(at)?;
            if !seen.insert(cpu) {
                return Err("duplicate fat architecture".into());
            }
            let slice = range(u64::from(be(at + 8)?), u64::from(be(at + 12)?), bytes.len())?;
            if slice.start < 8 + 20 * count
                || u32_at(&bytes[slice.clone()], 0)? != 0xfeed_facf
                || u32_at(&bytes[slice.clone()], 4)? != cpu
            {
                return Err("fat slice disagrees with architecture table".into());
            }
            let normalized = normalized(&bytes[slice])?;
            result.extend_from_slice(&cpu.to_le_bytes());
            result.extend_from_slice(&be(at + 4)?.to_le_bytes());
            result.extend_from_slice(&(normalized.len() as u64).to_le_bytes());
            result.extend(normalized);
        }
        return Ok(result);
    }
    let layout = layout(bytes)?;
    let mut bytes = bytes.to_vec();
    bytes[layout.stamp].fill(0);
    bytes[layout.signature_command].fill(0);
    for size in layout.linkedit_sizes {
        bytes[size].fill(0);
    }
    bytes.truncate(layout.signature.start);
    Ok(bytes)
}

/// Patch exactly the dedicated section. Never search for a stamp-shaped string elsewhere.
pub fn stamp(path: &Path, record: &[u8; aterm_stamp::LEN]) -> Result<(), String> {
    let mut bytes = crate::durable::read_regular_bytes(path, u64::MAX)
        .map_err(|e| format!("read stamp slice {}: {e}", path.display()))?;
    let layout = layout(&bytes)?;
    if !bytes[layout.stamp.clone()].starts_with(aterm_stamp::HEADER.as_bytes()) {
        return Err("stamp section has no identity record".into());
    }
    bytes[layout.stamp].copy_from_slice(record);
    std::fs::write(path, bytes).map_err(|e| format!("stamp {}: {e}", path.display()))
}

/// Every architecture must carry exactly the reservation's fields, in the stamp format's
/// order. U alone deliberately says nothing about identity.
pub fn prove_stamp(bytes: &[u8], expected: &[(&str, &str)]) -> Result<(), String> {
    if bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbe]) {
        let be = |at: usize| -> Result<u32, String> {
            Ok(u32::from_be_bytes(
                bytes
                    .get(at..at + 4)
                    .ok_or("truncated fat header")?
                    .try_into()
                    .map_err(|_| "fat word")?,
            ))
        };
        let count = be(4)? as usize;
        if count == 0 || count > bytes.len().saturating_sub(8) / 20 {
            return Err("invalid fat table".into());
        }
        for i in 0..count {
            let at = 8 + 20 * i;
            let slice = range(u64::from(be(at + 8)?), u64::from(be(at + 12)?), bytes.len())?;
            if slice.start < 8 + 20 * count
                || u32_at(&bytes[slice.clone()], 0)? != 0xfeed_facf
                || u32_at(&bytes[slice.clone()], 4)? != be(at)?
            {
                return Err("invalid fat architecture slice".into());
            }
            prove_stamp(&bytes[slice], expected)?;
        }
        return Ok(());
    }
    let record = &bytes[layout(bytes)?.stamp];
    let end = record.iter().position(|b| *b == 0).unwrap_or(record.len());
    if record[end..].iter().any(|b| *b != 0) {
        return Err("nonzero stamp padding".into());
    }
    let text = std::str::from_utf8(&record[..end]).map_err(|e| e.to_string())?;
    let fields: Option<Vec<_>> = text
        .strip_prefix(aterm_stamp::HEADER)
        .ok_or("invalid stamp header")?
        .lines()
        .map(|line| line.split_once('='))
        .collect();
    if fields.as_deref() != Some(expected) {
        return Err("stamp differs from reservation identity".into());
    }
    Ok(())
}

/// Read the frozen index hint from a retained native pilot image without executing it.
pub(crate) fn index_hint(bytes: &[u8]) -> Result<String, String> {
    let record = &bytes[layout(bytes)?.stamp];
    let text = std::str::from_utf8(record).map_err(|e| e.to_string())?;
    text.strip_prefix(aterm_stamp::HEADER)
        .ok_or("invalid stamp header")?
        .lines()
        .find_map(|line| line.strip_prefix("index_hint="))
        .map(str::to_owned)
        .ok_or_else(|| "retained image has no index hint".to_owned())
}

/// Immutable linked slices are addressed by U before any reservation is stamped into them.
/// These files are never executed; the build stamps a private copy.
pub fn keep_unstamped(path: &Path, store: &Path) -> Result<std::path::PathBuf, String> {
    crate::snapshot::private_dir(store, "slice artifact store")?;
    let bytes = crate::durable::read_regular_bytes(path, u64::MAX).map_err(|e| e.to_string())?;
    if bytes[layout(&bytes)?.stamp] != aterm_stamp::UNSTAMPED_RECORD {
        return Err("linked slice did not compile the constant postlink placeholder".into());
    }
    let content = normalized(&bytes)?;
    let mut h = aterm_digest::Sha256::new();
    h.update(&content);
    let key = aterm_codec::hex::encode_lower(&h.finalize());
    let dir = store.join("unstamped");
    crate::snapshot::private_dir(&dir, "unstamped slice store")?;
    let kept = dir.join(key);
    match crate::durable::read_regular_bytes(&kept, u64::MAX) {
        Ok(bytes) => {
            verify(&kept)?;
            if bytes[layout(&bytes)?.stamp] != aterm_stamp::UNSTAMPED_RECORD
                || normalized(&bytes)? != content
            {
                return Err("unstamped content store entry changed".into());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            crate::durable::copy_regular_fresh(path, &kept).map_err(|e| e.to_string())?;
        }
        Err(e) => return Err(format!("read unstamped slice: {e}")),
    }
    Ok(kept)
}

/// Only the current two slices need raw storage: cuts retain their own completed bundles.
pub fn prune_unstamped(store: &Path, keep: &[std::path::PathBuf]) -> Result<(), String> {
    for entry in std::fs::read_dir(store.join("unstamped")).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !keep.contains(&path)
            && entry
                .file_name()
                .to_str()
                .is_some_and(|n| aterm_codec::hex::is_lower(n, 64))
        {
            std::fs::remove_file(&path).map_err(|e| format!("prune unstamped slice: {e}"))?;
        }
    }
    Ok(())
}

/// Ad hoc signing is required after changing even one byte of an arm64 code page.
pub fn resign(path: &Path) -> Result<(), String> {
    for args in [vec!["--force", "--sign", "-"], vec!["--verify", "--strict"]] {
        let output = aterm_uds::spawnfd::command(aterm_uds::os_path!("codesign"))
            .args(args)
            .arg(path)
            .output()
            .map_err(|e| format!("codesign: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "codesign {}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    Ok(())
}

/// A normalized digest never makes an invalid or missing signature usable evidence.
pub fn verify(path: &Path) -> Result<(), String> {
    let output = aterm_uds::spawnfd::command(aterm_uds::os_path!("codesign"))
        // The pre-cut bundle has no resource envelope yet. U hashes every resource itself;
        // here codesign judges the executable's pages, not a later bundle resource seal.
        .args([
            "--verify",
            "--strict",
            "--all-architectures",
            "--ignore-resources",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("verify product signature: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "invalid product signature: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// The running image must report the newly stamped identity even when its measure is reused.
pub fn identity_report(stamp: &aterm_stamp::Stamp, report: &str) -> Result<(), String> {
    let fields: Vec<_> = report
        .lines()
        .filter_map(|line| line.strip_prefix("version:"))
        .map(str::trim)
        .collect();
    let expected = format!(
        "{} ({}, built {})",
        stamp.app_version, stamp.git_commit, stamp.build_time
    );
    if fields.as_slice() == [expected.as_str()] {
        Ok(())
    } else {
        Err(format!(
            "running image identity {fields:?}, expected {expected:?}"
        ))
    }
}

#[cfg(test)]
#[path = "postlink_identity_tests.rs"]
mod identity_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "postlink_tests.rs"]
mod tests;
