// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What becomes of a login a tab's CLI was observed to run on
//! (`docs/DESIGN-account-discovery-2026-11-09.md`, step 1 H): the harness hands each due sighting
//! to the journal sink (`aterm_agent::harness::journal::observed`), the sink hands it here, and in
//! a release build that holds a lab ([`crate::alab_view::holds`]) one thread of its own runs
//! `aterm_db::alab::Registry::observe_accounts` over what gathered: an account no row names is
//! born, and this Mac's `account-obs` row of each says it runs here, in how many tabs. Sightings
//! of one login coalesce, a pass runs at most once a minute, and a tab never waits on it.
//!
//! A tab's sighting comes when it is due (its first, a change of account, then hourly), so one
//! pass's gathering is no count of the tabs on a login: the queue keeps each tab's login and its
//! last sighting across passes, and a login's count is its tabs sighted within [`TAB_LIVE_S`]
//! that have not since ended or moved on. A login whose count moved by a tab's expiry is passed
//! again with the next pass. A test, a debug
//! build or a checkout's `targo run` never installs it, so nothing reaches a live registry from
//! there.
//!
//! The machine sweep (step 4) starts here too ([`start_sweep`]): in a process that lives on and
//! holds a lab, one thread reads every login place on this Mac every [`SWEEP_EVERY`]
//! (`aterm_agent::harness::observe::sweep`), so a login no tab runs is noted as a `seen` with no
//! tab, once a day and when its file names another account, and marked `idle` through the same
//! queue. The sweep hands each login at every look and the queue keeps what a pass took
//! ([`Queue::passed`]): a swept login goes to the registry once a day, and again at each look
//! until a pass of it succeeds, in whichever process looks.

use aterm_agent::harness::journal::LoginState;
use aterm_agent::harness::journal::ObservedLogin;
use aterm_agent::harness::observe::sweep::{IDLE_REFRESH_S, OnToken};
use aterm_db::alab::{Observed, observe::Sighting};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

/// The least time between two passes: the registry's rows change far more slowly
/// ([`aterm_db::alab::observe::REWRITE_DAYS`]), so one pass a minute gathers every tab's news.
const EVERY: Duration = Duration::from_secs(60);

/// How long a tab counts as running the login it was last sighted on: two of its hourly
/// refreshes (`aterm_agent::harness::observe::SEEN_REFRESH_S`), so one late refresh does not
/// drop it, and a tab that ended unheard falls out within two hours.
const TAB_LIVE_S: u64 = 2 * aterm_agent::harness::observe::SEEN_REFRESH_S.unsigned_abs();

/// A login by (CLI, claims realm, claims email).
type Login = (&'static str, String, String);

/// One login as its newest sighting said it, until the next pass takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Gathered {
    state: &'static str,
    src: &'static str,
    at: u64,
}

/// What waits for the next pass, each tab's login and last sighting, the count of tabs each
/// login's last pass took, when a pass that read the registry last took each login (Unix
/// seconds), and whether a pass's thread runs and when the last one ended.
#[derive(Default)]
struct Queue {
    by: BTreeMap<Login, Gathered>,
    tabs: BTreeMap<String, (Login, u64)>,
    counted: BTreeMap<Login, (u64, &'static str)>,
    passed: BTreeMap<(String, String, String), u64>,
    running: bool,
    last: Option<Instant>,
}

impl Queue {
    /// Gather `login`: the newest sighting's state and source; a running tab now on it (and on
    /// no other), or a tab that ended off it. A login with no tab is the machine sweep's, handed
    /// at every look: it is gathered unless a pass took the login within the day
    /// ([`Queue::took`]), so it is passed once a day and again until a pass succeeds.
    fn gather(&mut self, login: &ObservedLogin) {
        let key: Login = (
            login.cli.id(),
            login.realm.clone(),
            login.email.trim().to_ascii_lowercase(),
        );
        if login.tab.is_none()
            && self
                .passed
                .get(&(key.0.to_owned(), key.1.clone(), key.2.clone()))
                .is_some_and(|at| *at <= login.at && login.at - *at < IDLE_REFRESH_S.unsigned_abs())
        {
            return;
        }
        if let Some(tab) = login.tab {
            let tab = tab.as_str().to_owned();
            match login.state {
                LoginState::Running => {
                    if let Some((before, _)) = self.tabs.get(&tab)
                        && *before != key
                        && let Some((_, src)) = self.counted.get(before).copied()
                    {
                        // The login the tab left is passed again, its count one fewer.
                        let before = before.clone();
                        self.by.entry(before).or_insert(Gathered {
                            state: LoginState::Running.word(),
                            src,
                            at: login.at,
                        });
                    }
                    self.tabs.insert(tab, (key.clone(), login.at));
                }
                LoginState::Idle | LoginState::SignedOut => {
                    if self.tabs.get(&tab).is_some_and(|(on, _)| *on == key) {
                        self.tabs.remove(&tab);
                    }
                }
            }
        }
        let entry = self.by.entry(key).or_insert_with(|| Gathered {
            state: login.state.word(),
            src: login.src.word(),
            at: login.at,
        });
        if login.at >= entry.at {
            entry.state = login.state.word();
            entry.src = login.src.word();
            entry.at = login.at;
        }
    }

