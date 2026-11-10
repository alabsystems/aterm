// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::{
    accounts::LOGIN_ENV,
    accounts_pool::ExecutionIntent,
    contract::Scope,
    native_act::{Image, Process, RestartCommand},
    upgrade::{Agent, Dialect, Phase},
    upgrade_drive::{HarnessToken, Instances, Tell},
};
use aterm_spec::interp::State;

#[path = "native_record_tests.rs"]
mod record_tests;

fn intent(login: &str, generation: &str) -> ExecutionIntent {
    ExecutionIntent::Stored {
        namespace: "12".repeat(32),
        login: Some(login.into()),
        owner: Some("account".into()),
        generation: generation.repeat(32),
    }
}

/// [`laid_out`] of `phase`, made once: the same inputs, and each caller changes its own copy.
fn fixture(phase: ImagePhase) -> (St, Prepare) {
    static FIXTURES: std::sync::LazyLock<[(St, Prepare); 2]> = std::sync::LazyLock::new(|| {
        [
            laid_out(ImagePhase::Forward),
            laid_out(ImagePhase::Rollback),
        ]
    });
    FIXTURES[usize::from(phase == ImagePhase::Rollback)].clone()
}

fn laid_out(phase: ImagePhase) -> (St, Prepare) {
    let image = Image {
        path: b"/model/claude".to_vec(),
        device: 1,
        inode: 2,
    };
    let process = Process {
        pid: 42,
        birth: 3,
        parent: 43,
        group: 42,
        session: 43,
        image: image.clone(),
    };
    let command = RestartCommand {
        image: image.clone(),
        program: "/model/claude".into(),
        argv: vec!["--resume".into(), "original-conversation".into()],
        env: vec![(LOGIN_ENV.into(), "/model/old".into())],
        unset: None,
        cwd: Some("/model/project".into()),
        heal: None,
        dialect: Dialect::Zsh,
        settles: None,
        account_intent: Some(Box::new(intent("/model/old", "34"))),
    };
    let mut back = command.clone();
    back.argv.push("--rollback".into());
    back.env[0].1 = "/model/back".into();
    back.account_intent = Some(Box::new(intent("/model/back", "78")));
    let old = Prepare {
        rejection_domain: None,
        asked: None,
        id: "ab".repeat(16),
        host_epoch: "cd".repeat(16),
        holder: "model-holder".into(),
        revision: 1,
        expected_human_seq: Some(0),
        deadline_us: 100,
        duty: Scope::Relaunch,
        plan: NativePlan::Restart {
            process: process.clone(),
            observation: None,
            shell: Process {
                pid: 43,
                parent: 2,
                group: 43,
                ..process
            },
            launcher: image,
            socket: "/model/control.sock".into(),
            forward: Box::new(command.clone()),
            cold: None,
            rollback: Some(Box::new(back.clone())),
        },
        boot: None,
        custody: None,
    };
    let active = if phase == ImagePhase::Forward {
        &command
    } else {
        &back
    };
    let st = St {
        agent: Agent::Claude,
        phase: Phase::Exiting { at_s: 7 },
        pending_since: 3,
        salt: 4,
        native_coordination: true,
        native_retired_unissued: true,
        native_prepare: Request::prepare(old.clone()).encode().unwrap(),
        native: Some(Binding {
            id: old.id.clone(),
            capability: "ef".repeat(32),
        }),
        native_rollback: phase == ImagePhase::Rollback,
        line: active.legacy_line().unwrap(),
        back_line: if phase == ImagePhase::Forward {
            back.legacy_line().unwrap()
        } else {
            String::new()
        },
        account_intent: active.account_intent.as_deref().cloned(),
        back_account_intent: if phase == ImagePhase::Forward {
            back.account_intent.as_deref().cloned()
        } else {
            None
        },
        from: "1.0.1".into(),
        to: "1.0.2".into(),
        source: "native".into(),
        ..St::default()
    };
    let next = old
        .with_account_successor(
            phase,
            "01".repeat(16),
            2,
            1,
            200,
            intent("/model/new", "56"),
        )
        .unwrap();
    (st, next)
}

