// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host capability preflight, [`COULD_NOT_RUN`] sentinel, external [`TOOLS`] registry
//! ([`Tool::refusal`]), and child-shell descriptor probe ([`sh_held_any`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub mod bash;
pub mod chrome;
mod tools;
pub mod upstream;
pub use tools::TOOLS;

/// The words that open a refusal by the machine.
pub const COULD_NOT_RUN: &str = "aterm-gate: COULD NOT RUN";

/// The descriptor room a host preflight actually obtained.
#[cfg(unix)]
#[derive(Debug)]
pub struct OpenFileRoom {
    pub before: u64,
    pub after: u64,
    pub hard: u64,
    pub target: u64,
    pub refusal: Option<std::io::Error>,
}

#[cfg(unix)]
fn open_file_room_with(
    want: u64,
    ceiling: u64,
    mut read: impl FnMut() -> std::io::Result<(u64, u64)>,
    write: impl FnOnce(u64, u64) -> std::io::Result<()>,
) -> std::io::Result<OpenFileRoom> {
    let (before, hard) = read()?;
    let target = before.max(want.min(hard).min(ceiling));
    let refusal = if target > before {
        write(target, hard).err()
    } else {
        None
    };
    let (after, hard) = read().map_err(|error| {
        std::io::Error::other(format!(
            "could not read descriptor limit after preflight (before {before}, target {target}): {error}"
        ))
    })?;
    Ok(OpenFileRoom {
        before,
        after,
        hard,
        target,
        refusal,
    })
}

/// Raise only this process's soft descriptor limit toward `want`.
///
/// # Errors
/// A limit read failed.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn open_file_room(want: u64) -> std::io::Result<OpenFileRoom> {
    #[cfg(target_vendor = "apple")]
    let ceiling = u64::try_from(crate::spawnfd::descriptor_bound())
        .unwrap_or(0)
        .min(10_240);
    #[cfg(not(target_vendor = "apple"))]
    let ceiling = u64::MAX;
    open_file_room_with(
        want,
        ceiling,
        || {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: A valid resource and exclusively owned out-storage.
            if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok((limit.rlim_cur, limit.rlim_max))
        },
        |soft, hard| {
            let limit = libc::rlimit {
                rlim_cur: soft,
                rlim_max: hard,
            };
            // SAFETY: a valid resource and initialized storage; only the
            // soft value changes, within the observed hard ceiling.
            if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        },
    )
}

/// This test process's actual soft descriptor room after a bounded raise.
#[cfg(unix)]
#[must_use]
pub fn raise_open_files(want: u64) -> u64 {
    open_file_room(want).map_or(0, |room| room.after)
}

/// End the calling test as COULD NOT RUN.
///
/// # Panics
/// Always: a panic is the one outcome libtest records beside pass and fail.
#[track_caller]
pub fn could_not_run(why: impl std::fmt::Display) -> ! {
    panic!("{COULD_NOT_RUN} — {why}")
}

/// End the calling test COULD NOT RUN when this process runs as root.
///
/// # Panics
/// As root: that is the COULD NOT RUN.
#[track_caller]
pub fn could_not_run_as_root(fixture: &str) {
    #[cfg(unix)]
    if crate::peer::our_uid() == 0 {
        could_not_run(format!(
            "as root, {fixture} refuses nothing (root reads and writes through any mode); run the test as another user"
        ));
    }
    #[cfg(not(unix))]
    let _ = fixture;
}

/// End the calling test COULD NOT RUN in a process under sshd's Redirection Guard. OpenSSH for
/// Windows enforces `ProcessRedirectionTrustPolicy` on itself, and every process of an SSH
/// session inherits it: such a process follows no junction a non-administrator made ("File Not
/// Found", or os error 448, an untrusted mount point), so a test that makes one and goes through
/// it, or holds aterm's code to not going through it, decides nothing there. `fixture` says what
/// the test makes.
///
/// # Panics
/// Under the guard: that is the COULD NOT RUN.
#[cfg(all(windows, any(test, debug_assertions)))]
#[track_caller]
pub fn could_not_run_under_redirection_guard(fixture: &str) {
    if session::redirection_guarded() {
        could_not_run(format!(
            "{fixture}, and this process inherits sshd's Redirection Guard \
             (ProcessRedirectionTrustPolicy: EnforceRedirectionTrust), under which no junction a \
             non-administrator made is followed; run it outside sshd's processes, in a desktop \
             session (`lab windows exec --desktop`)"
        ));
    }
}

