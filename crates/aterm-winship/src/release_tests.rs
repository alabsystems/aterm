// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::handoff::tests::{
    BUILD, COMMIT, Change, PINS, STOCK_VV, TRUST_COMMIT, VERSION, trust_vv,
};

fn opts() -> Options {
    Options {
        version: VERSION.to_string(),
        build: BUILD,
        commit: COMMIT.to_string(),
        out: PathBuf::from("carry"),
        repo: None,
    }
}

fn toolchain(lane: Lane, version: String) -> Toolchain {
    Toolchain {
        lane,
        compiler: PathBuf::from("rustc.exe"),
        driver: PathBuf::from("cargo.exe"),
        library: PathBuf::from("rustc_driver-0.dll"),
        version,
    }
}

#[test]
fn options_name_a_release() {
    assert_eq!(opts().check(), Ok(()));
    for version in ["", "1", "0.200", "1.20261112.3"] {
        assert_eq!(is_version(version), version.contains('.'), "{version:?}");
    }
    for version in ["v0.200.0", "0.200.0-rc1", "0..1", "0.200.0+gabc"] {
        assert!(!is_version(version), "{version:?}");
    }
    let bad: [Change<Options>; 4] = [
        Box::new(|o| o.version = "0.200.0+g1a37de31a340".to_string()),
        Box::new(|o| o.build = 0),
        Box::new(|o| o.commit = COMMIT[..12].to_string()),
        Box::new(|o| o.commit = COMMIT.to_uppercase()),
    ];
    for change in bad {
        let mut o = opts();
        change(&mut o);
        assert!(o.check().is_err(), "{o:?} was taken");
    }
}

#[test]
fn the_release_identity_is_the_linux_workers() {
    // crates/aterm-release/src/buildplan.rs `release_env`, the rows a build off macOS gets
    // (aterm_stamp::input's names).
    assert_eq!(
        release_env(COMMIT, &COMMIT[..12], BUILD),
        vec![
            ("ATERM_STAMP", "commit".to_string()),
            ("SOURCE_DATE_EPOCH", BUILD.to_string()),
            ("ATERM_BUILD_GIT_COMMIT", COMMIT[..12].to_string()),
            ("ATERM_BUILD_GIT_COMMIT_FULL", COMMIT.to_string()),
            ("ATERM_BUILD_DEV_COMMITS", "0".to_string()),
            ("ATERM_RELEASE_BUILD", "1".to_string()),
        ]
    );
}

#[test]
fn trust_builds_only_when_it_is_the_locks_build() {
    let pinned = Ok(toolchain(Lane::Trust, trust_vv(TRUST_COMMIT)));
    assert_eq!(choose(&pinned, &PINS), (Lane::Trust, None));
    let unpinned = Ok(toolchain(
        Lane::Trust,
        trust_vv("dbc6adef214925852160bf2113a24fdfd18832e9"),
    ));
    let (lane, why) = choose(&unpinned, &PINS);
    assert_eq!(lane, Lane::Stock);
    assert!(why.is_some_and(|w| w.contains("trust.lock")));
    let none = Err("no targo.exe on PATH".to_string());
    assert_eq!(
        choose(&none, &PINS),
        (Lane::Stock, Some("no targo.exe on PATH".to_string()))
    );
    let renamed_stock = Ok(toolchain(Lane::Trust, STOCK_VV.to_string()));
    assert_eq!(choose(&renamed_stock, &PINS).0, Lane::Stock);
}

#[test]
fn build_json_must_be_the_release_build_in_its_lane() {
    let good = Build {
        version: VERSION.to_string(),
        commit: COMMIT[..12].to_string(),
        commit_full: COMMIT.to_string(),
        build_number: BUILD,
        arch: "x64".to_string(),
        toolchain: "stock 1.97.1".to_string(),
        dirty: false,
        built_at: "2026-11-12T10:00:00Z".to_string(),
    };
    assert_eq!(check_record(&good, &opts(), Lane::Stock), Ok(()));
    assert!(check_record(&good, &opts(), Lane::Trust).is_err());
    let bad: [Change<Build>; 5] = [
        Box::new(|b| b.version = "0.199.0".to_string()),
        Box::new(|b| b.commit_full = "0".repeat(40)),
        Box::new(|b| b.build_number = BUILD - 1),
        Box::new(|b| b.dirty = true),
        Box::new(|b| b.toolchain = "stockade 1.97.1".to_string()),
    ];
    for change in bad {
        let mut b = good.clone();
        change(&mut b);
        assert!(check_record(&b, &opts(), Lane::Stock).is_err(), "{b:?}");
    }
}

