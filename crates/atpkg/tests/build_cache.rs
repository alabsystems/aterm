// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The build cache's two entry points, run for real (`atpkg::build_cache`), under a temp
//! `HOME`.
//!
//! A build script that builds its own workspace (by `$CARGO`, and by `targo` through the
//! shim) finishes under the shim: the setting is on the command line, not inherited. With
//! it exported as `CARGO_BUILD_BUILD_DIR` the nested build waited on the outer build's
//! lock forever. Needs real `targo` (the one building this test); no network.
//!
//! `atpkg __reroute cargo …` hands a cargo the setting only where the command builds and
//! that cargo reads it.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A wait for something that must happen: a hang detector, not a budget.
const HANG: Duration = Duration::from_secs(60);

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
        eprintln!(
            "not run: needs Trust's targo, and this suite runs under {}",
            targo.display()
        );
        return;
    }
    let tmp = aterm_tempfile::Builder::new()
        .prefix("atpkg-build-cache-nested-")
        .tempdir()
        .unwrap();
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

/// An upstream `cargo` on PATH that answers `-vV` (after a rustup `+<toolchain>` too) as
/// `release` (`trust` adds Trust's `trust:` line, on 1.99.0-dev) and prints its arguments
/// otherwise; run once here, so the first exec's assessment is paid outside any bound.
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
    let run = |release: &str, args: &[&str], env: &[(&str, &str)]| {
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
            .current_dir(&ws);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(
        run("1.97.1", &["build", "-q"], &[]),
        format!("--config\n{config}\nbuild\n-q\n")
    );
    assert_eq!(
        run("1.97.1", &["+1.97.1", "test"], &[]),
        format!("+1.97.1\n--config\n{config}\ntest\n")
    );
    assert_eq!(run("1.97.1", &["metadata"], &[]), "metadata\n");
    assert_eq!(
        run("1.97.1", &["build"], &[("CARGO_TARGET_DIR", "/t")]),
        "build\n"
    );
    assert_eq!(run("1.85.0", &["build"], &[]), "build\n");
    // Trust's own cargo is targo: its verified lane as typed, `--unverified` gets it.
    assert_eq!(run("trust", &["check"], &[]), "check\n");
    assert_eq!(
        run("trust", &["build", "--unverified"], &[]),
        format!("--config\n{config}\nbuild\n--unverified\n")
    );
}
