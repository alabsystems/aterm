// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::BufRead as _;
use std::sync::mpsc;

struct HeldDiscovery<'a> {
    direct: Vec<String>,
    event: &'a mpsc::Sender<&'static str>,
    release: &'a Mutex<mpsc::Receiver<()>>,
}

impl MemberDiscovery for HeldDiscovery<'_> {
    fn begin(&self, _within: Duration) -> aterm_secrets::Result<NumericMembers> {
        Ok(self.direct.clone().into())
    }

    fn more(&self, _within: Duration) -> aterm_secrets::Result<Vec<String>> {
        self.event.send("dns").unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the controller releases its held discovery");
        Err(aterm_secrets::Error::new(
            "DNS is unavailable in this fixture",
        ))
    }
}

/// A retained member reached through the real, mutually authenticated fleet transport.
/// It refuses the authority extension before possession, so this fixture grants nobody.
struct CachedMember {
    port: u16,
    stop: std::sync::Arc<aterm_uds::stop::Stop>,
    serving: Option<std::thread::JoinHandle<()>>,
    wire: std::sync::Arc<Mutex<Vec<String>>>,
}

impl CachedMember {
    fn listen(secret: &aterm_net::fleet::FleetSecret, event: mpsc::Sender<&'static str>) -> Self {
        Self::listen_with_ask(secret, event, false)
    }

    fn listen_with_ask(
        secret: &aterm_net::fleet::FleetSecret,
        event: mpsc::Sender<&'static str>,
        numeric: bool,
    ) -> Self {
        Self::listen_noting(secret, event, numeric, std::sync::Arc::new(|| {}))
    }

    fn listen_noting(
        secret: &aterm_net::fleet::FleetSecret,
        event: mpsc::Sender<&'static str>,
        numeric: bool,
        note: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        use aterm_net::fleet::{FleetOp, FleetScope, Listening, NodeIdentity};
        use std::sync::Arc;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(aterm_uds::stop::Stop::new());
        let wire = Arc::new(Mutex::new(Vec::new()));
        let listening = Listening {
            secret: secret.clone(),
            scope: FleetScope::Facts,
            node: NodeIdentity::from_seed(&"22".repeat(16), &[22; 32]),
            serves: &[FleetOp::VaultJoin],
            slots: 1,
            per_source: 1,
            lifetime: aterm_spec::wait::MUST_HAPPEN,
        };
        let serving = std::thread::spawn({
            let stop = Arc::clone(&stop);
            let wire = Arc::clone(&wire);
            move || {
                let ask: Option<aterm_net::fleet::AskHandler> = numeric.then(|| {
                    let wire = Arc::clone(&wire);
                    let event = event.clone();
                    Arc::new(move |stream: &mut dyn aterm_net::fleet::Duplex, _| {
                        writeln!(stream,
                            "ALAB member=1 key={} network={} authority={} genesis={} name=numeric allow=1 invite=0 challenge={}",
                            "09".repeat(32), "01".repeat(16), "02".repeat(32),
                            "03".repeat(32), "04".repeat(16),
                        )?;
                        let mut line = String::new();
                        std::io::BufReader::new(&mut *stream).read_line(&mut line)?;
                        wire.lock().unwrap().push(line);
                        event.send("proof").unwrap();
                        note();
                        stream.write_all(b"NO why=unsupported-authority\n")
                    }) as aterm_net::fleet::AskHandler
                });
                aterm_net::fleet::serve_offering_with_ask_within(
                    &listener,
                    (
                        &listening,
                        Arc::new(|| (FleetScope::Facts, &[FleetOp::VaultJoin])),
                        Arc::new(Arc::default),
                    ),
                    ask,
                    &stop,
                    aterm_spec::wait::MUST_HAPPEN,
                    move |served, _| {
                        assert!(
                            served.peer_node().is_some(),
                            "the cached route proves its node"
                        );
                        let (wire, event) = (Arc::clone(&wire), event.clone());
                        served.answer(move |stream| {
                            stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                            stream.set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                            writeln!(stream,
                                "ALAB member=1 key={} network={} authority={} genesis={} name=cached allow=1 invite=0 challenge={}",
                                "09".repeat(32), "01".repeat(16), "02".repeat(32),
                                "03".repeat(32), "04".repeat(16),
                            )?;
                            let mut line = String::new();
                            std::io::BufReader::new(&mut *stream).read_line(&mut line)?;
                            wire.lock().unwrap().push(line);
                            event.send("proof").unwrap();
                            stream.write_all(b"NO why=unsupported-authority\n")
                        })
                    },
                    |_| {},
                );
            }
        });
        Self {
            port,
            stop,
            serving: Some(serving),
            wire,
        }
    }
}

struct NativeRound<'a> {
    probe: Mutex<Option<crate::lan_members::Probe>>,
    second: &'a Mutex<Vec<String>>,
    dns: std::sync::atomic::AtomicUsize,
}

