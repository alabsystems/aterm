// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One bounded account-menu read, with a latest pending request and owner fencing.
//!
//! A read is everything the Account card says that a file, a table or the journal holds
//! ([`AccountRead`]): this Mac's pool rows, and the card of the account the tab runs on
//! ([`aterm_agent::harness::account_card::build`]). The two fail apart: a roster that cannot be
//! read still leaves the card what the pool file, the claims table and the journal say, and the
//! card's reads never fail the rows. Each read runs on its own short-lived worker thread, so
//! what is worth keeping between two reads lives in [`State`]'s [`CardMemo`]: the history fold,
//! kept while the series files and the local day stand, and the pool file and claims table, read
//! at most once a minute.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_agent::harness::account_card::{self, AccountCard, History, HistoryIn};
use aterm_agent::harness::account_history::{self, HistoryWho};
use aterm_agent::harness::profile::AccountKey;
use aterm_agent::harness::{accounts, ai_pool, alab_seen, claims, footer::FactsOwner};

use crate::autopilot_menu::MenuOwner;
use crate::claude_lights::Fact;
use crate::fact_menu::{AccountMenuRow, SessionWeek};
use crate::session_timeline::{ForegroundStamp, SessionTimeline};
use crate::{App, Wake, WindowId};

/// This Mac's pool as the card lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PoolRows {
    /// One row an account, in roster order.
    pub(crate) rows: Vec<AccountMenuRow>,
    /// The shown name of the account this Mac's pool is pinned to (`None`: no committed pin; a
    /// pin on a row the roster no longer lists is that row's label).
    pub(crate) pinned: Option<String>,
    /// The saved manual selection's last observed queue place, without an ETA.
    pub(crate) queue: Option<String>,
}

/// One read for the Account card: the pool's rows, or why they could not be read, and the card
/// of the tab's account, which is built whether or not the rows were.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountRead {
    pub(crate) pool: Result<PoolRows, String>,
    /// `None` only where no read was made at all ([`AccountRead::failed`]).
    pub(crate) card: Option<AccountCard>,
}

impl AccountRead {
    /// A read that could not be made at all (no worker, no clock, a reader that stopped): no
    /// rows for `why`, and no card. It reads nothing, so the window's thread may make it.
    fn failed(why: impl Into<String>) -> Self {
        Self {
            pool: Err(why.into()),
            card: None,
        }
    }
}

/// How long a read of the pool file and the claims table serves the card's 2-second re-reads:
/// the table's read waits on the lab's database, and its rows move with claims that last an hour.
const MACHINES_EVERY_S: i64 = 60;

/// The local days of history a card's read spans: the seven weeks its daily row draws, and the
/// eight days of its hourly rows inside them.
const HISTORY_DAYS: i64 = 50;

/// What two reads of one card share.
#[derive(Default)]
struct CardMemo {
    history: Option<HistoryMemo>,
    machines: Option<MachinesMemo>,
    /// Which Mac this is, as the last pool read that worked named it, for a read whose pool
    /// read fails.
    this_mac: Option<ThisMac>,
}

/// This Mac as an accounts store names it: what tells its own rows in the pool file, the claims
/// table and the journal from another Mac's.
#[derive(Clone)]
struct ThisMac {
    cli: accounts::Cli,
    state_dir: PathBuf,
    machine: String,
    swarm_label: Option<String>,
    swarm_aliases: Vec<String>,
}

/// The last history fold and what it was folded from.
struct HistoryMemo {
    who: HistoryWho,
    state_dir: PathBuf,
    machine: String,
    /// [`account_history::stamp`] at the fold.
    stamp: u64,
    /// The local day of the fold: a day's turn moves every day's place.
    day: i64,
    history: Arc<History>,
}

/// The last read of the pool file and the claims table.
struct MachinesMemo {
    home: Option<PathBuf>,
    at: i64,
    read: Arc<Machines>,
}

struct Machines {
    pool: Option<ai_pool::Pool>,
    claims: Result<Option<(claims::View, claims::Book)>, String>,
}

