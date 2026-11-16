// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A usage-limit move's completion ([`renewal::record_completed_rotation`]): the bound
//! successor's report that its resumed turn was served proves the destination for the pool, and
//! what that proof releases. Each move here is the one [`reserved`] commits, `main` walled onto
//! `work` for the session `s-one`, then consumed ([`consume`]) as its successor's first turn
//! leaves it.

use super::*;
use codex_rotation::Proposal;

/// The session `s-two`, on `main` too, asking at its own weekly wall at `at`.
fn follower(world: &World, reader: &Reader, at: i64) -> Result<Option<Proposal>, &'static str> {
    let mut session = weekly_session(world);
    session.sid = "s-two";
    session.point = Point::Wall {
        window: "seven_day".into(),
        at: Some(at),
    };
    codex_rotation::prepare(
        &Seat {
            now: at,
            ..world.seat()
        },
        &session,
        &Identity::from_native("source", "user"),
        &Fresh(reader),
        Duration::from_secs(60),
    )
}

/// A COMPLETED MOVE RELEASES THE SESSIONS WALLED BEHIND ITS CANARY AT ONCE: while `work`'s canary
/// is unproven, a second session walled on `main` is held (its ask reads nothing and proposes
/// nothing). Once the first move's completion is recorded, the pool holds `work` proven with no
/// canary, and the same ask proposes `work` as a follow of the proven row: no rotation charged
/// (`rotates` false), no canary of its own (`takes_canary` false). Its commit charges nothing and
/// leaves no canary, and the pool's deduplication records hold the follower's binding alone: the
/// first move's was retired at its consume.
#[test]
fn a_completed_move_releases_its_walled_followers_at_once() {
    let (world, reader, env, mut first, _) = reserved("completion-follow");
    consume(&env, &mut first);
    assert!(matches!(follower(&world, &reader, NOW), Ok(None)));
    assert!(world.load(Cli::Codex).canary_holds(NOW));

    assert_eq!(renewal::record_completed_rotation(&env, &first), Ok(()));
    let state = world.load(Cli::Codex);
    assert!(state.proven);
    assert!(state.canary.is_none());
    assert_eq!(state.rotations, [NOW]);

    let next = follower(&world, &reader, NOW).unwrap().expect("a follow");
    assert_eq!(next.destination.label, "work");
    assert!(
        matches!(next.plan.advice(), Advice::Move { to, rotates: false, takes_canary: false, .. }
            if to == "work"),
        "{:?}",
        next.plan.advice()
    );
    let mut second = root(&next, "c", "00000000-0000-0000-0000-000000000003");
    second.sid = "s-two".into();
    next.commit(&second, NOW).unwrap();
    let state = world.load(Cli::Codex);
    assert_eq!(state.active.as_deref(), Some("work"));
    assert!(state.proven);
    assert!(state.canary.is_none());
    assert_eq!(state.rotations, [NOW]);
    let logins = world.seat().store(Cli::Codex).load_logins(NOW).unwrap().0;
    assert_eq!(
        logins.native_rotations.keys().collect::<Vec<_>>(),
        [&second.id]
    );
}

/// WITHOUT A REPORT THE CANARY'S HOLD IS STILL THE CEILING: ten minutes
/// ([`accounts::CANARY_HOLD_S`]), after which a walled follower takes the silent canary's place
/// as before. A move whose successor never reports holds its followers no longer than that.
#[test]
fn without_a_report_the_canary_holds_its_followers_ten_minutes_at_most() {
    assert_eq!(accounts::CANARY_HOLD_S, 600);
    let (world, reader, env, mut first, _) = reserved("completion-ceiling");
    consume(&env, &mut first);
    assert!(matches!(follower(&world, &reader, NOW + 599), Ok(None)));
    let taken = follower(&world, &reader, NOW + 600)
        .unwrap()
        .expect("the silent canary's place");
    assert!(
        matches!(taken.plan.advice(), Advice::Move { to, takes_canary: true, .. } if to == "work"),
        "{:?}",
        taken.plan.advice()
    );
    assert!(!world.load(Cli::Codex).proven);
}

