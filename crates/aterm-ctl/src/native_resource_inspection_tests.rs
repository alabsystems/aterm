// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn missing_registration_and_replacement_fail_closed() {
    assert!(lookup("taxxxxxxxxx").is_err());
    fn installed(session: &str) -> io::Result<aterm_json::Value> {
        Ok(aterm_json::json!({"session":session}))
    }
    register(installed).unwrap();
    assert_eq!(
        lookup("taxxxxxxxxx").unwrap()["session"].as_str(),
        Some("taxxxxxxxxx")
    );
    assert_eq!(
        register(installed).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
}
