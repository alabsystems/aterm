// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The navigator's host (tracker ST-04, `docs/DESIGN-streams-2026-11-12.md` §3): where it opens,
//! what it lists, and when it reads.
//!
//! THE LIST IS PUBLISHED ON CHANGE, AND IT KEEPS NO POLLING TIMER. The loop thread marks what may
//! have moved where it already handles the event ([`App::navigator_mark_structure`],
//! [`App::navigator_mark_row`], [`App::navigator_mark_words`], [`App::navigator_note_output`]): a
//! mark is an insert into a set, and one branch while no navigator is open.
//! [`App::publish_navigators`] runs once a loop turn and returns at its first lines when nothing
//! is marked. A structure mark walks the windows into vectors of integers and stops there when
//! they equal the last walk.
//!
//! A ROW IS MARKED WHEN A FACT ITS TITLE OR WORD READS CHANGED, and a row is built only for a
//! marked session. Of a walk that moved, a row reads its session's seat ([`Seat`]): the sessions
//! new to the walk and those whose seat moved are marked, and no other. Every other fact is
//! marked where its event is handled. A row that reads as it did publishes nothing. A row's
//! latest words are read only while it is on a view's screen, at most once a second however fast
//! its session prints. The one deadline that is not an event's is the minute the ages are worded
//! at, armed only while a view is on screen.
//!
//! A VIEW'S REVISION NEVER MOVES ON SCREEN WITHOUT AN INVALIDATION AND A FRAME: the raster on
//! glass answers the pointer only while its stamp is the view's. A change a view paints is a
//! band at its revision; one it lists off its screen is a frame that rasters one pixel
//! ([`TREE_ONLY`]); one it does not list asks it for nothing. A view nobody sees moves its
//! revision with each change, so a reader of its page compiles the list as it stands, and is
//! drawn whole, with a frame asked for, when it shows.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::native_app::{AppKind, AppViewState, DamageRegion, NativeApp};
use crate::navigator::{
    NavAction, NavRow, NavTree, NavigatorViewState, SPLIT_COLS, Shows, band_of,
};
use crate::session_store::{SessionHandle, SessionState};
use crate::tab_model::{AppInstanceId, SplitAxis, View, ViewId};
use crate::task_record::TaskState;
use crate::{App, WindowId};

/// The least time between two reads of the latest words.
const NAV_WORDS_STEP: Duration = Duration::from_secs(1);
/// How often the ages are worded again.
const NAV_AGE_STEP: Duration = Duration::from_secs(60);
/// The page's title where the machine has no name.
const THIS_MACHINE: &str = "This machine";

/// The damage of a view whose compiled page moved and whose pixels did not: a row it lists off
/// its screen reads differently, which is a node a screen reader and `inspect` read. A page is
/// one compile, from which its pixels, the tree the pointer is routed by and the tree a client
/// reads all come, so the view owes a frame; this is the least a frame rasters. The frame
/// compiles the page, finds no paint node changed, and paints this one pixel as it was.
const TREE_ONLY: DamageRegion = DamageRegion::Rect {
    x: 0,
    y: 0,
    width: 1,
    height: 1,
};

/// What a row's title reads of a walk ([`App::tab_label_of_session`]): a session nothing has
/// titled is named by its tab's place on the strip (`tab 3`) while its tab is shown, and by what
/// it runs, at no place, while its tab is parked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seat {
    /// In a shown tab, at this index of its window's tabs.
    Shown(u32),
    /// In a parked tab.
    Parked,
}

/// What a row's latest words were read from. Equal stamps: the words are what they were.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WordsStamp {
    read: crate::survey::WordsRead,
    topic_ms: u64,
    screen_moves: u64,
    concluded: bool,
}

/// One view of the navigator, as the last structure pass found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NavView {
    window: WindowId,
    view: ViewId,
    /// On a window that is not occluded, in its active tab, not behind a zoomed sibling.
    visible: bool,
    /// Its window's keys go to it: it has its tab's focus in its window's front tab. A key a
    /// person types there, or one a driver sends that window (`aterm ctl … key`), reaches it.
    keys: bool,
    /// A person's keyboard is on it: `keys`, in the key window.
    at_hand: bool,
    /// The bits of the width and height it is laid out in while it shows, so a view that was
    /// resized is a view that changed.
    size: (u32, u32),
}

impl NavView {
    /// The size it is laid out in, where it shows.
    fn viewport(&self) -> Option<(f32, f32)> {
        self.visible
            .then(|| (f32::from_bits(self.size.0), f32::from_bits(self.size.1)))
    }
}

/// What the host has built and read for the list.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct NavWork {
    pub(crate) rows_built: u64,
    pub(crate) words_read: u64,
}

/// One row a view lists.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NavListed {
    pub(crate) session: u64,
    pub(crate) depth: u16,
    pub(crate) open: bool,
    pub(crate) under: usize,
    pub(crate) pinned: bool,
}

