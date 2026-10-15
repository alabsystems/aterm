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
    aterm_time::unix_secs().unwrap_or(0)
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
            // The flow the row it answers with is read under, as the host's
            // are (`Row::quiet`): `[harness] upgrade_ask` as parsed.
            ask: crate::harness_host::upgrade_ask(),
            several_logins: crate::harness_host::several_logins(),
            instances: upgrade_drive::Instances::Machine,
        };
        upgrade_drive::ask_for(&opts, &ask.tab, ask.ask(), &ask.to)
    })
}

/// The host's last rows `shown` with a pressed word's answer `row` laid in
/// over its tab's, each judged again at `now` (`Row::reread`: the awake clock
/// as it stands, the host's own grace kept) — `None` when the tab has no row
/// there (only in place: the host's own rows may have landed first). The rows
/// the host read are served late here, at a press: judged with the clock they
/// were read with, rows carried over a night's sleep read `unwatched`, and
/// every other owed tab was posted "aterm itself has stopped checking it" at
/// the owner's first press after the wake, before the host's first look (the
/// review of 2026-09-30).
fn laid_in(shown: &[Row], row: Row, now: u64) -> Option<Vec<Row>> {
    let mut rows = shown.to_vec();
    let at = rows.iter().position(|r| r.tab == row.tab)?;
    // The answer is read from the record alone: its tab's agent is as far
    // from the host's hands as the host's last look found it (`Row::aside`).
    let mut row = row;
    row.keep_aside(&rows[at], now);
    rows[at] = row;
    for r in &mut rows {
        r.reread(now);
    }
    Some(rows)
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
    /// How long its tab has been behind, in the words it last said
    /// (`Row::age_said`: `6 h`, `7 h`; `None` for every stall whose words
    /// carry no age — only an `overdue` row's do): a standing row says the
    /// new age in place as it grows, never a new row.
    behind: Option<String>,
    /// Where its words last said its tab is ([`message_reporters::UpgradeSpot`]'s
    /// `place`: `in tab 2`, `in window 2, tab 1`): the look after its tab
    /// moved, or a window opened or closed beside it, restates the row that
    /// stands in place (ruling 440).
    place: String,
}

/// Whether a stall's `said` is a restart in flight that has not moved
/// (`stuck:<what>`, `upgrade_drive::Row::stall`).
fn stuck(said: &str) -> bool {
    said.starts_with("stuck:")
}

/// One waiting record's words: its title, its detail and the owner's words
/// it offers.
type WaitingSaid = (String, Vec<String>, Vec<Intent>);