/// End the calling test COULD NOT RUN in session 0, the services' session, where an SSH
/// session's processes run too: it has no interactive window station (an unnamed
/// `CreateWindowStationW` answers "Access is denied" there) and no display. `fixture` says what
/// the test needs of a desktop.
///
/// # Panics
/// In session 0: that is the COULD NOT RUN.
#[cfg(all(windows, any(test, debug_assertions)))]
#[track_caller]
pub fn could_not_run_in_session_zero(fixture: &str) {
    if session::id() == Some(0) {
        could_not_run(format!(
            "{fixture}, and this process runs in session 0 (where an SSH session's processes \
             run), which has no interactive window station or display; run it in a desktop \
             session (`lab windows exec --desktop`)"
        ));
    }
}

/// What a Windows test reads of the session it runs in.
#[cfg(all(windows, any(test, debug_assertions)))]
mod session {
    use crate::win::ffi;

    /// `PROCESS_MITIGATION_POLICY::ProcessRedirectionTrustPolicy`.
    const REDIRECTION_TRUST: i32 = 16;
    /// `PROCESS_MITIGATION_REDIRECTION_TRUST_POLICY`'s `EnforceRedirectionTrust` bit.
    const ENFORCE: u32 = 1;

    /// The session this process runs in; `None` where Windows does not say.
    pub(super) fn id() -> Option<u32> {
        let mut session = 0;
        // SAFETY: this process's own id and a live out pointer.
        let ok = unsafe { ffi::ProcessIdToSessionId(ffi::GetCurrentProcessId(), &mut session) };
        (ok != 0).then_some(session)
    }

    /// Whether this process enforces Redirection Trust; `false` where Windows has no such
    /// policy to read.
    pub(super) fn redirection_guarded() -> bool {
        let mut flags: u32 = 0;
        // SAFETY: the current-process pseudo-handle, and a live buffer of exactly the policy's
        // size, one `DWORD` of flags.
        let ok = unsafe {
            ffi::GetProcessMitigationPolicy(
                ffi::GetCurrentProcess(),
                REDIRECTION_TRUST,
                (&raw mut flags).cast(),
                std::mem::size_of::<u32>(),
            )
        };
        ok != 0 && flags & ENFORCE != 0
    }
}

/// Who provides a [`Tool`], and so the one command a COULD NOT RUN for it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// A Unix operating system ships it (macOS, or for a Linux lane Linux), and that OS is
    /// the answer there. Windows ships none: a Windows place in `at` is where its usual
    /// third-party copy lives.
    Os,
    /// Every operating system aterm runs on ships it: a Unix one as [`Provider::Os`] says, and
    /// Windows as `<name>.exe` on its `PATH` (System32's curl, tar, OpenSSH and hostname).
    OsAndWindows,
    /// Windows ships it, and Windows is the answer.
    Windows,
    /// Apple's Command Line Tools (the `base.clt` item), which nobody may redistribute.
    Clt,
    /// ALab's default set (the `base.pkg.toolset` item).
    Toolset {
        /// The default-set program that carries this tool.
        program: &'static str,
    },
    /// A tool the checkout pins by version and sha256.
    Workspace {
        /// Which `verify provision` run provides it ([`When::command`]).
        when: When,
    },
    /// A release host's own (`ship provision` acquires and audits it).
    ReleaseHost,
    /// The Linux guest's provisioning (`verify linux`).
    Guest,
    /// Nobody, yet: aterm provides no copy.
    Unprovided {
        /// What removes the need, or whose dependency it is.
        because: &'static str,
    },
    /// The hardware or the session, which no command changes.
    Hardware(&'static str),
}

