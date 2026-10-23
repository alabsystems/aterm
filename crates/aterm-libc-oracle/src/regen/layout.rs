// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `repr(C)` layout, reproduced and held to the measurements.

use std::collections::BTreeMap;

use super::decls::{Decls, Record};
use super::probe::Measured;

/// Each field type's measured size and alignment, keyed by its source text.
pub type TypeSizes = BTreeMap<String, (u64, u64)>;

/// `x` rounded up to a multiple of `align`.
#[must_use]
pub const fn up(x: u64, align: u64) -> u64 {
    x.div_ceil(align) * align
}

/// One field's measured shape, packing applied.
fn shape(ty: &str, types: &TypeSizes, packed: Option<u64>) -> Result<(u64, u64), String> {
    let (size, align) = *types
        .get(ty)
        .ok_or_else(|| format!("no measured size for field type `{ty}`"))?;
    Ok((size, packed.map_or(align, |p| align.min(p))))
}

/// A `repr(C)` struct of `fields` (name, type text).
/// # Errors
/// When a field type was not measured.
pub fn layout(
    fields: &[(&str, &str)],
    types: &TypeSizes,
    packed: Option<u64>,
    force_align: Option<u64>,
) -> Result<(Vec<u64>, u64, u64), String> {
    let (mut offset, mut align) = (0, 1);
    let mut offsets = Vec::with_capacity(fields.len());
    for (_, ty) in fields {
        let (size, field_align) = shape(ty, types, packed)?;
        offset = up(offset, field_align);
        offsets.push(offset);
        offset += size;
        align = align.max(field_align);
    }
    if let Some(forced) = force_align {
        align = align.max(forced);
    }
    Ok((offsets, up(offset, align), align))
}

fn measured(map: &BTreeMap<String, u64>, name: &str, what: &str) -> Result<u64, String> {
    map.get(name)
        .copied()
        .ok_or_else(|| format!("{name}: its {what} was not measured"))
}

/// One struct's disagreements with the measurements.
fn check(record: &Record, measured_cell: &Measured, out: &mut Vec<String>) -> Result<(), String> {
    let name = record.name.as_str();
    let size = measured(&measured_cell.size, name, "size")?;
    let align = measured(&measured_cell.align, name, "alignment")?;
    let repr = record.repr.clone().unwrap_or_default();
    let offset_of = |field: &str| {
        measured_cell
            .offset
            .get(&(name.to_string(), field.to_string()))
    };
    let fields: Vec<(&str, &str)> = record
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.ty.as_str()))
        .collect();
    let mut whole = Vec::new();
    let computed: Vec<u64> = if record.stripped {
        let mut end = 0;
        let mut offsets = Vec::new();
        for (field, ty) in &fields {
            let (field_size, field_align) = shape(ty, &measured_cell.field_types, repr.packed)?;
            let want = *offset_of(field)
                .ok_or_else(|| format!("{name}.{field}: its offset was not measured"))?;
            let natural = up(end, field_align);
            if natural > want {
                out.push(format!(
                    "UNPLACEABLE FIELD {name}.{field}: natural {natural}, measured {want}"
                ));
            }
            offsets.push(want);
            end = want + field_size;
        }
        if end > size {
            out.push(format!("PRIVATE STRUCT OVERFLOW {name}: {end} > {size}"));
        }
        offsets
    } else {
        let (offsets, computed_size, computed_align) =
            layout(&fields, &measured_cell.field_types, repr.packed, repr.align)?;
        if computed_size != size {
            whole.push(format!(
                "SIZE MISMATCH {name}: computed {computed_size}, measured {size}"
            ));
        }
        if computed_align != align {
            whole.push(format!(
                "ALIGN MISMATCH {name}: computed {computed_align}, measured {align}"
            ));
        }
        offsets
    };
    for ((field, _), offset) in fields.iter().zip(computed) {
        if let Some(&want) = offset_of(field).filter(|want| **want != offset) {
            out.push(format!(
                "OFFSET MISMATCH {name}.{field}: computed {offset}, measured {want}"
            ));
        }
    }
    out.extend(whole);
    Ok(())
}

/// Every place the engine disagrees with the measurements of one cell.
/// # Errors
/// When a struct or field type was not measured.
pub fn validate(decls: &Decls, measured_cell: &Measured) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for record in decls.structs.iter().filter(|r| !r.fields.is_empty()) {
        check(record, measured_cell, &mut out)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
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
        assert_eq!((up(0, 8), up(1, 8), up(8, 8), up(9, 4)), (0, 8, 8, 12));
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
        let wrong =
            validate(&decls(record(false, None)), &measured(24, 8, [0, 16])).expect("measured");
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
    }
}
