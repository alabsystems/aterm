// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Workspace tool provisioning (`targo --unverified verify provision`,
//! [`aterm_uds::host::Provider::Workspace`], [`ROWS`], [`place`]).

mod fish;
mod place;
mod rows;
pub mod stable;
#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use aterm_uds::host::When;

pub use place::SCRATCH_PREFIX;
pub use rows::{Anchor, Archive, Engine, Holds, Install, MICROSOFT_TEAM, ROWS, Row, row};
pub use stable::{FOREIGN_CELL_TARGETS, WasmClause, wasm_clause};

/// System `curl` binary path.
pub(crate) const CURL: &str = "/usr/bin/curl";

/// Stamp filename beside a pinned program ([`aterm_uds::host::PIN_STAMP`]).
pub const STAMP: &str = aterm_uds::host::PIN_STAMP;

/// What a placed row's `stamp.json` records.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stamp {
    /// The row.
    pub row: String,
    /// The version placed.
    pub version: String,
    /// Where it came from.
    #[serde(default)]
    pub url: Option<String>,
    /// The archive's sha256 it was held to.
    #[serde(default)]
    pub sha256: Option<String>,
    /// What the tree was held to besides: `sha256`, `developer-id <team>`, `crates.io`.
    pub anchor: String,
    /// A build's features (fish: none past `--no-default-features`).
    #[serde(default)]
    pub features: Option<Vec<String>>,
    /// What the placed program answered (`fish --version`).
    #[serde(default)]
    pub reports: Option<String>,
    /// The host it was placed for (`macos-aarch64`).
    #[serde(default)]
    pub platform: Option<String>,
    /// When it was placed (Unix seconds).
    pub placed: u64,
    /// Set by the retired adoption.
    #[serde(default)]
    pub adopted: bool,
}

/// The current Unix time in seconds (`0` where the clock says nothing).
pub(crate) fn now() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// Child command specification recorded for a row ([`Host::run`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spawn {
    /// The program, by absolute path.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// Where it runs.
    pub cwd: PathBuf,
    /// Variables set for it.
    pub set: Vec<(String, OsString)>,
    /// Variables removed from what it inherits; a name ending in `*` is a prefix.
    pub remove: Vec<String>,
}

impl Spawn {
    /// The command this spawn is, over the parent environment's variable names `parent`.
    #[must_use]
    pub fn command(&self, parent: &[OsString]) -> aterm_uds::spawnfd::Command {
        let mut cmd = aterm_uds::spawnfd::command(&self.program);
        cmd.args(&self.args).current_dir(&self.cwd);
        for key in &self.remove {
            if let Some(prefix) = key.strip_suffix('*') {
                for name in parent {
                    if name.to_string_lossy().starts_with(prefix) {
                        cmd.env_remove(name);
                    }
                }
            } else {
                cmd.env_remove(key);
            }
        }
        for (key, value) in &self.set {
            cmd.env(key, value);
        }
        cmd
    }
}

/// What the rows do to the outside world.
pub trait Host {
    /// Download `url` into `dest`.
    ///
    /// # Errors
    /// The download failed.
    fn fetch(&self, url: &str, dest: &Path, bytes: u64) -> Result<(), String>;

    /// Whether the Mach-O at `path` verifies as signed by a Developer ID of `team`.
    ///
    /// # Errors
    /// It does not, or codesign could not say.
    fn developer_id(&self, path: &Path, team: &str) -> Result<(), String>;

    /// Run `spawn` to completion: its stdout.
    ///
    /// # Errors
    /// It could not start, its output/read or wait failed, or it exited unsuccessfully
    /// (with its stderr's tail). An error need not prove the child's exit or first exec.
    fn run(&self, spawn: &Spawn) -> Result<String, String>;

    /// The Trust toolchain's `targo`.
    ///
    /// # Errors
    /// None is installed: what provides it.
    fn targo(&self) -> Result<PathBuf, String>;

    /// The rustup the rustup row drives.
    fn rustup(&self) -> &dyn stable::Rustup;
}

/// Where a run puts things on this machine, and for which host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    /// `<os>-<arch>` as Rust names them (`macos-aarch64`): the machine's own ([`platform_of`]).
    pub platform: String,
    /// The tooling cache ([`aterm_uds::host::tooling_dir`]).
    pub tooling: PathBuf,
    /// The browser lane's cache.
    pub browser: PathBuf,
    /// The checkout, whose `publish/config.sh` names the wasm32 pin.
    pub repo: PathBuf,
    /// rustup's homes.
    pub homes: stable::Homes,
}

