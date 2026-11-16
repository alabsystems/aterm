// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::codex_app_login::{IdentifiedRead, IdentifiedUsage};
use crate::harness::codex_renewal::Identity;
use crate::harness::codex_rotation::{self, UsageReader};
use std::cell::RefCell;
use std::collections::BTreeMap;

#[path = "codex_rotation_publication_tests.rs"]
mod publication;

#[path = "codex_rotation_canary_tests.rs"]
mod canary_ownership;

#[path = "codex_rotation_session_tests.rs"]
mod session_scope;

#[path = "codex_login_rotation_tests.rs"]
mod login_rotation;

#[path = "codex_rotation_pace_tests.rs"]
mod pace;

struct Reader {
    calls: RefCell<Vec<PathBuf>>,
    answers: BTreeMap<PathBuf, IdentifiedUsage>,
}

impl UsageReader for Reader {
    fn read(
        &self,
        path: &Path,
        _: Option<&str>,
        _: i64,
        wait: Duration,
        use_token: crate::harness::provider_read::Use,
    ) -> IdentifiedRead {
        (|| -> Option<IdentifiedUsage> {
            use_token
                .begin_home(
                    crate::harness::provider_read::Read::CodexEndpoint,
                    path,
                    NOW,
                )
                .ok()?;
            assert!(!wait.is_zero());
            self.calls.borrow_mut().push(path.to_owned());
            self.answers.get(path).cloned()
        })()
        .into()
    }
}

fn usage(account: &str, full: bool) -> IdentifiedUsage {
    usage_read_at(account, full, NOW)
}

/// [`usage`] as a read at `at` answers it.
fn usage_read_at(account: &str, full: bool, at: i64) -> IdentifiedUsage {
    IdentifiedUsage {
        identity: Identity::from_native(account, "user"),
        usage: crate::harness::codex_app_login::usage_from_backend(
            &aterm_json::json!({
                "plan_type": "pro",
                "rate_limit": {
                    "allowed": !full,
                    "primary_window": {"used_percent": 0, "limit_window_seconds": 18000,
                        "reset_at": NOW + 3600},
                    "secondary_window": {"used_percent": if full { 100 } else { 0 },
                        "limit_window_seconds": 604800, "reset_at": NOW + 86_400}
                }
            }),
            at,
        )
        .expect("real provider percentages"),
    }
}

fn weekly_session(world: &World) -> Session<'_> {
    let mut session = session(Cli::Codex, Supervisor::Own, Mode::Early, &world.exe);
    session.point = Point::Wall {
        window: "seven_day".into(),
        at: Some(NOW),
    };
    session
}

fn fixture(name: &str) -> (World, Reader) {
    let world = World::new(name);
    let work = world.two(Cli::Codex).join("auth.json");
    world.on_main(Cli::Codex, &[]);
    let source = world.home.join(".codex/auth.json");
    let reader = Reader {
        calls: RefCell::new(Vec::new()),
        answers: BTreeMap::from([
            (source, usage("source", true)),
            (work, usage("target", false)),
        ]),
    };
    (world, reader)
}

fn native_auth(path: &Path, account: &str) {
    native_auth_naming(path, account, None);
}

/// [`native_auth`], whose login also names `email`.
fn native_auth_naming(path: &Path, account: &str, email: Option<&str>) {
    let mut claims = aterm_json::json!({"https://api.openai.com/auth": {
        "chatgpt_account_id": account, "chatgpt_user_id": "user"
    }});
    if let (Some(email), Some(object)) = (email, claims.as_object_mut()) {
        object.insert(
            "email".to_owned(),
            aterm_json::Value::from(email.to_owned()),
        );
    }
    let encoded = aterm_codec::base64::encode(claims.to_string().as_bytes())
        .unwrap()
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_owned();
    let token = format!("e30.{encoded}.fixture");
    std::fs::write(
        path,
        aterm_json::json!({"auth_mode":"chatgpt","tokens":{
            "id_token":token, "access_token":token,"refresh_token":"fixture"
        }})
        .to_string(),
    )
    .unwrap();
}

/// The receipt `id` (one hex digit, repeated) of `proposal`'s move for the session `s-one` at its
/// walled turn `wall`, as its prepare issues it: Ready, nothing launched.
fn root(
    proposal: &codex_rotation::Proposal,
    id: &str,
    wall: &str,
) -> crate::harness::codex_renewal::Receipt {
    use crate::harness::codex_renewal::{self as renewal, Intent, Phase, Purpose, Receipt};
    let next = proposal.successor().unwrap();
    Receipt {
        destination: Some(next.identity),
        id: id.repeat(32),
        intent: Intent {
            purpose: Purpose::WeeklyQuota,
            thread: "00000000-0000-0000-0000-000000000001".into(),
            wall: wall.into(),
            identity: proposal.source_identity.clone(),
            limit: None,
        },
        sid: "s-one".into(),
        old_pid: 10,
        old_birth: 1,
        claim_revision: 1,
        human_seq: 0,
        old_auth: next.old_auth,
        new_auth: next.new_auth,
        generation: next.generation,
        row_stamp: renewal::row_stamp(&next.row),
        native_prepare: "sealed native recipe".into(),
        expires_ms: u64::MAX,
        phase: Phase::Ready,
        successor_pid: None,
        successor_birth: None,
        accepted_turn: None,
        launch_attempted: false,
        candidate: None,
        current_attempt: None,
        revoked: false,
    }
}

/// The host's environment over `world`'s records, at `NOW`.
fn env_of(world: &World) -> crate::harness::cli::Env {
    let mut env = crate::harness::cli::Env::from_process().unwrap();
    env.home = Some(world.home.clone());
    env.state = world.state.clone();
    env.config = Some(world.cfg.join("aterm.toml"));
    env.now = NOW;
    env
}

/// `receipt` consumed: its successor launched and accepted the walled turn, and the pool's
/// deduplication record of the move retired, as consume leaves them.
fn consume(env: &crate::harness::cli::Env, receipt: &mut crate::harness::codex_renewal::Receipt) {
    use crate::harness::codex_renewal::{self as renewal, Phase};
    receipt.phase = Phase::Consumed;
    receipt.launch_attempted = true;
    receipt.accepted_turn = Some(receipt.intent.wall.clone());
    receipt.successor_pid = Some(11);
    receipt.successor_birth = Some(2);
    renewal::with_receipt_writer(env, |_| renewal::write_receipt(env, receipt))
        .unwrap()
        .unwrap();
    assert_eq!(renewal::retire_pool_record(env, receipt), Ok(Some(true)));
}

/// A PIN ON ANOTHER ROW HOLDS A HANDOFF BEFORE ANY PROVIDER READ: its writer takes no target under
/// a pin, so a session on `main` under a pin on `work` reads nothing and proposes nothing, however
/// often its control retries. A PIN ON THE SESSION'S OWN ROW ENDS AT ITS WALL: the session moves,
/// and the writer takes it. CONTROL: with no pin it moves.
#[test]
fn a_pin_on_another_row_holds_the_handoff_before_any_provider_read() {
    let pin = |world: &World, label: &str| {
        world
            .seat()
            .store(Cli::Codex)
            .transact(NOW, &mut None, |state| {
                state.active = Some(label.to_string());
                state.pinned = true;
                Ok(Vec::new())
            })
            .unwrap();
    };
    let source = Identity::from_native("source", "user");
    let (world, reader) = fixture("quota-pinned-elsewhere");
    pin(&world, "work");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    );
    assert!(matches!(proposal, Ok(None)), "{proposal:?}");
    assert!(reader.calls.borrow().is_empty(), "{:?}", reader.calls);
    assert!(world.load(Cli::Codex).pinned, "the owner's pin stands");

    let (world, reader) = fixture("quota-pinned-here");
    pin(&world, "main");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .expect("its own wall ends the pin");
    assert_eq!(proposal.destination.label, "work");
    assert!(!world.load(Cli::Codex).pinned, "the wall ended the pin");

    let (world, reader) = fixture("quota-unpinned");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(proposal.is_some(), "CONTROL");
}

/// A LOW ROW IS NO HANDOFF'S TARGET, AS ITS WRITER REFUSES ONE: with `work` low the session proposes
/// nothing and reads no usage of `work`, rather than selecting a row the writer would refuse.
/// CONTROL: with `work` at room it is selected.
#[test]
fn a_low_row_is_no_handoffs_target() {
    let source = Identity::from_native("source", "user");
    let (world, reader) = fixture("quota-low-target");
    world
        .seat()
        .store(Cli::Codex)
        .transact(NOW, &mut None, |state| {
            Ok(accounts::note_sample(
                state,
                "work",
                Sample {
                    session_pct: Some(95),
                    session_reset: Some(NOW + 7200),
                    at: NOW,
                    ..Sample::default()
                },
                NOW,
            ))
        })
        .unwrap();
    let work = world.work_dir(Cli::Codex).join("auth.json");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(proposal.is_none(), "{proposal:?}");
    assert!(!reader.calls.borrow().contains(&work), "{:?}", reader.calls);
    let (world, reader) = fixture("quota-room-target");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(
        proposal.map(|proposal| proposal.destination.label),
        Some("work".to_string()),
        "CONTROL"
    );
}

