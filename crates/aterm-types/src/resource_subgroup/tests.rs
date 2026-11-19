// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;

fn request() -> Request {
    let job = Binding {
        session: "tqn5r6e0148".into(),
        attempt: [1; 16],
    };
    Request {
        job: job.clone(),
        capability: Capability::from_bytes([7; 32]),
        member: Submission {
            binding: Binding {
                attempt: [2; 16],
                ..job
            },
            peer_pid: 42,
            peer_birth: 43,
            argv: vec![b"/bin/tool".to_vec(), b"a\xff\n".to_vec()],
            cwd: b"/work/\xfe".to_vec(),
            env: vec![(b"KEY".to_vec(), b"\xfd\n".to_vec())],
            cpu_units: 2,
            memory_bytes: 100,
            label: "member".into(),
            timeout_ms: Some(80),
        },
        cpu: CpuDelegation::SharedBorrow { target_fd: 8 },
        gnu: GnuTransport::NamedFifo {
            locator: b"/private/fifo-\xff".to_vec(),
            witness: FifoWitness {
                device: 10,
                inode: 11,
            },
            descriptors: 2,
        },
        fence_targets: vec![10, 11],
    }
}
#[test]
fn lossless_job_bound_codec_and_transport_variants_reject_every_truncation() {
    let mut r = request();
    let bytes = r.encode().unwrap();
    assert!(r == Request::decode(&bytes, &r.job).unwrap());
    assert_eq!(r.fd_count().unwrap(), 9);
    for end in 0..bytes.len() {
        assert!(Request::decode(&bytes[..end], &r.job).is_err());
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(Request::decode(&extra, &r.job).is_err());
    let wrong = Binding {
        attempt: [3; 16],
        ..r.job.clone()
    };
    assert!(Request::decode(&bytes, &wrong).is_err());
    r.gnu = GnuTransport::NumericPipe {
        read_target: 4,
        write_target: 5,
    };
    assert!(r == Request::decode(&r.encode().unwrap(), &r.job).unwrap());
    r.cpu = CpuDelegation::FixedBorrow { units: 2 };
    r.gnu = GnuTransport::None;
    assert_eq!(r.fd_count().unwrap(), 6);
    r.fence_targets = (20..20 + MAX_FENCES as u32).collect();
    r.cpu = CpuDelegation::SharedBorrow { target_fd: 8 };
    r.gnu = GnuTransport::NumericPipe {
        read_target: 4,
        write_target: 5,
    };
    assert_eq!(r.fd_count().unwrap(), 134);
    assert!(r == Request::decode(&r.encode().unwrap(), &r.job).unwrap());
    r.fence_targets.push(200);
    assert!(r.validate().is_err());
}
#[test]
fn ambiguous_targets_replayed_root_and_missing_capability_refuse() {
    let mut r = request();
    assert!(r.validate().is_ok());
    r.fence_targets.push(8);
    assert!(r.validate().is_err());
    r.fence_targets.clear();
    assert!(r.validate().is_ok());
    r.member.binding = r.job.clone();
    assert!(r.validate().is_err());
    r.member.binding.attempt = [2; 16];
    assert!(r.validate().is_ok());
    r.capability = Capability::from_bytes([0; 32]);
    assert!(r.validate().is_err());
    r.capability = Capability::from_bytes([1; 32]);
    assert!(r.validate().is_ok());
    r.gnu = GnuTransport::NamedFifo {
        locator: b"/fifo".to_vec(),
        witness: FifoWitness {
            device: 0,
            inode: 1,
        },
        descriptors: 3,
    };
    assert!(r.validate().is_err());
}
#[test]
fn private_job_context_is_canonical_and_never_a_new_root_request() {
    for (session, length) in [("t0123456789", 112), ("s-000000000000000000ab", 123)] {
        let context = JobContext {
            job: Binding {
                session: session.into(),
                attempt: [9; 16],
            },
            capability: Capability::from_bytes([7; 32]),
        };
        let encoded = context.encode().unwrap();
        assert_eq!(encoded.len(), length);
        let decoded = JobContext::decode(&encoded).unwrap();
        assert_eq!(decoded.job, context.job);
        assert_eq!(decoded.capability.as_bytes(), context.capability.as_bytes());
        assert_eq!(decoded.encode().unwrap(), encoded);
        for invalid in [
            format!("{encoded}:extra"),
            encoded.replacen("v3:", "v2:", 1),
            encoded.to_uppercase(),
            format!("v3:{session}:{}:{}", "09".repeat(16), "00".repeat(32)),
            format!(
                "v3:s-000000000000000000a:{}:{}",
                "09".repeat(16),
                "07".repeat(32)
            ),
            format!(
                "v3:s-000000000000000000AB:{}:{}",
                "09".repeat(16),
                "07".repeat(32)
            ),
            "x".repeat(166),
        ] {
            assert!(JobContext::decode(&invalid).is_err());
        }
        for size in 0..encoded.len() {
            assert!(JobContext::decode(&encoded[..size]).is_err());
        }
    }

    // Both local identity forms keep the fixed-size attempt and capability fields. The
    // longest legacy identity is 11 bytes longer; the existing decoder cap is unchanged.
    for (session, length) in [("tzzzzzzzzzz", 112), ("s-ffffffffffffffffffff", 123)] {
        let maximal = JobContext {
            job: Binding {
                session: session.into(),
                attempt: [9; 16],
            },
            capability: Capability::from_bytes([7; 32]),
        }
        .encode()
        .unwrap();
        assert_eq!(maximal.len(), length);
        assert!(JobContext::decode(&maximal).is_ok());
    }
}
