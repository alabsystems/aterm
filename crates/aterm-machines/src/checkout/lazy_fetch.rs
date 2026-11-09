// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How a git keeps a read from fetching an object a partial clone lacks ([`NoLazyFetch`]), told
//! by what the git does rather than by its version.

use super::{NO_SIDE_RUNS, git};
use crate::model::User;
use crate::port::{Cmd, Ran, Scratch, System};
use std::ffi::OsString;
use std::path::Path;

mod trace;

/// The probe's small local-object trace is bounded before decoding.
const TRACE_LIMIT: usize = 256 * 1024;

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
    /// was seen to obey the variable ([`obeys_variable`]); otherwise `None`, where git's read
    /// fetched or attempted to fetch the object. Only a git that refuses
    /// the option is asked more than the one `--version`.
    ///
    /// # Errors
    /// git could not be asked: a run did not start, finish or exit normally, the scratch folder
    /// its question needs could not be made, a setup step failed, its trace was unavailable or incomplete,
    /// or the local-fetch control did not establish the observation ([`asked`]).
    pub(crate) fn of(sys: &dyn System, user: &User) -> Result<Option<Self>, String> {
        Self::of_git(sys, &|| git(sys, user, None))
    }

    /// [`NoLazyFetch::of`] for the git `git` names: a command with no subcommand yet.
    ///
    /// # Errors
    /// As [`NoLazyFetch::of`].
    pub(crate) fn of_git(sys: &dyn System, git: &dyn Fn() -> Cmd) -> Result<Option<Self>, String> {
        let command = git();
        let flag = sys
            .run_complete_stdout(&command.clone().args(["--no-lazy-fetch", "--version"]))
            .map_err(|_| "cannot ask Git whether it takes --no-lazy-fetch")?;
        if flag.status.is_none() {
            return Err(format!(
                "cannot ask Git whether it takes --no-lazy-fetch: {}",
                flag.describe()
            ));
        }
        if flag.success() {
            return Ok(Some(Self::Flag));
        }
        Ok(obeys_variable(sys, &|| command.clone())?.then_some(Self::Variable))
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

/// Whether the git `git` names obeys `GIT_NO_LAZY_FETCH=1` alone ([`asked`]).
///
/// # Errors
/// As [`asked`].
fn obeys_variable(sys: &dyn System, git: &dyn Fn() -> Cmd) -> Result<bool, String> {
    let probe = asked(sys, git)?;
    Ok(probe.read.status.is_some_and(|code| code != 0) && probe.no_fetch)
}

struct Probe {
    read: Ran,
    no_fetch: bool,
}

/// The read that shows whether the git `git` names obeys `GIT_NO_LAZY_FETCH=1`, in a scratch
/// folder of the System port's: two bare repositories, `promisor.git`, which holds one object
/// (the blob of its own `HEAD` file), and `partial.git`, which lacks it and names `promisor.git`
/// (a local path, so no fetch leaves this machine) as its promisor remote (`-c`
/// `remote.origin.promisor=true`); then `partial.git` asked for the object's type under the
/// variable. A complete Trace2 session must show no child or exec attempt. A second read of the
/// same object with the variable disabled must fetch it and expose that attempt in a complete
/// trace of the same format. Both reads concern only this private local promisor. Neither the system's nor the person's git config is read (`GIT_CONFIG_NOSYSTEM`,
/// `GIT_CONFIG_GLOBAL=/dev/null`), so none of their settings (a `url.<base>.insteadOf`, a
/// `protocol.file.allow`) changes what the read does: the question is the program's. Automatic
/// side runs are disabled. Trace files are private, bounded, and removed with the scratch owner.
///
/// # Errors
/// The question could not be asked, so nothing shows what git's read would do: the scratch
/// folder could not be made, a run did not start or finish normally, or a step setting the question up
/// failed (where the temporary folder is full or unwritable, say), named with the first line
/// git said, or `hash-object` named no object. Missing, oversized or incomplete traces and an
/// unsuccessful local-fetch control also establish no capability and return an error.
fn asked(sys: &dyn System, git: &dyn Fn() -> Cmd) -> Result<Probe, String> {
    const WHY: &str = "cannot see whether Git obeys GIT_NO_LAZY_FETCH";
    let base = git();
    let other_user = base.user.as_ref().filter(|user| user.uid != sys.euid());
    // A privileged process's TMPDIR can itself be private to root. The shared temporary
    // root permits traversal to the private allocation transferred to the Git actor.
    let scratch = if cfg!(unix) && other_user.is_some() {
        Scratch::new_in(sys, Path::new("/tmp"), LABEL)
    } else {
        Scratch::new(sys, LABEL)
    }
    .map_err(|_| WHY)?;
    let promisor = scratch.path().join("promisor.git");
    let partial = scratch.path().join("partial.git");
    if let Some(user) = other_user {
        sys.chown(scratch.path(), user.uid, user.gid)
            .map_err(|error| format!("{WHY}: cannot own scratch directory ({error})"))?;
    }
    let isolated = || {
        base.clone()
            .args(NO_SIDE_RUNS)
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
    let read = |value: &str, name: &str| -> Result<(Ran, std::path::PathBuf, u32), String> {
        let path = scratch.path().join(name);
        let path_text = path
            .to_str()
            .ok_or_else(|| format!("{WHY}: non-UTF-8 trace path"))?;
        let uid = base.user.as_ref().map_or(sys.euid(), |user| user.uid);
        sys.write_file(&path, b"", 0o600)
            .map_err(|error| format!("{WHY}: cannot prepare trace ({error})"))?;
        if let Some(user) = base.user.as_ref().filter(|user| user.uid != sys.euid()) {
            sys.chown(&path, user.uid, user.gid)
                .map_err(|error| format!("{WHY}: cannot own trace ({error})"))?;
        }
        let ran = run(isolated()
            .env("GIT_NO_LAZY_FETCH", value)
            .env("GIT_TRACE2_EVENT", path_text)
            .args(["-c", "remote.origin.promisor=true", "-c"])
            .arg(&url)
            .arg("--git-dir")
            .arg(&partial)
            .args(["cat-file", "-t", object]))?;
        Ok((ran, path, uid))
    };
    let observe = |path: &Path, uid: u32, code: i32| {
        let bytes = sys
            .read_owned_file(path, uid, TRACE_LIMIT, true)
            .map_err(|error| format!("{WHY}: cannot read trace ({error})"))?;
        trace::observe(&bytes, object, code).map_err(|why| format!("{WHY}: {why}"))
    };
    let (subject, path, uid) = read("1", "subject.trace")?;
    let code = subject.status.ok_or_else(|| {
        format!(
            "{WHY}: Git read did not exit normally ({})",
            subject.describe()
        )
    })?;
    if code == 0 {
        return Ok(Probe {
            read: subject,
            no_fetch: false,
        });
    };
    let subject_trace = observe(&path, uid, code)?;
    if subject_trace.attempted {
        return Ok(Probe {
            read: subject,
            no_fetch: false,
        });
    }
    let (control, path, uid) = read("0", "control.trace")?;
    if !control.success() || control.out().trim() != "blob" {
        return Err(format!(
            "{WHY}: local-fetch control failed ({})",
            control.describe()
        ));
    }
    let control_trace = observe(&path, uid, 0)?;
    if !control_trace.attempted || control_trace.format != subject_trace.format {
        return Err(format!(
            "{WHY}: local-fetch control did not establish the trace"
        ));
    }
    Ok(Probe {
        read: subject,
        no_fetch: true,
    })
}

/// The object [`before_the_option`]'s `hash-object` names.
#[cfg(any(test, debug_assertions))]
const OBJECT_NAMED: &str = "4444444444444444444444444444444444444444";

/// How a git before 2.44 answers [`NoLazyFetch::of`] on a test double: `--no-lazy-fetch`
/// refused as unknown (129); in the question's scratch folder ([`LABEL`]), its repositories
/// made and its object hashed; and its read failed quietly with no fetch attempt,
/// where it `obeys` the variable, or the object fetched from the promisor and named where it
/// does not. `None` for every other command.
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
        .chain(command.args.iter().map(OsString::as_os_str))
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
