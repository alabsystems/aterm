// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-dbus's marshalling against zvariant 5.12.0's, recorded in
//! tests/recorded/zvariant-5.12.0-values.tsv: every value encodes to the
//! recorded bytes and the recorded bytes decode to it, in both byte orders and
//! at the recorded offsets.
//!
//! Negative controls, so a match cannot be vacuous: every case is recorded and
//! every recording has a case; a neighbouring value misses every recording;
//! and no single-bit flip of a recording decodes to the recorded value. The
//! decoder accepts only canonical bytes, so a flipped recording either fails
//! or re-encodes to exactly the flipped bytes.

mod support;

use std::collections::BTreeMap;

use aterm_dbus::{Endian, Signature, Type, Value, wire};
use support::*;

fn cases() -> BTreeMap<&'static str, Value> {
    let sig = |s: &str| Value::Sig(Signature::new(s).unwrap());
    let ss = Value::dict(
        Type::Str,
        Type::Str,
        vec![
            ("level".into(), "1".into()),
            ("toolkit".into(), "aterm".into()),
        ],
    )
    .unwrap();
    let yd = Value::dict(
        Type::Byte,
        Type::F64,
        vec![
            (Value::Byte(1), Value::F64(0.25)),
            (Value::Byte(2), Value::F64(-8.0)),
        ],
    )
    .unwrap();
    let relations = Value::array(
        Type::structure([Type::U32, Type::array(so())]),
        vec![
            Value::Struct(vec![
                15u32.into(),
                refs(vec![object_ref(ME, "/org/a11y/atspi/accessible/3")]),
            ]),
            Value::Struct(vec![16u32.into(), refs(vec![])]),
        ],
    )
    .unwrap();
    let ay = |bytes: &[u8]| {
        Value::array(Type::Byte, bytes.iter().map(|&b| Value::Byte(b)).collect()).unwrap()
    };
    let ii = Type::structure([Type::I32, Type::I32]);
    let entries = [
        ("y_00", Value::Byte(0)),
        ("y_ff", Value::Byte(0xff)),
        ("b_true", Value::Bool(true)),
        ("b_false", Value::Bool(false)),
        ("n_neg2", Value::I16(-2)),
        ("n_min", Value::I16(i16::MIN)),
        ("q_beef", Value::U16(0xbeef)),
        ("q_at1", Value::U16(7)),
        ("i_neg1", Value::I32(-1)),
        ("i_1234567", Value::I32(1_234_567)),
        ("i_min", Value::I32(i32::MIN)),
        ("u_0", Value::U32(0)),
        ("u_max", Value::U32(u32::MAX)),
        ("u_at2", Value::U32(0xdead_beef)),
        ("x_min", Value::I64(i64::MIN)),
        ("x_at4", Value::I64(-5)),
        ("t_max", Value::U64(u64::MAX)),
        ("t_seq", Value::U64(0x0102_0304_0506_0708)),
        ("d_half", Value::F64(0.5)),
        ("d_big", Value::F64(-1.25e300)),
        ("d_negzero", Value::F64(-0.0)),
        ("s_empty", "".into()),
        ("s_iface", "org.a11y.atspi.Accessible".into()),
        ("s_utf8", "h\u{e9}llo \u{2713} \u{1d11e}".into()),
        ("s_at3", "abc".into()),
        ("o_slash", path("/")),
        ("o_root", path(ROOT)),
        ("g_empty", sig("")),
        ("g_asv", sig("a{sv}")),
        ("g_cache", sig("((so)(so)(so)iiassusau)")),
        ("v_u", Value::variant(Value::U32(7))),
        ("v_s", Value::variant("hello".into())),
        (
            "v_so",
            Value::variant(object_ref(ME, "/org/a11y/atspi/accessible/9")),
        ),
        ("v_v_i", Value::variant(Value::variant(Value::I32(-3)))),
        ("v_as", Value::variant(Value::strings(["a", "bc"]))),
        (
            "v_iiii",
            Value::variant(Value::Struct(vec![
                10.into(),
                (-20).into(),
                300.into(),
                40.into(),
            ])),
        ),
        ("v_t_at1", Value::variant(Value::U64(1))),
        ("v_g", Value::variant(sig("a(so)"))),
        ("v_o", Value::variant(path("/org/a11y/bus"))),
        ("ay_empty", ay(&[])),
        ("ay_123", ay(&[1, 2, 3])),
        ("as_empty", Value::strings(Vec::<String>::new())),
        (
            "as_ifaces",
            Value::strings([
                "org.a11y.atspi.Accessible",
                "org.a11y.atspi.Component",
                "org.a11y.atspi.Text",
            ]),
        ),
        ("au_states", u32s(&[0x0040_0102, 1])),
        (
            "ai_signs",
            Value::array(Type::I32, vec![(-1).into(), 0.into(), 1.into()]).unwrap(),
        ),
        (
            "ab",
            Value::array(Type::Bool, vec![true.into(), false.into(), true.into()]).unwrap(),
        ),
        (
            "ax_pair",
            Value::array(Type::I64, vec![i64::MIN.into(), 0i64.into()]).unwrap(),
        ),
        (
            "ax_at4",
            Value::array(Type::I64, vec![7i64.into()]).unwrap(),
        ),
        ("a_ii_empty", Value::array(ii.clone(), vec![]).unwrap()),
        ("a_ii_empty_at4", Value::array(ii, vec![]).unwrap()),
        ("so_root", object_ref(ME, ROOT)),
        ("a_so_empty", refs(vec![])),
        (
            "a_so_two",
            refs(vec![
                object_ref(ME, "/org/a11y/atspi/accessible/1"),
                object_ref(ME, "/org/a11y/atspi/accessible/22"),
            ]),
        ),
        ("ii", Value::Struct(vec![3.into(), 4.into()])),
        (
            "iiii",
            Value::Struct(vec![10.into(), (-20).into(), 300.into(), 40.into()]),
        ),
        (
            "sii",
            Value::Struct(vec!["word".into(), 0.into(), 4.into()]),
        ),
        ("yt", Value::Struct(vec![Value::Byte(1), Value::U64(2)])),
        (
            "y_t_nested",
            Value::Struct(vec![
                Value::Byte(9),
                Value::Struct(vec![Value::U64(0x1122_3344_5566_7788)]),
            ]),
        ),
        (
            "a_sss",
            Value::array(
                Type::structure([Type::Str, Type::Str, Type::Str]),
                vec![Value::Struct(vec![
                    "click".into(),
                    "Click the button".into(),
                    "".into(),
                ])],
            )
            .unwrap(),
        ),
        ("a_ss", ss),
        (
            "a_ss_empty",
            Value::dict(Type::Str, Type::Str, vec![]).unwrap(),
        ),
        (
            "a_sv",
            sv(vec![
                ("IsEnabled", true.into()),
                ("ScreenReaderEnabled", false.into()),
            ]),
        ),
        ("a_sv_empty", sv(vec![])),
        ("a_yd", yd),
        ("a_u_a_so", relations),
        (
            "event_body",
            Value::Struct(event_body("focused", 1, 0, Value::U32(0))),
        ),
        ("cache_item", Value::Struct(terminal_item())),
        (
            "a_cache_items",
            Value::array(
                cache_item_type(),
                vec![Value::Struct(terminal_item()), Value::Struct(link_item())],
            )
            .unwrap(),
        ),
        (
            "props_changed",
            Value::Struct(vec![
                "org.a11y.Status".into(),
                sv(vec![("IsEnabled", true.into())]),
                Value::strings(Vec::<String>::new()),
            ]),
        ),
        (
            "aay",
            Value::array(
                Type::array(Type::Byte),
                vec![ay(&[1]), ay(&[]), ay(&[2, 3])],
            )
            .unwrap(),
        ),
    ];
    let mut map = BTreeMap::new();
    for (name, value) in entries {
        assert!(map.insert(name, value).is_none(), "case {name} twice");
    }
    map
}

