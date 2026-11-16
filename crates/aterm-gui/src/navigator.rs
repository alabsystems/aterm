// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The navigator (tracker ST-04, `docs/DESIGN-streams-2026-11-12.md` §3): one native page listing
//! every live session of every window of this aterm. The person's own tabs come first, in the
//! strip's order, each with the sessions created under it; then, under the machine's name, every
//! session no tab of theirs leads to. A row reads one of five words ([`Word`]); a row with
//! sessions under it starts closed and says how many.
//!
//! The host ([`crate::app_navigator`]) publishes one arranged [`NavTree`] to the page and every
//! view of it; the page holds no session and reads no clock. A row on screen is built whole, a
//! row off it is one node, so a list of hundreds compiles one screen.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::native_app::{
    AppDescriptor, AppEvent, AppIcon, AppIndicators, AppKind, AppPresentation, CloseReadiness,
    CloseRequest, Command, CommonViewState, DamageRegion, EventResult, NativeAppModel,
    SemanticInput, TextInputEvent, UpdateCx, ViewCx,
};
use crate::native_text_input::TextInputState;
use crate::native_ui::{
    ActionId, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length, SemanticRole,
    SemanticValue, StyleRef, TextFieldSpec, TextSpec, UiContent, UiKey, UiNode, UiTree,
};
use crate::native_workers::{one_line, text_width};
use crate::task_record::TaskState;

/// The width of the navigator's pane in a split, in cells.
pub(crate) const SPLIT_COLS: u16 = 30;
/// The filter field's key, and the action that puts the keyboard in it.
pub(crate) const FILTER: &str = "navigator/filter";
/// A row's press, before its session's local id: go to that session.
pub(crate) const ROW: &str = "navigator/row/";
/// A row's disclosure button, before its session's local id.
const FOLD: &str = "navigator/fold/";
/// Open the selected row, or step to the first session under it.
pub(crate) const STEP_IN: &str = "navigator/step-in";
/// Close the selected row, or step to the row it is under.
pub(crate) const STEP_OUT: &str = "navigator/step-out";

/// The page's name where no machine's name is published yet.
const NAVIGATOR_TITLE: &str = "Navigator";
const NO_SESSIONS: &str = "No live sessions";
const NO_MATCH: &str = "No sessions match";
const FILTER_LABEL: &str = "Filter";

/// The one word a row reads (decision ST-D23). A wait on a person is a case of `stuck`: nothing
/// counts, sorts or marks a row for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Word {
    Working,
    Quiet,
    Idle,
    Ended,
    Stuck,
}

impl Word {
    pub(crate) const ALL: [Self; 5] = [
        Self::Working,
        Self::Quiet,
        Self::Idle,
        Self::Ended,
        Self::Stuck,
    ];

    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Quiet => "quiet",
            Self::Idle => "idle",
            Self::Ended => "ended",
            Self::Stuck => "stuck",
        }
    }

    /// `stuck` is the page's one word in colour; the four calm words share an ink.
    const fn style(self) -> StyleRef {
        match self {
            Self::Stuck => StyleRef::Danger,
            Self::Working | Self::Quiet | Self::Idle | Self::Ended => StyleRef::Plain,
        }
    }
}

/// What a session's word is read from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WordFacts<'a> {
    /// Its task's state, where it has a task.
    pub(crate) task: Option<TaskState>,
    /// A person took its task off Autopilot (`why=off`, `why=off-ctl`): the task's last state
    /// is `failed`, and nothing failed.
    pub(crate) taken_off: bool,
    /// It stopped to ask a person: the need the harness published for it. A box the harness
    /// answers itself is not one.
    pub(crate) asks: bool,
    /// What its program reads as now (`busy`, `question`, `prompt`, `wall:…`, `idle`,
    /// `unknown`, or `-` for a shell with no agent): as Tab Status read it, or, where nothing
    /// observes it, as the program's own report says.
    pub(crate) agent_word: &'a str,
    /// Its shell has a command running: what Tab Status observed of it (the fact the tab's busy
    /// mark is lit from), or, where nothing observes it, the shell's own word.
    pub(crate) running: bool,
}

/// A session's word, the first that holds in the order stuck, ended, working, quiet, idle.
/// A row is a conversation, never a job with a last state: a turn in progress outranks a task
/// that failed, and a task a person took off Autopilot did not fail.
pub(crate) fn word_of(facts: &WordFacts<'_>) -> Word {
    let busy = facts.agent_word == "busy";
    let failed = matches!(facts.task, Some(TaskState::Failed)) && !facts.taken_off && !busy;
    if facts.asks || failed || matches!(facts.task, Some(TaskState::Stalled)) {
        Word::Stuck
    } else if matches!(facts.task, Some(TaskState::Exited)) {
        Word::Ended
    } else if matches!(facts.task, Some(TaskState::Working))
        || busy
        || (facts.agent_word == "-" && facts.running)
    {
        Word::Working
    } else if facts.task.is_some() || facts.agent_word != "-" {
        Word::Quiet
    } else {
        Word::Idle
    }
}

/// One session's row as the host read it.
#[derive(Clone, Debug)]
pub(crate) struct NavRow {
    pub(crate) session: u64,
    pub(crate) title: String,
    /// The agent's program (`claude`, `codex`); a person's shell has none.
    pub(crate) agent: Option<String>,
    pub(crate) word: Word,
    /// The latest words the session's stream said.
    pub(crate) line: Option<String>,
    /// When the stream last moved, in Unix ms (0: never).
    pub(crate) moved_ms: u64,
    /// The session that created this one, where it is known and live.
    pub(crate) creator: Option<u64>,
}

/// When a stream moved is left out: a row that printed again is the same row until one of its
/// words differs, and the page words the time itself, once a minute ([`age_words`]).
impl PartialEq for NavRow {
    fn eq(&self, other: &Self) -> bool {
        self.session == other.session
            && self.title == other.title
            && self.agent == other.agent
            && self.word == other.word
            && self.line == other.line
            && self.creator == other.creator
    }
}

impl NavRow {
    /// Every field, for a screen reader and `inspect`: the painted texts are cut to their boxes.
    fn spoken(&self, listed: Listed, clock_ms: u64) -> String {
        let mut said = format!("{}, {}", self.title, self.word.word());
        if listed.under > 0 && !listed.open {
            said.push_str(", ");
            said.push_str(&under_words(listed.under));
        }
        if let Some(agent) = &self.agent {
            said.push_str(", ");
            said.push_str(agent);
        }
        let age = age_words(clock_ms, self.moved_ms);
        if !age.is_empty() {
            said.push_str(", ");
            said.push_str(&age);
        }
        if let Some(line) = &self.line {
            said.push_str(", ");
            said.extend(line.split_whitespace().flat_map(|word| [word, " "]));
            said.truncate(said.trim_end().len());
        }
        said
    }
}

/// How long since a stream last moved, as one coarse word: `now`, `12m`, `3h`, `2d`; nothing for
/// a stream that never moved.
pub(crate) fn age_words(now_ms: u64, moved_ms: u64) -> String {
    const MINUTE: u64 = 60_000;
    if moved_ms == 0 {
        return String::new();
    }
    let minutes = now_ms.saturating_sub(moved_ms) / MINUTE;
    match minutes {
        0 => "now".to_owned(),
        1..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    }
}

