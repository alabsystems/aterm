// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A key that changed after aterm set it and read it back is set again once, since nothing says
//! who changed it; one that changes again after that stays as it is, and its row says it
//! changed, never whose change it is nor that it is in place.

use super::*;

/// What a run of another revision reads of the held items: the record of an earlier one, as a
/// widened plan, an update's successor, another Mac's profile and a legacy upgrade each read it.
fn as_another_revision(root: &Path) {
    let path = root.join(HELD_RECORD);
    let mut record: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record
        .as_object_mut()
        .unwrap()
        .insert("revision".into(), Value::from(99_u64));
    std::fs::write(&path, aterm_json::to_vec(&record).unwrap()).unwrap();
}

/// The Dock key's writes and the Dock's relaunches so far.
fn dock_calls(host: &Fake, dock: &Target) -> (usize, usize) {
    (
        host.writes.iter().filter(|target| *target == dock).count(),
        host.relaunches("Dock"),
    )
}

/// The Dock's row in `report`.
fn dock_row<'a>(report: &'a Report, dock: &Target) -> &'a Row {
    report
        .rows
        .iter()
        .find(|row| row.name == dock.describe())
        .expect("the Dock's row")
}

/// aterm set the Dock and read it back; something then moved it, and nothing says what. The
/// next pass sets it again, as it sets any key not as wanted, so a reset by macOS or an installer
/// is undone by itself. Moved again after that, it stays as it is: a retry of the same run and a
/// run of another revision make no Dock call, and the row says it changed since aterm set it
/// ([`CHANGED`]), never `ok`, nor whose change it is. Before, the first move was kept, and no
/// unattended pass set the Dock again.
#[test]
fn a_dock_changed_after_a_verified_apply_is_set_again_once_then_left() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dock = wanted()[0].0.clone();
    let mut host = Fake::default();
    automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
    assert_eq!(
        dock_calls(&host, &dock).0,
        1,
        "the first pass sets the Dock once"
    );

    host.values.insert(dock.clone(), PValue::Bool(true));
    let again = automatic_pass(&mut host, root.path(), BUILD, at(1)).expect("retry");
    assert_eq!(
        dock_calls(&host, &dock).0,
        2,
        "the retry sets the moved Dock again"
    );
    assert_eq!(host.values.get(&dock), Some(&wanted()[0].1));
    assert_eq!(dock_row(&again, &dock).status, "applied");
    assert!(
        initial::kept::changed_since_at(root.path()).is_empty(),
        "one change is set again, never kept"
    );

    host.values.insert(dock.clone(), PValue::Bool(true));
    let left = automatic_pass(&mut host, root.path(), BUILD, at(2)).expect("retry");
    assert_eq!(dock_calls(&host, &dock).0, 2, "moved again, it is left");
    assert!(
        initial::kept::changed_since_at(root.path()).contains(Setting::Dock),
        "the record keeps the Dock moved again"
    );
    assert_eq!(dock_row(&left, &dock).status, CHANGED);

    as_another_revision(root.path());
    let report = automatic_pass(&mut host, root.path(), BUILD, at(3)).expect("another revision");
    assert_eq!(
        dock_calls(&host, &dock).0,
        2,
        "another revision makes no Dock call"
    );
    assert_eq!(host.values.get(&dock), Some(&PValue::Bool(true)));
    let row = dock_row(&report, &dock);
    assert_eq!(row.status, CHANGED, "{row:?}");
    assert!(
        row.detail.contains(aterm_types::macos_setup::CHANGED_SINCE)
            && !row.detail.contains("by you"),
        "{row:?}"
    );
    assert!(report.changed_since().contains(&dock.describe().as_str()));
}

