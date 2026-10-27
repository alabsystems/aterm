// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm acaffiene [<cmd>]`: the front door's verb.

use std::ffi::OsString;
use std::process::ExitCode;

use crate::launchd::Startup;
use crate::model::{Clocks, Mode};

/// The usage text.
pub const USAGE: &str = "\
aterm acaffiene — a menu-bar mug that keeps this Mac awake: a click fills it (no
idle sleep of the system or the display) until the next click; a right click or
Control-click picks 1, 8 or 24 hours or forever, Enable at Startup, or Quit.

USAGE:
    aterm acaffiene [start]
    aterm acaffiene on [1h|8h|24h|forever|<n>h|<n>m]
    aterm acaffiene off
    aterm acaffiene status
    aterm acaffiene startup [on|off]
    aterm acaffiene quit [own]
    aterm acaffiene serve
    aterm acaffiene bridge

    start      Put the mug in the menu bar as a launchd job of this login
               (acaffiene=started), or name the one there (acaffiene=running
               pid=<n>). The default.
    on [span]  Keep the Mac awake for the span (forever by default; at most a
               year) and put the mug in the menu bar. acaffiene=requested
               until=… says the request is saved, not that the mug took it.
    off        Stop keeping it awake; the mug stays, empty (acaffiene=off).
    status     acaffiene=on|off (the saved mode while a mug runs; a mug whose
               activity was refused saves off), until=<unix seconds>|forever|-,
               menu=running and pid=<n>, or menu=stopped, then the running
               mug's job (else this build's): startup=on|off|blocked|disabled
               and label=<launchd label>. It reads what was saved;
               `pmset -g assertions` shows whether the activity is held.
    startup    Say whether this build's mug starts at every login; `on` or `off`
               sets it (a plist in ~/Library/LaunchAgents; `off` also removes
               any whose build is gone) and leaves the running mug as it is.
               `blocked`: aterm's switch under Background App Activity (System
               Settings > General > Login Items & Extensions) is off, so no
               login starts it, and `on` exits 1 until that switch is on.
               `disabled`: launchd has the job disabled (`launchctl disable`);
               `on` lifts that.
    quit       Turn off and take the mug out of the menu bar (acaffiene=quit, or
               acaffiene=stopped when none ran). `own`: only this build's mug and
               job; another build's is left as it is, on or off.
    serve      Run the mug in this process (what its launchd job runs). It
               exits 0 even when it refuses, so launchd does not restart it,
               and, as that job, 75 when an update replaces its aterm, so
               launchd starts the new one.
    bridge     Keep the Mac awake while the mug restarts onto an aterm update,
               until the new mug takes over or keep-awake ends, at most 21 min
               (what a restarting mug runs).

Every ordinary aterm window launch starts the mug and keeps the Mac awake forever;
headless, diagnostic and private-control launches do not. Quit ends it until the
next aterm launch, which turns it back on. Enable at Startup separately starts it
at every login. Missing state, a reboot and an expired span return to forever.
Exit status: 1 when a command fails, 2 for a refused command line. macOS only.
";

/// One parsed `aterm acaffiene` command line.
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

/// Parse the words after `acaffiene`.
/// # Errors
/// What is wrong, as the part of the error line after `aterm acaffiene: `: an unknown command, any option (a word starting with `-`, help's aside), a span [`crate::model::parse_span`] refuses, a `startup` word other than `on` or `off`, or an extra operand.
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
    format!("aterm acaffiene: {why}; run `aterm acaffiene --help`")
}

/// What every command but help says off macOS.
pub const MACOS_ONLY: &str = "aterm acaffiene: macOS only";

/// When `mode` ends, as `status` and `on` print it at `now`: `forever`, the deadline in Unix seconds as the wall clock reads now ([`Clocks::wall_at`]), or `-` when off.
#[must_use]
pub fn until_word(mode: Mode, now: Clocks) -> String {
    match mode.settled(now) {
        Mode::Off => "-".to_string(),
        Mode::Forever => "forever".to_string(),
        Mode::Until { deadline, .. } => now.wall_at(deadline).to_string(),
    }
}

/// `on`'s asynchronous acknowledgment: the request is saved and a mug was found or its start requested, not proof that its activity has begun.
#[must_use]
pub fn on_line(mode: Mode, now: Clocks) -> String {
    format!("acaffiene=requested until={}\n", until_word(mode, now))
}

