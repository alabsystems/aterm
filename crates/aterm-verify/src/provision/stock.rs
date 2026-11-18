// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify provision`'s rustup row for the checkout's stock pin and the release chain's stock
//! toolchain, driven by the rustup the `rustup` row of [`aterm_uds::host::TOOLS`] resolves (the
//! row's stamped copy, once placed).
//!
//! STOCK EXCEPTION: the Trust sysroot carries only its host's std, so every dev lane that
//! compiles for another target rides the one stock toolchain the checkout pins ([`stock_pin`]):
//! `verify cells` compiles and lints its cells ([`CELL_TARGETS`]) on it, the web modules'
//! lanes build on it, and `--linux` adds the Linux lane's guest agent's std
//! ([`LINUX_LANE_TARGETS`]). This row installs that one toolchain, its stds and its components
//! ([`PIN_COMPONENTS`]), and beside it the release the checkout's `trust.lock` names for the
//! release chain's own slices ([`release_toolchain`], [`RELEASE_TARGETS`]); no dev lane takes
//! anything from that one, and nothing here installs rustup's floating `stable`. A public
//! snapshot ships no `trust.lock` and cuts no release: its row is the stock pin alone.

use std::path::{Path, PathBuf};

use aterm_uds::host::{Absent, Lookup};

use crate::rustup::CELL_TARGETS;

/// The std targets `--linux` adds to the stock pin: the Linux lane's guest agent
/// (`crates/aterm-vz-agent`) is a static musl binary cross-built for this one
/// (STOCK EXCEPTION: Trust has no Linux std; docs/DESIGN-vz-guest-2026-10-23.md §6).
pub const LINUX_LANE_TARGETS: [&str; 1] = ["aarch64-unknown-linux-musl"];

/// The toolchain the release chain builds its STOCK EXCEPTION slices on: the `[stock] toolchain`
/// of the checkout's `trust.lock` (for a cut, the candidate's). `aterm-release` compiles the
/// universal binary's x86_64 slice, `ship check --compat` (and `verify watch`'s stage that
/// rehearses it) and a Linux worker's static bootstrap verifier with that release by name in
/// `RUSTUP_TOOLCHAIN`, records its `rustc -vV`, and `ship provision`'s `x86 slice` probes it.
/// This row puts it in place, so `ship provision`, which runs this row, readies a release host.
///
/// `Ok(None)`: the checkout carries no `trust.lock`, as a public snapshot does (its manifest ships
/// none), so the row has no release-chain part there ([`lockless_note`]) and the stock pin is
/// the row, as [`stock_pin`] rides [`crate::rustup::STOCK_PIN`] alone without `publish/`. Nothing
/// stands in for the lock: no release toolchain is installed for it, rustup's `stable` least of
/// all. A release host (`release_host`, `verify provision --release`, which `ship provision` and
/// a cut run) builds those slices, so there it is refused.
///
/// # Errors
/// A lock that cannot be read or does not parse (among it one with no `[stock] toolchain`), and,
/// on a release host, a checkout that carries no lock.
pub fn release_toolchain(repo: &Path, release_host: bool) -> Result<Option<String>, String> {
    let path = repo.join(aterm_toolchain::trust_lock::LOCK_FILE);
    if std::fs::symlink_metadata(&path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        if release_host {
            return Err(format!(
                "{} carries no {}, whose `[stock] toolchain` names the release the release \
                 chain's STOCK EXCEPTION slices build with, and a release host builds them; \
                 nothing falls back to rustup's `stable`",
                repo.display(),
                aterm_toolchain::trust_lock::LOCK_FILE
            ));
        }
        return Ok(None);
    }
    aterm_toolchain::trust_lock::read(&path).map(|lock| Some(lock.stock_toolchain))
}