/// A run of another revision (an update's successor, a widened plan) reads the Dock moved since
/// an earlier run set it: it sets it again, as the same run's retry would, never leaves it as a
/// choice nobody showed.
#[test]
fn another_revision_sets_a_moved_dock_again() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dock = wanted()[0].0.clone();
    let mut host = Fake::default();
    automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
    host.values.insert(dock.clone(), PValue::Bool(true));
    as_another_revision(root.path());
    automatic_pass(&mut host, root.path(), BUILD, at(1)).expect("another revision");
    assert_eq!(dock_calls(&host, &dock).0, 2);
    assert_eq!(host.values.get(&dock), Some(&wanted()[0].1));
    assert!(initial::kept::changed_since_at(root.path()).is_empty());
}

/// Control: a Restore forgets what changed since aterm set it, so the next pass sets the Dock
/// again.
#[test]
fn a_restore_lets_the_next_pass_set_the_dock_again() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dock = wanted()[0].0.clone();
    let mut host = Fake::default();
    automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
    for minute in [1, 2] {
        host.values.insert(dock.clone(), PValue::Bool(true));
        automatic_pass(&mut host, root.path(), BUILD, at(minute)).expect("retry");
    }
    assert!(initial::kept::changed_since_at(root.path()).contains(Setting::Dock));

    run_mutation(&mut host, root.path(), Action::Restore).expect("restore");
    assert!(
        initial::kept::changed_since_at(root.path()).is_empty(),
        "a Restore forgets the kept keys"
    );
    host.values.insert(dock.clone(), PValue::Bool(true));
    let before = dock_calls(&host, &dock);
    as_another_revision(root.path());
    automatic_pass(&mut host, root.path(), BUILD, at(3)).expect("next pass");
    assert_eq!(
        dock_calls(&host, &dock).0,
        before.0 + 1,
        "the next pass sets the Dock again"
    );
    assert_eq!(host.values.get(&dock), Some(&wanted()[0].1));
}

/// What a pass found of `item`: settled, reading `reads`, left as it was (`drifted`) or changed
/// from `from`.
fn found<'a>(
    item: &'a [String],
    reads: &'a [(String, String)],
    drifted: bool,
    changed: &'a [(String, String)],
    new_targets: &'a [String],
) -> initial::kept::Found<'a> {
    initial::kept::Found {
        settled: item,
        readings: reads,
        drifted: if drifted { item } else { &[] },
        changed,
        new_targets,
        owed: &|_| false,
    }
}

/// A key aterm set again after it changed is kept when it changes once more; a first change, a
/// change only to targets its read-back did not name, and aterm's own want that moved (another
/// build's, which reads as aterm left it) are never kept.
#[test]
fn only_a_key_changed_again_after_aterm_set_it_again_is_kept() {
    let item = vec!["init.dock@me".to_owned()];
    let setting = |_: &str| Some(Setting::Dock);
    let reads = |value: &str| vec![(item[0].clone(), value.to_owned())];
    let from = |value: &str| vec![(item[0].clone(), value.to_owned())];

    let mut kept = initial::kept::Kept::default();
    assert!(kept.note(
        &found(&item, &reads("false"), false, &from("absent"), &[]),
        &setting
    ));
    assert!(
        kept.applied(&item[0]) && !kept.set_again(&item[0]),
        "{kept:?}"
    );
    assert!(kept.note(
        &found(&item, &reads("false"), false, &from("true"), &[]),
        &setting
    ));
    assert!(
        kept.set_again(&item[0]) && kept.changed_since().is_empty(),
        "{kept:?}"
    );
    assert!(kept.note(&found(&item, &reads("true"), true, &[], &[]), &setting));
    assert!(kept.keeps(&item[0]) && kept.changed_since().contains(Setting::Dock));
    assert!(
        !kept.note(&found(&item, &reads("true"), true, &[], &[]), &setting),
        "kept stays kept"
    );

    let mut grows = initial::kept::Kept::default();
    grows.note(
        &found(&item, &reads("a"), false, &from("none"), &[]),
        &setting,
    );
    grows.note(
        &found(&item, &reads("a; b"), false, &from("b"), &item),
        &setting,
    );
    assert!(
        !grows.set_again(&item[0]),
        "a new target is no change of what aterm set: {grows:?}"
    );

    let mut moved = initial::kept::Kept::default();
    moved.note(
        &found(&item, &reads("false"), false, &from("absent"), &[]),
        &setting,
    );
    moved.note(
        &found(&item, &reads("true"), false, &from("false"), &[]),
        &setting,
    );
    assert!(
        !moved.set_again(&item[0]),
        "aterm's want that moved is not a change: {moved:?}"
    );
    moved.note(
        &found(&item, &reads("false"), false, &from("absent"), &[]),
        &setting,
    );
    assert!(moved.set_again(&item[0]));
    assert!(moved.note(&found(&item, &reads("false"), true, &[], &[]), &setting));
    assert!(
        !moved.keeps(&item[0]) && !moved.applied(&item[0]),
        "drifted back to what aterm left is its want moving, let go: {moved:?}"
    );

    let mut owed = initial::kept::Kept::default();
    owed.note(
        &found(&item, &reads("hidden"), false, &from("absent"), &[]),
        &setting,
    );
    let before = owed.clone();
    assert!(
        !owed.note(
            &initial::kept::Found {
                owed: &|_| true,
                ..found(&item, &reads("NC widgets"), true, &[], &[])
            },
            &setting
        ),
        "a difference setup's own step owes is no change"
    );
    assert_eq!(owed, before);
}