/// `status`'s answer, one `key=value` per line: `acaffiene=on|off`, `until=` ([`until_word`]), `menu=running` and `pid=<n>` or `menu=stopped`, `startup=on|off|blocked|disabled` ([`Startup::word`]), `label=<label>`.
#[must_use]
pub fn status_text(
    mode: Mode,
    now: Clocks,
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
        "acaffiene={}\nuntil={}\n{menu}startup={}\nlabel={label}\n",
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

/// The verb's entry: `args` are the words after `acaffiene`, `version` the app version the about row prints.
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
        return aterm_uds::stdout::answer_exit("aterm acaffiene", USAGE, 0);
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
        Command::Help => aterm_uds::stdout::answer_exit("aterm acaffiene", USAGE, 0),
        Command::Start => mac::start(),
        Command::On { span } => mac::on(span),
        Command::Off => mac::off(),
        Command::Status => mac::status(),
        Command::Startup(set) => mac::startup(set),
        Command::Quit { own } => mac::quit(own),
        Command::Serve => mac::serve(version),
        Command::Bridge => mac::bridge(),
    }
}

/// Start the mug for an ordinary aterm window launch, without writing to its standard output.
#[cfg(target_os = "macos")]
pub fn autostart() {
    mac::autostart();
}

/// The verbs, on macOS.
#[cfg(target_os = "macos")]
mod mac {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use crate::code;
    use crate::instance::{self, Instance};
    use crate::launchd::{self, JobState, Started};
    use crate::model::{Clocks, Mode};
    use crate::paths::Paths;
    use crate::state;

    /// The verb's name, as its answers and errors say it.
    const VERB: &str = "aterm acaffiene";

    /// The one stderr line of a failure, and exit 1.
    fn fail(why: impl std::fmt::Display) -> ExitCode {
        eprintln!("{VERB}: {why}");
        ExitCode::FAILURE
    }

    /// The answer on stdout, and exit 0.
    fn answer(text: &str) -> ExitCode {
        aterm_uds::stdout::answer_exit(VERB, text, 0)
    }

