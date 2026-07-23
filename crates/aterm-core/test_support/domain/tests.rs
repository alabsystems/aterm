// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_types::TerminalSize;
use std::path::PathBuf;
use std::sync::Arc;

/// Minimal `Domain` implementation for tests that don't need controllable behavior.
struct MockDomain {
    id: DomainId,
    name: String,
}

impl Domain for MockDomain {
    fn domain_id(&self) -> DomainId {
        self.id
    }
    fn domain_name(&self) -> &str {
        &self.name
    }
    fn domain_type(&self) -> DomainType {
        DomainType::Local
    }
    fn state(&self) -> DomainState {
        DomainState::Attached
    }
    fn detachable(&self) -> bool {
        false
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
        vec![]
    }
    fn remove_pane(&self, _id: PaneId) -> Option<Arc<dyn Pane>> {
        None
    }
}

#[test]
fn domain_state_default() {
    assert_eq!(DomainState::default(), DomainState::Detached);
}

#[test]
fn domain_state_all_variants_debug() {
    // Ensure all DomainState variants are constructible and have distinct Debug output.
    let variants = [
        DomainState::Detached,
        DomainState::Attached,
        DomainState::Connecting,
        DomainState::Failed,
    ];
    for (i, a) in variants.iter().enumerate() {
        let da = format!("{a:?}");
        assert!(!da.is_empty());
        for b in &variants[i + 1..] {
            let db = format!("{b:?}");
            assert_ne!(a, b, "variants should be distinct");
            assert_ne!(da, db, "debug output should be distinct");
        }
    }
}

#[test]
fn domain_type_all_variants_debug() {
    // Ensure all DomainType variants are constructible and have distinct Debug output.
    let variants = [
        DomainType::Local,
        DomainType::Ssh,
        DomainType::Wsl,
        DomainType::Serial,
        DomainType::Mux,
        DomainType::SshMultiplexer,
        DomainType::Custom,
    ];
    for (i, a) in variants.iter().enumerate() {
        let da = format!("{a:?}");
        assert!(!da.is_empty());
        for b in &variants[i + 1..] {
            let db = format!("{b:?}");
            assert_ne!(a, b, "variants should be distinct");
            assert_ne!(da, db, "debug output should be distinct");
        }
    }
}

#[test]
fn domain_id_monotonic_and_display() {
    let id1 = DomainId::new();
    let id2 = DomainId::new();
    // Counter is monotonically increasing — stronger than just "not equal".
    assert!(id2.raw() > id1.raw());
    // Display format includes "domain:" prefix.
    let display = format!("{id1}");
    assert!(
        display.starts_with("domain:"),
        "expected 'domain:' prefix, got: {display}"
    );
}

#[test]
fn pane_id_monotonic_and_display() {
    let id1 = PaneId::new();
    let id2 = PaneId::new();
    assert!(id2.raw() > id1.raw());
    let display = format!("{id1}");
    assert!(
        display.starts_with("pane:"),
        "expected 'pane:' prefix, got: {display}"
    );
}

#[test]
fn spawn_config_builder() {
    let config = SpawnConfig::command("bash")
        .with_cwd("/home/user")
        .with_arg("-c")
        .with_arg("echo hello")
        .with_args(["-l", "-a"])
        .with_env("TERM", "xterm-256color");

    assert_eq!(config.command, Some("bash".to_string()));
    assert_eq!(config.cwd, Some(PathBuf::from("/home/user")));
    assert_eq!(config.args, vec!["-c", "echo hello", "-l", "-a"]);
    assert_eq!(config.env.get("TERM"), Some(&"xterm-256color".to_string()));
    assert!(config.env_remove.is_empty());
}

#[test]
fn spawn_config_default_shell() {
    let config = SpawnConfig::default_shell();
    assert_eq!(config.command, None);
    assert!(config.args.is_empty());
}