fn opts(path: &std::path::Path) -> Opts {
    Opts {
        home: path.join("home"),
        state: path.join("state"),
        account_roster: None,
        sock: None,
        token: Some(HarnessToken::new("fixture-token")),
        only_sid: None,
        dry_run: false,
        human_grace_s: 0,
        hand_back: false,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        timings: crate::harness::Timings::FAST,
        tell: Tell::Off,
        press: None,
    }
}

fn without_candidate(st: &St) -> St {
    let mut old = st.clone();
    old.native_account_candidate = None;
    old.progress_at = 0;
    old.progress_key.clear();
    old
}

#[test]
fn both_slots_survive_a_lost_directory_ack_and_exact_retry_in_either_phase() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let root = aterm_tempfile::tempdir().unwrap();
        let opts = opts(root.path());
        let (mut st, candidate) = fixture(phase);
        let old = st.clone();
        upgrade_drive::save_checked(&opts, "conversation", &st).unwrap();
        let fault = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
        assert_eq!(
            stage(&opts, "conversation", &mut st, &candidate, phase),
            Err("native-unsaved")
        );
        assert_eq!(without_candidate(&st), old);
        // The file was renamed before its directory acknowledgement failed.
        let mut loaded = upgrade_drive::load(&opts, "conversation").unwrap();
        assert!(loaded.native_account_candidate.is_some());
        assert_eq!(without_candidate(&loaded), old);
        assert_eq!(retry(&opts, "conversation", &loaded), Err("native-unsaved"));
        let mut rival = candidate.clone();
        rival.id = "02".repeat(16);
        let exact = loaded.clone();
        assert_eq!(
            stage(&opts, "conversation", &mut loaded, &rival, phase),
            Err("native-account-candidate-pending")
        );
        assert_eq!(loaded, exact);
        drop(fault);
        let saved = retry(&opts, "conversation", &loaded).unwrap();
        assert_eq!(*saved.prepare, candidate);
        assert_eq!(saved.predecessor, *old.native.as_ref().unwrap());
        assert_eq!(saved.phase, phase);
        assert_eq!(without_candidate(&loaded), old);
    }
}

#[test]
fn staging_refuses_changed_source_phase_recipe_and_dry_run_without_mutation() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut opts = opts(root.path());
    let (seed, candidate) = fixture(ImagePhase::Forward);
    for defect in 0..6 {
        let mut st = seed.clone();
        let mut candidate = candidate.clone();
        match defect {
            0 => st.native_retired_unissued = false,
            1 => st.native.as_mut().unwrap().id = "03".repeat(16),
            2 => st.native_rollback = true,
            3 => st.line.push(' '),
            4 => st.account_intent = None,
            5 => candidate.holder.push_str("-changed"),
            _ => unreachable!(),
        }
        let before = st.clone();
        assert!(
            stage(
                &opts,
                "conversation",
                &mut st,
                &candidate,
                ImagePhase::Forward
            )
            .is_err()
        );
        assert_eq!(st, before);
    }
    opts.dry_run = true;
    let mut st = seed.clone();
    assert_eq!(
        stage(
            &opts,
            "conversation",
            &mut st,
            &candidate,
            ImagePhase::Forward
        ),
        Err("native-dry-run")
    );
    assert_eq!(st, seed);
    assert!(!opts.state.exists());
}

#[test]
fn unknown_candidate_schema_survives_reload_and_cannot_revive_native_fallback() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let (seed, _) = fixture(ImagePhase::Forward);
    for raw in [
        Value::Null,
        Value::from("future schema"),
        Value::Object(aterm_json::Map::new()),
    ] {
        let mut wire: Value = aterm_json::from_str(&seed.to_json()).unwrap();
        let fields = wire.as_object_mut().unwrap();
        fields.insert("native_account_candidate".into(), raw.clone());
        fields.insert("native_retired_unissued".into(), Value::Bool(false));
        fields.insert("native_coordination".into(), Value::Bool(false));
        let loaded = St::from_json(&aterm_json::to_string(&wire).unwrap()).unwrap();
        assert!(
            loaded.native_retired_unissued && super::super::native_restart::coordinated(&loaded)
        );
        assert_eq!(
            loaded.native_account_candidate.as_ref().unwrap().value(),
            &raw
        );
        assert_eq!(
            retry(&opts, "conversation", &loaded),
            Err("native-account-candidate")
        );
        let mut replacement = St::default();
        loaded.kept_by_restart("different-tab", &mut replacement);
        assert_eq!(replacement, loaded);
        assert_eq!(St::from_json(&loaded.to_json()), Some(loaded));
    }
}

