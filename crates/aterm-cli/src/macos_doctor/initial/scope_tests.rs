// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_upgrade_checkpoint_round_trips_every_progress_field() {
    let mut desktop = Selection::none();
    desktop.set(Setting::Desktop, true);
    for access in [None, Some(12), Some(13)] {
        for only in [None, Some(Selection::none()), Some(desktop)] {
            for (selection, continues) in [
                (None, None),
                (Some(Selection::none()), Some(11)),
                (Some(Selection::all()), None),
            ] {
                let progress = Progress {
                    done: Stage::Workstation.bit() | Stage::Permissions.bit(),
                    completed: false,
                    admitted: true,
                    running: Some(Stage::Secrets),
                    plan: [7; 32],
                    revision: 13,
                    stopped: true,
                    access,
                    admitted_vault: AdmittedVault::Configured([9; 32]),
                    only,
                    selection,
                    continues,
                };
                assert_eq!(
                    parse_progress(&progress_checkpoint(progress)).unwrap(),
                    progress
                );
            }
        }
    }
}

fn provisional(root: &Path) -> Plan {
    let narrow = scoped(Plan::default(), false);
    authorize_at(root, &narrow).unwrap().unwrap();
    save_plan_as(root, &narrow, true).unwrap();
    let mut history = progress(root).unwrap().unwrap();
    history.done = Stage::Permissions.bit();
    history.completed = true;
    save_progress(root, history).unwrap();
    narrow
}

fn permissions_only_provisional(root: &Path) -> Plan {
    let mut old = Plan {
        selection: Selection::none(),
        ..Plan::default()
    };
    old.selection.set(Setting::Permissions, true);
    authorize_at(root, &old).unwrap().unwrap();
    save_plan_as(root, &old, true).unwrap();
    let mut history = progress(root).unwrap().unwrap();
    history.done = Stage::Permissions.bit();
    history.completed = true;
    save_progress(root, history).unwrap();
    old
}

fn interrupted_base_upgrade(root: &Path) -> Plan {
    interrupted_base_upgrade_in_vault(root, None)
}

fn interrupted_base_upgrade_in_vault(root: &Path, vault: Option<ConfiguredVault>) -> Plan {
    permissions_only_provisional(root);
    let (mut ticket, base) = prepare_upgrade(root, || false, || Ok(vault))
        .unwrap()
        .unwrap();
    assert_eq!(base.selection, aterm_types::macos_setup::BASE_PLAN);
    assert!(
        ticket
            .persist_for_watch_test(root, &base, true, Some((0, true)))
            .is_err(),
        "the process ended after publishing the plan, before its progress"
    );
    base
}

fn configured() -> ConfiguredVault {
    ConfiguredVault {
        config: aterm_secrets::client::Config {
            source: aterm_secrets::transport::Source::Url(
                "https://reviewed.example/v1/team".into(),
            ),
            mirrors: Vec::new(),
            name: "team".into(),
            signer: Some([7; 32]),
            followed: Vec::new(),
            host: "test-mac".into(),
        },
        home: PathBuf::from("/test-home"),
    }
}

#[test]
fn a_provisional_upgrade_admits_the_vault_joined_since_the_first_launch() {
    let root = aterm_tempfile::tempdir().unwrap();
    let narrow = provisional(root.path());
    let vault = configured();
    let (mut ticket, full) = prepare_upgrade(root.path(), || true, || Ok(Some(vault.clone())))
        .unwrap()
        .unwrap();
    let revision = ticket.revision();
    assert_eq!(
        ticket.upgrade_at(root.path(), &full).result.unwrap(),
        Some(revision + 1)
    );
    let saved = saved_plan(root.path()).unwrap().unwrap();
    assert_eq!(saved.selection, Selection::all());
    assert_ne!(
        saved.vault_url, narrow.vault_url,
        "the automatic upgrade must not join the default vault over a vault this Mac joined"
    );
    assert_eq!(
        aterm_machines::init_secrets::selected_vault(
            &saved.vault_url,
            &vault.home,
            Some(&vault.config),
        )
        .unwrap(),
        Some(&vault.config),
        "the saved Secrets and Repositories stages must name the configured vault"
    );
    let history = progress(root.path()).unwrap().unwrap();
    assert_eq!(
        history.admitted_vault,
        AdmittedVault::Configured(vault.identity())
    );
    assert_eq!(history.done, Stage::Permissions.bit());
}

#[test]
fn a_provisional_upgrade_without_a_configured_vault_keeps_the_default() {
    let root = aterm_tempfile::tempdir().unwrap();
    let narrow = provisional(root.path());
    let (mut ticket, full) = prepare_upgrade(root.path(), || true, || Ok(None))
        .unwrap()
        .unwrap();
    assert_eq!(full.vault_url, narrow.vault_url);
    assert!(
        ticket
            .upgrade_at(root.path(), &full)
            .result
            .unwrap()
            .is_some()
    );
    assert_eq!(
        progress(root.path()).unwrap().unwrap().admitted_vault,
        AdmittedVault::None,
        "a default-vault admission must still record the observed absence of another vault"
    );
}

/// A prepared upgrade meets records another writer published since it observed them. Over
/// a Restore it writes nothing; over the same provisional plan published again (a pass, a
/// resume) it still widens the plan in force, so it is admitted against the records as they
/// are now (design §3.5).
#[test]
fn a_prepared_provisional_upgrade_does_not_replace_a_restore_but_widens_the_same_plan() {
    for stopped in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let narrow = provisional(root.path());
        let (mut ticket, full) = prepare_upgrade(root.path(), || true, || Ok(Some(configured())))
            .unwrap()
            .unwrap();
        if stopped {
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":"restored"}),
            )
            .unwrap();
            let winner = observation_at(root.path()).unwrap();
            assert_eq!(ticket.upgrade_at(root.path(), &full).result.unwrap(), None);
            assert_eq!(observation_at(root.path()).unwrap(), winner);
        } else {
            save_plan_as(root.path(), &narrow, true).unwrap();
            assert!(
                ticket
                    .upgrade_at(root.path(), &full)
                    .result
                    .unwrap()
                    .is_some()
            );
            assert_eq!(
                saved_plan(root.path()).unwrap().unwrap().selection,
                Selection::all()
            );
        }
    }
}

