// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Benchmark build profile selection (`dev` or `opt`) and binary identity checks.

use std::path::Path;

/// One arm of the measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// The `test` profile: the dev measurement's original code generation.
    #[default]
    Dev,
    /// `--profile opt-test`: release code generation, debug assertions on.
    Opt,
}

impl Profile {
    /// Every arm, in the order `--profile` lists them.
    pub const ALL: [Profile; 2] = [Profile::Dev, Profile::Opt];

    /// The arm's name: the lane's `--profile` value and the bench's report.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Profile::Dev => "dev",
            Profile::Opt => "opt",
        }
    }

    /// The arm `--profile <raw>` names.
    /// # Errors
    /// Anything but an arm's [`name`](Profile::name).
    pub fn parse(raw: &str) -> Result<Profile, String> {
        Profile::ALL
            .into_iter()
            .find(|p| p.name() == raw)
            .ok_or_else(|| format!("`--profile` is `dev` or `opt`, not `{raw}`"))
    }

    /// Explicit cargo profile name for this arm.
    #[must_use]
    pub const fn cargo_profile(self) -> &'static str {
        match self {
            Profile::Dev => "test",
            Profile::Opt => "opt-test",
        }
    }

    /// The directory under the target dir cargo writes the arm's binaries to.
    #[must_use]
    pub const fn dir(self) -> &'static str {
        match self {
            Profile::Dev => "debug",
            Profile::Opt => "opt-test",
        }
    }

    /// The code generation the arm stands for, for the report and the ledger.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Profile::Dev => {
                "cargo `test` profile (inherits dev): opt-level 0 but the per-crate speed rows \
                 of [profile.dev.package.<name>] (inherited) and [profile.test.package.<name>], \
                 debug assertions and overflow checks on"
            }
            Profile::Opt => {
                "cargo profile `opt-test` (inherits release): opt-level 3 on every crate, no LTO, \
                 16 codegen units, debug assertions on (the test seams), overflow checks off (as \
                 shipped)"
            }
        }
    }

    /// Required opt-level across all crates on the measured path, or `None` for dev.
    #[must_use]
    pub const fn every_crate_at(self) -> Option<&'static str> {
        match self {
            Profile::Dev => None,
            Profile::Opt => Some("3"),
        }
    }

    /// Returns the arm whose build directory holds `exe`, if any.
    #[must_use]
    pub fn of_binary(exe: &Path) -> Option<Profile> {
        let dir = exe.parent()?.file_name()?.to_str()?;
        Profile::ALL.into_iter().find(|p| p.dir() == dir)
    }
}

/// Returns the built file's length and nanosecond modification time.
/// # Errors
/// A file whose metadata or time cannot be read.
pub fn identity(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .ok_or_else(|| format!("{}: no modification time", path.display()))?;
    Ok(format!(
        "{} bytes, modified {}.{:09}",
        meta.len(),
        modified.as_secs(),
        modified.subsec_nanos()
    ))
}
