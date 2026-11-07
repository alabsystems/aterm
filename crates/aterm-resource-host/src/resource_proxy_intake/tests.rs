// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;
use aterm_uds::{CtlListener, spawnfd};
use std::{
    fs::File,
    io::{Seek, SeekFrom},
    os::fd::{AsRawFd, FromRawFd},
    time::Duration,
};
use trust_resource_controller::{
    Channel, CommandSpec, Delegations as WireDelegations, JobBinding, ProducerChallenge, Role,
};
const ACTOR: &str = "ATERM_PROXY_INTAKE_FIXTURE";
const MODE: &str = "ATERM_PROXY_INTAKE_MODE";
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}
fn request() -> Request {
    Request {
        binding: JobBinding {
            session: "s-ab12".into(),
            root_attempt: [1; 16],
            capability: [7; 32],
        },
        member_attempt: [2; 16],
        role: Role::MetadataVersion,
        command: CommandSpec {
            program: b"/bin/echo".to_vec(),
            arg0: None,
            args: vec![b"-vV".to_vec()],
            environment: vec![],
            cwd: b"/".to_vec(),
        },
        delegations: WireDelegations {
            cpu: None,
            gnu: GnuTransport::None,
            fences: vec![],
        },
        timeout_ns: None,
    }
}
#[test]
fn cached_intake_actor() {
    let Some(path) = std::env::var_os(ACTOR) else {
        return;
    };
    let mode = std::env::var(MODE).unwrap();
    let source = |name| {
        let raw: i32 = std::env::var(name).unwrap().parse().unwrap();
        // SAFETY: F_GETFD validates the test actor's explicitly inherited owned slot.
        assert!(unsafe { libc::fcntl(raw, libc::F_GETFD) } >= 0);
        // SAFETY: the isolated actor owns this declared inherited descriptor once.
        unsafe { OwnedFd::from_raw_fd(raw) }
    };
    let proof = source("ATERM_INTAKE_PROOF_FD");
    let data = source("ATERM_INTAKE_DATA_FD");
    let channel = CtlStream::connect(path).unwrap();
    transport::write_frame(&channel, &request().encode().unwrap(), deadline()).unwrap();
    let response = transport::read_frame(&channel, deadline()).unwrap();
    if mode == "deny" {
        assert_eq!(response, [0]);
    } else {
        assert_eq!(response, subgroup::READY);
        let (_held, events) = CtlStream::pair().unwrap();
        let mut rights = vec![
            data.as_fd(),
            data.as_fd(),
            data.as_fd(),
            events.as_fd(),
            proof.as_fd(),
        ];
        if mode == "truncated" {
            rights.pop();
        }
        assert_eq!(
            aterm_uds::fdpass::send_with_fds(&channel, &[subgroup::FD_MARKER], &rights).unwrap(),
            1
        );
        assert_eq!(transport::read_frame(&channel, deadline()).unwrap(), [1]);
        return;
    }
    assert_eq!(transport::read_frame(&channel, deadline()).unwrap(), [1]);
}
struct Helper(spawnfd::Child);
impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn cached_helper_intake_success_deny_wrong_challenge_and_truncated_bundle() {
    // Preserve cached images even on failed assessment/execution; no unlink race.
    let scratch = std::mem::ManuallyDrop::new(aterm_tempfile::tempdir().unwrap());
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let store = aterm_capacity::host_executable::Store::prepare_at(&root.join("store")).unwrap();
    let program = store.cache_current().unwrap();
    let pinned = Arc::new(
        trust_resource_provenance::Store::open_at(&root.join("store"))
            .unwrap()
            .pin_program(program.path(), deadline())
            .unwrap(),
    );
    let expected = Binding {
        session: "s-ab12".into(),
        attempt: [1; 16],
    };
    for mode in ["deny", "truncated", "wrongnonce", "success"] {
        let path = root.join(mode);
        let listener = aterm_uds::kept_out(|| CtlListener::bind(&path)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (producer, proof) = UnixStream::pair().unwrap();
        let mut data = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(root.join(format!("{mode}.data")))
            .unwrap();
        let data_copy = OwnedFd::from(data.try_clone().unwrap());
        let proof_raw = proof.as_raw_fd();
        let data_raw = data_copy.as_raw_fd();
        let mut command = spawnfd::command(program.path());
        command
            .args([
                "--exact",
                "resource_proxy_intake::tests::cached_intake_actor",
                "--nocapture",
            ])
            .env(ACTOR, &path)
            .env(MODE, mode)
            .env("ATERM_INTAKE_PROOF_FD", proof_raw.to_string())
            .env("ATERM_INTAKE_DATA_FD", data_raw.to_string())
            .stdin(spawnfd::Stdio::null())
            .stdout(spawnfd::Stdio::null())
            .stderr(spawnfd::Stdio::null());
        command
            .inherit_guards(vec![proof.into(), data_copy])
            .unwrap();
        let mut helper = Helper(command.spawn().unwrap());
        drop(command);
        let channel = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    ready::until(listener.as_fd(), Interest::Read, deadline()).unwrap()
                }
                Err(e) => panic!("accept: {e}"),
            }
        };
        let (done, held) = std::sync::mpsc::sync_channel(1);
        let echo = if matches!(mode, "success" | "wrongnonce") {
            Some(std::thread::spawn(move || {
                let mut channel = Channel::new(producer).unwrap();
                let mut challenge =
                    ProducerChallenge::decode(&channel.receive(deadline()).unwrap()).unwrap();
                if mode == "wrongnonce" {
                    challenge.nonce[0] ^= 1;
                }
                channel
                    .send(&challenge.answer().unwrap(), deadline())
                    .unwrap();
                held.recv_timeout(Duration::from_secs(30)).unwrap();
            }))
        } else {
            None
        };
        let received = receive(&channel, &expected, &pinned, deadline(), |r, peer| {
            assert_eq!(peer.pid, helper.0.id());
            assert_eq!(r.binding.capability, [7; 32]);
            mode != "deny"
        });
        if mode == "success" {
            let incoming = received.unwrap();
            assert_eq!(incoming.helper.pid, helper.0.id());
            assert_eq!(incoming.request.member_attempt, [2; 16]);
            incoming.proof.validate().unwrap();
            let mut original = File::from(incoming.rights.stdin);
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                (
                    original.metadata().unwrap().dev(),
                    original.metadata().unwrap().ino()
                ),
                (
                    data.metadata().unwrap().dev(),
                    data.metadata().unwrap().ino()
                )
            );
            original.seek(SeekFrom::Start(7)).unwrap();
            assert_eq!(
                data.stream_position().unwrap(),
                7,
                "same original open description"
            );
            assert!(incoming.rights.delegated.cpu.is_none());
        } else {
            assert!(received.is_err(), "{mode} must refuse");
        }
        transport::write_frame(&channel, &[1], deadline()).unwrap();
        assert!(helper.0.wait().unwrap().success());
        if let Some(echo) = echo {
            done.send(()).unwrap();
            echo.join().unwrap();
        }
    }
}
