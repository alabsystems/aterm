// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use ring::rand::SystemRandom;

use super::testkit::*;
use super::*;

const ANTHROPIC_ASC: &str = include_str!("vendor_direct/fixtures/anthropic-claude-code.asc");
const MANIFEST_280: &[u8] = include_bytes!("vendor_direct/fixtures/claude-2.1.280-manifest.json");
const SIG_280: &[u8] = include_bytes!("vendor_direct/fixtures/claude-2.1.280-manifest.json.sig");
const MANIFEST_278: &[u8] = include_bytes!("vendor_direct/fixtures/claude-2.1.278-manifest.json");
const SIG_278: &[u8] = include_bytes!("vendor_direct/fixtures/claude-2.1.278-manifest.json.sig");

const NOW: u64 = 1_790_035_200;
const SIG_280_CREATED: u32 = 1_790_024_226;
const SIG_278_CREATED: u32 = 1_789_781_019;
const TEST_SIG_CREATED: u32 = 1_780_000_000;
const DOC: &[u8] = b"{\"version\":\"9.9.9\"}\n";

fn test_fingerprint() -> [u8; 20] {
    TEST_KEY.pinned.fingerprint
}

fn verify_test(doc: &[u8], sig: &[u8]) -> Result<VerifiedSignature, PgpError> {
    verify_detached(doc, sig, &TEST_KEY.pinned, NOW)
}

fn verify_anthropic(doc: &[u8], sig: &[u8]) -> Result<VerifiedSignature, PgpError> {
    verify_detached(doc, sig, &ANTHROPIC_CLAUDE_CODE_RELEASE_KEY, NOW)
}

const TEST_OK: Result<VerifiedSignature, PgpError> = Ok(VerifiedSignature {
    created: TEST_SIG_CREATED,
});

fn expires_after(secs: u32) -> Vec<u8> {
    subpacket(SUBPACKET_EXPIRATION, &secs.to_be_bytes())
}

fn good_hashed() -> Vec<u8> {
    [issuer(&test_fingerprint()), created_at(TEST_SIG_CREATED)].concat()
}

struct Spec {
    version: u8,
    sig_type: u8,
    pkalg: u8,
    hash: u8,
    hashed: Vec<u8>,
    unhashed: Vec<u8>,
    trailer_delta: i64,
}

impl Spec {
    fn good() -> Self {
        Self {
            version: 4,
            sig_type: SIG_TYPE_BINARY,
            pkalg: PKALG_RSA,
            hash: HASH_SHA512,
            hashed: good_hashed(),
            unhashed: subpacket(16, &test_fingerprint()[12..]),
            trailer_delta: 0,
        }
    }

    fn hashed(hashed: Vec<u8>) -> Self {
        Self {
            hashed,
            ..Self::good()
        }
    }
}

fn algorithms(
    hash: u8,
) -> (
    &'static digest::Algorithm,
    &'static dyn signature::RsaEncoding,
) {
    if hash == HASH_SHA256 {
        (&digest::SHA256, &signature::RSA_PKCS1_SHA256)
    } else {
        (&digest::SHA512, &signature::RSA_PKCS1_SHA512)
    }
}

fn sign_body(doc: &[u8], spec: &Spec) -> Vec<u8> {
    let mut prefix = vec![spec.version, spec.sig_type, spec.pkalg, spec.hash];
    prefix.extend_from_slice(&u16::try_from(spec.hashed.len()).unwrap().to_be_bytes());
    prefix.extend_from_slice(&spec.hashed);
    let count = i64::try_from(prefix.len()).unwrap() + spec.trailer_delta;
    let message = [
        doc,
        &prefix,
        &[4, 0xFF],
        &u32::try_from(count).unwrap().to_be_bytes(),
    ]
    .concat();
    let (alg, encoding) = algorithms(spec.hash);
    let mut raw = vec![0; TEST_KEY.pair.public().modulus_len()];
    TEST_KEY
        .pair
        .sign(encoding, &SystemRandom::new(), &message, &mut raw)
        .unwrap();
    let mut body = prefix;
    body.extend_from_slice(&u16::try_from(spec.unhashed.len()).unwrap().to_be_bytes());
    body.extend_from_slice(&spec.unhashed);
    body.extend_from_slice(&digest::digest(alg, &message).as_ref()[..2]);
    body.extend(mpi(&raw));
    body
}

