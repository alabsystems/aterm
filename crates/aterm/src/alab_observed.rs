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

use aterm_agent::harness::journal::LoginState;
use aterm_agent::harness::journal::ObservedLogin;
use aterm_db::alab::{Observed, observe::Sighting};
use std::{
    collections::BTreeMap,
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
/// login's last pass took, and whether a pass's thread runs and when the last one ended.
#[derive(Default)]
struct Queue {
    by: BTreeMap<Login, Gathered>,
    tabs: BTreeMap<String, (Login, u64)>,
    counted: BTreeMap<Login, (u64, &'static str)>,
    running: bool,
    last: Option<Instant>,
}

impl Queue {
    /// Gather `login`: the newest sighting's state and source; a running tab now on it (and on
    /// no other), or a tab that ended off it.
    fn gather(&mut self, login: &ObservedLogin) {
        let key: Login = (
            login.cli.id(),
            login.realm.clone(),
            login.email.trim().to_ascii_lowercase(),
        );
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
            cli: Cli::Claude,
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
}
