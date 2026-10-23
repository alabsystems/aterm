// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Conformance tests for `NativeUpdateSuccessorWarmBeforeClaim`.

#[cfg(target_os = "macos")]
use super::tests::StagedHandoff;
use super::tests::{ENV_LOCK, RestoreVar, env_alone, pipe_pair, stage_outgoing_handoff};
use super::*;
use crate::handoff_env::{HANDOFF_ENV_KEYS, HandoffEnv};
use aterm_models_update::native_update_successor_warm_before_claim_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
use std::sync::PoisonError;

#[cfg(target_os = "macos")]
const WARMING: i64 = 1;
#[cfg(target_os = "macos")]
const DIALLED: i64 = 2;
const CLAIMED: i64 = 3;
#[cfg(target_os = "macos")]
const ADOPTED: i64 = 4;
#[cfg(target_os = "macos")]
const EXITED: i64 = 7;

/// What the harness observed of the real successor, in the model's terms.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Observed {
    phase: i64,
    captured: bool,
    threads_spawned: bool,
    env_mut_after_spawn: bool,
    owned_taken: bool,
    claim_ok: bool,
    /// The warm prologue's main-thread section returned.
    warm_done: bool,
    /// This launch's control socket is an explicit relative path.
    relative_socket: bool,
    /// The backend was built at a size the carry contradicts.
    stale_build: bool,
    /// The outgoing process parked under a config edit the early warm never read.
    stale_config: bool,
}

/// Project an observation onto the model's state; the window, the warm present and the hint keep
/// the model's initial values (the walk is headless).
pub(crate) fn project(model: &Model, seen: Observed) -> State {
    let mut state = model.init_state();
    state.insert("phase", seen.phase);
    state.insert("captured", i64::from(seen.captured));
    state.insert("threads_spawned", i64::from(seen.threads_spawned));
    state.insert("env_mut_after_spawn", i64::from(seen.env_mut_after_spawn));
    state.insert("owned_taken", i64::from(seen.owned_taken));
    state.insert("claim_ok", i64::from(seen.claim_ok));
    state.insert("warm_done", i64::from(seen.warm_done));
    state.insert("relative_socket", i64::from(seen.relative_socket));
    state.insert("build_size_ok", i64::from(!seen.stale_build));
    state.insert("config_current", i64::from(!seen.stale_config));
    state
}

/// The model must admit `prev -> next` by exactly `action`, and every
/// invariant must hold at `next`.
#[cfg(target_os = "macos")]
fn step(model: &Model, prev: Observed, next: Observed, action: &str) {
    let (from, to) = (project(model, prev), project(model, next));
    crate::conformance_support::assert_transition(model, &from, &to, action);
}

/// No handoff name is left in the process environment.
fn env_is_clear() -> bool {
    HANDOFF_ENV_KEYS
        .iter()
        .all(|key| std::env::var_os(key).is_none())
}

/// A canary under every handoff name, planted once a thread exists.
fn plant_canaries() -> Vec<(&'static str, String)> {
    HANDOFF_ENV_KEYS
        .iter()
        .map(|key| {
            let canary = format!("canary-{key}");
            aterm_log::env::set(key, &canary);
            (key.name(), canary)
        })
        .collect()
}

