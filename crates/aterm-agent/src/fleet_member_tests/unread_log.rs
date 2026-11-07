// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admission while this Mac's vault exists but cannot be read now (`crate::fleet_log`): the
//! vault denies `m8` (an `aterm-swarm-roster` `deny_labels` row) and revoked `m7` (retired in
//! the membership log before its vault row is removed); `m9` is the control the vault
//! still admits. This Mac's book binds all three and an earlier `aterm fleet allow drive --from`
//! granted each under its key, so the book alone would admit every one. The service activation's
//! membership frontier (`crate::service_activation`) is read from the same store.

use super::*;
use crate::fleet_log::{LocalLog, ensure_local_node_key_in_log};
use crate::fleet_vault::Vault;
use aterm_secrets::client::{self, Paths};
use aterm_secrets::membership_log::RecordBudgets;

const NOW_MS: u64 = 1_760_000_000_000;

/// This Mac's own registry row, registered and adopted in `lab`'s vault, labelled `label`: the
/// registry and its row's key (this Mac's MACHINE_FACTS row is the only one the fixture can
/// give, since a facts row counts only where its own Mac signed it).
fn label_this_mac(lab: &Lab, label: &str) -> (aterm_db::alab::Registry, String) {
    use aterm_db::alab::register::{Outcome, Probe};
    let registry = aterm_db::alab::Registry::at(lab.paths.clone());
    registry.open().unwrap();
    let principal = member::node_principal(own_node().key());
    let probe = Probe {
        hw_model: "Mac16,8".into(),
        hosts: vec!["m42-studio".into()],
        node: Some(principal[2..].to_owned()),
        platform_uuid: "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43".into(),
        ..Probe::default()
    };
    let pass = registry
        .register(&lab.places.config, &probe, true, NOW_MS)
        .unwrap();
    let Outcome::Adopted { mac, .. } = pass.outcome else {
        panic!("expected adoption of the migrated historical row")
    };
    relabel(&registry, &mac, label);
    (registry, mac)
}

/// Write `label` into the registry row `mac`.
fn relabel(registry: &aterm_db::alab::Registry, mac: &str, label: &str) {
    use aterm_db::{
        alab::registry::MACHINE,
        typed::cell::{self, CellKey, CellValue},
    };
    registry
        .write(vec![vec![
            cell::change(
                &MACHINE,
                CellKey {
                    pk: mac,
                    col: "label",
                    elem: None,
                },
                &CellValue::owner(MACHINE.version, aterm_json::Value::from(label)),
            )
            .unwrap(),
        ]])
        .unwrap();
}

#[test]
fn registry_labels_override_a_stale_local_name_and_resolve_both_node_ids() {
    let lab = Lab::new("registry-label-authority");
    let own = own_node();
    let principal = member::node_principal(own.key());
    let (registry, mac) = label_this_mac(&lab, "m42");
    write_private(&lab.places.state, member::NAME_FILE, b"m22\n").unwrap();
    assert_eq!(
        names_this_machine(&lab.places, "M42").unwrap(),
        Some(own.node().to_owned())
    );
    assert!(
        names_this_machine(&lab.places, "m22").is_err(),
        "a stale primary or roster label cannot bypass the registry"
    );
    relabel(&registry, &mac, "m42-1234");
    assert_eq!(
        names_this_machine(&lab.places, "M42-1234").unwrap(),
        Some(own.node().to_owned())
    );
    assert!(names_this_machine(&lab.places, "m42").is_err());
    let derived = registry_target(&lab.places, &principal).unwrap().unwrap();
    let old = registry_target(&lab.places, own.node()).unwrap().unwrap();
    assert_eq!(derived.actor, old.actor);
    assert_eq!(derived.transport, own.node());
    assert_eq!(derived.node, principal);
}

/// A person's `remove <word>` as `aterm members remove` and `aterm fleet remove` run it, with
/// `lab`'s places for B2's resolver (`removal_seat_at`), never this account's.
fn remove_by_person(lab: &Lab, word: &str) -> Result<Vec<String>, String> {
    let words = ["remove".to_owned(), word.to_owned()];
    let Ok(crate::members_cli::LabVerb::Remove(args)) =
        crate::members_cli::parse_lab(crate::members_cli::Front::Members, &words)
    else {
        panic!("remove {word} is a removal");
    };
    crate::members_cli::remove_said(&lab.paths, &args, &|word| {
        removal_seat_at(&lab.places, &lab.paths, word)
    })
}

/// Every record `lab`'s vault holds.
fn held(lab: &Lab) -> std::collections::BTreeSet<aterm_secrets::membership_log::RecordId> {
    client::load_records(&lab.paths).unwrap().inventory()
}

/// The member `lab`'s log seats under `label` now.
fn standing(lab: &Lab, label: &str) -> Option<aterm_secrets::membership_log::MemberId> {
    let store = client::load_records(&lab.paths).unwrap();
    let membership = store.log().unwrap().membership();
    membership
        .members()
        .iter()
        .find(|(_, member)| member.label == label)
        .map(|(actor, _)| *actor)
}

/// Whether `lab`'s log removed `actor`.
fn removed(lab: &Lab, actor: &aterm_secrets::membership_log::MemberId) -> bool {
    let store = client::load_records(&lab.paths).unwrap();
    store.log().unwrap().membership().removal(actor).is_some()
}

/// B12: a person's removal by node id takes out the seat B2's resolver names, and names the log
/// name and node id it acted on; a second removal by that node id is refused (no standing seat
/// binds it now) and signs nothing.
///
/// NEGATIVE CONTROL: make `removal_seat_at` return `Ok(None)` at once: the node id goes to the
/// word path, which names no Mac by it, and the removal's `expect` fails.
#[test]
fn a_persons_removal_by_node_id_removes_the_seat_the_resolver_names() {
    let lab = Lab::new("remove-by-node-id");
    let m9 = standing(&lab, "m9").expect("CONTROL: m9 stands");
    let node = member::node_principal(lab.m9.key());
    let said = remove_by_person(&lab, &node).expect("m22 removes m9 by its node id");
    assert!(said[0].starts_with("removed m9: "), "{said:?}");
    assert!(
        said.iter()
            .any(|line| line.starts_with("m9 was ") && line.ends_with(&format!("node {node}"))),
        "{said:?}"
    );
    assert!(removed(&lab, &m9), "m9's seat is removed");

    let before = held(&lab);
    let refused = remove_by_person(&lab, &node).expect_err("no standing seat binds it now");
    assert!(refused.contains("no unique standing seat"), "{refused}");
    assert_eq!(held(&lab), before, "a refused removal signs nothing");
}

/// B12, the discriminating control: where the registry calls this Mac `m9` while the log's `m9`
/// is another Mac, a person's `remove m9` takes the registry's seat, this Mac's own, and refuses
/// (a Mac does not remove itself), signing nothing; resolving the word in secrets would remove
/// the log's `m9`, the hazard B2 closed.
///
/// NEGATIVE CONTROL: make `removal_seat_at` return `Ok(None)` at once: `m9` goes to the word
/// path, which removes the log's m9, and the refusal's `expect_err` fails.
#[test]
fn a_registry_label_removes_the_registrys_seat_never_the_logs_label() {
    let lab = Lab::new("remove-by-registry-label");
    let m9 = standing(&lab, "m9").expect("CONTROL: the log's m9 stands");
    let _ = label_this_mac(&lab, "m9");
    let seat = registry_target(&lab.places, "m9")
        .unwrap()
        .expect("CONTROL: the registry names a seat m9");
    assert_ne!(
        seat.actor, m9,
        "CONTROL: the registry's m9 is not the log's"
    );
    assert_eq!(seat.node, member::node_principal(own_node().key()));

    let before = held(&lab);
    let refused = remove_by_person(&lab, "m9").expect_err("the registry's m9 is this Mac");
    assert!(refused.contains("does not remove itself"), "{refused}");
    assert_eq!(held(&lab), before, "a refused removal signs nothing");
    assert_eq!(standing(&lab, "m9"), Some(m9), "the log's m9 stays in");
}

