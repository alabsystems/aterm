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
        assert_eq!(
            Profile::of_binary(Path::new("/w/target/aarch64-apple-darwin/opt-test/aterm")),
            Some(Profile::Opt)
        );
        for other in [
            "/w/target/release/aterm",
            "/w/target/profiling/aterm",
            "/Applications/aterm.app/Contents/MacOS/aterm",
            "aterm",
            "/",
        ] {
            assert_eq!(Profile::of_binary(Path::new(other)), None, "{other}");
        }
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
                custom if custom != "test" => assert_eq!(p.dir(), custom, "{p:?}"),
                _ => assert_eq!(p.dir(), "debug", "{p:?}"),
            }
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
        std::fs::write(&exe, b"another build").expect("write");
        assert_ne!(identity(&exe).expect("reads"), first);
        std::fs::remove_dir_all(&dir).expect("cleanup");
        assert!(identity(&exe).is_err(), "no file is no identity");
    }
}
