// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

// File descriptors belong to the process, not the test thread. A concurrent
// spawn may temporarily inherit another fixture's locked description before
// exec closes it, so each kernel fixture owns the process's descriptor table
// through its final close and child reap. Child fixtures exec a fresh test
// process and therefore have their own guard.
#[cfg(unix)]
pub(crate) static KERNEL_FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(unix)]
pub(crate) fn kernel_fixture() -> std::sync::MutexGuard<'static, ()> {
    KERNEL_FIXTURE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn records_round_trip_and_refuse_unbounded_or_ambiguous_input() {
    for birth in [None, Some(0), Some(u64::MAX)] {
        let record = Record { slot: 7, owner: Owner { pid: 42, birth } };
        assert_eq!(Record::parse(&record.encode()).unwrap(), record);
    }
    for text in [
        "trust-build-cpu-v2 slot=0 pid=1 birth=- held=1\n",
        "trust-build-cpu-v1 slot=0 pid=0 birth=- held=1\n",
        "trust-build-cpu-v1 slot=0 pid=1 birth=- held=0\n",
        "trust-build-cpu-v1 slot=+0 pid=1 birth=- held=1\n",
        "trust-build-cpu-v1 slot=00 pid=1 birth=- held=1\n",
        "trust-build-cpu-v1 slot=0 pid=1 birth=- held=1\n\n",
        "trust-build-cpu-v1 slot=0 pid=1 birth=- held=1 extra=1\n",
        "trust-build-cpu-v1 slot=0 pid=1 birth=- held=1",
    ] {
        assert!(Record::parse(text).is_err(), "accepted {text:?}");
    }
    assert!(Record::parse(&"x".repeat(MAX_RECORD_BYTES + 1)).is_err());
}