impl Machine {
    fn os_arch(&self) -> (&str, &str) {
        self.platform
            .split_once('-')
            .unwrap_or((&self.platform, ""))
    }
}

/// Where a row stands after a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// In place already, its stamp the pin's: nothing was fetched.
    Present(PathBuf),
    /// Fetched or built, checked and placed by this run.
    Placed(PathBuf),
    /// Nothing is pinned for this host: no failure, and nothing to provide.
    NotPinned(String),
    /// `--check`: not in place, and what a run would do.
    Missing(String),
    /// Refused or failed, with nothing left behind.
    Failed(String),
    /// A pinned file upstream no longer serves to this fetch (Ubuntu archived its release):
    /// COULD NOT RUN, naming what changes that, with nothing left behind.
    CouldNotRun(String),
}

/// Why a row was not placed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// It failed or was refused: [`State::Failed`].
    Failed(String),
    /// Upstream no longer serves a pinned file here: [`State::CouldNotRun`].
    CouldNotRun(String),
}

impl From<String> for Refused {
    fn from(why: String) -> Self {
        Self::Failed(why)
    }
}

impl From<Refused> for String {
    fn from(refused: Refused) -> Self {
        match refused {
            Refused::Failed(why) | Refused::CouldNotRun(why) => why,
        }
    }
}

impl State {
    /// Whether the row is where the tests look for it.
    #[must_use]
    pub const fn in_place(&self) -> bool {
        matches!(self, Self::Present(_) | Self::Placed(_))
    }
}

/// What a run was asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Rows named on the command line; empty: every wanted row.
    pub rows: Vec<String>,
    /// `--linux`: add the Linux lane's rows.
    pub linux: bool,
    /// `--record`: add the rows that re-record goldens.
    pub record: bool,
    /// `--check`: read only.
    pub check: bool,
    /// `-h`/`--help`.
    pub help: bool,
}

/// The usage text.
#[must_use]
pub fn usage() -> String {
    let names: Vec<&str> = ROWS.iter().map(|r| r.name).collect();
    format!(
        "targo --unverified verify provision [ROW...] [--linux] [--record] [--check]

Fetch or build the tools this checkout pins into this user's cache, each placed whole
or not at all beside a stamp.json; each download is held to a pinned sha256 (pwsh's
also to Microsoft's Developer ID), and wasm-bindgen builds from crates.io. The rustup
row instead has rustup add stable, its cross targets and the wasm32 pin to rustup's
homes, first installing rustup from a pinned rustup-init where there is none. It
reads the network.

  (no ROW)    the rows every checkout needs
  --linux     and the Linux lane's VM: lima, and Ubuntu's pinned image for its own VM
  --record    and wasm-bindgen, to re-record the wasm-glue goldens
  --check     say what is missing; fetch and change nothing
  ROW...      just these: {}

Exit: 0 each row asked for and pinned here is in place, 1 a row failed (--check: is
missing), 2 usage, 3 could not run (no $HOME, no row asked for is pinned here, or
upstream no longer serves a pinned file).
",
        names.join(" ")
    )
}

/// Parse what follows `verify provision`.
///
/// # Errors
/// An unknown flag or row: the usage error's words.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut opts = Options::default();
    for arg in args {
        match arg.as_str() {
            "--linux" => opts.linux = true,
            "--record" => opts.record = true,
            "--check" => opts.check = true,
            "-h" | "--help" => opts.help = true,
            flag if flag.starts_with('-') => {
                return Err(format!("verify provision: unknown flag `{flag}`"));
            }
            name if row(name).is_some() => {
                if !opts.rows.iter().any(|r| r == name) {
                    opts.rows.push(name.to_string());
                }
            }
            name => {
                let names: Vec<&str> = ROWS.iter().map(|r| r.name).collect();
                return Err(format!(
                    "verify provision: no row `{name}` (the rows: {})",
                    names.join(", ")
                ));
            }
        }
    }
    Ok(opts)
}

/// The rows `opts` asks for, in [`ROWS`] order.
#[must_use]
pub fn select(opts: &Options) -> Vec<&'static Row> {
    ROWS.iter()
        .filter(|r| {
            if opts.rows.is_empty() {
                match r.when() {
                    When::Always => true,
                    When::Linux => opts.linux,
                    When::Record => opts.record,
                }
            } else {
                opts.rows.iter().any(|n| n == r.name)
            }
        })
        .collect()
}

