// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FOOTER READS ITS OWN TAB'S USAGE TAP: the resolver's weekly figure from this process's own
//! `rl/<tab>-<pid>.json` beside Claude Code's `/usage` cache, the newer of the two, under the one
//! age rule (`profile::WeeklyFigure::words`); a reason where there is none (`profile::WeeklyGap`),
//! a restart promised only where a relaunch would carry the tap; and the accounts store's figures
//! and walls under the same rule ([`super::store_week`]).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aterm_agent::harness::accounts;
use aterm_agent::harness::footer;
use aterm_agent::harness::profile::{SubscriptionDisplay, WeeklyFigure, WeeklyGap};
use aterm_agent::harness::tap::{self, Refusal};

use super::{
    Asks, Identity, LaunchFacts, RosterMemo, TapAsks, TapSeat, resolve_facts_at, store_week,
};
use crate::session_timeline::SessionTimeline;

const PID: i32 = 4251;
/// The process's birth, and its registry row's `startedAt`.
const START: u64 = 1_790_607_848;
/// The tab its environment names.
const TAB: &str = "s-1a";
/// The clock every read is judged at.
const NOW: i64 = 1_792_050_000;
/// The week's reset.
const RESET: i64 = NOW + 86_400;
thread_local! {
    /// The scratch root's name of the test this thread runs ([`fresh`],
    /// [`super::rig::home_root`]): the resolver asks on the test's own thread.
    static NAME: std::cell::Cell<&'static str> = const { std::cell::Cell::new("footer-weekly") };
}

fn root() -> PathBuf {
    super::rig::home_root(NAME.get())
}

/// The default home's records, where the person's own `HOME` keeps them.
fn records() -> PathBuf {
    root().join(".claude")
}

fn rl() -> PathBuf {
    root().join("rl")
}

/// The harness state of the scratch accounts store.
fn store() -> PathBuf {
    root().join("state")
}

/// A fresh scratch home `name` for this thread's test ([`root`]) whose registry names
/// [`PID`], its records in [`records`] (`.claude`, where a relaunch carries no `HOME` of its
/// own).
fn fresh(name: &'static str) {
    NAME.set(name);
    let _ = std::fs::remove_dir_all(root());
    let (_, claude) = super::rig::claude_home(name, PID, START);
    std::fs::rename(claude, records()).unwrap();
}

/// The exec environment of a Claude Code in [`TAB`] on the default login, plus `more`.
fn env(more: &[String]) -> Vec<String> {
    [
        format!("HOME={}", root().display()),
        format!("{}={TAB}", aterm_types::domain::ENV_PARENT_SESSION_ID),
    ]
    .into_iter()
    .chain(more.iter().cloned())
    .collect()
}

/// aterm's own tap on a relaunch line.
fn tap_value() -> String {
    tap::settings_value(Path::new("/Applications/aterm.app/Contents/MacOS/aterm"))
        .expect("an admissible path")
}

/// The exec read of a Claude Code aterm relaunched with its tap, on the default login.
fn tapped(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
    Some(atpkg::caller_shell::ProcArgs {
        argv: vec!["claude".into(), "--settings".into(), tap_value()],
        env: env(&[]),
        ..Default::default()
    })
}

/// The same process started by hand: no tap on its line.
fn untapped(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
    Some(atpkg::caller_shell::ProcArgs {
        argv: vec!["claude".into()],
        env: env(&[]),
        ..Default::default()
    })
}

/// A window whose relaunch lines carry the tap, its readings under [`rl`], no store.
fn armed() -> TapSeat {
    TapSeat {
        on: true,
        named: true,
        dir: Some(rl()),
        store: None,
    }
}

/// A window with the tap off.
fn unarmed() -> TapSeat {
    TapSeat {
        on: false,
        ..armed()
    }
}

/// [`armed`] with this machine's accounts store under [`store`].
fn stored() -> TapSeat {
    TapSeat {
        store: Some(store()),
        ..armed()
    }
}

/// The asks of a process on its stored login, with no account listed, its settings chain
/// quiet.
fn asks(args: fn(i32) -> Option<atpkg::caller_shell::ProcArgs>, seat: fn() -> TapSeat) -> Asks {
    Asks {
        files: |_| None,
        args,
        roster: || Some(accounts::Roster::default()),
        home: || Some(root()),
        stored_login: |_, _, _| true,
        tap: TapAsks {
            seat,
            settings: |_, _| true,
            restart: |_, _| true,
        },
    }
}

/// The exec read of the same process run under a Claude home of its own ([`HOME_ENV`]), its
/// records moved there by [`own_config_dir`].
///
/// [`HOME_ENV`]: accounts::HOME_ENV
fn configured(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
    Some(atpkg::caller_shell::ProcArgs {
        env: env(&[format!(
            "{}={}",
            accounts::HOME_ENV,
            root().join("claude").display()
        )]),
        ..untapped(PID)?
    })
}

/// The exec read of the same process under a `HOME` of its own, its records moved there by
/// [`own_home_dir`].
fn own_home(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
    Some(atpkg::caller_shell::ProcArgs {
        env: vec![
            format!("HOME={}", root().join("other").display()),
            format!("{}={TAB}", aterm_types::domain::ENV_PARENT_SESSION_ID),
        ],
        ..untapped(PID)?
    })
}

/// Move the default home's records to the Claude home of its own [`configured`] names, its
/// profile with them: the records' new directory.
fn own_config_dir() -> PathBuf {
    let dir = root().join("claude");
    std::fs::rename(records(), &dir).unwrap();
    profile_in(Some(&dir.display().to_string()), &dir, None);
    dir
}

