// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn s(text: &str) -> Vec<u8> {
    let mut v = (text.len() as u32).to_le_bytes().to_vec();
    v.extend_from_slice(text.as_bytes());
    v
}

/// A function record's body, as the macro encodes the leading part.
fn head(kind: u8, class: &str, js: &str, symbol: &str, docs: &str, recv: u8, n: u8) -> Vec<u8> {
    let mut v = vec![kind];
    for t in [class, js, symbol, docs] {
        v.extend(s(t));
    }
    v.push(recv);
    v.push(n);
    v
}

#[test]
fn composite_descriptions_nest_their_parts() {
    let opt = TypeDesc::wrap(tag::OPTION, TypeDesc::tag(tag::U32));
    assert_eq!(opt.as_bytes(), &[tag::OPTION, tag::U32]);
    let res = TypeDesc::pair(
        tag::RESULT,
        TypeDesc::class("LinkHit"),
        TypeDesc::tag(tag::STRING),
    );
    assert_eq!(
        decode_ty(res.as_bytes()),
        Ok(Ty::Result(
            Box::new(Ty::Class("LinkHit".into())),
            Box::new(Ty::String)
        ))
    );
    assert_eq!(
        decode_ty(TypeDesc::wrap(tag::VEC, TypeDesc::tag(tag::F64)).as_bytes()),
        Ok(Ty::Vec(Box::new(Ty::F64)))
    );
}

/// A record assembled at compile time decodes to what went in, and two
/// concatenated records (what the linker makes of the section) decode in
/// order.
#[test]
fn records_round_trip_through_the_section() {
    const PARTS: &[Part] = &[
        Part::Bytes(&[kind::CLASS, 4, 0, 0, 0]),
        Part::Bytes(b"Item"),
        Part::Bytes(&[4, 0, 0, 0]),
        Part::Bytes(b"Item"),
        Part::Bytes(&[9, 0, 0, 0]),
        Part::Bytes(b"item_free"),
        Part::Bytes(&[0, 0, 0, 0]),
    ];
    const CLASS: [u8; record_len(PARTS)] = record(PARTS);
    let mut section = CLASS.to_vec();
    let mut body = head(
        kind::METHOD,
        "Item",
        "len",
        "item_len",
        " Doc.",
        receiver::SHARED,
        1,
    );
    body.extend(s("extra"));
    body.extend(TypeDesc::wrap(tag::OPTION, TypeDesc::tag(tag::U64)).as_bytes());
    body.extend(TypeDesc::tag(tag::USIZE).as_bytes());
    let mut fun = MAGIC.to_vec();
    fun.push(VERSION);
    fun.extend((body.len() as u32).to_le_bytes());
    fun.extend(&body);
    section.extend(&fun);

    let records = decode(&section).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].kind, kind::CLASS);
    assert_eq!(records[0].class, "Item");
    assert_eq!(records[0].symbol, "item_free");
    assert_eq!(
        records[1],
        Record {
            kind: kind::METHOD,
            class: "Item".into(),
            js_name: "len".into(),
            symbol: "item_len".into(),
            docs: " Doc.".into(),
            receiver: receiver::SHARED,
            params: vec![("extra".into(), Ty::Option(Box::new(Ty::U64)))],
            ret: Ty::Usize,
        }
    );
}

/// Foreign, mismatched-version, truncated, and over-deep sections are refused.
#[test]
fn foreign_old_and_cut_sections_are_refused() {
    let body = head(kind::FUNCTION, "", "f", "f", "", receiver::NONE, 0);
    let mut rec = MAGIC.to_vec();
    rec.push(VERSION);
    rec.extend(((body.len() + 1) as u32).to_le_bytes());
    rec.extend(&body);
    rec.push(tag::UNIT);
    assert!(decode(&rec).is_ok());

    let mut other = rec.clone();
    other[4] = VERSION + 1;
    assert_eq!(
        decode(&other),
        Err(DecodeError::Version {
            at: 0,
            found: VERSION + 1
        })
    );
    let mut foreign = rec.clone();
    foreign[0] = b'x';
    assert_eq!(decode(&foreign), Err(DecodeError::Magic { at: 0 }));
    assert_eq!(
        decode(&rec[..rec.len() - 1]),
        Err(DecodeError::Truncated { at: 0 })
    );
    assert_eq!(
        decode(&[
            MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION, 0xff, 0xff, 0xff, 0xff
        ]),
        Err(DecodeError::Truncated { at: 0 })
    );
    let mut bad_tag = rec.clone();
    *bad_tag.last_mut().unwrap() = 0x7f;
    assert!(matches!(
        decode(&bad_tag),
        Err(DecodeError::Malformed { .. })
    ));

    let mut deep = vec![tag::OPTION; 1 << 20];
    deep.push(tag::U8);
    assert!(decode_ty(&deep).unwrap_err().contains("nests deeper"));
    let mut real = vec![tag::OPTION; TypeDesc::CAP - 1];
    real.push(tag::U8);
    assert!(decode_ty(&real).is_ok(), "as deep as a description can be");
}

#[test]
fn class_tags_are_never_the_freed_mark() {
    assert_ne!(class_tag("AtermTerminal"), 0);
    assert_ne!(class_tag("AtermTerminal"), class_tag("LinkHit"));
    assert_eq!(class_tag(""), 0x811c_9dc5);
}

/// The lowering table the generator checks signatures against.
#[test]
fn the_lowering_table_covers_each_shape_once() {
    use ValType::*;
    assert_eq!(Ty::U64.param_words(), Some([I64, I32]));
    assert_eq!(Ty::Str.param_words(), Some([I32, I32]));
    assert_eq!(
        Ty::Option(Box::new(Ty::F64)).param_words(),
        Some([I32, F64])
    );
    assert_eq!(
        Ty::Option(Box::new(Ty::Option(Box::new(Ty::U8)))).param_words(),
        None
    );
    assert_eq!(Ty::Class("C".into()).param_words(), None);
    assert_eq!(
        Ty::JsObject("HTMLCanvasElement".into()).param_words(),
        Some([I32, I32])
    );
    assert_eq!(Ty::JsObject("HTMLCanvasElement".into()).ret_word(), None);
    assert_eq!(
        decode_ty(TypeDesc::js_object("OffscreenCanvas").as_bytes()),
        Ok(Ty::JsObject("OffscreenCanvas".into()))
    );
    assert_eq!(Ty::Unit.ret_word(), Some(None));
    assert_eq!(Ty::Str.ret_word(), None);
    let res = |ok: Ty| Ty::Result(Box::new(ok), Box::new(Ty::String));
    assert_eq!(res(Ty::Unit).ret_word(), Some(None));
    assert_eq!(
        res(Ty::Option(Box::new(Ty::U64))).ret_word(),
        Some(Some(I64))
    );
    assert!(res(Ty::Option(Box::new(Ty::U64))).ret_uses_slot());
    assert_eq!(
        Ty::Result(Box::new(Ty::Unit), Box::new(Ty::U32)).ret_word(),
        None
    );
    assert_eq!(Ty::Vec(Box::new(Ty::Bool)).ret_word(), None);
}
