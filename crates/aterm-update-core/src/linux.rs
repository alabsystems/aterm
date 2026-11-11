// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Linux release identity and ELF64 header validation shared by `aterm-release` and `aterm-update`.

/// A deliberately bounded native executable download (512 MiB).
pub const LINUX_BINARY_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// The side-effect-free `aterm update identity` response, verified against the signed [`crate::Manifest`].
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
            || !aterm_codec::hex::is_lower(&self.commit, 40)
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

    /// Refuse truncated, foreign, 32-bit, big-endian, or non-executable ELF64 headers.
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

/// A validated Linux target entry from a release manifest.
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
    if !aterm_codec::hex::is_lower(sha256, 64) {
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
#[path = "linux_tests.rs"]
mod tests;