impl MemberDiscovery for NativeRound<'_> {
    fn begin(&self, _: Duration) -> aterm_secrets::Result<NumericMembers> {
        Ok(NumericMembers::Probe(
            self.probe.lock().unwrap().take().unwrap(),
        ))
    }
    fn more(&self, _: Duration) -> aterm_secrets::Result<Vec<String>> {
        assert!(
            self.second
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.starts_with("prove ") && line.contains(" authority=")),
            "the queued numeric member must reach the real guard before DNS"
        );
        self.dns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(aterm_secrets::Error::new(
            "DNS is unavailable in this fixture",
        ))
    }
}

/// Both automatic producers consume later original-nonce UDP replies before DNS.
/// Actual ask TLS refuses each compiled-authority request; nobody is admitted.
#[test]
fn automatic_producers_continue_queued_numeric_refusals_before_dns() {
    for pending in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut mac = me(root.path(), "queued");
        let state = root.path().join("fleet");
        aterm_net::fleet_member::ensure_private_dir(&state).unwrap();
        let node = aterm_net::fleet_member::node_identity(&state).unwrap();
        mac.node = *node.key().bytes();
        if pending {
            let source = format!(
                "url = {:?}\nname = 'alab'\nsigner = '{}'\nhost = 'queued'\n",
                aterm_secrets::ALAB_DEFAULT_URL,
                aterm_secrets::ALAB_SIGNER
            );
            aterm_secrets::fsx::write_atomic(
                &mac.paths.dir.join("source.toml"),
                source.as_bytes(),
                0o600,
            )
            .unwrap();
            aterm_secrets::fsx::write_atomic(
                &mac.paths.dir.join("state.toml"),
                b"membership = 'pending'\n",
                0o600,
            )
            .unwrap();
        } else {
            client::prepare_alab_join_retry(&mac.paths).unwrap();
        }
        let original = client::config(&mac.paths).unwrap();
        let key = aterm_net::fleet_member::mint_secret().unwrap();
        let secret = aterm_net::fleet::FleetSecret::new(&key).unwrap();
        let (event, _events) = mpsc::channel();
        let second = CachedMember::listen_with_ask(&secret, event.clone(), true);
        let (probe, reply) = crate::lan_members::queued_test_probe();
        let reply = std::sync::Arc::new(reply);
        let first = CachedMember::listen_noting(
            &secret,
            event,
            true,
            std::sync::Arc::new({
                let reply = std::sync::Arc::clone(&reply);
                let port = second.port;
                move || reply(port, aterm_net::fleet::NodeKey::from_bytes([2; 32]))
            }),
        );
        reply(first.port, aterm_net::fleet::NodeKey::from_bytes([1; 32]));
        let found = NativeRound {
            probe: Mutex::new(Some(probe)),
            second: &second.wire,
            dns: std::sync::atomic::AtomicUsize::new(0),
        };
        let deadlines = Mutex::new(Vec::new());
        if pending {
            assert!(ask_members_at_state(mac.paths.clone(), &state, &found, &|| Ok(())).is_err());
        } else {
            let reach = Reach {
                browse: &found,
                dial: &|_| Err(aterm_secrets::Error::new("plain request is unanswered")),
                prove: &|mac, ats, until, current| {
                    deadlines.lock().unwrap().push(until);
                    prove_routes_at_state(mac, &state, ats, Some(until), true, current)
                },
                join_vault: &no_vault,
                follow: &|_| Follows::Runs,
            };
            let _ = joined(&mac, &JoinArgs::default(), &reach);
            let deadlines = deadlines.lock().unwrap();
            assert_eq!(deadlines.len(), 2);
            assert_eq!(
                deadlines[0], deadlines[1],
                "all native batches share one early phase"
            );
        }
        assert_eq!(found.dns.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(client::config(&mac.paths).unwrap(), original);
        assert!(!join::pending_proof(&mac.paths).unwrap());
        for member in [&first, &second] {
            assert!(
                member
                    .wire
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|line| line.starts_with("prove ") && line.contains(" authority="))
            );
        }
    }
}

#[test]
fn native_continuation_retains_terminal_outcomes_and_current_authority() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for ending in ["in", "uncertain", "withdrawn"] {
        let allowed = AtomicBool::new(true);
        let attempts = std::cell::Cell::new(0);
        let reads = std::cell::Cell::new(0);
        let until = std::time::Instant::now() + PROVE_WITHIN;
        let current = || {
            if allowed.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(aterm_secrets::Error::new("setup authority changed"))
            }
        };
        let result = bootstrap_native_proof(
            until,
            || {
                reads.set(reads.get() + 1);
                Ok(vec![format!("192.0.2.{}:7100", reads.get())])
            },
            |_, deadline| {
                assert!(deadline <= until);
                attempts.set(attempts.get() + 1);
                if attempts.get() == 1 {
                    if ending == "withdrawn" {
                        allowed.store(false, Ordering::SeqCst);
                    }
                    Proved::No
                } else if ending == "in" {
                    Proved::In
                } else {
                    Proved::SignedMaybe
                }
            },
            &current,
        );
        if ending == "withdrawn" {
            assert!(result.is_err());
            assert_eq!(reads.get(), 1, "withdrawal precedes another native receive");
        } else {
            assert!(matches!(
                (ending, result.unwrap()),
                ("in", Proved::In) | ("uncertain", Proved::SignedMaybe)
            ));
            assert_eq!(
                reads.get(),
                2,
                "a terminal proof never receives another batch"
            );
        }
    }
}

