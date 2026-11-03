// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Command Line Tools install, an early step of a Mac's first launch. A launch that finds
//! Apple's Command Line Tools (git, cc, the macOS SDK) missing, which only a root install with the
//! person's password adds, asks for them at once, with no setup pass and no page to wait for: the
//! window opens a terminal tab titled `Command Line Tools` ([`TAB_TITLE`]) in front of its current
//! tab, whose own program is `aterm init --only base.clt`, in plain words
//! ([`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB`]): its one Mac password prompt is the
//! person's, in the tab, and a run parked on the tools wakes once they are there (System State
//! design §3.3). Until they exist the shim keeps Apple's own dialog away from everything aterm
//! starts ([`crate::clt_shim`]).
//!
//! aterm opens the tab by itself once per account and build on this Mac ([`Mark`]: a person who
//! closed it without installing is not asked again by a later launch of that build, and an update
//! retries unfinished setup once), once no macOS dialog is up, no installer is up, the keyboard
//! was quiet for 2.5 s, Low Data Mode is not holding the download back (a tab that found it on
//! spends no opening, and the next opens once it is off), and the account is one of the Mac's
//! administrators, whom alone macOS lets install them. Which window has focus, and what it shows,
//! holds nothing back. A pass that finds the tools missing later (they were there at launch) asks
//! too, under its run's admission. The GitHub repositories row's Install… opens it on a person's
//! press at any time ([`App::press_developer_tools_install`]); a press while a macOS dialog aterm
//! opened is up is kept, and the tab opens once that dialog closes ([`Install::PressedQueued`]).
//! The page reads where the install stands
//! ([`App::developer_tools_install_step`]): the tab asking for the password, the installer
//! running, Apple's own installer window waiting for the person, a press waiting, or why the
//! last press opened nothing.
//! Keys typed while it spawns stay where they were ([`crate::tab_spawn::SpawnAsk::unasked`]).
//! The same checks run before landing: a busy or typed-into destination defers the request
//! without opening an empty tab or consuming its attempt. Stop withdraws a request not yet opened.
//! The tab's program waits for the window's go line, typed once it landed, before it does
//! anything. Its exact child lifetime holds off other setup prompts; the finished terminal keeps
//! the result visible, and a program that ended without saying how gets the window's end line.

use crate::App;
use crate::Wake;
use crate::system_state::{NotOpened, ToolsInstall};
use aterm_types::macos_setup::Observation;

mod probe;
mod receipt;

/// The "revision" of the want a launch that found the tools missing makes itself: newer than any
/// pass, so only a person's Stop withdraws it, as a pass that no longer waits on the tools for its
/// own Repositories (packaged git serves them) says nothing of the tools themselves.
pub(crate) const LAUNCH_WANT: u64 = u64::MAX;

/// Where this launch's install stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Install {
    /// Nothing waits on the tools.
    #[default]
    Unasked,
    /// This launch ([`LAUNCH_WANT`]), or this exact run's last pass, waits on them: its tab opens
    /// once it is due ([`Install::due`]).
    Wanted(u64),
    /// A shell is starting, with the latest request retained until it can land.
    /// Stop or a pass that no longer needs the tools can withdraw it meanwhile.
    Opening { wanted: Option<u64> },
    /// A person pressed Install… while a macOS permission gesture held the dialog slot: queued
    /// so the row explains why and the tab opens once the slot frees and the keyboard has been
    /// quiet for 2.5 s.
    PressedQueued { wanted: Option<u64> },
    /// A person's press of Install… started a shell, which lands once its own check passes
    /// ([`App::press_developer_tools_install`]), whatever run is admitted.
    Pressed,
    /// The tab was opened (or its opening failed): never again this launch, so a closed tab
    /// or a password left untyped costs one tab per launch at most.
    Opened,
}

impl Install {
    /// A pass ended waiting on the tools: wanted, unless its tab already opened.
    #[must_use]
    pub(crate) const fn asked(self, revision: u64) -> Self {
        match self {
            Self::Unasked => Self::Wanted(revision),
            Self::Wanted(previous) if revision >= previous => Self::Wanted(revision),
            Self::Opening { wanted } => Self::Opening {
                wanted: Some(match wanted {
                    Some(previous) if previous > revision => previous,
                    _ => revision,
                }),
            },
            Self::PressedQueued { wanted } => Self::PressedQueued {
                wanted: Some(match wanted {
                    Some(previous) if previous > revision => previous,
                    _ => revision,
                }),
            },
            other => other,
        }
    }

    /// A pass no longer waits on them. It cannot withdraw a newer run's request.
    #[must_use]
    pub(crate) const fn withdrawn_by(self, revision: u64) -> Self {
        match self {
            Self::Wanted(previous) if revision >= previous => Self::Unasked,
            Self::Opening {
                wanted: Some(previous),
            } if revision >= previous => Self::Opening { wanted: None },
            Self::PressedQueued {
                wanted: Some(previous),
            } if revision >= previous => Self::Unasked,
            other => other,
        }
    }

    /// A pass ended without waiting on the tools, or a person stopped the run: a want not yet
    /// opened is dropped.
    #[must_use]
    pub(crate) const fn withdrawn(self) -> Self {
        match self {
            Self::Wanted(_) | Self::PressedQueued { .. } => Self::Unasked,
            Self::Opening { .. } => Self::Opening { wanted: None },
            other => other,
        }
    }

    /// Whether the tab opens now: wanted, with no macOS dialog this process started up
    /// (`dialog_up`).
    #[must_use]
    pub(crate) const fn due(self, dialog_up: bool) -> bool {
        matches!(self, Self::Wanted(_)) && !dialog_up
    }
}

/// What the tab's program gives `aterm`: the install item alone, with its one password prompt.
pub(crate) const ARGS: [&str; 3] = ["init", "--only", "base.clt"];

/// The tab's fixed title, which the page names ([`crate::system_state::TOOLS_PASSWORD_HEADLINE`]).
pub(crate) const TAB_TITLE: &str = "Command Line Tools";

/// How long the page keeps the GitHub repositories row calm after the tab's program said the
/// tools are installed, while the row's next reading has not landed ([`Settling`]): a reading
/// that says the same as before past it is the truth again.
const SETTLE_BOUND: std::time::Duration = std::time::Duration::from_secs(120);

/// The mark that the Command Line Tools tab has opened for this account on this Mac, and on which
/// builds of aterm: a small file beside this account's aterm config, so each account on the Mac
/// keeps its own, written as the tab lands, one `opened <build>` line per build that opened it
/// ([`aterm_types::version::build_id`]), the newest [`MARK_BUILDS`] kept. A build it names never
/// opens the tab by itself again, so a person who closed it without installing is not asked again
/// by a later launch of that build, nor by another build that shares this account's config and
/// opened it already (a release beside a dev build); a build it does not name, an update's, opens
/// it by itself once more, so updating aterm retries unfinished setup once. A bare `opened`, the
/// mark an aterm that kept no builds wrote, names none. The GitHub repositories row's Install…
/// still opens it on their press. A tab whose program found Low Data Mode holding the download
/// back, or said its install failed (its exit 1: a password macOS refused, a download that broke),
/// takes its build's line back ([`Mark::forget`]): that opening installed nothing, and no person
/// chose that. A stop the tab's program caught (a person's Ctrl-C, at the password prompt or later,
/// [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED`]) keeps it, as a closed tab does:
/// both are a person's decline.
#[derive(Debug, Default)]
pub(crate) struct Mark {
    path: Option<std::path::PathBuf>,
    opened: bool,
}

/// The mark's file name, beside aterm's config.
#[cfg(not(test))]
const MARK: &str = "command-line-tools-tab";

/// The builds a [`Mark`] names: past them the oldest build's line goes, so an account that runs
/// more builds than this (a dev build per commit) may see an old one retry once more.
const MARK_BUILDS: usize = 8;

/// The longest build id a [`Mark`] line names; aterm's are far shorter (a version, `+g` and a
/// commit).
const MARK_BUILD_BYTES: usize = 96;

/// The longest [`Mark`] a launch reads or writes: [`MARK_BUILDS`] `opened <build>` lines.
const MARK_BYTES: usize = MARK_BUILDS * ("opened ".len() + MARK_BUILD_BYTES + 1);

impl Mark {
    /// This account's mark, read once at launch for this build
    /// ([`aterm_types::version::build_id`]). A mark that cannot be read counts as naming it:
    /// aterm asks no one twice for want of a reading. Tests read none.
    pub(crate) fn for_launch() -> Self {
        #[cfg(test)]
        {
            Self::default()
        }
        #[cfg(not(test))]
        {
            let path = crate::app_config::config_path()
                .and_then(|config| config.parent().map(|dir| dir.join(MARK)));
            let opened = path
                .as_deref()
                .is_some_and(|path| Self::opened_on_build(path, &aterm_types::version::build_id()));
            Self { path, opened }
        }
    }

    /// A mark kept at `path`, as a launch of this build reads it (a test's).
    #[cfg(all(test, unix))]
    pub(crate) fn at(path: std::path::PathBuf) -> Self {
        let opened = Self::opened_on_build(&path, &aterm_types::version::build_id());
        Self {
            path: Some(path),
            opened,
        }
    }

    /// Whether the tab has opened for this account on this build.
    pub(crate) const fn opened(&self) -> bool {
        self.opened
    }

    /// Whether the mark at `path` names `build`, as a launch reads it: no mark, a bare `opened`
    /// and one naming only other builds are no opening on `build`, so a newer build retries
    /// unfinished setup once while repeated launches of one build do not ask again. A mark that
    /// cannot be read, one too long or malformed to be a mark, and a `build` no mark line could
    /// name each count as an opening.
    fn opened_on_build(path: &std::path::Path, build: &str) -> bool {
        if !Self::nameable(build) {
            return true;
        }
        match Self::read(path) {
            Ok(None) => false,
            Ok(Some(text)) => Self::builds(&text).is_none_or(|builds| builds.contains(&build)),
            Err(_) => true,
        }
    }

