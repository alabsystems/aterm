// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-winship` — the Windows owner loop, as code: smoke a packaged build
//! headless in an isolated environment (`smoke`), build, stage and install
//! origin/main without stopping a running window (`update`), run that daily
//! (`schedule`), say what is installed and staged (`status`), and build a
//! claimed release commit's Windows zip and the receipt the release cutter
//! admits (`release-build`, `handoff`), and serve the release pump's requests
//! for one through the dev repository, signed by this host's pinned builder key
//! (`release-serve`, `release-key`, `pull`, `attest`).
//!
//! A dev tool: nothing here is linked into the shipped `aterm`. It runs the
//! dist's own images and the tree's own `apps\aterm-win\build.ps1` and
//! `install.ps1`, and shares two records with them (`pointer`): `BUILD.json`
//! in a dist folder and `%LOCALAPPDATA%\aterm\dist\latest.json`.
//!
//! It never ends a process it did not start: the one instance a smoke starts
//! is born in a job object and ended through that handle (`proc::Job`).
//!
//! No path of the checkout that built it is compiled in: the repository is
//! decided when it runs (`repo`).

pub mod attest;
pub mod env;
pub mod handoff;
pub mod plan;
pub mod pointer;
pub mod proc;
pub mod pull;
pub mod release;
pub mod repo;
pub mod schedule;
pub mod serve;
pub mod smoke;
pub mod update;

use std::path::{Path, PathBuf};

use crate::proc::Place;
use crate::update::{Layout, Out};

/// Exit code: the verb did what it was asked, every row passed.
pub const PASS: i32 = 0;
/// Exit code: a row, the build or the install failed.
pub const FAILED: i32 = 1;
/// Exit code: the command line was not understood.
pub const USAGE: i32 = 2;
/// Exit code: the verb could not run; nothing was judged.
pub const NOT_RUN: i32 = 3;

/// The help text.
pub const HELP: &str = "\
aterm-winship — the Windows owner loop (dev tool)

USAGE:
    aterm-winship smoke <dist dir> [--scratch <dir>] [--keep] [--frame <png>]
    aterm-winship update [--repo <dir>] [--no-install] [--restart] [--force] [--log]
    aterm-winship schedule [--daily HH:MM] [--repo <dir>] [--remove] [--dry-run]
    aterm-winship status [--repo <dir>]
    aterm-winship release-build --version <v> --build-number <n> --commit <sha> --out <dir>
                                [--repo <dir>] [--sign-with <key> [--builder <id>]]
    aterm-winship release-key init|show [--key <path>] [--builder <id>]
    aterm-winship release-serve [--once] [--repo <dir>] [--key <path>] [--builder <id>] [--log]

smoke     Start <dist>\\aterm-gui.exe --headless in an isolated environment
          (a scratch root; TMP, TEMP, LOCALAPPDATA, APPDATA and USERPROFILE
          redirected into it; an allowlist of machine variables), drive it
          through <dist>\\aterm.exe ctl, print one line per row, end the
          instance by the handle held on it and delete the scratch root.
          --scratch names the root (it must be empty or absent), --keep leaves
          it (with each sys view's answer in sys-<view>.txt), --frame copies
          the frame out.
          Exit 0 every row passed, 1 a row failed, 3 it could not run.
update    Fetch origin, check origin/main out in the build tree
          %LOCALAPPDATA%\\aterm\\build\\src (a worktree of the repository), run
          its apps\\aterm-win\\build.ps1 into
          %LOCALAPPDATA%\\aterm\\dist\\<commit12>.building (with -Tool, so the
          tree's own aterm-winship.exe is built beside it), give the finished
          folder the name <commit12>, smoke it with the tree's own tool (which,
          once it passed or failed the build, replaces the copy the scheduled
          task runs), write
          %LOCALAPPDATA%\\aterm\\dist\\latest.json, keep the staged, the
          previous and the installed dist and delete older ones, then run the
          dist's install.ps1 -Staged (-Staged -Restart with --restart; -Update
          too over an install that is already there, so its shortcuts and PATH
          entry are left as they are; nothing with --no-install). Builds
          nothing when the pointer already names origin/main, and installs
          nothing over a newer installed build (--force does both). --log
          appends the run to %LOCALAPPDATA%\\aterm\\dist\\update.log.
