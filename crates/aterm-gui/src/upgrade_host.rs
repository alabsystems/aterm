// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW'S SIDE OF THE LIVE AGENT UPGRADE — what the owner SEES of it
//! (gap audit 2026-09-24). The harness host's thread (`harness_host`, whose
//! per-session workers take the upgrade's steps, over the harness's
//! `upgrade_drive::View`) hands the window the upgrade rows of this
//! instance's tabs whenever they change (`Wake::AgentUpgrade`); this module
//! turns them into what the owner reads:
//!
//! * each tab's `upgrade=` column on `status` and `sessions`/`ls` (the rows go
//!   on the session timelines, which both verbs read);
//! * ONE RECORD while sessions wait — "Codex 0.158.0 installs itself in tab
//!   1" · worded by what holds it: "nothing to do: it starts on its own at a
//!   quiet moment in the tab, and from 2:15 PM as soon as it is idle and
//!   nobody has typed there for 20 seconds" where only the ladder's comfort
//!   does, else the floor that stands and when it moves (one session; "…
//!   installs itself in 2 tabs" for several) — on Settings ▸ Messages and
//!   `appstatus`, never a row and never a tab mark: waiting for its moment is
//!   the upgrade working, the longer the less it waits (the ladder, the
//!   owner's decision of 2026-09-28, ruling 380), and the band carries only
//!   what the person acts on (ruling e83d7d233);
//! * a ROW per STALLED tab (it was refused, stopped the same way round after
//!   round, runs where typing into the tab cannot reach, waits on what no
//!   rung passes — `blocked:*` — or is six hours behind: four past the
//!   ladder's last rung), which leaves when the stall ends — the tab itself
//!   carries `meta attention owner=upgrade`, raised by the host thread
//!   through the control socket; ONE ROW FOR AN AGENT'S OVERDUE TABS once
//!   two or more would each take one (ruling 363: three were the band's
//!   whole height), each tab's own entry then a record;
//! * a RECORD per finished move, with the model the resumed session answered
//!   on (`restart_outcome`), which until that day only the ledger's JSONL held;
//! * THE OWNER'S WORD without a shell (gap #21): the stalled row's capsules,
//!   the waiting record's (one session), and the tab menu's AGENT UPGRADE
//!   section press `Upgrade now` / `Not today` / `Skip version`, written off
//!   the winit thread through `upgrade_drive::ask_for` — the CLI's path, for
//!   the build the row or item named ([`WordWriter`], [`App::apply_upgrade_word`]);
//! * the managed-current record's counts ([`App::tabs_behind`]): "installed",
//!   not "up to date", while sessions lag, and never the hook to type into a
//!   tab whose foreground is an agent.
//!
//! Every row here is one the host VETTED (review of 2026-09-25,
//! `upgrade_drive::Row::standing`): a conversation still live, in its tab, on
//! a build older than its target. A state file for a conversation that ended
//! or was moved by hand is not handed over, so nothing here shows or counts it.
//!
//! Measured that day, before any of it: two sessions one and two releases
//! behind for 8h22m, `aterm ctl messages` with no harness record, both tabs
//! `attention=-`, and the one related record saying "Claude Code 2.1.282 …
//! up to date" and naming the atpkg hook for a tab whose foreground was Claude.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use aterm_agent::harness::upgrade::{Phase, Request};
use aterm_agent::harness::upgrade_drive::{self, Ask, Row};
use aterm_messages::{Intent, MessageId, Outcome, UpgradeWord};

use crate::App;
use crate::message_reporters;
use crate::toolchain_words::TabsBehind;

/// Unix seconds now: the clock the host's rows carry their instants on.
pub(crate) fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// How long `Not today` holds an upgrade (`--defer`): a day. It lapses on
/// its own, and an upgrade still behind is shown again when it does.
pub(crate) const NOT_TODAY_S: u64 = 24 * 3_600;

/// ONE WORD THE OWNER PRESSED on a tab's agent upgrade — a band capsule, a
/// Settings ▸ Messages button, a tab-menu item ([`Intent::AgentUpgrade`]):
/// the tab, the build the row or item was drawn for, and the word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WordAsk {
    /// The tab the agent runs in (`s-<hex>`).
    pub(crate) tab: String,
    /// The target build the owner was shown.
    pub(crate) to: String,
    /// Which word.
    pub(crate) word: UpgradeWord,
}

impl WordAsk {
    /// The harness's own word for it: what `aterm harness upgrade <tab>
    /// --now|--defer <a day>|--skip` asks.
    pub(crate) fn ask(&self) -> Ask {
        match self.word {
            UpgradeWord::Now => Ask::Now,
            UpgradeWord::NotToday => Ask::Defer(NOT_TODAY_S),
            UpgradeWord::Skip => Ask::Skip,
        }
    }
}

/// WHAT WRITES THE OWNER'S WORD: `upgrade_drive::ask_for` over the harness
/// state under the user's home — the CLI's own path, under the sweep lock
/// it takes — in production ([`live_word_writer`]); a refusal where this
/// window has no harness state, and in a test's `App` unless the test gives
/// it another (the injected-field shape AGENTS.md asks of every probe a
/// headless `App` can reach). It can wait on the lock (a step restarting a
/// session holds it), so it is only ever called off the winit thread
/// ([`App::send_upgrade_word`]).
#[derive(Clone)]
pub(crate) struct WordWriter(Arc<WriteFn>);

/// The writer's body: the tab's upgrade as the word left it, or why nothing
/// was written.
type WriteFn = dyn Fn(&WordAsk) -> Result<Row, String> + Send + Sync;

impl WordWriter {
    /// A writer from a function.
    pub(crate) fn new(f: impl Fn(&WordAsk) -> Result<Row, String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// Write `ask`: the tab's upgrade as the word left it, or why nothing was
    /// written.
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

/// THE OWNER'S WORDS, WRITTEN OFF THE WINIT THREAD ONE AT A TIME, IN THE
/// ORDER PRESSED ([`App::send_upgrade_word`]): one thread, started at the
/// first press, takes them from a channel. A word can wait up to 10 s on the
/// sweep lock, and the row gives no sign meanwhile, so a second press comes
/// while the first waits; each word replaces the last, so the last pressed
/// must be the last written — a thread per press raced them for the lock.
/// The thread ends when the view drops its sender; one that died (a panic
/// in the writer) is started again at the next press.
#[derive(Debug, Default)]
pub(crate) struct WordQueue {
    /// The writing thread's inbox, once it is started.
    tx: Option<std::sync::mpsc::Sender<WordAsk>>,
}

impl WordQueue {
    /// Queue `ask` behind every word pressed before it; the thread writes it
    /// through `writer` and hands the answer to `deliver` (both are taken
    /// only when the thread is started — for one window, always the same).
    /// `Err` when no thread could be started.
    pub(crate) fn send(
        &mut self,
        ask: WordAsk,
        writer: &WordWriter,
        deliver: impl Fn(WordAsk, Result<Row, String>) + Send + 'static,
    ) -> Result<(), String> {
        let ask = match &self.tx {
            Some(tx) => match tx.send(ask) {
                Ok(()) => return Ok(()),
                // Its thread is gone: start another.
                Err(std::sync::mpsc::SendError(ask)) => ask,
            },
            None => ask,
        };
        let (tx, rx) = std::sync::mpsc::channel::<WordAsk>();
        // Before the thread exists: the receiver is held here, so this lands.
        let _ = tx.send(ask);
        let writer = writer.clone();
        std::thread::Builder::new()
            .name("aterm-upgrade-word".to_string())
            .spawn(move || {
                // The owner pressed it and waits for the row to change.
                crate::qos::set_self(crate::qos::Role::Responsive);
                for ask in rx {
                    let result = writer.write(&ask);
                    deliver(ask, result);
                }
            })
            .map_err(|e| format!("the word could not be sent: {e}"))?;
        self.tx = Some(tx);
        Ok(())
    }
}

/// The production [`WordWriter`]: the harness state the window's host files
/// the upgrade under (`harness_host::harness_state`) and the user's home
/// (Claude's session files, which vet the tab's holder). No home or no state
/// directory: every word is refused, and said so.
pub(crate) fn live_word_writer(sock: Option<String>) -> WordWriter {
    match aterm_primer::home_dir().zip(crate::harness_host::harness_state()) {
        Some((home, state)) => word_writer_at(home, state, sock),
        None => WordWriter::default(),
    }
}

/// A [`WordWriter`] over this home and harness state: `ask_for` for the
/// ask's tab, word and target.
pub(crate) fn word_writer_at(home: PathBuf, state: PathBuf, sock: Option<String>) -> WordWriter {
    WordWriter::new(move |ask: &WordAsk| {
        let opts = upgrade_drive::Opts {
            home: home.clone(),
            state: state.clone(),
            sock: sock.clone(),
            only_sid: Some(ask.tab.clone()),
            dry_run: false,
            human_grace_s: 0,
            hand_back: true,
            background: false,
            // A word is written, nothing is read of what the tab's loop typed.
            aterm_state: None,
        };
        upgrade_drive::ask_for(&opts, &ask.tab, ask.ask(), &ask.to)
    })
}

/// One stalled tab's band row.
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
}

/// Whether a stall's `said` is a restart in flight that has not moved
/// (`stuck:<what>`, `upgrade_drive::Row::stall`).
fn stuck(said: &str) -> bool {
    said.starts_with("stuck:")
}

/// One waiting record's words: its title, its detail and the owner's words
/// it offers.
type WaitingSaid = (String, Vec<String>, Vec<Intent>);

/// A stalled tab's own entry ([`message_reporters::agent_upgrade_stalled`]):
/// its row — or, where it stands in its agent's one row (`said` ends
/// [`GROUPED`]), a RECORD with the same words and buttons, which Settings ▸
/// Messages keeps and the band does not show twice.
fn stall_entry(row: &Row, said: &str, now: u64, place: &str) -> aterm_messages::Message {
    let msg = message_reporters::agent_upgrade_stalled(row, now, place);
    if said.ends_with(GROUPED) || said.ends_with(PRESSED) {
        msg.hold(aterm_messages::Hold::LogOnly)
    } else {
        msg
    }
}

/// A stall's `said` shape for a tab that stands in its agent's ONE ROW
/// ([`App::post_upgrade_groups`]): its own entry is then a record, and the row
/// is the agent's.
const GROUPED: &str = "/grouped";

/// A stall's `said` shape for a tab a word pressed on its agent's one row is
/// on its way to ([`UpgradeView::pressed`]): a record until it lands.
const PRESSED: &str = "/pressed";

/// Whether a stall's `said` is a tab's entry that stands in its agent's one
/// row ([`GROUPED`], [`PRESSED`]): a record, the row being the agent's.
fn in_agents_row(said: &str) -> bool {
    said.ends_with(GROUPED) || said.ends_with(PRESSED)
}

/// Whether `fresh` is `posted`'s SAME STALL, only moved into its agent's one
/// row or between that row's shapes (`/row` → [`GROUPED`], [`GROUPED`] ⇄
/// [`PRESSED`]) — never out of it (round 35, D2: each such move withdrew the
/// tab's entry and recorded the same `⚠ Claude upgrade waits in tab N` again,
/// 8 copies a tab in 13 minutes and `Problems · 24 of 33` for two upgrades).
/// The entry is kept: a row leaves the band, withdrawn, its record the tab's
/// one entry; a record stays as it is.
fn regrouped(posted: &str, fresh: &str) -> bool {
    let bare = |said: &str| -> Option<String> {
        [GROUPED, PRESSED, "/row"]
            .iter()
            .find_map(|shape| said.strip_suffix(shape))
            .map(str::to_string)
    };
    fresh != posted && in_agents_row(fresh) && bare(fresh).is_some() && bare(fresh) == bare(posted)
}

/// AN AGENT'S ONE ROW FOR ITS STALLED TABS ([`App::post_upgrade_groups`]).
#[derive(Debug)]
struct GroupRow {
    /// The row — or, when the owner put it down before this process, its
    /// record ([`Self::seen`]). `None` for a row never posted: every stall it
    /// would have named the owner had seen through already, on its tab's own
    /// row.
    id: Option<MessageId>,
    /// Whose tabs.
    agent: aterm_agent::harness::upgrade::Agent,
    /// Its words as last posted or restated: title, detail and buttons.
    said: (String, Vec<String>, Vec<Intent>),
    /// The tabs it stands for now, each with the build it is stalled on its
    /// way to.
    members: BTreeMap<String, String>,
    /// The STALLS ([`message_reporters::agent_upgrade_group_member`]: a tab
    /// and its target) of a row the owner read or dismissed — in this
    /// process, or before it, read back from the row's record, or from each
    /// tab's own row seen through (`seen_through`): nothing is posted again
    /// while every stall is among them. A stall that is not — a tab that
    /// joins, a tab whose target moved on (the review of 2026-09-28: tabs
    /// alone let a dismissed row for 2.1.284 hide the same tabs stalled on
    /// 2.1.285 for good) — is news, and posts.
    seen: Option<BTreeSet<String>>,
}

impl GroupRow {
    /// Its stalls now ([`Self::seen`]'s terms).
    fn stalls(&self) -> BTreeSet<String> {
        stalls_of(&self.members)
    }
}

/// The stalls ([`message_reporters::agent_upgrade_group_member`]) of an
/// agent's row's members.
fn stalls_of(members: &BTreeMap<String, String>) -> BTreeSet<String> {
    members
        .iter()
        .map(|(tab, to)| message_reporters::agent_upgrade_group_member(tab, to))
        .collect()
}

/// A WORD PRESSED ON AN AGENT'S ONE ROW, on its way to each tab it was sent
/// for ([`App::send_upgrade_group_word`]). The entry pressed is answered once
/// every word has landed and at least one was written — never before (the
/// review of 2026-09-28: the row was answered first, and a lock another sweep
/// held refused each tab's word in turn, an error row each, under a row
/// answered as if it had worked) — and the refusals are said on it, never a
/// row per tab. Meanwhile the row stands as pressed ([`App::post_upgrade_groups`]
/// leaves it alone).
#[derive(Debug)]
struct GroupPress {
    /// The entry pressed: the agent's row, or a record of one (Settings ▸
    /// Messages).
    id: MessageId,
    /// Its agent's row's key ([`message_reporters::agent_upgrade_group_key`]).
    key: String,
    agent: aterm_agent::harness::upgrade::Agent,
    word: UpgradeWord,
    /// The build the row named its first tab with: what a refusal names.
    to: String,
    /// The tabs whose word has not landed yet.
    waiting: BTreeSet<String>,
    /// How many were written.
    landed: usize,
    /// Why each of the others was not.
    refused: Vec<String>,
    /// When it was pressed: a press whose words have not all come back after
    /// [`PRESS_BOUND`] is ended with what came back.
    at: std::time::Instant,
}

/// How long a press on an agent's row may wait for its words
/// ([`GroupPress::at`]): each waits at most ten seconds for the sweep lock,
/// one after another; a word that never comes back must not hold the row
/// for good.
const PRESS_BOUND: std::time::Duration = std::time::Duration::from_secs(300);

/// How long the host's rows may lag a word written ([`UpgradeView::worded`]):
/// the word's own announcement asks the host to look again, and a look takes
/// well under a second; a minute covers a busy machine, and a host that has
/// still not read the word by then is believed.
const WORD_LAG: std::time::Duration = std::time::Duration::from_secs(60);

/// What the window last did with the host's rows.
#[derive(Debug, Default)]
pub(crate) struct UpgradeView {
    /// The rows the host last handed over (this instance's live tabs).
    rows: Vec<Row>,
    /// The waiting records written for what waits now — each (title,
    /// detail, words), and whether a FLOOR held it rather than the ladder's
    /// comfort ([`Row::waits_for_comfort`]) — so a wait is one record per
    /// sentence, not one per change of some other row, and not one per flip
    /// of what holds it between looks. Cleared once nothing waits.
    waiting_said: Vec<(WaitingSaid, bool)>,
    /// Each stalled tab's row, and what it was posted for: a new reason or
    /// target is a new row, the same one is never posted again — not after
    /// the person read it either.
    stalled: BTreeMap<String, StallRow>,
    /// Each agent's ONE ROW for its stalled tabs, by its key
    /// ([`message_reporters::agent_upgrade_group_key`],
    /// [`App::post_upgrade_groups`]).
    groups: BTreeMap<String, GroupRow>,
    /// The tabs a word pressed on an agent's one row was sent for, until that
    /// tab's word lands ([`App::send_upgrade_group_word`]): their stall is a
    /// record meanwhile, so no row of their own flashes up between the
    /// answered row and the words taking effect one by one.
    pressed: BTreeSet<String>,
    /// The words pressed on agents' rows that are still landing
    /// ([`GroupPress`]).
    presses: Vec<GroupPress>,
    /// THE OWNER'S WORDS WRITTEN, by tab: the row each word came back with,
    /// and when ([`UpgradeView::words_landed`]). The host's rows that were
    /// read before a word was written take its request, so they never
    /// undo it on the glass for a moment (round 35, D1).
    worded: BTreeMap<String, (Row, std::time::Instant)>,
    /// The finished moves already recorded, by (conversation, when).
    done_said: BTreeSet<(String, u64)>,
    /// The managed-current wire text last posted, so a change in the rows
    /// re-derives its words (`App::post_managed_current` records only news).
    managed_text: Option<String>,
    /// What writes the owner's word ([`WordWriter`]).
    writer: WordWriter,
    /// Where a press's word goes to be written off the winit thread.
    queue: WordQueue,
    /// Each tab's menu section as last composed from the rows
    /// ([`UpgradeView::menu_for`]), and its revision: the tab chrome's
    /// cache epoch for it (`CachedChrome::upgrade_revision`), bumped — and
    /// every window's strip handed its menus again — only when a section
    /// changes.
    menus: Vec<(String, crate::session_chrome::UpgradeMenu)>,
    revision: u64,
    /// How many times THIS view asked the host to look again
    /// ([`UpgradeView::look_again`]): the host's own count is process-wide,
    /// and parallel tests bump it too.
    #[cfg(test)]
    looks_asked: u64,
}

impl UpgradeView {
    /// What the view shows is stale (a press refused because its round
    /// stopped, D22): ask the host to look again at its next wake.
    fn look_again(&mut self) {
        #[cfg(test)]
        {
            self.looks_asked += 1;
        }
        crate::harness_host::look_at_upgrades_soon();
    }

    /// A WORD WRITTEN, FROM ITS OWN ROW ([`Self::worded`]).
    fn word_landed(&mut self, row: &Row) {
        if row.request_at != 0 {
            self.worded
                .insert(row.tab.clone(), (row.clone(), std::time::Instant::now()));
        }
    }

    /// THE HOST'S ROWS, WITH THE OWNER'S WORDS THEY WERE READ TOO SOON TO
    /// CARRY (round 35, D1). The host reads the upgrades off the winit thread
    /// and hands them over as `Wake::AgentUpgrade`; a word is written off it
    /// too, one tab at a time. A summary read after one tab's word and before
    /// the next reached the window after every word had landed and the row
    /// pressed was answered, and read the later tabs overdue again: their
    /// agent's row was posted afresh — `Claude upgrade waits in 2 tabs`,
    /// drawn for about 60 ms, withdrawn at the next summary (2 presses of 2).
    /// A host row for the same conversation, target and phase whose word is
    /// OLDER than the one written (`request_at`) takes the written word, and
    /// only that: every other fact is the host's, fresher than the word's
    /// row. A row that carries the word or a newer one, moved on (another
    /// target, another phase, another conversation), or a tab gone, ends the
    /// entry — the host hands its rows over in the order it read them, so
    /// none after one that carries the word is older than it (a deferral
    /// that runs out is the host's to say); so does [`WORD_LAG`], past which
    /// the host is trusted as it reads. Only the host's rows
    /// ([`App::apply_host_upgrades`]): the window's own re-application of its
    /// rows as each word lands carries the words already.
    fn words_landed(&mut self, rows: &mut [Row]) {
        self.worded.retain(|tab, (word, at)| {
            at.elapsed() < WORD_LAG
                && rows.iter().any(|r| {
                    r.tab == *tab
                        && r.session == word.session
                        && same_build(&r.to, &word.to)
                        && r.phase == word.phase
                        && r.request_at < word.request_at
                })
        });
        for r in rows.iter_mut() {
            if let Some((word, _)) = self.worded.get(&r.tab)
                && r.request_at < word.request_at
            {
                r.request = word.request.clone();
                r.request_at = word.request_at;
            }
        }
    }

    /// A view whose owner's words go through `writer`.
    pub(crate) fn with_writer(writer: WordWriter) -> Self {
        Self {
            writer,
            ..Self::default()
        }
    }

    /// Remember the managed-current wire text the record was built from.
    pub(crate) fn note_managed_text(&mut self, text: &str) {
        self.managed_text = Some(text.to_string());
    }

    /// Whether `word` on tab `tab`'s upgrade to `to` would still do what
    /// its capsule says: the tab's row is to that build, and offers that
    /// word ([`message_reporters::agent_upgrade_words`]). What Settings ▸
    /// Messages enables a record's button by.
    pub(crate) fn offers(&self, tab: &str, to: &str, word: UpgradeWord, now: u64) -> bool {
        self.rows.iter().any(|r| {
            r.tab == tab
                && same_build(&r.to, to)
                && message_reporters::agent_upgrade_words(r, now).contains(&word)
        })
    }

    /// The tab's upgrade row, if the host handed one over.
    pub(crate) fn row_for(&self, tab: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.tab == tab)
    }

    /// Tab `tab`'s AGENT UPGRADE menu section (gap #21): what the upgrade
    /// moves and every word that does something for it
    /// ([`message_reporters::agent_upgrade_words`]); `None` with no row or
    /// no such word.
    pub(crate) fn menu_for(
        &self,
        tab: &str,
        now: u64,
    ) -> Option<crate::session_chrome::UpgradeMenu> {
        menu_of(self.row_for(tab)?, now)
    }

    /// The menus' cache epoch ([`Self::menus`]).
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
}

/// A row's menu section, when a word would do something.
fn menu_of(row: &Row, now: u64) -> Option<crate::session_chrome::UpgradeMenu> {
    let words = message_reporters::agent_upgrade_words(row, now);
    (!words.is_empty()).then(|| crate::session_chrome::UpgradeMenu {
        sid: row.tab.clone(),
        moving: atpkg::progress::sanitize_for_tty(&row.move_words(), 80),
        to: row.to.clone(),
        words,
    })
}

/// Whether the owner SAW a stalled row THROUGH (`upgrade_host`'s
/// once-only rule, the owner's report of 2026-09-27): read — its hold folded
/// — or dismissed. Any other end (resolved, withdrawn, answered, unseen,
/// evicted, recorded) is not.
fn seen_through(record: &aterm_messages::LogRecord) -> bool {
    matches!(
        record.retired(),
        Some(aterm_messages::Retired::Folded | aterm_messages::Retired::Dismissed)
    )
}

impl App {
    /// `Wake::AgentUpgrade`: the host's rows for this instance's live tabs,
    /// with the owner's words they were read too soon to carry
    /// ([`UpgradeView::words_landed`]).
    pub(crate) fn apply_host_upgrades(&mut self, mut rows: Vec<Row>) {
        self.upgrade_view.words_landed(&mut rows);
        self.apply_agent_upgrades(rows);
    }