#[test]
fn an_upgrade_requires_a_readable_vault_and_preserves_the_persons_plan() {
    let root = aterm_tempfile::tempdir().unwrap();
    let narrow = provisional(root.path());
    let before = observation_at(root.path()).unwrap();
    assert!(prepare_upgrade(root.path(), || true, || Err("cannot read config".into())).is_err());
    assert_eq!(observation_at(root.path()).unwrap(), before);
    assert!(
        prepare_upgrade(
            root.path(),
            || false,
            || panic!("no upgrade needs no config read")
        )
        .unwrap()
        .is_none()
    );
    save_plan_as(root.path(), &narrow, false).unwrap();
    assert!(
        prepare_upgrade(
            root.path(),
            || true,
            || panic!("the person's plan needs no vault read"),
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn widens_says_whether_an_upgrade_would_widen_the_saved_plan() {
    let root = aterm_tempfile::tempdir().unwrap();
    assert!(!widens_at(root.path()).unwrap(), "no plan saved yet");
    let narrow = provisional(root.path());
    assert!(widens_at(root.path()).unwrap());
    let (mut ticket, full) = prepare_upgrade(root.path(), || true, || Ok(None))
        .unwrap()
        .unwrap();
    ticket
        .upgrade_at(root.path(), &full)
        .result
        .unwrap()
        .unwrap();
    assert!(!widens_at(root.path()).unwrap(), "the whole profile");
    InitialSetupAuthorization::capture_at(root.path())
        .unwrap()
        .persist_at(root.path(), &narrow)
        .result
        .unwrap()
        .expect("the person's narrow plan is durably admitted");
    assert!(
        !widens_at(root.path()).unwrap(),
        "joining the fleet preserves the person's choice"
    );

    // A partial publication is unknown, rather than a valid decision not to widen.
    save_plan_as(root.path(), &full, false).unwrap();
    assert!(widens_at(root.path()).is_err());
}

/// A look that hears the ALab signal saves nothing. Automatic plans can expand; the person's
/// choices stay held. Only persistence starts the expanded run.
#[test]
fn a_look_saves_nothing_and_only_automatic_plans_expand_on_the_fleet() {
    let root = aterm_tempfile::tempdir().unwrap();
    let narrow = provisional(root.path());
    let before = observation_at(root.path()).unwrap();
    assert_eq!(
        widened_at(root.path(), || true, || Ok(None)).unwrap(),
        Some((narrow.clone(), Plan::default()))
    );
    assert_eq!(
        widened_at(root.path(), || false, || panic!("no signal reads no vault")).unwrap(),
        None
    );
    assert_eq!(
        observation_at(root.path()).unwrap(),
        before,
        "a look saves nothing"
    );
    let revision = revision_at(root.path()).unwrap();
    let progress_before = std::fs::read(root.path().join("initial-setup.json")).unwrap();
    save_plan_as(root.path(), &narrow, false).unwrap();
    assert!(!widens_at(root.path()).unwrap());
    assert_eq!(
        widened_at(
            root.path(),
            || true,
            || panic!("the person's plan needs no vault read"),
        )
        .unwrap(),
        None
    );
    assert_eq!(revision_at(root.path()).unwrap(), revision);
    assert_eq!(
        std::fs::read(root.path().join("initial-setup.json")).unwrap(),
        progress_before,
        "its run stays as it was"
    );
}

/// An upgrade whose commit stops part-way is held, saying the revision it widens, and the
/// next look finishes that commit, not a fresh one, from the revision it began from.
#[test]
fn an_upgrade_that_stops_part_way_is_finished_from_where_it_began() {
    let root = aterm_tempfile::tempdir().unwrap();
    provisional(root.path());
    let mut upgrade = InitialSetupUpgrade::default();
    let stopped = upgrade.persist_with(
        root.path(),
        || true,
        || Ok(None),
        |ticket, full| ticket.persist_for_watch_test(root.path(), full, true, Some((1, false))),
    );
    assert!(stopped.is_err());
    let base = upgrade.widens_from();
    assert!(
        base.is_some() && upgrade.pending(),
        "kept for the next look"
    );
    let finished = upgrade
        .persist_with(
            root.path(),
            || panic!("the held upgrade is finished, not looked for again"),
            || Ok(None),
            |ticket, full| ticket.upgrade_at(root.path(), full).result,
        )
        .unwrap()
        .expect("admitted");
    assert_eq!(Some(finished.from), base, "from the revision it began from");
    assert!(!upgrade.pending() && upgrade.widens_from().is_none());
    let admitted = progress(root.path()).unwrap().unwrap();
    assert_eq!((admitted.revision, admitted.admitted), (finished.to, true));
}

/// The review's Apply meets the profile lock busy past its wait (a retry pass, `aterm doctor
/// --apply`, a Full Disk Access follow-up), and that pass saves its progress before it lets
/// go. The held attempt's next look reads the records again: the plan in force is still the
/// same provisional plan, so the widen is admitted against them, never dropped (design §3.5).
#[test]
fn a_widen_apply_that_met_a_busy_lock_is_admitted_over_a_pass_that_saved_meanwhile() {
    let root = aterm_tempfile::tempdir().unwrap();
    provisional(root.path());
    let mut upgrade = InitialSetupUpgrade::default();
    let held = HeldLock::try_take(root.path()).unwrap().unwrap();
    let first = upgrade.persist_with(
        root.path(),
        || true,
        || Ok(None),
        |ticket, full| ticket.upgrade_at(root.path(), full).result,
    );
    assert_eq!(first, Err(SETUP_BUSY.to_string()));
    assert!(upgrade.pending(), "the busy look keeps the attempt");
    // The pass that held the lock saves its progress (any write: a new publication).
    let history = progress(root.path()).unwrap().unwrap();
    save_progress(root.path(), history).unwrap();
    drop(held);
    let second = upgrade
        .persist_with(
            root.path(),
            || panic!("the held attempt is finished, not looked for again"),
            || Ok(None),
            |ticket, full| ticket.upgrade_at(root.path(), full).result,
        )
        .unwrap()
        .expect("the review's Apply is admitted");
    assert_eq!(second.from, history.revision);
    assert!(!upgrade.pending());
    assert_eq!(
        saved_plan(root.path()).unwrap().unwrap().selection,
        Selection::all()
    );
    let admitted = progress(root.path()).unwrap().unwrap();
    assert_eq!((admitted.revision, admitted.admitted), (second.to, true));
    assert!(!widens_at(root.path()).unwrap(), "nothing left to widen");
}

/// The same busy look, but the writer that held the lock changed the plan in force: a Stop of
/// its run, a Restore, another plan, or a choice of the same partial plan ends this attempt.
#[test]
fn a_widen_apply_that_met_a_busy_lock_is_not_admitted_over_a_changed_plan() {
    for change in ["stop", "chosen", "restore", "another plan"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let narrow = provisional(root.path());
        if change == "stop" {
            let mut unfinished = progress(root.path()).unwrap().unwrap();
            unfinished.done = 0;
            unfinished.completed = false;
            save_progress(root.path(), unfinished).unwrap();
        }
        let mut upgrade = InitialSetupUpgrade::default();
        let held = HeldLock::try_take(root.path()).unwrap().unwrap();
        let first = upgrade.persist_with(
            root.path(),
            || true,
            || Ok(None),
            |ticket, full| ticket.upgrade_at(root.path(), full).result,
        );
        assert_eq!(first, Err(SETUP_BUSY.to_string()), "{change}");
        match change {
            "stop" => {
                let revision = progress(root.path()).unwrap().unwrap().revision;
                request_stop_at(root.path(), revision).unwrap();
            }
            "chosen" => save_plan_as(root.path(), &narrow, false).unwrap(),
            "restore" => write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":"restored"}),
            )
            .unwrap(),
            _ => {
                let mut other = narrow.clone();
                // A setting the base plan does not already carry, so this is a different plan.
                other.selection.set(Setting::Dock, true);
                save_plan_as(root.path(), &other, true).unwrap();
            }
        }
        drop(held);
        let changed = observation_at(root.path()).unwrap();
        let second = upgrade.persist_with(
            root.path(),
            || panic!("the held attempt is finished, not looked for again"),
            || Ok(None),
            |ticket, full| ticket.upgrade_at(root.path(), full).result,
        );
        assert_eq!(second, Ok(None), "{change}");
        assert!(
            observation_at(root.path()).unwrap() == changed,
            "{change}: nothing written"
        );
        assert!(!upgrade.pending(), "{change}: the attempt ends");
    }
}

/// The same busy look, but the setup that held the lock was a newer aterm's, which saved
/// this Mac's setup in its newer format before it let go. Its records are final for this
/// binary: the held attempt's next look fails NEWER_SETUP, writes nothing and ends the
/// attempt, so the watch settles the review's Apply, and no later look widens or retries.
#[test]
fn a_held_widen_apply_meeting_a_newer_aterms_records_ends() {
    let root = aterm_tempfile::tempdir().unwrap();
    provisional(root.path());
    let mut upgrade = InitialSetupUpgrade::default();
    let held = HeldLock::try_take(root.path()).unwrap().unwrap();
    let first = upgrade.persist_with(
        root.path(),
        || true,
        || Ok(None),
        |ticket, full| ticket.upgrade_at(root.path(), full).result,
    );
    assert_eq!(first, Err(SETUP_BUSY.to_string()));
    assert!(upgrade.pending(), "the busy look keeps the attempt");
    // The newer aterm that held the lock saves its decision in its own format.
    write_json(
        &root.path().join("first-run.json"),
        &json!({"profile": PROFILE, "state": "pending", "format": FORMAT + 1}),
    )
    .unwrap();
    drop(held);
    let newer = std::fs::read(root.path().join("first-run.json")).unwrap();
    let again = upgrade.persist_with(
        root.path(),
        || panic!("the held attempt is finished, not looked for again"),
        || Ok(None),
        |ticket, full| ticket.upgrade_at(root.path(), full).result,
    );
    assert!(
        again.as_ref().is_err_and(|why| setup_is_newer(why)),
        "{again:?}"
    );
    assert!(
        !upgrade.pending(),
        "a newer aterm's records end the attempt"
    );
    assert_eq!(upgrade.published_at(root.path()), Ok(None));
    assert_eq!(widens_at(root.path()), Ok(false), "the watch ends");
    let later = upgrade.persist_with(
        root.path(),
        || panic!("a newer aterm's records are not looked at for the signal"),
        || Ok(None),
        |_, _| panic!("nothing is admitted over a newer aterm's records"),
    );
    assert!(later.is_err_and(|why| setup_is_newer(&why)));
    assert!(!upgrade.pending(), "never kept");
    assert_eq!(
        std::fs::read(root.path().join("first-run.json")).unwrap(),
        newer,
        "the newer aterm's record is never written"
    );
}

/// A widen whose write fails and whose publication cannot be read back is one this binary
/// cannot identify, so it cannot finish it: the attempt is let go at once, never written
/// again nor rebound to a later publication, keeping only the target it reserved for Stop
/// (known once its plan's checkpoint landed, unknown otherwise) until the next persist.
#[test]
fn a_widen_whose_write_cannot_be_identified_is_let_go_keeping_its_stop_evidence() {
    for (at, published) in [(1, false), (0, true)] {
        let root = aterm_tempfile::tempdir().unwrap();
        provisional(root.path());
        let from = revision_at(root.path()).unwrap();
        let mut upgrade = InitialSetupUpgrade::default();
        let lost = upgrade.persist_with(
            root.path(),
            || true,
            || Ok(None),
            |ticket, full| ticket.lose_publication_for_watch_test(root.path(), full, at, published),
        );
        assert!(lost.is_err(), "{at}: {lost:?}");
        assert!(!upgrade.pending(), "{at}: let go");
        assert_eq!(upgrade.widens_from(), None);
        let evidence = upgrade.published_at(root.path());
        if at == 1 {
            let target = evidence
                .unwrap()
                .expect("the plan's checkpoint reserved it");
            assert_eq!(target.from, from);
            assert!(target.to > from);
        } else {
            assert!(evidence.is_err(), "unknown, not unwritten: {evidence:?}");
        }
        let before = observation_at(root.path()).unwrap();
        let again = upgrade.persist_with(
            root.path(),
            || true,
            || Ok(None),
            |ticket, full| ticket.upgrade_at(root.path(), full).result,
        );
        assert!(!matches!(again, Ok(Some(_))), "{at}: {again:?}");
        assert!(
            observation_at(root.path()).unwrap() == before,
            "{at}: never written again"
        );
        assert_eq!(upgrade.published_at(root.path()), Ok(None));
    }
}

/// The review's Apply waits for the profile lock another setup holds, as a choice does, and
/// says so the moment it starts waiting; once that setup lets go, having saved its progress,
/// the widen is admitted in the same press. One whose wait runs out is refused busy, its
/// ticket kept for the next look.
#[test]
fn a_widen_apply_waits_for_the_profile_lock_and_says_so() {
    let root = aterm_tempfile::tempdir().unwrap();
    provisional(root.path());
    let (mut ticket, full) = prepare_upgrade(root.path(), || true, || Ok(None))
        .unwrap()
        .unwrap();
    let mut held = Some(HeldLock::try_take(root.path()).unwrap().unwrap());
    let mut noted = 0;
    let admitted = ticket.upgrade_waiting(
        root.path(),
        None,
        noting_first(
            || noted += 1,
            || {
                if held.is_some() {
                    let history = progress(root.path()).unwrap().unwrap();
                    save_progress(root.path(), history).unwrap();
                    held = None;
                }
                true
            },
        ),
        &full,
    );
    assert!(
        admitted.result.unwrap().is_some(),
        "admitted once the lock was free"
    );
    assert_eq!(noted, 1, "said once, as it started waiting");
    assert_eq!(
        saved_plan(root.path()).unwrap().unwrap().selection,
        Selection::all()
    );

    let root = aterm_tempfile::tempdir().unwrap();
    provisional(root.path());
    let (mut ticket, full) = prepare_upgrade(root.path(), || true, || Ok(None))
        .unwrap()
        .unwrap();
    let _held = HeldLock::try_take(root.path()).unwrap().unwrap();
    let refused = ticket.upgrade_waiting(root.path(), None, || false, &full);
    assert_eq!(refused.result, Err(SETUP_BUSY.to_string()));
    assert!(ticket.pending(), "kept for the next look");
}

/// A Stop that came during the last stage of aterm's provisional run was spent: the run
/// completed, and its records hold no Stop. They still let the plan widen, so the next
/// launch may offer the widen review; in the launch that pressed Stop the watch offers none
/// (`provisional_setup_widens_here` in the binary).
#[test]
fn a_stop_spent_on_a_finished_provisional_run_leaves_its_widen_to_a_later_launch() {
    let root = aterm_tempfile::tempdir().unwrap();
    provisional(root.path());
    let revision = progress(root.path()).unwrap().unwrap().revision;
    super::super::request_stop_at(root.path(), revision).unwrap();
    assert!(
        widens_at(root.path()).unwrap(),
        "the finished run's spent Stop holds no widening on record"
    );
}

/// aterm's provisional plan, its run `finished` or not, widened on the ALab signal, and a
/// Stop asked for the widened run: the plan, its run as the widen found it, and the
/// widened revision.
fn widened_and_stopped(root: &Path, finished: bool) -> (Plan, Progress, u64) {
    let narrow = provisional(root);
    if !finished {
        let mut unfinished = progress(root).unwrap().unwrap();
        unfinished.done = 0;
        unfinished.completed = false;
        save_progress(root, unfinished).unwrap();
    }
    let before = progress(root).unwrap().unwrap();
    let (mut ticket, full) = prepare_upgrade(root, || true, || Ok(None))
        .unwrap()
        .unwrap();
    let widened = ticket.upgrade_at(root, &full).result.unwrap().unwrap();
    request_stop_at(root, widened).unwrap();
    (narrow, before, widened)
}

/// A Stop that holds the run a widen admitted, before it took a step, puts back the plan in
/// force before the widen, with its run as it was, so the page turns no more groups to Set
/// up (design §3.5); the Stop declined the widen, so the plan is the person's from then on.
#[test]
fn a_stop_does_not_turn_more_groups_to_set_up() {
    for finished in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let (narrow, before, widened) = widened_and_stopped(root.path(), finished);
        assert!(
            !put_back_held_widen(root.path(), widened - 1).unwrap(),
            "only the widened run"
        );
        assert!(put_back_held_widen(root.path(), widened).unwrap());
        assert_eq!(
            saved_plan_record(root.path()).unwrap(),
            Some((narrow.clone(), true))
        );
        assert!(
            !saved_plan_provisional(root.path()).unwrap(),
            "the person's: their Stop declined the widen"
        );
        let run = progress_file(root.path()).unwrap().unwrap();
        assert_eq!(progress(root.path()).unwrap(), Some(run));
        assert!(run.belongs_to(&narrow));
        assert_eq!(
            (run.revision, run.done, run.completed, run.access),
            (
                widened,
                before.done,
                finished,
                Some(before.access_revision())
            ),
            "the run as the widen found it, under the revision the Stop names"
        );
        assert_eq!(run.stopped, !finished, "an unfinished run stays stopped");
        assert_eq!(
            decision_at(root.path()).unwrap(),
            Decision::Applied(true),
            "Permissions alone owes no Mac settings"
        );
        assert!(finished_at(root.path()).unwrap());
        assert!(!widens_at(root.path()).unwrap(), "nothing widens it again");
        fold_stop_request(root.path()).unwrap();
        assert_eq!(stop_request_at(root.path()), None, "the Stop is spent");
        assert_eq!(progress(root.path()).unwrap(), Some(run));
        assert!(
            !put_back_held_widen(root.path(), widened).unwrap(),
            "nothing more to put back"
        );
    }
}

/// A Stop of the widened run declines the widen: once the plan is put back, the next
/// launch's upgrade finds nothing to widen, so the Dock reset and the sign-outs the person
/// stopped never run (design §3.5).
#[test]
fn a_stopped_widen_is_never_widened_again_by_a_later_launch() {
    for finished in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let (narrow, _, widened) = widened_and_stopped(root.path(), finished);
        assert!(put_back_held_widen(root.path(), widened).unwrap());
        // Relaunch: the Stop is spent on the run it held, and the launch looks again.
        fold_stop_request(root.path()).unwrap();
        assert!(
            prepare_upgrade(root.path(), || true, || Ok(None))
                .unwrap()
                .is_none(),
            "finished: {finished}"
        );
        assert_eq!(saved_plan(root.path()).unwrap(), Some(narrow));
    }
}