schedule  Register two per-user Scheduled Tasks, from a copy of this tool at
          %LOCALAPPDATA%\\aterm\\build\\aterm-winship.exe and with no window:
          aterm-winship-update runs `update --log --repo <the repository>`
          daily at HH:MM (04:30), and aterm-winship-release-serve runs
          `release-serve --once --log --repo <the repository>` every 10
          minutes. Each runs on battery, is not stopped when the PC goes on
          battery, runs a start missed while the PC slept or was off once it
          is back, and passes over a start while it is still running.
          --remove removes both; --dry-run prints the schtasks lines and the
          tasks' XML and changes nothing.
status    The installed build, the staged pointer, the repository, origin/main
          and the build tree. Reads only.
release-build
          Build the release cut's Windows zip, aterm-<v>-windows-x86_64.zip,
          and its receipt <zip>.handoff.toml into --out (absent or empty), for
          `targo --unverified ship cut --windows-artifacts <dir>` on the Mac.
          The checkout (--repo, else the working directory) must be the clean
          commit. It runs the checkout's apps\\aterm-win\\build.ps1 -Zip with
          the release identity pinned (ATERM_STAMP=commit, the build number as
          SOURCE_DATE_EPOCH, the commit, ATERM_RELEASE_BUILD=1) and the lane
          named: the Trust build the commit's trust.lock pins when the targo
          on PATH is it, else the stock release its stock_pin.rs pins, given
          to build.ps1 by path; cargo configuration the commit does not hold
          refuses the build. It checks BUILD.json, that the checkout is still
          the clean commit and the compiler unchanged, and admits the pair as
          the cutter will. Its last line prints the zip's SHA-256, which the
          Mac's `ship release --windows-sha256` must name: the carry is not
          trusted. --sign-with <key> also writes <zip>.handoff.sig, the
          receipt signed by that builder key as --builder (this computer's
          name), which the cut admits in place of the typed digest when
          apps\\aterm-win\\release-builders.toml pins the builder. Exit 0
          made, 1 the build or a check failed, 3 nothing was built.
release-key
          init makes this host's builder key, a PKCS#8 Ed25519 key at --key
          (%USERPROFILE%\\.aterm\\windows-builder\\ed25519.pk8), when there is
          none (one that is there is kept, never overwritten); init and show
          print the builder id (--builder, else this computer's name) and the
          public key, as the [[builder]] table to pin in
          apps\\aterm-win\\release-builders.toml, admitted for 90 days. The
          private key is never printed. Whoever can read the key file can
          have a pump release ship a Windows zip of their own until its
          not_after: keep it in the profile of a Windows account that runs
          release-serve and nothing else (no agent, no branch build), and pin
          a new key before the old one's not_after.
release-serve
          Serve the release pump's requests for a Windows zip through origin.
          A poll fetches origin/main and refs/aterm-release/windows/request;
          serves only when origin/main's apps\\aterm-win\\release-builders.toml
          pins this builder with this key, unexpired; checks the request (its
          one parent the release commit, whose one parent is on origin/main
          and which changes only RELEASES.ledger, CHANGELOG.md and entry files
          under changes/, pinning this builder, built fewer than twice);
          pushes the heartbeat refs/aterm-release/windows/alive, which names
          the request and, when it passes the request over, why; and when
          origin has no handoff for it yet, checks the release commit out in
          %LOCALAPPDATA%\\aterm\\release\\src (a worktree of the repository),
          builds it with this tool's release-build (holding update's lock),
          signs the receipt, and pushes the zip in parts with its receipt and
          signature as refs/aterm-release/windows/handoff/<v>-<n>-<commit12>,
          deleting the handoff refs of older requests. git never prompts for
          a credential. --once polls once, else every 10 minutes; --log
          appends to %LOCALAPPDATA%\\aterm\\release\\serve.log. Exit 0 the poll
          did its part, 1 a request was refused or a build or push failed, 3
          it could not run.

