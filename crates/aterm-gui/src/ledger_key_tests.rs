// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_key_runs_drive_ledger_for_the_session_as_html_into_a_private_file() {
    let out = Path::new("/tmp/aterm-ledger-s-1e91-1.html");
    assert_eq!(
        ledger_argv("s-1e918c4662a1b7b8bd43", out),
        [
            "drive",
            "ledger",
            "@s-1e918c4662a1b7b8bd43",
            "--format",
            "html",
            "--out",
            "/tmp/aterm-ledger-s-1e91-1.html",
        ]
    );
    let p = ledger_out_path("s-ab/../cd", 7);
    let name = p
        .file_name()
        .expect("a file")
        .to_string_lossy()
        .into_owned();
    assert_eq!(name, "aterm-ledger-s-abcd-7.html", "the sid is sanitized");
    assert!(p.starts_with(std::env::temp_dir()));
    let plan = plan_for("s-1");
    assert_eq!(plan.sid, "s-1");
    assert_eq!(plan.argv[2], "@s-1");
    assert_eq!(plan.argv[6], plan.out.to_string_lossy());
    assert_eq!(
        with_socket(&plan.argv, Some("/run/aterm/aterm-7.sock"))[..4],
        ["drive", "--socket", "/run/aterm/aterm-7.sock", "ledger"]
    );
    assert_eq!(with_socket(&plan.argv, None), plan.argv);
    assert!(!aterm_cli().to_string_lossy().contains("aterm-gui"));
}

#[test]
fn a_window_alias_finds_the_front_door_beside_it() {
    let dir = Path::new(if cfg!(windows) {
        r"C:\Program Files\aterm"
    } else {
        "/Applications/aterm.app/Contents/MacOS"
    });
    let suffix = std::env::consts::EXE_SUFFIX;
    let door = dir.join(format!("aterm{suffix}"));
    let alias = dir.join(format!("aterm-gui{suffix}"));
    assert_eq!(front_door_beside(&alias, |_| true), Some(door.clone()));
    assert_eq!(
        front_door_beside(&alias, |path| path == door),
        Some(door.clone())
    );
    assert_eq!(front_door_beside(&alias, |_| false), None);
    assert_eq!(front_door_beside(&door, |_| true), Some(door.clone()));
}
