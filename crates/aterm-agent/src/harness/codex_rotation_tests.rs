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
                assert!(renewal::record_completed_rotation(&env, &receipt).is_err());
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
                assert_eq!(
                    renewal::record_completed_rotation(&env, &receipt),
                    Ok(Some(()))
                );
                assert!(world.load(Cli::Codex).proven);
                assert!(world.load(Cli::Codex).canary.is_none());
                assert_eq!(
                    renewal::record_completed_rotation(&env, &receipt),
                    Ok(Some(()))
                );
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

#[test]
fn concurrent_native_plans_follow_the_proven_target_without_another_rotation() {
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
        .plan
    };
    let first = prepare(&first_session);
    let next = prepare(&next_session);
    let id = "a".repeat(32);
    let next_id = "c".repeat(32);
    let binding = "b".repeat(64);
    assert!(first.commit_native(NOW, &id, &binding).line.is_some());
    assert!(
        next.clone()
            .commit_native(NOW, &next_id, &binding)
            .line
            .is_none(),
        "a pending canary still owns admission"
    );
    let store = world.seat().store(Cli::Codex);
    store
        .transact(NOW, &mut None, |state| {
            Ok(accounts::prove(state, "work", NOW).into_iter().collect())
        })
        .unwrap();
    assert!(
        next.commit_native(NOW, &next_id, &binding).line.is_some(),
        "a successful canary lets the already-prepared session follow the same target"
    );
    assert_eq!(world.load(Cli::Codex).rotations, [NOW]);
    assert_eq!(store.load_logins(NOW).unwrap().0.native_rotations.len(), 2);
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