struct Row {
    name: String,
    sig: String,
    endian: Endian,
    offset: usize,
    bytes: Vec<u8>,
}

fn recorded() -> Vec<Row> {
    rows("zvariant-5.12.0-values.tsv")
        .into_iter()
        .map(|r| Row {
            name: r[0].clone(),
            sig: r[1].clone(),
            endian: match r[2].as_str() {
                "l" => Endian::Little,
                "B" => Endian::Big,
                other => panic!("endian {other}"),
            },
            offset: r[3].parse().unwrap(),
            bytes: unhex(&r[4]),
        })
        .collect()
}

#[test]
fn every_case_is_recorded_in_both_byte_orders_and_nothing_else_is() {
    let cases = cases();
    let rows = recorded();
    let mut seen: BTreeMap<&str, Vec<Endian>> = BTreeMap::new();
    for row in &rows {
        assert!(
            cases.contains_key(row.name.as_str()),
            "recorded {} has no case",
            row.name
        );
        seen.entry(row.name.as_str()).or_default().push(row.endian);
    }
    for name in cases.keys() {
        assert_eq!(
            seen.get(name).map(Vec::as_slice),
            Some(&[Endian::Little, Endian::Big][..]),
            "{name}"
        );
    }
    assert_eq!(rows.len(), 2 * cases.len());
}

#[test]
fn every_value_encodes_to_zvariants_bytes() {
    let cases = cases();
    for row in recorded() {
        let value = &cases[row.name.as_str()];
        assert_eq!(value.type_of().signature(), row.sig, "{}", row.name);
        let ours = wire::encode(std::slice::from_ref(value), row.endian, row.offset).unwrap();
        assert_eq!(
            hex(&ours),
            hex(&row.bytes),
            "{} {:?} @{}",
            row.name,
            row.endian,
            row.offset
        );
    }
}

