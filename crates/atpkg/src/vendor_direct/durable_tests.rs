// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn dir(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-vendor-durable-{label}-"))
}

#[test]
fn windows_replace_waits_for_a_scheduled_file_holder() {
    let d = dir("scheduled-windows-replace");
    let staged = d.join("staged");
    let dest = d.join("dest");
    std::fs::write(&staged, b"complete").unwrap();
    std::fs::write(&dest, b"before").unwrap();
    let mut calls = 0;
    let result = again_while_refused_windows_at(
        || {
            calls += 1;
            if calls <= 21 {
                Err(io::ErrorKind::PermissionDenied.into())
            } else {
                std::fs::rename(&staged, &dest)
            }
        },
        std::time::Instant::now() - Duration::from_secs(1),
        |_| {},
    );
    result.expect("the Windows holder releases before the hang detector");
    assert_eq!(calls, 22);
    assert_eq!(std::fs::read(&dest).unwrap(), b"complete");
    assert!(!staged.exists());
}

#[test]
fn windows_refusal_expiry_keeps_the_error_and_unpublished_bytes() {
    let d = dir("windows-refusal");
    let staged = d.join("staged");
    let dest = d.join("dest");
    std::fs::write(&staged, b"complete").unwrap();
    std::fs::write(&dest, b"before").unwrap();
    for (kind, elapsed) in [
        (io::ErrorKind::PermissionDenied, Duration::from_secs(61)),
        (io::ErrorKind::NotFound, Duration::ZERO),
    ] {
        let mut calls = 0;
        let out: io::Result<()> = again_while_refused_windows_at(
            || {
                calls += 1;
                Err(io::Error::new(kind, std::fmt::Error))
            },
            std::time::Instant::now() - elapsed,
            |_| panic!("an expired wait or unrelated error must not wait again"),
        );
        let error = out.unwrap_err();
        assert_eq!(error.kind(), kind);
        assert!(error.get_ref().unwrap().is::<std::fmt::Error>());
        assert_eq!(calls, 1);
    }
    assert_eq!(std::fs::read(&staged).unwrap(), b"complete");
    assert_eq!(std::fs::read(&dest).unwrap(), b"before");
}

#[test]
fn digest_update_waits_for_a_scheduled_native_lock_holder() {
    let d = dir("scheduled-lock");
    let dest = d.join("x.toml");
    std::fs::write(&dest, b"unchanged").unwrap();
    let mut previous = Some(lock_for_update(&dest).unwrap());
    let held = lock_for_update_with(&dest, Duration::from_secs(6), |file, left, lock| {
        assert!(
            matches!(
                Flock::try_lock(file.try_clone().unwrap()),
                Err(std::fs::TryLockError::WouldBlock)
            ),
            "CONTROL: the native predecessor still owns the rendezvous"
        );
        drop(previous.take());
        Flock::lock_within(file, left, lock)
    })
    .expect("a scheduled native holder may finish before the hang detector");
    assert_eq!(std::fs::read(&dest).unwrap(), b"unchanged");
    let late = lock_for_update_after(&dest, Duration::from_secs(61))
        .err()
        .expect("expired");
    assert_eq!(late.kind(), io::ErrorKind::TimedOut);
    assert!(
        matches!(
            Flock::try_lock(open_lock(&d.join("x.toml.lock"), true).unwrap()),
            Err(std::fs::TryLockError::WouldBlock)
        ),
        "the exhausted wait preserves the actual native holder"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"unchanged");
    drop(held);
}

#[test]
fn a_staged_file_accepts_a_refused_flush_after_writing_its_body() {
    let d = dir("refused-flush");
    let tmp = d.join("stage");
    stage_with(&tmp, b"a = 1\n", |_| {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    })
    .unwrap();
    assert_eq!(std::fs::read(tmp).unwrap(), b"a = 1\n");
}

