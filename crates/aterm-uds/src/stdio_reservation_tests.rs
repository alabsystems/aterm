// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::spawnfd::tests::STAGE_ENV;
use std::cell::Cell;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

fn while_reserved<T>(action: impl FnOnce() -> T) -> io::Result<T> {
    Admission::enter().while_reserved(action)
}

fn flags(fd: i32, operation: i32) -> i32 {
    // SAFETY: these operations only read flags for this staged process's own numbers.
    unsafe { libc::fcntl(fd, operation) }
}

fn closed() {
    for target in 0..3 {
        assert_eq!(flags(target, libc::F_GETFD), -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
    }
}

fn isolated(name: &str, body: impl FnOnce(&Path)) {
    if std::env::var(STAGE_ENV).as_deref() != Ok("stdio-reservation") {
        let mut command = crate::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env(STAGE_ENV, "stdio-reservation");
        let output = crate::deadline::output_while_process_progressing_complete_with_limit(
            &mut command,
            aterm_spec::wait::MUST_HAPPEN,
            name,
            64 * 1024,
        )
        .expect("the isolated reservation role");
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("STAGED stdio reservation ok"), "{stdout}");
        return;
    }
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("opaque-owner");
    std::fs::write(&path, b"retained owner").unwrap();
    let saved: [OwnedFd; 3] = std::array::from_fn(|target| {
        // SAFETY: duplicate each open role stdio above every stdio slot; the copy is fresh.
        let fd = unsafe { libc::fcntl(i32::try_from(target).unwrap(), libc::F_DUPFD_CLOEXEC, 3) };
        assert!(fd >= 3);
        // SAFETY: this fresh duplicate has no other owner.
        unsafe { OwnedFd::from_raw_fd(fd) }
    });
    for target in 0..3 {
        // SAFETY: only this isolated role uses its selected stdio during the body.
        assert_eq!(unsafe { libc::close(target) }, 0);
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&path)));
    for (target, from) in saved.iter().enumerate() {
        // SAFETY: restore this role's stdio from copies kept live throughout the body.
        assert_eq!(
            unsafe { libc::dup2(from.as_raw_fd(), i32::try_from(target).unwrap()) },
            i32::try_from(target).unwrap()
        );
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    println!("STAGED stdio reservation ok");
}

#[test]
fn reservations_admit_retained_owners_above_directional_cloexec_stdio() {
    const NAME: &str = "spawnfd::stdio_reservation::tests::reservations_admit_retained_owners_above_directional_cloexec_stdio";
    isolated(NAME, |path| {
        let null = std::fs::metadata("/dev/null").unwrap();
        let retained = while_reserved(|| {
            for target in 0..3 {
                assert_ne!(flags(target, libc::F_GETFD) & libc::FD_CLOEXEC, 0);
                assert_eq!(
                    flags(target, libc::F_GETFL) & libc::O_ACCMODE,
                    if target == 0 {
                        libc::O_RDONLY
                    } else {
                        libc::O_WRONLY
                    }
                );
                // SAFETY: fstat initializes this live plain record from a reserved open slot.
                let mut stat: libc::stat = unsafe { std::mem::zeroed() };
                // SAFETY: this stage's live reserved descriptor and writable output record.
                assert_eq!(unsafe { libc::fstat(target, &mut stat) }, 0);
                assert_eq!(stat.st_mode & libc::S_IFMT, libc::S_IFCHR);
                assert_eq!(stat.st_rdev, null.rdev());
            }
            std::fs::File::open(path).unwrap()
        })
        .unwrap();
        assert!(retained.as_raw_fd() > 2);
        closed();
        assert_eq!(
            retained.metadata().unwrap().ino(),
            std::fs::metadata(path).unwrap().ino()
        );
    });
}

#[test]
fn partial_admission_errors_and_action_unwinds_restore_only_owned_slots() {
    const NAME: &str = "spawnfd::stdio_reservation::tests::partial_admission_errors_and_action_unwinds_restore_only_owned_slots";
    isolated(NAME, |_| {
        let called = Cell::new(false);
        let error = Reservation::with(
            |target| {
                if target == 2 {
                    Err(io::Error::from_raw_os_error(libc::EIO))
                } else {
                    open_null(target)
                }
            },
            || called.set(true),
        )
        .err()
        .expect("partial admission refused");
        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert!(!called.get(), "opaque action ran after admission failed");
        closed();
        let panic =
            std::panic::catch_unwind(|| while_reserved(|| panic!("injected opaque unwind")));
        assert!(panic.is_err());
        closed();
        while_reserved(|| assert_ne!(flags(0, libc::F_GETFD), -1)).unwrap();
        closed();
    });
}

