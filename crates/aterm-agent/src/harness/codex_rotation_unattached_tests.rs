// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A rotation whose successor never attached gives the pool back what its move took.

use super::*;
use crate::harness::contract::Scope;
use crate::harness::native_act::{
    Image, NativePlan, Prepare, Process, Request as NativeRequest, RestartCommand,
};

/// The native host's closure verdict, as the fixture states it.
struct Closure(bool);

impl renewal::NativeUnattachedProof for Closure {
    fn ended_without_successor(&self, _: &Prepare) -> bool {
        self.0
    }
}

/// A sealed restart recipe the receipt writer parses, under `id`.
fn recipe(id: &str, deadline_us: u64) -> Prepare {
    let image = Image {
        path: b"/fixture/codex".to_vec(),
        device: 1,
        inode: 2,
    };
    let command = RestartCommand {
        settles: None,
        account_intent: None,
        image: image.clone(),
        program: at!("/fixture/codex").into(),
        argv: vec!["resume".into(), "original-thread".into()],
        env: vec![("CODEX_AUTH_PATH".into(), "/fixture/work/auth.json".into())],
        unset: None,
        cwd: Some("/fixture/work".into()),
        heal: None,
        dialect: crate::harness::upgrade::Dialect::Zsh,
    };
    Prepare {
        receipt_mode: None,
        rejection_domain: None,
        asked: None,
        id: id.into(),
        host_epoch: "cd".repeat(16),
        holder: "fixture".into(),
        revision: 3,
        expected_human_seq: Some(4),
        deadline_us,
        duty: Scope::Relaunch,
        plan: NativePlan::Restart {
            process: Process {
                pid: 10,
                birth: 1,
                parent: 20,
                group: 10,
                session: 20,
                image: image.clone(),
            },
            observation: None,
            shell: Process {
                pid: 20,
                birth: 30,
                parent: 1,
                group: 20,
                session: 20,
                image: image.clone(),
            },
            launcher: image,
            socket: at!("/fixture/control.sock").into(),
            forward: Box::new(command),
            cold: None,
            rollback: None,
        },
        boot: None,
        custody: None,
    }
}

/// A session on `main` walled and moved onto `work` (the pool's canary, owned by this exact
/// receipt), whose root signalled its source, appended one continuation and ended with `reason`;
/// when `attached`, that continuation's launcher attached first and never registered.
fn ended(
    name: &str,
    reason: renewal::CancellationKind,
    attached: bool,
) -> (World, crate::harness::cli::Env, Receipt) {
    let (world, reader) = fixture(name);
    let source = Identity::from_native("source", "user");
    let proposal = {
        let mut session = weekly_session(&world);
        session.sid = "s-one";
        codex_rotation::prepare(
            &world.seat(),
            &session,
            &source,
            &Fresh(&reader),
            Duration::from_secs(60),
        )
        .unwrap()
        .unwrap()
    };
    let mut receipt = root(&proposal, "a", "00000000-0000-0000-0000-000000000002");
    let original = recipe(&receipt.id, 120_000_000);
    receipt.native_prepare = NativeRequest::prepare(original.clone()).encode().unwrap();
    proposal.commit(&receipt, NOW).unwrap();
    native_auth(&receipt.old_auth, "source");
    native_auth(&receipt.new_auth, "target");
    let mut env = crate::harness::cli::Env::from_process().unwrap();
    env.home = Some(world.home.clone());
    env.state = world.state.clone();
    env.config = Some(world.cfg.join("aterm.toml"));
    env.now = NOW;
    receipt.phase = Phase::Committing;
    receipt.launch_attempted = true;
    let continuation = Prepare {
        id: "e".repeat(32),
        revision: 4,
        expected_human_seq: Some(5),
        deadline_us: 240_000_000,
        ..original
    };
    let retired = renewal::with_receipt_writer(&env, |writer| {
        renewal::write_receipt(&env, &receipt)?;
        let attempt = writer.propose(&receipt, &receipt.id, &continuation)?;
        let mut published = writer.publish(&receipt, &attempt)?;
        if attached {
            let NativePlan::Restart { shell, .. } = &continuation.plan else {
                unreachable!("a restart recipe");
            };
            let launcher = Process {
                pid: 40,
                birth: 41,
                parent: shell.pid,
                group: 40,
                session: shell.session,
                image: shell.image.clone(),
            };
            published.candidate = Some(renewal::Candidate::of(&launcher, true));
            renewal::write_receipt(&env, &published)?;
        }
        writer.cancel(&published, reason, 0, 0)?;
        renewal::read_receipt(&env, &receipt.id).ok_or("retired")
    })
    .unwrap()
    .unwrap();
    assert_eq!(retired.phase, Phase::Retired);
    assert!(retired.revoked && retired.current_attempt.is_some());
    assert_eq!(retired.candidate.is_some(), attached);
    (world, env, retired)
}

