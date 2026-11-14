// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn a_retake_during_membership_policy_withdraws_the_captured_custody() {
    for retake in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let vault = aterm_secrets::client::Paths::for_home(root.path());
        aterm_secrets::fsx::ensure_private_dir(&state).unwrap();
        aterm_agent::fleet_vault::take_fixture(&state, Some(&vault)).unwrap();
        let traffic = aterm_agent::fleet_vault::Traffic::capture(&state, Some(&vault)).unwrap();
        let verdict = denial_under_take(traffic, move || {
            if retake {
                aterm_agent::fleet_vault::take_fixture(&state, Some(&vault)).unwrap();
            }
            Arc::new(aterm_net::fleet::Denied::default())
        })();
        assert_eq!(verdict.withdrawn, retake);
        assert_eq!(verdict.refuses(None), retake);
    }
}

#[cfg(unix)]
#[test]
fn fleet_handshake_custody_overrides_a_retained_member_verdict() {
    use aterm_net::fleet::{Denied, NodeIdentity, NodeProof, Seat};
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let vault = aterm_secrets::client::Paths::for_home(root.path());
    aterm_secrets::fsx::ensure_private_dir(&state).unwrap();
    aterm_net::fleet_member::write_held(&state, aterm_net::fleet_member::Began::Here, |book| {
        book.write_key(&[0x5a; 24]).unwrap()
    })
    .unwrap();
    let me = NodeIdentity::from_seed(&"5a".repeat(16), &[0x5a; 32]).unwrap();
    let proof = NodeProof {
        node: me.node().to_owned(),
        key: me.key(),
    };
    let log: aterm_net::fleet_member::MemberKeys =
        [(me.key(), aterm_net::fleet_member::Standing::Member)]
            .into_iter()
            .collect();
    let verdict = || denied_for_fleet(&state, Some(&vault), Some(&[]), Some(&log));
    assert_eq!(verdict().seat(&proof), Seat::Out, "unknown custody refuses");
    aterm_agent::fleet_vault::take_fixture(&state, Some(&vault)).unwrap();
    assert_eq!(
        verdict().seat(&proof),
        Seat::Member,
        "explicit standalone control"
    );
    aterm_secrets::fsx::ensure_private_dir(&vault.dir).unwrap();
    let configure = |name: &str, signer: u8| {
        aterm_secrets::fsx::write_atomic(
            &vault.dir.join("source.toml"),
            format!("url = \"http://127.0.0.1:9/v1/{name}\"\nname = \"{name}\"\nsigner = \"{}\"\nhost = \"m22\"\n",
                aterm_secrets::b64::encode_padded(&[signer; 32])).as_bytes(),
            0o600,
        ).unwrap();
    };
    configure("first", 1);
    assert_eq!(
        verdict().seat(&proof),
        Seat::Out,
        "standalone custody cannot adopt a vault"
    );
    aterm_agent::fleet_vault::take_fixture(&state, Some(&vault)).unwrap();
    assert_eq!(verdict().seat(&proof), Seat::Member, "same-vault control");
    configure("other", 2);
    let withdrawn = verdict();
    assert_eq!(
        withdrawn.seat(&proof),
        Seat::Out,
        "retained old membership cannot admit"
    );
    assert!(
        withdrawn.refuses(None),
        "an unproved peer cannot bypass custody"
    );
    assert!(!withdrawn.is_empty());
    // Negative control: membership alone still seats this key after the local vault move.
    assert_eq!(
        Denied::under(Some(&[]), Some(&log)).seat(&proof),
        Seat::Member
    );
    aterm_agent::fleet_vault::take_fixture(&state, Some(&vault)).unwrap();
    assert_eq!(
        verdict().seat(&proof),
        Seat::Member,
        "explicit retake recovers"
    );
    aterm_secrets::fsx::write_atomic(&vault.dir.join("source.toml"), b"broken = [", 0o600).unwrap();
    assert_eq!(
        verdict().seat(&proof),
        Seat::Out,
        "unread configuration withdraws"
    );
    configure("other", 2);
    std::fs::remove_file(state.join(aterm_net::fleet_member::VAULT_FILE)).unwrap();
    assert_eq!(
        verdict().seat(&proof),
        Seat::Out,
        "a lost take cannot retain admission"
    );
}

/// A change of this Mac's fleet access is found once, and only a drop below Read and Write
/// that has rested lifts the remote holds: their holders can hand them back no more. A
/// drop on its way back up (the lock's click from Full Access through Read Only to Read
/// and Write) lifts none: not while it rests, not while the lock holds the raise its person
/// clicked for, and not once that raise is in force. The first look finds no change.
/// CONTROL: the same drop left to rest lifts them, once.
#[cfg(unix)]
#[test]
fn a_drop_below_read_and_write_lifts_the_remote_holds_once_it_has_rested() {
    use aterm_types::fleet_access::FleetAccess::{Full, ReadOnly, ReadWrite};
    use aterm_types::fleet_access::SETTLE;
    let lifts = std::cell::Cell::new(0);
    let lift = || {
        lifts.set(lifts.get() + 1);
        2
    };
    let t0 = Instant::now();
    let fresh = Some(Duration::ZERO);
    let mut seen = AccessSeen::default();
    let mut look =
        |level, age, raising, at: Instant| access_seen(&mut seen, (level, age), raising, at, &lift);
    let quiet = AccessLook {
        change: None,
        lifted: None,
        again: None,
    };
    assert_eq!(look(Full, None, None, t0), quiet, "the first look");
    assert_eq!(look(Full, None, None, t0), quiet);
    assert_eq!(
        look(ReadWrite, fresh, None, t0),
        AccessLook {
            change: Some((Full, ReadWrite)),
            ..quiet
        },
        "Read and Write still takes a `hold off`"
    );

    // CONTROL: a drop left to rest lifts the holds, once, and not before.
    assert_eq!(
        look(ReadOnly, fresh, None, t0),
        AccessLook {
            change: Some((ReadWrite, ReadOnly)),
            lifted: None,
            again: Some(t0 + SETTLE),
        }
    );
    assert_eq!(
        look(ReadOnly, fresh, None, t0 + SETTLE / 2).again,
        Some(t0 + SETTLE)
    );
    assert_eq!(lifts.get(), 0, "refused at once, lifted only once rested");
    assert_eq!(
        look(ReadOnly, fresh, None, t0 + SETTLE),
        AccessLook {
            lifted: Some(2),
            ..quiet
        }
    );
    assert_eq!(look(ReadOnly, fresh, None, t0 + 9 * SETTLE), quiet);
    assert_eq!(lifts.get(), 1);

    // Full Access, one click down to Read Only, a second on to Read and Write: the raise
    // waits its settle, longer than the drop's rest, and lands. Nothing is lifted.
    let t1 = t0 + 20 * SETTLE;
    assert_eq!(look(Full, fresh, None, t1).change, Some((ReadOnly, Full)));
    assert_eq!(lifts.get(), 1, "a raise lifts nothing");
    assert_eq!(
        look(ReadOnly, fresh, Some(false), t1).again,
        Some(t1 + SETTLE)
    );
    let clicked = t1 + SETTLE / 2;
    assert_eq!(
        look(ReadOnly, fresh, Some(true), clicked),
        quiet,
        "the lock's own clock brings the next look"
    );
    assert_eq!(look(ReadOnly, fresh, None, clicked + SETTLE), quiet);
    assert_eq!(
        look(ReadWrite, fresh, Some(false), clicked + SETTLE).change,
        Some((ReadOnly, ReadWrite))
    );
    assert_eq!(look(ReadWrite, fresh, None, clicked + 9 * SETTLE), quiet);
    assert_eq!(lifts.get(), 1, "the holds outlived the way down");

    // The same way down with the raise cancelled (the menu opened): the drop has rested by
    // then, and the holds go at that look.
    let t2 = t1 + 40 * SETTLE;
    assert!(look(ReadOnly, fresh, Some(false), t2).again.is_some());
    assert_eq!(look(ReadOnly, fresh, Some(true), t2 + SETTLE / 2), quiet);
    assert_eq!(
        look(ReadOnly, fresh, Some(false), t2 + 2 * SETTLE).lifted,
        Some(2)
    );
    assert_eq!(lifts.get(), 2);

    // A drop first seen after it has rested (a window with no status item, at its next
    // fleet line): the file says how long ago it was stated.
    let t3 = t2 + 40 * SETTLE;
    assert!(look(Full, fresh, None, t3).change.is_some());
    assert_eq!(
        look(ReadOnly, Some(SETTLE), None, t3 + SETTLE),
        AccessLook {
            change: Some((Full, ReadOnly)),
            lifted: Some(2),
            again: None,
        }
    );
    assert_eq!(lifts.get(), 3, "a drop from Full Access lifts them too");
    // A file that says no time is taken as stated now.
    assert!(look(Full, None, None, t3 + 2 * SETTLE).change.is_some());
    assert_eq!(
        look(ReadOnly, None, None, t3 + 2 * SETTLE).again,
        Some(t3 + 3 * SETTLE)
    );
    assert_eq!(lifts.get(), 3);
}