/// The local clock's offset from UTC at `t`, seconds east; UTC where the zone cannot be read.
fn local_tz(t: i64) -> i64 {
    crate::autopilot_timers::local_offset_at(t).unwrap_or(0)
}

impl Key {
    /// `other` is this key but for the session's own week reading: the same tab on the same
    /// account, read again.
    fn same_but_week(&self, other: &Key) -> bool {
        Key {
            week: other.week.clone(),
            ..self.clone()
        } == *other
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    session: u64,
    timeline: usize,
    foreground: Option<ForegroundStamp>,
    pgid: i32,
    birth: Option<u64>,
    owner: Option<FactsOwner>,
    cli: accounts::Cli,
    account: Option<String>,
    email: Option<String>,
    /// The account key the session's profile names: what finds the history of a login no roster
    /// row lists.
    profile_key: Option<AccountKey>,
    week: SessionWeek,
    config: Option<PathBuf>,
    state: Option<PathBuf>,
    home: Option<PathBuf>,
}

#[derive(Clone)]
struct Input {
    key: Key,
    // Keep this allocation alive so an old request cannot match a reused address.
    _owner: Arc<Mutex<SessionTimeline>>,
}

struct Active {
    generation: u64,
    wid: WindowId,
    input: Input,
    rows: Option<Arc<AccountRead>>,
    read_at: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct State {
    generation: u64,
    active: Option<Active>,
    in_flight: Option<u64>,
    pending: Option<(u64, Input)>,
    deferred_keyboard_selection: bool,
    /// What one read leaves the next, across the worker threads ([`CardMemo`]).
    memo: Arc<Mutex<CardMemo>>,
    #[cfg(test)]
    hold_for_test: bool,
}

#[derive(Debug)]
pub(crate) struct Completion {
    generation: u64,
    rows: AccountRead,
}

impl State {
    pub(crate) fn close(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.active = None;
        self.pending = None;
        self.deferred_keyboard_selection = false;
    }

    pub(crate) fn defer_keyboard_selection(&mut self, defer: bool) {
        self.deferred_keyboard_selection = defer;
    }

    pub(crate) fn cancel_keyboard_selection(&mut self) {
        self.deferred_keyboard_selection = false;
    }

    fn request(&mut self, wid: WindowId, input: Input, now: Instant) {
        let same = self
            .active
            .as_ref()
            .filter(|active| active.wid == wid && active.input.key == input.key);
        if let Some(active) = same
            && (self.in_flight == Some(active.generation)
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|(generation, _)| *generation == active.generation)
                || active
                    .read_at
                    .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_secs(2)))
        {
            return;
        }
        // A new week reading of the same tab on the same account moves only that account's
        // figure: the last read stays shown until the read it asks for lands, so the card's
        // sections, the pool's items and a highlight on one hold their places meanwhile.
        let kept = self
            .active
            .as_ref()
            .filter(|active| active.wid == wid && active.input.key.same_but_week(&input.key));
        let (rows, read_at) =
            kept.map_or((None, None), |active| (active.rows.clone(), active.read_at));
        if self.active.is_some() && kept.is_none() {
            self.deferred_keyboard_selection = false;
        }
        self.generation = self.generation.saturating_add(1);
        self.active = Some(Active {
            generation: self.generation,
            wid,
            input: input.clone(),
            rows,
            read_at,
        });
        self.pending = Some((self.generation, input));
    }
}

impl App {
    /// A saved choice supersedes every ready or in-flight observation of the earlier choice.
    pub(crate) fn account_menu_selection_changed(&mut self) {
        self.account_menu_read.close();
    }

    pub(crate) fn account_menu_read_deadline(&self) -> Option<Instant> {
        let menu = self.autopilot_menu.as_ref()?;
        if menu.owner != MenuOwner::Fact(Fact::Account) {
            return None;
        }
        if self
            .windows
            .get(&menu.wid)
            .is_none_or(|window| window.occluded)
        {
            return None;
        }
        let active = self.account_menu_read.active.as_ref()?;
        if active.wid != menu.wid
            || active.input.key.session != menu.session
            || self.account_menu_read.in_flight == Some(active.generation)
            || self
                .account_menu_read
                .pending
                .as_ref()
                .is_some_and(|(generation, _)| *generation == active.generation)
        {
            return None;
        }
        active.read_at.map(|at| at + Duration::from_secs(2))
    }

