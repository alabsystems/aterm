// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which build a run measures: the lane's `--profile dev|opt`, and how the
//! bench names the build it is running from.
//!
//! TWO ARMS, ONE CODE PATH. Both enable debug assertions, so
//! every `#[cfg(any(test, debug_assertions))]` seam the harness calls compiles
//! (AGENTS.md) and the headless `aterm` reads the `ATERM_FABRIC_COMMAND`
//! development seam that points it at the bridge. They differ only in code
//! generation:
//!
//! - **dev** explicitly selects the `test` profile, which inherits `dev`: opt-level
//!   0 except the root `Cargo.toml`'s per-crate speed rows, from two tables —
//!   `[profile.dev.package.<name>]`, which `test` inherits (aterm-core,
//!   aterm-grid, aterm-render … at 2, aterm-spec at 1), and
//!   `[profile.test.package.<name>]`, the test profile's own (aterm-effects at
//!   opt-level 1) — with overflow checks on. Every ledger entry before the opt
//!   arm is this arm, so it stays the default; which crates the rows reach is
//!   the lane's to read off cargo's plan, not this table's to list.
//! - **opt** is the root `Cargo.toml`'s `[profile.opt-test]`: it inherits
//!   `release`, NOT `dev`, so no dev speed row reaches it — a custom profile
//!   that inherited `dev` would keep those rows, and a named row outranks the
//!   profile's own `package."*"`, so aterm-spec would stay at opt-level 1 under
//!   an "optimized" label. Opt-level 3 on every crate, no LTO, 16 codegen
//!   units, debug assertions on (the seams), overflow checks off (release's:
//!   the arm stands in for the shipped build, which has none). The lane checks
//!   the build it measures against that claim, crate by crate, before it
//!   measures (`aterm-verify`'s `perf/fabric.rs`).
//!
//! WHERE EACH ARM'S BINARY IS. cargo writes a profile's binaries to
//! `<target dir>/<profile dir>/` — `debug` for `dev` and `test`, the profile's
//! own name for a custom one, wherever its intermediates go (atpkg's shim puts
//! those in its own build directory) — and hands a test or bench target that path in
//! `CARGO_BIN_EXE_<name>`. The harness launches its `aterm` (the window and,
//! through the seam, the bridge) by that path, which other builds write too,
//! so the bench reports the path, the arm it names ([`Profile::of_binary`]) and
//! the file's [`identity`] at its start and at its end; the lane holds all of
//! it to the binary its own build reported, read right after that build.
//!
//! Std-only: the lane mounts this file the way it mounts `spec.rs`, so the two
//! sides read one table.

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
    pub fn name(self) -> &'static str {
        match self {
            Profile::Dev => "dev",
            Profile::Opt => "opt",
        }
    }

    /// The arm `--profile <raw>` names.
    ///
    /// # Errors
    ///
    /// Anything but an arm's [`name`](Profile::name).
    pub fn parse(raw: &str) -> Result<Profile, String> {
        Profile::ALL
            .into_iter()
            .find(|p| p.name() == raw)
            .ok_or_else(|| format!("`--profile` is `dev` or `opt`, not `{raw}`"))
    }

    /// The explicit cargo profile: `test` keeps the dev measurement unchanged
    /// when the entry is built as a bench, whose default profile is release.
    #[must_use]
    pub fn cargo_profile(self) -> &'static str {
        match self {
            Profile::Dev => "test",
            Profile::Opt => "opt-test",
        }
    }

    /// The directory under the target dir cargo writes the arm's binaries to.
    #[must_use]
    pub fn dir(self) -> &'static str {
        match self {
            Profile::Dev => "debug",
            Profile::Opt => "opt-test",
        }
    }

    /// The code generation the arm stands for, for the report and the ledger.
    #[must_use]
    pub fn describe(self) -> &'static str {
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

    /// The opt-level the arm promises every crate on the measured path, which
    /// the lane holds its build to; `None` for dev, whose mix of speed rows is
    /// what dev means (and is reported, not held).
    #[must_use]
    pub fn every_crate_at(self) -> Option<&'static str> {
        match self {
            Profile::Dev => None,
            Profile::Opt => Some("3"),
        }
    }

    /// The arm whose build directory holds the binary at `exe`: its parent
    /// directory's name, which is the profile's (`<target>/<profile dir>/<bin>`,
    /// or `<target>/<triple>/<profile dir>/<bin>` for a cross build). `None` for
    /// a binary no arm builds — `target/release/aterm`, an installed one.
    /// Pure (unit-tested).
    #[must_use]
    pub fn of_binary(exe: &Path) -> Option<Profile> {
        let dir = exe.parent()?.file_name()?.to_str()?;
        Profile::ALL.into_iter().find(|p| p.dir() == dir)
    }
}

