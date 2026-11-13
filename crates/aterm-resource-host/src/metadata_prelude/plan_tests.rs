// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn exec_data_is_lossless_bounded_and_contains_no_admission_fields() {
    let plan = ExecPlan {
        footprint_mib: None,
        seal: Seal::NoFork,
        parent: transport::Peer {
            pid: 123,
            birth: 456,
        },
        targets: vec![3, 4, 5, 6, 7, 8, 9],
        command: CommandSpec {
            program: b"/native/bin/query".to_vec(),
            arg0: Some(vec![b'x', 0xff]),
            args: vec![vec![b'-', 0xff]],
            environment: vec![(b"LANG".to_vec(), b"C".to_vec())],
            cwd: b"/private/work".to_vec(),
        },
    };
    let bytes = plan.encode().unwrap();
    let decoded = ExecPlan::decode(&bytes).unwrap();
    assert_eq!(decoded.parent, plan.parent);
    assert!(decoded.command == plan.command);
    assert_eq!(decoded.targets, plan.targets);
    assert_eq!(decoded.footprint_mib, plan.footprint_mib);
    assert!(decoded.seal == plan.seal);
    for end in 0..bytes.len() {
        assert!(ExecPlan::decode(&bytes[..end]).is_err());
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(ExecPlan::decode(&extra).is_err());
    assert!(aterm_types::resource_admission::Submission::decode(&bytes).is_err());
    let mut old = bytes.clone();
    old[..8].copy_from_slice(b"AMEX0001");
    assert!(ExecPlan::decode(&old).is_err());
    old[..8].copy_from_slice(b"AMEX0002");
    assert!(ExecPlan::decode(&old).is_err());
    old[..8].copy_from_slice(b"AMEX0003");
    assert!(ExecPlan::decode(&old).is_err());
    let mut wrong_seal = bytes.clone();
    *wrong_seal.last_mut().unwrap() = 2;
    assert!(ExecPlan::decode(&wrong_seal).is_err());
    let mut unknown = bytes.clone();
    unknown[20] = 2;
    assert!(ExecPlan::decode(&unknown).is_err());
    for mib in [0, i32::MAX as u32 + 1, u32::MAX] {
        let mut invalid = bytes.clone();
        invalid[20] = 1;
        invalid.splice(21..21, mib.to_be_bytes());
        assert!(ExecPlan::decode(&invalid).is_err());
    }
    let mut limited = ExecPlan::decode(&bytes).unwrap();
    limited.footprint_mib = Some(64);
    #[cfg(target_os = "macos")]
    {
        let encoded = limited.encode().unwrap();
        assert_eq!(ExecPlan::decode(&encoded).unwrap().footprint_mib, Some(64));
        for end in 0..encoded.len() {
            assert!(ExecPlan::decode(&encoded[..end]).is_err());
        }
        let mut trailing = encoded;
        trailing.push(0);
        assert!(ExecPlan::decode(&trailing).is_err());
    }
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        limited.encode().unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    let mut changed = decoded;
    changed.targets.push(10);
    assert!(changed.encode().is_err());
    changed.targets.pop();
    changed.command.arg0.as_mut().unwrap().push(0);
    assert!(changed.encode().is_err());
    changed.command.arg0.as_mut().unwrap().pop();
    changed.command.environment[0].1.push(0);
    assert!(changed.encode().is_err());
    changed.command.environment[0].1 = vec![b'x'; MAX_PLAN + 1];
    assert!(changed.encode().is_err());
}
