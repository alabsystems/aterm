// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// NEGATIVE CONTROL: `Qualification::validate` rejects blank platforms and non-relative
/// executable paths even when every hash and protocol field matches.
#[test]
fn qualification_validate_rejects_blank_platform_and_non_relative_executable() {
    let valid_service = ServiceDeclaration {
        schema: 1,
        platform: "aarch64-apple-darwin".into(),
        image_sha256: "a".repeat(64),
        executable: PathBuf::from("Contents/MacOS/aterm"),
        executable_sha256: "b".repeat(64),
        qualification_sha256: "c".repeat(64),
        protocol: ProtocolRange { min: 1, max: 2 },
    };
    let report = Qualification {
        schema: 1,
        status: "passed".into(),
        scope: "release-gate-and-signed-image-protocol".into(),
        build: 42,
        source_revision: "1".repeat(40),
        verified_source_revision: "2".repeat(40),
        verified_input_sha256: "3".repeat(64),
        release_check_evidence: "verified".into(),
        image_sha256: "a".repeat(64),
        executable_sha256: "b".repeat(64),
        protocol: ProtocolRange { min: 1, max: 2 },
        protocol_probe_passed: true,
        native_signature_verified: true,
        live_service_health_tested: false,
    };
    let rev = "1".repeat(40);
    assert!(report.validate(42, &rev, &valid_service).is_ok());

    for bad_platform in ["", "   ", "\t\n"] {
        let mut s = valid_service.clone();
        s.platform = bad_platform.into();
        assert!(
            report.validate(42, &rev, &s).is_err(),
            "platform {bad_platform:?}"
        );
    }
    for bad_exe in [
        "",
        ".",
        "..",
        "/bin/aterm",
        "Contents/../MacOS/aterm",
        "Contents/./MacOS/aterm",
    ] {
        let mut s = valid_service.clone();
        s.executable = PathBuf::from(bad_exe);
        assert!(
            report.validate(42, &rev, &s).is_err(),
            "executable {bad_exe:?}"
        );
    }
}

/// NEGATIVE CONTROL: multi-hop symlink chains that bounce above the image root via an
/// intermediate in-image symlink (`sub/up -> ..` then `bounce -> sub/up/../reenter/aterm`)
/// or put a trailing slash on a regular file are refused by `image_digest`.
#[cfg(unix)]
#[test]
fn image_digest_refuses_multi_hop_root_escape_and_trailing_slash_on_file() {
    use std::os::unix::fs::symlink;

    let tmp = crate::test_scratch_dir("svc-img");
    let reenter = tmp.path().join("reenter");
    std::fs::create_dir_all(reenter.join("sub")).unwrap();
    std::fs::write(reenter.join("aterm"), b"exe").unwrap();
    symlink("..", reenter.join("sub/up")).unwrap();
    assert!(
        image_digest(&reenter).is_ok(),
        "an in-image parent hop that stays at root is valid"
    );

    symlink("sub/up/../reenter/aterm", reenter.join("bounce")).unwrap();
    let err = image_digest(&reenter).expect_err("two-hop escape above root must fail");
    assert!(err.contains("escapes image"), "{err}");
    std::fs::remove_file(reenter.join("bounce")).unwrap();

    symlink("aterm/", reenter.join("file_slash")).unwrap();
    assert!(
        image_digest(&reenter).is_err(),
        "trailing slash on regular file must fail"
    );
}