/// What a run says where the checkout at `repo` carries no `trust.lock` ([`release_toolchain`]
/// answered `Ok(None)`).
#[must_use]
pub fn lockless_note(repo: &Path) -> String {
    format!(
        "note: {} carries no {} (a public snapshot ships none), so this row puts no release \
         chain's stock toolchain in place; `verify provision --release` refuses such a checkout",
        repo.display(),
        aterm_toolchain::trust_lock::LOCK_FILE
    )
}

/// The stds the release chain takes from its stock toolchain ([`release_toolchain`]) past its
/// host's: on a Mac, the universal binary's x86_64 slice's. A Linux worker builds only for its
/// own host.
pub const RELEASE_TARGETS: &[&str] = if cfg!(target_os = "macos") {
    &["x86_64-apple-darwin"]
} else {
    &[]
};

pub use crate::rustup::{
    ClauseError, STOCK_PIN, STOCK_PIN_FILE, WasmClause, stock_pin, wasm_clause,
};

/// The components the pin carries past the minimal profile: clippy, since `verify cells` lints
/// each foreign cell with the pin's and a pin without it is a COULD NOT RUN; and off macOS
/// llvm-tools, whose `llvm-nm` the cells' win32 oracles (`aterm-win32-oracle`) read the Windows
/// import libraries with: GNU nm reads none of them, and macOS's own nm is LLVM's.
pub const PIN_COMPONENTS: &[&str] = if cfg!(target_os = "macos") {
    &["clippy"]
} else {
    &["clippy", "llvm-tools"]
};

/// The file rustup keeps its settings in, which a home rustup never ran in lacks.
const SETTINGS: &str = "settings.toml";

/// What every `rustup toolchain install` here carries. The row's copy is upstream rustup-init,
/// built with self-update on, and an install without this flag ends in rustup's self-update
/// against `CARGO_HOME`: where no rustup was self-installed there it fails the step (`rustup
/// is not installed at '~/.cargo'`) after the toolchain landed, and where a person's own
/// rustup is there it writes rustup's proxies into `CARGO_HOME/bin` or replaces that rustup
/// with a newer one. With it, nothing is written to `CARGO_HOME`.
const NO_SELF_UPDATE: &str = "--no-self-update";

/// What rustup holds now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Seen {
    /// rustup's home holds no settings yet: rustup never ran there.
    pub fresh_home: bool,
    /// The host rustup resolves a bare release or channel to
    /// ([`aterm_toolchain::rustup::default_host`]).
    pub host: String,
    /// `rustup toolchain list`'s names.
    pub toolchains: Vec<String>,
    /// The std targets installed on the pin.
    pub pin_targets: Vec<String>,
    /// The components installed on the pin, as rustup names them
    /// (`clippy-aarch64-apple-darwin`).
    pub pin_components: Vec<String>,
    /// The std targets installed on the release chain's toolchain ([`release_toolchain`]).
    pub release_targets: Vec<String>,
}

/// One acquisition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// `rustup set auto-install disable`, first in a home rustup never ran in.
    AutoInstallOff,
    /// `rustup toolchain install <pin> --profile minimal --component <c>… --no-self-update`, each
    /// of [`PIN_COMPONENTS`].
    PinToolchain(String),
    /// `rustup target add --toolchain <pin> <targets>`.
    PinTargets(String, Vec<String>),
    /// `rustup component add --toolchain <pin> <components>`, those of [`PIN_COMPONENTS`] the pin
    /// lacks.
    PinComponents(String, Vec<String>),
    /// `rustup toolchain install <release> --profile minimal [--target <t>]… --no-self-update`,
    /// the release the checkout's `trust.lock` names ([`release_toolchain`]).
    ReleaseToolchain(String, Vec<String>),
    /// `rustup target add --toolchain <release> <targets>`.
    ReleaseTargets(String, Vec<String>),
}

/// Whether `seen` holds the toolchain `name` as rustup resolves it on its host
/// ([`aterm_toolchain::rustup::holds_toolchain`]).
fn holds(seen: &Seen, name: &str) -> bool {
    aterm_toolchain::rustup::holds_toolchain(&seen.toolchains.join("\n"), name, &seen.host)
}

