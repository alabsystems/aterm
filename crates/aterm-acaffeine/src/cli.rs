// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm acaffeine [<cmd>]`: the front door's verb.

use std::ffi::OsString;
use std::process::ExitCode;

use crate::launchd::Startup;
use crate::model::{Clocks, Mode};

/// The usage text.
pub const USAGE: &str = "\
aterm acaffeine — a menu-bar mug that keeps this Mac awake: a click fills it (no
idle sleep of the system or the display) until the next click; a right click or
Control-click picks 1, 8 or 24 hours or forever, Keep Awake with Lid Closed,
Enable at Startup, or Quit. With the lid closed it holds nothing (the mode stays
on and holds again when the lid opens) unless you turn lid on.

USAGE:
    aterm acaffeine [start]
    aterm acaffeine on [1h|8h|24h|forever|<n>h|<n>m]
    aterm acaffeine off
    aterm acaffeine status
    aterm acaffeine lid [on|off]
    aterm acaffeine startup [on|off]
    aterm acaffeine quit [own]
    aterm acaffeine serve
    aterm acaffeine bridge

    start      Put the mug in the menu bar as a launchd job of this login
               (acaffeine=started), or name the one there (acaffeine=running
               pid=<n>). The default.
    on [span]  Keep the Mac awake for the span (forever by default; at most a
               year), then let it sleep, and put the mug in the menu bar.
               acaffeine=requested until=… says the request is saved, not that
               the mug took it.
    off        Stop keeping it awake; the mug stays, empty (acaffeine=off).
    status     acaffeine=on|off (the saved mode while a mug runs),
               until=<unix seconds>|forever|-, menu=running and pid=<n>, or
               menu=stopped, then the running mug's job (else this build's):
               startup=on|off|blocked|disabled and label=<launchd label>. It
               reads what was saved; `aterm doctor`'s awake: row reads whether
               macOS holds the mug's assertion. lid=on|off is Keep Awake with
               Lid Closed.
    lid        Say whether the mug keeps this Mac running with its lid closed
               (lid=on|off; off by default); `on` or `off` sets it, for the
               running mug and every later one. Off: closing the lid pauses the
               mug, which lets go of its assertions until the lid opens. On: it
               also holds PreventSystemSleep, which macOS honours on AC power
               only, so on battery a closed lid still sleeps the Mac. Either
               way the system's own sleep or a dark wake pauses it.
    startup    Say whether this build's mug starts at every login; `on` or `off`
               sets it (a plist in ~/Library/LaunchAgents; `off` also removes
               any whose build is gone) and leaves the running mug as it is.
               An `off` is kept: no aterm launch turns it back on.
               `blocked`: aterm's switch under Background App Activity (System
               Settings > General > Login Items & Extensions) is off, so no
               login starts it, and `on` exits 1 until that switch is on.
               `disabled`: launchd has the job disabled (`launchctl disable`);
               `on` lifts that.
    quit       Turn off and take the mug out of the menu bar (acaffeine=quit, or
               acaffeine=stopped when none ran). `own`: only this build's mug and
               job; another build's is left as it is, on or off.
    serve      Run the mug in this process (what its launchd job runs). It
               exits 0 when it does not run here (another mug runs, or this is
               not the login's process) and when it cannot keep the Mac awake
               for a reason no start again cures (a refused menu bar, a lock
               path that is a directory), which it records for `aterm doctor`;
               69 when the reason can clear up (a full disk, a lock another
               process holds a moment), so launchd starts it again about a
               minute later, from the copy of the app it ran from; and, as that
               job, 75 when an update replaces its aterm, so launchd starts the
               new one, or when a mode set on while it quits overtakes the
               Quit, so launchd starts it again.
    bridge     Keep the Mac awake while the mug restarts onto an aterm update,
               until the new mug takes over or keep-awake ends, at most 21 min
               (what a restarting mug runs).

Unless you turned it off, every ordinary aterm window (at launch and every five
minutes) and the vault subscriber's converge pass make sure the mug runs and turn
the installed app's Enable at Startup on (in /Applications or ~/Applications), so
a restart keeps the Mac awake with no window open; headless, diagnostic and
private-control launches do nothing. An off you choose (a click, off, quit, Quit,
or the end of a span) lasts, through a restart too, until you turn it on again;
so does turning Enable at Startup off, after which the subscriber's pass leaves
the mug to a window. A quit from outside the menu (a SIGTERM, Activity Monitor)
stops the mug and keeps its mode: the next window, pass or login starts it again.
With no saved state it keeps the Mac awake forever. Exit status: 1 when a command
fails, 2 for a refused command line. macOS only.
";

/// One parsed `aterm acaffeine` command line.
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
    /// `lid [on|off]`: say (`None`) or set Keep Awake with Lid Closed.
    Lid(Option<bool>),
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

/// Parse the words after `acaffeine`.
/// # Errors
/// What is wrong, as the part of the error line after `aterm acaffeine: `: an unknown command, any option (a word starting with `-`, help's aside), a span [`crate::model::parse_span`] refuses, a `lid` or `startup` word other than `on` or `off`, or an extra operand.
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
        Some("lid") => Command::Lid(match it.next() {
            None => None,
            Some("on") => Some(true),
            Some("off") => Some(false),
            Some(word) => return Err(format!("lid takes on or off, not {word:?}")),
        }),
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
    format!("aterm acaffeine: {why}; run `aterm acaffeine --help`")
}

/// What every command but help says off macOS.
pub const MACOS_ONLY: &str = "aterm acaffeine: macOS only";

/// When the span of `mode` ends, as `status` and `on` print it at `now`: `forever`, the deadline in Unix seconds as the wall clock reads now ([`Clocks::wall_at`]), or `-` when off.
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
    format!("acaffeine=requested until={}\n", until_word(mode, now))
}

/// `status`'s answer, one `key=value` per line: `acaffeine=on|off`, `until=` ([`until_word`]), `menu=running` and `pid=<n>` or `menu=stopped`, `startup=on|off|blocked|disabled` ([`Startup::word`]), `label=<label>`, `lid=on|off` ([`lid_line`]).
#[must_use]
pub fn status_text(
    mode: Mode,
    now: Clocks,
    menu_pid: Option<u32>,
    startup: Startup,
    label: &str,
    lid_awake: bool,
) -> String {
    let on_off = |on: bool| if on { "on" } else { "off" };
    let menu = menu_pid.map_or_else(
        || "menu=stopped\n".to_string(),
        |pid| format!("menu=running\npid={pid}\n"),
    );
    let mode = if menu_pid.is_some() { mode } else { Mode::Off };
    format!(
        "acaffeine={}\nuntil={}\n{menu}startup={}\nlabel={label}\n{}",
        on_off(mode.is_on(now)),
        until_word(mode, now),
        startup.word(),
        lid_line(lid_awake),
    )
}

/// `lid`'s answer: `lid=on|off`, Keep Awake with Lid Closed.
#[must_use]
pub fn lid_line(lid_awake: bool) -> String {
    format!("lid={}\n", if lid_awake { "on" } else { "off" })
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

/// The verb's entry: `args` are the words after `acaffeine`, `version` the app version the about row prints.
#[must_use = "handle or propagate the command's exit status"]
pub fn main_entry(args: Vec<OsString>, version: &str) -> ExitCode {
    let command = match parse(&args) {
        Ok(command) => command,
        Err(why) => {
            eprintln!("{}", usage_error(&why));
            return ExitCode::from(2);
        }
    };
    if command == Command::Help {
        return aterm_uds::stdout::answer_exit("aterm acaffeine", USAGE, 0);
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
        Command::Help => aterm_uds::stdout::answer_exit("aterm acaffeine", USAGE, 0),
        Command::Start => mac::start(),
        Command::On { span } => mac::on(span),
        Command::Off => mac::off(),
        Command::Status => mac::status(),
        Command::Lid(set) => mac::lid(set),
        Command::Startup(set) => mac::startup(set),
        Command::Quit { own } => mac::quit(own),
        Command::Serve => mac::serve(version),
        Command::Bridge => mac::bridge(),
    }
}

/// What keeping this Mac awake found or did ([`keep_awake`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kept {
    /// This process is not the login's (a private state root, another home, not macOS): why.
    /// Nothing was read or started.
    NotHere(String),
    /// The person turned keep-awake off: nothing was started.
    Off,
    /// A mug runs (its pid), or launchd was asked to start one (`None`).
    Running(Option<u32>),
    /// The mug could not be started, or the installed app did not say whether it can serve
    /// one: why (a start that failed is recorded, [`crate::trouble`]).
    Failed(String),
}

