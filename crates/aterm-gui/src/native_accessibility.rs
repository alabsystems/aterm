// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AccessKit projection for first-party native tab applications.
//!
//! The input is the already-compiled [`CompiledUi`] semantic snapshot. This adapter never
//! walks [`crate::native_ui::UiTree`], performs layout, or consults app model side tables.
//! Consequently pixels, hit testing, introspection, and accessibility share the exact same
//! stable keys, clipped bounds, values, and state.

#![allow(
    dead_code,
    reason = "native AccessKit host wiring lands with the tab-app window adapter"
)]

use std::collections::HashSet;
use std::ops::Range;

// FxHash, not SipHash, for the per-frame projection side tables below. They are
// keyed by internal `UiKey`s and by ids this module derived itself — never by
// untrusted input — and they are rebuilt from scratch on every presented native
// frame, so the HashDoS resistance was pure per-lookup cost. No map here is ever
// iterated (every use is `insert`/`get`/`contains_key`), so published node ids
// and tree order — which come from `stable_node_id` and the `compiled.semantics`
// walk — are unchanged.
use aterm_hash::{FxHashMap, FxHashSet};

use accesskit::{
    Action, Affine, Invalid, Live, Node, NodeId, Rect, Role, TextPosition, TextSelection, Toggled,
    Tree, TreeId, TreeUpdate,
};

use crate::native_ui::{
    ActionId, CompiledUi, ControlState, LogicalRect, SemanticNode, SemanticRole, SemanticValue,
    TextViewportSpec, UiContent, UiKey, text_viewport_geometry,
};
use crate::tab_model::ViewId;

/// A single platform range request is bounded even when its document is enormous.
pub(crate) const MAX_VIRTUAL_TEXT_RANGE: u64 = 1024 * 1024;