/// A closed row's count of the sessions under it: a count and nothing else.
pub(crate) fn under_words(n: usize) -> String {
    format!("{n} under")
}

/// Which of the list's two groups a row is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Group {
    /// A person's tab, or a session created under one.
    Pinned,
    /// A session no tab of the person's leads to, under the machine's name.
    Machine,
}

/// One row in the arrangement.
#[derive(Clone, Debug)]
pub(crate) struct NavEntry {
    pub(crate) row: Arc<NavRow>,
    pub(crate) group: Group,
    /// How many rows it sits under.
    pub(crate) depth: u16,
    /// How many rows sit under it, at every depth: they are the entries that follow it.
    pub(crate) under: u32,
    /// The entry it sits under.
    pub(crate) parent: Option<usize>,
}

/// Every live session, arranged: a forest in the order the page lists it, each entry followed by
/// the entries under it.
#[derive(Clone, Debug, Default)]
pub(crate) struct NavTree {
    pub(crate) machine: String,
    pub(crate) entries: Vec<NavEntry>,
    at: HashMap<u64, usize>,
}

impl NavTree {
    #[cfg(test)]
    pub(crate) fn sessions(&self) -> Vec<u64> {
        self.entries.iter().map(|entry| entry.row.session).collect()
    }

    pub(crate) fn index_of(&self, session: u64) -> Option<usize> {
        self.at.get(&session).copied()
    }

    /// The same arrangement with `rows` in place of the rows of their sessions.
    pub(crate) fn with_rows(&self, rows: &HashMap<u64, Arc<NavRow>>) -> Self {
        let mut tree = self.clone();
        for entry in &mut tree.entries {
            if let Some(row) = rows.get(&entry.row.session) {
                entry.row = Arc::clone(row);
            }
        }
        tree
    }

    /// The sessions directly under entry `parent` (`None`: the roots of `group`), in order.
    fn siblings(&self, parent: Option<usize>, group: Group) -> Vec<u64> {
        self.entries
            .iter()
            .filter(|entry| entry.parent == parent && (parent.is_some() || entry.group == group))
            .map(|entry| entry.row.session)
            .collect()
    }

    fn place(
        &mut self,
        session: u64,
        group: Group,
        depth: u16,
        parent: Option<usize>,
        kin: &Kin<'_>,
    ) {
        if self.at.contains_key(&session) {
            return;
        }
        let Some(row) = kin.rows.get(&session) else {
            return;
        };
        let at = self.entries.len();
        self.at.insert(session, at);
        self.entries.push(NavEntry {
            row: Arc::clone(row),
            group,
            depth,
            under: 0,
            parent,
        });
        if let Some(children) = kin.children.get(&session) {
            for child in children {
                self.place(*child, group, depth.saturating_add(1), Some(at), kin);
            }
        }
        self.entries[at].under = u32::try_from(self.entries.len() - at - 1).unwrap_or(u32::MAX);
    }
}

struct Kin<'a> {
    rows: &'a HashMap<u64, Arc<NavRow>>,
    children: HashMap<u64, Vec<u64>>,
}

/// Arrange the sessions of `rows`: `pinned` (the person's tabs, in window, strip and pane order)
/// first, each with the sessions of `parked` created under it through live creators; then every
/// session of `parked` no pinned session leads to, each with its own. A pinned session stays at
/// its tab's place whoever created it; a session with no row is not listed; a cycle of creators
/// breaks at its first session, after the roots. Over `held` (the arrangement the page shows
/// while it has the keys) every row keeps its place among its siblings and a newcomer follows
/// them.
pub(crate) fn arrange(
    machine: &str,
    pinned: &[u64],
    parked: &[u64],
    rows: &HashMap<u64, Arc<NavRow>>,
    held: Option<&NavTree>,
) -> NavTree {
    let mut walked = HashSet::with_capacity(pinned.len() + parked.len());
    let mut once = |walk: &[u64]| -> Vec<u64> {
        walk.iter()
            .copied()
            .filter(|session| rows.contains_key(session) && walked.insert(*session))
            .collect()
    };
    let mut pinned = once(pinned);
    let parked = once(parked);
    let mut roots = Vec::new();
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for session in &parked {
        match rows[session]
            .creator
            .filter(|creator| creator != session && walked.contains(creator))
        {
            Some(creator) => children.entry(creator).or_default().push(*session),
            None => roots.push(*session),
        }
    }
    if let Some(held) = held {
        pinned = steady(&held.siblings(None, Group::Pinned), &pinned);
        roots = steady(&held.siblings(None, Group::Machine), &roots);
        for (parent, under) in &mut children {
            if let Some(at) = held.index_of(*parent) {
                *under = steady(&held.siblings(Some(at), Group::Pinned), under);
            }
        }
    }
    let kin = Kin { rows, children };
    let mut tree = NavTree {
        machine: machine.to_owned(),
        entries: Vec::with_capacity(pinned.len() + parked.len()),
        at: HashMap::with_capacity(pinned.len() + parked.len()),
    };
    for session in &pinned {
        tree.place(*session, Group::Pinned, 0, None, &kin);
    }
    for session in &roots {
        tree.place(*session, Group::Machine, 0, None, &kin);
    }
    // What is left names its creators in a ring that no root leads into.
    for session in &parked {
        tree.place(*session, Group::Machine, 0, None, &kin);
    }
    tree
}

/// `fresh` in the order `held` had: what both hold stays where it was, a newcomer follows.
pub(crate) fn steady(held: &[u64], fresh: &[u64]) -> Vec<u64> {
    let now: HashSet<u64> = fresh.iter().copied().collect();
    let was: HashSet<u64> = held.iter().copied().collect();
    held.iter()
        .filter(|session| now.contains(session))
        .chain(fresh.iter().filter(|session| !was.contains(session)))
        .copied()
        .collect()
}

/// What a page shows of some sessions' rows ([`NavigatorViewState::shows`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Shows {
    /// It paints some: the band, top and bottom, that holds every one it paints.
    Painted((f32, f32)),
    /// It lists some and paints none: each is a node off the list's screen, which a screen
    /// reader and `inspect` read and no pixel shows.
    Listed,
    /// It lists none: each is inside a closed row or outside the filter, and no node.
    Nothing,
}

/// What one view folds and filters the arrangement by.
pub(crate) struct Fold<'a> {
    /// The session the person is in: its row and each row above it start open.
    pub(crate) person_in: Option<u64>,
    /// The rows the person opened.
    pub(crate) opened: &'a BTreeSet<u64>,
    /// The rows the person closed, which stands over the default.
    pub(crate) closed: &'a BTreeSet<u64>,
    pub(crate) filter: &'a str,
}

/// One row a view lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    /// Its entry in the tree.
    pub(crate) at: usize,
    /// Whether the rows directly under it are listed.
    pub(crate) open: bool,
    /// How many sessions sit under it.
    pub(crate) under: usize,
}