/// Whether any canary moved: the process environment was mutated under a
/// handoff name after the spawn.
fn canaries_moved(planted: &[(&'static str, String)]) -> bool {
    planted
        .iter()
        .any(|(key, canary)| std::env::var_os(key).as_deref() != Some(canary.as_ref()))
}

/// Save every name the walk touches, restored on drop (declare after the lock).
fn restore_all() -> Vec<RestoreVar> {
    ["XDG_RUNTIME_DIR", "HOME"]
        .into_iter()
        .chain(HANDOFF_ENV_KEYS.iter().map(|key| key.name()))
        .map(RestoreVar::new)
        .collect()
}

/// A real thread, alive until the returned sender drops or sends.
fn spawn_warm_thread() -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let warm = std::thread::spawn(move || {
        let _ = stopped.recv();
    });
    (stop, warm)
}

fn this_build() -> String {
    encode_target_identity(
        crate::build_info::build_number()
            .parse::<u64>()
            .unwrap_or(0),
        crate::build_info::git_commit(),
    )
}

/// [`pipe_pair`] with both ends owned. The fork-lane intake adopts the ends it is handed (the ready
/// write end, the commit read end) and closes them on drop, so a test publishes those with
/// `into_raw_fd()` and drops the others: each number has one closer.
fn owned_pipe(label: &str) -> (OwnedFd, OwnedFd) {
    let (read, write) = pipe_pair(label);
    // SAFETY: Pipe(2) just returned both numbers to this thread; nothing else holds them.
    unsafe { (OwnedFd::from_raw_fd(read), OwnedFd::from_raw_fd(write)) }
}

/// The launched lane end to end over the snapshot: capture, a thread, the real dial and grant, the
/// claim installed into the snapshot, the intake and both channels admitted, and no
/// process-environment access after the spawn.
#[cfg(target_os = "macos")]
#[test]
fn the_launched_lane_claims_into_the_snapshot_and_adopts_without_touching_the_environment() {
    use std::os::fd::BorrowedFd;
    let Some(_env) = env_alone() else { return };
    let _restore = restore_all();
    let model = native_update_successor_warm_before_claim_model();
    let staged = stage_outgoing_handoff("p1-launched", 2, None);
    let entries = decode_fds_bounded(&staged.fds_wire)
        .expect("the fixture's wire")
        .entries;
    let rendezvous =
        crate::handoff_rendezvous::Rendezvous::bind(&staged.nonce).expect("bind the rendezvous");
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    // SAFETY: `getpid` has no preconditions.
    let own = unsafe { libc::getpid() };
    let parent = AttestedParent::attest_live_for_test(own).expect("this process attests");

    for key in HANDOFF_ENV_KEYS {
        aterm_log::env::unset(key);
    }
    aterm_log::env::set(ENV_MANIFEST, &staged.manifest_path);
    aterm_log::env::set(ENV_NONCE, &staged.nonce);
    aterm_log::env::set(
        ENV_LAYOUT,
        staged.manifest_path.with_extension("layout.toml"),
    );
    aterm_log::env::set(ENV_TARGET, this_build());
    aterm_log::env::set(ENV_RENDEZVOUS, rendezvous.path());
    aterm_log::env::set(ENV_CLAIM, rendezvous.claim());
    aterm_log::env::set(crate::handoff_rendezvous::ENV_PROOF_TERM, "device");
    aterm_log::env::set(crate::control_socket_identity::ENV_IDENTITY, "");

    let launched = Observed::default();
    let mut env = HandoffEnv::capture();
    let captured = Observed {
        phase: WARMING,
        captured: env_is_clear() && env.present(ENV_MANIFEST) && env.present(ENV_RENDEZVOUS),
        ..launched
    };
    step(&model, launched, captured, "Capture");

    let (stop, warm) = spawn_warm_thread();
    let planted = plant_canaries();
    let warming = Observed {
        threads_spawned: warm.thread().id() != std::thread::current().id(),
        ..captured
    };
    step(&model, captured, warming, "SpawnWarm");
    let warming = {
        let done = Observed {
            warm_done: true,
            ..warming
        };
        step(&model, warming, done, "WarmDone");
        done
    };

    assert_eq!(
        crate::handoff_rendezvous::take_device_proof_term(&mut env),
        Some(true),
        "the proof term is read from the snapshot"
    );
    let nonce = env.string(ENV_NONCE).expect("the nonce is in the snapshot");
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let claimed = std::thread::scope(|scope| {
        let outgoing = scope.spawn(|| {
            let peer = rendezvous.accept_claim(
                Some(own),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                deadline,
                &|| false,
            )?;
            // SAFETY: Descriptors this test owns for the length of the call.
            let sessions = entries
                .iter()
                .map(|(id, fd, pid)| (*id, *pid, unsafe { BorrowedFd::borrow_raw(*fd) }))
                .collect::<Vec<_>>();
            // SAFETY: As above.
            let (ready, commit) = unsafe {
                (
                    BorrowedFd::borrow_raw(ready_write),
                    BorrowedFd::borrow_raw(commit_read),
                )
            };
            let granted = peer.transfer(&staged.nonce, &sessions, ready, commit, deadline, None);
            Ok::<_, crate::handoff_rendezvous::RendezvousError>((peer, granted))
        });
        let claimed = crate::handoff_rendezvous::claim_incoming(
            &mut env,
            &nonce,
            crate::handoff_rendezvous::ClaimDeadlines {
                dial: deadline,
                grant: deadline + aterm_spec::wait::MUST_HAPPEN,
            },
            Some(parent),
        );
        let (peer, granted) = crate::handoff_rendezvous::admitted(
            outgoing.join().expect("the outgoing half"),
            "the successor presents its claim",
        );
        assert!(granted.is_ok(), "the grant: {granted:?}");
        drop(peer);
        claimed.expect("the claim is granted")
    });
    let dialled = Observed {
        phase: DIALLED,
        env_mut_after_spawn: canaries_moved(&planted),
        ..warming
    };
    assert!(
        !env.present(ENV_RENDEZVOUS) && !env.present(ENV_CLAIM),
        "the dial consumed the rendezvous from the snapshot"
    );
    step(&model, warming, dialled, "Dial");
    assert!(
        !warm.is_finished(),
        "the dial never waited on the warm thread (JoinWorkerBeforeDial)"
    );

    assert_eq!(claimed.session_count(), 2);
    claimed.into_intake().install(&mut env);
    let claim_ok = Observed {
        phase: CLAIMED,
        claim_ok: [ENV_FDS, ENV_READY_FD, ENV_COMMIT_FD]
            .iter()
            .all(|key| env.present(*key)),
        env_mut_after_spawn: canaries_moved(&planted),
        ..dialled
    };
    step(&model, dialled, claim_ok, "ClaimOk");

    assert!(incoming_offered_in(&env));
    assert!(
        staged.manifest_path.exists(),
        "nothing owned before the intake"
    );
    let incoming = take_incoming_from(&mut env);
    let owned = Observed {
        owned_taken: incoming.adopted.len() == 2 && !staged.manifest_path.exists(),
        env_mut_after_spawn: canaries_moved(&planted),
        ..claim_ok
    };
    step(&model, claim_ok, owned, "TakeOwned");

    let target = take_target_identity_from(&mut env).expect("this build is the target");
    let adopted_fds = incoming
        .adopted
        .iter()
        .map(|adopted| adopted.master.raw())
        .collect::<Vec<_>>();
    let ready = take_ready_fd_from(
        &mut env,
        incoming.nonce.clone(),
        incoming.layout_digest,
        incoming.screen_digest,
        Some(target),
        &adopted_fds,
    )
    .expect("the readiness channel is admitted");
    let commit = take_commit_fd_from(
        &mut env,
        incoming.nonce.clone(),
        &adopted_fds,
        Some(ready.raw_fd()),
        Some(parent),
    );
    assert!(commit.is_some(), "the Commit channel is admitted");
    assert!(
        crate::control_socket_identity::consume_incoming_from(&mut env) == Ok(None),
        "the explicit empty witness"
    );
    let adopted = Observed {
        phase: ADOPTED,
        env_mut_after_spawn: canaries_moved(&planted),
        ..owned
    };
    step(&model, owned, adopted, "Adopt");
    assert!(
        env.exec_env().is_empty(),
        "every handoff name was consumed from the snapshot"
    );
    assert!(
        !canaries_moved(&planted),
        "and the process environment was never touched"
    );

    drop((ready, commit, incoming));
    let _ = stop.send(());
    warm.join().expect("the warm thread");
    drop(rendezvous);
    for (_, fd, _) in entries {
        aterm_pty::close_fd(fd);
    }
    for fd in [ready_read, ready_write, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    StagedHandoff::teardown(staged);
}

/// A failed claim leaves nothing: nobody listens at the rendezvous, the real dial fails, and the
/// outgoing process's manifest is untouched, with nothing owned and nothing shown.
#[cfg(target_os = "macos")]
#[test]
fn a_failed_claim_owns_nothing_and_touches_no_environment() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_all();
    let model = native_update_successor_warm_before_claim_model();
    let staged = stage_outgoing_handoff("p1-refused", 1, None);
    let path = crate::handoff_rendezvous::Rendezvous::bind(&staged.nonce)
        .expect("bind the rendezvous")
        .path()
        .to_path_buf();
    // SAFETY: `getpid` has no preconditions.
    let own = unsafe { libc::getpid() };
    let parent = AttestedParent::attest_live_for_test(own).expect("this process attests");
    for key in HANDOFF_ENV_KEYS {
        aterm_log::env::unset(key);
    }
    aterm_log::env::set(ENV_MANIFEST, &staged.manifest_path);
    aterm_log::env::set(ENV_NONCE, &staged.nonce);
    aterm_log::env::set(
        ENV_LAYOUT,
        staged.manifest_path.with_extension("layout.toml"),
    );
    aterm_log::env::set(ENV_RENDEZVOUS, &path);
    aterm_log::env::set(ENV_CLAIM, "0".repeat(64));

    let launched = Observed::default();
    let mut env = HandoffEnv::capture();
    let captured = Observed {
        phase: WARMING,
        captured: env_is_clear(),
        ..launched
    };
    step(&model, launched, captured, "Capture");
    let (stop, warm) = spawn_warm_thread();
    let planted = plant_canaries();
    let warming = Observed {
        threads_spawned: true,
        ..captured
    };
    step(&model, captured, warming, "SpawnWarm");
    let warming = {
        let done = Observed {
            warm_done: true,
            ..warming
        };
        step(&model, warming, done, "WarmDone");
        done
    };

    let soon = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let refused = crate::handoff_rendezvous::claim_incoming(
        &mut env,
        &staged.nonce,
        crate::handoff_rendezvous::ClaimDeadlines {
            dial: soon,
            grant: soon,
        },
        Some(parent),
    );
    assert!(refused.is_err(), "nobody is listening");
    let dialled = Observed {
        phase: DIALLED,
        env_mut_after_spawn: canaries_moved(&planted),
        ..warming
    };
    step(&model, warming, dialled, "Dial");
    let exited = Observed {
        phase: EXITED,
        owned_taken: !staged.manifest_path.exists(),
        env_mut_after_spawn: canaries_moved(&planted),
        ..dialled
    };
    step(&model, dialled, exited, "ClaimFail");
    assert!(
        env.present(ENV_MANIFEST) && staged.manifest_path.exists(),
        "the refused successor consumed nothing of the outgoing process's"
    );

    let _ = stop.send(());
    warm.join().expect("the warm thread");
    staged.teardown();
}

/// The fork lane: the launch environment as its parent writes it, captured, prearmed and taken
/// entirely from the snapshot after a thread exists, and the successor's proof is the parent's
/// expectation byte for byte.
#[cfg(unix)]
#[test]
fn the_fork_lane_intake_from_the_snapshot_proves_what_the_parent_expects() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_all();
    let staged = stage_outgoing_handoff("p1-fork", 3, None);
    let (ready_read, ready_write) = owned_pipe("ready");
    let (commit_read, commit_write) = owned_pipe("commit");
    for key in HANDOFF_ENV_KEYS {
        aterm_log::env::unset(key);
    }
    staged.publish_env(
        ready_write.into_raw_fd(),
        commit_read.into_raw_fd(),
        Some(this_build()),
    );

    let mut env = HandoffEnv::capture();
    assert!(env_is_clear(), "capture leaves no handoff name behind");
    let prearmed = prearm_incoming_fds_from(&mut env);
    assert!(!prearmed.rejects_boot() && prearmed.parent_pid().is_some());
    assert!(
        !env.present(ENV_PARENT_PID),
        "the parent's identity is typed state"
    );
    let exec = prearmed.final_exec_env_with(&env);
    assert!(exec.iter().any(|(key, _)| key == ENV_FDS.name()));
    assert!(exec.iter().any(|(key, _)| key == ENV_PARENT_PID.name()));

    let (stop, warm) = spawn_warm_thread();
    let planted = plant_canaries();

    let incoming = take_incoming_from(&mut env);
    assert_eq!(incoming.adopted.len(), 3, "every session is adopted");
    let target = take_target_identity_from(&mut env).expect("this build is the target");
    let adopted_fds = incoming
        .adopted
        .iter()
        .map(|adopted| adopted.master.raw())
        .collect::<Vec<_>>();
    let ready = take_ready_fd_from(
        &mut env,
        incoming.nonce.clone(),
        incoming.layout_digest,
        incoming.screen_digest,
        Some(target),
        &adopted_fds,
    )
    .expect("the readiness channel is admitted");
    let commit = take_commit_fd_from(
        &mut env,
        incoming.nonce.clone(),
        &adopted_fds,
        Some(ready.raw_fd()),
        prearmed.parent_pid(),
    );
    assert!(commit.is_some(), "the Commit channel is admitted");
    let identities = incoming
        .adopted
        .iter()
        .map(|item| (item.local_id, item.master.raw(), item.pid))
        .collect::<Vec<_>>();
    assert_eq!(
        ready.proof(&identities),
        Some(staged.expected),
        "THE SAME AUTHENTICATION: the proof over the snapshot is the parent's expectation"
    );
    assert!(env.exec_env().is_empty(), "every name consumed");
    assert!(
        !canaries_moved(&planted),
        "no process-environment access after the spawn"
    );

    drop((ready, commit, incoming, ready_read, commit_write));
    let _ = stop.send(());
    warm.join().expect("the warm thread");
    staged.teardown();
}

