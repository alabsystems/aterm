// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit native qualifier: `targo --unverified run -p aterm-uds --example wait_sample_live -- OUT`.
//! Samples only its own children; retains private reports and receipts. No windows or copied executables.

#[cfg(all(target_os = "macos", debug_assertions))]
mod native {
    use std::fmt::Write as _;
    use std::fs::File;
    use std::io::{self, Read as _, Write as _};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use aterm_sysprobe::jobs::{JobsReader, ReadReq, RunState};
    use aterm_uds::process::{Birth, Identity, read_identity, read_process_path};
    use aterm_uds::spawnfd::{Child, Stdio, command};
    use aterm_uds::{deadline, private_file, wait_sample};

    const HANG: Duration = Duration::from_secs(60);

    fn bad(what: &str) -> io::Error {
        io::Error::other(what)
    }

    fn until<T>(what: &str, mut read: impl FnMut() -> io::Result<Option<T>>) -> io::Result<T> {
        let by = Instant::now() + HANG;
        loop {
            if let Some(value) = read()? {
                return Ok(value);
            }
            if Instant::now() >= by {
                return Err(io::Error::new(io::ErrorKind::TimedOut, what));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn proof(line: &str) -> io::Result<()> {
        writeln!(io::stdout(), "{line}")?;
        io::stdout().flush()
    }

    fn child(mode: &str, path: Option<&Path>) -> io::Result<()> {
        if mode == "--read" {
            proof("read-ready")?;
            return io::stdin().read_exact(&mut [0u8]);
        }
        let path = path.ok_or_else(|| bad("the lock child needs its private lock path"))?;
        let file = File::open(path)?;
        if mode == "--waiter" {
            let path = path.to_path_buf();
            std::thread::Builder::new()
                .name("lock-proof".into())
                .spawn(move || {
                    let result = until("own main thread queued for the owned lock", || {
                        Ok((!aterm_uds::asleep::queued_for(&path)?.is_empty()).then_some(()))
                    })
                    .and_then(|()| proof("lock-queued"));
                    if let Err(error) = result {
                        eprintln!("lock proof: {error}");
                        std::process::exit(1);
                    }
                })?;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "this explicit qualifier's owned lock is released by process exit or the unlock below"
        )]
        file.lock()?;
        if mode == "--holder" {
            proof("lock-held")?;
            let _ = io::stdin().read_exact(&mut [0u8]);
        }
        file.unlock()
    }

    struct Kids(Vec<Child>);

    impl Kids {
        fn start(&mut self, program: &Path, args: &[&std::ffi::OsStr]) -> io::Result<usize> {
            let child = command(program)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?;
            self.0.push(child);
            Ok(self.0.len() - 1)
        }

        fn close(&mut self) -> io::Result<Vec<String>> {
            let mut receipts = Vec::new();
            for child in self.0.iter_mut().rev() {
                let pid = child.id();
                deadline::kill_within(child, HANG);
                let status = child
                    .try_wait()?
                    .ok_or_else(|| bad("an owned child was not reaped"))?;
                receipts.push(format!(
                    "{{\"pid\":{pid},\"reaped\":true,\"status\":{}}}",
                    quote(&status.to_string())
                ));
            }
            self.0.clear();
            Ok(receipts)
        }
    }

    impl Drop for Kids {
        fn drop(&mut self) {
            for child in &mut self.0 {
                deadline::kill_within(child, HANG);
            }
        }
    }

    fn waiting(child: &mut Child, executable: &Path) -> io::Result<Identity> {
        let pid = child.id();
        let mut reader = JobsReader::new();
        until(
            "the own child has one sleeping thread in its expected executable",
            || {
                if child.try_wait()?.is_some() {
                    return Err(bad("the own child exited before its waiting barrier"));
                }
                let identity = read_identity(i32::try_from(pid).map_err(io::Error::other)?);
                let facts = reader.read(ReadReq { pid, fds: false });
                let sleeping = facts.is_some_and(|p| {
                    !p.threads_truncated
                        && p.threads.is_some_and(|threads| {
                            threads.len() == 1
                                && matches!(
                                    threads[0].state,
                                    RunState::Sleeping | RunState::Uninterruptible
                                )
                        })
                });
                Ok(identity.filter(|identity| {
                    sleeping && read_process_path(identity.pid).as_deref() == Some(executable)
                }))
            },
        )
    }

    fn quote(text: &str) -> String {
        let mut out = String::from("\"");
        for ch in text.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                ch if ch <= '\u{1f}' => {
                    let _ = write!(out, "\\u{:04x}", u32::from(ch));
                }
                ch => out.push(ch),
            }
        }
        out.push('"');
        out
    }

    fn observe(
        out: &Path,
        name: &str,
        identity: Identity,
        expected: wait_sample::Kind,
    ) -> io::Result<String> {
        let observation = wait_sample::sample(identity, out, || false)?;
        let current = read_identity(identity.pid) == Some(identity);
        let passed = observation.kind == expected && current;
        let receipt = format!(
            "{{\"case\":{},\"identity\":{},\"expected_kind\":{},\"kind\":{},\"report\":{},\"elapsed_ms\":{},\"identity_current\":{current},\"passed\":{passed}}}",
            quote(name),
            quote(&identity.token()),
            quote(expected.token()),
            quote(observation.kind.token()),
            quote(&observation.report.to_string_lossy()),
            observation.elapsed.as_millis()
        );
        private_file::write_private(out, &format!("{name}.json"), receipt.as_bytes())?;
        if !passed {
            return Err(bad(&format!(
                "{name}: wanted {expected:?}, got {:?}; retained {}",
                observation.kind,
                observation.report.display()
            )));
        }
        Ok(receipt)
    }