/// A raw Darwin refusal that no std names by a refusal kind, `ENODEV` (what a full flush
/// of `/dev/null` answers), is accepted too: staging reads astream's rule, not a list of
/// kinds. Darwin's `ENOTSUP` (45) would not show that: the Trust std reads it as
/// `Unsupported`, the kind the test above feeds. CONTROL: `ENODEV`'s kind alone is no
/// refusal.
#[cfg(target_vendor = "apple")]
#[test]
fn a_staged_file_accepts_a_darwin_raw_flush_refusal() {
    let enodev = io::Error::from_raw_os_error(libc::ENODEV);
    assert!(
        aterm_durable::refusal_accepted(Err(io::Error::from(enodev.kind()))).is_err(),
        "CONTROL: std names ENODEV by kind ({enodev:?}); the errno half is untested"
    );
    let d = dir("raw-refused-flush");
    let tmp = d.join("stage");
    stage_with(&tmp, b"a = 1\n", |_| {
        Err(io::Error::from_raw_os_error(libc::ENODEV))
    })
    .unwrap();
    assert_eq!(std::fs::read(tmp).unwrap(), b"a = 1\n");
}

#[test]
fn a_staged_file_reports_a_failed_flush_after_writing_its_body() {
    let d = dir("failed-flush");
    let tmp = d.join("stage");
    let error =
        stage_with(&tmp, b"a = 1\n", |_| Err(io::Error::other(std::fmt::Error))).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(error.get_ref().unwrap().is::<std::fmt::Error>());
    assert_eq!(std::fs::read(tmp).unwrap(), b"a = 1\n");
}

#[test]
fn accepting_a_flush_refusal_does_not_accept_a_failed_stage_open() {
    let d = dir("occupied-stage");
    let tmp = d.join("stage");
    std::fs::write(&tmp, b"keep").unwrap();
    let error = stage_with(&tmp, b"replace", |_| {
        panic!("an unsuccessful open cannot reach the flush")
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    let error = stage_with(&d.join("stage\0"), b"replace", |_| {
        panic!("an invalid path cannot reach the flush")
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(std::fs::read(tmp).unwrap(), b"keep");
}

#[test]
fn each_replace_names_its_flush() {
    use aterm_durable::{Flush, seam::Recording};
    let d = dir("flush-tiers");
    let dest = d.join("x.toml");
    let recording = Recording::start();
    write_durable(&dest, b"a = 1\n").unwrap();
    // The directory's flush is recorded everywhere (on Windows `Flush::dir` only notes
    // it). The temp's durable flush is recorded on Windows (`durable_data`) and not on
    // Unix, where it is atpkg's own `fsync`.
    let durable: &[Flush] = if cfg!(windows) {
        &[Flush::Durable, Flush::Durable]
    } else {
        &[Flush::Durable]
    };
    assert_eq!(recording.take(), durable);
    write_ordered(&dest, b"a = 2\n").unwrap();
    assert_eq!(recording.take(), [Flush::Ordered, Flush::Ordered]);
    assert_eq!(std::fs::read(&dest).unwrap(), b"a = 2\n");
}

#[test]
fn a_leftover_temp_is_replaced_or_ignored() {
    let d = dir("leftover");
    let dest = d.join("x.toml");
    let stale = temp_path(&dest, u64::MAX).unwrap();
    std::fs::write(&stale, b"garbage garbage garbage garbage").unwrap();
    let foreign = d.join(".x.toml.tmp-1-0");
    std::fs::write(&foreign, b"garbage").unwrap();
    replace_via(&stale, &dest, b"a = 1\n").unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"a = 1\n");
    assert!(
        !stale.exists(),
        "the stale temp was replaced and renamed into place"
    );
    write_durable(&dest, b"a = 2\n").unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"a = 2\n");
    assert!(foreign.exists(), "another pid's temp is left alone");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
    }
}

#[test]
fn a_failed_replace_leaves_no_temp() {
    let d = dir("fail");
    let dest = d.join("x.toml");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("keep"), b"").unwrap();
    assert!(write_durable(&dest, b"a = 1\n").is_err());
    let temps = std::fs::read_dir(&d)
        .unwrap()
        .filter(|e| {
            let name = e.as_ref().unwrap().file_name();
            name.to_str().unwrap().starts_with(".x.toml.tmp-")
        })
        .count();
    assert_eq!(temps, 0);
    assert!(dest.join("keep").exists());
}

