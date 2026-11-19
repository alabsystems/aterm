// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Actual SDK discovery through native admission, independent of the managed shell shim.

use super::{Scratch, finish, output_while_working, started};
use aterm_capacity::{Admit, Ledger, Live, Need, Pace};
use aterm_spec::wait::{MUST_HAPPEN, until_within};
use aterm_uds::spawnfd::{self, Stdio};
use std::path::{Path, PathBuf};

const ACTOR: &str = "build_hop::sdk_root::sdkroot_front_actor_if_requested";
const CASE: &str = "ATPKG_SDKROOT_NATIVE_CASE";
const FRONT: &str = "ATPKG_SDKROOT_NATIVE_FRONT";
const TARGET: &str = "ATPKG_SDKROOT_NATIVE_TARGET";

/// Ask the OS independently under the same empty environment as Scratch's front.
pub(super) fn expected(scratch: &Scratch) -> String {
    // Refuse absent developer tools before xcrun can offer their installer.
    let mut select = spawnfd::command("/usr/bin/xcode-select");
    select
        .arg("-p")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", scratch.home())
        .current_dir(scratch.root());
    let selected = output_while_working(&mut select, "the selected Apple developer tools");
    assert!(selected.status.success(), "{selected:?}");
    let mut query = spawnfd::command("/usr/bin/xcrun");
    query
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", scratch.home())
        .current_dir(scratch.root());
    let output = output_while_working(&mut query, "the actual macOS SDK");
    assert!(output.status.success(), "{output:?}");
    let root = String::from_utf8(output.stdout).expect("the SDK path is UTF-8");
    let root = root.trim_end_matches(['\r', '\n']).to_owned();
    assert!(Path::new(&root).is_absolute() && Path::new(&root).is_dir());
    root
}

/// Reuse the genuine-front fixture's isolated home, ledger and helper policy.
fn isolated(scratch: &Scratch, program: &Path, args: &[std::ffi::OsString]) -> spawnfd::Command {
    let policy = scratch.front_command(None, &[], &[]);
    let mut command = spawnfd::command(program);
    command.args(args).env_clear();
    for (key, value) in policy.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        }
    }
    command
        .env_remove("SDKROOT")
        .current_dir(scratch.root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[test]
fn sdkroot_front_actor_if_requested() {
    let Some(case) = std::env::var_os(CASE) else {
        return;
    };
    let target = std::env::var_os(TARGET).expect("the actual target");
    if case == "reading" {
        // Count our own inert reader, never a kernel read. The real native hop
        // must take run_as_child rather than exec across this live guard.
        let reading = aterm_sysprobe::exec_gate::reading();
        let outcome =
            atpkg::build_admit::run_hidden(&[target, "--unverified".into(), "run".into()]);
        drop(reading);
        assert_eq!(outcome, std::process::ExitCode::SUCCESS);
        return;
    }
    assert_eq!(case, "own");
    let ledger = Ledger::at(PathBuf::from(
        std::env::var_os(atpkg::build_admit::LEDGER_ENV).expect("private ledger"),
    ));
    let claim = aterm_capacity::admit_with(
        &ledger,
        &Live,
        Need::new("native SDKROOT own-claim fixture").mem(1),
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .expect("actual own claim");
    let mut command =
        spawnfd::transparent(std::env::var_os(FRONT).expect("genuine front")).into_command();
    command
        .arg("__build_admit")
        .arg(target)
        .args(["--unverified", "check"]);
    command
        .inherit_exec_guards(vec![claim.descriptor().expect("original claim guard")])
        .expect("retain the actual guard across native exec");
    claim.hand_off();
    panic!(
        "the genuine own-claim front did not exec: {}",
        command.exec()
    );
}

#[test]
fn sdkroot_reaches_covered_own_and_unclaimed_child_execution() {
    let scratch = Scratch::new();
    let sdk = expected(&scratch);
    let cover = aterm_capacity::admit_with(
        &Ledger::at(scratch.ledger()),
        &Live,
        Need::new("native SDKROOT covering fixture").mem(1).covers(),
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .expect("actual covering parent claim");
    let child = scratch.front(&["--unverified", "check"], &[]);
    let seen = scratch.seen(child.id());
    assert_eq!(seen.sdkroot, sdk);
    assert_eq!(seen.pid, child.id(), "coverage still execs the target");
    assert!(seen.held.is_none(), "coverage creates no child claim");
    let rows = scratch.listed();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pid, std::process::id());
    let (status, stderr) = finish(child);
    assert!(status.success(), "{status}: {stderr}");
    drop(cover);
    assert!(scratch.listed().is_empty());

    for case in ["own", "reading"] {
        let scratch = Scratch::new();
        let args = ["--exact", ACTOR, "--nocapture", "--test-threads=1"].map(Into::into);
        let mut command = isolated(&scratch, &std::env::current_exe().unwrap(), &args);
        command
            .env(CASE, case)
            .env(FRONT, aterm_tempfile::bin_exe("atpkg"))
            .env(TARGET, scratch.targo());
        let child = started(&mut command);
        let seen = scratch.seen(child.id());
        assert_eq!(seen.sdkroot, sdk, "{case}: {seen:?}");
        assert_eq!(seen.cover, "unset", "{case}: no cover marker was invented");
        if case == "own" {
            assert_eq!(seen.pid, child.id(), "own-claim still execs the target");
            assert!(seen.held.is_some(), "the actual own claim stays held");
            assert_eq!(scratch.listed().len(), 1, "no second claim");
        } else {
            assert_eq!(seen.ppid, child.id(), "the reader forces a real child");
            assert_ne!(seen.pid, child.id());
            assert!(seen.held.is_none());
            assert!(scratch.listed().is_empty());
        }
        let (status, stderr) = finish(child);
        assert!(status.success(), "{case}: {status}: {stderr}");
        if case == "own" {
            assert!(
                stderr.contains("this process holds a claim of its own"),
                "{stderr}"
            );
        } else {
            assert!(
                stderr.contains("build runs as this front's child"),
                "{stderr}"
            );
        }
        until_within(MUST_HAPPEN, "the actual fixture claim retires", || {
            scratch.listed().is_empty().then_some(())
        });
    }
}

const MANIFEST: &str =
    "[package]\nname='native_sdkroot_probe'\nversion='0.0.0'\nedition='2024'\n[workspace]\n";
// Compilation tracks build.rs; SDKROOT is the script's declared runtime dependency.
// Its link arguments make the program's link write a map file at LINK_MAP, which the
// test writes in place of `@LINK_MAP@`.
const BUILD: &str = r#"fn main() {
    let sdk = std::env::var("SDKROOT").expect("native admission supplies SDKROOT");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out.join("sdkroot-observed"), sdk.as_bytes()).unwrap();
    println!("cargo:rustc-env=SDKROOT_PROBE={sdk}");
    println!("cargo:rerun-if-env-changed=SDKROOT");
    for arg in ["-Xlinker", "-map", "-Xlinker", LINK_MAP] {
        println!("cargo:rustc-link-arg-bins={arg}");
    }
}
const LINK_MAP: &str = @LINK_MAP@;
"#;
/// Both linked units link with this absolute system tool, so the compiler's tool
/// lookup observes `/usr/bin/cc` and its ancestors alone. A bare `cc` is looked up on
/// PATH, and trustc 27009 records no reusable tool inputs for a link during which an
/// entry was made, removed or renamed in an ancestor directory of a PATH candidate
/// (`cc` in each PATH directory, present or not). The compiler puts its own
/// `lib/rustlib/<host>/bin` on that PATH, so the tool store's ancestors are among
/// those directories (ATERM-KI-1117-3471).
const NATIVE_LINKER: &str = "/usr/bin/cc";
const PROGRAM: &str = r#"fn main() {
    assert_eq!(env!("SDKROOT_PROBE"), std::env::args().nth(1).unwrap());
    println!("native SDKROOT fixture executed");
}
"#;

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
}