fn packet(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0xC0 | tag];
    let n = body.len();
    if n < 192 {
        out.push(u8::try_from(n).unwrap());
    } else if n < 8384 {
        let m = n - 192;
        out.push(u8::try_from((m >> 8) + 192).unwrap());
        out.push(u8::try_from(m & 0xFF).unwrap());
    } else {
        out.push(0xFF);
        out.extend(u32::try_from(n).unwrap().to_be_bytes());
    }
    out.extend_from_slice(body);
    out
}

fn old_packet(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0x80 | (tag << 2) | 1];
    out.extend(u16::try_from(body.len()).unwrap().to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn signed(doc: &[u8], spec: &Spec) -> Vec<u8> {
    packet(TAG_SIGNATURE, &sign_body(doc, spec))
}

fn refusal(spec: &Spec) -> PgpError {
    verify_test(DOC, &signed(DOC, spec)).unwrap_err()
}

fn layout(body: &[u8]) -> (usize, usize) {
    let hashed_end = 6 + usize::from(u16::from_be_bytes([body[4], body[5]]));
    let unhashed_len = usize::from(u16::from_be_bytes([body[hashed_end], body[hashed_end + 1]]));
    (hashed_end, hashed_end + 2 + unhashed_len)
}

fn verifier_message(body: &[u8], doc: &[u8]) -> Vec<u8> {
    let (hashed_end, _) = layout(body);
    [
        doc,
        &body[..hashed_end],
        &[4, 0xFF],
        &u32::try_from(hashed_end).unwrap().to_be_bytes(),
    ]
    .concat()
}

fn with_left16_for(body: &[u8], doc: &[u8]) -> Vec<u8> {
    let (_, left16) = layout(body);
    let (alg, _) = algorithms(body[3]);
    let digest = digest::digest(alg, &verifier_message(body, doc));
    let mut out = body.to_vec();
    out[left16..left16 + 2].copy_from_slice(&digest.as_ref()[..2]);
    out
}

fn with_unhashed(body: &[u8], unhashed: &[u8]) -> Vec<u8> {
    let (hashed_end, left16) = layout(body);
    [
        &body[..hashed_end],
        &u16::try_from(unhashed.len()).unwrap().to_be_bytes(),
        unhashed,
        &body[left16..],
    ]
    .concat()
}

fn with_mpi_field(body: &[u8], field: &[u8]) -> Vec<u8> {
    let (_, left16) = layout(body);
    [&body[..left16 + 2], field].concat()
}

fn foreign_packet() -> Vec<u8> {
    dearmor(SIG_280).unwrap()
}

fn sig_280_text() -> &'static str {
    std::str::from_utf8(SIG_280).unwrap()
}

#[test]
fn real_manifests_verify_under_the_compiled_key() {
    for (release, manifest, sig, created) in [
        ("2.1.280", MANIFEST_280, SIG_280, SIG_280_CREATED),
        ("2.1.278", MANIFEST_278, SIG_278, SIG_278_CREATED),
    ] {
        assert_eq!(
            verify_anthropic(manifest, sig),
            Ok(VerifiedSignature { created }),
            "{release}"
        );
    }
}

#[test]
fn a_real_signature_binds_only_its_own_release() {
    assert_eq!(
        verify_anthropic(MANIFEST_278, SIG_280),
        Err(PgpError::DigestPrefixMismatch)
    );
    let mut flipped = MANIFEST_280.to_vec();
    flipped[30] ^= 0x01;
    assert_eq!(
        verify_anthropic(&flipped, SIG_280),
        Err(PgpError::DigestPrefixMismatch)
    );
    let binary = foreign_packet();
    assert_eq!(
        verify_anthropic(
            &flipped,
            &[&binary[..3], &with_left16_for(&binary[3..], &flipped)].concat()
        ),
        Err(PgpError::BadSignature)
    );
}

#[test]
fn a_real_signature_verifies_in_binary_form() {
    let binary = foreign_packet();
    assert_eq!(
        binary[0], 0x89,
        "GnuPG writes an old-format two-octet header"
    );
    assert_eq!(
        verify_anthropic(MANIFEST_280, &binary),
        Ok(VerifiedSignature {
            created: SIG_280_CREATED
        })
    );
}

