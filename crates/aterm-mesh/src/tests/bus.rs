// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `bus.log` in the node service (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 C2, §4.11): the
//! host opens it where the broker job kept it, under the root's mint secret, and serves it on the
//! same socket; a neighbour's `Bus` stream for it carries the ring of §4.11, so a peer cannot
//! forge a sender; the revocation pass covers it, so a removed peer's stream ends and a voided
//! removal's peer streams again with no restart; the keeper opens, closes and reopens it as the
//! fabric says. Every Mac here is B7a's in-process Mac (`super::Mac`), every wait the one-minute
//! hang detector.

use super::{HANG, Lab, Mac, lab_of, lock, sharing_principal};
use crate::Standings;
use crate::logs::{
    BUS_LOG, BUS_SOCKET, BusHost, BusKeeper, BusKeeperConfig, BusSlot, BusSpec, UNBOUND_RING,
};
use astream_broker::proto::{ERR_ORIGIN_REFUSED, ERR_UNAUTHORIZED};
use aterm_link::durability::Durability;
use aterm_link::enable::{BusHosting, RetiredJob};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The fleet every test's bridge would serve.
const FLEET: &str = "f1";

/// A fabric root: a short private folder (its socket path fits `sun_path`) holding a mint
/// secret derived from `tag`.
struct Root {
    dir: aterm_tempfile::TempDir,
    secret: [u8; 32],
}

impl Root {
    fn new(tag: u8) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("ab")
            .tempdir_in("/tmp")
            .expect("a scratch fabric root");
        aterm_uds::private_file::ensure_private_dir(dir.path()).expect("a private root");
        let secret = [tag ^ 0x5a; 32];
        aterm_uds::private_file::write_private(
            dir.path(),
            aterm_link::enable::MINT_SECRET,
            &secret,
        )
        .expect("the mint secret");
        Self { dir, secret }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn spec(&self) -> BusSpec {
        BusSpec {
            root: self.path().to_path_buf(),
            fleet: FLEET.to_owned(),
            tier: Durability::Ordered,
            backups: aterm_uds::backup::Backups::INERT,
        }
    }

    /// A local client of the log served in this root, holding `grant` minted under `secret`.
    fn local(
        &self,
        secret: &[u8],
        grant: &str,
    ) -> std::io::Result<astream_broker::Client<std::os::unix::net::UnixStream>> {
        let cap = astream_broker::mint(secret, grant).expect("a grant");
        let mut client = astream_broker::Client::connect(self.path().join(BUS_SOCKET))?;
        client.attach(&cap.filter, &cap.tag)?;
        Ok(client)
    }
}

/// `mac`'s `bus.log`, hosted in `root` and joined to its mesh.
fn host_bus(mac: &Mac, root: &Root) -> Arc<BusSlot> {
    let slot = Arc::new(BusSlot::default());
    slot.set(Some(Arc::new(
        BusHost::open(root.spec()).expect("bus.log opens"),
    )));
    mac.mesh().join_bus(&slot);
    slot
}

/// The link `from` holds to `to`.
fn link_to(from: &Mac, to: &Mac) -> Arc<aterm_net::fleet::link::Link> {
    from.run
        .as_ref()
        .expect("runs")
        .table
        .links()
        .into_iter()
        .find(|link| *link.peer().key.bytes() == to.key)
        .expect("a link to the peer")
}

/// A client of `to`'s `bus.log` over `from`'s link, holding the ring `to` handed `from`.
fn bus_stream(
    from: &Mac,
    to: &Mac,
) -> std::io::Result<(
    astream_broker::AnyClient,
    astream_broker::Closer,
    Vec<astream_broker::Capability>,
)> {
    let (stream, grants) = crate::logs::dial_bus(&link_to(from, to))?;
    let link: Arc<dyn astream_broker::StreamLink> = Arc::new(stream);
    link.set_read_timeout(Some(HANG))?;
    let (mut client, closer) = astream_broker::connect_stream(link);
    client.attach_all(grants.iter().map(|cap| (cap.filter.as_str(), &cap.tag[..])))?;
    Ok((client, closer, grants))
}

fn pid(principal: &str) -> u64 {
    astream_broker::producer_id_of(principal)
}

/// A mail lane to `dst`'s session `s-1` from `src`.
fn lane(dst: &str, src: &str) -> String {
    format!("/f/{FLEET}/in/{dst}/s-1/{src}/note")
}

