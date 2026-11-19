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

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use aterm_types::needs::NeedWord;

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

#[path = "navigator_resources.rs"]
pub(crate) mod resources;

/// The width of the navigator's pane in a split, in cells.
pub(crate) const SPLIT_COLS: u16 = 30;
/// The filter field's key, and the action that puts the keyboard in it.
pub(crate) const FILTER: &str = "navigator/filter";
/// A row's press, before its session's local id: go to that session.
pub(crate) const ROW: &str = "navigator/row/";
/// A row's disclosure button, before its session's local id.
const FOLD: &str = "navigator/fold/";
const CHART_DETAILS: &str = "navigator/chart-details";
/// Open the selected row, or step to the first session under it.
pub(crate) const STEP_IN: &str = "navigator/step-in";
/// Close the selected row, or step to the row it is under.
pub(crate) const STEP_OUT: &str = "navigator/step-out";

/// The page's name where no machine's name is published yet.
const NAVIGATOR_TITLE: &str = "Navigator";
const NO_SESSIONS: &str = "No live sessions";
const NO_MATCH: &str = "No sessions match";
const FILTER_LABEL: &str = "Filter";

/// The observed activity or obstruction on a session row.
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
    /// The published reason an operation needs attention, if any.
    pub(crate) need: Option<NeedWord>,
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
    let obstructed = matches!(
        facts.need,
        Some(
            NeedWord::Frozen
                | NeedWord::Stopped
                | NeedWord::Stalled
                | NeedWord::Login
                | NeedWord::Memory
                | NeedWord::Wall
                | NeedWord::Unsupervised
                | NeedWord::Approval
        )
    );
    if obstructed || failed || matches!(facts.task, Some(TaskState::Stalled)) {
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
    /// The stable shortcut number of this session in the local process.
    pub(crate) shortcut: Option<u8>,
    pub(crate) title: String,
    /// The task recorded when the session was created, across foreground program changes.
    pub(crate) original_topic: Option<String>,
    /// A deliberate name or role supplies the display title.
    pub(crate) named: bool,
    /// The foreground program's observed or self-reported name.
    pub(crate) program: Option<String>,
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
            && self.shortcut == other.shortcut
            && self.title == other.title
            && self.original_topic == other.original_topic
            && self.named == other.named
            && self.program == other.program
            && self.word == other.word
            && self.line == other.line
            && self.creator == other.creator
    }
}

impl NavRow {
    pub(crate) fn panel_title(&self) -> &str {
        if self.named {
            &self.title
        } else {
            self.original_topic.as_deref().unwrap_or(&self.title)
        }
    }

    /// Every field, for a screen reader and `inspect`: the painted texts are cut to their boxes.
    fn spoken(&self, listed: Listed, clock_ms: u64, numbered: bool) -> String {
        let title = if numbered {
            self.panel_title()
        } else {
            &self.title
        };
        let mut said = format!("{title}, {}", self.word.word());
        if numbered && let Some(topic) = self.original_topic.as_deref().filter(|t| *t != title) {
            said.push_str(", Original topic, ");
            said.push_str(topic);
        }
        if listed.under > 0 && !listed.open {
            said.push_str(", ");
            said.push_str(&under_words(listed.under));
        }
        if let Some(program) = &self.program {
            said.push_str(", ");
            said.push_str(program);
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
/// row that matches it (its title, program, word or latest words, in any case) with the rows above
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
        || row.program.as_deref().is_some_and(holds)
        || row.word.word().contains(needle)
        || row.line.as_deref().is_some_and(holds)
}

/// What a press on the page asks the host for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NavAction {
    Window(crate::tab_model::ViewId),
    /// Show the session here, retaining its place below the tabs until kept.
    Preview(u64),
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
    pub(crate) resources: resources::Panel,
    pub(crate) resources_at_ns: u64,
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

/// A built-in app view. Its stable identity survives tab and window reordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrganizerWindow {
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) label: String,
    pub(crate) selected: bool,
}

const WINDOW: &str = "navigator/window/";

/// The terminal leaves in this window’s strip order, followed by the other live sessions.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DockSessions {
    /// The window routes its session-number shortcuts through the organizer.
    pub(crate) numbered: bool,
    pub(crate) windows: Vec<OrganizerWindow>,
    pub(crate) order: Vec<u64>,
    pub(crate) shown: usize,
    pub(crate) preview: Option<u64>,
}

pub(crate) fn row_session(key: &str) -> Option<u64> {
    key.strip_prefix(ROW)?.parse().ok()
}

