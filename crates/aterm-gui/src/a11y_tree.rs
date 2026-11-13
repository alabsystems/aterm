// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure accessibility projection of the terminal window into an [`aterm_a11y::model::TreeUpdate`].
//!
//! Maps the visible grid, tab strip, find field, stats deck, harness chips, fact menus,
//! and chrome messages for `a11y_adapter`. Compiled under `a11y_tree` (`build.rs`).

use aterm_a11y::model::{
    Action, ActionContext, ActionRequest, Live, Node, NodeId, Rect, Role, TextDirection,
    TextPosition, TextSelection, Tree, TreeId, TreeUpdate,
};

const ROOT: NodeId = NodeId(0);
const GRID: NodeId = NodeId(1);
const TAB_LIST: NodeId = NodeId(2);
const FIND: NodeId = NodeId(3);
const FIND_RUN: NodeId = NodeId(4);
/// The focused rewound pane's slider ([`GridRewind`]).
pub(crate) const REWIND: NodeId = NodeId(5);
const RENAME: NodeId = NodeId(6);
const RENAME_RUN: NodeId = NodeId(7);
const MESSAGE_BASE: u64 = 1 << 20;
const MENU_BASE: u64 = 1 << 29;
const MENU_ROWS: u64 = 256;
const MENU_EPOCH_BITS: u32 = 20;
const CHIP_BASE: u64 = 1 << 30;
const STATS_BASE: u64 = 1 << 28;
const STATS_CORES: usize = 1 << 20;
const TAB_BASE: u64 = 1 << 40;
const ROW_BASE: u64 = 1 << 48;
const PANE_STRIDE: u64 = 1 << 32;
const PANE_BASE: u64 = 1 << 62;
const RUN_CHARS: usize = 256;
const RUNS_PER_ROW: u64 = 256;

fn row_chunks(line: &str) -> impl Iterator<Item = &str> {
    let mut remaining = Some(line);
    let mut emitted = 0;
    std::iter::from_fn(move || {
        let line = remaining.take()?;
        let end = if emitted < RUNS_PER_ROW - 1 {
            line.char_indices()
                .nth(RUN_CHARS)
                .map_or(line.len(), |(byte, _)| byte)
        } else {
            line.len()
        };
        let (chunk, tail) = line.split_at(end);
        if !tail.is_empty() {
            remaining = Some(tail);
        }
        emitted += 1;
        Some(chunk)
    })
}

/// A terminal's document as a client reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalDocument {
    /// Its rows and caret.
    pub(crate) snap: crate::accessibility::AccessibleSnapshot,
    /// The span of those rows that is selected.
    pub(crate) selection: Option<GridSpan>,
}

/// What a client must hear of a window with no frame to tell it. Compared whole, so a caret, a
/// scrolled view, a selection, a pane's focus, a step of Rewind, a menu that closed or a character
/// typed in a field counts as a printed row does.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct ClientReads {
    /// The leaves on screen, so a tab that reads the same as the last one still counts.
    pub(crate) views: Vec<crate::tab_model::ViewId>,
    /// A split's or mixed tab's terminals, one document each.
    pub(crate) panes: Vec<GridPane>,
    /// The one document of a tab with no panes.
    pub(crate) front: Option<TerminalDocument>,
    /// The focused pane's Rewind view.
    pub(crate) rewind: Option<GridRewind>,
    /// The paste or close question waiting on an answer.
    pub(crate) questions: Vec<GridMessage>,
    /// The open menu, whose rows a client presses.
    pub(crate) menu: Option<GridMenu>,
    /// The session field, which holds the keys while it is open.
    pub(crate) rename: Option<GridRename>,
    /// The find field.
    pub(crate) find: Option<GridFind>,
}

/// The span of `snap`, read from `cells`, that is selected, kept to the text of each row.
pub(crate) fn selected_span(
    cells: &aterm_core::terminal::TextFrame,
    snap: &crate::accessibility::AccessibleSnapshot,
) -> Option<GridSpan> {
    if !cells.any_selection() {
        return None;
    }
    let mut anchor: Option<(usize, usize)> = None;
    let mut focus: Option<(usize, usize)> = None;
    for (row, line) in snap.value().split_inclusive('\n').enumerate() {
        let Some((lo, hi)) = cells
            .selection_row_keys(row)
            .filter_map(|key| {
                let (lo, hi) = (usize::from(key.lo), usize::from(key.hi));
                let hi = hi.min(snap.cols.saturating_sub(1));
                (lo <= hi).then_some((lo, hi))
            })
            .reduce(|(a_lo, a_hi), (b_lo, b_hi)| (a_lo.min(b_lo), a_hi.max(b_hi)))
        else {
            continue;
        };
        let trimmed = line.strip_suffix('\n').unwrap_or(line).chars().count();
        let start = lo.min(trimmed);
        let end = (hi + 1).min(trimmed);
        anchor.get_or_insert((row, start));
        focus = Some((row, end));
    }
    let (anchor, focus) = (anchor?, focus?);
    (anchor != focus).then_some(GridSpan { anchor, focus })
}

