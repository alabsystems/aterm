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
    Action, Node, NodeId, Rect, Role, TextDirection, TextPosition, TextSelection, Toggled, Tree,
    TreeId, TreeUpdate,
};

use crate::prefs::{EditField, EditKind};
use crate::settings::SettingsState;

/// The overlay's root accessibility node. Control nodes are `ROOT + 1 + field_index`.
const ROOT: NodeId = NodeId(0);
/// DEFAULT-2: the single `Role::Terminal` node under [`ROOT`] when NO overlay is open —
/// carries the visible grid text so a screen reader reads the terminal itself.
const GRID: NodeId = NodeId(1);
/// The in-grid tab strip's `Role::TabList` under [`ROOT`], published only when the strip
/// is actually on screen AND is a switcher (≥ 2 tabs — a solo strip paints the window
/// title, not a switcher, so publishing a one-item tab list would describe a control the
/// user cannot see).
const TAB_LIST: NodeId = NodeId(2);
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

/// DEFAULT-2: the accessibility tree for a plain terminal session (no overlay open).
///
/// A `Role::Window` root parenting the visible tab strip (when it is on screen and is a
/// switcher) and ONE read-only `Role::Terminal` node labelled
/// [`crate::accessibility::LABEL`]. The terminal node carries the visible screen as a REAL
/// text document: one `Role::TextRun` child per grid row, each holding that row's text
/// with its hard line break included, per-character lengths, and — when `geom` is known —
/// per-character positions and widths. That structure is what makes the platform publish
/// an AT-SPI `Text` interface (`accesskit_consumer::Node::supports_text_ranges` requires
/// `Role::Terminal` PLUS at least one text run), which is the difference between a screen
/// reader announcing the bare name "aterm terminal" and being able to read, review and
/// navigate the screen by line, word and character.
///
/// The caret is published as a degenerate [`TextSelection`] anchored on the cursor's row
/// run at the cursor's column — the same offset [`crate::accessibility::AccessibleSnapshot::cursor_offset`]
/// reports, so the AT-SPI `CaretOffset` and the macOS `AXSelectedTextRange` cannot
/// diverge. A hidden cursor (DECTCEM) publishes NO selection at all rather than a bogus
/// one, which AT-SPI reports as offset `-1`.
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
    tabs: &[GridTab],
) -> TreeUpdate {
    // One text run per visible row. `split_inclusive` keeps each row's terminating '\n'
    // inside its run, which is exactly how AccessKit represents a hard line break: it is
    // one character of the run, and a caret at end-of-line sits ON it, not past it.
    let lines: Vec<&str> = snap.value().split_inclusive('\n').collect();
    let mut nodes: Vec<(NodeId, Node)> = Vec::with_capacity(lines.len() + tabs.len() + 3);
    let mut row_ids: Vec<NodeId> = Vec::with_capacity(lines.len());

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
        let id = NodeId(ROW_BASE + row as u64 * RUNS_PER_ROW + chunk_index as u64);
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
            let y0 = g.origin_y + row as f64 * g.cell_h;
            run.set_bounds(Rect {
                x0: g.origin_x,
                y0,
                x1: g.origin_x + snap.cols as f64 * g.cell_w,
                y1: y0 + g.cell_h,
            });
        }
        nodes.push((id, run));
        }
    }

    let mut grid = Node::new(Role::Terminal);
    grid.set_label(crate::accessibility::LABEL);
    // Retained for the platforms whose "value" is a plain string (UIA, NSAccessibility);
    // AT-SPI's Value interface is numeric and correctly ignores it, reading the text runs
    // above through the Text interface instead.
    grid.set_value(snap.value().to_string());
    grid.set_read_only();
    grid.set_text_direction(TextDirection::LeftToRight);
    grid.set_children(row_ids);
    if let Some(g) = geom {
        grid.set_bounds(Rect {
            x0: g.origin_x,
            y0: g.origin_y,
            x1: g.origin_x + snap.cols as f64 * g.cell_w,
            y1: g.origin_y + lines.len() as f64 * g.cell_h,
        });
    }
    // The caret: a degenerate selection on the cursor's row run. Clamped to the row's
    // trimmed length exactly like `cursor_offset`, so a caret parked in trailing blanks
    // lands on the line break rather than off the end of the run.
    if let Some((crow, ccol)) = snap.cursor
        && let Some(line) = lines.get(crow)
    {
        let trimmed = line.strip_suffix('\n').unwrap_or(line).chars().count();
        // The caret names the RUN it sits in, not the row: a row wider than
        // `RUN_CHARS` is published as several runs ([`row_chunks`]), and a
        // position addressed against the row's first run with a column past its
        // end is off the end of that node.
        let clamped = ccol.min(trimmed);
        let run = (clamped / RUN_CHARS).min(RUNS_PER_ROW as usize - 1);
        let position = TextPosition {
            node: NodeId(ROW_BASE + crow as u64 * RUNS_PER_ROW + run as u64),
            character_index: clamped - run * RUN_CHARS,
        };
        grid.set_text_selection(TextSelection {
            anchor: position,
            focus: position,
        });
    }
    nodes.push((GRID, grid));

    // The tab strip, when one is on screen and switching between tabs is a thing the user
    // can actually do. Each tab is focusable and clickable; the OS routes both back
    // through `tab_index_for`.
    let mut root_children: Vec<NodeId> = Vec::with_capacity(2);
    if !tabs.is_empty() {
        let mut items: Vec<NodeId> = Vec::with_capacity(tabs.len());
        for tab in tabs {
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
    root_children.push(GRID);

    let mut root = Node::new(Role::Window);
    root.set_label("aterm");
    root.set_children(root_children);
    nodes.push((ROOT, root));
    TreeUpdate {
        nodes,
        tree: Some(Tree::new(ROOT)),
        tree_id: TreeId::ROOT,
        focus: GRID,
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

    fn node<'a>(update: &'a TreeUpdate, id: NodeId) -> &'a Node {
        &update.nodes.iter().find(|(n, _)| *n == id).unwrap().1
    }

    const GEOM: GridGeometry = GridGeometry {
        origin_x: 8.0,
        origin_y: 30.0,
        cell_w: 9.0,
        cell_h: 18.0,
        strip_y: 12.0,
        strip_h: 18.0,
    };

    /// DEFAULT-2: `grid_tree` publishes the terminal grid as a read-only `Role::Terminal`
    /// node under a `Role::Window` root, carrying the snapshot's visible text — what a
    /// screen reader reads on a plain session.
    #[test]
    fn grid_tree_publishes_the_terminal_grid() {
        let snap = live_grid(2, 10, b"hello", Some((0, 5)));
        let update = grid_tree(&snap, None, &[]);
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
        let update = grid_tree(&snap, Some(GEOM), &[]);
        let grid = node(&update, GRID);
        let rows: Vec<NodeId> = (0..3).map(|r| NodeId(ROW_BASE + r * RUNS_PER_ROW)).collect();
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
        let update = grid_tree(&snap, Some(GEOM), &[]);
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
        let update = grid_tree(&snap, None, &[]);
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
        let update = grid_tree(&snap, None, &[]);
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
        let update = grid_tree(&snap, None, &[]);
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
        assert_eq!(sel.focus.node, NodeId(ROW_BASE + 1), "the SECOND run of row 0");
        assert_eq!(sel.focus.character_index, 281 - RUN_CHARS);
    }

    #[test]
    fn row_ids_are_stable_across_frames() {
        let first = grid_tree(&live_grid(3, 12, b"one", Some((0, 3))), None, &[]);
        let second = grid_tree(&live_grid(3, 12, b"one\r\ntwo", Some((1, 3))), None, &[]);
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
        let update = grid_tree(&snap, Some(GEOM), &[]);
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
        let update = grid_tree(&snap, None, &[]);
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
        let update = grid_tree(&live_grid(1, 20, b"echo hi", Some((0, 7))), None, &[]);
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
        let update = grid_tree(&snap, Some(GEOM), &tabs);
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