/// A LOCK HELD ELSEWHERE IS BUSY, A CHANGED LOGIN IS REFUSED: the destination's or the source's
/// auth lock, the machine's sign-in turn and the receipt writer's `accounts.lock`, each held by
/// another taker, leave the records unjudged ([`renewal::Unrecorded::Busy`]: asking again may
/// record it), and the pool unproven. A source login re-signed to another account since the wall
/// is the records' refusal ([`renewal::Unrecorded::Refused`]), the custody lock's word, and the
/// pool stays unproven. CONTROL: with every lock free and the logins as the receipt names them,
/// the completion is recorded.
#[test]
fn a_held_lock_leaves_a_completion_busy_and_a_changed_source_refuses_it() {
    let (world, _, env, mut first, _) = reserved("completion-busy");
    consume(&env, &mut first);
    for login in [&first.new_auth, &first.old_auth] {
        let held = crate::harness::codex_app_login::AuthLock::try_take(login)
            .unwrap()
            .unwrap();
        assert_eq!(
            renewal::record_completed_rotation(&env, &first),
            Err(renewal::Unrecorded::Busy),
            "{}",
            login.display()
        );
        drop(held);
    }
    let store = world.seat().store(Cli::Codex);
    let turn = store.signin_now().unwrap().unwrap();
    assert_eq!(
        renewal::record_completed_rotation(&env, &first),
        Err(renewal::Unrecorded::Busy),
        "the sign-in turn"
    );
    drop(turn);
    let writer = store.acquire_bounded().unwrap().unwrap();
    assert_eq!(
        renewal::record_completed_rotation(&env, &first),
        Err(renewal::Unrecorded::Busy),
        "the receipt writer"
    );
    drop(writer);
    assert!(!world.load(Cli::Codex).proven);

    native_auth(&first.old_auth, "re-signed");
    assert_eq!(
        renewal::record_completed_rotation(&env, &first),
        Err(renewal::Unrecorded::Refused("identity-mismatch"))
    );
    assert!(!world.load(Cli::Codex).proven);
    native_auth(&first.old_auth, "source");
    assert_eq!(renewal::record_completed_rotation(&env, &first), Ok(()));
    assert!(world.load(Cli::Codex).proven);
}

/// `main`'s roster row with `extra` (TOML lines) added to it.
fn mark_main(world: &World, extra: &str) {
    let roster = world.cfg.join(Cli::Codex.accounts().roster_file);
    let text = std::fs::read_to_string(&roster).unwrap();
    let marked = text.replacen(
        "label = \"main\"\n",
        &format!("label = \"main\"\n{extra}"),
        1,
    );
    assert_ne!(marked, text, "main's row");
    std::fs::write(&roster, marked).unwrap();
}