/// Minimal valid tree containing only an empty window root.
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

/// Physical-pixel geometry of the visible grid and tab strip within the client area.
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
    /// Top edge of the in-grid tab strip band in physical px.
    pub(crate) strip_y: f64,
    /// Height of the strip band in physical px (`0.0` when hidden).
    pub(crate) strip_h: f64,
}

/// Published tab entry for the in-grid tab strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridTab {
    /// Tab index in the window's tab set.
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
    /// Per-session theme name announced as the tab's description, if any.
    pub(crate) theme: Option<String>,
}

/// Normalized painted text selection in a document's `(row, column)` coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GridSpan {
    /// First selected cell, `(row, column)`.
    pub(crate) anchor: (usize, usize),
    /// One past the last selected cell, `(row, column)`.
    pub(crate) focus: (usize, usize),
}

/// Visible split pane published as its own terminal document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridPane {
    /// Stable pane slot used to namespace row run ids ([`PANE_STRIDE`]).
    pub(crate) slot: usize,
    /// What a screen reader announces on entering the pane.
    pub(crate) label: String,
    /// What this pane's terminal shows, as text.
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

/// The session field open over the tab strip, which holds the keyboard until it is answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridRename {
    /// What the field sets.
    pub(crate) label: &'static str,
    /// Its text, as the field paints it.
    pub(crate) text: String,
    /// The edit position as a character index into `text`.
    pub(crate) caret: usize,
}

/// Active find panel query, caret offset, and match status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridFind {
    /// The live query, exactly as the well paints it.
    pub(crate) query: String,
    /// The edit position as a character index into `query` (the painter's caret).
    pub(crate) caret: usize,
    /// Match count or error status and active modes, used as the field's description.
    pub(crate) status: String,
}

/// Transient chrome message, published as a live region but for the link caption, which is
/// published silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChromeMessage {
    /// Multi-line paste confirmation band ([`crate::paste_banner`]).
    PasteConfirm,
    /// Close/quit confirmation band ([`crate::close_confirm`]).
    CloseConfirm,
    /// The caption naming a hovered link's destination ([`crate::link_target`]).
    LinkCaption,
}

impl ChromeMessage {
    /// The messages in publish order.
    pub(crate) const ORDER: [Self; 3] = [Self::PasteConfirm, Self::CloseConfirm, Self::LinkCaption];

    /// Its id's offset from `MESSAGE_BASE`. Slots 0, 2 to 7 and 16 to 27 belonged to surfaces that
    /// are gone (the message band's rows, its overflow link and its capsules among them) and are
    /// given to no other surface.
    const fn slot(self) -> u64 {
        match self {
            Self::PasteConfirm => 1,
            Self::CloseConfirm => 8,
            Self::LinkCaption => 9,
        }
    }

    const fn node_id(self) -> NodeId {
        NodeId(MESSAGE_BASE + self.slot())
    }

    const fn politeness(self) -> Live {
        match self {
            Self::PasteConfirm | Self::CloseConfirm => Live::Assertive,
            // A pointer resting where links scroll past would speak every one of them.
            Self::LinkCaption => Live::Off,
        }
    }

    const fn role(self) -> Role {
        match self {
            Self::PasteConfirm | Self::CloseConfirm => Role::AlertDialog,
            Self::LinkCaption => Role::Label,
        }
    }
}

/// Frame state for one [`ChromeMessage`] (`text` is spoken; `detail` is description).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GridMessage {
    /// Message surface kind.
    pub(crate) message: ChromeMessage,
    /// Spoken sentence.
    pub(crate) text: String,
    /// Secondary detail exposed as description.
    pub(crate) detail: Option<String>,
}

