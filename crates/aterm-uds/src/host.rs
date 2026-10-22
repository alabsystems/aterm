// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A test the HOST cannot run ends typed, never as a pass: a GPU, a platform font, a
//! privilege, a descriptor ceiling, a macOS service or a tool the machine lacks decides
//! nothing about the code. Here, at the bottom of the graph, so every crate's tests reach
//! it, aterm-uds's own included (`aterm_spec::host` re-exports it, and aterm-verify's
//! `libtest::COULD_NOT_RUN_SENTINEL` is [`COULD_NOT_RUN`]).
//!
//! Every outside program a test, a check or the product runs is a row of [`TOOLS`]: its
//! name, WHY aterm runs it, where it is found and who provides it
//! (docs/DESIGN-init-system-2026-10-20.md §8). A gate that cannot find one ends COULD NOT
//! RUN naming that provider ([`Tool::refusal`]), so the remedy is the provider's one
//! command (for a tool the checkout pins, `targo --unverified verify provision`); never a
//! command that does not exist, and never a system-wide install.
//!
//! The one shell read of what a child holds ([`sh_held_any`]) is here too, for the same
//! reason: every crate's test that asks a child shell for its descriptors asks it this way.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

mod tools;
pub use tools::TOOLS;

/// The words that open a refusal by the machine. A test log whose every failure carries
/// them is recorded as COULD NOT RUN, not FAIL (aterm-verify's
/// `libtest::environment_refusals`, whose `COULD_NOT_RUN_SENTINEL` is this constant).
pub const COULD_NOT_RUN: &str = "aterm-gate: COULD NOT RUN";

/// The descriptor room a host preflight actually obtained. The hard limit is
/// observed again after the attempt; a refused soft raise is kept as evidence.
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

/// Raise only this process's soft descriptor limit toward `want`, bounded by
/// its inherited hard limit and the platform ceiling. Never lower either
/// limit. Children inherit the result; a test's own low-limit child can still
/// lower its soft limit afterwards.
///
/// # Errors
/// A limit read failed. A refused raise is reported in the returned room,
/// alongside the actual readback, rather than mistaken for an applied limit.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn open_file_room(want: u64) -> std::io::Result<OpenFileRoom> {
    #[cfg(target_vendor = "apple")]
    let ceiling = u64::try_from(crate::spawnfd::descriptor_bound())
        .unwrap_or(0)
        // Darwin's SDK sys/syslimits.h: OPEN_MAX. A finite kernel ceiling
        // can be lower even where the inherited hard limit is infinity.
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
            // SAFETY: a valid resource and exclusively owned out-storage.
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

/// This test process's actual soft descriptor room after a bounded raise,
/// or zero when it cannot be read. Suites can size their admission by it.
#[cfg(unix)]
#[must_use]
pub fn raise_open_files(want: u64) -> u64 {
    open_file_room(want).map_or(0, |room| room.after)
}

/// End the calling test as COULD NOT RUN: `why` names what this host lacks and, where
/// there is one, the fix.
///
/// # Panics
///
/// Always: a panic is the one outcome libtest records beside pass and fail.
#[track_caller]
pub fn could_not_run(why: impl std::fmt::Display) -> ! {
    panic!("{COULD_NOT_RUN} — {why}")
}

/// End the calling test COULD NOT RUN when this process runs as root, whose reads and
/// writes no file mode stops: `fixture` names the mode the test stands its failure up with
/// (`a mode-0 directory`), which for root stands up nothing. Returns for any other user,
/// and off Unix.
///
/// # Panics
///
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