/// Whether `t.dir` holds the pinned program, every file the row holds to a size at that
/// size, and a stamp that [`Target::admits`].
pub(crate) fn complete(t: &Target) -> bool {
    t.dir.join(&t.exe).is_file()
        && short(t).is_none()
        && place::read_stamp(&t.dir).is_some_and(|s| t.admits(&s))
}

/// The first file of `t.sizes` that is not in `t.dir` as a regular file of its size.
fn short(t: &Target) -> Option<&str> {
    t.sizes
        .iter()
        .find(|(name, bytes)| {
            !std::fs::symlink_metadata(t.dir.join(name))
                .is_ok_and(|m| m.is_file() && m.len() == *bytes)
        })
        .map(|(name, _)| name.as_str())
}

/// Returns `engine`'s executable on `machine` when present with a matching pin stamp ([`complete`]).
pub(crate) fn placed_browser(engine: Engine, machine: &Machine) -> Option<PathBuf> {
    let t = target(row(engine_name(engine))?, machine)?;
    complete(&t).then(|| t.dir.join(&t.exe))
}

/// Where `row` would be placed on `machine`, with what its stamp must say.
fn target(row: &Row, machine: &Machine) -> Option<Target> {
    let (os, arch) = machine.os_arch();
    match row.install {
        Install::Unpack { .. } | Install::Build { .. } | Install::Cargo | Install::Files { .. } => {
            let tool = row.tool_row()?;
            let pin = tool.pin?;
            if !pin.pinned_for(&machine.platform) {
                return None;
            }
            let sha256 = match row.install {
                Install::Cargo => None,
                Install::Files { .. } => {
                    let files = row.files_for(&machine.platform);
                    (!files.is_empty()).then(|| files_digest(&files, pin.version))
                }
                _ => Some(row.archive_for(&machine.platform)?.sha256.to_string()),
            };
            let features = match row.install {
                Install::Build { features, .. } => {
                    Some(features.iter().map(|f| (*f).to_string()).collect())
                }
                _ => None,
            };
            let sizes = match row.install {
                Install::Unpack {
                    holds: Holds::Image { bytes },
                    ..
                } => vec![(pin.exe.to_string(), bytes)],
                Install::Files { .. } => row
                    .files_for(&machine.platform)
                    .iter()
                    .map(|f| (place::file_name(&f.url_at(pin.version)), f.bytes))
                    .collect(),
                _ => Vec::new(),
            };
            Some(Target {
                dir: tool.pinned_root(&machine.tooling)?,
                exe: pin.exe.to_string(),
                version: pin.version.to_string(),
                sha256,
                platform: machine.platform.clone(),
                features,
                sizes,
            })
        }
        Install::Browser(engine) => {
            let pin = crate::browser::pin_for(engine, os, arch, &machine.browser)?;
            Some(Target {
                dir: pin.dir,
                exe: pin.binary.to_string_lossy().into_owned(),
                version: pin.version,
                sha256: Some(pin.sha256.to_string()),
                platform: machine.platform.clone(),
                features: None,
                sizes: Vec::new(),
            })
        }
        Install::Rustup { .. } => None,
    }
}

/// A row's directory and program, and what its stamp must say.
pub(crate) struct Target {
    dir: PathBuf,
    exe: String,
    version: String,
    sha256: Option<String>,
    platform: String,
    features: Option<Vec<String>>,
    /// Files in `dir` the row is in place only with, each at its size: an image row's
    /// image, every file of a files row.
    sizes: Vec<(String, u64)>,
}

impl Target {
    /// Whether `stamp` records this pin placed for this host.
    fn admits(&self, stamp: &Stamp) -> bool {
        !stamp.adopted
            && stamp.version == self.version
            && stamp.platform.as_deref() == Some(self.platform.as_str())
            && self
                .sha256
                .as_deref()
                .is_none_or(|h| stamp.sha256.as_deref() == Some(h))
            && self
                .features
                .as_ref()
                .is_none_or(|f| stamp.features.as_ref() == Some(f))
    }