The repository is --repo <dir>; else the one recorded in
%LOCALAPPDATA%\\aterm\\build\\repo.txt (written when the build tree is made and
when the task is scheduled); else the one the build tree is a worktree of;
else the aterm checkout this is started in. It is named by its main checkout.
";

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    /// Print [`HELP`].
    Help,
    /// `smoke`.
    Smoke(smoke::Options),
    /// `update`; `log` appends the run to the update log.
    Update {
        /// What to build and whether to install.
        opts: update::Options,
        /// Append the run to the update log.
        log: bool,
    },
    /// `schedule`.
    Schedule {
        /// The time of day, `HH:MM`.
        at: String,
        /// The `--repo` the task passes to `update`; decided at run time when `None`.
        repo: Option<PathBuf>,
        /// Remove the task.
        remove: bool,
        /// Print the schtasks line and the task's XML only.
        dry_run: bool,
    },
    /// `status`.
    Status {
        /// The repository whose origin/main is read; decided at run time when `None`.
        repo: Option<PathBuf>,
    },
    /// `release-build`.
    ReleaseBuild(release::Options),
    /// `release-key`.
    ReleaseKey {
        /// `init` makes the key when there is none; `show` only reads it.
        init: bool,
        /// The key file; [`attest::default_key`] when `None`.
        key: Option<PathBuf>,
        /// The builder id; this computer's name when `None`.
        builder: Option<String>,
    },
    /// `release-serve`; `log` appends the polls to the serve log.
    ReleaseServe {
        /// What to serve with.
        opts: serve::Options,
        /// Append the polls to the serve log.
        log: bool,
    },
}

type Words<'a> = std::slice::Iter<'a, String>;

