// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::VecDeque;

const ENTRY_TEXT: &str = "apple_id = release@example.test\nteam_id = TEAM123\npassword = fake-app-specific-password\nuse = aterm\n";

fn lab(with_entry: bool) -> (aterm_tempfile::TempDir, Paths) {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("alab.vault");
    test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    if with_entry {
        let admin = admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, ENTRY, ENTRY_TEXT.as_bytes())
            .unwrap();
        admin_publish(&paths, &admin, &doc).unwrap();
    }
    (root, paths)
}

#[test]
fn parser_binds_required_fields_and_rejects_bad_input() {
    let parsed = parse_notary(ENTRY_TEXT.as_bytes()).unwrap();
    assert_eq!(parsed.apple_id.as_str(), "release@example.test");
    assert_eq!(parsed.team_id.as_str(), "TEAM123");
    assert_eq!(parsed.password.as_str(), "fake-app-specific-password");
    for bad in [
        "apple_id = a\n",
        "apple_id = a\nteam_id = t\npassword = p\n",
        "apple_id = a\nteam_id = t\nuse = aterm\n",
        "apple_id = a\nteam_id = t\npassword = p\nuse = other\n",
        "apple_id = a\nteam_id = t\npassword = p\nuse = aterm\nextra = x\n",
        "apple_id = a\nteam_id = t\npassword = \"p\"\nuse = aterm\n",
    ] {
        assert!(parse_notary(bad.as_bytes()).is_err(), "{bad}");
    }
}

#[test]
fn existing_profile_is_preserved_without_storing() {
    let (_root, paths) = lab(true);
    let mut history = || Ok(true);
    let mut store =
        |_: &Notary| -> crate::Result<()> { panic!("working profile must not be replaced") };
    assert_eq!(
        materialize_with(&paths, &mut history, &mut store).unwrap(),
        Some("notary profile already authenticates")
    );
}

#[test]
fn missing_profile_is_stored_once_and_rechecked() {
    let (_root, paths) = lab(true);
    let mut answers = VecDeque::from([false, true]);
    let mut history_calls = 0;
    let mut history = || {
        history_calls += 1;
        Ok(answers.pop_front().unwrap())
    };
    let mut store = |notary: &Notary| {
        assert_eq!(notary.apple_id.as_str(), "release@example.test");
        assert_eq!(notary.team_id.as_str(), "TEAM123");
        assert_eq!(notary.password.as_str(), "fake-app-specific-password");
        Ok(())
    };
    assert_eq!(
        materialize_with(&paths, &mut history, &mut store).unwrap(),
        Some("notary profile stored and live-checked")
    );
    assert_eq!(history_calls, 2);
}

#[test]
fn failures_do_not_store_or_falsely_report_success() {
    let (_root, paths) = lab(true);
    let mut history = || Err(Error::new("Apple is unavailable"));
    let mut store = |_: &Notary| -> crate::Result<()> {
        panic!("unavailable history must not replace a profile")
    };
    assert!(materialize_with(&paths, &mut history, &mut store).is_err());

    let mut answers = VecDeque::from([false, false]);
    let mut history = || Ok(answers.pop_front().unwrap());
    let mut store = |_: &Notary| Ok(());
    assert!(materialize_with(&paths, &mut history, &mut store).is_err());
}

#[test]
fn absent_entry_does_not_call_tools() {
    let (_root, paths) = lab(false);
    let mut history = || -> crate::Result<bool> { panic!("absent entry must not check history") };
    let mut store = |_: &Notary| -> crate::Result<()> { panic!("absent entry must not store") };
    assert_eq!(
        materialize_with(&paths, &mut history, &mut store).unwrap(),
        None
    );
}
