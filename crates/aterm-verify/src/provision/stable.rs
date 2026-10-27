// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify provision`'s rustup row for stock stable and the wasm32 pin, driven by the rustup
//! the `rustup` row of [`aterm_uds::host::TOOLS`] resolves (the row's stamped copy, once
//! placed).

use std::path::{Path, PathBuf};

use aterm_uds::host::{Absent, Lookup};

/// The std targets the `cells` check (`targo --unverified verify cells`, by hand)
/// compiles its cells for, on upstream stable: every forge cell's triple (this
/// machine's own is stable's host std, there already). The x86_64 Mac's is also the
/// shipped binary's x86_64 slice.
pub const CELL_TARGETS: [&str; 7] = [
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "wasm32-unknown-unknown",
];

/// The std targets `--linux` adds to stable: the Linux lane's guest agent
/// (`crates/aterm-vz-agent`) is a static musl binary cross-built for this one
/// (STOCK EXCEPTION: Trust has no Linux std; docs/DESIGN-vz-guest-2026-10-23.md §6).
pub const LINUX_LANE_TARGETS: [&str; 1] = ["aarch64-unknown-linux-musl"];

pub use crate::rustup::{ClauseError, WasmClause, wasm_clause, wasm_pin, wasm_pin_wanted};

/// The wasm32 target the pin carries.
const WASM32: &str = "wasm32-unknown-unknown";

/// The component stable carries past the minimal profile: `verify cells` lints each
/// foreign cell with its toolchain's clippy, and skips a toolchain that has the std
/// but no clippy.
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
    /// `rustup toolchain list`'s names.
    pub toolchains: Vec<String>,
    /// The std targets installed on stable.
    pub stable_targets: Vec<String>,
    /// The components installed on stable, as rustup names them
    /// (`clippy-aarch64-apple-darwin`).
    pub stable_components: Vec<String>,
    /// The std targets installed on the wasm32 pin.
    pub pin_targets: Vec<String>,
}

/// One acquisition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// `rustup set auto-install disable`, first in a home rustup never ran in.
    AutoInstallOff,
    /// `rustup toolchain install stable --profile minimal --component clippy --no-self-update`.
    StableToolchain,
    /// `rustup target add --toolchain stable <targets>`.
    StableTargets(Vec<String>),
    /// `rustup component add --toolchain stable clippy`.
    StableClippy,
    /// `rustup toolchain install <pin> --profile minimal --target wasm32-unknown-unknown
    /// --no-self-update`.
    PinToolchain(String),
    /// `rustup target add --toolchain <pin> wasm32-unknown-unknown`.
    PinTarget(String),
}

/// Whether `listed` holds `name`, which rustup lists with a triple after it: a toolchain
/// (`stable-aarch64-apple-darwin`) or a component (`clippy-aarch64-apple-darwin`).
fn has(listed: &[String], name: &str) -> bool {
    listed
        .iter()
        .any(|t| t == name || t.starts_with(&format!("{name}-")))
}

/// What a machine that holds `seen` lacks, in order.
pub(crate) fn plan(seen: &Seen, pin: Option<&str>, stable_targets: &[&str]) -> Vec<Step> {
    let mut steps = Vec::new();
    if seen.fresh_home {
        steps.push(Step::AutoInstallOff);
    }
    if has(&seen.toolchains, "stable") {
        let missing: Vec<String> = stable_targets
            .iter()
            .filter(|t| !seen.stable_targets.iter().any(|i| i == *t))
            .map(|t| (*t).to_string())
            .collect();
        if !missing.is_empty() {
            steps.push(Step::StableTargets(missing));
        }
        if !has(&seen.stable_components, CLIPPY) {
            steps.push(Step::StableClippy);
        }
    } else {
        steps.push(Step::StableToolchain);
        steps.push(Step::StableTargets(
            stable_targets.iter().map(|t| (*t).to_string()).collect(),
        ));
    }
    if let Some(pin) = pin {
        if !has(&seen.toolchains, pin) {
            steps.push(Step::PinToolchain(pin.to_string()));
        } else if !seen.pin_targets.iter().any(|t| t == WASM32) {
            steps.push(Step::PinTarget(pin.to_string()));
        }
    }
    steps
}