/// The broker refusal's code of `result`, which must be refused.
fn refusal<T: std::fmt::Debug>(result: std::io::Result<T>) -> u8 {
    let error = result.expect_err("refused");
    astream_broker::broker_refusal(&error)
        .unwrap_or_else(|| panic!("a broker refusal, not {error}"))
        .code
}

/// Every payload `root`'s log holds under `filter`, read by a local client.
fn held(root: &Root, filter: &str) -> Vec<String> {
    let mut client = root
        .local(&root.secret, &format!("ro:{filter}"))
        .expect("a local reader");
    let (page, _) = client.fetch(0, filter, 1024).expect("a page");
    page.into_iter()
        .map(|(_, _, body)| super::payload_of(&body))
        .collect()
}

/// The host opens `bus.log` where the broker job kept it: in the root, sealed at rest (a
/// `rest.key` minted beside it, as `aterm link broker` minted one beside a mint secret), guarded
/// by the root's mint secret, retained and rate limited by aterm's rules, with its health file,
/// and served on `bus.sock`, so a local client's cap minted under the secret works as before and
/// one minted under another secret does not. A second open of a log the host holds is refused,
/// and once the host shuts, the log opens again.
#[test]
fn bus_log_is_hosted_where_the_broker_job_kept_it() {
    let root = Root::new(1);
    let host = BusHost::open(root.spec()).expect("bus.log opens");
    let log = root.path().join(BUS_LOG);
    assert!(
        root.path().join(aterm_link::at_rest::REST_KEY).exists(),
        "a rest.key is minted"
    );
    assert!(
        aterm_link::at_rest::is_sealed(&log),
        "bus.log is sealed at rest"
    );
    assert!(host.broker().rate_limit().is_some(), "it is rate limited");
    assert!(
        astream_broker::health_path(&log).exists(),
        "its health file is kept"
    );

    let mut writer = root
        .local(&root.secret, &format!("rw:/f/{FLEET}/>"))
        .expect("a cap minted under the mint secret attaches");
    writer
        .publish(
            7,
            1,
            &lane("n-0000000000000001", "n-0000000000000002"),
            b"hello",
        )
        .expect("a local post lands");
    assert_eq!(held(&root, &format!("/f/{FLEET}/in/>")), ["hello"]);
    let forged = root.local(&[9; 32], &format!("rw:/f/{FLEET}/>"));
    assert!(
        forged.is_err(),
        "a cap minted under another secret is refused"
    );

    let again = BusHost::open(root.spec()).expect_err("a held log is refused");
    assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists, "{again}");
    drop(writer);
    host.shut();
    drop(host);
    let reopened = BusHost::open(root.spec()).expect("the log opens again once shut");
    assert_eq!(held(&root, &format!("/f/{FLEET}/in/>")), ["hello"]);
    drop(reopened);
}

