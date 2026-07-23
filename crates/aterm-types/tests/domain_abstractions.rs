// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;
use std::sync::Arc;

use aterm_types::TerminalSize;
use aterm_types::domain::{
    Domain, DomainCapabilities, DomainConfigError, DomainConnectionInfo, DomainError, DomainId,
    DomainRegistry, DomainResult, DomainState, DomainType, MuxConnectionConfig, MuxProtocol,
    MuxTransport, Pane, PaneId, SpawnConfig, SshConnectionConfig,
};

struct MockDomain {
    id: DomainId,
    name: &'static str,
    domain_type: DomainType,
    state: DomainState,
    detachable: bool,
    connection: Option<DomainConnectionInfo>,
}

impl MockDomain {
    fn new(
        name: &'static str,
        domain_type: DomainType,
        connection: Option<DomainConnectionInfo>,
    ) -> Self {
        Self {
            id: DomainId::new(),
            name,
            domain_type,
            state: DomainState::Attached,
            detachable: matches!(domain_type, DomainType::Mux | DomainType::SshMultiplexer),
            connection,
        }
    }
}

impl Domain for MockDomain {
    fn domain_id(&self) -> DomainId {
        self.id
    }

    fn domain_name(&self) -> &str {
        self.name
    }

    fn domain_type(&self) -> DomainType {
        self.domain_type
    }

    fn connection_info(&self) -> Option<DomainConnectionInfo> {
        self.connection.clone()
    }

    fn state(&self) -> DomainState {
        self.state
    }

    fn detachable(&self) -> bool {
        self.detachable
    }

    fn attach(&self) -> DomainResult<()> {
        Ok(())
    }

    fn detach(&self) -> DomainResult<()> {
        Ok(())
    }

    fn spawn_pane(&self, _size: TerminalSize, _config: SpawnConfig) -> DomainResult<Arc<dyn Pane>> {
        Err(DomainError::NotSupported("mock".to_string()))
    }

    fn get_pane(&self, _id: PaneId) -> Option<Arc<dyn Pane>> {
        None
    }

    fn list_panes(&self) -> Vec<Arc<dyn Pane>> {
        Vec::new()
    }

    fn remove_pane(&self, _id: PaneId) -> Option<Arc<dyn Pane>> {
        None
    }
}

#[test]
fn ssh_target_parses_user_host_and_port() {
    let target = SshConnectionConfig::parse("alice@remote.invalid:2200").unwrap();
    assert_eq!(target.user.as_deref(), Some("alice"));
    assert_eq!(target.host, "remote.invalid");
    assert_eq!(target.port, 2200);
    assert_eq!(target.destination(), "alice@remote.invalid:2200");
}

#[test]
fn ssh_target_parses_bracketed_ipv6() {
    let target = SshConnectionConfig::parse("root@[2001:db8::10]:2222").unwrap();
    assert_eq!(target.user.as_deref(), Some("root"));
    assert_eq!(target.host, "2001:db8::10");
    assert_eq!(target.port, 2222);
    assert_eq!(target.destination(), "root@[2001:db8::10]:2222");
}

#[test]
fn ssh_target_rejects_empty_host_and_bad_port() {
    assert_eq!(
        SshConnectionConfig::parse(""),
        Err(DomainConfigError::EmptyTarget)
    );
    assert_eq!(
        SshConnectionConfig::parse("alice@"),
        Err(DomainConfigError::MissingHost)
    );
    assert_eq!(
        SshConnectionConfig::parse("remote.invalid:notaport"),
        Err(DomainConfigError::InvalidPort("notaport".to_string()))
    );
}

#[test]
fn domain_capabilities_distinguish_ssh_mux_from_plain_ssh() {
    let plain = DomainCapabilities::for_type(DomainType::Ssh);
    assert!(plain.remote);
    assert!(!plain.multiplexed);
    assert!(!plain.detachable);

    let mux = DomainCapabilities::for_type(DomainType::SshMultiplexer);
    assert!(mux.remote);
    assert!(mux.multiplexed);
    assert!(mux.detachable);
}