/// Negative control: the retired transport, which wrote the claimed descriptors into the process
/// environment (three `set_var`s), a data race once a thread is alive.
#[cfg(unix)]
#[test]
fn the_retired_env_transport_is_the_caught_mutant() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_all();
    let model = native_update_successor_warm_before_claim_model();
    let buggy = interp::with_buggy(&model, 1);
    for key in HANDOFF_ENV_KEYS {
        aterm_log::env::unset(key);
    }
    let _env_snapshot = HandoffEnv::capture();
    let (stop, warm) = spawn_warm_thread();
    let planted = plant_canaries();
    let claimed = Observed {
        phase: CLAIMED,
        captured: true,
        threads_spawned: true,
        claim_ok: true,
        env_mut_after_spawn: canaries_moved(&planted),
        ..Observed::default()
    };
    assert!(!claimed.env_mut_after_spawn);
    aterm_log::env::set(ENV_FDS, "0=9:4000");
    aterm_log::env::set(ENV_READY_FD, "10");
    aterm_log::env::set(ENV_COMMIT_FD, "11");
    let published = Observed {
        env_mut_after_spawn: canaries_moved(&planted),
        ..claimed
    };
    assert!(published.env_mut_after_spawn, "the canaries see the writes");
    let (from, to) = (project(&model, claimed), project(&model, published));
    assert_eq!(
        interp::admits(&model, &from, &to),
        None,
        "no shipping action mutates the environment after a spawn"
    );
    assert_eq!(
        interp::admits(&buggy, &from, &to),
        Some("PublishToEnv"),
        "the retired transport is the model's mutant"
    );
    assert!(!model.check_invariant("NoEnvMutationAfterSpawn", &to));
    let _ = stop.send(());
    warm.join().expect("the warm thread");
}

