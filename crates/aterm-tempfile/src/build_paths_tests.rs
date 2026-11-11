// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
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
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
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