/// Target published for a Claude Code composer rule chip or fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HarnessChip {
    /// Autopilot mode chip.
    Autopilot,
    /// Team handoff / manager chip.
    Team,
    /// Top rule effort toggle.
    TopEffort,
    /// Model fact.
    Model,
    /// Effort fact.
    Effort,
    /// Working directory fact.
    Path,
    /// Branch fact.
    Branch,
    /// Context usage fact.
    Context,
    /// Token/time usage fact.
    Usage,
    /// The account fact: line E's quota row, one chip over its parts, or line D's fallback unit
    /// (`⎆ main 37% left · ↺ Sat 10:00`).
    Account,
    /// Permission-mode indicator.
    Mode,
    /// Top-rule harness on/off switch.
    Switch,
    /// Passive status title beside the chips.
    Title,
    /// The top rule's mark, `ALab` with its fine print: a link to the site.
    Mark,
    /// The launch card line's mark, `ALab`: a link to the site.
    CardMark,
    /// The top rule's ⬆️ of a newer CLI build: a press asks for the move now.
    Upgrade,
}

impl HarnessChip {
    const SLOTS: u64 = 32;

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
            Self::Account => 12,
            Self::Mark => 13,
            Self::CardMark => 14,
            Self::Upgrade => 15,
        }
    }

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
            12 => Self::Account,
            13 => Self::Mark,
            14 => Self::CardMark,
            15 => Self::Upgrade,
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
            Fact::Account => Self::Account,
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
            Self::Account => Some(Fact::Account),
            Self::Autopilot
            | Self::Team
            | Self::Mode
            | Self::Switch
            | Self::Title
            | Self::Mark
            | Self::CardMark
            | Self::Upgrade => None,
        }
    }
}

/// What a screen reader names the Account chip ([`HarnessChip::Account`]) of a session of
/// `cli`'s pool: another CLI's names its product and the label its login is judged to in that
/// CLI's own pool (`Codex CLI · codex-1`; the product alone where none is judged, as where only
/// the footer's account, which Claude Code's roster judges, is known), so a reader hears which
/// CLI's account it is; Claude Code's is `None`, its footer fact's own words standing
/// (`claude_facts::fact_spoken_of`).
pub(crate) fn account_chip_label(
    cli: aterm_agent::harness::accounts::Cli,
    label: Option<&str>,
) -> Option<String> {
    if cli == aterm_agent::harness::accounts::Cli::Claude {
        return None;
    }
    let product = cli.connector().product;
    Some(match label.filter(|l| !l.is_empty()) {
        Some(label) => format!("{product} \u{b7} {label}"),
        None => product.to_string(),
    })
}

/// Harness chip or status node painted in a Claude Code composer rule.
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
    /// Toggle state for a two-state chip ([`HarnessChip::Switch`], the top
    /// rule's key [`HarnessChip::TopEffort`]), or `None` for a button.
    pub(crate) toggled: Option<aterm_a11y::model::Toggled>,
    /// Grid row index.
    pub(crate) row: usize,
    /// Its first cell.
    pub(crate) col: usize,
    /// Its cells.
    pub(crate) width: usize,
}

/// One row of an open in-grid menu (`crate::autopilot_menu`, `crate::tab_menu`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridMenuItem {
    /// What the row does, in words.
    pub(crate) label: String,
    /// Radio check state for a row that is one of a card's exclusive choices (`Some`: the model
    /// card's models, a tab menu's checked rows), or `None` for an action row.
    pub(crate) checked: Option<bool>,
    /// Whether the row is disabled.
    pub(crate) disabled: bool,
    /// Its grid row.
    pub(crate) row: usize,
    /// The row is a slider of snap stops, published as one (`Role::Slider`), never a menu item.
    pub(crate) slider: Option<GridSlider>,
}

/// A menu row that is a slider of snap stops (`crate::autopilot_menu::MenuRow::Stops`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridSlider {
    /// The 1-based stop its thumb stands on (`None`: its value is none of the stops).
    pub(crate) at: Option<usize>,
    /// How many stops it has.
    pub(crate) stops: usize,
    /// Its value in words (`high`).
    pub(crate) words: String,
}

/// Open popup menu (`Role::Menu`) over the pane: a fact's or a tab's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridMenu {
    /// Menu header label.
    pub(crate) label: String,
    /// Folded row-set hash ([`menu_epoch_from`]) encoded into item node ids.
    pub(crate) epoch: u32,
    /// Its rows, top down.
    pub(crate) items: Vec<GridMenuItem>,
    /// Highlighted item index.
    pub(crate) selected: Option<usize>,
    /// Top-left grid row.
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

fn chip_node_id(session: u64, chip: HarnessChip) -> Option<NodeId> {
    let id = session
        .checked_mul(HarnessChip::SLOTS)
        .and_then(|at| at.checked_add(chip.index()))
        .and_then(|at| CHIP_BASE.checked_add(at))?;
    (id < TAB_BASE).then_some(NodeId(id))
}