fn stamp(path: &Path) -> Stamp {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::metadata(path).expect("actual output metadata");
    Stamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
    }
}

/// The linked units whose compiler commands this build's verbose log shows, each
/// checked to name [`NATIVE_LINKER`].
fn dispatched(stderr: &str) -> Vec<&'static str> {
    let mut units = Vec::new();
    for line in stderr
        .lines()
        .filter(|line| line.trim_start().starts_with("Running `"))
    {
        for name in ["build_script_build", "native_sdkroot_probe"] {
            if line.contains(&format!("--crate-name {name} ")) {
                assert!(
                    line.contains(&format!(" -C linker={NATIVE_LINKER} ")),
                    "{name} must link with {NATIVE_LINKER}: {line}"
                );
                units.push(name);
            }
        }
    }
    units.sort_unstable();
    units
}

/// Use the physical Targo that compiled the suite, checked against this source's
/// lock. A managed shim would itself discover SDKROOT and mask this regression.
fn selected_raw_targo() -> PathBuf {
    let found = atpkg::trust_lock::find(&aterm_tempfile::manifest_dir()).expect("source lock");
    let lock = found.lock.expect("valid source lock");
    let targo = PathBuf::from(std::env::var_os("CARGO").expect("the suite's actual Targo"))
        .canonicalize()
        .expect("physical Targo");
    let trustc = targo.parent().unwrap().join("trustc");
    assert!(
        trustc.is_file(),
        "a raw compiler bin, not the managed shim: {targo:?}"
    );
    assert_eq!(
        atpkg::trust_lock::compiler_commit_of(&trustc).expect("actual compiler identity"),
        lock.compiler_commit,
        "the fixture must use the source-selected compiler"
    );
    targo
}