/// A put-back an earlier build saved provisional is the person's all the same: the Stop
/// declined the widen, so once its run is finished no later look widens it again.
#[test]
fn an_earlier_builds_provisional_put_back_is_never_widened_again() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, _, widened) = widened_and_stopped(root.path(), true);
    assert!(put_back_held_widen(root.path(), widened).unwrap());
    let path = root.path().join("automatic-plan.json");
    let mut record: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record
        .as_object_mut()
        .unwrap()
        .insert("provisional".into(), Value::Bool(true));
    write_json(&path, &record).unwrap();
    fold_stop_request(root.path()).unwrap();
    assert!(
        prepare_upgrade(root.path(), || true, || Ok(None))
            .unwrap()
            .is_none(),
        "the plan a Stop put back is widened again"
    );
}

/// Set up now releases a previous Stop for the plan shown, preserving the groups it left off.
#[test]
fn set_up_now_after_a_stopped_widen_preserves_the_plan_shown() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (narrow, _, widened) = widened_and_stopped(root.path(), false);
    assert!(put_back_held_widen(root.path(), widened).unwrap());
    let shown = snapshot_at(root.path(), narrow.clone()).unwrap();
    assert_eq!(shown.plan, narrow, "the page shows the plan put back");
    assert!(!shown.provisional, "as the person's");
    observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        InitialSetupRestart::default()
            .changing(Some(Selection::none()))
            .persist_with(
                root,
                &shown.plan,
                false,
                || Ok(None),
                save_progress,
                |root| observation_at(root).map_err(|error| error.to_string()),
            )
    })
    .result
    .unwrap()
    .expect("Set up now admitted");
    assert!(!saved_plan_provisional(root.path()).unwrap());
    assert!(!widens_at(root.path()).unwrap());
    assert_eq!(saved_plan(root.path()).unwrap(), Some(narrow));
}