/// Move the default home's records under the `HOME` of its own [`own_home`] names: the
/// records' new directory.
fn own_home_dir() -> PathBuf {
    let other = root().join("other").join(".claude");
    std::fs::create_dir_all(root().join("other")).unwrap();
    std::fs::rename(records(), &other).unwrap();
    profile_in(None, &other, None);
    other
}

/// Save this machine's accounts store with the machine's own account (`main`) holding a tap
/// figure of the week 40% used read at `fig`, where one is given, and a standing weekly wall
/// when `walled`.
fn save_main(fig: Option<i64>, walled: bool) {
    save_as(aterm_agent::harness::cli::ACCOUNTS_OWN_LABEL, fig, walled);
}

/// [`save_main`] with the account labelled `label`.
fn save_as(label: &str, fig: Option<i64>, walled: bool) {
    let mut acct = accounts::Account::default();
    if let Some(at) = fig {
        acct.windows.insert(
            "seven_day".into(),
            accounts::WindowFig {
                pct_x10: 400,
                reset: RESET,
                at,
                by: "m22".into(),
                source: accounts::SOURCE_TAP.into(),
                epoch_at: None,
            },
        );
    }
    if walled {
        acct.walls.insert(
            accounts::WEEK_LIMIT.into(),
            accounts::Wall {
                until: Some(RESET),
                at: NOW - 60,
                by: "m22".into(),
                attempts: 0,
                cleared: false,
            },
        );
    }
    let mut state = accounts::State::default();
    state.accounts.insert(label.into(), acct);
    accounts::Store::at(&store()).save(&state).unwrap();
}

/// Write the session's transcript holding the limit notice Claude Code writes at the weekly
/// wall, written two minutes before [`NOW`].
fn weekly_wall() {
    let line = r#"{"type":"assistant","timestamp":"2026-10-15T07:38:00Z","isApiErrorMessage":true,"error":"rate_limit","message":{"id":"wall","model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"You've hit your weekly limit \u00b7 resets Oct 18 at 9am (UTC)"}],"usage":{"input_tokens":0,"output_tokens":0}}}"#;
    let path = super::rig::transcript_of(&root(), &records());
    std::fs::write(path, format!("{line}\n")).unwrap();
}

/// Write the profile Claude Code keeps for the home whose records are `dir` (named by
/// `CLAUDE_CONFIG_DIR` `config`, where one is), with a `/usage` cache of `used` percent
/// fetched at `fetched` where one is given.
fn profile_in(config: Option<&str>, dir: &Path, cache: Option<(u32, i64)>) {
    let mut doc = aterm_json::json!({
        "oauthAccount": {
            "accountUuid": "tap-account",
            "emailAddress": "tap@example.test",
            "planDisplayName": "Max 20x"
        }
    });
    if let Some((used, fetched)) = cache {
        doc.as_object_mut().unwrap().insert(
            "cachedUsageUtilization".into(),
            aterm_json::json!({
                "fetchedAtMs": fetched * 1000,
                "accountUuid": "tap-account",
                "utilization": {
                    "seven_day": { "utilization": used, "resets_at": "2099-01-01T00:00:00Z" }
                }
            }),
        );
    }
    let path = aterm_agent::harness::profile::path_in(&root(), config, dir);
    std::fs::write(path, aterm_json::to_string(&doc).unwrap()).unwrap();
}

/// [`profile_in`] of the default home.
fn profile(cache: Option<(u32, i64)>) {
    profile_in(None, &records(), cache);
}

/// What the tap writes for process `pid` of [`TAB`]: the week `used_x10` used, resetting at
/// `resets_at`, first reported at `at`, from a tap whose environment names `login` and `home`.
fn reading(
    pid: u32,
    at: i64,
    (used_x10, resets_at): (u32, i64),
    login: Option<&str>,
    home: Option<&str>,
) {
    let reading = tap::Reading {
        at,
        sid: TAB.into(),
        pid,
        session: None,
        login: login.map(str::to_owned),
        home: home.map(str::to_owned),
        api_ms: 1_200,
        five_hour: None,
        seven_day: Some(tap::Window {
            pct: (used_x10 + 5) / 10,
            resets_at,
            pct_x10: Some(used_x10),
        }),
        warm: None,
        cache_expires_at: None,
        version: None,
        model: None,
        dur_us: 0,
    };
    std::fs::create_dir_all(rl()).unwrap();
    std::fs::write(file(pid), reading.to_json()).unwrap();
}

fn file(pid: u32) -> PathBuf {
    rl().join(tap::file_name(TAB, pid).expect("a tab and a pid"))
}

/// A tab whose foreground program is Claude Code [`PID`].
fn tab() -> Arc<Mutex<SessionTimeline>> {
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    {
        let mut held = timeline.lock().unwrap();
        held.note_foreground_group(PID);
        held.set_program(PID, Some("claude".into()));
    }
    timeline
}

/// [`PID`] as the resolver first meets it, its records in `dir`.
fn unread(dir: &Path) -> Identity {
    Identity {
        pid: PID,
        started: Some(START),
        dir: Some(dir.to_path_buf()),
        launch: LaunchFacts::default(),
        launch_for: None,
        login: None,
        argv: Vec::new(),
        task: None,
        login_checked_for: None,
        stored_login: false,
        unoffered: None,
        env_stamp: None,
    }
}

/// One resolver read of [`PID`], its records in `dir`, under `asks` at [`NOW`]: the
/// subscription it publishes.
fn read_in(dir: &Path, asks: Asks) -> SubscriptionDisplay {
    reread(&tab(), &mut unread(dir), asks)
}

