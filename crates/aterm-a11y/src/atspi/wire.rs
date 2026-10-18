// Copyright 2022, 2026 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_unix 0.22.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/atspi/object_id.rs, src/atspi/object_address.rs and the
// CacheItem construction in src/atspi/interfaces/cache.rs, and rewritten by
// the aterm project in 2026 to build aterm-dbus values instead of serde ones.

//! The AT-SPI values on the wire, built as aterm-dbus [`Value`]s: object
//! paths and references, interface and state sets, rectangles, attribute
//! maps, cache items and event bodies.
//!
//! Every shape here is the one accesskit_unix sent through zvariant: the
//! signatures are the AT-SPI introspection's (`a(so)`, `au`, `(iiii)`, ...),
//! and the paths are its object ids, so a screen reader reading the old
//! publisher reads this one the same way.

use std::collections::HashMap;

use aterm_dbus::{Type, Value};

use super::semantics::{
    Action, CacheNode, Interface, InterfaceSet, NodeId, NodeIdOrRoot, PlatformNode, Rect,
    RelationType, StateSet,
};

/// The application root's path.
pub(crate) const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
/// The `org.a11y.atspi.Cache` object's path.
pub(crate) const CACHE_PATH: &str = "/org/a11y/atspi/cache";
/// Where a node's path starts: `<prefix><adapter>/<node id as u128>`.
const ACCESSIBLE_PREFIX: &str = "/org/a11y/atspi/accessible/";
/// AT-SPI's null object (libatspi's `ATSPI_DBUS_PATH_NULL`).
const NULL_PATH: &str = "/org/a11y/atspi/null";

/// One object this process publishes: the application root, or a node of
/// one window's tree (one adapter per window).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ObjectId {
    Root,
    Node { adapter: usize, node: NodeId },
}

impl ObjectId {
    pub(crate) fn of(node: &PlatformNode) -> Self {
        Self::Node {
            adapter: node.adapter_id(),
            node: node.id(),
        }
    }

    pub(crate) fn in_adapter(adapter: usize, target: NodeIdOrRoot) -> Self {
        match target {
            NodeIdOrRoot::Node(node) => Self::Node { adapter, node },
            NodeIdOrRoot::Root => Self::Root,
        }
    }

    pub(crate) fn path(self) -> String {
        match self {
            Self::Root => ROOT_PATH.to_owned(),
            Self::Node { adapter, node } => {
                format!("{ACCESSIBLE_PREFIX}{adapter}/{}", u128::from(node))
            }
        }
    }
}

/// An object path value. Every path this module builds is a constant or the
/// prefix plus two decimal numbers, which the specification always accepts.
fn path_value(path: &str) -> Value {
    Value::path(path).expect("the publisher builds only valid object paths")
}

/// An array whose items the caller built all of type `elem`.
fn array(elem: Type, items: Vec<Value>) -> Value {
    Value::array(elem, items).expect("every item is of the element type")
}

/// `(so)`, an AT-SPI object reference.
pub(crate) fn object_ref_type() -> Type {
    Type::structure([Type::Str, Type::Path])
}

/// `(so)`: the object `id` of the connection named `bus`.
pub(crate) fn object_ref(bus: &str, id: ObjectId) -> Value {
    Value::Struct(vec![bus.into(), path_value(&id.path())])
}

/// `(so)`: the null reference, no bus name and AT-SPI's null path.
pub(crate) fn null_ref() -> Value {
    Value::Struct(vec!["".into(), path_value(NULL_PATH)])
}

/// `(so)`: `id` when there is one, else the null reference.
pub(crate) fn optional_ref(bus: &str, id: Option<ObjectId>) -> Value {
    id.map_or_else(null_ref, |id| object_ref(bus, id))
}

/// `a(so)`.
pub(crate) fn refs(bus: &str, ids: impl IntoIterator<Item = ObjectId>) -> Value {
    array(
        object_ref_type(),
        ids.into_iter().map(|id| object_ref(bus, id)).collect(),
    )
}

/// An interface's D-Bus name.
pub(crate) fn interface_name(interface: Interface) -> &'static str {
    match interface {
        Interface::Accessible => "org.a11y.atspi.Accessible",
        Interface::Action => "org.a11y.atspi.Action",
        Interface::Application => "org.a11y.atspi.Application",
        Interface::Component => "org.a11y.atspi.Component",
        Interface::Hyperlink => "org.a11y.atspi.Hyperlink",
        Interface::Selection => "org.a11y.atspi.Selection",
        Interface::Text => "org.a11y.atspi.Text",
        Interface::Value => "org.a11y.atspi.Value",
    }
}

/// `as`: the set's names, in the set's bit order (Accessible first).
pub(crate) fn interfaces(set: InterfaceSet) -> Value {
    Value::strings(set.iter().map(interface_name))
}

