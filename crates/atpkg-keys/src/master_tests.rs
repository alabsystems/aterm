// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const SYNTHETIC: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";

fn refused(input: &str) -> PhraseError {
    parse_master(input).expect_err("this input must be refused")
}

#[test]
fn exactly_fifty_two_characters_is_accepted_and_case_folds() {
    assert_eq!(SYNTHETIC.len(), MASTER_PHRASE_LEN, "fixture length");
    let phrase = parse_master(SYNTHETIC).expect("52 characters must parse");
    assert_eq!(phrase.as_str(), SYNTHETIC);
    let upper = parse_master(&SYNTHETIC.to_uppercase()).expect("case is folded");
    assert_eq!(
        phrase.seed().pubkey_b64().unwrap(),
        upper.seed().pubkey_b64().unwrap()
    );
}

#[test]
fn o_i_and_l_are_read_as_the_characters_they_resemble() {
    let mut misread = String::from(SYNTHETIC);
    misread = misread.replacen('0', "o", 1);
    misread = misread.replacen('1', "i", 2);
    misread = misread.replacen('1', "l", 1);
    assert_ne!(misread, SYNTHETIC, "the fixture must actually differ");
    assert_eq!(
        parse_master(&misread).unwrap().seed().pubkey_b64().unwrap(),
        parse_master(SYNTHETIC)
            .unwrap()
            .seed()
            .pubkey_b64()
            .unwrap(),
        "a misread spelling must name the same master"
    );
}

#[test]
fn fifty_one_and_fifty_three_characters_are_refused_with_the_count() {
    let short = &SYNTHETIC[..51];
    assert_eq!(
        refused(short),
        PhraseError::Length { got: 51 },
        "51 characters must not be accepted"
    );
    assert!(refused(short).message().contains("51"));

    let mut long = String::from(SYNTHETIC);
    long.push('a');
    assert_eq!(refused(&long), PhraseError::Length { got: 53 });
    assert!(refused(&long).message().contains("53"));

    let flood = SYNTHETIC.repeat(4);
    assert_eq!(refused(&flood), PhraseError::Length { got: 208 });
    assert_eq!(refused(""), PhraseError::Length { got: 0 });
}

#[test]
fn a_bad_character_is_refused_by_position_and_never_echoed() {
    let mut bad = String::from(SYNTHETIC);
    bad.replace_range(36..37, "u");
    assert_eq!(
        refused(&bad),
        PhraseError::BadChar { position: 37 },
        "the 37th character (1-based) is the bad one"
    );
    let msg = refused(&bad).message();
    assert!(msg.contains("character 37"), "{msg}");
    assert!(!msg.contains(&bad[30..40]), "{msg}");

    let mut first = String::from(SYNTHETIC);
    first.replace_range(0..1, "!");
    assert_eq!(refused(&first), PhraseError::BadChar { position: 1 });
    let mut last = String::from(SYNTHETIC);
    last.replace_range(51..52, "u");
    assert_eq!(refused(&last), PhraseError::BadChar { position: 52 });
}

#[test]
fn a_bad_character_is_reported_even_when_the_length_is_also_wrong() {
    let mut bad = String::from(&SYNTHETIC[..40]);
    bad.replace_range(9..10, "u");
    assert_eq!(refused(&bad), PhraseError::BadChar { position: 10 });
}

#[test]
fn separators_are_stripped_and_positions_count_significant_characters() {
    let grouped = "0123 4567 89ab cdef ghjk mnpq rstv-wxyz\n\
                   0123 4567 89ab cdef ghj0";
    assert_eq!(
        parse_master(grouped).unwrap().as_str(),
        SYNTHETIC,
        "grouping must not change the phrase"
    );
    let mut bad = String::from(grouped);
    bad.replace_range(5..6, "u");
    assert_eq!(refused(&bad), PhraseError::BadChar { position: 5 });
}

#[test]
fn generate_produces_a_fresh_fifty_two_character_phrase() {
    let a = generate_master().unwrap();
    assert_eq!(a.as_str().len(), MASTER_PHRASE_LEN);
    assert!(a.as_str().bytes().all(|c| ALPHABET.contains(&c)));
    assert!(
        a.as_str().bytes().all(|c| !c.is_ascii_uppercase()),
        "generated phrases are lowercase so the paper has one canonical spelling"
    );
    assert_eq!(encode_phrase(&a.seed().bytes).as_str(), a.as_str());
    assert_eq!(parse_master(a.as_str()).unwrap().as_str(), a.as_str());
}