    /// The text of the mark at `path`, a regular file no longer than [`MARK_BYTES`] read without
    /// following a link or waiting on a pipe; `None` where there is none.
    fn read(path: &std::path::Path) -> std::io::Result<Option<String>> {
        match aterm_effects::file_feed::read_bounded_regular_utf8(path, MARK_BYTES) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The builds a mark's `text` names, oldest first: one `opened <build>` line each, and a bare
    /// `opened` naming none. `None` for any other text, an empty one included, and for more
    /// lines than a mark keeps.
    fn builds(text: &str) -> Option<Vec<&str>> {
        let mut lines = 0;
        let mut builds = Vec::new();
        for line in text.lines() {
            lines += 1;
            match line.split_once(' ') {
                None if line == "opened" => {}
                Some(("opened", build)) if Self::nameable(build) => builds.push(build),
                _ => return None,
            }
        }
        (1..=MARK_BUILDS).contains(&lines).then_some(builds)
    }

    /// Whether a mark line can name `build`: one word, no longer than [`MARK_BUILD_BYTES`].
    fn nameable(build: &str) -> bool {
        (1..=MARK_BUILD_BYTES).contains(&build.len()) && !build.contains(char::is_whitespace)
    }

    /// Put `build` into the mark at `path` as its newest line (`opened`), or take it out, keeping
    /// the other builds the mark names, the newest [`MARK_BUILDS`] in all; a mark left naming none
    /// goes. The write is durable, its directory entry included, and replaces the file whole
    /// ([`crate::config_marker::write_marker`]). A mark that cannot be read or is malformed is left
    /// as found, an error, as is a `build` no line could name.
    fn record(path: &std::path::Path, build: &str, opened: bool) -> std::io::Result<()> {
        use std::fmt::Write as _;
        if !Self::nameable(build) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "no mark line can name this build",
            ));
        }
        let text = Self::read(path)?;
        let mut builds = match text.as_deref() {
            None => Vec::new(),
            Some(text) => Self::builds(text).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the mark is malformed and is left as found",
                )
            })?,
        };
        let before = builds.len();
        builds.retain(|&kept| kept != build);
        if opened {
            builds.push(build);
        } else if builds.len() == before {
            return Ok(());
        }
        if builds.is_empty() {
            return match std::fs::remove_file(path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        let keep = builds.len().saturating_sub(MARK_BUILDS);
        let mut mark = String::new();
        for named in &builds[keep..] {
            let _ = writeln!(mark, "opened {named}");
        }
        crate::config_marker::write_marker(path, &mark, MARK_BYTES, crate::durable::Flush::Durable)
    }

    /// The tab landed: never by itself again on this build. That promise rests on this write
    /// ([`Self::record`]), so it is durable, its directory entry included; it runs once per
    /// account and build. A write that fails leaves the mark for this launch alone, and says so in
    /// the log. `true` when this landing set the mark, which [`Mark::forget`] can then take back.
    fn note_opened(&mut self) -> bool {
        if std::mem::replace(&mut self.opened, true) {
            return false;
        }
        if let Some(path) = &self.path
            && let Err(error) = Self::record(path, &aterm_types::version::build_id(), true)
        {
            aterm_log::warn!(
                "System State: the Command Line Tools tab's mark was not written: {error}"
            );
        }
        true
    }

    /// The opening that set the mark installed nothing, and no person declined it: Low Data Mode
    /// held the download back, or the install failed. This build's line goes ([`Self::record`];
    /// the file too, when it names no other build), so aterm opens the tab by itself again (once
    /// the mode is off, or at the next launch). A mark that cannot be rewritten or removed keeps
    /// this build for later launches, and says so in the log.
    fn forget(&mut self) {
        self.opened = false;
        if let Some(path) = &self.path
            && let Err(error) = Self::record(path, &aterm_types::version::build_id(), false)
        {
            aterm_log::warn!(
                "System State: the Command Line Tools tab's mark was not removed: {error}"
            );
        }
    }
}

/// A watch on whether this Mac's network is in Low Data Mode (Network.framework's path monitor),
/// which wakes the window at each reading ([`Wake::DeveloperToolsLowData`]) until it is dropped.
pub(crate) struct NetworkWatch {
    /// The monitor (none in a test's stand-in).
    #[cfg(target_os = "macos")]
    _monitor: Option<aterm_objc::PathMonitor>,
    /// When it started: the automatic tab waits briefly for its first reading.
    since: std::time::Instant,
}

/// How long the automatic tab waits for a new [`NetworkWatch`]'s first reading before it opens
/// without one; the monitor hands over the path in use at once on a Mac with a network.
const FIRST_READING: std::time::Duration = std::time::Duration::from_secs(3);

impl NetworkWatch {
    /// Start one; `None` where the monitor cannot be had (and off macOS), where the tab's program
    /// reads Low Data Mode itself.
    fn start() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            let monitor = aterm_objc::PathMonitor::start(c"aterm.clt.low-data", |reading| {
                let _ = crate::workstation_setup::send_setup_wake(Wake::DeveloperToolsLowData(
                    reading.constrained,
                ));
            })?;
            Some(Self {
                _monitor: Some(monitor),
                since: std::time::Instant::now(),
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    /// A watch that reads nothing, whose readings a test hands the window itself.
    #[cfg(all(test, unix))]
    pub(crate) fn stand_in() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            _monitor: None,
            since: std::time::Instant::now(),
        }
    }
}

/// What this Mac shows of an install of Apple's Command Line Tools now ([`probe`]): the on-demand
/// placeholder, a live `softwareupdate` or `installer`, and Apple's own installer app and its age.
pub(crate) fn installing_now() -> Reading {
    probe::installing_now()
}

/// Whether the compiler is installed, using atpkg's read-only presence check. This runs no
/// compiler or installer: repositories can work through packaged git while no compiler exists.
pub(crate) fn c_toolchain_installed() -> Option<bool> {
    atpkg::prereq::installed_with(
        std::env::var_os("PATH").as_deref(),
        &atpkg::prereq::CcHost::this_machine(),
    )
}

/// What this Mac shows of an install of Apple's Command Line Tools under way, by itself
/// ([`activity`]): the page reads it beside the tab aterm opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Activity {
    /// Nothing installs them: a placeholder left behind by an install that ended is no install.
    #[default]
    Idle,
    /// Apple's own installer window (`Install Command Line Developer Tools`, which
    /// `xcode-select --install` and the `git` and `cc` shims open) waits for the person's own
    /// Install or Agree press: they are needed there.
    AppleWindow,
    /// An install runs: the on-demand placeholder is there and something holds it,
    /// `softwareupdate` or `installer` (aterm's own root install, Homebrew's), Apple's installer
    /// app while it or the placeholder is younger than
    /// [`aterm_types::macos_setup::APPLE_WINDOW_INSTALL_BOUND`] (the app keeps the placeholder
    /// once its install starts), or a live root run
    /// ([`aterm_types::macos_setup::tools_install_running`], as `aterm init` reads it).
    Installing,
}

/// What [`activity`] reads of this Mac at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reading {
    /// The on-demand placeholder ([`aterm_types::macos_setup::COMMAND_LINE_TOOLS_INSTALLING`]) is
    /// there.
    pub(crate) placeholder: bool,
    /// Apple's `softwareupdate` or `installer` runs, by its exact path.
    pub(crate) install_process: bool,
    /// Apple's `Install Command Line Developer Tools` app runs.
    pub(crate) apple_window: bool,
    /// One of those apps installs by its age or the placeholder's
    /// ([`aterm_types::macos_setup::ToolsInstaller::installs`]): it, or the placeholder, is
    /// younger than [`aterm_types::macos_setup::APPLE_WINDOW_INSTALL_BOUND`] or of an age that
    /// cannot be read. Beside the placeholder, that is the install a person started in its
    /// window, whose press created the placeholder, perhaps hours after the window opened.
    pub(crate) apple_window_young: bool,
    /// The placeholder carries the stamp of a run of aterm's root half that is alive now
    /// ([`aterm_types::macos_setup::placeholder_held_by_a_live_run`], the reader `aterm init`
    /// shares): between two steps, or waiting on Apple's installer window a person closed, with
    /// no installer process running this moment.
    pub(crate) live_run: bool,
}

/// [`Activity`] for `reading`: the placeholder counts as an install only while something that
/// installs holds it (an installer, Apple's window while it or the placeholder is younger than
/// [`aterm_types::macos_setup::APPLE_WINDOW_INSTALL_BOUND`], or a live run of aterm's root half
/// that stamped it), so a stale one nothing holds reads as no install, and Apple's window before
/// its install starts (no placeholder yet), or one left open past that bound beside a leftover
/// placeholder as old, waits for the person. `aterm init` reads the same.
#[must_use]
pub(crate) const fn activity(reading: Reading) -> Activity {
    if reading.placeholder
        && (reading.install_process || reading.apple_window_young || reading.live_run)
    {
        Activity::Installing
    } else if reading.apple_window {
        Activity::AppleWindow
    } else {
        Activity::Idle
    }
}

/// The owned installer child, including its retained result after an exit notification.
pub(crate) struct Terminal {
    session: u64,
    finished: bool,
    recovered: bool,
    receipt: Option<receipt::Receipt>,
    /// The installer was seen running while this tab's program lived: the program outliving it
    /// (reading the tools back, saying so) is no wait on the person.
    installer_seen: bool,
    /// How the program ended was read and acted on ([`App::developer_tools_install_ended`]).
    ended: bool,
    /// The window already wrote the tab's last line (its install never started), so its end
    /// adds none.
    told: bool,
    /// This tab's landing set the account's [`Mark`], which an opening that installed nothing
    /// for Low Data Mode takes back.
    spent_mark: bool,
}

impl Terminal {
    /// The tab `session`, its program running (`recovered`: adopted from an earlier aterm, with
    /// its `receipt`).
    const fn running(session: u64, recovered: bool, receipt: Option<receipt::Receipt>) -> Self {
        Self {
            session,
            finished: false,
            recovered,
            receipt,
            installer_seen: false,
            ended: false,
            told: false,
            spent_mark: false,
        }
    }
}

/// What the page says of the install beyond its tab and this Mac's installers.
#[derive(Debug, Default)]
pub(crate) struct Aftermath {
    /// The tab's program said the tools are installed ([`Settling`]).
    settling: Option<Settling>,
    /// Why the tab a person's Install… asked for did not open, until their next press.
    refused: Option<NotOpened>,
}

impl Aftermath {
    /// The GitHub repositories row read `reading` at `now`: a reading other than the one the
    /// tab's program ended beside, or [`SETTLE_BOUND`] passed, ends the calm [`Settling`] kept.
    pub(crate) fn settle(&mut self, reading: Option<&Observation>, now: std::time::Instant) {
        if self
            .settling
            .as_ref()
            .is_some_and(|settling| reading != settling.reading.as_ref() || now >= settling.until)
        {
            self.settling = None;
        }
    }
}

/// The tab's program said the tools are installed: the GitHub repositories row stays calm until
/// its next reading lands (one that differs from `reading`, the one it read when the program
/// ended), or [`SETTLE_BOUND`] passes.
#[derive(Debug)]
struct Settling {
    reading: Option<Observation>,
    until: std::time::Instant,
}

/// The tab's program for `exe`, this program's `aterm`: the install item alone ([`ARGS`]).
fn tab_command(exe: &std::path::Path) -> Vec<std::ffi::OsString> {
    tab_command_running(
        std::iter::once(exe.as_os_str().to_owned()).chain(ARGS.map(std::ffi::OsString::from)),
    )
}