/// `au`: the 64 state bits as two words, low first.
pub(crate) fn states(set: StateSet) -> Value {
    let bits = set.bits();
    let low = (bits & u64::from(u32::MAX)) as u32;
    let high = (bits >> 32) as u32;
    array(Type::U32, vec![Value::U32(low), Value::U32(high)])
}

/// A rectangle's four fields, `iiii`, as a method's out-arguments.
pub(crate) fn rect_fields(rect: Rect) -> Vec<Value> {
    vec![
        rect.x.into(),
        rect.y.into(),
        rect.width.into(),
        rect.height.into(),
    ]
}

/// `(iiii)`.
pub(crate) fn rect(rect: Rect) -> Value {
    Value::Struct(rect_fields(rect))
}

/// `a{ss}`, its keys sorted so the bytes do not follow a hash order.
pub(crate) fn string_map<K: AsRef<str>>(map: HashMap<K, String>) -> Value {
    let mut entries: Vec<(String, String)> = map
        .into_iter()
        .map(|(k, v)| (k.as_ref().to_owned(), v))
        .collect();
    entries.sort();
    Value::dict(
        Type::Str,
        Type::Str,
        entries
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
    )
    .expect("string keys and string values")
}

/// `a(ua(so))`: each relation's type and its targets.
pub(crate) fn relation_set(bus: &str, set: Vec<(RelationType, Vec<ObjectId>)>) -> Value {
    let elem = Type::structure([Type::U32, Type::array(object_ref_type())]);
    array(
        elem,
        set.into_iter()
            .map(|(kind, targets)| Value::Struct(vec![(kind as u32).into(), refs(bus, targets)]))
            .collect(),
    )
}

/// `a(sss)`: each action's localized name, description and key binding.
pub(crate) fn actions(actions: Vec<Action>) -> Value {
    let elem = Type::structure([Type::Str, Type::Str, Type::Str]);
    array(
        elem,
        actions
            .into_iter()
            .map(|a| {
                Value::Struct(vec![
                    a.localized_name.into(),
                    a.description.into(),
                    a.key_binding.into(),
                ])
            })
            .collect(),
    )
}

/// `((so)(so)(so)iiassusau)`, a cache item.
pub(crate) fn cache_item_type() -> Type {
    Type::structure([
        object_ref_type(),
        object_ref_type(),
        object_ref_type(),
        Type::I32,
        Type::I32,
        Type::array(Type::Str),
        Type::Str,
        Type::U32,
        Type::Str,
        Type::array(Type::U32),
    ])
}

/// A cache item's ten fields for `node`: itself, the application, its
/// parent, its index and child count, its interfaces, its name (AT-SPI's
/// "short name"), role, description (AT-SPI's "name") and states.
pub(crate) fn cache_item(bus: &str, node: CacheNode) -> Vec<Value> {
    let parent = ObjectId::in_adapter(node.adapter_id, node.parent);
    cache_fields(
        bus,
        object_ref(
            bus,
            ObjectId::Node {
                adapter: node.adapter_id,
                node: node.id,
            },
        ),
        object_ref(bus, parent),
        CacheText {
            index: node.index_in_parent,
            children: node.child_count,
            interfaces: node.interfaces,
            short_name: node.name,
            role: node.role as u32,
            name: node.description,
            states: node.states,
        },
    )
}

/// The application root's cache item: no parent, no index.
pub(crate) fn application_cache_item(bus: &str, item: CacheText) -> Vec<Value> {
    cache_fields(bus, object_ref(bus, ObjectId::Root), null_ref(), item)
}

/// A cache item's fields past its three references.
pub(crate) struct CacheText {
    pub(crate) index: i32,
    pub(crate) children: i32,
    pub(crate) interfaces: InterfaceSet,
    pub(crate) short_name: String,
    pub(crate) role: u32,
    pub(crate) name: String,
    pub(crate) states: StateSet,
}

fn cache_fields(bus: &str, object: Value, parent: Value, item: CacheText) -> Vec<Value> {
    vec![
        object,
        object_ref(bus, ObjectId::Root),
        parent,
        item.index.into(),
        item.children.into(),
        interfaces(item.interfaces),
        item.short_name.into(),
        item.role.into(),
        item.name.into(),
        states(item.states),
    ]
}

/// An AT-SPI event's body, `siiva{sv}`: the detail, two numbers, any data
/// and an empty property map (unused by AT-SPI 2.x).
pub(crate) fn event_body(kind: &str, detail1: i32, detail2: i32, any: Value) -> Vec<Value> {
    vec![
        kind.into(),
        detail1.into(),
        detail2.into(),
        Value::variant(any),
        Value::dict(Type::Str, Type::Variant, Vec::new()).expect("an empty a{sv}"),
    ]
}

#[cfg(test)]
mod tests {
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
        // Focused is bit 12 and Visible bit 30; ReadOnly (bit 43) is in the high word.
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
}
