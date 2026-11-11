// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use super::DirFlushed;
use super::{
    Flush, durable, durable_data, durable_dir, ordered, ordered_dir, primitive, refusal_accepted,
    seam,
};
use std::io::{self, Write as _};

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("aterm-durable-")
        .tempdir()
        .expect("a scratch directory")
}

#[cfg(target_vendor = "apple")]
#[test]
fn diagnosed_flush_keeps_the_failed_system_call_and_errno() {
    let null = std::fs::File::open("/dev/null").unwrap();
    for flush in [Flush::Durable, Flush::Ordered] {
        let plain = flush.file(&null).unwrap_err();
        let diagnosed = flush.file_diagnosed(&null).unwrap_err();
        let failure = diagnosed
            .get_ref()
            .unwrap()
            .downcast_ref::<super::OperationError>()
            .unwrap();
        assert_eq!(failure.operation, "F_FULLFSYNC");
        assert_eq!(failure.error.raw_os_error(), plain.raw_os_error());
        assert!(failure.error.raw_os_error().is_some());
    }
    let dir = scratch();
    let file = std::fs::File::create(dir.path().join("file")).unwrap();
    for flush in [Flush::Durable, Flush::Ordered] {
        flush.file_diagnosed(&file).unwrap();
        flush.dir_diagnosed(dir.path()).unwrap();
    }
}

/// The seam's numbers are this platform's own: [`seam::EIO`] is a failed flush and
/// [`seam::ENOTSUP`] the volume's refusal, as astream reads them here (on Windows a POSIX
/// 5 would be `ERROR_ACCESS_DENIED`, read as unavailable).
#[test]
fn the_seams_numbers_are_a_failed_flush_and_a_refusal_here() {
    let error = io::Error::from_raw_os_error;
    assert!(!astream_sys::dir_flush_unavailable(&error(seam::EIO)));
    assert!(astream_sys::dir_flush_unavailable(&error(seam::ENOTSUP)));
    assert!(astream_sys::file_flush_refused(&error(seam::ENOTSUP)));
}