/// Decode `node` to `(session, chip)`, if it is in the chip range.
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
pub(crate) const fn menu_epoch_from(hash: u64) -> u32 {
    (hash & ((1 << MENU_EPOCH_BITS) - 1)) as u32
}

fn menu_item_node_id(epoch: u32, item: usize) -> Option<NodeId> {
    let item = u64::try_from(item).ok().filter(|item| *item < MENU_ROWS)?;
    let epoch = u64::from(epoch) & ((1 << MENU_EPOCH_BITS) - 1);
    Some(NodeId(MENU_BASE + 1 + epoch * MENU_ROWS + item))
}

/// Decode `node` to `(epoch, item_index)` for an open menu's item.
pub(crate) fn menu_item_for(node: NodeId) -> Option<(u32, usize)> {
    let at = node
        .0
        .checked_sub(MENU_BASE + 1)
        .filter(|_| node.0 < CHIP_BASE)?;
    let epoch = u32::try_from(at / MENU_ROWS).ok()?;
    let item = usize::try_from(at % MENU_ROWS).ok()?;
    (menu_item_node_id(epoch, item) == Some(node)).then_some((epoch, item))
}

fn cells_rect(g: GridGeometry, row: usize, col: usize, width: usize, height: usize) -> Rect {
    Rect {
        x0: g.origin_x + col as f64 * g.cell_w,
        y0: g.origin_y + row as f64 * g.cell_h,
        x1: g.origin_x + (col + width) as f64 * g.cell_w,
        y1: g.origin_y + (row + height) as f64 * g.cell_h,
    }
}

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
        } else if matches!(chip.chip, HarnessChip::Mark | HarnessChip::CardMark) {
            Role::Link
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
        if chip.chip.fact().is_some() {
            node.set_has_popup(aterm_a11y::model::HasPopup::Menu);
            node.add_action(Action::ShowContextMenu);
        }
        nodes.push((id, node));
        ids.push(id);
    }
    ids
}

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
        let mut node = Node::new(match (&item.slider, item.checked) {
            (Some(_), _) => Role::Slider,
            (None, Some(_)) => Role::MenuItemRadio,
            (None, None) => Role::MenuItem,
        });
        node.set_label(item.label.clone());
        if let Some(slider) = &item.slider {
            if let Some(at) = slider.at {
                node.set_numeric_value(at as f64);
            }
            node.set_min_numeric_value(1.0);
            node.set_max_numeric_value(slider.stops.max(1) as f64);
            node.set_numeric_value_step(1.0);
            node.set_value(slider.words.clone());
            if !item.disabled {
                node.add_action(Action::Increment);
                node.add_action(Action::Decrement);
            }
        }
        if let Some(checked) = item.checked {
            node.set_toggled(if checked {
                aterm_a11y::model::Toggled::True
            } else {
                aterm_a11y::model::Toggled::False
            });
        }
        if item.disabled {
            node.set_disabled();
        } else if item.slider.is_none() {
            // A slider is stepped, never clicked: a click would pick the stop it rests on,
            // which a reader is not told.
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

/// Resource or CPU core utilization meter in the stats deck.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StatsMeter {
    /// Stable meter label.
    pub(crate) name: String,
    /// Utilization percentage in `0..=100` (`None` when unknown).
    pub(crate) value: Option<f32>,
    /// The value in words.
    pub(crate) words: String,
}

/// Stats deck resource meters and per-core meters.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StatsGroup {
    /// "Stats deck, newest first".
    pub(crate) name: &'static str,
    /// The strain gauge at the deck's top left, read first: how hard the whole machine works
    /// now and lately.
    pub(crate) gauge: Option<StatsMeter>,
    /// One meter a resource, in row order.
    pub(crate) meters: Vec<StatsMeter>,
    /// The cores' group's name: "18 CPU cores".
    pub(crate) cores_name: String,
    /// Per-core meters in cluster order (empty when unread).
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

/// Open stats close-up lens group and its section wells.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StatsLens {
    /// Whether the lens owns keyboard focus.
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

const STATS_LENS: u64 = STATS_BASE + 48;
/// The strain gauge's meter, past the resource meters' 15 ids.
const STATS_GAUGE: NodeId = NodeId(STATS_BASE + 16);