#[test]
fn compiled_key_is_the_committed_asc() {
    let block = ANTHROPIC_ASC.replace("PUBLIC KEY BLOCK", "SIGNATURE");
    let binary = dearmor(block.as_bytes()).unwrap();
    assert_eq!(
        binary[0], 0x99,
        "an old-format public-key packet, two-octet length"
    );
    let len = usize::from(u16::from_be_bytes([binary[1], binary[2]]));
    let body = &binary[3..3 + len];
    let mut r = Reader(body);
    assert_eq!(r.u8(), Some(4));
    let created = r.be32().unwrap();
    assert_eq!(r.u8(), Some(PKALG_RSA));
    let n_bits = r.be16().unwrap();
    let n = r.take(usize::from(n_bits).div_ceil(8)).unwrap();
    let e_bits = r.be16().unwrap();
    let e = r.take(usize::from(e_bits).div_ceil(8)).unwrap();
    assert!(r.is_empty());

    let key = ANTHROPIC_CLAUDE_CODE_RELEASE_KEY;
    assert_eq!(n_bits, 4096);
    assert_eq!(n, key.n);
    assert_eq!(e, key.e);
    assert_eq!(created, key.created);
    assert_eq!(v4_fingerprint(body), key.fingerprint);
    let hex: String = key.fingerprint.iter().map(|b| format!("{b:02X}")).collect();
    assert_eq!(hex, "31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE");
    assert_eq!(
        key_packet_body(created, n, e),
        body,
        "the test-key encoder agrees"
    );
}

#[test]
fn armor_crc_is_never_consulted() {
    let text = sig_280_text();
    let crc = text.lines().find(|l| l.starts_with('=')).unwrap();
    assert_ne!(crc, "=AAAA");
    let expected = Ok(VerifiedSignature {
        created: SIG_280_CREATED,
    });
    let wrong = text.replace(crc, "=AAAA");
    assert_eq!(verify_anthropic(MANIFEST_280, wrong.as_bytes()), expected);
    let absent = text.replace(&format!("{crc}\n"), "");
    assert_eq!(verify_anthropic(MANIFEST_280, absent.as_bytes()), expected);
}

#[test]
fn armor_headers_crlf_and_surrounding_blank_lines_are_tolerated() {
    let text = sig_280_text();
    let expected = Ok(VerifiedSignature {
        created: SIG_280_CREATED,
    });
    let with_headers = text.replacen(
        "-----\n\n",
        "-----\nVersion: GnuPG v2\nComment: a note\n\n",
        1,
    );
    assert_ne!(with_headers, text);
    assert_eq!(
        verify_anthropic(MANIFEST_280, with_headers.as_bytes()),
        expected
    );
    let crlf = text.replace('\n', "\r\n");
    assert_eq!(verify_anthropic(MANIFEST_280, crlf.as_bytes()), expected);
    let padded = format!("\n\n{text}\n\n");
    assert_eq!(verify_anthropic(MANIFEST_280, padded.as_bytes()), expected);
}

#[test]
fn armor_that_is_not_exactly_one_signature_block_is_refused() {
    let text = sig_280_text();
    let cases = [
        ("empty", String::new()),
        ("two blocks", format!("{text}{text}")),
        ("prose before", format!("signed:\n{text}")),
        ("prose after", format!("{text}thanks\n")),
        ("no END", text.replace("-----END PGP SIGNATURE-----", "")),
        ("another kind", text.replace("PGP SIGNATURE", "PGP MESSAGE")),
        (
            "a nested BEGIN",
            text.replacen("\n\n", "\n\n-----BEGIN PGP SIGNATURE-----\n", 1),
        ),
        ("unpadded base64", text.replacen("iQIz", "iQI", 1)),
        ("a space in the body", text.replacen("iQIz", "iQ Iz", 1)),
    ];
    for (what, case) in cases {
        assert_ne!(case, text, "{what}");
        assert_eq!(
            verify_anthropic(MANIFEST_280, case.as_bytes()),
            Err(PgpError::Armor),
            "{what}"
        );
    }
}