#[cfg(unix)]
mod unix {
    use std::io::{BufRead, Read, Write};
    use std::num::NonZeroU32;
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    fn width(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).unwrap()
    }
    fn owner() -> Owner {
        Owner::current(Some(123))
    }

    #[test]
    fn independent_pool_handles_share_exact_unit_capacity() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let first = Pool::open_in(dir.path(), width(2)).unwrap();
        let second = Pool::open_in(dir.path(), width(2)).unwrap();
        let a = first.try_acquire(owner()).unwrap().unwrap();
        let b = second.try_acquire(owner()).unwrap().unwrap();
        assert_ne!(a.slot(), b.slot());
        assert!(first.try_acquire(owner()).unwrap().is_none());
        assert!(
            second.snapshot().unwrap().iter().all(|state| matches!(state, SlotState::Held { .. }))
        );
        // An inherited duplicate still consumes a's slot even after this
        // process drops a. The independent handle cannot reuse it until the
        // child proves it started, closes the descriptor and is reaped.
        let mut child = start("inherit", dir.path(), Some(a.try_clone().unwrap().into_file()));
        drop(a);
        assert!(second.try_acquire(owner()).unwrap().is_none());
        child.0.stdin.as_mut().unwrap().write_all(b"x").unwrap();
        assert!(wait(&mut child).success());
        let c = second.try_acquire(owner()).unwrap().unwrap();
        assert_ne!(b.slot(), c.slot());
        drop((b, c));
        assert!(
            first
                .snapshot()
                .unwrap()
                .iter()
                .all(|state| matches!(state, SlotState::Available { .. }))
        );
    }

    #[test]
    fn duplicate_descriptor_keeps_lease_until_final_close() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), width(1)).unwrap();
        let lease = pool.try_acquire(owner()).unwrap().unwrap();
        let duplicate = lease.try_clone().unwrap();
        drop(lease);
        assert!(pool.try_acquire(owner()).unwrap().is_none());
        drop(duplicate);
        assert!(pool.try_acquire(owner()).unwrap().is_some());
    }

    #[test]
    fn opening_an_existing_pool_refuses_an_active_initializer_without_waiting() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), width(1)).unwrap();
        let held =
            std::fs::OpenOptions::new().write(true).open(pool.path().join("init.lock")).unwrap();
        held.lock().unwrap();
        assert!(Pool::open_in(dir.path(), width(1)).is_err());
        held.unlock().unwrap();
        assert!(Pool::open_in(dir.path(), width(1)).is_ok());
    }

    #[test]
    fn stale_unlocked_metadata_is_available_but_corrupt_held_metadata_is_busy() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), width(1)).unwrap();
        let mut file = pool.try_acquire(owner()).unwrap().unwrap().into_file();
        file.set_len(0).unwrap();
        file.write_all(&vec![b'x'; MAX_RECORD_BYTES + 1]).unwrap();
        assert_eq!(pool.snapshot().unwrap(), [SlotState::Held { slot: 0, reported_owner: None }]);
        drop(file);
        assert_eq!(pool.snapshot().unwrap(), [SlotState::Available { slot: 0 }]);
        let lease = pool.try_acquire(owner()).unwrap().unwrap();
        assert_eq!(
            pool.snapshot().unwrap(),
            [SlotState::Held { slot: 0, reported_owner: Some(owner()) }]
        );
        drop(lease);
    }

    #[test]
    fn pool_width_cannot_change_under_existing_users() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), width(1)).unwrap();
        assert_eq!(
            Pool::open_in(dir.path(), width(2)).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(pool.slots(), width(1));
    }

    #[test]
    fn observation_does_not_create_or_repair_a_pool() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("absent");
        assert!(Pool::open_existing(&absent).unwrap().is_none());
        assert!(!absent.exists());
        assert!(Pool::open_existing(dir.path()).unwrap().is_none());
        assert!(!dir.path().join(DIRECTORY_NAME).exists());
        let pool = Pool::open_in(dir.path(), width(2)).unwrap();
        let read = Pool::open_existing(dir.path()).unwrap().unwrap();
        assert_eq!(read.slots(), width(2));
        assert_eq!(pool.snapshot().unwrap(), read.snapshot().unwrap());
    }

    #[test]
    fn readable_capacity_parent_does_not_require_permission_rewriting() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let pool = Pool::open_in(dir.path(), width(1)).unwrap();
        assert_eq!(std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::metadata(pool.path()).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(pool.try_acquire(owner()).unwrap().is_some());
    }

    #[test]
    fn replacing_a_leased_slot_never_creates_duplicate_capacity() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), width(1)).unwrap();
        let lease = pool.try_acquire(owner()).unwrap().unwrap();
        let slot = pool.path().join("slot-00000000");
        std::fs::rename(&slot, pool.path().join("former-slot")).unwrap();
        std::fs::write(&slot, b"").unwrap();
        std::fs::set_permissions(&slot, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(pool.try_acquire(owner()).is_err());
        assert!(pool.snapshot().is_err());
        assert!(Pool::open_in(dir.path(), width(1)).is_err());
        assert!(Pool::open_existing(dir.path()).is_err());
        drop(lease);
    }

    #[test]
    fn readers_report_initialization_without_waiting_for_its_lock() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), width(1)).unwrap();
        let path = pool.path().join("pool");
        let before = std::fs::read(&path).unwrap();
        let initialization =
            std::fs::OpenOptions::new().write(true).open(pool.path().join("init.lock")).unwrap();
        initialization.lock().unwrap();
        let mut config = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        config.set_len(0).unwrap();
        config.write_all(b"partial initialization").unwrap();
        let root = dir.path().to_owned();
        let (send, receive) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let opened = Pool::open_existing(&root).unwrap_err().kind();
            let snapshot = pool.snapshot().unwrap_err().kind();
            send.send((opened, snapshot)).unwrap();
        });
        let result = receive
            .recv_timeout(Duration::from_secs(60))
            .expect("read-only pool observation must not wait on initialization");
        assert_eq!(result, (std::io::ErrorKind::WouldBlock, std::io::ErrorKind::WouldBlock));
        reader.join().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"partial initialization");
        config.set_len(0).unwrap();
        use std::io::{Seek, SeekFrom};
        config.seek(SeekFrom::Start(0)).unwrap();
        config.write_all(&before).unwrap();
        drop(config);
        drop(initialization);
        assert!(Pool::open_existing(dir.path()).unwrap().is_some());
    }

    #[test]
    fn interrupted_unpublished_initialization_is_not_repaired() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DIRECTORY_NAME);
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["slot-00000000", ".pool-init-dead"] {
            std::fs::write(path.join(name), b"incomplete unpublished data").unwrap();
            std::fs::set_permissions(path.join(name), std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        assert!(Pool::open_in(dir.path(), width(1)).is_err());
        assert!(!path.join("pool").exists());
    }

    #[test]
    fn existing_pool_opens_under_a_read_only_capacity_parent() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), width(2)).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = Pool::open_in(dir.path(), width(2));
        let mismatch = Pool::open_in(dir.path(), width(1));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let reopened = result.unwrap();
        assert_eq!(reopened.path(), pool.path());
        assert!(reopened.try_acquire(owner()).unwrap().is_some());
        assert!(mismatch.is_err());
    }

    #[test]
    fn unsafe_permissions_and_symlink_slots_are_refused_without_repair() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), width(1)).unwrap();
        let slot = pool.path().join("slot-00000000");
        std::fs::set_permissions(&slot, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(pool.try_acquire(owner()).is_err());
        assert_eq!(std::fs::metadata(&slot).unwrap().permissions().mode() & 0o777, 0o666);
        std::fs::remove_file(&slot).unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"keep this").unwrap();
        symlink(&victim, &slot).unwrap();
        assert!(pool.try_acquire(owner()).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep this");
    }

    #[test]
    fn hardlinked_slot_cannot_be_truncated_by_claiming() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), width(1)).unwrap();
        let slot = pool.path().join("slot-00000000");
        let alias = dir.path().join("alias");
        std::fs::hard_link(&slot, &alias).unwrap();
        assert!(pool.try_acquire(owner()).is_err());
        assert_eq!(std::fs::metadata(&alias).unwrap().len(), 0);
    }

    #[test]
    fn replaced_or_made_public_pool_is_refused() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), width(1)).unwrap();
        std::fs::set_permissions(pool.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(pool.snapshot().is_err());
        std::fs::set_permissions(pool.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::rename(pool.path(), dir.path().join("former-pool")).unwrap();
        std::fs::create_dir(pool.path()).unwrap();
        assert!(pool.try_acquire(owner()).is_err());
    }

    const MODE: &str = "TRUST_BUILD_CPU_TEST_MODE";
    const ROOT: &str = "TRUST_BUILD_CPU_TEST_ROOT";
    const FD: &str = "TRUST_BUILD_CPU_TEST_FD";
    const HELD: &str = "TRUST_BUILD_CPU_TEST_HELD";

    #[test]
    fn subprocess_holder() {
        let _fixture = kernel_fixture();
        let Ok(mode) = std::env::var(MODE) else { return };
        let _kept;
        let _lease;
        if mode == "inherit" {
            let fd = std::env::var(FD).unwrap().parse::<i32>().unwrap();
            // SAFETY: this child fixture owns the sole startup descriptor
            // deliberately inherited by its parent. No other Rust owner exists.
            _kept = Some(unsafe { std::fs::File::from_raw_fd(fd) });
            _lease = None;
        } else {
            let pool =
                Pool::open_in(std::path::Path::new(&std::env::var_os(ROOT).unwrap()), width(1))
                    .unwrap();
            _lease = Some(pool.try_acquire(Owner::current(None)).unwrap().unwrap());
            _kept = None;
        }
        println!("\n{HELD}");
        std::io::stdout().flush().unwrap();
        let mut byte = [0];
        let _ = std::io::stdin().read(&mut byte);
    }

    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    fn start(mode: &str, root: &std::path::Path, inherited: Option<std::fs::File>) -> Running {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "tests::unix::subprocess_holder", "--nocapture"])
            .env(MODE, mode)
            .env(ROOT, root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(file) = inherited {
            // The native stdio handoff keeps the inherited File alive without
            // a pre_exec hook: on macOS that hook forces fork, temporarily
            // copying every unrelated parallel test's lease before exec.
            // This fixture never writes stderr; stdout proves that it started.
            command.env(FD, "2").stderr(Stdio::from(file));
        }
        let mut child = Running(command.spawn().unwrap());
        let output = child.0.stdout.take().unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut notified = false;
            for line in std::io::BufReader::new(output).lines() {
                if !notified && line.as_deref().is_ok_and(|line| line == HELD) {
                    let _ = send.send(());
                    notified = true;
                }
            }
        });
        receive
            .recv_timeout(Duration::from_secs(60))
            .expect("child must prove that it holds the lease");
        child
    }

    fn wait(child: &mut Running) -> std::process::ExitStatus {
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < end, "child did not end within its hang detector");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn owner_death_releases_capacity_without_stale_record_cleanup() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), width(1)).unwrap();
        let mut child = start("acquire", dir.path(), None);
        assert!(pool.try_acquire(owner()).unwrap().is_none());
        child.0.kill().unwrap();
        assert!(!wait(&mut child).success());
        assert_eq!(pool.snapshot().unwrap(), [SlotState::Available { slot: 0 }]);
        assert!(pool.try_acquire(owner()).unwrap().is_some());
    }

    #[test]
    fn inherited_descriptor_outlives_parent_lease_and_releases_on_child_exit() {
        let _fixture = kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), width(1)).unwrap();
        let lease = pool.try_acquire(owner()).unwrap().unwrap();
        let mut child = start("inherit", dir.path(), Some(lease.try_clone().unwrap().into_file()));
        drop(lease);
        assert!(pool.try_acquire(owner()).unwrap().is_none());
        child.0.stdin.as_mut().unwrap().write_all(b"x").unwrap();
        assert!(wait(&mut child).success());
        assert!(pool.try_acquire(owner()).unwrap().is_some());
    }
}