/// One view of the list: its filter, its folds, its scroll and the row the keyboard is on.
#[derive(Debug)]
pub(crate) struct NavigatorViewState {
    pub(crate) common: CommonViewState,
    docked: bool,
    chart_details: bool,
    panel_headers: RefCell<PanelHeaderMemo>,
    pub(crate) sessions: DockSessions,
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
            chart_details: false,
            panel_headers: RefCell::default(),
            sessions: DockSessions::default(),
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
        if self.docked {
            self.sessions
                .order
                .iter()
                .filter_map(|session| tree.index_of(*session))
                .map(|at| Listed {
                    at,
                    open: false,
                    under: 0,
                })
                .collect()
        } else {
            listed(tree, &self.fold())
        }
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

    fn window_index(&self, key: &UiKey) -> Option<usize> {
        let id = key.as_str().strip_prefix(WINDOW)?.parse::<u64>().ok()?;
        self.sessions
            .windows
            .iter()
            .position(|row| row.view.get() == id)
    }

    fn in_filter(&self) -> bool {
        self.common
            .last_focus
            .as_ref()
            .is_some_and(|key| key.as_str() == FILTER)
    }

    fn frame(&self, tree: &NavTree) -> Option<Frame> {
        let (width, height) = self.laid.get()?;
        Some(Frame::of(tree, self, width, height))
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
        if let Some(at) = frame.place_of(tree, session) {
            self.reveal_span(&frame, frame.tops[at], frame.height(at));
        }
    }

    fn reveal_span(&mut self, frame: &Frame, top: f32, height: f32) {
        if height >= frame.room || top < self.scroll {
            self.scroll = top;
        } else if top + height > self.scroll + frame.room {
            self.scroll = top + height - frame.room;
        }
        self.scroll = self.scroll.clamp(0.0, frame.limit());
    }

