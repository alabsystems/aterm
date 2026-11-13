// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// BIP 173's valid Bech32 strings.
const VALID: &[&str] = &[
    "A12UEL5L",
    "a12uel5l",
    "an83characterlonghumanreadablepartthatcontainsthenumber1andtheexcludedcharactersbio1tt5tgs",
    "abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw",
    "11qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqc8247j",
    "split1checkupstagehandshakeupstreamerranterredcaperred2y9e3w",
    "?1ezyfcl",
];

/// BIP 173's valid segwit addresses.
const SEGWIT: &[(&str, &str)] = &[
    (
        "BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4",
        "0014751e76e8199196d454941c45d1b3a323f1433bd6",
    ),
    (
        "tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3q0sl5k7",
        "00201863143c14c5166804bd19203356da136c985678cd4d27a1b8c6329604903262",
    ),
    (
        "bc1pw508d6qejxtdg4y5r3zarvary0c5xw7kw508d6qejxtdg4y5r3zarvary0c5xw7k7grplx",
        "5128751e76e8199196d454941c45d1b3a323f1433bd6751e76e8199196d454941c45d1b3a323f1433bd6",
    ),
    ("BC1SW50QA3JX3S", "6002751e"),
    (
        "bc1zw508d6qejxtdg4y5r3zarvaryvg6kdaj",
        "5210751e76e8199196d454941c45d1b3a323",
    ),
    (
        "tb1qqqqqp399et2xygdj5xreqhjjvcmzhxw4aywxecjdzew6hylgvsesrxh6hy",
        "0020000000c4a5cad46221b2a187905e5266362b99d5e91c6ce24d165dab93e86433",
    ),
];

fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

#[test]
fn bip173_valid_strings_decode_and_re_encode() {
    for &text in VALID {
        let (hrp, groups, uppercase) =
            decode_groups(text).unwrap_or_else(|| panic!("valid: {text}"));
        assert_eq!(
            uppercase,
            text.bytes().any(|c| c.is_ascii_uppercase()),
            "{text}"
        );
        assert_eq!(
            encode_groups(&hrp, &groups),
            text.to_ascii_lowercase(),
            "{text}"
        );
        let mut flipped = text.to_ascii_lowercase().into_bytes();
        let last = flipped.len() - 1;
        flipped[last] = if flipped[last] == b'q' { b'p' } else { b'q' };
        let flipped = String::from_utf8(flipped).expect("ascii");
        assert_eq!(decode_groups(&flipped), None, "{flipped}");
    }
}

#[test]
fn bip173_valid_strings_decode_to_bytes() {
    assert_eq!(decode("A12UEL5L").expect("valid").data, Vec::<u8>::new());
    let decoded = decode("abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw").expect("valid");
    assert_eq!(decoded.hrp, "abcdef");
    assert_eq!(
        hex(&decoded.data),
        "00443214c74254b635cf84653a56d7c675be77df"
    );
    assert!(!decoded.uppercase);
    let decoded = decode("A12UEL5L").expect("valid");
    assert_eq!(decoded.hrp, "a");
    assert!(decoded.uppercase);
}

#[test]
fn bip173_segwit_programs_regroup_exactly() {
    for &(address, script) in SEGWIT {
        let (_, groups, _) = decode_groups(address).unwrap_or_else(|| panic!("{address}"));
        let version = groups[0];
        let program = from_groups(&groups[1..]).unwrap_or_else(|| panic!("{address}"));
        let op = if version == 0 { 0 } else { 0x50 + version };
        let mut expected = vec![op, program.len() as u8];
        expected.extend_from_slice(&program);
        assert_eq!(hex(&expected), script, "{address}");
        assert_eq!(to_groups(&program), groups[1..].to_vec(), "{address}");
    }
}