/// The real `warm_order` over every socket shape a launched successor can carry is the model's
/// `SpawnWarm` guard: before the dial exactly where the model lets a thread exist before the dial,
/// never on the relative shape, whose intake `chdir`s.
#[test]
fn the_real_warm_order_is_the_models_spawn_guard() {
    let model = native_update_successor_warm_before_claim_model();
    let buggy = interp::with_buggy(&model, 1);
    for explicit in [
        None,
        Some("/tmp/at.x/a.sock"),
        Some("a.sock"),
        Some("./run/a.sock"),
    ] {
        let order = crate::warm_order(true, explicit);
        let relative = explicit.is_some_and(|path| !path.starts_with('/'));
        let captured = project(
            &model,
            Observed {
                phase: CAPTURED_PHASE,
                captured: true,
                relative_socket: relative,
                ..Observed::default()
            },
        );
        assert_eq!(
            model.action_enabled("SpawnWarm", &captured),
            order == crate::WarmOrder::BeforeDial,
            "{explicit:?}: the model's guard and the real decision ({order:?}) agree"
        );
        assert_eq!(
            order == crate::WarmOrder::AfterIntake,
            relative,
            "{explicit:?}: only the relative shape keeps the late order"
        );
        if relative {
            let mut run = captured.clone();
            assert!(!model.action_enabled("WarmBeforeDialOnRelativeSocket", &run));
            assert!(buggy.fire("WarmBeforeDialOnRelativeSocket", &mut run));
            for action in ["Dial", "ClaimOk", "TakeOwned", "Adopt", "PrepareSocketDir"] {
                assert!(buggy.fire(action, &mut run), "{action}: {run:?}");
            }
            assert!(
                !model.check_invariant("NoCwdChangeAfterSpawn", &run),
                "the chdir with the warm's threads alive is the caught defect"
            );
        }
    }
    for explicit in [None, Some("/tmp/at.x/a.sock"), Some("a.sock")] {
        assert_eq!(
            crate::warm_order(false, explicit),
            crate::WarmOrder::AfterIntake
        );
    }
}

