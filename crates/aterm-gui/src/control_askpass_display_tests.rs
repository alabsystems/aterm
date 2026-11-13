// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{command_line, display_line};
use std::io::BufRead;

/// `/bin/ps -ww -o args= -p <pid>` in `locale`, the reader [`command_line`]
/// replaced, as its oracle, escaped as the sheet escapes.
fn ps_says(pid: u32, locale: &str) -> String {
    let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("ps"))
        .env("LC_ALL", locale)
        .args(["-ww", "-o", "args=", "-p"])
        .arg(pid.to_string())
        .output()
        .expect("run ps");
    crate::askpass::escape_display(String::from_utf8_lossy(&out.stdout).trim())
}

/// The sheet's command lines are what `ps` printed: the invoker's read in
/// process, sudo's from the argv the classification read, for a process
/// whose words hold a space, a newline, a tab and a non-ASCII letter.
/// NEGATIVE CONTROL: `ps` in the C locale escapes the letter, so the
/// comparison can fail; a pid no process holds shows nothing.
#[test]
fn the_command_lines_are_what_ps_printed() {
    let mut child = aterm_uds::spawnfd::command(aterm_uds::os_path!("sh"))
        .args([
            "-c",
            "echo up; read x",
            "sh",
            "a b",
            "new\nline",
            "t\tab",
            "café",
        ])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn sh");
    // Its argv is the shell's once it says so: read before that, it is ours.
    let mut up = String::new();
    std::io::BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut up)
        .expect("the child's line");
    assert_eq!(up, "up\n");
    let pid = child.id();
    let oracle = ps_says(pid, "en_US.UTF-8");
    assert!(
        oracle.contains("café") && oracle.contains("\\012"),
        "{oracle}"
    );
    assert_eq!(command_line(i32::try_from(pid).unwrap()), oracle);
    let argv = atpkg::caller_shell::process_args(pid)
        .expect("its argv")
        .argv;
    assert_eq!(display_line(&argv), oracle);
    assert_ne!(ps_says(pid, "C"), oracle);
    assert_eq!(command_line(i32::MAX), "");
    assert_eq!(command_line(0), "");
    drop(child.stdin.take());
    let _ = child.wait();
}
