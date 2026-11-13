// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Apple's Command Line Tools, installed headlessly through `softwareupdate` (design §1.13).

use super::{Ctx, Item};
use crate::init_clt::Unfinished;
use crate::model::Probe;
use crate::port::{Cmd, System};
use serde::Deserialize;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

mod recovery;

/// Where the tools install.
pub const CLT_DIR: &str = "/Library/Developer/CommandLineTools";

/// The Git executable supplied by the tools.
pub const CLT_GIT: &str = "/Library/Developer/CommandLineTools/usr/bin/git";

/// The compiler executable supplied by the tools.
pub const CLT_CLANG: &str = "/Library/Developer/CommandLineTools/usr/bin/clang";

/// The tools' package receipt (its version is the report's).
pub const CLT_RECEIPT: &str = "com.apple.pkg.CLTools_Executables";

/// The placeholder that makes `softwareupdate -l` list the tools, which System State reads as
/// an install running.
pub const PLACEHOLDER: &str = aterm_types::macos_setup::COMMAND_LINE_TOOLS_INSTALLING;

/// A root-owned, persistent lock inode shared by every aterm placeholder writer.
/// Never unlink it: replacement would give concurrent writers different locks.
const PLACEHOLDER_LOCK: &str = "/var/run/aterm-command-line-tools.lock";

fn placeholder_lock(sys: &dyn System) -> std::io::Result<Box<dyn std::any::Any>> {
    sys.exclusive(Path::new(PLACEHOLDER_LOCK), Duration::ZERO)
}

/// `softwareupdate`.
pub const SOFTWAREUPDATE: &str = "/usr/sbin/softwareupdate";

/// Apple's package `installer`: one running beside the on-demand placeholder installs the tools
/// from their package (a script's `installer -pkg`). Compared, never run.
const INSTALLER: &str = "/usr/sbin/installer";

/// Whether `exe` is one of Apple's command-line installers ([`SOFTWAREUPDATE`], [`INSTALLER`]),
/// by its exact path: compared, never run. What [`aterm_types::macos_setup::tools_held_by`] asks
/// of `aterm init` and the root half.
#[must_use]
pub fn is_install_process(exe: &Path) -> bool {
    exe == Path::new(SOFTWAREUPDATE) || exe == Path::new(INSTALLER)
}

/// `xcode-select`.
pub const XCODE_SELECT: &str = aterm_uds::os_path!("xcode-select");

/// `softwareupdate -l` reads Apple's catalog; slow networks take minutes.
pub const LIST_BUDGET: Duration = Duration::from_secs(20 * 60);

/// `softwareupdate -i` downloads about half a gigabyte and installs it: two hours, well beyond
/// the minutes both take on a working network.
pub const INSTALL_BUDGET: Duration = Duration::from_secs(2 * 60 * 60);

/// How long a run waits for Apple's installer window ([`XCODE_SELECT`] `--install`, or the one
/// already open) to finish once it is on screen: a person presses Install, and the download and
/// install take minutes. A window that closes ends the wait at once.
pub const INSTALLER_PATIENCE: Duration = Duration::from_secs(30 * 60);

/// How often that wait looks for the installed tools, in this process.
const INSTALLER_TICK: Duration = Duration::from_secs(2);

/// Why the tools wait while the network is in Low Data Mode.
pub const LOW_DATA: &str = "Low Data Mode is on for this network, and the Command Line Tools \
                            are about half a gigabyte to download: turn it off for this network \
                            in System Settings > Network, or join another network";

/// [`LOW_DATA`] when the network this Mac uses now is in Low Data Mode
/// ([`System::low_data_mode`]); nothing when it is not, or cannot be told.
#[must_use]
pub fn low_data(sys: &dyn System) -> Option<&'static str> {
    (sys.low_data_mode() == Some(true)).then_some(LOW_DATA)
}

/// The `[developer]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `true` installs the newest `Command Line Tools for Xcode` label when none is installed, and
    /// selects them when nothing is selected.
    pub command_line_tools: Option<bool>,
    /// `true` turns developer mode on and adds the console account to `_developer`
    /// ([`crate::root_steps::DeveloperMode`]).
    pub debugger: Option<bool>,
    /// Only the built-in, already authorized root batch retains install custody.
    #[serde(skip)]
    pub(crate) retained_root_run: bool,
}

