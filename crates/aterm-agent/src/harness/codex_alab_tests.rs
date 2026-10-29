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
        assert_eq!(product_version(&exe).as_deref(), Some("0.157.1"));
        assert!(alab_bytes(&exe));
    };
    let refused = || {
        assert_eq!(package_version(&exe), None);
        assert_eq!(alab_build(&exe), None);
        assert_eq!(product_version(&exe), None);
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
fn product_and_client_versions_keep_their_distinct_cores_and_source_metadata() {
    for (product, client) in [
        (
            "0.1.0-dev.13764+codex.0.164.0.g4b3a367a0",
            "0.165.0-dev.13764+g4b3a367a0",
        ),
        (
            "0.1.0-dev.13765+codex.0.164.0.g5c4b3a210",
            "0.165.0-dev.13765+g5c4b3a210",
        ),
        (
            "999999999.999999999.999999999-dev.999999999+codex.999999999.999999999.999999999.gfffffffff",
            "999999999.999999999.999999999-dev.999999999+gfffffffff",
        ),
    ] {
        let d = aterm_tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir(root.join("bin")).unwrap();
        let exe = root.join("bin/codex");
        std::fs::write(&exe, b"never executed").unwrap();
        let manifest = PACKAGE_0_157_1.replace("0.157.1", product);
        std::fs::write(root.join(PACKAGE_JSON), &manifest).unwrap();
        std::fs::write(
            root.join(ALAB_BUILD_JSON),
            format!(r#"{{"version":"{product}","client_version":"{client}"}}"#),
        )
        .unwrap();
        assert_eq!(product_version(&exe).as_deref(), Some(product));
        assert_eq!(alab_build(&exe).as_deref(), Some(client));
        assert_eq!(package_version(&exe), None);

        std::fs::write(
            root.join(PACKAGE_JSON),
            manifest.replace("bin/codex", "bin/other"),
        )
        .unwrap();
        assert_eq!(product_version(&exe), None);
        assert_eq!(alab_build(&exe), None);
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
            "resume",
            T1,
        ])
    );
    for a in [
        argv(&["codex", "resume", T2, "--no-daemon"]),
        argv(&["codex", "resume", "--last", "--no-daemon"]),
    ] {
        assert_eq!(
            rewrite_argv(&a, Some(T1)).expect("carried"),
            argv(&["resume", "--no-daemon", T1]),
            "{a:?}"
        );
    }
    for a in [
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
            argv(&["--no-daemon", "resume", T1]),
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
        argv(&["--search", "resume", T1])
    );
}

