// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;
use aterm_uds::{CtlListener, CtlStream, spawnfd};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

const ACTOR: &str = "ATERM_PRODUCER_PROOF_FIXTURE";
const SOURCE: &str = "ATERM_PRODUCER_PROOF_SOURCE";
fn deadline() -> Instant {
    crate::fixture_deadline(Instant::now())
}

#[test]
fn cached_helper_actor() {
    let Some(path) = std::env::var_os(ACTOR) else {
        return;
    };
    let raw: i32 = std::env::var(SOURCE).unwrap().parse().unwrap();
    // SAFETY: F_GETFD checks the deliberately inherited owned test descriptor.
    assert!(unsafe { libc::fcntl(raw, libc::F_GETFD) } >= 0);
    // SAFETY: this isolated actor owns the descriptor explicitly inherited by its parent.
    let proof = unsafe { OwnedFd::from_raw_fd(raw) };
    let channel = CtlStream::connect(path).unwrap();
    aterm_uds::fdpass::send_with_fds(&channel, b"R", &[proof.as_fd()]).unwrap();
    // The actual owner ends this helper through its owned Child, never a decoded PID.
    loop {
        std::thread::park();
    }
}
struct Helper(spawnfd::Child);
impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn cached_helper_parent_challenge_and_stale_wrong_answer_refusals() {
    // Retain the read-only image cache, including on any assessment/test failure.
    // No cached executable path is unlinked while macOS may assess an exec.
    let scratch = std::mem::ManuallyDrop::new(aterm_tempfile::tempdir().unwrap());
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let store = aterm_capacity::host_executable::Store::prepare_at(&root.join("store")).unwrap();
    aterm_capacity::host_executable::narrow_test_image();
    let program = store.cache_current().unwrap();
    let pinned = Arc::new(
        trust_resource_provenance::Store::open_at(&root.join("store"))
            .unwrap()
            .pin_program(program.path(), deadline())
            .unwrap(),
    );
    let path = root.join("proof");
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&path)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (producer, proof) = UnixStream::pair().unwrap();
    let raw = proof.as_raw_fd();
    let mut command = spawnfd::command(program.path());
    command
        .args([
            "--exact",
            "resource_producer::tests::cached_helper_actor",
            "--nocapture",
        ])
        .env(ACTOR, &path)
        .env(SOURCE, raw.to_string())
        .stdin(spawnfd::Stdio::null())
        .stdout(spawnfd::Stdio::null())
        .stderr(spawnfd::Stdio::null());
    command.inherit_guards(vec![proof.into()]).unwrap();
    let mut helper = Helper(command.spawn().unwrap());
    drop(command);
    let channel = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => aterm_uds::ready::until(
                listener.as_fd(),
                aterm_uds::ready::Interest::Read,
                deadline(),
            )
            .unwrap(),
            Err(e) => panic!("accept: {e}"),
        }
    };
    channel.set_read_timeout(Some(crate::FIXTURE_HANG)).unwrap();
    let peer = aterm_ctl::resource_admission_transport::local_peer(&channel).unwrap();
    assert_eq!(peer.pid, helper.0.id());
    let mut marker = [0; 1];
    let mut received = loop {
        match aterm_uds::fdpass::recv_with_fds(&channel, &mut marker, 1) {
            Ok(received) => break received,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => aterm_uds::ready::until(
                channel.as_fd(),
                aterm_uds::ready::Interest::Read,
                deadline(),
            )
            .unwrap(),
            Err(error) => panic!("receive proof: {error}"),
        }
    };
    assert_eq!((received.bytes, marker), (1, *b"R"));
    let proof = UnixStream::from(received.fds.pop().unwrap());
    let wrong = Peer {
        pid: peer.pid,
        birth: peer.birth + 1,
    };
    assert!(
        TransportProof::authenticate(
            proof.try_clone().unwrap(),
            wrong,
            pinned.clone(),
            [1; 16],
            [2; 16],
            deadline()
        )
        .is_err()
    );
    let wrong_socket = UnixStream::from(OwnedFd::from(channel));
    assert!(
        TransportProof::authenticate(
            wrong_socket,
            peer,
            pinned.clone(),
            [1; 16],
            [2; 16],
            deadline()
        )
        .is_err()
    );
    let (done, retained) = std::sync::mpsc::sync_channel(1);
    let echo = std::thread::spawn(move || {
        let mut channel = Channel::new(producer).unwrap();
        for correct in [false, true] {
            let mut challenge =
                ProducerChallenge::decode(&channel.receive(deadline()).unwrap()).unwrap();
            if !correct {
                challenge.nonce[0] ^= 1;
            }
            channel
                .send(&challenge.answer().unwrap(), deadline())
                .unwrap();
        }
        retained.recv_timeout(crate::FIXTURE_HANG).unwrap();
    });
    assert!(
        TransportProof::authenticate(
            proof.try_clone().unwrap(),
            peer,
            pinned.clone(),
            [1; 16],
            [2; 16],
            deadline()
        )
        .is_err()
    );
    let authenticated =
        TransportProof::authenticate(proof, peer, pinned, [1; 16], [2; 16], deadline()).unwrap();
    assert_eq!(authenticated.helper().pid, peer.pid);
    assert_eq!(authenticated.producer().pid, std::process::id());
    assert_eq!(authenticated.binding(), ([1; 16], [2; 16]));
    authenticated.validate().unwrap();
    // Simulate preemption after taking the start time without sleeping. The
    // original image, native identities and peer socket must still validate.
    // Restoring the 100 ms budget fails this actual authenticated proof.
    authenticated
        .validate_at(Instant::now() - Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        authenticated
            .validate_at(Instant::now() - Duration::from_secs(60))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    helper.0.kill().unwrap();
    helper.0.wait().unwrap();
    assert!(authenticated.validate().is_err());
    done.send(()).unwrap();
    echo.join().unwrap();
}
