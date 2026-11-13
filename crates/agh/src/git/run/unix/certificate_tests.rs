// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

#[test]
fn certificate_default_keeps_only_one_absolute_path() {
    let output = |code, bytes: &[u8]| ChildOut {
        code: Some(code),
        stdout: bytes.to_vec(),
        stderr: Vec::new(),
    };
    assert_eq!(certificate_default(&output(1, b"")).unwrap(), None);
    assert_eq!(
        certificate_default(&output(0, b"/moved store/share/cacert.pem\0")).unwrap(),
        Some(PathBuf::from("/moved store/share/cacert.pem")),
    );
    for (code, raw) in [
        (128, b"".as_slice()),
        (0, b""),
        (0, b"relative.pem\0"),
        (0, b"/first\0/second\0"),
        (0, b"/line\nbreak\0"),
        (1, b"/unexpected\0"),
        (0, b"/unterminated"),
    ] {
        assert!(
            certificate_default(&output(code, raw)).is_err(),
            "{code}: {raw:?}"
        );
    }
    assert!(certificate_default(&output(0, &vec![b'x'; 8193])).is_err());
}
