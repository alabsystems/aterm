// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Git must bootstrap on the oldest macOS supported by aterm.

const MACOS_11: u32 = 11 << 16;

/// Hold a Git object to the bootstrap deployment floor.
///
/// # Errors
/// A newer minimum macOS version, malformed load commands or a fat object that
/// must first be split into the package's target architecture.
pub(super) fn check(bytes: &[u8]) -> Result<(), String> {
    if let Some(version) = minimum(bytes)?
        && version > MACOS_11
    {
        return Err(format!(
            "Git requires macOS {}.{}.{}; build it for macOS 11.0 or earlier so it can bootstrap every supported Mac",
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
            .ok_or_else(|| "truncated Git Mach-O header".to_string())
    };
    let header = match bytes.get(..4) {
        Some([0xcf, 0xfa, 0xed, 0xfe]) => 32,
        Some([0xce, 0xfa, 0xed, 0xfe]) => 28,
        Some([0xca, 0xfe, 0xba, 0xbe | 0xbf]) => {
            return Err("pack Git as one thin Mach-O architecture per target".into());
        }
        _ => return Ok(None),
    };
    let count = word(16)?;
    let end = header + word(20)? as usize;
    if end > bytes.len() {
        return Err("truncated Git Mach-O load commands".into());
    }
    let mut at = header;
    let mut minimum = None;
    for _ in 0..count {
        let command = word(at)?;
        let size = word(at + 4)? as usize;
        if size < 8 || size > end.saturating_sub(at) {
            return Err("invalid Git Mach-O load command size".into());
        }
        let version = match command {
            0x32 if size >= 24 && word(at + 8)? == 1 => Some(word(at + 12)?),
            0x32 => return Err("Git has an invalid macOS build-version command".into()),
            0x24 if size >= 16 => Some(word(at + 8)?),
            0x24 => return Err("Git has a truncated macOS minimum-version command".into()),
            _ => None,
        };
        if let Some(version) = version {
            minimum = Some(minimum.map_or(version, |old: u32| old.max(version)));
        }
        at += size;
    }
    if at != end {
        return Err("Git Mach-O load-command count does not match its size".into());
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
            assert!(check(&object(command, MACOS_11)).is_ok());
            assert!(check(&object(command, (10 << 16) | (15 << 8))).is_ok());
            assert!(
                check(&object(command, 14 << 16))
                    .unwrap_err()
                    .contains("14.0.0")
            );
            assert!(check(&object(command, 15 << 16)).is_err());
        }
        let mut truncated = object(0x32, MACOS_11);
        truncated.truncate(48);
        assert!(check(&truncated).is_err());
        let mut wrong_platform = object(0x32, MACOS_11);
        wrong_platform[40..44].copy_from_slice(&2_u32.to_le_bytes());
        assert!(check(&wrong_platform).is_err());
    }
}