/// The items `table` asks for: developer mode before the tools, as it takes a moment and their
/// install takes minutes and can end short (round 9: the elevation's root half ran at 4 s and
/// developer mode was still off at 86 s, behind the tools' download).
/// # Errors
/// Never today; the signature is the family contract.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if table.debugger == Some(true) {
        items.push(Box::new(crate::root_steps::DeveloperMode));
    }
    if table.command_line_tools == Some(true) {
        items.push(Box::new(CommandLineTools {
            planned: Mutex::new(None),
            unfinished: Mutex::new(None),
            retained_root_run: table.retained_root_run,
        }));
    }
    Ok(items)
}

/// One `Command Line Tools` entry of `softwareupdate -l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// What `softwareupdate -i` takes.
    pub label: String,
    /// Its version, from the `Title:` line's `Version:` or else from the label.
    pub version: Vec<u32>,
    /// A beta (picked only when nothing else is listed).
    pub beta: bool,
}

/// Every `Command Line Tools` label in `softwareupdate -l`'s answer, in its order.
#[must_use]
pub fn parse_labels(text: &str) -> Vec<Label> {
    let mut out: Vec<Label> = Vec::new();
    let mut last_was_clt = false;
    for l in text.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("* ") {
            let label = rest.strip_prefix("Label: ").unwrap_or(rest).trim();
            last_was_clt = label.contains("Command Line Tools");
            if last_was_clt {
                out.push(Label {
                    label: label.to_owned(),
                    version: version_after_xcode(label),
                    beta: label.to_ascii_lowercase().contains("beta"),
                });
            }
            continue;
        }
        if last_was_clt
            && let Some(v) = t.split_once("Version: ").map(|(_, v)| leading_version(v))
            && !v.is_empty()
            && let Some(last) = out.last_mut()
        {
            last.version = v;
        }
        last_was_clt = false;
    }
    out
}

/// The label to install: the newest non-beta one, else the newest beta.
#[must_use]
pub fn pick(labels: &[Label]) -> Option<&Label> {
    let newest = |beta: bool| {
        labels
            .iter()
            .filter(|l| l.beta == beta)
            .max_by(|a, b| a.version.cmp(&b.version))
    };
    newest(false).or_else(|| newest(true))
}

fn version_after_xcode(label: &str) -> Vec<u32> {
    label
        .rsplit_once("Xcode")
        .map(|(_, rest)| leading_version(rest.trim_start_matches([' ', '-', '('])))
        .unwrap_or_default()
}

fn leading_version(text: &str) -> Vec<u32> {
    let digits: String = text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.split('.').map_while(|p| p.parse().ok()).collect()
}

/// What the probe found, for apply and revert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Planned {
    /// Not installed: install, then select if nothing is selected.
    Install,
    /// Installed but no usable developer directory selected: select.
    Select,
}

struct CommandLineTools {
    planned: Mutex<Option<Planned>>,
    /// Why the last probe or apply left the tools uninstalled ([`Item::unfinished`]).
    unfinished: Mutex<Option<Unfinished>>,
    retained_root_run: bool,
}

/// A failure of the install, in its words for a person's own terminal and its class for the
/// Mac Setup tab ([`Unfinished`]).
#[derive(Debug)]
struct Failure {
    class: Unfinished,
    why: String,
}

impl Failure {
    fn new(class: Unfinished, why: String) -> Self {
        Self { class, why }
    }
}

/// What holds an install of the tools now ([`holder`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    /// An install of the tools runs: `softwareupdate` or `installer` beside the on-demand
    /// placeholder, or a run of the root half alive now that stamped it
    /// ([`placeholder_held_by_a_live_run`]: between its steps, or waiting on Apple's installer
    /// window). It is that install's: the root half says another install runs, `aterm init`
    /// defers to it.
    Install,
    /// Apple's installer window, at any age, with the placeholder beside it or not: Apple's own
    /// flow, which makes the placeholder as its window opens (unless one is there already) and
    /// unlinks it when its request ends (a quit before any press unlinks only one it made),
    /// waiting for a press or running the install a press started. The root half waits on that
    /// window, never replacing a placeholder beside it, making one where none is there, or
    /// running `softwareupdate`; `aterm init` defers to it, naming the person's step there.
    AppleWindow,
    /// Nothing: no install runs, and a placeholder there is one an install that ended left, which
    /// the root half replaces.
    Nothing,
    /// Placeholder or process-table state could not be read, with no independent proof of
    /// installer custody: the root half takes a placeholder from no one.
    Unread,
}

