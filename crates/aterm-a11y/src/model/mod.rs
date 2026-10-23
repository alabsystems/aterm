// Copyright 2021 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit 0.24.1 (MIT OR Apache-2.0, used under Apache-2.0)
// src/lib.rs, modified by the aterm project in 2026.
//
// Derived from Chromium's accessibility abstraction.
// Copyright 2018 The Chromium Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-CHROMIUM file.

//! Accessibility tree model: [`Node`]s, [`Role`]s, actions, properties, and [`TreeUpdate`]s.

mod geometry;

pub use geometry::{Affine, Point, Rect, Size, Vec2};

/// What a node is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    #[default]
    Unknown,
    TextRun,
    Label,
    Link,
    MenuItem,
    TextInput,
    Button,
    List,
    Switch,
    Menu,
    MultilineTextInput,
    SearchInput,
    Alert,
    AlertDialog,
    Application,
    Document,
    /// A grid of cells read by row and column.
    Grid,
    Group,
    Heading,
    ListBox,
    /// One option of a [`Role::ListBox`].
    ListBoxOption,
    MenuItemRadio,
    /// A read-only gauge of a value within a known range.
    Meter,
    Navigation,
    ProgressIndicator,
    Slider,
    Status,
    Tab,
    TabList,
    Window,

    /// Like a multi-line document, but assistive technologies should behave
    /// as for a VT-100-style terminal.
    Terminal,
}

/// An action on a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Action {
    /// Do the equivalent of a single click or tap.
    Click,

    Focus,
    Blur,

    Collapse,
    Expand,

    /// Requires [`ActionRequest::data`] to be [`ActionData::CustomAction`].
    CustomAction,

    /// Decrement a numeric value by one step.
    Decrement,
    /// Increment a numeric value by one step.
    Increment,

    HideTooltip,
    ShowTooltip,

    /// Replace the selected text with [`ActionData::Value`], as typing or
    /// pasting does.
    ReplaceSelectedText,

    /// Scroll down by the unit [`ActionData::ScrollUnit`] names.
    ScrollDown,
    /// Scroll left by the unit [`ActionData::ScrollUnit`] names.
    ScrollLeft,
    /// Scroll right by the unit [`ActionData::ScrollUnit`] names.
    ScrollRight,
    /// Scroll up by the unit [`ActionData::ScrollUnit`] names.
    ScrollUp,

    /// Scroll the node into view, where [`ActionData::ScrollHint`] says.
    ScrollIntoView,

    /// Scroll the node to [`ActionData::ScrollToPoint`] in the window.
    ScrollToPoint,

    /// Requires [`ActionRequest::data`] to be [`ActionData::SetScrollOffset`].
    SetScrollOffset,

    /// Requires [`ActionRequest::data`] to be [`ActionData::SetTextSelection`].
    SetTextSelection,

    /// Make the node the starting point of the next Tab press, without
    /// focusing it.
    SetSequentialFocusNavigationStartingPoint,

    /// Replace the value with [`ActionData::Value`] or
    /// [`ActionData::NumericValue`] and reset any selection.
    SetValue,

    ShowContextMenu,
}