    /// Both clocks, read now ([`state::clocks`]).
    fn now() -> Result<Clocks, String> {
        state::clocks().ok_or_else(|| {
            "the clock reads before 1970, or the time since boot cannot be read".to_string()
        })
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
    pub(super) fn login_paths() -> Result<Paths, String> {
        let paths = Paths::from_env().ok_or("no home directory")?;
        let refusal = login_refusal(
            aterm_types::dirs::runs_under_private_state_root(),
            aterm_types::dirs::home_dir().as_deref(),
            aterm_uds::account::home().as_deref(),
        );
        refusal.map_or(Ok(paths), Err)
    }

    /// Why a process with a private state root, or whose `home` is not the login's `account`
    /// home, is not the login's ([`login_paths`]); `None` when it is.
    pub(super) fn login_refusal(
        private_root: bool,
        home: Option<&Path>,
        account: Option<&Path>,
    ) -> Option<String> {
        if private_root {
            return Some(
                "this process has a private state root (ATERM_STATE_HOME); the mug is the \
                 login's, and reads only the login's own files"
                    .to_string(),
            );
        }
        let (home, account) = (home?, account?);
        (!same_dir(home, account)).then(|| {
            format!(
                "HOME is {}, not this login's home {}; the mug is the login's, and reads only \
                 the login's own files",
                home.display(),
                account.display()
            )
        })
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
    fn here_now() -> Result<(Here, Clocks), String> {
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
            Ok(Running::Already(pid)) => answer(&format!("acaffiene=running pid={pid}\n")),
            Ok(Running::Started) => answer("acaffiene=started\n"),
            Err(why) => fail(why),
        }
    }

    pub(super) fn autostart() {
        let here = match here() {
            Ok(here) => here,
            Err(why) => {
                aterm_log::warn!("acaffiene: could not start with aterm: {why}");
                return;
            }
        };
        launchd::prune_orphans(&here.paths);
        if let Err(why) = set_mode(&here.paths, Mode::Forever) {
            aterm_log::warn!("acaffiene: could not keep aterm awake: {why}");
            return;
        }
        if let Err(why) = ensure(&here) {
            aterm_log::warn!("acaffiene: could not start with aterm: {why}");
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
            Ok(()) => answer("acaffiene=off\n"),
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
            Ok(()) if !ended.is_empty() => answer("acaffiene=quit\n"),
            Ok(()) => answer("acaffiene=stopped\n"),
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

    /// A bridge, refused as the mug is ([`login_paths`]): a mug that passed starts one in its
    /// own environment, and one root ran by hand would leave its pid file root's. A refusal
    /// exits 1, never 0, which the mug would read as a bridge that found nothing to hold.
    pub(super) fn bridge() -> ExitCode {
        if let Err(why) = login_paths() {
            eprintln!("{VERB}: bridge: {why}");
            return ExitCode::FAILURE;
        }
        crate::macos::bridge()
    }

    /// Write the job this mug is beside its pid in the lock ([`Instance::set_label`]); a lock left
    /// without it is read as the asking build's job.
    fn name_job(instance: &mut Instance, label: &str, lock: &Path) {
        if let Err(e) = instance.set_label(label) {
            eprintln!("{VERB}: serve: {}: {e}", lock.display());
        }
    }

    /// The mug's entry, refused where every verb that acts is ([`login_paths`]): root under
    /// sudo, which keeps the person's HOME, would otherwise make the lock and the copy theirs
    /// to open no more, and a scratch HOME would run a second mug beside the login's.
    pub(super) fn serve(version: &str) -> ExitCode {
        let paths = match login_paths() {
            Ok(paths) => paths,
            Err(why) => return refuse(None, why),
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

    /// The judgement every verb that acts makes first ([`mac::login_paths`]): a process is the
    /// login's only under the login's own home (one directory, however it is spelled) and no
    /// private state root; with no HOME there is nothing to judge. This test binary has a
    /// private root, so the live check refuses it.
    #[cfg(target_os = "macos")]
    #[test]
    fn only_the_logins_roots_run_the_mug() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let (login, other, link) = (
            dir.path().join("login"),
            dir.path().join("other"),
            dir.path().join("link"),
        );
        std::fs::create_dir(&login).expect("login");
        std::fs::create_dir(&other).expect("other");
        std::os::unix::fs::symlink(&login, &link).expect("link");
        assert_eq!(mac::login_refusal(false, Some(&login), Some(&login)), None);
        assert_eq!(
            mac::login_refusal(false, Some(&link), Some(&login)),
            None,
            "one directory, spelled twice"
        );
        let why = mac::login_refusal(false, Some(&other), Some(&login)).expect("another home");
        assert!(
            why.starts_with(&format!("HOME is {}", other.display())),
            "{why}"
        );
        let why = mac::login_refusal(true, Some(&login), Some(&login)).expect("a private root");
        assert!(why.contains("ATERM_STATE_HOME"), "{why}");
        assert_eq!(
            mac::login_refusal(false, None, Some(&login)),
            None,
            "no HOME to judge"
        );
        let why = mac::login_paths().expect_err("a test binary's private root");
        assert!(why.contains("ATERM_STATE_HOME"), "{why}");
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
        assert!(line.starts_with("aterm acaffiene: "), "{line}");
        assert!(line.contains("aterm acaffiene --help"), "{line}");
    }

    const NOW: Clocks = Clocks {
        since_boot: 300_000,
        wall: 1_790_000_000,
    };

    #[test]
    fn status_says_the_mode_the_mug_startup_and_label() {
        assert_eq!(
            status_text(Mode::Off, NOW, None, Startup::Off, "l.x"),
            "acaffiene=off\nuntil=-\nmenu=stopped\nstartup=off\nlabel=l.x\n"
        );
        assert_eq!(
            status_text(Mode::Forever, NOW, None, Startup::On, "l.x"),
            "acaffiene=off\nuntil=-\nmenu=stopped\nstartup=on\nlabel=l.x\n"
        );
        assert_eq!(
            status_text(Mode::Forever, NOW, Some(42), Startup::Blocked, "l.x"),
            "acaffiene=on\nuntil=forever\nmenu=running\npid=42\nstartup=blocked\nlabel=l.x\n"
        );
        let timed = Mode::activate(Some(HOUR), NOW);
        assert_eq!(
            status_text(timed, NOW, Some(7), Startup::Off, "l.x"),
            format!(
                "acaffiene=on\nuntil={}\nmenu=running\npid=7\nstartup=off\nlabel=l.x\n",
                NOW.wall + HOUR
            )
        );
        assert_eq!(
            status_text(timed, NOW + HOUR, Some(7), Startup::Off, "l.x"),
            "acaffiene=off\nuntil=-\nmenu=running\npid=7\nstartup=off\nlabel=l.x\n"
        );
    }

    /// Ten minutes into `on 1h`, the wall clock set back or forward a day: `status` says the
    /// span is on and ends 50 minutes from the clock as it reads now.
    #[test]
    fn status_says_the_end_as_the_wall_clock_reads_now() {
        let timed = Mode::activate(Some(HOUR), NOW);
        for wall in [NOW.wall - 24 * HOUR + 600, NOW.wall + 24 * HOUR + 600] {
            let later = Clocks {
                since_boot: NOW.since_boot + 600,
                wall,
            };
            assert_eq!(
                status_text(timed, later, Some(7), Startup::Off, "l.x"),
                format!(
                    "acaffiene=on\nuntil={}\nmenu=running\npid=7\nstartup=off\nlabel=l.x\n",
                    wall + HOUR - 600
                )
            );
        }
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
        assert_eq!(
            on_line(Mode::Forever, NOW),
            "acaffiene=requested until=forever\n"
        );
        assert_eq!(
            on_line(Mode::activate(Some(60), NOW), NOW),
            format!("acaffiene=requested until={}\n", NOW.wall + 60)
        );
        assert_eq!(startup_line(Startup::On), "startup=on\n");
        assert_eq!(startup_line(Startup::Off), "startup=off\n");
        assert_eq!(startup_line(Startup::Blocked), "startup=blocked\n");
        assert_eq!(startup_line(Startup::Disabled), "startup=disabled\n");
        assert_eq!(until_word(Mode::Off, NOW), "-");
    }
}
