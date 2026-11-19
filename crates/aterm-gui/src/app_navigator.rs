// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The navigator's host (tracker ST-04, `docs/DESIGN-streams-2026-11-12.md` §3): where it opens,
//! what it lists, and when it reads.
//!
//! The list's session facts are published on change. A visible resource panel projects the existing readings when its five-second buckets close. The loop thread marks what may
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
//! band at its revision; one it lists off its screen asks for no frame (a reader of its nodes
//! compiles the page as it stands, `App::note_native_nodes_behind`), save where an assistive
//! client reads the window's tree, which is told by a frame that rasters one pixel
//! ([`TREE_ONLY`]); one it does not list asks it for nothing. A view nobody sees moves its
//! revision with each change, so a reader of its page compiles the list as it stands, and is
//! drawn whole, with a frame asked for, when it shows.

#[path = "app_local_panel_roster.rs"]
mod local_panel_roster;
pub(crate) use local_panel_roster::LocalPanelRoster;

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app_native::TREE_ONLY;
use crate::native_app::{AppKind, AppViewState, DamageRegion, NativeApp};
use crate::navigator::{
    NavAction, NavRow, NavTree, NavigatorViewState, SPLIT_COLS, Shows, band_of,
};
use crate::session_store::{SessionHandle, SessionState};
use crate::tab_model::{AppInstanceId, SplitAxis, View, ViewId};
use crate::task_record::TaskState;
use crate::{App, WindowId};

/// A parked tab shown in place of the current tab. An auxiliary session borrows a live view
/// for the preview; a locally hidden session already owns its view here.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionPreview {
    pub(crate) tab: crate::tab_model::TabId,
    pub(crate) anchor: Option<crate::tab_model::TabId>,
    pub(crate) session: u64,
    pub(crate) borrowed: bool,
}

impl crate::WindowState {
    /// Tabs in the top strip, paired with their canonical indices.
    pub(crate) fn strip_tabs(
        &self,
    ) -> impl DoubleEndedIterator<Item = (usize, &crate::tab_model::Tab)> {
        self.tab_set
            .shown()
            .iter()
            .enumerate()
            .filter(|(_, tab)| !self.organizer_tabs.contains(&tab.id))
    }

    pub(crate) fn strip_tab_count(&self) -> usize {
        if self.organizer_tabs.is_empty() {
            self.tab_set.shown_len()
        } else {
            self.strip_tabs().count()
        }
    }

    pub(crate) fn strip_index_of(&self, id: crate::tab_model::TabId) -> Option<usize> {
        self.strip_tabs().position(|(_, tab)| tab.id == id)
    }

    pub(crate) fn strip_index_to_tab_index(&self, slot: usize) -> Option<usize> {
        self.strip_tabs().nth(slot).map(|(index, _)| index)
    }

    pub(crate) fn active_session_preview(&self) -> Option<SessionPreview> {
        self.dock
            .preview
            .filter(|p| self.tab_set.active_id() == Some(p.tab))
    }

    pub(crate) fn preview_on_chip(&self, chip: crate::tab_model::TabId) -> Option<SessionPreview> {
        self.active_session_preview()
            .filter(|p| p.anchor == Some(chip))
    }

    pub(crate) fn preview_chip_label(&self, chip: crate::tab_model::TabId) -> Option<String> {
        let preview = self.preview_on_chip(chip)?;
        let label = self
            .tab_chrome_titles_by_tab
            .get(&preview.tab)
            .map(|(_, title)| title.as_str())
            .or_else(|| {
                self.tab_set
                    .get(preview.tab)
                    .map(|tab| tab.presentation.title.as_str())
            })?;
        Some(format!("Preview · {label}"))
    }

    /// A preview edits in its anchor's title slot; a full session edits in its own.
    pub(crate) fn title_chip(
        &self,
        tab: crate::tab_model::TabId,
    ) -> Option<crate::tab_model::TabId> {
        let chip = self
            .dock
            .preview
            .filter(|preview| preview.tab == tab)
            .and_then(|preview| preview.anchor)
            .or_else(|| self.tab_set.chip_of(tab))?;
        self.strip_index_of(chip).map(|_| chip)
    }