/// Whether `listed` holds the component `name`, which rustup lists with its triple after it
/// (`clippy-aarch64-apple-darwin`).
fn has(listed: &[String], name: &str) -> bool {
    listed
        .iter()
        .any(|t| t == name || t.starts_with(&format!("{name}-")))
}

/// What a machine that holds `seen` lacks, in order: of the stock pin `pin` with `targets`' stds
/// and [`PIN_COMPONENTS`] (nothing of it where the checkout's pin could not be read, `None`), then of
/// the release chain's toolchain `release` with `release_targets`' stds (nothing of it where the
/// checkout has no release-chain part or its lock could not be read, `None`).
pub(crate) fn plan(
    seen: &Seen,
    pin: Option<&str>,
    targets: &[&str],
    release: Option<&str>,
    release_targets: &[&str],
) -> Vec<Step> {
    let mut steps = Vec::new();
    if seen.fresh_home {
        steps.push(Step::AutoInstallOff);
    }
    if let Some(pin) = pin {
        let mut targets = targets.to_vec();
        if release == Some(pin) {
            for target in release_targets {
                if !targets.contains(target) {
                    targets.push(target);
                }
            }
        }
        pin_plan(seen, pin, &targets, &mut steps);
        if release == Some(pin) {
            return steps;
        }
    }
    let Some(release) = release else {
        return steps;
    };
    if holds(seen, release) {
        let missing: Vec<String> = release_targets
            .iter()
            .filter(|t| !seen.release_targets.iter().any(|i| i == *t))
            .map(|t| (*t).to_string())
            .collect();
        if !missing.is_empty() {
            steps.push(Step::ReleaseTargets(release.to_string(), missing));
        }
    } else {
        steps.push(Step::ReleaseToolchain(
            release.to_string(),
            release_targets.iter().map(|t| (*t).to_string()).collect(),
        ));
    }
    steps
}

/// What `seen` lacks of the pin `pin` with `targets`' stds and [`PIN_COMPONENTS`], onto `steps`.
fn pin_plan(seen: &Seen, pin: &str, targets: &[&str], steps: &mut Vec<Step>) {
    let installed: Vec<String>;
    let (held, components): (&[String], &[String]) = if holds(seen, pin) {
        (&seen.pin_targets, &seen.pin_components)
    } else {
        steps.push(Step::PinToolchain(pin.to_string()));
        installed = PIN_COMPONENTS.iter().map(|c| (*c).to_string()).collect();
        (&[], &installed)
    };
    let missing: Vec<String> = targets
        .iter()
        .filter(|t| !held.iter().any(|i| i == *t))
        .map(|t| (*t).to_string())
        .collect();
    if !missing.is_empty() {
        steps.push(Step::PinTargets(pin.to_string(), missing));
    }
    let lacking: Vec<String> = PIN_COMPONENTS
        .iter()
        .filter(|c| !has(components, c))
        .map(|c| (*c).to_string())
        .collect();
    if !lacking.is_empty() {
        steps.push(Step::PinComponents(pin.to_string(), lacking));
    }
}

