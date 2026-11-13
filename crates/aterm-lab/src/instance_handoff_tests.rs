// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_toolchain::record_lock::RecordLock;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;

fn held(path: &Path) -> CloseOnlyLock {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    RecordLock::try_lock(file, 0).unwrap().into_close_only()
}

#[test]
fn an_exec_is_not_native_absence_and_a_reused_pid_is_not_ours() {
    let original = Identity {
        pid: 42,
        birth: process::Birth(10),
        idversion: Some(1),
    };
    let exec = Identity {
        idversion: Some(2),
        ..original
    };
    let reused = Identity {
        birth: process::Birth(11),
        ..original
    };
    assert!(!retired_by(
        original,
        Some(original),
        ProcessPresence::Present
    ));
    assert!(!retired_by(original, Some(exec), ProcessPresence::Present));
    assert!(retired_by(original, Some(reused), ProcessPresence::Present));
    assert!(retired_by(original, None, ProcessPresence::Absent));
    for presence in [
        ProcessPresence::Present,
        ProcessPresence::Exited,
        ProcessPresence::Unknown,
    ] {
        assert!(!retired_by(original, None, presence));
    }
    let changed_image_means_gone = exec != original;
    assert!(
        changed_image_means_gone && !retired_by(original, Some(exec), ProcessPresence::Present),
        "negative control: retiring an image version loses a still-live process lifetime"
    );
}

#[test]
fn descriptor_proof_rejects_bystanders_and_malformed_transfers_without_unlocking() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("original");
    let original = held(&path);
    let independent = File::open(&path).unwrap();
    let other = held(&world.path().join("other"));
    for case in 0..8 {
        let (reader, writer) = aterm_uds::CtlStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let actual = original.try_clone_fd().unwrap();
        let wrong = other.try_clone_fd().unwrap();
        let challenge = [b'a'; 64];
        let mut wire = aterm_types::lab_window_guard::CUSTODY_PREFIX.to_vec();
        wire.extend_from_slice(&challenge);
        wire.push(b'\n');
        let expected = wire.as_slice();
        match case {
            0 => {
                aterm_uds::fdpass::send_with_fds(&writer, &expected[..3], &[actual.as_fd()])
                    .unwrap();
                (&writer).write_all(&expected[3..]).unwrap();
            }
            1 => {
                aterm_uds::fdpass::send_with_fds(&writer, expected, &[independent.as_fd()])
                    .unwrap();
            }
            2 => {
                aterm_uds::fdpass::send_with_fds(&writer, expected, &[wrong.as_fd()]).unwrap();
            }
            3 => {
                aterm_uds::fdpass::send_with_fds(&writer, expected, &[]).unwrap();
            }
            4 => {
                aterm_uds::fdpass::send_with_fds(&writer, b"wrong", &[actual.as_fd()]).unwrap();
            }
            5 => {
                aterm_uds::fdpass::send_with_fds(
                    &writer,
                    expected,
                    &[actual.as_fd(), actual.as_fd()],
                )
                .unwrap();
            }
            6 => {
                let mut extra = expected.to_vec();
                extra.push(b'!');
                aterm_uds::fdpass::send_with_fds(&writer, &extra, &[actual.as_fd()]).unwrap();
            }
            _ => {
                let mut stale = aterm_types::lab_window_guard::CUSTODY_PREFIX.to_vec();
                stale.extend_from_slice(&[b'b'; 64]);
                stale.push(b'\n');
                aterm_uds::fdpass::send_with_fds(&writer, &stale, &[actual.as_fd()]).unwrap();
            }
        }
        drop(writer);
        let result = receive_proof(&reader, &original, &challenge, Instant::now() + HANG);
        assert_eq!(
            result.is_ok(),
            case == 0,
            "transfer case {case}: {result:?}"
        );
        assert!(
            matches!(
                RecordLock::try_lock(File::open(&path).unwrap(), 0),
                Err(std::fs::TryLockError::WouldBlock)
            ),
            "proof handling unlocked the original"
        );
    }
}