/// A WALL NO RECORDED ROW CAN TAKE IS READ ONCE, NOT ON EACH ASK: its control asks again every
/// few seconds, and nothing reads a provider on a timer. With `work` walled for the week, or low
/// (a row the roster's advice still offers at a wall, which the handoff refuses), a session at its
/// weekly wall proposes nothing; its first ask reads its own login, which records the wall, and the
/// asks after it read nothing, neither its login nor `work`. CONTROL: with `work` open it reads
/// both and moves.
#[test]
fn a_wall_no_recorded_row_can_take_is_read_once() {
    let source = Identity::from_native("source", "user");
    let walled = |state: &mut accounts::State| {
        accounts::note_wall(state, "work", "seven_day", Some(NOW + 3 * 86_400), NOW, NOW)
    };
    let low = |state: &mut accounts::State| {
        accounts::note_sample(
            state,
            "work",
            Sample {
                session_pct: Some(95),
                session_reset: Some(NOW + 7200),
                at: NOW,
                ..Sample::default()
            },
            NOW,
        )
    };
    type Work<'a> = &'a dyn Fn(&mut accounts::State) -> Vec<accounts::Event>;
    let cases: [(&str, Work<'_>); 2] = [("quota-no-row-open", &walled), ("quota-low-row", &low)];
    for (name, work) in cases {
        let (world, reader) = fixture(name);
        world
            .seat()
            .store(Cli::Codex)
            .transact(NOW, &mut None, |state| Ok(work(state)))
            .unwrap();
        for _ in 0..3 {
            let proposal = codex_rotation::prepare(
                &world.seat(),
                &weekly_session(&world),
                &source,
                &reader,
                Duration::from_secs(60),
            );
            assert!(matches!(proposal, Ok(None)), "{name}: {proposal:?}");
        }
        assert_eq!(
            *reader.calls.borrow(),
            vec![world.home.join(".codex/auth.json")],
            "{name}: its own login, once"
        );
    }

    let (world, reader) = fixture("quota-row-open");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(
        proposal.map(|proposal| proposal.destination.label),
        Some("work".to_string()),
        "CONTROL"
    );
    assert_eq!(
        reader.calls.borrow().len(),
        2,
        "CONTROL: {:?}",
        reader.calls
    );
}

/// A READ THAT ANSWERS NO FIGURE WAITS ITS RUNG, NOT THE NEXT ASK (ATERM-KI-1110-116): the client
/// asks every few seconds at its wall. With `work`'s read failing, the first ask reads both logins
/// and records the failure; the asks inside its minute read nothing, since `work` is no target
/// while it stands and the wall is on record; the first ask after the minute reads both again and
/// waits five minutes; a read that answers ends the run and moves the session. CONTROL: each first
/// ask after a wait reads. A FAILING READ OF THE SESSION'S OWN LOGIN WAITS ITS RUNG TOO: recorded
/// on its row though that login's context cannot be captured, it holds the asks inside its minute,
/// and the first after it reads that login again. A RUN ON THE SESSION'S OWN ROW IS NO WRITER'S
/// CONCERN: the writer still admits the row.
#[test]
fn a_read_that_answers_no_figure_waits_its_rung_not_the_next_ask() {
    let source = Identity::from_native("source", "user");
    let ask = |world: &World, reader: &Reader, at: i64| {
        let seat = Seat {
            now: at,
            ..world.seat()
        };
        codex_rotation::prepare(
            &seat,
            &weekly_session(world),
            &source,
            reader,
            Duration::from_secs(60),
        )
    };
    let unanswered = |world: &World, label: &str| world.load(Cli::Codex).accounts[label].unanswered;

    let (world, mut reader) = fixture("quota-unanswered-target");
    let main = main_of(&world);
    let work = world.work_dir(Cli::Codex).join("auth.json");
    native_auth(&work, "target");
    reader.answers.remove(&work);
    assert!(matches!(ask(&world, &reader, NOW), Ok(None)));
    assert_eq!(*reader.calls.borrow(), [main.clone(), work.clone()]);
    assert_eq!(
        unanswered(&world, "work"),
        Some(accounts::Unanswered {
            at: NOW,
            attempts: 1
        })
    );
    for at in [NOW + 5, NOW + 59] {
        assert!(matches!(ask(&world, &reader, at), Ok(None)), "{at}");
    }
    assert_eq!(reader.calls.borrow().len(), 2, "{:?}", reader.calls);
    assert!(matches!(ask(&world, &reader, NOW + 60), Ok(None)));
    assert_eq!(
        reader.calls.borrow().len(),
        4,
        "CONTROL: {:?}",
        reader.calls
    );
    assert!(matches!(ask(&world, &reader, NOW + 359), Ok(None)));
    assert_eq!(reader.calls.borrow().len(), 4, "{:?}", reader.calls);
    reader.answers = BTreeMap::from([
        (main.clone(), usage_read_at("source", true, NOW + 360)),
        (work, usage_read_at("target", false, NOW + 360)),
    ]);
    let proposal = ask(&world, &reader, NOW + 360).unwrap();
    assert_eq!(proposal.map(|p| p.destination.label), Some("work".into()));
    assert_eq!(unanswered(&world, "work"), None, "a figure ends the run");

    let (world, mut reader) = fixture("quota-unanswered-own-row");
    let main = main_of(&world);
    let answer = reader.answers.remove(&main).unwrap();
    assert_eq!(
        world
            .seat()
            .store(Cli::Codex)
            .capture_auth_context(&world.seat().bound_roster(Cli::Codex).unwrap().rows[0]),
        None,
        "the stand-in default login has no context"
    );
    assert!(matches!(ask(&world, &reader, NOW), Ok(None)));
    assert_eq!(*reader.calls.borrow(), std::slice::from_ref(&main));
    assert_eq!(
        unanswered(&world, "main"),
        Some(accounts::Unanswered {
            at: NOW,
            attempts: 1
        })
    );
    for at in [NOW + 5, NOW + 59] {
        assert!(matches!(ask(&world, &reader, at), Ok(None)), "{at}");
    }
    assert_eq!(reader.calls.borrow().len(), 1, "{:?}", reader.calls);
    let state = world.load(Cli::Codex);
    let roster = world.seat().bound_roster(Cli::Codex).unwrap();
    assert!(super::super::native_target_admitted(
        roster.row("main").unwrap(),
        &state,
        NOW + 5
    ));
    reader.answers.insert(main.clone(), answer);
    let proposal = ask(&world, &reader, NOW + 60).unwrap();
    assert_eq!(proposal.map(|p| p.destination.label), Some("work".into()));
    assert_eq!(
        reader.calls.borrow()[1],
        main,
        "CONTROL: {:?}",
        reader.calls
    );
}

/// A LOGIN A RENEWAL REPLACED SAYS NOTHING OF ITS ROW: a session still on the default login its
/// row `main` moved away from reads that login at its wall, and its failed read is not recorded on
/// `main`, whose own login it never read. CONTROL: the same failed read where `main` still holds
/// that login is recorded on it.
#[test]
fn a_failed_read_of_a_replaced_login_is_not_its_rows() {
    for (name, replaced) in [("quota-replaced-own", true), ("quota-held-own", false)] {
        let (world, mut reader) = fixture(name);
        let main = main_of(&world);
        reader.answers.remove(&main);
        if replaced {
            // No more capturable than the login the session holds: what tells the two apart is
            // which file was read, not the context the record compares.
            let moved = world.root.join("logins/codex/main-renewed");
            std::fs::create_dir_all(&moved).unwrap();
            std::fs::write(moved.join("auth.json"), "{}").unwrap();
            world
                .seat()
                .store(Cli::Codex)
                .transact_logins(NOW, &mut None, |_, logins| {
                    logins.labels.insert(
                        "main".to_string(),
                        accounts::Binding {
                            dir: Some(moved.display().to_string()),
                            was: vec![None],
                            ..accounts::Binding::default()
                        },
                    );
                    Ok(Vec::new())
                })
                .unwrap();
        }
        let proposal = codex_rotation::prepare(
            &world.seat(),
            &weekly_session(&world),
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
        );
        assert!(matches!(proposal, Ok(None)), "{name}: {proposal:?}");
        assert_eq!(
            *reader.calls.borrow(),
            std::slice::from_ref(&main),
            "{name}: the session's own login, once"
        );
        let unanswered = world
            .load(Cli::Codex)
            .accounts
            .get("main")
            .and_then(|account| account.unanswered);
        if replaced {
            assert_eq!(unanswered, None, "{name}");
        } else {
            assert!(unanswered.is_some(), "CONTROL {name}");
        }
    }
}

/// A HANDOFF'S READS LEAVE ROOM TO RECORD THEM: each read is given the ask's deadline less
/// [`codex_rotation::RECORD_WITHIN`], so a read cut short at its deadline is still recorded under
/// the store's lock ladder. CONTROL: the record of that read is there.
#[test]
fn a_handoffs_reads_leave_room_to_record_them() {
    struct Waits(RefCell<Vec<Duration>>);
    impl UsageReader for Waits {
        fn read(
            &self,
            _: &Path,
            _: Option<&str>,
            _: i64,
            wait: Duration,
            _: crate::harness::provider_read::Use,
        ) -> IdentifiedRead {
            self.0.borrow_mut().push(wait);
            IdentifiedRead::Unread
        }
    }
    let (world, _) = fixture("quota-record-room");
    let reader = Waits(RefCell::new(Vec::new()));
    let wait = Duration::from_secs(2);
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &Identity::from_native("source", "user"),
        &reader,
        wait,
    );
    assert!(matches!(proposal, Ok(None)), "{proposal:?}");
    let waits = reader.0.borrow();
    assert_eq!(waits.len(), 1, "{waits:?}");
    assert!(
        waits[0] <= wait - codex_rotation::RECORD_WITHIN,
        "{waits:?}"
    );
    assert!(
        world.load(Cli::Codex).accounts["main"].unanswered.is_some(),
        "CONTROL"
    );
}

/// A RUN ENDS WITH THE LOGIN IT WAS OF, AND NO MERGE BRINGS ONE BACK PAST A NEWER FIGURE: a
/// published sign-in of the row clears its run, and so does another credential published for it;
/// [`accounts::fill_observations`] takes a run another record held only where no figure it now holds
/// is as new. CONTROL: an older figure leaves the merged run.
#[test]
fn a_run_ends_with_its_login_and_no_merge_brings_it_back() {
    let run = accounts::Unanswered {
        at: NOW,
        attempts: 3,
    };
    let figure = |at| Sample {
        session_pct: Some(10),
        session_reset: Some(NOW + 3600),
        at,
        ..Sample::default()
    };
    for (sample, kept) in [
        (Some(figure(NOW + 1)), None),
        (Some(figure(NOW - 1)), Some(run)),
        (None, Some(run)),
    ] {
        let mut into = accounts::Account {
            sample,
            ..accounts::Account::default()
        };
        let from = accounts::Account {
            unanswered: Some(run),
            ..accounts::Account::default()
        };
        accounts::fill_observations(&mut into, from);
        assert_eq!(into.unanswered, kept, "{:?}", into.sample);
    }
}

/// A RUN COUNTS ONLY ITS OWN EPISODE AND YIELDS TO A NEWER FIGURE: a failed read
/// `UNANSWERED_RUN_S` after the run's newest begins a new run rather than taking up its waits, and
/// a failed read begun before a figure noted of the row since leaves the row as that figure says.
#[test]
fn an_unanswered_run_restarts_after_its_episode_and_yields_to_a_newer_figure() {
    let mut state = accounts::State::default();
    let attempts = |state: &accounts::State| state.accounts["work"].unanswered.map(|u| u.attempts);
    accounts::note_unanswered(&mut state, "work", NOW);
    accounts::note_unanswered(&mut state, "work", NOW + 60);
    assert_eq!(attempts(&state), Some(2));
    let later = NOW + 60 + accounts::UNANSWERED_RUN_S;
    accounts::note_unanswered(&mut state, "work", later);
    assert_eq!(attempts(&state), Some(1), "a later wall's run starts over");

    let figure = Sample {
        session_pct: Some(10),
        session_reset: Some(later + 3600),
        at: later + 30,
        ..Sample::default()
    };
    let _ = accounts::note_sample(&mut state, "work", figure, later + 30);
    assert_eq!(attempts(&state), None, "a figure ends the run");
    accounts::note_unanswered(&mut state, "work", later + 10);
    assert_eq!(
        attempts(&state),
        None,
        "an older failure leaves the newer figure"
    );
    accounts::note_unanswered(&mut state, "work", later + 31);
    assert_eq!(attempts(&state), Some(1), "CONTROL");
}

fn main_of(world: &World) -> PathBuf {
    world.home.join(".codex/auth.json")
}

#[test]
fn selected_quota_guard_requires_pool_commit_both_identities_and_current_policy() {
    use crate::harness::codex_renewal::{self as renewal, Intent, Phase, Purpose, Receipt};
    for changed in 0..7 {
        let (world, reader) = fixture("quota-selected-guard");
        if changed == 6 {
            let roster = accounts::Roster::load_for(
                Cli::Codex,
                &world.cfg.join(Cli::Codex.accounts().roster_file),
            )
            .unwrap();
            let work = PathBuf::from(roster.row("work").unwrap().dir.as_ref().unwrap());
            for home in [world.home.join(".codex"), work] {
                std::fs::create_dir_all(&home).unwrap();
                std::fs::write(
                    home.join("config.toml"),
                    "cli_auth_credentials_store='file'\n",
                )
                .unwrap();
            }
            world
                .seat()
                .store(Cli::Codex)
                .transact(NOW, &mut None, |state| {
                    Ok(accounts::note_auth(
                        state,
                        "work",
                        accounts::Auth::SignedOut,
                        NOW,
                    ))
                })
                .unwrap();
        }
        let mut env = crate::harness::cli::Env::from_process().unwrap();
        env.home = Some(world.home.clone());
        env.state = world.state.clone();
        env.config = Some(world.cfg.join("aterm.toml"));
        env.now = NOW;
        let proposal = codex_rotation::prepare(
            &world.seat(),
            &weekly_session(&world),
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
        )
        .unwrap()
        .unwrap();
        native_auth(&proposal.old_auth, "source");
        native_auth(&proposal.new_auth, "target");
        let next = proposal.successor().unwrap();
        let mut receipt = Receipt {
            destination: Some(next.identity.clone()),
            id: "a".repeat(32),
            intent: Intent {
                purpose: Purpose::WeeklyQuota,
                thread: "00000000-0000-0000-0000-000000000001".into(),
                wall: "00000000-0000-0000-0000-000000000002".into(),
                identity: proposal.source_identity.clone(),
                limit: None,
            },
            sid: "s-one".into(),
            old_pid: 10,
            old_birth: 1,
            claim_revision: 1,
            human_seq: 0,
            old_auth: next.old_auth.clone(),
            new_auth: next.new_auth.clone(),
            generation: next.generation,
            row_stamp: renewal::row_stamp(&next.row),
            native_prepare: "sealed native recipe".into(),
            expires_ms: u64::MAX,
            phase: Phase::Ready,
            successor_pid: None,
            successor_birth: None,
            accepted_turn: None,
            launch_attempted: false,
            candidate: None,
            current_attempt: None,
            revoked: false,
        };
        assert_eq!(
            renewal::with_selected(&env, &receipt, |_| Ok(true)),
            Err("pool-commit-missing")
        );
        assert_eq!(
            codex_rotation::with_prepared_writer(&env, &receipt, |_, _| Ok(true)),
            Ok(Some(true))
        );
        assert!(world.load(Cli::Codex).rotations.is_empty());
        for invalid in 0..3 {
            let mut attempted = receipt.clone();
            match invalid {
                0 => attempted.phase = Phase::Committing,
                1 => attempted.launch_attempted = true,
                _ => attempted.accepted_turn = Some("accepted-turn".into()),
            }
            assert_eq!(
                codex_rotation::with_prepared_writer(&env, &attempted, |_, _| {
                    panic!("invalid preparation reached the receipt writer")
                }),
                Err::<Option<()>, _>("not-ready")
            );
        }
        proposal.commit(&receipt, NOW).unwrap();
        assert_eq!(
            renewal::with_selected(&env, &receipt, |_| Ok(true)),
            Ok(Some(true))
        );
        let mut rollback = receipt.clone();
        rollback.phase = Phase::Committing;
        rollback.launch_attempted = true;
        renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &rollback)).unwrap();
        assert_eq!(
            codex_rotation::with_rollback_writer(&env, &rollback, |_| Ok(true)),
            Ok(Some(true))
        );
        match changed {
            0 => native_auth(&proposal.new_auth, "other-target"),
            1 => native_auth(&proposal.old_auth, "other-source"),
            2 => {
                receipt.native_prepare.push_str(" changed");
            }
            3 => {
                world
                    .seat()
                    .store(Cli::Codex)
                    .transact_logins(NOW, &mut None, |state, _| {
                        state.pinned = true;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            4 => std::fs::remove_file(&proposal.new_auth).unwrap(),
            6 => {
                let store = world.seat().store(Cli::Codex);
                let records = store
                    .read_account_records(
                        Some(&world.cfg.join(Cli::Codex.accounts().roster_file)),
                        NOW,
                    )
                    .unwrap();
                assert_eq!(
                    records.state.account("work").auth,
                    accounts::Auth::SignedOut
                );
                let generation = records
                    .logins
                    .get("work")
                    .map(|binding| binding.renewal_generation);
                let raw = records.raw_roster.row("work").unwrap();
                let context = store
                    .capture_auth_context(records.roster.row("work").unwrap())
                    .unwrap();
                let events = store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        accounts::stage_auth(
                            state,
                            raw,
                            logins,
                            context,
                            accounts::Auth::SignedOut,
                            NOW,
                        );
                        Ok(Vec::new())
                    })
                    .unwrap();
                assert!(
                    events
                        .iter()
                        .all(|event| event.kind != accounts::Kind::Auth)
                );
                assert_eq!(
                    store
                        .load_logins(NOW)
                        .unwrap()
                        .0
                        .get("work")
                        .map(|binding| binding.renewal_generation),
                    generation
                );
                assert!(
                    proposal
                        .plan
                        .clone()
                        .commit_native(
                            NOW,
                            &receipt.id,
                            &codex_rotation::receipt_binding(&receipt).unwrap()
                        )
                        .line
                        .is_none()
                );
            }
            _ => {
                world
                    .seat()
                    .store(Cli::Codex)
                    .transact(NOW, &mut None, |state| {
                        Ok(accounts::note_wall(
                            state,
                            "work",
                            "seven_day",
                            Some(NOW + 86_400),
                            NOW,
                            NOW,
                        ))
                    })
                    .unwrap();
            }
        }
        assert!(renewal::with_selected(&env, &receipt, |_| Ok(true)).is_err());
        assert!(codex_rotation::with_prepared_writer(&env, &receipt, |_, _| Ok(true)).is_err());
        // Failed/replaced/missing destination credentials and destination quota
        // do not authorize Forward, but cannot block the sealed compensation.
        // Source identity, immutable receipt and owner pin changes still refuse it.
        rollback.native_prepare = receipt.native_prepare.clone();
        let restoration = codex_rotation::with_rollback_writer(&env, &rollback, |_| Ok(true));
        if matches!(changed, 0 | 4 | 5 | 6) {
            assert_eq!(restoration, Ok(Some(true)));
        } else {
            assert!(restoration.is_err());
        }
    }
}

