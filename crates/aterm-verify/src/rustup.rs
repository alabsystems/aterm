// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! rustup's one stock toolchain for the STOCK EXCEPTION lanes (`verify cells`, the web modules'
//! lanes and wasm perf): the stock pin the checkout names ([`stock_pin`], [`STOCK_PIN`]), held to
//! what each lane needs of it ([`Stock::serving`]). The rustup they drive is the `rustup` row of
//! [`aterm_uds::host::TOOLS`], found by that row's resolver alone
//! (`aterm_uds::host::tool("rustup").locate()`).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use aterm_uds::output::CommandOutput as _;
use aterm_uds::spawnfd::{Command, command};

pub use aterm_toolchain::rustup::{
    ClauseError, STOCK_PIN, STOCK_PIN_FILE, WasmClause, default_host, holds_toolchain,
    lists_target, stock_pin, toolchain_names, wasm_clause,
};

/// Where the stock pin, and each std and component a lane takes from it, come from.
pub const PROVISION: &str = "targo --unverified verify provision rustup";

/// What answers a question put to rustup: a rustup at its path, or a test's stand-in.
pub trait Ask {
    /// `rustup args…`: its stdout.
    ///
    /// # Errors
    /// rustup could not be run, or it failed: what it said.
    fn ask(&self, args: &[&str]) -> Result<String, String>;
}

impl Ask for Path {
    fn ask(&self, args: &[&str]) -> Result<String, String> {
        let out = command(self)
            .args(args)
            .output_owned()
            .map_err(|e| format!("could not run {} ({e})", self.display()))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!(
                "`rustup {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

/// A rustup toolchain resolved to its own drivers by path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    pub name: String,
    pub cargo: PathBuf,
    pub rustc: PathBuf,
    /// Whether it has the clippy component (a foreign cell is linted).
    pub clippy: bool,
}

impl Toolchain {
    /// `name`'s drivers, as `rustup which --toolchain` answers them.
    /// # Errors
    /// rustup could not be asked, or names no `cargo`/`rustc` for `name`.
    pub fn resolve<A: Ask + ?Sized>(rustup: &A, name: &str) -> Result<Self, String> {
        let which = |tool: &str| -> Result<PathBuf, String> {
            let answer = rustup.ask(&["which", "--toolchain", name, tool])?;
            let path = PathBuf::from(answer.trim());
            if path.is_file() {
                Ok(path)
            } else {
                Err(format!(
                    "`rustup which --toolchain {name} {tool}` names no file: {}",
                    path.display()
                ))
            }
        };
        Ok(Self {
            name: name.to_string(),
            cargo: which("cargo")?,
            rustc: which("rustc")?,
            clippy: which("cargo-clippy").is_ok(),
        })
    }

    /// This toolchain's `cargo`, driving this toolchain's `rustc`.
    #[must_use]
    pub fn cargo(&self) -> Command {
        let mut cmd = command(&self.cargo);
        cmd.env("RUSTC", &self.rustc)
            .env("RUSTUP_TOOLCHAIN", &self.name);
        if let Some((var, value)) = self.loader_env() {
            cmd.env(var, value);
        }
        cmd
    }

    /// The `--remap-path-prefix` that names this toolchain's own library sources (rustup's
    /// `rust-src` component, where it is installed) by the path its standard library was built
    /// at, `/rustc/<commit>`: with `rust-src` present, rustc otherwise writes those sources' host
    /// path into what it builds from them.
    /// # Errors
    /// This toolchain's `rustc` could not say its sysroot or its commit.
    pub fn std_source_remap(&self) -> Result<String, String> {
        let ask = |args: &[&str]| -> Result<String, String> {
            let mut cmd = command(&self.rustc);
            if let Some((var, value)) = self.loader_env() {
                cmd.env(var, value);
            }
            let out = cmd
                .args(args)
                .output_owned()
                .map_err(|e| format!("could not run {} ({e})", self.rustc.display()))?;
            if !out.status.success() {
                return Err(format!(
                    "`{} {}` exited {:?}",
                    self.rustc.display(),
                    args.join(" "),
                    out.status.code()
                ));
            }
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let sysroot = ask(&["--print", "sysroot"])?;
        let version = ask(&["-vV"])?;
        let commit = version
            .lines()
            .find_map(|line| line.strip_prefix("commit-hash: "))
            .ok_or_else(|| format!("`{} -vV` names no commit-hash", self.rustc.display()))?;
        Ok(format!(
            "--remap-path-prefix={}/lib/rustlib/src/rust=/rustc/{}",
            sysroot.trim(),
            commit.trim()
        ))
    }

    /// The library search variable a rustup proxy hands the drivers it runs.
    #[must_use]
    pub fn loader_env(&self) -> Option<(&'static str, OsString)> {
        let lib = self.rustc.parent()?.parent()?.join("lib");
        let var = LOADER_VAR?;
        Some((
            var,
            loader_path(
                var,
                &lib,
                std::env::var_os(var).as_deref(),
                std::env::var_os("HOME").as_deref(),
            ),
        ))
    }
}

const LOADER_VAR: Option<&str> = if cfg!(target_os = "macos") {
    Some("DYLD_FALLBACK_LIBRARY_PATH")
} else if cfg!(unix) {
    Some("LD_LIBRARY_PATH")
} else {
    None
};

fn loader_path(var: &str, lib: &Path, inherited: Option<&OsStr>, home: Option<&OsStr>) -> OsString {
    let mut path = OsString::from(lib);
    match inherited.filter(|v| !v.is_empty()) {
        Some(rest) => {
            path.push(":");
            path.push(rest);
        }
        None if var == "DYLD_FALLBACK_LIBRARY_PATH" => {
            if let Some(home) = home {
                path.push(":");
                path.push(home);
                path.push("/lib");
            }
            path.push(":/usr/local/lib:/usr/lib");
        }
        None => {}
    }
    path
}

/// Why the checkout's stock pin cannot serve a lane, and what puts that right.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unready {
    pub why: String,
    pub fix: String,
}

impl Unready {
    fn provision(why: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            fix: PROVISION.to_string(),
        }
    }
}

/// One line: what rustup said over several lines (`error: …` then rustup's own `help: …`) is
/// joined, so a row that shows a single line still ends with the fix.
impl std::fmt::Display for Unready {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let why: Vec<&str> = self
            .why
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        write!(f, "{} (fix: {})", why.join(" "), self.fix)
    }
}