/// The rustup command line of `step`.
pub(crate) fn argv(step: &Step) -> Vec<String> {
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    match step {
        Step::AutoInstallOff => v(&["rustup", "set", "auto-install", "disable"]),
        Step::StableToolchain => v(&[
            "rustup",
            "toolchain",
            "install",
            "stable",
            "--profile",
            "minimal",
            "--component",
            CLIPPY,
            NO_SELF_UPDATE,
        ]),
        Step::StableTargets(ts) => {
            let mut a = v(&["rustup", "target", "add", "--toolchain", "stable"]);
            a.extend(ts.iter().cloned());
            a
        }
        Step::StableClippy => v(&[
            "rustup",
            "component",
            "add",
            "--toolchain",
            "stable",
            CLIPPY,
        ]),
        Step::PinToolchain(pin) => v(&[
            "rustup",
            "toolchain",
            "install",
            pin,
            "--profile",
            "minimal",
            "--target",
            WASM32,
            NO_SELF_UPDATE,
        ]),
        Step::PinTarget(pin) => v(&["rustup", "target", "add", "--toolchain", pin, WASM32]),
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
    let toolchains = lines(None, &["toolchain", "list"]);
    let installed = |t: &str, what: &str| {
        if has(&toolchains, t) {
            lines(Some(t), &[what, "list", "--installed"])
        } else {
            Vec::new()
        }
    };
    let stable_targets = installed("stable", "target");
    let stable_components = installed("stable", "component");
    let pin_targets = pin.map(|p| installed(p, "target")).unwrap_or_default();
    Seen {
        fresh_home: !homes.rustup_home.join(SETTINGS).is_file(),
        toolchains,
        stable_targets,
        stable_components,
        pin_targets,
    }
}

/// What this row asks stable for: the cells' stds ([`CELL_TARGETS`]), and with `linux`
/// (`--linux`) the Linux lane's [`LINUX_LANE_TARGETS`] last.
fn stable_targets(linux: bool) -> Vec<&'static str> {
    let mut targets: Vec<&str> = CELL_TARGETS.to_vec();
    if linux {
        targets.extend(LINUX_LANE_TARGETS);
    }
    targets
}

/// What rustup's home lacks of the row, read only through the rustup the row resolves:
/// empty when it is in place. `linux` (`--linux`) holds stable to the Linux lane's targets
/// too.
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
    Ok(plan(
        &observe(homes, &bin, rustup, pin),
        pin,
        &stable_targets(linux),
    )
    .iter()
    .map(|step| argv(step).join(" "))
    .collect())
}

