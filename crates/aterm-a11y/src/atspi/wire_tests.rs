// Copyright 2022, 2026 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_unix 0.22.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/atspi/object_id.rs, src/atspi/object_address.rs and the
// CacheItem construction in src/atspi/interfaces/cache.rs, and rewritten by
// the aterm project in 2026 to build aterm-dbus values instead of serde ones.

use super::*;
use crate::atspi::semantics::State;

#[test]
fn values_carry_the_at_spi_signatures() {
    let bus = ":1.42";
    assert_eq!(null_ref().type_of().signature(), "(so)");
    assert_eq!(
        null_ref(),
        Value::Struct(vec!["".into(), path_value("/org/a11y/atspi/null")])
    );
    assert_eq!(
        object_ref(bus, ObjectId::Root).type_of().signature(),
        "(so)"
    );
    assert_eq!(refs(bus, []).type_of().signature(), "a(so)");
    assert_eq!(
        interfaces(InterfaceSet::new(
            Interface::Text | Interface::Accessible | Interface::Component
        )),
        Value::strings([
            "org.a11y.atspi.Accessible",
            "org.a11y.atspi.Component",
            "org.a11y.atspi.Text"
        ])
    );
    assert_eq!(relation_set(bus, vec![]).type_of().signature(), "a(ua(so))");
    assert_eq!(actions(vec![]).type_of().signature(), "a(sss)");
    assert_eq!(
        Value::Struct(application_cache_item(
            bus,
            CacheText {
                index: -1,
                children: 1,
                interfaces: InterfaceSet::default(),
                short_name: String::new(),
                role: 75,
                name: String::new(),
                states: StateSet::empty(),
            }
        ))
        .type_of(),
        cache_item_type()
    );
    let body = event_body("insert", 5, 19, " x".into());
    assert_eq!(
        aterm_dbus::sig::signature_of(&body.iter().map(Value::type_of).collect::<Vec<_>>()),
        "siiva{sv}"
    );
}

#[test]
fn states_split_the_bits_low_word_first() {
    let set = State::Focused | State::Visible | State::ReadOnly;
    assert_eq!(
        states(set),
        array(
            Type::U32,
            vec![Value::U32((1 << 12) | (1 << 30)), Value::U32(1 << 11)]
        )
    );
}

#[test]
fn maps_are_sorted_by_key() {
    let map: HashMap<&str, String> = [("z", "1".into()), ("a", "2".into())].into();
    let Value::Dict(d) = string_map(map) else {
        panic!("a dict")
    };
    let keys: Vec<_> = d.entries().iter().filter_map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["a", "z"]);
}