/// Who provides a [`Tool`], and so the one command a COULD NOT RUN for it names
/// (docs/DESIGN-init-system-2026-10-20.md §8.1). No provider is Homebrew, and no remedy
/// says `brew` (§6.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// The operating system ships it, and the OS is the answer: no command adds it.
    Os,
    /// Apple's Command Line Tools (the `base.clt` item), which nobody may redistribute.
    Clt,
    /// ALab's default set (the `base.pkg.toolset` item): `program` is the program of the
    /// set that carries it.
    Toolset {
        /// The default-set program that carries this tool.
        program: &'static str,
    },
    /// A tool the checkout pins by version and sha256, which `targo --unverified verify
    /// provision` fetches or builds into this user's cache (the tooling cache,
    /// [`tooling_dir`], or the browser lane's own) (§4.4, §6.2–§6.4).
    Workspace {
        /// Which `verify provision` run provides it ([`When::command`]).
        when: When,
    },
    /// A release host's own (`ship provision` acquires and audits it).
    ReleaseHost,
    /// The Linux guest's provisioning (`verify linux`): another OS's tool.
    Guest,
    /// Nobody, yet: aterm provides no copy, and `because` says what removes the need or
    /// whose dependency it is. A §6.1 IN-HOUSE, DROP or OUTSIDE verdict whose slice has not
    /// landed (Python 3.11+ until S11's Rust verifier, node's dev lanes until S10), where
    /// naming another provider would send a person to a command that does not provide it.
    Unprovided {
        /// What removes the need, or whose dependency it is.
        because: &'static str,
    },
    /// The hardware or the session, which no command changes: what this host lacks.
    Hardware(&'static str),
}

/// Which `verify provision` run provides a [`Provider::Workspace`] row (§8.1).
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
    /// The flag that adds this row to a `verify provision` run: none for [`When::Always`].
    #[must_use]
    pub const fn flag(self) -> Option<&'static str> {
        match self {
            Self::Always => None,
            Self::Linux => Some("--linux"),
            Self::Record => Some("--record"),
        }
    }

    /// The `verify` words of the run that provides it: `verify provision`, `verify
    /// provision --linux` or `verify provision --record`.
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

/// The version of a [`Provider::Workspace`] tool the tree pins, and where `verify
/// provision` lays the program: `<tooling>/<package>-<version>/<exe>`
/// ([`Tool::pinned_in`]). The resolver reads exactly that version, so a stale copy is
/// never used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pin {
    /// What carries the program, the install directory's name before its version:
    /// `binaryen` for `wasm-opt`, `lima` for `limactl`, else the tool's own name.
    pub package: &'static str,
    /// The pinned version.
    pub version: &'static str,
    /// The program's path under `<name>-<version>/`.
    pub exe: &'static str,
    /// Built on this machine from a pinned source (fish), rather than fetched.
    pub built: bool,
    /// The hosts (`<os>-<arch>` as Rust names them, [`host_platform`]) the tree pins it
    /// for; empty for a crate targo builds wherever it runs (wasm-bindgen).
    pub hosts: &'static [&'static str],
}

impl Pin {
    /// Whether the tree pins this program for `platform` (`macos-aarch64`).
    #[must_use]
    pub fn pinned_for(&self, platform: &str) -> bool {
        self.hosts.is_empty() || self.hosts.contains(&platform)
    }
}

/// The macOS hosts a fetched or built pin covers: Apple silicon and Intel.
pub const MACOS_HOSTS: &[&str] = &["macos-aarch64", "macos-x86_64"];

/// The name of the stamp `verify provision` writes into a row's directory as it places
/// it: the record of what was fetched, held to what, and when. A program at a pinned path
/// with no stamp beside it was laid some other way and is not the pin.
pub const PIN_STAMP: &str = "stamp.json";