#[test]
fn every_packet_length_encoding_is_read() {
    let body = sign_body(DOC, &Spec::good());
    let len = u32::try_from(body.len()).unwrap().to_be_bytes();
    let new_two = packet(TAG_SIGNATURE, &body);
    assert_eq!(new_two[1] & 0xE0, 0xC0, "a two-octet new-format length");
    let framings = [
        ("new, two octets", new_two.clone()),
        (
            "new, five octets",
            [&[0xC0 | TAG_SIGNATURE, 0xFF][..], &len, &body].concat(),
        ),
        ("old, two octets", old_packet(TAG_SIGNATURE, &body)),
        (
            "old, four octets",
            [&[0x80 | (TAG_SIGNATURE << 2) | 2][..], &len, &body].concat(),
        ),
        (
            "old, one octet (a signature never fits one, so a marker carries it)",
            [&[0x80 | (TAG_MARKER << 2), 3][..], b"PGP", &new_two].concat(),
        ),
    ];
    for (what, sig) in framings {
        assert_eq!(verify_test(DOC, &sig), TEST_OK, "{what}");
    }
    let truncated_headers: [&[u8]; 5] = [
        &[0xC0 | TAG_SIGNATURE, 0xC0],
        &[0xC0 | TAG_SIGNATURE, 0xFF, 0, 0],
        &[0x80 | (TAG_MARKER << 2)],
        &[0x80 | (TAG_SIGNATURE << 2) | 1, 0],
        &[0x80 | (TAG_SIGNATURE << 2) | 2, 0, 0],
    ];
    for header in truncated_headers {
        assert_eq!(
            verify_test(DOC, header),
            Err(PgpError::Truncated),
            "{header:02x?}"
        );
    }
}

#[test]
fn sha256_signature_verifies() {
    let spec = Spec {
        hash: HASH_SHA256,
        ..Spec::good()
    };
    assert_eq!(verify_test(DOC, &signed(DOC, &spec)), TEST_OK);
}

#[test]
fn a_short_mpi_is_left_padded_to_the_modulus() {
    let modulus_len = TEST_KEY.pinned.n.len();
    let (doc, body) = (0..4096)
        .map(|i| {
            let doc = format!("short-mpi-{i}");
            let body = sign_body(doc.as_bytes(), &Spec::good());
            (doc, body)
        })
        .find(|(_, body)| body.len() - layout(body).1 - 4 < modulus_len)
        .expect("a short signature among 4096 documents");
    let (_, left16) = layout(&body);
    let mpi_bytes = &body[left16 + 4..];
    assert_eq!(
        verify_test(doc.as_bytes(), &packet(TAG_SIGNATURE, &body)),
        TEST_OK
    );
    let bits = u16::try_from(8 * modulus_len).unwrap().to_be_bytes();
    let pad = vec![0; modulus_len - mpi_bytes.len()];
    let fixed_length = [&bits[..], &pad, mpi_bytes].concat();
    assert_eq!(
        verify_test(
            doc.as_bytes(),
            &packet(TAG_SIGNATURE, &with_mpi_field(&body, &fixed_length))
        ),
        TEST_OK,
        "a fixed-length encoding (overstated count, leading zero byte) is v4-legal"
    );
    let unpadded = RsaPublicKeyComponents {
        n: TEST_KEY.pinned.n,
        e: TEST_KEY.pinned.e,
    }
    .verify(
        &signature::RSA_PKCS1_2048_8192_SHA512,
        &verifier_message(&body, doc.as_bytes()),
        mpi_bytes,
    );
    assert!(unpadded.is_err(), "the padding is load-bearing");
}

#[test]
fn a_flipped_document_byte_is_refused() {
    let body = sign_body(DOC, &Spec::good());
    let mut doc = DOC.to_vec();
    doc[3] ^= 0x01;
    assert_eq!(
        verify_test(&doc, &packet(TAG_SIGNATURE, &body)),
        Err(PgpError::DigestPrefixMismatch)
    );
    assert_eq!(
        verify_test(&doc, &packet(TAG_SIGNATURE, &with_left16_for(&body, &doc))),
        Err(PgpError::BadSignature)
    );
}