    /// The stamp of this pin placed now from `url`, held to `anchor`.
    fn stamp(
        &self,
        row: &str,
        url: Option<String>,
        anchor: &str,
        reports: Option<String>,
    ) -> Stamp {
        Stamp {
            row: row.into(),
            version: self.version.clone(),
            url,
            sha256: self.sha256.clone(),
            anchor: anchor.into(),
            features: self.features.clone(),
            reports,
            platform: Some(self.platform.clone()),
            placed: now(),
            adopted: false,
        }
    }
}

/// Why `row` has nothing to provide on `machine`.
fn not_pinned(row: &Row, machine: &Machine) -> String {
    format!("{}: nothing is pinned for {}", row.name, machine.platform)
}

/// Where `row` stands on `machine`, read only: in place, missing, or not pinned here.
pub fn check(row: &Row, machine: &Machine, host: &dyn Host) -> State {
    if matches!(row.install, Install::Rustup { .. }) {
        let pin = match stable::wasm_pin_wanted(&machine.repo) {
            Ok(pin) => pin,
            Err(why) => return State::Missing(format!("rustup: the wasm32 pin — {why}")),
        };
        let lacks = stable::missing(&machine.homes, host.rustup(), pin.as_deref());
        return if lacks.is_empty() {
            State::Present(machine.homes.rustup_home.clone())
        } else {
            State::Missing(format!("rustup: a run would {}", lacks.join("; ")))
        };
    }
    let Some(t) = target(row, machine) else {
        return State::NotPinned(not_pinned(row, machine));
    };
    if complete(&t) {
        State::Present(t.dir.join(&t.exe))
    } else if let Some(name) = short(&t).filter(|_| t.dir.join(&t.exe).is_file()) {
        State::Missing(format!(
            "{} {}: {} lacks {name} at its pinned size, which a run fetches again",
            row.name,
            t.version,
            t.dir.display()
        ))
    } else if t.dir.join(&t.exe).is_file() {
        State::Missing(format!(
            "{} {}: {} holds a copy without this pin's stamp for {}, which a run replaces",
            row.name,
            t.version,
            t.dir.display(),
            t.platform
        ))
    } else {
        State::Missing(format!(
            "{} {}: not in {}",
            row.name,
            t.version,
            t.dir.display()
        ))
    }
}

/// Provide `row` on `machine`: leave it where it is complete, else fetch or build it.
pub fn provide(row: &Row, machine: &Machine, host: &dyn Host, say: &mut dyn FnMut(&str)) -> State {
    if let Install::Rustup { version, .. } = row.install {
        return rustup_row(row, version, machine, host, say);
    }
    let Some(t) = target(row, machine) else {
        return State::NotPinned(not_pinned(row, machine));
    };
    let exe = t.dir.join(&t.exe);
    if complete(&t) {
        place::touch_stamp(&t.dir);
        return State::Present(exe);
    }
    let done = match row.install {
        Install::Unpack { .. } => {
            let archive = row
                .archive_for(&machine.platform)
                .expect("target() found it");
            unpack_row(row, &archive, &t, machine, host, say)
        }
        Install::Files { .. } => files_row(
            row,
            &row.files_for(&machine.platform),
            &t,
            machine,
            host,
            say,
        ),
        Install::Build { .. } => {
            let archive = row
                .archive_for(&machine.platform)
                .expect("target() found it");
            fish::build(&archive, &t, machine, host, say).map_err(Refused::from)
        }
        Install::Cargo => return cargo_row(row, &t, machine, host, say),
        Install::Browser(engine) => return browser_row(engine, &t, machine, host, say),
        Install::Rustup { .. } => unreachable!("handled above"),
    };
    match done {
        Ok(exe) => State::Placed(exe),
        Err(Refused::Failed(why)) => State::Failed(format!("{} {}: {why}", row.name, t.version)),
        Err(Refused::CouldNotRun(why)) => {
            State::CouldNotRun(format!("{} {}: {why}", row.name, t.version))
        }
    }
}

