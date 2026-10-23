// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Identities: the digest framing, artifact and identity digests, and their printing.

use aterm_aos::ident::{ARTIFACT_DOMAIN, IDENTITY_DOMAIN};
use aterm_aos::wire::Enc;
use aterm_aos::{ArtifactId, ArtifactKind, Digest, TaskIdentity};
use aterm_digest::Sha256;

use crate::toy::{Variant, code, identity};

#[test]
fn a_digest_is_sha256_over_the_domain_then_each_part_framed_by_its_length() {
    let mut h = Sha256::new();
    h.update(b"dom\0");
    for part in [&b"ab"[..], &b""[..], &b"c"[..]] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part);
    }
    assert_eq!(
        Digest::of(b"dom\0", &[b"ab", b"", b"c"]),
        Digest(h.finalize())
    );
}

#[test]
fn the_length_framing_and_the_domain_keep_digests_apart() {
    assert_ne!(
        Digest::of(b"d", &[b"ab", b"c"]),
        Digest::of(b"d", &[b"a", b"bc"])
    );
    assert_ne!(
        Digest::of(b"d", &[b"abc"]),
        Digest::of(b"d", &[b"ab", b"c"])
    );
    assert_ne!(Digest::of(b"d", &[b""]), Digest::of(b"d", &[]));
    assert_ne!(Digest::of(b"d1", &[b"x"]), Digest::of(b"d2", &[b"x"]));
}

#[test]
fn a_digest_prints_as_lowercase_hex_and_its_short_form_is_its_first_twelve_digits() {
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(37);
    }
    let d = Digest(bytes);
    let hex = d.hex();
    assert_eq!(hex.len(), 64);
    let decoded: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    assert_eq!(decoded, bytes);
    assert!(
        hex.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    );
    assert_eq!(d.short(), hex[..12]);
    assert_eq!(format!("{d}"), hex);
    assert_eq!(format!("{d:?}"), hex);
}

#[test]
fn an_artifact_digest_is_the_framed_digest_of_its_declaration() {
    let a = ArtifactId::new(ArtifactKind::Code, "demo", 3, b"build=1");
    assert_eq!(
        a.digest,
        Digest::of(
            ARTIFACT_DOMAIN,
            &[&[2], b"demo", &3u32.to_le_bytes(), b"build=1"]
        )
    );
    assert_eq!(
        (a.kind, a.name.as_str(), a.version),
        (ArtifactKind::Code, "demo", 3)
    );
}

#[test]
fn an_artifact_digest_changes_with_every_declared_field() {
    let base = ArtifactId::new(ArtifactKind::Code, "demo", 3, b"build=1");
    for other in [
        ArtifactId::new(ArtifactKind::Contract, "demo", 3, b"build=1"),
        ArtifactId::new(ArtifactKind::Policy, "demo", 3, b"build=1"),
        ArtifactId::new(ArtifactKind::Code, "demO", 3, b"build=1"),
        ArtifactId::new(ArtifactKind::Code, "demo", 4, b"build=1"),
        ArtifactId::new(ArtifactKind::Code, "demo", 3, b"build=2"),
    ] {
        assert_ne!(other.digest, base.digest, "{other:?}");
    }
}

#[test]
fn an_identity_digest_covers_every_field_of_every_artifact() {
    let id = identity(Variant::Good);
    let mut e = Enc::new();
    e.identity(&id);
    assert_eq!(id.digest(), Digest::of(IDENTITY_DOMAIN, &[e.as_bytes()]));
    let renamed = |a: &ArtifactId| ArtifactId {
        name: format!("{}-x", a.name),
        ..a.clone()
    };
    let variants = [
        TaskIdentity {
            task: renamed(&id.task),
            ..id.clone()
        },
        TaskIdentity {
            code: renamed(&id.code),
            ..id.clone()
        },
        TaskIdentity {
            contract: renamed(&id.contract),
            ..id.clone()
        },
        TaskIdentity {
            schema: renamed(&id.schema),
            ..id.clone()
        },
        TaskIdentity {
            profile: ArtifactId {
                version: id.profile.version + 1,
                ..id.profile.clone()
            },
            ..id.clone()
        },
    ];
    for v in &variants {
        assert_ne!(v.digest(), id.digest(), "{v:?}");
    }
}

#[test]
fn with_code_changes_the_code_and_nothing_else() {
    let id = identity(Variant::Stranded);
    let good = code(Variant::Good);
    let repaired = id.with_code(aterm_aos::task::Code::id(&good).clone());
    assert_eq!(repaired, identity(Variant::Good));
    assert_eq!(
        (
            &repaired.task,
            &repaired.contract,
            &repaired.schema,
            &repaired.profile
        ),
        (&id.task, &id.contract, &id.schema, &id.profile)
    );
    assert_ne!(repaired.digest(), id.digest());
}
