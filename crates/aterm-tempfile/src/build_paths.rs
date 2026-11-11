// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Source and build paths, read when the code runs ([`manifest_dir`], [`bin_exe`],
//! [`target_tmpdir`]).
//!
//! Cargo hands each compile the absolute paths of its checkout and build directory, and an
//! `env!` of one compiles that path into the unit. Cargo leaves the variables it sets out of the
//! unit's freshness, so the same unit, reused from a build directory at another path (a second
//! checkout of the commit, a seeded one), would read the first checkout's files and run its
//! helpers. These read each path at run time instead; aterm-census's `build-path-value` guard
//! keeps the compile-time reads out.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The variable naming the running package's directory.
const MANIFEST_DIR: &str = "CARGO_MANIFEST_DIR";

/// A variable's value when the environment sets it to something.
fn set(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

/// The directory of the package whose test, example or binary is running: `CARGO_MANIFEST_DIR`
/// in this process's environment. `targo test`, `targo run` and `targo --unverified verify` set
/// it for every process they start, to the package of the code that runs.
///
/// # Panics
/// When the variable is unset or empty: Cargo or verify did not start this process (a test
/// executable copied out and run by hand), so no checkout is known.
#[must_use]
pub fn manifest_dir() -> PathBuf {
    set(MANIFEST_DIR).map_or_else(
        || {
            panic!(
                "{MANIFEST_DIR} is unset: run this through `targo test`, `targo run` or `targo \
                 --unverified verify`, which name the package's directory"
            )
        },
        PathBuf::from,
    )
}

/// The executable of this package's binary target `name`, which Cargo names
/// `CARGO_BIN_EXE_<name>` only while it compiles an integration test or a bench.
///
/// The variable when this process's environment sets it: `targo --unverified verify` sets it for
/// each test executable it runs, from the compile's own report of the package's binaries. Else
/// Cargo's layout for a plain `targo test`, the first of these that holds a file:
/// - beside the deps of this test executable's profile folder ([`profile_folder`]), where the
///   classic layout, whose build directory is the target directory, puts the binaries;
/// - the folder Cargo copies that profile's binaries to ([`uplifted`]): `<target>/<profile>`, or
///   `<target>/<triple>/<profile>` under `CARGO_BUILD_TARGET`, where `<target>` is
///   `CARGO_TARGET_DIR` (or `CARGO_BUILD_TARGET_DIR`), else the workspace's `target`.
///
/// A run whose build directory is not its target directory and that names `--target` or
/// `--target-dir` on the command line, which this process cannot see, goes through verify or
/// sets the variable.
///
/// # Panics
/// When the variable is unset and the layout names no executable: this is not a test executable
/// in Cargo's build layout, `CARGO_TARGET_DIR` is relative, or nothing is at either path.
#[must_use]
pub fn bin_exe(name: &str) -> PathBuf {
    let var = format!("CARGO_BIN_EXE_{name}");
    if let Some(exe) = set(&var) {
        return PathBuf::from(exe);
    }
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let profile = profile_folder().unwrap_or_else(|why| panic!("{var} is unset and {why}"));
    let beside = profile.join(&file);
    if beside.is_file() {
        return beside;
    }
    let copied = target_dir()
        .map(|target| uplifted(&target, set("CARGO_BUILD_TARGET"), &profile).join(&file))
        .unwrap_or_else(|why| panic!("{var} is unset and {why}"));
    if copied.is_file() {
        return copied;
    }
    panic!(
        "{var} is unset and Cargo's layout has no binary at {} or {}: build it with `targo \
         test`, or run through `targo --unverified verify`, which sets {var}",
        beside.display(),
        copied.display()
    )
}

/// The folder of `target` that Cargo copies the binaries built in the profile folder `profile`
/// to: `<target>/<profile>`, or `<target>/<triple>/<profile>` for a build for `triple`.
fn uplifted(target: &Path, triple: Option<OsString>, profile: &Path) -> PathBuf {
    let mut dir = target.to_path_buf();
    dir.extend(triple);
    dir.join(profile.file_name().unwrap_or_default())
}

/// Cargo's scratch directory for a package's integration tests and benches
/// (`CARGO_TARGET_TMPDIR`): the variable when this process's environment sets it, else the `tmp`
/// folder beside the profile folder of the build directory that holds this test executable
/// ([`profile_folder`]), which is where Cargo makes it (`<build>/tmp`, `<build>/<triple>/tmp` under `--target`). Created when absent.
///
/// # Panics
/// When the variable is unset and this is not a test executable in Cargo's build layout, or the
/// directory cannot be created.
#[must_use]
pub fn target_tmpdir() -> PathBuf {
    let dir = set("CARGO_TARGET_TMPDIR").map_or_else(
        || {
            profile_folder()
                .and_then(|profile| {
                    profile
                        .parent()
                        .map(|build| build.join("tmp"))
                        .ok_or_else(|| format!("{} has no build directory", profile.display()))
                })
                .unwrap_or_else(|why| panic!("CARGO_TARGET_TMPDIR is unset and {why}"))
        },
        PathBuf::from,
    );
    if let Err(e) = std::fs::create_dir_all(&dir) {
        panic!("cannot create {}: {e}", dir.display());
    }
    dir
}

/// The profile folder (`<build>/debug`, `<build>/<triple>/debug`) of the build directory that
/// holds this test executable, in either of Cargo's layouts: `<profile>/deps/<exe>`, or
/// `<profile>/build/<package>/<hash>/out/<exe>` (the build-dir layout, which targo uses).
fn profile_folder() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("this executable is unknown: {e}"))?;
    profile_of(&exe).ok_or_else(|| format!("{} is not in Cargo's build layout", exe.display()))
}

/// [`profile_folder`] for the executable at `exe`.
fn profile_of(exe: &Path) -> Option<PathBuf> {
    let named = |dir: &Path, name: &str| dir.file_name().is_some_and(|n| n == name);
    let parent = exe.parent()?;
    if named(parent, "deps") {
        return parent.parent().map(Path::to_path_buf);
    }
    let build = parent.ancestors().nth(3)?;
    (named(parent, "out") && named(build, "build"))
        .then(|| build.parent().map(Path::to_path_buf))
        .flatten()
}

/// Cargo's target directory for this run: `CARGO_TARGET_DIR`, else `CARGO_BUILD_TARGET_DIR`,
/// else the workspace's `target`.
fn target_dir() -> Result<PathBuf, String> {
    for var in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"] {
        if let Some(dir) = set(var) {
            let dir = PathBuf::from(dir);
            return if dir.is_absolute() {
                Ok(dir)
            } else {
                Err(format!(
                    "{var} is relative ({}), so its folder is Cargo's caller's",
                    dir.display()
                ))
            };
        }
    }
    let package = manifest_dir();
    workspace_root(&package)
        .map(|root| root.join("target"))
        .ok_or_else(|| format!("no workspace manifest is at or above {}", package.display()))
}

/// The nearest directory at or above `package` whose `Cargo.toml` opens a `[workspace]` table.
fn workspace_root(package: &Path) -> Option<PathBuf> {
    package
        .ancestors()
        .find(|dir| {
            std::fs::read_to_string(dir.join("Cargo.toml"))
                .is_ok_and(|text| text.lines().any(|l| l.trim() == "[workspace]"))
        })
        .map(Path::to_path_buf)
}

#[cfg(test)]
#[path = "build_paths_tests.rs"]
mod tests;
