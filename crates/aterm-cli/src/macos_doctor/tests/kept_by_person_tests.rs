// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A key the person changed after aterm set it and read it back stays as they set it.

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

/// aterm set the Dock and read it back; the person then moved it. A retry of the same run and a
/// run of another revision make no Dock call, the value stays the person's, and the row says
/// whose it is. Before the kept record, the other revision's pass wrote the Dock back.
#[test]
fn the_dock_the_person_moved_after_a_verified_apply_stays_moved() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dock = wanted()[0].0.clone();
    let mut host = Fake::default();
    automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
    let applied = dock_calls(&host, &dock);
    assert_eq!(applied.0, 1, "the first pass sets the Dock once");

    host.values.insert(dock.clone(), PValue::Bool(true));
    automatic_pass(&mut host, root.path(), BUILD, at(1)).expect("retry");
    assert_eq!(
        dock_calls(&host, &dock),
        applied,
        "a retry makes no Dock call"
    );
    assert!(
        initial::kept::changed_by_you_at(root.path()).contains(Setting::Dock),
        "the record keeps the person's Dock"
    );

    as_another_revision(root.path());
    let report = automatic_pass(&mut host, root.path(), BUILD, at(2)).expect("another revision");
    assert_eq!(
        dock_calls(&host, &dock),
        applied,
        "another revision makes no Dock call"
    );
    assert_eq!(host.values.get(&dock), Some(&PValue::Bool(true)));
    let row = report
        .rows
        .iter()
        .find(|row| row.name == dock.describe())
        .expect("the Dock's row");
    assert!(
        row.detail
            .contains(aterm_types::macos_setup::CHANGED_BY_YOU),
        "{row:?}"
    );
    assert!(report.changed_since().contains(&dock.describe().as_str()));
}

/// Control: a Restore forgets what the person changed, so the next pass sets the Dock again.
#[test]
fn a_restore_lets_the_next_pass_set_the_dock_again() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dock = wanted()[0].0.clone();
    let mut host = Fake::default();
    automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
    host.values.insert(dock.clone(), PValue::Bool(true));
    automatic_pass(&mut host, root.path(), BUILD, at(1)).expect("retry");
    assert!(initial::kept::changed_by_you_at(root.path()).contains(Setting::Dock));

    run_mutation(&mut host, root.path(), Action::Restore).expect("restore");
    assert!(
        initial::kept::changed_by_you_at(root.path()).is_empty(),
        "a Restore forgets the person's keys"
    );
    host.values.insert(dock.clone(), PValue::Bool(true));
    let before = dock_calls(&host, &dock);
    as_another_revision(root.path());
    automatic_pass(&mut host, root.path(), BUILD, at(2)).expect("next pass");
    assert_eq!(
        dock_calls(&host, &dock).0,
        before.0 + 1,
        "the next pass sets the Dock again"
    );
    assert_eq!(host.values.get(&dock), Some(&wanted()[0].1));
}

/// A key that reads otherwise after aterm read it back is the person's; one that drifted from
/// aterm's want while it still reads as aterm left it is that want moving (another build's), not
/// the person, and the record lets it go.
#[test]
fn only_a_key_the_person_changed_is_kept() {
    let item = "init.dock@me".to_owned();
    let setting = |_: &str| Some(Setting::Dock);
    let settled = std::slice::from_ref(&item);
    let reads = |value: &str| vec![(item.clone(), value.to_owned())];

    let mut kept = initial::kept::Kept::default();
    assert!(kept.note(settled, &reads("false"), &[], &setting));
    assert!(kept.applied(&item) && kept.changed_by_you().is_empty());
    assert!(kept.note(settled, &reads("true"), settled, &setting));
    assert!(kept.keeps(&item) && kept.changed_by_you().contains(Setting::Dock));
    assert!(
        !kept.note(settled, &reads("true"), settled, &setting),
        "kept stays kept"
    );

    let mut moved = initial::kept::Kept::default();
    moved.note(settled, &reads("false"), &[], &setting);
    assert!(moved.note(settled, &reads("false"), settled, &setting));
    assert!(!moved.keeps(&item) && !moved.applied(&item), "{moved:?}");
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
