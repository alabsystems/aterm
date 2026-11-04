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

/// Where the tools install.
pub const CLT_DIR: &str = "/Library/Developer/CommandLineTools";

/// The file whose presence means the tools are installed.
pub const CLT_GIT: &str = "/Library/Developer/CommandLineTools/usr/bin/git";

/// The tools' package receipt (its version is the report's).
pub const CLT_RECEIPT: &str = "com.apple.pkg.CLTools_Executables";

/// The placeholder that makes `softwareupdate -l` list the tools, which System State reads as
/// an install running.
pub const PLACEHOLDER: &str = aterm_types::macos_setup::COMMAND_LINE_TOOLS_INSTALLING;

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
                            in System Settings > Network, or join another network, then run \
                            `aterm init --only base.clt`";

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
}

/// The items `table` asks for.
/// # Errors
/// Never today; the signature is the family contract.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    Ok(if table.command_line_tools == Some(true) {
        vec![Box::new(CommandLineTools {
            planned: Mutex::new(None),
            unfinished: Mutex::new(None),
        })]
    } else {
        Vec::new()
    })
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
    /// Installed but no developer directory selected: select.
    Select,
}

struct CommandLineTools {
    planned: Mutex<Option<Planned>>,
    /// Why the last probe or apply left the tools uninstalled ([`Item::unfinished`]).
    unfinished: Mutex<Option<Unfinished>>,
}

/// A failure of the install, in its words for a person's own terminal and its class for the
/// Command Line Tools tab ([`Unfinished`]).
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
    /// The process table could not be read, and no live run stamped a placeholder: nobody can
    /// vouch for a placeholder there, so the root half takes it from no one.
    Unread,
}

