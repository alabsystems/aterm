// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The network-place reader and JSON projection for database diagnostics.

use aterm_json::{Value, json};
use aterm_netplace::{Place, words::Field};

/// Read this machine's current place without starting a keeper or changing its evidence.
pub(super) fn current() -> Option<Place> {
    aterm_types::dirs::state_dir().map(|state| aterm_netplace::read(&state))
}

/// The same fields as `sys` uses, including explicit reasons for unknown values.
pub(super) fn value(place: Option<&Place>) -> Value {
    place.map_or_else(
        || json!({"why":"not_sampled"}),
        |place| fields(aterm_netplace::words::json_fields(place)),
    )
}

fn fields(values: Vec<(&'static str, Field)>) -> Value {
    Value::Object(
        values
            .into_iter()
            .map(|(key, value)| (key.to_owned(), field(value)))
            .collect(),
    )
}

fn field(value: Field) -> Value {
    match value {
        Field::Text(text) => Value::String(text),
        Field::Count(count) => Value::from(count),
        Field::Flag(flag) => Value::Bool(flag),
        Field::Absent => json!({"why":"not_sampled"}),
        Field::Object(values) => fields(values),
    }
}