/// Default bootstrap keeps looking after a real guarded refusal. Both listeners use
/// ordinary ask TLS and reject the compiled-authority extension before possession;
/// this proves continuation only and never claims a loopback machine was admitted.
#[test]
fn native_bootstrap_continues_after_a_guarded_numeric_refusal() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut mac = me(root.path(), "cold");
    let state = root.path().join("fleet");
    aterm_net::fleet_member::ensure_private_dir(&state).unwrap();
    let node = aterm_net::fleet_member::node_identity(&state).unwrap();
    mac.node = *node.key().bytes();
    client::prepare_alab_join_retry(&mac.paths).unwrap();
    let key = aterm_net::fleet_member::mint_secret().unwrap();
    let secret = aterm_net::fleet::FleetSecret::new(&key).unwrap();
    let (event, _events) = mpsc::channel();
    let first = CachedMember::listen_with_ask(&secret, event.clone(), true);
    let second = CachedMember::listen_with_ask(&secret, event, true);
    let mut batches = [first.port, second.port].into_iter();
    let until = std::time::Instant::now() + PROVE_WITHIN;
    let current = || {
        if client::config(&mac.paths)?.is_some() {
            Err(aterm_secrets::Error::new("the source was replaced"))
        } else {
            Ok(())
        }
    };
    let outcome = bootstrap_native_proof(
        until,
        || {
            Ok(batches
                .next()
                .map(|port| vec![format!("127.0.0.1:{port}")])
                .unwrap_or_default())
        },
        |ats, deadline| prove_routes_at_state(&mac, &state, ats, Some(deadline), true, &current),
        &current,
    )
    .unwrap();
    assert!(matches!(outcome, Proved::No));
    assert!(client::config(&mac.paths).unwrap().is_none());
    assert!(!join::pending_proof(&mac.paths).unwrap());
    for (index, member) in [&first, &second].into_iter().enumerate() {
        let wire = member.wire.lock().unwrap();
        assert_eq!(
            wire.len(),
            1,
            "numeric listener {index} must reach the actual default guard"
        );
        assert!(wire[0].starts_with("prove ") && wire[0].contains(" authority="));
    }
}

impl Drop for CachedMember {
    fn drop(&mut self) {
        self.stop.set();
        self.serving.take().unwrap().join().unwrap();
    }
}

/// Empty numeric discovery must not leave an authenticated cached route behind DNS.
/// Both a source-free recovery and a configured pending join use the actual runner,
/// saved fleet book, fleet TLS handshake and guarded authority request.
#[test]
fn automatic_fallback_proves_a_cached_member_before_waiting_for_dns() {
    use aterm_net::fleet::{FleetSecret, NodeIdentity};
    use aterm_net::fleet_member::{self, Began, JoinCode, Peer, Via};
    for pending in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut mac = me(root.path(), "restored");
        let state = root.path().join("fleet");
        aterm_secrets::fsx::ensure_private_dir(&state).unwrap();
        let node = NodeIdentity::from_seed(&"07".repeat(16), &[7; 32]).unwrap();
        mac.node = *node.key().bytes();
        let code = JoinCode {
            secret: fleet_member::mint_secret().unwrap(),
            hint: None,
        };
        let secret = FleetSecret::new(&code.secret).unwrap();
        let (event, events) = mpsc::channel();
        let cached = CachedMember::listen(&secret, event.clone());
        fleet_member::write_held(&state, Began::Here, |book| {
            book.write_key(&code.secret).unwrap();
        })
        .unwrap();
        assert!(
            fleet_member::read_join_code(&state).is_err(),
            "a joined member holds the fleet key, not an issuer's invitation file"
        );
        fleet_member::update_peers(&state, |book| {
            book.push(Peer {
                node: Some("22".repeat(16)),
                key: None,
                key_from: None,
                host: "retained.invalid".into(),
                addr: Some(std::net::Ipv4Addr::LOCALHOST.into()),
                port: cached.port,
                version: None,
                seen_ms: None,
                via: Via::Joined,
                fleet: Some(secret.mark()),
                signed: None,
            });
            Ok(())
        })
        .unwrap();
        if pending {
            let source = format!(
                "url = {:?}\nname = 'alab'\nsigner = '{}'\nhost = 'restored'\n",
                aterm_secrets::ALAB_DEFAULT_URL,
                aterm_secrets::ALAB_SIGNER,
            );
            aterm_secrets::fsx::write_atomic(
                &mac.paths.dir.join("source.toml"),
                source.as_bytes(),
                0o600,
            )
            .unwrap();
        } else {
            client::prepare_alab_join_retry(&mac.paths).unwrap();
        }
        let original = client::config(&mac.paths).unwrap();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        let found = HeldDiscovery {
            direct: Vec::new(),
            event: &event,
            release: &released,
        };
        let proof = |mac: &Me, ats: &[String], until, current: &Current<'_>| {
            current().unwrap();
            assert!(ats.is_empty(), "there is no discovered endpoint");
            let expected = client::config(&mac.paths).unwrap();
            let proving = join::ProofLock::take(&mac.paths, None).unwrap().unwrap();
            let scope =
                ProofScope::read(&mac.paths, expected.as_ref(), true, current, &proving).unwrap();
            prove_retained_routes(
                &mac.paths,
                &state,
                &mac.joining().unwrap(),
                &node,
                Some(until),
                &scope.admission,
            )
        };
        let reach = Reach {
            browse: &found,
            prove: &proof,
            dial: &|_| panic!("unavailable DNS ends this refused round"),
            join_vault: &no_vault,
            follow: &|_| Follows::Runs,
        };
        let first = std::thread::scope(|threads| {
            let worker = threads.spawn(|| joined(&mac, &JoinArgs::default(), &reach));
            let first = events.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
            // Clean up the held source before asserting, including the red path.
            release.send(()).unwrap();
            let _ = worker.join().unwrap();
            first
        });
        assert_eq!(first, "proof", "pending={pending}");
        let wire = cached.wire.lock().unwrap();
        assert_eq!(wire.len(), 1);
        assert!(wire[0].starts_with("prove ") && wire[0].contains(" authority="));
        assert_eq!(client::config(&mac.paths).unwrap(), original);
        assert!(!join::pending_proof(&mac.paths).unwrap());
    }
}