/// Which vault a process reads follows where its state root resolved, and nothing else
/// (ATERM-KI-1105-10, [`vault_under`]): on the account's own state it reads the account's
/// vault, with `CI` set as without; under a private root it reads none; on the account's own
/// state with no vault to be found it reads none and says why. A test binary runs under its
/// scratch root, so no test reads the person's vault.
///
/// NEGATIVE CONTROL (2026-11-05): `vault_under` deciding by the environment
/// (`private_root` or `ATERM_STATE_HOME` or `CI` set, a mutation inside the rule; the base
/// read the environment in the predicate that feeds it, [`state_root_is_private`]'s place)
/// fails "`CI` on the account's own state reads its vault". That predicate put back to the
/// base's reading fails no test of this crate, as `cfg!(test)` holds it true here; `aterm`'s
/// `unit_peers` test holds it.
#[cfg(unix)]
#[test]
fn the_vault_a_process_reads_follows_where_its_state_root_resolved() {
    if !crate::test_alone::alone() {
        return;
    }
    let home = aterm_tempfile::tempdir().unwrap();
    let vault = aterm_secrets::client::Paths::for_home(home.path());
    let found = || Ok(vault.clone());
    crate::test_env::scoped("CI", "1", || {
        assert_eq!(
            vault_under(false, found),
            Ok(Some(vault.clone())),
            "`CI` on the account's own state reads its vault"
        );
    });
    assert_eq!(
        vault_under(false, found),
        Ok(Some(vault.clone())),
        "the account's own state reads its vault"
    );
    assert_eq!(
        vault_under(true, || panic!("a private state root reads no vault")),
        Ok(None),
        "a private state root reads none"
    );
    let unfound = vault_under(false, || {
        Err(aterm_secrets::Error::new("no home directory"))
    });
    assert!(
        unfound
            .as_ref()
            .is_err_and(|why| why.contains("no home directory")),
        "the account's own state with no vault to be found reads none and says why: \
         {unfound:?}"
    );
    assert!(
        aterm_types::dirs::runs_under_private_state_root() && state_root_is_private(),
        "PRECONDITION: a test binary runs under its scratch root"
    );
    assert_eq!(vault_here(), Ok(None), "a test reads no vault");
}

/// The window's grants admit by the vault the process reads ([`grants_under`] of the rule's
/// answer, [`vault_under`], as [`window_grants`] reads them of [`vault_here`];
/// ATERM-KI-1105-10): beside a fleet state taken under the vault configured they admit as
/// made, beside one taken under another vault none does, `"*"` included, `CI` set or not, and
/// on the account's own state with no vault to be found none does, as the node job then
/// serves none, and why is said; under a private state root, which reads no vault, they admit
/// as made.
///
/// NEGATIVE CONTROLS (2026-11-05): `grants_under` reading a vault that cannot be found as
/// none (its `Err` arm admitting by [`in_effect`] under no vault, the window's reading before
/// 2026-11-05) fails "on the account's own state with no vault to be found no grant admits";
/// `vault_under` deciding by the environment (as in the rule's own test) fails "beside a
/// fleet taken under another vault no grant admits, `CI` set or not". A `window_grants` that
/// takes its vault from anything but [`vault_here`] fails no test: in a test binary the rule
/// reads no vault, as such a revert would.
#[cfg(unix)]
#[test]
fn the_windows_grants_admit_only_beside_the_vault_its_fleet_was_taken_under() {
    if !crate::test_alone::alone() {
        return;
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    aterm_secrets::fsx::ensure_private_dir(&state).unwrap();
    aterm_net::fleet_member::write_held(&state, aterm_net::fleet_member::Began::Here, |book| {
        book.write_key(&[0x5a; 24]).expect("write fleet key");
    })
    .expect("write_held");
    let vault = aterm_secrets::client::Paths::for_home(root.path());
    let configure = |name: &str, signer: u8| {
        aterm_secrets::fsx::ensure_private_dir(&vault.dir).unwrap();
        aterm_secrets::fsx::write_atomic(
            &vault.dir.join("source.toml"),
            format!(
                "url = \"http://127.0.0.1:9/v1/{name}\"\nname = \"{name}\"\nsigner = \"{}\"\n\
                 host = \"m22\"\n",
                aterm_secrets::b64::encode_padded(&[signer; 32])
            )
            .as_bytes(),
            0o600,
        )
        .unwrap();
    };
    configure("alab", 1);
    aterm_net::fleet_member::write_held(&state, aterm_net::fleet_member::Began::Here, |book| {
        book.keep_vault(Some(&format!(
            "# take 01010101010101010101010101010101\nname 616c6162\nkey {}\n",
            aterm_codec::hex::encode_lower(&[1; 32])
        )))
    })
    .unwrap()
    .unwrap();
    assert!(
        aterm_net::fleet_member::read_vault(&state)
            .unwrap()
            .is_some(),
        "PRECONDITION: the fleet state records the vault \"alab\""
    );
    let all = || {
        vec![
            aterm_net::fleet::DriveGrant::from_word(aterm_net::fleet::ALL_MEMBERS)
                .expect("the default grant"),
        ]
    };
    let found = || Ok(vault.clone());
    let unfound = || Err(aterm_secrets::Error::new("no home directory"));
    /// `grants` beside `state` under the vault the rule names for `private_root` and `live`.
    fn under(
        grants: Vec<aterm_net::fleet::DriveGrant>,
        state: Option<&std::path::Path>,
        private_root: bool,
        live: impl FnOnce() -> aterm_secrets::Result<aterm_secrets::client::Paths>,
    ) -> (Vec<aterm_net::fleet::DriveGrant>, Option<String>) {
        let vault = vault_under(private_root, live);
        grants_under(grants, state, vault.as_ref().map(Option::as_ref))
    }
    let (admitted, said) = under(all(), Some(&state), false, found);
    assert_eq!(
        (admitted.len(), said),
        (1, None),
        "the grant made under the vault the fleet was taken under admits"
    );
    let (admitted, said) = under(all(), Some(&state), false, unfound);
    assert_eq!(
        admitted,
        Vec::new(),
        "on the account's own state with no vault to be found no grant admits"
    );
    assert!(
        said.as_deref()
            .is_some_and(|why| why.contains("no home directory")),
        "and why is said: {said:?}"
    );
    assert_eq!(
        under(Vec::new(), Some(&state), false, unfound),
        (Vec::new(), None),
        "with no grant there is nothing to say"
    );
    assert_eq!(
        under(all(), None, false, unfound).0,
        Vec::new(),
        "nor with no state directory"
    );
    configure("other", 9);
    crate::test_env::scoped("CI", "1", || {
        assert_eq!(
            under(all(), Some(&state), false, found).0,
            Vec::new(),
            "beside a fleet taken under another vault no grant admits, `CI` set or not"
        );
    });
    assert_eq!(
        under(all(), Some(&state), false, found),
        (Vec::new(), None),
        "beside a fleet taken under another vault no grant admits"
    );
    assert_eq!(
        under(all(), Some(&state), true, || panic!("reads no vault"))
            .0
            .len(),
        1,
        "under a private state root, which reads no vault, the grants admit as made"
    );
}

/// A `vault-join` is served on the vault its judge read, in the window and the node job alike
/// ([`vault_join_on`]; ATERM-KI-1105-10): a process that reads no vault (under a private
/// state root) refuses it, a judged one is served on that vault, and one its judge refused
/// is refused with the judge's reason.
///
/// NEGATIVE CONTROL (2026-11-05): `vault_join_on` serving a join whose judge read no vault on
/// another vault anyway (the window read `Paths::live()` for it before; here a vault under a
/// scratch path, so the control reads no person's vault) fails "a process that reads no vault
/// refuses a vault-join".
#[cfg(unix)]
#[test]
fn a_vault_join_is_served_only_on_the_vault_its_judge_read() {
    let home = aterm_tempfile::tempdir().unwrap();
    let vault = aterm_secrets::client::Paths::for_home(home.path());
    let judged = aterm_agent::vault_join::Judged {
        node: [7; 32],
        fleet: false,
        book: aterm_secrets::client::Book::default(),
    };
    let refused = vault_join_on(Ok(judged), None);
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("follows no vault")),
        "a process that reads no vault refuses a vault-join: {refused:?}"
    );
    assert_eq!(
        vault_join_on(Ok(judged), Some(vault.clone())),
        Ok((judged, vault.clone())),
        "a judged vault-join is served on the vault its judge read"
    );
    let why = "this Mac has no state directory".to_owned();
    assert_eq!(
        vault_join_on(Err(why.clone()), Some(vault)),
        Err(why),
        "one its judge refused is refused with the judge's reason"
    );
}

