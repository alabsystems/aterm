// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's account judgments read the observation, not the belief
//! (`docs/DESIGN-account-discovery-2026-11-09.md`, step 1), from the live bug on m65.

use super::*;
use aterm_agent::harness::journal::ObservedSrc;
use aterm_agent::harness::observe::Observed;

const UNICORN: &str = "unicorn.m15.2@proton.example";
const BRONTO: &str = "brontosaurus.m22@proton.example";

fn look(seen: Observation) -> AccountLook {
    AccountLook {
        model: aterm_agent::harness::accounts::ModelWeek::Unread,
        screen_wall: None,
        profile_env: Vec::new(),
        token_served: ServedToken::Unnamed(None),
        who: accounts::ProcessLogin {
            login: None,
            default_home: true,
            route: accounts::AuthRoute::Stored,
        },
        label: Some("m22".into()),
        snap: snap("s-observed"),
        generation: Arc::default(),
        seen,
    }
}

fn seen(email: &str) -> Observation {
    Observation::Seen(Observed {
        cli: accounts::Cli::Claude.into(),
        email: email.to_owned(),
        key: None,
        region: None,
        src: ObservedSrc::Profile,
        path: "/home/someone/.claude.json".into(),
        mtime: Some(1),
        plan: None,
    })
}

fn roster(text: &str) -> accounts::Roster {
    accounts::Roster::parse_for(accounts::Cli::Claude, text).unwrap()
}

/// The execution hold, the probe and the tap judge the row the login file names: the believed
/// default row `m22` while the file agrees, the row that lists unicorn where one does, and none
/// for an account no row lists, so nothing is held, probed or filed under `m22` on unicorn's
/// behalf.
/// NEGATIVE CONTROL: with `AccountLook::judged` reading `accounts::judged_label`, the unlisted
/// tab is held under `m22`: `` left: Some("m22") right: None ``.
#[test]
fn hold_uses_observed_row() {
    let one = roster(&format!(
        "[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n"
    ));
    assert_eq!(look(seen(BRONTO)).judged(&one), Some("m22"));
    assert_eq!(look(Observation::Unread).judged(&one), Some("m22"));
    assert_eq!(look(seen(UNICORN)).judged(&one), None);
    let dir = if cfg!(windows) {
        "C:\\\\logins\\\\u"
    } else {
        "/logins/u"
    };
    let two = roster(&format!(
        "[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n\n\
         [[account]]\nlabel = \"m15\"\nclass = \"dedicated\"\ndir = \"{dir}\"\nemail = \"{UNICORN}\"\n"
    ));
    assert_eq!(look(seen(UNICORN)).judged(&two), Some("m15"));
}

/// A tap reading of an account no row lists is no row's: the reading's owner for the believed
/// row is another account, so `note_reading_held` files nothing there, and the tap's word is
/// [`TAP_UNLISTED`], which sends it to the journal instead.
/// NEGATIVE CONTROL: with `ReadingOwner::of` giving `Unobserved` for `Unlisted`, the owner is
/// unobserved: `` left: Unobserved right: Another ``.
#[test]
fn unlisted_tap_reading_goes_to_journal_not_state() {
    let one = roster(&format!(
        "[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n"
    ));
    let tab = look(seen(UNICORN));
    let reconciled = accounts::reconcile(&one, &tab.who, &tab.seen);
    assert!(matches!(reconciled, accounts::Reconciled::Unlisted(_)));
    assert_eq!(
        accounts::ReadingOwner::of(&reconciled, "m22"),
        accounts::ReadingOwner::Another
    );
    let mut state = accounts::State::default();
    let read = accounts::TapRead {
        seven_day: Some(accounts::WindowRead {
            pct_x10: 620,
            reset: 1_800_000_000,
            refilled: None,
        }),
        at: 1_790_000_000,
        ..Default::default()
    };
    let (events, _) = accounts::note_reading_held(
        &mut state,
        "m22",
        &read,
        1_790_000_000,
        accounts::ReadingOwner::of(&reconciled, "m22"),
    );
    assert!(events.is_empty());
    assert!(
        state
            .accounts
            .get("m22")
            .is_none_or(|a| a.windows.is_empty())
    );
    // The journal's notes of it are keyed by the observed account.
    aterm_agent::harness::journal::with_keyer(
        |_, email| (email == UNICORN).then(|| "ac-00000000000000a1".to_owned()),
        || {
            let key = aterm_agent::harness::journal::claude_email_key(UNICORN).unwrap();
            let notes =
                aterm_agent::harness::journal::quota_notes(accounts::Cli::Claude, key, &read);
            assert_eq!(notes.len(), 1);
            assert_eq!(notes[0].kind, "quota");
            assert_eq!(notes[0].window, Some("7d"));
            assert_eq!(notes[0].account, Some(key));
        },
    );
}