/// Acquire what rustup's home lacks with the rustup the row resolves; each step is said,
/// and a step that fails is said with why. `linux` (`--linux`) adds the Linux lane's stable
/// targets.
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
    let pin = match wasm_pin_wanted(repo) {
        Ok(pin) => pin,
        Err(why) => {
            failed.push(format!("the wasm32 pin is not acquired — {why}"));
            None
        }
    };
    let bin = homes.rustup().map_err(|absent| {
        failed.push(format!("no rustup to drive — {absent}"));
        failed.join("; ")
    })?;
    let seen = observe(homes, &bin, rustup, pin.as_deref());
    let mut acquired = Vec::new();
    for step in plan(&seen, pin.as_deref(), &stable_targets(linux)) {
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

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    fn provisioned() -> Seen {
        Seen {
            fresh_home: false,
            toolchains: names(&["stable-aarch64-apple-darwin", "1.97.1-aarch64-apple-darwin"]),
            stable_targets: names(TARGETS),
            stable_components: names(&[
                "cargo-aarch64-apple-darwin",
                "clippy-aarch64-apple-darwin",
            ]),
            pin_targets: names(&["aarch64-apple-darwin", WASM32]),
        }
    }

    #[test]
    fn a_missing_target_or_clippy_is_added_to_the_toolchain_that_lacks_it() {
        let mut s = provisioned();
        s.stable_targets = names(&["wasm32-unknown-unknown"]);
        s.stable_components = names(&["cargo-aarch64-apple-darwin"]);
        s.pin_targets = names(&["aarch64-apple-darwin"]);
        assert_eq!(
            plan(&s, Some("1.97.1"), TARGETS),
            [
                Step::StableTargets(names(&["x86_64-apple-darwin"])),
                Step::StableClippy,
                Step::PinTarget("1.97.1".into()),
            ]
        );
        assert_eq!(
            argv(&Step::StableClippy).join(" "),
            "rustup component add --toolchain stable clippy"
        );
    }

    #[test]
    fn a_missing_pin_is_installed_with_its_wasm32_target_and_stable_alone_when_absent() {
        let mut s = provisioned();
        s.toolchains = names(&["nightly-aarch64-apple-darwin"]);
        assert_eq!(
            plan(&s, Some("1.97.1"), TARGETS),
            [
                Step::StableToolchain,
                Step::StableTargets(names(TARGETS)),
                Step::PinToolchain("1.97.1".into()),
            ]
        );
        assert_eq!(plan(&provisioned(), None, TARGETS), []);
        let fresh = Seen {
            fresh_home: true,
            ..Seen::default()
        };
        assert_eq!(
            plan(&fresh, Some("1.97.1"), TARGETS),
            [
                Step::AutoInstallOff,
                Step::StableToolchain,
                Step::StableTargets(names(TARGETS)),
                Step::PinToolchain("1.97.1".into()),
            ],
            "a home rustup never ran in turns auto-install off first"
        );
    }

    #[test]
    fn only_linux_asks_stable_for_the_guest_agents_musl_std() {
        assert!(
            !stable_targets(false).contains(&"aarch64-unknown-linux-musl"),
            "{:?}",
            stable_targets(false)
        );
        let linux = stable_targets(true);
        assert_eq!(linux.last(), Some(&"aarch64-unknown-linux-musl"));
        let mut s = provisioned();
        s.stable_targets = stable_targets(false)
            .iter()
            .map(|t| (*t).to_string())
            .collect();
        assert_eq!(plan(&s, None, &stable_targets(false)), []);
        assert_eq!(
            plan(&s, None, &linux),
            [Step::StableTargets(names(&["aarch64-unknown-linux-musl"]))]
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

    const HOST: &str = "aarch64-apple-darwin";

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
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn pin() -> String {
        wasm_pin(&repo()).unwrap()
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

    fn position(calls: &[String], prefix: &str) -> usize {
        calls
            .iter()
            .position(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("no `{prefix}` in {calls:#?}"))
    }

    #[test]
    fn a_bare_home_gets_auto_install_off_then_every_toolchain_by_name() {
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
            "rustup toolchain install stable --profile minimal --component clippy",
        );
        position(
            &calls,
            &format!(
                "rustup toolchain install {pin} --profile minimal --target wasm32-unknown-unknown"
            ),
        );
        position(&calls, "rustup target add --toolchain stable");
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
        let add = ["target", "add", "--toolchain", &pin, WASM32].map(String::from);
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
    /// `CARGO_HOME/bin` it stays alone there, because each install carries
    /// `--no-self-update`. NEGATIVE CONTROL: the same two install lines without the flag, run
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
                .any(|a| a.contains("toolchain install stable"))
                && acquired
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

        for step in [Step::StableToolchain, Step::PinToolchain(pin.clone())] {
            let with: Vec<String> = argv(&step)[1..].to_vec();
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

    #[test]
    fn a_toolchain_missing_a_piece_gets_it_on_that_channel_alone() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let homes = homes(tmp.path());
        let rustup = FakeRustup::default();
        let pin = pin();
        {
            let targets: Vec<String> = CELL_TARGETS.iter().map(|t| (*t).to_string()).collect();
            let mut s = rustup.0.borrow_mut();
            s.auto_install_off = true;
            s.toolchains.insert(format!("stable-{HOST}"), targets);
            s.toolchains
                .insert(format!("{pin}-{HOST}"), vec![HOST.into()]);
        }
        std::fs::create_dir_all(&homes.rustup_home).unwrap();
        std::fs::write(homes.rustup_home.join(SETTINGS), "").unwrap();
        placed(&homes);
        assert_eq!(
            missing(&homes, &rustup, Some(&pin), false),
            Ok(vec![
                "rustup component add --toolchain stable clippy".to_string(),
                format!("rustup target add --toolchain {pin} wasm32-unknown-unknown"),
            ]),
            "the read-only probe names what a run adds"
        );
        run(&repo(), &homes, &rustup, false, &mut |_| {}).unwrap();
        let calls = rustup.calls();
        position(
            &calls,
            &format!("rustup target add --toolchain {pin} wasm32-unknown-unknown"),
        );
        position(&calls, "rustup component add --toolchain stable clippy");
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
        // adds it to stable alone.
        let musl = "rustup target add --toolchain stable aarch64-unknown-linux-musl";
        assert_eq!(
            missing(&homes, &rustup, Some(&pin), true),
            Ok(vec![musl.to_string()])
        );
        let acquired = run(&repo(), &homes, &rustup, true, &mut |_| {}).unwrap();
        assert_eq!(acquired, [musl.to_string()]);
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
