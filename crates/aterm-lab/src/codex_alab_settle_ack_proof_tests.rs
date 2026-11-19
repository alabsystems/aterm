// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn expected() -> Expected {
    Expected {
        sid: "session".into(),
        pid: 42,
        birth: 7,
        source_build: 2026111701,
        build: 2026111801,
        primary: "53001022-0fc4-47bb-a833-8aaba9352269".into(),
    }
}

fn observation(expected: &Expected) -> Observation {
    let plan = Plan {
        operation: "c2b25f5870e3463e8dac61c73180c242".into(),
        primary: expected.primary.clone(),
        build: expected.build,
        pid: expected.pid,
    };
    let readback = Readback {
        operation: plan.operation.clone(),
        primary: plan.primary.clone(),
        build: plan.build,
        pid: plan.pid,
        generation: 2,
        connection: 9,
        state: State::Settled,
    };
    Observation {
        attempt: Attempt {
            sid: expected.sid.clone(),
            plan,
            connection: 8,
        },
        cleanup: Some(Cleanup {
            retained_claim: Some(Claim {
                revision: 1,
                expires_us: 10,
            }),
        }),
        readback: Some(readback),
        settle_requests: 1,
        commits: 1,
    }
}

#[test]
fn settlement_requires_one_request_one_commit_and_the_same_operation_after_eof() {
    let expected = expected();
    let outcome = observation(&expected);
    assert!(outcome.complete(&expected).unwrap());

    let mut pending = outcome.clone();
    pending.cleanup = None;
    pending.readback.as_mut().unwrap().state = State::Waiting;
    assert!(!pending.complete(&expected).unwrap());
    pending.readback.as_mut().unwrap().state = State::Settled;
    assert!(pending.complete(&expected).is_err());

    let mut duplicate = outcome.clone();
    duplicate.settle_requests = 2;
    assert!(duplicate.complete(&expected).is_err());
    let mut changed = outcome.clone();
    changed.readback.as_mut().unwrap().operation = "0".repeat(32);
    assert!(changed.complete(&expected).is_err());
    let mut wrong_socket = outcome.clone();
    wrong_socket.readback.as_mut().unwrap().connection = outcome.attempt.connection;
    assert!(wrong_socket.complete(&expected).is_err());
    let mut lost_claim = outcome;
    lost_claim.cleanup.as_mut().unwrap().retained_claim = None;
    assert!(lost_claim.complete(&expected).is_err());
}

#[test]
fn guardian_frames_are_closed_and_correlate_the_original_request() -> io::Result<()> {
    let world = aterm_tempfile::Builder::new()
        .prefix("settle-guardian-")
        .tempdir_in("/tmp")?;
    let listener = Listener::bind(world.path())?;
    let socket = listener.path.clone();
    let mut host = CtlStream::connect(&socket)?;
    let pid = std::process::id();
    let mut guardian = listener.connect_host(
        i32::try_from(pid).map_err(io::Error::other)?,
        &std::env::current_exe()?,
    )?;
    assert!(
        !socket.exists(),
        "the consumed listener removes only its socket name"
    );
    guardian.nonce = "f3dce53cb65246fe".into();
    let expected = expected();
    let wire = aterm_json::to_vec(&Command {
        schema: 1,
        request: "f3dce53cb65246fe0000000000000001",
        action: Action::ArmSettleAckLoss,
        expect: Some(&expected),
    })
    .unwrap();
    let value: aterm_json::Value = aterm_json::from_slice(&wire).unwrap();
    assert_eq!(value["action"].as_str(), Some("arm-settle-ack-loss"));
    assert_eq!(value["expect"]["sourceBuild"].as_u64(), Some(2026111701));
    assert!(value["expect"].get("operation").is_none());

    // Use a real held socket. A parseable response addressed to a different
    // request must not become an acknowledgement of the original command.
    host.write_all(
        b"{\"schema\":1,\"request\":\"f3dce53cb65246fe0000000000000002\",\"action\":\"arm-settle-ack-loss\",\"outcome\":\"ok\",\"settleAckLoss\":null}\n",
    )?;
    let error = guardian
        .exchange(
            Action::ArmSettleAckLoss,
            Some(&expected),
            Instant::now() + EXCHANGE,
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("original request"));

    assert!(aterm_json::from_slice::<Reply>(
        br#"{"schema":1,"request":"f3dce53cb65246fe0000000000000001","action":"observe","outcome":"ok"}"#
    ).is_err());
    assert!(aterm_json::from_slice::<Reply>(
        br#"{"schema":1,"request":"f3dce53cb65246fe0000000000000001","action":"observe","outcome":"ok","settleAckLoss":null,"guessed":true}"#
    ).is_err());
    Ok(())
}