/// THE WALL BUG, at the host: a default-home tab whose `~/.claude.json` names unicorn hits its
/// weekly limit while the roster's dir-less row is `m22`. The wall is `m15`'s, the row that
/// lists unicorn, and `m22` is not walled.
/// NEGATIVE CONTROL: with `account_wall_of` passing `Observation::Unread` to the recorder, the
/// wall is the believed row's: `` left: ("wall", Some("m22")) right: ("wall", Some("m15")) ``.
#[test]
fn a_wall_is_recorded_on_the_account_the_login_file_names() {
    const NOW: i64 = 1_790_000_000;
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-hh-observed-wall-")
        .tempdir()
        .unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join(".claude.json"),
        format!(r#"{{"oauthAccount":{{"emailAddress":"{UNICORN}"}}}}"#),
    )
    .unwrap();
    let dir = if cfg!(windows) {
        "C:\\\\logins\\\\u"
    } else {
        "/logins/u"
    };
    let two = roster(&format!(
        "[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n\n\
         [[account]]\nlabel = \"m15\"\nclass = \"dedicated\"\ndir = \"{dir}\"\nemail = \"{UNICORN}\"\n"
    ));
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let store = accounts::Store::at(&state);
    let captured = usage_fixture(&store, &two, NOW);
    let env = vec!["PATH=/usr/bin".to_owned()];
    let read = WallRead {
        env: &env,
        settled: true,
        wall: Some(aterm_agent::harness::upgrade::StandingWall {
            at: (NOW - 10).unsigned_abs(),
            until: Some((NOW + 3600).unsigned_abs()),
            window: Some("seven_day".to_owned()),
            credits: None,
        }),
        screen_wall: None,
        started: Some(NOW - 70),
    };
    let (word, label) = super::super::account_wall_of(
        (&store, Some(home.as_path())),
        &captured,
        "s-observed",
        &read,
        NOW,
    );
    assert_eq!((word, label.as_deref()), ("wall", Some("m15")));
    let held = store.load(NOW).unwrap().0;
    assert_eq!(held.accounts["m15"].walls.len(), 1);
    assert!(held.accounts.get("m22").is_none_or(|a| a.walls.is_empty()));
}

/// THE FLOOD BUG: a tap reading of an account no row lists is sent to the journal once. The
/// same windows of the same account are news again only a heartbeat later; other windows or
/// another account are news at once.
/// NEGATIVE CONTROL: with `tap_unlisted_due` always true, the second step publishes again:
/// `` assertion failed: !tap_unlisted_due(..) ``.
#[test]
fn an_unlisted_tap_reading_is_published_once() {
    let key = |text: &str| journal::Key::new(text).unwrap();
    let (a, b) = (key("ac-00000000000000a1"), key("ac-00000000000000b2"));
    let window = |pct| {
        Some(tap::Window {
            pct,
            resets_at: 1_800_000_000,
            pct_x10: Some(pct * 10),
        })
    };
    let now = 1_790_000_000;
    let sent = (a, (window(62), None), now);
    assert!(tap_unlisted_due(None, a, (window(62), None), now));
    assert!(!tap_unlisted_due(
        Some(&sent),
        a,
        (window(62), None),
        now + 10
    ));
    assert!(!tap_unlisted_due(
        Some(&sent),
        a,
        (window(62), None),
        now + accounts::READING_HEARTBEAT_S - 1
    ));
    assert!(tap_unlisted_due(
        Some(&sent),
        a,
        (window(62), None),
        now + accounts::READING_HEARTBEAT_S
    ));
    assert!(tap_unlisted_due(
        Some(&sent),
        a,
        (window(63), None),
        now + 10
    ));
    assert!(tap_unlisted_due(
        Some(&sent),
        b,
        (window(62), None),
        now + 10
    ));
}

