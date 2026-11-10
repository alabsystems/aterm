// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded, physically stable ciphertext observation. No kernel claim,
//! decryption key, mutation, or credential authority is returned.
use super::*;

pub(super) struct CipherObservation {
    file: std::fs::File,
    directory: PathBuf,
    physical: codex_enrollment::DirectoryIdentity,
    digest: [u8; 32],
    maximum: usize,
}
impl CipherObservation {
    pub(super) fn read(directory: &Path, maximum: usize) -> Result<(Self, SecretBytes), String> {
        platform::require_available().map_err(str::to_owned)?;
        platform::check_private_dir(directory).map_err(|_| "catalogue observation unavailable")?;
        let physical = codex_enrollment::DirectoryIdentity::capture(directory)?;
        let (file, _) = platform::open_private(directory, "candidate.age")
            .map_err(|_| "catalogue observation unavailable")?;
        let bytes = read_opened(&file, maximum)?;
        let observed = Self {
            file,
            directory: directory.into(),
            physical,
            digest: aterm_digest::Sha256::digest(bytes.as_slice()),
            maximum,
        };
        observed.unchanged().map_err(str::to_owned)?;
        Ok((observed, bytes))
    }
    pub(super) fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub(super) fn unchanged(&self) -> Result<(), &'static str> {
        platform::check_private_dir(&self.directory)
            .map_err(|_| "catalogue observation changed")?;
        if !self.physical.matches(&self.directory)
            || !astream_sys::file_matches_path(&self.file, &self.directory.join("candidate.age"))
                .unwrap_or(false)
        {
            return Err("catalogue observation changed");
        }
        let (checked, _) = platform::open_private(&self.directory, "candidate.age")
            .map_err(|_| "catalogue observation changed")?;
        same_private_file(&self.file, &checked)?;
        let bytes =
            read_opened(&self.file, self.maximum).map_err(|_| "catalogue observation changed")?;
        if aterm_digest::Sha256::digest(bytes.as_slice()) != self.digest
            || !self.physical.matches(&self.directory)
            || !astream_sys::file_matches_path(&self.file, &self.directory.join("candidate.age"))
                .unwrap_or(false)
        {
            return Err("catalogue observation changed");
        }
        Ok(())
    }
}
#[cfg(unix)]
fn same_private_file(
    original: &std::fs::File,
    checked: &std::fs::File,
) -> Result<(), &'static str> {
    use std::os::unix::fs::MetadataExt as _;
    let original = original
        .metadata()
        .map_err(|_| "catalogue observation changed")?;
    let checked = checked
        .metadata()
        .map_err(|_| "catalogue observation changed")?;
    if original.dev() != checked.dev()
        || original.ino() != checked.ino()
        || original.nlink() != 1
        || checked.nlink() != 1
    {
        return Err("catalogue observation changed");
    }
    Ok(())
}
#[cfg(not(unix))]
fn same_private_file(_: &std::fs::File, _: &std::fs::File) -> Result<(), &'static str> {
    Err("catalogue observation unsupported")
}
#[cfg(unix)]
fn read_opened(file: &std::fs::File, maximum: usize) -> Result<SecretBytes, String> {
    use std::os::unix::fs::FileExt as _;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let remaining = maximum.saturating_add(1).saturating_sub(bytes.len());
        if remaining == 0 {
            return Err("catalogue observation oversized".into());
        }
        let length = remaining.min(chunk.len());
        let read = file
            .read_at(&mut chunk[..length], bytes.len() as u64)
            .map_err(|_| "catalogue observation unavailable")?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    if bytes.len() > maximum {
        return Err("catalogue observation oversized".into());
    }
    Ok(SecretBytes::new(bytes))
}
#[cfg(not(unix))]
fn read_opened(_: &std::fs::File, _: usize) -> Result<SecretBytes, String> {
    Err("catalogue observation unsupported".into())
}