#[test]
fn stale_snapshots_cannot_replace_remove_or_restore_over_a_pending_candidate() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let (mut st, candidate) = fixture(ImagePhase::Forward);
    let old = st.clone();
    stage(
        &opts,
        "conversation",
        &mut st,
        &candidate,
        ImagePhase::Forward,
    )
    .unwrap();
    let saved = upgrade_drive::load(&opts, "conversation").unwrap();
    let mut rival = candidate.clone();
    rival.id = "02".repeat(16);
    let mut stale = old.clone();
    assert_eq!(
        stage(
            &opts,
            "conversation",
            &mut stale,
            &rival,
            ImagePhase::Forward
        ),
        Err("native-unsaved")
    );
    assert_eq!(retry(&opts, "conversation", &stale), Err("native-unsaved"));
    assert!(upgrade_drive::save_checked(&opts, "conversation", &old).is_err());
    upgrade_drive::save(&opts, "conversation", &old);
    upgrade_drive::restore(&opts, "conversation", Some(&old));
    upgrade_drive::restore(&opts, "conversation", None);
    upgrade_drive::remove(&opts, "conversation");
    assert_eq!(
        upgrade_drive::load(&opts, "conversation"),
        Some(saved.clone())
    );
    for defect in 0..4 {
        let mut changed = saved.clone();
        match defect {
            0 => changed.native.as_mut().unwrap().capability = "45".repeat(32),
            1 => changed.line.push(' '),
            2 => changed.pending_since += 1,
            3 => changed.phase = Phase::Done,
            _ => unreachable!(),
        }
        assert!(upgrade_drive::save_checked(&opts, "conversation", &changed).is_err());
    }
    let mut waiting = saved.clone();
    waiting.wait = "account currently unavailable".into();
    upgrade_drive::save_checked(&opts, "conversation", &waiting).unwrap();
    assert_eq!(
        upgrade_drive::load(&opts, "conversation").unwrap().wait,
        waiting.wait
    );
    assert_eq!(without_candidate(&st), old);
}

#[test]
fn independent_writers_publish_at_most_one_candidate() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let (old, candidate) = fixture(ImagePhase::Forward);
    upgrade_drive::save_checked(&opts, "conversation", &old).unwrap();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (1..=2)
        .map(|id| {
            let opts = opts.clone();
            let mut st = old.clone();
            let mut candidate = candidate.clone();
            candidate.id = format!("{id:032x}");
            let gate = std::sync::Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.wait();
                stage(
                    &opts,
                    "conversation",
                    &mut st,
                    &candidate,
                    ImagePhase::Forward,
                )
            })
        })
        .collect();
    let outcomes: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    let winners: Vec<_> = outcomes
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .collect();
    assert_eq!(winners.len(), 1);
    let saved = upgrade_drive::load(&opts, "conversation").unwrap();
    assert_eq!(retry(&opts, "conversation", &saved).unwrap(), *winners[0]);
    assert_eq!(without_candidate(&saved), old);
}

#[test]
fn unreadable_existing_record_is_not_overwritten_or_removed() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let (mut st, candidate) = fixture(ImagePhase::Forward);
    upgrade_drive::save_checked(&opts, "conversation", &st).unwrap();
    let path = opts.state.join("upgrade/conversation.json");
    std::fs::write(&path, b"{unreadable candidate record").unwrap();
    assert!(upgrade_drive::load_record(&opts, "conversation").is_err());
    assert_eq!(
        stage(
            &opts,
            "conversation",
            &mut st,
            &candidate,
            ImagePhase::Forward
        ),
        Err("native-unsaved")
    );
    upgrade_drive::restore(&opts, "conversation", None);
    upgrade_drive::remove(&opts, "conversation");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"{unreadable candidate record"
    );
}

