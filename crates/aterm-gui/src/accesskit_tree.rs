// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cross-platform AccessKit projection for the compiled Settings semantic model.
//! Settings accessibility and `controls settings` both consume that native compiled
//! semantic tree. This module
//! maps [`crate::settings::SettingsState`] into an [`accesskit::TreeUpdate`] that
//! `accesskit_winit` fans out to the OS
//! accessibility APIs (UIA on Windows, AT-SPI on Linux, NSAccessibility on macOS), so a
//! screen reader and an AI consume the same semantic model that feeds the app-rendered
//! view.
//!
//! This module is the PURE mapping (model → `TreeUpdate`); it is unit-tested without any
//! window/adapter. The `accesskit_winit::Adapter` that attaches this to a live window and
//! pushes `update_if_active(|| settings_tree(state))` on change is the OS event-loop
//! wiring (runtime-verified with a real screen reader), built on top of this seam.
//!
//! Gated behind the OFF-BY-DEFAULT `a11y-accesskit` feature (see this crate's
//! `Cargo.toml`, which records why it left the default build). A stock Linux/Windows
//! build compiles none of this and publishes no accessibility tree at all — there is
//! no degraded fallback, because AccessKit is the only cross-platform publisher aterm
//! has. Everything below is therefore reachable only from
//! `cargo build -p aterm-gui --features a11y-accesskit`.

// macOS: AccessKit's NSAccessibility provider and the `a11y-appkit` grid publisher both
// claim the content view's accessibility tree — enabling both yields a corrupt/duplicated
// VoiceOver tree, so they are mutually exclusive.
#[cfg(all(target_os = "macos", feature = "a11y-appkit"))]
compile_error!(
    "features `a11y-appkit` and `a11y-accesskit` are mutually exclusive on macOS \
     (both claim the content view's accessibility tree); enable at most one"
);

use accesskit::{
    Action, Live, Node, NodeId, Rect, Role, TextDirection, TextPosition, TextSelection, Toggled,
    Tree, TreeId, TreeUpdate,
};

use crate::prefs::{EditField, EditKind};
use crate::settings::SettingsState;

/// The overlay's root accessibility node. Control nodes are `ROOT + 1 + field_index`.
const ROOT: NodeId = NodeId(0);
/// DEFAULT-2: the `Role::Terminal` node under [`ROOT`] when NO overlay is open and the
/// frame is ONE document — it carries the visible grid text so a screen reader reads the
/// terminal itself. A composed split publishes one [`PANE_BASE`] document per pane
/// instead, and this id is then absent from the tree.
const GRID: NodeId = NodeId(1);
/// The in-grid tab strip's `Role::TabList` under [`ROOT`], published only when the strip
/// is actually on screen AND is a switcher (≥ 2 tabs — a solo strip paints the window
/// title, not a switcher, so publishing a one-item tab list would describe a control the
/// user cannot see).
const TAB_LIST: NodeId = NodeId(2);
/// The find panel's `Role::SearchInput` under [`ROOT`], published only while find mode is
/// live. It is a SECOND text document in the same tree, which is why it needs its own
/// node: while the panel is up the keyboard belongs to the query field, not the grid, and
/// a tree that says otherwise points a screen reader's caret at the wrong text.
const FIND: NodeId = NodeId(3);
/// The find field's single `Role::TextRun` — the child that turns [`FIND`] into a real
/// text document (`accesskit_consumer::Node::supports_text_ranges` needs an input role
/// PLUS a run), so the caret in the query is navigable rather than a name with no text.
const FIND_RUN: NodeId = NodeId(4);
/// Chrome MESSAGE nodes are `MESSAGE_BASE + `[`ChromeMessage::slot`]. A range of its own,
/// well below [`TAB_BASE`], so [`tab_index_for`] refuses a message id and a screen
/// reader's click on a status band can never be decoded as "switch to tab N".
///
/// The STATUS BANDS live in this range with every other chrome message rather than in one
/// of their own: a band is the same kind of thing as the paste question and the config
/// warning — something the window says to the user unasked — and one range means one
/// announcement contract, one activation route and one slot round-trip to keep honest.
const MESSAGE_BASE: u64 = 1 << 20;
/// Tab items are `TAB_BASE + tab_index`. A high, disjoint range so a tab id can never be
/// mistaken for a Settings control id (`field_index + 1`) or a section group
/// ([`GROUP_BASE`]) by [`crate::app::App::on_accessibility_action`]'s routing.
const TAB_BASE: u64 = 1 << 40;
/// One `Role::TextRun` per visible grid ROW — at `ROW_BASE + row_index * RUNS_PER_ROW`,
/// plus one id per extra run a row wider than [`RUN_CHARS`] is split into. Disjoint from
/// every other id range above, and STABLE across frames: AccessKit's consumer diffs the
/// previous tree against the new one by node id, and it is precisely that diff which
/// synthesises the `object:text-changed` and `object:text-caret-moved` events a screen
/// reader listens for. Reusing row ids per frame is what makes "a line of output
/// arrived" an insert event rather than a remove-and-add of the whole screen.
const ROW_BASE: u64 = 1 << 48;

/// Id space one terminal document may spend on its row runs. A composed split frame
/// publishes one document PER PANE, and each needs row ids that stay stable while a
/// sibling's row count changes — so the pane's slot, not its position in a flat list,
/// picks the block. Slot 0 is the whole-screen document, so a single-pane frame mints
/// exactly the ids it always did.
const PANE_STRIDE: u64 = 1 << 32;

/// Pane container nodes are `PANE_BASE + slot`. Deliberately ABOVE [`ROW_BASE`]: that is
/// the same half-line [`tab_index_for`] already refuses, so a screen reader's click on a
/// pane can never be decoded as "switch to tab N".
const PANE_BASE: u64 = 1 << 62;

/// Characters per text run. AccessKit's word starts are `[u8]` and its consumer
/// addresses them with a `u8` cast of the query offset, so 256 is the widest a
/// run can be and still answer word queries correctly.
const RUN_CHARS: usize = 256;

/// Run-id stride per row: the id space a single row may spend on its runs. A
/// row wider than `RUN_CHARS * RUNS_PER_ROW` characters would collide with the
/// next row's ids, so the chunker stops there and the last run carries the
/// remainder (a 65,536-column terminal is not a case worth id-space for).
const RUNS_PER_ROW: u64 = 256;

/// Split one row into runs no wider than [`RUN_CHARS`] characters, on character
/// boundaries. A row that fits comes back as a single borrowed chunk, so the
/// common case allocates one `String` exactly as it did before.
fn row_chunks(line: &str) -> Vec<String> {
    let count = line.chars().count();
    if count <= RUN_CHARS {
        return vec![line.to_string()];
    }
    let cap = RUN_CHARS * RUNS_PER_ROW as usize;
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for (i, ch) in line.chars().enumerate() {
        if !current.is_empty()
            && i % RUN_CHARS == 0
            && (chunks.len() as u64) < RUNS_PER_ROW - 1
            && i < cap
        {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }
    chunks.push(current);
    chunks
}

/// Section `Role::Group` nodes live in a high, disjoint id range so they never collide with
/// a control id (`field_index + 1`). This is load-bearing: the OS routes an activate/focus
/// action by `target_id - 1 = field_index`
/// (`crate::app::App::on_accessibility_action`), so control ids MUST stay `field_index + 1`
/// and the group parents get ids out of that range.
const GROUP_BASE: u64 = 1 << 32;

/// Build the accessibility tree for the open Settings overlay: a window root whose children
/// are one `Role::Group` per settings section, each group parenting one node per control.
/// Every control is role-mapped from its [`EditKind`] via [`role_of`] — Bool→CheckBox
/// (toggled), a bounded numeric→Slider (min/max/value from [`crate::prefs::range_of`]),
/// an open-ended numeric→SpinButton, Enum/Theme→ComboBox (its option set in the
/// description), free-form→TextInput — and carries a [`describe`]d current-vs-default status.
/// Each control is actionable (Click to toggle/cycle/edit, Focus to select); focus follows
/// `state.selected`. Built entirely from the model — no AppKit, no window.
pub(crate) fn settings_tree(state: &SettingsState) -> TreeUpdate {
    // A pristine config's fields give each control its built-in default effective value, so
    // the description can say "overridden; default …" against the actual baseline.
    let defaults = crate::prefs::editable_fields(&crate::app_config::Config::default());
    let default_of = |key: &str| -> Option<String> {
        defaults
            .iter()
            .find(|d| d.key == key)
            .map(|d| strip_default_marker(SettingsState::display_value(d)))
            .filter(|s| !s.is_empty())
    };

    let mut nodes: Vec<(NodeId, Node)> = Vec::with_capacity(state.fields.len() + 8);

    // One control node per field, in field order, at id `field_index + 1`.
    for (i, f) in state.fields.iter().enumerate() {
        let id = NodeId(i as u64 + 1);
        let displayed = state.displayed_value(i);

        let mut node = Node::new(role_of(f));
        node.set_label(f.label);

        match f.kind {
            EditKind::Bool => {
                let on = displayed.trim().eq_ignore_ascii_case("true");
                node.set_toggled(Toggled::from(on));
            }
            // Numeric: the current value as text AND (when parseable) as a numeric value,
            // plus the bounded range's min/max/step a screen reader announces while scrubbing.
            EditKind::Float | EditKind::Integer => {
                node.set_value(displayed.clone());
                if let Some(v) = numeric_token(&displayed) {
                    node.set_numeric_value(v);
                }
                if let Some(r) = crate::prefs::range_of(f.key) {
                    node.set_min_numeric_value(r.min);
                    node.set_max_numeric_value(r.max);
                    node.set_numeric_value_step(r.step);
                }
            }
            // The displayed value (current option / configured value / live edit buffer).
            _ => node.set_value(displayed.clone()),
        }

        if let Some(desc) = describe(f, default_of(f.key), &state.trail_pack_ids) {
            node.set_description(desc);
        }

        // A ComboBox row reports its popup-menu state: expanded while ITS anchored menu
        // is open (activating it opens the menu), collapsed otherwise. Gated on the
        // SAME `uses_popup` predicate the activation path uses — a short Enum renders
        // segmented and can never open a menu, so reporting it "collapsed" would
        // promise an expansion that cannot happen.
        if crate::settings::uses_popup(f) {
            node.set_expanded(state.menu.as_ref().is_some_and(|m| m.field == i));
        }

        // A screen reader can activate (toggle/cycle/begin-edit) and focus the row.
        node.add_action(Action::Click);
        node.add_action(Action::Focus);
        nodes.push((id, node));
    }

    // Section `Group` parents (only sections that actually have controls), in section order.
    // Each group parents its members' control ids; the root parents the groups.
    let mut group_children: Vec<NodeId> = Vec::new();
    for section in crate::prefs::Section::ORDER {
        let members: Vec<NodeId> = state
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| crate::prefs::section_of(f.key) == section)
            .map(|(i, _)| NodeId(i as u64 + 1))
            .collect();
        if members.is_empty() {
            continue;
        }
        let gid = NodeId(GROUP_BASE + section.order_index() as u64);
        let mut group = Node::new(Role::Group);
        group.set_label(section.label());
        group.set_children(members);
        nodes.push((gid, group));
        group_children.push(gid);
    }

    let mut root = Node::new(Role::Window);
    root.set_label("aterm Settings");
    root.set_children(group_children);
    nodes.push((ROOT, root));

    let focus = if state.fields.is_empty() {
        ROOT
    } else {
        NodeId(state.selected.min(state.fields.len() - 1) as u64 + 1)
    };

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(ROOT)),
        tree_id: TreeId::ROOT,
        focus,
    }
}

