// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's fleet as LAN toolset sharing reads it (`atpkg::peers::set_fleet`, owner ruling
//! 2026-10-27: "everything must be ON BY DEFAULT in atpkg!!!"): atpkg reads no fleet book
//! itself, so `aterm` hands it this read of `aterm-net`'s.

use std::net::SocketAddr;

/// This Mac's fleet members ([`aterm_net::fleet_member::member_addrs`]): how many a server
/// answers, and the ones a download dials, each at the port a member's aterm serves its toolset
/// archives on ([`atpkg::peers::DEFAULT_PORT`]). A download dials none while macOS has not asked
/// about Local Network, since a dial of a LAN address would ask it then, nor once it was
/// refused, and says which; none an isolated debug drive's loopback guard refuses to dial.
pub(crate) fn fleet() -> atpkg::peers::Fleet {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return atpkg::peers::Fleet::default();
    };
    let isolated = cfg!(test)
        || std::env::var_os("ATERM_STATE_HOME").is_some()
        || std::env::var_os("CI").is_some();
    let paths = (!isolated)
        .then(|| aterm_secrets::client::Paths::live().ok())
        .flatten();
    let denied = paths
        .as_ref()
        .and_then(|p| aterm_secrets::client::swarm_denied_nodes(p).ok().flatten())
        .unwrap_or_default();
    let log = paths
        .as_ref()
        .and_then(|p| aterm_agent::fleet_log::LocalLog::of(p).keys());
    let members =
        aterm_net::fleet_member::member_addrs_with_denied(&state, &denied, log.as_deref());
    #[cfg(target_os = "macos")]
    {
        use aterm_cli::macos_doctor::LocalNetworkHeard;
        let held = match aterm_cli::macos_doctor::local_network_heard() {
            LocalNetworkHeard::On | LocalNetworkHeard::Asked => None,
            LocalNetworkHeard::Untold => Some(
                "macOS has not asked whether aterm may reach your local network, and a \
                 download never raises that question",
            ),
            LocalNetworkHeard::Off => Some(
                "macOS does not let aterm reach your local network; turn it on in System \
                 Settings > Privacy & Security > Local Network",
            ),
        };
        if let Some(held) = held {
            return atpkg::peers::Fleet {
                members: members.len(),
                dial: Vec::new(),
                held: Some(held.to_owned()),
            };
        }
    }
    let dial = members
        .iter()
        .map(|ip| SocketAddr::new(*ip, atpkg::peers::DEFAULT_PORT))
        .filter(|at| aterm_net::loopback::check(aterm_net::loopback::Reach::Dial(*at)).is_ok())
        .map(|at| at.to_string())
        .collect();
    atpkg::peers::Fleet {
        members: members.len(),
        dial,
        held: None,
    }
}
