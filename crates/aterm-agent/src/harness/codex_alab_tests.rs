// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::codex_rollout::{is_rollout_of, is_thread_id, thread_created_ms};

fn v(s: &str) -> Version {
    Version::parse(s).expect("a version")
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

const T1: &str = "01a0dc36-1dc1-7ee2-be91-11bc323e377c";
const T2: &str = "01a0dc3a-5c71-7143-bcee-58def0a15dfd";

const PACKAGE_0_157_1: &str = r#"{
  "layoutVersion": 1,
  "version": "0.157.1",
  "target": "aarch64-apple-darwin",
  "variant": "codex",
  "entrypoint": "bin/codex",
  "resourcesDir": "codex-resources",
  "pathDir": "codex-path"
}"#;

#[test]
fn the_measured_manifest_parses_and_a_changed_layout_refuses() {
    let (version, entry) = parse_package(PACKAGE_0_157_1).expect("the store's manifest");
    assert_eq!(version, v("0.157.1"));
    assert_eq!(entry, PathBuf::from("bin/codex"));
    for bad in [
        PACKAGE_0_157_1.replace("\"layoutVersion\": 1", "\"layoutVersion\": 2"),
        PACKAGE_0_157_1.replace("0.157.1", "0.157.1-alpha"),
        PACKAGE_0_157_1.replace("bin/codex", "/abs/codex"),
        PACKAGE_0_157_1.replace("bin/codex", "../codex"),
        "not json".to_string(),
    ] {
        assert_eq!(parse_package(&bad), None, "{bad}");
    }
}

#[test]
fn package_metadata_refuses_nonfiles_and_oversized_text() {
    let d = aterm_tempfile::tempdir().expect("private package fixture");
    let root = d.path();
    std::fs::create_dir(root.join("bin")).expect("bin");
    let exe = root.join("bin/codex");
    std::fs::write(&exe, "").expect("exe is never run");
    std::fs::write(root.join(ALAB_BUILD_JSON), "{}").expect("marker");
    let manifest = root.join(PACKAGE_JSON);
    let accepted = || {
        assert_eq!(package_version(&exe), Some(v("0.157.1")));
        assert_eq!(alab_build(&exe).as_deref(), Some("0.157.1"));
        assert!(alab_bytes(&exe));
    };
    let refused = || {
        assert_eq!(package_version(&exe), None);
        assert_eq!(alab_build(&exe), None);
        assert!(!alab_bytes(&exe));
    };
    let mut text = PACKAGE_0_157_1.to_string();
    text.extend(std::iter::repeat_n(
        ' ',
        MAX_PACKAGE_BYTES as usize - text.len(),
    ));
    std::fs::write(&manifest, &text).expect("valid manifest at the bound");
    accepted();
    text.push(' ');
    std::fs::write(&manifest, &text).expect("valid oversized JSON");
    refused();
    std::fs::write(&manifest, [0xff]).expect("invalid UTF-8");
    refused();
    std::fs::remove_file(&manifest).expect("remove owned file");
    std::fs::create_dir(&manifest).expect("manifest is a directory");
    refused();
    std::fs::remove_dir(&manifest).expect("remove owned directory");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let name = std::ffi::CString::new(manifest.as_os_str().as_bytes()).expect("FIFO path");
        // SAFETY: the NUL-terminated path is inside this fixture's exclusive directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        refused();
    }
}

#[test]
fn an_alab_package_with_a_pre_release_version_is_alab_bytes() {
    let d = crate::supervise::ScratchDir::new("codex", "alab-pre-release");
    let root = d.join("codex/0.164.0-dev.12365+g124c29509");
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    let exe = root.join("bin/codex");
    std::fs::write(&exe, "").expect("exe");
    let manifest = PACKAGE_0_157_1.replace("0.157.1", "0.164.0-dev.12365+g124c29509");
    std::fs::write(root.join(PACKAGE_JSON), &manifest).expect("manifest");
    assert_eq!(
        package_entrypoint(&manifest),
        Some(PathBuf::from("bin/codex"))
    );
    assert_eq!(parse_package(&manifest), None, "no order is guessed");
    assert!(!alab_bytes(&exe));
    assert_eq!(alab_build(&exe), None, "no marker, no ALab build");
    std::fs::write(root.join(ALAB_BUILD_JSON), "{}").expect("marker");
    assert!(
        alab_bytes(&exe),
        "the package and its marker say ALab built it"
    );
    assert_eq!(
        alab_build(&exe).as_deref(),
        Some("0.164.0-dev.12365+g124c29509")
    );
    assert_eq!(package_version(&exe), None);
    for bad in [
        manifest.replace("\"layoutVersion\": 1", "\"layoutVersion\": 2"),
        manifest.replace("bin/codex", "/abs/codex"),
        manifest.replace("bin/codex", "../codex"),
    ] {
        assert_eq!(package_entrypoint(&bad), None, "{bad}");
    }
}