/// The rustup command line of `step`.
pub(crate) fn argv(step: &Step) -> Vec<String> {
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    match step {
        Step::AutoInstallOff => v(&["rustup", "set", "auto-install", "disable"]),
        Step::PinToolchain(pin) => {
            let mut a = v(&[
                "rustup",
                "toolchain",
                "install",
                pin,
                "--profile",
                "minimal",
            ]);
            for c in PIN_COMPONENTS {
                a.extend(["--component".to_string(), (*c).to_string()]);
            }
            a.push(NO_SELF_UPDATE.to_string());
            a
        }
        Step::PinTargets(pin, ts) => {
            let mut a = v(&["rustup", "target", "add", "--toolchain", pin]);
            a.extend(ts.iter().cloned());
            a
        }
        Step::PinComponents(pin, cs) => {
            let mut a = v(&["rustup", "component", "add", "--toolchain", pin]);
            a.extend(cs.iter().cloned());
            a
        }
        Step::ReleaseToolchain(release, ts) => {
            let mut a = v(&[
                "rustup",
                "toolchain",
                "install",
                release,
                "--profile",
                "minimal",
            ]);
            for t in ts {
                a.extend(["--target".to_string(), t.clone()]);
            }
            a.push(NO_SELF_UPDATE.to_string());
            a
        }
        Step::ReleaseTargets(release, ts) => {
            let mut a = v(&["rustup", "target", "add", "--toolchain", release]);
            a.extend(ts.iter().cloned());
            a
        }
    }
}

/// Where rustup's toolchains live, and how the `rustup` row is resolved, read once:
/// rustup's own rules for its homes (`$RUSTUP_HOME`, else `~/.rustup`; `$CARGO_HOME`, else
/// `~/.cargo`), which every child is handed, and the row's lookup ([`Lookup`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Homes {
    /// `$HOME`, where each rustup child runs.
    pub home: PathBuf,
    /// Where rustup keeps its toolchains.
    pub rustup_home: PathBuf,
    /// The account's `CARGO_HOME`, which no step writes ([`NO_SELF_UPDATE`]): handed to each
    /// child as it would inherit it, so a test's rustup sees the scratch's.
    pub cargo_home: PathBuf,
    /// What the `rustup` row's resolver reads: `$ATERM_TEST_RUSTUP`, the tooling cache and
    /// `$PATH` (a test's, where the homes are a test's).
    pub lookup: Lookup,
}

impl Homes {
    /// This process's, with the row's copy looked for in `tooling`.
    #[must_use]
    pub fn from_env(home: &Path, tooling: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            rustup_home: aterm_toolchain::rustup::rustup_home(
                std::env::var_os("RUSTUP_HOME").as_deref(),
                home,
            ),
            cargo_home: match std::env::var_os("CARGO_HOME") {
                Some(v) if !v.is_empty() => PathBuf::from(v),
                _ => home.join(".cargo"),
            },
            lookup: Lookup {
                tooling: Some(tooling.to_path_buf()),
                ..Lookup::host(aterm_uds::host::tool("rustup"))
            },
        }
    }

    /// The rustup the `rustup` row resolves over these homes.
    ///
    /// # Errors
    /// The row finds none, or `$ATERM_TEST_RUSTUP` names no file.
    pub fn rustup(&self) -> Result<PathBuf, Absent> {
        aterm_uds::host::tool("rustup").locate_in(&self.lookup)
    }
}

/// What runs rustup: this machine ([`System`]), or a test's stateful stand-in.
pub trait Rustup {
    /// `tool args…` with `RUSTUP_TOOLCHAIN` set to `toolchain` (else cleared) and the
    /// homes' `RUSTUP_HOME` and `CARGO_HOME`: its stdout, or why it failed.
    fn run(
        &self,
        homes: &Homes,
        tool: &Path,
        args: &[String],
        toolchain: Option<&str>,
    ) -> Result<String, String>;
}

/// This machine.
pub struct System;

impl Rustup for System {
    fn run(
        &self,
        homes: &Homes,
        tool: &Path,
        args: &[String],
        toolchain: Option<&str>,
    ) -> Result<String, String> {
        let mut cmd = aterm_uds::spawnfd::command(tool);
        cmd.args(args)
            .env_remove("RUSTUP_TOOLCHAIN")
            .env("RUSTUP_HOME", &homes.rustup_home)
            .env("CARGO_HOME", &homes.cargo_home)
            .current_dir(&homes.home);
        if let Some(t) = toolchain {
            cmd.env("RUSTUP_TOOLCHAIN", t);
        }
        match cmd.output() {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => Err(format!("could not run {}: {e}", tool.display())),
        }
    }
}