/// This host as a [`Pin`] names one: `<os>-<arch>` (`macos-aarch64`).
#[must_use]
pub fn host_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// One outside program aterm runs: a row of [`TOOLS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tool {
    /// The program's name: the resolver's key, and `$ATERM_TEST_<NAME>`'s.
    pub name: &'static str,
    /// Why aterm runs it: the product or test reason. `verify list` prints it, so the
    /// reason cannot drift from the gate that needs it.
    pub why: &'static str,
    /// Where it is usually installed, searched after the pinned copy and before `$PATH`.
    pub at: &'static [&'static str],
    /// What a gate that finds none says is missing, where the name alone would mislead:
    /// `Python 3.11+` for `python3`, which macOS ships older. `None` says the name.
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
    /// The variable that names this tool for the tests: `ATERM_TEST_FISH` for `fish`,
    /// `ATERM_TEST_WASM_OPT` for `wasm-opt`.
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

    /// The directory `verify provision` lays the tree's pin of this tool in under
    /// `tooling`: `<package>-<version>`; `None` for a row with no pin.
    #[must_use]
    pub fn pinned_root(&self, tooling: &Path) -> Option<PathBuf> {
        let pin = self.pin?;
        Some(tooling.join(format!("{}-{}", pin.package, pin.version)))
    }

    /// Where the tree's pin of this tool lives under `tooling`:
    /// `<package>-<version>/<exe>`; `None` for a row with no pin.
    #[must_use]
    pub fn pinned_in(&self, tooling: &Path) -> Option<PathBuf> {
        Some(self.pinned_root(tooling)?.join(self.pin?.exe))
    }

    /// [`Tool::pinned_in`] where `verify provision` placed it: the program a file, its
    /// directory's [`PIN_STAMP`] a file beside it. A copy at the path with no stamp (laid
    /// by hand, or by the retired installers) is not the pin, and nothing resolves it.
    #[must_use]
    pub fn placed_in(&self, tooling: &Path) -> Option<PathBuf> {
        let exe = self.pinned_in(tooling)?;
        let stamp = self.pinned_root(tooling)?.join(PIN_STAMP);
        (exe.is_file() && std::fs::symlink_metadata(&stamp).is_ok_and(|m| m.is_file()))
            .then_some(exe)
    }

    /// This tool on the host `lookup` describes, in the one order every gate resolves:
    /// `$ATERM_TEST_<NAME>`, then the tooling cache's pinned version where `verify
    /// provision` placed it ([`Tool::placed_in`]), then [`Tool::at`],
    /// then the first `name` on `$PATH`; `None` where none has it.
    ///
    /// The pinned copy comes before the usual install paths, so every dev machine tests
    /// the same fish. The `$PATH` scan and the variable are LOAD-BEARING: a probe of fixed
    /// paths alone found no zsh or fish installed anywhere else (a Homebrew-on-Linux
    /// prefix, a Nix profile, a release archive unpacked without root), and every live
    /// fish test then passed having run nothing — which is how a fish integration that
    /// loaded in no aterm session shipped. One resolver for every crate keeps a named tool
    /// named everywhere.
    ///
    /// # Panics
    ///
    /// When `$ATERM_TEST_<NAME>` names something that is not a file: an operator error,
    /// never an absent tool.
    #[must_use]
    pub fn find_in(&self, lookup: &Lookup) -> Option<PathBuf> {
        if let Some(named) = &lookup.named {
            let named = PathBuf::from(named);
            assert!(
                named.is_file(),
                "{} points at {named:?}, which is not a file",
                self.var()
            );
            // Absolute, as every other path this returns: a tab told
            // `SHELL=<relative>` would not run it.
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
        // A relative `$PATH` entry is no hit: it names a different file from every
        // directory the tool is later started in.
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

    /// [`Tool::find_in`], or the calling test ends COULD NOT RUN naming this tool's
    /// provider ([`Tool::refusal`]): a missing tool is never a FAIL and never a pass.
    ///
    /// # Panics
    ///
    /// When the tool is found nowhere: that is the COULD NOT RUN.
    #[must_use]
    #[track_caller]
    pub fn need_in(&self, lookup: &Lookup) -> PathBuf {
        self.find_in(lookup)
            .unwrap_or_else(|| could_not_run(self.refusal()))
    }

    /// The provider's one command for a host without this tool, or, for an [`Provider::Os`]
    /// or [`Provider::Hardware`] row, that no command adds it: [`Tool::remedy_on`] this
    /// host.
    #[must_use]
    pub fn remedy(&self) -> String {
        self.remedy_on(&host_platform())
    }

    /// [`Tool::remedy`] on the host `platform` (`<os>-<arch>`): a pinned row names its
    /// `verify provision` run only where the tree pins it for that host, so a refusal
    /// never sends a person to a run that answers "nothing is pinned".
    #[must_use]
    pub fn remedy_on(&self, platform: &str) -> String {
        match self.provider {
            Provider::Os => "it comes with the operating system, and no command adds it".into(),
            // Never `xcode-select --install`: it raises a dialog on whoever's desktop this
            // is (§1.17). `base.clt` installs them headlessly, through softwareupdate.
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
                "a release host carries it (`targo --unverified ship provision` audits one)".into()
            }
            Provider::Guest => {
                "the Linux guest provides it: `targo --unverified verify linux` runs this there"
                    .into()
            }
            Provider::Unprovided { because } => format!("aterm provides none: {because}"),
            Provider::Hardware(_) => "no command adds it".into(),
        }
    }

    /// What a gate that found no such tool says: what is missing, then [`Tool::remedy`],
    /// then the variable that names another copy. Only for a gate that resolves the tool
    /// through [`Tool::find_in`], which reads that variable; any other gate says
    /// [`Tool::remedy`] alone.
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

    /// Its provider in a word or two, as `verify list` prints it: for a
    /// [`Provider::Workspace`] row the run that provides it ([`When::verb`]: `verify
    /// provision`, `verify provision --linux`); `default set`, `OS`, `unprovided` for a
    /// [`Provider::Unprovided`] row, or `hardware` for a [`Provider::Hardware`] row.
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

    /// This row as `verify list`'s NEEDS column names it: the tool and its provider in a
    /// word (`fish (verify provision)`), or a [`Provider::Hardware`] row's own words.
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
///
/// When no row has that name: every tool a gate names is a row, so a missing one is a
/// defect in the gate, never in the host.
#[must_use]
#[track_caller]
pub fn tool(name: &str) -> &'static Tool {
    TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("{name} is no row of aterm_uds::host::TOOLS"))
}