#[test]
fn a_flipped_hashed_subpacket_byte_is_refused() {
    let spec = Spec::hashed([good_hashed(), subpacket(101, b"release notes")].concat());
    let body = sign_body(DOC, &spec);
    assert_eq!(verify_test(DOC, &packet(TAG_SIGNATURE, &body)), TEST_OK);
    let (hashed_end, _) = layout(&body);
    let mut flipped = body.clone();
    flipped[hashed_end - 1] ^= 0x01;
    assert_eq!(
        verify_test(DOC, &packet(TAG_SIGNATURE, &flipped)),
        Err(PgpError::DigestPrefixMismatch)
    );
    assert_eq!(
        verify_test(DOC, &packet(TAG_SIGNATURE, &with_left16_for(&flipped, DOC))),
        Err(PgpError::BadSignature)
    );
}

#[test]
fn the_unhashed_area_is_never_read() {
    let body = sign_body(DOC, &Spec::good());
    let mutations = [
        Vec::new(),
        vec![0xFF; 40],
        subpacket(0x80 | 101, b"critical and unknown"),
        issuer(&[0x11; 20]),
        [created_at(0), created_at(u32::MAX)].concat(),
        expires_after(1),
    ];
    for unhashed in mutations {
        assert_eq!(
            verify_test(
                DOC,
                &packet(TAG_SIGNATURE, &with_unhashed(&body, &unhashed))
            ),
            TEST_OK,
            "{unhashed:02x?}"
        );
    }
}

#[test]
fn a_signature_by_another_key_is_refused() {
    let sig = signed(DOC, &Spec::good());
    assert_eq!(verify_anthropic(DOC, &sig), Err(PgpError::IssuerMismatch));
    assert_eq!(
        verify_detached(MANIFEST_280, SIG_280, &TEST_KEY.pinned, NOW),
        Err(PgpError::IssuerMismatch)
    );
    let impostor = PinnedRsaKey {
        n: ANTHROPIC_CLAUDE_CODE_RELEASE_KEY.n,
        e: ANTHROPIC_CLAUDE_CODE_RELEASE_KEY.e,
        ..TEST_KEY.pinned
    };
    assert_eq!(
        verify_detached(DOC, &sig, &impostor, NOW),
        Err(PgpError::BadSignature),
        "the modulus, not just the fingerprint, decides"
    );
}

#[test]
fn unsupported_header_fields_are_refused() {
    let cases = [
        (
            Spec {
                sig_type: 0x01,
                ..Spec::good()
            },
            PgpError::UnsupportedSignatureType(0x01),
        ),
        (
            Spec {
                version: 5,
                ..Spec::good()
            },
            PgpError::UnsupportedVersion(5),
        ),
        (
            Spec {
                version: 6,
                ..Spec::good()
            },
            PgpError::UnsupportedVersion(6),
        ),
        (
            Spec {
                pkalg: 3,
                ..Spec::good()
            },
            PgpError::UnsupportedPublicKeyAlgorithm(3),
        ),
        (
            Spec {
                hash: 2,
                ..Spec::good()
            },
            PgpError::UnsupportedHash(2),
        ),
    ];
    for (spec, expected) in cases {
        assert_eq!(refusal(&spec), expected);
    }
}

#[test]
fn a_v3_signature_is_refused() {
    let mut body = vec![3, 5, SIG_TYPE_BINARY];
    body.extend_from_slice(&TEST_SIG_CREATED.to_be_bytes());
    body.extend_from_slice(&test_fingerprint()[12..]);
    body.extend_from_slice(&[PKALG_RSA, HASH_SHA512, 0, 0]);
    body.extend(mpi(&[0x42; 512]));
    assert_eq!(
        verify_test(DOC, &packet(TAG_SIGNATURE, &body)),
        Err(PgpError::UnsupportedVersion(3))
    );
}