/// Map a control's [`EditKind`] to its accessibility [`Role`]: Bool→CheckBox, a bounded
/// numeric (one with a [`crate::prefs::range_of`])→Slider, an open-ended numeric→SpinButton,
/// Enum/Theme→ComboBox, and the free-form kinds (Text/Color)→TextInput.
fn role_of(f: &EditField) -> Role {
    match f.kind {
        EditKind::Bool => Role::CheckBox,
        EditKind::Float | EditKind::Integer => {
            if crate::prefs::range_of(f.key).is_some() {
                Role::Slider
            } else {
                Role::SpinButton
            }
        }
        EditKind::Enum { .. } | EditKind::Theme => Role::ComboBox,
        EditKind::Text | EditKind::Color => Role::TextInput,
    }
}

/// The leading numeric token of a displayed value (`"13 px"` → `13.0`), or `None` when the
/// value is non-numeric — e.g. an unset row showing an `"auto"`-style effective default.
fn numeric_token(displayed: &str) -> Option<f64> {
    displayed
        .split_whitespace()
        .next()
        .unwrap_or(displayed)
        .parse::<f64>()
        .ok()
}

/// Whether a non-bool control carries a user override (a present, non-blank seed). Mirrors
/// `settings::is_overridden` — kept local so the a11y map never reaches into the painter.
/// Bool rows are excluded (their seed is always the resolved value, never `None`).
fn is_overridden(f: &EditField) -> bool {
    if matches!(f.kind, EditKind::Bool) {
        return false;
    }
    f.seed
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty())
}

/// Drop a trailing `"(default)"` annotation from an effective-value string
/// (`"260 (default)"` → `"260"`, `"auto (default)"` → `"auto"`).
fn strip_default_marker(s: &str) -> String {
    let s = s.trim();
    s.strip_suffix("(default)")
        .map(str::trim)
        .unwrap_or(s)
        .to_string()
}

/// A machine-readable unit for a numeric control, surfaced in its a11y description.
fn units_of(key: &str) -> Option<&'static str> {
    match key {
        crate::prefs::EDIT_FONT_PX => Some("px"),
        crate::prefs::EDIT_CURSOR_TRAIL_MS => Some("ms"),
        _ => None,
    }
}

/// The ComboBox option set for an Enum/Theme control (its selectable values), else `None`.
/// Theme options are the built-in colour-scheme registry, resolved dynamically; the
/// `cursor_trail_style` row additionally lists one `pack:<id>` per loaded Trail Pack
/// (`pack_ids`, empty for every other row / a pack-free config — byte-identical there).
fn combo_options(f: &EditField, pack_ids: &[String]) -> Option<Vec<String>> {
    match f.kind {
        EditKind::Enum { .. } if f.key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE => Some(
            crate::prefs::cursor_trail_style_options(pack_ids.iter().map(String::as_str)),
        ),
        EditKind::Enum { options } => Some(options.iter().map(|s| (*s).to_string()).collect()),
        EditKind::Theme => {
            let names = aterm_types::scheme::builtin_names();
            (!names.is_empty()).then(|| names.iter().map(|s| (*s).to_string()).collect())
        }
        _ => None,
    }
}

/// Compose a control's a11y `description`: the ComboBox option set, the numeric unit, and
/// the current-vs-default status (`"overridden; default 13"`) — the context a screen reader
/// or AI driver needs beyond the bare label and value. `None` when there is nothing to add.
fn describe(f: &EditField, default: Option<String>, pack_ids: &[String]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(opts) = combo_options(f, pack_ids) {
        parts.push(format!("options: {}", opts.join(", ")));
    }
    if let Some(u) = units_of(f.key) {
        parts.push(format!("unit: {u}"));
    }
    match f.kind {
        // A Bool's seed is always the resolved value, so "overridden" can't be told apart
        // from the default — just report the default position.
        EditKind::Bool => {
            if let Some(d) = default {
                parts.push(format!("default {d}"));
            }
        }
        _ => {
            if is_overridden(f) {
                match default {
                    Some(d) => parts.push(format!("overridden; default {d}")),
                    None => parts.push("overridden".to_string()),
                }
            } else if let Some(d) = default {
                parts.push(format!("default {d}"));
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// A minimal valid tree (an empty window root) for when the Settings overlay is closed —
/// the OS a11y client always needs a root even if there are no controls to expose.
pub(crate) fn empty_tree() -> TreeUpdate {
    let mut root = Node::new(Role::Window);
    root.set_label("aterm");
    TreeUpdate {
        nodes: vec![(ROOT, root)],
        tree: Some(Tree::new(ROOT)),
        tree_id: TreeId::ROOT,
        focus: ROOT,
    }
}

/// Where the visible grid sits inside the window's client area, in PHYSICAL pixels —
/// the coordinate space `accesskit_winit` publishes root window bounds in, so a node's
/// `bounds` here lands on the same pixels the glyphs did.
///
/// Optional at the call site: without it the grid still exposes a complete AT-SPI `Text`
/// interface (text, caret, line/word/character granularity); only the geometric extras —
/// `Component` extents and `GetCharacterExtents` — are unavailable. Absent geometry is
/// therefore a degradation, never a wrong answer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GridGeometry {
    /// Left edge of column 0, physical px from the client area's left edge.
    pub(crate) origin_x: f64,
    /// Top edge of grid row 0 — BELOW the tab strip, matching the snapshot, which
    /// drops the strip rows.
    pub(crate) origin_y: f64,
    /// One cell's advance width in physical px.
    pub(crate) cell_w: f64,
    /// One cell's line height in physical px.
    pub(crate) cell_h: f64,
    /// Top edge of the in-grid tab strip band, physical px (`origin_y` minus the
    /// strip's rows). Equal to `origin_y` when no strip is on screen.
    pub(crate) strip_y: f64,
    /// Height of the whole strip band in physical px; `0.0` when no strip is drawn.
    pub(crate) strip_h: f64,
    /// Top edge of the STATUS BAR band — directly below the strip, directly above
    /// grid row 0. Equal to `origin_y` when no bar is up.
    pub(crate) bars_y: f64,
    /// Height of one bar row in physical px (each bar is exactly one row).
    pub(crate) bar_h: f64,
}

/// One publishable tab of the in-grid strip: the label the strip painted, whether it is
/// the active tab, and its COLUMN span (`start_col..end_col`) taken straight from the
/// cached `TabSegment` hit geometry the mouse uses — so the announced bounds and the
/// clickable pixels are the same rectangle by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridTab {
    /// The tab's index in the window's tab set — the argument `switch_tab_in` takes.
    pub(crate) index: usize,
    /// The label the strip painted for this tab.
    pub(crate) title: String,
    /// This is the active tab.
    pub(crate) selected: bool,
    /// First column of the tab's segment, inclusive.
    pub(crate) start_col: u16,
    /// One past the tab's last column.
    pub(crate) end_col: u16,
}

/// A PAINTED text selection, already expressed in one document's own row/column space
/// with both ends clamped to the text those rows actually publish.
///
/// The columns come from the renderer's OWN per-row selection predicate
/// (`aterm_core::render::RenderInput::selection_row_key`), which is the list the highlight
/// band is drawn from — one source, not a second copy of the geometry free to drift as the
/// selection rules change.
///
/// It is the same SOURCE, not exact equality at every edge, and the difference is worth
/// naming: that span is content-independent, while `TextSelection::contains_cell` snaps a
/// double-width glyph whole for every selection kind (see `aterm_core::render`'s own note
/// on `selection_row_span_of`), so an edge landing on a wide LEAD paints the continuation
/// one column past `hi`. A screen reader is then told about the glyph and not its pad
/// cell, which is the right answer for text and one cell narrower than the tint.
///
/// `focus` is the END of the span, not the direction the drag went: the per-row predicate
/// is normalised (it answers "which columns of this row are tinted"), and a screen reader
/// reads the selected TEXT, which is direction-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GridSpan {
    /// First selected cell, `(row, column)`.
    pub(crate) anchor: (usize, usize),
    /// One past the last selected cell, `(row, column)`.
    pub(crate) focus: (usize, usize),
}

/// One visible SPLIT PANE as its own terminal document.
///
/// A composed split frame tiles every pane onto the same rows, so the whole-frame
/// projection reads two unrelated programs' output spliced together on every line, with
/// nothing to say which is which or which one the keyboard is in. One document per pane
/// is the shape a screen reader can actually navigate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridPane {
    /// This pane's id slot. Namespaces the pane's row runs ([`PANE_STRIDE`]) so a sibling
    /// gaining or losing rows never renumbers this pane's lines out from under the
    /// consumer's diff.
    pub(crate) slot: usize,
    /// What a screen reader announces on entering the pane.
    pub(crate) label: String,
    /// This pane's rectangle of the composed frame, as text.
    pub(crate) snap: crate::accessibility::AccessibleSnapshot,
    /// The pane's origin in the visible grid, for `bounds` (both `0` for a single pane).
    pub(crate) row_off: usize,
    /// Left column of the pane in the visible grid.
    pub(crate) col_off: usize,
    /// The keyboard is in this pane.
    pub(crate) focused: bool,
    /// This pane's painted selection, in the pane's own coordinates.
    pub(crate) selection: Option<GridSpan>,
}

/// The find panel's query FIELD, as a screen reader needs it.
///
/// The panel is painted as terminal cells over the grid, so its text is already inside
/// the grid document — but as anonymous output, with the caret still parked on the shell
/// prompt. That is the difference between "there is a search box on screen" and "you are
/// typing in it".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridFind {
    /// The live query, exactly as the well paints it.
    pub(crate) query: String,
    /// The edit position as a CHARACTER index into `query` (the painter's caret).
    pub(crate) caret: usize,
    /// The answer the panel shows — `match 2 of 7`, `no matches`, `bad regex` — plus the
    /// active modes, as the field's accessible description.
    pub(crate) status: String,
}

/// A CHROME MESSAGE the window shows the user without being asked — the one class of
/// surface that a screen reader cannot discover by exploring, because by the time the
/// user goes looking it is gone.
///
/// Each variant owns a FIXED [`ChromeMessage::slot`], so its node id is the same from
/// frame to frame while the message stands. That stability is what makes AccessKit's
/// consumer emit "this region's text changed" rather than tearing one node down and
/// building another — and on AT-SPI and UIA the announcement fires on exactly that
/// name change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChromeMessage {
    /// The transient top-centre card ([`crate::notice`]): an update is ready, a gesture
    /// failed, a connection was authorised. It is drawn as a floating raster card, so
    /// unlike every band below, its words never reach the grid text at all.
    Notice,
    /// The multi-line-paste CONFIRMATION band ([`crate::paste_banner`]) — the pastejacking
    /// guard's only prompt on the platforms with no native alert. A question the user is
    /// being asked; the loudest thing this tree ever says.
    PasteConfirm,
    /// The config-load/reload warning band ([`crate::config_notice`]): which rules the
    /// config dropped, and which edits wait for a restart.
    ConfigWarning,
    /// The ALab toolchain status band ([`crate::status_bars::Lane::Toolchain`]).
    ToolchainStatus,
    /// aterm's own self-update status band ([`crate::status_bars::Lane::Update`]).
    UpdateStatus,
}

impl ChromeMessage {
    /// Every message, in the order they are published — the order they are painted down
    /// the window. Exhaustive by construction: a new variant that is not listed here is
    /// never published, and [`ChromeMessage::from_node`] would not round-trip it, which
    /// is what the slot round-trip test checks.
    pub(crate) const ORDER: [Self; 5] = [
        Self::ToolchainStatus,
        Self::UpdateStatus,
        Self::PasteConfirm,
        Self::ConfigWarning,
        Self::Notice,
    ];