fn stats_meter_id(k: usize) -> Option<NodeId> {
    (k < 15).then(|| NodeId(STATS_BASE + 1 + k as u64))
}

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
    let mut children = Vec::with_capacity(group.meters.len() + 2);
    if let Some(m) = &group.gauge {
        nodes.push((STATS_GAUGE, meter_node(m)));
        children.push(STATS_GAUGE);
    }
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

/// The focused pane's open Rewind view, published as a slider over its recorded instants.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GridRewind {
    /// The shown instant's 1-based place among the instants, and their count.
    pub(crate) state: (usize, usize),
    /// What the band says of it: the time, how long ago, and whether it is faithful.
    pub(crate) words: String,
    /// The band's rows in physical px.
    pub(crate) bounds: Option<Rect>,
}

fn push_rewind(nodes: &mut Vec<(NodeId, Node)>, rewind: &GridRewind) {
    let mut node = Node::new(Role::Slider);
    node.set_label("Rewind");
    node.set_numeric_value(rewind.state.0 as f64);
    node.set_min_numeric_value(1.0);
    node.set_max_numeric_value(rewind.state.1.max(1) as f64);
    node.set_numeric_value_step(1.0);
    node.set_description(rewind.words.clone());
    node.add_action(Action::Increment);
    node.add_action(Action::Decrement);
    node.add_action(Action::Focus);
    if let Some(b) = rewind.bounds {
        node.set_bounds(b);
    }
    nodes.push((REWIND, node));
}

/// Frame state projected alongside the visible grid snapshot by [`grid_tree`].
#[derive(Debug, Clone, Default)]
pub(crate) struct GridFrame<'a> {
    /// The in-grid tab strip, empty when it is not a switcher on screen.
    pub(crate) tabs: &'a [GridTab],
    /// Visible split panes (empty for a single-pane or zoomed frame).
    pub(crate) panes: &'a [GridPane],
    /// Whole-screen painted selection when `panes` is empty.
    pub(crate) selection: Option<GridSpan>,
    /// The find panel, live only while find mode is up.
    pub(crate) find: Option<&'a GridFind>,
    pub(crate) rename: Option<&'a GridRename>,
    /// The chrome messages on screen right now, empty on an ordinary frame.
    pub(crate) messages: &'a [GridMessage],
    /// Claude Code composer rule chips in painted order.
    pub(crate) chips: &'a [GridChip],
    /// The open in-grid menu, while one is open.
    pub(crate) menu: Option<&'a GridMenu>,
    /// The stats bars, while the window shows them (the Deck or the sill).
    pub(crate) stats: Option<&'a GridStats>,
    /// The focused pane's open Rewind view.
    pub(crate) rewind: Option<&'a GridRewind>,
    /// Canonical window title (`WindowState::current_title`), falling back to the app name.
    pub(crate) window_title: &'a str,
}

const fn tab_node_id(index: usize) -> NodeId {
    NodeId(TAB_BASE + index as u64)
}

/// Decode a tab node id to its slot index (validated against [`tab_action_index`]).
pub(crate) fn tab_index_for(node: NodeId) -> Option<usize> {
    node.0
        .checked_sub(TAB_BASE)
        .filter(|offset| *offset < (ROW_BASE - TAB_BASE))
        .map(|offset| offset as usize)
}

/// Verify that `request` still matches the stable tab id in `tabs[index]`.
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

fn character_lengths_of(text: &str) -> Box<[u8]> {
    if text.is_ascii() {
        return vec![1; text.len()].into_boxed_slice();
    }
    let mut lengths = Vec::with_capacity(text.chars().count());
    lengths.extend(text.chars().map(|c| c.len_utf8() as u8));
    lengths.into_boxed_slice()
}

fn word_starts_of(line: &str) -> Box<[u8]> {
    // The u8 offset space bounds both the useful scalar prefix and the scratch.
    const INDEX_SPACE: usize = u8::MAX as usize + 1;
    if line.is_empty() {
        return Box::new([0]);
    }
    let mut starts = None;
    let mut len = 1;
    let mut prev_ws = false;
    let mut chars = line.chars();
    for offset in 0..INDEX_SPACE {
        let Some(ch) = chars.next() else { break };
        let whitespace = ch.is_whitespace();
        if offset > 0 && prev_ws && !whitespace {
            #[expect(
                clippy::unnecessary_lazy_evaluations,
                reason = "initialize the 256-byte scratch only at the first transition, not at every word start"
            )]
            let scratch = starts.get_or_insert_with(|| [0u8; INDEX_SPACE]);
            scratch[len] = offset as u8;
            len += 1;
        }
        prev_ws = whitespace;
    }
    match starts.as_ref() {
        Some(starts) => Box::from(&starts[..len]),
        None => Box::new([0]),
    }
}