/// The version the tree pins for the [`TOOLS`] row `name`, at compile time: one source
/// for the resolver and for the code that installs the pin (the wasm-glue oracle's
/// `BINDGEN`, and `verify provision`'s rows).
///
/// # Panics
///
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

/// `a == b`, in a `const fn`.
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
///
/// As [`tool`] and [`Tool::find_in`].
#[must_use]
#[track_caller]
pub fn find_tool(name: &str) -> Option<PathBuf> {
    tool(name).find()
}

/// [`tool`]`(name)` on this host ([`Tool::need_in`]), or the calling test ends COULD NOT
/// RUN naming its provider.
///
/// # Panics
///
/// As [`tool`] and [`Tool::need_in`].
#[must_use]
#[track_caller]
pub fn need_tool(name: &str) -> PathBuf {
    let tool = tool(name);
    tool.need_in(&Lookup::host(tool))
}

/// End the calling test COULD NOT RUN when `absent` names any tool found nowhere — called
/// once the lanes this host has have run, so a defect in one of them still FAILS, and a
/// pass never claims a tool that did not run. Each is named with its provider. Returns
/// when `absent` is empty.
///
/// # Panics
///
/// When `absent` names a tool: that is the COULD NOT RUN.
#[track_caller]
pub fn could_not_run_without(absent: &[&str]) {
    if let Some(why) = refusals_for(absent) {
        could_not_run(why);
    }
}

