// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn identity(seed: u8) -> Identity {
    let mut secret = [0u8; 32];
    for (i, byte) in secret.iter_mut().enumerate() {
        *byte = seed.wrapping_mul(31).wrapping_add(i as u8 * 7);
    }
    Identity::from_secret_bytes(secret)
}

/// Deterministic filler, so a test's plaintext is not all one byte.
fn plaintext(len: usize) -> Vec<u8> {
    let mut state: u32 = 0x9e37_79b9;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

#[test]
fn round_trips_every_size_to_one_two_and_five_recipients() {
    let identities: Vec<Identity> = (1..=5).map(identity).collect();
    let recipients: Vec<Recipient> = identities.iter().map(Identity::recipient).collect();
    for count in [1usize, 2, 5] {
        for len in [0usize, 1, 65535, 65536, 65537, 200_000] {
            let data = plaintext(len);
            let file = encrypt(&recipients[..count], &data).expect("encrypt");
            for identity in &identities[..count] {
                let opened = decrypt(std::slice::from_ref(identity), &file).expect("decrypt");
                assert_eq!(
                    opened.as_slice(),
                    data.as_slice(),
                    "{count} recipients, {len} bytes"
                );
            }
            if count < 5 {
                assert_eq!(
                    decrypt(&identities[count..], &file).err(),
                    Some(AgeError::NoMatch),
                    "{count} recipients, {len} bytes"
                );
            }
        }
    }
}

#[test]
fn chunking_matches_the_spec() {
    let recipient = identity(1).recipient();
    for (len, chunks) in [
        (0usize, 1usize),
        (1, 1),
        (65536, 1),
        (65537, 2),
        (200_000, 4),
    ] {
        let file = encrypt(&[recipient], &plaintext(len)).expect("encrypt");
        let header_len = parse_header(&file).expect("header").len;
        assert_eq!(
            file.len(),
            header_len + 16 + len + 16 * chunks,
            "{len} bytes"
        );
    }
}

#[test]
fn header_is_well_formed() {
    let recipients = [identity(1).recipient(), identity(2).recipient()];
    let file = encrypt(&recipients, b"x").expect("encrypt");
    let header = parse_header(&file).expect("header");
    assert_eq!(header.stanzas.len(), 2);
    for stanza in &header.stanzas {
        assert_eq!(stanza.kind, "X25519");
        assert_eq!(stanza.args.len(), 1);
        assert_eq!(stanza.args[0].len(), 43);
        assert_eq!(stanza.body.len(), 32);
    }
    let text = std::str::from_utf8(&file[..header.len]).expect("ascii header");
    assert!(
        text.starts_with("age-encryption.org/v1\n-> X25519 "),
        "{text}"
    );
    assert!(text.lines().all(|line| line.len() <= 64), "{text}");
    let again = encrypt(&recipients, b"x").expect("encrypt");
    let again = parse_header(&again).expect("header");
    assert_ne!(header.stanzas[0].args, again.stanzas[0].args);
    assert_ne!(header.mac, again.mac);
}

#[test]
fn wrong_identity_is_no_match_and_no_identity_too() {
    let file = encrypt(&[identity(1).recipient()], b"secret").expect("encrypt");
    assert_eq!(
        decrypt(&[identity(2)], &file).err(),
        Some(AgeError::NoMatch)
    );
    assert_eq!(decrypt(&[], &file).err(), Some(AgeError::NoMatch));
    let opened = decrypt(&[identity(2), identity(1)], &file).expect("second identity");
    assert_eq!(opened.as_slice(), b"secret");
}

#[test]
fn armor_round_trips() {
    let recipient = identity(3).recipient();
    for len in [0usize, 1, 2, 3, 47, 48, 49, 1000, 70_000] {
        let file = encrypt(&[recipient], &plaintext(len)).expect("encrypt");
        let armored = armor(&file);
        assert!(armored.starts_with("-----BEGIN AGE ENCRYPTED FILE-----\n"));
        assert!(armored.ends_with("\n-----END AGE ENCRYPTED FILE-----\n"));
        assert!(
            armored
                .lines()
                .all(|line| line.len() <= 64 || line.starts_with("-----"))
        );
        assert_eq!(dearmor(armored.as_bytes()).expect("dearmor"), file, "{len}");
        let opened = decrypt(&[identity(3)], armored.as_bytes()).expect("decrypt armor");
        assert_eq!(opened.as_slice(), plaintext(len).as_slice());
        let crlf = format!("\n \t\r\n{}\n\n", armored.replace('\n', "\r\n"));
        assert_eq!(dearmor(crlf.as_bytes()).expect("crlf"), file, "{len}");
    }
    for len in [0usize, 1, 2, 3, 48, 96, 100] {
        let bytes = plaintext(len);
        assert_eq!(
            dearmor(armor(&bytes).as_bytes()).expect("dearmor"),
            bytes,
            "{len}"
        );
    }
    assert_eq!(
        armor(b""),
        "-----BEGIN AGE ENCRYPTED FILE-----\n-----END AGE ENCRYPTED FILE-----\n"
    );
}

#[test]
fn armor_whitespace_bounds() {
    let armored = armor(b"age");
    let ok = format!("{}\n{}", " ".repeat(1023), armored);
    assert!(
        dearmor(ok.as_bytes()).is_ok(),
        "1023 spaces and an LF are 1024 bytes"
    );
    let too_much = format!("{}\n{}", " ".repeat(1024), armored);
    assert_eq!(dearmor(too_much.as_bytes()).err(), Some(AgeError::Armor));
    let same_line = format!(" {armored}");
    assert_eq!(dearmor(same_line.as_bytes()).err(), Some(AgeError::Armor));
    let trailing_ok = format!("{}{}", armored, " ".repeat(1023));
    assert!(dearmor(trailing_ok.as_bytes()).is_ok());
    let trailing_too_much = format!("{}{}", armored, " ".repeat(1024));
    assert_eq!(
        dearmor(trailing_too_much.as_bytes()).err(),
        Some(AgeError::Armor)
    );
    assert_eq!(dearmor(b"").err(), Some(AgeError::Armor));
    assert_eq!(
        dearmor(b"-----BEGIN AGE ENCRYPTED FILE-----\n").err(),
        Some(AgeError::Armor),
        "no END line"
    );
}

#[test]
fn identity_and_recipient_spellings() {
    let id = identity(9);
    let text = id.to_bech32();
    let text = text.as_str().expect("ascii");
    assert!(text.starts_with("AGE-SECRET-KEY-1"));
    assert_eq!(text, text.to_ascii_uppercase());
    let parsed = Identity::parse(text).expect("parse");
    assert_eq!(parsed.secret_bytes(), id.secret_bytes());
    assert_eq!(
        Identity::parse(&text.to_ascii_lowercase()).err(),
        Some(AgeError::Key)
    );
    let recipient = id.recipient();
    let spelled = recipient.to_bech32();
    assert!(spelled.starts_with("age1"));
    assert_eq!(spelled.len(), 62);
    assert_eq!(Recipient::parse(&spelled), Ok(recipient));
    assert_eq!(
        Recipient::parse(&spelled.to_ascii_uppercase()),
        Err(AgeError::Key)
    );
    assert_eq!(Recipient::parse(text).err(), Some(AgeError::Key));
    assert_eq!(
        Identity::parse(&spelled.to_ascii_uppercase()).err(),
        Some(AgeError::Key)
    );
    assert_eq!(
        Recipient::parse(&bech32::encode("age", &[1u8; 31])),
        Err(AgeError::Key)
    );
    assert_eq!(
        Recipient::parse(&bech32::encode("age", &[1u8; 33])),
        Err(AgeError::Key)
    );
    let mut short = bech32::encode("age-secret-key-", &[1u8; 31]);
    short.make_ascii_uppercase();
    assert_eq!(Identity::parse(&short).err(), Some(AgeError::Key));
    assert_eq!(Recipient::parse("").err(), Some(AgeError::Key));
    let shown = format!("{id:?}");
    assert!(!shown.contains(&text[16..]), "Debug shows no secret");
    assert_eq!(shown, format!("Identity({spelled})"));
}

#[test]
fn interleaved_parsed_identities_still_open_only_their_own_ciphertext() {
    let identities: Vec<_> = (1..=24).map(identity).collect();
    for id in &identities {
        Identity::parse(id.to_bech32().as_str().unwrap()).unwrap();
    }
    for (index, original) in identities.iter().enumerate() {
        let text = original.to_bech32();
        let parsed = Identity::parse(text.as_str().unwrap()).unwrap();
        assert_eq!(parsed.recipient(), original.recipient());
        assert_eq!(parsed.secret_bytes(), original.secret_bytes());
        let ciphertext = encrypt(&[parsed.recipient()], b"scratch parsed identity").unwrap();
        assert_eq!(
            decrypt(std::slice::from_ref(original), &ciphertext)
                .unwrap()
                .as_slice(),
            b"scratch parsed identity"
        );
        assert!(
            decrypt(
                std::slice::from_ref(&identities[(index + 1) % identities.len()]),
                &ciphertext,
            )
            .is_err()
        );
        assert!(Identity::parse(&text.as_str().unwrap().to_ascii_lowercase()).is_err());
    }
}

/// age.md's example identity (32 bytes of 0x42), spelled at run time so no key-shaped
/// literal sits in the source.
#[test]
fn spec_example_identity_spelling() {
    let identity_text = format!("{}1{}GFPQ4EGAEX", "AGE-SECRET-KEY-", "GFPYYSJZ".repeat(6));
    let parsed = Identity::parse(&identity_text).expect("age.md's example identity");
    assert_eq!(parsed.secret_bytes(), &[0x42u8; 32]);
    let built = Identity::from_secret_bytes([0x42u8; 32]);
    assert_eq!(built.to_bech32().as_str(), Some(identity_text.as_str()));
    assert_eq!(built.recipient(), parsed.recipient());
}

#[test]
fn generated_identities_differ_and_open_their_files() {
    let a = Identity::generate().expect("generate");
    let b = Identity::generate().expect("generate");
    assert_ne!(a.secret_bytes(), b.secret_bytes());
    let file = encrypt(&[a.recipient()], b"hello").expect("encrypt");
    assert_eq!(decrypt(&[a], &file).expect("decrypt").as_slice(), b"hello");
    assert_eq!(decrypt(&[b], &file).err(), Some(AgeError::NoMatch));
}

/// The low-order u-coordinates, made at run time: 0 (order 2), 1 and p - 1 (order 4,
/// one on the curve and one on its twist), the two u-coordinates of points of order 8, and p
/// and p + 1 (0 and 1 again, unreduced); each also with the top bit set, which X25519 ignores.
fn low_order_points() -> Vec<[u8; 32]> {
    let unreduced = |low: u8| {
        let mut u = [0xffu8; 32];
        u[0] = low;
        u[31] = 0x7f;
        u
    };
    let order_eight = |hex: &str| {
        let mut u = [0u8; 32];
        assert!(aterm_codec::hex::decode_into(hex.as_bytes(), &mut u), "hex");
        u
    };
    let mut one = [0u8; 32];
    one[0] = 1;
    let mut points = vec![
        [0u8; 32],
        one,
        unreduced(0xec),
        order_eight("e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800"),
        order_eight("5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157"),
        unreduced(0xed),
        unreduced(0xee),
    ];
    let top_bit: Vec<[u8; 32]> = points
        .iter()
        .map(|u| {
            let mut u = *u;
            u[31] |= 0x80;
            u
        })
        .collect();
    points.extend(top_bit);
    points
}

#[test]
fn a_low_order_recipient_is_refused_by_parse_and_by_encrypt() {
    assert!(encrypt(&[], b"x").is_err(), "no recipient");
    let points = low_order_points();
    assert_eq!(points.len(), 14);
    for point in &points {
        for scalar in [[7u8; 32], [0xa5; 32]] {
            assert_eq!(x25519::x25519(&scalar, point), [0u8; 32], "{point:02x?}");
        }
        let spelled = bech32::encode("age", point);
        assert_eq!(Recipient::parse(&spelled), Err(AgeError::Key), "{spelled}");
        assert!(encrypt(&[Recipient(*point)], b"x").is_err());
        assert!(encrypt(&[identity(1).recipient(), Recipient(*point)], b"x").is_err());
    }
    for seed in 1..=8 {
        let recipient = identity(seed).recipient();
        assert_eq!(Recipient::parse(&recipient.to_bech32()), Ok(recipient));
    }
    for small in [2u8, 3, 9] {
        let mut u = [0u8; 32];
        u[0] = small;
        assert_eq!(
            Recipient::parse(&bech32::encode("age", &u)),
            Ok(Recipient(u))
        );
    }
}

/// `low_order`'s table answers as the ladder it replaced does, X25519 by a clamped scalar
/// being all zero: on the 14 spellings, on each with one bit of its first or last byte
/// flipped, on small coordinates, on the 256 coordinates just below 2^255 (p - 1, p, p + 1
/// and their neighbours) and on real keys.
#[test]
fn low_order_agrees_with_the_ladder() {
    let ladder = |u: &[u8; 32]| x25519::x25519(&[0x55; 32], u) == [0u8; 32];
    let spellings = low_order_points();
    let mut inputs = spellings.clone();
    for point in &spellings {
        for bit in (0..8).chain(248..256) {
            let mut u = *point;
            u[bit / 8] ^= 1 << (bit % 8);
            inputs.push(u);
        }
    }
    for low in 0..=u8::MAX {
        inputs.push(small(low));
        inputs.push(near_2_255(low));
    }
    inputs.extend((1..=64).map(|seed| identity(seed).recipient().0));
    for u in &inputs {
        assert_eq!(low_order(u), ladder(u), "{u:02x?}");
    }
    assert!(spellings.iter().all(low_order));
}

#[test]
fn a_stanza_whose_share_is_low_order_is_a_header_failure() {
    let file = encrypt(&[identity(1).recipient()], b"secret").expect("encrypt");
    let share = parse_header(&file).expect("header").stanzas[0].args[0].to_string();
    let at = file
        .windows(share.len())
        .position(|window| window == share.as_bytes())
        .expect("the share is in the header");
    for point in low_order_points() {
        let forged = b64::encode_nopad(&point);
        assert_eq!(forged.len(), share.len());
        let mut file = file.clone();
        file[at..at + share.len()].copy_from_slice(forged.as_bytes());
        for id in [identity(1), identity(2)] {
            assert_eq!(
                decrypt(std::slice::from_ref(&id), &file).err(),
                Some(AgeError::Header),
                "{point:02x?}"
            );
        }
    }
    assert_eq!(
        decrypt(&[identity(1)], &file)
            .expect("the original opens")
            .as_slice(),
        b"secret"
    );
}

/// A real file with one more stanza spliced in first, to reach the rules no CCTV
/// vector applicable here covers.
fn with_stanza_first(file: &[u8], extra_stanza: &str) -> Vec<u8> {
    let start = VERSION_LINE.len() + 1;
    let mut out = file[..start].to_vec();
    out.extend_from_slice(extra_stanza.as_bytes());
    out.extend_from_slice(&file[start..]);
    out
}

#[test]
fn scrypt_beside_another_stanza_is_a_header_failure() {
    let file = encrypt(&[identity(1).recipient()], b"x").expect("encrypt");
    let mixed = with_stanza_first(&file, "-> scrypt c2FsdHNhbHRzYWx0c2FsdA 10\nAAAA\n");
    assert_eq!(
        decrypt(&[identity(1)], &mixed).err(),
        Some(AgeError::Header)
    );
    let grease = with_stanza_first(&file, "-> grease x y\n\n");
    assert_eq!(decrypt(&[identity(1)], &grease).err(), Some(AgeError::Hmac));
}

#[test]
fn a_header_without_stanzas_or_past_the_bound_is_refused() {
    let no_stanza = format!(
        "age-encryption.org/v1\n--- {}\n{}",
        "A".repeat(43),
        "B".repeat(40)
    );
    assert_eq!(
        decrypt(&[identity(1)], no_stanza.as_bytes()).err(),
        Some(AgeError::Header)
    );
    let mut huge = b"age-encryption.org/v1\n-> grease\n".to_vec();
    while huge.len() <= MAX_HEADER {
        huge.extend_from_slice(&[b'A'; 64]);
        huge.push(b'\n');
    }
    huge.extend_from_slice(b"\n--- AAAA\n");
    assert_eq!(decrypt(&[identity(1)], &huge).err(), Some(AgeError::Header));
}

/// Malformed input never panics: truncations and bit flips of a valid file (binary
/// and armored) give an error, never a plaintext and never a panic.
#[test]
fn truncations_and_bit_flips_only_fail() {
    let data = plaintext(70_000);
    let file =
        encrypt(&[identity(4).recipient(), identity(5).recipient()], &data).expect("encrypt");
    let ids = [identity(4)];
    let header_len = parse_header(&file).expect("header").len;
    let mut cuts: Vec<usize> = (0..header_len + 64).collect();
    cuts.extend((header_len + 64..file.len()).step_by(997));
    cuts.extend(file.len() - 40..file.len());
    for cut in cuts {
        assert!(decrypt(&ids, &file[..cut]).is_err(), "cut at {cut}");
    }
    let small = plaintext(1000);
    let small_file =
        encrypt(&[identity(4).recipient(), identity(5).recipient()], &small).expect("encrypt");
    let armored = armor(&small_file).into_bytes();
    for cut in (0..armored.len() - 1).step_by(7) {
        assert!(
            decrypt(&ids, &armored[..cut]).is_err(),
            "armor cut at {cut}"
        );
    }
    let mut flips: Vec<usize> = (0..header_len + 16).map(|at| at * 8 + at % 8).collect();
    flips.extend(((header_len + 16) * 8..file.len() * 8).step_by(4099));
    flips.push(file.len() * 8 - 1);
    for bit in flips {
        let mut flipped = file.clone();
        flipped[bit / 8] ^= 1 << (bit % 8);
        assert!(decrypt(&ids, &flipped).is_err(), "bit {bit}");
    }
    for bit in (0..armored.len() * 8).step_by(37) {
        let mut flipped = armored.clone();
        flipped[bit / 8] ^= 1 << (bit % 8);
        if let Ok(opened) = decrypt(&ids, &flipped) {
            // Only a flip of armor whitespace that stays whitespace can survive.
            assert_eq!(opened.as_slice(), small.as_slice(), "armor bit {bit}");
        }
    }
    let mut prefixed = b"x".to_vec();
    prefixed.extend_from_slice(&file);
    assert_eq!(decrypt(&ids, &prefixed).err(), Some(AgeError::Armor));
    let mut suffixed = file.clone();
    suffixed.push(0);
    assert_eq!(decrypt(&ids, &suffixed).err(), Some(AgeError::Payload));
    assert_eq!(
        decrypt(&ids, &file).expect("decrypt").as_slice(),
        data.as_slice()
    );
}