/// A stalled tab's own entry, at `spot`
/// ([`message_reporters::agent_upgrade_stalled_at`]: the row that stands
/// takes the owner to its tab): its row — or, where it stands in its agent's
/// one row (`said` ends [`GROUPED`]), a RECORD with the same words and
/// buttons, which Settings ▸ Messages keeps and the band does not show twice.
fn stall_entry(
    row: &Row,
    said: &str,
    now: u64,
    spot: &message_reporters::UpgradeSpot,
) -> aterm_messages::Message {
    if said.ends_with(GROUPED) || said.ends_with(PRESSED) {
        message_reporters::agent_upgrade_stalled(row, now, &spot.place)
            .hold(aterm_messages::Hold::LogOnly)
    } else {
        message_reporters::agent_upgrade_stalled_at(row, now, spot)
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

/// Which way a stall's entry changed SHAPE ([`reshaped`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reshape {
    /// A row whose upgrade asks on its own again: a record from here.
    ToRecord,
    /// A record whose upgrade needs the glass: the move clock ran out, or a
    /// person's hold stood a whole re-ask — on a row of its own, or on its
    /// agent's one row ([`GROUPED`], round 44).
    ToRow,
}

/// Whether `fresh` is `posted`'s SAME STALL AND TARGET in the other shape —
/// `/row` ⇄ `/record` (ruling 279: the shape is part of what was said) — and
/// which way (review of 8899c91b4: each flip withdrew the entry and posted
/// another, so one waiting upgrade was several entries: its record kept
/// `behind for 23 h` for good beside the row the move clock raised at 24 h,
/// and a draft that stood, passed and stood again made four). One entry:
/// [`App::post_upgrade_stalls`] turns a row into its own record in place,
/// and replaces a record by the row it posts.
fn reshaped(posted: &str, fresh: &str) -> Option<Reshape> {
    let flip = |from: &str, to: &str, way: Reshape| {
        let (was, is) = (posted.strip_suffix(from)?, fresh.strip_suffix(to)?);
        (was == is).then_some(way)
    };
    flip("/row", "/record", Reshape::ToRecord)
        .or_else(|| flip("/record", "/row", Reshape::ToRow))
        // A record whose move clock ran out straight into its agent's row
        // (round 44): replaced by the tab's entry there, as by its own row —
        // it kept `behind for 23 h` for good beside a second entry.
        .or_else(|| flip("/record", GROUPED, Reshape::ToRow))
}

/// A stall record's first detail line (`behind for 7 h: its own work runs`,
/// `Row::stall_words`) saying `age` instead; `None` for a line that says no
/// age.
fn aged(line: &str, age: &str) -> Option<String> {
    let rest = line.strip_prefix("behind for ")?;
    Some(match rest.split_once(':') {
        Some((_, tail)) => format!("behind for {age}:{tail}"),
        None => format!("behind for {age}"),
    })
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
    /// How many of those `Upgrade now` only asked once more — a question
    /// unread behind a full queue, a round given up
    /// ([`message_reporters::upgrade_now_asks_again`],
    /// [`message_reporters::agent_upgrade_group_worded`]).
    asked_again: usize,
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

/// One word written ([`UpgradeView::worded`]): the row it came back with,
/// when it landed, and the phase it was pressed on when the word itself
/// moved the phase — a Defer or Skip ends an announcement (`Pending`), and
/// `--now` re-arms a round that gave up — which a host row read before the
/// word still carries (round 44).
type Worded = (Row, std::time::Instant, Option<Phase>);

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
    /// when, and the phase the word was pressed on where the word itself
    /// moved it ([`UpgradeView::words_landed`]). The host's rows that were
    /// read before a word was written take its request, so they never
    /// undo it on the glass for a moment (round 35, D1).
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

    /// A WORD WRITTEN, FROM ITS OWN ROW ([`Self::worded`]), and the phase
    /// the view's row for its tab had when it was pressed, where the word
    /// moved it: the view's rows take the word's only after this.
    fn word_landed(&mut self, row: &Row) {
        if row.request_at != 0 {
            let pressed_on = self
                .rows
                .iter()
                .find(|r| r.tab == row.tab && r.session == row.session)
                .map(|r| r.phase.clone())
                .filter(|phase| *phase != row.phase);
            self.worded.insert(
                row.tab.clone(),
                (row.clone(), std::time::Instant::now(), pressed_on),
            );
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
    /// row. The phase the word was PRESSED ON counts as the word's own
    /// (round 44): a `Not today` on an announced round is written `Pending`,
    /// and a summary read before it still says `Announced` — with the phase
    /// alone, the entry was dropped and the agent's row came back, with an
    /// `asks again` right after `Not today`. A row that carries the word or
    /// a newer one, moved on (another target, another phase, another
    /// conversation), or a tab gone, ends the
    /// entry — the host hands its rows over in the order it read them, so
    /// none after one that carries the word is older than it (a deferral
    /// that runs out is the host's to say); so does [`WORD_LAG`], past which
    /// the host is trusted as it reads. Only the host's rows
    /// ([`App::apply_host_upgrades`]): the window's own re-application of its
    /// rows as each word lands carries the words already.
    fn words_landed(&mut self, rows: &mut [Row]) {
        self.worded.retain(|tab, (word, at, pressed_on)| {
            at.elapsed() < WORD_LAG
                && rows.iter().any(|r| {
                    r.tab == *tab
                        && r.session == word.session
                        && same_build(&r.to, &word.to)
                        && (r.phase == word.phase || pressed_on.as_ref() == Some(&r.phase))
                        && r.request_at < word.request_at
                })
        });
        for r in rows.iter_mut() {
            if let Some((word, _, _)) = self.worded.get(&r.tab)
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
        // Each tab's `upgrade=`, on the timeline both verbs read — and its
        // note for the person there, which the Claude Code footer leads with
        // while the move waits only on them (`Row::pause_note`; the owner,
        // 2026-09-30). A note that moved is a frame owed: the row changes at
        // every look of an attended wait, its note only when what it says
        // does.
        let snapshot = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot();
        let mut noted: Vec<u64> = Vec::new();
        for h in &snapshot {
            let row = rows.iter().find(|r| r.tab == h.sid.as_str()).cloned();
            let note = row.as_ref().and_then(|r| r.pause_note(now));
            let mut timeline = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.set_upgrade(row);
            if timeline.set_upgrade_note(note) {
                noted.push(h.local_id);
            }
        }
        for session in noted {
            self.redraw_claude_footer(session);
        }
        self.record_upgrade_waiting(&rows, now);
        self.post_upgrade_stalls(&rows, now);
        self.note_upgrade_faults(&rows, now);
        self.note_upgrade_exceptions(&rows, now);
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
    /// A RECORD (a stall that asks again on its own, rulings 279 and 283, or
    /// a tab in its agent's row: never on the band, so never read through,
    /// and recorded whenever its entry changes) is the earlier process's
    /// record of the same stall, if any ([`Self::stall_already_recorded`]).
    /// `None`: nothing of the kind; a stall that ENDED since — its row resolved, or, down already,
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

    /// THE RECORD THIS STALL ALREADY HAS FROM BEFORE THIS PROCESS (round 44):
    /// a tab standing in its agent's row, or one whose upgrade asks on its
    /// own, has a RECORD for its entry, and every restart or update handoff
    /// recorded it again — one more `⚠` in Problems per grouped tab per
    /// process, the churn ruling 375 removed within one. The latest record
    /// under the stall's key is adopted when an earlier process wrote it
    /// (this one retired nothing of it), it is a record still, in the same
    /// mark — the entry's shape (ruling 279) — and it is THE SAME STALL
    /// ([`message_reporters::same_stall`]). This process's own records are
    /// never adopted: within it a changed entry is recorded anew, as before.
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

    /// ATERM'S OWN FAULTS IN THE UPGRADE, told once each (owner rules,
    /// 2026-09-28): a wait aterm does not classify, a notice its screen keeps
    /// refusing, a record no look has visited, a relaunched agent that never
    /// idles, a record with no progress in a day. ONE Warn record and ONE
    /// `aterm.log` line per fault, for the owner to read later as the developer
    /// — the Problems filter of the messages page — and nothing on the glass, on
    /// the tab or in the digest: there is nothing to do about them. A fault that
    /// ends is forgotten, so its return is news.
    fn note_upgrade_faults(&mut self, rows: &[Row], now: u64) {
        use aterm_agent::harness::upgrade_drive::Attention;
        let faults: BTreeMap<String, (&Row, aterm_agent::harness::upgrade_drive::Need)> = rows
            .iter()
            .filter_map(|r| match r.attention(now) {
                Attention::Fault(need) => Some((need.fingerprint.clone(), (r, need))),
                _ => None,
            })
            .collect();
        // A fault that ended is forgotten, so its return is news.
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

    /// THE UPGRADE'S EXCEPTIONS (owner rules, 2026-09-28): the rows the
    /// classifier calls a `Digest` ([`upgrade_drive::Row::attention`]) reach
    /// the exceptions digest — "To look at" — as ONE dismissable line per kind
    /// of wait, never a band row, a tab mark, a menu-bar row or a
    /// notification. The family's WHOLE answer at every look this host takes
    /// (the rows change, or a horizon passes and the host is handed them
    /// again — [`upgrade_drive::Row::next_attention_at`]): a wait that ended,
    /// or a tab that left, ends its line ten minutes after the look that
    /// stopped naming it, at the digest's own clock, so no second look is
    /// needed ([`App::except_replace`]). The upgrade's host fact — it cannot
    /// use its state folder — is an EVENT of [`Family::UpgradeHost`], never
    /// part of this pass.
    ///
    /// [`Family::UpgradeHost`]: aterm_messages::exceptions::Family::UpgradeHost
    fn note_upgrade_exceptions(&mut self, rows: &[Row], now: u64) {
        let spots: Vec<(&Row, message_reporters::UpgradeSpot)> = rows
            .iter()
            .map(|row| (row, self.upgrade_spot(&row.tab)))
            .collect();
        let entries = message_reporters::upgrade_exceptions(&spots, now);
        self.except_replace(aterm_messages::exceptions::Family::Upgrade, &entries);
    }

    /// Whether the stall `posted` stands for is now `fresh`, the same stall
    /// and target IN THE OTHER SHAPE ([`reshaped`]) — only while its entry
    /// is what that shape turns: a row still on the glass (one the owner put
    /// down is not raised for it, and changes as any stall said otherwise
    /// does), a record still a record; and never under a stuck restart's
    /// words. A stall that asks on its own while the owner's `Upgrade now`
    /// stands keeps its answer on the glass (`agent_upgrade_stalled`: calm,
    /// but a row), so it turns no row into a record.
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

    /// A stall's RECORD re-worded in place ([`App::reword_record`]) — in
    /// `msg`'s words where they were drawn for the capsules the record
    /// keeps; otherwise ONLY ITS AGE moves (review of 8899c91b4: a record
    /// written while its own work held it, `Not today` and `Skip version`,
    /// re-worded at its round's give-up to `Upgrade now asks it at its next
    /// pause; Skip This Version in the tab's menu …` — a button it does not
    /// have, and the menu for one it has; the day-five D21 class). The rest
    /// of its words stay as written, true of the capsules it offers, until
    /// the stall or its target changes. `behind`: the age its words say now
    /// (`Row::age_said`).
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
                //
                // The class decides it (owner rules, 2026-09-28): a row only for
                // the agent the upgrade ended, anything else a record.
                let shape = if r.interrupt(now).is_some() {
                    "row"
                } else {
                    "record"
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
        // A record NOTHING HAS CHECKED (`unwatched`, design record C10) is
        // aterm's own fault: a line in the log ([`Row::attention`]), never a
        // row, so it joins no group either.
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
            // NEVER AN INTERRUPT (owner rules, 2026-09-28): the one row that
            // stands is the agent the upgrade ended, in ITS tab, with its own
            // action. Only that shape is a "row" here, and it is never an
            // overdue wait but for a give-up that came after the agent was
            // gone — which must not be folded into "Couldn't upgrade Claude in
            // N tabs yet", a warn row with snoozes. So nothing joins an
            // agent's group: the code stays until its deletion (C6b) and
            // cannot raise a loud row
            // (`no_tab_joins_an_agents_group_row`).
            if row.interrupt(now).is_none()
                && said.starts_with("overdue/")
                && said.ends_with("/row")
            {
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
                    // The same stall in the other shape: kept, one entry
                    // (below).
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
        // THE SAME STALL IN THE OTHER SHAPE ([`reshaped`], review of
        // 8899c91b4): one entry. A row whose upgrade asks on its own again
        // leaves the glass as its own record, in place — the same id, which
        // the arm below keeps re-wording. A record that needs the glass
        // cannot come back to it: its tab's entry is dropped here, the loop
        // below posts the row (or adopts one the owner already saw), and the
        // record is replaced by it — `replaced` on Settings ▸ Messages, never
        // a second `Couldn't upgrade …` standing beside the row for good.
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
                    let msg = stall_entry(row, &fresh, now, &spot);
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
                    let spot = self.upgrade_spot(&row.tab);
                    let msg = stall_entry(row, &said, now, &spot);
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
                //
                // ITS AGE, TOO (2026-09-29, the owner's log: one waiting
                // upgrade is one message that says how long in place): a
                // standing row read `behind for 6 h` for as long as it stood.
                // Only the words its entry says move it (`Row::age_said`: an
                // `overdue` stall's age, by the hour or day it counts in; no
                // other stall's words carry one), so a look that changes
                // nothing else restates nothing.
                //
                // A RECORD SAYS IT IN PLACE TOO (second review of 2026-09-29):
                // an upgrade its agent's own work holds asks again on its own,
                // so its one entry is a record — written once, it said
                // `behind for 6 h` from the stall's start to the move clock.
                // Re-worded in place ([`App::reword_record`]: no new entry, and
                // nothing for a row the owner put down, whose words are what
                // it said when it left).
                //
                // ITS PLACE AND ITS `Show tab N` TOO (ruling 440): the row that
                // stands follows its tab. A tab moved to another window or
                // index changes both its words and its button; a window opened
                // or closed beside it changes only its words (`in tab 1` ↔ `in
                // window 1, tab 1`) — the button carries the window's id, not
                // its number. Either restates the row in place, so its words
                // and its button never name two places. The place is compared
                // as it was last said, never by the row's title, which a
                // refused press lays its own words over
                // ([`App::apply_upgrade_word`]) until something moves. A record
                // is never re-worded for its place alone (ruling 300: a window
                // opened later does not restate it).
                let person = row.person_seen(now);
                let behind = row.age_said(now);
                let spot = self.upgrade_spot(&row.tab);
                let msg = stall_entry(row, &said, now, &spot);
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
            let msg = stall_entry(row, &said, now, &spot);
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
    /// when its words or buttons change — its tabs, their words, each tab's
    /// age as the look read it (`Row::behind_for`) — and never posted twice: a row carried across a
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
            asked_again: 0,
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
                let asked_again = self
                    .upgrade_view
                    .row_for(&ask.tab)
                    .is_some_and(message_reporters::upgrade_now_asks_again);
                if asked_again {
                    self.upgrade_view.presses[at].asked_again += 1;
                }
                self.record_message(message_reporters::agent_upgrade_worded(
                    &row,
                    ask.word,
                    &place,
                    asked_again,
                ));
                if let Some(rows) = laid_in(&self.upgrade_view.rows, row, now_s()) {
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
                press.asked_again,
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
                // Above what the row SAYS — its own words, never a refusal an
                // earlier press laid over them (round 44: each refused press
                // stacked another copy, until the cap cut the stalls' line).
                let said = self
                    .upgrade_view
                    .groups
                    .get(&press.key)
                    .map_or(shown.detail, |group| group.said.1.clone());
                refused.detail.extend(said);
                refused.actions = shown.actions;
                refused.hold = shown.hold;
                refused.severity = shown.severity;
                refused.glyph = shown.glyph;
                let _ =
                    self.restate_message(press.id, crate::messages_host::restatement_of(&refused));
            }
            _ => {
                // A press from an OLDER entry of the agent's row (a record on
                // Settings ▸ Messages) while a newer row stands under its key:
                // the refusal is its own row, never that row's replacement —
                // under the key it superseded the live row, whose stalls then
                // read as put down (round 44).
                if self
                    .messages
                    .live_by_key(&press.key)
                    .is_some_and(|l| l.id != press.id)
                {
                    refused.key = None;
                }
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
        let spot = self.upgrade_spot(&ask.tab);
        let place = spot.place.clone();
        match result {
            Ok(row) => {
                aterm_log::info!(
                    "harness @{}: the owner's word from the window: {} ({})",
                    ask.tab,
                    ask.word.as_str(),
                    row.request.word()
                );
                // The row pressed waited on its question unread (ruling 307),
                // or its round gave up: `Upgrade now` asked it once more, and
                // says so.
                let asked_again = self
                    .upgrade_view
                    .row_for(&ask.tab)
                    .is_some_and(message_reporters::upgrade_now_asks_again);
                let said =
                    message_reporters::agent_upgrade_worded(&row, ask.word, &place, asked_again);
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
                if let Some(rows) = laid_in(&self.upgrade_view.rows, row, now_s()) {
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
                        // The row's own buttons where it stands, `Show tab N`
                        // on an interrupt's among them (ruling 440): the next
                        // look compares them with its fresh entry's, and a
                        // refusal drawn without that capsule was restated away
                        // at it, though nothing had moved.
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
                // Its words name where the tab is now: the place the next look
                // compares ([`StallRow::place`]).
                if restated && let Some(posted) = self.upgrade_view.stalled.get_mut(&ask.tab) {
                    posted.place = spot.place;
                }
                // The tab stands alone in its agent's row: that row says why,
                // above what it said, and keeps its buttons — as the tab's own
                // row would — never a row of its own beside it.
                let grouped = !restated
                    && self.alone_in_group(&ask.tab).is_some_and(|(key, id)| {
                        let Some(shown) = self.messages.live(id).map(|l| l.msg.clone()) else {
                            return false;
                        };
                        // Above the row's own words, never an earlier
                        // refusal's (round 44).
                        let said = self
                            .upgrade_view
                            .groups
                            .get(&key)
                            .map_or(shown.detail, |group| group.said.1.clone());
                        let mut words = refused.clone();
                        words.detail.extend(said);
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
        self.upgrade_spot(sid).place
    }

    /// Where session `sid`'s tab is: [`Self::upgrade_place`]'s words and the
    /// `Show tab` navigation to it (`None` for a session no tab of this
    /// instance shows).
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
                            .map(|tab| Intent::ShowTab { tab, window: wid.0 }),
                    };
                }
            }
        }
        message_reporters::UpgradeSpot {
            place: "in its tab".to_string(),
            nav: None,
        }
    }

    /// The session ids some tab of this instance shows: the set
    /// [`Self::upgrade_spot`] searches, taken whole. A caller that must decide
    /// whether a digest line's tab is still there asks THIS (and computes the
    /// line's key from each id), never parses an id back out of the key: a key
    /// shortens a subject that does not fit its cap, so it does not round-trip.
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

    /// HOW THE LIVE TABS STAND for the managed-current record
    /// ([`TabsBehind`]): every registered tab carrying the adoption mark
    /// (lowered, never raised, by measurement —
    /// `SessionTimeline::take_path_lowered`) split by what runs in front — an
    /// agent tab is never handed the hook to type, and one the live upgrade is
    /// moving heals with the move (its relaunch line sources the hook) — plus
    /// the shells still waiting to be adopted; the unmarked tabs whose PATH
    /// was measured, two jobs in a row, to put a foreign copy first; and the
    /// Claude sessions RUNNING A BUILD OLDER THAN THE VERSION THE RECORD NAMES
    /// (its wire text, remembered by `post_managed_current`), split into those
    /// the upgrade moves on its own and those it will not move —
    /// stalled, or held by the owner's word (review of 2026-09-25: an overdue
    /// session was said to move at its next turn end while the band showed it
    /// stalled). The build a session RUNS, whatever its row's target (audit
    /// 2026-10-09: at the install the rows still targeted the previous build
    /// for the two seconds before their retarget, so counting only rows onto
    /// the new one recorded "are current" nine times in a week while two tabs
    /// ran days-old builds). The version, not the copy: a session on the native
    /// install of an older version lags it just the same — measured
    /// 2026-09-25, one of the owner's two lagging sessions was the native
    /// 2.1.281, the session the gap audit's false "up to date" was said over.
    ///
    /// KNOWN LIMITS (review of 2026-10-09): a session is counted only while
    /// the live upgrade hands the window a row for it. A finished move whose
    /// model is still unconfirmed leaves the rows once its confirm-by and
    /// `DONE_SHOWN_S` pass (`upgrade_status::View::refresh_with`) while the
    /// harness starts no next round for it, so such a session goes uncounted
    /// whatever build it runs; and at an install, a session that was current
    /// on the previous build has no row until the harness mints the new
    /// round, two or three seconds later, so the first record can still say
    /// "current" — re-worded in place once those rows land
    /// (`post_managed_current`), though the message log keeps the first line.
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
            if !managed.as_deref().is_some_and(|v| runs_older(row, v)) {
                continue;
            }
            // A finished move onto an older build is behind too, and the
            // upgrade takes it on from there on its own.
            // Every other session behind will not move on its own: stalled
            // (a restart stuck in flight among them), or held by the owner.
            if row.phase == Phase::Done || (row.moving(now) && row.stall(now).is_none()) {
                tabs.agents_behind += 1;
            } else {
                tabs.agents_held += 1;
            }
        }
        tabs
    }
}

/// Whether the session `row` moves RUNS a build older than `managed`: the
/// build it is moving from, or — once relaunched, its relaunch line having
/// typed the target, or its move done — the one it moved onto, whatever its
/// row targets next. A relaunch still waiting for the idle point that carries
/// its conversation on (stuck there, even) runs the target already (review
/// of 2026-10-09). Where either does not parse as a version, any build but
/// `managed` itself is counted: an unreadable build is never called current.
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

    /// An agent the upgrade ENDED and could not bring back, ten minutes ago:
    /// the one state that takes a band row (`Row::attention`,
    /// `Interrupt`, kind `agent-down`), whose action is the resume command.
    fn gone(tab: &str) -> Row {
        Row {
            behind_since: now_s() - 7 * 3_600,
            ..refused(tab)
        }
    }

    /// The records of the harness tag whose title says `part`.
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
            wait_since: now_s() - 60,
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
    /// once. A tab six hours behind is a record too (the owner, 2026-09-28: an
    /// alert persists only where they can act); the ONE row is an agent the
    /// upgrade ended and could not bring back — one per tab, resolved when the
    /// owner's word holds it. NEGATIVE CONTROL: a session merely waiting for
    /// its turn end, and one six hours behind, put nothing on the glass.
    #[test]
    fn waiting_is_recorded_once_and_only_an_agent_the_upgrade_ended_takes_a_row() {
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

        // One tab is six hours behind: a RECORD, never a row, and a wait never
        // says `Couldn't`.
        let stalled = vec![row("s-a", 7 * 3_600), row("s-b", 60)];
        app.apply_agent_upgrades(stalled.clone());
        assert_eq!(app.messages.live_rows().count(), 0, "a wait is no row");
        let waits = records_titled(&app, "upgrade waits");
        assert_eq!(waits.len(), 1, "{:?}", harness_records(&app));
        assert!(!waits[0].0.contains("Couldn't"), "{}", waits[0].0);
        assert!(
            waits[0].1[0].starts_with("behind for 7 h"),
            "{:?}",
            waits[0].1
        );
        app.apply_agent_upgrades(stalled);
        assert_eq!(records_titled(&app, "upgrade waits").len(), 1, "once");
        assert_eq!(app.messages.live_rows().count(), 0);

        // The agent the upgrade ended is the one row, and it names the command.
        let ended = vec![gone("s-a"), row("s-b", 60)];
        app.apply_agent_upgrades(ended.clone());
        let live: Vec<_> = app.messages.live_rows().map(|l| l.msg.clone()).collect();
        assert_eq!(live.len(), 1, "{live:?}");
        assert_eq!(live[0].title, "Claude stopped in its tab");
        assert_eq!(live[0].key.as_deref(), Some("harness.upgrade.s-a"));
        assert!(
            live[0]
                .detail
                .iter()
                .any(|l| l.contains("`claude --resume conv-s-a`")),
            "the action is the command: {:?}",
            live[0].detail
        );
        app.apply_agent_upgrades(ended);
        assert_eq!(app.messages.live_rows().count(), 1, "one row per tab");

        // The owner's skip holds it: the row is resolved.
        let held = vec![
            Row {
                request: Request::Skip("2.1.282".to_string()),
                ..gone("s-a")
            },
            row("s-b", 60),
        ];
        app.apply_agent_upgrades(held);
        assert_eq!(app.messages.live_rows().count(), 0, "resolved");
    }

    /// A PERSON NO LOOK HAS SEEN LATELY NEVER REACHES THE BAND (the review of
    /// 2026-09-28; messages.log of 2026-09-27: a row said "someone is typing in
    /// its tab" for 5 h 50 min of a turn the harness itself had continued).
    /// The words still follow the person (`Row::person_seen`, on the record and
    /// `--status`), but a person at the tab is not the owner needed: no look at
    /// them, or at their absence, is ever a row, and the record is written
    /// once. NEGATIVE CONTROL: a later look that still sees them restates
    /// nothing.
    #[test]
    fn a_person_no_look_has_seen_lately_never_reaches_the_band() {
        let mut app = App::headless_for_test();
        let now = now_s();
        let at = |seen_ago: u64| Row {
            wait: "attended".to_string(),
            wait_since: now - 6 * 3_600,
            wait_seen: now - seen_ago,
            ..row("s-a", 7 * 3_600)
        };
        for seen_ago in [5, 3_600, 1] {
            app.apply_agent_upgrades(vec![at(seen_ago)]);
            assert_eq!(app.messages.live_rows().count(), 0, "{seen_ago}");
        }
        assert_eq!(records_titled(&app, "upgrade waits").len(), 1);
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

        let ended = |tab: &str| Row {
            session: "0badf00d-1111-2222-3333-444455556666".to_string(),
            ..gone(tab)
        };
        app.apply_agent_upgrades(vec![ended(&a), ended(&b)]);
        let (_, row_a) = row_of(&app, &a).expect("a's row");
        let (_, row_b) = row_of(&app, &b).expect("b's row");
        assert_eq!(row_a.title, "Claude stopped in window 1, tab 1");
        assert_eq!(row_b.title, "Claude stopped in window 2, tab 1");
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

    /// RULING 440 — THE ROW THAT STANDS TAKES THE OWNER TO ITS TAB: an agent
    /// the upgrade ended asks for a hand in its tab, so its row offers `Show
    /// tab N` for that tab, as the digest's lines do; and at the look after
    /// the tab moves — here into a second window, where its words become `in
    /// window 2, tab 1` — the row's place and its button are restated
    /// together, and a press shows the tab. At the look after the first
    /// window closes, its words lose the window while its button stays: the
    /// place alone restates it. NEGATIVE CONTROL: a session no tab shows (`in
    /// its tab`) offers nothing, since there is nowhere to go.
    #[test]
    fn an_ended_agents_row_takes_the_owner_to_its_tab() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let second = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(second));
        if app.store.read().unwrap().by_local(second).is_none() {
            app.store
                .write()
                .unwrap()
                .register(crate::session_store::test_handle(second));
        }
        let b = app
            .store
            .read()
            .unwrap()
            .by_local(second)
            .unwrap()
            .sid
            .as_str()
            .to_string();
        let ended = |tab: &str| Row {
            session: "0badf00d-1111-2222-3333-444455556666".to_string(),
            ..gone(tab)
        };
        app.apply_agent_upgrades(vec![ended(&b)]);
        let (_, one) = row_of(&app, &b).expect("b's row");
        assert_eq!(one.title, "Claude stopped in tab 2");
        assert_eq!(
            one.actions,
            [Intent::ShowTab {
                tab: 2,
                window: wid.0
            }]
        );
        // The same look again: the row and its button stand as they are.
        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![ended(&b)]);
        assert_eq!(app.messages.revision(), revision, "the same row, once");

        let moved = app
            .detach_active_tab_logical()
            .expect("the second tab moves");
        app.apply_agent_upgrades(vec![ended(&b)]);
        let (id, two) = row_of(&app, &b).expect("b's row, restated");
        let show = Intent::ShowTab {
            tab: 1,
            window: moved.0,
        };
        assert_eq!(two.title, "Claude stopped in window 2, tab 1");
        assert_eq!(
            two.actions,
            std::slice::from_ref(&show),
            "the place and the button move together"
        );
        assert_eq!(show.label(), "Show tab 1");
        assert!(
            crate::message_reporters::attention(&two).is_ok(),
            "{:?}",
            two.title
        );
        assert!(
            app.perform_intent(wid, id, show.clone()),
            "its press shows the tab"
        );

        // The first window closed: one window is left, so the words lose
        // their `window 2,` while the button — the window's id, not its
        // number — stays as it is. The place alone restates the row, in place.
        assert!(matches!(
            app.close_window_logical(wid),
            crate::CloseOutcome::Stay
        ));
        app.apply_agent_upgrades(vec![ended(&b)]);
        let (same, alone) = row_of(&app, &b).expect("b's row, restated again");
        assert_eq!(same, id, "restated in place, never a second row");
        assert_eq!(alone.title, "Claude stopped in tab 1");
        assert_eq!(alone.actions, [show], "the button already named the tab");

        // NEGATIVE CONTROL: no tab of this instance shows the session.
        let mut app = App::headless_for_test();
        app.apply_agent_upgrades(vec![ended("s-0000000000000000")]);
        let (_, nowhere) = row_of(&app, "s-0000000000000000").expect("its row");
        assert!(nowhere.title.ends_with("in its tab"), "{}", nowhere.title);
        assert!(nowhere.actions.is_empty(), "{:?}", nowhere.actions);
    }

    /// RULING 440 — A REFUSED PRESS ON THE ROW THAT STANDS: the refusal is laid
    /// over the interrupt's row with the row's own buttons, `Show tab N` among
    /// them, and the look after it, which moves nothing, leaves it as it is —
    /// the refusal's title is not the stall's, and only the place and the
    /// buttons are compared. NEGATIVE CONTROL: once the tab moves, the look
    /// does restate the row, where its tab is now.
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
            stall
                .actions
                .contains(&Intent::ShowTab { tab: 3, window: 0 }),
            "{:?}",
            stall.actions
        );
        app.apply_upgrade_word(
            &WordAsk {
                tab: c.clone(),
                to: ended.to.clone(),
                word: UpgradeWord::Now,
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

        // NEGATIVE CONTROL: the tab moved; the look restates the row there.
        let moved = app.detach_active_tab_logical().expect("c moves");
        app.apply_agent_upgrades(vec![ended]);
        let (_, there) = row_of(&app, &c).expect("c's row, restated");
        assert_ne!(there.title, refused.title);
        assert_eq!(
            there.actions,
            [Intent::ShowTab {
                tab: 1,
                window: moved.0
            }]
        );
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
        // Aterm's own failure is never a row (nothing the owner can do about
        // it): a Warn record under its own key, and the wait's record.
        assert_eq!(app.messages.live_rows().count(), 0, "no row");
        let fault = records_titled(&app, "Upgrade stuck on aterm");
        assert_eq!(fault.len(), 1, "{:?}", harness_records(&app));
        assert!(
            fault[0].1[0].contains("aterm's to fix"),
            "told as aterm's own: {:?}",
            fault[0].1
        );
        let id = record_id_of(&app, &a).expect("the wait's record");
        press_record(&mut app, id, 0);
        assert_eq!(
            app.messages.live_rows().count(),
            0,
            "answered, and not put back"
        );
        // Still refused ninety minutes after the word: told again, as aterm's
        // own, and `Upgrade now` is not offered again (it is already in force).
        let later = Row {
            request: Request::Now,
            request_at: now - upgrade_drive::TELL_S - 120,
            refused: 4,
            refused_at: now - upgrade_drive::TELL_S - 60,
            ..refused
        };
        app.apply_agent_upgrades(vec![later.clone()]);
        // (Only the answer to the owner's own press may be up: calm, and it
        // folds after two minutes.)
        assert!(
            app.messages
                .live_rows()
                .all(|l| l.msg.severity == aterm_messages::Severity::Info),
            "still no warning row"
        );
        let told = message_reporters::agent_upgrade_stalled(&later, now, "in its tab");
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
                .any(|l| l.contains("tries its notice again at the next quiet moment")),
            "{:?}",
            told.detail
        );
        // Never "at its turn ends": a goal-mode Codex's turns never end.
        assert!(
            !told.detail.iter().any(|l| l.contains("turn end")),
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

    /// ONLY A SESSION THAT WILL MOVE ON ITS OWN IS SAID TO MOVE (review of
    /// 2026-09-25: an overdue session was counted so while the band showed it
    /// stalled). An overdue, gave-up or skipped session is counted as staying
    /// behind — the title still says "installed". Every session RUNNING a
    /// build older than the record's is counted, whatever its row targets: one
    /// moving to another version lags it just the same, and so does one moving
    /// onto the NATIVE copy (the measured 2.1.281 session was native). One
    /// already on the record's build is not behind, wherever it moves next.
    /// NEGATIVE CONTROL: a healthy pending row onto the named version is
    /// counted as moving.
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
                agents_behind: 3,
                agents_held: 3,
                ..TabsBehind::default()
            },
            "healthy, native and 2.1.283-bound move; overdue, gave-up, skipped stay; one \
             already on 2.1.282 is not behind"
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
            detail.contains("3 running Claude Code sessions move onto it on their own"),
            "{detail}"
        );
        assert!(
            detail.contains("3 running Claude Code sessions are still on the old build"),
            "{detail}"
        );
        // With no managed record yet there is no build to count against.
        let mut fresh = App::headless_for_test();
        fresh.apply_agent_upgrades(vec![row("s-healthy", 60)]);
        assert_eq!(fresh.tabs_behind(), TabsBehind::default());
    }

    /// "ARE CURRENT" IS NEVER SAID WHILE A SESSION RUNS AN OLDER BUILD (audit
    /// 2026-10-09: nine `… are current · ready in every aterm tab` records in
    /// a week while two tabs ran days-old builds). At the install the marker
    /// lands while the rows still target the PREVIOUS build — their retarget
    /// comes two seconds later — so a count of rows onto the new build alone
    /// was zero and the record said "current". The sessions are counted by the
    /// build they run, a finished move onto an older build among them, and a
    /// relaunch onto an older build.
    /// NEGATIVE CONTROL: sessions that run the installed build leave the
    /// record "current" — one moving past it, one whose move onto it is done,
    /// and one RELAUNCHED onto it, fresh or stuck past `STALE_S` waiting for
    /// the idle point that carries its conversation on (review of
    /// 2026-10-09: the relaunch line typed the managed copy, so such a
    /// session runs it; read by the build it moves from, a stuck one was
    /// recorded "still on the old build" while the band's own stuck row said
    /// the new one had started).
    #[test]
    fn the_install_is_not_current_while_a_session_runs_an_older_build() {
        let title = |rows: Vec<Row>| {
            let mut app = App::headless_for_test();
            app.apply_agent_upgrades(rows);
            assert!(app.post_managed_current("claude 2.1.287 (Anthropic latest)"));
            app.messages
                .log()
                .records()
                .rfind(|r| r.key.as_deref() == Some(crate::toolchain_words::KEY_MANAGED))
                .map(|r| r.title.clone())
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
        assert_eq!(
            title(vec![to_previous]),
            "Claude Code 2.1.287 installed",
            "a row onto the previous build still runs an older one"
        );
        assert_eq!(
            title(vec![done_onto_previous]),
            "Claude Code 2.1.287 installed",
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
        assert_eq!(
            title(vec![relaunched("s-e", "2.1.286", 30)]),
            "Claude Code 2.1.287 installed",
            "relaunched onto the previous build, it runs that one"
        );
        let stuck_onto_it = relaunched("s-f", "2.1.287", 7 * 86_400);
        assert_eq!(
            stuck_onto_it.stall(now_s()).as_deref(),
            Some("stuck:relaunched"),
            "the reviewer's row: stuck past STALE_S"
        );
        assert_eq!(
            title(vec![relaunched("s-g", "2.1.287", 30), stuck_onto_it]),
            "Claude Code 2.1.287 is current",
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
        assert_eq!(
            title(vec![past_it, done_onto_it]),
            "Claude Code 2.1.287 is current"
        );
    }

    /// ONE INSTALL IS ONE RECORD (audit 2026-10-09: 28 `toolchain.managed`
    /// records for 11 updates, one install recorded at 12:48:46, :49 and :51
    /// as its rows retargeted and moved): the record is written when the SET
    /// OF VERSIONS changes, and the tab counts that move after it re-word that
    /// record in place — its words follow the sessions, the log keeps one
    /// entry. A new version is a new record. NEGATIVE CONTROL: the record's
    /// words did move, from "installed" to "current".
    #[test]
    fn one_install_is_one_record_whose_counts_move_in_place() {
        let mut app = App::headless_for_test();
        let managed = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(crate::toolchain_words::KEY_MANAGED))
                .map(|r| r.title.clone())
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
        // The retarget, two seconds later: the same counts, nothing said.
        app.apply_agent_upgrades(vec![moving("2.1.287")]);
        assert!(!app.post_managed_current(text), "the same words");
        // The move lands: the counts move, and the one record says so.
        app.apply_agent_upgrades(vec![Row {
            phase: Phase::Done,
            done_at: now_s() - 5,
            ..moving("2.1.287")
        }]);
        assert_eq!(
            managed(&app),
            vec!["Claude Code 2.1.287 is current".to_string()],
            "one record, re-worded in place"
        );
        assert!(app.post_managed_current("claude 2.1.288 (Anthropic latest)"));
        assert_eq!(managed(&app).len(), 2, "a new version is a new record");
    }

    /// A RECORD GONE FROM THE RING IS WRITTEN AGAIN when its words move
    /// (review of 2026-10-09): the in-place re-word needs the record still
    /// there, and once the shared ring (`LOG_CAP`) has evicted it the new
    /// counts are a new record — never silence until the versions move.
    /// NEGATIVE CONTROL: that new record is the one re-worded after it, and
    /// the same words again write nothing.
    #[test]
    fn a_managed_record_gone_from_the_ring_is_written_again() {
        let mut app = App::headless_for_test();
        let managed = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(crate::toolchain_words::KEY_MANAGED))
                .map(|r| r.title.clone())
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
        assert_eq!(managed(&app), Vec::<String>::new(), "evicted by the ring");
        // The move lands: the words move, with no record left to re-word.
        app.apply_agent_upgrades(vec![Row {
            phase: Phase::Done,
            done_at: now_s() - 5,
            ..moving.clone()
        }]);
        assert_eq!(
            managed(&app),
            vec!["Claude Code 2.1.287 is current".to_string()],
            "a new record"
        );
        assert!(!app.post_managed_current(text), "the same words: nothing");
        app.apply_agent_upgrades(vec![moving]);
        assert_eq!(
            managed(&app),
            vec!["Claude Code 2.1.287 installed".to_string()],
            "the new record is the one re-worded"
        );
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

    /// A MOVE THAT FINISHED DAYS AFTER IT HAPPENED SAYS WHEN IT HAPPENED (audit
    /// 2026-10-09: `Claude Code moved onto 2.1.287` was recorded seven days
    /// after the move, at the session's next idle turn end, dated as if it
    /// had just happened). The relaunch's instant rides the row
    /// (`Row::moved_at`) to the finished move's record, which names it on the
    /// reader's clock against the day the record is written. NEGATIVE CONTROL:
    /// a move finished minutes after its relaunch says nothing more — its
    /// record's own instant is the move's.
    #[test]
    fn a_move_finished_days_later_names_when_it_happened() {
        use crate::message_reporters::moved_line;
        // Tuesday 2026-09-22 14:04 UTC, read on a UTC clock.
        let at = 1_758_549_840_u64 + 365 * 86_400;
        assert_eq!(
            moved_line(at, at + 3 * 3_600, Some(0)).as_deref(),
            Some("moved at 2:04 PM")
        );
        assert_eq!(
            moved_line(at, at + 86_400, Some(0)).as_deref(),
            Some("moved yesterday 2:04 PM")
        );
        let week = moved_line(at, at + 7 * 86_400, Some(0)).expect("a week later");
        assert!(
            week.starts_with("moved ") && week.ends_with("2:04 PM"),
            "{week}"
        );
        assert!(!week.starts_with("moved at"), "with its day: {week}");
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
        // Through the window's door: the finished move's record carries it.
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
        // NEGATIVE CONTROL: moved two minutes before it finished.
        let mut app = App::headless_for_test();
        let prompt = Row {
            phase: Phase::Done,
            done_at: now - 5,
            moved_at: now - 125,
            outcome: "claude restarted on 2.1.287 \u{b7} model claude-opus-5-5".to_string(),
            to: "2.1.287".to_string(),
            ..row("s-a", 600)
        };
        app.apply_agent_upgrades(vec![prompt]);
        assert_eq!(
            records_titled(&app, "moved onto")[0].1,
            vec![
                "in its tab".to_string(),
                "model claude-opus-5-5".to_string()
            ]
        );
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

    /// The newest log record keyed for `tab`'s upgrade, live or not: what a
    /// wait leaves in Settings ▸ Messages (with the owner's words as buttons).
    fn record_id_of(app: &App, tab: &str) -> Option<MessageId> {
        let key = format!("harness.upgrade.{tab}");
        app.messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(key.as_str()))
            .map(|r| r.id)
    }

    /// The live band row keyed for `tab`'s upgrade.
    fn row_of(app: &App, tab: &str) -> Option<(MessageId, aterm_messages::Message)> {
        let key = format!("harness.upgrade.{tab}");
        app.messages
            .live_rows()
            .find(|l| l.msg.key.as_deref() == Some(key.as_str()))
            .map(|l| (l.id, l.msg.clone()))
    }

    /// RULING 283 — ONE ENTRY PER STOPPED STORY, NOW A RECORD (ruling 423:
    /// the owner's rule of 2026-09-28, an alert persists only where they can
    /// act). A round that stops once and will ask again on its own is a
    /// RECORD saying when; the same stop again is a digest line, until the
    /// digest exists one calm record under the same key, which stands through
    /// the re-armed round, its notice and its restart in flight and the next
    /// identical stop - never a row on the band, never a mark, and never
    /// withdrawn at a re-arm and posted anew a round later. It ends at Done.
    /// NEGATIVE CONTROL: before ruling 423 the second stop raised a standing
    /// row.
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

        // The first stop: a record that says when it asks again.
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

        // The same stop again: still no row, one record that counts it.
        app.apply_agent_upgrades(vec![at(failed(), 2, "")]);
        assert!(row_of(&app, "s-a").is_none(), "a repeating stop is no row");
        let id = record_id_of(&app, "s-a").expect("its record");
        let (title, _, detail) = record_of(&app, id);
        assert_eq!(title, "Couldn't upgrade Claude in its tab");
        assert!(
            detail[1].contains("stopped this way 2 times in a row"),
            "{detail:?}"
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
            assert_eq!(app.messages.live_rows().count(), 0, "{word}: no row");
        }
        // It moved at last: its record says it went through, under a ✓ - never
        // `⚠ Couldn't upgrade` beside the success record.
        app.apply_agent_upgrades(vec![Row {
            done_at: now,
            ..at(Phase::Done, 3, "")
        }]);
        assert!(row_of(&app, "s-a").is_none());
        assert!(
            app.messages
                .log()
                .records()
                .any(|r| r.title == "Claude Code moved onto 2.1.282"
                    && r.severity == aterm_messages::Severity::Success),
            "{:?}",
            harness_records(&app)
        );
    }

    /// A SHUTDOWN THAT HUNG (S2 of the in-flight review; ruling 327) takes one
    /// row - the upgrade asked the agent to end and it has not, so only the
    /// owner can (`Press Ctrl-C in that tab`), after [`HUNG_EXIT_AFTER_S`]
    /// (15 minutes, three times the restart's own bound; nothing harder is
    /// ever sent) - which follows what the restart waits on while it is stuck
    /// (`exiting`, then `exited`: one row, restated in place) and is resolved
    /// once the restart moves again - never left up by the host's in-flight
    /// guard saying "has not exited" of an agent that exited and relaunched.
    /// NEGATIVE CONTROL: within the bound the restart is no stall and takes no
    /// row.
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
        assert_eq!(first.title, "Claude is not exiting in its tab");
        assert!(
            first.detail[0].contains("has not exited"),
            "{:?}",
            first.detail
        );
        // The agent is gone and the line that resumes it never came: the same
        // key, the words follow the fact.
        app.apply_agent_upgrades(vec![Row {
            exited_at: now - aterm_agent::harness::relaunch::STALE_S - 30,
            ..at(Phase::Exiting { at_s: hung })
        }]);
        let (again, exited) = row_of(&app, "s-a").expect("still stuck");
        assert_eq!(again, id, "one row while it is stuck");
        assert!(
            exited.detail[0].contains("could not start it again"),
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
        let stuck = |tab| {
            at(
                tab,
                Phase::Exiting {
                    at_s: now - upgrade_drive::HUNG_EXIT_AFTER_S - 60,
                },
            )
        };
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

    /// Press button `action` of the record `id` on the Settings ▸ Messages
    /// page, the way a click there does (`perform_message_act`): a wait is a
    /// record, and its buttons are the owner's words.
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

    /// THE OWNER STEERS THE UPGRADE FROM ITS RECORD (gap #21: until now only
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
        // An overdue tab is a RECORD (Settings ▸ Messages), which carries the
        // owner's words as its buttons; the band holds no row for it.
        app.apply_agent_upgrades(vec![a.clone()]);
        assert!(row_of(&app, "s-a").is_none(), "a record, never a row");
        let id_a = record_id_of(&app, "s-a").expect("s-a's record");
        let buttons = |app: &App, id: MessageId| -> Vec<&'static str> {
            app.messages_state()
                .entries
                .iter()
                .find(|e| e.id == id.raw())
                .expect("the entry")
                .actions
                .iter()
                .map(|a| a.label)
                .collect()
        };
        assert_eq!(buttons(&app, id_a), ["Upgrade now", "Not today"]);

        press_record(&mut app, id_a, 0);
        assert_eq!(
            asked.try_recv(),
            Ok(WordAsk {
                tab: "s-a".to_string(),
                to: "2.1.282".to_string(),
                word: UpgradeWord::Now,
            })
        );
        assert!(asked.try_recv().is_err(), "one press, one word");
        // A record has no row to restate: the answer is a record of its own,
        // in the word's words. The rig's tab waits on a notice behind a full
        // queue (ruling 380): `Upgrade now` types it once more (ruling 307
        // (c)). Ruling 270: a word is a choice, not delivered work or a fix -
        // `ℹ`, never `✓` and `took`.
        let answered = records_titled(&app, "Claude is asked again in its tab");
        assert_eq!(answered.len(), 1, "{:?}", harness_records(&app));
        assert!(
            app.messages
                .log()
                .records()
                .any(|r| r.title == "Claude is asked again in its tab"
                    && r.severity == Severity::Info),
            "answered in the word's words"
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
        assert_eq!(app.messages.live_rows().count(), 0);
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
        let id_b = record_id_of(&app, "s-b").expect("s-b's record");
        assert_eq!(record_of(&app, id_b).0, "Claude upgrade waits in its tab");
        assert_eq!(buttons(&app, id_b).len(), 2);
        press_record(&mut app, id_b, 1);
        let deferral = asked.try_recv().expect("s-b's word");
        assert_eq!(
            (deferral.tab.as_str(), deferral.to.as_str(), deferral.word),
            ("s-b", "2.1.282", UpgradeWord::NotToday)
        );
        assert_eq!(deferral.ask(), Ask::Defer(24 * 3_600), "a day");
        assert!(row_of(&app, "s-b").is_none());
        assert_eq!(
            records_titled(&app, "Claude stays on 2.1.281 until tomorrow").len(),
            1,
            "{:?}",
            harness_records(&app)
        );
    }

    fn upgrade_view_request(app: &App, tab: &str) -> Option<Request> {
        app.upgrade_view.row_for(tab).map(|r| r.request.clone())
    }

    /// Row `id`'s record: its title, mark and detail as the log keeps them.
    fn record_of(app: &App, id: MessageId) -> (String, aterm_messages::Severity, Vec<String>) {
        let rec = app.messages.log().get(id).expect("on record");
        (rec.title.clone(), rec.severity, rec.detail.clone())
    }

    /// AN OVERDUE TAB'S MENU FOLLOWS WHAT MOVES IT NOW (review of gap #21): the
    /// tab's menu offers the owner's words for a stall that is now a RECORD
    /// (the band holds a row only for an agent the upgrade ended), and an
    /// overdue stall's remedy moves with its wait. Waiting on a notice behind
    /// a full queue, `Upgrade now` types it once more and is offered; the same
    /// stall waiting on the agent's READY answer — which `--now` does not
    /// waive, and where a press would only arm a new notice round — stops
    /// offering it. No capsule is on the record either way (a decline is no
    /// action). NEGATIVE CONTROL: the host's same rows again change nothing.
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

    /// A STALL'S ROW FOLLOWS ITS SHAPE, NOT ONLY ITS KIND (ruling 279): an
    /// announced move on the agent's own work asks again on its own and is a
    /// record (D5); the agent then ended and not brought back is the one
    /// state that takes a row — at once, the tab's mark and the glass
    /// agreeing — with no snooze capsule (a decline is no action there). Back
    /// to asking on its own, the row leaves the glass, withdrawn (never a `✓`:
    /// nothing moved). NEGATIVE CONTROLS: the same rows again change nothing.
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

        // The upgrade ended the agent and it did not come back: the one row.
        let ended = gone("s-a");
        app.apply_agent_upgrades(vec![ended.clone()]);
        let (id, msg) = row_of(&app, "s-a").expect("an ended agent is a row");
        assert_eq!(msg.hold, aterm_messages::Hold::STANDING);
        assert_eq!(msg.severity, aterm_messages::Severity::Warn);
        assert!(msg.actions.is_empty(), "no snooze: {:?}", msg.actions);
        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![ended]);
        assert_eq!(app.messages.revision(), revision, "the same row, once");

        // The wait's record stands as history; the row is a different story
        // (the agent ended), told once.
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
            .filter(|r| r.title.starts_with("Claude upgrade waits"))
            .collect();
        assert_eq!(waiting.len(), 2, "the wait's record, then its record again");
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

    /// A WORD THE HARNESS REFUSED, PRESSED ON A WAIT'S RECORD: nothing was
    /// written (here another step held the sweep lock). A wait is a record,
    /// never a row, so there is no stalled row to restate: the refusal is a ROW
    /// in the gesture-failure shape (ruling 270) that names the press to try
    /// again, and the host's same rows do not post the wait a row.
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
        assert_eq!(live[0].title, "Couldn't upgrade Claude in its tab now");
        assert!(
            live[0].detail[0].contains("press it again"),
            "{:?}",
            live[0].detail
        );
        assert!(live[0].detail[1].contains("lock"), "{:?}", live[0].detail);
        assert_eq!(live[0].hold, Hold::For(aterm_messages::HOLD_GESTURE));
        app.apply_agent_upgrades(waiting);
        assert!(
            harness_records(&app)
                .iter()
                .all(|(title, _)| !title.starts_with("Couldn't upgrade Claude in its tab yet")),
            "a wait is never posted as a failure"
        );

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
    /// word to re-arm). The refusal says so naming its tab and when it retries
    /// (a gesture-failure row of its own, carrying no button of the refused
    /// word), and the host is asked to look again at once - before, the view
    /// stayed stale for 60+ s and a second press changed nothing. NEGATIVE
    /// CONTROL: a lock-busy refusal asks for no look
    /// (`a_refused_word_on_a_waits_record_is_a_gesture_failure_row`).
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
        assert!(record_of(&app, id).2[0].contains("it has not read the upgrade's question yet"));
        let looks = app.upgrade_view.looks_asked;
        press_record(&mut app, id, 0);
        assert_eq!(
            app.upgrade_view.looks_asked,
            looks + 1,
            "the host is asked to look again, once"
        );
        let live: Vec<_> = app.messages.live_rows().map(|l| l.msg.clone()).collect();
        assert_eq!(live.len(), 1, "{live:?}");
        assert_eq!(live[0].title, "Couldn't upgrade Claude in its tab now");
        assert_eq!(live[0].detail[0], "retries soon");
        assert!(
            live[0]
                .detail
                .iter()
                .all(|l| !l.contains("turn is still running") && !l.contains("moves it at")),
            "{:?}",
            live[0].detail
        );
    }

    /// A PRESS ON A RECORD WHOSE UPGRADE MOVED ON LANDS ON NOTHING, through the
    /// production writer and the harness's real `ask_for` over a state
    /// directory: the window still shows the row for 2.1.282 while the tab's
    /// upgrade now moves to 2.1.283, and its `Skip version` must not skip a
    /// build the owner never saw — the state file is left byte for byte and
    /// the row says the upgrade moved on. NEGATIVE CONTROL: the row the host
    /// hands over next, for 2.1.283, writes the skip of 2.1.283.
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
        // Stopped the same way twice (a record since ruling 423, with `Not
        // today` and `Skip version` as its buttons).
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
        let said = app
            .messages
            .live_rows()
            .map(|l| l.msg.clone())
            .find(|m| m.title.starts_with("Couldn't skip"))
            .expect("the refusal's row");
        assert_eq!(said.title, "Couldn't skip Claude 2.1.282 in its tab");
        assert_eq!(said.detail[0], "a newer build: 2.1.283");

        app.apply_agent_upgrades(vec![Row {
            to: "2.1.283".to_string(),
            ..shown
        }]);
        let id = record_id_of(&app, "s-a").expect("the record for 2.1.283");
        assert_eq!(record_of(&app, id).0, "Couldn't upgrade Claude in its tab");
        press_record(&mut app, id, 1);
        let written = std::fs::read_to_string(&file).expect("state file");
        assert!(written.contains(r#""request":"skip:2.1.283""#), "{written}");
        assert_eq!(
            records_titled(&app, "Claude skips 2.1.283").len(),
            1,
            "{:?}",
            harness_records(&app)
        );
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

    /// `tab`'s upgrade as a stall that STANDS AS A ROW: the agent the upgrade
    /// ended and could not bring back (`Interrupt`, `agent-down`) - the only
    /// state that takes the band (ruling 423). The lifecycle tests below
    /// (once across a restart, withdrawn and never `✓`, news when it
    /// returns) keep their laws on it; a refusal or a give-up is a record.
    fn refused(tab: &str) -> Row {
        Row {
            phase: Phase::Failed("stale-exit".to_string()),
            exited_at: now_s() - 600,
            wait: String::new(),
            wait_since: 0,
            ..row(tab, 60)
        }
    }

    /// A STALL THE OWNER ANSWERED WITH A WORD IS QUIET FOR THE DAY: the row is
    /// answered with the word pressed (ruling 270), and the word holds or moves
    /// the upgrade, so the same stall is never a row the owner already saw
    /// through. `Not today` on a refusal; while the deferral holds, nothing;
    /// the engine's once-only rule (`STANDING_QUIET`, the deferral's own day)
    /// keeps the same key down after it, and another tab's stall is news.
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
        // An ended agent's row carries no snooze; the word comes from the tab's
        // menu, and answers the stall.
        assert!(app.send_upgrade_word(WordAsk {
            tab: "s-a".to_string(),
            to: "2.1.282".to_string(),
            word: aterm_messages::UpgradeWord::NotToday,
        }));
        assert!(row_of(&app, "s-a").is_none(), "answered");
        let deferred = Row {
            request: Request::DeferUntil(now_s() + NOT_TODAY_S),
            request_at: now_s(),
            ..refused("s-a")
        };
        app.apply_agent_upgrades(vec![deferred]);
        assert!(row_of(&app, "s-a").is_none(), "held: no stall");
        // THE ENGINE'S ONCE-ONLY RULE HOLDS IT (`STANDING_QUIET`, a day, the
        // deferral's own span): a stall the owner answered is not raised again
        // by the same key inside it, however the host rebuilds its view. NEGATIVE
        // CONTROL: another tab's stall is news.
        app.apply_agent_upgrades(vec![refused("s-a")]);
        assert!(
            row_of(&app, "s-a").is_none(),
            "answered a moment ago: quiet for the day"
        );
        app.apply_agent_upgrades(vec![refused("s-a"), refused("s-b")]);
        assert!(row_of(&app, "s-b").is_some(), "another tab: a row");
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
    /// one the owner dismissed stays quiet for the day, whatever its target
    /// (the engine's once-only rule, `STANDING_QUIET`). (The row
    /// layouts of earlier builds are not carried for it: the one row that
    /// stands is an ended agent's, which no earlier build worded so.)
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
        assert!(
            row_of(&next, a).is_none(),
            "a's tab is quiet for the day the owner put it away, whatever its target"
        );

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
        // A standing row is born late (the progress grace): a parent hands the
        // successor the rows that are on its glass.
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
        .hold(aterm_messages::Hold::STANDING)
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
        // `stopped` is a family that changed class: its record, classified and
        // under the carried row's key, is the newer build saying the row is
        // no longer its answer, so the engine retires the row as superseded
        // by it (never `✓`, never fixed).
        assert!(row_of(&app, stopped).is_none(), "{stopped}: down");
        assert!(
            matches!(
                retired(&app, ids[3]),
                Some(aterm_messages::Retired::Superseded { .. })
            ),
            "{stopped}: superseded by its record, never fixed: {:?}",
            retired(&app, ids[3])
        );
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
        // The same stall again, at the next look: one stall, nothing recorded
        // as over, and its row stays down.
        app.apply_agent_upgrades(vec![refused(a), refused(b)]);
        assert_eq!(over(&app, a), Vec::<String>::new(), "nothing ended");
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
        // The same stall again across the restart ends nothing either.
        next.apply_agent_upgrades(vec![refused(a), refused(b)]);
        assert!(row_of(&next, a).is_none(), "still one stall");
        next.flush_messages_log();
        drop(next.messages_log.take());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// OVERDUE TABS FORM NO ROW, GROUPED OR SINGLE (the owner, 2026-09-28: "don't
    /// continue to persist alerts to the user that are not actionable"). Three
    /// tabs a day behind on the same Claude upgrade were one standing row, the
    /// agent's (ruling 363); they are three records now, each a calm line in the
    /// log that never says `Couldn't`, and the band holds nothing for them.
    /// NEGATIVE CONTROL: the agents the upgrade ended stand one row each, in
    /// their own words — never grouped.
    #[test]
    fn overdue_tabs_form_no_row_and_no_group() {
        let (mut app, [a, b, c]) = three_tabs();
        app.apply_agent_upgrades(vec![
            row(&a, 7 * 3_600),
            row(&b, 8 * 3_600),
            row(&c, 9 * 3_600),
        ]);
        assert!(group_of(&app).is_none(), "no row for the agent's tabs");
        assert_eq!(app.messages.live_rows().count(), 0, "nothing on the band");
        let waits = records_titled(&app, "upgrade waits");
        assert_eq!(waits.len(), 3, "{:?}", harness_records(&app));
        assert!(waits.iter().all(|(title, _)| !title.contains("Couldn't")));
        app.apply_agent_upgrades(vec![gone(&a), gone(&b), row(&c, 9 * 3_600)]);
        assert!(group_of(&app).is_none(), "an ended agent's row is its own");
        assert_eq!(app.messages.live_rows().count(), 2);
        assert!(row_of(&app, &a).is_some() && row_of(&app, &b).is_some());
        assert!(row_of(&app, &c).is_none());
    }

    /// A tab suspended for an hour: the digest's per-tab wait (`Row::attention`,
    /// `Digest`, kind `suspended`).
    fn suspended(tab: &str) -> Row {
        Row {
            wait: "not-foreground".to_string(),
            wait_since: now_s() - 2 * 3_600,
            ..row(tab, 2 * 3_600)
        }
    }

    /// The entries "To look at" holds, by key.
    fn to_look_at(app: &App) -> Vec<(String, aterm_messages::DigestEntry)> {
        app.messages
            .digest()
            .entries
            .into_iter()
            .map(|e| (e.key.clone(), e))
            .collect()
    }

    /// A WAIT ONLY THE OWNER CAN END IS ONE LINE OF "To look at", NEVER A ROW
    /// (the owner, 2026-09-28: "batch these exceptions in a single dismissable
    /// alert"; "don't continue to persist alerts to the user that are not
    /// actionable"). Two tabs suspended for an hour are two lines of the digest —
    /// each naming its tab and the one thing to do, each with `Show tab` — and
    /// the band, the tabs' marks and the record's rows stay as they were: no
    /// row, no mark. The same look again changes nothing; the owner's one
    /// dismissal files both in the log, and the same waits, looked at again, are
    /// not raised for the span the dismissal earned. NEGATIVE CONTROL: the agent
    /// the upgrade ended still stands its ONE row, in its own tab, and takes no
    /// line of the digest.
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
        let second = &lines[&format!("harness.upgrade.{b}.suspended")];
        assert_eq!(first.title, "Claude in tab 1 is suspended");
        assert_eq!(second.title, "Claude in tab 2 is suspended");
        assert_eq!(first.then, "Run fg in that tab");
        assert_eq!(
            first.actions,
            [Intent::ShowTab { tab: 1, window: 0 }],
            "the line takes the owner to the tab"
        );
        assert_eq!(app.messages.live_rows().count(), 0, "no row");
        assert_eq!(app.messages.wanted_rows(), 0, "nothing wants the glass");
        assert!(!marked(&app), "no tab is marked");
        // The same look again: the digest did not move.
        let revision = app.messages.digest_revision();
        app.apply_agent_upgrades(vec![suspended(&a), suspended(&b), row(&c, 60)]);
        assert_eq!(app.messages.digest_revision(), revision);
        // The agent the upgrade ended: its own row, no line beside it.
        app.apply_agent_upgrades(vec![gone(&a), suspended(&b), row(&c, 60)]);
        assert!(row_of(&app, &a).is_some(), "the ended agent's row stands");
        assert_eq!(app.messages.live_rows().count(), 1, "the one row");
        assert_eq!(
            to_look_at(&app).len(),
            2,
            "b's line stands; a's ends in ten minutes"
        );
        // ONE DISMISSAL files them; the same waits looked at again stay filed.
        app.apply_agent_upgrades(vec![suspended(&a), suspended(&b), row(&c, 60)]);
        assert_eq!(app.dismiss_exceptions(), 2);
        assert!(to_look_at(&app).is_empty());
        app.apply_agent_upgrades(vec![suspended(&a), suspended(&b), row(&c, 60)]);
        assert!(
            to_look_at(&app).is_empty(),
            "a dismissal holds when the upgrade names the fact again"
        );
    }

    /// A LINE ENDS WHEN THE WAIT DOES, WITH NO SECOND LOOK: the upgrade sends its
    /// rows when they change, so a tab that moved (or left) is one look that no
    /// longer names its line — which stays ten minutes, then goes at the
    /// digest's own clock (`settle_messages`) and is `Resolved` in the log, not
    /// a dismissal. Three tabs waiting on their own work are ONE line naming
    /// all three, and it goes on standing while any of them does.
    #[test]
    fn a_line_ends_when_the_wait_ends_and_many_tabs_are_one_line() {
        let (mut app, [a, b, c]) = three_tabs();
        // A day behind, no notice answered, the agent's own work under it.
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
        assert_eq!(lines[0].1.title, "Claude in 3 tabs is behind");
        assert!(
            lines[0].1.actions.is_empty(),
            "three tabs: no single tab to show"
        );
        let id = lines[0].1.id;
        // Two of them move: the line stands for the third.
        app.apply_agent_upgrades(vec![working(&c, day + 180)]);
        assert_eq!(to_look_at(&app).len(), 1);
        assert_eq!(
            to_look_at(&app)[0].1.title,
            "Claude in tab 3 is still on 2.1.281"
        );
        // The last moves: ten minutes on, the line goes, resolved, not filed.
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

    /// NO TAB EVER JOINS AN AGENT'S GROUP ROW (C6b will delete the group's code;
    /// until it does, nothing may reach it): two agents the upgrade ended whose
    /// give-up is a day old are BOTH an interrupt and `overdue`, the one
    /// shape that used to be folded into "Couldn't upgrade Claude in 2 tabs yet"
    /// — a warn row with snoozes. They stand as their own rows, each with its
    /// own action, and no group row is ever posted.
    #[test]
    fn no_tab_joins_an_agents_group_row() {
        let (mut app, [a, b, c]) = three_tabs();
        let ended = |tab: &str| Row {
            phase: Phase::Failed(aterm_agent::harness::upgrade::GAVE_UP.to_string()),
            exited_at: now_s() - 600,
            wait: String::new(),
            wait_since: 0,
            ..row(tab, 30 * 3_600)
        };
        for rows in [
            vec![ended(&a), ended(&b), ended(&c)],
            vec![ended(&a), row(&b, 30 * 3_600), ended(&c)],
        ] {
            app.apply_agent_upgrades(rows);
            assert!(group_of(&app).is_none(), "no agent's row for its tabs");
            assert!(
                app.messages.live_rows().all(|l| !l
                    .msg
                    .key
                    .as_deref()
                    .is_some_and(message_reporters::agent_upgrade_group_key_is)),
                "no group key is ever live"
            );
            assert!(
                row_of(&app, &a).is_some(),
                "each ended agent has its own row"
            );
            assert!(row_of(&app, &c).is_some());
        }
    }

    /// WORDS (round 35, D5): a round re-armed after a give-up that its own work
    /// holds (ruling 364) neither promises nor offers `Upgrade now`, which
    /// moved nothing sooner there — and still not a day on (the fact, not the
    /// age: `Row::will_ask_again`). NEGATIVE CONTROL: the same wait in a round
    /// that did not give up keeps `Upgrade now`.
    #[test]
    fn a_rearmed_own_work_round_promises_no_upgrade_now() {
        use aterm_messages::UpgradeWord;
        let a = "s-a";
        let now = now_s();
        // Its own work holds it (ruling 364's round): a turn running. (This
        // module's rows wait on a full queue, which `Upgrade now` moves.)
        let rearmed = Row {
            last_stop: aterm_agent::harness::upgrade::GAVE_UP.to_string(),
            wait: "not-idle:busy".to_string(),
            ..row(a, 8 * 3_600)
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
            ..row(a, 8 * 3_600)
        };
        assert!(!queued.asks_on_its_own(now));
        assert!(message_reporters::agent_upgrade_words(&queued, now).contains(&UpgradeWord::Now));
    }

    /// A WAIT NEVER SAYS `Couldn't` (ruling 423; the owner, 2026-09-28: "Couldn't
    /// upgrade Claude in tab 1 yet" read as a failure with nothing to do). Every
    /// wait the agent's own work, a person or the ladder makes - at an hour, a
    /// day and nine days behind - is a record under a title that says the upgrade
    /// waits, and none is a row. NEGATIVE CONTROL: a stop that did fail says so.
    #[test]
    fn a_wait_never_says_couldnt() {
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
            }
        }
        app.apply_agent_upgrades(rows);
        assert_eq!(app.messages.live_rows().count(), 0, "a wait is no row");
        for (title, _) in harness_records(&app) {
            assert!(
                !title.starts_with("Couldn't upgrade"),
                "a wait is not a failure: {title}"
            );
        }
        // NEGATIVE CONTROL: a stop that repeats did not work, and says so.
        app.apply_agent_upgrades(vec![Row {
            phase: Phase::Failed("no-resume".to_string()),
            stop_streak: 2,
            streak_why: "no-resume".to_string(),
            wait: String::new(),
            ..row("s-stop", 60)
        }]);
        assert!(
            harness_records(&app)
                .iter()
                .any(|(title, _)| title == "Couldn't upgrade Claude in its tab")
        );
    }

    /// ATERM'S OWN FAULT IS TOLD ONCE, AND A FAULT THAT RETURNS IS NEWS (ruling
    /// 383): a wait this build does not classify, two hours on, is one Warn record
    /// under its own key and one `aterm.log` line - never a row - however many
    /// looks see it; once it ends it is forgotten, so its return is a new record.
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
        let fault = records_titled(&app, "Upgrade stuck on aterm");
        assert_eq!(fault.len(), 1, "{fault:?}");
        assert!(
            fault[0].1[0].contains("zzz-unclassified"),
            "{:?}",
            fault[0].1
        );
        // The fault ends (the wait is one this build knows) and returns.
        app.apply_agent_upgrades(vec![row("s-a", 3 * 3_600)]);
        app.apply_agent_upgrades(vec![unclassified]);
        assert_eq!(told(&app), 2, "its return is news");
    }

    /// ONE WAITING UPGRADE IS ONE ENTRY THROUGH ITS ROUNDS (2026-09-29, the
    /// owner's `messages.log`, ids 51-56 over four hours and tab 2 again):
    /// the same upgrade, hours behind on its agent's own work, recorded
    /// "Couldn't upgrade Claude in tab 1 yet", then posted as a warn row,
    /// withdrawn a minute or two later and recorded afresh, every round.
    /// Replayed here as the host hands the rows over: a round asking on its
    /// own (each re-ask clears the recorded wait, the next look finds the
    /// work again), its give-up and rest, and the re-armed round's first
    /// looks (nothing recorded yet, a person at the tab, a draft typed at an
    /// idle point), three rounds of it. ONE entry under the tab's key, a
    /// record, never withdrawn; nothing on the glass. NEGATIVE CONTROL (run by
    /// reverting `Row::rest_carries_on`): each re-armed round posted a warn
    /// row and withdrew it at the next look, two more entries a round. And the
    /// escalation still reaches the band: a person's hold that stands a whole
    /// re-ask takes a row.
    ///
    /// ITS AGE MOVES IN PLACE (second review of 2026-09-29): the rounds run
    /// an hour apart, 7 h, 8 h and 9 h behind, and the one record's first
    /// detail line says each — still one entry, nothing on the glass.
    /// NEGATIVE CONTROL (run by reverting the record arm in
    /// `post_upgrade_stalls`): the record was written once and said `behind
    /// for 7 h` at 9 h.
    #[test]
    fn a_waiting_upgrade_is_one_entry_through_its_rounds() {
        use aterm_agent::harness::upgrade::{GAVE_UP, REASK_S};
        use aterm_agent::harness::upgrade_drive::HeldBy;
        let mut app = App::headless_for_test();
        let now = now_s();
        // Each round an hour further behind: 7 h, 8 h, 9 h (a minute into
        // the hour, so the second the test takes never moves the word).
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
                // The re-ask is an act: nothing recorded at the look after it.
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
            // Re-armed: the act cleared the wait and the shells under it.
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
        let (title, severity, _) = record_of(&app, id);
        // A wait never says `Couldn't` (ruling 423).
        assert_eq!(title, "Claude upgrade waits in its tab");
        assert_eq!(severity, aterm_messages::Severity::Info);

        // A draft that has stood a whole re-ask is a person's hold. Main's
        // design raised a row for it; under the owner's rule of 2026-09-28 it
        // is a wait like any other (nothing the owner can do about a person's
        // hold that is not their own typing): still one record, in place,
        // nothing on the glass.
        app.apply_agent_upgrades(vec![Row {
            wait_since: now - REASK_S,
            ..rearmed(&at_round(2), "draft")
        }]);
        assert_eq!(app.messages.live_rows().count(), 0, "a hold that stood");
        assert_eq!(entries(&app).len(), 1, "still the one record");
        let (_, _, detail) = record_of(&app, id);
        assert!(detail[0].starts_with("behind for 9 h"), "{detail:?}");
    }

    /// A WAITING UPGRADE'S ENTRY SAYS HOW LONG IN PLACE (2026-09-29, the
    /// owner's log: one waiting upgrade is one message that updates its
    /// `behind for` until it resolves). Under the owner's rule of 2026-09-28 a
    /// wait is a record, never a row: an hour on, the same stall's record
    /// reads `behind for 8 h`, re-worded in place, no new entry, and nothing on
    /// the glass. NEGATIVE CONTROLS: the same age restates nothing (the
    /// revision stands); before the fix the entry kept `behind for 7 h` for as
    /// long as it stood.
    #[test]
    fn a_waiting_upgrades_age_moves_in_place() {
        let mut app = App::headless_for_test();
        let first = row("s-a", 7 * 3_600 + 60);
        app.apply_agent_upgrades(vec![first.clone()]);
        assert_eq!(app.messages.live_rows().count(), 0, "a wait is no row");
        let [ref at_seven] = entries_of_s_a(&app)[..] else {
            panic!("one entry: {:#?}", entries_of_s_a(&app))
        };
        let id = at_seven.id;
        assert!(
            at_seven.detail[0].starts_with("behind for 7 h"),
            "{:?}",
            at_seven.detail
        );
        let revision = app.messages.revision();
        app.apply_agent_upgrades(vec![first]);
        assert_eq!(app.messages.revision(), revision, "the same age: nothing");

        app.apply_agent_upgrades(vec![row("s-a", 8 * 3_600 + 60)]);
        let [ref at_eight] = entries_of_s_a(&app)[..] else {
            panic!("still one entry: {:#?}", entries_of_s_a(&app))
        };
        assert_eq!(at_eight.id, id, "re-worded in place");
        assert!(
            at_eight.detail[0].starts_with("behind for 8 h"),
            "{:?}",
            at_eight.detail
        );
        assert_eq!(app.messages.live_rows().count(), 0, "still no row");
    }

    /// AN ENTRY WHOSE WORDS CARRY NO AGE IS NOT RESTATED AS IT AGES (2026-09-29
    /// review): `posted.behind` held `Row::behind_for` for every stall, so a
    /// held-back entry — `it runs inside vim …`, no age in it — was rebuilt
    /// and restated with the same words at every minute the host looked (the
    /// store absorbs an identical restatement, so the band's revision alone
    /// cannot see it: the posted age is what decides). NEGATIVE CONTROL:
    /// `a_waiting_upgrades_age_moves_in_place` (an overdue entry's age moves
    /// in place). Without `Row::age_said` here the posted age reads `5 min`,
    /// then `6 min`: a restatement at every look.
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

    /// The stall rows of the reshape and re-word tests: tab `s-a`, `h` hours
    /// (and a minute) behind, 2.1.283 → 2.1.284 — asking on its own at its
    /// own work (announced, `background`, a shell under it), given up and
    /// resting, re-armed at its own work, and re-armed with a draft typed
    /// (`stood`: for a whole re-ask, a person's hold, a row).
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

        fn draft(&self, h: u64, stood: bool) -> Row {
            use aterm_agent::harness::upgrade::{GAVE_UP, REASK_S};
            Row {
                phase: Phase::Pending,
                last_stop: GAVE_UP.to_string(),
                wait: "draft".to_string(),
                wait_since: if stood {
                    self.now - REASK_S
                } else {
                    self.now - 30
                },
                ..self.base(h)
            }
        }
    }

    /// Every entry under tab `s-a`'s key, oldest first.
    fn entries_of_s_a(app: &App) -> Vec<aterm_messages::LogRecord> {
        app.messages
            .log()
            .records()
            .filter(|r| r.key.as_deref() == Some("harness.upgrade.s-a"))
            .cloned()
            .collect()
    }

    /// Whether a stall entry's remedy line names only buttons it has
    /// (`message_reporters::agent_upgrade_stalled`, day five, D21): `Skip
    /// version keeps it` only beside its `Skip version`, `Skip This Version in
    /// the tab's menu` only where it has none, and `Upgrade now` offered as a
    /// remedy (`asks it`, `tries it`) only beside its `Upgrade now`.
    fn names_only_its_capsules(rec: &aterm_messages::LogRecord) -> Result<(), String> {
        let has = |word: UpgradeWord| {
            rec.actions
                .iter()
                .any(|i| matches!(i, Intent::AgentUpgrade { word: w, .. } if *w == word))
        };
        let remedy = rec.detail.get(1).map_or("", String::as_str);
        let wrong = if remedy.contains("Skip version keeps it") && !has(UpgradeWord::Skip) {
            "names a Skip version capsule it does not have"
        } else if remedy.contains("in the tab's menu") && has(UpgradeWord::Skip) {
            "sends Skip to the tab's menu beside its own Skip version"
        } else if (remedy.contains("Upgrade now asks it")
            || remedy.contains("Upgrade now tries it"))
            && !has(UpgradeWord::Now)
        {
            "offers an Upgrade now capsule it does not have"
        } else {
            return Ok(());
        };
        let labels: Vec<&str> = rec.actions.iter().map(Intent::label).collect();
        Err(format!("{wrong}: {remedy:?} with {labels:?}"))
    }

    /// A RE-WORDED RECORD NAMES ONLY THE BUTTONS IT HAS (review of
    /// 8899c91b4, the day-five D21 class back on Settings ▸ Messages): the
    /// record of one waiting upgrade, re-worded in place as it ages, took the
    /// title and detail of each look but kept the capsules it was written
    /// with. Written at an announced round its own work holds (`Not today`,
    /// `Skip version`), at the give-up an hour on it read `Upgrade now asks
    /// it at its next pause; Skip This Version in the tab's menu …`; written
    /// at the give-up (`Upgrade now`, `Not today`), an hour on it read `…
    /// Skip version keeps it on 2.1.283`. Now, through Announced →
    /// Failed(GAVE_UP) → Pending and the reverse, the one record's age moves
    /// every hour and every button its remedy names is its own. NEGATIVE
    /// CONTROL (run by reverting `reword_stall_record` to pass the fresh
    /// message whole, and the center's capsule guard): `written at its own
    /// work, look 1: sends Skip to the tab's menu beside its own Skip
    /// version` at the give-up.
    #[test]
    fn a_reworded_records_words_name_only_the_capsules_it_has() {
        let shapes = Shapes { now: now_s() };
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
            for (i, look) in looks.into_iter().enumerate() {
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
                if let Err(why) = names_only_its_capsules(rec) {
                    panic!("{name}, look {i}: {why}");
                }
            }
        }
    }

    /// ONE WAITING UPGRADE IS ONE ENTRY ACROSS ITS SHAPES (review of
    /// 8899c91b4, under the owner's rule of 2026-09-28): the entry's shape was
    /// part of what was said, so a record that needed the glass and a row
    /// whose upgrade asked on its own again each withdrew one entry and posted
    /// another. A wait is now a record at every age (a move clock's 24 h and a
    /// person's draft that stood a whole re-ask included: nothing the owner
    /// can do about a timer or a hold), so the one entry stays THE ONE ENTRY
    /// through every shape the stall takes: the same id, re-worded as it ages,
    /// never withdrawn, nothing on the glass, and `✓` only at Done.
    #[test]
    fn a_waiting_upgrade_is_one_entry_across_its_shapes() {
        use aterm_messages::{LogState, Retired};
        let shapes = Shapes { now: now_s() };
        let one = |app: &App| -> aterm_messages::LogRecord {
            let entries = entries_of_s_a(app);
            assert_eq!(entries.len(), 1, "one entry: {entries:#?}");
            assert_eq!(app.messages.live_rows().count(), 0, "nothing on the glass");
            entries[0].clone()
        };

        // The move clock: a record at 23 h, the same record at 24 h, done.
        let mut app = App::headless_for_test();
        app.apply_agent_upgrades(vec![shapes.announced(23)]);
        let record = one(&app);
        assert_eq!(record.state, LogState::Retired(Retired::Recorded));
        app.apply_agent_upgrades(vec![shapes.announced(24)]);
        let at_day = one(&app);
        assert_eq!(at_day.id, record.id, "the same record at the move clock");
        assert!(
            at_day.detail[0].starts_with("behind for 24 h"),
            "{:?}",
            at_day.detail
        );
        app.apply_agent_upgrades(vec![Row {
            phase: Phase::Done,
            ..shapes.base(24)
        }]);
        let done = one(&app);
        assert_eq!(done.id, record.id);
        assert_eq!(
            done.state,
            LogState::Retired(Retired::Recorded),
            "a wait's record is history: the move's own record is the ✓"
        );

        // A person's draft that stands, passes and stands again.
        let mut app = App::headless_for_test();
        app.apply_agent_upgrades(vec![shapes.announced(8)]);
        let first = one(&app).id;
        app.apply_agent_upgrades(vec![shapes.draft(8, true)]);
        assert_eq!(one(&app).id, first, "the draft stood: the same record");
        app.apply_agent_upgrades(vec![shapes.announced(8)]);
        let back = one(&app);
        assert_eq!(back.id, first, "asks on its own: the same record");
        assert_eq!(back.state, LogState::Retired(Retired::Recorded));
        assert_eq!(back.severity, aterm_messages::Severity::Info);
        // Its age moves in place.
        app.apply_agent_upgrades(vec![shapes.announced(9)]);
        let aged = one(&app);
        assert_eq!(aged.id, first);
        assert!(
            aged.detail[0].starts_with("behind for 9 h"),
            "{:?}",
            aged.detail
        );
        names_only_its_capsules(&aged).unwrap();
        app.apply_agent_upgrades(vec![shapes.draft(9, true)]);
        assert_eq!(one(&app).id, first, "the draft stood again: still it");
    }
}
