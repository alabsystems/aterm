// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::members::desk::{self, Desk, Local};
use crate::members::join::{self, ProofAdmission, Stream};
use crate::members::joiner::Joining;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CompiledLineage(Option<crate::envelope::Retired>);

impl Drop for CompiledLineage {
    fn drop(&mut self) {
        crate::envelope::TEST_RETIRED.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

struct Rig {
    macs: TwoMacs,
    fresh: Paths,
    device: crate::commons::bootstrap::Device,
    pair: Ed25519KeyPair,
    node: [u8; 32],
    proving: join::ProofLock,
}

impl Rig {
    fn new() -> Self {
        let macs = two_macs(&[(Audience::Everyone, "shared", b"fixture-only".as_slice())]);
        let pair = node_pair(8);
        let node = node(8);
        publish_local_node_key(&macs.m7, node, |network, actor| {
            pair.sign(&node_proof_message(network, actor))
                .as_ref()
                .try_into()
                .unwrap()
        })
        .unwrap()
        .expect("published node key");
        post_records_to_servers(&macs.m7).unwrap();
        sync_records(&macs.m22, None).unwrap();
        let fresh = Paths::for_home(&macs.root.path().join("fresh"));
        machine_identity(&fresh).unwrap();
        let device = crate::commons::bootstrap::Device::open(&fresh)
            .unwrap()
            .unwrap();
        let proving = join::ProofLock::take(&fresh, None).unwrap().unwrap();
        Self {
            macs,
            fresh,
            device,
            pair,
            node,
            proving,
        }
    }

    fn prove(
        &self,
        admission: &ProofAdmission<'_>,
        handing: &(dyn Fn(&mut crate::members::Bundle) + Sync),
    ) -> (std::result::Result<join::Joined, join::Missed>, usize) {
        self.prove_serving(admission, handing, true)
    }

    fn prove_serving(
        &self,
        admission: &ProofAdmission<'_>,
        handing: &(dyn Fn(&mut crate::members::Bundle) + Sync),
        authority_supported: bool,
    ) -> (std::result::Result<join::Joined, join::Missed>, usize) {
        self.prove_witness(admission, handing, authority_supported, &|_| {}, false)
    }

    fn prove_witness(
        &self,
        admission: &ProofAdmission<'_>,
        handing: &(dyn Fn(&mut crate::members::Bundle) + Sync),
        authority_supported: bool,
        alter: &(dyn Fn(&mut Vec<u8>) + Sync),
        via_relay: bool,
    ) -> (std::result::Result<join::Joined, join::Missed>, usize) {
        let desk = std::sync::Mutex::new(Desk::new(Local::of(&self.macs.m22).unwrap().unwrap()));
        let joining = Joining {
            device: &self.device,
            records_bound: crate::crossing::RecordsBound::uncrossed(),
            node: self.node,
            name: "m7".into(),
            model: "fixture".into(),
        };
        let prefix = format!("aterm-fleet/v3 record {}\0", desk::PROVE_RECORD);
        let sign = |message: &[u8]| {
            self.pair
                .sign(&[prefix.as_bytes(), message].concat())
                .as_ref()
                .try_into()
                .unwrap()
        };
        let dials = AtomicUsize::new(0);
        let later = aterm_time::unix_millis().unwrap() + DARK_FOR_MS + 1;
        let decide = |proving: &desk::Proving<'_>| {
            let bytes = prove_at_ms(&self.macs.m22, proving, CURRENT, later)?;
            let mut bundle = crate::members::Bundle::decode(bytes.as_slice())?;
            handing(&mut bundle);
            bundle.encode()
        };
        let authority = |proving: &desk::Proving<'_>| {
            let mut records = member_authority(&self.macs.m22, proving)?;
            alter(&mut records);
            Ok(records)
        };
        let tried = std::thread::scope(|scope| {
            let dial = |_: &str| -> crate::Result<Box<dyn Stream>> {
                dials.fetch_add(1, Ordering::SeqCst);
                let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
                let ours = std::net::TcpStream::connect(listener.local_addr()?)?;
                let (mut theirs, _) = listener.accept()?;
                for stream in [&ours, &theirs] {
                    stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                    stream.set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                }
                let (desk, decide, authority) = (&desk, &decide, &authority);
                scope.spawn(move || {
                    drop(desk::serve_with(
                        desk,
                        &mut theirs,
                        std::net::IpAddr::from([192, 0, 2, 22]),
                        &|_| Err(Error::new("no phrase runs here")),
                        Some(desk::Proof {
                            decide,
                            refuses: &|_| None,
                            authority: authority_supported
                                .then_some(authority as desk::ProofAuthority<'_>),
                            fleet: None,
                        }),
                    ));
                });
                if via_relay {
                    let (client, mut proxy) = std::os::unix::net::UnixStream::pair()?;
                    for stream in [&client, &proxy] {
                        stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                        stream.set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                    }
                    scope.spawn(move || {
                        let mut upstream = ours;
                        crate::members::relay(&mut proxy, &mut upstream).unwrap();
                    });
                    Ok(Box::new(client))
                } else {
                    Ok(Box::new(ours))
                }
            };
            join::prove_first_admitted(
                &self.fresh,
                &dial,
                &joining,
                &sign,
                &["a-member:7".into(), "b-member:7".into()],
                admission,
            )
        });
        (tried, dials.load(Ordering::SeqCst))
    }

    fn compiled_lineage(&self) -> CompiledLineage {
        CompiledLineage(crate::envelope::TEST_RETIRED.with(|slot| {
            slot.replace(Some(crate::envelope::Retired {
                keys: Vec::new(),
                current: config(&self.macs.m22).unwrap().unwrap().signer,
            }))
        }))
    }

    fn unconfigured(&self) {
        assert!(config(&self.fresh).unwrap().is_none());
        assert!(!self.fresh.state_file().exists());
        assert!(!self.fresh.home.join(".secrets").exists());
        assert_eq!(
            status(&self.fresh).unwrap().membership,
            Membership::Unconfigured
        );
    }
}

#[test]
fn a_cold_default_proof_rejects_another_valid_vault_before_writing_any_source_or_files() {
    let _answering = ServersAnswering::set();
    let rig = Rig::new();
    let before = std::fs::read(rig.fresh.identity_file()).unwrap();
    // A hint can lead to a real, correctly signed member of another vault. The public
    // witness fails before possession, so another candidate or the pinned fallback may go.
    let (tried, dials) = rig.prove(
        &ProofAdmission {
            proving: &rig.proving,
            expected: None,
            default_alab: true,
            current: &|| Ok(()),
        },
        &|_| panic!("a foreign witness must receive no possession"),
    );
    assert!(!tried.unwrap_err().signed_maybe);
    assert_eq!(dials, 2);
    rig.unconfigured();
    assert!(!join::pending_proof(&rig.fresh).unwrap());
    assert_eq!(std::fs::read(rig.fresh.identity_file()).unwrap(), before);
    // The bundle is valid: the same explicit nondefault admission can take it.
    let (joined, dials) = rig.prove(
        &ProofAdmission {
            proving: &rig.proving,
            expected: None,
            default_alab: false,
            current: &|| Ok(()),
        },
        &|_| {},
    );
    assert!(joined.is_ok(), "{joined:?}");
    assert_eq!(dials, 1);
    assert!(materialization_ready_for_setup(&rig.fresh).unwrap());
}

#[test]
fn an_unsupported_member_is_definite_no_before_possession_or_durable_intent() {
    let _answering = ServersAnswering::set();
    let rig = Rig::new();
    let (tried, dials) = rig.prove_serving(
        &ProofAdmission {
            proving: &rig.proving,
            expected: None,
            default_alab: true,
            current: &|| Ok(()),
        },
        &|_| panic!("unsupported authority must receive no possession"),
        false,
    );
    assert!(!tried.unwrap_err().signed_maybe);
    assert_eq!(dials, 2);
    assert!(!join::pending_proof(&rig.fresh).unwrap());
    rig.unconfigured();
}

#[test]
fn a_retained_proof_stays_ambiguous_when_its_member_cannot_be_reached() {
    let _answering = ServersAnswering::set();
    let rig = Rig::new();
    let store = load_acknowledged_records(&rig.macs.m22).unwrap();
    let crate::membership_log::Body::Genesis { root, .. } = store.log().unwrap().genesis().body()
    else {
        panic!("genesis fixture");
    };
    let request = rig.device.request(root.network, root.authority).unwrap();
    let joining = Joining {
        device: &rig.device,
        records_bound: crate::crossing::RecordsBound::uncrossed(),
        node: rig.node,
        name: "m7".into(),
        model: "fixture".into(),
    };
    crate::members::proof_retry::sending(
        &rig.fresh,
        &rig.proving,
        &joining,
        "old-member:7",
        [7; 32],
        request.binding(),
        0,
    )
    .unwrap();
    let dials = AtomicUsize::new(0);
    let missed = join::prove_first_admitted(
        &rig.fresh,
        &|_| {
            dials.fetch_add(1, Ordering::SeqCst);
            Err(Error::new("member unavailable"))
        },
        &joining,
        &|_| panic!("offline member cannot ask for a signature"),
        &["new-address:7".into()],
        &ProofAdmission {
            proving: &rig.proving,
            expected: None,
            default_alab: true,
            current: &|| Ok(()),
        },
    )
    .unwrap_err();
    assert!(missed.signed_maybe);
    assert_eq!(dials.load(Ordering::SeqCst), 2);
    assert!(join::pending_proof(&rig.fresh).unwrap());
    rig.unconfigured();
}

#[test]
fn a_same_authority_custom_source_chosen_during_proof_is_never_replaced() {
    let _answering = ServersAnswering::set();
    let rig = Rig::new();
    let mut chosen = config(&rig.macs.m22).unwrap().unwrap();
    chosen.source = Source::Url("http://127.0.0.1:1/v1/alab?chosen=1".into());
    chosen.host = "my-own-name".into();
    let (tried, dials) = rig.prove(
        &ProofAdmission {
            proving: &rig.proving,
            expected: None,
            default_alab: false,
            current: &|| Ok(()),
        },
        &|_| save_config(&rig.fresh, &chosen).unwrap(),
    );
    let missed = tried.unwrap_err();
    assert!(missed.signed_maybe);
    assert_eq!(dials, 1);
    assert_eq!(config(&rig.fresh).unwrap(), Some(chosen));
    assert!(!rig.fresh.state_file().exists());
    assert!(!rig.fresh.home.join(".secrets").exists());
}

#[test]
fn withdrawn_authority_blocks_both_the_first_dial_and_the_final_publication() {
    let _answering = ServersAnswering::set();
    let rig = Rig::new();
    let (tried, dials) = rig.prove(
        &ProofAdmission {
            proving: &rig.proving,
            expected: None,
            default_alab: false,
            current: &|| Err(Error::new("setup was stopped")),
        },
        &|_| panic!("no proof after Stop"),
    );
    assert!(!tried.unwrap_err().signed_maybe);
    assert_eq!(dials, 0);
    rig.unconfigured();

    // Dial, locked validation, final publication: the last boundary still observes Stop
    // after a member's authenticated bundle has already passed its local checks.
    let reads = AtomicUsize::new(0);
    let current = || {
        let at = reads.fetch_add(1, Ordering::SeqCst);
        if at > 1 {
            assert!(StateLock::observe(&rig.fresh).unwrap().is_none());
        }
        match at {
            0..=2 => Ok(()),
            _ => Err(Error::new("setup was stopped before publication")),
        }
    };
    let (tried, dials) = rig.prove(
        &ProofAdmission {
            proving: &rig.proving,
            expected: None,
            default_alab: false,
            current: &current,
        },
        &|_| {},
    );
    assert!(tried.unwrap_err().signed_maybe);
    assert_eq!(dials, 1);
    assert_eq!(reads.load(Ordering::SeqCst), 4);
    rig.unconfigured();
}

#[test]
fn an_admitted_default_uses_the_compiled_lineage_and_checked_genesis_path() {
    let _answering = ServersAnswering::set();
    let rig = Rig::new();
    // Unit tests compile no live genesis. The existing compiled-lineage seam points this
    // thread's dormant ALab store at the scratch vault's real signer instead.
    let _compiled = rig.compiled_lineage();
    let (joined, dials) = rig.prove(
        &ProofAdmission {
            proving: &rig.proving,
            expected: None,
            default_alab: true,
            current: &|| Ok(()),
        },
        &|_| {},
    );
    assert!(joined.is_ok(), "{joined:?}");
    assert_eq!(dials, 1);
    assert!(alab_vault(&config(&rig.fresh).unwrap().unwrap()));
    assert!(materialization_ready_for_setup(&rig.fresh).unwrap());
}

#[test]
fn a_withdrawn_admission_publishes_no_crossed_custody_mark() {
    let _answering = ServersAnswering::set();
    let rig = Rig::new();
    let crossed = |bundle: &mut crate::members::Bundle| {
        let crate::members::bundle::Handed::Log { source, .. } = &mut bundle.handed else {
            panic!("log fixture");
        };
        *source = Some(crate::crossing::RecordsBound::here(true));
    };
    let checks = AtomicUsize::new(0);
    let current = || {
        if checks.fetch_add(1, Ordering::SeqCst) < 3 {
            Ok(())
        } else {
            Err(Error::new("setup changed before crossed custody"))
        }
    };
    let (tried, dials) = rig.prove(
        &ProofAdmission {
            proving: &rig.proving,
            expected: None,
            default_alab: false,
            current: &current,
        },
        &crossed,
    );
    assert!(tried.unwrap_err().signed_maybe);
    assert_eq!(dials, 1);
    assert_eq!(checks.load(Ordering::SeqCst), 4);
    assert!(!crossing_mark(&rig.fresh).path().exists());
    rig.unconfigured();

    // The same valid bundle really requires the irreversible mark once admitted.
    let (joined, _) = rig.prove(
        &ProofAdmission {
            proving: &rig.proving,
            expected: None,
            default_alab: false,
            current: &|| Ok(()),
        },
        &crossed,
    );
    assert!(joined.is_ok(), "{joined:?}");
    assert!(crossing_mark(&rig.fresh).path().exists());
}

#[test]
fn authenticated_witnesses_must_be_complete_without_duplicate_or_foreign_frames() {
    use crate::membership_log::{Body, Store, decode_records, encode_records};
    let _answering = ServersAnswering::set();
    let rig = Rig::new();
    let other = Rig::new();
    let foreign = load_acknowledged_records(&other.macs.m22)
        .unwrap()
        .log()
        .unwrap()
        .genesis()
        .bytes()
        .to_vec();
    let _compiled = rig.compiled_lineage();
    let duplicate = |bytes: &mut Vec<u8>| {
        let decoded = decode_records(bytes).unwrap();
        bytes.extend_from_slice(decoded[0].bytes());
    };
    let missing_parent = |bytes: &mut Vec<u8>| {
        let decoded = decode_records(bytes).unwrap();
        let omitted = decoded
            .iter()
            .find(|candidate| {
                !matches!(candidate.body(), Body::Genesis { .. })
                    && decoded.iter().any(|r| r.parents().contains(candidate.id()))
            })
            .expect("the complete fixture has a non-genesis parent")
            .id();
        *bytes = encode_records(decoded.iter().filter(|record| record.id() != omitted));
    };
    let mixed = |bytes: &mut Vec<u8>| bytes.extend_from_slice(&foreign);
    for alter in [
        &duplicate as &(dyn Fn(&mut Vec<u8>) + Sync),
        &missing_parent,
        &mixed,
    ] {
        // The ordinary accumulating store deliberately reduces these inputs to a usable
        // rooted projection. That projection is insufficient as a whole authority witness.
        let mut preview = load_acknowledged_records(&rig.macs.m22).unwrap().encode();
        alter(&mut preview);
        let mut projected = Store::for_vault(crate::ALAB_VAULT).unwrap();
        projected.ingest(&preview).unwrap();
        assert!(projected.log().is_some());
        assert!(!projected.log().unwrap().membership().members().is_empty());
        let (tried, _) = rig.prove_witness(
            &ProofAdmission {
                proving: &rig.proving,
                expected: None,
                default_alab: true,
                current: &|| Ok(()),
            },
            &|_| panic!("an incomplete authority must receive no possession"),
            true,
            alter,
            false,
        );
        assert!(!tried.unwrap_err().signed_maybe);
        assert!(!join::pending_proof(&rig.fresh).unwrap());
        rig.unconfigured();
    }
    let (valid, _) = rig.prove(
        &ProofAdmission {
            proving: &rig.proving,
            expected: None,
            default_alab: true,
            current: &|| Ok(()),
        },
        &|_| {},
    );
    assert!(valid.is_ok(), "{valid:?}");
}

#[test]
fn a_default_authority_exchange_crosses_the_node_to_window_relay() {
    let _answering = ServersAnswering::set();
    let rig = Rig::new();
    let _compiled = rig.compiled_lineage();
    let (joined, dials) = rig.prove_witness(
        &ProofAdmission {
            proving: &rig.proving,
            expected: None,
            default_alab: true,
            current: &|| Ok(()),
        },
        &|_| {},
        true,
        &|_| {},
        true,
    );
    assert!(joined.is_ok(), "{joined:?}");
    assert_eq!(dials, 1);
    assert!(materialization_ready_for_setup(&rig.fresh).unwrap());
    assert!(!join::pending_proof(&rig.fresh).unwrap());
}