/// Parses the arguments after the program name.
///
/// # Errors
/// What was not understood, one line.
pub fn parse(args: &[String]) -> Result<Verb, String> {
    let Some((verb, rest)) = args.split_first() else {
        return Ok(Verb::Help);
    };
    let mut words: Words<'_> = rest.iter();
    let value = |flag: &str, words: &mut Words<'_>| {
        words
            .next()
            .cloned()
            .ok_or_else(|| format!("{verb}: {flag} takes a value"))
    };
    let unknown = |word: &str| format!("{verb}: unknown argument '{word}' (aterm-winship help)");
    match verb.as_str() {
        "help" | "--help" | "-h" => Ok(Verb::Help),
        "smoke" => {
            let mut opts = smoke::Options {
                dist: PathBuf::new(),
                scratch: None,
                keep: false,
                frame: None,
            };
            let mut dist = None;
            while let Some(word) = words.next() {
                match word.as_str() {
                    "--scratch" => opts.scratch = Some(PathBuf::from(value(word, &mut words)?)),
                    "--frame" => opts.frame = Some(PathBuf::from(value(word, &mut words)?)),
                    "--keep" => opts.keep = true,
                    flag if flag.starts_with("--") => return Err(unknown(flag)),
                    path if dist.is_none() => dist = Some(PathBuf::from(path)),
                    extra => return Err(unknown(extra)),
                }
            }
            opts.dist =
                dist.ok_or("smoke: name the dist folder (aterm-winship smoke <dist dir>)")?;
            Ok(Verb::Smoke(opts))
        }
        "update" => {
            let mut opts = update::Options {
                repo: None,
                no_install: false,
                restart: false,
                force: false,
            };
            let mut log = false;
            while let Some(word) = words.next() {
                match word.as_str() {
                    "--repo" => opts.repo = Some(PathBuf::from(value(word, &mut words)?)),
                    "--no-install" => opts.no_install = true,
                    "--restart" => opts.restart = true,
                    "--force" => opts.force = true,
                    "--log" => log = true,
                    other => return Err(unknown(other)),
                }
            }
            if opts.no_install && opts.restart {
                return Err("update: --restart restarts windows for an install; --no-install installs nothing".to_string());
            }
            Ok(Verb::Update { opts, log })
        }
        "schedule" => {
            let (mut at, mut repo, mut remove, mut dry_run) =
                (schedule::DEFAULT_TIME.to_string(), None, false, false);
            while let Some(word) = words.next() {
                match word.as_str() {
                    "--daily" => at = schedule::time(&value(word, &mut words)?)?,
                    "--repo" => repo = Some(PathBuf::from(value(word, &mut words)?)),
                    "--remove" => remove = true,
                    "--dry-run" => dry_run = true,
                    other => return Err(unknown(other)),
                }
            }
            Ok(Verb::Schedule {
                at,
                repo,
                remove,
                dry_run,
            })
        }
        "status" => {
            let mut repo = None;
            while let Some(word) = words.next() {
                match word.as_str() {
                    "--repo" => repo = Some(PathBuf::from(value(word, &mut words)?)),
                    other => return Err(unknown(other)),
                }
            }
            Ok(Verb::Status { repo })
        }
        "release-build" => {
            let (mut version, mut build, mut commit, mut out, mut repo) =
                (None, None, None, None, None);
            let (mut sign_with, mut builder) = (None, None);
            while let Some(word) = words.next() {
                match word.as_str() {
                    "--version" => version = Some(value(word, &mut words)?),
                    "--build-number" => {
                        let text = value(word, &mut words)?;
                        build = Some(text.parse::<u64>().map_err(|_| {
                            format!("{verb}: --build-number {text:?} is not a number")
                        })?);
                    }
                    "--commit" => commit = Some(value(word, &mut words)?),
                    "--out" => out = Some(PathBuf::from(value(word, &mut words)?)),
                    "--repo" => repo = Some(PathBuf::from(value(word, &mut words)?)),
                    "--sign-with" => sign_with = Some(PathBuf::from(value(word, &mut words)?)),
                    "--builder" => builder = Some(value(word, &mut words)?),
                    other => return Err(unknown(other)),
                }
            }
            let need = |flag: &str| format!("{verb}: {flag} is required (aterm-winship help)");
            let sign = match (sign_with, builder) {
                (Some(key), builder) => Some(release::Sign { key, builder }),
                (None, Some(_)) => {
                    return Err(format!(
                        "{verb}: --builder names who signs, and --sign-with names no key"
                    ));
                }
                (None, None) => None,
            };
            let opts = release::Options {
                version: version.ok_or_else(|| need("--version"))?,
                build: build.ok_or_else(|| need("--build-number"))?,
                commit: commit.ok_or_else(|| need("--commit"))?,
                out: out.ok_or_else(|| need("--out"))?,
                repo,
                sign,
            };
            opts.check()?;
            Ok(Verb::ReleaseBuild(opts))
        }
        "release-key" => {
            let init = match words.next().map(String::as_str) {
                Some("init") => true,
                Some("show") => false,
                _ => return Err(format!("{verb}: say init or show (aterm-winship help)")),
            };
            let (mut key, mut builder) = (None, None);
            while let Some(word) = words.next() {
                match word.as_str() {
                    "--key" => key = Some(PathBuf::from(value(word, &mut words)?)),
                    "--builder" => builder = Some(value(word, &mut words)?),
                    other => return Err(unknown(other)),
                }
            }
            if let Some(id) = &builder {
                attest::check_id(id).map_err(|e| format!("{verb}: {e}"))?;
            }
            Ok(Verb::ReleaseKey { init, key, builder })
        }
        "release-serve" => {
            let mut opts = serve::Options {
                once: false,
                repo: None,
                key: None,
                builder: None,
            };
            let mut log = false;
            while let Some(word) = words.next() {
                match word.as_str() {
                    "--once" => opts.once = true,
                    "--repo" => opts.repo = Some(PathBuf::from(value(word, &mut words)?)),
                    "--key" => opts.key = Some(PathBuf::from(value(word, &mut words)?)),
                    "--builder" => opts.builder = Some(value(word, &mut words)?),
                    "--log" => log = true,
                    other => return Err(unknown(other)),
                }
            }
            if let Some(id) = &opts.builder {
                attest::check_id(id).map_err(|e| format!("{verb}: {e}"))?;
            }
            Ok(Verb::ReleaseServe { opts, log })
        }
        other => Err(format!("unknown verb '{other}' (aterm-winship help)")),
    }
}

fn system32(env: &[(String, String)], image: &str) -> PathBuf {
    env::lookup(env, "SystemRoot").map_or_else(
        || PathBuf::from(image),
        |root| Path::new(root).join("System32").join(image),
    )
}

