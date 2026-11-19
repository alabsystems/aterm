// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{while_lives, while_parent_alive};
use crate::spawnfd::{Child, Stdio};
use aterm_spec::wait::{MUST_HAPPEN, until_while_working, within};
use std::io::Write as _;
use std::time::Duration;

/// The gap between two looks at what must happen; the hang detector, not it, bounds the wait.
const LOOK: Duration = Duration::from_millis(10);

/// What the law started: the parent standing in for a test binary and, in the group that parent
/// leads, the stand-in it started. Dropped, it sends that group `SIGKILL` until the law saw the
/// group empty, and the parent `SIGKILL` until it is reaped, so a red run leaves neither.
struct Started {
    parent: Child,
    /// The parent's process group; `None` once the law saw it empty.
    group: Option<i32>,
}

impl Drop for Started {
    fn drop(&mut self) {
        if let Some(group) = self.group {
            // SAFETY: two integers. The group is the one this test started its parent at the head
            // of, and the law had not seen it empty: the parent unreaped, or the stand-in, held
            // its id at the law's last look.
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        let _ = self.parent.kill();
        let _ = self.parent.wait();
    }
}

/// Whether process group `group` has no member left, its last reaped (`ESRCH`; a group of a
/// zombie alone answers `EPERM` on Darwin).
fn empty(group: i32) -> bool {
    // SAFETY: two integers; signal 0 delivers nothing.
    let answered = unsafe { libc::kill(-group, 0) };
    answered != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// How the law's stand-in names its parent: by the pid written into its loop
/// ([`super::while_alive`] writes its own process's), or by the shell's `$PPID`
/// ([`while_parent_alive`]).
#[derive(Clone, Copy, Debug)]
enum Names {
    Pid,
    Parent,
}

/// THE LAW: a stand-in's loop [`while_lives`] wrote runs while the process it names lives, and
/// ends by itself once that process is gone, killed outright, with nothing signalling the
/// stand-in, whether the loop names it by its pid or, the stand-in its child, by `$PPID`
/// ([`while_parent_alive`]: the shell keeps the parent it started under, adopted or not). A
/// parent standing in for a test binary leads a process group of its own and starts the
/// stand-in, a busy loop, in that group; once the stand-in has looped twice (its check found the
/// parent each time), the parent alone is sent `SIGKILL` and reaped, and within the one-minute
/// hang detector the group must have no member left. NEGATIVE CONTROL: the loop it replaced,
/// `while :; do …; done`, never ends: the hang detector fails the law, and [`Started`] kills the
/// group.
#[test]
fn a_stand_in_loop_ends_once_the_process_it_names_is_gone() {
    for names in [Names::Pid, Names::Parent] {
        ends_once_its_parent_is_gone(names);
    }
}

/// [`a_stand_in_loop_ends_once_the_process_it_names_is_gone`] for a loop that `names` its parent.
fn ends_once_its_parent_is_gone(names: Names) {
    let dir = aterm_tempfile::tempdir().expect("a scratch directory");
    let looped = dir.path().join("looped");
    let parent = crate::spawnfd::command("/bin/sh")
        .args([
            "-c",
            "IFS= read -r s; /bin/sh -c \"$s\" & exec /bin/sleep 600",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("the parent");
    let group = i32::try_from(parent.id()).expect("a pid");
    let mut started = Started {
        parent,
        group: Some(group),
    };
    let body = format!(
        "n=$((n + 1)); [ \"$n\" = 2 ] && echo $$ > '{}'",
        looped.display()
    );
    let stand_in = match names {
        Names::Pid => while_lives(&started.parent.id().to_string(), &body),
        Names::Parent => while_parent_alive(&body),
    };
    let mut stdin = started.parent.stdin.take().expect("the parent's stdin");
    writeln!(stdin, "{stand_in}").expect("the stand-in's loop");
    drop(stdin);
    let stand_in = until_while_working(
        started.parent.id(),
        &format!("{names:?}: the stand-in looped twice while its parent lived"),
        || {
            std::fs::read_to_string(&looped)
                .ok()
                .filter(|pid| pid.ends_with('\n'))
        },
    );
    started.parent.kill().expect("SIGKILL to the parent alone");
    started.parent.wait().expect("the parent reaped");
    // A wall minute, not the stand-in's progress: the stand-in is busy, so a wait held open while
    // it works would wait out the negative control's loop to its 15-minute guard, where a
    // stand-in that runs at all in the minute reaches its next check, a builtin, at the end of
    // its body's current run.
    let emptied = within(MUST_HAPPEN, LOOK, || empty(group).then_some(()));
    if emptied.is_some() {
        started.group = None;
    }
    assert!(
        emptied.is_some(),
        "{names:?}: the stand-in (pid {}) ran on past the hang detector once its parent was gone",
        stand_in.trim()
    );
}
