// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host capability preflight, [`COULD_NOT_RUN`] sentinel, external [`TOOLS`] registry
//! ([`Tool::refusal`]), and child-shell descriptor probe ([`sh_held_any`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub mod chrome;
mod tools;
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

/// Who provides a [`Tool`], and so the one command a COULD NOT RUN for it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// The operating system ships it, and the OS is the answer.
    Os,
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
    /// Only to re-record a golden (`--record`).
    Record,
}

impl When {
    /// The flag that adds this row to a `verify provision` run.
    #[must_use]
    pub const fn flag(self) -> Option<&'static str> {
        match self {
            Self::Always => None,
            Self::Linux => Some("--linux"),
            Self::Record => Some("--record"),
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

    /// This tool on the host `lookup` describes, in the one order every gate resolves.
    ///
    /// # Panics
    /// When `$ATERM_TEST_<NAME>` names something that is not a file.
    #[must_use]
    pub fn find_in(&self, lookup: &Lookup) -> Option<PathBuf> {
        if let Some(named) = &lookup.named {
            let named = PathBuf::from(named);
            assert!(
                named.is_file(),
                "{} points at {named:?}, which is not a file",
                self.var()
            );
            return Some(std::path::absolute(&named).unwrap_or(named));
        }
        if let Some(pinned) = lookup
            .tooling
            .as_deref()
            .and_then(|tooling| self.placed_in(tooling))
        {
            return Some(pinned);
        }
        if let Some(found) = self
            .at
            .iter()
            .map(Path::new)
            .find(|candidate| candidate.is_file())
        {
            return Some(found.to_path_buf());
        }
        std::env::split_paths(lookup.path.as_ref()?)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(self.name))
            .find(|candidate| candidate.is_file())
    }

    /// [`Tool::find_in`] on this host ([`Lookup::host`]).
    #[must_use]
    pub fn find(&self) -> Option<PathBuf> {
        self.find_in(&Lookup::host(self))
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
        match self.provider {
            Provider::Os => "it comes with the operating system, and no command adds it".into(),
            Provider::Clt => "Apple's Command Line Tools provide it (`aterm init --only base.clt` \
                              installs them, asking one password in the terminal)"
                .into(),
            Provider::Toolset { program } => format!(
                "ALab's default set provides it (`aterm pkg install --default-set` installs {program})"
            ),
            Provider::Workspace { when } => {
                if let Some(pin) = self.pin.filter(|p| !p.pinned_for(platform)) {
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
            Provider::Guest => {
                "the Linux guest provides it: `targo --unverified verify linux` runs this there"
                    .into()
            }
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
            Provider::Os => "OS".to_string(),
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
                matches!(row.provider, Provider::Os | Provider::Clt),
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

/// [`user_cache`] on this host, from `$HOME` and `$XDG_CACHE_HOME`.
#[must_use]
pub fn host_user_cache(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
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
        held.push(format!(
            "{{ h=; for f in /dev/fd/*; do case ${{f#/dev/fd/}} in {}) h=1;; esac; done; [ -n \"$h\" ]; }}",
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