    /// A change in this row's height moves the rows below it even if their contents are the same.
    pub(crate) fn panel_row_resized(&self, before: &NavRow, after: &NavRow, width: f32) -> bool {
        if !self.sessions.numbered || same_panel_header(before, after, self.chart_details) {
            return false;
        }
        let m = Measure::at(width, self.docked);
        let header = |row: &NavRow| {
            PanelHeader::of(
                row,
                m.panel_width(),
                self.sessions.preview,
                self.chart_details,
            )
        };
        header(before).extra_height(m) != header(after).extra_height(m)
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
            chart_details: self.chart_details,
        }
    }

    fn scroll_by(&mut self, delta: f32, tree: &NavTree) {
        let limit = self.frame(tree).map_or(0.0, |frame| frame.limit());
        self.scroll = (self.scroll + delta).clamp(0.0, limit);
    }

    /// An assistive technology's or the inspector's reveal of a row. Whether the list moved.
    pub(crate) fn reveal_listed_control(&mut self, key: &UiKey, tree: &NavTree) -> bool {
        if let Some(index) = self.window_index(key) {
            let Some(frame) = self.frame(tree) else {
                return false;
            };
            let top = frame.content + ROW_GAP
                - (self.sessions.windows.len() - index) as f32 * (frame.m.title_h + ROW_GAP);
            let before = self.scroll;
            self.reveal_span(&frame, top, frame.m.title_h);
            return self.scroll != before;
        }
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
        let frame = Frame::of(tree, self, width, height);
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
        let frame = Frame::of(tree, self, width, height);
        let scroll = self.scroll.min(frame.limit());
        let (list_top, list_bottom) = frame.painted_span();
        let mut listed = false;
        let span = sessions
            .filter_map(|session| {
                let at = frame.place_of(tree, session)?;
                let top = list_top + frame.tops[at] - scroll;
                listed = true;
                let (top, bottom) = (top.max(list_top), (top + frame.height(at)).min(list_bottom));
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
        !self.docked && !self.filter.value().trim().is_empty()
    }

    fn toggle_chart_details(&mut self, tree: &NavTree) {
        self.chart_details = !self.chart_details;
        if let Some(session) = self.cursor {
            self.reveal(session, tree);
        }
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
        if self.docked {
            return;
        }
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

/// What a view draws that a step of the reducer can move ([`NavigatorViewState::drawn`]).
#[derive(PartialEq)]
struct Drawn {
    cursor: Option<u64>,
    focus: Option<UiKey>,
    ring: bool,
    scroll: u32,
    folds: u64,
    chart_details: bool,
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
                if let Some(key) = &focus
                    && key.as_str().starts_with(WINDOW)
                {
                    view.reveal_listed_control(key, tree);
                }
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
                let count = rows.len() + view.sessions.windows.len();
                let focused_window = view
                    .common
                    .last_focus
                    .as_ref()
                    .and_then(|key| view.window_index(key))
                    .map(|index| rows.len() + index);
                let on_details = view
                    .common
                    .last_focus
                    .as_ref()
                    .is_some_and(|key| key.as_str() == CHART_DETAILS);
                let at = focused_window.or_else(|| {
                    view.cursor.and_then(|cursor| {
                        rows.iter()
                            .position(|row| tree.entries[row.at].row.session == cursor)
                    })
                });
                if view.sessions.numbered && step < 0 && (on_details || at == Some(0)) {
                    view.common.last_focus = Some(UiKey::new(CHART_DETAILS));
                    view.common.focus_visible = true;
                    return Moved::Step;
                }
                let at = at.filter(|_| !on_details);
                let next = match (at, step.signum()) {
                    (None, -1) => count.checked_sub(1),
                    (None, _) => (count > 0).then_some(0),
                    (Some(at), -1) => Some(at.saturating_sub(1)),
                    (Some(at), _) => Some((at + 1).min(count.saturating_sub(1))),
                };
                if let Some(index) = next {
                    if let Some(row) = rows.get(index) {
                        view.select(tree.entries[row.at].row.session, tree);
                    } else if let Some(window) = view.sessions.windows.get(index - rows.len()) {
                        let key = UiKey::new(format!("{WINDOW}{}", window.view));
                        view.reveal_listed_control(&key, tree);
                        view.common.last_focus = Some(key);
                        view.common.focus_visible = true;
                    }
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
                self.actions.push_back(if view.docked {
                    NavAction::Preview(session)
                } else {
                    NavAction::Go(session)
                });
            }
            AppEvent::TextInput(TextInputEvent::Submit)
                if view.sessions.numbered
                    && view
                        .common
                        .last_focus
                        .as_ref()
                        .is_some_and(|key| key.as_str() == CHART_DETAILS) =>
            {
                view.toggle_chart_details(tree);
            }
            AppEvent::TextInput(TextInputEvent::Submit) => {
                let Some(index) = view
                    .common
                    .last_focus
                    .as_ref()
                    .and_then(|key| view.window_index(key))
                else {
                    return Moved::Nothing;
                };
                self.actions
                    .push_back(NavAction::Window(view.sessions.windows[index].view));
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
                if key == CHART_DETAILS && view.sessions.numbered {
                    view.toggle_chart_details(tree);
                } else if key == FILTER {
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
                } else if let Some(id) = session(WINDOW) {
                    let Some(row) = view
                        .sessions
                        .windows
                        .iter()
                        .find(|row| row.view.get() == id)
                    else {
                        return Moved::Nothing;
                    };
                    self.actions.push_back(NavAction::Window(row.view));
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
                    self.actions.push_back(if view.docked {
                        NavAction::Preview(session)
                    } else {
                        NavAction::Go(session)
                    });
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
                    self.actions.push_back(if view.docked {
                        NavAction::Preview(session)
                    } else {
                        NavAction::Go(session)
                    });
                }
            }
            event => {
                event.edit(&mut view.filter);
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
                    && after.chart_details == before.chart_details
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
    fn panel_width(self) -> f32 {
        (self.inner - 2. * ROW_END).max(1.)
    }

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

/// Offscreen and painted rows reserve the same measured header and chart space.
struct Frame {
    m: Measure,
    room: f32,
    rows: Vec<Listed>,
    /// Each row's top from the list's first line.
    tops: Vec<f32>,
    bottoms: Vec<f32>,
    /// Numbered rows alone have independently measured headers.
    headers: Vec<Arc<PanelHeader>>,
    dock_places: Vec<usize>,
    /// The first row of the machine's group.
    machine_from: Option<usize>,
    content: f32,
}

impl Frame {
    fn of(tree: &NavTree, view: &NavigatorViewState, width: f32, height: f32) -> Self {
        let docked = view.docked;
        let mut m = Measure::at(width, docked);
        if view.sessions.numbered {
            m.row_h += resources::chart_height(m.panel_width(), m.detail_line, view.chart_details);
        }
        let rows = view.listing(tree);
        let mut memo = view.sessions.numbered.then(|| {
            let mut memo = view.panel_headers.borrow_mut();
            memo.prepare(m.panel_width(), view.chart_details, rows.len());
            memo
        });
        let mut tops = Vec::with_capacity(rows.len());
        let mut bottoms = Vec::with_capacity(rows.len());
        let mut headers = Vec::new();
        let mut machine_from = None;
        let mut dock_places = if docked {
            vec![usize::MAX; tree.entries.len()]
        } else {
            Vec::new()
        };
        let mut top = 0.0;
        for (at, row) in rows.iter().enumerate() {
            if docked {
                dock_places[row.at] = at;
            }
            if machine_from.is_none()
                && if docked {
                    at == view.sessions.shown
                } else {
                    tree.entries[row.at].group == Group::Machine
                }
            {
                machine_from = Some(at);
                if at > 0 {
                    top += GROUP_GAP + ROW_GAP;
                }
                top += m.group_line + ROW_GAP;
            }
            tops.push(top);
            top += m.row_h;
            if let Some(memo) = &mut memo {
                let header = memo.header(
                    at,
                    &tree.entries[row.at].row,
                    m.panel_width(),
                    view.sessions.preview,
                    view.chart_details,
                );
                top += header.extra_height(m);
                headers.push(header);
            }
            bottoms.push(top);
            top += ROW_GAP;
        }
        if docked && machine_from.is_none() {
            machine_from = Some(rows.len());
            if !rows.is_empty() {
                top += GROUP_GAP + ROW_GAP;
            }
            top += m.group_line + ROW_GAP;
        }
        if rows.is_empty() {
            top += m.title_line + ROW_GAP;
        }
        if !view.sessions.windows.is_empty() {
            top += GROUP_GAP + ROW_GAP + m.group_line + ROW_GAP;
            top += (view.sessions.windows.len() as f32) * (m.title_h + ROW_GAP);
        }
        Self {
            m,
            room: room_of(m, height),
            content: top - ROW_GAP,
            rows,
            tops,
            bottoms,
            headers,
            dock_places,
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
            .and_then(|at| self.tops.get(at))
            .is_some_and(|top| top - scroll + crate::native_ui::LIST_PEEK_MIN <= self.room)
    }

    /// The rows any part of which shows at `scroll`.
    fn shown(&self, scroll: f32) -> std::ops::Range<usize> {
        let from = self.bottoms.partition_point(|bottom| *bottom <= scroll);
        let to = self.tops.partition_point(|top| *top < scroll + self.room);
        from..to
    }

    fn height(&self, at: usize) -> f32 {
        self.bottoms[at] - self.tops[at]
    }

    fn place_of(&self, tree: &NavTree, session: u64) -> Option<usize> {
        let entry = tree.index_of(session)?;
        let at = match self.dock_places.get(entry) {
            Some(&at) => at,
            None => self.rows.binary_search_by_key(&entry, |row| row.at).ok()?,
        };
        (at < self.rows.len()).then_some(at)
    }

    #[cfg(test)]
    fn top_of(&self, tree: &NavTree, session: u64) -> Option<f32> {
        Some(self.tops[self.place_of(tree, session)?])
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

fn panel_shortcut(number: u8) -> String {
    let chord = if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+Shift+"
    };
    format!("{chord}{number}")
}

fn panel_identity(row: &NavRow, preview: Option<u64>) -> std::borrow::Cow<'_, str> {
    let program = row.program.as_deref().unwrap_or("");
    if preview == Some(row.session) {
        if program.is_empty() {
            "Preview".into()
        } else {
            format!("Preview · {program}").into()
        }
    } else {
        program.into()
    }
}

fn same_panel_header(a: &NavRow, b: &NavRow, details: bool) -> bool {
    a.panel_title() == b.panel_title()
        && a.program == b.program
        && a.shortcut == b.shortcut
        && (!details || a.original_topic == b.original_topic)
}

/// A panel refresh frequently changes only session output, resource samples or the scroll.
/// Keep one measurement per listed position. Width, face coverage, text scale or Details
/// invalidates the view's measures; a row edit or changed preview can invalidate one position.
#[derive(Debug, Default)]
struct PanelHeaderMemo {
    context: Option<(u32, u32, u64, bool)>,
    rows: Vec<(Arc<NavRow>, bool, Arc<PanelHeader>)>,
}

impl PanelHeaderMemo {
    fn prepare(&mut self, width: f32, details: bool, count: usize) {
        let context = (
            width.to_bits(),
            crate::native_appearance::text_scale().to_bits(),
            crate::tray_raster::chrome_measure_epoch(),
            details,
        );
        if self.context != Some(context) {
            self.context = Some(context);
            self.rows.clear();
        }
        self.rows.truncate(count);
    }

    fn header(
        &mut self,
        at: usize,
        row: &Arc<NavRow>,
        width: f32,
        preview: Option<u64>,
        details: bool,
    ) -> Arc<PanelHeader> {
        let selected = preview == Some(row.session);
        if let Some((prior, was_preview, header)) = self.rows.get_mut(at)
            && *was_preview == selected
            && (Arc::ptr_eq(prior, row) || same_panel_header(prior, row, details))
        {
            if !Arc::ptr_eq(prior, row) {
                Arc::clone_from(prior, row);
            }
            return Arc::clone(header);
        }
        let header = Arc::new(PanelHeader::of(row, width, preview, details));
        let entry = (Arc::clone(row), selected, Arc::clone(&header));
        if let Some(slot) = self.rows.get_mut(at) {
            *slot = entry;
        } else {
            self.rows.push(entry);
        }
        header
    }
}

/// Header geometry is independent of the minute clock. Optional age text uses the remaining
/// room after the title, the program and its shortcut have been placed.
#[derive(Debug, Default)]
struct PanelHeader {
    second_title: Option<usize>,
    meta_below: bool,
    shortcut_below: bool,
    original_lines: Vec<std::ops::Range<usize>>,
}

impl PanelHeader {
    fn of(row: &NavRow, width: f32, preview: Option<u64>, details: bool) -> Self {
        let title = row.panel_title();
        let title_width = text_width(title, SemanticRole::Text, StyleRef::Primary);
        let identity = panel_identity(row, preview);
        let note_width = |s: &str| text_width(s, SemanticRole::Text, StyleRef::Note);
        let identity_width = if identity.is_empty() {
            0.
        } else {
            note_width(&identity)
        };
        let shortcut_width = row.shortcut.map_or(0., |n| note_width(&panel_shortcut(n)));
        let pair_width = identity_width
            + shortcut_width
            + if identity_width > 0. && shortcut_width > 0. {
                TEXT_GAP
            } else {
                0.
            };
        let meta_below = pair_width > 0. && title_width + TEXT_GAP + pair_width > width;
        let shortcut_below = identity_width > 0. && shortcut_width > 0. && pair_width > width;

        let wrap = |text, style| {
            let (px, _) = crate::native_ui::text_paint_metrics(SemanticRole::Text, style);
            crate::tray_raster::ui_text_wrap_ranges(text, px, (width - 1.).max(1.))
        };
        let title_lines = if title_width > width {
            wrap(title, StyleRef::Primary)
        } else {
            Vec::new()
        };
        let title_cut = || {
            !title_lines.is_empty()
                && (title_lines.len() > 2
                    || title_lines.iter().any(|range| {
                        text_width(
                            title[range.clone()].trim(),
                            SemanticRole::Text,
                            StyleRef::Primary,
                        ) > width
                    }))
        };
        let original_lines = row
            .original_topic
            .as_deref()
            .filter(|original| details && (*original != title || title_cut()))
            .map(|original| wrap(original, StyleRef::Note))
            .unwrap_or_default();
        Self {
            second_title: title_lines.get(1).map(|range| range.start),
            meta_below,
            shortcut_below,
            original_lines,
        }
    }

    fn extra_height(&self, m: Measure) -> f32 {
        let notes = usize::from(self.meta_below)
            + usize::from(self.shortcut_below)
            + if self.original_lines.is_empty() {
                0
            } else {
                1 + self.original_lines.len()
            };
        let extra_title = if self.second_title.is_some() {
            m.title_line
        } else {
            0.
        };
        extra_title + notes as f32 * m.detail_line
    }

    fn lines(&self, row: &NavRow, key: &str, cx: RowCx<'_>, width: f32) -> Vec<UiNode> {
        let m = cx.m;
        let title = row.panel_title();
        let identity = panel_identity(row, cx.preview);
        let shortcut = row.shortcut.map(panel_shortcut);
        let measure = |s: &str| text_width(s, SemanticRole::Text, StyleRef::Note);
        let shortcut_w = shortcut.as_deref().map_or(0., measure);
        let mut meta_room = width;
        if !self.meta_below {
            meta_room -= text_width(title, SemanticRole::Text, StyleRef::Primary) + TEXT_GAP;
        }
        if shortcut.is_some() && !self.shortcut_below {
            meta_room -= shortcut_w + TEXT_GAP;
        }
        let age = age_words(cx.clock_ms, row.moved_ms);
        let meta = if !age.is_empty() && identity.is_empty() && measure(&age) <= meta_room {
            age
        } else if !m.narrow
            && !age.is_empty()
            && !identity.is_empty()
            && cx.preview != Some(row.session)
        {
            let dated = format!("{identity} · {age}");
            if measure(&dated) <= meta_room {
                dated
            } else {
                identity.into_owned()
            }
        } else {
            identity.into_owned()
        };
        let line = |suffix: &str, h: f32, children: Vec<UiNode>| {
            plain_group(format!("{key}/{suffix}"))
                .layout(Layout::row().gap(TEXT_GAP).height(Length::Fixed(h)))
                .children(children)
        };
        let text = |suffix: &str, value: String, style, width| {
            words(
                format!("{key}/{suffix}"),
                value,
                SemanticRole::Text,
                style,
                width,
            )
        };
        let (first, rest) = title.split_at(self.second_title.unwrap_or(title.len()));
        let mut heading = vec![text(
            "title",
            first.trim_end().into(),
            StyleRef::Primary,
            Length::Fill,
        )];
        let mut metadata = Vec::new();
        if !meta.is_empty() {
            let node = text(
                "meta",
                one_line(&meta, SemanticRole::Text, StyleRef::Note, meta_room),
                StyleRef::Note,
                if self.meta_below {
                    Length::Fill
                } else {
                    Length::Fixed(measure(&meta))
                },
            );
            if self.meta_below {
                metadata.push(node);
            } else {
                heading.push(node);
            }
        }
        let mut chord_line = None;
        if let Some(chord) = shortcut {
            let node = text("shortcut", chord, StyleRef::Note, Length::Fixed(shortcut_w));
            if self.shortcut_below {
                chord_line = Some(line(
                    "chord",
                    m.detail_line,
                    vec![plain_group(format!("{key}/chord-space")), node],
                ));
            } else if self.meta_below {
                if metadata.is_empty() {
                    metadata.push(plain_group(format!("{key}/chord-space")));
                }
                metadata.push(node);
            } else {
                heading.push(node);
            }
        }
        let mut lines = vec![line("first", m.title_line, heading)];
        if !rest.is_empty() {
            let continued = one_line(rest.trim(), SemanticRole::Text, StyleRef::Primary, width);
            lines.push(line(
                "continued",
                m.title_line,
                vec![text(
                    "title-continued",
                    continued,
                    StyleRef::Primary,
                    Length::Fill,
                )],
            ));
        }
        if self.meta_below {
            lines.push(line("identity", m.detail_line, metadata));
        }
        lines.extend(chord_line);
        if !self.original_lines.is_empty()
            && let Some(topic) = &row.original_topic
        {
            let note = |suffix: &str, content: &str| {
                text(suffix, content.into(), StyleRef::Note, Length::Fill)
                    .layout(Layout::default().height(Length::Fixed(m.detail_line)))
            };
            lines.push(note("origin-label", "Original topic"));
            for (i, range) in self.original_lines.iter().enumerate() {
                lines.push(note(&format!("origin/{i}"), topic[range.clone()].trim()));
            }
        }
        lines
    }
}

/// What every row of one page is drawn with.
#[derive(Clone, Copy)]
struct RowCx<'a> {
    resources: &'a resources::Panel,
    scales: resources::Scales,
    resources_at_ns: u64,
    m: Measure,
    clock_ms: u64,
    cursor: Option<u64>,
    at_hand: bool,
    numbered: bool,
    chart_details: bool,
    /// The widest of the five words, so the latest words start in one column.
    word_w: f32,
    docked: bool,
    preview: Option<u64>,
}

fn press(row: &NavRow, listed: Listed, cx: RowCx<'_>) -> UiNode {
    let key = format!("{ROW}{}", row.session);
    let selected = cx.cursor == Some(row.session);
    let mut label = row.spoken(listed, cx.clock_ms, cx.numbered);
    if let Some(number) = row.shortcut.filter(|_| cx.numbered) {
        let chord = if cfg!(target_os = "macos") {
            "Command"
        } else {
            "Control Shift"
        };
        label.push_str(&format!(", {chord} {number}"));
    }
    if cx.preview == Some(row.session) {
        label = format!("Preview, {label}. Double-click to keep as a tab.");
    }
    let mut button = ButtonSpec::new(label).visual_label(String::new());
    if cx.numbered {
        button = button.description(cx.resources.get(&row.session).map_or_else(
            || "No resource observation for this session.".into(),
            |readings| readings.description(cx.scales, cx.resources_at_ns),
        ));
    }
    UiNode::new(
        key.clone(),
        UiContent::Button(
            Control::new(button, ActionId::new(key))
                .state(ControlState {
                    selected,
                    muted: selected && !cx.at_hand,
                    ..ControlState::default()
                })
                .style(StyleRef::Navigation),
        ),
    )
}

fn navigator_header(row: &NavRow, key: &str, cx: RowCx<'_>, text_w: f32) -> UiNode {
    let m = cx.m;
    let age = age_words(cx.clock_ms, row.moved_ms);
    let fit = |text: &str, width| one_line(text, SemanticRole::Text, StyleRef::Note, width);
    let measure = |text: &str| text_width(text, SemanticRole::Text, StyleRef::Note);
    let meta_limit = text_w * 0.45;
    let meta = match &row.program {
        program if cx.preview == Some(row.session) => {
            let label = program
                .as_ref()
                .map_or_else(|| "Preview".to_owned(), |name| format!("Preview · {name}"));
            fit(&label, meta_limit)
        }
        Some(program) if !m.narrow && !age.is_empty() => {
            let suffix = format!(" \u{b7} {age}");
            let name = fit(program, meta_limit - measure(&suffix));
            if name.is_empty() {
                fit(&age, meta_limit)
            } else {
                format!("{name}{suffix}")
            }
        }
        Some(program) => fit(program, meta_limit),
        None => fit(&age, meta_limit),
    };
    let meta_w = measure(&meta).min(meta_limit);
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
            meta,
            SemanticRole::Text,
            StyleRef::Note,
            Length::Fixed(meta_w),
        ));
    }
    plain_group(format!("{key}/first"))
        .layout(
            Layout::row()
                .gap(TEXT_GAP)
                .height(Length::Fixed(m.title_line)),
        )
        .children(first)
}

/// A single row action under its painted details and, in the local panel, its resource traces.
/// A navigator entry that has children also offers its disclosure button.
fn row_node(
    entry: &NavEntry,
    listed: Listed,
    cx: RowCx<'_>,
    header: Option<&PanelHeader>,
    height: f32,
) -> UiNode {
    let m = cx.m;
    let row = &*entry.row;
    let key = format!("navigator/item/{}", row.session);
    let (indent, leading) = if cx.docked {
        (0.0, ROW_END)
    } else {
        (
            INDENT * f32::from(entry.depth.min(INDENT_DEPTHS)),
            FOLD_SLOT,
        )
    };
    let text_w = (m.inner - indent - leading - ROW_END).max(1.0);

    let mut lines = match header {
        Some(header) => header.lines(row, &key, cx, text_w),
        None => vec![navigator_header(row, &key, cx, text_w)],
    };

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

    lines.push(second);
    if cx.numbered {
        let empty = resources::Readings::default();
        lines.push(resources::charts(
            &key,
            cx.resources.get(&row.session).unwrap_or(&empty),
            cx.scales,
            cx.chart_details.then_some(cx.resources_at_ns),
            text_w,
            m.detail_line,
        ));
    }
    let band = plain_group(format!("{key}/band"))
        .layout(Layout::column().padding(Insets {
            top: ROW_PAD,
            right: ROW_END,
            bottom: ROW_PAD,
            left: leading,
        }))
        .children(lines)
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
                .height(Length::Fixed(height))
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
    let frame = Frame::of(tree, view, width, height);
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
    let title = UiNode::new(
        "navigator/title",
        UiContent::Text(TextSpec::heading(one_line(
            if view.docked { "Sessions" } else { app.title() },
            SemanticRole::Heading,
            StyleRef::Primary,
            m.inner,
        ))),
    )
    .layout(Layout::default().height(Length::Fixed(m.title_h)));
    let heading = if view.sessions.numbered {
        let button = ButtonSpec::new("Session details")
            .description("Show the full original topic, chart ranges and time since collection.");
        let label_width = text_width("Details", SemanticRole::Button, StyleRef::Chip) + 24.;
        let title_width = text_width("Sessions", SemanticRole::Heading, StyleRef::Primary);
        let (width, button) = if title_width + label_width + TEXT_GAP <= m.inner {
            (label_width, button.visual_label("Details"))
        } else {
            (
                m.title_h,
                button.visual_icon(crate::native_ui::ButtonIcon::Info),
            )
        };
        let details = UiNode::new(
            CHART_DETAILS,
            UiContent::Button(
                Control::new(button, ActionId::new(CHART_DETAILS))
                    .state(ControlState {
                        expanded: Some(view.chart_details),
                        selected: view.chart_details,
                        ..ControlState::default()
                    })
                    .style(StyleRef::Chip),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(width))
                .height(Length::Fixed(m.title_h)),
        );
        plain_group("navigator/heading")
            .layout(Layout::row().height(Length::Fixed(m.title_h)))
            .children(vec![title, details])
    } else {
        title
    };
    let header = plain_group("navigator/header")
        .layout(
            Layout::column()
                .gap(HEADER_GAP)
                .width(Length::Fixed(m.inner))
                .height(Length::Fixed(m.header_h)),
        )
        .children(std::iter::once(heading).chain(filter).collect());

    let shown = frame.painted(scroll);
    let visible = if view.sessions.numbered {
        &frame.rows[shown.clone()]
    } else {
        &[]
    };
    let scales = resources::Scales::visible(
        visible
            .iter()
            .filter_map(|listed| app.resources.get(&tree.entries[listed.at].row.session)),
    );
    let cx = RowCx {
        resources: &app.resources,
        scales,
        resources_at_ns: app.resources_at_ns,
        m,
        clock_ms: app.clock_ms,
        cursor: view.cursor,
        at_hand: view.at_hand,
        preview: view.sessions.preview,
        docked: view.docked,
        numbered: view.sessions.numbered,
        chart_details: view.chart_details,
        word_w: Word::ALL
            .iter()
            .map(|word| text_width(word.word(), SemanticRole::Status, word.style()))
            .fold(0.0, f32::max),
    };
    let group_heading = |visible: bool| {
        let name = if view.docked {
            "Other sessions"
        } else {
            app.title()
        };
        let heading = UiNode::new(
            "navigator/machine",
            UiContent::Text(TextSpec {
                text: one_line(name, SemanticRole::Heading, StyleRef::Quiet, m.inner),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(m.group_line)));
        if visible {
            heading
        } else {
            heading.offscreen()
        }
    };
    let group_gap = || {
        let gap = plain_group("navigator/machine/gap")
            .layout(Layout::column().height(Length::Fixed(GROUP_GAP)));
        if view.docked {
            let rule = UiNode::new(
                "navigator/divider",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group).style(StyleRef::Cell)),
            )
            .layout(Layout::default().height(Length::Fixed(1.0)));
            gap.children(vec![rule]).paint_only_tree()
        } else {
            gap.paint_only()
        }
    };
    let mut children = Vec::with_capacity(frame.rows.len() + 2);
    for (at, listed) in frame.rows.iter().enumerate() {
        let entry = &tree.entries[listed.at];
        if frame.machine_from == Some(at) {
            if at > 0 {
                children.push(group_gap());
            }
            children.push(group_heading(frame.heads_machine(scroll)));
        }
        let height = frame.height(at);
        children.push(if shown.contains(&at) {
            row_node(
                entry,
                *listed,
                cx,
                frame.headers.get(at).map(Arc::as_ref),
                height,
            )
        } else {
            // A row off the list's screen is its press alone: `inspect`, Tab and the arrows
            // reach it, and the page compiles one screen of rows however many it lists.
            press(&entry.row, *listed, cx)
                .layout(Layout::default().height(Length::Fixed(height)))
                .offscreen()
        });
    }
    if frame.machine_from == Some(frame.rows.len()) {
        if !frame.rows.is_empty() {
            children.push(group_gap());
        }
        children.push(group_heading(true));
    }
    if frame.rows.is_empty() {
        let none = if !view.filtered() {
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

    if !view.sessions.windows.is_empty() {
        children.push(
            plain_group("navigator/windows/gap")
                .layout(Layout::default().height(Length::Fixed(GROUP_GAP)))
                .paint_only(),
        );
        children.push(
            UiNode::new(
                "navigator/windows",
                UiContent::Text(TextSpec::heading("Windows")),
            )
            .layout(Layout::default().height(Length::Fixed(m.group_line))),
        );
        for row in &view.sessions.windows {
            let key = format!("{WINDOW}{}", row.view);
            let button = Control::new(
                ButtonSpec::new(row.label.clone()),
                ActionId::new(key.clone()),
            )
            .style(StyleRef::Navigation)
            .state(ControlState {
                selected: row.selected,
                ..ControlState::default()
            });
            children.push(
                UiNode::new(key, UiContent::Button(button))
                    .layout(Layout::default().height(Length::Fixed(m.title_h))),
            );
        }
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