/// The rows a view lists, in order: every row not inside a closed one; or, with a filter, every
/// row that matches it (its title, agent, word or latest words, in any case) with the rows above
/// it shown open, whatever was folded.
pub(crate) fn listed(tree: &NavTree, fold: &Fold<'_>) -> Vec<Listed> {
    let entries = &tree.entries;
    let needle = fold.filter.trim().to_lowercase();
    if !needle.is_empty() {
        let mut kept = vec![false; entries.len()];
        let mut holds = vec![false; entries.len()];
        for at in (0..entries.len()).rev() {
            kept[at] = kept[at] || matches(&entries[at].row, &needle);
            if kept[at]
                && let Some(parent) = entries[at].parent
            {
                kept[parent] = true;
                holds[parent] = true;
            }
        }
        return (0..entries.len())
            .filter(|at| kept[*at])
            .map(|at| Listed {
                at,
                open: holds[at],
                under: entries[at].under as usize,
            })
            .collect();
    }
    let mut on_the_way = HashSet::new();
    let mut above = fold.person_in.and_then(|session| tree.index_of(session));
    while let Some(at) = above {
        on_the_way.insert(at);
        above = entries[at].parent;
    }
    let mut rows = Vec::new();
    let mut at = 0;
    while at < entries.len() {
        let entry = &entries[at];
        let session = entry.row.session;
        let open = entry.under > 0
            && !fold.closed.contains(&session)
            && (fold.opened.contains(&session) || on_the_way.contains(&at));
        rows.push(Listed {
            at,
            open,
            under: entry.under as usize,
        });
        at += if open { 1 } else { 1 + entry.under as usize };
    }
    rows
}

fn matches(row: &NavRow, needle: &str) -> bool {
    let holds = |text: &str| text.to_lowercase().contains(needle);
    holds(&row.title)
        || row.agent.as_deref().is_some_and(holds)
        || row.word.word().contains(needle)
        || row.line.as_deref().is_some_and(holds)
}

/// What a press on the page asks the host for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NavAction {
    /// Show this session and give it the keys.
    Go(u64),
}

/// The list: the published arrangement, the minute the ages are worded at, and what the page
/// has asked for.
#[derive(Debug, Default)]
pub(crate) struct NavigatorApp {
    tree: Arc<NavTree>,
    clock_ms: u64,
    actions: VecDeque<NavAction>,
}

impl NavigatorApp {
    pub(crate) fn publish(&mut self, tree: Arc<NavTree>) {
        self.tree = tree;
    }

    pub(crate) fn tree(&self) -> &Arc<NavTree> {
        &self.tree
    }

    /// The time the ages are worded against. The page reads no clock of its own, so a capture
    /// and `inspect` say what the last frame said.
    pub(crate) fn set_clock(&mut self, now_ms: u64) {
        self.clock_ms = now_ms;
    }

    pub(crate) fn take_action(&mut self) -> Option<NavAction> {
        self.actions.pop_front()
    }

    /// The filter's typed text, where `key` is the filter field.
    pub(crate) fn field_input<'a>(
        view: &'a NavigatorViewState,
        key: &str,
    ) -> Option<&'a TextInputState> {
        (key == FILTER).then_some(&view.filter)
    }
}

/// One view of the list: its filter, its folds, its scroll and the row the keyboard is on.
#[derive(Debug)]
pub(crate) struct NavigatorViewState {
    pub(crate) common: CommonViewState,
    docked: bool,
    filter: TextInputState,
    scroll: f32,
    /// The selected row's session. It outlives the keyboard's move to the filter.
    cursor: Option<u64>,
    /// Whether a person's keyboard is on this view: its window's keys go to it and its window is
    /// the key one. Without it the selection is drawn dimmed.
    at_hand: bool,
    person_in: Option<u64>,
    opened: BTreeSet<u64>,
    closed: BTreeSet<u64>,
    /// Counts the rows a person opened or closed.
    folds: u64,
    /// The width and height the page is laid out in: the host's word ([`Self::lay`]) or the
    /// last render's.
    laid: Cell<Option<(f32, f32)>>,
    /// Whether the selected row was scrolled into the room once the page had a size.
    revealed: bool,
}

impl Default for NavigatorViewState {
    fn default() -> Self {
        Self {
            common: CommonViewState::default(),
            docked: false,
            filter: TextInputState::new(String::new()),
            scroll: 0.0,
            cursor: None,
            at_hand: true,
            person_in: None,
            opened: BTreeSet::new(),
            closed: BTreeSet::new(),
            folds: 0,
            laid: Cell::new(None),
            revealed: false,
        }
    }
}

impl NavigatorViewState {
    /// A view that opens on the session the person is in: its row selected, open, with the
    /// keys on it. The selection's wash says where the keys are; the keyboard's ring waits for
    /// the first key.
    pub(crate) fn starting_in(person_in: Option<u64>) -> Self {
        let mut view = Self {
            person_in,
            cursor: person_in,
            ..Self::default()
        };
        view.common.last_focus = person_in.map(|session| UiKey::new(format!("{ROW}{session}")));
        view
    }

    pub(crate) fn in_dock(person_in: Option<u64>) -> Self {
        Self {
            docked: true,
            at_hand: false,
            ..Self::starting_in(person_in)
        }
    }