/// The rustup row: [`stable::run`], handed rustup-init at the pin where no rustup is installed.
fn rustup_row(
    row: &Row,
    version: &str,
    machine: &Machine,
    host: &dyn Host,
    say: &mut dyn FnMut(&str),
) -> State {
    let fetched = if stable::lacks_rustup(&machine.homes) {
        let fetched = (|| -> Result<(place::Scratch, PathBuf), String> {
            let archive = row.archive_for(&machine.platform).ok_or_else(|| {
                format!(
                    "no rustup-init {version} is pinned for {}",
                    machine.platform
                )
            })?;
            let scratch = place::Scratch::new(&machine.tooling, row.name)?;
            let url = archive.url_at(version);
            let init =
                place::fetch_checked(host, &scratch, &url, archive.sha256, archive.bytes, say)?;
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&init, std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| format!("chmod {}: {e}", init.display()))?;
            }
            Ok((scratch, init))
        })();
        match fetched {
            Ok(fetched) => Some(fetched),
            Err(why) => return State::Failed(format!("rustup: rustup was not installed — {why}")),
        }
    } else {
        None
    };
    let init = fetched.as_ref().map(|(_, init)| init.as_path());
    match stable::run(&machine.repo, &machine.homes, host.rustup(), init, say) {
        Ok(acquired) if acquired.is_empty() => State::Present(machine.homes.rustup_home.clone()),
        Ok(_) => State::Placed(machine.homes.rustup_home.clone()),
        Err(why) => match fetched {
            Some((scratch, _)) => State::Failed(format!(
                "rustup: {why}; keeping rustup-init's scratch at {}",
                scratch.keep().display()
            )),
            None => State::Failed(format!("rustup: {why}")),
        },
    }
}

/// An unpacked row: fetch, check, unpack under its cap, hold the tree to its anchor and its
/// pinned file to what [`Holds`] says it is, keep its `keep` files, stamp, place.
fn unpack_row(
    row: &Row,
    archive: &Archive,
    t: &Target,
    machine: &Machine,
    host: &dyn Host,
    say: &mut dyn FnMut(&str),
) -> Result<PathBuf, Refused> {
    let Install::Unpack {
        strip,
        anchor,
        keep,
        cap,
        holds,
        ..
    } = row.install
    else {
        unreachable!("provide() passes unpacked rows only")
    };
    let url = archive.url_at(&t.version);
    let scratch = place::Scratch::new(&machine.tooling, row.name)?;
    let file = place::fetch_checked(host, &scratch, &url, archive.sha256, archive.bytes, say)?;
    let cap = cap.unwrap_or(place::UNPACKED_CAP);
    let tree = place::unpack(
        &scratch,
        &file,
        place::container_of(&url)?,
        strip,
        cap,
        "tree",
    )?;
    place::links_within(&tree)?;
    let pinned = tree.join(&t.exe);
    match holds {
        Holds::Program if !place::is_executable(&pinned) => {
            return Err(format!("{url} holds no executable {}", t.exe).into());
        }
        Holds::Program => {}
        Holds::Image { bytes } => {
            match std::fs::symlink_metadata(&pinned) {
                Ok(m) if m.file_type().is_file() && m.len() == bytes => {}
                Ok(m) if m.file_type().is_file() => {
                    return Err(format!(
                        "{url} holds {} of {} bytes, not the pinned image's {bytes}",
                        t.exe,
                        m.len()
                    )
                    .into());
                }
                _ => return Err(format!("{url} holds no image {} (a regular file)", t.exe).into()),
            }
            place::read_only(&pinned)?;
        }
    }
    for name in keep {
        if !tree.join(name).is_file() {
            return Err(format!("{url} holds no {name}, which the tree keeps").into());
        }
    }
    let anchor_words = match anchor {
        Anchor::Sha256 => "sha256".to_string(),
        Anchor::DeveloperId { team } => {
            let machos = atpkg::macho::find_macho(&tree).map_err(|e| e.to_string())?;
            if machos.is_empty() {
                return Err(format!("{url} holds no Mach-O to hold to team {team}").into());
            }
            for macho in &machos {
                let shown = macho
                    .strip_prefix(&tree)
                    .unwrap_or(macho)
                    .display()
                    .to_string();
                host.developer_id(macho, team)
                    .map_err(|e| format!("{shown} is not signed by team {team}: {e}"))?;
            }
            say(&format!(
                "{} {}: all {} Mach-O files verify as Developer ID team {team}",
                row.name,
                t.version,
                machos.len()
            ));
            format!("developer-id {team}")
        }
    };
    place::write_stamp(&tree, &t.stamp(row.name, Some(url), &anchor_words, None))?;
    place::place(&scratch, &tree, &t.dir, &|| complete(t))?;
    Ok(t.dir.join(&t.exe))
}