#[test]
fn a_raced_foreign_slot_and_an_existing_open_file_are_never_closed_or_replaced() {
    const NAME: &str = "spawnfd::stdio_reservation::tests::a_raced_foreign_slot_and_an_existing_open_file_are_never_closed_or_replaced";
    isolated(NAME, |path| {
        let expected = std::fs::metadata(path).unwrap();
        let mut foreign = None;
        let called = Cell::new(false);
        let error = Reservation::with(
            |target| {
                if target == 1 {
                    let file = std::fs::File::open(path)?;
                    assert_eq!(file.as_raw_fd(), 1);
                    foreign = Some(file);
                }
                open_null(target)
            },
            || called.set(true),
        )
        .err()
        .expect("raced admission refused");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(!called.get());
        assert_eq!(flags(0, libc::F_GETFD), -1);
        assert_eq!(flags(2, libc::F_GETFD), -1);
        let foreign = foreign.unwrap();
        assert_eq!(foreign.metadata().unwrap().dev(), expected.dev());
        assert_eq!(foreign.metadata().unwrap().ino(), expected.ino());
        assert_eq!(flags(1, libc::F_GETFD), libc::FD_CLOEXEC);
        drop(foreign);
        closed();

        let original = std::fs::File::open(path).unwrap();
        assert_eq!(original.as_raw_fd(), 0);
        let before = flags(0, libc::F_GETFD);
        let retained = while_reserved(|| std::fs::File::open(path).unwrap()).unwrap();
        assert!(retained.as_raw_fd() > 2);
        assert_eq!(original.metadata().unwrap().dev(), expected.dev());
        assert_eq!(original.metadata().unwrap().ino(), expected.ino());
        assert_eq!(flags(0, libc::F_GETFD), before);
        assert_eq!(flags(1, libc::F_GETFD), -1);
        assert_eq!(flags(2, libc::F_GETFD), -1);
        drop(original);
        drop(retained);
        closed();
    });
}

#[test]
fn one_admission_outlives_its_kernel_file_and_reservation_action_before_another_enters() {
    const NAME: &str = "spawnfd::stdio_reservation::tests::one_admission_outlives_its_kernel_file_and_reservation_action_before_another_enters";
    isolated(NAME, |path| {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex, TryLockError, mpsc};
        const HANG: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
        struct Actors {
            release: Option<mpsc::Sender<()>>,
            threads: Vec<std::thread::JoinHandle<Option<String>>>,
        }
        impl Drop for Actors {
            fn drop(&mut self) {
                if let Some(release) = self.release.take() {
                    let _ = release.send(());
                }
                while let Some(thread) = self.threads.pop() {
                    let _ = thread.join();
                }
            }
        }
        for hold_kernel_file in [true, false] {
            let (entered, heard) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let (waiting, contended) = mpsc::channel();
            let allowed = Arc::new(AtomicBool::new(false));
            let retained = Arc::new(Mutex::new(None::<std::fs::File>));
            let first_path = path.to_owned();
            let mut actors = Actors {
                release: Some(release),
                threads: Vec::new(),
            };
            actors.threads.push(std::thread::spawn(move || {
                crate::process::translator_probe(
                    Admission::enter,
                    || {
                        let kernel = std::fs::File::open(first_path).unwrap();
                        assert_eq!(kernel.as_raw_fd(), 0);
                        if hold_kernel_file {
                            entered.send(()).unwrap();
                            released.recv_timeout(HANG).unwrap();
                        }
                        drop(kernel);
                        Ok("other-architecture".to_string())
                    },
                    || {
                        if !hold_kernel_file {
                            entered.send(()).unwrap();
                            released.recv_timeout(HANG).unwrap();
                        }
                        false
                    },
                )
            }));
            heard.recv_timeout(HANG).unwrap();
            let second_path = path.to_owned();
            let second_allowed = Arc::clone(&allowed);
            let second_retained = Arc::clone(&retained);
            actors.threads.push(std::thread::spawn(move || {
                crate::process::translator_probe(
                    || {
                        Admission::enter_with(|| {
                            match ADMISSION.try_lock() {
                                Err(TryLockError::WouldBlock) => {}
                                _ => panic!(
                                    "the first admission released before its input/action ended"
                                ),
                            }
                            waiting.send(()).unwrap();
                            ADMISSION
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                        })
                    },
                    || {
                        assert!(
                            second_allowed.load(Ordering::Acquire),
                            "kernel reader ran before admission"
                        );
                        let kernel = std::fs::File::open(&second_path).unwrap();
                        assert_eq!(
                            kernel.as_raw_fd(),
                            0,
                            "prior input/reservations have retired"
                        );
                        drop(kernel);
                        Ok("other-architecture".to_string())
                    },
                    || {
                        for target in 0..3 {
                            assert_ne!(flags(target, libc::F_GETFD), -1);
                        }
                        let owner = std::fs::File::open(&second_path).unwrap();
                        assert!(owner.as_raw_fd() > 2);
                        *second_retained.lock().unwrap() = Some(owner);
                        true
                    },
                )
            }));
            contended.recv_timeout(HANG).unwrap();
            allowed.store(true, Ordering::Release);
            actors.release.take().unwrap().send(()).unwrap();
            while let Some(thread) = actors.threads.pop() {
                assert!(thread.join().unwrap().is_some());
            }
            closed();
            let owner = retained.lock().unwrap().take().unwrap();
            assert!(owner.as_raw_fd() > 2);
            assert_eq!(
                owner.metadata().unwrap().ino(),
                std::fs::metadata(path).unwrap().ino()
            );
            drop(owner);
        }
    });
}
