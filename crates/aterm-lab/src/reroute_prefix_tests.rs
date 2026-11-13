// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::fs::PermissionsExt as _;

use super::*;

/// An owner's prefix under `root` holding one store build of `claude`, a
/// stand-in that prints its version and arguments.
fn owner_store(root: &Path) -> PathBuf {
    owner_store_of(
        root,
        "#!/bin/sh\n[ \"$1\" = --version ] && { echo '2.1.304 (Claude Code)'; exit 0; }\necho \"ran $0 updater=$DISABLE_AUTOUPDATER args=$*\"\n",
    )
}

/// [`owner_store`] with the stand-in `body`.
fn owner_store_of(root: &Path, body: &str) -> PathBuf {
    let build = 1_000_002_000_001_000_304_u64;
    let prefix = root.join("owner/Library/Application Support/aterm/pkg");
    let dir = prefix.join(format!("store/claude/{build}"));
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let exe = dir.join("bin/claude");
    std::fs::write(&exe, body).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let side = |ext: &str, text: &str| {
        std::fs::write(prefix.join(format!("store/claude/{build}.{ext}")), text).unwrap();
    };
    side("ready", "ok\nplatform=aarch64-macos\ncontents=bin:claude\n");
    side("shim-env", "DISABLE_AUTOUPDATER=1\n");
    side(
        "vendor",
        "schema = 1\nprogram = \"claude\"\nversion = \"2.1.304\"\n",
    );
    exe
}

/// Inside an aterm session (`ATERM_SESSION_ID`) the stub execs the twin, which execs the
/// clone with its shim environment; the owner's store is untouched.
#[test]
fn the_stub_runs_the_twin_which_runs_the_clone() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let source = owner_store(&root);
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    let hold = root.join("held/reroute");
    let laid = lay(&home, &source, Some(&hold)).expect("laid");
    assert_eq!(laid.version, "2.1.304 (Claude Code)");
    assert!(
        !hold.exists(),
        "the clone printed its version: its hold went"
    );
    assert_eq!(laid.program, "claude");
    assert_eq!(
        laid.exe,
        laid.prefix
            .join("store/claude/1000002000001000304/bin/claude")
    );
    assert_eq!(
        std::fs::read(&laid.exe).unwrap(),
        std::fs::read(&source).unwrap()
    );
    assert_eq!(
        std::fs::read_link(laid.prefix.join("store/claude/current")).unwrap(),
        laid.build_dir
    );
    assert!(
        laid.prefix
            .join("store/claude/1000002000001000304.vendor")
            .is_file()
    );
    assert!(atpkg::reroute::is_reroute_stub(&laid.stub));
    let run = |marker: Option<&str>| {
        let mut cmd = aterm_uds::spawnfd::command(&laid.stub);
        cmd.env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null());
        if let Some(id) = marker {
            cmd.env("ATERM_SESSION_ID", id);
        }
        let out = cmd.output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let ran = run(Some("1"));
    assert!(
        ran.starts_with(&format!("ran {} updater=1 args=", laid.exe.display())),
        "{ran}"
    );
    assert_eq!(run(None), "", "outside aterm the stub runs no twin");
    let owner = source.parent().unwrap().parent().unwrap().parent().unwrap();
    let names: Vec<_> = std::fs::read_dir(owner)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 4, "the owner's store is only read: {names:?}");
}

/// The owner's prefix is never laid, nor a scratch prefix twice, nor a
/// path that is no store build's.
#[test]
fn the_owners_prefix_and_a_laid_one_are_refused() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let source = owner_store(&root);
    let hold = root.join("held/reroute");
    let refused = lay(&root.join("owner"), &source, Some(&hold)).unwrap_err();
    assert!(refused.contains("never re-laid"), "{refused}");
    assert!(!hold.exists(), "nothing was started, so nothing is held");
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    lay(&home, &source, Some(&hold)).unwrap();
    let again = lay(&home, &source, Some(&root.join("held/again"))).unwrap_err();
    assert!(again.contains("already holds a store"), "{again}");
    assert!(!root.join("held/again").exists(), "{again}");
    let loose = root.join("claude");
    std::fs::copy(&source, &loose).unwrap();
    let other = root.join("other");
    std::fs::create_dir(&other).unwrap();
    assert!(
        lay(&other, &loose, Some(&hold))
            .unwrap_err()
            .contains("no store build")
    );
}

/// A clone killed or silent at its `--version` gave no proof it ran, so its stamped hold
/// stays; negative control: one that printed only an error ran, so `lay` fails and the hold
/// goes.
#[test]
fn a_clone_killed_before_it_printed_keeps_its_hold() {
    for (what, body, kept) in [
        ("killed, silent", "#!/bin/sh\nkill -9 $$\n", true),
        ("ended silently", "#!/bin/sh\nexit 3\n", true),
        (
            "an error, then killed",
            "#!/bin/sh\necho no >&2\nkill -9 $$\n",
            false,
        ),
    ] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let source = owner_store_of(&root, body);
        let home = root.join("home");
        std::fs::create_dir(&home).unwrap();
        let hold = root.join("held/reroute");
        let failed = lay(&home, &source, Some(&hold)).unwrap_err();
        assert_eq!(
            aterm_uds::deadline::exec_stamp(&hold).is_some(),
            kept,
            "{what}: {failed}"
        );
        let clone =
            atpkg::store::default_prefix(&home).join("store/claude/1000002000001000304/bin/claude");
        assert_eq!(
            std::fs::read(&clone).unwrap(),
            std::fs::read(&source).unwrap()
        );
    }
}

#[test]
fn a_store_executable_names_its_prefix_program_and_build() {
    let p = Path::new("/p/pkg/store/claude/1000002000001000304/bin/claude");
    assert_eq!(
        store_of(p),
        Some((
            PathBuf::from("/p/pkg"),
            "claude".into(),
            1_000_002_000_001_000_304
        ))
    );
    assert_eq!(
        store_of(Path::new("/p/pkg/store/claude/current/bin/claude")),
        None
    );
    assert_eq!(
        store_of(Path::new("/p/pkg/store/claude/7/libexec/claude")),
        None
    );
}