/// A record a newer aterm wrote keeps its kept keys and is never written; an unreadable one
/// makes an unattended pass set nothing, and only a Restore's forgetting replaces it.
#[test]
fn a_newer_or_unreadable_kept_record_is_left_and_honored() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let path = root.path().join(initial::kept::KEPT_RECORD);
    let newer = json!({
        "profile": PROFILE,
        "format": 99,
        "applied": [],
        "kept": [{"item": "init.dock@me", "setting": Setting::Dock as u64, "held": null}],
        "later": true,
    });
    let bytes = aterm_json::to_vec(&newer).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let mut kept = initial::kept::Kept::load(root.path()).unwrap();
    assert!(kept.keeps("init.dock@me"));
    kept.forget_all();
    kept.save(root.path()).unwrap();
    initial::kept::forget_at(root.path(), None).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "a newer record is never written"
    );

    std::fs::write(&path, b"{not json").unwrap();
    let mut host = Fake::default();
    assert!(automatic_pass(&mut host, root.path(), BUILD, at(0)).is_err());
    assert!(host.writes.is_empty(), "an unreadable record sets nothing");
    assert!(initial::kept::forget_at(root.path(), Some(Selection::all())).is_err());
    initial::kept::forget_at(root.path(), None).unwrap();
    assert!(initial::kept::Kept::load(root.path()).is_ok());
}

