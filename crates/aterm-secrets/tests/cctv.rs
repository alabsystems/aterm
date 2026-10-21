// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The C2SP CCTV age vectors that apply to an X25519-only reader (tests/cctv/README.md),
//! each held to its exact class: `success` to the payload's SHA-256, every failure to the
//! one `AgeError` it names.

use aterm_secrets::age::{AgeError, Identity, decrypt};
use std::path::{Path, PathBuf};

/// What a vector expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Success,
    Fails(AgeError),
}

fn expect(class: &str) -> Expect {
    match class {
        "success" => Expect::Success,
        "no match" => Expect::Fails(AgeError::NoMatch),
        "HMAC failure" => Expect::Fails(AgeError::Hmac),
        "header failure" => Expect::Fails(AgeError::Header),
        "payload failure" => Expect::Fails(AgeError::Payload),
        "armor failure" => Expect::Fails(AgeError::Armor),
        other => panic!("unknown CCTV class {other:?}"),
    }
}

struct Vector {
    name: String,
    expect: Expect,
    payload: Option<[u8; 32]>,
    identities: Vec<[u8; 32]>,
    armored: bool,
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("cctv")
}

fn hex32(text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    assert!(
        aterm_codec::hex::decode_into(text.as_bytes(), &mut out),
        "hex: {text}"
    );
    out
}

fn index() -> Vec<Vector> {
    let text = std::fs::read_to_string(dir().join("index.tsv")).expect("tests/cctv/index.tsv");
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 5, "index row: {line}");
            Vector {
                name: fields[0].to_string(),
                expect: expect(fields[1]),
                payload: (fields[2] != "-").then(|| hex32(fields[2])),
                identities: if fields[3] == "-" {
                    Vec::new()
                } else {
                    fields[3].split(',').map(hex32).collect()
                },
                armored: match fields[4] {
                    "y" => true,
                    "n" => false,
                    other => panic!("armored column {other:?}"),
                },
            }
        })
        .collect()
}

/// The age file: `<name>.age` raw, or `<name>.age.zlib` as CCTV ships its large vectors.
fn file(name: &str) -> Vec<u8> {
    let raw = dir().join(format!("{name}.age"));
    if raw.exists() {
        return std::fs::read(raw).expect("vector");
    }
    let compressed = std::fs::read(dir().join(format!("{name}.age.zlib"))).expect("vector");
    aterm_codec::inflate::zlib_decompress(&compressed, 32 << 20).expect("zlib vector")
}

fn sha256_hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

#[test]
fn every_applicable_cctv_vector_has_its_exact_class() {
    let vectors = index();
    assert_eq!(vectors.len(), 102, "the X25519-only subset of CCTV age");
    let mut failures = Vec::new();
    for vector in &vectors {
        let data = file(&vector.name);
        // The reader takes the armor path for exactly the files CCTV marks armored: those
        // that do not begin with the version line's fixed part (and are not empty).
        let read_as_armor = !data.is_empty() && !data.starts_with(b"age-encryption.org/");
        if vector.armored != read_as_armor {
            failures.push(format!(
                "{}: armored column disagrees with the file",
                vector.name
            ));
        }
        let identities: Vec<Identity> = vector
            .identities
            .iter()
            .map(|secret| Identity::from_secret_bytes(*secret))
            .collect();
        let got = decrypt(&identities, &data);
        match (vector.expect, got) {
            (Expect::Success, Ok(plaintext)) => {
                let want = vector.payload.expect("a success vector names its payload");
                let have = aterm_digest::Sha256::digest(plaintext.as_slice());
                if have != want {
                    failures.push(format!(
                        "{}: payload sha256 {} != {}",
                        vector.name,
                        sha256_hex(plaintext.as_slice()),
                        aterm_codec::hex::encode_lower(&want)
                    ));
                }
            }
            (Expect::Fails(want), Err(have)) if want == have => {}
            (want, Ok(plaintext)) => failures.push(format!(
                "{}: expected {want:?}, decrypted {} bytes",
                vector.name,
                plaintext.len()
            )),
            (want, Err(have)) => {
                failures.push(format!("{}: expected {want:?}, got {have:?}", vector.name))
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} vectors failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The classes the subset covers, so a vector dropped from the index shows here too.
#[test]
fn the_subset_covers_every_class() {
    let vectors = index();
    let count = |want: Expect| vectors.iter().filter(|v| v.expect == want).count();
    assert_eq!(count(Expect::Success), 19);
    assert_eq!(count(Expect::Fails(AgeError::NoMatch)), 4);
    assert_eq!(count(Expect::Fails(AgeError::Hmac)), 1);
    assert_eq!(count(Expect::Fails(AgeError::Header)), 33);
    assert_eq!(count(Expect::Fails(AgeError::Payload)), 19);
    assert_eq!(count(Expect::Fails(AgeError::Armor)), 26);
}

/// Every `success` vector that is not armored round-trips through our own armor.
#[test]
fn success_vectors_round_trip_through_our_armor() {
    for vector in index() {
        if vector.expect != Expect::Success || vector.armored {
            continue;
        }
        let data = file(&vector.name);
        let armored = aterm_secrets::age::armor(&data);
        assert_eq!(
            aterm_secrets::age::dearmor(armored.as_bytes()).as_deref(),
            Ok(data.as_slice()),
            "{}",
            vector.name
        );
    }
}