    fn fold(&self) -> Fold<'_> {
        Fold {
            person_in: self.person_in,
            opened: &self.opened,
            closed: &self.closed,
            filter: self.filter.value(),
        }
    }

    /// The rows this view lists of `tree`.
    pub(crate) fn listing(&self, tree: &NavTree) -> Vec<Listed> {
        listed(tree, &self.fold())
    }

    /// The selected row's session.
    #[cfg(test)]
    pub(crate) fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    #[cfg(test)]
    pub(crate) fn set_filter(&mut self, filter: &str) {
        self.filter = TextInputState::new(filter.to_owned());
    }

    /// The session whose row or disclosure button the keyboard is on.
    fn focused_row(&self) -> Option<u64> {
        let key = self.common.last_focus.as_ref()?.as_str();
        key.strip_prefix(ROW)
            .or_else(|| key.strip_prefix(FOLD))?
            .parse()
            .ok()
    }

    fn in_filter(&self) -> bool {
        self.common
            .last_focus
            .as_ref()
            .is_some_and(|key| key.as_str() == FILTER)
    }

    fn frame(&self, tree: &NavTree) -> Option<Frame> {
        let (width, height) = self.laid.get()?;
        Some(Frame::of(tree, &self.fold(), width, height, self.docked))
    }

    /// Put the selection, and the keyboard, on `session`'s row and scroll it into the room.
    fn select(&mut self, session: u64, tree: &NavTree) {
        self.cursor = Some(session);
        self.common.last_focus = Some(UiKey::new(format!("{ROW}{session}")));
        self.common.focus_visible = true;
        self.reveal(session, tree);
    }

    /// Scroll so `session`'s row shows, once the page has been laid out.
    fn reveal(&mut self, session: u64, tree: &NavTree) {
        let Some(frame) = self.frame(tree) else {
            return;
        };
        if let Some(top) = frame.top_of(tree, session) {
            if top < self.scroll {
                self.scroll = top;
            } else if top + frame.m.row_h > self.scroll + frame.room {
                self.scroll = top + frame.m.row_h - frame.room;
            }
        }
        self.scroll = self.scroll.clamp(0.0, frame.limit());
    }

    /// The host's word on the size this view is laid out in. A view opens on the session the
    /// person is in before it has a size, so the first size scrolls that row into the room;
    /// every size keeps the scroll inside the list. Whether the list moved.
    pub(crate) fn lay(&mut self, tree: &NavTree, width: f32, height: f32) -> bool {
        let before = self.scroll;
        self.laid.set(Some((width, height)));
        if !self.revealed {
            self.revealed = true;
            if let Some(session) = self.cursor {
                self.reveal(session, tree);
            }
        }
        if let Some(frame) = self.frame(tree) {
            self.scroll = self.scroll.clamp(0.0, frame.limit());
        }
        self.scroll != before
    }

    /// What this view draws that a step of the reducer can move, short of the filter's text.
    fn drawn(&self) -> Drawn {
        Drawn {
            cursor: self.cursor,
            focus: self.common.last_focus.clone(),
            ring: self.common.focus_visible,
            scroll: self.scroll.to_bits(),
            folds: self.folds,
        }
    }

    fn scroll_by(&mut self, delta: f32, tree: &NavTree) {
        let limit = self.frame(tree).map_or(0.0, |frame| frame.limit());
        self.scroll = (self.scroll + delta).clamp(0.0, limit);
    }

    /// An assistive technology's or the inspector's reveal of a row. Whether the list moved.
    pub(crate) fn reveal_listed_control(&mut self, key: &UiKey, tree: &NavTree) -> bool {
        let Some(session) = [ROW, FOLD]
            .into_iter()
            .find_map(|prefix| key.as_str().strip_prefix(prefix))
            .and_then(|session| session.parse().ok())
        else {
            return false;
        };
        let before = self.scroll;
        self.reveal(session, tree);
        self.scroll != before
    }

    /// The host's word on where the keys are and which session the person is in. `keys`: this
    /// view is where its window's keys go, a person's or a driver's (`aterm ctl … key`), and
    /// `at_hand`: its window is the key one too, so a person's keyboard is on it. While the view
    /// has its window's keys the selection is theirs to move; without them it rests on the
    /// session the person is in, whose row opens with every row above it. Whether what the view
    /// draws moved.
    pub(crate) fn follow(
        &mut self,
        keys: bool,
        at_hand: bool,
        person_in: Option<u64>,
        tree: &NavTree,
    ) -> bool {
        // THE RING GOES WHEN THE KEYBOARD THAT DREW IT LEAVES: a person's as their window stops
        // being the key one, anyone's as the window's keys go to another pane. A key driven into
        // a window that is not the key one draws it, and nothing here takes it back.
        let left = (self.at_hand && !at_hand) || !keys;
        let mut moved = self.at_hand != at_hand;
        self.at_hand = at_hand;
        if left && self.common.focus_visible {
            self.common.focus_visible = false;
            moved = true;
        }
        if keys {
            return moved;
        }
        if self.docked && person_in.is_none() && self.cursor.is_some() {
            self.person_in = None;
            self.cursor = None;
            self.common.last_focus = None;
            moved = true;
        }
        if let Some(session) = person_in
            && tree.index_of(session).is_some()
            && (self.person_in != Some(session) || self.cursor != Some(session))
        {
            self.person_in = Some(session);
            self.cursor = Some(session);
            if !self.in_filter() {
                self.common.last_focus = Some(UiKey::new(format!("{ROW}{session}")));
            }
            self.reveal(session, tree);
            moved = true;
        }
        moved
    }

    /// Carry this view from arrangement `old` to `new`: a selection whose session is gone moves
    /// to the row that took its place, and folds of sessions that are gone are forgotten.
    pub(crate) fn rebase(&mut self, old: &NavTree, new: &NavTree) {
        if let Some(gone) = self
            .cursor
            .filter(|session| new.index_of(*session).is_none())
        {
            let was = self
                .listing(old)
                .iter()
                .position(|row| old.entries[row.at].row.session == gone);
            let rows = self.listing(new);
            let next = was
                .map(|at| at.min(rows.len().saturating_sub(1)))
                .and_then(|at| rows.get(at))
                .map(|row| new.entries[row.at].row.session);
            self.cursor = next;
            if self.focused_row() == Some(gone) {
                self.common.last_focus = next.map(|session| UiKey::new(format!("{ROW}{session}")));
            }
        }
        self.opened
            .retain(|session| new.index_of(*session).is_some());
        self.closed
            .retain(|session| new.index_of(*session).is_some());
        if self
            .person_in
            .is_some_and(|session| new.index_of(session).is_none())
        {
            self.person_in = None;
        }
        if let Some(frame) = self.frame(new) {
            self.scroll = self.scroll.clamp(0.0, frame.limit());
        }
    }

    /// The sessions whose rows a page `width` by `height` paints, into `out`.
    pub(crate) fn on_screen(&self, tree: &NavTree, width: f32, height: f32, out: &mut Vec<u64>) {
        let frame = Frame::of(tree, &self.fold(), width, height, self.docked);
        let scroll = self.scroll.min(frame.limit());
        out.extend(
            frame
                .painted(scroll)
                .map(|at| tree.entries[frame.rows[at].at].row.session),
        );
    }

    /// The top and bottom of `session`'s row, as far as the page paints it, on a page `width`
    /// by `height`.
    #[cfg(test)]
    pub(crate) fn row_span(
        &self,
        tree: &NavTree,
        session: u64,
        width: f32,
        height: f32,
    ) -> Option<(f32, f32)> {
        self.rows_span(tree, [session], width, height)
    }

    /// The band that holds every one of `sessions`' rows the page paints, laid out once.
    pub(crate) fn rows_span(
        &self,
        tree: &NavTree,
        sessions: impl IntoIterator<Item = u64>,
        width: f32,
        height: f32,
    ) -> Option<(f32, f32)> {
        match self.shows(tree, sessions, width, height) {
            Shows::Painted(span) => Some(span),
            Shows::Listed | Shows::Nothing => None,
        }
    }

    /// What a page `width` by `height` shows of `sessions`' rows, laid out once.
    pub(crate) fn shows(
        &self,
        tree: &NavTree,
        sessions: impl IntoIterator<Item = u64>,
        width: f32,
        height: f32,
    ) -> Shows {
        let mut sessions = sessions.into_iter().peekable();
        if sessions.peek().is_none() {
            return Shows::Nothing;
        }
        let frame = Frame::of(tree, &self.fold(), width, height, self.docked);
        let scroll = self.scroll.min(frame.limit());
        let (list_top, list_bottom) = frame.painted_span();
        let mut listed = false;
        let span = sessions
            .filter_map(|session| {
                let top = list_top + frame.top_of(tree, session)? - scroll;
                listed = true;
                let (top, bottom) = (top.max(list_top), (top + frame.m.row_h).min(list_bottom));
                (bottom > top).then_some((top, bottom))
            })
            .reduce(|(top, bottom), (a, b)| (top.min(a), bottom.max(b)));
        match span {
            Some(span) => Shows::Painted(span),
            None if listed => Shows::Listed,
            None => Shows::Nothing,
        }
    }

    /// The sessions this view lists of `tree`, in order, into `out`.
    pub(crate) fn listed_sessions(&self, tree: &NavTree, out: &mut Vec<u64>) {
        out.extend(
            self.listing(tree)
                .iter()
                .map(|row| tree.entries[row.at].row.session),
        );
    }

    /// Whether the filter holds a word.
    pub(crate) fn filtered(&self) -> bool {
        !self.filter.value().trim().is_empty()
    }

    fn open(&mut self, session: u64) {
        self.closed.remove(&session);
        self.opened.insert(session);
        self.folds = self.folds.wrapping_add(1);
    }

    fn close(&mut self, session: u64) {
        self.opened.remove(&session);
        self.closed.insert(session);
        self.folds = self.folds.wrapping_add(1);
    }

    /// Open or close `session`'s row. A selection inside a row that closes moves to that row.
    fn toggle(&mut self, session: u64, tree: &NavTree) {
        let rows = self.listing(tree);
        let Some(row) = rows
            .iter()
            .find(|row| tree.entries[row.at].row.session == session)
        else {
            return;
        };
        if row.open {
            self.close(session);
            let inside = self
                .cursor
                .and_then(|cursor| tree.index_of(cursor))
                .is_some_and(|at| at > row.at && at <= row.at + row.under);
            if inside {
                self.cursor = Some(session);
                if self.focused_row().is_some() {
                    self.common.last_focus = Some(UiKey::new(format!("{ROW}{session}")));
                }
            }
        } else if row.under > 0 {
            self.open(session);
        }
    }

    /// Step In (`inward`) or Step Out from the selected row: In opens a closed row and from an
    /// open one moves to the first row under it; Out closes an open row and from a closed one
    /// moves to the row it is under.
    fn step_level(&mut self, inward: bool, tree: &NavTree) {
        let rows = self.listing(tree);
        let Some((at, row)) = self.cursor.and_then(|cursor| {
            rows.iter()
                .enumerate()
                .find(|(_, row)| tree.entries[row.at].row.session == cursor)
        }) else {
            return;
        };
        let session = tree.entries[row.at].row.session;
        let filtered = !self.filter.value().trim().is_empty();
        if inward {
            if row.open {
                if let Some(next) = rows.get(at + 1) {
                    self.select(tree.entries[next.at].row.session, tree);
                }
            } else if row.under > 0 && !filtered {
                self.open(session);
            }
        } else if row.open && !filtered {
            self.close(session);
        } else if let Some(parent) = tree.entries[row.at].parent {
            self.select(tree.entries[parent].row.session, tree);
        }
    }
}

