// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn unavailable_capability_and_absent_capability_have_different_answers() {
    let absent = observed(Ok(false), |available| json!(available));
    let busy = observed::<bool>(Err(Error::new("state is being changed")), |available| {
        json!(available)
    });
    assert_eq!(absent["value"].as_bool(), Some(false));
    assert_eq!(busy["state"].as_str(), Some("unknown"));
    assert!(busy.get("value").is_none());
}

#[test]
fn a_pid_is_reported_without_claiming_running_image_verification() {
    let report = service_observation(Ok(service::State::Installed {
        pid: Some(42),
        unit: "/nonexistent/aterm-status-unit".into(),
    }));
    assert_eq!(report["value"]["pid"].as_u64(), Some(42));
    assert_eq!(
        report["value"]["running_image_verified"].as_bool(),
        Some(false)
    );
    assert!(report["value"]["configured_program"].is_null());
}
