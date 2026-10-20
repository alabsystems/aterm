// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The recorded corpora (tests/recorded/) and the values both sides of the
//! oracle name: the recorder built each one in zbus/zvariant types, and these
//! builders make the same one in aterm-dbus's.

#![allow(
    dead_code,
    reason = "a part only a unix module uses is dead where that module is compiled out"
)]

use aterm_dbus::{Type, Value};

/// The rows of a recorded TSV, `#` comments skipped, split on tabs.
pub fn rows(file: &str) -> Vec<Vec<String>> {
    let path = format!("{}/tests/recorded/{file}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| line.split('\t').map(str::to_owned).collect())
        .collect()
}

pub fn unhex(text: &str) -> Vec<u8> {
    aterm_codec::hex::decode_vec(text.as_bytes()).unwrap_or_else(|| panic!("bad hex {text}"))
}

pub fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

pub const ME: &str = ":1.42";
pub const ROOT: &str = "/org/a11y/atspi/accessible/root";
pub const RULE: &str = "type='signal',sender='org.a11y.Bus',path='/org/a11y/bus',\
                        interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',\
                        arg0='org.a11y.Status'";
pub const AT_SPI_ADDRESS: &str =
    "unix:path=/run/user/1000/at-spi/bus_0,guid=0f3b26c3a5fe4e1f9b8a3e1c7d6e5f40";

pub fn path(p: &str) -> Value {
    Value::path(p).unwrap()
}

pub fn so() -> Type {
    Type::structure([Type::Str, Type::Path])
}

/// An AT-SPI object reference `(so)`.
pub fn object_ref(name: &str, p: &str) -> Value {
    Value::Struct(vec![name.into(), path(p)])
}

pub fn refs(items: Vec<Value>) -> Value {
    Value::array(so(), items).unwrap()
}

/// An `a{sv}` from (key, unwrapped value) pairs.
pub fn sv(entries: Vec<(&str, Value)>) -> Value {
    let entries = entries
        .into_iter()
        .map(|(k, v)| (Value::from(k), Value::variant(v)))
        .collect();
    Value::dict(Type::Str, Type::Variant, entries).unwrap()
}

pub fn u32s(items: &[u32]) -> Value {
    Value::array(Type::U32, items.iter().map(|&v| Value::U32(v)).collect()).unwrap()
}

/// An AT-SPI event body's five arguments `siiva{sv}`, as fields.
pub fn event_body(kind: &str, detail1: i32, detail2: i32, any_data: Value) -> Vec<Value> {
    vec![
        kind.into(),
        detail1.into(),
        detail2.into(),
        Value::variant(any_data),
        sv(vec![]),
    ]
}

/// A Cache item `((so)(so)(so)iiassusau)`'s fields.
#[allow(clippy::too_many_arguments, reason = "the item's own ten fields")]
pub fn cache_item_fields(
    node: &str,
    parent: &str,
    index: i32,
    children: i32,
    ifaces: &[&str],
    role: u32,
    name: &str,
    states: &[u32],
) -> Vec<Value> {
    vec![
        object_ref(ME, node),
        object_ref(ME, ROOT),
        object_ref(ME, parent),
        index.into(),
        children.into(),
        Value::strings(ifaces.iter().copied()),
        "".into(),
        role.into(),
        name.into(),
        u32s(states),
    ]
}

pub fn terminal_item() -> Vec<Value> {
    cache_item_fields(
        "/org/a11y/atspi/accessible/7",
        ROOT,
        0,
        2,
        &["org.a11y.atspi.Accessible", "org.a11y.atspi.Text"],
        43,
        "Terminal",
        &[0x0000_4002, 0],
    )
}

pub fn link_item() -> Vec<Value> {
    cache_item_fields(
        "/org/a11y/atspi/accessible/8",
        "/org/a11y/atspi/accessible/7",
        1,
        0,
        &[
            "org.a11y.atspi.Accessible",
            "org.a11y.atspi.Action",
            "org.a11y.atspi.Hyperlink",
        ],
        88,
        "https://example.org/",
        &[0, 0],
    )
}

pub fn cache_item_type() -> Type {
    Type::structure([
        so(),
        so(),
        so(),
        Type::I32,
        Type::I32,
        Type::array(Type::Str),
        Type::Str,
        Type::U32,
        Type::Str,
        Type::array(Type::U32),
    ])
}
