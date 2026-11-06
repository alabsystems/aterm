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
    fn direct(&self, _within: Duration) -> aterm_secrets::Result<Vec<String>> {
        Ok(self.direct.clone())
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
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
    wire: std::sync::Arc<Mutex<Vec<String>>>,
}

impl CachedMember {
    fn listen(secret: &aterm_net::fleet::FleetSecret, event: mpsc::Sender<&'static str>) -> Self {
        use aterm_net::fleet::{FleetOp, FleetScope, Listening, NodeIdentity};
        use std::sync::Arc;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
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
            let running = Arc::clone(&running);
            let wire = Arc::clone(&wire);
            move || {
                aterm_net::fleet::serve_with_handshake(
                    &listener,
                    &listening,
                    &running,
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
            running,
            serving: Some(serving),
            wire,
        }
    }
}

impl Drop for CachedMember {
    fn drop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Release);
        drop(std::net::TcpStream::connect((
            std::net::Ipv4Addr::LOCALHOST,
            self.port,
        )));
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
            book.write_code(&code).unwrap();
        })
        .unwrap();
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
            let peers: Vec<_> = fleet_member::read_peers(&state)
                .unwrap()
                .into_iter()
                .filter(|peer| peer.node.is_some())
                .map(|peer| (peer.host, peer.addr, peer.port))
                .collect();
            let code = fleet_member::read_join_code(&state).unwrap();
            let secret = FleetSecret::new(&code.secret).unwrap();
            proof_outcome(crate::vault_join::prove_over_fleet_admitted(
                &mac.paths,
                &mac.joining().unwrap(),
                (&secret, &node),
                &peers,
                Some(until),
                Some(&scope.admission),
            ))
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
    fn direct(&self, within: Duration) -> aterm_secrets::Result<Vec<String>> {
        (self.direct)(within)
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