/// With MESH off a window still listens, leaving the port only to a node job that holds it;
/// with MESH on it leaves the port to the node service, the one listener on this Mac.
#[test]
fn with_the_mesh_off_a_window_still_listens_and_with_it_on_leaves_the_port_to_the_node() {
    // MESH off, no node job: the window binds the port itself.
    assert_eq!(window_yields_port(false, || false), None);
    // MESH off, a node job holds the port: the window leaves it, as before the mesh.
    assert_eq!(
        window_yields_port(false, || true),
        Some("fleet served by systems.alab.aterm-node")
    );
    // MESH on: the node service is the one listener, held or not; the job is not even asked.
    assert!(
        window_yields_port(true, || unreachable!("no port lock is read"))
            .is_some_and(|line| line.contains("the one listener"))
    );
}

/// A detached book response that passed its early generation check must not publish its
/// old listener endpoint after waiting on the descriptor lock and then being canceled.
#[cfg(unix)]
#[test]
fn an_ended_book_response_keeps_the_current_listener_descriptor() {
    use aterm_net::fleet_member::{hold_book, lock_waiting_told, node_identity, self_entry};

    let state = aterm_tempfile::tempdir().unwrap();
    let me = node_identity(state.path()).unwrap();
    self_entry(
        state.path(),
        &me,
        ("127.0.0.1", 7200, aterm_stamp::app_version()),
    )
    .unwrap();
    let descriptor = state.path().join("fleet-self-entry");
    let committed = std::fs::read(&descriptor).unwrap();
    let ended = Stop::new();
    let within = aterm_spec::wait::MUST_HAPPEN;
    let (waiting, waited) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let held = hold_book(state.path()).unwrap();
        let worker = scope.spawn(|| {
            let mut response = Vec::new();
            let result = lock_waiting_told(within, waiting, || {
                share_book_at(
                    &mut response,
                    state.path(),
                    None,
                    (&[], None),
                    ("127.0.0.1:7100".parse().unwrap(), &ended),
                    &|| true,
                )
            });
            (result, response)
        });
        waited.recv_timeout(within).unwrap();
        ended.set();
        drop(held);
        let (result, response) = worker.join().unwrap();
        assert!(result.is_err());
        assert!(response.is_empty());
        assert_eq!(std::fs::read(descriptor).unwrap(), committed);
    });
}

#[cfg(unix)]
#[test]
fn wildcard_heartbeats_keep_the_automatic_joins_lan_address() {
    let routes = [
        Some("10.0.0.2".parse().unwrap()),
        Some("192.168.1.102".parse().unwrap()),
        Some("2001:db8::1".parse().unwrap()),
    ];
    assert_eq!(
        own_host_from("0.0.0.0:7100".parse().unwrap(), || routes).as_deref(),
        Some("192.168.1.102")
    );
    assert_eq!(
        own_host_from("0.0.0.0:7100".parse().unwrap(), || [None; 3]),
        None
    );
    for address in ["127.0.0.1:7100", "[::1]:7100", "192.168.1.2:7100"] {
        let address: SocketAddr = address.parse().unwrap();
        assert_eq!(
            own_host_from(address, || panic!("an explicit bind needs no route")),
            Some(address.ip().to_string())
        );
    }
}

/// What a Mac whose keyring binds each node as `bound` says holds: a node under that key.
fn held_by(
    bound: impl Fn(&str) -> Option<aterm_net::fleet::NodeKey>,
) -> impl Fn(&str, aterm_net::fleet::NodeKey) -> bool {
    move |node, key| bound(node) == Some(key)
}

/// A drive the membership log admits is held, as its admission was, with no book entry for its
/// node here; a book that binds the node to another key the log admits is that member's, and
/// a key the log took out is held nowhere.
/// NEGATIVE CONTROL: judge by the keyring alone (`bound_in(keys, log, node) == Some(key)`):
/// the log-admitted drive with no book entry is not held, and the cut hangs it up.
#[cfg(unix)]
#[test]
fn a_drive_the_log_admits_is_held_with_no_book_entry_for_its_node() {
    use aterm_net::fleet::NodeKey;
    use aterm_net::fleet_member::{Keyring, MemberKeys, Peer, Standing, Via};
    let (member, other, removed) = (
        NodeKey::from_bytes([0x31; 32]),
        NodeKey::from_bytes([0x32; 32]),
        NodeKey::from_bytes([0x33; 32]),
    );
    let node = "6d".repeat(16);
    let log: MemberKeys = [
        (member, Standing::Member),
        (other, Standing::Member),
        (removed, Standing::Out("removed".to_owned())),
    ]
    .into_iter()
    .collect();
    let none = Keyring::default();
    assert!(
        holds_in(&none, Some(&log), &node, member),
        "a key the log admits, with no book entry for its node"
    );
    assert!(
        !holds_in(&none, None, &node, member),
        "with no log, the book decides"
    );
    assert!(
        !holds_in(&none, Some(&log), &node, removed),
        "a key the log took out"
    );
    let bound_to = |key: NodeKey| {
        Keyring::of(&[Peer {
            node: Some(node.clone()),
            key: Some(key),
            key_from: None,
            host: "m9.local".to_owned(),
            addr: None,
            port: 7100,
            version: None,
            seen_ms: None,
            via: Via::Joined,
            fleet: None,
            signed: None,
        }])
    };
    assert!(
        !holds_in(&bound_to(other), Some(&log), &node, member),
        "the book binds the node to another member's key"
    );
    assert!(
        holds_in(&bound_to(removed), Some(&log), &node, member),
        "the book still binds the node to the key the log took out"
    );
    let star = Serving {
        scope: FleetScope::Content,
        grants: vec![
            aterm_net::fleet::DriveGrant::from_word(aterm_net::fleet::ALL_MEMBERS)
                .expect("the all-member grant"),
        ],
    };
    let driver = aterm_net::fleet::DriveGrant::from_word(&format!("{node}:{}", "31".repeat(32)))
        .expect("the driver's grant");
    assert!(star.serves(
        true,
        Some(&driver),
        &|node, key| holds_in(&none, Some(&log), node, key),
        &[]
    ));

    // Remote holds (`lift_undriving_holds` and `remote_holder`) keep a log-admitted driver's
    // hold while the log admits its key (even with no book entry or an old retired key in the
    // book), and lift it once the log takes its key out or `peers rm` removes a book-only
    // driver.
    assert_eq!(
        bound_or_admitted_in(&none, Some(&log), Some(member), &node),
        Some(member),
        "log-admitted driver with no book entry keeps its bound key for remote holds"
    );
    assert!(still_drives(
        &star.grants,
        || bound_or_admitted_in(&none, Some(&log), Some(member), &node),
        &node,
        &[],
    ));
    assert_eq!(
        bound_or_admitted_in(&bound_to(removed), Some(&log), Some(member), &node),
        Some(member),
        "log-admitted driver whose book entry still holds its retired key keeps its hold"
    );
    assert_eq!(
        bound_or_admitted_in(&bound_to(removed), Some(&log), Some(removed), &node),
        None,
        "a Mac the membership log removed drives no more even while the book names it"
    );
    let founder = NodeKey::from_bytes([0x34; 32]);
    assert_eq!(
        bound_or_admitted_in(&bound_to(founder), Some(&log), Some(founder), &node),
        Some(founder),
        "a book-admitted founder not yet in the log keeps its hold while in the book"
    );
    assert_eq!(
        bound_or_admitted_in(&none, Some(&log), Some(founder), &node),
        None,
        "a book-only driver loses its hold once peers rm removes it from the book"
    );
    // Negative controls: `bound_in` alone drops the log-admitted driver with no book entry,
    // and `keys.bound` alone keeps a Mac the membership log removed.
    assert_eq!(bound_in(&none, Some(&log), &node), None);
    assert_eq!(bound_to(removed).bound(&node), Some(removed));
}

