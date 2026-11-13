// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    CLAIM_FRAME_LEN, ClaimDeadlines, RendezvousError, dial_and_claim, listener_may_receive_claim,
};
use crate::seamless::AttestedParent;
use aterm_models_update::native_update_rendezvous_claim_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::time::Instant;

/// Dialer-observed listener properties projected into the formal rendezvous model.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ListenerFacts {
    same_uid: bool,
    is_parent: bool,
    parent_live: bool,
}

/// Projects [`ListenerFacts`] onto the formal rendezvous model's state prior to claim presentation.
pub(crate) fn project(model: &Model, facts: ListenerFacts) -> State {
    let mut state = model.init_state();
    state.insert("same_uid", i64::from(facts.same_uid));
    state.insert("is_parent", i64::from(facts.is_parent));
    state.insert("parent_live", i64::from(facts.parent_live));
    state
}

fn every_combination() -> impl Iterator<Item = ListenerFacts> {
    (0u8..8).map(|bits| ListenerFacts {
        same_uid: bits & 1 != 0,
        is_parent: bits & 2 != 0,
        parent_live: bits & 4 != 0,
    })
}

fn own_pid() -> libc::pid_t {
    libc::pid_t::try_from(std::process::id()).expect("a pid fits pid_t")
}

fn real_gate(facts: ListenerFacts) -> Result<(), RendezvousError> {
    // SAFETY: `getuid` and `getppid` have no preconditions.
    let (own_uid, launcher) = unsafe { (libc::getuid(), libc::getppid()) };
    let listener_uid = if facts.same_uid {
        own_uid
    } else {
        own_uid.wrapping_add(1)
    };
    let listener_pid = if facts.is_parent { own_pid() } else { launcher };
    let parent = if facts.parent_live {
        AttestedParent::attest_live_for_test(own_pid())
            .expect("this live process attests by its birth record")
    } else {
        AttestedParent::recycled_for_test(own_pid())
    };
    listener_may_receive_claim(
        Some(listener_uid),
        own_uid,
        u32::try_from(listener_pid).ok(),
        Some(parent),
    )
}

#[test]
fn the_dial_gate_admits_exactly_what_the_model_admits() {
    let model = native_update_rendezvous_claim_model();
    for facts in every_combination() {
        let admitted = real_gate(facts).is_ok();
        let modelled = model.action_enabled("PresentClaim", &project(&model, facts));
        assert_eq!(
            admitted, modelled,
            "the dial gate and the model's PresentClaim disagree at {facts:?}"
        );
    }
    assert!(
        real_gate(ListenerFacts {
            same_uid: true,
            is_parent: true,
            parent_live: true,
        })
        .is_ok()
    );
}

#[test]
fn the_real_gate_refuses_where_the_uid_only_mutant_presents() {
    let buggy = with_buggy(&native_update_rendezvous_claim_model(), 1);
    let mut told_apart = Vec::new();
    for facts in every_combination() {
        let mutant = buggy.action_enabled("PresentClaimUidOnly", &project(&buggy, facts));
        assert_eq!(
            mutant, facts.same_uid,
            "the Buggy guard is the uid half at {facts:?}"
        );
        if real_gate(facts).is_ok() != mutant {
            told_apart.push(facts);
        }
    }
    assert_eq!(
        told_apart.len(),
        3,
        "the real gate refuses the uid-only gate's impostor, gone-parent and both \
         presentations, and only those: {told_apart:?}"
    );
    assert!(
        told_apart
            .iter()
            .all(|facts| facts.same_uid && !(facts.is_parent && facts.parent_live)),
        "every disagreement is a same-uid presentation to something other than the live \
         attested parent: {told_apart:?}"
    );
}

#[test]
fn no_attested_parent_refuses_even_our_own_listener() {
    // SAFETY: `getuid` has no preconditions.
    let own_uid = unsafe { libc::getuid() };
    assert!(matches!(
        listener_may_receive_claim(Some(own_uid), own_uid, Some(std::process::id()), None),
        Err(RendezvousError::Dial(_))
    ));
    assert!(matches!(
        listener_may_receive_claim(
            Some(own_uid),
            own_uid,
            None,
            AttestedParent::attest_live_for_test(own_pid())
        ),
        Err(RendezvousError::Dial(_))
    ));
}

struct ForkedListener {
    pid: libc::pid_t,
    report: OwnedFd,
    path: PathBuf,
    _dir: aterm_tempfile::TempDir,
}

impl ForkedListener {
    fn start() -> Self {
        let dir = crate::test_scratch::short_dir("a forked listener's socket");
        let path = dir.path().join("l.sock");
        // SAFETY: all-zero `sockaddr_un` is a valid empty socket address.
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        let bytes = path.as_os_str().as_bytes();
        assert!(
            bytes.len() < addr.sun_path.len(),
            "the scratch path fits sun_path"
        );
        addr.sun_family = libc::sa_family_t::try_from(libc::AF_UNIX).expect("AF_UNIX");
        for (slot, byte) in addr.sun_path.iter_mut().zip(bytes) {
            *slot = libc::c_char::from_ne_bytes([*byte]);
        }
        let addr_len = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_un>())
            .expect("sockaddr_un size");
        addr.sun_len = u8::try_from(addr_len).expect("sockaddr_un fits sun_len");
        let (read_end, write_end) = aterm_uds::pipe().expect("report pipe");
        let report = write_end.as_raw_fd();
        let table = aterm_uds::spawnfd::descriptor_bound();