/// What may have moved since the list was last published, and what it was published from.
/// `App::nav` holds one only while a navigator is open.
#[derive(Debug, Default)]
pub(crate) struct NavWatch {
    /// The layout, the set of sessions, a focus or the machine's name may have moved.
    structure: bool,
    /// Sessions whose title, word, agent or creator may have moved.
    rows: BTreeSet<u64>,
    /// Sessions on a view's screen whose latest words may have moved.
    words: BTreeSet<u64>,
    /// Sessions read at the last step, read once more at the next: an agent's transcript read
    /// lands after the output that woke it.
    trailing: BTreeSet<u64>,
    /// A view scrolled, folded or filtered: which rows are on screen may have moved.
    screen_stale: bool,
    /// The sessions with a row on some view's screen.
    screen: BTreeSet<u64>,
    words_read_at: Option<Instant>,
    /// When the ages are next worded; `None` while no view is on screen.
    age_at: Option<Instant>,
    clock_ms: u64,
    /// The last walk: the terminal leaves of every window's shown tabs, then of its parked ones.
    pinned: Vec<u64>,
    parked: Vec<u64>,
    /// The last walk's seats, each terminal leaf's in the walk's order.
    seats: Vec<(u64, Seat)>,
    /// Each walked session's seat, as its row's title reads it.
    seat_of: HashMap<u64, Seat>,
    /// The sessions whose shell said a command was running when their row was last built
    /// ([`crate::vitals_jobs::JobFeed::runs`]).
    said: BTreeSet<u64>,
    /// Whether Tab Status was on when the rows were last marked for it: which of the two a
    /// row's `running` is read from.
    observed: bool,
    /// The views whose revision moved while nobody saw them: each is drawn whole, and asked
    /// for a frame, when it shows.
    owed: Vec<ViewId>,
    /// The next walk is made into these, so a walk that found nothing new allocates nothing.
    walk: NavWalk,
    views: Vec<NavView>,
    /// Whether the published order is one a view held while a person's keyboard was on it, not
    /// the walk's.
    held_order: bool,
    held: HashMap<u64, Arc<NavRow>>,
    stamps: HashMap<u64, WordsStamp>,
    machine: String,
    tree: Arc<NavTree>,
    #[cfg(test)]
    work: NavWork,
}

/// One walk of the windows ([`App::navigator_walk`]).
#[derive(Debug, Default)]
struct NavWalk {
    pinned: Vec<u64>,
    parked: Vec<u64>,
    seats: Vec<(u64, Seat)>,
    /// Where a walk that moved resolves each session's seat.
    seat_of: HashMap<u64, Seat>,
    views: Vec<NavView>,
}

impl NavWatch {
    fn idle(&self, now: Instant) -> bool {
        !(self.structure
            || self.screen_stale
            || !self.rows.is_empty()
            || self.words_due(now)
            || self.age_at.is_some_and(|at| now >= at))
    }

    fn words_owed(&self) -> bool {
        !(self.words.is_empty() && self.trailing.is_empty())
    }

    fn words_due(&self, now: Instant) -> bool {
        self.words_owed()
            && self
                .words_read_at
                .is_none_or(|at| now.saturating_duration_since(at) >= NAV_WORDS_STEP)
    }
}

/// Whether two arrangements list the same sessions at the same places.
fn same_shape(a: &NavTree, b: &NavTree) -> bool {
    a.machine == b.machine
        && a.entries.len() == b.entries.len()
        && a.entries.iter().zip(&b.entries).all(|(a, b)| {
            a.row.session == b.row.session && a.parent == b.parent && a.group == b.group
        })
}

fn concluded(state: Option<TaskState>) -> bool {
    matches!(
        state,
        Some(TaskState::Done | TaskState::Exited | TaskState::Failed)
    )
}

impl App {
    /// The layout, the set of sessions or a focus may have moved.
    pub(crate) fn navigator_mark_structure(&mut self) {
        if let Some(nav) = self.nav.as_deref_mut() {
            nav.structure = true;
        }
    }

    /// `session`'s title, state or creator may have moved.
    pub(crate) fn navigator_mark_row(&mut self, session: u64) {
        if let Some(nav) = self.nav.as_deref_mut() {
            nav.rows.insert(session);
        }
    }

    /// `session`'s agent's transcript was read or it posted: its latest words may have moved. A
    /// session with no row on a screen is read when its row comes onto one.
    pub(crate) fn navigator_mark_words(&mut self, session: u64) {
        if let Some(nav) = self.nav.as_deref_mut()
            && nav.screen.contains(&session)
        {
            nav.words.insert(session);
        }
    }

    /// `session` printed: its latest words may have moved ([`Self::navigator_mark_words`]).
    /// With Tab Status off nothing observes its shell, and the shell's own word on whether a
    /// command runs arrives as output: it is read here, off the term lock, and a row built from
    /// the other answer is marked. The output pass calls this after it stores the session's
    /// output latch (`SessionPool::output_pass`): a word the reader feeds after that store posts
    /// a wake of its own or leaves the hold dirty, and one fed before it is the word read here.
    pub(crate) fn navigator_note_output(&mut self, session: u64) {
        let Some(nav) = self.nav.as_deref_mut() else {
            return;
        };
        if nav.screen.contains(&session) {
            nav.words.insert(session);
        }
        if !self.config.tab_status_or_default()
            && nav.held.contains_key(&session)
            && self
                .pool
                .get(session)
                .is_some_and(|live| live.ctx.job_feed.runs() != nav.said.contains(&session))
        {
            nav.rows.insert(session);
        }
    }

