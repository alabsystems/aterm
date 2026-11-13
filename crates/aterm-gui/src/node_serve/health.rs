// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Boot health for the node's admitted listener, separate from unit/PID readiness.

use super::*;
use std::io::BufRead as _;

#[derive(Clone)]
pub(super) struct Inputs {
    state: PathBuf,
    config: Option<PathBuf>,
    vault: Option<aterm_secrets::client::Paths>,
    members: Option<Arc<aterm_agent::fleet_log::LocalLog>>,
    lan_unavailable: fn() -> Option<String>,
}

pub(super) enum Standing {
    /// Explicit policy or absent enrollment prerequisites.
    Held,
    /// An input that does not read now, which says nothing of this build
    /// ([`NodeUpdates::trial_unprovable`]).
    Unread(&'static str),
    Admitted(Box<Basis>),
}

pub(super) struct Basis {
    wanted: Wanted,
    secret: aterm_net::fleet::FleetSecret,
    node: NodeIdentity,
    vault: Option<(
        Option<aterm_secrets::client::Config>,
        std::mem::Discriminant<aterm_secrets::client::Membership>,
    )>,
}

impl Inputs {
    pub(super) fn of(cfg: &NodeServeConfig) -> Self {
        Self {
            state: cfg.state_dir.clone(),
            config: cfg.config_path.clone(),
            vault: cfg.vault_paths.clone(),
            members: cfg.members.clone(),
            lan_unavailable: cfg.lan_unavailable,
        }
    }

    pub(super) fn observe(&self) -> Standing {
        if aterm_agent::node_service::left_the_fleet(&self.state) {
            return Standing::Held;
        }
        let config = match self.config.as_deref() {
            None => String::new(),
            Some(path) => match aterm_types::dirs::read_aterm_config_text(path) {
                Ok(text) => text.unwrap_or_default(),
                Err(_) => return Standing::Unread("node configuration is unreadable"),
            },
        };
        let wanted = match node_table_in(&config, &default_node_wanted()) {
            Some(None) => return Standing::Held,
            Some(Some(wanted)) if level_of(wanted.allow.as_deref()).is_some() => wanted,
            _ => return Standing::Unread("node configuration does not admit a listener"),
        };
        let secret = match aterm_net::fleet::read_key_file(&self.state) {
            Ok(secret) => secret,
            Err(aterm_net::fleet::KeyFileError::Missing) => return Standing::Held,
            Err(_) => return Standing::Unread("fleet key is unreadable or invalid"),
        };
        let node = match aterm_net::fleet_member::read_node_identity(&self.state) {
            Ok(Some(node)) => node,
            Ok(None) => return Standing::Held,
            Err(_) => return Standing::Unread("node identity is unreadable or invalid"),
        };
        if (self.lan_unavailable)().is_some() {
            return Standing::Unread("the LAN broker does not answer");
        }
        let (vault, denied) = match self.vault.as_ref() {
            Some(paths) => {
                let status = match aterm_secrets::client::status(paths) {
                    Ok(status) => status,
                    Err(_) => return Standing::Unread("node vault policy is unreadable"),
                };
                let denied = match aterm_secrets::client::swarm_denied_nodes_now(paths) {
                    Ok(Some(denied)) => denied,
                    _ => return Standing::Unread("node membership verdict is unreadable"),
                };
                (
                    Some((status.config, std::mem::discriminant(&status.membership))),
                    denied,
                )
            }
            None => (None, Vec::new()),
        };
        let log = self.members.as_ref().and_then(|members| members.keys());
        if log.as_ref().is_some_and(|log| log.unread().is_some()) {
            return Standing::Unread("node membership log is unreadable");
        }
        let verdict = super::super::denied_for_fleet(
            &self.state,
            self.vault.as_ref(),
            Some(&denied),
            log.as_deref(),
        );
        let own = aterm_net::fleet::NodeProof {
            node: node.node().to_owned(),
            key: node.key(),
        };
        if verdict.refuses(Some(&own)) {
            return Standing::Held;
        }
        Standing::Admitted(Box::new(Basis {
            wanted,
            secret,
            node,
            vault,
        }))
    }
}

impl Basis {
    pub(super) fn is_generation(
        &self,
        secret: &aterm_net::fleet::FleetSecret,
        node: &Option<(String, aterm_net::fleet::NodeKey)>,
        listen: &str,
    ) -> bool {
        &self.secret == secret
            && node
                .as_ref()
                .is_some_and(|(id, key)| id == self.node.node() && *key == self.node.key())
            && self.wanted.listen == listen
    }