/// A COMPLETION NEEDS ITS SOURCE ROW UNHELD, AS IT NEEDS ITS SOURCE LOGIN UNCHANGED: the source row
/// taken into another Mac's custody since the wall is no longer the move the receipt sealed, and
/// its completion is the records' refusal, never a busy lock: held on the roster (`held_by`, as a
/// sync from the lab's pool records it) the writer refuses it (`selection-changed`), and held only
/// in the lab's pool document, before a sync has recorded it on the row, the custody lock does
/// (`source-held`). Either way the pool stays unproven behind its canary. CONTROL: with the hold
/// gone the same completion is recorded.
#[cfg(unix)] // seeds its pool entry through `vault_entry::put`, a unix-only seam
#[test]
fn a_completion_refuses_a_source_held_since_the_wall() {
    for (case, why) in [("roster", "selection-changed"), ("pool", "source-held")] {
        let (world, _, env, mut first, _) = reserved("completion-source-held");
        consume(&env, &mut first);
        match case {
            "roster" => mark_main(&world, "held_by = \"m2\"\n"),
            _ => {
                mark_main(&world, "email = \"main@example.test\"\n");
                crate::vault_entry::put(
                    &world.home,
                    crate::harness::ai_pool::AI_POOL_FILE,
                    "version=1\n[[account]]\nprovider='codex'\nlabel='main'\n\
                     email='main@example.test'\nwarm=['m2']\n",
                );
            }
        }
        assert_eq!(
            renewal::record_completed_rotation(&env, &first),
            Err(renewal::Unrecorded::Refused(why)),
            "{case}"
        );
        let state = world.load(Cli::Codex);
        assert!(!state.proven, "{case}");
        assert!(state.canary_holds(NOW), "{case}");
        match case {
            "roster" => {
                let roster = world.cfg.join(Cli::Codex.accounts().roster_file);
                let text = std::fs::read_to_string(&roster).unwrap();
                std::fs::write(&roster, text.replacen("held_by = \"m2\"\n", "", 1)).unwrap();
            }
            _ => crate::vault_entry::put(&world.home, crate::harness::ai_pool::AI_POOL_FILE, ""),
        }
        assert_eq!(
            renewal::record_completed_rotation(&env, &first),
            Ok(()),
            "{case}: CONTROL"
        );
        assert!(world.load(Cli::Codex).proven, "{case}");
    }
}

/// `label`'s usage on record read low at `NOW`: 95% of its five hours used, the week's room left.
fn read_low(world: &World, label: &str) {
    world
        .seat()
        .store(Cli::Codex)
        .transact(NOW, &mut None, |state| {
            Ok(accounts::note_sample(
                state,
                label,
                Sample {
                    at: NOW,
                    ..near_wall()
                },
                NOW,
            ))
        })
        .unwrap();
}

/// The bound roster's row `label`.
fn row_of(world: &World, label: &str) -> accounts::Row {
    world
        .seat()
        .bound_roster(Cli::Codex)
        .unwrap()
        .row(label)
        .unwrap()
        .clone()
}

/// `label`'s condition as a move's writer reads it: on the state with each row's login projected
/// onto it.
fn condition_of(world: &World, label: &str) -> accounts::Condition {
    let store = world.seat().store(Cli::Codex);
    let mut state = store.load(NOW).unwrap().0;
    store.hydrate_auth_contexts(&mut state, &world.seat().bound_roster(Cli::Codex).unwrap());
    accounts::condition(&row_of(world, label), &state, true, NOW)
}

/// A COMPLETION PROVES A DESTINATION THAT STILL RUNS, LOW INCLUDED: the successor served its turn
/// on `work`, which now reads low. No move selects a low row: the prepared and the selected
/// writers refuse it (CONTROL: both admit it while it reads fresh). Its completion is recorded
/// all the same, and the pool holds `work` proven with no canary.
#[test]
fn a_completion_proves_a_destination_that_now_reads_low() {
    let (world, _, env, mut first, _) = reserved("completion-low");
    assert_eq!(
        codex_rotation::with_prepared_writer(&env, &first, |_, _| Ok(true)),
        Ok(Some(true))
    );
    assert_eq!(
        renewal::with_selected_writer(&env, &first, |_, _| Ok(true)),
        Ok(Some(true))
    );
    read_low(&world, "work");
    assert_eq!(condition_of(&world, "work"), accounts::Condition::Low);
    assert_eq!(
        codex_rotation::with_prepared_writer(&env, &first, |_, _| Ok(true)),
        Err("target-unavailable")
    );
    assert_eq!(
        renewal::with_selected_writer(&env, &first, |_, _| Ok(true)),
        Err("target-unavailable")
    );
    consume(&env, &mut first);
    assert_eq!(renewal::record_completed_rotation(&env, &first), Ok(()));
    let state = world.load(Cli::Codex);
    assert!(state.proven);
    assert!(state.canary.is_none());
    assert_eq!(state.active.as_deref(), Some("work"));
}

