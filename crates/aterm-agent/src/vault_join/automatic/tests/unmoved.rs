// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const PEER: &str = "192.168.1.40";

fn admits(desk: &Arc<Mutex<Desk>>) -> bool {
    desk.lock_or_recover()
        .local()
        .automatically_admits(PEER.parse().unwrap())
}

/// A look that finds nothing the published member was read from moved (`Backend::unmoved`), at
/// the periodic refresh or after an invalidation (a network move, or a membership change the
/// node's watch saw), keeps that member on its desk and reads no vault: it takes only the home
/// LAN, read again. A look that finds a moved file reads the vault.
///
/// NEGATIVE CONTROL (2026-11-09): with `Shared::run` asking `Backend::unmoved` nothing (each
/// look reads), the two looks read the vault (`no look read the vault`, `left: 3, right: 1`).
#[test]
fn a_look_that_finds_nothing_moved_reads_no_vault() {
    let f = Fixture::home();
    let desk = f.ready_on("home-a");
    assert!(
        admits(&desk),
        "PRECONDITION: the member's home holds the peer"
    );
    assert_eq!(f.fake.reads.load(Ordering::SeqCst), 1);
    // Nothing moved, and the home LAN, read again, is gone.
    *f.fake.unmoved.lock_or_recover() = Some(None);

    *f.fake.advance.lock_or_recover() += REFRESH_EVERY;
    f.handle().shared.changed.notify_all();
    f.waits.recv_timeout(HANG).unwrap();
    f.handle().refresh();
    assert!(
        Arc::ptr_eq(&f.ready_on("home-a"), &desk),
        "the member kept on its desk"
    );
    f.waits.recv_timeout(HANG).unwrap();
    assert_eq!(
        f.fake.reads.load(Ordering::SeqCst),
        1,
        "no look read the vault"
    );
    assert!(!admits(&desk), "the home LAN read again");

    *f.fake.unmoved.lock_or_recover() = None;
    f.handle().refresh();
    f.waits.recv_timeout(HANG).unwrap();
    assert_eq!(
        f.fake.reads.load(Ordering::SeqCst),
        2,
        "a moved file reads it"
    );
    assert!(admits(&f.ready_on("home-a")), "the home the read gave");
}

/// A read an invalidation overtook before it published leaves the snapshot an older read's, so
/// the next look reads the vault again even where nothing moved since that read: a membership
/// change the node's watch saw while the vault was read is never kept from before it.
///
/// NEGATIVE CONTROL (2026-11-09): with `Shared::run` keeping `published` across an overtaken read,
/// the next look keeps the snapshot from before it (`the overtaken read is read again`, `left: 2,
/// right: 3`).
#[test]
fn a_read_an_invalidation_overtook_is_read_again() {
    let f = Fixture::home();
    assert!(admits(&f.ready_on("home-a")), "PRECONDITION");
    let (gate, entered, release) = Gate::pair();
    *f.fake.read_gate.lock_or_recover() = Some(gate);
    // The vault now gives no home; the read under way is of that.
    *f.fake.place.lock_or_recover() = Some(Place {
        network: "home-a",
        home: false,
    });
    f.handle().refresh();
    entered.recv_timeout(HANG).unwrap();
    f.handle().refresh();
    // From here nothing moves, and the place would give the old home again.
    *f.fake.unmoved.lock_or_recover() = Some(Some(aterm_secrets::home_lan::HomeNet::test()));
    release.send(()).unwrap();
    f.waits.recv_timeout(HANG).unwrap();
    assert_eq!(
        f.fake.reads.load(Ordering::SeqCst),
        3,
        "the overtaken read is read again"
    );
    assert!(
        !admits(&f.ready_on("home-a")),
        "the member the vault gives now"
    );
}

static ELSEWHERE: AtomicBool = AtomicBool::new(false);

