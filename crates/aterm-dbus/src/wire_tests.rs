// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn de(hex: &str, sig: &str) -> Result<Vec<Value>> {
    let bytes = aterm_codec::hex::decode_vec(hex.as_bytes()).unwrap();
    decode(
        &bytes,
        &sig::parse_signature(sig).unwrap(),
        Endian::Little,
        0,
    )
}

fn malformed(r: Result<Vec<Value>>) -> &'static str {
    match r {
        Err(Error::Malformed { what, .. }) => what,
        other => panic!("expected a malformed refusal, got {other:?}"),
    }
}

#[test]
fn pad_to_lands_on_the_next_boundary() {
    for align in [1, 2, 4, 8] {
        for pos in 0..32 {
            let p = pad_to(pos, align);
            assert!(p >= pos && p - pos < align && p.is_multiple_of(align));
        }
    }
}

#[test]
fn the_reader_refuses_what_the_specification_forbids() {
    assert_eq!(malformed(de("02000000", "b")), "boolean other than 0 or 1");
    assert_eq!(malformed(de("0100000041", "s")), "value runs past the end");
    assert_eq!(
        malformed(de("010000004101", "s")),
        "string not NUL-terminated"
    );
    assert_eq!(
        malformed(de("020000004100", "s")),
        "value runs past the end"
    );
    assert_eq!(malformed(de("02000000410000", "s")), "NUL inside a string");
    assert_eq!(malformed(de("01000000ff00", "s")), "string is not UTF-8");
    assert_eq!(malformed(de("02000000612f00", "o")), "invalid object path");
    assert_eq!(
        malformed(de("01280000", "g")),
        "signature ends inside a type"
    );
    assert_eq!(malformed(de("0101000000", "yu")), "nonzero padding");
    assert_eq!(
        malformed(de("0000000001", "u")),
        "bytes left after the last value"
    );
    assert_eq!(malformed(de("ffffff7f", "ay")), "array longer than 64 MiB");
    assert_eq!(
        malformed(de("05000000010203", "ay")),
        "array runs past the end"
    );
    assert_eq!(
        malformed(de("02000000010000000200000000000000", "au")),
        "value runs past the end"
    );
    assert!(matches!(
        de("0168000000000000", "v"),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(
        de("0000000000000000", "a{sv}").unwrap()[0]
            .as_dict()
            .map(|d| d.entries().len()),
        Some(0)
    );
    assert_eq!(
        malformed(de("00000000", "a{sv}")),
        "value runs past the end"
    );
    assert_eq!(
        malformed(de("0000000001000000", "a{sv}")),
        "nonzero padding"
    );
}

#[test]
fn nesting_is_bounded_across_variants() {
    let nested = |n: usize| {
        let mut v = Value::Byte(7);
        for _ in 0..n {
            v = Value::variant(v);
        }
        v
    };
    let bytes = encode(&[nested(64)], Endian::Little, 0).unwrap();
    assert!(decode(&bytes, &[Type::Variant], Endian::Little, 0).is_ok());
    assert!(matches!(
        encode(&[nested(65)], Endian::Little, 0),
        Err(Error::Invalid(_))
    ));
    let mut deep = Vec::new();
    for _ in 0..65 {
        deep.extend_from_slice(b"\x01v\x00");
    }
    deep.extend_from_slice(b"\x01y\x00\x07");
    assert_eq!(
        malformed(decode(&deep, &[Type::Variant], Endian::Little, 0)),
        "containers nested deeper than 64"
    );
}

#[test]
fn the_writer_refuses_what_the_specification_forbids() {
    assert!(matches!(
        encode(&[Value::Str("a\0b".into())], Endian::Little, 0),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        encode(&[Value::Struct(vec![])], Endian::Little, 0),
        Err(Error::Invalid(_))
    ));
    let nested = |n: usize| {
        let mut v = Value::Byte(7);
        for _ in 0..n {
            v = Value::array(v.type_of(), vec![v]).unwrap();
        }
        v
    };
    assert!(encode(&[nested(32)], Endian::Little, 0).is_ok());
    assert!(matches!(
        encode(&[nested(33)], Endian::Little, 0),
        Err(Error::Invalid("arrays nested deeper than 32"))
    ));
    assert!(matches!(
        encode(&[Value::Byte(1)], Endian::Little, MAX_MESSAGE + 1),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        encode(
            &[Value::Byte(1), Value::U16(2)],
            Endian::Little,
            MAX_MESSAGE
        ),
        Err(Error::Invalid("offset past the longest message"))
    ));
}

#[test]
fn caller_types_are_held_to_the_signature_rules() {
    let mut r = Reader::new(&[1, 0, 0, 0, 7], Endian::Little, 0);
    let mut calls = 0;
    let nothing = |_: &mut Reader<'_>| {
        calls += 1;
        if calls > 1 {
            Err(Error::Invalid("looped"))
        } else {
            Ok(())
        }
    };
    match r.array(1, nothing) {
        Err(Error::Malformed { what, .. }) => assert_eq!(what, "array element of no bytes"),
        other => panic!("an element of no bytes: {other:?}"),
    }
    let empty = Type::structure([]);
    let mut r = Reader::new(&[0; 8], Endian::Little, 0);
    assert!(matches!(
        r.value(&empty),
        Err(Error::Invalid("empty struct"))
    ));
    let mut bytes = vec![8, 0, 0, 0];
    bytes.extend_from_slice(&[0; 12]);
    let refused = |types: &[Type]| decode(&bytes, types, Endian::Little, 0).unwrap_err();
    assert!(matches!(
        refused(&[Type::array(empty)]),
        Error::Invalid("empty struct")
    ));
    let one = decode(
        &bytes,
        &[Type::array(Type::structure([Type::U64]))],
        Endian::Little,
        0,
    );
    assert_eq!(one.unwrap()[0].as_array().map(|a| a.items().len()), Some(1));
    assert!(matches!(
        refused(&[Type::dict(Type::Variant, Type::U64)]),
        Error::Invalid("dict key is not a basic type")
    ));
    let mut deep = Type::Byte;
    for _ in 0..33 {
        deep = Type::array(deep);
    }
    assert!(matches!(
        refused(&[deep]),
        Error::Invalid("arrays nested deeper than 32")
    ));
    assert!(matches!(
        decode(&[7], &[Type::Byte], Endian::Little, MAX_MESSAGE + 1),
        Err(Error::Invalid(_))
    ));
    assert!(decode(&[7], &[Type::Byte], Endian::Little, MAX_MESSAGE).is_ok());
}

#[test]
fn a_signature_value_may_name_a_descriptor() {
    for (hex, sig) in [("016800", "h"), ("05617b68737d00", "a{hs}")] {
        assert_eq!(
            de(hex, "g").unwrap(),
            vec![Value::Sig(Signature::new(sig).unwrap())]
        );
    }
    assert_eq!(
        malformed(de("01280000", "g")),
        "signature ends inside a type"
    );
    assert!(matches!(
        de("0168000000000000", "v"),
        Err(Error::Unsupported(_))
    ));
}

fn variant_dictionary(variants: usize) -> Value {
    let mut value = Value::dict(
        Type::Byte,
        Type::Byte,
        vec![(Value::Byte(1), Value::Byte(2))],
    )
    .unwrap();
    for _ in 0..variants {
        value = Value::variant(value);
    }
    value
}

fn variant_dictionary_bytes(variants: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 1..variants {
        bytes.extend_from_slice(b"\x01v\x00");
    }
    bytes.extend_from_slice(b"\x05a{yy}\x00");
    while bytes.len() % 4 != 0 {
        bytes.push(0);
    }
    bytes.extend_from_slice(&2u32.to_le_bytes());
    while bytes.len() % 8 != 0 {
        bytes.push(0);
    }
    bytes.extend_from_slice(&[1, 2]);
    bytes
}

#[test]
fn dictionary_entries_count_towards_the_encoder_depth_limit() {
    assert_eq!(
        encode(&[variant_dictionary(62)], Endian::Little, 0).unwrap(),
        variant_dictionary_bytes(62)
    );
    assert!(matches!(
        encode(&[variant_dictionary(63)], Endian::Little, 0),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn dictionary_entries_count_towards_the_decoder_depth_limit() {
    assert_eq!(
        decode(
            &variant_dictionary_bytes(62),
            &[Type::Variant],
            Endian::Little,
            0
        )
        .unwrap(),
        vec![variant_dictionary(62)]
    );
    assert!(matches!(
        decode(
            &variant_dictionary_bytes(63),
            &[Type::Variant],
            Endian::Little,
            0
        ),
        Err(Error::Malformed { .. })
    ));
}
