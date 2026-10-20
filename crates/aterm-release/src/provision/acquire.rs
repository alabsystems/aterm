// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stock exceptions a full `ship provision` acquires before it audits them (`--check`
//! acquires nothing): rustup itself (no default toolchain, auto-install off), upstream
//! stable with the `x86_64-apple-darwin` slice, the foreign cells' std and the clippy
//! that lints them (for `verify cells`, run by hand; the audit does not ask for them),
//! the wasm32 pin `publish/config.sh`'s wasm32 clause names, with its wasm32 target, and
//! rustup's `trust` link (atpkg's seam, the pass `aterm pkg repair` runs).
//!
//! Stock stable builds only the x86_64 slice and lints the foreign cells, and the pin
//! only type-checks the wasm32 clause, so each is installed by name with what its job
//! needs past the minimal profile; a rustup installed here gets no default toolchain,
//! so a bare `cargo` in an unpinned directory fails loudly instead of running stock
//! stable.

use std::path::{Path, PathBuf};

/// The wasm32 target the pin carries.
const WASM32: &str = "wasm32-unknown-unknown";

/// The component stable carries past the minimal profile: `verify cells` lints each
/// foreign cell with its toolchain's clippy, and skips a toolchain that has the std
/// but no clippy.
const CLIPPY: &str = "clippy";

/// What rustup holds now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Seen {
    /// The rustup binary, when there is one.
    pub rustup: Option<PathBuf>,
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
    /// rustup-init with no default toolchain and no PATH edits; auto-install off.
    InstallRustup,
    /// `rustup toolchain install stable --profile minimal --component clippy`.
    StableToolchain,
    /// `rustup target add --toolchain stable <targets>`.
    StableTargets(Vec<String>),
    /// `rustup component add --toolchain stable clippy`.
    StableClippy,
    /// `rustup toolchain install <pin> --profile minimal --target wasm32-unknown-unknown`.
    PinToolchain(String),
    /// `rustup target add --toolchain <pin> wasm32-unknown-unknown`.
    PinTarget(String),
    /// rustup's `trust` entry, attached (or a stale one re-pointed) by atpkg's seam.
    TrustSeam,
}

/// Whether `listed` holds `name`, which rustup lists with a triple after it: a toolchain
/// (`stable-aarch64-apple-darwin`) or a component (`clippy-aarch64-apple-darwin`).
fn has(listed: &[String], name: &str) -> bool {
    listed
        .iter()
        .any(|t| t == name || t.starts_with(&format!("{name}-")))
}

/// What a machine that holds `seen` lacks, in order — pure.
pub(crate) fn plan(seen: &Seen, pin: Option<&str>, stable_targets: &[&str]) -> Vec<Step> {
    let mut steps = Vec::new();
    let fresh = seen.rustup.is_none();
    if fresh {
        steps.push(Step::InstallRustup);
    }
    if fresh || !has(&seen.toolchains, "stable") {
        steps.push(Step::StableToolchain);
        steps.push(Step::StableTargets(
            stable_targets.iter().map(|t| (*t).to_string()).collect(),
        ));
    } else {
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
    }
    if let Some(pin) = pin {
        if fresh || !has(&seen.toolchains, pin) {
            steps.push(Step::PinToolchain(pin.to_string()));
        } else if !seen.pin_targets.iter().any(|t| t == WASM32) {
            steps.push(Step::PinTarget(pin.to_string()));
        }
    }
    steps.push(Step::TrustSeam);
    steps
}

/// The rustup command lines of `step` (none for the seam, which runs in-process).
pub(crate) fn argv(step: &Step) -> Vec<Vec<String>> {
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    match step {
        Step::InstallRustup => vec![
            v(&[
                "rustup-init",
                "-y",
                "--no-modify-path",
                "--profile",
                "minimal",
                "--default-toolchain",
                "none",
            ]),
            v(&["rustup", "set", "auto-install", "disable"]),
        ],
        Step::StableToolchain => vec![v(&[
            "rustup",
            "toolchain",
            "install",
            "stable",
            "--profile",
            "minimal",
            "--component",
            CLIPPY,
        ])],
        Step::StableTargets(ts) => {
            let mut a = v(&["rustup", "target", "add", "--toolchain", "stable"]);
            a.extend(ts.iter().cloned());
            vec![a]
        }
        Step::StableClippy => vec![v(&[
            "rustup",
            "component",
            "add",
            "--toolchain",
            "stable",
            CLIPPY,
        ])],
        Step::PinToolchain(pin) => vec![v(&[
            "rustup",
            "toolchain",
            "install",
            pin,
            "--profile",
            "minimal",
            "--target",
            WASM32,
        ])],
        Step::PinTarget(pin) => vec![v(&["rustup", "target", "add", "--toolchain", pin, WASM32])],
        Step::TrustSeam => Vec::new(),
    }
}