/// The account the task is registered for: the caller's, as `DOMAIN\name`.
fn caller(env: &[(String, String)]) -> Option<String> {
    let name = env::lookup(env, "USERNAME").filter(|n| !n.is_empty())?;
    Some(
        match env::lookup(env, "USERDOMAIN").filter(|d| !d.is_empty()) {
            Some(domain) => format!("{domain}\\{name}"),
            None => name.to_string(),
        },
    )
}

/// Now on the local clock, as seconds since the epoch shifted by the zone.
fn local_now() -> u64 {
    let utc = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let offset = aterm_uds::zone::local_offset_now().unwrap_or_default();
    utc.saturating_add_signed(offset)
}

/// Copies the running tool to where the scheduled task runs it from.
fn keep_tool(layout: &Layout) -> Result<PathBuf, String> {
    let tool = layout.tool();
    let me = std::env::current_exe().map_err(|e| format!("this tool's own path: {e}"))?;
    if me != tool {
        if let Some(dir) = tool.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let bytes = std::fs::read(&me).map_err(|e| format!("{}: {e}", me.display()))?;
        aterm_uds::atomic_file::write(&tool, &bytes, 0o755)
            .map_err(|e| format!("{}: {e}", tool.display()))?;
    }
    Ok(tool)
}

fn run_schtasks(args: &[String], env: &[(String, String)], out: &mut Out) -> i32 {
    match proc::run(
        &system32(env, "schtasks.exe"),
        args,
        Place::default(),
        smoke::HANG,
    ) {
        Ok(ran) if ran.ok() => {
            out.say(&format!("schedule: {}", smoke::one_line(&ran.text(), 200)));
            PASS
        }
        Ok(ran) => {
            out.say(&format!(
                "schedule: schtasks exited {:?}: {}",
                ran.code,
                smoke::one_line(&ran.text(), 200)
            ));
            FAILED
        }
        Err(e) => {
            out.say(&format!("schedule: {e}"));
            NOT_RUN
        }
    }
}

/// The first code of `codes` that is not [`PASS`], else [`PASS`].
fn first_failure(codes: &[i32]) -> i32 {
    codes.iter().copied().find(|&c| c != PASS).unwrap_or(PASS)
}

fn unschedule(dry_run: bool, layout: &Layout, env: &[(String, String)], out: &mut Out) -> i32 {
    let mut codes = Vec::new();
    for task in schedule::Task::ALL {
        let args = schedule::delete_args(task);
        if dry_run {
            out.say(&schedule::shown(&args));
            continue;
        }
        // A task that is not registered (one scheduled before this tool had it) is no
        // failure of its removal.
        match proc::run(
            &system32(env, "schtasks.exe"),
            &schedule::query_args(task),
            Place::default(),
            smoke::HANG,
        ) {
            Ok(ran) if !ran.ok() => {
                out.say(&format!("schedule: {} is not registered", task.name()));
                let _ = std::fs::remove_file(layout.task_xml(task));
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                out.say(&format!("schedule: {e}"));
                codes.push(NOT_RUN);
                continue;
            }
        }
        let code = run_schtasks(&args, env, out);
        if code == PASS {
            let _ = std::fs::remove_file(layout.task_xml(task));
        }
        codes.push(code);
    }
    let code = first_failure(&codes);
    if !dry_run && code == PASS {
        // The record of the repository stays: `update` by hand still reads it.
        let _ = std::fs::remove_file(layout.tool());
    }
    code
}

