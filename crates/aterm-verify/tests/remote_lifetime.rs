// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual linked transport custody while the replay reader waits for a local pipe.

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn main() {
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use aterm_uds::process::{Identity, ProcessPresence, process_presence, read_identity};
    use aterm_uds::spawnfd::command;
    use std::io::Write as _;
    use std::os::fd::AsFd as _;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    if std::env::var_os("REMOTE_LIFETIME_PARENT").is_some()
        && std::env::var_os("REMOTE_LIFETIME_LOST").is_some()
    {
        let world = std::path::PathBuf::from(std::env::var_os("REMOTE_LIFETIME_DIR").unwrap());
        aterm_uds::tether::through(world.join("missing-recorded-image"));
    }
    if let Some(code) = aterm_uds::tether::entry() {
        std::process::exit(if code == std::process::ExitCode::SUCCESS {
            0
        } else {
            1
        });
    }
    let exe = std::env::current_exe().unwrap();
    let raw: Vec<_> = std::env::args_os().skip(1).collect();
    if raw.first().is_some_and(|arg| arg == "--unowned-agh") {
        // Negative control: the old transport entry ran agh without owning its parent.
        let code = agh::cli::main_entry(raw[1..].to_vec());
        std::process::exit(if code == std::process::ExitCode::SUCCESS {
            0
        } else {
            1
        });
    }
    if std::env::var_os("REMOTE_LIFETIME_PARENT").is_some() {
        let world = std::path::PathBuf::from(std::env::var_os("REMOTE_LIFETIME_DIR").unwrap());
        if std::env::var_os("REMOTE_LIFETIME_LOST").is_some() {
            assert!(aterm_uds::tether::executable().is_none());
        }
        let control = std::env::var_os("REMOTE_LIFETIME_CONTROL").is_some();
        let program = if control {
            exe.into_os_string()
        } else {
            std::env::var_os("REMOTE_LIFETIME_VERIFY").unwrap()
        };
        let mut child = command(program);
        child.env_remove("REMOTE_LIFETIME_PARENT").arg(if control {
            "--unowned-agh"
        } else {
            "--remote-agh"
        });
        if !control {
            child.arg(std::process::id().to_string());
        }
        child
            .arg("--replay")
            .arg(world.join("tape"))
            .process_group(0);
        let mut child = child.spawn().unwrap();
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
    struct Cleanup(Vec<Identity>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for identity in &self.0 {
                if read_identity(identity.pid) == Some(*identity) {
                    // SAFETY: the unchanged identity names only this test's own child.
                    unsafe {
                        libc::kill(identity.pid, libc::SIGKILL);
                    }
                }
            }
        }
    }
    let mut bystander = command("/bin/sleep").arg("600").spawn().unwrap();
    let _bystander_cleanup = Cleanup(vec![read_identity(bystander.id() as i32).unwrap()]);
    for lost in [false, true] {
        for control in [true, false] {
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGKILL] {
                let world = aterm_tempfile::tempdir().unwrap();
                let tape = world.path().join("tape");
                let name = std::ffi::CString::new(tape.as_os_str().as_bytes()).unwrap();
                // SAFETY: name is a live NUL-terminated path in this test's private directory.
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                let mut pipe = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&tape)
                    .unwrap();
                let mut filled = 0;
                loop {
                    match pipe.write(&[b'{'; 8192]) {
                        Ok(0) => panic!("pipe accepted no bytes"),
                        Ok(n) => filled += n,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => panic!("fill private pipe: {e}"),
                    }
                }
                assert!(filled > 0);
                let mut probe = libc::pollfd {
                    fd: pipe.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: probe names one valid pollfd and the poll is nonblocking.
                assert_eq!(
                    unsafe { libc::poll(&mut probe, 1, 0) },
                    0,
                    "full pipe must not be writable before its reader starts"
                );
                let mut parent = command(&exe);
                parent
                    .env("REMOTE_LIFETIME_PARENT", "1")
                    .env("REMOTE_LIFETIME_DIR", world.path())
                    .env(
                        "REMOTE_LIFETIME_VERIFY",
                        aterm_tempfile::bin_exe("aterm-verify"),
                    );
                if lost {
                    parent.env("REMOTE_LIFETIME_LOST", "1");
                }
                if control {
                    parent.env("REMOTE_LIFETIME_CONTROL", "1");
                }
                let mut parent = parent.spawn().unwrap();
                let mut cleanup = Cleanup(vec![read_identity(parent.id() as i32).unwrap()]);
                let child = until_within(MUST_HAPPEN, "transport PID published", || {
                    std::fs::read_to_string(world.path().join("child"))
                        .ok()?
                        .parse::<i32>()
                        .ok()
                });
                cleanup.0.push(read_identity(child).unwrap());
                eprintln!(
                    "remote case lost={lost} control={control} signal={signal} parent={} child={child}",
                    parent.id()
                );
                aterm_uds::ready::until(
                    pipe.as_fd(),
                    aterm_uds::ready::Interest::Write,
                    std::time::Instant::now() + MUST_HAPPEN,
                )
                .expect("actual linked CLI drained the full private pipe");
                // SAFETY: parent is the unreaped owned child whose handle this test retains.
                assert_eq!(unsafe { libc::kill(parent.id() as i32, signal) }, 0);
                parent.wait().unwrap();
                if control {
                    assert!(
                        read_identity(child).is_some(),
                        "negative control must reproduce the orphan"
                    );
                    assert_eq!(read_identity(child), cleanup.0.last().copied());
                    // SAFETY: the unchanged identity is the actual FIFO reader this test started.
                    assert_eq!(unsafe { libc::kill(child, libc::SIGKILL) }, 0);
                }
                until_within(MUST_HAPPEN, "native transport absence", || {
                    (process_presence(child) == ProcessPresence::Absent).then_some(())
                });
                assert!(
                    bystander.try_wait().unwrap().is_none(),
                    "bystander survives"
                );
            }
        }
    }
    bystander.kill().unwrap();
    bystander.wait().unwrap();
    println!(
        "remote lifetime: linked reader retired after INT/TERM/HUP/KILL with recorded image present or lost; negative controls reproduced orphan; bystander survived"
    );
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
fn main() {}