/// `program` as the tab runs it: under `/usr/bin/env`, which adds the tab's variable, with no
/// shell between them.
fn tab_command_running(
    program: impl IntoIterator<Item = std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    let mut command = vec![
        std::ffi::OsString::from("/usr/bin/env"),
        format!("{}=1", aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB).into(),
    ];
    command.extend(program);
    command
}

/// This program's own `aterm`, which the tab runs; none off a Unix front door.
fn own_aterm() -> Option<std::path::PathBuf> {
    #[cfg(not(windows))]
    {
        crate::ledger_key::own_front_door()
    }
    #[cfg(windows)]
    {
        None
    }
}

/// The line typed into the tab in place of its go line when its install must not start: any
/// line but [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO`] ends the program at once,
/// having started and said nothing.
const TAB_STOP: &str = "stop";

/// The tab's last line when aterm could not save what it needs to follow the install, which
/// therefore never started.
fn not_started_line() -> String {
    format!(
        "{}: aterm could not save what it needs to keep track of it. {}",
        aterm_types::macos_setup::COMMAND_LINE_TOOLS_NOT_FINISHED,
        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TRY_AGAIN
    )
}

/// What a launch starts of the install ([`launch_install`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LaunchInstall {
    /// The watch for the tools' arrival ([`watch_for_tools`]).
    pub(crate) watch: bool,
    /// The want of the tools, whose tab opens by itself.
    pub(crate) want: bool,
}

impl LaunchInstall {
    /// Where the launch's install stands from the start: wanted as [`LAUNCH_WANT`], or unasked.
    pub(crate) const fn install(self) -> Install {
        if self.want {
            Install::Wanted(LAUNCH_WANT)
        } else {
            Install::Unasked
        }
    }

    /// The launch's want of the tools
    /// ([`crate::workstation_setup::Host::developer_tools_want`]).
    pub(crate) const fn want(self) -> Option<u64> {
        if self.want { Some(LAUNCH_WANT) } else { None }
    }
}

/// [`LaunchInstall`] for a launch with a `window` (not a headless instance) that reads this Mac
/// (`reads_mac`: not a test), with the tools `missing` and the account's [`Mark`] `opened` or not.
pub(crate) const fn launch_install(
    window: bool,
    reads_mac: bool,
    missing: bool,
    opened: bool,
) -> LaunchInstall {
    LaunchInstall {
        watch: reads_mac && missing,
        want: window && missing && !opened,
    }
}

/// Wake the window every second until the developer tools exist: their arrival by any route takes
/// the shim off new shells, and a launch's want gets its looks with no status poll to ride. Only
/// a Mac that lacks them runs it, and it reads with a stat and a readlink.
pub(crate) fn watch_for_tools() {
    let watch = std::thread::Builder::new()
        .name("aterm-clt-watch".to_owned())
        .spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let present = aterm_types::clt_shim::developer_git();
                let _ = crate::workstation_setup::send_setup_wake(Wake::DeveloperToolsWatch);
                if present {
                    return;
                }
            }
        });
    if let Err(error) = watch {
        aterm_log::warn!("System State: the Command Line Tools watch did not start: {error}");
    }
}

/// Tell the window whether the setup pass that just ended waits on Apple's Command Line Tools
/// (`wanted`), with that pass's exact `revision`: the setup run's hook
/// (`aterm_cli::macos_doctor::SetupHooks::developer_tools`), called from its worker.
pub fn offer_developer_tools_install(revision: u64, wanted: bool) {
    if !crate::workstation_setup::send_setup_wake(Wake::DeveloperToolsInstall(revision, wanted))
        && wanted
    {
        aterm_log::info!("System State: no window to install the Command Line Tools in");
    }
}

fn install_tab_then(revision: u64) -> crate::tab_spawn::TabSpawnThen {
    crate::tab_spawn::TabSpawnThen::Before {
        check: Box::new(move |app, _window| app.prepare_developer_tools_install_landing(revision)),
        then: Box::new(move |app, landed| {
            // A quiet refusal restored Wanted/Unasked before this callback. Only a real
            // opening (or its failure) consumes the launch's attempt.
            if matches!(
                app.workstation_setup.developer_tools,
                Install::Opening { .. }
            ) {
                app.workstation_setup.developer_tools = Install::Opened;
                match landed {
                    Ok(sid) => {
                        let spent = app.workstation_setup.developer_tools_mark.note_opened();
                        app.dispatch_developer_tools_install(revision, |app| {
                            app.start_developer_tools_install(&sid, spent);
                        });
                        // Only a real landing changes frontness. A refused spawn during
                        // shutdown must not republish the still-retained setup window.
                        app.publish_system_state_front();
                    }
                    Err(why) => aterm_log::warn!(
                        "System State: the Command Line Tools install tab did not open: {why}"
                    ),
                }
            }
            app.release_unused_access_dialog(std::time::Instant::now());
        }),
    }
}

/// [`install_tab_then`] for a person's press of Install…: its own landing check, and no run's
/// revision to answer to, since the press is the person's yes. A tab that did not start tells
/// them, as any tab they open does; one its check refused says why on the row
/// ([`App::prepare_pressed_install_landing`]).
fn pressed_tab_then() -> crate::tab_spawn::TabSpawnThen {
    crate::tab_spawn::TabSpawnThen::Before {
        check: Box::new(move |app, window| app.prepare_pressed_install_landing(window)),
        then: Box::new(move |app, landed| {
            if app.workstation_setup.developer_tools == Install::Pressed {
                app.workstation_setup.developer_tools = Install::Opened;
                match landed {
                    Ok(sid) => {
                        let spent = app.workstation_setup.developer_tools_mark.note_opened();
                        app.start_developer_tools_install(&sid, spent);
                        app.publish_system_state_front();
                    }
                    // Keys typed meanwhile stayed in System State (the tab spawns unasked, so
                    // none reaches its password prompt), which also keeps the spawn's own
                    // notice back: the press is the person's, so they hear it here.
                    Err(why) => {
                        aterm_log::warn!(
                            "System State: the Command Line Tools install tab did not open: {why}"
                        );
                        app.post_message(crate::message_reporters::new_tab_failed(&why));
                    }
                }
            }
            app.release_unused_access_dialog(std::time::Instant::now());
            app.republish_system_state_developer_tools();
        }),
    }
}