/// What [`could_not_run_without`] says for `absent`: each tool's [`Tool::refusal`], in
/// order; `None` when `absent` is empty. Apart, so a test reads the words without the
/// COULD NOT RUN panic, whose sentinel would record that test's own failure as the
/// machine's.
///
/// # Panics
///
/// As [`tool`].
#[must_use]
pub fn refusals_for(absent: &[&str]) -> Option<String> {
    if absent.is_empty() {
        return None;
    }
    let refusals: Vec<String> = absent.iter().map(|name| tool(name).refusal()).collect();
    Some(format!("that lane did not run: {}", refusals.join("; ")))
}

/// The name of the machine-wide directory [`Provider::Workspace`] tools install into, in
/// aterm's user cache ([`user_cache`]).
pub const TOOLING: &str = "tooling";

/// The prefix of a `verify provision` row's scratch in a cache it places into: a
/// download, an unpack or a build not yet renamed into place. One a run left (killed part
/// way) is what the build-cache reclaim's tooling pass removes a day later.
pub const TOOLING_SCRATCH: &str = ".provision-";

/// aterm's directory `name` in this user's cache, by each platform's rule: on macOS
/// `~/Library/Caches/aterm/<name>.noindex` (Spotlight skips `.noindex`), elsewhere
/// `$XDG_CACHE_HOME/aterm/<name>` when that is absolute (the XDG rule), else
/// `~/.cache/aterm/<name>`. One per machine user, whatever the checkout: the browser lane
/// unpacks its pinned browsers into `browser`, and the tools a check runs live in
/// [`TOOLING`].
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

/// The machine-wide tooling cache on this host: [`host_user_cache`]`(`[`TOOLING`]`)`, on
/// macOS `~/Library/Caches/aterm/tooling.noindex`. Every checkout and worktree reads it.
#[must_use]
pub fn tooling_dir() -> Option<PathBuf> {
    host_user_cache(TOOLING)
}

/// THE ONE SHELL PROBE OF A DESCRIPTOR ([`sh_held_any`]) for one number: the `/bin/sh`
/// condition "this shell holds descriptor `fd`".
#[cfg(all(unix, any(test, debug_assertions)))]
#[must_use]
pub fn sh_held(fd: std::os::fd::RawFd) -> String {
    sh_held_any(&[fd])
}

/// THE ONE SHELL PROBE OF A DESCRIPTOR, for every test whose child reports what it holds:
/// the `/bin/sh` condition "this shell holds one of `fds`", read from the shell's own table.
///
/// Never a path lookup of the number (`[ -e /dev/fd/N ]`, `test -e`, `stat /dev/fd/N`): on
/// Darwin that answers "absent" for an open descriptor whenever another process or thread
/// looks up the same number at that instant (the kernel's reasons are at
/// [`crate::spawnfd::descriptor_metadata`]), which read a held descriptor as gone and a
/// leaked one as clear. Measured 2026-10-20 with eight processes looking the number up:
/// `[ -e /dev/fd/3 ]` read a held fd 3 as absent in 25 of 500 shells, and fd 105 in 18 of
/// 500; this probe in 0 of 500 each. An `open` of the path (`printf … >/dev/fd/N`) is not a
/// lookup of that kind: 0 of 200 000 failed under the same contention.
///
/// A number below 10 is duplicated (`true <&N`: every sh takes a one-digit number, and
/// `true`, unlike `:`, is no special built-in whose failed redirection would end dash). The
/// rest are looked for in one listing of `/dev/fd` (a `readdir` of the shell's own table,
/// which no other lookup disturbs), whose own descriptor takes the lowest free number: below
/// each of them unless the shell holds every number under it, which a probed child,
/// holding its stdio and a few descriptors, does not.
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
            // `<&0` names its own target, which the shell skips: nothing is checked.
            0 => "{ true 1<&0; } 2>/dev/null".to_string(),
            // `2>/dev/null` would put a descriptor on 2 before the check; a failed
            // `<&2` has no stderr to say so on.
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