#[test]
fn a_phrase_decodes_to_exactly_the_seed_it_spells() {
    for raw in [
        [0x00; 32],
        [0xFF; 32],
        [0x01; 32],
        *b"0123456789abcdefghijklmnopqrstuv",
    ] {
        let phrase = encode_phrase(&raw);
        assert!(
            matches!(phrase.chars[51], b'0' | b'g'),
            "padding bits are zero"
        );
        let spelled = Ed25519KeyPair::from_seed_unchecked(&raw).unwrap();
        assert_eq!(
            phrase.seed().pubkey_b64().unwrap(),
            aterm_codec::base64::encode(spelled.public_key().as_ref()).unwrap()
        );
    }
    assert_eq!(
        parse_master(SYNTHETIC)
            .unwrap()
            .seed()
            .pubkey_b64()
            .unwrap(),
        "+K4Suo6CHtd/Se3P/nIfZ/FpgUX0kUMJafiLDrxMius="
    );
}

#[test]
fn the_padding_bits_of_the_last_character_are_masked() {
    let base = parse_master(SYNTHETIC)
        .unwrap()
        .seed()
        .pubkey_b64()
        .unwrap();
    let mut pad = String::from(SYNTHETIC);
    pad.replace_range(51..52, "7");
    assert_eq!(
        parse_master(&pad).unwrap().seed().pubkey_b64().unwrap(),
        base
    );
    let mut sig = String::from(SYNTHETIC);
    sig.replace_range(51..52, "g");
    assert_ne!(
        parse_master(&sig).unwrap().seed().pubkey_b64().unwrap(),
        base
    );
}

#[test]
fn the_fingerprint_matches_on_a_correct_transcription_and_differs_otherwise() {
    let phrase = parse_master(SYNTHETIC).unwrap();
    let fp = phrase.seed().fingerprint().unwrap();
    assert_eq!(fp.len(), 9, "8 hex characters and one hyphen: {fp}");
    assert_eq!(&fp[4..5], "-", "{fp}");

    let typed_back =
        parse_master("0123 4567 89ab cdef ghjk mnpq rstv wxyz 0123 4567 89ab cdef ghj0").unwrap();
    assert_eq!(typed_back.seed().fingerprint().unwrap(), fp);

    let mut slip = String::from(SYNTHETIC);
    slip.replace_range(17..18, "7");
    assert_ne!(&slip, SYNTHETIC, "the fixture must actually differ");
    assert_ne!(
        parse_master(&slip).unwrap().seed().fingerprint().unwrap(),
        fp,
        "a single wrong character must change the fingerprint"
    );
}

#[test]
fn the_fingerprint_is_taken_over_the_public_key() {
    let seed = parse_master(SYNTHETIC).unwrap().seed();
    let kp = seed.keypair().unwrap();
    assert_eq!(
        seed.fingerprint().unwrap(),
        fingerprint(kp.public_key().as_ref())
    );
    let raw_seed_fp = fingerprint(&seed.bytes);
    assert_ne!(seed.fingerprint().unwrap(), raw_seed_fp);
}

#[test]
fn debug_never_prints_secret_material() {
    let phrase = parse_master(SYNTHETIC).unwrap();
    let seed = phrase.seed();
    let p = format!("{phrase:?}");
    let s = format!("{seed:?}");
    assert_eq!(p, "MasterPhrase(<redacted>)");
    assert_eq!(s, "MasterSeed(<redacted>)");
    assert!(!p.contains("0123"), "{p}");
    assert!(!s.contains("0123"), "{s}");
}

#[test]
fn scrub_zeroes_the_buffer() {
    let mut buf = *b"0123456789abcdef";
    assert!(buf.iter().any(|b| *b != 0), "the fixture must start dirty");
    scrub(&mut buf);
    assert!(buf.iter().all(|b| *b == 0));
}

#[test]
fn b32_encode_decode_and_format_phrase_line_preserve_invariants() {
    for raw in [
        [0x00; 32],
        [0xFF; 32],
        [0x01; 32],
        *b"0123456789abcdefghijklmnopqrstuv",
    ] {
        let encoded = b32_encode(&raw);
        assert!(encoded.iter().all(|c| ALPHABET.contains(c)));
        assert!(matches!(encoded[51], b'0' | b'g'));
        assert_eq!(b32_decode(&encoded), raw);
        let line = format_phrase_line(&encoded);
        assert_eq!(line.len(), 65);
        assert_eq!(line[64], b'\n');
        for (i, &b) in line[..64].iter().enumerate() {
            if i % 5 == 4 {
                assert_eq!(b, b' ');
            } else {
                assert!(ALPHABET.contains(&b));
            }
        }
    }
}