/// One resolver read of `identity` in `timeline` under `asks` at [`NOW`]: the subscription
/// the tab's footer holds after it.
fn reread(
    timeline: &Arc<Mutex<SessionTimeline>>,
    identity: &mut Identity,
    asks: Asks,
) -> SubscriptionDisplay {
    resolve_facts_at(
        timeline,
        identity,
        &mut footer::TailCache::default(),
        &mut footer::FooterCache::default(),
        &mut RosterMemo::default(),
        asks,
        &footer::ReadClock {
            read_at: std::time::SystemTime::now(),
            now: NOW,
            offset_at: &|_, _| Some(0),
        },
    );
    let held = timeline.lock().unwrap();
    held.claude_footer()
        .and_then(|facts| facts.subscription.clone())
        .expect("a Claude Code footer names its subscription")
}

/// [`read_in`] of the default home.
fn read(asks: Asks) -> SubscriptionDisplay {
    read_in(&records(), asks)
}

/// The footer's weekly words of `asks`' read, or why it has none.
fn weekly(asks: Asks) -> (Option<String>, Option<WeeklyGap>) {
    weekly_in(&records(), asks)
}

/// [`weekly`] of a process whose records are in `dir`.
fn weekly_in(dir: &Path, asks: Asks) -> (Option<String>, Option<WeeklyGap>) {
    let shown = read_in(dir, asks);
    (shown.weekly, shown.weekly_gap)
}

fn words(text: &str) -> (Option<String>, Option<WeeklyGap>) {
    (Some(text.to_owned()), None)
}

fn none(gap: WeeklyGap) -> (Option<String>, Option<WeeklyGap>) {
    (None, Some(gap))
}

#[test]
fn the_footer_shows_its_own_taps_week_for_its_own_login_under_the_age_rule() {
    let pid = PID as u32;
    fresh("footer-weekly-tap");
    profile(None);
    let tap_on = asks(tapped, armed);

    reading(pid, NOW - 60, (380, RESET), None, None);
    assert_eq!(
        weekly(tap_on),
        words("weekly 62% left"),
        "this tab's own reading"
    );
    let figure = |at: i64| {
        Some(WeeklyFigure {
            used_x10: 380,
            resets_at: RESET,
            at,
        })
    };
    let shown = read(tap_on);
    assert_eq!(
        (shown.email.as_deref(), shown.weekly_figure),
        (Some("tap@example.test"), figure(NOW - 60)),
        "beside the account it is of, the figure behind the words published for the menu"
    );
    assert_eq!(
        weekly(asks(tapped, unarmed)),
        words("weekly 62% left"),
        "a process that carries the tap is read with the switch since turned off"
    );
    assert_eq!(
        weekly(asks(untapped, armed)),
        none(WeeklyGap::Untapped),
        "a process that does not carry the tap reads no tap file, even one its tab and pid name"
    );
    std::fs::remove_file(file(pid)).unwrap();
    assert_eq!(
        weekly(asks(tapped, unarmed)),
        none(WeeklyGap::NoReply),
        "and with no reading yet, its tap's reason, not the switch's"
    );

    reading(pid, NOW - 16 * 60, (380, RESET), None, None);
    assert_eq!(
        weekly(tap_on),
        words("weekly 62% left \u{b7} 16m ago"),
        "past fifteen minutes, with its age"
    );
    assert_eq!(
        read(tap_on).weekly_figure,
        figure(NOW - 16 * 60),
        "the aged figure keeps its time for the menu's row"
    );
    reading(pid, NOW - 61 * 60, (380, RESET), None, None);
    assert_eq!(
        weekly(tap_on),
        none(WeeklyGap::Stale),
        "past an hour, none: no reply in the hour brought one"
    );
    reading(pid, NOW - 60, (380, NOW - 1), None, None);
    assert_eq!(weekly(tap_on), none(WeeklyGap::NoReply), "its window reset");

    reading(pid, NOW - 60, (380, RESET), Some("/logins/work-2"), None);
    assert_eq!(
        weekly(tap_on),
        none(WeeklyGap::NoReply),
        "another login's tap"
    );
    reading(pid, NOW - 60, (380, RESET), None, Some("/homes/work-2"));
    assert_eq!(
        weekly(tap_on),
        none(WeeklyGap::NoReply),
        "a home of its own"
    );

    reading(pid, NOW - 60, (380, RESET), None, None);
    let born = std::time::UNIX_EPOCH + std::time::Duration::from_secs(START - 60);
    std::fs::File::options()
        .write(true)
        .open(file(pid))
        .and_then(|f| f.set_modified(born))
        .unwrap();
    assert_eq!(
        weekly(tap_on),
        none(WeeklyGap::NoReply),
        "a reading written before the process started"
    );
    std::fs::remove_file(file(pid)).unwrap();
    reading(pid + 1, NOW - 60, (380, RESET), None, None);
    assert_eq!(
        weekly(tap_on),
        none(WeeklyGap::NoReply),
        "another process's reading"
    );
    std::fs::remove_file(file(pid + 1)).unwrap();

    reading(pid, NOW - 60, (380, RESET), None, None);
    let refused = Asks {
        stored_login: |_, _, _| false,
        ..tap_on
    };
    assert_eq!(
        weekly(refused),
        (None, None),
        "a refused profile read gives no weekly and tells no reason"
    );

    profile(Some((71, NOW - 120)));
    assert_eq!(weekly(tap_on), words("weekly 62% left"), "the tap is newer");
    profile(Some((71, NOW - 30)));
    assert_eq!(
        weekly(tap_on),
        words("weekly 29% left"),
        "the cache is newer"
    );
    reading(pid, NOW - 20 * 60, (380, RESET), None, None);
    profile(Some((71, NOW - 30 * 60)));
    assert_eq!(
        weekly(tap_on),
        words("weekly 62% left \u{b7} 20m ago"),
        "the newer of two aged figures, with its age"
    );
    let _ = std::fs::remove_dir_all(root());
}