const fn row_run_id(slot: usize, row: usize, chunk: usize) -> NodeId {
    NodeId(ROW_BASE + slot as u64 * PANE_STRIDE + row as u64 * RUNS_PER_ROW + chunk as u64)
}

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

/// Emit a `Role::Terminal` document and its `Role::TextRun` row children, returning its node id.
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
    let lines: Vec<&str> = snap.value().split_inclusive('\n').collect();
    let mut row_ids: Vec<NodeId> = Vec::with_capacity(lines.len());
    let (row_off, col_off) = origin;

    for (row, line) in lines.iter().enumerate() {
        for (chunk_index, line) in row_chunks(line).enumerate() {
            let id = row_run_id(slot, row, chunk_index);
            row_ids.push(id);
            let mut run = Node::new(Role::TextRun);
            run.set_value(line);
            let character_lengths = character_lengths_of(line);
            let characters = character_lengths.len();
            run.set_character_lengths(character_lengths);
            run.set_word_starts(word_starts_of(line.strip_suffix('\n').unwrap_or(line)));
            if let Some(g) = geom {
                let mut positions: Vec<f32> = Vec::with_capacity(characters);
                let mut widths: Vec<f32> = Vec::with_capacity(characters);
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
    grid.set_value(snap.value());
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

/// Emit one split pane's terminal document, returning its node id.
fn push_pane_document(
    nodes: &mut Vec<(NodeId, Node)>,
    pane: &GridPane,
    geom: Option<GridGeometry>,
) -> NodeId {
    let id = NodeId(PANE_BASE + pane.slot as u64);
    push_terminal_document(
        nodes,
        id,
        pane.slot + 1,
        pane.label.clone(),
        &pane.snap,
        (pane.row_off, pane.col_off),
        geom,
        pane.selection,
    );
    id
}

/// Splice a mixed tab's terminal panes into its native window tree.
///
/// `update` is the composite tree of the tab's native leaves, whose window root lists one child per
/// native leaf in layout order. Each pane takes its `slot` among the `leaves` children and the
/// native roots fill the other slots in order. A focused pane takes the focus the composite left on
/// its window root. Returns the colliding id, leaving `update` unchanged, when a pane's node id is
/// already one of the native nodes.
pub(crate) fn splice_terminal_panes(
    update: &mut TreeUpdate,
    panes: &[GridPane],
    geom: Option<GridGeometry>,
    leaves: usize,
) -> Result<(), NodeId> {
    let Some(root) = update.tree.as_ref().map(|tree| tree.root) else {
        return Ok(());
    };
    let mut documents = Vec::new();
    let mut focused_pane = None;
    for pane in panes {
        let id = push_pane_document(&mut documents, pane, geom);
        if pane.focused {
            focused_pane = Some(id);
        }
    }
    let native: aterm_hash::FxHashSet<NodeId> = update.nodes.iter().map(|(id, _)| *id).collect();
    if let Some((id, _)) = documents.iter().find(|(id, _)| native.contains(id)) {
        return Err(*id);
    }
    let Some((_, window)) = update.nodes.iter_mut().find(|(id, _)| *id == root) else {
        return Ok(());
    };
    let slots = panes
        .iter()
        .map(|pane| pane.slot + 1)
        .fold(leaves, usize::max);
    let mut native_roots = window.children().to_vec().into_iter();
    let mut children = Vec::with_capacity(slots);
    for slot in 0..slots {
        match panes.iter().find(|pane| pane.slot == slot) {
            Some(pane) => children.push(NodeId(PANE_BASE + pane.slot as u64)),
            None => children.extend(native_roots.next()),
        }
    }
    children.extend(native_roots);
    window.set_children(children);
    update.nodes.append(&mut documents);
    if update.focus == root
        && let Some(pane) = focused_pane
    {
        update.focus = pane;
    }
    Ok(())
}

/// `field` with `text` in it as a run at `run`, and its caret at `caret`.
fn push_text_field(
    nodes: &mut Vec<(NodeId, Node)>,
    (id, run_id): (NodeId, NodeId),
    mut field: Node,
    text: &str,
    caret: usize,
) {
    let mut run = Node::new(Role::TextRun);
    run.set_value(text.to_owned());
    run.set_character_lengths(character_lengths_of(text));
    run.set_word_starts(word_starts_of(text));
    nodes.push((run_id, run));

    field.set_value(text.to_owned());
    field.set_children(vec![run_id]);
    let caret = TextPosition {
        node: run_id,
        character_index: caret.min(text.chars().count()),
    };
    field.set_text_selection(TextSelection {
        anchor: caret,
        focus: caret,
    });
    field.add_action(Action::Focus);
    nodes.push((id, field));
}

fn push_find_field(nodes: &mut Vec<(NodeId, Node)>, find: &GridFind) {
    let mut field = Node::new(Role::SearchInput);
    field.set_label("Find");
    if !find.status.is_empty() {
        field.set_description(find.status.clone());
    }
    push_text_field(nodes, (FIND, FIND_RUN), field, &find.query, find.caret);
}

fn push_rename_field(nodes: &mut Vec<(NodeId, Node)>, rename: &GridRename) {
    let mut field = Node::new(Role::TextInput);
    field.set_label(rename.label);
    push_text_field(
        nodes,
        (RENAME, RENAME_RUN),
        field,
        &rename.text,
        rename.caret,
    );
}

fn push_message(nodes: &mut Vec<(NodeId, Node)>, message: &GridMessage) -> NodeId {
    let id = message.message.node_id();
    let mut node = Node::new(message.message.role());
    node.set_label(message.text.clone());
    node.set_value(message.text.clone());
    node.set_live(message.message.politeness());
    if let Some(detail) = &message.detail {
        node.set_description(detail.clone());
    }
    nodes.push((id, node));
    id
}

/// The focus the window chrome claims over the window's content, in precedence order.
struct ChromeFocus {
    menu: Option<(NodeId, Option<NodeId>)>,
    find: bool,
    rename: bool,
    lens: Option<NodeId>,
}

impl ChromeFocus {
    /// The open menu's row (or the menu), else the session field, else the find field, else the
    /// keyboard-driven lens.
    fn focus(&self) -> Option<NodeId> {
        match self.menu {
            Some((id, row)) => Some(row.unwrap_or(id)),
            None if self.rename => Some(RENAME),
            None if self.find => Some(FIND),
            None => self.lens,
        }
    }
}

/// Emit the window chrome of `frame` (tab list, stats, messages, find, rewind, chips, menu) in
/// reading order, appending each top-level id to `root_children`; `cols` is the grid's width.
fn push_chrome(
    nodes: &mut Vec<(NodeId, Node)>,
    root_children: &mut Vec<NodeId>,
    geom: Option<GridGeometry>,
    frame: &GridFrame<'_>,
    cols: usize,
) -> ChromeFocus {
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
                x1: g.origin_x + cols as f64 * g.cell_w,
                y1: g.strip_y + g.strip_h,
            });
        }
        nodes.push((TAB_LIST, list));
        root_children.push(TAB_LIST);
    }

    let mut lens_focus = None;
    if let Some(stats) = frame.stats {
        root_children.push(push_stats(nodes, stats));
        if let Some(lens) = &stats.lens {
            let (id, focus) = push_stats_lens(nodes, lens);
            root_children.push(id);
            lens_focus = lens.keys.then_some(focus);
        }
    }

    for message in ChromeMessage::ORDER
        .iter()
        .filter_map(|kind| frame.messages.iter().find(|m| m.message == *kind))
    {
        root_children.push(push_message(nodes, message));
    }

    if let Some(find) = frame.find {
        push_find_field(nodes, find);
        root_children.push(FIND);
    }

    if let Some(rename) = frame.rename {
        push_rename_field(nodes, rename);
        root_children.push(RENAME);
    }

    if let Some(rewind) = frame.rewind {
        push_rewind(nodes, rewind);
        root_children.push(REWIND);
    }

    root_children.extend(push_chips(nodes, frame.chips, geom));
    let menu = frame.menu.map(|menu| push_menu(nodes, menu, geom));
    if let Some((id, _)) = menu {
        root_children.push(id);
    }

    ChromeFocus {
        menu,
        find: frame.find.is_some(),
        rename: frame.rename.is_some(),
        lens: lens_focus,
    }
}