/// A COMPLETION NEVER PROVES A DESTINATION THAT NO LONGER RUNS: `work` walled on record since the
/// move, or signed out here, refuses it (the records' refusal, not a busy lock), and the pool
/// stays unproven with its canary.
#[test]
fn a_completion_refuses_a_destination_walled_or_signed_out_since() {
    for case in ["walled", "signed out"] {
        let (world, _, env, mut first, _) = reserved("completion-no-longer-runs");
        consume(&env, &mut first);
        match case {
            "walled" => wall(&world, Cli::Codex, "work", NOW + HOUR),
            _ => world
                .seat()
                .store(Cli::Codex)
                .record_lapse_for_lab(&row_of(&world, "work"), NOW)
                .unwrap(),
        }
        assert!(!condition_of(&world, "work").runs(), "{case}");
        assert_eq!(
            renewal::record_completed_rotation(&env, &first),
            Err(renewal::Unrecorded::Refused("target-unavailable")),
            "{case}"
        );
        let state = world.load(Cli::Codex);
        assert!(!state.proven, "{case}");
        assert!(state.canary_holds(NOW), "{case}");
    }
}

/// A PROVEN LOW DESTINATION ROUTES ITS FOLLOWERS BY THE LOW RULE: with a third row `spare` signed
/// in and fresh, a session walled on `main` moves to `spare`, not onto the low `work`, and that is
/// a rotation of its own: charged, with the follower as `spare`'s new canary. With no fresh row,
/// the same session's ask proposes nothing: it waits, as no move selects a low row.
#[test]
fn a_proven_low_destination_routes_its_followers_to_a_fresh_row_or_none() {
    for spare in [true, false] {
        let (world, mut reader, env, mut first, _) = reserved("completion-low-follow");
        consume(&env, &mut first);
        read_low(&world, "work");
        assert_eq!(renewal::record_completed_rotation(&env, &first), Ok(()));
        assert!(world.load(Cli::Codex).proven);
        if spare {
            let dir = world.root.join("logins/codex/spare");
            std::fs::create_dir_all(&dir).unwrap();
            native_auth(&dir.join("auth.json"), "spare");
            let roster = world.cfg.join(Cli::Codex.accounts().roster_file);
            let mut text = std::fs::read_to_string(&roster).unwrap();
            text.push_str(&format!(
                "\n[[account]]\nlabel = \"spare\"\nclass = \"dedicated\"\ndir = {}\n",
                toml(&dir)
            ));
            std::fs::write(&roster, text).unwrap();
            reader
                .answers
                .insert(dir.join("auth.json"), usage("spare", false));
        }
        let asked = follower(&world, &reader, NOW).unwrap();
        if !spare {
            assert!(asked.is_none(), "{asked:?}");
            let state = world.load(Cli::Codex);
            assert_eq!(state.active.as_deref(), Some("work"));
            assert_eq!(state.rotations, [NOW]);
            continue;
        }
        let next = asked.expect("a move to the fresh row");
        assert_eq!(next.destination.label, "spare");
        assert!(
            matches!(next.plan.advice(), Advice::Move { to, rotates: true, takes_canary: false, .. }
                if to == "spare"),
            "{:?}",
            next.plan.advice()
        );
        let mut second = root(&next, "c", "00000000-0000-0000-0000-000000000003");
        second.sid = "s-two".into();
        next.commit(&second, NOW).unwrap();
        let state = world.load(Cli::Codex);
        assert_eq!(state.active.as_deref(), Some("spare"));
        assert!(!state.proven);
        assert_eq!(state.rotations, [NOW, NOW]);
        let canary = state.canary.as_ref().expect("the follower's canary");
        assert_eq!(canary.sid, "s-two");
        assert_eq!(canary.native_receipt.as_deref(), Some(second.id.as_str()));
    }
}
