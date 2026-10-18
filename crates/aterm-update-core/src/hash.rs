// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SHA-256 of a file, read in process on Unix. macOS (and every Unix but Linux)
//! hashes with ring, on the CPU's SHA-256 instructions — the bulk path atpkg's
//! `tree::BulkSha256` takes; it used to spawn `/usr/bin/shasum`, a Perl start plus a
//! software hash, for every downloaded update container. Linux streams bytes
//! through aterm-digest's first-party SHA-256, so native update verification does
//! not depend on Perl's optional `shasum`. Windows uses `certutil -hashfile`, built
//! through `aterm_uds::spawnfd::command` so a caller with no console (the window)
//! does not flash one for it.

use std::path::Path;

/// Lowercase SHA-256 of the complete file. Unix reads the file directly; Windows
/// uses its system hash command. Callers establish path ownership and regular-file
/// identity before trusting the file being hashed. A read that never returns (a
/// wedged volume) does not return here either: a caller that must bound it runs
/// this on a thread of its own (`aterm-update`'s `container_sha256`).
// Skip: format over the open error, and from_utf8_lossy/format over `certutil`
// OUTPUT (pure-ASCII hex by contract; command output must contain exactly 64 hex
// digits before it is returned) — hardened byte_loss/format classes. Audited
// (update-atpkg).
#[cfg_attr(trust_verify, trust::skip)]
pub fn sha256_file(path: &Path) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        let file = std::fs::File::open(path).map_err(|e| format!("open SHA-256 input: {e}"))?;
        sha256_stream(file)
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let file = std::fs::File::open(path).map_err(|e| format!("open SHA-256 input: {e}"))?;
        hardware_sha256_stream(file)
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

/// Feed everything `input` yields to `update`, in 64 KiB chunks with `Interrupted`
/// retried: the one read loop both Unix arms of [`sha256_file`] hash through.
// Skip: format over the read error, the class `sha256_file` skips.
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
        // The `Read` contract guarantees `count <= buffer.len()`; `min` keeps a
        // nonconforming reader from panicking the slice.
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
/// milliseconds; the same FIPS 180-4 digest as [`sha256_stream`], which the
/// recorded answers below hold both to.
#[cfg(any(all(unix, not(target_os = "linux")), test))]
fn hardware_sha256_stream(input: impl std::io::Read) -> Result<String, String> {
    let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
    read_stream(input, |chunk| hasher.update(chunk))?;
    Ok(aterm_codec::hex::encode_lower(hasher.finish().as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Both Unix arms' answers, recorded from ring's SHA-256 (the Linux arm's hasher
    /// until 2026-10-15, the macOS arm's since 2026-10-17) and held against ring on
    /// every run. The input is
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

    /// Both Unix arms — the scalar one Linux ships and the hardware one macOS ships —
    /// give the recorded answers, whole and through a reader that interrupts every
    /// read and hands out odd-sized pieces.
    #[test]
    fn stream_hash_matches_ring_and_the_recorded_answers() {
        for (len, want) in RECORDED {
            let bytes = pattern(len);
            assert_eq!(ring_sha256(&bytes), want, "ring vs the record, {len} bytes");
            for step in [bytes.len().max(1), 4093] {
                let reader = || Choppy {
                    rest: &bytes,
                    step,
                    interrupt: false,
                };
                assert_eq!(
                    sha256_stream(reader()).unwrap(),
                    want,
                    "scalar, {len} bytes, {step}-byte reads"
                );
                assert_eq!(
                    hardware_sha256_stream(reader()).unwrap(),
                    want,
                    "hardware, {len} bytes, {step}-byte reads"
                );
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
        for err in [
            sha256_stream(Broken).unwrap_err(),
            hardware_sha256_stream(Broken).unwrap_err(),
        ] {
            assert!(err.starts_with("read SHA-256 input: "), "{err}");
        }
    }

    /// The file arm every Unix ships, through `open(2)` and the read loop: an empty
    /// file, a million bytes (sixteen whole buffers and a partial one), a name that
    /// starts with a dash and holds a newline, and a path that is missing or a
    /// directory, which have no digest.
    #[cfg(unix)]
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