    /// The configuration was applied. A row reads whether its shell runs a command from what
    /// Tab Status observed while that is on and from the shell's own word while it is off, so
    /// turning it on or off marks every row.
    pub(crate) fn navigator_note_tab_status(&mut self) {
        let observed = self.config.tab_status_or_default();
        if let Some(nav) = self.nav.as_deref_mut()
            && nav.observed != observed
        {
            nav.observed = observed;
            nav.rows.extend(nav.held.keys().copied());
        }
    }

    /// A key, a press or a wheel reached a navigator view: its scroll, folds or filter may have
    /// put other rows on its screen.
    pub(crate) fn navigator_view_moved(&mut self) {
        if let Some(nav) = self.nav.as_deref_mut() {
            nav.screen_stale = true;
        }
    }

    /// A frame laid `view` out in `viewport`. Where that is not the size the list was last
    /// published for (the window, a divider or the text's size moved), which rows are on its
    /// screen is read again.
    pub(crate) fn navigator_note_viewport(
        &mut self,
        view: ViewId,
        viewport: crate::native_ui::LogicalRect,
    ) {
        if let Some(nav) = self.nav.as_deref_mut()
            && let Some(at) = nav.views.iter().find(|at| at.view == view)
            && at.viewport() != Some((viewport.width, viewport.height))
        {
            nav.structure = true;
        }
    }

    /// Whether `view` is a page that stands beside the panes of its tab as a sidebar: its edge
    /// on them is a hairline, not the pane divider.
    pub(crate) fn view_is_sidebar_page(&self, view: ViewId) -> bool {
        self.nav.is_some() && self.view_is_unrestored_page(view)
    }

    /// When the latest words marked on a screen are next read: at once, or a step after the
    /// last read. `None` while none are owed, so the list arms no deadline of its own.
    pub(crate) fn navigator_words_wake(&self) -> Option<Instant> {
        let nav = self.nav.as_deref()?;
        nav.words_owed().then(|| {
            nav.words_read_at
                .map_or_else(Instant::now, |at| at + NAV_WORDS_STEP)
        })
    }

    /// The next deadline the list owes: marked words on a screen, or the ages' minute.
    pub(crate) fn navigator_wake(&self) -> Option<Instant> {
        let nav = self.nav.as_deref()?;
        [self.navigator_words_wake(), nav.age_at]
            .into_iter()
            .flatten()
            .min()
    }

    /// The page's title: the machine's name as the machine writes it, else its host's name, else
    /// `This machine`.
    pub(crate) fn machine_title(&self) -> String {
        if let Some(name) = self.machine_name_now() {
            return name;
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(name) = aterm_types::dirs::state_dir()
            .and_then(|state| aterm_agent::machine_name::file::read(&state).ok().flatten())
            .map(|name| name.name())
            .filter(|name| !name.is_empty() && name != "unnumbered")
        {
            return name;
        }
        aterm_uds::process::lan_host_name()
            .map(|host| host.trim_end_matches(".local").trim().to_owned())
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| THIS_MACHINE.to_owned())
    }

