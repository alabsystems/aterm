// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::os::unix::fs::{PermissionsExt as _, symlink};

use super::*;

const NATIVE: &[u8] = b"approval_policy = 'never'\nsandbox_mode = 'danger-full-access'\n";

fn scratch(label: &str) -> (crate::testdir::Dir, PathBuf) {
    let scratch = crate::testdir::dir(label);
    let root = std::fs::canonicalize(&*scratch).unwrap();
    (scratch, root)
}

fn executable(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn package(root: &Path, alab: bool) -> PathBuf {
    let exe = root.join("bin/codex");
    executable(
        &exe,
        b"#!/bin/sh\nprintf 'target'; printf '<%s>' \"$@\"; printf '\\n'\n",
    );
    std::fs::write(
        root.join(crate::alab::PACKAGE_JSON),
        br#"{"layoutVersion":1,"version":"0.164.0","target":"aarch64-apple-darwin","entrypoint":"bin/codex"}"#,
    )
    .unwrap();
    if alab {
        std::fs::write(root.join(crate::alab::ALAB_BUILD_JSON), b"{}").unwrap();
    }
    exe
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn compiled_posture_requires_two_semantic_top_level_defaults() {
    assert!(native_posture(NATIVE));
    assert!(native_posture(
        b"# compiled defaults\nsandbox_mode=\"danger-full-access\"\napproval_policy='never'\n[features]\nplugins=false\n"
    ));
    for invalid in [
        &b"sandbox_mode='danger-full-access'\n"[..],
        &b"approval_policy='never'\n"[..],
        &b"approval_policy='on-request'\nsandbox_mode='danger-full-access'\n"[..],
        &b"approval_policy='never'\nsandbox_mode='workspace-write'\n"[..],
        &b"approval_policy=false\nsandbox_mode='danger-full-access'\n"[..],
        &b"[profile]\napproval_policy='never'\nsandbox_mode='danger-full-access'\n"[..],
        &b"approval_policy='never'\nsandbox_mode='danger-full-access'\napproval_policy='never'\n"[..],
        &b"approval_policy='never'\nsandbox_mode='danger-full-access'\ninvalid"[..],
        &b"\xff"[..],
    ] {
        assert!(!native_posture(invalid), "{invalid:?}");
    }
    let mut oversized = NATIVE.to_vec();
    oversized.resize(MAX_DEFAULTS_BYTES, b' ');
    assert!(!native_posture(&oversized));
}

#[test]
fn implicit_plain_and_profile_launches_delegate_without_cli_permission_overrides() {
    let (_scratch, root) = scratch("atpkg-native-profile-");
    let exe = package(&root, true);
    for words in [
        &[][..],
        &["--profile", "review"][..],
        &["--profile", "review", "-c", "model='chosen'", "a prompt"][..],
    ] {
        let typed = args(words);
        let (decision, target) = decide_with(
            "codex",
            &exe,
            &typed,
            Some(OsStr::new(" \tAtErM \n")),
            |physical| {
                assert_eq!(physical, std::fs::canonicalize(&exe).unwrap());
                Some(NATIVE.to_vec())
            },
        );
        assert_eq!(decision, crate::yolo::Launch::AsTyped, "{words:?}");
        assert_eq!(target, Some(std::fs::canonicalize(&exe).unwrap()));
    }
}

#[test]
fn explicit_permissions_and_yolo_never_run_a_defaults_probe() {
    for words in [
        &["--yolo", "--profile", "review"][..],
        &["-s", "read-only"][..],
        &["-a=on-request"][..],
        &["-c", "sandbox_mode='workspace-write'"][..],
        &["--config=approval_policy='on-request'"][..],
        &["--dangerously-bypass-approvals-and-sandbox"][..],
        &["--help"][..],
    ] {
        let typed = args(words);
        let (decision, target) = decide_with(
            "codex",
            Path::new("/not/a/real/package"),
            &typed,
            Some(OsStr::new("aterm")),
            |_| panic!("an explicit launch must not probe"),
        );
        assert_eq!(decision, crate::yolo::decide("codex", &typed));
        assert_eq!(target, None);
    }
}

#[test]
fn legacy_vendor_failed_probe_and_other_terminals_keep_the_existing_default() {
    let (_scratch, root) = scratch("atpkg-native-legacy-");
    let alab = package(&root.join("alab"), true);
    let vendor = package(&root.join("vendor"), false);
    let typed = args(&["--profile", "review"]);
    let legacy = crate::yolo::decide("codex", &typed);
    assert!(matches!(legacy, crate::yolo::Launch::Default(_)));
    for output in [
        None,
        Some(b"[features]\nplugins=false\n".to_vec()),
        Some(b"approval_policy='never'\n".to_vec()),
        Some(b"not TOML".to_vec()),
    ] {
        assert_eq!(
            decide_with("codex", &alab, &typed, Some(OsStr::new("aterm")), |_| {
                output
            }),
            (legacy.clone(), None),
        );
    }
    for (program, candidate, term) in [
        ("codex", vendor.as_path(), Some(OsStr::new("aterm"))),
        ("codex", alab.as_path(), Some(OsStr::new("iTerm.app"))),
        ("codex", alab.as_path(), None),
        ("claude", alab.as_path(), Some(OsStr::new("aterm"))),
    ] {
        assert_eq!(
            decide_with(program, candidate, &typed, term, |_| panic!(
                "must not probe"
            )),
            (crate::yolo::decide(program, &typed), None),
        );
    }
}

#[test]
fn retargeting_or_replacing_the_candidate_during_probe_keeps_legacy_posture() {
    let (_scratch, root) = scratch("atpkg-native-retarget-");
    let first = package(&root.join("first"), true);
    let second = package(&root.join("second"), true);
    let selected = root.join("selected");
    symlink(&first, &selected).unwrap();
    let typed = args(&["--profile", "review"]);
    let expected = (crate::yolo::decide("codex", &typed), None);
    let decision = decide_with(
        "codex",
        &selected,
        &typed,
        Some(OsStr::new("aterm")),
        |physical| {
            assert_eq!(physical, std::fs::canonicalize(&first).unwrap());
            std::fs::remove_file(&selected).unwrap();
            symlink(&second, &selected).unwrap();
            Some(NATIVE.to_vec())
        },
    );
    assert_eq!(decision, expected);
    assert_eq!(std::fs::canonicalize(&selected).unwrap(), second);
    let replacement = decide_with("codex", &first, &typed, Some(OsStr::new("aterm")), |_| {
        std::fs::rename(&first, first.with_extension("retained")).unwrap();
        executable(&first, b"replacement bytes");
        Some(NATIVE.to_vec())
    });
    assert_eq!(replacement, expected);
    assert_eq!(std::fs::read(&first).unwrap(), b"replacement bytes");
}

#[test]
fn a_retained_helper_cannot_acquire_a_verb_from_a_newer_producer() {
    let (_scratch, root) = scratch("atpkg-native-helper-");
    let current = root.join("current/aterm");
    let old = root.join("old/atpkg");
    executable(&current, b"current");
    executable(&old, b"older");
    let offered = root.join("atpkg");
    symlink(&current, &offered).unwrap();
    crate::stub::with_current_exe(&current, || {
        let launcher = Launcher::of_current_image(Some(&offered)).unwrap();
        assert!(launcher.pkg);
        assert_eq!(launcher.path, current);
        assert_eq!(Launcher::of_current_image(Some(&old)), None);
        assert_eq!(Launcher::of_current_image(None), None);
        std::fs::remove_file(&offered).unwrap();
        symlink(&old, &offered).unwrap();
        assert_eq!(Launcher::of_current_image(Some(&offered)), None);
        assert_eq!(
            launcher.path, current,
            "the recorded physical launcher cannot roll back with the alias"
        );
    });
    crate::stub::with_current_exe(&old, || {
        let launcher = Launcher::of_current_image(Some(&old)).unwrap();
        assert!(!launcher.pkg, "a standalone atpkg needs no pkg operand");
    });
}

fn run(shim: &Path, words: &[&str]) -> std::process::Output {
    let mut command = aterm_uds::spawnfd::command(shim);
    command.args(words).env_clear().env("TERM_PROGRAM", "aterm");
    let output =
        aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(output.status.success(), "{output:?}");
    output
}

#[test]
fn generated_shims_distinguish_standalone_launchers_from_retained_older_helpers() {
    let (_scratch, root) = scratch("atpkg-native-dispatch-");
    let current = root.join("current/aterm");
    let offered = root.join("other/atpkg");
    executable(&current, b"inert current image");
    executable(
        &offered,
        b"#!/bin/sh\nprintf 'bare'; printf '<%s>' \"$@\"; printf '\\n'\n",
    );
    let target = package(&root.join("package"), true);
    let shim = root.join("codex");
    let render = || {
        crate::platform::twin_prelude(
            "codex",
            &root,
            Some(&offered),
            &[],
            &crate::yolo::policy_of("codex"),
        )
    };
    let legacy = crate::stub::with_current_exe(&current, render);
    executable(
        &shim,
        crate::platform::sh_shim_content_twin(
            &target,
            &crate::shim_env::ShimEnv::NONE,
            None,
            &legacy,
        )
        .as_bytes(),
    );
    let legacy_run = run(&shim, &["--profile", "review"]);
    assert_eq!(
        String::from_utf8(legacy_run.stdout).unwrap(),
        "target<--dangerously-bypass-approvals-and-sandbox><--profile><review>\n"
    );
    let native = crate::stub::with_current_exe(&offered, render);
    executable(
        &shim,
        crate::platform::sh_shim_content_twin(
            &target,
            &crate::shim_env::ShimEnv::NONE,
            None,
            &native,
        )
        .as_bytes(),
    );
    let delegated = run(&shim, &["--profile", "review"]);
    assert_eq!(
        String::from_utf8(delegated.stdout).unwrap(),
        format!(
            "bare<{VERB}><codex><{}><--><--profile><review>\n",
            target.display(),
        )
    );
    assert!(delegated.stderr.is_empty());
}

#[test]
fn generated_shims_bind_the_runtime_candidate_and_keep_explicit_and_legacy_paths() {
    let (_scratch, root) = scratch("atpkg-native-shim-");
    let helper = root.join("app 'quoted'/aterm");
    executable(&helper, b"#!/bin/sh\nprintf 'helper:%s' \"${DISABLE_AUTOUPDATER-}\"; printf '<%s>' \"$@\"; printf '\\n'\n");
    let first = package(&root.join("first"), true);
    let second = package(&root.join("second"), true);
    let selected = root.join("exec/codex");
    std::fs::create_dir_all(selected.parent().unwrap()).unwrap();
    symlink(&first, &selected).unwrap();
    let shim = root.join("codex");
    let prelude = crate::stub::with_current_exe(&helper, || {
        crate::platform::twin_prelude(
            "codex",
            &root,
            Some(&helper),
            &[],
            &crate::yolo::policy_of("codex"),
        )
    });
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_owned()]).unwrap();
    let body = crate::platform::sh_shim_content_managed(&first, &env, &selected, &prelude);
    assert_eq!(
        crate::platform::parse_sh_shim_target(&body),
        Some(first.clone())
    );
    executable(&shim, body.as_bytes());
    std::fs::remove_file(&selected).unwrap();
    symlink(&second, &selected).unwrap();
    let delegated = run(&shim, &["--profile", "review", "a prompt"]);
    assert_eq!(
        String::from_utf8(delegated.stdout).unwrap(),
        format!(
            "helper:1<pkg><{VERB}><codex><{}><--><--profile><review><a prompt>\n",
            selected.display()
        ),
    );
    assert!(delegated.stderr.is_empty());
    for words in [
        &["--yolo", "a prompt"][..],
        &["-s", "read-only"][..],
        &["-c", "approval_policy='on-request'"][..],
    ] {
        let output = run(&shim, words);
        let expected = match crate::yolo::decide("codex", &args(words)) {
            crate::yolo::Launch::Flag(expanded) => expanded,
            crate::yolo::Launch::AsTyped => args(words),
            crate::yolo::Launch::Default(_) => panic!("explicit fixture"),
        };
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "target{}\n",
                expected
                    .iter()
                    .map(|arg| format!("<{arg}>"))
                    .collect::<String>()
            )
        );
    }
    std::fs::remove_file(&selected).unwrap();
    let fallback = run(&shim, &["--profile", "review"]);
    assert!(
        String::from_utf8(fallback.stdout)
            .unwrap()
            .contains(&format!(
                "<codex><{}><--><--profile><review>",
                first.display()
            ))
    );
    std::fs::remove_file(&helper).unwrap();
    let legacy = run(&shim, &["--profile", "review"]);
    assert_eq!(
        String::from_utf8(legacy.stdout).unwrap(),
        "target<--dangerously-bypass-approvals-and-sandbox><--profile><review>\n"
    );
    assert!(
        String::from_utf8(legacy.stderr)
            .unwrap()
            .starts_with(crate::yolo::DEFAULT_ECHO_PREFIX)
    );
}
