// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's fleet as LAN toolset sharing reads it (`atpkg::peers::set_fleet`;
//! docs/OWNER_RULINGS.md, 2026-10-27): atpkg reads no fleet book
//! itself, so `aterm` hands it this read of `aterm-net`'s.

use std::net::{IpAddr, SocketAddr};

/// This Mac's fleet members ([`aterm_net::fleet_member::member_addrs`]): how many a server
/// answers, and the ones a download dials, each at the port a member's aterm serves its toolset
/// archives on ([`atpkg::peers::DEFAULT_PORT`]). A download dials none while `[swarm] enabled`
/// does not read on (sharing off, or a file that cannot be read,
/// `docs/DESIGN-alab-swarm-2026-10-27.md` §9), none while macOS has not asked about Local
/// Network, since a dial of a LAN address would ask it then, nor once it was refused, and says
/// which; none the fleet's guard refuses to dial ([`aterm_net::loopback::check`]: off an
/// isolated debug drive's loopback, and every member from a private aterm, since the port is the
/// person's); none while this Mac's deny list cannot be read. The deny list and the membership
/// log are those of the vault this process's fleet readers read ([`aterm_gui::vault_here`]), so
/// a download and the window's archive server beside it read one.
pub(crate) fn fleet() -> atpkg::peers::Fleet {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return atpkg::peers::Fleet::default();
    };
    // A run on the account's own state that cannot find its vault reads no deny list: it dials
    // no member, as with no state directory.
    let Ok(paths) = aterm_gui::vault_here() else {
        return atpkg::peers::Fleet::default();
    };
    let log = paths
        .as_ref()
        .and_then(|p| aterm_agent::fleet_log::LocalLog::of(p).keys());
    let members = members_of(
        paths.as_ref(),
        aterm_secrets::client::swarm_denied_nodes,
        |denied| aterm_net::fleet_member::member_addrs_with_denied(&state, denied, log.as_deref()),
    );
    #[cfg(unix)]
    let sharing = aterm_secrets::swarm::enabled_live().map_err(|why| why.to_string());
    #[cfg(not(unix))]
    let sharing = Ok(true);
    // Away from the ALab LAN a download races only the members this network reaches
    // (`aterm_netplace::routes`): one behind the ALab LAN only spends the race's connect.
    #[cfg(unix)]
    let routes = aterm_netplace::routes::Routes::now(&state);
    #[cfg(unix)]
    let reaches = |ip: IpAddr| routes.dials(None, Some(ip));
    #[cfg(not(unix))]
    let reaches = |_: IpAddr| true;
    fleet_of(sharing, &members, reaches, may_dial)
}

/// Whether the fleet's guard lets this process dial `at` ([`aterm_net::loopback::check`]).
fn may_dial(at: SocketAddr) -> bool {
    aterm_net::loopback::check(aterm_net::loopback::Reach::Dial(at)).is_ok()
}

/// The fleet's addresses (`addrs` under a deny list) as this Mac's deny list in `vault` lets them
/// in (`read`): a process that reads no vault (`None`: under a private state root,
/// [`aterm_gui::vault_here`]) has a list that names no one; a vault whose deny list cannot be
/// read now (`Err`, or `Ok(None)`: a sync applying, a failed apply) lets no member's address in
/// until it can.
fn members_of(
    vault: Option<&aterm_secrets::client::Paths>,
    read: impl FnOnce(&aterm_secrets::client::Paths) -> aterm_secrets::Result<Option<Vec<String>>>,
    addrs: impl FnOnce(&[String]) -> Vec<IpAddr>,
) -> Vec<IpAddr> {
    let denied = match vault {
        Some(paths) => read(paths).ok().flatten(),
        None => Some(Vec::new()),
    };
    denied.map_or_else(Vec::new, |denied| addrs(&denied))
}

/// [`fleet`] of what it read: `sharing`, what `[swarm] enabled` reads (sharing off dials no
/// member); `members`, the fleet's addresses; `reaches`, whether this network reaches a member;
/// `guard`, whether this process may dial a member's toolset server. On a Mac a dial to a member
/// on the LAN goes through the LAN broker (`aterm_uds::lan`), and with no broker it fails as that
/// member's.
fn fleet_of(
    sharing: Result<bool, String>,
    members: &[IpAddr],
    reaches: impl Fn(IpAddr) -> bool,
    guard: impl Fn(SocketAddr) -> bool,
) -> atpkg::peers::Fleet {
    let held = match sharing {
        Ok(true) => None,
        Ok(false) => Some(
            "sharing is off ([swarm] enabled = false in aterm.toml), so a download dials no \
             fleet member"
                .to_owned(),
        ),
        Err(why) => Some(aterm_secrets::swarm::unreadable(&why)),
    };
    if let Some(held) = held {
        return atpkg::peers::Fleet {
            members: members.len(),
            dial: Vec::new(),
            held: Some(held),
        };
    }
    let mut any_reached = false;
    let dial: Vec<String> = members
        .iter()
        .copied()
        .filter(|ip| {
            let ok = reaches(*ip);
            any_reached |= ok;
            ok
        })
        .map(|ip| SocketAddr::new(ip, atpkg::peers::DEFAULT_PORT))
        .filter(|at| guard(*at))
        .map(|at| at.to_string())
        .collect();
    if !any_reached && !members.is_empty() {
        return atpkg::peers::Fleet {
            members: members.len(),
            dial: Vec::new(),
            held: Some(
                "this Mac is off the ALab LAN, and no fleet member is on the network it is on"
                    .to_owned(),
            ),
        };
    }
    atpkg::peers::Fleet {
        members: members.len(),
        dial,
        held: None,
    }
}

#[cfg(test)]
#[path = "unit_peers_tests.rs"]
mod tests;