/// A PROCESS ON A NAMED LOGIN reads the tap figure its own login's tap wrote (the lone-login
/// relaunch names that login in the tap's environment as in the process's), never another
/// login's nor the default one's; and an untapped one is promised a restart only where a
/// relaunch line would tap its login (`tap::Armed::lone_for`): the machine's lone one, bound in
/// this machine's store, or, where the machine has no lone login, one the roster lists; judged
/// against the roster as it reads now, not as it read when the process started.
#[test]
fn a_named_logins_tap_figure_is_its_own_and_its_restart_taps_the_login_every_relaunch_does() {
    const LOGIN: &str = "/logins/main";
    fn on_login(
        args: Option<atpkg::caller_shell::ProcArgs>,
    ) -> Option<atpkg::caller_shell::ProcArgs> {
        Some(atpkg::caller_shell::ProcArgs {
            env: env(&[format!("{}={LOGIN}", accounts::LOGIN_ENV)]),
            ..args?
        })
    }
    fn one_row() -> Option<accounts::Roster> {
        Some(accounts::Roster {
            rows: vec![accounts::Row {
                label: "main".into(),
                class: accounts::Class::Dedicated,
                dir: Some(LOGIN.into()),
                email: Some("tap@example.test".into()),
                ..accounts::Row::default()
            }],
        })
    }
    let listed =
        |args: fn(i32) -> Option<atpkg::caller_shell::ProcArgs>, seat: fn() -> TapSeat| Asks {
            roster: one_row,
            ..asks(args, seat)
        };
    let pid = PID as u32;
    fresh("footer-weekly-login");
    profile(None);
    let tap_on = listed(|pid| on_login(tapped(pid)), armed);
    reading(pid, NOW - 60, (380, RESET), Some(LOGIN), None);
    assert_eq!(
        weekly(tap_on),
        words("weekly 62% left"),
        "its own login's tap"
    );
    reading(pid, NOW - 60, (380, RESET), Some("/logins/work-2"), None);
    assert_eq!(weekly(tap_on), none(WeeklyGap::NoReply), "another login's");
    reading(pid, NOW - 60, (380, RESET), None, None);
    assert_eq!(
        weekly(tap_on),
        none(WeeklyGap::NoReply),
        "the default login's"
    );
    assert_eq!(
        weekly(listed(|pid| on_login(untapped(pid)), stored)),
        none(WeeklyGap::Untapped),
        "untapped on the lone login this machine's store binds: a restart adds it"
    );
    assert_eq!(
        weekly(listed(|pid| on_login(untapped(pid)), armed)),
        none(WeeklyGap::Untapped),
        "with no store to bind a lone login, the restart taps the login the roster lists"
    );
    fn two_rows() -> Option<accounts::Roster> {
        let mut roster = one_row()?;
        roster.rows.push(accounts::Row {
            label: "work".into(),
            class: accounts::Class::Dedicated,
            dir: Some("/logins/work-2".into()),
            email: Some("work@example.test".into()),
            ..accounts::Row::default()
        });
        Some(roster)
    }
    let several = |args: fn(i32) -> Option<atpkg::caller_shell::ProcArgs>| Asks {
        roster: two_rows,
        ..asks(args, stored)
    };
    let timeline = tab();
    let mut identity = unread(&records());
    let shown = reread(
        &timeline,
        &mut identity,
        several(|pid| on_login(untapped(pid))),
    );
    assert_eq!(
        (shown.email.as_deref(), shown.weekly, shown.weekly_gap),
        (Some("tap@example.test"), None, Some(WeeklyGap::Untapped)),
        "two accounts listed, no lone login: the restart taps this listed login"
    );
    let mut logins = accounts::Logins::default();
    accounts::note_renewed(
        &mut accounts::State::default(),
        &mut logins,
        "main",
        Some(LOGIN),
        "/logins/main.2",
        None,
        NOW,
    );
    accounts::Store::at(&store()).save_logins(&logins).unwrap();
    let shown = reread(
        &timeline,
        &mut identity,
        listed(|pid| on_login(untapped(pid)), stored),
    );
    assert_eq!(
        (shown.email.as_deref(), shown.weekly, shown.weekly_gap),
        (
            Some("tap@example.test"),
            None,
            Some(WeeklyGap::Unoffered(Refusal::Login))
        ),
        "the same process, read again once the roster is down to one row whose renewal \
         moved it to another login: the lone login is that one, so no restart taps this"
    );
    fn two_rows_bound() -> Option<accounts::Roster> {
        accounts::Store::at(&store()).bound(&two_rows()?, NOW).ok()
    }
    let shown = reread(
        &timeline,
        &mut identity,
        Asks {
            roster: two_rows_bound,
            ..asks(|pid| on_login(untapped(pid)), stored)
        },
    );
    assert_eq!(
        (shown.email.as_deref(), shown.weekly, shown.weekly_gap),
        (Some("tap@example.test"), None, Some(WeeklyGap::Untapped)),
        "two rows again, this one renewed away from this login (`row_left`): the restart \
         taps it"
    );
    let file = root().join(accounts::ACCOUNTS_FILE);
    std::fs::write(
        &file,
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\ndir = \"{LOGIN}\"\n\
             email = \"tap@example.test\"\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"/logins/work-2\"\n\
             email = \"work@example.test\"\n"
        ),
    )
    .unwrap();
    let relaunch = tap::Armed::arm(
        true,
        || {
            Some(PathBuf::from(
                "/Applications/aterm.app/Contents/MacOS/aterm",
            ))
        },
        Some(&file),
        &accounts::Store::at(&store()),
        Some(NOW),
    )
    .expect("armed");
    assert_eq!(
        relaunch.lone_for(Some(LOGIN)),
        Some(LOGIN),
        "and every relaunch line does tap it, by the logins the same roster and store arm"
    );
    fn elsewhere(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        Some(atpkg::caller_shell::ProcArgs {
            env: env(&[format!("{}=/logins/elsewhere", accounts::LOGIN_ENV)]),
            ..untapped(PID)?
        })
    }
    let shown = read(several(elsewhere));
    assert_eq!(
        (shown.email, shown.weekly, shown.weekly_gap),
        (None, None, None),
        "a login no row lists fails the account gate: no profile, no week and no reason, so no \
         restart is promised"
    );
    let _ = std::fs::remove_dir_all(root());
}

