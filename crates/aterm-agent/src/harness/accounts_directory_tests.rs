// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::accounts::{Binding, Class, Cli};
use std::collections::BTreeMap;

fn row(label: &str, dir: Option<&Path>) -> Row {
    Row {
        label: label.into(),
        class: Class::Dedicated,
        dir: dir.map(|path| path.to_string_lossy().into_owned()),
        ..Row::default()
    }
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut pending = vec![root.to_path_buf()];
    let mut result = BTreeMap::new();
    while let Some(path) = pending.pop() {
        for item in std::fs::read_dir(path).unwrap() {
            let item = item.unwrap();
            if item.file_type().unwrap().is_dir() {
                pending.push(item.path());
            } else if item.file_type().unwrap().is_file() {
                result.insert(item.path(), std::fs::read(item.path()).unwrap());
            }
        }
    }
    result
}

fn directory_case(case: i64) -> (i64, i64) {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let store = Store::at_for(&tmp.path().join("state"), Cli::Codex, &home);
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let first = data.join("first");
    let second = data.join("second");
    let mut new = vec![row("new", Some(&first))];
    let mut existing = Roster::default();
    let mut logins = Logins::default();
    let mut held = Binding::default();
    match case {
        1 => {}
        2 | 6 => {
            std::fs::create_dir_all(first.join("child")).unwrap();
            held.dir = Some(
                if case == 6 {
                    first.join("child/..")
                } else {
                    first.clone()
                }
                .to_string_lossy()
                .into_owned(),
            );
        }
        3 => held.was.push(Some(first.to_string_lossy().into_owned())),
        4 => existing.rows.push(row("kept", Some(&first))),
        5 => new.push(row("also", Some(&first))),
        7 => held.dir = Some(first.to_string_lossy().into_owned()),
        8 => held.dir = Some(data.join("FIRST").to_string_lossy().into_owned()),
        9 => new[0].dir = Some(data.join("missing-λ").to_string_lossy().into_owned()),
        10 => std::fs::write(&first, "inert file, not a directory").unwrap(),
        11 => new[0].dir = None,
        12 => {
            new[0].dir = None;
            held.dir = Some(store.default_home().unwrap().to_string_lossy().into_owned());
        }
        13 => new[0].dir = Some(store.default_home().unwrap().to_string_lossy().into_owned()),
        14 | 16 => {
            if case == 14 {
                std::fs::create_dir(&second).unwrap();
            }
            held.dir = Some(second.to_string_lossy().into_owned());
        }
        15 => {
            let unicode = data.join("existing-λ");
            std::fs::create_dir(&unicode).unwrap();
            new[0].dir = Some(unicode.to_string_lossy().into_owned());
        }
        _ => unreachable!(),
    }
    if matches!(case, 2 | 3 | 6 | 7 | 8 | 11 | 12 | 13 | 14 | 16) {
        logins.labels.insert("old".into(), held);
    }
    store.save_logins(&logins).unwrap();
    let before = files(tmp.path());
    let admitted = store.check_new_rows(&new, &existing, 10).is_ok();
    (i64::from(admitted), i64::from(files(tmp.path()) == before))
}

pub(crate) fn directory_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Check" => {
            let (accepted, unchanged) = directory_case(state["case"]);
            next.insert("accepted", accepted);
            next.insert("unchanged", unchanged);
            next.insert("phase", 1);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn directory_admission_conforms_to_retained_ownership_model() {
    let model = aterm_models_harness::account_directory_admission_model();
    assert_eq!(model.mutant_dials(), (1..=8).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = directory_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
            }
        }
    }
    let mut bad = model.init_state();
    bad.insert("case", 2);
    bad.insert("accepted", 1);
    assert!(!model.check_invariant("NoDirectoryTransfer", &bad));
    bad.insert("unchanged", 0);
    assert!(!model.check_invariant("AdmissionOnlyReads", &bad));
}

#[cfg(unix)]
#[test]
fn directory_admission_resolves_links_before_missing_names_and_refuses_broken_links() {
    use std::os::unix::fs::symlink;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(&tmp.path().join("state"));
    let real = tmp.path().join("real");
    let linked = tmp.path().join("linked");
    std::fs::create_dir(&real).unwrap();
    symlink(&real, &linked).unwrap();
    let mut logins = Logins::default();
    logins.labels.insert(
        "old".into(),
        Binding {
            dir: Some(real.join("future").to_string_lossy().into_owned()),
            ..Binding::default()
        },
    );
    store.save_logins(&logins).unwrap();
    let new = row("new", Some(&linked.join("future")));
    assert!(
        store
            .check_new_rows(&[new], &Roster::default(), 10)
            .is_err()
    );
    let rows = [
        row("first", Some(&real.join("future/leaf"))),
        row("second", Some(&linked.join("future/leaf"))),
    ];
    assert_eq!(
        retained_directory_owner(
            &Logins::default(),
            &rows,
            "first",
            rows[0].dir.as_deref().unwrap()
        )
        .unwrap(),
        Some("second")
    );
    assert!(!real.join("future").exists());
    assert!(Location::of(Some(real.join("missing/../other").to_str().unwrap()), None).is_err());
    let dangling = tmp.path().join("dangling");
    symlink(tmp.path().join("absent"), &dangling).unwrap();
    assert!(Location::of(Some(dangling.join("future").to_str().unwrap()), None).is_err());
    let looping = tmp.path().join("looping");
    symlink(&looping, &looping).unwrap();
    assert!(Location::of(Some(looping.to_str().unwrap()), None).is_err());
}
