// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P2 — the retired overlay model's cross-platform accessibility prototype. Production
//! Settings accessibility and `controls settings` both consume the native compiled
//! semantic tree; this feature-gated module remains only for legacy model coverage. It
//! maps [`crate::settings::SettingsState`] into an [`accesskit::TreeUpdate`] that
//! `accesskit_winit` fans out to the OS
//! accessibility APIs (UIA on Windows, AT-SPI on Linux, NSAccessibility on macOS) — so a
//! screen reader, an AI, and the on-glass view can never disagree.
//!
//! This module is the PURE mapping (model → `TreeUpdate`); it is unit-tested without any
//! window/adapter. The `accesskit_winit::Adapter` that attaches this to a live window and
//! pushes `update_if_active(|| settings_tree(state))` on change is the OS event-loop
//! wiring (runtime-verified with a real screen reader), built on top of this seam.
//!
//! Gated behind the non-default `a11y-accesskit` feature (see this crate's `Cargo.toml`),
//! so the production build neither links AccessKit nor compiles this module.

// macOS: AccessKit's NSAccessibility provider and the `a11y-appkit` grid publisher both
// claim the content view's accessibility tree — enabling both yields a corrupt/duplicated
// VoiceOver tree, so they are mutually exclusive.
#[cfg(all(target_os = "macos", feature = "a11y-appkit"))]
compile_error!(
    "features `a11y-appkit` and `a11y-accesskit` are mutually exclusive on macOS \
     (both claim the content view's accessibility tree); enable at most one"
);

use accesskit::{Action, Node, NodeId, Role, Toggled, Tree, TreeId, TreeUpdate};

use crate::prefs::{EditField, EditKind};
use crate::settings::SettingsState;

/// The overlay's root accessibility node. Control nodes are `ROOT + 1 + field_index`.
const ROOT: NodeId = NodeId(0);
/// DEFAULT-2: the single `Role::Terminal` node under [`ROOT`] when NO overlay is open —
/// carries the visible grid text so a screen reader reads the terminal itself.
const GRID: NodeId = NodeId(1);

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

/// DEFAULT-2: the accessibility tree for a plain terminal session (no overlay open) — a
/// `Role::Window` root parenting ONE read-only `Role::Terminal` node whose value is the
/// visible grid text (`AccessibleSnapshot::value()`), labelled [`crate::accessibility::LABEL`].
/// This is what a screen reader reads on a bare session; previously `push_a11y_tree` handed
/// it [`empty_tree`] (a childless root), so VoiceOver announced nothing. The grid text is the
/// SAME snapshot the SIGUSR1 `.txt` capture and the AppKit publisher use, so the three never
/// diverge.
pub(crate) fn grid_tree(snap: &crate::accessibility::AccessibleSnapshot) -> TreeUpdate {
    let mut grid = Node::new(Role::Terminal);
    grid.set_label(crate::accessibility::LABEL);
    grid.set_value(snap.value().to_string());
    grid.set_read_only();
    let mut root = Node::new(Role::Window);
    root.set_label("aterm");
    root.set_children(vec![GRID]);
    TreeUpdate {
        nodes: vec![(ROOT, root), (GRID, grid)],
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

    /// DEFAULT-2: `grid_tree` publishes the terminal grid as a read-only `Role::Terminal`
    /// node under a `Role::Window` root, carrying the snapshot's visible text — what a
    /// screen reader reads on a plain session. Built from a real engine so the text is the
    /// genuine `AccessibleSnapshot::value()` (the anti-divergence anchor with the SIGUSR1
    /// `.txt` snapshot).
    #[test]
    fn grid_tree_publishes_the_terminal_grid() {
        let mut term = aterm_core::terminal::Terminal::new(2, 10);
        term.process(b"hello");
        let cells: Vec<_> = (0..2).map(|r| term.render_row(r)).collect();
        let snap = crate::accessibility::AccessibleSnapshot::from_cells(&cells, 10, Some((0, 5)));

        let update = grid_tree(&snap);
        assert_eq!(update.nodes.len(), 2, "a Window root + one Terminal child");
        assert_eq!(update.focus, GRID);
        let root = &update.nodes.iter().find(|(id, _)| *id == ROOT).unwrap().1;
        let grid = &update.nodes.iter().find(|(id, _)| *id == GRID).unwrap().1;
        assert_eq!(root.role(), Role::Window);
        assert_eq!(root.children(), [GRID]);
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
