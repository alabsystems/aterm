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
mod tests {
    use super::*;

    fn object(command: u32, version: u32) -> Vec<u8> {
        let mut bytes = vec![0; 56];
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        bytes[16..20].copy_from_slice(&1_u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&24_u32.to_le_bytes());
        bytes[32..36].copy_from_slice(&command.to_le_bytes());
        bytes[36..40].copy_from_slice(&24_u32.to_le_bytes());
        if command == 0x32 {
            bytes[40..44].copy_from_slice(&1_u32.to_le_bytes());
            bytes[44..48].copy_from_slice(&version.to_le_bytes());
        } else {
            bytes[40..44].copy_from_slice(&version.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn bootstrap_accepts_macos_11_and_rejects_newer_bottle_deployment_targets() {
        for command in [0x32, 0x24] {
            assert!(check(&object(command, MACOS_11), "git").is_ok());
            assert!(check(&object(command, (10 << 16) | (15 << 8)), "git").is_ok());
            assert!(
                check(&object(command, 14 << 16), "git")
                    .unwrap_err()
                    .contains("14.0.0")
            );
            assert!(check(&object(command, 15 << 16), "openssl-signing").is_err());
        }
        let mut truncated = object(0x32, MACOS_11);
        truncated.truncate(48);
        assert!(check(&truncated, "git").is_err());
        let mut wrong_platform = object(0x32, MACOS_11);
        wrong_platform[40..44].copy_from_slice(&2_u32.to_le_bytes());
        assert!(check(&wrong_platform, "git").is_err());
    }
}
