// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! File SHA-256 hashing across Unix and Windows.

use std::path::Path;

/// Compute the lowercase hex SHA-256 digest of `path`.
// Skip: format over the open error, and from_utf8_lossy/format over `certutil`
// OUTPUT (pure-ASCII hex by contract; command output must contain exactly 64 hex
// digits before it is returned) — hardened byte_loss/format classes. Audited
// (update-atpkg).
#[cfg_attr(trust_verify, trust::skip)]
pub fn sha256_file(path: &Path) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        let file = open_sha256_input(path)?;
        sha256_open_file(file)
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let file = open_sha256_input(path)?;
        sha256_open_file(file)
    }

    #[cfg(windows)]
    {
        let meta = std::fs::metadata(path).map_err(|e| format!("open SHA-256 input: {e}"))?;
        if !meta.is_file() {
            return Err("open SHA-256 input: not a regular file".to_string());
        }
        let out = aterm_uds::spawnfd::command("certutil")
            .arg("-hashfile")
            .arg(path)
            .arg("SHA256")
            .output()
            .map_err(|e| format!("spawn certutil: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "certutil failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout
            .lines()
            .map(|l| {
                l.split_whitespace()
                    .collect::<String>()
                    .to_ascii_lowercase()
            })
            .find(|l| aterm_codec::hex::is_hex(l, 64))
            .ok_or_else(|| "certutil produced no digest".to_string())
    }
}

#[cfg(unix)]
pub(crate) fn sha256_open_file(file: std::fs::File) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        sha256_stream(file)
    }
    #[cfg(not(target_os = "linux"))]
    {
        hardware_sha256_stream(file)
    }
}

#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn open_sha256_input(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("open SHA-256 input: {e}"))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("open SHA-256 input: {e}"))?;
    if !meta.is_file() {
        return Err("open SHA-256 input: not a regular file".to_string());
    }
    Ok(file)
}

// Skip: format over the read error, the class `sha256_file` skips.
/// Feed everything `input` yields to `update`, in 64 KiB chunks with `Interrupted`
/// retried: the one read loop both Unix arms of [`sha256_file`] hash through.
#[cfg(any(unix, test))]
#[cfg_attr(trust_verify, trust::skip)]
fn read_stream(mut input: impl std::io::Read, mut update: impl FnMut(&[u8])) -> Result<(), String> {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = match input.read(&mut buffer) {
            Ok(count) => count,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("read SHA-256 input: {e}")),
        };
        if count == 0 {
            return Ok(());
        }
        update(&buffer[..count.min(buffer.len())]);
    }
}

/// Lowercase SHA-256 of everything `input` yields on aterm-digest's scalar hasher:
/// the Linux arm of [`sha256_file`]. Compiled for tests on every host, so the
/// recorded answers below hold this exact path wherever the suite runs, not only
/// on Linux.
#[cfg(any(target_os = "linux", test))]
fn sha256_stream(input: impl std::io::Read) -> Result<String, String> {
    let mut hasher = aterm_digest::Sha256::new();
    read_stream(input, |chunk| hasher.update(chunk))?;
    Ok(aterm_codec::hex::encode_lower(&hasher.finalize()))
}

/// Lowercase SHA-256 of everything `input` yields on ring, which drives the CPU's
/// SHA-256 instructions (ARMv8 SHA2 on Apple silicon, SHA-NI on x86_64): the macOS
/// arm of [`sha256_file`]. A 47 MiB update container hashes in tens of
/// milliseconds; the same FIPS 180-4 digest as `sha256_stream`, which the
/// recorded answers below hold both to.
#[cfg(any(all(unix, not(target_os = "linux")), test))]
fn hardware_sha256_stream(input: impl std::io::Read) -> Result<String, String> {
    let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
    read_stream(input, |chunk| hasher.update(chunk))?;
    Ok(aterm_codec::hex::encode_lower(hasher.finish().as_ref()))
}

#[cfg(test)]
#[path = "hash_tests.rs"]
mod tests;
