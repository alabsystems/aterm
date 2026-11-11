// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn containers_refuse_a_foreign_element() {
    assert!(Value::array(Type::U32, vec![Value::U32(1), Value::I32(2)]).is_err());
    assert!(Value::dict(Type::Variant, Type::Str, vec![]).is_err());
    assert!(Value::dict(Type::Str, Type::Variant, vec![("k".into(), Value::U32(1))]).is_err());
    let sv = Value::dict(
        Type::Str,
        Type::Variant,
        vec![("k".into(), Value::variant(1u32.into()))],
    )
    .unwrap();
    assert_eq!(sv.type_of().signature(), "a{sv}");
    assert_eq!(
        sv.as_dict().unwrap().get("k").and_then(Value::as_variant),
        Some(&Value::U32(1))
    );
    let empty = Value::array(Type::structure([Type::Str, Type::Path]), vec![]).unwrap();
    assert_eq!(empty.type_of().signature(), "a(so)");
    assert!(ObjectPath::new("/a/").is_err() && Signature::new("a").is_err());
}

#[test]
fn a_value_stays_small() {
    assert!(
        std::mem::size_of::<Value>() <= 32,
        "{}",
        std::mem::size_of::<Value>()
    );
}
