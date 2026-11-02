// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::Lab;
use aterm_secrets::commons::roster::Role;
use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

fn files(root: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
    let mut result = BTreeMap::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            result.insert(path.clone(), (meta.permissions().mode(), Vec::new()));
            result.extend(files(&path));
        } else {
            result.insert(
                path,
                (
                    meta.permissions().mode(),
                    std::fs::read(entry.path()).unwrap(),
                ),
            );
        }
    }
    result
}

#[test]
fn report_identifies_the_admitted_current_writer_without_changing_files() {
    let lab = Lab::new(&[Role::Writer]);
    let binding = lab.bootstrap(0).request().unwrap().binding().clone();
    let before = files(lab._home.path());
    let report = read_at(&lab.paths[0], Instant::now() + HANG).unwrap();
    assert_eq!(report["bindings"], json!([identity(&binding)]));
    assert_eq!(report["current"], identity(&binding));
    assert_eq!(report["current"]["actor"].as_str().unwrap().len(), 96);
    assert_eq!(report["grants_authority"].as_bool(), Some(false));
    assert_eq!(files(lab._home.path()), before);
}

#[test]
fn report_preserves_the_authenticated_predecessor_order_and_refuses_incomplete_chain() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    let before_binding = lab.bootstrap(0).request().unwrap().binding().clone();
    let after_binding = lab.bootstrap(1).request().unwrap().binding().clone();
    {
        let bootstrap = lab.bootstrap(0);
        let session = crate::table::Session::open(&bootstrap).unwrap();
        let directory = crate::catalog::Directory::new(&original, bootstrap.trust());
        let descriptor = crate::catalog::Descriptor::application(
            bootstrap.trust(),
            "lineage-test",
            "records",
            1,
        )
        .unwrap();
        let scope = directory.publish(&session, &descriptor).unwrap();
        let mut store =
            crate::table::store::Store::open(&session, &directory.path(scope).unwrap(), scope)
                .unwrap();
        store
            .commit_catalog(&session, b"retained lineage fixture record")
            .unwrap();
    }
    let config = lab._home.path().join("aterm.toml");
    std::fs::write(&config, "").unwrap();
    let selected = crate::test_complete_writer_successor(
        lab._home.path(),
        &original,
        &lab.paths[1],
        &config,
        Instant::now() + HANG,
    )
    .unwrap();
    let before = files(lab._home.path());
    let report = read_at(&original, Instant::now() + HANG).unwrap();
    assert_eq!(
        report["bindings"],
        json!([identity(&before_binding), identity(&after_binding)])
    );
    assert_eq!(report["current"], identity(&after_binding));
    assert_eq!(files(lab._home.path()), before);

    let old_lock = original.dir.join("commons.lock");
    std::fs::set_permissions(&old_lock, std::fs::Permissions::from_mode(0o644)).unwrap();
    let legacy = files(lab._home.path());
    assert!(read_at(&original, Instant::now() + HANG).is_err());
    assert_eq!(files(lab._home.path()), legacy);
    std::fs::set_permissions(&old_lock, std::fs::Permissions::from_mode(0o600)).unwrap();

    let selection = original.dir.join("commons/writer-generation/selection");
    let saved = std::fs::read(&selection).unwrap();
    let mut wrong: Value = aterm_json::from_slice(&saved).unwrap();
    wrong.as_object_mut().unwrap().insert(
        "actor".into(),
        Value::from(encode_lower(&after_binding.actor())),
    );
    std::fs::write(&selection, aterm_json::to_vec(&wrong).unwrap()).unwrap();
    let forged = files(lab._home.path());
    assert!(read_at(&original, Instant::now() + HANG).is_err());
    assert_eq!(files(lab._home.path()), forged);
    std::fs::write(&selection, saved).unwrap();

    let pin = selected.dir.join("commons/default.pin");
    std::fs::remove_file(&pin).unwrap();
    let incomplete = files(lab._home.path());
    assert!(read_at(&original, Instant::now() + HANG).is_err());
    assert_eq!(files(lab._home.path()), incomplete);
    assert!(!pin.exists());
}

#[test]
fn invalid_arguments_and_expired_observations_do_not_open_custody() {
    assert!(
        crate::cli::run(&["commons-lineage".into(), "--dir".into(), "/unopened".into()]).is_err()
    );
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("missing"));
    assert!(read_at(&paths, Instant::now()).is_err());
    assert!(read_at(&paths, Instant::now() + HANG).is_err());
    assert!(!paths.home.exists());
    assert!(report(&[]).is_err());
}

#[test]
fn legacy_private_metadata_is_refused_without_normalization() {
    let lab = Lab::new(&[Role::Writer]);
    lab.bootstrap(0);
    let lock = lab.paths[0].dir.join("commons.lock");
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
    let before = files(lab._home.path());
    assert!(read_at(&lab.paths[0], Instant::now() + HANG).is_err());
    assert_eq!(files(lab._home.path()), before);
    assert_eq!(
        std::fs::metadata(lock).unwrap().permissions().mode() & 0o777,
        0o644
    );
}