    /// Resolve a pointer against the identity the tab strip actually drew.
    pub(crate) fn painted_tab_index(&self, slot: usize) -> Option<usize> {
        if self.painted_strip_ids.is_empty() {
            return self.strip_index_to_tab_index(slot);
        }
        let id = *self.painted_strip_ids.get(slot)?;
        self.strip_tabs()
            .find_map(|(index, tab)| (tab.id == id).then_some(index))
    }

    pub(crate) fn strip_projection<'a, T: Clone>(
        &self,
        items: &'a [T],
    ) -> std::borrow::Cow<'a, [T]> {
        if self.organizer_tabs.is_empty() {
            std::borrow::Cow::Borrowed(&items[..items.len().min(self.tab_set.shown_len())])
        } else {
            std::borrow::Cow::Owned(
                self.strip_tabs()
                    .filter_map(|(index, _)| items.get(index).cloned())
                    .collect(),
            )
        }
    }

    pub(crate) fn selected_strip_index(&self) -> Option<usize> {
        self.dock
            .preview
            .filter(|preview| self.tab_set.active_id() == Some(preview.tab))
            .and_then(|preview| preview.anchor)
            .and_then(|anchor| self.strip_index_of(anchor))
            .or_else(|| {
                let canonical = self.tab_set.strip_active()?;
                if self.organizer_tabs.is_empty() {
                    Some(canonical)
                } else {
                    self.strip_index_of(self.tab_set.tab_at(canonical)?.id)
                }
            })
    }
}

/// The least time between two reads of the latest words.
const NAV_WORDS_STEP: Duration = Duration::from_secs(1);
/// How often the ages are worded again.
const NAV_AGE_STEP: Duration = Duration::from_secs(60);
/// The page's title where the machine has no name.
const THIS_MACHINE: &str = "This machine";

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

/// What a session says of itself, as its reader's last feed left it
/// ([`crate::vitals_jobs::JobFeed`]): whether its shell runs a command (its OSC 133 C with no D
/// yet), and its program's own word on itself (its OSC 7501 report, as
/// [`crate::program_state::survey_word`] words it: `busy` for a turn in progress, `idle` for one
/// ended). With Tab Status off nothing observes the session, and its row is read from these.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct OwnWord {
    runs: bool,
    word: Option<&'static str>,
    program: Option<aterm_core::terminal::program_state::ProgramApp>,
}

impl OwnWord {
    fn of(feed: &crate::vitals_jobs::JobFeed) -> Self {
        let (state, program) = feed.program();
        Self {
            runs: feed.runs(),
            word: state.map(crate::program_state::survey_word),
            program,
        }
    }
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
    /// This view belongs to the window's numbered local session organizer.
    local_panel: bool,
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
    #[cfg(all(unix, not(target_os = "macos")))]
    title_requested: bool,
    pending_title: Option<String>,
    /// A dock-only list has no uncovered shown host: keep no provider work until it returns.
    suspended: bool,
    /// The layout, the set of sessions, a focus or the machine's name may have moved.
    structure: bool,
    /// Sessions whose title, word, foreground program or creator may have moved.
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
    /// The union of local sessions painted in a session panel. Mirrored views read each producer once.
    panel_screen: BTreeSet<u64>,
    /// Next close of the shared five-second resource bucket, only when a panel has rows.
    resources_at: Option<Instant>,
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
    /// What each session said of itself when its row was last built ([`OwnWord`]), kept only
    /// where it said something.
    said: HashMap<u64, OwnWord>,
    /// Whether Tab Status was on when the rows were last marked for it: whether a row's word is
    /// read from what it observed or from what the session says of itself.
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
        if self.suspended && !self.structure {
            return true;
        }
        !(self.structure
            || self.screen_stale
            || !self.rows.is_empty()
            || self.words_due(now)
            || self.resources_at.is_some_and(|at| now >= at)
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
        self.local_panel_roster.invalidate();
        self.navigator_mark_presentation();
    }

