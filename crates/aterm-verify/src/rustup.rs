// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! rustup's toolchains for the STOCK EXCEPTION lanes (`verify cells` and wasm perf).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use aterm_uds::output::CommandOutput as _;
use aterm_uds::spawnfd::{Command, command};

pub use aterm_toolchain::rustup::{
    RUSTUP_LOOKED, find_rustup, find_rustup_in, lists_target, toolchain_names,
};

/// Does `rustup`'s toolchain `name` carry `triple`'s std?
#[must_use]
pub fn carries(rustup: &Path, name: &str, triple: &str) -> bool {
    command(rustup)
        .args(["target", "list", "--installed", "--toolchain", name])
        .output_owned()
        .is_ok_and(|o| {
            o.status.success() && lists_target(&String::from_utf8_lossy(&o.stdout), triple)
        })
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
    pub fn resolve(rustup: &Path, name: &str) -> Result<Self, String> {
        let which = |tool: &str| -> Result<PathBuf, String> {
            let out = command(rustup)
                .args(["which", "--toolchain", name, tool])
                .output_owned()
                .map_err(|e| format!("could not ask rustup for `{name}`'s {tool} ({e})"))?;
            let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            if out.status.success() && path.is_file() {
                Ok(path)
            } else {
                Err(format!(
                    "`rustup which --toolchain {name} {tool}` names no file: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
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

/// The first installed toolchain carrying `triple`'s std (`Ok(None)` when none does).
/// # Errors
/// rustup could not list its toolchains, or one that carries the std named no drivers.
pub fn with_std(rustup: &Path, triple: &str) -> Result<Option<Toolchain>, String> {
    let list = command(rustup)
        .args(["toolchain", "list"])
        .output_owned()
        .map_err(|e| format!("could not ask rustup which toolchains are installed ({e})"))?;
    if !list.status.success() {
        return Err(format!(
            "`rustup toolchain list` failed: {}",
            String::from_utf8_lossy(&list.stderr).trim_end()
        ));
    }
    let mut names = toolchain_names(&String::from_utf8_lossy(&list.stdout));
    names.sort_by_key(|tc| u8::from(tc.starts_with("nightly")));
    names
        .into_iter()
        .find(|tc| carries(rustup, tc, triple))
        .map(|tc| Toolchain::resolve(rustup, &tc))
        .transpose()
}

/// The wasm32 clause of `publish/config.sh`'s `CHECK_CMD_DEFAULT` and the stock channel it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmClause {
    pub clause: String,
    pub pin: String,
}

/// Why `publish/config.sh` names no usable wasm32 pin, and the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClauseError {
    pub why: String,
    pub remedy: String,
}

impl ClauseError {
    fn new(why: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            remedy: remedy.into(),
        }
    }
}

/// Reads the wasm32 clause out of `config` (`CHECK_CMD_DEFAULT="…"`).
/// # Errors
/// No clause; no stock channel; no target-add before it; no `RUSTC` naming the pin's own rustc.
pub fn wasm_clause(config: &str) -> Result<WasmClause, ClauseError> {
    let line = config
        .lines()
        .find_map(|l| l.strip_prefix("CHECK_CMD_DEFAULT=\""))
        .and_then(|l| l.trim_end().strip_suffix('"'))
        .unwrap_or_default();
    let Some(clause) = line
        .split(" && ")
        .find(|c| c.contains("--target wasm32-unknown-unknown"))
        .map(str::trim)
    else {
        return Err(ClauseError::new(
            "publish/config.sh's CHECK_CMD_DEFAULT has no wasm32 clause — nothing before the \
             promote compiles the site's /terminal engine",
            "restore its `cargo check --locked --target wasm32-unknown-unknown -p aterm-wasm` \
             clause (publish/config.sh, THE WASM32 CLAUSE)",
        ));
    };
    let pin = clause
        .split_once("rustup run ")
        .and_then(|(_, rest)| rest.split_once(" cargo "))
        .map(|(pin, _)| pin.to_string())
        .filter(|p| !p.is_empty() && !p.contains(' '));
    let Some(pin) = pin else {
        return Err(ClauseError::new(
            format!(
                "publish/config.sh's wasm32 clause names no stock channel — the wasm32 pin is the \
                 compiler this check and the stage's gate both run: {clause}"
            ),
            "spell the clause `rustup run <channel> cargo check …`",
        ));
    };
    let add = format!("rustup target add --toolchain {pin} wasm32-unknown-unknown");
    if !line.contains(&format!("{add} && ")) {
        return Err(ClauseError::new(
            format!(
                "publish/config.sh's CHECK_CMD_DEFAULT does not add the wasm32 target to {pin} \
                 before its wasm32 clause — `pub stage` on a host without that target fails after \
                 the verified build"
            ),
            format!("put `{add} && ` before the wasm32 clause"),
        ));
    }
    let rustc = format!("RUSTC=$(rustup which --toolchain {pin} rustc) ");
    if !clause.contains(&rustc) {
        return Err(ClauseError::new(
            format!(
                "publish/config.sh's wasm32 clause does not name {pin}'s own rustc — cargo would \
                 take `rustc` from the host PATH (the gate's fresh CARGO_HOME holds no rustup \
                 proxy): {clause}"
            ),
            format!("put `{rustc}` before `rustup run {pin} cargo` in the wasm32 clause"),
        ));
    }
    Ok(WasmClause {
        clause: clause.to_string(),
        pin,
    })
}

/// The wasm32 pin `repo`'s `publish/config.sh` names, or why it names none.
/// # Errors
/// The file cannot be read, or its clause is refused ([`wasm_clause`]).
pub fn wasm_pin(repo: &Path) -> Result<String, String> {
    std::fs::read_to_string(repo.join("publish/config.sh"))
        .map_err(|e| format!("publish/config.sh: {e}"))
        .and_then(|c| wasm_clause(&c).map_err(|r| format!("{} — {}", r.why, r.remedy)))
        .map(|clause| clause.pin)
}

/// The wasm32 pin a provisioned machine holds for `repo`.
/// # Errors
/// The file exists and cannot be read, or its clause is refused ([`wasm_clause`]).
pub fn wasm_pin_wanted(repo: &Path) -> Result<Option<String>, String> {
    match std::fs::symlink_metadata(repo.join("publish/config.sh")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => wasm_pin(repo).map(Some),
    }
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

    #[test]
    fn the_wasm32_clause_is_read_out_of_the_engines_own_line() {
        let good = "CHECK_CMD_DEFAULT=\"rustup run trust targo trust check && rustup target add \
                    --toolchain 1.95.0 wasm32-unknown-unknown && sh -c \
                    'RUSTFLAGS=-Ctarget-feature=+simd128 RUSTC=$(rustup which --toolchain 1.95.0 \
                    rustc) rustup run 1.95.0 cargo check --locked --target wasm32-unknown-unknown \
                    -p aterm-wasm'\"\n";
        let clause = wasm_clause(good).unwrap();
        assert_eq!(clause.pin, "1.95.0");
        assert!(
            clause
                .clause
                .starts_with("sh -c 'RUSTFLAGS=-Ctarget-feature=+simd128 RUSTC=")
        );
        for config in [
            "CHECK_CMD_DEFAULT=\"targo check\"\n",
            &good.replace(
                "rustup target add --toolchain 1.95.0 wasm32-unknown-unknown && ",
                "",
            ),
            &good.replace("RUSTC=$(rustup which --toolchain 1.95.0 rustc) ", ""),
            &good.replace("rustup run 1.95.0 cargo", "cargo"),
        ] {
            assert!(wasm_clause(config).is_err(), "{config}");
        }
    }
}
