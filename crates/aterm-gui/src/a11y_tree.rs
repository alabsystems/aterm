// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Accessibility projection of the terminal window: the visible grid, the tab strip, the
//! find field and the chrome messages, mapped into an [`aterm_a11y::model::TreeUpdate`] that the
//! native publishers (through `a11y_adapter`) serve to screen readers. Native Settings and Manual publish through `native_accessibility`; the
//! transient overlays build their own trees beside their painters.
//!
//! This module is the pure mapping (model → `TreeUpdate`), unit-tested without a window.
//! The `a11y_adapter::Adapter` that attaches it to a live window is the event-loop
//! wiring built on this seam.
//!
//! Compiled with the native accessibility publisher under `a11y_tree` (`build.rs`).

use aterm_a11y::model::{
    Action, ActionContext, ActionRequest, Live, Node, NodeId, Rect, Role, TextDirection,
    TextPosition, TextSelection, Tree, TreeId, TreeUpdate,
};

/// The window's root accessibility node.
const ROOT: NodeId = NodeId(0);
/// The `Role::Terminal` node under [`ROOT`] when no overlay is open and the frame is one
/// document; it carries the visible grid text. A composed split publishes one
/// [`PANE_BASE`] document per pane instead, and this id is then absent.
const GRID: NodeId = NodeId(1);
/// The in-grid tab strip's `Role::TabList` under [`ROOT`], published only when the strip
/// is on screen and is a switcher (≥ 2 tabs; a solo strip paints the window title).
const TAB_LIST: NodeId = NodeId(2);
/// The find panel's `Role::SearchInput` under [`ROOT`], published only while find mode is
/// live: a second text document, because while the panel is up the keyboard belongs to
/// the query field, not the grid.
const FIND: NodeId = NodeId(3);
/// The find field's single `Role::TextRun`, which makes [`FIND`] a real text document
/// (aterm-a11y's text documents need an input role plus a run), so
/// the caret in the query is navigable.
const FIND_RUN: NodeId = NodeId(4);
/// Chrome message nodes are `MESSAGE_BASE + `[`ChromeMessage::slot`]. The range is well
/// below [`TAB_BASE`], so [`tab_index_for`] refuses a message id and a click on a status
/// band never decodes as "switch to tab N". Status bands share this range with every other
/// chrome message: one announcement contract, one activation route, one slot round-trip.
const MESSAGE_BASE: u64 = 1 << 20;
/// The open Autopilot menu (`crate::autopilot_menu`, design §9.3.3) is `MENU_BASE`, and
/// its items `MENU_BASE + 1 + (epoch · MENU_ROWS + item)` ([`menu_item_node_id`],
/// [`menu_item_for`]): above every chrome message's slot (whose decoder refuses them) and
/// below [`CHIP_BASE`]. The EPOCH ([`GridMenu::epoch`]) names the rows the items were
/// published from, so a reader's click on an item of a tree built before the rows moved
/// carries that folded identity and is refused where it differs from the rows shown
/// now. The compact 22-bit hash is not a collision-free generation.
const MENU_BASE: u64 = 1 << 29;
/// The rows a menu may publish: the item's slot in its id.
const MENU_ROWS: u64 = 64;
/// The bits of a menu's epoch: with [`MENU_ROWS`] slots each, the last item id stays
/// below [`CHIP_BASE`] (`MENU_BASE + 1 + (2^22 − 1) · 64 + 63 < 2^30`).
const MENU_EPOCH_BITS: u32 = 22;
/// The chips and facts in a Claude Code composer's rules (design §9.2.1, owner surfaces
/// §5, `crate::claude_facts`) are `CHIP_BASE + session · 16 + slot` ([`chip_for`]):
/// Autopilot `0`, Team `1`, the top rule's key `2`, the model `3`, the effort `4`, the
/// working directory `5`, the branch `6`, the usage `7`, the permission-mode light `8`,
/// the top rule's harness switch `9`, the title slot `10`, the context `11`, the
/// switch's `on` button `12`; `13..16` are free and decode as nothing.
/// Between the menu's range and [`TAB_BASE`], so [`tab_index_for`] and
/// [`ChromeMessage::from_node`] refuse a chip and a click on one never decodes as a tab
/// switch or a band's capsule.
const CHIP_BASE: u64 = 1 << 30;
/// The stats deck (`crate::stats_deck`, `crate::stats_sill`; Stats Deck v2
/// §12) is `STATS_BASE + …`: its `Role::Group` at `STATS_BASE` with one
/// `Role::Meter` a resource after it ([`stats_meter_id`]), the open lens from
/// [`STATS_LENS`], the cores' group at `STATS_BASE + 64` and one meter a core
/// from `STATS_BASE + 128`. Below [`MENU_BASE`] and far above every chrome
/// message's slot, so no decoder takes a stats node for a message, a menu
/// item, a chip or a tab; a stats node offers no action but the keyboard
/// lens's focus, so the routing never needs one.
const STATS_BASE: u64 = 1 << 28;
/// The cores a published cores group may name (`STATS_BASE + 128 + core`
/// stays below [`MENU_BASE`]).
const STATS_CORES: usize = 1 << 20;
/// Tab items are `TAB_BASE + tab_index`. A high, disjoint range so a tab id can never be
/// mistaken for any other node by [`crate::App::on_accessibility_action`]'s routing.
const TAB_BASE: u64 = 1 << 40;
/// One `Role::TextRun` per visible grid row at `ROW_BASE + row_index * RUNS_PER_ROW`, plus
/// one id per extra run of a row wider than [`RUN_CHARS`]. Disjoint from every other range
/// and stable across frames: aterm-a11y's tree diffs trees by node id to synthesise the
/// `object:text-changed` and `object:text-caret-moved` events, so a new line of output is
/// an insert rather than a remove-and-add of the whole screen.
const ROW_BASE: u64 = 1 << 48;

/// Id space one terminal document may spend on its row runs. A split frame publishes one
/// document per pane, and the pane's slot (not its list position) picks the block, so a
/// sibling's row count changing never renumbers it. Slot 0 is the whole-screen document.
const PANE_STRIDE: u64 = 1 << 32;

/// Pane container nodes are `PANE_BASE + slot`, above [`ROW_BASE`] where
/// [`tab_index_for`] already refuses, so a click on a pane never decodes as a tab switch.
const PANE_BASE: u64 = 1 << 62;

/// Characters per text run. The model's word starts are `[u8]` and aterm-a11y's text casts the
/// query offset to `u8`, so 256 is the widest a run can be and answer word queries right.
const RUN_CHARS: usize = 256;

/// Run-id stride per row. A row wider than `RUN_CHARS * RUNS_PER_ROW` characters would
/// collide with the next row's ids, so the chunker stops there and the last run carries
/// the remainder.
const RUNS_PER_ROW: u64 = 256;

/// Split one row into runs no wider than [`RUN_CHARS`] characters, on character
/// boundaries. A row that fits comes back as a single chunk.
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

/// A minimal valid tree (an empty window root) for a surface with nothing to expose —
/// the OS a11y client always needs a root.
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

/// Where the visible grid sits in the window's client area, in physical pixels (the space
/// the AT-SPI adapter publishes root window bounds in), so node `bounds` land on the glyphs.
///
/// Optional: without it the grid still exposes a complete AT-SPI `Text` interface (text,
/// caret, line/word/character granularity); only `Component` extents and
/// `GetCharacterExtents` are unavailable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GridGeometry {
    /// Left edge of column 0, physical px from the client area's left edge.
    pub(crate) origin_x: f64,
    /// Top edge of grid row 0, below the tab strip (the snapshot drops the strip rows).
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
    /// Top edge of the status bar band, directly below the strip and above grid row 0.
    /// Equal to `origin_y` when no bar is up.
    pub(crate) bars_y: f64,
    /// Height of one bar row in physical px (each bar is exactly one row).
    pub(crate) bar_h: f64,
}

/// One publishable tab of the in-grid strip: the painted label, whether it is active, and
/// its column span (`start_col..end_col`) from the cached `TabSegment` hit geometry the
/// mouse uses, so announced bounds and clickable pixels are the same rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridTab {
    /// The tab's index in the window's tab set — the argument `switch_tab_in` takes.
    pub(crate) index: usize,
    /// The stable tab id captured with this slot.
    pub(crate) id: u64,
    /// The label the strip painted for this tab.
    pub(crate) title: String,
    /// This is the active tab.
    pub(crate) selected: bool,
    /// First column of the tab's segment, inclusive.
    pub(crate) start_col: u16,
    /// One past the tab's last column.
    pub(crate) end_col: u16,
    /// The theme the tab's session asks for of its own (or its identity's),
    /// announced as the tab's description so a tinted tab can be told apart;
    /// `None` for a tab in the window's theme.
    pub(crate) theme: Option<String>,
}

/// A painted text selection in one document's own row/column space, both ends clamped to
/// the text those rows publish.
///
/// The columns come from the renderer's per-row selection predicate
/// (`aterm_core::render::RenderInput::selection_row_key`), the same source the highlight
/// is drawn from. That span is content-independent, while `TextSelection::contains_cell`
/// snaps a double-width glyph whole (see `selection_row_span_of`), so an edge on a wide
/// lead paints its continuation one column past `hi`; a screen reader hears the glyph,
/// not its pad cell.
///
/// `focus` is the end of the span, not the drag direction: the predicate is normalised,
/// and a screen reader reads the selected text, which is direction-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GridSpan {
    /// First selected cell, `(row, column)`.
    pub(crate) anchor: (usize, usize),
    /// One past the last selected cell, `(row, column)`.
    pub(crate) focus: (usize, usize),
}

/// One visible split pane as its own terminal document. A composed split frame tiles
/// every pane onto the same rows, so reading the whole frame splices unrelated programs'
/// output together; one document per pane is navigable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridPane {
    /// This pane's id slot. Namespaces its row runs ([`PANE_STRIDE`]) so a sibling gaining
    /// or losing rows never renumbers this pane's lines under the consumer's diff.
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

/// The find panel's query field. The panel is painted as terminal cells, so its text is
/// already in the grid document, but as anonymous output with the caret on the shell
/// prompt; this node says the user is typing in a search box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridFind {
    /// The live query, exactly as the well paints it.
    pub(crate) query: String,
    /// The edit position as a character index into `query` (the painter's caret).
    pub(crate) caret: usize,
    /// The answer the panel shows — `match 2 of 7`, `no matches`, `bad regex` — plus the
    /// active modes, as the field's accessible description.
    pub(crate) status: String,
}