impl App {
    /// Start the install in its landed tab `sid`, recording the tab first: even an immediate
    /// failure's exit must release the slot and leave its explanation in the tab. The tab takes
    /// its fixed title, which the page names, and its program its go line, once the tab's
    /// recovery record is saved; without one the install never starts, and the tab says why.
    /// `spent`: its landing set the account's [`Mark`].
    fn start_developer_tools_install(&mut self, sid: &str, spent: bool) {
        let session = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(&aterm_session::SessionId::new(sid))
            .map(|handle| handle.local_id);
        let Some(session) = session else {
            return;
        };
        let Some(child) = self.pool.get(session) else {
            return;
        };
        let recovery = self
            .workstation_setup
            .developer_tools_mark
            .path
            .as_deref()
            .and_then(|mark| {
                (child.pid == child.shell_identity.pid && child.ctx.self_id.as_str() == sid)
                    .then(|| {
                        receipt::Receipt::for_child(
                            mark,
                            sid,
                            child.pid,
                            child.shell_identity.birth,
                        )
                    })
                    .flatten()
            });
        let recovery = match recovery {
            Some(receipt) => receipt.write_exclusive().map(|()| Some(receipt)),
            None if crate::keeper_link::on() => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the installer child has no verified recovery identity or state directory",
            )),
            None => Ok(None),
        };
        let (receipt, go) = match recovery {
            Ok(receipt) => (receipt, true),
            Err(error) => {
                aterm_log::warn!(
                    "System State: installer not started: could not save its recovery state: {error}"
                );
                (None, false)
            }
        };
        let mut terminal = Terminal::running(session, false, receipt);
        terminal.told = !go;
        terminal.spent_mark = spent;
        self.workstation_setup.developer_tools_attempt = Some(session);
        self.workstation_setup
            .developer_tools_session
            .push(terminal);
        if go {
            // An install starts: the shim's line says the tools are on their way again.
            self.workstation_setup.developer_tools_unfinished = false;
            self.settle_developer_tools_shim();
        }
        if let Err(error) = self.pin_developer_tools_title(session) {
            aterm_log::info!(
                "System State: the Command Line Tools tab kept its own title: {error:?}"
            );
        }
        if go {
            self.type_into_new_session(sid, aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO);
        } else {
            self.write_developer_tools_line(session, &not_started_line());
            self.type_into_new_session(sid, TAB_STOP);
        }
    }

    /// The tab's fixed title, [`TAB_TITLE`], as aterm's own pin.
    fn pin_developer_tools_title(
        &mut self,
        session: u64,
    ) -> Result<bool, crate::session_timeline::MetaWriteError> {
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            return Ok(false);
        };
        let changed = crate::session_timeline::write_session_meta(
            &ctx,
            crate::session_timeline::MetaField::Title,
            crate::session_timeline::MetaEdit::Set(TAB_TITLE),
            aterm_slog::Actor::Aterm,
        )?;
        if changed {
            self.refresh_meta_dependent_chrome(session);
        }
        Ok(changed)
    }

    /// Recover custody only for an adopted exact child with its own immutable record. This
    /// restores neither setup admission nor a command: adoption never types installer bytes.
    pub(crate) fn recover_developer_tools_install(&mut self, local: u64) {
        if self
            .workstation_setup
            .developer_tools_session
            .iter()
            .any(|terminal| terminal.session == local)
        {
            return;
        }
        let Some(child) = self.pool.get(local) else {
            return;
        };
        if child.handoff_local_id.is_none() || child.pid != child.shell_identity.pid {
            return;
        }
        let Some(receipt) = self
            .workstation_setup
            .developer_tools_mark
            .path
            .as_deref()
            .and_then(|mark| {
                receipt::Receipt::for_child(
                    mark,
                    child.ctx.self_id.as_str(),
                    child.pid,
                    child.shell_identity.birth,
                )
            })
            .filter(receipt::Receipt::matches)
        else {
            return;
        };
        let finished = child.child_exit.get().is_some()
            || child
                .child_reaped
                .load(std::sync::atomic::Ordering::Acquire)
            || child.pty_closed_at.get().is_some();
        let mut terminal = Terminal::running(local, true, Some(receipt));
        terminal.finished = finished;
        if !finished || self.workstation_setup.developer_tools_attempt.is_none() {
            self.workstation_setup.developer_tools_attempt = Some(local);
        }
        self.workstation_setup
            .developer_tools_session
            .push(terminal);
        self.workstation_setup.developer_tools = Install::Opened;
        if !finished {
            self.recover_access_dialog();
            // The earlier aterm's install runs on in its tab: its want comes with it (as the
            // oldest pass's, so any pass of this launch that no longer waits on the tools ends
            // it), and the shim's line says the tools are on their way, which this launch's
            // reconcile, with nothing installing them yet, had taken back.
            let setup = &mut self.workstation_setup;
            setup.developer_tools_unfinished = false;
            setup.developer_tools_want.get_or_insert(0);
            self.settle_developer_tools_want();
        }
    }

    /// Where an install of Apple's Command Line Tools stands for System State's page (design
    /// §3.3), with `status` the reading the page is about to show: an install running (or this
    /// tab's program outliving one it saw, or the tab saying the tools are installed while the
    /// row's next reading is still to come), else the tab asking for the password (or, restored
    /// from an earlier aterm, for whatever it asks), else Apple's own installer window waiting
    /// for the person, else a person's press queued behind a macOS dialog, else an account that
    /// may not install them, else Low Data Mode holding the download back, else why their last
    /// press opened nothing, else neither.
    pub(crate) fn developer_tools_install_step(
        &mut self,
        status: &crate::SetupStatus,
    ) -> Option<ToolsInstall> {
        self.read_developer_tools_exit();
        let reading = self.developer_tools_reading();
        let activity = activity(reading);
        let setup = &mut self.workstation_setup;
        setup
            .developer_tools_after
            .settle(status.repositories_reading(), std::time::Instant::now());
        // A program whose end is not read yet still counts as running: its exit status comes a
        // moment after its terminal closed. While it runs, Apple's own installer window is where
        // the person finishes (the program opens it where Apple's catalog lists no tools, and
        // asks them to press Install there), whatever placeholder is held.
        let mut tab = None;
        for terminal in setup
            .developer_tools_session
            .iter_mut()
            .filter(|terminal| !terminal.ended)
        {
            terminal.installer_seen |= activity == Activity::Installing;
            tab = tab.or(Some(if reading.apple_window {
                ToolsInstall::AppleWindow
            } else if terminal.installer_seen {
                ToolsInstall::Installing
            } else if terminal.recovered {
                ToolsInstall::Recovered
            } else {
                ToolsInstall::Password
            }));
        }
        if tab.is_some() {
            return tab;
        }
        if activity == Activity::Installing || setup.developer_tools_after.settling.is_some() {
            return Some(ToolsInstall::Installing);
        }
        if activity == Activity::AppleWindow {
            return Some(ToolsInstall::AppleWindow);
        }
        if matches!(setup.developer_tools, Install::PressedQueued { .. }) {
            return Some(ToolsInstall::Queued);
        }
        if setup.developer_tools_not_admin {
            return Some(ToolsInstall::NotAdmin);
        }
        if setup.developer_tools_low_data == Some(true) {
            return Some(ToolsInstall::LowData);
        }
        setup
            .developer_tools_after
            .refused
            .map(ToolsInstall::NotOpened)
    }

    /// A compiler installed outside this window ends its missing-tools exception without
    /// waiting for another package pass. A dismissed entry still needs that observation; one
    /// already resolved needs no more filesystem reads. Repository readiness is not compiler
    /// readiness: packaged git works without Apple's tools, and Linux packages them separately.
    pub(crate) fn settle_missing_tools_exception(&mut self) {
        use crate::toolchain_words::{CToolchain, c_toolchain_entry};
        let missing = c_toolchain_entry(CToolchain::Missing).key();
        if !self.has_unresolved_exception(&missing) {
            return;
        }
        if self
            .workstation_setup
            .developer_tools_installed
            .is_some_and(|read| read() == Some(true))
        {
            self.except_clear(&missing);
        }
    }

    /// What this Mac shows of an installer now ([`Reading`]): the same local evidence drives the
    /// displayed wait and each dispatch boundary.
    fn developer_tools_reading(&self) -> Reading {
        self.workstation_setup
            .developer_tools_probe
            .map_or_else(Reading::default, |read| read())
    }

    /// Whether an installer is up on this Mac, running or waiting in Apple's own window: no
    /// second one opens beside it.
    fn developer_tools_installing(&self) -> bool {
        activity(self.developer_tools_reading()) != Activity::Idle
    }

    /// A person's press of the GitHub repositories row's Install… (design §3.3): the tab opens
    /// now, whatever run is admitted and whether or not it opened on this Mac before, landing
    /// once System State is still the front view of its window and no macOS dialog this process
    /// started is up. When a macOS permission gesture holds the dialog slot, queues the press so
    /// the row explains why and opens the tab once the slot frees; a helper panel out of sight
    /// holds no press, giving way first (`App::press_meets_unseen_panel`). `true` when its shell
    /// started or queued, or the guard that panel freed started the run's own install tab;
    /// `false` starts nothing: a headless instance, an installer already running, a tab open,
    /// opening or queued, an account that may not install them (the row offers no Install…
    /// then), or no front page.
    pub(crate) fn press_developer_tools_install(&mut self) -> bool {
        if self.headless
            || self.developer_tools_install_active()
            || self.workstation_setup.developer_tools_not_admin
        {
            return false;
        }
        let wanted = matches!(self.workstation_setup.developer_tools, Install::Wanted(_));
        // A helper panel out of sight never holds the press: it gives way first. The guard it
        // frees can start the install the run wanted (`Self::tick_developer_tools_install`),
        // whose tab is then this press's.
        let _ = self.press_meets_unseen_panel(None, std::time::Instant::now());
        match self.workstation_setup.developer_tools {
            Install::Opening { .. } if wanted => return true,
            Install::Opening { .. } | Install::Pressed | Install::PressedQueued { .. } => {
                return false;
            }
            _ => {}
        }
        let Some(window) = self
            .windows
            .keys()
            .find(|id| self.system_state_frontmost_in(**id))
            .copied()
        else {
            return false;
        };
        if self.developer_tools_installing() {
            return false;
        }
        // A new press: the reason the last one opened nothing is spent.
        self.workstation_setup.developer_tools_after.refused = None;
        if self.access_dialog_up() {
            let wanted = match self.workstation_setup.developer_tools {
                Install::Wanted(revision) => Some(revision),
                _ => None,
            };
            self.workstation_setup.developer_tools = Install::PressedQueued { wanted };
            aterm_log::info!(
                "System State: Install… queued while a macOS permission gesture holds the dialog slot"
            );
            self.republish_system_state_developer_tools();
            return true;
        }
        let Some((ask, then)) = self.begin_pressed_install(own_aterm) else {
            return false;
        };
        self.open_off_loop(window, crate::tab_spawn::SpawnPlace::Tab, ask, then);
        true
    }

    /// How a person's pressed tab spawns and what it does once it lands, running this program's
    /// `aterm` (read only then), with the request now Pressed. Keys typed while it spawns stay in
    /// System State ([`crate::tab_spawn::SpawnAsk::unasked`]), so none reaches its password
    /// prompt; its landing tells the person if it did not start ([`pressed_tab_then`]). `None`
    /// starts nothing: a program that cannot name its `aterm` says so in the log.
    fn begin_pressed_install(
        &mut self,
        aterm: impl FnOnce() -> Option<std::path::PathBuf>,
    ) -> Option<(crate::tab_spawn::SpawnAsk, crate::tab_spawn::TabSpawnThen)> {
        let Some(exe) = aterm() else {
            aterm_log::warn!(
                "System State: this program cannot name itself; Install… opens no tab"
            );
            return None;
        };
        self.workstation_setup.developer_tools = Install::Pressed;
        aterm_log::info!(
            "System State: Install… starts the Command Line Tools tab (`aterm {}`)",
            ARGS.join(" ")
        );
        let ask = crate::tab_spawn::SpawnAsk {
            unasked: true,
            // The program waits for its go line, which only the landing types.
            exec_command: Some(tab_command(&exe)),
            ..crate::tab_spawn::SpawnAsk::default()
        };
        Some((ask, pressed_tab_then()))
    }

    /// The landing check of a pressed tab: System State still the front view of `window`, no
    /// installer up and the dialog slot free, which the child holds through its lifetime. A tab
    /// that cannot land opens nothing: a macOS permission gesture that took the dialog slot since
    /// the press keeps it queued; System State left behind says why on the row, with Install…
    /// there for another press; an installer that started meanwhile is the row's wait.
    fn prepare_pressed_install_landing(&mut self, window: crate::WindowId) -> bool {
        if self.workstation_setup.developer_tools != Install::Pressed {
            return false;
        }
        let front = self.system_state_frontmost_in(window);
        if front && !self.developer_tools_installing() {
            if !self.access_dialog_up() && self.reserve_access_dialog() {
                return true;
            }
            self.workstation_setup.developer_tools = Install::PressedQueued { wanted: None };
            aterm_log::info!(
                "System State: the Command Line Tools tab Install… asked for waits for the dialog slot"
            );
            self.republish_system_state_developer_tools();
            return false;
        }
        self.workstation_setup.developer_tools = Install::Opened;
        if !front {
            self.workstation_setup.developer_tools_after.refused = Some(NotOpened::LeftPage);
        }
        aterm_log::info!(
            "System State: the Command Line Tools tab Install… asked for did not land{}",
            if front {
                ": an installer is up"
            } else {
                ": System State was no longer in front"
            }
        );
        false
    }

    pub(crate) fn developer_tools_install_active(&self) -> bool {
        self.workstation_setup
            .developer_tools_session
            .iter()
            .any(|terminal| !terminal.finished)
    }

    /// The installer terminal `session` landed, as a pressed tab's landing leaves it, with no
    /// recovery receipt: it holds off other prompts until
    /// [`Self::finish_developer_tools_install`].
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn land_install_session_for_test(&mut self, session: u64) {
        self.workstation_setup.developer_tools_attempt = Some(session);
        self.workstation_setup
            .developer_tools_session
            .push(Terminal::running(session, false, None));
    }

    /// The tab's program ended, or its final view detached: only that ends its prompt
    /// ownership. Returning true also keeps its completed terminal available for reading the
    /// result, and how the program ended is acted on once it is read
    /// ([`Self::developer_tools_install_ended`]).
    pub(crate) fn finish_developer_tools_install(&mut self, session: u64) -> bool {
        let Some(terminal) = self
            .workstation_setup
            .developer_tools_session
            .iter_mut()
            .find(|terminal| terminal.session == session)
        else {
            return false;
        };
        terminal.finished = true;
        if let Some(receipt) = terminal.receipt.take() {
            receipt.remove_if_matching();
        }
        self.release_unused_access_dialog(std::time::Instant::now());
        self.read_developer_tools_exit();
        self.republish_system_state_developer_tools();
        true
    }

    /// Read how each finished tab's program ended, once: an exit seen before its status was
    /// collected is read again at the next step ([`Self::developer_tools_install_step`]). A tab
    /// restored from an earlier aterm whose program is not this one's child may never give its
    /// status: its end is read as unknown.
    fn read_developer_tools_exit(&mut self) {
        let unread: Vec<(u64, bool)> = self
            .workstation_setup
            .developer_tools_session
            .iter()
            .filter(|terminal| terminal.finished && !terminal.ended)
            .map(|terminal| (terminal.session, terminal.recovered))
            .collect();
        for (session, recovered) in unread {
            if self.pool.get(session).is_none() {
                // Its tab is gone with it: nothing to read, and nothing to write into.
                self.developer_tools_install_ended(session, None);
                continue;
            }
            match self.exit_status(session, crate::app_tabs::ExitLook::First) {
                Some(exit) => self.developer_tools_install_ended(session, Some(exit)),
                None if recovered => self.developer_tools_install_ended(session, None),
                None => {}
            }
        }
    }

    /// The tab `session`'s program ended with `exit` (`None`: unknown). Exit 0 said the tools
    /// are installed: the GitHub repositories row is read again at once, the run waiting on it
    /// wakes, the row stays calm until that reading lands ([`Settling`]), and the shim comes off
    /// new shells.
    /// Exit 1 said why the install did not finish, and takes the account's [`Mark`] back, so the
    /// next launch asks again. A stop's exit said a person stopped it, and keeps the mark, as a
    /// closed tab does. Low Data Mode's exit takes the opening back (the mark included) and
    /// waits, calmly, for the mode to end; a non-administrator's leaves the row saying so, with
    /// no Install…. Any other end said nothing, and the tab ends on the window's line unless the
    /// window already wrote one. Each but the last two leaves the row to Install… again. Only the
    /// current attempt changes that state: an older terminal's delayed end keeps its own result.
    /// A current end but 0 and Low Data Mode's leaves nothing installing the tools: the shim stays
    /// on new shells, and its line says they are missing rather than on their way until an
    /// install starts again ([`Self::settle_developer_tools_shim`]).
    pub(crate) fn developer_tools_install_ended(
        &mut self,
        session: u64,
        exit: Option<aterm_pty::ChildExit>,
    ) {
        let Some(terminal) = self
            .workstation_setup
            .developer_tools_session
            .iter_mut()
            .find(|terminal| terminal.session == session && !terminal.ended)
        else {
            return;
        };
        terminal.finished = true;
        terminal.ended = true;
        let told = terminal.told;
        let spent_mark = terminal.spent_mark;
        // EOF can precede the child's status. A person may start a retry meanwhile: retain the
        // old terminal's result, but only the current attempt changes the retry's state or shim.
        let current = self.workstation_setup.developer_tools_attempt == Some(session);
        match exit {
            Some(aterm_pty::ChildExit::Code(0)) => {
                aterm_log::info!("System State: the Command Line Tools tab installed them");
                if current {
                    self.workstation_setup.developer_tools_after.settling = Some(Settling {
                        reading: self
                            .system_state_host
                            .latest_status()
                            .repositories_reading()
                            .cloned(),
                        until: std::time::Instant::now() + SETTLE_BOUND,
                    });
                    if let Some(handlers) = self.system_state_handlers() {
                        (handlers.tools_installed)();
                    }
                    // The tools arrived: the shim comes off new shells now, ahead of the next
                    // pass's word.
                    self.settle_developer_tools_want();
                }
            }
            Some(aterm_pty::ChildExit::Code(1)) => {
                aterm_log::info!("System State: the Command Line Tools tab said it did not finish");
                // The program ran and failed (a wrong password, a download that broke), which is
                // no person's closing the tab: the next launch asks again by itself.
                if current {
                    let setup = &mut self.workstation_setup;
                    setup.developer_tools_unfinished = true;
                    setup.developer_tools_mark.forget();
                }
            }
            Some(aterm_pty::ChildExit::Code(code))
                if code == i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED) =>
            {
                aterm_log::info!("System State: the Command Line Tools tab was stopped");
                // A person declined (a Ctrl-C at the password prompt or later): the opening stays
                // spent, as for a closed tab, and Install… is the way on.
                if current {
                    self.workstation_setup.developer_tools_unfinished = true;
                }
            }
            Some(aterm_pty::ChildExit::Code(code))
                if code == i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA) =>
            {
                aterm_log::info!(
                    "System State: the Command Line Tools tab waits for Low Data Mode"
                );
                // That opening installed nothing: it is not spent. A pass that still wants the
                // tools opens the tab by itself once the mode is off.
                if current {
                    let setup = &mut self.workstation_setup;
                    if spent_mark {
                        setup.developer_tools_mark.forget();
                    }
                    if setup.developer_tools == Install::Opened {
                        setup.developer_tools = setup
                            .developer_tools_want
                            .map_or(Install::Unasked, Install::Wanted);
                    }
                    setup.developer_tools_low_data = Some(true);
                    if setup.developer_tools_reads_mac && setup.developer_tools_network.is_none() {
                        setup.developer_tools_network = NetworkWatch::start();
                    }
                }
            }
            Some(aterm_pty::ChildExit::Code(code))
                if code
                    == i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN) =>
            {
                aterm_log::info!(
                    "System State: the Command Line Tools tab found an account that may not install them"
                );
                if current {
                    self.workstation_setup.developer_tools_not_admin = true;
                }
            }
            Some(other) => {
                aterm_log::info!("System State: the Command Line Tools tab ended: {other:?}");
                if current {
                    self.workstation_setup.developer_tools_unfinished = true;
                }
                if !told {
                    let line = format!(
                        "{}. {}",
                        aterm_types::macos_setup::COMMAND_LINE_TOOLS_NOT_FINISHED,
                        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TRY_AGAIN
                    );
                    self.write_developer_tools_line(session, &line);
                }
            }
            None if current => self.workstation_setup.developer_tools_unfinished = true,
            None => {}
        }
        if current {
            self.settle_developer_tools_shim();
        }
    }

    /// Write `line` into the tab `session` as its last line, as a shell that failed at start
    /// keeps its own.
    fn write_developer_tools_line(&mut self, session: u64, line: &str) {
        if let Some(pooled) = self.pool.get(session) {
            let mut term = crate::term_lock(&pooled.term);
            let lead = if term.cursor().col == 0 { "" } else { "\r\n" };
            term.process(format!("{lead}{line}\r\n").as_bytes());
        }
        let _ = self.admit_output_redraws(session, std::time::Instant::now());
    }

    pub(crate) fn retire_developer_tools_install(&mut self, session: u64) {
        if self.finish_developer_tools_install(session) {
            self.workstation_setup
                .developer_tools_session
                .retain(|terminal| terminal.session != session);
        }
    }

    /// The window and tab of the Command Line Tools tab, while it is open (its program still
    /// running first): what the headline's Go to tab selects.
    pub(crate) fn developer_tools_tab(&self) -> Option<(crate::WindowId, usize)> {
        let mut sessions: Vec<&Terminal> = self
            .workstation_setup
            .developer_tools_session
            .iter()
            .collect();
        sessions.sort_by_key(|terminal| terminal.finished);
        sessions.into_iter().find_map(|terminal| {
            self.windows.iter().find_map(|(&window, state)| {
                state
                    .tab_set
                    .tabs()
                    .iter()
                    .position(|tab| {
                        tab.root.leaves().into_iter().any(|view| {
                            self.view_store
                                .get(view)
                                .copied()
                                .and_then(crate::tab_model::View::terminal_session)
                                == Some(terminal.session)
                        })
                    })
                    .map(|index| (window, index))
            })
        })
    }

    /// The headline's Go to tab: the Command Line Tools tab comes to the front of its window,
    /// and that window to the front. `false` when it is not open.
    pub(crate) fn go_to_developer_tools_tab(&mut self) -> bool {
        let Some((window, index)) = self.developer_tools_tab() else {
            return false;
        };
        self.switch_tab_in(window, index);
        self.raise_window(window);
        true
    }

    /// The watch's second, and the launch's first look: what the want keeps
    /// ([`Self::settle_developer_tools_want`]: the account's reading, the Low Data Mode watch, the
    /// shim, which comes off new shells once the tools exist, however they arrived), and a want
    /// waiting on a quiet keyboard or a free dialog slot looks again. A launch that found the tools
    /// missing wants them from its setup host's start
    /// ([`crate::workstation_setup::Host::for_launch_marked`]).
    pub(crate) fn developer_tools_watch(&mut self) {
        self.settle_developer_tools_want();
        self.tick_developer_tools_install();
    }

    /// A setup pass ended waiting on the Command Line Tools (`wanted`), or not
    /// ([`offer_developer_tools_install`]).
    pub(crate) fn want_developer_tools_install(&mut self, revision: u64, wanted: bool) {
        let install = &mut self.workstation_setup.developer_tools;
        *install = if wanted {
            install.asked(revision)
        } else {
            install.withdrawn_by(revision)
        };
        // The want outlives the tab's opening ([`Install::Opened`]): only a later pass that no
        // longer waits on the tools ends it, never an older one.
        let want = &mut self.workstation_setup.developer_tools_want;
        *want = if wanted {
            Some(want.map_or(revision, |previous| previous.max(revision)))
        } else {
            want.filter(|previous| *previous > revision)
        };
        self.settle_developer_tools_want();
        self.tick_developer_tools_install();
    }

    /// No pass wants the Command Line Tools any more: a person's Stop, or a plan that leaves the
    /// GitHub repositories out (the Developer group turned off).
    pub(crate) fn withdraw_developer_tools_want(&mut self) {
        self.workstation_setup.developer_tools_want = None;
        self.settle_developer_tools_want();
    }

    /// Settle what a pass's want of the Command Line Tools keeps. While a pass wants them the
    /// window reads, once, whether this account may install them, and watches for Low Data Mode,
    /// which holds the download back; the watch ends with the want, unless the mode still holds
    /// the row's wait. Then the shim follows ([`Self::settle_developer_tools_shim`]).
    pub(crate) fn settle_developer_tools_want(&mut self) {
        let setup = &mut self.workstation_setup;
        let wanted = setup.developer_tools_want.is_some();
        if setup.developer_tools_reads_mac {
            if wanted {
                if setup.developer_tools_network.is_none() {
                    setup.developer_tools_network = NetworkWatch::start();
                }
                // The account's groups read only on a Unix host; elsewhere no Mac's tools are due.
                #[cfg(unix)]
                let admin = aterm_uds::account::administrator();
                #[cfg(not(unix))]
                let admin = None;
                if admin == Some(false) {
                    setup.developer_tools_not_admin = true;
                }
            } else if setup.developer_tools_low_data != Some(true) {
                setup.developer_tools_network = None;
                setup.developer_tools_low_data = None;
            }
        }
        self.settle_developer_tools_shim();
    }

    /// The git-and-friends shim stands in front of `/usr/bin` for the shells aterm spawns from the
    /// first launch until the Command Line Tools exist, whatever becomes of an install (design
    /// §6.3), and this settles it: it comes off new shells once they exist, and its line says they
    /// are on their way ([`Self::developer_tools_on_their_way`]). Its directory stays for the
    /// shells that have it.
    pub(crate) fn settle_developer_tools_shim(&self) {
        self.workstation_setup
            .developer_tools_shim
            .settle(self.developer_tools_on_their_way());
    }

    /// Whether an install of the tools is on its way: this account may install them, no install
    /// of them ended unfinished since one last started
    /// ([`crate::workstation_setup::Host::developer_tools_unfinished`]), and either an install
    /// tab's program runs or a want stands that may open one ([`Self::developer_tools_install_authorized`]):
    /// a launch's want under a hold or a durable Stop, or a pass's its run no longer admits,
    /// installs nothing.
    fn developer_tools_on_their_way(&self) -> bool {
        let setup = &self.workstation_setup;
        !setup.developer_tools_not_admin
            && !setup.developer_tools_unfinished
            && (self.developer_tools_install_active()
                || setup
                    .developer_tools_want
                    .is_some_and(|revision| self.developer_tools_install_authorized(revision)))
    }

    /// The network watch read Low Data Mode `on` or off. Off, the automatic tab may open (its
    /// opening unspent), and the watch ends unless a pass still wants the tools.
    pub(crate) fn developer_tools_low_data(&mut self, on: bool) {
        let setup = &mut self.workstation_setup;
        // A reading the monitor submitted before its watch ended is moot.
        if setup.developer_tools_network.is_none() {
            return;
        }
        let was = setup.developer_tools_low_data.replace(on);
        if !on && setup.developer_tools_want.is_none() {
            setup.developer_tools_network = None;
            setup.developer_tools_low_data = None;
        }
        if was != Some(on) {
            aterm_log::info!(
                "System State: Low Data Mode is {} for the Command Line Tools",
                if on { "on" } else { "off" }
            );
            self.republish_system_state_developer_tools();
        }
        self.tick_developer_tools_install();
    }

    /// Whether the automatic tab waits on the network: Low Data Mode holds the download back, or
    /// a watch just started has not read it yet ([`FIRST_READING`]).
    fn developer_tools_wait_network(&self) -> bool {
        let setup = &self.workstation_setup;
        match setup.developer_tools_low_data {
            Some(on) => on,
            None => setup
                .developer_tools_network
                .as_ref()
                .is_some_and(|watch| watch.since.elapsed() < FIRST_READING),
        }
    }

    /// [`Install::due`] for a want still authorized, once the keyboard was quiet for 2.5 s
    /// ([`crate::hands_off_keys`]): the landed tab takes the window's keys. Never once the tab has
    /// opened on this build ([`Mark`]): Install… is the way then. A live installer, Low Data Mode
    /// ([`Self::developer_tools_wait_network`]) and an account that is none of the Mac's
    /// administrators hold the request, without consuming the automatic attempt.
    fn developer_tools_install_due(&self, dialog_up: bool) -> bool {
        !self.workstation_setup.developer_tools_mark.opened()
            && !self.workstation_setup.developer_tools_not_admin
            && !self.developer_tools_wait_network()
            && crate::hands_off_keys(true, self.last_keystroke_at, std::time::Instant::now())
            && matches!(self.workstation_setup.developer_tools, Install::Wanted(revision)
            if self.developer_tools_install_authorized(revision))
            && self.workstation_setup.developer_tools.due(dialog_up)
            && !self.developer_tools_installing()
    }

    /// A launch's want stands on nothing but no hold and no Stop kept in the run's records: it is
    /// the owner's standing wish on a Mac without the tools. A pass's stands on its run's admission.
    fn developer_tools_install_authorized(&self, revision: u64) -> bool {
        let status = self.system_state_host.latest_status();
        if revision == LAUNCH_WANT {
            let stopped = status.finished && status.run.is_some_and(|run| !run.allowed);
            return self.workstation_setup.setup_unheld() && !stopped;
        }
        self.person_permission_admission_stands()
            && self.workstation_setup.setup_unheld()
            && !status.changing
            && status
                .run
                .is_some_and(|run| run.allowed && run.revision == revision)
    }

    /// Recheck before registration changes the active tab, reserving the worker's dialog
    /// slot through the installer child's lifetime. A key typed while the shell started
    /// defers it too; a newer want survives a refused shell.
    fn prepare_developer_tools_install_landing(&mut self, revision: u64) -> bool {
        let Install::Opening { wanted } = self.workstation_setup.developer_tools else {
            return false;
        };
        if wanted == Some(revision)
            && crate::hands_off_keys(true, self.last_keystroke_at, std::time::Instant::now())
            && self.developer_tools_install_authorized(revision)
            && !self.access_dialog_up()
            && !self.developer_tools_installing()
            && self.reserve_access_dialog()
        {
            return true;
        }
        self.workstation_setup.developer_tools = wanted.map_or(Install::Unasked, Install::Wanted);
        aterm_log::info!(
            "System State: Command Line Tools install {}",
            if wanted.is_some() {
                "deferred"
            } else {
                "withdrawn"
            }
        );
        false
    }

    /// A tab may land after Stop or a newer choice. Its old command then stays untyped.
    fn dispatch_developer_tools_install(&mut self, revision: u64, start: impl FnOnce(&mut Self)) {
        if self.developer_tools_install_authorized(revision) {
            start(self);
        }
    }

    /// Open the install's tab if it is due ([`Install::due`]), or a person's queued press once
    /// it may go ([`Install::PressedQueued`]); else it waits for the next status, which comes
    /// each second while a window is up.
    pub(crate) fn tick_developer_tools_install(&mut self) {
        if self.headless {
            return;
        }
        let Some((window, ask, then)) = self.begin_developer_tools_install(own_aterm) else {
            return;
        };
        aterm_log::info!(
            "System State: starting the Command Line Tools install tab (`aterm {}`)",
            ARGS.join(" ")
        );
        self.open_off_loop(window, crate::tab_spawn::SpawnPlace::Tab, ask, then);
    }

    /// When the install is due, the window its tab opens in, how it spawns and what it does once
    /// it lands (running this program's `aterm`, read only then), with the request now Opening.
    /// A program that cannot name its `aterm` spends the launch's attempt and opens nothing. A
    /// person's queued press goes first, once nothing holds it, as [`Self::begin_pressed_install`]
    /// starts it; tools that arrived meanwhile let it go.
    fn begin_developer_tools_install(
        &mut self,
        aterm: impl FnOnce() -> Option<std::path::PathBuf>,
    ) -> Option<(
        crate::WindowId,
        crate::tab_spawn::SpawnAsk,
        crate::tab_spawn::TabSpawnThen,
    )> {
        if let Install::PressedQueued { wanted } = self.workstation_setup.developer_tools {
            // The tools arrived while the press waited: the GitHub repositories row reads again
            // and no longer needs them. A page with no reading of that row yet keeps the press.
            let latest = self.system_state_host.latest_status();
            if latest.repositories_reading().is_some() && latest.repositories_tools_wait().is_none()
            {
                self.workstation_setup.developer_tools =
                    wanted.map_or(Install::Unasked, Install::Wanted);
                self.republish_system_state_developer_tools();
                return None;
            }
            // A queued press is no longer this moment's, and its tab takes the window's keys:
            // it also waits for 2.5 s of quiet keyboard, as the automatic tab does.
            if self.developer_tools_install_active()
                || self.access_dialog_up()
                || self.developer_tools_installing()
                || !crate::hands_off_keys(true, self.last_keystroke_at, std::time::Instant::now())
            {
                return None;
            }
            let window = self
                .windows
                .keys()
                .find(|id| self.system_state_frontmost_in(**id))
                .copied()?;
            self.workstation_setup.developer_tools = Install::Opened;
            let started = self.begin_pressed_install(aterm);
            self.republish_system_state_developer_tools();
            let (ask, then) = started?;
            return Some((window, ask, then));
        }
        if !self.developer_tools_install_due(self.access_dialog_up()) {
            return None;
        }
        // Whichever window is frontmost, focused or not, whatever it shows.
        let window = self
            .frontmost_window
            .filter(|id| self.windows.contains_key(id))
            .or_else(|| self.windows.keys().next().copied())?;
        let Install::Wanted(revision) = self.workstation_setup.developer_tools else {
            return None;
        };
        self.workstation_setup.developer_tools = Install::Opened;
        let Some(exe) = aterm() else {
            aterm_log::warn!(
                "System State: this program cannot name itself; the Command Line Tools install \
                 opens no tab"
            );
            return None;
        };
        self.workstation_setup.developer_tools = Install::Opening {
            wanted: Some(revision),
        };
        let ask = crate::tab_spawn::SpawnAsk {
            unasked: true,
            // The program waits for its go line, which only the landing check sends: nothing
            // installs before it, and no shell, rc hook or global `-e` runs in between.
            exec_command: Some(tab_command(&exe)),
            ..crate::tab_spawn::SpawnAsk::default()
        };
        Some((window, ask, install_tab_then(revision)))
    }
}