#[test]
fn ssh_config_builder() {
    let config = SshConfig::new("example.com").with_port(2222);

    assert_eq!(config.host, "example.com");
    assert_eq!(config.port, 2222);
    assert!(config.use_agent);
    assert_eq!(config.connect_timeout_secs, 30);
}

#[test]
fn serial_config_builder() {
    let config = SerialConfig::new("/dev/ttyUSB0").with_baud_rate(9600);

    assert_eq!(config.port, "/dev/ttyUSB0");
    assert_eq!(config.baud_rate, 9600);
}

#[test]
fn domain_error_display() {
    let err = DomainError::NotAttached;
    assert_eq!(err.to_string(), "domain is not attached");

    let err = DomainError::ConnectionFailed("timeout".to_string());
    assert_eq!(err.to_string(), "connection failed: timeout");

    let pane_id = PaneId::new();
    let err = DomainError::PaneNotFound(pane_id);
    assert_eq!(
        err.to_string(),
        format!("pane not found: {:?}", pane_id.raw())
    );

    let domain_id = DomainId::new();
    let err = DomainError::DomainNotFound(domain_id);
    assert_eq!(
        err.to_string(),
        format!("domain not found: {:?}", domain_id.raw())
    );

    let err = DomainError::AuthenticationFailed("bad key".to_string());
    assert_eq!(err.to_string(), "authentication failed: bad key");

    let err = DomainError::Timeout;
    assert_eq!(err.to_string(), "operation timed out");

    let err = DomainError::Other("oops".to_string());
    assert_eq!(err.to_string(), "oops");
}

#[test]
fn pane_trait_methods_are_callable() {
    struct MockPane {
        pane_id: PaneId,
        domain_id: DomainId,
    }

    impl Pane for MockPane {
        fn pane_id(&self) -> PaneId {
            self.pane_id
        }

        fn domain_id(&self) -> DomainId {
            self.domain_id
        }

        fn size(&self) -> TerminalSize {
            TerminalSize::new(24, 80)
        }

        fn resize(&self, _size: TerminalSize) -> DomainResult<()> {
            Ok(())
        }

        fn write(&self, data: &[u8]) -> DomainResult<usize> {
            Ok(data.len())
        }

        fn read(&self, buf: &mut [u8]) -> DomainResult<usize> {
            let n = buf.len().min(4);
            buf[..n].copy_from_slice(&b"test"[..n]);
            Ok(n)
        }

        fn is_alive(&self) -> bool {
            true
        }

        fn exit_status(&self) -> Option<i32> {
            None
        }

        fn kill(&self) -> DomainResult<()> {
            Ok(())
        }
    }

    let pane: Arc<dyn Pane> = Arc::new(MockPane {
        pane_id: PaneId::new(),
        domain_id: DomainId::new(),
    });

    let mut buf = [0_u8; 8];
    let pane_id = pane.pane_id();
    let domain_id = pane.domain_id();
    assert_eq!(pane_id, pane.pane_id());
    assert_eq!(domain_id, pane.domain_id());
    assert_eq!(pane.size(), TerminalSize::new(24, 80));
    assert_eq!(pane.write(b"abc").unwrap(), 3);
    assert_eq!(pane.read(&mut buf).unwrap(), 4);
    assert_eq!(&buf[..4], b"test");
    assert!(pane.is_alive());
    assert_eq!(pane.exit_status(), None);
    pane.resize(TerminalSize::new(40, 120))
        .expect("invariant: mock resize should succeed");
    pane.kill().unwrap();
    assert_eq!(pane.pid(), None);
    assert!(pane.title().is_empty());
    assert_eq!(pane.cwd(), None);
    assert_eq!(pane.foreground_process_name(), None);
}

