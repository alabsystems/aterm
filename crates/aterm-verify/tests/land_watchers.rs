// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Darwin judgment retires its anchored lifetime helper while the landing stays alive.

#[cfg(target_os = "macos")]
fn main() {
    use aterm_spec::wait::{MUST_HAPPEN, until_within, within};
    use aterm_uds::process::{Birth, ProcessPresence, process_presence, read_birth};
    use aterm_uds::runs::Kernel as _;
    use aterm_uds::spawnfd::{Child, command};
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn publish(path: &Path, text: &str) {
        aterm_uds::atomic_file::write_flushed(path, text.as_bytes(), 0o600, None).unwrap();
    }

    fn wait_for(path: &Path) {
        until_within(MUST_HAPPEN, "the fixture's complete marker", || {
            std::fs::read_to_string(path).ok()
        });
    }

    if let Some(code) = aterm_uds::tether::entry() {
        std::process::exit(if code == std::process::ExitCode::SUCCESS {
            0
        } else {
            1
        });
    }
    let exe = std::env::current_exe().unwrap();
    let role = std::env::var("LAND_WATCHERS_ROLE").unwrap_or_default();
    if role == "leaf" {
        let dir = PathBuf::from(std::env::var_os("LAND_WATCHERS_DIR").unwrap());
        // SAFETY: getppid has no arguments and reads this fixture's actual parent.
        let verifier = unsafe { libc::getppid() };
        publish(
            &dir.join("ready"),
            &format!("{verifier} {}", std::process::id()),
        );
        wait_for(&dir.join("finish"));
        return;
    }
    if role == "parent" {
        let dir = PathBuf::from(std::env::var_os("LAND_WATCHERS_DIR").unwrap());
        let verifier = aterm_tempfile::bin_exe("aterm-verify");
        let payload = exe.to_str().expect("this fixture's executable path");
        // A judgment's command before its words ([`aterm_verify::land::judgment_command`]):
        // the same tether, group and runtime, here running the payload, which carries a
        // judgment's `--events` words so the case can tell its own processes by them.
        let mut child = aterm_verify::land::verifier_command(
            &verifier,
            &dir,
            &aterm_verify::EnvSnapshot::capture(),
        )
        .unwrap()
        .arg(payload)
        .args(["--base", "fixture-base", "--events"])
        .arg(dir.join("events"))
        .args([
            aterm_capacity::Admit::FLAG,
            aterm_capacity::Admit::Wait.word(),
        ])
        // The real CLI's existing record-only compiler wrapper runs the payload after its
        // ordinary early startup. The payload's receipt proves that main reached dispatch.
        .env("ATERM_VERIFY_UNIT_WRAPPER", "1")
        .env("ATERM_VERIFY_UNIT_BOOK", dir.join("units.json"))
        .env("ATERM_VERIFY_UNIT_CHECKOUT", &dir)
        .env("ATERM_VERIFY_UNIT_MODE", "record")
        .env("ATERM_VERIFY_UNIT_PENDING", dir.join("pending.json"))
        .env("ATERM_VERIFY_UNIT_RUN", "fixture")
        .env("ATERM_VERIFY_UNIT_STAGE", "TestCompile unit")
        .env("ATERM_VERIFY_UNIT_UNKNOWN", (16_u64 << 30).to_string())
        .env("LAND_WATCHERS_ROLE", "leaf")
        .spawn()
        .unwrap();
        wait_for(&dir.join("go"));
        if std::env::var_os("LAND_WATCHERS_INTERRUPT").is_some() {
            aterm_verify::land::interrupt_judgment_awaiting(&mut child, MUST_HAPPEN);
        } else {
            publish(&dir.join("finish"), "finish");
            let status = until_within(MUST_HAPPEN, "the judgment's owning reap", || {
                child.try_wait().transpose()
            })
            .unwrap();
            assert!(status.success(), "the normal judgment: {status}");
        }
        publish(&dir.join("retired"), "retired");
        loop {
            std::thread::park();
        }
    }

    struct Owned(Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            aterm_uds::deadline::kill_within(&mut self.0, MUST_HAPPEN);
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct Observed {
        pid: i32,
        birth: Birth,
    }
    impl Observed {
        fn ended(self) -> bool {
            matches!(
                process_presence(self.pid),
                ProcessPresence::Absent | ProcessPresence::Exited
            ) || read_birth(self.pid).is_some_and(|birth| birth != self.birth)
        }
    }

    struct Case {
        _scratch: aterm_tempfile::TempDir,
        parent: Owned,
        bystander: Owned,
        helpers: Vec<Observed>,
        dir: PathBuf,
        interrupt: bool,
    }
    let start = |interrupt| {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dir = scratch.path().to_path_buf();
        let bystander = Owned(command("/bin/sleep").arg("600").spawn().unwrap());
        let mut parent = command(&exe);
        parent
            .env("LAND_WATCHERS_ROLE", "parent")
            .env("LAND_WATCHERS_DIR", &dir);
        if interrupt {
            parent.env("LAND_WATCHERS_INTERRUPT", "1");
        }
        let parent = Owned(parent.spawn().unwrap());
        wait_for(&dir.join("ready"));
        let ready: Vec<i32> = std::fs::read_to_string(dir.join("ready"))
            .unwrap()
            .split_whitespace()
            .map(|word| word.parse().unwrap())
            .collect();
        let [judgment, leaf] = ready.as_slice() else {
            panic!("the leaf names its verifier and itself: {ready:?}");
        };
        let events = dir.join("events");
        // Both the judgment and its already-armed watcher forks carry these exact words.
        // No name, parent PID after adoption, or unrelated process can stand in for them.
        let helpers: Vec<_> = aterm_uds::runs::LiveKernel
            .uid_pids()
            .expect("the fixture's process listing")
            .into_iter()
            .filter_map(|pid| {
                if pid == *leaf {
                    return None;
                }
                let birth = read_birth(pid)?;
                let argv = aterm_uds::process::read_process_argv(pid)?;
                let own = argv
                    .windows(2)
                    .any(|pair| pair[0] == OsStr::new("--events") && pair[1] == events.as_os_str());
                (own && read_birth(pid) == Some(birth)).then_some(Observed { pid, birth })
            })
            .collect();
        assert!(helpers.iter().any(|helper| helper.pid == *judgment));
        assert!(
            helpers.iter().any(|helper| helper.pid != *judgment),
            "the live judgment has an armed lifetime watcher: {helpers:?}"
        );
        Case {
            _scratch: scratch,
            parent,
            bystander,
            helpers,
            dir,
            interrupt,
        }
    };
    let mut cases = [start(false), start(true)];
    for case in &cases {
        publish(&case.dir.join("go"), "go");
    }
    for case in &cases {
        wait_for(&case.dir.join("retired"));
    }
    let retired = within(MUST_HAPPEN, Duration::from_millis(10), || {
        cases
            .iter()
            .all(|case| case.helpers.iter().all(|helper| helper.ended()))
            .then_some(())
    })
    .is_some();
    let results: Vec<_> = cases
        .iter_mut()
        .map(|case| {
            assert!(case.parent.0.try_wait().unwrap().is_none());
            assert!(case.bystander.0.try_wait().unwrap().is_none());
            let pending: Vec<_> = case
                .helpers
                .iter()
                .filter(|helper| !helper.ended())
                .copied()
                .collect();
            (case.interrupt, pending)
        })
        .collect();
    // End only owned parents/bystanders. Their watches clean up the captured helpers;
    // no late signal addresses a captured numeric PID, which may have been reused.
    for case in &mut cases {
        aterm_uds::deadline::kill_within(&mut case.parent.0, MUST_HAPPEN);
        aterm_uds::deadline::kill_within(&mut case.bystander.0, MUST_HAPPEN);
    }
    until_within(
        MUST_HAPPEN,
        "the fixture's helpers after parent cleanup",
        || {
            cases
                .iter()
                .all(|case| case.helpers.iter().all(|helper| helper.ended()))
                .then_some(())
        },
    );
    assert!(
        retired,
        "judgment helpers outlived retirement while land stayed alive (interrupt, pending): {results:?}"
    );
    println!(
        "land watchers: normal and interrupted judgments retired every captured lifetime helper while land and a bystander stayed alive"
    );
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("land watchers: the native Darwin anchor regression is not run on this target");
}