/// Where a session on `main` that walls at `at` is moved, as the pool records stand.
fn forecast(world: &World, at: i64) -> Option<String> {
    let roster = accounts::Roster::load_for(
        Cli::Codex,
        &world.cfg.join(Cli::Codex.accounts().roster_file),
    )
    .unwrap();
    let enrolled = accounts::enrolled_in(Cli::Codex, &roster, &world.home);
    let state = world.seat().store(Cli::Codex).load(at).unwrap().0;
    crate::harness::accounts_pool::native_quota_target(
        (Cli::Codex, Mode::Early),
        (&roster, &state, &enrolled),
        roster.row("main").unwrap(),
        (None, &world.home),
        at,
    )
}

/// The finding's wedge: once the canary's hold lapses, every later wall in the pool is moved back
/// onto the destination this root's successor never ran on, and no other row is ever chosen. The
/// root's closure strikes that canary as a failed move: the pool is back on `main`, `work` is
/// held off, and the forecast no longer names it. Its deduplication record is retired in the
/// same account write. CONTROL: the committed records, unclosed, forecast `work` again. A last
/// budget whose launcher attached and exited unregistered closes the same way.
#[test]
fn an_unattached_rotation_strikes_its_own_canary_and_retires_its_record() {
    for (name, attached) in [
        ("native-unattached-strike", false),
        ("native-unattached-launched", true),
    ] {
        unattached_strikes(name, attached);
    }
}

fn unattached_strikes(name: &str, attached: bool) {
    let (world, env, retired) = ended(
        name,
        renewal::CancellationKind::SuccessorNeverAttached,
        attached,
    );
    let store = world.seat().store(Cli::Codex);
    let lapsed = NOW + accounts::CANARY_HOLD_S + 1;
    let before = store.load(NOW).unwrap().0;
    assert_eq!(before.active.as_deref(), Some("work"));
    assert_eq!(
        before.canary.as_ref().unwrap().native_receipt.as_deref(),
        Some(retired.id.as_str())
    );
    assert_eq!(
        forecast(&world, lapsed).as_deref(),
        Some("work"),
        "CONTROL: an unclosed root's canary wedges the pool onto its destination"
    );

    let removed = renewal::with_receipt_writer(&env, |writer| {
        writer.retire_unattached_rotation(&retired, &Closure(true))
    })
    .unwrap()
    .unwrap();
    assert!(removed);
    let after = store.load(NOW).unwrap().0;
    assert_eq!(after.active.as_deref(), Some("main"));
    assert!(
        after.proven,
        "the pool is back on the proven account it left"
    );
    assert!(after.canary.is_none());
    assert!(
        after.accounts["work"]
            .walls
            .contains_key(accounts::STRUCK_WINDOW)
    );
    assert_eq!(after.rotations, before.rotations, "the move stays charged");
    assert!(
        !store
            .load_logins(NOW)
            .unwrap()
            .0
            .native_rotations
            .contains_key(&retired.id)
    );
    assert_eq!(forecast(&world, lapsed), None);

    let again = renewal::with_receipt_writer(&env, |writer| {
        writer.retire_unattached_rotation(&retired, &Closure(true))
    })
    .unwrap()
    .unwrap();
    assert!(!again, "a repeated closure finds its record retired");
    assert_eq!(store.load(NOW).unwrap().0, after);
}