/// What holds an install of the tools now, with the on-demand placeholder there or not as the
/// caller found it (`placeholder`): by the one rule System State's page, `aterm init` and the root
/// half share ([`aterm_types::macos_setup::tools_held_by`]) over every process's executable as
/// the kernel names it ([`System::processes`]; one it names none for is none of Apple's
/// installers, which live on the sealed system volume), a command-line installer beside the
/// placeholder or a live run's stamp in it first, then Apple's installer window, placeholder or
/// not. `None` for `placeholder` preserves an unread observation. Without independent
/// custody evidence, unread placeholder or process-table state returns [`Holder::Unread`].
#[must_use]
pub fn holder(sys: &dyn System, placeholder: Option<bool>) -> Holder {
    use aterm_types::macos_setup::ToolsInstaller;
    let held = sys.processes().map(|processes| {
        aterm_types::macos_setup::tools_held_by(
            placeholder.unwrap_or(false),
            processes.into_iter().map(|process| process.exe),
            is_install_process,
        )
    });
    match held {
        Some(Some(ToolsInstaller::CommandLine)) => Holder::Install,
        _ if placeholder != Some(false) && placeholder_held_by_a_live_run(sys) => Holder::Install,
        Some(Some(ToolsInstaller::AppleWindow)) => Holder::AppleWindow,
        Some(None) if placeholder.is_some() => Holder::Nothing,
        _ => Holder::Unread,
    }
}