    pub(crate) fn service_account_menu_read(&mut self, now: Instant) {
        if self
            .account_menu_read_deadline()
            .is_some_and(|at| now >= at)
            && let Some(session) = self.autopilot_menu.as_ref().map(|menu| menu.session)
        {
            self.refresh_fact_menu(session);
        }
    }

    fn account_menu_input(&self, session: u64) -> Option<Input> {
        let entry = self.pool.get(session)?;
        let owner = entry.ctx.timeline.clone();
        let timeline = owner.lock().unwrap_or_else(|p| p.into_inner());
        let cli = timeline
            .agent()
            .reader
            .and_then(crate::harness_host::pool_cli)?;
        let pgid = timeline.agent().program_pgid;
        let birth = timeline
            .claude_footer_identity()
            .filter(|(pid, _)| *pid == pgid)
            .and_then(|(_, birth)| birth);
        let facts = timeline.claude_footer();
        let mut week = SessionWeek::default();
        let now = aterm_time::unix_secs()
            .and_then(|v| i64::try_from(v).ok())
            .unwrap_or(0);
        if cli == accounts::Cli::Claude
            && let Some(sub) = facts.and_then(|f| f.subscription.as_ref())
        {
            let walled = facts
                .and_then(|f| f.usage.as_ref())
                .and_then(|u| u.wall.as_ref())
                .is_some_and(|w| w.label == "7d");
            week.said = crate::fact_menu::weekly_facts(sub, walled, now).0;
            week.figure = crate::fact_menu::session_figure(sub, now);
            week.label.clone_from(&sub.label);
        }
        let home = aterm_primer::home_dir();
        // Only environment-to-path projection here. The platform fallback can read passwd,
        // and the test-root fallback can create directories, so both belong to the worker.
        let config = aterm_types::dirs::resolve_aterm_config_path(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            std::env::var_os("APPDATA").as_deref(),
            std::env::var_os("HOME").as_deref(),
        );
        let state = aterm_types::dirs::resolve_state_dir(
            aterm_types::dev_seam!("ATERM_STATE_HOME").map(PathBuf::from),
            aterm_types::dirs::StatePlatform {
                home: home.clone().filter(|path| path.is_absolute()),
                xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
                local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            },
        )
        .map(|root| {
            root.join("harness")
                .join(aterm_agent::harness::cli::HARNESS)
        });
        let key = Key {
            session,
            timeline: Arc::as_ptr(&owner) as usize,
            foreground: timeline.foreground_stamp(),
            pgid,
            birth,
            owner: facts.and_then(|f| f.owner.clone()),
            cli,
            account: (cli == accounts::Cli::Claude)
                .then(|| facts.and_then(|f| f.account.clone()))
                .flatten(),
            email: (cli == accounts::Cli::Claude)
                .then(|| {
                    facts
                        .and_then(|f| f.subscription.as_ref())
                        .and_then(|s| s.email.clone())
                })
                .flatten(),
            profile_key: (cli == accounts::Cli::Claude)
                .then(|| {
                    facts
                        .and_then(|f| f.subscription.as_ref())
                        .and_then(|s| s.account)
                })
                .flatten(),
            week,
            config,
            state,
            home,
        };
        drop(timeline);
        Some(Input { key, _owner: owner })
    }

    pub(crate) fn prepare_account_menu_read(&mut self, wid: WindowId, session: u64) {
        let Some(input) = self.account_menu_input(session) else {
            self.account_menu_read.close();
            return;
        };
        self.account_menu_read.request(wid, input, Instant::now());
        self.dispatch_account_menu_read();
    }

    pub(crate) fn account_menu_rows_cached(&self, session: u64) -> Option<&AccountRead> {
        let input = self.account_menu_input(session)?;
        self.account_menu_read
            .active
            .as_ref()
            .filter(|active| active.input.key == input.key)
            .and_then(|active| active.rows.as_deref())
    }