/// Splice the window chrome of `frame` into a native tab's window tree.
///
/// The chrome goes ahead of the tab's leaves, as in the terminal grid's tree, and an open menu,
/// session field, find field or keyboard-driven lens takes the focus. `frame`'s panes, selection
/// and title are not read. Returns the colliding id, leaving `update` unchanged, when a chrome node's id is
/// already one of the tree's nodes.
pub(crate) fn splice_chrome(
    update: &mut TreeUpdate,
    geom: Option<GridGeometry>,
    frame: &GridFrame<'_>,
    cols: usize,
) -> Result<(), NodeId> {
    let Some(root) = update.tree.as_ref().map(|tree| tree.root) else {
        return Ok(());
    };
    let mut chrome_nodes = Vec::new();
    let mut chrome_children = Vec::new();
    let chrome = push_chrome(&mut chrome_nodes, &mut chrome_children, geom, frame, cols);
    if chrome_children.is_empty() {
        return Ok(());
    }
    let present: aterm_hash::FxHashSet<NodeId> = update.nodes.iter().map(|(id, _)| *id).collect();
    if let Some((id, _)) = chrome_nodes.iter().find(|(id, _)| present.contains(id)) {
        return Err(*id);
    }
    let Some((_, window)) = update.nodes.iter_mut().find(|(id, _)| *id == root) else {
        return Ok(());
    };
    chrome_children.extend_from_slice(window.children());
    window.set_children(chrome_children);
    update.nodes.append(&mut chrome_nodes);
    if let Some(focus) = chrome.focus() {
        update.focus = focus;
    }
    Ok(())
}