    /// The visible pages or their ordering changed while the process kept the same sessions.
    pub(crate) fn navigator_mark_presentation(&mut self) {
        if let Some(nav) = self.nav.as_deref_mut() {
            nav.structure = true;
        }
    }

    /// `session`'s title, state or creator may have moved.
    pub(crate) fn navigator_mark_row(&mut self, session: u64) {
        if let Some(nav) = self.nav.as_deref_mut()
            && !nav.suspended
        {
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
    /// With Tab Status off nothing observes the session, and what it says of itself (its shell's
    /// word on whether a command runs, its program's own report) arrives as output: it is read
    /// here, off the term lock, and a row built from another answer is marked. The output pass
    /// calls this after it stores the session's output latch (`SessionPool::output_pass`): a word
    /// the reader feeds after that store posts a wake of its own or leaves the hold dirty, and
    /// one fed before it is the word read here.
    pub(crate) fn navigator_note_output(&mut self, session: u64) {
        let Some(nav) = self.nav.as_deref_mut().filter(|nav| !nav.suspended) else {
            return;
        };
        if nav.screen.contains(&session) {
            nav.words.insert(session);
        }
        if !self.config.tab_status_or_default()
            && nav.held.contains_key(&session)
            && self.pool.get(session).is_some_and(|live| {
                OwnWord::of(&live.ctx.job_feed)
                    != nav.said.get(&session).copied().unwrap_or_default()
            })
        {
            nav.rows.insert(session);
        }
    }

    /// The configuration was applied. A row's word is read from what Tab Status observed while
    /// that is on and from what the session says of itself while it is off ([`OwnWord`]), so
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
        self.nav.is_some()
            && self
                .navigator_instance_id()
                .is_some_and(|instance| self.is_navigator_view(view, instance))
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
        [self.navigator_words_wake(), nav.age_at, nav.resources_at]
            .into_iter()
            .flatten()
            .min()
    }

    /// The cached machine name, else the process host name, else `This machine`.
    pub(crate) fn machine_title(&self) -> String {
        if let Some(name) = self.machine_name_now() {
            return name;
        }
        if let Some(name) = self
            .nav
            .as_deref()
            .map(|nav| nav.pending_title.as_deref().unwrap_or(&nav.machine))
            .filter(|name| !name.is_empty())
        {
            return name.to_owned();
        }
        aterm_uds::process::lan_host_name()
            .map(|host| host.trim_end_matches(".local").trim().to_owned())
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| THIS_MACHINE.to_owned())
    }

    /// The non-macOS Unix name file is read once, on a worker; the page reads only its snapshot.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn request_navigator_title(&self, nav: &mut NavWatch) {
        if nav.title_requested || !nav.views.iter().any(|view| view.visible) {
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let fallback = nav.machine.clone();
        match std::thread::Builder::new()
            .name("navigator-title".into())
            .spawn(move || {
                let title = aterm_types::dirs::state_dir()
                    .and_then(|state| aterm_agent::machine_name::file::read(&state).ok().flatten())
                    .map(|name| name.name())
                    .filter(|name| !name.is_empty() && name != "unnumbered")
                    .unwrap_or(fallback);
                let _ = proxy.send_event(crate::Wake::NavigatorTitleReady(title));
            }) {
            Ok(_) => nav.title_requested = true,
            Err(error) => aterm_log::info!("navigator: title reader could not start: {error}"),
        }
    }

