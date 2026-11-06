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
mod tests {
    use super::*;

    /// A child's answer is bounded by a hang detector, never a latency budget.
    const MUST_HAPPEN: std::time::Duration = std::time::Duration::from_secs(60);

    /// Set in a child this test binary starts: which helper it prints.
    const ASK: &str = "ATERM_TEMPFILE_BUILD_PATH_ASK";

    /// The child half: print what the helper `ASK` names reads in this process, and report
    /// whether this process is that child.
    fn answer_if_asked() -> bool {
        let Some(ask) = std::env::var_os(ASK) else {
            return false;
        };
        let got = match ask.to_str() {
            Some("manifest") => manifest_dir(),
            Some("bin") => bin_exe("probe"),
            Some("tmp") => target_tmpdir(),
            other => panic!("unknown ask {other:?}"),
        };
        println!("ANSWER={}", got.display());
        true
    }

    /// This test binary, run again as a child on test `name` with `ask` and `env`.
    fn ask_child(name: &str, ask: &str, env: &[(&str, &Path)], unset: &[&str]) -> (bool, String) {
        let mut cmd =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
        cmd.args([
            "--exact",
            name,
            "--test-threads=1",
            "--nocapture",
            "--quiet",
        ])
        .env(ASK, ask);
        for (k, v) in env {
            cmd.env(k, v);
        }
        for k in unset {
            cmd.env_remove(k);
        }
        let out = aterm_uds::deadline::output_within(&mut cmd, MUST_HAPPEN).expect("the child ran");
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        (out.status.success(), text)
    }

    /// The path a child printed.
    fn answer(text: &str) -> PathBuf {
        let at = text.find("ANSWER=").expect("an answer") + "ANSWER=".len();
        let rest = &text[at..];
        PathBuf::from(rest.lines().next().unwrap_or(rest))
    }

    /// The package's directory is the one this process is handed, never the one it was compiled
    /// in: a child handed another directory reads that one. NEGATIVE CONTROL: an `env!` read
    /// would print this checkout's directory; and a child handed none refuses, naming the variable.
    #[test]
    fn the_package_directory_is_read_when_the_code_runs() {
        const NAME: &str = "build_paths::tests::the_package_directory_is_read_when_the_code_runs";
        if answer_if_asked() {
            return;
        }
        let here = manifest_dir();
        assert!(
            std::fs::read_to_string(here.join("Cargo.toml"))
                .expect("this package's manifest")
                .contains("name = \"aterm-tempfile\""),
            "{} is not this package",
            here.display()
        );
        let elsewhere = crate::tempdir().expect("scratch");
        let (ok, text) = ask_child(NAME, "manifest", &[(MANIFEST_DIR, elsewhere.path())], &[]);
        assert!(ok, "{text}");
        assert_eq!(answer(&text), elsewhere.path(), "{text}");
        let (ok, text) = ask_child(NAME, "manifest", &[], &[MANIFEST_DIR]);
        assert!(!ok, "a child with no package directory must refuse: {text}");
        assert!(text.contains("CARGO_MANIFEST_DIR is unset"), "{text}");
    }

    /// A package binary is the one the run names, else the one Cargo's layout puts under the
    /// run's target directory, beside this executable's profile. NEGATIVE CONTROL: with neither, a
    /// child refuses and names the variable to set, rather than run some other build's binary.
    #[test]
    fn a_package_binary_is_the_runs_own() {
        const NAME: &str = "build_paths::tests::a_package_binary_is_the_runs_own";
        if answer_if_asked() {
            return;
        }
        let scratch = crate::tempdir().expect("scratch");
        let named = scratch.path().join("named-probe");
        let (ok, text) = ask_child(NAME, "bin", &[("CARGO_BIN_EXE_probe", &named)], &[]);
        assert!(ok, "{text}");
        assert_eq!(answer(&text), named, "{text}");

        let target = scratch.path().join("target");
        let profile = profile_folder().expect("a test executable in Cargo's build layout");
        let profile = profile.file_name().expect("a profile name");
        let laid = target
            .join(profile)
            .join(format!("probe{}", std::env::consts::EXE_SUFFIX));
        let env = [("CARGO_TARGET_DIR", target.as_path())];
        let unset = [
            "CARGO_BIN_EXE_probe",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_TARGET",
        ];
        let (ok, text) = ask_child(NAME, "bin", &env, &unset);
        assert!(!ok, "no binary at the layout's path must refuse: {text}");
        assert!(text.contains("CARGO_BIN_EXE_probe is unset"), "{text}");
        std::fs::create_dir_all(laid.parent().expect("profile")).expect("mkdir");
        std::fs::write(&laid, b"").expect("the laid-out binary");
        let (ok, text) = ask_child(NAME, "bin", &env, &unset);
        assert!(ok, "{text}");
        assert_eq!(answer(&text), laid, "{text}");
    }

