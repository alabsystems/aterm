// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn callback_checks_the_actual_receiver_and_keeps_its_socket_out_of_children() {
    use aterm_uds::spawnfd::{Child, Stdio};
    use std::os::fd::AsRawFd;
    struct Cleanup(Child);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let directory = aterm_tempfile::tempdir().unwrap();
    let path = directory.path().join("callback.sock");
    let listener = aterm_uds::kept_out(|| std::os::unix::net::UnixListener::bind(&path)).unwrap();
    let owner = Process::observe(std::process::id()).unwrap();
    // Kernel callback custody only. This mints no SDK member, Source or Ready.
    let callback =
        connect_callback(&path, &owner, Instant::now() + Duration::from_secs(10)).unwrap();
    let (accepted, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Until(std::time::Instant::now() + Duration::from_secs(60)),
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    // SAFETY: F_GETFD observes this live owned descriptor and takes no pointer.
    let flags = unsafe { libc::fcntl(callback.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    drop((accepted, callback));
    let executable = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/cat"));
    let mut command = aterm_uds::spawnfd::command(executable);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = Cleanup(command.spawn().unwrap());
    let input = aterm_uds::deadline::cat_echoes(&mut child.0);
    let foreign = Process::observe(child.0.id()).unwrap();
    assert!(matches!(
        connect_callback(&path, &foreign, Instant::now() + Duration::from_secs(10)),
        Err(registry::Error::Denied)
    ));
    drop(input);
}

#[test]
fn output_drain_keeps_a_bounded_private_tail_and_retires_its_actual_scope() {
    struct Input {
        bytes: io::Cursor<Vec<u8>>,
        done: Option<std::sync::mpsc::Sender<()>>,
    }
    impl Read for Input {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let count = self.bytes.read(output)?;
            if count == 0 {
                self.done.take().unwrap().send(()).unwrap();
            }
            Ok(count)
        }
    }
    let lifetime = account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let (done, completed) = std::sync::mpsc::channel();
    let bytes: Vec<_> = (0..100_000).map(|index| (index % 251) as u8).collect();
    let expected = bytes[bytes.len() - 32 * 1024..].to_vec();
    let diagnostics = Arc::new(Mutex::new(VecDeque::new()));
    drain(
        Input {
            bytes: io::Cursor::new(bytes),
            done: Some(done),
        },
        scope,
        diagnostics.clone(),
    )
    .unwrap();
    completed.recv_timeout(Duration::from_secs(60)).unwrap();
    let drained = lifetime.pause().unwrap().wait();
    assert_eq!(
        diagnostics
            .lock()
            .unwrap()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        expected
    );
    drained.resume().unwrap();
}

#[test]
fn refused_birth_is_never_a_no_child_or_settled_cohort_receipt() {
    let mut failure = SpawnFailure::Refused(io::Error::other("unavailable original birth scope"));
    assert_eq!(
        settle_failure(&mut failure).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(matches!(failure, SpawnFailure::Refused(_)));
}

#[test]
fn postbirth_failure_settlement_keeps_and_reaps_the_actual_original_cohort() {
    use aterm_uds::spawnfd::Stdio;
    let executable = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/cat"));
    let mut command = aterm_uds::spawnfd::command(executable);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut spawned = command.spawn_cohort(BirthAttempt::new().unwrap()).unwrap();
    let input = aterm_uds::deadline::cat_echoes(spawned.child_mut());
    let original = Process::observe(spawned.child_mut().id()).unwrap();
    let mut failure = SpawnFailure::Unbound {
        spawned: Box::new(spawned),
        error: io::Error::other("postbirth fixture refusal"),
    };
    let until = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        match settle_failure(&mut failure) {
            Ok(()) => break,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("actual original cohort settlement failed: {error}"),
        }
        assert!(
            std::time::Instant::now() < until,
            "native group settlement hang detector"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(Process::observe(original.pid).is_none_or(|now| now.birth != original.birth));
    assert_eq!(
        aterm_uds::process::group_presence(original.group),
        aterm_uds::process::ProcessPresence::Absent
    );
    assert!(matches!(failure, SpawnFailure::Unbound { .. }));
    drop(input);
}