#[test]
fn zvariants_bytes_decode_to_every_value() {
    let cases = cases();
    for row in recorded() {
        let ty = Type::parse(&row.sig).unwrap();
        let ours = wire::decode(&row.bytes, &[ty], row.endian, row.offset).unwrap();
        assert_eq!(
            ours,
            vec![cases[row.name.as_str()].clone()],
            "{} {:?}",
            row.name,
            row.endian
        );
    }
}

/// The value with its first leaf changed: a number moved by one, a bool or
/// string changed, an empty container given an element.
fn neighbour(value: &Value) -> Value {
    match value {
        Value::Byte(v) => Value::Byte(v.wrapping_add(1)),
        Value::Bool(v) => Value::Bool(!v),
        Value::I16(v) => Value::I16(v.wrapping_add(1)),
        Value::U16(v) => Value::U16(v.wrapping_add(1)),
        Value::I32(v) => Value::I32(v.wrapping_add(1)),
        Value::U32(v) => Value::U32(v.wrapping_add(1)),
        Value::I64(v) => Value::I64(v.wrapping_add(1)),
        Value::U64(v) => Value::U64(v.wrapping_add(1)),
        Value::F64(v) => Value::F64(f64::from_bits(v.to_bits() ^ 1)),
        Value::Str(s) => Value::Str(format!("{s}x")),
        Value::Path(p) if p.as_str() == "/" => path("/x"),
        Value::Path(p) => path(&format!("{}x", p.as_str())),
        Value::Sig(s) => Value::Sig(Signature::new(format!("{}y", s.as_str())).unwrap()),
        Value::Variant(inner) => Value::variant(neighbour(inner)),
        Value::Struct(fields) => {
            let mut fields = fields.clone();
            fields[0] = neighbour(&fields[0]);
            Value::Struct(fields)
        }
        Value::Array(a) => {
            let mut items = a.items().to_vec();
            match items.first_mut() {
                Some(first) => *first = neighbour(first),
                None => items.push(zero(a.elem())),
            }
            Value::array(a.elem().clone(), items).unwrap()
        }
        Value::Dict(d) => {
            let mut entries = d.entries().to_vec();
            match entries.first_mut() {
                Some((_, v)) => *v = neighbour(v),
                None => entries.push((zero(d.key()), zero(d.value()))),
            }
            Value::dict(d.key().clone(), d.value().clone(), entries).unwrap()
        }
    }
}

fn zero(ty: &Type) -> Value {
    match ty {
        Type::Byte => Value::Byte(0),
        Type::Bool => Value::Bool(false),
        Type::I16 => Value::I16(0),
        Type::U16 => Value::U16(0),
        Type::I32 => Value::I32(0),
        Type::U32 => Value::U32(0),
        Type::I64 => Value::I64(0),
        Type::U64 => Value::U64(0),
        Type::F64 => Value::F64(0.0),
        Type::Str => "".into(),
        Type::Path => path("/"),
        Type::Sig => Value::Sig(Signature::new("").unwrap()),
        Type::Variant => Value::variant(Value::Byte(0)),
        Type::Array(elem) => Value::array((**elem).clone(), vec![]).unwrap(),
        Type::Dict(k, v) => Value::dict((**k).clone(), (**v).clone(), vec![]).unwrap(),
        Type::Struct(fields) => Value::Struct(fields.iter().map(zero).collect()),
    }
}

#[test]
fn a_neighbouring_value_misses_every_recording() {
    let cases = cases();
    for row in recorded() {
        let near = neighbour(&cases[row.name.as_str()]);
        let ours = wire::encode(std::slice::from_ref(&near), row.endian, row.offset).unwrap();
        assert_ne!(
            ours, row.bytes,
            "{}: the neighbour encodes like the case",
            row.name
        );
        let back = wire::decode(
            &row.bytes,
            &[Type::parse(&row.sig).unwrap()],
            row.endian,
            row.offset,
        )
        .unwrap();
        assert_ne!(
            back,
            vec![near],
            "{}: the recording decodes to the neighbour",
            row.name
        );
    }
}

#[test]
fn no_bit_flip_of_a_recording_decodes_to_its_value() {
    let mut flips = 0;
    for row in recorded() {
        let ty = [Type::parse(&row.sig).unwrap()];
        for i in 0..row.bytes.len() {
            for bit in 0..8 {
                let mut bytes = row.bytes.clone();
                bytes[i] ^= 1 << bit;
                flips += 1;
                if let Ok(values) = wire::decode(&bytes, &ty, row.endian, row.offset) {
                    let again = wire::encode(&values, row.endian, row.offset).unwrap();
                    assert_eq!(
                        again, bytes,
                        "{} byte {i} bit {bit}: accepted a non-canonical encoding",
                        row.name
                    );
                }
            }
        }
    }
    assert!(flips > 10_000, "only {flips} flips");
}
