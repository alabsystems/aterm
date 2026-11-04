// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn invocation() -> Invocation {
    Invocation {
        socket: "/private/native-account.sock".into(),
        session: "s-account".into(),
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