/// The on-demand placeholder's presence, preserving failed reads as unknown.
#[must_use]
pub fn placeholder_state(sys: &dyn System) -> Option<bool> {
    match sys.stat(Path::new(PLACEHOLDER)) {
        Ok(_) => Some(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// Each of Apple's installers among the processes the kernel names now
/// ([`aterm_types::macos_setup::tools_installer`]), placeholder or not; `None` where the process
/// table cannot be read. A process it names no executable for is none of them.
fn installers_listed(sys: &dyn System) -> Option<Vec<aterm_types::macos_setup::ToolsInstaller>> {
    Some(
        sys.processes()?
            .iter()
            .filter_map(|process| process.exe.as_deref())
            .filter_map(|exe| aterm_types::macos_setup::tools_installer(exe, is_install_process))
            .collect(),
    )
}

/// Whether Apple's installer window is among the processes the kernel names now
/// ([`installers_listed`]); not where the process table cannot be read.
fn apple_window_listed(sys: &dyn System) -> bool {
    installers_listed(sys).is_some_and(|listed| {
        listed.contains(&aterm_types::macos_setup::ToolsInstaller::AppleWindow)
    })
}

fn selected(sys: &dyn System) -> Result<Option<String>, String> {
    let cmd = Cmd::new(XCODE_SELECT).arg("-p");
    let ran = sys
        .run(&cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if ran.success() {
        let dir = ran.out().trim().to_owned();
        if dir.is_empty() {
            return Ok(None);
        }
        for bin in ["usr/bin", aterm_types::clt_shim::DEFAULT_TOOLCHAIN_BIN] {
            let candidate = Path::new(&dir).join(bin).join("clang");
            let inspected = sys.canonicalize(&candidate).and_then(|path| {
                if path.starts_with("/usr/bin") {
                    return Ok(false);
                }
                sys.stat(&path).map(|stat| {
                    !stat.dir && !stat.symlink && stat.mode & 0o111 != 0 && stat.len > 0
                })
            });
            match inspected {
                Ok(true) => return Ok(Some(dir)),
                Ok(false) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) => {}
                Err(e) => return Err(format!("{}: {e}", candidate.display())),
            }
        }
        return Ok(None);
    }
    if ran.status == Some(2)
        && ran
            .err()
            .contains("Unable to get active developer directory")
    {
        return Ok(None);
    }
    Err(format!("{}: {}", cmd.display(), ran.describe()))
}

fn installed(sys: &dyn System) -> Result<bool, String> {
    for executable in [CLT_GIT, CLT_CLANG] {
        match sys.stat(Path::new(executable)) {
            Ok(stat) if !stat.dir && !stat.symlink && stat.mode & 0o111 != 0 && stat.len > 0 => {}
            Ok(_) => return Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(format!("{executable}: {e}")),
        }
    }
    Ok(true)
}

fn receipt_version(sys: &dyn System) -> Option<String> {
    let ran = sys
        .run(&Cmd::new("/usr/sbin/pkgutil").arg(format!("--pkg-info={CLT_RECEIPT}")))
        .ok()
        .filter(crate::port::Ran::success)?;
    ran.out()
        .lines()
        .find_map(|l| l.strip_prefix("version: ").map(|v| v.trim().to_owned()))
}

/// What a run writes into the placeholder it holds
/// ([`aterm_types::macos_setup::placeholder_stamp`]): its process and when that started
/// ([`System::process_started`], `0` where unread), so a later run tells a placeholder a live run
/// holds (between its steps, or waiting on Apple's installer window, when no installer process
/// runs) from one left behind, and a run's guard removes only its own.
fn stamp(sys: &dyn System) -> Vec<u8> {
    let pid = std::process::id();
    aterm_types::macos_setup::placeholder_stamp(pid, sys.process_started(pid).unwrap_or(0))
        .into_bytes()
}

/// The placeholder's stamp, when it is a regular file root owns, no longer than a stamp, read
/// without following a link or waiting on a pipe ([`System::read_owned_file`]); `None` for one
/// Apple's or an older aterm's install left empty, or any other.
fn stamped(sys: &dyn System) -> Option<Vec<u8>> {
    read_stamp(sys).ok().filter(|bytes| !bytes.is_empty())
}

/// Read bounded, root-owned placeholder bytes without losing admission failures.
fn read_stamp(sys: &dyn System) -> std::io::Result<Vec<u8>> {
    sys.read_owned_file(
        Path::new(PLACEHOLDER),
        0,
        aterm_types::macos_setup::PLACEHOLDER_STAMP_MAX,
        false,
    )
}

/// Whether Apple's installer window is over with nothing else installing, by `listed` (what
/// [`installers_listed`] read this tick): no window and no command-line installer listed, and
/// either the on-demand placeholder
/// gone, which the window unlinks by its path when its request ends (Not Now, Cancel, Done),
/// whoever made it (itself, this run, or an earlier run that kept it beside the window), just
/// before it quits; or a placeholder aterm stamped still there ([`stamped`]):
/// a window quit before Install was pressed (its menu's Quit, the Dock's, a logout) ends no
/// request and unlinks at its exit only a file it created, never one aterm's run had made
/// beside it ([`aterm_types::macos_setup::APPLE_TOOLS_INSTALLER`]). The caller already observed
/// the external window. An unstamped placeholder (Apple's own or an older aterm's, which only
/// a window killed outright leaves) or an unread process table does not prove it closed.
fn window_closed(
    sys: &dyn System,
    listed: Option<&[aterm_types::macos_setup::ToolsInstaller]>,
) -> bool {
    listed.is_some_and(<[_]>::is_empty)
        && match sys.stat(Path::new(PLACEHOLDER)) {
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
            Ok(_) => stamped(sys).is_some(),
        }
}

/// The placeholder an earlier run of aterm's kept beside Apple's window, removed once that
/// window is gone: a stamp ([`stamped`]) of a run not alive now, with nothing installing beside
/// it ([`Holder::Nothing`]), which would otherwise outlive the wait as a stale placeholder. One a
/// live run holds ([`Holder::Install`]) is left, this run's own among them where its start was
/// read (its guard removes it, [`Placeholder`]; one whose start was not read reads gone and goes
/// here, as the guard would remove it with the window gone), and so is one with no stamp
/// (Apple's own, an older aterm's).
fn remove_a_gone_runs_placeholder(sys: &dyn System) {
    let Ok(_claim) = placeholder_lock(sys) else {
        return;
    };
    if stamped(sys).is_some() && holder(sys, Some(true)) == Holder::Nothing {
        let _ = sys.remove(Path::new(PLACEHOLDER));
    }
}

/// Whether the placeholder is held by a run of aterm's that is alive now: its stamp names a
/// process still running from the same start, by the one reader the window's page shares
/// ([`aterm_types::macos_setup::placeholder_held_by_a_live_run`]; a start of `0` matches no
/// process). Such a run started the install and alone may hold or replace the placeholder,
/// though no installer process runs this moment (it is between two steps, or waits on Apple's
/// installer window).
fn placeholder_held_by_a_live_run(sys: &dyn System) -> bool {
    stamped(sys).is_some_and(|bytes| {
        aterm_types::macos_setup::placeholder_held_by_a_live_run(&bytes, |pid| {
            sys.process_started(pid)
        })
    })
}

/// The placeholder this run put in place, removed when the install ends, and only while it is
/// still this run's (one a later run took over stays) and no Apple installer is listed
/// ([`installers_listed`]). A command-line installer which starts during a network wait also
/// keeps the placeholder it uses. A window open then came up beside this run's placeholder
/// (`xcode-select --install`'s, or one a `git` or another stub raised meanwhile), so it made none
/// of its own, and its install needs the file to find the tools in Apple's catalog: the
/// placeholder stays for it, the window unlinks it when its request ends, and every reader holds
/// the tools for that window meanwhile ([`Holder::AppleWindow`]). One the window leaves as it
/// goes without ending its request (quit before Install was pressed, or killed outright), its
/// run gone, is stale: a later run waiting on that window removes it as the window goes
/// ([`remove_a_gone_runs_placeholder`]), and the next run replaces it ([`Holder::Nothing`]).
/// An unread installer table proves no permission to remove this run's placeholder.
/// The exclusive guard survives through this destructor's removal decision and unlink.
struct Placeholder<'a> {
    sys: &'a dyn System,
    stamp: Vec<u8>,
    _claim: Box<dyn std::any::Any>,
}

impl Drop for Placeholder<'_> {
    fn drop(&mut self) {
        if stamped(self.sys).as_deref() == Some(self.stamp.as_slice())
            && installers_listed(self.sys).is_some_and(|owners| owners.is_empty())
        {
            let _ = self.sys.remove(Path::new(PLACEHOLDER));
        }
    }
}

fn run_ok(sys: &dyn System, cmd: &Cmd) -> Result<String, String> {
    let ran = sys
        .run(cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if ran.success() {
        Ok(ran.out())
    } else {
        Err(format!("{}: {}", cmd.display(), ran.describe()))
    }
}

impl CommandLineTools {
    fn install(sys: &dyn System) -> Result<(), Failure> {
        // A closed tab (SIGHUP), a Ctrl-C or a Ctrl-\ reaches this run, never `softwareupdate`
        // (its own process group): deferred, it lets the running step finish and the placeholder
        // guard remove the placeholder (or leave it to Apple's window that needs it,
        // [`Placeholder`]), and then ends the run as it would have, so a stop meant for a whole
        // apply is not lost.
        let _stops = sys.defer_stops();
        let mut installed = Self::install_placeheld(sys);
        // Recover inside the authorized root run, rather than opening another password tab.
        // Each attempt reacquires the placeholder and defers to any installer that took over.
        for seconds in [5, 15] {
            if !matches!(
                installed.as_ref(),
                Err(Failure {
                    class: Unfinished::Catalog | Unfinished::Download | Unfinished::NoneOffered,
                    ..
                })
            ) || sys.stopped().is_some()
            {
                break;
            }
            say(&format!(
                "Apple's Command Line Tools are not ready; retrying in {seconds} seconds"
            ));
            sys.sleep(Duration::from_secs(seconds));
            if sys.stopped().is_some() {
                break;
            }
            if let Some(why) = low_data(sys) {
                installed = Err(Failure::new(Unfinished::LowData, why.to_owned()));
                break;
            }
            match self::installed(sys) {
                Ok(true) => {
                    installed = Ok(());
                    break;
                }
                Err(why) => {
                    installed = Err(Failure::new(Unfinished::Unread, why));
                    break;
                }
                Ok(false) => {}
            }
            installed = Self::install_placeheld(sys);
        }
        if sys.stopped().is_some() {
            say(stopped_line(installed.is_ok()));
        }
        installed
    }

    /// Put softwareupdate's on-demand placeholder in place, stamped as this run's ([`stamp`]),
    /// and retain this run's exclusive claim (`Some`), unless Apple's installer window holds the
    /// tools, with a placeholder beside it or none ([`Holder::AppleWindow`]: `None`, this run
    /// makes or takes no placeholder and waits on that window). One already there is left alone
    /// where an install holds it or nobody can tell ([`Holder::Install`], [`Holder::Unread`]:
    /// another install runs); else it is stale, left by an install that ended without removing
    /// it, and this one takes its place. Unread custody prevents a claim before any mutation,
    /// including when no placeholder has been observed. The persistent lock serializes the
    /// entire claim through installation and cleanup, independent of readable process birth.
    fn hold_placeholder(sys: &dyn System) -> Result<Option<Placeholder<'_>>, Failure> {
        let claim = placeholder_lock(sys).map_err(|error| {
            if error.kind() == std::io::ErrorKind::TimedOut {
                Failure::new(
                    Unfinished::Busy,
                    "another aterm setup owns the developer-tools install".into(),
                )
            } else {
                Failure::new(Unfinished::Unread, format!("{PLACEHOLDER_LOCK}: {error}"))
            }
        })?;
        let placeholder = Path::new(PLACEHOLDER);
        let held =
            |e: std::io::Error| Failure::new(Unfinished::Other, format!("{PLACEHOLDER}: {e}"));
        let stamp = stamp(sys);
        let may_claim = |state| match holder(sys, state) {
            Holder::Nothing => Ok(true),
            Holder::AppleWindow => Ok(false),
            Holder::Install => Err(Failure::new(
                Unfinished::Busy,
                format!(
                    "another install of the Command Line Tools is running ({PLACEHOLDER} is its)"
                ),
            )),
            Holder::Unread => Err(Failure::new(
                Unfinished::Unread,
                "the current installer owner could not be read".into(),
            )),
        };
        if !may_claim(placeholder_state(sys))? {
            return Ok(None);
        }
        match sys.create_new(placeholder, &stamp, 0o644) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if !may_claim(Some(true))? {
                    return Ok(None);
                }
                sys.remove(placeholder).map_err(held)?;
                sys.create_new(placeholder, &stamp, 0o644).map_err(held)?;
            }
            created => created.map_err(held)?,
        }
        Ok(Some(Placeholder {
            sys,
            stamp,
            _claim: claim,
        }))
    }

    /// The install with softwareupdate's on-demand placeholder in place, removed when it ends
    /// (or left to Apple's window that needs it, [`Placeholder`]); or, where Apple's installer
    /// window holds the tools as this run begins or opens while it reads the catalog, the wait on
    /// that window alone, which takes over no placeholder and asks `softwareupdate` for none of
    /// the tools the window's own flow installs.
    fn install_placeheld(sys: &dyn System) -> Result<(), Failure> {
        let Some(_placeholder) = Self::hold_placeholder(sys)? else {
            return Self::through_open_window(sys);
        };
        Self::install_catalog(sys, None)
    }

    fn install_catalog(
        sys: &dyn System,
        custody: Option<&recovery::Custody<'_>>,
    ) -> Result<(), Failure> {
        if let Some(custody) = custody {
            custody.ready()?;
        }
        say("reading Apple's software catalog (minutes on a slow network)");
        let list = Cmd::new(SOFTWAREUPDATE).arg("-l").budget(LIST_BUDGET);
        let listed = run_ok(sys, &list).map_err(|why| Failure::new(Unfinished::Catalog, why))?;
        if let Some(signal) = sys.stopped() {
            return Err(Failure::new(
                Unfinished::Stopped,
                format!(
                    "stopped by signal {signal} once {} had read Apple's catalog; nothing was \
                     installed",
                    list.display()
                ),
            ));
        }
        // Apple's window raised while the catalog was read (by a `git` or another of Apple's
        // `/usr/bin` stubs, beside this run's placeholder, so it made none of its own) is the
        // person's install now: wait on it, the placeholder kept for it, rather than download
        // the same tools beside it or ask `xcode-select` for another window, which it refuses
        // while one is open. The window is looked for among the processes the kernel names, so
        // one it names none for (its file deleted) hides no window; only a process table that
        // cannot be read at all hides it.
        if apple_window_listed(sys) {
            return Self::through_open_window(sys);
        }
        if let Some(custody) = custody {
            custody.ready()?;
        }
        let labels = parse_labels(&listed);
        let Some(label) = pick(&labels) else {
            if installed(sys).map_err(|why| Failure::new(Unfinished::Unread, why))? {
                return Ok(());
            }
            return Err(Failure::new(
                Unfinished::NoneOffered,
                format!(
                    "{} lists no Command Line Tools for Xcode, with the on-demand placeholder \
                     in place",
                    list.display()
                ),
            ));
        };
        let install = Cmd::new(SOFTWAREUPDATE)
            .args(["-i", label.label.as_str()])
            .budget(INSTALL_BUDGET);
        say(&format!(
            "installing {}: about half a gigabyte to download, then the install (often ten \
             minutes or more)",
            label.label
        ));
        let ran = sys.run(&install).map_err(|e| {
            Failure::new(
                match e {
                    crate::port::RunFailed::TimedOut(_) => Unfinished::Download,
                    _ => Unfinished::Other,
                },
                format!("{}: {e}", install.display()),
            )
        })?;
        if !ran.success() {
            return Err(Failure::new(
                if downloaded(&ran) {
                    Unfinished::Installer
                } else {
                    Unfinished::Download
                },
                format!("{}: {}", install.display(), ran.describe()),
            ));
        }
        if !installed(sys).map_err(|why| Failure::new(Unfinished::Other, why))? {
            let said = ran
                .err()
                .lines()
                .chain(ran.out().lines().rev())
                .map(str::trim)
                .find(|l| !l.is_empty() && !SOFTWAREUPDATE_BOILERPLATE.contains(l))
                .unwrap_or("it said nothing more")
                .to_owned();
            return Err(Failure::new(
                Unfinished::Installer,
                format!(
                    "{} exited 0 but the Git and compiler executables are not both installed: {said}",
                    install.display()
                ),
            ));
        }
        Ok(())
    }

    /// Where Apple's installer window is open already ([`Holder::AppleWindow`]): as this run
    /// began, with a placeholder beside it or none, or raised while it read the catalog beside
    /// its placeholder. Its own flow installs the tools once the person presses Install there
    /// (or has, perhaps hours after the window opened) and unlinks the placeholder when its
    /// request ends, so this run takes nothing over and asks `softwareupdate` for nothing, which
    /// would start a second download of the same tools beside the window's, and only waits on
    /// that window ([`Self::wait_on_window`]), removing an earlier run's placeholder the window
    /// leaves as it is quit.
    fn through_open_window(sys: &dyn System) -> Result<(), Failure> {
        say(
            "Apple's installer window for the Command Line Tools is already open: finish there \
             (press Install if you have not yet; Ctrl-C stops waiting)",
        );
        Self::wait_on_window(
            sys,
            &format!(
                "Apple's installer window, already open, did not install the Command Line Tools \
                 within {} minutes",
                INSTALLER_PATIENCE.as_secs() / 60
            ),
        )
    }

    /// The wait in this process on Apple's installer window for the installed tools, every
    /// [`INSTALLER_TICK`] for at most [`INSTALLER_PATIENCE`] (then [`Unfinished::AppleWindow`],
    /// saying `patience`), ended early by a stop, and by the window closing without installing
    /// them ([`window_closed`]: Not Now, Cancel, or Done on Apple's own failure, or the window
    /// quit before Install was pressed, each a person's act there,
    /// [`Unfinished::WindowClosed`]). A placeholder an earlier run kept beside that window goes
    /// with it ([`remove_a_gone_runs_placeholder`]).
    fn wait_on_window(sys: &dyn System, patience: &str) -> Result<(), Failure> {
        let other = |why: String| Failure::new(Unfinished::Other, why);
        let ticks = INSTALLER_PATIENCE.as_millis() / INSTALLER_TICK.as_millis();
        for _ in 0..ticks {
            if installed(sys).map_err(other)? {
                return Ok(());
            }
            if let Some(signal) = sys.stopped() {
                return Err(Failure::new(
                    Unfinished::Stopped,
                    format!(
                        "stopped by signal {signal} while Apple's installer window was open; \
                         the tools are not installed yet"
                    ),
                ));
            }
            let listed = installers_listed(sys);
            if window_closed(sys, listed.as_deref()) {
                remove_a_gone_runs_placeholder(sys);
                // Read once more: the window quits a moment after its install ends.
                if installed(sys).map_err(other)? {
                    return Ok(());
                }
                return Err(Failure::new(
                    Unfinished::WindowClosed,
                    "Apple's installer window closed without installing the Command Line Tools"
                        .to_owned(),
                ));
            }
            sys.sleep(INSTALLER_TICK);
        }
        if installed(sys).map_err(other)? {
            return Ok(());
        }
        Err(Failure::new(Unfinished::AppleWindow, patience.to_owned()))
    }

    /// What the probe reads: the tools installed and selected, or the change to make.
    fn read(&self, sys: &dyn System) -> Probe {
        let (installed, selected) = match installed(sys).and_then(|i| selected(sys).map(|s| (i, s)))
        {
            Ok(both) => both,
            Err(why) => return Probe::Unusable { why },
        };
        let planned = match (installed, &selected) {
            (true, Some(dir)) => {
                let version = receipt_version(sys).map_or_else(String::new, |v| format!(" {v}"));
                return Probe::Ok {
                    now: format!("installed{version}; selected {dir}"),
                };
            }
            (true, None) => Planned::Select,
            (false, _) => {
                if !self.retained_root_run
                    && let Some(why) = low_data(sys)
                {
                    return Probe::Blocked { why: why.into() };
                }
                // softwareupdate owns catalog reachability and its recovery. A separate
                // HTTP probe can fail before the authorized install reaches those retries.
                Planned::Install
            }
        };
        if let Ok(mut slot) = self.planned.lock() {
            *slot = Some(planned);
        }
        Probe::Change {
            from: match planned {
                Planned::Install => "not installed".to_owned(),
                Planned::Select => "installed; no usable developer directory selected".to_owned(),
            },
            to: format!(
                "installed; selected {}",
                selected.as_deref().unwrap_or(CLT_DIR)
            ),
        }
    }

    /// Note why the tools are left uninstalled ([`Item::unfinished`]), or that nothing is.
    fn note(&self, why: Option<Unfinished>) {
        if let Ok(mut slot) = self.unfinished.lock() {
            *slot = why;
        }
    }
}