/// The stamp's `sha256` of a files row at `version`: the sha256 of its `SHA256SUMS`, one
/// `<sha256> *<name>` line a file in row order, so a changed pin of any file is another
/// row's stamp.
fn files_digest(files: &[Archive], version: &str) -> String {
    let mut sums = aterm_digest::Sha256::new();
    for file in files {
        sums.update(
            format!(
                "{} *{}\n",
                file.sha256,
                place::file_name(&file.url_at(version))
            )
            .as_bytes(),
        );
    }
    sums.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A files row: fetch each file into a download directory of its own and hold it to its
/// pin, lay it read-only under its name, stamp, and place them together.
fn files_row(
    row: &Row,
    files: &[Archive],
    t: &Target,
    machine: &Machine,
    host: &dyn Host,
    say: &mut dyn FnMut(&str),
) -> Result<PathBuf, Refused> {
    let scratch = place::Scratch::new(&machine.tooling, row.name)?;
    let tree = scratch.fresh("tree")?;
    let mut urls = Vec::new();
    for file in files {
        let url = file.url_at(&t.version);
        let got = place::fetch_checked(host, &scratch, &url, file.sha256, file.bytes, say)?;
        let kept = tree.join(place::file_name(&url));
        std::fs::rename(&got, &kept)
            .map_err(|e| format!("could not lay {}: {e}", kept.display()))?;
        place::read_only(&kept)?;
        urls.push(url);
    }
    if !tree.join(&t.exe).is_file() {
        return Err(format!("the row's files hold no {}", t.exe).into());
    }
    let url = urls.first().cloned();
    place::write_stamp(&tree, &t.stamp(row.name, url, "sha256", None))?;
    place::place(&scratch, &tree, &t.dir, &|| complete(t))?;
    Ok(t.dir.join(&t.exe))
}

fn cargo_row(
    row: &Row,
    t: &Target,
    machine: &Machine,
    host: &dyn Host,
    say: &mut dyn FnMut(&str),
) -> State {
    let exe = t.dir.join(&t.exe);
    let built = (|| -> Result<(), String> {
        let scratch = place::Scratch::new(&machine.tooling, row.name)?;
        let root = scratch.path().join("tree");
        let targo = host.targo()?;
        let mut args: Vec<OsString> = vec!["--unverified".into()];
        args.extend(crate::wasm_glue::BINDGEN_INSTALL.iter().map(OsString::from));
        args.extend([
            "--root".into(),
            root.clone().into_os_string(),
            "--target-dir".into(),
            scratch.path().join("target.noindex").into_os_string(),
        ]);
        say(&format!(
            "building wasm-bindgen-cli {}: targo --unverified install --locked",
            t.version
        ));
        let install = Spawn {
            program: targo,
            args,
            cwd: scratch.path().to_path_buf(),
            set: Vec::new(),
            remove: fish::removed(),
        };
        let scratch = scratch.run_build(host, &install)?;
        let built = root.join(&t.exe);
        let probe = Spawn {
            program: built.clone(),
            args: vec!["--version".into()],
            cwd: scratch.path().to_path_buf(),
            set: Vec::new(),
            remove: Vec::new(),
        };
        let (scratch, out) = scratch
            .run_copied(host, &probe)
            .map_err(|e| format!("{} did not answer --version: {e}", built.display()))?;
        if !crate::wasm_glue::reports_pin(&out) {
            return Err(format!("{} does not answer {}", built.display(), t.version));
        }
        let reports = out.trim().to_string();
        let url = format!("https://crates.io/crates/wasm-bindgen-cli/{}", t.version);
        place::write_stamp(
            &root,
            &t.stamp(row.name, Some(url), "crates.io", Some(reports)),
        )?;
        place::place(&scratch, &root, &t.dir, &|| complete(t))
    })();
    match built {
        Ok(()) => State::Placed(exe),
        Err(why) => State::Failed(format!("{} {}: {why}", row.name, t.version)),
    }
}

fn browser_row(
    engine: Engine,
    t: &Target,
    machine: &Machine,
    host: &dyn Host,
    say: &mut dyn FnMut(&str),
) -> State {
    let (os, arch) = machine.os_arch();
    let Some(pin) = crate::browser::pin_for(engine, os, arch, &machine.browser) else {
        return State::NotPinned(format!("nothing is pinned for {}", machine.platform));
    };
    match place_browser(engine_name(engine), &pin, t, machine, host, say) {
        Ok(()) => State::Placed(t.dir.join(&t.exe)),
        Err(why) => State::Failed(format!("{}: {why}", pin.what)),
    }
}

fn place_browser(
    name: &str,
    pin: &crate::browser::Pin,
    t: &Target,
    machine: &Machine,
    host: &dyn Host,
    say: &mut dyn FnMut(&str),
) -> Result<(), String> {
    let (os, _) = machine.os_arch();
    let scratch = place::Scratch::new(&machine.tooling, name)?;
    let zip = place::fetch_checked(host, &scratch, &pin.url, pin.sha256, pin.bytes, say)?;
    let tree = scratch.path().join("tree");
    let unzip = if os == "macos" {
        Spawn {
            program: PathBuf::from("/usr/bin/ditto"),
            args: vec![
                "-x".into(),
                "-k".into(),
                zip.into_os_string(),
                tree.clone().into_os_string(),
            ],
            cwd: scratch.path().to_path_buf(),
            set: Vec::new(),
            remove: Vec::new(),
        }
    } else {
        let unzip = aterm_uds::host::find_tool("unzip")
            .ok_or_else(|| aterm_uds::host::tool("unzip").refusal())?;
        Spawn {
            program: unzip,
            args: vec![
                "-q".into(),
                zip.into_os_string(),
                "-d".into(),
                tree.clone().into_os_string(),
            ],
            cwd: scratch.path().to_path_buf(),
            set: Vec::new(),
            remove: Vec::new(),
        }
    };
    host.run(&unzip)?;
    place::links_within(&tree)?;
    if !tree.join(&pin.binary).is_file() {
        return Err(format!("the archive holds no {}", pin.binary.display()));
    }
    place::write_stamp(&tree, &t.stamp(name, Some(pin.url.clone()), "sha256", None))?;
    place::place(&scratch, &tree, &t.dir, &|| complete(t))
}

const fn engine_name(engine: Engine) -> &'static str {
    match engine {
        Engine::Chromium => "chrome-headless-shell",
        Engine::Firefox => "firefox",
        Engine::Webkit => "webkit",
    }
}