/// One line of a wheel step.
const LINE_PX: f32 = 40.0;

fn edit(input: &mut TextInputState, event: TextInputEvent) {
    match event {
        TextInputEvent::Preedit { text, selection } => input.set_preedit(text, selection),
        TextInputEvent::Commit(text) => input.commit_preedit(&text),
        TextInputEvent::Backspace => input.delete_backward(),
        TextInputEvent::Delete => input.delete_forward(),
        TextInputEvent::Left { extend } => input.move_left(extend),
        TextInputEvent::Right { extend } => input.move_right(extend),
        TextInputEvent::Home { extend } => input.move_to_start(extend),
        TextInputEvent::End { extend } => input.move_to_end(extend),
        TextInputEvent::WordLeft { extend } => input.move_word_left(extend),
        TextInputEvent::WordRight { extend } => input.move_word_right(extend),
        TextInputEvent::KillToEnd => input.kill_to_end(),
        TextInputEvent::KillToStart => input.kill_to_start(),
        TextInputEvent::DeleteWordBackward => input.delete_word_backward(),
        TextInputEvent::DeleteWordForward => input.delete_word_forward(),
        TextInputEvent::SelectAll => input.select_all(),
        TextInputEvent::Undo => {
            input.undo();
        }
        TextInputEvent::Redo => {
            input.redo();
        }
        TextInputEvent::Submit | TextInputEvent::Cancel => input.cancel_preedit(),
    }
}

/// What a view draws that a step of the reducer can move ([`NavigatorViewState::drawn`]).
#[derive(PartialEq)]
struct Drawn {
    cursor: Option<u64>,
    focus: Option<UiKey>,
    ring: bool,
    scroll: u32,
    folds: u64,
}

impl Drawn {
    /// The row the keyboard is on, where it is on a row or its disclosure button.
    fn row(&self) -> Option<u64> {
        let key = self.focus.as_ref()?.as_str();
        key.strip_prefix(ROW)
            .or_else(|| key.strip_prefix(FOLD))?
            .parse()
            .ok()
    }
}

/// What a reducer step was.
enum Moved {
    /// Nothing the page handles.
    Nothing,
    /// A step of the selection, the folds or the scroll: what it moved is read off the view.
    Step,
    /// An edit of the filter.
    Text,
}

impl NavigatorApp {
    fn act(&mut self, view: &mut NavigatorViewState, event: AppEvent) -> Moved {
        let tree = Arc::clone(&self.tree);
        let tree = &*tree;
        match event {
            #[cfg(any(a11y_tree, test))]
            AppEvent::RevealControl(key) => {
                if !view.reveal_listed_control(&key, tree) {
                    return Moved::Nothing;
                }
            }
            #[cfg(any(a11y_tree, test))]
            AppEvent::InsertText(text) if view.in_filter() => {
                return self.act(view, AppEvent::TextInput(TextInputEvent::Commit(text)));
            }
            AppEvent::FocusChanged(focus) => {
                view.common.last_focus = focus;
                if let Some(session) = view.focused_row() {
                    view.cursor = Some(session);
                }
            }
            AppEvent::ScrollLines(lines) | AppEvent::ScrollAt { lines, .. } => {
                view.scroll_by(lines as f32 * LINE_PX, tree);
            }
            AppEvent::ScrollPage(direction) => {
                let room = view.frame(tree).map_or(LINE_PX, |frame| frame.room);
                view.scroll_by(direction as f32 * (room - LINE_PX).max(LINE_PX), tree);
            }
            AppEvent::StepFocus(step) => {
                let rows = view.listing(tree);
                let at = view.cursor.and_then(|cursor| {
                    rows.iter()
                        .position(|row| tree.entries[row.at].row.session == cursor)
                });
                let next = match (at, step.signum()) {
                    (None, -1) => rows.len().checked_sub(1),
                    (None, _) => (!rows.is_empty()).then_some(0),
                    (Some(at), -1) => Some(at.saturating_sub(1)),
                    (Some(at), _) => Some((at + 1).min(rows.len().saturating_sub(1))),
                };
                if let Some(row) = next.and_then(|next| rows.get(next)) {
                    view.select(tree.entries[row.at].row.session, tree);
                }
            }
            AppEvent::TextInput(event) if view.in_filter() => {
                self.edit_filter(view, event, tree);
                return Moved::Text;
            }
            AppEvent::TextInput(TextInputEvent::Submit) if view.focused_row().is_some() => {
                // Return on a row the list scrolled off its screen, which the host cannot press.
                let Some(session) = view
                    .cursor
                    .filter(|session| tree.index_of(*session).is_some())
                else {
                    return Moved::Nothing;
                };
                view.reveal(session, tree);
                self.actions.push_back(NavAction::Go(session));
            }
            AppEvent::TextInput(TextInputEvent::Right { .. }) if view.focused_row().is_some() => {
                view.step_level(true, tree);
            }
            AppEvent::TextInput(TextInputEvent::Left { .. }) if view.focused_row().is_some() => {
                view.step_level(false, tree);
            }
            AppEvent::Action(action) => {
                let key = action.id.as_str();
                let session = |prefix: &str| key.strip_prefix(prefix)?.parse::<u64>().ok();
                if key == FILTER {
                    match action.value {
                        Some(SemanticInput::Text(value)) => {
                            view.filter = TextInputState::new(value);
                            view.scroll = 0.0;
                        }
                        Some(SemanticInput::TextPosition { byte, extend }) => {
                            let anchor = if extend {
                                view.filter.selection().anchor
                            } else {
                                byte
                            };
                            view.filter.set_selection(anchor, byte);
                        }
                        _ => {}
                    }
                    view.common.last_focus = Some(UiKey::new(FILTER));
                    return Moved::Text;
                } else if key == STEP_IN {
                    view.step_level(true, tree);
                } else if key == STEP_OUT {
                    view.step_level(false, tree);
                } else if let Some(session) = session(FOLD) {
                    view.toggle(session, tree);
                } else if let Some(session) = session(ROW) {
                    if tree.index_of(session).is_none() {
                        return Moved::Nothing;
                    }
                    view.cursor = Some(session);
                    view.common.last_focus = Some(UiKey::new(format!("{ROW}{session}")));
                    self.actions.push_back(NavAction::Go(session));
                } else {
                    return Moved::Nothing;
                }
            }
            _ => return Moved::Nothing,
        }
        Moved::Step
    }

