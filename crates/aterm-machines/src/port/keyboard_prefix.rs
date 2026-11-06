// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The layers IOHIDKeyboardFilter::remapKey applies before the native and user tables.
//! Their exact tables and modes are kept as a before-image. Admission proves Caps passes
//! through every layer in every modifier/NumLock mode, and no other key enters Caps, so
//! changing native Caps cannot change another physical key through an upstream alias.

use crate::items::keyboard::CAPS_LOCK;
use crate::model::PValue;

type Mapping = Vec<(i64, i64)>;
type Modified = Vec<(u32, Mapping)>;

const UNREAD: &str = "incomplete upstream keyboard mappings; existing remaps left untouched";
const CONFLICT: &str =
    "an upstream keyboard mapping changes or enters Caps Lock; existing remaps left untouched";

/// Complete, checked upstream keyboard mapping state. Its tables preserve Caps in every
/// mode and have no inbound alias, leaving native/user composition sufficient for Caps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardPrefix {
    function: Mapping,
    modifiers: ModifierMaps,
    num_lock: Mapping,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ModifierMaps {
    // IOHIDFamily1633 (macOS11) exports its direct Fn-only table, without FnKeyMode.
    // Complete passthrough in every mode makes the unexported mode immaterial.
    FnOnly(Mapping),
    Unified {
        fn_mode: i64,
        enabled: bool,
        legacy: Modified,
        unified: Modified,
    },
}

const MODERN_FIELDS: [&str; 4] = [
    "FnKeyMode",
    "UnifiedKeyMapping",
    "LegacyKeyMaps",
    "UnifiedKeyMaps",
];

impl KeyboardPrefix {
    pub(crate) fn read(filter: &PValue) -> Result<Self, String> {
        let PValue::Dict(fields) = filter else {
            return Err(UNREAD.into());
        };
        let modifiers = if fields.iter().any(|(name, _)| name == "FnKeyboardUsageMap") {
            // This is a distinct, complete historical shape, never a fallback from an
            // incomplete modern record. Any mixture, including duplicate keys, refuses.
            if fields
                .iter()
                .any(|(name, _)| MODERN_FIELDS.contains(&name.as_str()))
            {
                return Err(UNREAD.into());
            }
            ModifierMaps::FnOnly(mapping(field(filter, "FnKeyboardUsageMap").ok_or(UNREAD)?)?)
        } else {
            let fn_mode = match field(filter, "FnKeyMode") {
                Some(PValue::Int(mode)) => *mode,
                _ => return Err(UNREAD.into()),
            };
            let enabled = match field(filter, "UnifiedKeyMapping") {
                Some(PValue::Bool(unified)) => *unified,
                _ => return Err(UNREAD.into()),
            };
            ModifierMaps::Unified {
                fn_mode,
                enabled,
                legacy: modified(field(filter, "LegacyKeyMaps").ok_or(UNREAD)?)?,
                unified: modified(field(filter, "UnifiedKeyMaps").ok_or(UNREAD)?)?,
            }
        };
        Ok(Self {
            function: mapping(field(filter, "FnFunctionUsageMap").ok_or(UNREAD)?)?,
            modifiers,
            num_lock: mapping(field(filter, "NumLockKeyboardUsageMap").ok_or(UNREAD)?)?,
        })
    }
}

#[cfg(test)]
impl Default for KeyboardPrefix {
    fn default() -> Self {
        Self::read(&PValue::Dict(tests::fields())).expect("complete empty modern fixture")
    }
}

fn field<'a>(value: &'a PValue, key: &str) -> Option<&'a PValue> {
    let PValue::Dict(fields) = value else {
        return None;
    };
    let mut values = fields.iter().filter(|(name, _)| name == key);
    let value = &values.next()?.1;
    values.next().is_none().then_some(value)
}

fn mapping(value: &PValue) -> Result<Mapping, String> {
    let PValue::Array(pairs) = value else {
        return Err(UNREAD.into());
    };
    let mut mapping = pairs
        .iter()
        .map(|pair| match (field(pair, "Src"), field(pair, "Dst")) {
            (Some(PValue::Int(source)), Some(PValue::Int(destination))) => {
                Ok((*source, *destination))
            }
            _ => Err(UNREAD.to_owned()),
        })
        .collect::<Result<Mapping, String>>()?;
    mapping.sort_unstable();
    if mapping.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(UNREAD.into());
    }
    // Identity Caps is harmless. A non-Caps source entering Caps would make a native
    // Caps change alter that physical key, even if physical Caps itself passes through.
    if mapping.iter().any(|(source, destination)| {
        (*source == CAPS_LOCK || *destination == CAPS_LOCK) && source != destination
    }) {
        return Err(CONFLICT.into());
    }
    Ok(mapping)
}

fn modified(value: &PValue) -> Result<Modified, String> {
    let PValue::Array(tables) = value else {
        return Err(UNREAD.into());
    };
    let mut maps = tables
        .iter()
        .map(|table| {
            let PValue::Dict(fields) = table else {
                return Err(UNREAD.to_owned());
            };
            let [(mask, pairs)] = fields.as_slice() else {
                return Err(UNREAD.to_owned());
            };
            let mask = if mask == "0" {
                0
            } else {
                u32::from_str_radix(mask.strip_prefix("0x").ok_or(UNREAD)?, 16)
                    .map_err(|_| UNREAD)?
            };
            Ok((mask, mapping(pairs)?))
        })
        .collect::<Result<Modified, String>>()?;
    maps.sort_unstable_by_key(|(mask, _)| *mask);
    if maps.windows(2).any(|tables| tables[0].0 == tables[1].0) {
        return Err(UNREAD.into());
    }
    Ok(maps)
}

#[cfg(test)]
pub(crate) mod tests {
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
}
