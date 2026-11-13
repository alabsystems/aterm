// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn actual_artifact_selection_rejects_missing_ambiguous_and_foreign_paths() {
    let target = Path::new("/original/target");
    let valid = br#"{"reason":"compiler-artifact","target":{"name":"unchanged_probe"},"filenames":["/original/target/libprobe.rlib"]}
{"reason":"compiler-artifact","target":{"name":"claude-alab"},"executable":"/original/target/claude-alab"}
"#;
    assert_eq!(
        dependency_artifact(valid, target).unwrap(),
        target.join("libprobe.rlib")
    );
    assert_eq!(
        executable_artifact(valid, target).unwrap(),
        target.join("claude-alab")
    );
    assert!(dependency_artifact(b"", target).is_err());
    assert!(executable_artifact(b"", target).is_err());
    let foreign = String::from_utf8(valid.to_vec())
        .unwrap()
        .replace("/original/target/", "/original/target-foreign/");
    assert!(dependency_artifact(foreign.as_bytes(), target).is_err());
    assert!(executable_artifact(foreign.as_bytes(), target).is_err());
    let ambiguous = format!(
        "{}{}",
        String::from_utf8_lossy(valid),
        String::from_utf8_lossy(valid)
            .replace("libprobe.rlib", "other.rlib")
            .replace("target/claude-alab", "target/other")
    );
    assert!(dependency_artifact(ambiguous.as_bytes(), target).is_err());
    assert!(executable_artifact(ambiguous.as_bytes(), target).is_err());
}
