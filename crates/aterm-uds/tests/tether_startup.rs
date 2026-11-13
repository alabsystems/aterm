// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Linux adoption before a child arms its parent watch.
//!
//! STOCK EXCEPTION: this requires Linux's native subreaper kernel API; Trust has no
//! Linux std. The isolated `lab linux` guest runs it on the checkout's stock pin.

#[cfg(target_os = "linux")]
fn main() {
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use aterm_uds::process::{Identity, read_identity, read_process_parent};
    use aterm_uds::spawnfd::command;

    if let Some(code) = aterm_uds::tether::entry() {
        std::process::exit(if code == std::process::ExitCode::SUCCESS {
            0
        } else {
            1
        });
    }
    let exe = std::env::current_exe().unwrap();
    let role = std::env::var("TETHER_STARTUP_ROLE").unwrap_or_default();
    if !role.is_empty() {
        let world = std::path::PathBuf::from(std::env::var_os("TETHER_STARTUP_DIR").unwrap());
        if role == "caller" {
            let mut child = command(exe)
                .env("TETHER_STARTUP_ROLE", "child")
                .env("TETHER_STARTUP_EXPECTED", std::process::id().to_string())
                .process_group(0)
                .spawn()
                .unwrap();
            aterm_uds::atomic_file::write_flushed(
                &world.join("child"),
                child.id().to_string().as_bytes(),
                0o600,
                None,
            )
            .unwrap();
            child.wait().unwrap();
            unreachable!();
        }
        assert_eq!(role, "child");
        std::fs::write(world.join("ready"), b"waiting before arm").unwrap();
        until_within(MUST_HAPPEN, "startup barrier released", || {
            world.join("go").exists().then_some(())
        });
        let explicit = std::env::var_os("TETHER_STARTUP_EXPLICIT").is_some();
        let expected = std::env::var("TETHER_STARTUP_EXPECTED")
            .unwrap()
            .parse()
            .unwrap();
        // SAFETY: this dedicated executable has started no other thread.
        let result = unsafe {
            if explicit {
                aterm_uds::tether::hold_parent_of(expected)
            } else {
                aterm_uds::tether::hold_parent()
            }
        };
        aterm_uds::atomic_file::write_flushed(
            &world.join("outcome"),
            if result.is_ok() { b"armed" } else { b"refused" },
            0o600,
            None,
        )
        .unwrap();
        if result.is_err() {
            return;
        }
        loop {
            std::thread::park();
        }
    }
    unsafe extern "C" {
        fn prctl(option: i32, ...) -> i32;
    }
    // SAFETY: PR_SET_CHILD_SUBREAPER takes one boolean integer and no pointers.
    assert_eq!(
        unsafe {
            prctl(
                36,
                1 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            )
        },
        0
    );
    struct Cleanup(Vec<Identity>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for id in &self.0 {
                if read_identity(id.pid) == Some(*id) {
                    // SAFETY: the unchanged identity is one of this test's own children.
                    unsafe {
                        libc::kill(id.pid, libc::SIGKILL);
                    }
                }
            }
        }
    }
    let mut bystander = command("/bin/sleep").arg("600").spawn().unwrap();
    let _bystander_cleanup = Cleanup(vec![read_identity(bystander.id() as i32).unwrap()]);
    for explicit in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let mut caller = command(&exe);
        caller
            .env("TETHER_STARTUP_ROLE", "caller")
            .env("TETHER_STARTUP_DIR", world.path());
        if explicit {
            caller.env("TETHER_STARTUP_EXPLICIT", "1");
        }
        let mut caller = caller.spawn().unwrap();
        let mut cleanup = Cleanup(vec![read_identity(caller.id() as i32).unwrap()]);
        let child = until_within(MUST_HAPPEN, "child entered startup barrier", || {
            if !world.path().join("ready").exists() {
                return None;
            }
            std::fs::read_to_string(world.path().join("child"))
                .ok()?
                .parse::<i32>()
                .ok()
        });
        cleanup.0.push(read_identity(child).unwrap());
        assert_eq!(read_process_parent(child), Some(caller.id()));
        let original = caller.id();
        caller.kill().unwrap();
        caller.wait().unwrap();
        until_within(MUST_HAPPEN, "kernel adoption by the subreaper", || {
            (read_process_parent(child) == Some(std::process::id())).then_some(())
        });
        std::fs::write(world.path().join("go"), b"arm now").unwrap();
        let outcome = until_within(MUST_HAPPEN, "startup custody outcome", || {
            std::fs::read(world.path().join("outcome")).ok()
        });
        assert_eq!(
            outcome,
            if explicit {
                b"refused".as_slice()
            } else {
                b"armed".as_slice()
            }
        );
        if !explicit {
            assert_eq!(read_identity(child), cleanup.0.last().copied());
            // SAFETY: child still has the recorded identity of this test's adopted child.
            assert_eq!(unsafe { libc::kill(child, libc::SIGKILL) }, 0);
        }
        let mut status = 0;
        // SAFETY: the kernel adopted this exact child into this subreaper.
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        if explicit {
            use std::os::unix::process::ExitStatusExt as _;
            assert!(std::process::ExitStatus::from_raw(status).success());
        }
        assert!(
            bystander.try_wait().unwrap().is_none(),
            "unrelated child survives"
        );
        println!(
            "startup case explicit={explicit}: original caller {original} died; subreaper {} adopted child {child}; outcome={}",
            std::process::id(),
            String::from_utf8_lossy(&outcome)
        );
    }
    bystander.kill().unwrap();
    bystander.wait().unwrap();
    until_within(MUST_HAPPEN, "all owned watcher children reaped", || {
        let mut status = 0;
        // SAFETY: this dedicated harness reaps only its own remaining adopted children.
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid < 0 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
            Some(())
        } else {
            None
        }
    });
    println!(
        "startup custody: explicit original parent refused adoption; current-parent negative control armed; bystander survived and owned children reaped"
    );
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("tether startup: Linux subreaper qualification not run on this target");
}
