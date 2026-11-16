// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn invocation() -> Invocation {
    Invocation {
        socket: "/private/native-account.sock".into(),
        session: "t000acc0vnt".into(),
        binding: Binding {
            id: "a".repeat(32),
            capability: "b".repeat(64),
        },
    }
}

fn command(root: &std::path::Path) -> ExecCommand {
    let program = std::env::current_exe().unwrap();
    let auth = root.join("auth.json");
    std::fs::write(&auth, b"{}").unwrap();
    ExecCommand {
        argv0: "codex".into(),
        image: Image::at(&program).unwrap(),
        program: program.to_str().unwrap().into(),
        argv: vec!["exec".into(), "original request\nsecond line".into()],
        auth_path: auth.to_str().unwrap().into(),
        cwd: None,
    }
}

#[test]
fn account_image_invocation_carries_no_command_or_auth_path() {
    let expected = invocation();
    let args = expected.arguments().unwrap();
    assert_eq!(Invocation::parse(&args), Some(expected));
    for extra in ["program=/bin/sh", "auth=/tmp/auth.json", "cap=override"] {
        let mut changed = args.clone();
        changed.push(extra.into());
        assert!(Invocation::parse(&changed).is_none());
    }
    let mut changed = args;
    changed[1] = "contract=2".into();
    assert!(Invocation::parse(&changed).is_none());
}

#[test]
fn account_image_reply_is_distinct_from_upgrade_and_shell_restart() {
    let root = aterm_tempfile::tempdir().unwrap();
    let launch = Reply::Launch {
        command: command(root.path()),
        deadline: 12345,
    };
    let encoded = launch.encode().unwrap();
    assert_eq!(Reply::parse(&encoded), Some(launch));
    assert!(Reply::parse(&encoded.replace("codex-exec-handoff", "native-act")).is_none());
    assert!(Reply::parse(&encoded.replace("12345", "012345")).is_none());
    assert!(Reply::parse(&format!("{} extra=1", encoded.trim())).is_none());
    assert!(
        Reply::parse("OK codex-exec-handoff state=execute deadline_us=1 deadline_us=2").is_none()
    );
}

#[test]
fn account_image_constructor_rejects_changed_image_and_linked_auth() {
    let root = aterm_tempfile::tempdir().unwrap();
    let original = command(root.path());
    validate(&original).unwrap();
    let mut wrong = original.clone();
    wrong.image.inode += 1;
    assert!(validate(&wrong).is_err());
    let link = root.path().join("link.json");
    std::os::unix::fs::symlink(&original.auth_path, &link).unwrap();
    let mut wrong = original;
    wrong.auth_path = link.to_str().unwrap().into();
    assert!(validate(&wrong).is_err());
}

const TAB: &str = "t000acc0vnt";

/// A legacy tab's sid, the longest a mover key takes.
const LONGEST_TAB: &str = "s-1a2b3c4d5e6f70819203";

/// The longest key: the longest tab, and every number at its largest.
fn longest_key() -> String {
    mover_key(LONGEST_TAB, u32::MAX, u64::MAX, u64::MAX).unwrap()
}

/// The digests are SHA-256 as `shasum -a 256` computes it over the same bytes.
#[test]
fn mover_key_has_its_fixed_form() {
    let key = mover_key(TAB, 4242, 1_763_200_000_123_456, 987_654);
    assert_eq!(key.as_deref(), Some("t000acc0vnt/4242-4a1366308fd6"));
    assert_eq!(key, mover_key(TAB, 4242, 1_763_200_000_123_456, 987_654));
    let longest = longest_key();
    assert_eq!(longest, "s-1a2b3c4d5e6f70819203/4294967295-c09292da8ea1");
    assert_eq!(longest.len(), 46);
    assert!(longest.bytes().all(|c| c.is_ascii_graphic()));
}

