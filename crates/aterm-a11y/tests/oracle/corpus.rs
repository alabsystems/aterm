// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The semantics oracle's input: windows' accessibility trees and what
//! happens to them, the same source compiled twice.
//!
//! `tests/recorder` (outside the workspace) compiles this file against the
//! registry `accesskit` 0.24.1, and the replay in `src/atspi/oracle.rs`
//! against aterm's own model. Both see it as a sibling of a `model` module,
//! so every name used here exists, with the same meaning, in both: that is
//! the model's compatibility contract, held by compiling this file.
//!
//! Two kinds of scenario:
//!
//! * fixtures shaped like the trees aterm-gui publishes (the terminal grid
//!   and its rows, the tab strip, chrome messages and dialogs, the find field,
//!   menus, the palette, Settings' controls), with the edits a session makes;
//! * generated ones: random trees over the same roles and properties, changed
//!   by random edits, from a fixed seed, so the input is the same on both
//!   sides and every run.
//!
//! And the blank-line documents ([`blank_line_documents`]): small text
//! inputs with empty runs anywhere, for the text navigation table the
//! scenarios cannot hold (the registry crates' paragraph search hangs on
//! them, see `transcript::blank_line_answer`).
//!
//! Every update is valid for both models: each node but the root has one
//! parent, focus and selections name live nodes, a run's character lengths
//! sum to its text's bytes and its positions and widths match them. A node is
//! always built by [`NodeSpec::build`], in one setter order, so two equal
//! specs build equal nodes under either model's equality.

use std::collections::BTreeMap;

use super::model::{
    Action, Affine, HasPopup, Invalid, Live, Node, NodeId, Rect, Role, TextDirection, TextPosition,
    TextSelection, Toggled, Tree, TreeId, TreeUpdate, Vec2,
};

/// One thing that happens to the windows of one application.
pub(crate) enum Step {
    /// A window opens with its first tree.
    Open {
        window: usize,
        update: TreeUpdate,
        focused: bool,
    },
    /// A window's tree changes.
    Update { window: usize, update: TreeUpdate },
    /// A window gains or loses the keyboard focus.
    Focus { window: usize, focused: bool },
    /// A window moves or resizes: its outer and inner screen rectangles.
    Bounds {
        window: usize,
        outer: Rect,
        inner: Rect,
    },
    /// A window closes.
    Close { window: usize },
}

/// A named sequence of steps over one application.
pub(crate) struct Scenario {
    pub(crate) name: String,
    pub(crate) steps: Vec<Step>,
}

/// Every scenario, fixtures first.
pub(crate) fn scenarios() -> Vec<Scenario> {
    let mut all = fixtures();
    for seed in 0..GENERATED {
        all.push(generated(seed));
    }
    all
}

/// How many generated scenarios there are.
const GENERATED: u64 = 40;

// --- the node description both sides build from ----------------------------

/// Everything a node can carry here, built into a model node in one order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NodeSpec {
    role: Role,
    label: Option<String>,
    description: Option<String>,
    value: Option<String>,
    url: Option<String>,
    children: Vec<u64>,
    bounds: Option<Rect>,
    /// `translate(dx, dy) * scale(s)`.
    transform: Option<(f64, f64, f64)>,
    selection: Option<((u64, usize), (u64, usize))>,
    /// A run's characters, each one selectable unit (a combining sequence or
    /// a CRLF is one).
    chars: Option<Vec<String>>,
    word_starts: Vec<u8>,
    /// Character positions and widths: each character `cell` wide, the hard
    /// line break zero-wide.
    cell: Option<f32>,
    direction: Option<TextDirection>,
    numeric: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
    step: Option<f64>,
    toggled: Option<Toggled>,
    popup: bool,
    invalid: bool,
    live: Option<Live>,
    selected: Option<bool>,
    expanded: Option<bool>,
    disabled: bool,
    read_only: bool,
    busy: bool,
    line_break: bool,
    actions: Vec<Action>,
}

impl NodeSpec {
    fn new(role: Role) -> Self {
        Self {
            role,
            label: None,
            description: None,
            value: None,
            url: None,
            children: Vec::new(),
            bounds: None,
            transform: None,
            selection: None,
            chars: None,
            word_starts: Vec::new(),
            cell: None,
            direction: None,
            numeric: None,
            min: None,
            max: None,
            step: None,
            toggled: None,
            popup: false,
            invalid: false,
            live: None,
            selected: None,
            expanded: None,
            disabled: false,
            read_only: false,
            busy: false,
            line_break: false,
            actions: Vec::new(),
        }
    }

    fn label(mut self, label: &str) -> Self {
        self.label = Some(label.to_owned());
        self
    }

    fn children(mut self, children: &[u64]) -> Self {
        self.children = children.to_vec();
        self
    }

    fn bounds(mut self, x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        self.bounds = Some(Rect::new(x0, y0, x1, y1));
        self
    }

    fn actions(mut self, actions: &[Action]) -> Self {
        self.actions = actions.to_vec();
        self
    }

    /// A text run of `text` split into characters (a `\r\n` is one), its
    /// words starting after each space.
    fn run(text: &str) -> Self {
        let mut spec = Self::new(Role::TextRun);
        spec.set_text(split(text));
        spec
    }

    fn set_text(&mut self, chars: Vec<String>) {
        self.word_starts = word_starts(&chars);
        self.chars = Some(chars);
    }

    fn laid_out(mut self, cell: f32) -> Self {
        self.cell = Some(cell);
        self
    }

    /// The model node, every setter in one order.
    pub(crate) fn build(&self) -> Node {
        let mut node = Node::new(self.role);
        if let Some(label) = &self.label {
            node.set_label(label.clone());
        }
        if let Some(description) = &self.description {
            node.set_description(description.clone());
        }
        if let Some(chars) = &self.chars {
            node.set_value(chars.concat());
            node.set_character_lengths(
                chars
                    .iter()
                    .map(|c| u8::try_from(c.len()).unwrap_or(u8::MAX))
                    .collect::<Vec<u8>>(),
            );
            node.set_word_starts(self.word_starts.clone());
            if let Some(cell) = self.cell {
                let mut positions = Vec::with_capacity(chars.len());
                let mut widths = Vec::with_capacity(chars.len());
                let mut x = 0.0f32;
                for c in chars {
                    positions.push(x);
                    let w = if c.ends_with('\n') { 0.0 } else { cell };
                    widths.push(w);
                    x += w;
                }
                node.set_character_positions(positions);
                node.set_character_widths(widths);
            }
        } else if let Some(value) = &self.value {
            node.set_value(value.clone());
        }
        if let Some(url) = &self.url {
            node.set_url(url.clone());
        }
        if !self.children.is_empty() {
            node.set_children(self.children.iter().map(|&c| NodeId(c)).collect::<Vec<_>>());
        }
        if let Some(bounds) = self.bounds {
            node.set_bounds(bounds);
        }
        if let Some((dx, dy, s)) = self.transform {
            node.set_transform(Affine::translate(Vec2::new(dx, dy)) * Affine::scale(s));
        }
        if let Some(((anchor_run, anchor_at), (focus_run, focus_at))) = self.selection {
            node.set_text_selection(TextSelection {
                anchor: TextPosition {
                    node: NodeId(anchor_run),
                    character_index: anchor_at,
                },
                focus: TextPosition {
                    node: NodeId(focus_run),
                    character_index: focus_at,
                },
            });
        }
        if let Some(direction) = self.direction {
            node.set_text_direction(direction);
        }
        if let Some(v) = self.numeric {
            node.set_numeric_value(v);
        }
        if let Some(v) = self.min {
            node.set_min_numeric_value(v);
        }
        if let Some(v) = self.max {
            node.set_max_numeric_value(v);
        }
        if let Some(v) = self.step {
            node.set_numeric_value_step(v);
        }
        if let Some(toggled) = self.toggled {
            node.set_toggled(toggled);
        }
        if self.popup {
            node.set_has_popup(HasPopup::Menu);
        }
        if self.invalid {
            node.set_invalid(Invalid::True);
        }
        if let Some(live) = self.live {
            node.set_live(live);
        }
        if let Some(selected) = self.selected {
            node.set_selected(selected);
        }
        if let Some(expanded) = self.expanded {
            node.set_expanded(expanded);
        }
        if self.disabled {
            node.set_disabled();
        }
        if self.read_only {
            node.set_read_only();
        }
        if self.busy {
            node.set_busy();
        }
        if self.line_break {
            node.set_is_line_breaking_object();
        }
        for &action in &self.actions {
            node.add_action(action);
        }
        node
    }
}