/// The checkout's stock pin as rustup holds it ([`stock`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stock {
    /// Its drivers.
    pub toolchain: Toolchain,
    /// The std targets installed on it.
    pub targets: Vec<String>,
}

impl Stock {
    /// The pin's drivers for a lane that compiles for `triple`, and lints it when `clippy`. A pin
    /// without that std, or without the clippy a lint needs, is unready, with [`PROVISION`]: no
    /// other toolchain answers in its place.
    ///
    /// # Errors
    /// The pin lacks `triple`'s std, or clippy where `clippy` asks for it.
    pub fn serving(&self, triple: &str, clippy: bool) -> Result<&Toolchain, Unready> {
        let name = &self.toolchain.name;
        if !self.targets.iter().any(|t| t == triple) {
            return Err(Unready::provision(format!(
                "`{name}`, the checkout's stock pin, has no {triple} std"
            )));
        }
        if clippy && !self.toolchain.clippy {
            return Err(Unready::provision(format!(
                "`{name}`, the checkout's stock pin, has no clippy"
            )));
        }
        Ok(&self.toolchain)
    }
}

/// The stock pin `pin` as `rustup`, whose default host is `host` ([`default_host`]), holds it:
/// its drivers and its installed stds. rustup is asked about the pin only once it lists the
/// pin's install for `host`, so a machine without it (another host's install of the release
/// included) is told so, with [`PROVISION`].
///
/// # Errors
/// rustup cannot list its toolchains, holds no `pin` for `host`, or names no drivers or stds
/// for it.
pub fn stock<A: Ask + ?Sized>(rustup: &A, pin: &str, host: &str) -> Result<Stock, Unready> {
    let listing = rustup.ask(&["toolchain", "list"]).map_err(|why| {
        Unready::provision(format!("rustup could not list its toolchains: {why}"))
    })?;
    if !holds_toolchain(&listing, pin, host) {
        return Err(Unready::provision(format!(
            "rustup holds no `{pin}`, the checkout's stock pin"
        )));
    }
    let toolchain = Toolchain::resolve(rustup, pin).map_err(Unready::provision)?;
    let targets = rustup
        .ask(&["target", "list", "--installed", "--toolchain", pin])
        .map_err(Unready::provision)?
        .lines()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    Ok(Stock { toolchain, targets })
}