#[test]
fn terminal_quota_receipts_retire_only_their_exact_pool_binding() {
    use crate::harness::codex_renewal::{self as renewal, Intent, Phase, Purpose, Receipt};
    for (phase, attempted) in [
        (Phase::Ready, false),
        (Phase::Committing, true),
        (Phase::Retired, false),
        (Phase::Retired, true),
        (Phase::Consumed, true),
    ] {
        let (world, reader) = fixture("quota-terminal-receipt");
        let mut env = crate::harness::cli::Env::from_process().unwrap();
        env.home = Some(world.home.clone());
        env.state = world.state.clone();
        env.config = Some(world.cfg.join("aterm.toml"));
        env.now = NOW;
        let proposal = codex_rotation::prepare(
            &world.seat(),
            &weekly_session(&world),
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
        )
        .unwrap()
        .unwrap();
        let next = proposal.successor().unwrap();
        let mut receipt = Receipt {
            destination: Some(next.identity.clone()),
            id: "a".repeat(32),
            intent: Intent {
                purpose: Purpose::WeeklyQuota,
                thread: "00000000-0000-0000-0000-000000000001".into(),
                wall: "00000000-0000-0000-0000-000000000002".into(),
                identity: proposal.source_identity.clone(),
                limit: None,
            },
            sid: "s-one".into(),
            old_pid: 10,
            old_birth: 1,
            claim_revision: 1,
            human_seq: 0,
            old_auth: next.old_auth,
            new_auth: next.new_auth,
            generation: next.generation,
            row_stamp: renewal::row_stamp(&next.row),
            native_prepare: "sealed native recipe".into(),
            expires_ms: u64::MAX,
            phase: Phase::Ready,
            successor_pid: None,
            successor_birth: None,
            accepted_turn: None,
            launch_attempted: false,
            candidate: None,
            current_attempt: None,
            revoked: false,
        };
        proposal.commit(&receipt, NOW).unwrap();
        native_auth(&receipt.old_auth, "source");
        native_auth(&receipt.new_auth, "target");
        receipt.phase = phase.clone();
        receipt.launch_attempted = attempted;
        if phase == Phase::Consumed {
            receipt.accepted_turn = Some(receipt.intent.wall.clone());
            receipt.launch_attempted = true;
        }
        renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &receipt)).unwrap();
        if phase != Phase::Consumed {
            assert!(renewal::record_completed_rotation(&env, &receipt).is_err());
        }
        let allowed = phase == Phase::Consumed || (phase == Phase::Retired && !attempted);
        let mut wrong = receipt.clone();
        wrong.native_prepare.push_str(" changed");
        assert!(renewal::retire_pool_record(&env, &wrong).is_err());
        if allowed {
            assert_eq!(renewal::retire_pool_record(&env, &receipt), Ok(Some(true)));
            assert_eq!(renewal::retire_pool_record(&env, &receipt), Ok(Some(false)));
            assert!(
                world
                    .seat()
                    .store(Cli::Codex)
                    .load_logins(NOW)
                    .unwrap()
                    .0
                    .native_rotations
                    .is_empty()
            );
            assert_eq!(world.load(Cli::Codex).rotations, [NOW]);
            if phase == Phase::Consumed {
                assert!(
                    !world.load(Cli::Codex).proven,
                    "consumption is not completion"
                );
                assert!(renewal::record_completed_rotation(&env, &wrong).is_err());
                native_auth(&receipt.new_auth, "changed-target");
                assert_eq!(
                    renewal::record_completed_rotation(&env, &receipt),
                    Err(renewal::Unrecorded::Refused("identity-mismatch"))
                );
                assert!(!world.load(Cli::Codex).proven);
                native_auth(&receipt.new_auth, "target");
                let active = world.load(Cli::Codex).active;
                let store = world.seat().store(Cli::Codex);
                store
                    .transact_logins(NOW, &mut None, |state, _| {
                        state.active = Some("main".into());
                        Ok(Vec::new())
                    })
                    .unwrap();
                assert_eq!(
                    codex_rotation::with_consumed_member_writer(&env, &receipt, |_, _| Ok(true)),
                    Ok(Some(true))
                );
                native_auth(&receipt.new_auth, "changed-target");
                assert!(
                    codex_rotation::with_consumed_member_writer(&env, &receipt, |_, _| Ok(true))
                        .is_err()
                );
                native_auth(&receipt.new_auth, "target");
                store
                    .transact_logins(NOW, &mut None, |state, _| {
                        state.active = active.clone();
                        Ok(Vec::new())
                    })
                    .unwrap();
                assert_eq!(renewal::record_completed_rotation(&env, &receipt), Ok(()));
                assert!(world.load(Cli::Codex).proven);
                assert!(world.load(Cli::Codex).canary.is_none());
                assert_eq!(renewal::record_completed_rotation(&env, &receipt), Ok(()));
            }
        } else {
            assert!(renewal::retire_pool_record(&env, &receipt).is_err());
            assert_eq!(
                world
                    .seat()
                    .store(Cli::Codex)
                    .load_logins(NOW)
                    .unwrap()
                    .0
                    .native_rotations
                    .len(),
                1
            );
        }
        if phase == Phase::Ready {
            let mut changed = receipt.clone();
            changed.native_prepare.push_str(" changed");
            assert!(renewal::cancel_unexecuted_rotation(&env, &changed, "work").is_err());
            let mut executed = receipt.clone();
            executed.launch_attempted = true;
            assert!(renewal::cancel_unexecuted_rotation(&env, &executed, "work").is_err());
            let retired = renewal::cancel_unexecuted_rotation(&env, &receipt, "work")
                .unwrap()
                .unwrap();
            assert_eq!(retired.phase, Phase::Retired);
            assert!(retired.revoked);
            assert_eq!(world.load(Cli::Codex).active.as_deref(), Some("main"));
            assert!(world.load(Cli::Codex).canary.is_none());
            assert!(
                world
                    .seat()
                    .store(Cli::Codex)
                    .load_logins(NOW)
                    .unwrap()
                    .0
                    .native_rotations
                    .is_empty()
            );
            assert_eq!(
                renewal::cancel_unexecuted_rotation(&env, &receipt, "work"),
                Ok(Some(retired.clone()))
            );
            assert_eq!(
                renewal::cancel_unexecuted_rotation(&env, &retired, "work"),
                Ok(Some(retired))
            );
        }
    }
}

#[test]
fn quota_proposal_binds_fresh_identities_and_defers_the_pool_commit() {
    let (world, reader) = fixture("quota-proposal");
    let s = weekly_session(&world);
    let expected = Identity::from_native("source", "user");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &s,
        &expected,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .expect("quota proposal");
    assert_eq!(proposal.source.label, "main");
    assert_eq!(proposal.destination.label, "work");
    assert_eq!(proposal.source_identity, expected);
    assert_eq!(
        proposal.destination_identity,
        Identity::from_native("target", "user")
    );
    assert_eq!(
        &*reader.calls.borrow(),
        &[proposal.old_auth.clone(), proposal.new_auth.clone()]
    );
    assert!(world.load(Cli::Codex).rotations.is_empty());
    assert!(proposal.plan.commit(NOW).line.is_some());
    assert_eq!(world.load(Cli::Codex).rotations, [NOW]);
}

#[test]
fn native_quota_commit_retries_charge_once_and_refuse_a_different_binding() {
    let (world, reader) = fixture("quota-commit-retry");
    let s = weekly_session(&world);
    let expected = Identity::from_native("source", "user");
    let prepare = || {
        codex_rotation::prepare(
            &world.seat(),
            &s,
            &expected,
            &reader,
            Duration::from_secs(60),
        )
        .unwrap()
        .unwrap()
        .plan
    };
    let first = prepare();
    let retry = prepare();
    let conflicting = prepare();
    let id = "a".repeat(32);
    let binding = "b".repeat(64);
    assert!(first.commit_native(NOW, &id, &binding).line.is_some());
    assert!(
        conflicting
            .commit_native(NOW, &id, &"c".repeat(64))
            .line
            .is_none()
    );
    assert!(retry.commit_native(NOW, &id, &binding).line.is_some());
    assert_eq!(world.load(Cli::Codex).rotations, [NOW]);
}

/// A session prepared while another's move to the same row was still its canary follows that row
/// once the move's completion is recorded: the first move's successor reported its walled turn
/// served ([`renewal::record_completed_rotation`]), which proves `work`. The follow charges no
/// second rotation, and the pool's deduplication records hold the follower's binding alone, the
/// first move's having been retired at its consume.
#[test]
fn concurrent_native_plans_follow_the_proven_target_without_another_rotation() {
    use crate::harness::codex_renewal as renewal;
    let (world, reader) = fixture("quota-concurrent-follow");
    let first_session = weekly_session(&world);
    let mut next_session = weekly_session(&world);
    next_session.sid = "next-native-session";
    let expected = Identity::from_native("source", "user");
    let prepare = |session: &Session<'_>| {
        codex_rotation::prepare(
            &world.seat(),
            session,
            &expected,
            &reader,
            Duration::from_secs(60),
        )
        .unwrap()
        .unwrap()
    };
    let first = prepare(&first_session);
    let next = prepare(&next_session);
    let mut moved = root(&first, "a", "00000000-0000-0000-0000-000000000002");
    let mut following = root(&next, "c", "00000000-0000-0000-0000-000000000003");
    following.sid = "next-native-session".into();
    first.commit(&moved, NOW).unwrap();
    assert_eq!(
        next.commit(&following, NOW),
        Err("pool-commit-refused"),
        "a pending canary still owns admission"
    );
    native_auth(&moved.old_auth, "source");
    native_auth(&moved.new_auth, "target");
    let env = env_of(&world);
    consume(&env, &mut moved);
    assert_eq!(renewal::record_completed_rotation(&env, &moved), Ok(()));
    assert_eq!(
        next.commit(&following, NOW),
        Ok(()),
        "a successful canary lets the already-prepared session follow the same target"
    );
    assert_eq!(world.load(Cli::Codex).rotations, [NOW]);
    let logins = world.seat().store(Cli::Codex).load_logins(NOW).unwrap().0;
    assert_eq!(
        logins.native_rotations.keys().collect::<Vec<_>>(),
        [&following.id]
    );
}

#[test]
fn native_quota_retry_cannot_override_a_later_selection_or_unusable_target() {
    for change in 0..4 {
        let (world, reader) = fixture("quota-retry-revoked");
        let s = weekly_session(&world);
        let expected = Identity::from_native("source", "user");
        let prepare = || {
            codex_rotation::prepare(
                &world.seat(),
                &s,
                &expected,
                &reader,
                Duration::from_secs(60),
            )
            .unwrap()
            .unwrap()
            .plan
        };
        let first = prepare();
        let retry = prepare();
        let id = "a".repeat(32);
        let binding = "b".repeat(64);
        assert!(first.commit_native(NOW, &id, &binding).line.is_some());
        let store = world.seat().store(Cli::Codex);
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                match change {
                    0 => state.pinned = true,
                    1 => state.active = Some("main".into()),
                    2 => {
                        return Ok(stage_current_auth(
                            &world,
                            Cli::Codex,
                            state,
                            logins,
                            "work",
                            accounts::Auth::SignedOut,
                            NOW,
                        ));
                    }
                    _ => {
                        return Ok(accounts::note_wall(
                            state,
                            "work",
                            "seven_day",
                            Some(NOW + 24 * HOUR),
                            NOW,
                            NOW,
                        ));
                    }
                }
                Ok(Vec::new())
            })
            .unwrap();
        let after_change = world.load(Cli::Codex);
        assert!(retry.commit_native(NOW, &id, &binding).line.is_none());
        assert_eq!(world.load(Cli::Codex), after_change);
        assert_eq!(after_change.rotations, [NOW]);
    }
}

#[test]
fn quota_proposal_refuses_wrong_source_restored_quota_and_unusable_destinations() {
    for case in 0..4 {
        let (world, mut reader) = fixture("quota-proposal-refused");
        let source = world.home.join(".codex/auth.json");
        let work = world.work_dir(Cli::Codex).join("auth.json");
        match case {
            0 => {
                reader
                    .answers
                    .insert(source, usage("replaced-source", true));
            }
            1 => {
                reader.answers.insert(source, usage("source", false));
            }
            2 => {
                reader.answers.insert(work, usage("target", true));
            }
            _ => {
                reader.answers.insert(work, usage("source", false));
            }
        }
        let s = weekly_session(&world);
        let result = codex_rotation::prepare(
            &world.seat(),
            &s,
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
        );
        if case == 1 {
            assert_eq!(result.as_ref().err().copied(), Some("not-walled"));
        }
        assert!(result.is_err() || result.is_ok_and(|proposal| proposal.is_none()));
        assert!(world.load(Cli::Codex).rotations.is_empty());
    }
}

#[test]
fn disabled_rotation_and_an_expired_budget_make_no_provider_requests() {
    for disabled in [false, true] {
        let (world, reader) = fixture("quota-proposal-disabled");
        if disabled {
            aterm_uds::atomic_file::write(
                &(world.cfg.join("aterm.toml")),
                ("[harness]\naccounts = 'off'\n").as_ref(),
                0o600,
            )
            .unwrap();
        }
        let s = weekly_session(&world);
        let result = codex_rotation::prepare(
            &world.seat(),
            &s,
            &Identity::from_native("source", "user"),
            &reader,
            if disabled {
                Duration::from_secs(60)
            } else {
                Duration::ZERO
            },
        );
        assert!(result.unwrap().is_none());
        assert!(reader.calls.borrow().is_empty());
    }
}

#[test]
#[cfg(unix)]
fn native_exec_proposal_uses_fresh_shared_account_reads_and_typed_dispatch() {
    use crate::harness::codex_exec_handoff::ExecCommand;
    let (world, reader) = fixture("exec-quota-proposal");
    let request = weekly_session(&world);
    let program = std::env::current_exe().unwrap();
    let image = crate::harness::native_act::Image::at(&program).unwrap();
    let source = Identity::from_native("source", "user");
    let request = Session {
        cli: request.cli,
        sid: request.sid,
        supervisor: request.supervisor,
        env: request.env,
        seen: &crate::harness::observe::Observation::Unread,
        sample: request.sample,
        point: request.point,
        mode: request.mode,
        relaunch: (),
    };
    let proposal = codex_rotation::prepare_exec(
        &world.seat(),
        &request,
        &world.exe,
        &source,
        &reader,
        Duration::from_secs(60),
        |row| {
            Some(ExecCommand {
                argv0: "codex".into(),
                image: image.clone(),
                program: program.to_str().unwrap().into(),
                argv: vec!["exec".into(), "--json".into(), "original\ninput".into()],
                auth_path: row
                    .dir
                    .as_ref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| world.home.join(".codex"))
                    .join("auth.json")
                    .to_str()
                    .unwrap()
                    .into(),
                cwd: None,
            })
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(proposal.source_identity, source);
    assert_eq!(
        proposal.destination_identity,
        Identity::from_native("target", "user")
    );
    assert_eq!(
        &*reader.calls.borrow(),
        &[proposal.old_auth.clone(), proposal.new_auth.clone()]
    );
    assert!(world.load(Cli::Codex).rotations.is_empty());
    let selected = proposal
        .plan
        .commit_native(NOW, &"d".repeat(32), &"e".repeat(64))
        .line
        .expect("native account selection");
    assert_eq!(Path::new(&selected.auth_path), proposal.new_auth);
    assert_eq!(selected.argv, ["exec", "--json", "original\ninput"]);
    assert_eq!(world.load(Cli::Codex).rotations, [NOW]);
}

/// A provider's read with `session` and `week` percent spent.
fn usage_at(account: &str, session: u64, week: u64) -> IdentifiedUsage {
    IdentifiedUsage {
        identity: Identity::from_native(account, "user"),
        usage: crate::harness::codex_app_login::usage_from_backend(
            &aterm_json::json!({
                "plan_type": "pro",
                "rate_limit": {
                    "allowed": week < 100,
                    "primary_window": {"used_percent": session, "limit_window_seconds": 18000,
                        "reset_at": NOW + 3600},
                    "secondary_window": {"used_percent": week,
                        "limit_window_seconds": 604800, "reset_at": NOW + 86_400}
                }
            }),
            NOW,
        )
        .expect("real provider percentages"),
    }
}

/// A FRESH READ THAT FINDS A ROW LOW SELECTS NOTHING, AND THE READ IS ON RECORD: a writer would
/// refuse that row, and the report judges it by what was just read. CONTROL: with room it moves.
#[test]
fn a_fresh_read_that_finds_a_row_low_selects_nothing_and_stays_on_record() {
    let source = Identity::from_native("source", "user");
    let (world, mut reader) = fixture("quota-fresh-low");
    let work = world.work_dir(Cli::Codex).join("auth.json");
    native_auth(&work, "target");
    reader.answers.insert(work, usage_at("target", 95, 0));
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(proposal.is_none(), "{proposal:?}");
    let state = world.load(Cli::Codex);
    assert_eq!(
        state.accounts["work"]
            .sample
            .as_ref()
            .and_then(|s| s.session_pct),
        Some(95),
        "the read is on record"
    );
}

/// A [`Reader`] whose provider refuses one login for good: every other login reads as `Reader`'s.
struct Refusing<'a> {
    reader: &'a Reader,
    refused: PathBuf,
}

impl UsageReader for Refusing<'_> {
    fn read(
        &self,
        path: &Path,
        email: Option<&str>,
        now: i64,
        wait: Duration,
        use_token: crate::harness::provider_read::Use,
    ) -> IdentifiedRead {
        if path == self.refused {
            return IdentifiedRead::SignedOut;
        }
        self.reader.read(path, email, now, wait, use_token)
    }
}