#[test]
fn directory_faults_preserve_strength_restore_nesting_and_stay_on_their_thread() {
    let dir = scratch();
    let recorded = seam::Recording::start();
    {
        let _failure = seam::FailingDirs::with_kind(io::ErrorKind::Other);
        assert_eq!(
            Flush::Ordered.dir(dir.path()).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        {
            let _refused = seam::FailingDirs::with_kind(io::ErrorKind::Unsupported);
            assert!(matches!(
                Flush::Durable.dir_best_effort(dir.path()),
                Ok(super::DirFlushed::Unavailable(_))
            ));
        }
        assert_eq!(
            Flush::Durable.dir(dir.path()).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        let path = dir.path().to_path_buf();
        std::thread::spawn(move || Flush::Ordered.dir(&path))
            .join()
            .unwrap()
            .unwrap();
    }
    Flush::Durable.dir(dir.path()).unwrap();
    {
        let _second = seam::FailingDirs::after(1, 5);
        Flush::Ordered.dir(dir.path()).unwrap();
        assert_eq!(
            Flush::Durable.dir(dir.path()).unwrap_err().raw_os_error(),
            Some(5)
        );
    }
    assert_eq!(
        recorded.take(),
        [
            Flush::Ordered,
            Flush::Durable,
            Flush::Durable,
            Flush::Durable,
            Flush::Ordered,
            Flush::Durable
        ]
    );
}

#[test]
fn the_seam_names_the_flush_each_operation_asked_for() {
    let dir = scratch();
    let mut file = std::fs::File::create(dir.path().join("f")).unwrap();
    file.write_all(b"bytes").unwrap();
    let recording = seam::Recording::start();
    ordered(&file).unwrap();
    durable(&file).unwrap();
    ordered_dir(dir.path()).unwrap();
    durable_dir(dir.path()).unwrap();
    Flush::Ordered.file(&file).unwrap();
    Flush::Durable.dir(dir.path()).unwrap();
    durable_data(&file).unwrap();
    let expected = [
        Flush::Ordered,
        Flush::Durable,
        Flush::Ordered,
        Flush::Durable,
        Flush::Ordered,
        Flush::Durable,
        Flush::Durable,
    ];
    assert_eq!(recording.take(), expected);
    assert_eq!(recording.take(), [], "a take empties the log");
}

#[test]
fn nothing_is_recorded_without_a_recording_and_an_inner_one_hides_the_outer() {
    let dir = scratch();
    let file = std::fs::File::create(dir.path().join("f")).unwrap();
    ordered(&file).unwrap();
    let outer = seam::Recording::start();
    assert_eq!(
        outer.take(),
        [],
        "the flush before the start is not recorded"
    );
    durable(&file).unwrap();
    {
        let inner = seam::Recording::start();
        ordered(&file).unwrap();
        assert_eq!(inner.take(), [Flush::Ordered]);
    }
    ordered(&file).unwrap();
    assert_eq!(outer.take(), [Flush::Durable, Flush::Ordered]);
}

#[test]
fn another_threads_flushes_are_not_this_threads() {
    let dir = scratch();
    let path = dir.path().join("f");
    std::fs::write(&path, b"x").unwrap();
    let recording = seam::Recording::start();
    std::thread::spawn(move || {
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        durable(&file).unwrap();
    })
    .join()
    .unwrap();
    assert_eq!(recording.take(), []);
}

#[test]
fn the_real_barrier_and_full_flush_take_a_file_and_a_directory() {
    let dir = scratch();
    let mut file = std::fs::File::create(dir.path().join("f")).unwrap();
    file.write_all(b"ordered").unwrap();
    primitive::barrier(&file).expect("the barrier on a regular file");
    primitive::full(&file).expect("the full flush on a regular file");
    #[cfg(unix)]
    {
        let handle = std::fs::File::open(dir.path()).unwrap();
        primitive::barrier(&handle).expect("the barrier on a directory");
        primitive::full(&handle).expect("the full flush on a directory");
    }
}

/// `ENOTSUP` as this platform numbers it ([`seam::ENOTSUP`]): Darwin's 45, Linux's 95.
#[cfg(unix)]
fn enotsup() -> std::io::Error {
    std::io::Error::from_raw_os_error(seam::ENOTSUP)
}

#[test]
fn accepting_a_file_refusal_preserves_success_and_portable_refusals() {
    assert!(refusal_accepted(Ok(())).is_ok());
    for kind in [io::ErrorKind::Unsupported, io::ErrorKind::InvalidInput] {
        assert!(refusal_accepted(Err(io::Error::from(kind))).is_ok());
    }
}

#[test]
fn accepting_a_file_refusal_preserves_other_errors_and_their_payloads() {
    for kind in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::NotFound,
        io::ErrorKind::StorageFull,
        io::ErrorKind::Other,
        io::ErrorKind::WriteZero,
    ] {
        let error = refusal_accepted(Err(io::Error::new(kind, std::fmt::Error))).unwrap_err();
        assert_eq!(error.kind(), kind);
        assert!(error.get_ref().unwrap().is::<std::fmt::Error>());
    }
}

#[cfg(unix)]
#[test]
fn accepting_a_file_refusal_preserves_real_io_failure_errnos() {
    for errno in [5, 28, 1, 9, 13, 2] {
        let error = refusal_accepted(Err(io::Error::from_raw_os_error(errno))).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(errno));
    }
}

#[cfg(target_vendor = "apple")]
#[test]
fn accepting_a_file_refusal_recognizes_darwin_raw_refusals() {
    for errno in [19, 22, 25, 45] {
        assert!(refusal_accepted(Err(io::Error::from_raw_os_error(errno))).is_ok());
    }
}

/// A directory flush that fails with `EIO` is an error to a best-effort writer too; one
/// the volume refuses with `ENOTSUP` is unavailable, carrying the refusal.
#[cfg(unix)]
#[test]
fn a_best_effort_dir_flush_reports_eio_and_reads_enotsup_as_unavailable() {
    let eio = DirFlushed::classify(Err(std::io::Error::from_raw_os_error(5)));
    assert_eq!(
        eio.map(|d| d.is_flushed()).map_err(|e| e.raw_os_error()),
        Err(Some(5)),
        "EIO is a failed flush"
    );
    match DirFlushed::classify(Err(enotsup())) {
        Ok(DirFlushed::Unavailable(e)) => {
            assert_eq!(e.raw_os_error(), enotsup().raw_os_error());
        }
        other => panic!("ENOTSUP is the volume's refusal, not {other:?}"),
    }
    assert!(DirFlushed::classify(Ok(())).unwrap().is_flushed());
}

