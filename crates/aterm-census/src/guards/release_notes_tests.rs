// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn checkout() -> aterm_tempfile::TempDir {
    let root = aterm_tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("crates/example/src")).unwrap();
    std::fs::create_dir_all(root.path().join("apps")).unwrap();
    std::fs::create_dir_all(root.path().join("changes/fixed")).unwrap();
    std::fs::write(root.path().join("crates/example/src/lib.rs"), "// Copyright 2026 Andrew Yates\n// SPDX-License-Identifier: Apache-2.0\npub fn example() {}\n").unwrap();
    root
}
fn census(root: &Path) -> Vec<Finding> {
    crate::without_parallel_workers(|| {
        crate::run_guards(root, &Err(String::new()))
            .unwrap()
            .findings
            .into_iter()
            .filter(|finding| finding.rule == RELEASE_NOTES.id)
            .collect()
    })
}
#[test]
fn actual_census_rejects_missing_bullet_and_double_entry_then_accepts_valid_fragment() {
    let root = checkout();
    let path = root.path().join("changes/fixed/session-note.md");
    for (text, line) in [
        ("The repaired behavior.\n", 1),
        ("- First.\n- Second.\n", 2),
    ] {
        std::fs::write(&path, text).unwrap();
        let found = census(root.path());
        assert!(
            found
                .iter()
                .any(|finding| finding.path == "changes/fixed/session-note.md"
                    && finding.line == line),
            "{found:?}"
        );
    }
    std::fs::write(
        path,
        "- The repaired behavior.\n  Its continuation ships unchanged.\n",
    )
    .unwrap();
    assert!(census(root.path()).is_empty());
}
#[test]
fn actual_census_reads_case_variants_and_hidden_nested_paths_instead_of_skipping_them() {
    let root = checkout();
    std::fs::remove_dir_all(root.path().join("changes")).unwrap();
    std::fs::create_dir_all(root.path().join("Changes/Fixed/.hidden")).unwrap();
    std::fs::write(
        root.path().join("Changes/Fixed/.hidden/note.md"),
        "- A valid bullet under an invalid path.\n",
    )
    .unwrap();
    let found = census(root.path());
    assert!(
        found
            .iter()
            .any(|finding| finding.path == "Changes/Fixed/.hidden/note.md"),
        "{found:?}"
    );
}
#[test]
fn actual_census_rejects_invalid_utf8_and_oversized_entries() {
    let root = checkout();
    let path = root.path().join("changes/fixed/session-note.md");
    std::fs::write(&path, [0xff, b'\n']).unwrap();
    assert!(!census(root.path()).is_empty());
    std::fs::write(path, vec![b'x'; MAX_FRAGMENT_BYTES + 1]).unwrap();
    assert!(!census(root.path()).is_empty());
}
#[cfg(unix)]
#[test]
fn actual_census_fails_closed_on_executable_unreadable_and_symlink_entries() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let root = checkout();
    let path = root.path().join("changes/fixed/session-note.md");
    std::fs::write(&path, "- A valid bullet.\n").unwrap();
    for mode in [0o755, 0o000] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        // A privileged test process can read mode 000. Judge a real permission
        // refusal only where the host actually supplies one, not a UID guess.
        let readable = std::fs::read(&path).is_ok();
        let found = census(root.path());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        if mode == 0o755 || !readable {
            assert!(!found.is_empty(), "mode {mode:o} was silently accepted");
        } else {
            assert!(found.is_empty(), "a readable plain file was rejected");
        }
    }
    std::fs::remove_file(&path).unwrap();
    symlink(root.path().join("crates/example/src/lib.rs"), path).unwrap();
    assert!(!census(root.path()).is_empty());
}