/// The built file's length and nanosecond modification time. Profile binary
/// paths are shared with other builds, so the lane compares this immediately
/// after building with the bench's readings at its start and end. A changed
/// reading refuses the report. Inodes are excluded: atpkg's cache copies a
/// fresh target by clone on macOS, preserving modification time with a new inode.
///
/// # Errors
///
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

#[cfg(test)]
mod tests {
    #[test]
    fn each_arm_parses_from_its_name_and_nothing_else_does() {
        use super::*;

        for p in Profile::ALL {
            assert_eq!(Profile::parse(p.name()), Ok(p));
        }
        assert_eq!(
            Profile::default(),
            Profile::Dev,
            "the arm every entry so far ran"
        );
        for bad in ["", "release", "opt-test", "Dev", "debug", "test"] {
            assert!(Profile::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_binary_names_the_arm_whose_directory_holds_it() {
        use super::*;

        for p in Profile::ALL {
            let exe = Path::new("/w/target").join(p.dir()).join("aterm");
            assert_eq!(Profile::of_binary(&exe), Some(p));
        }
        assert_eq!(
            Profile::of_binary(Path::new("/w/target/debug/aterm")),
            Some(Profile::Dev)
        );
        assert_eq!(
            Profile::of_binary(Path::new("/w/target/opt-test/aterm")),
            Some(Profile::Opt)
        );
        // A cross build nests the profile dir under the triple.
        assert_eq!(
            Profile::of_binary(Path::new("/w/target/aarch64-apple-darwin/opt-test/aterm")),
            Some(Profile::Opt)
        );
        // No arm builds these.
        for other in [
            "/w/target/release/aterm",
            "/w/target/profiling/aterm",
            "/Applications/aterm.app/Contents/MacOS/aterm",
            "aterm",
            "/",
        ] {
            assert_eq!(Profile::of_binary(Path::new(other)), None, "{other}");
        }
        // The arm's dir is the binary's PARENT, not any ancestor: a checkout
        // that happens to live under a `debug` directory is not the dev arm.
        assert_eq!(
            Profile::of_binary(Path::new("/debug/w/target/opt-test/aterm")),
            Some(Profile::Opt)
        );
        assert_eq!(
            Profile::of_binary(Path::new("/debug/w/target/release/aterm")),
            None
        );
    }

    #[test]
    fn an_arm_is_looked_up_where_cargo_writes_its_profile() {
        use super::*;

        for p in Profile::ALL {
            match p.cargo_profile() {
                // A custom profile's binaries land in a directory of its own
                // name.
                custom if custom != "test" => assert_eq!(p.dir(), custom, "{p:?}"),
                // `targo test`'s own profile, `test`, writes to `debug`.
                _ => assert_eq!(p.dir(), "debug", "{p:?}"),
            }
            // What the ledger says of an arm is the level the lane holds its
            // build to.
            if let Some(level) = p.every_crate_at() {
                let promise = format!("opt-level {level} on every crate");
                assert!(p.describe().contains(&promise), "{p:?}: {}", p.describe());
            }
        }
    }

    #[test]
    fn a_rewritten_binary_is_another_identity() {
        use super::*;

        let dir = std::env::temp_dir().join(format!("fabric-identity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let exe = dir.join("aterm");
        std::fs::write(&exe, b"one build").expect("write");
        let first = identity(&exe).expect("reads");
        assert_eq!(identity(&exe), Ok(first.clone()), "one file reads the same");
        assert!(first.starts_with("9 bytes, modified "), "{first}");
        // Another build written to the same path.
        std::fs::write(&exe, b"another build").expect("write");
        assert_ne!(identity(&exe).expect("reads"), first);
        std::fs::remove_dir_all(&dir).expect("cleanup");
        assert!(identity(&exe).is_err(), "no file is no identity");
    }
}
