// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window-side state and actions for live agent upgrades.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use aterm_agent::harness::upgrade::{Phase, Request};
use aterm_agent::harness::upgrade_drive::{self, Ask, Row};
use aterm_messages::{Intent, MessageId, Outcome, UpgradeWord};

use crate::App;
use crate::message_reporters;
use crate::toolchain_words::TabsBehind;

/// Current Unix timestamp in seconds.
pub(crate) fn now_s() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// How long `Not today` holds an upgrade (`--defer`): a day.
pub(crate) const NOT_TODAY_S: u64 = 24 * 3_600;

/// Cached foreground custody captured before the background view reads upgrade records. These
/// stamps travel with the publication, so its delivery cannot adopt the program then in front.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UpgradeSources(Vec<(String, crate::session_timeline::UpgradeStamp)>);

impl UpgradeSources {
    pub(crate) fn capture(store: &crate::session_store::Store) -> Self {
        let handles = store.read().unwrap_or_else(|p| p.into_inner()).snapshot();
        let mut sources: Vec<_> = handles
            .iter()
            .map(|handle| {
                let timeline = handle
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                (handle.sid.as_str().to_owned(), timeline.upgrade_stamp())
            })
            .collect();
        sources.sort_by(|a, b| a.0.cmp(&b.0));
        Self(sources)
    }

    fn current(
        &self,
        tab: &str,
        timeline: &crate::session_timeline::SessionTimeline,
    ) -> Option<crate::session_timeline::UpgradeStamp> {
        self.0
            .iter()
            .find(|(sid, _)| sid == tab)
            .map(|(_, stamp)| stamp.clone())
            .filter(|stamp| stamp.is_current(timeline))
    }
}

/// An upgrade action pressed by the owner on a tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WordAsk {
    /// The tab the agent runs in (`s-<hex>`).
    pub(crate) tab: String,
    /// The target build the owner was shown.
    pub(crate) to: String,
    /// Which word.
    pub(crate) word: UpgradeWord,
    /// The conversation of the tab's row for `to` at the press.
    pub(crate) session: String,
    /// The owner pressed the top rule's ⬆️ ([`Self::pressed`]); `None` for a word of the tab
    /// menu, a capsule or Settings.
    pub(crate) press: Option<PressAsk>,
    /// The source identity of that displayed row, checked again when the word is written.
    pub(crate) scope: Option<upgrade_drive::AskScope>,
}

/// The owner's press of a Claude Code tab's ⬆️ ([`WordAsk::pressed`]): a one-shot Now its own
/// lane moves ([`App::start_press_lane`], [`upgrade_drive::press`]), past an Autopilot or an
/// agent switched off and the agent's own background work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PressAsk {
    /// The tab's harness or its Autopilot is off: the restart types only its relaunch line
    /// ([`upgrade_drive::PressOpts::off`]).
    pub(crate) off: bool,
}

impl WordAsk {
    /// Create an upgrade action for `tab` targeting build `to`.
    pub(crate) const fn new(tab: String, to: String, word: UpgradeWord) -> Self {
        Self {
            tab,
            to,
            word,
            session: String::new(),
            press: None,
            scope: None,
        }
    }

    /// The owner's press of `tab`'s ⬆️ for build `to`, its harness or Autopilot `off`.
    pub(crate) const fn pressed(tab: String, to: String, off: bool) -> Self {
        Self {
            tab,
            to,
            word: UpgradeWord::Now,
            session: String::new(),
            press: Some(PressAsk { off }),
            scope: None,
        }
    }

    /// Convert this action into the harness [`Ask`].
    pub(crate) const fn ask(&self) -> Ask {
        if self.press.is_some() {
            return Ask::Press;
        }
        match self.word {
            UpgradeWord::Now => Ask::Now,
            UpgradeWord::NotToday => Ask::Defer(NOT_TODAY_S),
            UpgradeWord::Skip => Ask::Skip,
        }
    }
}

/// Writer that records the owner's upgrade action in harness state.
#[derive(Clone)]
pub(crate) struct WordWriter(Arc<WriteFn>);

type WriteFn = dyn Fn(&WordAsk) -> Result<Row, String> + Send + Sync;

impl WordWriter {
    /// Wrap a writer function.
    pub(crate) fn new(f: impl Fn(&WordAsk) -> Result<Row, String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// Execute `ask` and return the updated upgrade [`Row`].
    pub(crate) fn write(&self, ask: &WordAsk) -> Result<Row, String> {
        (self.0)(ask)
    }
}

impl Default for WordWriter {
    fn default() -> Self {
        Self::new(|_| Err("no harness state in this window".to_string()))
    }
}

impl std::fmt::Debug for WordWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WordWriter")
    }
}

/// Background queue that executes [`WordAsk`] actions off the winit thread in press order.
#[derive(Debug, Default)]
pub(crate) struct WordQueue {
    /// The writing thread's inbox, once it is started.
    tx: Option<std::sync::mpsc::Sender<WordAsk>>,
}

impl WordQueue {
    /// Queue `ask` for execution through `writer` and wake the window with the result.
    pub(crate) fn send(
        &mut self,
        ask: WordAsk,
        writer: &WordWriter,
        deliver: impl Fn(WordAsk, Result<Row, String>) + Send + 'static,
    ) -> Result<(), String> {
        let ask = match &self.tx {
            Some(tx) => match tx.send(ask) {
                Ok(()) => return Ok(()),
                Err(std::sync::mpsc::SendError(ask)) => ask,
            },
            None => ask,
        };
        let (tx, rx) = std::sync::mpsc::channel::<WordAsk>();
        let _ = tx.send(ask);
        let writer = writer.clone();
        crate::qos::spawn_lane_thread(
            "aterm-upgrade-word".to_string(),
            crate::qos::Role::Responsive,
            move || {
                for ask in rx {
                    let result = writer.write(&ask);
                    deliver(ask, result);
                }
            },
        )
        .map_err(|e| format!("the word could not be sent: {e}"))?;
        self.tx = Some(tx);
        Ok(())
    }
}

/// Production [`WordWriter`] over the user's home and harness state.
pub(crate) fn live_word_writer(sock: Option<String>) -> WordWriter {
    match aterm_primer::home_dir().zip(crate::harness_host::harness_state()) {
        Some((home, state)) => word_writer_at(home, state, sock),
        None => WordWriter::default(),
    }
}

/// [`WordWriter`] over `home`, `state`, and control socket `sock`.
pub(crate) fn word_writer_at(home: PathBuf, state: PathBuf, sock: Option<String>) -> WordWriter {
    WordWriter::new(move |ask: &WordAsk| {
        // The word only records the request and wakes the host's lane
        // (`upgrade_drive::ask_for_scope`), dialling no socket; the relaunch line that
        // lane types arms the tap itself (`harness_host::window_drive_opts`).
        let opts = upgrade_drive::Opts {
            only_sid: Some(ask.tab.clone()),
            aterm_state: None,
            ..crate::harness_host::window_drive_opts(home.clone(), state.clone(), sock.clone())
        };
        match &ask.scope {
            Some(scope) => upgrade_drive::ask_for_scope(
                &opts,
                &ask.tab,
                ask.ask(),
                &ask.to,
                &ask.session,
                scope,
            ),
            None => upgrade_drive::ask_for(&opts, &ask.tab, ask.ask(), &ask.to, &ask.session),
        }
    })
}

/// One step of a press's lane for its tab ([`App::start_press_lane`]): the word of what
/// [`upgrade_drive::press`] did, and the tab's upgrade row after it where its record reads.
type PressFn = dyn Fn(&WordAsk) -> (String, Option<Row>) + Send + Sync;

/// What steps a press's lane ([`App::start_press_lane`]).
#[derive(Clone)]
pub(crate) struct Presser(Arc<PressFn>);

impl Presser {
    /// Wrap a step function.
    pub(crate) fn new(
        f: impl Fn(&WordAsk) -> (String, Option<Row>) + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(f))
    }

    fn step(&self, ask: &WordAsk) -> (String, Option<Row>) {
        (self.0)(ask)
    }
}

impl Default for Presser {
    fn default() -> Self {
        Self::new(|_| (upgrade_drive::PRESS_LAPSED.to_string(), None))
    }
}

impl std::fmt::Debug for Presser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Presser")
    }
}

/// Production [`Presser`] over the user's home and harness state.
pub(crate) fn live_presser(sock: Option<String>) -> Presser {
    match aterm_primer::home_dir().zip(crate::harness_host::harness_state()) {
        Some((home, state)) => presser_at(home, state, sock),
        None => Presser::default(),
    }
}

/// [`Presser`] over `home`, `state`, and control socket `sock`: the acting lane's opts for the
/// pressed tab alone, its relaunch line arming the usage tap as the host's own step does.
pub(crate) fn presser_at(home: PathBuf, state: PathBuf, sock: Option<String>) -> Presser {
    Presser::new(move |ask: &WordAsk| {
        let opts = upgrade_drive::Opts {
            only_sid: Some(ask.tab.clone()),
            ..crate::harness_host::window_drive_opts_with_tap(
                home.clone(),
                state.clone(),
                sock.clone(),
                crate::harness_host::usage_tap_on(),
            )
        };
        let r = upgrade_drive::press(&opts, ask.press.is_some_and(|p| p.off));
        if r.is_act() {
            aterm_log::info!("harness @{}: the press's lane: {}", ask.tab, r.line());
        }
        let row = upgrade_drive::rows(&opts)
            .into_iter()
            .find(|row| row.tab == ask.tab && row.session == r.session);
        (r.step, row)
    })
}

/// How long a press's lane steps its tab at most: the press stands [`upgrade::PRESS_S`], and a
/// restart it began is seen through as long again.
const PRESS_LANE_MAX: std::time::Duration =
    std::time::Duration::from_secs(2 * aterm_agent::harness::upgrade::PRESS_S);

/// The pause before a press's lane's next step, `elapsed` since it began: close at first, where
/// the owner who pressed is watching (a draft sent, a box answered, a turn ended), then calmer.
fn press_pace(elapsed: std::time::Duration) -> std::time::Duration {
    if elapsed < std::time::Duration::from_secs(60) {
        std::time::Duration::from_secs(2)
    } else {
        std::time::Duration::from_secs(10)
    }
}

/// Whether a press's lane ends after a step that said `step`, `elapsed` since it began: its
/// press stands no longer and no move it began is in flight ([`upgrade_drive::PRESS_LAPSED`]),
/// the tab has no record to move or is on its build, the record cannot be written, or the
/// lane's span is spent ([`PRESS_LANE_MAX`]).
fn press_lane_ends(step: &str, elapsed: std::time::Duration) -> bool {
    step == upgrade_drive::PRESS_LAPSED
        || step == "current"
        || step.starts_with("skip:")
        || upgrade_drive::after(step, 0) == upgrade_drive::After::Broken
        || elapsed >= PRESS_LANE_MAX
}

/// Steps `ask`'s press through `presser` until its lane ends ([`press_lane_ends`]), handing
/// each row it reads to `deliver` and pausing `pace` between steps; `lanes` holds its tab while
/// it runs.
fn run_press_lane(
    ask: &WordAsk,
    presser: &Presser,
    lanes: &std::sync::Mutex<BTreeSet<String>>,
    pace: impl Fn(std::time::Duration) -> std::time::Duration,
    deliver: impl Fn(Row),
) {
    let began = std::time::Instant::now();
    loop {
        let (step, row) = presser.step(ask);
        if let Some(row) = row {
            deliver(row);
        }
        if press_lane_ends(&step, began.elapsed()) {
            break;
        }
        std::thread::sleep(pace(began.elapsed()));
    }
    lanes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&ask.tab);
}

pub(crate) fn same_upgrade_run(a: &Row, b: &Row) -> bool {
    a.tab == b.tab
        && a.session == b.session
        && a.agent == b.agent
        && a.from == b.from
        && a.proc_start == b.proc_start
}

pub(crate) fn same_move(a: &Row, b: &Row) -> bool {
    same_upgrade_run(a, b) && a.source == b.source && a.to == b.to
}

fn laid_in(shown: &[Row], row: Row, now: u64) -> Option<Vec<Row>> {
    let mut rows = shown.to_vec();
    let at = rows.iter().position(|r| same_move(r, &row))?;
    let mut row = row;
    row.keep_aside(&rows[at], now);
    rows[at] = row;
    for r in &mut rows {
        r.reread(now);
    }
    Some(rows)
}

/// One stalled tab's entry: a band row, or a record.
#[derive(Debug)]
struct StallRow {
    /// The row.
    id: MessageId,
    /// What it was posted for (`<stall>/<to>`).
    said: String,
    /// The target build it names — the one its capsules' words are for.
    to: String,
    /// The agent it moves: what the stall's end record names
    /// ([`message_reporters::agent_upgrade_stall_over`]).
    agent: aterm_agent::harness::upgrade::Agent,
    /// What its words say now: `said`, or a restart in flight that has not
    /// moved for `STALE_S` (`stuck:<what>`), whose words are laid over the
    /// entry in place ([`App::post_upgrade_stalls`]).
    shows: String,
    /// The words a stuck restart's replaced, told again once it moves.
    beneath: Option<aterm_messages::Restatement>,
    /// Whether its words name a person at the tab (`Row::person_seen`): a
    /// flip is restated in place, the stall and its buttons unchanged.
    person: bool,
    /// How long its tab has been behind, in the words it last said (`Row::age_said`, e.g.
    /// `6 h`; only an `overdue` row's words, and an `unwatched` row's since its last check,
    /// carry one): a standing row restates a grown age in place, never as a new row.
    behind: Option<String>,
    /// Where its words last said its tab is ([`message_reporters::UpgradeSpot`]'s
    /// `place`: `in tab 2`, `in window 2, tab 1`): when the tab moves, or a window opens or
    /// closes beside it, the standing row is restated in place.
    place: String,
}

fn stuck(said: &str) -> bool {
    said.starts_with("stuck:")
}

type WaitingSaid = (String, Vec<String>, Vec<Intent>);

/// Which way a stall's entry changed shape ([`reshaped`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reshape {
    /// A row whose stall no longer needs the glass: a record from here.
    ToRecord,
    /// A record whose stall now needs the glass (an interrupt): a row of its own.
    ToRow,
}

fn reshaped(posted: &str, fresh: &str) -> Option<Reshape> {
    let flip = |from: &str, to: &str, way: Reshape| {
        let (was, is) = (posted.strip_suffix(from)?, fresh.strip_suffix(to)?);
        (was == is).then_some(way)
    };
    flip("/row", "/record", Reshape::ToRecord).or_else(|| flip("/record", "/row", Reshape::ToRow))
}

fn aged(line: &str, age: &str) -> Option<String> {
    let rest = line.strip_prefix("behind for ")?;
    Some(match rest.split_once(':') {
        Some((_, tail)) => format!("behind for {age}:{tail}"),
        None => format!("behind for {age}"),
    })
}

const WORD_LAG: std::time::Duration = std::time::Duration::from_secs(60);

type Worded = (Row, std::time::Instant, Option<Phase>);

/// Window-side cache of live agent upgrade rows and menus.
#[derive(Debug, Default)]
pub(crate) struct UpgradeView {
    /// The rows the host last handed over (this instance's live tabs): every
    /// conversation's, as handed over — what a pressed word's answer is laid
    /// over ([`laid_in`]) and the window applies again.
    rows: Vec<Row>,
    /// Each tab's one row of [`Self::rows`] (`upgrade_drive::tab_rows`
    /// LC-9): what the tab says, its menu, the words its records offer, the counts
    /// ([`Self::row_for`], [`Self::offers`], [`App::tabs_behind`]).
    shown: Vec<Row>,
    /// The waiting records written for what waits now (title, detail, words), and whether a
    /// floor held it rather than the ladder's comfort ([`Row::waits_for_comfort`]), so a
    /// wait is one record per sentence, not one per change of some other row or per flip of
    /// what holds it between looks.
    waiting_said: Vec<(WaitingSaid, bool)>,
    /// Each stalled tab's row, and what it was posted for: a new reason or
    /// target is a new row, the same one is never posted again — not after
    /// the person read it either.
    stalled: BTreeMap<String, StallRow>,
    /// The owner's words written, by tab: the row each came back with, when, and the phase
    /// it was pressed on where the word moved it ([`UpgradeView::words_landed`]), so host
    /// rows read before a word was written never undo it on the glass.
    worded: BTreeMap<String, Worded>,
    /// The finished moves already recorded, by (conversation, when).
    done_said: BTreeSet<(String, u64)>,
    /// The faults of aterm's own already recorded, by fingerprint (kind and
    /// tab): one Warn record and one `aterm.log` line each, until the fault
    /// ends — a fault that returns is news
    /// ([`App::note_upgrade_faults`]).
    faults_said: BTreeSet<String>,
    /// The managed-current wire text last posted, so a change in the rows
    /// re-derives its words (`App::post_managed_current` records only news).
    managed_text: Option<String>,
    /// The tabs the host's last rows left as they stand ([`App::apply_host_upgrades_from`]):
    /// their record could not be read, so what the window shows of them is kept.
    kept: BTreeSet<String>,
    /// What writes the owner's word ([`WordWriter`]).
    writer: WordWriter,
    /// Where a press's word goes to be written off the winit thread.
    queue: WordQueue,
    /// What steps a press of the ⬆️ ([`Presser`]).
    presser: Presser,
    /// The tabs a press's lane steps now ([`App::start_press_lane`]): one lane a tab.
    pressing: Arc<std::sync::Mutex<BTreeSet<String>>>,
    /// Whether each press this window sent was made with its tab's harness or Autopilot off
    /// ([`PressAsk::off`]), by tab: a lane started for it from the host's rows, which can
    /// come before the press's own answer, types what the press asked.
    pressed_off: BTreeMap<String, bool>,
    /// Each tab's menu section as last composed from the rows
    /// ([`UpgradeView::menu_for`]), and its revision: the tab chrome's
    /// cache epoch for it (`CachedChrome::upgrade_revision`), bumped — and
    /// every window's strip handed its menus again — only when a section
    /// changes.
    menus: Vec<(String, crate::session_chrome::UpgradeMenu)>,
    revision: u64,
    /// How many times this view asked the host to look again ([`UpgradeView::look_again`]):
    /// the host's own count is process-wide, and parallel tests bump it too.
    #[cfg(test)]
    looks_asked: u64,
}

impl UpgradeView {
    /// What writes the owner's words from here on, as a test fakes it (the ⬆️'s tests, which
    /// drive a pane over a Unix socket).
    #[cfg(all(test, unix))]
    pub(crate) fn set_writer(&mut self, writer: WordWriter) {
        self.writer = writer;
    }

    /// What steps a press of the ⬆️ from here on, as a test fakes it.
    #[cfg(test)]
    pub(crate) fn set_presser(&mut self, presser: Presser) {
        self.presser = presser;
    }

    fn look_again(&mut self) {
        #[cfg(test)]
        {
            self.looks_asked += 1;
        }
        crate::harness_host::look_at_upgrades_soon();
    }

    fn word_landed(&mut self, row: &Row) {
        if row.request_at != 0
            && let Some(shown) = self.rows.iter().find(|r| same_move(r, row))
        {
            let pressed_on = (shown.phase != row.phase).then(|| shown.phase.clone());
            self.worded.insert(
                row.tab.clone(),
                (row.clone(), std::time::Instant::now(), pressed_on),
            );
        }
    }

    fn words_landed(&mut self, rows: &mut [Row]) {
        let now = now_s();
        self.worded.retain(|tab, (word, at, pressed_on)| {
            at.elapsed() < WORD_LAG
                && rows.iter().any(|r| {
                    r.tab == *tab
                        && same_move(r, word)
                        && (r.phase == word.phase || pressed_on.as_ref() == Some(&r.phase))
                        && (r.request_at < word.request_at || r.pressed_at < word.pressed_at)
                })
        });
        for r in rows.iter_mut() {
            if let Some((word, _, _)) = self.worded.get(&r.tab)
                && same_move(r, word)
            {
                let mut moved = false;
                if r.request_at < word.request_at {
                    r.request = word.request.clone();
                    r.request_at = word.request_at;
                    moved = true;
                }
                if r.pressed_at < word.pressed_at {
                    r.pressed_at = word.pressed_at;
                    r.pressed_to.clone_from(&word.pressed_to);
                    moved = true;
                }
                if moved {
                    r.lift_passed_aside(word, now);
                    r.reread(now);
                }
            }
        }
    }

    /// Create a view that writes owner actions through `writer` and steps a press of the ⬆️
    /// through `presser`.
    pub(crate) fn with_writer(writer: WordWriter, presser: Presser) -> Self {
        Self {
            writer,
            presser,
            ..Self::default()
        }
    }

    /// Record the wire text of the managed-current state.
    pub(crate) fn note_managed_text(&mut self, text: &str) {
        self.managed_text = Some(text.to_string());
    }

    /// Whether `word` is currently offered for `tab`'s upgrade to `to` at `now`.
    pub(crate) fn offers(&self, tab: &str, to: &str, word: UpgradeWord, now: u64) -> bool {
        self.shown.iter().any(|r| {
            r.tab == tab
                && same_build(&r.to, to)
                && message_reporters::agent_upgrade_words(r, now).contains(&word)
        })
    }

    /// Current upgrade [`Row`] shown for `tab`, if any.
    pub(crate) fn row_for(&self, tab: &str) -> Option<&Row> {
        self.shown.iter().find(|r| r.tab == tab)
    }

    /// Cached agent-upgrade menu section for `tab` at `now`.
    pub(crate) fn menu_for(
        &self,
        tab: &str,
        now: u64,
    ) -> Option<crate::session_chrome::UpgradeMenu> {
        menu_of(self.row_for(tab)?, now)
    }

    /// Menu cache revision counter.
    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }
}

fn menu_of(row: &Row, now: u64) -> Option<crate::session_chrome::UpgradeMenu> {
    let words = message_reporters::agent_upgrade_words(row, now);
    (!words.is_empty()).then(|| crate::session_chrome::UpgradeMenu {
        sid: row.tab.clone(),
        moving: atpkg::progress::sanitize_for_tty(&row.move_words(), 80),
        to: row.to.clone(),
        words,
    })
}

const fn seen_through(record: &aterm_messages::LogRecord) -> bool {
    matches!(
        record.retired(),
        Some(aterm_messages::Retired::Folded | aterm_messages::Retired::Dismissed)
    )
}