/// A chrome message the window shows unasked: a surface a screen reader cannot discover
/// by exploring, because it may be gone by then.
///
/// Each variant owns a fixed [`ChromeMessage::slot`], so its node id is stable while the
/// message stands and aterm-a11y's tree emits "text changed" (which is what AT-SPI and
/// UIA announce) rather than replacing the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChromeMessage {
    /// The multi-line-paste confirmation band ([`crate::paste_banner`]): the pastejacking
    /// guard's prompt on platforms with no native alert. A question; the loudest thing
    /// this tree says.
    PasteConfirm,
    /// The close/quit confirmation band ([`crate::close_confirm`], Windows): a destructive
    /// question, announced as an alert.
    CloseConfirm,
    /// The message band's top row (`crate::message_band`): one fixed slot per band row,
    /// not per message, so a name change is "text changed", not a new node.
    BandRow0,
    /// The band's second row.
    BandRow1,
    /// The band's third row.
    BandRow2,
    /// The band's overflow row (`N more messages`): one link to Settings ▸ Messages.
    BandOverflow,
    /// One capsule of one band row: a button child of that row's node, named by the
    /// capsule's full label.
    BandCapsule {
        /// The band row, 0 = topmost.
        row: u8,
        /// The capsule: the authored intents by index, then [`CAPSULE_DETAILS`] for the
        /// implicit `Details ›` ([`capsule_index`] / [`capsule_action`]).
        index: u8,
    },
    /// The needs pill (`crate::presence`): the window's most pressing need as the pill
    /// says it, spoken without its age so the sentence moves only with the need.
    PresenceStatus,
}

/// The capsule index of the implicit `Details ›` in a [`ChromeMessage::BandCapsule`]
/// slot: after the two authored intents a row may carry.
pub(crate) const CAPSULE_DETAILS: u8 = 2;
/// Capsule slots per band row (`16 + row·4 + index`): two authored, the Details, one
/// spare.
const CAPSULES_PER_ROW: u64 = 4;
/// The first capsule slot.
const CAPSULE_BASE: u64 = 16;

/// The slot index of a capsule's action: an authored intent keeps its index; the
/// implicit `Details ›` (`ActionIndex::DETAILS`) takes [`CAPSULE_DETAILS`].
pub(crate) fn capsule_index(action: aterm_messages::ActionIndex) -> u8 {
    if action.is_details() {
        CAPSULE_DETAILS
    } else {
        action.0
    }
}

/// The inverse of [`capsule_index`].
pub(crate) fn capsule_action(index: u8) -> aterm_messages::ActionIndex {
    if index == CAPSULE_DETAILS {
        aterm_messages::ActionIndex::DETAILS
    } else {
        aterm_messages::ActionIndex(index)
    }
}

impl ChromeMessage {
    /// Every row-level message in publish order (the painted order, top down). A variant
    /// missing here is never published and [`ChromeMessage::from_node`] would not
    /// round-trip it, which the slot round-trip test checks. Capsules are children of
    /// their row and decode by arithmetic.
    pub(crate) const ORDER: [Self; 7] = [
        Self::PresenceStatus,
        Self::BandRow0,
        Self::BandRow1,
        Self::BandRow2,
        Self::BandOverflow,
        Self::PasteConfirm,
        Self::CloseConfirm,
    ];

    /// The band row's message for `row` (0 = topmost), or `None` past the band's three.
    pub(crate) const fn band_row(row: usize) -> Option<Self> {
        match row {
            0 => Some(Self::BandRow0),
            1 => Some(Self::BandRow1),
            2 => Some(Self::BandRow2),
            _ => None,
        }
    }

    /// The band row this message names, for the three row slots; `None` otherwise.
    pub(crate) const fn band_row_index(self) -> Option<usize> {
        match self {
            Self::BandRow0 => Some(0),
            Self::BandRow1 => Some(1),
            Self::BandRow2 => Some(2),
            _ => None,
        }
    }

    /// This message's permanent id slot. The band's first two rows take slots 3 and 4.
    /// Slots 0 and 2 are unused: nothing is published there and
    /// [`ChromeMessage::from_node`] decodes nothing, so a stale activate on one is a no-op.
    /// The close confirmation (Windows) takes 8, the first slot after the band's overflow
    /// row and below the capsule range.
    const fn slot(self) -> u64 {
        match self {
            Self::PasteConfirm => 1,
            Self::BandRow0 => 3,
            Self::BandRow1 => 4,
            Self::PresenceStatus => 5,
            Self::BandRow2 => 6,
            Self::BandOverflow => 7,
            Self::CloseConfirm => 8,
            Self::BandCapsule { row, index } => {
                CAPSULE_BASE + (row as u64) * CAPSULES_PER_ROW + index as u64
            }
        }
    }

    /// This message's node id in [`grid_tree`].
    fn node_id(self) -> NodeId {
        NodeId(MESSAGE_BASE + self.slot())
    }

    /// Decode a screen reader's action target back to the message it names, or `None`
    /// when the id is not one of [`grid_tree`]'s message nodes. Rejects rather than
    /// clamps, so a stale request is a no-op.
    pub(crate) fn from_node(node: NodeId) -> Option<Self> {
        let slot = node.0.checked_sub(MESSAGE_BASE)?;
        if let Some(offset) = slot.checked_sub(CAPSULE_BASE) {
            let (row, index) = (offset / CAPSULES_PER_ROW, offset % CAPSULES_PER_ROW);
            if row < 3 && index <= u64::from(CAPSULE_DETAILS) {
                return Some(Self::BandCapsule {
                    row: row as u8,
                    index: index as u8,
                });
            }
            return None;
        }
        Self::ORDER.into_iter().find(|m| m.slot() == slot)
    }

    /// How loudly this message interrupts. Only the two confirmations are assertive: each
    /// stops something (the paste, the close) until answered, and a polite announcement
    /// would queue behind terminal output. Everything else is polite.
    const fn politeness(self) -> Live {
        match self {
            Self::PasteConfirm | Self::CloseConfirm => Live::Assertive,
            Self::BandRow0
            | Self::BandRow1
            | Self::BandRow2
            | Self::BandOverflow
            | Self::BandCapsule { .. }
            | Self::PresenceStatus => Live::Polite,
        }
    }

    /// The platform role. `Alert` reaches AT-SPI as `Notification` and `AlertDialog` as
    /// `Alert` ("news" vs "being asked"). A band row that warns or asks (`alarm`) is a
    /// polite `Alert`; a metered one is a `ProgressIndicator`; one without a meter is a
    /// `Status` (AT-SPI `StatusBar`). Only the paste and close confirmations are dialogs.
    const fn role(self, metered: bool, alarm: bool) -> Role {
        match self {
            Self::PasteConfirm | Self::CloseConfirm => Role::AlertDialog,
            Self::BandRow0 | Self::BandRow1 | Self::BandRow2 => {
                if alarm {
                    Role::Alert
                } else if metered {
                    Role::ProgressIndicator
                } else {
                    Role::Status
                }
            }
            // A link to the page, not news of its own.
            Self::BandOverflow => Role::Link,
            Self::BandCapsule { .. } => Role::Button,
            // The band never draws a meter: a status line, whatever the row says.
            Self::PresenceStatus => Role::Status,
        }
    }
}

/// One capsule of a band row as the frame paints it — the button child of the row's
/// node ([`push_message`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BandCapsule {
    /// The full label (the painted form may be the short one).
    pub(crate) label: &'static str,
    /// The first cell of the chip.
    pub(crate) col: usize,
    /// Its cells.
    pub(crate) width: usize,
    /// Its slot within the row ([`capsule_index`]).
    pub(crate) index: u8,
}

/// One chrome message as the frame shows it.
///
/// `text` is the spoken sentence; `detail` is what a reader may read but must not be
/// interrupted with. Announcements fire when the spoken sentence changes, so volatile
/// figures (a download's percentage) live in the description.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GridMessage {
    /// Which surface this is — and, through it, the node id, the role and the politeness.
    pub(crate) message: ChromeMessage,
    /// The owner and action-list revision published with this row and its capsules.
    pub(crate) context: Option<ActionContext>,
    /// The sentence a screen reader speaks. Stable while the surface says the same thing.
    pub(crate) text: String,
    /// Live figures, key hints, the rest of a multi-line banner: read on demand.
    pub(crate) detail: Option<String>,
    /// Determinate progress in `0..=1`, when the band draws a meter.
    pub(crate) progress: Option<f32>,
    /// Work in flight with no fraction (the comet): an indeterminate progress indicator
    /// (the role, with no numeric value).
    pub(crate) busy: bool,
    /// A warning, an error, or a question (an ask, a consequential capsule) — the row
    /// is an `Alert`, not a `Status`.
    pub(crate) alarm: bool,
    /// A screen reader may activate this message (a band row's primary intent, a
    /// capsule's). `false` for a message that only informs, so the tree never offers an
    /// action the pixels do not have.
    pub(crate) activates: bool,
    /// Which status bar row (0 = topmost) this message occupies, if any.
    ///
    /// A status band reserves its own row between the strip and grid row 0, and
    /// [`GridGeometry`] knows where, so its rectangle is real for click routing,
    /// magnifiers and touch exploration. `None` for a message with no rectangle worth
    /// publishing: the paste question overwrites grid rows, so a rectangle there would
    /// sit on text that is not it.
    pub(crate) bar_row: Option<usize>,
    /// The row's capsules, each published as a button child with its own rectangle.
    pub(crate) capsules: Vec<BandCapsule>,
}