/// A FOOTER KEPT OVER A TRANSIENT REGISTRY MISS KEEPS ITS WEEK: the read that finds no
/// registry row judges the tab's own tap figure as a fresh read does, and never falls back to
/// the bare profile read's `weekly unavailable`.
#[test]
fn a_footer_kept_over_a_registry_miss_keeps_its_taps_week() {
    fresh("footer-weekly-kept");
    profile(None);
    reading(PID as u32, NOW - 60, (380, RESET), None, None);
    let timeline = tab();
    let mut identity = unread(&records());
    let shown = reread(&timeline, &mut identity, asks(tapped, armed));
    assert_eq!(
        (shown.weekly, shown.weekly_gap),
        words("weekly 62% left"),
        "the control: the registry row read"
    );
    std::fs::remove_file(records().join(format!("sessions/{PID}.json"))).unwrap();
    let kept = reread(&timeline, &mut identity, asks(tapped, armed));
    assert_eq!(
        (kept.weekly, kept.weekly_gap),
        words("weekly 62% left"),
        "the registry row missed once"
    );
    let _ = std::fs::remove_dir_all(root());
}

/// A tapped process whose own environment names a Claude home of its own reads no tap figure,
/// even one whose tap said no home: the figure is only the default home's (`tap::gate` adds
/// the tap to no other).
#[test]
fn a_tapped_process_under_a_home_of_its_own_reads_no_tap_figure() {
    fn configured(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        Some(atpkg::caller_shell::ProcArgs {
            env: env(&[format!(
                "{}={}",
                accounts::HOME_ENV,
                root().join("claude").display()
            )]),
            ..tapped(PID)?
        })
    }
    fresh("footer-weekly-home");
    let dir = root().join("claude");
    std::fs::rename(records(), &dir).unwrap();
    profile_in(Some(&dir.display().to_string()), &dir, None);
    reading(PID as u32, NOW - 60, (380, RESET), None, None);
    let shown = read_in(&dir, asks(configured, armed));
    assert_eq!(
        shown.email.as_deref(),
        Some("tap@example.test"),
        "the control: its profile read passed its gate"
    );
    assert_eq!(
        (shown.weekly, shown.weekly_gap),
        none(WeeklyGap::NoReply),
        "CLAUDE_CONFIG_DIR set: a reading that names no home is still not this home's figure"
    );
    let _ = std::fs::remove_dir_all(root());
}

/// A WINDOW THAT ONLY RESET AWAY IS NO NEW REPORT, read by the footer: an idle tab's
/// five-hour window leaves the tap's re-report at its reset, and the week the tap first
/// reported three hours before is three hours old, not current.
#[test]
fn a_week_the_tap_re_reports_after_a_reset_is_as_old_as_its_first_report() {
    fresh("footer-weekly-reset");
    profile(None);
    let vars = tap::TapVars {
        sid: Some(TAB.into()),
        pid: Some(PID.to_string()),
        login: None,
        home: None,
    };
    let run = |five: bool, now: i64| {
        let five = if five {
            format!(
                r#""five_hour":{{"used_percentage":12,"resets_at":{}}},"#,
                NOW - 30
            )
        } else {
            String::new()
        };
        let stdin = format!(
            r#"{{"cost":{{"total_api_duration_ms":1200}},"rate_limits":{{{five}"seven_day":{{"used_percentage":38.0,"resets_at":{RESET}}}}}}}"#
        );
        let _ = tap::run(
            &mut stdin.as_bytes(),
            false,
            &vars,
            Some(&rl()),
            now,
            &mut Vec::<u8>::new(),
        );
    };
    run(true, NOW - 3 * 3600);
    run(false, NOW - 20);
    assert_eq!(
        weekly(asks(tapped, armed)),
        none(WeeklyGap::Stale),
        "the week three hours old"
    );
    run(false, NOW - 10);
    assert_eq!(weekly(asks(tapped, armed)), none(WeeklyGap::Stale));
    let _ = std::fs::remove_dir_all(root());
}