/// A widened run that finished a step of its own, or that a choice replaced, keeps what
/// is in force.
#[test]
fn only_a_widened_run_that_did_nothing_is_put_back() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, _, widened) = widened_and_stopped(root.path(), true);
    let mut ran = progress(root.path()).unwrap().unwrap();
    ran.done |= Stage::Workstation.bit();
    save_progress(root.path(), ran).unwrap();
    assert!(
        !put_back_held_widen(root.path(), widened).unwrap(),
        "it set something"
    );
    assert_eq!(
        saved_plan(root.path()).unwrap().unwrap().selection,
        Selection::all()
    );
    let root = aterm_tempfile::tempdir().unwrap();
    let (narrow, _, widened) = widened_and_stopped(root.path(), true);
    // A person's choice of that plan (Set up now or a group press after the Stop).
    let chosen = restart_at(root.path(), &narrow).unwrap();
    assert!(
        !put_back_held_widen(root.path(), widened).unwrap(),
        "a choice came since"
    );
    assert!(
        !put_back_held_widen(root.path(), chosen).unwrap(),
        "the person's run"
    );
    assert!(!saved_plan_provisional(root.path()).unwrap());
}

/// Each write of a put-back leaves whole records: cut short before its plan's record, the
/// widened run stands stopped; after it, the plan put back is in force with its run, and
/// the next writer of the plan's record, or of a Stop, finishes what was owed first.
#[test]
fn a_put_back_cut_short_at_any_write_leaves_whole_records() {
    let writers: [fn(&Path, &Plan); 2] = [
        |root, plan| save_plan_as(root, plan, true).unwrap(),
        |root, _| fold_stop_request(root).unwrap(),
    ];
    for cut in [
        PutBackWrite::Plan,
        PutBackWrite::Decision,
        PutBackWrite::Progress,
    ] {
        for writer in writers {
            let root = aterm_tempfile::tempdir().unwrap();
            let (narrow, before, widened) = widened_and_stopped(root.path(), true);
            let held = progress(root.path()).unwrap().unwrap();
            let ended = put_back_held_widen_with(root.path(), widened, |write| {
                if write == cut {
                    Err("the process ended".into())
                } else {
                    Ok(())
                }
            });
            assert!(ended.is_err(), "{cut:?}");
            let records = Records::read(root.path()).unwrap();
            assert!(
                finished_at(root.path()).unwrap(),
                "{cut:?}: nothing resumes"
            );
            if cut == PutBackWrite::Plan {
                assert_eq!(
                    records.plan.as_ref().map(|(plan, _)| plan.selection),
                    Some(Selection::all())
                );
                assert_eq!(records.progress, Some(held));
                assert!(records.stopped_run());
                continue;
            }
            assert_eq!(records.plan, Some((narrow.clone(), true)), "{cut:?}");
            let run = records.progress.unwrap();
            assert!(run.belongs_to(&narrow), "{cut:?}");
            assert_eq!(
                (run.revision, run.done, run.completed),
                (widened, before.done, true)
            );
            assert_eq!(revision_at(root.path()).unwrap(), widened);
            writer(root.path(), &narrow);
            assert_eq!(progress_file(root.path()).unwrap(), Some(run), "{cut:?}");
            assert_eq!(
                decision_at(root.path()).unwrap(),
                Decision::Applied(true),
                "{cut:?}"
            );
        }
    }
}