#[test]
fn domain_trait_methods_are_callable() {
    let domain: Arc<dyn Domain> = Arc::new(MockDomain {
        id: DomainId::new(),
        name: "local".to_string(),
    });

    assert_eq!(domain.domain_name(), "local");
    assert_eq!(domain.domain_label(), "local".to_string());
    assert_eq!(domain.domain_type(), DomainType::Local);
    assert_eq!(domain.state(), DomainState::Attached);
    assert!(domain.spawnable());
    assert!(!domain.detachable());
    domain.attach().unwrap();
    domain.detach().unwrap();
    assert!(domain.list_panes().is_empty());
    assert!(domain.get_pane(PaneId::new()).is_none());
    assert!(domain.remove_pane(PaneId::new()).is_none());
    assert!(matches!(
        domain.spawn_pane(TerminalSize::new(24, 80), SpawnConfig::default()),
        Err(DomainError::NotSupported(_))
    ));
}

#[test]
fn domain_registry_operations() {
    let registry = DomainRegistry::new();

    // Register domains
    let domain1 = Arc::new(MockDomain {
        id: DomainId::new(),
        name: "local".to_string(),
    });
    let domain2 = Arc::new(MockDomain {
        id: DomainId::new(),
        name: "ssh".to_string(),
    });

    registry.register(domain1.clone());
    registry.register(domain2.clone());

    // Check list
    assert_eq!(registry.list().len(), 2);

    // Check get — verify correct domain returned for each ID
    let got1 = registry.get(domain1.domain_id()).unwrap();
    assert_eq!(got1.domain_name(), "local");
    let got2 = registry.get(domain2.domain_id()).unwrap();
    assert_eq!(got2.domain_name(), "ssh");

    // Check get by name — verify correct domain returned
    assert_eq!(
        registry.get_by_name("local").unwrap().domain_id(),
        domain1.domain_id()
    );
    assert_eq!(
        registry.get_by_name("ssh").unwrap().domain_id(),
        domain2.domain_id()
    );
    assert!(registry.get_by_name("nonexistent").is_none());

    // Check default (first registered)
    let default = registry.default_domain().unwrap();
    assert_eq!(default.domain_id(), domain1.domain_id());

    // Change default
    registry.set_default(domain2.domain_id());
    let default = registry.default_domain().unwrap();
    assert_eq!(default.domain_id(), domain2.domain_id());

    // Unregister
    let _ = registry.unregister(domain1.domain_id());
    assert!(registry.get(domain1.domain_id()).is_none());
    assert_eq!(registry.list().len(), 1);
}

#[test]
fn pane_dimension_order_round_trip() {
    use aterm_types::sync::Mutex;

    struct DimensionPane {
        id: PaneId,
        domain_id: DomainId,
        size: Mutex<TerminalSize>,
    }

    impl Pane for DimensionPane {
        fn pane_id(&self) -> PaneId {
            self.id
        }
        fn domain_id(&self) -> DomainId {
            self.domain_id
        }
        fn size(&self) -> TerminalSize {
            *self.size.lock()
        }
        fn resize(&self, size: TerminalSize) -> DomainResult<()> {
            *self.size.lock() = size;
            Ok(())
        }
        fn write(&self, data: &[u8]) -> DomainResult<usize> {
            Ok(data.len())
        }
        fn read(&self, _buf: &mut [u8]) -> DomainResult<usize> {
            Ok(0)
        }
        fn is_alive(&self) -> bool {
            true
        }
        fn exit_status(&self) -> Option<i32> {
            None
        }
        fn kill(&self) -> DomainResult<()> {
            Ok(())
        }
    }

    let pane: Arc<dyn Pane> = Arc::new(DimensionPane {
        id: PaneId::new(),
        domain_id: DomainId::new(),
        size: Mutex::new(TerminalSize::new(24, 80)),
    });

    let initial = pane.size();
    assert_eq!(
        initial,
        TerminalSize::new(24, 80),
        "initial dimensions should preserve rows/cols"
    );

    pane.resize(TerminalSize::new(40, 120))
        .expect("invariant: mock resize should succeed");

    let after = pane.size();
    assert_eq!(
        after,
        TerminalSize::new(40, 120),
        "resize should preserve rows/cols"
    );
}
