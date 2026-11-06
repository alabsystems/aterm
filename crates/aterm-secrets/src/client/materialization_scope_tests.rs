// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const INPUTS: [&str; 2] = ["credential", "inventory"];

fn fixture() -> (aterm_tempfile::TempDir, Paths) {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = root.path().join("team.vault");
    let paths = Paths::for_home(&root.path().join("reader"));
    create(&paths, source.to_str().unwrap(), None, Some("reader")).unwrap();
    publish(&paths, &|doc| {
        for name in INPUTS.into_iter().chain(["unrelated"]) {
            doc.set_entry(&Audience::Everyone, name, b"fixture-first")
                .unwrap();
        }
    });
    (root, paths)
}

fn publish(paths: &Paths, change: &dyn Fn(&mut Document)) {
    let opened = admin_open(paths).unwrap();
    let mut document = opened.doc.clone();
    change(&mut document);
    admin_publish(paths, &opened, &document).unwrap();
    sync(paths, None).unwrap();
}

fn capture(paths: &Paths) -> MaterializationSnapshot {
    let configured = config(paths).unwrap().unwrap();
    with_materialization_ready_for_entries(paths, &configured, &INPUTS, || Ok(()))
        .unwrap()
        .unwrap()
        .snapshot
}

#[test]
fn scoped_materialization_allows_unrelated_updates_but_not_changed_or_missing_inputs() {
    let (_root, paths) = fixture();
    let configured = config(&paths).unwrap().unwrap();
    let broad = with_materialization_ready(&paths, &configured, || Ok(()))
        .unwrap()
        .unwrap();
    let scoped = capture(&paths);
    assert_ne!(
        scoped, broad.snapshot,
        "entry scope is part of snapshot identity"
    );
    publish(&paths, &|doc| {
        doc.set_entry(&Audience::Everyone, "unrelated", b"fixture-next")
            .unwrap();
    });
    assert!(scoped.is_current(&paths).unwrap());
    assert!(
        !broad.snapshot.is_current(&paths).unwrap(),
        "broad callers retain their existing contract"
    );
    for name in INPUTS {
        let then = capture(&paths);
        publish(&paths, &|doc| {
            doc.set_entry(&Audience::Everyone, name, b"fixture-next")
                .unwrap();
        });
        assert!(
            !then.is_current(&paths).unwrap(),
            "changed scoped input {name}"
        );
    }
    let then = capture(&paths);
    publish(&paths, &|doc| {
        doc.unset_entry(&Audience::Everyone, "inventory").unwrap();
    });
    assert!(!then.is_current(&paths).unwrap());
    assert!(
        with_materialization_ready_for_entries::<()>(&paths, &configured, &INPUTS, || {
            panic!("a missing registered input must not reach the reader")
        })
        .unwrap()
        .is_none()
    );
    assert!(with_materialization_ready_for_entries(&paths, &configured, &[], || Ok(())).is_err());
}

#[test]
fn scoped_materialization_still_requires_complete_ready_source_and_membership() {
    for failure in ["incomplete", "source", "revoked"] {
        let (_root, paths) = fixture();
        let captured = capture(&paths);
        match failure {
            "incomplete" => {
                std::fs::remove_file(paths.home.join(".secrets/unrelated")).unwrap();
            }
            "source" => {
                let mut changed = config(&paths).unwrap().unwrap();
                changed.source = Source::Path(paths.home.join("different-vault"));
                save_config(&paths, &changed).unwrap();
            }
            "revoked" => {
                let mut state = load_state(&paths).unwrap();
                state.membership = Stored::Revoked;
                save_state(&paths, &state).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(!captured.is_current(&paths).unwrap(), "{failure}");
    }
}
