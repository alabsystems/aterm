// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

struct Agents(Option<PathBuf>);

impl Drop for Agents {
    fn drop(&mut self) {
        super::super::upgrade_drive::AGENTS_DIR.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

fn image_at(prefix: &Path, build: &str) -> PathBuf {
    let path = prefix.join("store/claude").join(build).join("bin/claude");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // Format data only: this test never starts a process or claims a kernel outcome.
    let head: &[u8] = if cfg!(target_os = "macos") {
        b"\xcf\xfa\xed\xfe\0\0\0\0"
    } else {
        b"\x7fELF\0\0\0\0"
    };
    std::fs::write(&path, head).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn factory(prefix: &Path, target: &Path) -> PathBuf {
    let twin = prefix.join("agents/claude");
    std::fs::create_dir_all(twin.parent().unwrap()).unwrap();
    let mut prelude = atpkg::platform::twin_prelude(
        "claude",
        prefix,
        None,
        atpkg::selfupdate::verbs_of("claude"),
        &atpkg::yolo::policy_of("claude"),
    );
    prelude.push_str(&atpkg::launch_tap::sh_prelude("claude", None));
    let env = atpkg::shim_env::ShimEnv::admit(&[
        "DISABLE_AUTOUPDATER=1".into(),
        "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1".into(),
        "NATIVE_EXPORT=managed".into(),
        "OMIT_NATIVE=managed".into(),
    ])
    .unwrap();
    atpkg::platform::install_twin_to_env(&twin, target, &env, &prelude).unwrap();
    twin
}

/// A launch that names its model, as the factory tests run it.
const NAMED: &[&str] = &["claude", "--model", "claude-test", "--verbose"];

fn select(exe: &Path, argv: &[&str], native: bool) -> Result<SelectedCommand, NoPlan> {
    let argv: Vec<String> = argv.iter().map(|word| (*word).to_string()).collect();
    line_tap_selected(
        Dialect::Zsh,
        None,
        None,
        exe,
        &argv,
        Some("same-conversation"),
        Some("plan"),
        &[("NATIVE_EXPORT", "caller")],
        Some(&["OMIT_NATIVE"]),
        Some(&["OMIT_NATIVE"]),
        Some("{\"statusLine\":{}}"),
        native,
        None,
    )
}

#[test]
fn native_forward_freezes_the_factory_build_and_policy_while_rollback_stays_physical() {
    let world = aterm_tempfile::tempdir().unwrap();
    let prefix = std::fs::canonicalize(world.path()).unwrap();
    let a = image_at(&prefix, "1000002000001000318");
    let b = image_at(&prefix, "1000002000001000319");
    let c = image_at(&prefix, "1000002000001000320");
    let twin = factory(&prefix, &b);
    let previous = super::super::upgrade_drive::AGENTS_DIR
        .with(|slot| slot.borrow_mut().replace(prefix.join("agents")));
    let _agents = Agents(previous);
    let selected = select(&twin, NAMED, true).unwrap_or_else(|_| panic!("native factory plan"));
    assert_eq!(selected.exe, b);
    assert!(
        selected
            .argv
            .windows(2)
            .any(|pair| pair == ["--resume", "same-conversation"])
    );
    assert!(
        selected
            .argv
            .windows(2)
            .any(|pair| pair == ["--model", "claude-test"])
    );
    assert!(
        selected
            .argv
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "plan"])
    );
    assert!(
        selected
            .argv
            .windows(2)
            .any(|pair| pair == ["--settings", "{\"statusLine\":{}}"])
    );
    assert!(
        !selected
            .argv
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions")
    );
    assert!(
        selected
            .env
            .iter()
            .any(|(name, value)| name == "NATIVE_EXPORT" && value == "caller")
    );
    assert!(
        selected
            .env
            .iter()
            .any(|(name, value)| name == "DISABLE_AUTOUPDATER" && value == "1")
    );
    assert_eq!(
        selected
            .env
            .iter()
            .filter(|(name, _)| name == SURVEY_SWITCH_VAR)
            .count(),
        1
    );
    assert!(!selected.env.iter().any(|(name, _)| name == "OMIT_NATIVE"));
    let planned = PlannedCommand {
        dialect: Dialect::Zsh,
        heal: None,
        cwd: None,
        target: selected.exe,
        argv: selected.argv,
        env: selected.env,
        unset: Some(vec!["OMIT_NATIVE".into()]),
        account_intent: None,
    };
    let captured = planned.capture_native().unwrap();
    assert_eq!(
        captured.image,
        super::super::native_act::Image::at(&b).unwrap()
    );
    assert_eq!(
        captured.legacy_line().as_deref(),
        Some(selected.line.as_str())
    );
    factory(&prefix, &c);
    assert_eq!(captured.program, b.to_str().unwrap());
    assert_eq!(
        select(&twin, NAMED, true)
            .unwrap_or_else(|_| panic!("new native plan"))
            .exe,
        c
    );
    assert_eq!(
        select(&a, NAMED, false)
            .unwrap_or_else(|_| panic!("physical rollback"))
            .exe,
        a
    );
    let alias = prefix.join("mutable-alias");
    std::os::unix::fs::symlink(&b, &alias).unwrap();
    let mut aliased = planned.clone();
    aliased.target = alias.clone();
    assert!(aliased.capture_native().is_none());
    assert_eq!(
        select(&alias, NAMED, true)
            .unwrap_or_else(|_| panic!("physical alias selection"))
            .exe,
        b
    );
    std::fs::rename(&b, prefix.join("old-format-data")).unwrap();
    std::fs::write(&b, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        planned.capture_native().is_none(),
        "a new script cannot become a native command"
    );
}

#[test]
fn unknown_configured_and_off_twin_scripts_have_no_native_plan() {
    let world = aterm_tempfile::tempdir().unwrap();
    let prefix = std::fs::canonicalize(world.path()).unwrap();
    let b = image_at(&prefix, "1000002000001000319");
    let twin = factory(&prefix, &b);
    let previous = super::super::upgrade_drive::AGENTS_DIR
        .with(|slot| slot.borrow_mut().replace(prefix.join("agents")));
    let _agents = Agents(previous);
    let mut body = std::fs::read_to_string(&twin).unwrap();
    body.insert_str("#!/bin/sh\n".len(), "echo unknown-policy >&2\n");
    std::fs::write(&twin, body).unwrap();
    assert!(select(&twin, NAMED, true).is_err());
    let outside = prefix.join("opaque-claude");
    std::fs::write(
        &outside,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", b.display()),
    )
    .unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(select(&outside, NAMED, true).is_err());
    assert_eq!(
        select(&b, NAMED, true)
            .unwrap_or_else(|_| panic!("direct native image"))
            .exe,
        b
    );
}

/// THE COORDINATED UPGRADE'S SELECTION KEEPS THE LAUNCH'S `--model` AND ADDS NONE: native or
/// not, a launch without `--model` is selected without one, and one with `--model claude-x` keeps
/// exactly that pair.
#[test]
fn an_upgrade_selection_keeps_the_launchs_model_flag_and_adds_none() {
    let world = aterm_tempfile::tempdir().unwrap();
    let prefix = std::fs::canonicalize(world.path()).unwrap();
    let b = image_at(&prefix, "1000002000001000319");
    let twin = factory(&prefix, &b);
    let previous = super::super::upgrade_drive::AGENTS_DIR
        .with(|slot| slot.borrow_mut().replace(prefix.join("agents")));
    let _agents = Agents(previous);
    let flags = |argv: &[String]| -> Vec<usize> {
        argv.iter()
            .enumerate()
            .filter(|(_, word)| *word == "--model" || word.starts_with("--model="))
            .map(|(at, _)| at)
            .collect()
    };
    for native in [true, false] {
        let bare = select(&twin, &["claude", "--verbose"], native)
            .unwrap_or_else(|_| panic!("a selection without a model, native {native}"));
        assert!(
            flags(&bare.argv).is_empty(),
            "native {native}: {:?}",
            bare.argv
        );
        let named = select(
            &twin,
            &["claude", "--model", "claude-x", "--verbose"],
            native,
        )
        .unwrap_or_else(|_| panic!("a selection with a model, native {native}"));
        let at = flags(&named.argv);
        assert_eq!(at.len(), 1, "native {native}: {:?}", named.argv);
        assert_eq!(named.argv[at[0]], "--model", "native {native}");
        assert_eq!(
            named.argv.get(at[0] + 1).map(String::as_str),
            Some("claude-x"),
            "native {native}"
        );
    }
}