/// An issuer invitation left beside a missing or unsafe current key is not fleet custody.
/// The real transport is listening: an accidental invitation fallback would enter its proof.
#[test]
fn retained_proof_refuses_an_invitation_without_a_readable_current_fleet_key() {
    use aterm_net::fleet::{FleetSecret, NodeIdentity};
    use aterm_net::fleet_member::{self, Began, JoinCode, Peer, Via};
    use std::os::unix::fs::PermissionsExt as _;
    for missing in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut mac = me(root.path(), "restored");
        let state = root.path().join("fleet");
        aterm_secrets::fsx::ensure_private_dir(&state).unwrap();
        let node = NodeIdentity::from_seed(&"07".repeat(16), &[7; 32]).unwrap();
        mac.node = *node.key().bytes();
        let code = JoinCode {
            secret: fleet_member::mint_secret().unwrap(),
            hint: None,
        };
        let secret = FleetSecret::new(&code.secret).unwrap();
        let (event, events) = mpsc::channel();
        let cached = CachedMember::listen(&secret, event);
        fleet_member::write_held(&state, Began::Here, |book| {
            book.write_code(&code).unwrap();
            if !missing {
                book.write_key(&code.secret).unwrap();
            }
        })
        .unwrap();
        if !missing {
            std::fs::set_permissions(
                state.join(aterm_net::fleet::KEY_FILE),
                std::fs::Permissions::from_mode(0o640),
            )
            .unwrap();
        }
        assert!(fleet_member::read_join_code(&state).is_ok());
        assert!(aterm_net::fleet::read_key_file(&state).is_err());
        fleet_member::update_peers(&state, |book| {
            book.push(Peer {
                node: Some("22".repeat(16)),
                key: None,
                key_from: None,
                host: "retained.invalid".into(),
                addr: Some(std::net::Ipv4Addr::LOCALHOST.into()),
                port: cached.port,
                version: None,
                seen_ms: None,
                via: Via::Joined,
                fleet: Some(secret.mark()),
                signed: None,
            });
            Ok(())
        })
        .unwrap();
        let expected = client::config(&mac.paths).unwrap();
        let proving = join::ProofLock::take(&mac.paths, None).unwrap().unwrap();
        let current = || Ok(());
        let scope =
            ProofScope::read(&mac.paths, expected.as_ref(), true, &current, &proving).unwrap();
        assert!(matches!(
            prove_retained_routes(
                &mac.paths,
                &state,
                &mac.joining().unwrap(),
                &node,
                Some(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN),
                &scope.admission
            ),
            Proved::No
        ));
        assert!(
            cached.wire.lock().unwrap().is_empty(),
            "no possession is attempted: missing={missing}"
        );
        assert!(events.try_recv().is_err());
        assert_eq!(client::config(&mac.paths).unwrap(), expected);
        assert!(!join::pending_proof(&mac.paths).unwrap());
    }
}

