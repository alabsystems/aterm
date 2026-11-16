// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const RECEIPT: &[u8] = b"schema = 1\nversion = \"0.200.0\"\n";
const TODAY: &str = "2026-11-15";

fn key() -> Vec<u8> {
    generate().expect("a key")
}

fn pins(entries: &[(&str, &str, &str)]) -> String {
    entries
        .iter()
        .map(|(id, public, not_after)| {
            format!(
                "[[builder]]\nid = \"{id}\"\npublic_key = \"{public}\"\nnot_after = \"{not_after}\"\n"
            )
        })
        .collect()
}

fn pinned(id: &str, pkcs8: &[u8], not_after: &str) -> Builders {
    let public = public_key(pkcs8).expect("public key");
    Builders::parse(&pins(&[(id, &public, not_after)])).expect("pins")
}

#[test]
fn the_committed_pins_file_reads_and_admits_no_key_it_does_not_pin() {
    let file = aterm_tempfile::manifest_dir()
        .join("..")
        .join("..")
        .join(pull::BUILDERS_FILE);
    let text = std::fs::read_to_string(&file).expect("the pins file");
    let builders = Builders::parse(&text).expect("it reads");
    // A key made here is pinned nowhere: under a pinned id it is a bad signature, under an
    // id the file does not name an unknown builder. Never admitted.
    let pkcs8 = key();
    for pinned in builders.all() {
        let attestation = sign(&pkcs8, &pinned.id, RECEIPT).expect("sign");
        let refused = verify(&builders, TODAY, RECEIPT, &attestation).expect_err("admitted");
        assert!(
            matches!(refused.fault, Fault::BadSignature | Fault::Expired),
            "{refused}"
        );
    }
    let attestation = sign(&pkcs8, "no-such-builder", RECEIPT).expect("sign");
    let refused = verify(&builders, TODAY, RECEIPT, &attestation).expect_err("admitted");
    assert_eq!(refused.fault, Fault::UnknownBuilder, "{refused}");
}

#[test]
fn a_pinned_builders_attestation_is_admitted() {
    let pkcs8 = key();
    let builders = pinned("m6-hp-32gb-1tb", &pkcs8, "2027-11-15");
    let attestation = sign(&pkcs8, "m6-hp-32gb-1tb", RECEIPT).expect("sign");
    let text = attestation.to_toml().expect("toml");
    assert_eq!(Attestation::parse(&text), Ok(attestation.clone()));
    let attested = verify(&builders, TODAY, RECEIPT, &attestation).expect("admitted");
    assert_eq!(attested.builder, "m6-hp-32gb-1tb");
    assert_eq!(attested.public_key, public_key(&pkcs8).expect("public"));
    // Its last day is admitted; the next is not.
    assert!(verify(&builders, "2027-11-15", RECEIPT, &attestation).is_ok());
    let expired = verify(&builders, "2027-11-16", RECEIPT, &attestation).expect_err("expired");
    assert_eq!(expired.fault, Fault::Expired, "{expired}");
}

#[test]
fn an_attestation_is_refused_for_what_it_does_not_prove() {
    let pkcs8 = key();
    let other = key();
    let builders = pinned("m6-hp-32gb-1tb", &pkcs8, "2027-11-15");
    let good = sign(&pkcs8, "m6-hp-32gb-1tb", RECEIPT).expect("sign");
    // A receipt changed by one byte.
    let mut tampered = RECEIPT.to_vec();
    tampered[0] ^= 1;
    let refused = verify(&builders, TODAY, &tampered, &good).expect_err("tampered");
    assert_eq!(refused.fault, Fault::BadSignature, "{refused}");
    assert!(refused.to_string().contains("BAD SIGNATURE"), "{refused}");
    // Another key signing under the pinned builder's name.
    let forged = sign(&other, "m6-hp-32gb-1tb", RECEIPT).expect("sign");
    assert_eq!(
        verify(&builders, TODAY, RECEIPT, &forged).map_err(|r| r.fault),
        Err(Fault::BadSignature)
    );
    // The pinned key's signature over the receipt without the domain: another protocol's.
    let raw = ring::signature::Ed25519KeyPair::from_pkcs8(&pkcs8)
        .expect("pair")
        .sign(RECEIPT);
    let undomained = Attestation {
        builder: "m6-hp-32gb-1tb".to_string(),
        signature: aterm_codec::hex::encode_lower(raw.as_ref()),
    };
    assert_eq!(
        verify(&builders, TODAY, RECEIPT, &undomained).map_err(|r| r.fault),
        Err(Fault::BadSignature)
    );
    // A builder the file does not pin.
    let stranger = sign(&pkcs8, "another-host", RECEIPT).expect("sign");
    assert_eq!(
        verify(&builders, TODAY, RECEIPT, &stranger).map_err(|r| r.fault),
        Err(Fault::UnknownBuilder)
    );
    // A signature that does not decode.
    let mut short = good.clone();
    short.signature.pop();
    assert_eq!(
        verify(&builders, TODAY, RECEIPT, &short).map_err(|r| r.fault),
        Err(Fault::Malformed)
    );
    // Controls: the genuine attestation is admitted.
    assert!(verify(&builders, TODAY, RECEIPT, &good).is_ok());
}

