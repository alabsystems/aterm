// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Accessibility projection from [`CompiledUi`] without walking [`crate::native_ui::UiTree`].

use std::ops::Range;

use aterm_hash::{FxHashMap, FxHashSet};

use aterm_a11y::model::{
    Action, ActionContext, Affine, Invalid, Live, Node, NodeId, Rect, Role, TextPosition,
    TextSelection, Toggled, Tree, TreeId, TreeUpdate,
};

use crate::native_ui::{
    ActionId, CompiledUi, ControlState, LogicalRect, SemanticNode, SemanticRole, SemanticValue,
    TextViewportSpec, UiContent, UiKey,
};
use crate::tab_model::ViewId;

/// A single platform range request is bounded even when its document is enormous.
#[cfg(test)]
pub(crate) const MAX_VIRTUAL_TEXT_RANGE: u64 = 1024 * 1024;

/// Deterministically derive an accessibility node identity from a stable semantic key.
#[cfg(test)]
pub(crate) fn stable_node_id(key: &UiKey) -> NodeId {
    let mut hash = aterm_hash::Fnv1a64::new();
    hash.write(b"aterm/native-a11y/v1\0");
    hash.write(key.as_str().as_bytes());
    NodeId(hash.finish().max(1))
}

/// Deterministically namespace a semantic identity by its stable native view.
pub(crate) fn stable_node_id_for_view(view: ViewId, key: &UiKey) -> NodeId {
    let mut hash = aterm_hash::Fnv1a64::new();
    hash.write(b"aterm/native-a11y/view/v1\0");
    hash.write(&view.get().to_le_bytes());
    hash.write(key.as_str().as_bytes());
    NodeId(hash.finish().max(1))
}

fn composite_root_id() -> NodeId {
    NodeId(aterm_hash::fnv1a_64(b"aterm/native-a11y/window/v1").max(1))
}

/// Lifecycle and review identity attached to every published native route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AccessibilityOwner {
    pub(crate) view: ViewId,
    pub(crate) generation: u64,
    pub(crate) review_revision: u64,
}

impl AccessibilityOwner {
    const fn context(self) -> ActionContext {
        ActionContext::native_review(self.view.get(), self.generation, self.review_revision)
    }
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

/// One virtual text surface resolved against `document_key`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VirtualTextTarget {
    pub(crate) node: NodeId,
    pub(crate) key: UiKey,
    pub(crate) document_key: String,
    pub(crate) visible_bounds: LogicalRect,
    /// Smallest canonical UTF-8 byte interval covering the projected rows.
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
    /// One canonical byte offset per model character, plus the end offset.
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
#[cfg(test)]
pub(crate) struct VirtualTextRequest {
    pub(crate) node: NodeId,
    pub(crate) key: UiKey,
    pub(crate) document_key: String,
    /// UTF-8 byte range in the immutable document snapshot selected by the host.
    pub(crate) range: Range<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(test)]
pub(crate) enum VirtualTextError {
    UnknownNode,
    ReversedRange,
    RangeTooLarge,
}

/// Pure projection product. `update` can be sent directly to a platform adapter; routes
/// carry platform action requests back to stable `UiKey`s.
#[derive(Debug, PartialEq)]
pub(crate) struct NativeAccessibilityProjection {
    update: TreeUpdate,
    routes: Vec<AccessibilityRoute>,
    virtual_text: Vec<VirtualTextTarget>,
}

impl NativeAccessibilityProjection {
    #[cfg(test)]
    pub(crate) fn update(&self) -> &TreeUpdate {
        &self.update
    }

    #[cfg(test)]
    pub(crate) fn into_update(self) -> TreeUpdate {
        self.update
    }

    #[cfg(test)]
    pub(crate) fn into_update_and_routes(self) -> (TreeUpdate, Vec<AccessibilityRoute>) {
        (self.update, self.routes)
    }

    pub(crate) fn into_update_routes_and_virtual_text(
        self,
    ) -> (TreeUpdate, Vec<AccessibilityRoute>, Vec<VirtualTextTarget>) {
        (self.update, self.routes, self.virtual_text)
    }