fn pending_subscriber(
    root: &Path,
    cached: &CachedMember,
    key: &[u8],
) -> (Paths, std::path::PathBuf) {
    use aterm_net::fleet::FleetSecret;
    use aterm_net::fleet_member::{self, Began, Peer, Via};
    let mac = me(root, "restored");
    let paths = mac.paths.clone();
    let state = root.join("fleet");
    fleet_member::ensure_private_dir(&state).unwrap();
    fleet_member::node_identity(&state).unwrap();
    let secret = FleetSecret::new(key).unwrap();
    fleet_member::write_held(&state, Began::Here, |book| book.write_key(key))
        .unwrap()
        .unwrap();
    assert!(fleet_member::read_join_code(&state).is_err());
    fleet_member::update_peers(&state, |book| {
        book.push(Peer {
            node: Some("22".repeat(16)),
            key: None,
            key_from: None,
            host: "retained.invalid".into(),
            addr: Some(std::net::Ipv4Addr::LOCALHOST.into()),
            port: cached.port,
            version: None,
            seen_ms: None,
            via: Via::Issuer,
            fleet: Some(secret.mark()),
            signed: None,
        });
        Ok(())
    })
    .unwrap();
    let source = format!(
        "url = {:?}\nname = 'alab'\nsigner = '{}'\nhost = 'restored'\n",
        aterm_secrets::ALAB_DEFAULT_URL,
        aterm_secrets::ALAB_SIGNER
    );
    aterm_secrets::fsx::write_atomic(&paths.dir.join("source.toml"), source.as_bytes(), 0o600)
        .unwrap();
    aterm_secrets::fsx::write_atomic(
        &paths.dir.join("state.toml"),
        b"membership = 'pending'\n",
        0o600,
    )
    .unwrap();
    assert!(matches!(
        client::status(&paths).unwrap().membership,
        client::Membership::Pending
    ));
    (paths, state)
}

/// A route/broker wake on a Pending subscriber must reach its retained peer even while DNS
/// is held and then empty or unavailable. This invokes the real subscriber producer and TLS.
#[test]
fn a_pending_subscriber_proves_its_retained_member_before_held_dns() {
    for returns_error in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let key = aterm_net::fleet_member::mint_secret().unwrap();
        let secret = aterm_net::fleet::FleetSecret::new(&key).unwrap();
        let (event, events) = mpsc::channel();
        let cached = CachedMember::listen(&secret, event.clone());
        let (paths, state) = pending_subscriber(root.path(), &cached, &key);
        let original = client::config(&paths).unwrap();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        let held = HeldDiscovery {
            direct: Vec::new(),
            event: &event,
            release: &released,
        };
        let discovery = || {
            let found = held.more(BROWSE_WITHIN);
            if returns_error { found } else { Ok(Vec::new()) }
        };
        let first = std::thread::scope(|threads| {
            let worker = threads
                .spawn(|| ask_members_at_state(paths.clone(), &state, &discovery, &|| Ok(())));
            let first = events.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
            release.send(()).unwrap();
            assert!(
                worker.join().unwrap().is_err(),
                "the real member refuses before possession, then DNS supplies no member"
            );
            first
        });
        assert_eq!(
            first, "proof",
            "the Pending subscriber must not wait for DNS first"
        );
        let wire = cached.wire.lock().unwrap();
        assert_eq!(wire.len(), 1);
        assert!(wire[0].starts_with("prove ") && wire[0].contains(" authority="));
        assert_eq!(client::config(&paths).unwrap(), original);
        assert!(!join::pending_proof(&paths).unwrap());
    }
}

/// Neither Stop nor a same-authority source replacement during numeric discovery may be
/// adopted by the subscriber's new early retained proof or its later plain request.
#[test]
fn pending_subscriber_rechecks_stop_and_source_before_early_retained_proof() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Withdrawal<'a> {
        change: &'a (dyn Fn() + Sync),
        browses: AtomicUsize,
    }
    impl MemberDiscovery for Withdrawal<'_> {
        fn begin(&self, _: Duration) -> aterm_secrets::Result<NumericMembers> {
            (self.change)();
            Ok(Vec::new().into())
        }
        fn more(&self, _: Duration) -> aterm_secrets::Result<Vec<String>> {
            self.browses.fetch_add(1, Ordering::SeqCst);
            Err(aterm_secrets::Error::new("discovery is unavailable"))
        }
    }
    for source_replaced in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let key = aterm_net::fleet_member::mint_secret().unwrap();
        let secret = aterm_net::fleet::FleetSecret::new(&key).unwrap();
        let (event, events) = mpsc::channel();
        let cached = CachedMember::listen(&secret, event);
        let (paths, state) = pending_subscriber(root.path(), &cached, &key);
        let source_path = paths.dir.join("source.toml");
        let original = std::fs::read_to_string(&source_path).unwrap();
        let replaced = original.replace(
            aterm_secrets::ALAB_DEFAULT_URL,
            "http://127.0.0.1:1/v1/alab",
        );
        let allowed = AtomicBool::new(true);
        let change = || {
            if source_replaced {
                aterm_secrets::fsx::write_atomic(&source_path, replaced.as_bytes(), 0o600).unwrap();
            } else {
                allowed.store(false, Ordering::SeqCst);
            }
        };
        let discovery = Withdrawal {
            change: &change,
            browses: AtomicUsize::new(0),
        };
        let current = || {
            if allowed.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err("setup stopped".into())
            }
        };
        assert!(ask_members_at_state(paths.clone(), &state, &discovery, &current).is_err());
        assert!(cached.wire.lock().unwrap().is_empty());
        assert!(events.try_recv().is_err());
        assert_eq!(discovery.browses.load(Ordering::SeqCst), 0);
        assert_eq!(
            std::fs::read_to_string(&source_path).unwrap(),
            if source_replaced {
                &replaced
            } else {
                &original
            }
            .as_str()
        );
        assert!(!join::pending_proof(&paths).unwrap());
    }
}

