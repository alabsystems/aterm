// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Linux release identity shared by the cutter, bootstrap installer contract,
//! and native updater. Linux updates are raw ELF executables, not archives.
//! An ELF header is a compatibility check, NEVER an authenticity check: callers
//! must first verify the signed manifest and then the complete file's digest.

/// A deliberately bounded native executable download (512 MiB).
pub const LINUX_BINARY_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// The side-effect-free `aterm update identity` response. A candidate is executed
/// for this probe ONLY AFTER its signed artifact digest has been authenticated.
/// This proves the loader and compiled identity agree with the signed metadata;
/// it does not replace signature verification or the ordinary-launch health trial.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BinaryIdentity {
    pub schema: u32,
    pub version: String,
    pub build_number: u64,
    pub commit: String,
    pub target: String,
    pub dirty: bool,
}

impl BinaryIdentity {
    pub fn verify_against(
        &self,
        manifest: &crate::Manifest,
        target: LinuxTarget,
    ) -> Result<(), String> {
        if self.schema != 1
            || self.dirty
            || self.build_number == 0
            || self.version != manifest.version
            || self.build_number != manifest.build_number
            || self.commit.len() != 40
            || !self
                .commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || manifest.commit.as_deref() != Some(self.commit.as_str())
            || self.target != target.triple()
        {
            return Err(
                "Linux executable's compiled identity differs from its signed release".into(),
            );
        }
        Ok(())
    }
}

/// Linux targets for which the release protocol defines executable assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxTarget {
    X86_64,
    Aarch64,
}

impl LinuxTarget {
    /// The executable target, not the host on which a cross-build runs.
    #[must_use]
    pub const fn native() -> Option<Self> {
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(Self::X86_64)
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Some(Self::Aarch64)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn arch(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }

    #[must_use]
    pub const fn triple(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64-unknown-linux-gnu",
            Self::Aarch64 => "aarch64-unknown-linux-gnu",
        }
    }

    /// The one versioned asset spelling. Admission also validates `version`.
    #[must_use]
    pub fn asset_name(self, version: &str) -> String {
        format!("aterm-{version}-linux-{}", self.arch())
    }

    /// Refuse truncated, foreign, 32-bit, big-endian, or non-executable ELF.
    /// The 64-byte header may be supplied independently of the rest of a file.
    pub fn validate_elf_header(self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() < 64 || bytes.get(..4) != Some(b"\x7fELF") {
            return Err("Linux artifact is not a complete ELF64 header".into());
        }
        if bytes[4] != 2 || bytes[5] != 1 || bytes[6] != 1 {
            return Err("Linux artifact must be ELF64, little-endian, ELF version 1".into());
        }
        let kind = u16::from_le_bytes([bytes[16], bytes[17]]);
        let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
        let version = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        let header_size = u16::from_le_bytes([bytes[52], bytes[53]]);
        let expected_machine = match self {
            Self::X86_64 => 62,
            Self::Aarch64 => 183,
        };
        if !matches!(kind, 2 | 3) || version != 1 || header_size != 64 {
            return Err("Linux artifact is not a supported ELF64 executable or PIE".into());
        }
        if machine != expected_machine {
            return Err(format!(
                "Linux artifact ELF machine {machine} does not match {} ({expected_machine})",
                self.triple()
            ));
        }
        Ok(())
    }
}

/// A complete, validated target entry from a manifest. Its authentication is
/// the caller's responsibility: parsing does not establish signing authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxArtifact<'a> {
    pub target: LinuxTarget,
    pub name: &'a str,
    pub sha256: &'a str,
    pub size: u64,
}