/// The phase the model enters at `Capture`.
const CAPTURED_PHASE: i64 = 1;

/// A stale hint costs a warm, never a frame: the real hint parser, build font
/// (`launch_font_for_zoom`, what the prologue builds at), launch font (`successor_font_px`, from
/// the carry) and miss decision (`warm_miss_px`) decide the model's `Reshape`/`RealPresent`
/// guards.
#[cfg(target_os = "macos")]
#[test]
fn a_stale_hint_is_reselected_before_any_carried_present() {
    use crate::handoff_warm_hint::WarmHint;
    let model = native_update_successor_warm_before_claim_model();
    let carry =
        |device: Option<(u32, u32)>, size: Option<(u32, u32)>| crate::session_store::WindowCarry {
            rows: 30,
            cols: 100,
            outer_x: None,
            outer_y: None,
            inner_px: None,
            presence_row: false,
            status_bar_rows: 0,
            messages: Default::default(),
            next_message_id: Default::default(),
            update_verified_unix_ms: None,
            font_px_milli: device.map(|(px, _)| px),
            font_reset_px_milli: device.map(|(_, reset)| reset),
            font_size_milli: size.map(|(px, _)| px),
            font_reset_size_milli: size.map(|(_, reset)| reset),
            update_health_said: Default::default(),
            messages_captured_unix_ms: None,
        };
    let zoomed = (Some((28_000, 24_000)), Some((14_000, 12_000)));
    let unzoomed = (None, None);
    let older = (Some((28_000, 24_000)), None);
    const ZOOM_HINT: &str =
        "w1;windows=1;sessions=1;cols=100;rows=30;px=28000;reset=24000;size=14000;sizereset=12000";
    let cases = [
        ("w1;windows=1;sessions=1;cols=100;rows=30", unzoomed, false),
        (ZOOM_HINT, zoomed, false),
        (
            "w1;windows=1;sessions=1;cols=100;rows=30;px=28000;reset=24000",
            older,
            false,
        ),
        ("w1;windows=1;sessions=1;cols=100;rows=30", zoomed, true),
        (ZOOM_HINT, unzoomed, true),
        (
            "w1;windows=1;sessions=1;cols=100;rows=30;px=999999;reset=24000",
            unzoomed,
            false,
        ),
        ("w9;px=28000", zoomed, true),
    ];
    for (wire, (device, size), miss_expected) in cases {
        let zoom = WarmHint::parse(wire)
            .map_or_else(crate::app_config::CarriedZoom::default, |hint| {
                hint.carried_zoom()
            });
        let built = crate::app_config::launch_font_for_zoom(12.0, zoom).build_px(None);
        let launch =
            crate::app_config::successor_font_px(12.0, Some(&carry(device, size))).build_px(None);
        let miss = crate::warm_miss_px(built, launch);
        assert_eq!(
            miss.is_some(),
            miss_expected,
            "{wire} against {device:?}/{size:?}"
        );
        if let Some(px) = miss {
            assert!(
                (px - launch).abs() < f32::EPSILON,
                "the join selects the carry's"
            );
        }
        let adopted = project(
            &model,
            Observed {
                phase: 4,
                captured: true,
                threads_spawned: true,
                owned_taken: true,
                claim_ok: true,
                warm_done: true,
                stale_build: miss.is_some(),
                ..Observed::default()
            },
        );
        assert_eq!(
            model.action_enabled("Reshape", &adopted),
            miss.is_some(),
            "{wire}: a miss is exactly what the join re-selects"
        );
        assert_eq!(
            model.action_enabled("RealPresent", &adopted),
            miss.is_none(),
            "{wire}: no carried present at a stale hint's size"
        );
        let reshaped = miss.unwrap_or(built);
        assert!(crate::warm_miss_px(reshaped, launch).is_none());
    }
}

