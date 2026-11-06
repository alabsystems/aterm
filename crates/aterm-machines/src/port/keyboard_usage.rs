// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyboard conformance includes a composite service whose primary collection is a mouse.

use crate::model::PValue;

pub(crate) const MATCHING: &str = r#"[{"PrimaryUsagePage":1,"PrimaryUsage":6},{"DeviceUsagePairs":[{"DeviceUsagePage":1,"DeviceUsage":6}]}]"#;

pub(crate) fn matching_model(vendor: u32, product: u32) -> String {
    format!(
        "[{{\"PrimaryUsagePage\":1,\"PrimaryUsage\":6,\"VendorID\":{vendor},\"ProductID\":{product}}},{{\"DeviceUsagePairs\":[{{\"DeviceUsagePage\":1,\"DeviceUsage\":6}}],\"VendorID\":{vendor},\"ProductID\":{product}}}]"
    )
}

fn field<'a>(value: &'a PValue, key: &str) -> Option<&'a PValue> {
    let PValue::Dict(fields) = value else {
        return None;
    };
    let mut matches = fields.iter().filter(|(name, _)| name == key);
    let value = &matches.next()?.1;
    matches.next().is_none().then_some(value)
}

pub(crate) fn has_keyboard_usage(pairs: &PValue) -> bool {
    let PValue::Array(pairs) = pairs else {
        return false;
    };
    pairs.iter().any(|pair| {
        field(pair, "DeviceUsagePage") == Some(&PValue::Int(1))
            && field(pair, "DeviceUsage") == Some(&PValue::Int(6))
    })
}

pub(crate) fn is_keyboard_service(service: &PValue) -> bool {
    (field(service, "PrimaryUsagePage") == Some(&PValue::Int(1))
        && field(service, "PrimaryUsage") == Some(&PValue::Int(6)))
        || field(service, "DeviceUsagePairs").is_some_and(has_keyboard_usage)
}