impl Kept {
    /// What it says, in one line.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::NotHere(why) => format!("not this login's to keep awake: {why}"),
            Self::Off => "off by your choice (`aterm acaffeine on` turns it on)".to_string(),
            Self::Running(Some(pid)) => format!("the mug (pid {pid}) keeps this Mac awake"),
            Self::Running(None) => "launchd was asked to start the mug".to_string(),
            Self::Failed(why) => format!("the mug did not start: {why}"),
        }
    }
}

/// What keeping this Mac awake does, given what was read ([`keep_awake`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeepPlan {
    /// Start the mug.
    pub start: bool,
    /// Turn Enable at Startup on.
    pub turn_startup_on: bool,
}

/// The plan for a saved mode that is `on` now, for an installed app's mug when `installed` (in
/// `/Applications` or `~/Applications`, [`crate::launchd::is_installed`]), whose Enable at
/// Startup is `startup_on`, and which the person turned off when `startup_turned_off`. A
/// person's off, of the mode or of Enable at Startup, is never undone: nothing here writes the
/// mode, and only an installed app's job is put at every login.
#[must_use]
pub const fn keep_plan(
    on: bool,
    installed: bool,
    startup_on: bool,
    startup_turned_off: bool,
) -> KeepPlan {
    KeepPlan {
        start: on,
        turn_startup_on: on && installed && !startup_on && !startup_turned_off,
    }
}

/// The trouble the window raises now ([`crate::trouble`]): its why once it has lasted
/// [`crate::trouble::LASTING_SECS`] at `now` (Unix seconds), unless the person turned keep-awake
/// off or this process is not the login's.
#[must_use]
pub fn lasting_trouble(
    kept: &Kept,
    trouble: Option<&crate::trouble::Trouble>,
    now: u64,
) -> Option<String> {
    match kept {
        Kept::NotHere(_) | Kept::Off => None,
        Kept::Running(_) | Kept::Failed(_) => trouble
            .filter(|trouble| trouble.lasting(now))
            .map(|trouble| trouble.why.clone()),
    }
}

/// Make sure this build's mug keeps this Mac awake, unless the person turned it off: what the
/// old `aterm caffeine` name left tidied once a process ([`crate::legacy::tidy`]), the installed
/// app's Enable at Startup turned on unless the person turned it off, and the mug started.
/// Nothing is written to standard output: an ordinary window's launch and its watch run it.
#[must_use]
pub fn keep_awake() -> Kept {
    #[cfg(target_os = "macos")]
    {
        mac::keep_awake()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Kept::NotHere("aCaffeine is macOS only".to_string())
    }
}

/// [`keep_awake`] for the installed app's mug, whichever aterm runs it: the converge pass, whose
/// runner may be a build or a held copy of the app. It first retires the old `aterm caffeine`
/// name's login jobs no aterm will run again ([`crate::legacy::retire_jobs`], which takes no lock
/// and touches no copy), whatever the app or the mode, so a pass that starts the mug on a Mac
/// where no window opens after a restart stops them too. The app is the first in
/// `/Applications` or `~/Applications` that has the `acaffeine` verb ([`installed_app`]); with
/// none, nothing is started ([`Kept::NotHere`]), and one that did not answer is
/// [`Kept::Failed`].
#[must_use]
pub fn keep_installed_awake() -> Kept {
    #[cfg(target_os = "macos")]
    {
        mac::keep_installed_awake()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Kept::NotHere("aCaffeine is macOS only".to_string())
    }
}

/// What the window's keep-awake watch does with its exceptions entry after a pass
/// ([`EntryWatch::step`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryWord {
    /// Raise it, saying why.
    Raise(String),
    /// Clear it, whichever process raised it.
    Clear,
}

/// The window's memory of the exceptions entry its keep-awake watch raised.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntryWatch {
    /// The why this process raised, until it cleared it.
    raised: Option<String>,
}

impl EntryWatch {
    /// What to do with the entry after a pass that found `kept` and `lasting`
    /// ([`lasting_trouble`]): a lasting trouble is raised once for each why; with none, the entry
    /// is cleared on every pass that read the mug, because the messages log keeps an entry an
    /// earlier process raised (before a quit or an update's relaunch), which this process never
    /// raised and must clear all the same, and a clear by key that finds nothing ends nothing.
    /// A process that is not the login's reads nothing and says nothing.
    pub fn step(&mut self, kept: &Kept, lasting: Option<String>) -> Option<EntryWord> {
        match lasting {
            Some(why) if self.raised.as_deref() == Some(why.as_str()) => None,
            Some(why) => {
                self.raised = Some(why.clone());
                Some(EntryWord::Raise(why))
            }
            None if matches!(kept, Kept::NotHere(_)) => None,
            None => {
                self.raised = None;
                Some(EntryWord::Clear)
            }
        }
    }

    /// Whether this process raised the entry and has not cleared it.
    #[must_use]
    pub const fn raised(&self) -> bool {
        self.raised.is_some()
    }
}

/// [`keep_awake`], and the trouble the window raises now ([`lasting_trouble`]).
#[must_use]
pub fn keep_awake_watch() -> (Kept, Option<String>) {
    let kept = keep_awake();
    #[cfg(target_os = "macos")]
    let trouble = mac::login_paths()
        .ok()
        .and_then(|paths| crate::trouble::read(&paths));
    #[cfg(not(target_os = "macos"))]
    let trouble = None;
    let now = aterm_time::unix_secs().unwrap_or_default();
    let lasting = lasting_trouble(&kept, trouble.as_ref(), now);
    (kept, lasting)
}

