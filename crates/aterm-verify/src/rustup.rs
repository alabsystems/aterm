// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! rustup's toolchains, for the STOCK EXCEPTION lanes: the foreign cells
//! (`verify cells`) and the wasm perf lane. Trust carries only the host std,
//! so a cross triple is compiled by a rustup toolchain that has its std,
//! driven by that toolchain's own `cargo` and `rustc` by path. Where rustup
//! is, and what its listings say, is `aterm_toolchain::rustup`'s one rule.

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

/// A rustup toolchain resolved to its own drivers by path, never a `cargo`
/// off PATH trusted to honour `RUSTUP_TOOLCHAIN` (only rustup's proxies read
/// it). `RUSTC` pins the compiler for cargo and every build script.
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
    ///
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

    /// The library search variable a rustup proxy hands the drivers it runs:
    /// this toolchain's `lib/` first, then what this process inherited. Its
    /// drivers run by path here, never through a proxy, and stable 1.98's
    /// `rust-lld` loads `@rpath/libLLVM.dylib` from there alone (its rpath is
    /// its own `rustlib/<host>/lib` and a CI path), so without it every wasm32
    /// link dies in dyld. `None` off unix, or when `rustc` stands outside a
    /// `<toolchain>/bin`.
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

/// The variable the dynamic loader searches after a binary's own rpaths.
const LOADER_VAR: Option<&str> = if cfg!(target_os = "macos") {
    Some("DYLD_FALLBACK_LIBRARY_PATH")
} else if cfg!(unix) {
    Some("LD_LIBRARY_PATH")
} else {
    None
};

/// `lib` first, then `inherited`. An unset `DYLD_FALLBACK_LIBRARY_PATH` means
/// dyld's default fallback, which setting the variable replaces, so that
/// default follows `lib` as rustup keeps it.
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

/// The first installed toolchain carrying `triple`'s std, release channels
/// before nightly (a nightly accepts what a release refuses): `Ok(None)` when
/// none does.
///
/// # Errors
/// rustup could not list its toolchains, or one that carries the std named
/// no drivers.
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
        // Unset (or empty), dyld's own default stays behind the toolchain's.
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
}