#[test]
fn concurrent_writers_never_tear_the_file() {
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    let d = dir("race");
    let dest = d.join("x.toml");
    let a = vec![b'A'; 256 * 1024];
    let b = vec![b'B'; 128 * 1024];
    write_durable(&dest, &a).unwrap();
    let done = AtomicBool::new(false);
    let torn = AtomicUsize::new(0);
    std::thread::scope(|s| {
        let writers: Vec<_> = (0..8)
            .map(|i| {
                let (dest, body) = (&dest, if i % 2 == 0 { &a } else { &b });
                s.spawn(move || {
                    for _ in 0..100 {
                        write_durable(dest, body).unwrap();
                    }
                })
            })
            .collect();
        let reader = s.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                let got = read_durable(&dest, 1024 * 1024).unwrap().into_bytes();
                if got != a && got != b {
                    torn.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        let written: Vec<_> = writers.into_iter().map(|w| w.join()).collect();
        done.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        for result in written {
            result.unwrap();
        }
    });
    assert_eq!(torn.load(Ordering::Relaxed), 0, "a reader saw a torn file");
    let last = std::fs::read(&dest).unwrap();
    assert!(last == a || last == b);
}

#[test]
fn a_missing_file_is_read_again_once_a_replace_settles() {
    let d = dir("settled");
    let dest = d.join("x.toml");
    let mut asked = 0;
    let got = read_settled(&dest, 64, |path| {
        asked += 1;
        // The replace a reader waited out lands its file before the guard is granted.
        std::fs::write(path, b"a = 1\n")?;
        Ok(Some(()))
    })
    .unwrap();
    assert_eq!((got.as_str(), asked), ("a = 1\n", 1));
    let present = read_settled(&dest, 64, |_| -> io::Result<Option<()>> {
        unreachable!("a file that is there is never waited for")
    });
    assert_eq!(present.unwrap(), "a = 1\n");
}

#[test]
fn a_missing_file_no_replace_ever_named_stays_missing() {
    let d = dir("never-replaced");
    let dest = d.join("x.toml");
    let err = read_settled(&dest, 64, |_| Ok(None::<()>)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    let refused = read_settled(&dest, 64, |_| -> io::Result<Option<()>> {
        Err(io::Error::new(io::ErrorKind::TimedOut, "held"))
    })
    .unwrap_err();
    assert_eq!(
        refused.kind(),
        io::ErrorKind::TimedOut,
        "a replace never settled is no answer about the file"
    );
}

#[cfg(windows)]
#[test]
fn a_windows_replace_leaves_its_rendezvous_for_readers() {
    let d = dir("rendezvous");
    let dest = d.join("x.toml");
    assert!(
        replace_settled(&dest).unwrap().is_none(),
        "nothing replaced yet"
    );
    write_durable(&dest, b"a = 1\n").unwrap();
    let settled = replace_settled(&dest)
        .unwrap()
        .expect("the writer's lock is left");
    let held = Flock::try_lock(open_lock(&replace_lock(&dest).unwrap(), false).unwrap());
    assert!(
        matches!(held, Err(std::fs::TryLockError::WouldBlock)),
        "a reader's shared hold keeps a writer's rename out"
    );
    drop(settled);
    assert_eq!(read_durable(&dest, 64).unwrap(), "a = 1\n");
}

#[test]
fn a_dropped_update_lock_is_free_while_a_copy_of_its_descriptor_lives() {
    let d = dir("lock-dup");
    let dest = d.join("x.toml");
    let held = lock_for_update(&dest).unwrap();
    let childs_copy = held.try_clone().unwrap();
    drop(held);
    let again = lock_for_update(&dest).expect("free while its copy lives");
    drop((again, childs_copy));
}

#[test]
fn a_held_update_lock_times_out() {
    let d = dir("lock-held");
    let dest = d.join("x.toml");
    let _held = lock_for_update(&dest).unwrap();
    let err = lock_for_update_after(&dest, Duration::from_secs(61))
        .err()
        .expect("held");
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
}

#[cfg(unix)]
#[test]
fn lock_for_update_refuses_symlink_lock_file() {
    let d = dir("lock-symlink");
    let dest = d.join("x.toml");
    let victim = d.join("victim");
    std::fs::write(&victim, b"untouched").unwrap();
    std::os::unix::fs::symlink(&victim, d.join("x.toml.lock")).unwrap();
    assert!(lock_for_update(&dest).is_err());
}