    /// The machine's own name where this process already holds it (macOS: the title bar's).
    fn machine_name_now(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            self.machine_view
                .as_ref()
                .map(|machine| machine.name.as_str())
                .filter(|name| !name.is_empty() && *name != "unnumbered")
                .map(str::to_owned)
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn navigator_instance_id(&self) -> Option<AppInstanceId> {
        self.native_runtime.instance_by_kind(AppKind::Navigator)
    }

    fn is_navigator_view(&self, view: ViewId, instance: AppInstanceId) -> bool {
        matches!(self.view_store.get(view), Some(View::Native(native)) if native.instance == instance)
    }

    /// Whether `view` is a navigator's: a page the restore manifest and Reopen Closed Tab leave
    /// out, since what it lists is the machine's sessions and not a document of its own.
    pub(crate) fn view_is_unrestored_page(&self, view: ViewId) -> bool {
        self.navigator_instance_id()
            .is_some_and(|instance| self.is_navigator_view(view, instance))
    }

    /// Open the navigator in the front window: as the left pane of the active tab, about
    /// [`SPLIT_COLS`] cells wide, where `split` and the tab is terminals with room for it; else as
    /// a tab of its own. A tab that already holds it gets the keys on it. Whether it is a split.
    pub(crate) fn open_navigator(&mut self, split: bool) -> Result<bool, String> {
        let wid = self.frontmost_window.ok_or("No window for the navigator")?;
        let person_in = self.focused_session_id(wid).or_else(|| {
            self.windows
                .get(&wid)?
                .active_terminal
                .as_ref()
                .map(|terminal| terminal.session)
        });
        let instance = match self.navigator_instance_id() {
            Some(instance) => instance,
            None => self
                .native_runtime
                .insert_instance(NativeApp::Navigator(Box::default()))
                .map_err(|error| format!("The navigator could not open: {error:?}"))?,
        };
        let state =
            || AppViewState::Navigator(Box::new(NavigatorViewState::starting_in(person_in)));
        let ws = self
            .windows
            .get(&wid)
            .ok_or("No window for the navigator")?;
        let here = |tab: &crate::tab_model::Tab| {
            let mut found = None;
            tab.root.any_leaf(&mut |&view| {
                found = self.is_navigator_view(view, instance).then_some(view);
                found.is_some()
            });
            found
        };
        // A split takes its cells from the whole tab: every pane beside it keeps its least width,
        // a native page keeps the whole view its window's least size is measured for, and a
        // session another view shows too is never re-gridded for it, whichever pane holds it.
        let fits = ws.tab_set.active().is_some_and(|tab| {
            let plain = !tab.root.any_leaf(&mut |view| {
                self.view_store
                    .get(*view)
                    .copied()
                    .and_then(View::terminal_session)
                    .is_none_or(|session| self.pool.views(session).is_some_and(|views| views > 1))
            });
            let rest = usize::from(ws.cols.saturating_sub(SPLIT_COLS + 1));
            plain
                && rest >= usize::from(crate::pane::MIN_PANE_COLS) * tab.root.len()
                && ws.rows > crate::pane::MIN_PANE_ROWS
        });
        // The active tab's own navigator, then (for a tab, or a split with no room) a tab that
        // holds nothing else: the window never grows a second navigator tab.
        let active = ws.tab_set.active().and_then(|tab| Some((tab, here(tab)?)));
        let existing = active.or_else(|| {
            (!(split && fits))
                .then(|| {
                    ws.tab_set
                        .tabs()
                        .iter()
                        .filter(|tab| tab.root.len() == 1)
                        .find_map(|tab| Some((tab, here(tab)?)))
                })
                .flatten()
        });
        let existing = existing.map(|(tab, view)| (tab.id, view, tab.root.len() > 1));
        let cols = ws.cols;
        let is_split = match existing {
            Some((tab, view, is_split)) => {
                {
                    let mut window = self.windows.get_mut(&wid).ok_or("The window closed")?;
                    window.tab_set.switch_to(tab);
                    if let Some(tab) = window.tab_set.active_mut() {
                        tab.set_focus(view);
                    }
                }
                self.resync_active_or_window(wid);
                is_split
            }
            None if split && fits => {
                let ratio = f32::from(SPLIT_COLS) / f32::from(cols.saturating_sub(1).max(1));
                self.seat_native_first(wid, SplitAxis::Horizontal, instance, state(), ratio)
                    .map_err(|error| format!("The navigator could not open: {error:?}"))?;
                true
            }
            None => {
                let title = self.machine_title();
                let presentation = crate::tab_model::TabPresentation {
                    tooltip: Some(title.clone()),
                    title,
                    icon: Some(crate::tab_model::TabIconKind::Workers),
                    indicators: crate::tab_model::TabIndicators::default(),
                    conn: None,
                    closable: true,
                };
                let (tab, view) = self
                    .install_native_tab(wid, instance, state(), presentation)
                    .map_err(|error| format!("The navigator could not open: {error:?}"))?;
                {
                    let mut window = self.windows.get_mut(&wid).ok_or("The window closed")?;
                    window.tab_set.switch_to(tab);
                    if let Some(tab) = window.tab_set.active_mut() {
                        tab.set_focus(view);
                    }
                }
                self.resync_active_or_window(wid);
                false
            }
        };
        {
            let mut window = self.windows.get_mut(&wid).ok_or("The window closed")?;
            crate::app_tabs::cancel_pending_close(&mut window);
            window.last_present = None;
        }
        let observed = self.config.tab_status_or_default();
        let nav = self.nav.get_or_insert_with(|| {
            Box::new(NavWatch {
                observed,
                ..NavWatch::default()
            })
        });
        nav.structure = true;
        self.publish_navigators(Instant::now());
        self.sync_active_session();
        Ok(is_split)
    }

    /// Every view of the navigator into `views`, with whether it shows, the size it shows at,
    /// whether its window's keys go to it and whether a person's keyboard is on it.
    fn navigator_views_now(&self, instance: AppInstanceId, views: &mut Vec<NavView>) {
        views.clear();
        for (&window, ws) in &self.windows {
            let active = ws.tab_set.active_id();
            for tab in ws.tab_set.tabs() {
                let front = active == Some(tab.id);
                tab.root.visit(&mut |&view| {
                    if self.is_navigator_view(view, instance) {
                        let focused = tab.focus == view;
                        let viewport = (front && !ws.occluded && (!tab.zoomed || focused))
                            .then(|| self.native_ui_viewport_for(window, view).ok())
                            .flatten();
                        let keys = front && focused;
                        views.push(NavView {
                            window,
                            view,
                            visible: viewport.is_some(),
                            keys,
                            at_hand: keys && ws.focused,
                            size: viewport.map_or((0, 0), |viewport| {
                                (viewport.width.to_bits(), viewport.height.to_bits())
                            }),
                        });
                    }
                });
            }
        }
    }

    /// The terminal leaves of every window's shown tabs (windows in their order, tabs in the
    /// strip's, panes in the tab's) and of every window's parked tabs, and each leaf's seat in
    /// the order walked, into `walk`. A person's tab stands for a pinned session here:
    /// `SessionMeta.pinned` is not written for one yet (decision ST-D22; tracker ST-07 makes a
    /// person's open write it).
    fn navigator_walk(&self, walk: &mut NavWalk) {
        walk.pinned.clear();
        walk.parked.clear();
        walk.seats.clear();
        for ws in self.windows.values() {
            for (place, tab) in ws.tab_set.tabs().iter().enumerate() {
                let (out, seat) = if tab.parked {
                    (&mut walk.parked, Seat::Parked)
                } else {
                    let place = u32::try_from(place).unwrap_or(u32::MAX);
                    (&mut walk.pinned, Seat::Shown(place))
                };
                let seats = &mut walk.seats;
                tab.root.visit(&mut |&view| {
                    if let Some(session) = self
                        .view_store
                        .get(view)
                        .copied()
                        .and_then(View::terminal_session)
                    {
                        out.push(session);
                        seats.push((session, seat));
                    }
                });
            }
        }
    }

    fn navigator_handle(&self, session: u64) -> Option<SessionHandle> {
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .filter(|handle| handle.state != SessionState::Exited)
            .cloned()
    }

    /// One session's row as it reads now, with `line` and `moved_ms` as its last read left them;
    /// `said` is its shell's own word on whether a command runs. The store's lock is not held:
    /// the row reads it for its creator.
    fn navigator_row(&self, handle: &SessionHandle, line: Option<String>, said: bool) -> NavRow {
        let (task, taken_off, from, task_agent, gone, asks, agent_word, program) = {
            let timeline = handle
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let task = timeline.task();
            (
                task.map(|task| task.state),
                task.is_some_and(|task| {
                    matches!(
                        task.why,
                        Some(crate::task_record::WHY_OFF | crate::task_record::WHY_OFF_CTL)
                    )
                }),
                task.map(|task| task.from.clone())
                    .filter(|from| from != crate::task_record::FROM_CTL),
                task.and_then(|task| task.agent),
                task.filter(|task| crate::native_workers::agent_gone(task))
                    .and_then(crate::app_take_over::worker_label),
                timeline.need().need.is_some(),
                timeline.shown_agent_word().0,
                crate::harness_host::agent_of(timeline.agent()),
            )
        };
        let session = handle.local_id;
        // A worker whose agent has gone is titled by its shell's folder: its row names what it
        // ran, unless a role or a pinned title names the session (as its Workers row does).
        let title = match gone {
            Some(name) if self.named_label_of_session(session).is_none() => name,
            _ => self.tab_label_of_session(session),
        };
        let creator = from.and_then(|from| {
            self.local_session_of(&aterm_session::SessionId::new(&from))
                .filter(|creator| *creator != session)
        });
        NavRow {
            session,
            title,
            agent: task_agent
                .or(program)
                .map(|program| program.name().to_owned()),
            word: crate::navigator::word_of(&crate::navigator::WordFacts {
                task,
                taken_off,
                asks,
                agent_word,
                // Tab Status reads a shell that says nothing of itself too (by the program that
                // holds its terminal), so its reading stands while it is on. Off, nothing
                // observes the session, and the shell's own word is the fact.
                running: if self.config.tab_status_or_default() {
                    self.session_status
                        .status(session)
                        .is_some_and(|status| crate::session_status::status_indicators(status).busy)
                } else {
                    said
                },
            }),
            line,
            moved_ms: handle.ctx.screen_moves.moved_ms(),
            creator,
        }
    }

    /// Read `session`'s latest words where what they are read from moved since the last read.
    /// The row as it reads after, where a word of it differs.
    fn navigator_read_words(&self, nav: &mut NavWatch, session: u64) -> Option<Arc<NavRow>> {
        let handle = self.navigator_handle(session)?;
        let held = Arc::clone(nav.held.get(&session)?);
        let (read, task) = {
            let timeline = handle
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            (
                timeline.words_read(),
                timeline.task().map(|task| task.state),
            )
        };
        let stamp = WordsStamp {
            read,
            topic_ms: handle
                .ctx
                .fabric
                .last_topic_post()
                .map_or(0, |post| post.at_ms),
            screen_moves: handle.ctx.screen_moves.count(),
            concluded: concluded(task),
        };
        #[cfg(test)]
        {
            nav.work.words_read += 1;
        }
        if nav.stamps.get(&session) == Some(&stamp) {
            return None;
        }
        let Ok(line) = crate::survey::last_words_now(&handle, stamp.concluded) else {
            // The screen could not be read now, which says nothing of what it holds: the row
            // keeps its line, and the next step reads it.
            nav.words.insert(session);
            return None;
        };
        nav.stamps.insert(session, stamp);
        let moved_ms = handle.ctx.screen_moves.moved_ms();
        if held.line == line
            && crate::navigator::age_words(nav.clock_ms, moved_ms)
                == crate::navigator::age_words(nav.clock_ms, held.moved_ms)
        {
            // The same words at the same age: the row draws as it did.
            return None;
        }
        Some(Arc::new(NavRow {
            line,
            moved_ms,
            ..(*held).clone()
        }))
    }

    /// Publish the list where something marked did move, and repaint what moved: the page whole
    /// where its arrangement changed, the changed rows' band where only their words did.
    pub(crate) fn publish_navigators(&mut self, now: Instant) {
        if self.nav.as_deref().is_none_or(|nav| nav.idle(now)) {
            return;
        }
        let Some(instance) = self.navigator_instance_id() else {
            self.nav = None;
            return;
        };
        let Some(mut nav) = self.nav.take() else {
            return;
        };
        let mut changed: BTreeSet<u64> = BTreeSet::new();
        let mut rearrange = false;
        let mut facts = false;

        // Structure: a walk of integers, compared with the last in place.
        if std::mem::take(&mut nav.structure) {
            facts = true;
            let mut walk = std::mem::take(&mut nav.walk);
            self.navigator_views_now(instance, &mut walk.views);
            if walk.views != nav.views {
                nav.screen_stale = true;
                std::mem::swap(&mut walk.views, &mut nav.views);
                nav.owed
                    .retain(|view| nav.views.iter().any(|at| at.view == *view));
            }
            self.navigator_walk(&mut walk);
            if walk.seats != nav.seats {
                // A session's seat is its tab's place while the tab is shown, and no place
                // while it is parked, as its title reads it ([`Seat`]). A session another
                // window shows too reads as the label does: parked where any tab that holds it
                // is, else at the first place walked.
                walk.seat_of.clear();
                for (session, seat) in &walk.seats {
                    let held = walk.seat_of.entry(*session).or_insert(*seat);
                    if *seat == Seat::Parked {
                        *held = Seat::Parked;
                    }
                }
                // The sessions new to the walk and those whose seat moved are read again. A
                // parked session's seat holds no place, so a tab that opens, closes or moves
                // ahead of it leaves its row alone.
                nav.rows.extend(
                    walk.seat_of
                        .iter()
                        .filter(|(session, seat)| nav.seat_of.get(*session) != Some(*seat))
                        .map(|(session, _)| *session),
                );
                std::mem::swap(&mut walk.seat_of, &mut nav.seat_of);
                std::mem::swap(&mut walk.seats, &mut nav.seats);
                // The arrangement reads the two orders, which a place alone does not move.
                if walk.pinned != nav.pinned || walk.parked != nav.parked {
                    std::mem::swap(&mut walk.pinned, &mut nav.pinned);
                    std::mem::swap(&mut walk.parked, &mut nav.parked);
                    rearrange = true;
                }
            }
            nav.walk = walk;
            let machine = self.machine_name_now();
            if nav.machine.is_empty() || machine.as_ref().is_some_and(|name| *name != nav.machine) {
                nav.machine = machine.unwrap_or_else(|| self.machine_title());
                rearrange = true;
            }
        }
        // A list holds its order while a person's keyboard is on it, and takes the strip's once
        // it goes elsewhere: a list in a window that is not the key one holds nothing.
        let keys = nav.views.iter().any(|view| view.at_hand);
        rearrange |= nav.held_order && !keys;

        // Rows: only the marked and the new, and only of sessions the walk holds (a session no
        // tab holds is no row, so a mark for one builds nothing).
        for session in std::mem::take(&mut nav.rows) {
            let handle = nav
                .seat_of
                .contains_key(&session)
                .then(|| self.navigator_handle(session))
                .flatten();
            let Some(handle) = handle else {
                nav.said.remove(&session);
                if nav.held.remove(&session).is_some() {
                    nav.stamps.remove(&session);
                    rearrange = true;
                }
                continue;
            };
            let said = handle.ctx.job_feed.runs();
            if said {
                nav.said.insert(session);
            } else {
                nav.said.remove(&session);
            }
            let held = nav.held.get(&session);
            let row = self.navigator_row(&handle, held.and_then(|row| row.line.clone()), said);
            #[cfg(test)]
            {
                nav.work.rows_built += 1;
            }
            if held.is_none_or(|held| **held != row) {
                rearrange |= held.is_none_or(|held| held.creator != row.creator);
                nav.held.insert(session, Arc::new(row));
                changed.insert(session);
                // A state edge may move which words the row reads (a concluded task's last say).
                if nav.screen.contains(&session) {
                    nav.words.insert(session);
                    nav.words_read_at = None;
                }
            }
        }

        let old = Arc::clone(&nav.tree);
        let mut tree = Arc::clone(&old);
        let mut reshaped = false;
        if rearrange {
            // The store is the judge of what is live: a tab whose session has no handle is no row.
            let arranged = crate::navigator::arrange(
                &nav.machine,
                &nav.pinned,
                &nav.parked,
                &nav.held,
                keys.then_some(&*old),
            );
            nav.held_order = keys;
            nav.held
                .retain(|session, _| arranged.index_of(*session).is_some());
            nav.stamps
                .retain(|session, _| arranged.index_of(*session).is_some());
            nav.said
                .retain(|session| arranged.index_of(*session).is_some());
            reshaped = !same_shape(&old, &arranged);
            if reshaped || !changed.is_empty() {
                tree = Arc::new(arranged);
            }
        } else if !changed.is_empty() {
            tree = Arc::new(old.with_rows(&nav.held));
        }
        if !Arc::ptr_eq(&tree, &old) {
            self.native_runtime
                .publish_navigator(instance, Arc::clone(&tree));
        }

        // Each view's own facts: where its window's keys are, whether a person's keyboard is on
        // it, and which session the person is in.
        let mut moved_views: Vec<ViewId> = Vec::new();
        if facts || reshaped {
            for at in &nav.views {
                let person_in = (!at.keys)
                    .then(|| self.focused_session_id(at.window))
                    .flatten();
                let moved = self.native_runtime.change_view_state(at.view, |state| {
                    let AppViewState::Navigator(state) = state else {
                        return false;
                    };
                    state.follow(at.keys, at.at_hand, person_in, &tree)
                });
                if moved {
                    moved_views.push(at.view);
                    nav.screen_stale = true;
                }
            }
        }

        // Which rows are on a screen, and the words of those that came onto one. A view is
        // told its size here, before its first frame: a list that opens on the session the
        // person is in opens with that row in its room.
        let mut arrived: Vec<u64> = Vec::new();
        if std::mem::take(&mut nav.screen_stale) || reshaped {
            let mut on_screen = Vec::new();
            for at in &nav.views {
                let Some((width, height)) = at.viewport() else {
                    continue;
                };
                let moved = self.native_runtime.change_view_state(at.view, |state| {
                    let AppViewState::Navigator(state) = state else {
                        return false;
                    };
                    let moved = state.lay(&tree, width, height);
                    state.on_screen(&tree, width, height, &mut on_screen);
                    moved
                });
                if moved && !moved_views.contains(&at.view) {
                    moved_views.push(at.view);
                }
            }
            let screen: BTreeSet<u64> = on_screen.into_iter().collect();
            arrived.extend(screen.difference(&nav.screen));
            nav.words.retain(|session| screen.contains(session));
            nav.trailing.retain(|session| screen.contains(session));
            nav.screen = screen;
        }
        let due = nav.words_due(now);
        let mut read: Vec<u64> = arrived;
        if due {
            read.extend(&nav.trailing);
            read.extend(&nav.words);
            nav.trailing = std::mem::take(&mut nav.words);
            nav.words_read_at = Some(now);
            read.sort_unstable();
            read.dedup();
        }
        let mut reworded: HashMap<u64, Arc<NavRow>> = HashMap::new();
        for session in read {
            if let Some(row) = self.navigator_read_words(&mut nav, session) {
                nav.held.insert(session, Arc::clone(&row));
                reworded.insert(session, row);
                changed.insert(session);
            }
        }

        // The ages: one wording a minute, while a view shows.
        let was_clock = nav.clock_ms;
        let mut aged = false;
        if !nav.views.iter().any(|at| at.visible) {
            nav.age_at = None;
        } else if nav.age_at.is_none_or(|at| now >= at) {
            aged = true;
            nav.age_at = Some(now + NAV_AGE_STEP);
            nav.clock_ms = aterm_gui_timing::wall_ms();
            // A row's time is read with its words; a row that only printed takes it here. The
            // row keeps a time that words, at this clock, as its session's own does; it is
            // repainted where that wording is not the one it was drawn with.
            for session in &nav.screen {
                let Some(held) = nav.held.get(session).map(Arc::clone) else {
                    continue;
                };
                let moved_ms = self
                    .navigator_handle(*session)
                    .map_or(held.moved_ms, |handle| handle.ctx.screen_moves.moved_ms());
                let age = crate::navigator::age_words(nav.clock_ms, moved_ms);
                if age != crate::navigator::age_words(nav.clock_ms, held.moved_ms) {
                    let row = Arc::new(NavRow {
                        moved_ms,
                        ..(*held).clone()
                    });
                    nav.held.insert(*session, Arc::clone(&row));
                    reworded.insert(*session, row);
                }
                if age != crate::navigator::age_words(was_clock, held.moved_ms) {
                    changed.insert(*session);
                }
            }
            self.native_runtime
                .set_navigator_clock(instance, nav.clock_ms);
        }
        if !reworded.is_empty() {
            tree = Arc::new(tree.with_rows(&reworded));
            self.native_runtime
                .publish_navigator(instance, Arc::clone(&tree));
        }
        nav.tree = Arc::clone(&tree);

        // Repaint: the page whole where its arrangement or its own facts moved; else the band of
        // the rows that changed, at the view's revision, so the page rasters that band alone.
        let (mut was_listed, mut now_listed) = (Vec::new(), Vec::new());
        for at in &nav.views {
            let moved = reshaped || moved_views.contains(&at.view);
            let revise = |runtime: &mut crate::native_app::NativeRuntime| {
                runtime.change_view_state(at.view, |state| {
                    let common = state.common_mut();
                    common.presentation_revision = common.presentation_revision.wrapping_add(1);
                    true
                });
            };
            let Some((width, height)) = at.viewport() else {
                // A VIEW NOBODY SEES IS DRAWN WHOLE WHEN IT SHOWS, where a row, the arrangement
                // or the clock its ages are worded against moved meanwhile (the ages' pass
                // words the rows of the views that show, and this view's are not among them).
                // Its revision moves with each such pass: no pointer reaches it, and a covered
                // window still draws for a client that waits on a frame, so the page that
                // frame compiled must not answer `inspect` for a later change. The view is
                // owed the invalidation and the frame by the pass that finds it shown.
                if moved || !changed.is_empty() || aged {
                    revise(&mut self.native_runtime);
                    if !nav.owed.contains(&at.view) {
                        nav.owed.push(at.view);
                    }
                }
                continue;
            };
            let owed = nav.owed.iter().position(|view| *view == at.view);
            if let Some(owed) = owed {
                nav.owed.swap_remove(owed);
            }
            let mut whole = moved || owed.is_some();
            let Some(AppViewState::Navigator(state)) = self.native_runtime.view_state(at.view)
            else {
                continue;
            };
            if !whole && !changed.is_empty() && state.filtered() {
                // A FILTERED LIST IS ITS MATCHES: a row whose change took it out of the filter,
                // or brought it in, moves every row after it, so the page is drawn whole.
                was_listed.clear();
                now_listed.clear();
                state.listed_sessions(&old, &mut was_listed);
                state.listed_sessions(&tree, &mut now_listed);
                if was_listed != now_listed {
                    whole = true;
                    nav.screen_stale = true;
                }
            }
            let shows = if whole {
                Shows::Nothing
            } else {
                state.shows(&tree, changed.iter().copied(), width, height)
            };
            if whole {
                revise(&mut self.native_runtime);
                self.invalidate_native_view_cache(at.window, at.view, DamageRegion::All);
                self.refresh_native_presentation(at.window, instance, at.view);
            } else {
                match shows {
                    Shows::Painted(span) => {
                        let band = band_of(span, width);
                        self.invalidate_native_view_cache(at.window, at.view, band);
                    }
                    // A ROW THIS VIEW LISTS OFF ITS SCREEN CHANGED: no pixel of it is on the
                    // glass, and its node says the new words. The revision stays (moved with no
                    // frame it would leave the raster on glass refused for the pointer, and the
                    // next frame a whole raster), and the view asks for the least frame there is.
                    Shows::Listed => {
                        self.invalidate_native_view_cache(at.window, at.view, TREE_ONLY);
                    }
                    // Nothing of what changed is in this view's page: it compiles as it did.
                    Shows::Nothing => continue,
                }
            }
            if let Some(window) = self.windows.get(&at.window) {
                window.request_redraw();
            }
        }
        self.nav = Some(nav);
    }

    /// Whether `session`'s row is on a view's screen, where its latest words are read.
    #[cfg(test)]
    pub(crate) fn navigator_reads_words(&self, session: u64) -> bool {
        self.nav
            .as_deref()
            .is_some_and(|nav| nav.screen.contains(&session))
    }

    /// Hand off what a press on the navigator asked for: show the session and raise its window.
    pub(crate) fn drain_navigator_actions(&mut self, wid: WindowId, instance: AppInstanceId) {
        while let Some(NavAction::Go(session)) = self.native_runtime.take_navigator_action(instance)
        {
            if !self.focus_session_preferring(Some(wid), session) {
                aterm_log::info!("navigator: session {session} is gone");
            }
        }
        self.navigator_view_moved();
    }
}

#[cfg(test)]
impl App {
    pub(crate) fn navigator_instance(&self) -> Option<AppInstanceId> {
        self.navigator_instance_id()
    }