    /// The rows for this instance's live tabs: the host's
    /// ([`Self::apply_host_upgrades`]), or the window's own with a word that
    /// landed laid in.
    pub(crate) fn apply_agent_upgrades(&mut self, rows: Vec<Row>) {
        let now = now_s();
        let before = self.tabs_behind();
        // Each tab's `upgrade=`, on the timeline both verbs read.
        let snapshot = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot();
        for h in &snapshot {
            let row = rows.iter().find(|r| r.tab == h.sid.as_str()).cloned();
            h.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .set_upgrade(row);
        }
        self.record_upgrade_waiting(&rows, now);
        self.post_upgrade_stalls(&rows, now);
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
        // Each tab's AGENT UPGRADE menu section: a change re-keys the tab
        // chrome's cache and hands every window's strip its menus again, so
        // a right-click offers the words for the build the band names.
        let menus: Vec<(String, crate::session_chrome::UpgradeMenu)> = rows
            .iter()
            .filter_map(|r| Some((r.tab.clone(), menu_of(r, now)?)))
            .collect();
        self.upgrade_view.rows = rows;
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

    /// A TAB-MENU UPGRADE ROW WAS CHOSEN (gap #21) — the in-grid card's
    /// activation, or the macOS strip's `NSMenu` relay
    /// (`Wake::TabMenuUpgrade`): the word, for the session `sid` and the
    /// build `to` the menu showed. The tab the menu popped on is re-resolved
    /// by its STABLE id and must still run `sid` in one of its panes (the
    /// `TabMenuAction` rule: a tab that closed meanwhile drops the word,
    /// never lands it on whatever sits in the slot now) — and the word is
    /// for `sid` itself, never the pane focused NOW: a split tab's menu is
    /// composed for the focused pane, the focus can move before the choice
    /// lands, and two panes' agents commonly move to the same build, where
    /// the harness's target check cannot tell them apart. Written as a band
    /// capsule's is ([`Self::send_upgrade_word`]); what it did is recorded.
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
        let _ = self.send_upgrade_word(WordAsk { tab: sid, to, word });
    }