/// Build the [`TreeUpdate`] for a terminal window frame.
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

    let chrome = push_chrome(&mut nodes, &mut root_children, geom, frame, snap.cols);

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
            let id = push_pane_document(&mut nodes, pane, geom);
            if pane.focused {
                focused_pane = Some(id);
            }
            root_children.push(id);
        }
    }

    let focus = chrome
        .focus()
        .or(focused_pane)
        .unwrap_or_else(|| root_children.last().copied().unwrap_or(GRID));

    let mut root = Node::new(Role::Window);
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

/// The chips whose nodes [`harness_node_lines`] reports, each under its kind's word.
#[cfg(any(test, debug_assertions))]
const REPORTED_CHIPS: [(HarnessChip, &str); 2] = [
    (HarnessChip::Autopilot, "autopilot"),
    (HarnessChip::Account, "account"),
];

/// The kinds of chip node this build reports in `aterm ctl chrome`'s `a11y` lines (a development
/// build's): `a11y kinds=autopilot,account`, printed once per read before every window's nodes,
/// whatever any window has drawn or has open (an overlay's included), so a reader tells a kind
/// this build does not print from a chip no window has drawn yet. It is a fact of the build, not
/// of a window ([`harness_node_lines`] prints a window's nodes).
#[cfg(any(test, debug_assertions))]
pub(crate) fn harness_kinds_line() -> String {
    let kinds: Vec<&str> = REPORTED_CHIPS.iter().map(|&(_, kind)| kind).collect();
    format!("a11y kinds={}", kinds.join(","))
}

/// THE HARNESS'S NODES AS A SCREEN READER IS HANDED THEM (`aterm ctl chrome`'s `a11y` lines, a
/// development build's, after [`harness_kinds_line`]): of `update`, window `window`'s tree as
/// [`grid_tree`] builds it for the adapter, each Autopilot chip's node (`a11y window=<id>
/// autopilot sid=<sid> role=<role> label="…" description="…"|-`, its session named by `sid_of`)
/// and each Account chip's (`a11y window=<id> account sid=<sid> …`), in the tree's order. Every other node is left out. Two chips are Account chips: line E's quota row, whose
/// label is its quota view's sentences (`Accounts. main, this tab, …`, `QuotaView::spoken`),
/// and line D's fallback unit, drawn where line E is not, whose label is the account fact's
/// (`Account: main, this tab, …`, `claude_facts::fact_spoken_of`, which can carry the same
/// sentences); a lab reads line E's by that head
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §7 step 5).
#[cfg(any(test, debug_assertions))]
pub(crate) fn harness_node_lines(
    window: u64,
    update: &TreeUpdate,
    sid_of: impl Fn(u64) -> Option<String>,
) -> Vec<String> {
    let said = |node: &Node| {
        let description = node
            .description()
            .map_or_else(|| "-".to_string(), |d| format!("{d:?}"));
        format!(
            "role={:?} label={:?} description={description}",
            node.role(),
            node.label().unwrap_or_default()
        )
    };
    update
        .nodes
        .iter()
        .filter_map(|(id, node)| {
            let (session, chip) = chip_for(*id)?;
            let &(_, kind) = REPORTED_CHIPS.iter().find(|&&(c, _)| c == chip)?;
            let sid = sid_of(session).unwrap_or_else(|| "-".to_string());
            Some(format!(
                "a11y window={window} {kind} sid={sid} {}",
                said(node)
            ))
        })
        .collect()
}

#[cfg(test)]
#[path = "a11y_tree_tests.rs"]
mod tests;
