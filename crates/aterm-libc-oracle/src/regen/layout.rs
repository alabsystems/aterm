// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `repr(C)` layout, reproduced and held to the measurements.

use std::collections::BTreeMap;

use super::decls::{Decls, Record};
use super::probe::Measured;

/// Each field type's measured size and alignment, keyed by its source text.
pub type TypeSizes = BTreeMap<String, (u64, u64)>;

/// `x` rounded up to a multiple of `align`, or `None` when `align == 0` or the
/// rounded value overflows `u64`.
#[must_use]
pub const fn up(x: u64, align: u64) -> Option<u64> {
    if align == 0 {
        return None;
    }
    x.div_ceil(align).checked_mul(align)
}

/// One field's measured shape, packing applied.
fn shape(ty: &str, types: &TypeSizes, packed: Option<u64>) -> Result<(u64, u64), String> {
    if packed == Some(0) {
        return Err("packed(0) is invalid".into());
    }
    let (size, align) = *types
        .get(ty)
        .ok_or_else(|| format!("no measured size for field type `{ty}`"))?;
    if align == 0 {
        return Err(format!("invalid alignment 0 for field type `{ty}`"));
    }
    Ok((size, packed.map_or(align, |p| align.min(p))))
}

/// A `repr(C)` struct of `fields` (name, type text): each field's offset, then
/// the struct's size and alignment.
/// # Errors
/// When a field type was not measured, an alignment or packing is zero, or a size overflows.
pub fn layout(
    fields: &[(&str, &str)],
    types: &TypeSizes,
    packed: Option<u64>,
    force_align: Option<u64>,
) -> Result<(Vec<u64>, u64, u64), String> {
    if packed == Some(0) {
        return Err("packed(0) is invalid".into());
    }
    if force_align == Some(0) {
        return Err("align(0) is invalid".into());
    }
    let (mut offset, mut align) = (0, 1);
    let mut offsets = Vec::with_capacity(fields.len());
    for (name, ty) in fields {
        let (size, field_align) = shape(ty, types, packed)?;
        offset = up(offset, field_align)
            .ok_or_else(|| format!("field `{name}` offset overflowed u64"))?;
        offsets.push(offset);
        offset = offset
            .checked_add(size)
            .ok_or_else(|| format!("field `{name}` extent overflowed u64"))?;
        align = align.max(field_align);
    }
    if let Some(forced) = force_align {
        align = align.max(forced);
    }
    let total = up(offset, align).ok_or_else(|| "struct size overflowed u64".to_string())?;
    Ok((offsets, total, align))
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
    let mut key = (record.name.clone(), String::new());
    let mut offset_of = |field: &str| {
        key.1.clear();
        key.1.push_str(field);
        measured_cell.offset.get(&key).copied()
    };
    let fields: Vec<(&str, &str)> = record
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.ty.as_str()))
        .collect();
    let mut whole = Vec::new();
    let computed: Vec<u64> = if record.stripped {
        if repr.packed == Some(0) {
            return Err(format!("{name}: packed(0) is invalid"));
        }
        let mut end = 0;
        let mut offsets = Vec::new();
        for (field, ty) in &fields {
            let (field_size, field_align) = shape(ty, &measured_cell.field_types, repr.packed)?;
            let want = offset_of(field)
                .ok_or_else(|| format!("{name}.{field}: its offset was not measured"))?;
            let natural = up(end, field_align)
                .ok_or_else(|| format!("{name}.{field}: natural offset overflowed u64"))?;
            if natural > want {
                out.push(format!(
                    "UNPLACEABLE FIELD {name}.{field}: natural {natural}, measured {want}"
                ));
            } else if !want.is_multiple_of(field_align) {
                out.push(format!(
                    "UNPLACEABLE FIELD {name}.{field}: align {field_align}, measured {want}"
                ));
            }
            offsets.push(want);
            end = want
                .checked_add(field_size)
                .ok_or_else(|| format!("{name}.{field}: extent overflowed u64"))?;
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
        if let Some(want) = offset_of(field).filter(|want| *want != offset) {
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
/// When a struct or field type was not measured, an alignment or packing is zero, or a size overflows.
pub fn validate(decls: &Decls, measured_cell: &Measured) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for record in decls.structs.iter().filter(|r| !r.fields.is_empty()) {
        check(record, measured_cell, &mut out)?;
    }
    Ok(out)
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