/// `text` as selectable characters: a `\r\n` and a base letter with its
/// combining acute accent are one character each.
fn split(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in text.chars() {
        let joins = match out.last() {
            Some(last) => (last == "\r" && c == '\n') || c == '\u{301}',
            None => false,
        };
        match out.last_mut() {
            Some(last) if joins => last.push(c),
            _ => out.push(c.to_string()),
        }
    }
    out
}

/// Where words start: the first character, and each one after a space.
fn word_starts(chars: &[String]) -> Vec<u8> {
    let mut starts = Vec::new();
    let mut after_space = true;
    for (i, c) in chars.iter().enumerate() {
        let space = c == " ";
        if after_space
            && !space
            && !c.ends_with('\n')
            && let Ok(i) = u8::try_from(i)
        {
            starts.push(i);
        }
        after_space = space;
    }
    starts
}

/// A window's tree as specs, and its focus.
#[derive(Clone)]
pub(crate) struct Snapshot {
    nodes: BTreeMap<u64, NodeSpec>,
    focus: u64,
}

impl Snapshot {
    fn new(nodes: impl IntoIterator<Item = (u64, NodeSpec)>, focus: u64) -> Self {
        Self {
            nodes: nodes.into_iter().collect(),
            focus,
        }
    }

    fn node(&mut self, id: u64) -> &mut NodeSpec {
        self.nodes.get_mut(&id).expect("a node of the snapshot")
    }

    /// Every node, with the tree's data: a window's first update.
    fn full(&self) -> TreeUpdate {
        let mut tree = Tree::new(NodeId(ROOT));
        tree.toolkit_name = Some("aterm".to_owned());
        tree.toolkit_version = Some("0.106.0".to_owned());
        TreeUpdate {
            nodes: self
                .nodes
                .iter()
                .map(|(&id, spec)| (NodeId(id), spec.build()))
                .collect(),
            tree: Some(tree),
            tree_id: TreeId::ROOT,
            focus: NodeId(self.focus),
        }
    }

    /// Only the nodes that differ from `before`, as a toolkit sends them.
    fn since(&self, before: &Snapshot) -> TreeUpdate {
        TreeUpdate {
            nodes: self
                .nodes
                .iter()
                .filter(|(id, spec)| before.nodes.get(id) != Some(spec))
                .map(|(&id, spec)| (NodeId(id), spec.build()))
                .collect(),
            tree: None,
            tree_id: TreeId::ROOT,
            focus: NodeId(self.focus),
        }
    }

    /// The ids under `id`, `id` included.
    fn subtree(&self, id: u64) -> Vec<u64> {
        let mut out = vec![id];
        let mut i = 0;
        while let Some(&at) = out.get(i) {
            if let Some(spec) = self.nodes.get(&at) {
                out.extend(spec.children.iter().copied());
            }
            i += 1;
        }
        out
    }
}

/// Records a scenario as snapshots: each change is sent as the difference
/// from the last one sent, or whole.
struct Script {
    name: String,
    steps: Vec<Step>,
    sent: BTreeMap<usize, Snapshot>,
}

impl Script {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            steps: Vec::new(),
            sent: BTreeMap::new(),
        }
    }

    fn open(&mut self, window: usize, snapshot: &Snapshot, focused: bool) {
        self.steps.push(Step::Open {
            window,
            update: snapshot.full(),
            focused,
        });
        self.sent.insert(window, snapshot.clone());
    }

    /// The changes since the last update, as a difference.
    fn update(&mut self, window: usize, snapshot: &Snapshot) {
        let update = match self.sent.get(&window) {
            Some(before) => snapshot.since(before),
            None => snapshot.full(),
        };
        self.steps.push(Step::Update { window, update });
        self.sent.insert(window, snapshot.clone());
    }

    /// The whole tree again (unchanged nodes included, the tree data too).
    fn resend(&mut self, window: usize, snapshot: &Snapshot) {
        self.steps.push(Step::Update {
            window,
            update: snapshot.full(),
        });
        self.sent.insert(window, snapshot.clone());
    }

    fn focus(&mut self, window: usize, focused: bool) {
        self.steps.push(Step::Focus { window, focused });
    }

    fn bounds(&mut self, window: usize, outer: Rect, inner: Rect) {
        self.steps.push(Step::Bounds {
            window,
            outer,
            inner,
        });
    }

    fn close(&mut self, window: usize) {
        self.steps.push(Step::Close { window });
        self.sent.remove(&window);
    }

    fn done(self) -> Scenario {
        Scenario {
            name: self.name,
            steps: self.steps,
        }
    }
}

// --- fixtures ------------------------------------------------------------------

const ROOT: u64 = 0;
const TERMINAL: u64 = 1;
const TAB_LIST: u64 = 2;
const FIND: u64 = 3;
const FIND_RUN: u64 = 4;
/// aterm-gui's id ranges, so the object paths carry its large numbers.
const MESSAGE_BASE: u64 = 1 << 20;
const MENU_BASE: u64 = 1 << 29;
const TAB_BASE: u64 = 1 << 40;
const ROW_BASE: u64 = 1 << 48;

/// The grid's geometry: 8 by 16 pixel cells from (12, 36).
const CELL_W: f64 = 8.0;
const CELL_H: f64 = 16.0;
const GRID_X: f64 = 12.0;
const GRID_Y: f64 = 36.0;

fn fixtures() -> Vec<Scenario> {
    vec![
        terminal(),
        chrome(),
        find(),
        menu(),
        settings(),
        palette(),
        two_windows(),
        unicode(),
        hyperlink_and_dialog_root(),
    ]
}

/// A grid row's run, laid out in its cells.
fn row(index: u64, text: &str) -> (u64, NodeSpec) {
    let y0 = GRID_Y + index as f64 * CELL_H;
    let run = NodeSpec::run(text).laid_out(CELL_W as f32).bounds(
        GRID_X,
        y0,
        GRID_X + 40.0 * CELL_W,
        y0 + CELL_H,
    );
    (ROW_BASE + index * 256, run)
}