/// One row's line, as a run prints it.
#[must_use]
pub fn line(row: &Row, state: &State) -> String {
    match state {
        State::Present(p) => format!("{} — in place: {}", row.name, p.display()),
        State::Placed(p) => format!("{} — placed: {}", row.name, p.display()),
        State::NotPinned(why) => format!("{} — not provided: {why}", row.name),
        State::Missing(why) => format!("MISSING — {why}"),
        State::Failed(why) => format!("FAIL — {why}"),
        State::CouldNotRun(why) => format!("COULD NOT RUN — {why}"),
    }
}

/// Check or provision each selected row in order ([`crate::exit`]).
pub fn run(opts: &Options, machine: &Machine, host: &dyn Host, say: &mut dyn FnMut(&str)) -> i32 {
    let rows = select(opts);
    let mut states = Vec::new();
    for row in &rows {
        let state = if opts.check {
            check(row, machine, host)
        } else {
            provide(row, machine, host, &mut |l: &str| {
                say(&format!("{}: {l}", row.name))
            })
        };
        say(&line(row, &state));
        states.push(state);
    }
    let failed = states
        .iter()
        .filter(|s| matches!(s, State::Failed(_) | State::Missing(_)))
        .count();
    let unserved = states
        .iter()
        .filter(|s| matches!(s, State::CouldNotRun(_)))
        .count();
    let placed = states.iter().filter(|s| s.in_place()).count();
    if failed > 0 {
        say(&format!(
            "FAIL — {failed} of {} rows not in place",
            rows.len()
        ));
        crate::exit::FAILED
    } else if unserved > 0 {
        say(&format!(
            "COULD NOT RUN — {unserved} of {} rows pin a file upstream no longer serves here",
            rows.len()
        ));
        crate::exit::COULD_NOT_RUN
    } else if placed == 0 {
        say(&format!(
            "COULD NOT RUN — none of the rows asked for is pinned for {}",
            machine.platform
        ));
        crate::exit::COULD_NOT_RUN
    } else {
        let rows = if placed == 1 { "row" } else { "rows" };
        say(&format!("PASS — {placed} {rows} in place"));
        crate::exit::PASS
    }
}

/// This machine.
pub struct System {
    repo: PathBuf,
    rustup: stable::System,
}