/// The actual automatic CLI runner must enter its guarded proof before unavailable
/// DNS. Channel order proves this without making a scheduler delay a latency ceiling.
#[test]
fn automatic_fallback_proves_a_numeric_member_before_waiting_for_dns() {
    for pending in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mac = me(root.path(), "waiting");
        if pending {
            let saved = format!(
                "url = {:?}\nname = 'alab'\nsigner = '{}'\nhost = 'waiting'\n",
                aterm_secrets::ALAB_DEFAULT_URL,
                aterm_secrets::ALAB_SIGNER,
            );
            aterm_secrets::fsx::write_atomic(
                &mac.paths.dir.join("source.toml"),
                saved.as_bytes(),
                0o600,
            )
            .unwrap();
        } else {
            // This is the source-free state retained when the initial bootstrap
            // heard nobody, before a numeric member arrives in the fallback.
            client::prepare_alab_join_retry(&mac.paths).unwrap();
        }
        let original = client::config(&mac.paths).unwrap();
        let (event, events) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        let found = HeldDiscovery {
            direct: vec!["127.0.0.1:7100".into()],
            event: &event,
            release: &released,
        };
        let wire = Mutex::new(Vec::new());
        let proof = |mac: &Me, ats: &[String], _: std::time::Instant, current: &Current<'_>| {
            current().unwrap();
            assert_eq!(ats, ["127.0.0.1:7100"]);
            let expected = client::config(&mac.paths).unwrap();
            let proving = join::ProofLock::take(&mac.paths, None).unwrap().unwrap();
            let scope =
                ProofScope::read(&mac.paths, expected.as_ref(), true, current, &proving).unwrap();
            let tried = std::thread::scope(|threads| {
                let dial = |_: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
                    let (mut member, joiner) = UnixStream::pair()?;
                    for stream in [&member, &joiner] {
                        stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                        stream.set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                    }
                    let event = &event;
                    let wire = &wire;
                    threads.spawn(move || {
                        writeln!(member,
                            "ALAB member=1 key={} network={} authority={} genesis={} name=fixture allow=1 invite=0 challenge={}",
                            "09".repeat(32), "01".repeat(16), "02".repeat(32),
                            "03".repeat(32), "04".repeat(16),
                        ).unwrap();
                        let mut line = String::new();
                        std::io::BufReader::new(&mut member).read_line(&mut line).unwrap();
                        wire.lock().unwrap().push(line);
                        event.send("proof").unwrap();
                        member.write_all(b"NO why=unsupported-authority\n").unwrap();
                    });
                    Ok(Box::new(joiner))
                };
                scope.direct(
                    &mac.paths,
                    &dial,
                    &mac.joining().unwrap(),
                    &|_| [0; 64],
                    ats,
                )
            });
            proof_outcome(tried)
        };
        let reach = Reach {
            browse: &found,
            dial: &|_| {
                Err(aterm_secrets::Error::new(
                    "plain fixture ask is not answered",
                ))
            },
            prove: &proof,
            join_vault: &no_vault,
            follow: &|_| Follows::Runs,
        };
        let first = std::thread::scope(|threads| {
            let worker = threads.spawn(|| joined(&mac, &JoinArgs::default(), &reach));
            let first = events.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
            // Release before asserting so a red regression also joins every worker.
            release.send(()).unwrap();
            let _ = worker.join().unwrap();
            first
        });
        assert_eq!(first, "proof", "pending={pending}");
        let wire = wire.lock().unwrap();
        assert_eq!(wire.len(), 1);
        assert!(wire[0].starts_with("prove ") && wire[0].contains(" authority="));
        assert_eq!(client::config(&mac.paths).unwrap(), original);
        assert!(!join::pending_proof(&mac.paths).unwrap());
    }
}

struct Batches<'a> {
    direct: &'a (dyn Fn(Duration) -> aterm_secrets::Result<Vec<String>> + Sync),
    more: &'a (dyn Fn(Duration) -> aterm_secrets::Result<Vec<String>> + Sync),
}

impl MemberDiscovery for Batches<'_> {
    fn begin(&self, within: Duration) -> aterm_secrets::Result<NumericMembers> {
        (self.direct)(within).map(Into::into)
    }
    fn more(&self, within: Duration) -> aterm_secrets::Result<Vec<String>> {
        (self.more)(within)
    }
}