/// A Commons peer heard again after a silence makes one fleet heartbeat due at once, named
/// by its source, between the interval's own; the next look makes none, and the interval's
/// own still comes.
/// NEGATIVE CONTROL: unhook the clock from the signal (a `due` that reads only the interval),
/// or the beacon source from the Commons peers (a look that hears no one): no heartbeat is
/// due before the interval, and the wake reason reads `Timer`.
#[cfg(unix)]
#[test]
fn a_new_commons_beacon_sends_one_heartbeat() {
    use crate::network_moved::tests::{HANG, peer};
    use crate::network_moved::{Heard, Moved, Woke, spawn_source};
    use aterm_types::network_state::Reachability::{LastSeen, Reachable};
    let moved = std::sync::Arc::new(Moved::new());
    let now = std::time::Instant::now();
    let every = Duration::from_secs(3600);
    let mut heartbeat = HeartbeatClock::new(Some(now), every, moved.watch());
    let mut seen = moved.watch();
    let (looks, looked) = std::sync::mpsc::channel();
    let mut heard = Heard::new(1);
    assert!(spawn_source(
        "test-commons",
        std::sync::Arc::clone(&moved),
        (Woke::Beacon, Duration::ZERO),
        move || {
            let peers: Vec<_> = looked.recv().ok()?;
            Some(heard.look(&peers))
        }
    ));
    assert_eq!(
        heartbeat.dispatch_when_free(now, false, || true),
        None,
        "the interval has not run out"
    );
    looks
        .send(vec![peer(7, LastSeen)])
        .expect("the beacon source runs");
    looks
        .send(vec![peer(7, Reachable)])
        .expect("the beacon source runs");
    assert_eq!(seen.wait(HANG), Woke::Beacon);
    assert_eq!(
        heartbeat.dispatch_when_free(now, false, || true),
        Some(Woke::Beacon),
        "one heartbeat at once"
    );
    assert_eq!(
        heartbeat.dispatch_when_free(now, false, || true),
        None,
        "and only one"
    );
    assert_eq!(
        heartbeat.dispatch_when_free(now + every, false, || true),
        Some(Woke::Timer)
    );
}

/// A HEARTBEAT A MOVE MAKES DUE WAITS OUT THIS MAC'S SPREAD (docs/DESIGN-fluid-swarm-2026-10-28.md,
/// F4's follow-up): with ~35 Macs back on the LAN at once, each registers with the issuer at
/// its own offset after the move, not at the move's instant; then once, as before.
/// NEGATIVE CONTROL: a `due` that takes the move by the wall clock (`take()`) or a signal
/// that ignores its spread fails one of the first two: the heartbeat comes before the spread,
/// or not at it.
#[cfg(unix)]
#[test]
fn a_heartbeat_a_move_makes_due_waits_out_this_macs_spread() {
    use crate::network_moved::{Moved, Woke};
    let spread = Duration::from_millis(2_500);
    let moved = std::sync::Arc::new(Moved::spread_by(spread));
    let t0 = std::time::Instant::now() + Duration::from_secs(3600);
    let every = Duration::from_secs(3 * 3600);
    let mut heartbeat = HeartbeatClock::new(Some(t0), every, moved.watch());
    moved.tell_at(Woke::Route, t0);
    assert_eq!(
        heartbeat.dispatch_when_free(t0, false, || true),
        None,
        "not before this Mac's spread"
    );
    assert_eq!(
        heartbeat.dispatch_when_free(t0 + spread, false, || true),
        Some(Woke::Route),
        "at this Mac's spread"
    );
    assert_eq!(
        heartbeat.dispatch_when_free(t0 + spread, false, || true),
        None,
        "and only one"
    );
}

/// A MOVE TOLD WHILE A HEARTBEAT ROUND IS OUT STILL GETS A ROUND: back home alone (no herd
/// spread), the route move starts a round at once, often while the LAN is still down, and
/// the settled place's move comes seconds later while that round is out. It is not taken
/// then (a round could not start), and once the round ends it is due, once.
/// NEGATIVE CONTROL: `dispatch_when_free` taking the move whatever `busy` says answers it
/// while the round is out, where the
/// caller drops it, and "not while a round is out" fails.
#[cfg(unix)]
#[test]
fn a_move_told_while_a_round_is_out_still_gets_a_round() {
    use crate::network_moved::{Moved, Woke};
    let moved = std::sync::Arc::new(Moved::new());
    let t0 = std::time::Instant::now() + Duration::from_secs(3600);
    let every = Duration::from_secs(3 * 3600);
    let mut heartbeat = HeartbeatClock::new(Some(t0), every, moved.watch());
    moved.tell_at(Woke::Place, t0);
    assert_eq!(
        heartbeat.dispatch_when_free(t0, true, || true),
        None,
        "not while a round is out"
    );
    assert_eq!(
        heartbeat.dispatch_when_free(t0 + Duration::from_secs(3), false, || true),
        Some(Woke::Place),
        "once it ended"
    );
    assert_eq!(
        heartbeat.dispatch_when_free(t0 + Duration::from_secs(3), false, || true),
        None,
        "and only one"
    );

    // A free dispatcher can still refuse to start (for example, the OS could
    // not create its worker). No thread exhaustion is needed to judge this
    // production clock/dispatch boundary: refusal is its actual bool result.
    let at = t0 + Duration::from_secs(10);
    moved.tell_at(Woke::Route, at);
    let starts = std::cell::Cell::new(0);
    let refused = heartbeat.dispatch_when_free(at, false, || {
        starts.set(starts.get() + 1);
        false
    });
    assert_eq!(starts.get(), 1, "a real dispatch decision was reached");
    assert_eq!(
        heartbeat.dispatch_when_free(at + Duration::from_secs(1), false, || {
            starts.set(starts.get() + 1);
            true
        }),
        Some(Woke::Route),
        "dispatch refusal must retain the network move for the next free tick"
    );
    assert_eq!(starts.get(), 2);
    assert_eq!(refused, None, "a refused dispatch started no round");
    assert_eq!(
        heartbeat.dispatch_when_free(at + Duration::from_secs(1), false, || {
            panic!("an accepted generation must not dispatch twice")
        }),
        None
    );

    // A newer move arriving inside an accepted dispatch belongs to the next
    // round; acknowledgement may consume only the generation it selected.
    let next = at + Duration::from_secs(2);
    moved.tell_at(Woke::Place, next);
    assert_eq!(
        heartbeat.dispatch_when_free(next, false, || {
            moved.tell_at(Woke::Route, next);
            true
        }),
        Some(Woke::Place)
    );
    assert_eq!(
        heartbeat.dispatch_when_free(next, false, || true),
        Some(Woke::Route)
    );
    assert_eq!(heartbeat.dispatch_when_free(next, false, || true), None);

    // A refused ordinary timer also remains due; failure cannot start a new
    // three-minute interval without a round having started.
    let timer = next + every;
    assert_eq!(heartbeat.dispatch_when_free(timer, false, || false), None);
    assert_eq!(
        heartbeat.dispatch_when_free(timer + Duration::from_secs(1), false, || true),
        Some(Woke::Timer)
    );
    assert_eq!(
        heartbeat.dispatch_when_free(timer + Duration::from_secs(1), false, || true),
        None
    );

    // The timer may start before a new move's herd spread expires. It does
    // not acknowledge that not-yet-due generation.
    let spread = Duration::from_millis(2_500);
    let spread_moves = std::sync::Arc::new(Moved::spread_by(spread));
    let mut delayed = HeartbeatClock::new(Some(t0), every, spread_moves.watch());
    let timer = t0 + every;
    spread_moves.tell_at(Woke::Route, timer);
    assert_eq!(
        delayed.dispatch_when_free(timer, false, || true),
        Some(Woke::Timer)
    );
    assert_eq!(
        delayed.dispatch_when_free(timer + spread, false, || true),
        Some(Woke::Route)
    );
    assert_eq!(
        delayed.dispatch_when_free(timer + spread, false, || true),
        None
    );
}

/// A DRIVE IS NAMED BY ITS PROVEN NODE.
#[cfg(unix)]
#[test]
fn a_drive_is_named_by_the_node_it_proved() {
    let (n, m) = ("1a2b3c4d".repeat(4), "9f8e7d6c".repeat(4));
    let knows = [n.clone(), m.clone()];
    assert_eq!(
        driver_name(&n, Some("mac-11.local"), &knows),
        "mac-11.local (node 1a2b3c4d)"
    );
    assert_ne!(
        driver_name(&n, Some("mac-11.local"), &knows),
        driver_name(&m, Some("mac-11.local"), &knows),
        "a node that names itself as another Mac reads as itself"
    );
    for unshown in [None, Some("the message bus"), Some("")] {
        assert_eq!(
            driver_name(&n, unshown, &knows),
            "node 1a2b3c4d",
            "{unshown:?}"
        );
    }
    // NEGATIVE CONTROL: name the node by its first eight characters again, and the holder
    // reads as the node it shares them with.
    let (holder, reader) = (
        "c0ffee34a18b0000000000000000000a".to_owned(),
        "c0ffee34a27c0000000000000000000b".to_owned(),
    );
    assert_eq!(
        driver_name(&holder, Some("127.0.0.1"), &[holder.clone(), reader]),
        "127.0.0.1 (node c0ffee34a1)",
        "a head another node this Mac knows shares is never the name"
    );
}

