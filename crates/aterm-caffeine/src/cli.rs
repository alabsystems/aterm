// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm caffeine [<cmd>]`: the front door's verb.
//!
//! The grammar ([`parse`]) and the text the verbs print ([`USAGE`], [`status_text`],
//! [`until_word`]) are pure. A word the grammar does not know, any option, or an extra operand
//! is one line on stderr and exit 2, before anything else runs, at a terminal and through a
//! pipe alike; off macOS every command but help says so and exits 1.
//!
//! `start`, `on` and `off` return at once: the mug is its own launchd job
//! ([`crate::launchd`]), which they bootstrap or kickstart. `serve` is what that job runs: it
//! takes the one-mug lock ([`crate::instance`]), re-executes from its bundle copy
//! ([`crate::code`]) handing the lock over, and runs the menu-bar app.

use std::ffi::OsString;
use std::process::ExitCode;

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
    aterm caffeine quit
    aterm caffeine serve

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
               startup=on|off, label=<launchd label>. These fields do not
               confirm that a keep-awake activity is held.
    startup    Say whether this build's mug starts at every login; `on` or `off`
               sets it (a plist in ~/Library/LaunchAgents; `off` also removes
               that of a build that is gone). The running mug stays.
    quit       Turn off and take the mug out of the menu bar.
    serve      Run the mug in this process (what its launchd job runs).

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
    /// `quit`: turn off and end the mug.
    Quit,
    /// `serve`: run the mug here.
    Serve,
}

/// Parse the words after `caffeine`.
///
/// # Errors
///
/// What is wrong, as the part of the error line after `aterm caffeine: `: an unknown command,
/// any option (a word starting with `-`, help's aside), a span [`crate::model::parse_span`]
/// refuses, a `startup` word other than `on` or `off`, or an extra operand.
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
        Some("quit") => Command::Quit,
        Some("serve") => Command::Serve,
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

/// When `mode` ends, as `status` and `on` print it at `now`: `forever`, the deadline in Unix
/// seconds, or `-` when off.
#[must_use]
pub fn until_word(mode: Mode, now: u64) -> String {
    match mode.settled(now) {
        Mode::Off => "-".to_string(),
        Mode::Forever => "forever".to_string(),
        Mode::Until { deadline, .. } => deadline.to_string(),
    }
}

/// `on`'s asynchronous acknowledgment: the request is saved and a mug was
/// found or its start requested, not proof that its activity has begun.
#[must_use]
pub fn on_line(mode: Mode, now: u64) -> String {
    format!("caffeine=requested until={}\n", until_word(mode, now))
}

/// `status`'s answer, one `key=value` per line: `caffeine=on|off`, `until=`
/// ([`until_word`]), `menu=running` and `pid=<n>` or `menu=stopped`, `startup=on|off`,
/// `label=<label>`. `caffeine=` and `until=` report the saved mode while a mug
/// runs (`menu_pid`), off while none does. A known begin refusal publishes Off;
/// this is saved request state, not an activity acknowledgment or powerd probe.
#[must_use]
pub fn status_text(
    mode: Mode,
    now: u64,
    menu_pid: Option<u32>,
    startup: bool,
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
        on_off(startup),
    )
}

/// `startup`'s answer: `startup=on|off`.
#[must_use]
pub fn startup_line(on: bool) -> String {
    format!("startup={}\n", if on { "on" } else { "off" })
}