#[test]
fn only_a_yes_is_kept_for_the_process() {
    let kept = AtomicBool::new(false);
    let asked = std::cell::Cell::new(0);
    let ask = |answer: bool| {
        kept_if_found(&kept, || {
            asked.set(asked.get() + 1);
            answer
        })
    };
    assert!(!ask(false));
    assert!(!ask(false), "a probe that heard nothing is asked again");
    assert!(ask(true));
    assert!(ask(false), "kept");
    assert_eq!(asked.get(), 3);
}

/// Every Mac syncs the base plan — Permissions, the Secrets vault lane and Spotlight — so
/// sync is on by default (§8.1); the workstation preferences, Dock, Repositories and the
/// sign-outs wait for the ALab signal. The negative control is the base plan's Secrets and
/// Spotlight bits, which an old build left out of a Mac outside ALab (Permissions alone).
#[test]
fn a_mac_outside_alab_syncs_the_base_plan_and_waits_for_the_signal() {
    let full = scoped(Plan::default(), true);
    assert_eq!(full.selection, Selection::all());
    let elsewhere = scoped(Plan::default(), false);
    let base = [
        Setting::Permissions,
        Setting::Secrets,
        Setting::Spotlight,
        Setting::Repositories,
    ];
    for setting in Setting::ALL {
        assert_eq!(
            elsewhere.selection.contains(setting),
            base.contains(&setting),
            "{setting:?}"
        );
    }
    // The rest of the profile waits for the signal.
    for gated in [
        Setting::CapsLock,
        Setting::NaturalScrolling,
        Setting::TrackpadScrolling,
        Setting::Desktop,
        Setting::Widgets,
        Setting::Dock,
        Setting::Messages,
        Setting::FaceTime,
    ] {
        assert!(!elsewhere.selection.contains(gated), "{gated:?}");
    }
    assert_eq!(elsewhere.vault_url, Plan::default().vault_url);
}

