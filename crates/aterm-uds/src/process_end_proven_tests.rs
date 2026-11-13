// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    Birth, EndControl, EndRefusal, Ended, Identity, KEEPS_PID_VERSIONS, end_proven,
    signal_by_handle,
};
use std::time::Duration;

fn ident(pid: i32, idversion: i32) -> Identity {
    Identity {
        pid,
        birth: Birth(1_792_000_000_000_042),
        idversion: Some(idversion),
    }
}

struct Fake {
    reads: std::cell::RefCell<std::vec::IntoIter<Option<Identity>>>,
    signals: Vec<(i32, i32)>,
    naps: usize,
    refuses: Option<i32>,
}
impl Fake {
    fn new(reads: Vec<Option<Identity>>) -> Self {
        Self {
            reads: std::cell::RefCell::new(reads.into_iter()),
            signals: Vec::new(),
            naps: 0,
            refuses: None,
        }
    }
}
impl EndControl for Fake {
    fn identity(&mut self, _pid: i32) -> Option<Identity> {
        self.reads.borrow_mut().next().flatten()
    }
    fn signal(&mut self, token: &Identity, sig: i32) -> std::io::Result<()> {
        if self.refuses == Some(sig) {
            return Err(std::io::Error::from_raw_os_error(3));
        }
        self.signals.push((token.pid, sig));
        Ok(())
    }
    fn nap(&mut self, _pause: Duration) {
        self.naps += 1;
    }
}

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

#[test]
fn a_process_that_exits_within_the_grace_takes_one_sigterm() {
    let t = ident(4242, 7);
    let mut c = Fake::new(vec![Some(t), Some(t), None]);
    let out = end_proven(t, Duration::from_secs(60), &mut c, |_| true).expect("ended");
    assert_eq!(
        out,
        Ended {
            termed: true,
            killed: false,
            gone: true
        }
    );
    assert_eq!(c.signals, [(4242, SIGTERM)]);
}

#[test]
fn a_process_that_outlives_the_grace_is_sigkilled() {
    let t = ident(4242, 7);
    let mut c = Fake::new(vec![Some(t), Some(t), Some(t), Some(t), Some(t), None]);
    let out = end_proven(t, Duration::ZERO, &mut c, |_| true).expect("ended");
    assert_eq!(
        out,
        Ended {
            termed: true,
            killed: true,
            gone: true
        }
    );
    assert_eq!(c.signals, [(4242, SIGTERM), (4242, SIGKILL)]);
}

#[test]
fn a_moved_identity_before_the_term_sends_no_signal() {
    let t = ident(4242, 7);
    let mut c = Fake::new(vec![Some(ident(4242, 8)), Some(ident(4242, 8))]);
    assert_eq!(
        end_proven(t, Duration::from_secs(60), &mut c, |_| true),
        Err(EndRefusal::Stale)
    );
    assert!(c.signals.is_empty(), "no signal after a moved identity");
}

#[test]
fn a_moved_identity_before_the_kill_sends_no_second_signal() {
    let t = ident(4242, 7);
    let mut c = Fake::new(vec![Some(t), Some(t), Some(t), Some(ident(4242, 8))]);
    let out = end_proven(t, Duration::ZERO, &mut c, |_| true).expect("ended");
    assert_eq!(
        out,
        Ended {
            termed: true,
            killed: false,
            gone: false
        }
    );
    assert_eq!(c.signals, [(4242, SIGTERM)]);
}

#[test]
fn a_lost_witness_before_the_kill_sends_no_second_signal() {
    let t = ident(4242, 7);
    let mut c = Fake::new(vec![Some(t), Some(t), Some(t), Some(t)]);
    let mut judged = 0;
    let out = end_proven(t, Duration::ZERO, &mut c, |_| {
        judged += 1;
        judged == 1
    })
    .expect("termed");
    assert_eq!(
        out,
        Ended {
            termed: true,
            killed: false,
            gone: false
        }
    );
    assert_eq!(c.signals, [(4242, SIGTERM)]);
    assert_eq!(judged, 2, "the tier is judged before each signal");
}

#[test]
fn a_lost_witness_sends_no_signal() {
    let t = ident(4242, 7);
    let mut c = Fake::new(vec![Some(t)]);
    assert_eq!(
        end_proven(t, Duration::from_secs(60), &mut c, |_| false),
        Err(EndRefusal::NotProven)
    );
    assert!(c.signals.is_empty());
}

#[test]
fn an_exec_during_the_judgment_sends_no_signal() {
    let t = ident(4242, 7);
    let mut c = Fake::new(vec![Some(t), Some(ident(4242, 8))]);
    assert_eq!(
        end_proven(t, Duration::from_secs(60), &mut c, |_| true),
        Err(EndRefusal::Stale)
    );
    assert!(c.signals.is_empty(), "no signal after an exec mid-judgment");
}