pub(crate) fn artifact<'a>(
    target: LinuxTarget,
    version: &str,
    name: Option<&'a str>,
    sha256: Option<&'a str>,
    size: Option<u64>,
) -> Result<Option<LinuxArtifact<'a>>, String> {
    let (name, sha256, size) = match (name, sha256, size) {
        (None, None, None) => return Ok(None),
        (Some(name), Some(sha256), Some(size)) => (name, sha256, size),
        _ => {
            return Err(format!(
                "incomplete Linux {} artifact: name, sha256, and size must appear together",
                target.arch()
            ));
        }
    };
    if !matches!(
        crate::tag::parse_release_tag(&format!("v{version}")),
        Ok(crate::tag::TagKind::Candidate(_))
    ) {
        return Err("Linux artifact requires a canonical three-component release version".into());
    }
    if name != target.asset_name(version) {
        return Err(format!(
            "Linux {} artifact must be named {}",
            target.arch(),
            target.asset_name(version)
        ));
    }
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(format!(
            "Linux {} sha256 is not lowercase SHA-256",
            target.arch()
        ));
    }
    if !(64..=LINUX_BINARY_MAX_BYTES).contains(&size) {
        return Err(format!(
            "Linux {} size {size} is outside 64..={LINUX_BINARY_MAX_BYTES}",
            target.arch()
        ));
    }
    Ok(Some(LinuxArtifact {
        target,
        name,
        sha256,
        size,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_identity_binds_every_signed_field_and_rejects_dirty_sources() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let manifest = crate::Manifest::parse(&format!(
            "schema=1\nversion='0.86.0'\nbuild_number=123\ncommit='{commit}'\ndmg='aterm-0.86.0.dmg'\nsha256='x'\n"
        )).unwrap();
        let identity = BinaryIdentity {
            schema: 1,
            version: manifest.version.clone(),
            build_number: 123,
            commit: commit.into(),
            target: LinuxTarget::Aarch64.triple().into(),
            dirty: false,
        };
        identity
            .verify_against(&manifest, LinuxTarget::Aarch64)
            .unwrap();
        let json = aterm_json::to_vec(&identity).unwrap();
        let back: BinaryIdentity = aterm_json::from_slice(&json).unwrap();
        assert_eq!(identity, back);
        for index in 0..8 {
            let mut wrong = identity.clone();
            match index {
                0 => wrong.schema = 2,
                1 => wrong.version = "0.87.0".into(),
                2 => wrong.build_number = 124,
                3 => wrong.commit = "f".repeat(40),
                4 => wrong.commit.truncate(12),
                5 => wrong.target = LinuxTarget::X86_64.triple().into(),
                6 => wrong.dirty = true,
                _ => wrong.build_number = 0,
            }
            assert!(
                wrong
                    .verify_against(&manifest, LinuxTarget::Aarch64)
                    .is_err(),
                "mutant {index}"
            );
        }
    }

    fn header(target: LinuxTarget) -> [u8; 64] {
        let mut bytes = [0; 64];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4..7].copy_from_slice(&[2, 1, 1]);
        bytes[16] = 3;
        bytes[18] = match target {
            LinuxTarget::X86_64 => 62,
            LinuxTarget::Aarch64 => 183,
        };
        bytes[20] = 1;
        bytes[52] = 64;
        bytes
    }

    #[test]
    fn exact_native_and_cross_architecture_headers() {
        for target in [LinuxTarget::X86_64, LinuxTarget::Aarch64] {
            let bytes = header(target);
            target.validate_elf_header(&bytes).unwrap();
            let other = if target == LinuxTarget::X86_64 {
                LinuxTarget::Aarch64
            } else {
                LinuxTarget::X86_64
            };
            assert!(other.validate_elf_header(&bytes).is_err());
        }
    }

    #[test]
    fn elf_compatibility_is_fail_closed() {
        let target = LinuxTarget::Aarch64;
        let bytes = header(target);
        for length in 0..64 {
            assert!(target.validate_elf_header(&bytes[..length]).is_err());
        }
        for (index, wrong) in [(0, 0), (4, 1), (5, 2), (6, 2), (16, 1), (20, 2), (52, 0)] {
            let mut bad = bytes;
            bad[index] = wrong;
            assert!(target.validate_elf_header(&bad).is_err(), "byte {index}");
        }
        let mut executable = bytes;
        executable[16] = 2;
        target.validate_elf_header(&executable).unwrap();
    }

    #[test]
    fn canonical_names_do_not_alias_targets_or_tarballs() {
        assert_eq!(LinuxTarget::Aarch64.triple(), "aarch64-unknown-linux-gnu");
        assert_eq!(
            LinuxTarget::X86_64.asset_name("0.86.0"),
            "aterm-0.86.0-linux-x86_64"
        );
        assert_eq!(
            LinuxTarget::Aarch64.asset_name("0.86.0"),
            "aterm-0.86.0-linux-aarch64"
        );
    }
}