/// THE JOIN'S AUDIT LINE NAMES THE MEMBER AS THE BOOK DOES.
#[cfg(unix)]
#[test]
fn a_registration_names_the_member_by_its_fleet_address_and_node() {
    use aterm_net::fleet_member::{Peer, Recorded, Via};
    let member = Peer {
        node: Some("6f860a011103430d".to_owned()),
        key: None,
        key_from: None,
        host: "lab-mini".to_owned(),
        addr: Some(std::net::IpAddr::from([127, 0, 0, 1])),
        port: 58054,
        version: Some("0.105.0".to_owned()),
        seen_ms: Some(1),
        via: Via::Joined,
        fleet: None,
        signed: None,
    };
    for recorded in [Recorded::New, Recorded::Updated] {
        let note = registration_note(recorded, &member);
        assert!(note.contains(" 127.0.0.1:58054 "), "{note}");
        assert!(note.contains("6f860a011103430d"), "{note}");
    }
    assert_ne!(
        registration_note(Recorded::New, &member),
        registration_note(Recorded::Updated, &member)
    );
}

#[cfg(unix)]
#[test]
fn a_held_place_names_the_act_that_moves_the_listener() {
    let addr = SocketAddr::from(([127, 0, 0, 1], 58733));
    let dir = aterm_tempfile::tempdir().expect("scratch folder");
    let state = dir.path().join("state");
    aterm_uds::private_file::ensure_private_dir(&state).unwrap();
    let held = unbound_line(addr, std::io::ErrorKind::AddrInUse, Some(&state));
    assert!(
        held.starts_with("fleet listener: 127.0.0.1:58733 is held by another process")
            && held.contains(": `aterm fleet join --port <n>` moves it; tried again every"),
        "{held}"
    );
    aterm_uds::private_file::write_private(&state, aterm_net::fleet_member::CODE_FILE, b"code\n")
        .unwrap();
    let issuer = unbound_line(addr, std::io::ErrorKind::AddrInUse, Some(&state));
    assert!(
        issuer.contains(": `aterm fleet init --rotate --port <n>` moves it (every member"),
        "{issuer}"
    );
    let absent = unbound_line(addr, std::io::ErrorKind::AddrNotAvailable, Some(&state));
    assert!(
        absent.contains("is not an address of this machine yet") && !absent.contains("moves"),
        "{absent}"
    );
}

#[test]
fn every_move_of_the_listener_is_noted() {
    let mut desk = FleetDesk {
        wanted: None,
        generation: 0,
        revision: 0,
        configured: false,
        started: false,
        thread_spawned: false,
        standing: Standing::Off,
        said: String::new(),
        stop: None,
        mesh: false,
    };
    let at = |place: &str, level: &str| format!("fleet listening on {place} (serving {level})");
    assert!(noted_move(
        &mut desk,
        Standing::Listening,
        &at("127.0.0.1:52510", "facts")
    ));
    assert!(
        !noted_move(
            &mut desk,
            Standing::Listening,
            &at("127.0.0.1:52510", "facts")
        ),
        "a rekey where it was"
    );
    assert!(noted_move(
        &mut desk,
        Standing::Listening,
        &at("127.0.0.1:52506", "facts")
    ));
    assert!(noted_move(
        &mut desk,
        Standing::Listening,
        &at("0.0.0.0:52506", "content")
    ));
    let held = "fleet listener: 0.0.0.0:52506 is held by another process";
    assert!(noted_move(&mut desk, Standing::Retrying, held));
    assert!(
        !noted_move(&mut desk, Standing::Retrying, held),
        "tried again"
    );
    assert_eq!(desk.standing, Standing::Retrying);
}

#[test]
fn the_listener_follows_listen_and_allow() {
    let table = |listen: Option<&str>, allow: Option<&str>| FleetConfig {
        listen: listen.map(str::to_owned),
        allow: allow.map(str::to_owned),
        watch: None,
        drive_from: None,
    };
    assert_eq!(fleet_wanted(None), None);
    assert_eq!(fleet_wanted(Some(&table(None, Some("content")))), None);
    assert_eq!(fleet_wanted(Some(&table(Some(""), None))), None);
    let on = fleet_wanted(Some(&table(Some("0.0.0.0:7100"), None)));
    assert_eq!(
        on,
        Some(Wanted {
            listen: "0.0.0.0:7100".to_owned(),
            allow: None,
            drive_from: Vec::new(),
        })
    );
    assert_ne!(
        on,
        fleet_wanted(Some(&table(Some("0.0.0.0:7100"), Some("content"))))
    );
    assert_ne!(on, fleet_wanted(Some(&table(Some("0.0.0.0:7101"), None))));
}

#[test]
fn the_drive_level_serves_only_the_nodes_its_list_names() {
    let node = "ab".repeat(16);
    let grant = format!("{node}:{}", "cd".repeat(32));
    let wanted = |allow: Option<&str>, from: &[&str]| {
        fleet_wanted(Some(&FleetConfig {
            listen: Some("0.0.0.0:7100".to_owned()),
            allow: allow.map(str::to_owned),
            watch: None,
            drive_from: Some(from.iter().map(|s| (*s).to_owned()).collect()),
        }))
        .expect("a listener")
    };
    assert_eq!(level_of(Some("drive")), Some((FleetScope::Content, true)));
    assert_eq!(level_of(Some("DRIVE")), Some((FleetScope::Content, true)));
    for (allow, scope) in [
        (None, FleetScope::Facts),
        (Some("facts"), FleetScope::Facts),
        (Some("content"), FleetScope::Content),
    ] {
        assert_eq!(level_of(allow), Some((scope, false)), "{allow:?}");
    }
    assert_eq!(level_of(Some("steer")), None);
    let listed = wanted(Some("drive"), &[&node, "mac-03", "", "zz", &grant]);
    assert_eq!(
        listed.drive_from,
        vec![aterm_net::fleet::DriveGrant::from_word(&grant).expect("a grant")],
        "only a node with its key grants"
    );
    assert_eq!(listed.drive_from[0].node, node);
    assert!(
        wanted(Some("drive"), &[]).drive_from.is_empty(),
        "none named: nobody"
    );
}