/// A config edit during the hold is warmed again before any carried present. Between the read
/// `main_entry` takes before the early warm and `warm_config_moved` after the intake, the outgoing
/// process leaves `aterm.toml` alone, saves the same bytes, or changes the font as a Settings save
/// does (a new file renamed over the old).
#[test]
fn a_config_edit_during_the_hold_is_warmed_again_before_any_carried_present() {
    use crate::native_config_service::VersionedConfigService;
    let model = native_update_successor_warm_before_claim_model();
    let buggy = interp::with_buggy(&model, 1);
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let path = dir.path().join("aterm.toml");
    let save = |text: &str| {
        let staged = dir.path().join(".aterm.toml.save");
        std::fs::write(&staged, text).expect("stage the save");
        std::fs::rename(&staged, &path).expect("rename it over aterm.toml");
    };
    let font_px = |service: &VersionedConfigService| service.snapshot().config.font_px;
    let holds = [
        ("an idle hold", None, 14.0),
        ("the same bytes saved again", Some("font_px = 14.0\n"), 14.0),
        ("a Settings font change", Some("font_px = 16.0\n"), 16.0),
    ];
    for (hold, edit, parked_px) in holds {
        save("font_px = 14.0\n");
        let read = crate::WarmConfigRead::take(Some(&path));
        assert!(read.text.is_some(), "{hold}: the file reads");
        let warmed = VersionedConfigService::load_path(&path).expect("the warm's read");
        if let Some(text) = edit {
            save(text);
        }
        let moved = crate::warm_config_moved(&read, Some(&path));
        assert_eq!(
            moved,
            font_px(&warmed) != Some(parked_px),
            "{hold}: a re-warm exactly when the early warm read another config"
        );
        if moved {
            let rewarmed = VersionedConfigService::load_path(&path).expect("the re-warm's read");
            assert_eq!(font_px(&rewarmed), Some(parked_px), "{hold}");
        }
        let adopted = project(
            &model,
            Observed {
                phase: 4,
                captured: true,
                threads_spawned: true,
                owned_taken: true,
                claim_ok: true,
                warm_done: true,
                stale_config: moved,
                ..Observed::default()
            },
        );
        assert_eq!(model.action_enabled("Rewarm", &adopted), moved, "{hold}");
        assert_eq!(
            model.action_enabled("RealPresent", &adopted),
            !moved,
            "{hold}: no carried present under the early warm's config"
        );
        if moved {
            let mut stale = adopted.clone();
            assert!(buggy.fire("PresentUnderStaleConfig", &mut stale));
            assert!(!model.check_invariant("CarriedPresentUnderCurrentConfig", &stale));
        }
    }
}