fn run_schedule(
    at: &str,
    repo: Option<&Path>,
    dry_run: bool,
    layout: &Layout,
    env: &[(String, String)],
    out: &mut Out,
) -> i32 {
    // The tasks are given the repository by its absolute path, checked now: they
    // start in the system directory, long after this checkout may be gone.
    let now = local_now();
    let made = std::env::current_dir()
        .map_err(|e| format!("the working directory: {e}"))
        .and_then(|cwd| repo::resolve(repo, layout, &cwd))
        .and_then(|repo| {
            let mut tasks = Vec::new();
            for task in schedule::Task::ALL {
                let start = match task {
                    schedule::Task::Update => schedule::start_boundary(now, at)?,
                    schedule::Task::Serve => schedule::soon_boundary(now),
                };
                let xml = schedule::task_xml(
                    task,
                    &system32(env, "conhost.exe"),
                    &layout.tool(),
                    &repo.dir,
                    &start,
                    caller(env).as_deref(),
                )?;
                tasks.push((task, start, xml));
            }
            Ok((repo, tasks))
        });
    let (repo, tasks) = match made {
        Ok(made) => made,
        Err(e) => {
            out.say(&format!("schedule: not run: {e}"));
            return NOT_RUN;
        }
    };
    if dry_run {
        for (task, _, xml) in &tasks {
            let file = layout.task_xml(*task);
            out.say(&schedule::shown(&schedule::create_args(*task, &file)));
            out.say(&format!("{}:", file.display()));
            for line in xml.lines() {
                out.say(line);
            }
        }
        for (task, start, _) in &tasks {
            out.say(&format!(
                "schedule: dry run; {} would run `{} --repo {}` {}, first on {start}",
                task.name(),
                task.verb(),
                repo.dir.display(),
                task.when()
            ));
        }
        out.say(&format!(
            "schedule: dry run; would copy this tool to {}, record the repository {} ({}) and register both tasks: each {}; nothing changed",
            layout.tool().display(),
            repo.dir.display(),
            repo.from.word(),
            schedule::CONDITIONS
        ));
        return PASS;
    }
    let written = keep_tool(layout)
        .and_then(|_| repo::record(layout, &repo.dir))
        .and_then(|()| {
            tasks.iter().try_for_each(|(task, _, xml)| {
                let file = layout.task_xml(*task);
                aterm_uds::atomic_file::write(&file, &schedule::utf16(xml), 0o644)
                    .map_err(|e| format!("{}: {e}", file.display()))
            })
        });
    if let Err(e) = written {
        out.say(&format!("schedule: not run: {e}"));
        return NOT_RUN;
    }
    let mut codes = Vec::new();
    for (task, start, _) in &tasks {
        let code = run_schtasks(
            &schedule::create_args(*task, &layout.task_xml(*task)),
            env,
            out,
        );
        if code == PASS {
            out.say(&format!(
                "schedule: {} runs `{} --repo {}` {}, first on {start}: it {}",
                task.name(),
                task.verb(),
                repo.dir.display(),
                task.when(),
                schedule::CONDITIONS
            ));
        }
        codes.push(code);
    }
    first_failure(&codes)
}

/// What `release-key` says of the key at `path` for `builder`, `today_secs` being now: makes it
/// first with `init` ([`attest::init_key`]). Its lines go to `say`; the exit code.
fn release_key(
    init: bool,
    path: &Path,
    builder: &str,
    today_secs: u64,
    say: &mut dyn FnMut(&str),
) -> i32 {
    if init {
        match attest::init_key(path) {
            Ok(true) => say(&format!("release-key: made {}", path.display())),
            Ok(false) => say(&format!(
                "release-key: {} holds a key already; it is kept",
                path.display()
            )),
            Err(e) => {
                say(&format!("release-key: {e}"));
                return FAILED;
            }
        }
    }
    let public = match attest::read_key(path).and_then(|key| attest::public_key(&key)) {
        Ok(public) => public,
        Err(e) => {
            say(&format!(
                "release-key: not run: {e} (make one with `aterm-winship release-key init`)"
            ));
            return NOT_RUN;
        }
    };
    say(&format!("builder:    {builder}"));
    say(&format!("public key: {public}"));
    say(&format!(
        "pin it in {} (not_after is the last day its attestations are admitted: whoever can \
         read the key file until then can attest a zip, so keep it in an account that runs \
         release-serve alone, and pin a new key before then):",
        pull::BUILDERS_FILE
    ));
    say("[[builder]]");
    say(&format!("id = \"{builder}\""));
    say(&format!("public_key = \"{public}\""));
    say(&format!(
        "not_after = \"{}\"",
        pull::date_of(today_secs + attest::PIN_DAYS * 86_400)
    ));
    PASS
}