    /// The tests' scratch directory is the `tmp` beside this executable's profile folder, where
    /// Cargo makes it, or the one the run names. NEGATIVE CONTROL: a child handed another
    /// directory reads and creates that one.
    #[test]
    fn the_tests_scratch_directory_sits_in_this_builds_directory() {
        const NAME: &str =
            "build_paths::tests::the_tests_scratch_directory_sits_in_this_builds_directory";
        if answer_if_asked() {
            return;
        }
        let profile = profile_folder().expect("a test executable in Cargo's build layout");
        let build = profile.parent().expect("build directory");
        let (ok, text) = ask_child(NAME, "tmp", &[], &["CARGO_TARGET_TMPDIR"]);
        assert!(ok, "{text}");
        assert_eq!(answer(&text), build.join("tmp"), "{text}");
        let scratch = crate::tempdir().expect("scratch");
        let named = scratch.path().join("named-tmp");
        let (ok, text) = ask_child(NAME, "tmp", &[("CARGO_TARGET_TMPDIR", &named)], &[]);
        assert!(ok, "{text}");
        assert_eq!(answer(&text), named, "{text}");
        assert!(named.is_dir(), "the named directory is created");
    }

    /// A profile's binaries are copied to its folder in the target directory, under the triple of
    /// a build for one. NEGATIVE CONTROL: drop the triple, and the Windows lane's
    /// `CARGO_BUILD_TARGET` build names a folder its binaries are not in.
    #[test]
    fn a_build_for_a_triple_copies_its_binaries_under_that_triple() {
        let profile = Path::new("/build/x86_64-pc-windows-msvc/debug");
        assert_eq!(
            uplifted(
                Path::new("/lane/target"),
                Some("x86_64-pc-windows-msvc".into()),
                profile
            ),
            Path::new("/lane/target/x86_64-pc-windows-msvc/debug")
        );
        assert_eq!(
            uplifted(Path::new("/w/target"), None, Path::new("/build/release")),
            Path::new("/w/target/release")
        );
    }

    /// Both of Cargo's layouts name the profile folder; anything else names none. NEGATIVE
    /// CONTROL: an `out` folder outside a `build/<package>/<hash>` and a copy beside a profile
    /// folder are refused.
    #[test]
    fn the_profile_folder_is_read_from_either_cargo_layout() {
        let p = |s: &str| profile_of(Path::new(s));
        assert_eq!(
            p("/b/debug/deps/t-1").as_deref(),
            Some(Path::new("/b/debug"))
        );
        assert_eq!(
            p("/b/x86_64-apple-darwin/debug/build/pkg/0123/out/t-0123").as_deref(),
            Some(Path::new("/b/x86_64-apple-darwin/debug"))
        );
        assert_eq!(p("/b/debug/build/pkg/0123/t-0123"), None);
        assert_eq!(p("/b/debug/other/pkg/0123/out/t-0123"), None);
        assert_eq!(p("/b/debug/t-1"), None);
    }

    /// The workspace is the nearest manifest with a `[workspace]` table at or above a package.
    #[test]
    fn the_workspace_root_is_the_nearest_workspace_manifest() {
        let scratch = crate::tempdir().expect("scratch");
        let root = scratch.path();
        let package = root.join("crates/p");
        std::fs::create_dir_all(&package).expect("mkdir");
        std::fs::write(package.join("Cargo.toml"), "[package]\nname = \"p\"\n").expect("p");
        assert_eq!(workspace_root(&package), None);
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .expect("ws");
        assert_eq!(workspace_root(&package).as_deref(), Some(root));
    }
}