/// A session on a Mac that lists no account is sighted: the worker's sighting names the login
/// its home's profile holds and opens its session under that account, with no roster path, no
/// roster file, a roster that lists none, and the account rotation and login repair both off;
/// the same tab is named at every look (the store keeps the machine id its first look made),
/// and the account advice of that Mac stays inert.
/// NEGATIVE CONTROL: with `sight` taking its roster from `account_roster(&seat)?` (the gate
/// `account_note` sat behind), the Mac with no roster is not sighted: `no roster path: left:
/// None right: Some(["seen", "session-start"])`.
#[test]
fn a_session_on_a_mac_that_lists_no_account_is_sighted() {
    use aterm_agent::harness::journal::{self, LoginState};
    journal::with_keyer(
        |_, email| (email == UNICORN).then(|| "ac-00000000000000a1".to_owned()),
        || {
            let a = Accts::new();
            std::fs::write(
                a.tmp.path().join("home").join(".claude.json"),
                format!(r#"{{"oauthAccount":{{"emailAddress":"{UNICORN}"}}}}"#),
            )
            .unwrap();
            a.spawn("s-sighted", ACCT_TALK_A, "main");
            let mut quiet = on();
            quiet.set("accounts", "off").unwrap();
            quiet.set("logins", "off").unwrap();
            let kinds = |sighted: &Option<observe::Sighted>| {
                sighted
                    .as_ref()
                    .map(|sighted| sighted.notes.iter().map(|n| n.kind).collect::<Vec<_>>())
            };
            let mut tabs = Vec::new();
            let mut first = |why: &str, cfg: &SupervisorConfig| {
                let idle = a.worker("s-sighted", cfg);
                let sighted = idle.sight();
                assert_eq!(
                    kinds(&sighted),
                    Some(vec!["seen", "session-start"]),
                    "{why}"
                );
                let login = sighted.unwrap().login.expect("the registry's login");
                assert_eq!(
                    (login.email.as_str(), login.state),
                    (UNICORN, LoginState::Running),
                    "{why}"
                );
                tabs.push(login.tab.expect("its tab"));
                assert_eq!(kinds(&idle.sight()), Some(vec![]), "{why}: noted once");
                assert_eq!(idle.account(AccountPoint::TurnEnd), None, "{why}");
            };
            a.listed.store(false, Ordering::SeqCst);
            first("no roster path", &on());
            a.listed.store(true, Ordering::SeqCst);
            std::fs::remove_file(a.roster_path()).unwrap();
            first("no roster file", &on());
            std::fs::write(a.roster_path(), "").unwrap();
            first("a roster that lists none", &on());
            first("accounts and logins off", &quiet);
            assert!(
                tabs.windows(2).all(|pair| pair[0] == pair[1]),
                "one tab, by the kept machine id: {tabs:?}"
            );
        },
    );
}

/// THE ACCOUNTS CARD NAMES THE ACCOUNT ON A MAC THAT LISTS NONE: with no row to judge the login
/// against, the card still says the address the session's login file names, and that no account
/// is listed, never "unknown".
/// NEGATIVE CONTROL: with the card returning at `RosterUnread::Nothing` before it reads the login
/// (as it did), its account is `None` and its inventory unknown.
#[test]
fn the_card_on_a_mac_that_lists_no_account_names_the_observed_address() {
    let a = Accts::new();
    std::fs::write(
        a.tmp.path().join("home").join(".claude.json"),
        format!(r#"{{"oauthAccount":{{"emailAddress":"{UNICORN}"}}}}"#),
    )
    .unwrap();
    a.spawn("s-card", ACCT_TALK_A, "main");
    std::fs::write(a.roster_path(), "").unwrap();
    let idle = a.worker("s-card", &on());
    // The card reads a process whose start the kernel says: this test's own.
    let pid = std::process::id();
    let mut snapshot = idle.kept.lock().unwrap().snapshot.clone().unwrap();
    let env = a.procs.lock().unwrap().remove(&snapshot.pid).unwrap();
    snapshot.pid = pid;
    snapshot.start = aterm_agent::harness::restart_stamp::kernel_start(pid).unwrap();
    a.procs.lock().unwrap().insert(pid, env);
    a.tabs
        .lock()
        .unwrap()
        .insert(snapshot.tab.clone(), snapshot.clone());
    idle.kept.lock().unwrap().snapshot = Some(snapshot);
    let request = AdviceTurn::begin(&idle.kept).capture(&idle.kept).unwrap();
    let card = idle
        .read_account_progress(&request, AccountPoint::TurnEnd, None, None)
        .expect("the card")
        .value;
    assert_eq!(card.account.as_deref(), Some(UNICORN));
    assert_eq!(card.known, Some(0));
    assert_eq!(
        card.details.first().map(String::as_str),
        Some("No accounts are listed")
    );
}

/// THE ACCOUNT JUDGMENT READS THE MODEL THE SESSION'S PROCESS RUNS (FL-264): the kept snapshot's
/// argv, the fact the Fable fallback reads ([`IdleHost::launch_model`]), and beside it what pins
/// the model as Claude Code chooses it ([`WorkerIdle::model_week_in`]): the model its last turn
/// was served by (`IdleHost::answered_by`, a `/model` switch), its `ANTHROPIC_MODEL` and its
/// settings' `model` ([`Acts::model_settings`]). So the look's [`accounts::Me`] ranks the rows by
/// the model the line runs: Fable, with the fallback, where nothing names one; Opus where the
/// line names Opus (the line the fallback relaunches), the environment or a setting pins it, or a
/// turn was served by it; none for a model of no week of its own; unread where a pin cannot be
/// read. CLOSED GATE: an agent this host reads no launch of (no Fable fallback) reads unread, in
/// the look and for the fallback alike.
#[test]
fn an_account_look_carries_the_model_its_launch_names() {
    use aterm_agent::harness::accounts::ModelWeek;
    use aterm_agent::harness::netwatch::Source;
    use aterm_agent::harness::profile::Bucket;
    let a = Accts::new();
    let roster = a.roster();
    let fable = ModelWeek::FallsBack(Bucket::Fable);
    let opus = ModelWeek::Of(Bucket::SevenDayOpus);
    struct Case {
        argv: &'static [&'static str],
        env: &'static [&'static str],
        settings: Option<Source>,
        served: Option<&'static str>,
        agent: Program,
        look: ModelWeek,
        fallback: ModelWeek,
    }
    let case = |argv: &'static [&'static str], look: ModelWeek, fallback: ModelWeek| Case {
        argv,
        env: &[],
        settings: None,
        served: None,
        agent: Program::Claude,
        look,
        fallback,
    };
    let opus_line: &'static [&'static str] = &["/opt/claude", "--model", "claude-opus-5-5"];
    let plain: &'static [&'static str] = &["/opt/claude"];
    let cases = [
        case(plain, fable, fable),
        case(opus_line, opus, opus),
        case(
            &["/opt/claude", "--model", "haiku"],
            ModelWeek::Bucketless,
            ModelWeek::Bucketless,
        ),
        Case {
            env: &["ANTHROPIC_MODEL=claude-opus-5-5"],
            ..case(plain, opus, fable)
        },
        Case {
            settings: Some(Source::Text(r#"{"model": "opus"}"#.to_owned())),
            ..case(plain, opus, fable)
        },
        Case {
            settings: Some(Source::Unreadable),
            ..case(plain, ModelWeek::Unread, fable)
        },
        Case {
            served: Some("claude-opus-5-5"),
            ..case(plain, opus, fable)
        },
        Case {
            agent: Program::Codex,
            ..case(plain, ModelWeek::Unread, ModelWeek::Unread)
        },
        Case {
            agent: Program::Codex,
            env: &["ANTHROPIC_MODEL=claude-opus-5-5"],
            served: Some("claude-opus-5-5"),
            ..case(opus_line, ModelWeek::Unread, ModelWeek::Unread)
        },
    ];
    for (i, c) in cases.into_iter().enumerate() {
        let mut snap = a.spawn("s-model", ACCT_TALK_A, "main");
        snap.argv = c.argv.iter().map(|arg| (*arg).to_owned()).collect();
        a.procs
            .lock()
            .unwrap()
            .get_mut(&snap.pid)
            .expect("its environment")
            .extend(c.env.iter().map(|kv| (*kv).to_owned()));
        a.tabs
            .lock()
            .unwrap()
            .insert(snap.tab.clone(), snap.clone());
        let mut idle = idle_host("s-model", c.agent, switched(&on()), a.hooks());
        idle.kept.lock().unwrap().snapshot = Some(snap);
        // The agent holds its tab, as the fallback's reader requires.
        idle.hooks.acts.follow = Arc::new(|_, _| Foreground::Agent);
        let settings = c.settings.clone();
        idle.hooks.acts.model_settings = Arc::new(move |_, _, _| {
            settings
                .iter()
                .map(|source| ("settings.json".to_owned(), source.clone()))
                .collect()
        });
        if let Some(model) = c.served {
            idle.answered_by(model, ACCT_NOW);
        }
        let look = idle
            .account_look(&(idle.hooks.acts.account_seat)().unwrap(), &roster)
            .expect("the look");
        assert_eq!(look.me(&roster).model, c.look, "case {i}: {:?}", c.argv);
        assert_eq!(
            idle.launch_model().week(),
            c.fallback,
            "case {i}: the fallback reads the line alone: {:?}",
            c.argv
        );
    }
}

/// THE MOVE'S COMMIT JUDGES THE MODEL THE ADVICE DID (FL-264): a Fable session at a five-hour
/// wall on `main`, `active` the proven, fresh and Fable-walled `work-2`: the advice passes over it
/// for `pool-a`, which has Fable's room, and the commit (`accounts::begin_rotation_for` with the
/// point's window and the look's model) rotates there, so the session moves. CONTROL: a session
/// of a model of no week of its own (`--model haiku`) follows `work-2` and rotates nothing.
/// NEGATIVE CONTROL: with the commit asked as `accounts::begin_rotation` (no window, the model
/// unread), or with `ModelWeek::Unread`, `work-2` holds the Fable session's move:
/// `wait:account-changed`, and the session never leaves its walled row.
#[test]
fn a_move_past_an_active_walled_on_the_sessions_model_commits() {
    let fable_week = accounts::week_bucket(aterm_phase::WeekModel::Fable);
    for (argv, to, rotations) in [
        (&["/opt/claude"][..], "pool-a", 1),
        (&["/opt/claude", "--model", "haiku"][..], "work-2", 0),
    ] {
        let a = Accts::new();
        let mut snap = a.spawn("s-fable", ACCT_TALK_A, "main");
        snap.argv = argv.iter().map(|arg| (*arg).to_owned()).collect();
        a.tabs.lock().unwrap().insert(snap.tab.clone(), snap);
        a.wall(ACCT_TALK_A, ACCT_NOW - 10);
        a.store()
            .transact(a.clock(), &mut None, |state| {
                state.active = Some("work-2".into());
                state.proven = true;
                Ok(accounts::note_wall(
                    state,
                    "work-2",
                    fable_week,
                    Some(ACCT_NOW + 3 * 86_400),
                    ACCT_NOW - 60,
                    ACCT_NOW - 60,
                ))
            })
            .unwrap();
        let idle = a.worker("s-fable", &on());
        assert_eq!(moves(&idle.account(usage_wall())), Some(to), "{argv:?}");
        assert_eq!(
            idle.restart_once(&move_to(to, AccountAt::Wall)).as_deref(),
            Some("adopted"),
            "{argv:?}"
        );
        let state = a.state();
        assert_eq!(state.active.as_deref(), Some(to), "{argv:?}");
        assert_eq!(state.rotations.len(), rotations, "{argv:?}");
    }
}