/// One of the targets in a Claude Code composer's rules the tree publishes: the harness's
/// Autopilot (design §9.2.1, §2.3), the Team chip (owner surfaces §5), the top rule's
/// effort key, the bottom rule's facts (`crate::claude_facts`), the permission-mode light,
/// the top rule's harness switch and the chips' title slot — everything aterm draws in
/// the rules, so a reader that hears Claude's own `─` there (the grid's text is Claude's)
/// loses none of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HarnessChip {
    /// The Autopilot chip: a click steps the session's word (or, where a switch of
    /// aterm.toml holds the harness off, turns that switch on).
    Autopilot,
    /// The Team chip (`✋ you`, `⇠ <manager>`, `⇉N`): a click hands the tab back, goes to
    /// the manager, or opens the Overview at a manager.
    Team,
    /// The top rule's effort key (Claude's tag, or aterm's ghost of it): a
    /// click toggles it for the session.
    TopEffort,
    /// The model fact (`◆ Opus 5.5`): a click hides or shows it.
    Model,
    /// The effort fact (`xhigh`): a click hides or shows it.
    Effort,
    /// The working directory fact (`⌂ ~/aterm`): a click hides or shows it.
    Path,
    /// The branch fact (`⎇ main`): a click hides or shows it.
    Branch,
    /// The context fact (`◐ ctx 84% left`): a click hides or shows it.
    Context,
    /// The usage fact (`Σ 6m33s ↑20M ↓82k`).
    Usage,
    /// The permission-mode light (`⏸ plan`), drawn only where the mode is not one the
    /// owner expects: a click presses Claude's own shift+tab toward bypass or auto.
    Mode,
    /// The harness switch on the composer's top rule (`alab harness ● on`, `alab
    /// harness ○ off`), named with the product (`alab harness on: Claude Code`): a
    /// `Role::Switch` with its on/off state; a click flips it.
    Switch,
    /// An off switch's `on` button: a click turns the harness on.
    SwitchOn,
    /// The chips' title slot beside them (a chip's title, a refusal, a toggle's notice,
    /// the act's label): text, published as a status node no reader is interrupted by.
    Title,
}

impl HarnessChip {
    /// The targets a session's ids have room for.
    const SLOTS: u64 = 16;

    /// Its place in its session's ids.
    const fn index(self) -> u64 {
        match self {
            Self::Autopilot => 0,
            Self::Team => 1,
            Self::TopEffort => 2,
            Self::Model => 3,
            Self::Effort => 4,
            Self::Path => 5,
            Self::Branch => 6,
            Self::Usage => 7,
            Self::Mode => 8,
            Self::Switch => 9,
            Self::Title => 10,
            Self::Context => 11,
            Self::SwitchOn => 12,
        }
    }

    /// The target in slot `index` of a session's ids, or `None` for a free slot.
    const fn at(index: u64) -> Option<Self> {
        Some(match index {
            0 => Self::Autopilot,
            1 => Self::Team,
            2 => Self::TopEffort,
            3 => Self::Model,
            4 => Self::Effort,
            5 => Self::Path,
            6 => Self::Branch,
            7 => Self::Usage,
            8 => Self::Mode,
            9 => Self::Switch,
            10 => Self::Title,
            11 => Self::Context,
            12 => Self::SwitchOn,
            _ => return None,
        })
    }

    /// The node of a fact.
    pub(crate) const fn of_fact(fact: crate::claude_lights::Fact) -> Self {
        use crate::claude_lights::Fact;
        match fact {
            Fact::Model => Self::Model,
            Fact::Effort => Self::Effort,
            Fact::Path => Self::Path,
            Fact::Branch => Self::Branch,
            Fact::Context => Self::Context,
            Fact::Usage => Self::Usage,
            Fact::TopEffort => Self::TopEffort,
        }
    }

    /// The fact it is, where it is one.
    pub(crate) const fn fact(self) -> Option<crate::claude_lights::Fact> {
        use crate::claude_lights::Fact;
        match self {
            Self::Model => Some(Fact::Model),
            Self::Effort => Some(Fact::Effort),
            Self::Path => Some(Fact::Path),
            Self::Branch => Some(Fact::Branch),
            Self::Context => Some(Fact::Context),
            Self::Usage => Some(Fact::Usage),
            Self::TopEffort => Some(Fact::TopEffort),
            Self::Autopilot
            | Self::Team
            | Self::Mode
            | Self::Switch
            | Self::SwitchOn
            | Self::Title => None,
        }
    }
}

/// One harness chip as the frame paints it (design §9.2.1): GLASS the `text` reads never
/// see, so the tree is where a screen reader meets it — a `Role::Button` over its cells,
/// clickable, NAMED IN WORDS and never a glyph (`Autopilot: cruise. Lands at 18:00.`,
/// `Worker of lab-sweep`), the trail's newest act its DESCRIPTION (`Last: approved at
/// 14:05.`), never a live region: a reader is not interrupted by every act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridChip {
    /// The session (local id) whose composer rule it is in.
    pub(crate) session: u64,
    /// Which chip.
    pub(crate) chip: HarnessChip,
    /// What it is called, in words.
    pub(crate) label: String,
    /// The newest act, in words, where the harness has taken one.
    pub(crate) description: Option<String>,
    /// A switch's state (the harness switch, [`HarnessChip::Switch`]): published as a
    /// `Role::Switch` toggled so, `Mixed` while it turns on. `None` for a button.
    pub(crate) toggled: Option<aterm_a11y::model::Toggled>,
    /// Its grid row (the snapshot's: below the tab strip and the bars).
    pub(crate) row: usize,
    /// Its first cell.
    pub(crate) col: usize,
    /// Its cells.
    pub(crate) width: usize,
}

/// One row of the open Autopilot menu as its model (`crate::autopilot_menu`) hands it to
/// the tree (design §9.3.3): its words (`Cruise for 1 hour, then land`), never its glyphs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridMenuItem {
    /// What the row does, in words.
    pub(crate) label: String,
    /// `Some(checked)` for a row of the mode choice (the `●` row is checked: a
    /// `Role::MenuItemRadio`); `None` for an action row (a `Role::MenuItem`).
    pub(crate) checked: Option<bool>,
    /// The row is shown but does nothing (the mode and timer rows under the master
    /// switch, design §9.3.3).
    pub(crate) disabled: bool,
    /// Its grid row.
    pub(crate) row: usize,
}

/// The open Autopilot menu, cells over the pane (design §9.3.3): a `Role::Menu` named by
/// its header, one item per row. The keyboard is the menu's while it is open, so the
/// tree's focus is its highlighted row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridMenu {
    /// Its header, in words (`Autopilot, tab 3, fix the footer`).
    pub(crate) label: String,
    /// The rows it was built from, folded ([`menu_epoch_from`]): minted into every item's
    /// id, so a click names the rows it was heard from as well as its place in them.
    pub(crate) epoch: u32,
    /// Its rows, top down.
    pub(crate) items: Vec<GridMenuItem>,
    /// The highlighted row, which the keys move.
    pub(crate) selected: Option<usize>,
    /// Its first grid row and cell, and its size in cells.
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

/// The node id of `session`'s `chip`, or `None` for a session id too large for the
/// range (no local id is).
fn chip_node_id(session: u64, chip: HarnessChip) -> Option<NodeId> {
    let id = session
        .checked_mul(HarnessChip::SLOTS)
        .and_then(|at| at.checked_add(chip.index()))
        .and_then(|at| CHIP_BASE.checked_add(at))?;
    (id < TAB_BASE).then_some(NodeId(id))
}

/// Decode a screen reader's action target back to the harness chip it names, or `None`
/// when the id is not one of [`grid_tree`]'s chip nodes. Rejects rather than clamps.
pub(crate) fn chip_for(node: NodeId) -> Option<(u64, HarnessChip)> {
    let at = node
        .0
        .checked_sub(CHIP_BASE)
        .filter(|_| node.0 < TAB_BASE)?;
    Some((
        at / HarnessChip::SLOTS,
        HarnessChip::at(at % HarnessChip::SLOTS)?,
    ))
}

/// Fold a hash of the menu's rows to a menu epoch ([`GridMenu::epoch`]).
pub(crate) fn menu_epoch_from(hash: u64) -> u32 {
    (hash & ((1 << MENU_EPOCH_BITS) - 1)) as u32
}

/// The node id of item `item` of a menu published at `epoch`, or `None` past the rows a
/// menu may publish.
fn menu_item_node_id(epoch: u32, item: usize) -> Option<NodeId> {
    let item = u64::try_from(item).ok().filter(|item| *item < MENU_ROWS)?;
    let epoch = u64::from(epoch) & ((1 << MENU_EPOCH_BITS) - 1);
    Some(NodeId(MENU_BASE + 1 + epoch * MENU_ROWS + item))
}

/// Decode a screen reader's action target back to the open menu's item it names: the
/// epoch of the rows it was published from and its index in [`GridMenu::items`] — or
/// `None` when the id is no item of a menu [`grid_tree`] published. The window acts on it
/// only where that epoch is the open menu's now (`App::choose_autopilot_menu_item_for_reader`).
pub(crate) fn menu_item_for(node: NodeId) -> Option<(u32, usize)> {
    let at = node
        .0
        .checked_sub(MENU_BASE + 1)
        .filter(|_| node.0 < CHIP_BASE)?;
    let epoch = u32::try_from(at / MENU_ROWS).ok()?;
    let item = usize::try_from(at % MENU_ROWS).ok()?;
    (menu_item_node_id(epoch, item) == Some(node)).then_some((epoch, item))
}

/// The rectangle of `width` cells at grid `row`, `col`, `height` rows tall.
fn cells_rect(g: GridGeometry, row: usize, col: usize, width: usize, height: usize) -> Rect {
    Rect {
        x0: g.origin_x + col as f64 * g.cell_w,
        y0: g.origin_y + row as f64 * g.cell_h,
        x1: g.origin_x + (col + width) as f64 * g.cell_w,
        y1: g.origin_y + (row + height) as f64 * g.cell_h,
    }
}

