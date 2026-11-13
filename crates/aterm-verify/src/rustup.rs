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
#[path = "rustup_tests.rs"]
mod tests;
