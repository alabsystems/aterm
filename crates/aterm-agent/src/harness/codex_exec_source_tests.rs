// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn facts(root: &Path) -> (Process, ProcArgs) {
    let image = Image::at(&std::env::current_exe().unwrap()).unwrap();
    let process = Process {
        pid: 123,
        birth: 456,
        parent: 10,
        group: 123,
        session: 10,
        image,
    };
    let args = ProcArgs {
        exec_path: String::from_utf8(process.image.path.clone()).unwrap(),
        argv: vec![
            "original-codex-name".into(),
            "exec".into(),
            "--json".into(),
            "-".into(),
            "line one\nline two".into(),
        ],
        env: vec![
            format!("HOME={}", root.display()),
            format!("CODEX_AUTH_PATH={}/source/auth.json", root.display()),
            "UNRELATED_SECRET=must-not-be-retained".into(),
        ],
    };
    (process, args)
}

/// A session on this machine's own login carries no pointer, as a TUI's does: a pointer to it
/// would name no row, so its row would never hand off. A managed login's carries its own.
#[test]
fn a_session_on_the_default_login_carries_no_pointer() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (process, args) = facts(root.path());
    let mut source = ExecSource::from_observations(process, args, root.path().into()).unwrap();
    let default_home = root.path().join(".codex");
    source.auth_path = default_home.join("auth.json");
    assert_eq!(source.account_env(&default_home), Some(Vec::new()));
    source.auth_path = root.path().join("source/auth.json");
    let carried = source.account_env(&default_home).unwrap();
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].0, "CODEX_AUTH_PATH");
}

#[test]
fn native_exec_source_retains_the_whole_invocation_and_only_auth_paths() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (process, args) = facts(root.path());
    let expected = args.argv.clone();
    let source = ExecSource::from_observations(process, args, root.path().into()).unwrap();
    let target = root.path().join("other/auth.json");
    let command = source.command_for(&target).unwrap();
    assert_eq!(command.argv0, expected[0]);
    assert_eq!(command.argv, expected[1..]);
    assert_eq!(command.auth_path, target.to_str().unwrap());
    assert_eq!(source.auth_path, root.path().join("source/auth.json"));
    assert_eq!(source.config_home, root.path().join(".codex"));
    let debug = format!("{source:?}");
    assert!(!debug.contains("line one"));
    assert!(!debug.contains("UNRELATED_SECRET"));
}

#[test]
fn native_exec_source_keeps_managed_login_attribution_with_a_shared_config_home() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (process, mut args) = facts(root.path());
    let config = root.path().join("shared-config");
    args.env.push(format!("CODEX_HOME={}", config.display()));
    let source = ExecSource::from_observations(process, args, root.path().into()).unwrap();
    let login = crate::harness::accounts::ProcessLogin::of_env_for(
        crate::harness::accounts::Cli::Codex,
        source.account_env(&root.path().join(".codex")).unwrap(),
    );
    assert!(login.default_home && login.route == crate::harness::accounts::AuthRoute::Stored);
    assert_eq!(login.login.as_deref(), root.path().join("source").to_str());
    assert_eq!(source.config_home, config);
    assert_eq!(source.auth_path, root.path().join("source/auth.json"));
}

#[test]
fn native_exec_source_rejects_other_modes_changed_image_and_ambiguous_environment() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (process, args) = facts(root.path());
    for mode in ["resume", "review", "login"] {
        let mut changed = args.clone();
        changed.argv[1] = mode.into();
        assert!(
            ExecSource::from_observations(process.clone(), changed, root.path().into()).is_none()
        );
    }
    let mut changed = process.clone();
    changed.image.inode += 1;
    assert!(ExecSource::from_observations(changed, args.clone(), root.path().into()).is_none());
    for key in ["HOME", "CODEX_HOME", "CODEX_AUTH_PATH"] {
        let mut changed = args.clone();
        changed.env.push(format!("{key}=/first"));
        changed.env.push(format!("{key}=/second"));
        assert!(
            ExecSource::from_observations(process.clone(), changed, root.path().into()).is_none()
        );
    }
    let mut changed = args;
    changed.env.retain(|v| !v.starts_with("CODEX_AUTH_PATH="));
    changed
        .env
        .push("CODEX_AUTH_PATH=relative/auth.json".into());
    assert!(ExecSource::from_observations(process, changed, root.path().into()).is_none());
}
