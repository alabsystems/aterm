// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;
use std::time::Duration;

/// A push's exit as git prints it on stderr.
fn exited(stderr: &str) -> ChildOut {
    ChildOut {
        code: Some(1),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

/// ONLY CURL'S LOW-SPEED ABORT OF A POST, UNDER A PROGRESS WINDOW, IS A TRANSFER THE WINDOW ENDED:
/// git's `RPC failed; curl 28 Operation too slow …` for the POST that carries the pack names the
/// window. NEGATIVE CONTROLS: the same exit of a run with no window (agh set no low-speed bound),
/// ref discovery's GET aborted the same way before any pack (`unable to access`), and the other
/// receive failures (an HTTP error, a reset, a connection that timed out or was refused, a remote
/// that hung up) name none, and go to the receive-failure recovery as before.
#[test]
fn only_a_low_speed_abort_of_a_post_under_a_window_is_a_stalled_transfer() {
    let window = Some(Duration::from_secs(60));
    let stalled = "error: RPC failed; curl 28 Operation too slow. Less than 1 bytes/sec \
                   transferred the last 60 seconds\n\
                   send-pack: unexpected disconnect while reading sideband packet\n\
                   fatal: the remote end hung up unexpectedly\n";
    assert_eq!(
        transfer_stalled(window, &exited(stalled)),
        Some(Duration::from_secs(60))
    );
    assert_eq!(transfer_stalled(None, &exited(stalled)), None);
    for other in [
        "fatal: unable to access 'https://github.com/o/r.git/': Operation too slow. Less than 1 \
         bytes/sec transferred the last 60 seconds\n",
        "error: RPC failed; HTTP 500 curl 22 The requested URL returned error: 500\n\
         fatal: the remote end hung up unexpectedly\n",
        "error: RPC failed; curl 56 Recv failure: Connection reset by peer\n",
        "error: RPC failed; curl 28 Connection timed out after 300000 milliseconds\n",
        "fatal: unable to access 'https://github.com/o/r.git/': Failed to connect to github.com \
         port 443 after 2 ms: Couldn't connect to server\n",
        "fatal: the remote end hung up unexpectedly\n",
        "",
    ] {
        assert_eq!(transfer_stalled(window, &exited(other)), None, "{other}");
    }
}

/// A STALLED TRANSFER'S ERROR NAMES THE STALL AND HAS AN UNKNOWN SIDE EFFECT: `stalled_transfer`
/// puts the window's words before the exit's (here the wording `failed` gives a sent atomic push
/// with no complete rejection), keeps its could-not-run kind and sets the side effect Unknown,
/// which settles an intent Unknown. This holds only `stalled_transfer`'s own effect:
/// `push_native_refs` returns it from its receive closure, whose `map_err` rewraps it as
/// `OutcomeUnknown` (`native receive remains uncertain: …`), the words kept.
#[test]
fn a_stalled_transfer_is_of_unknown_outcome() {
    let exit = "atomic rejection lacks complete request-bound evidence: error: RPC failed; curl 28 \
                Operation too slow";
    let error = stalled_transfer(Error::could_not_run(exit), Duration::from_secs(60));
    assert_eq!(error.kind, Kind::CouldNotRun);
    assert_eq!(error.side_effect, SideEffect::Unknown);
    assert!(
        error.why.contains("60s") && error.why.ends_with(exit),
        "{}",
        error.why
    );
    assert!(matches!(
        crate::write::settle_error(&error),
        crate::journal::Settled::Unknown { .. }
    ));
}

/// A RE-READ AGH CANNOT PARSE KEEPS THE FAILED PUSH'S WORDS: a receive-failure re-read whose
/// output may be truncated, is not UTF-8, or holds a malformed or unguarded row is a protocol
/// fault of unknown side effect whose words start with the failed push's (here a stalled
/// transfer's), as a re-read that fails keeps them. NEGATIVE CONTROL: a well-formed read of the
/// guarded refs returns them.
#[test]
fn a_re_read_agh_cannot_parse_keeps_the_failed_push_words() {
    let prior = stalled_transfer(
        Error::could_not_run("atomic rejection lacks complete request-bound evidence"),
        Duration::from_secs(60),
    );
    let sha = "1".repeat(40);
    let guards = ["refs/tags/v1", "refs/heads/main"];
    let read = |stdout: Vec<u8>| ChildOut {
        code: Some(0),
        stdout,
        stderr: Vec::new(),
    };
    let good = read(format!("{sha}\trefs/tags/v1\n").into_bytes());
    let observed = recovered_refs(&good, &guards, &prior).expect("a well-formed re-read");
    assert_eq!(
        observed.into_iter().collect::<Vec<_>>(),
        [("refs/tags/v1", sha.as_str())]
    );
    for (case, stdout) in [
        ("truncated", vec![b'x'; ChildOut::OUTPUT_KEEP]),
        ("non-UTF-8", vec![0xff]),
        ("malformed", b"no tab\n".to_vec()),
        (
            "unguarded",
            format!("{sha}\trefs/tags/other\n").into_bytes(),
        ),
    ] {
        let error = recovered_refs(&read(stdout), &guards, &prior).unwrap_err();
        assert_eq!(
            (error.kind, error.side_effect),
            (Kind::CouldNotRun, SideEffect::Unknown),
            "{case}"
        );
        assert!(
            matches!(error.sub, Sub::Protocol),
            "{case}: {:?}",
            error.sub
        );
        assert!(
            error.why.starts_with(&prior.why) && error.why.contains("moved less than a byte"),
            "{case}: {}",
            error.why
        );
    }
}
