// Copyright 2022 Tait Hoyem and the atspi contributors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from atspi-common 0.13.0 (Apache-2.0 OR MIT, used under
// Apache-2.0) src/interface.rs, src/state.rs, src/role.rs,
// src/relation_type.rs and src/lib.rs, and cut by the aterm project in 2026
// to the values aterm's publisher sends, as plain enums and bit sets in
// place of serde and enumflags2.

//! AT-SPI's vocabulary: interfaces, states, roles, relation types and the
//! enums its methods take. Each value is AT-SPI's number for it (an
//! interface or a state is a bit, its index in the specification's list),
//! so the sets and casts here are what goes on the wire. Only the values
//! aterm's semantics produce or read are named.

use std::ops::{BitAnd, BitOr, BitXor};

/// An AT-SPI interface, as the bit it is in an interface set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub(crate) enum Interface {
    Accessible = 0,
    Action = 1,
    Application = 2,
    Component = 5,
    Hyperlink = 10,
    Selection = 14,
    Text = 18,
    Value = 19,
}

impl Interface {
    const ALL: [Interface; 8] = [
        Interface::Accessible,
        Interface::Action,
        Interface::Application,
        Interface::Component,
        Interface::Hyperlink,
        Interface::Selection,
        Interface::Text,
        Interface::Value,
    ];

    fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

/// A set of interfaces, iterated in bit order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct InterfaceSet(u32);

impl InterfaceSet {
    pub(crate) fn new(value: impl Into<InterfaceSet>) -> Self {
        value.into()
    }

    pub(crate) fn bits(&self) -> u32 {
        self.0
    }

    pub(crate) fn contains(self, other: impl Into<InterfaceSet>) -> bool {
        let other = other.into();
        self.0 & other.0 == other.0
    }

    pub(crate) fn insert(&mut self, other: impl Into<InterfaceSet>) {
        self.0 |= other.into().0;
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = Interface> + use<> {
        let bits = self.0;
        Interface::ALL
            .into_iter()
            .filter(move |i| bits & i.bit() != 0)
    }
}

impl From<Interface> for InterfaceSet {
    fn from(value: Interface) -> Self {
        Self(value.bit())
    }
}

impl FromIterator<Interface> for InterfaceSet {
    fn from_iter<I: IntoIterator<Item = Interface>>(iter: I) -> Self {
        Self(iter.into_iter().fold(0, |bits, i| bits | i.bit()))
    }
}

impl BitOr for Interface {
    type Output = InterfaceSet;

    fn bitor(self, other: Interface) -> InterfaceSet {
        InterfaceSet(self.bit() | other.bit())
    }
}

impl BitOr<Interface> for InterfaceSet {
    type Output = InterfaceSet;

    fn bitor(self, other: Interface) -> InterfaceSet {
        InterfaceSet(self.0 | other.bit())
    }
}

impl BitAnd for InterfaceSet {
    type Output = InterfaceSet;

    fn bitand(self, other: InterfaceSet) -> InterfaceSet {
        InterfaceSet(self.0 & other.0)
    }
}

impl BitXor for InterfaceSet {
    type Output = InterfaceSet;

    fn bitxor(self, other: InterfaceSet) -> InterfaceSet {
        InterfaceSet(self.0 ^ other.0)
    }
}

/// An AT-SPI state, as the bit it is in a state set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u64)]
pub(crate) enum State {
    Active = 1,
    Checked = 4,
    Defunct = 6,
    Editable = 7,
    Enabled = 8,
    Focusable = 11,
    Focused = 12,
    Horizontal = 14,
    MultiLine = 17,
    Pressed = 20,
    Selectable = 22,
    Selected = 23,
    Sensitive = 24,
    Showing = 25,
    SingleLine = 26,
    Vertical = 29,
    Visible = 30,
    Indeterminate = 32,
    SelectableText = 38,
    Checkable = 41,
    ReadOnly = 43,
}

impl State {
    const ALL: [State; 21] = [
        State::Active,
        State::Checked,
        State::Defunct,
        State::Editable,
        State::Enabled,
        State::Focusable,
        State::Focused,
        State::Horizontal,
        State::MultiLine,
        State::Pressed,
        State::Selectable,
        State::Selected,
        State::Sensitive,
        State::Showing,
        State::SingleLine,
        State::Vertical,
        State::Visible,
        State::Indeterminate,
        State::SelectableText,
        State::Checkable,
        State::ReadOnly,
    ];