#[test]
fn a_footer_with_no_weekly_figure_says_why() {
    fn own_settings(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        Some(atpkg::caller_shell::ProcArgs {
            argv: vec![
                "claude".into(),
                "--settings".into(),
                "/work/own.json".into(),
            ],
            ..untapped(PID)?
        })
    }
    fn unknown_flag(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        Some(atpkg::caller_shell::ProcArgs {
            argv: vec!["claude".into(), "--a-flag-of-a-newer-claude".into()],
            ..untapped(PID)?
        })
    }
    fn one_shot(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        Some(atpkg::caller_shell::ProcArgs {
            argv: vec!["claude".into(), "--no-session-persistence".into()],
            ..untapped(PID)?
        })
    }
    fresh("footer-weekly-why");
    profile(None);
    let seat = |seat: fn() -> TapSeat| asks(untapped, seat);
    assert_eq!(
        weekly(seat(TapSeat::default)),
        none(WeeklyGap::TapOff),
        "the tap off: only /usage gives one"
    );
    assert_eq!(
        weekly(seat(armed)),
        none(WeeklyGap::Untapped),
        "the tap on, this process started without it, and a restart adds it"
    );
    assert_eq!(
        weekly(seat(|| TapSeat {
            named: false,
            ..armed()
        })),
        none(WeeklyGap::Unoffered(Refusal::Exe)),
        "no aterm the tap's command can name: no restart adds it"
    );
    assert_eq!(
        weekly(asks(own_settings, armed)),
        none(WeeklyGap::Unoffered(Refusal::Line)),
        "its own --settings"
    );
    assert_eq!(
        weekly(asks(unknown_flag, armed)),
        none(WeeklyGap::Unoffered(Refusal::Argv)),
        "a flag this build does not know: no relaunch line repeats it, and no --settings of its own"
    );
    assert_eq!(
        weekly(asks(one_shot, armed)),
        none(WeeklyGap::Unoffered(Refusal::Argv)),
        "an option that cannot resume"
    );
    let own_status_line = Asks {
        tap: TapAsks {
            seat: armed,
            settings: |_, _| false,
            restart: |_, _| true,
        },
        ..seat(armed)
    };
    assert_eq!(
        weekly(own_status_line),
        none(WeeklyGap::Unoffered(Refusal::Settings)),
        "its own statusLine"
    );
    let unrestarted = Asks {
        tap: TapAsks {
            seat: armed,
            settings: |_, _| true,
            restart: |tab, pid| {
                assert_eq!(
                    (tab, pid),
                    (TAB, PID as u32),
                    "asked of its own tab and process"
                );
                false
            },
        },
        ..seat(armed)
    };
    assert_eq!(
        weekly(unrestarted),
        none(WeeklyGap::Unrestarted),
        "a line would tap it, and no restart in place is offered for it (its Autopilot off)"
    );
    let dir = own_config_dir();
    assert_eq!(
        weekly_in(&dir, asks(configured, armed)),
        none(WeeklyGap::Unoffered(Refusal::Home)),
        "a CLAUDE_CONFIG_DIR of its own: no restart adds it"
    );
    std::fs::rename(&dir, records()).unwrap();
    let other = own_home_dir();
    assert_eq!(
        weekly_in(&other, asks(own_home, armed)),
        none(WeeklyGap::Unoffered(Refusal::Home)),
        "the default Claude home under a HOME of its own: no relaunch line taps it"
    );
    std::fs::rename(&other, records()).unwrap();
    assert_eq!(
        weekly_in(&root().join(".claude"), asks(untapped, armed)),
        none(WeeklyGap::Untapped),
        "the control: the same process in its own home"
    );
    let _ = std::fs::remove_dir_all(root());
}

/// The accounts store's week reaches the footer through the resolver: a weekly wall it
/// holds says `weekly 0% left`, a figure newer than the footer's own aged one wins, and a
/// refused profile read reads none of it.
#[test]
fn the_resolver_reads_the_stores_week_under_the_same_rule() {
    let save = save_main;
    fresh("footer-weekly-store");
    profile(None);
    save(None, true);
    assert_eq!(
        weekly(asks(untapped, stored)),
        words("weekly 0% left"),
        "the store's weekly wall"
    );
    assert_eq!(
        weekly(asks(untapped, armed)),
        none(WeeklyGap::Untapped),
        "the control: no store read"
    );
    reading(PID as u32, NOW - 20 * 60, (380, RESET), None, None);
    save(Some(NOW - 5 * 60), false);
    assert_eq!(
        weekly(asks(tapped, stored)),
        words("weekly 60% left"),
        "the store's newer figure over the footer's own aged one"
    );
    save(Some(NOW - 30 * 60), false);
    assert_eq!(
        weekly(asks(tapped, stored)),
        words("weekly 62% left \u{b7} 20m ago"),
        "the footer's own over an older one"
    );
    save(Some(NOW - 60), true);
    let refused = Asks {
        stored_login: |_, _, _| false,
        ..asks(tapped, stored)
    };
    assert_eq!(
        weekly(refused),
        (None, None),
        "a refused profile read reads no store"
    );
    let _ = std::fs::remove_dir_all(root());
}