/// A peer's stream to `bus.log` posts mail only as itself and only to this Mac: its own lane lands;
/// a lane naming another Mac, a person or this Mac as sender, its own mail addressed to a third
/// Mac, its own lane under another producer, a presence row and a halt row are refused; a grant bound to another peer does not attach on its stream.
/// A halt another Mac signed is relayed; one whose signature does not verify, or numbered under
/// the relaying peer's producer, is refused.
#[test]
fn a_peer_cannot_forge_a_sender() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 3);
    let (a, b, c) = (&macs[0], &macs[1], &macs[2]);
    let root = Root::new(2);
    let _slot = host_bus(b, &root);
    let (mut client, _closer, _) = bus_stream(a, b).expect("B serves A's bus.log stream");

    client
        .publish(
            pid(&a.principal),
            1,
            &lane(&b.principal, &a.principal),
            b"from a",
        )
        .expect("A's own lane lands");
    for (producer, subject, what) in [
        (
            pid(&c.principal),
            lane(&b.principal, &c.principal),
            "another Mac's lane",
        ),
        (
            pid("h-owner"),
            lane(&b.principal, "h-owner"),
            "a person's lane",
        ),
        (
            pid(&b.principal),
            lane(&b.principal, &b.principal),
            "this Mac's own lane",
        ),
        (
            pid(&a.principal),
            lane(&c.principal, &a.principal),
            "its own mail to a third Mac",
        ),
        (
            pid(&c.principal),
            lane(&b.principal, &a.principal),
            "its lane as another producer",
        ),
        (
            pid(&a.principal),
            format!("/f/{FLEET}/pub/{}/node/presence", a.principal),
            "a presence row",
        ),
        (
            pid(&a.principal),
            format!("/f/{FLEET}/fleet/h-owner/halt"),
            "a halt row",
        ),
    ] {
        assert_eq!(
            refusal(client.publish(producer, 2, &subject, b"forged")),
            ERR_UNAUTHORIZED,
            "{what} is refused"
        );
    }
    let theirs = astream_broker::mint(
        &root.secret,
        &format!("rw,p={}:/f/{FLEET}/in/*/*/{}/*", c.principal, c.principal),
    )
    .expect("a grant");
    assert!(
        client.attach(&theirs.filter, &theirs.tag).is_err(),
        "a grant bound to another peer does not attach on A's stream"
    );

    let halt = format!("/f/{FLEET}/halt/{}", c.principal);
    let base = astream_broker::incarnation_base(3).expect("an incarnation");
    let signed = astream_broker::origin::seal(&c.seed, &halt, base + 1, b"halt");
    let (_, again) = client
        .relay(pid(&c.principal), base + 1, &halt, &signed)
        .expect("a halt C signed is relayed");
    assert!(!again, "it is new to B");
    let wrong = astream_broker::origin::seal(&[55; 32], &halt, base + 2, b"halt");
    assert_eq!(
        refusal(client.relay(pid(&c.principal), base + 2, &halt, &wrong)),
        ERR_ORIGIN_REFUSED,
        "a halt whose signature does not verify is refused"
    );
    // The relay grant binds the producer to the subject's owner, so this one is refused at the
    // grant, before its signature is read.
    let as_peer = astream_broker::origin::seal(&c.seed, &halt, base + 3, b"halt");
    assert_eq!(
        refusal(client.relay(pid(&a.principal), base + 3, &halt, &as_peer)),
        ERR_UNAUTHORIZED,
        "a halt numbered under the relaying peer's producer is refused"
    );
    let own = lane(&b.principal, &a.principal);
    let mail = astream_broker::origin::seal(&a.seed, &own, base + 4, b"m");
    assert!(
        client
            .relay(pid(&a.principal), base + 4, &own, &mail)
            .is_err(),
        "mail is never relayed, not even the peer's own: it goes one hop"
    );

    assert_eq!(held(&root, &format!("/f/{FLEET}/in/>")), ["from a"]);
    assert_eq!(held(&root, &format!("/f/{FLEET}/halt/>")), ["halt"]);
}

/// NEGATIVE CONTROL of [`a_peer_cannot_forge_a_sender`]: a host opened with [`UNBOUND_RING`] set
/// mints the peer's mail grant over every lane, and the peer's post as another Mac and as a
/// person land, and its mail for a third Mac.
#[test]
fn with_an_unbound_ring_a_peer_forges_a_sender() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 3);
    let (a, b, c) = (&macs[0], &macs[1], &macs[2]);
    let root = Root::new(3);
    UNBOUND_RING.with(|on| on.set(true));
    let slot = host_bus(b, &root);
    UNBOUND_RING.with(|on| on.set(false));
    let (mut client, _closer, _) = bus_stream(a, b).expect("B serves A's bus.log stream");
    client
        .publish(
            pid(&c.principal),
            1,
            &lane(&b.principal, &c.principal),
            b"as c",
        )
        .expect("with the control on, a post as another Mac lands");
    client
        .publish(
            pid("h-owner"),
            1,
            &lane(&b.principal, "h-owner"),
            b"as a person",
        )
        .expect("with the control on, a post as a person lands");
    client
        .publish(
            pid(&a.principal),
            2,
            &lane(&c.principal, &a.principal),
            b"to c",
        )
        .expect("with the control on, mail for a third Mac lands");
    assert_eq!(
        held(&root, &format!("/f/{FLEET}/in/>")),
        ["as c", "as a person", "to c"]
    );
    drop(slot);
}

/// A's live stream of its own halt row on B's `bus.log`, and the removal that ends it.
fn tail_of(a: &Mac, b: &Mac) -> (astream_broker::AnySubscription, astream_broker::Closer) {
    let (client, closer, _) = bus_stream(a, b).expect("B serves A's bus.log stream");
    let tail = client
        .subscribe(0, &format!("/f/{FLEET}/halt/{}", a.principal))
        .expect("A tails its own halt row");
    (tail, closer)
}