/// The kernel's own refusal is accepted: `F_FULLFSYNC` on `/dev/null`, which devfs refuses
/// with an errno (`ENODEV`) std names by neither kind a check of the kind alone accepts, so
/// it is the errno half of the classification that accepts it.
#[cfg(target_vendor = "apple")]
#[test]
fn accepting_a_file_refusal_accepts_the_kernels_own() {
    let null = std::fs::File::open("/dev/null").unwrap();
    let refused = primitive::full(&null).expect_err("/dev/null takes no F_FULLFSYNC");
    assert!(
        !matches!(
            refused.kind(),
            io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
        ),
        "CONTROL: the kind alone names this refusal ({refused:?}); the errno half is untested"
    );
    assert!(
        refusal_accepted(Err(refused)).is_ok(),
        "the real refusal of F_FULLFSYNC on /dev/null"
    );
}

/// Over real directories: a directory flushes either way; one that is gone, and one this
/// process may write but not open (`0300`, unless it runs as root), are unavailable to the
/// best-effort flush and an error to the required one.
#[cfg(unix)]
#[test]
fn a_real_directory_flushes_and_an_unopenable_one_is_unavailable() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch();
    for flush in [Flush::Durable, Flush::Ordered] {
        let flushed = flush.dir_best_effort(dir.path()).unwrap();
        assert!(flushed.is_flushed(), "{flush:?}: {flushed:?}");
    }
    let unavailable = |path: &std::path::Path| match Flush::Durable.dir_best_effort(path) {
        Ok(DirFlushed::Unavailable(e)) => e.kind(),
        other => panic!("{} is unavailable, not {other:?}", path.display()),
    };
    let gone = dir.path().join("gone");
    assert!(
        Flush::Durable.dir(&gone).is_err(),
        "the required flush refuses"
    );
    assert_eq!(unavailable(&gone), std::io::ErrorKind::NotFound);

    let write_only = dir.path().join("write-only");
    std::fs::create_dir(&write_only).unwrap();
    std::fs::set_permissions(&write_only, std::fs::Permissions::from_mode(0o300)).unwrap();
    if std::fs::File::open(&write_only).is_err() {
        assert!(Flush::Durable.dir(&write_only).is_err(), "required");
        assert_eq!(
            unavailable(&write_only),
            std::io::ErrorKind::PermissionDenied
        );
    }
    std::fs::set_permissions(&write_only, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// The barrier itself.
#[cfg(target_vendor = "apple")]
#[test]
fn this_volume_takes_the_barrier_on_a_file_and_a_directory() {
    let dir = scratch();
    let mut file = std::fs::File::create(dir.path().join("f")).unwrap();
    file.write_all(b"ordered").unwrap();
    if let Err(error) = astream_sys::barrier_sync(&file) {
        panic!("this volume refused F_BARRIERFSYNC on a regular file: {error}");
    }
    let handle = std::fs::File::open(dir.path()).unwrap();
    if let Err(error) = astream_sys::barrier_sync(&handle) {
        panic!(
            "this volume refused F_BARRIERFSYNC on a directory, so every ordered \
             directory flush here is a full one: {error}"
        );
    }
}

/// What [`primitive::barrier_or_full`] did with one injected barrier answer.
#[cfg(target_vendor = "apple")]
fn barrier_answering(
    answer: std::io::Error,
    full_fails: bool,
) -> (std::io::Result<()>, usize, usize) {
    let dir = scratch();
    let mut file = std::fs::File::create(dir.path().join("f")).unwrap();
    file.write_all(b"ordered").unwrap();
    let (mut barriers, mut fulls) = (0, 0);
    let result = primitive::barrier_or_full(
        &file,
        |_| {
            barriers += 1;
            Err(answer)
        },
        |f| {
            fulls += 1;
            if full_fails {
                Err(std::io::Error::from_raw_os_error(5))
            } else {
                primitive::full(f)
            }
        },
    );
    (result, barriers, fulls)
}

/// The four errnos astream-sys reads as a REFUSED barrier (`ENODEV`, `EINVAL`, `ENOTTY`,
/// `ENOTSUP`) promise no ordering.
#[cfg(target_vendor = "apple")]
#[test]
fn a_refused_barrier_is_answered_by_the_full_flush() {
    for errno in [19, 22, 25, 45] {
        let (result, barriers, fulls) =
            barrier_answering(std::io::Error::from_raw_os_error(errno), false);
        result.unwrap_or_else(|e| panic!("errno {errno}: the full flush failed: {e}"));
        assert_eq!((barriers, fulls), (1, 1), "errno {errno}");
    }
    let (result, _, fulls) = barrier_answering(std::io::Error::from_raw_os_error(45), true);
    assert_eq!(
        result.map_err(|e| e.raw_os_error()),
        Err(Some(5)),
        "a refused barrier answers with the full flush's own error"
    );
    assert_eq!(fulls, 1);

    let dir = scratch();
    let file = std::fs::File::create(dir.path().join("f")).unwrap();
    let mut fulls = 0;
    primitive::barrier_or_full(
        &file,
        |_| Ok(()),
        |_| {
            fulls += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(fulls, 0, "a barrier that succeeds owes no full flush");
}

/// THE NEGATIVE CONTROL of the test above.
#[cfg(target_vendor = "apple")]
#[test]
fn a_failed_barrier_is_returned_and_never_covered_by_a_full_flush() {
    for errno in [5, 28, 9, 1] {
        let (result, barriers, fulls) =
            barrier_answering(std::io::Error::from_raw_os_error(errno), false);
        assert_eq!(
            result.map_err(|e| e.raw_os_error()),
            Err(Some(errno)),
            "errno {errno} is the barrier's own failure"
        );
        assert_eq!((barriers, fulls), (1, 0), "errno {errno}");
    }
    for kind in [std::io::ErrorKind::Unsupported, std::io::ErrorKind::Other] {
        let (result, _, fulls) = barrier_answering(std::io::Error::from(kind), false);
        assert_eq!(result.map_err(|e| e.kind()), Err(kind));
        assert_eq!(fulls, 0, "{kind:?} carries no errno, so it is no refusal");
    }
}

/// The pair [`primitive::barrier`] itself hands [`primitive::barrier_or_full`] on Apple.
#[cfg(target_vendor = "apple")]
#[test]
fn the_shipped_barrier_answers_a_refusal_with_the_real_full_flush() {
    let null = std::fs::File::open("/dev/null").expect("/dev/null opens");
    let barrier = astream_sys::barrier_sync(&null);
    assert!(
        barrier.as_ref().is_err_and(astream_sys::barrier_refused),
        "devfs no longer refuses F_BARRIERFSYNC on /dev/null ({barrier:?}): this test \
         needs another descriptor whose barrier is refused"
    );
    let full = primitive::full(&null).map_err(|e| e.raw_os_error());
    assert!(
        full.is_err(),
        "devfs's full flush on /dev/null succeeded, so a shipped barrier that skipped \
         it would pass here: this test needs a descriptor whose full flush fails"
    );
    assert_eq!(
        primitive::barrier(&null).map_err(|e| e.raw_os_error()),
        full,
        "a refused barrier answers with the real full flush's own result"
    );
}

/// [`ordered_or_fsync`] falls back only on a refusal: a failed flush is returned untouched,
/// and a flush that succeeded owes nothing more.
#[cfg(target_vendor = "apple")]
#[test]
fn only_a_refused_flush_is_answered_by_the_fallback() {
    for errno in [19, 22, 25, 45] {
        let mut fallbacks = 0;
        let answer = primitive::refused_or(Err(std::io::Error::from_raw_os_error(errno)), || {
            fallbacks += 1;
            Err(std::io::Error::from_raw_os_error(5))
        });
        assert_eq!(
            (answer.map_err(|e| e.raw_os_error()), fallbacks),
            (Err(Some(5)), 1),
            "errno {errno}: the fallback's own answer"
        );
    }
    for errno in [5, 28, 9, 1] {
        let mut fallbacks = 0;
        let answer = primitive::refused_or(Err(std::io::Error::from_raw_os_error(errno)), || {
            fallbacks += 1;
            Ok(())
        });
        assert_eq!(
            (answer.map_err(|e| e.raw_os_error()), fallbacks),
            (Err(Some(errno)), 0),
            "errno {errno} is a failed flush"
        );
    }
    let mut fallbacks = 0;
    primitive::refused_or(Ok(()), || {
        fallbacks += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(fallbacks, 0);
}

/// The shipped [`super::ordered_or_fsync`] on a descriptor that refuses both of [`ordered`]'s
/// flushes issues one plain `fsync(2)` ([`primitive::fsync`], which the seam counts by
/// [`seam::Recording::take_plain`]) and one ordered flush to the seam, and one that takes
/// the barrier issues no `fsync(2)`. This binary takes the shipped flushes
/// ([`shipped_durable_flushes_in_tests!`]), so the refused full flush is the real
/// `F_FULLFSYNC`'s.
#[cfg(target_vendor = "apple")]
#[test]
fn the_shipped_fallback_takes_fsync_where_both_flushes_are_refused() {
    let null = std::fs::File::open("/dev/null").expect("/dev/null opens");
    let refused = ordered(&null);
    assert!(
        refused.as_ref().is_err_and(astream_sys::barrier_refused),
        "devfs no longer refuses both flushes on /dev/null ({refused:?}): this test needs \
         another descriptor that does"
    );
    let recording = seam::Recording::start();
    super::ordered_or_fsync(&null).expect("the refused flushes answered by fsync");
    assert_eq!(
        recording.take_plain(),
        1,
        "one fsync(2) answered the refusals"
    );
    assert_eq!(
        recording.take(),
        [Flush::Ordered],
        "the fallback is a plain fsync(2)"
    );
    let dir = scratch();
    let file = std::fs::File::create(dir.path().join("f")).unwrap();
    super::ordered_or_fsync(&file).expect("this volume takes an ordered flush");
    assert_eq!(
        recording.take_plain(),
        0,
        "a flush that succeeded owes no fsync(2)"
    );
    assert_eq!(recording.take(), [Flush::Ordered]);
}

/// A plain `fsync(2)` is counted on its own thread, apart from the [`Flush`]es: a file's
/// and a directory's bare descriptor each move [`seam::Recording::take_plain`] by one and
/// record no [`Flush`], a nested recording sees only its own and hands the outer one
/// back whole, and no recording counts nothing. NEGATIVE CONTROL: a durable flush moves
/// the plain count by none.
#[cfg(unix)]
#[test]
fn the_seam_counts_each_plain_fsync_on_its_thread() {
    let dir = scratch();
    let file = std::fs::File::create(dir.path().join("f")).unwrap();
    let folder = std::os::fd::OwnedFd::from(std::fs::File::open(dir.path()).unwrap());
    primitive::fsync(&file).expect("no recording yet");
    let outer = seam::Recording::start();
    assert_eq!(
        outer.take_plain(),
        0,
        "a flush before the recording is none of its"
    );
    primitive::fsync(&file).expect("a file's plain flush");
    {
        let inner = seam::Recording::start();
        primitive::fsync(&folder).expect("a directory's bare descriptor");
        primitive::fsync(std::os::fd::AsFd::as_fd(&folder)).expect("and borrowed");
        assert_eq!(inner.take_plain(), 2);
        assert_eq!(inner.take_plain(), 0, "a take empties the count");
        assert_eq!(inner.take(), [], "a plain flush is no Flush");
    }
    durable(&file).expect("the control: a full flush");
    assert_eq!(outer.take(), [Flush::Durable]);
    assert_eq!(
        outer.take_plain(),
        1,
        "the outer recording's own plain flush"
    );
}

#[test]
fn a_later_write_on_the_same_device_is_ordered_and_an_unreadable_one_is_durable() {
    let dir = scratch();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::write(&a, b"a").unwrap();
    std::fs::create_dir(&b).unwrap();
    #[cfg(unix)]
    let same_device = Flush::Ordered;
    #[cfg(not(unix))]
    let same_device = Flush::Durable;
    assert_eq!(Flush::ordered_if_same_device(&a, &b), same_device);
    #[cfg(unix)]
    assert_eq!(
        Flush::ordered_if_same_device(&a, dir.path()),
        Flush::Ordered
    );
    assert_eq!(
        Flush::ordered_if_same_device(&a, &dir.path().join("absent")),
        Flush::Durable,
        "a path that cannot be read is another device for all this knows"
    );
}

/// Two volumes.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_later_write_on_another_device_is_durable() {
    let dir = scratch();
    assert_eq!(
        Flush::ordered_if_same_device(dir.path(), std::path::Path::new("/dev")),
        Flush::Durable
    );
}