/// The stock pin the checkout at `repo` names ([`stock_pin`]), as `rustup` holds it on the
/// default host of the rustup home this process names ([`stock`]).
///
/// # Errors
/// The checkout's engine gate disagrees with the pin or is refused, or [`stock`]'s.
pub fn pinned<A: Ask + ?Sized>(rustup: &A, repo: &Path) -> Result<Stock, Unready> {
    let pin = stock_pin(repo).map_err(|refused| Unready {
        why: refused.why,
        fix: refused.remedy,
    })?;
    stock(
        rustup,
        &pin,
        &aterm_toolchain::rustup::default_host_from_env(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toolchains_lib_leads_the_loader_path_and_keeps_what_was_inherited() {
        let lib = Path::new("/r/toolchains/stable/lib");
        let joined = |var, inherited: Option<&str>, home: Option<&str>| {
            loader_path(var, lib, inherited.map(OsStr::new), home.map(OsStr::new))
        };
        let dyld = "DYLD_FALLBACK_LIBRARY_PATH";
        assert_eq!(
            joined(dyld, Some("/a:/b"), Some("/h")),
            "/r/toolchains/stable/lib:/a:/b"
        );
        for unset in [None, Some("")] {
            assert_eq!(
                joined(dyld, unset, Some("/h")),
                "/r/toolchains/stable/lib:/h/lib:/usr/local/lib:/usr/lib"
            );
        }
        assert_eq!(
            joined(dyld, None, None),
            "/r/toolchains/stable/lib:/usr/local/lib:/usr/lib"
        );
        let ld = "LD_LIBRARY_PATH";
        assert_eq!(joined(ld, None, Some("/h")), "/r/toolchains/stable/lib");
        assert_eq!(joined(ld, Some("/a"), None), "/r/toolchains/stable/lib:/a");
    }

    #[test]
    fn the_loader_path_is_the_lib_beside_the_toolchains_bin() {
        let at = |rustc: &str| Toolchain {
            name: "stable".into(),
            cargo: PathBuf::from("/r/toolchains/stable/bin/cargo"),
            rustc: PathBuf::from(rustc),
            clippy: false,
        };
        let Some(var) = LOADER_VAR else {
            assert_eq!(at("/r/toolchains/stable/bin/rustc").loader_env(), None);
            return;
        };
        let (named, value) = at("/r/toolchains/stable/bin/rustc")
            .loader_env()
            .expect("a toolchain's rustc");
        assert_eq!(named, var);
        assert!(
            value
                .to_string_lossy()
                .starts_with("/r/toolchains/stable/lib"),
            "{value:?}"
        );
        assert_eq!(
            at("rustc").loader_env(),
            None,
            "no toolchain around a bare rustc"
        );
    }

    const HOST: &str = "aarch64-apple-darwin";
    const PIN: &str = "1.97.1";

    /// A rustup holding `toolchains` in its listing's order, each with its stds and whether it
    /// has clippy, whose drivers are files under `dir`; it records every question.
    struct Held {
        dir: aterm_tempfile::TempDir,
        toolchains: Vec<(&'static str, Vec<&'static str>, bool)>,
        asked: std::cell::RefCell<Vec<String>>,
    }

    impl Held {
        fn new(toolchains: Vec<(&'static str, Vec<&'static str>, bool)>) -> Self {
            Self {
                dir: aterm_tempfile::tempdir().unwrap(),
                toolchains,
                asked: std::cell::RefCell::default(),
            }
        }

        fn held(&self, name: &str) -> Option<&(&'static str, Vec<&'static str>, bool)> {
            self.toolchains.iter().find(|(t, _, _)| *t == name)
        }
    }

    impl Ask for Held {
        fn ask(&self, args: &[&str]) -> Result<String, String> {
            self.asked.borrow_mut().push(args.join(" "));
            match args {
                ["toolchain", "list"] => Ok(self
                    .toolchains
                    .iter()
                    .enumerate()
                    .map(|(i, (t, _, _))| {
                        format!("{t}-{HOST}{}\n", if i == 0 { " (default)" } else { "" })
                    })
                    .collect()),
                ["target", "list", "--installed", "--toolchain", name] => self
                    .held(name)
                    .map(|(_, stds, _)| stds.iter().map(|s| format!("{s}\n")).collect())
                    .ok_or_else(|| format!("toolchain '{name}' is not installed")),
                ["which", "--toolchain", name, tool] => {
                    let (_, _, clippy) = self
                        .held(name)
                        .ok_or_else(|| format!("toolchain '{name}' is not installed"))?;
                    if *tool == "cargo-clippy" && !clippy {
                        return Err(format!("'{tool}' is not installed for '{name}'"));
                    }
                    let path = self.dir.path().join(name).join("bin").join(tool);
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(&path, "").unwrap();
                    Ok(format!("{}\n", path.display()))
                }
                other => Err(format!("unexpected rustup question {other:?}")),
            }
        }
    }

    const EVERY: [&str; 7] = crate::provision::CELL_TARGETS;

    /// EVERY LANE RIDES THE PIN, NEVER THE FIRST TOOLCHAIN THAT HAS THE STD: with `stable` listed
    /// first (rustup's own order) and carrying every cell's std and clippy, each cell's std is
    /// served by the pin's own drivers. NEGATIVE CONTROL: a choice that takes the first listed
    /// toolchain carrying the std answers `stable` here, and the name and path checks fail.
    #[test]
    fn every_lane_rides_the_pin_though_stable_is_listed_first_with_every_std() {
        let rustup = Held::new(vec![
            ("stable", EVERY.to_vec(), true),
            (PIN, EVERY.to_vec(), true),
            ("nightly", EVERY.to_vec(), true),
        ]);
        let stock = stock(&rustup, PIN, HOST).unwrap();
        for triple in EVERY {
            let served = stock.serving(triple, true).unwrap();
            assert_eq!(served.name, PIN, "{triple}");
            assert!(
                served.cargo.starts_with(rustup.dir.path().join(PIN))
                    && served.rustc.starts_with(rustup.dir.path().join(PIN)),
                "{triple}: {served:?}"
            );
        }
        let asked = rustup.asked.borrow();
        assert!(
            asked
                .iter()
                .filter(|q| *q != "toolchain list")
                .all(|q| q.contains(&format!("--toolchain {PIN}"))),
            "nothing but the pin is asked about: {asked:#?}"
        );
    }

    /// A PIN THAT LACKS A PIECE IS UNREADY, NAMING `verify provision`, AND NOTHING STANDS IN: the
    /// pin without a cell's std, without clippy, or not held at all is refused though `stable`
    /// carries all of it; a lane that only compiles is served without clippy. NEGATIVE CONTROL:
    /// a `serving` that skips the std check serves the Windows cell, and its assertion fails.
    #[test]
    fn a_pin_missing_its_std_its_clippy_or_itself_is_unready_and_names_provision() {
        let rustup = Held::new(vec![
            ("stable", EVERY.to_vec(), true),
            (PIN, vec![HOST, "wasm32-unknown-unknown"], false),
        ]);
        let stock = stock(&rustup, PIN, HOST).unwrap();
        let no_std = stock.serving("x86_64-pc-windows-msvc", false).unwrap_err();
        assert_eq!(no_std.fix, PROVISION);
        assert!(
            no_std.why.contains("`1.97.1`") && no_std.why.contains("x86_64-pc-windows-msvc std"),
            "{no_std}"
        );
        let no_clippy = stock.serving(HOST, true).unwrap_err();
        assert_eq!(no_clippy.fix, PROVISION);
        assert!(no_clippy.why.contains("has no clippy"), "{no_clippy}");
        assert_eq!(
            stock
                .serving("wasm32-unknown-unknown", false)
                .map(|t| t.name.as_str()),
            Ok(PIN),
            "a lane that only compiles needs no clippy"
        );

        let without = Held::new(vec![("stable", EVERY.to_vec(), true)]);
        let absent = super::stock(&without, PIN, HOST).unwrap_err();
        assert_eq!(absent.fix, PROVISION);
        assert!(absent.why.contains("holds no `1.97.1`"), "{absent}");
        assert_eq!(
            *without.asked.borrow(),
            ["toolchain list"],
            "a pin rustup does not list is asked nothing more"
        );
        assert!(
            absent
                .to_string()
                .ends_with("(fix: targo --unverified verify provision rustup)"),
            "{absent}"
        );
    }

    /// A pin rustup holds only for another host is not held, and rustup is asked nothing more.
    #[test]
    fn a_pin_held_only_for_another_host_is_unready() {
        let rustup = Held::new(vec![(PIN, EVERY.to_vec(), true)]);
        let other = stock(&rustup, PIN, "x86_64-apple-darwin").unwrap_err();
        assert!(other.why.contains("holds no `1.97.1`"), "{other}");
        assert_eq!(*rustup.asked.borrow(), ["toolchain list"]);
    }

    /// Unready reads as one line, rustup's own lines joined, with the fix last.
    #[test]
    fn an_unready_pin_reads_as_one_line_ending_with_its_fix() {
        let unready = Unready::provision(
            "`rustup which` failed: error: toolchain '1.97.1' is not installed\n\
             help: run `rustup toolchain install 1.97.1` to install it\n",
        );
        let said = unready.to_string();
        assert_eq!(said.lines().count(), 1, "{said}");
        assert!(said.ends_with(&format!("(fix: {PROVISION})")), "{said}");
        assert!(said.contains("is not installed help: run"), "{said}");
    }

    /// The checkout's pin, and a public snapshot's (no `publish/`), is [`STOCK_PIN`], asked of
    /// rustup; an engine gate the checkout's reader refuses is unready with the gate's own fix,
    /// and rustup is asked nothing.
    #[test]
    fn the_checkout_and_a_public_snapshot_ride_the_pin_and_a_gate_that_disagrees_is_unready() {
        let repo = aterm_tempfile::manifest_dir().join("../..");
        let public = aterm_tempfile::tempdir().unwrap();
        for tree in [repo.as_path(), public.path()] {
            let rustup = Held::new(vec![("stable", EVERY.to_vec(), true)]);
            let absent = pinned(&rustup, tree).unwrap_err();
            assert!(
                absent.why.contains(&format!("holds no `{STOCK_PIN}`")),
                "{}: {absent}",
                tree.display()
            );
            assert_eq!(absent.fix, PROVISION);
        }

        std::fs::create_dir_all(public.path().join("publish")).unwrap();
        std::fs::write(
            public.path().join(STOCK_PIN_FILE),
            "CHECK_CMD_DEFAULT=\"true\"\n",
        )
        .unwrap();
        let rustup = Held::new(vec![(STOCK_PIN, EVERY.to_vec(), true)]);
        let refused = pinned(&rustup, public.path()).unwrap_err();
        assert!(refused.why.contains("no wasm32 clause"), "{refused}");
        assert!(refused.fix.contains("restore"), "{refused}");
        assert!(
            rustup.asked.borrow().is_empty(),
            "a gate that is refused asks rustup nothing"
        );
    }
}
