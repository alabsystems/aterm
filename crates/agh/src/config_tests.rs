// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::path::{Path, PathBuf};

use super::{ConfigText, Dirs, read_config_in};

/// A relative root (a relative `XDG_CONFIG_HOME`) names no credential store: agh runs with
/// no config rather than read one under the current directory.
#[test]
fn a_relative_root_is_no_dirs() {
    let abs = |p: &str| Some(PathBuf::from(p));
    let dirs = Dirs::of(abs("/c/aterm"), abs("/s"), abs("/h")).expect("absolute roots");
    assert_eq!(dirs.config, Path::new("/c/aterm/agh"));
    assert_eq!(dirs.state, Path::new("/s/agh"));
    assert_eq!(dirs.home, Path::new("/h"));
    assert_eq!(Dirs::of(abs("rel/aterm"), abs("/s"), abs("/h")), None);
    assert_eq!(Dirs::of(abs("/c/aterm"), abs("rel"), abs("/h")), None);
    assert_eq!(Dirs::of(abs("/c/aterm"), abs("/s"), abs("rel")), None);
    assert_eq!(Dirs::of(None, abs("/s"), abs("/h")), None);
}

/// A system directory that is root's (`/private/etc` on macOS, where `/etc` is root's link
/// to it; `/etc` elsewhere), read as the agh directory itself: the remedy names no
/// `--config`, which no routed verb takes and which changes no file that routes.
#[test]
fn another_users_agh_directory_names_no_config_flag() {
    if aterm_uds::peer::our_uid() == 0 {
        return;
    }
    let home = Path::new("/nonexistent-home");
    for dir in [Path::new("/private/etc"), Path::new("/etc")] {
        if std::fs::symlink_metadata(dir).is_err() {
            continue;
        }
        match read_config_in(dir, "hosts", home, Some(dir)) {
            ConfigText::Refused { error, remedy } => {
                let remedy = remedy.unwrap_or_default();
                assert!(!remedy.is_empty(), "{}: {error}", dir.display());
                assert!(!remedy.contains("--config"), "{}: {remedy}", dir.display());
            }
            ConfigText::Missing | ConfigText::Io(_) | ConfigText::NotUtf8 | ConfigText::Text(_) => {
                panic!(
                    "{}: read_private took a root-owned directory",
                    dir.display()
                );
            }
        }
    }
}