/// The live stream `tail` ends, not idles.
fn ends(mut tail: astream_broker::AnySubscription) {
    let deadline = Instant::now() + HANG;
    loop {
        assert!(
            Instant::now() < deadline,
            "the removed key's stream must end, not idle"
        );
        match tail.recv_event() {
            Ok(Some(_)) => {}
            Ok(None) => return,
            Err(e) => {
                assert_ne!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut,
                    "it must end, not idle"
                );
                return;
            }
        }
    }
}

/// B's log takes A out: its live `bus.log` stream ends, a new one is refused at the node
/// service and by the broker itself, and the halts A originated leave the log's current view.
#[test]
fn a_removed_peers_stream_ends() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 2);
    let (a, b) = (&macs[0], &macs[1]);
    let root = Root::new(4);
    let slot = host_bus(b, &root);
    let halt = format!("/f/{FLEET}/halt/{}", a.principal);
    let base = astream_broker::incarnation_base(5).expect("an incarnation");
    {
        let (mut client, _closer, _) = bus_stream(a, b).expect("a stream");
        let signed = astream_broker::origin::seal(&a.seed, &halt, base + 1, b"a halts");
        client
            .relay(pid(&a.principal), base + 1, &halt, &signed)
            .expect("A's halt lands");
    }
    let (tail, _closer) = tail_of(a, b);

    b.read(Standings {
        current: [b.key].into(),
        removed: [a.key].into(),
        ..Standings::default()
    });
    b.follow();
    ends(tail);
    let refused = bus_stream(a, b)
        .err()
        .expect("a removed key's stream is refused");
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{refused}"
    );
    assert!(refused.to_string().contains("out"), "{refused}");
    let host = slot.host().expect("hosted");
    assert!(
        host.broker().is_identity_revoked(&a.key),
        "bus.log revokes A's key"
    );
    assert!(
        host.broker().is_principal_revoked(&a.principal),
        "and its principal"
    );
    let (_ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a pair");
    assert_eq!(
        host.broker()
            .serve_stream(Arc::new(theirs), a.key)
            .err()
            .map(|e| e.kind()),
        Some(std::io::ErrorKind::PermissionDenied),
        "bus.log's broker refuses the key itself"
    );
    let mut reader = root
        .local(&root.secret, &format!("ro:{halt}"))
        .expect("a reader");
    assert!(
        reader.last_all(&halt).expect("the rows").0.is_empty(),
        "a removed Mac's halts are retired"
    );
}

/// A seat whose key comes to share its principal with another current key is contested: its
/// live `bus.log` stream ends and a new one is refused, as a removed key's, and it streams again
/// once the other key leaves, with no restart.
#[test]
fn a_contested_peers_stream_ends_and_resumes_once_it_is_alone() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 2);
    let (a, b) = (&macs[0], &macs[1]);
    let root = Root::new(6);
    let slot = host_bus(b, &root);
    let (tail, _closer) = tail_of(a, b);
    let rival = sharing_principal(&a.key, 7);
    let alone = Standings {
        current: [a.key, b.key].into(),
        ..Standings::default()
    };
    let mut contested = alone.clone();
    contested.current.insert(rival);
    b.read(contested);
    b.follow();
    ends(tail);
    let refused = bus_stream(a, b)
        .err()
        .expect("a contested key's stream is refused");
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{refused}"
    );
    let host = slot.host().expect("hosted");
    assert!(
        host.broker().is_identity_revoked(&a.key),
        "A's key is revoked"
    );
    assert!(
        host.broker().is_principal_revoked(&a.principal),
        "and the principal it shares"
    );

    b.read(alone);
    b.follow();
    assert!(!host.broker().is_identity_revoked(&a.key), "lifted");
    assert!(!host.broker().is_principal_revoked(&a.principal));
    let (mut client, _closer, _) = bus_stream(a, b).expect("A streams again");
    client
        .publish(
            pid(&a.principal),
            1,
            &lane(&b.principal, &a.principal),
            b"alone",
        )
        .expect("A posts again");
    assert_eq!(held(&root, &format!("/f/{FLEET}/in/>")), ["alone"]);
}

