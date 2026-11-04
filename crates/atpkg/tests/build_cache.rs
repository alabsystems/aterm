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
    if let Ok(ready) = std::env::var("BC_HOP_READY") {
        std::fs::write(&ready, std::process::id().to_string()).unwrap();
        let release = std::env::var("BC_HOP_RELEASE").unwrap();
        let from = std::time::Instant::now();
        while !std::path::Path::new(&release).exists() {
            assert!(from.elapsed() < std::time::Duration::from_secs(60), "hang detector: lock witness");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    for (name, program, args) in [
        ("cargo", cargo.as_str(), vec!["--unverified", "build", "-p", "inner"]),
        ("shim", "/bin/sh", vec![shim.as_str(), "--unverified", "build", "-p", "inner"]),
    ] {
        let target = format!("{out}/nested-{name}");
        let status = std::process::Command::new(program)
            .args(&args)
            .env("CARGO_TARGET_DIR", &target)
            .env("ATERM_ADMIT", "wait")
            .status()
            .unwrap();
        assert!(status.success(), "nested {name}: {status}");
        assert!(std::path::Path::new(&target).join("debug/libinner.rlib").is_file(), "nested {name} published its compiled library in the requested target dir");
    }
}
"#;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn a_build_script_building_its_own_workspace_finishes_under_the_shim() {
    nested_build(false, "build");
}

/// The same through an alias cargo expands to `build`: the shim places it by its `--config`
/// alone, which reaches no build script's own cargo.
#[test]
fn a_build_script_building_its_own_workspace_through_an_alias_finishes_under_the_shim() {
    nested_build(false, "xb");
}

/// A real Cargo, identified by this suite's compile-time compiler path, has its active build
/// lock while its build.rs is stopped at an explicit barrier. The admission hop's covering
/// claim remains held, and nested builds explicitly told to wait still finish under it.
#[test]
fn a_real_cargo_build_script_keeps_its_lock_and_covered_nested_builds_live() {
    nested_build(true, "build");
}

/// Test observation, never admission authority: the actual compiler path baked into this
/// suite, a stable kernel process identity, and a held file that Cargo has open at the barrier.
fn real_cargo_lock(pid: u32, cargo: &Path) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    let Some(identity) = aterm_uds::process::read_identity(raw) else {
        return false;
    };
    let Ok(cargo) = cargo.canonicalize() else {
        return false;
    };
    if aterm_uds::process::read_process_path(raw).and_then(|p| p.canonicalize().ok()) != Some(cargo)
    {
        return false;
    }
    let locked = atpkg::build_admit::possible_build_lock(
        &|| vec![pid],
        &|pid| aterm_uds::holders::open_files(pid).unwrap_or_default(),
        &|path| aterm_uds::ownerlock::probe(path) == aterm_uds::ownerlock::Owner::Live,
    );
    locked.is_some() && aterm_uds::process::read_identity(raw) == Some(identity)
}

/// `verb` is `build`, or `xb`, which the workspace's config makes an alias of it.
fn nested_build(hop: bool, verb: &str) {
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
    write(&ws.join(".cargo/config.toml"), "[alias]\nxb = \"build\"\n");
    let shim = root.join("targo-shim");
    let block = atpkg::build_cache::shim_block("targo").unwrap();
    let forwarding = if hop {
        let quoted = format!("'{}'", targo.to_string_lossy().replace('\'', "'\\''"));
        atpkg::build_cache::registered::shim_hop(
            Path::new(env!("CARGO_BIN_EXE_atpkg")),
            "targo",
            &quoted,
            None,
        )
    } else {
        format!("exec '{}' \"$@\"\n", targo.display())
    };
    write(&shim, &format!("#!/bin/sh\n{block}{forwarding}"));

    let log = root.join("build.log");
    let messages = root.join("build-messages.jsonl");
    let ledger = root.join("ledger");
    let ready = root.join("build-script-ready");
    let release = root.join("build-script-release");
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command
        .arg(&shim)
        .args(["--unverified", verb, "-q", "--message-format=json"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("PWD", &ws)
        .env("BC_SHIM", &shim)
        .current_dir(&ws)
        .stdout(std::fs::File::create(&messages).unwrap())
        .stderr(std::fs::File::create(&log).unwrap());
    if hop {
        command
            .env(atpkg::build_admit::LEDGER_ENV, &ledger)
            .env("ATERM_STATE_HOME", root.join("state"))
            .env("ATERM_ADMIT", "now")
            .env("BC_HOP_READY", &ready)
            .env("BC_HOP_RELEASE", &release);
    }
    let mut child = command.spawn().unwrap();
    let witness = if hop {
        let script_pid = aterm_spec::wait::until_within(HANG, "real Cargo's build script", || {
            std::fs::read_to_string(&ready).ok()?.parse::<i32>().ok()
        });
        let parent = aterm_uds::process::read_process_parent(script_pid);
        let locked = parent.is_some_and(|pid| real_cargo_lock(pid, targo));
        let claims = aterm_capacity::Ledger::at(&ledger)
            .list()
            .expect("scratch ledger");
        let covered = parent.is_some_and(|pid| {
            claims
                .iter()
                .any(|claim| claim.pid == pid && claim.held.is_some() && claim.need.covers)
        });
        let ordinary = real_cargo_lock(std::process::id(), targo);
        std::fs::write(&release, "go").expect("release the build script");
        Some((locked, covered, ordinary))
    } else {
        None
    };
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
    if let Some((locked, covered, ordinary)) = witness {
        assert!(
            locked,
            "the real Cargo's active locked file and image were not witnessed"
        );
        assert!(covered, "the real Cargo had no covering claim");
        assert!(
            !ordinary,
            "an ordinary ancestor cannot become Cargo by its file names"
        );
        aterm_spec::wait::until_within(HANG, "the outer claim let go", || {
            aterm_capacity::Ledger::at(&ledger)
                .list()
                .ok()?
                .is_empty()
                .then_some(())
        });
    }

    let cache: PathBuf = atpkg::build_cache::build_dir(&atpkg::build_cache::cache_root(&home), &ws);
    let script_outputs: Vec<PathBuf> = std::fs::read_to_string(&messages)
        .unwrap()
        .lines()
        .map(|line| aterm_json::from_str::<aterm_json::Value>(line).expect("Cargo JSON message"))
        .filter(|message| {
            message.get("reason").and_then(aterm_json::Value::as_str)
                == Some("build-script-executed")
        })
        .map(|message| {
            PathBuf::from(
                message
                    .get("out_dir")
                    .and_then(aterm_json::Value::as_str)
                    .expect("actual build-script output directory"),
            )
        })
        .collect();
    assert!(
        !script_outputs.is_empty(),
        "Cargo reported the actual build script's output"
    );
    let cache = cache.canonicalize().unwrap();
    for output in script_outputs {
        assert!(
            output.is_dir() && output.canonicalize().unwrap().starts_with(&cache),
            "actual intermediates in the cache: {}",
            output.display()
        );
    }
    assert!(
        ws.join("target/debug/outer").is_file(),
        "the binary in target/"
    );
    assert!(
        !ws.join("target/debug/deps").exists(),
        "no intermediates in target/"
    );
    assert!(
        !ws.join("target/debug/build").exists(),
        "no per-unit intermediates in target/"
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