/// Which `verify provision` run provides a [`Provider::Workspace`] row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    /// Every run: what testing a checkout needs.
    Always,
    /// The Linux lane's own (`--linux`).
    Linux,
    /// Only to prepare a release host (`--release`).
    Release,
}

impl When {
    /// The flag that adds this row to a `verify provision` run.
    #[must_use]
    pub const fn flag(self) -> Option<&'static str> {
        match self {
            Self::Always => None,
            Self::Linux => Some("--linux"),
            Self::Release => Some("--release"),
        }
    }

    /// The `verify` words of the run that provides it.
    #[must_use]
    pub fn verb(self) -> String {
        match self.flag() {
            None => "verify provision".to_string(),
            Some(flag) => format!("verify provision {flag}"),
        }
    }

    /// The whole command: `targo --unverified ` and [`When::verb`].
    #[must_use]
    pub fn command(self) -> String {
        format!("targo --unverified {}", self.verb())
    }
}

/// The version of a [`Provider::Workspace`] tool the tree pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pin {
    /// What carries the program, the install directory's name before its version.
    pub package: &'static str,
    /// The pinned version.
    pub version: &'static str,
    /// The program's path under `<name>-<version>/`.
    pub exe: &'static str,
    /// Built on this machine from a pinned source (fish), rather than fetched.
    pub built: bool,
    /// The hosts (`<os>-<arch>` as Rust names them, [`host_platform`]) the tree pins it for.
    pub hosts: &'static [&'static str],
}

impl Pin {
    /// Whether the tree pins this program for `platform` (`macos-aarch64`).
    #[must_use]
    pub fn pinned_for(&self, platform: &str) -> bool {
        self.hosts.is_empty() || self.hosts.contains(&platform)
    }
}

/// The macOS hosts a fetched or built pin covers.
pub const MACOS_HOSTS: &[&str] = &["macos-aarch64", "macos-x86_64"];

/// The name of the stamp `verify provision` writes into a row's directory as it places it.
pub const PIN_STAMP: &str = "stamp.json";

/// This host as a [`Pin`] names one.
#[must_use]
pub fn host_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// One outside program aterm runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tool {
    /// The program's name: the resolver's key.
    pub name: &'static str,
    /// Why aterm runs it: the product or test reason.
    pub why: &'static str,
    /// Where it is usually installed, searched after the pinned copy and before `$PATH`.
    pub at: &'static [&'static str],
    /// What a gate that finds none says is missing, where the name alone would mislead.
    pub missing: Option<&'static str>,
    /// The tree's pin in the tooling cache, for a [`Provider::Workspace`] row that has one.
    pub pin: Option<Pin>,
    /// Who provides it.
    pub provider: Provider,
    /// The Linux lane's guest provides this row too, at the first of `at`: it builds a
    /// [`Provider::Workspace`] row's pin, and lays a Mac's copy of an [`Provider::Os`] row only
    /// macOS ships. On a Linux host without it (for a pin, one the pin does not cover), the
    /// guest is its provider ([`Tool::remedy_on`]).
    pub in_guest: bool,
    /// The package a Linux distribution adds this [`Provider::Os`] row by, where its base
    /// install has none (Debian's and Ubuntu's `zsh`): a Linux host without it is sent to its
    /// package manager ([`Tool::remedy_on`]).
    pub linux_package: Option<&'static str>,
}

/// How a refusal sends a test to the Linux lane's guest.
const GUEST_RUNS: &str = "`targo --unverified verify linux` runs this there";

/// A fixed place or `$PATH` entry a lookup may take: on Unix a file with an execute bit, as a
/// shell's `$PATH` search and `execvp` take one, so a mode-0644 `rustup` early on `$PATH` is
/// passed over for the next.
fn runnable(candidate: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(candidate)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        candidate.is_file()
    }
}