/// A voided removal seats A again: B's next pass un-revokes its key on the `bus.log` it already
/// hosts (the same host, never restarted), and A streams and posts again.
#[test]
fn a_voided_removals_peer_streams_again_with_no_restart() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 2);
    let (a, b) = (&macs[0], &macs[1]);
    let root = Root::new(5);
    let slot = host_bus(b, &root);
    let hosted = slot.host().expect("hosted");
    let (tail, _closer) = tail_of(a, b);
    let both = Standings {
        current: [a.key, b.key].into(),
        ..Standings::default()
    };
    b.read(Standings {
        current: [b.key].into(),
        removed: [a.key].into(),
        ..Standings::default()
    });
    b.follow();
    ends(tail);

    b.read(both);
    b.follow();
    let now = slot.host().expect("hosted");
    assert!(
        Arc::ptr_eq(&now, &hosted),
        "the same host serves: nothing restarted"
    );
    assert!(
        !now.broker().is_identity_revoked(&a.key),
        "A's key is un-revoked"
    );
    assert!(
        !now.broker().is_principal_revoked(&a.principal),
        "and its principal"
    );
    let (mut client, _closer, _) = bus_stream(a, b).expect("A streams again");
    client
        .publish(
            pid(&a.principal),
            1,
            &lane(&b.principal, &a.principal),
            b"back",
        )
        .expect("A posts again");
    assert_eq!(held(&root, &format!("/f/{FLEET}/in/>")), ["back"]);
}

/// A host that arrives after the pass (a reopen) takes everything the pass applied before any
/// stream of it is served: the revoked keys and the origin keys, so a relayed halt is judged at
/// once, not refused as not ready.
#[test]
fn a_reopened_bus_log_takes_what_the_pass_applied() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 3);
    let (a, b, c) = (&macs[0], &macs[1], &macs[2]);
    b.read(Standings {
        current: [b.key, c.key].into(),
        removed: [a.key].into(),
        ..Standings::default()
    });
    b.follow();
    let root = Root::new(6);
    let slot = Arc::new(BusSlot::default());
    b.mesh().join_bus(&slot);
    slot.set(Some(Arc::new(
        BusHost::open(root.spec()).expect("bus.log opens"),
    )));
    let host = slot.host().expect("hosted");
    assert!(
        host.broker().is_identity_revoked(&a.key),
        "the reopened log revokes A"
    );
    let (mut client, _closer, _) = bus_stream(c, b).expect("B serves C");
    let halt = format!("/f/{FLEET}/halt/{}", c.principal);
    let base = astream_broker::incarnation_base(7).expect("an incarnation");
    let signed = astream_broker::origin::seal(&c.seed, &halt, base + 1, b"c halts");
    client
        .relay(pid(&c.principal), base + 1, &halt, &signed)
        .expect("the origin keys are installed: C's halt lands");
}

/// What a keeper test's hosting reads: 0 unprovisioned, 1 on, 2 off.
const ON: u8 = 1;
const OFF: u8 = 2;

