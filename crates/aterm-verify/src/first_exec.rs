// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A just-built binary's FIRST EXEC, paid at the point of launch with no clock
//! on it.
//!
//! macOS's syspolicyd assesses every new executable inode the first time it
//! runs and caches the verdict per inode. Measured on the owner's Mac
//! (2026-10-07/08): under a gate's load the first exec of a freshly built 167 MB
//! debug `aterm-gui` took 64 s and the next one 0.13 s. The smokes launch the
//! `aterm-gui` they just built and give its control socket a minute to listen
//! ([`crate::smoke_stages::SOCKET_POLLS`]), so a smoke could go red on the
//! system's wait, not on a defect. A suspended `posix_spawn` does not trigger
//! the assessment (measured): the process has to start.
//!
//! IT RUNS AT THE LAUNCH, NOT AFTER THE BUILD ROW. On this Mac cargo puts each
//! binary at `<target>/debug/<name>` by COPYING it, on every invocation, rebuilt
//! or not (measured 2026-10-08: two no-op `targo build`s, two inodes; a
//! re-copied binary that had already run took 4.3 s and 13 s again, its same
//! inode 20 ms). Every smoke runs its own build first, so a warm-up anywhere
//! before that build warms a file that is gone by the launch. So `bring_up`
//! calls [`warm_first_exec`] after its build, on the path it then launches.
//!
//! The suites' own launches are warmed the same way, by the test helper
//! `aterm_spec::wait::first_exec` (this crate takes no dependencies, so it
//! keeps its own). It decides nothing: one line in the row's report, never a
//! ladder row and never a skip. The launch keeps its hang detector; this takes
//! the system's wait out from under it. Off macOS nothing assesses: the run
//! costs a process start and leaves the binary in the page cache.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::Ctx;
use crate::exec::{self, Capture, Cmd, ExecEnv, Run};
use crate::ladder::Report;

/// How long one warm-up run may take before it is killed: generous, because
/// the wait it absorbs is the system's (64 s measured for `aterm-gui`, and a
/// fresh two-line script's first run 103 s to 600 s on a backed-up
/// syspolicyd) and nothing is decided from it.
pub const BOUND: Duration = Duration::from_secs(10 * 60);

/// The flag `aterm-gui` (the window library's dev bin) answers before any
/// setup: `aterm_gui::main_entry`'s CLI parse prints
/// `build_info::version_display()` and exits on `--version`, before the logger,
/// the config, the updater, a socket or the event loop; ahead of it run only
/// static stamps and the handoff-environment capture, which this run's
/// environment gives nothing to act on.
pub const ATERM_GUI_FLAG: &str = "--version";

/// Run `bin` once with `flag`, under [`BOUND`], right before the caller
/// launches it on a clock, and add one line to `r` ([`warm_within`]).
pub fn warm_first_exec(ctx: &Ctx, r: &mut Report, bin: &Path, flag: &str) {
    warm_within(ctx.exec_env(), BOUND, r, bin, flag);
}

/// [`warm_first_exec`] under `bound`. The child is spawned through
/// [`exec::run`], so it leads its own process group, reads no stdin, is on the
/// list an interrupted gate kills, and is killed with its group at the bound.
/// Its stdout and stderr go nowhere, it starts in `env`'s cwd as every stage
/// child does, and its environment is ONLY a PATH and a scratch HOME, XDG dirs
/// and TMPDIR (`env -i`, as AGENTS.md drives a private instance): no `ATERM_*`
/// session, capability, handoff, fabric or update authority reaches it, and
/// nothing it resolves is the person's.
fn warm_within(env: ExecEnv<'_>, bound: Duration, r: &mut Report, bin: &Path, flag: &str) {
    let what = format!("{} {flag}", file_name(bin));
    let tmp = match crate::mktemp_dir("atv-warm") {
        Ok(tmp) => tmp,
        Err(e) => {
            r.raw(format!("  warm  {what}: not run, no scratch home ({e})"));
            return;
        }
    };
    let mut cmd = Cmd::new(bin).arg(flag).capture(Capture::Silent);
    cmd.envs = scratch_env(env.path, &tmp);
    cmd.exact_env = true;
    let env = ExecEnv {
        child_ceiling: Some(bound),
        ..env
    };
    let started = Instant::now();
    let run = exec::run(&cmd, env);
    r.raw(warm_line(&what, &run, started.elapsed(), bound));
    std::fs::remove_dir_all(&tmp).ok();
}

/// `bin`'s file name, for the report line.
fn file_name(bin: &Path) -> String {
    bin.file_name()
        .unwrap_or(bin.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// The whole environment of a warm-up child: PATH, then HOME, the XDG dirs and
/// TMPDIR, each a directory of its own under `tmp`, made here (a program that
/// looks finds them as on a machine, and empty).
fn scratch_env(path: &OsStr, tmp: &Path) -> Vec<(OsString, OsString)> {
    let mut vars = vec![(OsString::from("PATH"), path.to_os_string())];
    for (name, sub) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "run"),
        ("TMPDIR", "tmp"),
    ] {
        let dir = tmp.join(sub);
        std::fs::create_dir(&dir).ok();
        vars.push((OsString::from(name), dir.into_os_string()));
    }
    vars
}