/// The host rustup-init is downloaded for.
fn host_triple() -> Option<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Some("aarch64-apple-darwin"),
        ("x86_64", "macos") => Some("x86_64-apple-darwin"),
        ("x86_64", "linux") => Some("x86_64-unknown-linux-gnu"),
        ("aarch64", "linux") => Some("aarch64-unknown-linux-gnu"),
        _ => None,
    }
}

/// The directories rustup and atpkg resolve, read from the environment once: rustup's
/// own rules (`$CARGO_HOME`, else `~/.cargo`; `$RUSTUP_HOME`, else `~/.rustup`), which
/// every child is handed explicitly so rustup-init installs where the next step looks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Homes {
    /// `$HOME`.
    pub home: PathBuf,
    /// Where rustup-init puts `bin/rustup`.
    pub cargo_home: PathBuf,
    /// Where rustup keeps its toolchains.
    pub rustup_home: PathBuf,
    /// `$XDG_CONFIG_HOME`, for the atpkg prefix the seam resolves.
    pub xdg_config_home: Option<PathBuf>,
    /// `$PATH`, searched for an installed rustup before `<cargo_home>/bin`.
    pub path: Option<std::ffi::OsString>,
    /// Homebrew's rustup keg, looked at last: this machine's
    /// ([`atpkg::toolchain::rustup::HOMEBREW_RUSTUP`]), or `None` where the homes are a
    /// test's and nothing of this machine's may answer.
    pub homebrew_keg: Option<PathBuf>,
}

impl Homes {
    /// This process's.
    pub(crate) fn from_env(home: &Path) -> Self {
        let set = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Self {
            home: home.to_path_buf(),
            cargo_home: set("CARGO_HOME").unwrap_or_else(|| home.join(".cargo")),
            rustup_home: atpkg::toolchain::rustup_home(
                std::env::var_os("RUSTUP_HOME").as_deref(),
                home,
            ),
            xdg_config_home: set("XDG_CONFIG_HOME"),
            path: std::env::var_os("PATH"),
            homebrew_keg: Some(PathBuf::from(atpkg::toolchain::rustup::HOMEBREW_RUSTUP)),
        }
    }

    fn rustup_init(&self) -> PathBuf {
        self.cargo_home.join("rustup-init")
    }

    /// The one rustup finder (`aterm_toolchain::rustup::find_rustup_among`) over these
    /// homes: PATH, then `<cargo_home>/bin`, `~/.cargo/bin` and the keg they name.
    fn find_rustup(&self) -> Option<PathBuf> {
        atpkg::toolchain::rustup::find_rustup_among(
            self.path.as_deref(),
            Some(self.cargo_home.as_os_str()),
            Some(self.home.as_os_str()),
            self.homebrew_keg.as_deref(),
        )
    }
}

/// What runs rustup: this machine ([`System`]), or a test's stateful stand-in.
pub(crate) trait Rustup {
    /// `tool args…` with `RUSTUP_TOOLCHAIN` set to `toolchain` (else cleared) and the
    /// homes' `CARGO_HOME`/`RUSTUP_HOME`: its stdout, or why it failed.
    fn run(
        &self,
        homes: &Homes,
        tool: &Path,
        args: &[String],
        toolchain: Option<&str>,
    ) -> Result<String, String>;

    /// This host's rustup-init, from rustup's own dist server, at `dest`.
    fn fetch_init(&self, dest: &Path) -> Result<(), String>;
}