    /// The waiting record: recorded when its words change, never a row — and
    /// STABLE ACROSS LOOKS (the second review of 2026-09-28): a sentence
    /// already said is never recorded again, and once a floor's sentence is
    /// said for an upgrade (a goal, a draft, a turn it cannot place …), the
    /// ladder's comfort (`nothing to do: …`) does not replace it — for as
    /// long as anything waits: the memory clears once nothing does (the move,
    /// or a stall, of the one tab waiting). A person near
    /// a goal-mode tab reads `attended` at one look and `goal` at the next
    /// (the gate asks the person first): each flip was a new, contradicting
    /// record.
    fn record_upgrade_waiting(&mut self, rows: &[Row], now: u64) {
        let waiting: Vec<&Row> = rows
            .iter()
            .filter(|r| {
                matches!(r.phase, Phase::Pending | Phase::Announced { .. })
                    && r.moving(now)
                    && r.stall(now).is_none()
                    // A session the owner hurried is its word's record
                    // already (`Claude moves at its next pause`, the row the
                    // press answered): round 18, day four, D7 — one `Upgrade
                    // now` press logged a second record (`… ready for 1
                    // session` then) that named no tab.
                    && r.request != Request::Now
            })
            .collect();
        let versions: Vec<&str> = waiting.iter().map(|r| r.to.as_str()).collect();
        // One agent's sessions are named by it; a mix names both products.
        let product = match waiting.first() {
            Some(first) if waiting.iter().all(|r| r.agent == first.agent) => first.agent.product(),
            _ => "Claude Code and Codex",
        };
        // ONE session: titled by its tab, with the clock time the ladder
        // stops waiting for comfort (the owner's decision of 2026-09-28).
        let one = match waiting.as_slice() {
            [only] if !only.tab.is_empty() => {
                let place = self.upgrade_place(&only.tab);
                Some(message_reporters::agent_upgrade_waiting_in(only, &place))
            }
            _ => None,
        };
        let Some(mut msg) =
            one.or_else(|| message_reporters::agent_upgrade_waiting(product, &versions))
        else {
            self.upgrade_view.waiting_said.clear();
            return;
        };
        // ONE session waiting: the record carries the owner's words for it
        // (`Upgrade now`, `Not today`), which Settings ▸ Messages offers for
        // as long as that upgrade stands ([`UpgradeView::offers`]). Several:
        // no one tab is the record's, so none — each tab's menu has its own.
        if let [only] = waiting.as_slice() {
            for intent in message_reporters::agent_upgrade_capsules(only, now) {
                msg = msg.action(intent);
            }
        }
        let floor = matches!(waiting.as_slice(), [only] if !only.waits_for_comfort());
        let said = (msg.title.clone(), msg.detail.clone(), msg.actions.clone());
        let before = &self.upgrade_view.waiting_said;
        let again = before.iter().any(|(was, _)| *was == said);
        // The ladder's comfort under a floor already said for this upgrade
        // (its title names the build and the tab).
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

    /// THE ROW THIS STALL ALREADY HAS, from before this process (the owner's
    /// report of 2026-09-27: after an update the window posted the stalls of
    /// two tabs afresh, the owner's view of them gone with the process that
    /// had kept it): a row under the stall's key still on the band — carried
    /// across the handoff — or, off it, the latest record under that key, when
    /// it is THE SAME STALL ([`message_reporters::same_stall`]) and the
    /// owner saw it through: read (its hold folded) or dismissed.
    /// `None`: a RECORD (a stall that asks again on its own, rulings 279 and
    /// 283: never on the band, so never read through, and recorded whenever
    /// its entry changes); nothing of the kind; a stall that ENDED since — its row resolved, or, down already,
    /// its end recorded ([`Self::record_stall_over`]) — so the same words
    /// coming back are news; one whose row was withdrawn (its shape or words
    /// changed, ruling 279); one the owner ANSWERED with a word (`Upgrade
    /// now`, `Not today`: [`Self::apply_upgrade_word`]), which moves or holds
    /// the upgrade, so the same stall back is the word having not moved it —
    /// news too; or one the owner never finished with: never shown (unseen,
    /// evicted), or still up, unread, when the last process stopped, quit or
    /// handed off (review of 2026-09-27: a Standing row "stays until the stall
    /// ends … or the person reads it", so such a row comes back to the band
    /// once — a carry this process seeded is live, and kept by the first arm).
    fn stall_already_shown(&self, msg: &aterm_messages::Message) -> Option<MessageId> {
        if msg.hold == aterm_messages::Hold::LogOnly {
            return None;
        }
        let key = msg.key.as_deref()?;
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

    /// THE END OF A STALL WHOSE ROW IS DOWN ALREADY (review of 2026-09-27):
    /// `posted`'s row was read or dismissed before the stall ended, so there
    /// is no live row to resolve, and the log's last word under its key would
    /// stay the person's read — and the same stall, back, would be taken for
    /// the row already seen ([`Self::stall_already_shown`]) and never shown.
    /// A record under the row's key says the stall ended
    /// ([`message_reporters::agent_upgrade_stall_over`]). Nothing when the
    /// log no longer holds the row (nothing there to take it for), nor for
    /// an entry the owner did not see through — a record, a row withdrawn,
    /// unseen or evicted — which nothing takes a returning stall for.
    ///
    /// Nor when a record came under the key after the row (ruling 307): the
    /// owner's word from the tab menu is recorded there already, and it — not
    /// the stall — is what a returning stall is read against. `end`: what
    /// became of the upgrade — its round asks again, its stuck restart moves
    /// again (ruling 327), or its tab left — and so what the record says.
    ///
    /// A TAB THAT STOOD IN ITS AGENT'S ONE ROW (round 35, D3): its entry is a
    /// record ([`in_agents_row`]), never on the band to be seen through, and
    /// the row's own end is the agent's — so without its end here the tab's
    /// `⚠ Claude upgrade waits in tab N` stood in the log with nothing after
    /// it, of a tab that had moved or whose conversation had ended.
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
            .filter(|rec| seen_through(rec) || in_agents_row(&posted.said))
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

    /// One row per stalled tab; a tab that recovered has its row resolved.
    /// A stall the owner already has from before this process is not posted
    /// again ([`Self::stall_already_shown`]).
    ///
    /// A ROW AN EARLIER PROCESS LEFT UP — carried across the handoff, live
    /// when the old process handed off — that no stall of this pass adopts is
    /// WITHDRAWN (the owner's two `Couldn't upgrade Claude · no READY answer
    /// after 4 notices` rows, 2026-09-27: a new process's view holds none of
    /// the rows the old one posted, so a stall that ended across the update,
    /// a give-up being no stall since the rest-and-re-arm, stood on the band
    /// until dismissed). Its tab no longer stalls; its tab is gone, or runs
    /// no upgrade the host hands over (a tab closed, a conversation ended or
    /// moved, a move finished before this process looked); or it stalls in
    /// another shape (ruling 279: a record's, so the row is no longer
    /// earned). NEVER RESOLVED `✓`: this process never saw that stall end,
    /// and a stall that stops standing is not a problem fixed (ruling 266,
    /// "changed is not fixed"; ruling 290's `✓` is for a move the process
    /// that kept the row saw go through). Withdrawn, the row is no record the
    /// owner saw through, so the same stall coming back is news
    /// ([`Self::stall_already_shown`]).
    ///
    /// ONLY ONCE THE HOST HAS SEEN EVERY TAB, so nothing flickers: rows reach
    /// the window only after a whole look — the roster read, every
    /// conversation's holder vetted (`upgrade_drive::View::refresh`), and the
    /// first one is always handed over — and a successor's host starts only
    /// at Commit, after the carry is seeded (`harness_host`), so the first
    /// call here is that first full look; a row whose tab still stalls the
    /// same way was adopted above, never posted again. A tab whose restart
    /// is in flight keeps its row until the move ends one way or the other
    /// (ruling 283: no recovery yet).
    ///
    /// A RESTART IN FLIGHT THAT STICKS (S2, `stuck:<what>` past `STALE_S`)
    /// says so on the tab's row IN PLACE (ruling 327; the merge review of
    /// 2026-09-27: it withdrew a repeating stop's row for a new one at every
    /// change of what it waited on, the same stop next posted yet another,
    /// and the stuck row the in-flight guard then kept read "has not exited"
    /// of an agent that had exited and relaunched). The row's own stall stays
    /// what it was posted for, so the same stop next is that row again
    /// (ruling 283), and once the restart moves the row tells what it told
    /// before it stuck. A row only a stuck restart earned is resolved once
    /// the restart moves: that stall is over — and put down already, its end
    /// is recorded as the upgrade moving again, never asking again
    /// ([`Self::record_stall_over`]). A row the person put down is
    /// not raised again for it: a stuck restart there is news, and posts, as
    /// any stall said otherwise does.
    fn post_upgrade_stalls(&mut self, rows: &[Row], now: u64) {
        let stalled: BTreeMap<String, (&Row, String)> = rows
            .iter()
            .filter_map(|r| {
                let stall = r.stall(now)?;
                // THE ENTRY'S SHAPE IS PART OF WHAT WAS SAID (ruling 279): a
                // stall still asking on its own is a record, any other a
                // row ([`message_reporters::agent_upgrade_stalled`]). Keyed
                // on the kind alone, a record that turned movable (its turn
                // still running) stayed a record — never live, so never
                // restated — while the tab took its mark, and a row that
                // turned into the asking stood on the glass.
                let shape = if r.asks_on_its_own(now) {
                    "record"
                } else {
                    "row"
                };
                Some((r.tab.clone(), (r, format!("{stall}/{}/{shape}", r.to))))
            })
            .collect();
        // AN AGENT'S OVERDUE TABS ARE ONE ROW (2026-09-28, the owner: "also
        // fix this stacking failures of claude upgrades!" — three overdue
        // tabs were three rows, the band's whole height): once two or more
        // tabs of one agent would each take an OVERDUE row — a move that will
        // come, waiting on its turn end, its own work or a word of the
        // owner's — they stand in ONE row of the agent's
        // ([`Self::post_upgrade_groups`]), and each tab's own entry is a
        // record: its mark, its `upgrade=` column and its menu's words are its
        // own as ever. The row stays the agent's until its last tab leaves; a
        // tab alone from the start keeps its own row. A stop, a refusal, a
        // restart that sticks or an agent typing cannot reach keeps its own
        // row: each has its own remedy in its own tab (rulings 283, 307, 327).
        let mut stalled = stalled;
        // A word pressed on the agent's row is on its way to these tabs: a
        // record each until it lands.
        for tab in &self.upgrade_view.pressed {
            if let Some((_, said)) = stalled.get_mut(tab)
                && said.ends_with("/row")
            {
                *said = format!("{}{PRESSED}", said.trim_end_matches("/row"));
            }
        }
        let mut rowish: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (tab, (row, said)) in &stalled {
            if said.starts_with("overdue/") && said.ends_with("/row") {
                rowish
                    .entry(message_reporters::agent_upgrade_group_key(row.agent))
                    .or_default()
                    .push(tab.clone());
            }
        }
        // A tab stays in its agent's row across a restart or a handoff too
        // (the review of 2026-09-28): one the row still on the band names, or
        // the row the owner saw through before this process — never the
        // tab's own row again, alone, beside or after its agent's.
        for (key, tabs) in &rowish {
            let all = tabs.len() >= 2 || self.upgrade_view.groups.contains_key(key);
            let named = self.group_named(key);
            for tab in tabs {
                if (all || named.contains(tab))
                    && let Some((_, said)) = stalled.get_mut(tab)
                {
                    *said = format!("{}{GROUPED}", said.trim_end_matches("/row"));
                }
            }
        }
        // The grouped tabs whose OWN row the owner saw through, the same stall
        // (the review of 2026-09-28): read before their entries turn records
        // below, so an agent's row that forms fresh — this build's first
        // process, a second tab joining one the owner put down — does not
        // show again the stalls the owner already saw.
        let own_seen: BTreeSet<String> =
            stalled
                .iter()
                .filter(|(tab, (_, said))| {
                    said.ends_with(GROUPED)
                        && self.upgrade_view.stalled.get(*tab).is_none_or(|p| {
                            !p.said.ends_with(GROUPED) && !p.said.ends_with(PRESSED)
                        })
                })
                .filter(|(tab, (row, _))| {
                    let place = self.upgrade_place(tab);
                    let own = message_reporters::agent_upgrade_stalled(row, now, &place);
                    self.stall_already_shown(&own)
                        .is_some_and(|id| self.messages.live(id).is_none())
                })
                .map(|(tab, _)| tab.clone())
                .collect();
        // A RESTART IN FLIGHT is no recovery yet (ruling 283): a stop that
        // repeats keeps its entry through the re-armed round, and the story
        // ends at Done (resolved) or at the next stop (the same entry, when
        // it stops the same way) — never a `✓` as the agent is signalled and
        // a new row a minute later.
        let in_flight: Vec<&str> = rows
            .iter()
            .filter(|r| matches!(r.phase, Phase::Exiting { .. } | Phase::Relaunched { .. }))
            .map(|r| r.tab.as_str())
            .collect();
        // A REFUSAL A RE-ARMED ROUND MEETS AGAIN IS ONE STALL (ruling 307,
        // as ruling 283 for a repeating stop): the round re-arms for the same
        // build, reads not stalled after its first look, and meets the same
        // refusal again. Counted as recovered, a dismissed row came back at
        // every round with a false `no longer stalled` in the log each time.
        // Its entry stands through the re-armed round; it ends when the tab
        // leaves the rows, a restart begins, the move is done, or the target
        // changes. A round the OWNER'S WORD holds (`Not today`, `Skip`) is
        // no re-arm (review of round 21): kept, the entry swallowed the
        // same refusal when the deferral ran out, and `Not today` read as
        // never again. Ended, the owner's word is what the returning stall
        // is read against ([`Self::record_stall_over`]), and it is news.
        // (`moving` is exactly "not held" here: a tab not in `stalled`
        // reads no stall.)
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
                    // Its restart stuck, and moved again: that stall is over.
                    None if flying && stuck(&posted.said) => Some((tab.clone(), true)),
                    None if flying => None,
                    None if rearmed(tab, posted) => None,
                    None => Some((tab.clone(), true)),
                    // Its restart sticks: said on the row, in place (below).
                    Some((_, fresh))
                        if flying && stuck(fresh) && self.messages.live(posted.id).is_some() =>
                    {
                        None
                    }
                    // The same stall moved into (or within) its agent's row:
                    // kept, one entry (below).
                    Some((_, fresh))
                        if posted.shows == posted.said && regrouped(&posted.said, fresh) =>
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
            // A tab that recovered is fixed; one still stalled, said
            // otherwise, is withdrawn for its new words — never a `✓` for
            // an upgrade that has not moved. One that MOVED (Done) is fixed
            // in words that say so (day five, D26: its record kept `⚠
            // Couldn't upgrade Claude in tab 1` beside the success record):
            // the finished words the log files it under, `✓`.
            let done = rows
                .iter()
                .find(|r| recovered && r.tab == tab && r.phase == Phase::Done);
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
            // Down already, and the tab stalls no more: its end on the
            // record, so the same stall back is news (a stall that changed
            // is another stall, and posts; one that moved is its done
            // record's). Worded by what happened (ruling 307(d)): a stuck
            // restart that moved and is still in flight (the filter's first
            // arm) carries on, and asks nothing again (ruling 327).
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
        // THE SAME STALL, MOVED INTO ITS AGENT'S ROW or between that row's
        // shapes ([`regrouped`], round 35, D2): one entry. A row leaves the
        // band — the agent's row stands for it — withdrawn, its record the
        // tab's entry from here; a record stays as it is.
        let moved_in: Vec<(String, String)> = stalled
            .iter()
            .filter_map(|(tab, (_, fresh))| {
                let posted = self.upgrade_view.stalled.get(tab)?;
                (posted.shows == posted.said && regrouped(&posted.said, fresh))
                    .then(|| (tab.clone(), fresh.clone()))
            })
            .collect();
        for (tab, fresh) in moved_in {
            let Some(posted) = self.upgrade_view.stalled.get_mut(&tab) else {
                continue;
            };
            let id = posted.id;
            posted.shows.clone_from(&fresh);
            posted.said = fresh;
            let _ = self.withdraw_message(id);
        }
        // A restart that stuck and moves again, still in flight: its row
        // tells what it told before it stuck.
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
        // What the agents' rows are made of, before the loop below takes the
        // map.
        let stalled_now: BTreeMap<String, (Row, String)> = stalled
            .iter()
            .map(|(tab, (row, said))| (tab.clone(), ((*row).clone(), said.clone())))
            .collect();
        for (tab, (row, said)) in stalled {
            if let Some(posted) = self.upgrade_view.stalled.get(&tab) {
                let id = posted.id;
                // ANOTHER STALL'S WORDS ON THE ROW: a stuck restart's laid
                // over it (the words they replace kept beneath, once), or
                // its own stall back over a stuck restart's — told afresh.
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
                    let place = self.upgrade_place(&row.tab);
                    let msg = stall_entry(row, &said, now, &place);
                    let _ = self.restate_message(id, crate::messages_host::restatement_of(&msg));
                    if let Some(posted) = self.upgrade_view.stalled.get_mut(&tab) {
                        if said == posted.said {
                            posted.beneath = None;
                        } else if beneath.is_some() {
                            posted.beneath = beneath;
                        }
                        posted.shows = said;
                        posted.person = row.person_seen(now);
                    }
                    continue;
                }
                // The same stall, never posted again — but an OVERDUE one's
                // remedy moves with its wait (at work, `--now` moves it;
                // waiting on the READY answer, a draft or a box, it does
                // not), and a capsule drawn for the old wait would press a
                // word that no longer does what it says. Restated in place
                // when its words change; a row the person already read is
                // left down. So is one whose words stop naming a person at
                // the tab (no look has seen them lately: the session works,
                // and the move waits for its next turn end) or start again —
                // its stall and its buttons stay the same, and until
                // 2026-09-28 the band said "someone is using its tab" for as
                // long as the stall stood.
                let person = row.person_seen(now);
                let fresh = message_reporters::agent_upgrade_capsules(row, now);
                if posted.person != person
                    || self
                        .messages
                        .live(id)
                        .is_some_and(|l| l.msg.actions != fresh)
                {
                    let place = self.upgrade_place(&row.tab);
                    let msg = stall_entry(row, &said, now, &place);
                    let _ = self.restate_message(id, crate::messages_host::restatement_of(&msg));
                }
                if let Some(posted) = self.upgrade_view.stalled.get_mut(&tab) {
                    posted.person = person;
                }
                continue;
            }
            let place = self.upgrade_place(&row.tab);
            let msg = stall_entry(row, &said, now, &place);
            if let Some(id) = self.stall_already_shown(&msg) {
                // Still on the band (carried): brought to this build's words
                // where they differ. Off it: the owner saw it, and it stays
                // down.
                if self
                    .messages
                    .live(id)
                    .is_some_and(|l| l.msg.title != msg.title || l.msg.detail != msg.detail)
                {
                    let _ = self.restate_message(id, crate::messages_host::restatement_of(&msg));
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
                    },
                );
                continue;
            }
            let id = self.post_message(msg);
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
                },
            );
        }
        self.post_upgrade_groups(&stalled_now, rows, &own_seen, now);
        // THE ROWS NO ENTRY OF THIS VIEW OWNS — an earlier process's, carried
        // across the handoff and not adopted above: withdrawn, whatever became
        // of their tab ([`Self::post_upgrade_stalls`]) — an agent's one row
        // too, once no tab of it stalls as a row.
        let owned: BTreeSet<MessageId> = self
            .upgrade_view
            .stalled
            .values()
            .map(|p| p.id)
            .chain(self.upgrade_view.groups.values().filter_map(|g| g.id))
            .chain(self.upgrade_view.presses.iter().map(|p| p.id))
            .collect();
        let restarting: BTreeSet<String> = in_flight
            .iter()
            .map(|tab| message_reporters::agent_upgrade_key(tab))
            .collect();
        let left: Vec<MessageId> = self
            .messages
            .live_rows()
            .filter(|l| !owned.contains(&l.id))
            .filter(|l| {
                l.msg.key.as_deref().is_some_and(|key| {
                    (message_reporters::agent_upgrade_key_tab(key).is_some()
                        && !restarting.contains(key))
                        || message_reporters::agent_upgrade_group_key_is(key)
                })
            })
            .map(|l| l.id)
            .collect();
        for id in left {
            let _ = self.withdraw_message(id);
        }
    }

    /// EACH AGENT'S ONE ROW FOR ITS STALLED TABS
    /// ([`message_reporters::agent_upgrade_stalled_group`]), from this pass's
    /// stalls (`stalled`: each stalled tab's row and what it was said for —
    /// those ending [`GROUPED`] stand in their agent's row), the host's rows
    /// (`rows`: what became of a tab no longer stalling) and the grouped tabs
    /// whose own row the owner saw through (`own_seen`). Restated in place
    /// when its words or buttons change — its tabs, their words, "behind for"
    /// in whole hours — and never posted twice: a row carried across a
    /// handoff is adopted; one the owner read or dismissed, in this process or
    /// before it, is not posted again while every STALL it would name — a tab
    /// and its target ([`message_reporters::agent_upgrade_group_members`]) —
    /// is among those the owner saw, and a stall that is not is news. A row a
    /// press is landing on is left as it stands ([`GroupPress`]). Once no tab
    /// of its agent stands in it the row ends ([`Self::end_upgrade_group`]).
    fn post_upgrade_groups(
        &mut self,
        stalled: &BTreeMap<String, (Row, String)>,
        rows: &[Row],
        own_seen: &BTreeSet<String>,
        now: u64,
    ) {
        self.expire_group_presses();
        let mut members: BTreeMap<String, Vec<(Row, String)>> = BTreeMap::new();
        for (tab, (row, said)) in stalled {
            if said.ends_with(GROUPED) {
                let place = self.upgrade_place(tab);
                members
                    .entry(message_reporters::agent_upgrade_group_key(row.agent))
                    .or_default()
                    .push((row.clone(), place));
            }
        }
        let pressing = |view: &UpgradeView, key: &str| view.presses.iter().any(|p| p.key == key);
        let ended: Vec<String> = self
            .upgrade_view
            .groups
            .keys()
            .filter(|key| !members.contains_key(*key) && !pressing(&self.upgrade_view, key))
            .cloned()
            .collect();
        for key in ended {
            if let Some(group) = self.upgrade_view.groups.remove(&key) {
                self.end_upgrade_group(key, group, stalled, rows, now);
            }
        }
        for (key, list) in members {
            // A word pressed on this row is landing: it stands as pressed.
            if pressing(&self.upgrade_view, &key) {
                continue;
            }
            let Some((first, _)) = list.first() else {
                continue;
            };
            let agent = first.agent;
            let now_members: BTreeMap<String, String> = list
                .iter()
                .map(|(row, _)| (row.tab.clone(), row.to.clone()))
                .collect();
            let stalls = stalls_of(&now_members);
            let refs: Vec<(&Row, String)> = list.iter().map(|(r, p)| (r, p.clone())).collect();
            let Some(msg) = message_reporters::agent_upgrade_stalled_group(&refs, now) else {
                continue;
            };
            let said = (msg.title.clone(), msg.detail.clone(), msg.actions.clone());
            // Put down by the owner in this process (read, dismissed): the
            // stalls it named are seen, as one read before a restart's are.
            let down = self.upgrade_view.groups.get(&key).is_some_and(|group| {
                group.seen.is_none() && group.id.is_none_or(|id| self.messages.live(id).is_none())
            });
            if let Some(group) = self.upgrade_view.groups.get_mut(&key) {
                if down {
                    group.seen = Some(group.stalls());
                }
                let unseen = group
                    .seen
                    .as_ref()
                    .is_some_and(|seen| !stalls.is_subset(seen));
                if group.seen.is_some() && !unseen {
                    group.members = now_members;
                    continue;
                }
                if !unseen {
                    let id = group.id;
                    let changed = group.said != said;
                    group.said = said;
                    group.members = now_members;
                    if changed && let Some(id) = id {
                        let _ =
                            self.restate_message(id, crate::messages_host::restatement_of(&msg));
                    }
                    continue;
                }
                // A stall the owner has not seen joined a row they put down —
                // a tab, or a tab's newer target: news.
                let id = self.post_message(msg);
                self.upgrade_view.groups.insert(
                    key,
                    GroupRow {
                        id: Some(id),
                        agent,
                        said,
                        members: now_members,
                        seen: None,
                    },
                );
                continue;
            }
            // Carried across a handoff: adopted, in this build's words.
            if let Some(id) = self.messages.live_by_key(&key).map(|l| l.id) {
                let _ = self.restate_message(id, crate::messages_host::restatement_of(&msg));
                self.upgrade_view.groups.insert(
                    key,
                    GroupRow {
                        id: Some(id),
                        agent,
                        said,
                        members: now_members,
                        seen: None,
                    },
                );
                continue;
            }
            // Read or dismissed before this process — the row's record, read
            // back — or each stall seen through on its tab's own row: stays
            // down while every stall is among those.
            let record = self
                .messages
                .log()
                .records()
                .rev()
                .find(|r| r.key.as_deref() == Some(key.as_str()))
                .filter(|r| seen_through(r))
                .map(|r| {
                    (
                        r.id,
                        message_reporters::agent_upgrade_group_members(&r.detail),
                    )
                });
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut seen_id = None;
            if let Some((id, named)) = record {
                seen_id = Some(id);
                seen.extend(
                    named
                        .iter()
                        .map(|(tab, to)| message_reporters::agent_upgrade_group_member(tab, to)),
                );
            }
            seen.extend(
                now_members
                    .iter()
                    .filter(|(tab, _)| own_seen.contains(*tab))
                    .map(|(tab, to)| message_reporters::agent_upgrade_group_member(tab, to)),
            );
            if !stalls.is_empty() && stalls.is_subset(&seen) {
                self.upgrade_view.groups.insert(
                    key,
                    GroupRow {
                        id: seen_id,
                        agent,
                        said,
                        members: now_members,
                        seen: Some(seen),
                    },
                );
                continue;
            }
            let id = self.post_message(msg);
            self.upgrade_view.groups.insert(
                key,
                GroupRow {
                    id: Some(id),
                    agent,
                    said,
                    members: now_members,
                    seen: None,
                },
            );
        }
    }

    /// The tabs an agent's row from before this pass still NAMES
    /// ([`message_reporters::agent_upgrade_group_members`]): the row under
    /// `key` on the band — this process's, or one carried across a handoff —
    /// or, off it, the latest record under `key` when the owner saw it
    /// through. Such a tab stands in its agent's row even alone (the review of
    /// 2026-09-28: after a restart the one tab still stalling of a row the
    /// owner had dismissed came back as a row of its own, and a carried
    /// one-tab row was withdrawn for a fresh row of the tab's). Empty when
    /// neither.
    fn group_named(&self, key: &str) -> BTreeSet<String> {
        let detail = match self.messages.live_by_key(key) {
            Some(live) => live.msg.detail.clone(),
            None => match self
                .messages
                .log()
                .records()
                .rev()
                .find(|r| r.key.as_deref() == Some(key))
            {
                Some(r) if seen_through(r) => r.detail.clone(),
                _ => return BTreeSet::new(),
            },
        };
        message_reporters::agent_upgrade_group_members(&detail)
            .into_iter()
            .map(|(tab, _)| tab)
            .collect()
    }

    /// AN AGENT'S ROW WHOSE TABS NO LONGER STAND IN IT (the review of
    /// 2026-09-28: it was resolved `✓` whatever became of them, with the
    /// words it stalled with), by what became of them, from the host's rows:
    ///
    /// * DOWN ALREADY (the owner read or dismissed it): its end is recorded
    ///   under its key ([`message_reporters::agent_upgrade_group_over`]), so
    ///   the same stalls back are news, in this process and after a restart
    ///   — never taken for the row already seen. Nothing for a row never
    ///   posted (each stall seen on its tab's own row: each tab's own end is
    ///   its entry's).
    /// * A RESTART IN FLIGHT is no recovery yet (ruling 283): the row is kept
    ///   as it stands until the move ends one way or the other.
    /// * A TAB STILL STALLING in another shape (a stop, a refusal, a stuck
    ///   restart: its own row), or ONE THE OWNER'S WORD HOLDS OR HURRIES (the
    ///   word's own record says what it did): withdrawn, never a `✓` — the
    ///   upgrade has not moved.
    /// * Otherwise resolved `✓` — in finished words where tabs MOVED (day
    ///   five's D26, for the row: `Claude upgraded in 3 tabs`, never the
    ///   row's `Couldn't upgrade …` beside the success records).
    fn end_upgrade_group(
        &mut self,
        key: String,
        group: GroupRow,
        stalled: &BTreeMap<String, (Row, String)>,
        rows: &[Row],
        now: u64,
    ) {
        let Some(id) = group.id else {
            return;
        };
        // Down already: seen before, or put down since the last look.
        if group.seen.is_some() || self.messages.live(id).is_none() {
            self.record_group_over(&key, id, group.agent);
            return;
        }
        let of = |tab: &String| rows.iter().find(|r| r.tab == *tab);
        let flying = group
            .members
            .keys()
            .filter_map(of)
            .any(|r| matches!(r.phase, Phase::Exiting { .. } | Phase::Relaunched { .. }));
        if flying {
            self.upgrade_view.groups.insert(key, group);
            return;
        }
        let elsewhere = group.members.keys().any(|tab| stalled.contains_key(tab));
        let worded = group
            .members
            .keys()
            .filter_map(of)
            .any(|r| r.phase != Phase::Done && r.word_in_force(now));
        if elsewhere || worded {
            let _ = self.withdraw_message(id);
            return;
        }
        let done: Vec<&Row> = group
            .members
            .keys()
            .filter_map(of)
            .filter(|r| r.phase == Phase::Done)
            .collect();
        if !done.is_empty() {
            let place = match done.as_slice() {
                [one] => Some(self.upgrade_place(&one.tab)),
                _ => None,
            };
            let _ = self.restate_message(
                id,
                aterm_messages::Restatement {
                    finished: Some(Some(message_reporters::agent_upgrade_group_went_through(
                        group.agent,
                        done.len(),
                        place.as_deref(),
                    ))),
                    ..aterm_messages::Restatement::default()
                },
            );
        }
        let _ = self.resolve_message(id, Outcome::Ok);
    }

    /// THE END OF AN AGENT'S ROW THAT IS DOWN ALREADY
    /// ([`Self::end_upgrade_group`]; [`Self::record_stall_over`] for a tab's
    /// row): recorded under its key while the log's last word there is the
    /// owner's read or dismissal of `id`, and nothing when a newer record
    /// came under the key after it (the owner's word, already said there).
    fn record_group_over(
        &mut self,
        key: &str,
        id: MessageId,
        agent: aterm_agent::harness::upgrade::Agent,
    ) {
        let Some((title, detail)) = self
            .messages
            .log()
            .get(id)
            .filter(|rec| seen_through(rec))
            .map(|rec| (rec.title.clone(), rec.detail.clone()))
        else {
            return;
        };
        let newer = self
            .messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(key))
            .is_some_and(|r| r.id != id);
        if newer {
            return;
        }
        self.record_message(message_reporters::agent_upgrade_group_over(
            agent, &title, &detail,
        ));
    }

    /// THE OWNER'S WORD ON AN AGENT'S ONE ROW ([`Intent::AgentUpgradeTabs`]):
    /// the word written for each listed tab that still takes it, for the build
    /// the row named it with, exactly as each tab's own press would be
    /// ([`Self::send_upgrade_word`]). The entry pressed is ANSWERED once every
    /// word has landed and one was written, in words that say what was asked
    /// of how many tabs ([`GroupPress`]) — never before (the review of
    /// 2026-09-28) — and each tab's own answer is recorded under its own key
    /// as its word lands. Refusals are said on the entry pressed: every word
    /// refused, the row says why and keeps its buttons, as one tab's row does;
    /// some, the answer names them — never a row per tab. The tabs still
    /// stalling after the words stand in a new row. `false` when no listed tab
    /// takes the word now (a tab whose word from an earlier press is still
    /// landing takes none).
    pub(crate) fn send_upgrade_group_word(
        &mut self,
        id: MessageId,
        word: UpgradeWord,
        moves: &[(String, String)],
    ) -> bool {
        let now = now_s();
        let landing: BTreeSet<&String> = self
            .upgrade_view
            .presses
            .iter()
            .flat_map(|p| p.waiting.iter())
            .collect();
        let taking: Vec<(String, String)> = moves
            .iter()
            .filter(|(tab, to)| {
                !landing.contains(tab) && self.upgrade_view.offers(tab, to, word, now)
            })
            .cloned()
            .collect();
        let Some((first_tab, first_to)) = taking.first().cloned() else {
            return false;
        };
        let agent = self
            .upgrade_view
            .row_for(&first_tab)
            .map(|row| row.agent)
            .unwrap_or_default();
        self.upgrade_view.presses.push(GroupPress {
            id,
            key: message_reporters::agent_upgrade_group_key(agent),
            agent,
            word,
            to: first_to,
            waiting: taking.iter().map(|(tab, _)| tab.clone()).collect(),
            landed: 0,
            refused: Vec::new(),
            at: std::time::Instant::now(),
        });
        self.upgrade_view
            .pressed
            .extend(taking.iter().map(|(tab, _)| tab.clone()));
        let mut sent = false;
        for (tab, to) in taking {
            sent |= self.send_upgrade_word(WordAsk { tab, to, word });
        }
        sent
    }

    /// ONE TAB'S WORD OF A PRESS ON ITS AGENT'S ROW LANDED
    /// ([`GroupPress`]): written, its own answer is recorded under its key and
    /// its row the word left taken at once, as a tab's own press is
    /// ([`Self::apply_upgrade_word`]); refused, the refusal is kept for the
    /// press's answer. The last to land ends the press
    /// ([`Self::finish_group_press`]).
    fn land_group_word(&mut self, at: usize, ask: &WordAsk, result: Result<Row, String>) {
        let place = self.upgrade_place(&ask.tab);
        self.upgrade_view.presses[at].waiting.remove(&ask.tab);
        match result {
            Ok(row) => {
                aterm_log::info!(
                    "harness @{}: the owner's word from the window, for its agent's tabs: {} ({})",
                    ask.tab,
                    ask.word.as_str(),
                    row.request.word()
                );
                self.upgrade_view.presses[at].landed += 1;
                self.upgrade_view.word_landed(&row);
                let queued = self
                    .upgrade_view
                    .row_for(&ask.tab)
                    .is_some_and(|shown| shown.wait == "queued");
                self.record_message(message_reporters::agent_upgrade_worded(
                    &row, ask.word, &place, queued,
                ));
                let mut rows = self.upgrade_view.rows.clone();
                if let Some(r) = rows.iter_mut().find(|r| r.tab == row.tab) {
                    *r = row;
                    self.apply_agent_upgrades(rows);
                }
            }
            Err(why) => {
                aterm_log::warn!(
                    "harness @{}: the owner's word from the window, for its agent's tabs ({}),                      was not written: {why}",
                    ask.tab,
                    ask.word.as_str()
                );
                if upgrade_drive::refused_stopped(&why).is_some() {
                    self.upgrade_view.look_again();
                }
                self.upgrade_view.presses[at].refused.push(why);
            }
        }
        if let Some(done) = self
            .upgrade_view
            .presses
            .iter()
            .position(|p| p.waiting.is_empty())
        {
            let press = self.upgrade_view.presses.remove(done);
            self.finish_group_press(press);
        }
    }

    /// A PRESS ON AN AGENT'S ROW WHOSE WORDS HAVE ALL LANDED ([`GroupPress`]):
    /// some written — the entry pressed is ANSWERED with the word's label, in
    /// words that count the tabs that took it and name the ones that did not
    /// (a record of those words when the entry is not on the band); none
    /// written — the row says why, above what it said, and keeps its buttons
    /// (a row of its own, one for the press, when the entry is not on the
    /// band).
    fn finish_group_press(&mut self, press: GroupPress) {
        let live = self.messages.live(press.id).is_some();
        let ours = self
            .upgrade_view
            .groups
            .get(&press.key)
            .is_some_and(|group| group.id == Some(press.id));
        if press.landed > 0 {
            let mut words = message_reporters::agent_upgrade_group_worded(
                press.agent,
                press.word,
                press.landed,
            );
            if let Some(why) = press.refused.first() {
                words = words.line(message_reporters::agent_upgrade_group_word_refused_line(
                    press.agent,
                    press.word,
                    press.refused.len(),
                    why,
                ));
            }
            if !live {
                self.record_message(words);
                return;
            }
            let _ = self.restate_message(
                press.id,
                aterm_messages::Restatement {
                    title: Some(words.title.clone()),
                    detail: Some(words.detail.clone()),
                    severity: Some(words.severity),
                    glyph: Some(words.glyph),
                    finished: Some(None),
                    ..aterm_messages::Restatement::default()
                },
            );
            if self.answer_message(press.id, press.word.label()) && ours {
                self.upgrade_view.groups.remove(&press.key);
            }
            return;
        }
        let Some(why) = press.refused.first() else {
            return;
        };
        let mut refused = message_reporters::agent_upgrade_group_word_refused(
            press.agent,
            press.word,
            press.refused.len(),
            (&press.to, why),
        );
        match self.messages.live(press.id).map(|l| l.msg.clone()) {
            Some(shown) if ours => {
                refused.detail.extend(shown.detail);
                refused.actions = shown.actions;
                refused.hold = shown.hold;
                refused.severity = shown.severity;
                refused.glyph = shown.glyph;
                let _ =
                    self.restate_message(press.id, crate::messages_host::restatement_of(&refused));
            }
            _ => {
                let _ = self.post_message(refused);
            }
        }
    }

    /// Presses whose words have not all come back after [`PRESS_BOUND`] end
    /// with what came back ([`Self::finish_group_press`]); their tabs are no
    /// longer on their way to a word.
    fn expire_group_presses(&mut self) {
        while let Some(at) = self
            .upgrade_view
            .presses
            .iter()
            .position(|p| p.at.elapsed() >= PRESS_BOUND)
        {
            let press = self.upgrade_view.presses.remove(at);
            for tab in &press.waiting {
                self.upgrade_view.pressed.remove(tab);
            }
            self.finish_group_press(press);
        }
    }

    /// A PRESS OF THE OWNER'S WORD ([`Intent::AgentUpgrade`]) — a band
    /// capsule, a Settings ▸ Messages button, a tab-menu item: written off
    /// the winit thread by the view's [`WordWriter`] (which may wait on the
    /// sweep lock), behind every word pressed before it ([`WordQueue`]), its
    /// result handed back as `Wake::AgentUpgradeWord`. A test's `App` has no
    /// event loop to hand it to, and writes it here. `false` only when no
    /// thread could be started (said on the row).
    pub(crate) fn send_upgrade_word(&mut self, ask: WordAsk) -> bool {
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

    /// WHAT THE OWNER'S WORD DID, SHOWN WHERE IT WAS PRESSED
    /// (`Wake::AgentUpgradeWord`). Written: the tab's stalled row — the one
    /// for this build — takes the word's words ("Claude stays on 2.1.281
    /// until tomorrow", `ℹ`, its stall title below) and leaves the band
    /// ANSWERED with the word's label (ruling 270: a choice is not delivered
    /// work — never `✓` nor `took` — and `Upgrade now` has moved nothing yet;
    /// the `✓` is `agent_upgrade_done`'s, when it moves). Its record never
    /// offers the word it was answered with again (`LogRecord::still_offers`).
    /// With no such row (a record's button, the tab menu) the same words are
    /// a record. The tab's `upgrade=`, the waiting record and the counts take
    /// the row the word left at once, in place of the view's — never one the
    /// host has since dropped — and the host's next rows, which the word's
    /// own announcement asks for, say the same. Refused (another step held
    /// the lock, the upgrade moved on, it stopped for good): the row says
    /// why, above what it said, and keeps its capsules; with no row, the
    /// refusal is a row of its own in the gesture-failure shape (the
    /// person's press failed, H11), never only a record.
    pub(crate) fn apply_upgrade_word(&mut self, ask: &WordAsk, result: Result<Row, String>) {
        let now = now_s();
        // A word pressed on its agent's one row has landed for this tab: its
        // answer is that press's ([`Self::land_group_word`]).
        self.upgrade_view.pressed.remove(&ask.tab);
        if let Some(at) = self
            .upgrade_view
            .presses
            .iter()
            .position(|p| p.word == ask.word && p.waiting.contains(&ask.tab))
        {
            self.land_group_word(at, ask, result);
            return;
        }
        let posted = self
            .upgrade_view
            .stalled
            .get(&ask.tab)
            .filter(|posted| same_build(&posted.to, &ask.to))
            .map(|posted| posted.id);
        let place = self.upgrade_place(&ask.tab);
        match result {
            Ok(row) => {
                aterm_log::info!(
                    "harness @{}: the owner's word from the window: {} ({})",
                    ask.tab,
                    ask.word.as_str(),
                    row.request.word()
                );
                // The row pressed waited on its question unread (ruling 307):
                // `Upgrade now` typed it once more, and says so.
                let queued = self
                    .upgrade_view
                    .row_for(&ask.tab)
                    .is_some_and(|shown| shown.wait == "queued");
                let said = message_reporters::agent_upgrade_worded(&row, ask.word, &place, queued);
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
                // THE TAB STANDS ALONE IN ITS AGENT'S ROW (the review of
                // 2026-09-28: its own entry is a record, so the word — its tab
                // menu's, its record's button — answered nothing, and the row
                // it pressed went `✓` at the next look, with the words it
                // stalled with): that row is answered with the word's words,
                // ruling 270's `answered`. A row of several tabs counts down
                // at the next look, the word the tab's own record.
                let alone = (!answered).then(|| self.alone_in_group(&ask.tab)).flatten();
                let grouped = alone.is_some_and(|(key, id)| {
                    let done = answer(self, id);
                    if done {
                        self.upgrade_view.groups.remove(&key);
                    }
                    done
                });
                // Answered either way, the word is the tab's stall's end
                // (round 35 review, ruling 376): its entry leaves the view, so
                // the look below records no `… asks again …` over it — the
                // tab's entry being a record in its agent's row, D3's end
                // would otherwise follow `Not today` at once.
                if answered || grouped {
                    self.upgrade_view.stalled.remove(&ask.tab);
                } else {
                    self.record_message(said);
                }
                // Only in place: the host's own rows can land before this
                // answer (a tab closed, a conversation ended), and it hands
                // over only a change — a row put back here would stand
                // until something else moved.
                let mut rows = self.upgrade_view.rows.clone();
                if let Some(r) = rows.iter_mut().find(|r| r.tab == row.tab) {
                    *r = row;
                    self.apply_agent_upgrades(rows);
                }
            }
            Err(why) => {
                aterm_log::warn!(
                    "harness @{}: the owner's word from the window ({}) was not written: {why}",
                    ask.tab,
                    ask.word.as_str()
                );
                let shown = self.upgrade_view.row_for(&ask.tab).cloned();
                let agent = shown.as_ref().map(|r| r.agent).unwrap_or_default();
                let refused = message_reporters::agent_upgrade_word_refused(
                    ask.word,
                    agent,
                    (&ask.tab, &ask.to),
                    &why,
                    &place,
                );
                // A ROUND THAT STOPPED under the press (day five, D22): what
                // the view shows is stale, so the host is asked to look again
                // now — its next rows restate or retire the entry — and until
                // then the entry keeps neither the refused word's capsule nor
                // the lines about the round that is gone (`its turn is still
                // running`, `Upgrade now moves it…`), only the move itself.
                let stopped = upgrade_drive::refused_stopped(&why).is_some();
                if stopped {
                    self.upgrade_view.look_again();
                }
                let restated = match (posted, shown.as_ref()) {
                    (Some(id), Some(row)) => {
                        let stalled = message_reporters::agent_upgrade_stalled(row, now, &place);
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
                // The tab stands alone in its agent's row: that row says why,
                // above what it said, and keeps its buttons — as the tab's own
                // row would — never a row of its own beside it.
                let grouped = !restated
                    && self.alone_in_group(&ask.tab).is_some_and(|(_, id)| {
                        let Some(shown) = self.messages.live(id).map(|l| l.msg.clone()) else {
                            return false;
                        };
                        let mut words = refused.clone();
                        words.detail.extend(shown.detail);
                        words.actions = shown.actions;
                        words.hold = shown.hold;
                        words.severity = shown.severity;
                        words.glyph = shown.glyph;
                        self.restate_message(id, crate::messages_host::restatement_of(&words))
                    });
                if !restated && !grouped {
                    let _ = self.post_message(refused);
                }
            }
        }
    }

    /// The agent's row tab `tab` stands in ALONE, if one is on the band: its
    /// key and id ([`Self::apply_upgrade_word`]: a word on that tab is the
    /// row's word).
    fn alone_in_group(&self, tab: &str) -> Option<(String, MessageId)> {
        self.upgrade_view.groups.iter().find_map(|(key, group)| {
            let id = group.id?;
            (group.seen.is_none()
                && group.members.len() == 1
                && group.members.contains_key(tab)
                && self.messages.live(id).is_some())
            .then(|| (key.clone(), id))
        })
    }

    /// Where tab `sid` is, in the person's words (ruling 270): `in tab 2` by
    /// its place in its window's strip, `in its tab` when no window shows it
    /// now. Never the raw `s-<hex>` a person never sees. EVERY terminal leaf
    /// of a tab counts, not only its focused one (round 17 review, V5): an
    /// agent in a split pane the keyboard is not in is still in that tab.
    ///
    /// WITH MORE THAN ONE WINDOW the window is named too, `in window 2, tab
    /// 1`: tabs are counted per window, and the band is every window's, so two
    /// agents first in their own windows both read `in tab 1` — the same
    /// line twice, and a word pressed on the wrong one. Windows are counted
    /// in the order they were opened (their ids), as the connection map's
    /// `Window 2` counts them; one window keeps the short words.
    pub(crate) fn upgrade_place(&self, sid: &str) -> String {
        let store = self.store.read().unwrap_or_else(|p| p.into_inner());
        let several = self.windows.len() > 1;
        for (window, ws) in self.windows.values().enumerate() {
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
                    return message_reporters::tab_place(several.then_some(window + 1), index + 1);
                }
            }
        }
        "in its tab".to_string()
    }

    /// HOW THE LIVE TABS STAND for the managed-current record
    /// ([`TabsBehind`]): every registered tab carrying the adoption mark
    /// (lowered, never raised, by measurement —
    /// `SessionTimeline::take_path_lowered`) split by what runs in front — an
    /// agent tab is never handed the hook to type, and one the live upgrade is
    /// moving heals with the move (its relaunch line sources the hook) — plus
    /// the shells still waiting to be adopted; the unmarked tabs whose PATH
    /// was measured, two jobs in a row, to put a foreign copy first; and the
    /// Claude sessions behind THE VERSION THE RECORD NAMES (its wire text,
    /// remembered by `post_managed_current`), split into those the upgrade
    /// moves on its own and those it will not move —
    /// stalled, or held by the owner's word (review of 2026-09-25: an overdue
    /// session was said to move at its next turn end while the band showed it
    /// stalled, and a row moving to ANY version was counted). The version,
    /// not the copy: a session moving onto the native install of that version
    /// lags it just the same — measured 2026-09-25, one of the owner's two
    /// lagging sessions was the native 2.1.281 moving to native 2.1.282, the
    /// session the gap audit's false "up to date" was said over.
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
            let agent = timeline
                .agent()
                .program
                .as_deref()
                .and_then(aterm_phase::program_of)
                .is_some();
            let moving = timeline.upgrade().is_some_and(|row| row.moving(now));
            match (agent, moving) {
                (false, _) => tabs.frozen_shells += 1,
                (true, false) => tabs.frozen_agents += 1,
                (true, true) => {}
            }
        }
        let managed = self
            .upgrade_view
            .managed_text
            .as_deref()
            .and_then(|text| crate::toolchain_words::managed_version(text, "claude"));
        // The managed-current record names Claude Code's build: a Codex
        // row is behind another build altogether.
        for row in self
            .upgrade_view
            .rows
            .iter()
            .filter(|r| r.agent == aterm_agent::harness::upgrade::Agent::Claude)
        {
            if !managed.as_deref().is_some_and(|v| same_build(v, &row.to)) {
                continue;
            }
            if row.moving(now) && row.stall(now).is_none() {
                tabs.agents_behind += 1;
            } else if matches!(
                row.phase,
                Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
            ) {
                tabs.agents_held += 1;
            }
        }
        tabs
    }
}

