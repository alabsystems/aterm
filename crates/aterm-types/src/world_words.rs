// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A scratch world's words ([`crate::world`]) in a file of their own, `std` only, so code that
//! links no crate mounts them by `#[path]` and takes the same world: the `aterm-order-ld` wrapper
//! a macOS release link runs (`crates/aterm/build.rs`) starts each image its layout probe measures
//! in it (`probe_world` in `crates/aterm/src/macho_order.rs`), and `aterm-uds`, below this crate,
//! mounts them as its `world`, so its LAN client sends no ask of the root broker from a world.

use std::path::{Path, PathBuf};

/// The environment variable whose presence says an aterm runs in a scratch world. The lanes that
/// reach the local network read it raw (`std::env::var_os`), not through [`crate::dev_seam!`], so
/// it holds in a release bundle too. A build with development seams also takes it as its state
/// root ([`crate::dirs`]); [`state_home`] is where a build without them keeps its state under the
/// world's HOME, so both keep it in one folder.
pub const MARKER: &str = "ATERM_STATE_HOME";

/// [`MARKER`]'s value for a world whose HOME is `home`: the platform's per-user state folder under
/// it, where [`crate::dirs::resolve_state_dir`] keeps aterm's state when nothing overrides it and,
/// off macOS, no absolute `XDG_STATE_HOME` names another root; `None` where the platform keeps
/// state outside HOME (Windows) or keeps none.
#[must_use]
pub fn state_home(home: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(home.join("Library/Application Support/aterm"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Some(home.join(".local/state/aterm"))
    }
    #[cfg(not(unix))]
    {
        let _ = home;
        None
    }
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
