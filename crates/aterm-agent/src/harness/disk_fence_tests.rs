// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the disk-free worker's fence: the worker's folder by its two names, and the one
//! command its approval rule passes besides a read.

use std::path::Path;

use super::*;

fn words(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

/// The worker's folder is read by its last two names, wherever the state root lies. CONTROL:
/// a folder of the same last name under another parent, the harness state directory itself, a
/// folder under the worker's, and one that climbs out of it are no worker's.
#[test]
fn a_worker_folder_is_named_by_the_harness_state_and_its_own_name() {
    for yes in [
        "/Users//a/Library/Application Support/aterm/harness/claude-harness/disk-worker",
        "/private/tmp/scratch/state/harness/claude-harness/disk-worker/",
        "claude-harness/disk-worker",
    ] {
        assert!(is_worker_folder(Path::new(yes)), "{yes}");
    }
    for no in [
        "/Users//a/Developer/disk-worker",
        "/Users//a/state/harness/claude-harness",
        "/Users//a/state/harness/claude-harness/disk-worker/sub",
        "/Users//a/state/harness/claude-harness/disk-worker/..",
        "/Users//a/state/harness/codex-harness/disk-worker",
        "disk-worker",
        "/",
        "",
    ] {
        assert!(!is_worker_folder(Path::new(no)), "{no}");
    }
}

/// `aterm harness disk` and every verb after it is the worker's command, by the bare name or an
/// absolute path to an `aterm`. CONTROL: another harness verb, another program named by a path
/// that ends in `aterm`'s letters, a relative path, a program word the shell may expand, and a
/// word before the program are not.
#[test]
fn aterm_harness_disk_by_name_or_absolute_path_is_the_workers_command() {
    for yes in [
        "aterm harness disk",
        "aterm harness disk why",
        "aterm harness disk free /Users//a/.cargo/registry/cache --dry-run",
        "/Applications/aterm.app/Contents/MacOS/aterm harness disk free ~/.npm/_cacache",
        "~/.local/bin/aterm harness disk --json",
    ] {
        assert!(runs_harness_disk(&words(yes)), "{yes}");
    }
    for no in [
        "aterm harness",
        "aterm harness diskfree",
        "aterm harness accounts",
        "aterm ctl @self status",
        "aterm disk harness",
        "/usr/local/bin/xaterm harness disk",
        "./aterm harness disk",
        "target/debug/aterm harness disk",
        "/tmp/../tmp/aterm harness disk",
        "$HOME/.local/bin/aterm harness disk",
        "/opt/*/aterm harness disk",
        "\"\" harness disk",
        "\\aterm harness disk",
        "env aterm harness disk",
        "A=1 aterm harness disk",
        "2>/dev/null aterm harness disk",
        "",
    ] {
        assert!(!runs_harness_disk(&words(no)), "{no}");
    }
}

/// `aterm ctl [@<selector>] sys …` is the worker's System State read. CONTROL: another ctl verb,
/// a `sys` after another word, two selectors, and a program word the shell may expand are not.
#[test]
fn a_system_state_read_is_the_workers_ctl_command() {
    for yes in [
        "aterm ctl sys",
        "aterm ctl @self sys disk scope=aterm",
        "aterm ctl @self sys procs scope=global top=16",
        "/Applications/aterm.app/Contents/MacOS/aterm ctl @s-1 sys why",
    ] {
        assert!(runs_ctl_sys(&words(yes)), "{yes}");
    }
    for no in [
        "aterm ctl @self send sys",
        "aterm ctl @self @other sys",
        "aterm ctl @self status",
        "aterm ctl",
        "aterm sys",
        "$HOME/aterm ctl sys",
        "env aterm ctl sys",
        "",
    ] {
        assert!(!runs_ctl_sys(&words(no)), "{no}");
    }
}