#[test]
fn the_resume_rewrite_preserves_each_options_command_level() {
    let root = argv(&["codex", "-c", "a=1", "--search", "fix the build"]);
    assert_eq!(
        rewrite_argv(&root, Some(T1)).unwrap(),
        argv(&["-c", "a=1", "--search", "resume", T1]),
        "root flags stay before the resume subcommand"
    );
    let child = argv(&[
        "codex", "-c", "a=1", "fork", "--last", "-c", "a=2", "--search",
    ]);
    assert_eq!(
        rewrite_argv(&child, Some(T1)).unwrap(),
        argv(&["-c", "a=1", "resume", "-c", "a=2", "--search", T1]),
        "a fork's selectors go while its child flags keep their scope"
    );
    assert_eq!(
        rewrite_argv(&child, None).unwrap(),
        argv(&["-c", "a=1", "-c", "a=2", "--search"]),
        "the flags-only worker API retains its original order"
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
            argv(&["codex", "migrate-nfs-db"]),
            ArgvRefusal::NotResumable("migrate-nfs-db".into()),
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
        (
            argv(&["codex", "tcp-tunnel"]),
            ArgvRefusal::NotResumable("tcp-tunnel".into()),
        ),
        (
            argv(&["codex", "execpolicy", "check"]),
            ArgvRefusal::NotResumable("execpolicy".into()),
        ),
        (
            argv(&["codex", "cloud-tasks"]),
            ArgvRefusal::NotResumable("cloud-tasks".into()),
        ),
        (
            argv(&["codex", "responses-api-proxy"]),
            ArgvRefusal::NotResumable("responses-api-proxy".into()),
        ),
        (
            argv(&["codex", "stdio-to-uds", "/tmp/codex.sock"]),
            ArgvRefusal::NotResumable("stdio-to-uds".into()),
        ),
        (
            argv(&["/tmp/.codex/apply_patch", "*** Begin Patch"]),
            ArgvRefusal::NotResumable("apply_patch".into()),
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
        argv(&["codex", "-p", "work"]),
        argv(&["codex", "-p", "exec"]),
        argv(&["codex", "resume", T1]),
        argv(&["codex", "-c", "exec=1", "fork"]),
        argv(&["codex", "--image", "a.png", "exec"]),
        argv(&["codex", "-i", "a.png", "b.png", "migrate-nfs-db"]),
        argv(&["codex", "fix the bug"]),
        argv(&["codex", "--search", "--", "--help"]),
    ] {
        assert!(is_tui_argv(&tui), "{tui:?}");
        let snap = crate::harness::relaunch::Snapshot {
            tab: "s-1".into(),
            pid: 4242,
            start: "Mon Oct 27 00:00:00 2026".into(),
            shell: 4241,
            program: "/store/bin/codex".into(),
            argv: tui.clone(),
            session: Some(T1.into()),
            cwd: "/work".into(),
            version: Some("0.165.0".into()),
            run: crate::harness::relaunch::AgentRun::Codex(crate::harness::relaunch::CodexRun {
                home: "/Users//me/.codex".into(),
                home_env: None,
                self_claimed: None,
            }),
            dialect: None,
            claude_dir: None,
            login: None,
            home: None,
            stored_login: false,
            profile: None,
        };
        assert_eq!(
            crate::harness::relaunch::launch_ended(
                &snap,
                &crate::harness::relaunch::ExitRecord::Crashed
            ),
            None,
            "{tui:?}"
        );
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
        argv(&["codex", "--image=a.png", "exec", "hi"]),
        argv(&["codex", "-ia.png", "exec", "hi"]),
        argv(&["codex", "-m", "x", "migrate-nfs-db"]),
        argv(&["codex", "-m", "x", "agents"]),
        argv(&["codex", "tcp-tunnel"]),
        argv(&["codex", "execpolicy", "check"]),
        argv(&["codex", "cloud-tasks"]),
        argv(&["codex", "responses-api-proxy"]),
        argv(&["codex", "stdio-to-uds", "/tmp/codex.sock"]),
        argv(&["codex", "-h"]),
        argv(&["codex", "--help"]),
        argv(&["codex", "-V"]),
        argv(&["codex", "--version"]),
        argv(&["codex", "resume", "--help"]),
        argv(&["codex", "fork", "-h"]),
        argv(&["/tmp/.codex/apply_patch", "*** Begin Patch"]),
        argv(&["codex", "--codex-run-as-process-setup"]),
        argv(&["codex", "--codex-run-as-apply-patch", "*** Begin Patch"]),
        argv(&["codex", "--codex-run-as-arg0-exec-helper"]),
        argv(&["codex", "--codex-run-as-fs-helper"]),
        argv(&["codex", "-hfoo"]),
    ] {
        assert!(!is_tui_argv(&not), "{not:?}");
        let snap = crate::harness::relaunch::Snapshot {
            tab: "s-1".into(),
            pid: 4242,
            start: "Mon Oct 27 00:00:00 2026".into(),
            shell: 4241,
            program: "/store/bin/codex".into(),
            argv: not.clone(),
            session: Some(T1.into()),
            cwd: "/work".into(),
            version: Some("0.165.0".into()),
            run: crate::harness::relaunch::AgentRun::Codex(crate::harness::relaunch::CodexRun {
                home: "/Users//me/.codex".into(),
                home_env: None,
                self_claimed: None,
            }),
            dialect: None,
            claude_dir: None,
            login: None,
            home: None,
            stored_login: false,
            profile: None,
        };
        assert_eq!(
            crate::harness::relaunch::launch_ended(
                &snap,
                &crate::harness::relaunch::ExitRecord::Crashed
            ),
            Some("one-shot"),
            "{not:?}"
        );
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
    assert_eq!(
        resumed_thread(&argv(&["codex", "--image=a.png", "resume", t])),
        Some(t.to_string()),
        "an attached image takes no following value"
    );
    assert_eq!(
        resumed_thread(&argv(&["codex", "resume", "--", t])),
        Some(t.to_string()),
        "a thread id after `--` in `codex resume` is still the resumed thread"
    );
    for a in [
        &["codex"][..],
        &["codex", "resume", "--last"][..],
        &["codex", "resume"][..],
        &["codex", "resume", t, "--help"][..],
        &["/tmp/.codex/apply_patch", "resume", t][..],
        &["codex", "fork", t][..],
        &["codex", "--image", "a.png", "b.png", "resume", t][..],
        &["codex", "resume", "--image", "a.png", "b.png", t][..],
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

/// A POOL ROW OF ITS OWN REACHES CODEX ONLY AS `CODEX_AUTH_PATH`, SO ONLY A BUILD THAT KEEPS
/// ITS LOGIN ITS OWN MAY RUN ONE: codex-alab whose bytes hold [`AUTH_PATH_MARK`], itself or
/// behind atpkg's launcher. A vendor package (no marker), a codex-alab that names the variable
/// but predates the mark, a bare word, a relative path, a launcher that names no binary and a
/// missing file are each refused, each for its own reason.
#[test]
fn only_a_codex_alab_that_holds_the_auth_path_mark_may_run_a_row_of_its_own() {
    use AuthPathUnread::{Missing, Older, Unnamed, Vendor};
    let d = aterm_tempfile::tempdir().expect("private package fixture");
    let root = d.path().canonicalize().expect("canonical");
    let reads = lay_package(&root.join("store/codex/new"), true);
    let before = lay_package(&root.join("store/codex/old"), false);
    let vendor = lay_package(&root.join("store/codex-old/vendor"), true);
    std::fs::remove_file(root.join("store/codex-old/vendor").join(ALAB_BUILD_JSON))
        .expect("an OpenAI package carries no marker");
    let launcher = |name: &str, body: &str| {
        let path = root.join(name);
        std::fs::write(&path, body).expect("launcher");
        path
    };
    let to = |exe: &Path| format!("#!/bin/sh\n# atpkg shim\nexec '{}' \"$@\"\n", exe.display());
    assert_eq!(auth_path_unread(&reads), None);
    assert_eq!(auth_path_unread(&launcher("twin-new", &to(&reads))), None);
    assert_eq!(auth_path_unread(&before), Some(Older(before.clone())));
    assert_eq!(
        auth_path_unread(&launcher("twin-old", &to(&before))),
        Some(Older(before.clone()))
    );
    assert_eq!(auth_path_unread(&vendor), Some(Vendor(vendor.clone())));
    assert_eq!(
        auth_path_unread(&launcher("twin-vendor", &to(&vendor))),
        Some(Vendor(vendor.clone()))
    );
    for word in ["codex", "bin/codex"] {
        assert_eq!(
            auth_path_unread(Path::new(word)),
            Some(Unnamed(PathBuf::from(word)))
        );
    }
    let reroute = launcher(
        "reroute",
        "#!/bin/sh\n# atpkg reroute stub v1\nexec \"$__aterm_agents/$__aterm_name\" \"$@\"\n",
    );
    assert_eq!(auth_path_unread(&reroute), Some(Unnamed(reroute.clone())));
    let non_utf8_script = root.join("non-utf8-script");
    std::fs::write(
        &non_utf8_script,
        [
            b"#!/bin/sh\n# \xff\nexec '".as_slice(),
            reads.as_os_str().as_encoded_bytes(),
            b"' \"$@\"\n",
        ]
        .concat(),
    )
    .expect("non-utf8 script");
    assert_eq!(
        auth_path_unread(&non_utf8_script),
        Some(Unnamed(non_utf8_script))
    );
    let oversized_script = launcher(
        "oversized-script",
        &format!(
            "#!/bin/sh\n#{}\nexec '{}' \"$@\"\n",
            "x".repeat(LAUNCHER_MAX as usize),
            reads.display()
        ),
    );
    assert_eq!(
        auth_path_unread(&oversized_script),
        Some(Unnamed(oversized_script))
    );
    let absent = root.join("absent/bin/codex");
    assert_eq!(auth_path_unread(&absent), Some(Missing(absent.clone())));
    std::fs::write(&before, [b"\xcf\xfa ", AUTH_PATH_MARK.as_bytes()].concat())
        .expect("the build replaced in place");
    assert_eq!(
        auth_path_unread(&before),
        None,
        "a build rewritten in place is read again"
    );
}

/// The scan finds a mark that straddles two reads and misses one the file only nearly holds.
#[test]
fn a_mark_across_a_read_boundary_is_found_and_a_near_miss_is_not() {
    let d = aterm_tempfile::tempdir().expect("scan fixture");
    let needle = AUTH_PATH_MARK.as_bytes();
    let file = d.path().join("blob");
    let mut bytes = vec![b'C'; SCAN_CHUNK - 4];
    bytes.extend_from_slice(needle);
    std::fs::write(&file, &bytes).expect("straddling blob");
    assert!(file_holds(&file, needle).expect("read"));
    let mut near = vec![b'C'; SCAN_CHUNK - 4];
    near.extend_from_slice(&needle[..needle.len() - 1]);
    std::fs::write(&file, &near).expect("near-miss blob");
    assert!(!file_holds(&file, needle).expect("read"));
    std::fs::write(&file, b"").expect("empty");
    assert!(!file_holds(&file, needle).expect("read"));
    assert!(
        file_holds(d.path(), needle).is_err(),
        "a directory is no file"
    );
}

/// UNDER THE OWNER'S PIN ON A ROW OF ITS OWN, THE ROW ON THIS MACHINE'S OWN LOGIN SERVES ONLY ONCE
/// THAT PIN ENDS: walled, the refusal names its wall's end, the pin's end ([`UNPIN_FIX`]) and
/// its own pin, made once that wall ends; signed out, its sign-in and then its pin. CONTROL: with
/// no pin (the pool's pick, or the review's serving row), a walled row serves by itself once its
/// wall ends and is offered no pin (the next usage read would undo one) and no pin's end, and a
/// signed-out row only its sign-in.
#[test]
fn under_a_pin_the_default_row_serves_only_once_that_pin_ends() {
    let why = AuthPathUnread::Older(PathBuf::from("/opt/codex-alab/bin/codex"));
    let until = 1_800_003_750;
    let walled = DefaultRow::Walled {
        label: "main".into(),
        window: "five_hour".into(),
        reset: Some(1_800_003_600),
        until,
    };
    let signed_out = DefaultRow::SignedOut {
        label: "main".into(),
        stored: true,
    };
    let pin = unread_fix(Some("main"));
    let ends = crate::harness::usage::rfc3339_utc(until);
    let words = only_default_pinned(&why, "work", &walled);
    assert!(
        words.contains(&pin) && words.contains(&ends) && words.contains(UNPIN_FIX),
        "{words}"
    );
    let words = only_default_pinned(&why, "work", &signed_out);
    assert!(
        words.contains(&pin) && words.contains(DEFAULT_SIGN_IN),
        "{words}"
    );
    for row in [None, Some("work")] {
        let words = only_default(&why, row, &walled);
        assert!(
            words.contains(&ends) && !words.contains(&pin) && !words.contains(UNPIN_FIX),
            "CONTROL {row:?}: {words}"
        );
        let words = only_default(&why, row, &signed_out);
        assert!(
            words.contains(DEFAULT_SIGN_IN) && !words.contains(&pin),
            "CONTROL {row:?}: {words}"
        );
    }
}

/// A REFUSAL ON A CODEX THAT KEEPS NO ROW'S LOGIN ITS OWN STARTS NO CLAUSE AFTER "; " WITH A
/// CAPITAL: for every condition of the row on this machine's own login, the pool's pick, the
/// review's row, the owner's pin and a starter's row, rotation off or not, each clause joined
/// after "; " (the way back [`unread_advice`] gives among them) starts lower-case.
#[test]
fn no_clause_of_a_refusal_starts_with_a_capital() {
    let why = AuthPathUnread::Older(PathBuf::from("/opt/codex-alab/bin/codex"));
    let defaults = [
        DefaultRow::Unlisted,
        DefaultRow::Moved("main".into()),
        DefaultRow::Serves("main".into()),
        DefaultRow::Walled {
            label: "main".into(),
            window: "five_hour".into(),
            reset: Some(1_800_003_600),
            until: 1_800_003_750,
        },
        DefaultRow::SignedOut {
            label: "main".into(),
            stored: true,
        },
        DefaultRow::SignedOut {
            label: "main".into(),
            stored: false,
        },
        DefaultRow::Other {
            label: "main".into(),
            why: "held for another Mac's warm set".into(),
        },
    ];
    for default in &defaults {
        let words = [
            only_default(&why, None, default),
            only_default(&why, Some("work"), default),
            only_default_pinned(&why, "work", default),
            only_default_carried(&why, "work", default, true),
            only_default_carried(&why, "work", default, false),
        ];
        for said in words {
            for clause in said.split("; ").skip(1) {
                assert!(
                    !clause.starts_with(char::is_uppercase),
                    "{default:?}: `{clause}` in {said}"
                );
            }
        }
    }
}
