// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SHA-256 of a file. Linux streams bytes through aterm-digest's first-party
//! SHA-256, so native update verification does not depend on Perl's optional
//! `shasum`.
//! Other Unix hosts use `/usr/bin/shasum`; Windows uses `certutil -hashfile`,
//! built through `aterm_uds::spawnfd::command` so a caller with no console (the
//! window) does not flash one for it.

use std::path::Path;

/// Lowercase SHA-256 of the complete file. Native Linux reads the file directly;
/// other hosts use their existing system hash command. Callers establish path
/// ownership and regular-file identity before trusting the file being hashed.
// Skip: from_utf8_lossy/format over `shasum` OUTPUT (pure-ASCII hex by
// contract; command output must contain exactly 64 hex digits before it is
// returned) — hardened byte_loss/format classes. Audited (update-atpkg).
#[cfg_attr(trust_verify, trust::skip)]
pub fn sha256_file(path: &Path) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        let file = std::fs::File::open(path).map_err(|e| format!("open SHA-256 input: {e}"))?;
        sha256_stream(file)
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let out = shasum_command(path)
            .output()
            .map_err(|e| format!("spawn shasum: {e}"))?;
        shasum_digest(&out)
    }

    #[cfg(windows)]
    {
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
            .find(|l| aterm_codec::hex::is_hex(l, 64))
            .ok_or_else(|| "certutil produced no digest".to_string())
    }
}

/// Lowercase SHA-256 of everything `input` yields: the Linux arm of
/// [`sha256_file`], read in 64 KiB chunks with `Interrupted` retried. Compiled
/// for tests on every host, so the recorded answers below hold this exact loop
/// wherever the suite runs, not only on Linux.
// Skip: format over the read error, the class `sha256_file` skips (this is its
// Linux arm, moved out whole).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(trust_verify, trust::skip)]
fn sha256_stream(mut input: impl std::io::Read) -> Result<String, String> {
    let mut hasher = aterm_digest::Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = match input.read(&mut buffer) {
            Ok(count) => count,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("read SHA-256 input: {e}")),
        };
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(aterm_codec::hex::encode_lower(&hasher.finalize()))
}

/// The `shasum -a 256 -- <path>` [`sha256_file`] runs, for a caller that must bound
/// it: the update checker hashes a downloaded container on its own thread while it
/// holds the check lane and the stage lock, and runs this through its bounded
/// helper (`aterm-update`'s `output_within`), so a read that never returns (a
/// wedged volume) cannot keep both for good. [`shasum_digest`] reads its output.
#[cfg(all(unix, not(target_os = "linux")))]
#[must_use]
pub fn shasum_command(path: &Path) -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/shasum");
    cmd.args(["-a", "256", "--"]).arg(path);
    cmd
}