/// Why [`Tool::locate_in`] found no copy, and the provider's one command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Absent {
    /// What was looked at ([`Tool::looked_in`]), or the `$ATERM_TEST_<NAME>` that names no file.
    pub why: String,
    /// [`Tool::remedy`]; for a `$ATERM_TEST_<NAME>` that names no file, unsetting it first.
    pub fix: String,
}

impl std::fmt::Display for Absent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.why, self.fix)
    }
}

/// What a resolver reads from its environment, injected so a test can stand up any host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lookup {
    /// `$ATERM_TEST_<NAME>`.
    pub named: Option<OsString>,
    /// The tooling cache ([`tooling_dir`]).
    pub tooling: Option<PathBuf>,
    /// `$PATH`.
    pub path: Option<OsString>,
}

impl Lookup {
    /// This process's: `$ATERM_TEST_<NAME>` for `tool`, the tooling cache and `$PATH`.
    #[must_use]
    pub fn host(tool: &Tool) -> Self {
        Self {
            named: std::env::var_os(tool.var()),
            tooling: tooling_dir(),
            path: std::env::var_os("PATH"),
        }
    }
}

impl Tool {
    /// The variable that names this tool for the tests.
    #[must_use]
    pub fn var(&self) -> String {
        let name: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("ATERM_TEST_{name}")
    }

    /// The directory `verify provision` lays the tree's pin of this tool in under `tooling`.
    #[must_use]
    pub fn pinned_root(&self, tooling: &Path) -> Option<PathBuf> {
        let pin = self.pin?;
        Some(tooling.join(format!("{}-{}", pin.package, pin.version)))
    }

    /// Where the tree's pin of this tool lives under `tooling`.
    #[must_use]
    pub fn pinned_in(&self, tooling: &Path) -> Option<PathBuf> {
        Some(self.pinned_root(tooling)?.join(self.pin?.exe))
    }

    /// [`Tool::pinned_in`] where `verify provision` placed it.
    #[must_use]
    pub fn placed_in(&self, tooling: &Path) -> Option<PathBuf> {
        let exe = self.pinned_in(tooling)?;
        let stamp = self.pinned_root(tooling)?.join(PIN_STAMP);
        (exe.is_file() && std::fs::symlink_metadata(&stamp).is_ok_and(|m| m.is_file()))
            .then_some(exe)
    }

    /// The one resolution order: `Err` when `$ATERM_TEST_<NAME>` names no file.
    fn resolve(&self, lookup: &Lookup) -> Result<Option<PathBuf>, String> {
        if let Some(named) = &lookup.named {
            let named = PathBuf::from(named);
            if !named.is_file() {
                return Err(format!(
                    "{} points at {named:?}, which is not a file",
                    self.var()
                ));
            }
            return Ok(Some(std::path::absolute(&named).unwrap_or(named)));
        }
        if let Some(pinned) = lookup
            .tooling
            .as_deref()
            .and_then(|tooling| self.placed_in(tooling))
        {
            return Ok(Some(pinned));
        }
        if let Some(found) = self.places().map(Path::new).find(|at| runnable(at)) {
            return Ok(Some(found.to_path_buf()));
        }
        let Some(path) = lookup.path.as_ref() else {
            return Ok(None);
        };
        let file = self.path_file_on(std::env::consts::OS);
        Ok(std::env::split_paths(path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(&file))
            .find(|candidate| runnable(candidate)))
    }