/// codex-alab's own version line (codex-alab Decision 8, 2026-10-24): the manifest and the
/// marker's `version` are codex-alab's `0.1.0` line, the marker's `client_version` is what
/// upstream's code writes as the rollout's `cli_version`. The build is the client version.
#[test]
fn codex_alab_s_own_line_names_its_build_by_its_client_version() {
    const OWN: &str = "0.1.0-dev.12521+g3d3eb12d8";
    const CLIENT: &str = "0.164.0-dev.12521+g3d3eb12d8";
    let d = aterm_tempfile::tempdir().expect("private package fixture");
    let root = d.path().join(format!("codex/{OWN}"));
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    let exe = root.join("bin/codex");
    std::fs::write(&exe, "").expect("exe is never run");
    std::fs::write(
        root.join(PACKAGE_JSON),
        PACKAGE_0_157_1.replace("0.157.1", OWN),
    )
    .expect("manifest");
    let marker = root.join(ALAB_BUILD_JSON);
    let record = |text: &str| std::fs::write(&marker, text).expect("marker");
    record(&format!(
        r#"{{"source":"3d3eb12d8","version":"{OWN}","client_version":"{CLIENT}"}}"#
    ));
    assert!(alab_bytes(&exe));
    assert_eq!(
        alab_build(&exe).as_deref(),
        Some(CLIENT),
        "the rollouts name the client version, never codex-alab's own"
    );
    record(&format!(r#"{{"source":"3d3eb12d8","version":"{OWN}"}}"#));
    assert_eq!(
        alab_build(&exe).as_deref(),
        Some(OWN),
        "a package from before the line has one version"
    );
    for bad in [
        format!(r#"{{"version":"0.2.0-dev.1+g000000000","client_version":"{CLIENT}"}}"#),
        format!(r#"{{"version":"{OWN}","client_version":164}}"#),
        format!(r#"{{"version":"{OWN}","client_version":null}}"#),
        format!(r#"["{CLIENT}"]"#),
        "not json".to_string(),
    ] {
        record(&bad);
        assert_eq!(alab_build(&exe), None, "{bad}");
        assert!(alab_bytes(&exe), "still ALab bytes, but no build: {bad}");
    }
}

#[test]
fn a_version_is_read_from_the_package_that_names_the_binary_never_by_running_it() {
    let d = crate::supervise::ScratchDir::new("codex", "pkg");
    let root = d.join("store/codex/1000000000157000001");
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    let exe = root.join("bin/codex");
    std::fs::write(&exe, "#!/bin/sh\nsleep 600\n").expect("exe");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    std::fs::write(root.join(PACKAGE_JSON), PACKAGE_0_157_1).expect("manifest");
    let started = std::time::Instant::now();
    assert_eq!(package_version(&exe), Some(v("0.157.1")));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
    let other = d.join("elsewhere/bin");
    std::fs::create_dir_all(&other).expect("other");
    std::fs::write(other.join("codex"), "x").expect("exe");
    std::fs::write(
        d.join("elsewhere").join(PACKAGE_JSON),
        PACKAGE_0_157_1.replace("bin/codex", "bin/not-codex"),
    )
    .expect("manifest");
    assert_eq!(package_version(&other.join("codex")), None);
    assert_eq!(Version::parse("codex-cli"), None);
}

#[test]
fn the_rewrite_resumes_the_thread_with_every_kept_flag() {
    let a = argv(&[
        "/store/codex/bin/codex",
        "--no-daemon",
        "-m",
        "gpt-6-astra",
        "-c",
        "model_reasoning_effort=\"high\"",
        "--config=sandbox_mode=\"read-only\"",
        "-mgpt-x",
        "--add-dir",
        "-",
        "--model=gpt-5.4-mini",
        "fix the bug",
    ]);
    assert_eq!(
        rewrite_argv(&a, Some(T1)).expect("carried"),
        argv(&[
            "resume",
            "--no-daemon",
            "-m",
            "gpt-6-astra",
            "-c",
            "model_reasoning_effort=\"high\"",
            "--config=sandbox_mode=\"read-only\"",
            "-mgpt-x",
            "--add-dir",
            "-",
            "--model=gpt-5.4-mini",
            T1,
        ])
    );
    for a in [
        argv(&["codex", "resume", T2, "--no-daemon"]),
        argv(&["codex", "resume", "--last", "--no-daemon"]),
        argv(&["codex", "--no-daemon", "fork", T2, "carry on"]),
        argv(&[
            "codex",
            "--no-daemon",
            "-i",
            "a.png",
            "b.png",
            "look at these",
        ]),
    ] {
        assert_eq!(
            rewrite_argv(&a, Some(T1)).expect("carried"),
            argv(&["resume", "--no-daemon", T1]),
            "{a:?}"
        );
    }
    assert_eq!(
        rewrite_argv(&argv(&["codex", "-s", "read-only", "hi"]), None).expect("carried"),
        argv(&["-s", "read-only"])
    );
    assert_eq!(
        rewrite_argv(&argv(&["codex", "--search", "--", "--no-daemon"]), Some(T1))
            .expect("carried"),
        argv(&["resume", "--search", T1])
    );
}

#[test]
fn an_unknown_or_unresumable_launch_is_refused_not_guessed() {
    for (a, why) in [
        (
            argv(&["codex", "--full-auto"]),
            ArgvRefusal::UnknownFlag("--full-auto".into()),
        ),
        (
            argv(&["codex", "-x"]),
            ArgvRefusal::UnknownFlag("-x".into()),
        ),
        (
            argv(&["codex", "--no-daemon=1"]),
            ArgvRefusal::UnknownFlag("--no-daemon=1".into()),
        ),
        (
            argv(&["codex", "exec", "hi"]),
            ArgvRefusal::NotResumable("exec".into()),
        ),
        (
            argv(&["codex", "--remote", "ws://h:1"]),
            ArgvRefusal::NotResumable("--remote".into()),
        ),
        (
            argv(&["codex", "--worktree"]),
            ArgvRefusal::NotResumable("--worktree".into()),
        ),
        (
            argv(&["codex", "app-server", "--listen", "unix://"]),
            ArgvRefusal::NotResumable("app-server".into()),
        ),
    ] {
        assert_eq!(rewrite_argv(&a, Some(T1)), Err(why), "{a:?}");
    }
}

#[test]
fn only_an_interactive_tui_is_a_tui_never_the_daemon_or_its_updater() {
    for tui in [
        argv(&["/store/bin/codex"]),
        argv(&["codex", "--no-daemon", "-m", "x"]),
        argv(&["codex", "resume", T1]),
        argv(&["codex", "-c", "exec=1", "fork"]),
        argv(&["codex", "fix the bug"]),
    ] {
        assert!(is_tui_argv(&tui), "{tui:?}");
    }
    for not in [
        argv(&[
            "/c/packages/app-server-daemon/releases/0.157.0-aarch64-apple-darwin/bin/codex",
            "app-server",
            "--listen",
            "unix://",
            "--managed-daemon",
        ]),
        argv(&["codex", "app-server", "daemon", "pid-update-loop"]),
        argv(&["codex", "exec", "hi"]),
        argv(&["codex", "-m", "x", "agents"]),
    ] {
        assert!(!is_tui_argv(&not), "{not:?}");
    }
}

#[test]
fn a_thread_id_is_a_hyphenated_uuid_and_nothing_else() {
    assert!(is_thread_id(T1));
    for bad in [
        "",
        "01a0dc36",
        "01a0dc36-1dc1-7ee2-be91-11bc323e377",
        "01a0dc36-1dc1-7ee2-be91-11bc323e377cX",
        "01a0dc36-1dc1-7ee2-be91-11bc323e377g",
        "01a0dc36 1dc1-7ee2-be91-11bc323e377c",
        "; rm -rf /",
    ] {
        assert!(!is_thread_id(bad), "{bad:?}");
    }
    assert_eq!(lock_thread(&format!("{T1}.lock")), Some(T1));
    assert_eq!(lock_thread(".coordination.lock"), None);
    assert!(is_rollout_of(
        &format!("rollout-2026-09-25T22-35-29-{T1}.jsonl"),
        T1
    ));
    assert!(!is_rollout_of(
        &format!("rollout-2026-09-25T22-35-29-{T1}.jsonl.zst"),
        T1
    ));
    assert!(!is_rollout_of(
        &format!("rollout-2026-09-25T22-35-29-{T2}.jsonl"),
        T1
    ));
}

#[test]
fn the_relaunch_on_exit_reads_a_thread_and_its_instant() {
    let argv = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let t = "01a0e4bd-2b37-75a0-939c-43fb6f1a5dfb";
    assert_eq!(
        resumed_thread(&argv(&["codex", "--no-alt-screen", "resume", t])),
        Some(t.to_string())
    );
    assert_eq!(
        resumed_thread(&argv(&["codex", "-m", "o4", "resume", "--no-daemon", t])),
        Some(t.to_string())
    );
    for a in [
        &["codex"][..],
        &["codex", "resume", "--last"][..],
        &["codex", "resume"][..],
        &["codex", "fork", t][..],
        &["codex", "say hello"][..],
    ] {
        assert_eq!(resumed_thread(&argv(a)), None, "{a:?}");
    }
    assert_eq!(thread_created_ms(t), Some(1_790_543_997_751));
    assert_eq!(
        thread_created_ms("01a0e4bd-2b37-45a0-939c-43fb6f1a5dfb"),
        None,
        "v4"
    );
    assert_eq!(thread_created_ms("not-a-thread"), None);
}