impl Action {
    #[inline]
    const fn mask(self) -> u32 {
        1 << (self as u8)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Orientation {
    /// E.g. most toolbars and separators.
    Horizontal,
    /// E.g. menu or combo box.
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextDirection {
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

/// Whether a form control's input is invalid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    True,
    Grammar,
    Spelling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Toggled {
    False,
    True,
    Mixed,
}

impl From<bool> for Toggled {
    #[inline]
    fn from(b: bool) -> Self {
        match b {
            false => Self::False,
            true => Self::True,
        }
    }
}

/// How a live region's changes are announced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Live {
    Off,
    Polite,
    Assertive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HasPopup {
    Menu,
    Listbox,
    Tree,
    Grid,
    Dialog,
}

/// The stable identity of a [`Node`], unique within its tree.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct NodeId(pub u64);

impl From<u64> for NodeId {
    #[inline]
    fn from(inner: u64) -> Self {
        Self(inner)
    }
}

impl From<NodeId> for u64 {
    #[inline]
    fn from(outer: NodeId) -> Self {
        outer.0
    }
}

impl std::fmt::Debug for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// The identity of a [`Tree`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TreeId(u128);

impl TreeId {
    /// The root tree.
    pub const ROOT: Self = Self(0);
}

/// A position in a text document: a [`Role::TextRun`] node and an index
/// into its [`Node::character_lengths`] (their count at the run's end).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPosition {
    pub node: NodeId,
    pub character_index: usize,
}

/// A text selection; a caret when `anchor` equals `focus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSelection {
    /// Where the selection started.
    pub anchor: TextPosition,
    /// The selection's moving end, and the caret.
    pub focus: TextPosition,
}

/// One accessible object in a window's UI tree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    role: Role,
    actions: u32,
    action_context: Option<ActionContext>,
    disabled: bool,
    read_only: bool,
    busy: bool,
    offscreen: bool,
    line_breaking: bool,
    children: Vec<NodeId>,
    label: Option<Box<str>>,
    description: Option<Box<str>>,
    value: Option<Box<str>>,
    url: Option<Box<str>>,
    bounds: Option<Rect>,
    transform: Option<Box<Affine>>,
    text_selection: Option<Box<TextSelection>>,
    character_lengths: Box<[u8]>,
    word_starts: Box<[u8]>,
    character_positions: Option<Box<[f32]>>,
    character_widths: Option<Box<[f32]>>,
    text_direction: Option<TextDirection>,
    numeric_value: Option<f64>,
    min_numeric_value: Option<f64>,
    max_numeric_value: Option<f64>,
    numeric_value_step: Option<f64>,
    toggled: Option<Toggled>,
    has_popup: Option<HasPopup>,
    invalid: Option<Invalid>,
    live: Option<Live>,
    selected: Option<bool>,
    expanded: Option<bool>,
}

/// A copied property: `getter`, `setter` and `clearer`.
macro_rules! copy_property {
    ($(#[$doc:meta])* $field:ident: $ty:ty, $getter:ident, $setter:ident, $clearer:ident) => {
        $(#[$doc])*
        #[inline]
        pub const fn $getter(&self) -> Option<$ty> {
            self.$field
        }
        #[inline]
        pub const fn $setter(&mut self, value: $ty) {
            self.$field = Some(value);
        }
        #[inline]
        pub const fn $clearer(&mut self) {
            self.$field = None;
        }
    };
}

/// A string property.
macro_rules! string_property {
    ($(#[$doc:meta])* $field:ident, $setter:ident, $clearer:ident) => {
        $(#[$doc])*
        #[inline]
        pub fn $field(&self) -> Option<&str> {
            self.$field.as_deref()
        }
        #[inline]
        pub fn $setter(&mut self, value: impl Into<Box<str>>) {
            self.$field = Some(value.into());
        }
        #[inline]
        pub fn $clearer(&mut self) {
            self.$field = None;
        }
    };
}

/// A flag: `getter`, `setter` and `clearer`.
macro_rules! flag {
    ($(#[$doc:meta])* $field:ident, $getter:ident, $setter:ident, $clearer:ident) => {
        $(#[$doc])*
        #[inline]
        pub const fn $getter(&self) -> bool {
            self.$field
        }
        #[inline]
        pub const fn $setter(&mut self) {
            self.$field = true;
        }
        #[inline]
        pub const fn $clearer(&mut self) {
            self.$field = false;
        }
    };
}

impl Node {
    #[inline]
    pub fn new(role: Role) -> Self {
        Self {
            role,
            ..Default::default()
        }
    }

    #[inline]
    pub const fn role(&self) -> Role {
        self.role
    }

    #[inline]
    pub const fn set_role(&mut self, value: Role) {
        self.role = value;
    }

    #[inline]
    pub const fn supports_action(&self, action: Action) -> bool {
        (self.actions & action.mask()) != 0
    }

    #[inline]
    pub const fn add_action(&mut self, action: Action) {
        self.actions |= action.mask();
    }

    #[inline]
    pub const fn remove_action(&mut self, action: Action) {
        self.actions &= !(action.mask());
    }

    #[inline]
    pub const fn clear_actions(&mut self) {
        self.actions = 0;
    }

    flag! {
        /// Use for a control or group of controls that disallows input.
        disabled, is_disabled, set_disabled, clear_disabled
    }
    flag! {
        /// Use for a text widget that allows focus and selection but not input.
        read_only, is_read_only, set_read_only, clear_read_only
    }
    flag! {
        busy, is_busy, set_busy, clear_busy
    }
    flag! {
        /// Whether this node is held off screen by its scrolling container.
        offscreen, is_offscreen, set_offscreen, clear_offscreen
    }
    flag! {
        /// Whether this node causes a hard line break.
        line_breaking,
        is_line_breaking_object,
        set_is_line_breaking_object,
        clear_is_line_breaking_object
    }

    #[inline]
    pub fn children(&self) -> &[NodeId] {
        &self.children
    }

    #[inline]
    pub fn set_children(&mut self, value: impl Into<Vec<NodeId>>) {
        self.children = value.into();
    }

    #[inline]
    pub fn push_child(&mut self, item: NodeId) {
        self.children.push(item);
    }

    #[inline]
    pub fn clear_children(&mut self) {
        self.children.clear();
    }

    string_property! {
        /// The label of a control that can have one.
        label, set_label, clear_label
    }
    string_property! {
        description, set_description, clear_description
    }
    string_property! {
        value, set_value, clear_value
    }
    string_property! {
        url, set_url, clear_url
    }

    copy_property! {
        /// The bounding box of this node in its coordinate space.
        bounds: Rect, bounds, set_bounds, clear_bounds
    }

    /// Transform applied to coordinates within this node and its descendants (`None` for identity).
    #[inline]
    pub fn transform(&self) -> Option<&Affine> {
        self.transform.as_deref()
    }

    #[inline]
    pub fn set_transform(&mut self, value: impl Into<Box<Affine>>) {
        self.transform = Some(value.into());
    }

    #[inline]
    pub fn clear_transform(&mut self) {
        self.transform = None;
    }

    #[inline]
    pub fn text_selection(&self) -> Option<&TextSelection> {
        self.text_selection.as_deref()
    }

    #[inline]
    pub fn set_text_selection(&mut self, value: impl Into<Box<TextSelection>>) {
        self.text_selection = Some(value.into());
    }

    #[inline]
    pub fn clear_text_selection(&mut self) {
        self.text_selection = None;
    }

    /// Length in UTF-8 bytes of each selectable character in a text run.
    #[inline]
    pub fn character_lengths(&self) -> &[u8] {
        &self.character_lengths
    }

    #[inline]
    pub fn set_character_lengths(&mut self, value: impl Into<Box<[u8]>>) {
        self.character_lengths = value.into();
    }

    #[inline]
    pub fn clear_character_lengths(&mut self) {
        self.character_lengths = Box::default();
    }

    /// Sorted character indices that start a word in a text run.
    #[inline]
    pub fn word_starts(&self) -> &[u8] {
        &self.word_starts
    }

    #[inline]
    pub fn set_word_starts(&mut self, value: impl Into<Box<[u8]>>) {
        self.word_starts = value.into();
    }

    #[inline]
    pub fn clear_word_starts(&mut self) {
        self.word_starts = Box::default();
    }

    /// Each character's position along [`Node::text_direction`] within the node's bounds.
    #[inline]
    pub fn character_positions(&self) -> Option<&[f32]> {
        self.character_positions.as_deref()
    }

    #[inline]
    pub fn set_character_positions(&mut self, value: impl Into<Box<[f32]>>) {
        self.character_positions = Some(value.into());
    }

    #[inline]
    pub fn clear_character_positions(&mut self) {
        self.character_positions = None;
    }

    /// Each character's advance along [`Node::text_direction`].
    #[inline]
    pub fn character_widths(&self) -> Option<&[f32]> {
        self.character_widths.as_deref()
    }

    #[inline]
    pub fn set_character_widths(&mut self, value: impl Into<Box<[f32]>>) {
        self.character_widths = Some(value.into());
    }

    #[inline]
    pub fn clear_character_widths(&mut self) {
        self.character_widths = None;
    }

    copy_property! {
        text_direction: TextDirection, text_direction, set_text_direction, clear_text_direction
    }
    copy_property! {
        /// The owner and authorization captured by this publication.
        action_context: ActionContext, action_context, set_action_context, clear_action_context
    }
    copy_property! {
        numeric_value: f64, numeric_value, set_numeric_value, clear_numeric_value
    }
    copy_property! {
        min_numeric_value: f64, min_numeric_value, set_min_numeric_value, clear_min_numeric_value
    }
    copy_property! {
        max_numeric_value: f64, max_numeric_value, set_max_numeric_value, clear_max_numeric_value
    }
    copy_property! {
        numeric_value_step: f64, numeric_value_step, set_numeric_value_step,
        clear_numeric_value_step
    }
    copy_property! {
        toggled: Toggled, toggled, set_toggled, clear_toggled
    }
    copy_property! {
        has_popup: HasPopup, has_popup, set_has_popup, clear_has_popup
    }
    copy_property! {
        invalid: Invalid, invalid, set_invalid, clear_invalid
    }
    copy_property! {
        live: Live, live, set_live, clear_live
    }
    copy_property! {
        /// Whether this node is selected (`None` where selection does not apply).
        selected: bool, is_selected, set_selected, clear_selected
    }
    copy_property! {
        /// Whether this node is expanded (`None` where it neither expands nor collapses).
        expanded: bool, is_expanded, set_expanded, clear_expanded
    }
}

/// What is global to a tree rather than to one node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    /// The root node's id.
    pub root: NodeId,
    /// The name of the UI toolkit in use.
    pub toolkit_name: Option<String>,
    /// The version of the UI toolkit.
    pub toolkit_version: Option<String>,
}

impl Tree {
    #[inline]
    pub const fn new(root: NodeId) -> Tree {
        Tree {
            root,
            toolkit_name: None,
            toolkit_version: None,
        }
    }
}

/// An atomic change to a [`Tree`], from its previous state to its next.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeUpdate {
    /// The new and changed nodes, in any order.
    pub nodes: Vec<(NodeId, Node)>,

    /// The tree's data: required in a tree's first update, optional after.
    pub tree: Option<Tree>,

    /// The tree this update applies to: always [`TreeId::ROOT`].
    pub tree_id: TreeId,

    /// The node with the keyboard focus while the window has it (the root
    /// when no other node does), in every update.
    pub focus: NodeId,
}

/// How far a `Scroll*` action scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollUnit {
    /// One item, line or character, or about that.
    Item,
    /// What fits in the viewport.
    Page,
}

/// Where a node scrolled into view should land in its scrolling container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollHint {
    TopLeft,
    BottomRight,
    TopEdge,
    BottomEdge,
    LeftEdge,
    RightEdge,
}

/// What an [`ActionRequest`] carries.
#[derive(Clone, Debug, PartialEq)]
pub enum ActionData {
    CustomAction(i32),
    Value(Box<str>),
    NumericValue(f64),
    ScrollUnit(ScrollUnit),
    /// Where [`Action::ScrollIntoView`] should put the node.
    ScrollHint(ScrollHint),
    /// For [`Action::ScrollToPoint`]: a point relative to the window.
    ScrollToPoint(Point),
    /// For [`Action::SetScrollOffset`]: in the node's coordinate space.
    SetScrollOffset(Point),
    SetTextSelection(TextSelection),
}

/// Lossless context captured by a publication before its action is queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActionContext {
    domain: ActionDomain,
    owner: u64,
    generation: u64,
    review_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ActionDomain {
    Native,
    Message,
    MessageLog,
    Tab,
}

impl ActionContext {
    /// The host's stable view identity and current lifecycle generation.
    #[inline]
    pub const fn new(owner: u64, generation: u64) -> Self {
        Self::native_review(owner, generation, 0)
    }

    /// One native review within a live view.
    #[inline]
    pub const fn native_review(owner: u64, generation: u64, review_revision: u64) -> Self {
        Self {
            domain: ActionDomain::Native,
            owner,
            generation,
            review_revision,
        }
    }

    /// A message's exact id and checked action-list revision, captured together.
    #[inline]
    pub const fn message(owner: u64, generation: u64) -> Self {
        Self {
            domain: ActionDomain::Message,
            owner,
            generation,
            review_revision: 0,
        }
    }

    /// The message log's link, which does not authorize any individual row's action.
    #[inline]
    pub const fn message_log() -> Self {
        Self {
            domain: ActionDomain::MessageLog,
            owner: 0,
            generation: 0,
            review_revision: 0,
        }
    }

    /// A tab's stable identity, independent of its current position in the strip.
    #[inline]
    pub const fn tab(owner: u64) -> Self {
        Self {
            domain: ActionDomain::Tab,
            owner,
            generation: 0,
            review_revision: 0,
        }
    }

    /// Stable owner identity, without truncation or hashing.
    #[inline]
    pub const fn owner(self) -> u64 {
        self.owner
    }

    /// Native lifecycle generation or message action-list revision.
    #[inline]
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// An action an assistive technology asks of a node.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionRequest {
    /// The target node's owner and authorization when the action was created.
    pub context: Option<ActionContext>,
    pub action: Action,
    pub target_tree: TreeId,
    pub target_node: NodeId,
    pub data: Option<ActionData>,
}

/// Turns a window's accessibility on.
pub trait ActivationHandler {
    /// Return the initial full tree if available now, or `None` to deliver it on the next update.
    fn request_initial_tree(&mut self) -> Option<TreeUpdate>;
}

/// Carries out what assistive technologies ask.
pub trait ActionHandler {
    /// Perform `request`, or do nothing if its action is not supported.
    fn do_action(&mut self, request: ActionRequest);
}

/// Turns a window's accessibility off.
pub trait DeactivationHandler {
    /// Drop cached accessibility state until the next activation.
    fn deactivate_accessibility(&mut self);
}

#[cfg(test)]
mod action_context_tests {
    use super::ActionContext;

    #[test]
    fn native_review_identity_preserves_lifecycle_and_never_aliases_other_contexts() {
        let review = ActionContext::native_review(u64::MAX, u64::MAX, 9);
        assert_eq!(review.owner(), u64::MAX);
        assert_eq!(review.generation(), u64::MAX);
        assert_eq!(review, ActionContext::native_review(u64::MAX, u64::MAX, 9));
        assert_ne!(review, ActionContext::new(u64::MAX, u64::MAX));
        assert_ne!(review, ActionContext::native_review(u64::MAX, u64::MAX, 10));
        assert_ne!(review, ActionContext::native_review(u64::MAX, 1, 9));
        assert_ne!(review, ActionContext::message(u64::MAX, u64::MAX));
        assert_ne!(review, ActionContext::tab(u64::MAX));
    }
}