#[test]
fn retired_predecessor_is_protected_even_before_a_candidate_exists() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let (held, _) = fixture(ImagePhase::Forward);
    upgrade_drive::save_checked(&opts, "conversation", &held).unwrap();
    let saved = upgrade_drive::load(&opts, "conversation").unwrap();
    assert!(upgrade_drive::save_checked(&opts, "conversation", &St::default()).is_err());
    upgrade_drive::restore(&opts, "conversation", Some(&St::default()));
    upgrade_drive::remove(&opts, "conversation");
    assert_eq!(upgrade_drive::load(&opts, "conversation"), Some(saved));
    let mut dry = opts.clone();
    dry.dry_run = true;
    assert!(upgrade_drive::save_checked(&dry, "conversation", &held).is_err());
}

#[test]
fn an_unknown_agent_cannot_hide_existing_native_debt() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let (held, _) = fixture(ImagePhase::Forward);
    upgrade_drive::save_checked(&opts, "conversation", &held).unwrap();
    let path = opts.state.join("upgrade/conversation.json");
    let mut wire: Value = aterm_json::from_str(&held.to_json()).unwrap();
    wire.as_object_mut()
        .unwrap()
        .insert("agent".into(), Value::from("future-agent"));
    let original = aterm_json::to_string(&wire).unwrap();
    std::fs::write(&path, &original).unwrap();
    assert!(upgrade_drive::load_record(&opts, "conversation").is_err());
    assert!(upgrade_drive::save_checked(&opts, "conversation", &St::default()).is_err());
    upgrade_drive::remove(&opts, "conversation");
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}

#[test]
fn first_candidate_cannot_overwrite_a_newer_predecessor_record() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let (mut old, candidate) = fixture(ImagePhase::Forward);
    let mut newer = old.clone();
    newer.native_prepare.clear();
    newer.native = None;
    newer.line = "newer conversation".into();
    upgrade_drive::save_checked(&opts, "conversation", &newer).unwrap();
    assert_eq!(
        stage(
            &opts,
            "conversation",
            &mut old,
            &candidate,
            ImagePhase::Forward
        ),
        Err("native-unsaved")
    );
    let saved = upgrade_drive::load(&opts, "conversation").unwrap();
    assert_eq!(without_candidate(&saved), newer);
    assert!(saved.native_account_candidate.is_none());
}

fn asks_for_file_and_directory_durability(flushes: &[aterm_durable::Flush]) -> bool {
    flushes.iter().filter(|flush| flush.is_durable()).count() >= 2
}

#[test]
fn ordinary_save_and_restore_preserve_native_durability_in_both_phases() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        for staged in [false, true] {
            for restore in [false, true] {
                for kind in [std::io::ErrorKind::Unsupported, std::io::ErrorKind::Other] {
                    let root = aterm_tempfile::tempdir().unwrap();
                    let opts = opts(root.path());
                    let (mut st, candidate) = fixture(phase);
                    if staged {
                        stage(&opts, "conversation", &mut st, &candidate, phase).unwrap();
                    } else {
                        upgrade_drive::save_checked(&opts, "conversation", &st).unwrap();
                    }
                    let expected = upgrade_drive::load(&opts, "conversation").unwrap();
                    st.wait = "waiting for an account".into();
                    let fault = aterm_durable::seam::FailingDirs::with_kind(kind);
                    let recording = aterm_durable::seam::Recording::start();
                    assert_eq!(
                        upgrade_drive::ordinary_write_for_test(&opts, "conversation", &st, restore)
                            .unwrap_err()
                            .kind(),
                        kind
                    );
                    assert!(asks_for_file_and_directory_durability(&recording.take()));
                    let mut loaded = upgrade_drive::load(&opts, "conversation").unwrap();
                    loaded.wait = expected.wait.clone();
                    loaded.progress_at = expected.progress_at;
                    loaded.progress_key = expected.progress_key.clone();
                    assert_eq!(loaded, expected);
                    // A visible rename after a failed barrier is still not an ack.
                    if staged {
                        assert_eq!(retry(&opts, "conversation", &st), Err("native-unsaved"));
                    }
                    drop(fault);
                    if staged {
                        assert_eq!(
                            *retry(&opts, "conversation", &st).unwrap().prepare,
                            candidate
                        );
                    } else {
                        upgrade_drive::ordinary_write_for_test(&opts, "conversation", &st, restore)
                            .unwrap();
                    }
                    assert!(asks_for_file_and_directory_durability(&recording.take()));
                }
            }
        }
    }
}

