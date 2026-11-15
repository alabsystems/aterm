use super::*;
use crate::bootstrap_graph::archive::tests::request;

#[test]
fn awaiting_and_run_are_distinct_exact_request_context_records() {
    let request = request();
    let digest = [8; 32];
    let awaiting = awaiting(&request, digest).unwrap();
    let run = run(&request, digest, 123).unwrap();
    validate_awaiting(&request, digest, &awaiting).unwrap();
    assert_eq!(decode_run(&request, digest, &run).unwrap(), 123);
    assert!(validate_awaiting(&request, digest, &run).is_err());
    assert!(decode_run(&request, digest, &awaiting).is_err());
    for case in 0..5 {
        let mut other = request.clone();
        match case {
            0 => other.host.push('x'),
            1 => other.boot.push('x'),
            2 => other.key.sequence += 1,
            3 => other.key.operation[0] ^= 1,
            4 => other.archives[0].sha256[0] ^= 1,
            _ => unreachable!(),
        }
        assert!(validate_awaiting(&other, digest, &awaiting).is_err());
        assert!(decode_run(&other, digest, &run).is_err());
    }
    assert!(validate_awaiting(&request, [9; 32], &awaiting).is_err());
    assert!(decode_run(&request, [9; 32], &run).is_err());
}

#[test]
fn work_duration_never_exceeds_original_request_and_no_partial_record_passes() {
    let request = request();
    for duration in [0, request.timeout_ns + 1, u64::MAX] {
        assert!(run(&request, [8; 32], duration).is_err());
    }
    let record = run(&request, [8; 32], request.timeout_ns).unwrap();
    for cut in 0..record.len() {
        assert!(decode_run(&request, [8; 32], &record[..cut]).is_err());
    }
    let mut extra = record;
    extra.push(0);
    assert!(decode_run(&request, [8; 32], &extra).is_err());
}
