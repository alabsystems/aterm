// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How the git a command runs keeps a read from fetching an object a partial clone lacks
//! ([`NoLazyFetch`], agh's one answer), asked through the System port as that command's account.

use super::git;
use crate::model::User;
use crate::port::{Cmd, Ran, Scratch, System};
use agh::git::lazy_fetch as shared;
use std::path::Path;

pub(crate) use shared::NoLazyFetch;

/// The label of the scratch folder each question makes, as the account its git runs as.
const LABEL: &str = "git-lazy-fetch";

/// How `user`'s git ([`git`]) keeps a read from fetching lazily ([`NoLazyFetch::of`]).
///
/// # Errors
/// git could not be asked ([`NoLazyFetch::of`]).
pub(crate) fn of(sys: &dyn System, user: &User) -> Result<Option<NoLazyFetch>, String> {
    of_git(sys, git(sys, user, None))
}

/// [`of`] for the git `git` names: a command with no subcommand yet.
///
/// # Errors
/// As [`of`].
pub(crate) fn of_git(sys: &dyn System, git: Cmd) -> Result<Option<NoLazyFetch>, String> {
    NoLazyFetch::of(&Asked { sys, git })
}

/// [`git`] for `user` in `path`, with `--no-lazy-fetch` where git takes it
/// ([`NoLazyFetch::Flag`]), and with the variable alone where it does not.
pub(crate) fn guarded(off: NoLazyFetch, sys: &dyn System, user: &User, path: Option<&Path>) -> Cmd {
    let command = git(sys, user, path);
    match off.option() {
        Some(option) => command.arg(option),
        None => command,
    }
}

/// The git `git` names, asked through `sys`: each run as the command's account, the scratch
/// folder and each trace file that account's.
struct Asked<'a> {
    sys: &'a dyn System,
    git: Cmd,
}

impl Asked<'_> {
    /// The account the git runs as, where it is another than this process's.
    fn other_user(&self) -> Option<&User> {
        self.git
            .user
            .as_ref()
            .filter(|user| user.uid != self.sys.euid())
    }
}

impl From<Ran> for shared::Ran {
    fn from(ran: Ran) -> Self {
        Self {
            status: ran.status,
            stdout: ran.stdout,
            stderr: ran.stderr,
        }
    }
}

impl shared::Git for Asked<'_> {
    fn run(&self, args: &[&std::ffi::OsStr], env: &[(&str, &str)]) -> Result<shared::Ran, String> {
        let mut command = self.git.clone().args(args);
        for (key, value) in env {
            command = command.env(*key, *value);
        }
        self.sys
            .run_complete_stdout(&command)
            .map(shared::Ran::from)
            .map_err(|why| why.to_string())
    }

    fn scratch(&self) -> Result<Box<dyn AsRef<Path> + '_>, String> {
        let other_user = self.other_user();
        // A privileged process's TMPDIR can itself be private to root. The shared temporary
        // root permits traversal to the private allocation transferred to the Git actor.
        let scratch = if cfg!(unix) && other_user.is_some() {
            Scratch::new_in(self.sys, Path::new("/tmp"), LABEL)
        } else {
            Scratch::new(self.sys, LABEL)
        }
        .map_err(|error| format!("cannot make a scratch directory ({error})"))?;
        if let Some(user) = other_user {
            self.sys
                .chown(scratch.path(), user.uid, user.gid)
                .map_err(|error| format!("cannot own scratch directory ({error})"))?;
        }
        Ok(Box::new(scratch))
    }

    fn trace_file(&self, path: &Path) -> Result<(), String> {
        self.sys
            .write_file(path, b"", 0o600)
            .map_err(|error| format!("cannot prepare trace ({error})"))?;
        if let Some(user) = self.other_user() {
            self.sys
                .chown(path, user.uid, user.gid)
                .map_err(|error| format!("cannot own trace ({error})"))?;
        }
        Ok(())
    }

    fn read_trace(&self, path: &Path, limit: usize) -> Result<Vec<u8>, String> {
        let uid = self
            .git
            .user
            .as_ref()
            .map_or(self.sys.euid(), |user| user.uid);
        self.sys
            .read_owned_file(path, uid, limit, true)
            .map_err(|error| format!("cannot read trace ({error})"))
    }
}

/// The object [`before_the_option`]'s `hash-object` names.
#[cfg(any(test, debug_assertions))]
const OBJECT_NAMED: &str = "4444444444444444444444444444444444444444";

/// How a git before 2.45 answers [`of`] on a test double: `--no-lazy-fetch` refused as unknown
/// (129); in the question's scratch folder ([`LABEL`]), its repositories made and its object
/// hashed; and its read failed quietly with no fetch attempt, where it `obeys` the variable, or
/// the object fetched from the promisor and named where it does not. `None` for every other
/// command.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn before_the_option(sys: &crate::port::Fake, command: &Cmd, obeys: bool) -> Option<Ran> {
    let has = |word: &str| command.args.iter().any(|arg| arg == word);
    if has("--no-lazy-fetch") {
        return Some(Ran::exit(129, "unknown option: --no-lazy-fetch"));
    }
    let scratch = format!(".aterm-machines-{LABEL}-");
    if !command
        .args
        .iter()
        .any(|arg| arg.to_string_lossy().contains(&scratch))
    {
        return None;
    }
    if has("init") {
        Some(Ran::ok(""))
    } else if has("hash-object") {
        Some(Ran::ok(&format!("{OBJECT_NAMED}\n")))
    } else if has("cat-file") {
        let disabled = command
            .env
            .iter()
            .rev()
            .find(|(key, _)| key == "GIT_NO_LAZY_FETCH")
            .is_some_and(|(_, value)| value == "1");
        let attempted = !obeys || !disabled;
        let ran = if attempted {
            Ran::ok("blob\n")
        } else {
            Ran::exit(128, "fatal: git cat-file: could not get object info\n")
        };
        record_trace(sys, command, &ran, attempted);
        Some(ran)
    } else {
        None
    }
}

/// The fake Git's trace, including the same query and terminal status as its answer.
#[cfg(any(test, debug_assertions))]
fn record_trace(sys: &crate::port::Fake, command: &Cmd, ran: &Ran, attempted: bool) {
    let Some((_, path)) = command
        .env
        .iter()
        .rev()
        .find(|(key, _)| key == "GIT_TRACE2_EVENT")
    else {
        return;
    };
    let argv: Vec<_> = std::iter::once(command.program.as_os_str())
        .chain(command.args.iter().map(std::ffi::OsString::as_os_str))
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let mut events = vec![
        aterm_json::json!({"event":"version", "sid":"probe", "evt":"4"}),
        aterm_json::json!({"event":"start", "sid":"probe", "argv":argv}),
        aterm_json::json!({"event":"cmd_name", "sid":"probe", "name":"cat-file"}),
    ];
    if attempted {
        events.push(aterm_json::json!({"event":"child_start", "sid":"probe"}));
    }
    for event in ["exit", "atexit"] {
        events.push(aterm_json::json!({"event":event, "sid":"probe", "code":ran.status}));
    }
    let bytes = format!(
        "{}\n",
        events
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
    sys.file(path, bytes.as_bytes());
    let mut stat = sys.stat(Path::new(path)).expect("fake trace exists");
    stat.uid = command.user.as_ref().map_or(sys.euid(), |user| user.uid);
    stat.mode = 0o600;
    sys.stat_of(path, stat);
}

#[cfg(test)]
mod tests;