#[test]
fn writer_checks_prior_native_metadata_and_keeps_plain_writes_unaffected() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let native = St {
        native_coordination: true,
        ..St::default()
    };
    let plain = St::default();
    assert!(!crate::harness::native_restart::coordinated(&plain));
    let recording = aterm_durable::seam::Recording::start();
    upgrade_drive::native_writer_for_test(&opts, "conversation", &plain, Some(&native), false)
        .unwrap();
    assert!(asks_for_file_and_directory_durability(&recording.take()));
    let fault = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    upgrade_drive::save(&opts, "conversation", &plain);
    assert!(recording.take().is_empty());
    upgrade_drive::restore(&opts, "conversation", Some(&plain));
    assert!(recording.take().is_empty());
    drop(fault);
    upgrade_drive::native_writer_for_test(&opts, "conversation", &native, Some(&plain), false)
        .unwrap();
    assert!(asks_for_file_and_directory_durability(&recording.take()));
}

#[test]
fn a_rollback_marker_alone_keeps_the_record_native() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let st = St {
        native_rollback: true,
        ..St::default()
    };
    assert!(crate::harness::native_restart::coordinated(&st));
    let loaded = St::from_json(&st.to_json()).unwrap();
    assert!(loaded.native_coordination);
    let recording = aterm_durable::seam::Recording::start();
    upgrade_drive::save(&opts, "conversation", &st);
    assert!(asks_for_file_and_directory_durability(&recording.take()));
}

#[test]
fn an_unacknowledged_native_write_cannot_note_a_build_failure() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let st = St {
        native_coordination: true,
        phase: Phase::Failed("no-resume".into()),
        failed_at: 7,
        to: "1.0.2".into(),
        source: "native".into(),
        ..St::default()
    };
    let fault = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    assert!(upgrade_drive::ordinary_write_for_test(&opts, "conversation", &st, false).is_err());
    let notes = opts.state.join("upgrade/build-starts");
    assert!(!notes.exists());
    drop(fault);
    upgrade_drive::ordinary_write_for_test(&opts, "conversation", &st, false).unwrap();
    assert_eq!(std::fs::read_dir(notes).unwrap().count(), 1);
}

/// The state a real step reads: the barrier fault, the run's dryness and the slot the record
/// holds. What the model's history adds (attempts, exposures, rivals, reloads) reaches no record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Inputs {
    barrier_failed: bool,
    dry: bool,
    staged: bool,
    readable: bool,
}

impl Inputs {
    fn of(state: &State) -> Self {
        Self {
            barrier_failed: state["barrier_failed"] == 1,
            dry: state["dry"] == 1,
            staged: state["staged"] == 1,
            readable: state["readable"] == 1,
        }
    }
}

/// What one real step showed: each fact the model's next state takes from the code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Observed {
    /// `Attempt` exposed its candidate.
    exposed: bool,
    /// `Rival` was refused yet changed the slot.
    slot_changed: bool,
    /// `Reload` or `StaleSave` came back with another slot.
    lost: bool,
    /// In a live run, `Rewrite` asked for less durability, or succeeded past a failed barrier.
    weakened: bool,
    /// The in-memory state holds a slot afterwards, whatever the save did.
    staged: bool,
    /// Something besides the slot changed.
    debt_cleared: bool,
}