    pub(super) fn same_as(&self, other: &Self) -> bool {
        self.wanted.listen == other.wanted.listen
            && self.wanted.allow == other.wanted.allow
            && self.wanted.drive_from == other.wanted.drive_from
            && self.secret == other.secret
            && self.node.node() == other.node.node()
            && self.node.key() == other.node.key()
            && self.vault == other.vault
    }

    pub(super) fn prove(&self, addr: SocketAddr, within: Duration) -> Result<Proven, String> {
        prove(addr, &self.secret, &self.node, within)
    }
}

/// Constructed only by a complete, authenticated reply from the expected node.
#[derive(Debug)]
pub(super) struct Proven(());

fn loopback_of(mut addr: SocketAddr) -> SocketAddr {
    addr.set_ip(if addr.is_ipv4() {
        std::net::Ipv4Addr::LOCALHOST.into()
    } else {
        std::net::Ipv6Addr::LOCALHOST.into()
    });
    addr
}

/// Whether another socket holds the loopback address of a wildcard listener's port. macOS lets the
/// two coexist and gives a dial of that address to the more specific socket, so a self-dial reaches
/// something else (an `ssh -L`, a local server) while this listener serves every other address, and
/// its miss says nothing of this build. Binding that address fails only for such a socket: the
/// listener's own wildcard one does not conflict.
pub(super) fn loopback_held_elsewhere(addr: SocketAddr) -> bool {
    addr.ip().is_unspecified()
        && matches!(
            aterm_uds::kept_out(|| std::net::TcpListener::bind(loopback_of(addr))),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse
        )
}

/// Why a self-dial of the listener at `addr` that missed says nothing of this build, if it does not.
/// Its loopback address may be another socket's ([`loopback_held_elsewhere`]). A listener named on an
/// address that is no fleet source (a Tailscale or public one) refuses a connection from that address
/// as it does any peer's, so no dial of it can ever answer, and every later release would meet the same.
pub(super) fn self_dial_says_nothing(addr: SocketAddr) -> Option<&'static str> {
    if loopback_held_elsewhere(addr) {
        Some("another socket holds this listener's loopback address")
    } else if !addr.ip().is_unspecified() && !aterm_net::fleet::is_fleet_source(addr.ip()) {
        Some("this listener refuses a dial from its own address, which is no fleet source")
    } else {
        None
    }
}

fn prove(
    mut addr: SocketAddr,
    secret: &aterm_net::fleet::FleetSecret,
    node: &NodeIdentity,
    within: Duration,
) -> Result<Proven, String> {
    let until = Instant::now()
        .checked_add(within)
        .ok_or("node health deadline is out of range")?;
    if addr.ip().is_unspecified() {
        addr = loopback_of(addr);
    }
    let left = || {
        until
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or("node health deadline expired")
    };
    let tcp = aterm_uds::connect_tcp(&addr, left()?).map_err(|e| e.to_string())?;
    // Prove this node using the ordinary fleet protocol, without publishing peer-heard
    // evidence or requiring this node to be its own mesh neighbour.
    let me = node.clone().dialing_mesh(None).noting(None);
    let reached =
        aterm_net::fleet::client_handshake_as(tcp, secret, Some(&me), FleetOp::Census, left()?)
            .map_err(|e| e.to_string())?;
    if !reached
        .peer
        .as_ref()
        .is_some_and(|peer| peer.node == node.node() && peer.key == node.key())
    {
        return Err("node health reply came from another or unproved node".into());
    }
    let stream = reached
        .transport
        .into_bounded_stream(until)
        .map_err(|e| e.to_string())?;
    let limit = aterm_net::fleet_member::MAX_CENSUS_REPLY;
    let mut reader = std::io::BufReader::new(stream.take(limit as u64 + 7));
    let mut reply = String::new();
    reader.read_line(&mut reply).map_err(|e| e.to_string())?;
    if reply.len() > limit + 6 || !reply.ends_with('\n') {
        return Err("node health reply is oversized or unfinished".into());
    }
    // A framed refusal from the authenticated node proves its listener as a census does. The census's
    // own inputs (a vault this Mac has no Commons in, a writer holding the roster) say nothing of this
    // build, and a Mac in that state would otherwise revert every update.
    if reply.starts_with("ERR ") {
        return Ok(Proven(()));
    }
    let json = reply
        .strip_prefix("OK ")
        .ok_or("node health request was not answered successfully")?;
    aterm_agent::fleet_census::SwarmCensus::from_json(json)
        .map_err(|_| "node health census is invalid")?;
    Ok(Proven(()))
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod tests;