/// Emit the harness's chips, each a clickable `Role::Button` over its cells, named in
/// words with its newest act as its description; return their ids in painted order. The
/// Autopilot chip also says it has a popup menu (`HasPopup::Menu`) and offers
/// `Action::ShowContextMenu`, which opens it (design §9.3.3: what its right-click does,
/// where a click cycles the mode), and so does every fact — the model's, the effort's, the
/// folder's and the branch's menus, the usage's Traffic card (`crate::fact_menu`); Team has
/// none. The title slot is no button: a `Role::Status` named by its words, with no action
/// and no live politeness, so a reader finds it beside the chips and is never interrupted
/// by it.
fn push_chips(
    nodes: &mut Vec<(NodeId, Node)>,
    chips: &[GridChip],
    geom: Option<GridGeometry>,
) -> Vec<NodeId> {
    let mut ids = Vec::with_capacity(chips.len());
    for chip in chips {
        let Some(id) = chip_node_id(chip.session, chip.chip) else {
            continue;
        };
        let title = chip.chip == HarnessChip::Title;
        let mut node = Node::new(if title {
            Role::Status
        } else if chip.toggled.is_some() {
            Role::Switch
        } else {
            Role::Button
        });
        node.set_label(chip.label.clone());
        if let Some(toggled) = chip.toggled {
            node.set_toggled(toggled);
        }
        if let Some(description) = &chip.description {
            node.set_description(description.clone());
        }
        if let Some(g) = geom {
            node.set_bounds(cells_rect(g, chip.row, chip.col, chip.width, 1));
        }
        if !title {
            node.add_action(Action::Click);
        }
        let menu = chip.chip == HarnessChip::Autopilot || chip.chip.fact().is_some();
        if menu {
            node.set_has_popup(aterm_a11y::model::HasPopup::Menu);
            node.add_action(Action::ShowContextMenu);
        }
        nodes.push((id, node));
        ids.push(id);
    }
    ids
}

/// Emit the open menu as a `Role::Menu` with one item per row — a mode row a
/// `Role::MenuItemRadio` with its checked state, an action row a `Role::MenuItem` — each
/// named in words, a disabled row marked so and offered no click; return the menu's id
/// and the id of its highlighted row.
fn push_menu(
    nodes: &mut Vec<(NodeId, Node)>,
    menu: &GridMenu,
    geom: Option<GridGeometry>,
) -> (NodeId, Option<NodeId>) {
    let menu_id = NodeId(MENU_BASE);
    let mut children = Vec::with_capacity(menu.items.len());
    let mut focus = None;
    for (i, item) in menu.items.iter().enumerate() {
        let Some(id) = menu_item_node_id(menu.epoch, i) else {
            break;
        };
        let mut node = Node::new(match item.checked {
            Some(_) => Role::MenuItemRadio,
            None => Role::MenuItem,
        });
        node.set_label(item.label.clone());
        if let Some(checked) = item.checked {
            node.set_toggled(if checked {
                aterm_a11y::model::Toggled::True
            } else {
                aterm_a11y::model::Toggled::False
            });
        }
        if item.disabled {
            node.set_disabled();
        } else {
            node.add_action(Action::Click);
        }
        if let Some(g) = geom {
            node.set_bounds(cells_rect(g, item.row, menu.col, menu.width, 1));
        }
        if menu.selected == Some(i) {
            focus = Some(id);
        }
        nodes.push((id, node));
        children.push(id);
    }
    let mut node = Node::new(Role::Menu);
    node.set_label(menu.label.clone());
    if let Some(g) = geom {
        node.set_bounds(cells_rect(g, menu.row, menu.col, menu.width, menu.height));
    }
    node.set_children(children);
    nodes.push((menu_id, node));
    (menu_id, focus)
}

/// One stats meter: a resource's % of its maximum or a core's % busy, named
/// stably ("CPU", "Super core 1") and described in words (the row's sentence,
/// `aterm_vitals::words::row_sentence`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StatsMeter {
    /// The stable name; only the value and the words change.
    pub(crate) name: String,
    /// The %, `0..=100`; `None` when it is not known, so a reader hears no
    /// value rather than a zero.
    pub(crate) value: Option<f32>,
    /// The value in words.
    pub(crate) words: String,
}

/// The stats deck as a reader meets it (Stats Deck v2 §12): a group named
/// for the deck holding one meter a resource in row order, then a group of
/// one meter a core.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StatsGroup {
    /// "Stats deck, last 10 minutes".
    pub(crate) name: &'static str,
    /// One meter a resource, in row order.
    pub(crate) meters: Vec<StatsMeter>,
    /// The cores' group's name: "18 CPU cores".
    pub(crate) cores_name: String,
    /// One meter a core, in cluster order; empty before the cores are read,
    /// and then there is no cores group.
    pub(crate) cores: Vec<StatsMeter>,
}

/// The stats deck on this window (the Deck or the sill), `None` when off.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GridStats {
    /// The deck's group: its meters and its cores.
    pub(crate) group: StatsGroup,
    /// Where the deck's card sits, physical px.
    pub(crate) bounds: Option<Rect>,
    /// The open lens, while one is open.
    pub(crate) lens: Option<StatsLens>,
}

/// The open stats lens as a reader meets it (Stats Deck v2 §12): a group named
/// "<Name> close-up", described by its explainer and its keys, holding a well
/// for the answer and for each section the lens shows, each named by its
/// heading and described by its lines. A lens that takes the keys (opened by
/// ⌘I, or pinned by a click or Return) holds them, so its first well is the
/// window's focus; a hover lens is only there to be read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StatsLens {
    /// Whether it takes ←/→/↑/↓, ⌘C and Esc: the keyboard or pinned lens.
    pub(crate) keys: bool,
    /// "Disk close-up".
    pub(crate) name: String,
    /// What its heat measures.
    pub(crate) explainer: String,
    /// The wells in reading order: each one's heading and its lines.
    pub(crate) wells: Vec<(String, String)>,
    /// Where the lens card sits, physical px.
    pub(crate) bounds: Option<Rect>,
}

/// The lens group's id (its wells follow it, at most 15): after the deck's
/// meters and before the cores' group at `STATS_BASE + 64`.
const STATS_LENS: u64 = STATS_BASE + 48;

/// The deck's `k`th meter's id (the group itself is `STATS_BASE`); `None`
/// past the 15 the range holds before the lens's.
fn stats_meter_id(k: usize) -> Option<NodeId> {
    (k < 15).then(|| NodeId(STATS_BASE + 1 + k as u64))
}

/// Emit the stats deck's group, its meters and its cores; return the
/// group's id.
fn push_stats(nodes: &mut Vec<(NodeId, Node)>, stats: &GridStats) -> NodeId {
    let meter_node = |m: &StatsMeter| {
        let mut node = Node::new(Role::Meter);
        node.set_label(m.name.clone());
        if let Some(v) = m.value {
            node.set_numeric_value(f64::from(v));
        }
        node.set_min_numeric_value(0.0);
        node.set_max_numeric_value(100.0);
        node.set_description(m.words.clone());
        node
    };
    let group = &stats.group;
    let group_id = NodeId(STATS_BASE);
    let mut children = Vec::with_capacity(group.meters.len() + 1);
    for (k, m) in group.meters.iter().enumerate() {
        let Some(id) = stats_meter_id(k) else {
            break;
        };
        nodes.push((id, meter_node(m)));
        children.push(id);
    }
    if !group.cores.is_empty() {
        let cores_id = NodeId(STATS_BASE + 64);
        let mut cells = Vec::with_capacity(group.cores.len().min(STATS_CORES));
        for (i, m) in group.cores.iter().take(STATS_CORES).enumerate() {
            let id = NodeId(STATS_BASE + 128 + i as u64);
            nodes.push((id, meter_node(m)));
            cells.push(id);
        }
        let mut node = Node::new(Role::Group);
        node.set_label(group.cores_name.clone());
        node.set_children(cells);
        nodes.push((cores_id, node));
        children.push(cores_id);
    }
    let mut node = Node::new(Role::Group);
    node.set_label(group.name);
    if let Some(b) = stats.bounds {
        node.set_bounds(b);
    }
    node.set_children(children);
    nodes.push((group_id, node));
    group_id
}

/// Emit the open lens; return its group's id and the node that takes focus
/// when it takes the keys (its first well, else the group).
fn push_stats_lens(nodes: &mut Vec<(NodeId, Node)>, lens: &StatsLens) -> (NodeId, NodeId) {
    let group_id = NodeId(STATS_LENS);
    let mut children = Vec::with_capacity(lens.wells.len());
    for (k, (name, words)) in lens.wells.iter().take(15).enumerate() {
        let id = NodeId(STATS_LENS + 1 + k as u64);
        let mut well = Node::new(Role::Group);
        well.set_label(name.clone());
        well.set_description(words.clone());
        if lens.keys && k == 0 {
            well.add_action(Action::Focus);
        }
        nodes.push((id, well));
        children.push(id);
    }
    let keys = if lens.keys {
        "Left and Right arrows choose the instrument, Up and Down the scope, Command-C \
         copies its sentence, Escape closes"
    } else {
        "Click to pin it; Escape or a click outside closes it"
    };
    let mut group = Node::new(Role::Group);
    group.set_label(lens.name.clone());
    group.set_description(format!("{} {keys}", lens.explainer));
    if let Some(b) = lens.bounds {
        group.set_bounds(b);
    }
    let focus = children.first().copied().unwrap_or(group_id);
    group.set_children(children);
    nodes.push((group_id, group));
    (group_id, focus)
}

/// Everything a frame publishes besides its plain visible grid: the tab strip, split
/// panes, painted selection, find field, and chrome messages. `Default` describes an
/// ordinary single-pane frame with nothing selected and no panel open.
#[derive(Debug, Clone, Default)]
pub(crate) struct GridFrame<'a> {
    /// The in-grid tab strip, empty when it is not a switcher on screen.
    pub(crate) tabs: &'a [GridTab],
    /// One entry per visible pane; empty for a single-pane (or zoomed) frame, which is
    /// published as one whole-screen document.
    pub(crate) panes: &'a [GridPane],
    /// The whole-screen document's painted selection. Only consulted when `panes` is
    /// empty; a split frame carries its selection per pane.
    pub(crate) selection: Option<GridSpan>,
    /// The find panel, live only while find mode is up.
    pub(crate) find: Option<&'a GridFind>,
    /// The chrome messages on screen right now, empty on an ordinary frame.
    pub(crate) messages: &'a [GridMessage],
    /// The harness's chips drawn in the frame's Claude Code composer rules, in painted
    /// order; empty where none is drawn.
    pub(crate) chips: &'a [GridChip],
    /// The open Autopilot menu, while one is open.
    pub(crate) menu: Option<&'a GridMenu>,
    /// The stats bars, while the window shows them (the Deck or the sill).
    pub(crate) stats: Option<&'a GridStats>,
    /// The window's canonical title (`WindowState::current_title`), not necessarily the
    /// caption bar's string: `apply_title` suppresses the chrome write while the close
    /// warning or find status owns it.
    ///
    /// It becomes the root's name, which AT-SPI, UIA and NSAccessibility read as the
    /// window's, so several windows can be told apart by ear. Empty falls back to the app
    /// name.
    pub(crate) window_title: &'a str,
}