    fn edit_filter(
        &mut self,
        view: &mut NavigatorViewState,
        event: TextInputEvent,
        tree: &NavTree,
    ) {
        match event {
            TextInputEvent::Cancel if view.filter.value().is_empty() => {
                // Escape in an empty filter gives the keys back to the list.
                if let Some(session) = view.cursor {
                    view.select(session, tree);
                }
            }
            TextInputEvent::Cancel => {
                view.filter = TextInputState::new(String::new());
                view.scroll = 0.0;
            }
            TextInputEvent::Submit => {
                let needle = view.filter.value().trim().to_lowercase();
                let first = view
                    .listing(tree)
                    .into_iter()
                    .map(|row| &tree.entries[row.at].row)
                    .find(|row| needle.is_empty() || matches(row, &needle))
                    .map(|row| row.session);
                if let Some(session) = first {
                    view.cursor = Some(session);
                    self.actions.push_back(NavAction::Go(session));
                }
            }
            event => {
                edit(&mut view.filter, event);
                view.scroll = 0.0;
            }
        }
    }
}

impl NativeAppModel for NavigatorApp {
    type ViewState = NavigatorViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::Navigator,
            name: NAVIGATOR_TITLE,
            icon: AppIcon::Workers,
            singleton: true,
        }
    }

    fn update(
        &mut self,
        view: &mut Self::ViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        let before = view.drawn();
        match self.act(view, event) {
            Moved::Nothing => return EventResult::Bubble,
            Moved::Text => {}
            Moved::Step => {
                let after = view.drawn();
                if after == before {
                    // A KEY OR A WHEEL STEP THAT MOVED NOTHING REPAINTS NOTHING: an arrow at an
                    // end of the list, a wheel at the scroll's limit, Return on a row.
                    cx.unchanged();
                    return EventResult::Handled;
                }
                // A SELECTION THAT MOVED BETWEEN ROWS THAT STAYED IN PLACE REPAINTS THOSE ROWS:
                // the view's revision stays, so the page rasters their band and not itself.
                let rows = [before.cursor, after.cursor, before.row(), after.row()];
                let in_place = after.scroll == before.scroll
                    && after.folds == before.folds
                    && before.row().is_some()
                    && after.row().is_some();
                let band = view
                    .laid
                    .get()
                    .filter(|_| in_place)
                    .and_then(|(width, height)| {
                        view.rows_span(&self.tree, rows.into_iter().flatten(), width, height)
                            .map(|span| band_of(span, width))
                    });
                if let Some(band) = band {
                    cx.repaint(band);
                    return EventResult::Handled;
                }
            }
        }
        view.common.presentation_revision = view.common.presentation_revision.wrapping_add(1);
        cx.repaint(DamageRegion::All);
        EventResult::Handled
    }

    fn view(&self, view: &Self::ViewState, cx: &ViewCx<'_>) -> UiTree {
        page(self, view, cx.viewport.width, cx.viewport.height)
    }

    fn commands(&self, _view: &Self::ViewState, _out: &mut Vec<Command>) {}

    fn presentation(&self, _view: &Self::ViewState) -> AppPresentation {
        let title = self.title().to_owned();
        AppPresentation {
            tooltip: Some(format!("{title}: {} live", self.tree.entries.len())),
            title,
            icon: AppIcon::Workers,
            // `working` asks for nobody's eye, and on a machine with agents it is nearly always.
            indicators: AppIndicators {
                dirty: false,
                busy: false,
                attention: false,
            },
            closable: true,
        }
    }

    fn prepare_close(&mut self, _close: CloseRequest, _cx: &mut UpdateCx<'_>) -> CloseReadiness {
        CloseReadiness::Ready
    }
}

impl NavigatorApp {
    fn title(&self) -> &str {
        if self.tree.machine.is_empty() {
            NAVIGATOR_TITLE
        } else {
            &self.tree.machine
        }
    }
}

/// The damage of the band `top..bottom` across a page `width` wide.
pub(crate) fn band_of((top, bottom): (f32, f32), width: f32) -> DamageRegion {
    let y = top.floor().max(0.0) as u32;
    DamageRegion::Rect {
        x: 0,
        y,
        width: width.ceil().max(1.0) as u32,
        height: (bottom.ceil().max(0.0) as u32).saturating_sub(y).max(1),
    }
}

/// The space between the title and the filter.
const HEADER_GAP: f32 = 6.0;
/// The space between the header and the list.
const ROOT_GAP: f32 = 16.0;
/// The space between rows (the Settings rail's).
const ROW_GAP: f32 = 2.0;
/// The space before the machine's group.
const GROUP_GAP: f32 = 10.0;
/// A row's pad above its first line and below its second.
const ROW_PAD: f32 = 6.0;
/// The disclosure button's slot, which every row keeps so the titles stand in one column.
const FOLD_SLOT: f32 = 32.0;
/// The space after a row's last text.
const ROW_END: f32 = 12.0;
/// How far a row stands in for each row it sits under, and for how many of them.
const INDENT: f32 = 14.0;
const INDENT_DEPTHS: u16 = 4;
/// The space between two texts on one line.
const TEXT_GAP: f32 = 10.0;
/// The widest the list's measure grows.
const MEASURE: f32 = 720.0;

/// The page's measures at one width and text scale.
#[derive(Clone, Copy)]
struct Measure {
    narrow: bool,
    pad: f32,
    inner: f32,
    header_h: f32,
    title_h: f32,
    filter_h: f32,
    title_line: f32,
    detail_line: f32,
    group_line: f32,
    row_h: f32,
}

impl Measure {
    fn at(width: f32, docked: bool) -> Self {
        let scale = crate::native_appearance::text_scale();
        let narrow = width < 480.0;
        let pad = if narrow { 12.0 } else { 24.0 };
        let title_h = 34.0 * scale.max(1.0);
        let filter_h = 36.0_f32.max(40.0 * scale);
        let title_line = 22.0_f32.max(20.0 * scale);
        let detail_line = 18.0_f32.max(16.0 * scale);
        Self {
            narrow,
            pad,
            inner: (width - pad * 2.0).clamp(1.0, MEASURE),
            header_h: if docked {
                title_h
            } else {
                title_h + HEADER_GAP + filter_h
            },
            title_h,
            filter_h,
            title_line,
            detail_line,
            group_line: 22.0_f32.max(16.0 * scale),
            row_h: title_line + detail_line + ROW_PAD * 2.0,
        }
    }
}