#[cfg(all(test, unix))]
#[path = "clt_install/landing_tests.rs"]
mod landing_tests;

#[cfg(all(test, unix))]
#[path = "clt_install/child_tests.rs"]
mod child_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unattended_setup_cannot_open_a_password_install_tab() {
        let mut app = App::headless_for_test();
        let permissions = Some(crate::InitialSetupPermissionPolicy {
            revision: 4,
            allowed: true,
        });
        app.observe_initial_setup_permissions(permissions);
        app.publish_system_state(crate::SetupStatus {
            plan: Some(aterm_types::macos_setup::Plan::default()),
            permissions,
            run: Some(crate::InitialSetupRunPolicy {
                revision: 4,
                allowed: true,
            }),
            ..crate::SetupStatus::default()
        });
        app.want_developer_tools_install(4, true);
        assert!(!app.developer_tools_install_due(false));
        app.note_system_state_touch();
        app.note_system_state_apply(crate::system_state::Admission::Person);
        app.accepted_initial_setup_apply();
        app.observe_initial_setup_permissions(permissions);
        assert!(
            app.developer_tools_install_due(false),
            "explicit admission is the negative control"
        );
    }

    fn policy(revision: u64) -> Option<crate::InitialSetupPermissionPolicy> {
        Some(crate::InitialSetupPermissionPolicy {
            revision,
            allowed: true,
        })
    }

    fn observe_run(
        app: &mut App,
        revision: u64,
        allowed: bool,
        permissions: Option<crate::InitialSetupPermissionPolicy>,
    ) {
        let mut status = app.system_state_host.latest_status().clone();
        status
            .plan
            .get_or_insert_with(aterm_types::macos_setup::Plan::default);
        status.permissions = permissions;
        status.run = Some(crate::InitialSetupRunPolicy { revision, allowed });
        if allowed {
            app.note_system_state_touch();
            app.note_system_state_apply(crate::system_state::Admission::Person);
            app.accepted_initial_setup_apply();
        }
        app.observe_initial_setup_permissions(permissions);
        app.publish_system_state(status);
    }

    fn stopped() -> App {
        let mut app = App::headless_for_test();
        observe_run(&mut app, 4, true, policy(4));
        app.stop_initial_setup_permissions();
        app
    }

    fn accept_later_apply(app: &mut App) {
        use crate::native_app::{ActionInvocation, AppEvent};
        use crate::native_ui::ActionId;
        use crate::workstation_setup::{Request, WorkstationSetupDecision};
        use aterm_types::macos_setup::{Plan, Selection, Setting};

        let wid = crate::WindowId(0);
        app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
        app.set_system_state_handlers_for_test(crate::InitialSetupHandlers::inert_for_test());
        let mut plan = Plan {
            selection: Selection::none(),
            ..Plan::default()
        };
        plan.selection.set(Setting::Repositories, true);
        let (send, receive) = std::sync::mpsc::channel();
        let request = Request::new(plan, send);
        app.headless = false;
        app.offer_system_state(request.clone());
        app.headless = true;
        assert!(request.pending());
        let (_, view) = app.active_native_view(wid).unwrap();
        app.with_gesture_source(false, |app| {
            app.interact_system_state(wid);
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    // A press on a group answers the offer.
                    id: ActionId::new("system-state/group/1/leave"),
                    value: None,
                }),
            )
        })
        .unwrap();
        let mut answer = receive.try_recv().unwrap();
        assert!(matches!(
            answer.decision,
            WorkstationSetupDecision::Begin(_)
        ));
        answer.receipt.take().unwrap().accept();
        app.resolve_system_state_apply();
    }

    #[test]
    fn clt_install_a_pass_withdraws_its_own_revisions_want_and_never_a_newer_one() {
        assert_eq!(Install::Wanted(4).withdrawn_by(4), Install::Unasked);
        assert_eq!(Install::Wanted(4).withdrawn_by(5), Install::Unasked);
        assert_eq!(Install::Wanted(5).withdrawn_by(4), Install::Wanted(5));
        let opening = |wanted| Install::Opening { wanted };
        assert_eq!(opening(Some(4)).withdrawn_by(4), opening(None));
        assert_eq!(opening(Some(5)).withdrawn_by(4), opening(Some(5)));
        assert_eq!(Install::Opened.withdrawn_by(9), Install::Opened);
    }

    #[test]
    fn clt_install_a_delayed_want_cannot_rearm_a_stopped_run() {
        let mut app = stopped();
        app.want_developer_tools_install(4, true);
        assert!(
            !app.developer_tools_install_due(false),
            "a delayed pass callback must not reopen installation after Stop"
        );
    }

    /// A person typing (a vault password into System State, say) holds the tab back until the
    /// keyboard was quiet for 2.5 s, so the tab that takes the window's keys catches none of it.
    #[test]
    fn clt_install_the_tab_waits_out_a_typing_person() {
        let mut app = App::headless_for_test();
        observe_run(&mut app, 4, true, policy(4));
        app.want_developer_tools_install(4, true);
        assert!(app.developer_tools_install_due(false));
        let now = std::time::Instant::now();
        app.last_keystroke_at = Some(now);
        assert!(
            !app.developer_tools_install_due(false),
            "a key typed just now holds the tab"
        );
        app.last_keystroke_at = now.checked_sub(std::time::Duration::from_secs(3));
        assert!(
            app.developer_tools_install_due(false),
            "a quiet keyboard lets it open"
        );
    }

    #[test]
    fn clt_install_a_run_can_keep_an_older_permissions_approval() {
        let mut app = App::headless_for_test();
        let permissions = Some(crate::InitialSetupPermissionPolicy {
            revision: 4,
            allowed: true,
        });
        app.note_system_state_touch();
        app.note_system_state_apply(crate::system_state::Admission::Person);
        app.accepted_initial_setup_apply();
        app.observe_initial_setup_permissions(permissions);
        app.publish_system_state(crate::SetupStatus {
            plan: Some(aterm_types::macos_setup::Plan::default()),
            permissions,
            run: Some(crate::InitialSetupRunPolicy {
                revision: 5,
                allowed: true,
            }),
            ..crate::SetupStatus::default()
        });
        app.want_developer_tools_install(5, true);
        assert!(
            app.developer_tools_install_due(false),
            "the current run's repository tools are independent of its carried Permissions approval"
        );
    }

    #[test]
    fn clt_install_a_durable_stop_does_not_reopen_tools_without_a_local_stop() {
        let mut app = App::headless_for_test();
        app.observe_initial_setup_permissions(policy(4));
        app.publish_system_state(crate::SetupStatus {
            plan: Some(aterm_types::macos_setup::Plan::default()),
            permissions: policy(4),
            run: Some(crate::InitialSetupRunPolicy {
                revision: 4,
                allowed: false,
            }),
            finished: true,
            ..crate::SetupStatus::default()
        });
        app.want_developer_tools_install(4, true);
        assert!(
            !app.developer_tools_install_due(false),
            "a Stop read from durable records withdraws installation even when another process asked for it"
        );
    }

    #[test]
    fn clt_install_a_new_apply_only_rearms_its_own_repository_request() {
        let mut app = stopped();
        accept_later_apply(&mut app);
        app.want_developer_tools_install(5, true);
        assert!(
            !app.developer_tools_install_due(false),
            "the new admission still needs its fresh policy"
        );
        observe_run(&mut app, 5, true, policy(5));
        assert!(
            app.developer_tools_install_due(false),
            "Repositories may install tools with Permissions left as is"
        );
        app.want_developer_tools_install(4, false);
        assert!(
            app.developer_tools_install_due(false),
            "an older pass cannot withdraw the new run's request"
        );
        app.want_developer_tools_install(4, true);
        assert!(
            app.developer_tools_install_due(false),
            "an older pass cannot replace the new run's request"
        );
        assert_eq!(app.workstation_setup.developer_tools, Install::Wanted(5));
        observe_run(&mut app, 4, false, policy(4));
        assert!(
            !app.developer_tools_install_due(false),
            "an older denied run policy cannot authorize the current run's install"
        );
        observe_run(&mut app, 5, true, policy(5));
        assert!(app.developer_tools_install_due(false));
        app.want_developer_tools_install(6, false);
        assert!(
            !app.developer_tools_install_due(false),
            "a newer pass that no longer waits withdraws the older want"
        );
    }

    #[test]
    fn clt_install_a_stopped_or_replaced_tab_never_starts_the_install_command() {
        for replaced in [false, true] {
            let mut app = App::headless_for_test();
            observe_run(&mut app, 4, true, policy(4));
            app.workstation_setup.developer_tools = Install::Opened;
            let mut commands = 0;
            app.dispatch_developer_tools_install(4, |_| commands += 1);
            assert_eq!(commands, 1, "the live install has an inert command sink");
            app.stop_initial_setup_permissions();
            if replaced {
                accept_later_apply(&mut app);
                observe_run(&mut app, 5, true, policy(5));
            }
            app.dispatch_developer_tools_install(4, |_| commands += 1);
            assert_eq!(
                commands, 1,
                "Stop or a newer accepted plan retires the old tab's command"
            );
            app.want_developer_tools_install(5, true);
            assert_eq!(
                app.workstation_setup.developer_tools,
                Install::Opened,
                "an opened tab retains the once-per-launch bound"
            );
        }
    }

    #[test]
    fn clt_install_a_changing_read_withholds_only_run_eligibility_until_a_settled_read() {
        let mut app = App::headless_for_test();
        let status = crate::SetupStatus {
            plan: Some(aterm_types::macos_setup::Plan::default()),
            permissions: policy(4),
            run: Some(crate::InitialSetupRunPolicy {
                revision: 5,
                allowed: true,
            }),
            ..crate::SetupStatus::default()
        };
        app.note_system_state_touch();
        app.note_system_state_apply(crate::system_state::Admission::Person);
        app.accepted_initial_setup_apply();
        app.receive_system_state_status(Some(0), status.clone());
        app.want_developer_tools_install(5, true);
        assert!(app.developer_tools_install_due(false));
        let changing = crate::SetupStatus {
            changing: true,
            ..crate::SetupStatus::default()
        };
        app.receive_system_state_status(Some(1), changing.clone());
        assert!(
            app.developer_tools_install_due(false),
            "a stale status generation cannot withdraw the current run"
        );
        app.receive_system_state_status(Some(0), changing);
        assert!(!app.developer_tools_install_due(false));
        let mut commands = 0;
        app.dispatch_developer_tools_install(5, |_| commands += 1);
        assert_eq!(
            commands, 0,
            "a tab landing during the unknown read stays untyped"
        );
        assert_eq!(app.system_state_host.latest_status().plan, status.plan);
        assert_eq!(
            app.system_state_host.latest_status().permissions,
            status.permissions
        );
        app.receive_system_state_status(Some(0), status);
        assert!(app.developer_tools_install_due(false));
        app.dispatch_developer_tools_install(5, |_| commands += 1);
        assert_eq!(
            commands, 1,
            "the settled current run restores the same deferred work"
        );
    }

    /// A launch's or a pass's ask makes the install wanted; its tab opens unless a macOS dialog is
    /// up, whatever the window shows, and never twice in a launch. A Stop drops a want not yet
    /// opened, and a pass's later word never drops a launch's.
    #[test]
    fn the_install_tab_opens_once_unless_a_dialog_is_up() {
        let wanted = Install::default().asked(4);
        assert_eq!(wanted, Install::Wanted(4));
        assert_eq!(wanted.asked(4), Install::Wanted(4));
        assert!(wanted.due(false));
        assert!(!wanted.due(true), "a macOS dialog is up");
        assert!(!Install::Unasked.due(false), "nobody asked");
        assert_eq!(
            wanted.withdrawn(),
            Install::Unasked,
            "withdrawn before it opened"
        );
        assert_eq!(
            Install::Opened.asked(4),
            Install::Opened,
            "never twice a launch"
        );
        assert_eq!(Install::Opened.withdrawn(), Install::Opened);
        assert!(!Install::Opened.due(false));

        let launch = Install::Unasked.asked(LAUNCH_WANT);
        assert_eq!(launch.asked(9), launch, "a pass replaces no launch's want");
        assert_eq!(launch.withdrawn_by(9), launch, "nor withdraws it");
        assert_eq!(launch.withdrawn(), Install::Unasked, "a Stop does");
    }

    /// A launch's want asks nobody first: no run, no person's admission, no page, as a new Mac's
    /// one password comes first. A hold and a Stop kept in the run's records still stand.
    /// NEGATIVE CONTROL: a pass's own want, on the same unadmitted app, stays held
    /// (`unattended_setup_cannot_open_a_password_install_tab`).
    #[test]
    fn a_launchs_want_opens_with_no_run_and_no_person() {
        let mut app = App::headless_for_test();
        app.want_developer_tools_install(LAUNCH_WANT, true);
        assert!(
            app.developer_tools_install_due(false),
            "with nothing published and nobody at the page"
        );
        assert!(!app.developer_tools_install_due(true), "a dialog is up");

        let mut stopped = App::headless_for_test();
        stopped.publish_system_state(crate::SetupStatus {
            run: Some(crate::InitialSetupRunPolicy {
                revision: 4,
                allowed: false,
            }),
            finished: true,
            ..crate::SetupStatus::default()
        });
        stopped.want_developer_tools_install(LAUNCH_WANT, true);
        assert!(
            !stopped.developer_tools_install_due(false),
            "a durable Stop"
        );

        let mut local = App::headless_for_test();
        local.want_developer_tools_install(LAUNCH_WANT, true);
        local.stop_initial_setup_permissions();
        assert_eq!(local.workstation_setup.developer_tools, Install::Unasked);
        assert!(!local.developer_tools_install_due(false), "a person's Stop");
        local.want_developer_tools_install(4, true);
        assert!(
            !local.developer_tools_install_due(false),
            "a delayed pass rearms nothing"
        );
    }

    /// A launch with a window asks for the tools where they are missing and the account never had
    /// the tab; the watch that takes the shim off runs where it reads this Mac, whether or not the
    /// tab may open.
    /// NEGATIVE CONTROL: a Mac with them, or a headless instance, wants nothing, and a test's
    /// reading starts no watch.
    #[test]
    fn a_launch_asks_only_on_a_mac_without_the_tools_and_only_once_per_account() {
        let at = |window, missing, opened| launch_install(window, window, missing, opened);
        assert_eq!(
            at(true, true, false),
            LaunchInstall {
                watch: true,
                want: true
            }
        );
        assert_eq!(
            at(true, true, true),
            LaunchInstall {
                watch: true,
                want: false
            }
        );
        for (window, missing) in [(true, false), (false, true), (false, false)] {
            assert_eq!(
                at(window, missing, false),
                LaunchInstall {
                    watch: false,
                    want: false
                }
            );
        }
        assert_eq!(
            launch_install(true, false, true, false),
            LaunchInstall {
                watch: false,
                want: true
            },
            "a test's window wants them and starts no watch"
        );
    }

    /// The launch's want is made where the window's setup host is: a host built over a shim that
    /// finds the tools missing wants them as [`LAUNCH_WANT`] from its first moment, and an app
    /// that keeps it opens the tab with no pass, no run and no page. That is the host the launch
    /// builds ([`crate::workstation_setup::Host::for_launch`]), over the shim it reconciled
    /// ([`crate::clt_shim::Shim::for_launch`]), so no launch can drop the step and keep the host.
    /// NEGATIVE CONTROL: the same host over a shim that finds the tools, in a headless instance,
    /// or with the account's mark, wants nothing.
    #[cfg(unix)]
    #[test]
    fn the_launchs_setup_host_wants_the_tools_from_its_start_where_they_are_missing() {
        let tmp = aterm_tempfile::tempdir().expect("a state directory");
        let host = |headless: bool, present: fn() -> bool, mark: &str| {
            crate::workstation_setup::Host::for_launch_marked(
                None,
                headless,
                Mark::at(tmp.path().join(mark)),
                crate::clt_shim::Shim::at(tmp.path().join("clt-shim"), present),
            )
        };
        let missing = host(false, || false, "no-mark");
        assert_eq!(missing.developer_tools, Install::Wanted(LAUNCH_WANT));
        assert_eq!(missing.developer_tools_want, Some(LAUNCH_WANT));
        let mut app = App::headless_for_test();
        app.workstation_setup = missing;
        assert!(
            app.developer_tools_install_due(false),
            "the launch's tab is due with nothing published"
        );

        for (headless, present, mark) in [
            (false, (|| true) as fn() -> bool, "no-mark"),
            (true, || false, "no-mark"),
        ] {
            let host = host(headless, present, mark);
            assert_eq!(host.developer_tools, Install::Unasked, "{headless}");
            assert_eq!(host.developer_tools_want, None, "{headless}");
        }
        super::landing_tests::this_builds_mark(&tmp.path().join("opened"));
        let marked = host(false, || false, "opened");
        assert_eq!(
            marked.developer_tools,
            Install::Unasked,
            "the tab opened before"
        );
    }

    /// The per-build mark is written durably, the file and then its directory entry, so a
    /// power loss right after the tab landed never opens it again on that build; a second landing
    /// writes nothing.
    #[test]
    #[cfg(unix)]
    fn the_per_build_mark_is_written_durably_with_its_directory() {
        let dir = aterm_tempfile::tempdir().expect("a config directory");
        let path = dir.path().join("command-line-tools-tab");
        let mut mark = Mark::at(path.clone());
        assert!(!mark.opened());
        let recording = aterm_durable::seam::Recording::start();
        mark.note_opened();
        assert_eq!(
            recording.take(),
            [aterm_durable::Flush::Durable, aterm_durable::Flush::Durable],
            "the mark's file, then its directory"
        );
        assert!(mark.opened() && path.is_file());
        mark.note_opened();
        assert!(recording.take().is_empty(), "written once");
    }

    /// The mark names each build that opened the tab, the newest [`MARK_BUILDS`]: a later launch
    /// of a build it names asks no more, nor do two builds sharing the account launched in turn,
    /// while a build it does not name (an update) retries unfinished setup once. A bare `opened`
    /// (an older aterm's mark) names no build. A mark that cannot be read or is malformed, and a
    /// build no line could name, hold, and such a mark is left as found.
    #[test]
    fn each_build_opens_the_tab_by_itself_at_most_once() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("command-line-tools-tab");
        let opened = |build: &str| Mark::opened_on_build(&path, build);
        let text = || std::fs::read_to_string(&path).unwrap();
        assert!(!opened("release"), "no mark");
        Mark::record(&path, "release", true).unwrap();
        assert!(opened("release"), "a later launch of that build");
        assert!(!opened("dev"), "another build retries once");
        Mark::record(&path, "dev", true).unwrap();
        assert!(
            opened("release") && opened("dev"),
            "builds launched in turn"
        );
        assert_eq!(text(), "opened release\nopened dev\n");

        for build in 1..MARK_BUILDS {
            Mark::record(&path, &format!("update-{build}"), true).unwrap();
        }
        assert!(!opened("release"), "the oldest gives way");
        assert!(opened("dev") && opened(&format!("update-{}", MARK_BUILDS - 1)));
        assert_eq!(text().lines().count(), MARK_BUILDS);

        // Low Data Mode takes one build's opening back and keeps the others'; the file goes
        // with the last one.
        let before = text();
        Mark::record(&path, "release", false).unwrap();
        assert_eq!(text(), before, "a build it does not name changes nothing");
        Mark::record(&path, "dev", false).unwrap();
        assert!(!opened("dev") && opened("update-1"));
        let only = dir.path().join("only");
        Mark::record(&only, "release", true).unwrap();
        Mark::record(&only, "release", false).unwrap();
        assert!(!only.exists());

        std::fs::write(&path, "opened\n").unwrap();
        assert!(
            !opened("release"),
            "an older aterm's bare mark names no build"
        );
        Mark::record(&path, "release", true).unwrap();
        assert_eq!(text(), "opened release\n");

        let too_many = "opened again\n".repeat(MARK_BUILDS + 1);
        for malformed in [
            "",
            "\n",
            "opened two words\n",
            "closed\n",
            too_many.as_str(),
        ] {
            std::fs::write(&path, malformed).unwrap();
            assert!(opened("release"), "{malformed:?} holds");
            assert!(
                Mark::record(&path, "release", true).is_err(),
                "{malformed:?}"
            );
            assert!(
                Mark::record(&path, "release", false).is_err(),
                "{malformed:?}"
            );
            assert_eq!(text(), malformed, "left as found");
        }
        assert!(
            Mark::opened_on_build(dir.path(), "release"),
            "unreadable holds"
        );
        let unnameable = "x".repeat(MARK_BUILD_BYTES + 1);
        assert!(
            Mark::opened_on_build(&dir.path().join("none"), &unnameable),
            "a build no line could name holds"
        );
        assert!(Mark::record(&dir.path().join("none"), &unnameable, true).is_err());
    }

    /// The tab's own program is the install item alone, which asks the one password, under the
    /// tab's variable and with no shell between: nothing is typed but its go line.
    #[test]
    fn the_tab_runs_the_command_line_tools_item_as_its_own_program() {
        assert_eq!(
            tab_command(std::path::Path::new(
                "/Applications/aterm.app/Contents/MacOS/aterm"
            )),
            [
                "/usr/bin/env",
                "ATERM_COMMAND_LINE_TOOLS_TAB=1",
                "/Applications/aterm.app/Contents/MacOS/aterm",
                "init",
                "--only",
                "base.clt",
            ]
            .map(std::ffi::OsString::from)
        );
    }

    /// A placeholder counts as an install only while something that installs holds it (an
    /// installer, Apple's window while it or the placeholder is younger than its bound, or a live
    /// run of aterm's root half that stamped it), and Apple's own window before its install
    /// starts, or left open past that bound beside a leftover placeholder as old, waits for the
    /// person (design §3.3).
    #[test]
    fn the_placeholder_is_an_install_only_while_something_installs() {
        let read = |placeholder, install_process, apple_window| {
            activity(Reading {
                placeholder,
                install_process,
                apple_window,
                ..Reading::default()
            })
        };
        let young_window = |placeholder| {
            activity(Reading {
                placeholder,
                apple_window: true,
                apple_window_young: true,
                ..Reading::default()
            })
        };
        assert_eq!(
            young_window(true),
            Activity::Installing,
            "the install a person started in Apple's window"
        );
        assert_eq!(
            young_window(false),
            Activity::AppleWindow,
            "its window before the press"
        );
        let stamped = |placeholder| {
            activity(Reading {
                placeholder,
                live_run: true,
                ..Reading::default()
            })
        };
        assert_eq!(
            stamped(true),
            Activity::Installing,
            "a live run between its steps holds it"
        );
        assert_eq!(stamped(false), Activity::Idle, "no placeholder, no install");
        assert_eq!(
            read(true, false, false),
            Activity::Idle,
            "a stale placeholder"
        );
        assert_eq!(
            read(false, true, false),
            Activity::Idle,
            "an update of something else"
        );
        assert_eq!(read(true, true, false), Activity::Installing);
        assert_eq!(
            read(false, false, true),
            Activity::AppleWindow,
            "waiting for a press"
        );
        assert_eq!(
            read(true, false, true),
            Activity::AppleWindow,
            "a window left open past its bound beside a leftover placeholder as old"
        );
        assert_eq!(read(false, false, false), Activity::Idle);
    }
}
