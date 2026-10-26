// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK-FREE WORKER'S FENCE (`docs/DESIGN-disk-free-ask-2026-10-24.md` §8.4): aterm starts a
//! worker of its own in [`WORKER_FOLDER`] under the harness state directory, the supervisor knows
//! that session by its task record's trusted folder ([`is_worker_folder`]), and the worker's
//! approval rule (`RULE_DISK_WORKER` in `supervise::policy::approval`) passes, besides a command
//! proven read-only, only `aterm harness disk` ([`runs_harness_disk`]); every other box of the
//! worker's is declined.

use std::path::{Component, Path};

use crate::harness::cli::HARNESS;

/// The folder a disk-free worker starts in, under the harness state directory
/// (`<aterm state>/harness/`[`HARNESS`]`/disk-worker`), which its task record names as its
/// trusted folder.
pub const WORKER_FOLDER: &str = "disk-worker";

/// The program name [`runs_harness_disk`] reads as aterm's.
pub const ATERM: &str = "aterm";

/// Whether `p`, a session's trusted folder (`meta trusted_cwd=`), is a disk-free worker's: its
/// last component is [`WORKER_FOLDER`] and its parent's is the harness state directory's,
/// [`HARNESS`].
///
/// The two names are read, not the whole path. The trusted folder reaches the supervisor as the
/// task record spells it (a `meta` reply, or a tape's facts), which another state root spells
/// otherwise (a scratch home, a lab instance, `ATERM_STATE_HOME`), as does a firmlink
/// (`/private/var` for `/var`); the pair of names is a folder only aterm's worker launch makes.
#[must_use]
pub fn is_worker_folder(p: &Path) -> bool {
    let mut names = p.components().rev();
    let named =
        |c: Option<Component<'_>>, name: &str| matches!(c, Some(Component::Normal(n)) if n == name);
    named(names.next(), WORKER_FOLDER) && named(names.next(), HARNESS)
}

/// Whether `words`, one simple command's words as the shell splits them, run `aterm harness
/// disk` with any verb, flags and operands after it: the first word names aterm ([`ATERM`]),
/// bare as the worker's `PATH` finds it or by an absolute path to an executable of that name
/// (`/…/aterm`, `~/…/aterm`), and the next two are `harness` and `disk`.
///
/// A word before the program (an assignment, a redirect, a wrapper such as `env`, `command` or
/// `nice`) makes no such command, nor does a program word the shell may read otherwise than
/// written: one holding a quote, an escape, an expansion, a glob or a `..` component.
#[must_use]
pub fn runs_harness_disk<S: AsRef<str>>(words: &[S]) -> bool {
    let [program, harness, disk, ..] = words else {
        return false;
    };
    names_aterm(program.as_ref()) && harness.as_ref() == "harness" && disk.as_ref() == "disk"
}

/// Whether `words`, one simple command's words, run `aterm ctl` on this machine's System State
/// (`aterm ctl [@<selector>] sys …`, every view of which reads), as the disk-free worker's task
/// says to find what fills the volume: the program word as [`runs_harness_disk`] reads it, then
/// `ctl`, at most one `@` selector, then `sys`.
#[must_use]
pub fn runs_ctl_sys<S: AsRef<str>>(words: &[S]) -> bool {
    let [program, ctl, rest @ ..] = words else {
        return false;
    };
    let rest = match rest {
        [selector, rest @ ..] if selector.as_ref().starts_with('@') => rest,
        rest => rest,
    };
    names_aterm(program.as_ref())
        && ctl.as_ref() == "ctl"
        && rest.first().is_some_and(|verb| verb.as_ref() == "sys")
}

/// Whether `word`, a command's program word, is [`ATERM`] bare or an absolute path whose last
/// component is it, in plain characters only ([`runs_harness_disk`]).
fn names_aterm(word: &str) -> bool {
    if word == ATERM {
        return true;
    }
    let Some(rest) = word.strip_prefix("~/").or_else(|| word.strip_prefix('/')) else {
        return false;
    };
    let plain = rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | '+'));
    plain && !rest.split('/').any(|c| c == "..") && rest.rsplit('/').next() == Some(ATERM)
}

#[cfg(test)]
#[path = "disk_fence_tests.rs"]
mod tests;
