// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test support: a stand-in's shell loop that ends by itself once its test binary is gone.
//!
//! A test's stand-in that leads a process group or a session of its own is out of reach of what
//! ends a test binary: a run that ends one (verify's hang kill, its stop at a red case, its halt
//! or interrupt) signals that binary's process group alone, and on Darwin a libtest binary's
//! spawns start untethered (libtest's `main` never calls [`crate::tether::entry`]). Some such
//! stand-ins end anyway: the leader of a session on a pty whose master closes with the binary is
//! sent the hangup's `SIGHUP`, and each member of a group orphaned with a stopped member is sent
//! `SIGHUP` and `SIGCONT`. Where neither reaches a stand-in, or it ignores or traps `SIGHUP`, a
//! loop written `while :; do …; done` runs on, adopted, until something kills it, a busy one at
//! a whole core. [`while_alive`] and [`while_parent_alive`] write the loop so that it ends at its
//! first check after the binary is gone, with nothing left to signal it.

/// `body`, a nonempty shell list, run over and over while this process (the test binary that
/// writes the loop) lives: `while kill -0 <this pid> 2>/dev/null; do body; done` ([`while_lives`]).
/// For a loop a shell is handed as an argument (`sh -c`) or a file nothing caches, never for
/// the text of a cached file stand-in: on macOS such a stand-in (`aterm_tempfile::stand_in`,
/// aterm-verify's `standin`) is a link to one cached file per distinct text, and macOS assesses
/// the first exec of each file, so a pid in the text lays a new file, and costs a new
/// assessment, on every run of the test binary. A cached stand-in the test binary execs itself
/// writes [`while_parent_alive`]'s loop instead, the same text every run.
#[must_use]
pub fn while_alive(body: &str) -> String {
    while_lives(&std::process::id().to_string(), body)
}

/// `body`, a nonempty shell list, run over and over while the shell's parent lives:
/// `while kill -0 $PPID 2>/dev/null; do body; done` ([`while_lives`]), the same text in every
/// run, for a file stand-in. Its loop names the test binary only where that binary execs the
/// file itself (a spawn through [`crate::spawnfd`] from the test, with no shell or helper
/// between): the shell sets `$PPID` as it starts and never changes it, so it still names the
/// binary once the binary is gone and the stand-in adopted.
#[must_use]
pub fn while_parent_alive(body: &str) -> String {
    while_lives("$PPID", body)
}

/// `body`, a nonempty shell list, run over and over while the process `pid` names lives: `pid` is
/// a shell word that expands to one pid (`123`, `$PPID`, `"$1"`), and the loop is
/// `while kill -0 pid 2>/dev/null; do body; done`. `kill` is a builtin of every `sh` (bash, dash,
/// zsh), so the check forks nothing and a body of `:` keeps the loop runnable for all of its
/// time. Once that process is gone and reaped (a zombie still answers `kill -0`), killed outright
/// included, the loop ends at its next check, once the body's current run ends. Another process
/// of this user that takes the pid keeps it running until that one ends too; one of another
/// user's answers `EPERM`, which ends it.
#[must_use]
pub fn while_lives(pid: &str, body: &str) -> String {
    format!("while kill -0 {pid} 2>/dev/null; do {body}; done")
}

#[cfg(test)]
#[path = "stand_in_loop_tests.rs"]
mod tests;