#[test]
fn a_toolchain_is_its_bins_one_library() {
    let bin = aterm_tempfile::tempdir().expect("tempdir");
    for (name, bytes) in [("rustc.exe", &b"compiler"[..]), ("cargo.exe", b"driver")] {
        std::fs::write(bin.path().join(name), bytes).expect("write");
    }
    let found = |vv: &str| {
        Toolchain::in_bin(
            Lane::Stock,
            bin.path(),
            "rustc.exe",
            "cargo.exe",
            vv.to_string(),
        )
    };
    assert!(found(STOCK_VV).is_err(), "a bin with no library");
    std::fs::write(
        bin.path().join("rustc_driver-e41b336de1bfe42c.dll"),
        b"library",
    )
    .expect("write");
    let toolchain = found(STOCK_VV).expect("one library");
    let digest = |b: &[u8]| aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(b));
    assert_eq!(
        toolchain.digests(),
        Ok([digest(b"compiler"), digest(b"driver"), digest(b"library")])
    );
    std::fs::write(
        bin.path().join("rustc_driver-0123456789abcdef.dll"),
        b"other",
    )
    .expect("write");
    assert!(found(STOCK_VV).is_err(), "a bin with two libraries");
    assert!(
        Toolchain::in_bin(
            Lane::Trust,
            bin.path(),
            "trustc.exe",
            "targo.exe",
            STOCK_VV.to_string()
        )
        .is_err(),
        "a bin without the named files"
    );
}

#[test]
fn the_out_folder_is_absent_or_empty() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let absent = root.path().join("carry");
    let made = fresh_out(&absent).expect("an absent folder is made");
    assert!(made.is_dir() && made.is_absolute());
    assert!(fresh_out(&absent).is_ok(), "an empty folder is taken");
    std::fs::write(absent.join("old.zip"), b"old").expect("write");
    let err = fresh_out(&absent).expect_err("a folder that holds a file was taken");
    assert!(err.contains("not empty"), "{err}");
}

fn vars(pairs: &[(&str, &Path)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.display().to_string()))
        .collect()
}

fn touch(file: &Path) {
    std::fs::create_dir_all(file.parent().expect("a parent")).expect("dirs");
    std::fs::write(file, b"[build]\nrustc-wrapper = \"sccache\"\n").expect("write");
}

#[test]
fn cargo_config_above_the_checkout_or_in_cargo_home_is_found() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let t = scratch.path();
    let root = t.join("a").join("b").join("repo");
    std::fs::create_dir_all(&root).expect("root");
    // Under the scratch folder only: a folder above it is this machine's, not the test's.
    let found = |env: &[(String, String)]| -> Vec<PathBuf> {
        foreign_cargo_config(env, &root)
            .into_iter()
            .filter(|file| file.starts_with(t))
            .collect()
    };
    let home = t.join("home");
    let env = vars(&[("CARGO_HOME", home.as_path())]);
    assert_eq!(found(&env), Vec::<PathBuf>::new(), "none yet");
    // The checkout's own is its commit's, and another name is no configuration.
    touch(&root.join(".cargo").join("config.toml"));
    touch(&t.join("a").join(".cargo").join("credentials.toml"));
    assert_eq!(found(&env), Vec::<PathBuf>::new());
    let legacy = t.join("a").join("b").join(".cargo").join("config");
    let above = t.join("a").join(".cargo").join("config.toml");
    let in_home = home.join("config.toml");
    for file in [&legacy, &above, &in_home] {
        touch(file);
    }
    assert_eq!(found(&env), vec![legacy.clone(), above.clone(), in_home]);
    // With no CARGO_HOME, cargo's home is the profile's .cargo.
    let profile = t.join("profile");
    let in_profile = profile.join(".cargo").join("config");
    touch(&in_profile);
    assert_eq!(
        found(&vars(&[("USERPROFILE", profile.as_path())])),
        vec![legacy, above, in_profile]
    );
}