/// The verb's entry: `args` are the words after `caffeine`, `version` the app version the
/// about row prints.
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
        Command::Quit => mac::quit(),
        Command::Serve => mac::serve(version),
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

    fn here() -> Result<Here, String> {
        let paths = Paths::from_env().ok_or("no home directory")?;
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

    /// The mug holding the lock, when the process its pid names is one: a pid left behind by
    /// a mug killed outright, now another process's, is no mug ([`instance::running_mug`]).
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
        // A login plist of a build that is gone, before anything is started.
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
        let Some(paths) = Paths::from_env() else {
            return fail("no home directory");
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
        // The running mug's job (the label it wrote beside its pid), whichever build asks;
        // this build's when none runs.
        let label = holder
            .as_ref()
            .and_then(|holder| holder.label.as_deref())
            .unwrap_or(&here.label);
        let text = super::status_text(
            mode,
            now,
            holder.as_ref().map(|holder| holder.pid),
            launchd::startup_enabled(&here.paths, label),
            label,
        );
        answer(&text)
    }

    /// Enable at Startup for this build's job: the plist it writes runs this executable. (The
    /// running mug's own menu row sets it for that mug's job.)
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
        answer(&super::startup_line(launchd::startup_enabled(
            &here.paths,
            &here.label,
        )))
    }

    /// Send `pid` SIGTERM (never pid 0 or 1, nor a negative group).
    fn terminate(pid: u32) -> Result<(), String> {
        let pid = libc::pid_t::try_from(pid)
            .ok()
            .filter(|pid| *pid > 1)
            .ok_or_else(|| format!("{pid} is not a mug's pid"))?;
        // SAFETY: two integers; `pid` is positive and not launchd's, so this signals one process.
        if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
            Ok(())
        } else {
            Err(format!("kill {pid}: {}", std::io::Error::last_os_error()))
        }
    }

    /// How long [`quit`] waits for the mug it ended to let go of the lock: it keeps no SIGTERM
    /// handler, so it ends at once, and this only bounds a hang.
    const QUIT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

    pub(super) fn quit() -> ExitCode {
        let here = match here() {
            Ok(here) => here,
            Err(why) => return fail(why),
        };
        // Off first, so a mug that comes back anyway comes back empty.
        let saved = set_mode(&here.paths, Mode::Off);
        let holder = running_mug(&here.paths);
        // A job's mug is ended by launchd (`bootout`): its KeepAlive restarts one that dies
        // of a signal. The job is the running mug's (the label it wrote beside its pid),
        // whichever build runs `quit`; this build's when no mug names one. Any other holder
        // (one run by hand) gets the SIGTERM itself.
        let label = holder
            .as_ref()
            .and_then(|holder| holder.label.clone())
            .unwrap_or_else(|| here.label.clone());
        let job_pid = match launchd::job_state(&label) {
            Ok(JobState::Loaded { pid: Some(pid) }) => Some(pid),
            _ => None,
        };
        let mut ended = Vec::new();
        if let Some(pid) = job_pid {
            if let Err(e) = launchd::bootout(&label) {
                return fail(e);
            }
            ended.push(pid);
        }
        if let Some(pid) = holder
            .map(|holder| holder.pid)
            .filter(|pid| Some(*pid) != job_pid)
        {
            if let Err(why) = terminate(pid) {
                return fail(why);
            }
            ended.push(pid);
        }
        // `caffeine=quit` says the mug has ended, not that it was told to: the self-uninstall
        // sweep takes the lock next, and a mug still leaving would keep its files.
        if let Err(pid) = instance::await_released(&here.paths.lock_file(), &ended, QUIT_WAIT) {
            return fail(format!(
                "the mug (pid {pid}) still holds {} {} s after it was ended",
                here.paths.lock_file().display(),
                QUIT_WAIT.as_secs()
            ));
        }
        match saved {
            Err(why) => fail(why),
            Ok(()) if !ended.is_empty() => answer("caffeine=quit\n"),
            Ok(()) => answer("caffeine=stopped\n"),
        }
    }

    /// The one stderr line of a `serve` that will not run, and exit 0: launchd's KeepAlive
    /// starts the job again on any other status, and none of these refusals cures itself, so
    /// it would only refuse again every throttle interval (a transient one waits for the next
    /// start).
    fn refuse(why: impl std::fmt::Display) -> ExitCode {
        eprintln!("{VERB}: serve: {why}");
        ExitCode::SUCCESS
    }

    /// Write the job this mug is beside its pid in the lock ([`Instance::set_label`]); a lock
    /// left without it is read as the asking build's job.
    fn name_job(instance: &mut Instance, label: &str, lock: &Path) {
        if let Err(e) = instance.set_label(label) {
            eprintln!("{VERB}: serve: {}: {e}", lock.display());
        }
    }

    pub(super) fn serve(version: &str) -> ExitCode {
        let Some(paths) = Paths::from_env() else {
            return refuse("no home directory");
        };
        let code_dir = paths.code_dir();
        let lock = paths.lock_file();
        let (origin, instance) = if code::runs_from_copy(&code_dir) {
            let Some(origin) = code::origin_exe(&code_dir) else {
                return refuse(format!(
                    "running from a copy in {}, but {} names no executable it was started as",
                    code_dir.display(),
                    code::ORIGIN_ENV
                ));
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
                Err(e) => return refuse(format!("this executable's path cannot be read: {e}")),
            };
            let Some(mut instance) = hold(&paths) else {
                return ExitCode::SUCCESS;
            };
            let label = launchd::label_for(&origin);
            name_job(&mut instance, &label, &lock);
            // Once per mug, by the lock's holder: a login plist of a build that is gone.
            launchd::prune_orphans(&paths);
            match code::prepare_copy(&origin, &code_dir, &label) {
                // Returns only when the exec failed: never fall back to running AppKit from
                // the bundle, which would take every open of the app.
                Ok(Some(copy)) => {
                    let e = code::reexec_holding(&copy, &origin, &instance);
                    return refuse(format!(
                        "re-running from {}: {e}; not starting",
                        copy.display()
                    ));
                }
                Ok(None) => (origin, instance),
                Err(e) => {
                    return refuse(format!(
                        "cannot make its own copy of {} in {}: {e}; not starting (running \
                         from the bundle would take every open of the app)",
                        origin.display(),
                        code_dir.display()
                    ));
                }
            }
        };
        // Nothing the mug starts inherits the handover's variables.
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

    /// Take the one-mug lock; `None` (said on stderr) when another mug holds it, or it cannot
    /// be taken (a FIFO, a directory or a link at its name: [`refuse`]'s case).
    ///
    /// A refusal while no running mug holds it is tried once more: `status`, `start`, `on` and
    /// `quit` read the lock with `ownerlock::probe`, which holds a free lock for an instant,
    /// and a `serve` that lands in that instant must not leave as if a mug ran (it exits 0, so
    /// launchd would not start it again). A mug that has the lock but has not yet written its
    /// pid refuses the second try too.
    fn hold(paths: &Paths) -> Option<Instance> {
        let lock = paths.lock_file();
        let mut taken = Instance::try_hold(&lock);
        if matches!(taken, Ok(None)) && instance::running_mug(&lock, &paths.code_dir()).is_none() {
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
                let _ = refuse(format!("{}: {e}; not starting", lock.display()));
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
        assert_eq!(parsed("quit"), Ok(Command::Quit));
        assert_eq!(parsed("serve"), Ok(Command::Serve));
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
        // `help` is a command only in the first place.
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
        // Commands are lower case.
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
            "quit x",
            "serve x",
        ] {
            let why = parsed(line).expect_err(line);
            assert!(why.starts_with("unexpected argument "), "{line:?}: {why}");
        }
        let why = parsed("startup maybe").expect_err("maybe");
        assert!(why.starts_with("startup takes "), "{why}");
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
            status_text(Mode::Off, NOW, None, false, "l.x"),
            "caffeine=off\nuntil=-\nmenu=stopped\nstartup=off\nlabel=l.x\n"
        );
        // A mode saved while no mug runs (a logout's SIGTERM saves nothing) holds nothing.
        assert_eq!(
            status_text(Mode::Forever, NOW, None, true, "l.x"),
            "caffeine=off\nuntil=-\nmenu=stopped\nstartup=on\nlabel=l.x\n"
        );
        assert_eq!(
            status_text(Mode::Forever, NOW, Some(42), true, "l.x"),
            "caffeine=on\nuntil=forever\nmenu=running\npid=42\nstartup=on\nlabel=l.x\n"
        );
        let timed = Mode::activate(Some(HOUR), NOW);
        assert_eq!(
            status_text(timed, NOW, Some(7), false, "l.x"),
            format!(
                "caffeine=on\nuntil={}\nmenu=running\npid=7\nstartup=off\nlabel=l.x\n",
                NOW + HOUR
            )
        );
        // A deadline that has passed is off.
        assert_eq!(
            status_text(timed, NOW + HOUR, Some(7), false, "l.x"),
            "caffeine=off\nuntil=-\nmenu=running\npid=7\nstartup=off\nlabel=l.x\n"
        );
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
        assert_eq!(startup_line(true), "startup=on\n");
        assert_eq!(startup_line(false), "startup=off\n");
        assert_eq!(until_word(Mode::Off, NOW), "-");
    }
}