#[test]
fn a_process_that_execs_after_its_sigterm_is_not_gone() {
    let t = ident(4242, 7);
    let mut c = Fake::new(vec![Some(t), Some(t), Some(ident(4242, 8))]);
    let out = end_proven(t, Duration::from_secs(60), &mut c, |_| true).expect("termed");
    assert_eq!(
        out,
        Ended {
            termed: true,
            killed: false,
            gone: false
        }
    );
    assert_eq!(c.signals, [(4242, SIGTERM)]);
    let mut reborn = ident(4242, 8);
    reborn.birth.0 += 1;
    let mut c = Fake::new(vec![Some(t), Some(t), Some(reborn)]);
    let out = end_proven(t, Duration::from_secs(60), &mut c, |_| true).expect("termed");
    assert!(out.gone, "{out:?}");
}

#[test]
fn a_sigkilled_process_that_still_reads_is_not_gone() {
    let t = ident(4242, 7);
    let mut c = Fake::new(vec![Some(t), Some(t), Some(t), Some(t), Some(t), Some(t)]);
    let out = end_proven(t, Duration::ZERO, &mut c, |_| true).expect("ended");
    assert_eq!(
        out,
        Ended {
            termed: true,
            killed: true,
            gone: false
        }
    );
}

#[test]
fn a_signal_refused_for_a_process_just_gone_reads_gone() {
    let t = ident(4242, 7);
    let mut c = Fake::new(vec![Some(t), Some(t), None]);
    c.refuses = Some(SIGTERM);
    let out = end_proven(t, Duration::from_secs(60), &mut c, |_| true);
    assert_eq!(
        out,
        Ok(Ended {
            termed: false,
            killed: false,
            gone: true
        })
    );
    let mut c = Fake::new(vec![Some(t), Some(t), Some(t), Some(t), Some(t), None]);
    c.refuses = Some(SIGKILL);
    let out = end_proven(t, Duration::ZERO, &mut c, |_| true);
    assert_eq!(
        out,
        Ok(Ended {
            termed: true,
            killed: false,
            gone: true
        })
    );
    let mut c = Fake::new(vec![Some(t), Some(t), Some(t)]);
    c.refuses = Some(SIGTERM);
    assert_eq!(
        end_proven(t, Duration::from_secs(60), &mut c, |_| true),
        Err(EndRefusal::SignalFailed)
    );
}

#[test]
fn a_token_is_pid_birth_and_version() {
    assert_eq!(
        ident(63452, 331207).token(),
        "63452@1792000000000042:331207"
    );
}

/// A token's two forms: Apple's `<pid>@<birth_us>:<idversion>`, Linux's `<pid>@<start_ticks>`
/// with no version. Negative control: print `idversion.unwrap_or(0)` always, and the
/// versionless token reads `…:0`, a version Linux never had.
#[test]
fn a_token_prints_its_version_only_where_the_kernel_keeps_one() {
    let apple = Identity {
        pid: 4242,
        birth: Birth(1_792_000_000_000_042),
        idversion: Some(7),
    };
    assert_eq!(apple.token(), "4242@1792000000000042:7");
    let linux = Identity {
        pid: 4242,
        birth: Birth(51_832_760),
        idversion: None,
    };
    assert_eq!(linux.token(), "4242@51832760");
}

#[test]
fn a_token_round_trips_its_identity() {
    let apple = ident(63452, 331_207);
    assert_eq!(Identity::parse_shaped(&apple.token(), true), Some(apple));
    let linux = Identity {
        pid: 4242,
        birth: Birth(51_832_760),
        idversion: None,
    };
    assert_eq!(Identity::parse_shaped(&linux.token(), false), Some(linux));
    let here = if KEEPS_PID_VERSIONS { apple } else { linux };
    assert_eq!(Identity::parse(&here.token()), Some(here));
}

#[test]
fn a_bare_pid_a_pattern_or_a_word_is_no_token() {
    for versioned in [true, false] {
        for bad in [
            "",
            "4242",
            "all",
            "*",
            "4242@*",
            "42*@1792000000000042:7",
            "aterm@1792000000000042:7",
            "1@1792000000000042:7",
            "0@1792000000000042:7",
            "-4242@1792000000000042:7",
            "+4242@1792000000000042:7",
            "4242@+1792000000000042:7",
            "4242@1792000000000042:-7",
            "4242@1792000000000042:+7",
            " 4242@1792000000000042:7",
            "4242@1792000000000042:7 ",
            "4242 @1792000000000042:7",
            "04242@1792000000000042:7",
            "4242@01792000000000042:7",
            "4242@1792000000000042:07",
            "99999999999@1792000000000042:7",
            "4242@99999999999999999999999:7",
            "4242@1792000000000042:99999999999",
            "4242@1792000000000042:7:8",
            "4242@1792000000000042:7@9",
            "4242@1792000000000042@9",
            "4242@",
            "@1792000000000042:7",
            "4242@1792000000000042:",
            "4242@51832760:",
        ] {
            assert_eq!(Identity::parse_shaped(bad, versioned), None, "{bad:?}");
        }
    }
}