#[test]
fn a_direct_native_hop_supplies_sdkroot_and_reuses_linked_units() {
    let scratch = Scratch::new();
    let sdk = expected(&scratch);
    let targo = selected_raw_targo();
    let project = scratch.root().join("project");
    let evidence = scratch.root().join("evidence");
    // A PATH directory with no `cc`. The program's link makes its map file in this
    // directory's parent, an ancestor of a PATH candidate, as aterm's state writers
    // make and remove files in the tool store's ancestors at any moment, during links too.
    let namespace = scratch.root().join("linker-namespace");
    let link_map = namespace.join("native_sdkroot_probe.map");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir(&evidence).unwrap();
    std::fs::create_dir_all(namespace.join("bin")).unwrap();
    std::fs::write(project.join("Cargo.toml"), MANIFEST).unwrap();
    let link_map_literal = format!("{:?}", link_map.to_str().expect("a UTF-8 scratch path"));
    std::fs::write(
        project.join("build.rs"),
        BUILD.replace("@LINK_MAP@", &link_map_literal),
    )
    .unwrap();
    std::fs::write(project.join("src/main.rs"), PROGRAM).unwrap();
    let args = vec![
        "__build_admit".into(),
        targo.as_os_str().to_owned(),
        "--unverified".into(),
        "build".into(),
        "--offline".into(),
        "--verbose".into(),
        "--target-dir".into(),
        scratch.root().join("target.noindex").into_os_string(),
        "--config".into(),
        format!(
            "build.build-dir='{}'",
            scratch.root().join("build.noindex").display()
        )
        .into(),
        "--config".into(),
        format!("target.'cfg(target_os = \"macos\")'.linker=\"{NATIVE_LINKER}\"").into(),
        "--message-format=json".into(),
    ];
    let path = std::env::join_paths([
        namespace.join("bin").as_path(),
        targo.parent().unwrap(),
        Path::new("/usr/bin"),
        Path::new("/bin"),
    ])
    .unwrap();
    let mut previous = None;
    for (second, label) in [(false, "first"), (true, "second")] {
        let namespace_before = stamp(&namespace);
        let mut command = isolated(&scratch, &aterm_tempfile::bin_exe("atpkg"), &args);
        command
            .env("PATH", &path)
            .env("CARGO_LOG", "targo_core::compiler::fingerprint=debug")
            .current_dir(&project);
        let output = output_while_working(&mut command, "native hop's linked Rust fixture");
        std::fs::write(evidence.join(format!("{label}.stdout")), &output.stdout).unwrap();
        std::fs::write(evidence.join(format!("{label}.stderr")), &output.stderr).unwrap();
        std::fs::write(
            evidence.join(format!("{label}.status")),
            format!("{}\n", output.status),
        )
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        if !second {
            assert_eq!(
                dispatched(&String::from_utf8_lossy(&output.stderr)),
                ["build_script_build", "native_sdkroot_probe"],
                "the first build dispatches both linked units"
            );
            // Nothing but the program's link writes in this directory.
            assert!(link_map.is_file(), "the program's link wrote {link_map:?}");
            assert_ne!(
                stamp(&namespace),
                namespace_before,
                "the first build's link changed {namespace:?}"
            );
        }
        let rows = String::from_utf8(output.stdout).unwrap();
        let rows = rows
            .lines()
            .map(|line| aterm_json::from_str::<aterm_json::Value>(line).expect("actual Cargo JSON"))
            .collect::<Vec<_>>();
        let artifacts = rows
            .iter()
            .filter(|row| row["reason"].as_str() == Some("compiler-artifact"))
            .collect::<Vec<_>>();
        assert_eq!(
            artifacts.len(),
            2,
            "build script and linked program: {rows:?}"
        );
        for artifact in &artifacts {
            let fresh = artifact["fresh"]
                .as_bool()
                .expect("actual artifact freshness");
            if second {
                assert!(
                    fresh,
                    "{artifact:?}\n{}\nevidence: {}",
                    String::from_utf8_lossy(&output.stderr),
                    evidence.display()
                );
            }
        }
        let executable = artifacts
            .iter()
            .find(|row| row["target"]["name"].as_str() == Some("native_sdkroot_probe"))
            .and_then(|row| row["executable"].as_str())
            .map(PathBuf::from)
            .expect("actual linked program artifact");
        let out = rows
            .iter()
            .find(|row| row["reason"].as_str() == Some("build-script-executed"))
            .and_then(|row| row["out_dir"].as_str())
            .map(PathBuf::from)
            .expect("the actual build script output directory")
            .join("sdkroot-observed");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), sdk);
        let observed = (
            executable.clone(),
            stamp(&executable),
            out.clone(),
            stamp(&out),
        );
        if let Some(previous) = &previous {
            assert_eq!(
                &observed, previous,
                "second run actually reuses both linked and build-script output"
            );
        }
        previous = Some(observed);
        let mut program = spawnfd::command(&executable);
        program.arg(&sdk);
        let ran = output_while_working(&mut program, "the emitted linked SDKROOT fixture");
        assert!(ran.status.success(), "{ran:?}");
        assert_eq!(ran.stdout, b"native SDKROOT fixture executed\n");
        until_within(
            MUST_HAPPEN,
            "the linked fixture's native claim retires",
            || scratch.listed().is_empty().then_some(()),
        );
    }
}