#[test]
fn bip173_invalid_strings_are_refused() {
    for text in [
        "\u{20}1nwldj5",
        "\u{7f}1axkwrx",
        "\u{80}1eym55h",
        "pzry9x0s0muk",
        "1pzry9x0s0muk",
        "x1b4n0q5v",
        "li1dgmt3",
        "de1lg7wt\u{ff}",
        "A1G7SGD8",
        "10a06t8",
        "1qzzfhee",
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t5",
        "tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3q0sL5k7",
    ] {
        assert_eq!(decode_groups(text), None, "{text:?}");
        assert_eq!(decode(text), None, "{text:?}");
    }
}

#[test]
fn no_length_limit() {
    let text = "an84characterslonghumanreadablepartthatcontainsthenumber1andtheexcludedcharactersbio1569pvx";
    assert!(text.len() > 90);
    assert!(decode(text).is_some());
    let long = encode("age", &[0xa5; 200]);
    assert!(long.len() > 300);
    assert_eq!(decode(&long).expect("long").data, vec![0xa5; 200]);
}

#[test]
fn padding_must_be_short_and_zero() {
    let mut groups = to_groups(&[0x75]);
    assert_eq!(groups.len(), 2);
    assert_eq!(
        decode(&encode_groups("age", &groups)).expect("clean").data,
        vec![0x75]
    );
    groups.push(0);
    assert_eq!(
        decode(&encode_groups("age", &groups)),
        None,
        "a spare zero group"
    );
    groups.pop();
    *groups.last_mut().expect("groups") |= 1;
    assert_eq!(
        decode(&encode_groups("age", &groups)),
        None,
        "a set padding bit"
    );
    let groups = to_groups(&[0x75, 0x1e]);
    assert_eq!(groups.len(), 4);
    assert_eq!(
        decode(&encode_groups("age", &groups)).expect("clean").data,
        vec![0x75, 0x1e]
    );
}

#[test]
fn age_length_round_trips() {
    for (hrp, seed) in [("age", 0x11u8), ("age-secret-key-", 0x7e)] {
        for len in [0usize, 1, 31, 32, 33] {
            let data: Vec<u8> = (0..len).map(|i| seed.wrapping_mul(i as u8 + 1)).collect();
            let text = encode(hrp, &data);
            assert_eq!(text, text.to_ascii_lowercase());
            let decoded = decode(&text).expect("round trip");
            assert_eq!(decoded.hrp, hrp);
            assert_eq!(decoded.data, data);
            assert!(!decoded.uppercase);
            let upper = decode(&text.to_ascii_uppercase()).expect("uppercase round trip");
            assert_eq!(upper.hrp, hrp);
            assert_eq!(upper.data, data);
            assert!(upper.uppercase);
        }
    }
    assert_eq!(encode("age", &[0u8; 32]).len(), 3 + 1 + 52 + 6);
    assert_eq!(encode("age-secret-key-", &[0u8; 32]).len(), 15 + 1 + 52 + 6);
}

#[test]
fn mixed_case_is_refused() {
    let text = encode("age", &[9u8; 32]);
    let mut mixed = text.clone().into_bytes();
    let at = mixed
        .iter()
        .rposition(u8::is_ascii_lowercase)
        .expect("a letter");
    mixed[at] = mixed[at].to_ascii_uppercase();
    let mixed = String::from_utf8(mixed).expect("ascii");
    assert_eq!(decode(&mixed), None);
    assert_eq!(
        decode(&format!("AGE{}", &text[3..])),
        None,
        "uppercase HRP only"
    );
}

#[test]
fn characters_outside_the_charset_are_refused() {
    let text = encode("age", &[3u8; 32]);
    for bad in ['b', 'i', 'o', '1', ' '] {
        let mut bytes = text.clone().into_bytes();
        let at = bytes.len() - 10;
        bytes[at] = bad as u8;
        let candidate = String::from_utf8(bytes).expect("ascii");
        assert_eq!(decode(&candidate), None, "{bad:?}");
    }
    assert_eq!(decode(""), None);
    assert_eq!(decode("age1"), None);
}