/// The node's own backend keeps a read that settled while nothing it was read from moves: a
/// vault round's rewrite of `state.toml` (its `heard` time and receipts) moves nothing, a change
/// of what the file holds besides them does, as do a writer holding the vault and a network move.
///
/// NEGATIVE CONTROLS (2026-11-09): with `Vault::unmoved` judging the stamps only by whether they
/// can be taken (`desk_inputs_quiet(..).is_some()`), the state's change is not seen (`a change of
/// the state reads again`); with it taking them as `desk_inputs` (not under the state lock), the
/// writer is given the kept member (`a writer holding the vault meets a fresh read`).
#[test]
fn the_node_keeps_a_settled_read_until_a_file_it_was_read_from_moves() {
    let root = aterm_tempfile::tempdir().unwrap();
    let fixture = client::test_commons_excluded(root.path()).expect("the excluded member");
    let vault = Vault {
        network: || {
            Some(
                if ELSEWHERE.load(Ordering::SeqCst) {
                    "elsewhere"
                } else {
                    "home"
                }
                .to_owned(),
            )
        },
        ..Vault::new(fixture.member.clone())
    };
    assert!(vault.unmoved("home").is_none(), "nothing read yet");
    let (local, network) = vault.read().unwrap().expect("the member");
    assert!(local.whole(), "PRECONDITION: a whole member");
    assert_eq!(local.home_from(), HomeFrom::None, "PRECONDITION");
    assert_eq!(network, "home");
    assert_eq!(vault.unmoved("home"), Some(None), "a settled read stands");
    assert_eq!(
        vault.unmoved("elsewhere"),
        None,
        "a read on another network"
    );

    client::sync(&fixture.member, None).expect("a round that finds nothing new");
    rewrite_state(&fixture.member, &|text| bump("heard", text));
    assert_eq!(
        vault.unmoved("home"),
        Some(None),
        "a round and a heartbeat's rewrite move nothing"
    );

    {
        let _writer = client::test_hold_state_lock(&fixture.member).unwrap();
        assert_eq!(
            vault.unmoved("home"),
            None,
            "a writer holding the vault meets a fresh read"
        );
    }
    assert_eq!(vault.unmoved("home"), Some(None), "the writer gone");

    ELSEWHERE.store(true, Ordering::SeqCst);
    assert_eq!(vault.unmoved("home"), None, "a network move");
    ELSEWHERE.store(false, Ordering::SeqCst);
    assert_eq!(vault.unmoved("home"), Some(None));

    rewrite_state(&fixture.member, &|text| bump("requested", text));
    assert_eq!(
        vault.unmoved("home"),
        None,
        "a change of the state reads again"
    );
    vault.read().unwrap().expect("the member");
    assert_eq!(vault.unmoved("home"), Some(None), "read again, it stands");
    fixture.server.stop();
}

/// Rewrite `paths`' `state.toml` by rename, as every writer of it does, to what `edit` makes of
/// it.
fn rewrite_state(paths: &Paths, edit: &dyn Fn(&str) -> String) {
    let state = paths.dir.join("state.toml");
    let text = std::fs::read_to_string(&state).unwrap();
    let next = state.with_extension("next");
    std::fs::write(&next, edit(&text)).unwrap();
    std::fs::set_permissions(&next, std::fs::metadata(&state).unwrap().permissions()).unwrap();
    std::fs::rename(&next, &state).unwrap();
}

/// `text`, a `state.toml`, with its `key` line 60 higher.
fn bump(key: &str, text: &str) -> String {
    let line = text
        .lines()
        .find(|line| line.starts_with(&format!("{key} = ")))
        .unwrap_or_else(|| panic!("state.toml holds {key}: {text}"));
    let value: i64 = line[key.len() + 3..].parse().unwrap();
    text.replace(line, &format!("{key} = {}", value + 60))
}

fn at_home() -> Option<String> {
    Some("home".to_owned())
}

