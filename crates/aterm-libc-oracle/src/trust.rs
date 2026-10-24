// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Discovers the pinned Trust toolchain and its native host triple.

use std::path::{Path, PathBuf};

use aterm_toolchain::discovery;

use crate::verdict::CouldNotRun;

/// The pinned toolchain's drivers and the host it compiles for natively.
#[derive(Clone, Debug)]
pub struct Trust {
    pub targo: PathBuf,
    pub trustc: PathBuf,
    pub trustdoc: PathBuf,
    /// `trustc -vV`'s `host:` triple.
    pub host: String,
}

/// The `host:` line of `trustc -vV`.
#[must_use]
pub fn parse_host(verbose_version: &str) -> Option<String> {
    verbose_version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_string)
}

/// Whether `libdir` holds the `lib<krate>-*.rlib` a host build links.
#[must_use]
pub fn has_rlib(libdir: &Path, krate: &str) -> bool {
    let prefix = format!("lib{krate}-");
    std::fs::read_dir(libdir).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(&prefix) && name.ends_with(".rlib")
        })
    })
}

fn ask(trustc: &Path, args: &[&str]) -> Result<String, CouldNotRun> {
    let shown = format!("{} {}", trustc.display(), args.join(" "));
    let out = aterm_uds::spawnfd::command(trustc)
        .args(args)
        .output()
        .map_err(|e| CouldNotRun::new(format!("{shown} could not start: {e}"), REPAIR))?;
    if !out.status.success() {
        return Err(CouldNotRun::new(
            format!(
                "{shown} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            REPAIR,
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

const REPAIR: &str = "`aterm pkg doctor`, then `aterm pkg repair`";

/// Find the pinned toolchain for the checkout at `root` and ask it its host.
/// # Errors
/// When no pinned toolchain with `targo`, `trustc` and `trustdoc` is found, or it cannot report a
/// host or link a host build.
pub fn discover(root: &Path) -> Result<Trust, CouldNotRun> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let stage2 = std::env::var_os("TRUST_STAGE2_BIN").map(PathBuf::from);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let toolchain = discovery::Toolchain::discover(
        stage2.as_deref(),
        &home,
        &path,
        discovery::pinned_channel(root).as_deref(),
    );
    if !toolchain.have_targo() {
        return Err(CouldNotRun::bare(toolchain.missing_targo_label()));
    }
    if !toolchain.have_trustdoc() {
        return Err(CouldNotRun::bare(toolchain.missing_trustdoc_label()));
    }
    let trustc = toolchain.stage2_dir.join("trustc");
    if !discovery::is_executable_file(&trustc) {
        return Err(CouldNotRun::new(
            format!("no trustc beside {}", toolchain.targo.display()),
            REPAIR,
        ));
    }
    let host = parse_host(&ask(&trustc, &["-vV"])?)
        .ok_or_else(|| CouldNotRun::new("trustc -vV reported no host triple", REPAIR))?;
    let libdir = ask(&trustc, &["--print", "target-libdir", "--target", &host])?;
    let libdir = PathBuf::from(libdir.trim());
    for krate in ["core", "std"] {
        if !has_rlib(&libdir, krate) {
            return Err(CouldNotRun::new(
                format!("lib{krate} for {host} is absent from {}", libdir.display()),
                REPAIR,
            ));
        }
    }
    Ok(Trust {
        targo: toolchain.targo,
        trustc,
        trustdoc: toolchain.trustdoc,
        host,
    })
}

impl Trust {
    /// The formatter beside the pinned `trustc`.
    #[must_use]
    pub fn trustfmt(&self) -> PathBuf {
        self.trustc.with_file_name("trustfmt")
    }
}

/// A rustup toolchain's drivers (STOCK EXCEPTION: cross cells compile with stock Rust).
#[derive(Clone, Debug)]
pub struct Stock {
    /// The toolchain as named (`stable`).
    pub name: String,
    pub cargo: PathBuf,
    pub rustc: PathBuf,
    pub rustdoc: PathBuf,
    /// `lib/rustlib`, where each installed target's std lives.
    pub rustlib: PathBuf,
}

impl Stock {
    /// Whether the toolchain carries a std for `triple`.
    #[must_use]
    pub fn has_target(&self, triple: &str) -> bool {
        has_rlib(&self.rustlib.join(triple).join("lib"), "core")
    }
}

/// Locates the rustup toolchain `name` (`stable` or `<name>-<host>`).
/// # Errors
/// When rustup holds no such toolchain.
pub fn stock(name: &str, host: &str) -> Result<Stock, CouldNotRun> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let rustup = discovery::rustup_home(std::env::var_os("RUSTUP_HOME").as_deref(), &home);
    let toolchains = rustup.join("toolchains");
    [name.to_string(), format!("{name}-{host}")]
        .iter()
        .map(|dir| toolchains.join(dir))
        .find(|dir| discovery::is_executable_file(&dir.join("bin/cargo")))
        .map(|dir| Stock {
            name: name.to_string(),
            cargo: dir.join("bin/cargo"),
            rustc: dir.join("bin/rustc"),
            rustdoc: dir.join("bin/rustdoc"),
            rustlib: dir.join("lib/rustlib"),
        })
        .ok_or_else(|| {
            CouldNotRun::new(
                format!(
                    "no rustup toolchain `{name}` under {} for the cells this box is not native to",
                    toolchains.display()
                ),
                format!("rustup toolchain install {name}, or name another with --stock"),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_is_the_verbose_versions_host_line() {
        let vv = "rustc 1.99.0-dev (321aaeda7 2026-09-17) (trustc 0.1.0)\nbinary: trustc\nhost: aarch64-apple-darwin\nrelease: 1.99.0-dev\n";
        assert_eq!(parse_host(vv).as_deref(), Some("aarch64-apple-darwin"));
        assert_eq!(parse_host("host: \n"), None);
        assert_eq!(parse_host("binary: trustc\n"), None);
    }
}
