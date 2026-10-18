// Copyright 2022 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_unix 0.22.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/atspi/bus.rs (`emit_object_event`, `emit_window_event`,
// `emit_cache_add`, `emit_cache_remove`), and rewritten by the aterm project
// in 2026 to build aterm-dbus signals.

//! The signals a tree change sends: AT-SPI's `Event.Object` and
//! `Event.Window` events and the `Cache` object's additions and removals,
//! each as accesskit_unix sent it.

use aterm_dbus::{Message, Value};

use super::semantics::{CacheNode, NodeId, NodeIdOrRoot, ObjectEvent, Property, WindowEvent};
use super::wire::{self, CACHE_PATH, ObjectId};

const OBJECT: &str = "org.a11y.atspi.Event.Object";
const WINDOW: &str = "org.a11y.atspi.Event.Window";
const CACHE: &str = "org.a11y.atspi.Cache";

/// `interface.member` from `path`. Every name here is a constant and every
/// path the publisher builds is valid, so building one cannot fail.
fn signal(path: &str, interface: &str, member: &str, body: Vec<Value>) -> Message {
    Message::signal(path, interface, member)
        .expect("constant names and generated paths are valid")
        .with_body(body)
}

/// The signal for an object event on `target` of window `adapter`.
pub(crate) fn object_event(
    bus: &str,
    adapter: usize,
    target: NodeIdOrRoot,
    event: ObjectEvent,
) -> Message {
    let path = ObjectId::in_adapter(adapter, target).path();
    let node = |node: NodeId| ObjectId::Node { adapter, node };
    let default_any = || Value::U32(0);
    let (member, body) = match event {
        ObjectEvent::ActiveDescendantChanged(child) => (
            "ActiveDescendantChanged",
            wire::event_body("", 0, 0, wire::object_ref(bus, node(child))),
        ),
        ObjectEvent::Announcement(message, politeness) => (
            "Announcement",
            wire::event_body("", politeness as i32, 0, message.into()),
        ),
        ObjectEvent::BoundsChanged(bounds) => (
            "BoundsChanged",
            wire::event_body("", 0, 0, wire::rect(bounds)),
        ),
        ObjectEvent::CaretMoved(offset) => (
            "TextCaretMoved",
            wire::event_body("", offset, 0, default_any()),
        ),
        ObjectEvent::ChildAdded(index, child) => (
            "ChildrenChanged",
            wire::event_body(
                "add",
                i32::try_from(index).unwrap_or(i32::MAX),
                0,
                wire::object_ref(bus, node(child)),
            ),
        ),
        ObjectEvent::ChildRemoved(child) => (
            "ChildrenChanged",
            wire::event_body("remove", -1, 0, wire::object_ref(bus, node(child))),
        ),
        ObjectEvent::PropertyChanged(property) => {
            let (kind, any) = match property {
                Property::Name(name) => ("accessible-name", name.into()),
                Property::Description(text) => ("accessible-description", text.into()),
                Property::Parent(parent) => (
                    "accessible-parent",
                    wire::object_ref(bus, ObjectId::in_adapter(adapter, parent)),
                ),
                Property::Role(role) => ("accessible-role", Value::U32(role as u32)),
                Property::Value(value) => ("accessible-value", Value::F64(value)),
            };
            ("PropertyChange", wire::event_body(kind, 0, 0, any))
        }
        ObjectEvent::SelectionChanged => (
            "SelectionChanged",
            wire::event_body("", 0, 0, default_any()),
        ),
        ObjectEvent::StateChanged(state, on) => (
            "StateChanged",
            wire::event_body(state.to_static_str(), i32::from(on), 0, default_any()),
        ),
        ObjectEvent::TextInserted {
            start_index,
            length,
            content,
        } => (
            "TextChanged",
            wire::event_body("insert", start_index, length, content.into()),
        ),
        ObjectEvent::TextRemoved {
            start_index,
            length,
            content,
        } => (
            "TextChanged",
            wire::event_body("delete", start_index, length, content.into()),
        ),
        ObjectEvent::TextSelectionChanged => (
            "TextSelectionChanged",
            wire::event_body("", 0, 0, default_any()),
        ),
    };
    signal(&path, OBJECT, member, body)
}

/// The signal for a window of `adapter` becoming active or inactive; its
/// any-data is the window's name.
pub(crate) fn window_event(
    adapter: usize,
    target: NodeId,
    name: String,
    event: WindowEvent,
) -> Message {
    let path = ObjectId::Node {
        adapter,
        node: target,
    }
    .path();
    let member = match event {
        WindowEvent::Activated => "Activate",
        WindowEvent::Deactivated => "Deactivate",
    };
    signal(
        &path,
        WINDOW,
        member,
        wire::event_body("", 0, 0, name.into()),
    )
}

/// `Cache.AddAccessible` for `node`. Its body is the item's ten fields as
/// arguments, not one struct: zbus sends a struct body that way, and
/// accesskit_unix handed it the item (the recorded `cache_add` of
/// crates/aterm-dbus/tests/recorded/zbus-5.16.0-messages.tsv).
pub(crate) fn cache_add(bus: &str, node: CacheNode) -> Message {
    signal(
        CACHE_PATH,
        CACHE,
        "AddAccessible",
        wire::cache_item(bus, node),
    )
}

/// `Cache.RemoveAccessible`: the removed object's reference, as its two
/// fields `so` for the same reason as [`cache_add`]'s.
pub(crate) fn cache_remove(bus: &str, adapter: usize, node: NodeId) -> Message {
    let reference = wire::object_ref(bus, ObjectId::Node { adapter, node });
    let fields = match reference {
        Value::Struct(fields) => fields,
        other => vec![other],
    };
    signal(CACHE_PATH, CACHE, "RemoveAccessible", fields)
}