    fn dispatch_account_menu_read(&mut self) {
        if self.account_menu_read.in_flight.is_some() {
            return;
        }
        let Some((generation, input)) = self.account_menu_read.pending.take() else {
            return;
        };
        self.account_menu_read.in_flight = Some(generation);
        #[cfg(test)]
        if self.account_menu_read.hold_for_test {
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            self.on_account_menu_read(Completion {
                generation,
                rows: AccountRead::failed("background account delivery is unavailable"),
            });
            return;
        };
        let memo = Arc::clone(&self.account_menu_read.memo);
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-account-menu",
            crate::qos::Role::Background,
            move || {
                let done = complete_read(generation, || read_account(&input, &memo));
                let _ = proxy.send_event(Wake::AccountMenuRead(done));
            },
        );
        if let Err(error) = spawned {
            self.on_account_menu_read(Completion {
                generation,
                rows: AccountRead::failed(format!("account reader could not start: {error}")),
            });
        }
    }

    pub(crate) fn on_account_menu_read(&mut self, completion: Completion) {
        if self.account_menu_read.in_flight != Some(completion.generation) {
            return;
        }
        self.account_menu_read.in_flight = None;
        let matching = self.account_menu_read.active.as_ref().and_then(|active| {
            let menu = self.autopilot_menu.as_ref()?;
            (active.generation == completion.generation
                && menu.wid == active.wid
                && self.windows.contains_key(&menu.wid)
                && menu.session == active.input.key.session
                && menu.owner == MenuOwner::Fact(Fact::Account))
            .then_some((
                active.wid,
                active.input.key.session,
                active.input.key.clone(),
            ))
        });
        let mut accepted = false;
        if let Some((_, session, key)) = matching
            && self
                .account_menu_input(session)
                .is_some_and(|input| input.key == key)
            && let Some(active) = self.account_menu_read.active.as_mut()
        {
            active.rows = Some(Arc::new(completion.rows));
            active.read_at = Some(Instant::now());
            accepted = true;
        }
        if let Some((wid, session)) = self.autopilot_menu.as_ref().and_then(|menu| {
            (menu.owner == MenuOwner::Fact(Fact::Account) && self.windows.contains_key(&menu.wid))
                .then_some((menu.wid, menu.session))
        }) {
            let changed = self.account_menu_read.active.as_ref().is_none_or(|active| {
                self.account_menu_input(session)
                    .is_none_or(|input| active.input.key != input.key)
            });
            if changed {
                self.prepare_account_menu_read(wid, session);
            }
            self.rebuild_fact_menu_rows(session);
            if accepted
                && std::mem::take(&mut self.account_menu_read.deferred_keyboard_selection)
                && let Some(menu) = self
                    .autopilot_menu
                    .as_mut()
                    .filter(|menu| menu.highlight.is_none())
            {
                menu.highlight = crate::autopilot_menu::step(&menu.rows, None, true);
                menu.reveal_highlight();
                self.request_menu_frame(wid);
            }
        } else {
            self.account_menu_read.close();
        }
        self.dispatch_account_menu_read();
    }
}

fn unix_now() -> Option<i64> {
    aterm_time::unix_secs().and_then(|value| i64::try_from(value).ok())
}

/// One read for `input`'s card, on the worker: the pool's rows and the account's card.
fn read_account(input: &Input, memo: &Mutex<CardMemo>) -> AccountRead {
    match unix_now() {
        Some(now) => read_account_at(&input.key, memo, now),
        None => AccountRead::failed("account clock is unavailable"),
    }
}

