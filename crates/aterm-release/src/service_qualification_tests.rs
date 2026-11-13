// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn a_release_requires_one_qualification_asset() {
    let mut names = crate::channel::required_asset_names("0.149.0", true, false);
    assert!(validate_assets(&names, "0.149.0", true, false, &[]).is_err());
    names.push(REPORT_ASSET.into());
    validate_assets(&names, "0.149.0", true, false, &[]).unwrap();
    names.push(REPORT_ASSET.into());
    assert!(validate_assets(&names, "0.149.0", true, false, &[]).is_err());
}
#[cfg(target_os = "macos")]
#[test]
fn absent_release_gate_refuses_before_starting_an_image() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let result = produce(
        dir.path(),
        &"a".repeat(40),
        &"b".repeat(64),
        &dir.path().join("absent.app"),
        &dir.path().join("absent.dmg"),
        149,
        &"c".repeat(40),
        &dir.path().join("aterm-appcast.toml"),
    );
    assert!(result.is_err());
    assert!(!dir.path().join(REPORT_ASSET).exists());
}
#[test]
fn protocol_is_explicit_and_bounded() {
    assert!(parse_protocol(br#"{"schema":1,"min":1,"max":2}"#).is_ok());
    for wrong in [
        br#"{}"#.as_slice(),
        br#"{"schema":1,"min":0,"max":2}"#,
        br#"{"schema":1,"min":3,"max":2}"#,
        br#"{"schema":2,"min":1,"max":2}"#,
    ] {
        assert!(parse_protocol(wrong).is_err());
    }
}
#[test]
fn report_and_complete_image_are_bound_to_the_existing_appcast() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let image = dir.path().join("aterm.app");
    std::fs::create_dir_all(image.join("Contents/MacOS")).unwrap();
    std::fs::write(image.join("Contents/MacOS/aterm"), b"signed image fixture").unwrap();
    let manifest = dir.path().join("aterm-appcast.toml");
    let revision = "a".repeat(40);
    std::fs::write(&manifest, format!("schema=1\nversion='0.149.0'\nbuild_number=149\ncommit='{revision}'\ndmg='aterm.dmg'\nsha256='{}'\n", "b".repeat(64))).unwrap();
    let report = emit(
        &image,
        149,
        &revision,
        &revision,
        &"c".repeat(64),
        "release gates passed",
        ProtocolRange { min: 1, max: 1 },
        &manifest,
    )
    .unwrap();
    let extension: Extension =
        aterm_toml::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    let bytes = std::fs::read(&report).unwrap();
    assert_eq!(extension.service.qualification_sha256, sha(&bytes));
    assert_eq!(
        extension.service.image_sha256,
        image_digest(&image).unwrap()
    );
    let decoded: Qualification = aterm_json::from_slice(&bytes).unwrap();
    assert!(!decoded.live_service_health_tested);
    verify(&image, &manifest).unwrap();
    let original_manifest = std::fs::read_to_string(&manifest).unwrap();
    for bad_schema in [false, true] {
        let mut invalid: Qualification = aterm_json::from_slice(&bytes).unwrap();
        if bad_schema {
            invalid.schema = 2;
        } else {
            invalid.status = "failed".into();
        }
        let invalid_bytes = aterm_json::to_vec(&invalid).unwrap();
        std::fs::write(&report, &invalid_bytes).unwrap();
        std::fs::write(
            &manifest,
            original_manifest.replace(&sha(&bytes), &sha(&invalid_bytes)),
        )
        .unwrap();
        assert!(
            verify(&image, &manifest).is_err(),
            "even a hash-bound report must be supported and passed"
        );
    }
    std::fs::write(&report, &bytes).unwrap();
    std::fs::write(&manifest, original_manifest).unwrap();
    std::fs::write(image.join("Contents/resource"), b"changed").unwrap();
    assert_ne!(
        extension.service.image_sha256,
        image_digest(&image).unwrap()
    );
    assert!(verify(&image, &manifest).is_err());
    verify_published_report(&manifest).unwrap();
    assert!(
        emit(
            &image,
            150,
            &revision,
            &revision,
            &"c".repeat(64),
            "passed",
            ProtocolRange { min: 1, max: 1 },
            &manifest
        )
        .is_err()
    );
    assert_eq!(
        report_digest(&std::fs::read(&manifest).unwrap()).unwrap(),
        sha(&bytes)
    );
    assert!(report_digest(&vec![b' '; 1024 * 1024 + 1]).is_err());
    let bad_exec = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("Contents/MacOS/aterm", "Contents/MacOS/other");
    assert!(report_digest(bad_exec.as_bytes()).is_err());
    let uncommitted = dir.path().join("uncommitted-appcast.toml");
    std::fs::write(
        &uncommitted,
        format!(
            "schema=1\nversion='0.149.0'\nbuild_number=149\ncommit='{revision}'\ndmg='aterm.dmg'\nsha256='{}'\n",
            "b".repeat(64)
        ),
    )
    .unwrap();
    assert!(
        emit(
            &image,
            149,
            &revision,
            "not-a-40-hex-commit",
            &"c".repeat(64),
            "passed",
            ProtocolRange { min: 1, max: 1 },
            &uncommitted,
        )
        .is_err()
    );
}