#[test]
fn a_grant_written_applies_at_once_and_cuts_only_what_it_takes_away() {
    let grant = |n: u8| {
        aterm_net::fleet::DriveGrant::from_word(&format!(
            "{}:{}",
            format!("{n:02x}").repeat(16),
            "cd".repeat(32)
        ))
        .expect("a grant")
    };
    let (a, b) = (grant(0x0a), grant(0x0b));
    let toml = |listen: &str, allow: &str, from: &[&aterm_net::fleet::DriveGrant]| {
        format!(
            "theme = \"Nord\"\n[fleet]\nlisten = \"{listen}\"\nallow = \"{allow}\"\n\
             drive_from = [{}]\n",
            from.iter()
                .map(|grant| format!("\"{}\"", grant.word()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let wanted = |text: &str| table_in(text).expect("a [fleet] that reads");
    let running = wanted(&toml("127.0.0.1:7100", "drive", &[&b]));
    let now = |text: &str| serving_now(running.as_ref(), Some(text));
    let drive = |grants: Vec<aterm_net::fleet::DriveGrant>| Serving {
        scope: FleetScope::Content,
        grants,
    };
    let both = toml("127.0.0.1:7100", "drive", &[&a, &b]);
    assert_eq!(
        now(&both),
        drive(vec![a.clone(), b.clone()]),
        "a grant added drives at once"
    );
    assert_eq!(
        now(&toml("127.0.0.1:7100", "DRIVE", &[&b])),
        drive(vec![b.clone()])
    );
    assert_eq!(
        serving_now(
            wanted(&toml("127.0.0.1:7100", "content", &[])).as_ref(),
            Some(&both)
        ),
        drive(vec![a.clone(), b.clone()]),
        "so does the first, which raises the level to drive"
    );
    assert_eq!(
        now(&toml("127.0.0.1:7100", "facts", &[&a, &b])),
        Serving {
            scope: FleetScope::Facts,
            grants: Vec::new()
        }
    );
    let unrelated = format!(
        "font_px = \"not a number\"\n{}",
        toml("127.0.0.1:7100", "content", &[])
    );
    for gone in [
        toml("127.0.0.1:7100", "content", &[]),
        toml("", "drive", &[&a, &b]),
        toml("127.0.0.1:7100", "steer", &[&a, &b]),
        "theme = \"Nord\"\n".to_owned(),
        String::new(),
        unrelated,
    ] {
        assert_eq!(now(&gone), drive(Vec::new()), "{gone}");
    }
    for broken in [
        "[fleet\n".to_owned(),
        "theme = Nord\n".to_owned(),
        "[fleet]\nallow = 5\n".to_owned(),
    ] {
        assert_eq!(
            now(&broken),
            drive(vec![b.clone()]),
            "a [fleet] that does not read leaves the running grants: {broken}"
        );
    }
    assert_eq!(
        serving_now(running.as_ref(), None),
        drive(vec![b.clone()]),
        "so does an aterm.toml that cannot be read"
    );

    let bound = |node: &str| {
        [&a, &b]
            .into_iter()
            .find(|grant| grant.node == node)
            .map(|grant| grant.key)
    };
    let served = now(&toml("127.0.0.1:7100", "drive", &[&b]));
    assert!(
        served.serves(true, Some(&b), &held_by(bound), &[]),
        "a drive whose grant stands"
    );
    assert!(
        served.serves(true, None, &held_by(bound), &[])
            && served.serves(false, None, &held_by(bound), &[])
    );
    assert!(
        !served.serves(true, Some(&a), &held_by(bound), &[]),
        "a drive whose grant went"
    );
    let facts = now(&toml("127.0.0.1:7100", "facts", &[]));
    assert!(
        !facts.serves(true, None, &held_by(bound), &[])
            && facts.serves(false, None, &held_by(bound), &[])
    );
    assert!(!now(&toml("127.0.0.1:7100", "content", &[])).serves(
        true,
        Some(&b),
        &held_by(bound),
        &[]
    ));

    let place = |listen: &str, allow: &str| wanted(&toml(listen, allow, &[&a]));
    let here = place("127.0.0.1:7100", "content");
    for stays in [
        place("127.0.0.1:7100", "drive"),
        place("127.0.0.1:7100", "facts"),
    ] {
        assert!(in_place(here.as_ref(), stays.as_ref()), "{stays:?}");
    }
    for moves in [
        place("127.0.0.1:7101", "content"),
        place("127.0.0.1:7100", "steer"),
        None,
    ] {
        assert!(!in_place(here.as_ref(), moves.as_ref()), "{moves:?}");
    }
    assert!(!in_place(None, here.as_ref()));
}

#[test]
fn a_node_drives_on_while_a_grant_names_it_with_the_key_it_is_bound_to() {
    let grant = |n: u8, key: u8| {
        aterm_net::fleet::DriveGrant::from_word(&format!(
            "{}:{}",
            format!("{n:02x}").repeat(16),
            format!("{key:02x}").repeat(32)
        ))
        .expect("a grant")
    };
    let (a, b) = (grant(0x0a, 0xcd), grant(0x0b, 0xcd));
    let grants = [a.clone(), b];
    let (key, other) = (a.key, grant(0x0a, 0xef).key);
    assert!(still_drives(&grants, || Some(key), &a.node, &[]));
    assert!(
        !still_drives(&grants, || Some(other), &a.node, &[]),
        "its issuer's book binds it to another key"
    );
    assert!(
        !still_drives(&grants, || None, &a.node, &[]),
        "its issuer's book names it no more"
    );
    assert!(
        !still_drives(
            &grants,
            || unreachable!("no grant names it"),
            &"0c".repeat(16),
            &[],
        ),
        "no grant names it"
    );
    assert!(!still_drives(&[], || Some(key), &a.node, &[]));
    let all = aterm_net::fleet::DriveGrant::from_word(aterm_net::fleet::ALL_MEMBERS)
        .expect("the all-member grant");
    let every = [all.clone()];
    assert!(still_drives(&every, || Some(other), &a.node, &[]));
    assert!(!still_drives(&every, || None, &a.node, &[]));
    let drive = Serving {
        scope: FleetScope::Content,
        grants: vec![all],
    };
    let (c, bound) = (grant(0x0c, 0xef), |node: &str| {
        (node == a.node).then_some(key)
    });
    assert!(drive.serves(true, Some(&a), &held_by(bound), &[]));
    assert!(
        !drive.serves(true, Some(&c), &held_by(bound), &[]),
        "under `*`, a drive of a node no book here binds"
    );
    assert!(
        !drive.serves(true, Some(&a), &|_, key| key == other, &[]),
        "under `*`, a drive admitted under a key its node is bound to no more"
    );
    let named = Serving {
        grants: vec![a.clone()],
        ..drive.clone()
    };
    assert!(named.serves(true, Some(&a), &held_by(bound), &[]));
    assert!(
        !named.serves(true, Some(&a), &|_, _| false, &[]),
        "under its own grant, a drive of a node no book here binds"
    );
    assert!(
        !Serving {
            grants: vec![grant(0x0a, 0xef)],
            ..drive
        }
        .serves(true, Some(&a), &held_by(bound), &[]),
        "a named grant of another key"
    );
}

#[cfg(unix)]
static OPEN_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(unix)]
#[test]
fn a_change_that_takes_a_connection_away_as_it_is_admitted_refuses_or_cuts_it() {
    let _guard = OPEN_TEST_LOCK.lock_or_recover();
    let grant = aterm_net::fleet::DriveGrant::from_word(&format!(
        "{}:{}",
        "5e".repeat(16),
        "cd".repeat(32)
    ))
    .expect("a grant");
    let drive = |grants: Vec<aterm_net::fleet::DriveGrant>| Serving {
        scope: FleetScope::Content,
        grants,
    };
    let entered = |grant: &aterm_net::fleet::DriveGrant| {
        OPEN.lock_or_recover()
            .iter()
            .any(|open| open.driver.as_ref() == Some(grant))
    };
    let (near, _far) = aterm_uds::CtlStream::pair().expect("a line");
    let bound = |_: &str| Some(grant.key);
    let refused = Opened::served(
        true,
        Some(grant.clone()),
        &near,
        || drive(Vec::new()),
        &held_by(bound),
    )
    .expect("held");
    assert!(
        refused.is_none() && !entered(&grant),
        "its grant taken back while its admission read the issuer's book"
    );
    let facts = || Serving {
        scope: FleetScope::Facts,
        grants: Vec::new(),
    };
    assert!(
        Opened::served(true, None, &near, facts, &held_by(bound))
            .expect("held")
            .is_none(),
        "a read at content once it serves facts"
    );
    let open = Opened::served(
        true,
        Some(grant.clone()),
        &near,
        || drive(vec![grant.clone()]),
        &held_by(bound),
    )
    .expect("held")
    .expect("its grant stands");
    assert!(entered(&grant));
    assert!(
        cut_unserved(&drive(Vec::new()), &held_by(bound)) >= 1 && !entered(&grant),
        "taken back once it is held: the cut that follows hangs it up"
    );
    drop(open);
    let all = || {
        drive(vec![
            aterm_net::fleet::DriveGrant::from_word(aterm_net::fleet::ALL_MEMBERS)
                .expect("the all-member grant"),
        ])
    };
    let open = Opened::served(true, Some(grant.clone()), &near, all, &held_by(bound))
        .expect("held")
        .expect("`*` admits a node bound here");
    cut_unserved(&all(), &held_by(bound));
    assert!(
        entered(&grant),
        "`*` serves a node bound here to its key still"
    );
    assert!(
        cut_unserved(&all(), &|_, _| false) >= 1 && !entered(&grant),
        "under `*`, a drive whose node no book binds any more is cut too"
    );
    drop(open);
}

#[test]
fn the_slots_are_a_share_of_the_descriptor_limit() {
    let most = aterm_net::fleet::MAX_SLOTS;
    assert_eq!(slots_for(Some(256), 64), 8, "a Dock-launched app's 256");
    assert_eq!(slots_for(Some(512), 128), most);
    assert_eq!(
        slots_for(None, 1024),
        most,
        "an unreadable limit bounds nothing"
    );
    assert_eq!(slots_for(Some(1 << 20), 8), 8, "the control server's cap");
    assert_eq!(slots_for(Some(16), 8), 1, "a configured listener serves");
}

/// `units=` SAYS WHERE LAN SHARING STANDS, AS THE DOCTOR'S ROW DECIDES IT
/// (`atpkg::peers::Serves`), one word for each place.
#[test]
fn the_units_word_says_where_lan_sharing_stands() {
    use atpkg::peers::Serves;
    let at = SocketAddr::from(([10, 0, 0, 2], atpkg::peers::DEFAULT_PORT));
    let words = [
        Some(Serves::Serving(at)),
        Some(Serves::Idle),
        Some(Serves::NoMember),
        Some(Serves::Off),
        None,
    ]
    .map(units_word);
    assert_eq!(words, ["serving", "idle", "no-member", "off", "-"]);
    assert_eq!(
        units_word(Some(Serves::of(true, Some(at), 3))),
        "off",
        "the person's off stands over a live server"
    );
}

/// `units=` READS NO FILE ON THE EVENT LOOP: one read is out at a time, started once none has
/// landed or the last is [`UNITS_WORD`]'s `every` old; a `status` waits for it until `wait`
/// after it started, and answers a word younger than `stale`, else `-`.
#[test]
fn the_units_word_is_read_off_the_event_loop_one_read_at_a_time() {
    let UnitsTiming { every, wait, stale } = UNITS_WORD;
    let t0 = Instant::now();
    let mut held = UnitsWord::UNREAD;
    assert!(held.ask(t0, every), "none read: the first asks");
    assert!(!held.ask(t0, every), "one read at a time");
    assert_eq!(held.wait_left(t0, wait), Some(wait));
    assert_eq!(held.wait_left(t0 + wait, wait), None, "waited for once");
    assert_eq!(held.answer(t0, stale), "-", "none landed");
    held.landed(Some("serving"), t0);
    assert_eq!(held.wait_left(t0, wait), None, "none out");
    assert!(!held.ask(t0 + every / 2, every));
    assert_eq!(held.answer(t0 + every / 2, stale), "serving");
    assert!(held.ask(t0 + every, every), "a word this old is read again");
    held.landed(None, t0 + every);
    assert!(
        held.ask(t0 + every, every),
        "a read that never ran keeps the word and is asked again"
    );
    assert_eq!(held.answer(t0 + every, stale), "serving");
    assert_eq!(
        held.answer(t0 + stale, stale),
        "-",
        "a word as old as the bound is no answer while its read is out"
    );
    held.landed(Some("off"), t0 + stale);
    assert!(!held.ask(t0 + stale, every));
    assert_eq!(held.answer(t0 + stale, stale), "off");
}

/// A `status` AFTER A QUIET SPELL SAYS WHAT THE STORE HOLDS NOW, NOT THE WORD LAST READ: the
/// read it starts lands before it answers (the person's off over an earlier `serving`).
#[test]
fn a_status_after_a_quiet_spell_answers_the_read_it_started() {
    static DESK: UnitsDesk = UnitsDesk::new();
    DESK.land(Some("serving"));
    let reads = std::sync::atomic::AtomicUsize::new(0);
    let read_off = |desk: &'static UnitsDesk| {
        reads.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || desk.land(Some("off")));
        true
    };
    // The word in hand is already old (`every` zero); the wait is a hang detector, and no
    // answer here is held to a clock.
    let hang = Duration::from_secs(60);
    let quiet = UnitsTiming {
        every: Duration::ZERO,
        wait: hang,
        stale: hang,
    };
    assert_eq!(DESK.word(quiet, read_off), "off");
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    let fresh = UnitsTiming {
        every: hang,
        ..quiet
    };
    assert_eq!(
        DESK.word(fresh, |_| unreachable!("the word is fresh")),
        "off",
        "a fresh word is answered as it stands"
    );
    assert_eq!(
        DESK.word(quiet, |_| false),
        "off",
        "a read that could not start keeps the word while it is young"
    );
}

/// Facts stay on the Mac that made them and go to the fleet issuer its book names, never one
/// known by a name, so neither case reads this host's own names.
#[cfg(unix)]
#[test]
fn facts_push_stores_locally_or_over_fleet_and_rejects_forged_node() {
    use aterm_agent::survey_facts::SwarmSurveyFacts;
    use aterm_net::fleet::{FleetOp, FleetScope, FleetSecret, Listening, NodeIdentity};
    use aterm_net::fleet_member::{
        Began, Peer, Via, node_identity, read_pushed_facts, serve_facts, write_held,
    };

    let secret = FleetSecret::new(&[0x44; 24]).expect("secret");
    let holder_dir = aterm_tempfile::tempdir().expect("holder dir");
    let member_dir = aterm_tempfile::tempdir().expect("member dir");

    let holder_id: NodeIdentity = node_identity(holder_dir.path()).expect("holder node");
    let member_id: NodeIdentity = node_identity(member_dir.path()).expect("member node");

    // 1. The issuer (its book names no issuer but itself) keeps its own facts alone.
    let holder_facts = SwarmSurveyFacts {
        operation_lines: Vec::new(),
        node: Some(holder_id.node().to_owned()),
        label: Some("fixture-holder".to_owned()),
        roster_unread: false,
        version: "0.146.0".to_owned(),
        window: true,
        window_last: Some(1_793_160_000),
        sessions: 2,
        ssh_host_key: Some("SHA256:holderkey".to_owned()),
        accounts: Vec::new(),
        accounts_unread: Vec::new(),
        session_accounts: std::collections::BTreeMap::new(),
        moves: None,
        server_heard: Vec::new(),
        home: None,
        home_free_mb: None,
        repos: None,
        update: None,
    };
    let holder_payload = holder_facts.to_survey_reply(
        100,
        Some("fixture-holder.local"),
        1_793_160_000_000,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    assert_eq!(
        push_facts_to_issuer(holder_dir.path(), &secret, &holder_payload).expect("local store"),
        FactsPushOutcome::StoredLocally(holder_id.node().to_owned()),
    );
    assert_eq!(
        read_pushed_facts(holder_dir.path()),
        vec![(holder_id.node().to_owned(), holder_payload.clone())],
    );

    // 2. A member (`m7`) dials the issuer its book names over `FleetOp::Facts`.
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(Stop::new());
    let holder_state = holder_dir.path().to_path_buf();
    let listening = Listening {
        secret: secret.clone(),
        scope: FleetScope::Facts,
        node: Some(holder_id.clone()),
        serves: SERVES,
        slots: 4,
        per_source: 4,
        lifetime: aterm_spec::wait::MUST_HAPPEN,
    };
    let server_stop = Arc::clone(&stop);
    let server = std::thread::spawn(move || {
        aterm_net::fleet::serve_offering_with_ask_within(
            &listener,
            (
                &listening,
                Arc::new(|| (FleetScope::Facts, SERVES)),
                Arc::new(|| Arc::new(aterm_net::fleet::Denied::default())),
            ),
            None,
            &server_stop,
            aterm_spec::wait::MUST_HAPPEN,
            move |served, op| {
                assert_eq!(op, FleetOp::Facts);
                let proof = served.peer_node().cloned();
                let state = holder_state.clone();
                served.answer(|stream| {
                    let _ = serve_facts(stream, proof.as_ref(), &state, |payload| {
                        SwarmSurveyFacts::from_survey_reply(payload).map(|f| f.node)
                    })?;
                    Ok(())
                })
            },
            |_| {},
        );
    });

    write_held(member_dir.path(), Began::Here, |book| {
        book.write_key(&[0x44; 24]).unwrap();
        book.update(|peers| {
            peers.push(Peer {
                host: "fixture-holder.local".to_owned(),
                addr: Some("127.0.0.1".parse().unwrap()),
                port,
                via: Via::Issuer,
                node: Some(holder_id.node().to_owned()),
                version: Some("0.146.0".to_owned()),
                seen_ms: Some(1_793_160_000_000),
                key: Some(holder_id.key()),
                key_from: None,
                fleet: None,
                signed: None,
            });
            Ok(())
        })
        .unwrap();
    })
    .unwrap();

    let member_facts = SwarmSurveyFacts {
        operation_lines: Vec::new(),
        node: Some(member_id.node().to_owned()),
        label: Some("m7".to_owned()),
        roster_unread: false,
        version: "0.146.0".to_owned(),
        window: false,
        window_last: Some(1_793_150_000),
        sessions: 0,
        ssh_host_key: Some("SHA256:memberkey".to_owned()),
        accounts: Vec::new(),
        accounts_unread: Vec::new(),
        session_accounts: std::collections::BTreeMap::new(),
        moves: None,
        server_heard: Vec::new(),
        home: None,
        home_free_mb: None,
        repos: None,
        update: None,
    };
    let member_payload = member_facts.to_survey_reply(
        200,
        Some("m7.local"),
        1_793_160_000_000,
        0,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    assert_eq!(
        push_facts_to_issuer(member_dir.path(), &secret, &member_payload).expect("remote push"),
        FactsPushOutcome::PushedRemote {
            host: "fixture-holder.local".to_owned(),
            port,
        },
    );

    // The member keeps its own facts besides, for the census it computes (F17).
    assert_eq!(
        read_pushed_facts(member_dir.path()),
        vec![(member_id.node().to_owned(), member_payload.clone())],
    );

    // Negative control 1: forging another node's `node=` on `member_id`'s connection is rejected.
    let mut forged = member_facts;
    forged.node = Some(holder_id.node().to_owned());
    let forged_payload = forged.to_survey_reply(
        200,
        Some("m7.local"),
        1_793_160_000_000,
        0,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    let err = push_facts_to_issuer(member_dir.path(), &secret, &forged_payload)
        .expect_err("forged node id must be refused");
    assert!(err.contains("ERR node-mismatch"), "{err}");

    // Negative control 2: spoofing the listener's own `node_id` with a different Ed25519 key
    // is rejected with `ERR key-mismatch` and never overwrites `holder_dir`'s own facts.
    let spoof_holder =
        NodeIdentity::from_seed(holder_id.node(), &[0x99; 32]).expect("spoof holder id");
    let spoof_err = aterm_net::fleet_member::push_facts_to_peer(
        "fixture-holder.local",
        Some("127.0.0.1".parse().unwrap()),
        port,
        (&secret, Some(&spoof_holder)),
        &forged_payload,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .expect_err("spoofed holder key must be refused");
    assert!(spoof_err.contains("ERR key-mismatch"), "{spoof_err}");

    stop.set();
    let _ = server.join();

    let stored = read_pushed_facts(holder_dir.path());
    assert_eq!(stored.len(), 2);
    assert!(
        stored
            .iter()
            .any(|(n, p)| n == holder_id.node() && p == &holder_payload)
    );
    assert!(
        stored
            .iter()
            .any(|(n, p)| n == member_id.node() && p == &member_payload)
    );
}

/// A local `hold off` ends a remote hold whose node no grant here admits (or that this Mac
/// cannot be driven by at all) while no deny list was ever read, without waiting for one;
/// where the answer turns on the deny list, the hold stands while none was read, and a list
/// that names the node ends it; otherwise the dial judges.
///
/// NEGATIVE CONTROL (2026-11-01): with `holder_after` reading the deny list before it looks at
/// `ready` (the order the review found: an unread list stood every hold, even one no grant
/// admits), this fails (`left: Again(..), right: Gone("it may drive this Mac no more")`).
#[cfg(unix)]
#[test]
fn a_hold_off_ends_an_ungranted_remote_hold_before_any_deny_list_is_read() {
    use crate::fabric::Holder;
    let node = "9a".repeat(16);
    let key = aterm_net::fleet::NodeKey::from_bytes([0xcd; 32]);
    let unread = || None;
    let dialed = |_| -> Holder { panic!("no dial where the deny list decides") };
    assert_eq!(
        holder_after(
            &node,
            Err("it may drive this Mac no more"),
            |k| *k,
            unread,
            dialed
        ),
        Holder::Gone("it may drive this Mac no more".to_owned())
    );
    assert_eq!(
        holder_after(&node, Ok(key), |k| *k, unread, dialed),
        Holder::Again(DENY_LIST_UNREAD.to_owned())
    );
    let naming = || Some(Arc::from(vec![node.clone()]));
    assert_eq!(
        holder_after(&node, Ok(key), |k| *k, naming, dialed),
        Holder::Gone("it is on the swarm roster's deny-list".to_owned())
    );
    assert_eq!(
        holder_after(
            &node,
            Ok(key),
            |k| *k,
            || Some(Arc::from(Vec::new())),
            |_| Holder::Drives
        ),
        Holder::Drives
    );
}

/// A deny list that stays unread says why, once for each new cause and never again for the
/// same one (the read runs per dialer and per tick), and says so once more when a list is
/// read at last; a failed read after that keeps the list and records its new cause once.
///
/// NEGATIVE CONTROL (2026-11-01): with `update_denied_nodes_cache` saying nothing while the
/// list is unread (the shape before: a standing cause looked like "try again", logged at
/// `debug!` where it was logged at all), this fails (`an unread list says why`).
#[cfg(unix)]
#[test]
fn an_unread_deny_list_says_why_once_for_each_cause() {
    use aterm_secrets::client::{SwarmDenialSnapshot as Snapshot, SwarmDenialUnavailable as Why};

    let mut cache = DeniedCache::new();
    let failed = |why: &str| Err(aterm_secrets::Error::new(why.to_owned()));
    let (denied, said) =
        update_denied_nodes_cache(&mut cache, failed(".lock: owned by uid 0, not this user"));
    assert!(denied.is_none());
    let said = said.expect("an unread list says why");
    assert!(
        said.contains(DENY_LIST_UNREAD) && said.contains("owned by uid 0"),
        "{said}"
    );
    assert_eq!(
        update_denied_nodes_cache(&mut cache, failed(".lock: owned by uid 0, not this user")),
        (None, None),
        "the same cause is said once"
    );
    let said = update_denied_nodes_cache(&mut cache, Ok(Snapshot::Unavailable(Why::WriterBusy))).1;
    assert!(
        said.is_some_and(|said| said.contains(&Why::WriterBusy.to_string())),
        "a new cause is said again"
    );
    let (denied, said) = update_denied_nodes_cache(&mut cache, Ok(Snapshot::Available(Vec::new())));
    assert!(denied.is_some());
    assert!(
        said.is_some_and(|said| said.contains("admitted again")),
        "a list read at last is said"
    );
    let (denied, said) = update_denied_nodes_cache(&mut cache, failed("transient"));
    assert!(
        denied.is_some(),
        "a failed read retains the authoritative list"
    );
    assert!(
        said.is_some_and(|said| said.contains("transient")),
        "a new retained-list cause is recorded"
    );
    assert!(
        update_denied_nodes_cache(&mut cache, failed("transient"))
            .1
            .is_none(),
        "a repeated failure is quiet"
    );
    assert!(
        update_denied_nodes_cache(&mut cache, Ok(Snapshot::Available(Vec::new())))
            .1
            .is_some(),
        "recovery is recorded once"
    );
    assert!(
        update_denied_nodes_cache(&mut cache, Ok(Snapshot::Available(Vec::new())))
            .1
            .is_none(),
        "an unchanged current list is quiet"
    );
}

/// No deny list until one was read, then the last one read through a mid-apply or failed
/// read, and an open drive cut once its node is denied.
///
/// NEGATIVE CONTROL (2026-11-01): with `update_denied_nodes_cache` answering an empty list
/// while none was read (the shape before), the first read is a list (`assertion failed:
/// update_denied_nodes_cache(&mut cache, Ok(None)).is_none()`).
#[cfg(unix)]
#[test]
fn denied_nodes_cache_retains_list_while_locked_and_cuts_open_drive() {
    use aterm_secrets::client::{SwarmDenialSnapshot as Snapshot, SwarmDenialUnavailable as Why};

    let _guard = OPEN_TEST_LOCK.lock_or_recover();
    let mut cache = DeniedCache::new();
    // Never read: a mid-apply read and a failed one leave no list, which the handshake reads
    // as unread and refuses every dialer under, never as a list that names no one.
    assert!(
        update_denied_nodes_cache(&mut cache, Ok(Snapshot::Unavailable(Why::WriterBusy)))
            .0
            .is_none()
    );
    let failed = update_denied_nodes_cache(&mut cache, Err(aterm_secrets::Error::new("wide"))).0;
    assert!(failed.is_none());
    assert!(denied_under(failed.as_deref(), None).unread);

    let revoked_node = "9a".repeat(16);
    let good_node = "9b".repeat(16);
    let updated = update_denied_nodes_cache(
        &mut cache,
        Ok(Snapshot::Available(vec![revoked_node.clone()])),
    )
    .0
    .expect("a list read now");
    assert_eq!(&*updated, std::slice::from_ref(&revoked_node));
    assert!(!denied_under(Some(&updated), None).unread);

    // A transient lock or parse error retains the last authoritative deny-list.
    assert_eq!(
        update_denied_nodes_cache(&mut cache, Ok(Snapshot::Unavailable(Why::WriterBusy)))
            .0
            .as_deref(),
        Some(std::slice::from_ref(&revoked_node))
    );
    assert_eq!(
        update_denied_nodes_cache(&mut cache, Err(aterm_secrets::Error::new("transient")))
            .0
            .as_deref(),
        Some(std::slice::from_ref(&revoked_node))
    );

    // Open drive for `revoked_node` is cut as soon as it enters `deny_nodes`, while `good_node`
    // (negative control) remains open under `"*"`.
    let grant_of = |node: &str, key_byte: u8| {
        aterm_net::fleet::DriveGrant::from_word(&format!(
            "{node}:{}",
            format!("{key_byte:02x}").repeat(32)
        ))
        .expect("grant")
    };
    let revoked_grant = grant_of(&revoked_node, 0xcd);
    let good_grant = grant_of(&good_node, 0xef);
    let all = Serving {
        scope: FleetScope::Content,
        grants: vec![
            aterm_net::fleet::DriveGrant::from_word(aterm_net::fleet::ALL_MEMBERS).expect("all"),
        ],
    };
    let bound = |node: &str| {
        if node == revoked_node {
            Some(revoked_grant.key)
        } else if node == good_node {
            Some(good_grant.key)
        } else {
            None
        }
    };
    let (near_revoked, _far_revoked) = aterm_uds::CtlStream::pair().expect("pair");
    let (near_good, _far_good) = aterm_uds::CtlStream::pair().expect("pair");
    let open_revoked = Opened::served_with_denied(
        true,
        Some(revoked_grant.clone()),
        &near_revoked,
        || all.clone(),
        &held_by(bound),
        &[],
    )
    .expect("io")
    .expect("admitted before revocation");
    let open_good = Opened::served_with_denied(
        true,
        Some(good_grant.clone()),
        &near_good,
        || all.clone(),
        &held_by(bound),
        &[],
    )
    .expect("io")
    .expect("admitted good node");

    let cut = cut_unserved_with_denied(&all, &held_by(bound), std::slice::from_ref(&revoked_node));
    assert_eq!(cut, 1, "only the denied node's open drive must be cut");
    let still_open = |grant: &aterm_net::fleet::DriveGrant| {
        OPEN.lock_or_recover()
            .iter()
            .any(|o| o.driver.as_ref() == Some(grant))
    };
    assert!(!still_open(&revoked_grant));
    assert!(still_open(&good_grant));
    drop((open_revoked, open_good));
}