/// The node id for tab `index`, and its inverse.
fn tab_node_id(index: usize) -> NodeId {
    NodeId(TAB_BASE + index as u64)
}

/// Decode the slot of a tab node. Action delivery must also validate its
/// publication's stable owner with [`tab_action_index`].
pub(crate) fn tab_index_for(node: NodeId) -> Option<usize> {
    node.0
        .checked_sub(TAB_BASE)
        .filter(|offset| *offset < (ROW_BASE - TAB_BASE))
        .map(|offset| offset as usize)
}

/// The request still addresses the stable tab that occupied its published slot.
/// The caller supplies the live tab id; the queued context is never restamped.
pub(crate) fn tab_action_index(
    request: &ActionRequest,
    id_at: impl FnOnce(usize) -> Option<u64>,
) -> Option<usize> {
    if request.target_tree != TreeId::ROOT
        || !matches!(request.action, Action::Click | Action::Focus)
    {
        return None;
    }
    let index = tab_index_for(request.target_node)?;
    let expected = ActionContext::tab(id_at(index)?);
    (request.context == Some(expected)).then_some(index)
}

/// Word starts within one line, in character indices, as the model defines them: index 0
/// always starts a word, and every non-whitespace character preceded by whitespace starts
/// the next (trailing whitespace stays with its word; leading whitespace is its own word).
/// This backs `GetStringAtOffset(.., WORD_START)`.
///
/// The consumer addresses this `[u8]` list by casting the query offset to `u8`, so a run
/// is capped at [`RUN_CHARS`] characters ([`row_chunks`]); within a run the indices are
/// exact, and the `break` below only guards that invariant.
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

/// The node id of run `chunk` of row `row` in the terminal document in id `slot`. Slot 0
/// is the whole-screen document.
fn row_run_id(slot: usize, row: usize, chunk: usize) -> NodeId {
    NodeId(ROW_BASE + slot as u64 * PANE_STRIDE + row as u64 * RUNS_PER_ROW + chunk as u64)
}

/// A [`TextPosition`] naming column `col` of row `row` inside one terminal document.
///
/// The column is clamped to the text the row publishes (trailing blanks trimmed), so a
/// position in empty cells lands on the row's hard line break, not past the node's end:
/// the model's rule. Native publishers convert this same model position to their
/// platform offset units. The position names the run it
/// sits in, since a row wider than [`RUN_CHARS`] is several runs ([`row_chunks`]).
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

