// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real parent deaths through land's command builder, with private capacity custody, and the
//! living parent's interrupt and supervision of a judgment main's tip doomed.

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn main() {
    use aterm_capacity::{Admit, Fixed, Ledger, Need, Pace, Reading, admit_with};
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use aterm_uds::spawnfd::command;
    use std::path::Path;

    // Existence is the reader's readiness event. Publish only complete bytes:
    // create-then-write exposes an empty marker between those two operations.
    fn publish(path: &Path, text: &str) {
        aterm_uds::atomic_file::write_flushed(path, text.as_bytes(), 0o600, None).unwrap();
    }
    fn publish_pid(path: &Path) {
        publish(path, &std::process::id().to_string());
    }

    let role = std::env::var("LAND_LIFETIME_ROLE").unwrap_or_default();
    let dir = std::env::var_os("LAND_LIFETIME_DIR").map(std::path::PathBuf::from);
    if let Some(code) = aterm_uds::tether::entry() {
        std::process::exit(if code == std::process::ExitCode::SUCCESS {
            0
        } else {
            1
        });
    }
    let exe = std::env::current_exe().unwrap();
    let mut raw: Vec<_> = std::env::args_os().skip(1).collect();
    if role == "parent" {
        let dir = dir.unwrap();
        let escaped = std::env::var_os("LAND_LIFETIME_ESCAPED").is_some();
        let compiler = std::env::var_os("LAND_LIFETIME_COMPILER").is_some();
        let mut child = if !escaped && std::env::var_os("LAND_LIFETIME_CONTROL").is_some() {
            command(&exe)
        } else if compiler {
            let home = dir.join("bootstrap-home");
            std::fs::create_dir(&home).unwrap();
            std::fs::write(dir.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
            let env = aterm_verify::EnvSnapshot {
                home,
                ..Default::default()
            };
            let plan = aterm_verify::NativeBuildPlan::for_default_target(&env, &dir, &dir);
            aterm_verify::native_verifier::build_command(&dir, &exe, &plan).unwrap()
        } else {
            // A recheck's child is the same command over the source readers alone.
            let checks: &[&str] = if std::env::var_os("LAND_LIFETIME_RECHECK").is_some() {
                &aterm_verify::land::RECHECKS
            } else {
                &[]
            };
            aterm_verify::land::judgment_command(
                &exe,
                &dir,
                "fake-base",
                checks,
                &dir.join("events"),
                Admit::Wait,
                &aterm_verify::EnvSnapshot::capture(),
                None,
            )
            .unwrap()
        };
        let mut child = child
            .env(
                "LAND_LIFETIME_ROLE",
                if escaped { "bootstrap" } else { "verify" },
            )
            .spawn()
            .unwrap();
        if let Some(how) = std::env::var_os("LAND_LIFETIME_INTERRUPT") {
            // The landing's poll found main doomed the judgment: interrupt it and live on. The
            // negative control only says it did.
            until_within(MUST_HAPPEN, "the test let the interrupt go", || {
                dir.join("go").exists().then_some(())
            });
            if how != "pretend" {
                // What the group started outside it is awaited until the hang detector passes
                // with none of it ending, so only a hang, never a loaded host, fails a case.
                aterm_verify::land::interrupt_judgment_awaiting(&mut child, MUST_HAPPEN);
            }
            publish_pid(&dir.join("interrupted"));
            loop {
                std::thread::park();
            }
        }
        if std::env::var_os("LAND_LIFETIME_SUPERVISE").is_some() {
            // The landing's own supervision: each poll is counted, and main dooms the judgment
            // once the test says so.
            let mut polls = 0_u32;
            let ending = aterm_verify::land::supervise_judgment(
                &mut child,
                &dir.join("events"),
                &mut |_| {
                    polls += 1;
                    publish(&dir.join("polls"), &polls.to_string());
                    dir.join("doom").exists()
                },
            )
            .unwrap();
            publish(
                &dir.join("ending"),
                &match ending {
                    aterm_verify::land::Ending::Interrupted => "interrupted".to_owned(),
                    aterm_verify::land::Ending::Exited(code) => format!("exited {code}"),
                },
            );
            loop {
                std::thread::park();
            }
        }
        child.wait().unwrap();
        unreachable!();
    }
    if role == "bootstrap" {
        // Targo's run boundary can give the verifier a group outside the bootstrap's group.
        let mut child = command(&exe);
        child.process_group(0).env("LAND_LIFETIME_ROLE", "verify");
        assert!(
            raw.iter()
                .any(|arg| arg == aterm_verify::land::PARENT_LIFETIME)
        );
        child.arg(aterm_verify::land::PARENT_LIFETIME);
        child.spawn().unwrap().wait().unwrap();
        unreachable!();
    }
    if role == "verify" {
        // A compiler stand-in carries no verifier flag. Direct and detached verifier
        // roles arm the same early watch before starting any other thread.
        if !(std::env::var_os("LAND_LIFETIME_ESCAPED").is_some()
            && std::env::var_os("LAND_LIFETIME_CONTROL").is_some())
        {
            // SAFETY: this fresh executable has started no other thread.
            unsafe { aterm_verify::land::own_judgment(&mut raw) }.unwrap();
        }
        if std::env::var_os("LAND_LIFETIME_RECHECK").is_some() {
            let words: Vec<_> = raw.iter().map(|arg| arg.to_string_lossy()).collect();
            publish(&dir.as_ref().unwrap().join("args"), &words.join(" "));
        }
        if std::env::var_os("LAND_LIFETIME_HANDLER").is_some() {
            // The real verifier's interrupt handling: end the stage groups, report, die of it.
            aterm_verify::exec::group::kill_on_interrupt();
        }
        if std::env::var_os("LAND_LIFETIME_DEAF").is_some() {
            // A leader that outlasts SIGTERM. A caught signal, unlike an ignored one, is no
            // disposition its stage inherits.
            extern "C" fn outlast(_: libc::c_int) {}
            // SAFETY: the handler does nothing, which is async-signal-safe, and lives for the
            // process.
            unsafe {
                libc::signal(libc::SIGTERM, outlast as *const () as libc::sighandler_t);
            }
        }
        let dir = dir.unwrap();
        let _claim = admit_with(
            &Ledger::at(dir.join("ledger")),
            &Fixed::new(Reading::of_gib(48, 40, 14)),
            Need::new("fake verification work").mem_gib(19),
            Admit::Now,
            Pace::default(),
            &mut |_| {},
        )
        .unwrap();
        publish_pid(&dir.join("verify"));
        let mut cmd = aterm_verify::exec::Cmd::new(&exe);
        cmd.envs.push(("LAND_LIFETIME_ROLE".into(), "leaf".into()));
        let _ = aterm_verify::exec::run(
            &cmd,
            aterm_verify::exec::ExecEnv {
                events: None,
                cwd: &dir,
                path: std::ffi::OsStr::new("/usr/bin:/bin"),
                scratch: &dir,
                child_ceiling: None,
                remove_env: &[],
                add_env: &[],
                stage_groups: None,
                host: &aterm_verify::exec::LIVE_HOST,
                hang_samples: None,
            },
        );
        if std::env::var_os("LAND_LIFETIME_HANDLER").is_some() {
            aterm_verify::exec::group::end_interrupted();
        }
        if std::env::var_os("LAND_LIFETIME_SUPERVISE").is_some() {
            // The stage ended by itself: the run exits as a green verifier would.
            std::process::exit(0);
        }
        unreachable!();
    }
    if role == "leaf" {
        if std::env::var_os("LAND_LIFETIME_DEAF_STAGE").is_some() {
            // A stage that outlasts SIGTERM too, so its own watch ends it only END_GRACE after
            // its verifier is gone: an interrupt that awaits it must follow the verifier's
            // descendants, not its children alone.
            extern "C" fn outlast(_: libc::c_int) {}
            // SAFETY: the handler does nothing, which is async-signal-safe, and lives for the
            // process.
            unsafe {
                libc::signal(libc::SIGTERM, outlast as *const () as libc::sighandler_t);
            }
        }
        publish_pid(&dir.unwrap().join("leaf"));
        loop {
            std::thread::park();
        }
    }
    bootstrap_placement();

    fn pid(dir: &Path, name: &str) -> i32 {
        std::fs::read_to_string(dir.join(name))
            .unwrap()
            .parse()
            .unwrap()
    }
    fn live(pid: i32) -> bool {
        aterm_uds::process::read_identity(pid).is_some()
    }
    struct Cleanup(Vec<aterm_uds::process::Identity>);
    impl Cleanup {
        fn record(&mut self, pid: i32) {
            self.0
                .push(aterm_uds::process::read_identity(pid).expect("own stand-in is running"));
        }
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for identity in &self.0 {
                if aterm_uds::process::read_identity(identity.pid) == Some(*identity) {
                    // SAFETY: the identity still names a process this test started.
                    unsafe {
                        libc::kill(identity.pid, libc::SIGKILL);
                    }
                }
            }
        }
    }
    for phase in ["direct", "detached", "compiler"] {
        let escaped = phase == "detached";
        for control in [true, false] {
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGKILL] {
                let scratch = aterm_tempfile::tempdir().unwrap();
                let dir = scratch.path();
                let mut cleanup = Cleanup(Vec::new());
                let mut bystander = command("/bin/sleep").arg("600").spawn().unwrap();
                cleanup.record(bystander.id() as i32);
                let mut parent = command(&exe);
                parent
                    .env("LAND_LIFETIME_ROLE", "parent")
                    .env("LAND_LIFETIME_DIR", dir);
                if escaped {
                    parent.env("LAND_LIFETIME_ESCAPED", "1");
                }
                if phase == "compiler" {
                    parent.env("LAND_LIFETIME_COMPILER", "1");
                }
                if control {
                    parent.env("LAND_LIFETIME_CONTROL", "1");
                }
                let mut parent = parent.spawn().unwrap();
                cleanup.record(parent.id() as i32);
                until_within(MUST_HAPPEN, "leaf executed", || {
                    dir.join("leaf").exists().then_some(())
                });
                let child = pid(dir, "verify");
                let leaf = pid(dir, "leaf");
                cleanup.record(child);
                cleanup.record(leaf);
                let ledger = Ledger::at(dir.join("ledger"));
                assert_eq!(ledger.list().unwrap().len(), 1);
                // SAFETY: this test started and still owns the waiting parent.
                assert_eq!(unsafe { libc::kill(parent.id() as i32, signal) }, 0);
                parent.wait().unwrap();
                if control {
                    assert!(live(child), "negative control must reproduce the orphan");
                    assert_eq!(ledger.list().unwrap().len(), 1, "orphan retains its claim");
                    // SAFETY: these are this test's own stand-ins, proved running by their markers.
                    unsafe {
                        libc::kill(child, libc::SIGKILL);
                        libc::kill(leaf, libc::SIGKILL);
                    }
                } else {
                    until_within(MUST_HAPPEN, "verify and stage ended", || {
                        (!live(child) && !live(leaf)).then_some(())
                    });
                }
                until_within(MUST_HAPPEN, "capacity released", || {
                    ledger.list().unwrap().is_empty().then_some(())
                });
                assert!(
                    bystander.try_wait().unwrap().is_none(),
                    "bystander survives"
                );
                bystander.kill().unwrap();
                bystander.wait().unwrap();
            }
        }
    }
    // A verifier's start: the parent, its judgment's verifier holding its capacity claim, and
    // the stage leaf beneath it, each running; `env` names how.
    struct Started {
        _scratch: aterm_tempfile::TempDir,
        dir: std::path::PathBuf,
        cleanup: Cleanup,
        bystander: aterm_uds::spawnfd::Child,
        parent: aterm_uds::spawnfd::Child,
        child: i32,
        leaf: i32,
        ledger: Ledger,
    }
    let start = |env: &[(&str, &str)]| {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dir = scratch.path().to_path_buf();
        let mut cleanup = Cleanup(Vec::new());
        let bystander = command("/bin/sleep").arg("600").spawn().unwrap();
        cleanup.record(bystander.id() as i32);
        let mut parent = command(&exe);
        parent
            .env("LAND_LIFETIME_ROLE", "parent")
            .env("LAND_LIFETIME_DIR", &dir);
        for (key, value) in env {
            parent.env(key, value);
        }
        let parent = parent.spawn().unwrap();
        cleanup.record(parent.id() as i32);
        until_within(MUST_HAPPEN, "leaf executed", || {
            dir.join("leaf").exists().then_some(())
        });
        let child = pid(&dir, "verify");
        let leaf = pid(&dir, "leaf");
        cleanup.record(child);
        cleanup.record(leaf);
        let ledger = Ledger::at(dir.join("ledger"));
        assert_eq!(
            ledger.list().unwrap().len(),
            1,
            "the judgment holds its claim"
        );
        Started {
            _scratch: scratch,
            dir,
            cleanup,
            bystander,
            parent,
            child,
            leaf,
            ledger,
        }
    };
    let finish = |mut started: Started| {
        assert!(
            started.parent.try_wait().unwrap().is_none(),
            "the interrupting parent lives on"
        );
        assert!(
            started.bystander.try_wait().unwrap().is_none(),
            "bystander survives"
        );
        started.parent.kill().unwrap();
        started.parent.wait().unwrap();
        started.bystander.kill().unwrap();
        started.bystander.wait().unwrap();
        drop(started.cleanup);
    };
    // An interrupt from the living parent, as `verify land` gives a judgment main doomed: the
    // judgment's group, a verifier Targo's run left in a group of its own, the stage beneath it
    // and its capacity claim have all ended once the interrupt returns, while the parent, which
    // goes on to the re-judge, and a bystander live on. So they do for a verifier that handles
    // the signal as the real one does, and one that outlasts it until the grace runs out, in its
    // judgment's group or (its own watch's grace) in its own, there with a stage that outlasts
    // it too (its watch's grace after that), which only a wait for the verifier's descendants,
    // not its children alone, sees end. NEGATIVE CONTROL: a parent that only says it interrupted
    // leaves all three running.
    for env in [
        &[("LAND_LIFETIME_INTERRUPT", "1")][..],
        &[
            ("LAND_LIFETIME_INTERRUPT", "1"),
            ("LAND_LIFETIME_ESCAPED", "1"),
        ],
        &[
            ("LAND_LIFETIME_INTERRUPT", "1"),
            ("LAND_LIFETIME_HANDLER", "1"),
        ],
        &[
            ("LAND_LIFETIME_INTERRUPT", "1"),
            ("LAND_LIFETIME_DEAF", "1"),
        ],
        &[
            ("LAND_LIFETIME_INTERRUPT", "1"),
            ("LAND_LIFETIME_ESCAPED", "1"),
            ("LAND_LIFETIME_DEAF", "1"),
            ("LAND_LIFETIME_DEAF_STAGE", "1"),
        ],
        &[("LAND_LIFETIME_INTERRUPT", "pretend")],
    ] {
        let started = start(env);
        publish_pid(&started.dir.join("go"));
        until_within(MUST_HAPPEN, "the interrupt returned", || {
            started.dir.join("interrupted").exists().then_some(())
        });
        let held = started.ledger.list().unwrap().len();
        if env.contains(&("LAND_LIFETIME_INTERRUPT", "pretend")) {
            assert!(
                live(started.child) && live(started.leaf) && held == 1,
                "negative control: nothing ended without the interrupt"
            );
            // SAFETY: these are this test's own stand-ins, proved running just now.
            unsafe {
                libc::kill(started.child, libc::SIGKILL);
                libc::kill(started.leaf, libc::SIGKILL);
            }
        } else {
            assert!(
                !live(started.child) && !live(started.leaf),
                "{env:?}: the verifier and its stage ended before the interrupt returned"
            );
            assert_eq!(held, 0, "{env:?}: its capacity was released by then");
        }
        finish(started);
    }
    // The landing's supervision of a real judgment: a stage boundary the run's events file
    // reports is polled, and a `true` interrupts the run, ending it, its stage and its claim.
    // NEGATIVE CONTROL: a boundary polled `false` interrupts nothing, and the run's own exit
    // ends the supervision with its code.
    let stage = aterm_verify::report::stage_event(
        0,
        Some(aterm_verify::checks::Check::Fmt),
        &aterm_verify::ladder::Report::new("fmt"),
        std::time::Duration::ZERO,
        std::time::Duration::ZERO,
        None,
    );
    let mut value: aterm_json::Value =
        aterm_json::from_str(stage.trim_end().strip_prefix("verify-event: ").unwrap()).unwrap();
    value.as_object_mut().unwrap().insert(
        "invocation".into(),
        aterm_json::json!({"id": "12".repeat(16), "source_observation": null, "identity_error": null}),
    );
    let boundary = format!("verify-event: {value}\n");
    for (doomed, recheck) in [(true, false), (false, false), (false, true)] {
        let mut env = vec![("LAND_LIFETIME_SUPERVISE", "1")];
        if recheck {
            env.push(("LAND_LIFETIME_RECHECK", "1"));
        }
        let started = start(&env);
        let dir = &started.dir;
        if recheck {
            let args = std::fs::read_to_string(dir.join("args")).unwrap();
            assert!(
                args.starts_with("fmt census export-scan --base fake-base --events "),
                "{args}"
            );
            assert!(!args.contains(aterm_verify::cli::INPUT_DRIFT), "{args}");
        }
        if doomed {
            publish_pid(&dir.join("doom"));
        }
        publish(&dir.join("events"), &boundary);
        until_within(MUST_HAPPEN, "the boundary was polled", || {
            std::fs::read_to_string(dir.join("polls"))
                .is_ok_and(|polls| polls == "1")
                .then_some(())
        });
        if !doomed {
            assert!(live(started.child) && live(started.leaf));
            // SAFETY: this test's own stand-in, proved running just now.
            unsafe {
                libc::kill(started.leaf, libc::SIGKILL);
            }
        }
        let ending = until_within(MUST_HAPPEN, "the supervision ended", || {
            std::fs::read_to_string(dir.join("ending")).ok()
        });
        assert_eq!(
            ending,
            if doomed { "interrupted" } else { "exited 0" },
            "doomed: {doomed}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("polls")).unwrap(),
            "1",
            "one boundary, one poll"
        );
        assert!(
            !live(started.child) && !live(started.leaf),
            "doomed: {doomed}: the run and its stage are gone"
        );
        assert!(
            started.ledger.list().unwrap().is_empty(),
            "doomed: {doomed}: its capacity was released"
        );
        finish(started);
    }
    println!(
        "land lifetime: compiler, direct and detached groups, INT/TERM/HUP/KILL; descendants, capacity, bystander and negative controls passed; an interrupt from the living parent ended both group shapes, a handling and an outlasting verifier, an outlasting stage beneath a detached one, their stages and claims before it returned; supervision polled a reported boundary and interrupted, and its control and a recheck's child exited"
    );
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
fn main() {
    if let Some(code) = aterm_uds::tether::entry() {
        std::process::exit(if code == std::process::ExitCode::SUCCESS {
            0
        } else {
            1
        });
    }
    #[cfg(unix)]
    bootstrap_placement();
}