/// [`read_account`] at `now`: the pool read as before, then the card's own reads (the series,
/// the pool file, the claims table, the journal), each kept in `memo` while it stands.
fn read_account_at(key: &Key, memo: &Mutex<CardMemo>, now: i64) -> AccountRead {
    let places = || -> Result<(PathBuf, PathBuf), String> {
        let config = key
            .config
            .clone()
            .or_else(aterm_types::dirs::aterm_config_path)
            .ok_or("account configuration path is unavailable")?;
        let state_dir = key
            .state
            .clone()
            .or_else(crate::harness_host::harness_state)
            .ok_or("account state directory is unavailable")?;
        Ok((config, state_dir))
    };
    let places = places();
    let loaded = places.as_ref().map_err(Clone::clone).and_then(|places| {
        crate::fact_menu::PoolRead::load(key.cli, (&places.0, &places.1, key.home.as_deref()), now)
    });
    let pool = loaded.as_ref().map_err(Clone::clone).map(|read| {
        read.rows(
            (key.account.as_deref(), key.email.as_deref()),
            key.week.clone(),
            now,
        )
    });
    let state_dir = places.as_ref().ok().map(|places| places.1.as_path());
    AccountRead {
        pool,
        card: Some(card_of(key, loaded.as_ref().ok(), state_dir, memo, now)),
    }
}

/// The card of `key`'s account at `now` from what `pool` read (`None`: the pool read failed, and
/// the card is built over an empty roster, so it names the account by the session's own address
/// and its Macs by the pool file, the claims table and the journal alone). Which of those Macs
/// is this one is the store's to say: a read that worked leaves it in `memo`, and a read that
/// failed takes it from there, else from the store's own state file, so this Mac is never
/// spoken of as another.
fn card_of(
    key: &Key,
    pool: Option<&crate::fact_menu::PoolRead>,
    state_dir: Option<&std::path::Path>,
    memo: &Mutex<CardMemo>,
    now: i64,
) -> AccountCard {
    let this_mac = this_mac_of(key, pool, state_dir, memo, now);
    let empty = (
        accounts::Roster::default(),
        accounts::State {
            machine: this_mac
                .as_ref()
                .map(|mac| mac.machine.clone())
                .unwrap_or_default(),
            swarm_label: this_mac.as_ref().and_then(|mac| mac.swarm_label.clone()),
            swarm_aliases: this_mac.map(|mac| mac.swarm_aliases).unwrap_or_default(),
            ..accounts::State::default()
        },
        std::collections::BTreeSet::new(),
    );
    let (roster, state, enrolled) = pool.map_or((&empty.0, &empty.1, &empty.2), |read| {
        (&read.roster, &read.state, &read.enrolled)
    });
    let logins = pool
        .and_then(|read| read.store.load_logins(now).ok())
        .map(|(logins, _)| logins)
        .unwrap_or_default();
    let session = account_card::Session {
        label: key.week.label.as_deref().or(key.account.as_deref()),
        email: key.email.as_deref(),
        profile_key: key.profile_key,
    };
    let who = account_card::who_of(key.cli, roster, state, enrolled, &session);
    // A history is this Mac's series under its own machine id, which only the store names.
    let history_of = pool
        .zip(state_dir)
        .and_then(|_| account_card::history_who(key.cli, roster, state, &session, &who))
        .zip(state_dir);
    let day = (now + local_tz(now)).div_euclid(86_400);
    let (kept, fresh) = match &history_of {
        None => (None, None),
        Some((who, state_dir)) => {
            let stamp = account_history::stamp(state_dir, &state.machine);
            let kept = memo
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .history
                .as_ref()
                .filter(|kept| {
                    kept.who == *who
                        && kept.state_dir == *state_dir
                        && kept.machine == state.machine
                        && kept.stamp == stamp
                        && kept.day == day
                })
                .map(|kept| Arc::clone(&kept.history));
            match kept {
                Some(kept) => (Some(kept), None),
                None => {
                    #[cfg(test)]
                    crate::work_counts::account_history_folded();
                    let read = account_history::read(
                        state_dir,
                        &state.machine,
                        who,
                        HISTORY_DAYS * 86_400,
                        local_tz,
                        now,
                    );
                    (None, Some((read, stamp)))
                }
            }
        }
    };
    let machines = machines_of(key.home.as_deref(), memo, now);
    let journal = alab_seen::journal();
    let card = account_card::build(&account_card::Inputs {
        cli: key.cli,
        roster,
        state,
        enrolled,
        logins: &logins,
        session,
        pool: machines.pool.as_ref(),
        claims: &machines.claims,
        journal: &journal,
        history: match (&kept, &fresh) {
            (Some(kept), _) => HistoryIn::Kept(Arc::clone(kept)),
            (None, Some((read, stamp))) => HistoryIn::Read {
                read,
                stamp: *stamp,
            },
            (None, None) => HistoryIn::NotRead,
        },
        now,
        tz_at: local_tz,
    });
    if let (Some((who, state_dir)), Some((_, stamp))) = (history_of, fresh) {
        memo.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .history = Some(HistoryMemo {
            who,
            state_dir: state_dir.to_path_buf(),
            machine: state.machine.clone(),
            stamp,
            day,
            history: Arc::clone(&card.history),
        });
    }
    card
}

