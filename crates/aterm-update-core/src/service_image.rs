// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared signed service-image declarations, qualification and complete-image hashing.

#[cfg(unix)]
use crate::hash::sha256_open_file;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Explicit service wire compatibility, signed as part of the release appcast.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolRange {
    /// Oldest supported wire version.
    pub min: u32,
    /// Newest supported wire version.
    pub max: u32,
}
impl ProtocolRange {
    /// Whether every retained peer can use this image.
    pub fn supports(&self, retained: &[u32]) -> bool {
        self.min > 0
            && self.min <= self.max
            && retained.iter().all(|v| (self.min..=self.max).contains(v))
    }
}

/// The required `[service]` declaration in a signed appcast.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceDeclaration {
    /// Schema of the image digest and receipt.
    pub schema: u32,
    /// Native target triple.
    pub platform: String,
    /// Digest of the complete directory image using [`image_digest`].
    pub image_sha256: String,
    /// Relative path of the service executable within the image.
    pub executable: PathBuf,
    /// Digest of the executable's bytes.
    pub executable_sha256: String,
    /// Digest of the release qualification report's exact bytes.
    pub qualification_sha256: String,
    /// Service wire compatibility.
    pub protocol: ProtocolRange,
}

/// Release qualification shared by the producer and the independent service store.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Qualification {
    /// Report format.
    pub schema: u32,
    /// Verdict for the declared scope.
    pub status: String,
    /// Precisely the checks whose verdict this report records.
    pub scope: String,
    /// Monotonic release build.
    pub build: u64,
    /// Release source revision including its reserved claim.
    pub source_revision: String,
    /// Revision whose tree passed the release gate.
    pub verified_source_revision: String,
    /// Executable bytes judged before release signing.
    pub verified_input_sha256: String,
    /// Release gate's own evidence, including an accepted measurement exception.
    pub release_check_evidence: String,
    /// Complete signed image digest.
    pub image_sha256: String,
    /// Signed executable digest.
    pub executable_sha256: String,
    /// Range returned by the exact signed image.
    pub protocol: ProtocolRange,
    /// The exact image's protocol command succeeded.
    pub protocol_probe_passed: bool,
    /// The release native-signature check succeeded.
    pub native_signature_verified: bool,
    /// Physical health belongs to activation, not this release qualification.
    pub live_service_health_tested: bool,
}

impl Qualification {
    /// Refuse a failed, unsupported, incomplete or differently bound report.
    pub fn validate(
        &self,
        build: u64,
        revision: &str,
        service: &ServiceDeclaration,
    ) -> Result<(), String> {
        if self.schema != 1
            || self.status != "passed"
            || self.scope != "release-gate-and-signed-image-protocol"
            || build == 0
            || self.build != build
            || !valid_revision(revision)
            || self.source_revision != revision
            || !valid_revision(&self.verified_source_revision)
            || !valid_hash(&self.verified_input_sha256)
            || self.release_check_evidence.trim().is_empty()
            || service.schema != 1
            || service.platform.trim().is_empty()
            || !valid_executable(&service.executable)
            || !valid_hash(&self.image_sha256)
            || self.image_sha256 != service.image_sha256
            || !valid_hash(&self.executable_sha256)
            || self.executable_sha256 != service.executable_sha256
            || !valid_hash(&service.qualification_sha256)
            || self.protocol != service.protocol
            || !self.protocol.supports(&[])
            || !self.protocol_probe_passed
            || !self.native_signature_verified
            || self.live_service_health_tested
        {
            return Err("service qualification is not a complete matching pass".into());
        }
        Ok(())
    }
}