/// A read during which `state.toml` moved settles nothing, though the file holds what it held
/// before the read once the read is done: the state is stamped by a digest of what it holds
/// (`client::state_standing`), which can go from A to B and back to A, so a member read under B
/// (an apply that failed partway through `Local::of`, its roster unread) is never kept under A's
/// stamps. Here the read itself writes B, and A is written back after it; the next look reads
/// again, and once read with nothing moving, the member stands.
///
/// NEGATIVE CONTROL (2026-11-09): with `Vault::read` settling by the stamps taken before the read
/// alone (no `desk_inputs` taken after it), the read under B stands (`a read during which the
/// state moved is read again`, `left: Some(None), right: None`).
#[test]
fn a_read_during_which_the_state_moved_and_came_back_is_read_again() {
    let root = aterm_tempfile::tempdir().unwrap();
    let fixture = client::test_commons_excluded(root.path()).expect("the excluded member");
    let state = fixture.member.dir.join("state.toml");
    let before = std::fs::read_to_string(&state).unwrap();
    let standing = client::state_standing(&fixture.member).unwrap();
    let moving = Vault {
        network: at_home,
        local: |paths| {
            let local = Local::of(paths)?;
            rewrite_state(paths, &|text| bump("requested", text));
            Ok(local)
        },
        ..Vault::new(fixture.member.clone())
    };
    let (local, _) = moving.read().unwrap().expect("the member");
    assert!(local.whole(), "PRECONDITION: a whole member");
    assert_eq!(local.home_from(), HomeFrom::None, "PRECONDITION");
    assert_ne!(
        client::state_standing(&fixture.member).unwrap(),
        standing,
        "PRECONDITION: the read moved the state"
    );
    rewrite_state(&fixture.member, &|_| before.clone());
    assert_eq!(
        client::state_standing(&fixture.member).unwrap(),
        standing,
        "PRECONDITION: the state holds what it held before the read"
    );
    assert_eq!(
        moving.unmoved("home"),
        None,
        "a read during which the state moved is read again"
    );

    let still = Vault {
        network: at_home,
        ..Vault::new(fixture.member.clone())
    };
    still.read().unwrap().expect("the member");
    assert_eq!(
        still.unmoved("home"),
        Some(None),
        "CONTROL: a read nothing moved during stands"
    );
    fixture.server.stop();
}

/// The node's own backend settles only a read it may keep: a member every read of which answered
/// (`Local::whole`), whose home LAN no vault file names with this Mac's route (`HomeFrom::Vault`,
/// a debug build's lab vault). A member a fault shaped, and a lab's, are read again at the next
/// look while nothing they were read from moves.
///
/// NEGATIVE CONTROLS (2026-11-09): with `Vault::read` not asking `Local::whole`, the member a fault
/// shaped stands (`a member a fault shaped is read again`, `left: Some(None), right: None`); with
/// it not asking `HomeFrom`, the lab's stands (`a lab vault's home is read with its vault`, `left:
/// Some(None), right: None`).
#[test]
fn the_node_keeps_no_read_a_fault_shaped_and_no_labs_home() {
    let root = aterm_tempfile::tempdir().unwrap();
    let fixture = client::test_commons_excluded(root.path()).expect("the excluded member");
    let shaped = Vault {
        network: at_home,
        local: |paths| Ok(Local::of(paths)?.map(|local| local.with_whole(false))),
        ..Vault::new(fixture.member.clone())
    };
    let (local, _) = shaped.read().unwrap().expect("the member");
    assert!(!local.whole(), "PRECONDITION: a member a fault shaped");
    assert_eq!(
        shaped.unmoved("home"),
        None,
        "a member a fault shaped is read again"
    );

    let vault = Vault {
        network: at_home,
        ..Vault::new(fixture.member.clone())
    };
    vault.read().unwrap().expect("the member");
    assert_eq!(
        vault.unmoved("home"),
        Some(None),
        "CONTROL: a whole member's read stands"
    );
    let cidr = aterm_secrets::members::lab_lan::Cidr::parse("192.168.64.0/24").unwrap();
    aterm_secrets::members::lab_lan::write(&fixture.member, &cidr).unwrap();
    let (local, _) = vault.read().unwrap().expect("the member");
    assert!(local.whole(), "PRECONDITION: a whole member");
    assert_eq!(
        local.home_from(),
        HomeFrom::Vault,
        "PRECONDITION: a lab vault's home"
    );
    assert_eq!(
        vault.unmoved("home"),
        None,
        "a lab vault's home is read with its vault"
    );
    fixture.server.stop();
}
