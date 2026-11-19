// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The fixtures agh's in-crate policy and conformance tests share: the canonical agh.toml's
//! text over a scratch root, a private file written once, and that text written as a scratch
//! `Dirs`' agh.toml.

use std::path::Path;

#[cfg(any(unix, windows))]
use crate::config::Dirs;

/// `path` as the inside of a TOML basic string: its backslashes and quotes escaped.
pub(crate) fn toml_path(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// The canonical config (`tests/fixtures/canonical.toml`) with `{root}` filled, escaped by
/// [`toml_path`].
pub(crate) fn canonical_text(root: &Path) -> String {
    include_str!("../tests/fixtures/canonical.toml").replace("{root}", &toml_path(root))
}

/// Write `bytes` to a new file at `path`, mode 0600, or panic naming it.
#[cfg(unix)]
pub(crate) fn setup_file(path: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(bytes))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

/// Write `bytes` to a new private file at `path`, or panic naming it: on windows the token
/// user's under a protected access list of the user and SYSTEM, made so at its creation
/// (`write_private_new`), in a folder kept so.
#[cfg(windows)]
pub(crate) fn setup_file(path: &Path, bytes: &[u8]) {
    let dir = path.parent().expect("a folder");
    let name = path.file_name().and_then(|n| n.to_str()).expect("a name");
    let made = aterm_uds::private_file::write_private_new(dir, name, bytes, |_| Ok(()))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(made, "{}: there is one already", path.display());
}

/// Creates a `.noindex` scratch directory for tests with a short UTF-8-safe prefix.
pub(crate) fn scratch_dir(tag: &str) -> aterm_tempfile::TempDir {
    let end = tag.floor_char_boundary(24);
    let prefix = format!("agh-{}-", &tag[..end]);
    aterm_tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".noindex")
        .tempdir()
        .expect("a scratch directory")
}

/// Write `text` as `dirs`' agh.toml (`Dirs::config_file`), making the config directory
/// private and its parent first.
#[cfg(any(unix, windows))]
pub(crate) fn write_config(dirs: &Dirs, text: &str) {
    let parent = dirs.config.parent().expect("the config's parent");
    std::fs::create_dir_all(parent).expect("the config's parent");
    aterm_uds::private_file::create_private_dir(&dirs.config).expect("the config directory");
    setup_file(&dirs.config_file(), text.as_bytes());
}

#[cfg(test)]
mod tests {
    /// NEGATIVE CONTROL: multi-byte UTF-8 tags straddling byte 24 must truncate at a valid char
    /// boundary without panicking and preserve the `.noindex` Spotlight-exclusion suffix.
    #[test]
    fn scratch_dir_truncates_utf8_tags_and_preserves_noindex_suffix() {
        let dir = super::scratch_dir("aaaaaaaaaaaaaaaaaaaaaaaé-extra-tail");
        let name = dir
            .path()
            .file_name()
            .and_then(|os| os.to_str())
            .expect("scratch dir name is UTF-8");
        assert!(name.starts_with("agh-aaaaaaaaaaaaaaaaaaaaaaa-"));
        assert!(name.ends_with(".noindex"));
    }
}