/// Wait until `done`, poking the keeper; the hang detector bounds it.
fn until(keeper: &BusKeeper, what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + HANG;
    while !done() {
        assert!(Instant::now() < deadline, "{what} within the hang detector");
        keeper.poke();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The keeper hosts `bus.log` while the fabric is on, retiring the broker job before each open,
/// and opens it at the tier the retired job ran at (its retirement records the tier), not first
/// at the default and again once it reads the record; it closes the log when the fabric is turned
/// off (the socket stops answering), opens it again when it is back on, the records kept, and
/// closes it cleanly as it ends, so the log opens again at once.
#[test]
fn the_keeper_hosts_bus_log_while_the_fabric_is_on() {
    let root = Root::new(8);
    let state = Arc::new(AtomicU8::new(ON));
    let retired = Arc::new(AtomicUsize::new(0));
    let opened_before: Arc<Mutex<Vec<bool>>> = Arc::default();
    // The tier record: the guarded default until the job's retirement writes the tier it ran at.
    let strict = Arc::new(AtomicU8::new(0));
    let keeper = {
        let (state, retired, opened_before, recorded, strict) = (
            Arc::clone(&state),
            Arc::clone(&retired),
            Arc::clone(&opened_before),
            Arc::clone(&strict),
            Arc::clone(&strict),
        );
        let sock: PathBuf = root.path().join(BUS_SOCKET);
        BusKeeper::start(BusKeeperConfig {
            root: root.path().to_path_buf(),
            backups: aterm_uds::backup::Backups::INERT,
            hosting: Arc::new(move || match state.load(Ordering::Acquire) {
                ON => BusHosting::On {
                    fleet: FLEET.to_owned(),
                    tier: if recorded.load(Ordering::Acquire) == 1 {
                        Durability::Strict
                    } else {
                        Durability::Ordered
                    },
                },
                OFF => BusHosting::Off,
                _ => BusHosting::Unprovisioned,
            }),
            retire: Some(Arc::new(move || {
                lock(&opened_before).push(sock.exists());
                if retired.fetch_add(1, Ordering::AcqRel) > 0 {
                    return Ok(RetiredJob::Absent);
                }
                strict.store(1, Ordering::Release);
                Ok(RetiredJob::Retired {
                    label: "systems.alab.astream-broker".to_owned(),
                    tier: Some(Durability::Strict),
                })
            })),
            every: HANG,
        })
        .expect("the keeper starts")
    };
    until(&keeper, "bus.log is hosted", || {
        keeper.slot().host().is_some()
    });
    assert_eq!(
        *lock(&opened_before),
        [false],
        "the job is retired before the log opens"
    );
    let first = keeper.slot().host().expect("hosted");
    assert_eq!(
        first.spec().tier,
        Durability::Strict,
        "the first open takes the tier the retired job ran at"
    );
    assert_eq!(
        retired.load(Ordering::Acquire),
        1,
        "the job is retired once, before the one open"
    );
    drop(first);
    let mut writer = root
        .local(&root.secret, &format!("rw:/f/{FLEET}/>"))
        .expect("a local client");
    writer
        .publish(
            7,
            1,
            &lane("n-0000000000000001", "n-0000000000000002"),
            b"kept",
        )
        .expect("a post");
    drop(writer);

    state.store(OFF, Ordering::Release);
    until(&keeper, "bus.log closes", || keeper.slot().host().is_none());
    assert!(
        root.local(&root.secret, &format!("ro:/f/{FLEET}/>"))
            .is_err(),
        "nothing answers on bus.sock while the fabric is off"
    );

    state.store(ON, Ordering::Release);
    until(&keeper, "bus.log is hosted again", || {
        keeper.slot().host().is_some()
    });
    assert_eq!(
        retired.load(Ordering::Acquire),
        2,
        "the job is retired before each open"
    );
    assert_eq!(held(&root, &format!("/f/{FLEET}/in/>")), ["kept"]);

    drop(keeper);
    let after = BusHost::open(BusSpec {
        tier: Durability::Strict,
        ..root.spec()
    })
    .expect("the keeper closed the log as it ended");
    drop(after);
}

/// Where the stand-in of [`a_quit_signal_leaves_every_log_closed_cleanly`] keeps its node state,
/// its fabric root (and the tag of its mint secret), where it writes each log's identity, and
/// whether it closes its logs before it dies.
const QUIT_STATE: &str = "ATERM_TEST_MESH_QUIT_STATE";
const QUIT_ROOT: &str = "ATERM_TEST_MESH_QUIT_ROOT";
const QUIT_TAG: &str = "ATERM_TEST_MESH_QUIT_TAG";
const QUIT_OUT: &str = "ATERM_TEST_MESH_QUIT_OUT";
const QUIT_CLOSES: &str = "ATERM_TEST_MESH_QUIT_CLOSES";

/// The tag of the stand-in's mint secret.
const QUIT_SECRET_TAG: u8 = 10;

/// The seed of the stand-in's node key.
const QUIT_SEED: [u8; 32] = [11; 32];

/// The mesh of the node state in `state`, opened as the node service opens it (registered for
/// the quit), its standings seating this Mac alone (so its leases need no neighbour's word), its
/// brokers naming the boot this thread names.
fn quit_mesh(state: &Path) -> std::io::Result<Arc<crate::Mesh>> {
    let alone = Standings {
        current: [astream_broker::origin::public_key(&QUIT_SEED)]
            .into_iter()
            .collect(),
        ..Standings::default()
    };
    crate::Mesh::open(crate::MeshConfig {
        state_dir: state.to_path_buf(),
        backups: aterm_uds::backup::Backups::INERT,
        clock: aterm_net::fleet::link::ManualClock::new()
            as Arc<dyn aterm_net::fleet::link::LinkClock>,
        wall: Arc::new(std::time::SystemTime::now),
        standings: Arc::new(move || Some(alone.clone())),
        changed: None,
        watch_every: HANG,
        observe: None,
    })
}

/// `bus.log` in `root` as the stand-in hosts it: relaxed, so a log left open moves its
/// generation when another boot opens it.
fn quit_spec(root: &Path) -> BusSpec {
    BusSpec {
        root: root.to_path_buf(),
        fleet: FLEET.to_owned(),
        tier: Durability::Relaxed,
        backups: aterm_uds::backup::Backups::INERT,
    }
}

/// Each log's identity: `tables.log`, `metrics.log`, then `bus.log`.
fn identities(mesh: &crate::Mesh, bus: &BusHost) -> String {
    format!(
        "{}\n{}\n{}\n",
        mesh.broker(crate::MeshLog::Tables).log_identity(),
        mesh.broker(crate::MeshLog::Metrics).log_identity(),
        bus.broker().log_identity()
    )
}

/// The stand-in: the node service's mesh and keeper, as it runs them (each registered for the
/// quit by its own open), a record on every log, each log's identity written, then the quit's
/// close where `QUIT_CLOSES` says so, and death by SIGKILL, which runs no drop.
fn quit_stand_in(state: &Path) -> ! {
    let var = |name: &str| std::env::var_os(name).expect("the stand-in's environment");
    let root = PathBuf::from(var(QUIT_ROOT));
    let tag: u8 = var(QUIT_TAG)
        .to_str()
        .and_then(|t| t.parse().ok())
        .expect("a tag");
    let mesh = quit_mesh(state).expect("the stand-in's mesh opens");
    // A record on each mesh log, so each holds what its identity is read back from.
    let me = mesh.principal().to_owned();
    mesh.publish(crate::MeshLog::Tables, &format!("/t/10/{me}"), b"kept")
        .expect("a table record");
    mesh.publish(
        crate::MeshLog::Metrics,
        &format!("/m/{me}/shared/load"),
        b"kept",
    )
    .expect("a metrics row");
    let keeper = BusKeeper::start(BusKeeperConfig {
        root: root.clone(),
        backups: aterm_uds::backup::Backups::INERT,
        hosting: Arc::new(|| BusHosting::On {
            fleet: FLEET.to_owned(),
            tier: Durability::Relaxed,
        }),
        retire: None,
        every: HANG,
    })
    .expect("the keeper starts");
    until(&keeper, "bus.log is hosted", || {
        keeper.slot().host().is_some()
    });
    mesh.join_bus(keeper.slot());
    let secret = [tag ^ 0x5a; 32];
    let cap = astream_broker::mint(&secret, &format!("rw:/f/{FLEET}/>")).expect("a grant");
    let mut client = astream_broker::Client::connect(root.join(BUS_SOCKET)).expect("a client");
    client.attach(&cap.filter, &cap.tag).expect("attached");
    client
        .publish(
            7,
            1,
            &lane("n-0000000000000001", "n-0000000000000002"),
            b"kept",
        )
        .expect("a post");
    drop(client);
    let host = keeper.slot().host().expect("hosted");
    std::fs::write(var(QUIT_OUT), identities(&mesh, &host)).expect("the identities");
    drop(host);
    if std::env::var_os(QUIT_CLOSES).is_some_and(|v| v == "1") {
        // As `aterm node serve`'s quit hook closes them.
        crate::logs::close_bus_at_quit();
        crate::logs::close_mesh_at_quit();
    }
    aterm_uds::process::kill_self_now()
}

/// THE QUIT LEAVES EVERY LOG CLOSED CLEANLY: a stand-in process runs the node service's mesh
/// and `bus.log` keeper, then does what its orderly quit does (`close_bus_at_quit` and
/// `close_mesh_at_quit` over the registrations its own opens made) and dies with no drop run. Opened again in another boot,
/// every log keeps its identity (no generation moved), and the post on `bus.log` is held: each
/// was flushed and marked closed.
///
/// NEGATIVE CONTROL: the same stand-in dying without the close leaves every log open, and
/// another boot's opening moves each one's generation.
#[test]
fn a_quit_signal_leaves_every_log_closed_cleanly() {
    use std::os::unix::process::ExitStatusExt as _;
    if let Some(state) = std::env::var_os(QUIT_STATE) {
        quit_stand_in(Path::new(&state));
    }
    for closes in [true, false] {
        let state = aterm_tempfile::Builder::new()
            .prefix("aq")
            .tempdir_in("/tmp")
            .expect("a scratch state dir");
        aterm_uds::private_file::ensure_private_dir(state.path()).expect("a private state dir");
        let mut seed = aterm_codec::hex::encode_lower(&QUIT_SEED);
        seed.push('\n');
        aterm_uds::private_file::write_private(
            state.path(),
            aterm_net::fleet_member::NODE_KEY_FILE,
            seed.as_bytes(),
        )
        .expect("the node key");
        aterm_net::fleet_member::node_identity(state.path()).expect("the node identity");
        let root = Root::new(QUIT_SECRET_TAG);
        let out = state.path().join("identities");
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("this test"));
        child
            .args([
                "--exact",
                "tests::bus::a_quit_signal_leaves_every_log_closed_cleanly",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(QUIT_STATE, state.path())
            .env(QUIT_ROOT, root.path())
            .env(QUIT_TAG, QUIT_SECRET_TAG.to_string())
            .env(QUIT_OUT, &out)
            .env(QUIT_CLOSES, if closes { "1" } else { "0" })
            .stdin(aterm_uds::spawnfd::Stdio::null());
        let mut running = child.spawn().expect("the stand-in starts");
        let deadline = Instant::now() + HANG;
        let status = loop {
            if let Some(status) = running.try_wait().expect("the stand-in's status") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = running.kill();
                panic!("closes = {closes}: the stand-in did not end within the hang detector");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // SIGKILL's number, which `kill_self_now` raises.
        const SIGKILL: i32 = 9;
        assert_eq!(
            status.signal(),
            Some(SIGKILL),
            "closes = {closes}: the stand-in died of its kill ({status})"
        );
        let before = std::fs::read_to_string(&out).expect("the stand-in's identities");
        let (after, held) = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    crate::logs::BOOT.with(|boot| boot.set(Some([12; 16])));
                    let mesh = quit_mesh(state.path()).expect("the mesh opens again");
                    let bus = BusHost::open(quit_spec(root.path())).expect("bus.log opens again");
                    let after = identities(&mesh, &bus);
                    let held = held(&root, &format!("/f/{FLEET}/in/>"));
                    bus.shut();
                    mesh.close();
                    (after, held)
                })
                .join()
                .expect("the reopening thread")
        });
        if closes {
            assert_eq!(after, before, "a log the quit closed kept its generation");
            assert_eq!(held, ["kept"], "the post the quit flushed is held");
        } else {
            for (was, now) in before.lines().zip(after.lines()) {
                assert_ne!(
                    was, now,
                    "a log left open moves its generation in another boot"
                );
            }
        }
    }
}

