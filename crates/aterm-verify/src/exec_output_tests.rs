// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Scope;
use aterm_uds::spawnfd::{Command, command};

/// A command that reports its own pid and process group, then sleeps as long as
/// `OWNED_OUTPUT_SLEEP` says, so a direct spawn's group can be read while it lives.
fn reporter() -> Command {
    let mut command = command("/bin/sh");
    command.args([
        "-c",
        "echo \"$$ $(ps -o pgid= -p $$ | tr -d ' ')\"; sleep \"$OWNED_OUTPUT_SLEEP\"",
    ]);
    command
}

fn reported(output: &std::process::Output) -> (i32, i32) {
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields = text.split_whitespace();
    let pid: i32 = fields
        .next()
        .and_then(|field| field.parse().ok())
        .unwrap_or_else(|| panic!("the reporter named no pid: {text:?}"));
    let pgid: i32 = fields
        .next()
        .and_then(|field| field.parse().ok())
        .unwrap_or_else(|| panic!("the reporter named no pgid: {text:?}"));
    (pid, pgid)
}

fn direct_spawn_group(command: &mut Command) -> i32 {
    command.env("OWNED_OUTPUT_SLEEP", "30");
    let mut child = command.spawn().expect("the reused command spawns");
    // SAFETY: `getpgid` takes an integer and reads the process table; it signals nothing.
    let pgid = unsafe { libc::getpgid(i32::try_from(child.id()).expect("pid fits i32")) };
    child.kill().expect("kill the reused child");
    child.wait().expect("wait for the reused child");
    pgid
}

#[test]
fn owned_output_leaves_a_reused_commands_process_group_as_configured() {
    // SAFETY: `getpgid(0)` reads this process's group; it signals nothing.
    let ours = unsafe { libc::getpgid(0) };
    assert!(ours > 1, "this process has a real group: {ours}");

    // A caller-configured group: the owned spawn still leads its own group, and a
    // later direct spawn of the same command lands in the configured one again.
    let mut configured = reporter();
    configured
        .env("OWNED_OUTPUT_SLEEP", "0")
        .process_group(ours);
    let output = Scope::default()
        .output(&mut configured)
        .expect("owned output");
    let (pid, pgid) = reported(&output);
    assert_eq!(pgid, pid, "the owned child led a group of its own");
    assert_eq!(
        direct_spawn_group(&mut configured),
        ours,
        "the reused command kept its configured process group"
    );

    // No group configured: a later direct spawn stays in this process's group.
    let mut plain = reporter();
    plain.env("OWNED_OUTPUT_SLEEP", "0");
    let output = Scope::default().output(&mut plain).expect("owned output");
    let (pid, pgid) = reported(&output);
    assert_eq!(pgid, pid, "the owned child led a group of its own");
    assert_eq!(
        direct_spawn_group(&mut plain),
        ours,
        "the reused command gained no process group of its own"
    );
}