#[test]
fn fleet_membership_expands_only_automatic_plans_and_respects_holds() {
    let narrow = scoped(Plan::default(), false);
    let records = |provisional: bool, decision: Decision| Records {
        plan: Some((narrow.clone(), true)),
        progress: None,
        decision,
        restored: None,
        provisional,
        stop_request: None,
        cancelled: None,
        person_access: false,
    };
    let widened = upgrade_of(&records(true, Decision::Pending), || true).unwrap();
    assert_eq!(widened.selection, Selection::all());
    assert_eq!(widened.vault_url, narrow.vault_url);
    assert_eq!(
        upgrade_of(&records(true, Decision::Pending), || false),
        None
    );
    assert_eq!(
        upgrade_of(&records(false, Decision::Pending), || true),
        None,
        "fleet membership preserves the person's plan"
    );
    assert_eq!(
        upgrade_of(&records(true, Decision::Stopped(Stop::Cancelled)), || true),
        None,
        "Cancel stands"
    );
    let full = Records {
        plan: Some((Plan::default(), true)),
        ..records(true, Decision::Pending)
    };
    assert_eq!(upgrade_of(&full, || true), None);
    let mut stopped = Progress::for_plan(&narrow);
    stopped.admitted = true;
    stopped.stopped = true;
    let stopped = Records {
        progress: Some(stopped),
        ..records(true, Decision::Pending)
    };
    assert_eq!(upgrade_of(&stopped, || true), None, "Stop stands");
    let mut asked = Progress::for_plan(&narrow);
    asked.admitted = true;
    let asked = Records {
        progress: Some(asked),
        stop_request: Some(StopRequest::Revision(asked.revision)),
        ..records(true, Decision::Pending)
    };
    assert_eq!(
        upgrade_of(&asked, || true),
        None,
        "a Stop stands from the moment it is asked for"
    );
    let mut finished = Progress::for_plan(&narrow);
    finished.admitted = true;
    finished.completed = true;
    let unreadable_stop = Records {
        progress: Some(finished),
        stop_request: Some(StopRequest::Unreadable),
        ..records(true, Decision::Pending)
    };
    assert!(
        !unreadable_stop.stopped_run(),
        "a completed run alone would ignore a late Stop request"
    );
    assert_eq!(
        upgrade_of(&unreadable_stop, || true),
        None,
        "an unreadable Stop remains held even after completion"
    );
}

/// The ALab signal is only an `In` membership of the ALab vault, held under this Mac's own
/// recipient key (§8.1). A `Pending` request to the ALab address starts a join and nothing
/// more; `Plain`, another vault and an unconfigured login never count. The negative controls
/// are the two that stop being the signal with SYNC-1: a `Pending` request to the ALab vault,
/// which the interim counted, and which an old build still reads as the signal.
#[test]
fn only_an_in_membership_of_the_alab_vault_is_the_alab_signal() {
    use aterm_secrets::client::{Config, Membership, Status};
    use aterm_secrets::transport::Source;
    let alab: [u8; 32] = aterm_secrets::b64::decode_padded(aterm_secrets::ALAB_SIGNER)
        .unwrap()
        .try_into()
        .unwrap();
    let status = |membership: Membership, source: Source, signer: Option<[u8; 32]>| Status {
        config: Some(Config {
            source,
            mirrors: Vec::new(),
            name: "alab".into(),
            signer,
            followed: Vec::new(),
            host: "mac".into(),
        }),
        machine: None,
        membership,
        names: Vec::new(),
    };
    let url = |text: &str| Source::Url(text.into());
    let default = || url(aterm_types::macos_setup::DEFAULT_VAULT_URL);
    let other = || url("https://vault.example/v1/team");
    let in_ = || Membership::In { seq: 3, heard: 0 };
    // An `In` membership of the ALab vault, wherever it is served from, is the signal.
    assert!(member_of_the_alab_vault(&status(
        in_(),
        default(),
        Some(alab)
    )));
    assert!(member_of_the_alab_vault(&status(
        in_(),
        other(),
        Some(alab)
    )));
    // A pending request to the ALab vault starts a join only; it is not the signal.
    assert!(!member_of_the_alab_vault(&status(
        Membership::Pending,
        default(),
        Some(alab)
    )));
    // Another vault of one's own, a plain file of one's own, or one at the ALab address
    // under another key is not ALab's.
    assert!(!member_of_the_alab_vault(&status(
        in_(),
        other(),
        Some([7; 32])
    )));
    assert!(!member_of_the_alab_vault(&status(
        in_(),
        default(),
        Some([7; 32])
    )));
    assert!(!member_of_the_alab_vault(&status(
        Membership::Plain,
        other(),
        None
    )));
    assert!(!member_of_the_alab_vault(&status(
        Membership::Plain,
        Source::Path("/Users//someone/home.secrets".into()),
        None
    )));
    // Anyone can ask to join some vault, or the ALab key at another address.
    assert!(!member_of_the_alab_vault(&status(
        Membership::Pending,
        other(),
        Some(alab)
    )));
    assert!(!member_of_the_alab_vault(&status(
        Membership::Pending,
        default(),
        Some([7; 32])
    )));
    assert!(!member_of_the_alab_vault(&status(
        Membership::Pending,
        default(),
        None
    )));
    assert!(!member_of_the_alab_vault(&status(
        Membership::Unconfigured,
        default(),
        Some(alab)
    )));
}

#[test]
fn a_vault_that_did_not_answer_is_not_called_absent() {
    assert_eq!(VaultHere::Absent.note(), Some(NO_VAULT));
    assert_eq!(VaultHere::Unreached.note(), Some(NOT_REACHED));
    assert_eq!(VaultHere::Found.note(), None);
    assert_eq!(VaultHere::Unasked.note(), None);
    for here in [
        VaultHere::Unasked,
        VaultHere::Found,
        VaultHere::Absent,
        VaultHere::Unreached,
    ] {
        // Only what leaves the vault off this network has a note to say so.
        assert_eq!(here.not_here(), here.note().is_some(), "{here:?}");
    }
}