/// §11 C3, held topics: the node service names in this Mac's presence row the topics the newest
/// record on its own topics face names, as its fabric bridge writes it on `bus.log`; with no
/// `bus.log` hosted, or no record yet, none.
/// NEGATIVE CONTROL: another node's topics face, on the same log, names topics this Mac does not
/// hold.
#[test]
fn held_topics_are_what_this_macs_newest_topics_record_names() {
    use aterm_link::federate::{topics_body, topics_face};
    use std::collections::BTreeSet;
    let lab = Lab::new();
    let macs = lab_of(&lab, 1);
    let a = &macs[0];
    assert!(crate::mail::held_topics(a.mesh()).is_empty(), "no bus.log");
    let root = Root::new(61);
    let _slot = host_bus(a, &root);
    assert!(
        crate::mail::held_topics(a.mesh()).is_empty(),
        "nothing named yet"
    );
    let mut bridge = root
        .local(&root.secret, "rw:/f/alab/pub/>")
        .expect("a local writer");
    let set = |topics: &[&str]| -> BTreeSet<String> {
        topics.iter().map(|topic| (*topic).to_owned()).collect()
    };
    let other = topics_face("n-00000000000000ff");
    for (seq, (face, topics)) in (1u64..).zip([
        (topics_face(&a.principal), set(&["build"])),
        (topics_face(&a.principal), set(&["build", "deploy"])),
        (other, set(&["leak"])),
    ]) {
        bridge
            .publish(7, seq, &face, topics_body(seq, &topics).as_bytes())
            .expect("named");
    }
    assert_eq!(crate::mail::held_topics(a.mesh()), ["build", "deploy"]);
}