/// The store's figures (the tap's `windows["seven_day"]`, the `/usage` probe's sample) take
/// the footer's own age rule, never only "while their window runs".
#[test]
fn the_stores_weekly_figures_take_the_same_age_rule() {
    let fig = |pct_x10: u32, at: i64| accounts::WindowFig {
        pct_x10,
        reset: RESET,
        at,
        by: "m22".into(),
        source: accounts::SOURCE_TAP.into(),
        epoch_at: None,
    };
    let with = |fig: Option<accounts::WindowFig>, sample: Option<accounts::Sample>| {
        let mut acct = accounts::Account::default();
        if let Some(fig) = fig {
            acct.windows.insert("seven_day".into(), fig);
        }
        acct.sample = sample;
        acct
    };
    let words = |acct: &accounts::Account| {
        let week = store_week(acct, NOW);
        let said = week
            .figure
            .map(|figure| figure.words(NOW).expect("a figure held is shown"));
        (week.walled, said)
    };
    assert_eq!(
        words(&with(Some(fig(380, NOW - 60)), None)),
        (false, Some("weekly 62% left".into()))
    );
    assert_eq!(
        words(&with(Some(fig(380, NOW - 20 * 60)), None)),
        (false, Some("weekly 62% left \u{b7} 20m ago".into()))
    );
    assert_eq!(
        words(&with(Some(fig(380, NOW - 2 * 86_400)), None)),
        (false, None),
        "a days-old reading whose window still runs is no figure"
    );
    let sample = |pct: u32, at: i64| accounts::Sample {
        week_pct: Some(pct),
        week_reset: Some(RESET),
        at,
        ..accounts::Sample::default()
    };
    assert_eq!(
        words(&with(None, Some(sample(40, NOW - 61 * 60)))),
        (false, None),
        "nor an hour-old sample"
    );
    assert_eq!(
        words(&with(
            Some(fig(380, NOW - 20 * 60)),
            Some(sample(40, NOW - 5 * 60))
        )),
        (false, Some("weekly 60% left".into())),
        "the newer of the two"
    );
    assert_eq!(
        words(&with(
            Some(fig(380, NOW - 3 * 3600)),
            Some(sample(38, NOW - 60))
        )),
        (false, Some("weekly 62% left".into())),
        "equal values do not make a newer panel sample a tap heartbeat"
    );
    assert_eq!(
        words(&with(
            Some(fig(380, NOW - 20 * 60)),
            Some(sample(38, NOW - 60))
        )),
        (false, Some("weekly 62% left".into())),
        "older records with no per-window time retain the sample's actual timestamp"
    );
    assert_eq!(
        words(&with(
            Some(fig(380, NOW + 60)),
            Some(sample(40, NOW - 5 * 60))
        )),
        (false, Some("weekly 62% left".into())),
        "a reading stamped a minute ahead of this clock is just read"
    );
    assert_eq!(
        words(&with(
            Some(fig(380, NOW + 61)),
            Some(sample(40, NOW - 5 * 60))
        )),
        (false, Some("weekly 60% left".into())),
        "one stamped further ahead loses to one it can show"
    );
    let mut walled = with(None, None);
    walled.walls.insert(
        accounts::WEEK_LIMIT.into(),
        accounts::Wall {
            until: Some(RESET),
            at: NOW - 60,
            by: "m22".into(),
            attempts: 0,
            cleared: false,
        },
    );
    assert_eq!(words(&walled), (true, None));

    let mut state = accounts::State {
        machine: "m22".into(),
        ..accounts::State::default()
    };
    let old = NOW - 3 * 3600;
    let read = accounts::TapRead {
        seven_day: Some(accounts::WindowRead {
            pct_x10: 380,
            reset: RESET,
            refilled: None,
        }),
        at: old,
        seen_before: true,
        ..accounts::TapRead::default()
    };
    assert_eq!(
        accounts::note_reading(&mut state, "main", &read, old).len(),
        1
    );
    assert_eq!(
        accounts::note_reading(&mut state, "main", &read, NOW - 60).len(),
        1,
        "the tap heartbeat publishes a new sample"
    );
    let reload = |state: &accounts::State, now| {
        accounts::State::from_json(&state.to_json(), now).expect("the stored observation times")
    };
    state = reload(&state, NOW);
    let held = state.accounts["main"].sample.as_ref().unwrap();
    assert_eq!((held.at, held.week_read_at), (NOW - 60, Some(old)));
    assert_eq!(
        words(&state.accounts["main"]),
        (false, None),
        "a real tap heartbeat cannot refresh the old week after storage and reload"
    );
    let mut legacy: aterm_json::Value = aterm_json::from_str(&state.to_json()).unwrap();
    let legacy_sample = legacy
        .as_object_mut()
        .and_then(|top| top.get_mut("accounts"))
        .and_then(aterm_json::Value::as_object_mut)
        .and_then(|accounts| accounts.get_mut("main"))
        .and_then(aterm_json::Value::as_object_mut)
        .and_then(|account| account.get_mut("sample"))
        .and_then(aterm_json::Value::as_object_mut)
        .unwrap();
    legacy_sample.remove("session_read_at");
    legacy_sample.remove("week_read_at");
    let legacy = accounts::State::from_json(&aterm_json::to_string(&legacy).unwrap(), NOW).unwrap();
    assert_eq!(
        legacy.accounts["main"]
            .sample
            .as_ref()
            .unwrap()
            .week_read_at,
        None
    );
    assert_eq!(
        words(&legacy.accounts["main"]),
        (false, Some("weekly 62% left".into())),
        "legacy absence retains its timestamp; values cannot reconstruct an observation's origin"
    );

    assert_eq!(
        accounts::note_sample(&mut state, "main", sample(38, NOW), NOW).len(),
        1,
        "an independent settled panel read, even with the same percent and reset"
    );
    state = reload(&state, NOW);
    assert_eq!(
        words(&state.accounts["main"]),
        (false, Some("weekly 62% left".into())),
        "the real fresh probe survives storage and reaches the weekly consumer"
    );
    let beat = NOW + accounts::READING_HEARTBEAT_S;
    assert_eq!(
        accounts::note_reading(&mut state, "main", &read, beat).len(),
        1
    );
    state = reload(&state, beat);
    let held = state.accounts["main"].sample.as_ref().unwrap();
    assert_eq!(
        (held.at, held.week_read_at),
        (beat, Some(NOW)),
        "a later tap heartbeat preserves the newer matching probe observation"
    );
    assert_eq!(
        store_week(&state.accounts["main"], beat)
            .figure
            .and_then(|figure| figure.words(beat)),
        Some("weekly 62% left".into())
    );
    let kept = state.clone();
    assert!(accounts::note_reading(&mut state, "main", &read, beat + 1).is_empty());
    assert_eq!(
        state, kept,
        "publication time still bounds heartbeat scheduling"
    );
}