    #[cfg(test)]
    pub(crate) fn routes(&self) -> &[AccessibilityRoute] {
        &self.routes
    }

    #[cfg(test)]
    pub(crate) fn virtual_text(&self) -> &[VirtualTextTarget] {
        &self.virtual_text
    }

    #[cfg(test)]
    pub(crate) fn id_for_key(&self, key: &UiKey) -> Option<NodeId> {
        self.routes
            .iter()
            .find(|route| &route.key == key)
            .map(|route| route.node)
    }

    #[cfg(test)]
    pub(crate) fn route_for_node(&self, node: NodeId) -> Option<&AccessibilityRoute> {
        self.routes.iter().find(|route| route.node == node)
    }

    /// Build a bounded range request for the host's virtual text provider.
    #[cfg(test)]
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
    window_root_index: usize,
}

impl StagedNativeAccessibility {
    pub(crate) const fn new(
        owners: Vec<AccessibilityOwner>,
        projection: NativeAccessibilityProjection,
    ) -> Self {
        let window_root_index = projection.update.nodes.len().saturating_sub(1);
        Self {
            owners,
            projection,
            window_root_index,
        }
    }

    /// Returns whether the staged composite still matches the active owners and window title.
    pub(crate) fn matches(&self, owners: &[AccessibilityOwner], window_title: &str) -> bool {
        let root = self.projection.update.tree.as_ref().map(|tree| tree.root);
        self.owners.as_slice() == owners
            && self
                .projection
                .update
                .nodes
                .get(self.window_root_index)
                .is_some_and(|(node, value)| {
                    Some(*node) == root
                        && value.label() == Some(normalized_window_title(window_title))
                })
    }
}

const fn normalized_window_title(title: &str) -> &str {
    if title.is_empty() { "aterm" } else { title }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PublishedNativeAccessibility {
    /// Primary owner: the fallback for an unqualified route when there is a single owner.
    pub(crate) view: ViewId,
    pub(crate) generation: u64,
    owners: Vec<AccessibilityOwner>,
    routes: Vec<AccessibilityRoute>,
    virtual_text: Vec<VirtualTextTarget>,
    qualified_routes: bool,
}

impl PublishedNativeAccessibility {
    /// The first line of the first text surface this table routes, as a client read it.
    #[cfg(test)]
    pub(crate) fn first_text_line(&self) -> Option<VirtualTextLine> {
        self.virtual_text.first()?.lines.first().cloned()
    }

    #[cfg(test)]
    pub(crate) fn new(view: ViewId, generation: u64, routes: Vec<AccessibilityRoute>) -> Self {
        let owner = AccessibilityOwner {
            view,
            generation,
            review_revision: 0,
        };
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

    #[cfg(test)]
    pub(crate) fn with_virtual_text(
        view: ViewId,
        generation: u64,
        routes: Vec<AccessibilityRoute>,
        virtual_text: Vec<VirtualTextTarget>,
    ) -> Self {
        let owner = AccessibilityOwner {
            view,
            generation,
            review_revision: 0,
        };
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

    pub(crate) const fn composite(
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
            (self.owners.len() == 1).then(|| AccessibilityOwner {
                view: self.view,
                generation: self.generation,
                review_revision: self.owners[0].review_revision,
            })
        })?;
        (self.owners.contains(&owner)
            && (!self.qualified_routes || stable_node_id_for_view(owner.view, &route.key) == node))
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

    #[cfg(test)]
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
    /// Scroll the list that holds `key` off screen until it shows.
    ScrollIntoView {
        key: UiKey,
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
    WrongOwnerContext,
}

/// Validate and lower a platform request against the exact route table that was published.
pub(crate) fn route_accessibility_action(
    published: &PublishedNativeAccessibility,
    request: &aterm_a11y::model::ActionRequest,
) -> Result<RoutedAccessibilityAction, AccessibilityActionError> {
    if request.target_tree != TreeId::ROOT {
        return Err(AccessibilityActionError::WrongTree);
    }
    let route = published
        .route(request.target_node)
        .ok_or(AccessibilityActionError::UnknownNode)?;
    let owner = published
        .route_owner(request.target_node)
        .ok_or(AccessibilityActionError::WrongOwnerContext)?;
    let expected_context = owner.context();
    if request.context != Some(expected_context) {
        return Err(AccessibilityActionError::WrongOwnerContext);
    }
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
                (
                    SemanticRole::Slider,
                    Some(aterm_a11y::model::ActionData::NumericValue(value)),
                ) => {
                    if !value.is_finite() {
                        return Err(AccessibilityActionError::NonFiniteValue);
                    }
                    crate::native_app::SemanticInput::Number(*value)
                }
                (SemanticRole::TextField, Some(aterm_a11y::model::ActionData::Value(value))) => {
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
        Action::ScrollIntoView => Ok(RoutedAccessibilityAction::ScrollIntoView {
            key: route.key.clone(),
        }),
        Action::ScrollUp => Ok(RoutedAccessibilityAction::Scroll {
            key: route.key.clone(),
            lines: -3,
        }),
        Action::ScrollDown => Ok(RoutedAccessibilityAction::Scroll {
            key: route.key.clone(),
            lines: 3,
        }),
        Action::ReplaceSelectedText => match (route.role, request.data.as_ref()) {
            (SemanticRole::TextViewport, Some(aterm_a11y::model::ActionData::Value(value))) => {
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
                Some(aterm_a11y::model::ActionData::SetTextSelection(selection)),
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
    /// The viewport's own children, in reading order; each is one of `nodes`.
    children: Vec<NodeId>,
    /// Every node under the viewport, its children's children included.
    nodes: Vec<PendingNode>,
    selection: Option<TextSelection>,
}

/// What a concealed text view ([`TextViewportSpec::concealed`]) publishes in place of its lines.
pub(crate) const PRIVATE_TEXT_WITHHELD: &str = "Private document: its text is not read out";

/// Claim the node id of `key`, refusing a second key with the same id.
fn claim_node(
    key: &UiKey,
    id_for: &mut impl FnMut(&UiKey) -> NodeId,
    by_id: &mut FxHashMap<NodeId, UiKey>,
) -> Result<NodeId, AccessibilityProjectionError> {
    let id = id_for(key);
    if let Some(first) = by_id.insert(id, key.clone()) {
        return Err(AccessibilityProjectionError::IdCollision {
            first,
            second: key.clone(),
        });
    }
    Ok(id)
}

/// A rectangle of logical points as a node's bounds.
fn node_bounds(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
    Rect {
        x0: f64::from(x0),
        y0: f64::from(y0),
        x1: f64::from(x1.max(x0)),
        y1: f64::from(y1.max(y0)),
    }
}

/// A grid's table as its readers other than the person's eyes get it (one-mesh design §4.8,
/// "Revealed values stay out of the accessibility tree"): a [`Role::Grid`] named by its shape,
/// holding a row of the shown columns' headers and a row per shown record, each cell a
/// [`Role::Label`] whose label is its value. A concealed cell's label is
/// [`crate::native_editor::HIDDEN_VALUE`] and it carries no value: `reading` never held one.
/// Returns the grid's id; its nodes join `pending`.
#[expect(
    clippy::too_many_arguments,
    reason = "the projection's id and collision maps travel with the grid they place"
)]
fn materialize_grid(
    key: &UiKey,
    rect: LogicalRect,
    geometry: &crate::native_ui::TextViewportGeometry,
    grid: &crate::native_editor::GridProjection,
    reading: &crate::native_editor::GridReading,
    id_for: &mut impl FnMut(&UiKey) -> NodeId,
    by_id: &mut FxHashMap<NodeId, UiKey>,
    pending: &mut Vec<PendingNode>,
) -> Result<NodeId, AccessibilityProjectionError> {
    use crate::native_editor::CellReading;
    let (data_rows, _) = crate::native_ui::editor_grid_slots(geometry, grid);
    let column_name = |column: usize, header: &CellReading| match header {
        CellReading::Value(header) if !amacs::table::first_line(header).trim().is_empty() => {
            amacs::table::first_line(header).trim().to_string()
        }
        _ => format!("column {}", column + 1),
    };
    let cell_x = |column: usize| {
        grid.shown
            .iter()
            .find(|shown| shown.index == column)
            .map_or((geometry.text_x, geometry.text_x), |shown| {
                let x0 = geometry.text_x + shown.x as f32 * geometry.cell_w;
                (x0, x0 + (shown.width + 2) as f32 * geometry.cell_w)
            })
    };
    let right = rect.right();
    let bottom = geometry.body_y + geometry.body_h;
    let row_y = |slot: usize| {
        let y0 = (geometry.body_y + slot as f32 * geometry.line_h).min(bottom);
        (y0, (y0 + geometry.line_h).min(bottom))
    };

    let grid_key = UiKey::new(format!("{}/grid", key.as_str()));
    let grid_id = claim_node(&grid_key, id_for, by_id)?;
    let mut table = Node::new(Role::Grid);
    table.set_label("Table");
    table.set_description(format!(
        "{} rows, {} columns",
        reading.rows, reading.columns
    ));
    table.set_read_only();
    table.set_bounds(node_bounds(geometry.text_x, geometry.body_y, right, bottom));
    let grid_index = pending.len();
    pending.push(PendingNode {
        id: grid_id,
        node: table,
        children: Vec::new(),
    });

    let mut rows = Vec::with_capacity(reading.records.len() + 1);
    let header_row = std::iter::once((
        0usize,
        "Column headers".to_string(),
        reading
            .headers
            .iter()
            .map(|(column, header)| (*column, header))
            .collect::<Vec<_>>(),
    ));
    let data = reading
        .records
        .iter()
        .take(data_rows)
        .enumerate()
        .map(|(slot, (record, cells))| {
            (
                slot + 1,
                format!("Row {}", record + 1),
                reading
                    .headers
                    .iter()
                    .map(|(column, _)| *column)
                    .zip(cells)
                    .collect::<Vec<_>>(),
            )
        });
    for (slot, label, cells) in header_row.chain(data) {
        let row_key = UiKey::new(format!("{}/{slot}", grid_key.as_str()));
        let row_id = claim_node(&row_key, id_for, by_id)?;
        let (y0, y1) = row_y(slot);
        let mut row = Node::new(Role::Group);
        row.set_label(label);
        row.set_bounds(node_bounds(geometry.text_x, y0, right, y1));
        let mut children = Vec::with_capacity(cells.len());
        let mut cell_nodes = Vec::with_capacity(cells.len());
        for (column, cell) in cells {
            let cell_key = UiKey::new(format!("{}/{column}", row_key.as_str()));
            let cell_id = claim_node(&cell_key, id_for, by_id)?;
            let (x0, x1) = cell_x(column);
            let mut node = Node::new(Role::Label);
            node.set_label(cell.words().to_string());
            node.set_description(if slot == 0 {
                format!("column {}", column + 1)
            } else {
                reading
                    .headers
                    .iter()
                    .find(|(shown, _)| *shown == column)
                    .map_or_else(
                        || format!("column {}", column + 1),
                        |(_, header)| column_name(column, header),
                    )
            });
            node.set_bounds(node_bounds(x0, y0, x1.min(right), y1));
            children.push(cell_id);
            cell_nodes.push(PendingNode {
                id: cell_id,
                node,
                children: Vec::new(),
            });
        }
        rows.push(row_id);
        pending.push(PendingNode {
            id: row_id,
            node: row,
            children,
        });
        pending.extend(cell_nodes);
    }
    pending[grid_index].children = rows;
    Ok(grid_id)
}

fn materialize_visible_text(
    parent: NodeId,
    key: &UiKey,
    rect: LogicalRect,
    spec: &TextViewportSpec,
    id_for: &mut impl FnMut(&UiKey) -> NodeId,
    by_id: &mut FxHashMap<NodeId, UiKey>,
) -> Result<VisibleTextMaterialization, AccessibilityProjectionError> {
    let geometry = spec.geometry(rect);
    let mut pending = Vec::new();
    let mut children = Vec::new();
    let mut lines = Vec::new();
    let mut selected_start = None::<u64>;
    let mut selected_end = None::<u64>;
    let mut primary_caret = None::<u64>;

    // A concealed document's text lines are drawn for the person's eyes only: its readers get
    // that its text is withheld, never a line of it (one-mesh design §4.8).
    if spec.concealed && spec.projection.is_some() {
        let withheld_key = UiKey::new(format!("{}/withheld", key.as_str()));
        let withheld_id = claim_node(&withheld_key, id_for, by_id)?;
        let mut withheld = Node::new(Role::Label);
        withheld.set_label(PRIVATE_TEXT_WITHHELD);
        withheld.set_bounds(node_bounds(
            geometry.text_x,
            geometry.body_y,
            rect.right(),
            geometry.body_y + geometry.body_h,
        ));
        children.push(withheld_id);
        pending.push(PendingNode {
            id: withheld_id,
            node: withheld,
            children: Vec::new(),
        });
    } else if geometry.body_h > 0.0
        && geometry.text_x < rect.right()
        && let Some(projection) = spec.projection.as_ref()
    {
        let visible = crate::native_ui::whole_editor_rows(&geometry).min(projection.lines.len());
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
            children.push(line_id);
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

    // A grid draws no text lines: its readers get its table and its current cell, announced as
    // it changes, from its reading, where a concealed cell is `hidden value` and carries no
    // value (one-mesh design §4.8). (The viewport's own value stays its document key: the
    // virtual-text contract resolves the surface by it.)
    if let (Some(grid), Some(reading)) = (spec.grid.as_ref(), spec.grid_reading.as_deref()) {
        children.push(materialize_grid(
            key,
            rect,
            &geometry,
            grid,
            reading,
            id_for,
            by_id,
            &mut pending,
        )?);
        let cell_key = UiKey::new(format!("{}/grid-cell", key.as_str()));
        let cell_id = claim_node(&cell_key, id_for, by_id)?;
        let mut cell_node = Node::new(Role::Label);
        cell_node.set_description("Current cell");
        announce_live(&mut cell_node, Live::Polite, &reading.cursor_words());
        cell_node.set_bounds(Rect {
            x0: f64::from(geometry.text_x),
            y0: f64::from(geometry.body_y),
            x1: f64::from(rect.right()),
            y1: f64::from(geometry.body_y + geometry.body_h),
        });
        children.push(cell_id);
        pending.push(PendingNode {
            id: cell_id,
            node: cell_node,
            children: Vec::new(),
        });
    }

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
        status_node.set_description("Editor status");
        announce_live(&mut status_node, Live::Polite, status);
        status_node.set_bounds(Rect {
            x0: f64::from(rect.x),
            y0: f64::from(rect.bottom() - geometry.footer_h),
            x1: f64::from(rect.right()),
            y1: f64::from(rect.bottom()),
        });
        children.push(status_id);
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
        children,
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
#[cfg(test)]
pub(crate) fn project_native_accessibility(
    compiled: &CompiledUi,
    focused: Option<&UiKey>,
) -> Result<NativeAccessibilityProjection, AccessibilityProjectionError> {
    project_with_ids(compiled, focused, stable_node_id)
}

/// Project logical native coordinates into physical-pixel window coordinates.
#[cfg(test)]
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

/// Project one visible native leaf into window coordinates qualified by `owner`.
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
    let context = owner.context();
    for (_, node) in &mut projection.update.nodes {
        node.set_action_context(context);
    }
    Ok(projection)
}

/// Publish all visible native split leaves as one accessibility window tree.
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
    let mut seen_views =
        aterm_hash::FxHashSet::with_capacity_and_hasher(projections.len(), Default::default());
    let mut seen_nodes = aterm_hash::FxHashSet::default();
    let mut child_roots = Vec::with_capacity(projections.len());
    let mut nodes = Vec::new();
    let mut routes = Vec::new();
    let mut virtual_text = Vec::new();
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
    }

    let mut window = Node::new(Role::Window);
    window.set_label(normalized_window_title(window_title));
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
    })
}

#[cfg(test)]
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

    for semantic in &compiled.semantics {
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
        node.set_bounds(rect_to_model(semantic.rect));
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
            pending[index].children.extend(visible.children);
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

    if let Some(description) = &semantic.description {
        node.set_description(description.clone());
    }

    let enabled = semantic.state.is_none_or(|state| state.enabled);
    if let Some(state) = semantic.state {
        lower_state(&mut node, semantic.role, state);
    }
    lower_value(&mut node, semantic)?;
    // A Text node lowers to a Label, whose words AppKit and AT-SPI read from
    // its value (`label_comes_from_value`); UIA names it by the label it keeps.
    if semantic.role == SemanticRole::Text && node.value().is_none() && !semantic.label.is_empty() {
        node.set_value(semantic.label.clone());
    }

    if matches!(semantic.role, SemanticRole::RichText) {
        node.set_read_only();
    }
    if semantic.role == SemanticRole::Status {
        let announced = node
            .value()
            .map(str::to_string)
            .unwrap_or_else(|| semantic.label.clone());
        announce_live(&mut node, Live::Polite, &announced);
    }
    if enabled {
        lower_actions(&mut node, semantic, focusable);
    }
    if semantic.offscreen {
        node.set_offscreen();
        node.add_action(Action::ScrollIntoView);
    }
    Ok(node)
}

/// Mark `node` a live region that speaks `announced` on every platform, so it sets both its
/// label and its value.
fn announce_live(node: &mut Node, politeness: Live, announced: &str) {
    node.set_label(announced.to_string());
    node.set_value(announced.to_string());
    node.set_live(politeness);
}

const fn lower_role(role: SemanticRole) -> Role {
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
        SemanticRole::Navigation => Role::Navigation,
        SemanticRole::Status => Role::Status,
        SemanticRole::List => Role::List,
        SemanticRole::ListBox => Role::ListBox,
        SemanticRole::ListBoxOption => Role::ListBoxOption,
        SemanticRole::ProgressIndicator => Role::ProgressIndicator,
    }
}

fn lower_state(node: &mut Node, role: SemanticRole, state: ControlState) {
    if !state.enabled {
        node.set_disabled();
    }
    if state.selected || role == SemanticRole::ListBoxOption {
        node.set_selected(state.selected);
    }
    if state.invalid {
        node.set_invalid(Invalid::True);
    }
    if state.busy {
        node.set_busy();
    }
    if let Some(expanded) = state.expanded {
        node.set_expanded(expanded);
    }
}

fn lower_value(
    node: &mut Node,
    semantic: &SemanticNode,
) -> Result<(), AccessibilityProjectionError> {
    match &semantic.value {
        SemanticValue::None => {}
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
            node.set_value(
                semantic
                    .value_text
                    .clone()
                    .unwrap_or_else(|| value.to_string()),
            );
        }
    }
    Ok(())
}

const fn lower_actions(node: &mut Node, semantic: &SemanticNode, focusable: bool) {
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
        SemanticRole::RichText | SemanticRole::List | SemanticRole::ListBox => {
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
        | SemanticRole::Navigation
        | SemanticRole::ListBoxOption
        | SemanticRole::ProgressIndicator
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
        Action::ScrollIntoView,
        Action::ReplaceSelectedText,
        Action::SetTextSelection,
    ]
    .into_iter()
    .filter(|action| node.supports_action(*action))
    .collect()
}

fn rect_to_model(rect: LogicalRect) -> Rect {
    Rect {
        x0: f64::from(rect.x),
        y0: f64::from(rect.y),
        x1: f64::from(rect.right()),
        y1: f64::from(rect.bottom()),
    }
}

#[cfg(test)]
#[path = "native_accessibility_tests.rs"]
mod tests;