    /// Everything gathered, and each login whose count of live tabs moved since its last pass,
    /// as the registry's planner reads them at `now` (Unix seconds), leaving the queue empty.
    /// A login some live tab runs is `running`, whatever its newest sighting said; one no tab
    /// runs any more is `idle`.
    fn take(&mut self, now: u64) -> Vec<Sighting> {
        self.tabs
            .retain(|_, (_, at)| at.saturating_add(TAB_LIVE_S) >= now);
        let mut live: BTreeMap<&Login, u64> = BTreeMap::new();
        for (login, _) in self.tabs.values() {
            *live.entry(login).or_default() += 1;
        }
        let moved: Vec<(Login, Gathered)> = self
            .counted
            .iter()
            .filter(|(login, (tabs, _))| live.get(login).copied().unwrap_or(0) != *tabs)
            .filter(|(login, _)| !self.by.contains_key(*login))
            .map(|(login, (_, src))| {
                (
                    login.clone(),
                    Gathered {
                        state: LoginState::Idle.word(),
                        src,
                        at: now,
                    },
                )
            })
            .collect();
        let live: BTreeMap<Login, u64> = live
            .into_iter()
            .map(|(login, n)| (login.clone(), n))
            .collect();
        std::mem::take(&mut self.by)
            .into_iter()
            .chain(moved)
            .map(|(login, seen)| {
                let tabs = live.get(&login).copied().unwrap_or(0);
                let state = if tabs > 0 {
                    LoginState::Running.word()
                } else if seen.state == LoginState::Running.word() {
                    LoginState::Idle.word()
                } else {
                    seen.state
                };
                if tabs == 0 {
                    self.counted.remove(&login);
                } else {
                    self.counted.insert(login.clone(), (tabs, seen.src));
                }
                let (cli, realm, email) = login;
                Sighting {
                    cli: cli.to_owned(),
                    realm,
                    email,
                    state: state.to_owned(),
                    tabs,
                    src: seen.src.to_owned(),
                    at_ms: seen.at.saturating_mul(1_000),
                }
            })
            .collect()
    }

    /// Keep that a pass at `now` (Unix seconds) read the registry with `sightings`: each login
    /// is passed, so the sweep's next sighting of it within the day is not gathered. Logins
    /// passed more than a day ago are dropped.
    fn took(&mut self, sightings: &[Sighting], now: u64) {
        self.passed
            .retain(|_, at| now.saturating_sub(*at) < IDLE_REFRESH_S.unsigned_abs());
        for seen in sightings {
            let login = (seen.cli.clone(), seen.realm.clone(), seen.email.clone());
            self.passed.insert(login, now);
        }
    }

    /// Whether a pass's thread should start now: something waits and none runs. How long it
    /// first sleeps is [`Queue::wait`].
    fn start(&mut self) -> bool {
        if self.running || self.by.is_empty() {
            return false;
        }
        self.running = true;
        true
    }