/// ONLY THE MACHINE'S DEFAULT HOME, ON ITS OWN LOGIN, READS THE STORE'S WEEK: the store keeps
/// the default home's weeks under the label its own login is judged to (the tap's note), so a
/// Claude Code under a `CLAUDE_CONFIG_DIR` or a `HOME` of its own, signed in to whichever
/// account, never shows the machine's own account's week or wall as its own.
#[test]
fn a_process_outside_the_default_home_reads_none_of_the_stores_week() {
    fresh("footer-weekly-store-own");
    profile(None);
    save_main(Some(NOW - 60), false);
    assert_eq!(
        weekly(asks(untapped, stored)),
        words("weekly 60% left"),
        "the control: the default home's own account's week"
    );
    let dir = own_config_dir();
    let shown = read_in(&dir, asks(configured, stored));
    assert_eq!(
        (shown.email.as_deref(), shown.weekly, shown.weekly_gap),
        (
            Some("tap@example.test"),
            None,
            Some(WeeklyGap::Unoffered(Refusal::Home))
        ),
        "a CLAUDE_CONFIG_DIR of its own: its profile passed its gate, and the store's week is none of its"
    );
    save_main(None, true);
    assert_eq!(
        weekly_in(&dir, asks(configured, stored)),
        none(WeeklyGap::Unoffered(Refusal::Home)),
        "nor is the store's weekly wall"
    );
    std::fs::rename(&dir, records()).unwrap();
    assert_eq!(
        weekly(asks(untapped, stored)),
        words("weekly 0% left"),
        "the control: the wall is the default home's own"
    );
    save_main(Some(NOW - 60), false);
    let other = own_home_dir();
    assert_eq!(
        weekly_in(&other, asks(own_home, stored)),
        none(WeeklyGap::Unoffered(Refusal::Home)),
        "the default Claude home under a HOME of its own"
    );
    std::fs::rename(&other, records()).unwrap();
    fn lone_row() -> Option<accounts::Roster> {
        Some(accounts::Roster {
            rows: vec![accounts::Row {
                label: aterm_agent::harness::cli::ACCOUNTS_OWN_LABEL.into(),
                class: accounts::Class::Dedicated,
                dir: Some("/logins/main".into()),
                email: Some("tap@example.test".into()),
                ..accounts::Row::default()
            }],
        })
    }
    let shown = read(Asks {
        roster: lone_row,
        ..asks(untapped, stored)
    });
    assert_eq!(
        (shown.email.as_deref(), shown.weekly.as_deref()),
        (Some("tap@example.test"), None),
        "the one row is another login's, not the default one this process runs on: no label is \
         judged, so no store week (never the store's active or `main` by default)"
    );
    fn personal() -> Option<accounts::Roster> {
        Some(accounts::Roster {
            rows: vec![accounts::Row {
                label: "personal".into(),
                class: accounts::Class::Dedicated,
                dir: None,
                email: Some("tap@example.test".into()),
                ..accounts::Row::default()
            }],
        })
    }
    let on_personal = Asks {
        roster: personal,
        ..asks(untapped, stored)
    };
    let shown = read(on_personal);
    assert_eq!(
        (shown.weekly.as_deref(), shown.label.as_deref()),
        (None, Some("personal")),
        "the default login's row is labelled `personal`: the week saved under `main` is not its"
    );
    save_as("personal", Some(NOW - 60), false);
    let shown = read(on_personal);
    assert_eq!(
        (shown.weekly.as_deref(), shown.label.as_deref()),
        (Some("weekly 60% left"), Some("personal")),
        "the store's week under the label this process's login is judged to"
    );
    let _ = std::fs::remove_dir_all(root());
}

/// A 7D WALL IN THE TRANSCRIPT BEATS THE FOOTER'S OWN AGED FIGURE: at the weekly limit, a tap
/// reading twenty minutes old says `weekly 0% left`, never the week it read before the wall;
/// a current reading still says its own.
#[test]
fn a_weekly_wall_beats_the_footers_own_aged_figure() {
    fresh("footer-weekly-wall");
    profile(None);
    reading(PID as u32, NOW - 20 * 60, (970, RESET), None, None);
    assert_eq!(
        weekly(asks(tapped, armed)),
        words("weekly 3% left \u{b7} 20m ago"),
        "the control: no wall"
    );
    weekly_wall();
    assert_eq!(
        weekly(asks(tapped, armed)),
        words("weekly 0% left"),
        "the wall over an aged reading"
    );
    reading(PID as u32, NOW - 60, (970, RESET), None, None);
    assert_eq!(
        weekly(asks(tapped, armed)),
        words("weekly 3% left"),
        "a current reading says its own"
    );
    let _ = std::fs::remove_dir_all(root());
}

/// THE WINDOW READS WHERE THE TAP WRITES: the live seat's readings are in the directory
/// `aterm harness tap` writes them to, so a tapped tab never waits on a file nobody writes.
#[test]
fn the_live_seat_reads_the_directory_the_tap_writes() {
    let seat = super::live_tap();
    assert!(seat.dir.is_some(), "a harness state directory");
    assert_eq!(seat.dir, tap::rl_dir());
}
