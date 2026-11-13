// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(crate) fn fields() -> Vec<(String, PValue)> {
    vec![
        ("FnFunctionUsageMap".into(), PValue::Array(Vec::new())),
        ("FnKeyMode".into(), PValue::Int(0)),
        ("UnifiedKeyMapping".into(), PValue::Bool(false)),
        ("LegacyKeyMaps".into(), PValue::Array(Vec::new())),
        ("UnifiedKeyMaps".into(), PValue::Array(Vec::new())),
        ("NumLockKeyboardUsageMap".into(), PValue::Array(Vec::new())),
    ]
}

pub(crate) fn historical_fields() -> Vec<(String, PValue)> {
    vec![
        ("FnFunctionUsageMap".into(), PValue::Array(Vec::new())),
        ("FnKeyboardUsageMap".into(), PValue::Array(Vec::new())),
        ("NumLockKeyboardUsageMap".into(), PValue::Array(Vec::new())),
    ]
}

pub(crate) fn pairs(mapping: &[(i64, i64)]) -> PValue {
    PValue::Array(
        mapping
            .iter()
            .map(|(source, destination)| {
                PValue::Dict(vec![
                    ("Src".into(), PValue::Int(*source)),
                    ("Dst".into(), PValue::Int(*destination)),
                ])
            })
            .collect(),
    )
}

fn put(fields: &mut [(String, PValue)], key: &str, value: PValue) {
    fields.iter_mut().find(|(name, _)| name == key).unwrap().1 = value;
}

#[test]
fn unrelated_function_modifier_and_numlock_maps_pass_in_every_mode() {
    let mut fields = fields();
    put(
        &mut fields,
        "FnFunctionUsageMap",
        pairs(&[(0x7_0000_003a, 0xc_0000_006f), (CAPS_LOCK, CAPS_LOCK)]),
    );
    put(
        &mut fields,
        "LegacyKeyMaps",
        PValue::Array(vec![PValue::Dict(vec![(
            "0x2".into(),
            pairs(&[(0x7_0000_0050, 0x7_0000_004a)]),
        )])]),
    );
    put(
        &mut fields,
        "UnifiedKeyMaps",
        PValue::Array(vec![PValue::Dict(vec![(
            "0x1".into(),
            pairs(&[(0x7_0000_004f, 0x7_0000_004d)]),
        )])]),
    );
    put(
        &mut fields,
        "NumLockKeyboardUsageMap",
        pairs(&[(0x7_0000_005e, 0x7_0000_004f)]),
    );
    let before = KeyboardPrefix::read(&PValue::Dict(fields.clone())).unwrap();
    put(&mut fields, "FnKeyMode", PValue::Int(1));
    put(&mut fields, "UnifiedKeyMapping", PValue::Bool(true));
    let after = KeyboardPrefix::read(&PValue::Dict(fields)).unwrap();
    assert_ne!(
        before, after,
        "complete before-images retain the modes as well as maps"
    );
}

#[test]
fn every_upstream_layer_refuses_caps_remapping_and_inbound_aliases() {
    for name in [
        "FnFunctionUsageMap",
        "LegacyKeyMaps",
        "UnifiedKeyMaps",
        "NumLockKeyboardUsageMap",
    ] {
        for map in [[(CAPS_LOCK, 0x7_0000_00e2)], [(0x7_0000_003a, CAPS_LOCK)]] {
            let mut fields = fields();
            let pairs = pairs(&map);
            let value = if matches!(name, "LegacyKeyMaps" | "UnifiedKeyMaps") {
                PValue::Array(vec![PValue::Dict(vec![("0x1".into(), pairs)])])
            } else {
                pairs
            };
            put(&mut fields, name, value);
            assert_eq!(
                KeyboardPrefix::read(&PValue::Dict(fields)),
                Err(CONFLICT.into())
            );
        }
    }
}

#[test]
fn complete_historical_fn_only_format_proves_passthrough_without_assuming_a_mode() {
    let mut fields = historical_fields();
    put(
        &mut fields,
        "FnFunctionUsageMap",
        pairs(&[(0x7_0000_003a, 0xc_0000_006f)]),
    );
    put(
        &mut fields,
        "FnKeyboardUsageMap",
        pairs(&[(0x7_0000_0050, 0x7_0000_004a), (CAPS_LOCK, CAPS_LOCK)]),
    );
    put(
        &mut fields,
        "NumLockKeyboardUsageMap",
        pairs(&[(0x7_0000_005e, 0x7_0000_004f)]),
    );
    let before = KeyboardPrefix::read(&PValue::Dict(fields.clone())).unwrap();
    assert!(matches!(before.modifiers, ModifierMaps::FnOnly(_)));
    let empty = KeyboardPrefix::read(&PValue::Dict(historical_fields())).unwrap();
    assert_ne!(
        empty,
        KeyboardPrefix::default(),
        "the schema is part of the before-image"
    );
    for (name, _) in historical_fields() {
        for map in [[(CAPS_LOCK, 0x7_0000_00e2)], [(0x7_0000_003a, CAPS_LOCK)]] {
            let mut conflict = fields.clone();
            put(&mut conflict, &name, pairs(&map));
            assert_eq!(
                KeyboardPrefix::read(&PValue::Dict(conflict)),
                Err(CONFLICT.into())
            );
        }
        let mut absent = fields.clone();
        absent.retain(|(key, _)| key != &name);
        assert!(
            KeyboardPrefix::read(&PValue::Dict(absent)).is_err(),
            "{name}"
        );
    }
}

#[test]
fn mixed_or_partial_modern_records_never_fall_back_to_the_historical_shape() {
    for name in MODERN_FIELDS {
        let mut mixed = historical_fields();
        mixed.push(fields().into_iter().find(|(key, _)| key == name).unwrap());
        assert!(
            KeyboardPrefix::read(&PValue::Dict(mixed)).is_err(),
            "{name}"
        );
    }
    let mut mixed = fields();
    mixed.push(("FnKeyboardUsageMap".into(), pairs(&[])));
    assert!(KeyboardPrefix::read(&PValue::Dict(mixed)).is_err());
    let mut ambiguous = historical_fields();
    ambiguous.push(("FnKeyboardUsageMap".into(), pairs(&[])));
    assert!(KeyboardPrefix::read(&PValue::Dict(ambiguous)).is_err());
}

#[test]
fn absent_or_malformed_prefix_state_is_never_assumed_empty() {
    for (missing, _) in fields() {
        let mut absent = fields();
        absent.retain(|(key, _)| key != &missing);
        assert!(
            KeyboardPrefix::read(&PValue::Dict(absent)).is_err(),
            "{missing}"
        );
    }
    let mut malformed = fields();
    put(
        &mut malformed,
        "LegacyKeyMaps",
        PValue::Array(vec![PValue::Dict(vec![("unknown".into(), pairs(&[]))])]),
    );
    assert!(KeyboardPrefix::read(&PValue::Dict(malformed)).is_err());
    let mut duplicate = fields();
    put(
        &mut duplicate,
        "FnFunctionUsageMap",
        pairs(&[(CAPS_LOCK, CAPS_LOCK), (CAPS_LOCK, CAPS_LOCK)]),
    );
    assert!(KeyboardPrefix::read(&PValue::Dict(duplicate)).is_err());
}