/// The next state after an action only a later attempt reads, or `None` for one the code runs.
/// What these change reaches no record, whichever record there is: a state they are enabled in
/// also enables `Attempt`, which plants and judges that record.
fn modeled(state: &State, action: &str) -> Option<State> {
    let (field, value) = match action {
        "FailBarrier" => ("barrier_failed", 1),
        "RepairBarrier" => ("barrier_failed", 0),
        "DryRun" => ("dry", 1),
        "LiveRun" => ("dry", 0),
        _ => return None,
    };
    let mut next = state.clone();
    next.insert(field, value);
    Some(next)
}

/// Run `action` on a fresh record planted from `inputs` in `phase`.
fn observe(
    inputs: Inputs,
    action: &str,
    bypass_barrier: bool,
    phase: ImagePhase,
    restore: bool,
) -> Observed {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut opts = opts(root.path());
    opts.dry_run = inputs.dry;
    let (mut st, candidate) = fixture(phase);
    let old = st.clone();
    if inputs.staged {
        st.native_account_candidate = Some(if inputs.readable {
            Pending::new(st.native.as_ref().unwrap(), phase, &candidate).unwrap()
        } else {
            Pending::read(&Value::Null)
        });
    }
    // Reconstruct the modeled disk state, not only its in-memory mirror. Every
    // transition then exercises the real prior-record comparison under its lock.
    let mut planting = opts.clone();
    planting.dry_run = false;
    upgrade_drive::save_checked(&planting, "conversation", &st).unwrap();
    let original_slot = st.native_account_candidate.clone();
    let mut seen = Observed::default();
    match action {
        "Attempt" => {
            let fault = inputs.barrier_failed.then(|| {
                aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported)
            });
            let result = stage(&opts, "conversation", &mut st, &candidate, phase);
            seen.exposed = result.is_ok()
                || (bypass_barrier
                    && st
                        .native_account_candidate
                        .as_ref()
                        .and_then(|pending| pending.checked(&st))
                        .is_some());
            drop(fault);
        }
        "Rival" => {
            let mut rival = candidate.clone();
            rival.id = "02".repeat(16);
            assert!(stage(&opts, "conversation", &mut st, &rival, phase).is_err());
            seen.slot_changed = st.native_account_candidate != original_slot;
        }
        "Opaque" => st.native_account_candidate = Some(Pending::read(&Value::Null)),
        "Reload" => {
            st = upgrade_drive::load(&opts, "conversation").unwrap();
            seen.lost = st.native_account_candidate != original_slot;
        }
        "StaleSave" => {
            upgrade_drive::save_checked(&planting, "conversation", &st).unwrap();
            let result = upgrade_drive::save_checked(&opts, "conversation", &old);
            assert!(opts.dry_run || result.is_err());
            st = upgrade_drive::load(&opts, "conversation").unwrap();
            seen.lost = st.native_account_candidate != original_slot;
        }
        "Rewrite" => {
            let fault = inputs.barrier_failed.then(|| {
                aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported)
            });
            let recording = aterm_durable::seam::Recording::start();
            let result = if bypass_barrier && !opts.dry_run {
                upgrade_drive::native_writer_for_test(&opts, "conversation", &st, Some(&st), true)
            } else {
                upgrade_drive::ordinary_write_for_test(&opts, "conversation", &st, restore)
            };
            let flushes = recording.take();
            drop(fault);
            seen.weakened = !opts.dry_run
                && (!asks_for_file_and_directory_durability(&flushes)
                    || (inputs.barrier_failed && result.is_ok()));
        }
        other => unreachable!("{other}"),
    }
    seen.staged = st.native_account_candidate.is_some();
    seen.debt_cleared = without_candidate(&st) != old;
    seen
}

