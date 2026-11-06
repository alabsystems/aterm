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

/// An answer that stays in the output buffer because the flush fails exits as unavailable,
/// never `0`. NEGATIVE CONTROL: the same answer through a writer that takes it exits `0`.
#[test]
fn an_answer_the_parent_never_got_is_not_a_success() {
    struct Unflushable(Vec<u8>);
    impl std::io::Write for Unflushable {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }
    let synthetic = b"synthetic answer";
    assert_eq!(
        emit(&mut Unflushable(Vec::new()), synthetic),
        code(ReadError::Unavailable)
    );
    let mut taken = Vec::new();
    assert_eq!(emit(&mut taken, synthetic), 0);
    assert_eq!(taken, synthetic);
}