struct Lab {
    root: aterm_tempfile::TempDir,
    running: Option<aterm_secrets::server::Running>,
    home: PathBuf,
    paths: Paths,
    places: Places,
    m7: NodeIdentity,
    m8: NodeIdentity,
    m9: NodeIdentity,
    m9_paths: Paths,
    /// `records`, `vault` and `state.toml` while m7, m8 and m9 were all admitted.
    admitted: Vec<(PathBuf, Vec<u8>)>,
}

impl Drop for Lab {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop();
        }
    }
}

/// Let the Mac `label` (its home under `root`) into the log of the vault `admin` writes, its node
/// `node` (`aterm members add` by its phrase), and post the records: its vault paths.
fn admit(admin: &Paths, root: &Path, label: &str, node: &NodeIdentity) -> Paths {
    let member_paths = Paths::for_home(&root.join(format!("{label}-home")));
    let _ = client::machine_identity(&member_paths).unwrap();
    let device = aterm_secrets::commons::bootstrap::Device::open(&member_paths)
        .unwrap()
        .unwrap();
    let store = client::load_records(admin).unwrap();
    let aterm_secrets::membership_log::Body::Genesis { root: log_root, .. } =
        store.log().unwrap().genesis().body()
    else {
        panic!("expected Genesis");
    };
    let request = device
        .request(log_root.network, log_root.authority)
        .unwrap();
    let admitted =
        client::admit_by_phrase(admin, &request, *node.key().bytes(), label, 1_000).unwrap();
    assert!(matches!(admitted, client::ByLog::Admitted(_)));
    client::post_records_to_servers(admin).unwrap();
    member_paths
}

/// This Mac's own node: the founder's, bound by the log once [`Lab::new`] returns.
fn own_node() -> NodeIdentity {
    NodeIdentity::from_seed(&"a1".repeat(16), &[0x11; 32]).unwrap()
}