/// The installed app whose mug a pass with no window keeps: the first in `/Applications` or
/// `~/Applications` with the `acaffeine` verb ([`crate::launchd::serving_install`]).
#[must_use]
pub fn installed_app() -> crate::launchd::Installed {
    #[cfg(target_os = "macos")]
    {
        match mac::login_paths() {
            Ok(paths) => mac::installed(&paths),
            Err(why) => crate::launchd::Installed::Absent(why),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::launchd::Installed::Absent("aCaffeine is macOS only".to_string())
    }
}

/// Whether the person turned an installed app's Enable at Startup off
/// ([`crate::launchd::choose_startup`]): the subscriber's pass, which runs at login with no
/// window, then leaves the mug to a window.
#[must_use]
pub fn installed_startup_turned_off() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::login_paths().is_ok_and(|paths| {
            crate::launchd::installed_executables(&paths.home)
                .iter()
                .any(|exe| {
                    crate::launchd::startup_turned_off(&paths, &crate::launchd::label_for(exe))
                })
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Labels of the old `aterm caffeine` name's login jobs no aterm will run again
/// ([`crate::legacy::retired_jobs`]), writing nothing and taking no lock: the converge pass reads
/// them so an old job whose `aterm` is gone or answers no to `aterm help caffeine` (0.145.0) is
/// retired even when the mug already runs. On a home that is not on a disk of this Mac only a job
/// whose `aterm` lies inside the home is among them: one outside it may be another Mac's mug.
#[must_use]
pub fn retired_old_jobs() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        mac::login_paths()
            .ok()
            .map_or_else(Vec::new, |paths| mac::retired_old_jobs(&paths))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Retire the old `aterm caffeine` name's login jobs no aterm will run again
/// ([`crate::legacy::retire_jobs`]), starting no mug and touching no Enable at Startup.
///
/// # Errors
/// This process is not the login's, this executable's path cannot be read, or a retired job's
/// plist could not be removed.
pub fn retire_old_jobs() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        mac::login_paths().and_then(|paths| mac::retire_old_jobs_only(&paths))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("aCaffeine is macOS only".to_string())
    }
}

/// The mug's files, when this process has the login's own roots ([`mac::login_paths`]).
/// # Errors
/// Why this process is not the login's.
#[cfg(target_os = "macos")]
pub(crate) fn login_paths() -> Result<crate::paths::Paths, String> {
    mac::login_paths()
}

/// Take this login's turn to change the mode and start or end the mug on it ([`mac::change`]).
#[cfg(target_os = "macos")]
pub(crate) fn change(
    paths: &crate::paths::Paths,
    said: impl FnOnce(String),
) -> Option<crate::state::Held> {
    mac::change(paths, said)
}

/// The verbs, on macOS.
#[cfg(target_os = "macos")]
mod mac {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use super::Kept;
    use crate::code;
    use crate::instance::{self, Instance};
    use crate::launchd::{self, JobState, Started};
    use crate::model::{Clocks, Mode};
    use crate::paths::Paths;
    use crate::{state, trouble};

    /// The verb's name, as its answers and errors say it.
    const VERB: &str = "aterm acaffeine";

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
    pub(super) struct Here {
        pub paths: Paths,
        pub origin: PathBuf,
        pub label: String,
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
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Running {
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

    /// How long [`ensure`] waits for a mug to hold the lock: this build's job's process holding
    /// none to take it or end (a mug takes it as it starts, after its first exec, which macOS may
    /// assess for seconds, and one that quits lets go of it just before it ends), and a job it
    /// started to show its mug there.
    const SETTLE: Duration = Duration::from_secs(10);

    /// How often [`ensure`] looks again while it waits.
    const SETTLE_POLL: Duration = Duration::from_millis(100);

    /// Make sure a mug runs: the one holding the lock, else this build's job, started
    /// ([`ensure_with`]).
    fn ensure(here: &Here) -> Result<Running, String> {
        ensure_at(&here.paths, &here.label, &here.origin)
    }

    /// [`ensure`] for the job `label` running `origin` ([`ensure_with`]).
    fn ensure_at(paths: &Paths, label: &str, origin: &Path) -> Result<Running, String> {
        ensure_with(
            || running_mug(paths),
            || launchd::ensure_running(paths, label, origin),
            Instant::now() + SETTLE,
            || std::thread::sleep(SETTLE_POLL),
        )
    }

    /// [`ensure`], with the mug holding the lock read by `mug` and this build's job started by
    /// `job` ([`launchd::ensure_running`]), looking again after `pause` up to `until`. A job whose
    /// process holds no lock is waited for until it does or ends: one starting has not taken it
    /// yet, and one ending after the person's Quit has let go of it to read the mode again, so a
    /// mode saved on after that read starts the job again once its process is gone, where that
    /// mug would otherwise end, its job left loaded and idle, with nothing to start it; past
    /// `until` that process is taken for the running mug. A job started is waited for until a mug
    /// holds the lock with its job's label written: a [`change`] held across this then lasts
    /// until a `quit` of another build finds that mug and its job, where it would otherwise find
    /// neither, answer `stopped`, and leave the mug starting to read its off (a `quit` of this
    /// build finds the job by its own label).
    pub(super) fn ensure_with(
        mut mug: impl FnMut() -> Option<instance::Holder>,
        mut job: impl FnMut() -> Result<Started, String>,
        until: Instant,
        mut pause: impl FnMut(),
    ) -> Result<Running, String> {
        let mut started = false;
        loop {
            let holder = mug();
            if started {
                if holder.is_some_and(|holder| holder.label.is_some()) || Instant::now() >= until {
                    return Ok(Running::Started);
                }
                pause();
                continue;
            }
            if let Some(holder) = holder {
                return Ok(Running::Already(holder.pid));
            }
            match job()? {
                Started::AlreadyRunning { .. } if Instant::now() < until => pause(),
                Started::AlreadyRunning { pid } => return Ok(Running::Already(pid)),
                Started::Kicked | Started::Bootstrapped => started = true,
            }
        }
    }

    /// How long a change waits for another's [`change`] before it goes on without it.
    const CHANGE_WAIT: Duration = Duration::from_secs(30);

    /// How often a change waiting for another's looks again.
    const CHANGE_POLL: Duration = Duration::from_millis(20);

    /// Take this login's turn to change the mode and start or end the mug on it: a keep-awake
    /// pass ([`keep_awake_at`], a window's or the converge pass's), `on` and [`quit`] each hold it
    /// from the read or save of the mode to the start or end of the mug, and a job's mug ending
    /// after the person's Quit from before it removes its copy, its lock still held, over its
    /// let-go of the lock, its read of the mode and its naming of the job again, so none runs
    /// inside another. A `quit` run between a window's read of an on mode and its start would
    /// otherwise save off, find no mug to end and answer `stopped`, and the window would then
    /// start a mug that reads off. The turn is a `flock` on the session plists' folder
    /// ([`Paths::session_dir`]), made if missing, which no save of the mode locks
    /// ([`state::Held`]); `None`, with why passed to `said`, when it is not taken within
    /// [`CHANGE_WAIT`] or cannot be, and the change goes on without it.
    pub(super) fn change(paths: &Paths, said: impl FnOnce(String)) -> Option<state::Held> {
        match change_in(&paths.session_dir(), CHANGE_WAIT) {
            Ok(held) => Some(held),
            Err(why) => {
                said(format!(
                    "{why}; going on without taking turns with another start or quit"
                ));
                None
            }
        }
    }

    /// [`change`]'s `flock` on `dir`, made if missing, waited for up to `wait`; why it was not
    /// taken: still held by another once `wait` is up, or the folder's error.
    pub(super) fn change_in(dir: &Path, wait: Duration) -> Result<state::Held, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        match state::hold_dir(dir, wait, || std::thread::sleep(CHANGE_POLL)) {
            Ok(Some(held)) => Ok(held),
            Ok(None) => Err(format!(
                "another start or quit still held {} after {} s",
                dir.display(),
                wait.as_secs()
            )),
            Err(e) => Err(format!("{}: {e}", dir.display())),
        }
    }

    /// What [`keep_awake_at`] reaches beyond the mug's files (launchd), so a test runs it over a
    /// scratch root with nothing started.
    #[derive(Clone, Copy)]
    pub(super) struct KeepDoors {
        /// [`launchd::prune_orphans`].
        pub prune_orphans: fn(&Paths),
        /// [`launchd::set_startup`].
        pub set_startup: fn(&Paths, &str, &Path, bool) -> std::io::Result<()>,
        /// Make sure a mug runs ([`ensure_at`]).
        pub ensure: fn(&Paths, &str, &Path) -> Result<Running, String>,
    }

    /// The live doors.
    const LIVE_KEEP: KeepDoors = KeepDoors {
        prune_orphans: launchd::prune_orphans,
        set_startup: launchd::set_startup,
        ensure: ensure_at,
    };

    pub(super) fn start() -> ExitCode {
        let here = match here() {
            Ok(here) => here,
            Err(why) => return fail(why),
        };
        launchd::prune_orphans(&here.paths);
        match ensure(&here) {
            Ok(Running::Already(pid)) => answer(&format!("acaffeine=running pid={pid}\n")),
            Ok(Running::Started) => answer("acaffeine=started\n"),
            Err(why) => fail(why),
        }
    }

    /// [`super::keep_awake`] for this executable's mug. The first pass of a process also retires
    /// what the old `aterm caffeine` name left ([`crate::legacy::tidy`]) and removes the
    /// temporaries that cut-short writes left beside the state files ([`state::sweep_temps`]) and
    /// the job plists ([`launchd::sweep_plist_temps`]), once for each window launch, as the
    /// autostart did before the watch.
    pub(super) fn keep_awake() -> Kept {
        static TIDY: std::sync::Once = std::sync::Once::new();
        let here = match here() {
            Ok(here) => here,
            Err(why) => return Kept::NotHere(why),
        };
        TIDY.call_once(|| {
            for line in crate::legacy::tidy(&here.paths, &here.origin) {
                aterm_log::warn!("acaffeine: what `aterm caffeine` left: {line}");
            }
            let beside = state::sweep_temps(&here.paths);
            let plists = launchd::sweep_plist_temps(&here.paths);
            for (file, swept) in beside.into_iter().chain(plists) {
                match swept {
                    Ok(0) => {}
                    Ok(gone) => {
                        let noun = if gone == 1 {
                            "temporary"
                        } else {
                            "temporaries"
                        };
                        aterm_log::info!(
                            "acaffeine: removed {gone} {noun} of {} that writes cut short left",
                            file.display()
                        );
                    }
                    Err(e) => {
                        aterm_log::warn!("acaffeine: temporaries of {}: {e}", file.display());
                    }
                }
            }
        });
        match now() {
            Ok(now) => keep_awake_at(&here, now, &LIVE_KEEP),
            Err(why) => Kept::Failed(why),
        }
    }

    /// The installed app whose mug a pass with no window keeps ([`launchd::serving_install`]):
    /// this process's own executable needs no asking.
    pub(super) fn installed(paths: &Paths) -> launchd::Installed {
        let own = std::env::current_exe().and_then(std::fs::canonicalize).ok();
        launchd::serving_install(
            &launchd::installed_present(&paths.home),
            own.as_deref(),
            code::serves_acaffeine,
        )
    }

    /// [`super::keep_installed_awake`]: the installed app's mug (in `/Applications` or
    /// `~/Applications`), when one installed there has the `acaffeine` verb ([`installed`]); an
    /// older aterm there is never started as the job, which would fail every ten seconds.
    pub(super) fn keep_installed_awake() -> Kept {
        match login_paths() {
            Ok(paths) => keep_installed_awake_with(paths, &LIVE_INSTALLED),
            Err(why) => Kept::NotHere(why),
        }
    }

    /// What [`keep_installed_awake_with`] reaches beyond the login's files, so a test runs it over
    /// a scratch root with nothing asked, retired or started.
    #[derive(Clone, Copy)]
    pub(super) struct InstalledDoors {
        /// Retire the old name's login jobs no aterm will run again ([`retire_old_jobs`]).
        pub retire_old: fn(&Paths) -> Vec<String>,
        /// The installed app whose mug is kept ([`installed`]).
        pub installed: fn(&Paths) -> launchd::Installed,
        /// Both clocks ([`now`]).
        pub now: fn() -> Result<Clocks, String>,
        /// [`keep_awake_at`]'s doors.
        pub keep: KeepDoors,
    }

    /// The live doors.
    const LIVE_INSTALLED: InstalledDoors = InstalledDoors {
        retire_old: retire_old_jobs,
        installed,
        now,
        keep: LIVE_KEEP,
    };

    /// [`keep_installed_awake`] under the login's `paths`: the old name's login jobs no aterm will
    /// run again retired first, whatever the installed app or the mode, then the installed app's
    /// mug kept ([`keep_awake_at`]).
    pub(super) fn keep_installed_awake_with(paths: Paths, doors: &InstalledDoors) -> Kept {
        for line in (doors.retire_old)(&paths) {
            aterm_log::warn!("acaffeine: what `aterm caffeine` left: {line}");
        }
        let origin = match (doors.installed)(&paths) {
            launchd::Installed::At(origin) => origin,
            launchd::Installed::Absent(why) => return Kept::NotHere(why),
            launchd::Installed::Unanswered(why) => return Kept::Failed(why),
        };
        let now = match (doors.now)() {
            Ok(now) => now,
            Err(why) => return Kept::Failed(why),
        };
        let here = Here {
            label: launchd::label_for(&origin),
            paths,
            origin,
        };
        keep_awake_at(&here, now, &doors.keep)
    }

    /// Retire the old name's login jobs no aterm will run again for this executable
    /// ([`crate::legacy::retire_jobs`]): the part of [`crate::legacy::tidy`] that takes no lock
    /// and touches no copy. What could not be removed, one line each.
    fn retire_old_jobs(paths: &Paths) -> Vec<String> {
        match std::env::current_exe().and_then(std::fs::canonicalize) {
            Ok(exe) => crate::legacy::retire_jobs(paths, &exe),
            Err(e) => vec![format!("this executable's path cannot be read: {e}")],
        }
    }

    /// Labels of the old name's login jobs no aterm will run again for this executable, writing
    /// nothing.
    pub(super) fn retired_old_jobs(paths: &Paths) -> Vec<String> {
        let Ok(exe) = std::env::current_exe().and_then(std::fs::canonicalize) else {
            return Vec::new();
        };
        retired_old_jobs_with(paths, &exe, crate::legacy::has_old_verb)
    }

    /// [`retired_old_jobs`], asking another program whether it has the old verb with `has_verb`.
    pub(super) fn retired_old_jobs_with(
        paths: &Paths,
        exe: &Path,
        has_verb: impl Fn(&Path) -> Option<bool>,
    ) -> Vec<String> {
        crate::legacy::retired_jobs_with(paths, exe, has_verb)
            .into_iter()
            .map(|(label, _)| label)
            .collect()
    }

    /// [`super::retire_old_jobs`] under `paths`: retires only the old name's login jobs no aterm
    /// will run again, starting no mug.
    pub(super) fn retire_old_jobs_only(paths: &Paths) -> Result<String, String> {
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|e| format!("this executable's path cannot be read: {e}"))?;
        retire_old_jobs_only_with(
            paths,
            &exe,
            crate::legacy::has_old_verb,
            launchd::unload_idle,
        )
    }

    /// [`retire_old_jobs_only`], asking another program whether it has the old verb with
    /// `has_verb` and unloading each retired job with `unload`.
    pub(super) fn retire_old_jobs_only_with(
        paths: &Paths,
        exe: &Path,
        has_verb: impl Fn(&Path) -> Option<bool>,
        unload: impl Fn(&str) -> launchd::Unload,
    ) -> Result<String, String> {
        let jobs = crate::legacy::retired_jobs_with(paths, exe, has_verb);
        if jobs.is_empty() {
            return Ok("no old `aterm caffeine` login item remained".to_string());
        }
        let labels: Vec<String> = jobs.iter().map(|(label, _)| label.clone()).collect();
        let failed = launchd::retire_with(jobs, unload);
        if !failed.is_empty() {
            return Err(failed.join("; "));
        }
        let noun = if labels.len() == 1 { "item" } else { "items" };
        Ok(format!(
            "retired old `aterm caffeine` login {noun}: {}",
            labels.join(", ")
        ))
    }

    /// Keep this Mac awake with `here`'s mug unless the person turned keep-awake off: an
    /// installed app's Enable at Startup ([`launchd::is_installed`]) turned on unless the person
    /// turned it off, then the mug started. Nothing here writes the mode: with no saved state the
    /// mug keeps the Mac awake forever, and a person's off is theirs. The read of the mode and
    /// the start of the mug are one [`change`], so a `quit` waits for the start rather than end
    /// nothing and leave a mug starting to read its off. A start that fails is recorded
    /// ([`trouble::note`]).
    pub(super) fn keep_awake_at(here: &Here, now: Clocks, doors: &KeepDoors) -> Kept {
        let _turn = change(&here.paths, |why| aterm_log::warn!("acaffeine: {why}"));
        let wanted = state::load(&here.paths.state_file(), now).is_on(now);
        (doors.prune_orphans)(&here.paths);
        let plan = super::keep_plan(
            wanted,
            launchd::is_installed(&here.origin, &here.paths.home),
            launchd::startup_enabled(&here.paths, &here.label),
            launchd::startup_turned_off(&here.paths, &here.label),
        );
        if !plan.start {
            trouble::clear(&here.paths);
            return Kept::Off;
        }
        if plan.turn_startup_on {
            match (doors.set_startup)(&here.paths, &here.label, &here.origin, true) {
                Ok(()) => aterm_log::info!(
                    "acaffeine: Enable at Startup is on, as aterm installs it ({})",
                    here.paths.startup_plist(&here.label).display()
                ),
                Err(e) => aterm_log::warn!("acaffeine: could not turn on Enable at Startup: {e}"),
            }
        }
        match (doors.ensure)(&here.paths, &here.label, &here.origin) {
            Ok(Running::Already(pid)) => Kept::Running(Some(pid)),
            Ok(Running::Started) => Kept::Running(None),
            Err(why) => {
                trouble::note(&here.paths, &format!("the mug did not start: {why}"));
                Kept::Failed(why)
            }
        }
    }

    /// Save `mode` and tell a running mug.
    fn set_mode(paths: &Paths, mode: Mode) -> Result<(), String> {
        let file = paths.state_file();
        state::save(&file, mode).map_err(|e| format!("{}: {e}", file.display()))?;
        crate::notify::post_changed();
        if mode == Mode::Off {
            turned_off(paths);
        }
        Ok(())
    }

    pub(super) fn on(span: Option<u64>) -> ExitCode {
        let here = match here() {
            Ok(here) => here,
            Err(why) => return fail(why),
        };
        let _turn = change(&here.paths, |why| eprintln!("{VERB}: {why}"));
        let now = match now() {
            Ok(now) => now,
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
            Ok(()) => answer("acaffeine=off\n"),
            Err(why) => fail(why),
        }
    }

    /// The person's off is saved: nothing keeps the Mac awake for them now, so no trouble keeping
    /// it awake is theirs to hear of.
    fn turned_off(paths: &Paths) {
        trouble::clear(paths);
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
            state::load_lid(&here.paths.lid_file()),
        );
        answer(&text)
    }

    /// Keep Awake with Lid Closed: said, or saved and told to a running mug, which reads it on
    /// the notification ([`crate::notify::post_changed`]); a mug started later reads it at its
    /// start.
    pub(super) fn lid(set: Option<bool>) -> ExitCode {
        let paths = match login_paths() {
            Ok(paths) => paths,
            Err(why) => return fail(why),
        };
        let file = paths.lid_file();
        if let Some(on) = set {
            if let Err(e) = state::save_lid(&file, on) {
                return fail(format!("{}: {e}", file.display()));
            }
            crate::notify::post_changed();
        }
        answer(&super::lid_line(state::load_lid(&file)))
    }

    /// Enable at Startup for this build's job: the plist it writes runs this executable.
    /// On also lifts launchd's disable of the job; one Login Items' switch blocks fails,
    /// naming that switch. Off is the person's, kept ([`launchd::choose_startup`]).
    pub(super) fn startup(set: Option<bool>) -> ExitCode {
        let here = match here() {
            Ok(here) => here,
            Err(why) => return fail(why),
        };
        if let Some(on) = set
            && let Err(e) = launchd::choose_startup(&here.paths, &here.label, &here.origin, on)
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

    pub(super) fn quit(own: bool) -> ExitCode {
        let here = match here() {
            Ok(here) => here,
            Err(why) => return fail(why),
        };
        let _turn = change(&here.paths, |why| eprintln!("{VERB}: {why}"));
        let now = match now() {
            Ok(now) => now,
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
            if let Err(why) = instance::terminate(pid) {
                return fail(why);
            }
            ended.push(pid);
        }
        if let Err(pid) = instance::await_released(&lock, &ended, instance::QUIT_HANG) {
            return fail(format!(
                "the mug (pid {pid}) still holds {} {} s after it was ended",
                lock.display(),
                instance::QUIT_HANG.as_secs()
            ));
        }
        if !other && here.paths.code_dir().exists() {
            discard_copy(&here.paths);
        }
        if !own && saved.is_ok() && state::saved_on(&here.paths.state_file(), now) {
            start_again(&here);
        }
        match saved {
            Err(why) => fail(why),
            Ok(()) if !ended.is_empty() => answer("acaffeine=quit\n"),
            Ok(()) => answer("acaffeine=stopped\n"),
        }
    }

    /// Start a mug for a writer whose on landed while [`quit`] ended the last one: one that takes no
    /// [`change`] (the ending mug's own menu, or a window's start or an `on` of a build before it)
    /// found the ending mug still holding its lock and started none, and the bootout leaves no job
    /// that launchd would start again. Said on stderr; `quit`'s answer stays.
    /// Not after `quit own`, a scoped uninstall's: it ends this build because the build is going,
    /// and the build that stays starts its own mug at its next window.
    fn start_again(here: &Here) {
        let how = match ensure(here) {
            Ok(Running::Already(pid)) => format!("one runs (pid {pid})"),
            Ok(Running::Started) => "one was started".to_string(),
            Err(why) => format!("starting one failed: {why}"),
        };
        eprintln!("{VERB}: the mode was set on again while the mug quit; {how}");
    }

    /// Remove the ended mug's copy of the app, as a mug that quits from its menu does: once an
    /// update retires the bundle it was cloned from, its blocks are its own.
    fn discard_copy(paths: &Paths) {
        let Ok(Some(held)) = Instance::try_hold(&paths.lock_file()) else {
            return;
        };
        match code::discard_copies(&paths.code_dir()) {
            Ok(kept) => kept.say(),
            Err(e) => eprintln!("{VERB}: {}: {e}", paths.code_dir().display()),
        }
        drop(held);
    }

    /// The one stderr line of a `serve` that does not run here, and exit 0, which launchd's
    /// KeepAlive does not start again: this process is not the login's ([`login_paths`]), which
    /// no start again cures.
    pub(super) fn refuse(paths: Option<&Paths>, why: impl std::fmt::Display) -> ExitCode {
        if let Some(paths) = paths {
            crate::macos::release_bridge(paths);
        }
        eprintln!("{VERB}: serve: {why}");
        ExitCode::SUCCESS
    }

    /// The one stderr line of a `serve` that could not keep the Mac awake for a reason that can
    /// clear up, recorded as the mug's trouble ([`trouble::note`]), and
    /// [`launchd::EXIT_RETRY`], which launchd's KeepAlive starts again, about a minute later as
    /// the job ([`launchd::pause_before_retry`]), from the copy of the app it ran from
    /// ([`code::prepare_copy`]). A bridge an update restart left holds on meanwhile.
    pub(super) fn retry(paths: &Paths, why: impl std::fmt::Display) -> ExitCode {
        let why = why.to_string();
        eprintln!("{VERB}: serve: {why}; launchd starts it again");
        trouble::note(paths, &why);
        launchd::pause_before_retry();
        ExitCode::from(launchd::EXIT_RETRY)
    }

    /// The one stderr line of a `serve` that cannot keep the Mac awake for a reason no start again
    /// cures, recorded as the mug's trouble ([`trouble::note`]) for the window's watch, `aterm
    /// doctor`'s `awake:` row and the converge pass to report, and exit 0, which launchd's
    /// KeepAlive does not start again: a start a minute would cure nothing. A window's watch or a
    /// converge pass starts the job again, which tries once more. A bridge an update restart left
    /// holds on, to its own end.
    pub(super) fn stall(paths: &Paths, why: impl std::fmt::Display) -> ExitCode {
        let why = why.to_string();
        eprintln!("{VERB}: serve: {why}; a window or a converge pass starts it again");
        trouble::note(paths, &why);
        ExitCode::SUCCESS
    }

    /// [`retry`] for an error that can clear up, else [`stall`] ([`lasts`]).
    fn retry_or_stall(paths: &Paths, e: &std::io::Error, why: impl std::fmt::Display) -> ExitCode {
        if lasts(e) {
            stall(paths, why)
        } else {
            retry(paths, why)
        }
    }

    /// Whether a file error is one no start again cures: a path that is not what it must be (a
    /// directory, a FIFO, a link, another user's file, which [`Instance::try_hold`] and
    /// [`code::prepare_copy`] refuse as invalid input), a permission refused, or an operation the
    /// file system does not have. A full disk, a file gone and a passing shortage clear up.
    pub(super) fn lasts(e: &std::io::Error) -> bool {
        use std::io::ErrorKind;
        matches!(
            e.kind(),
            ErrorKind::InvalidInput
                | ErrorKind::PermissionDenied
                | ErrorKind::IsADirectory
                | ErrorKind::NotADirectory
                | ErrorKind::Unsupported
        ) || matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTSUP))
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
                return retry(
                    &paths,
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
                    Ok(instance) => instance,
                    Err(code) => return code,
                },
            };
            name_job(&mut instance, &launchd::label_for(&origin), &lock);
            (origin, instance)
        } else {
            let origin = match std::env::current_exe().and_then(std::fs::canonicalize) {
                Ok(origin) => origin,
                Err(e) => {
                    return retry(
                        &paths,
                        format!("this executable's path cannot be read: {e}"),
                    );
                }
            };
            let mut instance = match hold(&paths) {
                Ok(instance) => instance,
                Err(code) => return code,
            };
            let label = launchd::label_for(&origin);
            name_job(&mut instance, &label, &lock);
            launchd::prune_orphans(&paths);
            match code::prepare_copy(&origin, &code_dir, &label) {
                Ok(Some(copy)) => {
                    let e = code::reexec_holding(&copy, &origin, &instance);
                    return retry_or_stall(
                        &paths,
                        &e,
                        format!("re-running from {}: {e}; not starting", copy.display()),
                    );
                }
                Ok(None) => (origin, instance),
                Err(e) => {
                    return retry_or_stall(
                        &paths,
                        &e,
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

    /// Take the one-mug lock. Another mug holding it ends this `serve` with 0 (said on stderr):
    /// that mug keeps the Mac awake. A lock something else still holds after [`HOLD_PATIENCE`] (a
    /// `quit` discarding the copy, a mug a moment from its end), or one not taken for a reason
    /// that can clear up (a full disk), is [`retry`]'s; a name that cannot hold the lock at all (a
    /// directory, a FIFO, a link, another user's file) is [`stall`]'s ([`lasts`]).
    pub(super) fn hold(paths: &Paths) -> Result<Instance, ExitCode> {
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
            Ok(Some(instance)) => Ok(instance),
            Ok(None) => match instance::running_mug(&lock, &code_dir) {
                Some(mug) => {
                    eprintln!("{VERB}: serve: already running (pid {})", mug.pid);
                    Err(ExitCode::SUCCESS)
                }
                None => {
                    let by = instance::running_pid(&lock)
                        .map_or_else(String::new, |pid| format!(" by pid {pid}"));
                    Err(retry(
                        paths,
                        format!(
                            "{} is held{by}, which is no mug, {} s on; not starting",
                            lock.display(),
                            HOLD_PATIENCE.as_secs()
                        ),
                    ))
                }
            },
            Err(e) => Err(retry_or_stall(
                paths,
                &e,
                format!("{}: {e}; not starting", lock.display()),
            )),
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
        let dir = crate::test_scratch_dir("cli");
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

    /// A start finds the mug holding the lock; else, while this build's job runs a process that
    /// holds none (a mug starting, or one ending after a Quit), it looks again until a mug holds
    /// the lock or the job's process is gone and the job is started, and past its time takes the
    /// job's process for the mug; a job it started it waits for until a mug holds the lock with
    /// its label written, or its time is up ([`mac::ensure_with`]).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_start_waits_out_a_jobs_process_that_holds_no_lock() {
        use std::cell::Cell;
        use std::time::{Duration, Instant};

        use crate::instance::Holder;
        use crate::launchd::Started;
        use mac::{Running, ensure_with};
        let later = Instant::now() + Duration::from_secs(60);
        let named = |pid| Holder {
            pid,
            label: Some("com.aterm.aterm.acaffeine.dev.0123abcd".to_string()),
        };
        let bounded = |waits: &Cell<u32>| {
            waits.set(waits.get() + 1);
            assert!(waits.get() < 10, "a wait that ends");
        };

        let (kicked, looks, waits) = (Cell::new(false), Cell::new(0), Cell::new(0));
        let ended = ensure_with(
            || kicked.get().then(|| named(9)),
            || {
                looks.set(looks.get() + 1);
                kicked.set(looks.get() == 3);
                Ok(if kicked.get() {
                    Started::Kicked
                } else {
                    Started::AlreadyRunning { pid: 7 }
                })
            },
            later,
            || bounded(&waits),
        );
        assert_eq!(
            ended,
            Ok(Running::Started),
            "the ending mug's job, started again"
        );
        assert_eq!((looks.get(), waits.get()), (3, 2));

        let (mugs, waits) = (Cell::new(0), Cell::new(0));
        let starting = ensure_with(
            || {
                mugs.set(mugs.get() + 1);
                (mugs.get() == 3).then(|| named(9))
            },
            || Ok(Started::AlreadyRunning { pid: 8 }),
            later,
            || bounded(&waits),
        );
        assert_eq!(
            starting,
            Ok(Running::Already(9)),
            "a starting mug takes the lock"
        );

        let (mugs, waits) = (Cell::new(0), Cell::new(0));
        let started = ensure_with(
            || {
                mugs.set(mugs.get() + 1);
                match mugs.get() {
                    1 => None,
                    2 => Some(Holder {
                        pid: 9,
                        label: None,
                    }),
                    _ => Some(named(9)),
                }
            },
            || Ok(Started::Bootstrapped),
            later,
            || bounded(&waits),
        );
        assert_eq!(started, Ok(Running::Started));
        assert_eq!(
            (mugs.get(), waits.get()),
            (3, 1),
            "the started job is waited for, a pause between looks, until its mug names its label"
        );

        let past = ensure_with(
            || None,
            || Ok(Started::AlreadyRunning { pid: 7 }),
            Instant::now(),
            || panic!("no wait past its time"),
        );
        assert_eq!(past, Ok(Running::Already(7)));
        for started in [Started::Kicked, Started::Bootstrapped] {
            assert_eq!(
                ensure_with(
                    || None,
                    || Ok(started),
                    Instant::now(),
                    || panic!("no wait")
                ),
                Ok(Running::Started),
                "a started job whose mug is not up in time"
            );
        }
        let held = ensure_with(
            || Some(named(5)),
            || panic!("the mug holding the lock runs"),
            later,
            || panic!("no wait"),
        );
        assert_eq!(held, Ok(Running::Already(5)));
        assert_eq!(
            ensure_with(|| None, || Err("refused".to_string()), later, || {}),
            Err("refused".to_string())
        );
    }

    /// One change of mode and its start or end of the mug at a time: a second waits out the
    /// first's turn up to its time, and takes it once the first lets go; the turn's folder is
    /// made if missing, and a folder that cannot be made says why, at once ([`mac::change_in`]).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_change_waits_for_anothers_turn() {
        use std::time::Duration;
        let dir = crate::test_scratch_dir("cli");
        let folder = dir.path().join("state").join("acaffeine");
        let first = mac::change_in(&folder, Duration::ZERO).expect("the first turn");
        assert!(folder.is_dir(), "made");
        let start = std::time::Instant::now();
        let Err(why) = mac::change_in(&folder, Duration::from_millis(200)) else {
            panic!("another's turn");
        };
        assert!(start.elapsed() >= Duration::from_millis(200), "waited");
        assert!(
            why.starts_with("another start or quit still held "),
            "{why}"
        );
        drop(first);
        assert!(
            mac::change_in(&folder, Duration::ZERO).is_ok(),
            "taken once the first let go"
        );
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"").expect("a file where the folder goes");
        let start = std::time::Instant::now();
        let Err(why) = mac::change_in(&blocked.join("acaffeine"), Duration::from_secs(60)) else {
            panic!("no folder");
        };
        assert!(start.elapsed() < Duration::from_secs(60), "no wait");
        assert!(why.starts_with(&blocked.display().to_string()), "{why}");
        assert!(!why.contains("still held"), "{why}");
    }

    /// A `serve` that will not run lets a bridge an update restart left go, and exits 0; one
    /// that cannot find the mug's files touches none.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_serve_refusal_lets_a_left_bridge_go() {
        let dir = crate::test_scratch_dir("cli");
        let paths = crate::paths::Paths {
            state_dir: dir.path().join("state"),
            home: dir.path().join("home"),
            logs_dir: dir.path().join("logs"),
            local_volume: |_| true,
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

    #[cfg(target_os = "macos")]
    fn scratch(dir: &std::path::Path) -> crate::paths::Paths {
        crate::paths::Paths {
            state_dir: dir.join("state"),
            home: dir.join("home"),
            logs_dir: dir.join("logs"),
            local_volume: |_| true,
        }
    }

    /// `serve`'s lock: one something that is no mug still holds is started again by launchd
    /// (69), with why recorded and a bridge an update restart left holding on; a name that
    /// cannot hold a lock at all (a directory, a link, a FIFO) ends 0 with why recorded, so
    /// launchd does not start it every minute to the same refusal, copying the app each time.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_serve_that_cannot_take_the_lock_retries_only_what_can_clear_up() {
        let dir = crate::test_scratch_dir("cli");
        let paths = scratch(dir.path());
        std::fs::create_dir_all(&paths.state_dir).expect("state dir");
        std::fs::write(paths.bridge_file(), "4242\n").expect("bridge file");
        let held = crate::instance::Instance::try_hold(&paths.lock_file())
            .expect("hold")
            .expect("free");
        let code = mac::hold(&paths).expect_err("held by this test, which is no mug");
        assert_eq!(code, ExitCode::from(crate::launchd::EXIT_RETRY));
        let why = crate::trouble::read(&paths).expect("recorded").why;
        assert!(why.contains("which is no mug"), "{why}");
        assert!(paths.bridge_file().exists(), "the bridge holds on");
        drop(held);
        assert!(mac::hold(&paths).is_ok(), "a free lock is taken");

        type Plant = fn(&std::path::Path);
        let plants: [(&str, Plant); 3] = [
            ("a directory", |lock| {
                std::fs::create_dir(lock).expect("dir")
            }),
            ("a link", |lock| {
                let target = lock.with_extension("target");
                std::fs::write(&target, b"").expect("target");
                std::os::unix::fs::symlink(&target, lock).expect("link");
            }),
            ("a FIFO", |lock| {
                use std::os::unix::ffi::OsStrExt as _;
                let name = std::ffi::CString::new(lock.as_os_str().as_bytes()).expect("no NUL");
                // SAFETY: a NUL-terminated path the call reads and does not retain.
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
            }),
        ];
        for (name, plant) in plants {
            let dir = crate::test_scratch_dir("cli");
            let paths = scratch(dir.path());
            std::fs::create_dir_all(&paths.state_dir).expect("state dir");
            plant(&paths.lock_file());
            let code = mac::hold(&paths).expect_err(name);
            assert_eq!(
                code,
                ExitCode::SUCCESS,
                "{name}: never started again by launchd"
            );
            let why = crate::trouble::read(&paths).map(|trouble| trouble.why);
            assert!(why.is_some(), "{name}: recorded for doctor and the window");
        }
    }

    /// The copy refusals no start again cures (a FIFO in the bundle, a link at the code
    /// directory) are told from a full disk and a passing shortage, which launchd retries.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_copy_refusal_that_lasts_is_told_from_one_that_clears_up() {
        use std::io::{Error, ErrorKind};
        let dir = crate::test_scratch_dir("cli");
        let app = dir.path().join("X.app");
        let exe = app.join("Contents/MacOS/aterm");
        std::fs::create_dir_all(exe.parent().expect("MacOS")).expect("bundle");
        std::fs::write(&exe, b"exe").expect("exe");
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("elsewhere");
        let link = dir.path().join("code-link");
        std::os::unix::fs::symlink(&elsewhere, &link).expect("link");
        let refused = crate::code::prepare_copy(&exe, &link, "l").expect_err("a link");
        assert!(mac::lasts(&refused), "{refused}");
        let fifo = app.join("Contents/pipe");
        {
            use std::os::unix::ffi::OsStrExt as _;
            let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("no NUL");
            // SAFETY: a NUL-terminated path the call reads and does not retain.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
        }
        let refused = crate::code::prepare_copy(&exe, &dir.path().join("code"), "l")
            .expect_err("a FIFO in the bundle");
        assert!(mac::lasts(&refused), "{refused}");
        for clears in [
            Error::from(ErrorKind::StorageFull),
            Error::from_raw_os_error(libc::ENOSPC),
            Error::from_raw_os_error(libc::ENOENT),
            Error::from_raw_os_error(libc::ENOMEM),
        ] {
            assert!(!mac::lasts(&clears), "{clears}");
        }
    }

    /// Keeping this Mac awake (the window's watch, the converge pass) never undoes a person's
    /// off: over a saved off it starts nothing, writes no mode and turns no Enable at Startup on.
    /// With nothing saved it starts the mug and puts an installed app's job (here a
    /// `~/Applications` install's) at every login; an Enable at Startup the person turned off
    /// stays off; a dev build's is left as it is. 0.145's launch, which saved forever, fails it.
    #[cfg(target_os = "macos")]
    #[test]
    fn keeping_awake_over_a_persons_off_starts_and_writes_nothing() {
        use std::cell::{Cell, RefCell};
        thread_local! {
            static STARTED: Cell<u32> = const { Cell::new(0) };
            static STARTUP: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
        }
        let doors = mac::KeepDoors {
            prune_orphans: |_| {},
            set_startup: |paths, label, origin, on| {
                STARTUP.with_borrow_mut(|calls| calls.push(on));
                crate::launchd::set_startup(paths, label, origin, on)
            },
            ensure: |_, _, _| {
                STARTED.set(STARTED.get() + 1);
                Ok(mac::Running::Started)
            },
        };
        let at = |dir: &std::path::Path, installed: bool| {
            let paths = scratch(dir);
            let origin = if installed {
                paths
                    .home
                    .join("Applications/aterm.app/Contents/MacOS/aterm")
            } else {
                dir.join("worktree/target/debug/aterm")
            };
            std::fs::create_dir_all(origin.parent().expect("a directory")).expect("mkdir");
            std::fs::write(&origin, b"exe").expect("exe");
            mac::Here {
                label: crate::launchd::label_for(&origin),
                paths,
                origin,
            }
        };
        let calls = || (STARTED.get(), STARTUP.with_borrow(Clone::clone));
        let reset = || {
            STARTED.set(0);
            STARTUP.set(Vec::new());
        };

        let dir = crate::test_scratch_dir("cli");
        let here = at(dir.path(), true);
        let file = here.paths.state_file();
        crate::state::save(&file, Mode::Off).expect("the person's off");
        let saved = std::fs::read(&file).expect("saved");
        reset();
        assert_eq!(mac::keep_awake_at(&here, NOW, &doors), Kept::Off);
        assert_eq!(
            calls(),
            (0, Vec::new()),
            "nothing started, no startup turned on"
        );
        assert_eq!(
            std::fs::read(&file).expect("kept"),
            saved,
            "no mode written"
        );
        assert!(!here.paths.startup_plist(&here.label).exists());

        let dir = crate::test_scratch_dir("cli");
        let here = at(dir.path(), true);
        reset();
        assert_eq!(mac::keep_awake_at(&here, NOW, &doors), Kept::Running(None));
        assert_eq!(calls(), (1, vec![true]), "started, and put at every login");
        assert!(here.paths.startup_plist(&here.label).is_file());
        assert!(!here.paths.state_file().exists(), "no mode written");

        let dir = crate::test_scratch_dir("cli");
        let here = at(dir.path(), true);
        crate::launchd::choose_startup(&here.paths, &here.label, &here.origin, false)
            .expect("the person's startup off");
        reset();
        assert_eq!(mac::keep_awake_at(&here, NOW, &doors), Kept::Running(None));
        assert_eq!(
            calls(),
            (1, Vec::new()),
            "the person's startup off stays off"
        );

        let dir = crate::test_scratch_dir("cli");
        let here = at(dir.path(), false);
        reset();
        assert_eq!(mac::keep_awake_at(&here, NOW, &doors), Kept::Running(None));
        assert_eq!(
            calls(),
            (1, Vec::new()),
            "a dev build's job is not put at login"
        );
    }

    /// The converge pass's keep-awake retires the old `aterm caffeine` name's login jobs no aterm
    /// will run again (here one whose aterm is gone and one whose aterm says it has no old verb)
    /// before anything else, whatever the installed app or the mode, so a pass that starts the mug
    /// on a Mac where no window opens after a restart stops them too; then it keeps the installed
    /// app's mug as before.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_converge_pass_retires_the_old_names_jobs_whatever_it_keeps() {
        use std::cell::Cell;
        thread_local! {
            static STARTED: Cell<u32> = const { Cell::new(0) };
        }
        let keep = mac::KeepDoors {
            prune_orphans: |_| {},
            set_startup: |_, _, _, _| Ok(()),
            ensure: |_, _, _| {
                STARTED.set(STARTED.get() + 1);
                Ok(mac::Running::Started)
            },
        };
        let doors = mac::InstalledDoors {
            retire_old: |paths| {
                crate::legacy::retire_jobs_with(
                    paths,
                    std::path::Path::new("/nonexistent/this-aterm"),
                    |_| Some(false),
                    |_| crate::launchd::Unload::Unloaded,
                )
                .failed
            },
            installed: |paths| {
                crate::launchd::Installed::At(
                    paths
                        .home
                        .join("Applications/aterm.app/Contents/MacOS/aterm"),
                )
            },
            now: || Ok(NOW),
            keep,
        };
        let absent = mac::InstalledDoors {
            installed: |_| crate::launchd::Installed::Absent("not installed".to_string()),
            ..doors
        };
        let old_jobs = |dir: &std::path::Path| {
            let paths = scratch(dir);
            std::fs::create_dir_all(paths.launch_agents()).expect("agents");
            let other = dir.join("0.145.0/aterm");
            std::fs::create_dir_all(other.parent().expect("a folder")).expect("mkdir");
            std::fs::write(&other, b"exe").expect("exe");
            let plists = [
                ("com.aterm.aterm.caffeine", other),
                (
                    "com.aterm.aterm.caffeine.dev.0000000a",
                    dir.join("gone/aterm"),
                ),
            ]
            .map(|(label, program)| {
                let text = crate::launchd::plist_text(label, &program, &paths.log_file(), None)
                    .replace("<string>acaffeine</string>", "<string>caffeine</string>");
                let plist = paths.startup_plist(label);
                std::fs::write(&plist, text).expect("plist");
                plist
            });
            (paths, plists)
        };
        let retired = |plists: &[std::path::PathBuf]| plists.iter().all(|p| !p.exists());

        let dir = crate::test_scratch_dir("cli");
        let (paths, plists) = old_jobs(dir.path());
        STARTED.set(0);
        assert_eq!(
            mac::keep_installed_awake_with(paths, &absent),
            Kept::NotHere("not installed".to_string())
        );
        assert!(retired(&plists), "retired with no installed app");
        assert_eq!(STARTED.get(), 0);

        let dir = crate::test_scratch_dir("cli");
        let (paths, plists) = old_jobs(dir.path());
        crate::state::save(&paths.state_file(), Mode::Off).expect("the person's off");
        STARTED.set(0);
        assert_eq!(mac::keep_installed_awake_with(paths, &doors), Kept::Off);
        assert!(retired(&plists), "retired over the person's off");
        assert_eq!(STARTED.get(), 0);

        let dir = crate::test_scratch_dir("cli");
        let (paths, plists) = old_jobs(dir.path());
        STARTED.set(0);
        assert_eq!(
            mac::keep_installed_awake_with(paths, &doors),
            Kept::Running(None)
        );
        assert!(retired(&plists), "retired, and the mug started");
        assert_eq!(STARTED.get(), 1);
    }

    /// When a mug already runs, `sync.awake` reads and retires the old `aterm caffeine` login jobs
    /// no aterm will run again without starting a mug, while keeping an 0.144 job whose `aterm`
    /// still answers `help caffeine` (`Some(true)`).
    #[cfg(target_os = "macos")]
    #[test]
    fn retired_old_jobs_reads_and_retires_only_dead_or_post_rename_login_items() {
        use std::cell::RefCell;
        let dir = crate::test_scratch_dir("cli");
        let paths = scratch(dir.path());
        std::fs::create_dir_all(paths.launch_agents()).expect("agents");
        let post_rename = dir.path().join("0.145.0/aterm");
        let pre_rename = dir.path().join("0.144.0/aterm");
        for bin in [&post_rename, &pre_rename] {
            std::fs::create_dir_all(bin.parent().expect("parent")).expect("mkdir");
            std::fs::write(bin, b"exe").expect("exe");
        }
        for (label, program) in [
            ("com.aterm.aterm.caffeine", post_rename.clone()),
            (
                "com.aterm.aterm.caffeine.dev.0000000a",
                dir.path().join("gone/aterm"),
            ),
            ("com.aterm.aterm.caffeine.dev.0000000b", pre_rename.clone()),
        ] {
            let text = crate::launchd::plist_text(label, &program, &paths.log_file(), None)
                .replace("<string>acaffeine</string>", "<string>caffeine</string>");
            std::fs::write(paths.startup_plist(label), text).expect("plist");
        }
        let this_exe = std::path::Path::new("/nonexistent/this-aterm");
        let has_verb = |program: &std::path::Path| Some(program == pre_rename);
        let found = mac::retired_old_jobs_with(&paths, this_exe, has_verb);
        assert_eq!(
            found,
            [
                "com.aterm.aterm.caffeine",
                "com.aterm.aterm.caffeine.dev.0000000a"
            ]
        );
        assert!(
            paths.startup_plist("com.aterm.aterm.caffeine").exists(),
            "reading retired jobs writes nothing"
        );
        let unloaded = RefCell::new(Vec::new());
        let said = mac::retire_old_jobs_only_with(&paths, this_exe, has_verb, |label| {
            unloaded.borrow_mut().push(label.to_owned());
            crate::launchd::Unload::Unloaded
        })
        .expect("retires dead and 0.145.0 jobs");
        assert_eq!(
            said,
            "retired old `aterm caffeine` login items: com.aterm.aterm.caffeine, \
             com.aterm.aterm.caffeine.dev.0000000a"
        );
        assert_eq!(
            *unloaded.borrow(),
            [
                "com.aterm.aterm.caffeine",
                "com.aterm.aterm.caffeine.dev.0000000a"
            ]
        );
        assert!(!paths.startup_plist("com.aterm.aterm.caffeine").exists());
        assert!(
            !paths
                .startup_plist("com.aterm.aterm.caffeine.dev.0000000a")
                .exists()
        );
        assert!(
            paths
                .startup_plist("com.aterm.aterm.caffeine.dev.0000000b")
                .exists(),
            "NEGATIVE CONTROL: 0.144 job whose aterm still has `caffeine` is kept"
        );
        assert_eq!(
            mac::retired_old_jobs_with(&paths, this_exe, has_verb),
            Vec::<String>::new()
        );
    }

    /// An exceptions entry an earlier process raised (before a quit, or an update's relaunch) is
    /// cleared by the next process's watch once the trouble is gone, though that process never
    /// raised it; a lasting trouble is raised once for each why; a process that is not the
    /// login's says nothing.
    #[test]
    fn the_entry_an_earlier_process_raised_is_cleared_after_a_relaunch() {
        let why = "the menu bar refused the mug".to_string();
        let mut before = EntryWatch::default();
        let running = Kept::Running(Some(7));
        assert_eq!(
            before.step(&running, Some(why.clone())),
            Some(EntryWord::Raise(why.clone()))
        );
        assert!(before.raised());
        assert_eq!(before.step(&running, Some(why)), None, "raised once");
        assert_eq!(
            before.step(&running, Some("the disk is full".to_string())),
            Some(EntryWord::Raise("the disk is full".to_string())),
            "a new why replaces it"
        );
        drop(before);
        for kept in [
            Kept::Running(Some(8)),
            Kept::Running(None),
            Kept::Off,
            Kept::Failed("launchctl bootstrap: 5".to_string()),
        ] {
            let mut relaunched = EntryWatch::default();
            assert_eq!(
                relaunched.step(&kept, None),
                Some(EntryWord::Clear),
                "{kept:?}"
            );
            assert!(!relaunched.raised());
        }
        let mut elsewhere = EntryWatch::default();
        assert_eq!(
            elsewhere.step(&Kept::NotHere("a private root".to_string()), None),
            None
        );
    }

    /// An ordinary launch never undoes a person's off: a saved off starts nothing and turns
    /// nothing on, an Enable at Startup the person turned off stays off, and only the installed
    /// app's job is put at every login.
    #[test]
    fn a_launch_keeps_a_persons_off() {
        let plan = |on, installed, startup_on, turned_off| {
            let plan = keep_plan(on, installed, startup_on, turned_off);
            (plan.start, plan.turn_startup_on)
        };
        assert_eq!(
            plan(true, true, false, false),
            (true, true),
            "installed, nothing chosen"
        );
        assert_eq!(plan(true, true, true, false), (true, false), "on already");
        assert_eq!(
            plan(true, true, false, true),
            (true, false),
            "the person's startup off"
        );
        assert_eq!(
            plan(true, false, false, false),
            (true, false),
            "a dev build's job"
        );
        for installed in [true, false] {
            assert_eq!(
                plan(false, installed, false, false),
                (false, false),
                "the person's off"
            );
        }
    }

    /// The window raises a trouble once it has lasted, and never one of a person's off or of a
    /// process that is not the login's.
    #[test]
    fn only_a_lasting_trouble_of_a_kept_mug_is_raised() {
        let trouble = crate::trouble::Trouble {
            since: 1_000,
            why: "the menu bar refused the mug".to_string(),
        };
        let lasted = 1_000 + crate::trouble::LASTING_SECS;
        for kept in [
            Kept::Running(Some(7)),
            Kept::Failed("launchctl".to_string()),
        ] {
            assert_eq!(
                lasting_trouble(&kept, Some(&trouble), lasted),
                Some(trouble.why.clone()),
                "{kept:?}"
            );
            assert_eq!(lasting_trouble(&kept, Some(&trouble), lasted - 1), None);
            assert_eq!(lasting_trouble(&kept, None, lasted), None);
        }
        for kept in [Kept::Off, Kept::NotHere("a private root".to_string())] {
            assert_eq!(
                lasting_trouble(&kept, Some(&trouble), lasted),
                None,
                "{kept:?}"
            );
        }
    }

    #[test]
    fn every_command_parses() {
        assert_eq!(parsed(""), Ok(Command::Start));
        assert_eq!(parsed("start"), Ok(Command::Start));
        assert_eq!(parsed("on"), Ok(Command::On { span: None }));
        assert_eq!(parsed("off"), Ok(Command::Off));
        assert_eq!(parsed("status"), Ok(Command::Status));
        assert_eq!(parsed("lid"), Ok(Command::Lid(None)));
        assert_eq!(parsed("lid on"), Ok(Command::Lid(Some(true))));
        assert_eq!(parsed("lid off"), Ok(Command::Lid(Some(false))));
        assert!(parsed("lid yes").is_err());
        assert!(parsed("lid on now").is_err());
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
        assert!(line.starts_with("aterm acaffeine: "), "{line}");
        assert!(line.contains("aterm acaffeine --help"), "{line}");
    }

    const NOW: Clocks = Clocks {
        since_boot: 300_000,
        wall: 1_790_000_000,
    };

    #[test]
    fn status_says_the_mode_the_mug_startup_and_label() {
        assert_eq!(
            status_text(Mode::Off, NOW, None, Startup::Off, "l.x", false),
            "acaffeine=off\nuntil=-\nmenu=stopped\nstartup=off\nlabel=l.x\nlid=off\n"
        );
        assert_eq!(
            status_text(Mode::Forever, NOW, None, Startup::On, "l.x", false),
            "acaffeine=off\nuntil=-\nmenu=stopped\nstartup=on\nlabel=l.x\nlid=off\n"
        );
        assert_eq!(
            status_text(Mode::Forever, NOW, Some(42), Startup::Blocked, "l.x", false),
            "acaffeine=on\nuntil=forever\nmenu=running\npid=42\nstartup=blocked\nlabel=l.x\nlid=off\n"
        );
        let timed = Mode::activate(Some(HOUR), NOW);
        assert_eq!(
            status_text(timed, NOW, Some(7), Startup::Off, "l.x", false),
            format!(
                "acaffeine=on\nuntil={}\nmenu=running\npid=7\nstartup=off\nlabel=l.x\nlid=off\n",
                NOW.wall + HOUR
            )
        );
        assert_eq!(
            status_text(timed, NOW + HOUR, Some(7), Startup::Off, "l.x", false),
            "acaffeine=off\nuntil=-\nmenu=running\npid=7\nstartup=off\nlabel=l.x\nlid=off\n"
        );
        assert_eq!(
            status_text(Mode::Forever, NOW, Some(42), Startup::On, "l.x", true),
            "acaffeine=on\nuntil=forever\nmenu=running\npid=42\nstartup=on\nlabel=l.x\nlid=on\n"
        );
        assert_eq!(lid_line(true), "lid=on\n");
        assert_eq!(lid_line(false), "lid=off\n");
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
                status_text(timed, later, Some(7), Startup::Off, "l.x", false),
                format!(
                    "acaffeine=on\nuntil={}\nmenu=running\npid=7\nstartup=off\nlabel=l.x\nlid=off\n",
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
            "acaffeine=requested until=forever\n"
        );
        assert_eq!(
            on_line(Mode::activate(Some(60), NOW), NOW),
            format!("acaffeine=requested until={}\n", NOW.wall + 60)
        );
        assert_eq!(startup_line(Startup::On), "startup=on\n");
        assert_eq!(startup_line(Startup::Off), "startup=off\n");
        assert_eq!(startup_line(Startup::Blocked), "startup=blocked\n");
        assert_eq!(startup_line(Startup::Disabled), "startup=disabled\n");
        assert_eq!(until_word(Mode::Off, NOW), "-");
    }
}