/// Neither an unproven closure nor another cancellation's reason releases the pool: the canary,
/// the pool's active account and the record all stay as the move left them.
#[test]
fn an_unattached_rotation_closes_only_on_its_own_reason_and_the_hosts_proof() {
    for (name, reason, proven, refusal) in [
        (
            "native-unattached-unproven",
            renewal::CancellationKind::SuccessorNeverAttached,
            false,
            "rotation-not-terminal",
        ),
        (
            "native-unattached-owner-stop",
            renewal::CancellationKind::OwnerStop,
            true,
            "unattached-rotation-not-terminal",
        ),
    ] {
        let (world, env, retired) = ended(name, reason, false);
        let store = world.seat().store(Cli::Codex);
        let state = store.load(NOW).unwrap().0.to_json();
        let logins = store.load_logins(NOW).unwrap().0.to_json();
        assert_eq!(
            renewal::with_receipt_writer(&env, |writer| {
                writer.retire_unattached_rotation(&retired, &Closure(proven))
            }),
            Err(refusal),
            "{name}"
        );
        assert_eq!(store.load(NOW).unwrap().0.to_json(), state, "{name}");
        assert_eq!(
            store.load_logins(NOW).unwrap().0.to_json(),
            logins,
            "{name}"
        );
    }
}

/// The closure strikes only the canary its own root still owns. Where the pool has moved on
/// since (another session took the canary once its hold lapsed, the account it moved to was
/// proven, or a person pinned it), the pool is left exactly as it stands and only the root's
/// record is retired: no strike rolls `active` back, and no struck wall is added.
#[test]
fn an_unattached_rotation_leaves_a_pool_that_moved_on_and_retires_only_its_record() {
    for (name, moved_on) in [
        (
            "native-unattached-taken",
            (|state: &mut accounts::State| {
                let held = state.canary.clone();
                assert!(
                    accounts::take_canary(
                        state,
                        held.as_ref(),
                        "s-two",
                        NOW + accounts::CANARY_HOLD_S + 1
                    )
                    .is_some(),
                    "another session takes the lapsed canary"
                );
            }) as fn(&mut accounts::State),
        ),
        ("native-unattached-proven", |state| {
            state.proven = true;
        }),
        ("native-unattached-pinned", |state| {
            state.pinned = true;
        }),
    ] {
        let (world, env, retired) = ended(
            name,
            renewal::CancellationKind::SuccessorNeverAttached,
            false,
        );
        let store = world.seat().store(Cli::Codex);
        let mut state = store.load(NOW).unwrap().0;
        moved_on(&mut state);
        store.save(&state).unwrap();
        let before = store.load(NOW).unwrap().0;
        let removed = renewal::with_receipt_writer(&env, |writer| {
            writer.retire_unattached_rotation(&retired, &Closure(true))
        })
        .unwrap()
        .unwrap();
        assert!(removed, "{name}: the record is retired");
        let after = store.load(NOW).unwrap().0;
        assert_eq!(after.active, before.active, "{name}");
        assert_eq!(after.canary, before.canary, "{name}");
        assert_eq!((after.proven, after.pinned), (before.proven, before.pinned));
        assert!(
            after
                .accounts
                .get("work")
                .is_none_or(|work| !work.walls.contains_key(accounts::STRUCK_WINDOW)),
            "{name}: nothing is struck"
        );
        assert!(
            !store
                .load_logins(NOW)
                .unwrap()
                .0
                .native_rotations
                .contains_key(&retired.id),
            "{name}"
        );
    }
}
