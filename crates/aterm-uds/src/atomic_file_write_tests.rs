// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Every arm of [`write_flushed`] publishes by its rename: up to the rename `path` holds what it
/// held and the one temporary beside it every new byte, and after it `path` holds the new bytes
/// in the write's mode and no temporary is left.
#[test]
fn every_flush_publishes_the_file_whole_by_its_rename() {
    for flush in [None, Some(Flush::Ordered), Some(Flush::Durable)] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("pid");
        write_flushed(&path, b"old\n", 0o600, flush).unwrap();
        let landed = write_landing(
            &path,
            b"4242\n",
            0o640,
            || {
                assert_eq!(
                    std::fs::read(&path)?,
                    b"old\n",
                    "{flush:?}: not yet replaced"
                );
                let temps: Vec<Vec<u8>> = std::fs::read_dir(dir.path())?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<io::Result<Vec<_>>>()?
                    .into_iter()
                    .filter(|temp| *temp != path)
                    .map(std::fs::read)
                    .collect::<io::Result<_>>()?;
                assert_eq!(
                    temps,
                    [b"4242\n".to_vec()],
                    "{flush:?}: the whole temporary"
                );
                Ok(())
            },
            flush,
            Flush::dir,
        )
        .unwrap();
        landed.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"4242\n", "{flush:?}");
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&path).unwrap().permissions()
            ) & 0o777,
            0o640,
            "{flush:?}"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "{flush:?}: no temporary is left"
        );
    }
}

/// What each arm of [`write_flushed`] asks of aterm-durable, as its seam records this thread's
/// flushes: no flush at all with none, and the temporary's and the directory's with one. The
/// durable arm's two full flushes are the control: the recording sees this writer's flushes, so
/// the empty one is no recording that missed them.
#[test]
fn a_write_with_no_flush_asks_none_and_a_durable_one_two_full_flushes() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("pid");
    let recording = aterm_durable::seam::Recording::start();
    for (flush, asked) in [
        (None, Vec::new()),
        (Some(Flush::Ordered), vec![Flush::Ordered; 2]),
        (Some(Flush::Durable), vec![Flush::Durable; 2]),
    ] {
        write_flushed(&path, b"4242\n", 0o600, flush).unwrap();
        assert_eq!(recording.take(), asked, "{flush:?}");
        assert_eq!(recording.take_plain(), 0, "{flush:?}: no plain fsync(2)");
        assert_eq!(std::fs::read(&path).unwrap(), b"4242\n", "{flush:?}");
    }
    write(&path, b"4242\n", 0o600).unwrap();
    assert_eq!(
        recording.take(),
        [Flush::Durable; 2],
        "write is the durable arm"
    );
}

/// A directory whose entries cannot be flushed here (`ENOTSUP`) is refused by
/// [`write_entry_flushed`], with either flush, and accepted by [`write_flushed`]; a failed flush
/// (`EIO`) is refused by both. Each asks one flush of the temporary and one of the directory,
/// and the rename has landed either way. NEGATIVE CONTROL: with `write_entry_flushed` taking
/// `best_effort(Flush::dir)`, the `ENOTSUP` arm returns `Ok` and its assertion fails.
#[test]
fn an_entry_flushed_write_refuses_a_directory_it_cannot_flush() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint");
    let recording = aterm_durable::seam::Recording::start();
    for flush in [Flush::Ordered, Flush::Durable] {
        for (errno, best_effort_fails) in [
            (aterm_durable::seam::ENOTSUP, false),
            (aterm_durable::seam::EIO, true),
        ] {
            let bytes = format!("{flush:?} {errno}\n");
            let failing = aterm_durable::seam::FailingDirs::with(errno);
            let entry = write_entry_flushed(&path, bytes.as_bytes(), 0o600, flush);
            let best = write_flushed(&path, bytes.as_bytes(), 0o600, Some(flush));
            drop(failing);
            assert_eq!(
                entry.map_err(|e| e.raw_os_error()),
                Err(Some(errno)),
                "{flush:?} {errno}: an entry flush needs its directory's"
            );
            assert_eq!(
                best.map_err(|e| e.raw_os_error()),
                if best_effort_fails {
                    Err(Some(errno))
                } else {
                    Ok(())
                },
                "{flush:?} {errno}"
            );
            assert_eq!(recording.take(), vec![flush; 4], "{flush:?} {errno}");
            assert_eq!(std::fs::read(&path).unwrap(), bytes.as_bytes());
        }
        write_entry_flushed(&path, b"flushed\n", 0o600, flush).unwrap();
        assert_eq!(recording.take(), vec![flush; 2], "{flush:?}");
    }
}

#[test]
fn producer_error_and_panic_leave_the_old_file_and_no_temporary() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("records");
    write(&path, b"old", 0o600).unwrap();
    let failed = write_with(&path, 0o600, |writer| {
        writer.write_all(b"incomplete prefix")?;
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "producer failed"))
    });
    assert_eq!(failed.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

    let panicked = std::panic::catch_unwind(|| {
        let _ = write_with(&path, 0o600, |writer| {
            writer.write_all(b"another prefix")?;
            panic!("producer panic");
        });
    });
    assert!(panicked.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    write_with(&path, 0o600, |writer| {
        writer.write_all(b"complete replacement")
    })
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"complete replacement");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