#[test]
fn variables_that_steer_cargo_are_named() {
    let env: Vec<(String, String)> = [
        ("CARGO_BUILD_RUSTC_WRAPPER", "sccache"),
        (
            "cargo_target_x86_64_pc_windows_msvc_linker",
            "C:\\evil\\link.exe",
        ),
        ("CARGO_PROFILE_RELEASE_LTO", "off"),
        ("CARGO_SOURCE_CRATES_IO_REPLACE_WITH", "mirror"),
        ("RUSTFLAGS", "-Clink-arg=x"),
        ("CARGO_BUILD_JOBS", ""),
        ("CARGO_TARGET_DIR", "C:\\t"),
        ("CARGO_HOME", "C:\\c"),
        ("CARGO_TERM_COLOR", "never"),
        ("CARGO_INCREMENTAL", "0"),
        ("CARGO_PKG_NAME", "aterm-winship"),
        ("RUSTC_WRAPPER", "sccache"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect();
    assert_eq!(
        steering_env(&env),
        vec![
            "CARGO_BUILD_RUSTC_WRAPPER",
            "CARGO_PROFILE_RELEASE_LTO",
            "CARGO_SOURCE_CRATES_IO_REPLACE_WITH",
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
            "RUSTFLAGS",
        ]
    );
}

#[cfg(windows)]
#[test]
fn a_release_build_refuses_configuration_its_commit_does_not_hold() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let root = scratch.path().join("repo");
    let run = |args: &[&str]| {
        let out = aterm_uds::deadline::output_within(
            aterm_uds::fixture_git::git()
                .arg("-C")
                .arg(&root)
                .args(args),
            ASK,
        )
        .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    std::fs::create_dir_all(&root).expect("root");
    run(&["init", "--quiet"]);
    touch(&root.join(".cargo").join("config.toml"));
    std::fs::write(root.join(".gitignore"), ".cargo/config\n").expect("ignore");
    run(&["add", "."]);
    run(&[
        "-c",
        "user.name=F",
        "-c",
        "user.email=f@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "one",
    ]);
    let env = vars(&[("CARGO_HOME", scratch.path().join("home").as_path())]);
    // What this machine holds above the scratch folder is refused too; the test's own is not.
    let above = foreign_cargo_config(&env, &root);
    let expect = |got: Result<(), String>, ignored: bool| match got {
        Ok(()) => assert!(above.is_empty() && !ignored, "nothing was refused"),
        Err(why) => {
            let own = root
                .join(".cargo")
                .join("config.toml")
                .display()
                .to_string();
            assert!(!why.contains(&own), "the commit's own was refused: {why}");
            let hidden = root.join(".cargo/config").display().to_string();
            assert_eq!(why.contains(&hidden), ignored, "{why}");
            assert!(!why.contains('\n'), "{why:?}");
        }
    };
    expect(check_cargo_inputs(&env, &root), false);
    // An ignored file in the checkout's .cargo, which `git status` does not name.
    touch(&root.join(".cargo").join("config"));
    expect(check_cargo_inputs(&env, &root), true);
    std::fs::remove_file(root.join(".cargo").join("config")).expect("remove");
    // A steering variable.
    let mut steered = env.clone();
    steered.push((
        "CARGO_BUILD_RUSTC".to_string(),
        "C:\\other\\rustc.exe".to_string(),
    ));
    let why = check_cargo_inputs(&steered, &root).expect_err("a steering variable was taken");
    assert!(why.contains("CARGO_BUILD_RUSTC"), "{why}");
}

#[test]
fn build_ps1_is_given_the_hashed_driver_by_path() {
    let toolchain = Toolchain {
        lane: Lane::Stock,
        compiler: PathBuf::from("C:\\rust\\bin\\rustc.exe"),
        driver: PathBuf::from("C:\\rust\\bin\\cargo.exe"),
        library: PathBuf::from("C:\\rust\\bin\\rustc_driver-0.dll"),
        version: STOCK_VV.to_string(),
    };
    let args = build_ps1_args(Path::new("build.ps1"), Path::new("dist"), &toolchain);
    let pair = |flag: &str, value: &str| args.windows(2).any(|w| w[0] == *flag && w[1] == *value);
    assert!(pair("-Toolchain", "stock"), "{args:?}");
    assert!(pair("-Builder", "C:\\rust\\bin\\cargo.exe"), "{args:?}");
    assert!(
        pair("-Out", "dist") && args.iter().any(|a| a == "-Zip"),
        "{args:?}"
    );
}

#[test]
fn a_commit_stamping_another_version_is_refused_in_one_line() {
    let manifest = "[workspace.package]\nversion = \"0.200.0\"\n";
    assert_eq!(check_stamped(manifest, VERSION), Ok(()));
    for (manifest, version) in [(manifest, "0.199.0"), ("[package]\n", VERSION)] {
        let err = check_stamped(manifest, version).expect_err("another version was taken");
        assert!(err.contains(version), "{err}");
        // Every refusal is one line (lib.rs prints it after `aterm-winship: `).
        assert!(!err.contains('\n'), "{err:?}");
    }
}

#[test]
fn the_version_is_read_as_build_ps1_reads_it() {
    let manifest = "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"0.199.0\"\nedition = \"2024\"\n\n[workspace.dependencies]\nfoo = { version = \"1\" }\n";
    assert_eq!(workspace_version(manifest), Some("0.199.0"));
    assert_eq!(
        workspace_version("  version=\"1.2.3\" # x\n"),
        Some("1.2.3")
    );
    assert_eq!(workspace_version("[package]\nname = \"x\"\n"), None);
    assert_eq!(workspace_version("version = \"\"\n"), None);
    // `versions = …` is another key.
    assert_eq!(
        workspace_version("versions = \"1\"\nversion = \"2.0\"\n"),
        Some("2.0")
    );
}
