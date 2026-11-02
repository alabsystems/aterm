// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn service_is_scoped_to_the_exact_vendor_path_text() {
    let path = Path::new("/private/state/logins/person+1");
    assert_eq!(
        service(stage_text(path).unwrap()),
        "Claude Code-credentials-362343d7"
    );
    assert_ne!(
        service(stage_text(path).unwrap()),
        service(stage_text(Path::new("/private/state/../state/logins/person+1")).unwrap())
    );
    for invalid in [
        "",
        "/",
        "logins/person+1",
        "/private/state/person+1",
        "/private/state/logins/person",
        "/private/state/logins/+1",
        "/private/state/logins/person+0",
        "/private/state/logins/person+10000",
        "/private/state/logins/person+01",
        "/private/state/logins/person+other+1",
    ] {
        assert_eq!(
            stage_text(Path::new(invalid)),
            Err(ReadError::Refused),
            "{invalid}"
        );
    }
    let long = format!("/private/state/logins/{}+1", "x".repeat(65));
    assert_eq!(stage_text(Path::new(&long)), Err(ReadError::Refused));
}

#[test]
fn expired_deadline_never_starts_a_helper() {
    let answer = read_claude_stage(Path::new("/private/state/logins/person+1"), Instant::now());
    assert!(matches!(answer, Err(ReadError::TimedOut)));
}
