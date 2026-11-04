// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-acaffeine` — the menu-bar keep-awake mug, `aterm acaffeine`.

#![warn(missing_docs)]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

pub mod cli;
pub mod code;
pub mod held;
pub mod icon;
pub mod instance;
pub mod launchd;
pub mod legacy;
#[cfg(target_os = "macos")]
mod macos;
pub mod menu;
pub mod model;
pub mod notify;
pub mod paths;
pub mod state;
pub mod trouble;

/// Creates an isolated `.noindex` temporary directory for `aterm-acaffeine` tests so test churn
/// stays out of Spotlight and `fseventsd`.
#[cfg(test)]
pub(crate) fn test_scratch_dir(tag: &str) -> aterm_tempfile::TempDir {
    let clean: String = tag
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let short = &clean[..clean.floor_char_boundary(12)];
    aterm_tempfile::Builder::new()
        .prefix(&format!("acaf-{short}-"))
        .suffix(".noindex")
        .tempdir()
        .expect("create aterm-acaffeine test scratch dir")
}

#[cfg(test)]
mod tests {
    use super::test_scratch_dir;

    /// NEGATIVE CONTROL: non-ASCII tags over the byte budget must truncate on a valid UTF-8 char
    /// boundary without panicking and still produce a `.noindex` scratch directory.
    #[test]
    fn test_scratch_dir_truncates_utf8_tags_and_preserves_noindex_suffix() {
        let dir = test_scratch_dir("éééééééééé-handover");
        let name = dir
            .path()
            .file_name()
            .and_then(|os| os.to_str())
            .expect("UTF-8 scratch directory name");
        assert!(
            name.starts_with("acaf-éééééé-"),
            "unexpected prefix: {name}"
        );
        assert!(
            name.ends_with(".noindex"),
            "missing .noindex suffix: {name}"
        );
    }
}