impl System {
    /// This machine, for the checkout at `repo`.
    #[must_use]
    pub fn new(repo: &Path) -> Self {
        Self {
            repo: repo.to_path_buf(),
            rustup: stable::System,
        }
    }
}

fn tail(text: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(text);
    let lines: Vec<&str> = text.trim_end().lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// The download of `url` into `dest`.
pub(crate) fn fetch_spawn(url: &str, dest: &Path, bytes: u64) -> Spawn {
    let bytes = bytes.to_string();
    Spawn {
        program: PathBuf::from(CURL),
        args: [
            "-q",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "-sSfL",
            "--max-filesize",
            &bytes,
            "--retry",
            "0",
            "--connect-timeout",
            "30",
            "--speed-limit",
            "1024",
            "--speed-time",
            "60",
            "-o",
        ]
        .into_iter()
        .map(OsString::from)
        .chain([dest.as_os_str().to_owned(), url.into()])
        .collect(),
        cwd: dest.parent().unwrap_or(Path::new("/")).to_path_buf(),
        set: Vec::new(),
        remove: Vec::new(),
    }
}

impl Host for System {
    fn fetch(&self, url: &str, dest: &Path, bytes: u64) -> Result<(), String> {
        self.run(&fetch_spawn(url, dest, bytes)).map(|_| ())
    }

    fn developer_id(&self, path: &Path, team: &str) -> Result<(), String> {
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let deadline =
            std::time::Instant::now() + aterm_update_core::codesign::verify_timeout_for(len);
        aterm_update_core::codesign::verify_developer_id_until(path, team, false, deadline)
            .map_err(|e| e.to_string())
    }

    fn run(&self, spawn: &Spawn) -> Result<String, String> {
        use aterm_uds::output::CommandOutput as _;
        let parent: Vec<OsString> = std::env::vars_os().map(|(k, _)| k).collect();
        let out = spawn
            .command(&parent)
            .output_owned()
            .map_err(|e| format!("could not run {}: {e}", spawn.program.display()))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!(
                "{} exited {:?}: {}",
                spawn.program.display(),
                out.status.code(),
                tail(&out.stderr, 30)
            ))
        }
    }

    fn targo(&self) -> Result<PathBuf, String> {
        let toolchain = crate::run_toolchain(&crate::EnvSnapshot::capture(), &self.repo);
        if toolchain.targo.is_file() {
            Ok(toolchain.targo)
        } else {
            Err(format!(
                "no Trust targo at {}: {}",
                toolchain.targo.display(),
                aterm_uds::host::tool("targo").remedy()
            ))
        }
    }

    fn rustup(&self) -> &dyn stable::Rustup {
        &self.rustup
    }
}

/// The host a run provisions for.
#[must_use]
pub fn platform_of(os: &str, arch: &str, translated: bool) -> String {
    if os == "macos" && arch == "x86_64" && translated {
        "macos-aarch64".into()
    } else {
        format!("{os}-{arch}")
    }
}

/// Whether this process runs translated by Rosetta (`sysctl.proc_translated`).
fn translated() -> bool {
    #[cfg(target_vendor = "apple")]
    {
        aterm_uds::process::translation() == aterm_uds::process::Translation::Translated
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        false
    }
}

/// This machine's [`Machine`] for the checkout at `repo`; `None` where no `$HOME` names the caches.
#[must_use]
pub fn this_machine(repo: &Path) -> Option<Machine> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)?;
    Some(Machine {
        platform: platform_of(std::env::consts::OS, std::env::consts::ARCH, translated()),
        tooling: aterm_uds::host::tooling_dir()?,
        browser: crate::browser::chromium::host_cache_root()?,
        repo: repo.to_path_buf(),
        homes: stable::Homes::from_env(&home),
    })
}

/// `verify provision`'s whole CLI, from `cwd`.
#[must_use]
pub fn main(args: &[String], cwd: &Path) -> i32 {
    let opts = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}\n\n{}", usage());
            return crate::exit::USAGE;
        }
    };
    if opts.help {
        print!("{}", usage());
        return crate::exit::PASS;
    }
    let repo = crate::checkout_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let Some(machine) = this_machine(&repo) else {
        eprintln!("verify provision: COULD NOT RUN — no $HOME names this user's cache");
        return crate::exit::COULD_NOT_RUN;
    };
    run(&opts, &machine, &System::new(&repo), &mut |l| {
        eprintln!("verify provision: {l}");
    })
}
