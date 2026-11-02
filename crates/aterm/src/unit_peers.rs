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
/// which; none an isolated debug drive's loopback guard refuses to dial; none while this Mac's
/// deny list cannot be read.
pub(crate) fn fleet() -> atpkg::peers::Fleet {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return atpkg::peers::Fleet::default();
    };
    let isolated = cfg!(test)
        || std::env::var_os("ATERM_STATE_HOME").is_some()
        || std::env::var_os("CI").is_some();
    // A live run with no home reads no vault, so no deny list: it dials no member, as with no
    // state directory.
    let paths = match (!isolated).then(aterm_secrets::client::Paths::live) {
        None => None,
        Some(Ok(paths)) => Some(paths),
        Some(Err(_)) => return atpkg::peers::Fleet::default(),
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
    fleet_of(sharing, &members, local_network_holds, reaches)
}

/// The fleet's addresses (`addrs` under a deny list) as this Mac's deny list in `vault` lets them
/// in (`read`): an isolated run reads no vault (`None`), so its list names no one; a vault whose
/// deny list cannot be read now (`Err`, or `Ok(None)`: a sync applying, a failed apply) lets no
/// member's address in until it can.
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
    reaches: impl Fn(IpAddr) -> bool,
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
    let reached: Vec<IpAddr> = members.iter().copied().filter(|ip| reaches(*ip)).collect();
    if reached.is_empty() && !members.is_empty() {
        return atpkg::peers::Fleet {
            members: members.len(),
            dial: Vec::new(),
            held: Some(
                "this Mac is off the ALab LAN, and no fleet member is on the network it is on"
                    .to_owned(),
            ),
        };
    }
    let dial = reached
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
    use super::{fleet_of, members_of};
    use std::net::{IpAddr, Ipv4Addr};

    /// A download lets in no member's address while this Mac's deny list cannot be read (a failed
    /// read, or a sync applying), only once it reads; an isolated run reads no vault, and so
    /// names no one.
    ///
    /// NEGATIVE CONTROL (2026-11-01): with `members_of` reading an unread deny list as one that
    /// names no one (`.unwrap_or_default()`, the shape before), this fails (`no member while the
    /// deny list is unread`).
    #[test]
    fn a_download_lets_in_no_member_while_the_deny_list_is_unread() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        let member = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30));
        let addrs = |denied: &[String]| {
            assert!(denied.is_empty());
            vec![member]
        };
        for unread in [Err(aterm_secrets::Error::new("unreadable")), Ok(None)] {
            assert!(
                members_of(Some(&paths), |_| unread, addrs).is_empty(),
                "no member while the deny list is unread"
            );
        }
        assert_eq!(
            members_of(Some(&paths), |_| Ok(Some(Vec::new())), addrs),
            vec![member]
        );
        assert_eq!(
            members_of(None, |_| panic!("an isolated run reads no vault"), addrs),
            vec![member]
        );
    }

    /// A download dials fleet members only while `[swarm] enabled` reads on: sharing off, or a
    /// switch that cannot be read, dials none and says why, without reading Local Network state.
    /// NEGATIVE CONTROL: hold the dials only on `Ok(false)`: the unreadable switch dials.
    #[test]
    fn a_download_dials_no_member_unless_sharing_reads_on() {
        let members = [IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30))];
        let on = fleet_of(Ok(true), &members, || None, |_| true);
        assert_eq!(on.dial.len(), 1, "sharing on: the member is dialed");
        assert_eq!(on.held, None);
        for (sharing, says) in [
            (Ok(false), "sharing is off"),
            (
                Err("aterm.toml: not UTF-8 text".to_owned()),
                "cannot read its settings file",
            ),
        ] {
            let held = fleet_of(
                sharing.clone(),
                &members,
                || panic!("sharing held: no Local Network read"),
                |_| true,
            );
            assert!(held.dial.is_empty(), "{sharing:?}: no member dialed");
            assert_eq!(held.members, 1, "{sharing:?}: the server's count stays");
            assert!(
                held.held.as_deref().is_some_and(|why| why.contains(says)),
                "{sharing:?}: {:?}",
                held.held
            );
        }
        let refused = fleet_of(Ok(true), &members, || Some("macOS refused"), |_| true);
        assert!(refused.dial.is_empty());
        assert_eq!(refused.held.as_deref(), Some("macOS refused"));
    }

    /// Away from the ALab LAN a download races only the fleet members this network reaches;
    /// with none it races no member and says the network holds it, never macOS's Local Network.
    /// NEGATIVE CONTROL: dropping the `reaches` filter dials the member behind the ALab LAN, and
    /// "only the member this network reaches" fails.
    #[test]
    fn away_a_download_races_only_the_members_this_network_reaches() {
        let home = IpAddr::V4(Ipv4Addr::new(192, 168, 4, 30));
        let here = IpAddr::V4(Ipv4Addr::new(172, 20, 10, 5));
        let away = |ip: IpAddr| ip == here;
        let fleet = fleet_of(Ok(true), &[home, here], || None, away);
        assert_eq!(
            fleet.dial,
            [format!("{here}:{}", atpkg::peers::DEFAULT_PORT)],
            "only the member this network reaches"
        );
        assert_eq!((fleet.members, fleet.held), (2, None));
        let none = fleet_of(Ok(true), &[home], || None, away);
        assert!(none.dial.is_empty());
        assert_eq!(none.members, 1);
        assert!(none.held.is_some(), "the network holds it");
    }
}