/// Whether two version strings name the same build: equal as versions
/// (`2.1` is `2.1.0`), or, where either does not parse, as text.
fn same_build(a: &str, b: &str) -> bool {
    use aterm_agent::harness::upgrade::Version;
    match (Version::parse(a), Version::parse(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_agent::harness::upgrade::Request;

    /// A pending upgrade `behind_for` behind, waiting on a notice behind a
    /// full queue — the wait `Upgrade now` still moves once it is overdue
    /// (ruling 380: on a turn still running, hours on the ladder's last rung,
    /// it moves nothing `--now` does not; until the owner's decision of
    /// 2026-09-28 this rig waited `not-idle:busy`).
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

    fn harness_records(app: &App) -> Vec<(String, Vec<String>)> {
        app.messages
            .log()
            .records()
            .filter(|r| r.tag == aterm_messages::tags::HARNESS)
            .map(|r| (r.title.clone(), r.detail.clone()))
            .collect()
    }

    /// ONE SESSION WAITING (ruling 380, the owner, 2026-09-28): its record is
    /// titled by its build — `… installs itself`, the tab in its move line
    /// where the place would break the title rule — says there is nothing to
    /// do where only the ladder's comfort holds it, and is written ONCE across
    /// looks, past the ladder's Land rung too (its clock time is that rung's,
    /// never the look's). No row, no mark.
    #[test]
    fn one_waiting_session_is_one_record_written_once() {
        let mut app = App::headless_for_test();
        // Held by nothing but the ladder's comfort (its screen settling): the
        // record's sentence is the ladder's own promise.
        let settling = Row {
            wait: "settling".to_string(),
            ..row("s-b", 3 * 3_600)
        };
        for _ in 0..3 {
            app.apply_agent_upgrades(vec![settling.clone()]);
        }
        assert_eq!(app.messages.live_rows().count(), 0, "no row");
        let records = harness_records(&app);
        assert_eq!(records.len(), 1, "{records:?}");
        assert!(records[0].0.contains(" installs itself"), "{records:?}");
        assert!(records[0].1[0].starts_with("nothing to do: it starts on its own"));
        assert!(records[0].1[1].contains("Claude Code 2.1.281 \u{2192} 2.1.282"));
    }

    /// THE WAITING RECORD IS STABLE ACROSS LOOKS (the second review of
    /// 2026-09-28): a person near a goal-mode Codex tab reads `attended` at
    /// one look and `goal` at the next — the gate asks the person first — and
    /// each flip wrote a new record contradicting the last. Now the ladder's
    /// promise is said once, the goal's floor once, and no flip after that
    /// writes anything: the floor's sentence stands. A floor said first is
    /// never followed by the comfort at all. NEGATIVE CONTROLS: another floor
    /// (a draft) is news, said once; and once nothing waits, the same wait
    /// back is news again.
    #[test]
    fn a_wait_flipping_between_comfort_and_a_floor_is_recorded_once_each() {
        let codex = |tab: &str, wait: &str| Row {
            agent: aterm_agent::harness::upgrade::Agent::Codex,
            from: "0.157.1".to_string(),
            to: "0.158.0".to_string(),
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
            app.apply_agent_upgrades(vec![codex("s-g", "attended")]);
            app.apply_agent_upgrades(vec![codex("s-g", "goal")]);
        }
        let said = lines(&app);
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[0].starts_with("nothing to do: it starts on its own"));
        assert!(said[1].contains("goal"), "{said:?}");
        assert_eq!(app.messages.live_rows().count(), 0, "no row");

        // The floor first: the comfort never follows it.
        let mut app = App::headless_for_test();
        for _ in 0..4 {
            app.apply_agent_upgrades(vec![codex("s-h", "goal")]);
            app.apply_agent_upgrades(vec![codex("s-h", "attended")]);
        }
        assert_eq!(lines(&app).len(), 1, "{:?}", lines(&app));
        // Another floor is news, once; the goal back is not.
        for wait in ["draft", "goal", "draft", "attended"] {
            app.apply_agent_upgrades(vec![codex("s-h", wait)]);
        }
        let said = lines(&app);
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[1].contains("draft"), "{said:?}");
        // Nothing waits: the memory goes, and the same wait back is said.
        app.apply_agent_upgrades(Vec::new());
        app.apply_agent_upgrades(vec![codex("s-h", "goal")]);
        assert_eq!(lines(&app).len(), 3, "{:?}", lines(&app));
    }

    /// WAITING IS A RECORD, NEVER A ROW, and an unchanged wait is recorded
    /// once. A STALLED tab is a row — one per tab and reason, resolved when the
    /// stall ends. NEGATIVE CONTROL: a session merely waiting for its turn end
    /// puts nothing on the glass. Before 2026-09-24 none of it existed: the
    /// window's only sink was one log line per act.
    #[test]
    fn waiting_is_recorded_once_and_only_a_stalled_tab_takes_a_row() {
        let mut app = App::headless_for_test();
        let waiting = vec![row("s-a", 3_600), row("s-b", 60)];
        app.apply_agent_upgrades(waiting.clone());
        assert_eq!(app.messages.live_rows().count(), 0, "waiting is no row");
        assert_eq!(
            harness_records(&app),
            vec![(
                "Claude Code 2.1.282 installs itself in 2 tabs".to_string(),
                vec![
                    "nothing to do: each starts on its own at a quiet moment in its tab"
                        .to_string()
                ]
            )]
        );
        app.apply_agent_upgrades(waiting);
        assert_eq!(harness_records(&app).len(), 1, "the same wait: not again");

        // One tab stalls (six hours behind): a row for it, and the waiting
        // record says one session now.
        let stalled = vec![row("s-a", 7 * 3_600), row("s-b", 60)];
        app.apply_agent_upgrades(stalled.clone());
        let live: Vec<_> = app.messages.live_rows().map(|l| l.msg.clone()).collect();
        assert_eq!(live.len(), 1, "{live:?}");
        assert_eq!(live[0].title, "Couldn't upgrade Claude in its tab yet");
        assert_eq!(live[0].key.as_deref(), Some("harness.upgrade.s-a"));
        assert!(
            live[0].detail[0].starts_with("behind for 7 h"),
            "{:?}",
            live[0].detail
        );
        assert!(
            live[0]
                .detail
                .iter()
                .any(|l| l.contains("`aterm harness upgrade s-a --now`")),
            "the owner's word is named: {:?}",
            live[0].detail
        );
        app.apply_agent_upgrades(stalled);
        assert_eq!(app.messages.live_rows().count(), 1, "one row per stall");

        // The owner's skip holds it: the stall ends and its row is resolved.
        let held = vec![
            Row {
                request: Request::Skip("2.1.282".to_string()),
                ..row("s-a", 7 * 3_600)
            },
            row("s-b", 60),
        ];
        app.apply_agent_upgrades(held);
        assert_eq!(app.messages.live_rows().count(), 0, "resolved");
    }

    /// A PERSON NO LOOK HAS SEEN LATELY STOPS BEING NAMED ON THE BAND (the
    /// review of 2026-09-28; messages.log of 2026-09-27: the row posted at
    /// 1790566047 said "someone is typing in its tab" until it was retired at
    /// 02:18, through 5 h 50 min of a turn the harness itself had continued).
    /// The words follow the person (`Row::person_seen`), but a posted row was
    /// restated only when its stall or its buttons changed, and neither does
    /// then. Now the flip is restated IN PLACE — the same row, never withdrawn
    /// and posted again — and so is a look that sees them again. NEGATIVE
    /// CONTROL: a later look that still sees them restates nothing.
    #[test]
    fn a_person_no_look_has_seen_lately_stops_being_named_on_the_band() {
        let mut app = App::headless_for_test();
        let now = now_s();
        let at = |seen_ago: u64| Row {
            wait: "attended".to_string(),
            wait_since: now - 6 * 3_600,
            wait_seen: now - seen_ago,
            ..row("s-a", 7 * 3_600)
        };
        let band = |app: &App| {
            let live: Vec<_> = app
                .messages
                .live_rows()
                .map(|l| (l.id, l.msg.detail.clone()))
                .collect();
            assert_eq!(live.len(), 1, "{live:?}");
            live[0].clone()
        };
        app.apply_agent_upgrades(vec![at(5)]);
        let (id, detail) = band(&app);
        assert_eq!(detail[0], "behind for 7 h: someone is using its tab");
        app.apply_agent_upgrades(vec![at(3_600)]);
        let (unseen, detail) = band(&app);
        assert_eq!(unseen, id, "restated in place, never posted again");
        assert_eq!(detail[0], "behind for 7 h: it waits for its next turn end");
        app.apply_agent_upgrades(vec![at(1)]);
        let (seen, detail) = band(&app);
        assert_eq!(seen, id);
        assert_eq!(detail[0], "behind for 7 h: someone is using its tab");
        // NEGATIVE CONTROL: still seen, nothing restated.
        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![at(2)]);
        assert_eq!(app.messages.revision(), revision);
    }

    /// Two registered tabs, as `(local id, sid)`.
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

    /// RULING 270's place words, for an agent in a split pane the keyboard
    /// is NOT in (round 17 review, V5): still `in tab 1` — every terminal
    /// leaf of the tab is read, not only the focused one. NEGATIVE CONTROL: a
    /// session no window shows is `in its tab`.
    #[test]
    fn an_agent_in_an_unfocused_split_is_placed_in_its_tab() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let agent = app.focused_session_id(wid).expect("the front session");
        let other = app.split_active_stub_tab(wid);
        assert_eq!(
            app.focused_session_id(wid),
            Some(other),
            "the new pane has the keyboard"
        );
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
        let agent_sid = sid_of(&app, agent);
        let other_sid = sid_of(&app, other);
        assert_eq!(app.upgrade_place(&agent_sid), "in tab 1");
        assert_eq!(app.upgrade_place(&other_sid), "in tab 1");
        assert_eq!(app.upgrade_place("s-0000000000000000"), "in its tab");
    }

    /// TWO WINDOWS, EACH WITH AN AGENT IN ITS FIRST TAB: the band is every
    /// window's, and tabs are counted per window, so both stalls read `in tab
    /// 1` — the same line twice, a capsule pressed on the wrong one. Once the
    /// owner has more than one window the place names the window too (`in
    /// window 1, tab 1`, `in window 2, tab 1`), in the order the windows
    /// opened; never a session id (ruling 270). NEGATIVE CONTROL: one window
    /// keeps the short words, tab by tab.
    #[test]
    fn two_windows_first_tabs_are_told_apart_by_their_window() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let first = app.focused_session_id(wid).expect("the front session");
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
        let (a, b) = (sid_of(&app, first), sid_of(&app, second));
        // NEGATIVE CONTROL: one window, two tabs.
        assert_eq!(app.upgrade_place(&a), "in tab 1");
        assert_eq!(app.upgrade_place(&b), "in tab 2");
        let moved = app
            .detach_active_tab_logical()
            .expect("the second tab moves");
        assert_ne!(moved, wid);
        assert_eq!(app.upgrade_place(&a), "in window 1, tab 1");
        assert_eq!(app.upgrade_place(&b), "in window 2, tab 1");
        assert_eq!(app.upgrade_place("s-0000000000000000"), "in its tab");

        let stalled = |tab: &str| Row {
            phase: Phase::Failed("not-a-shell-job".to_string()),
            ..row(tab, 60)
        };
        app.apply_agent_upgrades(vec![stalled(&a), stalled(&b)]);
        let (_, row_a) = row_of(&app, &a).expect("a's row");
        let (_, row_b) = row_of(&app, &b).expect("b's row");
        assert_eq!(row_a.title, "Couldn't upgrade Claude in window 1, tab 1");
        assert_eq!(row_b.title, "Couldn't upgrade Claude in window 2, tab 1");
        for msg in [&row_a, &row_b] {
            assert!(
                crate::message_reporters::attention(msg).is_ok(),
                "{:?}",
                msg.title
            );
            assert!(
                msg.detail[..=message_reporters::STALL_MOVE_LINE]
                    .iter()
                    .chain(std::iter::once(&msg.title))
                    .all(|line| !line.contains(&a) && !line.contains(&b)),
                "never a session id: {msg:?}"
            );
        }
    }

    /// A headless app with three tabs in its one window, and each tab's
    /// session id (`s-<hex>`), in strip order.
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

    /// The live row an agent's stalled tabs stand in, if one is up.
    fn group_of(app: &App) -> Option<(MessageId, aterm_messages::Message)> {
        let key = message_reporters::agent_upgrade_group_key(
            aterm_agent::harness::upgrade::Agent::Claude,
        );
        app.messages
            .live_rows()
            .find(|l| l.msg.key.as_deref() == Some(key.as_str()))
            .map(|l| (l.id, l.msg.clone()))
    }

    /// ONE AGENT'S OVERDUE TABS ARE ONE ROW (2026-09-28, the owner's
    /// screenshot: three rows — "Couldn't upgrade Claude yet, window 2, tab 1 ·
    /// behind for 26 h", "Claude upgrade waits in window 1, tab 1 · behind for 6
    /// h", "Couldn't upgrade Claude yet, window 1, tab 2 · behind for 7 h" —
    /// the band's whole height; the owner: "also fix this stacking failures of
    /// claude upgrades!"). The three tabs' overdue moves, as that window's
    /// records stood, stand in ONE row: its title counts them in the most
    /// serious of their words, its first line names them most behind first,
    /// its second says what its buttons do, each tab's own words are behind
    /// it, and each button is the word for exactly the tabs that take it. Each
    /// tab's own entry is a record, never a row. As tabs finish the count
    /// drops IN PLACE — the same row, restated — one tab left reads exactly as
    /// that tab's own row, and the last one gone resolves it. NEGATIVE
    /// CONTROLS: the builds before posted three rows (each tab's own key live
    /// on the band); a tab alone from the start keeps its own row, its words
    /// those of `agent_upgrade_stalled`; a tab whose stall is a stop keeps its
    /// own row beside the agent's.
    #[test]
    fn one_agents_overdue_tabs_are_one_row() {
        use aterm_messages::UpgradeWord;
        let (mut app, [a, b, c]) = three_tabs();
        let now = now_s();
        let hours = |h: u64| h * 3_600;
        // Tab 1 behind 26 h, its notice refused by aterm's own fence; tab 2
        // behind 7 h on a shell of its own; tab 3 behind 6 h, its turn
        // running.
        let refused = Row {
            wait: "announce-refused:changed".to_string(),
            refused: 3,
            refused_at: now - hours(5),
            ..row(&a, hours(26))
        };
        let working = Row {
            wait: "not-idle:shell".to_string(),
            ..row(&b, hours(7))
        };
        let turning = row(&c, hours(6));
        app.apply_agent_upgrades(vec![refused.clone(), working.clone(), turning.clone()]);
        let (id, group) = group_of(&app).expect("one row for the agent's tabs");
        assert_eq!(app.messages.live_rows().count(), 1, "one row, not three");
        for tab in [&a, &b, &c] {
            assert!(row_of(&app, tab).is_none(), "{tab}: no row of its own");
            assert!(
                app.messages.log().records().any(|r| r.key.as_deref()
                    == Some(message_reporters::agent_upgrade_key(tab).as_str())),
                "{tab}: its own entry is a record"
            );
        }
        assert_eq!(group.title, "Couldn't upgrade Claude in 3 tabs yet");
        assert!(
            aterm_messages::text::glass_title_fault(&group.title).is_none(),
            "{}",
            group.title
        );
        assert_eq!(
            group.detail[0],
            "tab 1 for 26 h \u{00b7} tab 2 for 7 h \u{00b7} tab 3 for 6 h"
        );
        assert!(
            group.detail[1].starts_with("Upgrade now moves 2 of them at their next pauses"),
            "{}",
            group.detail[1]
        );
        assert!(
            group
                .detail
                .iter()
                .any(|l| l.starts_with("tab 1 \u{00b7} Claude Code")
                    && l.contains("aterm could not type its notice")),
            "each tab's own words: {:#?}",
            group.detail
        );
        assert_eq!(
            message_reporters::sentence_lines(group.key.as_deref(), &group.detail),
            2
        );
        assert_eq!(
            group.actions,
            [
                Intent::AgentUpgradeTabs {
                    word: UpgradeWord::Now,
                    moves: vec![
                        (a.clone(), "2.1.282".to_string()),
                        (c.clone(), "2.1.282".to_string())
                    ],
                },
                Intent::AgentUpgradeTabs {
                    word: UpgradeWord::NotToday,
                    moves: [&a, &b, &c]
                        .iter()
                        .map(|t| ((*t).clone(), "2.1.282".to_string()))
                        .collect(),
                },
            ]
        );
        assert_eq!(
            message_reporters::agent_upgrade_group_members(&group.detail),
            [&a, &b, &c].map(|t| ((*t).clone(), "2.1.282".to_string())),
            "the shell line names every tab's id, with its target"
        );

        // A tab finishes: the same row, its count down.
        app.apply_agent_upgrades(vec![refused.clone(), working.clone()]);
        let (again, two) = group_of(&app).expect("still the agent's row");
        assert_eq!(again, id, "restated in place, never a new row");
        assert_eq!(two.title, "Couldn't upgrade Claude in 2 tabs yet");
        // One left: its own words — its buttons the row's own word for that
        // one tab, and its shell line naming the stall it stands for, so a
        // press answers THIS row and a restart reads it back as the agent's.
        app.apply_agent_upgrades(vec![working.clone()]);
        let (again, one) = group_of(&app).expect("still the agent's row");
        assert_eq!(again, id);
        let own = message_reporters::agent_upgrade_stalled(&working, now_s(), "in tab 2");
        assert_eq!(one.title, own.title, "one tab reads as its own row");
        let (shell, words) = one.detail.split_last().expect("lines");
        assert_eq!(words, &own.detail[..own.detail.len() - 1]);
        assert_eq!(
            shell,
            &format!(
                "{} \u{2014} tabs {}@2.1.282",
                own.detail.last().expect("its shell line"),
                b
            )
        );
        assert_eq!(
            one.actions.iter().map(Intent::label).collect::<Vec<_>>(),
            own.actions.iter().map(Intent::label).collect::<Vec<_>>()
        );
        assert!(
            one.actions.iter().all(|intent| matches!(
                intent,
                Intent::AgentUpgradeTabs { moves, .. } if moves == &[(b.clone(), "2.1.282".to_string())]
            )),
            "{:?}",
            one.actions
        );
        // The last one gone: resolved.
        app.apply_agent_upgrades(vec![]);
        assert!(group_of(&app).is_none());
        assert_eq!(
            app.messages.log().get(id).map(|r| r.state.clone()),
            Some(aterm_messages::LogState::Retired(
                aterm_messages::Retired::Resolved(Outcome::Ok)
            ))
        );

        // NEGATIVE CONTROL: a tab alone from the start keeps its own row.
        let (mut lone, [a, _, _]) = three_tabs();
        let alone = row(&a, hours(7));
        lone.apply_agent_upgrades(vec![alone.clone()]);
        assert!(group_of(&lone).is_none());
        let (_, own) = row_of(&lone, &a).expect("its own row");
        assert_eq!(
            own.title,
            message_reporters::agent_upgrade_stalled(&alone, now_s(), "in tab 1").title
        );
        // NEGATIVE CONTROL: a stop keeps its own row beside the agent's.
        let (mut mixed, [a, b, c]) = three_tabs();
        let stopped = Row {
            phase: Phase::Failed("not-a-shell-job".to_string()),
            ..row(&c, 60)
        };
        mixed.apply_agent_upgrades(vec![row(&a, hours(8)), row(&b, hours(9)), stopped]);
        assert!(group_of(&mixed).is_some(), "the two overdue tabs' row");
        assert!(row_of(&mixed, &c).is_some(), "the stop's own row");
        assert_eq!(mixed.messages.live_rows().count(), 2);
    }

    /// A WORD PRESSED ON AN AGENT'S ONE ROW IS EACH LISTED TAB'S WORD, and the
    /// row is answered once: `Not today` reaches the writer for every tab the
    /// row named, for the build it named each with, and the row leaves the
    /// band answered in words that say so — never a `✓`. NEGATIVE CONTROL: a
    /// tab that joined after the row was drawn is not in the press.
    #[test]
    fn a_word_on_an_agents_row_is_each_listed_tabs_word() {
        use aterm_messages::UpgradeWord;
        let (mut app, [a, b, c]) = three_tabs();
        let (writer, asked) = recording(|ask: &WordAsk| {
            Ok(Row {
                request: Request::DeferUntil(now_s() + NOT_TODAY_S),
                request_at: now_s(),
                ..row(&ask.tab, 7 * 3_600)
            })
        });
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600)]);
        let (id, group) = group_of(&app).expect("the agent's row");
        // Never `… upgrade waits in 2 tabs` (ruling 380): a row could not be
        // done yet.
        assert_eq!(group.title, "Couldn't upgrade Claude in 2 tabs yet");
        // A third tab stalls after the row was drawn: not in the press.
        let _ = c;
        assert_eq!(
            press(&mut app, id, "Not today"),
            "OK acted=Not%20today performed=1"
        );
        let asks: Vec<WordAsk> = std::iter::from_fn(|| asked.try_recv().ok()).collect();
        assert!(
            asks.iter()
                .all(|ask| ask.word == UpgradeWord::NotToday && ask.to == "2.1.282"),
            "{asks:?}"
        );
        let mut tabs: Vec<String> = asks.into_iter().map(|ask| ask.tab).collect();
        tabs.sort();
        let mut want = vec![a.clone(), b.clone()];
        want.sort();
        assert_eq!(tabs, want, "each listed tab's word, once");
        assert!(group_of(&app).is_none(), "answered: off the band");
        assert_eq!(
            app.messages.log().get(id).map(|r| r.state.clone()),
            Some(aterm_messages::LogState::Retired(
                aterm_messages::Retired::Answered {
                    label: "Not today".to_string()
                }
            ))
        );
        assert_eq!(
            record_of(&app, id).0,
            "Claude waits until tomorrow in 2 tabs"
        );
    }

    /// AN AGENT'S ROW THE OWNER PUT DOWN IS NOT POSTED AGAIN (the once-only
    /// rule of 2026-09-27, for the one row): dismissed, it stays down in this
    /// process, and the next process reads the tabs it named back from its
    /// record and keeps it down while every tab still stalling is among them.
    /// NEGATIVE CONTROL: a tab that was not among them joining is news — a new
    /// row.
    #[test]
    fn an_agents_row_the_owner_saw_is_not_posted_again_after_a_restart() {
        let dir = std::env::temp_dir().join(format!("aterm-upgrade-group-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join(crate::messages_store::FILE_NAME);
        let (mut first, [a, b, c]) = three_tabs();
        first.messages_log = crate::messages_store::Writer::spawn(&path);
        first.apply_agent_upgrades(vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600)]);
        let (id, _) = group_of(&first).expect("the agent's row");
        assert!(first.messages.dismiss(id, std::time::Instant::now()));
        first.sync_messages();
        // In this process too: the same tabs stay down.
        first.apply_agent_upgrades(vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600)]);
        assert!(group_of(&first).is_none(), "dismissed: down");
        first.flush_messages_log();
        drop(first.messages_log.take());

        let relaunched = || {
            let (mut next, _) = three_tabs();
            next.messages = aterm_messages::MessageCenter::new(
                crate::messages_store::load_tail(&path, u64::MAX).log,
                std::time::Instant::now(),
            );
            next
        };
        let mut next = relaunched();
        next.apply_agent_upgrades(vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600)]);
        assert!(group_of(&next).is_none(), "the owner dismissed it: down");
        // NEGATIVE CONTROL: a tab it did not name joins — news.
        next.apply_agent_upgrades(vec![
            row(&a, 7 * 3_600),
            row(&b, 8 * 3_600),
            row(&c, 9 * 3_600),
        ]);
        let (_, back) = group_of(&next).expect("a new tab: a new row");
        assert_eq!(back.title, "Couldn't upgrade Claude in 3 tabs yet");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// WHAT THE OWNER SAW OF AN AGENT'S ROW IS ITS STALLS, NOT ITS TABS (the
    /// review of 2026-09-28): a row for 2.1.282 the owner dismissed kept the
    /// same tabs down for good — overdue again days later on 2.1.283, or the
    /// same stall back after it had ended, nothing reached the band, in this
    /// process or after a restart. Now a tab's newer target is news, and a
    /// row down already whose stalls end leaves a record of their end under
    /// its key, so the same stalls back are news too — after a restart as in
    /// this process. NEGATIVE CONTROL: the same stalls, still standing, stay
    /// down.
    #[test]
    fn an_agents_row_the_owner_saw_is_news_again_for_a_newer_build_or_a_stall_back() {
        let on = |tab: &str, to: &str| Row {
            to: to.to_string(),
            ..row(tab, 7 * 3_600)
        };
        // A newer target, mid-stall.
        let (mut app, [a, b, _]) = three_tabs();
        app.apply_agent_upgrades(vec![on(&a, "2.1.282"), on(&b, "2.1.282")]);
        let (id, _) = group_of(&app).expect("the agent's row");
        assert!(app.messages.dismiss(id, std::time::Instant::now()));
        app.apply_agent_upgrades(vec![on(&a, "2.1.282"), on(&b, "2.1.282")]);
        assert!(
            group_of(&app).is_none(),
            "NEGATIVE CONTROL: the same stalls stay down"
        );
        app.apply_agent_upgrades(vec![on(&a, "2.1.283"), on(&b, "2.1.283")]);
        let (again, newer) = group_of(&app).expect("a newer build: news");
        assert_ne!(again, id);
        assert!(
            newer.detail.iter().any(|l| l.contains("2.1.283")),
            "{:?}",
            newer.detail
        );

        // The same stalls back after they ended, across a restart.
        let dir =
            std::env::temp_dir().join(format!("aterm-upgrade-group-over-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join(crate::messages_store::FILE_NAME);
        let (mut first, [a, b, _]) = three_tabs();
        first.messages_log = crate::messages_store::Writer::spawn(&path);
        let stalls = vec![on(&a, "2.1.282"), on(&b, "2.1.282")];
        first.apply_agent_upgrades(stalls.clone());
        let (id, _) = group_of(&first).expect("the agent's row");
        assert!(first.messages.dismiss(id, std::time::Instant::now()));
        first.sync_messages();
        // Both tabs stop stalling: the end is on the record, under the key.
        first.apply_agent_upgrades(vec![row(&a, 60), row(&b, 60)]);
        let key = message_reporters::agent_upgrade_group_key(
            aterm_agent::harness::upgrade::Agent::Claude,
        );
        let over = first
            .messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(key.as_str()))
            .map(|r| r.title.clone());
        assert_eq!(over.as_deref(), Some("Claude upgrade wait ended in 2 tabs"));
        first.flush_messages_log();
        drop(first.messages_log.take());
        let (mut next, _) = three_tabs();
        next.messages = aterm_messages::MessageCenter::new(
            crate::messages_store::load_tail(&path, u64::MAX).log,
            std::time::Instant::now(),
        );
        next.apply_agent_upgrades(stalls);
        assert!(
            group_of(&next).is_some(),
            "the same stalls back after they ended: news"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AN AGENT'S ROW STAYS THE AGENT'S ACROSS A RESTART, AND THE STALLS THE
    /// OWNER SAW ON THE TABS' OWN ROWS STAY SEEN (the review of 2026-09-28).
    /// Three cases, each the failure the owner reported on 2026-09-27 in
    /// another shape: a dismissed row of two tabs, one still stalling after
    /// the restart — its own row came back; a one-tab row still on the band,
    /// carried across a handoff — withdrawn for a fresh row of the tab's own;
    /// and two tabs whose OWN rows the owner had dismissed, grouped for the
    /// first time by this build — a new row for stalls already seen. Now none
    /// posts anything, and the carried row is adopted, the same id. NEGATIVE
    /// CONTROL: a tab whose own row the owner never saw joins — news.
    #[test]
    fn an_agents_row_stays_the_agents_across_a_restart_and_seen_stalls_stay_seen() {
        let dir =
            std::env::temp_dir().join(format!("aterm-upgrade-group-alone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join(crate::messages_store::FILE_NAME);
        let relaunched = || {
            let (mut next, _) = three_tabs();
            next.messages = aterm_messages::MessageCenter::new(
                crate::messages_store::load_tail(&path, u64::MAX).log,
                std::time::Instant::now(),
            );
            next
        };

        // One tab of a dismissed row left after a restart.
        let (mut first, [a, b, c]) = three_tabs();
        first.messages_log = crate::messages_store::Writer::spawn(&path);
        first.apply_agent_upgrades(vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600)]);
        let (id, _) = group_of(&first).expect("the agent's row");
        assert!(first.messages.dismiss(id, std::time::Instant::now()));
        first.sync_messages();
        first.flush_messages_log();
        drop(first.messages_log.take());
        let mut next = relaunched();
        next.apply_agent_upgrades(vec![row(&a, 7 * 3_600)]);
        assert_eq!(
            next.messages.live_rows().count(),
            0,
            "one tab of the row the owner dismissed: no row of its own"
        );
        let _ = std::fs::remove_file(&path);

        // A one-tab row carried across a handoff: adopted, never withdrawn.
        let (mut parent, [a, b, _]) = three_tabs();
        parent.apply_agent_upgrades(vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600)]);
        parent.apply_agent_upgrades(vec![row(&a, 7 * 3_600)]);
        let (carried_id, one) = group_of(&parent).expect("the one-tab row");
        let carry = parent.messages.carried();
        let mut successor = App::headless_for_test();
        successor.messages.seed_carried(
            &carry,
            crate::messages_host::wall_stamp_now(),
            std::time::Instant::now(),
        );
        successor.apply_agent_upgrades(vec![row(&a, 7 * 3_600)]);
        let live: Vec<_> = successor
            .messages
            .live_rows()
            .map(|l| (l.id, l.msg.key.clone()))
            .collect();
        assert_eq!(
            live,
            [(carried_id, one.key.clone())],
            "the carried row, adopted"
        );

        // Two tabs whose own rows the owner dismissed, grouped for the first
        // time after a restart.
        let (mut first, _) = three_tabs();
        first.messages_log = crate::messages_store::Writer::spawn(&path);
        for tab in [&a, &b] {
            let own = message_reporters::agent_upgrade_stalled(
                &row(tab, 7 * 3_600),
                now_s(),
                "in its tab",
            );
            let id = first.post_message(own);
            assert!(first.messages.dismiss(id, std::time::Instant::now()));
        }
        first.sync_messages();
        first.flush_messages_log();
        drop(first.messages_log.take());
        let mut next = relaunched();
        next.apply_agent_upgrades(vec![row(&a, 7 * 3_600), row(&b, 7 * 3_600)]);
        assert_eq!(
            next.messages.live_rows().count(),
            0,
            "both stalls seen on their own rows: nothing posted"
        );
        // NEGATIVE CONTROL: a tab the owner never saw joins.
        next.apply_agent_upgrades(vec![
            row(&a, 7 * 3_600),
            row(&b, 7 * 3_600),
            row(&c, 7 * 3_600),
        ]);
        let (_, news) = group_of(&next).expect("an unseen stall joined: news");
        assert_eq!(news.title, "Couldn't upgrade Claude in 3 tabs yet");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AN AGENT'S ROW ENDS BY WHAT BECAME OF ITS TABS, NEVER A BARE `✓` (the
    /// review of 2026-09-28; rulings 270, 283 and day five's D26 for a tab's
    /// row). Its one tab's word — pressed on the row, or from the tab's menu —
    /// ANSWERS the row with the word's words; a restart that only began keeps
    /// the row; tabs that moved end it in finished words. NEGATIVE CONTROLS:
    /// the builds before resolved each of these `Ok` with the words it stalled
    /// with (`Couldn't upgrade Claude …`).
    #[test]
    fn an_agents_row_ends_by_what_became_of_its_tabs() {
        use aterm_messages::{LogState, Retired, UpgradeWord};
        let answered =
            |app: &App, id: MessageId| app.messages.log().get(id).map(|r| r.state.clone());
        let deferred = |ask: &WordAsk| {
            Ok(Row {
                request: Request::DeferUntil(now_s() + NOT_TODAY_S),
                request_at: now_s(),
                ..row(&ask.tab, 7 * 3_600)
            })
        };
        // Pressed on the row of its one tab.
        let (mut app, [a, b, _]) = three_tabs();
        let (writer, _) = recording(deferred);
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600)]);
        app.apply_agent_upgrades(vec![row(&a, 7 * 3_600)]);
        let (id, _) = group_of(&app).expect("the one-tab row");
        assert_eq!(
            press(&mut app, id, "Not today"),
            "OK acted=Not%20today performed=1"
        );
        assert_eq!(
            answered(&app, id),
            Some(LogState::Retired(Retired::Answered {
                label: "Not today".to_string()
            }))
        );
        assert_eq!(
            record_of(&app, id).0,
            "Claude waits until tomorrow in 1 tab"
        );
        assert_eq!(app.messages.live_rows().count(), 0);

        // The tab's own menu word, on the last tab of the row.
        let (mut app, [a, b, _]) = three_tabs();
        let (writer, _) = recording(deferred);
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600)]);
        app.apply_agent_upgrades(vec![row(&a, 7 * 3_600)]);
        let (id, _) = group_of(&app).expect("the one-tab row");
        app.send_upgrade_word(WordAsk {
            tab: a.clone(),
            to: "2.1.282".to_string(),
            word: UpgradeWord::NotToday,
        });
        assert_eq!(
            answered(&app, id),
            Some(LogState::Retired(Retired::Answered {
                label: "Not today".to_string()
            })),
            "the row the owner's word moved is answered, never ✓"
        );
        assert_eq!(
            record_of(&app, id).0,
            "Claude stays on 2.1.281 until tomorrow"
        );
        assert_eq!(app.messages.live_rows().count(), 0);
        // NEGATIVE CONTROL (round 35 review, ruling 376): the owner's word is
        // the tab's stall's end — no `… asks again …` or `… no longer waits
        // …` record under the tab's key after it (the build of D3 wrote
        // `Claude upgrade asks again in tab 1` right after `Not today`).
        let key = message_reporters::agent_upgrade_key(&a);
        let after: Vec<String> = app
            .messages
            .log()
            .records()
            .filter(|r| r.key.as_deref() == Some(key.as_str()))
            .map(|r| r.title.clone())
            .collect();
        assert!(
            after
                .iter()
                .all(|t| !t.contains("asks again") && !t.contains("no longer waits")),
            "{after:?}"
        );
        // The same word refused (another sweep held the lock): said on that
        // row, which keeps its buttons — never a row of its own beside it.
        let (mut app, [a, b, _]) = three_tabs();
        let (writer, _) = recording(|_| Err("busy:another-sweep".to_string()));
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600)]);
        app.apply_agent_upgrades(vec![row(&a, 7 * 3_600)]);
        let (id, before) = group_of(&app).expect("the one-tab row");
        app.send_upgrade_word(WordAsk {
            tab: a.clone(),
            to: "2.1.282".to_string(),
            word: UpgradeWord::NotToday,
        });
        assert_eq!(app.messages.live_rows().count(), 1);
        let (still, refused) = group_of(&app).expect("the row, still up");
        assert_eq!(still, id);
        assert_eq!(refused.detail[0], "press it again");
        assert_eq!(refused.actions, before.actions);

        // A restart that only began: the row stays; the move landed: finished
        // words.
        let (mut app, [a, b, _]) = three_tabs();
        app.apply_agent_upgrades(vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600)]);
        let (id, _) = group_of(&app).expect("the agent's row");
        let flying = |tab: &str| Row {
            phase: Phase::Exiting { at_s: now_s() },
            ..row(tab, 7 * 3_600)
        };
        app.apply_agent_upgrades(vec![flying(&a), flying(&b)]);
        assert!(
            app.messages.live(id).is_some(),
            "a restart in flight is no recovery yet"
        );
        let done = |tab: &str| Row {
            phase: Phase::Done,
            done_at: now_s(),
            ..row(tab, 7 * 3_600)
        };
        app.apply_agent_upgrades(vec![done(&a), done(&b)]);
        assert_eq!(
            answered(&app, id),
            Some(LogState::Retired(Retired::Resolved(Outcome::Ok)))
        );
        assert_eq!(record_of(&app, id).0, "Claude upgraded in 2 tabs");
    }

    /// A PRESS ON AN AGENT'S ROW IS ANSWERED ONLY ONCE A WORD IS WRITTEN, AND
    /// ITS REFUSALS ARE SAID ON THE ROW (the review of 2026-09-28): the row
    /// was answered before any word was written, and a lock another sweep
    /// held refused each tab's word in turn — an error row per tab, and the
    /// tabs, stalling still, a new row of the agent's, under the row answered
    /// as if it had worked. Every word refused: the ONE row says why, keeps its
    /// buttons, and stays on the band — one live row. Some written: the row is
    /// answered with the tabs that took it, and names the ones that did not.
    #[test]
    fn a_press_on_an_agents_row_is_answered_only_once_a_word_is_written() {
        use aterm_messages::{LogState, Retired};
        // Every word refused: the lock another sweep held.
        let (mut app, [a, b, c]) = three_tabs();
        let (writer, asked) = recording(|_| Err("busy:another-sweep".to_string()));
        app.upgrade_view.writer = writer;
        let stalls = vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600), row(&c, 9 * 3_600)];
        app.apply_agent_upgrades(stalls.clone());
        let (id, before) = group_of(&app).expect("the agent's row");
        assert_eq!(
            press(&mut app, id, "Not today"),
            "OK acted=Not%20today performed=1"
        );
        assert_eq!(std::iter::from_fn(|| asked.try_recv().ok()).count(), 3);
        app.apply_agent_upgrades(stalls.clone());
        assert!(
            app.messages.live_rows().count() <= 1,
            "{:?}",
            app.messages
                .live_rows()
                .map(|l| l.msg.title.clone())
                .collect::<Vec<_>>()
        );
        let (still, refused) = group_of(&app).expect("the row pressed, still up");
        assert_eq!(still, id);
        assert_eq!(refused.title, "Couldn't postpone the upgrade in 3 tabs");
        assert_eq!(refused.detail[0], "press it again");
        assert_eq!(refused.actions, before.actions, "its buttons kept");

        // Some written, one refused: answered with the two, naming the one.
        let (mut app, [a, b, c]) = three_tabs();
        let refused_tab = c.clone();
        let (writer, _) = recording(move |ask: &WordAsk| {
            if ask.tab == refused_tab {
                Err("busy:another-sweep".to_string())
            } else {
                Ok(Row {
                    request: Request::DeferUntil(now_s() + NOT_TODAY_S),
                    request_at: now_s(),
                    ..row(&ask.tab, 7 * 3_600)
                })
            }
        });
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![
            row(&a, 7 * 3_600),
            row(&b, 8 * 3_600),
            row(&c, 9 * 3_600),
        ]);
        let (id, _) = group_of(&app).expect("the agent's row");
        press(&mut app, id, "Not today");
        assert_eq!(
            app.messages.log().get(id).map(|r| r.state.clone()),
            Some(LogState::Retired(Retired::Answered {
                label: "Not today".to_string()
            }))
        );
        let (title, _, detail) = record_of(&app, id);
        assert_eq!(title, "Claude waits until tomorrow in 2 tabs");
        assert!(
            detail
                .iter()
                .any(|l| l == "not written for 1 tab: press it again"),
            "{detail:?}"
        );
    }

    /// A NOTICE ATERM COULD NOT TYPE, UNDER THE OWNER'S `Upgrade now` (the
    /// review of 2026-09-28): the row offered `Upgrade now`, and pressed, the
    /// same row came back at once — the refusal count the stall is told by
    /// stood through the word. The word starts it over (`St::new_round`, the
    /// row the writer hands back), so the answered row stays answered. And a
    /// refusal still standing ninety minutes after the word says aterm's
    /// fence is what fails, offering what the owner can still do — never
    /// `Upgrade now` again.
    #[test]
    fn upgrade_now_on_a_notice_aterm_could_not_type_is_not_put_back() {
        use aterm_messages::UpgradeWord;
        let (mut app, [a, _, _]) = three_tabs();
        let now = now_s();
        let refused = Row {
            wait: "announce-refused:changed".to_string(),
            refused: 5,
            refused_at: now - upgrade_drive::TELL_S - 60,
            ..row(&a, 3 * 3_600)
        };
        let (writer, _) = recording(|ask: &WordAsk| {
            Ok(Row {
                wait: "announce-refused:changed".to_string(),
                request: Request::Now,
                request_at: now_s(),
                refused: 0,
                refused_at: 0,
                ..row(&ask.tab, 3 * 3_600)
            })
        });
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![refused.clone()]);
        let (id, shown) = row_of(&app, &a).expect("aterm's own failure, told");
        assert!(
            shown
                .actions
                .iter()
                .any(|i| i.label() == UpgradeWord::Now.label()),
            "no word in force: Upgrade now offered"
        );
        press(&mut app, id, UpgradeWord::Now.label());
        assert_eq!(
            app.messages.live_rows().count(),
            0,
            "answered, and not put back"
        );
        // Still refused ninety minutes after the word.
        let later = Row {
            request: Request::Now,
            request_at: now - upgrade_drive::TELL_S - 120,
            refused: 4,
            refused_at: now - upgrade_drive::TELL_S - 60,
            ..refused
        };
        app.apply_agent_upgrades(vec![later]);
        let (_, told) = row_of(&app, &a).expect("told again, as aterm's own");
        assert!(
            !told
                .actions
                .iter()
                .any(|i| i.label() == UpgradeWord::Now.label()),
            "{:?}",
            told.actions
        );
        assert!(
            told.detail
                .iter()
                .any(|l| l.contains("what fails is aterm typing its notice")),
            "{:?}",
            told.detail
        );
    }

    fn upgrade_column(app: &App, local: u64) -> String {
        let store = app.store.read().unwrap();
        let h = store.by_local(local).expect("registered");
        let (_, cols) = h.ctx.timeline.lock().unwrap().owner_columns(false, now_s());
        cols.rsplit(' ').next().unwrap_or_default().to_string()
    }

    /// Each tab's `upgrade=` column is ITS row; a tab with none reads `-`, and
    /// a row that leaves the summary leaves the column.
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

    /// THE MANAGED RECORD'S COUNTS (gap audit 2026-09-24): a frozen tab with
    /// an agent in front is never counted for the hook to type — it is an
    /// agent tab, or healed by the move when the upgrade is moving it — and
    /// the sessions the upgrade moves are counted. NEGATIVE CONTROL: the same
    /// frozen tab at its shell is counted for the hook.
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
        front(&app, a, "claude");
        assert_eq!(
            app.tabs_behind(),
            TabsBehind {
                frozen_shells: 1,
                frozen_agents: 1,
                ..TabsBehind::default()
            }
        );
        let text = "claude 2.1.282 (Anthropic latest)";
        app.upgrade_view.note_managed_text(text);
        app.apply_agent_upgrades(vec![row(&sid_a, 60)]);
        assert_eq!(
            app.tabs_behind(),
            TabsBehind {
                frozen_shells: 1,
                agents_behind: 1,
                ..TabsBehind::default()
            },
            "the move heals it: counted as moving, not as a tab to type into"
        );
        // The rows moved the counts, so the window re-derived the record from
        // the remembered text itself: a second post is no news.
        assert!(!app.post_managed_current(text), "already recorded");
        let (title, detail) = app
            .messages
            .log()
            .records()
            .rfind(|r| r.key.as_deref() == Some(crate::toolchain_words::KEY_MANAGED))
            .map(|r| (r.title.clone(), r.detail.join(" ")))
            .expect("the record");
        assert_eq!(title, "Claude Code 2.1.282 installed", "not up to date");
        assert!(
            detail.contains("1 running Claude Code session moves onto it on its own"),
            "{detail}"
        );
        assert!(
            detail.contains("1 tab from before this update picks them up with `"),
            "{detail}"
        );
    }

    /// ONLY A SESSION THAT WILL MOVE ON ITS OWN, ONTO THE VERSION THE RECORD
    /// NAMES, IS SAID TO MOVE AT ITS NEXT TURN END (review of 2026-09-25: an
    /// overdue session was counted so while the band showed it stalled, and
    /// the count took rows moving to any version). An overdue, gave-up or
    /// skipped session is counted as staying behind — the title still says
    /// "installed" — and a row moving to ANOTHER version is not this record's
    /// at all; one moving onto the NATIVE copy of the named version lags it
    /// just the same and is counted (the measured 2.1.281 session was native).
    /// NEGATIVE CONTROL: a healthy pending row onto the named version is
    /// counted as moving.
    #[test]
    fn only_a_session_moving_on_its_own_onto_the_named_build_is_said_to_move() {
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
        ]);
        assert_eq!(
            app.tabs_behind(),
            TabsBehind {
                agents_behind: 2,
                agents_held: 3,
                ..TabsBehind::default()
            },
            "healthy + native move; overdue, gave-up, skipped stay; 2.1.283 is not this record's"
        );
        assert!(!app.post_managed_current(text), "re-derived by the rows");
        let (title, detail) = app
            .messages
            .log()
            .records()
            .rfind(|r| r.key.as_deref() == Some(crate::toolchain_words::KEY_MANAGED))
            .map(|r| (r.title.clone(), r.detail.join(" ")))
            .expect("the record");
        assert_eq!(title, "Claude Code 2.1.282 installed");
        assert!(
            detail.contains("2 running Claude Code sessions move onto it on their own"),
            "{detail}"
        );
        assert!(
            detail.contains("3 running Claude Code sessions stay behind it"),
            "{detail}"
        );
        // With no managed record yet there is no build to count against.
        let mut fresh = App::headless_for_test();
        fresh.apply_agent_upgrades(vec![row("s-healthy", 60)]);
        assert_eq!(fresh.tabs_behind(), TabsBehind::default());
    }

    /// A TAB MEASURED TO PUT A FOREIGN COPY FIRST, two jobs in a row, with no
    /// adoption mark, is counted as shadowed — never as "from before this
    /// update" (review of 2026-09-25: one job's `PATH=` override raised the
    /// mark and the band said so). NEGATIVE CONTROL: one job's frozen reading
    /// counts nothing.
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

    /// A FINISHED MOVE is one record, with the model its first answer named;
    /// the same finish handed over again is not recorded twice.
    #[test]
    fn a_finished_move_is_recorded_once_with_its_model() {
        let mut app = App::headless_for_test();
        let done = Row {
            phase: Phase::Done,
            done_at: now_s() - 5,
            outcome: "claude restarted on 2.1.282 · model claude-opus-5-5".to_string(),
            ..row("s-a", 600)
        };
        app.apply_agent_upgrades(vec![done.clone()]);
        app.apply_agent_upgrades(vec![done]);
        let records = harness_records(&app);
        assert_eq!(
            records,
            vec![(
                "Claude Code moved onto 2.1.282".to_string(),
                vec![
                    "in its tab".to_string(),
                    "model claude-opus-5-5".to_string()
                ]
            )]
        );
        assert_eq!(app.messages.live_rows().count(), 0, "a record, never a row");
    }

    // ------------------------------------------------ the owner's word (gap #21)

    /// A [`WordWriter`] that answers `answer` and hands every word it is
    /// asked to write to the returned receiver.
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

    /// The live band row keyed for `tab`'s upgrade.
    fn row_of(app: &App, tab: &str) -> Option<(MessageId, aterm_messages::Message)> {
        let key = format!("harness.upgrade.{tab}");
        app.messages
            .live_rows()
            .find(|l| l.msg.key.as_deref() == Some(key.as_str()))
            .map(|l| (l.id, l.msg.clone()))
    }

    /// RULING 283 — ONE ENTRY PER STOPPED STORY. A round that stops once and
    /// will ask again on its own is a RECORD (no row, no tab mark); the same
    /// stop again raises ONE row, which stands through the re-armed round,
    /// its notice and its restart in flight, and the next identical stop —
    /// never resolved at a re-arm and posted anew a round later (the audit's
    /// replay: failed → rest → re-arm → failed). It resolves at Done.
    /// NEGATIVE CONTROL: before the fix the re-armed round read no stall, its
    /// row was resolved `Ok`, and the next stop posted a new id.
    #[test]
    fn a_stop_that_repeats_keeps_one_row_across_its_rearms_and_resolves_at_done() {
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

        // The first stop: a record that says when it asks again, no row.
        app.apply_agent_upgrades(vec![at(failed(), 1, "")]);
        assert!(row_of(&app, "s-a").is_none(), "a first stop is no row");
        assert!(
            harness_records(&app)
                .iter()
                .any(|(title, _)| title == "Claude upgrade retries later in its tab"),
            "{:?}",
            harness_records(&app)
        );
        // Re-armed and asking: nothing on the glass.
        app.apply_agent_upgrades(vec![at(Phase::Pending, 1, "no-resume")]);
        app.apply_agent_upgrades(vec![at(announced.clone(), 1, "")]);
        assert!(row_of(&app, "s-a").is_none());

        // The same stop again: one row.
        app.apply_agent_upgrades(vec![at(failed(), 2, "")]);
        let (id, first) = row_of(&app, "s-a").expect("a stop that repeats is a row");
        assert_eq!(first.title, "Couldn't upgrade Claude in its tab");
        assert!(
            first.detail[1].contains("stopped this way 2 times in a row"),
            "{:?}",
            first.detail
        );
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
            let live: Vec<_> = app.messages.live_rows().map(|l| l.id).collect();
            assert_eq!(live, vec![id], "{word}: the same one row");
        }
        // It moved at last: resolved, nothing live.
        app.apply_agent_upgrades(vec![Row {
            done_at: now,
            ..at(Phase::Done, 3, "")
        }]);
        assert!(row_of(&app, "s-a").is_none(), "resolved at Done");
        // Day five, D26: its record says it went through, under a ✓ — never
        // `⚠ Couldn't upgrade` beside the success record.
        let (title, severity, _) = record_of(&app, id);
        assert_eq!(title, "Claude upgraded in its tab");
        assert_eq!(severity, aterm_messages::Severity::Success);
    }

    /// RULING 283 THROUGH A RESTART THAT STICKS (ruling 327, the merge review
    /// of 2026-09-27): a stop that repeats keeps its ONE row through the
    /// re-armed round's restart in flight even when that restart stands past
    /// `STALE_S` — S2 (`stuck:<what>`) lays its words over that row in place,
    /// never withdrawing it for a new one, and once the restart moves again
    /// the row says the stop again; the same stop next is still that row, in
    /// its count's words, and Done resolves it. Before the fix the stuck
    /// restart withdrew the row for a new one at each word of its wait, the
    /// last of which the in-flight guard kept up through the relaunch its
    /// words denied, and the same stop next posted yet another.
    #[test]
    fn a_stop_that_repeats_keeps_its_one_row_through_a_restart_that_sticks() {
        let mut app = App::headless_for_test();
        let now = now_s();
        let stale = now - 400;
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
        app.apply_agent_upgrades(vec![at(failed(), 2, "")]);
        let (id, _) = row_of(&app, "s-a").expect("a stop that repeats is a row");
        app.apply_agent_upgrades(vec![at(Phase::Pending, 2, "no-resume")]);
        app.apply_agent_upgrades(vec![at(announced, 2, "")]);
        let said = |app: &App| -> String {
            let live: Vec<_> = app.messages.live_rows().map(|l| l.id).collect();
            assert_eq!(live, vec![id], "the same one row");
            row_of(app, "s-a").expect("live").1.detail.join(" | ")
        };
        let steps = [
            (at(Phase::Exiting { at_s: stale }, 2, ""), "has not exited"),
            (
                Row {
                    exited_at: now - 30,
                    ..at(Phase::Exiting { at_s: stale }, 2, "")
                },
                "it ended for the upgrade",
            ),
            (
                at(Phase::Relaunched { at_s: now }, 2, ""),
                "stopped this way 2 times in a row",
            ),
            (
                at(Phase::Relaunched { at_s: stale }, 2, ""),
                "the line that resumes it was typed",
            ),
            (at(failed(), 3, ""), "stopped this way 3 times in a row"),
            (
                at(Phase::Pending, 3, "no-resume"),
                "stopped this way 3 times in a row",
            ),
        ];
        for (step, words) in steps {
            let word = step.phase.word();
            app.apply_agent_upgrades(vec![step]);
            let detail = said(&app);
            assert!(detail.contains(words), "{word}: {detail}");
            if words.starts_with("stopped") {
                assert!(
                    !detail.contains("nothing is forced"),
                    "{word}: no stuck words once it moved: {detail}"
                );
            }
        }
        app.apply_agent_upgrades(vec![Row {
            done_at: now,
            ..at(Phase::Done, 3, "")
        }]);
        assert!(row_of(&app, "s-a").is_none(), "resolved at Done");
        assert_eq!(record_of(&app, id).0, "Claude upgraded in its tab");
    }

    /// A RESTART THAT STICKS WITH NO STALL BEFORE IT (S2 of the in-flight
    /// review; ruling 327) takes one row, which follows what it waits on while it is
    /// stuck (`exiting`, then `exited`: one row, restated in place) and is
    /// resolved once the restart moves again — never left up by the host's
    /// in-flight guard (ruling 283's, for a stall from before the restart)
    /// saying "has not exited" of an agent that exited and relaunched.
    /// NEGATIVE CONTROL: within `STALE_S` the restart is no stall and takes
    /// no row.
    #[test]
    fn a_restart_that_sticks_keeps_one_row_while_stuck_and_resolves_once_it_moves() {
        let mut app = App::headless_for_test();
        let now = now_s();
        let at = |phase: Phase| Row {
            phase,
            wait: String::new(),
            ..row("s-a", 60)
        };
        app.apply_agent_upgrades(vec![at(Phase::Exiting { at_s: now - 60 })]);
        assert!(row_of(&app, "s-a").is_none(), "within STALE_S, no row");
        app.apply_agent_upgrades(vec![at(Phase::Exiting { at_s: now - 400 })]);
        let (id, first) = row_of(&app, "s-a").expect("a restart that sticks is a row");
        assert!(
            first.detail[0].contains("has not exited"),
            "{:?}",
            first.detail
        );
        app.apply_agent_upgrades(vec![Row {
            exited_at: now - 30,
            ..at(Phase::Exiting { at_s: now - 400 })
        }]);
        let (again, exited) = row_of(&app, "s-a").expect("still stuck");
        assert_eq!(again, id, "one row while it is stuck");
        assert!(
            exited.detail[0].contains("it ended for the upgrade"),
            "{:?}",
            exited.detail
        );
        app.apply_agent_upgrades(vec![at(Phase::Relaunched { at_s: now })]);
        assert!(
            app.messages.live_rows().next().is_none(),
            "it moved: no row says it has not"
        );
    }

    /// A STUCK ROW PUT DOWN ENDS ON THE RECORD AS WHAT HAPPENED (ruling 327
    /// with ruling 307(d); the merge review of 2026-09-28). The person
    /// dismissed the row a stuck restart alone earned, and the restart then
    /// moved again, still in flight: the row is down, so the stall's end is
    /// a record under its key — `Claude upgrade moves again in its tab`, the
    /// stuck words as `was: …`. Before the fix it read `Claude upgrade asks
    /// again in its tab`, ruling 307's words for a tab whose round goes on,
    /// of a restart that asks nothing again: it carries on to Done, which
    /// adds nothing under the key. NEGATIVE CONTROL: the same stuck row left
    /// up (`s-b`) is resolved when its restart moves, and nothing is
    /// recorded under its key.
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
        let recorded = |app: &App, tab: &str| -> Vec<(String, Vec<String>)> {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(&*format!("harness.upgrade.{tab}")))
                .filter(|r| r.retired() == Some(&aterm_messages::Retired::Recorded))
                .map(|r| (r.title.clone(), r.detail.clone()))
                .collect()
        };
        let stuck = |tab| at(tab, Phase::Exiting { at_s: now - 400 });
        app.apply_agent_upgrades(vec![stuck(a), stuck(b)]);
        let (id, row_a) = row_of(&app, a).expect("a restart that sticks is a row");
        assert!(row_a.detail[0].contains("has not exited"), "{row_a:?}");
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
        let (title, detail) = &ended[0];
        assert_eq!(title, "Claude upgrade moves again in its tab", "{detail:?}");
        assert!(
            detail[0].starts_with("was: ") && detail[0].contains("has not exited"),
            "{detail:?}"
        );
        assert_eq!(
            recorded(&app, b),
            Vec::<(String, Vec<String>)>::new(),
            "a row left up is resolved, never recorded over"
        );

        let done = |tab| Row {
            done_at: now,
            ..at(tab, Phase::Done)
        };
        app.apply_agent_upgrades(vec![done(a), done(b)]);
        assert_eq!(recorded(&app, a), ended, "Done adds nothing under the key");
    }

    /// Press the capsule labelled `label` on row `id`, the way a click does
    /// (`notice act`: the engine's `act`, then the host's perform).
    fn press(app: &mut App, id: MessageId, label: &str) -> String {
        app.take_notice(aterm_messages::NoticeRequest::Act {
            id,
            press: aterm_messages::Press::Label(label.to_string()),
        })
    }

    /// THE OWNER STEERS THE UPGRADE FROM ITS ROW (gap #21: until now only
    /// `aterm harness upgrade <tab> --now|--defer|--skip`, typed into some
    /// other shell, could). A press of a stalled row's capsule reaches the
    /// writer — `upgrade_drive::ask_for` in production — with THAT row's tab,
    /// THAT row's target build and the word pressed, once; and the row
    /// leaves the band FIXED, its record re-titled with what the word did
    /// under ✓, its stall title kept below — a confirmation earns no row of
    /// its own (ruling 76) — and the host's next rows (the word landed, the
    /// stall is over) post no stall row again. NEGATIVE
    /// CONTROLS: the other tab's row is untouched and keeps its capsules, and
    /// its `Not today` reaches the writer as that tab's own deferral of a day.
    #[test]
    fn a_capsule_press_writes_that_tabs_word_for_that_build_and_its_row_says_so() {
        use aterm_messages::{Severity, UpgradeWord};
        let mut app = App::headless_for_test();
        let (a, b) = (row("s-a", 7 * 3_600), row("s-b", 7 * 3_600));
        let (writer, asked) = recording(|ask: &WordAsk| {
            let request = match ask.word {
                UpgradeWord::Now => Request::Now,
                UpgradeWord::NotToday => Request::DeferUntil(now_s() + NOT_TODAY_S),
                UpgradeWord::Skip => Request::Skip(ask.to.clone()),
            };
            Ok(Row {
                request,
                request_at: now_s(),
                ..row(&ask.tab, 7 * 3_600)
            })
        });
        app.upgrade_view.writer = writer;
        // One overdue tab at a time: two of one agent stand in ONE row of the
        // agent's (`one_agents_overdue_tabs_are_one_row`), so the other tab's
        // own row is posted once this one's word has moved it.
        app.apply_agent_upgrades(vec![a.clone()]);
        let (id_a, msg_a) = row_of(&app, "s-a").expect("s-a's stalled row");
        let labels: Vec<&str> = msg_a.actions.iter().map(Intent::label).collect();
        assert_eq!(labels, ["Upgrade now", "Not today"]);

        assert_eq!(
            press(&mut app, id_a, "Upgrade now"),
            "OK acted=Upgrade%20now performed=1"
        );
        assert_eq!(
            asked.try_recv(),
            Ok(WordAsk {
                tab: "s-a".to_string(),
                to: "2.1.282".to_string(),
                word: UpgradeWord::Now,
            })
        );
        assert!(asked.try_recv().is_err(), "one press, one word");
        assert!(
            row_of(&app, "s-a").is_none(),
            "the stall's row left the band"
        );
        let (title, severity, detail) = record_of(&app, id_a);
        // The rig's row waits on a notice behind a full queue (ruling 380):
        // `Upgrade now` types it once more (ruling 307 (c)).
        assert_eq!(
            title, "Claude is asked again in its tab",
            "answered, in the word's words"
        );
        // Ruling 270: a word is a choice, not delivered work or a fix — `ℹ`,
        // answered with its label, never `✓` and `took`.
        assert_eq!(severity, Severity::Info);
        assert_eq!(
            app.messages.log().get(id_a).map(|r| r.state.clone()),
            Some(aterm_messages::LogState::Retired(
                aterm_messages::Retired::Answered {
                    label: "Upgrade now".to_string()
                }
            ))
        );
        assert_eq!(
            detail.last().map(String::as_str),
            Some("Couldn't upgrade Claude in its tab yet"),
            "then what it said"
        );
        assert_eq!(
            upgrade_view_request(&app, "s-a"),
            Some(Request::Now),
            "the view took the row the word left"
        );
        // The host's next rows say the same: no stall row is posted again.
        let hurried = Row {
            request: Request::Now,
            request_at: now_s(),
            ..a
        };
        app.apply_agent_upgrades(vec![hurried, b.clone()]);
        assert!(row_of(&app, "s-a").is_none());
        assert_eq!(app.messages.live_rows().count(), 1);
        // Round 18, day four (D7): the press is its row's answer, never a
        // second record `… installs itself` (`… ready for 1 session` until
        // ruling 380).
        assert!(
            harness_records(&app)
                .iter()
                .all(|(title, _)| !title.contains("installs itself")),
            "{:?}",
            harness_records(&app)
        );

        // The other tab's row is its own.
        let (id_b, msg_b) = row_of(&app, "s-b").expect("s-b's stalled row");
        assert_eq!(msg_b.title, "Couldn't upgrade Claude in its tab yet");
        assert_eq!(msg_b.actions.len(), 2);
        assert_eq!(
            press(&mut app, id_b, "Not today"),
            "OK acted=Not%20today performed=1"
        );
        let deferral = asked.try_recv().expect("s-b's word");
        assert_eq!(
            (deferral.tab.as_str(), deferral.to.as_str(), deferral.word),
            ("s-b", "2.1.282", UpgradeWord::NotToday)
        );
        assert_eq!(deferral.ask(), Ask::Defer(24 * 3_600), "a day");
        assert!(row_of(&app, "s-b").is_none());
        assert_eq!(
            record_of(&app, id_b).0,
            "Claude stays on 2.1.281 until tomorrow"
        );
        // Ruling 270 (day three): the record offers only the words that
        // still do something — never the `Not today` just pressed.
        let kept: Vec<&str> = app
            .messages_state()
            .entries
            .iter()
            .find(|e| e.id == id_b.raw())
            .expect("the entry")
            .actions
            .iter()
            .map(|a| a.label)
            .collect();
        assert_eq!(kept, ["Upgrade now"], "{kept:?}");
    }

    fn upgrade_view_request(app: &App, tab: &str) -> Option<Request> {
        app.upgrade_view.row_for(tab).map(|r| r.request.clone())
    }

    /// Row `id`'s record: its title, mark and detail as the log keeps them.
    fn record_of(app: &App, id: MessageId) -> (String, aterm_messages::Severity, Vec<String>) {
        let rec = app.messages.log().get(id).expect("on record");
        (rec.title.clone(), rec.severity, rec.detail.clone())
    }

    /// A STALLED ROW'S CAPSULES FOLLOW WHAT MOVES IT NOW (review of gap #21):
    /// a row is posted once per stall and target, but an OVERDUE stall's
    /// remedy moves with its wait. Posted while the agent was at work, it
    /// offers `Upgrade now`; the same stall waiting on the agent's READY
    /// answer — which `--now` does not waive, and where a press would only
    /// arm a new notice round — must stop offering it, on the SAME row,
    /// restated in place (never a second row), as the tab's menu already
    /// does. NEGATIVE CONTROL: the host's same rows again leave the row
    /// untouched.
    #[test]
    fn an_overdue_rows_capsules_follow_what_moves_it_now() {
        use aterm_messages::UpgradeWord::{NotToday, Skip};
        let mut app = App::headless_for_test();
        let labels = |m: &aterm_messages::Message| -> Vec<&'static str> {
            m.actions.iter().map(Intent::label).collect()
        };
        app.apply_agent_upgrades(vec![row("s-a", 7 * 3_600)]);
        let (id, at_work) = row_of(&app, "s-a").expect("the stalled row");
        assert_eq!(labels(&at_work), ["Upgrade now", "Not today"]);

        let waits = Row {
            wait: "awaiting-ready".to_string(),
            ..row("s-a", 7 * 3_600)
        };
        app.apply_agent_upgrades(vec![waits.clone()]);
        let (same, now_row) = row_of(&app, "s-a").expect("still its row");
        assert_eq!(same, id, "restated in place, never posted again");
        assert_eq!(app.messages.live_rows().count(), 1);
        assert_eq!(labels(&now_row), ["Not today", "Skip version"]);
        assert!(
            now_row
                .detail
                .iter()
                .any(|l| l.contains("does not move it past that wait")),
            "its words say what it waits on: {:?}",
            now_row.detail
        );
        assert_eq!(
            app.upgrade_view.menu_for("s-a", now_s()).map(|m| m.words),
            Some(vec![NotToday, Skip]),
            "the band and the menu agree"
        );

        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![waits]);
        assert_eq!(app.messages.revision(), revision, "nothing to restate");
    }

    /// A STALL'S ROW FOLLOWS ITS SHAPE, NOT ONLY ITS KIND (ruling 279): an
    /// announced move on the agent's own work asks again on its own and is a
    /// record (D5); the same tab's turn then running makes it movable —
    /// `Upgrade now` moves it — and it takes a row at once, the tab's mark
    /// and the glass agreeing. Back to asking on its own, the row leaves the
    /// glass, withdrawn (never a `✓`: nothing moved). NEGATIVE CONTROLS: the
    /// same rows again change nothing; keyed on the kind alone (the code
    /// before 279) the movable stall had no row at all.
    #[test]
    fn a_stall_that_turns_movable_takes_a_row_and_one_asking_on_its_own_leaves_the_glass() {
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
        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![asking.clone()]);
        assert_eq!(app.messages.revision(), revision, "the same record, once");

        // Behind a full queue of its notices, which `Upgrade now` types once
        // more (ruling 380: a turn still running, hours on the ladder's last
        // rung, is the upgrade asking on its own, a record).
        let movable = Row {
            wait: "queued".to_string(),
            ..asking.clone()
        };
        assert!(!movable.asks_on_its_own(now));
        app.apply_agent_upgrades(vec![movable.clone()]);
        let (id, msg) = row_of(&app, "s-a").expect("a movable stall is a row");
        assert_eq!(msg.hold, aterm_messages::Hold::Standing);
        assert_eq!(msg.severity, aterm_messages::Severity::Warn);
        assert!(
            msg.actions
                .iter()
                .any(|a| a.label() == aterm_messages::UpgradeWord::Now.label()),
            "{:?}",
            msg.actions
        );
        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![movable]);
        assert_eq!(app.messages.revision(), revision, "the same row, once");

        app.apply_agent_upgrades(vec![asking]);
        assert!(row_of(&app, "s-a").is_none(), "back to a record");
        assert_eq!(app.messages.live_rows().count(), 0);
        assert_eq!(
            app.messages.log().get(id).and_then(|r| r.retired()),
            Some(&aterm_messages::Retired::Withdrawn),
            "withdrawn, never resolved as fixed"
        );
    }

    /// A WORD'S ANSWER NEVER BRINGS BACK A ROW THE HOST HAS SINCE DROPPED
    /// (review of gap #21): the word is written off this thread, and the
    /// host's own rows can land first — the tab closed, its conversation
    /// ended. The host hands over only a CHANGE, so a row the answer put
    /// back would stand in the view — the waiting record, Settings ▸
    /// Messages' buttons — until something else moved. What the word did is
    /// still recorded. NEGATIVE CONTROL: with the row still in the view, the
    /// answer's row takes its place at once.
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
        };
        app.apply_agent_upgrades(vec![row("s-a", 60), row("s-b", 60)]);
        // The host's rows land before the answer: s-a is gone.
        app.apply_agent_upgrades(vec![row("s-b", 60)]);
        app.apply_upgrade_word(&ask("s-a"), Ok(deferred("s-a")));
        assert!(
            app.upgrade_view.row_for("s-a").is_none(),
            "not brought back"
        );
        assert!(
            harness_records(&app)
                .iter()
                .any(|(title, _)| title == "Claude stays on 2.1.281 until tomorrow"),
            "what the word did is recorded"
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

    /// A TAB-MENU WORD IS FOR THE SESSION ITS MENU NAMED (review of gap #21):
    /// a split tab runs a session per pane, and its menu is composed for the
    /// FOCUSED one. Focus can move while the menu is open or its click is
    /// queued (the `pane` control verb, a script), and both panes' agents
    /// commonly move to the SAME managed build, so the harness's target
    /// check cannot catch a word re-resolved to the pane focused at dispatch
    /// time. The word goes to the session the owner saw. NEGATIVE CONTROL:
    /// a session no longer in that tab drops the word.
    #[test]
    fn a_tab_menu_word_is_for_the_session_its_menu_named() {
        use aterm_messages::UpgradeWord::Skip;
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let first = app.tab_session_id_text(wid, 0).expect("tab 0's session");
        let _ = app.split_active_stub_tab(wid);
        let second = app
            .tab_session_id_text(wid, 0)
            .expect("the focused pane's session");
        assert_ne!(first, second, "the new pane has the focus");
        let (writer, asked) = recording(|ask: &WordAsk| {
            Ok(Row {
                request: Request::Skip(ask.to.clone()),
                request_at: now_s(),
                ..row(&ask.tab, 60)
            })
        });
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![row(&first, 60), row(&second, 60)]);
        let skip_of = |app: &App| {
            app.windows[&wid]
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
                .expect("the skip row")
        };
        assert!(app.open_tab_context_menu(wid, 0, 5, false));
        let skip = skip_of(&app);
        // The focus moves to the other pane before the choice lands.
        assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Left));
        assert_eq!(app.tab_session_id_text(wid, 0).as_deref(), Some(&*first));
        app.activate_tab_menu_entry(wid, skip);
        assert_eq!(
            asked.try_recv().map(|a| a.tab),
            Ok(second.clone()),
            "the session the menu named"
        );

        // A session that left the tab: nothing is sent.
        let tab = app.windows[&wid].tab_set.tabs()[0].id;
        app.dispatch_tab_menu_upgrade(wid, tab, "s-gone".to_string(), "2.1.282".to_string(), Skip);
        assert!(asked.try_recv().is_err(), "dropped");
    }

    /// THE OWNER'S WORDS ARE WRITTEN ONE AT A TIME, IN THE ORDER PRESSED
    /// (review of gap #21). The writer can wait up to 10 s on the sweep lock
    /// (a step restarting some session holds it), and a row gives no sign
    /// meanwhile, so a second press — the same word again, or the owner's
    /// changed mind — comes while the first waits. Each word replaces the
    /// last, so the last pressed must be the last written: a thread per
    /// press raced the two for the lock, and the earlier word could land
    /// last. Here the first word waits as on a held lock while the second
    /// is pressed. The pass does not depend on timing (the second never
    /// enters the writer before the first leaves); a thread per press is
    /// caught whenever its second thread runs within the 200 ms.
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
                // The first word waits, as on a step holding the lock.
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
        };
        let mut queue = WordQueue::default();
        queue
            .send(ask(UpgradeWord::Now), &writer, deliver(answered.clone()))
            .expect("sent");
        queue
            .send(ask(UpgradeWord::NotToday), &writer, deliver(answered))
            .expect("sent");
        std::thread::sleep(std::time::Duration::from_millis(200));
        release.send(()).expect("the first word waits");
        let order: Vec<UpgradeWord> = (0..2).map(|_| answers.recv().expect("an answer")).collect();
        assert!(
            !overlapped.load(Ordering::SeqCst),
            "a second word entered the writer while the first waited"
        );
        assert_eq!(order, [UpgradeWord::Now, UpgradeWord::NotToday]);
    }

    /// A WORD THE HARNESS REFUSED IS SAID ON ITS ROW: nothing was written
    /// (here another step held the sweep lock), so the row stays the stall's
    /// — Warn, Standing, both capsules to press again — with why above what
    /// it said, and the host's same rows do not post it twice.
    #[test]
    fn a_refused_word_is_said_on_its_row_which_keeps_its_capsules() {
        use aterm_messages::Hold;
        let mut app = App::headless_for_test();
        let (writer, asked) = recording(|_| Err("busy:another-sweep".to_string()));
        app.upgrade_view.writer = writer;
        let stalled = vec![row("s-a", 7 * 3_600)];
        app.apply_agent_upgrades(stalled.clone());
        let (id, before) = row_of(&app, "s-a").expect("stalled row");
        assert_eq!(
            press(&mut app, id, "Upgrade now"),
            "OK acted=Upgrade%20now performed=1"
        );
        assert!(asked.try_recv().is_ok(), "the writer was asked");
        assert_eq!(
            app.upgrade_view.looks_asked, 0,
            "NEGATIVE CONTROL (D22): a busy refusal is no reason to look again"
        );
        let (same, said) = row_of(&app, "s-a").expect("still the stall's row");
        assert_eq!(same, id);
        assert_eq!(said.title, "Couldn't upgrade Claude in its tab now");
        assert!(
            said.detail[0].contains("press it again"),
            "{:?}",
            said.detail
        );
        // The remedy, then why (ruling 307), then what it said — and still
        // the same stall by its words, wherever the refusal put them.
        assert!(said.detail[1].contains("lock"), "{:?}", said.detail);
        assert_eq!(said.detail[2..], before.detail[..], "then what it said");
        assert!(message_reporters::same_stall(&said.detail, &before.detail));
        assert_eq!(said.actions, before.actions, "pressable again");
        assert_eq!(said.hold, Hold::Standing);
        app.apply_agent_upgrades(stalled);
        assert_eq!(app.messages.live_rows().count(), 1, "not posted twice");

        // Ruling 270: with no stalled row to restate (a tab-menu press, a
        // Settings record's button), the refused press is a ROW in the
        // gesture-failure shape — never only a quiet record.
        let mut app = App::headless_for_test();
        app.apply_upgrade_word(
            &WordAsk {
                tab: "s-z".to_string(),
                to: "2.1.282".to_string(),
                word: aterm_messages::UpgradeWord::NotToday,
            },
            Err("busy:another-sweep".to_string()),
        );
        let refused: Vec<_> = app.messages.live_rows().map(|l| l.msg.clone()).collect();
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0].title, "Couldn't postpone the upgrade in its tab");
        // The press itself is the remedy (ruling 307): the row carries it.
        assert_eq!(
            refused[0].actions,
            [aterm_messages::Intent::AgentUpgrade {
                tab: "s-z".to_string(),
                to: "2.1.282".to_string(),
                word: aterm_messages::UpgradeWord::NotToday,
            }]
        );
        assert_eq!(refused[0].severity, aterm_messages::Severity::Error);
        assert_eq!(
            refused[0].hold,
            Hold::For(aterm_messages::HOLD_GESTURE),
            "the person's own press failed"
        );
    }

    /// DAY FIVE, D22 — A PRESS ON A ROUND THAT STOPPED UNDER IT: the writer
    /// refuses `Upgrade now` typed `stopped:` (the round is not the owner's
    /// word to re-arm). The row says so naming its tab, drops the refused
    /// `Upgrade now` and the lines about the round that is gone, keeps the
    /// words that still do something, and the host is asked to look again at
    /// once — before, the view stayed stale for 60+ s and a second press
    /// changed nothing. NEGATIVE CONTROL: a lock-busy refusal keeps every
    /// capsule and every line (`a_refused_word_is_said_on_its_row_which_keeps_its_capsules`).
    #[test]
    fn a_press_refused_because_its_round_stopped_drops_the_refused_word_and_looks_again() {
        use aterm_messages::UpgradeWord;
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
        let (id, before) = row_of(&app, "s-a").expect("the overdue row");
        assert!(before.detail[0].contains("it has not read the upgrade's question yet"));
        let looks = app.upgrade_view.looks_asked;
        assert_eq!(
            press(&mut app, id, "Upgrade now"),
            "OK acted=Upgrade%20now performed=1"
        );
        assert_eq!(
            app.upgrade_view.looks_asked,
            looks + 1,
            "the host is asked to look again, once"
        );
        let (same, said) = row_of(&app, "s-a").expect("the same entry");
        assert_eq!(same, id);
        assert_eq!(said.title, "Couldn't upgrade Claude in its tab now");
        assert_eq!(said.detail[0], "retries soon");
        assert!(
            said.detail
                .iter()
                .all(|l| !l.contains("turn is still running") && !l.contains("moves it at")),
            "{:?}",
            said.detail
        );
        let words: Vec<UpgradeWord> = said
            .actions
            .iter()
            .filter_map(|intent| match intent {
                Intent::AgentUpgrade { word, .. } => Some(*word),
                _ => None,
            })
            .collect();
        assert_eq!(words, [UpgradeWord::NotToday, UpgradeWord::Skip]);
    }

    /// A PRESS ON A ROW WHOSE UPGRADE MOVED ON LANDS ON NOTHING, through the
    /// production writer and the harness's real `ask_for` over a state
    /// directory: the window still shows the row for 2.1.282 while the tab's
    /// upgrade now moves to 2.1.283, and its `Skip version` must not skip a
    /// build the owner never saw — the state file is left byte for byte and
    /// the row says the upgrade moved on. NEGATIVE CONTROL: the row the host
    /// hands over next, for 2.1.283, writes the skip of 2.1.283.
    #[test]
    fn a_press_on_a_row_whose_upgrade_moved_on_lands_on_nothing() {
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
        // Stopped the same way twice (a row, ruling 283): `Not today` and
        // `Skip version`.
        let shown = Row {
            phase: Phase::Failed("no-resume".to_string()),
            stop_streak: 2,
            streak_why: "no-resume".to_string(),
            ..row("s-a", 60)
        };
        app.apply_agent_upgrades(vec![shown.clone()]);
        let (id, _) = row_of(&app, "s-a").expect("the stalled row for 2.1.282");
        assert_eq!(
            press(&mut app, id, "Skip version"),
            "OK acted=Skip%20version performed=1"
        );
        assert_eq!(
            std::fs::read_to_string(&file).expect("state file"),
            before,
            "nothing written"
        );
        let (_, said) = row_of(&app, "s-a").expect("the row");
        assert_eq!(said.title, "Couldn't skip Claude 2.1.282 in its tab");
        assert_eq!(said.detail[0], "a newer build: 2.1.283");

        app.apply_agent_upgrades(vec![Row {
            to: "2.1.283".to_string(),
            ..shown
        }]);
        let (id, fresh) = row_of(&app, "s-a").expect("the row for 2.1.283");
        assert_eq!(fresh.title, "Couldn't upgrade Claude in its tab");
        assert_eq!(
            press(&mut app, id, "Skip version"),
            "OK acted=Skip%20version performed=1"
        );
        let written = std::fs::read_to_string(&file).expect("state file");
        assert!(written.contains(r#""request":"skip:2.1.283""#), "{written}");
        assert!(row_of(&app, "s-a").is_none(), "resolved by the word");
        assert_eq!(record_of(&app, id).0, "Claude skips 2.1.283");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// ONE SESSION WAITING: its record carries `Upgrade now` and `Not today`
    /// for its tab and build, and Settings ▸ Messages offers them only while
    /// that upgrade still takes them — a newer target disables them, the
    /// session gone drops them; a press from the page reaches the writer, and what the
    /// word did is recorded. Two waiting: no one tab is the record's, so no
    /// capsule.
    #[test]
    fn one_waiting_session_offers_its_words_while_its_upgrade_stands() {
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
        assert_eq!(offered(&app), [("Upgrade now", true), ("Not today", true)]);
        // The page's own press: written for that tab and build, and recorded.
        let outcome = app.perform_message_act(crate::WindowId(0), id.raw(), 1);
        assert!(
            matches!(
                outcome,
                crate::native_app::MessageActOutcome::Performed { .. }
            ),
            "{outcome:?}"
        );
        let word = asked.try_recv().expect("written");
        assert_eq!(
            (word.tab.as_str(), word.to.as_str(), word.word),
            ("s-a", "2.1.282", UpgradeWord::NotToday)
        );
        assert!(
            harness_records(&app)
                .iter()
                .any(|(title, _)| title == "Claude stays on 2.1.281 until tomorrow"),
            "{:?}",
            harness_records(&app)
        );
        // The upgrade moved on: the old record's words are for a build no
        // longer offered.
        app.apply_agent_upgrades(vec![Row {
            to: "2.1.283".to_string(),
            ..row("s-a", 60)
        }]);
        assert_eq!(
            offered(&app),
            [("Upgrade now", false), ("Not today", false)]
        );
        // Ruling 315 (day eight E3): the tab's session is gone — no row for
        // it — so its words are not offered at all, where they stood drawn,
        // disabled, three relaunches later. Another tab's row does not keep
        // them.
        app.apply_agent_upgrades(vec![row("s-b", 60)]);
        assert!(
            offered(&app).is_empty(),
            "a gone session's words are dropped"
        );
    }

    /// A PRESS OF A WORD THE UPGRADE NO LONGER TAKES WRITES NOTHING (review
    /// of gap #21). Settings ▸ Messages disables a record's button by
    /// [`UpgradeView::offers`], but the page presses from the model it last
    /// drew, and the harness re-checks only the build: an `Upgrade now` from
    /// a record drawn before the owner's `--now` landed would arm a second
    /// round for a move already hurried — the very press 2d93d2f9b took off
    /// the menu. The host re-checks the word as it performs, as it does a
    /// stop or a tab. NEGATIVE CONTROL: the same record's `Not today`, still
    /// taken, is written.
    #[test]
    fn a_press_of_a_word_the_upgrade_no_longer_takes_writes_nothing() {
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
        app.apply_agent_upgrades(vec![row("s-a", 60)]);
        let drawn = app
            .messages
            .log()
            .records()
            .rfind(|r| r.key.as_deref() == Some(KEY_WAITING))
            .map(|r| r.id)
            .expect("the waiting record");
        // The owner's `--now` lands (the CLI, the tab menu): hurried.
        app.apply_agent_upgrades(vec![Row {
            request: Request::Now,
            request_at: now_s(),
            ..row("s-a", 60)
        }]);
        assert!(
            !app.upgrade_view
                .offers("s-a", "2.1.282", UpgradeWord::Now, now_s())
        );
        let outcome = app.perform_message_act(crate::WindowId(0), drawn.raw(), 0);
        assert!(
            matches!(
                outcome,
                crate::native_app::MessageActOutcome::Refused { .. }
            ),
            "{outcome:?}"
        );
        assert!(asked.try_recv().is_err(), "nothing written");

        let outcome = app.perform_message_act(crate::WindowId(0), drawn.raw(), 1);
        assert!(
            matches!(
                outcome,
                crate::native_app::MessageActOutcome::Performed { .. }
            ),
            "{outcome:?}"
        );
        assert_eq!(asked.try_recv().map(|a| a.word), Ok(UpgradeWord::NotToday));
    }

    const KEY_WAITING: &str = message_reporters::KEY_AGENT_UPGRADE;

    /// The AGENT UPGRADE rows of a composed tab menu: (label, build, word).
    fn upgrade_items(
        entries: &[crate::session_chrome::TabMenuEntry],
    ) -> Vec<(String, String, aterm_messages::UpgradeWord)> {
        entries
            .iter()
            .filter_map(|e| match e {
                crate::session_chrome::TabMenuEntry::Upgrade {
                    label, to, word, ..
                } => Some((label.clone(), to.clone(), *word)),
                _ => None,
            })
            .collect()
    }

    /// A TAB WHOSE AGENT HAS AN UPGRADE PENDING OFFERS EVERY WORD ON IT IN
    /// ITS CONTEXT MENU (gap #21), for the build the band names; choosing one
    /// writes it for THAT tab — resolved from the menu's pop-time tab — and
    /// the build the card SHOWED, even when the upgrade moved on while the
    /// card was open (the harness refuses that one: `ask_for`), and what it
    /// did is recorded. NEGATIVE CONTROLS: the menu before any row has no
    /// section, and when the row leaves the section leaves with it — the
    /// rows re-key the tab chrome's cache.
    #[test]
    fn a_tabs_menu_offers_its_upgrades_words_and_a_choice_writes_that_tabs_word() {
        use aterm_messages::UpgradeWord::{NotToday, Now, Skip};
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let sid = app.tab_session_id_text(wid, 0).expect("tab 0's session");
        assert!(upgrade_items(&app.tab_context_menu_model(wid, 0)).is_empty());
        let (writer, asked) = recording(|ask: &WordAsk| {
            Ok(Row {
                request: Request::Skip(ask.to.clone()),
                request_at: now_s(),
                ..row(&ask.tab, 60)
            })
        });
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![row(&sid, 60)]);
        let build = |word| ("2.1.282".to_string(), word);
        assert_eq!(
            upgrade_items(&app.tab_context_menu_model(wid, 0))
                .into_iter()
                .map(|(label, to, word)| (label, (to, word)))
                .collect::<Vec<_>>(),
            [
                ("Upgrade Now".to_string(), build(Now)),
                ("Not Today".to_string(), build(NotToday)),
                ("Skip This Version".to_string(), build(Skip)),
            ]
        );
        assert!(app.open_tab_context_menu(wid, 0, 5, false));
        // The upgrade moves on while the card is open: the card keeps what
        // it showed, and so does the word it sends.
        app.apply_agent_upgrades(vec![Row {
            to: "2.1.283".to_string(),
            ..row(&sid, 60)
        }]);
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
            })
        );
        assert!(
            harness_records(&app)
                .iter()
                .any(|(title, _)| title == "Claude skips 2.1.282"),
            "{:?}",
            harness_records(&app)
        );
        // The row leaves, and its menu section with it.
        app.apply_agent_upgrades(Vec::new());
        assert!(
            upgrade_items(&app.tab_context_menu_model(wid, 0)).is_empty(),
            "the rows re-keyed the chrome cache"
        );
    }

    // ------------------------------------------ once, across a restart (2026-09-27)

    /// `tab`'s upgrade, refused (not its shell's foreground job): a stall a
    /// new round meets again. (A round that GAVE UP is no stall since main's
    /// rest-and-re-arm of 2026-09-27: it rests, then asks again.)
    fn refused(tab: &str) -> Row {
        Row {
            phase: Phase::Failed("not-a-shell-job".to_string()),
            ..row(tab, 60)
        }
    }

    /// A STALL THE OWNER ANSWERED WITH A WORD IS NEWS WHEN IT COMES BACK: the
    /// row is answered with the word pressed (ruling 270), and the word holds
    /// or moves the upgrade, so the same stall standing again once it runs
    /// out is the word not having moved it — never a row the owner already
    /// saw through. `Not today` on a refusal; while the deferral holds,
    /// nothing; once it has run out and the refusal stands, a row again.
    #[test]
    fn a_stall_answered_with_a_word_is_news_when_it_returns() {
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
        let (id, _) = row_of(&app, "s-a").expect("the stall's row");
        assert!(press(&mut app, id, "Not today").starts_with("OK"));
        assert!(row_of(&app, "s-a").is_none(), "answered");
        let deferred = Row {
            request: Request::DeferUntil(now_s() + NOT_TODAY_S),
            request_at: now_s(),
            ..refused("s-a")
        };
        app.apply_agent_upgrades(vec![deferred]);
        assert!(row_of(&app, "s-a").is_none(), "held: no stall");
        app.apply_agent_upgrades(vec![refused("s-a")]);
        assert!(row_of(&app, "s-a").is_some(), "back: a row");
    }

    /// A REFUSAL THE OWNER PUT OFF IS NEWS WHEN IT RETURNS, EVEN ACROSS A
    /// RE-ARM (review of round 21, against ruling 307(e)): the owner
    /// dismissed a refused row; the round re-armed for the same build and
    /// met the refusal again (one stall: its row stays down); then `Not
    /// today` came from the TAB MENU — no live row to answer, so the word is
    /// a record. While it holds, nothing; once it has run out and the same
    /// refusal stands, a row again. Before the fix the held round counted as
    /// a re-arm, the entry stood, and the returning refusal matched it:
    /// nothing was posted, and `Not today` meant never. NEGATIVE CONTROL:
    /// the same re-arm with no word keeps the row down (the one stall).
    #[test]
    fn a_refusal_the_owner_put_off_from_the_tab_menu_is_news_when_it_returns() {
        use aterm_messages::UpgradeWord;
        let rearmed_refused = |tab: &str, request: Request| Row {
            phase: Phase::Pending,
            last_stop: "not-a-shell-job".to_string(),
            request,
            request_at: now_s(),
            ..row(tab, 60)
        };
        let mut app = App::headless_for_test();
        let (writer, _asked) = recording(move |ask: &WordAsk| {
            Ok(Row {
                phase: Phase::Pending,
                last_stop: "not-a-shell-job".to_string(),
                request: Request::DeferUntil(now_s() + NOT_TODAY_S),
                request_at: now_s(),
                ..row(&ask.tab, 60)
            })
        });
        app.upgrade_view.writer = writer;
        app.apply_agent_upgrades(vec![refused("s-a")]);
        let (id, _) = row_of(&app, "s-a").expect("the refusal's row");
        assert!(app.messages.dismiss(id, std::time::Instant::now()));
        app.sync_messages();
        // The round re-arms and meets the refusal again: one stall.
        app.apply_agent_upgrades(vec![rearmed_refused("s-a", Request::None)]);
        assert!(
            row_of(&app, "s-a").is_none(),
            "one stall: its row stays down"
        );
        // NEGATIVE CONTROL: an unheld re-arm keeps the entry.
        assert!(app.upgrade_view.stalled.contains_key("s-a"));
        // `Not today` from the tab menu: no live row, so a record.
        assert!(app.send_upgrade_word(WordAsk {
            tab: "s-a".to_string(),
            to: "2.1.282".to_string(),
            word: UpgradeWord::NotToday,
        }));
        assert!(row_of(&app, "s-a").is_none(), "held: no stall");
        assert!(
            !app.upgrade_view.stalled.contains_key("s-a"),
            "a held round is no re-arm"
        );
        // The deferral runs out and the same refusal stands: news.
        app.apply_agent_upgrades(vec![rearmed_refused("s-a", Request::None)]);
        assert!(row_of(&app, "s-a").is_some(), "back: a row");
    }

    /// A STALL THE OWNER ALREADY SAW IS NOT POSTED AGAIN WHEN ATERM RESTARTS
    /// (the owner's report of 2026-09-27: the updated window posted its tabs'
    /// give-ups afresh, the view that had kept them gone with the process).
    /// The next process reads the message log back, as a launch does: a row
    /// the owner dismissed stays down; one carried across the handoff — on
    /// the band when this process starts — is kept, never a second row.
    /// NEGATIVE CONTROLS: a row still up, UNREAD, when the last process
    /// stopped comes back to the band — once (review of 2026-09-27: a
    /// Standing row stays until the stall ends or the person reads it); a
    /// stall that ENDED since (its row resolved) is news when it comes back;
    /// so is another stall of a tab whose last one was dismissed. And AN
    /// EARLIER BUILD'S ROW is the same stall when its words are (review of
    /// 2026-09-27: the first update onto a new layout posted every stall the
    /// owner had read again): dismissed in the layout before ruling 270 (the
    /// move alone, no place before it), it stays down; carried, it is brought
    /// to this build's words in its slot, never stacked under.
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
        // c's stall ends: its row is resolved.
        first.apply_agent_upgrades(vec![refused(a), refused(b)]);
        assert!(row_of(&first, c).is_none());
        first.flush_messages_log();
        drop(first.messages_log.take());

        // THE NEXT PROCESS: the log read back, the stalls as they stand.
        let relaunched = || {
            let mut next = App::headless_for_test();
            next.messages = aterm_messages::MessageCenter::new(
                crate::messages_store::load_tail(&path, u64::MAX).log,
                std::time::Instant::now(),
            );
            next
        };
        let mut next = relaunched();
        next.apply_agent_upgrades(vec![refused(a), refused(b), refused(c)]);
        assert!(row_of(&next, a).is_none(), "the owner dismissed it");
        let (again_b, _) = row_of(&next, b).expect("unread when the last process stopped: back");
        next.apply_agent_upgrades(vec![refused(a), refused(b), refused(c)]);
        assert_eq!(
            row_of(&next, b).map(|(id, _)| id),
            Some(again_b),
            "once: the same row, not another"
        );
        // NEGATIVE CONTROLS: an ended stall back, and another stall.
        assert!(row_of(&next, c).is_some(), "it ended, and is back: news");
        let newer = Row {
            to: "2.1.283".to_string(),
            ..refused(a)
        };
        next.apply_agent_upgrades(vec![newer, refused(b), refused(c)]);
        assert!(row_of(&next, a).is_some(), "another stall of a's tab");

        // THE HANDOFF: b's row carried, on the band as the successor starts.
        let mut successor = relaunched();
        let now = now_s();
        let carried = successor.post_message(message_reporters::agent_upgrade_stalled(
            &refused(b),
            now,
            "in its tab",
        ));
        successor.apply_agent_upgrades(vec![refused(b)]);
        assert_eq!(
            row_of(&successor, b).map(|(id, _)| id),
            Some(carried),
            "kept, not posted again"
        );
        assert_eq!(successor.messages.live_rows().count(), 1);
        // A carried row in the layout before ruling 270: the title naming
        // no tab, the move alone on its line.
        let earlier = |tab: &str| {
            let mut words =
                message_reporters::agent_upgrade_stalled(&refused(tab), now, "in tab 4");
            words.title = "Couldn't upgrade Claude".to_string();
            let line = &mut words.detail[message_reporters::STALL_MOVE_LINE];
            *line = line
                .strip_prefix("in tab 4 \u{00b7} ")
                .expect("placed")
                .to_string();
            words
        };
        let mut older = relaunched();
        let old = older.post_message(earlier(b));
        older.apply_agent_upgrades(vec![refused(b)]);
        let (id, msg) = row_of(&older, b).expect("b's row");
        assert_eq!(id, old, "the same stall, kept in its slot");
        assert_eq!(msg.title, "Couldn't upgrade Claude in its tab");
        assert!(
            msg.detail[message_reporters::STALL_MOVE_LINE].starts_with("in its tab \u{00b7} "),
            "in this build's words: {:?}",
            msg.detail
        );
        assert_eq!(older.messages.live_rows().count(), 1, "never stacked");
        let _ = std::fs::remove_dir_all(&dir);

        // DISMISSED in the layout before this one: down after the update.
        let dir =
            std::env::temp_dir().join(format!("aterm-upgrade-rows-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join(crate::messages_store::FILE_NAME);
        let mut before = App::headless_for_test();
        before.messages_log = crate::messages_store::Writer::spawn(&path);
        let seen = before.post_message(earlier(a));
        assert!(before.messages.dismiss(seen, std::time::Instant::now()));
        before.sync_messages();
        before.flush_messages_log();
        drop(before.messages_log.take());
        let mut updated = App::headless_for_test();
        updated.messages = aterm_messages::MessageCenter::new(
            crate::messages_store::load_tail(&path, u64::MAX).log,
            std::time::Instant::now(),
        );
        updated.apply_agent_upgrades(vec![refused(a)]);
        assert!(row_of(&updated, a).is_none(), "dismissed before the update");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A successor of an aterm that left `left` up: each posted there, then
    /// carried across the handoff exactly as an update carries them
    /// (`MessageCenter::carried` → `seed_carried`), before the successor's
    /// host has looked at anything. The rows' ids, as the parent posted them.
    fn successor_carrying(left: &[aterm_messages::Message]) -> (App, Vec<MessageId>) {
        let mut parent = App::headless_for_test();
        let ids = left
            .iter()
            .map(|m| parent.post_message(m.clone()))
            .collect();
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

    /// A ROW 0.94.0 POSTED: `Couldn't upgrade Claude · no READY answer after
    /// 4 notices`, keyed on its tab (the owner's two, 2026-09-27).
    fn gave_up_row(tab: &str) -> aterm_messages::Message {
        aterm_messages::Message::new(
            aterm_messages::tags::HARNESS,
            aterm_messages::Severity::Warn,
            "Couldn't upgrade Claude",
        )
        .line("no READY answer after 4 notices")
        .line("Upgrade now asks it again at its next pause")
        .line("Claude Code 2.1.280 \u{2192} 2.1.283")
        .hold(aterm_messages::Hold::Standing)
        .key(&message_reporters::agent_upgrade_key(tab))
    }

    /// CARRIED ROWS ARE WITHDRAWN ONCE THE NEW PROCESS HAS SEEN EVERY TAB,
    /// NEVER RESOLVED AS FIXED (the owner's two `Couldn't upgrade Claude · no
    /// READY answer after 4 notices` rows, 2026-09-27: live when 0.94.0 handed
    /// off, and nothing in the next build retired them — a give-up is no stall
    /// since the rest-and-re-arm, and the new process's view held neither
    /// row). The old process left up six upgrade rows, carried by the real
    /// handoff seam: `a` and `b` (0.94.0's words; both tabs now rest after their
    /// give-ups), `gone` (a tab the host no longer hands over: closed, or its
    /// conversation ended), `stopped` (its stall now a record's shape, ruling
    /// 283), `still` (still refused the same way) and `moving` (its restart
    /// in flight). Until the host's first look, every one stands: nothing is
    /// withdrawn on a guess, so nothing flickers. At the first look — every
    /// tab seen — `a`, `b`, `gone` and `stopped` are WITHDRAWN (never `✓`:
    /// this process never saw a stall end), `stopped`'s record is kept,
    /// `still`'s row is kept in its slot, never posted again, and nothing new
    /// is posted. NEGATIVE CONTROLS: `moving` keeps its row while its restart
    /// is in flight and loses it — withdrawn, not `✓` — once the move is
    /// over; a row THIS process posted and saw end is resolved as before
    /// (ruling 290); `a`'s stall coming back later is news, a row again (a
    /// withdrawn row is none the owner saw through); and the sweep takes
    /// only rows under ONE TAB'S upgrade key (review of 2026-09-27): carried
    /// rows under another family's key, a key that only begins like one, the
    /// waiting record's bare key and the finished moves' `done` key all
    /// stand through every look.
    #[test]
    fn carried_rows_are_withdrawn_once_every_tab_is_seen_never_fixed() {
        let now = now_s();
        let (a, b, gone, stopped, still, moving) =
            ("s-a", "s-b", "s-gone", "s-c", "s-still", "s-d");
        let other = |tag: aterm_messages::Tag, title: &str, key: &str| {
            aterm_messages::Message::new(tag, aterm_messages::Severity::Warn, title)
                .line("carried across the handoff")
                .hold(aterm_messages::Hold::Standing)
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
        // Nothing is retired before the host has looked.
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
        // Tab `mine`, stalled in this process's own time: OVERDUE, a stall
        // whose end is plain (it catches up). Not a refusal: a refusal the
        // re-armed round meets again is one stall through that round
        // (ruling 307(e)), so a Pending row of the same build is no end.
        let mine = "s-mine";
        app.apply_agent_upgrades(vec![
            rested(a),
            rested(b),
            first_stop.clone(),
            refused(still),
            relaunching,
            row(mine, 7 * 3_600),
        ]);
        let withdrawn = Some(aterm_messages::Retired::Withdrawn);
        for (tab, id) in [(a, ids[0]), (b, ids[1]), (gone, ids[2]), (stopped, ids[3])] {
            assert!(row_of(&app, tab).is_none(), "{tab}: down");
            assert_eq!(
                retired(&app, id),
                withdrawn,
                "{tab}: withdrawn, never fixed"
            );
        }
        assert!(
            harness_records(&app)
                .iter()
                .any(|(title, _)| title == "Claude upgrade retries later in its tab"),
            "{:?}",
            harness_records(&app)
        );
        assert_eq!(
            row_of(&app, still).map(|(id, _)| id),
            Some(ids[4]),
            "still stalled: kept in its slot"
        );
        // NEGATIVE CONTROL: a restart in flight keeps its row.
        assert_eq!(row_of(&app, moving).map(|(id, _)| id), Some(ids[5]));
        let (own, _) = row_of(&app, mine).expect("this process's own stall");
        // NEGATIVE CONTROL: no other key is a tab's upgrade key.
        others_stand(&app, "the first look");
        assert_eq!(
            app.messages.live_rows().count(),
            3 + others.len(),
            "nothing else posted"
        );

        // The move ends, and `mine`'s stall with it: the carried row is
        // withdrawn; the row this process posted and saw end is resolved.
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

        // `a`'s stall comes back: news, a row again.
        app.apply_agent_upgrades(vec![refused(a), refused(still)]);
        let (again, _) = row_of(&app, a).expect("a row again");
        assert_ne!(again, ids[0]);
        others_stand(&app, "a's stall back");
    }

    /// A STALL THAT ENDED WHILE ITS ROW WAS DOWN IS NEWS WHEN IT RETURNS
    /// (review of 2026-09-27) — and a refusal a RE-ARMED ROUND meets again is
    /// no end at all (ruling 307, as ruling 283 for a repeating stop). The
    /// owner dismissed two refusals' rows. `a`'s round re-armed for the same
    /// build and met the same refusal: one stall throughout, nothing recorded
    /// as over, and its row stays down — until ruling 307 it came back at
    /// every round with `no longer stalled` in the log each time. `b`'s tab
    /// left the rows (closed): its end is on the record, named by its tab,
    /// so its return is a row again, in this process and in the next one.
    #[test]
    fn a_stall_that_ended_while_its_row_was_down_is_news_when_it_returns() {
        let dir = std::env::temp_dir().join(format!("aterm-upgrade-over-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join(crate::messages_store::FILE_NAME);
        let mut app = App::headless_for_test();
        app.messages_log = crate::messages_store::Writer::spawn(&path);
        assert!(app.messages_log.is_some(), "the writer opened");
        let (a, b) = ("s-a", "s-b");
        let rearmed = |tab: &str| Row {
            phase: Phase::Announced {
                at_s: now_s(),
                asks: 1,
            },
            ..row(tab, 60)
        };
        let over = |app: &App, tab: &str| -> Vec<String> {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(&*format!("harness.upgrade.{tab}")))
                .filter(|r| r.retired() == Some(&aterm_messages::Retired::Recorded))
                .map(|r| r.title.clone())
                .collect()
        };
        app.apply_agent_upgrades(vec![refused(a), refused(b)]);
        for tab in [a, b] {
            let (id, _) = row_of(&app, tab).expect("a row");
            assert!(app.messages.dismiss(id, std::time::Instant::now()));
        }
        app.sync_messages();
        // a's round re-arms for the same build, and meets the refusal again.
        app.apply_agent_upgrades(vec![rearmed(a), refused(b)]);
        assert_eq!(over(&app, a), Vec::<String>::new(), "a re-arm ends nothing");
        app.apply_agent_upgrades(vec![refused(a), refused(b)]);
        assert!(row_of(&app, a).is_none(), "one stall: its row stays down");
        // b's tab leaves the rows: its end, recorded once, with its tab.
        app.apply_agent_upgrades(vec![refused(a)]);
        let ended = over(&app, b);
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert!(
            ended[0].starts_with("Claude upgrade no longer waits in "),
            "{ended:?}"
        );
        assert!(!ended[0].contains("stalled"), "{ended:?}");
        // It comes back, refused again: news.
        app.apply_agent_upgrades(vec![refused(a), refused(b)]);
        assert!(row_of(&app, b).is_some(), "the stall is back: a row");
        // NEGATIVE CONTROL: a never ended, and stays down.
        assert!(row_of(&app, a).is_none(), "dismissed, and never over");
        app.flush_messages_log();
        drop(app.messages_log.take());

        // THE NEXT PROCESS: the log read back.
        let mut next = App::headless_for_test();
        next.messages = aterm_messages::MessageCenter::new(
            crate::messages_store::load_tail(&path, u64::MAX).log,
            std::time::Instant::now(),
        );
        next.messages_log = crate::messages_store::Writer::spawn(&path);
        assert!(next.messages_log.is_some(), "the writer opened again");
        next.apply_agent_upgrades(vec![refused(a), refused(b)]);
        assert!(
            row_of(&next, b).is_some(),
            "up and unread when the last process stopped"
        );
        assert!(row_of(&next, a).is_none(), "dismissed, and never over");
        // A re-arm across the restart ends nothing either.
        next.apply_agent_upgrades(vec![rearmed(a), refused(b)]);
        next.apply_agent_upgrades(vec![refused(a), refused(b)]);
        assert!(row_of(&next, a).is_none(), "still one stall");
        next.flush_messages_log();
        drop(next.messages_log.take());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A HOST SUMMARY READ BEFORE THE WORDS NEVER BRINGS BACK THE ROW THEY
    /// ANSWERED (round 35, D1): live, `Not today` and `Upgrade now` on
    /// `Claude upgrade waits in 3 tabs` each drew a second row, `… in 2 tabs`,
    /// for about 60 ms — a summary the host read after the first tab's word
    /// and before the others reached the window once the press was answered,
    /// and read those tabs overdue again. Such rows take the word written
    /// (`UpgradeView::worded`): no row comes back, no tab's stall is recorded
    /// again, and each tab's `upgrade=` reads its word. NEGATIVE CONTROL: the
    /// same tabs stalled on a NEWER build are another stall, and post.
    #[test]
    fn a_summary_read_before_the_words_never_brings_the_agents_row_back() {
        let (mut app, [a, b, c]) = three_tabs();
        let (writer, _asked) = recording(|ask: &WordAsk| {
            Ok(Row {
                request: Request::DeferUntil(now_s() + NOT_TODAY_S),
                request_at: now_s(),
                ..row(&ask.tab, 7 * 3_600)
            })
        });
        app.upgrade_view.writer = writer;
        let before = vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600), row(&c, 9 * 3_600)];
        app.apply_agent_upgrades(before.clone());
        let (id, _) = group_of(&app).expect("the agent's row");
        assert_eq!(
            press(&mut app, id, "Not today"),
            "OK acted=Not%20today performed=1"
        );
        assert!(group_of(&app).is_none(), "answered");
        let records = app.messages.log().records().count();
        // The host's rows, read before the words were written, land now.
        app.apply_host_upgrades(before.clone());
        assert!(group_of(&app).is_none(), "no second row of the agent's");
        assert_eq!(app.messages.live_rows().count(), 0, "nothing on the band");
        assert_eq!(
            app.messages.log().records().count(),
            records,
            "no stall recorded again: {:#?}",
            harness_records(&app)
        );
        for tab in [&a, &b, &c] {
            assert!(
                app.upgrade_view
                    .row_for(tab)
                    .is_some_and(|r| matches!(r.request, Request::DeferUntil(_))),
                "{tab}: its word stands"
            );
        }
        // NEGATIVE CONTROL: a newer build is another stall — news.
        let newer: Vec<Row> = before
            .iter()
            .map(|r| Row {
                to: "2.1.283".to_string(),
                ..r.clone()
            })
            .collect();
        app.apply_host_upgrades(newer);
        assert!(group_of(&app).is_some(), "a newer build's stall posts");
        // NEGATIVE CONTROL: once the host's rows carry the words, what it
        // reads after them is believed — a deferral run out stalls again.
        let (mut again, [a, b, c]) = three_tabs();
        let (writer, _asked) = recording(|ask: &WordAsk| {
            Ok(Row {
                request: Request::DeferUntil(now_s() + NOT_TODAY_S),
                request_at: now_s(),
                ..row(&ask.tab, 7 * 3_600)
            })
        });
        again.upgrade_view.writer = writer;
        let before = vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600), row(&c, 9 * 3_600)];
        again.apply_host_upgrades(before.clone());
        let (id, _) = group_of(&again).expect("the agent's row");
        assert!(press(&mut again, id, "Not today").starts_with("OK"));
        let carried: Vec<Row> = again.upgrade_view.rows.clone();
        again.apply_host_upgrades(carried);
        assert!(group_of(&again).is_none(), "held");
        again.apply_host_upgrades(before);
        assert!(group_of(&again).is_some(), "run out: the host is believed");
    }

    /// A TAB'S STALL IS ONE ENTRY THROUGH ITS AGENT'S ROW (round 35, D2 and
    /// D3): live, every join, press and regroup withdrew a tab's entry and
    /// recorded the same `⚠ Claude upgrade waits in tab N` again (8 copies a
    /// tab in 13 minutes; `Problems · 24 of 33` for two upgrades), and a tab
    /// that left the row kept its `⚠` with nothing after it. Now a tab's own
    /// row that joins leaves the band withdrawn and stays its one entry; the
    /// record of a tab that stands in the row stays one record while it is
    /// pressed and back; and a tab that leaves has its end recorded. NEGATIVE
    /// CONTROL: the same tab stalled on a newer build is a new entry.
    #[test]
    fn a_tabs_stall_is_one_entry_through_its_agents_row_and_its_end_is_recorded() {
        let (mut app, [a, b, _]) = three_tabs();
        let entries = |app: &App, tab: &str| {
            let key = message_reporters::agent_upgrade_key(tab);
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(key.as_str()))
                .map(|r| r.title.clone())
                .collect::<Vec<_>>()
        };
        let both = || vec![row(&a, 26 * 3_600), row(&b, 9 * 3_600)];
        app.apply_agent_upgrades(vec![row(&a, 26 * 3_600)]);
        let (own, _) = row_of(&app, &a).expect("tab 1 alone: its own row");
        app.apply_agent_upgrades(both());
        assert!(group_of(&app).is_some(), "the agent's row");
        assert!(row_of(&app, &a).is_none(), "tab 1's row left the band");
        assert_eq!(entries(&app, &a).len(), 1, "{:?}", entries(&app, &a));
        assert_eq!(
            app.messages.log().get(own).map(|r| r.state.clone()),
            Some(aterm_messages::LogState::Retired(
                aterm_messages::Retired::Withdrawn
            ))
        );
        // Pressed and back, and looked at again: one entry each still.
        app.upgrade_view.pressed.insert(b.clone());
        app.apply_agent_upgrades(both());
        app.upgrade_view.pressed.remove(&b);
        app.apply_agent_upgrades(both());
        app.apply_agent_upgrades(both());
        assert_eq!(entries(&app, &a).len(), 1, "{:?}", entries(&app, &a));
        assert_eq!(entries(&app, &b).len(), 1, "{:?}", entries(&app, &b));
        // NEGATIVE CONTROL: tab 1 stalled on a newer build is a new entry.
        let newer = Row {
            to: "2.1.283".to_string(),
            ..row(&a, 26 * 3_600)
        };
        app.apply_agent_upgrades(vec![newer, row(&b, 9 * 3_600)]);
        assert_eq!(entries(&app, &a).len(), 2, "{:?}", entries(&app, &a));
        // Tab 1 leaves: its end is on the record, after its stall.
        app.apply_agent_upgrades(vec![row(&b, 9 * 3_600)]);
        assert_eq!(
            entries(&app, &a).last().map(String::as_str),
            Some("Claude upgrade no longer waits in tab 1")
        );
    }

    /// WORDS (round 35, D4 and D5): two tabs are `both`, never `all 2`; and a
    /// round re-armed after a give-up that its own work holds (ruling 364)
    /// neither promises nor offers `Upgrade now`, which moved nothing sooner
    /// there. NEGATIVE CONTROLS: three tabs are `all 3`; the same wait in a
    /// round that did not give up keeps `Upgrade now`.
    #[test]
    fn two_tabs_are_both_and_a_rearmed_own_work_round_promises_no_upgrade_now() {
        use aterm_messages::UpgradeWord;
        let (mut app, [a, b, c]) = three_tabs();
        app.apply_agent_upgrades(vec![row(&a, 7 * 3_600), row(&b, 8 * 3_600)]);
        let (_, two) = group_of(&app).expect("the agent's row");
        assert!(
            two.detail[1].contains("moves both at their next pauses")
                && two.detail[1].contains("puts both off until tomorrow"),
            "{}",
            two.detail[1]
        );
        app.apply_agent_upgrades(vec![
            row(&a, 7 * 3_600),
            row(&b, 8 * 3_600),
            row(&c, 9 * 3_600),
        ]);
        let (_, three) = group_of(&app).expect("the agent's row");
        assert!(
            three.detail[1].contains("moves all 3 at"),
            "{}",
            three.detail[1]
        );

        let now = now_s();
        // Its own work holds it (ruling 364's round): a turn running. (This
        // module's rows wait on a full queue, which `Upgrade now` moves.)
        let rearmed = Row {
            last_stop: aterm_agent::harness::upgrade::GAVE_UP.to_string(),
            wait: "not-idle:busy".to_string(),
            ..row(&a, 8 * 3_600)
        };
        assert!(rearmed.asks_on_its_own(now), "ruling 364's record");
        assert!(!message_reporters::agent_upgrade_words(&rearmed, now).contains(&UpgradeWord::Now));
        let said = message_reporters::agent_upgrade_stalled(&rearmed, now, "in tab 1");
        assert!(
            said.detail
                .iter()
                .all(|l| !l.contains("moves it at its next turn end")),
            "{:#?}",
            said.detail
        );
        assert!(
            said.actions
                .iter()
                .all(|i| i.label() != UpgradeWord::Now.label()),
            "{:?}",
            said.actions
        );
        // NEGATIVE CONTROL: a re-armed round held by what `Upgrade now` does
        // move (a full queue) is no record, and offers it. (A turn still
        // running offers it on no round since ruling 380 (b), give-up or not.)
        let queued = Row {
            last_stop: aterm_agent::harness::upgrade::GAVE_UP.to_string(),
            ..row(&a, 8 * 3_600)
        };
        assert!(!queued.asks_on_its_own(now));
        assert!(message_reporters::agent_upgrade_words(&queued, now).contains(&UpgradeWord::Now));
    }
}