/// Run the native compile/run placement probe from an image that serves the tether trampoline.
#[cfg(unix)]
fn bootstrap_placement() {
    use aterm_verify as verify;
    use std::{path::PathBuf, time::Duration};
    let env = verify::EnvSnapshot::capture();
    let workspace = aterm_tempfile::manifest_dir()
        .join("../..")
        .canonicalize()
        .unwrap();
    let tools = verify::preflight(&env, &workspace).unwrap();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let physical = scratch.path().canonicalize().unwrap();
    let root = physical.as_path();
    let package = root.join("crates/aterm-verify");
    std::fs::create_dir_all(package.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers=['crates/aterm-verify']\nresolver='2'\n",
    )
    .unwrap();
    std::fs::write(
        package.join("Cargo.toml"),
        "[package]\nname='aterm-verify'\nversion='0.0.0'\nedition='2024'\n",
    )
    .unwrap();
    std::fs::write(
        package.join("build.rs"),
        "fn main() { std::fs::write(\"observed-out-dir\", std::env::var_os(\"OUT_DIR\").unwrap().as_encoded_bytes()).unwrap(); }\n",
    )
    .unwrap();
    std::fs::write(package.join("src/main.rs"), r#"
fn main() {
    std::fs::write(std::env::var_os("BOOTSTRAP_RUN_MARKER").unwrap(), b"actual verifier executed").unwrap();
    for key in ["CARGO_MANIFEST_DIR", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "CARGO_BUILD_BUILD_DIR", "CARGO_HOME", "CARGO_PKG_NAME", "OUT_DIR", "BOOTSTRAP_TOOL_POLICY"] {
        println!("{key}={}", std::env::var(key).unwrap_or_else(|_| "<unset>".into()));
    }
}
"#).unwrap();
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    let snapshot = verify::EnvSnapshot {
        home,
        ..Default::default()
    };
    let plan = verify::NativeBuildPlan::for_default_target(&snapshot, root, root);
    let other_source = root.join("other-source");
    std::fs::create_dir(&other_source).unwrap();
    assert!(!plan.target().exists());
    assert!(verify::native_verifier::build_command(&other_source, &tools.targo, &plan).is_err());
    assert!(
        !plan.target().exists(),
        "a source mismatch must precede preparation"
    );
    let selected = PathBuf::from(
        aterm_toolchain::build_cache::directory_from_config(plan.build_dir().unwrap()).unwrap(),
    );
    let marker = root.join("verifier-ran");
    let compile = |plan: &verify::NativeBuildPlan| {
        let mut command = verify::native_verifier::build_command(root, &tools.targo, plan).unwrap();
        let args: Vec<_> = command.get_args().collect();
        let verb = args.iter().position(|arg| *arg == "build").unwrap();
        for (at, arg) in args.iter().enumerate() {
            if *arg == "--config" {
                assert!(
                    at > verb,
                    "every bootstrap config uses the command-local parser"
                );
            }
        }
        let out =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !marker.exists(),
            "bootstrap must compile without running the verifier"
        );
        let executable = verify::native_verifier::built(root, &out.stdout).unwrap();
        let out_dir =
            PathBuf::from(std::fs::read_to_string(package.join("observed-out-dir")).unwrap());
        (executable, out_dir)
    };
    let control = root.join(".ubd/control.noindex");
    let control_env = verify::EnvSnapshot {
        cargo_layout: vec![("CARGO_BUILD_BUILD_DIR", control.clone().into_os_string())],
        ..snapshot.clone()
    };
    let control_plan = verify::NativeBuildPlan::for_default_target(&control_env, root, root);
    let (_, unplaced) = compile(&control_plan);
    assert!(
        unplaced.starts_with(&control),
        "negative control: {unplaced:?}"
    );
    let (executable, placed) = compile(&plan);
    assert!(
        placed.starts_with(&selected),
        "selected bootstrap was ignored: {placed:?}"
    );
    assert!(!placed.starts_with(control));
    let mut command = verify::land::judgment_command(
        &executable,
        root,
        "main",
        &[],
        &root.join("events"),
        aterm_capacity::Admit::Wait,
        &snapshot,
        None,
    )
    .unwrap();
    for name in verify::CARGO_LAYOUT_VARS {
        command.env(name, root.join("later-compiler-value"));
    }
    command
        .env("CARGO_PKG_NAME", "obsolete-bootstrap")
        .env("OUT_DIR", "obsolete-bootstrap")
        .env("CARGO_MANIFEST_DIR", "obsolete-bootstrap")
        .env("BOOTSTRAP_TOOL_POLICY", "retained");
    verify::native_verifier::configure_runtime(&mut command, root, &snapshot);
    command.env("BOOTSTRAP_RUN_MARKER", &marker);
    let out = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read(marker).unwrap(), b"actual verifier executed");
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!(
            "CARGO_MANIFEST_DIR={}\nCARGO_TARGET_DIR=<unset>\nCARGO_BUILD_TARGET_DIR=<unset>\nCARGO_BUILD_BUILD_DIR=<unset>\nCARGO_HOME=<unset>\nCARGO_PKG_NAME=<unset>\nOUT_DIR=<unset>\nBOOTSTRAP_TOOL_POLICY=retained\n",
            package.display()
        )
    );
}
