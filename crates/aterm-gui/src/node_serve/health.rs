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
    /// Explicit policy, absent enrollment prerequisites, or an unavailable LAN broker.
    Held,
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
            return Standing::Held;
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
        let verdict = denied_under(Some(&denied), log.as_deref());
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
        addr.set_ip(if addr.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
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
    let json = reply
        .strip_prefix("OK ")
        .ok_or("node health request was not answered successfully")?;
    aterm_agent::fleet_census::SwarmCensus::from_json(json)
        .map_err(|_| "node health census is invalid")?;
    Ok(Proven(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_requires_the_expected_authenticated_node_and_a_complete_census() {
        let expected = NodeIdentity::from_seed(&"1".repeat(32), &[1; 32]).unwrap();
        let other = NodeIdentity::from_seed(&"2".repeat(32), &[2; 32]).unwrap();
        let census = aterm_agent::fleet_census::SwarmCensus::default().to_json();
        for (node, reply, succeeds) in [
            (expected.clone(), Some(format!("OK {census}\n")), true),
            (other, Some(format!("OK {census}\n")), false),
            (expected.clone(), None, false),
            (expected.clone(), Some("OK {}\n".to_owned()), false),
        ] {
            let secret = aterm_net::fleet::FleetSecret::new(&[0x5a; 24]).unwrap();
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let addr = listener.local_addr().unwrap();
            let running = Arc::new(AtomicBool::new(true));
            let serving = Arc::clone(&running);
            let listening = aterm_net::fleet::Listening {
                secret: secret.clone(),
                scope: FleetScope::Facts,
                node: Some(node),
                serves: &[FleetOp::Census],
                slots: 2,
                per_source: 2,
                lifetime: aterm_spec::wait::MUST_HAPPEN,
            };
            let server = std::thread::spawn(move || {
                aterm_net::fleet::serve_with_handshake(
                    &listener,
                    &listening,
                    &serving,
                    aterm_spec::wait::MUST_HAPPEN,
                    move |served, _| {
                        served.answer(|stream| {
                            if let Some(reply) = &reply {
                                stream.write_all(reply.as_bytes())?;
                                stream.flush()?;
                            }
                            Ok(())
                        })
                    },
                    |_| {},
                );
            });
            let proved = prove(addr, &secret, &expected, aterm_spec::wait::MUST_HAPPEN);
            running.store(false, Ordering::Release);
            server.join().unwrap();
            assert_eq!(proved.is_ok(), succeeds, "{proved:?}");
        }
    }
}