/// A theme-file edit during the hold is warmed again too, though `aterm.toml` never moves: the
/// config names a user theme, and the theme's file is saved (renamed over) while this successor
/// holds its dial.
#[test]
fn a_theme_file_edit_during_the_hold_is_warmed_again() {
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let themes = dir.path().join("aterm").join("themes");
    std::fs::create_dir_all(&themes).expect("the theme dir");
    let path = dir.path().join("aterm").join("aterm.toml");
    std::fs::write(&path, "theme = \"hold-theme\"\n").expect("the config");
    let save_theme = |background: &str| {
        let staged = themes.join(".hold-theme.conf.save");
        std::fs::write(
            &staged,
            format!("foreground = #d0d0d0\nbackground = {background}\n"),
        )
        .expect("stage the theme");
        std::fs::rename(&staged, themes.join("hold-theme.conf")).expect("save the theme");
    };
    save_theme("#102030");
    crate::test_env::scoped("XDG_CONFIG_HOME", dir.path(), || {
        let read = crate::WarmConfigRead::take(Some(&path));
        let early = crate::warm_config(true, crate::app_config::CarriedZoom::default());
        assert_eq!(
            early.theme.bg, 0x10_2030,
            "the early warm read the theme file"
        );
        assert!(
            !crate::warm_config_moved(&read, Some(&path)),
            "an idle hold keeps the early warm"
        );
        save_theme("#405060");
        assert!(
            crate::warm_config_moved(&read, Some(&path)),
            "the theme the outgoing process parked under is not the one the early warm read"
        );
        let late = crate::warm_config(true, crate::app_config::CarriedZoom::default());
        assert_eq!(late.theme.bg, 0x40_5060, "the re-warm reads the edit");
        let _ = early.backend_handle.join().expect("the early worker");
        let _ = late.backend_handle.join().expect("the re-warm's worker");
    });
}

/// The hint never reaches the proof: the real intake over a snapshot with no hint, a stale one and
/// a hostile one computes the proof the outgoing process expects, every time.
#[cfg(target_os = "macos")]
#[test]
fn the_hint_never_reaches_the_adoption_proof() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_all();
    let hint_key = crate::handoff_warm_hint::ENV_WARM_HINT;
    for (label, hint) in [
        ("absent", None),
        (
            "stale",
            Some("w1;windows=3;sessions=9;cols=80;rows=24;px=40000;reset=12000"),
        ),
        (
            "hostile",
            Some("w1;windows=4294967295;sessions=0;cols=1;rows=1;px=0;reset=0"),
        ),
    ] {
        let staged = stage_outgoing_handoff(&format!("p2-hint-{label}"), 2, None);
        let (ready_read, ready_write) = owned_pipe("ready");
        let (commit_read, commit_write) = owned_pipe("commit");
        for key in HANDOFF_ENV_KEYS {
            aterm_log::env::unset(key);
        }
        staged.publish_env(
            ready_write.into_raw_fd(),
            commit_read.into_raw_fd(),
            Some(this_build()),
        );
        if let Some(hint) = hint {
            aterm_log::env::set(hint_key, hint);
        }
        let mut env = HandoffEnv::capture();
        assert!(env_is_clear(), "{label}: the hint left the process too");
        let prearmed = prearm_incoming_fds_from(&mut env);
        assert!(!prearmed.rejects_boot(), "{label}");
        let taken = crate::handoff_warm_hint::take_from(&mut env);
        assert_eq!(
            taken.is_ok_and(|hint| hint.is_some()),
            hint.is_some(),
            "{label}"
        );
        let incoming = take_incoming_from(&mut env);
        assert_eq!(incoming.adopted.len(), 2, "{label}");
        let target = take_target_identity_from(&mut env).expect("this build is the target");
        let adopted_fds = incoming
            .adopted
            .iter()
            .map(|adopted| adopted.master.raw())
            .collect::<Vec<_>>();
        let identities = incoming
            .adopted
            .iter()
            .map(|item| (item.local_id, item.master.raw(), item.pid))
            .collect::<Vec<_>>();
        if let (Some(hint), Some(layout), Some(screen)) =
            (hint, incoming.layout_digest, incoming.screen_digest)
        {
            let mut folded = layout;
            for (at, byte) in hint.bytes().enumerate() {
                folded[at % folded.len()] ^= byte;
            }
            assert_ne!(
                adoption_proof(
                    incoming.nonce.as_deref().expect("the nonce"),
                    target.build,
                    &target.commit,
                    &folded,
                    &screen,
                    &identities,
                ),
                Ok(staged.expected),
                "{label}: a hint folded into the digest proves something else"
            );
        }
        let ready = take_ready_fd_from(
            &mut env,
            incoming.nonce.clone(),
            incoming.layout_digest,
            incoming.screen_digest,
            Some(target),
            &adopted_fds,
        )
        .expect("the readiness channel is admitted");
        assert_eq!(
            ready.proof(&identities),
            Some(staged.expected),
            "{label}: the proof is the parent's expectation whatever the hint said"
        );
        let commit = take_commit_fd_from(
            &mut env,
            incoming.nonce.clone(),
            &adopted_fds,
            Some(ready.raw_fd()),
            prearmed.parent_pid(),
        );
        assert!(commit.is_some(), "{label}");
        drop((ready, commit, incoming, ready_read, commit_write));
        staged.teardown();
    }
    let model = native_update_successor_warm_before_claim_model();
    let adopted = Observed {
        phase: 4,
        captured: true,
        threads_spawned: true,
        owned_taken: true,
        claim_ok: true,
        warm_done: true,
        ..Observed::default()
    };
    let from = project(&model, adopted);
    let mut to = from.clone();
    to.insert("hint_in_digest", 1);
    assert_eq!(interp::admits(&model, &from, &to), None);
    assert_eq!(
        interp::admits(&interp::with_buggy(&model, 1), &from, &to),
        Some("FoldHint")
    );
    assert!(!model.check_invariant("HintNotInProof", &to));
}