/// Whether `path` is a canonical, non-empty relative executable path (`Normal` segments separated
/// by single `/`, with no `.`, `..`, backslashes, or leading/trailing slashes).
#[must_use]
pub fn valid_executable_path(path: &Path) -> bool {
    let Some(raw) = path.to_str() else {
        return false;
    };
    !raw.is_empty()
        && !raw.contains('\\')
        && raw
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

fn valid_executable(path: &Path) -> bool {
    valid_executable_path(path)
}

fn valid_revision(s: &str) -> bool {
    aterm_codec::hex::is_lower(s, 40)
}

fn valid_hash(s: &str) -> bool {
    aterm_codec::hex::is_lower(s, 64)
}
#[cfg(unix)]
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
#[cfg(unix)]
fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

#[cfg(unix)]
const MAX_SYMLINK_HOPS: usize = 32;

#[cfg(unix)]
enum SymlinkStep {
    CurDir,
    ParentDir,
    Normal(std::ffi::OsString),
}

#[cfg(unix)]
fn push_symlink_target(pending: &mut Vec<SymlinkStep>, target: &Path) -> Result<(), String> {
    if target.as_os_str().is_empty() || target.is_absolute() {
        return Err("service image symlink escapes image".into());
    }
    let mut steps = Vec::new();
    for component in target.components() {
        match component {
            std::path::Component::Normal(s) => steps.push(SymlinkStep::Normal(s.to_os_string())),
            std::path::Component::CurDir => steps.push(SymlinkStep::CurDir),
            std::path::Component::ParentDir => steps.push(SymlinkStep::ParentDir),
            _ => return Err("service image symlink escapes image".into()),
        }
    }
    if target.as_os_str().as_encoded_bytes().ends_with(b"/") {
        steps.push(SymlinkStep::CurDir);
    }
    for step in steps.into_iter().rev() {
        pending.push(step);
    }
    Ok(())
}

#[cfg(unix)]
fn resolve_confined_symlink(root: &Path, path: &Path, first_target: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .and_then(|p| p.strip_prefix(root).ok())
        .ok_or("service image symlink escapes image")?;
    let mut stack = Vec::new();
    for component in parent.components() {
        match component {
            std::path::Component::Normal(s) => stack.push(s.to_os_string()),
            _ => return Err("service image symlink escapes image".into()),
        }
    }
    let mut pending = Vec::new();
    let mut hops = 1usize;
    push_symlink_target(&mut pending, first_target)?;
    while let Some(step) = pending.pop() {
        match step {
            SymlinkStep::CurDir => {}
            SymlinkStep::ParentDir => {
                if stack.pop().is_none() {
                    return Err("service image symlink escapes image".into());
                }
            }
            SymlinkStep::Normal(name) => {
                stack.push(name);
                let current = stack.iter().fold(root.to_path_buf(), |mut acc, part| {
                    acc.push(part);
                    acc
                });
                let meta = std::fs::symlink_metadata(&current).map_err(error)?;
                if meta.file_type().is_symlink() {
                    hops += 1;
                    if hops > MAX_SYMLINK_HOPS {
                        return Err("service image symlink loop or excessive depth".into());
                    }
                    stack.pop();
                    let next_target = std::fs::read_link(&current).map_err(error)?;
                    push_symlink_target(&mut pending, &next_target)?;
                } else if !pending.is_empty() && !meta.is_dir() {
                    return Err("service image symlink traverses non-directory".into());
                }
            }
        }
    }
    let final_path = stack.iter().fold(root.to_path_buf(), |mut acc, part| {
        acc.push(part);
        acc
    });
    let final_meta = std::fs::symlink_metadata(&final_path).map_err(error)?;
    if !(final_meta.is_file() || final_meta.is_dir()) {
        return Err("special file in service image".into());
    }
    if !path.canonicalize().map_err(error)?.starts_with(root) {
        return Err("service image symlink escapes image".into());
    }
    Ok(())
}

#[cfg(unix)]
/// Canonical complete-image digest: sorted relative names, entry kind, Unix mode and content.
/// Symlinks are bound by their exact target and may resolve only within the image.
pub fn image_digest(root: &Path) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt;
    fn walk(
        root: &Path,
        path: &Path,
        known_type: Option<std::fs::FileType>,
        hash: &mut aterm_digest::Sha256,
    ) -> Result<(), String> {
        let name = path
            .strip_prefix(root)
            .map_err(error)?
            .to_str()
            .ok_or("non-UTF8 image name")?;
        if known_type.is_some_and(|ft| ft.is_file()) {
            use std::os::unix::fs::OpenOptionsExt as _;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(path)
                .map_err(error)?;
            let opened = file.metadata().map_err(error)?;
            if !opened.is_file() {
                return Err("service image entry changed during hashing".into());
            }
            hash.update((name.len() as u64).to_be_bytes());
            hash.update(name.as_bytes());
            hash.update((opened.permissions().mode() & 0o7777).to_be_bytes());
            hash.update(b"f");
            hash.update(sha256_open_file(file)?.as_bytes());
            return Ok(());
        }
        let meta = std::fs::symlink_metadata(path).map_err(error)?;
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
        hash.update((meta.permissions().mode() & 0o7777).to_be_bytes());
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(path).map_err(error)?;
            resolve_confined_symlink(root, path, &target)?;
            hash.update(b"l");
            hash.update((target.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes());
            hash.update(target.as_os_str().as_encoded_bytes());
        } else if meta.is_file() {
            use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(path)
                .map_err(error)?;
            let opened = file.metadata().map_err(error)?;
            if !opened.is_file() || (opened.dev(), opened.ino()) != (meta.dev(), meta.ino()) {
                return Err("service image entry changed during hashing".into());
            }
            hash.update(b"f");
            hash.update(sha256_open_file(file)?.as_bytes());
        } else if meta.is_dir() {
            hash.update(b"d");
            let mut entries = std::fs::read_dir(path)
                .map_err(error)?
                .map(|e| e.map(|e| (e.file_name(), e.file_type().ok())))
                .collect::<Result<Vec<_>, _>>()
                .map_err(error)?;
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, entry_type) in entries {
                walk(root, &path.join(name), entry_type, hash)?;
            }
        } else {
            return Err("special file in service image".into());
        }
        Ok(())
    }
    if !std::fs::symlink_metadata(root).map_err(error)?.is_dir() {
        return Err("service image is not a directory".into());
    }
    let root = root.canonicalize().map_err(error)?;
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-service-image-v1\0");
    walk(&root, &root, None, &mut hash)?;
    Ok(hex(&hash.finalize()))
}

