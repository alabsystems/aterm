use super::*;

#[test]
fn workload_support_marker_declares_only_its_distinct_closed_protocol() {
    assert_eq!(&WORKLOAD_PROTOCOL_MARKER[..16], b"TRUSTWORKLOAD001");
    let fields = WORKLOAD_PROTOCOL_MARKER[16..]
        .chunks_exact(8)
        .map(|value| u64::from_be_bytes(value.try_into().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(fields, [3, 6, 7, 40, 48, 1]);
    assert_ne!(WORKLOAD_PROTOCOL_MARKER, crate::resolver::RESOLVER_PROTOCOL_MARKER);
}
fn invocation() -> WorkloadInvocation {
    let file = |path: &[u8]| InputFile { path: path.into(), sha256: [7; 32] };
    WorkloadInvocation {
        invocation: [8; 16],
        observed_host_store: None,
        root: b"/sdk".into(),
        program: file(b"/sdk/bin/targo"),
        compiler: file(b"/sdk/bin/trustc"),
        cwd: b"/sources/vc".into(),
        cargo_home: b"/cache/cargo".into(),
        output_root: b"/outputs/run".into(),
        refutation_step: file(b"/sources/ay/proofs/step.rs"),
        refutation_loop: file(b"/sources/ay/proofs/loop.rs"),
        trust_spec_source: file(b"/sources/spec.rs"),
        repetitions: 3,
        timeout_ns: 3_600_000_000_000,
    }
}
#[test]
fn exact_invocation_roundtrip_is_data_and_old_roles_remain_closed() {
    let value = invocation();
    let frame = value.encode().unwrap();
    assert_eq!(WorkloadInvocation::decode(&frame).unwrap(), value);
    assert!(crate::Request::decode(&frame).is_err());
    assert!(crate::resolver::ResolverInvocation::decode(&frame).is_err());
    for length in 0..frame.len() {
        assert!(WorkloadInvocation::decode(&frame[..length]).is_err());
    }
    let mut extra = frame;
    extra.push(0);
    assert!(WorkloadInvocation::decode(&extra).is_err());
}
#[test]
fn normalized_bound_paths_and_exact_sdk_siblings_are_required() {
    for mode in 0..9 {
        let mut value = invocation();
        match mode {
            0 => value.root = b"/sdk/../other".into(),
            1 => value.cwd = b"relative".into(),
            2 => value.cargo_home = b"/cache//cargo".into(),
            3 => value.output_root = b"/sdk/target".into(),
            4 => value.output_root = b"/sources/vc/target".into(),
            5 => value.output_root = b"/cache/cargo/target".into(),
            6 => value.program.path = b"/different/bin/targo".into(),
            7 => value.compiler.path = b"/different/bin/trustc".into(),
            8 => value.refutation_loop.path = value.refutation_step.path.clone(),
            _ => unreachable!(),
        }
        assert!(value.encode().is_err(), "mode {mode}");
    }
}
#[test]
fn finite_population_and_invocation_identity_are_mandatory() {
    for mode in 0..4 {
        let mut value = invocation();
        match mode {
            0 => value.invocation = [0; 16],
            1 => value.repetitions = 0,
            2 => value.repetitions = WORKLOAD_MAX_REPETITIONS + 1,
            3 => value.timeout_ns = 0,
            _ => unreachable!(),
        }
        assert!(value.encode().is_err(), "mode {mode}");
    }
}
#[test]
fn continuation_cannot_cross_original_root_checkpoint_or_invocation() {
    let value = Continuation {
        invocation_sha256: [1; 32],
        root_attempt: [2; 16],
        checkpoint: 1,
        checkpoint_sha256: [3; 32],
        decision: Decision::Continue,
    };
    assert_eq!(Continuation::decode(&value.encode().unwrap()).unwrap(), value);
    value.check_binding([1; 32], [2; 16], 1, [3; 32]).unwrap();
    for mode in 0..4 {
        let mut altered = value.clone();
        match mode {
            0 => altered.invocation_sha256[0] ^= 1,
            1 => altered.root_attempt[0] ^= 1,
            2 => altered.checkpoint += 1,
            3 => altered.checkpoint_sha256[0] ^= 1,
            _ => unreachable!(),
        }
        assert!(altered.check_binding([1; 32], [2; 16], 1, [3; 32]).is_err());
    }
    let mut invalid = value.clone();
    invalid.checkpoint = 0;
    assert!(invalid.encode().is_err());
    invalid.checkpoint = WORKLOAD_MAX_CHECKPOINTS + 1;
    assert!(invalid.encode().is_err());
    invalid.checkpoint = 1;
    invalid.root_attempt = [0; 16];
    assert!(invalid.encode().is_err());
    let mut stop = value;
    stop.decision = Decision::Stop;
    assert_eq!(Continuation::decode(&stop.encode().unwrap()).unwrap(), stop);
}