    fn controls(out: &Path, identity: Identity) -> io::Result<String> {
        for (name, stale) in [
            (
                "birth",
                Identity {
                    birth: Birth(identity.birth.0.wrapping_add(1)),
                    ..identity
                },
            ),
            (
                "exec",
                Identity {
                    idversion: identity.idversion.map(|v| v.wrapping_add(1)),
                    ..identity
                },
            ),
        ] {
            let before = std::fs::read_dir(out)?.count();
            let error = wait_sample::sample(stale, out, || false)
                .expect_err("a stale identity must be refused");
            if error.kind() != io::ErrorKind::NotFound || std::fs::read_dir(out)?.count() != before
            {
                return Err(bad(&format!(
                    "stale {name} was not refused before creating a report"
                )));
            }
        }
        let canceled = out.join("pre-canceled-no-directory");
        if canceled.try_exists()? {
            return Err(bad("the pre-cancel control path already exists"));
        }
        let error = wait_sample::sample(identity, &canceled, || true)
            .expect_err("pre-canceled sample must be refused");
        if error.kind() != io::ErrorKind::Interrupted
            || canceled.try_exists()?
            || read_identity(identity.pid) != Some(identity)
        {
            return Err(bad("pre-cancel changed the filesystem or the own process"));
        }
        Ok("{\"stale_birth_rejected_before_report\":true,\"stale_exec_rejected_before_report\":true,\"pre_cancel_without_directory\":true}".to_owned())
    }

    pub fn run() -> io::Result<()> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if let Some(mode) = args
            .first()
            .and_then(|a| a.to_str())
            .filter(|a| matches!(*a, "--holder" | "--waiter" | "--read"))
        {
            let expected = if mode == "--read" { 1 } else { 2 };
            if args.len() != expected {
                return Err(bad("unexpected hidden child arguments"));
            }
            return child(mode, args.get(1).map(Path::new));
        }
        if args.len() != 1 {
            return Err(bad("usage: wait_sample_live OUT"));
        }
        let out = PathBuf::from(&args[0]);
        private_file::create_private_dir(&out)?;
        let out = std::fs::canonicalize(out)?;
        let scratch = aterm_tempfile::Builder::new()
            .prefix("wait-sample-live")
            .tempdir()?
            .keep(); // Retain scratch on any failure; never unlink beneath an unreaped child.
        eprintln!("owned scratch: {}", scratch.display());
        let lock = scratch.join("owned.lock");
        File::create(&lock)?;
        let executable = std::env::current_exe()?;
        let mut kids = Kids(Vec::new());
        let timer = kids.start(Path::new("/bin/sleep"), &[std::ffi::OsStr::new("120")])?;
        let timer_identity = waiting(&mut kids.0[timer], Path::new("/bin/sleep"))?;
        let timer_receipt = observe(&out, "timer", timer_identity, wait_sample::Kind::Timer)?;
        let negatives = controls(&out, timer_identity)?;
        private_file::write_private(&out, "controls.json", negatives.as_bytes())?;
        let holder = kids.start(
            &executable,
            &[std::ffi::OsStr::new("--holder"), lock.as_os_str()],
        )?;
        deadline::prints(&mut kids.0[holder], "lock-held", HANG);
        let waiter = kids.start(
            &executable,
            &[std::ffi::OsStr::new("--waiter"), lock.as_os_str()],
        )?;
        deadline::prints(&mut kids.0[waiter], "lock-queued", HANG);
        let lock_identity = waiting(&mut kids.0[waiter], &executable)?;
        let lock_receipt = observe(&out, "lock", lock_identity, wait_sample::Kind::Lock)?;
        let read = kids.start(&executable, &[std::ffi::OsStr::new("--read")])?;
        deadline::prints(&mut kids.0[read], "read-ready", HANG);
        let read_identity = waiting(&mut kids.0[read], &executable)?;
        let read_receipt = observe(&out, "read", read_identity, wait_sample::Kind::Read)?;
        let cleanup = kids.close()?;
        std::fs::remove_dir_all(scratch)?; // Only after every owned child was reaped.
        let result = format!(
            "{{\"passed\":true,\"cases\":[{timer_receipt},{lock_receipt},{read_receipt}],\"controls\":{negatives},\"cleanup\":[{}]}}\n",
            cleanup.join(",")
        );
        private_file::write_private(&out, "result.json", result.as_bytes())?;
        private_file::write_private(&out, "result.txt", b"PASS: owned Timer, blocked flock and Read; stale birth/exec and pre-cancel rejected; all four owned children reaped. Raw reports retained.\n")?;
        print!("{result}");
        Ok(())
    }
}

fn main() {
    #[cfg(all(target_os = "macos", debug_assertions))]
    let result = native::run();
    #[cfg(not(all(target_os = "macos", debug_assertions)))]
    let result: std::io::Result<()> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "wait_sample_live requires a macOS dev build",
    ));
    if let Err(error) = result {
        eprintln!("wait_sample_live: {error}");
        std::process::exit(1);
    }
}