/// A newer aterm's record of the keys that changed since aterm set them names a row this build
/// does not know (a
/// setting the newer build added), under a later format or this build's own (a newer aterm that
/// added the row without a new format). It is a newer aterm's record: an unattended pass still
/// runs, honouring what it can read and leaving the key on the unknown row as it is,
/// and Restore, Set up now and `doctor --apply` never write over it, which would drop the row the
/// key is on. Before, the unknown row made the record unreadable: the pass set nothing, and a
/// Restore replaced the newer record with an empty one.
#[test]
fn a_newer_kept_record_with_a_new_row_is_frozen_not_damaged() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let path = root.path().join(initial::kept::KEPT_RECORD);
    for format in [3, 2, 1] {
        let newer = aterm_json::json!({
            "profile": PROFILE,
            "format": format,
            "applied": [],
            "kept": [
                {"item": "init.new@me", "setting": 63, "held": null},
                {"item": "init.dock@me", "setting": Setting::Dock as u64, "held": null},
            ],
        });
        std::fs::write(&path, aterm_json::to_vec(&newer).unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let kept = initial::kept::Kept::load(root.path()).expect("a newer record reads");
        assert!(
            kept.keeps("init.new@me"),
            "the unknown row's key is still kept (format {format})"
        );
        assert!(kept.keeps("init.dock@me"));
        assert_eq!(kept.changed_since(), {
            let mut dock = Selection::none();
            dock.set(Setting::Dock, true);
            dock
        });
        let mut host = Fake::default();
        let pass = automatic_pass(&mut host, root.path(), BUILD, at(0));
        assert!(pass.is_ok(), "the pass runs: {:?}", pass.err());
        initial::kept::forget_at(root.path(), None).unwrap();
        initial::kept::forget_at(root.path(), Some(Selection::all())).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a newer record is never written (format {format})"
        );
    }

    // One of a later format this build cannot read at all is left as it is too.
    let unreadable = aterm_json::json!({"profile": PROFILE, "format": 3, "kept": "later"});
    std::fs::write(&path, aterm_json::to_vec(&unreadable).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    initial::kept::forget_at(root.path(), None).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

/// A key kept as it changed again after aterm set it again is left as it is (design §6.6):
/// System State reads Changed since aterm set it, and `aterm init` leaves it. `aterm doctor
/// --check` judges the same Mac and reads that key `changed`, not as drift needing attention
/// (exit 1), as it reads a group left as is or out of scope, and never `ok`.
#[test]
fn check_reads_a_key_changed_since_aterm_set_it_as_changed_not_as_drift() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dock = wanted()[0].0.clone();
    let mut host = Fake::default();
    automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
    for minute in [1, 2] {
        host.values.insert(dock.clone(), PValue::Bool(true));
        automatic_pass(&mut host, root.path(), BUILD, at(minute)).expect("retry");
    }
    assert!(
        initial::kept::changed_since_at(root.path()).contains(Setting::Dock),
        "the record keeps the changed Dock"
    );
    // `aterm doctor --check` runs in a process of its own: it holds only what it read.
    host.hold(&[]);
    let report = execute(&mut host, root.path(), Action::Check).expect("check");
    let row = report
        .rows
        .iter()
        .find(|row| row.name == dock.describe())
        .expect("the Dock's row");
    assert!(
        !row.needs_attention() && !report.needs_attention(),
        "doctor --check reads the changed Dock as needing attention: {row:?}"
    );
    assert_eq!(row.status, CHANGED, "{row:?}");
}

/// A sign-out aterm did that reads signed in again took a person's Apple Account: it is kept at
/// once, never signed out again by an unattended pass, and its words say so, while a preference
/// beside it is held as read back, to be set again once. Before, a sign-out was held as read back
/// too, so the next unattended pass would sign the person out again.
#[test]
fn a_sign_out_signed_in_again_is_kept_at_once_and_never_signed_out_again() {
    use aterm_machines::init::Hold;
    let (messages, dock) = ("init.messages@me".to_owned(), "init.dock@me".to_owned());
    let setting = |item: &str| {
        Some(if item.starts_with("init.messages") {
            Setting::Messages
        } else {
            Setting::Dock
        })
    };
    let both = vec![messages.clone(), dock.clone()];
    let read_back = vec![
        (messages.clone(), "signed out".to_owned()),
        (dock.clone(), "as wanted".to_owned()),
    ];
    let mut kept = initial::kept::Kept::default();
    kept.note(&found(&both, &read_back, false, &[], &[]), &setting);

    let mut holds = Vec::new();
    kept.hold(&mut holds, &|_| None);
    let hold = |item: &str| holds.iter().find(|(each, _)| each == item).map(|(_, h)| h);
    assert_eq!(
        hold(&messages),
        Some(&Hold::AsIs(initial::kept::SIGNED_IN.into()))
    );
    assert!(
        matches!(hold(&dock), Some(Hold::ReadBack { again: false, .. })),
        "{holds:?}"
    );

    let signed_in = vec![messages.clone()];
    let reads = vec![(messages.clone(), "account remains registered".to_owned())];
    assert!(kept.note(
        &initial::kept::Found {
            settled: &signed_in,
            readings: &reads,
            drifted: &signed_in,
            changed: &[],
            new_targets: &[],
            owed: &|_| false,
        },
        &setting
    ));
    assert!(kept.keeps(&messages) && kept.changed_since().contains(Setting::Messages));
    assert_eq!(
        kept.kept_holds(),
        [(messages, Hold::AsIs(initial::kept::SIGNED_IN.into()))]
    );
}

