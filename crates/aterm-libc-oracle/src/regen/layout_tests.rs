// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::regen::decls::{Field, Repr};

fn types() -> TypeSizes {
    [("u8", (1, 1)), ("u32", (4, 4)), ("u64", (8, 8))]
        .into_iter()
        .map(|(t, s)| (t.to_string(), s))
        .collect()
}

#[test]
fn repr_c_pads_to_each_alignment_and_to_the_whole() {
    let fields = [("a", "u8"), ("b", "u64"), ("c", "u32")];
    assert_eq!(
        layout(&fields, &types(), None, None),
        Ok((vec![0, 8, 16], 24, 8))
    );
    assert_eq!(
        layout(&fields, &types(), Some(4), None),
        Ok((vec![0, 4, 12], 16, 4))
    );
    assert_eq!(
        layout(&fields, &types(), Some(1), None),
        Ok((vec![0, 1, 9], 13, 1))
    );
    assert_eq!(
        layout(&[("a", "u8")], &types(), None, Some(16)),
        Ok((vec![0], 16, 16))
    );
    assert_eq!(layout(&[], &types(), None, None), Ok((vec![], 0, 1)));
    assert!(layout(&[("a", "f80")], &types(), None, None).is_err());
    assert!(layout(&[("a", "u8")], &types(), Some(0), None).is_err());
    assert!(layout(&[("a", "u8")], &types(), None, Some(0)).is_err());
    assert_eq!(
        (
            up(0, 8),
            up(1, 8),
            up(8, 8),
            up(9, 4),
            up(1, 0),
            up(u64::MAX, 8)
        ),
        (Some(0), Some(8), Some(8), Some(12), None, None)
    );
}

fn record(stripped: bool, packed: Option<u64>) -> Record {
    Record {
        name: "s".into(),
        repr: packed.map(|p| Repr {
            packed: Some(p),
            ..Repr::default()
        }),
        stripped,
        traits: vec![],
        fields: vec![
            Field {
                name: "a".into(),
                ty: "u8".into(),
            },
            Field {
                name: "b".into(),
                ty: "u64".into(),
            },
        ],
    }
}

fn measured(size: u64, align: u64, offsets: [u64; 2]) -> Measured {
    let mut m = Measured {
        field_types: types(),
        ..Measured::default()
    };
    m.size.insert("s".into(), size);
    m.align.insert("s".into(), align);
    m.offset.insert(("s".into(), "a".into()), offsets[0]);
    m.offset.insert(("s".into(), "b".into()), offsets[1]);
    m
}

#[test]
fn a_public_layout_must_reproduce_every_measurement() {
    let decls = |r| Decls {
        structs: vec![r],
        ..Decls::default()
    };
    let ok = validate(&decls(record(false, None)), &measured(16, 8, [0, 8]));
    assert_eq!(ok, Ok(vec![]));
    let packed = validate(&decls(record(false, Some(1))), &measured(9, 1, [0, 1]));
    assert_eq!(packed, Ok(vec![]));
    let wrong = validate(&decls(record(false, None)), &measured(24, 8, [0, 16])).expect("measured");
    assert_eq!(
        wrong,
        [
            "OFFSET MISMATCH s.b: computed 8, measured 16",
            "SIZE MISMATCH s: computed 16, measured 24",
        ]
    );
}

#[test]
fn a_stripped_layout_places_public_fields_at_their_measured_offsets() {
    let decls = |r| Decls {
        structs: vec![r],
        ..Decls::default()
    };
    let gap = validate(&decls(record(true, None)), &measured(40, 8, [0, 16]));
    assert_eq!(gap, Ok(vec![]));
    let bad = validate(&decls(record(true, None)), &measured(8, 8, [4, 2])).expect("measured");
    assert_eq!(
        bad,
        [
            "UNPLACEABLE FIELD s.b: natural 8, measured 2",
            "PRIVATE STRUCT OVERFLOW s: 10 > 8",
        ]
    );
    let misaligned =
        validate(&decls(record(true, None)), &measured(24, 8, [0, 12])).expect("measured");
    assert_eq!(misaligned, ["UNPLACEABLE FIELD s.b: align 8, measured 12"]);
}