/// Emit one terminal document (a read-only `Role::Terminal` node plus one `Role::TextRun`
/// child per visible row) and return its node id.
///
/// The runs make the platform publish an AT-SPI `Text` interface
/// (aterm-a11y's text documents require `Role::Terminal` plus a
/// run), so a screen reader can read the screen by line, word and character.
///
/// `selection` outranks the cursor, as in any text control: the highlight is the
/// selection and its far end the caret. With nothing selected the caret is a degenerate
/// selection; with the cursor hidden (DECTCEM) and nothing selected, no selection is
/// published (AT-SPI reports caret offset `-1`).
///
/// `origin` is the document's top-left cell in the visible grid, so a split pane's
/// rectangle is the one it was painted into.
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
    // One text run per visible row. `split_inclusive` keeps each row's '\n' in its run,
    // which is the model's hard line break: one character, with an end-of-line caret on it.
    let lines: Vec<&str> = snap.value().split_inclusive('\n').collect();
    let mut row_ids: Vec<NodeId> = Vec::with_capacity(lines.len());
    let (row_off, col_off) = origin;

    for (row, line) in lines.iter().enumerate() {
        // One run per row unless the row exceeds u8 addressing: the consumer looks up
        // word starts with `binary_search(&(pos.character_index as u8))`, so on a row
        // over 256 characters a query wraps and answers about a different word. A wide
        // row is therefore split into consecutive runs, each with its own word starts.
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
                // One character per cell (`push_visible_row` emits one per rendered cell,
                // wide-glyph continuations included), so the character index is the
                // column. The hard line break is zero-width at the end of the row.
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
    // For platforms whose "value" is a plain string (UIA, NSAccessibility); AT-SPI's Value
    // interface is numeric and ignores it, reading the text runs through Text instead.
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
/// caret, and the panel's answer as the accessible description. Without it the painted
/// panel is anonymous grid text and the reader's caret stays on the shell prompt.
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
    // The caret in the query, capped at its length: one line, one run, so the character
    // index is the caret index.
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

/// Emit one chrome message as a live region and return its node id.
///
/// The announced string goes in both the name and the value: AT-SPI (`aterm_a11y::atspi`)
/// announces the name (on change), UIA (as accesskit_windows did) raises
/// `LiveRegionChanged` on the name change and reads the name, and NSAccessibility (as
/// accesskit_macos did) announces the value and raises nothing for a node without one. `crate::native_accessibility::announce_live` follows
/// the same rule.
///
/// Bounds are set only for a message with a rectangle (see [`GridMessage::bar_row`]); a
/// message spliced over grid rows is told, not placed.
///
/// The message never takes focus, even when assertive: the keyboard is still in the
/// terminal, and the paste confirmation is answered by Enter/Escape reaching the grid.
fn push_message(
    nodes: &mut Vec<(NodeId, Node)>,
    message: &GridMessage,
    geom: Option<GridGeometry>,
    cols: usize,
) -> NodeId {
    let id = message.message.node_id();
    let metered = message.progress.is_some() || message.busy;
    let mut node = Node::new(message.message.role(metered, message.alarm));
    node.set_label(message.text.clone());
    node.set_value(message.text.clone());
    node.set_live(message.message.politeness());
    if let Some(context) = message.context {
        node.set_action_context(context);
    }
    if let Some(detail) = &message.detail {
        node.set_description(detail.clone());
    }
    let row_rect = message.bar_row.and_then(|row| {
        let g = geom?;
        (g.bar_h > 0.0).then(|| {
            let y0 = g.bars_y + row as f64 * g.bar_h;
            (g, y0)
        })
    });
    if let Some((g, y0)) = row_rect {
        node.set_bounds(Rect {
            x0: g.origin_x,
            y0,
            x1: g.origin_x + cols as f64 * g.cell_w,
            y1: y0 + g.bar_h,
        });
    }
    // Each capsule is its own button, named by the full label (the painted form may be
    // abbreviated), bounded by its cells within the row, and clickable.
    if let Some(band_row) = message.message.band_row_index() {
        let mut children = Vec::with_capacity(message.capsules.len());
        for cap in &message.capsules {
            let kind = ChromeMessage::BandCapsule {
                row: band_row as u8,
                index: cap.index,
            };
            let mut button = Node::new(Role::Button);
            button.set_label(cap.label.to_string());
            if let Some((g, y0)) = row_rect {
                button.set_bounds(Rect {
                    x0: g.origin_x + cap.col as f64 * g.cell_w,
                    y0,
                    x1: g.origin_x + (cap.col + cap.width) as f64 * g.cell_w,
                    y1: y0 + g.bar_h,
                });
            }
            if let Some(context) = message.context {
                button.set_action_context(context);
                button.add_action(Action::Click);
            }
            let child = kind.node_id();
            nodes.push((child, button));
            children.push(child);
        }
        if !children.is_empty() {
            node.set_children(children);
        }
    }
    // A determinate meter as a numeric value, so AT-SPI publishes a `Value` interface on
    // the progress bar. No fill leaves the value unset: the model's indeterminate bar.
    if let Some(fill) = message.progress {
        node.set_min_numeric_value(0.0);
        node.set_max_numeric_value(1.0);
        node.set_numeric_value(f64::from(fill.clamp(0.0, 1.0)));
    }
    if message.activates && message.context.is_some() {
        node.add_action(Action::Click);
    }
    nodes.push((id, node));
    id
}

/// The accessibility tree for a plain terminal session (no overlay open).
///
/// A `Role::Window` root parenting, in painted order: the tab strip (when on screen and
/// a switcher), the chrome messages being shown (live regions), the find field (while
/// find mode is live), and the terminal: one read-only `Role::Terminal` document, or one
/// per visible pane for a split.
///
/// aterm-a11y's tree diffs this tree against the previous one to synthesise
/// `object:text-changed:insert`/`:delete` and `object:text-caret-moved`, which is why
/// row ids ([`ROW_BASE`]) stay stable across frames.
pub(crate) fn grid_tree(
    snap: &crate::accessibility::AccessibleSnapshot,
    geom: Option<GridGeometry>,
    frame: &GridFrame<'_>,
) -> TreeUpdate {
    let rows = if frame.panes.is_empty() {
        snap.value().split_inclusive('\n').count()
    } else {
        snap.rows
    };
    let mut nodes: Vec<(NodeId, Node)> =
        Vec::with_capacity(rows + frame.tabs.len() + frame.panes.len() + frame.messages.len() + 5);
    let mut root_children: Vec<NodeId> =
        Vec::with_capacity(3 + frame.panes.len() + frame.messages.len());

    // The tab strip, when on screen and a switcher. Each tab is focusable and clickable;
    // the OS routes both back through `tab_index_for`.
    if !frame.tabs.is_empty() {
        let mut items: Vec<NodeId> = Vec::with_capacity(frame.tabs.len());
        for tab in frame.tabs {
            let id = tab_node_id(tab.index);
            let mut node = Node::new(Role::Tab);
            node.set_label(tab.title.clone());
            if let Some(theme) = &tab.theme {
                node.set_description(format!("theme {theme}"));
            }
            node.set_selected(tab.selected);
            node.set_action_context(ActionContext::tab(tab.id));
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

    // The stats deck, under the titlebar or in the bottom pad: a named group of meters
    // whose values change in place (no live region: a reader asks, nothing announces).
    let mut lens_focus = None;
    if let Some(stats) = frame.stats {
        root_children.push(push_stats(&mut nodes, stats));
        // The open lens over the grid: a lens that takes the keys holds them,
        // so a reader lands in its focus well.
        if let Some(lens) = &stats.lens {
            let (id, focus) = push_stats_lens(&mut nodes, lens);
            root_children.push(id);
            lens_focus = lens.keys.then_some(focus);
        }
    }

    // The chrome messages, in painted order, between the strip and the grid, before the
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

    // The harness's chips (glass the grid's text never holds), then the menu over the
    // pane: both before the terminal, as the bands are, so a reader stepping into the
    // window meets them.
    root_children.extend(push_chips(&mut nodes, frame.chips, geom));
    let menu = frame.menu.map(|menu| push_menu(&mut nodes, menu, geom));
    if let Some((id, _)) = menu {
        root_children.push(id);
    }

    // One document per visible pane, or the whole screen as one. A pane's slot (never its
    // list position) picks its row-id block.
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

    // Focus follows the keyboard: the find field while the panel owns the keys, else the
    // focused pane. The fallback is the last child: the single grid, or the last pane of
    // a split whose focus the layout could not name.
    let focus = match (menu, frame.find.is_some(), focused_pane) {
        // The open menu holds the keys: its highlighted row, else the menu.
        (Some((id, row)), _, _) => row.unwrap_or(id),
        (None, true, _) => FIND,
        // The lens that takes the keys holds the arrows: its focus well.
        (None, false, _) if lens_focus.is_some() => lens_focus.unwrap_or(GRID),
        (None, false, Some(pane)) => pane,
        (None, false, None) => root_children.last().copied().unwrap_or(GRID),
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

    /// A real engine's visible grid.
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

    /// Band rows are published in painted order between the strip and the grid, each
    /// with the band rectangle it was drawn into; a surface with no fixed place carries
    /// none, and each capsule is a button child bounded by its own cells.
    #[test]
    fn a_status_band_is_published_where_it_is_painted() {
        let snap = live_grid(2, 20, b"hi", Some((0, 2)));
        let bars = vec![
            GridMessage {
                bar_row: Some(0),
                capsules: vec![BandCapsule {
                    label: "Packages",
                    col: 10,
                    width: 6,
                    index: 0,
                }],
                ..message(
                    ChromeMessage::BandRow0,
                    "Installing ALab tools \u{00b7} extracting",
                )
            },
            GridMessage {
                bar_row: Some(1),
                ..message(
                    ChromeMessage::BandRow1,
                    "Updating to aterm v0.62.0 \u{00b7} downloading",
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
        // The capsule: a button child of its row, named in full, bounded by its cells.
        let capsule = ChromeMessage::BandCapsule { row: 0, index: 0 };
        let button = node(&update, capsule.node_id());
        assert_eq!(button.role(), Role::Button);
        assert_eq!(button.label(), Some("Packages"));
        assert!(button.supports_action(Action::Click));
        assert_eq!(button.action_context(), bars[0].context);
        assert_eq!(
            node(&update, bars[0].message.node_id()).action_context(),
            bars[0].context
        );
        let mut unbound = bars.clone();
        unbound[0].context = None;
        let unbound = grid_tree(&snap, Some(GEOM), &frame(&unbound));
        assert!(!node(&unbound, capsule.node_id()).supports_action(Action::Click));
        let b = button.bounds().expect("a capsule has its cells");
        assert_eq!(b.x0, GEOM.origin_x + 10.0 * GEOM.cell_w);
        assert_eq!(b.x1, GEOM.origin_x + 16.0 * GEOM.cell_w);
        assert_eq!(b.y0, GEOM.bars_y);
        assert_eq!(
            node(&update, bars[0].message.node_id()).children(),
            [capsule.node_id()]
        );
        assert_eq!(ChromeMessage::from_node(capsule.node_id()), Some(capsule));
        // A surface with no reserved row publishes no rectangle rather than a wrong one
        // (the paste question overwrites the band's rows in place: `bar_row: None`).
        let floating = grid_tree(
            &snap,
            Some(GEOM),
            &frame(&[message(ChromeMessage::PasteConfirm, "Paste 3 lines?")]),
        );
        assert_eq!(
            node(&floating, ChromeMessage::PasteConfirm.node_id()).bounds(),
            None
        );

        // Painted order, and the grid still comes last so a reader meets the
        // chrome before the content it sits above.
        let children = node(&update, ROOT).children();
        let bar_ids: Vec<NodeId> = bars.iter().map(|b| b.message.node_id()).collect();
        assert_eq!(&children[..bars.len()], &bar_ids[..]);
        assert_eq!(children.last().copied(), Some(GRID));

        // …and with no row up the tree is byte-identical to the no-band path.
        let without = grid_tree(&snap, Some(GEOM), &GridFrame::default());
        assert_eq!(
            without.nodes.len() + bars.len() + 1,
            update.nodes.len(),
            "a hidden row publishes no node at all (the capsule is the +1)"
        );
    }

    /// The grid is a read-only `Role::Terminal` under a `Role::Window` root, carrying the
    /// visible text.
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

    /// The grid is a real text document (`Role::Terminal` plus `Role::TextRun` children,
    /// which `supports_text_ranges()` requires): one run per row, each carrying its hard
    /// line break, concatenating back to the snapshot text.
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

    /// The caret is a degenerate selection on the cursor's row run at the cursor's column,
    /// with the same model position consumed by every native publisher.
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
    }

    /// A caret in a row's trailing blanks clamps onto that row's hard line break
    /// (the model's rule).
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
    }

    /// A hidden cursor (DECTCEM) publishes no selection; AT-SPI then reports caret
    /// offset -1.
    #[test]
    fn hidden_cursor_publishes_no_caret() {
        let snap = live_grid(2, 10, b"hello", None);
        let update = grid_tree(&snap, None, &GridFrame::default());
        assert_eq!(node(&update, GRID).text_selection(), None);
    }

    /// A row wider than the `[u8]` word-index space is published as several runs, each
    /// inside the addressable range, so a query at column 281 of a 300-column row does
    /// not wrap to a different word.
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

    /// Per-character geometry: one cell per character, so the character index is the
    /// column, and the hard line break is zero-width at the end of the row
    /// (`GetCharacterExtents`, braille cursor routing).
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

    /// Word starts follow the model's definition: index 0 always starts a word, leading
    /// whitespace is its own word, and trailing whitespace belongs to the word before it.
    #[test]
    fn word_starts_follow_the_model_definition() {
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
    /// items with the strip's labels and selection, bounds from the segment spans the
    /// mouse hit-tests, and ids that decode back to the index `switch_tab_in` takes.
    #[test]
    fn tab_strip_publishes_as_a_tab_list() {
        let snap = live_grid(2, 40, b"hi", Some((0, 2)));
        let tabs = vec![
            GridTab {
                index: 0,
                id: (1 << 60) + 11,
                title: "build".into(),
                selected: false,
                start_col: 0,
                end_col: 10,
                theme: Some("Hearth".into()),
            },
            GridTab {
                index: 1,
                id: (1 << 60) + 12,
                title: "logs".into(),
                selected: true,
                start_col: 10,
                end_col: 20,
                theme: None,
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
        // A tab drawn in its own theme says which (spec `session-theme-draw`);
        // one in the window's theme says nothing.
        assert_eq!(
            node(&update, tab_node_id(0)).description(),
            Some("theme Hearth")
        );
        assert_eq!(logs.description(), None);
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
        // The producer's exact stable identity travels with the queued request.
        let mut request = ActionRequest {
            context: logs.action_context(),
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: tab_node_id(1),
            data: None,
        };
        let lookup = |index: usize| tabs.get(index).map(|tab| tab.id);
        assert_eq!(tab_action_index(&request, lookup), Some(1));
        request.action = Action::Focus;
        assert_eq!(tab_action_index(&request, lookup), Some(1));
        assert_eq!(request.context, Some(ActionContext::tab(tabs[1].id)));
        // A same-count reorder keeps the slot but changes its actual owner.
        let mut reordered = tabs.clone();
        reordered.swap(0, 1);
        assert_eq!(
            tab_action_index(&request, |index| reordered.get(index).map(|tab| tab.id)),
            None
        );
        assert_eq!(tab_action_index(&request, |_| None), None, "closed tab");
        for context in [
            None,
            Some(ActionContext::new(tabs[1].id, 0)),
            Some(ActionContext::message(tabs[1].id, 0)),
        ] {
            request.context = context;
            assert_eq!(
                tab_action_index(&request, lookup),
                None,
                "unbound or wrong domain"
            );
        }
    }

    /// A painted selection is published as the terminal's text selection and outranks
    /// the caret, as in any text control (AT-SPI `getNSelections()` is non-zero over a
    /// live highlight).
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

    /// A composed split publishes one terminal document per pane, not one whose rows read
    /// `<left pane row>▐<right pane row>`.
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

        // Each pane owns a disjoint block of row ids, so the consumer's diff never
        // mistakes a sibling's rows for this pane's. Slot 0 stays the whole-screen
        // document's, so a split never reuses the unsplit frame's ids.
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

    /// The find panel publishes a `Role::SearchInput` carrying the live query, a caret
    /// inside it and the panel's answer, and takes focus, since the keyboard belongs to
    /// the query while the panel is up.
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
        // A run child makes the field a navigable text document
        // (`supports_text_ranges` needs an input role plus a run).
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

    /// The stats deck is one group of meters in row order, then the cores'
    /// group (Stats Deck v2 §12): a meter's value is its % on a 0–100 range,
    /// an unknown one has no value rather than a zero, and the keyboard
    /// lens's first well takes the focus.
    #[test]
    fn the_stats_deck_is_a_group_of_meters_and_a_group_of_cores() {
        let snap = live_grid(2, 20, b"", None);
        let meter = |name: &str, value: Option<f32>| StatsMeter {
            name: name.into(),
            value,
            words: format!("{name} said"),
        };
        let stats = GridStats {
            group: StatsGroup {
                name: "Stats deck, last 10 minutes",
                meters: vec![meter("CPU", Some(98.0)), meter("Tokens", None)],
                cores_name: "2 CPU cores".into(),
                cores: vec![
                    meter("Super core 1", Some(54.0)),
                    meter("Super core 2", None),
                ],
            },
            bounds: None,
            lens: Some(StatsLens {
                keys: true,
                name: "CPU close-up".into(),
                explainer: "Heat is the share of the cores in use.".into(),
                wells: vec![("Now".into(), "CPU 98 percent of capacity.".into())],
                bounds: None,
            }),
        };
        let frame = GridFrame {
            stats: Some(&stats),
            ..GridFrame::default()
        };
        let update = grid_tree(&snap, Some(GEOM), &frame);
        let group = node(&update, NodeId(STATS_BASE));
        assert_eq!(group.role(), Role::Group);
        assert_eq!(group.label(), Some("Stats deck, last 10 minutes"));
        let kids = group.children();
        assert_eq!(kids.len(), 3, "two meters and the cores");
        let cpu = node(&update, kids[0]);
        assert_eq!(
            (cpu.role(), cpu.label(), cpu.numeric_value()),
            (Role::Meter, Some("CPU"), Some(98.0))
        );
        assert_eq!(
            (cpu.min_numeric_value(), cpu.max_numeric_value()),
            (Some(0.0), Some(100.0))
        );
        assert_eq!(cpu.description(), Some("CPU said"));
        let tokens = node(&update, kids[1]);
        assert_eq!(
            (tokens.label(), tokens.numeric_value()),
            (Some("Tokens"), None)
        );
        assert_eq!(tokens.description(), Some("Tokens said"));
        let cores = node(&update, kids[2]);
        assert_eq!(
            (cores.role(), cores.label()),
            (Role::Group, Some("2 CPU cores"))
        );
        let cells: Vec<_> = cores
            .children()
            .iter()
            .map(|&id| {
                let n = node(&update, id);
                (n.role(), n.label(), n.numeric_value())
            })
            .collect();
        assert_eq!(
            cells,
            [
                (Role::Meter, Some("Super core 1"), Some(54.0)),
                (Role::Meter, Some("Super core 2"), None)
            ]
        );
        let lens = node(&update, NodeId(STATS_LENS));
        assert_eq!(lens.label(), Some("CPU close-up"));
        assert_eq!(update.focus, lens.children()[0], "the lens's first well");
        assert_eq!(
            node(&update, update.focus).label(),
            Some("Now"),
            "the well is named by its heading"
        );
    }

    fn message(message: ChromeMessage, text: &str) -> GridMessage {
        GridMessage {
            message,
            context: match message {
                ChromeMessage::BandRow0 | ChromeMessage::BandRow1 | ChromeMessage::BandRow2 => {
                    Some(ActionContext::message(7, 0))
                }
                ChromeMessage::BandOverflow => Some(ActionContext::message_log()),
                _ => None,
            },
            text: text.to_string(),
            detail: None,
            progress: None,
            busy: false,
            alarm: false,
            activates: false,
            bar_row: None,
            capsules: Vec::new(),
        }
    }

    /// A live region carries its sentence in both name and value (see [`push_message`]).
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

    /// No flooding: volatile figures go in the description and the meter in the numeric
    /// value, so only a change of the spoken sentence announces.
    #[test]
    fn a_status_band_keeps_its_volatile_figures_out_of_the_announcement() {
        let snap = live_grid(2, 20, b"hi", None);
        let at = |done: &str, fill: f32| GridMessage {
            message: ChromeMessage::BandRow0,
            context: Some(ActionContext::message(7, 0)),
            text: "Updating to aterm v0.48.0 \u{00b7} downloading".to_string(),
            detail: Some(format!("{done} / 1.2 GB")),
            progress: Some(fill),
            busy: false,
            alarm: false,
            activates: true,
            bar_row: Some(0),
            capsules: Vec::new(),
        };
        let id = NodeId(MESSAGE_BASE + ChromeMessage::BandRow0.slot());
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

        // No meter and no work in flight: a plain status band (AT-SPI StatusBar), left
        // numerically unset. (A busy row is a value-less `ProgressIndicator`, see
        // `an_indeterminate_row_is_a_busy_progress_indicator`; its spinner is paint only.)
        let flat = tree(&[message(
            ChromeMessage::BandRow1,
            "Installing ALab tools \u{00b7} starting\u{2026}",
        )]);
        let flat = node(&flat, NodeId(MESSAGE_BASE + ChromeMessage::BandRow1.slot()));
        assert_eq!(flat.role(), Role::Status);
        assert_eq!(flat.numeric_value(), None);
        // A warning row is an Alert, meter or not.
        let warn = tree(&[GridMessage {
            alarm: true,
            ..message(ChromeMessage::BandRow2, "Couldn't install aterm v0.48.0")
        }]);
        assert_eq!(
            node(&warn, NodeId(MESSAGE_BASE + ChromeMessage::BandRow2.slot())).role(),
            Role::Alert
        );
    }

    /// An indeterminate row (work in flight with no fraction, the comet) is a
    /// `ProgressIndicator` with no numeric value, the model's indeterminate progress bar;
    /// a warning is still an `Alert`, busy or not.
    #[test]
    fn an_indeterminate_row_is_a_busy_progress_indicator() {
        let snap = live_grid(2, 20, b"hi", None);
        let id = NodeId(MESSAGE_BASE + ChromeMessage::BandRow0.slot());
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
        let busy = tree(&[GridMessage {
            busy: true,
            ..message(ChromeMessage::BandRow0, "Installing ALab toolchain")
        }]);
        let busy = node(&busy, id);
        assert_eq!(busy.role(), Role::ProgressIndicator);
        assert_eq!(busy.numeric_value(), None, "indeterminate: no value");
        let alarm = tree(&[GridMessage {
            busy: true,
            alarm: true,
            ..message(ChromeMessage::BandRow0, "Update did not apply")
        }]);
        assert_eq!(node(&alarm, id).role(), Role::Alert);
    }

    /// The multi-line-paste guard is a question: a real AT-SPI `Alert`, assertive, that
    /// does not take focus (Enter and Escape answer it by reaching the grid).
    #[test]
    fn the_paste_confirmation_interrupts_without_stealing_the_keyboard() {
        let snap = live_grid(2, 20, b"hi", Some((0, 2)));
        let asked = crate::paste_banner::question("one\ntwo\nthree\nfour\nfive\nsix\nseven");
        let messages = vec![GridMessage {
            detail: Some(crate::paste_banner::announcement_detail()),
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
        assert_eq!(ask.label(), Some(asked.as_str()));
        assert_eq!(
            ask.description(),
            Some(crate::paste_banner::announcement_detail().as_str())
        );
        assert!(
            !ask.supports_action(Action::Click),
            "a generic activate names neither answer"
        );
        assert_eq!(update.focus, GRID, "Enter still reaches the terminal");
    }

    /// A message id round-trips to its own surface, is stable across frames (so a name
    /// change diffs as an announcement), and is refused by the tab decoder.
    #[test]
    fn message_ids_round_trip_are_stable_and_are_never_read_as_a_tab() {
        let snap = live_grid(2, 20, b"hi", None);
        for kind in ChromeMessage::ORDER {
            let id = NodeId(MESSAGE_BASE + kind.slot());
            assert_eq!(ChromeMessage::from_node(id), Some(kind));
            assert_eq!(tab_index_for(id), None, "{kind:?} is not a tab");
        }
        // Slots 0 and 2 are unused: no row is published there and no activate decodes
        // to them.
        for (retired, what) in [
            (0, "retired Notice slot"),
            (2, "retired ConfigWarning slot"),
        ] {
            assert_eq!(
                ChromeMessage::from_node(NodeId(MESSAGE_BASE + retired)),
                None,
                "{what}"
            );
            assert!(
                ChromeMessage::ORDER.iter().all(|m| m.slot() != retired),
                "no row is published at the {what}"
            );
        }
        // The capsule slots decode by arithmetic: three per row, three rows; the
        // spare slot in each group and anything past the band decode to nothing.
        for row in 0..3u8 {
            for index in 0..=CAPSULE_DETAILS {
                let kind = ChromeMessage::BandCapsule { row, index };
                assert_eq!(ChromeMessage::from_node(kind.node_id()), Some(kind));
                assert_eq!(tab_index_for(kind.node_id()), None);
            }
            assert_eq!(
                ChromeMessage::from_node(NodeId(
                    MESSAGE_BASE + CAPSULE_BASE + u64::from(row) * CAPSULES_PER_ROW + 3
                )),
                None,
                "the spare capsule slot is unassigned"
            );
        }
        assert_eq!(
            ChromeMessage::from_node(NodeId(MESSAGE_BASE + CAPSULE_BASE + 3 * CAPSULES_PER_ROW)),
            None,
            "no fourth band row"
        );
        assert_eq!(
            capsule_action(CAPSULE_DETAILS),
            aterm_messages::ActionIndex::DETAILS
        );
        assert_eq!(capsule_index(aterm_messages::ActionIndex(1)), 1);
        assert_eq!(
            ChromeMessage::from_node(NodeId(MESSAGE_BASE + ChromeMessage::PresenceStatus.slot())),
            Some(ChromeMessage::PresenceStatus),
            "the band's slot round-trips"
        );
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
                messages: &[message(ChromeMessage::BandRow0, "one")],
                ..GridFrame::default()
            },
        );
        let second = grid_tree(
            &snap,
            None,
            &GridFrame {
                messages: &[message(ChromeMessage::BandRow0, "two")],
                ..GridFrame::default()
            },
        );
        let id_of = |u: &TreeUpdate| {
            u.nodes
                .iter()
                .find(|(_, n)| n.role() == Role::Status)
                .map(|(id, _)| *id)
                .unwrap()
        };
        assert_eq!(id_of(&first), id_of(&second), "one node, new words");
    }

    /// The accessible window is named what the titlebar names it (assistive clients read
    /// the root's name as the window's); an uncomposed title falls back to the app name.
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

    /// Whether `text` is words a screen reader says: letters, digits, spaces and `.:,`
    /// alone (design §9.2.5).
    fn spoken(text: &str) -> bool {
        text.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | ':' | ','))
    }

    /// THE HARNESS'S CHIPS ARE BUTTONS IN WORDS (design §9.2.1): each drawn chip
    /// (Autopilot and the Team chip) is one clickable `Role::Button` over its cells,
    /// named with no symbol, the newest act its description and never a live region;
    /// their ids decode back to the chip and are refused by the tab and message decoders. NEGATIVE CONTROL: the words check
    /// refuses a glyph (`∞ cruise`), and a frame with no chip publishes no chip node.
    #[test]
    fn the_harness_chips_are_buttons_named_in_words() {
        let snap = live_grid(3, 40, b"hi", Some((0, 2)));
        let chips = [
            GridChip {
                session: 4,
                chip: HarnessChip::Autopilot,
                label: "Autopilot: cruise. Lands at 18:00.".to_string(),
                description: Some("Last: approved at 14:05.".to_string()),
                toggled: None,
                row: 2,
                col: 4,
                width: 1,
            },
            GridChip {
                session: 4,
                chip: HarnessChip::Team,
                label: "Worker of fmt".to_string(),
                description: Some("Goes to its manager.".to_string()),
                toggled: None,
                row: 2,
                col: 8,
                width: 1,
            },
        ];
        let update = grid_tree(
            &snap,
            Some(GEOM),
            &GridFrame {
                chips: &chips,
                ..GridFrame::default()
            },
        );
        let ids: Vec<NodeId> = chips
            .iter()
            .map(|c| chip_node_id(c.session, c.chip).expect("an id"))
            .collect();
        assert_eq!(node(&update, ROOT).children(), [ids[0], ids[1], GRID]);
        for (chip, id) in chips.iter().zip(&ids) {
            let n = node(&update, *id);
            assert_eq!(n.role(), Role::Button);
            assert!(n.supports_action(Action::Click), "clickable");
            assert_eq!(n.label(), Some(chip.label.as_str()));
            assert!(spoken(n.label().unwrap()), "{:?}", n.label());
            assert_eq!(n.description(), chip.description.as_deref());
            assert_eq!(n.live(), None, "never a live region");
            assert_eq!(
                n.bounds(),
                Some(Rect {
                    x0: GEOM.origin_x + chip.col as f64 * GEOM.cell_w,
                    y0: GEOM.origin_y + 2.0 * GEOM.cell_h,
                    x1: GEOM.origin_x + (chip.col + 1) as f64 * GEOM.cell_w,
                    y1: GEOM.origin_y + 3.0 * GEOM.cell_h,
                })
            );
            assert_eq!(chip_for(*id), Some((chip.session, chip.chip)));
            assert_eq!(tab_index_for(*id), None);
            assert_eq!(ChromeMessage::from_node(*id), None);
            // Autopilot's menu is announced and reachable without a pointer (design
            // §9.3.3); Team has none (NEGATIVE CONTROL).
            let menu = chip.chip == HarnessChip::Autopilot;
            assert_eq!(
                n.has_popup(),
                menu.then_some(aterm_a11y::model::HasPopup::Menu),
                "{:?}",
                chip.chip
            );
            assert_eq!(
                n.supports_action(Action::ShowContextMenu),
                menu,
                "{:?}",
                chip.chip
            );
        }
        assert!(spoken(chips[0].description.as_deref().unwrap()));
        assert_eq!(chip_for(GRID), None);
        assert_eq!(chip_for(tab_node_id(3)), None);
        // NEGATIVE CONTROLS.
        assert!(!spoken("\u{221e} cruise"));
        let bare = grid_tree(&snap, Some(GEOM), &GridFrame::default());
        assert!(bare.nodes.iter().all(|(id, _)| chip_for(*id).is_none()));
    }

    /// THE LIGHT, THE SWITCH AND THE TITLE SLOT HAVE IDS OF THEIR OWN (review G1):
    /// every target round-trips through its id, two sessions' ids never meet, and the
    /// title slot is a `Role::Status` named by its words with no action and no live
    /// politeness, where the light and the switch's `on` are clickable buttons. NEGATIVE
    /// CONTROL: a free slot of a session's ids decodes as nothing, never as a target.
    #[test]
    fn the_light_the_switch_and_the_title_slot_have_ids_of_their_own() {
        let all = [
            HarnessChip::Autopilot,
            HarnessChip::Team,
            HarnessChip::TopEffort,
            HarnessChip::Model,
            HarnessChip::Effort,
            HarnessChip::Path,
            HarnessChip::Branch,
            HarnessChip::Usage,
            HarnessChip::Mode,
            HarnessChip::Switch,
            HarnessChip::Title,
            HarnessChip::Context,
            HarnessChip::SwitchOn,
        ];
        let mut ids = std::collections::HashSet::new();
        for session in [0, 1, 7] {
            for chip in all {
                let id = chip_node_id(session, chip).expect("an id");
                assert_eq!(chip_for(id), Some((session, chip)));
                assert!(ids.insert(id), "{session} {chip:?}: one id each");
            }
        }
        for free in u64::try_from(all.len()).unwrap()..HarnessChip::SLOTS {
            assert_eq!(
                chip_for(NodeId(CHIP_BASE + HarnessChip::SLOTS + free)),
                None,
                "slot {free}: free"
            );
        }
        let snap = live_grid(3, 40, b"hi", Some((0, 2)));
        let at = |chip, label: &str, col| GridChip {
            session: 2,
            chip,
            label: label.to_string(),
            description: None,
            row: 2,
            col,
            width: 4,
            toggled: None,
        };
        let chips = [
            at(HarnessChip::Mode, "Permission mode: plan.", 2),
            at(
                HarnessChip::SwitchOn,
                "on: turn the Claude Code harness on",
                20,
            ),
            at(
                HarnessChip::Title,
                "Permission mode: click for bypass or auto",
                8,
            ),
        ];
        let update = grid_tree(
            &snap,
            Some(GEOM),
            &GridFrame {
                chips: &chips,
                ..GridFrame::default()
            },
        );
        for chip in &chips {
            let n = node(&update, chip_node_id(2, chip.chip).expect("an id"));
            let title = chip.chip == HarnessChip::Title;
            assert_eq!(n.role(), if title { Role::Status } else { Role::Button });
            assert_eq!(n.supports_action(Action::Click), !title, "{:?}", chip.chip);
            assert!(
                !n.supports_action(Action::ShowContextMenu),
                "{:?}",
                chip.chip
            );
            assert_eq!(n.label(), Some(chip.label.as_str()));
            assert_eq!(n.live(), None, "never a live region");
        }
    }

    /// THE OPEN AUTOPILOT MENU IS A MENU (design §9.3.3): a `Role::Menu` named by its
    /// header with one item per row in words — the mode rows radios with their checked
    /// state, an action row a plain item — a disabled row marked disabled and offered no
    /// click, and the tree's focus on the highlighted row. NEGATIVE CONTROL: with no
    /// menu, the focus is the grid's and no menu node is published.
    #[test]
    fn the_open_autopilot_menu_is_a_menu_of_items_in_words() {
        let snap = live_grid(12, 40, b"hi", Some((0, 2)));
        let item = |label: &str, checked, disabled, row| GridMenuItem {
            label: label.to_string(),
            checked,
            disabled,
            row,
        };
        let menu = GridMenu {
            label: "Autopilot, tab 3, fix the footer".to_string(),
            epoch: 0x2a_5a5a,
            items: vec![
                item("Cruise", Some(true), false, 2),
                item("Land now", Some(false), false, 3),
                item("Off", Some(false), false, 4),
                item("Cruise for 1 hour, then land", None, true, 6),
                item("Harness settings", None, false, 8),
            ],
            selected: Some(1),
            row: 1,
            col: 4,
            width: 30,
            height: 8,
        };
        let update = grid_tree(
            &snap,
            Some(GEOM),
            &GridFrame {
                menu: Some(&menu),
                ..GridFrame::default()
            },
        );
        let root = node(&update, ROOT);
        assert_eq!(root.children(), [NodeId(MENU_BASE), GRID]);
        let m = node(&update, NodeId(MENU_BASE));
        assert_eq!(m.role(), Role::Menu);
        assert_eq!(m.label(), Some(menu.label.as_str()));
        assert_eq!(m.children().len(), menu.items.len());
        for (i, (id, want)) in m.children().iter().zip(&menu.items).enumerate() {
            let n = node(&update, *id);
            assert_eq!(menu_item_for(*id), Some((menu.epoch, i)));
            assert!(id.0 > MENU_BASE && id.0 < CHIP_BASE, "{id:?}");
            assert_eq!(chip_for(*id), None);
            assert_eq!(ChromeMessage::from_node(*id), None);
            assert_eq!(tab_index_for(*id), None);
            assert_eq!(n.label(), Some(want.label.as_str()));
            assert!(spoken(&want.label), "{}", want.label);
            match want.checked {
                Some(checked) => {
                    assert_eq!(n.role(), Role::MenuItemRadio);
                    assert_eq!(
                        n.toggled(),
                        Some(if checked {
                            aterm_a11y::model::Toggled::True
                        } else {
                            aterm_a11y::model::Toggled::False
                        })
                    );
                }
                None => {
                    assert_eq!(n.role(), Role::MenuItem);
                    assert_eq!(n.toggled(), None);
                }
            }
            assert_eq!(n.is_disabled(), want.disabled, "{}", want.label);
            assert_eq!(n.supports_action(Action::Click), !want.disabled);
        }
        assert_eq!(update.focus, m.children()[1], "the highlighted row");
        assert_eq!(menu_item_for(NodeId(MENU_BASE)), None, "the menu is no row");
        // The same rows at another epoch are other ids: an item heard from one is never
        // read as the other's.
        let moved = grid_tree(
            &snap,
            Some(GEOM),
            &GridFrame {
                menu: Some(&GridMenu {
                    epoch: menu.epoch + 1,
                    ..menu.clone()
                }),
                ..GridFrame::default()
            },
        );
        let moved_items = node(&moved, NodeId(MENU_BASE)).children().to_vec();
        for (old, new) in m.children().iter().zip(&moved_items) {
            assert_ne!(old, new);
            assert_eq!(menu_item_for(*new).map(|(e, _)| e), Some(menu.epoch + 1));
        }
        // The id range's ends: the widest epoch's last row stays below the chips'.
        let last = menu_item_node_id(u32::MAX, (MENU_ROWS - 1) as usize).unwrap();
        assert!(last.0 < CHIP_BASE, "{last:?}");
        assert_eq!(
            menu_item_for(last),
            Some((menu_epoch_from(u64::MAX), (MENU_ROWS - 1) as usize))
        );
        assert_eq!(menu_item_node_id(0, MENU_ROWS as usize), None);
        assert_eq!(menu_item_for(NodeId(CHIP_BASE)), None);
        // NEGATIVE CONTROL: no menu.
        let bare = grid_tree(&snap, Some(GEOM), &GridFrame::default());
        assert_eq!(bare.focus, GRID);
        assert!(bare.nodes.iter().all(|(id, _)| id.0 != MENU_BASE));
    }
}