    /// This message's permanent id slot.
    const fn slot(self) -> u64 {
        match self {
            Self::Notice => 0,
            Self::PasteConfirm => 1,
            Self::ConfigWarning => 2,
            Self::ToolchainStatus => 3,
            Self::UpdateStatus => 4,
        }
    }

    /// This message's node id in [`grid_tree`].
    fn node_id(self) -> NodeId {
        NodeId(MESSAGE_BASE + self.slot())
    }

    /// Decode a screen reader's action target back to the message it names, or `None`
    /// when the id is not one of [`grid_tree`]'s message nodes. Rejects rather than
    /// clamps: a stale request must be a no-op, never an answer to a different question.
    pub(crate) fn from_node(node: NodeId) -> Option<Self> {
        let slot = node.0.checked_sub(MESSAGE_BASE)?;
        Self::ORDER.into_iter().find(|m| m.slot() == slot)
    }

    /// How loudly this message interrupts. Only the paste confirmation is assertive: it
    /// is a security question that stops the paste until it is answered, and a polite
    /// announcement would queue behind whatever the terminal is printing. Everything
    /// else is news, and news waits its turn.
    const fn politeness(self) -> Live {
        match self {
            Self::PasteConfirm => Live::Assertive,
            Self::Notice | Self::ConfigWarning | Self::ToolchainStatus | Self::UpdateStatus => {
                Live::Polite
            }
        }
    }

    /// The platform role. `Alert` reaches AT-SPI as `Notification` and `AlertDialog` as
    /// `Alert`, which is the distinction between "here is some news" and "you are being
    /// asked something"; a metered band is a real `ProgressIndicator`, and one without a
    /// meter is a `Status` (AT-SPI `StatusBar`).
    const fn role(self, metered: bool) -> Role {
        match self {
            Self::Notice | Self::ConfigWarning => Role::Alert,
            Self::PasteConfirm => Role::AlertDialog,
            Self::ToolchainStatus | Self::UpdateStatus => {
                if metered {
                    Role::ProgressIndicator
                } else {
                    Role::Status
                }
            }
        }
    }
}

/// One chrome message as the frame actually shows it.
///
/// `text` is the SPOKEN sentence and `detail` is everything a reader may go and read but
/// must not be interrupted with — because the announcement fires on the spoken sentence
/// CHANGING, and a download whose percentage lives in the spoken half would say itself a
/// hundred times on the way to 100%. Keeping the volatile figures in the description is
/// what makes a live region informative instead of a flood.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GridMessage {
    /// Which surface this is — and, through it, the node id, the role and the politeness.
    pub(crate) message: ChromeMessage,
    /// The sentence a screen reader speaks. Stable while the surface says the same thing.
    pub(crate) text: String,
    /// Live figures, key hints, the rest of a multi-line banner: read on demand.
    pub(crate) detail: Option<String>,
    /// Determinate progress in `0..=1`, when the band draws a meter.
    pub(crate) progress: Option<f32>,
    /// A screen reader may activate this message — the update pill's one-click apply, a
    /// status band's deliberate settings page. `false` for a card that only informs, so
    /// the tree never offers an action the pixels do not have.
    pub(crate) activates: bool,
    /// Which STATUS BAR row (0 = topmost) this message occupies, for the messages that
    /// have one.
    ///
    /// A status band RESERVES a row of its own between the strip and grid row 0, and
    /// [`GridGeometry`] carries where that band starts and how tall a row is — so its
    /// rectangle is a real place on the glass and a screen reader can route a click, a
    /// magnifier can follow it, and a touch explorer can find it. `None` for every
    /// message that has no rectangle worth publishing: the notice card floats and slides
    /// through its whole life, and the paste/config bands overwrite rows that belong to
    /// the grid document, so a rectangle there would put the announcement on top of text
    /// that is not it.
    pub(crate) bar_row: Option<usize>,
}

/// Everything a frame publishes BESIDES its plain visible grid: the tab strip, the split
/// panes it is composed from, the painted selection, the find field, and the chrome
/// messages the window is currently showing.
///
/// One value rather than one parameter each, so a surface that grows an accessible
/// counterpart later extends this struct instead of the whole call chain. `Default` is
/// the honest description of an ordinary single-pane frame with nothing selected and no
/// panel open.
#[derive(Debug, Clone, Default)]
pub(crate) struct GridFrame<'a> {
    /// The in-grid tab strip, empty when it is not a switcher on screen.
    pub(crate) tabs: &'a [GridTab],
    /// One entry per visible pane, EMPTY for a single-pane (or zoomed) frame — which is
    /// then published exactly as it always was, as one whole-screen document.
    pub(crate) panes: &'a [GridPane],
    /// The whole-screen document's painted selection. Only consulted when `panes` is
    /// empty; a split frame carries its selection per pane.
    pub(crate) selection: Option<GridSpan>,
    /// The find panel, live only while find mode is up.
    pub(crate) find: Option<&'a GridFind>,
    /// The chrome messages on screen right now, empty on an ordinary frame.
    pub(crate) messages: &'a [GridMessage],
    /// The window's own CANONICAL title (`WindowState::current_title`).
    ///
    /// Not always the string on the caption bar: `apply_title`'s title authority
    /// suppresses the chrome write while the close warning or the find status owns it,
    /// and a reader should hear what the window IS rather than whichever transient
    /// message is borrowing its caption.
    ///
    /// This becomes the root's name, which is the name every assistive client reads for
    /// the WINDOW (AT-SPI takes the root `Role::Window` node's name as the frame's; UIA
    /// and NSAccessibility read it the same way). A constant "aterm" leaves a user with
    /// five windows open unable to tell them apart by ear, while the sighted user has had
    /// the running program and the cwd in the titlebar the whole time. Empty falls back
    /// to the app name, so a window whose title has not been composed yet still has one.
    pub(crate) window_title: &'a str,
}

/// The node id for tab `index`, and its inverse.
fn tab_node_id(index: usize) -> NodeId {
    NodeId(TAB_BASE + index as u64)
}

/// Decode a screen reader's action target back to a tab index, or `None` when the id is
/// not one of [`grid_tree`]'s tab items. Rejects ids outside the tab range rather than
/// clamping: a stale request must be a no-op, never a switch to the wrong tab.
pub(crate) fn tab_index_for(node: NodeId) -> Option<usize> {
    node.0
        .checked_sub(TAB_BASE)
        .filter(|offset| *offset < (ROW_BASE - TAB_BASE))
        .map(|offset| offset as usize)
}

/// Word starts within one line, in CHARACTER indices, as AccessKit defines them: index 0
/// always starts a word, and every non-whitespace character preceded by whitespace starts
/// the next one (trailing whitespace stays with the word it follows, a line's leading
/// whitespace is its own word). This is what gives a screen reader working
/// `GetStringAtOffset(.., WORD_START)` on a terminal line.
///
/// The `[u8]` index space is why a run is capped at [`RUN_CHARS`] characters
/// ([`row_chunks`]): the consumer addresses this list by casting the QUERY
/// offset to `u8`, so a run longer than 256 characters answers word queries
/// about the wrong word rather than merely losing precision. Within a run the
/// indices are exact, and the `break` below is a belt-and-braces guard on an
/// invariant the chunker already holds.
fn word_starts_of(line: &str) -> Vec<u8> {
    let mut starts = vec![0u8];
    let mut prev_ws = false;
    for (i, ch) in line.chars().enumerate() {
        if i > 0 && prev_ws && !ch.is_whitespace() {
            let Ok(index) = u8::try_from(i) else { break };
            starts.push(index);
        }
        prev_ws = ch.is_whitespace();
    }
    starts
}

/// The node id of run `chunk` of row `row` in the terminal document occupying id `slot`.
///
/// Slot 0 is the whole-screen document, so a single-pane frame mints exactly the ids it
/// has always minted — which is what keeps AccessKit's consumer diffing "one line
/// changed" rather than tearing down the screen.
fn row_run_id(slot: usize, row: usize, chunk: usize) -> NodeId {
    NodeId(ROW_BASE + slot as u64 * PANE_STRIDE + row as u64 * RUNS_PER_ROW + chunk as u64)
}

/// A [`TextPosition`] naming column `col` of row `row` inside one terminal document.
///
/// The column is clamped to the text the row actually publishes (its trailing blanks are
/// trimmed away), so a position in a run of empty cells lands ON that row's hard line
/// break rather than past the end of the node — AccessKit's own rule, and the same clamp
/// [`crate::accessibility::AccessibleSnapshot::cursor_offset`] applies, so the AT-SPI
/// `CaretOffset` and the macOS `AXSelectedTextRange` cannot diverge.
///
/// A position also names the RUN it sits in, not merely the row: a row wider than
/// [`RUN_CHARS`] is published as several runs ([`row_chunks`]), and a column addressed
/// against the row's FIRST run would be off the end of that node.
fn text_position(slot: usize, lines: &[&str], row: usize, col: usize) -> Option<TextPosition> {
    let line = lines.get(row)?;
    let trimmed = line.strip_suffix('\n').unwrap_or(line).chars().count();
    let clamped = col.min(trimmed);
    let chunk = (clamped / RUN_CHARS).min(RUNS_PER_ROW as usize - 1);
    Some(TextPosition {
        node: row_run_id(slot, row, chunk),
        character_index: clamped - chunk * RUN_CHARS,
    })
}