#[test]
fn automatic_fallback_keeps_ambiguous_proofs_terminal_and_reserves_the_dns_turn() {
    for (first, numeric) in [
        ("in", true),
        ("uncertain", true),
        ("no", true),
        ("in", false),
        ("uncertain", false),
        ("no", false),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mac = me(root.path(), "routing");
        let spent = Mutex::new(Duration::ZERO);
        let attempts = Mutex::new(Vec::new());
        let batches = Batches {
            direct: &|within| {
                assert_eq!(within, BROWSE_WITHIN);
                let began = std::time::Instant::now();
                let found = if numeric {
                    vec!["192.0.2.1:7100".to_owned()]
                } else {
                    Vec::new()
                };
                *spent.lock().unwrap() = began.elapsed();
                Ok(found)
            },
            more: &|within| {
                assert_eq!(first, "no", "proof already ended the round");
                assert!(within <= BROWSE_WITHIN.saturating_sub(*spent.lock().unwrap()));
                let mut found = vec!["member.local:7100".into(), "member.local:7100".into()];
                if numeric {
                    found.push("192.0.2.1:7100".into());
                }
                Ok(found)
            },
        };
        let proof = |_: &Me, ats: &[String], until: std::time::Instant, current: &Current<'_>| {
            current().unwrap();
            let mut attempts = attempts.lock().unwrap();
            attempts.push((ats.to_vec(), until));
            match (first, attempts.len()) {
                ("in", 1) | ("no", 2) => Proved::In,
                ("uncertain", 1) => Proved::SignedMaybe,
                ("no", 1) => Proved::No,
                _ => panic!("unexpected duplicate proof"),
            }
        };
        let reach = Reach {
            browse: &batches,
            prove: &proof,
            dial: &|_| panic!("a successful or ambiguous proof sends no plain request"),
            join_vault: &|_| panic!("a successful or ambiguous proof sends no HTTP enrollment"),
            follow: &|_| Follows::Runs,
        };
        let (code, said) = joined(&mac, &JoinArgs::default(), &reach);
        assert_eq!(code, 0, "{said}");
        let attempts = attempts.lock().unwrap();
        assert_eq!(attempts.len(), if first == "no" { 2 } else { 1 });
        assert_eq!(
            attempts[0].0,
            if numeric {
                vec!["192.0.2.1:7100"]
            } else {
                Vec::new()
            },
        );
        if first == "no" {
            assert_eq!(attempts[1].0, ["member.local:7100"]);
            assert!(
                attempts[0].1 < attempts[1].1,
                "DNS keeps the later proof deadline"
            );
            assert!(attempts[1].1 <= std::time::Instant::now() + PROVE_WITHIN);
        }
    }
}

