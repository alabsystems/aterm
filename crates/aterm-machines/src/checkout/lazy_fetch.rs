// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How a git keeps a read from fetching an object a partial clone lacks ([`NoLazyFetch`]), told
//! by what the git does rather than by its version.

use super::git;
use crate::model::User;
use crate::port::{Cmd, Ran, Scratch, System};
use std::ffi::OsString;
use std::path::Path;

/// What git says on standard error, in English (`LC_ALL=C`), where `GIT_NO_LAZY_FETCH` stops a
/// lazy fetch: `promisor-remote.c`'s `fetch_objects`, the one place every lazy fetch passes,
/// says it before it would start the fetch, and returns instead.
const REFUSED: &str = "lazy fetching disabled";

/// The label of the scratch folder [`asked`] makes.
const LABEL: &str = "git-lazy-fetch";

/// How a git keeps a read from fetching an object a partial clone lacks, beside the
/// `GIT_NO_LAZY_FETCH=1` every [`git`] run carries ([`NoLazyFetch::of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoLazyFetch {
    /// git takes `--no-lazy-fetch` (2.44 and later), which [`NoLazyFetch::git`] passes too.
    Flag,
    /// git refuses `--no-lazy-fetch` as unknown (a git before 2.44) and obeys the variable
    /// alone, as it was seen to ([`NoLazyFetch::of`]): the May 2024 security releases from
    /// 2.39.4 on, which distributions carry to older versions (Ubuntu 24.04's 2.43.0 is one).
    /// [`NoLazyFetch::git`] passes no option such a git refuses.
    Variable,
}

impl NoLazyFetch {
    /// How `user`'s git ([`git`]) keeps a read from fetching lazily: [`NoLazyFetch::Flag`] where
    /// `git --no-lazy-fetch --version` succeeds; otherwise [`NoLazyFetch::Variable`] where git
    /// was seen to obey the variable ([`obeys_variable`]); otherwise `None`, where git's read did
    /// not show it ([`obeyed`]: it fetched the object, failed without saying it would not fetch
    /// lazily, or a signal ended it). Only a git that refuses the option is asked more than the
    /// one `--version`.
    ///
    /// # Errors
    /// git could not be asked: a run did not start or finish, the scratch folder its question
    /// needs could not be made, or a step setting the question up failed ([`asked`]), so
    /// nothing shows what git's read would do.
    pub(crate) fn of(sys: &dyn System, user: &User) -> Result<Option<Self>, String> {
        Self::of_git(sys, &|| git(sys, user, None))
    }

    /// [`NoLazyFetch::of`] for the git `git` names: a command with no subcommand yet.
    ///
    /// # Errors
    /// As [`NoLazyFetch::of`].
    pub(crate) fn of_git(sys: &dyn System, git: &dyn Fn() -> Cmd) -> Result<Option<Self>, String> {
        let flag = sys
            .run_complete_stdout(&git().args(["--no-lazy-fetch", "--version"]))
            .map_err(|_| "cannot ask Git whether it takes --no-lazy-fetch")?;
        if flag.success() {
            return Ok(Some(Self::Flag));
        }
        Ok(obeys_variable(sys, git)?.then_some(Self::Variable))
    }

    /// [`git`] for `user` in `path`, with `--no-lazy-fetch` where git takes it
    /// ([`NoLazyFetch::Flag`]), and with the variable alone where it does not.
    pub(crate) fn git(self, sys: &dyn System, user: &User, path: Option<&Path>) -> Cmd {
        let command = git(sys, user, path);
        match self {
            Self::Flag => command.arg("--no-lazy-fetch"),
            Self::Variable => command,
        }
    }
}

/// Whether the git `git` names obeys `GIT_NO_LAZY_FETCH=1` alone ([`asked`], [`obeyed`]).
///
/// # Errors
/// As [`asked`].
fn obeys_variable(sys: &dyn System, git: &dyn Fn() -> Cmd) -> Result<bool, String> {
    Ok(obeyed(&asked(sys, git)?))
}