/// Emit ONE terminal document — a read-only `Role::Terminal` node plus one
/// `Role::TextRun` child per visible row — and answer its node id.
///
/// The run children are what make the platform publish an AT-SPI `Text` interface
/// (`accesskit_consumer::Node::supports_text_ranges` requires `Role::Terminal` PLUS at
/// least one text run), which is the difference between a screen reader announcing the
/// bare name "aterm terminal" and being able to read, review and navigate the screen by
/// line, word and character.
///
/// `selection` outranks the cursor when both exist, because that is what a text control
/// reports and what an assistive client announces: a live highlight IS the selection, and
/// its far end IS the caret. With nothing selected the caret is published alone as a
/// degenerate selection; with the cursor hidden (DECTCEM) and nothing selected, NO
/// selection is published rather than a bogus one — AT-SPI then reports caret offset
/// `-1`, which is the honest answer.
///
/// `origin` is the document's top-left cell in the visible grid, so a split pane's
/// announced rectangle is the rectangle it was painted into.
#[allow(
    clippy::too_many_arguments,
    reason = "one document's whole description"
)]
fn push_terminal_document(
    nodes: &mut Vec<(NodeId, Node)>,
    id: NodeId,
    slot: usize,
    label: String,
    snap: &crate::accessibility::AccessibleSnapshot,
    origin: (usize, usize),
    geom: Option<GridGeometry>,
    selection: Option<GridSpan>,
) {
    // One text run per visible row. `split_inclusive` keeps each row's terminating '\n'
    // inside its run, which is exactly how AccessKit represents a hard line break: it is
    // one character of the run, and a caret at end-of-line sits ON it, not past it.
    let lines: Vec<&str> = snap.value().split_inclusive('\n').collect();
    let mut row_ids: Vec<NodeId> = Vec::with_capacity(lines.len());
    let (row_off, col_off) = origin;

    for (row, line) in lines.iter().enumerate() {
        // ONE RUN PER ROW, UNLESS THE ROW OUTRUNS u8 ADDRESSING. AccessKit
        // carries word starts as `[u8]` AND the consumer addresses them by
        // casting the QUERY offset to u8 (accesskit_consumer's
        // `word_starts.binary_search(&(pos.character_index as u8))`), so on a
        // row longer than 256 characters a query at column 281 wraps to 25 and
        // answers about a different word entirely — measured on a 300-column
        // instance. Capping the LIST cannot stop the query from wrapping; only
        // keeping every run inside the addressable range can. So a wide row is
        // split into consecutive runs, each its own text run with its own
        // word starts, which is exactly what runs are for.
        for (chunk_index, chunk) in row_chunks(line).into_iter().enumerate() {
            let line = &chunk;
            let id = row_run_id(slot, row, chunk_index);
            row_ids.push(id);
            let mut run = Node::new(Role::TextRun);
            run.set_value((*line).to_string());
            run.set_character_lengths(
                line.chars()
                    .map(|c| u8::try_from(c.len_utf8()).unwrap_or(4))
                    .collect::<Vec<u8>>(),
            );
            run.set_word_starts(word_starts_of(line.strip_suffix('\n').unwrap_or(line)));
            if let Some(g) = geom {
                // One character per cell (`push_visible_row` emits exactly one char per
                // rendered cell, wide-glyph continuations included), so the character index
                // IS the column. The hard line break is zero-width at the end of the row.
                let mut positions: Vec<f32> = Vec::with_capacity(line.len());
                let mut widths: Vec<f32> = Vec::with_capacity(line.len());
                let col0 = chunk_index * RUN_CHARS;
                for (i, ch) in line.chars().enumerate() {
                    positions.push(((col0 + i) as f64 * g.cell_w) as f32);
                    widths.push(if ch == '\n' { 0.0 } else { g.cell_w as f32 });
                }
                run.set_character_positions(positions);
                run.set_character_widths(widths);
                let x0 = g.origin_x + col_off as f64 * g.cell_w;
                let y0 = g.origin_y + (row_off + row) as f64 * g.cell_h;
                run.set_bounds(Rect {
                    x0,
                    y0,
                    x1: x0 + snap.cols as f64 * g.cell_w,
                    y1: y0 + g.cell_h,
                });
            }
            nodes.push((id, run));
        }
    }

    let mut grid = Node::new(Role::Terminal);
    grid.set_label(label);
    // Retained for the platforms whose "value" is a plain string (UIA, NSAccessibility);
    // AT-SPI's Value interface is numeric and correctly ignores it, reading the text runs
    // above through the Text interface instead.
    grid.set_value(snap.value().to_string());
    grid.set_read_only();
    grid.set_text_direction(TextDirection::LeftToRight);
    grid.set_children(row_ids);
    if let Some(g) = geom {
        let x0 = g.origin_x + col_off as f64 * g.cell_w;
        let y0 = g.origin_y + row_off as f64 * g.cell_h;
        grid.set_bounds(Rect {
            x0,
            y0,
            x1: x0 + snap.cols as f64 * g.cell_w,
            y1: y0 + lines.len() as f64 * g.cell_h,
        });
    }
    let published =
        match selection {
            Some(span) => text_position(slot, &lines, span.anchor.0, span.anchor.1)
                .zip(text_position(slot, &lines, span.focus.0, span.focus.1)),
            None => snap
                .cursor
                .and_then(|(row, col)| text_position(slot, &lines, row, col))
                .map(|caret| (caret, caret)),
        };
    if let Some((anchor, focus)) = published {
        grid.set_text_selection(TextSelection { anchor, focus });
    }
    nodes.push((id, grid));
}

/// Emit the find panel's query field: a `Role::SearchInput` carrying the live query, its
/// caret, and the panel's own answer as the accessible description.
///
/// The panel is painted as terminal cells, so its text is ALREADY inside the grid
/// document — but only as anonymous output. Without this node a screen reader user who
/// presses the find chord hears nothing change, keeps hearing the shell prompt's caret,
/// and types into a field the tree never mentions.
fn push_find_field(nodes: &mut Vec<(NodeId, Node)>, find: &GridFind) {
    let mut run = Node::new(Role::TextRun);
    run.set_value(find.query.clone());
    run.set_character_lengths(
        find.query
            .chars()
            .map(|c| u8::try_from(c.len_utf8()).unwrap_or(4))
            .collect::<Vec<u8>>(),
    );
    run.set_word_starts(word_starts_of(&find.query));
    nodes.push((FIND_RUN, run));

    let mut field = Node::new(Role::SearchInput);
    field.set_label("Find");
    field.set_value(find.query.clone());
    if !find.status.is_empty() {
        field.set_description(find.status.clone());
    }
    field.set_children(vec![FIND_RUN]);
    // The caret in the QUERY, capped at its length: the field is one line, so it is one
    // run and the character index is the caret index.
    let caret = TextPosition {
        node: FIND_RUN,
        character_index: find.caret.min(find.query.chars().count()),
    };
    field.set_text_selection(TextSelection {
        anchor: caret,
        focus: caret,
    });
    field.add_action(Action::Focus);
    nodes.push((FIND, field));
}

/// Emit one chrome message as a LIVE REGION and answer its node id.
///
/// THE ANNOUNCED STRING GOES IN BOTH THE NAME AND THE VALUE, because the three platforms
/// read different halves and a node carrying one of them is audible on some and silent on
/// the rest: `accesskit_atspi_common` announces the NAME (and only when the name changes),
/// `accesskit_windows` raises `LiveRegionChanged` on the same name change and the reader
/// then reads the name, while `accesskit_macos` announces the VALUE and raises nothing at
/// all for a node that has none. `crate::native_accessibility::announce_live` states the
/// same rule for the native tab apps' live regions.
///
/// `bounds` only for a message that HAS a rectangle — see [`GridMessage::bar_row`]. A
/// floating card and a band spliced over grid rows are things the user is TOLD, not places
/// on the glass, and a wrong rectangle would put the announcement somewhere the words are
/// not; a status band reserves its own row and the geometry knows exactly where it is.
///
/// The message does NOT take focus, not even when it is assertive. Focus is the tree's
/// statement of where the keyboard is, and the keyboard is still in the terminal — the
/// paste confirmation is answered by Enter/Escape reaching the very grid this would have
/// stolen focus from.
fn push_message(
    nodes: &mut Vec<(NodeId, Node)>,
    message: &GridMessage,
    geom: Option<GridGeometry>,
    cols: usize,
) -> NodeId {
    let id = message.message.node_id();
    let metered = message.progress.is_some();
    let mut node = Node::new(message.message.role(metered));
    node.set_label(message.text.clone());
    node.set_value(message.text.clone());
    node.set_live(message.message.politeness());
    if let Some(detail) = &message.detail {
        node.set_description(detail.clone());
    }
    if let Some(row) = message.bar_row
        && let Some(g) = geom
        && g.bar_h > 0.0
    {
        let y0 = g.bars_y + row as f64 * g.bar_h;
        node.set_bounds(Rect {
            x0: g.origin_x,
            y0,
            x1: g.origin_x + cols as f64 * g.cell_w,
            y1: y0 + g.bar_h,
        });
    }
    // A determinate meter as the platform's own numeric value, so AT-SPI publishes a
    // `Value` interface on the progress bar and a reader can ask "how far along" instead
    // of waiting for the next sentence. An absent fill leaves the node numerically
    // unset, which is exactly how AccessKit spells an indeterminate progress bar.
    if let Some(fill) = message.progress {
        node.set_min_numeric_value(0.0);
        node.set_max_numeric_value(1.0);
        node.set_numeric_value(f64::from(fill.clamp(0.0, 1.0)));
    }
    if message.activates {
        node.add_action(Action::Click);
    }
    nodes.push((id, node));
    id
}

