// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm caffeine [<cmd>]`: the front door's verb.

use std::ffi::OsString;
use std::process::ExitCode;

use crate::launchd::Startup;
use crate::model::Mode;

/// The usage text.
pub const USAGE: &str = "\
aterm caffeine — a coffee mug in the menu bar that keeps this Mac awake: a click
fills it (no idle sleep of the system or the display) until the next click; a
right click (or a Control-click) picks 1, 8 or 24 hours or forever, starts it at
login, or quits.

USAGE:
    aterm caffeine [start]
    aterm caffeine on [1h|8h|24h|forever|<n>h|<n>m]
    aterm caffeine off
    aterm caffeine status
    aterm caffeine startup [on|off]
    aterm caffeine quit [own]
    aterm caffeine serve
    aterm caffeine bridge

    start      Put the mug in the menu bar (its own launchd job, in this
               login); nothing when it is there. The default.
    on [span]  Request the span (forever by default; at most a year), and put
               the mug in the menu bar. caffeine=requested acknowledges the
               request; status reports the saved mode while a mug runs.
    off        Stop keeping it awake (the mug stays, empty).
    status     caffeine=on|off (saved mode with a live mug; known activation
               refusals turn it off, not a fresh powerd assertion probe),
               until=<unix seconds>|forever|-, menu=running (with pid=) or
               stopped, and the running mug's job (else this build's):
               startup=on|off|blocked|disabled, label=<launchd label>. These
               fields do not confirm that a keep-awake activity is held.
    startup    Say whether this build's mug starts at every login; `on` or `off`
               sets it (a plist in ~/Library/LaunchAgents; `off` also removes
               that of a build that is gone). The running mug stays. `blocked`:
               the plist is there, but aterm's switch under Background App
               Activity (System Settings > General > Login Items & Extensions)
               is off, so no login starts it; `on` fails until it is on.
               `disabled`: launchd has the job disabled (`launchctl disable`),
               so no login starts it; `on` lifts that.
    quit       Turn off and take the mug out of the menu bar. `own`: only this
               build's mug and its job; another build's is left as it is, on or off.
    serve      Run the mug in this process (what its launchd job runs).
    bridge     Keep the Mac awake while the mug restarts onto an update of
               aterm, until the new mug does (what a restarting mug runs).

A restart of the Mac starts it off. macOS only.
";

/// One parsed `aterm caffeine` command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// `-h`, `--help` or `help`: the usage.
    Help,
    /// No words, or `start`: make sure the mug runs.
    Start,
    /// `on [span]`: keep awake for `span` seconds, `None` forever, and make sure the mug runs.
    On {
        /// The span, `None` for forever.
        span: Option<u64>,
    },
    /// `off`: stop keeping awake.
    Off,
    /// `status`: the mode, the mug, Enable at Startup and the label.
    Status,
    /// `startup [on|off]`: say (`None`) or set Enable at Startup.
    Startup(Option<bool>),
    /// `quit [own]`: turn off and end the mug, or with `own` only this build's.
    Quit {
        /// End only this build's mug and job, leaving another build's running.
        own: bool,
    },
    /// `serve`: run the mug here.
    Serve,
    /// `bridge`: keep the Mac awake while the mug restarts onto an update.
    Bridge,
}

/// Parse the words after `caffeine`.
/// # Errors
/// What is wrong, as the part of the error line after `aterm caffeine: `: an unknown command, any option (a word starting with `-`, help's aside), a span [`crate::model::parse_span`] refuses, a `startup` word other than `on` or `off`, or an extra operand.
pub fn parse(args: &[OsString]) -> Result<Command, String> {
    let words: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if words.iter().any(|w| w == "-h" || w == "--help")
        || words.first().is_some_and(|w| w == "help")
    {
        return Ok(Command::Help);
    }
    if let Some(option) = words.iter().find(|w| w.starts_with('-')) {
        return Err(format!("unknown option {option:?}"));
    }
    let mut it = words.iter().map(String::as_str);
    let first = it.next();
    let command = match first {
        None | Some("start") => Command::Start,
        Some("on") => Command::On {
            span: match it.next() {
                None => None,
                Some(word) => crate::model::parse_span(word)?,
            },
        },
        Some("off") => Command::Off,
        Some("status") => Command::Status,
        Some("startup") => Command::Startup(match it.next() {
            None => None,
            Some("on") => Some(true),
            Some("off") => Some(false),
            Some(word) => return Err(format!("startup takes on or off, not {word:?}")),
        }),
        Some("quit") => Command::Quit {
            own: match it.next() {
                None => false,
                Some("own") => true,
                Some(word) => return Err(format!("quit takes own, not {word:?}")),
            },
        },
        Some("serve") => Command::Serve,
        Some("bridge") => Command::Bridge,
        Some(other) => return Err(format!("unknown command {other:?}")),
    };
    if let Some(extra) = it.next() {
        return Err(format!(
            "unexpected argument {extra:?} after {:?}",
            first.unwrap_or_default()
        ));
    }
    Ok(command)
}

