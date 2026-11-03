// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn fixture() -> (aterm_tempfile::TempDir, Staging, PathBuf) {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let root = dir.path().join("live");
    let staging = Staging {
        apply_lock: root.join("apply.lock"),
        stage_lock: root.join("stage.lock"),
        download: root.join("download"),
        staged_app: root.join("staged/aterm.app"),
        ready: root.join("ready.toml"),
        status: root.join("status.toml"),
        root,
    };
    let incoming = dir.path().join("private/incoming.app");
    std::fs::create_dir_all(&staging.staged_app).unwrap();
    std::fs::create_dir_all(&incoming).unwrap();
    std::fs::write(staging.staged_app.join("identity"), "old").unwrap();
    std::fs::write(incoming.join("identity"), "new").unwrap();
    std::fs::write(&staging.ready, "old-marker").unwrap();
    (dir, staging, incoming)
}

fn old_pair(staging: &Staging) {
    assert_eq!(
        std::fs::read_to_string(staging.staged_app.join("identity")).unwrap(),
        "old"
    );
    assert_eq!(
        std::fs::read_to_string(&staging.ready).unwrap(),
        "old-marker"
    );
}

#[test]
fn local_candidate_rename_failure_keeps_the_prior_pair() {
    let (_dir, staging, incoming) = fixture();
    std::fs::remove_dir_all(&incoming).unwrap();
    assert!(
        publish_local_pair(&staging, &incoming, "new-marker", |_, _| {
            panic!("no marker write after failed exchange")
        })
        .is_err()
    );
    old_pair(&staging);
}

#[test]
fn local_marker_failure_before_or_after_atomic_replacement_restores_prior_pair() {
    for replace_first in [false, true] {
        let (_dir, staging, incoming) = fixture();
        let result = publish_local_pair(&staging, &incoming, "new-marker", |path, bytes| {
            if replace_first {
                crate::manifest::write_durable(path, bytes, "test marker", Flush::Ordered)?;
            }
            Err("injected actual marker writer failure".into())
        });
        assert!(result.is_err());
        old_pair(&staging);
        assert_eq!(
            std::fs::read_to_string(incoming.join("identity")).unwrap(),
            "new"
        );
        assert!(
            incoming
                .parent()
                .unwrap()
                .join("local-stage-transaction.toml")
                .exists()
        );
    }
}

#[test]
fn local_success_keeps_old_candidate_custody_until_caller_retires_owned_scratch() {
    let (_dir, staging, incoming) = fixture();
    publish_local_pair(&staging, &incoming, "new-marker", |path, bytes| {
        crate::manifest::write_durable(path, bytes, "test marker", Flush::Ordered)
    })
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(staging.staged_app.join("identity")).unwrap(),
        "new"
    );
    assert_eq!(
        std::fs::read_to_string(&staging.ready).unwrap(),
        "new-marker"
    );
    assert_eq!(
        std::fs::read_to_string(incoming.join("identity")).unwrap(),
        "old"
    );
    assert_eq!(
        std::fs::read_to_string(
            incoming
                .parent()
                .unwrap()
                .join("local-stage-prior-ready.toml")
        )
        .unwrap(),
        "old-marker"
    );
}