/// The record's format before `again` (1) kept a preference on its first change, which is no
/// evidence of a choice: this build reads that key as aterm's, set and read back, so the next
/// pass that finds it otherwise sets it once more; a sign-out it kept stays kept. Before, every
/// key a format-1 record kept stayed kept for good.
#[test]
fn a_format_1_record_sets_its_kept_preference_again_once_and_keeps_its_sign_out() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let legacy = json!({
        "profile": PROFILE,
        "format": 1,
        "applied": [],
        "kept": [
            {"item": "init.dock@me", "setting": Setting::Dock as u64, "held": "as wanted"},
            {"item": "init.messages@me", "setting": Setting::Messages as u64, "held": null},
        ],
    });
    std::fs::write(
        root.path().join(initial::kept::KEPT_RECORD),
        aterm_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    let kept = initial::kept::Kept::load(root.path()).unwrap();
    assert!(!kept.keeps("init.dock@me") && kept.applied("init.dock@me"));
    assert!(!kept.set_again("init.dock@me"), "{kept:?}");
    assert!(kept.keeps("init.messages@me"));
    let mut holds = Vec::new();
    kept.hold(&mut holds, &|_| None);
    assert!(
        holds.iter().any(|(item, hold)| item == "init.dock@me"
            && matches!(hold, aterm_machines::init::Hold::ReadBack { again: false, held: Some(held), .. } if held == "as wanted")),
        "{holds:?}"
    );
}

/// An Accounts pass reads a sign-out kept as signed in again `changed`, left as it is with no
/// try, beside a sign-out that is not done yet: only that one keeps the pass from completing,
/// and the kept one reads neither as a wait nor as a failure.
#[test]
fn an_accounts_pass_leaves_a_kept_sign_out_changed_beside_one_not_done() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut selected = Selection::none();
    selected.set(Setting::Messages, true);
    selected.set(Setting::FaceTime, true);
    authorize_automatic_setup_at(root.path(), selected).unwrap();
    let mut kept = initial::kept::Kept::default();
    kept.kept.push(initial::kept::Key {
        item: "Account sign-out".to_owned(),
        setting: Some(Setting::Messages),
        held: None,
        again: false,
    });
    kept.save(root.path()).unwrap();
    let mut host = Fake {
        accounts_fail: true,
        facetime_blocked: true,
        ..Fake::default()
    };
    let report = apply_part_with(
        root.path(),
        &Inert::default(),
        &mut host,
        BUILD,
        at(0),
        Part::Accounts,
    )
    .expect("pass")
    .expect("the pass ran");
    let status = |name: &str| {
        let row = report.rows.iter().find(|row| row.name == name).unwrap();
        (row.status.clone(), row.detail.clone())
    };
    let (kept_status, kept_detail) = status("Account sign-out");
    assert_eq!(kept_status, CHANGED, "{}", report.to_text());
    assert!(
        kept_detail.ends_with(initial::kept::SIGNED_IN),
        "{kept_detail}"
    );
    assert_eq!(status("FaceTime sign-out").0, "blocked");
    assert!(
        !host
            .events
            .iter()
            .any(|event| event == "journal Account sign-out"),
        "a kept sign-out is never tried again: {:?}",
        host.events
    );
    assert!(!report.complete());
}