#[test]
fn exactly_one_creation_time_within_the_key_and_clock_bounds() {
    let fpr = test_fingerprint();
    let skew_edge = u32::try_from(NOW + FUTURE_SKEW_SECS).unwrap();
    let cases = [
        (issuer(&fpr), Err(PgpError::MissingCreationTime)),
        (
            [
                issuer(&fpr),
                created_at(TEST_SIG_CREATED),
                created_at(TEST_SIG_CREATED),
            ]
            .concat(),
            Err(PgpError::DuplicateSubpacket(2)),
        ),
        (
            [issuer(&fpr), created_at(TEST_KEY_CREATED - 1)].concat(),
            Err(PgpError::CreatedBeforeKey),
        ),
        (
            [issuer(&fpr), created_at(TEST_KEY_CREATED)].concat(),
            Ok(VerifiedSignature {
                created: TEST_KEY_CREATED,
            }),
        ),
        (
            [issuer(&fpr), created_at(skew_edge + 1)].concat(),
            Err(PgpError::CreatedInFuture),
        ),
        (
            [issuer(&fpr), created_at(skew_edge)].concat(),
            Ok(VerifiedSignature { created: skew_edge }),
        ),
    ];
    for (hashed, expected) in cases {
        assert_eq!(
            verify_test(DOC, &signed(DOC, &Spec::hashed(hashed))),
            expected
        );
    }
    let only_unhashed = Spec {
        hashed: issuer(&fpr),
        unhashed: created_at(TEST_SIG_CREATED),
        ..Spec::good()
    };
    assert_eq!(refusal(&only_unhashed), PgpError::MissingCreationTime);
}

#[test]
fn every_expiration_is_honoured() {
    let lapse = u32::try_from(NOW - u64::from(TEST_SIG_CREATED)).unwrap();
    let cases = [
        (expires_after(lapse), Err(PgpError::Expired)),
        (expires_after(lapse + 1), TEST_OK),
        (expires_after(0), TEST_OK),
        (
            [expires_after(lapse + 100), expires_after(lapse)].concat(),
            Err(PgpError::Expired),
        ),
        (
            [expires_after(0), expires_after(lapse)].concat(),
            Err(PgpError::Expired),
        ),
    ];
    for (extra, expected) in cases {
        let spec = Spec::hashed([good_hashed(), extra].concat());
        assert_eq!(verify_test(DOC, &signed(DOC, &spec)), expected);
    }
}

#[test]
fn unknown_critical_subpackets_are_refused() {
    let fpr = test_fingerprint();
    let cases = [
        (
            [good_hashed(), subpacket(0x80 | 101, b"x")].concat(),
            Err(PgpError::UnknownCriticalSubpacket(101)),
        ),
        (
            [good_hashed(), subpacket(0x80 | 16, &fpr[12..])].concat(),
            Err(PgpError::UnknownCriticalSubpacket(16)),
        ),
        ([good_hashed(), subpacket(101, b"x")].concat(), TEST_OK),
        (
            [
                issuer(&fpr),
                subpacket(
                    0x80 | SUBPACKET_CREATION_TIME,
                    &TEST_SIG_CREATED.to_be_bytes(),
                ),
            ]
            .concat(),
            TEST_OK,
        ),
    ];
    for (hashed, expected) in cases {
        assert_eq!(
            verify_test(DOC, &signed(DOC, &Spec::hashed(hashed))),
            expected
        );
    }
}

#[test]
fn subpacket_lengths_in_all_three_encodings() {
    let two_octet = {
        let len = 300 - 192;
        let mut sp = vec![
            u8::try_from((len >> 8) + 192).unwrap(),
            u8::try_from(len & 0xFF).unwrap(),
            101,
        ];
        sp.extend_from_slice(&[0x5A; 299]);
        sp
    };
    let five_octet = [
        &[0xFF, 0, 0, 0, 5, SUBPACKET_CREATION_TIME][..],
        &TEST_SIG_CREATED.to_be_bytes(),
    ]
    .concat();
    let hashed = [issuer(&test_fingerprint()), five_octet, two_octet].concat();
    assert_eq!(
        verify_test(DOC, &signed(DOC, &Spec::hashed(hashed))),
        TEST_OK
    );
}

#[test]
fn malformed_hashed_subpackets_are_refused() {
    let fpr = test_fingerprint();
    let cases = [
        [good_hashed(), vec![0x00]].concat(),
        [good_hashed(), vec![10, 101, 1, 2]].concat(),
        [good_hashed(), vec![0xC0]].concat(),
        [good_hashed(), vec![0xFF, 0, 0]].concat(),
        [issuer(&fpr), subpacket(SUBPACKET_CREATION_TIME, &[1, 2, 3])].concat(),
        [good_hashed(), subpacket(SUBPACKET_EXPIRATION, &[0; 5])].concat(),
    ];
    for hashed in cases {
        assert_eq!(
            refusal(&Spec::hashed(hashed.clone())),
            PgpError::BadSubpacket,
            "{hashed:02x?}"
        );
    }
}

