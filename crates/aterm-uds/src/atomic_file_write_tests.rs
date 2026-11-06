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