        // Held across the fork alone, so the child copies no neighbour's descriptor between
        // its creation and its close-on-fork mark (`aterm_uds::kept_out`); the report's write
        // end, which the child keeps, loses its own mark for this fork alone.
        let spawning = aterm_uds::spawn_lock::handing(&[report]);
        // SAFETY: child only invokes async-signal-safe syscalls before `_exit`.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            // SAFETY: async-signal-safe syscalls on child-owned descriptors and stack memory before `_exit`.
            unsafe {
                for fd in 3..table {
                    if fd != report {
                        libc::close(fd);
                    }
                }
                let tell = |byte: u8| {
                    libc::write(report, core::ptr::from_ref(&byte).cast(), 1);
                };
                let code: i32 = 'serve: {
                    let listener = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
                    if listener < 0
                        || libc::bind(listener, core::ptr::from_ref(&addr).cast(), addr_len) != 0
                        || libc::listen(listener, 1) != 0
                    {
                        break 'serve 2;
                    }
                    tell(b'L');
                    let mut ready = libc::pollfd {
                        fd: listener,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    if libc::poll(&mut ready, 1, 30_000) != 1 {
                        break 'serve 3;
                    }
                    let served =
                        libc::accept(listener, core::ptr::null_mut(), core::ptr::null_mut());
                    if served < 0 {
                        break 'serve 4;
                    }
                    let mut buf = [0u8; 128];
                    let mut total = 0usize;
                    while total < CLAIM_FRAME_LEN {
                        let mut readable = libc::pollfd {
                            fd: served,
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        if libc::poll(&mut readable, 1, 10_000) != 1 {
                            break;
                        }
                        let read = libc::read(served, buf.as_mut_ptr().cast(), buf.len());
                        if read <= 0 {
                            break;
                        }
                        total += read.unsigned_abs();
                    }
                    tell(u8::try_from(total.min(255)).unwrap_or(255));
                    libc::close(served);
                    0
                };
                libc::_exit(code);
            }
        }
        drop(spawning);
        drop(write_end);
        let listener = Self {
            pid,
            report: read_end.into(),
            path,
            _dir: dir,
        };
        assert_eq!(
            listener.next_report(),
            Some(b'L'),
            "the forked listener is up"
        );
        listener
    }

    fn next_report(&self) -> Option<u8> {
        let mut byte = 0u8;
        // SAFETY: 1-byte read into a stack local from a valid descriptor owned by `self`.
        let read = unsafe {
            libc::read(
                self.report.as_raw_fd(),
                core::ptr::from_mut(&mut byte).cast(),
                1,
            )
        };
        (read == 1).then_some(byte)
    }

    fn claim_bytes_received(&self) -> Option<u8> {
        self.next_report()
    }
}

impl Drop for ForkedListener {
    fn drop(&mut self) {
        // SAFETY: signals and reaps the child PID spawned by `ForkedListener::spawn`.
        unsafe {
            libc::kill(self.pid, libc::SIGKILL);
            let mut status = 0;
            libc::waitpid(self.pid, &mut status, 0);
        }
    }
}

fn deadlines() -> ClaimDeadlines {
    let at = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    ClaimDeadlines {
        dial: at,
        grant: at,
    }
}

const SECRET: &str = "abababababababababababababababababababababababababababababababab";
const NONCE: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn a_same_uid_listener_that_is_not_the_attested_parent_gets_no_claim() {
    let listener = ForkedListener::start();
    let attested_elsewhere = AttestedParent::attest_live_for_test(own_pid());
    let outcome = dial_and_claim(
        &listener.path,
        SECRET,
        NONCE,
        deadlines(),
        attested_elsewhere,
        false,
    );
    match outcome {
        Err(RendezvousError::Dial(reason)) => assert!(
            reason.contains(&format!("(pid {})", listener.pid)),
            "the refusal names the listening process the kernel reported: {reason}"
        ),
        Err(other) => panic!("expected the identity refusal, got {other}"),
        Ok(_) => panic!("expected the identity refusal, got a grant"),
    }
    assert_eq!(
        listener.claim_bytes_received(),
        Some(0),
        "not one byte of the claim reached the impostor"
    );
}

#[test]
fn the_attested_parent_listener_receives_the_claim() {
    let listener = ForkedListener::start();
    let parent = AttestedParent::attest_live_for_test(listener.pid);
    assert!(
        parent.is_some(),
        "the forked listener attests by its birth record"
    );
    let outcome = dial_and_claim(&listener.path, SECRET, NONCE, deadlines(), parent, false);
    match outcome {
        Err(RendezvousError::Grant(_)) => {}
        Err(other) => panic!("expected a grant-stage end after the claim, got {other}"),
        Ok(_) => panic!("the fixture never grants"),
    }
    assert_eq!(
        listener.claim_bytes_received(),
        u8::try_from(CLAIM_FRAME_LEN).ok(),
        "the attested parent received the whole claim frame"
    );
}