fn room_of(m: Measure, height: f32) -> f32 {
    (height - m.pad * 2.0 - m.header_h - ROOT_GAP).max(0.0)
}

/// Where each listed row stands in the list, by arithmetic: every row is one height.
struct Frame {
    m: Measure,
    room: f32,
    rows: Vec<Listed>,
    /// Each row's top from the list's first line.
    tops: Vec<f32>,
    /// The first row of the machine's group.
    machine_from: Option<usize>,
    content: f32,
}

impl Frame {
    fn of(tree: &NavTree, fold: &Fold<'_>, width: f32, height: f32, docked: bool) -> Self {
        let m = Measure::at(width, docked);
        let rows = listed(tree, fold);
        let mut tops = Vec::with_capacity(rows.len());
        let mut machine_from = None;
        let mut top = 0.0;
        for (at, row) in rows.iter().enumerate() {
            if machine_from.is_none() && tree.entries[row.at].group == Group::Machine {
                machine_from = Some(at);
                if at > 0 {
                    top += GROUP_GAP + ROW_GAP;
                }
                top += m.group_line + ROW_GAP;
            }
            tops.push(top);
            top += m.row_h + ROW_GAP;
        }
        Self {
            m,
            room: room_of(m, height),
            content: if rows.is_empty() {
                m.title_line
            } else {
                top - ROW_GAP
            },
            rows,
            tops,
            machine_from,
        }
    }

    fn limit(&self) -> f32 {
        (self.content - self.room).max(0.0)
    }

    /// The height under the room in which a list that runs on fades out: the page's bottom
    /// margin, so a row the room's edge cuts reads as cut and never as the list's end.
    fn band(&self) -> f32 {
        if self.limit() > 0.0 { self.m.pad } else { 0.0 }
    }

    /// The rows any part of which the page paints at `scroll`: the room's and the band's.
    fn painted(&self, scroll: f32) -> std::ops::Range<usize> {
        let shown = self.shown(scroll);
        let to = self
            .tops
            .partition_point(|top| *top < scroll + self.room + self.band());
        shown.start..to.max(shown.end)
    }

    /// Whether the machine's heading is painted at `scroll`: only over a row of its own that
    /// shows in the room. A heading the room's edge leaves with nothing under it is not drawn.
    fn heads_machine(&self, scroll: f32) -> bool {
        self.machine_from
            .is_some_and(|at| self.tops[at] - scroll + crate::native_ui::LIST_PEEK_MIN <= self.room)
    }

    /// The rows any part of which shows at `scroll`.
    fn shown(&self, scroll: f32) -> std::ops::Range<usize> {
        let from = self
            .tops
            .partition_point(|top| top + self.m.row_h <= scroll);
        let to = self.tops.partition_point(|top| *top < scroll + self.room);
        from..to
    }

    fn top_of(&self, tree: &NavTree, session: u64) -> Option<f32> {
        let entry = tree.index_of(session)?;
        let at = self.rows.binary_search_by_key(&entry, |row| row.at).ok()?;
        Some(self.tops[at])
    }

    /// The top and bottom of what the list paints on the page: its room and the band under it.
    fn painted_span(&self) -> (f32, f32) {
        let top = self.m.pad + self.m.header_h + ROOT_GAP;
        (top, top + self.room + self.band())
    }
}

fn plain_group(key: impl Into<Arc<str>>) -> UiNode {
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
}

fn words(key: String, text: String, role: SemanticRole, style: StyleRef, width: Length) -> UiNode {
    UiNode::new(key, UiContent::Text(TextSpec { text, role, style }))
        .layout(Layout::default().width(width).height(Length::Fill))
}

/// What every row of one page is drawn with.
#[derive(Clone, Copy)]
struct RowCx {
    m: Measure,
    clock_ms: u64,
    cursor: Option<u64>,
    at_hand: bool,
    /// The widest of the five words, so the latest words start in one column.
    word_w: f32,
}

fn press(row: &NavRow, listed: Listed, cx: RowCx) -> UiNode {
    let key = format!("{ROW}{}", row.session);
    let selected = cx.cursor == Some(row.session);
    UiNode::new(
        key.clone(),
        UiContent::Button(
            Control::new(
                ButtonSpec::new(row.spoken(listed, cx.clock_ms)).visual_label(String::new()),
                ActionId::new(key),
            )
            .state(ControlState {
                selected,
                muted: selected && !cx.at_hand,
                ..ControlState::default()
            })
            .style(StyleRef::Navigation),
        ),
    )
}

/// A row on the list's screen: a whole-row press under its two painted lines, and its
/// disclosure button over them where sessions sit under it.
fn row_node(entry: &NavEntry, listed: Listed, cx: RowCx) -> UiNode {
    let m = cx.m;
    let row = &*entry.row;
    let key = format!("navigator/item/{}", row.session);
    let indent = INDENT * f32::from(entry.depth.min(INDENT_DEPTHS));
    let text_w = (m.inner - indent - FOLD_SLOT - ROW_END).max(1.0);

    let age = age_words(cx.clock_ms, row.moved_ms);
    let meta = match &row.agent {
        Some(agent) if !m.narrow && !age.is_empty() => format!("{agent} \u{b7} {age}"),
        Some(agent) if !m.narrow => agent.clone(),
        _ => age,
    };
    let meta_w = if meta.is_empty() {
        0.0
    } else {
        text_width(&meta, SemanticRole::Text, StyleRef::Note).min(text_w * 0.45)
    };
    let title_room = if meta.is_empty() {
        text_w
    } else {
        (text_w - meta_w - TEXT_GAP).max(1.0)
    };
    let mut first = vec![words(
        format!("{key}/title"),
        one_line(
            &row.title,
            SemanticRole::Text,
            StyleRef::Primary,
            title_room,
        ),
        SemanticRole::Text,
        StyleRef::Primary,
        Length::Fill,
    )];
    if !meta.is_empty() {
        first.push(words(
            format!("{key}/meta"),
            one_line(&meta, SemanticRole::Text, StyleRef::Note, meta_w),
            SemanticRole::Text,
            StyleRef::Note,
            Length::Fixed(meta_w),
        ));
    }
    let first = plain_group(format!("{key}/first"))
        .layout(
            Layout::row()
                .gap(TEXT_GAP)
                .height(Length::Fixed(m.title_line)),
        )
        .children(first);

    let count = (listed.under > 0 && !listed.open).then(|| under_words(listed.under));
    let count_w = count.as_deref().map_or(0.0, |count| {
        text_width(count, SemanticRole::Text, StyleRef::Note)
    });
    let word_w = cx.word_w.min(text_w);
    let said_room = text_w
        - word_w
        - TEXT_GAP
        - if count.is_some() {
            count_w + TEXT_GAP
        } else {
            0.0
        };
    let mut second = vec![words(
        format!("{key}/word"),
        row.word.word().to_owned(),
        SemanticRole::Status,
        row.word.style(),
        Length::Fixed(word_w),
    )];
    // Latest words with less room than the word's own box are left out whole, not shown as a stub.
    match row
        .line
        .as_deref()
        .filter(|_| said_room >= word_w + TEXT_GAP)
    {
        Some(line) => second.push(words(
            format!("{key}/line"),
            one_line(line, SemanticRole::Text, StyleRef::Note, said_room),
            SemanticRole::Text,
            StyleRef::Note,
            Length::Fill,
        )),
        None if count.is_some() => second.push(plain_group(format!("{key}/space"))),
        None => {}
    }
    if let Some(count) = count {
        second.push(words(
            format!("{key}/under"),
            count,
            SemanticRole::Text,
            StyleRef::Note,
            Length::Fixed(count_w.min((text_w - word_w - TEXT_GAP).max(1.0))),
        ));
    }
    let second = plain_group(format!("{key}/second"))
        .layout(
            Layout::row()
                .gap(TEXT_GAP)
                .height(Length::Fixed(m.detail_line)),
        )
        .children(second);

    let band = plain_group(format!("{key}/band"))
        .layout(Layout::column().padding(Insets {
            top: ROW_PAD,
            right: ROW_END,
            bottom: ROW_PAD,
            left: FOLD_SLOT,
        }))
        .children(vec![first, second])
        .paint_only_tree();

    let mut face = vec![press(row, listed, cx), band];
    if listed.under > 0 {
        let fold = format!("{FOLD}{}", row.session);
        let act = if listed.open { "Hide" } else { "Show" };
        face.push(
            plain_group(format!("{key}/seat"))
                .layout(Layout::row().padding(Insets {
                    top: ROW_PAD,
                    bottom: ROW_PAD + m.detail_line,
                    ..Insets::default()
                }))
                .children(vec![
                    UiNode::new(
                        fold.clone(),
                        UiContent::Button(
                            Control::new(
                                ButtonSpec::new(format!("{act} sessions under {}", row.title))
                                    .visual_label(String::new()),
                                ActionId::new(fold),
                            )
                            .state(ControlState {
                                expanded: Some(listed.open),
                                ..ControlState::default()
                            })
                            .style(StyleRef::Plain),
                        ),
                    )
                    .layout(
                        Layout::default()
                            .width(Length::Fixed(FOLD_SLOT))
                            .height(Length::Fill),
                    ),
                ]),
        );
    }
    plain_group(key)
        .layout(
            Layout::default()
                .height(Length::Fixed(m.row_h))
                .padding(Insets {
                    left: indent,
                    ..Insets::default()
                }),
        )
        .children(face)
}