/// Deterministically derive an AccessKit identity from a stable semantic key.
///
/// FNV-1a is used only as a stable identifier projection, never as a security primitive.
/// A collision inside one projected tree is detected and fails the whole projection.
pub(crate) fn stable_node_id(key: &UiKey) -> NodeId {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in b"aterm/native-a11y/v1\0"
        .iter()
        .chain(key.as_str().as_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Keep zero available to legacy root trees and ensure every native key has a concrete id.
    NodeId(hash.max(1))
}

/// Deterministically namespace a semantic identity by its stable native view.
///
/// Native split siblings are allowed to render the same app (and therefore the
/// same `UiKey`s).  A view-qualified id keeps those independently actionable
/// trees stable without making sibling order or focus part of identity.
pub(crate) fn stable_node_id_for_view(view: ViewId, key: &UiKey) -> NodeId {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in b"aterm/native-a11y/view/v1\0"
        .iter()
        .chain(view.get().to_le_bytes().iter())
        .chain(key.as_str().as_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    NodeId(hash.max(1))
}

fn composite_root_id() -> NodeId {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in b"aterm/native-a11y/window/v1" {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    NodeId(hash.max(1))
}

/// Lifecycle stamp attached to every route in a published native leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AccessibilityOwner {
    pub(crate) view: ViewId,
    pub(crate) generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccessibilityRoute {
    pub(crate) node: NodeId,
    pub(crate) key: UiKey,
    /// Reducer action for Click/SetValue/etc.; structural nodes legitimately have none.
    pub(crate) action: Option<ActionId>,
    pub(crate) role: SemanticRole,
    owner: Option<AccessibilityOwner>,
    supported: Vec<Action>,
}

impl AccessibilityRoute {
    pub(crate) fn supports(&self, action: Action) -> bool {
        self.supported.contains(&action)
    }
}

/// One virtual text surface. The host resolves ranges against `document_key`; the adapter
/// deliberately does not materialize document lines as accessibility nodes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VirtualTextTarget {
    pub(crate) node: NodeId,
    pub(crate) key: UiKey,
    pub(crate) document_key: String,
    pub(crate) visible_bounds: LogicalRect,
    /// Smallest canonical UTF-8 byte interval covering the projected rows.
    /// Individual line ranges below remain authoritative when a pathological
    /// long line was windowed and therefore leaves a gap in this envelope.
    pub(crate) visible_range: Range<u64>,
    pub(crate) lines: Vec<VirtualTextLine>,
    pub(crate) primary_selection: Option<VirtualTextSelection>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VirtualTextLine {
    pub(crate) node: NodeId,
    pub(crate) key: UiKey,
    pub(crate) source: Range<u64>,
    pub(crate) text: String,
    /// One canonical byte offset per AccessKit character, plus the end offset.
    pub(crate) character_source_offsets: Vec<u64>,
    pub(crate) character_positions: Vec<f32>,
    pub(crate) character_widths: Vec<f32>,
    pub(crate) selections: Vec<VirtualTextSelectionSpan>,
    pub(crate) carets: Vec<VirtualTextCaret>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VirtualTextSelectionSpan {
    pub(crate) source: Range<u64>,
    pub(crate) primary: bool,
    pub(crate) continues: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VirtualTextCaret {
    pub(crate) source_byte: u64,
    pub(crate) primary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VirtualTextSelection {
    pub(crate) anchor_byte: u64,
    pub(crate) focus_byte: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VirtualTextRequest {
    pub(crate) node: NodeId,
    pub(crate) key: UiKey,
    pub(crate) document_key: String,
    /// UTF-8 byte range in the immutable document snapshot selected by the host.
    pub(crate) range: Range<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VirtualTextError {
    UnknownNode,
    ReversedRange,
    RangeTooLarge,
}

/// Pure projection product. `update` can be sent directly to an AccessKit adapter; routes
/// carry platform action requests back to stable `UiKey`s.
#[derive(Debug, PartialEq)]
pub(crate) struct NativeAccessibilityProjection {
    update: TreeUpdate,
    routes: Vec<AccessibilityRoute>,
    virtual_text: Vec<VirtualTextTarget>,
    source_nodes_visited: usize,
}

impl NativeAccessibilityProjection {
    pub(crate) fn update(&self) -> &TreeUpdate {
        &self.update
    }

    pub(crate) fn into_update(self) -> TreeUpdate {
        self.update
    }

    pub(crate) fn into_update_and_routes(self) -> (TreeUpdate, Vec<AccessibilityRoute>) {
        (self.update, self.routes)
    }

    pub(crate) fn into_update_routes_and_virtual_text(
        self,
    ) -> (TreeUpdate, Vec<AccessibilityRoute>, Vec<VirtualTextTarget>) {
        (self.update, self.routes, self.virtual_text)
    }

    pub(crate) fn routes(&self) -> &[AccessibilityRoute] {
        &self.routes
    }

    pub(crate) fn virtual_text(&self) -> &[VirtualTextTarget] {
        &self.virtual_text
    }

    /// Diagnostic witness that the canonical semantic vector was consumed once.
    pub(crate) fn source_nodes_visited(&self) -> usize {
        self.source_nodes_visited
    }

    pub(crate) fn id_for_key(&self, key: &UiKey) -> Option<NodeId> {
        self.routes
            .iter()
            .find(|route| &route.key == key)
            .map(|route| route.node)
    }

    pub(crate) fn route_for_node(&self, node: NodeId) -> Option<&AccessibilityRoute> {
        self.routes.iter().find(|route| route.node == node)
    }

    /// Build a bounded range request for the host's virtual text provider.
    pub(crate) fn request_virtual_text(
        &self,
        node: NodeId,
        range: Range<u64>,
    ) -> Result<VirtualTextRequest, VirtualTextError> {
        if range.start > range.end {
            return Err(VirtualTextError::ReversedRange);
        }
        if range.end - range.start > MAX_VIRTUAL_TEXT_RANGE {
            return Err(VirtualTextError::RangeTooLarge);
        }
        let target = self
            .virtual_text
            .iter()
            .find(|target| target.node == node)
            .ok_or(VirtualTextError::UnknownNode)?;
        Ok(VirtualTextRequest {
            node,
            key: target.key.clone(),
            document_key: target.document_key.clone(),
            range,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AccessibilityProjectionError {
    EmptyTree,
    MultipleRoots,
    DuplicateKey(UiKey),
    MissingParent { node: UiKey, parent: UiKey },
    IdCollision { first: UiKey, second: UiKey },
    MultipleFocusedNodes,
    InvalidBounds(UiKey),
    InvalidNumber(UiKey),
    InvalidContainerTransform,
    InvalidCompositeBounds,
    DuplicateCompositeView(ViewId),
    CompositeIdCollision(NodeId),
    MissingVirtualDocument(UiKey),
    InvalidVirtualText(UiKey),
    InvalidTextInput(UiKey),
}

/// Projection staged from the exact `CompiledUi` used for the pending native frame.
pub(crate) struct StagedNativeAccessibility {
    pub(crate) owners: Vec<AccessibilityOwner>,
    pub(crate) projection: NativeAccessibilityProjection,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PublishedNativeAccessibility {
    /// Primary owner retained for compatibility with single-native callers.
    /// Composite routing always consults the node-qualified owner below.
    pub(crate) view: ViewId,
    pub(crate) generation: u64,
    owners: Vec<AccessibilityOwner>,
    routes: Vec<AccessibilityRoute>,
    virtual_text: Vec<VirtualTextTarget>,
    qualified_routes: bool,
}

impl PublishedNativeAccessibility {
    pub(crate) fn new(view: ViewId, generation: u64, routes: Vec<AccessibilityRoute>) -> Self {
        let owner = AccessibilityOwner { view, generation };
        Self {
            view,
            generation,
            owners: vec![owner],
            routes: routes
                .into_iter()
                .map(|mut route| {
                    route.owner.get_or_insert(owner);
                    route
                })
                .collect(),
            virtual_text: Vec::new(),
            qualified_routes: false,
        }
    }

    pub(crate) fn with_virtual_text(
        view: ViewId,
        generation: u64,
        routes: Vec<AccessibilityRoute>,
        virtual_text: Vec<VirtualTextTarget>,
    ) -> Self {
        let owner = AccessibilityOwner { view, generation };
        Self {
            view,
            generation,
            owners: vec![owner],
            routes: routes
                .into_iter()
                .map(|mut route| {
                    route.owner.get_or_insert(owner);
                    route
                })
                .collect(),
            virtual_text,
            qualified_routes: false,
        }
    }

    pub(crate) fn composite(
        primary: AccessibilityOwner,
        owners: Vec<AccessibilityOwner>,
        routes: Vec<AccessibilityRoute>,
        virtual_text: Vec<VirtualTextTarget>,
    ) -> Self {
        Self {
            view: primary.view,
            generation: primary.generation,
            owners,
            routes,
            virtual_text,
            qualified_routes: true,
        }
    }

    pub(crate) fn route(&self, node: NodeId) -> Option<&AccessibilityRoute> {
        self.routes.iter().find(|route| route.node == node)
    }

    pub(crate) fn route_owner(&self, node: NodeId) -> Option<AccessibilityOwner> {
        let route = self.route(node)?;
        let owner = route.owner.or_else(|| {
            (self.owners.len() == 1).then_some(AccessibilityOwner {
                view: self.view,
                generation: self.generation,
            })
        })?;
        (!self.qualified_routes || stable_node_id_for_view(owner.view, &route.key) == node)
            .then_some(owner)
    }

    pub(crate) fn owners(&self) -> &[AccessibilityOwner] {
        &self.owners
    }

    #[cfg(test)]
    pub(crate) fn retag_route_for_test(&mut self, node: NodeId, owner: AccessibilityOwner) -> bool {
        let Some(route) = self.routes.iter_mut().find(|route| route.node == node) else {
            return false;
        };
        route.owner = Some(owner);
        true
    }

    pub(crate) fn virtual_text(&self) -> &[VirtualTextTarget] {
        &self.virtual_text
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RoutedAccessibilityAction {
    Focus {
        key: UiKey,
    },
    Activate {
        key: UiKey,
        action: ActionId,
        value: Option<crate::native_app::SemanticInput>,
    },
    Scroll {
        key: UiKey,
        lines: i32,
    },
    ReplaceSelectedText {
        key: UiKey,
        text: String,
    },
    SetTextSelection {
        key: UiKey,
        anchor_byte: usize,
        focus_byte: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AccessibilityActionError {
    WrongTree,
    UnknownNode,
    UnsupportedAction,
    MissingReducerAction,
    MissingValue,
    WrongValueKind,
    NonFiniteValue,
    InvalidTextSelection,
}

/// Validate and lower a platform request against the exact route table that was published.
pub(crate) fn route_accessibility_action(
    published: &PublishedNativeAccessibility,
    request: &accesskit::ActionRequest,
) -> Result<RoutedAccessibilityAction, AccessibilityActionError> {
    if request.target_tree != TreeId::ROOT {
        return Err(AccessibilityActionError::WrongTree);
    }
    let route = published
        .route(request.target_node)
        .ok_or(AccessibilityActionError::UnknownNode)?;
    if !route.supports(request.action) {
        return Err(AccessibilityActionError::UnsupportedAction);
    }
    match request.action {
        Action::Focus => Ok(RoutedAccessibilityAction::Focus {
            key: route.key.clone(),
        }),
        Action::Click => Ok(RoutedAccessibilityAction::Activate {
            key: route.key.clone(),
            action: route
                .action
                .clone()
                .ok_or(AccessibilityActionError::MissingReducerAction)?,
            value: None,
        }),
        Action::SetValue => {
            let value = match (route.role, request.data.as_ref()) {
                (SemanticRole::Slider, Some(accesskit::ActionData::NumericValue(value))) => {
                    if !value.is_finite() {
                        return Err(AccessibilityActionError::NonFiniteValue);
                    }
                    crate::native_app::SemanticInput::Number(*value)
                }
                (SemanticRole::TextField, Some(accesskit::ActionData::Value(value))) => {
                    crate::native_app::SemanticInput::Text(value.to_string())
                }
                (_, None) => return Err(AccessibilityActionError::MissingValue),
                _ => return Err(AccessibilityActionError::WrongValueKind),
            };
            Ok(RoutedAccessibilityAction::Activate {
                key: route.key.clone(),
                action: route
                    .action
                    .clone()
                    .ok_or(AccessibilityActionError::MissingReducerAction)?,
                value: Some(value),
            })
        }
        Action::ScrollUp => Ok(RoutedAccessibilityAction::Scroll {
            key: route.key.clone(),
            lines: -3,
        }),
        Action::ScrollDown => Ok(RoutedAccessibilityAction::Scroll {
            key: route.key.clone(),
            lines: 3,
        }),
        Action::ReplaceSelectedText => match (route.role, request.data.as_ref()) {
            (SemanticRole::TextViewport, Some(accesskit::ActionData::Value(value))) => {
                Ok(RoutedAccessibilityAction::ReplaceSelectedText {
                    key: route.key.clone(),
                    text: value.to_string(),
                })
            }
            (_, None) => Err(AccessibilityActionError::MissingValue),
            _ => Err(AccessibilityActionError::WrongValueKind),
        },
        Action::SetTextSelection => {
            let (
                SemanticRole::TextViewport,
                Some(accesskit::ActionData::SetTextSelection(selection)),
            ) = (route.role, request.data.as_ref())
            else {
                return Err(if request.data.is_none() {
                    AccessibilityActionError::MissingValue
                } else {
                    AccessibilityActionError::WrongValueKind
                });
            };
            let target = published
                .virtual_text
                .iter()
                .find(|target| target.node == request.target_node)
                .ok_or(AccessibilityActionError::InvalidTextSelection)?;
            let source_byte = |position: TextPosition| {
                target
                    .lines
                    .iter()
                    .find(|line| line.node == position.node)
                    .and_then(|line| line.character_source_offsets.get(position.character_index))
                    .and_then(|byte| usize::try_from(*byte).ok())
                    .ok_or(AccessibilityActionError::InvalidTextSelection)
            };
            Ok(RoutedAccessibilityAction::SetTextSelection {
                key: route.key.clone(),
                anchor_byte: source_byte(selection.anchor)?,
                focus_byte: source_byte(selection.focus)?,
            })
        }
        Action::Blur
        | Action::Collapse
        | Action::Expand
        | Action::CustomAction
        | Action::Decrement
        | Action::Increment
        | Action::HideTooltip
        | Action::ShowTooltip
        | Action::ScrollLeft
        | Action::ScrollRight
        | Action::ScrollIntoView
        | Action::ScrollToPoint
        | Action::SetScrollOffset
        | Action::SetSequentialFocusNavigationStartingPoint
        | Action::ShowContextMenu => Err(AccessibilityActionError::UnsupportedAction),
    }
}

struct PendingNode {
    id: NodeId,
    node: Node,
    children: Vec<NodeId>,
}

struct VisibleTextMaterialization {
    target: VirtualTextTarget,
    nodes: Vec<PendingNode>,
    selection: Option<TextSelection>,
}

fn materialize_visible_text(
    parent: NodeId,
    key: &UiKey,
    rect: LogicalRect,
    spec: &TextViewportSpec,
    id_for: &mut impl FnMut(&UiKey) -> NodeId,
    by_id: &mut FxHashMap<NodeId, UiKey>,
) -> Result<VisibleTextMaterialization, AccessibilityProjectionError> {
    let geometry = text_viewport_geometry(rect);
    let mut pending = Vec::new();
    let mut lines = Vec::new();
    let mut selected_start = None::<u64>;
    let mut selected_end = None::<u64>;
    let mut primary_caret = None::<u64>;

    if geometry.body_h > 0.0
        && geometry.text_x < rect.right()
        && let Some(projection) = spec.projection.as_ref()
    {
        let visible = ((geometry.body_h / geometry.line_h).ceil() as usize)
            .saturating_add(1)
            .min(projection.lines.len());
        for (row, projected) in projection.lines.iter().take(visible).enumerate() {
            let y = geometry.body_y + row as f32 * geometry.line_h;
            let height = (geometry.body_y + geometry.body_h - y)
                .min(geometry.line_h)
                .max(0.0);
            if height <= 0.0 {
                break;
            }
            let line_key = UiKey::new(format!(
                "{}/visible-line/{}",
                key.as_str(),
                projected.number
            ));
            let valid_source = projected.source.end >= projected.source.start
                && projected.source.end - projected.source.start == projected.text.len()
                && projected.selections.iter().all(|selection| {
                    selection.bytes.start <= selection.bytes.end
                        && selection.bytes.end <= projected.text.len()
                        && projected.text.is_char_boundary(selection.bytes.start)
                        && projected.text.is_char_boundary(selection.bytes.end)
                })
                && projected.carets.iter().all(|(byte, _)| {
                    *byte <= projected.text.len() && projected.text.is_char_boundary(*byte)
                });
            if !valid_source {
                return Err(AccessibilityProjectionError::InvalidVirtualText(line_key));
            }
            let line_id = id_for(&line_key);
            if let Some(first) = by_id.insert(line_id, line_key.clone()) {
                return Err(AccessibilityProjectionError::IdCollision {
                    first,
                    second: line_key,
                });
            }

            let source_start = u64::try_from(projected.source.start).unwrap_or(u64::MAX);
            let source_end = u64::try_from(projected.source.end).unwrap_or(u64::MAX);
            let mut character_lengths = Vec::with_capacity(projected.text.chars().count());
            let mut source_offsets = Vec::with_capacity(character_lengths.capacity() + 1);
            let mut positions = Vec::with_capacity(character_lengths.capacity());
            let mut widths = Vec::with_capacity(character_lengths.capacity());
            let mut column = 0usize;
            for (byte, character) in projected.text.char_indices() {
                character_lengths.push(u8::try_from(character.len_utf8()).unwrap_or(4));
                source_offsets.push(source_start.saturating_add(byte as u64));
                positions.push(column as f32 * geometry.cell_w);
                let cells = if character == '\t' {
                    4 - column % 4
                } else if character.is_control() {
                    0
                } else {
                    1
                };
                widths.push(cells as f32 * geometry.cell_w);
                column = column.saturating_add(cells);
            }
            source_offsets.push(source_end);

            let mut line_node = Node::new(Role::TextRun);
            line_node.set_value(projected.text.clone());
            line_node.set_character_lengths(character_lengths);
            line_node.set_character_positions(positions.clone());
            line_node.set_character_widths(widths.clone());
            if projection.total_lines > projected.number.saturating_add(1) {
                line_node.set_is_line_breaking_object();
            }
            line_node.set_bounds(Rect {
                x0: f64::from(geometry.text_x),
                y0: f64::from(y),
                x1: f64::from(rect.right()),
                y1: f64::from(y + height),
            });

            let selections = projected
                .selections
                .iter()
                .map(|selection| {
                    let start = source_start.saturating_add(selection.bytes.start as u64);
                    let end = source_start.saturating_add(selection.bytes.end as u64);
                    if selection.primary {
                        selected_start =
                            Some(selected_start.map_or(start, |value| value.min(start)));
                        selected_end = Some(selected_end.map_or(end, |value| value.max(end)));
                    }
                    VirtualTextSelectionSpan {
                        source: start..end,
                        primary: selection.primary,
                        continues: selection.continues,
                    }
                })
                .collect::<Vec<_>>();
            let carets = projected
                .carets
                .iter()
                .map(|(byte, primary)| {
                    let source_byte = source_start.saturating_add(*byte as u64);
                    if *primary {
                        primary_caret = Some(source_byte);
                    }
                    VirtualTextCaret {
                        source_byte,
                        primary: *primary,
                    }
                })
                .collect();
            lines.push(VirtualTextLine {
                node: line_id,
                key: line_key,
                source: source_start..source_end,
                text: projected.text.clone(),
                character_source_offsets: source_offsets,
                character_positions: positions,
                character_widths: widths,
                selections,
                carets,
            });
            pending.push(PendingNode {
                id: line_id,
                node: line_node,
                children: Vec::new(),
            });
        }
    }

    // The editor modeline is a real status surface, including live config
    // diagnostics. Project it as a polite live region instead of leaving the
    // information available only to sighted users in painted footer pixels.
    if let Some(status) = spec
        .semantic_status
        .as_deref()
        .or(spec.status.as_deref())
        .filter(|status| !status.is_empty())
    {
        let status_key = UiKey::new(format!("{}/status", key.as_str()));
        let status_id = id_for(&status_key);
        if let Some(first) = by_id.insert(status_id, status_key.clone()) {
            return Err(AccessibilityProjectionError::IdCollision {
                first,
                second: status_key,
            });
        }
        let mut status_node = Node::new(Role::Status);
        // The CONTEXT is the description; the announced string is the status itself
        // ([`announce_live`] — the name is what AT-SPI and UIA speak, so a constant
        // "Editor status" name would be the only thing a Linux/Windows reader ever heard,
        // once, and never the diagnostic that changed).
        status_node.set_description("Editor status");
        announce_live(&mut status_node, Live::Polite, status);
        status_node.set_bounds(Rect {
            x0: f64::from(rect.x),
            y0: f64::from(rect.bottom() - geometry.footer_h),
            x1: f64::from(rect.right()),
            y1: f64::from(rect.bottom()),
        });
        pending.push(PendingNode {
            id: status_id,
            node: status_node,
            children: Vec::new(),
        });
    }

    let visible_range = lines.first().map_or(0, |line| line.source.start)
        ..lines.last().map_or(0, |line| line.source.end);
    let primary_selection = match (selected_start, selected_end, primary_caret) {
        (Some(start), Some(end), caret) => {
            let focus = caret.unwrap_or(end);
            Some(VirtualTextSelection {
                anchor_byte: if focus <= start { end } else { start },
                focus_byte: focus,
            })
        }
        (None, None, Some(caret)) => Some(VirtualTextSelection {
            anchor_byte: caret,
            focus_byte: caret,
        }),
        _ => None,
    };
    let selection = primary_selection.and_then(|selection| {
        Some(TextSelection {
            anchor: text_position_for_source(&lines, selection.anchor_byte)?,
            focus: text_position_for_source(&lines, selection.focus_byte)?,
        })
    });

    Ok(VisibleTextMaterialization {
        target: VirtualTextTarget {
            node: parent,
            key: key.clone(),
            document_key: spec.document_key.clone(),
            visible_bounds: LogicalRect::new(
                geometry.text_x,
                geometry.body_y,
                (rect.right() - geometry.text_x).max(0.0),
                geometry.body_h,
            ),
            visible_range,
            lines,
            primary_selection,
        },
        nodes: pending,
        selection,
    })
}

fn text_position_for_source(lines: &[VirtualTextLine], source_byte: u64) -> Option<TextPosition> {
    let line = lines.iter().min_by_key(|line| {
        if source_byte < line.source.start {
            line.source.start - source_byte
        } else {
            source_byte.saturating_sub(line.source.end)
        }
    })?;
    let character_index = line
        .character_source_offsets
        .iter()
        .position(|offset| source_byte <= *offset)
        .unwrap_or_else(|| line.character_source_offsets.len().saturating_sub(1));
    Some(TextPosition {
        node: line.node,
        character_index,
    })
}

/// Project native semantics using stable [`UiKey`] identities.
///
/// `focused` is the host's authoritative content focus. When omitted, exactly zero or one
/// `ControlState::focused` flag is accepted; zero falls back to the semantic root.
pub(crate) fn project_native_accessibility(
    compiled: &CompiledUi,
    focused: Option<&UiKey>,
) -> Result<NativeAccessibilityProjection, AccessibilityProjectionError> {
    project_with_ids(compiled, focused, stable_node_id)
}

/// Project logical native coordinates into the physical-pixel window coordinates AccessKit
/// requires. The root transform is the only host-owned geometry added to the canonical tree;
/// every descendant bound still comes verbatim from [`CompiledUi::semantics`].
pub(crate) fn project_native_accessibility_in_container(
    compiled: &CompiledUi,
    focused: Option<&UiKey>,
    transform: Affine,
) -> Result<NativeAccessibilityProjection, AccessibilityProjectionError> {
    let coefficients = transform.as_coeffs();
    if coefficients.iter().any(|value| !value.is_finite())
        || transform.determinant().abs() <= f64::EPSILON
    {
        return Err(AccessibilityProjectionError::InvalidContainerTransform);
    }
    project_with_transform_and_ids(compiled, focused, Some(transform), stable_node_id)
}

/// Project one visible native leaf into window coordinates with identities and
/// action routes qualified by the leaf's stable lifecycle owner.
pub(crate) fn project_native_accessibility_for_view_in_container(
    compiled: &CompiledUi,
    focused: Option<&UiKey>,
    transform: Affine,
    owner: AccessibilityOwner,
) -> Result<NativeAccessibilityProjection, AccessibilityProjectionError> {
    let coefficients = transform.as_coeffs();
    if coefficients.iter().any(|value| !value.is_finite())
        || transform.determinant().abs() <= f64::EPSILON
    {
        return Err(AccessibilityProjectionError::InvalidContainerTransform);
    }
    let mut projection =
        project_with_transform_and_ids(compiled, focused, Some(transform), |key| {
            stable_node_id_for_view(owner.view, key)
        })?;
    for route in &mut projection.routes {
        route.owner = Some(owner);
    }
    Ok(projection)
}

/// Publish all visible native split leaves as one AccessKit window tree.
///
/// Each child projection has already consumed the exact host transform used by
/// its presented raster.  This function only adds a stable `Role::Window` root,
/// checks cross-leaf identity uniqueness, and selects the focused native leaf's
/// existing focus node (or the window root while a terminal sibling is focused).
pub(crate) fn compose_native_accessibility(
    projections: Vec<(AccessibilityOwner, NativeAccessibilityProjection)>,
    focused_native: Option<ViewId>,
    window_bounds: Rect,
    window_title: &str,
) -> Result<NativeAccessibilityProjection, AccessibilityProjectionError> {
    if projections.is_empty() {
        return Err(AccessibilityProjectionError::EmptyTree);
    }
    if !window_bounds.x0.is_finite()
        || !window_bounds.y0.is_finite()
        || !window_bounds.x1.is_finite()
        || !window_bounds.y1.is_finite()
        || window_bounds.x1 <= window_bounds.x0
        || window_bounds.y1 <= window_bounds.y0
    {
        return Err(AccessibilityProjectionError::InvalidCompositeBounds);
    }

    let root = composite_root_id();
    let mut seen_views = HashSet::with_capacity(projections.len());
    let mut seen_nodes = HashSet::new();
    let mut child_roots = Vec::with_capacity(projections.len());
    let mut nodes = Vec::new();
    let mut routes = Vec::new();
    let mut virtual_text = Vec::new();
    let mut source_nodes_visited = 0usize;
    let mut focused_node = None;

    for (owner, mut projection) in projections {
        if !seen_views.insert(owner.view) {
            return Err(AccessibilityProjectionError::DuplicateCompositeView(
                owner.view,
            ));
        }
        let leaf_root = projection
            .update
            .tree
            .as_ref()
            .map(|tree| tree.root)
            .ok_or(AccessibilityProjectionError::EmptyTree)?;
        if focused_native == Some(owner.view) {
            focused_node = Some(projection.update.focus);
        }
        for (node, _) in &projection.update.nodes {
            if *node == root || !seen_nodes.insert(*node) {
                return Err(AccessibilityProjectionError::CompositeIdCollision(*node));
            }
        }
        child_roots.push(leaf_root);
        nodes.append(&mut projection.update.nodes);
        for route in &mut projection.routes {
            route.owner = Some(owner);
        }
        routes.append(&mut projection.routes);
        virtual_text.append(&mut projection.virtual_text);
        source_nodes_visited = source_nodes_visited.saturating_add(projection.source_nodes_visited);
    }

    let mut window = Node::new(Role::Window);
    // The window's own title, exactly as `accesskit_tree::grid_tree` names the terminal
    // window: the root's name is what every assistive client calls the WINDOW, so a user
    // moving between windows must not hear "aterm" for all of them — and it must not
    // change meaning merely because the front tab is a native app rather than a shell.
    window.set_label(if window_title.is_empty() {
        "aterm"
    } else {
        window_title
    });
    window.set_bounds(window_bounds);
    window.set_children(child_roots);
    nodes.push((root, window));

    Ok(NativeAccessibilityProjection {
        update: TreeUpdate {
            nodes,
            tree: Some(Tree::new(root)),
            tree_id: TreeId::ROOT,
            focus: focused_node.unwrap_or(root),
        },
        routes,
        virtual_text,
        source_nodes_visited,
    })
}

fn project_with_ids(
    compiled: &CompiledUi,
    focused: Option<&UiKey>,
    id_for: impl FnMut(&UiKey) -> NodeId,
) -> Result<NativeAccessibilityProjection, AccessibilityProjectionError> {
    project_with_transform_and_ids(compiled, focused, None, id_for)
}

fn project_with_transform_and_ids(
    compiled: &CompiledUi,
    focused: Option<&UiKey>,
    root_transform: Option<Affine>,
    mut id_for: impl FnMut(&UiKey) -> NodeId,
) -> Result<NativeAccessibilityProjection, AccessibilityProjectionError> {
    if compiled.semantics.is_empty() {
        return Err(AccessibilityProjectionError::EmptyTree);
    }

    let focusable = compiled
        .focus_order
        .iter()
        .cloned()
        .collect::<FxHashSet<_>>();
    let viewport_specs = compiled
        .paint
        .iter()
        .filter_map(|paint| match &paint.content {
            UiContent::TextViewport(spec) => Some((paint.key.clone(), spec)),
            _ => None,
        })
        .collect::<FxHashMap<_, _>>();
    let text_field_specs = compiled
        .paint
        .iter()
        .filter_map(|paint| match &paint.content {
            UiContent::TextField(control) => Some((paint.key.clone(), &control.spec)),
            _ => None,
        })
        .collect::<FxHashMap<_, _>>();
    let mut by_key: FxHashMap<UiKey, usize> =
        FxHashMap::with_capacity_and_hasher(compiled.semantics.len(), aterm_hash::FxBuildHasher);
    let mut by_id: FxHashMap<NodeId, UiKey> =
        FxHashMap::with_capacity_and_hasher(compiled.semantics.len(), aterm_hash::FxBuildHasher);
    let mut pending: Vec<PendingNode> = Vec::with_capacity(compiled.semantics.len());
    let mut routes = Vec::with_capacity(compiled.semantics.len());
    let mut virtual_text = Vec::new();
    let mut root: Option<NodeId> = None;
    let mut explicit_focus = None;
    let mut state_focus = None;
    let mut state_focus_ambiguous = false;
    let mut source_nodes_visited = 0;

    // This is the sole traversal of the canonical semantic source.
    for semantic in &compiled.semantics {
        source_nodes_visited += 1;
        if by_key.contains_key(&semantic.key) {
            return Err(AccessibilityProjectionError::DuplicateKey(
                semantic.key.clone(),
            ));
        }
        if !semantic.rect.is_valid() || semantic.rect.is_empty() {
            return Err(AccessibilityProjectionError::InvalidBounds(
                semantic.key.clone(),
            ));
        }

        let id = id_for(&semantic.key);
        if let Some(first) = by_id.insert(id, semantic.key.clone()) {
            return Err(AccessibilityProjectionError::IdCollision {
                first,
                second: semantic.key.clone(),
            });
        }

        let parent_index = if let Some(parent) = semantic.parent.as_ref() {
            Some(*by_key.get(parent).ok_or_else(|| {
                AccessibilityProjectionError::MissingParent {
                    node: semantic.key.clone(),
                    parent: parent.clone(),
                }
            })?)
        } else {
            if root.replace(id).is_some() {
                return Err(AccessibilityProjectionError::MultipleRoots);
            }
            None
        };

        let mut node = lower_node(semantic, focusable.contains(&semantic.key))?;
        if semantic.role == SemanticRole::TextField
            && let Some(input) = text_field_specs
                .get(&semantic.key)
                .and_then(|spec| spec.input.as_ref())
        {
            let SemanticValue::Text(value) = &semantic.value else {
                return Err(AccessibilityProjectionError::InvalidTextInput(
                    semantic.key.clone(),
                ));
            };
            let selection = &input.selection;
            let valid = input.text.len() <= crate::native_text_input::MAX_TEXT_INPUT_BYTES
                && value == &input.text
                && selection.anchor <= input.text.len()
                && selection.head <= input.text.len()
                && input.text.is_char_boundary(selection.anchor)
                && input.text.is_char_boundary(selection.head)
                && input.preedit.as_ref().is_none_or(|marked| {
                    marked.start <= marked.end
                        && marked.end <= input.text.len()
                        && input.text.is_char_boundary(marked.start)
                        && input.text.is_char_boundary(marked.end)
                });
            if !valid {
                return Err(AccessibilityProjectionError::InvalidTextInput(
                    semantic.key.clone(),
                ));
            }
            node.set_character_lengths(
                input
                    .text
                    .chars()
                    .map(|character| u8::try_from(character.len_utf8()).unwrap_or(4))
                    .collect::<Vec<_>>(),
            );
            node.set_text_selection(TextSelection {
                anchor: TextPosition {
                    node: id,
                    character_index: input.text[..selection.anchor].chars().count(),
                },
                focus: TextPosition {
                    node: id,
                    character_index: input.text[..selection.head].chars().count(),
                },
            });
        }
        if semantic.parent.is_none()
            && let Some(transform) = root_transform
        {
            node.set_transform(transform);
        }
        node.set_bounds(rect_to_accesskit(semantic.rect));
        if let Some(parent_index) = parent_index {
            pending[parent_index].children.push(id);
        }

        if focused.is_some_and(|key| key == &semantic.key) {
            explicit_focus = Some(id);
        }
        if semantic.state.is_some_and(|state| state.focused) && state_focus.replace(id).is_some() {
            state_focus_ambiguous = true;
        }

        let visible_text = if semantic.role == SemanticRole::TextViewport {
            let SemanticValue::Text(document_key) = &semantic.value else {
                return Err(AccessibilityProjectionError::MissingVirtualDocument(
                    semantic.key.clone(),
                ));
            };
            if document_key.trim().is_empty() {
                return Err(AccessibilityProjectionError::MissingVirtualDocument(
                    semantic.key.clone(),
                ));
            }
            let spec = viewport_specs.get(&semantic.key).ok_or_else(|| {
                AccessibilityProjectionError::MissingVirtualDocument(semantic.key.clone())
            })?;
            if spec.document_key != *document_key {
                return Err(AccessibilityProjectionError::MissingVirtualDocument(
                    semantic.key.clone(),
                ));
            }
            Some(materialize_visible_text(
                id,
                &semantic.key,
                semantic.rect,
                spec,
                &mut id_for,
                &mut by_id,
            )?)
        } else {
            None
        };
        if let Some(selection) = visible_text
            .as_ref()
            .and_then(|visible| visible.selection.as_ref())
        {
            node.set_text_selection(*selection);
        }

        let index = pending.len();
        by_key.insert(semantic.key.clone(), index);
        routes.push(AccessibilityRoute {
            node: id,
            key: semantic.key.clone(),
            action: semantic.action.clone(),
            role: semantic.role,
            owner: None,
            supported: supported_actions(&node),
        });
        pending.push(PendingNode {
            id,
            node,
            children: Vec::new(),
        });
        if let Some(visible) = visible_text {
            pending[index]
                .children
                .extend(visible.nodes.iter().map(|line| line.id));
            pending.extend(visible.nodes);
            virtual_text.push(visible.target);
        }
    }

    let root = root.ok_or(AccessibilityProjectionError::EmptyTree)?;
    let focus = if let Some(explicit) = explicit_focus {
        explicit
    } else if state_focus_ambiguous {
        return Err(AccessibilityProjectionError::MultipleFocusedNodes);
    } else {
        state_focus.unwrap_or(root)
    };

    // Materialize AccessKit nodes from the one-pass intermediate. This does not revisit the
    // semantic source or app tree.
    let nodes = pending
        .into_iter()
        .map(|mut pending| {
            if !pending.children.is_empty() {
                pending.node.set_children(pending.children);
            }
            (pending.id, pending.node)
        })
        .collect();

    Ok(NativeAccessibilityProjection {
        update: TreeUpdate {
            nodes,
            tree: Some(Tree::new(root)),
            tree_id: TreeId::ROOT,
            focus,
        },
        routes,
        virtual_text,
        source_nodes_visited,
    })
}

fn lower_node(
    semantic: &SemanticNode,
    focusable: bool,
) -> Result<Node, AccessibilityProjectionError> {
    let mut node = Node::new(lower_role(semantic.role));
    if !semantic.label.is_empty() {
        node.set_label(semantic.label.clone());
    }

    let enabled = semantic.state.is_none_or(|state| state.enabled);
    if let Some(state) = semantic.state {
        lower_state(&mut node, state);
    }
    lower_value(&mut node, semantic)?;

    if matches!(semantic.role, SemanticRole::RichText) {
        node.set_read_only();
    }
    if semantic.role == SemanticRole::Status {
        // A `Status` semantic carries its sentence in the LABEL (`UiContent::Text` and
        // `UiContent::Group` both project their text there) and usually has no value at
        // all, which is silent on macOS. `announce_live` mirrors whichever one is present
        // onto the other so every platform speaks the same words.
        let announced = node
            .value()
            .map(str::to_string)
            .unwrap_or_else(|| semantic.label.clone());
        announce_live(&mut node, Live::Polite, &announced);
    }
    if enabled {
        lower_actions(&mut node, semantic, focusable);
    }
    Ok(node)
}

/// Mark `node` a LIVE REGION whose spoken text is `announced`, on every platform.
///
/// THE NAME AND THE VALUE ARE BOTH LOAD-BEARING, and each platform reads a different
/// one — a node that carries only one of them is announced on some platforms and silent
/// on the rest:
///
/// * AT-SPI (`accesskit_atspi_common`) emits `ObjectEvent::Announcement` carrying the
///   node's NAME, and only when the name CHANGES (or the node is newly added). A live
///   region whose name is a constant caption therefore speaks that caption once and
///   never says anything again, however often its value moves.
/// * UIA (`accesskit_windows`) raises `UIA_LiveRegionChangedEventId` under the same
///   name-changed condition, and the screen reader then reads the element's name.
/// * NSAccessibility (`accesskit_macos`) does the opposite: it announces the node's
///   VALUE, and raises nothing at all when the node has no value.
///
/// So the announced string goes in BOTH, and any fixed caption belongs in the
/// `description` — which is a property change on every platform and never an
/// announcement. Keeping this in one function is what stops a new live surface from
/// re-deriving half the rule.
fn announce_live(node: &mut Node, politeness: Live, announced: &str) {
    node.set_label(announced.to_string());
    node.set_value(announced.to_string());
    node.set_live(politeness);
}

fn lower_role(role: SemanticRole) -> Role {
    match role {
        SemanticRole::Application => Role::Application,
        SemanticRole::Group => Role::Group,
        SemanticRole::Heading => Role::Heading,
        SemanticRole::Text => Role::Label,
        SemanticRole::Button => Role::Button,
        SemanticRole::Switch => Role::Switch,
        SemanticRole::Slider => Role::Slider,
        SemanticRole::TextField => Role::TextInput,
        SemanticRole::RichText => Role::Document,
        SemanticRole::TextViewport => Role::MultilineTextInput,
        SemanticRole::Link => Role::Link,
        SemanticRole::Navigation => Role::Navigation,
        SemanticRole::Status => Role::Status,
    }
}

fn lower_state(node: &mut Node, state: ControlState) {
    if !state.enabled {
        node.set_disabled();
    }
    if state.selected {
        node.set_selected(true);
    }
    if state.invalid {
        node.set_invalid(Invalid::True);
    }
    if state.busy {
        node.set_busy();
    }
}

fn lower_value(
    node: &mut Node,
    semantic: &SemanticNode,
) -> Result<(), AccessibilityProjectionError> {
    match &semantic.value {
        SemanticValue::None => {}
        // For a virtual viewport this string is a provider identity, not user-authored text.
        // Keep it in the sidecar target and never announce it as the document value.
        SemanticValue::Text(_) if semantic.role == SemanticRole::TextViewport => {}
        SemanticValue::Text(value) => node.set_value(value.clone()),
        SemanticValue::Bool(value) => {
            node.set_toggled(Toggled::from(*value));
            node.set_value(value.to_string());
        }
        SemanticValue::Number {
            value,
            minimum,
            maximum,
        } => {
            if !value.is_finite()
                || !minimum.is_finite()
                || !maximum.is_finite()
                || minimum > maximum
                || value < minimum
                || value > maximum
            {
                return Err(AccessibilityProjectionError::InvalidNumber(
                    semantic.key.clone(),
                ));
            }
            node.set_numeric_value(*value);
            node.set_min_numeric_value(*minimum);
            node.set_max_numeric_value(*maximum);
            node.set_value(value.to_string());
        }
    }
    Ok(())
}

fn lower_actions(node: &mut Node, semantic: &SemanticNode, focusable: bool) {
    if focusable {
        node.add_action(Action::Focus);
    }
    let has_reducer_action = semantic.action.is_some();
    if has_reducer_action {
        node.add_action(Action::Click);
    }
    match semantic.role {
        SemanticRole::Slider if has_reducer_action => {
            node.add_action(Action::SetValue);
        }
        SemanticRole::TextField if has_reducer_action => {
            node.add_action(Action::SetValue);
        }
        SemanticRole::RichText => {
            node.add_action(Action::ScrollUp);
            node.add_action(Action::ScrollDown);
        }
        SemanticRole::TextViewport => {
            node.add_action(Action::ScrollUp);
            node.add_action(Action::ScrollDown);
            node.add_action(Action::ReplaceSelectedText);
            node.add_action(Action::SetTextSelection);
        }
        SemanticRole::Slider
        | SemanticRole::TextField
        | SemanticRole::Application
        | SemanticRole::Group
        | SemanticRole::Heading
        | SemanticRole::Text
        | SemanticRole::Button
        | SemanticRole::Switch
        | SemanticRole::Link
        | SemanticRole::Navigation
        | SemanticRole::Status => {}
    }
}

fn supported_actions(node: &Node) -> Vec<Action> {
    [
        Action::Click,
        Action::Focus,
        Action::SetValue,
        Action::ScrollUp,
        Action::ScrollDown,
        Action::ReplaceSelectedText,
        Action::SetTextSelection,
    ]
    .into_iter()
    .filter(|action| node.supports_action(*action))
    .collect()
}

fn rect_to_accesskit(rect: LogicalRect) -> Rect {
    Rect {
        x0: f64::from(rect.x),
        y0: f64::from(rect.y),
        x1: f64::from(rect.right()),
        y1: f64::from(rect.bottom()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_ui::{
        ButtonSpec, Control, Flow, GroupSpec, Insets, Layout, Length, SliderSpec, StyleRef,
        SwitchSpec, TextFieldSpec, TextSpec, TextViewportSpec, UiContent, UiNode, UiTree,
    };

    fn button(key: &str, label: &str) -> UiNode {
        UiNode::new(
            key,
            UiContent::Button(Control::new(
                ButtonSpec::new(label),
                ActionId::new(format!("activate/{key}")),
            )),
        )
        .layout(Layout::default().height(Length::Fixed(40.0)))
    }

    fn compiled_form(order: &[&str]) -> CompiledUi {
        let children = order
            .iter()
            .map(|key| button(key, &key.to_ascii_uppercase()))
            .collect();
        UiTree::new(
            UiNode::new(
                "app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(
                Layout::column()
                    .padding(Insets::all(4.0))
                    .gap(2.0)
                    .clipped(),
            )
            .children(children),
        )
        .compile(LogicalRect::new(0.0, 0.0, 240.0, 200.0))
        .unwrap()
    }

    fn node<'a>(projection: &'a NativeAccessibilityProjection, key: &str) -> &'a Node {
        let id = projection.id_for_key(&UiKey::new(key)).unwrap();
        &projection
            .update()
            .nodes
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .unwrap()
            .1
    }

    fn semantic(
        key: &str,
        parent: Option<&str>,
        role: SemanticRole,
        value: SemanticValue,
    ) -> SemanticNode {
        SemanticNode {
            key: UiKey::new(key),
            parent: parent.map(UiKey::new),
            rect: LogicalRect::new(0.0, 0.0, 10.0, 10.0),
            role,
            label: key.to_string(),
            value,
            state: None,
            action: None,
            audit_id: None,
        }
    }

    #[test]
    fn stable_ids_derive_from_keys_not_order_or_filter_position() {
        let first = project_native_accessibility(&compiled_form(&["alpha", "beta"]), None).unwrap();
        let reordered =
            project_native_accessibility(&compiled_form(&["beta", "alpha"]), None).unwrap();
        let filtered = project_native_accessibility(&compiled_form(&["beta"]), None).unwrap();

        let beta = UiKey::new("beta");
        assert_eq!(first.id_for_key(&beta), reordered.id_for_key(&beta));
        assert_eq!(first.id_for_key(&beta), filtered.id_for_key(&beta));
        assert_eq!(first.id_for_key(&beta), Some(stable_node_id(&beta)));
        assert_ne!(
            first.id_for_key(&UiKey::new("alpha")),
            first.id_for_key(&beta)
        );
    }

    #[test]
    fn text_field_accessibility_uses_the_same_preedit_value_and_selection_as_paint() {
        let mut input = crate::native_text_input::TextInputState::new("aβz".to_string());
        input.set_selection(1, 3);
        input.set_preedit("に".to_string(), Some(0..3));
        let input = input.projection();
        let field = Control::new(
            TextFieldSpec {
                label: "Search".to_string(),
                placeholder: None,
                secret: false,
                visual_value: None,
                input: Some(input.clone()),
                swatch: None,
            },
            ActionId::new("search"),
        )
        .value(SemanticValue::Text(input.text.clone()))
        .state(ControlState {
            focused: true,
            ..ControlState::default()
        });
        let compiled = UiTree::new(UiNode::new("search", UiContent::TextField(field)))
            .compile(LogicalRect::new(0.0, 0.0, 240.0, 40.0))
            .unwrap();
        let projection = project_native_accessibility(&compiled, None).unwrap();
        let id = projection.id_for_key(&UiKey::new("search")).unwrap();
        let node = node(&projection, "search");
        assert_eq!(node.value(), Some("aにz"));
        assert_eq!(node.character_lengths(), &[1, 3, 1]);
        assert_eq!(
            node.text_selection(),
            Some(&TextSelection {
                anchor: TextPosition {
                    node: id,
                    character_index: 1,
                },
                focus: TextPosition {
                    node: id,
                    character_index: 2,
                },
            })
        );
    }

    #[test]
    fn projection_consumes_compiled_semantics_once_and_preserves_hierarchy() {
        let compiled = compiled_form(&["alpha", "beta"]);
        let projection = project_native_accessibility(&compiled, None).unwrap();
        assert_eq!(projection.source_nodes_visited(), compiled.semantics.len());
        assert_eq!(projection.update().nodes.len(), compiled.semantics.len());
        assert_eq!(projection.routes().len(), compiled.semantics.len());

        let root_id = projection.id_for_key(&UiKey::new("app")).unwrap();
        let root = node(&projection, "app");
        assert_eq!(projection.update().tree.as_ref().unwrap().root, root_id);
        assert_eq!(root.role(), Role::Application);
        assert_eq!(
            root.children(),
            [
                projection.id_for_key(&UiKey::new("alpha")).unwrap(),
                projection.id_for_key(&UiKey::new("beta")).unwrap(),
            ]
        );
        assert_eq!(projection.update().focus, root_id);
    }

    #[test]
    fn every_semantic_role_has_one_standard_role_mapping() {
        let mappings = [
            (SemanticRole::Application, Role::Application),
            (SemanticRole::Group, Role::Group),
            (SemanticRole::Heading, Role::Heading),
            (SemanticRole::Text, Role::Label),
            (SemanticRole::Button, Role::Button),
            (SemanticRole::Switch, Role::Switch),
            (SemanticRole::Slider, Role::Slider),
            (SemanticRole::TextField, Role::TextInput),
            (SemanticRole::RichText, Role::Document),
            (SemanticRole::TextViewport, Role::MultilineTextInput),
            (SemanticRole::Link, Role::Link),
            (SemanticRole::Navigation, Role::Navigation),
            (SemanticRole::Status, Role::Status),
        ];
        for (semantic, accesskit) in mappings {
            assert_eq!(lower_role(semantic), accesskit);
        }
    }

    #[test]
    fn paint_only_responsive_status_copy_is_not_published_to_accessibility() {
        const COMPLETE: &str = "On, currently silent while this window is unfocused.";
        let visual = crate::native_ui::fit_native_status_label(COMPLETE, 96.0);
        assert_ne!(visual, COMPLETE, "fixture must exercise responsive copy");
        let compiled = UiTree::new(
            UiNode::new(
                "app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column())
            .children(vec![
                UiNode::new(
                    "status",
                    UiContent::Group(GroupSpec {
                        label: Some(COMPLETE.to_string()),
                        role: SemanticRole::Status,
                        style: StyleRef::Plain,
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(24.0)))
                .children(vec![
                    UiNode::new(
                        "status/visual",
                        UiContent::Text(TextSpec {
                            text: visual.clone(),
                            role: SemanticRole::Status,
                            style: StyleRef::Plain,
                        }),
                    )
                    .paint_only(),
                ]),
            ]),
        )
        .compile(LogicalRect::new(0.0, 0.0, 96.0, 24.0))
        .unwrap();

        assert!(compiled.paint.iter().any(|node| {
            node.key == UiKey::new("status/visual")
                && matches!(&node.content, UiContent::Text(spec) if spec.text == visual)
        }));
        assert_eq!(compiled.semantics.len(), 2);
        assert!(compiled.semantic(&UiKey::new("status/visual")).is_none());

        let projection = project_native_accessibility(&compiled, None).unwrap();
        assert_eq!(projection.update().nodes.len(), 2);
        assert!(
            projection
                .id_for_key(&UiKey::new("status/visual"))
                .is_none()
        );
        let statuses = projection
            .update()
            .nodes
            .iter()
            .map(|(_, node)| node)
            .filter(|node| node.role() == Role::Status)
            .collect::<Vec<_>>();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].live(), Some(Live::Polite));
        assert_eq!(statuses[0].label(), Some(COMPLETE));
        // The COMPLETE sentence, never the responsive visual truncation — and in the
        // value as well as the name, because NSAccessibility announces a live region by
        // its value and says nothing at all for a node that has none.
        assert_eq!(statuses[0].value(), Some(COMPLETE));
        assert!(statuses[0].children().is_empty());
        assert!(
            projection
                .update()
                .nodes
                .iter()
                .all(|(_, node)| node.label() != Some(visual.as_str()))
        );
    }

    /// A live region has to carry the SAME sentence in its name and its value, because
    /// the three platforms read different halves: AT-SPI and UIA announce the name (and
    /// only when it changes), NSAccessibility announces the value (and nothing when
    /// there is none). Publishing one half is how a status surface ends up audible on
    /// exactly one operating system.
    #[test]
    fn every_live_region_announces_the_same_sentence_by_name_and_by_value() {
        const SENTENCE: &str = "Reading page 3 of 12";
        let compiled = UiTree::new(
            UiNode::new(
                "app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column())
            .children(vec![
                UiNode::new(
                    "reader/status",
                    UiContent::Text(TextSpec {
                        text: SENTENCE.to_string(),
                        role: SemanticRole::Status,
                        style: StyleRef::Plain,
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(24.0))),
            ]),
        )
        .compile(LogicalRect::new(0.0, 0.0, 240.0, 24.0))
        .unwrap();

        let projection = project_native_accessibility(&compiled, None).unwrap();
        let live: Vec<_> = projection
            .update()
            .nodes
            .iter()
            .map(|(_, node)| node)
            .filter(|node| node.live().is_some_and(|live| live != Live::Off))
            .collect();
        assert_eq!(live.len(), 1, "the status node is the frame's live region");
        assert_eq!(live[0].label(), Some(SENTENCE), "AT-SPI/UIA announce this");
        assert_eq!(
            live[0].value(),
            Some(SENTENCE),
            "NSAccessibility announces this"
        );
    }

    #[test]
    fn labels_values_states_actions_focus_and_bounds_lower_without_side_tables() {
        let switch = Control::new(
            SwitchSpec {
                label: "Automatic updates".to_string(),
                description: None,
            },
            ActionId::new("updates/toggle"),
        )
        .value(SemanticValue::Bool(true))
        .state(ControlState {
            enabled: false,
            selected: true,
            invalid: true,
            busy: true,
            focused: false,
            ..ControlState::default()
        });
        let slider = Control::new(
            SliderSpec {
                label: "Font size".to_string(),
                step: 1.0,
                display_value: "14".to_string(),
            },
            ActionId::new("font/size"),
        )
        .value(SemanticValue::Number {
            value: 14.0,
            minimum: 6.0,
            maximum: 32.0,
        })
        .state(ControlState {
            focused: true,
            ..ControlState::default()
        });
        let field = Control::new(
            TextFieldSpec {
                label: "Family".to_string(),
                placeholder: None,
                secret: false,
                visual_value: None,
                input: None,
                swatch: None,
            },
            ActionId::new("font/family"),
        )
        .value(SemanticValue::Text("Berkeley Mono".to_string()));
        let tree = UiTree::new(
            UiNode::new(
                "app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column().clipped())
            .children(vec![
                UiNode::new("updates", UiContent::Switch(switch))
                    .layout(Layout::default().height(Length::Fixed(40.0))),
                UiNode::new("font", UiContent::Slider(slider))
                    .layout(Layout::default().height(Length::Fixed(40.0))),
                UiNode::new("family", UiContent::TextField(field))
                    .layout(Layout::default().height(Length::Fixed(40.0))),
                UiNode::new(
                    "status",
                    UiContent::Text(TextSpec {
                        text: "Update ready".to_string(),
                        role: SemanticRole::Status,
                        style: crate::native_ui::StyleRef::Plain,
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(24.0))),
            ]),
        );
        let compiled = tree
            .compile(LogicalRect::new(10.0, 20.0, 300.0, 144.0))
            .unwrap();
        let projection = project_native_accessibility(&compiled, None).unwrap();

        let updates = node(&projection, "updates");
        assert_eq!(updates.role(), Role::Switch);
        assert_eq!(updates.label(), Some("Automatic updates"));
        assert_eq!(updates.value(), Some("true"));
        assert_eq!(updates.toggled(), Some(Toggled::True));
        assert!(updates.is_disabled());
        assert_eq!(updates.is_selected(), Some(true));
        assert_eq!(updates.invalid(), Some(Invalid::True));
        assert!(updates.is_busy());
        assert!(!updates.supports_action(Action::Click));
        assert!(!updates.supports_action(Action::Focus));

        let font = node(&projection, "font");
        assert_eq!(font.numeric_value(), Some(14.0));
        assert_eq!(font.min_numeric_value(), Some(6.0));
        assert_eq!(font.max_numeric_value(), Some(32.0));
        assert!(font.supports_action(Action::Click));
        assert!(font.supports_action(Action::Focus));
        assert!(font.supports_action(Action::SetValue));
        assert!(!font.supports_action(Action::Increment));
        assert!(!font.supports_action(Action::Decrement));
        assert_eq!(
            projection.update().focus,
            projection.id_for_key(&UiKey::new("font")).unwrap()
        );
        assert_eq!(
            font.bounds(),
            Some(Rect {
                x0: 10.0,
                y0: 60.0,
                x1: 310.0,
                y1: 100.0,
            })
        );

        let family = node(&projection, "family");
        assert_eq!(family.value(), Some("Berkeley Mono"));
        assert!(family.supports_action(Action::SetValue));
        assert!(!family.supports_action(Action::ReplaceSelectedText));
        assert_eq!(node(&projection, "status").live(), Some(Live::Polite));
    }

    #[test]
    fn explicit_host_focus_wins_over_stale_per_control_flags() {
        let mut compiled = compiled_form(&["alpha", "beta"]);
        for semantic in &mut compiled.semantics {
            if semantic.role == SemanticRole::Button {
                semantic.state.as_mut().unwrap().focused = true;
            }
        }
        let beta = UiKey::new("beta");
        let projection = project_native_accessibility(&compiled, Some(&beta)).unwrap();
        assert_eq!(projection.update().focus, stable_node_id(&beta));
        assert_eq!(
            project_native_accessibility(&compiled, None),
            Err(AccessibilityProjectionError::MultipleFocusedNodes)
        );
    }

    #[test]
    fn clipped_bounds_and_omitted_subtrees_are_used_verbatim() {
        let tree = UiTree::new(
            UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
                .layout(Layout::column().clipped())
                .children(vec![
                    button("first", "First"),
                    button("partial", "Partial"),
                    button("offscreen", "Offscreen"),
                ]),
        );
        let compiled = tree
            .compile(LogicalRect::new(5.0, 7.0, 100.0, 50.0))
            .unwrap();
        assert_eq!(
            compiled.semantic(&UiKey::new("partial")).unwrap().rect,
            LogicalRect::new(5.0, 47.0, 100.0, 10.0)
        );
        assert!(compiled.semantic(&UiKey::new("offscreen")).is_none());

        let projection = project_native_accessibility(&compiled, None).unwrap();
        assert_eq!(projection.update().nodes.len(), 3);
        assert!(projection.id_for_key(&UiKey::new("offscreen")).is_none());
        assert_eq!(
            node(&projection, "partial").bounds(),
            Some(Rect {
                x0: 5.0,
                y0: 47.0,
                x1: 105.0,
                y1: 57.0,
            })
        );
    }

    #[test]
    fn container_transform_maps_logical_bounds_to_window_physical_pixels() {
        let compiled = compiled_form(&["alpha"]);
        let transform = Affine::translate(accesskit::Vec2::new(0.0, 48.0)) * Affine::scale(2.0);
        let projection =
            project_native_accessibility_in_container(&compiled, None, transform).unwrap();
        let root = node(&projection, "app");
        assert_eq!(root.transform(), Some(&transform));
        assert_eq!(node(&projection, "alpha").transform(), None);
        assert_eq!(
            project_native_accessibility_in_container(&compiled, None, Affine::scale(0.0)),
            Err(AccessibilityProjectionError::InvalidContainerTransform)
        );
        assert_eq!(
            project_native_accessibility_in_container(&compiled, None, Affine::scale(f64::NAN)),
            Err(AccessibilityProjectionError::InvalidContainerTransform)
        );
    }

    #[test]
    fn composite_namespaces_identical_sibling_keys_and_retains_each_transform_and_owner() {
        let compiled = compiled_form(&["alpha"]);
        let first = AccessibilityOwner {
            view: ViewId::from_stored(41),
            generation: 7,
        };
        let second = AccessibilityOwner {
            view: ViewId::from_stored(42),
            generation: 9,
        };
        let first_transform =
            Affine::translate(accesskit::Vec2::new(12.0, 64.0)) * Affine::scale(1.0);
        let second_transform =
            Affine::translate(accesskit::Vec2::new(412.0, 64.0)) * Affine::scale(2.0);
        let first_projection = project_native_accessibility_for_view_in_container(
            &compiled,
            None,
            first_transform,
            first,
        )
        .unwrap();
        let second_projection = project_native_accessibility_for_view_in_container(
            &compiled,
            None,
            second_transform,
            second,
        )
        .unwrap();
        let first_root = first_projection.update().tree.as_ref().unwrap().root;
        let second_root = second_projection.update().tree.as_ref().unwrap().root;
        let first_alpha = first_projection.id_for_key(&UiKey::new("alpha")).unwrap();
        let second_alpha = second_projection.id_for_key(&UiKey::new("alpha")).unwrap();
        assert_ne!(first_root, second_root);
        assert_ne!(first_alpha, second_alpha);

        let projection = compose_native_accessibility(
            vec![(first, first_projection), (second, second_projection)],
            Some(second.view),
            Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 900.0,
                y1: 700.0,
            },
            "Settings \u{2014} aterm",
        )
        .unwrap();
        let window_root = projection.update().tree.as_ref().unwrap().root;
        let window = projection
            .update()
            .nodes
            .iter()
            .find(|(id, _)| *id == window_root)
            .unwrap();
        assert_eq!(window.1.role(), Role::Window);
        // The accessible window is named what the titlebar names it, on the native
        // branch exactly as on the terminal one — a user with several windows open has
        // nothing else to tell them apart by ear.
        assert_eq!(window.1.label(), Some("Settings \u{2014} aterm"));
        assert_eq!(window.1.children(), [first_root, second_root]);
        assert_eq!(
            projection.update().nodes.len(),
            compiled.semantics.len() * 2 + 1
        );
        assert_eq!(
            projection
                .update()
                .nodes
                .iter()
                .find(|(id, _)| *id == first_root)
                .unwrap()
                .1
                .transform(),
            Some(&first_transform)
        );
        assert_eq!(
            projection
                .update()
                .nodes
                .iter()
                .find(|(id, _)| *id == second_root)
                .unwrap()
                .1
                .transform(),
            Some(&second_transform)
        );
        assert_eq!(
            projection.route_for_node(first_alpha).unwrap().owner,
            Some(first)
        );
        assert_eq!(
            projection.route_for_node(second_alpha).unwrap().owner,
            Some(second)
        );
        assert_eq!(
            projection.update().focus,
            stable_node_id_for_view(second.view, &UiKey::new("app"))
        );
    }

    #[test]
    fn virtual_text_is_one_node_with_bounded_addressable_ranges() {
        let visible = crate::native_editor::EditorViewportProjection {
            first_line: 7,
            total_lines: 20,
            lines: vec![crate::native_editor::EditorViewportLine {
                number: 7,
                source: 100..105,
                column_start: 0,
                text: "aé\tz".to_string(),
                selections: vec![crate::native_editor::EditorSelectionSpan {
                    bytes: 1..4,
                    continues: false,
                    primary: true,
                }],
                carets: vec![(4, true)],
                syntax: Vec::new(),
                diagnostics: Vec::new(),
            }],
        };
        let tree = UiTree::new(
            UiNode::new(
                "editor",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .children(vec![UiNode::new(
                "editor/document",
                UiContent::TextViewport(TextViewportSpec {
                    label: "README.md editor".to_string(),
                    document_key: "document:18446744073709551615".to_string(),
                    selectable: true,
                    projection: Some(visible),
                    preedit: String::new(),
                    status: Some("1 config error · Save blocked".to_string()),
                    semantic_status: Some(
                        "Config error · Ln 7, Col 3 · complete diagnostic message".to_string(),
                    ),
                    minibuffer: None,
                    cursor_label: None,
                    dirty: false,
                    saving: false,
                    focused: false,
                    action: None,
                }),
            )]),
        );
        let compiled = tree
            .compile(LogicalRect::new(0.0, 0.0, 800.0, 600.0))
            .unwrap();
        let projection = project_native_accessibility(&compiled, None).unwrap();
        assert_eq!(projection.update().nodes.len(), 4);
        assert_eq!(projection.virtual_text().len(), 1);
        let target = &projection.virtual_text()[0];
        let document = node(&projection, "editor/document");
        assert_eq!(document.role(), Role::MultilineTextInput);
        assert_eq!(document.label(), Some("README.md editor"));
        assert_eq!(document.value(), None, "provider identity is not announced");
        assert!(document.supports_action(Action::SetTextSelection));
        assert!(document.supports_action(Action::ReplaceSelectedText));
        assert!(document.supports_action(Action::ScrollDown));
        assert_eq!(target.visible_range, 100..105);
        assert_eq!(target.lines.len(), 1);
        let line = &target.lines[0];
        let cell_w =
            crate::native_ui::text_viewport_geometry(LogicalRect::new(0.0, 0.0, 800.0, 600.0))
                .cell_w;
        let expected_positions = [0.0, cell_w, 2.0 * cell_w, 4.0 * cell_w];
        let expected_widths = [cell_w, cell_w, 2.0 * cell_w, cell_w];
        assert_eq!(line.source, 100..105);
        assert_eq!(line.character_source_offsets, [100, 101, 103, 104, 105]);
        assert_eq!(line.character_positions, expected_positions);
        assert_eq!(line.character_widths, expected_widths);
        assert_eq!(
            target.primary_selection,
            Some(VirtualTextSelection {
                anchor_byte: 101,
                focus_byte: 104,
            })
        );
        assert_eq!(
            document.text_selection(),
            Some(&TextSelection {
                anchor: TextPosition {
                    node: line.node,
                    character_index: 1,
                },
                focus: TextPosition {
                    node: line.node,
                    character_index: 3,
                },
            })
        );
        let text_run = projection
            .update()
            .nodes
            .iter()
            .find(|(node, _)| *node == line.node)
            .map(|(_, node)| node)
            .unwrap();
        assert_eq!(text_run.role(), Role::TextRun);
        assert_eq!(text_run.value(), Some("aé\tz"));
        assert_eq!(text_run.character_lengths(), [1, 2, 1, 1]);
        assert_eq!(
            text_run.character_positions(),
            Some(&expected_positions[..])
        );
        assert_eq!(text_run.character_widths(), Some(&expected_widths[..]));
        assert!(!text_run.is_read_only());
        let status_id = stable_node_id(&UiKey::new("editor/document/status"));
        let status = projection
            .update()
            .nodes
            .iter()
            .find(|(id, _)| *id == status_id)
            .map(|(_, node)| node)
            .expect("editor status live region");
        assert_eq!(status.role(), Role::Status);
        assert_eq!(status.live(), Some(Live::Polite));
        assert_eq!(
            status.value(),
            Some("Config error · Ln 7, Col 3 · complete diagnostic message")
        );
        // AT-SPI and UIA announce a live region by its NAME and only when the name
        // moves, so the diagnostic has to be the name too; "Editor status" is the fixed
        // caption and belongs in the description, where it is a property change and
        // never an announcement.
        assert_eq!(
            status.label(),
            Some("Config error · Ln 7, Col 3 · complete diagnostic message")
        );
        assert_eq!(status.description(), Some("Editor status"));

        let published = PublishedNativeAccessibility::with_virtual_text(
            ViewId::from_stored(7),
            1,
            projection.routes().to_vec(),
            projection.virtual_text().to_vec(),
        );
        let replacement = accesskit::ActionRequest {
            action: Action::ReplaceSelectedText,
            target_tree: TreeId::ROOT,
            target_node: target.node,
            data: Some(accesskit::ActionData::Value("replacement".into())),
        };
        assert_eq!(
            route_accessibility_action(&published, &replacement),
            Ok(RoutedAccessibilityAction::ReplaceSelectedText {
                key: UiKey::new("editor/document"),
                text: "replacement".to_string(),
            })
        );
        let selection = accesskit::ActionRequest {
            action: Action::SetTextSelection,
            target_tree: TreeId::ROOT,
            target_node: target.node,
            data: Some(accesskit::ActionData::SetTextSelection(TextSelection {
                anchor: TextPosition {
                    node: line.node,
                    character_index: 0,
                },
                focus: TextPosition {
                    node: line.node,
                    character_index: 2,
                },
            })),
        };
        assert_eq!(
            route_accessibility_action(&published, &selection),
            Ok(RoutedAccessibilityAction::SetTextSelection {
                key: UiKey::new("editor/document"),
                anchor_byte: 100,
                focus_byte: 103,
            })
        );

        let request = projection
            .request_virtual_text(target.node, 9_000_000..9_000_200)
            .unwrap();
        assert_eq!(request.document_key, target.document_key);
        assert_eq!(request.range, 9_000_000..9_000_200);
        assert_eq!(
            projection
                .request_virtual_text(target.node, 0..MAX_VIRTUAL_TEXT_RANGE.saturating_add(1)),
            Err(VirtualTextError::RangeTooLarge)
        );
        let reversed = Range { start: 9, end: 2 };
        assert_eq!(
            projection.request_virtual_text(target.node, reversed),
            Err(VirtualTextError::ReversedRange)
        );
        assert_eq!(
            projection.request_virtual_text(NodeId(42), 0..1),
            Err(VirtualTextError::UnknownNode)
        );
    }

    #[test]
    fn virtual_text_publication_is_bounded_to_rows_intersecting_the_viewport() {
        let lines = (0..2_048)
            .map(|number| crate::native_editor::EditorViewportLine {
                number,
                source: number * 2..number * 2 + 1,
                column_start: 0,
                text: "x".to_string(),
                selections: Vec::new(),
                carets: Vec::new(),
                syntax: Vec::new(),
                diagnostics: Vec::new(),
            })
            .collect();
        let tree = UiTree::new(
            UiNode::new(
                "editor",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .children(vec![UiNode::new(
                "editor/document",
                UiContent::TextViewport(TextViewportSpec {
                    label: "large.txt".to_string(),
                    document_key: "document:1@7".to_string(),
                    selectable: true,
                    projection: Some(crate::native_editor::EditorViewportProjection {
                        first_line: 0,
                        total_lines: 10_000_000,
                        lines,
                    }),
                    preedit: String::new(),
                    status: None,
                    semantic_status: None,
                    minibuffer: None,
                    cursor_label: None,
                    dirty: false,
                    saving: false,
                    focused: false,
                    action: None,
                }),
            )]),
        );
        let mut compiled = tree
            .compile(LogicalRect::new(0.0, 0.0, 800.0, 200.0))
            .unwrap();
        let projection = project_native_accessibility(&compiled, None).unwrap();
        let target = &projection.virtual_text()[0];

        assert_eq!(target.lines.len(), 7);
        assert_eq!(target.lines.last().unwrap().source, 12..13);
        assert_eq!(projection.update().nodes.len(), 2 + target.lines.len());
        assert_eq!(projection.source_nodes_visited(), 2);

        let paint = compiled
            .paint
            .iter_mut()
            .find(|paint| paint.key == UiKey::new("editor/document"))
            .unwrap();
        let UiContent::TextViewport(spec) = &mut paint.content else {
            panic!("text viewport paint node");
        };
        spec.projection.as_mut().unwrap().lines[0].source.end += 1;
        assert!(matches!(
            project_native_accessibility(&compiled, None),
            Err(AccessibilityProjectionError::InvalidVirtualText(_))
        ));
    }

    #[test]
    fn action_routes_preserve_stable_key_and_typed_reducer_action() {
        let compiled = compiled_form(&["save"]);
        let projection = project_native_accessibility(&compiled, None).unwrap();
        let id = projection.id_for_key(&UiKey::new("save")).unwrap();
        let route = projection.route_for_node(id).unwrap();
        assert_eq!(route.key, UiKey::new("save"));
        assert_eq!(route.action.as_ref().unwrap().as_str(), "activate/save");
        assert_eq!(node(&projection, "save").role(), Role::Button);
        assert!(node(&projection, "save").supports_action(Action::Click));
    }

    #[test]
    fn published_routes_validate_tree_node_action_and_typed_values() {
        let switch = Control::new(
            SwitchSpec {
                label: "Enabled".to_string(),
                description: None,
            },
            ActionId::new("set/enabled"),
        )
        .value(SemanticValue::Bool(false));
        let slider = Control::new(
            SliderSpec {
                label: "Size".to_string(),
                step: 1.0,
                display_value: "12".to_string(),
            },
            ActionId::new("set/size"),
        )
        .value(SemanticValue::Number {
            value: 12.0,
            minimum: 6.0,
            maximum: 32.0,
        });
        let tree = UiTree::new(
            UiNode::new("app", UiContent::Group(GroupSpec::new("app")))
                .layout(Layout::column())
                .children(vec![
                    UiNode::new("enabled", UiContent::Switch(switch))
                        .layout(Layout::default().height(Length::Fixed(40.0))),
                    UiNode::new("size", UiContent::Slider(slider))
                        .layout(Layout::default().height(Length::Fixed(40.0))),
                ]),
        );
        let compiled = tree
            .compile(LogicalRect::new(0.0, 0.0, 200.0, 80.0))
            .unwrap();
        let projection = project_native_accessibility(&compiled, None).unwrap();
        let enabled = projection.id_for_key(&UiKey::new("enabled")).unwrap();
        let size = projection.id_for_key(&UiKey::new("size")).unwrap();
        let (_, routes) = projection.into_update_and_routes();
        let published = PublishedNativeAccessibility::new(ViewId::from_stored(7), 3, routes);

        let click = accesskit::ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: enabled,
            data: None,
        };
        assert_eq!(
            route_accessibility_action(&published, &click),
            Ok(RoutedAccessibilityAction::Activate {
                key: UiKey::new("enabled"),
                action: ActionId::new("set/enabled"),
                value: None,
            })
        );

        let set_size = accesskit::ActionRequest {
            action: Action::SetValue,
            target_tree: TreeId::ROOT,
            target_node: size,
            data: Some(accesskit::ActionData::NumericValue(18.0)),
        };
        assert_eq!(
            route_accessibility_action(&published, &set_size),
            Ok(RoutedAccessibilityAction::Activate {
                key: UiKey::new("size"),
                action: ActionId::new("set/size"),
                value: Some(crate::native_app::SemanticInput::Number(18.0)),
            })
        );

        let wrong_kind = accesskit::ActionRequest {
            data: Some(accesskit::ActionData::Value("18".into())),
            ..set_size.clone()
        };
        assert_eq!(
            route_accessibility_action(&published, &wrong_kind),
            Err(AccessibilityActionError::WrongValueKind)
        );
        let unsupported = accesskit::ActionRequest {
            action: Action::Increment,
            ..set_size
        };
        assert_eq!(
            route_accessibility_action(&published, &unsupported),
            Err(AccessibilityActionError::UnsupportedAction)
        );
    }

    #[test]
    fn malformed_semantics_and_hash_collisions_fail_closed() {
        assert!(matches!(
            project_native_accessibility(&CompiledUi::default(), None),
            Err(AccessibilityProjectionError::EmptyTree)
        ));

        let compiled = compiled_form(&["alpha", "beta"]);
        assert!(matches!(
            project_with_ids(&compiled, None, |_| NodeId(7)),
            Err(AccessibilityProjectionError::IdCollision { .. })
        ));

        let missing_parent = CompiledUi {
            semantics: vec![semantic(
                "child",
                Some("absent"),
                SemanticRole::Text,
                SemanticValue::None,
            )],
            ..CompiledUi::default()
        };
        assert_eq!(
            project_native_accessibility(&missing_parent, None),
            Err(AccessibilityProjectionError::MissingParent {
                node: UiKey::new("child"),
                parent: UiKey::new("absent"),
            })
        );

        let two_roots = CompiledUi {
            semantics: vec![
                semantic("one", None, SemanticRole::Group, SemanticValue::None),
                semantic("two", None, SemanticRole::Group, SemanticValue::None),
            ],
            ..CompiledUi::default()
        };
        assert_eq!(
            project_native_accessibility(&two_roots, None),
            Err(AccessibilityProjectionError::MultipleRoots)
        );
    }

    #[test]
    fn stale_focus_falls_back_to_root_and_invalid_numbers_fail_closed() {
        let compiled = compiled_form(&["alpha"]);
        let projection =
            project_native_accessibility(&compiled, Some(&UiKey::new("gone"))).unwrap();
        assert_eq!(
            projection.update().focus,
            projection.id_for_key(&UiKey::new("app")).unwrap()
        );

        let invalid = CompiledUi {
            semantics: vec![semantic(
                "slider",
                None,
                SemanticRole::Slider,
                SemanticValue::Number {
                    value: f64::NAN,
                    minimum: 0.0,
                    maximum: 1.0,
                },
            )],
            ..CompiledUi::default()
        };
        assert_eq!(
            project_native_accessibility(&invalid, None),
            Err(AccessibilityProjectionError::InvalidNumber(UiKey::new(
                "slider"
            )))
        );
    }

    #[test]
    fn layout_flow_import_is_the_compiled_source_not_an_adapter_concern() {
        // Compile-time/exhaustiveness witness: the adapter accepts the result of every flow
        // without receiving a Flow itself, so it cannot run a parallel layout traversal.
        for flow in [Flow::Overlay, Flow::Row, Flow::Column] {
            let tree = UiTree::new(
                UiNode::new("root", UiContent::Group(GroupSpec::new("root"))).layout(Layout {
                    flow,
                    ..Layout::default()
                }),
            );
            let compiled = tree
                .compile(LogicalRect::new(0.0, 0.0, 20.0, 20.0))
                .unwrap();
            assert!(project_native_accessibility(&compiled, None).is_ok());
        }
    }
}