#[test]
fn the_issuer_fingerprint_must_name_the_pinned_key() {
    let fpr = test_fingerprint();
    let created = created_at(TEST_SIG_CREATED);
    let v5_style = subpacket(SUBPACKET_ISSUER_FINGERPRINT, &[&[5][..], &fpr].concat());
    let short = subpacket(
        SUBPACKET_ISSUER_FINGERPRINT,
        &[&[4][..], &fpr[..19]].concat(),
    );
    let cases = [
        (created.clone(), PgpError::MissingIssuer),
        (
            [issuer(&[0x11; 20]), created.clone()].concat(),
            PgpError::IssuerMismatch,
        ),
        (
            [v5_style, created.clone()].concat(),
            PgpError::IssuerMismatch,
        ),
        ([short, created.clone()].concat(), PgpError::IssuerMismatch),
        (
            [issuer(&fpr), issuer(&fpr), created.clone()].concat(),
            PgpError::DuplicateSubpacket(33),
        ),
    ];
    for (hashed, expected) in cases {
        assert_eq!(refusal(&Spec::hashed(hashed)), expected);
    }
    let only_unhashed = Spec {
        hashed: created,
        unhashed: issuer(&fpr),
        ..Spec::good()
    };
    assert_eq!(refusal(&only_unhashed), PgpError::MissingIssuer);
}

#[test]
fn the_hashed_length_trailer_is_exact() {
    for delta in [-1, 1] {
        let body = sign_body(
            DOC,
            &Spec {
                trailer_delta: delta,
                ..Spec::good()
            },
        );
        assert_eq!(
            verify_test(DOC, &packet(TAG_SIGNATURE, &body)),
            Err(PgpError::DigestPrefixMismatch),
            "{delta}"
        );
        assert_eq!(
            verify_test(DOC, &packet(TAG_SIGNATURE, &with_left16_for(&body, DOC))),
            Err(PgpError::BadSignature),
            "{delta}"
        );
    }
}

#[test]
fn partial_and_indeterminate_lengths_are_refused() {
    let body = sign_body(DOC, &Spec::good());
    let partial = [&[0xC0 | TAG_SIGNATURE, 0xE9][..], &body].concat();
    assert_eq!(verify_test(DOC, &partial), Err(PgpError::PartialLength));
    let indeterminate = [&[0x80 | (TAG_SIGNATURE << 2) | 3][..], &body].concat();
    assert_eq!(
        verify_test(DOC, &indeterminate),
        Err(PgpError::IndeterminateLength)
    );
}