impl App {
    /// Synchronous fixture publication; the production host captures before its off-thread read.
    #[cfg(test)]
    pub(crate) fn apply_host_upgrades(&mut self, rows: Vec<Row>, kept: Vec<String>) {
        let sources = UpgradeSources::capture(&self.store);
        self.apply_host_upgrades_from(rows, kept, sources);
    }

    /// Apply `Wake::AgentUpgrade` rows from the harness host.
    pub(crate) fn apply_host_upgrades_from(
        &mut self,
        mut rows: Vec<Row>,
        kept: Vec<String>,
        sources: UpgradeSources,
    ) {
        let mut kept: BTreeSet<String> = kept.into_iter().collect();
        // A late batch leaves each changed foreground's previous publication untouched. This
        // applies to the menus and messages too, rather than only hiding its arrow afterward.
        let handles = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot();
        for handle in &handles {
            let timeline = handle
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if sources.current(handle.sid.as_str(), &timeline).is_none() {
                kept.insert(handle.sid.as_str().to_owned());
                rows.retain(|row| row.tab != handle.sid.as_str());
            }
        }
        let held: Vec<Row> = self
            .upgrade_view
            .rows
            .iter()
            .filter(|r| kept.contains(&r.tab) && !rows.iter().any(|n| n.session == r.session))
            .cloned()
            .collect();
        if !held.is_empty() {
            rows.extend(held.iter().cloned());
            rows.sort_by(|a, b| a.session.cmp(&b.session));
        }
        self.upgrade_view.kept = kept;
        self.upgrade_view.words_landed(&mut rows);
        // A press that stands with no lane stepping it (pressed in another window, or before
        // this one started) gets one; where this window did not send it, its restart types only
        // the relaunch line, since whether the tab's harness was on is not in the record.
        let now = now_s();
        let unstepped: Vec<WordAsk> = rows
            .iter()
            .filter(|r| r.press_stands(now))
            .map(|r| {
                let off = self
                    .upgrade_view
                    .pressed_off
                    .get(&r.tab)
                    .copied()
                    .unwrap_or(true);
                WordAsk {
                    session: r.session.clone(),
                    ..WordAsk::pressed(r.tab.clone(), r.to.clone(), off)
                }
            })
            .collect();
        self.apply_agent_upgrades_owned(rows, Some(&sources), &held);
        for ask in &unstepped {
            self.start_press_lane(ask);
        }
    }

    /// Synchronous fixture publication through the same custody admission as the queued host.
    #[cfg(test)]
    pub(crate) fn apply_agent_upgrades(&mut self, rows: Vec<Row>) {
        let sources = UpgradeSources::capture(&self.store);
        self.apply_agent_upgrades_owned(rows, Some(&sources), &[]);
    }

    /// Update upgrade state and messages. Fresh rows use producer stamps; kept records and word
    /// replies inherit the accepted row's owner. Neither path captures custody at delivery.
    fn apply_agent_upgrades_owned(
        &mut self,
        rows: Vec<Row>,
        sources: Option<&UpgradeSources>,
        inherited: &[Row],
    ) {
        let now = now_s();
        let before = self.tabs_behind();
        let shown = upgrade_drive::tab_rows(&rows);
        let snapshot = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot();
        let mut noted: Vec<u64> = Vec::new();
        for h in &snapshot {
            let row = shown.iter().find(|r| r.tab == h.sid.as_str()).cloned();
            if row.is_none() && self.upgrade_view.kept.contains(h.sid.as_str()) {
                continue;
            }
            let mut timeline = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            // The top rule's ⬆️ reads the row: a move of what it says repaints the footer.
            let arrow = |t: &crate::session_timeline::SessionTimeline| {
                t.upgrade().map(crate::claude_footer::TopUp::of)
            };
            let was = arrow(&timeline);
            let next = match row {
                Some(row) => {
                    let retained =
                        sources.is_none() || inherited.iter().any(|old| same_move(old, &row));
                    let stamp = if retained {
                        None
                    } else {
                        let Some(stamp) =
                            sources.and_then(|sources| sources.current(h.sid.as_str(), &timeline))
                        else {
                            continue;
                        };
                        Some(stamp)
                    };
                    let owner = timeline
                        .accepted_upgrade_owner(&row)
                        .or_else(|| stamp.and_then(|stamp| stamp.owner(&row)));
                    Some((row, owner))
                }
                None => {
                    if sources
                        .and_then(|sources| sources.current(h.sid.as_str(), &timeline))
                        .is_none()
                    {
                        continue;
                    }
                    None
                }
            };
            timeline.set_upgrade_owned(next);
            let arrow_moved = arrow(&timeline) != was;
            if arrow_moved {
                noted.push(h.local_id);
            }
        }
        for session in noted {
            self.redraw_claude_footer(session);
        }
        self.record_upgrade_waiting(&shown, now);
        self.post_upgrade_stalls(&shown, &rows, now);
        self.note_upgrade_faults(&rows, now);
        self.note_upgrade_exceptions(&shown, now);
        for row in rows.iter().filter(|r| r.phase == Phase::Done) {
            if row.done_at == 0
                || !self
                    .upgrade_view
                    .done_said
                    .insert((row.session.clone(), row.done_at))
            {
                continue;
            }
            let place = self.upgrade_place(&row.tab);
            self.record_message(message_reporters::agent_upgrade_done(row, &place));
        }
        let menus: Vec<(String, crate::session_chrome::UpgradeMenu)> = shown
            .iter()
            .filter_map(|r| Some((r.tab.clone(), menu_of(r, now)?)))
            .collect();
        self.upgrade_view.rows = rows;
        self.upgrade_view.shown = shown;
        if menus != self.upgrade_view.menus {
            self.upgrade_view.menus = menus;
            self.upgrade_view.revision = self.upgrade_view.revision.wrapping_add(1);
            let windows: Vec<crate::WindowId> = self.windows.keys().copied().collect();
            self.refresh_tab_chrome_windows(windows);
        }
        if self.tabs_behind() != before
            && let Some(text) = self.upgrade_view.managed_text.clone()
        {
            let _ = self.post_managed_current(&text);
        }
    }

    /// Dispatch a selected tab-menu upgrade action for `sid`.
    pub(crate) fn dispatch_tab_menu_upgrade(
        &mut self,
        window: crate::WindowId,
        tab: crate::tab_model::TabId,
        sid: String,
        to: String,
        word: UpgradeWord,
    ) {
        let in_tab = self
            .windows
            .get(&window)
            .and_then(|ws| ws.tab_set.tabs().iter().find(|t| t.id == tab))
            .is_some_and(|t| {
                let store = self.store.read().unwrap_or_else(|p| p.into_inner());
                t.root.leaves().into_iter().any(|view| {
                    self.view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        .and_then(|session| store.by_local(session))
                        .is_some_and(|h| h.sid.as_str() == sid)
                })
            });
        if !in_tab {
            aterm_log::info!(
                "tab context-menu upgrade word {} dropped: tab {tab} no longer runs session \
                 {sid} in its window",
                word.as_str()
            );
            return;
        }
        let _ = self.send_upgrade_word(WordAsk::new(sid, to, word));
    }

    fn record_upgrade_waiting(&mut self, rows: &[Row], now: u64) {
        let waiting: Vec<&Row> = rows
            .iter()
            .filter(|r| {
                matches!(r.phase, Phase::Pending | Phase::Announced { .. })
                    && r.moving(now)
                    && r.stall(now).is_none()
                    && r.request != Request::Now
            })
            .collect();
        let versions: Vec<&str> = waiting.iter().map(|r| r.to.as_str()).collect();
        let mut products: Vec<&str> = waiting.iter().map(|r| r.agent.product()).collect();
        products.sort_unstable();
        products.dedup();
        let product = products.join(" and ");
        let one = match waiting.as_slice() {
            [only] if !only.tab.is_empty() => {
                let place = self.upgrade_place(&only.tab);
                Some(message_reporters::agent_upgrade_waiting_in(only, &place))
            }
            _ => None,
        };
        let Some(mut msg) =
            one.or_else(|| message_reporters::agent_upgrade_waiting(&product, &versions))
        else {
            self.upgrade_view.waiting_said.clear();
            return;
        };
        if let [only] = waiting.as_slice() {
            for intent in message_reporters::agent_upgrade_capsules(only, now) {
                msg = msg.action(intent);
            }
        }
        let floor = matches!(waiting.as_slice(), [only] if !only.waits_for_comfort());
        let said = (msg.title.clone(), msg.detail.clone(), msg.actions.clone());
        let before = &self.upgrade_view.waiting_said;
        let again = before.iter().any(|(was, _)| *was == said);
        let under_floor = !floor
            && before
                .iter()
                .any(|((title, ..), was_floor)| *was_floor && *title == said.0);
        if again || under_floor {
            return;
        }
        self.upgrade_view.waiting_said.push((said, floor));
        self.record_message(msg);
    }