    /// How long a starting pass waits so passes stay [`EVERY`] apart.
    fn wait(&self, now: Instant) -> Duration {
        self.last.map_or(Duration::ZERO, |last| {
            EVERY.saturating_sub(now.saturating_duration_since(last))
        })
    }
}

/// The queue, once [`install`] found this process holds a lab.
static QUEUE: OnceLock<Mutex<Queue>> = OnceLock::new();

/// Take up `login` from the journal sink: gather it, and start a pass's thread where none runs.
/// Inert where [`install`] did not run.
pub(crate) fn take_up(login: &ObservedLogin) {
    let Some(queue) = QUEUE.get() else {
        return;
    };
    let start = match queue.lock() {
        Ok(mut queue) => {
            queue.gather(login);
            queue.start()
        }
        Err(_) => false,
    };
    if !start {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("aterm-lab-observed".into())
        .spawn(move || run(queue, pass));
    if spawned.is_err() {
        if let Ok(mut queue) = queue.lock() {
            queue.running = false;
        }
        aterm_log::debug!("lab account observation: no thread");
    }
}

/// A pass's thread: wait out the rest period, take what gathered, pass it, and go again while
/// more gathered meanwhile.
fn run(queue: &Mutex<Queue>, pass: fn(&[Sighting]) -> Result<Observed, String>) {
    /// Clears `running` however the thread ends, a panic in a pass included: a thread that died
    /// with it set would leave every later sighting of this process unpassed.
    struct Running<'a>(&'a Mutex<Queue>);
    impl Drop for Running<'_> {
        fn drop(&mut self) {
            let mut queue = match self.0.lock() {
                Ok(queue) => queue,
                Err(poisoned) => poisoned.into_inner(),
            };
            queue.running = false;
        }
    }
    let _running = Running(queue);
    loop {
        let wait = match queue.lock() {
            Ok(queue) => queue.wait(Instant::now()),
            Err(_) => return,
        };
        std::thread::sleep(wait);
        let now = aterm_time::unix_millis().unwrap_or(0) / 1_000;
        let sightings = match queue.lock() {
            Ok(mut queue) => queue.take(now),
            Err(_) => return,
        };
        let done = pass(&sightings);
        log_pass(&done);
        let Ok(mut queue) = queue.lock() else {
            return;
        };
        // Only a pass whose plan ran took its sightings: one that failed, or read no registry
        // (`quiet`), leaves each swept login to be handed again at the sweep's next look.
        if done.is_ok_and(|done| done.quiet.is_none()) {
            queue.took(&sightings, now);
        }
        queue.last = Some(Instant::now());
        if queue.by.is_empty() {
            queue.running = false;
            return;
        }
    }
}

/// One pass's line, by counts and finding keys, never by address: a pass that born or found
/// something speaks at `Info`, one that only marked stays at debug.
fn log_pass(done: &Result<Observed, String>) {
    match done {
        Ok(done) if done.born > 0 || !done.findings.is_empty() => aterm_log::info!(
            "lab account observation: {} born, {} marked, {} record(s), findings {:?}",
            done.born,
            done.marked,
            done.records,
            done.findings
        ),
        Ok(done) => aterm_log::debug!(
            "lab account observation: {}, {} marked, {} record(s)",
            done.quiet.unwrap_or("planned"),
            done.marked,
            done.records
        ),
        Err(why) => aterm_log::info!("lab account observation: {why}"),
    }
}

fn pass(sightings: &[Sighting]) -> Result<Observed, String> {
    let paths = aterm_secrets::client::Paths::live()
        .map_err(|_| "no vault state on this Mac".to_owned())?;
    let config =
        aterm_types::dirs::aterm_config_path().ok_or_else(|| "no aterm config path".to_owned())?;
    let now_ms = aterm_time::unix_millis()
        .filter(|ms| *ms > 0)
        .ok_or_else(|| "no valid clock on this Mac".to_owned())?;
    aterm_db::alab::Registry::at(paths)
        .observe_accounts(&config, sightings, now_ms)
        .map_err(|why| why.to_string())
}

/// How often a process looks at this Mac's login places. What it notes is bounded by the sweep's
/// own file (once a day for an unchanged place, whichever process looks) and what it passes to
/// the registry by the queue ([`Queue::took`]), so this is how soon a login file that came to
/// name another account is heard of, and how soon a note or a pass that failed is made again.
const SWEEP_EVERY: Duration = Duration::from_secs(15 * 60);

/// Start this process's machine sweep on a thread of its own, once. Called where a Commons
/// runtime started, a process that lives on: a short command would read every CLI's login file
/// at each start. Inert where [`install`] did not open the queue (a test, a debug build), so
/// nothing there reads a login place on a timer or notes one.
pub(crate) fn start_sweep() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    if QUEUE.get().is_none() {
        return;
    }
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("aterm-lab-sweep".into())
            .spawn(|| {
                loop {
                    sweep_now();
                    std::thread::sleep(SWEEP_EVERY);
                }
            });
        if spawned.is_err() {
            aterm_log::debug!("lab account sweep: no thread");
        }
    });
}

