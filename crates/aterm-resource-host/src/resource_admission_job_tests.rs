// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn memory_sampling_error_names_host_fault_not_a_limit() {
    let (cause, detail) =
        memory_fault(Err(io::Error::from_raw_os_error(libc::EACCES)), 1024, None).unwrap();
    assert_eq!(cause, CompletionCause::HostFault);
    assert!(detail.contains("PermissionDenied"));
}

#[test]
fn measured_memory_limit_and_critical_pressure_keep_their_evidence() {
    let (cause, detail) = memory_fault(Ok(1025), 1024, None).unwrap();
    assert_eq!(cause, CompletionCause::ResourceLimit);
    assert!(detail.contains("1025") && detail.contains("1024"));
    let (cause, detail) =
        memory_fault(Ok(512), 1024, Some(aterm_sysprobe::MemoryLevel::Critical)).unwrap();
    assert_eq!(cause, CompletionCause::ResourceLimit);
    assert!(detail.contains("critical") && detail.contains("512"));
    assert!(memory_fault(Ok(1024), 1024, None).is_none());
}

#[test]
fn completed_stamp_decides_deadline_independently_of_actor_wake() {
    let start = Instant::now();
    let limit = Duration::from_secs(3);
    for (elapsed, expected) in [
        (Duration::from_secs(2), CompletionCause::Exited),
        (limit, CompletionCause::Timeout),
        (Duration::from_secs(4), CompletionCause::Timeout),
    ] {
        assert_eq!(ended_cause(start, start + elapsed, Some(limit)), expected);
        assert_eq!(
            ended_cause(start, start + elapsed, None),
            CompletionCause::Exited
        );
    }
}
