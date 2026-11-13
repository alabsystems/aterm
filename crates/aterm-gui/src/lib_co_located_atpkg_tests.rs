// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn a_window_open_when_its_copy_updated_still_finds_the_package_tool() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(bin.join("aterm"), b"the new executable").unwrap();
    let tool = format!("atpkg{}", std::env::consts::EXE_SUFFIX);
    #[cfg(unix)]
    std::os::unix::fs::symlink("aterm", bin.join(&tool)).unwrap();
    #[cfg(not(unix))]
    std::fs::write(bin.join(&tool), b"atpkg").unwrap();
    let expected = std::fs::canonicalize(&bin).unwrap().join(&tool);
    assert_eq!(
        super::co_located_atpkg_for(&bin.join("aterm")),
        Some(expected.clone())
    );
    assert_eq!(
        super::co_located_atpkg_for(&bin.join("aterm (deleted)")),
        Some(expected),
        "the running executable's name after an update replaced it"
    );
    assert_eq!(super::co_located_atpkg_for(&bin.join("aterm (gone)")), None);
    std::fs::remove_file(bin.join(&tool)).unwrap();
    assert_eq!(
        super::co_located_atpkg_for(&bin.join("aterm (deleted)")),
        None
    );
}