/// One sweep of this Mac's login places, by the rosters beside its `aterm.toml`.
fn sweep_now() {
    let Ok(state) = aterm_agent::harness::cli::harness_state_dir() else {
        return;
    };
    let Some(home) = crate::alab_journal::user_home() else {
        return;
    };
    let config = aterm_types::dirs::aterm_config_path();
    let now = aterm_agent::supervise::limit::unix_now();
    let noted = sweep_at(&state, &home, config.as_deref(), now);
    aterm_log::debug!("lab account sweep: {noted} login(s) noted");
}

/// [`sweep_now`] at `now` for the harness state `state`, the user home `home` and the rosters
/// beside `config`; nothing before the epoch. Returns how many logins it noted.
fn sweep_at(state: &Path, home: &Path, config: Option<&Path>, now: i64) -> usize {
    if now <= 0 {
        return 0;
    }
    use aterm_agent::ai::connector::Cli;
    let rosters: Vec<_> = Cli::ALL
        .into_iter()
        .filter_map(|cli| Some((cli, crate::alab_journal::roster_of(config, cli)?)))
        .collect();
    let claude = rosters
        .iter()
        .find_map(|(cli, roster)| (*cli == Cli::Claude).then_some(roster));
    let on_token = claude.map_or_else(OnToken::none, |roster| on_token(state, home, roster, now));
    aterm_agent::harness::observe::sweep::run(state, home, &rosters, &on_token, now)
}

/// Which rows of Claude Code's `roster` run on their vault token on this Mac, read value-free
/// from this Mac's vault (`accounts_tokens::rows_on_token_read`): such a row's directory may
/// hold a stale profile, so the sweep reads it as its token's account. Unread where this Mac
/// has no vault state or it could not be read just now; the sweep then reads no row's place.
fn on_token(
    state: &Path,
    home: &Path,
    roster: &aterm_agent::harness::accounts::Roster,
    now: i64,
) -> OnToken {
    use aterm_agent::harness::{accounts, accounts_tokens};
    let store = accounts::Store::at_for(state, accounts::Cli::Claude, home);
    aterm_secrets::client::Paths::live()
        .ok()
        .and_then(|paths| accounts_tokens::rows_on_token_read(&paths, &store, roster, now))
        .map_or(OnToken::Unread, |rows| {
            OnToken::Rows(rows.into_keys().collect())
        })
}