/// Whether `read`, [`asked`]'s, shows a git that obeys `GIT_NO_LAZY_FETCH`: it failed, so the
/// object never arrived, and git said it would not fetch lazily ([`REFUSED`]). A git that ignores
/// the variable fetches the object from the promisor and succeeds; one that tried the promisor
/// and failed says nothing of the kind.
fn obeyed(read: &Ran) -> bool {
    read.status.is_some_and(|code| code != 0)
        && String::from_utf8_lossy(&read.stderr).contains(REFUSED)
}

/// The read that shows whether the git `git` names obeys `GIT_NO_LAZY_FETCH=1`, in a scratch
/// folder of the System port's: two bare repositories, `promisor.git`, which holds one object
/// (the blob of its own `HEAD` file), and `partial.git`, which lacks it and names `promisor.git`
/// (a local path, so no fetch leaves this machine) as its promisor remote (`-c`
/// `remote.origin.promisor=true`); then `partial.git` asked for the object's type under the
/// variable. Neither the system's nor the person's git config is read (`GIT_CONFIG_NOSYSTEM`,
/// `GIT_CONFIG_GLOBAL=/dev/null`), so none of their settings (a `url.<base>.insteadOf`, a
/// `protocol.file.allow`) changes what the read does: the question is the program's. Four
/// short git runs, writing nothing outside the scratch folder.
///
/// # Errors
/// The question could not be asked, so nothing shows what git's read would do: the scratch
/// folder could not be made, a run did not start or finish, or a step setting the question up
/// failed (where the temporary folder is full or unwritable, say), named with the first line
/// git said, or `hash-object` named no object. Only the read itself is git's answer.
fn asked(sys: &dyn System, git: &dyn Fn() -> Cmd) -> Result<Ran, String> {
    const WHY: &str = "cannot see whether Git obeys GIT_NO_LAZY_FETCH";
    let scratch = Scratch::new(sys, LABEL).map_err(|_| WHY)?;
    let promisor = scratch.path().join("promisor.git");
    let partial = scratch.path().join("partial.git");
    let isolated = || {
        git()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
    };
    let run = |command: Cmd| {
        sys.run_complete_stdout(&command)
            .map_err(|_| WHY.to_owned())
    };
    let set_up = |step: &str, command: Cmd| -> Result<Ran, String> {
        let ran = run(command)?;
        if ran.success() {
            Ok(ran)
        } else {
            Err(format!("{WHY}: git {step} failed ({})", ran.describe()))
        }
    };
    for repository in [&promisor, &partial] {
        set_up(
            "init",
            isolated()
                .args(["init", "--bare", "--template=", "--quiet", "--"])
                .arg(repository),
        )?;
    }
    let hashed = set_up(
        "hash-object",
        isolated()
            .arg("--git-dir")
            .arg(&promisor)
            .args(["hash-object", "-w", "--no-filters", "--"])
            .arg(promisor.join("HEAD")),
    )?;
    let object = hashed.out();
    let object = object.trim();
    if !matches!(object.len(), 40 | 64) || !object.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{WHY}: git hash-object named no object"));
    }
    let mut url = OsString::from("remote.origin.url=");
    url.push(promisor.as_os_str());
    run(isolated()
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(["-c", "remote.origin.promisor=true", "-c"])
        .arg(url)
        .arg("--git-dir")
        .arg(&partial)
        .args(["cat-file", "-t", object]))
}

/// The object [`before_the_option`]'s `hash-object` names.
#[cfg(any(test, debug_assertions))]
const OBJECT_NAMED: &str = "4444444444444444444444444444444444444444";

/// How a git before 2.44 answers [`NoLazyFetch::of`] on a test double: `--no-lazy-fetch`
/// refused as unknown (129); in the question's scratch folder ([`LABEL`]), its repositories
/// made and its object hashed; and its read failed, saying lazy fetching is off ([`REFUSED`]),
/// where it `obeys` the variable, or the object fetched from the promisor and named where it
/// does not. `None` for every other command.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn before_the_option(command: &Cmd, obeys: bool) -> Option<Ran> {
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
    } else if has("cat-file") && obeys {
        Some(Ran::exit(
            128,
            "warning: lazy fetching disabled; some objects may not be available\n\
             fatal: git cat-file: could not get object info\n",
        ))
    } else if has("cat-file") {
        Some(Ran::ok("blob\n"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
