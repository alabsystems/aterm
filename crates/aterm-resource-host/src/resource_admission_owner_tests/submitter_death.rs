// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual submitting-process death over its own native authenticated channel.
//! This qualifies the existing owner, not a compiler/provider admission route.

use super::*;
use aterm_uds::{AcceptWait, AcceptedMode, CtlListener, spawnfd};
use std::{
    io::Write as _,
    os::fd::AsFd,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    os::unix::process::ExitStatusExt as _,
};

const ROLE: &str = "--resource-owner-submitter-death-fixture";
const EXACT: &str = "actor::owner::tests::submitter_death::actual_submitter_sigkill_preserves_outside_owner_custody";

fn binding() -> Binding {
    Binding {
        session: "s-acde".into(),
        attempt: [83; 16],
    }
}

fn descriptor_identity(fd: &impl AsFd) -> (u64, u64) {
    let metadata = File::from(fd.as_fd().try_clone_to_owned().unwrap())
        .metadata()
        .unwrap();
    (metadata.dev(), metadata.ino())
}

/// Run before libtest: the submitting process creates both native channels.
pub(super) fn early_role() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.first().is_none_or(|arg| arg != ROLE) {
        return;
    }
    let result = std::panic::catch_unwind(|| {
        assert_eq!(args.len(), 3);
        let (events, interest) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        let pid = std::process::id();
        let birth = process::read_birth(i32::try_from(pid).unwrap()).unwrap().0;
        let output = || -> OwnedFd {
            std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .unwrap()
                .into()
        };
        wire::attach(
            args[1].to_str().unwrap(),
            &Submission {
                binding: binding(),
                peer_pid: pid,
                peer_birth: birth,
                argv: vec![b"/bin/cat".to_vec()],
                cwd: args[2].as_bytes().to_vec(),
                env: Vec::new(),
                cpu_units: 1,
                memory_bytes: MEMORY,
                label: "actual submitting process death".into(),
                timeout_ms: Some(HANG.as_millis().try_into().unwrap()),
            },
            Attachments {
                stdin: std::io::stdin().as_fd().try_clone_to_owned().unwrap(),
                stdout: output(),
                stderr: output(),
                events: events.into(),
            },
            Instant::now() + HANG,
        )
        .unwrap();
        // SIGKILL must close this interest. No Cancel, shutdown or Rust Drop runs.
        loop {
            std::hint::black_box(&interest);
            std::thread::park();
        }
    });
    std::process::exit(if result.is_ok() { 0 } else { 101 });
}