/// Which Mac this is for `key`'s card: what `pool` read names (kept in `memo` for a later read
/// that fails), else what `memo` keeps for the same store, else the identity in the store's own
/// state file read apart from the roster, whose failure is what brought the read here.
fn this_mac_of(
    key: &Key,
    pool: Option<&crate::fact_menu::PoolRead>,
    state_dir: Option<&std::path::Path>,
    memo: &Mutex<CardMemo>,
    now: i64,
) -> Option<ThisMac> {
    let state_dir = state_dir?;
    let of = |state: &accounts::State| ThisMac {
        cli: key.cli,
        state_dir: state_dir.to_path_buf(),
        machine: state.machine.clone(),
        swarm_label: state.swarm_label.clone(),
        swarm_aliases: state.swarm_aliases.clone(),
    };
    {
        let mut memo = memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(read) = pool {
            memo.this_mac = Some(of(&read.state));
            return memo.this_mac.clone();
        }
        if let Some(kept) = memo
            .this_mac
            .as_ref()
            .filter(|kept| kept.cli == key.cli && kept.state_dir == state_dir)
        {
            return Some(kept.clone());
        }
    }
    let store = match (key.cli, key.home.as_deref()) {
        (_, Some(home)) => accounts::Store::at_for(state_dir, key.cli, home),
        (accounts::Cli::Claude, None) => accounts::Store::at(state_dir),
        (_, None) => return None,
    };
    let (state, _) = store.load(now).ok()?;
    Some(of(&state))
}

/// The pool file and the claims table under `home` at `now`: the read `memo` keeps while it is
/// under [`MACHINES_EVERY_S`] old and the clock has not stepped back, else a new one. With no
/// home there is no pool file and no table to ask.
fn machines_of(home: Option<&std::path::Path>, memo: &Mutex<CardMemo>, now: i64) -> Arc<Machines> {
    let kept = memo
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .machines
        .as_ref()
        .filter(|kept| {
            kept.home.as_deref() == home && (0..MACHINES_EVERY_S).contains(&(now - kept.at))
        })
        .map(|kept| Arc::clone(&kept.read));
    if let Some(kept) = kept {
        return kept;
    }
    #[cfg(test)]
    crate::work_counts::account_machines_read();
    let read = Arc::new(Machines {
        pool: home.and_then(ai_pool::read_under),
        claims: home.map_or(Ok(None), claims_of),
    });
    memo.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .machines = Some(MachinesMemo {
        home: home.map(std::path::Path::to_path_buf),
        at: now,
        read: Arc::clone(&read),
    });
    read
}

/// The claims table under `home` ([`claims::read_book`]). A read that failed on a Mac with no
/// vault is the table's absence, never a read that failed just now: a Mac in no lab holds no
/// table, and the card then says it does not read other Macs.
fn claims_of(home: &std::path::Path) -> Result<Option<(claims::View, claims::Book)>, String> {
    let read = claims::read_book(home);
    #[cfg(unix)]
    if read.is_err()
        && matches!(
            aterm_secrets::client::config(&aterm_secrets::client::Paths::for_home(home)),
            Ok(None)
        )
    {
        return Ok(None);
    }
    read
}

