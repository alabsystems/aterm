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

/// A lapsed Claude Code login file: no token (`aterm-lab`'s `claude_login_proof::DEAD_LOGIN`).
const CLAUDE_DEAD: &str = "{\"claudeAiOauth\":{\"accessToken\":\"\",\"refreshToken\":\"\",\
                           \"expiresAt\":0,\"scopes\":[\"user:inference\",\"user:profile\"],\
                           \"subscriptionType\":null}}\n";

/// A Mac whose Claude Code logins were each made here, in a directory of its own.
struct Mac {
    _dir: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
    store: accounts::Store,
    /// Its rows' labels, in roster order.
    labels: Vec<String>,
}

impl Mac {
    /// 20 logins, all ended at [`START`]; the two [`USED`] ones a session here ran on yesterday.
    fn new() -> Self {
        let labels: Vec<String> = (0..IDLE)
            .map(|i| format!("idle-{i}"))
            .chain(USED.map(str::to_owned))
            .collect();
        let mac = Self::laid(labels.clone(), |_| None);
        mac.record(&labels, &USED);
        mac
    }

    /// Three logins a session here ran on yesterday (ONE RULE FOR A LAPSED LOGIN, one-mesh AC1):
    /// `lapsed`, whose credential file shows it lapsed and whose records say nothing ended;
    /// `ended`, whose records say it ended at [`START`]; and `lapsed-ended`, both.
    fn lapsed() -> Self {
        let mac = Self::laid(
            ["lapsed", "ended", "lapsed-ended"]
                .map(str::to_owned)
                .to_vec(),
            |label| label.starts_with("lapsed").then_some(CLAUDE_DEAD),
        );
        mac.record(
            &["ended", "lapsed-ended"],
            &["lapsed", "ended", "lapsed-ended"],
        );
        mac
    }

    /// A Mac with a login made here for each of `labels`, each naming an email, its directory
    /// holding a `.credentials.json` of `credential`'s bytes where that names some.
    fn laid(labels: Vec<String>, credential: impl Fn(&str) -> Option<&'static str>) -> Self {
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
        aterm_uds::atomic_file::write(
            &config.join("aterm.toml"),
            b"[fleet]\nmachine = \"m27\"\n",
            0o600,
        )
        .unwrap();
        let store = accounts::Store::at(&state);
        store.machine_id("0123456789abcdef").unwrap();
        let rows = labels
            .iter()
            .map(|label| {
                let made = store.logins_dir().join(label);
                fs::create_dir_all(&made).unwrap();
                fs::write(made.join(".claude.json"), "{}").unwrap();
                if let Some(body) = credential(label) {
                    fs::write(made.join(".credentials.json"), body).unwrap();
                }
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
        Self {
            _dir: dir,
            home,
            state,
            config,
            store,
            labels,
        }
    }

    /// Records each of `ended`'s logins ended at [`START`], and a session here on each of `used`
    /// yesterday, ended since.
    fn record(&self, ended: &[impl AsRef<str>], used: &[&str]) {
        self.store
            .transact_logins(START, &mut None, |state, logins| {
                let mut events = Vec::new();
                for label in ended {
                    events.extend(accounts::note_login_end(
                        state,
                        logins,
                        label.as_ref(),
                        START,
                    ));
                }
                for (sid, label) in used.iter().enumerate() {
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
    }

    /// A session here that runs on `label` now, noted a minute before [`START`] and not ended.
    fn run_on(&self, label: &str) {
        self.store
            .transact_logins(START, &mut None, |state, _| {
                Ok(accounts::note_session(
                    state,
                    "s-live",
                    label,
                    None,
                    START - MINUTE,
                ))
            })
            .unwrap();
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
            scope: None,
            fetch: &fetch,
            spawn_renewal: &spawn,
            sync_cooldown: &cooldown,
            rediscovery: &rediscovery,
            renew_guard: &guard,
            find_browser: &|_: &Path| true,
            vault_sync: |_: &Path, _scope| super::super::sign_in_ladder::Vault::NotMember,
            spawn_vault: &|_work| Ok(()),
            default_check: |_: &Path, _scope| {
                crate::harness::cli::LoginCheck::CouldNotRun(String::new())
            },
            // A route the ladder can take, so only the doors decide what starts.
            mailbox: &|_, _, _| (super::super::sign_in_ladder::Mailbox::Ready, None),
            log: Arc::new(|_: &str| {}),
            run_attempt: None,
            save_oauth: |_, _| Ok("inert".into()),
            shared: shared_renewal::Effects::default(),
        };
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
                let label = self
                    .labels
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

/// THE UPKEEP STARTS NOTHING FOR A LAPSED LOGIN (one-mesh AC1): an hour of the window's upkeep
/// on a Mac whose used `lapsed` login's credential file shows it lapsed, though nothing recorded
/// it ended, reads no provider and starts no sign-in for it, with or without a session here
/// running on it: it reads expired ([`accounts::login_state`]), but the upkeep's doors open only
/// on its records. The recorded ends beside it start as before (§4.15 rule 4), `lapsed-ended` included: a lapse takes
/// no renewal away, and they show the upkeep's starts are seen. NEGATIVE CONTROL: with the door
/// reading `login_state` ([`accounts::with_renewal_on_login_state`]), the same upkeep signs the
/// lapsed login in by itself.
#[test]
fn the_upkeep_starts_nothing_for_a_lapsed_login() {
    let by_label = |started: &[(i64, String)]| {
        let mut labels: Vec<String> = started.iter().map(|(_, label)| label.clone()).collect();
        labels.sort();
        labels
    };
    let mac = Mac::lapsed();
    let (started, probes) =
        super::super::provider_read::captured(|| mac.upkeep_until(START, START + HOUR, |_| MINUTE));
    assert!(probes.is_empty(), "the upkeep read a provider: {probes:?}");
    assert_eq!(
        by_label(&started),
        ["ended", "lapsed-ended"],
        "only the recorded ends start, each once: {started:?}"
    );
    // A session running on the lapsed login opens no door either: the session door reads the
    // record, and nothing recorded a wall.
    let mac = Mac::lapsed();
    mac.run_on("lapsed");
    let (started, probes) =
        super::super::provider_read::captured(|| mac.upkeep_until(START, START + HOUR, |_| MINUTE));
    assert!(probes.is_empty(), "the upkeep read a provider: {probes:?}");
    assert_eq!(
        by_label(&started),
        ["ended", "lapsed-ended"],
        "a session on the lapsed login starts it no sign-in: {started:?}"
    );
    let mac = Mac::lapsed();
    let started =
        accounts::with_renewal_on_login_state(|| mac.upkeep_until(START, START + HOUR, |_| MINUTE));
    assert_eq!(
        by_label(&started),
        ["ended", "lapsed", "lapsed-ended"],
        "NEGATIVE CONTROL: a door on `login_state` starts the lapsed login: {started:?}"
    );
}
