// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn canonical_text_signature_accepts_line_endings_but_not_changed_content() {
    use ring::signature::KeyPair;
    let signer = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let header = [4, 1, 22, 8, 0, 0];
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(b"first\r\nsecond\r\n");
    hash.update(&header);
    hash.update(&[4, 255, 0, 0, 0, 6]);
    let hash = hash.finish();
    let signature = signer.sign(hash.as_ref());
    let mut body = header.to_vec();
    body.extend_from_slice(&[0, 0]);
    body.extend_from_slice(&hash.as_ref()[..2]);
    for part in signature.as_ref().as_chunks::<32>().0 {
        body.extend_from_slice(&256u16.to_be_bytes());
        body.extend_from_slice(part);
    }
    let mut packet = vec![0xc2, body.len() as u8];
    packet.extend_from_slice(&body);
    let armor = format!(
        "-----BEGIN PGP SIGNATURE-----\n\n{}\n-----END PGP SIGNATURE-----\n",
        aterm_codec::base64::encode(&packet).unwrap()
    );
    let keys = [signer.public_key().as_ref().try_into().unwrap()];
    for text in [
        b"first\nsecond\n".as_slice(),
        b"first\r\nsecond\r\n",
        b"first\rsecond\r",
    ] {
        super::verify(text, &armor, &keys).unwrap();
    }
    assert!(super::verify(b"first\nchanged\n", &armor, &keys).is_err());
}

#[test]
fn published_signature_and_altered_token() {
    let keys =
        super::signing_keys(include_str!("../../tests/fixtures/proton-key-v4.asc"), b"").unwrap();
    let sig = include_str!("../../tests/fixtures/proton-signature-v4.asc");
    super::verify(b"hello world", sig, &keys).unwrap();
    assert!(super::verify(b"hello earth", sig, &keys).is_err());
}