/// Open the queue where this process holds a lab ([`crate::alab_view::holds`]).
pub(crate) fn install() {
    if crate::alab_view::holds() {
        let _ = QUEUE.set(Mutex::new(Queue::default()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_agent::{
        ai::connector::Cli,
        harness::journal::{LoginState, ObservedSrc, Tab},
    };

    fn login(email: &str, tab: Option<&str>, state: LoginState, at: u64) -> ObservedLogin {
        ObservedLogin {
            cli: Cli::Claude.into(),
            realm: "claude".into(),
            email: email.into(),
            state,
            tab: tab.map(|sid| Tab::of("m65-test", sid)),
            at,
            src: ObservedSrc::Profile,
        }
    }

    /// Sightings of one login coalesce whatever the spelling, counting each tab once and keeping
    /// the newest state; the planner reads milliseconds; a taken queue is empty, and a second
    /// pass waits out the minute from the last.
    /// NEGATIVE CONTROL: with `gather` keyed by the email as spelled, two rows go to the
    /// planner: `` left: 2 right: 1 ``.
    #[test]
    fn sightings_of_one_login_coalesce_and_passes_stay_a_minute_apart() {
        let mut queue = Queue::default();
        assert!(!queue.start(), "nothing gathered, no pass");
        queue.gather(&login(
            "unicorn@proton.example",
            Some("s1"),
            LoginState::Running,
            10,
        ));
        queue.gather(&login(
            "Unicorn@Proton.example ",
            Some("s2"),
            LoginState::Running,
            12,
        ));
        queue.gather(&login(
            "unicorn@proton.example",
            Some("s1"),
            LoginState::Running,
            11,
        ));
        assert!(queue.start());
        assert!(!queue.start(), "one thread at a time");
        let taken = queue.take(12);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].tabs, 2);
        assert_eq!(taken[0].at_ms, 12_000);
        assert_eq!(taken[0].state, "running");
        assert_eq!(taken[0].src, "profile");
        assert!(queue.by.is_empty());
        let now = Instant::now();
        assert_eq!(queue.wait(now), Duration::ZERO);
        queue.last = Some(now);
        assert_eq!(queue.wait(now), EVERY);
    }

    /// The count is of the tabs running a login, not of the sightings one pass gathered: three
    /// tabs whose sightings fall due in three passes count three on the third; a tab that ends
    /// or moves to another login leaves the count, and one that stays silent past
    /// [`TAB_LIVE_S`] falls out with the next pass, its login passed again as idle.
    /// NEGATIVE CONTROL: with the tab map emptied by `take`, the third pass counts one: `` left:
    /// 1 right: 3 ``.
    #[test]
    fn tabs_count_every_tab_running_the_login_across_passes() {
        const A: &str = "a@proton.example";
        const B: &str = "b@proton.example";
        let mut queue = Queue::default();
        for (n, tab) in ["s1", "s2", "s3"].into_iter().enumerate() {
            let at = 100 + 60 * n as u64;
            queue.gather(&login(A, Some(tab), LoginState::Running, at));
            let taken = queue.take(at);
            assert_eq!(taken.len(), 1);
            assert_eq!(taken[0].tabs, n as u64 + 1, "pass {n}");
        }
        queue.gather(&login(A, Some("s1"), LoginState::Idle, 400));
        let taken = queue.take(400);
        assert_eq!((taken[0].tabs, taken[0].state.as_str()), (2, "running"));
        queue.gather(&login(B, Some("s2"), LoginState::Running, 460));
        let mut taken = queue.take(460);
        taken.sort_by(|x, y| x.email.cmp(&y.email));
        assert_eq!(
            taken
                .iter()
                .map(|s| (s.email.as_str(), s.tabs))
                .collect::<Vec<_>>(),
            [(A, 1), (B, 1)],
            "s2 moved from a to b"
        );
        let late = 220 + TAB_LIVE_S + 1;
        queue.gather(&login(B, Some("s2"), LoginState::Running, late));
        let mut taken = queue.take(late);
        taken.sort_by(|x, y| x.email.cmp(&y.email));
        assert_eq!(
            taken
                .iter()
                .map(|s| (s.email.as_str(), s.tabs, s.state.as_str()))
                .collect::<Vec<_>>(),
            [(A, 0, "idle"), (B, 1, "running")],
            "s3 went silent past the live span"
        );
        assert!(queue.take(late).is_empty(), "nothing moved since");
    }

    /// A pass's thread takes what gathered, and ends once nothing more waits so the next
    /// sighting starts another. A test never installs the queue, so `take_up` is inert here.
    /// NEGATIVE CONTROL: with `run` leaving `running` set, `` assertion failed: queue.start() ``.
    #[test]
    fn a_pass_ends_with_its_queue_and_take_up_is_inert_uninstalled() {
        fn quiet(sightings: &[Sighting]) -> Result<Observed, String> {
            assert_eq!(sightings.len(), 1);
            Ok(Observed {
                quiet: Some("not-held"),
                born: 0,
                marked: 0,
                findings: Vec::new(),
                records: 0,
            })
        }
        let queue = Mutex::new(Queue::default());
        queue.lock().unwrap().gather(&login(
            "a@proton.example",
            Some("s1"),
            LoginState::Running,
            5,
        ));
        assert!(queue.lock().unwrap().start());
        run(&queue, quiet);
        let mut held = queue.lock().unwrap();
        assert!(!held.running);
        assert!(held.last.is_some());
        held.gather(&login("a@proton.example", None, LoginState::Idle, 6));
        assert!(held.start());
        drop(held);
        take_up(&login("b@proton.example", None, LoginState::Running, 7));
        assert!(QUEUE.get().is_none());
    }

    /// A pass that panics still ends its thread's claim: the next sighting starts a pass.
    /// NEGATIVE CONTROL: without `run`'s drop guard, `` assertion failed: held.start() ``.
    #[test]
    fn a_pass_that_panics_frees_the_queue() {
        fn broken(_: &[Sighting]) -> Result<Observed, String> {
            panic!("a pass that panics");
        }
        let queue = Mutex::new(Queue::default());
        let seen = login("a@proton.example", Some("s1"), LoginState::Running, 5);
        queue.lock().unwrap().gather(&seen);
        assert!(queue.lock().unwrap().start());
        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&queue, broken)));
        assert!(ended.is_err());
        let mut held = queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(!held.running);
        held.gather(&seen);
        assert!(held.start());
    }

    /// A swept login reaches the planner as idle with no tab, and as running where a tab of
    /// this process runs it: the sweep says a login is here, never that no tab runs it.
    #[test]
    fn a_swept_login_is_idle_unless_a_tab_runs_it() {
        const A: &str = "a@proton.example";
        let mut queue = Queue::default();
        queue.gather(&login(A, None, LoginState::Idle, 100));
        let taken = queue.take(100);
        assert_eq!((taken[0].tabs, taken[0].state.as_str()), (0, "idle"));
        queue.gather(&login(A, Some("s1"), LoginState::Running, 160));
        queue.gather(&login(A, None, LoginState::Idle, 170));
        let taken = queue.take(170);
        assert_eq!(taken.len(), 1);
        assert_eq!((taken[0].tabs, taken[0].state.as_str()), (1, "running"));
    }

    /// The sweep hands a login at every look, and the queue passes it once a day: a swept
    /// login is gathered again until a pass whose plan ran took it, then not within the day, in
    /// whatever spelling; a pass that failed or read no registry took nothing, so the next look
    /// is gathered and passed again. A tab's sighting is gathered whatever was passed.
    /// NEGATIVE CONTROL: with `run` keeping every pass as taken, the look after the quiet pass
    /// is not gathered: `` not taken yet: gathered again ``.
    #[test]
    fn a_swept_login_is_passed_again_until_a_pass_takes_it() {
        const A: &str = "a@proton.example";
        fn incomplete(_: &[Sighting]) -> Result<Observed, String> {
            Ok(Observed {
                quiet: Some("incomplete"),
                born: 0,
                marked: 0,
                findings: Vec::new(),
                records: 0,
            })
        }
        fn failed(_: &[Sighting]) -> Result<Observed, String> {
            Err("no vault state on this Mac".to_owned())
        }
        fn planned(sightings: &[Sighting]) -> Result<Observed, String> {
            Ok(Observed {
                quiet: None,
                born: 0,
                marked: sightings.len(),
                findings: Vec::new(),
                records: 1,
            })
        }
        let day = IDLE_REFRESH_S.unsigned_abs();
        let now = aterm_time::unix_millis().unwrap() / 1_000;
        let queue = Mutex::new(Queue::default());
        for pass in [incomplete, failed, planned] {
            let mut held = queue.lock().unwrap();
            held.last = None;
            held.gather(&login(A, None, LoginState::Idle, now));
            assert!(held.start(), "not taken yet: gathered again");
            drop(held);
            run(&queue, pass);
        }
        let mut held = queue.lock().unwrap();
        held.gather(&login(
            " A@Proton.example",
            None,
            LoginState::Idle,
            now + 900,
        ));
        assert!(!held.start(), "taken: not within the day");
        held.gather(&login(A, Some("s1"), LoginState::Running, now + 900));
        assert_eq!(held.take(now + 900).len(), 1, "a tab's sighting always");
        held.gather(&login(A, None, LoginState::Idle, now + day + 60));
        assert_eq!(held.take(now + day + 60).len(), 1, "the day is over");
    }

    /// The sweep reads the login places of the home and the rosters it is given and keeps its
    /// day under the state it is given; with no journal sink in a test nothing is keyed, so it
    /// notes nothing and writes no file, and a clock before the epoch sweeps nothing.
    /// `start_sweep` is inert where the queue is not installed.
    #[test]
    fn a_sweep_reads_its_own_home_and_start_is_inert_uninstalled() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-lab-sweep-")
            .tempdir()
            .unwrap();
        let state = scratch.path().join("state");
        std::fs::write(
            scratch.path().join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"a@proton.example"}}"#,
        )
        .unwrap();
        assert_eq!(sweep_at(&state, scratch.path(), None, 0), 0);
        assert_eq!(sweep_at(&state, scratch.path(), None, 1_790_000_000), 0);
        assert!(!state.exists());
        start_sweep();
        assert!(QUEUE.get().is_none());
    }
}