/// DEFAULT-2: the accessibility tree for a plain terminal session (no overlay open).
///
/// A `Role::Window` root parenting, in the order they are painted: the visible tab strip
/// (when it is on screen and is a switcher), the chrome MESSAGES the window is currently
/// showing ([`ChromeMessage`] — live regions, so they are spoken as they arrive rather
/// than waiting to be found), the find panel's query field (while find mode is live), and
/// the terminal itself — ONE read-only `Role::Terminal` document for an ordinary frame,
/// or one PER VISIBLE PANE for a composed split, since a split frame's rows carry every
/// pane side by side and read whole they are two unrelated programs' output spliced
/// together.
///
/// FOCUS is the tree's statement of where the keyboard is, and every assistive client
/// reads it: the find field while the panel owns the keys, else the focused pane, else
/// the single grid.
///
/// The events follow from the structure, not from a second code path: AccessKit's consumer
/// diffs this tree against the previously published one and synthesises
/// `object:text-changed:insert` / `:delete` from the row text that actually changed and
/// `object:text-caret-moved` from the selection focus that actually moved. That is why the
/// row node ids ([`ROW_BASE`]) must stay stable across frames.
///
/// The grid text is the SAME snapshot the SIGUSR1 `.txt` capture and the AppKit publisher
/// use, so the three never diverge.
pub(crate) fn grid_tree(
    snap: &crate::accessibility::AccessibleSnapshot,
    geom: Option<GridGeometry>,
    frame: &GridFrame<'_>,
) -> TreeUpdate {
    let rows = snap.value().split_inclusive('\n').count();
    let mut nodes: Vec<(NodeId, Node)> =
        Vec::with_capacity(rows + frame.tabs.len() + frame.panes.len() + frame.messages.len() + 5);
    let mut root_children: Vec<NodeId> =
        Vec::with_capacity(3 + frame.panes.len() + frame.messages.len());

    // The tab strip, when one is on screen and switching between tabs is a thing the user
    // can actually do. Each tab is focusable and clickable; the OS routes both back
    // through `tab_index_for`.
    if !frame.tabs.is_empty() {
        let mut items: Vec<NodeId> = Vec::with_capacity(frame.tabs.len());
        for tab in frame.tabs {
            let id = tab_node_id(tab.index);
            let mut node = Node::new(Role::Tab);
            node.set_label(tab.title.clone());
            node.set_selected(tab.selected);
            node.add_action(Action::Click);
            node.add_action(Action::Focus);
            if let Some(g) = geom
                && tab.end_col > tab.start_col
            {
                node.set_bounds(Rect {
                    x0: g.origin_x + f64::from(tab.start_col) * g.cell_w,
                    y0: g.strip_y,
                    x1: g.origin_x + f64::from(tab.end_col) * g.cell_w,
                    y1: g.strip_y + g.strip_h,
                });
            }
            items.push(id);
            nodes.push((id, node));
        }
        let mut list = Node::new(Role::TabList);
        list.set_label("Tabs");
        list.set_children(items);
        if let Some(g) = geom
            && g.strip_h > 0.0
        {
            list.set_bounds(Rect {
                x0: g.origin_x,
                y0: g.strip_y,
                x1: g.origin_x + snap.cols as f64 * g.cell_w,
                y1: g.strip_y + g.strip_h,
            });
        }
        nodes.push((TAB_LIST, list));
        root_children.push(TAB_LIST);
    }

    // The chrome messages, in painted order, between the strip and the grid — which is
    // where the status bands physically are, and a defensible place to meet the two
    // surfaces that float above or overwrite the grid's top rows. They come BEFORE the
    // terminal so a reader stepping into the window meets the interruption first.
    for message in ChromeMessage::ORDER
        .iter()
        .filter_map(|kind| frame.messages.iter().find(|m| m.message == *kind))
    {
        root_children.push(push_message(&mut nodes, message, geom, snap.cols));
    }

    if let Some(find) = frame.find {
        push_find_field(&mut nodes, find);
        root_children.push(FIND);
    }

    // One document per visible pane, or the whole composed screen as one when the frame
    // is not split. A pane's slot (never its position in this list) picks its row-id
    // block, so a sibling closing does not renumber a surviving pane's lines.
    let mut focused_pane: Option<NodeId> = None;
    if frame.panes.is_empty() {
        push_terminal_document(
            &mut nodes,
            GRID,
            0,
            crate::accessibility::LABEL.to_string(),
            snap,
            (0, 0),
            geom,
            frame.selection,
        );
        root_children.push(GRID);
    } else {
        for pane in frame.panes {
            let id = NodeId(PANE_BASE + pane.slot as u64);
            push_terminal_document(
                &mut nodes,
                id,
                pane.slot + 1,
                pane.label.clone(),
                &pane.snap,
                (pane.row_off, pane.col_off),
                geom,
                pane.selection,
            );
            if pane.focused {
                focused_pane = Some(id);
            }
            root_children.push(id);
        }
    }

    // Focus follows the KEYBOARD, which is what every assistive client reads it as: the
    // find field while the panel owns the keys, else the pane the keyboard is in. The
    // last child is the fallback — the single grid on an ordinary frame, and the last pane
    // on a split whose focus the layout could not name, which is a real node either way.
    let focus = match (frame.find.is_some(), focused_pane) {
        (true, _) => FIND,
        (false, Some(pane)) => pane,
        (false, None) => root_children.last().copied().unwrap_or(GRID),
    };

    let mut root = Node::new(Role::Window);
    // The window's own title (see `GridFrame::window_title`) — what a screen reader
    // names this window when the user moves between windows.
    root.set_label(if frame.window_title.is_empty() {
        "aterm"
    } else {
        frame.window_title
    });
    root.set_children(root_children);
    nodes.push((ROOT, root));
    TreeUpdate {
        nodes,
        tree: Some(Tree::new(ROOT)),
        tree_id: TreeId::ROOT,
        focus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::Config;

    fn state() -> SettingsState {
        SettingsState::from_config(&Config::default())
    }

    /// A real engine's visible grid, so every assertion below runs against the genuine
    /// `AccessibleSnapshot::value()` (the anti-divergence anchor with the SIGUSR1 `.txt`
    /// snapshot), not a hand-written string.
    fn live_grid(
        rows: u16,
        cols: u16,
        bytes: &[u8],
        cursor: Option<(usize, usize)>,
    ) -> crate::accessibility::AccessibleSnapshot {
        let mut term = aterm_core::terminal::Terminal::new(rows, cols);
        term.process(bytes);
        let cells: Vec<_> = (0..usize::from(rows)).map(|r| term.render_row(r)).collect();
        crate::accessibility::AccessibleSnapshot::from_cells(&cells, usize::from(cols), cursor)
    }

    fn node(update: &TreeUpdate, id: NodeId) -> &Node {
        &update.nodes.iter().find(|(n, _)| *n == id).unwrap().1
    }

    const GEOM: GridGeometry = GridGeometry {
        origin_x: 8.0,
        origin_y: 30.0,
        cell_w: 9.0,
        cell_h: 18.0,
        strip_y: 12.0,
        strip_h: 18.0,
        bars_y: 30.0,
        bar_h: 18.0,
    };

    /// THE STATUS BARS ARE NOT MUTE, AND THEY ARE SOMEWHERE. They reserve real terminal
    /// rows and are the only announcement of work the user did not start (a toolchain
    /// installing, an update downloading), so a tree that omitted them both described a
    /// window taller than the one on glass and said nothing about the work. Published in
    /// painted order between the strip and the grid, each carrying the band rectangle it
    /// was drawn into — which is what lets a magnifier follow it and a click reach it —
    /// while the surfaces with no fixed place on the glass carry none.
    #[test]
    fn a_status_band_is_published_where_it_is_painted() {
        let snap = live_grid(2, 20, b"hi", Some((0, 2)));
        let bars = vec![
            GridMessage {
                bar_row: Some(0),
                ..message(
                    ChromeMessage::ToolchainStatus,
                    "Installing the ALab toolchain \u{00b7} extracting",
                )
            },
            GridMessage {
                bar_row: Some(1),
                ..message(
                    ChromeMessage::UpdateStatus,
                    "aterm update v0.62.0 \u{00b7} downloading\u{2026}",
                )
            },
        ];
        fn frame(messages: &[GridMessage]) -> GridFrame<'_> {
            GridFrame {
                messages,
                ..GridFrame::default()
            }
        }
        let update = grid_tree(&snap, Some(GEOM), &frame(&bars));

        for (index, band) in bars.iter().enumerate() {
            let published = node(&update, band.message.node_id());
            assert_eq!(published.label(), Some(band.text.as_str()));
            let bounds = published
                .bounds()
                .expect("a published band carries the row it reserved");
            // One row each, stacked below the strip and above grid row 0.
            assert_eq!(bounds.y0, GEOM.bars_y + index as f64 * GEOM.bar_h);
            assert_eq!(bounds.y1, bounds.y0 + GEOM.bar_h);
            assert_eq!(bounds.x0, GEOM.origin_x);
            assert_eq!(bounds.x1, GEOM.origin_x + 20.0 * GEOM.cell_w);
        }
        // A surface with no reserved row publishes NO rectangle rather than a wrong one.
        let floating = grid_tree(
            &snap,
            Some(GEOM),
            &frame(&[message(ChromeMessage::Notice, "\u{2191} Update ready")]),
        );
        assert_eq!(
            node(&floating, ChromeMessage::Notice.node_id()).bounds(),
            None
        );

        // Painted order, and the grid still comes last so a reader meets the
        // chrome before the content it sits above.
        let children = node(&update, ROOT).children();
        let bar_ids: Vec<NodeId> = bars.iter().map(|b| b.message.node_id()).collect();
        assert_eq!(&children[..bars.len()], &bar_ids[..]);
        assert_eq!(children.last().copied(), Some(GRID));

        // …and with no bar up the tree is byte-identical to the no-bar path.
        let without = grid_tree(&snap, Some(GEOM), &GridFrame::default());
        assert_eq!(
            without.nodes.len() + bars.len(),
            update.nodes.len(),
            "a hidden bar publishes no node at all"
        );
    }

    /// DEFAULT-2: `grid_tree` publishes the terminal grid as a read-only `Role::Terminal`
    /// node under a `Role::Window` root, carrying the snapshot's visible text — what a
    /// screen reader reads on a plain session.
    #[test]
    fn grid_tree_publishes_the_terminal_grid() {
        let snap = live_grid(2, 10, b"hello", Some((0, 5)));
        let update = grid_tree(&snap, None, &GridFrame::default());
        assert_eq!(update.focus, GRID);
        let root = node(&update, ROOT);
        let grid = node(&update, GRID);
        assert_eq!(root.role(), Role::Window);
        assert_eq!(root.children(), [GRID], "no strip published without tabs");
        assert_eq!(grid.role(), Role::Terminal);
        assert_eq!(
            grid.value(),
            Some(snap.value()),
            "the node carries the live grid text"
        );
        assert_eq!(grid.label(), Some(crate::accessibility::LABEL));
        assert!(grid.is_read_only());
        assert!(
            snap.value().starts_with("hello"),
            "sanity: real text, got {:?}",
            snap.value()
        );
    }

    /// THE POINT OF THIS MODULE'S TERMINAL BRANCH: the grid is a real text document, not
    /// an opaque node with a name. `accesskit_consumer::Node::supports_text_ranges()` —
    /// the exact predicate `accesskit_atspi_common` consults before it puts
    /// `org.a11y.atspi.Text` on the node — requires `Role::Terminal` PLUS at least one
    /// `Role::TextRun` child, so the row runs below are what turn "aterm terminal" into a
    /// screen a blind user can read. One run per visible row, each carrying its own hard
    /// line break as one character, and the runs concatenating BYTE-FOR-BYTE back to the
    /// snapshot text.
    #[test]
    fn terminal_node_publishes_one_text_run_per_row() {
        let snap = live_grid(3, 12, b"alpha\r\nbeta", Some((1, 4)));
        let update = grid_tree(&snap, Some(GEOM), &GridFrame::default());
        let grid = node(&update, GRID);
        let rows: Vec<NodeId> = (0..3)
            .map(|r| NodeId(ROW_BASE + r * RUNS_PER_ROW))
            .collect();
        assert_eq!(grid.children(), rows, "one text run per visible row");

        let mut rebuilt = String::new();
        for (r, id) in rows.iter().enumerate() {
            let run = node(&update, *id);
            assert_eq!(run.role(), Role::TextRun, "row {r} is a text run");
            let value = run.value().unwrap();
            assert!(
                value.ends_with('\n'),
                "row {r} carries its line break: {value:?}"
            );
            assert_eq!(
                run.character_lengths().len(),
                value.chars().count(),
                "row {r}: one character length per character, line break included"
            );
            assert_eq!(
                run.character_lengths()
                    .iter()
                    .map(|n| *n as usize)
                    .sum::<usize>(),
                value.len(),
                "row {r}: character lengths cover every UTF-8 byte"
            );
            rebuilt.push_str(value);
        }
        assert_eq!(
            rebuilt,
            snap.value(),
            "the runs ARE the snapshot text — the a11y document and the SIGUSR1 capture \
             cannot drift"
        );
        assert!(rebuilt.starts_with("alpha\nbeta\n"), "sanity: {rebuilt:?}");
    }

    /// The caret is a degenerate selection on the cursor's row run, at the cursor's
    /// column, and it lands on exactly the offset `AccessibleSnapshot::cursor_offset`
    /// reports — the number AT-SPI serves as `CaretOffset` and macOS as
    /// `AXSelectedTextRange`. Both are derived here from the same clamped column, so the
    /// two platforms cannot disagree about where the cursor is.
    #[test]
    fn caret_is_a_degenerate_selection_at_the_cursor() {
        let snap = live_grid(3, 12, b"alpha\r\nbeta", Some((1, 4)));
        let update = grid_tree(&snap, Some(GEOM), &GridFrame::default());
        let selection = node(&update, GRID).text_selection().copied().unwrap();
        assert_eq!(
            selection.anchor, selection.focus,
            "a caret, not a selection"
        );
        assert_eq!(
            selection.focus.node,
            NodeId(ROW_BASE + RUNS_PER_ROW),
            "on row 1's run"
        );
        assert_eq!(selection.focus.character_index, 4);
        // The global offset this resolves to: "alpha\n" is 6 characters, + column 4.
        assert_eq!(snap.cursor_offset(), Some(10));
    }

    /// A caret parked in a row's trailing blanks clamps ONTO that row's hard line break
    /// (AccessKit's rule: "when the caret is at the end of such a line, the focus should
    /// be on the line break, not after it"), matching `cursor_offset`'s own clamp.
    #[test]
    fn caret_in_trailing_blanks_clamps_onto_the_line_break() {
        let snap = live_grid(2, 20, b"hi", Some((0, 17)));
        let update = grid_tree(&snap, None, &GridFrame::default());
        let selection = node(&update, GRID).text_selection().copied().unwrap();
        assert_eq!(selection.focus.node, NodeId(ROW_BASE));
        assert_eq!(
            selection.focus.character_index, 2,
            "clamped to the trimmed row length — the index of the '\\n'"
        );
        assert_eq!(
            snap.cursor_offset(),
            Some(2),
            "same offset as the AppKit publisher"
        );
    }

    /// A hidden cursor (DECTCEM) publishes NO selection rather than a bogus one; AT-SPI
    /// then reports caret offset -1, which is the honest answer.
    #[test]
    fn hidden_cursor_publishes_no_caret() {
        let snap = live_grid(2, 10, b"hello", None);
        let update = grid_tree(&snap, None, &GridFrame::default());
        assert_eq!(node(&update, GRID).text_selection(), None);
        assert_eq!(snap.cursor_offset(), None);
    }

    /// Row node ids are STABLE across frames. This is what makes AccessKit's consumer diff
    /// report "one line changed" (an `object:text-changed` insert/delete pair) instead of
    /// tearing the whole screen down and rebuilding it, and it is the reason a screen
    /// reader hears the new output line rather than the entire visible buffer.
    /// A row wider than the `[u8]` word-index space is published as SEVERAL
    /// runs. Measured before this: accesskit's consumer addresses word starts
    /// by casting the query offset to `u8`, so on a 300-column row a query at
    /// column 281 wrapped to 25 and answered about a different word. Capping
    /// the list could not stop the query from wrapping; keeping every run
    /// inside the addressable range can.
    #[test]
    fn a_row_wider_than_the_index_space_is_split_into_addressable_runs() {
        let cols: u16 = 300;
        let text: Vec<u8> = std::iter::repeat_n(b'w', 290).collect();
        let snap = live_grid(2, cols, &text, Some((0, 281)));
        let update = grid_tree(&snap, None, &GridFrame::default());
        let grid = node(&update, GRID);

        // Row 0 spends more than one run; every run stays addressable.
        let runs: Vec<NodeId> = grid.children().to_vec();
        let row0: Vec<&NodeId> = runs
            .iter()
            .filter(|id| id.0 >= ROW_BASE && id.0 < ROW_BASE + RUNS_PER_ROW)
            .collect();
        assert!(
            row0.len() >= 2,
            "a {cols}-column row must not ride one run: {row0:?}"
        );
        for id in &runs {
            let run = node(&update, *id);
            let chars = run.value().map_or(0, |v| v.chars().count());
            assert!(
                chars <= RUN_CHARS,
                "run {id:?} is {chars} characters — past the u8 query space"
            );
        }

        // The caret at column 281 names the run it actually sits in, with an
        // index inside that run rather than an offset off the end of run 0.
        let sel = grid.text_selection().copied().expect("a caret");
        assert_eq!(
            sel.focus.node,
            NodeId(ROW_BASE + 1),
            "the SECOND run of row 0"
        );
        assert_eq!(sel.focus.character_index, 281 - RUN_CHARS);
    }

    #[test]
    fn row_ids_are_stable_across_frames() {
        let first = grid_tree(
            &live_grid(3, 12, b"one", Some((0, 3))),
            None,
            &GridFrame::default(),
        );
        let second = grid_tree(
            &live_grid(3, 12, b"one\r\ntwo", Some((1, 3))),
            None,
            &GridFrame::default(),
        );
        let ids = |u: &TreeUpdate| node(u, GRID).children().to_vec();
        assert_eq!(ids(&first), ids(&second), "same row ids frame to frame");
        assert_ne!(
            node(&second, NodeId(ROW_BASE + RUNS_PER_ROW)).value(),
            node(&first, NodeId(ROW_BASE + RUNS_PER_ROW)).value(),
            "…while the changed row's text genuinely differs"
        );
    }

    /// Per-character geometry: one cell per character, so the character index IS the
    /// column, and the hard line break is zero-width at the end of the row. This is what
    /// `GetCharacterExtents` and braille cursor routing read.
    #[test]
    fn character_geometry_tracks_the_cell_grid() {
        let snap = live_grid(2, 10, b"abc", Some((0, 3)));
        let update = grid_tree(&snap, Some(GEOM), &GridFrame::default());
        let run = node(&update, NodeId(ROW_BASE));
        assert_eq!(run.value(), Some("abc\n"));
        assert_eq!(run.character_positions().unwrap(), [0.0, 9.0, 18.0, 27.0]);
        assert_eq!(
            run.character_widths().unwrap(),
            [9.0, 9.0, 9.0, 0.0],
            "the line break occupies no cell"
        );
        assert_eq!(
            run.bounds().unwrap(),
            Rect {
                x0: 8.0,
                y0: 30.0,
                x1: 8.0 + 10.0 * 9.0,
                y1: 48.0
            },
            "row 0 spans the full grid width, one cell tall"
        );
        assert_eq!(
            node(&update, GRID).bounds().unwrap(),
            Rect {
                x0: 8.0,
                y0: 30.0,
                x1: 98.0,
                y1: 66.0
            },
            "the terminal node covers every visible row"
        );
    }

    /// Without geometry the Text interface is untouched — only the geometric extras go
    /// missing. A degraded answer, never a wrong one.
    #[test]
    fn missing_geometry_degrades_only_the_geometry() {
        let snap = live_grid(2, 10, b"abc", Some((0, 3)));
        let update = grid_tree(&snap, None, &GridFrame::default());
        let run = node(&update, NodeId(ROW_BASE));
        assert_eq!(run.value(), Some("abc\n"));
        assert_eq!(run.character_lengths().len(), 4);
        assert_eq!(run.character_positions(), None);
        assert_eq!(run.bounds(), None);
        assert_eq!(node(&update, GRID).bounds(), None);
        assert!(
            node(&update, GRID).text_selection().is_some(),
            "caret still published"
        );
    }

    /// Word starts follow AccessKit's definition, which is what gives a screen reader a
    /// working "read next word" on a terminal line: index 0 always starts a word, leading
    /// whitespace is its own word, and trailing whitespace belongs to the word it follows.
    #[test]
    fn word_starts_follow_the_accesskit_definition() {
        assert_eq!(word_starts_of("hello world"), [0, 6]);
        assert_eq!(word_starts_of("  ab"), [0, 2]);
        assert_eq!(word_starts_of(""), [0]);
        assert_eq!(word_starts_of("   "), [0]);
        assert_eq!(word_starts_of("$ echo hi"), [0, 2, 7]);
        let update = grid_tree(
            &live_grid(1, 20, b"echo hi", Some((0, 7))),
            None,
            &GridFrame::default(),
        );
        assert_eq!(node(&update, NodeId(ROW_BASE)).word_starts(), [0, 5]);
    }

    /// The tab strip publishes as a `Role::TabList` of focusable, clickable `Role::Tab`
    /// items carrying the strip's own labels and selection, with bounds taken from the
    /// SAME segment spans the mouse hit-tests — and each item's id decodes back to the
    /// index `switch_tab_in` takes.
    #[test]
    fn tab_strip_publishes_as_a_tab_list() {
        let snap = live_grid(2, 40, b"hi", Some((0, 2)));
        let tabs = vec![
            GridTab {
                index: 0,
                title: "build".into(),
                selected: false,
                start_col: 0,
                end_col: 10,
            },
            GridTab {
                index: 1,
                title: "logs".into(),
                selected: true,
                start_col: 10,
                end_col: 20,
            },
        ];
        let update = grid_tree(
            &snap,
            Some(GEOM),
            &GridFrame {
                tabs: &tabs,
                ..GridFrame::default()
            },
        );
        assert_eq!(
            node(&update, ROOT).children(),
            [TAB_LIST, GRID],
            "the strip is announced before the grid, as it is painted"
        );
        let list = node(&update, TAB_LIST);
        assert_eq!(list.role(), Role::TabList);
        assert_eq!(list.children(), [tab_node_id(0), tab_node_id(1)]);
        let logs = node(&update, tab_node_id(1));
        assert_eq!(logs.role(), Role::Tab);
        assert_eq!(logs.label(), Some("logs"));
        assert_eq!(logs.is_selected(), Some(true));
        assert!(logs.supports_action(Action::Click));
        assert!(logs.supports_action(Action::Focus));
        assert_eq!(node(&update, tab_node_id(0)).is_selected(), Some(false));
        assert_eq!(
            logs.bounds().unwrap(),
            Rect {
                x0: 8.0 + 10.0 * 9.0,
                y0: 12.0,
                x1: 8.0 + 20.0 * 9.0,
                y1: 30.0
            },
            "the announced rectangle is the clickable segment"
        );
        assert_eq!(tab_index_for(tab_node_id(1)), Some(1));
        assert_eq!(tab_index_for(GRID), None, "the grid is not a tab");
        assert_eq!(tab_index_for(ROOT), None);
        assert_eq!(
            tab_index_for(NodeId(ROW_BASE)),
            None,
            "a row run is not a tab"
        );
        assert_eq!(
            tab_index_for(NodeId(GROUP_BASE)),
            None,
            "a settings group is not a tab"
        );
    }

    /// A PAINTED selection is published as the terminal's text selection, and it outranks
    /// the caret — which is what an assistive client announces and what every other text
    /// control reports. Before this the tree carried the caret alone, so a screen-reader
    /// user who had just selected a line was told nothing about it (AT-SPI answered
    /// `getNSelections() == 0` over a live highlight).
    #[test]
    fn a_painted_selection_is_published_and_outranks_the_caret() {
        let snap = live_grid(3, 20, b"alpha\r\nbeta gamma", Some((2, 0)));
        let selected = grid_tree(
            &snap,
            None,
            &GridFrame {
                selection: Some(GridSpan {
                    anchor: (1, 5),
                    focus: (1, 10),
                }),
                ..GridFrame::default()
            },
        );
        let sel = node(&selected, GRID).text_selection().copied().unwrap();
        assert_eq!(sel.anchor.node, NodeId(ROW_BASE + RUNS_PER_ROW), "row 1");
        assert_eq!(sel.anchor.character_index, 5);
        assert_eq!(sel.focus.node, NodeId(ROW_BASE + RUNS_PER_ROW));
        assert_eq!(sel.focus.character_index, 10);
        assert_ne!(
            sel.anchor, sel.focus,
            "a real range, not the degenerate caret"
        );

        // Nothing selected ⇒ the caret is published exactly as before.
        let caret_only = grid_tree(&snap, None, &GridFrame::default());
        let caret = node(&caret_only, GRID).text_selection().copied().unwrap();
        assert_eq!(caret.anchor, caret.focus, "the caret alone");
        assert_eq!(caret.focus.node, NodeId(ROW_BASE + 2 * RUNS_PER_ROW));
    }

    /// A selection end parked in a row's trailing blanks clamps onto that row's hard line
    /// break — the same clamp the caret takes, so the announced range never addresses a
    /// character past the end of the run that carries it.
    #[test]
    fn a_selection_end_in_trailing_blanks_clamps_onto_the_line_break() {
        let snap = live_grid(2, 40, b"hi", Some((1, 0)));
        let update = grid_tree(
            &snap,
            None,
            &GridFrame {
                selection: Some(GridSpan {
                    anchor: (0, 0),
                    focus: (0, 37),
                }),
                ..GridFrame::default()
            },
        );
        let sel = node(&update, GRID).text_selection().copied().unwrap();
        assert_eq!(sel.focus.character_index, 2, "clamped to \"hi\"");
    }

    /// A COMPOSED SPLIT publishes one terminal document per pane instead of one whose rows
    /// are two programs' output spliced together. Measured before this on a live instance:
    /// a vertical split published a single `terminal` node whose every line read
    /// `<left pane row>▐<right pane row>`, with nothing to say which half was which or
    /// which one the keyboard was in.
    #[test]
    fn a_split_frame_publishes_one_terminal_document_per_pane() {
        // The whole composed row — what the single-document projection would publish.
        let composed = live_grid(2, 21, b"left\x1b[12Gright", Some((0, 16)));
        let first_line = composed.value().lines().next().unwrap();
        assert!(
            first_line.contains("left") && first_line.contains("right"),
            "sanity: ONE composed row carries both panes: {first_line:?}"
        );
        let panes = vec![
            GridPane {
                slot: 0,
                label: "aterm terminal — pane 1 of 2".into(),
                snap: live_grid(2, 10, b"left", None),
                row_off: 0,
                col_off: 0,
                focused: false,
                selection: None,
            },
            GridPane {
                slot: 1,
                label: "aterm terminal — pane 2 of 2".into(),
                snap: live_grid(2, 10, b"right", Some((0, 5))),
                row_off: 0,
                col_off: 11,
                focused: true,
                selection: None,
            },
        ];
        let update = grid_tree(
            &composed,
            Some(GEOM),
            &GridFrame {
                panes: &panes,
                ..GridFrame::default()
            },
        );
        let left = NodeId(PANE_BASE);
        let right = NodeId(PANE_BASE + 1);
        assert_eq!(
            node(&update, ROOT).children(),
            [left, right],
            "one document per pane, in layout order"
        );
        assert!(
            !update.nodes.iter().any(|(id, _)| *id == GRID),
            "the spliced whole-screen document is NOT also published"
        );
        assert_eq!(node(&update, left).role(), Role::Terminal);
        assert_eq!(
            node(&update, left).label(),
            Some("aterm terminal — pane 1 of 2")
        );
        assert_eq!(
            node(&update, left).value(),
            Some("left\n\n"),
            "the left pane's own text, without its neighbour"
        );
        assert_eq!(node(&update, right).value(), Some("right\n\n"));
        assert_eq!(
            update.focus, right,
            "focus names the pane the keyboard is in"
        );

        // Each pane owns a disjoint block of row ids, so a sibling's rows can never be
        // mistaken for this pane's lines by the consumer's diff.
        // Slot 0 stays the WHOLE-SCREEN document's, so a split never reuses the ids the
        // unsplit frame minted for text that is no longer there.
        assert_eq!(
            node(&update, left).children()[0],
            NodeId(ROW_BASE + PANE_STRIDE)
        );
        assert_eq!(
            node(&update, right).children()[0],
            NodeId(ROW_BASE + 2 * PANE_STRIDE)
        );
        // …and each pane's rectangle is where it was painted.
        assert_eq!(
            node(&update, right).bounds().unwrap().x0,
            GEOM.origin_x + 11.0 * GEOM.cell_w
        );
        assert_eq!(
            tab_index_for(right),
            None,
            "a pane id is never decoded as a tab switch"
        );
    }

    /// The find panel publishes a real `Role::SearchInput` carrying the live query, a
    /// caret INSIDE it, and the panel's own answer — and it takes focus, because while the
    /// panel is up the keyboard belongs to the query, not to the grid. Before this the
    /// panel existed only as anonymous cells in the grid document and the published caret
    /// stayed on the shell prompt.
    #[test]
    fn the_find_field_is_a_text_input_that_owns_focus() {
        let snap = live_grid(2, 20, b"hello", Some((0, 5)));
        let find = GridFind {
            query: "hell".into(),
            caret: 4,
            status: "match 1 of 3; case sensitive".into(),
        };
        let update = grid_tree(
            &snap,
            None,
            &GridFrame {
                find: Some(&find),
                ..GridFrame::default()
            },
        );
        assert_eq!(
            node(&update, ROOT).children(),
            [FIND, GRID],
            "the panel is announced above the grid, as it is painted"
        );
        let field = node(&update, FIND);
        assert_eq!(field.role(), Role::SearchInput);
        assert_eq!(field.label(), Some("Find"));
        assert_eq!(field.value(), Some("hell"));
        assert_eq!(field.description(), Some("match 1 of 3; case sensitive"));
        assert_eq!(field.children(), [FIND_RUN]);
        assert!(field.supports_action(Action::Focus));
        // A run child is what makes the field a navigable text document
        // (`supports_text_ranges` needs an input role PLUS a run).
        let run = node(&update, FIND_RUN);
        assert_eq!(run.role(), Role::TextRun);
        assert_eq!(run.value(), Some("hell"));
        assert_eq!(run.character_lengths().len(), 4);
        let caret = field.text_selection().copied().unwrap();
        assert_eq!(caret.focus.node, FIND_RUN);
        assert_eq!(caret.focus.character_index, 4);
        assert_eq!(update.focus, FIND, "the keyboard is in the query field");

        // Closing the panel hands focus straight back to the terminal.
        let closed = grid_tree(&snap, None, &GridFrame::default());
        assert_eq!(closed.focus, GRID);
        assert!(!closed.nodes.iter().any(|(id, _)| *id == FIND));
    }

    fn message(message: ChromeMessage, text: &str) -> GridMessage {
        GridMessage {
            message,
            text: text.to_string(),
            detail: None,
            progress: None,
            activates: false,
            bar_row: None,
        }
    }

    /// THE POINT OF A LIVE REGION: the three platforms announce different halves of the
    /// node, so the sentence goes in BOTH. `accesskit_atspi_common` emits its
    /// `Announcement` carrying the NAME and `accesskit_windows` raises
    /// `LiveRegionChanged` for the reader to read the name — while `accesskit_macos`
    /// announces the VALUE and raises nothing at all for a node without one. A message
    /// that filled in one of them would be spoken on one operating system and silent on
    /// the other two.
    #[test]
    fn every_chrome_message_announces_the_same_sentence_by_name_and_by_value() {
        let snap = live_grid(2, 20, b"hi", None);
        let messages: Vec<GridMessage> = ChromeMessage::ORDER
            .iter()
            .map(|kind| message(*kind, &format!("{kind:?} speaks")))
            .collect();
        let update = grid_tree(
            &snap,
            None,
            &GridFrame {
                messages: &messages,
                ..GridFrame::default()
            },
        );
        for kind in ChromeMessage::ORDER {
            let node = node(&update, NodeId(MESSAGE_BASE + kind.slot()));
            let spoken = format!("{kind:?} speaks");
            assert_eq!(node.label(), Some(spoken.as_str()), "{kind:?} name");
            assert_eq!(node.value(), Some(spoken.as_str()), "{kind:?} value");
            assert_eq!(node.live(), Some(kind.politeness()), "{kind:?} politeness");
        }
    }

    /// The transient card is the ONE surface whose words never reach the grid text: it is
    /// a floating raster, not terminal cells, so without this node an "Update ready" or a
    /// "New tab failed: EMFILE" exists only as pixels. It is announced politely, above
    /// the terminal, and it does NOT take focus — the keyboard is still in the shell.
    #[test]
    fn a_transient_notice_is_a_polite_alert_above_the_terminal() {
        let snap = live_grid(2, 20, b"hi", Some((0, 2)));
        let messages = vec![GridMessage {
            activates: true,
            ..message(
                ChromeMessage::Notice,
                "\u{2191} Update ready \u{2014} build 42",
            )
        }];
        let update = grid_tree(
            &snap,
            None,
            &GridFrame {
                messages: &messages,
                ..GridFrame::default()
            },
        );
        let id = NodeId(MESSAGE_BASE + ChromeMessage::Notice.slot());
        assert_eq!(
            node(&update, ROOT).children(),
            [id, GRID],
            "the card is announced above the terminal, as it is painted"
        );
        let card = node(&update, id);
        assert_eq!(card.role(), Role::Alert, "AT-SPI Notification");
        assert_eq!(card.live(), Some(Live::Polite));
        assert_eq!(
            card.value(),
            Some("\u{2191} Update ready \u{2014} build 42")
        );
        assert!(card.supports_action(Action::Click), "one-click apply");
        assert_eq!(update.focus, GRID, "the keyboard is still in the terminal");

        // A card that only informs offers no action, so the tree never promises one the
        // pixels do not have.
        let informational = vec![message(ChromeMessage::Notice, "\u{2717} New tab failed")];
        let update = grid_tree(
            &snap,
            None,
            &GridFrame {
                messages: &informational,
                ..GridFrame::default()
            },
        );
        assert!(!node(&update, id).supports_action(Action::Click));
    }

    /// FLOODING IS THE FAILURE MODE A LIVE REGION HAS. The announcement fires when the
    /// spoken sentence CHANGES, so a download whose byte counter lived in that sentence
    /// would say itself over and over on the way to 100%. The volatile figures belong in
    /// the description — which is a property change on every platform and never an
    /// announcement — and the meter in the node's own numeric value, where a reader can
    /// ask for it instead of being told.
    #[test]
    fn a_status_band_keeps_its_volatile_figures_out_of_the_announcement() {
        let snap = live_grid(2, 20, b"hi", None);
        let at = |done: &str, fill: f32| GridMessage {
            message: ChromeMessage::UpdateStatus,
            text: "aterm update v0.48.0 \u{00b7} downloading\u{2026}".to_string(),
            detail: Some(format!("{done} / 1.2 GB")),
            progress: Some(fill),
            activates: true,
            bar_row: Some(0),
        };
        let id = NodeId(MESSAGE_BASE + ChromeMessage::UpdateStatus.slot());
        let tree = |m: &[GridMessage]| {
            grid_tree(
                &snap,
                None,
                &GridFrame {
                    messages: m,
                    ..GridFrame::default()
                },
            )
        };
        let early = tree(&[at("120 MB", 0.25)]);
        let later = tree(&[at("900 MB", 0.75)]);
        let (early, later) = (node(&early, id), node(&later, id));
        assert_eq!(
            early.label(),
            later.label(),
            "the spoken sentence does not move with the byte counter"
        );
        assert_eq!(early.value(), later.value());
        assert_eq!(early.description(), Some("120 MB / 1.2 GB"));
        assert_eq!(later.description(), Some("900 MB / 1.2 GB"));
        assert_eq!(early.role(), Role::ProgressIndicator, "a real meter");
        assert_eq!(early.numeric_value(), Some(0.25));
        assert_eq!(later.numeric_value(), Some(0.75));
        assert_eq!(early.min_numeric_value(), Some(0.0));
        assert_eq!(early.max_numeric_value(), Some(1.0));

        // No meter ⇒ a plain status band (AT-SPI StatusBar), left numerically unset,
        // which is how AccessKit spells an indeterminate progress bar.
        let flat = tree(&[message(
            ChromeMessage::ToolchainStatus,
            "Installing the ALab toolchain \u{00b7} starting\u{2026}",
        )]);
        let flat = node(
            &flat,
            NodeId(MESSAGE_BASE + ChromeMessage::ToolchainStatus.slot()),
        );
        assert_eq!(flat.role(), Role::Status);
        assert_eq!(flat.numeric_value(), None);
    }

    /// The multi-line-paste guard is a QUESTION, and on the platforms with no native
    /// alert it is the only one asked. It reaches AT-SPI as a real `Alert` and interrupts
    /// (assertive) — but it must not take focus: Enter and Escape answer it by reaching
    /// the very grid a focus move would have taken them from.
    #[test]
    fn the_paste_confirmation_interrupts_without_stealing_the_keyboard() {
        let snap = live_grid(2, 20, b"hi", Some((0, 2)));
        // Built from the SAME two seams the painted band writes its title row from, so
        // this pins the anti-drift property instead of restating one side of it.
        let asked = crate::paste_banner::question("one\ntwo\nthree\nfour\nfive\nsix\nseven");
        let messages = vec![GridMessage {
            detail: Some(crate::paste_banner::ANSWER_KEYS.to_string()),
            ..message(ChromeMessage::PasteConfirm, &asked)
        }];
        let update = grid_tree(
            &snap,
            None,
            &GridFrame {
                messages: &messages,
                ..GridFrame::default()
            },
        );
        let ask = node(
            &update,
            NodeId(MESSAGE_BASE + ChromeMessage::PasteConfirm.slot()),
        );
        assert_eq!(ask.role(), Role::AlertDialog, "AT-SPI Alert");
        assert_eq!(ask.live(), Some(Live::Assertive));
        assert_eq!(ask.label(), Some("!  Paste 7 lines?"));
        assert_eq!(ask.label(), Some(asked.as_str()));
        assert_eq!(ask.description(), Some(crate::paste_banner::ANSWER_KEYS));
        assert!(
            !ask.supports_action(Action::Click),
            "a generic activate names neither answer"
        );
        assert_eq!(update.focus, GRID, "Enter still reaches the terminal");
    }

    /// A message id round-trips to its own surface, is the SAME id from frame to frame
    /// (which is what makes AccessKit diff "the region's text changed" rather than tear
    /// the node down and build another — and the name change is exactly what fires the
    /// announcement), and is refused by the tab decoder, so a reader's activate on a
    /// status band can never be read as "switch to tab N".
    #[test]
    fn message_ids_round_trip_are_stable_and_are_never_read_as_a_tab() {
        let snap = live_grid(2, 20, b"hi", None);
        for kind in ChromeMessage::ORDER {
            let id = NodeId(MESSAGE_BASE + kind.slot());
            assert_eq!(ChromeMessage::from_node(id), Some(kind));
            assert_eq!(tab_index_for(id), None, "{kind:?} is not a tab");
        }
        assert_eq!(
            ChromeMessage::from_node(NodeId(MESSAGE_BASE + 99)),
            None,
            "an unassigned slot decodes to nothing rather than the nearest surface"
        );
        assert_eq!(ChromeMessage::from_node(ROOT), None);

        let first = grid_tree(
            &snap,
            None,
            &GridFrame {
                messages: &[message(ChromeMessage::Notice, "one")],
                ..GridFrame::default()
            },
        );
        let second = grid_tree(
            &snap,
            None,
            &GridFrame {
                messages: &[message(ChromeMessage::Notice, "two")],
                ..GridFrame::default()
            },
        );
        let id_of = |u: &TreeUpdate| {
            u.nodes
                .iter()
                .find(|(_, n)| n.role() == Role::Alert)
                .map(|(id, _)| *id)
                .unwrap()
        };
        assert_eq!(id_of(&first), id_of(&second), "one node, new words");
    }

    /// The accessible WINDOW is named what the titlebar names it. Every assistive client
    /// reads the root's name as the window's, and it is the only thing a user with
    /// several aterm windows open has to tell them apart by ear — the sighted user has
    /// had the running program and the cwd up there the whole time. An uncomposed title
    /// falls back to the app name rather than publishing a nameless window.
    #[test]
    fn the_window_is_named_what_its_titlebar_names_it() {
        let snap = live_grid(2, 20, b"hi", None);
        let titled = grid_tree(
            &snap,
            None,
            &GridFrame {
                window_title: "vim src/main.rs \u{2014} ~/aterm",
                ..GridFrame::default()
            },
        );
        assert_eq!(
            node(&titled, ROOT).label(),
            Some("vim src/main.rs \u{2014} ~/aterm")
        );
        let untitled = grid_tree(&snap, None, &GridFrame::default());
        assert_eq!(node(&untitled, ROOT).label(), Some("aterm"));
    }

    /// An ordinary frame publishes not one message node — a window with nothing to say is
    /// byte-identical to the tree it always was.
    #[test]
    fn a_quiet_frame_publishes_no_message_nodes() {
        let snap = live_grid(2, 20, b"hi", Some((0, 2)));
        let update = grid_tree(&snap, None, &GridFrame::default());
        assert!(
            update
                .nodes
                .iter()
                .all(|(id, _)| ChromeMessage::from_node(*id).is_none())
        );
        assert_eq!(node(&update, ROOT).children(), [GRID]);
    }

    /// Locate a control node by its field key (control nodes are pushed first, in field
    /// order, so the field's index is its node's position in `nodes`).
    fn node_for<'a>(update: &'a TreeUpdate, s: &SettingsState, key: &str) -> &'a Node {
        let idx = s.fields.iter().position(|f| f.key == key).unwrap();
        &update.nodes[idx].1
    }

    #[test]
    fn tree_has_one_node_per_control_plus_groups_and_root() {
        let s = state();
        let update = settings_tree(&s);
        let populated = crate::prefs::Section::ORDER
            .iter()
            .filter(|sec| {
                s.fields
                    .iter()
                    .any(|f| crate::prefs::section_of(f.key) == **sec)
            })
            .count();
        assert_eq!(
            update.nodes.len(),
            s.fields.len() + populated + 1,
            "one node per control + one Group per populated section + root"
        );
        assert!(
            update.nodes.iter().any(|(id, _)| *id == ROOT),
            "root present"
        );
        assert_eq!(update.tree_id, TreeId::ROOT);
        // Focus is the selected control's node.
        assert_eq!(update.focus, NodeId(s.selected as u64 + 1));
        // Control ids stay `field_index + 1` so OS action routing (id-1) is unchanged.
        for (i, _) in s.fields.iter().enumerate() {
            assert_eq!(update.nodes[i].0, NodeId(i as u64 + 1));
        }
    }

    #[test]
    fn control_roles_map_from_edit_kind() {
        let s = state();
        let update = settings_tree(&s);
        let role = |key: &str| node_for(&update, &s, key).role();
        assert_eq!(role(crate::prefs::EDIT_CURSOR_TRAIL), Role::CheckBox); // Bool
        assert_eq!(role(crate::prefs::EDIT_FONT_PX), Role::Slider); // Float w/ range
        assert_eq!(role(crate::prefs::EDIT_CURSOR_TRAIL_MS), Role::Slider); // Integer w/ range
        assert_eq!(role(crate::prefs::EDIT_SCROLLBACK), Role::SpinButton); // Integer, no range
        // The "Trail effect" popup row is an ordinary Enum → ComboBox (the
        // per-effect checkbox expansion is retired, design graft #1).
        assert_eq!(role(crate::prefs::EDIT_CURSOR_TRAIL_STYLE), Role::ComboBox); // Enum
        assert_eq!(role(crate::prefs::EDIT_CURSOR_STYLE), Role::ComboBox); // Enum
        assert_eq!(role(crate::prefs::EDIT_THEME), Role::ComboBox); // Theme
        assert_eq!(role(crate::prefs::EDIT_FONT_FAMILY), Role::TextInput); // Text
        assert_eq!(role(crate::prefs::EDIT_FOREGROUND), Role::TextInput); // Color
    }

    #[test]
    fn slider_exposes_range_and_numeric_value() {
        let mut s = state();
        // Give the font-size slider an explicit in-range value.
        let i = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_FONT_PX)
            .unwrap();
        s.fields[i].seed = Some("15".to_string());
        let update = settings_tree(&s);
        let n = node_for(&update, &s, crate::prefs::EDIT_FONT_PX);
        let r = crate::prefs::range_of(crate::prefs::EDIT_FONT_PX).unwrap();
        assert_eq!(n.role(), Role::Slider);
        assert_eq!(n.min_numeric_value(), Some(r.min));
        assert_eq!(n.max_numeric_value(), Some(r.max));
        assert_eq!(n.numeric_value_step(), Some(r.step));
        assert_eq!(
            n.numeric_value(),
            Some(15.0),
            "current value parsed from the seed"
        );
    }

    #[test]
    fn spinbutton_has_no_range_bounds() {
        let s = state();
        let update = settings_tree(&s);
        let n = node_for(&update, &s, crate::prefs::EDIT_SCROLLBACK);
        assert_eq!(n.role(), Role::SpinButton);
        assert_eq!(n.min_numeric_value(), None, "open-ended: no min");
        assert_eq!(n.max_numeric_value(), None, "open-ended: no max");
    }

    #[test]
    fn combobox_description_lists_the_option_set() {
        let s = state();
        let update = settings_tree(&s);
        let n = node_for(&update, &s, crate::prefs::EDIT_CURSOR_STYLE);
        let opts = combo_options(
            &s.fields[s
                .fields
                .iter()
                .position(|f| f.key == crate::prefs::EDIT_CURSOR_STYLE)
                .unwrap()],
            &s.trail_pack_ids,
        )
        .unwrap();
        let desc = n.description().unwrap_or_default();
        assert!(
            desc.contains("options:"),
            "combo description names its options: {desc}"
        );
        for o in &opts {
            assert!(desc.contains(o.as_str()), "option {o} present in {desc}");
        }
    }

    #[test]
    fn description_reports_current_vs_default() {
        let mut s = state();
        // An overridden numeric names the override + the built-in default.
        let i = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_CURSOR_TRAIL_MS)
            .unwrap();
        s.fields[i].seed = Some("500".to_string());
        let update = settings_tree(&s);
        let n = node_for(&update, &s, crate::prefs::EDIT_CURSOR_TRAIL_MS);
        let desc = n.description().unwrap_or_default();
        assert!(desc.contains("overridden"), "overridden reported: {desc}");
        assert!(
            desc.contains("default 260"),
            "default stripped of marker: {desc}"
        );
        assert!(desc.contains("unit: ms"), "unit surfaced: {desc}");

        // An untouched control reports just the default (no "overridden").
        let s2 = state();
        let update2 = settings_tree(&s2);
        let n2 = node_for(&update2, &s2, crate::prefs::EDIT_FONT_FAMILY);
        let desc2 = n2.description().unwrap_or_default();
        assert!(
            desc2.starts_with("default "),
            "unset reports the default: {desc2}"
        );
        assert!(
            !desc2.contains("overridden"),
            "unset is not overridden: {desc2}"
        );
    }

    #[test]
    fn sections_are_groups_parenting_every_control() {
        let s = state();
        let update = settings_tree(&s);
        let (_, root) = update.nodes.iter().find(|(id, _)| *id == ROOT).unwrap();
        assert_eq!(root.role(), Role::Window);
        // Root's children are Group nodes, one per populated section.
        let groups: Vec<&Node> = root
            .children()
            .iter()
            .map(|gid| &update.nodes.iter().find(|(id, _)| id == gid).unwrap().1)
            .collect();
        assert!(!groups.is_empty(), "at least one section group");
        assert!(
            groups.iter().all(|g| g.role() == Role::Group),
            "children are Groups"
        );
        // Every control id appears under exactly one group.
        let mut seen: Vec<NodeId> = groups.iter().flat_map(|g| g.children().to_vec()).collect();
        seen.sort_by_key(|n| n.0);
        let expected: Vec<NodeId> = (0..s.fields.len()).map(|i| NodeId(i as u64 + 1)).collect();
        assert_eq!(seen, expected, "each control parented by exactly one group");
    }

    #[test]
    fn strip_default_marker_drops_annotation() {
        assert_eq!(strip_default_marker("260 (default)"), "260");
        assert_eq!(strip_default_marker("auto (default)"), "auto");
        assert_eq!(strip_default_marker("15 px"), "15 px");
    }

    /// A ComboBox row reads EXPANDED exactly while its popup menu is open; every other
    /// combo stays collapsed and non-combo rows carry no expanded state at all.
    #[test]
    fn combobox_expanded_tracks_open_menu() {
        let mut s = state();
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_THEME)
            .unwrap();
        let collapsed = settings_tree(&s);
        assert_eq!(
            node_for(&collapsed, &s, crate::prefs::EDIT_THEME).is_expanded(),
            Some(false),
            "menu closed → combo reads collapsed"
        );
        assert!(s.menu_open(), "theme row opens its menu");
        let expanded = settings_tree(&s);
        assert_eq!(
            node_for(&expanded, &s, crate::prefs::EDIT_THEME).is_expanded(),
            Some(true),
            "open menu → its combo reads expanded"
        );
        // cursor_trail_style is another popup combo in the overlay. (cursor_style
        // is no longer one: with the underline option retired it holds two
        // options, renders SEGMENTED, and correctly carries no expanded state.)
        assert_eq!(
            node_for(&expanded, &s, crate::prefs::EDIT_CURSOR_TRAIL_STYLE).is_expanded(),
            Some(false),
            "another combo stays collapsed"
        );
        assert_eq!(
            node_for(&expanded, &s, crate::prefs::EDIT_CURSOR_STYLE).is_expanded(),
            None,
            "the two-option segmented control promises no popup"
        );
        assert_eq!(
            node_for(&expanded, &s, crate::prefs::EDIT_CURSOR_TRAIL).is_expanded(),
            None,
            "a toggle row has no expanded state"
        );
    }
}