/// A FORK-LANE SUCCESSOR MAKES ROOM FOR THE SESSIONS IT ADOPTS (
/// U9).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_fork_lane_successor_makes_room_for_the_sessions_it_adopts() {
    /// Sessions handed over: their masters and slaves (this process is both
    /// halves) fit the 96 spare, and their adoption does not.
    const SESSIONS: usize = 32;
    if !crate::control_auth::enter_low_nofile_test_child(96) {
        return;
    }
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_all();
    let staged = stage_outgoing_handoff("u9-fork-room", SESSIONS, None);
    let (ready_read, ready_write) = owned_pipe("ready");
    let (commit_read, commit_write) = owned_pipe("commit");
    for key in HANDOFF_ENV_KEYS {
        aterm_log::env::unset(key);
    }
    staged.publish_env(
        ready_write.into_raw_fd(),
        commit_read.into_raw_fd(),
        Some(this_build()),
    );
    let mut env = HandoffEnv::capture();
    let prearmed = prearm_incoming_fds_from(&mut env);
    assert!(!prearmed.rejects_boot() && prearmed.parent_pid().is_some());
    crate::descriptor_limit::settle_incoming_descriptor_limit(&env);
    assert_eq!(
        crate::descriptor_limit::park_raised_from(),
        crate::descriptor_limit::launcher_soft_descriptor_limit(),
        "a forked successor's own raise is recorded as a park's, from the launcher's limit"
    );
    let incoming = take_incoming_from(&mut env);
    assert_eq!(incoming.adopted.len(), SESSIONS, "every session is adopted");

    let mut pipes = Vec::new();
    #[cfg_attr(not(target_vendor = "apple"), allow(unused_mut))]
    let mut witnesses: Vec<aterm_uds::exitwatch::ExitWatch> = Vec::new();
    for index in 0..incoming.adopted.len() {
        let mut fds = [0i32; 2];
        // SAFETY: Plain pipe(2) into a valid 2-slot out-array.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            panic!(
                "adopted session {index} of {SESSIONS}: its wake pipe could not open: {}",
                std::io::Error::last_os_error()
            );
        }
        // SAFETY: Pipe(2) just returned both numbers; nothing else holds them.
        pipes.push(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) });
        #[cfg(target_vendor = "apple")]
        {
            let Some(witness) = aterm_uds::exitwatch::ExitWatch::watch(std::process::id()) else {
                panic!(
                    "adopted session {index} of {SESSIONS}: its exit witness could not open: {}",
                    std::io::Error::last_os_error()
                );
            };
            witnesses.push(witness);
        }
    }
    drop((pipes, witnesses, incoming, ready_read, commit_write));
    staged.teardown();
}