#[test]
fn an_attestation_file_of_another_shape_is_refused() {
    let good = format!("builder = \"m6\"\nsignature = \"{}\"\n", "a".repeat(128));
    assert!(Attestation::parse(&good).is_ok());
    for bad in [
        String::new(),
        format!("builder = \"m6\"\nsignature = \"{}\"\n", "A".repeat(128)),
        format!("builder = \"m6\"\nsignature = \"{}\"\n", "a".repeat(127)),
        format!("builder = \"M6\"\nsignature = \"{}\"\n", "a".repeat(128)),
        format!("{good}extra = 1\n"),
        format!("builder = \"m6\"\nsignature = \"{}\"\n", "g".repeat(128)),
    ] {
        let refused = Attestation::parse(&bad).expect_err(&bad);
        assert_eq!(refused.fault, Fault::Malformed, "{bad:?}");
    }
}

#[test]
fn the_pins_file_refuses_what_is_not_one_pin_per_builder() {
    let a = "1".repeat(64);
    let b = "2".repeat(64);
    let good = pins(&[("m6-a", &a, "2027-01-31"), ("m6-b", &b, "2028-02-29")]);
    assert_eq!(Builders::parse(&good).expect("pins").all().len(), 2);
    assert!(Builders::parse("").expect("empty").all().is_empty());
    let cases: [(&str, String); 10] = [
        (
            "a duplicate id",
            pins(&[("m6-a", &a, "2027-01-31"), ("m6-a", &b, "2027-01-31")]),
        ),
        (
            "a duplicate key",
            pins(&[("m6-a", &a, "2027-01-31"), ("m6-b", &a, "2027-01-31")]),
        ),
        (
            "an upper-case key",
            pins(&[("m6-a", &"A".repeat(64), "2027-01-31")]),
        ),
        ("a short key", pins(&[("m6-a", &a[1..], "2027-01-31")])),
        (
            "a non-hex key",
            pins(&[("m6-a", &"z".repeat(64), "2027-01-31")]),
        ),
        ("no such day", pins(&[("m6-a", &a, "2027-02-29")])),
        ("no such month", pins(&[("m6-a", &a, "2027-13-01")])),
        ("a short year", pins(&[("m6-a", &a, "27-01-31")])),
        ("an id with a space", pins(&[("m6 a", &a, "2027-01-31")])),
        (
            "an unknown key",
            format!("{good}[[builder]]\nid = \"m6-c\"\nkey = \"x\"\n"),
        ),
    ];
    for (what, text) in cases {
        let refused = Builders::parse(&text).expect_err(what);
        assert_eq!(refused.fault, Fault::Malformed, "{what}");
    }
}

#[test]
fn a_builder_serves_only_with_its_pinned_key_unexpired() {
    let pkcs8 = key();
    let public = public_key(&pkcs8).expect("public");
    let builders = pinned("m6", &pkcs8, "2027-01-31");
    assert!(builders.admits("m6", &public, TODAY).is_ok());
    let other = public_key(&key()).expect("public");
    assert_eq!(
        builders.admits("m6", &other, TODAY).map_err(|r| r.fault),
        Err(Fault::UnknownBuilder)
    );
    assert_eq!(
        builders.admits("m7", &public, TODAY).map_err(|r| r.fault),
        Err(Fault::UnknownBuilder)
    );
    assert_eq!(
        builders
            .admits("m6", &public, "2027-02-01")
            .map_err(|r| r.fault),
        Err(Fault::Expired)
    );
}