/// The first word of each line: rustup's `toolchain list` and `target list --installed`
/// (`stable-aarch64-apple-darwin (default)` names `stable-aarch64-apple-darwin`).
fn first_words(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect()
}

/// What rustup `bin` holds on this machine of the stock pin `pin` and the release chain's
/// toolchain `release`; an answer it does not give reads as nothing held.
fn observe(
    homes: &Homes,
    bin: &Path,
    rustup: &dyn Rustup,
    pin: Option<&str>,
    release: Option<&str>,
) -> Seen {
    let lines = |toolchain: Option<&str>, args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        rustup
            .run(homes, bin, &args, toolchain)
            .map(|out| first_words(&out))
            .unwrap_or_default()
    };
    let mut seen = Seen {
        fresh_home: !homes.rustup_home.join(SETTINGS).is_file(),
        host: aterm_toolchain::rustup::default_host(&homes.rustup_home),
        toolchains: lines(None, &["toolchain", "list"]),
        ..Seen::default()
    };
    if let Some(p) = pin.filter(|p| holds(&seen, p)) {
        seen.pin_targets = lines(Some(p), &["target", "list", "--installed"]);
        seen.pin_components = lines(Some(p), &["component", "list", "--installed"]);
    }
    if let Some(r) = release.filter(|r| holds(&seen, r)) {
        seen.release_targets = lines(Some(r), &["target", "list", "--installed"]);
    }
    seen
}

/// What this row asks the pin for: the cells' stds ([`CELL_TARGETS`]), and with `linux`
/// (`--linux`) the Linux lane's [`LINUX_LANE_TARGETS`] last.
fn pin_targets(linux: bool) -> Vec<&'static str> {
    let mut targets: Vec<&str> = CELL_TARGETS.to_vec();
    if linux {
        targets.extend(LINUX_LANE_TARGETS);
    }
    targets
}

/// What rustup's home lacks of the row, read only through the rustup the row resolves:
/// empty when it is in place. `pin` is the checkout's stock pin and `release` the release its
/// `trust.lock` names for the release chain ([`release_toolchain`]), each `None` where the
/// checkout has none to ask for (a pin that could not be read, a checkout with no lock), which
/// asks nothing of it; `linux` (`--linux`) holds the pin to the Linux lane's targets too.
///
/// # Errors
/// The row resolves no rustup to read with.
pub fn missing(
    homes: &Homes,
    rustup: &dyn Rustup,
    pin: Option<&str>,
    release: Option<&str>,
    linux: bool,
) -> Result<Vec<String>, Absent> {
    let bin = homes.rustup()?;
    let seen = observe(homes, &bin, rustup, pin, release);
    Ok(
        plan(&seen, pin, &pin_targets(linux), release, RELEASE_TARGETS)
            .iter()
            .map(|step| argv(step).join(" "))
            .collect(),
    )
}