/// aterm's grid: a read-only terminal document of `lines`, the caret at
/// `caret` (row, column), under the window with a two-tab strip.
fn grid(lines: &[&str], caret: Option<(u64, usize)>, selected_tab: u64) -> Snapshot {
    let mut nodes: Vec<(u64, NodeSpec)> = Vec::new();
    let mut rows = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let (id, run) = row(i as u64, line);
        rows.push(id);
        nodes.push((id, run));
    }
    let mut terminal = NodeSpec::new(Role::Terminal)
        .label("aterm terminal")
        .children(&rows)
        .bounds(
            GRID_X,
            GRID_Y,
            GRID_X + 40.0 * CELL_W,
            GRID_Y + lines.len() as f64 * CELL_H,
        )
        .actions(&[
            Action::Focus,
            Action::ScrollUp,
            Action::ScrollDown,
            Action::SetTextSelection,
            Action::ReplaceSelectedText,
        ]);
    terminal.value = Some(lines.concat());
    terminal.read_only = true;
    terminal.direction = Some(TextDirection::LeftToRight);
    terminal.selection = caret.map(|(r, c)| ((ROW_BASE + r * 256, c), (ROW_BASE + r * 256, c)));
    nodes.push((TERMINAL, terminal));
    let tabs = [TAB_BASE, TAB_BASE + 1];
    for (i, &tab) in tabs.iter().enumerate() {
        let mut spec = NodeSpec::new(Role::Tab)
            .label(if i == 0 { "zsh" } else { "logs" })
            .bounds(
                GRID_X + i as f64 * 120.0,
                4.0,
                GRID_X + (i + 1) as f64 * 120.0,
                28.0,
            )
            .actions(&[Action::Click, Action::Focus]);
        spec.description = Some(format!("theme {i}"));
        spec.selected = Some(tab == selected_tab);
        nodes.push((tab, spec));
    }
    nodes.push((
        TAB_LIST,
        NodeSpec::new(Role::TabList)
            .label("Tabs")
            .children(&tabs)
            .bounds(GRID_X, 4.0, GRID_X + 240.0, 28.0),
    ));
    nodes.push((
        ROOT,
        NodeSpec::new(Role::Window)
            .label("~")
            .children(&[TAB_LIST, TERMINAL]),
    ));
    Snapshot::new(nodes, TERMINAL)
}

fn terminal() -> Scenario {
    let mut s = Script::new("fixture terminal");
    let lines = ["user@host:~$ ls\n", "notes  todo.txt\n", "user@host:~$ "];
    let mut tree = grid(&lines, Some((2, 13)), TAB_BASE);
    s.open(0, &tree, true);
    s.bounds(
        0,
        Rect::new(100.0, 50.0, 452.0, 400.0),
        Rect::new(102.0, 80.0, 450.0, 398.0),
    );
    // Typing at the prompt: the row's run grows and the caret follows.
    for typed in ["user@host:~$ c", "user@host:~$ ca", "user@host:~$ cat"] {
        let caret = typed.chars().count();
        tree = grid(&[lines[0], lines[1], typed], Some((2, caret)), TAB_BASE);
        s.update(0, &tree);
    }
    // A selection from the second row into the third, and back to a caret.
    tree.node(TERMINAL).selection = Some(((ROW_BASE + 256, 2), (ROW_BASE + 512, 4)));
    s.update(0, &tree);
    tree.node(TERMINAL).selection = Some(((ROW_BASE + 512, 16), (ROW_BASE + 512, 16)));
    s.update(0, &tree);
    // Output scrolls the screen: every row's text moves up one run.
    let scrolled = [
        "notes  todo.txt\n",
        "user@host:~$ cat todo.txt\n",
        "- ship it\n",
        "user@host:~$ ",
    ];
    tree = grid(&scrolled, Some((3, 13)), TAB_BASE);
    s.update(0, &tree);
    // The caret hidden (DECTCEM): no selection at all.
    tree.node(TERMINAL).selection = None;
    s.update(0, &tree);
    // The other tab is selected, and the whole tree is sent again.
    tree = grid(&scrolled, Some((3, 13)), TAB_BASE + 1);
    s.resend(0, &tree);
    s.focus(0, false);
    s.focus(0, true);
    // Focus moves to a tab, then back.
    tree.focus = TAB_BASE + 1;
    s.update(0, &tree);
    tree.focus = TERMINAL;
    s.update(0, &tree);
    s.bounds(
        0,
        Rect::new(300.0, 250.0, 652.0, 600.0),
        Rect::new(302.0, 280.0, 650.0, 598.0),
    );
    // The screen clears to an empty prompt row.
    tree = grid(&["\n", ""], Some((1, 0)), TAB_BASE + 1);
    s.update(0, &tree);
    s.done()
}

fn chrome() -> Scenario {
    let mut s = Script::new("fixture chrome messages");
    let mut tree = grid(&["$ cp a b\n", "$ "], Some((1, 2)), TAB_BASE);
    let status = MESSAGE_BASE + 1;
    let progress = MESSAGE_BASE + 2;
    let overflow = MESSAGE_BASE + 3;
    let mut status_spec = NodeSpec::new(Role::Status)
        .label("Copied")
        .bounds(12.0, 520.0, 332.0, 540.0)
        .actions(&[Action::Click]);
    status_spec.value = Some("Copied".to_owned());
    status_spec.live = Some(Live::Polite);
    let mut progress_spec = NodeSpec::new(Role::ProgressIndicator).label("Indexing");
    progress_spec.numeric = Some(30.0);
    progress_spec.min = Some(0.0);
    progress_spec.max = Some(100.0);
    let link = NodeSpec::new(Role::Link)
        .label("2 more")
        .actions(&[Action::Click]);
    tree.nodes.insert(status, status_spec);
    tree.nodes.insert(progress, progress_spec);
    tree.nodes.insert(overflow, link);
    tree.node(ROOT).children = vec![TAB_LIST, TERMINAL, status, progress, overflow];
    s.open(0, &tree, true);
    // The status band says something else: a name change and an announcement.
    tree.node(status).label = Some("Pasted 3 lines".to_owned());
    tree.node(status).value = Some("Pasted 3 lines".to_owned());
    s.update(0, &tree);
    // The meter moves, then loses its value (indeterminate, busy).
    tree.node(progress).numeric = Some(75.0);
    s.update(0, &tree);
    tree.node(progress).numeric = None;
    tree.node(progress).busy = true;
    s.update(0, &tree);
    // An assertive alert appears, is replaced, and goes.
    let alert = MESSAGE_BASE + 4;
    let mut alert_spec = NodeSpec::new(Role::Alert).label("Disk almost full");
    alert_spec.live = Some(Live::Assertive);
    alert_spec.description = Some("2% left".to_owned());
    tree.nodes.insert(alert, alert_spec);
    tree.node(ROOT).children.push(alert);
    s.update(0, &tree);
    tree.node(alert).label = Some("Disk full".to_owned());
    s.update(0, &tree);
    tree.nodes.remove(&alert);
    tree.node(ROOT).children.retain(|&c| c != alert);
    s.update(0, &tree);
    // A paste confirmation: a dialog with two buttons takes the focus.
    let dialog = MESSAGE_BASE + 5;
    let ok = MESSAGE_BASE + 6;
    let cancel = MESSAGE_BASE + 7;
    let toggle = MESSAGE_BASE + 8;
    let mut ok_spec = NodeSpec::new(Role::Button)
        .label("Paste")
        .bounds(200.0, 300.0, 280.0, 324.0)
        .actions(&[Action::Click, Action::Focus]);
    ok_spec.description = Some("3 lines".to_owned());
    let mut cancel_spec = NodeSpec::new(Role::Button)
        .label("Cancel")
        .bounds(290.0, 300.0, 370.0, 324.0)
        .actions(&[Action::Click, Action::Focus]);
    cancel_spec.disabled = true;
    let mut toggle_spec = NodeSpec::new(Role::Button)
        .label("Always")
        .actions(&[Action::Click]);
    toggle_spec.toggled = Some(Toggled::False);
    toggle_spec.popup = true;
    tree.nodes.insert(ok, ok_spec);
    tree.nodes.insert(cancel, cancel_spec);
    tree.nodes.insert(toggle, toggle_spec);
    tree.nodes.insert(
        dialog,
        NodeSpec::new(Role::AlertDialog)
            .label("Paste 3 lines?")
            .children(&[ok, cancel, toggle])
            .bounds(180.0, 200.0, 420.0, 340.0),
    );
    tree.node(ROOT).children.push(dialog);
    tree.focus = ok;
    s.update(0, &tree);
    tree.node(toggle).toggled = Some(Toggled::True);
    tree.node(cancel).disabled = false;
    tree.focus = cancel;
    s.update(0, &tree);
    tree.node(toggle).toggled = Some(Toggled::Mixed);
    s.update(0, &tree);
    s.focus(0, false);
    // The dialog closes while the window is in the background.
    for id in tree.subtree(dialog) {
        tree.nodes.remove(&id);
    }
    tree.node(ROOT).children.retain(|&c| c != dialog);
    tree.focus = TERMINAL;
    s.update(0, &tree);
    s.focus(0, true);
    s.done()
}