    fn bit(self) -> u64 {
        1 << (self as u64)
    }

    /// The state's name in a `StateChanged` event's detail.
    pub(crate) fn to_static_str(self) -> &'static str {
        match self {
            State::Active => "active",
            State::Checked => "checked",
            State::Defunct => "defunct",
            State::Editable => "editable",
            State::Enabled => "enabled",
            State::Focusable => "focusable",
            State::Focused => "focused",
            State::Horizontal => "horizontal",
            State::MultiLine => "multi-line",
            State::Pressed => "pressed",
            State::Selectable => "selectable",
            State::Selected => "selected",
            State::Sensitive => "sensitive",
            State::Showing => "showing",
            State::SingleLine => "single-line",
            State::Vertical => "vertical",
            State::Visible => "visible",
            State::Indeterminate => "indeterminate",
            State::SelectableText => "selectable-text",
            State::Checkable => "checkable",
            State::ReadOnly => "read-only",
        }
    }
}

/// A set of states, iterated in bit order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct StateSet(u64);

impl StateSet {
    pub(crate) fn empty() -> Self {
        Self(0)
    }

    pub(crate) fn bits(&self) -> u64 {
        self.0
    }

    pub(crate) fn contains(self, other: impl Into<StateSet>) -> bool {
        let other = other.into();
        self.0 & other.0 == other.0
    }

    pub(crate) fn insert(&mut self, other: impl Into<StateSet>) {
        self.0 |= other.into().0;
    }

    pub(crate) fn iter(self) -> impl Iterator<Item = State> {
        State::ALL
            .into_iter()
            .filter(move |s| self.0 & s.bit() != 0)
    }
}

impl From<State> for StateSet {
    fn from(value: State) -> Self {
        Self(value.bit())
    }
}

impl BitOr for State {
    type Output = StateSet;

    fn bitor(self, other: State) -> StateSet {
        StateSet(self.bit() | other.bit())
    }
}

impl BitOr<State> for StateSet {
    type Output = StateSet;

    fn bitor(self, other: State) -> StateSet {
        StateSet(self.0 | other.bit())
    }
}

impl BitXor for StateSet {
    type Output = StateSet;

    fn bitxor(self, other: StateSet) -> StateSet {
        StateSet(self.0 ^ other.0)
    }
}

/// An AT-SPI role, as its number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub(crate) enum Role {
    Alert = 2,
    Frame = 23,
    Label = 29,
    List = 31,
    Menu = 33,
    MenuItem = 35,
    PageTab = 37,
    PageTabList = 38,
    Panel = 39,
    ProgressBar = 42,
    Button = 43,
    RadioMenuItem = 45,
    Slider = 51,
    StatusBar = 54,
    Terminal = 60,
    ToggleButton = 62,
    Unknown = 67,
    Application = 75,
    Embedded = 78,
    Entry = 79,
    DocumentFrame = 82,
    Heading = 83,
    Link = 88,
    ListBox = 98,
    Notification = 101,
    Landmark = 110,
    Static = 116,
}

/// An AT-SPI relation type. aterm's model has no relations (a control's
/// `controls` was accesskit_atspi_common's only source, for
/// `ControllerFor`), so a relation set is always empty and no type is named.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RelationType {}

/// The coordinates a point or an extent is given in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoordType {
    Screen,
    Window,
    Parent,
}

/// What `GetStringAtOffset` returns the text of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Granularity {
    Char,
    Word,
    Sentence,
    Line,
    Paragraph,
}

/// A component's layer: aterm's are all widgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layer {
    Widget = 3,
}

/// Where `ScrollTo` puts the object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollType {
    TopLeft,
    BottomRight,
    TopEdge,
    BottomEdge,
    LeftEdge,
    RightEdge,
    Anywhere,
}

/// How urgently an announcement is spoken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub(crate) enum Politeness {
    None = 0,
    Polite = 1,
    Assertive = 2,
}