    /// The places in `at` a host whose OS is `os` (`macos`, `linux`, `windows`, as
    /// [`std::env::consts::OS`] names it) can name: off Windows the `/`-rooted ones, on Windows
    /// the rest (`C:\…`). Neither family's path is a place on the other: a Windows path is a
    /// relative name on Unix, and a `/`-rooted one names no drive on Windows.
    pub fn places_on(&self, os: &str) -> impl Iterator<Item = &'static str> + use<> {
        let windows = os == "windows";
        self.at
            .iter()
            .copied()
            .filter(move |at| at.starts_with('/') != windows)
    }

    /// [`Tool::places_on`] this host.
    pub fn places(&self) -> impl Iterator<Item = &'static str> + use<> {
        self.places_on(std::env::consts::OS)
    }

    /// The file a `$PATH` directory holds this tool as on a host whose OS is `os`
    /// ([`Tool::places_on`]): a program Windows ships is `<name>.exe` there, where the row's
    /// name carries no extension; any other is the row's name. A Unix row's name is not looked
    /// for as an `.exe` on Windows, where System32's `bash.exe` is WSL's.
    #[must_use]
    pub fn path_file_on(&self, os: &str) -> String {
        let ships = matches!(self.provider, Provider::OsAndWindows | Provider::Windows);
        if ships && os == "windows" && Path::new(self.name).extension().is_none() {
            format!("{}.exe", self.name)
        } else {
            self.name.to_string()
        }
    }

    /// This tool on the host `lookup` describes, in the one order every gate resolves.
    ///
    /// # Panics
    /// When `$ATERM_TEST_<NAME>` names something that is not a file.
    #[must_use]
    pub fn find_in(&self, lookup: &Lookup) -> Option<PathBuf> {
        self.resolve(lookup).unwrap_or_else(|why| panic!("{why}"))
    }

    /// [`Tool::find_in`] on this host ([`Lookup::host`]).
    #[must_use]
    pub fn find(&self) -> Option<PathBuf> {
        self.find_in(&Lookup::host(self))
    }

    /// [`Tool::find_in`] for a check that reports rather than panics: found, or what was
    /// looked at (or the `$ATERM_TEST_<NAME>` that names no file) and the provider's one
    /// command.
    ///
    /// # Errors
    /// Found nowhere, or `$ATERM_TEST_<NAME>` names something that is not a file.
    pub fn locate_in(&self, lookup: &Lookup) -> Result<PathBuf, Absent> {
        match self.resolve(lookup) {
            Ok(Some(found)) => Ok(found),
            Ok(None) => Err(Absent {
                why: format!(
                    "no {} ({})",
                    self.missing.unwrap_or(self.name),
                    self.looked_in(lookup.tooling.as_deref())
                ),
                fix: self.remedy(),
            }),
            Err(why) => Err(Absent {
                why,
                fix: format!(
                    "unset {} or point it at a {}, since it outranks every other place; \
                     without it, {}",
                    self.var(),
                    self.name,
                    self.remedy()
                ),
            }),
        }
    }

    /// [`Tool::locate_in`] on this host ([`Lookup::host`]).
    ///
    /// # Errors
    /// As [`Tool::locate_in`].
    pub fn locate(&self) -> Result<PathBuf, Absent> {
        self.locate_in(&Lookup::host(self))
    }

    /// Where [`Tool::find_in`] looks, in its order: the variable, the stamped pin (under
    /// `tooling`, where that is known), the usual paths, then `$PATH`.
    #[must_use]
    pub fn looked_in(&self, tooling: Option<&Path>) -> String {
        let mut places = vec![format!("${}", self.var())];
        if let Some(pin) = self.pin {
            places.push(match tooling.and_then(|t| self.pinned_in(t)) {
                Some(exe) => format!("the stamped {} at {}", pin.version, exe.display()),
                None => format!("the stamped {} in this user's tooling cache", pin.version),
            });
        }
        places.extend(self.places().map(str::to_string));
        places.push("$PATH".to_string());
        let last = places.pop().unwrap_or_default();
        if places.is_empty() {
            format!("looked in {last}")
        } else {
            format!("looked in {} and {last}", places.join(", "))
        }
    }

    /// [`Tool::find_in`], or the calling test ends COULD NOT RUN naming this tool's provider.
    ///
    /// # Panics
    /// When the tool is found nowhere.
    #[must_use]
    #[track_caller]
    pub fn need_in(&self, lookup: &Lookup) -> PathBuf {
        self.find_in(lookup)
            .unwrap_or_else(|| could_not_run(self.refusal()))
    }

    /// The provider's one command for a host without this tool.
    #[must_use]
    pub fn remedy(&self) -> String {
        self.remedy_on(&host_platform())
    }

    /// [`Tool::remedy`] on the host `platform` (`<os>-<arch>`).
    #[must_use]
    pub fn remedy_on(&self, platform: &str) -> String {
        let os = platform.split_once('-').map_or(platform, |(os, _)| os);
        let windows = os == "windows";
        match self.provider {
            Provider::Os if windows => {
                let looked: Vec<&str> = self.places_on(os).collect();
                if looked.is_empty() {
                    "Windows ships none, and aterm provides none".into()
                } else {
                    format!(
                        "Windows ships none, and aterm looks for one at {}",
                        looked.join(" or ")
                    )
                }
            }
            Provider::Os if self.in_guest && os == "linux" => format!(
                "the Linux guest lays a Mac's copy at {}: {GUEST_RUNS}",
                self.places_on(os).next().unwrap_or(self.name)
            ),
            Provider::Os if os == "linux" && self.linux_package.is_some() => format!(
                "the Linux distribution's package manager adds it, as `sudo apt install {}` does \
                 on Debian and Ubuntu",
                self.linux_package.unwrap_or(self.name)
            ),
            Provider::OsAndWindows | Provider::Windows if windows => {
                "it comes with Windows, and no command adds it".into()
            }
            Provider::Os | Provider::OsAndWindows => {
                "it comes with the operating system, and no command adds it".into()
            }
            Provider::Windows => format!("only Windows ships it, and this host is {platform}"),
            Provider::Clt => "Apple's Command Line Tools provide it (`aterm init --only base.clt` \
                              installs them, asking one password in the terminal)"
                .into(),
            Provider::Toolset { program } => format!(
                "ALab's default set provides it (`aterm pkg install --default-set` installs {program})"
            ),
            Provider::Workspace { when } => {
                if let Some(pin) = self.pin.filter(|p| !p.pinned_for(platform)) {
                    if self.in_guest && platform.starts_with("linux-") {
                        return format!(
                            "the Linux guest builds the pinned {} as {}: {GUEST_RUNS}",
                            pin.version,
                            self.at.first().copied().unwrap_or(self.name)
                        );
                    }
                    return format!(
                        "aterm provides none for {platform}: `{}` pins it for {} only",
                        when.command(),
                        pin.hosts.join(" and ")
                    );
                }
                let does = match self.pin {
                    Some(pin) if pin.built => format!("builds the pinned {}", pin.version),
                    Some(pin) => format!("fetches the pinned {}", pin.version),
                    None => "fetches the pinned build".to_string(),
                };
                format!("`{}` {does} into this user's cache", when.command())
            }
            Provider::ReleaseHost => {
                "a release host carries it (`targo --unverified ship provision --id <machine-id> \
                 --check` audits one)"
                    .into()
            }
            Provider::Guest => format!("the Linux guest provides it: {GUEST_RUNS}"),
            Provider::Unprovided { because } => format!("aterm provides none: {because}"),
            Provider::Hardware(_) => "no command adds it".into(),
        }
    }

    /// What a gate that found no such tool says.
    #[must_use]
    pub fn refusal(&self) -> String {
        self.refusal_on(&host_platform())
    }

    /// [`Tool::refusal`] on the host `platform` ([`Tool::remedy_on`]).
    #[must_use]
    pub fn refusal_on(&self, platform: &str) -> String {
        match self.provider {
            Provider::Hardware(what) => format!("this host lacks {what}, and no command adds it"),
            _ => format!(
                "no {} here: {} (or {}=<path> names one)",
                self.missing.unwrap_or(self.name),
                self.remedy_on(platform),
                self.var()
            ),
        }
    }

    /// Its provider in a word or two, as `verify list` prints it.
    #[must_use]
    pub fn provided_by(&self) -> String {
        match self.provider {
            Provider::Os | Provider::OsAndWindows => "OS".to_string(),
            Provider::Windows => "Windows".to_string(),
            Provider::Clt => "CLT".to_string(),
            Provider::Toolset { .. } => "default set".to_string(),
            Provider::Workspace { when } => when.verb(),
            Provider::ReleaseHost => "release host".to_string(),
            Provider::Guest => "Linux guest".to_string(),
            Provider::Unprovided { .. } => "unprovided".to_string(),
            Provider::Hardware(_) => "hardware".to_string(),
        }
    }

    /// This row as `verify list`'s NEEDS column names it.
    #[must_use]
    pub fn needs_label(&self) -> String {
        match self.provider {
            Provider::Hardware(what) => what.to_string(),
            _ => format!("{} ({})", self.name, self.provided_by()),
        }
    }
}