/// The find field over the grid: a search input with one run.
fn with_find(mut tree: Snapshot, query: &str, caret: usize, status: &str) -> Snapshot {
    let run = NodeSpec::run(query);
    let mut field = NodeSpec::new(Role::SearchInput)
        .label("Find")
        .children(&[FIND_RUN])
        .actions(&[Action::Focus, Action::SetTextSelection, Action::SetValue]);
    field.value = Some(query.to_owned());
    if !status.is_empty() {
        field.description = Some(status.to_owned());
    }
    field.selection = Some(((FIND_RUN, caret), (FIND_RUN, caret)));
    tree.nodes.insert(FIND_RUN, run);
    tree.nodes.insert(FIND, field);
    tree.node(ROOT).children.push(FIND);
    tree.focus = FIND;
    tree
}

fn find() -> Scenario {
    let mut s = Script::new("fixture find field");
    let base = grid(&["foo bar\n", "foo\n", "$ "], Some((2, 2)), TAB_BASE);
    s.open(0, &base, true);
    // Find opens empty: the run has no characters.
    let mut tree = with_find(base.clone(), "", 0, "");
    s.update(0, &tree);
    for (query, status) in [("f", "3 matches"), ("fo", "2 matches"), ("foo", "1 of 2")] {
        tree = with_find(base.clone(), query, query.chars().count(), status);
        s.update(0, &tree);
    }
    // The caret moves inside the query, then a selection of all of it.
    tree.node(FIND).selection = Some(((FIND_RUN, 1), (FIND_RUN, 1)));
    s.update(0, &tree);
    tree.node(FIND).selection = Some(((FIND_RUN, 0), (FIND_RUN, 3)));
    s.update(0, &tree);
    // A deletion of the whole query.
    tree = with_find(base.clone(), "", 0, "no query");
    s.update(0, &tree);
    // Find closes: the field goes and the focus returns to the grid.
    s.update(0, &base);
    s.done()
}

fn menu_tree(epoch: u64, checked: usize, focus_item: usize, disabled: usize) -> Snapshot {
    let mut tree = grid(&["$ claude\n", "> "], Some((1, 2)), TAB_BASE);
    let menu = MENU_BASE;
    let items: Vec<u64> = (0..4).map(|i| MENU_BASE + 1 + epoch * 64 + i).collect();
    for (i, &item) in items.iter().enumerate() {
        let role = if i < 2 {
            Role::MenuItemRadio
        } else {
            Role::MenuItem
        };
        let mut spec = NodeSpec::new(role)
            .label(["Off", "Auto", "Settings", "Close menu"][i])
            .bounds(400.0, 60.0 + i as f64 * 20.0, 560.0, 80.0 + i as f64 * 20.0)
            .actions(&[Action::Click, Action::Focus]);
        if i < 2 {
            spec.toggled = Some(if i == checked {
                Toggled::True
            } else {
                Toggled::False
            });
        }
        spec.disabled = i == disabled;
        tree.nodes.insert(item, spec);
    }
    let mut chip = NodeSpec::new(Role::Button)
        .label("Autopilot")
        .actions(&[Action::Click]);
    chip.popup = true;
    chip.expanded = Some(true);
    tree.nodes.insert(MENU_BASE - 1, chip);
    tree.nodes.insert(
        menu,
        NodeSpec::new(Role::Menu)
            .label("Autopilot")
            .children(&items)
            .bounds(400.0, 60.0, 560.0, 140.0),
    );
    tree.node(ROOT).children.push(MENU_BASE - 1);
    tree.node(ROOT).children.push(menu);
    tree.focus = items[focus_item];
    tree
}

fn menu() -> Scenario {
    let mut s = Script::new("fixture menu");
    let mut tree = menu_tree(0, 0, 0, 3);
    s.open(0, &tree, true);
    for (checked, focus) in [(0, 1), (1, 1), (1, 2)] {
        tree = menu_tree(0, checked, focus, 3);
        s.update(0, &tree);
    }
    // The rows move under the menu: a new epoch renames every item.
    tree = menu_tree(1, 1, 0, 2);
    s.update(0, &tree);
    // The menu closes.
    let closed = grid(&["$ claude\n", "> "], Some((1, 2)), TAB_BASE);
    s.update(0, &closed);
    s.done()
}

const APP: u64 = 10;
const NAV: u64 = 11;
const LIST: u64 = 12;
const GROUP: u64 = 13;

