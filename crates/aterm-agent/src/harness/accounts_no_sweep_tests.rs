// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! NO SWEEP (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §10, §4.15 rule 4): the window's upkeep
//! renews only the logins this Mac uses, one account's sign-in at least ten minutes after
//! another's. A fixture Mac with 20 rows runs a week on an injected clock with no session: the
//! upkeep reads no provider and starts no sign-in for a row nobody here ran on for a week, and
//! the two due logins it does use start ten minutes apart. NEGATIVE CONTROL: with the use
//! condition removed ([`accounts::without_use_condition`]), the same upkeep renews idle rows.

use super::*;
use std::sync::{Arc, Mutex};

const START: i64 = 1_800_000_000;
const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const WEEK: i64 = 7 * 24 * HOUR;

/// The rows nobody on this Mac has run on: 18 of the 20.
const IDLE: usize = 18;
/// The two rows a session here ran on yesterday, listed after every idle row.
const USED: [&str; 2] = ["used-1", "used-2"];

type Work = Box<dyn FnOnce() + Send>;

/// A Mac whose 20 Claude Code logins, each made here, all ended at [`START`].
struct Mac {
    _dir: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
    store: accounts::Store,
}

impl Mac {
    fn new() -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-no-sweep-")
            .tempdir()
            .expect("tempdir");
        let root = dir.path().to_path_buf();
        let home = root.join("home");
        let config = root.join("config");
        let state = root.join("state");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("aterm.toml"), "[fleet]\nmachine = \"m27\"\n").unwrap();
        let store = accounts::Store::at(&state);
        store.machine_id("0123456789abcdef").unwrap();
        let labels: Vec<String> = (0..IDLE)
            .map(|i| format!("idle-{i}"))
            .chain(USED.map(str::to_owned))
            .collect();
        let rows = labels
            .iter()
            .map(|label| {
                let made = store.logins_dir().join(label);
                fs::create_dir_all(&made).unwrap();
                fs::write(made.join(".claude.json"), "{}").unwrap();
                Row {
                    label: label.clone(),
                    dir: Some(made.to_str().unwrap().to_owned()),
                    email: Some(format!("{label}@example.test")),
                    ..Row::default()
                }
            })
            .collect();
        let roster = Roster { rows };
        fs::write(
            config.join("accounts.toml"),
            format_roster_toml(&roster).unwrap(),
        )
        .unwrap();
        store
            .transact_logins(START, &mut None, |state, logins| {
                let mut events = Vec::new();
                for label in &labels {
                    events.extend(accounts::note_login_end(state, logins, label, START));
                }
                // Yesterday's sessions here, on the two used rows, ended since.
                for (sid, label) in USED.iter().enumerate() {
                    events.extend(accounts::note_session(
                        state,
                        &format!("s-{sid}"),
                        label,
                        None,
                        START - 24 * HOUR,
                    ));
                }
                state.sessions.clear();
                Ok(events)
            })
            .unwrap();
        Self {
            _dir: dir,
            home,
            state,
            config,
            store,
        }
    }

    fn roster(&self) -> PathBuf {
        self.config.join("accounts.toml")
    }

    /// The window's upkeep every `step` seconds from `from` until `until`, its renewals queued and
    /// then dropped unrun (no provider is contacted), and each sign-in it started: when, and which
    /// account the Mac's spacing record says it was.
    fn upkeep_until(&self, from: i64, until: i64, step: impl Fn(i64) -> i64) -> Vec<(i64, String)> {
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let queued: Arc<Mutex<Vec<Work>>> = Arc::new(Mutex::new(Vec::new()));
        let into_queue = Arc::clone(&queued);
        let spawn = move |work| {
            into_queue.lock().unwrap().push(work);
            Ok(())
        };
        let fetch = |_: &Env, source: &str| -> Result<String, String> {
            Err(format!("no fetch in a test: {source}"))
        };
        let cooldown = Mutex::new(None);
        let rediscovery = Mutex::new(Rediscovery::default());
        let fx = UpkeepEffects {
            fetch: &fetch,
            spawn_renewal: &spawn,
            sync_cooldown: &cooldown,
            rediscovery: &rediscovery,
            renew_guard: &guard,
            find_browser: &|_: &Path| true,
            vault_sync: |_: &Path| super::super::sign_in_ladder::Vault::NotMember,
            spawn_vault: &|_work| Ok(()),
            default_check: |_: &Path| crate::harness::cli::LoginCheck::CouldNotRun(String::new()),
            // A route the ladder can take, so only the doors decide what starts.
            mailbox: &|_, _, _| (super::super::sign_in_ladder::Mailbox::Ready, None),
            log: Arc::new(|_: &str| {}),
            run_attempt: None,
            shared: shared_renewal::Effects::default(),
        };
        let labels: Vec<String> = (0..IDLE)
            .map(|i| format!("idle-{i}"))
            .chain(USED.map(str::to_owned))
            .collect();
        let mut started = Vec::new();
        let mut now = from;
        while now <= until {
            let _ = upkeep(
                &self.home,
                &self.state,
                (&self.roster(), now, UpkeepStep::Renew),
                &fx,
            );
            let ran: Vec<Work> = queued.lock().unwrap().drain(..).collect();
            assert!(
                ran.len() <= 1,
                "one sign-in at a time on a Mac: {}",
                ran.len()
            );
            if !ran.is_empty() {
                let last = super::super::sign_in_ladder::last_started(&self.store)
                    .unwrap()
                    .expect("a started sign-in records its start");
                let label = labels
                    .iter()
                    .find(|label| {
                        last == super::super::sign_in_ladder::Started::new(Cli::Claude, label, now)
                    })
                    .expect("the record names a row started now");
                started.push((now, label.clone()));
            }
            // Dropping the queued work releases its admission without running a provider.
            drop(ran);
            now += step(now - from);
        }
        started
    }
}

/// Every minute for the first hour, then every quarter hour to the week's end.
fn week_step(since: i64) -> i64 {
    if since < HOUR { MINUTE } else { 15 * MINUTE }
}

#[test]
fn a_week_with_no_session_signs_in_to_no_idle_row_and_spaces_the_two_used_ones() {
    let mac = Mac::new();
    let (started, probes) =
        super::super::provider_read::captured(|| mac.upkeep_until(START, START + WEEK, week_step));
    assert!(probes.is_empty(), "the upkeep read a provider: {probes:?}");
    assert!(
        started
            .iter()
            .all(|(_, label)| USED.contains(&label.as_str())),
        "only the logins this Mac uses are renewed: {started:?}"
    );
    let [(first, a), (second, b)] = started.as_slice() else {
        panic!("each used login starts once: {started:?}")
    };
    assert_ne!(a, b);
    assert_eq!(*first, START, "the first due login starts at once");
    assert_eq!(
        second - first,
        super::super::sign_in_ladder::SPACING_S,
        "the second starts at the first pass ten minutes after the first, and not before"
    );
}

#[test]
fn negative_control_without_the_use_condition_the_upkeep_renews_idle_rows() {
    let mac = Mac::new();
    let started =
        accounts::without_use_condition(|| mac.upkeep_until(START, START + HOUR, |_| MINUTE));
    assert!(
        started.iter().any(|(_, label)| label.starts_with("idle-")),
        "CONTROL: the timer renewal the use condition stops: {started:?}"
    );
    assert!(
        started
            .windows(2)
            .all(|pair| pair[1].0 - pair[0].0 >= super::super::sign_in_ladder::SPACING_S),
        "the spacing still holds: {started:?}"
    );
}
