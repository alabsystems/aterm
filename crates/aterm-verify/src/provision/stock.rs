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
//! ([`LINUX_LANE_TARGETS`]). This row installs that one toolchain, its stds and its clippy, and
//! beside it the toolchain the release chain still builds its own slices on
//! ([`RELEASE_TOOLCHAIN`], [`RELEASE_TARGETS`]); no dev lane takes anything from that one.

use std::path::{Path, PathBuf};

use aterm_uds::host::{Absent, Lookup};

/// The std targets the `cells` check (`targo --unverified verify cells`, by hand)
/// compiles its cells for, on the stock pin: every forge cell's triple (this machine's
/// own among them, which a minimal install already carries). wasm32's is also the one the web
/// modules' lanes build for, and the Macs' and Linux gnu ones the libc oracle's cross cells.
pub const CELL_TARGETS: [&str; 7] = [
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "wasm32-unknown-unknown",
];

/// The std targets `--linux` adds to the stock pin: the Linux lane's guest agent
/// (`crates/aterm-vz-agent`) is a static musl binary cross-built for this one
/// (STOCK EXCEPTION: Trust has no Linux std; docs/DESIGN-vz-guest-2026-10-23.md §6).
pub const LINUX_LANE_TARGETS: [&str; 1] = ["aarch64-unknown-linux-musl"];

/// The toolchain the release chain builds its STOCK EXCEPTION slices on: `aterm-release`
/// compiles the universal binary's x86_64 slice, `ship check --compat` (and `verify watch`'s
/// stage that rehearses it) and a Linux worker's static bootstrap verifier with
/// `RUSTUP_TOOLCHAIN=stable`, and `ship provision`'s `x86 slice` probes it. This row puts it in
/// place, so `ship provision`, which runs this row, readies a release host, until the release
/// chain builds on a pinned release as the dev lanes do.
pub const RELEASE_TOOLCHAIN: &str = "stable";

/// The stds the release chain takes from [`RELEASE_TOOLCHAIN`] past its host's: on a Mac, the
/// universal binary's x86_64 slice's. A Linux worker builds only for its own host.
pub const RELEASE_TARGETS: &[&str] = if cfg!(target_os = "macos") {
    &["x86_64-apple-darwin"]
} else {
    &[]
};

pub use crate::rustup::{
    ClauseError, STOCK_PIN, STOCK_PIN_FILE, WasmClause, stock_pin, wasm_clause,
};

/// The component the pin carries past the minimal profile: `verify cells` lints each foreign
/// cell with the pin's clippy, and a pin without it is a COULD NOT RUN.
const CLIPPY: &str = "clippy";

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
    /// The std targets installed on [`RELEASE_TOOLCHAIN`].
    pub release_targets: Vec<String>,
}

/// One acquisition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// `rustup set auto-install disable`, first in a home rustup never ran in.
    AutoInstallOff,
    /// `rustup toolchain install <pin> --profile minimal --component clippy --no-self-update`.
    PinToolchain(String),
    /// `rustup target add --toolchain <pin> <targets>`.
    PinTargets(String, Vec<String>),
    /// `rustup component add --toolchain <pin> clippy`.
    PinClippy(String),
    /// `rustup toolchain install stable --profile minimal [--target <t>]… --no-self-update`.
    ReleaseToolchain(Vec<String>),
    /// `rustup target add --toolchain stable <targets>`.
    ReleaseTargets(Vec<String>),
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
/// and clippy (nothing of it where the checkout's pin could not be read, `None`), then of
/// [`RELEASE_TOOLCHAIN`] with `release`'s stds.
pub(crate) fn plan(
    seen: &Seen,
    pin: Option<&str>,
    targets: &[&str],
    release: &[&str],
) -> Vec<Step> {
    let mut steps = Vec::new();
    if seen.fresh_home {
        steps.push(Step::AutoInstallOff);
    }
    if let Some(pin) = pin {
        pin_plan(seen, pin, targets, &mut steps);
    }
    if holds(seen, RELEASE_TOOLCHAIN) {
        let missing: Vec<String> = release
            .iter()
            .filter(|t| !seen.release_targets.iter().any(|i| i == *t))
            .map(|t| (*t).to_string())
            .collect();
        if !missing.is_empty() {
            steps.push(Step::ReleaseTargets(missing));
        }
    } else {
        steps.push(Step::ReleaseToolchain(
            release.iter().map(|t| (*t).to_string()).collect(),
        ));
    }
    steps
}