/// A CANDIDATE WHOSE LOGIN READS SIGNED OUT IS PASSED OVER ON THE RECORD: the handoff proposes
/// nothing onto it, its row says signed out under the credential that was read, the ledger
/// holds that once, and no later pick offers the row.
/// CONTROL: the same candidate read as nothing at all (its answer missing) keeps the word it had.
#[test]
fn a_candidate_whose_login_reads_signed_out_is_recorded_signed_out() {
    let source = Identity::from_native("source", "user");
    for refused in [true, false] {
        let (world, mut reader) = fixture("quota-signed-out");
        let work = world.work_dir(Cli::Codex).join("auth.json");
        native_auth(&work, "target");
        if !refused {
            reader.answers.remove(&work);
        }
        let refusing = Refusing {
            reader: &reader,
            refused: if refused {
                work.clone()
            } else {
                PathBuf::new()
            },
        };
        let proposal = codex_rotation::prepare(
            &world.seat(),
            &weekly_session(&world),
            &source,
            &refusing,
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(proposal.is_none(), "refused={refused}: {proposal:?}");
        let roster = world.seat().bound_roster(Cli::Codex).unwrap();
        let row = roster.row("work").unwrap();
        let mut state = world.load(Cli::Codex);
        world
            .seat()
            .store(Cli::Codex)
            .hydrate_auth_contexts(&mut state, &roster);
        let word = state.account("work").current_auth(row);
        let signed_out = world
            .seat()
            .store(Cli::Codex)
            .tail(64)
            .unwrap()
            .into_iter()
            .filter(|event| {
                event.kind == accounts::Kind::Auth
                    && event.account == "work"
                    && event.why == "signed-out"
            })
            .count();
        if refused {
            assert_eq!(word, accounts::Auth::SignedOut);
            assert_eq!(signed_out, 1);
            assert_eq!(
                condition_of(&world, Cli::Codex, "work"),
                Condition::SignedOut
            );
        } else {
            assert_ne!(word, accounts::Auth::SignedOut, "CONTROL: nothing read");
            assert_eq!(signed_out, 0, "CONTROL: nothing written");
        }
    }
}

/// A WALL BESIDE A READ AT THAT LIMIT ENDS AT THE READ'S RESET, AND A LATER WALL BESIDE A READ SHORT
/// OF IT KEEPS THAT RESET, through the writer a session's plan and a handoff's prepare both reach. A
/// read short of a limit says nothing of when that limit's wall ends: the planning model's case 9.
#[test]
fn a_wall_takes_the_reset_of_a_read_at_that_limit() {
    let walls_after = |reads: &[(Sample, &str)]| {
        let world = World::new("wall-reset");
        world.two(Cli::Codex);
        world.on_main(Cli::Codex, &[]);
        let exe = crate::harness::codex_alab::lay_package(&world.root.join("store/tried"), true);
        for (sample, window) in reads {
            let mut session = session(Cli::Codex, Supervisor::Aterm, Mode::Early, &exe);
            session.sample = Some(sample.clone());
            session.point = Point::Wall {
                window: (*window).into(),
                at: Some(NOW),
            };
            let _ = session_plan(&world.seat(), &session, |_| {});
        }
        let state = world.load(Cli::Codex);
        let window = reads.last().expect("a read").1;
        state.accounts["main"].walls[window].until
    };
    let at_limit = Sample {
        week_pct: Some(100),
        week_reset: Some(NOW + 86_400),
        at: NOW,
        ..Sample::default()
    };
    let short = Sample {
        session_pct: Some(95),
        session_reset: Some(NOW + 3600),
        at: NOW,
        ..Sample::default()
    };
    assert_eq!(
        walls_after(&[(at_limit.clone(), "seven_day")]),
        Some(NOW + 86_400)
    );
    assert_eq!(walls_after(&[(short.clone(), "five_hour")]), None);
    assert_eq!(
        walls_after(&[(at_limit, "seven_day"), (short, "seven_day")]),
        Some(NOW + 86_400)
    );
}

/// A HANDOFF'S PREPARE, AFTER THE READ A LAUNCH MADE, KEEPS THE WEEKLY RESET THAT READ NAMED.
#[test]
fn a_prepare_keeps_the_reset_a_launch_read_named() {
    let source = Identity::from_native("source", "user");
    let (world, reader) = fixture("prepare-keeps-reset");
    let main = world.seat().store(Cli::Codex);
    main.transact(NOW, &mut None, |state| {
        let read = Sample {
            week_pct: Some(100),
            week_reset: Some(NOW + 86_400),
            at: NOW,
            ..Sample::default()
        };
        Ok(accounts::note_sample(state, "main", read, NOW))
    })
    .unwrap();
    codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .expect("a proposal");
    let state = world.load(Cli::Codex);
    let until = state.accounts["main"]
        .sample
        .as_ref()
        .and_then(|s| s.week_reset);
    assert_eq!(until, Some(NOW + 86_400));
}

/// A LOGIN NO ROW HANDS OFF IS A REFUSAL THAT STANDS, HOWEVER OFTEN IT IS ASKED, and so is a login
/// whose read shows no wall a handoff leaves; a spent budget or a lock is one to ask about again.
#[test]
fn a_session_no_row_hands_off_stands_and_a_busy_one_is_asked_again() {
    use codex_rotation::{NoHandoff, prepare_refusal_stands};
    for why in ["source-unbound", "source-missing", "source-unmanaged"] {
        assert_eq!(prepare_refusal_stands(why), Some(NoHandoff::NoRowOfThePool));
    }
    assert_eq!(
        prepare_refusal_stands("not-walled"),
        Some(NoHandoff::NoWallToLeave)
    );
    assert_eq!(prepare_refusal_stands("roster-busy"), None);
}

/// A provider's read at `at`: `session` and `week` percent spent, the five-hour window resetting
/// at `session_reset` (`None`: the provider names no reset).
fn usage_resetting(
    account: &str,
    (session, week): (u64, u64),
    session_reset: Option<i64>,
    at: i64,
) -> IdentifiedUsage {
    let primary = match session_reset {
        Some(reset) => aterm_json::json!({"used_percent": session,
            "limit_window_seconds": 18000, "reset_at": reset}),
        None => aterm_json::json!({"used_percent": session, "limit_window_seconds": 18000}),
    };
    IdentifiedUsage {
        identity: Identity::from_native(account, "user"),
        usage: crate::harness::codex_app_login::usage_from_backend(
            &aterm_json::json!({
                "plan_type": "pro",
                "rate_limit": {
                    "allowed": session < 100 && week < 100,
                    "primary_window": primary,
                    "secondary_window": {"used_percent": week,
                        "limit_window_seconds": 604800, "reset_at": NOW + 86_400}
                }
            }),
            at,
        )
        .expect("real provider percentages"),
    }
}

/// A session as its host asks of it ([`codex_rotation::wall_asked`]): at a usage wall, of no named
/// window. The handoff's own read of the session's login names which limit it is.
fn asked_session(world: &World) -> Session<'_> {
    let mut session = session(Cli::Codex, Supervisor::Own, Mode::Early, &world.exe);
    session.point = codex_rotation::wall_asked(NOW);
    session
}

/// `label`'s condition at `at`, on the records as they stand.
fn condition_at(world: &World, label: &str, at: i64) -> Condition {
    let records = world
        .seat()
        .store(Cli::Codex)
        .read_account_records(
            Some(&world.cfg.join(Cli::Codex.accounts().roster_file)),
            NOW,
        )
        .expect("current account snapshot");
    accounts::condition(
        records.roster.row(label).expect("row"),
        &records.state,
        true,
        at,
    )
}