pub(crate) fn queue_status(
    store: &accounts::Store,
    cli: accounts::Cli,
    roster: &accounts::Roster,
    state: &accounts::State,
    now: i64,
) -> Option<String> {
    let activity =
        aterm_agent::harness::accounts_sync::login_activity_batch([Some(store), None], cli, now)?;
    queue_status_from(store, cli, roster, state, &activity)
}

fn queue_status_from(
    store: &accounts::Store,
    cli: accounts::Cli,
    roster: &accounts::Roster,
    state: &accounts::State,
    activity: &aterm_agent::harness::accounts_sync::LoginActivityBatch,
) -> Option<String> {
    let selection = state.pending_pin.as_ref()?;
    let (name, queued) = match selection {
        accounts::PendingSelection::Account(request) => {
            let row = roster.row(request.label())?;
            let queued = activity
                .for_store(store, cli)?
                .iter()
                .find(|entry| entry.matches_row(row))?
                .queued
                .as_ref()?;
            (format!("Account {}", row.shown_name()), queued)
        }
        accounts::PendingSelection::Next(_) => (
            "Next account".to_owned(),
            activity.queued_next_for(store, cli)?,
        ),
    };
    if !queued.matches_selection(selection) {
        return None;
    }
    let position = queued.position();
    let blocker = match position.blocker {
        accounts::SigninBlocker::ManualAhead => "earlier manual sign-in",
        accounts::SigninBlocker::UnscheduledSignin => "another sign-in holds admission",
        accounts::SigninBlocker::None => "awaiting admission",
    };
    Some(format!(
        "{} ahead · {name}: sign-in queued · {blocker}",
        position.ahead
    ))
}

fn complete_read(generation: u64, read: impl FnOnce() -> AccountRead) -> Completion {
    let rows = std::panic::catch_unwind(std::panic::AssertUnwindSafe(read)).unwrap_or_else(|_| {
        AccountRead::failed("account reader stopped before completing the read")
    });
    Completion { generation, rows }
}

#[cfg(test)]
impl App {
    /// Hold every account read in flight, as the worker had not answered yet.
    pub(crate) fn hold_account_menu_reads_for_test(&mut self) {
        self.account_menu_read.hold_for_test = true;
    }

    /// Answer the account read in flight with `read`, as the worker would: whether one was.
    pub(crate) fn complete_account_menu_read_for_test(&mut self, read: AccountRead) -> bool {
        let Some(generation) = self.account_menu_read.in_flight else {
            return false;
        };
        self.on_account_menu_read(Completion {
            generation,
            rows: read,
        });
        true
    }
}

#[cfg(test)]
impl AccountRead {
    /// A test's read of pool `rows` (none pinned), or the reason there are none, with a card
    /// that knows no account.
    pub(crate) fn for_test(rows: Result<Vec<AccountMenuRow>, String>) -> Self {
        Self {
            pool: rows.map(|rows| PoolRows {
                rows,
                pinned: None,
                queue: None,
            }),
            card: Some(Self::card_for_test()),
        }
    }

    /// A test's read of pool `rows` (none pinned) with the `card` its worker built.
    pub(crate) fn with_card_for_test(rows: Vec<AccountMenuRow>, card: AccountCard) -> Self {
        Self {
            pool: Ok(PoolRows {
                rows,
                pinned: None,
                queue: None,
            }),
            card: Some(card),
        }
    }

    /// A card of no account: an empty roster, no pool file, no claims table, no journal.
    pub(crate) fn card_for_test() -> AccountCard {
        account_card::build(&account_card::Inputs {
            cli: accounts::Cli::Claude,
            roster: &accounts::Roster::default(),
            state: &accounts::State::default(),
            enrolled: &std::collections::BTreeSet::new(),
            logins: &accounts::Logins::default(),
            session: account_card::Session::default(),
            pool: None,
            claims: &Ok(None),
            journal: &alab_seen::Journal::Off,
            history: HistoryIn::NotRead,
            now: 0,
            tz_at: |_| 0,
        })
    }
}

#[cfg(test)]
#[path = "account_menu_read_tests.rs"]
mod tests;