/// The error line for a command line [`parse`] refused.
#[must_use]
pub fn usage_error(why: &str) -> String {
    format!("aterm caffeine: {why}; run `aterm caffeine --help`")
}

/// What every command but help says off macOS.
pub const MACOS_ONLY: &str = "aterm caffeine: macOS only";

/// When `mode` ends, as `status` and `on` print it at `now`: `forever`, the deadline in Unix seconds, or `-` when off.
#[must_use]
pub fn until_word(mode: Mode, now: u64) -> String {
    match mode.settled(now) {
        Mode::Off => "-".to_string(),
        Mode::Forever => "forever".to_string(),
        Mode::Until { deadline, .. } => deadline.to_string(),
    }
}

/// `on`'s asynchronous acknowledgment: the request is saved and a mug was found or its start requested, not proof that its activity has begun.
#[must_use]
pub fn on_line(mode: Mode, now: u64) -> String {
    format!("caffeine=requested until={}\n", until_word(mode, now))
}

/// `status`'s answer, one `key=value` per line: `caffeine=on|off`, `until=` ([`until_word`]), `menu=running` and `pid=<n>` or `menu=stopped`, `startup=on|off|blocked|disabled` ([`Startup::word`]), `label=<label>`.
#[must_use]
pub fn status_text(
    mode: Mode,
    now: u64,
    menu_pid: Option<u32>,
    startup: Startup,
    label: &str,
) -> String {
    let on_off = |on: bool| if on { "on" } else { "off" };
    let menu = menu_pid.map_or_else(
        || "menu=stopped\n".to_string(),
        |pid| format!("menu=running\npid={pid}\n"),
    );
    let mode = if menu_pid.is_some() { mode } else { Mode::Off };
    format!(
        "caffeine={}\nuntil={}\n{menu}startup={}\nlabel={label}\n",
        on_off(mode.is_on(now)),
        until_word(mode, now),
        startup.word(),
    )
}

/// `startup`'s answer: `startup=on|off|blocked|disabled` ([`Startup::word`]).
#[must_use]
pub fn startup_line(startup: Startup) -> String {
    format!("startup={}\n", startup.word())
}

/// What `startup on` says when Login Items' switch blocks the plist it wrote.
pub const STARTUP_BLOCKED: &str = "Login Items has aterm's background activity off, so no \
login starts the mug: turn aterm on under Background App Activity in System Settings > \
General > Login Items & Extensions";

/// Whether the mug `quit own` finds is another build's, which it leaves running with its mode: the running `holder`'s label (none named yet counts as another's) is not `here`, or, with none running, the label the free lock still names (`named`: a mug between its runs, which launchd starts again) is another.
#[must_use]
pub fn another_builds_mug(
    holder: Option<&crate::instance::Holder>,
    named: Option<&str>,
    here: &str,
) -> bool {
    match holder {
        Some(holder) => holder.label.as_deref() != Some(here),
        None => named.is_some_and(|label| label != here),
    }
}

/// The verb's entry: `args` are the words after `caffeine`, `version` the app version the about row prints.
#[must_use]
pub fn main_entry(args: Vec<OsString>, version: &str) -> ExitCode {
    let command = match parse(&args) {
        Ok(command) => command,
        Err(why) => {
            eprintln!("{}", usage_error(&why));
            return ExitCode::from(2);
        }
    };
    if command == Command::Help {
        return aterm_uds::stdout::answer_exit("aterm caffeine", USAGE, 0);
    }
    run(command, version)
}