#[test]
fn builder_ids_are_host_names() {
    for good in ["m6-hp-32gb-1tb", "a", "0host", "x.y_z"] {
        assert_eq!(check_id(good), Ok(()), "{good}");
    }
    for bad in ["", "M6", "-m6", ".m6", "m6 hp", "m6/hp", &"a".repeat(65)] {
        assert!(check_id(bad).is_err(), "{bad:?}");
    }
    let env = |name: &str| vec![("COMPUTERNAME".to_string(), name.to_string())];
    assert_eq!(
        default_builder(&env("M6-HP-32GB-1TB")),
        Ok("m6-hp-32gb-1tb".to_string())
    );
    assert!(default_builder(&env("")).is_err());
    assert!(default_builder(&env("A B")).is_err());
    assert!(default_builder(&[]).is_err());
}

#[test]
fn the_key_is_made_once_and_kept() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let path = scratch.path().join("windows-builder").join("ed25519.pk8");
    assert_eq!(init_key(&path), Ok(true));
    let first = std::fs::read(&path).expect("key");
    assert_eq!(read_key(&path), Ok(first.clone()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o077, 0, "the key is readable by others: {mode:o}");
    }
    // A second init keeps it.
    assert_eq!(init_key(&path), Ok(false));
    assert_eq!(std::fs::read(&path).expect("key"), first);
    // Nothing is left beside it.
    let names: Vec<_> = std::fs::read_dir(path.parent().expect("dir"))
        .expect("dir")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    let public = public_key(&first).expect("public");
    assert!(aterm_codec::hex::is_lower(&public, 64));
    // Negative controls: a file there that is no key is refused and never replaced.
    let junk = scratch.path().join("junk.pk8");
    std::fs::write(&junk, b"not a key").expect("write");
    assert!(read_key(&junk).is_err());
    assert!(init_key(&junk).is_err());
    assert_eq!(std::fs::read(&junk).expect("junk"), b"not a key");
    let big = scratch.path().join("big.pk8");
    std::fs::write(&big, vec![0_u8; KEY_MAX_BYTES as usize + 1]).expect("write");
    assert!(read_key(&big).is_err());
    let env = vec![(
        "USERPROFILE".to_string(),
        scratch.path().display().to_string(),
    )];
    assert_eq!(
        default_key(&env),
        Some(
            scratch
                .path()
                .join(".aterm")
                .join("windows-builder")
                .join("ed25519.pk8")
        )
    );
    assert_eq!(
        default_key(&[("USERPROFILE".to_string(), "relative".to_string())]),
        None
    );
}

#[test]
fn an_attestation_is_written_beside_its_receipt_and_read_back() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let dir = scratch.path();
    let asset = "aterm-0.200.0-windows-x86_64.zip";
    std::fs::write(dir.join(crate::handoff::receipt_name(asset)), RECEIPT).expect("receipt");
    let pkcs8 = key();
    let path = write_beside(dir, asset, &pkcs8, "m6").expect("signed");
    assert_eq!(path, dir.join(pull::attestation_name(asset)));
    let builders = pinned("m6", &pkcs8, "2027-01-31");
    assert_eq!(
        verify_beside(dir, asset, &builders, TODAY).map(|a| a.builder),
        Ok("m6".to_string())
    );
    // It is never written over.
    assert!(write_beside(dir, asset, &pkcs8, "m6").is_err());
    // The receipt changed under it: refused as a bad signature.
    std::fs::write(
        dir.join(crate::handoff::receipt_name(asset)),
        b"schema = 1\nversion = \"0.200.1\"\n",
    )
    .expect("receipt");
    assert_eq!(
        verify_beside(dir, asset, &builders, TODAY).map_err(|r| r.fault),
        Err(Fault::BadSignature)
    );
    // No attestation at all: malformed, never admitted.
    std::fs::remove_file(&path).expect("remove");
    assert_eq!(
        verify_beside(dir, asset, &builders, TODAY).map_err(|r| r.fault),
        Err(Fault::Malformed)
    );
}

#[test]
fn the_message_is_the_domain_then_the_receipts_bytes() {
    assert_eq!(
        message(b"abc"),
        b"aterm-windows-handoff-attestation/1\nabc".to_vec()
    );
}