struct OwnedChild(spawnfd::Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.0.is_reaped() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn actual_submitter_sigkill_preserves_outside_owner_custody() {
    if !aterm_uds::test_binary::isolated_lifecycle(EXACT) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    let mut world = World::new();
    let socket_root = aterm_tempfile::Builder::new()
        .prefix("death-")
        .tempdir_in(aterm_uds::test_binary::short_root().unwrap())
        .unwrap();
    let socket = socket_root.path().join("submit.sock");
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&socket)).unwrap();
    let principal = aterm_uds::rand::hex_token::<32>().unwrap();
    let token_path = socket_root
        .path()
        .join(aterm_types::control_socket::token_name_for_sock(
            "submit.sock",
        ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(token_path)
        .unwrap()
        .write_all(principal.as_bytes())
        .unwrap();
    let image = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    let (worker_input, worker_continuation) = aterm_uds::pipe().unwrap();
    let worker_input_identity = descriptor_identity(&worker_input);
    let mut command = spawnfd::command(&image);
    command
        .arg(ROLE)
        .arg(&socket)
        .arg(&world.scratch.path)
        .stdin(spawnfd::Stdio::from(worker_input));
    let mut submitter = OwnedChild(command.spawn().unwrap());
    let (unrelated_input, unrelated_continuation) = aterm_uds::pipe().unwrap();
    let mut unrelated = OwnedChild(
        spawnfd::command("/bin/cat")
            .stdin(spawnfd::Stdio::from(unrelated_input))
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + HANG;
    let (stream, _) = aterm_uds::accept(
        &listener,
        AcceptWait::Until(deadline),
        AcceptedMode::Nonblocking,
    )
    .unwrap();
    let peer = wire::local_peer(&stream).unwrap();
    assert_eq!(peer.pid, submitter.0.id());
    assert_eq!(
        peer.birth,
        process::read_birth(i32::try_from(submitter.0.id()).unwrap())
            .unwrap()
            .0
    );
    assert_eq!(
        std::fs::canonicalize(
            process::read_process_path(i32::try_from(peer.pid).unwrap()).unwrap()
        )
        .unwrap(),
        image
    );
    // The private Instance principal is freshly owned by this fixture. Check it
    // as well as the actual child peer, without printing capability bytes.
    let authenticated = wire::read_line(&stream, deadline).unwrap() == format!("AUTH {principal}");
    assert!(authenticated, "the fixture Instance principal was refused");
    assert_eq!(wire::read_line(&stream, deadline).unwrap(), "version");
    wire::write_all(&stream, b"OK native owner fixture\n", deadline).unwrap();
    assert_eq!(
        wire::read_line(&stream, deadline).unwrap(),
        aterm_types::resource_admission::REQUEST.trim_end()
    );
    let expected = binding();
    let submission = wire::receive(&stream, authenticated, deadline, |request, actual| {
        actual == peer
            && request.binding == expected
            && request.cwd == world.scratch.path.as_os_str().as_bytes()
            && request.argv == [b"/bin/cat".to_vec()]
            && request.env.is_empty()
            && request.cpu_units == 1
            && request.memory_bytes == MEMORY
            && world.owner.accepts(&request.binding)
    })
    .unwrap();
    assert_eq!(submission.peer, peer);
    assert_eq!(
        descriptor_identity(&submission.fds.stdin),
        worker_input_identity
    );
    let null = File::open("/dev/null").unwrap();
    assert_eq!(
        descriptor_identity(&submission.fds.stdout),
        descriptor_identity(&null)
    );
    assert_eq!(
        descriptor_identity(&submission.fds.stderr),
        descriptor_identity(&null)
    );
    let memory = queue_authenticated(&mut world.owner, &world.scratch.path, submission);
    let recipe = world
        .owner
        .pending
        .back_mut()
        .unwrap()
        .recipe
        .as_mut()
        .unwrap();
    recipe.program = "/bin/cat".into();
    recipe.arguments.clear();
    wire::acknowledge(&stream, deadline).unwrap();
    drop(stream);
    world.owner.tick_queue();
    let key = (expected.session.clone(), expected.attempt);
    let job = world.owner.jobs.get_mut(&key).unwrap();
    assert!(job.launched && !job.reaped && !job.is_settled());
    job.timeout = None; // This control must exercise EOF, never the timeout branch.
    let worker_pid = job.child.child_mut().id();
    let worker_birth = job.root_birth;
    assert_ne!(worker_pid, peer.pid);
    assert_ne!(worker_pid, unrelated.0.id());
    assert_eq!(
        process::read_birth(i32::try_from(worker_pid).unwrap())
            .unwrap()
            .0,
        worker_birth
    );
    assert!(!job.child.child_mut().exited().unwrap());
    assert!(!unrelated.0.exited().unwrap());
    assert_eq!(world.owner.debt.reserved().unwrap(), 1);
    assert_eq!(
        world.owner.ledger.cohort_claim_state(&memory).unwrap(),
        CohortClaimState::Retained
    );

    submitter.0.kill().unwrap();
    assert_eq!(submitter.0.wait().unwrap().signal(), Some(libc::SIGKILL));
    assert!(submitter.0.is_reaped());
    // The outside owner retains the writer: submitter death cannot naturally
    // complete cat. Its original owned handle is live before EOF processing.
    let job = world.owner.jobs.get_mut(&key).unwrap();
    assert_eq!(job.child.child_mut().id(), worker_pid);
    assert!(!job.child.child_mut().exited().unwrap());
    assert!(!job.child.child_mut().is_reaped());
    assert_eq!(world.owner.debt.reserved().unwrap(), 1);
    world.owner.tick_jobs();
    let job = world.owner.jobs.get_mut(&key).unwrap();
    assert_eq!(job.cause, CompletionCause::Cancelled);
    assert!(job.cleanup_at.is_some());
    assert!(!job.reaped && !job.child.child_mut().is_reaped());
    assert_eq!(world.owner.debt.reserved().unwrap(), 1);
    assert_eq!(
        world.owner.ledger.cohort_claim_state(&memory).unwrap(),
        CohortClaimState::Retained
    );
    world.settle();
    world.assert_settled(&expected, &memory, worker_pid);
    assert!(
        !unrelated.0.exited().unwrap(),
        "owner touched an unrelated child"
    );
    drop(worker_continuation);
    // Its own guard kills and reaps the unrelated child while its writer is
    // still held, independently of the tested owner's worker cleanup.
    drop(unrelated);
    drop(unrelated_continuation);
    eprintln!(
        "actual submitter SIGKILL: original worker reaped, resources settled, outside owner survived"
    );
}