    fn stall_already_shown(&self, msg: &aterm_messages::Message) -> Option<MessageId> {
        let key = msg.key.as_deref()?;
        if msg.hold == aterm_messages::Hold::LogOnly {
            return self.stall_already_recorded(key, msg);
        }
        if let Some(live) = self.messages.live_by_key(key) {
            return message_reporters::same_stall(&live.msg.detail, &msg.detail).then_some(live.id);
        }
        let record = self
            .messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(key))?;
        (seen_through(record) && message_reporters::same_stall(&record.detail, &msg.detail))
            .then_some(record.id)
    }

    fn stall_already_recorded(
        &self,
        key: &str,
        msg: &aterm_messages::Message,
    ) -> Option<MessageId> {
        let record = self
            .messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(key))?;
        (record.retired_at.is_none()
            && record.retired() == Some(&aterm_messages::Retired::Recorded)
            && record.severity == msg.severity
            && message_reporters::same_stall(&record.detail, &msg.detail))
        .then_some(record.id)
    }

    fn record_stall_over(
        &mut self,
        posted: &StallRow,
        place: &str,
        end: message_reporters::StallEnd,
    ) {
        let Some((key, detail)) = self
            .messages
            .log()
            .get(posted.id)
            .filter(|rec| seen_through(rec))
            .and_then(|rec| Some((rec.key.clone()?, rec.detail.clone())))
        else {
            return;
        };
        let newer = self
            .messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(key.as_str()))
            .is_some_and(|r| r.id != posted.id);
        if newer {
            return;
        }
        self.record_message(message_reporters::agent_upgrade_stall_over(
            posted.agent,
            &key,
            &detail,
            place,
            end,
        ));
    }

    fn note_upgrade_faults(&mut self, rows: &[Row], now: u64) {
        use aterm_agent::harness::upgrade_drive::Attention;
        let faults: BTreeMap<String, (&Row, aterm_agent::harness::upgrade_drive::Need)> = rows
            .iter()
            .filter_map(|r| match r.attention(now) {
                Attention::Fault(need) => Some((need.fingerprint.clone(), (r, need))),
                _ => None,
            })
            .collect();
        self.upgrade_view
            .faults_said
            .retain(|fingerprint| faults.contains_key(fingerprint));
        for (fingerprint, (row, need)) in faults {
            if !self.upgrade_view.faults_said.insert(fingerprint) {
                continue;
            }
            aterm_log::warn!("{}", need.log_line);
            let place = self.upgrade_place(&row.tab);
            self.record_message(message_reporters::agent_upgrade_fault(row, &need, &place));
        }
    }

    fn note_upgrade_exceptions(&mut self, rows: &[Row], now: u64) {
        let spots: Vec<(&Row, message_reporters::UpgradeSpot)> = rows
            .iter()
            .map(|row| (row, self.upgrade_spot(&row.tab)))
            .collect();
        let entries = message_reporters::upgrade_exceptions(&spots, now);
        self.except_replace(aterm_messages::exceptions::Family::Upgrade, &entries);
    }

    fn entry_reshaped(
        &self,
        posted: &StallRow,
        row: &Row,
        fresh: &str,
        now: u64,
    ) -> Option<Reshape> {
        if posted.shows != posted.said {
            return None;
        }
        let way = reshaped(&posted.said, fresh)?;
        let fits = match way {
            Reshape::ToRecord => {
                self.messages.live(posted.id).is_some()
                    && message_reporters::agent_upgrade_stalled(row, now, "").hold
                        == aterm_messages::Hold::LogOnly
            }
            Reshape::ToRow => {
                self.messages.live(posted.id).is_none()
                    && self.messages.log().get(posted.id).is_some_and(|rec| {
                        rec.retired() == Some(&aterm_messages::Retired::Recorded)
                    })
            }
        };
        fits.then_some(way)
    }

    fn reword_stall_record(
        &mut self,
        id: MessageId,
        msg: aterm_messages::Message,
        behind: Option<&str>,
    ) {
        let Some(rec) = self.messages.log().get(id) else {
            return;
        };
        let msg = msg.normalized();
        let msg = if msg.actions == rec.actions {
            msg
        } else {
            let Some(first) = behind
                .zip(rec.detail.first())
                .and_then(|(age, l)| aged(l, age))
            else {
                return;
            };
            let mut detail = rec.detail.clone();
            detail[0] = first;
            let mut kept = msg;
            kept.title = rec.title.clone();
            kept.detail = detail;
            kept.actions = rec.actions.clone();
            kept
        };
        let _ = self.reword_record(id, &msg);
    }

    fn post_upgrade_stalls(&mut self, shown: &[Row], rows: &[Row], now: u64) {
        let stalled: BTreeMap<String, (&Row, String)> = shown
            .iter()
            .filter(|r| !self.upgrade_view.kept.contains(&r.tab))
            .filter_map(|r| {
                let stall = r.stall(now)?;
                let shape = if r.interrupt(now).is_some() {
                    "row"
                } else {
                    "record"
                };
                Some((r.tab.clone(), (r, format!("{stall}/{}/{shape}", r.to))))
            })
            .collect();
        let in_flight: Vec<&str> = rows
            .iter()
            .filter(|r| matches!(r.phase, Phase::Exiting { .. } | Phase::Relaunched { .. }))
            .map(|r| r.tab.as_str())
            .collect();
        let rearmed = |tab: &str, posted: &StallRow| {
            posted.said.starts_with("refused:")
                && rows.iter().any(|r| {
                    r.tab == tab
                        && same_build(&r.to, &posted.to)
                        && matches!(r.phase, Phase::Pending | Phase::Announced { .. })
                        && r.moving(now)
                })
        };
        let gone: Vec<(String, bool)> = self
            .upgrade_view
            .stalled
            .iter()
            .filter_map(|(tab, posted)| {
                let flying = in_flight.contains(&tab.as_str());
                match stalled.get(tab) {
                    None if self.upgrade_view.kept.contains(tab) => None,
                    None if flying && stuck(&posted.said) => Some((tab.clone(), true)),
                    None if flying => None,
                    None if rearmed(tab, posted) => None,
                    None => Some((tab.clone(), true)),
                    Some((_, fresh))
                        if flying && stuck(fresh) && self.messages.live(posted.id).is_some() =>
                    {
                        None
                    }
                    Some((row, fresh))
                        if self.entry_reshaped(posted, row, fresh, now).is_some() =>
                    {
                        None
                    }
                    Some((_, fresh)) if *fresh != posted.said => Some((tab.clone(), false)),
                    Some(_) => None,
                }
            })
            .collect();
        for (tab, recovered) in gone {
            let Some(posted) = self.upgrade_view.stalled.remove(&tab) else {
                continue;
            };
            let id = posted.id;
            let done = rows.iter().find(|r| {
                recovered && r.tab == tab && r.phase == Phase::Done && same_build(&r.to, &posted.to)
            });
            if let Some(done) = done {
                let place = self.upgrade_place(&tab);
                let _ = self.restate_message(
                    id,
                    aterm_messages::Restatement {
                        finished: Some(Some(message_reporters::agent_upgrade_went_through(
                            done, &place,
                        ))),
                        ..aterm_messages::Restatement::default()
                    },
                );
            }
            let live = if recovered {
                self.resolve_message(id, Outcome::Ok)
            } else {
                self.withdraw_message(id)
            };
            if recovered && !live && done.is_none() {
                use message_reporters::StallEnd;
                let place = self.upgrade_place(&tab);
                let end = if in_flight.contains(&tab.as_str()) && stuck(&posted.said) {
                    StallEnd::MovesAgain
                } else if rows.iter().any(|r| r.tab == tab) {
                    StallEnd::AsksAgain
                } else {
                    StallEnd::Left
                };
                self.record_stall_over(&posted, &place, end);
            }
        }
        let flips: Vec<(String, String, Reshape)> = stalled
            .iter()
            .filter_map(|(tab, (row, fresh))| {
                let posted = self.upgrade_view.stalled.get(tab)?;
                let way = self.entry_reshaped(posted, row, fresh, now)?;
                Some((tab.clone(), fresh.clone(), way))
            })
            .collect();
        let mut replaced: BTreeMap<String, MessageId> = BTreeMap::new();
        for (tab, fresh, way) in flips {
            let Some((row, _)) = stalled.get(&tab) else {
                continue;
            };
            match way {
                Reshape::ToRecord => {
                    let Some(id) = self.upgrade_view.stalled.get(&tab).map(|p| p.id) else {
                        continue;
                    };
                    let spot = self.upgrade_spot(&tab);
                    let msg = message_reporters::agent_upgrade_stalled_at(row, now, &spot);
                    let _ = self.keep_as_record(id, &msg);
                    if let Some(posted) = self.upgrade_view.stalled.get_mut(&tab) {
                        posted.shows.clone_from(&fresh);
                        posted.said = fresh;
                        posted.person = row.person_seen(now);
                        posted.behind = row.age_said(now);
                        posted.place = spot.place;
                    }
                }
                Reshape::ToRow => {
                    if let Some(posted) = self.upgrade_view.stalled.remove(&tab) {
                        replaced.insert(tab, posted.id);
                    }
                }
            }
        }
        let moved: Vec<(MessageId, Option<aterm_messages::Restatement>)> = self
            .upgrade_view
            .stalled
            .iter_mut()
            .filter(|(tab, posted)| {
                in_flight.contains(&tab.as_str())
                    && !stalled.contains_key(*tab)
                    && posted.shows != posted.said
            })
            .map(|(_, posted)| {
                posted.shows.clone_from(&posted.said);
                (posted.id, posted.beneath.take())
            })
            .collect();
        for (id, beneath) in moved {
            if let Some(words) = beneath {
                let _ = self.restate_message(id, words);
            }
        }
        for (tab, (row, said)) in stalled {
            if let Some(posted) = self.upgrade_view.stalled.get(&tab) {
                let id = posted.id;
                if said != posted.shows {
                    let beneath = (said != posted.said
                        && posted.shows == posted.said
                        && !stuck(&posted.said))
                    .then(|| {
                        self.messages
                            .live(id)
                            .map(|l| crate::messages_host::restatement_of(&l.msg))
                    })
                    .flatten();
                    let spot = self.upgrade_spot(&row.tab);
                    let msg = message_reporters::agent_upgrade_stalled_at(row, now, &spot);
                    let _ = self.restate_message(id, crate::messages_host::restatement_of(&msg));
                    if let Some(posted) = self.upgrade_view.stalled.get_mut(&tab) {
                        if said == posted.said {
                            posted.beneath = None;
                        } else if beneath.is_some() {
                            posted.beneath = beneath;
                        }
                        posted.shows = said;
                        posted.person = row.person_seen(now);
                        posted.behind = row.age_said(now);
                        posted.place = spot.place;
                    }
                    continue;
                }
                let person = row.person_seen(now);
                let behind = row.age_said(now);
                let spot = self.upgrade_spot(&row.tab);
                let msg = message_reporters::agent_upgrade_stalled_at(row, now, &spot);
                let live = self.messages.live(id);
                let moved = posted.person != person || posted.behind != behind;
                let followed = live
                    .is_some_and(|l| posted.place != spot.place || l.msg.actions != msg.actions);
                if moved || followed {
                    if live.is_some() {
                        let _ =
                            self.restate_message(id, crate::messages_host::restatement_of(&msg));
                    } else {
                        self.reword_stall_record(id, msg, behind.as_deref());
                    }
                }
                if let Some(posted) = self.upgrade_view.stalled.get_mut(&tab) {
                    posted.person = person;
                    posted.behind = behind;
                    posted.place = spot.place;
                }
                continue;
            }
            let spot = self.upgrade_spot(&row.tab);
            let msg = message_reporters::agent_upgrade_stalled_at(row, now, &spot);
            if let Some(id) = self.stall_already_shown(&msg) {
                // Still on the band (carried): brought to this build's words
                // — and buttons — where they differ. Off it: the owner saw it,
                // and it stays down.
                if self.messages.live(id).is_some_and(|l| {
                    l.msg.title != msg.title
                        || l.msg.detail != msg.detail
                        || l.msg.actions != msg.actions
                }) {
                    let _ = self.restate_message(id, crate::messages_host::restatement_of(&msg));
                }
                if let Some(record) = replaced.remove(&tab) {
                    let _ = self.supersede_record(record, id);
                }
                self.upgrade_view.stalled.insert(
                    tab,
                    StallRow {
                        id,
                        shows: said.clone(),
                        said,
                        to: row.to.clone(),
                        agent: row.agent,
                        beneath: None,
                        person: row.person_seen(now),
                        behind: row.age_said(now),
                        place: spot.place,
                    },
                );
                continue;
            }
            let id = self.post_message(msg);
            if let Some(record) = replaced.remove(&tab) {
                let _ = self.supersede_record(record, id);
            }
            self.upgrade_view.stalled.insert(
                tab,
                StallRow {
                    id,
                    shows: said.clone(),
                    said,
                    to: row.to.clone(),
                    agent: row.agent,
                    beneath: None,
                    person: row.person_seen(now),
                    behind: row.age_said(now),
                    place: spot.place,
                },
            );
        }
        let owned: BTreeSet<MessageId> = self.upgrade_view.stalled.values().map(|p| p.id).collect();
        let restarting: BTreeSet<String> = in_flight
            .iter()
            .copied()
            .chain(self.upgrade_view.kept.iter().map(String::as_str))
            .map(message_reporters::agent_upgrade_key)
            .collect();
        let left: Vec<MessageId> = self
            .messages
            .live_rows()
            .filter(|l| !owned.contains(&l.id))
            .filter(|l| {
                l.msg.key.as_deref().is_some_and(|key| {
                    message_reporters::agent_upgrade_key_tab(key).is_some()
                        && !restarting.contains(key)
                })
            })
            .map(|l| l.id)
            .collect();
        for id in left {
            let _ = self.withdraw_message(id);
        }
    }

    /// Starts the lane of the owner's press `ask` ([`upgrade_drive::press`]) off the winit
    /// thread, where no lane steps its tab yet: it steps the tab until the press lapses or the
    /// move it began lands, each row it reads coming back as `Wake::AgentUpgradePress`. Without
    /// an event loop (a test's app) one step runs here.
    pub(crate) fn start_press_lane(&mut self, ask: &WordAsk) {
        let lanes = Arc::clone(&self.upgrade_view.pressing);
        if !lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(ask.tab.clone())
        {
            return;
        }
        let presser = self.upgrade_view.presser.clone();
        let Some(proxy) = self.proxy.clone() else {
            let (_, row) = presser.step(ask);
            if let Some(row) = row {
                self.apply_press_row(row);
            }
            lanes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&ask.tab);
            return;
        };
        let tab = ask.tab.clone();
        let ask = ask.clone();
        let lane = Arc::clone(&lanes);
        let started = crate::qos::spawn_lane_thread(
            "aterm-upgrade-press".to_string(),
            crate::qos::Role::Responsive,
            move || {
                run_press_lane(&ask, &presser, &lane, press_pace, |row| {
                    let _ = proxy.send_event(crate::Wake::AgentUpgradePress { row });
                });
            },
        );
        if let Err(e) = started {
            aterm_log::warn!("harness @{tab}: the press's lane did not start: {e}");
            lanes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&tab);
        }
    }

    /// Apply a row a press's lane read (`Wake::AgentUpgradePress`): laid over the tab's row as
    /// shown, or added where the window shows none (the host stood down with the harness off).
    /// Like a word's reply it inherits the accepted owner of its run (the row the press was
    /// made on, shown or kept), capturing no custody at delivery.
    pub(crate) fn apply_press_row(&mut self, row: Row) {
        let now = now_s();
        let rows = laid_in(&self.upgrade_view.rows, row.clone(), now).unwrap_or_else(|| {
            let mut rows: Vec<Row> = self
                .upgrade_view
                .rows
                .iter()
                .filter(|r| r.tab != row.tab)
                .cloned()
                .collect();
            rows.push(row);
            rows.sort_by(|a, b| a.session.cmp(&b.session));
            rows
        });
        self.apply_agent_upgrades_owned(rows, None, &[]);
    }

    /// Queue an owner upgrade action from a band capsule, Settings button, or tab menu.
    pub(crate) fn send_upgrade_word(&mut self, mut ask: WordAsk) -> bool {
        if let Some(press) = ask.press {
            self.upgrade_view
                .pressed_off
                .insert(ask.tab.clone(), press.off);
        }
        if let Some(row) = self
            .upgrade_view
            .row_for(&ask.tab)
            .filter(|r| same_build(&r.to, &ask.to))
        {
            ask.session.clone_from(&row.session);
            ask.scope = Some(upgrade_drive::AskScope::from(row));
        }
        let writer = self.upgrade_view.writer.clone();
        let Some(proxy) = self.proxy.clone() else {
            let result = writer.write(&ask);
            self.apply_upgrade_word(&ask, result);
            return true;
        };
        let sent = self
            .upgrade_view
            .queue
            .send(ask.clone(), &writer, move |ask, result| {
                let _ = proxy.send_event(crate::Wake::AgentUpgradeWord { ask, result });
            });
        match sent {
            Ok(()) => true,
            Err(why) => {
                self.apply_upgrade_word(&ask, Err(why));
                false
            }
        }
    }

    /// Apply the result of an owner upgrade action (`Wake::AgentUpgradeWord`).
    pub(crate) fn apply_upgrade_word(&mut self, ask: &WordAsk, result: Result<Row, String>) {
        let now = now_s();
        let posted = self
            .upgrade_view
            .stalled
            .get(&ask.tab)
            .filter(|posted| same_build(&posted.to, &ask.to))
            .map(|posted| posted.id);
        let spot = self.upgrade_spot(&ask.tab);
        let place = spot.place.clone();
        match result {
            Ok(row) => {
                let posted = posted.filter(|_| {
                    self.upgrade_view
                        .row_for(&ask.tab)
                        .is_some_and(|shown| same_move(shown, &row))
                });
                aterm_log::info!(
                    "harness @{}: the owner's word from the window: {} ({})",
                    ask.tab,
                    ask.word.as_str(),
                    row.request.word()
                );
                let asked_again = self
                    .upgrade_view
                    .row_for(&ask.tab)
                    .is_some_and(message_reporters::upgrade_now_asks_again);
                let said = message_reporters::agent_upgrade_worded(
                    &row,
                    ask.word,
                    ask.press.is_some(),
                    &place,
                    asked_again,
                );
                self.upgrade_view.word_landed(&row);
                let answer = |app: &mut App, id: MessageId| {
                    let stall_title = app.messages.live(id).map(|l| l.msg.title.clone());
                    let mut detail = said.detail.clone();
                    detail.extend(stall_title);
                    app.restate_message(
                        id,
                        aterm_messages::Restatement {
                            title: Some(said.title.clone()),
                            detail: Some(detail),
                            severity: Some(said.severity),
                            glyph: Some(said.glyph),
                            finished: Some(None),
                            ..aterm_messages::Restatement::default()
                        },
                    ) && app.answer_message(id, ask.word.label())
                };
                let answered = posted.is_some_and(|id| answer(self, id));
                if answered {
                    self.upgrade_view.stalled.remove(&ask.tab);
                } else {
                    self.record_message(said);
                }
                if let Some(rows) = laid_in(&self.upgrade_view.rows, row.clone(), now_s()) {
                    self.apply_agent_upgrades_owned(rows, None, &[]);
                    if let Some(local) =
                        self.local_session_of(&aterm_session::SessionId::new(&row.tab))
                        && let Some(handle) = self.pool.get(local)
                    {
                        handle
                            .ctx
                            .timeline
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .upgrade_word_accepted(&row);
                    }
                }
                if ask.press.is_some() {
                    self.start_press_lane(ask);
                }
            }
            Err(why) => {
                aterm_log::warn!(
                    "harness @{}: the owner's word from the window ({}) was not written: {why}",
                    ask.tab,
                    ask.word.as_str()
                );
                if ask.press.is_some() {
                    self.upgrade_view.pressed_off.remove(&ask.tab);
                }
                if ask.word == UpgradeWord::Now
                    && let Some(local) =
                        self.local_session_of(&aterm_session::SessionId::new(&ask.tab))
                {
                    self.show_upgrade_refusal(
                        local,
                        &message_reporters::agent_upgrade_refused_words(&why),
                    );
                }
                let shown = self.upgrade_view.row_for(&ask.tab).cloned();
                let agent = shown.as_ref().map(|r| r.agent).unwrap_or_default();
                let refused = message_reporters::agent_upgrade_word_refused(
                    ask.word,
                    agent,
                    (&ask.tab, &ask.to),
                    &why,
                    &place,
                );
                let stopped = upgrade_drive::refused_stopped(&why).is_some();
                if stopped {
                    self.upgrade_view.look_again();
                }
                let restated = match (posted, shown.as_ref()) {
                    (Some(id), Some(row)) => {
                        let stalled = message_reporters::agent_upgrade_stalled_at(row, now, &spot);
                        let mut words = refused.clone();
                        if stopped {
                            words
                                .detail
                                .extend(stalled.detail.into_iter().skip(2).take(1));
                            words.actions = message_reporters::agent_upgrade_words(row, now)
                                .into_iter()
                                .filter(|word| *word != ask.word)
                                .take(2)
                                .map(|word| Intent::AgentUpgrade {
                                    tab: row.tab.clone(),
                                    to: row.to.clone(),
                                    word,
                                })
                                .collect();
                        } else {
                            words.detail.extend(stalled.detail);
                            words.actions = stalled.actions;
                        }
                        words.hold = stalled.hold;
                        words.severity = stalled.severity;
                        words.glyph = stalled.glyph;
                        self.restate_message(id, crate::messages_host::restatement_of(&words))
                    }
                    _ => false,
                };
                if restated && let Some(posted) = self.upgrade_view.stalled.get_mut(&ask.tab) {
                    posted.place = spot.place;
                }
                if !restated {
                    let _ = self.post_message(refused);
                }
            }
        }
    }

    /// Human-readable tab location for `sid` (such as `in tab 2`).
    pub(crate) fn upgrade_place(&self, sid: &str) -> String {
        self.upgrade_spot(sid).place
    }

    /// Location text and `Show tab` navigation target for `sid`.
    pub(crate) fn upgrade_spot(&self, sid: &str) -> message_reporters::UpgradeSpot {
        let store = self.store.read().unwrap_or_else(|p| p.into_inner());
        let several = self.windows.len() > 1;
        for (window, (wid, ws)) in self.windows.iter().enumerate() {
            for (index, tab) in ws.tab_set.tabs().iter().enumerate() {
                let shown = tab.root.any_leaf(&mut |view| {
                    self.view_store
                        .get(*view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        .and_then(|local| store.by_local(local))
                        .is_some_and(|h| h.sid.as_str() == sid)
                });
                if shown {
                    return message_reporters::UpgradeSpot {
                        place: message_reporters::tab_place(
                            several.then_some(window + 1),
                            index + 1,
                        ),
                        nav: u16::try_from(index + 1)
                            .ok()
                            .map(|tab| Intent::ShowSession {
                                tab,
                                window: wid.0,
                                session: sid.to_string(),
                            }),
                    };
                }
            }
        }
        message_reporters::UpgradeSpot {
            place: "in its tab".to_string(),
            nav: None,
        }
    }

    /// Human-readable tab and pane location for supervised agent session `sid`.
    pub(crate) fn session_place(&self, sid: &str) -> String {
        let place = self.upgrade_place(sid);
        match self.session_pane(sid, false) {
            Some(pane) => format!("{place}, {pane}"),
            None => place,
        }
    }

    /// Human-readable tab and pane location for `sid` and whether its tab is visible.
    pub(crate) fn pane_place(&self, sid: &str) -> (String, bool) {
        let place = self.upgrade_place(sid);
        match self.session_pane(sid, true) {
            Some(pane) => (format!("{place}, {pane}"), true),
            None => (place, false),
        }
    }

    fn session_pane(&self, sid: &str, every_pane: bool) -> Option<String> {
        let local_of = |view: &crate::tab_model::ViewId| {
            self.view_store
                .get(*view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
        };
        let is_sid = |local: u64| {
            self.pool
                .get(local)
                .is_some_and(|session| session.ctx.self_id.as_str() == sid)
        };
        let observed_agent = |local: u64| {
            self.pool
                .get(local)
                .and_then(|session| crate::harness_host::admitted(&session.ctx))
        };
        self.windows.values().find_map(|ws| {
            ws.tab_set.tabs().iter().find_map(|tab| {
                tab.root
                    .any_leaf(&mut |view| local_of(view).is_some_and(is_sid))
                    .then(|| {
                        if every_pane {
                            pane_word(
                                &tab.root,
                                |view| local_of(view).is_some_and(is_sid),
                                |_| true,
                            )
                        } else {
                            agent_pane_word(
                                &tab.root,
                                |view| local_of(view).is_some_and(is_sid),
                                |view| local_of(view).and_then(observed_agent),
                            )
                        }
                    })
            })
        })?
    }

    /// All session IDs currently shown in any tab of this window.
    pub(crate) fn shown_session_sids(&self) -> std::collections::HashSet<String> {
        let store = self.store.read().unwrap_or_else(|p| p.into_inner());
        let mut sids = std::collections::HashSet::new();
        for ws in self.windows.values() {
            for tab in ws.tab_set.tabs() {
                tab.root.any_leaf(&mut |view| {
                    if let Some(handle) = self
                        .view_store
                        .get(*view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        .and_then(|local| store.by_local(local))
                    {
                        sids.insert(handle.sid.as_str().to_string());
                    }
                    false
                });
            }
        }
        sids
    }

    /// Summary of live tabs running behind the managed-current agent build.
    pub(crate) fn tabs_behind(&self) -> TabsBehind {
        let now = now_s();
        let store = self.store.read().unwrap_or_else(|p| p.into_inner());
        let mut tabs = TabsBehind {
            frozen_shells: self
                .seamless_adopt
                .iter()
                .filter(|adopted| adopted.frozen_path)
                .count(),
            ..TabsBehind::default()
        };
        for id in store.running_ids() {
            let Some(h) = store.by_local(id) else {
                continue;
            };
            let timeline = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            if !store.has_frozen_path(id) {
                tabs.shadowed += usize::from(timeline.path_settled_frozen());
                continue;
            }
            let agent = crate::harness_host::identified_agent(timeline.agent()).is_some()
                || timeline
                    .agent()
                    .program
                    .as_deref()
                    .is_some_and(atpkg::stub::is_agent_program);
            let moving = timeline.upgrade().is_some_and(|row| row.moving(now));
            match (agent, moving) {
                (false, _) => tabs.frozen_shells += 1,
                (true, false) => tabs.frozen_agents += 1,
                (true, true) => {}
            }
        }
        let text = self.upgrade_view.managed_text.as_deref();
        for row in &self.upgrade_view.shown {
            let managed = text.and_then(|text| {
                crate::toolchain_words::managed_version(text, row.agent.program().name())
            });
            if !managed.as_deref().is_some_and(|v| runs_older(row, v)) {
                continue;
            }
            if row.phase == Phase::Done || (row.moving(now) && row.stall(now).is_none()) {
                tabs.agents_behind.count(row.agent);
            } else {
                tabs.agents_held.count(row.agent);
            }
        }
        tabs
    }
}

fn runs_older(row: &Row, managed: &str) -> bool {
    use aterm_agent::harness::upgrade::Version;
    let runs = if matches!(row.phase, Phase::Done | Phase::Relaunched { .. }) {
        &row.to
    } else {
        &row.from
    };
    match (Version::parse(runs), Version::parse(managed)) {
        (Some(runs), Some(managed)) => runs < managed,
        _ => !same_build(runs, managed),
    }
}

fn same_build(a: &str, b: &str) -> bool {
    use aterm_agent::harness::upgrade::Version;
    match (Version::parse(a), Version::parse(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

fn agent_pane_word<T>(
    tree: &crate::tab_model::SplitTree<T>,
    is_target: impl Fn(&T) -> bool,
    admitted: impl Fn(&T) -> Option<aterm_phase::Program>,
) -> Option<String> {
    pane_word(tree, is_target, |leaf| {
        admitted(leaf).is_some_and(|program| program.harness_table())
    })
}

/// Compass or numbered pane label for the leaf selected by `is_target` in `tree`.
#[must_use]
pub(crate) fn pane_word<T>(
    tree: &crate::tab_model::SplitTree<T>,
    is_target: impl Fn(&T) -> bool,
    is_agent: impl Fn(&T) -> bool,
) -> Option<String> {
    use crate::tab_model::{SplitAxis, SplitTree};
    let agents = |tree: &SplitTree<T>| {
        let mut n = 0_usize;
        tree.visit(&mut |leaf| n += usize::from(is_agent(leaf)));
        n
    };
    if agents(tree) < 2 {
        return None;
    }
    if let SplitTree::Split {
        axis,
        first,
        second,
        ..
    } = tree
        && agents(first) == 1
        && agents(second) == 1
    {
        let in_first = first.any_leaf(&mut |leaf| is_target(leaf));
        let side = match (axis, in_first) {
            (SplitAxis::Horizontal, true) => "left",
            (SplitAxis::Horizontal, false) => "right",
            (SplitAxis::Vertical, true) => "top",
            (SplitAxis::Vertical, false) => "bottom",
        };
        return Some(format!("{side} pane"));
    }
    let mut index = 0_usize;
    let mut found = None;
    tree.visit(&mut |leaf| {
        index += 1;
        if found.is_none() && is_target(leaf) {
            found = Some(index);
        }
    });
    found.map(|n| format!("pane {n}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchain_words::PerAgent;
    use aterm_agent::harness::upgrade::{Agent, Request};

    fn row(tab: &str, behind_for: u64) -> Row {
        let now = now_s();
        Row {
            session: format!("conv-{tab}"),
            tab: tab.to_string(),
            from: "2.1.281".to_string(),
            to: "2.1.282".to_string(),
            source: "managed".to_string(),
            phase: Phase::Pending,
            behind_since: now - behind_for,
            wait: "queued".to_string(),
            wait_since: now - behind_for,
            ..Row::default()
        }
    }

    fn gone(tab: &str) -> Row {
        Row {
            behind_since: now_s() - 7 * 3_600,
            ..refused(tab)
        }
    }

    fn records_titled(app: &App, part: &str) -> Vec<(String, Vec<String>)> {
        harness_records(app)
            .into_iter()
            .filter(|(title, _)| title.contains(part))
            .collect()
    }

    fn harness_records(app: &App) -> Vec<(String, Vec<String>)> {
        app.messages
            .log()
            .records()
            .filter(|r| r.tag == aterm_messages::tags::HARNESS)
            .map(|r| (r.title.clone(), r.detail.clone()))
            .collect()
    }

    #[test]
    fn status_names_the_tabs_newest_move() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let local = app.focused_session_id(wid).expect("the front session");
        if app.store.read().unwrap().by_local(local).is_none() {
            app.store
                .write()
                .unwrap()
                .register(crate::session_store::test_handle(local));
        }
        let tab = app
            .store
            .read()
            .unwrap()
            .by_local(local)
            .unwrap()
            .sid
            .as_str()
            .to_string();
        crate::claude_footer::probe::drawer(&app, local, 4343, "claude", "2.1.281");
        app.pool
            .get(local)
            .unwrap()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .set_program(4343, Some("claude".into()));
        let now = now_s();
        let finished = Row {
            session: "2d28d268-0000-4000-8000-000000000001".to_string(),
            phase: Phase::Done,
            done_at: now - 180,
            minted_at: now - 600,
            ..row(&tab, 3_600)
        };
        let failed = Row {
            session: "b529bdd0-0000-4000-8000-000000000002".to_string(),
            phase: Phase::Failed("no-resume".to_string()),
            exited_at: now - 60,
            minted_at: now - 120,
            ..row(&tab, 3_600)
        };
        let shown = |app: &App| {
            let store = app.store.read().unwrap();
            let handle = store.by_local(local).expect("the session");
            let timeline = handle.ctx.timeline.lock().unwrap();
            timeline.upgrade().map(|r| r.session.clone())
        };
        app.apply_agent_upgrades(vec![finished.clone(), failed.clone()]);
        assert_eq!(shown(&app), Some(failed.session.clone()), "the newest move");
        app.apply_agent_upgrades(vec![failed.clone(), finished.clone()]);
        assert_eq!(
            shown(&app),
            Some(failed.session.clone()),
            "CONTROL: either order"
        );
    }

    #[test]
    fn a_wait_flipping_between_comfort_and_a_floor_is_recorded_once_each() {
        let claude = |tab: &str, wait: &str| Row {
            wait: wait.to_string(),
            ..row(tab, 3 * 3_600)
        };
        let lines = |app: &App| -> Vec<String> {
            harness_records(app)
                .into_iter()
                .map(|(_, detail)| detail[0].clone())
                .collect()
        };
        let mut app = App::headless_for_test();
        for _ in 0..4 {
            app.apply_agent_upgrades(vec![claude("s-g", "attended")]);
            app.apply_agent_upgrades(vec![claude("s-g", "box")]);
        }
        let said = lines(&app);
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[1].contains("box"), "{said:?}");
        assert_eq!(app.messages.live_rows().count(), 0, "no row");

        let mut app = App::headless_for_test();
        for _ in 0..4 {
            app.apply_agent_upgrades(vec![claude("s-h", "box")]);
            app.apply_agent_upgrades(vec![claude("s-h", "attended")]);
        }
        assert_eq!(lines(&app).len(), 1, "{:?}", lines(&app));
        for wait in ["draft", "box", "draft", "attended"] {
            app.apply_agent_upgrades(vec![claude("s-h", wait)]);
        }
        let said = lines(&app);
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[1].contains("draft"), "{said:?}");
        app.apply_agent_upgrades(Vec::new());
        app.apply_agent_upgrades(vec![claude("s-h", "box")]);
        assert_eq!(lines(&app).len(), 3, "{:?}", lines(&app));
    }

    fn two_tabs(app: &App) -> [(u64, String); 2] {
        let mut store = app.store.write().unwrap();
        let (a, b) = (
            crate::session_store::test_handle(901),
            crate::session_store::test_handle(902),
        );
        let ids = [
            (901, a.sid.as_str().to_string()),
            (902, b.sid.as_str().to_string()),
        ];
        store.register(a);
        store.register(b);
        ids
    }

    #[test]
    fn an_ended_agents_row_takes_the_owner_to_its_tab() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let unfocused = app.focused_session_id(wid).expect("the front session");
        let _ = app.split_active_stub_tab(wid);
        let second = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(second));
        let sid_of = |app: &App, local: u64| {
            if app.store.read().unwrap().by_local(local).is_none() {
                app.store
                    .write()
                    .unwrap()
                    .register(crate::session_store::test_handle(local));
            }
            let store = app.store.read().unwrap();
            store.by_local(local).unwrap().sid.as_str().to_string()
        };
        let (split, b) = (sid_of(&app, unfocused), sid_of(&app, second));
        assert_eq!(app.upgrade_place(&split), "in tab 1", "an unfocused pane");
        let ended = |tab: &str| Row {
            session: "0badf00d-1111-2222-3333-444455556666".to_string(),
            ..gone(tab)
        };
        app.apply_agent_upgrades(vec![ended(&b)]);
        let (_, one) = row_of(&app, &b).expect("b's row");
        assert!(one.title.ends_with("in tab 2"), "{}", one.title);
        assert_eq!(
            one.actions,
            [Intent::ShowSession {
                tab: 2,
                window: wid.0,
                session: b.clone(),
            }]
        );
        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![ended(&b)]);
        assert_eq!(app.messages.revision(), revision, "the same row, once");

        let moved = app
            .detach_active_tab_logical()
            .expect("the second tab moves");
        app.apply_agent_upgrades(vec![ended(&b)]);
        let (id, two) = row_of(&app, &b).expect("b's row, restated");
        let show = Intent::ShowSession {
            tab: 1,
            window: moved.0,
            session: b.clone(),
        };
        assert!(two.title.ends_with("in window 2, tab 1"), "{}", two.title);
        assert_eq!(app.upgrade_place(&split), "in window 1, tab 1");
        assert_eq!(
            two.actions,
            std::slice::from_ref(&show),
            "the place and the button move together"
        );
        assert!(
            crate::message_reporters::attention(&two).is_ok(),
            "{:?}",
            two.title
        );
        assert!(
            app.perform_intent(wid, id, show.clone()),
            "its press shows the tab"
        );

        assert!(matches!(
            app.close_window_logical(wid),
            crate::CloseOutcome::Stay
        ));
        app.apply_agent_upgrades(vec![ended(&b)]);
        let (same, alone) = row_of(&app, &b).expect("b's row, restated again");
        assert_eq!(same, id, "restated in place, never a second row");
        assert!(alone.title.ends_with("in tab 1"), "{}", alone.title);
        assert_eq!(alone.actions, [show], "the button already named the tab");

        let mut app = App::headless_for_test();
        app.apply_agent_upgrades(vec![ended("s-0000000000000000")]);
        let (_, nowhere) = row_of(&app, "s-0000000000000000").expect("its row");
        assert!(nowhere.title.ends_with("in its tab"), "{}", nowhere.title);
        assert!(nowhere.actions.is_empty(), "{:?}", nowhere.actions);
    }

    #[test]
    fn a_refused_press_on_an_interrupts_row_stands_until_its_tab_moves() {
        let (mut app, [_, _, c]) = three_tabs();
        let ended = Row {
            session: "0badf00d-1111-2222-3333-444455556666".to_string(),
            ..gone(&c)
        };
        app.apply_agent_upgrades(vec![ended.clone()]);
        let (id, stall) = row_of(&app, &c).expect("c's row");
        assert!(
            stall.actions.contains(&Intent::ShowSession {
                tab: 3,
                window: 0,
                session: c.clone()
            }),
            "{:?}",
            stall.actions
        );
        app.apply_upgrade_word(
            &WordAsk {
                tab: c.clone(),
                to: ended.to.clone(),
                word: UpgradeWord::Now,
                session: String::new(),
                press: None,
                scope: None,
            },
            Err("busy:another-sweep".to_string()),
        );
        let (same, refused) = row_of(&app, &c).expect("the refusal, on c's row");
        assert_eq!(same, id);
        assert_ne!(refused.title, stall.title, "the refusal is said on the row");
        assert_eq!(refused.actions, stall.actions, "with the row's own buttons");

        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![ended.clone()]);
        assert_eq!(
            app.messages.revision(),
            revision,
            "a look that moves nothing leaves the refusal"
        );

        let moved = app.detach_active_tab_logical().expect("c moves");
        app.apply_agent_upgrades(vec![ended]);
        let (_, there) = row_of(&app, &c).expect("c's row, restated");
        assert_ne!(there.title, refused.title);
        assert_eq!(
            there.actions,
            [Intent::ShowSession {
                tab: 1,
                window: moved.0,
                session: c.clone(),
            }]
        );
    }

    fn three_tabs() -> (App, [String; 3]) {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let first = app.focused_session_id(wid).expect("the front session");
        let mut locals = vec![first];
        for _ in 0..2 {
            let next = app.next_session_id;
            app.push_stub_tab(wid, crate::stub_session(next));
            locals.push(next);
        }
        let sids: Vec<String> = locals
            .into_iter()
            .map(|local| {
                if app.store.read().unwrap().by_local(local).is_none() {
                    app.store
                        .write()
                        .unwrap()
                        .register(crate::session_store::test_handle(local));
                }
                let store = app.store.read().unwrap();
                store.by_local(local).unwrap().sid.as_str().to_string()
            })
            .collect();
        (app, [sids[0].clone(), sids[1].clone(), sids[2].clone()])
    }

    fn upgrade_column(app: &App, local: u64) -> String {
        let store = app.store.read().unwrap();
        let h = store.by_local(local).expect("registered");
        let (_, cols) = h.ctx.timeline.lock().unwrap().owner_columns(false, now_s());
        cols.rsplit(' ').next().unwrap_or_default().to_string()
    }

    #[test]
    fn each_tab_carries_its_own_upgrade_column() {
        let mut app = App::headless_for_test();
        let [(a, sid_a), (b, _)] = two_tabs(&app);
        app.apply_agent_upgrades(vec![row(&sid_a, 60)]);
        assert_eq!(upgrade_column(&app, a), "upgrade=pending/2.1.282/queued/1m");
        assert_eq!(upgrade_column(&app, b), "upgrade=-", "not the other tab's");
        app.apply_agent_upgrades(Vec::new());
        assert_eq!(upgrade_column(&app, a), "upgrade=-", "gone with its row");
    }

    fn by_session(mut rows: Vec<Row>) -> Vec<Row> {
        rows.sort_by(|x, y| x.session.cmp(&y.session));
        rows
    }

    fn records_exactly(app: &App, title: &str) -> usize {
        harness_records(app)
            .iter()
            .filter(|(was, _)| was == title)
            .count()
    }

    fn done_title(app: &App, row: &Row) -> String {
        message_reporters::agent_upgrade_done(row, &app.upgrade_place(&row.tab)).title
    }

    fn went_through_title(app: &App, row: &Row) -> String {
        message_reporters::agent_upgrade_went_through(row, &app.upgrade_place(&row.tab))
    }

    fn asks_again_title(app: &App, row: &Row) -> String {
        message_reporters::agent_upgrade_stall_over(
            row.agent,
            "",
            &[],
            &app.upgrade_place(&row.tab),
            message_reporters::StallEnd::AsksAgain,
        )
        .title
    }

    #[test]
    fn a_tabs_upgrade_column_is_its_newest_move_not_its_first_conversation() {
        let now = now_s();
        let finished = |session: &str, tab: &str| Row {
            session: session.to_string(),
            phase: Phase::Done,
            done_at: now - 240,
            minted_at: now - 3_600,
            ..row(tab, 3_600)
        };
        for (finished_as, failed_as) in [("0aaa", "f000"), ("f000", "0aaa")] {
            let mut app = App::headless_for_test();
            let [(a, sid_a), (b, _)] = two_tabs(&app);
            let failed = Row {
                session: failed_as.to_string(),
                from: "2.1.282".to_string(),
                to: "2.1.283".to_string(),
                minted_at: now - 200,
                exited_at: now - 100,
                ..refused(&sid_a)
            };
            app.apply_agent_upgrades(by_session(vec![finished(finished_as, &sid_a), failed]));
            let column = upgrade_column(&app, a);
            assert_eq!(
                column.split('/').nth(1),
                Some("2.1.283"),
                "{finished_as} finished: {column}"
            );
            assert!(!column.starts_with("upgrade=done/"), "{column}");
            assert_eq!(
                app.upgrade_view.row_for(&sid_a).map(|r| r.session.as_str()),
                Some(failed_as)
            );
            assert_eq!(
                app.upgrade_view.stalled.get(&sid_a).map(|s| s.to.as_str()),
                Some("2.1.283"),
                "the band row names the same move"
            );
            let done = finished(finished_as, &sid_a);
            assert_eq!(
                records_exactly(&app, &done_title(&app, &done)),
                1,
                "{:?}",
                harness_records(&app)
            );
            assert_eq!(upgrade_column(&app, b), "upgrade=-");
            app.apply_agent_upgrades(vec![done.clone()]);
            assert!(!app.upgrade_view.stalled.contains_key(&sid_a));
            assert_eq!(
                records_exactly(&app, &went_through_title(&app, &done)),
                0,
                "{:?}",
                harness_records(&app)
            );
        }
        let mut app = App::headless_for_test();
        let [(a, sid_a), _] = two_tabs(&app);
        app.apply_agent_upgrades(vec![finished("0aaa", &sid_a)]);
        let column = upgrade_column(&app, a);
        assert!(column.starts_with("upgrade=done/2.1.282/"), "{column}");
    }

    #[test]
    fn a_tabs_band_row_and_column_name_the_same_move() {
        let now = now_s();
        let ended = |session: &str, to: &str, minted: u64, exited: u64, tab: &str| Row {
            session: session.to_string(),
            to: to.to_string(),
            minted_at: now - minted,
            exited_at: now - exited,
            ..refused(tab)
        };
        let named = |app: &App, local: u64, sid: &str| {
            (
                upgrade_column(app, local)
                    .split('/')
                    .nth(1)
                    .map(str::to_string),
                app.upgrade_view.stalled.get(sid).map(|s| s.to.clone()),
            )
        };
        let newest = Some("2.1.283".to_string());
        for (older_as, newer_as) in [("0aaa", "f000"), ("f000", "0aaa")] {
            let mut app = App::headless_for_test();
            let [(a, sid_a), _] = two_tabs(&app);
            app.apply_agent_upgrades(by_session(vec![
                ended(older_as, "2.1.282", 7_200, 3_000, &sid_a),
                ended(newer_as, "2.1.283", 600, 300, &sid_a),
            ]));
            assert_eq!(named(&app, a, &sid_a), (newest.clone(), newest.clone()));
        }
        let mut app = App::headless_for_test();
        let [(a, sid_a), _] = two_tabs(&app);
        app.apply_agent_upgrades(vec![ended("0aaa", "2.1.282", 7_200, 3_000, &sid_a)]);
        let older = Some("2.1.282".to_string());
        assert_eq!(named(&app, a, &sid_a), (older.clone(), older));
    }

    #[test]
    fn a_word_landing_keeps_every_conversations_row() {
        let now = now_s();
        let faulty = Row {
            session: "0aaa".to_string(),
            wait: "zzz-unclassified".to_string(),
            wait_since: now - 2 * 3_600,
            minted_at: now - 3 * 3_600,
            ..row("s-a", 3 * 3_600)
        };
        let front = Row {
            session: "f000".to_string(),
            minted_at: now - 60,
            ..row("s-a", 60)
        };
        let told = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some("harness.upfault.s-a"))
                .count()
        };
        let ask = WordAsk {
            tab: "s-a".to_string(),
            to: "2.1.282".to_string(),
            word: UpgradeWord::NotToday,
            session: String::new(),
            press: None,
            scope: None,
        };
        let kept = |app: &App| {
            app.upgrade_view
                .rows
                .iter()
                .find(|r| r.session == "0aaa")
                .map(|r| r.request.clone())
        };
        let unshown = Row {
            session: "9zzz".to_string(),
            minted_at: now - 30,
            ..row("s-a", 30)
        };
        let sessions =
            |rows: &[Row]| -> Vec<String> { rows.iter().map(|r| r.session.clone()).collect() };
        for (rows, answered) in [
            (vec![faulty.clone(), front.clone()], front.clone()),
            (vec![faulty.clone(), front.clone()], unshown.clone()),
            (vec![faulty.clone()], faulty.clone()),
        ] {
            let mut app = App::headless_for_test();
            app.apply_host_upgrades(rows.clone(), Vec::new());
            assert_eq!(told(&app), 1, "{:?}", harness_records(&app));
            app.apply_upgrade_word(
                &ask,
                Ok(Row {
                    request: Request::DeferUntil(now + NOT_TODAY_S),
                    request_at: now,
                    ..answered.clone()
                }),
            );
            assert_eq!(
                sessions(&app.upgrade_view.rows),
                sessions(&rows),
                "{}: every conversation's row is kept",
                answered.session
            );
            app.apply_host_upgrades(rows.clone(), Vec::new());
            assert_eq!(told(&app), 1, "once: {:?}", harness_records(&app));
            if answered.session == unshown.session {
                assert!(
                    app.upgrade_view
                        .rows
                        .iter()
                        .all(|r| r.request == Request::None),
                    "no row took another conversation's word"
                );
            } else if rows.len() == 2 {
                assert_eq!(
                    kept(&app),
                    Some(Request::None),
                    "the faulty conversation is kept, without another's word"
                );
                assert!(
                    matches!(
                        upgrade_view_request(&app, "s-a"),
                        Some(Request::DeferUntil(_))
                    ),
                    "the tab's row took its word"
                );
            }
        }
    }

    #[test]
    fn a_restart_in_flight_speaks_for_its_tab_and_ends_as_went_through() {
        let now = now_s();
        let namings = [Some(("0aaa", "f000")), Some(("f000", "0aaa")), None];
        for (names, put_down) in namings
            .into_iter()
            .flat_map(|names| [(names, false), (names, true)])
        {
            let mut app = App::headless_for_test();
            let [(a, sid_a), _] = two_tabs(&app);
            let (move_as, newer_as) = names.unwrap_or(("0aaa", ""));
            let moving = |phase: Phase| Row {
                session: move_as.to_string(),
                phase,
                minted_at: now - 3_600,
                ..refused(&sid_a)
            };
            let newer = Row {
                session: newer_as.to_string(),
                from: "2.1.282".to_string(),
                to: "2.1.283".to_string(),
                minted_at: now - 60,
                ..row(&sid_a, 60)
            };
            let with_newer = |r: Row| {
                if names.is_some() {
                    by_session(vec![r, newer.clone()])
                } else {
                    vec![r]
                }
            };
            let case = format!("{names:?}, put down {put_down}");
            app.apply_agent_upgrades(vec![moving(Phase::Failed("stale-exit".to_string()))]);
            let (id, _) = row_of(&app, &sid_a).expect("its row");
            if put_down {
                assert!(app.messages.dismiss(id, std::time::Instant::now()));
            }
            app.apply_agent_upgrades(with_newer(moving(Phase::Relaunched { at_s: now - 30 })));
            let column = upgrade_column(&app, a);
            assert!(
                column.starts_with("upgrade=restarting/2.1.282/")
                    || column.starts_with("upgrade=stalled/2.1.282/"),
                "{case}: {column}"
            );
            assert!(
                app.upgrade_view.stalled.contains_key(&sid_a),
                "{case}: it stands"
            );
            app.apply_agent_upgrades(with_newer(Row {
                done_at: now,
                ..moving(Phase::Done)
            }));
            let column = upgrade_column(&app, a);
            let want = if names.is_some() {
                "upgrade=pending/2.1.283/"
            } else {
                "upgrade=done/2.1.282/"
            };
            assert!(column.starts_with(want), "{case}: {column}");
            assert!(
                !app.upgrade_view.stalled.contains_key(&sid_a),
                "{case}: ended"
            );
            let done = Row {
                done_at: now,
                ..moving(Phase::Done)
            };
            assert_eq!(
                records_exactly(&app, &asks_again_title(&app, &done)),
                0,
                "{case}: {:?}",
                harness_records(&app)
            );
            assert_eq!(
                records_exactly(&app, &went_through_title(&app, &done)),
                usize::from(!put_down),
                "{case}: {:?}",
                harness_records(&app)
            );
            assert_eq!(records_exactly(&app, &done_title(&app, &done)), 1, "{case}");
        }
    }

    #[test]
    fn a_conversation_pending_beside_a_move_down_never_ends_its_row() {
        let now = now_s();
        for ((down_as, pending_as), put_down) in [("0aaa", "f000"), ("f000", "0aaa")]
            .into_iter()
            .flat_map(|names| [(names, false), (names, true)])
        {
            let case = format!("{down_as} down, put down {put_down}");
            let mut app = App::headless_for_test();
            let [(_, sid_a), _] = two_tabs(&app);
            let down = Row {
                session: down_as.to_string(),
                to: "2.1.283".to_string(),
                minted_at: now - 3_600,
                ..refused(&sid_a)
            };
            let pending = Row {
                session: pending_as.to_string(),
                from: "2.1.282".to_string(),
                to: "2.1.283".to_string(),
                minted_at: now - 60,
                ..row(&sid_a, 60)
            };
            app.apply_agent_upgrades(vec![down.clone()]);
            let (id, _) = row_of(&app, &sid_a).expect("its row");
            if put_down {
                assert!(app.messages.dismiss(id, std::time::Instant::now()));
            }
            let posted = |app: &App| app.upgrade_view.stalled.get(&sid_a).map(|p| p.id);
            app.apply_agent_upgrades(by_session(vec![down.clone(), pending.clone()]));
            assert_eq!(
                app.upgrade_view.row_for(&sid_a).map(|r| r.session.as_str()),
                Some(down_as),
                "{case}"
            );
            assert_eq!(posted(&app), Some(id), "{case}: the same entry");
            app.apply_agent_upgrades(vec![down.clone()]);
            assert_eq!(posted(&app), Some(id), "{case}: the same entry");
            assert_eq!(
                row_of(&app, &sid_a).map(|(live, _)| live),
                (!put_down).then_some(id),
                "{case}: never raised again"
            );
            assert_eq!(
                records_exactly(&app, &asks_again_title(&app, &down)),
                0,
                "{case}: {:?}",
                harness_records(&app)
            );
        }
        let mut app = App::headless_for_test();
        let [(_, sid_a), _] = two_tabs(&app);
        let down = Row {
            session: "0aaa".to_string(),
            to: "2.1.283".to_string(),
            ..refused(&sid_a)
        };
        app.apply_agent_upgrades(vec![down.clone()]);
        let (id, _) = row_of(&app, &sid_a).expect("its row");
        assert!(app.messages.dismiss(id, std::time::Instant::now()));
        app.apply_agent_upgrades(vec![Row {
            session: "f000".to_string(),
            from: "2.1.282".to_string(),
            to: "2.1.283".to_string(),
            ..row(&sid_a, 60)
        }]);
        assert_eq!(
            records_exactly(&app, &asks_again_title(&app, &down)),
            1,
            "{:?}",
            harness_records(&app)
        );
    }

    #[test]
    fn a_pressed_word_names_the_conversation_its_tab_showed() {
        use aterm_messages::UpgradeWord;
        let now = now_s();
        for (down_as, pending_as) in [("0aaa", "f000"), ("f000", "0aaa")] {
            let mut app = App::headless_for_test();
            let (writer, asked) = recording(|_| Err("busy:another-sweep".to_string()));
            app.upgrade_view.writer = writer;
            app.apply_agent_upgrades(by_session(vec![
                Row {
                    session: down_as.to_string(),
                    to: "2.1.283".to_string(),
                    minted_at: now - 3_600,
                    ..refused("s-a")
                },
                Row {
                    session: pending_as.to_string(),
                    from: "2.1.282".to_string(),
                    to: "2.1.283".to_string(),
                    minted_at: now - 60,
                    ..row("s-a", 60)
                },
            ]));
            let press = |app: &mut App, to: &str| {
                assert!(app.send_upgrade_word(WordAsk::new(
                    "s-a".to_string(),
                    to.to_string(),
                    UpgradeWord::NotToday,
                )));
                asked.try_recv().expect("written").session
            };
            assert_eq!(press(&mut app, "2.1.283"), down_as);
            assert_eq!(press(&mut app, "2.1.282"), "", "NEGATIVE CONTROL");
        }
    }

    #[test]
    fn a_frozen_tab_running_an_agent_is_never_counted_for_the_hook() {
        let mut app = App::headless_for_test();
        let [(a, sid_a), (b, _)] = two_tabs(&app);
        {
            let mut store = app.store.write().unwrap();
            store.mark_frozen_path(a);
            store.mark_frozen_path(b);
        }
        let front = |app: &App, local: u64, program: &str| {
            let store = app.store.read().unwrap();
            let mut tl = store.by_local(local).unwrap().ctx.timeline.lock().unwrap();
            tl.note_foreground_group(i32::try_from(local).unwrap());
            tl.set_program(i32::try_from(local).unwrap(), Some(program.to_string()));
        };
        front(&app, a, "zsh");
        front(&app, b, "zsh");
        assert_eq!(
            app.tabs_behind(),
            TabsBehind {
                frozen_shells: 2,
                ..TabsBehind::default()
            },
            "two shells: the hook, twice"
        );
        for program in atpkg::stub::AGENT_PROGRAMS
            .iter()
            .copied()
            .chain(["claude-code"])
        {
            front(&app, a, program);
            assert_eq!(
                app.tabs_behind(),
                TabsBehind {
                    frozen_shells: 1,
                    frozen_agents: 1,
                    ..TabsBehind::default()
                },
                "{program} takes prompts, never the shell's hook command"
            );
        }
        front(&app, a, "node");
        assert_eq!(
            app.tabs_behind(),
            TabsBehind {
                frozen_shells: 2,
                ..TabsBehind::default()
            },
            "a node whose screen named no agent"
        );
        {
            let store = app.store.read().unwrap();
            let mut tl = store.by_local(a).unwrap().ctx.timeline.lock().unwrap();
            tl.set_agent_reader_for_test(true);
        }
        assert_eq!(
            app.tabs_behind(),
            TabsBehind {
                frozen_shells: 1,
                frozen_agents: 1,
                ..TabsBehind::default()
            },
            "a node-hosted Claude Code takes prompts, never the shell's hook command"
        );
        front(&app, a, "claude");
        let text = "claude 2.1.282 (Anthropic latest)";
        app.upgrade_view.note_managed_text(text);
        app.apply_agent_upgrades(vec![row(&sid_a, 60)]);
        assert_eq!(
            app.tabs_behind(),
            TabsBehind {
                frozen_shells: 1,
                agents_behind: PerAgent::of(&[(Agent::Claude, 1)]),
                ..TabsBehind::default()
            },
            "the move heals it: counted as moving, not as a tab to type into"
        );
    }

    #[test]
    fn a_node_hosted_agent_known_by_its_reader_keeps_its_prompt_out_of_the_shell_hook_advice() {
        use aterm_phase::Program;
        let rows = aterm_phase::prompt::fixtures::screen(aterm_phase::muse::fixtures::IDLE);
        let muse = aterm_phase::identify(Some("node"), &rows).program();
        assert_eq!(muse, Program::Muse, "the measured runtime-hosted composer");
        for (program, reader, owns_prompt) in [
            ("node", None, false),
            ("zsh", Some(muse), false),
            ("node", Some(muse), true),
        ] {
            let mut app = App::headless_for_test();
            app.session_factory.shell_override = Some("/bin/zsh".into());
            let [(id, _), _] = two_tabs(&app);
            let ctx = {
                let mut store = app.store.write().unwrap();
                store.mark_frozen_path(id);
                store.by_local(id).unwrap().ctx.clone()
            };
            {
                let mut timeline = ctx.timeline.lock().unwrap();
                timeline.note_foreground_group(42);
                timeline.set_program(42, Some("node".into()));
                timeline.publish_agent(
                    "idle",
                    None,
                    None,
                    reader,
                    crate::session_timeline::AgentStamp {
                        generation: crate::control::ScreenGen { epoch: 0, seq: 1 },
                        fp: 1,
                    },
                );
                if program != "node" {
                    timeline.set_program(42, Some(program.into()));
                    assert_eq!(timeline.agent().reader, Some(muse), "the stale reader");
                }
                let identified = crate::harness_host::identified_agent(timeline.agent());
                assert_eq!(
                    identified,
                    owns_prompt.then_some(muse),
                    "{program}, reader={reader:?}: the prompt's owner"
                );
                assert_eq!(
                    crate::harness_host::agent_of(timeline.agent()),
                    identified.filter(|p| p.harness_table()),
                    "{program}, reader={reader:?}: supervision is a filter on the owner"
                );
            }
            let expected = if owns_prompt {
                TabsBehind {
                    frozen_agents: 1,
                    ..TabsBehind::default()
                }
            } else {
                TabsBehind {
                    frozen_shells: 1,
                    ..TabsBehind::default()
                }
            };
            assert_eq!(app.tabs_behind(), expected, "{program}, reader={reader:?}");
            app.post_managed_current("muse 1.4.9-R6297.1 (latest)");
            let advice = app
                .messages
                .log()
                .records()
                .rfind(|r| r.key.as_deref() == Some(crate::toolchain_words::KEY_MANAGED))
                .map(|r| r.detail.join(" "))
                .expect("the installed-agents record");
            let hook = app.hook_dialect().remedy().expect("the selected zsh hook");
            assert!(advice.contains(&hook), "the actual hook advice: {advice}");
            assert_eq!(
                advice.contains("once the agent exits"),
                owns_prompt,
                "{program}, reader={reader:?}: {advice}"
            );
        }
    }

    #[test]
    fn only_a_session_running_an_older_build_that_moves_on_its_own_is_said_to_move() {
        let mut app = App::headless_for_test();
        let text = "claude 2.1.282 (Anthropic latest)";
        app.upgrade_view.note_managed_text(text);
        let native = Row {
            source: "native".to_string(),
            ..row("s-native", 60)
        };
        let other_build = Row {
            to: "2.1.283".to_string(),
            ..row("s-other", 60)
        };
        let on_it = Row {
            from: "2.1.282".to_string(),
            to: "2.1.283".to_string(),
            ..row("s-on-it", 60)
        };
        let gave_up = Row {
            phase: Phase::Failed("unanswered".to_string()),
            ..row("s-gave-up", 60)
        };
        let skipped = Row {
            request: Request::Skip("2.1.282".to_string()),
            ..row("s-skipped", 60)
        };
        app.apply_agent_upgrades(vec![
            row("s-healthy", 60),
            row("s-overdue", 7 * 3_600),
            gave_up,
            skipped,
            native,
            other_build,
            on_it,
        ]);
        assert_eq!(
            app.tabs_behind(),
            TabsBehind {
                agents_behind: PerAgent::of(&[(Agent::Claude, 3)]),
                agents_held: PerAgent::of(&[(Agent::Claude, 3)]),
                ..TabsBehind::default()
            },
            "healthy, native and 2.1.283-bound move; overdue, gave-up, skipped stay; one \
             already on 2.1.282 is not behind"
        );
        let mut fresh = App::headless_for_test();
        fresh.apply_agent_upgrades(vec![row("s-healthy", 60)]);
        assert_eq!(fresh.tabs_behind(), TabsBehind::default());
    }

    #[test]
    fn a_codex_session_behind_the_managed_codex_is_counted_and_named() {
        let mut app = App::headless_for_test();
        let text = "claude 2.1.282 (Anthropic latest); codex 0.157.0 (OpenAI latest)";
        app.upgrade_view.note_managed_text(text);
        let codex = |tab: &str, from: &str, behind_for: u64| Row {
            agent: Agent::Codex,
            from: from.to_string(),
            to: "0.157.0".to_string(),
            ..row(tab, behind_for)
        };
        app.apply_agent_upgrades(vec![
            row("s-claude", 60),
            codex("s-codex", "0.156.0", 60),
            codex("s-codex-overdue", "0.156.0", 7 * 3_600),
            Row {
                to: "0.158.0".to_string(),
                ..codex("s-codex-on-it", "0.157.0", 60)
            },
        ]);
        assert_eq!(
            app.tabs_behind(),
            TabsBehind {
                agents_behind: PerAgent::of(&[(Agent::Claude, 1), (Agent::Codex, 1)]),
                agents_held: PerAgent::of(&[(Agent::Codex, 1)]),
                ..TabsBehind::default()
            },
            "each agent against its own build; one already on 0.157.0 is not behind"
        );
        assert!(!app.post_managed_current(text), "re-derived by the rows");
        let record = |app: &App| {
            app.messages
                .log()
                .records()
                .rfind(|r| r.key.as_deref() == Some(crate::toolchain_words::KEY_MANAGED))
                .map(|r| (r.title.clone(), r.detail.join(" ")))
                .expect("the record")
        };
        let (title, detail) = record(&app);
        assert_eq!(title, "Claude Code 2.1.282 and Codex CLI 0.157.0 installed");
        assert!(
            detail.contains("1 running Claude Code session moves onto it on its own"),
            "{detail}"
        );
        assert!(
            detail.contains("1 running Codex session moves onto it on its own"),
            "{detail}"
        );
        assert!(
            detail.contains("1 running Codex session is still on the old build"),
            "{detail}"
        );
        let mut alone = App::headless_for_test();
        alone.upgrade_view.note_managed_text(text);
        alone.apply_agent_upgrades(vec![codex("s-codex", "0.156.0", 60)]);
        assert!(!alone.post_managed_current(text), "re-derived by the rows");
        let (title, detail) = record(&alone);
        assert_eq!(title, "Claude Code 2.1.282 and Codex CLI 0.157.0 installed");
        assert!(
            detail.contains("1 running Codex session moves onto it on its own")
                && !detail.contains("Claude Code session"),
            "{detail}"
        );
    }

    #[test]
    fn the_install_is_not_current_while_a_session_runs_an_older_build() {
        let current = |rows: Vec<Row>| {
            let mut app = App::headless_for_test();
            app.apply_agent_upgrades(rows);
            assert!(app.post_managed_current("claude 2.1.287 (Anthropic latest)"));
            app.messages
                .log()
                .records()
                .rfind(|r| r.key.as_deref() == Some(crate::toolchain_words::KEY_MANAGED))
                .map(|r| r.title.ends_with(" is current"))
                .expect("the record")
        };
        let to_previous = Row {
            from: "2.1.285".to_string(),
            to: "2.1.286".to_string(),
            ..row("s-a", 60)
        };
        let done_onto_previous = Row {
            phase: Phase::Done,
            done_at: now_s() - 5,
            from: "2.1.285".to_string(),
            to: "2.1.286".to_string(),
            ..row("s-b", 600)
        };
        assert!(
            !current(vec![to_previous]),
            "a row onto the previous build still runs an older one"
        );
        assert!(
            !current(vec![done_onto_previous]),
            "a finished move onto the previous build runs it"
        );
        let relaunched = |tab: &str, to: &str, ago: u64| Row {
            phase: Phase::Relaunched {
                at_s: now_s() - ago,
            },
            from: "2.1.285".to_string(),
            to: to.to_string(),
            ..row(tab, ago + 600)
        };
        assert!(
            !current(vec![relaunched("s-e", "2.1.286", 30)]),
            "relaunched onto the previous build, it runs that one"
        );
        let stuck_onto_it = relaunched("s-f", "2.1.287", 7 * 86_400);
        assert_eq!(
            stuck_onto_it.stall(now_s()).as_deref(),
            Some("stuck:relaunched"),
            "the reviewer's row: stuck past STALE_S"
        );
        assert!(
            current(vec![relaunched("s-g", "2.1.287", 30), stuck_onto_it]),
            "relaunched onto the installed build, fresh or stuck, it runs it"
        );
        let past_it = Row {
            from: "2.1.287".to_string(),
            to: "2.1.288".to_string(),
            ..row("s-c", 60)
        };
        let done_onto_it = Row {
            phase: Phase::Done,
            done_at: now_s() - 5,
            from: "2.1.286".to_string(),
            to: "2.1.287".to_string(),
            ..row("s-d", 600)
        };
        assert!(current(vec![past_it, done_onto_it]));
    }

    #[test]
    fn one_install_is_one_record_whose_counts_move_in_place() {
        let mut app = App::headless_for_test();
        let managed = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(crate::toolchain_words::KEY_MANAGED))
                .map(|r| r.title.ends_with(" is current"))
                .collect::<Vec<_>>()
        };
        let moving = |to: &str| Row {
            from: "2.1.286".to_string(),
            to: to.to_string(),
            ..row("s-a", 60)
        };
        app.apply_agent_upgrades(vec![moving("2.1.286")]);
        let text = "claude 2.1.287 (Anthropic latest)";
        assert!(app.post_managed_current(text));
        app.apply_agent_upgrades(vec![moving("2.1.287")]);
        assert!(!app.post_managed_current(text), "the same words");
        app.apply_agent_upgrades(vec![Row {
            phase: Phase::Done,
            done_at: now_s() - 5,
            ..moving("2.1.287")
        }]);
        assert_eq!(managed(&app), [true], "one record, re-worded in place");
        assert!(app.post_managed_current("claude 2.1.288 (Anthropic latest)"));
        assert_eq!(managed(&app).len(), 2, "a new version is a new record");
    }

    #[test]
    fn a_managed_record_gone_from_the_ring_is_written_again() {
        let mut app = App::headless_for_test();
        let managed = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(crate::toolchain_words::KEY_MANAGED))
                .map(|r| r.title.ends_with(" is current"))
                .collect::<Vec<_>>()
        };
        let moving = Row {
            from: "2.1.286".to_string(),
            to: "2.1.287".to_string(),
            ..row("s-a", 60)
        };
        app.apply_agent_upgrades(vec![moving.clone()]);
        let text = "claude 2.1.287 (Anthropic latest)";
        assert!(app.post_managed_current(text));
        for n in 0..aterm_messages::LOG_CAP {
            app.record_message(
                aterm_messages::Message::new(
                    aterm_messages::tags::CONFIG,
                    aterm_messages::Severity::Info,
                    format!("other news {n}"),
                )
                .record(),
            );
        }
        assert!(managed(&app).is_empty(), "evicted by the ring");
        app.apply_agent_upgrades(vec![Row {
            phase: Phase::Done,
            done_at: now_s() - 5,
            ..moving.clone()
        }]);
        assert_eq!(managed(&app), [true], "a new record");
        assert!(!app.post_managed_current(text), "the same words: nothing");
        app.apply_agent_upgrades(vec![moving]);
        assert_eq!(
            managed(&app),
            [false],
            "the new record is the one re-worded"
        );
    }

    #[test]
    fn a_tab_measured_frozen_is_shadowed_not_from_before_this_update() {
        use crate::session_status::program::PathVerdict;
        let app = App::headless_for_test();
        let [(a, _), _] = two_tabs(&app);
        let job = |app: &App, pgid: i32| {
            let store = app.store.read().unwrap();
            let mut tl = store.by_local(a).unwrap().ctx.timeline.lock().unwrap();
            tl.note_foreground_group(pgid);
            tl.set_program(pgid, Some("make".to_string()));
            tl.set_leader(pgid, None, Some(PathVerdict::Frozen));
        };
        job(&app, 71);
        assert_eq!(app.tabs_behind(), TabsBehind::default(), "one job: nothing");
        job(&app, 72);
        assert_eq!(
            app.tabs_behind(),
            TabsBehind {
                shadowed: 1,
                ..TabsBehind::default()
            }
        );
        assert!(!app.store.read().unwrap().has_frozen_path(a), "no mark");
    }

    #[test]
    fn a_move_finished_days_later_names_when_it_happened() {
        use crate::message_reporters::moved_line;
        let at = 1_758_549_840_u64 + 365 * 86_400;
        let said: Vec<String> = [3 * 3_600, 86_400, 7 * 86_400]
            .into_iter()
            .map(|later| moved_line(at, at + later, Some(0)).expect("a move hours on"))
            .collect();
        assert!(said.iter().all(|l| l.ends_with("2:04 PM")), "{said:?}");
        assert!(said[0] != said[1] && said[1] != said[2], "{said:?}");
        assert_eq!(
            moved_line(at, at + 5 * 60, Some(0)),
            None,
            "minutes: the record's own"
        );
        assert_eq!(
            moved_line(0, at, Some(0)),
            None,
            "an older record: no instant"
        );
        assert_eq!(
            moved_line(at, at + 7 * 86_400, None),
            None,
            "no clock, no time"
        );
        let mut app = App::headless_for_test();
        let now = now_s();
        let done = Row {
            phase: Phase::Done,
            done_at: now - 5,
            moved_at: now - 7 * 86_400,
            outcome: "claude restarted on 2.1.287 \u{b7} model claude-opus-5-5".to_string(),
            to: "2.1.287".to_string(),
            ..row("s-a", 8 * 86_400)
        };
        app.apply_agent_upgrades(vec![done]);
        let records = records_titled(&app, "moved onto");
        assert_eq!(records.len(), 1, "{records:?}");
        let detail = &records[0].1;
        match crate::presence::local_offset_s() {
            Some(_) => assert!(detail[1].starts_with("moved "), "{detail:?}"),
            None => assert_eq!(detail.len(), 2, "{detail:?}"),
        }
        assert_eq!(
            detail.last().map(String::as_str),
            Some("model claude-opus-5-5")
        );
        let mut app = App::headless_for_test();
        let prompt = Row {
            phase: Phase::Done,
            done_at: now - 5,
            moved_at: now - 125,
            outcome: "claude restarted on 2.1.287 \u{b7} model claude-opus-5-5".to_string(),
            to: "2.1.287".to_string(),
            ..row("s-a", 600)
        };
        app.apply_agent_upgrades(vec![prompt.clone()]);
        app.apply_agent_upgrades(vec![prompt]);
        let records = records_titled(&app, "moved onto");
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(
            records[0].1.len(),
            2,
            "no moved-at line: {:?}",
            records[0].1
        );
        assert_eq!(app.messages.live_rows().count(), 0, "a record, never a row");
    }

    fn recording(
        answer: impl Fn(&WordAsk) -> Result<Row, String> + Send + Sync + 'static,
    ) -> (WordWriter, std::sync::mpsc::Receiver<WordAsk>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = WordWriter::new(move |ask| {
            let _ = tx.send(ask.clone());
            answer(ask)
        });
        (writer, rx)
    }

    fn record_id_of(app: &App, tab: &str) -> Option<MessageId> {
        let key = format!("harness.upgrade.{tab}");
        app.messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(key.as_str()))
            .map(|r| r.id)
    }

    fn row_of(app: &App, tab: &str) -> Option<(MessageId, aterm_messages::Message)> {
        let key = format!("harness.upgrade.{tab}");
        app.messages
            .live_rows()
            .find(|l| l.msg.key.as_deref() == Some(key.as_str()))
            .map(|l| (l.id, l.msg.clone()))
    }

    #[test]
    fn a_stop_that_repeats_is_one_record_across_its_rearms_and_ends_at_done() {
        let mut app = App::headless_for_test();
        let now = now_s();
        let at = |phase: Phase, streak: u32, last_stop: &str| Row {
            phase,
            stop_streak: streak,
            streak_why: "no-resume".to_string(),
            last_stop: last_stop.to_string(),
            retry_at: now + 3_600,
            wait: String::new(),
            ..row("s-a", 60)
        };
        let failed = || Phase::Failed("no-resume".to_string());
        let announced = Phase::Announced {
            at_s: now - 60,
            asks: 1,
        };

        app.apply_agent_upgrades(vec![at(failed(), 1, "")]);
        assert!(row_of(&app, "s-a").is_none(), "a first stop is no row");
        assert!(
            record_id_of(&app, "s-a").is_some(),
            "a first stop is a record"
        );
        app.apply_agent_upgrades(vec![at(Phase::Pending, 1, "no-resume")]);
        app.apply_agent_upgrades(vec![at(announced.clone(), 1, "")]);
        assert!(row_of(&app, "s-a").is_none());

        app.apply_agent_upgrades(vec![at(failed(), 2, "")]);
        assert!(row_of(&app, "s-a").is_none(), "a repeating stop is no row");
        let id = record_id_of(&app, "s-a").expect("its record");
        for (phase, streak, last) in [
            (Phase::Pending, 2, "no-resume"),
            (announced, 2, ""),
            (Phase::Exiting { at_s: now }, 2, ""),
            (Phase::Relaunched { at_s: now }, 2, ""),
            (failed(), 3, ""),
            (Phase::Pending, 3, "no-resume"),
        ] {
            let word = phase.word();
            app.apply_agent_upgrades(vec![at(phase, streak, last)]);
            assert_eq!(app.messages.live_rows().count(), 0, "{word}: no row");
            assert_eq!(
                record_id_of(&app, "s-a"),
                Some(id),
                "{word}: the one record"
            );
        }
        app.apply_agent_upgrades(vec![Row {
            done_at: now,
            ..at(Phase::Done, 3, "")
        }]);
        assert!(row_of(&app, "s-a").is_none());
        assert!(
            app.messages
                .log()
                .records()
                .any(|r| r.tag == aterm_messages::tags::HARNESS
                    && r.severity == aterm_messages::Severity::Success),
            "{:?}",
            harness_records(&app)
        );
    }

    #[test]
    fn a_restart_that_sticks_keeps_one_row_while_stuck_and_resolves_once_it_moves() {
        let mut app = App::headless_for_test();
        let now = now_s();
        let hung = now - upgrade_drive::HUNG_EXIT_AFTER_S - 60;
        let at = |phase: Phase| Row {
            phase,
            wait: String::new(),
            ..row("s-a", 60)
        };
        app.apply_agent_upgrades(vec![at(Phase::Exiting { at_s: now - 400 })]);
        assert!(row_of(&app, "s-a").is_none(), "within the bound, no row");
        app.apply_agent_upgrades(vec![at(Phase::Exiting { at_s: hung })]);
        let (id, first) = row_of(&app, "s-a").expect("a shutdown that hung is a row");
        app.apply_agent_upgrades(vec![Row {
            exited_at: now - aterm_agent::harness::relaunch::STALE_S - 30,
            ..at(Phase::Exiting { at_s: hung })
        }]);
        let (again, exited) = row_of(&app, "s-a").expect("still stuck");
        assert_eq!(again, id, "one row while it is stuck");
        assert_ne!(
            exited.detail, first.detail,
            "restated with what it waits on"
        );
        app.apply_agent_upgrades(vec![at(Phase::Relaunched { at_s: now })]);
        assert!(
            app.messages.live_rows().next().is_none(),
            "it moved: no row says it has not"
        );
    }

    #[test]
    fn a_stuck_row_put_down_ends_on_the_record_as_moving_again_never_asking() {
        let mut app = App::headless_for_test();
        let now = now_s();
        let (a, b) = ("s-a", "s-b");
        let at = |tab: &str, phase: Phase| Row {
            phase,
            wait: String::new(),
            ..row(tab, 60)
        };
        let recorded = |app: &App, tab: &str| -> Vec<MessageId> {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(&*format!("harness.upgrade.{tab}")))
                .filter(|r| r.retired() == Some(&aterm_messages::Retired::Recorded))
                .map(|r| r.id)
                .collect()
        };
        let stuck = |tab| {
            at(
                tab,
                Phase::Exiting {
                    at_s: now - upgrade_drive::HUNG_EXIT_AFTER_S - 60,
                },
            )
        };
        app.apply_agent_upgrades(vec![stuck(a), stuck(b)]);
        let (id, _) = row_of(&app, a).expect("a restart that sticks is a row");
        assert!(row_of(&app, b).is_some(), "s-b's row too");
        assert!(app.messages.dismiss(id, std::time::Instant::now()));
        app.sync_messages();

        let moving = |tab| at(tab, Phase::Relaunched { at_s: now });
        app.apply_agent_upgrades(vec![moving(a), moving(b)]);
        assert!(
            app.messages.live_rows().next().is_none(),
            "both moved: no row says they have not"
        );
        let ended = recorded(&app, a);
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert!(
            recorded(&app, b).is_empty(),
            "a row left up is resolved, never recorded over"
        );

        let done = |tab| Row {
            done_at: now,
            ..at(tab, Phase::Done)
        };
        app.apply_agent_upgrades(vec![done(a), done(b)]);
        assert_eq!(recorded(&app, a), ended, "Done adds nothing under the key");
    }

    fn press_record(app: &mut App, id: MessageId, action: u8) {
        let outcome = app.perform_message_act(crate::WindowId(0), id.raw(), action);
        assert!(
            matches!(
                outcome,
                crate::native_app::MessageActOutcome::Performed { .. }
            ),
            "{outcome:?}"
        );
    }

    fn upgrade_view_request(app: &App, tab: &str) -> Option<Request> {
        app.upgrade_view.row_for(tab).map(|r| r.request.clone())
    }

    fn record_of(app: &App, id: MessageId) -> (String, aterm_messages::Severity, Vec<String>) {
        let rec = app.messages.log().get(id).expect("on record");
        (rec.title.clone(), rec.severity, rec.detail.clone())
    }

    #[test]
    fn an_overdue_tabs_menu_follows_what_moves_it_now() {
        use aterm_messages::UpgradeWord::{NotToday, Now, Skip};
        let mut app = App::headless_for_test();
        app.apply_agent_upgrades(vec![row("s-a", 7 * 3_600)]);
        assert!(row_of(&app, "s-a").is_none(), "a record, never a row");
        assert_eq!(app.messages.live_rows().count(), 0);
        assert_eq!(
            app.upgrade_view.menu_for("s-a", now_s()).map(|m| m.words),
            Some(vec![Now, NotToday, Skip]),
            "at work: `Upgrade now` moves it"
        );

        let waits = Row {
            wait: "awaiting-ready".to_string(),
            ..row("s-a", 7 * 3_600)
        };
        app.apply_agent_upgrades(vec![waits.clone()]);
        assert_eq!(app.messages.live_rows().count(), 0);
        assert_eq!(
            app.upgrade_view.menu_for("s-a", now_s()).map(|m| m.words),
            Some(vec![NotToday, Skip]),
            "on the READY answer `--now` moves nothing"
        );

        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![waits]);
        assert_eq!(app.messages.revision(), revision, "nothing to restate");
    }

    #[test]
    fn a_stall_that_turns_into_an_ended_agent_takes_a_row_and_one_asking_on_its_own_leaves_the_glass()
     {
        let mut app = App::headless_for_test();
        let now = now_s();
        let asking = Row {
            phase: Phase::Announced {
                at_s: now - 600,
                asks: 1,
            },
            wait: "background".to_string(),
            ..row("s-a", 7 * 3_600)
        };
        assert!(asking.asks_on_its_own(now));
        app.apply_agent_upgrades(vec![asking.clone()]);
        assert!(row_of(&app, "s-a").is_none(), "asking on its own: a record");
        assert_eq!(app.messages.live_rows().count(), 0);
        let record = entries_of_s_a(&app)[0].id;
        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![asking.clone()]);
        assert_eq!(app.messages.revision(), revision, "the same record, once");

        let ended = gone("s-a");
        app.apply_agent_upgrades(vec![ended.clone()]);
        let (id, msg) = row_of(&app, "s-a").expect("an ended agent is a row");
        assert_eq!(msg.hold, aterm_messages::Hold::STANDING);
        assert_eq!(msg.severity, aterm_messages::Severity::Warn);
        assert!(msg.actions.is_empty(), "no snooze: {:?}", msg.actions);
        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![ended]);
        assert_eq!(app.messages.revision(), revision, "the same row, once");

        assert_eq!(
            app.messages.log().get(record).and_then(|r| r.retired()),
            Some(&aterm_messages::Retired::Recorded),
            "the wait's record stays as history"
        );

        app.apply_agent_upgrades(vec![asking]);
        assert!(row_of(&app, "s-a").is_none(), "back to a record");
        assert_eq!(app.messages.live_rows().count(), 0);
        let rec = app.messages.log().get(id).expect("the row's entry");
        assert_eq!(
            rec.retired(),
            Some(&aterm_messages::Retired::Withdrawn),
            "withdrawn, never resolved as fixed"
        );
        assert_eq!(rec.severity, aterm_messages::Severity::Warn, "never a ✓");
        let waiting: Vec<_> = entries_of_s_a(&app)
            .into_iter()
            .filter(|r| r.retired() == Some(&aterm_messages::Retired::Recorded))
            .collect();
        assert_eq!(waiting.len(), 2, "the wait's record, then its record again");
    }

    #[test]
    fn an_equal_second_old_batch_cannot_undo_a_later_accepted_word() {
        const N: u64 = 1_790_001_234;
        let mut app = App::headless_for_test();
        let local = app.focused_session_id(crate::WindowId(0)).unwrap();
        let tab = app.session_sid(local).unwrap();
        crate::claude_footer::probe::drawer(&app, local, 4343, "claude", "2.1.281");
        let ctx = app.pool.get(local).unwrap().ctx.clone();
        ctx.timeline
            .lock()
            .unwrap()
            .set_program(4343, Some("claude".into()));
        let baseline = row(&tab, 60);
        app.apply_agent_upgrades(vec![baseline.clone()]);
        let now = Row {
            request: Request::Now,
            request_at: N,
            ..baseline.clone()
        };
        app.apply_upgrade_word(
            &WordAsk::new(tab.clone(), now.to.clone(), UpgradeWord::Now),
            Ok(now.clone()),
        );
        let state = |app: &App| {
            let timeline = ctx.timeline.lock().unwrap();
            (
                app.upgrade_view.row_for(&tab).unwrap().request.clone(),
                timeline.upgrade().unwrap().request.clone(),
            )
        };
        assert_eq!(state(&app), (Request::Now, Request::Now));
        // The host's read starts before the next accepted word, within the same recorded second.
        let before_latest_word = UpgradeSources::capture(&app.store);
        let defer = Request::DeferUntil(N + NOT_TODAY_S);
        app.apply_upgrade_word(
            &WordAsk::new(tab.clone(), now.to.clone(), UpgradeWord::NotToday),
            Ok(Row {
                request: defer.clone(),
                request_at: N,
                ..baseline.clone()
            }),
        );
        assert_eq!(state(&app), (defer.clone(), defer.clone()));
        app.apply_host_upgrades_from(vec![now], Vec::new(), before_latest_word);
        assert_eq!(
            state(&app),
            (defer.clone(), defer),
            "an older read cannot roll back the later accepted word just because the seconds tie"
        );

        let now = Row {
            request: Request::Now,
            request_at: N,
            ..baseline.clone()
        };
        let ask_now = WordAsk::new(tab.clone(), now.to.clone(), UpgradeWord::Now);
        app.apply_upgrade_word(&ask_now, Ok(now.clone()));
        let before_round_trip = UpgradeSources::capture(&app.store);
        let old_defer = Row {
            request: Request::DeferUntil(N + NOT_TODAY_S),
            request_at: N,
            ..baseline.clone()
        };
        app.apply_upgrade_word(
            &WordAsk::new(tab.clone(), now.to.clone(), UpgradeWord::NotToday),
            Ok(old_defer.clone()),
        );
        // That read can obtain the deferred row before the person accepts Now again.
        app.apply_upgrade_word(&ask_now, Ok(now.clone()));
        assert_eq!(state(&app), (Request::Now, Request::Now));
        app.apply_host_upgrades_from(vec![old_defer], Vec::new(), before_round_trip);
        assert_eq!(
            state(&app),
            (Request::Now, Request::Now),
            "returning to the same word in one second cannot make an earlier read current"
        );

        let before_same_word = UpgradeSources::capture(&app.store);
        app.apply_upgrade_word(&ask_now, Ok(now));
        app.apply_host_upgrades_from(
            vec![Row {
                request: Request::DeferUntil(N + NOT_TODAY_S),
                request_at: N,
                ..baseline.clone()
            }],
            Vec::new(),
            before_same_word,
        );
        assert_eq!(
            state(&app),
            (Request::Now, Request::Now),
            "an accepted same-valued reply also ends the earlier read's authority"
        );

        // A read begun after that acceptance is current, even when its different choice has N too.
        let current = UpgradeSources::capture(&app.store);
        let skip = Request::Skip(baseline.to.clone());
        app.apply_host_upgrades_from(
            vec![Row {
                request: skip.clone(),
                request_at: N,
                ..baseline
            }],
            Vec::new(),
            current,
        );
        assert_eq!(
            state(&app),
            (skip.clone(), skip),
            "a genuinely current external choice at the same second remains authoritative"
        );
    }

    #[test]
    fn a_words_answer_never_brings_back_a_row_the_host_dropped() {
        let mut app = App::headless_for_test();
        let deferred = |tab: &str| Row {
            request: Request::DeferUntil(now_s() + NOT_TODAY_S),
            request_at: now_s(),
            ..row(tab, 60)
        };
        let ask = |tab: &str| WordAsk {
            tab: tab.to_string(),
            to: "2.1.282".to_string(),
            word: UpgradeWord::NotToday,
            session: String::new(),
            press: None,
            scope: None,
        };
        app.apply_agent_upgrades(vec![row("s-a", 60), row("s-b", 60)]);
        app.apply_agent_upgrades(vec![row("s-b", 60)]);
        app.apply_upgrade_word(&ask("s-a"), Ok(deferred("s-a")));
        assert!(
            app.upgrade_view.row_for("s-a").is_none(),
            "not brought back"
        );
        app.apply_upgrade_word(&ask("s-b"), Ok(deferred("s-b")));
        assert!(
            matches!(
                upgrade_view_request(&app, "s-b"),
                Some(Request::DeferUntil(_))
            ),
            "the answer's row took s-b's place"
        );
    }

    #[test]
    fn the_owners_words_are_written_one_at_a_time_in_the_order_pressed() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::mpsc;
        let (release, held) = mpsc::channel::<()>();
        let held = std::sync::Mutex::new(held);
        let inside = std::sync::Arc::new(AtomicUsize::new(0));
        let overlapped = std::sync::Arc::new(AtomicBool::new(false));
        let (i, o) = (inside.clone(), overlapped.clone());
        let writer = WordWriter::new(move |ask: &WordAsk| {
            if i.fetch_add(1, Ordering::SeqCst) != 0 {
                o.store(true, Ordering::SeqCst);
            }
            if ask.word == UpgradeWord::Now {
                let _ = held.lock().unwrap_or_else(|p| p.into_inner()).recv();
            }
            i.fetch_sub(1, Ordering::SeqCst);
            Err("written".to_string())
        });
        let (answered, answers) = mpsc::channel::<UpgradeWord>();
        let deliver = |to: mpsc::Sender<UpgradeWord>| {
            move |ask: WordAsk, _: Result<Row, String>| {
                let _ = to.send(ask.word);
            }
        };
        let ask = |word| WordAsk {
            tab: "s-a".to_string(),
            to: "2.1.282".to_string(),
            word,
            session: String::new(),
            press: None,
            scope: None,
        };
        let mut queue = WordQueue::default();
        queue
            .send(ask(UpgradeWord::Now), &writer, deliver(answered.clone()))
            .expect("sent");
        queue
            .send(ask(UpgradeWord::NotToday), &writer, deliver(answered))
            .expect("sent");
        release.send(()).expect("the first word waits");
        let order: Vec<UpgradeWord> = (0..2).map(|_| answers.recv().expect("an answer")).collect();
        assert!(
            !overlapped.load(Ordering::SeqCst),
            "a second word entered the writer while the first waited"
        );
        assert_eq!(order, [UpgradeWord::Now, UpgradeWord::NotToday]);
    }

    #[test]
    fn a_refused_word_on_a_waits_record_is_a_gesture_failure_row() {
        use aterm_messages::Hold;
        let mut app = App::headless_for_test();
        let (writer, asked) = recording(|_| Err("busy:another-sweep".to_string()));
        app.upgrade_view.writer = writer;
        let waiting = vec![row("s-a", 7 * 3_600)];
        app.apply_agent_upgrades(waiting.clone());
        assert_eq!(app.messages.live_rows().count(), 0, "a wait is a record");
        let id = record_id_of(&app, "s-a").expect("its record");
        press_record(&mut app, id, 0);
        assert!(asked.try_recv().is_ok(), "the writer was asked");
        assert_eq!(
            app.upgrade_view.looks_asked, 0,
            "NEGATIVE CONTROL (D22): a busy refusal is no reason to look again"
        );
        let live: Vec<_> = app.messages.live_rows().map(|l| l.msg.clone()).collect();
        assert_eq!(live.len(), 1, "{live:?}");
        assert_eq!(live[0].hold, Hold::For(aterm_messages::HOLD_GESTURE));
        assert_eq!(live[0].severity, aterm_messages::Severity::Error);
        assert_eq!(
            live[0].actions,
            [Intent::AgentUpgrade {
                tab: "s-a".to_string(),
                to: "2.1.282".to_string(),
                word: UpgradeWord::Now,
            }],
            "the press itself is the remedy"
        );
        app.apply_agent_upgrades(waiting);
        assert_eq!(
            app.messages.live_rows().count(),
            1,
            "the host's same rows post the wait no row"
        );
    }

    #[test]
    fn a_press_refused_because_its_round_stopped_says_so_and_looks_again() {
        let mut app = App::headless_for_test();
        let (writer, _asked) = recording(|_| {
            Err(
                "stopped:0:the upgrade in tab s-a stopped (no-resume): `--now` re-arms only \
                 one that gave up"
                    .to_string(),
            )
        });
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![row("s-a", 7 * 3_600)]);
        let id = record_id_of(&app, "s-a").expect("the overdue record");
        let looks = app.upgrade_view.looks_asked;
        press_record(&mut app, id, 0);
        assert_eq!(
            app.upgrade_view.looks_asked,
            looks + 1,
            "the host is asked to look again, once"
        );
        let live: Vec<_> = app.messages.live_rows().map(|l| l.msg.clone()).collect();
        assert_eq!(live.len(), 1, "{live:?}");
        assert!(
            live[0]
                .actions
                .iter()
                .all(|i| i.label() != aterm_messages::UpgradeWord::Now.label()),
            "no button of the refused word: {:?}",
            live[0].actions
        );
    }

    #[test]
    fn a_press_on_a_record_whose_upgrade_moved_on_lands_on_nothing() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-gui-upgrade-word-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (home, state) = (dir.join("home"), dir.join("state"));
        std::fs::create_dir_all(state.join("upgrade")).expect("state");
        let file = state.join("upgrade").join("conv-s-a.json");
        std::fs::write(
            &file,
            r#"{"phase":"failed","why":"no-resume","from":"2.1.281","to":"2.1.283","source":"managed","tab":"s-a","salt":5}"#,
        )
        .expect("state file");
        let before = std::fs::read_to_string(&file).expect("state file");
        let mut app = App::headless_for_test();
        app.upgrade_view.writer = word_writer_at(home, state, None);
        let shown = Row {
            phase: Phase::Failed("no-resume".to_string()),
            stop_streak: 2,
            streak_why: "no-resume".to_string(),
            ..row("s-a", 60)
        };
        app.apply_agent_upgrades(vec![shown.clone()]);
        let id = record_id_of(&app, "s-a").expect("the record for 2.1.282");
        press_record(&mut app, id, 1);
        assert_eq!(
            std::fs::read_to_string(&file).expect("state file"),
            before,
            "nothing written"
        );
        let said: Vec<_> = app.messages.live_rows().map(|l| l.msg.clone()).collect();
        assert_eq!(said.len(), 1, "the refusal's row: {said:?}");
        assert!(
            said[0].detail[0].contains("2.1.283"),
            "{:?}",
            said[0].detail
        );

        app.apply_agent_upgrades(vec![Row {
            to: "2.1.283".to_string(),
            ..shown
        }]);
        let id = record_id_of(&app, "s-a").expect("the record for 2.1.283");
        press_record(&mut app, id, 1);
        let written = std::fs::read_to_string(&file).expect("state file");
        assert!(written.contains(r#""request":"skip:2.1.283""#), "{written}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_queued_word_cannot_cross_a_source_build_change_at_the_same_target() {
        use std::sync::{Mutex, mpsc};
        use std::time::Duration;

        let mut failures = Vec::new();
        // Without birth stamps the source build itself must bound the queued choice.
        for (old_start, new_start) in [("", ""), ("A", "B")] {
            for word in [UpgradeWord::Now, UpgradeWord::NotToday, UpgradeWord::Skip] {
                let dir = aterm_tempfile::TempDir::new().expect("owned harness state");
                let (home, state) = (dir.path().join("home"), dir.path().join("state"));
                std::fs::create_dir_all(state.join("upgrade")).expect("state");
                let file = state.join("upgrade/conv-s-a.json");
                let initial = format!(
                    concat!(
                        r#"{{"phase":"pending","from":"2.1.281","to":"2.1.284","#,
                        r#""source":"managed","tab":"s-a","proc_start":"{}","salt":5}}"#,
                    ),
                    old_start,
                );
                std::fs::write(&file, initial).expect("old scope");
                let opts = upgrade_drive::Opts {
                    aterm_state: None,
                    ..crate::harness_host::window_drive_opts(home.clone(), state.clone(), None)
                };
                let real = word_writer_at(home, state.clone(), None);
                let (entered, waiting) = mpsc::channel();
                let (release, gate) = mpsc::channel();
                let gate = Mutex::new(gate);
                let gated = WordWriter::new(move |ask| {
                    entered.send(()).expect("the writer started");
                    gate.lock()
                        .expect("gate")
                        .recv_timeout(Duration::from_secs(60))
                        .expect("the replacement was saved");
                    real.write(ask)
                });
                let mut app = App::headless_for_test();
                let (capture, pressed) = recording(|_| Err("queued".to_string()));
                app.upgrade_view.writer = capture;
                app.apply_agent_upgrades(vec![Row {
                    to: "2.1.284".to_string(),
                    proc_start: old_start.to_string(),
                    ..row("s-a", 60)
                }]);
                assert!(app.send_upgrade_word(WordAsk::new(
                    "s-a".to_string(),
                    "2.1.284".to_string(),
                    word,
                )));
                // Capture through the real press path, then use the window's actual queue and
                // writer. The gate admits a completed sweep before the backend takes its lock.
                let ask = pressed
                    .try_recv()
                    .expect("the displayed scope was captured");
                assert_eq!(ask.session, "conv-s-a");
                let (answered, answer) = mpsc::channel();
                let mut queue = WordQueue::default();
                queue
                    .send(ask, &gated, move |_, result| {
                        answered.send(result).expect("the window awaits its reply");
                    })
                    .expect("queued");
                waiting
                    .recv_timeout(Duration::from_secs(60))
                    .expect("writer at the gate");
                upgrade_drive::retarget_pending_for_test(&opts, "conv-s-a", "2.1.282", new_start)
                    .expect("a real pending retarget under the sweep lock");
                let fresh = std::fs::read(&file).expect("new scope saved at the same key");
                let body = std::str::from_utf8(&fresh).expect("state JSON");
                assert!(body.contains(r#""from":"2.1.282""#), "{body}");
                assert!(body.contains(r#""to":"2.1.284""#), "{body}");
                assert!(body.contains(r#""request":"-""#), "{body}");
                assert!(
                    body.contains(&format!(r#""proc_start":"{new_start}""#)),
                    "{body}"
                );
                assert!(!upgrade_drive::ledger_path(&opts).exists());
                assert!(!upgrade_drive::word_marker(&state).exists());
                release
                    .send(())
                    .expect("let the queued press reach the backend");
                let result = answer
                    .recv_timeout(Duration::from_secs(60))
                    .expect("the real writer completed");
                let unchanged = std::fs::read(&file).expect("state after the reply") == fresh;
                let no_ledger = !upgrade_drive::ledger_path(&opts).exists();
                let no_marker = !upgrade_drive::word_marker(&state).exists();
                if result.is_ok() || !unchanged || !no_ledger || !no_marker {
                    failures.push(format!(
                        "{word:?} ({old_start:?} -> {new_start:?}): reply={result:?}, \
                         fresh_file_unchanged={unchanged}, no_ledger={no_ledger}, \
                         no_word_marker={no_marker}"
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn one_waiting_session_offers_its_words_while_its_upgrade_takes_them() {
        use crate::native_app::MessageActOutcome;
        use aterm_messages::UpgradeWord;
        let mut app = App::headless_for_test();
        let (writer, asked) = recording(|ask: &WordAsk| {
            Ok(Row {
                request: Request::DeferUntil(now_s() + NOT_TODAY_S),
                request_at: now_s(),
                ..row(&ask.tab, 60)
            })
        });
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![row("s-a", 60), row("s-b", 60)]);
        let last = |app: &App| {
            app.messages
                .log()
                .records()
                .rfind(|r| r.key.as_deref() == Some(KEY_WAITING))
                .map(|r| (r.id, r.actions.clone()))
                .expect("the waiting record")
        };
        assert!(last(&app).1.is_empty(), "two sessions: no one tab");
        app.apply_agent_upgrades(vec![row("s-a", 60)]);
        let (id, actions) = last(&app);
        assert_eq!(
            actions,
            [UpgradeWord::Now, UpgradeWord::NotToday].map(|word| Intent::AgentUpgrade {
                tab: "s-a".to_string(),
                to: "2.1.282".to_string(),
                word,
            })
        );
        let offered = |app: &App| {
            app.messages_state()
                .entries
                .iter()
                .find(|e| e.id == id.raw())
                .map(|e| {
                    e.actions
                        .iter()
                        .map(|a| (a.label, a.still_actionable))
                        .collect::<Vec<_>>()
                })
                .expect("the entry")
        };
        let (now_label, not_today) = (UpgradeWord::Now.label(), UpgradeWord::NotToday.label());
        assert_eq!(offered(&app), [(now_label, true), (not_today, true)]);
        app.apply_agent_upgrades(vec![Row {
            request: Request::Now,
            request_at: now_s(),
            ..row("s-a", 60)
        }]);
        assert_eq!(offered(&app), [(now_label, false), (not_today, true)]);
        let outcome = app.perform_message_act(crate::WindowId(0), id.raw(), 0);
        assert!(
            matches!(outcome, MessageActOutcome::Refused { .. }),
            "{outcome:?}"
        );
        assert!(asked.try_recv().is_err(), "nothing written");
        let outcome = app.perform_message_act(crate::WindowId(0), id.raw(), 1);
        assert!(
            matches!(outcome, MessageActOutcome::Performed { .. }),
            "{outcome:?}"
        );
        let word = asked.try_recv().expect("written");
        assert_eq!(
            (word.tab.as_str(), word.to.as_str(), word.word),
            ("s-a", "2.1.282", UpgradeWord::NotToday)
        );
        assert!(
            app.messages
                .log()
                .records()
                .filter(|r| r.tag == aterm_messages::tags::HARNESS)
                .all(|r| r.severity != aterm_messages::Severity::Success),
            "a word is a choice, never a ✓"
        );
        app.apply_agent_upgrades(vec![Row {
            to: "2.1.283".to_string(),
            ..row("s-a", 60)
        }]);
        assert_eq!(offered(&app), [(now_label, false), (not_today, false)]);
        app.apply_agent_upgrades(vec![row("s-b", 60)]);
        assert!(
            offered(&app).is_empty(),
            "a gone session's words are dropped"
        );
    }

    const KEY_WAITING: &str = message_reporters::KEY_AGENT_UPGRADE;

    fn upgrade_items(
        entries: &[crate::session_chrome::TabMenuEntry],
    ) -> Vec<(String, aterm_messages::UpgradeWord)> {
        entries
            .iter()
            .filter_map(|e| match e {
                crate::session_chrome::TabMenuEntry::Upgrade { to, word, .. } => {
                    Some((to.clone(), *word))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_tabs_menu_word_is_for_the_session_and_build_its_card_showed() {
        use aterm_messages::UpgradeWord::{NotToday, Now, Skip};
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let first = app
            .tab_terminal_session(wid, 0)
            .and_then(|session| app.session_id_text(session))
            .expect("tab 0's session");
        let _ = app.split_active_stub_tab(wid);
        let sid = app
            .tab_terminal_session(wid, 0)
            .and_then(|session| app.session_id_text(session))
            .expect("the focused pane's session");
        assert_ne!(first, sid, "the new pane has the focus");
        assert!(upgrade_items(&app.tab_context_menu_model(wid, 0)).is_empty());
        let (writer, asked) = recording(|ask: &WordAsk| {
            Ok(Row {
                request: Request::Skip(ask.to.clone()),
                request_at: now_s(),
                ..row(&ask.tab, 60)
            })
        });
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![row(&first, 60), row(&sid, 60)]);
        let build = |word| ("2.1.282".to_string(), word);
        assert_eq!(
            upgrade_items(&app.tab_context_menu_model(wid, 0)),
            [build(Now), build(NotToday), build(Skip)]
        );
        assert!(app.open_tab_context_menu(wid, 0, 5, false));
        assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Left));
        assert_eq!(
            app.tab_terminal_session(wid, 0)
                .and_then(|session| app.session_id_text(session))
                .as_deref(),
            Some(&*first)
        );
        app.apply_agent_upgrades(vec![
            row(&first, 60),
            Row {
                to: "2.1.283".to_string(),
                ..row(&sid, 60)
            },
        ]);
        let skip = app.windows[&wid]
            .tab_menu
            .as_ref()
            .expect("open")
            .entries
            .iter()
            .position(|e| {
                matches!(
                    e,
                    crate::session_chrome::TabMenuEntry::Upgrade { word: Skip, .. }
                )
            })
            .expect("the skip row");
        app.activate_tab_menu_entry(wid, skip);
        assert!(app.windows[&wid].tab_menu.is_none(), "the card dismissed");
        assert_eq!(
            asked.try_recv(),
            Ok(WordAsk {
                tab: sid.clone(),
                to: "2.1.282".to_string(),
                word: Skip,
                session: String::new(),
                press: None,
                scope: None,
            })
        );
        let tab = app.windows[&wid].tab_set.tabs()[0].id;
        app.dispatch_tab_menu_upgrade(wid, tab, "s-gone".to_string(), "2.1.282".to_string(), Skip);
        assert!(asked.try_recv().is_err(), "dropped");
        app.apply_agent_upgrades(Vec::new());
        assert!(
            upgrade_items(&app.tab_context_menu_model(wid, 0)).is_empty(),
            "the rows re-keyed the chrome cache"
        );
    }

    fn refused(tab: &str) -> Row {
        Row {
            phase: Phase::Failed("stale-exit".to_string()),
            exited_at: now_s() - 600,
            wait: String::new(),
            wait_since: 0,
            ..row(tab, 60)
        }
    }

    #[test]
    fn a_stall_answered_with_a_word_is_quiet_for_the_day() {
        let mut app = App::headless_for_test();
        let (writer, _asked) = recording(|ask: &WordAsk| {
            Ok(Row {
                request: Request::DeferUntil(now_s() + NOT_TODAY_S),
                request_at: now_s(),
                ..refused(&ask.tab)
            })
        });
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![refused("s-a")]);
        assert!(row_of(&app, "s-a").is_some(), "the stall's row");
        assert!(app.send_upgrade_word(WordAsk {
            tab: "s-a".to_string(),
            to: "2.1.282".to_string(),
            word: aterm_messages::UpgradeWord::NotToday,
            session: String::new(),
            press: None,
            scope: None,
        }));
        assert!(row_of(&app, "s-a").is_none(), "answered");
        let deferred = Row {
            request: Request::DeferUntil(now_s() + NOT_TODAY_S),
            request_at: now_s(),
            ..refused("s-a")
        };
        app.apply_agent_upgrades(vec![deferred]);
        assert!(row_of(&app, "s-a").is_none(), "held: no stall");
        app.apply_agent_upgrades(vec![refused("s-a")]);
        assert!(
            row_of(&app, "s-a").is_none(),
            "answered a moment ago: quiet for the day"
        );
        app.apply_agent_upgrades(vec![refused("s-a"), refused("s-b")]);
        assert!(row_of(&app, "s-b").is_some(), "another tab: a row");
    }

    #[test]
    fn a_refusal_the_owner_put_off_from_the_tab_menu_is_news_when_it_returns() {
        use aterm_messages::UpgradeWord;
        let rearmed_refused = |tab: &str, request: Request| Row {
            request,
            request_at: now_s(),
            ..refused(tab)
        };
        let mut app = App::headless_for_test();
        let (writer, _asked) = recording(move |ask: &WordAsk| {
            Ok(Row {
                request: Request::DeferUntil(now_s() + NOT_TODAY_S),
                request_at: now_s(),
                ..refused(&ask.tab)
            })
        });
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![refused("s-a")]);
        let (id, _) = row_of(&app, "s-a").expect("the refusal's row");
        assert!(app.messages.dismiss(id, std::time::Instant::now()));
        app.sync_messages();
        app.apply_agent_upgrades(vec![rearmed_refused("s-a", Request::None)]);
        assert!(
            row_of(&app, "s-a").is_none(),
            "one stall: its row stays down"
        );
        assert!(app.upgrade_view.stalled.contains_key("s-a"));
        assert!(app.send_upgrade_word(WordAsk {
            tab: "s-a".to_string(),
            to: "2.1.282".to_string(),
            word: UpgradeWord::NotToday,
            session: String::new(),
            press: None,
            scope: None,
        }));
        assert!(row_of(&app, "s-a").is_none(), "held: no stall");
        assert!(
            !app.upgrade_view.stalled.contains_key("s-a"),
            "a held round is no re-arm"
        );
        app.apply_agent_upgrades(vec![rearmed_refused("s-a", Request::None)]);
        assert!(row_of(&app, "s-a").is_some(), "back: a row");
    }

    #[test]
    fn a_stall_the_owner_saw_is_not_posted_again_after_a_restart() {
        let dir = std::env::temp_dir().join(format!("aterm-upgrade-rows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join(crate::messages_store::FILE_NAME);
        let mut first = App::headless_for_test();
        first.messages_log = crate::messages_store::Writer::spawn(&path);
        assert!(first.messages_log.is_some(), "the writer opened");
        let (a, b, c) = ("s-a", "s-b", "s-c");
        first.apply_agent_upgrades(vec![refused(a), refused(b), refused(c)]);
        assert_eq!(first.messages.live_rows().count(), 3);
        let (id_a, _) = row_of(&first, a).expect("a's row");
        assert!(first.messages.dismiss(id_a, std::time::Instant::now()));
        first.sync_messages();
        first.apply_agent_upgrades(vec![refused(a), refused(b)]);
        assert!(row_of(&first, c).is_none());
        first.flush_messages_log();
        drop(first.messages_log.take());

        let mut next = App::headless_for_test();
        next.messages = aterm_messages::MessageCenter::new(
            crate::messages_store::load_tail(&path, u64::MAX).log,
            std::time::Instant::now(),
        );
        next.apply_agent_upgrades(vec![refused(a), refused(b), refused(c)]);
        assert!(row_of(&next, a).is_none(), "the owner dismissed it");
        let (again_b, _) = row_of(&next, b).expect("unread when the last process stopped: back");
        next.apply_agent_upgrades(vec![refused(a), refused(b), refused(c)]);
        assert_eq!(
            row_of(&next, b).map(|(id, _)| id),
            Some(again_b),
            "once: the same row, not another"
        );
        assert!(row_of(&next, c).is_some(), "it ended, and is back: news");
        let newer = Row {
            to: "2.1.283".to_string(),
            ..refused(a)
        };
        next.apply_agent_upgrades(vec![newer, refused(b), refused(c)]);
        assert!(
            row_of(&next, a).is_none(),
            "a's tab is quiet for the day the owner put it away, whatever its target"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn successor_carrying(left: &[aterm_messages::Message]) -> (App, Vec<MessageId>) {
        let mut parent = App::headless_for_test();
        let ids = left
            .iter()
            .map(|m| parent.post_message(m.clone()))
            .collect();
        let _ = parent.messages.settle(
            std::time::Instant::now() + aterm_messages::PROGRESS_GRACE,
            true,
        );
        let carry = parent.messages.carried();
        assert_eq!(carry.live.len(), left.len(), "every row is carried");
        let mut successor = App::headless_for_test();
        successor.messages.seed_carried(
            &carry,
            crate::messages_host::wall_stamp_now(),
            std::time::Instant::now(),
        );
        (successor, ids)
    }

    fn gave_up_row(tab: &str) -> aterm_messages::Message {
        aterm_messages::Message::new(
            aterm_messages::tags::HARNESS,
            aterm_messages::Severity::Warn,
            "Couldn't upgrade Claude",
        )
        .line("no READY answer after 4 notices")
        .line("Upgrade now asks it again at its next pause")
        .line("Claude Code 2.1.280 \u{2192} 2.1.283")
        .hold(aterm_messages::Hold::STANDING)
        .key(&message_reporters::agent_upgrade_key(tab))
    }

    #[test]
    fn a_tab_left_as_it_stands_keeps_its_rows_and_the_rest_are_renewed() {
        let (carried, other, mine) = ("s-carried", "s-other", "s-mine");
        let (mut app, ids) = successor_carrying(&[gave_up_row(carried), gave_up_row(other)]);
        let retired = |app: &App, id: MessageId| {
            app.messages
                .log()
                .get(id)
                .and_then(|r| r.retired().cloned())
        };
        app.apply_host_upgrades(vec![gone(mine)], vec![carried.to_string()]);
        assert_eq!(
            row_of(&app, carried).map(|(id, _)| id),
            Some(ids[0]),
            "the carried row of a tab left as it stands stays"
        );
        assert_eq!(
            retired(&app, ids[1]),
            Some(aterm_messages::Retired::Withdrawn),
            "another tab's carried row: withdrawn, as ever"
        );
        let (own, _) = row_of(&app, mine).expect("this process's own row");
        app.apply_host_upgrades(Vec::new(), vec![carried.to_string(), mine.to_string()]);
        assert_eq!(row_of(&app, mine).map(|(id, _)| id), Some(own));
        assert!(app.upgrade_view.rows.iter().any(|r| r.tab == mine));
        assert_eq!(row_of(&app, carried).map(|(id, _)| id), Some(ids[0]));
        app.apply_host_upgrades(Vec::new(), Vec::new());
        assert!(row_of(&app, mine).is_none());
        assert!(row_of(&app, carried).is_none());
        assert_eq!(
            retired(&app, ids[0]),
            Some(aterm_messages::Retired::Withdrawn)
        );
        assert!(app.upgrade_view.rows.is_empty());
    }

    #[test]
    fn a_tab_left_as_it_stands_raises_no_stall_off_the_row_the_window_holds() {
        let tab = "s-held";
        let mut app = App::headless_for_test();
        app.upgrade_view.rows = vec![refused(tab)];
        app.apply_host_upgrades(Vec::new(), vec![tab.to_string()]);
        assert!(
            row_of(&app, tab).is_none(),
            "no stall entry off the row the window holds"
        );
        assert!(
            app.upgrade_view.rows.iter().any(|r| r.tab == tab),
            "its row is kept"
        );
        app.apply_host_upgrades(vec![refused(tab)], Vec::new());
        let (id, _) = row_of(&app, tab).expect("a whole look's stall is posted");
        app.apply_host_upgrades(Vec::new(), vec![tab.to_string()]);
        assert_eq!(row_of(&app, tab).map(|(id, _)| id), Some(id), "kept");
    }

    #[test]
    fn carried_rows_are_withdrawn_once_every_tab_is_seen_never_fixed() {
        let now = now_s();
        let (a, b, gone, stopped, still, moving) =
            ("s-a", "s-b", "s-gone", "s-c", "s-still", "s-d");
        let other = |tag: aterm_messages::Tag, title: &str, key: &str| {
            aterm_messages::Message::new(tag, aterm_messages::Severity::Warn, title)
                .line("carried across the handoff")
                .hold(aterm_messages::Hold::STANDING)
                .key(key)
        };
        let others = [
            other(
                aterm_messages::tags::FABRIC,
                "Peer mail is waiting",
                "fabric.x",
            ),
            other(
                aterm_messages::tags::HARNESS,
                "Couldn't relaunch Claude",
                "harness.relaunch.s-a",
            ),
            other(
                aterm_messages::tags::HARNESS,
                "Upgrades are paused",
                "harness.upgradex.s-a",
            ),
            other(
                aterm_messages::tags::HARNESS,
                "Claude Code 2.1.283 is ready",
                message_reporters::KEY_AGENT_UPGRADE,
            ),
            other(
                aterm_messages::tags::HARNESS,
                "Upgraded Claude in tab 1",
                "harness.upgrade.done",
            ),
        ];
        let mut left = vec![
            gave_up_row(a),
            gave_up_row(b),
            gave_up_row(gone),
            gave_up_row(stopped),
            message_reporters::agent_upgrade_stalled(&refused(still), now, "in its tab"),
            gave_up_row(moving),
        ];
        left.extend(others.iter().cloned());
        let (mut app, ids) = successor_carrying(&left);
        let kept = &ids[6..];
        let carried: BTreeSet<MessageId> = app
            .messages
            .live_rows()
            .filter(|l| l.msg.origin == aterm_messages::Origin::Carried)
            .map(|l| l.id)
            .collect();
        assert_eq!(
            carried,
            ids.iter().copied().collect::<BTreeSet<_>>(),
            "the handoff seeded every row"
        );
        let retired = |app: &App, id: MessageId| {
            app.messages
                .log()
                .get(id)
                .and_then(|r| r.retired().cloned())
        };
        assert!(ids.iter().all(|id| retired(&app, *id).is_none()));
        let others_stand = |app: &App, when: &str| {
            for (id, msg) in kept.iter().zip(&others) {
                assert!(
                    app.messages.live(*id).is_some() && retired(app, *id).is_none(),
                    "{when}: {:?} stands",
                    msg.key
                );
            }
        };

        let rested = |tab: &str| Row {
            phase: Phase::Failed(aterm_agent::harness::upgrade::GAVE_UP.to_string()),
            ..row(tab, 60)
        };
        assert_eq!(rested(a).stall(now), None, "a give-up is no stall");
        let first_stop = Row {
            phase: Phase::Failed("no-resume".to_string()),
            stop_streak: 1,
            streak_why: "no-resume".to_string(),
            retry_at: now + 3_600,
            wait: String::new(),
            ..row(stopped, 60)
        };
        assert!(first_stop.stall(now).is_some());
        assert!(first_stop.asks_on_its_own(now), "a record");
        let relaunching = Row {
            phase: Phase::Exiting { at_s: now },
            ..row(moving, 60)
        };
        let mine = "s-mine";
        app.apply_agent_upgrades(vec![
            rested(a),
            rested(b),
            first_stop.clone(),
            refused(still),
            relaunching,
            refused(mine),
        ]);
        let withdrawn = Some(aterm_messages::Retired::Withdrawn);
        for (tab, id) in [(a, ids[0]), (b, ids[1]), (gone, ids[2])] {
            assert!(row_of(&app, tab).is_none(), "{tab}: down");
            assert_eq!(
                retired(&app, id),
                withdrawn,
                "{tab}: withdrawn, never fixed"
            );
        }
        assert!(row_of(&app, stopped).is_none(), "{stopped}: down");
        assert!(
            matches!(
                retired(&app, ids[3]),
                Some(aterm_messages::Retired::Superseded { .. })
            ),
            "{stopped}: superseded by its record, never fixed: {:?}",
            retired(&app, ids[3])
        );
        assert_eq!(
            row_of(&app, still).map(|(id, _)| id),
            Some(ids[4]),
            "still stalled: kept in its slot"
        );
        assert_eq!(row_of(&app, moving).map(|(id, _)| id), Some(ids[5]));
        let (own, _) = row_of(&app, mine).expect("this process's own stall");
        others_stand(&app, "the first look");
        assert_eq!(
            app.messages.live_rows().count(),
            3 + others.len(),
            "nothing else posted"
        );

        let done = Row {
            phase: Phase::Done,
            done_at: now,
            ..row(moving, 60)
        };
        app.apply_agent_upgrades(vec![
            rested(a),
            rested(b),
            first_stop,
            refused(still),
            done,
            row(mine, 60),
        ]);
        assert!(row_of(&app, moving).is_none());
        assert_eq!(
            retired(&app, ids[5]),
            withdrawn,
            "the move ended: withdrawn"
        );
        assert_eq!(
            retired(&app, own),
            Some(aterm_messages::Retired::Resolved(Outcome::Ok)),
            "this process's own row, as before"
        );
        assert_eq!(row_of(&app, still).map(|(id, _)| id), Some(ids[4]));
        others_stand(&app, "the move's end");

        app.apply_agent_upgrades(vec![refused(a), refused(still)]);
        let (again, _) = row_of(&app, a).expect("a row again");
        assert_ne!(again, ids[0]);
        others_stand(&app, "a's stall back");
    }

    #[test]
    fn a_stall_that_ended_while_its_row_was_down_is_news_when_it_returns() {
        let mut app = App::headless_for_test();
        let (a, b) = ("s-a", "s-b");
        let over = |app: &App, tab: &str| -> Vec<MessageId> {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(&*format!("harness.upgrade.{tab}")))
                .filter(|r| r.retired() == Some(&aterm_messages::Retired::Recorded))
                .map(|r| r.id)
                .collect()
        };
        app.apply_agent_upgrades(vec![refused(a), refused(b)]);
        for tab in [a, b] {
            let (id, _) = row_of(&app, tab).expect("a row");
            assert!(app.messages.dismiss(id, std::time::Instant::now()));
        }
        app.sync_messages();
        app.apply_agent_upgrades(vec![refused(a), refused(b)]);
        assert!(over(&app, a).is_empty(), "nothing ended");
        assert!(row_of(&app, a).is_none(), "one stall: its row stays down");
        app.apply_agent_upgrades(vec![refused(a)]);
        let ended = over(&app, b);
        assert_eq!(ended.len(), 1, "{ended:?}");
        app.apply_agent_upgrades(vec![refused(a), refused(b)]);
        assert!(row_of(&app, b).is_some(), "the stall is back: a row");
        assert!(row_of(&app, a).is_none(), "dismissed, and never over");
    }

    fn suspended(tab: &str) -> Row {
        Row {
            wait: "not-foreground".to_string(),
            wait_since: now_s() - 2 * 3_600,
            ..row(tab, 2 * 3_600)
        }
    }

    fn to_look_at(app: &App) -> Vec<(String, aterm_messages::DigestEntry)> {
        app.messages
            .digest()
            .entries
            .into_iter()
            .map(|e| (e.key.clone(), e))
            .collect()
    }

    #[test]
    fn a_wait_only_the_owner_ends_is_a_line_of_the_digest_and_no_row() {
        let (mut app, [a, b, c]) = three_tabs();
        let marked = |app: &App| {
            app.windows[&crate::WindowId(0)]
                .tab_set
                .tabs()
                .iter()
                .any(|tab| tab.presentation.indicators.attention)
        };
        app.apply_agent_upgrades(vec![suspended(&a), suspended(&b), row(&c, 60)]);
        let lines: std::collections::BTreeMap<String, aterm_messages::DigestEntry> =
            to_look_at(&app).into_iter().collect();
        assert_eq!(
            lines
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            [
                format!("harness.upgrade.{a}.suspended"),
                format!("harness.upgrade.{b}.suspended")
            ]
            .into(),
            "{lines:?}"
        );
        let first = &lines[&format!("harness.upgrade.{a}.suspended")];
        assert_eq!(
            first.actions,
            [Intent::ShowSession {
                tab: 1,
                window: 0,
                session: a.clone()
            }],
            "the line takes the owner to the tab"
        );
        assert_eq!(app.messages.live_rows().count(), 0, "no row");
        assert_eq!(app.messages.wanted_rows(), 0, "nothing wants the glass");
        assert!(!marked(&app), "no tab is marked");
        let revision = app.messages.digest_revision();
        app.apply_agent_upgrades(vec![suspended(&a), suspended(&b), row(&c, 60)]);
        assert_eq!(app.messages.digest_revision(), revision);
        app.apply_agent_upgrades(vec![gone(&a), suspended(&b), row(&c, 60)]);
        assert!(row_of(&app, &a).is_some(), "the ended agent's row stands");
        assert_eq!(app.messages.live_rows().count(), 1, "the one row");
        assert_eq!(
            to_look_at(&app).len(),
            2,
            "b's line stands; a's ends in ten minutes"
        );
        app.apply_agent_upgrades(vec![suspended(&a), suspended(&b), row(&c, 60)]);
        assert_eq!(app.dismiss_exceptions(), 2);
        assert!(to_look_at(&app).is_empty());
        app.apply_agent_upgrades(vec![suspended(&a), suspended(&b), row(&c, 60)]);
        assert!(
            to_look_at(&app).is_empty(),
            "a dismissal holds when the upgrade names the fact again"
        );
    }

    #[test]
    fn a_line_ends_when_the_wait_ends_and_many_tabs_are_one_line() {
        let (mut app, [a, b, c]) = three_tabs();
        let working = |tab: &str, behind: u64| Row {
            wait: "not-idle:busy".to_string(),
            wait_since: now_s() - behind,
            minted_at: now_s() - behind,
            ..row(tab, behind)
        };
        let day = 25 * 3_600;
        app.apply_agent_upgrades(vec![
            working(&a, day),
            working(&b, day + 60),
            working(&c, day + 120),
        ]);
        let lines = to_look_at(&app);
        assert_eq!(lines.len(), 1, "one line for three tabs: {lines:?}");
        assert_eq!(lines[0].0, "harness.upgrade.own-work");
        assert!(
            lines[0].1.actions.is_empty(),
            "three tabs: no single tab to show"
        );
        let id = lines[0].1.id;
        app.apply_agent_upgrades(vec![working(&c, day + 180)]);
        assert_eq!(to_look_at(&app).len(), 1);
        assert!(
            to_look_at(&app)[0].1.title.contains("tab 3"),
            "{}",
            to_look_at(&app)[0].1.title
        );
        app.apply_agent_upgrades(Vec::new());
        assert_eq!(to_look_at(&app).len(), 1, "not before the digest's clock");
        app.settle_messages(std::time::Instant::now() + std::time::Duration::from_secs(11 * 60));
        assert!(
            to_look_at(&app).is_empty(),
            "the wait ended, so did the line"
        );
        assert!(
            matches!(
                app.messages
                    .log()
                    .get(id)
                    .and_then(aterm_messages::LogRecord::retired),
                Some(aterm_messages::Retired::Resolved(_))
            ),
            "{:?}",
            app.messages
                .log()
                .get(id)
                .and_then(aterm_messages::LogRecord::retired)
        );
    }

    #[test]
    fn a_rearmed_own_work_round_promises_no_upgrade_now() {
        use aterm_messages::UpgradeWord;
        let a = "s-a";
        let now = now_s();
        let rearmed = Row {
            last_stop: aterm_agent::harness::upgrade::GAVE_UP.to_string(),
            wait: "not-idle:busy".to_string(),
            ..row(a, 8 * 3_600)
        };
        assert!(rearmed.asks_on_its_own(now), "ruling 364's record");
        assert!(!message_reporters::agent_upgrade_words(&rearmed, now).contains(&UpgradeWord::Now));
        let said = message_reporters::agent_upgrade_stalled(&rearmed, now, "in tab 1");
        assert!(
            said.actions
                .iter()
                .all(|i| i.label() != UpgradeWord::Now.label()),
            "{:?}",
            said.actions
        );
        let queued = Row {
            last_stop: aterm_agent::harness::upgrade::GAVE_UP.to_string(),
            ..row(a, 8 * 3_600)
        };
        assert!(!queued.asks_on_its_own(now));
        assert!(message_reporters::agent_upgrade_words(&queued, now).contains(&UpgradeWord::Now));
    }

    #[test]
    fn no_wait_at_any_age_takes_a_row() {
        let mut app = App::headless_for_test();
        let now = now_s();
        let mut rows = Vec::new();
        for (n, wait) in [
            "queued",
            "not-idle:busy",
            "background",
            "attended",
            "awaiting-ready",
            "limited",
            "draft",
        ]
        .into_iter()
        .enumerate()
        {
            for (m, behind) in [3_600, 30 * 3_600, 9 * 86_400].into_iter().enumerate() {
                let tab = format!("s-{n}-{m}");
                rows.push(Row {
                    wait: wait.to_string(),
                    wait_since: now - behind,
                    ..row(&tab, behind)
                });
                rows.push(Row {
                    wait: wait.to_string(),
                    wait_since: now - behind,
                    wait_seen: now - 5,
                    ..row(&format!("{tab}-seen"), behind)
                });
            }
        }
        app.apply_agent_upgrades(rows);
        assert_eq!(app.messages.live_rows().count(), 0, "a wait is no row");
        assert_eq!(app.messages.wanted_rows(), 0, "nothing wants the glass");
    }

    #[test]
    fn a_fault_is_recorded_once_and_a_returning_one_is_news() {
        let mut app = App::headless_for_test();
        let now = now_s();
        let unclassified = Row {
            wait: "zzz-unclassified".to_string(),
            wait_since: now - 2 * 3_600,
            ..row("s-a", 3 * 3_600)
        };
        let told = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some("harness.upfault.s-a"))
                .count()
        };
        for _ in 0..3 {
            app.apply_agent_upgrades(vec![unclassified.clone()]);
        }
        assert_eq!(told(&app), 1, "{:?}", harness_records(&app));
        assert_eq!(app.messages.live_rows().count(), 0, "a fault is no row");
        app.apply_agent_upgrades(vec![row("s-a", 3 * 3_600)]);
        app.apply_agent_upgrades(vec![unclassified]);
        assert_eq!(told(&app), 2, "its return is news");
    }

    #[test]
    fn a_waiting_upgrade_is_one_entry_through_its_rounds() {
        use aterm_agent::harness::upgrade::GAVE_UP;
        use aterm_agent::harness::upgrade_drive::HeldBy;
        let mut app = App::headless_for_test();
        let now = now_s();
        let behind = |round: u64| 7 * 3_600 + round * 3_600 + 60;
        let at_round = |round: u64| Row {
            from: "2.1.283".to_string(),
            to: "2.1.284".to_string(),
            wait: String::new(),
            ..row("s-a", behind(round))
        };
        let shells = vec![HeldBy {
            pid: 7_492,
            name: "zsh".to_string(),
            since: now - 1_200,
        }];
        let asked = |asks: u32| Phase::Announced {
            at_s: now - 60,
            asks,
        };
        let at_work = |base: &Row, phase: Phase, last_stop: &str| Row {
            phase,
            last_stop: last_stop.to_string(),
            wait: "background".to_string(),
            held_by: shells.clone(),
            ..base.clone()
        };
        let rearmed = |base: &Row, wait: &str| Row {
            phase: Phase::Pending,
            last_stop: GAVE_UP.to_string(),
            wait: wait.to_string(),
            wait_since: now - 30,
            wait_seen: if wait == "attended" { now } else { 0 },
            ..base.clone()
        };
        let mut looks: Vec<(String, u64, Row)> = Vec::new();
        for round in 0..3 {
            let base = at_round(round);
            let hours = behind(round) / 3_600;
            for asks in 1..=4 {
                looks.push((
                    format!("round {round} ask {asks}"),
                    hours,
                    Row {
                        phase: asked(asks),
                        ..base.clone()
                    },
                ));
                looks.push((
                    format!("round {round} ask {asks} at work"),
                    hours,
                    at_work(&base, asked(asks), ""),
                ));
            }
            looks.push((
                format!("round {round} gave up"),
                hours,
                Row {
                    phase: Phase::Failed(GAVE_UP.to_string()),
                    retry_at: now + 3_600,
                    release: GAVE_UP.to_string(),
                    ..base.clone()
                },
            ));
            for (what, wait) in [
                ("re-armed", ""),
                ("someone typing", "attended"),
                ("a draft", "draft"),
            ] {
                looks.push((format!("round {round} {what}"), hours, rearmed(&base, wait)));
            }
            looks.push((
                format!("round {round} re-armed at work"),
                hours,
                at_work(&base, Phase::Pending, GAVE_UP),
            ));
        }
        let entries = |app: &App| -> Vec<(MessageId, aterm_messages::LogState)> {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some("harness.upgrade.s-a"))
                .map(|r| (r.id, r.state.clone()))
                .collect()
        };
        for (look, hours, r) in looks {
            app.apply_agent_upgrades(vec![r]);
            assert_eq!(
                app.messages.live_rows().count(),
                0,
                "{look}: nothing on the glass"
            );
            assert_eq!(entries(&app).len(), 1, "{look}: one entry");
            let (id, _) = entries(&app)[0].clone();
            let (_, _, detail) = record_of(&app, id);
            let says = format!("behind for {hours} h");
            assert!(
                detail.first().is_some_and(|l| l.starts_with(&says)),
                "{look}: the record says {says}: {detail:?}"
            );
        }
        let [(id, ref state)] = entries(&app)[..] else {
            unreachable!("one entry")
        };
        assert_eq!(
            *state,
            aterm_messages::LogState::Retired(aterm_messages::Retired::Recorded),
            "a record, never withdrawn"
        );
        let (_, severity, _) = record_of(&app, id);
        assert_eq!(severity, aterm_messages::Severity::Info);

        app.apply_agent_upgrades(vec![at_work(&at_round(17), asked(1), "")]);
        assert_eq!(app.messages.live_rows().count(), 0, "a day behind");
        assert_eq!(entries(&app).len(), 1, "the same record at the move clock");
        app.apply_agent_upgrades(vec![Row {
            phase: Phase::Done,
            ..at_round(17)
        }]);
        let [(done, ref state)] = entries(&app)[..] else {
            unreachable!("one entry")
        };
        assert_eq!(
            (done, state.clone()),
            (
                id,
                aterm_messages::LogState::Retired(aterm_messages::Retired::Recorded)
            )
        );
    }

    #[test]
    fn a_held_back_entrys_age_restates_nothing() {
        let mut app = App::headless_for_test();
        let held = |behind: u64| Row {
            wait: "terminal:vim".to_string(),
            ..row("s-h", behind)
        };
        app.apply_agent_upgrades(vec![held(5 * 60)]);
        assert_eq!(app.messages.live_rows().count(), 0, "a hold is no row");
        let key = "harness.upgrade.s-h";
        let entries = |app: &App| -> Vec<aterm_messages::LogRecord> {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(key))
                .cloned()
                .collect()
        };
        let [ref first] = entries(&app)[..] else {
            panic!("one entry: {:#?}", entries(&app))
        };
        let id = first.id;
        assert!(
            !first.detail.iter().any(|d| d.contains("behind for")),
            "{:?}",
            first.detail
        );
        let posted_age = |app: &App| {
            app.upgrade_view
                .stalled
                .get("s-h")
                .map(|p| p.behind.clone())
        };
        assert_eq!(posted_age(&app), Some(None), "no age in its words");
        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![held(6 * 60)]);
        assert_eq!(posted_age(&app), Some(None), "a minute on: still none");
        assert_eq!(app.messages.revision(), revision, "a minute on: nothing");
        let [ref same] = entries(&app)[..] else {
            panic!("still one entry")
        };
        assert_eq!(same.id, id);
    }

    struct Shapes {
        now: u64,
    }

    impl Shapes {
        fn base(&self, h: u64) -> Row {
            Row {
                from: "2.1.283".to_string(),
                to: "2.1.284".to_string(),
                wait: String::new(),
                ..row("s-a", h * 3_600 + 60)
            }
        }

        fn shells(&self) -> Vec<aterm_agent::harness::upgrade_drive::HeldBy> {
            vec![aterm_agent::harness::upgrade_drive::HeldBy {
                pid: 7_492,
                name: "zsh".to_string(),
                since: self.now - 1_200,
            }]
        }

        fn announced(&self, h: u64) -> Row {
            Row {
                phase: Phase::Announced {
                    at_s: self.now - 60,
                    asks: 1,
                },
                wait: "background".to_string(),
                held_by: self.shells(),
                ..self.base(h)
            }
        }

        fn gave_up(&self, h: u64) -> Row {
            use aterm_agent::harness::upgrade::GAVE_UP;
            Row {
                phase: Phase::Failed(GAVE_UP.to_string()),
                retry_at: self.now + 3_600,
                release: GAVE_UP.to_string(),
                ..self.base(h)
            }
        }

        fn rearmed_at_work(&self, h: u64) -> Row {
            Row {
                phase: Phase::Pending,
                last_stop: aterm_agent::harness::upgrade::GAVE_UP.to_string(),
                wait: "background".to_string(),
                held_by: self.shells(),
                ..self.base(h)
            }
        }
    }

    fn entries_of_s_a(app: &App) -> Vec<aterm_messages::LogRecord> {
        app.messages
            .log()
            .records()
            .filter(|r| r.key.as_deref() == Some("harness.upgrade.s-a"))
            .cloned()
            .collect()
    }

    #[test]
    fn a_reworded_record_keeps_its_words_with_its_buttons() {
        let shapes = Shapes { now: now_s() };
        let words_of = |actions: &[Intent]| -> Vec<UpgradeWord> {
            actions
                .iter()
                .filter_map(|i| match i {
                    Intent::AgentUpgrade { word, .. } => Some(*word),
                    _ => None,
                })
                .collect()
        };
        let same_words = |a: &[UpgradeWord], b: &[UpgradeWord]| {
            a.len() == b.len() && a.iter().all(|w| b.contains(w))
        };
        for (name, looks) in [
            (
                "written at its own work",
                vec![
                    shapes.announced(7),
                    shapes.gave_up(8),
                    shapes.rearmed_at_work(9),
                ],
            ),
            (
                "written at its give-up",
                vec![shapes.gave_up(7), shapes.announced(8), shapes.gave_up(9)],
            ),
        ] {
            let mut app = App::headless_for_test();
            let mut written: Option<aterm_messages::LogRecord> = None;
            let mut offered_other = false;
            for (i, look) in looks.into_iter().enumerate() {
                let offered = message_reporters::agent_upgrade_words(&look, shapes.now);
                app.apply_agent_upgrades(vec![look]);
                let entries = entries_of_s_a(&app);
                assert_eq!(entries.len(), 1, "{name}, look {i}: one entry");
                let rec = &entries[0];
                assert_eq!(
                    rec.state,
                    aterm_messages::LogState::Retired(aterm_messages::Retired::Recorded),
                    "{name}, look {i}: a record"
                );
                let says = format!("behind for {} h", 7 + i);
                assert!(
                    rec.detail[0].starts_with(&says),
                    "{name}, look {i}: says {says}: {:?}",
                    rec.detail
                );
                if let Some(before) = &written {
                    assert_eq!(
                        rec.actions, before.actions,
                        "{name}, look {i}: the buttons it was written with"
                    );
                    if !same_words(&words_of(&rec.actions), &offered) {
                        offered_other = true;
                        assert_eq!(
                            rec.detail[1..],
                            before.detail[1..],
                            "{name}, look {i}: words for {offered:?} laid over {:?}",
                            words_of(&rec.actions)
                        );
                    }
                }
                written = Some(rec.clone());
            }
            assert!(offered_other, "{name}: a look offered other buttons");
        }
    }

    #[test]
    fn the_pane_word_tells_each_agent_of_a_tab_apart() {
        use crate::tab_model::{SplitAxis, SplitTree};
        let split = |axis, first, second| SplitTree::Split {
            axis,
            ratio: 0.5,
            first: Box::new(first),
            second: Box::new(second),
        };
        let leaf = SplitTree::leaf;
        let agents = |agents: &'static [u32]| move |leaf: &u32| agents.contains(leaf);
        let word = |tree: &SplitTree<u32>, of: u32, is_agent: &dyn Fn(&u32) -> bool| {
            pane_word(tree, |leaf| *leaf == of, is_agent)
        };
        let side = split(SplitAxis::Horizontal, leaf(1), leaf(2));
        assert_eq!(
            word(&side, 1, &agents(&[1, 2])).as_deref(),
            Some("left pane")
        );
        assert_eq!(
            word(&side, 2, &agents(&[1, 2])).as_deref(),
            Some("right pane")
        );
        assert_eq!(
            word(&side, 1, &agents(&[1])),
            None,
            "an agent beside a shell"
        );
        assert_eq!(word(&leaf(1), 1, &agents(&[1])), None, "one leaf");
        let stacked = split(SplitAxis::Vertical, leaf(1), leaf(2));
        assert_eq!(
            word(&stacked, 1, &agents(&[1, 2])).as_deref(),
            Some("top pane")
        );
        assert_eq!(
            word(&stacked, 2, &agents(&[1, 2])).as_deref(),
            Some("bottom pane")
        );
        let three = split(
            SplitAxis::Horizontal,
            leaf(1),
            split(SplitAxis::Vertical, leaf(2), leaf(3)),
        );
        let all = agents(&[1, 2, 3]);
        let words: Vec<_> = [1, 2, 3].map(|of| word(&three, of, &all)).into();
        assert_eq!(
            words,
            [
                Some("pane 1".into()),
                Some("pane 2".into()),
                Some("pane 3".into())
            ]
        );
        let with_shell = agents(&[1, 3]);
        assert_eq!(word(&three, 1, &with_shell).as_deref(), Some("left pane"));
        assert_eq!(word(&three, 3, &with_shell).as_deref(), Some("right pane"));
        let flat = split(
            SplitAxis::Horizontal,
            split(SplitAxis::Horizontal, leaf(1), leaf(2)),
            leaf(3),
        );
        assert_eq!(word(&flat, 2, &all).as_deref(), Some("pane 2"));
    }

    #[test]
    fn the_pane_words_count_admitted_watchers_and_leave_refused_programs_out() {
        use crate::tab_model::{SplitAxis, SplitTree};
        use aterm_phase::Program;
        let split = |first, second| SplitTree::Split {
            axis: SplitAxis::Horizontal,
            ratio: 0.5,
            first: Box::new(SplitTree::leaf(first)),
            second: Box::new(SplitTree::leaf(second)),
        };
        let word = |tree: &SplitTree<(u8, Option<Program>)>, id| {
            agent_pane_word(tree, |leaf| leaf.0 == id, |leaf| leaf.1)
        };
        let ordinary = split((1, Some(Program::Claude)), (2, Some(Program::Agy)));
        assert_eq!(word(&ordinary, 1).as_deref(), Some("left pane"));
        assert_eq!(word(&ordinary, 2).as_deref(), Some("right pane"));
        let refused = split((1, Some(Program::Claude)), (2, None));
        assert_eq!(word(&refused, 1), None, "only one listed agent");
        let no_agent = split((1, None), (2, Some(Program::Generic)));
        assert_eq!(word(&no_agent, 1), None, "no Harness table, no row");

        let watchers = split((1, Some(Program::Codex)), (2, Some(Program::Codex)));
        let mixed = split((1, Some(Program::Claude)), (2, Some(Program::Codex)));
        let vendor = split((1, Some(Program::Codex)), (2, None));
        let outcomes = [
            word(&watchers, 1).as_deref() == Some("left pane"),
            word(&watchers, 2).as_deref() == Some("right pane"),
            word(&mixed, 1).as_deref() == Some("left pane"),
            word(&mixed, 2).as_deref() == Some("right pane"),
            word(&vendor, 1).is_none(),
        ];
        assert_eq!(
            outcomes, [true; 5],
            "PANE_WORDS_REQUIRE_THE_PAGES_ADMITTED_WATCHERS"
        );
    }

    /// A press's lane steps its tab until the press lapses, handing back each row it reads and
    /// pausing between steps, and lets its tab go when it ends; a step that only waits (a draft,
    /// another sweep's lock) or acts goes on. NEGATIVE CONTROL: a record with no press standing
    /// ends the lane at its first step.
    #[test]
    fn a_press_lane_steps_until_the_press_lapses() {
        let lanes = std::sync::Mutex::new(BTreeSet::from(["s-a".to_string()]));
        let ask = WordAsk::pressed("s-a".to_string(), "2.1.282".to_string(), false);
        let script = |steps: Vec<&'static str>| {
            let steps = std::sync::Mutex::new(steps.into_iter());
            Presser::new(move |_| {
                let step = steps
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .next()
                    .unwrap_or(upgrade_drive::PRESS_LAPSED);
                (step.to_string(), Some(row("s-a", 60)))
            })
        };
        let run = |presser: &Presser| {
            let delivered = std::cell::Cell::new(0);
            let paused = std::cell::Cell::new(0);
            run_press_lane(
                &ask,
                presser,
                &lanes,
                |_| {
                    paused.set(paused.get() + 1);
                    std::time::Duration::ZERO
                },
                |_| delivered.set(delivered.get() + 1),
            );
            (delivered.get(), paused.get())
        };
        let presser = script(vec![
            "wait:draft",
            "busy:another-sweep",
            "terminated:SIGTERM",
            upgrade_drive::PRESS_LAPSED,
        ]);
        assert_eq!(run(&presser), (4, 3), "four steps, three pauses");
        assert!(
            lanes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty(),
            "the tab let go"
        );
        assert_eq!(
            run(&script(vec![upgrade_drive::PRESS_LAPSED])),
            (1, 0),
            "CONTROL: no press, one step"
        );
        for step in ["current", "skip:no-record", "busy:state-unwritable"] {
            assert!(press_lane_ends(step, std::time::Duration::ZERO), "{step}");
        }
        assert!(
            press_lane_ends("wait:draft", PRESS_LANE_MAX),
            "the span spent"
        );
        assert!(
            !press_lane_ends(
                "wait:draft",
                PRESS_LANE_MAX - std::time::Duration::from_secs(1)
            ),
            "CONTROL: within the span a wait goes on"
        );
        assert!(
            press_pace(std::time::Duration::from_secs(5))
                < press_pace(std::time::Duration::from_secs(300)),
            "close at first, then calmer"
        );
    }

    /// A press that stands in the host's rows with no lane stepping it gets one, typing what this
    /// window's press asked (the harness on) or, for a press this window did not send, only the
    /// relaunch line; one lane a tab. NEGATIVE CONTROL: a row with no press standing starts none.
    #[test]
    fn a_standing_press_with_no_lane_gets_one_from_the_hosts_rows() {
        let mut app = App::headless_for_test();
        let (tx, asked) = std::sync::mpsc::channel::<WordAsk>();
        app.upgrade_view.set_presser(Presser::new(move |ask| {
            let _ = tx.send(ask.clone());
            (upgrade_drive::PRESS_LAPSED.to_string(), None)
        }));
        let now = now_s();
        let pressed = |tab: &str| Row {
            request: Request::Now,
            request_at: now - 5,
            pressed_at: now - 5,
            pressed_to: "2.1.282".to_string(),
            ..row(tab, 60)
        };
        app.apply_host_upgrades(vec![row("s-a", 60)], Vec::new());
        assert!(asked.try_recv().is_err(), "CONTROL: no press, no lane");
        app.apply_host_upgrades(vec![pressed("s-a")], Vec::new());
        let ask = asked.try_recv().expect("a lane for the standing press");
        assert_eq!(
            (ask.tab.as_str(), ask.session.as_str(), ask.press),
            ("s-a", "conv-s-a", Some(PressAsk { off: true })),
            "a press this window did not send types only the relaunch line"
        );
        app.upgrade_view
            .pressed_off
            .insert("s-b".to_string(), false);
        app.apply_host_upgrades(vec![pressed("s-b")], Vec::new());
        let ask = asked.try_recv().expect("a lane for this window's press");
        assert_eq!(
            ask.press,
            Some(PressAsk { off: false }),
            "what the press asked"
        );
        app.upgrade_view
            .pressing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert("s-b".to_string());
        app.apply_host_upgrades(vec![pressed("s-b")], Vec::new());
        assert!(asked.try_recv().is_err(), "one lane a tab");

        // A stale host batch read before a press (including on a tab whose Autopilot was off
        // and whose request_at already matched the press's second) keeps the press standing
        // and its off-aside cleared on the glass.
        let stale_off = Row {
            request: Request::Now,
            request_at: now,
            pressed_at: 0,
            pressed_to: String::new(),
            aside: "autopilot-off".to_string(),
            wait: "autopilot-off".to_string(),
            ..row("s-c", 60)
        };
        app.apply_host_upgrades(vec![stale_off.clone()], Vec::new());
        let pressed_reply = Row {
            request: Request::Now,
            request_at: now,
            pressed_at: now,
            pressed_to: "2.1.282".to_string(),
            aside: String::new(),
            wait: "draft".to_string(),
            ..row("s-c", 60)
        };
        app.apply_upgrade_word(
            &WordAsk::pressed("s-c".to_string(), "2.1.282".to_string(), true),
            Ok(pressed_reply),
        );
        let _ = asked.try_recv();
        app.apply_host_upgrades(vec![stale_off], Vec::new());
        let shown = app.upgrade_view.row_for("s-c").expect("s-c shown");
        assert!(shown.press_stands(now), "press survives stale host batch");
        assert_eq!(shown.aside, "", "off-aside remains cleared");
        assert_eq!(shown.wait, "draft", "underlying wait restored");
    }
}