/// Whether `softwareupdate -i` got as far as its install: it says `Downloaded <name>` once the
/// download is whole and `Installing <name>` as Apple's installer starts.
fn downloaded(ran: &crate::port::Ran) -> bool {
    ran.out()
        .lines()
        .chain(ran.err().lines())
        .map(str::trim)
        .any(|line| line.starts_with("Downloaded ") || line.starts_with("Installing "))
}

/// The line a stopped install says once its step finished, in plain words (no signal number:
/// the person pressed Ctrl-C or closed the tab).
const fn stopped_line(installed: bool) -> &'static str {
    if installed {
        "stopped, with the tools installed"
    } else {
        "stopped before the tools were installed"
    }
}

/// One progress line on this run's terminal, so a minutes-long download does not look hung. A
/// terminal gone with its tab is no error: the install goes on.
fn say(line: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "Command Line Tools: {line}\u{2026}");
}

const SOFTWAREUPDATE_BOILERPLATE: [&str; 2] =
    ["Software Update Tool", "Finding available software"];

impl Item for CommandLineTools {
    fn id(&self) -> String {
        "developer.command-line-tools".to_owned()
    }

    fn family(&self) -> &'static str {
        "developer"
    }

    fn title(&self) -> String {
        "Command Line Tools for Xcode".to_owned()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let probe = self.read(sys);
        self.note(match &probe {
            Probe::Blocked { .. } => Some(Unfinished::LowData),
            Probe::Unusable { .. } => Some(Unfinished::Other),
            _ => None,
        });
        probe
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let applied = (|| {
            if !installed(sys).map_err(|why| Failure::new(Unfinished::Other, why))? {
                if self.retained_root_run {
                    recovery::install(sys)?;
                } else {
                    Self::install(sys)?;
                }
            }
            if selected(sys)
                .map_err(|why| Failure::new(Unfinished::Other, why))?
                .is_none()
            {
                run_ok(sys, &Cmd::new(XCODE_SELECT).args(["--switch", CLT_DIR]))
                    .map_err(|why| Failure::new(Unfinished::Other, why))?;
            }
            Ok(())
        })();
        self.note(
            applied
                .as_ref()
                .err()
                .map(|failure: &Failure| failure.class),
        );
        applied.map_err(|failure| failure.why)
    }

    fn unfinished(&self) -> Option<Unfinished> {
        self.unfinished.lock().ok().and_then(|slot| *slot)
    }

    fn revert(&self) -> Option<String> {
        match (*self.planned.lock().ok()?)? {
            Planned::Install => Some(format!(
                "{}; {}",
                Cmd::new("/bin/rm").args(["-rf", CLT_DIR]).display(),
                Cmd::new("/usr/sbin/pkgutil")
                    .arg(format!("--forget={CLT_RECEIPT}"))
                    .display()
            )),
            Planned::Select => Some(Cmd::new(XCODE_SELECT).arg("--reset").display()),
        }
    }
}

#[cfg(test)]
#[path = "developer_tests.rs"]
mod tests;
