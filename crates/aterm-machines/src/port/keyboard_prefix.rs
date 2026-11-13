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
#[path = "keyboard_prefix_tests.rs"]
pub(crate) mod tests;