/// Acquire what rustup's home lacks with the rustup the row resolves; each step is said,
/// and a step that fails is said with why. `linux` (`--linux`) adds the Linux lane's
/// targets to the pin; `release_host` (`--release`) refuses a checkout with no `trust.lock`,
/// which elsewhere is said ([`lockless_note`]) and asks nothing of the release chain's part.
///
/// # Errors
/// Returns an error naming every step that failed, the unreadable pin, the checkout's lock that
/// names no release chain's toolchain (on a release host, its absence too), or the rustup the
/// row could not resolve, or requirements still absent after successful commands.
pub fn run(
    repo: &Path,
    homes: &Homes,
    rustup: &dyn Rustup,
    linux: bool,
    release_host: bool,
    report: &mut dyn FnMut(&str),
) -> Result<Vec<String>, String> {
    let mut failed = Vec::new();
    let pin = match stock_pin(repo) {
        Ok(pin) => Some(pin),
        Err(refused) => {
            failed.push(format!("the stock pin is not acquired — {refused}"));
            None
        }
    };
    let release = match release_toolchain(repo, release_host) {
        Ok(Some(release)) => Some(release),
        Ok(None) => {
            report(&lockless_note(repo));
            None
        }
        Err(refused) => {
            failed.push(format!(
                "the release chain's stock toolchain is not acquired — {refused}"
            ));
            None
        }
    };
    let bin = homes.rustup().map_err(|absent| {
        failed.push(format!("no rustup to drive — {absent}"));
        failed.join("; ")
    })?;
    let seen = observe(homes, &bin, rustup, pin.as_deref(), release.as_deref());
    let mut acquired = Vec::new();
    for step in plan(
        &seen,
        pin.as_deref(),
        &pin_targets(linux),
        release.as_deref(),
        RELEASE_TARGETS,
    ) {
        let args = &argv(&step)[1..];
        match rustup.run(homes, &bin, args, None) {
            Ok(_) => {
                let line = format!("rustup {}", args.join(" "));
                report(&format!("completed command: {line}"));
                acquired.push(line);
            }
            Err(why) => failed.push(format!(
                "`{} {}` failed: {why}",
                bin.display(),
                args.join(" ")
            )),
        }
    }
    let after = observe(homes, &bin, rustup, pin.as_deref(), release.as_deref());
    for step in plan(
        &after,
        pin.as_deref(),
        &pin_targets(linux),
        release.as_deref(),
        RELEASE_TARGETS,
    ) {
        failed.push(format!(
            "still missing after provisioning: {}",
            argv(&step).join(" ")
        ));
    }
    if failed.is_empty() {
        Ok(acquired)
    } else {
        Err(failed.join("; "))
    }
}

#[cfg(test)]
#[path = "stock_tests.rs"]
mod tests;

/// The proxies rustup 1.29.2's self-update lays in `CARGO_HOME/bin` beside a rustup it finds
/// there, as the pinned copy laid them in a scratch `CARGO_HOME` (13 links to `rustup`).
#[cfg(test)]
const SELF_UPDATE_PROXIES: [&str; 13] = [
    "cargo",
    "cargo-clippy",
    "cargo-fmt",
    "cargo-miri",
    "clippy-driver",
    "rls",
    "rust-analyzer",
    "rust-gdb",
    "rust-gdbgui",
    "rust-lldb",
    "rustc",
    "rustdoc",
    "rustfmt",
];

/// What the row's copy (rustup-init 1.29.2, self-update built in) does to `cargo_home` at the
/// end of `rustup <args>`, as measured with it in scratch homes: a `toolchain install`
/// without [`NO_SELF_UPDATE`] self-updates, which fails (`rustup is not installed at …`)
/// where `cargo_home` holds no `bin/rustup` and writes [`SELF_UPDATE_PROXIES`] beside one
/// that is there; every other call, and an install with the flag, leaves `cargo_home` alone.
///
/// # Errors
/// rustup's own refusal, where the self-update finds no rustup to update.
#[cfg(test)]
pub(super) fn self_update(cargo_home: &Path, args: &[String]) -> Result<(), String> {
    let install = args.len() >= 2 && args[0] == "toolchain" && args[1] == "install";
    if !install || args.iter().any(|a| a == NO_SELF_UPDATE) {
        return Ok(());
    }
    let bin = cargo_home.join("bin");
    if !bin.join("rustup").is_file() {
        return Err(format!(
            "error: rustup is not installed at '{}'",
            cargo_home.display()
        ));
    }
    for proxy in SELF_UPDATE_PROXIES {
        let at = bin.join(proxy);
        if std::fs::symlink_metadata(&at).is_err() {
            std::os::unix::fs::symlink("rustup", &at).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Tests for `run` and `observe` over a stateful rustup.
#[cfg(test)]
#[path = "stock_stateful_tests.rs"]
mod stateful;