/// A Restore makes the plan a person's (design §3.4), so no later launch widens it over the
/// groups the Restore put back, even where it left aterm's own provisional plan of
/// Permissions alone as it was; and Set up now from the page afterwards keeps it so. Widening
/// would set Keyboard & Trackpad and Desktop & Dock again and sign Messages and FaceTime out,
/// which nothing undoes.
#[test]
fn a_restored_provisional_plan_is_never_widened_after_set_up_now() {
    let root = aterm_tempfile::tempdir().unwrap();
    let narrow = provisional(root.path());
    assert!(widens_at(root.path()).unwrap(), "aterm's own plan widens");
    // Restore previous settings (macos_doctor::restore's two writes).
    save_restore(root.path()).unwrap();
    leave_restored_as_is(root.path()).unwrap();
    assert!(
        !saved_plan_provisional(root.path()).unwrap(),
        "the Restore saved the plan as a person's"
    );
    assert!(!widens_at(root.path()).unwrap(), "a Restore holds it");
    // Set up now, from the page that shows the plan in force.
    let shown = snapshot_at(root.path(), narrow.clone()).unwrap().plan;
    observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        InitialSetupRestart::default()
            .changing(Some(Selection::none()))
            .persist_with(
                root,
                &shown,
                false,
                || Ok(None),
                save_progress,
                |root| observation_at(root).map_err(|error| error.to_string()),
            )
    })
    .result
    .unwrap()
    .expect("admitted");
    assert!(
        !widens_at(root.path()).unwrap(),
        "after a Restore and Set up now the plan is still a person's: {}",
        std::fs::read_to_string(root.path().join("automatic-plan.json")).unwrap()
    );
}

/// A Restore's record holds the ALab upgrade for as long as it stands, whatever decision an
/// admission since wrote over it: a provisional plan beside it never widens.
#[test]
fn a_restore_record_holds_the_alab_upgrade_while_it_stands() {
    let root = aterm_tempfile::tempdir().unwrap();
    provisional(root.path());
    assert!(widens_at(root.path()).unwrap());
    for state in [RestoredState::Pending, RestoredState::Applied] {
        save_restored_decision(root.path(), state, Restored::at_restore(None)).unwrap();
        assert!(saved_plan_provisional(root.path()).unwrap());
        assert!(!widens_at(root.path()).unwrap(), "{state:?}");
    }
}

/// A provisional plan an older aterm saved before every Mac got the base plan (aterm's
/// permissions alone, releases v0.130.0 to v0.144), on a Mac that is not yet an ALab member:
/// it widens to the base plan whatever the ALab answer is, so the Mac syncs (its Secrets
/// lane), and an ALab Mac can join, become `In` and widen to the whole profile. A plan a
/// person shaped stays exactly as it is, and the base plan itself widens no further.
#[test]
fn an_older_builds_permissions_only_provisional_plan_reaches_the_base_plan() {
    let root = aterm_tempfile::tempdir().unwrap();
    let old = permissions_only_provisional(root.path());
    let records = observation_at(root.path()).unwrap().records;
    assert!(records.provisional, "precondition: aterm's own plan");
    assert!(
        records.progress.as_ref().is_some_and(|progress| {
            progress.admitted && progress.completed && progress.belongs_to(&old)
        }),
        "precondition: its run finished"
    );
    assert!(!keeps_vault_sync(&records), "precondition: it never syncs");
    let reached = upgrade_of(&records, || false).expect("widens with no ALab signal");
    assert_eq!(reached.selection, aterm_types::macos_setup::BASE_PLAN);
    assert!(reached.selection.contains(Setting::Repositories));
    assert_eq!(reached.vault_url, old.vault_url);
    assert_eq!(
        upgrade_of(&records, || true).map(|plan| plan.selection),
        Some(Selection::all()),
        "the ALab signal still widens it to the whole profile"
    );
    assert!(widens_at(root.path()).unwrap(), "the watch has work");
    assert_eq!(
        widened_at(root.path(), || false, || Ok(None))
            .unwrap()
            .map(|(_, widened)| widened.selection),
        Some(aterm_types::macos_setup::BASE_PLAN)
    );
    let (mut ticket, base) = prepare_upgrade(root.path(), || false, || Ok(None))
        .unwrap()
        .unwrap();
    ticket
        .upgrade_at(root.path(), &base)
        .result
        .unwrap()
        .unwrap();
    let records = observation_at(root.path()).unwrap().records;
    assert!(keeps_vault_sync(&records), "the base plan syncs");
    assert_eq!(
        upgrade_of(&records, || false),
        None,
        "nothing past the base"
    );

    let person = aterm_tempfile::tempdir().unwrap();
    authorize_at(person.path(), &old).unwrap().unwrap();
    save_plan_as(person.path(), &old, false).unwrap();
    let records = observation_at(person.path()).unwrap().records;
    assert_eq!(
        upgrade_of(&records, || false),
        None,
        "a plan a person shaped stays as it is"
    );
}

#[test]
fn an_interrupted_base_upgrade_recovers_and_can_later_widen_on_the_alab_signal() {
    let root = aterm_tempfile::tempdir().unwrap();
    let base = interrupted_base_upgrade(root.path());
    let recovered = observation_at(root.path()).unwrap().records;
    let pending = recovered.progress.unwrap();
    assert!(pending.belongs_to(&base));
    assert!(!pending.admitted && !pending.completed && !pending.stopped);
    assert_eq!(pending.done, Stage::Permissions.bit());
    assert!(keeps_vault_sync(&recovered));
    assert!(
        !owes_secrets_stage(&recovered),
        "recovery alone admits no effects"
    );

    // A fresh startup has no ticket from the interrupted process. It admits its recovered
    // offer through the ordinary automatic path, preserving the already-finished grants.
    let mut startup = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    assert_eq!(
        startup.offer_reading(root.path(), || Ok(None)).unwrap(),
        base
    );
    let admitted = startup
        .persist_for_watch_test(root.path(), &base, false, None)
        .unwrap()
        .expect("the recovered automatic offer is admitted");
    let records = observation_at(root.path()).unwrap().records;
    let running = records.progress.unwrap();
    assert_eq!(running.revision, admitted);
    assert!(running.admitted && !running.completed && running.belongs_to(&base));
    assert_eq!(running.done, Stage::Permissions.bit());
    assert!(keeps_vault_sync(&records) && owes_secrets_stage(&records));
    assert!(saved_plan_provisional(root.path()).unwrap());

    let (mut upgrade, full) = prepare_upgrade(root.path(), || true, || Ok(None))
        .unwrap()
        .expect("membership later widens the recovered base plan");
    assert_eq!(full.selection, Selection::all());
    upgrade
        .upgrade_at(root.path(), &full)
        .result
        .unwrap()
        .unwrap();
    let full_run = progress(root.path()).unwrap().unwrap();
    assert!(full_run.admitted && full_run.belongs_to(&full));
    assert_eq!(full_run.done, Stage::Permissions.bit());
}