#[cfg(not(target_os = "macos"))]
fn run(_command: Command, _version: &str) -> ExitCode {
    eprintln!("{MACOS_ONLY}");
    ExitCode::FAILURE
}

#[cfg(target_os = "macos")]
fn run(command: Command, version: &str) -> ExitCode {
    match command {
        Command::Help => aterm_uds::stdout::answer_exit("aterm caffeine", USAGE, 0),
        Command::Start => mac::start(),
        Command::On { span } => mac::on(span),
        Command::Off => mac::off(),
        Command::Status => mac::status(),
        Command::Startup(set) => mac::startup(set),
        Command::Quit { own } => mac::quit(own),
        Command::Serve => mac::serve(version),
        Command::Bridge => crate::macos::bridge(),
    }
}

/// The verbs, on macOS.
#[cfg(target_os = "macos")]
mod mac {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use crate::code;
    use crate::instance::{self, Instance};
    use crate::launchd::{self, JobState, Started};
    use crate::model::Mode;
    use crate::paths::Paths;
    use crate::state;

    /// The verb's name, as its answers and errors say it.
    const VERB: &str = "aterm caffeine";

    /// The one stderr line of a failure, and exit 1.
    fn fail(why: impl std::fmt::Display) -> ExitCode {
        eprintln!("{VERB}: {why}");
        ExitCode::FAILURE
    }

    /// The answer on stdout, and exit 0.
    fn answer(text: &str) -> ExitCode {
        aterm_uds::stdout::answer_exit(VERB, text, 0)
    }

    /// The wall clock, in whole seconds.
    fn now() -> Result<u64, String> {
        aterm_time::unix_secs().ok_or_else(|| "the clock reads before 1970".to_string())
    }

    /// Where a verb acts: the mug's files, this executable (canonical), and its job's label.
    struct Here {
        paths: Paths,
        origin: PathBuf,
        label: String,
    }

    /// The mug's files, when this process has the login's own roots. The mug is a launchd job of
    /// the login, which reads the files under the login's home whatever `$HOME` this shell has: a
    /// verb run with a scratch `$HOME` or a private state root (`ATERM_STATE_HOME`, a test's) would
    /// start a real mug that never reads what it wrote, and report on files that mug never writes,
    /// so it is refused.
    fn login_paths() -> Result<Paths, String> {
        let paths = Paths::from_env().ok_or("no home directory")?;
        if aterm_types::dirs::runs_under_private_state_root() {
            return Err(
                "this process has a private state root (ATERM_STATE_HOME); the mug is the \
                 login's, and reads only the login's own files"
                    .to_string(),
            );
        }
        if let (Some(home), Some(account)) =
            (aterm_types::dirs::home_dir(), aterm_uds::account::home())
            && !same_dir(&home, &account)
        {
            return Err(format!(
                "HOME is {}, not this login's home {}; the mug is the login's, and reads only \
                 the login's own files",
                home.display(),
                account.display()
            ));
        }
        Ok(paths)
    }