#[test]
fn registry_filters_remote_and_multiplexed_domains() {
    let registry = DomainRegistry::new();
    let local: Arc<dyn Domain> = Arc::new(MockDomain::new("local", DomainType::Local, None));
    let ssh: Arc<dyn Domain> = Arc::new(MockDomain::new(
        "ssh",
        DomainType::Ssh,
        Some(DomainConnectionInfo::Ssh(
            SshConnectionConfig::parse("dev@remote.invalid").unwrap(),
        )),
    ));
    let ssh_mux_config = MuxConnectionConfig::ssh(
        "prod",
        SshConnectionConfig::parse("prod@remote.invalid:2022").unwrap(),
    )
    .with_max_panes(16)
    .with_protocol(MuxProtocol::Aterm);
    let ssh_mux: Arc<dyn Domain> = Arc::new(MockDomain::new(
        "prod",
        DomainType::SshMultiplexer,
        Some(DomainConnectionInfo::SshMultiplexer(ssh_mux_config.clone())),
    ));

    registry.register(local);
    registry.register(ssh);
    registry.register(ssh_mux);

    let mut remote_names: Vec<String> = registry
        .list_remote()
        .into_iter()
        .map(|d| d.domain_name().to_string())
        .collect();
    remote_names.sort();
    assert_eq!(remote_names, ["prod", "ssh"]);

    let muxes = registry.list_multiplexers();
    assert_eq!(muxes.len(), 1);
    assert_eq!(muxes[0].domain_name(), "prod");
    assert_eq!(
        muxes[0].connection_info(),
        Some(DomainConnectionInfo::SshMultiplexer(ssh_mux_config))
    );
}

#[test]
fn mux_config_supports_local_socket_transport() {
    let config = MuxConnectionConfig::local_socket("dev", "/tmp/aterm-mux.sock");
    assert_eq!(config.name, "dev");
    assert_eq!(
        config.transport,
        MuxTransport::LocalSocket(PathBuf::from("/tmp/aterm-mux.sock"))
    );
    assert_eq!(config.protocol, MuxProtocol::Aterm);
}

#[test]
fn mux_config_supports_ssh_shared_connection_metadata() {
    let target = SshConnectionConfig::parse("muxer@remote.invalid:2200").unwrap();
    let config = MuxConnectionConfig::ssh("shared", target.clone())
        .with_protocol(MuxProtocol::TmuxControl)
        .with_max_panes(8);

    assert_eq!(config.name, "shared");
    assert_eq!(config.transport, MuxTransport::Ssh(target));
    assert_eq!(config.protocol, MuxProtocol::TmuxControl);
    assert_eq!(config.max_panes, Some(8));
}

#[test]
fn concurrent_register_and_default_domain_does_not_deadlock() {
    // Regression (lock-order inversion): register()/unregister() lock
    // domains(write) -> default_domain(write), while default_domain() USED to hold
    // its default_domain(read) guard while calling get() (which locks domains) — the
    // OPPOSITE nesting, a classic lock-order inversion that deadlocks when a
    // register() and a default_domain() race. default_domain() now copies the id out
    // and drops its guard before get(), so the two locks are never held at once. This
    // stress loop hammers both paths from many threads: pre-fix it hangs (CI timeout),
    // post-fix it completes near-instantly.
    use std::thread;

    let registry = Arc::new(DomainRegistry::new());
    let seed: Arc<dyn Domain> = Arc::new(MockDomain::new("seed", DomainType::Local, None));
    let seed_id = seed.domain_id();
    registry.register(seed);
    registry.set_default(seed_id);

    let mut handles = Vec::new();
    // Writers: churn register/unregister (domains -> default_domain nesting).
    for t in 0..4u64 {
        let reg = Arc::clone(&registry);
        handles.push(thread::spawn(move || {
            for i in 0..2000u64 {
                let d: Arc<dyn Domain> = Arc::new(MockDomain::new("w", DomainType::Local, None));
                let id = d.domain_id();
                reg.register(d);
                if (i + t) % 3 == 0 {
                    // Success is irrelevant here — a racing writer may have already
                    // unregistered it; the test only hammers the lock ordering.
                    let _ = reg.unregister(id);
                }
            }
        }));
    }
    // Readers: hammer default_domain() (default_domain -> domains nesting pre-fix).
    for _ in 0..4 {
        let reg = Arc::clone(&registry);
        handles.push(thread::spawn(move || {
            for _ in 0..2000 {
                let _ = reg.default_domain();
                let _ = reg.list();
            }
        }));
    }
    for h in handles {
        h.join().expect("no worker thread panicked or deadlocked");
    }
    // Reaching here == every thread joined: no deadlock. (Content is racy; the
    // completion itself is the assertion.)
    let _ = registry.default_domain();
}