#[test]
fn an_interrupted_base_upgrade_preserves_its_admitted_configured_vault() {
    let root = aterm_tempfile::tempdir().unwrap();
    let vault = configured();
    let base = interrupted_base_upgrade_in_vault(root.path(), Some(vault.clone()));
    assert_ne!(base.vault_url, Plan::default().vault_url);
    let recovered = progress(root.path()).unwrap().unwrap();
    assert!(recovered.belongs_to(&base) && !recovered.admitted);
    assert_eq!(
        recovered.admitted_vault,
        AdmittedVault::Configured(vault.identity())
    );

    let mut startup = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    assert_eq!(
        startup
            .offer_reading(root.path(), || Ok(Some(vault.clone())))
            .unwrap(),
        base
    );
    startup
        .persist_for_watch_test(root.path(), &base, false, None)
        .unwrap()
        .unwrap();
    let admitted = progress(root.path()).unwrap().unwrap();
    assert!(admitted.admitted && admitted.belongs_to(&base));
    assert_eq!(
        admitted.admitted_vault,
        AdmittedVault::Configured(vault.identity())
    );
    assert_eq!(
        aterm_machines::init_secrets::selected_vault(
            &base.vault_url,
            &vault.home,
            Some(&vault.config),
        )
        .unwrap(),
        Some(&vault.config),
        "recovery retains the vault configured before the interrupted admission"
    );
}

#[test]
fn an_interrupted_base_upgrade_preserves_stop_and_restore_holds() {
    for stop in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        interrupted_base_upgrade(root.path());
        if stop {
            let pending = progress(root.path()).unwrap().unwrap();
            request_stop_at(root.path(), pending.revision).unwrap();
        } else {
            save_restored_decision(
                root.path(),
                RestoredState::Applied,
                Restored::at_restore(None),
            )
            .unwrap();
        }
        let held = observation_at(root.path()).unwrap();
        assert!(
            prepare_upgrade(
                root.path(),
                || true,
                || panic!("a standing hold reads no vault"),
            )
            .unwrap()
            .is_none(),
            "stop: {stop}"
        );
        assert_eq!(observation_at(root.path()).unwrap(), held);
    }
}

#[test]
fn a_partial_upgrade_recovery_rejects_unproven_or_unadmitted_expansions() {
    for change in [
        "missing predecessor",
        "changed predecessor",
        "chosen",
        "extra setting",
        "unadmitted vault",
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut base = interrupted_base_upgrade(root.path());
        let path = root.path().join("automatic-plan.json");
        let mut record: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let fields = record.as_object_mut().unwrap();
        let upgrade = fields.get_mut("upgrade").unwrap().as_object_mut().unwrap();
        match change {
            "missing predecessor" => {
                upgrade.remove("previous_plan");
            }
            "changed predecessor" => {
                upgrade
                    .get_mut("previous_plan")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("selection".into(), Value::from(Selection::none().bits()));
            }
            "chosen" => {
                upgrade
                    .get_mut("previous_plan")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("provisional".into(), Value::Bool(false));
            }
            "extra setting" => {
                base.selection.set(Setting::Desktop, true);
                let mut pending = parse_progress(upgrade.get("pending").unwrap()).unwrap();
                pending.plan = plan_digest(&base);
                pending.selection = Some(base.selection);
                upgrade.insert("pending".into(), progress_checkpoint(pending));
                fields.insert("selection".into(), Value::from(base.selection.bits()));
            }
            "unadmitted vault" => {
                base.vault_url = "https://substituted.example/v1/team".into();
                let mut pending = parse_progress(upgrade.get("pending").unwrap()).unwrap();
                assert_eq!(pending.admitted_vault, AdmittedVault::None);
                pending.plan = plan_digest(&base);
                upgrade.insert("pending".into(), progress_checkpoint(pending));
                fields.insert("vault_url".into(), Value::from(base.vault_url));
            }
            _ => unreachable!(),
        }
        write_json(&path, &record).unwrap();
        assert!(observation_at(root.path()).is_err(), "{change}");
    }
}

/// NEGATIVE CONTROL: widening an older provisional setup carries over its admitted
/// Permissions approval across `UpgradeProgress::save` (including an interrupted retry), while
/// an older unverified provisional setup carries nothing.
#[test]
fn widening_an_older_provisional_setup_carries_its_permissions_approval_and_retry_prefix() {
    let strip_older = |root: &Path| {
        let path = root.join("initial-setup.json");
        let mut record: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record.as_object_mut().unwrap().remove(PROGRESS_BY_PRESS);
        write_json(&path, &record).unwrap();
        let _ = std::fs::remove_file(root.join(PERSON_ACCESS));
    };

    let root = aterm_tempfile::tempdir().unwrap();
    provisional(root.path());
    note_person_asked_at(root.path()).unwrap();
    strip_older(root.path());
    assert_eq!(person_access_at(root.path()), PersonAccess::Older);
    assert!(
        initial_setup_snapshot_at(root.path(), Plan::default())
            .unwrap()
            .permissions_allowed
    );

    let (mut ticket, full) = prepare_upgrade(root.path(), || true, || Ok(None))
        .unwrap()
        .unwrap();
    assert!(
        ticket
            .persist_for_watch_test(root.path(), &full, true, Some((1, false)))
            .is_err()
    );
    let widened = ticket
        .upgrade_at(root.path(), &full)
        .result
        .unwrap()
        .expect("retry after UpgradePlan matches prefix");
    assert!(widened > 0);
    assert_eq!(person_access_at(root.path()), PersonAccess::Asked);
    assert!(
        initial_setup_snapshot_at(root.path(), Plan::default())
            .unwrap()
            .permissions_allowed
    );

    let unverified = aterm_tempfile::tempdir().unwrap();
    provisional(unverified.path());
    strip_older(unverified.path());
    write_json(
        &unverified.path().join("first-run.json"),
        &json!({"profile":PROFILE,"state":"applied","accounts_verified":false}),
    )
    .unwrap();
    let (mut ticket, full) = prepare_upgrade(unverified.path(), || true, || Ok(None))
        .unwrap()
        .unwrap();
    ticket
        .upgrade_at(unverified.path(), &full)
        .result
        .unwrap()
        .unwrap();
    assert_eq!(person_access_at(unverified.path()), PersonAccess::NotAsked);
    assert!(
        initial_setup_snapshot_at(unverified.path(), Plan::default())
            .unwrap()
            .permissions_allowed
    );
}