/// The lowercase digest a finished [`shasum_command`] printed, or its refusal.
// Skip: from_utf8_lossy/format over `shasum` OUTPUT, as `sha256_file` above.
#[cfg(all(unix, not(target_os = "linux")))]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shasum_digest(out: &std::process::Output) -> Result<String, String> {
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
        // Checksum tools prefix the digest with a backslash when the filename
        // is escaped (for a newline or backslash). The digest itself is unchanged.
        .map(|h| h.strip_prefix('\\').unwrap_or(h))
        .map(|h| h.to_ascii_lowercase())
        .filter(|h| aterm_codec::hex::is_hex(h, 64))
        .ok_or_else(|| "shasum produced no SHA-256 digest".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(unix, not(target_os = "linux")))]
    #[test]
    fn shasum_output_requires_a_complete_hex_digest() {
        use std::os::unix::process::ExitStatusExt as _;

        let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let mut output = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: format!("{}  input file\n", digest.to_ascii_uppercase()).into_bytes(),
            stderr: Vec::new(),
        };
        assert_eq!(shasum_digest(&output).unwrap(), digest);
        for malformed in ["", "Usage: shasum", &digest[..63], &"g".repeat(64)] {
            output.stdout = malformed.as_bytes().to_vec();
            assert!(shasum_digest(&output).is_err(), "{malformed:?}");
        }
        output.stdout = digest.as_bytes().to_vec();
        output.status = std::process::ExitStatus::from_raw(1 << 8);
        assert!(shasum_digest(&output).is_err());
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    #[test]
    fn shasum_hashes_option_like_and_escaped_filenames() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-sha-option-")
            .tempdir()
            .unwrap();
        for name in ["--help", "line\nbreak", "back\\slash"] {
            std::fs::write(dir.path().join(name), b"abc").unwrap();
            let output = shasum_command(Path::new(name))
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert_eq!(
                shasum_digest(&output).unwrap(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                "{name:?}"
            );
        }
    }

    #[test]
    fn sha256_matches_known_vector() {
        // SHA-256("abc") — the canonical test vector.
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-sha-")
            .tempdir()
            .unwrap();
        let f = dir.path().join("v.txt");
        std::fs::write(&f, b"abc").unwrap();
        let got = sha256_file(&f).unwrap();
        assert_eq!(
            got,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// The Linux arm's answers, recorded from ring's SHA-256 (its hasher until
    /// 2026-10-15) and held against ring on every run. The input is
    /// [`pattern`]; the lengths are empty, one byte, both sides of the 64 KiB
    /// read buffer, and sixteen whole buffers.
    const RECORDED: [(usize, &str); 6] = [
        (
            0,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            1,
            "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
        ),
        (
            65_535,
            "dda402a2c028f0cbbdbc5c6ebae965eed9c75f71236e7022b0386d3455d5ae2f",
        ),
        (
            65_536,
            "4b640d85ab3ba30fd02c9fc9db4a8928f416322ad27022ea58a65aaee68a4df2",
        ),
        (
            65_537,
            "237356e18b503616912abb8ffaed3a72591e397d4ac294c4637917d48a3f529d",
        ),
        (
            1 << 20,
            "631b84027d6b9e52b539c4e8373622d23032dfadc64d60af87339c9037e4f769",
        ),
    ];

    /// `len` bytes of `i % 251`: a prime period, so no 64-byte block repeats
    /// its neighbour and a block-order slip changes the digest.
    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn ring_sha256(bytes: &[u8]) -> String {
        aterm_codec::hex::encode_lower(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
    }

    /// A reader that fails `Interrupted` before every read and then hands out
    /// at most `step` bytes, so the arm's retry and its short-read path both run.
    struct Choppy<'a> {
        rest: &'a [u8],
        step: usize,
        interrupt: bool,
    }

    impl std::io::Read for Choppy<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.interrupt = !self.interrupt;
            if self.interrupt {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let n = self.step.min(buf.len()).min(self.rest.len());
            buf[..n].copy_from_slice(&self.rest[..n]);
            self.rest = &self.rest[n..];
            Ok(n)
        }
    }

    #[test]
    fn stream_hash_matches_ring_and_the_recorded_answers() {
        for (len, want) in RECORDED {
            let bytes = pattern(len);
            assert_eq!(ring_sha256(&bytes), want, "ring vs the record, {len} bytes");
            assert_eq!(
                sha256_stream(bytes.as_slice()).unwrap(),
                want,
                "{len} bytes"
            );
            let choppy = Choppy {
                rest: &bytes,
                step: 4093,
                interrupt: false,
            };
            assert_eq!(sha256_stream(choppy).unwrap(), want, "{len} bytes, choppy");
        }
    }

    /// The negative control: the comparison above can tell inputs apart. One
    /// flipped bit in the largest row moves both hashers off the record and
    /// onto the same new answer, and no two rows share a digest.
    #[test]
    fn the_recorded_answers_are_not_vacuous() {
        let (len, want) = RECORDED[RECORDED.len() - 1];
        let mut bytes = pattern(len);
        bytes[65_536] ^= 1;
        let flipped = sha256_stream(bytes.as_slice()).unwrap();
        assert_ne!(flipped, want);
        assert_eq!(flipped, ring_sha256(&bytes));
        for (i, (_, a)) in RECORDED.iter().enumerate() {
            for (_, b) in &RECORDED[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn a_read_error_is_an_error_not_a_digest() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("wedged"))
            }
        }
        let err = sha256_stream(Broken).unwrap_err();
        assert!(err.starts_with("read SHA-256 input: "), "{err}");
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