    #[cfg(any(test, all(unix, not(target_os = "macos"))))]
    pub(crate) fn navigator_title_ready(&mut self, title: String) {
        if let Some(nav) = self.nav.as_mut()
            && nav.machine != title
            && nav.pending_title.as_ref() != Some(&title)
        {
            nav.pending_title = Some(title);
            nav.structure = true;
        }
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

    /// Whether `view` is a page the restore manifest and Reopen Closed Tab leave out
    /// ([`crate::native_app::AppKind::left_out_of_restore`]): the navigator, whose list is the
    /// machine's sessions, and System Resources, whose head is the machine's reading; neither is
    /// a document of its own.
    pub(crate) fn view_is_unrestored_page(&self, view: ViewId) -> bool {
        matches!(self.view_store.get(view), Some(View::Native(native))
            if self.native_runtime.app(native.instance)
                .is_some_and(|app| app.kind().left_out_of_restore()))
    }

    fn ensure_navigator_instance(&mut self) -> Result<AppInstanceId, String> {
        match self.navigator_instance_id() {
            Some(instance) => Ok(instance),
            None => self
                .native_runtime
                .insert_instance(NativeApp::Navigator(Box::default()))
                .map_err(|error| format!("The navigator could not open: {error:?}")),
        }
    }

    /// Allocate one window-owned view of the same page; it takes neither a tab nor its keys.
    pub(crate) fn create_dock_navigator(&mut self, wid: WindowId) -> Result<ViewId, String> {
        if !self.windows.contains_key(&wid) {
            return Err("No window for the navigator".into());
        }
        self.land_native_layout_faces();
        let person_in = self
            .focused_session_id(wid)
            .or_else(|| self.shown_front(wid).map(|front| front.session));
        let instance = self.ensure_navigator_instance()?;
        let view = self
            .view_store
            .insert_native(instance)
            .map_err(|error| format!("The navigator could not open: {error:?}"))?;
        if let Err(error) = self.native_runtime.attach_view(
            view,
            instance,
            AppViewState::Navigator(Box::new(NavigatorViewState::in_dock(person_in))),
        ) {
            self.view_store.remove(view);
            return Err(format!("The navigator could not open: {error:?}"));
        }
        let observed = self.config.tab_status_or_default();
        self.nav
            .get_or_insert_with(|| {
                Box::new(NavWatch {
                    observed,
                    ..NavWatch::default()
                })
            })
            .structure = true;
        Ok(view)
    }

    /// Open the navigator in the front window: as the left pane of the active tab, about
    /// [`SPLIT_COLS`] cells wide, where `split` and the tab is terminals with room for it; else as
    /// a tab of its own. A tab that already holds it gets the keys on it. Whether it is a split.
    pub(crate) fn open_navigator(&mut self, split: bool) -> Result<bool, String> {
        let wid = self.frontmost_window.ok_or("No window for the navigator")?;
        self.open_navigator_in(wid, split)
    }

    pub(crate) fn open_navigator_in(&mut self, wid: WindowId, split: bool) -> Result<bool, String> {
        let person_in = self.focused_session_id(wid).or_else(|| {
            self.windows
                .get(&wid)?
                .active_terminal
                .as_ref()
                .map(|terminal| terminal.session)
        });
        let instance = self.ensure_navigator_instance()?;
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
            if let Some(view) = ws.dock.view
                && self.is_navigator_view(view, instance)
            {
                let viewport = (!ws.occluded)
                    .then(|| self.native_ui_viewport_for(window, view).ok())
                    .flatten();
                views.push(NavView {
                    window,
                    view,
                    visible: viewport.is_some(),
                    keys: false,
                    at_hand: false,
                    local_panel: ws.dock.roster_panel,
                    size: viewport
                        .map_or((0, 0), |rect| (rect.width.to_bits(), rect.height.to_bits())),
                });
            }
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
                            local_panel: false,
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
            .filter(|handle| handle.local_id == session && handle.state != SessionState::Exited)
            .cloned()
    }

    fn navigator_state(&self, view: ViewId) -> Option<&NavigatorViewState> {
        match self.native_runtime.view_state(view)? {
            AppViewState::Navigator(state) => Some(state),
            _ => None,
        }
    }

    /// One session's row as it reads now, with `line` and `moved_ms` as its last read left them;
    /// `said` is what the session says of itself. The store's lock is not held: the row reads it
    /// for its creator.
    fn navigator_row(&self, handle: &SessionHandle, line: Option<String>, said: OwnWord) -> NavRow {
        let observed = self.config.tab_status_or_default();
        let (task, original_topic, taken_off, from, gone, need, agent_word, program) = {
            let timeline = handle
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let task = timeline.task();
            let front = timeline.agent();
            let program = if observed {
                crate::harness_host::identified_agent(front)
                    .map(|agent| agent.name())
                    .or(front.program.as_deref())
                    .map(str::to_owned)
            } else {
                said.program.map(|app| {
                    let name = app.as_str();
                    aterm_phase::program_of(name)
                        .map_or(name, |agent| agent.name())
                        .to_owned()
                })
            };
            (
                task.map(|task| task.state),
                task.and_then(|task| task.line.clone()),
                task.is_some_and(|task| {
                    matches!(
                        task.why,
                        Some(crate::task_record::WHY_OFF | crate::task_record::WHY_OFF_CTL)
                    )
                }),
                task.map(|task| task.from.clone())
                    .filter(|from| from != crate::task_record::FROM_CTL),
                task.filter(|task| crate::native_workers::agent_gone(task))
                    .and_then(crate::app_take_over::worker_label),
                timeline.need().need.as_ref().map(|need| need.word),
                timeline.shown_agent_word().0,
                program,
            )
        };
        let session = handle.local_id;
        // A worker whose agent has gone is titled by its shell's folder: its row names what it
        // ran, unless a role or a pinned title names the session (as its Workers row does).
        let name = self.named_label_of_session(session);
        let named = name.is_some();
        let title = name
            .or(gone)
            .unwrap_or_else(|| self.unnamed_tab_label_of_session(session));
        let creator = from.and_then(|from| {
            aterm_session::SessionId::parse(&from)
                .and_then(|from| self.local_session_of(&from))
                .filter(|creator| *creator != session)
        });
        // Tab Status reads an agent by its screen, and a shell that says nothing of itself too
        // (by the program that holds its terminal), so its readings stand while it is on. Off,
        // nothing observes the session, and what it says of itself is the fact: its program's
        // own report, and its shell's word on whether a command runs. The shell's word alone
        // would read an agent `working` for as long as its process lives.
        let (agent_word, running) = if observed {
            (
                agent_word,
                self.session_status
                    .status(session)
                    .is_some_and(|status| crate::session_status::status_indicators(status).busy),
            )
        } else {
            (said.word.unwrap_or("-"), said.runs)
        };
        NavRow {
            session,
            shortcut: self.local_panel_roster.number(session),
            title,
            original_topic,
            named,
            program,
            word: crate::navigator::word_of(&crate::navigator::WordFacts {
                task,
                taken_off,
                need,
                agent_word,
                running,
            }),
            line,
            moved_ms: handle.ctx.screen_moves.moved_ms(),
            creator,
        }
    }

    /// Read each visible producer once, even when several windows display the same session.
    fn local_panel_readings(
        &self,
        keys: &BTreeSet<u64>,
        now: Instant,
        at_ns: u64,
    ) -> crate::navigator::resources::Panel {
        let snapshot = self.vitals.snapshot.as_deref();
        keys.iter()
            .filter_map(|&key| {
                let session = self.navigator_handle(key)?;
                // The reader may have sampled a previous launch of this session. Its sweep
                // carries that launch's identity and its own kernel process-generation clock.
                let resources = snapshot
                    .and_then(|snapshot| snapshot.pane_resources.get(&key))
                    .filter(|history| {
                        history.source.as_ref().is_some_and(|source| {
                            source.local == key
                                && source.sid == session.sid.as_str()
                                && source.nonce == session.nonce.to_hex()
                        }) && history.root.is_some_and(|root| {
                            root.pid > 0
                                && Some(root.pid) == u32::try_from(session.shell_identity.pid).ok()
                        })
                    });
                let cpu = snapshot
                    .and_then(|snapshot| snapshot.pane_cpu.get(&key))
                    .filter(|cpu| {
                        resources.is_some_and(|r| cpu.root == r.root && cpu.source == r.source)
                    });
                let tokens = snapshot
                    .and_then(|snapshot| snapshot.pane_tokens.get(&key))
                    .filter(|history| {
                        session
                            .ctx
                            .timeline
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .panel_token_owner(session.nonce)
                            .as_ref()
                            == Some(&history.owner)
                    });
                Some((
                    key,
                    crate::navigator::resources::Readings::of(
                        cpu,
                        resources,
                        tokens,
                        at_ns,
                        session.ctx.sink.input_byte_rates(now),
                        session.ctx.job_feed.output_byte_rates(now),
                    ),
                ))
            })
            .collect()
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
        self.reconcile_local_panel_layout(now);
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
            if nav.views.iter().all(|view| {
                self.windows.get(&view.window).is_none_or(|ws| {
                    ws.dock.view == Some(view.view) && (ws.occluded || ws.dock.cells == 0)
                })
            }) {
                nav.suspended = true;
                nav.rows.clear();
                nav.words.clear();
                nav.trailing.clear();
                nav.screen.clear();
                nav.panel_screen.clear();
                nav.resources_at = None;
                nav.age_at = None;
                nav.words_read_at = None;
                nav.screen_stale = true;
                nav.walk = walk;
                self.nav = Some(nav);
                return;
            }
            let resumed = std::mem::replace(&mut nav.suspended, false);
            self.navigator_walk(&mut walk);
            if resumed {
                nav.rows
                    .extend(walk.pinned.iter().chain(&walk.parked).copied());
                rearrange = true;
            }
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
        #[cfg(all(unix, not(target_os = "macos")))]
        self.request_navigator_title(&mut nav);
        if let Some(title) = nav.pending_title.take()
            && nav.machine != title
        {
            nav.machine = title;
            rearrange = true;
            facts = true;
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
            let said = OwnWord::of(&handle.ctx.job_feed);
            if said == OwnWord::default() {
                nav.said.remove(&session);
            } else {
                nav.said.insert(session, said);
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
                .retain(|session, _| arranged.index_of(*session).is_some());
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
            let organizer = if nav
                .views
                .iter()
                .any(|at| self.dock_view(at.window) == Some(at.view))
            {
                self.organizer_windows()
            } else {
                Vec::new()
            };
            for at in &nav.views {
                let person_in = (!at.keys)
                    .then(|| self.focused_session_id(at.window))
                    .flatten();
                let dock = self
                    .windows
                    .get(&at.window)
                    .filter(|ws| ws.dock.view == Some(at.view))
                    .map(|ws| {
                        let mut sessions = crate::navigator::DockSessions {
                            numbered: ws.dock.roster_panel,
                            windows: organizer.clone(),
                            ..Default::default()
                        };
                        let mut seen = std::collections::HashSet::new();
                        for (_, tab) in ws.strip_tabs() {
                            tab.root.visit(&mut |view| {
                                if let Some(s) = self
                                    .view_store
                                    .get(*view)
                                    .copied()
                                    .and_then(View::terminal_session)
                                    && tree.index_of(s).is_some()
                                    && seen.insert(s)
                                {
                                    sessions.order.push(s);
                                }
                            });
                        }
                        sessions.shown = sessions.order.len();
                        sessions.order.extend(
                            nav.parked
                                .iter()
                                .chain(&nav.pinned)
                                .copied()
                                .filter(|s| tree.index_of(*s).is_some() && seen.insert(*s)),
                        );
                        if !ws.dock.hidden_order.is_empty() {
                            let sorted = crate::navigator::steady(
                                &ws.dock.hidden_order,
                                &sessions.order[sessions.shown..],
                            );
                            sessions.order.truncate(sessions.shown);
                            sessions.order.extend(sorted);
                        }
                        sessions.preview = ws
                            .dock
                            .preview
                            .filter(|preview| ws.tab_set.active_id() == Some(preview.tab))
                            .map(|preview| preview.session);
                        sessions
                    });
                let moved = self.native_runtime.change_view_state(at.view, |state| {
                    let AppViewState::Navigator(state) = state else {
                        return false;
                    };
                    let reordered = dock.is_some_and(|dock| {
                        if state.sessions == dock {
                            return false;
                        }
                        state.sessions = dock;
                        true
                    });
                    state.follow(at.keys, at.at_hand, person_in, &tree) || reordered
                });
                if moved {
                    moved_views.push(at.view);
                    nav.screen_stale = true;
                }
            }
        }

        // A rewrapped identity shifts every card below it, including which sessions supply
        // the visible telemetry. One changed card's old raster band cannot cover that shift.
        if !reshaped && !changed.is_empty() {
            for at in nav.views.iter().filter(|at| at.local_panel) {
                let Some((width, _)) = at.viewport() else {
                    continue;
                };
                let Some(state) = self.navigator_state(at.view) else {
                    continue;
                };
                if changed.iter().any(|&session| {
                    let Some(i) = old.index_of(session) else {
                        return false;
                    };
                    let Some(after) = nav.held.get(&session) else {
                        return false;
                    };
                    state.panel_row_resized(&old.entries[i].row, after, width)
                }) {
                    nav.screen_stale = true;
                    if !moved_views.contains(&at.view) {
                        moved_views.push(at.view);
                    }
                }
            }
        }

        // Which rows are on a screen, and the words of those that came onto one. A view is
        // told its size here, before its first frame: a list that opens on the session the
        // person is in opens with that row in its room.
        let mut arrived: Vec<u64> = Vec::new();
        if std::mem::take(&mut nav.screen_stale) || reshaped {
            let mut on_screen = Vec::new();
            let mut panel_screen = BTreeSet::new();
            for at in &nav.views {
                let Some((width, height)) = at.viewport() else {
                    continue;
                };
                let from = on_screen.len();
                let moved = self.native_runtime.change_view_state(at.view, |state| {
                    let AppViewState::Navigator(state) = state else {
                        return false;
                    };
                    let moved = state.lay(&tree, width, height);
                    state.on_screen(&tree, width, height, &mut on_screen);
                    moved
                });
                if at.local_panel {
                    panel_screen.extend(&on_screen[from..]);
                }
                if moved && !moved_views.contains(&at.view) {
                    moved_views.push(at.view);
                }
            }
            if nav.panel_screen != panel_screen {
                nav.panel_screen = panel_screen;
                nav.resources_at = (!nav.panel_screen.is_empty()).then_some(now);
            }
            let screen: BTreeSet<u64> = on_screen.into_iter().collect();
            arrived.extend(screen.difference(&nav.screen));
            nav.words.retain(|session| screen.contains(session));
            nav.trailing.retain(|session| screen.contains(session));
            nav.screen = screen;
        }
        let mut resources_changed = false;
        if nav.resources_at.is_some_and(|at| now >= at) {
            let epoch = aterm_session::terminal_stream::epoch();
            let age = now.saturating_duration_since(epoch);
            let at_ns = u64::try_from(age.as_nanos()).unwrap_or(u64::MAX);
            let readings = self.local_panel_readings(&nav.panel_screen, now, at_ns);
            resources_changed = self
                .native_runtime
                .publish_panel_resources(instance, readings, at_ns);
            let bucket_ns = aterm_vitals::pane_cpu::BUCKET_NS;
            nav.resources_at = Some(now + Duration::from_nanos(bucket_ns - at_ns % bucket_ns));
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
            let moved =
                reshaped || moved_views.contains(&at.view) || (at.local_panel && resources_changed);
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
            let Some(state) = self.navigator_state(at.view) else {
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
                    // A ROW THIS VIEW LISTS OFF ITS SCREEN CHANGED: no pixel or target of it is
                    // on the glass, and only its node says the new words. The revision stays
                    // (moved with no frame it would leave the raster on glass refused for the
                    // pointer, and the next frame a whole raster). Where an assistive client
                    // reads the window's tree it is told by the least frame there is; elsewhere
                    // the view asks for no frame, and a reader of its nodes (`inspect`) compiles
                    // the page as it stands. Scrolled onto the screen, the row is drawn from the
                    // list as published.
                    Shows::Listed => {
                        if self
                            .windows
                            .get(&at.window)
                            .is_some_and(|window| window.assistive_client())
                        {
                            self.invalidate_native_view_cache(at.window, at.view, TREE_ONLY);
                        } else {
                            self.note_native_nodes_behind(at.window, at.view);
                            continue;
                        }
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

    fn organizer_windows(&self) -> Vec<crate::navigator::OrganizerWindow> {
        let mut rows = Vec::new();
        let home = self.tab_home_window();
        for (&wid, window) in &self.windows {
            let current = window.tab_set.active_id();
            for tab in window.tab_set.tabs() {
                let selected = self.frontmost_window == Some(wid) && current == Some(tab.id);
                let mut has_native_view = false;
                tab.root.visit(&mut |view| {
                    let Some(View::Native(native)) = self.view_store.get(*view) else {
                        return;
                    };
                    let Some(app) = self.native_runtime.app(native.instance) else {
                        return;
                    };
                    has_native_view = true;
                    let label = if app.kind().is_organizer_window()
                        || self.view_is_private(*view)
                        || tab.presentation.title.is_empty()
                    {
                        app.kind().display_name()
                    } else {
                        &tab.presentation.title
                    };
                    rows.push(crate::navigator::OrganizerWindow {
                        view: *view,
                        label: if Some(wid) == home {
                            label.to_owned()
                        } else {
                            format!("{label} · Window {}", wid.0 + 1)
                        },
                        selected: selected && tab.focus == *view,
                    });
                });
                if !has_native_view && Some(wid) != home && current == Some(tab.id) {
                    let label = self
                        .focused_session_id(wid)
                        .map(|s| self.tab_label_of_session(s))
                        .unwrap_or_else(|| tab.presentation.title.clone());
                    rows.push(crate::navigator::OrganizerWindow {
                        view: tab.focus,
                        label: format!("{label} · Window {}", wid.0 + 1),
                        selected,
                    });
                }
            }
        }
        rows
    }

    fn focus_organizer_window(&mut self, view: ViewId) {
        let target = self.windows.iter().find_map(|(&wid, ws)| {
            ws.tab_set
                .tabs()
                .iter()
                .position(|tab| tab.root.contains(view))
                .map(|index| (wid, index))
        });
        if let Some((wid, index)) = target {
            self.frontmost_window = Some(wid);
            self.switch_tab_in(wid, index);
            if let Some(session) = self
                .view_store
                .get(view)
                .copied()
                .and_then(View::terminal_session)
            {
                self.focus_session_pane(wid, session, view);
            } else if let Some(mut ws) = self.windows.get_mut(&wid) {
                if let Some(active) = ws.tab_set.active_mut() {
                    active.set_focus(view);
                }
                crate::app_tabs::cancel_pending_close(&mut ws);
            }
            self.sync_active_session();
            self.raise_window(wid);
            self.request_window_redraw(wid);
        }
    }

    /// The organizer previews sessions in place; the standalone navigator raises their window.
    pub(crate) fn drain_navigator_actions(&mut self, wid: WindowId, instance: AppInstanceId) {
        while let Some(action) = self.native_runtime.take_navigator_action(instance) {
            match action {
                NavAction::Window(view) => self.focus_organizer_window(view),
                NavAction::Go(session) => {
                    if self.navigator_handle(session).is_none()
                        || !self.focus_session_preferring(Some(wid), session)
                    {
                        aterm_log::info!("navigator: session {session} is gone");
                    }
                }
                NavAction::Preview(session) => {
                    self.activate_local_panel_session(wid, session);
                }
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