fn settings_tree(opacity: f64, on: bool, name: &str, scale: f64) -> Snapshot {
    let mut nodes: Vec<(u64, NodeSpec)> = Vec::new();
    let pages: Vec<u64> = (0..3).map(|i| 100 + i).collect();
    for (i, &page) in pages.iter().enumerate() {
        let mut spec = NodeSpec::new(Role::MenuItem)
            .label(["Appearance", "Keys", "About"][i])
            .bounds(0.0, i as f64 * 24.0, 160.0, (i + 1) as f64 * 24.0)
            .actions(&[Action::Click, Action::Focus]);
        spec.selected = Some(i == 0);
        nodes.push((page, spec));
    }
    nodes.push((
        LIST,
        NodeSpec::new(Role::List)
            .label("Pages")
            .children(&pages)
            .bounds(0.0, 0.0, 160.0, 72.0),
    ));
    nodes.push((
        NAV,
        NodeSpec::new(Role::Navigation)
            .label("Sections")
            .children(&[LIST])
            .bounds(0.0, 0.0, 160.0, 400.0),
    ));
    let heading = 20;
    let label = 21;
    let switch = 22;
    let slider = 23;
    let field = 24;
    let field_run = 25;
    let doc = 26;
    let viewport = 27;
    let status = 28;
    let button = 29;
    nodes.push((
        heading,
        NodeSpec::new(Role::Heading)
            .label("Appearance")
            .bounds(180.0, 0.0, 600.0, 30.0),
    ));
    let mut label_spec = NodeSpec::new(Role::Label).bounds(180.0, 40.0, 600.0, 60.0);
    label_spec.value = Some("Changes apply at once".to_owned());
    nodes.push((label, label_spec));
    let mut switch_spec = NodeSpec::new(Role::Switch)
        .label("Ligatures")
        .bounds(180.0, 70.0, 230.0, 90.0)
        .actions(&[Action::Click, Action::Focus]);
    switch_spec.toggled = Some(if on { Toggled::True } else { Toggled::False });
    nodes.push((switch, switch_spec));
    let mut slider_spec = NodeSpec::new(Role::Slider)
        .label("Opacity")
        .bounds(180.0, 100.0, 400.0, 120.0)
        .actions(&[
            Action::Focus,
            Action::SetValue,
            Action::Increment,
            Action::Decrement,
        ]);
    slider_spec.numeric = Some(opacity);
    slider_spec.min = Some(0.0);
    slider_spec.max = Some(1.0);
    slider_spec.step = Some(0.05);
    nodes.push((slider, slider_spec));
    nodes.push((field_run, NodeSpec::run(name).laid_out(7.0)));
    let mut field_spec = NodeSpec::new(Role::TextInput)
        .label("Profile name")
        .children(&[field_run])
        .bounds(180.0, 130.0, 400.0, 150.0)
        .actions(&[
            Action::Focus,
            Action::SetValue,
            Action::ReplaceSelectedText,
            Action::SetTextSelection,
        ]);
    field_spec.value = Some(name.to_owned());
    field_spec.direction = Some(TextDirection::LeftToRight);
    let end = split(name).len();
    field_spec.selection = Some(((field_run, end), (field_run, end)));
    field_spec.invalid = name.is_empty();
    nodes.push((field, field_spec));
    let doc_runs: Vec<u64> = (0..3).map(|i| 30 + i).collect();
    for (i, &run) in doc_runs.iter().enumerate() {
        let text = ["aterm draws text with care.\n", "\n", "Fonts are hinted."][i];
        let mut spec = NodeSpec::run(text).laid_out(7.0).bounds(
            180.0,
            160.0 + i as f64 * 18.0,
            600.0,
            178.0 + i as f64 * 18.0,
        );
        spec.line_break = true;
        nodes.push((run, spec));
    }
    let mut doc_spec = NodeSpec::new(Role::Document)
        .label("Font notes")
        .children(&doc_runs)
        .bounds(180.0, 160.0, 600.0, 214.0);
    doc_spec.read_only = true;
    doc_spec.direction = Some(TextDirection::LeftToRight);
    nodes.push((doc, doc_spec));
    let viewport_runs: Vec<u64> = (0..2).map(|i| 40 + i).collect();
    for (i, &run) in viewport_runs.iter().enumerate() {
        let text = ["[font]", "size = 13"][i];
        nodes.push((
            run,
            NodeSpec::run(text).laid_out(7.0).bounds(
                180.0,
                220.0 + i as f64 * 18.0,
                600.0,
                238.0 + i as f64 * 18.0,
            ),
        ));
    }
    let mut viewport_spec = NodeSpec::new(Role::MultilineTextInput)
        .label("config.toml")
        .children(&viewport_runs)
        .bounds(180.0, 220.0, 600.0, 256.0)
        .actions(&[Action::Focus, Action::SetValue, Action::SetTextSelection]);
    viewport_spec.direction = Some(TextDirection::LeftToRight);
    viewport_spec.selection = Some(((40, 1), (41, 4)));
    nodes.push((viewport, viewport_spec));
    let mut status_spec = NodeSpec::new(Role::Status)
        .label("Saved")
        .bounds(180.0, 380.0, 600.0, 400.0);
    status_spec.description = Some("Editor status".to_owned());
    status_spec.live = Some(Live::Polite);
    nodes.push((status, status_spec));
    let mut button_spec = NodeSpec::new(Role::Button)
        .label("Reset")
        .bounds(500.0, 350.0, 600.0, 374.0)
        .actions(&[Action::Click, Action::Focus]);
    button_spec.busy = !on;
    nodes.push((button, button_spec));
    nodes.push((
        GROUP,
        NodeSpec::new(Role::Group)
            .label("Font")
            .children(&[
                heading, label, switch, slider, field, doc, viewport, status, button,
            ])
            .bounds(170.0, 0.0, 610.0, 400.0),
    ));
    let mut app = NodeSpec::new(Role::Application)
        .label("Settings")
        .children(&[NAV, GROUP])
        .bounds(0.0, 0.0, 610.0, 400.0);
    app.transform = Some((24.0, 48.0, scale));
    nodes.push((APP, app));
    nodes.push((
        ROOT,
        NodeSpec::new(Role::Window)
            .label("Settings")
            .children(&[APP]),
    ));
    Snapshot::new(nodes, slider)
}

fn settings() -> Scenario {
    let mut s = Script::new("fixture settings");
    let mut tree = settings_tree(0.5, false, "work", 1.0);
    s.open(0, &tree, true);
    s.bounds(
        0,
        Rect::new(40.0, 40.0, 700.0, 520.0),
        Rect::new(40.0, 68.0, 700.0, 520.0),
    );
    tree = settings_tree(0.55, false, "work", 1.0);
    s.update(0, &tree);
    tree = settings_tree(0.55, true, "work", 1.0);
    tree.focus = 22;
    s.update(0, &tree);
    // Typing into the name field, focused.
    for name in ["work2", "wor", ""] {
        tree = settings_tree(0.55, true, name, 1.0);
        tree.focus = 24;
        s.update(0, &tree);
    }
    // The window moves to a 2x display: every transform doubles.
    tree = settings_tree(0.55, true, "", 2.0);
    tree.focus = 24;
    s.update(0, &tree);
    // Another page is selected and the label becomes the read-out.
    tree.node(100).selected = Some(false);
    tree.node(101).selected = Some(true);
    tree.node(21).value = Some("Keys".to_owned());
    tree.focus = 101;
    s.update(0, &tree);
    // The viewport's selection collapses, and the status speaks.
    tree.node(27).selection = Some(((41, 9), (41, 9)));
    tree.node(28).label = Some("Saved config.toml".to_owned());
    tree.focus = 27;
    s.update(0, &tree);
    // Read-only and disabled controls.
    tree.node(24).read_only = true;
    tree.node(23).disabled = true;
    tree.node(22).disabled = true;
    s.update(0, &tree);
    s.done()
}

fn palette_tree(query: &str, rows: &[&str], selected: usize) -> Snapshot {
    let list = u64::MAX;
    let mut nodes: Vec<(u64, NodeSpec)> = Vec::new();
    let mut ids = Vec::new();
    for (i, text) in rows.iter().enumerate() {
        let id = i as u64 + 1;
        let mut spec = NodeSpec::new(Role::MenuItem)
            .label(text)
            .bounds(100.0, 60.0 + i as f64 * 22.0, 500.0, 82.0 + i as f64 * 22.0)
            .actions(&[Action::Focus, Action::Click]);
        spec.selected = Some(i == selected);
        spec.description = Some(format!("{query}: {i}"));
        nodes.push((id, spec));
        ids.push(id);
    }
    nodes.push((
        list,
        NodeSpec::new(Role::ListBox)
            .label("Commands")
            .children(&ids)
            .bounds(100.0, 60.0, 500.0, 300.0),
    ));
    let mut window = NodeSpec::new(Role::Window)
        .label("Command palette")
        .children(&[list]);
    window.live = Some(Live::Polite);
    nodes.push((ROOT, window));
    let focus = ids.get(selected).copied().unwrap_or(list);
    Snapshot::new(nodes, focus)
}

