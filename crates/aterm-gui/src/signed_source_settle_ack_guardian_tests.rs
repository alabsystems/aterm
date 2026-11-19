// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_uds::CtlListener;
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::fs::PermissionsExt as _;

const SID: &str = "s-a0";
const OPERATION: &str = "0ca23613657842a2820f753733c849f6";
const PRIMARY: &str = "b2c5bd23-907f-49e8-8db4-c226fea42b52";
const EXPECTATION: &str = r#"{"sid":"s-a0","pid":42,"birth":31,"sourceBuild":2026111701,"build":2026111801,"primary":"b2c5bd23-907f-49e8-8db4-c226fea42b52"}"#;

#[test]
fn signed_guardian_requires_real_private_world_and_canonical_marker() -> io::Result<()> {
    let temp = aterm_tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let config = root.join("cfg");
    let runtime = root.join("run");
    for dir in [&home, &config, &runtime] {
        std::fs::DirBuilder::new().mode(0o700).create(dir)?;
    }
    let marker = aterm_types::world::state_home(&home).unwrap();
    let socket = root.join("sa.sock");
    let _listener = CtlListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    assert!(validate_world(&socket, &runtime, &home, &config, &runtime, &marker).is_err());
    private_file::write_private(&root, "lab-world", b"aterm-lab-world/1\n")?;
    assert!(validate_world(&socket, &runtime, &home, &config, &runtime, &marker).is_ok());
    assert!(validate_world(&socket, &config, &home, &config, &runtime, &marker).is_err());
    assert!(validate_world(&socket, &runtime, &home, &config, &runtime, &root).is_err());
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666))?;
    assert!(validate_world(&socket, &runtime, &home, &config, &runtime, &marker).is_err());
    Ok(())
}

#[test]
fn signed_guardian_uses_closed_wire_and_rejects_replay_and_changed_namespace() {
    let arm = format!(
        r#"{{"schema":1,"request":"ad59516a82bcb20e0000000000000001","action":"arm-settle-ack-loss","expect":{EXPECTATION}}}"#
    );
    let arm: Command = aterm_json::from_str(&arm).unwrap();
    assert_eq!(arm.expect.as_ref().unwrap().original_peer(), (SID, 42, 31));
    let mut sequence = Sequence::default();
    sequence.accept(&arm).unwrap();
    assert!(sequence.accept(&arm).is_err());
    let other: Command = aterm_json::from_str(
        r#"{"schema":1,"request":"0d59516a82bcb20e0000000000000002","action":"observe"}"#,
    )
    .unwrap();
    assert!(sequence.accept(&other).is_err());
    let next: Command = aterm_json::from_str(
        r#"{"schema":1,"request":"ad59516a82bcb20e0000000000000002","action":"observe"}"#,
    )
    .unwrap();
    sequence.accept(&next).unwrap();
    for raw in [
        r#"{"schema":1,"request":"ad59516a82bcb20e0000000000000003","action":"observe","expect":null}"#,
        r#"{"schema":1,"request":"ad59516a82bcb20e0000000000000003","action":"unknown"}"#,
        r#"{"schema":1,"request":"ad59516a82bcb20e0000000000000003","action":"observe","operation":"foreign"}"#,
    ] {
        assert!(aterm_json::from_str::<Command>(raw).is_err());
    }
    let response = aterm_json::to_vec(&Reply {
        schema: 1,
        request: &next.request,
        action: next.action,
        outcome: "ok",
        observation: None,
    })
    .unwrap();
    assert_eq!(
        response.as_slice(),
        br#"{"schema":1,"request":"ad59516a82bcb20e0000000000000002","action":"observe","outcome":"ok","settleAckLoss":null}"#
    );
}

#[test]
fn signed_guardian_observes_cleanup_only_on_the_held_connection_after_eof() -> io::Result<()> {
    let fault = Arc::new(Fault::new(SID.into()));
    fault
        .arm(aterm_json::from_str(EXPECTATION).unwrap())
        .unwrap();
    settle_ack_fault::observe_dispatch(&fault, || {
        settle_ack_fault::record_prepare(settle_ack_fault::NativeObservation {
            sid: SID,
            operation: OPERATION,
            primary: PRIMARY,
            source_build: 2026111701,
            build: 2026111801,
            pid: 42,
            birth: 31,
            generation: 1,
        });
    });
    let attempt = fault
        .select(
            &format!("@{SID} native-act settle source-exec operation={OPERATION}"),
            42,
            7,
        )
        .unwrap();
    let mut out = Outbox::default();
    out.extend_from_slice(
        format!("OK native source-exec state=settled operation={OPERATION} primary={PRIMARY} build=2026111801\n").as_bytes(),
    );
    let (sdk, peer) = CtlStream::pair()?;
    let guardian = Guardian {
        fault: OnceLock::from(Arc::clone(&fault)),
        teardown: Mutex::new(None),
        stopping: AtomicBool::new(false),
    };
    assert!(fault.hold(&attempt, out.fixture_unsent_status().unwrap(), 2));
    drop(std::mem::take(&mut out));
    drop(peer);
    settle_ack_fault::await_peer_eof(
        &sdk,
        false,
        &guardian.stopping,
        Instant::now() + Duration::from_secs(1),
    )?;
    *guardian.teardown.lock().unwrap() = Some((7, attempt));

    let report_cleanup = || {
        assert!(guardian.teardown.try_lock().is_ok());
        settle_ack_fault::record_eof(SID, OPERATION, 42, 7, 23, 110);
    };
    guardian.release(8, report_cleanup);
    let pending = aterm_json::to_value(&guardian.snapshot().unwrap()).unwrap();
    assert!(pending["cleanup"].is_null());
    guardian.release(7, report_cleanup);
    let received = aterm_json::to_value(&guardian.snapshot().unwrap()).unwrap();
    assert_eq!(
        received["cleanup"]["retained_claim"]["revision"].as_u64(),
        Some(23)
    );
    assert_eq!(received["settleRequests"].as_u64(), Some(1));
    Ok(())
}