/// A FIVE-HOUR WALL HANDS A SESSION OFF, AND ITS ROW IS WALLED FOR FIVE HOURS, NOT A WEEK: with the
/// session's login at its five-hour limit, an hour from its reset, and 40% of its week used, the
/// handoff proposes the fresh row. The wall it records on the row it leaves is the five-hour one,
/// until that window's reset; no weekly wall is recorded, and the row is usable again once the
/// reset and its grace pass, though its week has not turned. Nothing is charged before the commit,
/// and the commit charges one rotation and moves `active`, as the week's does.
/// CONTROL: where the week is spent too, the wall on record is the week's, with the week's reset.
#[test]
fn a_five_hour_wall_hands_off_and_holds_its_row_only_to_the_five_hour_reset() {
    let source = Identity::from_native("source", "user");
    {
        let reset = NOW + HOUR;
        let (world, mut reader) = fixture("quota-five-hour");
        reader.answers.insert(
            main_of(&world),
            usage_resetting("source", (100, 40), Some(reset), NOW),
        );
        let proposal = codex_rotation::prepare(
            &world.seat(),
            &asked_session(&world),
            &source,
            &reader,
            Duration::from_secs(60),
        )
        .unwrap()
        .expect("a five-hour wall with a fresh row to take it");
        assert_eq!(proposal.source.label, "main");
        assert_eq!(proposal.destination.label, "work");
        let state = world.load(Cli::Codex);
        let walls = &state.accounts["main"].walls;
        assert_eq!(
            walls.keys().collect::<Vec<_>>(),
            [accounts::SESSION_LIMIT],
            "the five-hour wall alone, no weekly one"
        );
        assert_eq!(walls[accounts::SESSION_LIMIT].until, Some(reset));
        assert!(state.rotations.is_empty(), "a proposal charges none");
        assert_eq!(state.active.as_deref(), Some("main"));
        assert!(
            proposal
                .plan
                .commit_native(NOW, &"a".repeat(32), &"b".repeat(64))
                .line
                .is_some()
        );
        let state = world.load(Cli::Codex);
        assert_eq!(state.rotations, [NOW], "one rotation charged");
        assert_eq!(state.active.as_deref(), Some("work"));
        let back = reset + accounts::RESET_GRACE_S;
        assert_eq!(
            condition_at(&world, "main", back - 1),
            Condition::Limited { until: back }
        );
        assert_eq!(
            condition_at(&world, "main", back),
            Condition::Usable,
            "usable at its five-hour reset, with its week (40%) still running"
        );
    }

    let (world, mut reader) = fixture("quota-both-walls");
    reader
        .answers
        .insert(main_of(&world), usage_at("source", 100, 100));
    codex_rotation::prepare(
        &world.seat(),
        &asked_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .expect("CONTROL: the week's wall");
    let state = world.load(Cli::Codex);
    let walls = &state.accounts["main"].walls;
    assert_eq!(
        walls.keys().collect::<Vec<_>>(),
        [accounts::WEEK_LIMIT],
        "CONTROL"
    );
    assert_eq!(
        walls[accounts::WEEK_LIMIT].until,
        Some(NOW + 86_400),
        "CONTROL"
    );
}

/// A FIVE-HOUR WALL ABOUT TO RESET MOVES NOTHING: within [`accounts::RESET_SOON_S`] of the reset
/// the handoff is refused for good (its control answers that none is coming), with no row read but
/// the session's own, no wall recorded and no rotation charged; the session waits the reset out.
/// The bound is exact: at fifteen minutes it stays, a second past it moves. A read that names no
/// reset stays too: nothing bounds how long its wall would hold a session with no row to move to.
#[test]
fn a_five_hour_wall_about_to_reset_is_refused_and_charges_nothing() {
    use codex_rotation::{NoHandoff, prepare_refusal_stands};
    let source = Identity::from_native("source", "user");
    let soon = accounts::RESET_SOON_S;
    for (away, moves) in [
        (Some(600), false),
        (Some(soon), false),
        (Some(soon + 1), true),
        (Some(-60), false),
        (None, false),
    ] {
        let (world, mut reader) = fixture("quota-five-hour-soon");
        reader.answers.insert(
            main_of(&world),
            usage_resetting("source", (100, 40), away.map(|away| NOW + away), NOW),
        );
        let result = codex_rotation::prepare(
            &world.seat(),
            &asked_session(&world),
            &source,
            &reader,
            Duration::from_secs(60),
        );
        if moves {
            let proposal = result.unwrap().expect("past the bound it moves");
            assert_eq!(proposal.destination.label, "work", "{away:?}");
            continue;
        }
        let why = result.expect_err("a reset this near is waited out");
        assert_eq!(why, "not-walled", "{away:?}");
        assert_eq!(
            prepare_refusal_stands(why),
            Some(NoHandoff::NoWallToLeave),
            "{away:?}: the wire answer is `state=not-needed`"
        );
        assert_eq!(
            *reader.calls.borrow(),
            [main_of(&world)],
            "{away:?}: its own login alone"
        );
        let state = world.load(Cli::Codex);
        assert!(state.rotations.is_empty(), "{away:?}");
        assert_eq!(state.active.as_deref(), Some("main"), "{away:?}");
        assert!(
            state
                .accounts
                .get("main")
                .is_none_or(|account| account.walls.is_empty()),
            "{away:?}: no wall is recorded of a handoff that did not run"
        );
    }
}

/// A ROW AT ITS OWN FIVE-HOUR WALL TAKES NO SESSION, from a weekly wall or a five-hour one: the
/// fresh read of `work` finds its five-hour window spent, so nothing is proposed or charged, and
/// the read stands on record as `work`'s five-hour wall.
#[test]
fn a_row_at_its_five_hour_wall_is_no_handoffs_target() {
    let source = Identity::from_native("source", "user");
    for spent in [(0, 100), (100, 40)] {
        let (world, mut reader) = fixture("quota-target-five-hour");
        let work = world.work_dir(Cli::Codex).join("auth.json");
        native_auth(&work, "target");
        reader.answers.insert(
            main_of(&world),
            usage_resetting("source", spent, Some(NOW + HOUR), NOW),
        );
        reader
            .answers
            .insert(work.clone(), usage_at("target", 100, 0));
        let proposal = codex_rotation::prepare(
            &world.seat(),
            &asked_session(&world),
            &source,
            &reader,
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(proposal.is_none(), "{spent:?}: {proposal:?}");
        assert!(reader.calls.borrow().contains(&work), "{spent:?}: read");
        let state = world.load(Cli::Codex);
        assert!(state.rotations.is_empty(), "{spent:?}");
        assert_eq!(
            state.accounts["work"].walls[accounts::SESSION_LIMIT].until,
            Some(NOW + HOUR),
            "{spent:?}: the read is on record"
        );
    }
}

/// TWO ROWS NEAR THEIR FIVE-HOUR WALLS DO NOT TRADE A SESSION BACK AND FORTH. `main` walls and
/// its session moves to `work`, which charges one rotation and makes `work` active. Half an hour
/// later `work` walls too. `main` is still walled on record until its own five-hour reset, so no
/// row can take the session: the handoff reads the session's login once, proposes nothing, charges
/// nothing, leaves `active` where it is, and reads nothing at the asks that follow. Its control
/// answers `state=waiting`, not a move back onto the walled row.
/// THE ROW IT LEFT COMES BACK AT ITS FIVE-HOUR RESET: the first ask after `main`'s reset and grace
/// reads both logins, finds `main` with room, and moves the session there.
#[test]
fn two_rows_at_five_hour_walls_do_not_trade_a_session_back_and_forth() {
    let (world, mut reader) = fixture("quota-five-hour-ping-pong");
    let main = main_of(&world);
    let work = world.work_dir(Cli::Codex).join("auth.json");
    native_auth(&work, "target");
    let main_reset = NOW + HOUR;
    reader.answers.insert(
        main.clone(),
        usage_resetting("source", (100, 40), Some(main_reset), NOW),
    );
    let on_main = Identity::from_native("source", "user");
    let on_work = Identity::from_native("target", "user");
    let first = codex_rotation::prepare(
        &world.seat(),
        &asked_session(&world),
        &on_main,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .expect("`main` walls and `work` takes its session");
    assert_eq!(first.destination.label, "work");
    assert!(
        first
            .plan
            .commit_native(NOW, &"a".repeat(32), &"b".repeat(64))
            .line
            .is_some()
    );
    assert_eq!(world.load(Cli::Codex).rotations, [NOW]);

    // The same session, now on `work`, meets `work`'s five-hour wall inside the hour.
    let roster = world.seat().bound_roster(Cli::Codex).unwrap();
    let (var, value) = accounts::login_env_for(Cli::Codex, roster.row("work").unwrap());
    let value = value.expect("`work` has a login of its own");
    let env = [(var, value.as_str())];
    let ask = |reader: &Reader, at: i64| {
        let mut session = asked_session(&world);
        session.env = &env;
        session.point = codex_rotation::wall_asked(at);
        let seat = Seat {
            now: at,
            ..world.seat()
        };
        codex_rotation::prepare(&seat, &session, &on_work, reader, Duration::from_secs(60))
    };
    let walled = NOW + HOUR / 2;
    reader.answers.insert(
        work.clone(),
        usage_resetting("target", (100, 30), Some(walled + 4 * HOUR), walled),
    );
    reader.calls.borrow_mut().clear();
    for later in [0, 5, 10] {
        let answer = ask(&reader, walled + later);
        assert!(matches!(answer, Ok(None)), "+{later}s: {answer:?}");
    }
    assert_eq!(
        reader.calls.borrow().as_slice(),
        std::slice::from_ref(&work),
        "its own login, once: `main` is walled on record, so no ask reads it"
    );
    let state = world.load(Cli::Codex);
    assert_eq!(state.rotations, [NOW], "no second rotation is charged");
    assert_eq!(state.active.as_deref(), Some("work"), "`active` stays");
    assert_eq!(
        state.accounts["main"].walls[accounts::SESSION_LIMIT].until,
        Some(main_reset)
    );

    // `main`'s window resets: the first ask after its grace finds a row with room.
    let back = main_reset + accounts::RESET_GRACE_S;
    reader.answers.insert(
        main.clone(),
        usage_resetting("source", (0, 40), Some(back + 5 * HOUR), back),
    );
    reader.calls.borrow_mut().clear();
    let moved = ask(&reader, back)
        .unwrap()
        .expect("`main` takes the session back once its five-hour window reset");
    assert_eq!(
        (&*moved.source.label, &*moved.destination.label),
        ("work", "main")
    );
    assert_eq!(*reader.calls.borrow(), [work, main]);
}

/// A FIVE-HOUR WALL WITH NO ROW TO TAKE ITS SESSION WAITS ONLY TO ITS OWN RESET, never the week's:
/// with the other row walled too, each ask until the session's five-hour reset and its grace is
/// answered to wait, on the record alone (each login was read once, at the first ask). The first
/// ask after it reads the session's login again, finds its window turned, and is refused for good
/// (its control answers `state=not-needed`), with nothing charged and `active` where it was.
#[test]
fn a_five_hour_wall_with_no_target_waits_only_to_its_own_reset() {
    use codex_rotation::{NoHandoff, prepare_refusal_stands};
    let (world, mut reader) = fixture("quota-five-hour-wait");
    let main = main_of(&world);
    let work = world.work_dir(Cli::Codex).join("auth.json");
    native_auth(&work, "target");
    let reset = NOW + HOUR / 2;
    reader.answers.insert(
        main.clone(),
        usage_resetting("source", (100, 40), Some(reset), NOW),
    );
    reader.answers.insert(
        work.clone(),
        usage_resetting("target", (100, 0), Some(NOW + 4 * HOUR), NOW),
    );
    let source = Identity::from_native("source", "user");
    let ask = |reader: &Reader, at: i64| {
        let mut session = asked_session(&world);
        session.point = codex_rotation::wall_asked(at);
        let seat = Seat {
            now: at,
            ..world.seat()
        };
        codex_rotation::prepare(&seat, &session, &source, reader, Duration::from_secs(60))
    };
    let back = reset + accounts::RESET_GRACE_S;
    for at in [NOW, NOW + 5, reset, back - 1] {
        let answer = ask(&reader, at);
        assert!(matches!(answer, Ok(None)), "at {at}: {answer:?}");
    }
    assert_eq!(
        *reader.calls.borrow(),
        [main.clone(), work],
        "each login once, at the first ask"
    );

    reader.answers.insert(
        main.clone(),
        usage_resetting("source", (0, 40), Some(back + 5 * HOUR), back),
    );
    reader.calls.borrow_mut().clear();
    let why = ask(&reader, back).expect_err("its window turned: no handoff is coming");
    assert_eq!(prepare_refusal_stands(why), Some(NoHandoff::NoWallToLeave));
    assert_eq!(*reader.calls.borrow(), [main]);
    let state = world.load(Cli::Codex);
    assert!(state.rotations.is_empty());
    assert_eq!(state.active.as_deref(), Some("main"));
}

/// AN EXEC SESSION'S FIVE-HOUR WALL HANDS OFF THROUGH THE SAME READS AND RECORD: the typed command
/// names the fresh row's login, the wall on the row it leaves is the five-hour one with its reset,
/// and the commit charges one rotation.
#[test]
#[cfg(unix)]
fn native_exec_proposal_hands_off_at_a_five_hour_wall() {
    use crate::harness::codex_exec_handoff::ExecCommand;
    let (world, mut reader) = fixture("exec-quota-five-hour");
    reader.answers.insert(
        main_of(&world),
        usage_resetting("source", (100, 40), Some(NOW + HOUR), NOW),
    );
    let request = asked_session(&world);
    let program = std::env::current_exe().unwrap();
    let image = crate::harness::native_act::Image::at(&program).unwrap();
    let source = Identity::from_native("source", "user");
    let request = Session {
        cli: request.cli,
        sid: request.sid,
        supervisor: request.supervisor,
        env: request.env,
        seen: &crate::harness::observe::Observation::Unread,
        sample: request.sample,
        point: request.point,
        mode: request.mode,
        relaunch: (),
    };
    let proposal = codex_rotation::prepare_exec(
        &world.seat(),
        &request,
        &world.exe,
        &source,
        &reader,
        Duration::from_secs(60),
        |row| {
            Some(ExecCommand {
                argv0: "codex".into(),
                image: image.clone(),
                program: program.to_str().unwrap().into(),
                argv: vec!["exec".into(), "--json".into(), "original\ninput".into()],
                auth_path: row
                    .dir
                    .as_ref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| world.home.join(".codex"))
                    .join("auth.json")
                    .to_str()
                    .unwrap()
                    .into(),
                cwd: None,
            })
        },
    )
    .unwrap()
    .expect("an exec session's five-hour wall");
    assert_eq!(proposal.destination.label, "work");
    let state = world.load(Cli::Codex);
    assert!(state.rotations.is_empty());
    assert_eq!(
        state.accounts["main"].walls.keys().collect::<Vec<_>>(),
        [accounts::SESSION_LIMIT]
    );
    assert_eq!(
        state.accounts["main"].walls[accounts::SESSION_LIMIT].until,
        Some(NOW + HOUR)
    );
    let selected = proposal
        .plan
        .commit_native(NOW, &"d".repeat(32), &"e".repeat(64))
        .line
        .expect("native account selection");
    assert_eq!(Path::new(&selected.auth_path), proposal.new_auth);
    assert_eq!(world.load(Cli::Codex).rotations, [NOW]);
}

/// AN EXEC RUN REFUSED NEAR A FIVE-HOUR RESET IS HANDED OFF AT ITS NEXT WALL: a headless run is one
/// root turn that keeps its request, so the same session asks again at each wall it meets. Asked at
/// a five-hour wall ten minutes from its reset it is refused as not walled, with its own login
/// read alone and nothing recorded that would hold a later read. Asked again once that reset and
/// its grace have passed, the time its control lets that refusal stand, with its week now spent,
/// it is proposed the fresh row, and the wall on the row it leaves is the week's.
#[test]
#[cfg(unix)]
fn an_exec_session_refused_near_a_five_hour_reset_is_handed_off_at_its_next_wall() {
    use crate::harness::codex_exec_handoff::ExecCommand;
    use codex_rotation::{NoHandoff, prepare_refusal_stands};
    let (world, mut reader) = fixture("exec-quota-asked-again");
    let main = main_of(&world);
    let work = world.work_dir(Cli::Codex).join("auth.json");
    let program = std::env::current_exe().unwrap();
    let image = crate::harness::native_act::Image::at(&program).unwrap();
    let source = Identity::from_native("source", "user");
    let ask = |reader: &Reader, at: i64| {
        let asked = asked_session(&world);
        let request = Session {
            cli: asked.cli,
            sid: asked.sid,
            supervisor: asked.supervisor,
            env: asked.env,
            seen: &crate::harness::observe::Observation::Unread,
            sample: asked.sample,
            point: codex_rotation::wall_asked(at),
            mode: asked.mode,
            relaunch: (),
        };
        let seat = Seat {
            now: at,
            ..world.seat()
        };
        codex_rotation::prepare_exec(
            &seat,
            &request,
            &world.exe,
            &source,
            reader,
            Duration::from_secs(60),
            |row| {
                Some(ExecCommand {
                    argv0: "codex".into(),
                    image: image.clone(),
                    program: program.to_str().unwrap().into(),
                    argv: vec!["exec".into(), "--json".into(), "original\ninput".into()],
                    auth_path: row
                        .dir
                        .as_ref()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| world.home.join(".codex"))
                        .join("auth.json")
                        .to_str()
                        .unwrap()
                        .into(),
                    cwd: None,
                })
            },
        )
    };

    reader.answers.insert(
        main.clone(),
        usage_resetting("source", (100, 40), Some(NOW + 600), NOW),
    );
    let why = ask(&reader, NOW).expect_err("a reset ten minutes away is waited out");
    assert_eq!(why, "not-walled");
    assert_eq!(prepare_refusal_stands(why), Some(NoHandoff::NoWallToLeave));
    assert_eq!(
        reader.calls.borrow().as_slice(),
        std::slice::from_ref(&main),
        "its own login alone"
    );
    let state = world.load(Cli::Codex);
    assert!(state.rotations.is_empty());
    assert!(
        state
            .accounts
            .get("main")
            .is_none_or(|account| account.walls.is_empty()),
        "the refusal records no wall"
    );

    let again = NOW + accounts::RESET_SOON_S + accounts::RESET_GRACE_S;
    reader.answers.insert(
        main.clone(),
        usage_resetting("source", (0, 100), Some(again + 5 * HOUR), again),
    );
    reader
        .answers
        .insert(work.clone(), usage_read_at("target", false, again));
    reader.calls.borrow_mut().clear();
    let proposal = ask(&reader, again)
        .unwrap()
        .expect("the same session, asked again at its weekly wall");
    assert_eq!(proposal.source_identity, source);
    assert_eq!(proposal.destination.label, "work");
    assert_eq!(*reader.calls.borrow(), [main, work]);
    let state = world.load(Cli::Codex);
    assert!(
        state.rotations.is_empty(),
        "nothing is charged before commit"
    );
    assert_eq!(
        state.accounts["main"].walls.keys().collect::<Vec<_>>(),
        [accounts::WEEK_LIMIT]
    );
}

/// A SESSION WHOSE ROW'S LOGIN NAMES ANOTHER ACCOUNT IS TOLD AT ONCE: however often it is asked no
/// read of that login hands it off, so its control answers that no handoff is coming, and nothing is
/// read of the provider. CONTROL: the same session with a login of its row's own is proposed one.
#[test]
fn a_session_whose_rows_login_names_another_is_told_at_once() {
    let (world, reader) = fixture("quota-row-names-another");
    let roster = world.cfg.join(Cli::Codex.accounts().roster_file);
    let text = std::fs::read_to_string(&roster).unwrap();
    std::fs::write(
        &roster,
        text.replacen(
            "label = \"main\"\n",
            "label = \"main\"\nemail = \"owner@example.com\"\n",
            1,
        ),
    )
    .unwrap();
    let login = world.home.join(".codex/auth.json");
    let prepare = || {
        codex_rotation::prepare(
            &world.seat(),
            &weekly_session(&world),
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
        )
    };
    native_auth_naming(&login, "source", Some("owner@example.com"));
    assert!(matches!(prepare(), Ok(Some(_))), "CONTROL");
    let read = reader.calls.borrow().len();
    native_auth_naming(&login, "source", Some("z@example.com"));
    let why = prepare().expect_err("its login names another account");
    assert_eq!(
        codex_rotation::prepare_refusal_stands(why),
        Some(codex_rotation::NoHandoff::LoginNamesAnother),
        "{why}"
    );
    assert_eq!(reader.calls.borrow().len(), read, "no provider read");
}

/// A CANDIDATE WHOSE MANAGED LOGIN NAMES ANOTHER ACCOUNT IS NOT READ FROM THE PROVIDER: its file
/// identity already refuses it. CONTROL: the same candidate naming its row is read and selected.
#[test]
fn a_candidate_whose_login_names_another_is_not_read() {
    for (tag, email, selected) in [
        ("mismatch", "far@example.com", false),
        ("match", "work@example.com", true),
    ] {
        let (world, reader) = fixture(&format!("quota-candidate-{tag}"));
        let roster = world.cfg.join(Cli::Codex.accounts().roster_file);
        let text = std::fs::read_to_string(&roster).unwrap();
        std::fs::write(
            &roster,
            text.replacen(
                "label = \"work\"\n",
                "label = \"work\"\nemail = \"work@example.com\"\n",
                1,
            ),
        )
        .unwrap();
        let source = world.home.join(".codex/auth.json");
        let work = world.work_dir(Cli::Codex).join("auth.json");
        native_auth_naming(&work, "target", Some(email));
        let proposal = codex_rotation::prepare(
            &world.seat(),
            &weekly_session(&world),
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
        )
        .unwrap();
        assert_eq!(proposal.is_some(), selected, "{tag}");
        assert_eq!(
            reader.calls.borrow().contains(&work),
            selected,
            "{tag}: {:?}",
            reader.calls
        );
        assert!(reader.calls.borrow().contains(&source), "{tag}");
    }
}

/// A RECORD THE DEADLINE DOES NOT HANG ON FITS IN WHAT IS LEFT OF IT: a spent deadline waits for no
/// pause, and a wide one takes the whole ladder.
#[test]
fn a_fresh_read_waits_no_longer_for_its_record_than_the_deadline_leaves() {
    assert!(super::super::ladder_within(Duration::ZERO).is_empty());
    let some = Duration::from_millis(100);
    let within = super::super::ladder_within(some);
    assert!(!within.is_empty());
    assert!(within.iter().sum::<Duration>() <= some);
    assert_eq!(
        super::super::ladder_within(Duration::from_secs(3600)),
        accounts::LOCK_LADDER
    );
}

/// What a provider's read refuses a login's ordinary use by, as its `/wham/usage` reply says it.
#[derive(Clone, Copy, Debug)]
enum Refuses {
    /// Nothing: `rate_limit.allowed` true and no reached type.
    Nothing,
    /// `rate_limit.allowed` false, with no reached type.
    Ordinary,
    /// `rate_limit.allowed` false, and `rate_limit_reached_type` `rate_limit_reached`.
    RateLimitReached,
    /// `spend_control.reached`, beside `rate_limit.allowed` true: the spend control alone refuses.
    SpendControl,
    /// A workspace's reached type, the provider's word as it spells it
    /// (`usage-workspace-credits.json`'s shape).
    Workspace(&'static str),
    /// A credit-only plan out of credits, with no windows (`usage-credit-only.json`'s shape).
    CreditOnly,
}

impl Refuses {
    /// Each of the provider's four workspace words
    /// ([`crate::harness::codex_app_login::tests::WORKSPACE_WORDS`]).
    fn workspaces() -> [Self; 4] {
        crate::harness::codex_app_login::tests::WORKSPACE_WORDS
            .map(|(word, _)| Self::Workspace(word))
    }

    /// One of each kind outside the ordinary bucket's own.
    fn beyond_windows() -> Vec<Self> {
        let mut kinds = vec![Self::SpendControl];
        kinds.extend(Self::workspaces());
        kinds.push(Self::CreditOnly);
        kinds
    }
}

/// A provider's read of `account` at `NOW` refusing by `refuses`, with `session` and `week`
/// percent spent and the five-hour window resetting at `session_reset`; a credit-only read has no
/// windows.
fn read_refusing(
    account: &str,
    refuses: Refuses,
    (session, week): (f64, f64),
    session_reset: Option<i64>,
) -> IdentifiedUsage {
    let payload = match refuses {
        Refuses::CreditOnly => aterm_json::json!({
            "plan_type": "self_serve_business_usage_based",
            "credits": {"balance": "0", "has_credits": false, "unlimited": false},
            "rate_limit": {"allowed": false, "limit_reached": true}
        }),
        _ => {
            let primary = match session_reset {
                Some(reset) => aterm_json::json!({"used_percent": session,
                    "limit_window_seconds": 18000, "reset_at": reset}),
                None => aterm_json::json!({"used_percent": session, "limit_window_seconds": 18000}),
            };
            let mut payload = aterm_json::json!({
                "plan_type": "team",
                "rate_limit": {
                    "allowed": matches!(refuses, Refuses::Nothing | Refuses::SpendControl),
                    "primary_window": primary,
                    "secondary_window": {"used_percent": week,
                        "limit_window_seconds": 604_800, "reset_at": NOW + 86_400}
                }
            });
            let object = payload.as_object_mut().expect("a reply");
            match refuses {
                Refuses::RateLimitReached => {
                    object.insert(
                        "rate_limit_reached_type".to_owned(),
                        aterm_json::json!({"type": "rate_limit_reached"}),
                    );
                }
                Refuses::Workspace(word) => {
                    object.insert(
                        "rate_limit_reached_type".to_owned(),
                        aterm_json::json!({"type": word}),
                    );
                }
                Refuses::SpendControl => {
                    object.insert(
                        "spend_control".to_owned(),
                        aterm_json::json!({"reached": true, "individual_limit": {
                            "limit": "25000", "used": "25000", "remaining": "0",
                            "used_percent": 100, "remaining_percent": 0,
                            "reset_at": NOW + 20 * 86_400}}),
                    );
                }
                _ => {}
            }
            payload
        }
    };
    IdentifiedUsage {
        identity: Identity::from_native(account, "user"),
        usage: crate::harness::codex_app_login::usage_from_backend(&payload, NOW)
            .expect("real provider percentages"),
    }
}

/// THE WALL A HANDOFF LEAVES, BY KIND, IN THE FOUR RULES' ORDER: a spent week first, as the week's
/// wall, whatever else the read refuses; then a spend control or a workspace's refusal, under the
/// most-used window, even beside a five-hour window about to reset; then a spent five-hour window,
/// only where its reset is more than `RESET_SOON_S` away, so an ordinary refusal it explains
/// (`rate_limit_reached` among them) stays with it; then an ordinary refusal no spent window
/// explains, under the most-used window. "Spent" is the sample's whole percent, so a window at a
/// raw 99.6% is spent: a week there leaves as the week's wall where nothing is refused, though the
/// read itself records no wall for it below a raw 100%, and beside a five-hour wall about to reset.
/// Each of the provider's four workspace words, spelled as it spells them, leaves beside that
/// five-hour wall too. A read that refuses nothing and spends nothing stays.
#[test]
fn the_wall_a_handoff_leaves_follows_the_four_rules_by_kind() {
    use accounts::{SESSION_LIMIT, WEEK_LIMIT};
    let soon = Some(NOW + 300);
    let later = Some(NOW + 2 * HOUR);
    let mut cases = vec![
        (Refuses::Ordinary, (0.0, 100.0), later, Some(WEEK_LIMIT)),
        (Refuses::SpendControl, (0.0, 100.0), later, Some(WEEK_LIMIT)),
        (
            Refuses::SpendControl,
            (20.0, 10.0),
            later,
            Some(SESSION_LIMIT),
        ),
        (Refuses::SpendControl, (20.0, 50.0), later, Some(WEEK_LIMIT)),
        (Refuses::SpendControl, (30.0, 30.0), later, Some(WEEK_LIMIT)),
        (
            Refuses::SpendControl,
            (100.0, 10.0),
            soon,
            Some(SESSION_LIMIT),
        ),
        (
            Refuses::SpendControl,
            (100.0, 10.0),
            None,
            Some(SESSION_LIMIT),
        ),
        (
            Refuses::Workspace("workspace_owner_usage_limit_reached"),
            (10.0, 60.0),
            later,
            Some(WEEK_LIMIT),
        ),
        (Refuses::RateLimitReached, (100.0, 10.0), soon, None),
        (
            Refuses::RateLimitReached,
            (100.0, 10.0),
            later,
            Some(SESSION_LIMIT),
        ),
        (Refuses::Ordinary, (100.0, 10.0), soon, None),
        (Refuses::Ordinary, (100.0, 10.0), None, None),
        (Refuses::Ordinary, (99.6, 10.0), soon, None),
        (Refuses::Ordinary, (99.6, 10.0), later, Some(SESSION_LIMIT)),
        (Refuses::Ordinary, (100.0, 10.0), later, Some(SESSION_LIMIT)),
        (Refuses::Ordinary, (94.2, 31.0), later, Some(SESSION_LIMIT)),
        (Refuses::Ordinary, (20.0, 50.0), later, Some(WEEK_LIMIT)),
        (Refuses::CreditOnly, (0.0, 0.0), None, Some(SESSION_LIMIT)),
        (Refuses::Nothing, (20.0, 10.0), later, None),
        (Refuses::Nothing, (99.6, 10.0), later, Some(SESSION_LIMIT)),
        (Refuses::Nothing, (99.6, 10.0), soon, None),
        (Refuses::Nothing, (20.0, 99.6), later, Some(WEEK_LIMIT)),
        (
            Refuses::RateLimitReached,
            (100.0, 99.6),
            soon,
            Some(WEEK_LIMIT),
        ),
    ];
    for workspace in Refuses::workspaces() {
        cases.push((workspace, (20.0, 10.0), later, Some(SESSION_LIMIT)));
        cases.push((workspace, (100.0, 10.0), soon, Some(SESSION_LIMIT)));
    }
    for (refuses, used, reset, leaves) in cases {
        let read = read_refusing("source", refuses, used, reset);
        assert_eq!(
            codex_rotation::walled_window(&read, None, NOW),
            leaves.map_or(codex_rotation::Walled::No, |window| {
                codex_rotation::Walled::At(window.to_owned())
            }),
            "{refuses:?} at {used:?}, five-hour reset {reset:?}"
        );
    }
}

/// A SPEND CONTROL BESIDE A FIVE-HOUR WALL ABOUT TO RESET LEAVES: the five-hour window is spent and
/// resets in 5 minutes, which alone would be waited out, but the workspace's spend control stands
/// past that reset, so the session moves to the fresh row. The wall recorded on the row it leaves
/// is the spent five-hour window's, with its reset.
#[test]
fn a_spend_control_beside_a_five_hour_wall_about_to_reset_leaves() {
    let (world, mut reader) = fixture("quota-spend-soon");
    let reset = NOW + 300;
    reader.answers.insert(
        main_of(&world),
        read_refusing("source", Refuses::SpendControl, (100.0, 10.0), Some(reset)),
    );
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &asked_session(&world),
        &Identity::from_native("source", "user"),
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .expect("a spend control leaves whatever its five-hour reset");
    assert_eq!(proposal.source.label, "main");
    assert_eq!(proposal.destination.label, "work");
    let walls = &world.load(Cli::Codex).accounts["main"].walls;
    assert_eq!(walls.keys().collect::<Vec<_>>(), [accounts::SESSION_LIMIT]);
    assert_eq!(walls[accounts::SESSION_LIMIT].until, Some(reset));
}

/// What `prepare` answered a session whose own read was `read` ([`asked_with`]).
struct Asked {
    result: Result<Option<codex_rotation::Proposal>, &'static str>,
    calls: Vec<PathBuf>,
    walls: BTreeMap<String, accounts::Wall>,
}

/// The session's own read `read` asked at `NOW` in a fresh pool: what `prepare` answered, the
/// logins it read and the walls on record of `main` after it.
fn asked_with(name: &str, read: IdentifiedUsage) -> Asked {
    let (world, mut reader) = fixture(name);
    reader.answers.insert(main_of(&world), read);
    let result = codex_rotation::prepare(
        &world.seat(),
        &asked_session(&world),
        &Identity::from_native("source", "user"),
        &reader,
        Duration::from_secs(60),
    );
    let walls = world
        .load(Cli::Codex)
        .accounts
        .get("main")
        .map(|account| account.walls.clone())
        .unwrap_or_default();
    let calls = reader.calls.borrow().clone();
    Asked {
        result,
        calls,
        walls,
    }
}

/// AN ORDINARY REFUSAL A FIVE-HOUR WALL ABOUT TO RESET EXPLAINS STAYS: a read that refuses ordinary
/// use (no type) with its five-hour window spent and resetting in 5 minutes is the codex bucket's
/// own wall, which the reset ends, so the handoff is refused (`not-walled`, its control's
/// `state=not-needed`) with no candidate read and nothing recorded. So is the same read at a raw
/// 99.6%, which the sample takes as spent. CONTROL: a spend control at the same windows leaves.
#[test]
fn an_ordinary_refusal_at_a_five_hour_wall_about_to_reset_stays() {
    for session in [100.0, 99.6] {
        let Asked {
            result,
            calls,
            walls,
        } = asked_with(
            "quota-ordinary-soon",
            read_refusing(
                "source",
                Refuses::Ordinary,
                (session, 10.0),
                Some(NOW + 300),
            ),
        );
        assert_eq!(result.err(), Some("not-walled"), "{session}");
        assert_eq!(calls.len(), 1, "{session}: its own login alone");
        assert!(walls.is_empty(), "{session}: nothing recorded");
    }
    let Asked { result, .. } = asked_with(
        "quota-ordinary-soon-control",
        read_refusing(
            "source",
            Refuses::SpendControl,
            (100.0, 10.0),
            Some(NOW + 300),
        ),
    );
    assert!(matches!(result, Ok(Some(_))), "CONTROL: {result:?}");
}

/// `RATE_LIMIT_REACHED` IS THE ORDINARY WALL'S OWN TYPE, NOT A WORKSPACE'S: a read whose reached type
/// is `rate_limit_reached` (`usage-five-hour-reached.json`'s shape), its five-hour window spent and
/// resetting in 300 s, stays to wait out that reset, as the same wall with no type does.
#[test]
fn a_rate_limit_reached_read_at_a_five_hour_wall_about_to_reset_stays() {
    let Asked {
        result,
        calls,
        walls,
    } = asked_with(
        "quota-reached-soon",
        read_refusing(
            "source",
            Refuses::RateLimitReached,
            (100.0, 10.0),
            Some(NOW + 300),
        ),
    );
    assert_eq!(result.err(), Some("not-walled"));
    assert_eq!(calls.len(), 1, "its own login alone");
    assert!(walls.is_empty(), "nothing recorded");
}

/// A CANDIDATE THAT REFUSES ORDINARY USE IS NO DESTINATION, WHATEVER ITS WINDOWS: a read with room
/// in both windows but a spend control reached, a workspace's refusal or a credit-only plan out of
/// credits is not `usable`, and its fallback wall names no reset. CONTROL: the same windows with
/// nothing refused are.
#[test]
fn a_candidate_refusing_ordinary_use_is_not_usable() {
    let roomy = (10.0, 0.0);
    let reset = Some(NOW + HOUR);
    assert!(codex_rotation::usable(
        &read_refusing("target", Refuses::Nothing, roomy, reset),
        None,
        NOW
    ));
    for refuses in Refuses::beyond_windows() {
        let read = read_refusing("target", refuses, roomy, reset);
        assert!(!codex_rotation::usable(&read, None, NOW), "{refuses:?}");
        assert_eq!(
            read.usage.walls,
            [(accounts::SESSION_LIMIT, None)],
            "{refuses:?}"
        );
    }
}

/// A SOURCE THAT REFUSES ORDINARY USE BY ANY KIND MOVES TO A ROOMY ROW, AND A ROW THAT REFUSES SO
/// IS PASSED OVER: for a spend control, each workspace type and a credit-only plan out of credits,
/// with the source's windows roomy, the handoff proposes `work`, and the wall it records on `main`
/// is the fallback window's with no reset: a rung wall (`NO_RESET_WALL_S`), not the five-hour
/// window's turnover. Where `work` refuses the same way, or names the source's own account (a seat
/// of the same workspace), nothing is proposed or charged.
#[test]
fn a_source_refusing_by_any_kind_moves_and_a_row_refusing_so_is_passed_over() {
    let source = Identity::from_native("source", "user");
    let roomy = (20.0, 10.0);
    let reset = Some(NOW + HOUR);
    for refuses in Refuses::beyond_windows() {
        let (world, mut reader) = fixture("quota-refusal-kind");
        reader.answers.insert(
            main_of(&world),
            read_refusing("source", refuses, roomy, reset),
        );
        let proposal = codex_rotation::prepare(
            &world.seat(),
            &asked_session(&world),
            &source,
            &reader,
            Duration::from_secs(60),
        )
        .unwrap()
        .unwrap_or_else(|| panic!("{refuses:?}: a roomy row takes it"));
        assert_eq!(
            (
                proposal.source.label.as_str(),
                proposal.destination.label.as_str()
            ),
            ("main", "work"),
            "{refuses:?}"
        );
        let state = world.load(Cli::Codex);
        let walls = &state.accounts["main"].walls;
        assert_eq!(
            walls.keys().collect::<Vec<_>>(),
            [accounts::SESSION_LIMIT],
            "{refuses:?}"
        );
        let wall = &walls[accounts::SESSION_LIMIT];
        assert_eq!(wall.until, None, "{refuses:?}: a rung wall");
        assert_eq!(
            wall.stands_until(),
            Some(NOW + accounts::NO_RESET_WALL_S[0]),
            "{refuses:?}"
        );
        assert!(state.rotations.is_empty(), "{refuses:?}");

        for (tag, candidate) in [
            ("refusing", read_refusing("target", refuses, roomy, reset)),
            ("same account", usage("source", false)),
        ] {
            let (world, mut reader) = fixture("quota-refusal-kind-passed");
            let work = world.work_dir(Cli::Codex).join("auth.json");
            reader.answers.insert(
                main_of(&world),
                read_refusing("source", refuses, roomy, reset),
            );
            reader.answers.insert(work.clone(), candidate);
            let result = codex_rotation::prepare(
                &world.seat(),
                &asked_session(&world),
                &source,
                &reader,
                Duration::from_secs(60),
            );
            assert!(matches!(result, Ok(None)), "{refuses:?} {tag}: {result:?}");
            assert!(
                reader.calls.borrow().contains(&work),
                "{refuses:?} {tag}: read"
            );
            assert!(
                world.load(Cli::Codex).rotations.is_empty(),
                "{refuses:?} {tag}"
            );
        }
    }
}

/// The limit other than the codex bucket the limit tests ask about.
const OTHER: &str = "codex_other";

/// A provider's read of `account` at `NOW`: its codex bucket at `codex` percent of its five-hour
/// window and its week, and where `other` is given, the limit [`OTHER`] listed beside it at that
/// percent of its five-hour window, resetting in two hours (40% of its week spent), refused where
/// it is spent.
fn read_with_limit(account: &str, codex: (u64, u64), other: Option<u64>) -> IdentifiedUsage {
    let limits: Vec<aterm_json::Value> = other
        .into_iter()
        .map(|used| {
            aterm_json::json!({"limit_name": OTHER, "metered_feature": OTHER,
                "rate_limit": {"allowed": used < 100, "limit_reached": used >= 100,
                    "primary_window": {"used_percent": used, "limit_window_seconds": 18000,
                        "reset_at": NOW + 2 * HOUR},
                    "secondary_window": {"used_percent": 40, "limit_window_seconds": 604_800,
                        "reset_at": NOW + 3 * 86_400}}})
        })
        .collect();
    IdentifiedUsage {
        identity: Identity::from_native(account, "user"),
        usage: crate::harness::codex_app_login::usage_from_backend(
            &aterm_json::json!({
                "plan_type": "pro",
                "rate_limit": {
                    "allowed": codex.0 < 100 && codex.1 < 100,
                    "primary_window": {"used_percent": codex.0, "limit_window_seconds": 18000,
                        "reset_at": NOW + HOUR},
                    "secondary_window": {"used_percent": codex.1,
                        "limit_window_seconds": 604_800, "reset_at": NOW + 86_400}
                },
                "additional_rate_limits": limits
            }),
            NOW,
        )
        .expect("real provider percentages"),
    }
}

/// A pool of `main` (the default login, of the account `source`, active and proven) and one row of
/// a login of its own for each of `others`, of the account its label names, in that order; each
/// login's file, by label. Every login is one whose context the records capture, so what a read of
/// it answers is recorded.
fn pool_of(world: &World, others: &[&str]) -> BTreeMap<String, PathBuf> {
    let mut roster = "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n".to_owned();
    let main = main_of(world);
    std::fs::create_dir_all(main.parent().expect("default home")).expect("default home");
    native_auth(&main, "source");
    let mut logins = BTreeMap::from([("main".to_owned(), main)]);
    for label in others {
        let dir = world.root.join("logins/codex").join(label);
        std::fs::create_dir_all(&dir).expect("login dir");
        native_auth(&dir.join("auth.json"), label);
        roster.push_str(&format!(
            "\n[[account]]\nlabel = \"{label}\"\nclass = \"dedicated\"\ndir = {}\n",
            toml(&dir)
        ));
        logins.insert((*label).to_owned(), dir.join("auth.json"));
    }
    std::fs::write(world.cfg.join(Cli::Codex.accounts().roster_file), roster).expect("roster");
    world.on_main(Cli::Codex, &[]);
    logins
}

/// A session on `main` asking about the limit [`OTHER`].
fn limit_ask(source: &Identity) -> codex_rotation::Ask<'_> {
    codex_rotation::Ask::of(source, Some(OTHER))
}

/// A SOURCE WALLED ON ANOTHER MODEL'S LIMIT MOVES TO A ROW WITH ROOM ON IT: `main` reads with room
/// in its codex bucket and `codex_other` at its five-hour limit two hours from its reset, and the
/// handoff is asked about `codex_other`. A candidate that lists no such limit (`alpha`) and one
/// spent on it (`bravo`), which the ranking reads first (by label), are read and passed over,
/// though their codex buckets have room; `charlie`, with room on it, is proposed. The plan
/// records the wall on `main` under that limit alone, to
/// its own reset, so `main` is no less fresh for every other session; `bravo`'s read records its
/// own limit wall, `alpha`'s one that names no reset (its read lists no room there), and
/// `charlie` and every row's ordinary walls stay as they were. Asked again, the forecast passes
/// over `alpha` and `bravo`, which the records hold, and reads neither again.
/// Nothing is charged before the commit. CONTROL: the same reads asked about the codex bucket
/// find no wall to leave.
#[test]
fn a_source_walled_on_another_limit_moves_to_a_row_with_room_on_it() {
    let world = World::new("quota-limit-moves");
    let logins = pool_of(&world, &["alpha", "bravo", "charlie"]);
    let source = Identity::from_native("source", "user");
    let reader = Reader {
        calls: RefCell::new(Vec::new()),
        answers: BTreeMap::from([
            (
                logins["main"].clone(),
                read_with_limit("source", (10, 10), Some(100)),
            ),
            (
                logins["alpha"].clone(),
                read_with_limit("alpha", (10, 10), None),
            ),
            (
                logins["bravo"].clone(),
                read_with_limit("bravo", (10, 10), Some(100)),
            ),
            (
                logins["charlie"].clone(),
                read_with_limit("charlie", (10, 10), Some(30)),
            ),
        ]),
    };
    let ask = |reader: &Reader| {
        codex_rotation::prepare(
            &world.seat(),
            &asked_session(&world),
            limit_ask(&source),
            reader,
            Duration::from_secs(60),
        )
    };
    let proposal = ask(&reader)
        .unwrap()
        .expect("`charlie` has room on the limit `main` stands at");
    assert_eq!(
        (
            proposal.source.label.as_str(),
            proposal.destination.label.as_str()
        ),
        ("main", "charlie")
    );
    for passed in ["alpha", "bravo"] {
        assert!(
            reader.calls.borrow().contains(&logins[passed]),
            "{passed} was read: {:?}",
            reader.calls
        );
    }
    let state = world.load(Cli::Codex);
    let main = &state.accounts["main"];
    assert!(main.walls.is_empty(), "{:?}", main.walls);
    assert_eq!(main.limit_walls.keys().collect::<Vec<_>>(), [OTHER]);
    assert_eq!(main.limit_walls[OTHER].until, Some(NOW + 2 * HOUR));
    assert!(condition_at(&world, "main", NOW).fresh());
    assert_eq!(
        state.accounts["bravo"].limit_walls[OTHER].until,
        Some(NOW + 2 * HOUR)
    );
    assert_eq!(state.accounts["alpha"].limit_walls[OTHER].until, None);
    for label in ["alpha", "bravo", "charlie"] {
        let account = state.accounts.get(label).cloned().unwrap_or_default();
        assert!(account.walls.is_empty(), "{label}: {:?}", account.walls);
        assert!(condition_at(&world, label, NOW).fresh(), "{label}");
    }
    assert!(
        state
            .accounts
            .get("charlie")
            .is_none_or(|account| account.limit_walls.is_empty())
    );
    assert!(
        state.rotations.is_empty(),
        "nothing charged before the commit"
    );

    reader.calls.borrow_mut().clear();
    let again = ask(&reader).unwrap().expect("asked again, `charlie` again");
    assert_eq!(again.destination.label, "charlie");
    for held in ["alpha", "bravo"] {
        assert!(
            !reader.calls.borrow().contains(&logins[held]),
            "the records hold `{held}` off the limit: {:?}",
            reader.calls
        );
    }

    let control = codex_rotation::prepare(
        &world.seat(),
        &asked_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    );
    assert_eq!(control.err(), Some("not-walled"), "CONTROL");
}

/// A HANDOFF ASKED ABOUT A LIMIT ITS SOURCE'S READ DOES NOT LIST SAYS SO: `main`'s read lists no
/// `codex_other` at all, so nothing shows that limit standing; the handoff is refused
/// `limit-absent`, apart from `not-walled`, whose kind (`NoHandoff::LimitAbsent`) the control
/// logs in its own words, and nothing is read but `main` or recorded. A read whose entry has room
/// is `not-walled`.
#[test]
fn a_limit_the_sources_read_does_not_list_is_answered_absent() {
    use codex_rotation::{NoHandoff, prepare_refusal_stands};
    let source = Identity::from_native("source", "user");
    for (other, why) in [(None, "limit-absent"), (Some(30), "not-walled")] {
        let (world, mut reader) = fixture("quota-limit-absent");
        reader
            .answers
            .insert(main_of(&world), read_with_limit("source", (10, 10), other));
        let result = codex_rotation::prepare(
            &world.seat(),
            &asked_session(&world),
            limit_ask(&source),
            &reader,
            Duration::from_secs(60),
        );
        assert_eq!(result.err(), Some(why), "{other:?}");
        assert_eq!(
            reader.calls.borrow().len(),
            1,
            "{other:?}: its own login alone"
        );
        let main = world
            .load(Cli::Codex)
            .accounts
            .get("main")
            .cloned()
            .unwrap_or_default();
        assert!(
            main.walls.is_empty() && main.limit_walls.is_empty(),
            "{other:?}"
        );
    }
    assert_eq!(
        prepare_refusal_stands("limit-absent"),
        Some(NoHandoff::LimitAbsent)
    );
    assert_ne!(
        NoHandoff::LimitAbsent.as_str(),
        NoHandoff::NoWallToLeave.as_str()
    );
}

/// THE SOURCE'S OTHER LIMIT DOES NOT BLOCK AN ORDINARY MOVE, AND A RECORDED LIMIT WALL DOES NOT
/// MAKE A ROW LIMITED: `main` at its codex week, with `codex_other` spent too, asks with no limit;
/// `work`, whose records hold a wall of `codex_other` and whose read shows that limit spent, has
/// room in its codex bucket, and the ordinary handoff proposes it, as before limits were read.
/// The wall it records on `main` is the week's alone: an ordinary ask records no limit wall of the
/// session's own. `work`'s read is recorded, its spent `codex_other` in its limit walls alone, and
/// it stays fresh for every other session.
#[test]
fn another_limit_neither_blocks_nor_holds_an_ordinary_move() {
    let (world, mut reader) = fixture("quota-limit-ordinary");
    let work = world.work_dir(Cli::Codex).join("auth.json");
    native_auth(&work, "target");
    reader.answers.insert(
        main_of(&world),
        read_with_limit("source", (10, 100), Some(100)),
    );
    reader
        .answers
        .insert(work, read_with_limit("target", (10, 10), Some(100)));
    world
        .seat()
        .store(Cli::Codex)
        .transact(NOW, &mut None, |state| {
            Ok(accounts::note_wall(
                state,
                "work",
                &accounts::limit_wall(OTHER),
                Some(NOW + 2 * HOUR),
                NOW - MIN,
                NOW,
            ))
        })
        .unwrap();
    assert!(condition_at(&world, "work", NOW).fresh());
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &asked_session(&world),
        &Identity::from_native("source", "user"),
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .expect("the ordinary move reads the codex bucket alone");
    assert_eq!(proposal.destination.label, "work");
    let state = world.load(Cli::Codex);
    assert_eq!(
        state.accounts["main"].walls.keys().collect::<Vec<_>>(),
        [accounts::WEEK_LIMIT]
    );
    assert!(state.accounts["main"].limit_walls.is_empty());
    let work = &state.accounts["work"];
    assert_eq!(
        work.sample.as_ref().map(|s| s.at),
        Some(NOW),
        "the read is on record"
    );
    assert!(work.walls.is_empty(), "{:?}", work.walls);
    assert_eq!(work.limit_walls.keys().collect::<Vec<_>>(), [OTHER]);
    assert!(condition_at(&world, "work", NOW).fresh());
}

/// A LIMIT'S ENTRY IS JUDGED BY THE FOUR RULES OVER WINDOWS OF EVERY LENGTH: a spent week-long
/// window leaves whatever its reset (rule 1); a refusal no reset ends leaves beside a window about
/// to reset (rule 2); a spent window of any other length, five hours, a day, fifteen minutes or
/// none named, leaves only where its latest reset is more than `RESET_SOON_S` away, and stays
/// where it names none (rule 3), whatever ordinary refusal it explains; an ordinary refusal no
/// spent window explains leaves (rule 4). Each leaves as that limit's own wall
/// (`limit:codex_other`). An entry with room stays, and a read that lists none is absent. The
/// codex bucket's own walls play no part.
#[test]
fn a_limits_entry_follows_the_four_rules_over_windows_of_every_length() {
    use crate::ai::codex_limits::Bucket;
    use crate::harness::codex_app_login::{LimitUsage, LimitWindow, Refusal};
    use codex_rotation::Walled;
    let soon = Some(NOW + 120);
    let later = Some(NOW + 20 * HOUR);
    let at = Walled::At(accounts::limit_wall(OTHER));
    let cases = [
        (vec![(Bucket::Week, soon)], None, at.clone()),
        (
            vec![(Bucket::FiveHour, soon)],
            Some(Refusal::SpendControl),
            at.clone(),
        ),
        (vec![(Bucket::FiveHour, soon)], None, Walled::No),
        (
            vec![(Bucket::FiveHour, soon)],
            Some(Refusal::Ordinary),
            Walled::No,
        ),
        (vec![(Bucket::FiveHour, later)], None, at.clone()),
        (vec![(Bucket::Day, soon)], None, Walled::No),
        (vec![(Bucket::Day, later)], None, at.clone()),
        (vec![(Bucket::Day, None)], None, Walled::No),
        (
            vec![(Bucket::Minutes(15), Some(NOW + 600))],
            None,
            Walled::No,
        ),
        (
            vec![(Bucket::Minutes(15), Some(NOW + 1000))],
            None,
            at.clone(),
        ),
        (
            vec![(
                Bucket::Unplaced(crate::ai::codex_limits::Slot::Primary),
                later,
            )],
            None,
            at.clone(),
        ),
        (
            vec![(Bucket::FiveHour, soon), (Bucket::Day, later)],
            None,
            at.clone(),
        ),
        (
            vec![(Bucket::FiveHour, soon), (Bucket::Day, None)],
            None,
            Walled::No,
        ),
        (Vec::new(), Some(Refusal::Ordinary), at.clone()),
        (Vec::new(), None, Walled::No),
    ];
    for (walls, refusal, leaves) in cases {
        let mut read = read_with_limit("source", (100, 100), None);
        read.usage.limits.insert(
            OTHER.to_owned(),
            LimitUsage {
                windows: walls
                    .iter()
                    .map(|&(bucket, reset)| LimitWindow {
                        bucket,
                        used: 100,
                        reset,
                    })
                    .collect(),
                refusal,
            },
        );
        assert_eq!(
            codex_rotation::walled_window(&read, Some(OTHER), NOW),
            leaves,
            "{walls:?} {refusal:?}"
        );
    }
    let read = read_with_limit("source", (10, 10), None);
    assert_eq!(
        codex_rotation::walled_window(&read, Some(OTHER), NOW),
        Walled::Absent
    );
    assert_eq!(codex_rotation::walled_window(&read, None, NOW), Walled::No);
}

/// A ROW IS USABLE FOR A LIMIT ONLY WHERE ITS READ LISTS THAT LIMIT WITH ROOM: besides the codex
/// bucket's rule, the entry must be listed, none of its windows spent, nothing refusing it, and
/// none of them low. With no limit asked, the codex bucket's rule alone decides, whatever another
/// limit shows.
#[test]
fn a_row_is_usable_for_a_limit_only_with_room_on_it() {
    let usable = |read: &IdentifiedUsage, limit| codex_rotation::usable(read, limit, NOW);
    assert!(usable(
        &read_with_limit("t", (10, 10), Some(30)),
        Some(OTHER)
    ));
    assert!(!usable(&read_with_limit("t", (10, 10), None), Some(OTHER)));
    assert!(!usable(
        &read_with_limit("t", (10, 10), Some(100)),
        Some(OTHER)
    ));
    assert!(
        !usable(&read_with_limit("t", (10, 10), Some(95)), Some(OTHER)),
        "low"
    );
    assert!(!usable(
        &read_with_limit("t", (10, 100), Some(30)),
        Some(OTHER)
    ));
    for other in [None, Some(30), Some(100)] {
        assert!(
            usable(&read_with_limit("t", (10, 10), other), None),
            "{other:?}"
        );
    }
}

/// `world`'s records with `active` the proven pool row, and each of `walled` holding a wall of
/// [`OTHER`] two hours from its reset.
fn seat_active(world: &World, active: &str, walled: &[&str]) {
    world
        .seat()
        .store(Cli::Codex)
        .transact(NOW, &mut None, |state| {
            state.active = Some(active.to_owned());
            state.proven = true;
            let mut events = Vec::new();
            for label in walled {
                events.extend(accounts::note_wall(
                    state,
                    label,
                    &accounts::limit_wall(OTHER),
                    Some(NOW + 2 * HOUR),
                    NOW - MIN,
                    NOW,
                ));
            }
            Ok(events)
        })
        .unwrap();
}

/// A LIMIT ASK PASSES OVER AN `active` ROW WITHOUT ROOM ON THAT LIMIT, AND ITS MOVE COMMITS: the
/// pool's `active` is `alpha` (proven, its codex bucket with room), and a session on `main` is
/// walled on `codex_other`. Where `alpha`'s read lists no `codex_other`, lists it low (95% of its
/// five-hour window) or spent, the first ask follows the pool to `alpha`, reads it, and waits, as
/// an ordinary move off an `active` a fresh read finds low does; that read is recorded as a wall
/// of `codex_other` on `alpha` alone, so `alpha` stays fresh for every other session. The next ask
/// passes over `alpha` without reading it and proposes `charlie`, which has room on the limit, and
/// its commit rotates the pool there (one rotation charged): the records hold `alpha` off the
/// limit, so it is no row the session could follow. Where the records already hold `alpha` so,
/// the first ask proposes `charlie`. CONTROL: with `alpha` listing room on `codex_other`, the ask
/// follows the pool onto `alpha`, charging no rotation.
#[test]
fn a_limit_ask_passes_over_an_active_row_without_room_on_it_and_commits() {
    for (case, other, recorded) in [
        ("absent", None, false),
        ("low", Some(95), false),
        ("spent", Some(100), false),
        ("recorded", Some(100), true),
    ] {
        let world = World::new("quota-limit-active");
        let logins = pool_of(&world, &["alpha", "charlie"]);
        let walled: &[&str] = if recorded { &["alpha"] } else { &[] };
        seat_active(&world, "alpha", walled);
        let source = Identity::from_native("source", "user");
        let reader = Reader {
            calls: RefCell::new(Vec::new()),
            answers: BTreeMap::from([
                (
                    logins["main"].clone(),
                    read_with_limit("source", (10, 10), Some(100)),
                ),
                (
                    logins["alpha"].clone(),
                    read_with_limit("alpha", (10, 10), other),
                ),
                (
                    logins["charlie"].clone(),
                    read_with_limit("charlie", (10, 10), Some(30)),
                ),
            ]),
        };
        let ask = || {
            reader.calls.borrow_mut().clear();
            codex_rotation::prepare(
                &world.seat(),
                &asked_session(&world),
                limit_ask(&source),
                &reader,
                Duration::from_secs(60),
            )
        };
        if !recorded {
            assert!(matches!(ask(), Ok(None)), "{case}: the first ask waits");
            assert_eq!(
                *reader.calls.borrow(),
                [logins["main"].clone(), logins["alpha"].clone()],
                "{case}"
            );
            let state = world.load(Cli::Codex);
            assert!(
                state.accounts["alpha"]
                    .limit_wall_until(OTHER, NOW)
                    .is_some(),
                "{case}: alpha is held off the limit on record"
            );
            assert!(state.accounts["alpha"].walls.is_empty(), "{case}");
            assert!(condition_at(&world, "alpha", NOW).fresh(), "{case}");
        }
        let proposal = ask()
            .unwrap()
            .unwrap_or_else(|| panic!("{case}: `charlie` has room on the limit"));
        assert_eq!(proposal.destination.label, "charlie", "{case}");
        assert_eq!(
            *reader.calls.borrow(),
            [logins["main"].clone(), logins["charlie"].clone()],
            "{case}: alpha is not read again"
        );
        let receipt = root(&proposal, "a", "00000000-0000-0000-0000-000000000002");
        assert_eq!(proposal.commit(&receipt, NOW), Ok(()), "{case}");
        let state = world.load(Cli::Codex);
        assert_eq!(
            (state.active.as_deref(), state.rotations.as_slice()),
            (Some("charlie"), [NOW].as_slice()),
            "{case}"
        );
    }

    let world = World::new("quota-limit-active-roomy");
    let logins = pool_of(&world, &["alpha", "charlie"]);
    seat_active(&world, "alpha", &[]);
    let source = Identity::from_native("source", "user");
    let reader = Reader {
        calls: RefCell::new(Vec::new()),
        answers: BTreeMap::from([
            (
                logins["main"].clone(),
                read_with_limit("source", (10, 10), Some(100)),
            ),
            (
                logins["alpha"].clone(),
                read_with_limit("alpha", (10, 10), Some(30)),
            ),
        ]),
    };
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &asked_session(&world),
        limit_ask(&source),
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .expect("CONTROL: alpha has room");
    assert_eq!(proposal.destination.label, "alpha", "CONTROL");
    let receipt = root(&proposal, "a", "00000000-0000-0000-0000-000000000002");
    assert_eq!(proposal.commit(&receipt, NOW), Ok(()), "CONTROL");
    let state = world.load(Cli::Codex);
    assert_eq!(
        (state.active.as_deref(), state.rotations.len()),
        (Some("alpha"), 0),
        "CONTROL: a follow charges nothing"
    );
}

/// A PIN HOLDS A LIMIT ASK BEFORE ANY PROVIDER READ, ON ITS OWN ROW TOO: the owner pinned `main`,
/// whose session is walled on `codex_other` with its codex bucket roomy. A wall of another limit
/// ends no pin (`accounts::note_wall`), so the session is held where it is, as under a pin on
/// another row: nothing is read, nothing is recorded, and the pin stands, however often it asks.
/// CONTROL: the same pin and the same session asking about the codex bucket at its week moves to
/// `charlie`, its own wall ending the pin.
#[test]
fn a_pin_holds_a_limit_ask_before_any_provider_read() {
    let world = World::new("quota-limit-pinned");
    let logins = pool_of(&world, &["charlie"]);
    world
        .seat()
        .store(Cli::Codex)
        .transact(NOW, &mut None, |state| {
            state.pinned = true;
            Ok(Vec::new())
        })
        .unwrap();
    let source = Identity::from_native("source", "user");
    let mut reader = Reader {
        calls: RefCell::new(Vec::new()),
        answers: BTreeMap::from([
            (
                logins["main"].clone(),
                read_with_limit("source", (10, 10), Some(100)),
            ),
            (
                logins["charlie"].clone(),
                read_with_limit("charlie", (10, 10), Some(30)),
            ),
        ]),
    };
    for _ in 0..3 {
        let asked = codex_rotation::prepare(
            &world.seat(),
            &asked_session(&world),
            limit_ask(&source),
            &reader,
            Duration::from_secs(60),
        );
        assert!(matches!(asked, Ok(None)), "{asked:?}");
    }
    assert!(reader.calls.borrow().is_empty(), "{:?}", reader.calls);
    let state = world.load(Cli::Codex);
    assert!(state.pinned, "the owner's pin stands");
    assert!(
        state
            .accounts
            .get("main")
            .is_none_or(|main| main.limit_walls.is_empty() && main.walls.is_empty())
    );

    reader.answers.insert(
        logins["main"].clone(),
        read_with_limit("source", (10, 100), Some(100)),
    );
    let control = codex_rotation::prepare(
        &world.seat(),
        &asked_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .expect("CONTROL: its own week's wall ends the pin");
    assert_eq!(control.destination.label, "charlie");
    assert!(!world.load(Cli::Codex).pinned, "CONTROL");
}

/// A LIMIT MOVE'S WRITER AND COMMIT REFUSE A TARGET THAT LIMIT HOLDS SINCE ITS PREPARE: a move off
/// `main`'s `codex_other` wall to `charlie` is prepared, and before it is published another
/// session's read records `charlie` walled on `codex_other` too. The receipt naming that limit is
/// refused by the prepared writer (`target_stands`), and the pool's commit, whose re-advice passes
/// over the row that limit holds, refuses it, charging nothing. A receipt of the same move naming
/// no limit is not held by that wall: it is no wall of the codex bucket. CONTROL: with no such
/// wall the writer takes the receipt and the commit rotates the pool to `charlie`.
#[test]
fn a_limit_moves_writer_and_commit_refuse_a_target_that_limit_holds_since_its_prepare() {
    for walled in [true, false] {
        let world = World::new("quota-limit-target-walled");
        let logins = pool_of(&world, &["charlie"]);
        let source = Identity::from_native("source", "user");
        let reader = Reader {
            calls: RefCell::new(Vec::new()),
            answers: BTreeMap::from([
                (
                    logins["main"].clone(),
                    read_with_limit("source", (10, 10), Some(100)),
                ),
                (
                    logins["charlie"].clone(),
                    read_with_limit("charlie", (10, 10), Some(30)),
                ),
            ]),
        };
        let proposal = codex_rotation::prepare(
            &world.seat(),
            &asked_session(&world),
            limit_ask(&source),
            &reader,
            Duration::from_secs(60),
        )
        .unwrap()
        .expect("`charlie` has room on the limit");
        assert_eq!(proposal.destination.label, "charlie");
        let ordinary = root(&proposal, "a", "00000000-0000-0000-0000-000000000002");
        let mut receipt = ordinary.clone();
        receipt.intent.limit = Some(OTHER.to_owned());
        if walled {
            world
                .seat()
                .store(Cli::Codex)
                .transact(NOW, &mut None, |state| {
                    Ok(accounts::note_wall(
                        state,
                        "charlie",
                        &accounts::limit_wall(OTHER),
                        Some(NOW + 2 * HOUR),
                        NOW,
                        NOW,
                    ))
                })
                .unwrap();
        }
        let env = env_of(&world);
        let written = codex_rotation::with_prepared_writer(&env, &receipt, |_, _| Ok(true));
        let committed = proposal.commit(&receipt, NOW);
        let state = world.load(Cli::Codex);
        if walled {
            assert_eq!(written, Err("target-unavailable"));
            assert_eq!(
                codex_rotation::with_prepared_writer(&env, &ordinary, |_, _| Ok(true)),
                Ok(Some(true)),
                "no wall of the codex bucket"
            );
            assert_eq!(committed, Err("pool-commit-refused"));
            assert_eq!(
                (state.active.as_deref(), state.rotations.len()),
                (Some("main"), 0)
            );
        } else {
            assert_eq!(written, Ok(Some(true)), "CONTROL");
            assert_eq!(committed, Ok(()), "CONTROL");
            assert_eq!(
                (state.active.as_deref(), state.rotations.as_slice()),
                (Some("charlie"), [NOW].as_slice()),
                "CONTROL"
            );
        }
    }
}

/// A LIMIT'S ENTRY HAS ROOM ONLY WHERE IT SHOWS SOME, OVER WINDOWS OF EVERY LENGTH: an entry the
/// provider lists with no figure (`rate_limit` null) shows nothing to judge room by, so its row is
/// no target for that limit, as a codex bucket with no week figure is none; a day-long window at
/// 99%, twenty hours from its reset, is low as a five-hour one would be, and so is a fifteen-minute
/// one at 95% ten minutes from its reset, though no five-hour or week window shows it; a window a
/// week long at 95% is short of a week's low share, and so is one thirty days long, which the
/// read places in no named bucket. CONTROL: the day-long window at 50% has room, and a low window
/// whose reset has passed holds nothing.
#[test]
fn a_limits_entry_has_room_only_where_it_shows_some_over_windows_of_every_length() {
    let read = |rate_limit: aterm_json::Value| IdentifiedUsage {
        identity: Identity::from_native("target", "user"),
        usage: crate::harness::codex_app_login::usage_from_backend(
            &aterm_json::json!({
                "plan_type": "pro",
                "rate_limit": {"allowed": true,
                    "primary_window": {"used_percent": 10, "limit_window_seconds": 18000,
                        "reset_at": NOW + HOUR},
                    "secondary_window": {"used_percent": 10, "limit_window_seconds": 604_800,
                        "reset_at": NOW + 86_400}},
                "additional_rate_limits": [{"limit_name": OTHER, "metered_feature": OTHER,
                    "rate_limit": rate_limit}]
            }),
            NOW,
        )
        .expect("real provider percentages"),
    };
    let window = |used: u32, seconds: i64, reset: i64| {
        aterm_json::json!({"allowed": true, "primary_window": {"used_percent": used,
            "limit_window_seconds": seconds, "reset_at": reset}})
    };
    let usable = |read: &IdentifiedUsage, at: i64| codex_rotation::usable(read, Some(OTHER), at);
    let unfigured = read(aterm_json::Value::Null);
    assert!(unfigured.usage.limits.contains_key(OTHER), "it is listed");
    assert!(!usable(&unfigured, NOW), "no figure");
    let day = read(window(99, 86_400, NOW + 20 * HOUR));
    assert!(!usable(&day, NOW), "a day at 99%");
    let quarter = read(window(95, 900, NOW + 600));
    assert!(!usable(&quarter, NOW), "fifteen minutes at 95%");
    let week = read(window(95, 604_800, NOW + 86_400));
    assert!(usable(&week, NOW), "a week at 95%");
    let month = read(window(95, 30 * 86_400, NOW + 86_400));
    assert!(usable(&month, NOW), "thirty days at 95%");
    assert!(
        usable(&read(window(50, 86_400, NOW + 20 * HOUR)), NOW),
        "CONTROL"
    );
    assert!(usable(&quarter, NOW + 601), "CONTROL: reset");
}

/// AN EXEC SESSION'S PREPARE ASKS WHAT ITS INTENT ASKS: an exec source on `main`'s login, whose
/// read shows its codex bucket roomy and `codex_other` spent, prepares under an intent naming
/// `codex_other` ([`crate::harness::codex_exec_handoff::ExecSource::prepare_on`]): the handoff
/// judges that limit, so it reads `charlie` and records `main`'s wall of it. The source is no
/// live process, so the prepare then answers `exec-source-changed`, as for a run that changed
/// under it. CONTROL: under the same intent naming no limit, the codex bucket is judged, `main`
/// alone is read, and nothing is recorded.
#[test]
#[cfg(unix)]
fn an_exec_sources_prepare_asks_what_its_intent_asks() {
    use crate::harness::codex_renewal::{Intent, Purpose};
    for limit in [Some(OTHER), None] {
        let world = World::new("exec-source-limit");
        let logins = pool_of(&world, &["charlie"]);
        let reader = Reader {
            calls: RefCell::new(Vec::new()),
            answers: BTreeMap::from([
                (
                    logins["main"].clone(),
                    read_with_limit("source", (10, 10), Some(100)),
                ),
                (
                    logins["charlie"].clone(),
                    read_with_limit("charlie", (10, 10), Some(30)),
                ),
            ]),
        };
        let image =
            crate::harness::native_act::Image::at(&std::env::current_exe().unwrap()).unwrap();
        let process = crate::harness::native_act::Process {
            pid: 123,
            birth: 456,
            parent: 10,
            group: 123,
            session: 10,
            image,
        };
        let args = atpkg::caller_shell::ProcArgs {
            exec_path: String::from_utf8(process.image.path.clone()).unwrap(),
            argv: vec!["codex".into(), "exec".into(), "-".into()],
            env: vec![format!("HOME={}", world.home.display())],
        };
        let exec = crate::harness::codex_exec_handoff::ExecSource::observed(
            process,
            args,
            world.home.clone(),
        )
        .expect("an exec source on main's login");
        assert_eq!(exec.auth_path, logins["main"]);
        let intent = Intent {
            purpose: Purpose::WeeklyQuota,
            thread: "00000000-0000-0000-0000-000000000001".into(),
            wall: "00000000-0000-0000-0000-000000000002".into(),
            identity: Identity::from_native("source", "user"),
            limit: limit.map(str::to_owned),
        };
        let prepared = exec.prepare_on(
            &world.seat(),
            "s-exec",
            &intent,
            &reader,
            Duration::from_secs(60),
            &world.exe,
        );
        assert_eq!(prepared.err(), Some("exec-source-changed"), "{limit:?}");
        let main = world
            .load(Cli::Codex)
            .accounts
            .get("main")
            .cloned()
            .unwrap_or_default();
        if limit.is_some() {
            assert_eq!(
                *reader.calls.borrow(),
                [logins["main"].clone(), logins["charlie"].clone()]
            );
            assert!(main.limit_wall_until(OTHER, NOW).is_some());
        } else {
            assert_eq!(*reader.calls.borrow(), [logins["main"].clone()], "CONTROL");
            assert!(main.limit_walls.is_empty(), "CONTROL");
        }
    }
}