#[test]
fn physical_token_custody_rejects_replacement_and_in_place_changes() {
    for replace in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let socket = world.path().join("a.sock");
        let name = aterm_types::control_socket::token_name_for_sock("a.sock");
        aterm_uds::private_file::write_private(world.path(), &name, b"aabbccdd\n").unwrap();
        let token = Token::capture(&socket).unwrap();
        assert!(token.current().unwrap());
        if replace {
            aterm_uds::private_file::write_private(world.path(), "replacement", b"aabbccdd\n")
                .unwrap();
            std::fs::rename(world.path().join("replacement"), &token.path).unwrap();
        } else {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&token.path)
                .unwrap();
            file.write_all_at(b"00112233\n", 0).unwrap();
        }
        assert!(token.current().is_err());
    }
}

#[test]
fn only_an_actual_description_proof_can_pin_a_peer_and_renew_credentials() {
    use std::io::BufRead as _;
    for genuine in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let socket = world.path().join("a.sock");
        let name = aterm_types::control_socket::token_name_for_sock("a.sock");
        aterm_uds::private_file::write_private(world.path(), &name, b"aabbccdd\n").unwrap();
        let lane = world.path().join("lane");
        let original = held(&lane);
        let descriptor = if genuine {
            original.try_clone_fd().unwrap()
        } else {
            File::open(&lane).unwrap().into()
        };
        let mut owner = Tracking::new(original, &socket, None).unwrap();
        let initial = owner.token.identity;
        aterm_uds::private_file::write_private(world.path(), "next", b"00112233\n").unwrap();
        std::fs::rename(world.path().join("next"), &owner.token.path).unwrap();
        let listener = aterm_uds::CtlListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                let (stream, _) = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Until(Instant::now() + HANG),
                    aterm_uds::AcceptedMode::Blocking,
                )
                .unwrap();
                stream.set_read_timeout(Some(HANG)).unwrap();
                let mut reader = std::io::BufReader::new(&stream);
                let mut auth = String::new();
                reader.read_line(&mut auth).unwrap();
                assert!(auth == "AUTH 00112233\n");
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let challenge = request.trim().split_once(" nonce=").unwrap().1;
                let mut wire = aterm_types::lab_window_guard::CUSTODY_PREFIX.to_vec();
                wire.extend_from_slice(challenge.as_bytes());
                wire.push(b'\n');
                aterm_uds::fdpass::send_with_fds(&stream, &wire, &[descriptor.as_fd()]).unwrap();
            });
            let observed = owner.observe(&socket, Instant::now() + HANG);
            assert_eq!(observed.is_ok(), genuine, "description proof result");
            if genuine {
                assert_eq!(
                    observed.unwrap().map(|id| id.pid),
                    i32::try_from(std::process::id()).ok()
                );
            }
            server.join().unwrap();
        });
        if genuine {
            assert_ne!(owner.token.identity, initial);
        } else {
            assert!(owner.identity().is_none());
            assert_eq!(owner.token.identity, initial);
        }
    }
}

#[test]
fn an_unavailable_successor_keeps_one_original_hang_detector() {
    let world = aterm_tempfile::tempdir().unwrap();
    let socket = world.path().join("a.sock");
    let name = aterm_types::control_socket::token_name_for_sock("a.sock");
    aterm_uds::private_file::write_private(world.path(), &name, b"aabbccdd\n").unwrap();
    let original = held(&world.path().join("lane"));
    let mut owner = Tracking::new(original, &socket, None).unwrap();
    assert!(owner.pid().is_none());
    assert!(owner.waiting());
    let deadline = owner.pending.unwrap();
    assert!(owner.waiting());
    assert_eq!(owner.pending, Some(deadline));
    owner.pending = Some(Instant::now());
    assert!(!owner.waiting());
    assert!(owner.refusal.is_some());
    std::fs::remove_file(&owner.token.path).unwrap();
    assert!(!owner.waiting());
}