#[test]
fn automatic_fallback_retains_one_cached_fleet_turn_when_discovery_is_empty() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for numeric in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mac = me(root.path(), "retained");
        let attempts = AtomicUsize::new(0);
        let batches = Batches {
            direct: &|_| Ok(if numeric { vec![M1.into()] } else { Vec::new() }),
            more: &|_| {
                assert!(
                    numeric,
                    "an ambiguous cached proof ends discovery before DNS"
                );
                Ok(Vec::new())
            },
        };
        let reach = Reach {
            browse: &batches,
            prove: &|_, ats, _, current| {
                current().unwrap();
                assert_eq!(
                    attempts.fetch_add(1, Ordering::SeqCst),
                    0,
                    "one retained-fleet turn"
                );
                if numeric {
                    assert_eq!(ats, [M1]);
                    Proved::No
                } else {
                    assert!(
                        ats.is_empty(),
                        "the retained fleet needs no discovered endpoint"
                    );
                    Proved::SignedMaybe
                }
            },
            dial: &|_| {
                assert!(
                    numeric,
                    "an ambiguous retained proof sends no plain request"
                );
                Err(aterm_secrets::Error::new("fixture member does not answer"))
            },
            join_vault: &|_| {
                assert!(
                    numeric,
                    "an ambiguous retained proof sends no HTTP enrollment"
                );
                no_vault(&mac.paths)
            },
            follow: &|_| Follows::Runs,
        };
        let (code, said) = joined(&mac, &JoinArgs::default(), &reach);
        assert_eq!(code, 0, "{said}");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn automatic_fallback_carries_withdrawal_through_the_possession_boundary() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for cut in ["source", "stop"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut mac = me(root.path(), "guarded");
        let custom = format!(
            "url = 'http://127.0.0.1:1/v1/custom'\nname = 'custom'\nsigner = '{}'\nhost = 'guarded'\n",
            aterm_secrets::ALAB_SIGNER
        );
        aterm_secrets::fsx::write_atomic(
            &mac.paths.dir.join("source.toml"),
            custom.as_bytes(),
            0o600,
        )
        .unwrap();
        let node = aterm_net::fleet::NodeIdentity::from_seed(&"01".repeat(16), &[7; 32]).unwrap();
        mac.node = *node.key().bytes();
        let desk = member(root.path(), "member");
        let allowed = AtomicBool::new(true);
        let decisions = AtomicUsize::new(0);
        let batches = Batches {
            direct: &|_| Ok(vec![M1.into()]),
            more: &|_| panic!("withdrawal ends discovery before DNS"),
        };
        let proof = |mac: &Me, ats: &[String], _: std::time::Instant, current: &Current<'_>| {
            let expected = client::config(&mac.paths).unwrap();
            let proving = join::ProofLock::take(&mac.paths, None).unwrap().unwrap();
            let scope =
                ProofScope::read(&mac.paths, expected.as_ref(), true, current, &proving).unwrap();
            let tried = std::thread::scope(|threads| {
                let dial = |_: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
                    let (mut member, joiner) = UnixStream::pair()?;
                    for stream in [&member, &joiner] {
                        stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                        stream.set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                    }
                    let (desk, allowed, decisions) = (&desk, &allowed, &decisions);
                    threads.spawn(move || {
                        let refuses = |_: &desk::Proving<'_>| {
                            if cut == "stop" {
                                allowed.store(false, Ordering::Release);
                            } else {
                                let chosen = format!(
                                    "url = 'http://127.0.0.1:1/v1/chosen'\nname = 'chosen'\nsigner = '{}'\nhost = 'guarded'\n",
                                    aterm_secrets::ALAB_SIGNER
                                );
                                aterm_secrets::fsx::write_atomic(&mac.paths.dir.join("source.toml"), chosen.as_bytes(), 0o600).unwrap();
                            }
                            None
                        };
                        let decide = |_: &desk::Proving<'_>| {
                            decisions.fetch_add(1, Ordering::SeqCst);
                            Err(aterm_secrets::Error::new("possession must not reach this decision"))
                        };
                        let _ = desk::serve_with(desk, &mut member, IpAddr::from([192, 0, 2, 9]), &bundle,
                            Some(desk::Proof { decide: &decide, refuses: &refuses, authority: None, fleet: None }));
                    });
                    Ok(Box::new(joiner))
                };
                scope.direct(
                    &mac.paths,
                    &dial,
                    &mac.joining().unwrap(),
                    &|message| node.sign_record(desk::PROVE_RECORD, message),
                    ats,
                )
            });
            proof_outcome(tried)
        };
        let reach = Reach {
            browse: &batches,
            prove: &proof,
            dial: &|_| panic!("withdrawal sends no plain request"),
            join_vault: &|_| panic!("withdrawal sends no HTTP enrollment"),
            follow: &|_| panic!("withdrawal starts no subscriber"),
        };
        let current = || {
            allowed
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| aterm_secrets::Error::new("Stopped"))
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            join_tried(
                &mac,
                &JoinArgs::default(),
                &reach,
                &mut out,
                &mut err,
                &current
            ),
            Tried::Ended(1)
        );
        assert_eq!(decisions.load(Ordering::Acquire), 0, "{cut}");
        assert!(!join::pending_proof(&mac.paths).unwrap());
        assert!(!mac.paths.home.join(".secrets").exists());
    }
}

#[test]
fn phrase_owner_discovery_stays_complete_and_explicit_addresses_skip_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mac = me(root.path(), "phrase");
    let desk = member(root.path(), "owner");
    let ((code, said), _) = with_member(&desk, |dial| {
        let (heard, missed) = join::ask_all(dial, &mac.joining().unwrap(), &[M1.to_owned()]);
        assert_eq!(heard.len(), 1, "{missed:?}");
        let phrase = allow(&desk);
        let batches = Batches {
            direct: &|_| Ok(vec!["another-member:7100".into()]),
            more: &|within| {
                assert!(within <= BROWSE_WITHIN);
                Ok(vec![M1.into()])
            },
        };
        let reach = Reach {
            browse: &batches,
            prove: &|_, _, _, _| panic!("a phrase is not an automatic proof"),
            dial,
            join_vault: &no_vault,
            follow: &|_| Follows::Runs,
        };
        joined(
            &mac,
            &JoinArgs {
                phrase: Some(SecretBytes::from(phrase)),
                ..JoinArgs::default()
            },
            &reach,
        )
    });
    assert_eq!(code, 0, "{said}");
    let named = me(root.path(), "named");
    let batches = Batches {
        direct: &|_| panic!("explicit --at performs no discovery"),
        more: &|_| panic!("explicit --at performs no discovery"),
    };
    let reach = Reach {
        browse: &batches,
        prove: &|_, ats, _, current| {
            current().unwrap();
            assert_eq!(ats, [M1]);
            Proved::SignedMaybe
        },
        dial: &|_| panic!("an ambiguous proof sends no plain request"),
        join_vault: &|_| Ok(()),
        follow: &|_| Follows::Runs,
    };
    assert_eq!(
        joined(
            &named,
            &JoinArgs {
                at: vec![M1.into()],
                no_wait: true,
                ..JoinArgs::default()
            },
            &reach
        )
        .0,
        0
    );
}
