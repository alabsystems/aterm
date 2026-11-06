// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;

fn request() -> Request {
    let job = Binding {
        session: "s-ab12".into(),
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
    r.fence_targets.push(8);
    assert!(r.validate().is_err());
    r.fence_targets.clear();
    r.member.binding = r.job.clone();
    assert!(r.validate().is_err());
    r.member.binding.attempt = [2; 16];
    r.capability = Capability::from_bytes([0; 32]);
    assert!(r.validate().is_err());
    r.capability = Capability::from_bytes([1; 32]);
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
    let context = JobContext {
        job: Binding {
            session: "s-ab".into(),
            attempt: [9; 16],
        },
        capability: Capability::from_bytes([7; 32]),
    };
    let encoded = context.encode().unwrap();
    let decoded = JobContext::decode(&encoded).unwrap();
    assert_eq!(decoded.job, context.job);
    assert_eq!(decoded.capability.as_bytes(), context.capability.as_bytes());
    for invalid in [
        format!("{encoded}:extra"),
        encoded.replacen("v3:", "v2:", 1),
        encoded.to_uppercase(),
        format!("v3:s-ab:{}:{}", "09".repeat(16), "00".repeat(32)),
    ] {
        assert!(JobContext::decode(&invalid).is_err());
    }
    for size in 0..encoded.len() {
        assert!(JobContext::decode(&encoded[..size]).is_err());
    }
    let maximal = JobContext {
        job: Binding {
            session: format!("s-{}", "a".repeat(62)),
            attempt: [9; 16],
        },
        capability: Capability::from_bytes([7; 32]),
    }
    .encode()
    .unwrap();
    assert_eq!(maximal.len(), 165);
    assert!(JobContext::decode(&maximal).is_ok());
}
