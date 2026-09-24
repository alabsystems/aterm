// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SHA-256 of a file. Linux streams bytes through the existing ring dependency,
//! so native update verification does not depend on Perl's optional `shasum`.
//! Other Unix hosts use `/usr/bin/shasum`; Windows uses `certutil -hashfile`.

use std::path::Path;
#[cfg(not(target_os = "linux"))]
use std::process::Command;

/// Lowercase SHA-256 of the complete file. Native Linux reads the file directly;
/// other hosts use their existing system hash command. Callers establish path
/// ownership and regular-file identity before trusting the file being hashed.
// Skip: from_utf8_lossy/format over `shasum` OUTPUT (pure-ASCII hex by
// contract; a mangled digest fails the strict-equality check downstream —
// fail-closed) — hardened byte_loss/format classes. Audited (update-atpkg).
#[cfg_attr(trust_verify, trust::skip)]
pub fn sha256_file(path: &Path) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        use std::io::Read as _;
        let mut file = std::fs::File::open(path).map_err(|e| format!("open SHA-256 input: {e}"))?;
        let mut context = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = match file.read(&mut buffer) {
                Ok(count) => count,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("read SHA-256 input: {e}")),
            };
            if count == 0 {
                break;
            }
            context.update(&buffer[..count]);
        }
        let digest = context.finish();
        Ok(digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    #[cfg(all(unix, not(target_os = "linux")))]
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

    #[cfg(target_os = "linux")]
    #[test]
    fn native_hash_handles_multiple_buffers_empty_input_and_unusual_names() {
        let dir = std::env::temp_dir().join(format!("aterm-sha-edge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("- input\nfile");
        std::fs::write(&file, []).unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let bytes = vec![b'a'; 1_000_000];
        std::fs::write(&file, &bytes).unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        assert!(sha256_file(&dir.join("absent")).is_err());
        assert!(sha256_file(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
