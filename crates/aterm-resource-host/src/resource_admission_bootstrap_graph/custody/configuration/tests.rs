// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual descriptor/clock controls, not authenticated Root or issuer fixtures.
use super::super::tests::Inputs;
use super::*;
use std::os::unix::fs::OpenOptionsExt as _;
use std::time::Duration;

fn request(inputs: &Inputs, remaining: Duration) -> (wire::ConfigurationAttachment, OwnedFd) {
    let path = inputs.original.path.join("config.toml");
    let bytes = b"[build]\njobs = 4\n";
    std::fs::write(&path, bytes).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .unwrap();
    let metadata = file.metadata().unwrap();
    let stamp = |seconds, nanoseconds: i64| wire::configuration::Timestamp {
        seconds,
        nanoseconds: nanoseconds.try_into().unwrap(),
    };
    let request = wire::ConfigurationAttachment {
        key: inputs.original.request.key.clone(),
        sequence: 1,
        relative: b"config.toml".to_vec(),
        file: trust_resource_controller::FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        bytes: bytes.len() as u64,
        sha256: aterm_digest::Sha256::digest(bytes),
        modified: stamp(metadata.mtime(), metadata.mtime_nsec()),
        changed: stamp(metadata.ctime(), metadata.ctime_nsec()),
        input_remaining_ns: remaining.as_nanos().try_into().unwrap(),
    };
    (request, file.into())
}

#[test]
fn earlier_actual_input_endpoint_prevents_late_content_reads_under_the_root_deadline() {
    let inputs = Inputs::new();
    let mut preparing = inputs.preparing();
    preparing.submitted = true;
    preparing.source_submitted = true;
    preparing.materialization_submitted = true;
    let root_deadline = preparing.deadline;
    let (request, description) = request(&inputs, Duration::from_secs(2));
    let expected = preparing.configuration_anchor + Duration::from_secs(2);
    let token = preparing.configuration_transfer(request.clone()).unwrap();
    assert_eq!(token.deadline(), expected.min(preparing.deadline));
    let configuration = OriginalConfiguration {
        request,
        source: Arc::clone(&inputs.original),
        description,
        deadline: token.deadline(),
        revoked: AtomicBool::new(false),
    };
    configuration.check_current(root_deadline).unwrap();
    std::thread::sleep(
        token.deadline().saturating_duration_since(Instant::now()) + Duration::from_millis(2),
    );
    // A checksum read would yield InvalidData here. The expired original input
    // endpoint must refuse first, despite the still-future Root endpoint.
    std::fs::write(inputs.original.path.join("config.toml"), b"changed file").unwrap();
    let error = configuration.check_current(root_deadline).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(Instant::now() < root_deadline);
    assert!(configuration.revoked.load(Ordering::Acquire));
}

#[test]
fn delayed_configuration_cannot_restart_the_original_enrollment_anchor() {
    let inputs = Inputs::new();
    let mut preparing = inputs.preparing();
    preparing.submitted = true;
    preparing.source_submitted = true;
    preparing.materialization_submitted = true;
    preparing.configuration_anchor = Instant::now() - Duration::from_secs(2);
    let (request, _description) = request(&inputs, Duration::from_secs(1));
    let result = preparing.configuration_transfer(request);
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut));
    assert_eq!(
        preparing.configuration_budget.remaining_files(),
        wire::configuration::MAX_FILES - 1
    );
}

#[test]
fn configuration_cannot_skip_preparation_or_repeat_its_original_one_use_slot() {
    let inputs = Inputs::new();
    let mut preparing = inputs.preparing();
    preparing.submitted = true;
    preparing.source_submitted = true;
    let (configuration, _description) = request(&inputs, Duration::from_secs(2));
    assert!(
        preparing
            .configuration_transfer(configuration.clone())
            .is_err()
    );
    assert_eq!(
        preparing.configuration_budget.remaining_files(),
        wire::configuration::MAX_FILES
    );
    let preparation = wire::MaterializationPreparation {
        key: inputs.original.request.key.clone(),
        input_remaining_ns: 30_000_000_000,
    };
    let token = preparing
        .materialization_transfer(preparation.clone())
        .unwrap();
    assert!(
        token.contents_directory().is_err(),
        "a request alone supplied contents identity"
    );
    assert_eq!(preparing.deadline, token.deadline());
    assert!(preparing.materialization_transfer(preparation).is_err());
    let config = preparing.configuration_transfer(configuration).unwrap();
    assert!(config.deadline() <= token.deadline());
    assert_eq!(
        preparing.configuration_budget.remaining_files(),
        wire::configuration::MAX_FILES - 1
    );
}

#[test]
fn declaration_budget_refuses_second_file_before_content_and_never_restarts_sequence() {
    let inputs = Inputs::new();
    let mut preparing = inputs.preparing();
    preparing.submitted = true;
    preparing.source_submitted = true;
    preparing.materialization_submitted = true;
    let (mut first, _description) = request(&inputs, Duration::from_secs(2));
    first.bytes = wire::configuration::MAX_BYTES - 10;
    preparing.configuration_transfer(first.clone()).unwrap();
    let mut second = first.clone();
    second.sequence = 2;
    second.bytes = 11;
    assert!(preparing.configuration_transfer(second.clone()).is_err());
    assert_eq!(preparing.configuration_budget.remaining_bytes(), 10);
    assert!(preparing.configuration_transfer(first).is_err());
    second.bytes = 10;
    preparing.configuration_transfer(second.clone()).unwrap();
    assert_eq!(preparing.configuration_budget.remaining_bytes(), 0);
    second.sequence = 3;
    second.bytes = 0;
    assert!(preparing.configuration_transfer(second).is_err());
}