/// This machine.
pub(crate) struct System;

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
            .env("CARGO_HOME", &homes.cargo_home)
            .env("RUSTUP_HOME", &homes.rustup_home)
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

    fn fetch_init(&self, dest: &Path) -> Result<(), String> {
        let triple = host_triple().ok_or("no rustup-init for this host")?;
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        }
        let url = format!("https://static.rust-lang.org/rustup/dist/{triple}/rustup-init");
        let out = aterm_uds::spawnfd::command("curl")
            .args(["--proto", "=https", "--tlsv1.2", "-sSf", "-o"])
            .arg(dest)
            .arg(&url)
            .output()
            .map_err(|e| format!("curl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "{url}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", dest.display()))
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

/// What rustup holds on this machine; an answer it does not give reads as nothing held.
fn observe(homes: &Homes, rustup: &dyn Rustup, pin: Option<&str>) -> Seen {
    let Some(bin) = homes.find_rustup() else {
        return Seen::default();
    };
    let lines = |toolchain: Option<&str>, args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        rustup
            .run(homes, &bin, &args, toolchain)
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
        rustup: Some(bin),
        toolchains,
        stable_targets,
        stable_components,
        pin_targets,
    }
}

/// Acquire what this machine lacks; each step is said, and a step that fails is said with
/// why — the audit that follows names its remedy.
pub(crate) fn run(repo: &Path, homes: &Homes, rustup: &dyn Rustup, report: &mut dyn FnMut(&str)) {
    let pin = match std::fs::read_to_string(repo.join("publish/config.sh"))
        .map_err(|e| e.to_string())
        .and_then(|c| {
            crate::release_steps::wasm_clause(&c).map_err(|r| format!("{} — {}", r.why, r.remedy))
        }) {
        Ok(clause) => Some(clause.pin),
        Err(why) => {
            report(&format!("the wasm32 pin is not acquired — {why}"));
            None
        }
    };
    let mut targets: Vec<&str> = super::FOREIGN_CELL_TARGETS.to_vec();
    if cfg!(target_os = "macos") {
        targets.insert(0, "x86_64-apple-darwin");
    }
    let seen = observe(homes, rustup, pin.as_deref());
    for step in plan(&seen, pin.as_deref(), &targets) {
        if step == Step::TrustSeam {
            seam(homes, report);
            continue;
        }
        if step == Step::InstallRustup
            && let Err(why) = rustup.fetch_init(&homes.rustup_init())
        {
            report(&format!("rustup was not installed — {why}"));
            return;
        }
        for mut a in argv(&step) {
            let tool = match a.remove(0).as_str() {
                "rustup-init" => homes.rustup_init(),
                // Found again per command: rustup-init has only just laid it.
                _ => homes
                    .find_rustup()
                    .unwrap_or_else(|| homes.cargo_home.join("bin/rustup")),
            };
            let name = tool
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into());
            match rustup.run(homes, &tool, &a, None) {
                Ok(_) => report(&format!("acquired: {name} {}", a.join(" "))),
                Err(why) => report(&format!(
                    "`{} {}` failed: {why}",
                    tool.display(),
                    a.join(" ")
                )),
            }
        }
    }
}

/// rustup's `trust` entry through atpkg's own seam pass (`aterm pkg repair`): a first
/// attach where it is absent and trust is installed, a stale or dangling link re-pointed
/// at atpkg's view, and anything it did not lay left alone with the one fix.
fn seam(homes: &Homes, report: &mut dyn FnMut(&str)) {
    let layout = atpkg::Layout {
        prefix: atpkg::toolchain::atpkg_prefix(&homes.home, homes.xdg_config_home.as_deref()),
    };
    for event in atpkg::seam::repair(&layout, &homes.rustup_home) {
        report(&format!("rustup trust: {event}"));
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
            rustup: Some(PathBuf::from("/r/rustup")),
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
                Step::TrustSeam,
            ]
        );
        assert_eq!(
            argv(&Step::StableClippy).concat().join(" "),
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
                Step::TrustSeam,
            ]
        );
        // No readable pin: nothing is guessed.
        assert_eq!(plan(&provisioned(), None, TARGETS), [Step::TrustSeam]);
    }
}

/// `run` and `observe` over a stateful rustup: what it holds changes as it is asked, and a
/// call naming a toolchain it lacks auto-installs it until `set auto-install disable`, then
/// fails as rustup 1.29 does, so "every toolchain by name" and "auto-install off before
/// any install" are observed, not assumed.
#[cfg(test)]
mod stateful {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::*;

    const HOST: &str = "aarch64-apple-darwin";

    #[derive(Default)]
    struct State {
        rustup: bool,
        auto_install_off: bool,
        /// Installed toolchain (rustup's full name) -> its std targets.
        toolchains: BTreeMap<String, Vec<String>>,
        /// Installed toolchain -> its components past the minimal profile's.
        components: BTreeMap<String, Vec<String>>,
        calls: Vec<String>,
        auto_installed: Vec<String>,
    }

    #[derive(Default)]
    struct FakeRustup(RefCell<State>);

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
            s.calls.push(format!(
                "{}{} {}",
                toolchain.map(|t| format!("+{t} ")).unwrap_or_default(),
                name.as_deref().unwrap_or(""),
                args.join(" ")
            ));
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            if name.as_deref() == Some("rustup-init") {
                assert!(
                    a.ends_with(&["--default-toolchain", "none"]),
                    "rustup is installed with no default toolchain: {a:?}"
                );
                let bin = homes.cargo_home.join("bin/rustup");
                std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
                std::fs::write(&bin, "#!/bin/false\n").unwrap();
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
                s.rustup = true;
                return Ok(String::new());
            }
            assert!(s.rustup, "rustup ran before it was installed: {a:?}");
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
                // As rustup lists them: each with the triple it is for.
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

