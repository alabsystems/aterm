// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SHA-256 of a file, by shelling the OS hash tool — keeps the crate crypto-free
//! (no in-process hash dependency), matching the release build scripts. Used to
//! verify a downloaded artifact against a manifest digest. Unix shells
//! `/usr/bin/shasum`; Windows shells `certutil -hashfile` (ships with the OS).

use std::path::Path;
use std::process::Command;

/// `shasum -a 256 <file>` (Unix) / `certutil -hashfile <file> SHA256` (Windows)
/// → lowercase hex digest. Used to verify a downloaded artifact against the
/// manifest (shelling the OS tool keeps the crate crypto-free, matching the
/// build scripts).
// Skip: from_utf8_lossy/format over `shasum` OUTPUT (pure-ASCII hex by
// contract; a mangled digest fails the strict-equality check downstream —
// fail-closed) — hardened byte_loss/format classes. Audited (update-atpkg).
#[cfg_attr(trust_verify, trust::skip)]
pub fn sha256_file(path: &Path) -> Result<String, String> {
    #[cfg(unix)]
    {
        let out = Command::new("/usr/bin/shasum")
            .args(["-a", "256"])
            .arg(path)
            .output()
            .map_err(|e| format!("spawn shasum: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "shasum failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout
            .split_whitespace()
            .next()
            .map(|h| h.to_ascii_lowercase())
            .ok_or_else(|| "shasum produced no digest".to_string())
    }

    #[cfg(windows)]
    {
        let out = Command::new("certutil")
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
        // Output shape: a header line, the digest line, a status line. Older
        // Windows builds print the digest as spaced hex pairs — strip spaces
        // and take the first line that is exactly 64 hex digits.
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout
            .lines()
            .map(|l| {
                l.split_whitespace()
                    .collect::<String>()
                    .to_ascii_lowercase()
            })
            .find(|l| l.len() == 64 && l.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| "certutil produced no digest".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        // SHA-256("abc") — the canonical test vector.
        let dir = std::env::temp_dir().join(format!("aterm-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("v.txt");
        std::fs::write(&f, b"abc").unwrap();
        let got = sha256_file(&f).unwrap();
        assert_eq!(
            got,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