fn palette() -> Scenario {
    let mut s = Script::new("fixture palette");
    let all = ["New tab", "New window", "Split right", "Split down"];
    let mut tree = palette_tree("", &all, 0);
    s.open(0, &tree, true);
    tree = palette_tree("", &all, 2);
    s.update(0, &tree);
    tree = palette_tree("sp", &all[2..], 0);
    s.update(0, &tree);
    tree = palette_tree("spx", &[], 0);
    s.update(0, &tree);
    tree = palette_tree("", &all, 3);
    s.update(0, &tree);
    s.done()
}

fn two_windows() -> Scenario {
    let mut s = Script::new("fixture two windows");
    let first = grid(&["$ top\n", "$ "], Some((1, 2)), TAB_BASE);
    let second = settings_tree(0.3, true, "home", 1.0);
    s.open(0, &first, true);
    s.open(1, &second, false);
    s.focus(0, false);
    s.focus(1, true);
    s.close(0);
    let third = palette_tree("", &["One", "Two"], 1);
    s.open(2, &third, false);
    s.focus(1, false);
    s.focus(2, true);
    s.close(1);
    s.close(2);
    s.done()
}

fn unicode() -> Scenario {
    let mut s = Script::new("fixture unicode text");
    // Multibyte characters, a combining sequence, an emoji, a CRLF, and
    // runs laid out in every direction.
    let lines = [
        "caf\u{e9} na\u{ef}ve r\u{e9}sum\u{e9}\n",
        "e\u{301}clair \u{4e2d}\u{6587} \u{1f642} ok\r\n",
        "\u{5d0}\u{5d1}\u{5d2} \u{5d3}\n",
        "down",
    ];
    let mut tree = grid(&lines, Some((1, 3)), TAB_BASE);
    tree.node(ROW_BASE + 512).direction = Some(TextDirection::RightToLeft);
    tree.node(ROW_BASE + 768).direction = Some(TextDirection::TopToBottom);
    s.open(0, &tree, true);
    tree.node(TERMINAL).selection = Some(((ROW_BASE, 2), (ROW_BASE + 256, 6)));
    s.update(0, &tree);
    // An edit inside the multibyte run, keeping its neighbours.
    let edited = [
        lines[0],
        "e\u{301}clair \u{4e2d}\u{6587}\u{6587} \u{1f642}\u{1f642} ok\r\n",
        lines[2],
        lines[3],
    ];
    let mut next = grid(&edited, Some((1, 9)), TAB_BASE);
    next.node(ROW_BASE + 512).direction = Some(TextDirection::RightToLeft);
    next.node(ROW_BASE + 768).direction = Some(TextDirection::BottomToTop);
    s.update(0, &next);
    s.done()
}

fn hyperlink_and_dialog_root() -> Scenario {
    let mut s = Script::new("fixture link and dialog root");
    let link = 5;
    let label = 6;
    let button = 7;
    let mut link_spec = NodeSpec::new(Role::Link)
        .children(&[label])
        .bounds(10.0, 10.0, 90.0, 30.0)
        .actions(&[Action::Click, Action::Focus]);
    link_spec.url = Some("https://example.org/aterm".to_owned());
    let mut label_spec = NodeSpec::new(Role::Label).bounds(10.0, 10.0, 90.0, 30.0);
    label_spec.value = Some("aterm docs".to_owned());
    // A button named by its label child, not by a label of its own.
    let button_spec = NodeSpec::new(Role::Button)
        .children(&[label + 10])
        .bounds(10.0, 40.0, 90.0, 60.0)
        .actions(&[Action::Click]);
    let mut button_label = NodeSpec::new(Role::Label);
    button_label.value = Some("Open".to_owned());
    let mut tree = Snapshot::new(
        [
            (link, link_spec),
            (label, label_spec),
            (button, button_spec),
            (label + 10, button_label),
            (
                ROOT,
                NodeSpec::new(Role::AlertDialog)
                    .label("Links")
                    .children(&[link, button]),
            ),
        ],
        link,
    );
    s.open(0, &tree, true);
    tree.node(link).url = Some("https://example.org/other".to_owned());
    tree.node(label).value = Some("other docs".to_owned());
    s.update(0, &tree);
    tree.node(link).url = None;
    tree.node(label + 10).value = Some("Close".to_owned());
    tree.focus = button;
    s.update(0, &tree);
    // The root stops being a window: a group root has no window events.
    tree.node(ROOT).role = Role::Group;
    s.update(0, &tree);
    s.focus(0, false);
    s.done()
}

// --- generated -------------------------------------------------------------------

/// xorshift64*: the same numbers on every machine.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

const TEXT_ROLES: &[Role] = &[
    Role::Terminal,
    Role::Document,
    Role::TextInput,
    Role::SearchInput,
    Role::MultilineTextInput,
    Role::Label,
];
const LEAF_ROLES: &[Role] = &[
    Role::Button,
    Role::Switch,
    Role::Slider,
    Role::ProgressIndicator,
    Role::Status,
    Role::Alert,
    Role::Heading,
    Role::Label,
    Role::Link,
    Role::Unknown,
];
const CONTAINER_ROLES: &[Role] = &[
    Role::Group,
    Role::Application,
    Role::Navigation,
    Role::AlertDialog,
];
const SELECT_ROLES: &[(Role, Role)] = &[
    (Role::TabList, Role::Tab),
    (Role::ListBox, Role::MenuItem),
    (Role::Menu, Role::MenuItem),
    (Role::Menu, Role::MenuItemRadio),
    (Role::List, Role::MenuItem),
];
const ACTIONS: &[Action] = &[
    Action::Click,
    Action::Focus,
    Action::SetValue,
    Action::ScrollUp,
    Action::ScrollDown,
    Action::SetTextSelection,
    Action::ReplaceSelectedText,
];
const PIECES: &[&str] = &[
    "a",
    "b",
    "c",
    "x",
    "y",
    "z",
    "1",
    "2",
    " ",
    " ",
    " ",
    "-",
    ".",
    "\u{e9}",
    "\u{df}",
    "\u{4e2d}",
    "\u{1f642}",
    "e\u{301}",
];
const WORDS: &[&str] = &["ok", "save", "zsh", "", "Find", "logs", "\u{e9}t\u{e9}"];

struct Generator {
    rng: Rng,
    next_id: u64,
}

impl Generator {
    fn id(&mut self) -> u64 {
        self.next_id += 1 + self.rng.below(3) as u64;
        // Now and then an id from aterm-gui's high ranges.
        if self.rng.chance(15) {
            self.next_id + (1 << 48)
        } else {
            self.next_id
        }
    }

    fn word(&mut self) -> String {
        self.rng.pick(WORDS).to_owned()
    }

    fn rect(&mut self) -> Rect {
        let x = self.rng.below(400) as f64;
        let y = self.rng.below(300) as f64;
        let w = 1 + self.rng.below(200);
        let h = 1 + self.rng.below(60);
        Rect::new(x, y, x + w as f64, y + h as f64)
    }

    fn chars(&mut self, end_line: bool) -> Vec<String> {
        let n = self.rng.below(14);
        let mut chars: Vec<String> = (0..n).map(|_| self.rng.pick(PIECES).to_owned()).collect();
        if end_line {
            chars.push(if self.rng.chance(20) { "\r\n" } else { "\n" }.to_owned());
        }
        chars
    }

    /// A run: ending in a line break, or (a viewport's line, `end_line`
    /// false) never empty unless it is its document's last.
    fn run(&mut self, end_line: bool, last: bool) -> NodeSpec {
        let mut spec = NodeSpec::new(Role::TextRun);
        let mut chars = self.chars(end_line);
        if chars.is_empty() && !last {
            chars.push(self.rng.pick(PIECES).to_owned());
        }
        spec.set_text(chars);
        if self.rng.chance(70) {
            spec.cell = Some(4.0 + self.rng.below(6) as f32);
            spec.bounds = Some(self.rect());
        }
        spec.line_break = self.rng.chance(20);
        spec
    }

