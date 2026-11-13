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
pub(crate) fn press_pace(elapsed: std::time::Duration) -> std::time::Duration {
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

/// One stalled tab's entry: a live row, or a record.
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
                t.root.any_leaf(&mut |view| {
                    self.view_store
                        .get(*view)
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
                // Still live (carried): brought to this build's words
                // — and buttons — where they differ. Gone: the owner saw it,
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

    /// Queue an owner upgrade action from a message's action (`notice act` or Settings ▸ Messages),
    /// the tab menu, or a tab's ⬆️ chip.
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
        let several = self.windows.len() > 1;
        for (window, (wid, ws)) in self.windows.iter().enumerate() {
            for (index, tab) in ws.tab_set.tabs().iter().enumerate() {
                // Off the store's lock before a worker's name reads the store again.
                let shown = {
                    let store = self.store.read().unwrap_or_else(|p| p.into_inner());
                    tab.root.any_leaf(&mut |view| {
                        self.view_store
                            .get(*view)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session)
                            .and_then(|local| store.by_local(local))
                            .is_some_and(|h| h.sid.as_str() == sid)
                    })
                };
                if shown {
                    let window_named = several.then_some(window + 1);
                    return message_reporters::UpgradeSpot {
                        place: if tab.parked {
                            message_reporters::worker_place(
                                window_named,
                                &self.tab_name(tab, index),
                            )
                        } else {
                            message_reporters::tab_place(window_named, index + 1)
                        },
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
    use aterm_agent::harness::upgrade::Agent;

    let runs = if matches!(row.phase, Phase::Done | Phase::Relaunched { .. }) {
        &row.to
    } else {
        &row.from
    };
    if let Some(ord) = aterm_agent::harness::upgrade::build_order(row.agent, runs, managed) {
        return ord == std::cmp::Ordering::Less;
    }
    match row.agent {
        Agent::Codex => {
            if let (Some(a), Some(b)) = (
                atpkg::alab::AlabVersion::parse(runs.trim()),
                atpkg::alab::AlabVersion::parse(managed.trim()),
            ) {
                return a.precedence() < b.precedence()
                    || (a.precedence() == b.precedence() && a.sha9 != b.sha9);
            }
        }
        Agent::Claude | Agent::Agy | Agent::Muse => {}
    }
    !same_build(runs, managed)
}

fn parse_vendor_version(text: &str) -> Option<atpkg::vendor_direct::Version> {
    let t = text.trim();
    let t = t.strip_prefix('v').unwrap_or(t);
    atpkg::vendor_direct::Version::parse(t)
}

fn same_build(a: &str, b: &str) -> bool {
    use aterm_agent::harness::upgrade::Version;
    if let (Some(a), Some(b)) = (Version::parse(a), Version::parse(b)) {
        return a == b;
    }
    if let (Some(a), Some(b)) = (parse_vendor_version(a), parse_vendor_version(b)) {
        return a == b;
    }
    a == b
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
#[path = "upgrade_host_tests.rs"]
mod tests;