/// The report line for one run of `what` that took `took`.
fn warm_line(what: &str, run: &Run, took: Duration, bound: Duration) -> String {
    let ms = took.as_millis();
    if let Some(why) = &run.spawn_error {
        return format!("  warm  {what}: could not start ({why})");
    }
    if run.ok {
        return format!("  warm  {what}: {ms} ms");
    }
    match run.code {
        Some(code) => format!("  warm  {what}: {ms} ms, exit {code}"),
        // `exec`'s ceiling ends a child only once the bound has passed; a child
        // dead of a signal before it is a signal death.
        None if took >= bound => format!(
            "  warm  {what}: still running at the {} s bound, killed with its group",
            bound.as_secs()
        ),
        None => format!("  warm  {what}: {ms} ms, ended by a signal"),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    /// A scratch dir removed on drop, so a failed assertion leaves nothing behind.
    struct Scratch(PathBuf);

    impl std::ops::Deref for Scratch {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The sentinel the gate's own environment carries ([`env_in`]): an
    /// `ATERM_*` the warm-up child must never see.
    const SENTINEL: &str = "ATERM_WARM_SENTINEL";

    /// An [`ExecEnv`] in `dir` whose additions carry [`SENTINEL`], as a gate's
    /// carry the variables every other stage child gets.
    fn env_in<'a>(dir: &'a Path, add: &'a [(OsString, OsString)]) -> ExecEnv<'a> {
        ExecEnv {
            cwd: dir,
            path: OsStr::new("/usr/bin:/bin"),
            scratch: dir,
            child_ceiling: Some(exec::DEFAULT_CHILD_CEILING),
            remove_env: &[],
            add_env: add,
            compile_qos: exec::CompileQos::Utility,
        }
    }

    fn sentinel() -> Vec<(OsString, OsString)> {
        vec![(OsString::from(SENTINEL), OsString::from("1"))]
    }

    /// `/bin/sh`, with a script as its flag: the run is `<bin> <flag>`, and
    /// `sh` reads a script it is handed without exec'ing it, so the test waits
    /// on no first-exec assessment of a fresh file ([`BOUND`]: one took 600 s).
    fn sh_script(dir: &Path, body: &str) -> PathBuf {
        let script = dir.join("standin.sh");
        std::fs::write(&script, format!("{body}\n")).expect("write the script");
        script
    }

    /// A present binary runs ONCE, with its flag, in an environment that is
    /// exactly the scratch one: PATH, a HOME, XDG dirs and TMPDIR the run made
    /// and removed, and the shell's own variables; the gate's additions (an
    /// `ATERM_*` sentinel) and its inherited environment never reach it. The
    /// line is reported, and is no ladder row: the warm-up never fails or skips
    /// its stage.
    #[test]
    fn a_binary_runs_once_with_its_flag_in_exactly_the_scratch_environment() {
        let tmp = Scratch(crate::mktemp_dir("atv-warm-test").expect("mktemp"));
        let ran = tmp.join("ran");
        let script = sh_script(
            &tmp,
            &format!(
                "{{ printf 'args=%s\\n' \"$0\"; env; }} >> '{}'",
                ran.display()
            ),
        );
        let add = sentinel();
        let mut r = Report::new("smoke");
        warm_within(
            env_in(&tmp, &add),
            BOUND,
            &mut r,
            Path::new("/bin/sh"),
            script.to_str().expect("a UTF-8 path"),
        );

        let text = std::fs::read_to_string(&ran).expect("the stand-in ran");
        let args: Vec<&str> = text.lines().filter(|l| l.starts_with("args=")).collect();
        assert_eq!(
            args,
            [format!("args={}", script.display())],
            "once, with its flag:\n{text}"
        );
        // What `sh` itself exports to the `env` it runs.
        let shell_own = ["PWD", "OLDPWD", "SHLVL", "_"];
        let names: BTreeSet<&str> = text
            .lines()
            .filter(|l| !l.starts_with("args="))
            .filter_map(|l| l.split_once('=').map(|(name, _)| name))
            .filter(|name| !shell_own.contains(name))
            .collect();
        assert_eq!(
            names,
            BTreeSet::from([
                "HOME",
                "PATH",
                "TMPDIR",
                "XDG_CACHE_HOME",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_RUNTIME_DIR",
                "XDG_STATE_HOME",
            ]),
            "the whole environment, and no {SENTINEL}:\n{text}"
        );
        let value = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                .map(PathBuf::from)
                .unwrap_or_else(|| panic!("no {key} in:\n{text}"))
        };
        let home = value("HOME");
        assert!(
            home.starts_with("/tmp"),
            "a scratch HOME: {}",
            home.display()
        );
        assert!(!home.exists(), "the scratch is removed: {}", home.display());
        for key in ["XDG_CONFIG_HOME", "XDG_RUNTIME_DIR", "TMPDIR"] {
            assert_eq!(value(key).parent(), home.parent(), "{key} is scratch too");
        }

        let rendered = r.render();
        assert_eq!(r.outcomes().count(), 0, "no ladder row:\n{rendered}");
        let lines: Vec<&str> = rendered.lines().filter(|l| l.contains("warm")).collect();
        assert_eq!(lines.len(), 1, "{rendered}");
        assert!(
            lines[0].starts_with("  warm  sh ") && lines[0].ends_with(" ms"),
            "{rendered}"
        );
    }

    /// A binary that does not exit is killed with its group at the bound and
    /// reported so — still no ladder row.
    #[test]
    fn a_binary_that_never_exits_is_killed_at_the_bound() {
        let tmp = Scratch(crate::mktemp_dir("atv-warm-test").expect("mktemp"));
        let script = sh_script(&tmp, "sleep 600");
        let mut r = Report::new("smoke");
        warm_within(
            env_in(&tmp, &[]),
            Duration::from_secs(1),
            &mut r,
            Path::new("/bin/sh"),
            script.to_str().expect("a UTF-8 path"),
        );
        let rendered = r.render();
        assert_eq!(r.outcomes().count(), 0, "no ladder row:\n{rendered}");
        assert!(
            rendered.contains(&format!(
                "  warm  sh {}: still running at the 1 s bound, killed with its group",
                script.display()
            )),
            "{rendered}"
        );
    }
}