/// What holds an install of the tools now, with the on-demand placeholder there or not as the
/// caller found it (`placeholder`): by the one rule System State's page, `aterm init` and the root
/// half share ([`aterm_types::macos_setup::tools_held_by`]) over every process's executable as
/// the kernel names it ([`System::processes`]; one it names none for is none of Apple's
/// installers, which live on the sealed system volume), a command-line installer beside the
/// placeholder or a live run's stamp in it first, then Apple's installer window, placeholder or
/// not. [`Holder::Unread`] only where the process table itself cannot be read.
#[must_use]
pub fn holder(sys: &dyn System, placeholder: bool) -> Holder {
    use aterm_types::macos_setup::ToolsInstaller;
    let held = sys.processes().map(|processes| {
        aterm_types::macos_setup::tools_held_by(
            placeholder,
            processes.into_iter().map(|process| process.exe),
            is_install_process,
        )
    });
    match held {
        Some(Some(ToolsInstaller::CommandLine)) => Holder::Install,
        _ if placeholder && placeholder_held_by_a_live_run(sys) => Holder::Install,
        Some(Some(ToolsInstaller::AppleWindow)) => Holder::AppleWindow,
        Some(None) => Holder::Nothing,
        None => Holder::Unread,
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
        return Ok((!dir.is_empty()).then_some(dir));
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
    match sys.stat(Path::new(CLT_GIT)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{CLT_GIT}: {e}")),
    }
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
    sys.read_owned_file(
        Path::new(PLACEHOLDER),
        0,
        aterm_types::macos_setup::PLACEHOLDER_STAMP_MAX,
        false,
    )
    .ok()
    .filter(|bytes| !bytes.is_empty())
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
    if stamped(sys).is_some() && holder(sys, true) == Holder::Nothing {
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
/// still this run's (one a later run took over stays) and Apple's installer window is not
/// listed ([`apple_window_listed`]). A window open then came up beside this run's placeholder
/// (`xcode-select --install`'s, or one a `git` or another stub raised meanwhile), so it made none
/// of its own, and its install needs the file to find the tools in Apple's catalog: the
/// placeholder stays for it, the window unlinks it when its request ends, and every reader holds
/// the tools for that window meanwhile ([`Holder::AppleWindow`]). One the window leaves as it
/// goes without ending its request (quit before Install was pressed, or killed outright), its
/// run gone, is stale: a later run waiting on that window removes it as the window goes
/// ([`remove_a_gone_runs_placeholder`]), and the next run replaces it ([`Holder::Nothing`]).
struct Placeholder<'a> {
    sys: &'a dyn System,
    stamp: Vec<u8>,
}

impl Drop for Placeholder<'_> {
    fn drop(&mut self) {
        if stamped(self.sys).as_deref() == Some(self.stamp.as_slice())
            && !apple_window_listed(self.sys)
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
    /// and say it is this run's (`Some`, its stamp), unless Apple's installer window holds the
    /// tools, with a placeholder beside it or none ([`Holder::AppleWindow`]: `None`, this run
    /// makes or takes no placeholder and waits on that window). One already there is left alone
    /// where an install holds it or nobody can tell ([`Holder::Install`], [`Holder::Unread`]:
    /// another install runs); else it is stale, left by an install that ended without removing
    /// it, and this one takes its place.
    fn hold_placeholder(sys: &dyn System) -> Result<Option<Vec<u8>>, Failure> {
        let placeholder = Path::new(PLACEHOLDER);
        let held =
            |e: std::io::Error| Failure::new(Unfinished::Other, format!("{PLACEHOLDER}: {e}"));
        let stamp = stamp(sys);
        // With no placeholder there, Apple's window holds the tools all the same: it opened
        // beside another install's placeholder, which that install has removed since.
        if sys
            .stat(placeholder)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            && holder(sys, false) == Holder::AppleWindow
        {
            return Ok(None);
        }
        match sys.create_new(placeholder, &stamp, 0o644) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match holder(sys, true) {
                Holder::Install | Holder::Unread => {
                    return Err(Failure::new(
                        Unfinished::Busy,
                        format!(
                            "another install of the Command Line Tools is running ({PLACEHOLDER} \
                             is its)"
                        ),
                    ));
                }
                Holder::AppleWindow => return Ok(None),
                Holder::Nothing => {
                    sys.remove(placeholder).map_err(held)?;
                    sys.create_new(placeholder, &stamp, 0o644).map_err(held)?;
                }
            },
            created => created.map_err(held)?,
        }
        Ok(Some(stamp))
    }

    /// The install with softwareupdate's on-demand placeholder in place, removed when it ends
    /// (or left to Apple's window that needs it, [`Placeholder`]); or, where Apple's installer
    /// window holds the tools as this run begins or opens while it reads the catalog, the wait on
    /// that window alone, which takes over no placeholder and asks `softwareupdate` for none of
    /// the tools the window's own flow installs.
    fn install_placeheld(sys: &dyn System) -> Result<(), Failure> {
        let Some(stamp) = Self::hold_placeholder(sys)? else {
            return Self::through_open_window(sys);
        };
        let _placeholder = Placeholder { sys, stamp };
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
        if !installed(sys).map_err(|why| Failure::new(Unfinished::Other, why))?
            && receipt_version(sys).is_none()
        {
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
                    "{} exited 0 but installed nothing ({CLT_RECEIPT} has no receipt): {said}",
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
                "Apple's installer window, already open, did not install them within {} minutes: \
                 press Install in it, or quit it, then run `aterm init --only base.clt`",
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
                    "Apple's installer window closed without installing the Command Line Tools: \
                     press Install\u{2026} on System State, or run `aterm init --only base.clt`, \
                     to try again"
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
                if let Some(why) = low_data(sys) {
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
                Planned::Select => "installed; no developer directory selected".to_owned(),
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
            Probe::Blocked { .. } => Some(Unfinished::Catalog),
            Probe::Unusable { .. } => Some(Unfinished::Other),
            _ => None,
        });
        probe
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let applied = (|| {
            if !installed(sys).map_err(|why| Failure::new(Unfinished::Other, why))? {
                Self::install(sys)?;
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
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::model::{HostFacts, Mode, Outcome};
    use crate::port::{Fake, Ran, Stat};

    fn ctx() -> Ctx {
        Ctx {
            facts: HostFacts::default(),
            users: Vec::new(),
            console: None,
            host_name: "lab-07".into(),
            cache_dir: "/c".into(),
            disrupt: true,
        }
    }

    const VM_LIST: &str = "Software Update Tool\n\
\n\
Finding available software\n\
Software Update found the following new or updated software:\n\
* Label: Command Line Tools for Xcode 27.0-27.0\n\
\tTitle: Command Line Tools for Xcode 27.0, Version: 27.0, Size: 519460KiB, Recommended: YES, \n\
* Label: macOS 27.0.1-26A434\n\
\tTitle: macOS 27.0.1, Version: 27.0.1, Size: 3235867KiB, Recommended: YES, Action: restart, \n\
";

    const OLD_LIST: &str = "Software Update Tool\n\
\n\
Finding available software\n\
Software Update found the following new or updated software:\n\
* Label: Command Line Tools for Xcode-15.3\n\
\tTitle: Command Line Tools for Xcode, Version: 15.3, Size: 707397KiB, Recommended: YES, \n\
* Label: Command Line Tools for Xcode-14.3\n\
\tTitle: Command Line Tools for Xcode, Version: 14.3, Size: 711657KiB, Recommended: YES, \n\
* Label: Command Line Tools beta 2 for Xcode-16.0\n\
\tTitle: Command Line Tools beta 2 for Xcode, Version: 16.0, Size: 721000KiB, Recommended: YES, \n\
";

    fn none_selected() -> Ran {
        Ran::exit(
            2,
            "xcode-select: error: Unable to get active developer directory. Use `sudo xcode-select --switch path/to/Xcode.app` to set one (or see `man xcode-select`)",
        )
    }

    fn git() -> Stat {
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o755,
            dir: false,
            symlink: false,
            len: 3_600_000,
        }
    }

    const RECEIPT: &str = "package-id: com.apple.pkg.CLTools_Executables\nversion: 27.0.0.0.1788430756\nvolume: /\nlocation: /\ninstall-time: 1792019691\n";

    fn no_receipt() -> Ran {
        Ran::exit(
            1,
            "No receipt for 'com.apple.pkg.CLTools_Executables' found at '/'.",
        )
    }

    const PKGUTIL: &str = "/usr/sbin/pkgutil --pkg-info=com.apple.pkg.CLTools_Executables";

    fn no_space() -> Ran {
        Ran {
            status: Some(0),
            stdout: b"Software Update Tool\n\nFinding available software\n".to_vec(),
            stderr: b"Not enough free disk space: a total of 9.52 GB is required.\n".to_vec(),
        }
    }

    fn installed_quietly() -> Ran {
        Ran::ok("Software Update Tool\n\nFinding available software\n")
    }

    const SOFTWAREUPDATE_LIST: &str = "/usr/sbin/softwareupdate -l";

    #[test]
    fn a_catalog_failure_recovers_in_the_same_authorized_run() {
        let sys = Fake::root();
        sys.on(
            SOFTWAREUPDATE_LIST,
            Ran {
                status: Some(1),
                ..Ran::ok("")
            },
        )
        .on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST))
        .on(
            "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
            installed_quietly(),
        )
        .on(PKGUTIL, Ran::ok(RECEIPT));
        CommandLineTools::install(&sys).unwrap();
        let calls = sys.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
                .count(),
            2
        );
        assert!(calls.iter().any(|c| c == "sleep 5000ms"));
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("sudo") || c.contains("xcode-select --install"))
        );
        assert!(sys.written(PLACEHOLDER).is_none());
    }

    #[test]
    fn a_persistent_catalog_failure_is_bounded_and_a_stop_prevents_recovery() {
        for stop_after in [None, Some(0), Some(1)] {
            let mut sys = Fake::root();
            sys.stop_after = stop_after;
            sys.on(
                SOFTWAREUPDATE_LIST,
                Ran {
                    status: Some(1),
                    ..Ran::ok("")
                },
            );
            assert_eq!(
                CommandLineTools::install(&sys).unwrap_err().class,
                Unfinished::Catalog
            );
            let calls = sys.calls();
            assert_eq!(
                calls
                    .iter()
                    .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
                    .count(),
                if stop_after.is_some() { 1 } else { 3 }
            );
            assert_eq!(
                calls.iter().filter(|c| c.starts_with("sleep ")).count(),
                match stop_after {
                    Some(0) => 0,
                    Some(_) => 1,
                    None => 2,
                }
            );
            assert!(sys.written(PLACEHOLDER).is_none());
        }
    }

    #[test]
    fn recovery_rechecks_low_data_and_tools_installed_during_the_wait() {
        for low_data in [false, true] {
            let mut sys = Fake::root();
            sys.low_data = Some(low_data);
            sys.on(
                SOFTWAREUPDATE_LIST,
                Ran {
                    status: Some(1),
                    ..Ran::ok("")
                },
            );
            if !low_data {
                sys.on_sleep(|sys, _| {
                    sys.stat_of(CLT_GIT, git());
                });
            }
            let result = CommandLineTools::install(&sys);
            if low_data {
                assert_eq!(result.unwrap_err().class, Unfinished::LowData);
            } else {
                result.unwrap();
            }
            assert_eq!(
                sys.calls()
                    .iter()
                    .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
                    .count(),
                1
            );
            assert!(sys.written(PLACEHOLDER).is_none());
        }
    }

    #[test]
    fn a_download_failure_recovers_but_an_installer_failure_is_not_retried() {
        const INSTALL: &str =
            "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'";
        for reached_installer in [false, true] {
            let sys = Fake::root();
            sys.on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST))
                .on(
                    INSTALL,
                    Ran {
                        status: Some(1),
                        ..Ran::ok(if reached_installer {
                            "Installing Command Line Tools\n"
                        } else {
                            "Download failed\n"
                        })
                    },
                )
                .on(INSTALL, installed_quietly())
                .on(PKGUTIL, Ran::ok(RECEIPT));
            let result = CommandLineTools::install(&sys);
            if reached_installer {
                assert_eq!(result.unwrap_err().class, Unfinished::Installer);
            } else {
                result.unwrap();
            }
            let calls = sys.calls();
            assert_eq!(
                calls.iter().filter(|c| c.as_str() == INSTALL).count(),
                if reached_installer { 1 } else { 2 }
            );
            assert_eq!(
                calls.iter().filter(|c| c.starts_with("sleep ")).count(),
                usize::from(!reached_installer)
            );
            assert!(sys.written(PLACEHOLDER).is_none());
        }
    }

    #[test]
    fn the_newest_label_is_picked_in_the_new_space_form_and_the_old_hyphen_form() {
        let labels = parse_labels(VM_LIST);
        assert_eq!(
            labels,
            vec![Label {
                label: "Command Line Tools for Xcode 27.0-27.0".into(),
                version: vec![27, 0],
                beta: false,
            }]
        );
        let old = parse_labels(OLD_LIST);
        assert_eq!(old.len(), 3);
        assert_eq!(
            pick(&old).unwrap().label,
            "Command Line Tools for Xcode-15.3"
        );
        let betas: Vec<Label> = old.into_iter().filter(|l| l.beta).collect();
        assert_eq!(pick(&betas).unwrap().version, vec![16, 0]);
        assert_eq!(
            version_after_xcode("Command Line Tools for Xcode 26.4-26.4.1"),
            vec![26, 4]
        );
        let without = VM_LIST.replace(
            "* Label: Command Line Tools for Xcode 27.0-27.0\n\tTitle: Command Line Tools for Xcode 27.0, Version: 27.0, Size: 519460KiB, Recommended: YES, \n",
            "",
        );
        assert_ne!(without, VM_LIST);
        assert!(pick(&parse_labels(&without)).is_none());
    }

    #[test]
    fn a_title_version_outranks_the_labels_own_digits() {
        let text = "* Label: Command Line Tools for Xcode 26.4-26.4.1\n\tTitle: Command Line Tools for Xcode 26.4, Version: 26.4.1, Size: 1KiB, \n* Label: Command Line Tools for Xcode 26.4-26.4\n\tTitle: Command Line Tools for Xcode 26.4, Version: 26.4, Size: 1KiB, \n";
        assert_eq!(
            pick(&parse_labels(text)).unwrap().label,
            "Command Line Tools for Xcode 26.4-26.4.1"
        );
    }

    #[test]
    fn installed_and_selected_tools_are_ok_with_their_receipt_version() {
        let sys = Fake::root();
        sys.stat_of(CLT_GIT, git())
            .on(
                "/usr/bin/xcode-select -p",
                Ran::ok("/Library/Developer/CommandLineTools\n"),
            )
            .on(PKGUTIL, Ran::ok(RECEIPT));
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            items[0].probe(&sys, &ctx()),
            Probe::Ok {
                now: "installed 27.0.0.0.1788430756; selected /Library/Developer/CommandLineTools"
                    .into()
            }
        );
        assert!(sys.calls().iter().all(|c| !c.contains("softwareupdate")));
    }

    #[test]
    fn a_fresh_mac_installs_the_listed_label_then_selects_it_and_the_placeholder_goes() {
        let sys = Fake::root();
        sys.on("/usr/bin/xcode-select -p", none_selected())
            .on("/usr/bin/xcode-select -p", none_selected())
            .on(
                "/usr/bin/xcode-select -p",
                Ran::ok("/Library/Developer/CommandLineTools\n"),
            )
            .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
            .on(
                "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                installed_quietly(),
            )
            .on(PKGUTIL, Ran::ok(RECEIPT))
            .on(
                "/usr/bin/xcode-select --switch /Library/Developer/CommandLineTools",
                Ran::ok(""),
            );
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        let rows = {
            let probe = items[0].probe(&sys, &ctx());
            assert_eq!(
                probe,
                Probe::Change {
                    from: "not installed".into(),
                    to: "installed; selected /Library/Developer/CommandLineTools".into()
                }
            );
            items[0].apply(&sys, &ctx()).unwrap();
            sys.stat_of(CLT_GIT, git());
            items[0].probe(&sys, &ctx())
        };
        assert!(matches!(rows, Probe::Ok { .. }), "{rows:?}");
        let calls = sys.calls();
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle)).unwrap();
        assert!(
            at("create /tmp/.com.apple.dt.CommandLineTools") < at("/usr/sbin/softwareupdate -l")
        );
        assert!(
            at("/usr/sbin/softwareupdate -i") < at("remove /tmp/.com.apple.dt.CommandLineTools")
        );
        assert!(at("remove /tmp/.com.apple.dt") < at("/usr/bin/xcode-select --switch"));
        assert_eq!(
            items[0].revert().as_deref(),
            Some(
                "/bin/rm -rf /Library/Developer/CommandLineTools; /usr/sbin/pkgutil --forget=com.apple.pkg.CLTools_Executables"
            )
        );
    }

    /// A closed tab or a Ctrl-C while `softwareupdate -l` reads the catalog installs nothing and
    /// leaves no placeholder to refuse the next install: the stops are caught before the
    /// placeholder exists.
    #[test]
    fn a_stop_while_the_catalog_is_read_installs_nothing_and_removes_the_placeholder() {
        let mut sys = Fake::root();
        sys.stop_after = Some(0);
        sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST));
        let failure = CommandLineTools::install(&sys).unwrap_err();
        assert!(failure.why.starts_with("stopped by signal"), "{failure:?}");
        assert_eq!(failure.class, Unfinished::Stopped);
        let calls = sys.calls();
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
        let caught = at("defer stops").expect("stops caught");
        let created = at("create /tmp/.com.apple.dt.CommandLineTools").expect("placeholder");
        assert!(
            caught < created,
            "caught before the placeholder exists: {calls:?}"
        );
        assert!(at("/usr/sbin/softwareupdate -i").is_none(), "{calls:?}");
        assert_stop_sent_after_the_placeholder_goes(&calls);
        assert!(sys.written(PLACEHOLDER).is_none());

        let unstopped = Fake::root();
        unstopped
            .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
            .on(
                "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                installed_quietly(),
            )
            .on(PKGUTIL, Ran::ok(RECEIPT));
        CommandLineTools::install(&unstopped).unwrap();
        let calls = unstopped.calls();
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("/usr/sbin/softwareupdate -i")),
            "the negative control installs"
        );
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
        let removed =
            at("remove /tmp/.com.apple.dt.CommandLineTools").expect("placeholder removed");
        let restored = at("stops restored").expect("stops restored");
        assert!(
            removed < restored,
            "the stops stay caught until the placeholder is gone: {calls:?}"
        );
        assert!(at("stop sent again").is_none(), "{calls:?}");
    }

    /// In Low Data Mode the probe plans nothing and reads nothing from Apple: the half-gigabyte
    /// download waits, saying why; with Low Data Mode off (the negative control) it plans the
    /// install.
    #[test]
    fn low_data_mode_holds_the_download_back_and_says_how_to_go_on() {
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        let mut sys = Fake::root();
        sys.low_data = Some(true);
        sys.on("/usr/bin/xcode-select -p", none_selected());
        assert_eq!(
            items[0].probe(&sys, &ctx()),
            Probe::Blocked {
                why: LOW_DATA.into()
            }
        );
        assert!(LOW_DATA.contains("`aterm init --only base.clt`"));
        assert!(
            sys.calls()
                .iter()
                .all(|c| !c.contains("curl") && !c.contains("softwareupdate")),
            "{:?}",
            sys.calls()
        );
        for low_data in [Some(false), None] {
            let mut sys = Fake::root();
            sys.low_data = low_data;
            sys.on("/usr/bin/xcode-select -p", none_selected());
            assert!(
                matches!(items[0].probe(&sys, &ctx()), Probe::Change { .. }),
                "{low_data:?}"
            );
        }
    }

    #[test]
    fn an_unlisted_catalog_recovers_without_launching_apples_installer() {
        let sys = Fake::root();
        sys.on(SOFTWAREUPDATE_LIST, Ran::ok("No new software available.\n"))
            .on(SOFTWAREUPDATE_LIST, Ran::ok("No new software available.\n"))
            .on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST))
            .on(
                "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                installed_quietly(),
            )
            .on(PKGUTIL, Ran::ok(RECEIPT));
        CommandLineTools::install(&sys).unwrap();
        let calls = sys.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
                .count(),
            3
        );
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.starts_with("/usr/sbin/softwareupdate -i"))
                .count(),
            1
        );
        assert!(calls.iter().any(|c| c == "sleep 5000ms"));
        assert!(calls.iter().any(|c| c == "sleep 15000ms"));
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("xcode-select --install") || c.contains("sudo"))
        );
        assert!(sys.written(PLACEHOLDER).is_none());
    }

    #[test]
    fn an_unlisted_catalog_reports_unavailability_without_a_gui_fallback() {
        for stop_after in [None, Some(0)] {
            let mut sys = Fake::root();
            sys.stop_after = stop_after;
            sys.on(SOFTWAREUPDATE_LIST, Ran::ok("No new software available.\n"));
            let failure = CommandLineTools::install(&sys).unwrap_err();
            assert_eq!(
                failure.class,
                if stop_after.is_some() {
                    Unfinished::Stopped
                } else {
                    Unfinished::NoneOffered
                }
            );
            let calls = sys.calls();
            assert!(!calls.iter().any(|c| c.contains("xcode-select --install")
                || c.starts_with("/usr/sbin/softwareupdate -i")));
            assert!(sys.written(PLACEHOLDER).is_none());
            if stop_after.is_none() {
                assert_eq!(
                    calls
                        .iter()
                        .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
                        .count(),
                    3
                );
            }
        }
    }

    #[test]
    fn tools_arriving_during_an_unlisted_catalog_retry_end_the_wait() {
        let sys = Fake::root();
        sys.on(SOFTWAREUPDATE_LIST, Ran::ok("No new software available.\n"));
        sys.on_sleep(|sys, _| {
            sys.stat_of(CLT_GIT, git());
        });
        CommandLineTools::install(&sys).unwrap();
        let calls = sys.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
                .count(),
            1
        );
        assert!(!calls.iter().any(|c| c.contains("xcode-select --install")
            || c.starts_with("/usr/sbin/softwareupdate -i")));
        assert!(sys.written(PLACEHOLDER).is_none());
    }

    /// Apple's window open with no placeholder beside it as the run begins (it opened beside
    /// another install's, which has ended since) holds the tools all the same: the run makes no
    /// placeholder, asks `softwareupdate` for nothing and `xcode-select --install` for no other
    /// window, and waits on that one until the tools are there. The window is found among the
    /// processes the kernel names, so one process whose executable it names none for (its file
    /// deleted) hides no window. (Reading the window through [`System::process_paths`], which is
    /// `None` once any executable is unread, misses it and fails this test.)
    /// NEGATIVE CONTROL: with no window either, the run makes its placeholder, lists the tools
    /// and installs them.
    #[test]
    fn apples_window_with_no_placeholder_is_waited_on_and_nothing_is_made_beside_it() {
        for unnamed_beside in [false, true] {
            let sys = Fake::root();
            sys.running(&["/sbin/launchd", APPLE_WINDOW])
                .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST));
            if unnamed_beside {
                sys.process(unnamed(), None);
            }
            assert_eq!(holder(&sys, false), Holder::AppleWindow, "{unnamed_beside}");
            sys.on_sleep(|sys, _| {
                if sys
                    .calls()
                    .iter()
                    .filter(|c| c.starts_with("sleep "))
                    .count()
                    == 2
                {
                    sys.stat_of(CLT_GIT, git());
                }
            });
            CommandLineTools::install(&sys).unwrap();
            let calls = sys.calls();
            assert!(
                !calls.iter().any(|c| c.starts_with(SOFTWAREUPDATE)
                    || c.starts_with("create ")
                    || c.starts_with("remove ")
                    || c.contains("xcode-select --install")),
                "{unnamed_beside}: {calls:?}"
            );
            assert_eq!(
                calls.iter().filter(|c| c.starts_with("sleep ")).count(),
                2,
                "{unnamed_beside}: it waited on the window: {calls:?}"
            );
            assert!(sys.written(PLACEHOLDER).is_none(), "{unnamed_beside}");
        }

        let control = Fake::root();
        control
            .running(&["/sbin/launchd"])
            .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
            .on(
                "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                installed_quietly(),
            )
            .on(PKGUTIL, Ran::ok(RECEIPT));
        assert_eq!(holder(&control, false), Holder::Nothing);
        CommandLineTools::install(&control).unwrap();
        let calls = control.calls();
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
        let created = at(&format!("create {PLACEHOLDER}")).expect("this install's own");
        let listed = at("/usr/sbin/softwareupdate -l").expect("the install goes on");
        let installed = at("/usr/sbin/softwareupdate -i ").expect("and installs");
        assert!(created < listed && listed < installed, "{calls:?}");
    }

    /// Apple's window raised while the run read the catalog (a `git` stub's, beside this run's
    /// placeholder, so it made none of its own) is the person's install: whether the catalog
    /// lists the tools or none, the run downloads nothing beside it and asks `xcode-select
    /// --install` for no other window, and waits on that one until the tools are there.
    #[test]
    fn a_window_raised_while_the_catalog_is_read_is_waited_on_and_nothing_installs_beside_it() {
        let unlisted =
            "Software Update Tool\n\nFinding available software\nNo new software available.\n";
        for catalog in [VM_LIST, unlisted] {
            let sys = Fake::root();
            sys.running(&["/sbin/launchd"])
                .on("/usr/sbin/softwareupdate -l", Ran::ok(catalog))
                .on_running(
                    "/usr/sbin/softwareupdate -l",
                    &["/sbin/launchd", APPLE_WINDOW],
                );
            sys.on_sleep(|sys, _| {
                sys.stat_of(CLT_GIT, git());
            });
            CommandLineTools::install(&sys).unwrap();
            let calls = sys.calls();
            assert!(
                !calls
                    .iter()
                    .any(|c| c.starts_with("/usr/sbin/softwareupdate -i")
                        || c.contains("xcode-select --install")),
                "{calls:?}"
            );
            assert!(
                calls.iter().any(|c| c.starts_with("sleep ")),
                "it waited on the window: {calls:?}"
            );
        }
    }

    /// The run's placeholder guard leaves its placeholder in place while Apple's window is listed
    /// as the run ends, when another program raises a window while the catalog is read,
    /// and however the wait ends (its patience
    /// spent, or a stop): that window made none of its own, its install needs the file to find
    /// the tools in Apple's catalog, and it unlinks the file when its request ends. Every reader
    /// then holds the tools for the window, so the next run waits on it, making, removing and
    /// asking `softwareupdate` for nothing.
    /// NEGATIVE CONTROL: the window gone before the run ends (quit, or killed outright: either
    /// leaves this run's placeholder, which the window did not make) ends the wait as closed and
    /// takes the placeholder with it.
    #[test]
    fn the_guard_keeps_its_placeholder_while_apples_window_is_listed_as_the_run_ends() {
        for (route, stop_after, class) in [
            ("raised", None, Unfinished::AppleWindow),
            ("stopped", Some(2), Unfinished::Stopped),
        ] {
            let mut sys = Fake::root();
            sys.stop_after = stop_after;
            window_comes_up(&sys);
            let failure = CommandLineTools::install(&sys).unwrap_err();
            assert_eq!(failure.class, class, "{route}: {failure:?}");
            let calls = sys.calls();
            assert!(
                !calls.iter().any(|c| c.starts_with("remove ")),
                "{route}: {calls:?}"
            );
            assert_eq!(
                sys.written(PLACEHOLDER),
                Some(stamp(&sys)),
                "{route}: kept for the window still open"
            );
            assert_eq!(holder(&sys, true), Holder::AppleWindow, "{route}");

            sys.stop_after = None;
            let next = CommandLineTools::install(&sys).unwrap_err();
            assert_eq!(next.class, Unfinished::AppleWindow, "{route}: {next:?}");
            let again = &sys.calls()[calls.len()..];
            assert!(
                !again.iter().any(|c| c.starts_with(SOFTWAREUPDATE)
                    || c.starts_with("create ")
                    || c.starts_with("remove ")
                    || c.contains("xcode-select --install")),
                "{route}: {again:?}"
            );
        }

        {
            let route = "raised";
            let gone = Fake::root();
            window_comes_up(&gone);
            gone.on_sleep(|sys, _| {
                sys.running(&["/sbin/launchd"]);
            });
            let failure = CommandLineTools::install(&gone).unwrap_err();
            assert_eq!(
                failure.class,
                Unfinished::WindowClosed,
                "{route}: {failure:?}"
            );
            assert!(
                gone.written(PLACEHOLDER).is_none(),
                "{route}: {:?}",
                gone.calls()
            );
            assert!(
                gone.calls()
                    .iter()
                    .any(|c| c == &format!("remove {PLACEHOLDER}")),
                "{route}"
            );
        }
    }

    /// Another program raises Apple's window beside this run's placeholder during its catalog read.
    fn window_comes_up(sys: &Fake) {
        sys.running(&["/sbin/launchd"])
            .on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST))
            .on_running(SOFTWAREUPDATE_LIST, &["/sbin/launchd", APPLE_WINDOW]);
    }

    /// The placeholder is removed, then the stops are restored, then the one caught is sent again.
    fn assert_stop_sent_after_the_placeholder_goes(calls: &[String]) {
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
        let removed =
            at("remove /tmp/.com.apple.dt.CommandLineTools").expect("placeholder removed");
        let restored = at("stops restored").expect("stops restored");
        let sent = at("stop sent again: signal 2").expect("the stop is sent again");
        assert!(removed < restored && restored < sent, "{calls:?}");
    }

    /// A stop during the install itself lets it finish and verify, then ends the run: the item
    /// is Ok, and the next item of the apply never starts.
    #[test]
    fn a_stop_during_the_install_finishes_it_then_ends_the_run() {
        let mut sys = Fake::root();
        sys.stop_after = Some(1);
        sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
            .on(
                "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                installed_quietly(),
            )
            .on(PKGUTIL, Ran::ok(RECEIPT));
        CommandLineTools::install(&sys).unwrap();
        let calls = sys.calls();
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("/usr/sbin/softwareupdate -i")),
            "{calls:?}"
        );
        assert_stop_sent_after_the_placeholder_goes(&calls);
    }

    #[test]
    fn the_placeholder_is_removed_even_when_softwareupdate_lists_no_tools() {
        let sys = Fake::root();
        sys.on("/usr/bin/xcode-select -p", none_selected()).on(
            "/usr/sbin/softwareupdate -l",
            Ran::ok(&VM_LIST.replace("Command Line Tools for Xcode 27.0", "Safari 27.0")),
        );
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert!(
            rows[0]
                .why
                .as_deref()
                .unwrap()
                .contains("lists no Command Line Tools")
        );
        assert!(
            sys.calls()
                .iter()
                .any(|c| c.starts_with("remove /tmp/.com.apple.dt.CommandLineTools"))
        );
        assert!(sys.written(PLACEHOLDER).is_none());
    }

    #[test]
    fn an_install_softwareupdate_refused_with_exit_0_fails_with_its_own_words() {
        let sys = Fake::root();
        sys.on("/usr/bin/xcode-select -p", none_selected())
            .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
            .on(
                "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                no_space(),
            )
            .on(PKGUTIL, no_receipt());
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Failed);
        let why = rows[0].why.as_deref().unwrap();
        assert!(why.contains("installed nothing"), "{why}");
        assert!(why.contains("9.52 GB is required"), "{why}");
        let calls = sys.calls();
        assert!(calls.iter().all(|c| !c.contains("--switch")));
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("remove /tmp/.com.apple.dt"))
        );
    }

    #[test]
    fn a_first_catalog_outage_reaches_recovery_through_the_apply_engine() {
        let sys = Fake::root();
        let install = "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'";
        sys.on("/usr/bin/xcode-select -p", none_selected())
            .on("/usr/bin/xcode-select -p", none_selected())
            .on(
                "/usr/bin/xcode-select -p",
                Ran::ok("/Library/Developer/CommandLineTools\n"),
            )
            .on(
                SOFTWAREUPDATE_LIST,
                Ran::exit(1, "catalog network unavailable"),
            )
            .on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST))
            .on(install, installed_quietly())
            .on_stat(install, Path::new(CLT_GIT), git())
            .on(PKGUTIL, Ran::ok(RECEIPT))
            .on(
                "/usr/bin/xcode-select --switch /Library/Developer/CommandLineTools",
                Ran::ok(""),
            );
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert!(matches!(items[0].probe(&sys, &ctx()), Probe::Ok { .. }));
        let calls = sys.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
                .count(),
            2
        );
        assert_eq!(calls.iter().filter(|c| c.as_str() == install).count(), 1);
        assert!(calls.iter().any(|c| c == "sleep 5000ms"));
        assert!(!calls.iter().any(|c| c.contains("curl")
            || c.contains("sudo")
            || c.contains("xcode-select --install")));
        assert!(sys.written(PLACEHOLDER).is_none());
        assert_eq!(items[0].unfinished(), None);
    }

    #[test]
    fn installed_tools_with_nothing_selected_are_only_selected() {
        let sys = Fake::root();
        sys.stat_of(CLT_GIT, git())
            .on("/usr/bin/xcode-select -p", none_selected())
            .on("/usr/bin/xcode-select -p", none_selected())
            .on(
                "/usr/bin/xcode-select -p",
                Ran::ok("/Library/Developer/CommandLineTools\n"),
            )
            .on(
                "/usr/bin/xcode-select --switch /Library/Developer/CommandLineTools",
                Ran::ok(""),
            );
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:#?}");
        assert_eq!(
            rows[0].revert.as_deref(),
            Some("/usr/bin/xcode-select --reset")
        );
        assert!(sys.calls().iter().all(|c| !c.contains("softwareupdate")));
    }

    #[test]
    fn a_selected_xcode_is_left_selected_when_the_tools_are_added() {
        let sys = Fake::root();
        sys.on(
            "/usr/bin/xcode-select -p",
            Ran::ok("/Applications/Xcode.app/Contents/Developer\n"),
        )
        .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
        .on(
            "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
            installed_quietly(),
        )
        .on(PKGUTIL, Ran::ok(RECEIPT));
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        items[0].apply(&sys, &ctx()).unwrap();
        assert!(sys.calls().iter().all(|c| !c.contains("--switch")));
    }

    #[test]
    fn false_or_absent_leaves_the_tools_alone() {
        assert!(
            expand(
                &Table {
                    command_line_tools: Some(false)
                },
                &ctx()
            )
            .unwrap()
            .is_empty()
        );
        assert!(expand(&Table::default(), &ctx()).unwrap().is_empty());
    }

    /// Apple's own installer app's executable.
    const APPLE_WINDOW: &str = "/System/Library/CoreServices/Install Command Line Developer \
                                Tools.app/Contents/MacOS/Install Command Line Developer Tools";

    /// The processes running beside the install: launchd and the one `running` names
    /// (`softwareupdate`, Apple's package `installer`, Apple's window `apple`, or a program
    /// merely called `installer`, `lookalike`), under pids from [`Fake::PIDS`] on, their starts
    /// unread until a test gives one ([`Fake::born`]).
    fn installers(sys: &Fake, running: Option<&str>) {
        let exe = match running {
            Some("softwareupdate") => Some(SOFTWAREUPDATE),
            Some("installer") => Some(INSTALLER),
            Some("apple") => Some(APPLE_WINDOW),
            Some("lookalike") => Some("/tmp/installer"),
            Some(other) => panic!("no process {other}"),
            None => None,
        };
        sys.running(&["/sbin/launchd"].into_iter().chain(exe).collect::<Vec<_>>());
    }

    #[test]
    fn a_clt_marker_an_install_holds_is_never_replaced_or_removed_by_an_install() {
        let ready = || {
            let sys = Fake::root();
            sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
                .on(
                    "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                    installed_quietly(),
                )
                .on(PKGUTIL, Ran::ok(RECEIPT));
            sys
        };

        let fresh = ready();
        CommandLineTools::install(&fresh).unwrap();
        assert!(fresh.written(PLACEHOLDER).is_none());
        assert!(
            fresh
                .calls()
                .iter()
                .any(|c| c == "/usr/sbin/softwareupdate -l")
        );
        assert!(
            fresh
                .calls()
                .iter()
                .any(|c| c.starts_with("/usr/sbin/softwareupdate -i "))
        );
        assert!(
            fresh
                .calls()
                .iter()
                .any(|c| c == &format!("remove {PLACEHOLDER}"))
        );

        let no_tools = Fake::root();
        no_tools.on("/usr/sbin/softwareupdate -l", Ran::ok("no tools listed\n"));
        assert!(CommandLineTools::install(&no_tools).is_err());
        assert!(no_tools.written(PLACEHOLDER).is_none());
        assert!(
            no_tools
                .calls()
                .iter()
                .any(|c| c == &format!("remove {PLACEHOLDER}"))
        );

        let saved = b"another install owns this marker\n";
        let target = "/unowned-clt-marker-target";
        let mut observed = Vec::new();
        for linked in [false, true] {
            let sys = ready();
            installers(&sys, Some("softwareupdate"));
            if linked {
                sys.file(target, saved).link(PLACEHOLDER, target);
                assert!(sys.stat(Path::new(PLACEHOLDER)).unwrap().symlink);
            } else {
                sys.file(PLACEHOLDER, saved);
                assert_eq!(sys.written(PLACEHOLDER).as_deref(), Some(saved.as_slice()));
            }
            let result = CommandLineTools::install(&sys);
            assert_eq!(
                result.as_ref().err().map(|failure| failure.class),
                Some(Unfinished::Busy)
            );
            let calls = sys.calls();
            let preserved = if linked {
                sys.stat(Path::new(PLACEHOLDER)).is_ok_and(|s| s.symlink)
                    && sys.written(PLACEHOLDER).is_none()
                    && sys.written(target).as_deref() == Some(saved.as_slice())
            } else {
                sys.written(PLACEHOLDER).as_deref() == Some(saved.as_slice())
            };
            observed.push((
                result.is_err(),
                preserved,
                !calls.iter().any(|c| c.starts_with(SOFTWAREUPDATE)),
                !calls.iter().any(|c| c == &format!("remove {PLACEHOLDER}")),
            ));
        }
        assert_eq!(
            observed,
            vec![(true, true, true, true); 2],
            "occupied CLT markers retain their owners"
        );
    }

    /// A second run (a second `aterm init` while the first waits on Apple's installer window,
    /// with no installer process running) takes no placeholder a live run's stamp names: only
    /// the run that started the install holds or replaces it. One whose
    /// run is gone (no such process, or one started at another time) is stale and taken, as is
    /// one whose stamp names no start (`0` matches no live process: the pid alone may be any
    /// later process's), and a run's own guard removes the placeholder only while it is still
    /// its own.
    #[test]
    fn only_the_run_that_started_the_install_holds_or_replaces_its_placeholder() {
        let ready = |stamp: &[u8]| {
            let sys = Fake::root();
            sys.file(PLACEHOLDER, stamp)
                .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
                .on(
                    "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                    installed_quietly(),
                )
                .on(PKGUTIL, Ran::ok(RECEIPT));
            installers(&sys, None);
            sys
        };
        // The first run, alive, from the start its stamp names.
        {
            let stamp = &b"aterm 4242 1000\n"[..];
            let live = ready(stamp);
            live.born(4242, 1000);
            assert!(placeholder_held_by_a_live_run(&live));
            let failure = CommandLineTools::install(&live).unwrap_err();
            assert_eq!(failure.class, Unfinished::Busy, "{failure:?}");
            assert_eq!(
                live.written(PLACEHOLDER).as_deref(),
                Some(stamp),
                "its own stays"
            );
            let calls = live.calls();
            assert!(
                !calls
                    .iter()
                    .any(|c| c.starts_with(SOFTWAREUPDATE) || c.starts_with("remove ")),
                "{calls:?}"
            );
        }
        // The first run is gone, the pid is another process's now, or the stamp names no start:
        // stale, taken.
        for (stamp, born) in [
            (&b"aterm 4242 1000\n"[..], None),
            (b"aterm 4242 1000\n", Some(999)),
            (b"aterm 4242 0\n", Some(7)),
        ] {
            let gone = ready(stamp);
            if let Some(started) = born {
                gone.born(4242, started);
            }
            assert!(!placeholder_held_by_a_live_run(&gone));
            CommandLineTools::install(&gone).unwrap();
            let calls = gone.calls();
            let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
            let removed = at(&format!("remove {PLACEHOLDER}")).expect("the stale one goes");
            let created = at(&format!("create {PLACEHOLDER}")).expect("this run's own");
            assert!(removed < created, "{calls:?}");
            assert!(gone.written(PLACEHOLDER).is_none(), "removed when it ends");
        }
        // A stamp a person's account wrote is no run's: root owns a run's placeholder.
        let forged = ready(b"aterm 4242 1000\n");
        forged.born(4242, 1000).stat_of(
            PLACEHOLDER,
            crate::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o644,
                dir: false,
                symlink: false,
                len: 16,
            },
        );
        assert!(!placeholder_held_by_a_live_run(&forged));

        // This run's guard leaves a placeholder that is no longer its own.
        let taken = Fake::root();
        let install = "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'";
        taken
            .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
            .on(install, installed_quietly())
            .on_write(install, Path::new(PLACEHOLDER), b"aterm 4243 2000\n")
            .on(PKGUTIL, Ran::ok(RECEIPT));
        installers(&taken, None);
        CommandLineTools::install(&taken).unwrap();
        assert_eq!(
            taken.written(PLACEHOLDER).as_deref(),
            Some(&b"aterm 4243 2000\n"[..]),
            "another run's placeholder stays: {:?}",
            taken.calls()
        );
        assert!(
            !taken
                .calls()
                .iter()
                .any(|c| c == &format!("remove {PLACEHOLDER}")),
            "{:?}",
            taken.calls()
        );
    }

    /// A placeholder no install holds (neither `softwareupdate` nor `installer` runs, nor Apple's
    /// installer window, nor a live run of the root half) is stale, left by an install that ended
    /// without removing it: the install takes its place instead of failing on it, and removes it
    /// when it ends, beside a program merely called `installer` too. Apple's package installer
    /// and a process table that cannot be read leave it alone: another install, or one nobody can
    /// rule out.
    #[test]
    fn a_stale_clt_marker_nothing_holds_is_replaced_by_the_install() {
        let stale = stale_marker();
        installers(&stale, None);
        CommandLineTools::install(&stale).unwrap();
        let calls = stale.calls();
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
        let removed = at(&format!("remove {PLACEHOLDER}")).expect("the stale marker goes");
        let created = at(&format!("create {PLACEHOLDER}")).expect("this install's own");
        let listed = at("/usr/sbin/softwareupdate -l").expect("the install goes on");
        assert!(removed < created && created < listed, "{calls:?}");
        assert!(stale.written(PLACEHOLDER).is_none(), "removed when it ends");

        // A program merely called `installer` holds nothing: as stale as with none.
        let beside = stale_marker();
        installers(&beside, Some("lookalike"));
        CommandLineTools::install(&beside).unwrap();
        assert!(
            beside
                .calls()
                .iter()
                .any(|c| c.starts_with("/usr/sbin/softwareupdate -i ")),
            "{:?}",
            beside.calls()
        );

        // Apple's package installer holds it: another install of the tools is running.
        let package = stale_marker();
        installers(&package, Some("installer"));
        let failure = CommandLineTools::install(&package).unwrap_err();
        assert_eq!(failure.class, Unfinished::Busy, "{failure:?}");

        // Nothing could tell whether one runs: it is left to whoever may hold it.
        let unread = stale_marker();
        assert_eq!(
            CommandLineTools::install(&unread).unwrap_err().class,
            Unfinished::Busy
        );
        assert!(unread.written(PLACEHOLDER).is_some());
    }

    /// A placeholder left by an install that ended, ready for an install that lists and installs
    /// the tools.
    fn stale_marker() -> Fake {
        let sys = Fake::root();
        sys.file(PLACEHOLDER, b"left by an install that ended\n")
            .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
            .on(
                "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                installed_quietly(),
            )
            .on(PKGUTIL, Ran::ok(RECEIPT));
        sys
    }

    /// A process the kernel names no executable for (one whose file a replacement deleted while
    /// it ran) is none of Apple's installers, which live on the sealed system volume: beside a
    /// stale placeholder whose stamped run is gone, the install takes the placeholder's place
    /// and goes on, as it would beside named programs alone.
    /// NEGATIVE CONTROL: the same table with `softwareupdate` named in it is another install
    /// running, which the install leaves alone.
    #[test]
    fn a_process_the_kernel_names_no_file_for_holds_no_placeholder() {
        let ready = |softwareupdate: bool| {
            let sys = stale_marker();
            sys.file(PLACEHOLDER, b"aterm 4242 1000\n");
            installers(&sys, softwareupdate.then_some("softwareupdate"));
            sys.process(unnamed(), None);
            sys
        };
        let unnamed_beside = ready(false);
        assert_eq!(holder(&unnamed_beside, true), Holder::Nothing);
        CommandLineTools::install(&unnamed_beside).unwrap();
        let calls = unnamed_beside.calls();
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
        let removed = at(&format!("remove {PLACEHOLDER}")).expect("the stale marker goes");
        let created = at(&format!("create {PLACEHOLDER}")).expect("this install's own");
        let installed = at("/usr/sbin/softwareupdate -i ").expect("and installs");
        assert!(removed < created && created < installed, "{calls:?}");

        let control = ready(true);
        assert_eq!(holder(&control, true), Holder::Install);
        let failure = CommandLineTools::install(&control).unwrap_err();
        assert_eq!(failure.class, Unfinished::Busy, "{failure:?}");
        assert!(
            !control
                .calls()
                .iter()
                .any(|c| c.starts_with(SOFTWAREUPDATE) || c.starts_with("remove ")),
            "{:?}",
            control.calls()
        );
    }

    /// A process the kernel names no executable for.
    fn unnamed() -> crate::port::Process {
        crate::port::Process {
            pid: Fake::PIDS + 7,
            ppid: crate::port::Process::LAUNCHD,
            terminal: false,
            exe: None,
            comm: "deleted".into(),
        }
    }

    /// Apple's installer window beside the placeholder is Apple's own flow at any age: it made
    /// that placeholder as it opened (`O_EXCL`, before any press) and removes it when its request
    /// ends, so a press hours later leaves no trace aterm reads. The install neither removes nor
    /// replaces the placeholder, never asks `softwareupdate` for the tools (a second download of
    /// them beside the window's own) nor `xcode-select --install` for another window, and waits
    /// on that window until the tools are there. A command-line installer or a live run of the
    /// root half beside the window is another install running.
    /// NEGATIVE CONTROL: the same placeholder with no window beside it is stale, and the install
    /// takes its place, lists the tools and installs them.
    #[test]
    fn apples_window_beside_the_placeholder_is_waited_on_at_any_age_and_never_replaced() {
        let window = Fake::PIDS + 1;
        let day = 24 * 60 * 60;
        for age in [Some(60), Some(3 * 60 * 60), Some(10 * day), None] {
            let sys = stale_marker();
            sys.file(PLACEHOLDER, b"");
            installers(&sys, Some("apple"));
            if let Some(age) = age {
                sys.born(window, sys.now - age);
            }
            assert_eq!(holder(&sys, true), Holder::AppleWindow, "{age:?}");
            sys.on_sleep(|sys, _| {
                if sys
                    .calls()
                    .iter()
                    .filter(|c| c.starts_with("sleep "))
                    .count()
                    == 2
                {
                    sys.stat_of(CLT_GIT, git());
                }
            });
            CommandLineTools::install(&sys).unwrap();
            let calls = sys.calls();
            assert!(
                !calls.iter().any(|c| c.starts_with(SOFTWAREUPDATE)
                    || c.starts_with("remove ")
                    || c.starts_with("create ")
                    || c.contains("xcode-select --install")),
                "{age:?}: {calls:?}"
            );
            assert_eq!(
                calls.iter().filter(|c| c.starts_with("sleep ")).count(),
                2,
                "{age:?}: it waited on the window: {calls:?}"
            );
            assert_eq!(
                sys.written(PLACEHOLDER).as_deref(),
                Some(&b""[..]),
                "{age:?}: the window's own stays"
            );
        }

        for beside in ["softwareupdate", "live run"] {
            let sys = stale_marker();
            sys.file(PLACEHOLDER, b"aterm 4242 1000\n");
            if beside == "live run" {
                installers(&sys, Some("apple"));
                sys.born(4242, 1000);
            } else {
                sys.running(&["/sbin/launchd", APPLE_WINDOW, SOFTWAREUPDATE]);
            }
            assert_eq!(holder(&sys, true), Holder::Install, "{beside}");
            let failure = CommandLineTools::install(&sys).unwrap_err();
            assert_eq!(failure.class, Unfinished::Busy, "{beside}: {failure:?}");
            assert!(
                !sys.calls().iter().any(|c| c.starts_with(SOFTWAREUPDATE)
                    || c.starts_with("remove ")
                    || c.starts_with("sleep ")),
                "{beside}: {:?}",
                sys.calls()
            );
        }

        let control = stale_marker();
        control.file(PLACEHOLDER, b"");
        installers(&control, None);
        CommandLineTools::install(&control).unwrap();
        let calls = control.calls();
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
        let removed = at(&format!("remove {PLACEHOLDER}")).expect("the stale marker goes");
        let created = at(&format!("create {PLACEHOLDER}")).expect("this install's own");
        let listed = at("/usr/sbin/softwareupdate -l").expect("the install goes on");
        let installed = at("/usr/sbin/softwareupdate -i ").expect("and installs");
        assert!(
            removed < created && created < listed && listed < installed,
            "{calls:?}"
        );
    }

    /// Apple's installer window that closes without installing the tools ends the wait on it at
    /// once, as its own class: a stub raised it while this run read the catalog, or it was open
    /// already, beside its own
    /// placeholder, beside one an earlier run kept for it, or with none. A person pressed Not
    /// Now, Cancel, or Done on Apple's own failure there (the window unlinks the placeholder,
    /// whoever made it, and quits), or quit the window before pressing Install, which leaves a
    /// placeholder aterm stamped (the window unlinks at its exit only a file it made): this run's,
    /// which its guard removes (the wait does as the window goes where its start was not read,
    /// which reads its run gone), or an earlier run's, which the wait removes as the window goes.
    /// NEGATIVE CONTROL: a window gone with Apple's own empty placeholder still there (killed
    /// outright, so it removed nothing) is no end, and the wait runs to its patience.
    #[test]
    fn apples_window_closing_without_the_tools_ends_the_wait_at_once() {
        let sleeps = |sys: &Fake| {
            sys.calls()
                .iter()
                .filter(|c| c.starts_with("sleep "))
                .count()
        };
        // Already open beside its own placeholder when the run began.
        let found = Fake::root();
        found.file(PLACEHOLDER, b"");
        installers(&found, Some("apple"));
        found.on_sleep(|sys, _| {
            let _ = sys.remove(Path::new(PLACEHOLDER));
            sys.running(&["/sbin/launchd"]);
        });
        let failure = CommandLineTools::install(&found).unwrap_err();
        assert_eq!(failure.class, Unfinished::WindowClosed, "{failure:?}");
        assert_eq!(sleeps(&found), 1, "{:?}", found.calls());

        // Already open with no placeholder beside it: this run made none to hold the wait.
        let alone = Fake::root();
        installers(&alone, Some("apple"));
        alone.on_sleep(|sys, _| {
            sys.running(&["/sbin/launchd"]);
        });
        let failure = CommandLineTools::install(&alone).unwrap_err();
        assert_eq!(failure.class, Unfinished::WindowClosed, "{failure:?}");
        assert_eq!(sleeps(&alone), 1, "{:?}", alone.calls());

        // Raised while this run read the catalog, beside this run's placeholder, which the
        // window unlinks as it closes.
        let raised = Fake::root();
        raised
            .running(&["/sbin/launchd"])
            .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
            .on_running(
                "/usr/sbin/softwareupdate -l",
                &["/sbin/launchd", APPLE_WINDOW],
            );
        raised.on_sleep(|sys, _| {
            let _ = sys.remove(Path::new(PLACEHOLDER));
            sys.running(&["/sbin/launchd"]);
        });
        let failure = CommandLineTools::install(&raised).unwrap_err();
        assert_eq!(failure.class, Unfinished::WindowClosed, "{failure:?}");
        assert_eq!(sleeps(&raised), 1, "{:?}", raised.calls());

        // Quit before Install was pressed, beside a placeholder aterm stamped, which stays: this
        // a stub raised it during the catalog read, or an earlier run, gone now, kept its
        // placeholder for it. Whether
        // this run's own start is read (its stamp then a live run's, as on a Mac) or not (a start
        // of 0, which matches no process), the stamped placeholder is gone once the run has
        // ended, removed once.
        for (route, own_start) in ["raised", "kept"]
            .into_iter()
            .flat_map(|route| [(route, false), (route, true)])
        {
            let quit = Fake::root();
            if own_start {
                quit.born(std::process::id(), 1_000);
            }
            match route {
                "raised" => {
                    quit.running(&["/sbin/launchd"])
                        .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
                        .on_running(
                            "/usr/sbin/softwareupdate -l",
                            &["/sbin/launchd", APPLE_WINDOW],
                        );
                }
                _ => {
                    quit.file(PLACEHOLDER, b"aterm 4242 1000\n");
                    installers(&quit, Some("apple"));
                    assert_eq!(holder(&quit, true), Holder::AppleWindow);
                }
            }
            quit.on_sleep(|sys, _| {
                sys.running(&["/sbin/launchd"]);
            });
            let failure = CommandLineTools::install(&quit).unwrap_err();
            assert_eq!(
                failure.class,
                Unfinished::WindowClosed,
                "{route} {own_start}: {failure:?}"
            );
            let calls = quit.calls();
            assert_eq!(sleeps(&quit), 1, "{route} {own_start}: {calls:?}");
            assert!(
                quit.written(PLACEHOLDER).is_none(),
                "{route} {own_start}: the stamped placeholder goes with the window: {calls:?}"
            );
            assert_eq!(
                calls.iter().filter(|c| c.starts_with("remove ")).count(),
                1,
                "{route} {own_start}: {calls:?}"
            );
            assert!(
                !calls
                    .iter()
                    .any(|c| c.starts_with("/usr/sbin/softwareupdate -i")),
                "{route} {own_start}: {calls:?}"
            );
        }

        let killed = Fake::root();
        killed.file(PLACEHOLDER, b"");
        installers(&killed, Some("apple"));
        killed.on_sleep(|sys, _| {
            sys.running(&["/sbin/launchd"]);
        });
        let failure = CommandLineTools::install(&killed).unwrap_err();
        assert_eq!(failure.class, Unfinished::AppleWindow, "{failure:?}");
        assert_eq!(sleeps(&killed), 900, "every 2 s for 30 minutes");
    }

    /// A placeholder a live run of aterm's stamped stays as Apple's window goes: the wait that
    /// ends `WindowClosed` removes only a stamp whose run is gone
    /// ([`remove_a_gone_runs_placeholder`]). This run's own, its start read, is still there when
    /// the wait decides the window closed (its guard removes it once the wait has returned); and
    /// another run's, which took the stale stamp an earlier run kept in the tick after the window
    /// went, is left, the wait removing nothing.
    /// NEGATIVE CONTROL: that other run gone (its start matches no process), its stamp goes with
    /// the window. The mutant `remove_a_gone_runs_placeholder` without its
    /// `holder(sys, true) == Holder::Nothing` check removes both live stamps and fails this test.
    #[test]
    fn a_live_runs_placeholder_stays_as_apples_window_goes() {
        let own = Fake::root();
        own.born(std::process::id(), 1_000);
        let own_stamp = stamp(&own);
        own.file(PLACEHOLDER, &own_stamp);
        installers(&own, Some("apple"));
        assert_eq!(holder(&own, true), Holder::Install, "this run is alive");
        own.on_sleep(|sys, _| {
            sys.running(&["/sbin/launchd"]);
        });
        let failure = CommandLineTools::wait_on_window(&own, "patience").unwrap_err();
        assert_eq!(failure.class, Unfinished::WindowClosed, "{failure:?}");
        assert_eq!(
            own.written(PLACEHOLDER).as_deref(),
            Some(own_stamp.as_slice()),
            "the wait leaves this run's own to its guard: {:?}",
            own.calls()
        );

        for live in [true, false] {
            let kept = Fake::root();
            kept.file(PLACEHOLDER, b"aterm 4242 1000\n");
            installers(&kept, Some("apple"));
            kept.on_sleep(move |sys, _| {
                sys.running(&["/sbin/launchd"]);
                sys.file(PLACEHOLDER, b"aterm 4243 2000\n");
                if live {
                    sys.born(4243, 2_000);
                }
            });
            let failure = CommandLineTools::install(&kept).unwrap_err();
            assert_eq!(
                failure.class,
                Unfinished::WindowClosed,
                "{live}: {failure:?}"
            );
            let calls = kept.calls();
            assert_eq!(
                calls.iter().filter(|c| c.starts_with("remove ")).count(),
                usize::from(!live),
                "{live}: {calls:?}"
            );
            assert_eq!(
                kept.written(PLACEHOLDER).is_some(),
                live,
                "{live}: {calls:?}"
            );
        }
    }

    /// Each way the install falls short is named by its class ([`Item::unfinished`]), which
    /// `machines here apply --plain` exits with and System State's tab says in plain words.
    #[test]
    fn each_way_the_install_falls_short_is_named_by_its_class() {
        let applied = |script: &dyn Fn(&Fake)| {
            let sys = Fake::root();
            sys.on("/usr/bin/xcode-select -p", none_selected());
            script(&sys);
            let items = expand(
                &Table {
                    command_line_tools: Some(true),
                },
                &ctx(),
            )
            .unwrap();
            let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
            assert_ne!(rows[0].outcome, Outcome::Changed, "{rows:?}");
            items[0].unfinished()
        };
        let install = "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'";
        assert_eq!(
            applied(&|sys| {
                sys.on(
                    "/usr/sbin/softwareupdate -l",
                    Ran::exit(1, "Can't connect to the Apple Software Update server."),
                );
            }),
            Some(Unfinished::Catalog)
        );
        assert_eq!(
            applied(&|sys| {
                sys.on("/usr/sbin/softwareupdate -l", Ran::ok("no tools listed\n"));
            }),
            Some(Unfinished::NoneOffered)
        );
        assert_eq!(
            applied(&|sys| {
                sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST)).on(
                    install,
                    Ran::exit(
                        1,
                        "Error downloading Command Line Tools for Xcode: network lost",
                    ),
                );
            }),
            Some(Unfinished::Download)
        );
        assert_eq!(
            applied(&|sys| {
                sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST)).on(
                    install,
                    Ran {
                        status: Some(1),
                        stdout: b"Downloading Command Line Tools\nDownloaded Command Line \
                                  Tools\nInstalling Command Line Tools\n"
                            .to_vec(),
                        stderr: b"Failed to install Command Line Tools\n".to_vec(),
                    },
                );
            }),
            Some(Unfinished::Installer)
        );
        assert_eq!(
            applied(&|sys| {
                sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
                    .on(install, no_space())
                    .on(PKGUTIL, no_receipt());
            }),
            Some(Unfinished::Installer),
            "exit 0 with nothing installed"
        );
        assert_eq!(
            applied(&|sys| {
                sys.file(PLACEHOLDER, b"");
                installers(sys, Some("softwareupdate"));
            }),
            Some(Unfinished::Busy)
        );
        assert_eq!(
            applied(&|sys| {
                sys.file(PLACEHOLDER, b"");
                installers(sys, Some("apple"));
                sys.on_sleep(|sys, _| {
                    let _ = sys.remove(Path::new(PLACEHOLDER));
                    sys.running(&["/sbin/launchd"]);
                });
            }),
            Some(Unfinished::WindowClosed)
        );
    }
}
