// Copyright 2022, 2023 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_atspi_common 0.19.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/lib.rs, src/action.rs, src/callback.rs, src/error.rs,
// src/events.rs and src/rect.rs, and cut by the aterm project in 2026 to
// plain values: no serde or zvariant derives (the publisher builds its wire
// values in `wire`).

//! AT-SPI tree semantics without a bus (`adapter`, `node`, `context`, and `util`).

mod adapter;
mod context;
mod node;
mod util;

use std::fmt;

pub(crate) use super::types::{
    CoordType, Granularity, Interface, InterfaceSet, Layer, Politeness, RelationType, Role,
    ScrollType, State, StateSet,
};
pub(crate) use adapter::{Adapter, next_adapter_id};
pub(crate) use context::{ActionHandlerNoMut, ActionHandlerWrapper, AppContext, SharedLock};
pub(crate) use node::{CacheNode, NodeIdOrRoot, NodeIdentity, PlatformNode, PlatformRoot};
pub(crate) use util::WindowBounds;

/// One AT-SPI action of a node: its name, description and key binding.
pub(crate) struct Action {
    pub(crate) localized_name: String,
    pub(crate) description: String,
    pub(crate) key_binding: String,
}

/// Where a window's tree reports its registrations and events.
pub(crate) trait AdapterCallback {
    fn register_interfaces(&self, adapter: &Adapter, id: NodeIdentity, interfaces: InterfaceSet);
    fn unregister_interfaces(&self, adapter: &Adapter, id: NodeIdentity, interfaces: InterfaceSet);
    fn emit_event(&self, adapter: &Adapter, event: Event);
}

/// Why a node cannot answer.
#[derive(Debug)]
pub(crate) enum Error {
    Defunct,
    UnsupportedInterface,
    TooManyChildren,
    IndexOutOfRange,
    TooManyCharacters,
    UnsupportedTextGranularity,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Defunct => "defunct",
            Self::UnsupportedInterface => "unsupported interface",
            Self::TooManyChildren => "too many children",
            Self::IndexOutOfRange => "index out of range",
            Self::TooManyCharacters => "too many characters",
            Self::UnsupportedTextGranularity => "unsupported text granularity",
        })
    }
}

impl std::error::Error for Error {}

pub(crate) type Result<T> = std::result::Result<T, Error>;

/// An AT-SPI rectangle: integer screen, window or parent coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

impl Rect {
    pub(crate) const INVALID: Rect = Rect {
        x: -1,
        y: -1,
        width: -1,
        height: -1,
    };
}

impl From<crate::model::Rect> for Rect {
    fn from(value: crate::model::Rect) -> Rect {
        Rect {
            x: value.x0 as i32,
            y: value.y0 as i32,
            width: value.width() as i32,
            height: value.height() as i32,
        }
    }
}

/// An event a tree update sends.
#[derive(Debug)]
pub(crate) enum Event {
    Object {
        target: NodeIdOrRoot,
        event: ObjectEvent,
    },
    Window {
        target: NodeIdentity,
        name: String,
        event: WindowEvent,
    },
    Cache(CacheEvent),
}

#[derive(Debug)]
pub(crate) enum CacheEvent {
    Added(NodeIdentity),
    Removed(NodeIdentity),
}

#[derive(Debug)]
pub(crate) enum Property {
    Name(String),
    Description(String),
    Parent(NodeIdOrRoot),
    Role(Role),
    Value(f64),
}

#[allow(
    clippy::enum_variant_names,
    reason = "AT-SPI's event names, as accesskit_atspi_common spelled them"
)]
#[derive(Debug)]
pub(crate) enum ObjectEvent {
    ActiveDescendantChanged(NodeIdentity),
    Announcement(String, Politeness),
    BoundsChanged(Rect),
    CaretMoved(i32),
    ChildAdded(usize, NodeIdentity),
    ChildRemoved(NodeIdentity),
    PropertyChanged(Property),
    SelectionChanged,
    StateChanged(State, bool),
    TextInserted {
        start_index: i32,
        length: i32,
        content: String,
    },
    TextRemoved {
        start_index: i32,
        length: i32,
        content: String,
    },
    TextSelectionChanged,
}

#[derive(Debug)]
pub(crate) enum WindowEvent {
    Activated,
    Deactivated,
}