/// Negative control: let the versioned shape take a token with no `:<version>`, and the
/// Linux-shaped token reads as an Apple one.
#[cfg(target_vendor = "apple")]
#[test]
fn a_versionless_token_is_refused_where_the_kernel_keeps_versions() {
    assert_eq!(Identity::parse("4242@51832760"), None);
    assert_eq!(
        Identity::parse("4242@1792000000000042:7"),
        Some(ident(4242, 7))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_versioned_token_is_refused_where_the_kernel_keeps_none() {
    assert_eq!(Identity::parse("4242@1792000000000042:7"), None);
    assert_eq!(
        Identity::parse("4242@51832760"),
        Some(Identity {
            pid: 4242,
            birth: Birth(51_832_760),
            idversion: None,
        })
    );
}

/// What [`signal_by_handle`] did, in order, with what it passed: the pid opened and read,
/// the handle and signal sent, the pid and signal killed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Open(i32),
    Read(i32),
    Send(u8, i32),
    Kill(i32, i32),
}

/// The pid opened, read and signalled, over a handle that is a plain number: `open` answers
/// `opened`, and the read answers `reads`.
fn by_handle(
    token: Identity,
    opened: std::io::Result<u8>,
    reads: Option<Identity>,
) -> (std::io::Result<()>, Vec<Step>) {
    let log = std::cell::RefCell::new(Vec::new());
    let result = signal_by_handle(
        &token,
        15,
        |pid| {
            log.borrow_mut().push(Step::Open(pid));
            opened
        },
        |pid| {
            log.borrow_mut().push(Step::Read(pid));
            reads
        },
        |handle: &u8, sig| {
            log.borrow_mut().push(Step::Send(*handle, sig));
            Ok(())
        },
        |pid, sig| {
            log.borrow_mut().push(Step::Kill(pid, sig));
            Ok(())
        },
    );
    (result, log.into_inner())
}

/// The handle is opened on the token's pid first, that pid's identity re-read after it, and
/// the signal goes through the handle only while the read still names the token. An open the
/// kernel will not make (`ENOSYS`, or a seccomp filter's `EPERM`) takes the same read before
/// a `kill` of that pid; an open that finds the process gone sends and reads nothing.
/// Negative controls: ignore the re-read's answer, and the pid reused after the open is
/// signalled; fall back on `ENOSYS` alone, and the filtered open sends nothing.
#[test]
fn a_handle_signal_reaches_only_the_process_the_handle_was_opened_on() {
    let token = Identity {
        pid: 4242,
        birth: Birth(51_832_760),
        idversion: None,
    };
    let reused = Identity {
        birth: Birth(51_900_000),
        ..token
    };
    let (opened, read) = (Step::Open(4242), Step::Read(4242));
    let (sent, log) = by_handle(token, Ok(3), Some(token));
    assert!(sent.is_ok(), "{sent:?}");
    assert_eq!(log, [opened, read, Step::Send(3, 15)]);
    let (sent, log) = by_handle(token, Ok(3), Some(reused));
    assert!(sent.is_err(), "a reused pid is refused");
    assert_eq!(log, [opened, read], "nothing sent to a reused pid");
    let (sent, log) = by_handle(token, Ok(3), None);
    assert!(sent.is_err());
    assert_eq!(log, [opened, read], "nothing sent to a gone pid");
    for kind in [
        std::io::ErrorKind::Unsupported,
        std::io::ErrorKind::PermissionDenied,
    ] {
        let (sent, log) = by_handle(token, Err(kind.into()), Some(token));
        assert!(sent.is_ok(), "{kind:?}: {sent:?}");
        assert_eq!(log, [opened, read, Step::Kill(4242, 15)], "{kind:?}");
        let (sent, log) = by_handle(token, Err(kind.into()), Some(reused));
        assert!(sent.is_err(), "{kind:?}");
        assert_eq!(log, [opened, read], "{kind:?}: no kill of a reused pid");
    }
    let (sent, log) = by_handle(
        token,
        Err(std::io::Error::from_raw_os_error(3)),
        Some(token),
    );
    assert_eq!(sent.map_err(|e| e.raw_os_error()), Err(Some(3)));
    assert_eq!(log, [opened], "a gone process is neither read nor sent");
}