#[test]
fn leftover_and_missing_bytes_are_refused() {
    let body = sign_body(DOC, &Spec::good());
    let sig = packet(TAG_SIGNATURE, &body);
    let cases = [
        ([&sig[..], &[0x00, 0x01]].concat(), PgpError::TrailingBytes),
        (
            packet(TAG_SIGNATURE, &[&body[..], &[0x00]].concat()),
            PgpError::TrailingBytes,
        ),
        (
            [&sig[..], &[0xC0 | TAG_SIGNATURE]].concat(),
            PgpError::Truncated,
        ),
        (sig[..sig.len() - 1].to_vec(), PgpError::Truncated),
        (
            packet(TAG_SIGNATURE, &body[..body.len() - 1]),
            PgpError::Truncated,
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(verify_test(DOC, &input), Err(expected));
    }
}

#[test]
fn only_signature_and_marker_packets_are_accepted() {
    let sig = signed(DOC, &Spec::good());
    let marker = packet(TAG_MARKER, b"PGP");
    assert_eq!(verify_test(DOC, &[&marker[..], &sig].concat()), TEST_OK);
    assert_eq!(verify_test(DOC, &marker), Err(PgpError::NoSignature));
    let literal = packet(11, b"b\x00\x00\x00\x00\x00hi");
    assert_eq!(
        verify_test(DOC, &[&literal[..], &sig].concat()),
        Err(PgpError::UnexpectedPacket(11))
    );
    let key = packet(
        6,
        &key_packet_body(TEST_KEY_CREATED, TEST_KEY.pinned.n, TEST_KEY.pinned.e),
    );
    assert_eq!(
        verify_test(DOC, &[&sig[..], &key].concat()),
        Err(PgpError::UnexpectedPacket(6))
    );
}

#[test]
fn one_verifying_signature_among_several_is_enough() {
    let ours = signed(DOC, &Spec::good());
    let foreign = foreign_packet();
    assert_eq!(verify_test(DOC, &[&foreign[..], &ours].concat()), TEST_OK);
    assert_eq!(verify_test(DOC, &[&ours[..], &foreign].concat()), TEST_OK);
    let eight = [foreign.repeat(MAX_SIGNATURES - 1), ours.clone()].concat();
    assert_eq!(verify_test(DOC, &eight), TEST_OK);
    let nine = [foreign.repeat(MAX_SIGNATURES), ours].concat();
    assert_eq!(verify_test(DOC, &nine), Err(PgpError::TooManySignatures));
}

#[test]
fn the_furthest_refusal_is_the_one_reported() {
    let mut body = sign_body(DOC, &Spec::good());
    *body.last_mut().unwrap() ^= 0x01;
    let broken = packet(TAG_SIGNATURE, &body);
    let foreign = foreign_packet();
    assert_eq!(
        verify_test(DOC, &[&foreign[..], &broken].concat()),
        Err(PgpError::BadSignature)
    );
    assert_eq!(
        verify_test(DOC, &[&broken[..], &foreign].concat()),
        Err(PgpError::BadSignature)
    );
}

#[test]
fn a_packet_claiming_the_pinned_issuer_outranks_a_time_refusal() {
    let lapse = u32::try_from(NOW - u64::from(TEST_SIG_CREATED)).unwrap();
    let expired = signed(
        DOC,
        &Spec::hashed([good_hashed(), expires_after(lapse)].concat()),
    );
    let foreign = foreign_packet();
    assert_eq!(
        verify_test(DOC, &[&foreign[..], &expired].concat()),
        Err(PgpError::Expired),
        "another issuer never masks a time refusal"
    );
    let claims_ours = packet(
        TAG_SIGNATURE,
        &with_mpi_field(&sign_body(DOC, &Spec::good()), &mpi(&[0x42; 512])),
    );
    assert_eq!(verify_test(DOC, &claims_ours), Err(PgpError::BadSignature));
    for sig in [
        [&expired[..], &claims_ours].concat(),
        [&claims_ours[..], &expired].concat(),
    ] {
        assert_eq!(
            verify_test(DOC, &sig),
            Err(PgpError::BadSignature),
            "the report is unauthenticated"
        );
    }
}

#[test]
fn the_mpi_must_fit_its_bit_count_and_the_modulus() {
    let body = sign_body(DOC, &Spec::good());
    let cases = [
        [&[0x00, 0x00][..]].concat(),
        [&[0x0F, 0xF9][..], &[0xFF; 512]].concat(),
        [&[0x10, 0x08][..], &[0x01; 513]].concat(),
    ];
    for field in cases {
        assert_eq!(
            verify_test(DOC, &packet(TAG_SIGNATURE, &with_mpi_field(&body, &field))),
            Err(PgpError::BadMpi),
            "{:02x?}",
            &field[..2]
        );
    }
    let fits = [&[0x10, 0x00][..], &[0x00], &[0x01; 511]].concat();
    assert_eq!(
        verify_test(DOC, &packet(TAG_SIGNATURE, &with_mpi_field(&body, &fits))),
        Err(PgpError::BadSignature),
        "a leading zero byte inside the bit count is v4-legal"
    );
}

#[test]
fn oversized_input_is_refused_before_parsing() {
    let big = vec![b'\n'; MAX_SIGNATURE_LEN + 1];
    assert_eq!(verify_test(DOC, &big), Err(PgpError::TooLarge));
    let at_cap = vec![b'\n'; MAX_SIGNATURE_LEN];
    assert_eq!(verify_test(DOC, &at_cap), Err(PgpError::Armor));
}
