// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real parent deaths through land's command builder, with private capacity custody.

#[cfg(target_vendor = "apple")]
fn main() {
    use aterm_capacity::{Admit, Fixed, Ledger, Need, Pace, Reading, admit_with};
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use aterm_uds::spawnfd::command;
    use std::path::Path;

    if let Some(code) = aterm_uds::tether::entry() {
        std::process::exit(if code == std::process::ExitCode::SUCCESS {
            0
        } else {
            1
        });
    }
    let exe = std::env::current_exe().unwrap();
    let role = std::env::var("LAND_LIFETIME_ROLE").unwrap_or_default();
    let dir = std::env::var_os("LAND_LIFETIME_DIR").map(std::path::PathBuf::from);
    if role == "parent" {
        let dir = dir.unwrap();
        let mut child = if std::env::var_os("LAND_LIFETIME_CONTROL").is_some() {
            command(&exe)
        } else {
            aterm_verify::land::judgment_command(
                &exe,
                &dir,
                "fake-base",
                &dir.join("events"),
                Admit::Wait,
                None,
            )
        };
        child
            .env("LAND_LIFETIME_ROLE", "verify")
            .spawn()
            .unwrap()
            .wait()
            .unwrap();
        unreachable!();
    }
    if role == "verify" {
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
        std::fs::write(dir.join("verify"), std::process::id().to_string()).unwrap();
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
        unreachable!();
    }
    if role == "leaf" {
        std::fs::write(dir.unwrap().join("leaf"), std::process::id().to_string()).unwrap();
        loop {
            std::thread::park();
        }
    }
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
    println!(
        "land lifetime: INT, TERM, HUP, KILL; descendants, capacity, bystander; untethered negative controls passed"
    );
}

#[cfg(not(target_vendor = "apple"))]
fn main() {}
