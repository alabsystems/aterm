// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bootstrap and signing tools must run on the oldest supported macOS.

const MACOS_11: u32 = 11 << 16;

/// Hold an object to the shared macOS deployment floor.
///
/// # Errors
/// A newer minimum macOS version, malformed load commands or a fat object that
/// must first be split into the package's target architecture.
pub(super) fn check(bytes: &[u8], program: &str) -> Result<(), String> {
    if let Some(version) = minimum(bytes)?
        && version > MACOS_11
    {
        return Err(format!(
            "{program} requires macOS {}.{}.{}; build it for macOS 11.0 or earlier so it can run on every supported Mac",
            version >> 16,
            (version >> 8) & 255,
            version & 255,
        ));
    }
    Ok(())
}

fn minimum(bytes: &[u8]) -> Result<Option<u32>, String> {
    let word = |at: usize| {
        bytes
            .get(at..at + 4)
            .and_then(|part| <[u8; 4]>::try_from(part).ok())
            .map(u32::from_le_bytes)
            .ok_or_else(|| "truncated Mach-O header".to_string())
    };
    let header = match bytes.get(..4) {
        Some([0xcf, 0xfa, 0xed, 0xfe]) => 32,
        Some([0xce, 0xfa, 0xed, 0xfe]) => 28,
        Some([0xca, 0xfe, 0xba, 0xbe | 0xbf]) => {
            return Err("pack one thin Mach-O architecture per target".into());
        }
        _ => return Ok(None),
    };
    let count = word(16)?;
    let end = header + word(20)? as usize;
    if end > bytes.len() {
        return Err("truncated Mach-O load commands".into());
    }
    let mut at = header;
    let mut minimum = None;
    for _ in 0..count {
        let command = word(at)?;
        let size = word(at + 4)? as usize;
        if size < 8 || size > end.saturating_sub(at) {
            return Err("invalid Mach-O load command size".into());
        }
        let version = match command {
            0x32 if size >= 24 && word(at + 8)? == 1 => Some(word(at + 12)?),
            0x32 => return Err("invalid macOS build-version command".into()),
            0x24 if size >= 16 => Some(word(at + 8)?),
            0x24 => return Err("truncated macOS minimum-version command".into()),
            _ => None,
        };
        if let Some(version) = version {
            minimum = Some(minimum.map_or(version, |old: u32| old.max(version)));
        }
        at += size;
    }
    if at != end {
        return Err("Mach-O load-command count does not match its size".into());
    }
    Ok(minimum)
}

#[cfg(test)]
#[path = "macos_tests.rs"]
mod tests;