/// What `seen` lacks of the pin `pin` with `targets`' stds and clippy, onto `steps`.
fn pin_plan(seen: &Seen, pin: &str, targets: &[&str], steps: &mut Vec<Step>) {
    let (held, components): (&[String], &[String]) = if holds(seen, pin) {
        (&seen.pin_targets, &seen.pin_components)
    } else {
        steps.push(Step::PinToolchain(pin.to_string()));
        (&[], &[CLIPPY.to_string()])
    };
    let missing: Vec<String> = targets
        .iter()
        .filter(|t| !held.iter().any(|i| i == *t))
        .map(|t| (*t).to_string())
        .collect();
    if !missing.is_empty() {
        steps.push(Step::PinTargets(pin.to_string(), missing));
    }
    if !has(components, CLIPPY) {
        steps.push(Step::PinClippy(pin.to_string()));
    }
}

/// The rustup command line of `step`.
pub(crate) fn argv(step: &Step) -> Vec<String> {
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    match step {
        Step::AutoInstallOff => v(&["rustup", "set", "auto-install", "disable"]),
        Step::PinToolchain(pin) => v(&[
            "rustup",
            "toolchain",
            "install",
            pin,
            "--profile",
            "minimal",
            "--component",
            CLIPPY,
            NO_SELF_UPDATE,
        ]),
        Step::PinTargets(pin, ts) => {
            let mut a = v(&["rustup", "target", "add", "--toolchain", pin]);
            a.extend(ts.iter().cloned());
            a
        }
        Step::PinClippy(pin) => v(&["rustup", "component", "add", "--toolchain", pin, CLIPPY]),
        Step::ReleaseToolchain(ts) => {
            let mut a = v(&[
                "rustup",
                "toolchain",
                "install",
                RELEASE_TOOLCHAIN,
                "--profile",
                "minimal",
            ]);
            for t in ts {
                a.extend(["--target".to_string(), t.clone()]);
            }
            a.push(NO_SELF_UPDATE.to_string());
            a
        }
        Step::ReleaseTargets(ts) => {
            let mut a = v(&["rustup", "target", "add", "--toolchain", RELEASE_TOOLCHAIN]);
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

/// What rustup `bin` holds on this machine; an answer it does not give reads as nothing
/// held.
fn observe(homes: &Homes, bin: &Path, rustup: &dyn Rustup, pin: Option<&str>) -> Seen {
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
    if holds(&seen, RELEASE_TOOLCHAIN) {
        seen.release_targets = lines(Some(RELEASE_TOOLCHAIN), &["target", "list", "--installed"]);
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
/// empty when it is in place. `pin` is the checkout's stock pin (`None` where it could not be
/// read, which asks nothing of it); `linux` (`--linux`) holds the pin to the Linux lane's
/// targets too.
///
/// # Errors
/// The row resolves no rustup to read with.
pub fn missing(
    homes: &Homes,
    rustup: &dyn Rustup,
    pin: Option<&str>,
    linux: bool,
) -> Result<Vec<String>, Absent> {
    let bin = homes.rustup()?;
    let seen = observe(homes, &bin, rustup, pin);
    Ok(plan(&seen, pin, &pin_targets(linux), RELEASE_TARGETS)
        .iter()
        .map(|step| argv(step).join(" "))
        .collect())
}

/// Acquire what rustup's home lacks with the rustup the row resolves; each step is said,
/// and a step that fails is said with why. `linux` (`--linux`) adds the Linux lane's
/// targets to the pin.
///
/// # Errors
/// Returns an error naming every step that failed, the unreadable pin, or the rustup the
/// row could not resolve.
pub fn run(
    repo: &Path,
    homes: &Homes,
    rustup: &dyn Rustup,
    linux: bool,
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
    let bin = homes.rustup().map_err(|absent| {
        failed.push(format!("no rustup to drive — {absent}"));
        failed.join("; ")
    })?;
    let seen = observe(homes, &bin, rustup, pin.as_deref());
    let mut acquired = Vec::new();
    for step in plan(&seen, pin.as_deref(), &pin_targets(linux), RELEASE_TARGETS) {
        let args = &argv(&step)[1..];
        match rustup.run(homes, &bin, args, None) {
            Ok(_) => {
                let line = format!("rustup {}", args.join(" "));
                report(&format!("acquired: {line}"));
                acquired.push(line);
            }
            Err(why) => failed.push(format!(
                "`{} {}` failed: {why}",
                bin.display(),
                args.join(" ")
            )),
        }
    }
    if failed.is_empty() {
        Ok(acquired)
    } else {
        Err(failed.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGETS: &[&str] = &["x86_64-apple-darwin", "wasm32-unknown-unknown"];
    const RELEASE: &[&str] = &["x86_64-apple-darwin"];
    const PIN: &str = "1.97.1";
    const HOST: &str = "aarch64-apple-darwin";

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    fn provisioned() -> Seen {
        Seen {
            fresh_home: false,
            host: HOST.into(),
            toolchains: names(&["stable-aarch64-apple-darwin", "1.97.1-aarch64-apple-darwin"]),
            pin_targets: names(TARGETS),
            pin_components: names(&["cargo-aarch64-apple-darwin", "clippy-aarch64-apple-darwin"]),
            release_targets: names(&[HOST, "x86_64-apple-darwin"]),
        }
    }

    #[test]
    fn a_missing_target_or_clippy_is_added_to_the_pin() {
        let mut s = provisioned();
        s.pin_targets = names(&["wasm32-unknown-unknown"]);
        s.pin_components = names(&["cargo-aarch64-apple-darwin"]);
        assert_eq!(
            plan(&s, Some(PIN), TARGETS, RELEASE),
            [
                Step::PinTargets(PIN.into(), names(&["x86_64-apple-darwin"])),
                Step::PinClippy(PIN.into()),
            ]
        );
        assert_eq!(
            argv(&Step::PinClippy(PIN.into())).join(" "),
            "rustup component add --toolchain 1.97.1 clippy"
        );
        assert_eq!(plan(&provisioned(), Some(PIN), TARGETS, RELEASE), []);
    }

    /// THE PIN IS PROVISIONED WHATEVER STABLE HOLDS: a machine whose floating `stable` carries
    /// every cell's std and clippy, and which lacks the pin, gets the pin installed with clippy
    /// and every target; nothing of the pin is asked of `stable`. NEGATIVE CONTROL: a plan that
    /// counts `stable`'s stds and clippy as the pin's leaves this machine as it is, and the first
    /// assertion fails.
    #[test]
    fn a_missing_pin_is_installed_with_clippy_and_every_target_whatever_stable_holds() {
        let mut s = provisioned();
        s.toolchains = names(&[
            "stable-aarch64-apple-darwin",
            "nightly-aarch64-apple-darwin",
        ]);
        s.pin_targets = Vec::new();
        s.pin_components = Vec::new();
        let steps = plan(&s, Some(PIN), TARGETS, RELEASE);
        assert_eq!(
            steps,
            [
                Step::PinToolchain(PIN.into()),
                Step::PinTargets(PIN.into(), names(TARGETS)),
            ]
        );
        assert_eq!(
            argv(&steps[0]).join(" "),
            "rustup toolchain install 1.97.1 --profile minimal --component clippy --no-self-update"
        );
        assert!(
            steps
                .iter()
                .flat_map(argv)
                .all(|word| !word.starts_with("stable")),
            "{steps:?}"
        );
        let fresh = Seen {
            fresh_home: true,
            host: HOST.into(),
            ..Seen::default()
        };
        assert_eq!(
            plan(&fresh, Some(PIN), TARGETS, RELEASE),
            [
                Step::AutoInstallOff,
                Step::PinToolchain(PIN.into()),
                Step::PinTargets(PIN.into(), names(TARGETS)),
                Step::ReleaseToolchain(names(RELEASE)),
            ],
            "a home rustup never ran in turns auto-install off first"
        );
        assert_eq!(
            plan(&fresh, None, TARGETS, RELEASE),
            [Step::AutoInstallOff, Step::ReleaseToolchain(names(RELEASE))],
            "a pin that could not be read is asked for nothing, the release chain's stable still"
        );
    }

    /// THE RELEASE CHAIN'S `stable` IS IN PLACE WITH ITS SLICE'S STD, AND NOTHING MORE: absent, it
    /// is installed with the x86_64 slice's std and no clippy; held without that std, the std is
    /// added; held with it, nothing. NEGATIVE CONTROL: a plan without the release part leaves a
    /// bare machine without `stable`, and `ship provision`'s `x86 slice` stays red.
    #[test]
    fn the_release_chains_stable_is_put_in_place_with_its_slice_std() {
        let mut s = provisioned();
        s.toolchains = names(&["1.97.1-aarch64-apple-darwin"]);
        s.release_targets = Vec::new();
        let steps = plan(&s, Some(PIN), TARGETS, RELEASE);
        assert_eq!(steps, [Step::ReleaseToolchain(names(RELEASE))]);
        assert_eq!(
            argv(&steps[0]).join(" "),
            "rustup toolchain install stable --profile minimal --target x86_64-apple-darwin \
             --no-self-update"
        );
        let mut s = provisioned();
        s.release_targets = names(&[HOST]);
        let steps = plan(&s, Some(PIN), TARGETS, RELEASE);
        assert_eq!(steps, [Step::ReleaseTargets(names(RELEASE))]);
        assert_eq!(
            argv(&steps[0]).join(" "),
            "rustup target add --toolchain stable x86_64-apple-darwin"
        );
        assert_eq!(plan(&provisioned(), Some(PIN), TARGETS, &[]), []);
        assert_eq!(
            argv(&Step::ReleaseToolchain(Vec::new())).join(" "),
            "rustup toolchain install stable --profile minimal --no-self-update",
            "a Linux worker's stable carries its host's std alone"
        );
    }

    /// ANOTHER HOST'S INSTALL IS NOT THE HOST'S: a machine whose rustup lists the pin and
    /// `stable` only for x86_64 on an arm64 host gets both installed for its host, since rustup
    /// resolves `1.97.1` and `stable` to `-aarch64-apple-darwin` and finds neither. NEGATIVE
    /// CONTROL: a plan that reads `1.97.1-x86_64-apple-darwin` as the pin adds stds to a
    /// toolchain rustup cannot resolve, and the run never converges.
    #[test]
    fn another_hosts_install_of_the_pin_or_stable_is_not_held() {
        let s = Seen {
            host: HOST.into(),
            toolchains: names(&[
                "1.97.1-x86_64-apple-darwin",
                "stable-x86_64-apple-darwin",
                "1.97.1-mine",
            ]),
            ..Seen::default()
        };
        assert_eq!(
            plan(&s, Some(PIN), TARGETS, RELEASE),
            [
                Step::PinToolchain(PIN.into()),
                Step::PinTargets(PIN.into(), names(TARGETS)),
                Step::ReleaseToolchain(names(RELEASE)),
            ]
        );
    }

    #[test]
    fn only_linux_asks_the_pin_for_the_guest_agents_musl_std() {
        assert!(
            !pin_targets(false).contains(&"aarch64-unknown-linux-musl"),
            "{:?}",
            pin_targets(false)
        );
        let linux = pin_targets(true);
        assert_eq!(linux.last(), Some(&"aarch64-unknown-linux-musl"));
        let mut s = provisioned();
        s.pin_targets = pin_targets(false)
            .iter()
            .map(|t| (*t).to_string())
            .collect();
        assert_eq!(plan(&s, Some(PIN), &pin_targets(false), RELEASE), []);
        assert_eq!(
            plan(&s, Some(PIN), &linux, RELEASE),
            [Step::PinTargets(
                PIN.into(),
                names(&["aarch64-unknown-linux-musl"])
            )]
        );
    }
}

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
mod stateful {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::*;

    /// The host this rustup installs for: the one `observe` reads off a scratch home whose
    /// settings name none.
    const HOST: &str = aterm_toolchain::rustup::NATIVE_TRIPLE;

    #[derive(Default)]
    struct State {
        auto_install_off: bool,
        /// Installed toolchain (rustup's full name) -> its std targets.
        toolchains: BTreeMap<String, Vec<String>>,
        /// Installed toolchain -> its components past the minimal profile's.
        components: BTreeMap<String, Vec<String>>,
        calls: Vec<String>,
        /// The binary each call ran.
        ran: Vec<PathBuf>,
        auto_installed: Vec<String>,
    }

    #[derive(Default)]
    pub(super) struct FakeRustup(RefCell<State>);

    impl FakeRustup {
        fn calls(&self) -> Vec<String> {
            self.0.borrow().calls.clone()
        }

        fn find(s: &State, name: &str) -> Option<String> {
            s.toolchains
                .keys()
                .find(|t| *t == name || t.starts_with(&format!("{name}-")))
                .cloned()
        }

        /// `name`, installed, auto-installed, or refused.
        fn need(s: &mut State, name: &str) -> Result<String, String> {
            if let Some(t) = Self::find(s, name) {
                return Ok(t);
            }
            if s.auto_install_off {
                return Err(format!("error: toolchain '{name}-{HOST}' is not installed"));
            }
            let full = format!("{name}-{HOST}");
            s.toolchains.insert(full.clone(), vec![HOST.into()]);
            s.auto_installed.push(name.to_string());
            Ok(full)
        }
    }

    impl Rustup for FakeRustup {
        fn run(
            &self,
            homes: &Homes,
            tool: &Path,
            args: &[String],
            toolchain: Option<&str>,
        ) -> Result<String, String> {
            let mut s = self.0.borrow_mut();
            let name = tool.file_name().map(|n| n.to_string_lossy().into_owned());
            assert_eq!(
                name.as_deref(),
                Some("rustup"),
                "only rustup runs, never an installer: {}",
                tool.display()
            );
            s.calls.push(format!(
                "{}rustup {}",
                toolchain.map(|t| format!("+{t} ")).unwrap_or_default(),
                args.join(" ")
            ));
            s.ran.push(tool.to_path_buf());
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            match a.as_slice() {
                ["toolchain", "list"] => Ok(s
                    .toolchains
                    .keys()
                    .map(|t| {
                        let d = if t.starts_with("stable") {
                            " (default)"
                        } else {
                            ""
                        };
                        format!("{t}{d}\n")
                    })
                    .collect()),
                ["target", "list", "--installed"] => {
                    let t = Self::need(&mut s, toolchain.ok_or("no toolchain named")?)?;
                    Ok(s.toolchains[&t].iter().map(|x| format!("{x}\n")).collect())
                }
                ["component", "list", "--installed"] => {
                    let t = Self::need(&mut s, toolchain.ok_or("no toolchain named")?)?;
                    let extra = s.components.get(&t).into_iter().flatten();
                    let host = ["cargo", "rustc"]
                        .into_iter()
                        .chain(extra.map(String::as_str));
                    Ok(host
                        .map(|c| format!("{c}-{HOST}\n"))
                        .chain(s.toolchains[&t].iter().map(|x| format!("rust-std-{x}\n")))
                        .collect())
                }
                ["set", "auto-install", "disable"] => {
                    std::fs::create_dir_all(&homes.rustup_home).unwrap();
                    std::fs::write(homes.rustup_home.join(SETTINGS), "auto_install = false\n")
                        .unwrap();
                    s.auto_install_off = true;
                    Ok(String::new())
                }
                ["toolchain", "install", name, rest @ ..] => {
                    let (mut targets, mut components) = (vec![HOST.to_string()], Vec::new());
                    let mut it = rest.iter();
                    while let Some(x) = it.next() {
                        match *x {
                            "--target" => targets.extend(it.next().map(|t| (*t).to_string())),
                            "--component" => {
                                components.extend(it.next().map(|c| (*c).to_string()));
                            }
                            _ => {}
                        }
                    }
                    s.toolchains.insert(format!("{name}-{HOST}"), targets);
                    s.components.insert(format!("{name}-{HOST}"), components);
                    self_update(&homes.cargo_home, args)?;
                    Ok(String::new())
                }
                ["component", "add", "--toolchain", name, components @ ..] => {
                    let t = Self::need(&mut s, name)?;
                    let have = s.components.entry(t).or_default();
                    have.extend(components.iter().map(|x| (*x).to_string()));
                    Ok(String::new())
                }
                ["target", "add", "--toolchain", name, targets @ ..] => {
                    let t = Self::need(&mut s, name)?;
                    let have = s.toolchains.get_mut(&t).unwrap();
                    have.extend(targets.iter().map(|x| (*x).to_string()));
                    Ok(String::new())
                }
                other => Err(format!("unexpected rustup call {other:?}")),
            }
        }
    }

    fn repo() -> PathBuf {
        aterm_tempfile::manifest_dir().join("../..")
    }

    fn pin() -> String {
        stock_pin(&repo()).unwrap()
    }

    /// A scratch account's homes: nothing on `$PATH`, no `$ATERM_TEST_RUSTUP`, and the row's
    /// copy looked for in the scratch's own tooling cache.
    pub(super) fn homes(dir: &Path) -> Homes {
        Homes {
            home: dir.to_path_buf(),
            rustup_home: dir.join(".rustup"),
            cargo_home: dir.join(".cargo"),
            lookup: Lookup {
                named: None,
                tooling: Some(dir.join("tooling.noindex")),
                path: None,
            },
        }
    }

    /// The row's copy, as `verify provision` places it: `bin/rustup` beside its stamp.
    fn placed(homes: &Homes) -> PathBuf {
        let row = aterm_uds::host::tool("rustup");
        let tooling = homes.lookup.tooling.as_deref().unwrap();
        let exe = row.pinned_in(tooling).unwrap();
        stub(&exe);
        std::fs::write(
            row.pinned_root(tooling)
                .unwrap()
                .join(aterm_uds::host::PIN_STAMP),
            "{}",
        )
        .unwrap();
        exe
    }

    /// The release chain's install line on this host.
    fn release_install() -> String {
        let targets = RELEASE_TARGETS.iter().map(|t| (*t).to_string()).collect();
        argv(&Step::ReleaseToolchain(targets)).join(" ")
    }

    fn position(calls: &[String], prefix: &str) -> usize {
        calls
            .iter()
            .position(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("no `{prefix}` in {calls:#?}"))
    }

    #[test]
    fn a_bare_home_gets_auto_install_off_then_the_pin_by_name() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let homes = homes(tmp.path());
        let rustup = FakeRustup::default();
        let refused = run(&repo(), &homes, &rustup, false, &mut |_| {}).unwrap_err();
        assert!(
            refused.contains("no rustup to drive") && refused.contains("verify provision"),
            "{refused}"
        );
        assert!(rustup.calls().is_empty(), "{:#?}", rustup.calls());
        let copy = placed(&homes);
        let acquired = run(&repo(), &homes, &rustup, false, &mut |_| {}).unwrap();
        assert!(!acquired.is_empty());
        let calls = rustup.calls();
        assert!(
            position(&calls, "rustup set auto-install disable")
                < position(&calls, "rustup toolchain install"),
            "auto-install is off before anything is installed: {calls:#?}"
        );
        let pin = pin();
        position(
            &calls,
            &format!("rustup toolchain install {pin} --profile minimal --component clippy"),
        );
        position(
            &calls,
            &format!(
                "rustup target add --toolchain {pin} {}",
                CELL_TARGETS.join(" ")
            ),
        );
        let release = release_install();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.contains("stable"))
                .collect::<Vec<_>>(),
            [&release],
            "rustup's floating stable gets the release chain's toolchain alone: {calls:#?}"
        );
        assert!(
            rustup.0.borrow().auto_installed.is_empty(),
            "every toolchain is installed by name: {:?}",
            rustup.0.borrow().auto_installed
        );
        assert!(
            rustup.0.borrow().ran.iter().all(|r| *r == copy),
            "every call ran the row's copy: {:?}",
            rustup.0.borrow().ran
        );
        let add = ["component", "add", "--toolchain", &pin, CLIPPY].map(String::from);
        rustup
            .run(&homes, &copy, &add, None)
            .expect("the pin was installed by name");
        assert_eq!(
            missing(&homes, &rustup, Some(&pin), false),
            Ok(Vec::<String>::new())
        );
        assert_eq!(
            run(&repo(), &homes, &rustup, false, &mut |_| {}),
            Ok(Vec::new())
        );
    }

    /// The release chain builds its x86_64 compat slice on rustup's `stable`: a bare home gets
    /// it by name, with that slice's std on a Mac, and a second run finds it in place.
    #[test]
    fn a_bare_home_gets_the_release_chains_stable_with_its_slice_std() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let homes = homes(tmp.path());
        let rustup = FakeRustup::default();
        placed(&homes);
        run(&repo(), &homes, &rustup, false, &mut |_| {}).unwrap();
        let calls = rustup.calls();
        let release = release_install();
        position(&calls, &release);
        if cfg!(target_os = "macos") {
            assert!(
                release.contains(" --target x86_64-apple-darwin "),
                "{release}"
            );
        }
        {
            let state = rustup.0.borrow();
            let stable = &state.toolchains[&format!("stable-{HOST}")];
            for t in RELEASE_TARGETS {
                assert!(stable.iter().any(|s| s == t), "{stable:?}");
            }
            assert!(state.auto_installed.is_empty());
        }
        assert_eq!(
            run(&repo(), &homes, &rustup, false, &mut |_| {}),
            Ok(Vec::new())
        );
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|d| {
                d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// A TOOLCHAIN INSTALL WRITES NOTHING TO `CARGO_HOME`: on a bare account (no `CARGO_HOME`
    /// at all) every step succeeds and none is made, and where a person's own rustup sits in
    /// `CARGO_HOME/bin` it stays alone there, because the install carries
    /// `--no-self-update`. NEGATIVE CONTROL: the same install line without the flag, run
    /// through the fake that models the pinned copy's self-update, fail the bare account's
    /// step (`rustup is not installed at …`) and write 13 proxies beside the person's rustup.
    #[test]
    fn a_toolchain_install_never_self_updates_into_cargo_home() {
        let pin = pin();
        let bare = aterm_tempfile::tempdir().unwrap();
        let bare = homes(bare.path());
        placed(&bare);
        let rustup = FakeRustup::default();
        let acquired = run(&repo(), &bare, &rustup, false, &mut |_| {}).unwrap();
        assert!(
            acquired
                .iter()
                .any(|a| a.contains(&format!("toolchain install {pin}"))),
            "{acquired:#?}"
        );
        assert!(!bare.cargo_home.exists(), "no CARGO_HOME was made");

        let own = aterm_tempfile::tempdir().unwrap();
        let own = homes(own.path());
        let copy = placed(&own);
        stub(&own.cargo_home.join("bin/rustup"));
        run(&repo(), &own, &FakeRustup::default(), false, &mut |_| {}).unwrap();
        assert_eq!(entries(&own.cargo_home.join("bin")), ["rustup"]);

        {
            let with: Vec<String> = argv(&Step::PinToolchain(pin.clone()))[1..].to_vec();
            assert_eq!(with.last().map(String::as_str), Some(NO_SELF_UPDATE));
            let without: Vec<String> = with
                .iter()
                .filter(|a| *a != NO_SELF_UPDATE)
                .cloned()
                .collect();
            let refused = FakeRustup::default()
                .run(&bare, &copy, &without, None)
                .unwrap_err();
            assert!(refused.contains("rustup is not installed at"), "{refused}");
            FakeRustup::default()
                .run(&own, &copy, &without, None)
                .unwrap();
            assert_eq!(
                entries(&own.cargo_home.join("bin")).len(),
                1 + SELF_UPDATE_PROXIES.len(),
                "control: without the flag the install writes CARGO_HOME"
            );
            std::fs::remove_dir_all(own.cargo_home.join("bin")).unwrap();
            stub(&own.cargo_home.join("bin/rustup"));
        }
    }

    /// An executable that is not run: the fake answers for it.
    fn stub(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/false\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A PIN MISSING A PIECE GETS IT ON THE PIN ALONE, WHATEVER STABLE HOLDS: where `stable`
    /// carries every cell's std and clippy and the pin only its host's std, the probe and the
    /// run name the pin's missing stds and its clippy, and nothing of `stable`.
    #[test]
    fn a_pin_missing_a_piece_gets_it_on_the_pin_alone() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let homes = homes(tmp.path());
        let rustup = FakeRustup::default();
        let pin = pin();
        {
            let targets: Vec<String> = CELL_TARGETS.iter().map(|t| (*t).to_string()).collect();
            let mut s = rustup.0.borrow_mut();
            s.auto_install_off = true;
            s.toolchains.insert(format!("stable-{HOST}"), targets);
            s.components
                .insert(format!("stable-{HOST}"), vec![CLIPPY.into()]);
            s.toolchains
                .insert(format!("{pin}-{HOST}"), vec![HOST.into()]);
        }
        std::fs::create_dir_all(&homes.rustup_home).unwrap();
        std::fs::write(homes.rustup_home.join(SETTINGS), "").unwrap();
        placed(&homes);
        let others: Vec<&str> = CELL_TARGETS
            .iter()
            .copied()
            .filter(|t| *t != HOST)
            .collect();
        let add = format!("rustup target add --toolchain {pin} {}", others.join(" "));
        let clippy = format!("rustup component add --toolchain {pin} clippy");
        assert_eq!(
            missing(&homes, &rustup, Some(&pin), false),
            Ok(vec![add.clone(), clippy.clone()]),
            "the read-only probe names what a run adds"
        );
        run(&repo(), &homes, &rustup, false, &mut |_| {}).unwrap();
        let calls = rustup.calls();
        position(&calls, &add);
        position(&calls, &clippy);
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("stable") && !c.contains(" list")),
            "nothing of the pin is added to stable: {calls:#?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("toolchain install") || c.contains(" set ")),
            "{calls:#?}"
        );
        assert!(rustup.0.borrow().auto_installed.is_empty());
        assert_eq!(
            missing(&homes, &rustup, Some(&pin), false),
            Ok(Vec::<String>::new())
        );

        // `--linux` holds the same machine to the guest agent's musl std as well, and
        // adds it to the pin alone.
        let musl = format!("rustup target add --toolchain {pin} aarch64-unknown-linux-musl");
        assert_eq!(
            missing(&homes, &rustup, Some(&pin), true),
            Ok(vec![musl.clone()])
        );
        let acquired = run(&repo(), &homes, &rustup, true, &mut |_| {}).unwrap();
        assert_eq!(acquired, [musl]);
        assert_eq!(
            missing(&homes, &rustup, Some(&pin), true),
            Ok(Vec::<String>::new())
        );
    }

    /// THE ROW DRIVES ITS STAMPED COPY, BEFORE ANY RUSTUP ON `$PATH`, AND NEVER HOMEBREW'S
    /// KEG: the homes' rustup is the `rustup` row's answer over their lookup. NEGATIVE
    /// CONTROLS: with no copy, a rustup on `$PATH` (a person's own) answers, and the keg
    /// planted beside it answers only when its own directory is put on `$PATH`.
    #[test]
    fn the_row_drives_its_stamped_copy_before_path_and_never_the_keg() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let keg = tmp.path().join("opt/homebrew/opt/rustup/bin/rustup");
        stub(&keg);
        let on_path = tmp.path().join("bin");
        stub(&on_path.join("rustup"));
        let mut h = homes(tmp.path());
        assert!(h.rustup().is_err(), "the keg never answers");
        h.lookup.path = Some(on_path.clone().into_os_string());
        assert_eq!(h.rustup(), Ok(on_path.join("rustup")));
        let copy = placed(&h);
        assert_eq!(h.rustup(), Ok(copy), "the stamped copy before $PATH");
        let kegged = Homes {
            lookup: Lookup {
                tooling: None,
                path: Some(keg.parent().unwrap().as_os_str().to_owned()),
                named: None,
            },
            ..h
        };
        assert_eq!(kegged.rustup(), Ok(keg));
    }
}