/// The [`TOOLS`] row named `name`.
///
/// # Panics
/// When no row has that name.
#[must_use]
#[track_caller]
pub fn tool(name: &str) -> &'static Tool {
    TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("{name} is no row of aterm_uds::host::TOOLS"))
}

/// The version the tree pins for the [`TOOLS`] row `name`, at compile time.
///
/// # Panics
/// At compile time, when no pinned row has that name.
#[must_use]
pub const fn pinned_version(name: &str) -> &'static str {
    let mut i = 0;
    while i < TOOLS.len() {
        let row = &TOOLS[i];
        if same_str(row.name, name)
            && let Some(pin) = row.pin
        {
            return pin.version;
        }
        i += 1;
    }
    panic!("no pinned row of aterm_uds::host::TOOLS has that name")
}

/// Return the fixed absolute path for OS/CLT tool `name`, or the bare name off Unix.
///
/// # Panics
///
/// When no Os or Clt row has that name, or the row lists no path (the root tools only
/// aterm-machines may spell, which `$PATH` finds for the read-only uses): at compile time
/// under [`os_path!`](crate::os_path).
#[must_use]
pub const fn os_path(name: &'static str) -> &'static str {
    let mut i = 0;
    while i < TOOLS.len() {
        let row = &TOOLS[i];
        if same_str(row.name, name) {
            assert!(
                matches!(
                    row.provider,
                    Provider::Os | Provider::OsAndWindows | Provider::Clt
                ),
                "an aterm_uds::host::TOOLS row the OS does not provide has no fixed path"
            );
            assert!(
                !row.at.is_empty(),
                "this aterm_uds::host::TOOLS row lists no path: `$PATH` finds it"
            );
            return if cfg!(unix) { row.at[0] } else { name };
        }
        i += 1;
    }
    panic!("no row of aterm_uds::host::TOOLS has that name")
}

/// Evaluate [`os_path`](crate::host::os_path)`(name)` at compile time.
#[macro_export]
macro_rules! os_path {
    ($name:literal) => {
        const { $crate::host::os_path($name) }
    };
}

const fn same_str(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// [`tool`]`(name)` on this host ([`Tool::find`]).
///
/// # Panics
/// As [`tool`] and [`Tool::find_in`].
#[must_use]
#[track_caller]
pub fn find_tool(name: &str) -> Option<PathBuf> {
    tool(name).find()
}

/// [`tool`]`(name)` on this host ([`Tool::need_in`]).
///
/// # Panics
/// As [`tool`] and [`Tool::need_in`].
#[must_use]
#[track_caller]
pub fn need_tool(name: &str) -> PathBuf {
    let tool = tool(name);
    tool.need_in(&Lookup::host(tool))
}

/// End the calling test COULD NOT RUN when `absent` names any tool found nowhere.
///
/// # Panics
/// When `absent` names a tool found nowhere.
#[track_caller]
pub fn could_not_run_without(absent: &[&str]) {
    if let Some(why) = refusals_for(absent) {
        could_not_run(why);
    }
}

/// What [`could_not_run_without`] says for `absent`.
///
/// # Panics
/// As [`tool`].
#[must_use]
pub fn refusals_for(absent: &[&str]) -> Option<String> {
    if absent.is_empty() {
        return None;
    }
    let refusals: Vec<String> = absent.iter().map(|name| tool(name).refusal()).collect();
    Some(format!("that lane did not run: {}", refusals.join("; ")))
}

/// The name of the machine-wide directory [`Provider::Workspace`] tools install into.
pub const TOOLING: &str = "tooling";

/// The prefix of a `verify provision` row's scratch in a cache it places into: a
/// download, an unpack or a build not yet renamed into place.
pub const TOOLING_SCRATCH: &str = ".provision-";

/// Aterm's directory `name` in this user's cache, by each platform's rule.
#[must_use]
pub fn user_cache(
    os: &str,
    home: Option<&Path>,
    xdg_cache: Option<&Path>,
    name: &str,
) -> Option<PathBuf> {
    if os == "macos" {
        return home.map(|h| h.join(format!("Library/Caches/aterm/{name}.noindex")));
    }
    xdg_cache
        .filter(|p| p.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| home.map(|h| h.join(".cache")))
        .map(|c| c.join("aterm").join(name))
}

/// Select the installed check tools independently of a test's disposable HOME.
#[must_use]
pub fn tool_home() -> Option<PathBuf> {
    tool_home_from(
        std::env::var_os("ATERM_TEST_TOOL_HOME"),
        std::env::var_os("HOME"),
    )
}

fn tool_home_from(selected: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    selected
        .filter(|p| Path::new(p).is_absolute())
        .or(home)
        .map(PathBuf::from)
}

/// [`user_cache`] on this host, from the selected tool home and `$XDG_CACHE_HOME`.
#[must_use]
pub fn host_user_cache(name: &str) -> Option<PathBuf> {
    let home = tool_home();
    let xdg = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from);
    user_cache(std::env::consts::OS, home.as_deref(), xdg.as_deref(), name)
}

/// The machine-wide tooling cache on this host.
#[must_use]
pub fn tooling_dir() -> Option<PathBuf> {
    host_user_cache(TOOLING)
}

/// The shell probe of a descriptor ([`sh_held_any`]) for one number.
#[cfg(all(unix, any(test, debug_assertions)))]
#[must_use]
pub fn sh_held(fd: std::os::fd::RawFd) -> String {
    sh_held_any(&[fd])
}

/// The shell probe of descriptors, for every test whose child reports what it holds.
///
/// # Panics
/// When `fds` is empty.
#[cfg(all(unix, any(test, debug_assertions)))]
#[must_use]
pub fn sh_held_any(fds: &[std::os::fd::RawFd]) -> String {
    use std::os::fd::RawFd;
    assert!(!fds.is_empty(), "a probe names at least one number");
    let (low, high): (Vec<RawFd>, Vec<RawFd>) = fds.iter().partition(|&&fd| fd < 10);
    let mut held: Vec<String> = low
        .iter()
        .map(|&fd| match fd {
            0 => "{ true 1<&0; } 2>/dev/null".to_string(),
            2 => "true <&2".to_string(),
            _ => format!("{{ true <&{fd}; }} 2>/dev/null"),
        })
        .collect();
    if !high.is_empty() {
        let numbers = high.iter().map(ToString::to_string).collect::<Vec<_>>();
        // Reserve a free low number for the directory walk. Without this,
        // its own handle can impersonate a missing high descriptor in a dense
        // table. The subshell leaves the caller's descriptor 3 untouched.
        held.push(format!(
            "( exec 3<&-; h=; for f in /dev/fd/*; do case ${{f#/dev/fd/}} in {}) h=1;; esac; done; [ -n \"$h\" ] )",
            numbers.join("|")
        ));
    }
    format!("{{ {}; }}", held.join(" || "))
}

/// The `/bin/sh -c` script that exits 3 when the shell holds `fd` ([`sh_held`]), else 0.
#[cfg(all(unix, any(test, debug_assertions)))]
#[must_use]
pub fn held_probe(fd: std::os::fd::RawFd) -> String {
    format!("{} && exit 3; exit 0", sh_held(fd))
}

#[cfg(test)]
mod tests;