/// The page: the machine's name above the list, with a filter only in a tab host.
pub(crate) fn page(
    app: &NavigatorApp,
    view: &NavigatorViewState,
    width: f32,
    height: f32,
) -> UiTree {
    let tree = &*app.tree;
    let frame = Frame::of(tree, &view.fold(), width, height, view.docked);
    view.laid.set(Some((width, height)));
    let m = frame.m;
    let limit = frame.limit();
    let scroll = view.scroll.min(limit);

    let filter = (!view.docked).then(|| {
        UiNode::new(
            FILTER,
            UiContent::TextField(
                Control::new(
                    TextFieldSpec {
                        label: FILTER_LABEL.into(),
                        placeholder: Some(FILTER_LABEL.into()),
                        secret: false,
                        visual_value: None,
                        input: Some(view.filter.projection()),
                        swatch: None,
                        swatch_line: false,
                        trailing_hint: None,
                        description: None,
                        end_button: false,
                    },
                    ActionId::new(FILTER),
                )
                .value(SemanticValue::Text(view.filter.value().into()))
                .state(ControlState {
                    focused: view.in_filter(),
                    ..ControlState::default()
                })
                .style(StyleRef::Secondary),
            ),
        )
        .layout(Layout::default().height(Length::Fixed(m.filter_h)))
    });
    let header = plain_group("navigator/header")
        .layout(
            Layout::column()
                .gap(HEADER_GAP)
                .width(Length::Fixed(m.inner))
                .height(Length::Fixed(m.header_h)),
        )
        .children(
            std::iter::once(
                UiNode::new(
                    "navigator/title",
                    UiContent::Text(TextSpec::heading(one_line(
                        app.title(),
                        SemanticRole::Heading,
                        StyleRef::Primary,
                        m.inner,
                    ))),
                )
                .layout(Layout::default().height(Length::Fixed(m.title_h))),
            )
            .chain(filter)
            .collect(),
        );

    let cx = RowCx {
        m,
        clock_ms: app.clock_ms,
        cursor: view.cursor,
        at_hand: view.at_hand,
        word_w: Word::ALL
            .iter()
            .map(|word| text_width(word.word(), SemanticRole::Status, word.style()))
            .fold(0.0, f32::max),
    };
    let shown = frame.painted(scroll);
    let heads_machine = frame.heads_machine(scroll);
    let mut children = Vec::with_capacity(frame.rows.len() + 2);
    for (at, listed) in frame.rows.iter().enumerate() {
        let entry = &tree.entries[listed.at];
        if frame.machine_from == Some(at) {
            if at > 0 {
                children.push(
                    plain_group("navigator/machine/gap")
                        .layout(Layout::default().height(Length::Fixed(GROUP_GAP)))
                        .paint_only(),
                );
            }
            let heading = UiNode::new(
                "navigator/machine",
                UiContent::Text(TextSpec {
                    text: one_line(app.title(), SemanticRole::Heading, StyleRef::Quiet, m.inner),
                    role: SemanticRole::Heading,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(m.group_line)));
            children.push(if heads_machine {
                heading
            } else {
                heading.offscreen()
            });
        }
        children.push(if shown.contains(&at) {
            row_node(entry, *listed, cx)
        } else {
            // A row off the list's screen is its press alone: `inspect`, Tab and the arrows
            // reach it, and the page compiles one screen of rows however many it lists.
            press(&entry.row, *listed, cx)
                .layout(Layout::default().height(Length::Fixed(m.row_h)))
                .offscreen()
        });
    }
    if frame.rows.is_empty() {
        let none = if view.filter.value().trim().is_empty() {
            NO_SESSIONS
        } else {
            NO_MATCH
        };
        children.push(
            UiNode::new(
                "navigator/empty",
                UiContent::Text(TextSpec {
                    text: none.to_owned(),
                    role: SemanticRole::Status,
                    style: StyleRef::Note,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(m.title_line))),
        );
    }

    let list = UiNode::new(
        "navigator/list",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::List)),
    )
    .layout(
        Layout::column()
            .width(Length::Fixed(m.inner))
            .height(Length::Fixed(frame.room + frame.band()))
            .gap(ROW_GAP)
            .clipped()
            .scrolled(scroll),
    )
    .room(frame.room, limit > 0.0)
    .lists_every_row()
    .children(children);

    let mut root = UiNode::new(
        "navigator/root",
        UiContent::Group(GroupSpec::new(app.title())),
    )
    .layout(
        Layout::column()
            .padding(Insets {
                bottom: m.pad - frame.band(),
                ..Insets::all(m.pad)
            })
            .gap(ROOT_GAP)
            .clipped(),
    )
    .children(vec![header, list]);
    if let UiContent::Group(spec) = &mut root.content {
        spec.role = SemanticRole::Application;
    }
    UiTree::new(root)
}

#[cfg(test)]
#[path = "navigator_tests.rs"]
mod tests;
