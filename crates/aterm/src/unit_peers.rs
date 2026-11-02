// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's fleet as LAN toolset sharing reads it (`atpkg::peers::set_fleet`, owner ruling
//! 2026-10-27: "everything must be ON BY DEFAULT in atpkg!!!"): atpkg reads no fleet book
//! itself, so `aterm` hands it this read of `aterm-net`'s.

use std::net::{IpAddr, SocketAddr};

/// This Mac's fleet members ([`aterm_net::fleet_member::member_addrs`]): how many a server
/// answers, and the ones a download dials, each at the port a member's aterm serves its toolset
/// archives on ([`atpkg::peers::DEFAULT_PORT`]). A download dials none while `[swarm] enabled`
/// does not read on (sharing off, or a file that cannot be read,
/// `docs/DESIGN-alab-swarm-2026-10-27.md` §9), none while macOS has not asked about Local
/// Network, since a dial of a LAN address would ask it then, nor once it was refused, and says
/// which; none an isolated debug drive's loopback guard refuses to dial.
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
    #[cfg(unix)]
    let sharing = aterm_secrets::swarm::enabled_live().map_err(|why| why.to_string());
    #[cfg(not(unix))]
    let sharing = Ok(true);
    fleet_of(sharing, &members, local_network_holds)
}

/// Why macOS's Local Network holds a download's dials, where it does.
fn local_network_holds() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        use aterm_cli::macos_doctor::LocalNetworkHeard;
        match aterm_cli::macos_doctor::local_network_heard() {
            LocalNetworkHeard::On | LocalNetworkHeard::Asked => None,
            LocalNetworkHeard::Untold => Some(
                "macOS has not asked whether aterm may reach your local network, and a \
                 download never raises that question",
            ),
            LocalNetworkHeard::Off => Some(
                "macOS does not let aterm reach your local network; turn it on in System \
                 Settings > Privacy & Security > Local Network",
            ),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// [`fleet`] of what it read: `sharing`, what `[swarm] enabled` reads (asked first: sharing
/// off dials no member, and reads no Local Network state); `members`, the fleet's addresses;
/// `local_network`, why macOS holds the dials, if it does.
fn fleet_of(
    sharing: Result<bool, String>,
    members: &[IpAddr],
    local_network: impl FnOnce() -> Option<&'static str>,
) -> atpkg::peers::Fleet {
    let held = match sharing {
        Ok(true) => local_network().map(str::to_owned),
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

#[cfg(test)]
mod tests {
    use super::fleet_of;
    use std::net::{IpAddr, Ipv4Addr};

    /// A download dials fleet members only while `[swarm] enabled` reads on: sharing off, or a
    /// switch that cannot be read, dials none and says why, without reading Local Network state.
    /// NEGATIVE CONTROL: hold the dials only on `Ok(false)`: the unreadable switch dials.
    #[test]
    fn a_download_dials_no_member_unless_sharing_reads_on() {
        let members = [IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30))];
        let on = fleet_of(Ok(true), &members, || None);
        assert_eq!(on.dial.len(), 1, "sharing on: the member is dialed");
        assert_eq!(on.held, None);
        for (sharing, says) in [
            (Ok(false), "sharing is off"),
            (
                Err("aterm.toml: not UTF-8 text".to_owned()),
                "cannot read its settings file",
            ),
        ] {
            let held = fleet_of(sharing.clone(), &members, || {
                panic!("sharing held: no Local Network read")
            });
            assert!(held.dial.is_empty(), "{sharing:?}: no member dialed");
            assert_eq!(held.members, 1, "{sharing:?}: the server's count stays");
            assert!(
                held.held.as_deref().is_some_and(|why| why.contains(says)),
                "{sharing:?}: {:?}",
                held.held
            );
        }
        let refused = fleet_of(Ok(true), &members, || Some("macOS refused"));
        assert!(refused.dial.is_empty());
        assert_eq!(refused.held.as_deref(), Some("macOS refused"));
    }
}
