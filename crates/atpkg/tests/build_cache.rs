// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `atpkg::build_cache` under an isolated temp `HOME`.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A wait for something that must happen, the first exec of the build script cargo just
/// linked among it: the first-exec hang detector, not a budget.
const HANG: Duration = aterm_uds::deadline::FIRST_EXEC_HANG;

const BUILD_RS: &str = r#"
fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    let shim = std::env::var("BC_SHIM").unwrap();
    let cargo = std::env::var("CARGO").unwrap();
    for (name, program, args) in [
        ("cargo", cargo.as_str(), vec!["--unverified", "build", "-p", "inner"]),
        ("shim", "/bin/sh", vec![shim.as_str(), "--unverified", "build", "-p", "inner"]),
    ] {
        let target = format!("{out}/nested-{name}");
        let status = std::process::Command::new(program)
            .args(&args)
            .env("CARGO_TARGET_DIR", &target)
            .status()
            .unwrap();
        assert!(status.success(), "nested {name}: {status}");
        assert!(std::path::Path::new(&target).join("debug/deps").is_dir(), "nested {name} kept to its own target dir");
    }
}
"#;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn a_build_script_building_its_own_workspace_finishes_under_the_shim() {
    let targo = Path::new(env!("CARGO"));
    if targo.file_name().is_none_or(|n| n != "targo") {
        aterm_spec::host::could_not_run(format!(
            "needs Trust's targo, and this suite runs under {}",
            targo.display()
        ));
    }
    // Cargo links and runs outer's build script under it: kept unless that script exited on
    // its own.
    let mut tmp = aterm_uds::deadline::KeptUntilRan::new(
        aterm_tempfile::Builder::new()
            .prefix("atpkg-build-cache-nested-")
            .tempdir()
            .unwrap()
            .keep(),
    );
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(home.join("Library/Caches")).unwrap();
    let ws = home.join("ws");
    write(
        &ws.join("Cargo.toml"),
        "[workspace]\nmembers = [\"outer\", \"inner\"]\nresolver = \"2\"\n",
    );
    write(
        &ws.join("inner/Cargo.toml"),
        "[package]\nname = \"inner\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(&ws.join("inner/src/lib.rs"), "pub fn f() -> u32 { 7 }\n");
    write(
        &ws.join("outer/Cargo.toml"),
        "[package]\nname = \"outer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(&ws.join("outer/src/main.rs"), "fn main() {}\n");
    write(&ws.join("outer/build.rs"), BUILD_RS);
    let shim = root.join("targo-shim");
    write(
        &shim,
        &format!(
            "#!/bin/sh\n{}exec '{}' \"$@\"\n",
            atpkg::build_cache::shim_block("targo").unwrap(),
            targo.display()
        ),
    );

    let log = root.join("build.log");
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .arg(&shim)
        .args(["--unverified", "build", "-q"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("PWD", &ws)
        .env("BC_SHIM", &shim)
        .current_dir(&ws)
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > HANG {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the build did not finish in {HANG:?}: the nested cargo waits on a lock");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        status.success(),
        "{status}: {}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    // Cargo succeeds only once the build script exited 0 on its own.
    tmp.ran();

    let cache: PathBuf = atpkg::build_cache::build_dir(&atpkg::build_cache::cache_root(&home), &ws);
    assert!(
        cache.join("debug/deps").is_dir(),
        "intermediates in the cache: {}",
        cache.display()
    );
    assert!(
        ws.join("target/debug/outer").is_file(),
        "the binary in target/"
    );
    assert!(
        !ws.join("target/debug/deps").exists(),
        "no intermediates in target/"
    );
}

fn fake_cargo(dir: &Path, release: &str) -> PathBuf {
    let path = dir.join("cargo");
    let (release, trust) = match release {
        "trust" => ("1.99.0-dev", " 'trust: 0.1.0'"),
        other => (other, ""),
    };
    write(
        &path,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = -vV ] || [ \"$2\" = -vV ]; then printf '%s\\n' 'cargo {release} (0123456789 2026-07-14)' \
             'release: {release}' 'commit-hash: 0123456789abcdef' 'host: aarch64-apple-darwin'{trust}; exit 0; fi\n\
             if [ -n \"$ATPKG_TEST_PID_TO\" ]; then echo $$ > \"$ATPKG_TEST_PID_TO\"; fi\n\
             printf '%s\\n' \"$@\"\n"
        ),
    );
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let warmed = aterm_uds::spawnfd::command(&path)
        .arg("-vV")
        .output()
        .unwrap();
    assert!(warmed.status.success(), "{warmed:?}");
    path
}

#[test]
fn cargo_in_a_session_gets_the_cache_only_where_it_builds_and_reads_it() {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("atpkg-build-cache-reroute-")
        .tempdir()
        .unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(home.join("Library/Caches")).unwrap();
    let ws = home.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let config = atpkg::build_cache::config_value(&atpkg::build_cache::cache_root(&home)).unwrap();
    let command = |release: &str, args: &[&str], env: &[(&str, &str)]| {
        let bin = root.join(format!("bin-{release}"));
        fake_cargo(&bin, release);
        let mut cmd = aterm_uds::spawnfd::command(env!("CARGO_BIN_EXE_atpkg"));
        cmd.arg("__reroute")
            .arg("cargo")
            .args(args)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env(atpkg::build_admit::LEDGER_ENV, root.join("ledger"))
            .current_dir(&ws);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    };
    let run = |release: &str, args: &[&str], env: &[(&str, &str)]| {
        let out = command(release, args, env).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(
        run("1.97.1", &["build", "-q"], &[]),
        format!("--config\n{config}\nbuild\n-q\n")
    );
    // A rerouted build becomes cargo by exec, in the process its caller started, and claims
    // nothing (BUILD-ADMIT): a front that claimed and stayed as cargo's parent would be what a
    // tab's readers name (`program=atpkg`), the naming gate the shim's hop failed.
    let pid_to = root.join("cargo-pid");
    let pid_env = pid_to.to_string_lossy().into_owned();
    let child = command(
        "1.97.1",
        &["build", "-q"],
        &[("ATPKG_TEST_PID_TO", &pid_env)],
    )
    .stdout(aterm_uds::spawnfd::Stdio::null())
    .spawn()
    .unwrap();
    let started = child.id();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(&pid_to).unwrap().trim(),
        started.to_string(),
        "cargo ran in the rerouting process itself"
    );
    assert!(
        !root.join("ledger").exists(),
        "a rerouted build claims nothing, in the scratch ledger its test names or this user's"
    );
    assert_eq!(
        run("1.97.1", &["+1.97.1", "test"], &[]),
        format!("+1.97.1\n--config\n{config}\ntest\n")
    );
    assert_eq!(run("1.97.1", &["metadata"], &[]), "metadata\n");
    assert_eq!(
        run("1.97.1", &["build"], &[("CARGO_TARGET_DIR", "/t")]),
        "--config\nbuild.build-dir='/t/build.noindex'\nbuild\n"
    );
    assert_eq!(
        run("1.97.1", &["build"], &[("CARGO_BUILD_BUILD_DIR", "/b")]),
        "build\n"
    );
    assert_eq!(run("1.85.0", &["build"], &[]), "build\n");
    assert_eq!(run("trust", &["check"], &[]), "check\n");
    assert_eq!(
        run("trust", &["build", "--unverified"], &[]),
        format!("--config\n{config}\nbuild\n--unverified\n")
    );
}