impl Lab {
    fn new(tag: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-unread-log-{tag}-"))
            .tempdir()
            .unwrap();
        let socket = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let running = aterm_secrets::server::start(
            socket,
            &aterm_secrets::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: root.path().join("server"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(50),
                keep_awake: false,
            },
        )
        .unwrap();
        let url = format!("http://{}/v1/test", running.addr);
        let home = root.path().join("m22-home");
        let paths = Paths::for_home(&home);
        let places = Places {
            state: home.join("state"),
            config: home.join("cfg/aterm/aterm.toml"),
        };
        let created = client::create(&paths, &url, None, Some("m22")).unwrap();
        client::admin_seed_swarm_roster(&paths, &[]).unwrap();
        assert!(
            LocalLog::of(&paths).keys().is_none(),
            "a vault with no genesis yet has no verdict: admission falls back to the book"
        );
        client::seed_vault_genesis(&paths, &root.path().join("test.genesis")).unwrap();
        client::admin_mirror_log(&paths, &[]).unwrap();
        let keys = LocalLog::of(&paths).keys();
        assert!(ensure_local_node_key_in_log(
            &paths,
            &own_node(),
            keys.as_deref()
        ));
        assert_eq!(vault_paths_for(&places).unwrap().dir, paths.dir);

        let m7 = NodeIdentity::from_seed(&"b2".repeat(16), &[0x33; 32]).unwrap();
        let m8 = NodeIdentity::from_seed(&"d4".repeat(16), &[0x66; 32]).unwrap();
        let m9 = NodeIdentity::from_seed(&"e6".repeat(16), &[0x99; 32]).unwrap();
        let _ = admit(&paths, root.path(), "m7", &m7);
        let _ = admit(&paths, root.path(), "m8", &m8);
        let m9_paths = admit(&paths, root.path(), "m9", &m9);
        client::configure_log_member(&m9_paths, &format!("{url}#{}", created.signer), "m9")
            .unwrap();
        assert!(
            LocalLog::of(&m9_paths).keys().is_none(),
            "a member before its first sync has no verdict: admission falls back to the book"
        );
        client::sync_records(&m9_paths, None).unwrap();
        assert!(
            LocalLog::of(&m9_paths)
                .keys()
                .is_some_and(|keys| keys.unread().is_none()),
            "a member's first sync reads its log"
        );

        member::ensure_private_dir(&places.state).unwrap();
        let own = own_node();
        write_private(
            &places.state,
            member::NODE_FILE,
            format!("{}\n", own.node()).as_bytes(),
        )
        .unwrap();
        write_private(
            &places.state,
            member::NODE_KEY_FILE,
            format!("{}\n", aterm_codec::hex::encode_lower(&[0x11; 32])).as_bytes(),
        )
        .unwrap();
        let book: String = [(&m7, "m7"), (&m8, "m8"), (&m9, "m9")]
            .iter()
            .map(|(id, label)| {
                let host = format!("{label}.local");
                format!(
                    "{}\n",
                    Peer::own(id, (host.as_str(), 7100, "0.146.0"), NOW_MS).line()
                )
            })
            .collect();
        write_private(&places.state, PEERS_FILE, book.as_bytes()).unwrap();
        aterm_net::fleet_member::store_pushed_facts_with_key(
            &places.state,
            m9.node(),
            Some(m9.key()),
            "OK 0\n",
        )
        .unwrap();
        let grants: Vec<String> = [&m7, &m8, &m9]
            .iter()
            .map(|id| format!("\"{}:{}\"", id.node(), id.key().hex()))
            .collect();
        std::fs::create_dir_all(places.config.parent().unwrap()).unwrap();
        crate::test_config::write(
            &places.config,
            format!(
                "[fleet]\nallow = \"drive\"\ndrive_from = [{}]\n",
                grants.join(", ")
            ),
        )
        .unwrap();

        let admitted = Self::snapshot_of(&paths);

        // m8: roster deny.
        let admin = client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        let mut roster = aterm_secrets::swarm_roster::SwarmRoster::from_document(&doc)
            .unwrap()
            .unwrap_or_default();
        roster.deny_labels.push("m8".to_owned());
        doc.set_entry(
            &aterm_secrets::text::Audience::Everyone,
            aterm_secrets::SWARM_ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .unwrap();
        client::admin_publish(&paths, &admin, &doc).unwrap();
        // Changing the control entry's audience does not revoke membership.
        // The later explicit revocation removes m7 while m9 remains admitted.
        let admin = client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        let ctrl = doc
            .entries()
            .into_iter()
            .find(|e| e.name == aterm_secrets::commons::ENTRY)
            .expect("commons control entry");
        doc.unset_entry(&ctrl.audience, aterm_secrets::commons::ENTRY)
            .unwrap();
        doc.set_entry(
            &aterm_secrets::text::Audience::Hosts(vec!["m22".to_owned()]),
            aterm_secrets::commons::ENTRY,
            ctrl.value.as_slice(),
        )
        .unwrap();
        client::admin_publish(&paths, &admin, &doc).unwrap();
        let admin = client::admin_open(&paths).unwrap();
        let revoked = client::admin_revoke(&paths, &admin, "m7", false).unwrap();
        assert!(revoked.retirement.is_some_and(|retired| retired.changed));

        Self {
            root,
            running: Some(running),
            home,
            paths,
            places,
            m7,
            m8,
            m9,
            m9_paths,
            admitted,
        }
    }

    fn snapshot_of(paths: &Paths) -> Vec<(PathBuf, Vec<u8>)> {
        [
            paths.records_file(),
            paths.dir.join("vault"),
            paths.dir.join("state.toml"),
        ]
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
    }

    fn put_back(snapshot: &[(PathBuf, Vec<u8>)]) {
        for (path, bytes) in snapshot {
            aterm_secrets::fsx::write_atomic(path, bytes, 0o600).unwrap();
        }
    }

    /// Cross into the fenced records layout (a directory holding `current`, its pin beside it).
    /// This Mac's node key rotates on the way, as the write that fences it.
    fn fence(&self) {
        aterm_secrets::fsx::write_atomic(
            &self.paths.records_file().with_extension("crossed"),
            b"crossed\n",
            0o600,
        )
        .unwrap();
        let keys = LocalLog::of(&self.paths).keys();
        let rotated = NodeIdentity::from_seed(&"a1".repeat(16), &[0x12; 32]).unwrap();
        assert!(ensure_local_node_key_in_log(
            &self.paths,
            &rotated,
            keys.as_deref()
        ));
        write_private(
            &self.places.state,
            member::NODE_KEY_FILE,
            format!("{}\n", aterm_codec::hex::encode_lower(&[0x12; 32])).as_bytes(),
        )
        .unwrap();
        assert!(self.paths.records_file().is_dir(), "the layout is fenced");
    }

    fn pin(&self) -> PathBuf {
        self.paths.dir.join("records.layout")
    }

    fn aside(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn members(&self) -> [&NodeIdentity; 3] {
        [&self.m7, &self.m8, &self.m9]
    }

    /// The census's inputs, after a census refreshed and one served from the cache it wrote:
    /// each reads the vault and the log, and each prunes the stored survey facts they hold out.
    /// An unread roster defers the pass without replacing the cached census.
    fn census(&self) -> Result<crate::fleet_census::CensusInputs, String> {
        let now_s = NOW_MS / 1000;
        crate::fleet_census::refresh_local_census(
            &self.places.state,
            Some(&self.home),
            Vec::new(),
            now_s,
        )?;
        crate::fleet_census::read_or_compute_census(
            &self.places.state,
            Some(&self.home),
            false,
            now_s,
        )?;
        crate::fleet_census::collect_local_census_inputs(
            &self.places.state,
            Some(&self.home),
            Vec::new(),
            now_s,
        )
    }

    fn assert_m9_facts_kept(&self, when: &str) {
        assert!(
            aterm_net::fleet_member::read_pushed_facts(&self.places.state)
                .iter()
                .any(|(node, _)| node == self.m9.node()),
            "{when}: m9's stored survey facts were removed"
        );
    }

    /// The control m9's stored survey facts are still kept, and the census denies it nothing.
    fn assert_m9_kept(&self, when: &str, census: &crate::fleet_census::CensusInputs) {
        self.assert_m9_facts_kept(when);
        assert!(
            !aterm_net::fleet::is_denied_node(self.m9.node(), &census.roster.deny_nodes)
                && !aterm_net::fleet::is_denied_key(&self.m9.key(), &census.roster.deny_nodes),
            "{when}: the census denies m9: {:?}",
            census.roster.deny_nodes
        );
    }

    /// Whether the store's own reader reads it now, as authority reads it: what each scenario
    /// must break.
    fn store_reads(&self) -> bool {
        client::load_acknowledged_records(&self.paths).is_ok_and(|store| {
            store
                .log()
                .is_none_or(|log| client::local_swarm_roster_for_log(&self.paths, log).is_ok())
        })
    }

    /// With the vault read: the log holds m7 and m8 out, and the book admits the control m9.
    fn assert_read(&self, when: &str) {
        assert!(self.store_reads(), "{when}: the store reads");
        let keys = LocalLog::of(&self.paths)
            .keys()
            .unwrap_or_else(|| panic!("{when}: the log reads"));
        assert!(keys.holds_out(&self.m7.key()), "{when}: m7");
        assert!(keys.holds_out(&self.m8.key()), "{when}: m8");
        assert!(keys.admits(&self.m9.key()), "{when}: m9");
        assert_eq!(
            [&self.m7, &self.m8].map(|id| drives_here(&self.places, id.node())),
            [Some(false); 2],
            "{when}: the vault's verdict refuses its grants"
        );
        assert_eq!(
            drives_here(&self.places, self.m9.node()),
            Some(true),
            "{when}: the control drives"
        );
        assert_eq!(
            driving(&self.places),
            Some(vec![self.m9.node().to_owned()]),
            "{when}"
        );
        let (denied, log) = denied_and_log(&self.places);
        assert!(denied.is_some() && log.is_some(), "{when}: both read");
        let census = self.census().unwrap();
        let rows: Vec<&str> = census
            .fleet_book
            .iter()
            .filter_map(|p| p.node.as_deref())
            .collect();
        assert_eq!(rows, [self.m9.node()], "{when}: census rows");
        self.assert_m9_kept(when, &census);
    }

    /// With the vault unreadable now: no node gets in that the book alone would admit, the
    /// control included, and this Mac does not count itself out.
    fn assert_unread(&self, when: &str) {
        assert!(
            !self.store_reads(),
            "PRECONDITION {when}: the store's read fails"
        );
        let keys = LocalLog::of(&self.paths)
            .keys()
            .unwrap_or_else(|| panic!("{when}: an unreadable vault is no missing log"));
        for id in self.members() {
            assert!(
                keys.holds_out(&id.key()) && !keys.admits(&id.key()),
                "{when}: {} is held out",
                id.node()
            );
            assert_eq!(
                drives_here(&self.places, id.node()),
                None,
                "{when}: whether {} drives here, which its grant admits, is judged without the \
                 vault",
                id.node()
            );
        }
        assert_eq!(driving(&self.places), None, "{when}");
        assert_eq!(
            denied_and_log(&self.places),
            (None, None),
            "{when}: read whole or not at all"
        );
        let cached = crate::fleet_census::read_cached_census(&self.places.state)
            .expect("the preceding healthy read cached its census")
            .to_json();
        let census = match self.census() {
            Ok(census) => census,
            Err(_) => {
                assert_eq!(
                    crate::fleet_census::read_cached_census(&self.places.state)
                        .expect("a deferred pass keeps the cached census")
                        .to_json(),
                    cached,
                    "{when}: an unread roster must not publish a partial or empty census",
                );
                self.assert_m9_facts_kept(when);
                return;
            }
        };
        assert!(
            census.fleet_book.iter().all(|p| {
                !self
                    .members()
                    .iter()
                    .any(|id| p.node.as_deref() == Some(id.node()))
            }),
            "{when}: census rows {:?}",
            census.fleet_book
        );
        let own = member::read_node_identity(&self.places.state)
            .unwrap()
            .unwrap();
        assert!(
            !aterm_net::fleet::is_denied_node(own.node(), &census.roster.deny_nodes)
                && !aterm_net::fleet::is_denied_key(&own.key(), &census.roster.deny_nodes),
            "{when}: this Mac's own unreadable store does not count it out: {:?}",
            census.roster.deny_nodes
        );
        self.assert_m9_kept(when, &census);
    }
}

/// A vault apply holding the state lock, an older valid snapshot put back, an interrupted
/// layout transition, a missing data file and a reader below the pinned capacity each leave a
/// fresh reader with no verdict: none of them is a Mac with no log, so none falls back to the
/// book.
#[test]
fn a_vault_this_mac_cannot_read_now_admits_no_node_its_book_would() {
    let lab = Lab::new("per-call");
    let none = Paths::for_home(&lab.root.path().join("no-vault-home"));
    assert!(
        LocalLog::of(&none).keys().is_none(),
        "a Mac with no vault has no verdict: admission falls back to the book"
    );
    lab.assert_read("read");

    let writer = client::test_hold_state_lock(&lab.paths).unwrap();
    lab.assert_unread("a vault apply holding the state lock");
    drop(writer);
    lab.assert_read("the apply done");

    let current = Lab::snapshot_of(&lab.paths);
    Lab::put_back(&lab.admitted);
    lab.assert_unread("an older snapshot put back");
    Lab::put_back(&current);
    lab.assert_read("the current snapshot back");
    Lab::put_back(&lab.admitted[..1]);
    lab.assert_unread("an older records file alone put back");
    Lab::put_back(&current);
    lab.assert_read("the current records file back");

    lab.fence();
    lab.assert_read("fenced");
    std::fs::rename(lab.pin(), lab.aside("pin")).unwrap();
    lab.assert_unread("a layout transition cut before its pin");
    std::fs::rename(lab.aside("pin"), lab.pin()).unwrap();
    lab.assert_read("the pin back");
    let data = lab.paths.records_data_file().unwrap();
    std::fs::rename(&data, lab.aside("current")).unwrap();
    lab.assert_unread("the layout's data file missing");
    std::fs::rename(lab.aside("current"), &data).unwrap();
    lab.assert_read("the data file back");
    RecordBudgets {
        held: RecordBudgets::PRODUCTION.held - 1,
        ..RecordBudgets::PRODUCTION
    }
    .within(|| lab.assert_unread("a reader below the store's pinned capacity"));
    lab.assert_read("at production capacity");
}

/// The listener's long-lived reader keeps the verdict it last read while its store cannot be
/// read, for the keys the log alone holds out (a member's rotated key, which no deny list
/// names) as for the rest: the flat layout's records file gone while its vault stays
/// configured, the acknowledged history gone with it (a store that reads no log while the
/// version this Mac applied names one), a reader below the pinned capacity (an older aterm still
/// running after an update),
/// a layout transition cut before its pin and the fenced layout's data file missing.
#[test]
fn a_listener_keeps_its_last_verdict_while_its_store_cannot_be_read() {
    let lab = Lab::new("listener");
    let rotated = NodeIdentity::from_seed(&"e6".repeat(16), &[0x9a; 32]).unwrap();
    assert!(ensure_local_node_key_in_log(&lab.m9_paths, &rotated, None));
    client::post_records_to_servers(&lab.m9_paths).unwrap();
    client::sync_records(&lab.paths, None).unwrap();
    let retired = lab.m9.key();

    let warm = LocalLog::of(&lab.paths);
    let before = warm.keys().expect("the log reads");
    assert!(before.holds_out(&retired) && before.admits(&rotated.key()));
    let retained = client::swarm_denied_nodes(&lab.paths).unwrap().unwrap();
    assert!(
        !aterm_net::fleet::is_denied_key(&retired, &retained),
        "PRECONDITION: the log alone holds the retired key out"
    );
    let grants = granted_here(&lab.places);
    let proof = aterm_net::fleet::NodeProof {
        node: lab.m9.node().to_owned(),
        key: retired,
    };
    let served = [Peer::own(
        &lab.m9,
        ("m9.local", 7100, "0.146.0"),
        NOW_MS + 5,
    )];
    let judge = |when: &str, log: Option<Arc<member::MemberKeys>>| {
        let log = log.unwrap_or_else(|| panic!("{when}: the listener keeps a verdict"));
        assert!(log.holds_out(&retired), "{when}: the retired key");
        assert!(log.admits(&rotated.key()), "{when}: the current key");
        assert!(
            member::recheck_driver(&proof, &lab.places.state, &grants, Some(&log)).is_err(),
            "{when}: the retired key drives by its grant and the book"
        );
        let mut book = Vec::new();
        assert_eq!(
            member::merge_signed(
                &mut book,
                &served,
                (None, NOW_MS + 10),
                (Some(&log), &retained, &member::Keyring::default()),
            ),
            0,
            "{when}: the retired key's entry is merged: {book:?}"
        );
    };
    judge("read", warm.keys());
    assert!(
        member::recheck_driver(&proof, &lab.places.state, &grants, None).is_ok(),
        "NEGATIVE CONTROL: the book and grant alone admit the retired key"
    );

    // The flat layout's records file gone, its vault still configured: the store reads empty,
    // which the history this Mac acknowledged refuses.
    assert!(
        lab.paths.records_file().is_file(),
        "PRECONDITION: the flat layout"
    );
    std::fs::rename(lab.paths.records_file(), lab.aside("records")).unwrap();
    judge("the flat layout's records file missing", warm.keys());
    // The acknowledged history gone with the records: the store reads no log, as one with no
    // genesis yet does, but the version this Mac applied names the vault's log, so the read
    // fails; the same vault stays configured, and the listener keeps its verdict.
    let history = lab.paths.acknowledged_history_files().map(|file| {
        let aside = lab.aside(&file.file_name().unwrap().to_string_lossy());
        (file, aside)
    });
    for (file, aside) in &history {
        std::fs::rename(file, aside).unwrap();
    }
    assert!(
        client::load_acknowledged_records(&lab.paths).is_ok_and(|store| store.log().is_none()),
        "PRECONDITION: the store reads no log"
    );
    judge(
        "the records and the history beside them missing",
        warm.keys(),
    );
    assert!(
        LocalLog::of(&lab.paths)
            .keys()
            .is_some_and(|fresh| fresh.unread().is_some() && !fresh.admits(&rotated.key())),
        "CONTROL: a reader with no verdict before has none to keep, and the version this Mac \
         applied names a log its store does not hold, so it admits no key: the listener's verdict \
         is its own"
    );
    for (file, aside) in &history {
        std::fs::rename(aside, file).unwrap();
    }
    // CONTROL: with its configuration gone too, the vault is gone and leaves no log to keep.
    let source = lab.paths.dir.join("source.toml");
    std::fs::rename(&source, lab.aside("source.toml")).unwrap();
    assert!(
        warm.keys().is_none(),
        "a Mac whose vault is gone has no verdict: admission falls back to the book"
    );
    std::fs::rename(lab.aside("source.toml"), &source).unwrap();
    std::fs::rename(lab.aside("records"), lab.paths.records_file()).unwrap();
    judge("the records file back", warm.keys());
    lab.fence();
    judge("fenced", warm.keys());

    let smaller = RecordBudgets {
        held: RecordBudgets::PRODUCTION.held - 1,
        ..RecordBudgets::PRODUCTION
    };
    let (kept, fresh) = smaller.within(|| (warm.keys(), LocalLog::of(&lab.paths).keys()));
    judge("a reader below the pinned capacity", kept);
    assert!(
        fresh.is_some_and(|fresh| fresh.holds_out(&rotated.key())),
        "a fresh reader below the pinned capacity admits no key"
    );

    std::fs::rename(lab.pin(), lab.aside("pin")).unwrap();
    judge("a layout transition cut before its pin", warm.keys());
    std::fs::rename(lab.aside("pin"), lab.pin()).unwrap();

    let data = lab.paths.records_data_file().unwrap();
    std::fs::rename(&data, lab.aside("current")).unwrap();
    judge("the layout's data file missing", warm.keys());
    std::fs::rename(lab.aside("current"), &data).unwrap();
    judge("the store back", warm.keys());
}

/// The flat layout's records file gone while the vault stays configured: the store reads empty,
/// but the history this Mac acknowledged holds the vault's genesis, so the vault exists and
/// cannot be read now. A reader started now (a listener launched while the file is gone) admits
/// no node the book alone would, and a sync that brings the records back reads the log again.
#[test]
fn a_flat_records_file_gone_admits_no_node_its_book_would() {
    let lab = Lab::new("records-gone");
    lab.assert_read("read");
    let grants = granted_here(&lab.places);
    let proof = |id: &NodeIdentity| aterm_net::fleet::NodeProof {
        node: id.node().to_owned(),
        key: id.key(),
    };
    assert!(
        member::recheck_driver(&proof(&lab.m7), &lab.places.state, &grants, None).is_ok(),
        "NEGATIVE CONTROL: the book and grant alone admit m7, which the log removed"
    );
    assert!(
        lab.paths.records_file().is_file(),
        "PRECONDITION: the flat layout"
    );
    std::fs::rename(lab.paths.records_file(), lab.aside("records")).unwrap();
    assert!(
        client::load_records(&lab.paths).is_ok_and(|store| store.log().is_none()),
        "PRECONDITION: the raw store reads empty"
    );
    let fresh = LocalLog::of(&lab.paths).keys();
    assert!(
        fresh
            .as_ref()
            .is_some_and(|keys| keys.holds_out(&lab.m7.key()) && keys.holds_out(&lab.m8.key())),
        "a vault whose records file is gone is no Mac without a log: the book's keyring would \
         admit m7 and m8, which the log holds out"
    );
    for id in [&lab.m7, &lab.m8] {
        assert!(
            member::recheck_driver(&proof(id), &lab.places.state, &grants, fresh.as_deref())
                .is_err(),
            "{} drives by its grant and the book",
            id.node()
        );
    }
    lab.assert_unread("the flat layout's records file gone");
    // No lockout: a sync takes the records back from the vault's server.
    client::sync_records(&lab.paths, None).unwrap();
    lab.assert_read("the records synced back");
}

/// A reader keeps its verdict until a file it reads changes, the acknowledged history's
/// included. A write cut after its records were made durable and before they were acknowledged
/// leaves the records ahead of that history: what they hold out stands out at once, and what
/// they grant waits for that history. The next writer acknowledges them and moves the history
/// alone, and a reader that read before it reads again.
#[test]
fn an_acknowledgement_alone_reads_the_store_again() {
    use std::os::unix::fs::MetadataExt as _;
    let lab = Lab::new("acknowledgement");
    for file in lab.paths.acknowledged_history_files() {
        assert!(file.is_file(), "the reader watches {}", file.display());
    }
    let rotated = NodeIdentity::from_seed(&"e6".repeat(16), &[0x9a; 32]).unwrap();
    assert!(ensure_local_node_key_in_log(&lab.m9_paths, &rotated, None));
    // m9's rotation, written into this Mac's records by a write cut before its acknowledgement.
    let mut ahead = client::load_records(&lab.paths).unwrap();
    let m9_records = client::load_records(&lab.m9_paths).unwrap().encode();
    assert!(ahead.ingest(&m9_records).unwrap() > 0);
    assert!(
        crate::fleet_log::log_member_keys(ahead.log().unwrap()).holds_out(&lab.m9.key()),
        "PRECONDITION: the records ahead hold m9's first key out"
    );
    aterm_secrets::fsx::write_atomic(&lab.paths.records_file(), &ahead.encode(), 0o600).unwrap();
    assert!(
        client::load_acknowledged_records(&lab.paths).is_ok_and(|store| {
            crate::fleet_log::log_member_keys(store.log().unwrap()).admits(&lab.m9.key())
        }),
        "PRECONDITION: the history this Mac acknowledged admits m9's first key"
    );
    let warm = LocalLog::of(&lab.paths);
    let before = warm.keys().expect("the acknowledged log reads");
    assert!(
        before.holds_out(&lab.m9.key()),
        "records a writer made durable hold out what they hold out before any writer \
         acknowledges them: m9's rotated-out key"
    );
    assert!(
        !before.admits(&rotated.key()),
        "records no writer acknowledged grant nothing: m9's new key waits for its acknowledgement"
    );
    // Every file the reader reads but the history's: the acknowledgement must move only those.
    let unmoved = || {
        [
            lab.paths.records_file(),
            lab.paths.dir.join("source.toml"),
            lab.paths.dir.join("state.toml"),
            lab.paths.dir.join("vault"),
            lab.paths.dir.join("identity"),
        ]
        .map(|file| {
            let meta = std::fs::symlink_metadata(&file)
                .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            (file, meta.ino(), meta.len(), meta.modified().unwrap())
        })
    };
    let stamps = unmoved();
    assert_eq!(
        client::save_records(&lab.paths, &[]).unwrap(),
        0,
        "the next writer acknowledges the records and has none to add"
    );
    assert_eq!(
        unmoved(),
        stamps,
        "PRECONDITION: the acknowledgement moved no file the reader reads but the history's"
    );
    let after = warm.keys().expect("the acknowledged log reads");
    assert!(
        after.holds_out(&lab.m9.key()) && after.admits(&rotated.key()),
        "a reader that read before the acknowledgement reads it: m9's new key is in"
    );
    let fresh = LocalLog::of(&lab.paths).keys().expect("the log reads");
    assert!(
        fresh.holds_out(&lab.m9.key()) && fresh.admits(&rotated.key()),
        "CONTROL: a fresh reader"
    );
}

/// The service activation's membership frontier (`crate::service_activation`), the other
/// authority read of this store: a request names, and its target checks, the frontier of the
/// history this Mac acknowledged, never the raw heads. In a write's cut window that frontier is
/// the one compared, and once the records ahead are acknowledged a request must name every head
/// of the frontier they leave; a store that omits that history (its records file gone, an older
/// one put back) refuses as unavailable rather than compare raw heads.
#[test]
fn a_service_activation_compares_the_acknowledged_frontier() {
    use crate::service_activation::{membership_frontier, require_membership_frontier};
    let lab = Lab::new("activation");
    let heads = |store: &aterm_secrets::membership_log::Store| -> Vec<String> {
        store
            .frontier()
            .iter()
            .map(|id| aterm_codec::hex::encode_lower(id))
            .collect()
    };
    let naming = |records: &[String]| aterm_secrets::service_repair::HealthRequirement {
        vault: String::new(),
        signer: String::new(),
        records: records.to_vec(),
        frontier: 0,
    };
    let unavailable = |when: &str| {
        assert_eq!(
            membership_frontier(&lab.paths).err().map(|e| e.kind()),
            Some(aterm_secrets::ErrorKind::MembershipHistoryUnavailable),
            "{when}: the frontier a request names"
        );
        let checked = require_membership_frontier(&lab.paths, &naming(&[])).err();
        assert_eq!(
            checked.map(|e| e.kind()),
            Some(aterm_secrets::ErrorKind::MembershipHistoryUnavailable),
            "{when}: the frontier a target checks"
        );
    };

    let acknowledged = membership_frontier(&lab.paths).unwrap();
    assert!(!acknowledged.is_empty());
    assert_eq!(
        acknowledged,
        heads(&client::load_records(&lab.paths).unwrap()),
        "PRECONDITION: in steady state the raw and acknowledged frontiers are one"
    );
    require_membership_frontier(&lab.paths, &naming(&acknowledged)).unwrap();
    assert!(
        require_membership_frontier(&lab.paths, &naming(&[])).is_err(),
        "CONTROL: a request that names no frontier"
    );

    // m9's rotation, written into this Mac's records by a write cut before its acknowledgement.
    let rotated = NodeIdentity::from_seed(&"e6".repeat(16), &[0x9a; 32]).unwrap();
    assert!(ensure_local_node_key_in_log(&lab.m9_paths, &rotated, None));
    let mut ahead = client::load_records(&lab.paths).unwrap();
    let m9_records = client::load_records(&lab.m9_paths).unwrap().encode();
    assert!(ahead.ingest(&m9_records).unwrap() > 0);
    aterm_secrets::fsx::write_atomic(&lab.paths.records_file(), &ahead.encode(), 0o600).unwrap();
    let raw = heads(&client::load_records(&lab.paths).unwrap());
    assert!(
        raw.len() > acknowledged.len() && acknowledged.iter().all(|head| raw.contains(head)),
        "PRECONDITION: the raw heads hold the acknowledged ones and more: {raw:?}, {acknowledged:?}"
    );
    assert_eq!(
        membership_frontier(&lab.paths).unwrap(),
        acknowledged,
        "a request names the acknowledged frontier, not the raw heads"
    );
    require_membership_frontier(&lab.paths, &naming(&acknowledged))
        .expect("a request naming the acknowledged frontier is the current one");
    assert_eq!(client::save_records(&lab.paths, &[]).unwrap(), 0);
    assert_eq!(
        membership_frontier(&lab.paths).unwrap(),
        raw,
        "the next writer acknowledges the records ahead"
    );
    // Once they are, a request prepared in the cut window names some heads of the frontier and
    // omits the rest: it is stale.
    assert!(
        require_membership_frontier(&lab.paths, &naming(&acknowledged)).is_err(),
        "a request that omits a head of the frontier passes"
    );
    require_membership_frontier(&lab.paths, &naming(&raw))
        .expect("a request naming every head of the frontier is the current one");

    let current = Lab::snapshot_of(&lab.paths);
    std::fs::rename(lab.paths.records_file(), lab.aside("records")).unwrap();
    unavailable("the flat layout's records file gone");
    std::fs::rename(lab.aside("records"), lab.paths.records_file()).unwrap();
    assert_eq!(membership_frontier(&lab.paths).unwrap(), raw);
    Lab::put_back(&lab.admitted[..1]);
    unavailable("an older records file put back");
    Lab::put_back(&current);
    assert_eq!(membership_frontier(&lab.paths).unwrap(), raw);
}

/// A reader keeps its verdict through a read that fails only while the vault it read it under
/// stays configured. This Mac joins another vault here (by that vault's log) and takes its
/// records, a listener's reader still holding the verdict of the vault it left; a read of the
/// store under the new vault fails (as it does while the new vault's pass holds the state lock),
/// and that reader must not admit the members of the vault left, whom the new one does not name.
#[test]
fn a_join_elsewhere_keeps_no_verdict_of_the_vault_left() {
    let lab = Lab::new("join-elsewhere");
    let warm = LocalLog::of(&lab.paths);
    assert!(
        warm.keys().expect("the log reads").admits(&lab.m9.key()),
        "PRECONDITION: the vault this Mac is in admits m9"
    );
    let addr = lab.running.as_ref().unwrap().addr;
    let url = format!("http://{addr}/v1/other");
    let other = Paths::for_home(&lab.root.path().join("m30-home"));
    let created = client::create(&other, &url, None, Some("m30")).unwrap();
    client::admin_seed_swarm_roster(&other, &[]).unwrap();
    client::seed_vault_genesis(&other, &lab.root.path().join("other.genesis")).unwrap();
    client::admin_mirror_log(&other, &[]).unwrap();

    client::configure_log_member(&lab.paths, &format!("{url}#{}", created.signer), "m22").unwrap();
    assert!(client::sync_records(&lab.paths, None).unwrap() > 0);
    let pass = client::test_hold_state_lock(&lab.paths).unwrap();
    assert!(
        !lab.store_reads(),
        "PRECONDITION: the store cannot be read under the vault joined"
    );
    let keys = warm
        .keys()
        .expect("a vault that cannot be read is no Mac without a log");
    assert!(
        !keys.admits(&lab.m9.key()) && keys.unread().is_some(),
        "the reader kept the verdict of the vault this Mac left: m9 is admitted"
    );
    let grants = granted_here(&lab.places);
    let proof = aterm_net::fleet::NodeProof {
        node: lab.m9.node().to_owned(),
        key: lab.m9.key(),
    };
    assert!(
        member::recheck_driver(&proof, &lab.places.state, &grants, Some(&keys)).is_err(),
        "m9 drives this Mac by its grant under the vault joined"
    );
    let fresh = LocalLog::of(&lab.paths)
        .keys()
        .expect("CONTROL: a fresh reader");
    assert!(
        fresh.unread().is_some(),
        "CONTROL: a fresh reader reads none"
    );
    drop(pass);
}

/// A reader keeps its verdict through a read that fails while the same vault stays configured,
/// whatever rewrote its configuration since: a mirror added (`aterm secrets mirrors add`), the
/// file written again as it was (a same-vault join), and a signer rotation (the new signer
/// pinned, reached from the old one). A vault apply holds the state lock after each, as a
/// rotation's own apply does after it saves the new pin, so the store cannot be read. A
/// configuration that pins another key line under the vault's name (a replacement's shape)
/// keeps no verdict read under the line before.
#[test]
fn a_same_vault_rewrite_of_its_configuration_keeps_the_verdict() {
    let lab = Lab::new("same-vault");
    let warm = LocalLog::of(&lab.paths);
    let m9 = lab.m9.key();
    let reads = |when: &str| {
        let keys = warm
            .keys()
            .unwrap_or_else(|| panic!("{when}: the log reads"));
        assert!(
            keys.admits(&m9) && keys.unread().is_none(),
            "{when}: the reader reads the log again"
        );
    };
    let kept = |when: &str| {
        let pass = client::test_hold_state_lock(&lab.paths).unwrap();
        assert!(
            !lab.store_reads(),
            "PRECONDITION {when}: the store cannot be read"
        );
        let keys = warm
            .keys()
            .expect("a vault that cannot be read is no Mac without a log");
        drop(pass);
        assert!(
            keys.admits(&m9) && keys.unread().is_none(),
            "{when}: the reader dropped the verdict of the vault still configured, so m9 is \
             turned away"
        );
    };
    reads("PRECONDITION: the vault this Mac is in");
    let source = lab.paths.dir.join("source.toml");
    let original = std::fs::read(&source).unwrap();

    aterm_secrets::fsx::write_atomic(
        &lab.paths.dir.join("state.toml"),
        &std::fs::read(lab.paths.dir.join("state.toml")).unwrap(),
        0o600,
    )
    .unwrap();
    kept("CONTROL: the state file written again, the configuration untouched");
    reads("CONTROL: the pass done");

    let config = client::config(&lab.paths).unwrap().unwrap();
    let mirrored = client::set_mirrors(
        &lab.paths,
        &config,
        &["http://127.0.0.1:9/v1/test".to_owned()],
    )
    .unwrap();
    assert_eq!(
        (mirrored.mirrors.len(), &mirrored.name, mirrored.signer),
        (1, &config.name, config.signer),
        "PRECONDITION: a mirror added to the same vault"
    );
    kept("a mirror added");
    reads("the mirror's pass done");

    aterm_secrets::fsx::write_atomic(&source, &std::fs::read(&source).unwrap(), 0o600).unwrap();
    kept("the configuration written again as it was");
    reads("the rewrite's pass done");

    let old = config.signer.expect("PRECONDITION: a pinned vault");
    let key = |key: &[u8; 32]| format!("\"{}\"", aterm_secrets::b64::encode_padded(key));
    let pinned = |signer: &[u8; 32], followed: &[[u8; 32]]| {
        let kept: String = String::from_utf8(original.clone())
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("signer") && !line.starts_with("followed"))
            .map(|line| format!("{line}\n"))
            .collect();
        let followed: Vec<String> = followed.iter().map(key).collect();
        format!(
            "{kept}signer = {}\nfollowed = [{}]\n",
            key(signer),
            followed.join(", ")
        )
    };
    let rotated = [0x5b; 32];
    aterm_secrets::fsx::write_atomic(&source, pinned(&rotated, &[old]).as_bytes(), 0o600).unwrap();
    let now = client::config(&lab.paths).unwrap().unwrap();
    assert_eq!(
        (now.signer, now.followed.as_slice(), &now.name),
        (Some(rotated), &[old][..], &config.name),
        "PRECONDITION: the vault's pin rotated"
    );
    kept("a signer rotation");
    aterm_secrets::fsx::write_atomic(&source, &original, 0o600).unwrap();
    reads("the original pin back");

    // Another key line under the vault's name, as a replacement pins: the store holds no genesis
    // that line signs, so it reads no log, and admission falls back to the book.
    aterm_secrets::fsx::write_atomic(&source, pinned(&[0x5c; 32], &[]).as_bytes(), 0o600).unwrap();
    let pass = client::test_hold_state_lock(&lab.paths).unwrap();
    assert!(
        warm.keys().is_none(),
        "another key line under the vault's name kept the verdict read under the line before"
    );
    drop(pass);
    aterm_secrets::fsx::write_atomic(&source, &original, 0o600).unwrap();
    reads("the original pin back");
}

/// Vault "other", made on this lab's server by m30 with its log seeded, which lets this Mac in by
/// its envelope (`aterm secrets approve`): where [`join_other`] takes this Mac.
fn other_vault(lab: &Lab) -> (Paths, String) {
    let addr = lab.running.as_ref().unwrap().addr;
    let url = format!("http://{addr}/v1/other");
    let other = Paths::for_home(&lab.root.path().join("m30-home"));
    let created = client::create(&other, &url, None, Some("m30")).unwrap();
    client::admin_seed_swarm_roster(&other, &[]).unwrap();
    client::seed_vault_genesis(&other, &lab.root.path().join("other.genesis")).unwrap();
    client::admin_mirror_log(&other, &[]).unwrap();
    let recipient = client::read_machine_identity(&lab.paths)
        .unwrap()
        .expect("this Mac's key")
        .recipient()
        .to_bech32();
    let admin = client::admin_open(&other).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_machine("m22", &recipient).unwrap();
    client::admin_publish_approved(&other, &admin, &doc, std::slice::from_ref(&recipient)).unwrap();
    (other, format!("{url}#{}", created.signer))
}

/// `aterm secrets join` of `source` on this Mac (`client::join`, as the CLI runs it).
fn join_other(lab: &Lab, source: &str) {
    client::join(
        &lab.paths,
        &client::JoinOptions {
            source: source.to_owned(),
            host: Some("m22".to_owned()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: client::JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .unwrap();
}

/// `aterm secrets join` of another vault (`client::join`): the join applies the new vault's
/// version, which its log's writer published, and its records are not here yet. That vault exists
/// and cannot be read now, so neither a fresh reader nor the listener's, which read this Mac's
/// vault before, lets in a node the book alone would admit: not m9, nor m7 and m8, which the
/// vault left took out. Once its records are here, its log reads.
///
/// NEGATIVE CONTROL: read a store with no log as a Mac with no vault (drop the
/// `applied_version_names_log` refusal from `acknowledged_member_keys`): fails "the listener's
/// reader falls back to the book", and the book lets m7, m8 and m9 drive.
#[test]
fn a_join_elsewhere_admits_no_node_by_the_book_until_the_vault_joineds_log_is_here() {
    let lab = Lab::new("real-join");
    let warm = LocalLog::of(&lab.paths);
    assert!(
        warm.keys().expect("the log reads").admits(&lab.m9.key()),
        "PRECONDITION: the vault this Mac is in admits m9"
    );
    let (_other, source) = other_vault(&lab);
    join_other(&lab, &source);
    assert!(
        client::load_records(&lab.paths).unwrap().log().is_none()
            && client::load_acknowledged_records(&lab.paths)
                .is_ok_and(|store| store.log().is_none()),
        "PRECONDITION: the vault joined holds no log here yet"
    );
    assert!(
        client::applied_version_names_log(&lab.paths).unwrap(),
        "PRECONDITION: the version applied of the vault joined names its log"
    );
    for (reader, which) in [
        (&warm, "the listener's reader"),
        (&LocalLog::of(&lab.paths), "a fresh reader"),
    ] {
        let keys = reader
            .keys()
            .unwrap_or_else(|| panic!("{which} falls back to the book"));
        assert!(
            keys.unread().is_some(),
            "{which}: the vault cannot be read now"
        );
        for id in lab.members() {
            assert!(
                keys.holds_out(&id.key()) && !keys.admits(&id.key()),
                "{which}: {} is held out",
                id.node()
            );
        }
    }
    for id in lab.members() {
        assert_eq!(
            drives_here(&lab.places, id.node()),
            None,
            "{} drives this Mac by the book and its grant",
            id.node()
        );
    }
    assert!(client::sync_records(&lab.paths, None).unwrap() > 0);
    let keys = LocalLog::of(&lab.paths)
        .keys()
        .expect("CONTROL: the vault joined's log reads");
    assert!(
        keys.unread().is_none(),
        "CONTROL: the vault joined's log reads"
    );
}

/// ATERM-KI-1105-10's steady state. This Mac's fleet was taken under vault "test" (its key, its
/// book binding m7, m8 and m9, and a drive grant of each, the join's take recording that vault),
/// and this Mac then moves to vault "other" (`aterm secrets join`, [`join_other`]) and takes its
/// records. The log of "other" reads and binds none of the three, so F17 judges each by the book
/// as with no log; the grants were made under the vault left, so none admits: not m7, which
/// "test" revoked, not m8, which its roster denies, and not the control m9. The grants stay in
/// aterm.toml as the person made them, and `aterm fleet peers` says why none admits now.
///
/// NEGATIVE CONTROL (2026-11-05): let every grant admit whichever vault it was made under
/// (`fleet_vault::grants_in_effect` returning `grants`, as before the fix): fails "m7, m8 and m9
/// drive this Mac by the book and the grants made under the vault it left" with
/// `[Some(true), Some(true), Some(true)]`.
#[test]
fn after_a_move_to_another_vault_no_mac_of_the_vault_left_drives_by_its_grants() {
    let lab = Lab::new("moved");
    {
        let book = member::hold_book(&lab.places.state).unwrap();
        book.write_key(&[0x44; 24]).unwrap();
        crate::fleet_vault::take(&book, Vault::configured(&lab.paths).as_ref()).unwrap();
    }
    assert_eq!(
        crate::fleet_vault::tenure(&lab.places.state, Some(&lab.paths)),
        crate::fleet_vault::Tenure::Here,
        "PRECONDITION: the fleet was taken under the vault this Mac is in"
    );
    assert_eq!(
        lab.members().map(|id| drives_here(&lab.places, id.node())),
        [Some(false), Some(false), Some(true)],
        "PRECONDITION: the vault this Mac is in holds m7 and m8 out, and the control drives"
    );

    let (_other, source) = other_vault(&lab);
    join_other(&lab, &source);
    assert!(client::sync_records(&lab.paths, None).unwrap() > 0);
    let keys = LocalLog::of(&lab.paths)
        .keys()
        .expect("PRECONDITION: the log of the vault joined reads");
    assert!(keys.unread().is_none(), "PRECONDITION: it reads");
    for id in lab.members() {
        assert!(
            keys.standing(&id.key()).is_none(),
            "PRECONDITION: the log of the vault joined binds no key of {}, so F17 judges it by \
             the book",
            id.node()
        );
    }
    let (denied, log) = denied_and_log(&lab.places);
    assert!(
        denied.is_some() && log.is_some(),
        "PRECONDITION: the deny list and the log of the vault joined read"
    );

    assert_eq!(
        lab.members().map(|id| drives_here(&lab.places, id.node())),
        [Some(false); 3],
        "m7, m8 and m9 drive this Mac by the book and the grants made under the vault it left"
    );
    assert_eq!(driving(&lab.places), Some(Vec::new()));
    assert!(granted_here(&lab.places).is_empty());
    assert_eq!(
        configured_grants(&lab.places).len(),
        3,
        "a person's grants stay in aterm.toml as they made them"
    );
    let proof = |id: &NodeIdentity| aterm_net::fleet::NodeProof {
        node: id.node().to_owned(),
        key: id.key(),
    };
    for id in lab.members() {
        assert!(
            matches!(
                member::recheck_driver(
                    &proof(id),
                    &lab.places.state,
                    &granted_here(&lab.places),
                    Some(&keys)
                ),
                Err(member::Undriven::Unlisted(_))
            ),
            "{} drives by the grants in effect",
            id.node()
        );
        assert!(
            member::recheck_driver(
                &proof(id),
                &lab.places.state,
                &configured_grants(&lab.places),
                Some(&keys)
            )
            .is_ok(),
            "CONTROL: the book and the grants made under the vault left would admit {} under the \
             log of the vault joined",
            id.node()
        );
    }
    let said = void_grants(&lab.places).expect("`aterm fleet peers` says why no grant admits");
    assert!(
        said.contains("taken under vault \"test\"")
            && said.contains("this Mac is in vault \"other\" now")
            && said.contains("drive_from keeps 3 grants as they are")
            && said.contains("`aterm fleet sync`"),
        "{said}"
    );
}

/// The fleet taken again under the vault joined drives by the grants made under it exactly as
/// before. A grant written beside the fleet still taken under the vault left admits no Mac; the
/// take again goes in a join's order (the grants made under the vault left out of aterm.toml, as
/// its `Ungrant::OtherVault` takes them, then the vault joined recorded), and the person grants
/// under it after: m31, which the log of "other" seats, drives by its grant through that log, and
/// m9, which no log binds, by its grant and the book (F17), as the person granted it under
/// "other"; m7 and m8, granted nothing under "other", drive nothing.
///
/// NEGATIVE CONTROL (2026-11-05): a fleet taken under any vault but the one it records read as
/// taken elsewhere (`fleet_vault::tenure` answering `Elsewhere` where the vaults match) fails
/// "m31, which the vault joined seats, drives by its grant made under it".
#[test]
fn the_fleet_taken_again_under_the_vault_joined_drives_by_its_grants_as_before() {
    let lab = Lab::new("taken-again");
    {
        let book = member::hold_book(&lab.places.state).unwrap();
        book.write_key(&[0x44; 24]).unwrap();
        crate::fleet_vault::take(&book, Vault::configured(&lab.paths).as_ref()).unwrap();
    }
    let (other, source) = other_vault(&lab);
    join_other(&lab, &source);
    let m31 = NodeIdentity::from_seed(&"c3".repeat(16), &[0x31; 32]).unwrap();
    let _ = admit(&other, lab.root.path(), "m31", &m31);
    assert!(client::sync_records(&lab.paths, None).unwrap() > 0);
    let keys = LocalLog::of(&lab.paths)
        .keys()
        .expect("PRECONDITION: the log of the vault joined reads");
    assert!(
        keys.admits(&m31.key()) && keys.standing(&lab.m9.key()).is_none(),
        "PRECONDITION: the vault joined seats m31 and binds no key of m9"
    );
    let mut book = std::fs::read_to_string(lab.places.state.join(PEERS_FILE)).unwrap();
    book.push_str(&format!(
        "{}\n",
        Peer::own(&m31, ("m31.local", 7100, "0.146.0"), NOW_MS).line()
    ));
    write_private(&lab.places.state, PEERS_FILE, book.as_bytes()).unwrap();
    let grant_m31 = format!(
        "[fleet]\nallow = \"drive\"\ndrive_from = [\"{}:{}\"]\n",
        m31.node(),
        m31.key().hex()
    );
    crate::test_config::write(&lab.places.config, &grant_m31).unwrap();
    assert_eq!(
        drives_here(&lab.places, m31.node()),
        Some(false),
        "PRECONDITION: a grant beside a fleet taken under the vault left admits no Mac"
    );

    // The take again, in a join's order: the grants made beside the vault left out (the join's
    // `Ungrant::OtherVault` lowers drive to content), then the vault joined recorded.
    crate::test_config::write(&lab.places.config, "[fleet]\nallow = \"content\"\n").unwrap();
    {
        let book = member::hold_book(&lab.places.state).unwrap();
        crate::fleet_vault::take(&book, Vault::configured(&lab.paths).as_ref()).unwrap();
    }
    assert_eq!(
        (driving(&lab.places), drives_here(&lab.places, m31.node())),
        (Some(Vec::new()), Some(false)),
        "PRECONDITION: the take again keeps no grant"
    );
    // The person's grant, made under the vault joined.
    crate::test_config::write(&lab.places.config, &grant_m31).unwrap();
    assert_eq!(
        drives_here(&lab.places, m31.node()),
        Some(true),
        "m31, which the vault joined seats, drives by its grant made under it"
    );
    crate::test_config::write(
        &lab.places.config,
        format!(
            "[fleet]\nallow = \"drive\"\ndrive_from = [\"{}:{}\", \"{}:{}\"]\n",
            m31.node(),
            m31.key().hex(),
            lab.m9.node(),
            lab.m9.key().hex()
        ),
    )
    .unwrap();
    assert_eq!(
        [&m31, &lab.m7, &lab.m8, &lab.m9].map(|id| drives_here(&lab.places, id.node())),
        [Some(true), Some(false), Some(false), Some(true)],
        "m31 through the log, m9 by the book (F17) as granted under the vault joined, m7 and m8 \
         granted nothing there"
    );
    let mut seated = driving(&lab.places).unwrap();
    seated.sort();
    let mut expected = vec![m31.node().to_owned(), lab.m9.node().to_owned()];
    expected.sort();
    assert_eq!(seated, expected);
    assert_eq!(void_grants(&lab.places), None);
}