    /// Random properties on `spec`, any role.
    fn decorate(&mut self, spec: &mut NodeSpec) {
        if self.rng.chance(60) {
            spec.label = Some(self.word());
        }
        if self.rng.chance(25) {
            spec.description = Some(self.word());
        }
        if spec.chars.is_none() && self.rng.chance(20) {
            spec.value = Some(self.word());
        }
        if self.rng.chance(60) {
            spec.bounds = Some(self.rect());
        }
        if self.rng.chance(10) {
            spec.transform = Some((
                self.rng.below(50) as f64,
                self.rng.below(50) as f64,
                self.rng.pick(&[1.0, 1.5, 2.0]),
            ));
        }
        spec.actions = ACTIONS
            .iter()
            .copied()
            .filter(|_| self.rng.chance(25))
            .collect();
        if self.rng.chance(15) {
            spec.toggled = Some(
                self.rng
                    .pick(&[Toggled::True, Toggled::False, Toggled::Mixed]),
            );
        }
        if self.rng.chance(15) {
            spec.live = Some(self.rng.pick(&[Live::Off, Live::Polite, Live::Assertive]));
        }
        if matches!(spec.role, Role::Slider | Role::ProgressIndicator) || self.rng.chance(10) {
            if self.rng.chance(80) {
                spec.numeric = Some(self.rng.below(100) as f64);
            }
            if self.rng.chance(50) {
                spec.min = Some(0.0);
                spec.max = Some(100.0);
            }
            if self.rng.chance(30) {
                spec.step = Some(5.0);
            }
        }
        if spec.role == Role::Link && self.rng.chance(50) {
            spec.url = Some(format!("https://example.org/{}", self.rng.below(9)));
        }
        spec.popup = self.rng.chance(8);
        spec.invalid = self.rng.chance(8);
        spec.expanded = self.rng.chance(8).then(|| self.rng.chance(50));
        spec.disabled = self.rng.chance(10);
        spec.read_only = self.rng.chance(15);
        spec.busy = self.rng.chance(5);
        if self.rng.chance(15) {
            spec.direction = Some(self.rng.pick(&[
                TextDirection::LeftToRight,
                TextDirection::LeftToRight,
                TextDirection::RightToLeft,
                TextDirection::TopToBottom,
                TextDirection::BottomToTop,
            ]));
        }
    }

    /// A random subtree under a new id; its nodes go into `nodes`.
    fn subtree(&mut self, nodes: &mut BTreeMap<u64, NodeSpec>, depth: usize) -> u64 {
        let id = self.id();
        let kind = self.rng.below(if depth >= 2 { 2 } else { 4 });
        let mut spec;
        match kind {
            0 => {
                spec = NodeSpec::new(self.rng.pick(LEAF_ROLES));
                self.decorate(&mut spec);
            }
            1 => {
                spec = NodeSpec::new(self.rng.pick(TEXT_ROLES));
                self.decorate(&mut spec);
                let runs = 1 + self.rng.below(4);
                // A viewport's lines carry no line break (aterm-gui's
                // Settings editor); a grid's rows end in one.
                let viewport = self.rng.chance(30);
                for i in 0..runs {
                    let run_id = self.id();
                    let run = self.run(!viewport && i + 1 < runs, i + 1 == runs);
                    spec.children.push(run_id);
                    nodes.insert(run_id, run);
                }
                if spec.direction.is_none() && self.rng.chance(70) {
                    spec.direction = Some(TextDirection::LeftToRight);
                }
                self.select(nodes, &mut spec);
            }
            2 => {
                let (container, item) = self.rng.pick(SELECT_ROLES);
                spec = NodeSpec::new(container);
                self.decorate(&mut spec);
                for _ in 0..self.rng.below(5) {
                    let item_id = self.id();
                    let mut item_spec = NodeSpec::new(item);
                    self.decorate(&mut item_spec);
                    if self.rng.chance(70) {
                        item_spec.selected = Some(self.rng.chance(40));
                    }
                    spec.children.push(item_id);
                    nodes.insert(item_id, item_spec);
                }
            }
            _ => {
                spec = NodeSpec::new(self.rng.pick(CONTAINER_ROLES));
                self.decorate(&mut spec);
                for _ in 0..self.rng.below(4) {
                    let child = self.subtree(nodes, depth + 1);
                    spec.children.push(child);
                }
            }
        }
        nodes.insert(id, spec);
        id
    }

    /// A random caret or selection in the text document `spec`, or none.
    fn select(&mut self, nodes: &BTreeMap<u64, NodeSpec>, spec: &mut NodeSpec) {
        let runs: Vec<(u64, usize)> = spec
            .children
            .iter()
            .filter_map(|id| {
                let run = nodes.get(id)?;
                Some((*id, run.chars.as_ref()?.len()))
            })
            .collect();
        if runs.is_empty() || self.rng.chance(20) {
            spec.selection = None;
            return;
        }
        let (a, a_len) = self.rng.pick(&runs);
        let anchor = (a, self.rng.below(a_len + 1));
        let focus = if self.rng.chance(50) {
            anchor
        } else {
            let (f, f_len) = self.rng.pick(&runs);
            (f, self.rng.below(f_len + 1))
        };
        spec.selection = Some((anchor, focus));
    }

    fn tree(&mut self) -> Snapshot {
        let mut nodes = BTreeMap::new();
        let mut root = NodeSpec::new(if self.rng.chance(85) {
            Role::Window
        } else {
            self.rng.pick(&[Role::AlertDialog, Role::Group])
        });
        self.decorate(&mut root);
        root.transform = None;
        for _ in 0..1 + self.rng.below(5) {
            let child = self.subtree(&mut nodes, 0);
            root.children.push(child);
        }
        nodes.insert(ROOT, root);
        let ids: Vec<u64> = nodes.keys().copied().collect();
        let focus = self.rng.pick(&ids);
        Snapshot { nodes, focus }
    }