/// Runs the tool on `args` (those after the program name); the exit code.
#[must_use]
pub fn run(args: &[String]) -> i32 {
    let verb = match parse(args) {
        Ok(Verb::Help) => {
            print!("{HELP}");
            return PASS;
        }
        Ok(verb) => verb,
        Err(e) => {
            eprintln!("aterm-winship: {e}");
            return USAGE;
        }
    };
    if !cfg!(windows) {
        eprintln!("aterm-winship: not run: this tool drives Windows builds and runs on Windows");
        return NOT_RUN;
    }
    let env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect();
    if let Verb::Smoke(opts) = &verb {
        let outcome = smoke::smoke(opts, &env, &mut |line| println!("{line}"));
        match &outcome {
            smoke::Outcome::Ran(rows) => {
                let failed = rows.iter().filter(|(_, v)| v.is_err()).count();
                println!("smoke: {} rows, {failed} failed", rows.len());
            }
            smoke::Outcome::NotRun(why) => println!("NOT RUN smoke: {why}"),
        }
        return outcome.code();
    }
    if let Verb::ReleaseBuild(opts) = &verb {
        return match release::release_build(opts, &env) {
            Ok(made) => {
                if let Some(attestation) = &made.attestation {
                    println!(
                        "release-build: signed the receipt in {}",
                        attestation.display()
                    );
                }
                println!(
                    "release-build: wrote {} ({} bytes, sha256 {}) and {}, built in the {} lane; \
                     carry the folder to the Mac's --windows-artifacts directory, and give \
                     `ship release` --windows-sha256 {} as read here, not off the carried files",
                    made.zip.display(),
                    made.admitted.size,
                    made.admitted.sha256,
                    made.receipt.display(),
                    made.admitted.lane,
                    made.admitted.sha256
                );
                PASS
            }
            Err((code, why)) => {
                eprintln!("aterm-winship: {why}");
                code
            }
        };
    }
    if let Verb::ReleaseKey { init, key, builder } = &verb {
        let resolved = key
            .clone()
            .or_else(|| attest::default_key(&env))
            .ok_or_else(|| {
                "the environment has no USERPROFILE: name the key with --key <path>".to_string()
            })
            .and_then(|path| {
                let builder = match builder {
                    Some(builder) => builder.clone(),
                    None => attest::default_builder(&env)?,
                };
                Ok((path, builder))
            });
        return match resolved {
            Ok((path, builder)) => {
                release_key(*init, &path, &builder, pull::now_secs(), &mut |line| {
                    println!("{line}")
                })
            }
            Err(e) => {
                eprintln!("aterm-winship: not run: {e}");
                NOT_RUN
            }
        };
    }
    let layout = match Layout::from_env(&env) {
        Ok(layout) => layout,
        Err(e) => {
            eprintln!("aterm-winship: not run: {e}");
            return NOT_RUN;
        }
    };
    match verb {
        Verb::Update { opts, log } => {
            let mut out = if log {
                match Out::logged(&layout.log()) {
                    Ok(out) => out,
                    Err(e) => {
                        eprintln!("aterm-winship: not run: {e}");
                        return NOT_RUN;
                    }
                }
            } else {
                Out::terminal()
            };
            let code = update::update(&opts, &layout, &env, &mut out);
            out.say(&format!("update: exit {code}"));
            code
        }
        Verb::Schedule {
            at,
            repo,
            remove,
            dry_run,
        } => {
            let mut out = Out::terminal();
            if remove {
                unschedule(dry_run, &layout, &env, &mut out)
            } else {
                run_schedule(&at, repo.as_deref(), dry_run, &layout, &env, &mut out)
            }
        }
        Verb::Status { repo } => update::status(repo.as_deref(), &layout, &mut Out::terminal()),
        Verb::ReleaseServe { opts, log } => {
            let mut out = if log {
                serve::rotate(&layout.serve_log());
                match Out::logged(&layout.serve_log()) {
                    Ok(out) => out,
                    Err(e) => {
                        eprintln!("aterm-winship: not run: {e}");
                        return NOT_RUN;
                    }
                }
            } else {
                Out::terminal()
            };
            serve::serve(&opts, &layout, &env, &mut out)
        }
        Verb::Help | Verb::Smoke(_) | Verb::ReleaseBuild(_) | Verb::ReleaseKey { .. } => PASS,
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