    /// Whether `a` and `b` name one directory: resolved when both resolve, else as spelled.
    fn same_dir(a: &Path, b: &Path) -> bool {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        }
    }

    fn here() -> Result<Here, String> {
        let paths = login_paths()?;
        let origin = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|e| format!("this executable's path cannot be read: {e}"))?;
        let label = launchd::label_for(&origin);
        Ok(Here {
            paths,
            origin,
            label,
        })
    }

    /// [`here`] and [`now`].
    fn here_now() -> Result<(Here, u64), String> {
        Ok((here()?, now()?))
    }

    /// Whether a mug runs now, or how it was started.
    enum Running {
        /// One runs, with this pid.
        Already(u32),
        /// launchd was asked to start it.
        Started,
    }

    /// The mug holding the lock, when the process its pid names is one: a pid left behind by a mug
    /// killed outright, now another process's, is no mug ([`instance::running_mug`]).
    fn running_mug(paths: &Paths) -> Option<instance::Holder> {
        instance::running_mug(&paths.lock_file(), &paths.code_dir())
    }

    /// Make sure a mug runs: the one holding the lock, else this build's job, started.
    fn ensure(here: &Here) -> Result<Running, String> {
        if let Some(holder) = running_mug(&here.paths) {
            return Ok(Running::Already(holder.pid));
        }
        match launchd::ensure_running(&here.paths, &here.label, &here.origin)? {
            Started::AlreadyRunning { pid } => Ok(Running::Already(pid)),
            Started::Kicked | Started::Bootstrapped => Ok(Running::Started),
        }
    }

    pub(super) fn start() -> ExitCode {
        let here = match here() {
            Ok(here) => here,
            Err(why) => return fail(why),
        };
        launchd::prune_orphans(&here.paths);
        match ensure(&here) {
            Ok(Running::Already(pid)) => answer(&format!("caffeine=running pid={pid}\n")),
            Ok(Running::Started) => answer("caffeine=started\n"),
            Err(why) => fail(why),
        }
    }

    /// Save `mode` and tell a running mug.
    fn set_mode(paths: &Paths, mode: Mode) -> Result<(), String> {
        let file = paths.state_file();
        state::save(&file, mode).map_err(|e| format!("{}: {e}", file.display()))?;
        crate::notify::post_changed();
        Ok(())
    }

    pub(super) fn on(span: Option<u64>) -> ExitCode {
        let (here, now) = match here_now() {
            Ok(both) => both,
            Err(why) => return fail(why),
        };
        let mode = Mode::activate(span, now);
        if let Err(why) = set_mode(&here.paths, mode) {
            return fail(why);
        }
        match ensure(&here) {
            Ok(_) => answer(&super::on_line(mode, now)),
            Err(why) => fail(format!("request saved, but the mug did not start: {why}")),
        }
    }

    pub(super) fn off() -> ExitCode {
        let paths = match login_paths() {
            Ok(paths) => paths,
            Err(why) => return fail(why),
        };
        match set_mode(&paths, Mode::Off) {
            Ok(()) => answer("caffeine=off\n"),
            Err(why) => fail(why),
        }
    }

    pub(super) fn status() -> ExitCode {
        let (here, now) = match here_now() {
            Ok(both) => both,
            Err(why) => return fail(why),
        };
        let mode = state::load(&here.paths.state_file(), now);
        let holder = running_mug(&here.paths);
        let label = holder
            .as_ref()
            .and_then(|holder| holder.label.as_deref())
            .unwrap_or(&here.label);
        let text = super::status_text(
            mode,
            now,
            holder.as_ref().map(|holder| holder.pid),
            launchd::startup(&here.paths, label, &crate::macos::STARTUP_DOORS),
            label,
        );
        answer(&text)
    }

    /// Enable at Startup for this build's job: the plist it writes runs this executable.
    /// On also lifts launchd's disable of the job; one Login Items' switch blocks fails,
    /// naming that switch.
    pub(super) fn startup(set: Option<bool>) -> ExitCode {
        let here = match here() {
            Ok(here) => here,
            Err(why) => return fail(why),
        };
        if let Some(on) = set
            && let Err(e) = launchd::set_startup(&here.paths, &here.label, &here.origin, on)
        {
            return fail(format!(
                "{}: {e}",
                here.paths.startup_plist(&here.label).display()
            ));
        }
        let doors = crate::macos::STARTUP_DOORS;
        let mut startup = launchd::startup(&here.paths, &here.label, &doors);
        if set == Some(true) && startup == launchd::Startup::Disabled {
            if let Err(e) = (doors.enable)(&here.label) {
                return fail(e);
            }
            startup = launchd::startup(&here.paths, &here.label, &doors);
        }
        if set == Some(true) && startup == launchd::Startup::Blocked {
            eprintln!("{VERB}: {}", super::STARTUP_BLOCKED);
            return aterm_uds::stdout::answer_exit(VERB, &super::startup_line(startup), 1);
        }
        answer(&super::startup_line(startup))
    }

    /// Send `pid` SIGTERM (never pid 0 or 1, nor a negative group).
    fn terminate(pid: u32) -> Result<(), String> {
        let pid = libc::pid_t::try_from(pid)
            .ok()
            .filter(|pid| *pid > 1)
            .ok_or_else(|| format!("{pid} is not a mug's pid"))?;
        // SAFETY: two integers; `pid` is positive and not launchd's, so this signals one process.
        if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(format!("kill {pid}: {e}"))
        }
    }

    /// How long [`quit`] waits for the mug it ended to let go of the lock: launchd's SIGTERM
    /// ends a job's mug at once (its default action), and the one a hand-run mug gets from
    /// `quit` itself quits it through its run loop within moments, so this only bounds a hang.
    const QUIT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

    pub(super) fn quit(own: bool) -> ExitCode {
        let (here, now) = match here_now() {
            Ok(both) => both,
            Err(why) => return fail(why),
        };
        let lock = here.paths.lock_file();
        let holder = running_mug(&here.paths);
        let named = instance::last_holder(&lock).and_then(|named| named.label);
        let other =
            own && super::another_builds_mug(holder.as_ref(), named.as_deref(), &here.label);
        if other {
            eprintln!(
                "{VERB}: {}; it and its mode are left as they are",
                if holder.is_some() {
                    "another build's mug is running"
                } else {
                    "the lock names another build's mug, between its runs or ended"
                }
            );
        }
        let holder = holder.filter(|_| !other);
        let saved = if other || state::load(&here.paths.state_file(), now) == Mode::Off {
            Ok(())
        } else {
            set_mode(&here.paths, Mode::Off)
        };
        let mut labels: Vec<String> = holder
            .as_ref()
            .and_then(|holder| holder.label.clone())
            .or(named)
            .filter(|_| !own)
            .into_iter()
            .collect();
        if !labels.contains(&here.label) {
            labels.push(here.label.clone());
        }
        let mut ended: Vec<u32> = Vec::new();
        for label in &labels {
            match launchd::job_state(label) {
                Ok(JobState::Loaded { pid }) => {
                    if let Err(e) = launchd::bootout(label) {
                        return fail(e);
                    }
                    ended.extend(pid);
                }
                Ok(JobState::Absent) => {}
                Err(e) => return fail(format!("launchctl print {label}: {e}")),
            }
        }
        if let Some(pid) = holder
            .map(|holder| holder.pid)
            .filter(|pid| !ended.contains(pid))
        {
            if let Err(why) = terminate(pid) {
                return fail(why);
            }
            ended.push(pid);
        }
        if let Err(pid) = instance::await_released(&lock, &ended, QUIT_WAIT) {
            return fail(format!(
                "the mug (pid {pid}) still holds {} {} s after it was ended",
                lock.display(),
                QUIT_WAIT.as_secs()
            ));
        }
        if !other && here.paths.code_dir().exists() {
            discard_copy(&here.paths);
        }
        match saved {
            Err(why) => fail(why),
            Ok(()) if !ended.is_empty() => answer("caffeine=quit\n"),
            Ok(()) => answer("caffeine=stopped\n"),
        }
    }

    /// Remove the ended mug's copy of the app, as a mug that quits from its menu does: once an
    /// update retires the bundle it was cloned from, its blocks are its own.
    fn discard_copy(paths: &Paths) {
        let Ok(Some(held)) = Instance::try_hold(&paths.lock_file()) else {
            return;
        };
        if let Err(e) = code::discard_copies(&paths.code_dir()) {
            eprintln!("{VERB}: {}: {e}", paths.code_dir().display());
        }
        drop(held);
    }

    /// The one stderr line of a `serve` that will not run, and exit 0: launchd's KeepAlive starts the job again on any other status, and none of these refusals cures itself, so it would only refuse again every throttle interval (a transient one waits for the next start).
    pub(super) fn refuse(paths: Option<&Paths>, why: impl std::fmt::Display) -> ExitCode {
        if let Some(paths) = paths {
            crate::macos::release_bridge(paths);
        }
        eprintln!("{VERB}: serve: {why}");
        ExitCode::SUCCESS
    }

    /// Write the job this mug is beside its pid in the lock ([`Instance::set_label`]); a lock left
    /// without it is read as the asking build's job.
    fn name_job(instance: &mut Instance, label: &str, lock: &Path) {
        if let Err(e) = instance.set_label(label) {
            eprintln!("{VERB}: serve: {}: {e}", lock.display());
        }
    }

    pub(super) fn serve(version: &str) -> ExitCode {
        let Some(paths) = Paths::from_env() else {
            return refuse(None, "no home directory");
        };
        let code_dir = paths.code_dir();
        let lock = paths.lock_file();
        let (origin, instance) = if code::runs_from_copy(&code_dir) {
            code::mark_ran(&code_dir);
            let Some(origin) = code::origin_exe(&code_dir) else {
                return refuse(
                    Some(&paths),
                    format!(
                        "running from a copy in {}, but {} names no executable it was started as",
                        code_dir.display(),
                        code::ORIGIN_ENV
                    ),
                );
            };
            let mut instance = match Instance::adopt(&lock) {
                Some(instance) => instance,
                None => match hold(&paths) {
                    Some(instance) => instance,
                    None => return ExitCode::SUCCESS,
                },
            };
            name_job(&mut instance, &launchd::label_for(&origin), &lock);
            (origin, instance)
        } else {
            let origin = match std::env::current_exe().and_then(std::fs::canonicalize) {
                Ok(origin) => origin,
                Err(e) => {
                    return refuse(
                        Some(&paths),
                        format!("this executable's path cannot be read: {e}"),
                    );
                }
            };
            let Some(mut instance) = hold(&paths) else {
                return ExitCode::SUCCESS;
            };
            let label = launchd::label_for(&origin);
            name_job(&mut instance, &label, &lock);
            launchd::prune_orphans(&paths);
            match code::prepare_copy(&origin, &code_dir, &label) {
                Ok(Some(copy)) => {
                    let e = code::reexec_holding(&copy, &origin, &instance);
                    return refuse(
                        Some(&paths),
                        format!("re-running from {}: {e}; not starting", copy.display()),
                    );
                }
                Ok(None) => (origin, instance),
                Err(e) => {
                    return refuse(
                        Some(&paths),
                        format!(
                            "cannot make its own copy of {} in {}: {e}; not starting (running \
                         from the bundle would take every open of the app)",
                            origin.display(),
                            code_dir.display()
                        ),
                    );
                }
            }
        };
        aterm_log::env::unset(code::ORIGIN_ENV);
        aterm_log::env::unset(instance::LOCK_FD_ENV);
        let label = launchd::label_for(&origin);
        let restart_on_update =
            std::env::var_os("XPC_SERVICE_NAME").is_some_and(|name| name == label.as_str());
        eprintln!(
            "{VERB}: serving (pid {}, label {label}, started as {}, running {})",
            std::process::id(),
            origin.display(),
            std::env::current_exe().map_or_else(|e| e.to_string(), |p| p.display().to_string()),
        );
        crate::macos::serve(crate::macos::ServeContext {
            paths,
            label,
            origin,
            version: version.to_string(),
            restart_on_update,
            instance,
        })
    }

    /// How long [`hold`] waits out a lock held by something other than a running mug.
    const HOLD_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

    /// Take the one-mug lock; `None` (said on stderr) when another mug holds it, or it cannot be
    /// taken (a FIFO, a directory or a link at its name: [`refuse`]'s case).
    fn hold(paths: &Paths) -> Option<Instance> {
        let lock = paths.lock_file();
        let code_dir = paths.code_dir();
        let start = std::time::Instant::now();
        let mut taken = Instance::try_hold(&lock);
        while matches!(taken, Ok(None))
            && instance::running_mug(&lock, &code_dir).is_none()
            && start.elapsed() < HOLD_PATIENCE
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
            taken = Instance::try_hold(&lock);
        }
        match taken {
            Ok(Some(instance)) => Some(instance),
            Ok(None) => {
                let pid = instance::running_pid(&lock)
                    .map_or_else(String::new, |pid| format!(" (pid {pid})"));
                eprintln!("{VERB}: serve: already running{pid}");
                None
            }
            Err(e) => {
                let _ = refuse(
                    Some(paths),
                    format!("{}: {e}; not starting", lock.display()),
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HOUR;

    fn words(line: &str) -> Vec<OsString> {
        line.split_whitespace().map(OsString::from).collect()
    }

    fn parsed(line: &str) -> Result<Command, String> {
        parse(&words(line))
    }

    /// A `serve` that will not run lets a bridge an update restart left go, and exits 0; one
    /// that cannot find the mug's files touches none.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_serve_refusal_lets_a_left_bridge_go() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let paths = crate::paths::Paths {
            state_dir: dir.path().join("state"),
            home: dir.path().join("home"),
            logs_dir: dir.path().join("logs"),
        };
        std::fs::create_dir_all(&paths.state_dir).expect("state dir");
        std::fs::write(paths.bridge_file(), "4242\n").expect("bridge file");
        assert_eq!(mac::refuse(None, "a test refusal"), ExitCode::SUCCESS);
        assert!(paths.bridge_file().exists());
        assert_eq!(
            mac::refuse(Some(&paths), "a test refusal"),
            ExitCode::SUCCESS
        );
        assert!(!paths.bridge_file().exists(), "the bridge was left holding");
    }

    #[test]
    fn every_command_parses() {
        assert_eq!(parsed(""), Ok(Command::Start));
        assert_eq!(parsed("start"), Ok(Command::Start));
        assert_eq!(parsed("on"), Ok(Command::On { span: None }));
        assert_eq!(parsed("off"), Ok(Command::Off));
        assert_eq!(parsed("status"), Ok(Command::Status));
        assert_eq!(parsed("startup"), Ok(Command::Startup(None)));
        assert_eq!(parsed("startup on"), Ok(Command::Startup(Some(true))));
        assert_eq!(parsed("startup off"), Ok(Command::Startup(Some(false))));
        assert_eq!(parsed("quit"), Ok(Command::Quit { own: false }));
        assert_eq!(parsed("quit own"), Ok(Command::Quit { own: true }));
        assert!(parsed("quit all").is_err());
        assert_eq!(parsed("serve"), Ok(Command::Serve));
        assert_eq!(parsed("bridge"), Ok(Command::Bridge));
        assert!(parsed("bridge now").is_err());
    }

    /// `on` hands its word to `crate::model::parse_span`, whose own tests hold the table.
    #[test]
    fn on_takes_a_span_word() {
        assert_eq!(
            parsed("on 8h"),
            Ok(Command::On {
                span: Some(8 * HOUR)
            })
        );
        let why = parsed("on 1x").expect_err("not a span");
        assert!(why.contains("\"1x\""), "{why}");
    }

    #[test]
    fn help_is_asked_for_by_word_or_flag() {
        for line in [
            "-h",
            "--help",
            "help",
            "on --help",
            "status -h",
            "bogus --help",
        ] {
            assert_eq!(parsed(line), Ok(Command::Help), "{line:?}");
        }
        assert!(parsed("startup help").is_err());
    }

    #[test]
    fn any_option_is_refused() {
        for line in [
            "--aterm-front-door-routing-probe",
            "-x",
            "on -1h",
            "status --json",
            "serve --foreground",
            "-",
            "--",
        ] {
            let why = parsed(line).expect_err(line);
            assert!(why.starts_with("unknown option "), "{line:?}: {why}");
        }
    }

    #[test]
    fn unknown_words_and_extra_operands_are_refused() {
        for line in ["bogus", "ON"] {
            let why = parsed(line).expect_err(line);
            assert!(why.starts_with("unknown command "), "{line:?}: {why}");
        }
        for line in [
            "off now",
            "start now",
            "on 1h 2h",
            "status x",
            "startup on off",
            "quit own x",
            "serve x",
        ] {
            let why = parsed(line).expect_err(line);
            assert!(why.starts_with("unexpected argument "), "{line:?}: {why}");
        }
        let why = parsed("startup maybe").expect_err("maybe");
        assert!(why.starts_with("startup takes "), "{why}");
        let why = parsed("quit x").expect_err("x");
        assert!(why.starts_with("quit takes own"), "{why}");
    }

    #[cfg(unix)]
    #[test]
    fn a_word_that_is_not_utf8_is_unknown() {
        use std::os::unix::ffi::OsStringExt as _;
        let word = OsString::from_vec(vec![b'o', 0xff]);
        let why = parse(&[word]).expect_err("not a command");
        assert!(why.starts_with("unknown command "), "{why}");
    }

    /// A refused command line ends `main_entry` with 2 before any platform work: the front
    /// door's routing probe passes an option no verb knows.
    #[test]
    fn a_refused_command_line_exits_2_from_the_entry() {
        for line in [
            "--aterm-front-door-routing-probe",
            "bogus",
            "off now",
            "on 0h",
            "startup maybe",
            "serve --foreground",
        ] {
            assert_eq!(
                main_entry(words(line), "0.0.0"),
                ExitCode::from(2),
                "{line:?}"
            );
        }
    }

    /// A word from the command line reaches the error line escaped: one line, no escape
    /// sequence a terminal would act on.
    #[test]
    fn the_error_line_is_one_line_naming_help() {
        let word = OsString::from("bo\ngus\u{1b}[2J");
        let line = usage_error(&parse(&[word]).expect_err("bad"));
        assert!(!line.contains('\n'), "{line}");
        assert!(!line.contains('\u{1b}'), "{line}");
        assert!(line.starts_with("aterm caffeine: "), "{line}");
        assert!(line.contains("aterm caffeine --help"), "{line}");
    }

    #[test]
    fn status_says_the_mode_the_mug_startup_and_label() {
        const NOW: u64 = 1_790_000_000;
        assert_eq!(
            status_text(Mode::Off, NOW, None, Startup::Off, "l.x"),
            "caffeine=off\nuntil=-\nmenu=stopped\nstartup=off\nlabel=l.x\n"
        );
        assert_eq!(
            status_text(Mode::Forever, NOW, None, Startup::On, "l.x"),
            "caffeine=off\nuntil=-\nmenu=stopped\nstartup=on\nlabel=l.x\n"
        );
        assert_eq!(
            status_text(Mode::Forever, NOW, Some(42), Startup::Blocked, "l.x"),
            "caffeine=on\nuntil=forever\nmenu=running\npid=42\nstartup=blocked\nlabel=l.x\n"
        );
        let timed = Mode::activate(Some(HOUR), NOW);
        assert_eq!(
            status_text(timed, NOW, Some(7), Startup::Off, "l.x"),
            format!(
                "caffeine=on\nuntil={}\nmenu=running\npid=7\nstartup=off\nlabel=l.x\n",
                NOW + HOUR
            )
        );
        assert_eq!(
            status_text(timed, NOW + HOUR, Some(7), Startup::Off, "l.x"),
            "caffeine=off\nuntil=-\nmenu=running\npid=7\nstartup=off\nlabel=l.x\n"
        );
    }

    #[test]
    fn quit_own_leaves_another_builds_mug_running_or_between_runs() {
        let running = |label: Option<&str>| crate::instance::Holder {
            pid: 7,
            label: label.map(str::to_owned),
        };
        assert!(!another_builds_mug(
            Some(&running(Some("l.here"))),
            None,
            "l.here"
        ));
        assert!(another_builds_mug(
            Some(&running(Some("l.old"))),
            None,
            "l.here"
        ));
        assert!(another_builds_mug(Some(&running(None)), None, "l.here"));
        assert!(!another_builds_mug(
            Some(&running(Some("l.here"))),
            Some("l.old"),
            "l.here"
        ));
        assert!(another_builds_mug(None, Some("l.old"), "l.here"));
        assert!(!another_builds_mug(None, Some("l.here"), "l.here"));
        assert!(!another_builds_mug(None, None, "l.here"));
    }

    #[test]
    fn on_and_startup_answer_in_one_line() {
        const NOW: u64 = 1_790_000_000;
        assert_eq!(
            on_line(Mode::Forever, NOW),
            "caffeine=requested until=forever\n"
        );
        assert_eq!(
            on_line(Mode::activate(Some(60), NOW), NOW),
            format!("caffeine=requested until={}\n", NOW + 60)
        );
        assert_eq!(startup_line(Startup::On), "startup=on\n");
        assert_eq!(startup_line(Startup::Off), "startup=off\n");
        assert_eq!(startup_line(Startup::Blocked), "startup=blocked\n");
        assert_eq!(startup_line(Startup::Disabled), "startup=disabled\n");
        assert_eq!(until_word(Mode::Off, NOW), "-");
    }
}