    pub(crate) fn navigator_views(&self) -> Vec<(WindowId, ViewId)> {
        self.navigator_instance_id()
            .map_or_else(Vec::new, |instance| {
                let mut views = Vec::new();
                self.navigator_views_now(instance, &mut views);
                views.into_iter().map(|at| (at.window, at.view)).collect()
            })
    }

    pub(crate) fn navigator_tree(&self) -> Option<Arc<NavTree>> {
        self.nav.as_deref().map(|nav| Arc::clone(&nav.tree))
    }

    fn navigator_state(&self, view: ViewId) -> Option<&NavigatorViewState> {
        match self.native_runtime.view_state(view)? {
            AppViewState::Navigator(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) fn navigator_listing(&self, view: ViewId) -> Vec<NavListed> {
        let (Some(tree), Some(state)) = (self.navigator_tree(), self.navigator_state(view)) else {
            return Vec::new();
        };
        state
            .listing(&tree)
            .into_iter()
            .map(|row| {
                let entry = &tree.entries[row.at];
                NavListed {
                    session: entry.row.session,
                    depth: entry.depth,
                    open: row.open,
                    under: row.under,
                    pinned: entry.group == crate::navigator::Group::Pinned,
                }
            })
            .collect()
    }

    pub(crate) fn navigator_selection(&self, view: ViewId) -> Option<u64> {
        self.navigator_state(view)?.cursor()
    }

    pub(crate) fn navigator_work(&self) -> NavWork {
        self.nav
            .as_deref()
            .map_or_else(NavWork::default, |nav| nav.work)
    }

    /// The top and bottom of `session`'s row in `view`, where it is on screen.
    pub(crate) fn navigator_row_span(&self, view: ViewId, session: u64) -> Option<(f32, f32)> {
        let window = self
            .navigator_views()
            .into_iter()
            .find_map(|(window, at)| (at == view).then_some(window))?;
        let viewport = self.native_ui_viewport_for(window, view).ok()?;
        let tree = self.navigator_tree()?;
        self.navigator_state(view)?
            .row_span(&tree, session, viewport.width, viewport.height)
    }
}

#[cfg(test)]
#[path = "app_navigator_tests.rs"]
mod tests;
