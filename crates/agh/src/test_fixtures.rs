// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The fixtures agh's in-crate policy and conformance tests share: the canonical agh.toml's
//! text over a scratch root, a private file written once, and that text written as a scratch
//! `Dirs`' agh.toml.

use std::path::Path;

#[cfg(unix)]
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

/// Write `text` as `dirs`' agh.toml (`Dirs::config_file`), making the config directory
/// private and its parent first.
#[cfg(unix)]
pub(crate) fn write_config(dirs: &Dirs, text: &str) {
    let parent = dirs.config.parent().expect("the config's parent");
    std::fs::create_dir_all(parent).expect("the config's parent");
    aterm_uds::private_file::create_private_dir(&dirs.config).expect("the config directory");
    setup_file(&dirs.config_file(), text.as_bytes());
}