/// The model's next state after `action` showed `seen` from `state`.
fn apply(state: &State, action: &str, seen: Observed) -> State {
    let mut next = state.clone();
    match action {
        "Attempt" => {
            next.insert("attempts", state["attempts"] + 1);
            next.insert("exposed", state["exposed"] + i64::from(seen.exposed));
            if seen.exposed && state["barrier_failed"] == 1 {
                next.insert("unpublished", 1);
            }
            if seen.exposed && state["dry"] == 1 {
                next.insert("dry_exposed", 1);
            }
        }
        "Rival" => {
            next.insert("rival", 1);
            if seen.slot_changed {
                next.insert("exact", 0);
            }
        }
        "Opaque" => {
            next.insert("readable", 0);
        }
        "Reload" => {
            next.insert("reloaded", 1);
            if seen.lost {
                next.insert("lost", 1);
            }
        }
        "StaleSave" => {
            next.insert("stale", 1);
            if seen.lost {
                next.insert("lost", 1);
            }
        }
        "Rewrite" => {
            next.insert("rewritten", 1);
            if seen.weakened {
                next.insert("weakened", 1);
            }
        }
        other => unreachable!("{other}"),
    }
    next.insert("staged", i64::from(seen.staged));
    if seen.debt_cleared {
        next.insert("debt", 0);
    }
    next
}

fn model_step(
    state: &State,
    action: &str,
    bypass_barrier: bool,
    phase: ImagePhase,
    restore: bool,
) -> State {
    modeled(state, action).unwrap_or_else(|| {
        let seen = observe(Inputs::of(state), action, bypass_barrier, phase, restore);
        apply(state, action, seen)
    })
}

/// What `action` shows from `inputs` in each phase, which must agree, and for `Rewrite` with
/// each phase's restore too.
fn observe_in_both_phases(inputs: Inputs, action: &str) -> Observed {
    let forward = observe(inputs, action, false, ImagePhase::Forward, false);
    let rollback = observe(inputs, action, false, ImagePhase::Rollback, false);
    assert_eq!(
        forward, rollback,
        "the slot protocol must preserve either active phase"
    );
    if action == "Rewrite" {
        for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
            assert_eq!(forward, observe(inputs, action, false, phase, true));
        }
    }
    forward
}

/// What each real input showed for each action.
pub(super) type Seen<'a> = std::collections::BTreeMap<(Inputs, &'a str), Observed>;

/// `action` from `state`, with what each real input showed kept in `seen`: [`observe`] reads
/// only its [`Inputs`], so the states that agree on them share one observation.
pub(super) fn transition<'a>(state: &State, action: &'a str, seen: &mut Seen<'a>) -> State {
    modeled(state, action).unwrap_or_else(|| {
        let inputs = Inputs::of(state);
        let observed = *seen
            .entry((inputs, action))
            .or_insert_with(|| observe_in_both_phases(inputs, action));
        apply(state, action, observed)
    })
}

#[test]
fn client_slot_refines_every_model_transition_and_breaks_on_early_exposure() {
    let model = aterm_models_harness::native_account_client_slot_model();
    aterm_spec::interp::prove_and_catch(&model);
    // NEGATIVE CONTROL (2026-11-09): with one phase's copy served for both this fails at
    // "Rollback's fixture". With a retry that exposes its candidate past a failed save, or with
    // `seen` keyed on the action alone, the walk below fails at `Attempt` from
    // `barrier_failed: 1`.
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        assert_eq!(fixture(phase), laid_out(phase), "{phase:?}'s fixture");
    }
    let mut seen = Seen::new();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name, &mut seen);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut before = model.init_state();
    assert!(model.fire("FailBarrier", &mut before));
    let bad = model_step(&before, "Attempt", true, ImagePhase::Forward, false);
    assert!(!model.check_invariant("PublicationBeforeExposure", &bad));
    assert!(!model.successors("Attempt", &before).contains(&bad));
    let mut before = model.init_state();
    assert!(model.fire("FailBarrier", &mut before));
    let bad = model_step(&before, "Rewrite", true, ImagePhase::Forward, false);
    assert!(!model.check_invariant("RewriteKeepsDurability", &bad));
    assert!(!model.successors("Rewrite", &before).contains(&bad));
}
