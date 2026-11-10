// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn renamed(prepare: &Prepare, binding: &Binding, byte: &str) -> (Prepare, Binding) {
    let mut other = prepare.clone();
    other.id = byte.repeat(16);
    let mut other_binding = binding.clone();
    other_binding.id.clone_from(&other.id);
    (other, other_binding)
}

fn journal_bytes(paths: &Paths) -> Vec<u8> {
    std::fs::read(directory(paths).join("noexec.astream")).unwrap()
}

#[test]
fn closure_reads_each_exact_state_and_the_custody_an_absence_was_read_from() {
    let (_home, paths, prepare, binding) = fixture();
    // Unenrolled: absent from no custody, and neither read creates anything.
    assert_eq!(
        observe_closure_at(&paths, &prepare).unwrap(),
        Closure::Absent(None)
    );
    assert_eq!(custody_at(&paths).unwrap(), None);
    assert!(!paths.dir.exists());
    let step = |prepare: &Prepare, binding: &Binding, event| {
        record_event_at(&paths, prepare, binding, Some(event)).unwrap();
    };
    let state = |prepare: &Prepare| observe_closure_at(&paths, prepare).unwrap();
    step(&prepare, &binding, Event::Prepare);
    let custody = custody_at(&paths).unwrap().expect("an established custody");
    assert_eq!(state(&prepare), Closure::Exact(Recorded::Prepared));
    step(&prepare, &binding, Event::MayExecute);
    assert_eq!(state(&prepare), Closure::Exact(Recorded::MayExecute));
    step(&prepare, &binding, Event::Seal);
    assert_eq!(
        state(&prepare),
        Closure::Exact(Recorded::Sealed { may_execute: true })
    );
    let (open, open_binding) = renamed(&prepare, &binding, "91");
    step(&open, &open_binding, Event::Prepare);
    step(&open, &open_binding, Event::Seal);
    assert_eq!(
        state(&open),
        Closure::Exact(Recorded::Sealed { may_execute: false })
    );
    let (closed, closed_binding) = renamed(&prepare, &binding, "a1");
    step(&closed, &closed_binding, Event::Prepare);
    step(&closed, &closed_binding, Event::NoExec);
    assert_eq!(state(&closed), Closure::Exact(Recorded::NoExec));
    let before = journal_bytes(&paths);
    // Another prepare of a recorded id, a stamped spelling included, is another record.
    let mut changed = prepare.clone();
    changed.deadline_us += 1;
    let mut stamped = prepare.clone();
    stamped.custody = Some(custody.clone());
    for other in [&changed, &stamped] {
        assert_eq!(state(other), Closure::Other);
    }
    // An id no record names is absent from the custody that was read.
    let (absent, _) = renamed(&prepare, &binding, "b1");
    assert_eq!(state(&absent), Closure::Absent(Some(custody.clone())));
    // A self-exec plan is no restart recipe.
    let mut own = prepare.clone();
    let NativePlan::Restart { process, .. } = &prepare.plan else {
        unreachable!()
    };
    own.plan = NativePlan::OwnImage {
        child: process.clone(),
        target: process.image.clone(),
        version: "0.1.0".into(),
    };
    own.duty = crate::harness::contract::Scope::Upgrade;
    own.expected_human_seq = None;
    assert!(observe_closure_at(&paths, &own).is_err());
    assert_eq!(journal_bytes(&paths), before);
    assert_eq!(custody_at(&paths).unwrap(), Some(custody));
}

#[test]
fn a_replaced_custody_reads_as_an_absence_from_another_custody() {
    let (_home, paths, prepare, binding) = fixture();
    record_event_at(&paths, &prepare, &binding, Some(Event::Prepare)).unwrap();
    let original = custody_at(&paths).unwrap().unwrap();
    // The whole vault set aside, as a process's first live start over does.
    std::fs::rename(&paths.dir, paths.home.join("vault-set-aside")).unwrap();
    assert_eq!(
        observe_closure_at(&paths, &prepare).unwrap(),
        Closure::Absent(None)
    );
    assert_eq!(custody_at(&paths).unwrap(), None);
    // A later admission enrolls a new identity's journal: the old prepare is absent from
    // a custody its stamp would not name.
    let (next, next_binding) = renamed(&prepare, &binding, "c1");
    record_event_at(&paths, &next, &next_binding, Some(Event::Prepare)).unwrap();
    let replaced = custody_at(&paths).unwrap().unwrap();
    assert_ne!(replaced, original);
    assert_eq!(
        observe_closure_at(&paths, &prepare).unwrap(),
        Closure::Absent(Some(replaced))
    );
}

#[test]
fn a_journal_enrolled_again_under_the_same_identity_is_another_custody() {
    let (_home, paths, prepare, binding) = fixture();
    // Enrolled with no record yet: no instance to name, so its absence proves nothing.
    let key = write_key(&paths).unwrap();
    drop(open(&paths, &key, LIMITS).unwrap());
    assert_eq!(custody_at(&paths).unwrap(), None);
    assert_eq!(
        observe_closure_at(&paths, &prepare).unwrap(),
        Closure::Absent(None)
    );
    let (first, first_binding) = renamed(&prepare, &binding, "d1");
    record_event_at(&paths, &first, &first_binding, Some(Event::Prepare)).unwrap();
    let original = custody_at(&paths).unwrap().unwrap();
    assert_eq!(
        observe_closure_at(&paths, &prepare).unwrap(),
        Closure::Absent(Some(original.clone()))
    );
    // The journal and its enrollment removed, the identity kept: the next admission enrolls
    // a journal under the identical anchor, whose absence is not the first one's.
    std::fs::remove_dir_all(directory(&paths)).unwrap();
    std::fs::remove_file(paths.dir.join(ENROLLMENT)).unwrap();
    let (next, next_binding) = renamed(&prepare, &binding, "e1");
    record_event_at(&paths, &next, &next_binding, Some(Event::Prepare)).unwrap();
    assert!(
        enrolled(&paths, &key).unwrap(),
        "the same identity's anchor"
    );
    let again = custody_at(&paths).unwrap().unwrap();
    assert_ne!(again, original);
    assert_eq!(
        observe_closure_at(&paths, &prepare).unwrap(),
        Closure::Absent(Some(again))
    );
}
