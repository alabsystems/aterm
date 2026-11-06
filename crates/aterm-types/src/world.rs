// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A scratch world: the private HOME, configuration and runtime folders a check, a measurement
//! or a smoke starts an aterm in. That aterm is never the person's, so it starts none of the
//! automatic lanes that act for the login's own account or reach the person's real LAN, and none
//! that can put an OS consent prompt on the person's desk: macOS keys the firewall's
//! incoming-connection question and a background item to the account and the app, never to
//! HOME, so a world cannot answer them privately and must not ask them at all.
//!
//! A world says what it is in the two places the lanes read, because a release build of aterm
//! (no development seams, [`crate::dev_seam!`]) reads only some of them:
//! - its environment carries [`MARKER`] at [`state_home`], which the lanes that reach the local
//!   network read as a plain environment variable in every build: the converge runner's window
//!   lane, the `_alab._tcp` browse, the Commons runtime, and the node job's updater and account
//!   claims;
//! - its `aterm.toml` carries [`LANES_OFF`], each table the product's own switch for one lane.
//!
//! The fleet listener reads neither: which vault it reads follows where its state root resolved
//! ([`crate::dirs::runs_under_private_state_root`]), so a release build in a world reads its
//! HOME's vault, and `[swarm]` in [`LANES_OFF`] keeps it from listening.
//!
//! verify measure's world, the GUI smoke's sandbox and the perf arena's windowed lanes take
//! both. The lab's worlds (`aterm-lab`'s `instance::World`) carry the marker at this folder and
//! keep their own tables, which have no `[swarm]` table, and two of its drives (`commons_lan`,
//! `commons_startup`) remove the marker to drive the local-network lanes themselves.

use std::path::{Path, PathBuf};

/// The environment variable whose presence says an aterm runs in a scratch world. The lanes that
/// reach the local network read it raw (`std::env::var_os`), not through [`crate::dev_seam!`], so
/// it holds in a release bundle too. A build with development seams also takes it as its state
/// root ([`crate::dirs`]); [`state_home`] is where a build without them keeps its state under the
/// world's HOME, so both keep it in one folder.
pub const MARKER: &str = "ATERM_STATE_HOME";

/// [`MARKER`]'s value for a world whose HOME is `home`: the platform's per-user state folder
/// under it ([`crate::dirs::resolve_state_dir`] with no override and no `XDG_STATE_HOME`), or
/// `None` where the platform keeps state outside HOME (Windows) or keeps none.
#[must_use]
pub fn state_home(home: &Path) -> Option<PathBuf> {
    crate::dirs::resolve_state_dir(
        None,
        crate::dirs::StatePlatform {
            home: Some(home.to_path_buf()),
            ..crate::dirs::StatePlatform::default()
        },
    )
}

/// The `aterm.toml` tables a world turns its automatic lanes off with, each the product's own
/// switch for one lane:
/// - `[update]`: no update check, download or apply;
/// - `[packages]`: no package loop;
/// - `[machine] universal_control = "leave"`: the Mac's own preferences left alone;
/// - `[keeper]`: no keeper job in the login's launchd domain, which a lasting bundle's window
///   would share with the person's own windows of it and leave running after the world ends;
/// - `[swarm]`: no part in the swarm: no `_alab._tcp` browse, no fleet reads and no vault
///   sharing, each a use of the person's real LAN.
///
/// Tables only, no root keys, so a world may put its own root keys and tables before it.
pub const LANES_OFF: &str = "[update]\nenabled = false\nauto_apply = false\n\
                             [packages]\nenabled = false\n\
                             [machine]\nuniversal_control = \"leave\"\n\
                             [keeper]\nenabled = false\n\
                             [swarm]\nenabled = false\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_marker_names_the_platforms_state_folder_under_the_worlds_home() {
        let home = Path::new("/w/home");
        #[cfg(target_os = "macos")]
        let want = "/w/home/Library/Application Support/aterm";
        #[cfg(not(target_os = "macos"))]
        let want = "/w/home/.local/state/aterm";
        assert_eq!(state_home(home), Some(PathBuf::from(want)));
    }

    #[test]
    fn the_lanes_off_open_tables_only() {
        let first = LANES_OFF.lines().next().unwrap_or_default();
        assert!(first.starts_with('['), "{LANES_OFF}");
    }
}