/// Every tab's sid has a key, in either form the tab-id grammar gives it: a minted one, and a
/// legacy one as long as that grammar makes it.
#[test]
fn mover_key_takes_a_tab_sid_of_either_form() {
    use aterm_types::tab_id::{LEGACY_LEN, TabId, is_legacy, is_tab_id};
    assert!(is_tab_id(TAB) && is_legacy(LONGEST_TAB));
    let legacy = format!("s-{}", "f".repeat(LEGACY_LEN - 2));
    assert!(is_legacy(&legacy));
    let minted = [0, u64::MAX].map(|bits| TabId::mint(|| bits, |_| false));
    let tabs = [
        TAB,
        LONGEST_TAB,
        legacy.as_str(),
        minted[0].as_str(),
        minted[1].as_str(),
    ];
    for tab in tabs {
        let key = mover_key(tab, 4242, 100, 200).unwrap_or_else(|| panic!("{tab}"));
        assert_eq!(key.split_once('/').map(|(tab, _)| tab), Some(tab));
    }
}

#[test]
fn mover_key_refuses_a_tab_that_is_not_a_plain_sid() {
    assert!(mover_key(LONGEST_TAB, 1, 2, 3).is_some());
    assert!(mover_key("t-0", 1, 2, 3).is_some());
    let too_long = format!("{LONGEST_TAB}w");
    for tab in [
        "",
        too_long.as_str(),
        "t_0",
        "t/0",
        "t 0",
        "t.0",
        "t\n0",
        "t\u{e9}",
    ] {
        assert_eq!(mover_key(tab, 1, 2, 3), None, "{tab:?}");
    }
    // A key is no plain sid itself, so it never names a tab.
    let key = mover_key(TAB, 1, 2, 3).unwrap();
    assert_eq!(mover_key(&key, 1, 2, 3), None);
}

#[test]
fn two_births_or_uniqueids_give_two_keys() {
    let key = mover_key(TAB, 4242, 100, 200).unwrap();
    for other in [
        mover_key(TAB, 4242, 101, 200),
        mover_key(TAB, 4242, 100, 201),
        mover_key(TAB, 4243, 100, 200),
        mover_key("t000acc0vnu", 4242, 100, 200),
    ] {
        assert_ne!(other.as_deref(), Some(key.as_str()));
    }
    // The fields hash apart: a birth's last digit never becomes the unique id's first.
    assert_ne!(mover_key(TAB, 1, 23, 4), mover_key(TAB, 1, 2, 34));
}

#[test]
fn record_key_refuses_a_mover_key() {
    let key = mover_key(TAB, 4242, 100, 200).unwrap();
    assert_ne!(key, TAB);
    for agent in super::super::upgrade::Agent::others() {
        assert!(agent.record_key(TAB).is_some(), "{agent:?}");
        assert_eq!(agent.record_key(&key), None, "{agent:?}");
    }
}

/// The pool's ledger and canary read a session as one word of at most `WORD_MAX` bytes: the
/// longest key comes back from both as it went in. An exec move's canary is its native receipt's
/// (`accounts_pool::finish_move` sets the owner), and reading a state judges such a canary's sid
/// before it takes the state: the key passes that judgment too.
#[test]
fn one_line_and_canary_of_keep_it_whole() {
    use super::super::accounts::{Canary, State, WORD_MAX};
    let key = longest_key();
    assert_eq!(super::super::one_line(&key, WORD_MAX), key);
    let now = 1_763_200_000;
    let canary = Canary {
        sid: key,
        at: now,
        took: false,
        native_receipt: Some("0123456789abcdef".repeat(2)),
    };
    let state = State {
        machine: "0a0b".into(),
        canary: Some(canary.clone()),
        ..State::default()
    };
    let read = State::from_json(&state.to_json(), now).unwrap();
    assert_eq!(read.canary, Some(canary));
}

#[test]
fn account_image_command_preserves_argv_without_shell_or_environment_fields() {
    let root = aterm_tempfile::tempdir().unwrap();
    let original = command(root.path());
    let encoded = original.encode().unwrap();
    assert_eq!(ExecCommand::parse(&encoded), Some(original));
    let aterm_json::Value::Object(mut object) = aterm_json::from_str(&encoded).unwrap() else {
        panic!("command object")
    };
    object.insert("env".into(), aterm_json::json!({"HOME":"/elsewhere"}));
    assert!(
        ExecCommand::parse(&aterm_json::to_string(&aterm_json::Value::Object(object)).unwrap())
            .is_none()
    );
}