        fn fetch_init(&self, dest: &Path) -> Result<(), String> {
            self.0.borrow_mut().calls.push("fetch rustup-init".into());
            assert!(dest.ends_with(".cargo/rustup-init"), "{}", dest.display());
            Ok(())
        }
    }

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn pin() -> String {
        let config = std::fs::read_to_string(repo().join("publish/config.sh")).unwrap();
        crate::release_steps::wasm_clause(&config).unwrap().pin
    }

    fn homes(dir: &Path) -> Homes {
        Homes {
            home: dir.to_path_buf(),
            cargo_home: dir.join(".cargo"),
            rustup_home: dir.join(".rustup"),
            xdg_config_home: None,
            path: None,
            homebrew_keg: None,
        }
    }

    fn position(calls: &[String], prefix: &str) -> usize {
        calls
            .iter()
            .position(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("no `{prefix}` in {calls:#?}"))
    }

    #[test]
    fn a_bare_machine_gets_rustup_with_auto_install_off_then_every_toolchain_by_name() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let homes = homes(tmp.path());
        let rustup = FakeRustup::default();
        run(&repo(), &homes, &rustup, &mut |_| {});
        let calls = rustup.calls();
        assert_eq!(calls[0], "fetch rustup-init");
        assert_eq!(
            calls[1],
            "rustup-init -y --no-modify-path --profile minimal --default-toolchain none"
        );
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
        // `pub stage`'s wasm32 clause asks for the pin's target with auto-install off: it answers.
        let bin = homes
            .find_rustup()
            .expect("rustup-init laid rustup where it is looked for");
        let add = ["target", "add", "--toolchain", &pin, WASM32].map(String::from);
        rustup
            .run(&homes, &bin, &add, None)
            .expect("the pin was installed by name");
        // A second pass acquires nothing: stable has the clippy `verify cells` lints with.
        assert_eq!(
            plan(
                &observe(&homes, &rustup, Some(&pin)),
                Some(&pin),
                &stable_targets()
            ),
            [Step::TrustSeam]
        );
    }

    /// What [`run`] asks stable for on this host.
    fn stable_targets() -> Vec<&'static str> {
        let mut targets: Vec<&str> = super::super::FOREIGN_CELL_TARGETS.to_vec();
        if cfg!(target_os = "macos") {
            targets.insert(0, "x86_64-apple-darwin");
        }
        targets
    }

    #[test]
    fn a_toolchain_missing_a_piece_gets_it_on_that_channel_alone() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let homes = homes(tmp.path());
        let rustup = FakeRustup::default();
        let pin = pin();
        {
            let mut targets: Vec<String> = super::super::FOREIGN_CELL_TARGETS
                .iter()
                .map(|t| (*t).to_string())
                .collect();
            targets.push("x86_64-apple-darwin".into());
            let mut s = rustup.0.borrow_mut();
            s.rustup = true;
            s.auto_install_off = true;
            s.toolchains.insert(format!("stable-{HOST}"), targets);
            s.toolchains
                .insert(format!("{pin}-{HOST}"), vec![HOST.into()]);
        }
        let bin = homes.cargo_home.join("bin/rustup");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/bin/false\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        run(&repo(), &homes, &rustup, &mut |_| {});
        let calls = rustup.calls();
        position(
            &calls,
            &format!("rustup target add --toolchain {pin} wasm32-unknown-unknown"),
        );
        // Stable had every std and no clippy: `verify cells` skipped each cell on it.
        position(&calls, "rustup component add --toolchain stable clippy");
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("toolchain install") || c.contains("rustup-init")),
            "{calls:#?}"
        );
        assert!(rustup.0.borrow().auto_installed.is_empty());
        assert_eq!(
            plan(
                &observe(&homes, &rustup, Some(&pin)),
                Some(&pin),
                &stable_targets()
            ),
            [Step::TrustSeam]
        );
    }

    #[test]
    fn an_installed_rustup_on_path_is_found_before_cargo_homes() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let on_path = tmp.path().join("bin");
        std::fs::create_dir_all(&on_path).unwrap();
        for bin in [on_path.join("rustup"), tmp.path().join(".cargo/bin/rustup")] {
            std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
            std::fs::write(&bin, "#!/bin/false\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut h = homes(tmp.path());
        assert_eq!(h.find_rustup(), Some(tmp.path().join(".cargo/bin/rustup")));
        h.path = Some(on_path.clone().into_os_string());
        assert_eq!(h.find_rustup(), Some(on_path.join("rustup")));
        // The keg is looked at only where the homes name one, and last.
        let keg = tmp.path().join("keg/bin/rustup");
        std::fs::create_dir_all(keg.parent().unwrap()).unwrap();
        std::fs::copy(on_path.join("rustup"), &keg).unwrap();
        let bare = homes(&tmp.path().join("bare"));
        assert_eq!(bare.find_rustup(), None);
        let kegged = Homes {
            homebrew_keg: Some(keg.clone()),
            ..bare
        };
        assert_eq!(kegged.find_rustup(), Some(keg));
    }
}
