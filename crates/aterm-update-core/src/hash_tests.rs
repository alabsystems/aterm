// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn sha256_matches_known_vector() {
    let dir = crate::test_scratch_dir("sha-vec");
    let f = dir.path().join("v.txt");
    std::fs::write(&f, b"abc").unwrap();
    let got = sha256_file(&f).unwrap();
    assert_eq!(
        got,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

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

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn ring_sha256(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

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
    let dir = crate::test_scratch_dir("sha-edge");
    let file = dir.path().join("- input\nfile");
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
    assert!(sha256_file(&dir.path().join("absent")).is_err());
    assert!(sha256_file(dir.path()).is_err());
    let fifo = dir.path().join("input.fifo");
    let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `c_fifo` is a valid NUL-terminated path inside the private temp directory.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    let fifo_err = sha256_file(&fifo).unwrap_err();
    assert!(
        fifo_err.contains("not a regular file"),
        "a FIFO at the SHA-256 input path must be refused without blocking: {fifo_err}"
    );
}