#[cfg(not(unix))]
/// Complete service-image hashing requires Unix file identity and permissions.
pub fn image_digest(_root: &Path) -> Result<String, String> {
    Err("service image hashing requires Unix".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NEGATIVE CONTROL: `Qualification::validate` rejects blank platforms and non-relative
    /// executable paths even when every hash and protocol field matches.
    #[test]
    fn qualification_validate_rejects_blank_platform_and_non_relative_executable() {
        let valid_service = ServiceDeclaration {
            schema: 1,
            platform: "aarch64-apple-darwin".into(),
            image_sha256: "a".repeat(64),
            executable: PathBuf::from("Contents/MacOS/aterm"),
            executable_sha256: "b".repeat(64),
            qualification_sha256: "c".repeat(64),
            protocol: ProtocolRange { min: 1, max: 2 },
        };
        let report = Qualification {
            schema: 1,
            status: "passed".into(),
            scope: "release-gate-and-signed-image-protocol".into(),
            build: 42,
            source_revision: "1".repeat(40),
            verified_source_revision: "2".repeat(40),
            verified_input_sha256: "3".repeat(64),
            release_check_evidence: "verified".into(),
            image_sha256: "a".repeat(64),
            executable_sha256: "b".repeat(64),
            protocol: ProtocolRange { min: 1, max: 2 },
            protocol_probe_passed: true,
            native_signature_verified: true,
            live_service_health_tested: false,
        };
        let rev = "1".repeat(40);
        assert!(report.validate(42, &rev, &valid_service).is_ok());

        for bad_platform in ["", "   ", "\t\n"] {
            let mut s = valid_service.clone();
            s.platform = bad_platform.into();
            assert!(
                report.validate(42, &rev, &s).is_err(),
                "platform {bad_platform:?}"
            );
        }
        for bad_exe in [
            "",
            ".",
            "..",
            "/bin/aterm",
            "Contents/../MacOS/aterm",
            "Contents/./MacOS/aterm",
        ] {
            let mut s = valid_service.clone();
            s.executable = PathBuf::from(bad_exe);
            assert!(
                report.validate(42, &rev, &s).is_err(),
                "executable {bad_exe:?}"
            );
        }
    }

    /// NEGATIVE CONTROL: multi-hop symlink chains that bounce above the image root via an
    /// intermediate in-image symlink (`sub/up -> ..` then `bounce -> sub/up/../reenter/aterm`)
    /// or put a trailing slash on a regular file are refused by `image_digest`.
    #[cfg(unix)]
    #[test]
    fn image_digest_refuses_multi_hop_root_escape_and_trailing_slash_on_file() {
        use std::os::unix::fs::symlink;

        let tmp = crate::test_scratch_dir("svc-img");
        let reenter = tmp.path().join("reenter");
        std::fs::create_dir_all(reenter.join("sub")).unwrap();
        std::fs::write(reenter.join("aterm"), b"exe").unwrap();
        symlink("..", reenter.join("sub/up")).unwrap();
        assert!(
            image_digest(&reenter).is_ok(),
            "an in-image parent hop that stays at root is valid"
        );

        symlink("sub/up/../reenter/aterm", reenter.join("bounce")).unwrap();
        let err = image_digest(&reenter).expect_err("two-hop escape above root must fail");
        assert!(err.contains("escapes image"), "{err}");
        std::fs::remove_file(reenter.join("bounce")).unwrap();

        symlink("aterm/", reenter.join("file_slash")).unwrap();
        assert!(
            image_digest(&reenter).is_err(),
            "trailing slash on regular file must fail"
        );
    }
}