    /// One random change to `tree`.
    fn mutate(&mut self, tree: &mut Snapshot) {
        let ids: Vec<u64> = tree.nodes.keys().copied().collect();
        let id = self.rng.pick(&ids);
        match self.rng.below(10) {
            // Text edits in a run, its document's selection kept valid.
            0 | 1 => {
                let runs: Vec<u64> = ids
                    .iter()
                    .copied()
                    .filter(|i| tree.nodes[i].chars.is_some())
                    .collect();
                if runs.is_empty() {
                    return;
                }
                let run = self.rng.pick(&runs);
                let mut chars = tree.nodes[&run].chars.clone().unwrap_or_default();
                let line_end = chars.last().is_some_and(|c| c.ends_with('\n'));
                let body = chars.len() - usize::from(line_end);
                if self.rng.chance(50) || body == 0 {
                    let at = self.rng.below(body + 1);
                    for (k, piece) in self.chars(false).into_iter().enumerate() {
                        chars.insert(at + k, piece);
                    }
                } else {
                    // A run without a line break keeps a character: an empty
                    // run inside a document sends the registry crates'
                    // paragraph search into an endless loop.
                    let keep = usize::from(!line_end);
                    if body <= keep {
                        return;
                    }
                    let at = self.rng.below(body - keep);
                    let n = 1 + self.rng.below(body - keep - at);
                    chars.drain(at..at + n);
                }
                tree.node(run).set_text(chars);
                let len = tree.nodes[&run].chars.as_ref().map_or(0, Vec::len);
                for spec in tree.nodes.values_mut() {
                    if let Some(((ar, ai), (fr, fi))) = spec.selection.as_mut() {
                        if *ar == run {
                            *ai = (*ai).min(len);
                        }
                        if *fr == run {
                            *fi = (*fi).min(len);
                        }
                    }
                }
            }
            // A new caret or selection in some document.
            2 => {
                let docs: Vec<u64> = ids
                    .iter()
                    .copied()
                    .filter(|i| TEXT_ROLES.contains(&tree.nodes[i].role))
                    .collect();
                if docs.is_empty() {
                    return;
                }
                let doc = self.rng.pick(&docs);
                let mut spec = tree.nodes[&doc].clone();
                self.select(&tree.nodes, &mut spec);
                tree.nodes.insert(doc, spec);
            }
            // New properties on a node.
            3 | 4 => {
                let mut spec = tree.nodes[&id].clone();
                let children = std::mem::take(&mut spec.children);
                let chars = spec.chars.clone();
                let selection = spec.selection;
                self.decorate(&mut spec);
                spec.children = children;
                spec.chars = chars;
                spec.selection = selection;
                if spec.role != Role::TextRun || id == ROOT {
                    spec.cell = tree.nodes[&id].cell;
                }
                if id == ROOT {
                    spec.transform = None;
                }
                tree.nodes.insert(id, spec);
            }
            // A new subtree under a non-run node.
            5 => {
                if tree.nodes[&id].role == Role::TextRun
                    || TEXT_ROLES.contains(&tree.nodes[&id].role)
                {
                    return;
                }
                let child = self.subtree(&mut tree.nodes, 1);
                let spec = tree.node(id);
                let at = self.rng.below(spec.children.len() + 1);
                spec.children.insert(at, child);
            }
            // A subtree removed (never the root, never a document's only run).
            6 => {
                if id == ROOT {
                    return;
                }
                let Some(parent) = tree
                    .nodes
                    .iter()
                    .find(|(_, spec)| spec.children.contains(&id))
                    .map(|(&p, _)| p)
                else {
                    return;
                };
                let in_doc = TEXT_ROLES.contains(&tree.nodes[&parent].role);
                if in_doc && tree.nodes[&parent].children.len() < 2 {
                    return;
                }
                let gone = tree.subtree(id);
                for g in &gone {
                    tree.nodes.remove(g);
                }
                tree.node(parent).children.retain(|&c| c != id);
                for spec in tree.nodes.values_mut() {
                    if let Some(((ar, _), (fr, _))) = spec.selection
                        && (gone.contains(&ar) || gone.contains(&fr))
                    {
                        spec.selection = None;
                    }
                }
                if gone.contains(&tree.focus) {
                    tree.focus = parent;
                }
            }
            // Children reordered (never a document's runs: an empty last
            // run must stay last, see above).
            7 => {
                let spec = tree.node(id);
                if spec.children.len() > 1 && !TEXT_ROLES.contains(&spec.role) {
                    spec.children.reverse();
                }
            }
            // The focus moves.
            _ => tree.focus = id,
        }
    }
}

fn generated(seed: u64) -> Scenario {
    let mut g = Generator {
        rng: Rng::new(seed + 1),
        next_id: 0,
    };
    let mut s = Script::new(&format!("generated {seed}"));
    let mut tree = g.tree();
    let windows = 1 + usize::from(g.rng.chance(25));
    let mut others = Vec::new();
    s.open(0, &tree, g.rng.chance(70));
    for w in 1..windows {
        let other = g.tree();
        s.open(w, &other, false);
        others.push(other);
    }
    for _ in 0..12 {
        match g.rng.below(12) {
            0 => {
                let w = g.rng.below(windows);
                s.focus(w, g.rng.chance(50));
            }
            1 => {
                let outer = g.rect();
                let inner = Rect::new(outer.x0 + 2.0, outer.y0 + 20.0, outer.x1 - 2.0, outer.y1);
                s.bounds(0, outer, inner);
            }
            2 => s.resend(0, &tree),
            _ => {
                for _ in 0..1 + g.rng.below(3) {
                    g.mutate(&mut tree);
                }
                s.update(0, &tree);
            }
        }
    }
    for w in (1..windows).rev() {
        s.close(w);
    }
    s.done()
}

// --- blank lines ---------------------------------------------------------------

/// The runs the blank-line documents are made of: a blank line as aterm-gui's
/// Settings and Manual viewports send one (an empty run: their runs carry no
/// line break), two words, a line ending in a break, and a break alone.
const BLANK_LINE_RUNS: [&str; 4] = ["", "a b", "x\n", "\n"];

/// The text input of a blank-line document's window.
const BLANK_LINE_INPUT: u64 = 1;

/// A document of the blank-line table: one run per line, with word starts
/// (as the corpus's runs carry) or without (as aterm-gui's viewports send).
#[derive(Clone, Debug)]
pub(crate) struct Document {
    lines: Vec<&'static str>,
    words: bool,
}

/// Every document of one to three runs over [`BLANK_LINE_RUNS`], with word
/// starts and without: the shapes accesskit_consumer 0.37.0's paragraph
/// search never returned from, and the ones around them. The fixtures and
/// generated scenarios keep an empty run last, where it hangs nothing.
pub(crate) fn blank_line_documents() -> Vec<Document> {
    let mut all = Vec::new();
    for words in [true, false] {
        let mut shapes: Vec<Vec<&'static str>> = vec![Vec::new()];
        for _ in 0..3 {
            shapes = shapes
                .into_iter()
                .flat_map(|shape| {
                    BLANK_LINE_RUNS.iter().map(move |run| {
                        let mut longer = shape.clone();
                        longer.push(*run);
                        longer
                    })
                })
                .collect();
            all.extend(shapes.iter().map(|lines| Document {
                lines: lines.clone(),
                words,
            }));
        }
    }
    all
}

impl Document {
    /// The document as a row key: its runs, and `words` when they carry
    /// word starts.
    pub(crate) fn label(&self) -> String {
        format!("{:?}{}", self.lines, if self.words { " words" } else { "" })
    }

    /// Its length in characters: the last text offset.
    pub(crate) fn len(&self) -> i32 {
        self.lines.iter().map(|l| l.chars().count()).sum::<usize>() as i32
    }

    /// Its window's first tree: a window whose one child, focused, is a
    /// multi-line text input of one run per line.
    pub(crate) fn update(&self) -> TreeUpdate {
        let runs: Vec<u64> = (0..self.lines.len() as u64)
            .map(|i| BLANK_LINE_INPUT + 1 + i)
            .collect();
        let mut nodes: Vec<(u64, NodeSpec)> = Vec::new();
        for (&id, line) in runs.iter().zip(&self.lines) {
            let mut run = NodeSpec::run(line).laid_out(8.0);
            if !self.words {
                run.word_starts.clear();
            }
            nodes.push((id, run));
        }
        nodes.push((
            BLANK_LINE_INPUT,
            NodeSpec::new(Role::MultilineTextInput)
                .label("Settings")
                .children(&runs)
                .bounds(0.0, 0.0, 320.0, 16.0 * runs.len() as f64),
        ));
        nodes.push((
            ROOT,
            NodeSpec::new(Role::Window)
                .label("Settings")
                .children(&[BLANK_LINE_INPUT]),
        ));
        Snapshot::new(nodes, BLANK_LINE_INPUT).full()
    }
}
