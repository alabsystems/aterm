// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for fleet membership join codes, peer book persistence, and loopback join/reach flows.

use super::*;
use crate::fleet::{FleetEvent, Listening, read_key_file, serve_with_handshake};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const HANG: Duration = Duration::from_secs(60);

#[test]
fn mesh_identity_and_book_migration_are_key_derived_and_idempotent() {
    let scratch = State::new("mesh-identity");
    let dir = scratch.dir().to_owned();
    let old = node_identity_with_mesh(&dir, false).unwrap();
    let peer = NodeIdentity::from_seed("0123456789abcdef0123456789abcdef", &[7; 32]).unwrap();
    let entry = Peer::own(&peer, ("peer.local", 7100, "0.154.0"), 1);
    write_private(&dir, PEERS_FILE, book_text(&[entry]).as_bytes()).unwrap();
    let mesh = node_identity_with_mesh(&dir, true).unwrap();
    assert_eq!(mesh.node(), node_principal(old.key()));
    assert_eq!(mesh.key(), old.key());
    assert_eq!(
        node_identity_with_mesh(&dir, true).unwrap().node(),
        mesh.node()
    );
    assert_eq!(
        node_identity_with_mesh(&dir, false).unwrap().node(),
        old.node()
    );
    let peers = read_peers(&dir).unwrap();
    assert_eq!(
        peers[0].node.as_deref(),
        Some(node_principal(peer.key()).as_str())
    );
    assert_eq!(peers[0].key, Some(peer.key()));
    assert!(
        peers[0].signed.is_none(),
        "only the peer can re-sign its new id"
    );
    let aliases = std::fs::read(dir.join("mesh-node-aliases")).unwrap();
    let before = std::fs::read(dir.join(PEERS_FILE)).unwrap();
    assert_eq!(rewrite_mesh_book(&dir).unwrap(), 0);
    assert_eq!(std::fs::read(dir.join(PEERS_FILE)).unwrap(), before);
    assert_eq!(
        std::fs::read(dir.join("mesh-node-aliases")).unwrap(),
        aliases
    );
    let rotated = NodeIdentity::from_seed(old.node(), &[8; 32]).unwrap();
    assert_ne!(node_principal(rotated.key()), mesh.node());
    write_private(
        &dir,
        NODE_KEY_FILE,
        format!("{}\n", "08".repeat(32)).as_bytes(),
    )
    .unwrap();
    assert_eq!(
        node_identity_with_mesh(&dir, true).unwrap().node(),
        node_principal(rotated.key())
    );
    assert!(names_node(
        mesh.node(),
        &format!("node-{}", &mesh.node()[2..10])
    ));
    assert!(is_node_word(&format!("node-{}", mesh.node())));
    assert_eq!(
        node_principal(crate::fleet::NodeKey::from_hex(&"ab".repeat(32)).unwrap()),
        "n-abababababababab"
    );
}

#[test]
fn mesh_grants_cross_versions_only_for_the_verified_key() {
    use crate::fleet::{DriveGrant, NodeKey};
    let old = "0123456789abcdef0123456789abcdef";
    let key = NodeKey::from_hex(&"07".repeat(32)).unwrap();
    let new = node_principal(key);
    for (written, proved) in [(old, new.as_str()), (new.as_str(), old)] {
        let word = format!("{written}:{}", key.hex());
        let grant = DriveGrant::from_word(&word).unwrap();
        assert_eq!(grant.word(), word);
        assert!(grant.admits(proved, key));
        assert!(!grant.admits(proved, NodeKey::from_hex(&"08".repeat(32)).unwrap()));
        // Negative control: comparing ids refuses the mixed-version proof.
        assert_ne!(grant.node, proved);
    }
    assert!(
        !aterm_codec::hex::is_lower(&new, 32),
        "the old parser refuses the principal"
    );
    assert!(NodeIdentity::from_seed(&new, &[7; 32]).is_some());
    assert!(!is_node_id("n-abcdef"));
    assert!(!is_node_id("n-ABABABABABABABAB"));
}

#[path = "fleet_member_tests/self_entry.rs"]
mod self_entry;

#[test]
fn signed_primary_names_keep_temporary_aliases_and_reject_tampering() {
    let node = "0123456789abcdef0123456789abcdef";
    let me = NodeIdentity::from_seed(node, &[7; 32]).unwrap();
    let peer =
        Peer::own(&me, ("192.168.1.2", 7100, "0.148.0"), 10).with_primary_name(&me, Some("m62"));
    let parsed = Peer::parse(&peer.line()).unwrap();
    assert_eq!(parsed.primary_name(), Some("m62"));
    assert!(parsed.answers_to(&temporary_name(node)));
    assert!(is_node_word(&temporary_name(node)));
    assert!(parsed.answers_to("m62"));
    let tampered = Peer::parse(&peer.line().replace("name=m62", "name=m63")).unwrap();
    assert!(!tampered.signature_holds());
    assert_eq!(tampered.primary_name(), None);
    let legacy = peer.line().split(" name=").next().unwrap().to_owned();
    assert!(Peer::parse(&legacy).unwrap().signature_holds());
    let renamed =
        Peer::own(&me, ("192.168.1.2", 7100, "0.148.0"), 11).with_primary_name(&me, Some("studio"));
    assert!(renamed.answers_to(&temporary_name(node)));
    assert!(renamed.answers_to("studio"));
    assert!(!renamed.answers_to("m62"));
}

#[test]
fn conflicting_primary_names_refuse_ambiguous_resolution() {
    let a = NodeIdentity::from_seed("0123456789abcdef0123456789abcdef", &[7; 32]).unwrap();
    let b = NodeIdentity::from_seed("abcdef0123456789abcdef0123456789", &[8; 32]).unwrap();
    let peers = [
        Peer::own(&a, ("192.168.1.2", 7100, "0.148.0"), 10).with_primary_name(&a, Some("studio")),
        Peer::own(&b, ("192.168.1.3", 7100, "0.148.0"), 10).with_primary_name(&b, Some("studio")),
    ];
    assert!(find(&peers, "studio").is_err());
    assert_eq!(find(&peers, &temporary_name(a.node())).unwrap(), Some(0));
    let shadow = Peer::own(&b, (&temporary_name(a.node()), 7100, "0.148.0"), 11);
    assert_eq!(
        find(&[peers[0].clone(), shadow], &temporary_name(a.node())).unwrap(),
        Some(0)
    );
}

#[test]
fn a_signed_name_enriches_the_same_descriptor_after_a_legacy_copy_arrives_first() {
    let me = NodeIdentity::from_seed("0123456789abcdef0123456789abcdef", &[7; 32]).unwrap();
    let plain = Peer::own(&me, ("192.168.1.2", 7100, "0.149.0"), 10);
    let named = plain.clone().with_primary_name(&me, Some("m62"));
    let mut book = vec![plain.clone()];
    assert_eq!(record(&mut book, named.clone()), Ok(Recorded::Updated));
    assert_eq!(book[0].primary_name(), Some("m62"));
    assert!(book[0].answers_to(&temporary_name(me.node())));

    let mut later_plain = plain.clone();
    later_plain.seen_ms = Some(11);
    record(&mut book, later_plain).unwrap();
    assert_eq!(
        book[0].primary_name(),
        Some("m62"),
        "a relay cannot strip the name"
    );
    assert_eq!(book[0].seen_ms, Some(11));

    let conflicting = plain.clone().with_primary_name(&me, Some("other"));
    record(&mut book, conflicting).unwrap();
    assert_eq!(book[0].primary_name(), Some("m62"));

    let newer = Peer::own(&me, ("192.168.1.2", 7100, "0.149.0"), 12);
    record(&mut book, newer).unwrap();
    assert_eq!(
        book[0].primary_name(),
        None,
        "a new signed version may clear a name"
    );
}

#[test]
fn name_enrichment_requires_a_valid_signature_for_the_same_descriptor_version() {
    let me = NodeIdentity::from_seed("0123456789abcdef0123456789abcdef", &[7; 32]).unwrap();
    let plain = Peer::own(&me, ("192.168.1.2", 7100, "0.149.0"), 10);
    let named = plain.clone().with_primary_name(&me, Some("m62"));
    let mut damaged = named.clone();
    damaged.signed.as_mut().unwrap().name.as_mut().unwrap().sig[0] ^= 1;
    let mut changed_endpoint = named;
    changed_endpoint.host = "192.168.1.3".into();
    let older =
        Peer::own(&me, ("192.168.1.2", 7100, "0.149.0"), 9).with_primary_name(&me, Some("old"));
    for incoming in [damaged, changed_endpoint, older] {
        let mut book = vec![plain.clone()];
        record(&mut book, incoming).unwrap();
        assert_eq!(book[0].primary_name(), None);
        assert_eq!(book[0].host, "192.168.1.2");
        assert_eq!(book[0].signed, plain.signed);
    }
}

#[test]
fn a_named_registration_repairs_a_relay_copy_and_survives_a_book_read() {
    let state = State::new("named-registration-enrichment");
    issue(state.dir());
    let me = NodeIdentity::from_seed("0123456789abcdef0123456789abcdef", &[7; 32]).unwrap();
    let seq = 1_760_000_000_000;
    let plain = Peer::own(&me, ("127.0.0.1", 7101, "0.149.0"), seq);
    write_private(state.dir(), PEERS_FILE, book_text(&[plain]).as_bytes()).unwrap();
    let server = Issuing::start(secret(5), state.dir(), issuer(0x11));
    let reg = Registration {
        node: me.node().to_owned(),
        host: "127.0.0.1".into(),
        port: 7101,
        version: "0.149.0".into(),
        key: None,
        signed: None,
    }
    .signed_by(&me, seq)
    .with_primary_name(&me, Some("m62"));
    join_as(server.port, (&secret(5), Some(&me)), &reg)
        .0
        .unwrap();
    assert_eq!(
        read_peers(state.dir()).unwrap()[0].primary_name(),
        Some("m62")
    );
    let book = fetch_book(
        &target_of(&issuer_entry(0x11, server.port)),
        (&secret(5), None),
        (Instant::now() + HANG, HANG),
    )
    .unwrap();
    assert_eq!(find(&book.peers, "m62").unwrap(), Some(0));
    assert_eq!(
        find(&book.peers, &temporary_name(me.node())).unwrap(),
        Some(0)
    );
}

#[test]
fn a_full_book_with_maximum_signed_names_round_trips() {
    let state = State::new("maximum-signed-names");
    let host = "a".repeat(253);
    let version = "+".repeat(64);
    let name = "n".repeat(63);
    let peers: Vec<_> = (0..PEERS_MAX)
        .map(|index| {
            let node = format!("{index:032x}");
            let me = NodeIdentity::from_seed(&node, &[7; 32]).unwrap();
            let mut peer = Peer::own(&me, (&host, u16::MAX, &version), u64::MAX)
                .with_primary_name(&me, Some(&name));
            peer.addr = Some("fe80:ffff:ffff:ffff:ffff:ffff:ffff:ffff".parse().unwrap());
            peer.seen_ms = Some(u64::MAX);
            peer
        })
        .collect();
    let encoded = book_text(&peers);
    assert!(encoded.len() <= PEERS_FILE_MAX);
    write_private(state.dir(), PEERS_FILE, encoded.as_bytes()).unwrap();
    assert_eq!(read_peers(state.dir()).unwrap(), peers);
    let server = Issuing::start(secret(5), state.dir(), issuer(0x11));
    let target = target_of(&issuer_entry(0x11, server.port));
    let book = fetch_book(&target, (&secret(5), None), (Instant::now() + HANG, HANG)).unwrap();
    assert_eq!(book.peers, peers);
}

fn waiting<R>(
    wait: Duration,
    told: Option<std::sync::mpsc::Sender<()>>,
    run: impl FnOnce() -> R,
) -> R {
    TEST_WAIT.with_borrow_mut(|seam| *seam = Some((wait, told)));
    let ran = run();
    TEST_WAIT.with_borrow_mut(|seam| *seam = None);
    ran
}

fn refused_at_once(dir: &Path) -> io::Error {
    waiting(Duration::ZERO, None, || node_identity(dir)).unwrap_err()
}

fn secret(byte: u8) -> FleetSecret {
    FleetSecret::new(&[byte; SECRET_LEN]).unwrap()
}

fn mode(path: &Path, bits: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
}

struct State(PathBuf);

impl State {
    fn new(tag: &str) -> Self {
        use std::os::unix::fs::DirBuilderExt as _;
        let root = std::env::temp_dir().join(format!(
            "aterm-fleet-member-{tag}-{}-{}",
            std::process::id(),
            aterm_uds::rand::hex_token::<4>().unwrap()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        mode(&root, 0o700);
        let state = root.join("state");
        ensure_private_dir(&state).unwrap();
        Self(state)
    }

    fn dir(&self) -> &Path {
        &self.0
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(root) = self.0.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn write_key(dir: &Path, secret: &[u8]) -> Result<(), String> {
    hold_book(dir)?.write_key(secret).map_err(|e| e.to_string())
}

fn keep_copy(dir: &Path, issuer: &str, copies: &[Peer]) -> Result<(), String> {
    hold_book(dir)?
        .keep_issuer_book(Some((issuer, copies)))
        .map_err(|e| e.to_string())
}

fn code() -> JoinCode {
    let mut secret = [0u8; SECRET_LEN];
    for (at, byte) in secret.iter_mut().enumerate() {
        *byte = (at as u8).wrapping_mul(37).wrapping_add(11);
    }
    JoinCode {
        secret,
        hint: Some(("m7.local".to_owned(), 7100)),
    }
}

fn split(text: &str) -> (Vec<char>, String) {
    let body = text.strip_prefix(CODE_PREFIX).unwrap();
    let (chars, issuer) = body.rsplit_once('@').unwrap_or((body, "m7.local:7100"));
    (
        chars.chars().filter(|c| *c != '-').collect(),
        issuer.to_owned(),
    )
}

fn join(chars: &[char], issuer: &str) -> String {
    format!("{CODE_PREFIX}{}@{issuer}", chars.iter().collect::<String>())
}

#[test]
fn a_join_code_reads_back_as_itself() {
    let code = code();
    let bare = code.text();
    assert!(
        bare.starts_with(CODE_PREFIX) && !bare.contains('@'),
        "the code names no member: {bare}"
    );
    assert_eq!(
        JoinCode::parse(&bare),
        Ok(JoinCode {
            hint: None,
            ..code.clone()
        })
    );
    let text = code.text_at(("m7.local", 7100));
    let (chars, issuer) = split(&text);
    assert_eq!(chars.len(), CODE_SYMBOLS);
    assert_eq!(issuer, "m7.local:7100");
    let groups = text[CODE_PREFIX.len()..text.find('@').unwrap()].split('-');
    assert!(groups.clone().all(|group| group.len() == GROUP), "{text}");
    assert_eq!(groups.count(), CODE_SYMBOLS / GROUP);

    assert_eq!(
        JoinCode::parse(&text),
        Ok(code.clone()),
        "an older code's place is a hint"
    );
    assert_eq!(
        JoinCode::parse(&format!("  {}\n", text.to_ascii_uppercase())),
        Ok(JoinCode {
            hint: Some(("M7.LOCAL".to_owned(), 7100)),
            ..code.clone()
        })
    );
    assert_eq!(JoinCode::parse(&join(&chars, &issuer)), Ok(code.clone()));
    let lookalikes: Vec<char> = chars
        .iter()
        .map(|c| match c {
            '0' => 'O',
            '1' => 'l',
            c => *c,
        })
        .collect();
    assert_eq!(
        JoinCode::parse(&join(&lookalikes, &issuer)),
        Ok(code.clone())
    );
    for host in ["10.0.0.7", "fe80::1", "lab-mini_2"] {
        let code = JoinCode {
            hint: Some((host.to_owned(), 7100)),
            ..code.clone()
        };
        assert_eq!(JoinCode::parse(&code.text_at((host, 7100))), Ok(code));
    }
    assert!(
        !format!("{code:?}").contains(&chars.iter().collect::<String>()[..8]),
        "a code's Debug shows no secret"
    );
}

#[test]
fn a_join_code_is_found_wherever_it_sits() {
    let text = code().text_at(("m7.local", 7100));
    let body = &text[CODE_PREFIX.len()..text.find('@').unwrap()];
    let span = |t: &str| -> Option<String> {
        match code_spans(t).as_slice() {
            [one] => Some(t[one.clone()].to_owned()),
            _ => None,
        }
    };
    let spans = |t: &str| code_spans(t).len();
    assert_eq!(span(&text).as_deref(), Some(text.as_str()));
    let pasted = format!("ran aterm fleet join {body}@m7.local:7100, then stopped");
    assert_eq!(span(&pasted), Some(format!("{body}@m7.local:7100")));
    let upper = body.to_ascii_uppercase();
    assert_eq!(span(&format!("({upper})")).as_deref(), Some(upper.as_str()));
    let spaced = format!("ATF1- {}", upper.replace('-', " "));
    assert_eq!(
        span(&format!("typed {spaced}.")).as_deref(),
        Some(spaced.as_str())
    );
    let glued = body.replace('-', "");
    assert_eq!(
        span(&format!("x {glued} y")).as_deref(),
        Some(glued.as_str())
    );

    let (mut chars, _) = split(&text);
    chars[5] = if chars[5] == 'a' { 'b' } else { 'a' };
    let typo: String = chars.iter().collect();
    assert_eq!(spans(&typo), 0, "a typo is no code");
    assert_eq!(spans(&glued[1..]), 0, "35 characters are no code");
    let prose = "the fast fox and a red cat sat by the gate to watch a dog nap on the mat \
                 at dawn and then at night";
    assert_eq!(spans(prose), 0);
}

#[test]
fn every_one_character_typo_and_neighbour_swap_is_refused() {
    let (chars, issuer) = split(&code().text_at(("m7.local", 7100)));
    let mut typos = 0;
    for at in 0..chars.len() {
        for wrong in ALPHABET.iter().map(|b| char::from(*b)) {
            if wrong == chars[at] {
                continue;
            }
            let mut typo = chars.clone();
            typo[at] = wrong;
            assert_eq!(
                JoinCode::parse(&join(&typo, &issuer)),
                Err(CodeError::Typo),
                "character {at} as {wrong}"
            );
            typos += 1;
        }
    }
    assert_eq!(typos, CODE_SYMBOLS * 31);
    for at in 0..chars.len() - 1 {
        if chars[at] == chars[at + 1] {
            continue;
        }
        let mut swapped = chars.clone();
        swapped.swap(at, at + 1);
        assert_eq!(
            JoinCode::parse(&join(&swapped, &issuer)),
            Err(CodeError::Typo),
            "characters {at} and {} swapped",
            at + 1
        );
    }
}

#[test]
fn a_short_code_is_refused_counting_what_was_pasted() {
    let short = JoinCode::parse("atf1-garbage@127.0.0.1:47501").unwrap_err();
    assert_eq!(short, CodeError::Length(7));
    let said = short.to_string();
    assert!(
        said.contains("36 letters and digits after its `atf1-`") && said.contains("not 7"),
        "{said}"
    );
    let said = CodeError::Character(5).to_string();
    assert!(said.contains("character 5 after its `atf1-`"), "{said}");
}

#[test]
fn a_code_that_is_not_whole_is_refused_by_what_is_wrong() {
    let text = code().text_at(("m7.local", 7100));
    let (chars, issuer) = split(&text);
    let cases = [
        (text.replacen("atf1-", "atf2-", 1), CodeError::Prefix),
        (
            join(&chars[1..], &issuer),
            CodeError::Length(CODE_SYMBOLS - 1),
        ),
        (
            join(&[&chars[..], &['7'][..]].concat(), &issuer),
            CodeError::Length(CODE_SYMBOLS + 1),
        ),
        (
            join(&[&chars[..4], &['u'][..], &chars[5..]].concat(), &issuer),
            CodeError::Character(5),
        ),
        (join(&chars, "m7.local"), CodeError::Place("no port")),
        (
            join(&chars, "m7.local:0"),
            CodeError::Place("a port is 1 to 65535"),
        ),
        (
            join(&chars, "m7 local:7100"),
            CodeError::Place("a host is an address or a name of letters, digits, `.`, `-` and `_`"),
        ),
    ];
    for (text, want) in cases {
        assert_eq!(JoinCode::parse(&text), Err(want), "{text:?}");
        let said = want.to_string();
        assert!(
            !said.contains(&chars[..6].iter().collect::<String>()),
            "{said}"
        );
    }
}

#[test]
fn the_key_and_the_node_id_are_written_as_their_readers_read_them() {
    let state = State::new("key");
    write_key(state.dir(), &[0x3e; SECRET_LEN]).unwrap();
    assert_eq!(read_key_file(state.dir()).unwrap(), secret(0x3e));
    let meta = std::fs::metadata(state.dir().join(KEY_FILE)).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert!(
        write_key(state.dir(), &[1; 8]).is_err(),
        "no fleet takes 8 bytes"
    );

    let node = node_id(state.dir()).unwrap();
    assert!(is_node_id(&node), "{node}");
    assert_eq!(node_id(state.dir()).unwrap(), node, "minted once");
    std::fs::write(state.dir().join(NODE_FILE), "not a node\n").unwrap();
    assert!(
        node_id(state.dir()).is_err(),
        "a broken node file is refused, not re-minted"
    );
}

/// A thread at a background class holds the book's lock at the default class and is put back as
/// it lets the lock go; one at a higher class keeps its own, and a hold let go on another thread
/// leaves that thread's class as it is: a holder a loaded Mac starves of a core keeps a person's
/// verb waiting past its wait.
#[cfg(target_os = "macos")]
#[test]
fn a_holder_at_a_background_class_holds_the_books_lock_at_the_default_class() {
    use libc::qos_class_t::{
        QOS_CLASS_BACKGROUND, QOS_CLASS_DEFAULT, QOS_CLASS_USER_INTERACTIVE, QOS_CLASS_UTILITY,
    };
    let ask = |asked: libc::qos_class_t| {
        // SAFETY: sets this thread's own class; takes no pointers and mutates no shared state.
        let rc = unsafe { libc::pthread_set_qos_class_self_np(asked, 0) };
        assert_eq!(rc, 0, "{asked:?}");
    };
    let class = || aterm_uds::qos::requested_class_of_self().map(|(class, _)| class);
    let at = |class: libc::qos_class_t| Some(class as libc::c_uint);
    let state = State::new("lift");
    let (seen, other) = std::thread::scope(|scope| {
        let seen = scope
            .spawn(|| {
                [
                    QOS_CLASS_UTILITY,
                    QOS_CLASS_BACKGROUND,
                    QOS_CLASS_USER_INTERACTIVE,
                ]
                .map(|asked| {
                    ask(asked);
                    let before = class();
                    let held = {
                        let _book = hold_book(state.dir()).unwrap();
                        class()
                    };
                    (before, held, class())
                })
            })
            .join()
            .unwrap();
        let book = scope
            .spawn(|| {
                ask(QOS_CLASS_UTILITY);
                hold_book(state.dir()).unwrap()
            })
            .join()
            .unwrap();
        let other = scope.spawn(move || {
            ask(QOS_CLASS_USER_INTERACTIVE);
            drop(book);
            class()
        });
        (seen, other.join().unwrap())
    });
    assert_eq!(
        seen,
        [
            (
                at(QOS_CLASS_UTILITY),
                at(QOS_CLASS_DEFAULT),
                at(QOS_CLASS_UTILITY)
            ),
            (
                at(QOS_CLASS_BACKGROUND),
                at(QOS_CLASS_DEFAULT),
                at(QOS_CLASS_BACKGROUND)
            ),
            (
                at(QOS_CLASS_USER_INTERACTIVE),
                at(QOS_CLASS_USER_INTERACTIVE),
                at(QOS_CLASS_USER_INTERACTIVE)
            ),
        ],
        "(asked, while holding, after): a background holder runs at the default class"
    );
    assert_eq!(
        other,
        at(QOS_CLASS_USER_INTERACTIVE),
        "a hold let go on another thread leaves that thread's class"
    );
}

/// A taker at a lower class is lifted before it takes the book's lock, so it waits for a held one
/// at the default class too, read off it from outside while it waits: a lift made only once the
/// take succeeded left a moment, between the take and the lift, where it held the lock at its own
/// class, where a thread at background can wait minutes for a core.
#[cfg(target_os = "macos")]
#[test]
fn a_taker_at_a_lower_class_waits_for_the_books_lock_at_the_default_class() {
    use libc::qos_class_t::{QOS_CLASS_DEFAULT, QOS_CLASS_UTILITY};
    let state = State::new("liftwait");
    let held = BookLock::take(state.dir(), Duration::ZERO).unwrap();
    let (told, heard) = std::sync::mpsc::channel();
    let dir = state.dir().to_path_buf();
    let taker = std::thread::spawn(move || {
        // SAFETY: sets this thread's own class; takes no pointers and mutates no shared state.
        let rc = unsafe { libc::pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0) };
        assert_eq!(rc, 0);
        waiting(HANG, Some(told), || hold_book(&dir).map(drop))
    });
    heard
        .recv_timeout(HANG)
        .expect("the taker found the lock held");
    let waited_at = aterm_uds::qos::requested_class_of(&taker).map(|(class, _)| class);
    drop(held);
    taker.join().unwrap().unwrap();
    assert_eq!(
        waited_at,
        Some(QOS_CLASS_DEFAULT as libc::c_uint),
        "a utility-class taker waited for the book's lock at 0x{:x}",
        waited_at.unwrap_or_default()
    );
}

#[test]
fn a_node_key_is_minted_once_and_read_back_as_itself() {
    let state = State::new("nodekey");
    assert!(read_node_identity(state.dir()).unwrap().is_none());
    {
        let _claim = BookLock::take(state.dir(), Duration::ZERO).unwrap();
        assert_eq!(
            refused_at_once(state.dir()).kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(read_node_id(state.dir()).unwrap().is_none());
        assert!(read_node_seed(state.dir()).unwrap().is_none());
    }
    let old_node = node_id(state.dir()).unwrap();
    assert!(
        read_node_identity(state.dir()).unwrap().is_none(),
        "a node id from before keys came: no key until init or join"
    );
    {
        let _claim = BookLock::take(state.dir(), Duration::ZERO).unwrap();
        assert_eq!(
            refused_at_once(state.dir()).kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            read_node_id(state.dir()).unwrap().as_deref(),
            Some(old_node.as_str())
        );
        assert!(read_node_seed(state.dir()).unwrap().is_none());
    }

    let me = node_identity(state.dir()).unwrap();
    assert_eq!(me.node(), node_id(state.dir()).unwrap());
    let file = state.dir().join(NODE_KEY_FILE);
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let text = std::fs::read_to_string(&file).unwrap();
    let digits = text.strip_suffix('\n').unwrap();
    assert!(aterm_codec::hex::is_lower(digits, 64));
    {
        let _claim = BookLock::take(state.dir(), Duration::ZERO).unwrap();
        let read = waiting(Duration::ZERO, None, || node_identity(state.dir()))
            .expect("a minted node reads while another holds the book's lock, never waiting");
        assert_eq!((read.node(), read.key()), (me.node(), me.key()));
        let kept = read_node_identity(state.dir()).unwrap().unwrap();
        assert_eq!((kept.node(), kept.key()), (me.node(), me.key()));
        assert!(std::fs::read(&file).unwrap() == text.as_bytes());
    }
    assert_eq!(
        node_identity(state.dir()).unwrap().key(),
        me.key(),
        "minted once"
    );
    let read = read_node_identity(state.dir()).unwrap().unwrap();
    assert_eq!((read.node(), read.key()), (me.node(), me.key()));

    write_private(state.dir(), NODE_KEY_FILE, b"not a key\n").unwrap();
    assert!(read_node_identity(state.dir()).is_err());
    assert!(
        node_identity(state.dir()).is_err(),
        "a broken key file is refused, not re-minted"
    );
    mode(&file, 0o644);
    assert!(
        read_node_identity(state.dir()).is_err(),
        "a key others can read"
    );
}

#[test]
fn the_state_directory_is_made_private_and_a_symlink_refused() {
    let state = State::new("dir");
    let fresh = state.dir().join("fresh");
    assert_eq!(ensure_private_dir(&fresh).unwrap(), None);
    let meta = std::fs::metadata(&fresh).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o700);

    mode(&fresh, 0o755);
    assert!(
        write_private(&fresh, NODE_FILE, b"x").is_err(),
        "a writer never repairs"
    );
    assert_eq!(ensure_private_dir(&fresh).unwrap(), Some(0o755));
    assert!(write_private(&fresh, NODE_FILE, b"x").is_ok());

    let linked = state.dir().join("linked");
    std::os::unix::fs::symlink(&fresh, &linked).unwrap();
    assert!(ensure_private_dir(&linked).is_err());
    assert!(write_key(&linked, &[2; SECRET_LEN]).is_err());
    assert!(
        !fresh.join(KEY_FILE).exists(),
        "nothing written through the link"
    );
}

fn joined(node: u8, host: &str, ip: [u8; 4], port: u16) -> Peer {
    Peer {
        node: Some(format!("{node:02x}").repeat(16)),
        key: None,
        key_from: None,
        host: host.to_owned(),
        addr: Some(IpAddr::from(ip)),
        port,
        version: Some("0.95.0+g1a2b3c4".to_owned()),
        seen_ms: Some(1_760_000_000_000),
        via: Via::Joined,
        fleet: None,
        signed: None,
    }
}

fn manual(host: &str, port: u16) -> Peer {
    Peer {
        node: None,
        key: None,
        key_from: None,
        host: host.to_owned(),
        addr: host.parse().ok(),
        port,
        version: None,
        seen_ms: None,
        via: Via::Manual,
        fleet: None,
        signed: None,
    }
}

fn copy(peer: Peer) -> Peer {
    Peer {
        via: Via::IssuerBook,
        ..peer
    }
}

fn node_of(byte: u8) -> String {
    format!("{byte:02x}").repeat(16)
}

fn identity(node: &str, seed: u8) -> NodeIdentity {
    NodeIdentity::from_seed(node, &[seed; 32]).unwrap()
}

fn keep_identity(dir: &Path, who: &NodeIdentity, seed: u8) {
    assert_eq!(identity(who.node(), seed).key(), who.key());
    let _lock = BookLock::take(dir, HANG).unwrap();
    write_private(dir, NODE_FILE, format!("{}\n", who.node()).as_bytes()).unwrap();
    write_private(
        dir,
        NODE_KEY_FILE,
        format!("{}\n", aterm_codec::hex::encode_lower(&[seed; 32])).as_bytes(),
    )
    .unwrap();
}

fn proof(node: &str, key: NodeKey) -> NodeProof {
    NodeProof {
        node: node.to_owned(),
        key,
    }
}

fn keyed(node: u8, key: NodeKey, ip: [u8; 4], port: u16) -> Peer {
    Peer {
        key: Some(key),
        ..joined(node, "m7.local", ip, port)
    }
}

#[test]
fn a_peers_key_is_on_its_line_and_an_older_line_binds_none() {
    let peer = keyed(0xab, identity(&node_of(0xab), 1).key(), [10, 0, 0, 7], 7100);
    let line = peer.line();
    assert!(
        line.starts_with(&format!(
            "peer node={} key={} host=",
            node_of(0xab),
            identity(&node_of(0xab), 1).key().hex()
        )),
        "{line}"
    );
    assert_eq!(Peer::parse(&line), Some(peer.clone()));

    let older = line.replace(&format!(" key={}", peer.key.unwrap().hex()), "");
    assert_eq!(
        Peer::parse(&older),
        Some(Peer {
            key: None,
            ..peer.clone()
        })
    );
    assert_eq!(
        Peer::parse(&joined(0xab, "m7.local", [10, 0, 0, 7], 7100).line())
            .unwrap()
            .key,
        None
    );

    let keyless_node = manual("m7", 7100)
        .line()
        .replace("key=-", &format!("key={}", "ab".repeat(32)));
    assert_eq!(Peer::parse(&keyless_node), None, "a key with no node id");

    let moved = Peer {
        key_from: Some(IpAddr::from([10, 0, 0, 8])),
        ..joined(0xab, "m7.local", [10, 0, 0, 66], 7100)
    };
    let moved_line = moved.line();
    assert!(
        moved_line.contains(" key=- key_from=10.0.0.8 host="),
        "{moved_line}"
    );
    assert_eq!(Peer::parse(&moved_line), Some(moved));
    let bound = peer.line().replace(" host=", " key_from=10.0.0.8 host=");
    assert_eq!(Peer::parse(&bound).unwrap().key_from, None, "{bound}");
    let nameless = manual("m7", 7100)
        .line()
        .replace(" host=", " key_from=10.0.0.8 host=");
    assert_eq!(Peer::parse(&nameless).unwrap().key_from, None, "{nameless}");
    assert_eq!(
        Peer::parse(&moved_line.replace("key_from=10.0.0.8", "key_from=m7")),
        None,
        "a key_from that is no address"
    );
    for bad in ["AB".repeat(32), "ab".repeat(31), "zz".repeat(32)] {
        let bad = line.replace(&peer.key.unwrap().hex(), &bad);
        assert_eq!(Peer::parse(&bad), None, "{bad}");
    }
}

#[test]
fn a_peer_is_one_line_and_a_bad_line_is_refused() {
    for peer in [
        joined(0xab, "m7.local", [10, 0, 0, 7], 7100),
        manual("lab-mini", 7101),
        manual("fe80::1", 7100),
        copy(manual("lab-mini", 7102)),
        copy(joined(0xcd, "m8.local", [10, 0, 0, 8], 7100)),
    ] {
        let line = peer.line();
        assert!(!line.contains('\n'));
        assert_eq!(Peer::parse(&line), Some(peer));
    }
    for line in [
        "peer node=- host=m7 addr=- port=7100 version=- seen_ms=- via=joined",
        "peer node=- host=m7 addr=- port=7100 version=- seen_ms=- via=issuer",
        "peer node=xyz host=m7 addr=- port=7100 version=- seen_ms=- via=manual",
        "peer node=- host=m7%0A addr=- port=7100 version=- seen_ms=- via=manual",
        "peer node=- host=m7 addr=- port=0 version=- seen_ms=- via=manual",
        "peer node=- host=m7 addr=- port=7100 version=- seen_ms=- via=sideways",
        "node=- host=m7 port=7100 via=manual",
    ] {
        assert_eq!(Peer::parse(line), None, "{line}");
    }
    let state = State::new("lines");
    let good = manual("m7", 7100).line();
    std::fs::write(
        state.dir().join(PEERS_FILE),
        format!("# a comment\n{good}\n\nnot a peer\n"),
    )
    .unwrap();
    mode(&state.dir().join(PEERS_FILE), 0o600);
    assert!(matches!(read_peers(state.dir()), Err(PeersError::Line(4))));
}

#[test]
fn the_book_is_read_only_when_private() {
    let state = State::new("book");
    let dir = state.dir();
    assert_eq!(
        read_peers(dir).unwrap(),
        Vec::new(),
        "no book is an empty one"
    );
    update_peers(dir, |book| record(book, manual("m7", 7100))).unwrap();
    let file = dir.join(PEERS_FILE);
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(read_peers(dir).unwrap(), vec![manual("m7", 7100)]);

    let refused = |dir: &Path| match read_peers(dir) {
        Err(PeersError::File(e)) => e.to_string(),
        other => panic!("{other:?} should be refused"),
    };
    mode(&file, 0o644);
    assert!(refused(dir).contains("want 0600"));
    mode(&file, 0o600);
    mode(dir, 0o750);
    assert!(refused(dir).contains("want 0700"));
    assert!(
        update_peers(dir, |book| record(book, manual("m8", 7100))).is_err(),
        "nor written into"
    );
    mode(dir, 0o700);

    let second = dir.parent().unwrap().join("second-link");
    std::fs::hard_link(&file, &second).unwrap();
    assert!(refused(dir).contains("another link"));
    std::fs::remove_file(&second).unwrap();

    let real = dir.parent().unwrap().join("real-book");
    std::fs::rename(&file, &real).unwrap();
    std::os::unix::fs::symlink(&real, &file).unwrap();
    assert!(refused(dir).contains("symlink"));
    std::fs::remove_file(&file).unwrap();
    std::fs::rename(&real, &file).unwrap();
    let linked = dir.parent().unwrap().join("linked-state");
    std::os::unix::fs::symlink(dir, &linked).unwrap();
    assert!(refused(&linked).contains("symlink"));
    assert_eq!(read_peers(dir).unwrap(), vec![manual("m7", 7100)]);
}

const LOCK_CHILD: &str = "ATERM_NET_FLEET_LOCK_CHILD";

/// NEGATIVE CONTROL: a leave over a symlinked state directory goes on unlocked to clear
/// config without writing `fleet-off` or unlinking `fleet.key` inside the symlink target.
#[test]
fn a_leave_goes_on_unlocked_only_where_no_edit_could_lock() {
    let state = State::new("leave-lock");
    let dir = state.dir();
    let lock = dir.join(PEERS_LOCK);
    std::fs::create_dir(&lock).unwrap();
    assert_eq!(leave_held(dir, |_| 7), Ok(7), "a lock that is no file");
    std::fs::remove_dir(&lock).unwrap();
    let outside = dir.parent().unwrap().join("outside-state");
    std::fs::create_dir(&outside).unwrap();
    mode(&outside, 0o700);
    std::fs::write(outside.join(KEY_FILE), b"kept").unwrap();
    let linked = dir.parent().unwrap().join("linked-state");
    std::os::unix::fs::symlink(&outside, &linked).unwrap();
    let linked_leave = leave_held(&linked, |leaving| {
        (
            leaving.locked(),
            leaving.write_off().err().map(|e| e.kind()),
            leaving.remove(KEY_FILE).map_err(|e| e.kind()),
            leaving.remove("../outside").map_err(|e| e.kind()),
        )
    });
    assert_eq!(
        linked_leave,
        Ok((
            false,
            Some(io::ErrorKind::PermissionDenied),
            Ok(false),
            Err(io::ErrorKind::InvalidInput),
        )),
        "a symlink"
    );
    assert_eq!(std::fs::read(outside.join(KEY_FILE)).unwrap(), b"kept");
    assert!(!outside.join(OFF_FILE).exists());
    let none = dir.join("none");
    let made = leave_held(&none, |_| {
        let bits = std::fs::metadata(&none).unwrap().permissions().mode() & 0o777;
        let other = BookLock::take(&none, Duration::ZERO)
            .err()
            .map(|e| e.kind());
        (bits, other)
    });
    assert_eq!(
        made,
        Ok((0o700, Some(io::ErrorKind::WouldBlock))),
        "no directory: made as an init makes it, then locked"
    );
    let file = dir.join("a-file");
    std::fs::write(&file, b"x").unwrap();
    let dangling = dir.join("dangling");
    std::os::unix::fs::symlink(dir.join("nowhere"), &dangling).unwrap();
    let unwritable = dir.join("unwritable");
    std::fs::create_dir(&unwritable).unwrap();
    mode(&unwritable, 0o500);
    for (odd, what) in [
        (file.clone(), "a file in its place"),
        (file.join("state"), "beneath a file"),
        (dangling.join("state"), "beneath a link to nothing"),
        (
            unwritable.join("state"),
            "in a directory this user cannot write",
        ),
    ] {
        let left = leave_held(&odd, |leaving| {
            leaving.remove(KEY_FILE).map_err(|e| e.kind())
        });
        assert_eq!(left, Ok(Ok(false)), "{what}: none any writer makes");
        assert!(!odd.is_dir(), "{what}");
    }
    mode(&unwritable, 0o700);
    // `.` hides `gone` from `Path::parent`: the make finds no parent, as one removed under it.
    let gone = dir.join("gone/.");
    let refused = leave_held(&gone, |_| ()).unwrap_err().to_string();
    let shown = gone.display().to_string();
    assert_eq!(refused.matches(&shown).count(), 1, "{refused}");
    mode(dir, 0o750);
    let held = leave_held(dir, |leaving| {
        let bits = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        let other = BookLock::take(dir, Duration::ZERO).err().map(|e| e.kind());
        (leaving.narrowed(), bits, other)
    });
    assert_eq!(
        held,
        Ok((Some(0o750), 0o700, Some(io::ErrorKind::WouldBlock))),
        "a directory open to others, narrowed as an edit narrows it, then locked"
    );
    let other = BookLock::take(dir, Duration::ZERO).unwrap();
    mode(dir, 0o750);
    let refused = waiting(Duration::ZERO, None, || leave_held(dir, |_| ()))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("(750), and is 0700 now"), "{refused}");
    let shown = dir.display().to_string();
    assert_eq!(refused.matches(&shown).count(), 1, "{refused}");
    drop(other);

    let exe = std::env::current_exe().expect("the test binary");
    let mut child = aterm_uds::spawnfd::command(exe);
    child
        .args([
            "--exact",
            "fleet_member::tests::leave_with_no_descriptor_free",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(LOCK_CHILD, dir);
    let out = aterm_uds::deadline::output_until(&mut child, Instant::now() + HANG)
        .expect("the child ran");
    let said = String::from_utf8_lossy(&out.stdout);
    let refused = said.lines().find(|line| line.contains("leave: refused"));
    assert!(
        refused.is_some_and(|line| line.matches(&shown).count() == 1),
        "a failure of the moment, which another edit may not meet: {said}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_lock_refusal_names_the_directory_once() {
    use std::os::unix::ffi::OsStrExt as _;
    let state = State::new("lock-named");
    let dir = state.dir();
    let lock = dir.join(PEERS_LOCK);
    let shown = dir.display().to_string();
    let once = |refused: String| assert_eq!(refused.matches(&shown).count(), 1, "{refused}");
    std::fs::create_dir(&lock).unwrap();
    once(
        write_held(dir, Began::Here, |_| ())
            .unwrap_err()
            .to_string(),
    );
    std::fs::remove_dir(&lock).unwrap();
    std::os::unix::fs::symlink(dir.join("nowhere"), &lock).unwrap();
    once(node_identity(dir).unwrap_err().to_string());
    std::fs::remove_file(&lock).unwrap();
    let fifo = std::ffi::CString::new(lock.as_os_str().as_bytes()).unwrap();
    // SAFETY: a NUL-terminated path; mkfifo reads it and nothing else.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    once(node_identity(dir).unwrap_err().to_string());
    std::fs::remove_file(&lock).unwrap();
    let other = BookLock::take(dir, Duration::ZERO).unwrap();
    let busy = refused_at_once(dir);
    assert_eq!(busy.kind(), io::ErrorKind::WouldBlock);
    once(busy.to_string());
    let (told, held) = std::sync::mpsc::channel();
    let minting = std::thread::spawn({
        let dir = dir.to_path_buf();
        move || waiting(HANG, Some(told), || node_identity(&dir)).map(|me| me.key())
    });
    held.recv_timeout(HANG)
        .expect("the writer finds the lock held and waits");
    drop(other);
    let minted = minting
        .join()
        .unwrap()
        .expect("a writer that ends is waited out");
    assert_eq!(
        read_node_identity(dir).unwrap().map(|me| me.key()),
        Some(minted)
    );
}

#[test]
fn leave_with_no_descriptor_free() {
    use std::os::fd::AsRawFd as _;
    let Some(dir) = std::env::var_os(LOCK_CHILD) else {
        return;
    };
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` writes the struct this frame owns.
    let got = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    assert_eq!(got, 0);
    // The lowest descriptor free, as the limit: no file opens after.
    let lowest = std::fs::File::open("/dev/null").unwrap().as_raw_fd();
    limit.rlim_cur = libc::rlim_t::try_from(lowest).unwrap();
    // SAFETY: `setrlimit` reads the struct this frame owns: a soft limit under the hard one.
    let set = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) };
    assert_eq!(set, 0);
    match leave_held(Path::new(&dir), |_| ()) {
        Ok(()) => println!("leave: ran"),
        Err(e) => println!("leave: refused: {e}"),
    }
}

#[test]
fn the_book_is_keyed_by_node_and_never_grows_past_its_cap() {
    let mut book = vec![manual("10.0.0.7", 7100), manual("lab-mini", 7100)];
    assert_eq!(
        record(&mut book, joined(1, "m7.local", [10, 0, 0, 7], 7100)),
        Ok(Recorded::New)
    );
    assert_eq!(
        book.len(),
        2,
        "the registration stands for the person's entry"
    );
    let mut moved = joined(1, "m7.local", [10, 0, 0, 8], 7100);
    moved.version = Some("0.96.0".to_owned());
    assert_eq!(record(&mut book, moved.clone()), Ok(Recorded::Updated));
    assert_eq!(book.iter().filter(|p| p.node == moved.node).count(), 1);
    moved.key_from = Some(IpAddr::from([10, 0, 0, 7]));
    assert_eq!(book.iter().find(|p| p.node == moved.node), Some(&moved));
    assert_eq!(
        record(&mut book, manual("LAB-MINI", 7100)),
        Ok(Recorded::Already)
    );
    assert_eq!(
        record(&mut book, manual("lab-mini", 7101)),
        Ok(Recorded::New)
    );

    let mut full: Vec<Peer> = (0..PEERS_MAX)
        .map(|n| Peer {
            node: Some(format!("{n:032x}")),
            key: None,
            ..joined(0, "m", [10, 1, (n / 256) as u8, (n % 256) as u8], 7100)
        })
        .collect();
    let member = full[17].clone();
    assert!(record(&mut full, joined(0xee, "new", [10, 0, 1, 1], 7100)).is_err());
    assert!(record(&mut full, manual("other", 7100)).is_err());
    assert_eq!(full.len(), PEERS_MAX);
    assert_eq!(
        record(&mut full, member),
        Ok(Recorded::Updated),
        "a member re-joins"
    );
}

#[test]
fn one_address_registers_a_few_members_and_evicts_none() {
    let (moved, elsewhere) = (
        joined(1, "m1", [10, 0, 0, 1], 7100),
        joined(0x99, "m9", [10, 0, 0, 9], 7100),
    );
    let mut book = vec![moved.clone(), elsewhere.clone()];
    let at_its_old_address = joined(2, "m2", [10, 0, 0, 1], 7100);
    assert_eq!(
        record(&mut book, at_its_old_address.clone()),
        Ok(Recorded::New)
    );
    assert_eq!(
        book,
        vec![moved.clone(), elsewhere.clone(), at_its_old_address],
        "the machine that moved keeps its entry"
    );
    for (node, port) in (3..).zip(7101..7100 + JOINED_PER_ADDRESS as u16 - 1) {
        assert_eq!(
            record(&mut book, joined(node, "m", [10, 0, 0, 1], port)),
            Ok(Recorded::New)
        );
    }
    let from_one = |book: &[Peer]| {
        book.iter()
            .filter(|p| p.addr == Some(IpAddr::from([10, 0, 0, 1])))
            .count()
    };
    assert_eq!(from_one(&book), JOINED_PER_ADDRESS);
    let held = book.clone();
    for more in [
        joined(0xee, "made-up", [10, 0, 0, 1], 7200),
        joined(0x99, "m9", [10, 0, 0, 1], 7300),
    ] {
        let refused = record(&mut book, more.clone());
        assert!(
            refused.as_ref().is_err_and(|why| why.contains("10.0.0.1")),
            "{more:?}: {refused:?}"
        );
        assert_eq!(book, held, "nothing evicted, nothing moved");
    }
    assert_eq!(
        record(&mut book, joined(2, "m2", [10, 0, 0, 1], 7100)),
        Ok(Recorded::Updated),
        "a member of that address re-joins"
    );
    let rejoined = joined(1, "m1", [10, 0, 0, 40], 7100);
    assert_eq!(record(&mut book, rejoined.clone()), Ok(Recorded::Updated));
    assert_eq!(book.iter().filter(|p| p.node == rejoined.node).count(), 1);
    let rejoined = Peer {
        key_from: Some(IpAddr::from([10, 0, 0, 1])),
        ..rejoined
    };
    assert!(book.contains(&rejoined), "it moved its own entry");
    assert_eq!(from_one(&book), JOINED_PER_ADDRESS - 1);
    assert_eq!(
        record(&mut book, joined(0xef, "other", [10, 0, 0, 2], 7100)),
        Ok(Recorded::New)
    );
}

#[test]
fn a_registration_never_takes_a_persons_entry_by_the_name_it_gives() {
    let (by_name, by_address) = (manual("lab-mini", 7100), manual("10.0.0.7", 7100));
    let mut book = vec![by_name.clone(), by_address.clone()];
    assert_eq!(
        record(&mut book, joined(1, "lab-mini", [10, 0, 0, 8], 7100)),
        Ok(Recorded::New)
    );
    assert!(
        book.contains(&by_name),
        "its own host= takes nothing: {book:?}"
    );
    assert_eq!(
        record(&mut book, joined(2, "m7", [10, 0, 0, 7], 7100)),
        Ok(Recorded::New)
    );
    assert!(
        !book.contains(&by_address),
        "the place it registered from was the person's entry: {book:?}"
    );

    let mut theirs = vec![by_name.clone()];
    let issuer = Peer {
        via: Via::Issuer,
        ..joined(3, "lab-mini.local", [10, 0, 0, 9], 7100)
    };
    assert_eq!(record(&mut theirs, issuer.clone()), Ok(Recorded::New));
    assert_eq!(theirs, vec![issuer]);
    assert_eq!(
        record(&mut theirs, manual("LAB-MINI", 7100)),
        Ok(Recorded::Already)
    );
    assert_eq!(
        record(&mut theirs, manual("lab-mini.lan", 7100)),
        Ok(Recorded::New),
        "lab-mini.lan is another machine than lab-mini.local"
    );
}

#[test]
fn a_node_is_bound_to_its_first_key_and_another_is_refused() {
    let key = |seed: u8| identity(&node_of(0xab), seed).key();
    let mut book = Vec::new();
    assert_eq!(
        record(&mut book, keyed(0xab, key(1), [10, 0, 0, 7], 7100)),
        Ok(Recorded::New)
    );
    assert_eq!(
        record(&mut book, keyed(0xab, key(1), [10, 0, 0, 9], 7101)),
        Ok(Recorded::Updated),
        "its key moves it"
    );
    assert_eq!(book, vec![keyed(0xab, key(1), [10, 0, 0, 9], 7101)]);
    let held = book.clone();
    for (refused, says) in [
        (keyed(0xab, key(2), [10, 0, 0, 9], 7101), "another key"),
        (joined(0xab, "m7.local", [10, 0, 0, 9], 7101), "proves none"),
    ] {
        let why = record(&mut book, refused).unwrap_err();
        assert!(
            why.contains(says)
                && why.contains(&format!(
                    "`aterm fleet peers rm {}` on the issuer",
                    node_of(0xab)
                )),
            "{why}"
        );
        assert_eq!(book, held, "a refusal changes nothing");
    }
    remove(&mut book, &[], "abababab").unwrap();
    assert_eq!(
        record(&mut book, keyed(0xab, key(2), [10, 0, 0, 9], 7101)),
        Ok(Recorded::New),
        "once removed, the node binds its new key"
    );
    assert_eq!(book[0].key, Some(key(2)));

    let older = joined(0xcd, "lab-mini", [10, 0, 0, 8], 7100);
    let upgraded = |ip: [u8; 4]| Peer {
        key: Some(identity(&node_of(0xcd), 3).key()),
        ..joined(0xcd, "lab-mini", ip, 7100)
    };
    let mut book = vec![older.clone()];
    let why = record(&mut book, upgraded([10, 0, 0, 66])).unwrap_err();
    assert!(
        why.contains("recorded from 10.0.0.8")
            && why.contains(&format!("peers rm {}`", node_of(0xcd))),
        "{why}"
    );
    assert_eq!(book, vec![older], "no key is bound from another address");
    assert_eq!(
        record(&mut book, joined(0xcd, "lab-mini", [10, 0, 0, 8], 7102)),
        Ok(Recorded::Updated),
        "an older aterm's registration, as before keys came"
    );
    assert_eq!(
        record(&mut book, upgraded([10, 0, 0, 8])),
        Ok(Recorded::Updated)
    );
    assert_eq!(
        book,
        vec![upgraded([10, 0, 0, 8])],
        "bound where it was recorded"
    );
    assert!(
        record(&mut book, joined(0xcd, "lab-mini", [10, 0, 0, 8], 7100)).is_err(),
        "and now held to it"
    );

    let at = |ip: [u8; 4]| Some(IpAddr::from(ip));
    let keyed_at = |ip: [u8; 4], seed: u8| Peer {
        key: Some(identity(&node_of(0xef), seed).key()),
        ..joined(0xef, "lab-mini", ip, 7100)
    };
    let mut book = vec![joined(0xef, "lab-mini", [10, 0, 0, 8], 7100)];
    assert_eq!(
        record(&mut book, joined(0xef, "lab-mini", [10, 0, 0, 66], 7100)),
        Ok(Recorded::Updated),
        "moved keyless, as before keys came"
    );
    assert_eq!(
        (book[0].addr, book[0].key_from),
        (at([10, 0, 0, 66]), at([10, 0, 0, 8]))
    );
    let mut book: Vec<Peer> = book
        .iter()
        .map(|peer| Peer::parse(&peer.line()).unwrap())
        .collect();
    let held = book.clone();
    let why = record(&mut book, keyed_at([10, 0, 0, 66], 6)).unwrap_err();
    assert!(
        why.contains("recorded from 10.0.0.8") && why.contains(BEFORE_KEYS),
        "{why}"
    );
    assert_eq!(book, held, "no key is bound where a keyless move put it");
    assert_eq!(
        record(&mut book, joined(0xef, "lab-mini", [10, 0, 0, 23], 7100)),
        Ok(Recorded::Updated),
        "moved keyless again"
    );
    assert_eq!(
        book[0].key_from,
        at([10, 0, 0, 8]),
        "still from where it was"
    );
    let mut back = book.clone();
    record(&mut back, joined(0xef, "lab-mini", [10, 0, 0, 8], 7100)).unwrap();
    assert_eq!(
        back,
        vec![joined(0xef, "lab-mini", [10, 0, 0, 8], 7100)],
        "back where it was recorded, which its address says"
    );
    assert_eq!(
        record(&mut book, keyed_at([10, 0, 0, 8], 7)),
        Ok(Recorded::Updated)
    );
    assert_eq!(
        book,
        vec![keyed_at([10, 0, 0, 8], 7)],
        "bound, key_from gone"
    );
    assert!(
        record(&mut book, keyed_at([10, 0, 0, 66], 6))
            .unwrap_err()
            .contains("another key")
    );
}

#[test]
fn a_peer_is_proven_only_by_the_key_its_entry_binds() {
    let (a, b, i) = (
        identity(&node_of(0xa1), 1),
        identity(&node_of(0xb2), 2),
        identity(&node_of(0x11), 3),
    );
    let ring = Keyring::of(&[
        keyed(0xa1, a.key(), [10, 0, 0, 7], 7100),
        joined(0xb2, "m8", [10, 0, 0, 8], 7100),
        manual("lab-mini", 7100),
    ]);
    assert_eq!(
        ring.proven(Some(&proof(a.node(), a.key()))),
        Ok(a.node().to_owned())
    );
    assert_eq!(
        ring.proven(Some(&proof(a.node(), b.key()))),
        Err(Unproven::OtherKey(a.node().to_owned()))
    );
    assert_eq!(
        ring.proven(Some(&proof(b.node(), b.key()))),
        Err(Unproven::Unbound(b.node().to_owned()))
    );
    assert_eq!(ring.proven(None), Err(Unproven::NoProof));

    let state = State::new("ring");
    write_book(
        state.dir(),
        &[
            Peer {
                key: Some(i.key()),
                ..issuer_entry(0x11, 7100)
            },
            keyed(0xa1, a.key(), [10, 0, 0, 7], 7100),
        ],
    );
    keep_copy(
        state.dir(),
        i.node(),
        &[
            copy(keyed(0xa1, b.key(), [10, 0, 0, 7], 7100)),
            copy(keyed(0xb2, b.key(), [10, 0, 0, 8], 7102)),
            copy(keyed(0x11, i.key(), [10, 0, 0, 1], 7100)),
        ],
    )
    .unwrap();
    let ring = Keyring::read(state.dir());
    assert_eq!(
        [
            ring.bound(i.node()),
            ring.bound(a.node()),
            ring.bound(b.node())
        ],
        [Some(i.key()), Some(a.key()), Some(b.key())],
        "the issuer, its own entry over the copy's, and the copy's"
    );
    assert_eq!(
        [i.node(), a.node(), b.node(), node_of(0xd4).as_str()].map(|node| ring.copy_binds(node)),
        [false, true, true, false],
        "a peers rm here leaves bound what the copy binds, but its issuer"
    );
    mode(&state.dir().join(PEERS_FILE), 0o644);
    assert_eq!(Keyring::read(state.dir()), Keyring::default());

    let n = identity(&node_of(0xc3), 6);
    let state = State::new("ringplace");
    let issuer = Peer {
        key: Some(i.key()),
        ..issuer_entry(0x11, 7100)
    };
    write_book(state.dir(), &[issuer, manual("m7", 7100)]);
    let at_m7 = copy(keyed(0xc3, n.key(), [10, 0, 0, 7], 7100));
    keep_copy(state.dir(), i.node(), std::slice::from_ref(&at_m7)).unwrap();
    let mut read = read_peers(state.dir()).unwrap();
    with_copies(&mut read, [at_m7.clone(), copy(manual("m7.local", 7100))]);
    assert_eq!(
        read.iter().map(|peer| peer.via).collect::<Vec<_>>(),
        [Via::Issuer, Via::Manual, Via::IssuerBook],
        "the copy that names a node, beside the person's entry"
    );
    assert_eq!(read[2], at_m7);
    let ring = Keyring::read(state.dir());
    assert_eq!(ring.bound(n.node()), Some(n.key()));
    assert_eq!(
        ring,
        Keyring::of(&read),
        "a read's targets keep the copy's key"
    );
    let bound = bound_peers(state.dir());
    assert_eq!(
        bound
            .iter()
            .map(|peer| peer.node.clone())
            .collect::<Vec<_>>(),
        [Some(i.node().to_owned()), Some(n.node().to_owned())]
    );
    assert_eq!(
        find(&bound, "m7").map(|at| at.map(|at| bound[at].key)),
        Ok(Some(Some(n.key())))
    );
    assert!(find(&read, "m7").is_err(), "two peers answer to it");
    assert_eq!(
        ring.proven(Some(&proof(n.node(), b.key()))),
        Err(Unproven::OtherKey(n.node().to_owned()))
    );
    let at = SocketAddr::from(([10, 0, 0, 7], 7100));
    let person = target_of(&manual("m7", 7100));
    assert_eq!(
        answered_as(&person, &ring, at, Some(&proof(n.node(), n.key()))).unwrap(),
        Some((n.node().to_owned(), true))
    );
    assert!(
        matches!(
            answered_as(&person, &ring, at, Some(&proof(n.node(), b.key()))),
            Err(Unreached::Impostor { issuer: false, .. })
        ),
        "another key for the copy's node, at the person's entry"
    );
}

#[test]
fn an_impostor_of_the_issuer_names_the_remedy_here() {
    let (i, m) = (identity(&node_of(0x11), 3), identity(&node_of(0xa1), 1));
    let issuer = Peer {
        key: Some(i.key()),
        ..issuer_entry(0x11, 7100)
    };
    let member = copy(keyed(0xa1, m.key(), [10, 0, 0, 7], 7100));
    let ring = Keyring::of([&issuer, &member]);
    let at = SocketAddr::from(([192, 168, 100, 100], 7100));
    let other = identity(&node_of(0x11), 4).key();
    let said = |target: &Peer, node: &str| {
        answered_as(&target_of(target), &ring, at, Some(&proof(node, other)))
            .unwrap_err()
            .to_string()
    };
    let of_issuer = said(&issuer, i.node());
    assert!(
        of_issuer.contains("claims issuer 11111111")
            && of_issuer.contains("peers rm it here, rejoin"),
        "{of_issuer}"
    );
    let of_member = said(&member, m.node());
    assert!(
        of_member.contains("peers rm it on the issuer"),
        "{of_member}"
    );
    for reason in [of_issuer, of_member] {
        assert!(reason.chars().count() <= 103, "{reason}");
    }
}

#[test]
fn a_drive_grant_holds_the_key_it_was_made_for() {
    let (a, m) = (identity(&node_of(0xa1), 1), identity(&node_of(0xa1), 9));
    let grant = DriveGrant {
        node: a.node().to_owned(),
        key: a.key(),
    };
    assert_eq!(DriveGrant::from_word(&grant.word()), Some(grant.clone()));
    assert_eq!(
        DriveGrant::from_word(&grant.word().to_ascii_uppercase()),
        Some(grant.clone())
    );
    let keyless = format!("{}:", a.node());
    for none in [a.node(), "", ":", "mac-03:00", &keyless] {
        assert_eq!(DriveGrant::from_word(none), None, "{none:?}");
    }
    assert_eq!(DriveGrant::node_of(&grant.word()), a.node());
    assert_eq!(DriveGrant::node_of(a.node()), a.node());
    let grants = [grant];

    let approved = Keyring::of(&[keyed(0xa1, a.key(), [10, 0, 0, 7], 7100)]);
    assert_eq!(
        approved.driver(Some(&proof(a.node(), a.key())), &grants),
        Ok(a.node().to_owned())
    );
    assert_eq!(
        approved.driver(Some(&proof(a.node(), a.key())), &[]),
        Err(Undriven::Unlisted(a.node().to_owned()))
    );
    assert_eq!(
        Keyring::default().driver(Some(&proof(a.node(), a.key())), &[]),
        Err(Undriven::Unlisted(a.node().to_owned())),
        "a node no grant names is refused as that, bound or not"
    );
    let rebound = Keyring::of(&[keyed(0xa1, m.key(), [10, 0, 0, 9], 7100)]);
    assert_eq!(
        rebound.proven(Some(&proof(m.node(), m.key()))),
        Ok(m.node().to_owned()),
        "the book alone would admit it"
    );
    let refused = rebound.driver(Some(&proof(m.node(), m.key())), &grants);
    assert_eq!(refused, Err(Undriven::Rekeyed(a.node().to_owned())));
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("aterm fleet allow drive --from")
    );
    assert_eq!(
        rebound.driver(Some(&proof(a.node(), a.key())), &grants),
        Err(Undriven::Unproven(Unproven::OtherKey(a.node().to_owned()))),
        "and the Mac the grant was made for is held to the book too"
    );
    assert_eq!(
        Keyring::default().driver(Some(&proof(m.node(), m.key())), &grants),
        Err(Undriven::Rekeyed(a.node().to_owned())),
        "a key its grant does not hold is refused as that, bound or not"
    );
    assert_eq!(
        approved.driver(None, &grants),
        Err(Undriven::Unproven(Unproven::NoProof))
    );
}

#[test]
fn an_all_member_grant_still_requires_an_issuer_binding() {
    let member = identity(&node_of(0xa2), 3);
    let all = DriveGrant::from_word(crate::fleet::ALL_MEMBERS).expect("all parses");
    assert!(all.all());
    assert_eq!(all.word(), crate::fleet::ALL_MEMBERS);
    let grants = [all];
    let bound = Keyring::of(&[keyed(0xa2, member.key(), [10, 0, 0, 8], 7100)]);
    assert_eq!(
        bound.driver(Some(&proof(member.node(), member.key())), &grants),
        Ok(member.node().to_owned())
    );
    assert_eq!(
        Keyring::default().driver(Some(&proof(member.node(), member.key())), &grants),
        Err(Undriven::Unproven(Unproven::Unbound(
            member.node().to_owned()
        )))
    );
    assert_eq!(
        bound.driver(None, &grants),
        Err(Undriven::Unproven(Unproven::NoProof))
    );
    let named = DriveGrant {
        node: member.node().to_owned(),
        key: member.key(),
    };
    let both = [grants[0].clone(), named.clone()];
    assert_eq!(
        DriveGrant::admitting(&both, member.node(), member.key()),
        Some(&named)
    );
    assert_eq!(
        DriveGrant::admitting(&both, &node_of(0xa3), member.key()),
        Some(&named),
        "the verified key keeps its named grant across an id change"
    );
    let rekeyed = identity(&node_of(0xa2), 4).key();
    assert_eq!(
        DriveGrant::admitting(std::slice::from_ref(&named), member.node(), rekeyed),
        None
    );
}

#[test]
fn signed_administrative_records_require_signature_current_grant_and_live_membership() {
    let state = State::new("signed-service-repair-authority");
    let driver = identity(&node_of(0xe7), 41);
    let claimant = identity(driver.node(), 42);
    let proof = proof(driver.node(), driver.key());
    let domain = "service-repair-v1";
    let body = b"node=target job=server old=image-a desired=image-b nonce=one";
    let signature = driver.sign_record(domain, body);
    let grants = [DriveGrant {
        node: driver.node().to_owned(),
        key: driver.key(),
    }];
    write_book(
        state.dir(),
        &[keyed(0xe7, driver.key(), [10, 0, 0, 7], 7100)],
    );
    let membership: MemberKeys = [(driver.key(), Standing::Member)].into_iter().collect();
    let verify = |body: &[u8],
                  domain: &str,
                  signature: &[u8; SIG_LEN],
                  grants: &[DriveGrant],
                  log: &MemberKeys| {
        signed_record_driver(
            &proof,
            domain,
            body,
            signature,
            state.dir(),
            grants,
            Some(log),
        )
    };
    assert_eq!(
        verify(body, domain, &signature, &grants, &membership),
        Ok(driver.node().to_owned())
    );
    assert!(
        verify(
            b"node=target job=server old=image-a desired=evil nonce=one",
            domain,
            &signature,
            &grants,
            &membership
        )
        .is_err()
    );
    assert!(
        verify(
            body,
            "another-administrative-operation",
            &signature,
            &grants,
            &membership
        )
        .is_err()
    );
    assert!(
        verify(
            body,
            domain,
            &claimant.sign_record(domain, body),
            &grants,
            &membership
        )
        .is_err()
    );
    assert!(verify(body, domain, &signature, &[], &membership).is_err());
    let wrong_key = [DriveGrant {
        node: driver.node().to_owned(),
        key: claimant.key(),
    }];
    assert!(verify(body, domain, &signature, &wrong_key, &membership).is_err());
    let revoked: MemberKeys = [(driver.key(), Standing::Out("membership revoked".to_owned()))]
        .into_iter()
        .collect();
    assert!(verify(body, domain, &signature, &grants, &revoked).is_err());
    assert_eq!(
        verify(body, domain, &signature, &grants, &membership),
        Ok(driver.node().to_owned())
    );
    write_book(
        state.dir(),
        &[keyed(0xe7, claimant.key(), [10, 0, 0, 7], 7100)],
    );
    assert!(verify(body, domain, &signature, &grants, &membership).is_err());
}

#[test]
fn with_the_issuer_dark_a_member_drives_a_member_its_log_admits() {
    // The member's issuer is dark (its port is closed) and its copy of the issuer's book binds no
    // driver: the drive is judged on this Mac alone, by its membership log.
    let member = State::new("dark-issuer-member");
    write_key(member.dir(), &[5; SECRET_LEN]).unwrap();
    let i = identity(&issuer(0x11).node, 9);
    let n = identity(&node_of(0xd4), 4);
    let gone = identity(&node_of(0xd5), 5);
    let stranger = identity(&node_of(0xe5), 6);
    let dark = Peer {
        key: Some(i.key()),
        ..issuer_entry(0x11, closed_port())
    };
    write_book(member.dir(), std::slice::from_ref(&dark));
    keep_copy(member.dir(), i.node(), &[]).unwrap();
    let all = [DriveGrant::from_word(crate::fleet::ALL_MEMBERS).expect("all parses")];
    let log: MemberKeys = [
        (n.key(), Standing::Member),
        (gone.key(), Standing::Out("removed by m7".to_owned())),
    ]
    .into_iter()
    .collect();
    let as_of = |who: &NodeIdentity| Some(proof(who.node(), who.key()));
    let drive = |who: &NodeIdentity, grants: &[DriveGrant], log: Option<&MemberKeys>| {
        driver_as_of(as_of(who).as_ref(), (member.dir(), grants), log)
    };

    assert_eq!(
        drive(&n, &all, Some(&log)),
        Ok(n.node().to_owned()),
        "the log admits it, with the issuer dark and no book binding it"
    );
    assert_eq!(
        drive(&n, &all, None),
        Err(Undriven::Unproven(Unproven::Unbound(n.node().to_owned()))),
        "with no log, no book binds it"
    );
    let named = [DriveGrant {
        node: n.node().to_owned(),
        key: n.key(),
    }];
    assert_eq!(drive(&n, &named, Some(&log)), Ok(n.node().to_owned()));
    let claimant = identity(n.node(), 7);
    let log_and_claimant: MemberKeys = [
        (n.key(), Standing::Member),
        (claimant.key(), Standing::Member),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        drive(&claimant, &named, Some(&log_and_claimant)),
        Err(Undriven::Rekeyed(n.node().to_owned())),
        "a named grant holds its key, whatever the log admits"
    );
    assert_eq!(
        drive(&stranger, &all, Some(&log_and_claimant)),
        Err(Undriven::Unproven(Unproven::Unbound(
            stranger.node().to_owned()
        ))),
        "a key the log does not name goes by the book"
    );

    write_book(
        member.dir(),
        &[
            dark.clone(),
            keyed(0xd4, n.key(), [10, 0, 0, 4], 7100),
            keyed(0xd5, gone.key(), [10, 0, 0, 5], 7100),
        ],
    );
    assert_eq!(
        drive(&gone, &all, None),
        Ok(gone.node().to_owned()),
        "the book alone binds it"
    );
    let removed = drive(&gone, &all, Some(&log));
    assert_eq!(
        removed,
        Err(Undriven::Removed {
            node: gone.node().to_owned(),
            why: "removed by m7".to_owned()
        }),
        "the log took it out: refused though the book binds it"
    );
    assert!(removed.as_ref().unwrap_err().ungranted());
    assert_eq!(
        drive(&claimant, &all, Some(&log_and_claimant)),
        Err(Undriven::Unproven(Unproven::OtherKey(n.node().to_owned()))),
        "a member claiming the node id the book binds to another member's key"
    );
    assert_eq!(
        drive(&n, &all, Some(&log)),
        Ok(n.node().to_owned()),
        "bound and admitted"
    );
    assert_eq!(
        driver_as_of(None, (member.dir(), &all), Some(&log)),
        Err(Undriven::Unproven(Unproven::NoProof))
    );

    let node = || n.node().to_owned();
    for refusal in [
        Undriven::Unproven(Unproven::Unbound(node())),
        Undriven::Unproven(Unproven::OtherKey(node())),
        Undriven::Unlisted(node()),
        Undriven::Rekeyed(node()),
        Undriven::Removed {
            node: node(),
            why: "removed by m7".to_owned(),
        },
    ] {
        let said = refusal.to_string();
        assert!(
            said.contains("the dialled Mac")
                && !said.contains("this Mac")
                && !said.contains("this machine")
                && !said.contains(" here")
                && !said.contains("issuer"),
            "the driving Mac reads it too: {said}"
        );
    }
}

#[test]
fn rm_takes_one_peer_or_names_the_candidates() {
    let a = joined(0xaa, "m7.local", [10, 0, 0, 7], 7100);
    let b = joined(0xbb, "m7.lan", [10, 0, 0, 8], 7100);
    let mut book = vec![a.clone(), b.clone(), manual("lab-mini", 7100)];
    let ambiguous = remove(&mut book, &[], "m7").unwrap_err();
    assert!(
        ambiguous.contains("names 2 peers")
            && ambiguous.contains("aaaaaaaa")
            && ambiguous.ends_with("name one by its node id or its address and port"),
        "{ambiguous}"
    );
    let moved = joined(0xcc, "m7.lan", [10, 0, 0, 7], 7100);
    let shared = find(&[a.clone(), moved], "10.0.0.7:7100").unwrap_err();
    assert!(
        shared.ends_with("name one by its node id"),
        "a place two share names neither: {shared}"
    );
    assert_eq!(book.len(), 3, "an ambiguous name removes nothing");
    assert_eq!(remove(&mut book, &[], "m7.lan"), Ok(b));
    assert_eq!(remove(&mut book, &[], "aaaaaaaa"), Ok(a));
    assert!(remove(&mut book, &[], "10.0.0.7").is_err());
    assert_eq!(
        remove(&mut book, &[], "lab-mini:7100"),
        Ok(manual("lab-mini", 7100))
    );
    assert!(book.is_empty());
}

const EVERY_OP: &[FleetOp] = &[FleetOp::Survey, FleetOp::Book, FleetOp::Join];

const OLDER_OPS: &[FleetOp] = &[FleetOp::Survey, FleetOp::Join];

struct Issuing {
    port: u16,
    running: Arc<AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
}

impl Issuing {
    fn start(secret: FleetSecret, state: &Path, issuer: Issuer) -> Self {
        Self::serving(secret, state, issuer, EVERY_OP)
    }

    fn serving(
        secret: FleetSecret,
        state: &Path,
        issuer: Issuer,
        serves: &'static [FleetOp],
    ) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        Self::on(listener, secret, state, issuer, serves)
    }

    fn on(
        listener: TcpListener,
        secret: FleetSecret,
        state: &Path,
        issuer: Issuer,
        serves: &'static [FleetOp],
    ) -> Self {
        Self::hooked(listener, secret, state, issuer, serves, Arc::new(|_| None))
    }

    fn proving(secret: FleetSecret, state: &Path, issuer: Issuer, me: NodeIdentity) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        Self::run(
            listener,
            (secret, Some(me)),
            state,
            issuer,
            EVERY_OP,
            Arc::new(|_| None),
        )
    }

    /// A member that judges joins by `log` and serves its own signed entry with its book.
    fn member(secret: FleetSecret, state: &Path, me: NodeIdentity, log: MemberKeys) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let named = Issuer {
            node: me.node().to_owned(),
            host: "127.0.0.1".to_owned(),
            port: listener.local_addr().unwrap().port(),
            version: "0.146.0".to_owned(),
        };
        Self::gated(listener, (secret, Some(me)), state, named, Some(log))
    }

    fn hooked(
        listener: TcpListener,
        secret: FleetSecret,
        state: &Path,
        issuer: Issuer,
        serves: &'static [FleetOp],
        before: Arc<dyn Fn(FleetOp) -> Option<&'static str> + Send + Sync>,
    ) -> Self {
        Self::run(listener, (secret, None), state, issuer, serves, before)
    }

    fn gated(
        listener: TcpListener,
        (secret, me): (FleetSecret, Option<NodeIdentity>),
        state: &Path,
        issuer: Issuer,
        log: Option<MemberKeys>,
    ) -> Self {
        Self::listen(
            listener,
            (secret, me),
            (state, issuer),
            EVERY_OP,
            Arc::new(|_| None),
            log,
        )
    }

    fn run(
        listener: TcpListener,
        (secret, me): (FleetSecret, Option<NodeIdentity>),
        state: &Path,
        issuer: Issuer,
        serves: &'static [FleetOp],
        before: Arc<dyn Fn(FleetOp) -> Option<&'static str> + Send + Sync>,
    ) -> Self {
        Self::listen(
            listener,
            (secret, me),
            (state, issuer),
            serves,
            before,
            None,
        )
    }

    fn listen(
        listener: TcpListener,
        (secret, me): (FleetSecret, Option<NodeIdentity>),
        (state, issuer): (&Path, Issuer),
        serves: &'static [FleetOp],
        before: Arc<dyn Fn(FleetOp) -> Option<&'static str> + Send + Sync>,
        log: Option<MemberKeys>,
    ) -> Self {
        let port = listener.local_addr().unwrap().port();
        let own = me
            .as_ref()
            .filter(|_| log.is_some())
            .map(|me| Peer::own(me, (&issuer.host, port, &issuer.version), 1_760_000_000_000));
        let running = Arc::new(AtomicBool::new(true));
        let listening = Listening {
            secret,
            scope: FleetScope::Content,
            node: me,
            serves,
            slots: 8,
            per_source: 8,
            lifetime: HANG,
        };
        let (state, issuer) = (state.to_path_buf(), Arc::new(issuer));
        let secret = listening.secret.clone();
        let serving = std::thread::spawn({
            let running = Arc::clone(&running);
            move || {
                serve_with_handshake(
                    &listener,
                    &listening,
                    &running,
                    HANG,
                    move |served, op| {
                        let proof = served.peer_node().cloned();
                        served.answer(|stream| {
                            stream.set_read_timeout(Some(HANG))?;
                            let from = stream.peer_addr()?.ip();
                            if op != FleetOp::Join
                                && let Some(answer) = before(op)
                            {
                                stream.write_all(answer.as_bytes())?;
                                return stream.flush();
                            }
                            if op == FleetOp::Book {
                                let node = issuer.node.clone();
                                return serve_book_at(
                                    stream,
                                    &state,
                                    move || Ok(node),
                                    None,
                                    (&[], own.as_ref()),
                                )
                                .map(|_| ());
                            }
                            if op != FleetOp::Join {
                                return answer_survey(stream, &issuer.node);
                            }
                            let issuer = Issuer::clone(&issuer);
                            serve_join_with_denied(
                                stream,
                                (from, proof.as_ref()),
                                (&state, &secret),
                                move || Ok(issuer),
                                1_760_000_000_000,
                                (&[], log.as_ref()),
                            )
                            .map(|_| ())
                        })
                    },
                    |_: FleetEvent| {},
                );
            }
        });
        Self {
            port,
            running,
            serving: Some(serving),
        }
    }
}

impl Drop for Issuing {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        let _ = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, self.port));
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

fn answer_survey<S: Read + Write>(stream: &mut S, node: &str) -> io::Result<()> {
    let mut byte = [0u8; 1];
    while stream.read(&mut byte)? == 1 && byte[0] != b'\n' {}
    stream.write_all(
        format!(
            "OK 1\ninstance pid=1 host=m7 version=0.95.0 sessions=0 level=content cut=0 at_ms=1 \
             node={node}\n"
        )
        .as_bytes(),
    )?;
    stream.flush()
}

fn issuer(node: u8) -> Issuer {
    Issuer {
        node: format!("{node:02x}").repeat(16),
        host: "m7.local".to_owned(),
        port: 7100,
        version: "0.95.0".to_owned(),
    }
}

fn issue(state: &Path) {
    let book = hold_book(state).unwrap();
    book.write_code(&code()).unwrap();
    book.write_key(&[5; SECRET_LEN]).unwrap();
}

fn join_over(port: u16, secret: &FleetSecret, reg: &Registration) -> Result<Joined, String> {
    join_as(port, (secret, None), reg).0
}

fn join_as(
    port: u16,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    reg: &Registration,
) -> (Result<Joined, String>, Option<NodeProof>) {
    let mut reached = dial("127.0.0.1", None, port, (secret, me), FleetOp::Join, HANG)
        .expect("the issuer proves join");
    reached
        .transport
        .stream()
        .get_ref()
        .set_read_timeout(Some(HANG))
        .unwrap();
    let joined = register(reached.transport.stream(), reg).map_err(|refused| match refused {
        Unregistered::Answered(why) => why,
        Unregistered::Unanswered(why) => panic!("the issuer answers every join: {why}"),
    });
    (joined, reached.peer)
}

#[test]
fn a_registration_tells_an_issuers_refusal_from_no_answer() {
    let reg = Registration {
        node: "ab".repeat(16),
        host: "lab-mini".to_owned(),
        port: 7101,
        version: "0.95.0+g1a2b3c4".to_owned(),
        key: None,
        signed: None,
    };
    for said in [Some("ERR the book is full\n"), None] {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let running = Arc::new(AtomicBool::new(true));
        let listening = Listening {
            secret: secret(6),
            scope: FleetScope::Content,
            node: None,
            serves: &[FleetOp::Join],
            slots: 8,
            per_source: 8,
            lifetime: HANG,
        };
        let serving = std::thread::spawn({
            let running = Arc::clone(&running);
            move || {
                serve_with_handshake(
                    &listener,
                    &listening,
                    &running,
                    HANG,
                    move |served, _| {
                        served.answer(|stream| {
                            stream.set_read_timeout(Some(HANG))?;
                            let mut byte = [0u8; 1];
                            while stream.read(&mut byte)? == 1 && byte[0] != b'\n' {}
                            if let Some(said) = said {
                                stream.write_all(said.as_bytes())?;
                            }
                            stream.flush()
                        })
                    },
                    |_: FleetEvent| {},
                );
            }
        });
        let mut reached = dial(
            "127.0.0.1",
            None,
            port,
            (&secret(6), None),
            FleetOp::Join,
            HANG,
        )
        .expect("it proves join");
        let refused =
            register_by(&mut reached, &reg, Instant::now() + HANG).expect_err("nothing recorded");
        match (said, refused) {
            (Some(_), Unregistered::Answered(why)) => {
                assert_eq!(why, "the book is full");
            }
            (None, Unregistered::Unanswered(why)) => {
                assert!(why.starts_with("the issuer did not answer"), "{why}");
            }
            (said, refused) => panic!("{said:?}: {refused:?}"),
        }
        running.store(false, Ordering::Release);
        serving.join().unwrap();
    }
}

#[test]
fn the_longest_join_answer_with_its_seed_fits_one_record() {
    let root = NodeIdentity::from_seed(&"11".repeat(16), &[0x11; 32]).unwrap();
    let seed = fleet_seed::FleetSeed::mint(&root, secret(5).mark(), u64::MAX).unwrap();
    let longest = Issuer {
        node: "11".repeat(16),
        host: "h".repeat(253),
        port: u16::MAX,
        version: "+".repeat(64),
    };
    let answer = join_answer(&longest, PEERS_MAX, Some(&seed));
    assert!(answer.len() < RECORD_MAX, "{}", answer.len());
    let joined = joined_of(&answer).expect("an older reader's fields are all there");
    assert_eq!((joined.issuer, joined.seed), (longest, Some(seed)));
    assert_eq!(
        joined_of(&join_answer(&issuer(0x11), 1, None))
            .unwrap()
            .seed,
        None,
        "a member holding none"
    );
}

#[test]
fn a_join_registers_the_member_with_the_issuer() {
    let state = State::new("join");
    let issuing = Issuing::start(secret(5), state.dir(), issuer(0x11));
    let reg = Registration {
        node: "ab".repeat(16),
        host: "lab-mini".to_owned(),
        port: 7101,
        version: "0.95.0+g1a2b3c4".to_owned(),
        key: None,
        signed: None,
    };
    assert!(
        join_over(issuing.port, &secret(5), &reg).is_err(),
        "no code here: no issuer"
    );
    assert_eq!(read_peers(state.dir()).unwrap(), Vec::new());
    issue(state.dir());
    let joined = join_over(issuing.port, &secret(5), &reg).expect("the join is answered OK");
    assert_eq!(
        joined,
        Joined {
            issuer: issuer(0x11),
            peers: 1,
            seed: None,
        }
    );
    let book = read_peers(state.dir()).unwrap();
    assert_eq!(
        book,
        vec![Peer {
            node: Some(reg.node.clone()),
            key: None,
            key_from: None,
            host: "lab-mini".to_owned(),
            addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
            port: 7101,
            version: Some(reg.version.clone()),
            seen_ms: Some(1_760_000_000_000),
            via: Via::Joined,
            fleet: None,
            signed: None,
        }]
    );

    let again = Registration {
        port: 7102,
        ..reg.clone()
    };
    assert_eq!(
        join_over(issuing.port, &secret(5), &again).unwrap().peers,
        1
    );
    assert_eq!(read_peers(state.dir()).unwrap()[0].port, 7102);

    // A member holding the fleet seed answers with it, for the joiner to verify (machine names
    // §3.3).
    let root = NodeIdentity::from_seed(&"11".repeat(16), &[0x11; 32]).unwrap();
    let seed = fleet_seed::FleetSeed::mint(&root, secret(5).mark(), 1_793_000_000_000).unwrap();
    write_private(
        state.dir(),
        fleet_seed::FILE,
        format!("{}\n", seed.words()).as_bytes(),
    )
    .unwrap();
    let seeded = join_over(issuing.port, &secret(5), &again).unwrap();
    assert_eq!(seeded.seed, Some(seed));
    assert!(
        seeded
            .seed
            .is_some_and(|seed| seed.verifies(&root.key(), secret(5).mark()))
    );

    let itself = Registration {
        node: issuer(0x11).node,
        ..reg.clone()
    };
    assert!(join_over(issuing.port, &secret(5), &itself).is_err());
    let book = read_peers(state.dir()).unwrap();
    assert_eq!(book.len(), 1);
    assert_eq!(book[0].node.as_deref(), Some(reg.node.as_str()));

    let stranger = dial(
        "127.0.0.1",
        None,
        issuing.port,
        (&secret(6), None),
        FleetOp::Join,
        HANG,
    );
    assert!(
        matches!(
            stranger,
            Err(Unreached::Handshake(_, FleetError::ListenerWrongSecret))
        ),
        "{:?}",
        stranger.err()
    );
    assert_eq!(read_peers(state.dir()).unwrap().len(), 1);
}

struct Duplex {
    input: std::io::Cursor<Vec<u8>>,
    output: Vec<u8>,
}

impl std::io::Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf)
    }
}

impl std::io::Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_join_answers_with_the_member_as_the_book_records_it() {
    let state = State::new("joinsays");
    issue(state.dir());
    let reg = Registration {
        node: "cd".repeat(16),
        host: "lab-mini".to_owned(),
        port: 58054,
        version: "0.105.0".to_owned(),
        key: None,
        signed: None,
    };
    let from = IpAddr::from(Ipv4Addr::LOCALHOST);
    let serve = |reg: &Registration| {
        let mut stream = Duplex {
            input: std::io::Cursor::new(reg.line().into_bytes()),
            output: Vec::new(),
        };
        let outcome = serve_join(
            &mut stream,
            (from, None),
            (state.dir(), &secret(5)),
            || Ok(issuer(0x11)),
            7,
        )
        .expect("the stream reads and writes");
        (outcome, String::from_utf8(stream.output).unwrap())
    };
    let (outcome, answer) = serve(&reg);
    assert!(answer.starts_with("OK node="), "{answer}");
    let (recorded, member) = outcome.expect("recorded");
    assert_eq!(recorded, Recorded::New);
    assert_eq!(
        (member.node.as_deref(), member.addr, member.port),
        (Some(reg.node.as_str()), Some(from), 58054)
    );
    assert_eq!(read_peers(state.dir()).unwrap(), vec![member.clone()]);
    let moved = Registration {
        port: 58060,
        ..reg.clone()
    };
    let (recorded, member) = serve(&moved).0.expect("recorded again");
    assert_eq!((recorded, member.port), (Recorded::Updated, 58060));
    let itself = Registration {
        node: issuer(0x11).node,
        ..reg
    };
    let (outcome, answer) = serve(&itself);
    assert!(
        outcome.is_err()
            && answer.starts_with("ERR the registration names the answering member's own node")
            && !answer.contains("this machine"),
        "{answer}"
    );
    assert_eq!(read_peers(state.dir()).unwrap().len(), 1);
}

#[test]
fn a_join_is_judged_by_the_log_whatever_secret_it_holds() {
    let state = State::new("joinlog");
    write_key(state.dir(), &[5; SECRET_LEN]).unwrap();
    let member = identity(&node_of(0xcd), 4);
    let removed = identity(&node_of(0xce), 5);
    let stranger = identity(&node_of(0xcf), 6);
    let log: MemberKeys = [
        (member.key(), Standing::Member),
        (removed.key(), Standing::Out("removed by m7".to_owned())),
    ]
    .into_iter()
    .collect();
    let reg = |who: &NodeIdentity| {
        Registration {
            node: who.node().to_owned(),
            host: "lab-mini".to_owned(),
            port: 58054,
            version: "0.146.0".to_owned(),
            key: None,
            signed: None,
        }
        .signed_by(who, 1_760_000_000_000)
    };
    let serve = |reg: &Registration, who: &NodeIdentity, log: Option<&MemberKeys>| {
        let mut stream = Duplex {
            input: std::io::Cursor::new(reg.line().into_bytes()),
            output: Vec::new(),
        };
        let outcome = serve_join_with_denied(
            &mut stream,
            (
                IpAddr::from(Ipv4Addr::LOCALHOST),
                Some(&proof(who.node(), who.key())),
            ),
            (state.dir(), &secret(5)),
            || Ok(issuer(0x11)),
            7,
            (&[], log),
        )
        .expect("the stream reads and writes");
        (outcome, String::from_utf8(stream.output).unwrap())
    };
    let (outcome, answer) = serve(&reg(&member), &member, Some(&log));
    let (recorded, peer) = outcome.unwrap_or_else(|why| panic!("{why}: {answer}"));
    assert_eq!(recorded, Recorded::New);
    assert!(
        peer.signature_holds(),
        "the joiner's own signed entry: {peer:?}"
    );
    let (outcome, answer) = serve(&reg(&removed), &removed, Some(&log));
    assert!(
        outcome.is_err()
            && answer.starts_with(&format!("ERR {NOT_ADMITTED}"))
            && answer.contains("it took that key out"),
        "{answer}"
    );
    assert_eq!(
        read_peers(state.dir()).unwrap().len(),
        1,
        "the refused recorded nothing"
    );
    // A key the log names no way joins by the fleet secret: a founder's, while no record binds
    // its node key (Genesis v2 names none).
    let (outcome, answer) = serve(&reg(&stranger), &stranger, Some(&log));
    assert!(outcome.is_ok(), "{answer}");
    let (outcome, _) = serve(&reg(&removed), &removed, None);
    assert!(
        outcome.is_ok(),
        "with no log, the fleet secret admits as it did"
    );

    let mut forged = reg(&member);
    forged.port = 58055;
    let (outcome, answer) = serve(&forged, &member, Some(&log));
    assert!(
        outcome.is_err() && answer.contains("signature does not hold"),
        "a field changed after signing: {answer}"
    );
}

/// `who`'s signed join at the member whose state is `state` (node `12…`, fleet secret 5), judged
/// by `log`.
fn join_here(
    state: &Path,
    who: &NodeIdentity,
    log: Option<&MemberKeys>,
) -> Result<(Recorded, Peer), String> {
    let reg = Registration {
        node: who.node().to_owned(),
        host: "lab-mini".to_owned(),
        port: 58054,
        version: "0.146.0".to_owned(),
        key: None,
        signed: None,
    }
    .signed_by(who, 1_760_000_000_000);
    let mut stream = Duplex {
        input: std::io::Cursor::new(reg.line().into_bytes()),
        output: Vec::new(),
    };
    serve_join_with_denied(
        &mut stream,
        (
            IpAddr::from(Ipv4Addr::LOCALHOST),
            Some(&proof(who.node(), who.key())),
        ),
        (state, &secret(5)),
        || Ok(issuer(0x12)),
        7,
        (&[], log),
    )
    .expect("the stream reads and writes")
}

/// A node only this Mac's copy of its issuer's book binds (it joined there, never here) is taken
/// by no other key: a join that claims it is refused, as the issuer refused one, and a signed entry
/// that claims it is not merged. A key the log took out gives way to the one the log admits.
/// NEGATIVE CONTROL: drop the copy's check (`copy_rebinds`) from the join and the merge: X's join
/// is recorded, and this Mac's keyring binds P's node to X's key.
#[test]
fn a_node_only_the_copy_of_the_issuers_book_binds_is_taken_by_no_other_key() {
    let state = State::new("copybinds");
    write_key(state.dir(), &[5; SECRET_LEN]).unwrap();
    let i = identity(&node_of(0x11), 0x11);
    let p = identity(&node_of(0xd1), 0x21);
    let x = identity(&node_of(0xd1), 0x22);
    write_book(
        state.dir(),
        &[Peer {
            key: Some(i.key()),
            ..issuer_entry(0x11, 7100)
        }],
    );
    keep_copy(
        state.dir(),
        i.node(),
        &[copy(keyed(0xd1, p.key(), [10, 0, 0, 7], 7100))],
    )
    .unwrap();
    let outcome = join_here(state.dir(), &x, None);
    assert!(
        outcome
            .as_ref()
            .is_err_and(|why| why.contains("copy of its issuer's book")),
        "{outcome:?}"
    );
    assert_eq!(Keyring::read(state.dir()).bound(p.node()), Some(p.key()));
    let mut book = read_peers(state.dir()).unwrap();
    let copies = copied_keys(state.dir(), &book);
    let claim = Peer::own(&x, ("m9.local", 7301, "0.146.0"), 1_760_000_000_001);
    assert_eq!(
        merge_signed(
            &mut book,
            &[claim],
            (Some(&node_of(0x12)), 1_760_000_000_100),
            (None, &[], &copies)
        ),
        0,
        "a signed entry that claims the node under another key"
    );
    let log: MemberKeys = [
        (p.key(), Standing::Out("removed by m7".to_owned())),
        (x.key(), Standing::Member),
    ]
    .into_iter()
    .collect();
    let outcome = join_here(state.dir(), &x, Some(&log));
    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(
        Keyring::read(state.dir()).bound(p.node()),
        Some(x.key()),
        "the book's own entry ranks over the copy's"
    );
}

/// When a merged entry was last heard is no later than this Mac's clock: no signature covers it,
/// so an entry served as heard in the far future would make every other joined entry here read 90
/// days silent, and the prune at its recording would take them.
/// NEGATIVE CONTROL: keep the served `seen_ms` (drop the clamp): the member's entry is pruned.
#[test]
fn a_merged_entry_heard_in_the_future_ages_no_entry_here() {
    let now = 1_760_000_000_000_u64;
    let member = identity(&node_of(0xe1), 0x31);
    let far = identity(&node_of(0xe2), 0x32);
    // The far member is one this book holds, heard a minute ago.
    let mut book = vec![
        Peer {
            seen_ms: Some(now - 60_000),
            ..keyed(0xe1, member.key(), [10, 0, 0, 7], 7100)
        },
        Peer::own(&far, ("m8.local", 7100, "0.146.0"), now - 60_000),
    ];
    let served = Peer {
        seen_ms: Some(u64::MAX),
        addr: Some(IpAddr::from([10, 0, 0, 8])),
        ..Peer::own(&far, ("m8.local", 7100, "0.146.0"), now)
    };
    assert_eq!(
        merge_signed(
            &mut book,
            &[served],
            (None, now),
            (None, &[], &Keyring::default())
        ),
        1
    );
    assert!(
        book.iter()
            .any(|peer| peer.node.as_deref() == Some(member.node())),
        "{book:?}"
    );
    assert!(
        book.iter()
            .all(|peer| peer.seen_ms.is_some_and(|seen| seen <= now)),
        "{book:?}"
    );
}

/// A merged entry's `addr` is not covered by its signature: merging `JOINED_PER_ADDRESS` entries
/// whose serving book named `10.0.0.9` neither evicts a silent member at `10.0.0.9` nor blocks a
/// member at `10.0.0.9` from joining, and merging a newer signed entry for a member this Mac has
/// already reached keeps the `addr` this Mac recorded first-hand.
/// NEGATIVE CONTROL: keep the served `addr` in `merge_signed`: the silent member at `10.0.0.9` is
/// evicted, `10.0.0.9`'s join is refused, and the first-hand member's `addr` moves to `10.0.0.9`.
#[test]
fn a_merged_entrys_unsigned_addr_neither_crowds_an_address_nor_moves_a_held_one() {
    let now = 1_760_000_000_000_u64;
    let silent = identity(&node_of(0xe3), 0x33);
    let known = identity(&node_of(0xe4), 0x34);
    let newcomer = identity(&node_of(0xe5), 0x35);
    let others = [0x36_u8, 0x37, 0x38, 0x39].map(|seed| identity(&node_of(seed), seed));
    let mut book = vec![
        Peer {
            seen_ms: Some(now - LIVE_WITHIN_MS - 60_000),
            ..keyed(0xe3, silent.key(), [10, 0, 0, 9], 7100)
        },
        Peer {
            seen_ms: Some(now - 60_000),
            signed: Some(Signed::by(
                &known,
                ("m4.local", 7100, "0.146.0"),
                now - 60_000,
            )),
            ..keyed(0xe4, known.key(), [10, 0, 0, 4], 7100)
        },
    ];
    let mut served: Vec<Peer> = others
        .iter()
        .enumerate()
        .map(|(i, who)| Peer {
            addr: Some(IpAddr::from([10, 0, 0, 9])),
            ..Peer::own(who, (&format!("m{i}.local"), 7100, "0.147.0"), now)
        })
        .collect();
    served.push(Peer {
        addr: Some(IpAddr::from([10, 0, 0, 9])),
        ..Peer::own(&known, ("m4.local", 7100, "0.147.0"), now)
    });
    assert_eq!(
        merge_signed(
            &mut book,
            &served,
            (None, now),
            (None, &[], &Keyring::default())
        ),
        served.len()
    );
    assert!(
        book.iter()
            .any(|peer| peer.node.as_deref() == Some(silent.node())),
        "the silent member at 10.0.0.9 was not evicted by unsigned merged addrs: {book:?}"
    );
    let held_known = book
        .iter()
        .find(|peer| peer.node.as_deref() == Some(known.node()))
        .expect("known stays in the book");
    assert_eq!(
        held_known.addr,
        Some(IpAddr::from([10, 0, 0, 4])),
        "first-hand addr is preserved across a newer signed merge"
    );
    for other in &others {
        let merged = book
            .iter()
            .find(|peer| peer.node.as_deref() == Some(other.node()))
            .expect("merged entry in book");
        assert_eq!(merged.addr, None, "merged entry carries no unsigned addr");
    }
    let join_at_nine = Peer {
        seen_ms: Some(now),
        ..keyed(0xe5, newcomer.key(), [10, 0, 0, 9], 7101)
    };
    assert_eq!(
        record(&mut book, join_at_nine),
        Ok(Recorded::New),
        "a first-hand join from 10.0.0.9 is not crowded out by merged entries"
    );
}

/// Under a log, a signed entry under a key the log names no way (a founder's, while no record
/// binds its node key: Genesis v2 names none) is merged as with no log; one under a key it took
/// out is not.
/// NEGATIVE CONTROL: merge only keys the log admits (`log.admits`): the founder's entry is not
/// merged.
#[test]
fn under_a_log_a_founders_entry_is_merged_and_a_removed_macs_is_not() {
    let now = 1_760_000_000_000_u64;
    let founder = identity(&node_of(0xf1), 0x41);
    let removed = identity(&node_of(0xf2), 0x42);
    let log: MemberKeys = [(removed.key(), Standing::Out("removed by m7".to_owned()))]
        .into_iter()
        .collect();
    let mut book = Vec::new();
    let served = [
        Peer::own(&founder, ("m1.local", 7100, "0.146.0"), now),
        Peer::own(&removed, ("m2.local", 7100, "0.146.0"), now),
    ];
    assert_eq!(
        merge_signed(
            &mut book,
            &served,
            (None, now),
            (Some(&log), &[], &Keyring::default())
        ),
        1
    );
    assert_eq!(
        book.iter()
            .filter_map(|peer| peer.node.clone())
            .collect::<Vec<_>>(),
        vec![founder.node().to_owned()]
    );
}

/// The member this Mac joined through, back under a new node key once the log took its old one
/// out, stays the member this Mac joined through.
/// NEGATIVE CONTROL: take its entry out as any other's (`book.remove(at)`): the book names no
/// issuer after its join.
#[test]
fn the_member_joined_through_stays_that_under_its_new_key() {
    let state = State::new("rekeyedissuer");
    write_key(state.dir(), &[5; SECRET_LEN]).unwrap();
    let old = identity(&node_of(0x13), 0x51);
    let new = identity(&node_of(0x13), 0x52);
    write_book(
        state.dir(),
        &[Peer {
            key: Some(old.key()),
            ..issuer_entry(0x13, 7100)
        }],
    );
    let log: MemberKeys = [
        (
            old.key(),
            Standing::Out("its member bound another".to_owned()),
        ),
        (new.key(), Standing::Member),
    ]
    .into_iter()
    .collect();
    let outcome = join_here(state.dir(), &new, Some(&log));
    assert!(outcome.is_ok(), "{outcome:?}");
    let book = read_peers(state.dir()).unwrap();
    assert_eq!(
        book.iter()
            .map(|peer| (peer.via, peer.key))
            .collect::<Vec<_>>(),
        vec![(Via::Issuer, Some(new.key()))]
    );
}

#[test]
fn of_two_signed_entries_of_one_node_the_later_self_seq_stands() {
    let me = identity(&node_of(0xab), 1);
    let at = |port: u16, seq: u64| Peer {
        addr: Some(IpAddr::from([10, 0, 0, 7])),
        ..Peer::own(&me, ("m7.local", port, "0.146.0"), seq)
    };
    let mut book = vec![at(7100, 20)];
    assert_eq!(record(&mut book, at(7200, 10)), Ok(Recorded::Already));
    assert_eq!(book[0].port, 7100, "an earlier entry changes nothing");
    assert_eq!(record(&mut book, at(7300, 30)), Ok(Recorded::Updated));
    assert_eq!(book[0].port, 7300);
    assert!(book[0].signature_holds());
    let line = book[0].line();
    assert!(line.contains(" seq=30 sig="), "{line}");
    assert_eq!(Peer::parse(&line).as_ref(), Some(&book[0]));
    let unsigned = Peer::parse(&line.replace(" seq=30 sig=", " sig=")).is_none();
    assert!(unsigned, "a signature without its seq is no entry");
    let mut moved = book[0].clone();
    moved.host = "elsewhere.local".to_owned();
    assert!(!moved.signature_holds(), "a host changed after signing");
}

#[test]
fn a_registration_binds_only_the_key_its_connection_proved() {
    let state = State::new("joinkey");
    issue(state.dir());
    let me = identity(&node_of(0xcd), 4);
    let reg = Registration {
        node: me.node().to_owned(),
        host: "lab-mini".to_owned(),
        port: 58054,
        version: "0.105.0".to_owned(),
        key: Some(me.key()),
        signed: None,
    };
    let from = IpAddr::from(Ipv4Addr::LOCALHOST);
    let serve = |reg: &Registration, proved: Option<&NodeProof>| {
        let mut stream = Duplex {
            input: std::io::Cursor::new(reg.line().into_bytes()),
            output: Vec::new(),
        };
        let outcome = serve_join(
            &mut stream,
            (from, proved),
            (state.dir(), &secret(5)),
            || Ok(issuer(0x11)),
            7,
        )
        .expect("the stream reads and writes");
        (outcome, String::from_utf8(stream.output).unwrap())
    };
    for (proved, which) in [
        (None, "no proof"),
        (Some(proof(&node_of(0xee), me.key())), "another node"),
        (
            Some(proof(me.node(), identity(me.node(), 5).key())),
            "another key",
        ),
    ] {
        let (outcome, answer) = serve(&reg, proved.as_ref());
        assert!(
            outcome.is_err() && answer.starts_with("ERR ") && answer.contains("did not prove"),
            "{which}: {answer}"
        );
    }
    assert_eq!(
        read_peers(state.dir()).unwrap(),
        Vec::new(),
        "nothing recorded"
    );

    let (outcome, answer) = serve(&reg, Some(&proof(me.node(), me.key())));
    assert!(answer.starts_with("OK "), "{answer}");
    assert_eq!(outcome.unwrap().1.key, Some(me.key()));
    let (outcome, answer) = serve(
        &Registration {
            key: None,
            ..reg.clone()
        },
        None,
    );
    assert!(
        outcome.is_err()
            && answer.contains(&format!(
                "`aterm fleet peers rm {}` on the issuer",
                me.node()
            )),
        "{answer}"
    );

    let older = Registration {
        node: node_of(0xef),
        key: None,
        ..reg
    };
    assert_eq!(serve(&older, None).0.unwrap().1.key, None);
}

#[test]
fn a_keyless_move_never_moves_where_a_key_is_bound_from() {
    let state = State::new("keyfrom");
    issue(state.dir());
    let (real, taker) = (identity(&node_of(0xcd), 4), identity(&node_of(0xcd), 5));
    write_book(
        state.dir(),
        &[joined(0xcd, "lab-mini", [10, 0, 0, 8], 7100)],
    );
    let serve = |from: [u8; 4], key: Option<&NodeIdentity>| {
        let reg = Registration {
            node: real.node().to_owned(),
            host: "lab-mini".to_owned(),
            port: 7100,
            version: "0.105.0".to_owned(),
            key: key.map(NodeIdentity::key),
            signed: None,
        };
        let mut stream = Duplex {
            input: std::io::Cursor::new(reg.line().into_bytes()),
            output: Vec::new(),
        };
        let proved = key.map(|key| proof(key.node(), key.key()));
        serve_join(
            &mut stream,
            (IpAddr::from(from), proved.as_ref()),
            (state.dir(), &secret(5)),
            || Ok(issuer(0x11)),
            7,
        )
        .expect("the stream reads and writes")
    };
    let entry = || read_peers(state.dir()).unwrap().remove(0);
    let at = |ip: [u8; 4]| Some(IpAddr::from(ip));

    assert_eq!(serve([10, 0, 0, 66], None).unwrap().0, Recorded::Updated);
    assert_eq!(
        (entry().addr, entry().key_from, entry().key),
        (at([10, 0, 0, 66]), at([10, 0, 0, 8]), None)
    );
    let why = serve([10, 0, 0, 66], Some(&taker)).unwrap_err();
    assert!(
        why.contains("recorded from 10.0.0.8") && why.contains(BEFORE_KEYS),
        "{why}"
    );
    assert_eq!(entry().key, None, "no key bound");

    assert_eq!(
        serve([10, 0, 0, 8], Some(&real)).unwrap().0,
        Recorded::Updated
    );
    assert_eq!(
        (entry().addr, entry().key_from, entry().key),
        (at([10, 0, 0, 8]), None, Some(real.key()))
    );
    let why = serve([10, 0, 0, 66], Some(&taker)).unwrap_err();
    assert!(why.contains("another key"), "{why}");
}

#[test]
fn an_issuer_that_names_no_host_records_no_one() {
    let state = State::new("nameless");
    issue(state.dir());
    let nameless = Issuer {
        host: String::new(),
        ..issuer(0x11)
    };
    let issuing = Issuing::start(secret(5), state.dir(), nameless);
    let reg = Registration {
        node: "ab".repeat(16),
        host: "lab-mini".to_owned(),
        port: 7101,
        version: "0.95.0".to_owned(),
        key: None,
        signed: None,
    };
    assert!(join_over(issuing.port, &secret(5), &reg).is_err());
    assert_eq!(read_peers(state.dir()).unwrap(), Vec::new());
}

#[test]
fn a_join_binds_the_members_key_and_refuses_another_for_its_node() {
    let state = State::new("joinbind");
    issue(state.dir());
    let issuing_as = identity(&issuer(0x11).node, 9);
    let issuing = Issuing::proving(secret(5), state.dir(), issuer(0x11), issuing_as.clone());
    let me = identity(&node_of(0xab), 1);
    let reg = |me: &NodeIdentity| Registration {
        node: me.node().to_owned(),
        host: "lab-mini".to_owned(),
        port: 7101,
        version: "0.106.0".to_owned(),
        key: Some(me.key()),
        signed: None,
    };
    let (joined, issuer_proved) = join_as(issuing.port, (&secret(5), Some(&me)), &reg(&me));
    joined.expect("the join is answered OK");
    assert_eq!(
        issuer_proved,
        Some(proof(issuing_as.node(), issuing_as.key()))
    );
    let book = read_peers(state.dir()).unwrap();
    assert_eq!(
        book.iter()
            .map(|p| (p.node.clone(), p.key))
            .collect::<Vec<_>>(),
        vec![(Some(me.node().to_owned()), Some(me.key()))]
    );

    let taker = identity(me.node(), 2);
    let why = join_as(issuing.port, (&secret(5), Some(&taker)), &reg(&taker))
        .0
        .unwrap_err();
    assert!(
        why.contains("another key")
            && why.contains(&format!(
                "`aterm fleet peers rm {}` on the issuer",
                taker.node()
            )),
        "{why}"
    );
    let unproved = join_as(issuing.port, (&secret(5), None), &reg(&taker))
        .0
        .unwrap_err();
    assert!(unproved.contains("did not prove"), "{unproved}");
    assert_eq!(read_peers(state.dir()).unwrap(), book, "its entry stands");

    update_peers(state.dir(), |book| {
        remove(book, &[], "abababab").map(|_| ())
    })
    .unwrap();
    join_as(issuing.port, (&secret(5), Some(&taker)), &reg(&taker))
        .0
        .expect("once removed, it joins afresh");
    assert_eq!(read_peers(state.dir()).unwrap()[0].key, Some(taker.key()));
}

#[test]
fn a_check_gives_a_reason_per_peer() {
    let state = State::new("reach");
    let member = Issuing::start(secret(5), state.dir(), issuer(0x11));
    let stranger = Issuing::start(secret(6), state.dir(), issuer(0x22));
    let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);
    let at = |port| manual("127.0.0.1", port);
    let peers = [at(member.port), at(closed_port), at(stranger.port)];
    let check = |peers: &[Peer], within| {
        let (reads, book) = read_whole(
            peers,
            &Keyring::of(peers),
            None,
            (&secret(5), None),
            within,
            "survey facts",
        );
        assert!(matches!(book, BookRead::NoIssuer), "{book:?}");
        assert_eq!(
            reads.iter().map(|read| &read.peer).collect::<Vec<_>>(),
            peers.iter().collect::<Vec<_>>(),
            "one answer per peer, in order"
        );
        reads
            .into_iter()
            .map(|read| read.read.map(|surveyed| surveyed.checked()))
            .collect::<Vec<_>>()
    };
    let results = check(&peers, HANG);
    assert!(
        matches!(
            &results[0],
            Ok(Checked { scope: FleetScope::Content, node: Some(node), .. })
                if *node == issuer(0x11).node
        ),
        "the check names the node that answered: {:?}",
        results[0]
    );
    assert!(
        matches!(
            &results[1],
            Err(Unreached::Connect(addr, e))
                if e.kind() == io::ErrorKind::ConnectionRefused && addr.port() == closed_port
        ),
        "{:?}",
        results[1]
    );
    assert!(
        matches!(
            results[2],
            Err(Unreached::Handshake(_, FleetError::ListenerWrongSecret))
        ),
        "{:?}",
        results[2]
    );

    let silent = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let silent_port = silent.local_addr().unwrap().port();
    let results = check(&[at(silent_port)], Duration::from_millis(300));
    assert!(
        matches!(results[..], [Err(Unreached::Deadline(Stage::Handshake, _))]),
        "{results:?}"
    );
    drop(silent);
}

fn issuer_entry(node: u8, port: u16) -> Peer {
    Peer {
        via: Via::Issuer,
        ..joined(node, "127.0.0.1", [127, 0, 0, 1], port)
    }
}

fn write_book(state: &Path, peers: &[Peer]) {
    write_private(state, PEERS_FILE, book_text(peers).as_bytes()).unwrap();
}

#[test]
fn a_fleets_members_share_toolsets_at_their_lan_addresses_most_recently_heard_first() {
    let state = State::new("members");
    let key = |node: u8| identity(&node_of(node), node).key();
    let heard = |node: u8, ip: [u8; 4], seen_ms: u64| Peer {
        seen_ms: Some(seen_ms),
        ..keyed(node, key(node), ip, 7100)
    };
    write_book(
        state.dir(),
        &[
            heard(0xa1, [10, 0, 0, 1], 1),
            heard(0xb2, [192, 168, 1, 2], 3),
            heard(0xc3, [127, 0, 0, 1], 4),
            heard(0xd4, [8, 8, 8, 8], 5),
            heard(0xe5, [10, 0, 0, 5], 2),
            joined(0xf6, "old.local", [10, 0, 0, 6], 7100),
            manual("10.0.0.7", 7100),
        ],
    );
    assert_eq!(
        member_addrs(state.dir()),
        Vec::<IpAddr>::new(),
        "no fleet key is held, so no member is"
    );
    write_key(state.dir(), &[0x3e; SECRET_LEN]).unwrap();
    write_private(
        state.dir(),
        NODE_FILE,
        format!("{}\n", node_of(0xe5)).as_bytes(),
    )
    .unwrap();
    assert_eq!(
        member_addrs(state.dir()),
        [IpAddr::from([192, 168, 1, 2]), IpAddr::from([10, 0, 0, 1])],
        "the bound members on the LAN, the most recently heard first: not this node, a \
         loopback or public address, a node no key binds, nor a person's entry"
    );
    assert_eq!(
        member_addrs_with_denied(state.dir(), &[node_of(0xb2)], None),
        [IpAddr::from([10, 0, 0, 1])],
        "denied node_id is excluded from LAN toolset sharing"
    );
    assert_eq!(
        member_addrs_with_denied(state.dir(), &[key(0xa1).hex().to_uppercase()], None),
        [IpAddr::from([192, 168, 1, 2])],
        "denied 64-hex node_key is excluded from LAN toolset sharing"
    );
    let log: MemberKeys = [
        (key(0xa1), Standing::Member),
        (key(0xb2), Standing::Out("removed".to_owned())),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        member_addrs_with_denied(state.dir(), &[], Some(&log)),
        [IpAddr::from([10, 0, 0, 1])],
        "log-held-out key is excluded from LAN toolset sharing"
    );
}

fn closed_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// In a test binary, [`default_port`] is never an aterm's that runs the tests: it is not
/// [`DEFAULT_PORT`], and a dial there is refused. Only a test binary holds such a port: the
/// installed app and a debug `aterm` hold none, so a fleet's place there is [`DEFAULT_PORT`].
#[test]
fn a_test_binarys_default_port_is_none_an_aterm_here_serves_and_refuses_a_dial() {
    for exe in [
        "/Applications/aterm.app/Contents/MacOS/aterm",
        "/Users//me/aterm/target/debug/aterm",
    ] {
        assert!(hold(Path::new(exe)).is_none(), "{exe}");
    }
    assert_ne!(default_port(), DEFAULT_PORT);
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, default_port()));
    let dialled = TcpStream::connect_timeout(&at, HANG);
    assert_eq!(
        dialled.as_ref().err().map(io::Error::kind),
        Some(io::ErrorKind::ConnectionRefused),
        "{at}: {dialled:?}"
    );
}

/// A place that names [`DEFAULT_PORT`] itself, as a book, a `[fleet] listen` or a word can, comes
/// by no [`default_port`]: the person's aterm answers there, under another secret, so a test
/// binary's dial stops before its connect. The heartbeat's dial of a member its book holds there
/// is one (`reach_node` for a join: an aterm_gui test binary's node job dialled 127.0.0.1:7100 and
/// then its member's name there, 2026-11-02). The cut answers any dial past that, so this test
/// reaches that aterm with the guard gone too.
#[test]
fn a_test_binary_dials_no_default_port_however_the_place_is_named() {
    let me = identity(&node_of(0x3a), 8);
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_PORT));
    let cut = format!("{at}\n[::1]:{DEFAULT_PORT}\n");
    let target = Target {
        host: "localhost".to_owned(),
        addr: Some(at.ip()),
        port: at.port(),
        node: Some(me.node().to_owned()),
        key: Some(me.key()),
    };
    let reads = aterm_uds::partition::cut_here(&cut, || {
        [
            dial_ask("127.0.0.1", None, at.port(), HANG).map(|_| ()),
            dial_ask("127.0.0.1", Some(at.ip()), at.port(), HANG).map(|_| ()),
            dial(
                "localhost",
                None,
                at.port(),
                (&secret(6), Some(&me)),
                FleetOp::Book,
                HANG,
            )
            .map(|_| ()),
            proves_itself(&target, &Keyring::default(), (&secret(6), &me), HANG).map(|_| ()),
            reach_node(
                &target,
                &Keyring::default(),
                (&secret(6), &me),
                FleetOp::Join,
                (deadline_after(HANG), HANG),
            )
            .map(|_| ()),
        ]
    });
    for read in reads {
        let why = read.expect_err("no dial");
        assert!(
            matches!(&why, Unreached::Local(said) if said.contains(&format!(":{DEFAULT_PORT}"))),
            "{why:?}"
        );
    }
}

#[test]
fn every_member_serves_its_book_with_or_without_a_code() {
    let state = State::new("book");
    let first_hand = [
        joined(0x22, "m2.local", [10, 0, 0, 2], 7100),
        manual("lab-mini", 7101),
    ];
    let mut held = first_hand.to_vec();
    held.push(Peer {
        via: Via::Issuer,
        ..joined(0x33, "m3", [10, 0, 0, 3], 7100)
    });
    held.push(copy(joined(0x44, "m4", [10, 0, 0, 4], 7100)));
    write_book(state.dir(), &held);
    let issuing = Issuing::start(secret(5), state.dir(), issuer(0x11));
    let target = target_of(&issuer_entry(0x11, issuing.port));
    let deadline = || (Instant::now() + HANG, HANG);

    let mut shared = first_hand.to_vec();
    shared.push(joined(0x33, "m3", [10, 0, 0, 3], 7100));
    let book = fetch_book(&target, (&secret(5), None), deadline()).expect("a member's book");
    assert_eq!(book.node, issuer(0x11).node);
    assert_eq!(
        book.peers, shared,
        "no code here, and the book is shared: first-hand entries in order, the member it joined \
         through as one, never a copy"
    );
    assert_eq!(book.at, IpAddr::from(Ipv4Addr::LOCALHOST));

    issue(state.dir());
    let book = fetch_book(&target, (&secret(5), None), deadline()).expect("the issuer's book");
    assert_eq!(book.peers, shared, "a code changes nothing of it");

    let someone_else = Target {
        node: Some(node_of(0x99)),
        key: None,
        ..target.clone()
    };
    assert!(
        matches!(
            fetch_book(&someone_else, (&secret(5), None), deadline()),
            Err(BookUnread::OtherNode { said, .. }) if said == issuer(0x11).node
        ),
        "another node's book is no book of this member's issuer"
    );
    let stranger = fetch_book(&target, (&secret(6), None), deadline());
    assert!(
        matches!(
            &stranger,
            Err(BookUnread::Unreached(Unreached::Handshake(
                _,
                FleetError::ListenerWrongSecret
            )))
        ),
        "{stranger:?}"
    );

    write_private(state.dir(), PEERS_FILE, b"peer garbage\n").unwrap();
    let unshared = fetch_book(&target, (&secret(5), None), deadline());
    assert!(
        matches!(&unshared, Err(BookUnread::NotNow { node: Some(node), .. }) if *node == issuer(0x11).node),
        "{unshared:?}"
    );
}

#[test]
fn a_book_reply_carries_the_held_seed_in_a_trailer_an_older_reader_never_reads() {
    let state = State::new("book-seed");
    let first_hand = [
        joined(0x22, "m2.local", [10, 0, 0, 2], 7100),
        manual("lab-mini", 7101),
    ];
    write_book(state.dir(), &first_hand);
    let issuing = Issuing::start(secret(5), state.dir(), issuer(0x11));
    let target = target_of(&issuer_entry(0x11, issuing.port));
    let deadline = || (Instant::now() + HANG, HANG);
    let unseeded = fetch_book(&target, (&secret(5), None), deadline()).expect("a book");
    assert_eq!((unseeded.peers.len(), unseeded.seed), (2, None));
    let mut wire = Vec::new();
    let node = issuer(0x11).node;
    serve_book(&mut wire, state.dir(), || Ok(node.clone()))
        .unwrap()
        .unwrap();
    let text = String::from_utf8(wire).unwrap();
    assert!(!text.contains("more="), "no trailer, no count: {text}");

    let root = NodeIdentity::from_seed(&"11".repeat(16), &[0x11; 32]).unwrap();
    let seed = fleet_seed::FleetSeed::mint(&root, secret(5).mark(), 1_793_000_000_000).unwrap();
    write_private(
        state.dir(),
        fleet_seed::FILE,
        format!("{}\n", seed.words()).as_bytes(),
    )
    .unwrap();
    let seeded = fetch_book(&target, (&secret(5), None), deadline()).expect("a book");
    assert_eq!(seeded.seed, Some(seed));
    assert_eq!(seeded.peers, unseeded.peers, "the trailer is no peer");

    let mut wire = Vec::new();
    assert_eq!(
        serve_book(&mut wire, state.dir(), || Ok(node.clone())).unwrap(),
        Ok(2),
        "the peer lines it sent"
    );
    let text = String::from_utf8(wire).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], format!("OK 2 node={node} more=1"));
    assert_eq!(book_head(lines[0]), Ok(3));
    // A reader before the trailer takes the head's first count and returns after that many
    // lines, so the seed's line is never read as an entry.
    let older_n: usize = lines[0].split(' ').nth(1).unwrap().parse().unwrap();
    assert_eq!(older_n, 2);
    assert!(
        lines[1..=older_n]
            .iter()
            .all(|line| Peer::parse(line).is_some())
    );
    assert_eq!(lines[older_n + 1..], [seed.words().as_str()]);
    assert_eq!(Peer::parse(lines[older_n + 1]), None);

    let twice: &'static str =
        Box::leak(format!("OK 0 node={node} more=2\n{0}\n{0}\n", seed.words()).into_boxed_str());
    let doubled = Issuing::hooked(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        secret(5),
        state.dir(),
        issuer(0x11),
        EVERY_OP,
        Arc::new(move |op| (op == FleetOp::Book).then_some(twice)),
    );
    let book = fetch_book(
        &target_of(&issuer_entry(0x11, doubled.port)),
        (&secret(5), None),
        deadline(),
    )
    .expect("a book");
    assert_eq!(book.seed, None, "a trailer with two seeds carries none");

    let later: &'static str = Box::leak(
        format!(
            "OK 0 node={node} more={}\n{}{}\n",
            TRAILER_LINES_MAX + 2,
            "tombstone x\n".repeat(TRAILER_LINES_MAX + 1),
            seed.words()
        )
        .into_boxed_str(),
    );
    let longer = Issuing::hooked(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        secret(5),
        state.dir(),
        issuer(0x11),
        EVERY_OP,
        Arc::new(move |op| (op == FleetOp::Book).then_some(later)),
    );
    let book = fetch_book(
        &target_of(&issuer_entry(0x11, longer.port)),
        (&secret(5), None),
        deadline(),
    )
    .expect("a later aterm's book");
    assert_eq!(
        book.seed,
        Some(seed),
        "its trailer's lines this aterm does not know are passed over"
    );
}

#[test]
fn a_book_of_long_lines_sends_fewer_peers_and_keeps_room_for_its_trailer() {
    let line = "x".repeat(RECORD_MAX - 1);
    let trailer = vec!["t".repeat(TRAILER_MAX / TRAILER_LINES_MAX - 1); TRAILER_LINES_MAX + 2];
    let named = format!("node={}", node_of(0x11));
    let (reply, served) = book_reply(
        &named,
        std::iter::repeat_n(line.clone(), PEERS_MAX),
        &trailer,
    );
    assert!(served < PEERS_MAX, "{served}");
    assert!(reply.len() <= REPLY_MAX, "{}", reply.len());
    assert!(
        BOOK_HEAD_MAX + (served + 1) * (line.len() + 1) + TRAILER_MAX > REPLY_MAX,
        "one more peer line would eat into the trailer's room"
    );
    let lines: Vec<&str> = reply.lines().collect();
    assert_eq!(
        lines[0],
        format!("OK {served} {named} more={TRAILER_LINES_MAX}")
    );
    assert_eq!(book_head(lines[0]), Ok(served + TRAILER_LINES_MAX));
    assert_eq!(
        lines.len(),
        1 + served + TRAILER_LINES_MAX,
        "the trailer's first lines"
    );
    let widest = format!(
        "OK {PEERS_MAX} node={} more={TRAILER_LINES_MAX}\n",
        "f".repeat(64)
    );
    assert!(widest.len() <= BOOK_HEAD_MAX, "{}", widest.len());

    let halves = vec!["h".repeat(TRAILER_MAX / 2); 2];
    let (reply, _) = book_reply(&named, std::iter::empty(), &halves);
    assert_eq!(
        reply,
        format!("OK 0 {named} more=1\n{}\n", halves[0]),
        "the trailer's lines while they fit its room"
    );
    let short = vec!["s".to_owned(); TRAILER_LINES_MAX + 5];
    let (reply, _) = book_reply(&named, std::iter::empty(), &short);
    assert_eq!(
        reply.lines().next(),
        Some(format!("OK 0 {named} more={TRAILER_LINES_MAX}").as_str()),
        "short lines stop at the line count, not the bytes"
    );
    let (reply, served) = book_reply(&named, std::iter::repeat_n("p".to_owned(), 3), &[]);
    assert_eq!((reply, served), (format!("OK 3 {named}\np\np\np\n"), 3));
}

#[test]
fn a_book_head_counts_its_trailer_once_and_within_bounds() {
    assert_eq!(book_head("OK 3 node=ab more=1"), Ok(4));
    assert_eq!(book_head("OK 3 more=0"), Ok(3));
    assert_eq!(book_counts("OK 3 node=ab more=2"), Ok((3, 2)));
    assert_eq!(
        book_head(&format!("OK 0 more={TRAILER_READ_MAX}")),
        Ok(TRAILER_READ_MAX),
        "a later aterm's longer trailer is read"
    );
    for refused in [
        "OK 3 more=+1".to_owned(),
        "OK 3 more=".to_owned(),
        "OK 3 more=1 more=1".to_owned(),
        format!("OK 3 more={}", TRAILER_READ_MAX + 1),
        format!("OK {} more=1", PEERS_MAX + 1),
        "OK more=1".to_owned(),
    ] {
        assert!(book_head(&refused).is_err(), "{refused}");
    }
    assert_eq!(book_head("ERR not-now more=9"), Ok(0));
}

#[test]
fn an_older_issuer_leaves_its_member_reading_what_it_knows() {
    let state = State::new("older");
    issue(state.dir());
    let older = Issuing::serving(secret(5), state.dir(), issuer(0x11), OLDER_OPS);
    let target = target_of(&issuer_entry(0x11, older.port));
    assert!(matches!(
        fetch_book(&target, (&secret(5), None), (Instant::now() + HANG, HANG)),
        Err(BookUnread::Older(at)) if at.port() == older.port
    ));
    let stale = copy(joined(0x55, "127.0.0.1", [127, 0, 0, 1], closed_port()));
    let peers = [issuer_entry(0x11, older.port), stale];
    let (reads, book) = read_whole(
        &peers,
        &Keyring::of(&peers),
        None,
        (&secret(5), None),
        HANG,
        "survey",
    );
    assert!(
        matches!(book, BookRead::Refused(BookUnread::Older(_))),
        "the issuer's own survey answered where `NO op` came from: {book:?}"
    );
    assert_eq!(reads.len(), 1, "the stale copy is left out: {reads:?}");
    assert_eq!(reads[0].peer, peers[0]);
    assert!(
        matches!(&reads[0].read, Ok(surveyed) if surveyed.node.as_deref() == Some(issuer(0x11).node.as_str())),
        "the issuer's own survey is read: {:?}",
        reads[0].read
    );
}

#[test]
fn an_older_issuers_no_op_ends_the_dial_there() {
    let state = State::new("no-op");
    issue(state.dir());
    let (v6, v4, port) = twin_listeners();
    let _newer = Issuing::on(v6, secret(5), state.dir(), issuer(0x11), EVERY_OP);
    let _older = Issuing::on(v4, secret(5), state.dir(), issuer(0x11), OLDER_OPS);
    let issuer_at = |addr: IpAddr| Target {
        host: "::1".to_owned(),
        addr: Some(addr),
        port,
        node: Some(issuer(0x11).node),
        key: None,
    };
    let deadline = || (Instant::now() + HANG, HANG);
    let read = fetch_book(
        &issuer_at(IpAddr::from(Ipv4Addr::LOCALHOST)),
        (&secret(5), None),
        deadline(),
    );
    assert!(
        matches!(read, Err(BookUnread::Older(at)) if at.is_ipv4()),
        "{read:?}"
    );
    let control = fetch_book(
        &issuer_at(IpAddr::from(std::net::Ipv6Addr::LOCALHOST)),
        (&secret(5), None),
        deadline(),
    );
    assert!(control.is_ok(), "{control:?}");
}

#[test]
fn a_member_reads_every_member_its_issuers_book_names() {
    let issuer_state = State::new("whole-issuer");
    let other_state = State::new("whole-other");
    let other = Issuing::start(secret(5), other_state.dir(), issuer(0x22));
    let named = closed_port();
    let own = node_of(0xaa);
    write_book(
        issuer_state.dir(),
        &[
            joined(0xaa, "127.0.0.1", [127, 0, 0, 1], closed_port()),
            joined(0x22, "127.0.0.1", [127, 0, 0, 1], other.port),
            manual("127.0.0.1", named),
        ],
    );
    issue(issuer_state.dir());
    let issuing = Issuing::start(secret(5), issuer_state.dir(), issuer(0x11));
    let stale = copy(joined(0x55, "127.0.0.1", [127, 0, 0, 1], closed_port()));
    let peers = [issuer_entry(0x11, issuing.port), stale];

    let (reads, book) = read_whole(
        &peers,
        &Keyring::of(&peers),
        Some(&own),
        (&secret(5), None),
        HANG,
        "survey",
    );
    let BookRead::Fetched(book) = book else {
        panic!("the issuer's book: {book:?}");
    };
    assert_eq!(book.peers.len(), 3);
    let read: Vec<(Via, Option<&str>, u16)> = reads
        .iter()
        .map(|read| (read.peer.via, read.peer.node.as_deref(), read.peer.port))
        .collect();
    assert_eq!(
        read,
        [
            (Via::Issuer, Some(issuer(0x11).node.as_str()), issuing.port),
            (
                Via::IssuerBook,
                Some(issuer(0x22).node.as_str()),
                other.port
            ),
            (Via::IssuerBook, None, named),
        ],
        "the issuer, then the book's members; not this machine, not the stale copy"
    );
    let answered = |at: usize| match &reads[at].read {
        Ok(surveyed) => surveyed.node.clone(),
        Err(e) => panic!("{at}: {e}"),
    };
    assert_eq!(answered(0), Some(issuer(0x11).node));
    assert_eq!(answered(1), Some(issuer(0x22).node));
    assert!(
        matches!(&reads[2].read, Err(Unreached::Connect(_, e)) if e.kind() == io::ErrorKind::ConnectionRefused),
        "{:?}",
        reads[2].read
    );
}

#[test]
fn a_copy_the_book_keeps_at_its_place_is_listed_as_the_book_has_it_now() {
    let issuer_state = State::new("fresh-issuer");
    let other_state = State::new("fresh-other");
    let other = Issuing::start(secret(5), other_state.dir(), issuer(0x22));
    let member = joined(0x22, "127.0.0.1", [127, 0, 0, 1], other.port);
    write_book(
        issuer_state.dir(),
        &[Peer {
            seen_ms: Some(1_760_000_100_000),
            version: Some("0.96.0".to_owned()),
            ..member.clone()
        }],
    );
    issue(issuer_state.dir());
    let issuing = Issuing::start(secret(5), issuer_state.dir(), issuer(0x11));
    let peers = [issuer_entry(0x11, issuing.port), copy(member)];

    let (reads, book) = read_whole(
        &peers,
        &Keyring::of(&peers),
        Some(&node_of(0xaa)),
        (&secret(5), None),
        HANG,
        "survey",
    );
    assert!(matches!(book, BookRead::Fetched(_)), "{book:?}");
    let read: Vec<(Via, Option<u64>, Option<&str>)> = reads
        .iter()
        .map(|read| {
            (
                read.peer.via,
                read.peer.seen_ms,
                read.peer.version.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        read[1..],
        [(Via::IssuerBook, Some(1_760_000_100_000), Some("0.96.0"))],
        "the book's entry, not the copy it replaced"
    );
    assert!(reads[1].read.is_ok(), "{:?}", reads[1].read);
}

#[test]
fn a_books_keys_are_copied_only_from_a_proven_issuer() {
    let state = State::new("bookkeys");
    let member = identity(&node_of(0xab), 1);
    write_book(
        state.dir(),
        &[keyed(0xab, member.key(), [10, 0, 0, 7], 7100)],
    );
    issue(state.dir());
    let issuing_as = identity(&issuer(0x11).node, 9);
    let issuing = Issuing::proving(secret(5), state.dir(), issuer(0x11), issuing_as.clone());
    let me = identity(&node_of(0x77), 7);
    let fetch = |key: Option<NodeKey>| {
        let target = Target {
            host: "127.0.0.1".to_owned(),
            addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
            port: issuing.port,
            node: Some(issuer(0x11).node),
            key,
        };
        fetch_book(
            &target,
            (&secret(5), Some(&me)),
            (Instant::now() + HANG, HANG),
        )
    };
    let keys_copied = |book: &Book| {
        copies_of(book, None)
            .iter()
            .map(|c| c.key)
            .collect::<Vec<_>>()
    };

    let book = fetch(Some(issuing_as.key())).expect("the issuer's book");
    assert!(book.proven);
    assert_eq!(keys_copied(&book), vec![Some(member.key())]);
    let claimed = fetch(None).expect("the issuer's book");
    assert!(!claimed.proven);
    assert_eq!(
        keys_copied(&claimed),
        vec![None],
        "a claim's keys are no keys"
    );
    let other = fetch(Some(identity(&issuer(0x11).node, 8).key()));
    assert!(
        matches!(&other, Err(BookUnread::Unreached(Unreached::Impostor { node, issuer: true, .. })) if *node == issuer(0x11).node),
        "{other:?}"
    );
}

#[test]
fn a_merge_replaces_the_copy_whole_dedupes_and_is_bounded() {
    let issuer_at = IpAddr::from([10, 0, 0, 7]);
    let own_issuer = Peer {
        via: Via::Issuer,
        ..joined(0x11, "m7.local", [10, 0, 0, 7], 7100)
    };
    let mine = vec![
        own_issuer.clone(),
        manual("lab-mini", 7101),
        copy(joined(0x99, "gone", [10, 0, 0, 9], 7100)),
    ];
    let fetched = Book {
        node: node_of(0x11),
        proven: true,
        seed: None,
        at: issuer_at,
        peers: vec![
            joined(0xaa, "me", [10, 0, 0, 10], 7100),
            joined(0x11, "m7.local", [10, 0, 0, 7], 7100),
            joined(0x22, "m2", [10, 0, 0, 2], 7100),
            joined(0x22, "m2-again", [10, 0, 0, 22], 7100),
            manual("lab-mini", 7101),
            joined(0x33, "lab-mini", [10, 0, 0, 3], 7101),
            Peer {
                via: Via::Issuer,
                ..joined(0x34, "m34", [10, 0, 0, 34], 7100)
            },
            copy(joined(0x35, "m35", [10, 0, 0, 35], 7100)),
            manual("127.0.0.1", 7105),
            joined(0x44, "m4", [127, 0, 0, 1], 7106),
        ],
    };
    let mut book = mine.clone();
    merge_book(&mut book, &fetched, Some(&node_of(0xaa)));
    let relocated = Peer {
        host: "10.0.0.7".to_owned(),
        addr: Some(issuer_at),
        ..copy(manual("127.0.0.1", 7105))
    };
    let sibling = Peer {
        addr: Some(issuer_at),
        ..copy(joined(0x44, "m4", [127, 0, 0, 1], 7106))
    };
    assert_eq!(
        book,
        [
            own_issuer.clone(),
            manual("lab-mini", 7101),
            copy(joined(0x22, "m2", [10, 0, 0, 2], 7100)),
            copy(joined(0x33, "lab-mini", [10, 0, 0, 3], 7101)),
            relocated,
            sibling,
        ]
    );
    let once = book.clone();
    merge_book(&mut book, &fetched, Some(&node_of(0xaa)));
    assert_eq!(book, once, "a merge twice is the merge once");

    let mut looped = vec![own_issuer.clone()];
    merge_book(
        &mut looped,
        &Book {
            at: IpAddr::from(Ipv4Addr::LOCALHOST),
            ..fetched.clone()
        },
        Some(&node_of(0xaa)),
    );
    assert!(
        looped.contains(&copy(manual("127.0.0.1", 7105))),
        "{looped:?}"
    );

    let crowd = Book {
        peers: (0..PEERS_MAX + 10)
            .map(|n| Peer {
                node: Some(format!("{:032x}", n + 0x1000)),
                key: None,
                ..joined(
                    0,
                    &format!("m{n}"),
                    [10, 1, (n / 256) as u8, (n % 256) as u8],
                    7100,
                )
            })
            .collect(),
        ..fetched.clone()
    };
    let mut full = mine;
    merge_book(&mut full, &crowd, Some(&node_of(0xaa)));
    assert_eq!(full.len(), PEERS_MAX);
    assert_eq!(full[..2], [own_issuer, manual("lab-mini", 7101)]);
    assert_eq!(
        full.iter()
            .filter(|peer| peer.via == Via::IssuerBook)
            .count(),
        PEERS_MAX - 2
    );
}

#[test]
fn a_copy_of_the_issuers_book_yields_to_the_machines_own_entries() {
    let own = vec![issuer_entry(0x11, 7100), manual("LAB-MINI", 7101)];
    let copies = vec![
        copy(manual("lab-mini", 7101)),
        copy(joined(0x22, "m2", [10, 0, 0, 2], 7100)),
        copy(joined(0x11, "m7-again", [10, 0, 0, 77], 7100)),
        copy(joined(0x33, "lab-mini.local", [10, 0, 0, 3], 7101)),
    ];
    let mut book = own.clone();
    assert!(remove(&mut book, &copies, &node_of(0x22)).is_err());
    assert!(remove(&mut book, &copies, "m2").is_err());
    assert_eq!(book, own, "nothing taken out by hand");
    assert_eq!(
        remove(&mut book, &copies, "lab-mini").map(|peer| peer.via),
        Ok(Via::Manual),
        "the machine's own entry, never the copy at its place"
    );
    assert!(record(&mut book, copy(manual("m9", 7100))).is_err());

    let mut read = own.clone();
    with_copies(&mut read, copies.clone());
    assert_eq!(
        read,
        [
            own[0].clone(),
            own[1].clone(),
            copies[1].clone(),
            copies[3].clone()
        ],
        "a node of the book's own stands for its copy, a place for one that names none"
    );

    let crowd = |n: usize| -> Vec<Peer> {
        (0..n)
            .map(|n| Peer {
                node: Some(format!("{:032x}", n + 0x1000)),
                key: None,
                ..joined(0, "m", [10, 1, (n / 256) as u8, (n % 256) as u8], 7100)
            })
            .collect()
    };
    let mut nearly = crowd(PEERS_MAX - 1);
    with_copies(&mut nearly, copies.iter().skip(1).cloned());
    assert_eq!(nearly.len(), PEERS_MAX);
    assert_eq!(nearly[..PEERS_MAX - 1], crowd(PEERS_MAX - 1)[..]);
    assert_eq!(nearly[PEERS_MAX - 1], copies[1]);
    let mut full = crowd(PEERS_MAX);
    with_copies(&mut full, copies);
    assert_eq!(
        full,
        crowd(PEERS_MAX),
        "a full book leaves no room for a copy"
    );
}

#[test]
fn a_word_that_names_the_issuer_and_a_copy_removes_neither() {
    let own = vec![issuer_entry(0x11, 48401)];
    let copies: Vec<Peer> = [(0x22, 48403), (0x33, 48404), (0x44, 48408)]
        .into_iter()
        .map(|(node, port)| copy(joined(node, "127.0.0.1", [127, 0, 0, 1], port)))
        .collect();
    let mut book = own.clone();
    let refused = remove(&mut book, &copies, "127.0.0.1").unwrap_err();
    assert_eq!(
        refused,
        "`127.0.0.1` names 4 peers (11111111111111111111111111111111 at 127.0.0.1:48401, \
         22222222222222222222222222222222 at 127.0.0.1:48403, \
         33333333333333333333333333333333 at 127.0.0.1:48404, \
         44444444444444444444444444444444 at 127.0.0.1:48408): name one by its node id or its address and port"
    );
    assert_eq!(book, own, "nothing taken out");
    let read: Vec<Peer> = own.iter().chain(&copies).cloned().collect();
    assert_eq!(
        named(&own, &copies, "127.0.0.1"),
        Err(find(&read, "127.0.0.1").unwrap_err()),
        "one sentence, as a drive's word is refused"
    );
    assert!(remove(&mut book, &copies, "127.0.0.1:48403").is_err());

    let by_hand = manual("127.0.0.1", 48409);
    book.push(by_hand.clone());
    assert_eq!(
        remove(&mut book, &copies, "127.0.0.1"),
        Err(
            "`127.0.0.1` names 5 peers (11111111111111111111111111111111 at 127.0.0.1:48401, \
             22222222222222222222222222222222 at 127.0.0.1:48403, \
             33333333333333333333333333333333 at 127.0.0.1:48404, \
             44444444444444444444444444444444 at 127.0.0.1:48408, - at 127.0.0.1:48409): name one by its node id or \
             its address and port"
                .to_owned()
        ),
        "an entry named by hand elsewhere is one more peer"
    );
    let alone = &copies[..1];
    let two =
        "`127.0.0.1` names 2 peers (22222222222222222222222222222222 at 127.0.0.1:48403, - at \
               127.0.0.1:48409): name one by its node id or its address and port"
            .to_owned();
    assert_eq!(
        named(std::slice::from_ref(&by_hand), alone, "127.0.0.1"),
        Err(two.clone())
    );
    assert_eq!(
        remove(&mut vec![by_hand.clone()], alone, "127.0.0.1"),
        Err(two),
        "never the entry of a peer other than the one the word names"
    );
    assert!(remove(&mut book, &[], "127.0.0.1").is_err());
    let at_its_place = [copy(manual("127.0.0.1", 48403))];
    assert_eq!(
        named(
            &book[..1],
            &[alone, &at_its_place].concat(),
            "127.0.0.1:48403"
        ),
        Ok(Some(&alone[0])),
        "an entry with no node at a node's place stands for that node"
    );
    assert_eq!(remove(&mut book, &copies, "127.0.0.1:48409"), Ok(by_hand));
    assert_eq!(
        remove(&mut book, &copies, "127.0.0.1:48401").map(|peer| peer.via),
        Ok(Via::Issuer)
    );

    let mut book = own.clone();
    let copied_by_hand = [copy(manual("127.0.0.1", 5555))];
    assert_eq!(
        remove(&mut book, &copied_by_hand, "127.0.0.1"),
        Err(
            "`127.0.0.1` names 2 peers (11111111111111111111111111111111 at 127.0.0.1:48401, - at \
             127.0.0.1:5555): name one by its node id or its address and port"
                .to_owned()
        ),
        "a hand entry the copy holds is another peer too"
    );
    assert_eq!(book, own, "the issuer stays");
}

#[test]
fn a_member_that_names_anothers_place_takes_neither_its_copy_nor_its_key() {
    let (y, z) = (identity(&node_of(0x5a), 1), identity(&node_of(0x5b), 2));
    let at_y = |peer: Peer| Peer {
        host: "y-mac.local".to_owned(),
        ..peer
    };
    let y_entry = at_y(keyed(0x5a, y.key(), [10, 0, 0, 5], 7100));
    let z_entry = at_y(keyed(0x5b, z.key(), [10, 0, 0, 66], 7100));
    let i = identity(&node_of(0x11), 3);
    let own = node_of(0xaa);
    for (order, members) in [
        ("ahead", [&z_entry, &y_entry]),
        ("after", [&y_entry, &z_entry]),
    ] {
        let mut peers: Vec<Peer> = members.into_iter().cloned().collect();
        peers.push(manual("y-mac", 7100));
        let fetched = Book {
            node: i.node().to_owned(),
            proven: true,
            seed: None,
            at: IpAddr::from([10, 0, 0, 7]),
            peers,
        };
        let copies = copies_of(&fetched, Some(&own));
        assert_eq!(
            copies,
            members.map(|member| copy(member.clone())),
            "{order}: both members, and no entry by hand at their place"
        );
        let state = State::new("placekeys");
        write_book(
            state.dir(),
            &[Peer {
                key: Some(i.key()),
                ..issuer_entry(0x11, 7100)
            }],
        );
        keep_copy(state.dir(), i.node(), &copies).unwrap();
        for (ring, read) in [
            (Keyring::read(state.dir()), "the kept copy"),
            (
                Keyring::default().with_book(&fetched, Some(&own)),
                "a fresh book",
            ),
        ] {
            for member in [&y, &z] {
                assert_eq!(
                    ring.proven(Some(&proof(member.node(), member.key()))),
                    Ok(member.node().to_owned()),
                    "{order}, {read}"
                );
            }
        }
    }
}

#[test]
fn a_copy_is_read_only_beside_the_issuer_it_came_from() {
    let state = State::new("copy-file");
    let (issuer, other) = (node_of(0x11), node_of(0x99));
    let copies = vec![
        copy(joined(0x22, "m2", [10, 0, 0, 2], 7100)),
        copy(manual("lab-mini", 7101)),
    ];
    assert!(read_issuer_book(state.dir(), &issuer).is_empty(), "no file");
    let mut written = copies.clone();
    written.push(manual("not-a-copy", 7100));
    keep_copy(state.dir(), &issuer, &written).unwrap();
    assert_eq!(read_issuer_book(state.dir(), &issuer), copies);
    assert!(
        read_issuer_book(state.dir(), &other).is_empty(),
        "another issuer's copy is none of this one's"
    );
    let text =
        String::from_utf8(read_private(state.dir(), ISSUER_BOOK_FILE, 4096).unwrap()).unwrap();
    write_private(
        state.dir(),
        ISSUER_BOOK_FILE,
        format!("{text}{}\n", manual("by-hand", 7100).line()).as_bytes(),
    )
    .unwrap();
    assert!(
        read_issuer_book(state.dir(), &issuer).is_empty(),
        "a line that is no copy: the copy is none"
    );

    let own = [issuer_entry(0x11, 7100), manual("lab-mini", 7101)];
    let mut held = own.to_vec();
    held.push(copies[0].clone());
    write_book(state.dir(), &held);
    assert_eq!(read_peers(state.dir()).unwrap(), own);
    update_peers(state.dir(), |book| {
        book.push(manual("m9", 7100));
        Ok(())
    })
    .unwrap();
    let text = String::from_utf8(read_private(state.dir(), PEERS_FILE, 4096).unwrap()).unwrap();
    let vias: Vec<&str> = text
        .lines()
        .filter_map(|line| line.split(' ').find_map(|field| field.strip_prefix("via=")))
        .collect();
    assert_eq!(vias, ["issuer", "manual", "manual"], "{text}");
}

#[test]
fn a_books_members_are_asked_while_the_issuers_own_survey_waits() {
    let (asked, gate) = std::sync::mpsc::channel::<()>();
    let member_state = State::new("gate-member");
    let member = Issuing::hooked(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        secret(5),
        member_state.dir(),
        issuer(0x22),
        EVERY_OP,
        Arc::new(move |op| {
            if op == FleetOp::Survey {
                let _ = asked.send(());
            }
            None
        }),
    );
    let issuer_state = State::new("gate-issuer");
    write_book(
        issuer_state.dir(),
        &[joined(0x22, "127.0.0.1", [127, 0, 0, 1], member.port)],
    );
    issue(issuer_state.dir());
    let gate = std::sync::Mutex::new(gate);
    let issuing = Issuing::hooked(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        secret(5),
        issuer_state.dir(),
        issuer(0x11),
        EVERY_OP,
        Arc::new(move |op| {
            if op == FleetOp::Survey {
                let _ = gate.lock().unwrap().recv_timeout(HANG);
            }
            None
        }),
    );
    let peers = [issuer_entry(0x11, issuing.port)];
    let (reads, book) = read_whole(
        &peers,
        &Keyring::of(&peers),
        None,
        (&secret(5), None),
        HANG,
        "survey",
    );
    assert!(matches!(book, BookRead::Fetched(_)), "{book:?}");
    let read: Vec<(Via, Option<String>)> = reads
        .iter()
        .map(|read| match &read.read {
            Ok(surveyed) => (read.peer.via, surveyed.node.clone()),
            Err(e) => panic!("{:?} at {}: {e:?}", read.peer.via, read.peer.target()),
        })
        .collect();
    assert_eq!(
        read,
        [
            (Via::Issuer, Some(issuer(0x11).node)),
            (Via::IssuerBook, Some(issuer(0x22).node)),
        ]
    );
}

#[test]
fn a_machine_at_the_issuers_old_address_never_speaks_for_the_issuer() {
    for squatter_serves in [EVERY_OP, OLDER_OPS] {
        let (v6, v4, port) = twin_listeners();
        let member_state = State::new("squat-member");
        let member = Issuing::start(secret(5), member_state.dir(), issuer(0x22));
        let issuer_state = State::new("squat-issuer");
        write_book(
            issuer_state.dir(),
            &[joined(0x22, "127.0.0.1", [127, 0, 0, 1], member.port)],
        );
        issue(issuer_state.dir());
        let _issuer = Issuing::on(v6, secret(5), issuer_state.dir(), issuer(0x11), EVERY_OP);
        let squatter_state = State::new("squat-other");
        let _squatter = Issuing::on(
            v4,
            secret(5),
            squatter_state.dir(),
            issuer(0x33),
            squatter_serves,
        );
        let kept = copy(joined(0x22, "127.0.0.1", [127, 0, 0, 1], member.port));
        let peers = [
            Peer {
                host: "::1".to_owned(),
                ..issuer_entry(0x11, port)
            },
            kept.clone(),
        ];
        let (reads, book) = read_whole(
            &peers,
            &Keyring::of(&peers),
            None,
            (&secret(5), None),
            HANG,
            "survey",
        );
        assert!(
            matches!(&book, BookRead::Fetched(book) if book.node == issuer(0x11).node),
            "{squatter_serves:?}: {book:?}"
        );
        assert_eq!(reads.len(), 2, "{squatter_serves:?}: {reads:?}");
        assert!(
            matches!(&reads[0].read, Ok(s) if s.addr.is_ipv6() && s.node == Some(issuer(0x11).node)),
            "{squatter_serves:?}: {:?}",
            reads[0].read
        );
        assert_eq!(reads[1].peer, kept, "{squatter_serves:?}");
        assert!(
            matches!(&reads[1].read, Ok(s) if s.node == Some(issuer(0x22).node)),
            "{squatter_serves:?}: {:?}",
            reads[1].read
        );
    }
}

#[test]
fn an_issuer_that_cannot_share_its_book_now_leaves_the_copy() {
    let state = State::new("not-now");
    issue(state.dir());
    write_private(state.dir(), PEERS_FILE, b"peer garbage\n").unwrap();
    let issuing = Issuing::start(secret(5), state.dir(), issuer(0x11));
    let kept = copy(joined(0x55, "127.0.0.1", [127, 0, 0, 1], closed_port()));
    let peers = [issuer_entry(0x11, issuing.port), kept.clone()];
    let (reads, book) = read_whole(
        &peers,
        &Keyring::of(&peers),
        None,
        (&secret(5), None),
        HANG,
        "survey",
    );
    assert!(matches!(book, BookRead::Unshared(_)), "{book:?}");
    assert_eq!(reads.len(), 2, "{reads:?}");
    assert!(reads[0].read.is_ok(), "{:?}", reads[0].read);
    assert_eq!(reads[1].peer, kept);
}

#[test]
fn a_book_that_rebinds_a_member_replaces_its_copy_and_key() {
    let i = identity(&node_of(0x11), 3);
    let (old, new) = (identity(&node_of(0xd4), 1), identity(&node_of(0xd4), 2));
    let issuer = Peer {
        key: Some(i.key()),
        ..issuer_entry(0x11, 7100)
    };
    let member = |key: NodeKey| keyed(0xd4, key, [10, 0, 0, 9], 7100);
    let peers = vec![issuer, copy(member(old.key()))];
    let queue = || Queue::seated(peers.clone(), 0, Keyring::of(&peers));
    let book = |key: NodeKey| Book {
        node: i.node().to_owned(),
        peers: vec![member(key)],
        at: IpAddr::from([10, 0, 0, 1]),
        proven: true,
        seed: None,
    };

    let mut queued = queue();
    assert_eq!(queued.keys.bound(old.node()), Some(old.key()));
    assert_eq!(queue_book(&mut queued, 2, &book(new.key()), None), vec![1]);
    assert_eq!(
        queued.skip,
        vec![0, 1],
        "the copy under its old key is not read"
    );
    assert_eq!(
        queued.peers.get(2),
        Some(&copy(member(new.key()))),
        "queued under the new one"
    );
    assert_eq!(queued.keys.bound(old.node()), Some(new.key()));
    assert_eq!(
        queued.keys.bound(i.node()),
        Some(i.key()),
        "the book's own key stands"
    );

    let mut queued = queue();
    assert_eq!(
        queue_book(&mut queued, 2, &book(old.key()), None),
        Vec::<usize>::new()
    );
    assert_eq!(queued.peers, peers, "the same copy, read once");
    assert_eq!(queued.keys.bound(old.node()), Some(old.key()));
}

#[test]
fn a_member_rebound_on_the_issuer_is_read_under_its_new_key() {
    let (m_state, issuer_state) = (State::new("rekey-m"), State::new("rekey-i"));
    let (old, new) = (identity(&node_of(0xd4), 1), identity(&node_of(0xd4), 2));
    let m = Issuing::proving(
        secret(5),
        m_state.dir(),
        Issuer {
            node: node_of(0xd4),
            ..issuer(0x11)
        },
        new.clone(),
    );
    let issuing_as = identity(&issuer(0x11).node, 9);
    let read = |bound: NodeKey| {
        write_book(
            issuer_state.dir(),
            &[Peer {
                host: "127.0.0.1".to_owned(),
                ..keyed(0xd4, bound, [127, 0, 0, 1], m.port)
            }],
        );
        issue(issuer_state.dir());
        let issuing = Issuing::proving(
            secret(5),
            issuer_state.dir(),
            issuer(0x11),
            issuing_as.clone(),
        );
        let peers = [
            Peer {
                key: Some(issuing_as.key()),
                ..issuer_entry(0x11, issuing.port)
            },
            copy(Peer {
                host: "127.0.0.1".to_owned(),
                ..keyed(0xd4, old.key(), [127, 0, 0, 1], m.port)
            }),
        ];
        let (reads, book) = read_whole(
            &peers,
            &Keyring::of(&peers),
            None,
            (&secret(5), None),
            HANG,
            "survey",
        );
        assert!(
            matches!(&book, BookRead::Fetched(book) if book.proven),
            "{book:?}"
        );
        reads
            .into_iter()
            .filter(|read| read.peer.node.as_deref() == Some(new.node()))
            .map(|read| (read.peer.key, read.read.map(|surveyed| surveyed.proven)))
            .collect::<Vec<_>>()
    };

    let rebound = read(new.key());
    assert!(
        matches!(rebound.as_slice(), [(Some(key), Ok(true))] if *key == new.key()),
        "{rebound:?}"
    );
    let stale = read(old.key());
    assert!(
        matches!(stale.as_slice(), [(Some(key), Err(Unreached::Impostor { issuer: false, .. }))] if *key == old.key()),
        "{stale:?}"
    );
}

#[test]
fn a_node_the_book_binds_anew_is_proven_by_every_entry_that_reached_it() {
    let (m_state, issuer_state) = (State::new("anew-m"), State::new("anew-i"));
    let (proves, other) = (identity(&node_of(0xd5), 1), identity(&node_of(0xd5), 2));
    let m = Issuing::proving(
        secret(5),
        m_state.dir(),
        Issuer {
            node: node_of(0xd5),
            ..issuer(0x11)
        },
        proves.clone(),
    );
    let issuing_as = identity(&issuer(0x11).node, 9);
    let read = |bound: NodeKey| {
        write_book(
            issuer_state.dir(),
            &[
                manual("127.0.0.1", m.port),
                Peer {
                    host: "127.0.0.1".to_owned(),
                    ..keyed(0xd5, bound, [127, 0, 0, 1], m.port)
                },
            ],
        );
        issue(issuer_state.dir());
        let issuing = Issuing::proving(
            secret(5),
            issuer_state.dir(),
            issuer(0x11),
            issuing_as.clone(),
        );
        let peers = [
            Peer {
                key: Some(issuing_as.key()),
                ..issuer_entry(0x11, issuing.port)
            },
            copy(manual("127.0.0.1", m.port)),
        ];
        let (reads, book) = read_whole(
            &peers,
            &Keyring::of(&peers),
            None,
            (&secret(5), None),
            HANG,
            "survey",
        );
        assert!(
            matches!(&book, BookRead::Fetched(book) if book.proven),
            "{book:?}"
        );
        reads
            .into_iter()
            .filter(|read| read.peer.port == m.port)
            .map(|read| (read.peer.node.is_some(), read.read))
            .collect::<Vec<_>>()
    };

    let anew = read(proves.key());
    assert!(
        matches!(
            anew.as_slice(),
            [(false, Ok(by_hand)), (true, Ok(booked))] if by_hand.proven && booked.proven
        ),
        "{anew:?}"
    );
    let other_key = read(other.key());
    assert!(
        matches!(
            other_key.as_slice(),
            [(false, by_hand), (true, Err(Unreached::Impostor { issuer: false, .. }))]
                if !matches!(by_hand, Ok(surveyed) if surveyed.proven)
        ),
        "{other_key:?}"
    );
}

#[test]
fn an_answer_proven_by_a_key_the_book_no_longer_binds_is_not_that_node() {
    let (m_state, issuer_state) = (State::new("unbound-m"), State::new("unbound-i"));
    let (old, new) = (identity(&node_of(0xd7), 1), identity(&node_of(0xd7), 2));
    let m = Issuing::proving(
        secret(5),
        m_state.dir(),
        Issuer {
            node: node_of(0xd7),
            ..issuer(0x11)
        },
        old.clone(),
    );
    let issuing_as = identity(&issuer(0x11).node, 9);
    let read = |bound: Option<NodeKey>| {
        let booked: Vec<Peer> = bound
            .map(|key| Peer {
                host: "127.0.0.1".to_owned(),
                ..keyed(0xd7, key, [127, 0, 0, 1], m.port)
            })
            .into_iter()
            .collect();
        write_book(issuer_state.dir(), &booked);
        issue(issuer_state.dir());
        let issuing = Issuing::proving(
            secret(5),
            issuer_state.dir(),
            issuer(0x11),
            issuing_as.clone(),
        );
        let peers = [
            Peer {
                key: Some(issuing_as.key()),
                ..issuer_entry(0x11, issuing.port)
            },
            manual("127.0.0.1", m.port),
            copy(Peer {
                host: "127.0.0.1".to_owned(),
                ..keyed(0xd7, old.key(), [127, 0, 0, 1], m.port)
            }),
        ];
        let (reads, book) = read_whole(
            &peers,
            &Keyring::of(&peers),
            None,
            (&secret(5), None),
            HANG,
            "survey",
        );
        assert!(
            matches!(&book, BookRead::Fetched(book) if book.proven),
            "{book:?}"
        );
        reads
            .into_iter()
            .filter(|read| read.peer.port == m.port)
            .map(|read| (read.peer.via, read.read))
            .collect::<Vec<_>>()
    };

    let rekeyed = read(Some(new.key()));
    assert!(
        matches!(
            rekeyed.as_slice(),
            [
                (Via::Manual, Err(Unreached::Impostor { issuer: false, .. })),
                (
                    Via::IssuerBook,
                    Err(Unreached::Impostor { issuer: false, .. })
                ),
            ]
        ),
        "{rekeyed:?}"
    );
    let taken_out = read(None);
    assert!(
        matches!(
            taken_out.as_slice(),
            [(Via::Manual, Ok(by_hand))] if !by_hand.proven
                && by_hand.node.as_deref() == Some(old.node())
        ),
        "{taken_out:?}"
    );
    let kept = read(Some(old.key()));
    assert!(
        matches!(
            kept.as_slice(),
            [(Via::Manual, Ok(by_hand)), (Via::IssuerBook, Ok(booked))]
                if by_hand.proven && booked.proven
        ),
        "{kept:?}"
    );
}

/// A dial that reads another dial's exchange at an address follows how that one ended, while its
/// own deadline is still to come: passed over at a stalled handshake, it passes on without another
/// try at that address; ended by that dial's own deadline, it makes its own.
#[test]
fn a_dial_reading_a_stalled_address_passes_on_without_dialing_it_again() {
    for (passed, dials) in [(true, 0), (false, 1)] {
        let to = SocketAddr::from((Ipv4Addr::LOCALHOST, 7));
        let (held, dialed) = (HeldAddrs::default(), AtomicUsize::new(0));
        let why = Unreached::Deadline(Stage::Handshake, Duration::from_secs(3));
        let opened = held.enter(to).ok().expect("the first dial's own exchange");
        let exchange = Arc::clone(&opened.exchange);
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                held.ask(
                    to,
                    (Instant::now() + HANG, HANG),
                    || {
                        dialed.fetch_add(1, Ordering::SeqCst);
                        AtAddr::Passed(Unreached::Deadline(Stage::Connect, Duration::from_secs(1)))
                    },
                    |_| Ok(None),
                    |_| Ok(Vec::new()),
                )
            });
            // The reader holds the exchange: the list's, `opened`'s, this one's and its own.
            aterm_spec::wait::until_within(
                HANG,
                "the second dial reads the first's exchange",
                || (Arc::strong_count(&exchange) >= 4).then_some(()),
            );
            let first = |_: &mut TlsTransport<ClientConnection>| Ok(Vec::new());
            let reached = if passed {
                AtAddr::Passed(why)
            } else {
                AtAddr::Ended(why)
            };
            let _ = opened.ask(to, reached, |_| Ok(None), first);
            let read = reader.join().expect("the reader");
            assert!(
                matches!(read, AtAddr::Passed(_)),
                "passed {passed}: its address is passed over"
            );
        });
        assert_eq!(dialed.load(Ordering::SeqCst), dials, "passed {passed}");
    }
}

#[test]
fn a_read_holds_one_connection_to_an_address_at_a_time() {
    let (in_flight, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (gate, held) = std::sync::mpsc::channel::<()>();
    let held = std::sync::Mutex::new(held);
    let (m_state, issuer_state) = (State::new("one-at-m"), State::new("one-at-i"));
    let n = identity(&node_of(0xd8), 1);
    let m = Issuing::run(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        (secret(5), Some(n.clone())),
        m_state.dir(),
        Issuer {
            node: node_of(0xd8),
            ..issuer(0x11)
        },
        EVERY_OP,
        Arc::new({
            let (in_flight, most) = (Arc::clone(&in_flight), Arc::clone(&most));
            move |op| {
                if op == FleetOp::Survey {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    let _ = held.lock().unwrap().recv_timeout(HANG);
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                }
                None
            }
        }),
    );
    let joined_there = Peer {
        host: "127.0.0.1".to_owned(),
        ..keyed(0xd8, n.key(), [127, 0, 0, 1], m.port)
    };
    write_book(issuer_state.dir(), std::slice::from_ref(&joined_there));
    issue(issuer_state.dir());
    let issuing_as = identity(&issuer(0x11).node, 9);
    let issuing = Issuing::proving(
        secret(5),
        issuer_state.dir(),
        issuer(0x11),
        issuing_as.clone(),
    );
    let peers = [
        Peer {
            key: Some(issuing_as.key()),
            ..issuer_entry(0x11, issuing.port)
        },
        manual("127.0.0.1", m.port),
        copy(joined_there),
    ];
    let (reads, _) = read_whole(
        &peers,
        &Keyring::of(&peers),
        None,
        (&secret(5), None),
        Duration::from_secs(1),
        "survey",
    );
    drop(gate);
    let at_m: Vec<_> = reads
        .iter()
        .filter(|read| read.peer.port == m.port)
        .map(|read| read.read.as_ref().map(|surveyed| surveyed.addr))
        .collect();
    assert_eq!(at_m.len(), 2, "{at_m:?}");
    assert_eq!(most.load(Ordering::SeqCst), 1, "{at_m:?}");
}

#[test]
fn two_entries_of_one_mac_read_its_one_answer() {
    let b = identity(&node_of(0xb1), 1);
    let (arrived, came) = std::sync::mpsc::channel::<()>();
    let (gate, gated) = std::sync::mpsc::channel::<()>();
    let (arrived, gated) = (std::sync::Mutex::new(arrived), std::sync::Mutex::new(gated));
    let surveys = Arc::new(AtomicUsize::new(0));
    let state = State::new("one-answer");
    let mac = Issuing::run(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        (secret(5), Some(b.clone())),
        state.dir(),
        Issuer {
            node: node_of(0xb1),
            ..issuer(0x11)
        },
        EVERY_OP,
        Arc::new({
            let surveys = Arc::clone(&surveys);
            move |op| {
                if op == FleetOp::Survey {
                    surveys.fetch_add(1, Ordering::SeqCst);
                    let _ = arrived.lock().unwrap().send(());
                    let _ = gated.lock().unwrap().recv_timeout(HANG);
                }
                None
            }
        }),
    );
    let joined = Target {
        host: "127.0.0.1".to_owned(),
        addr: Some(IpAddr::from([127, 0, 0, 1])),
        port: mac.port,
        node: Some(node_of(0xb1)),
        key: Some(b.key()),
    };
    let by_hand = Target {
        node: None,
        key: None,
        ..joined.clone()
    };
    let (held, keys) = (Arc::new(HeldAddrs::default()), Keyring::default());
    let read = |target: &Target| read_one(target, &keys, (&secret(5), None), HANG, "survey", &held);
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| read(&joined));
        came.recv_timeout(HANG)
            .expect("the first entry's survey reached the Mac");
        let second = scope.spawn(|| read(&by_hand));
        // A third holder of the first's exchange: the second entry's dial reads it.
        let since = Instant::now();
        while !held
            .open
            .lock()
            .unwrap()
            .iter()
            .any(|(_, open)| Arc::strong_count(open) > 2)
        {
            assert!(
                since.elapsed() < HANG,
                "the second entry never came to the Mac"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(gate);
        (first.join().unwrap(), second.join().unwrap())
    });
    let (first, second) = (first.unwrap(), second.unwrap());
    assert_eq!(
        (first.node.as_deref(), first.proven),
        (Some(b.node()), true)
    );
    assert_eq!(
        (second.node.as_deref(), second.addr),
        (Some(b.node()), first.addr)
    );
    assert_eq!(second.lines, first.lines);
    assert_eq!(
        surveys.load(Ordering::SeqCst),
        1,
        "one survey answered both"
    );
}

#[test]
fn an_address_another_node_holds_gives_way_to_the_name_at_once() {
    let (x, m) = (node_of(0xa1), identity(&node_of(0xa2), 1));
    let (v6, v4, port) = twin_listeners();
    let _x = Naming::on(v6, secret(5), x.clone());
    let (arrived, came) = std::sync::mpsc::channel::<()>();
    let (gate, gated) = std::sync::mpsc::channel::<()>();
    let (arrived, gated) = (std::sync::Mutex::new(arrived), std::sync::Mutex::new(gated));
    let state = State::new("gives-way-m");
    let _m = Issuing::run(
        v4,
        (secret(5), Some(m.clone())),
        state.dir(),
        Issuer {
            node: m.node().to_owned(),
            ..issuer(0x11)
        },
        EVERY_OP,
        Arc::new(move |op| {
            if op == FleetOp::Survey {
                let _ = arrived.lock().unwrap().send(());
                let _ = gated.lock().unwrap().recv_timeout(HANG);
            }
            None
        }),
    );
    let at_m = Target {
        host: "127.0.0.1".to_owned(),
        addr: Some(IpAddr::from([127, 0, 0, 1])),
        port,
        node: Some(m.node().to_owned()),
        key: Some(m.key()),
    };
    let moved = Target {
        host: "::1".to_owned(),
        node: Some(x.clone()),
        key: None,
        ..at_m.clone()
    };
    let (held, keys) = (Arc::new(HeldAddrs::default()), Keyring::default());
    let read = |target: &Target| read_one(target, &keys, (&secret(5), None), HANG, "survey", &held);
    let found = std::thread::scope(|scope| {
        let reading_m = scope.spawn(|| read(&at_m));
        came.recv_timeout(HANG).expect("M's survey began");
        let found = read(&moved);
        drop(gate);
        let _ = reading_m.join().unwrap();
        found
    });
    match found {
        Ok(surveyed) => {
            assert_eq!(surveyed.node.as_deref(), Some(x.as_str()));
            assert!(surveyed.addr.is_ipv6(), "{:?}", surveyed.addr);
        }
        Err(e) => panic!("X's name was never asked: {e}"),
    }
}

#[test]
fn the_issuers_book_never_waits_on_another_entrys_survey_of_it() {
    let n = identity(&node_of(0xd9), 1);
    let (n_asked, n_came) = std::sync::mpsc::channel::<()>();
    let n_asked = std::sync::Mutex::new(n_asked);
    let n_state = State::new("book-first-n");
    let member = Issuing::run(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        (secret(5), Some(n.clone())),
        n_state.dir(),
        Issuer {
            node: node_of(0xd9),
            ..issuer(0x22)
        },
        EVERY_OP,
        Arc::new(move |op| {
            if op == FleetOp::Survey {
                let _ = n_asked.lock().unwrap().send(());
            }
            None
        }),
    );
    let issuer_state = State::new("book-first-i");
    write_book(
        issuer_state.dir(),
        &[Peer {
            host: "127.0.0.1".to_owned(),
            ..keyed(0xd9, n.key(), [127, 0, 0, 1], member.port)
        }],
    );
    issue(issuer_state.dir());
    let issuing_as = identity(&issuer(0x11).node, 9);
    let (gate, gated) = std::sync::mpsc::channel::<()>();
    let gated = std::sync::Mutex::new(gated);
    // The issuer's ops being answered now, and the most there were at once.
    let (answering, most) = (AtomicUsize::new(0), Arc::new(AtomicUsize::new(0)));
    let issuing = Issuing::run(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        (secret(5), Some(issuing_as.clone())),
        issuer_state.dir(),
        issuer(0x11),
        EVERY_OP,
        Arc::new({
            let most = Arc::clone(&most);
            move |op| {
                most.fetch_max(
                    answering.fetch_add(1, Ordering::SeqCst) + 1,
                    Ordering::SeqCst,
                );
                match op {
                    FleetOp::Survey => {
                        let _ = gated.lock().unwrap().recv_timeout(HANG);
                    }
                    // Long past the entry's dial, which begins at once.
                    FleetOp::Book => std::thread::sleep(Duration::from_millis(300)),
                    _ => {}
                }
                answering.fetch_sub(1, Ordering::SeqCst);
                None
            }
        }),
    );
    let peers = [
        Peer {
            key: Some(issuing_as.key()),
            ..issuer_entry(0x11, issuing.port)
        },
        manual("127.0.0.1", issuing.port),
    ];
    let (reads, book) = std::thread::scope(|scope| {
        scope.spawn(move || {
            let _ = n_came.recv_timeout(HANG);
            drop(gate);
        });
        read_whole(
            &peers,
            &Keyring::of(&peers),
            None,
            (&secret(5), None),
            HANG,
            "survey",
        )
    });
    assert!(
        matches!(&book, BookRead::Fetched(book) if book.proven),
        "{book:?}"
    );
    let read: Vec<(Via, Option<String>)> = reads
        .iter()
        .map(|read| match &read.read {
            Ok(surveyed) => (read.peer.via, surveyed.node.clone()),
            Err(e) => panic!("{:?} at {}: {e:?}", read.peer.via, read.peer.target()),
        })
        .collect();
    assert_eq!(
        read,
        [
            (Via::Issuer, Some(issuer(0x11).node)),
            (Via::Manual, Some(issuer(0x11).node)),
            (Via::IssuerBook, Some(node_of(0xd9))),
        ]
    );
    assert_eq!(
        most.load(Ordering::SeqCst),
        1,
        "the issuer was asked twice at once"
    );
}

#[test]
fn the_issuers_book_waits_out_an_exchange_open_where_its_name_answers() {
    const WITHIN: Duration = Duration::from_secs(1);
    let (asked, came) = std::sync::mpsc::channel::<()>();
    let asked = std::sync::Mutex::new(asked);
    let state = State::new("book-waits");
    issue(state.dir());
    let issuing = Issuing::hooked(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        secret(5),
        state.dir(),
        issuer(0x11),
        EVERY_OP,
        Arc::new(move |op| {
            if op == FleetOp::Book {
                let _ = asked.lock().unwrap().send(());
            }
            None
        }),
    );
    let by_name = Target {
        host: "127.0.0.1".to_owned(),
        addr: None,
        port: issuing.port,
        node: Some(issuer(0x11).node),
        key: None,
    };
    let held = HeldAddrs::default();
    let Ok(theirs) = held.enter(SocketAddr::from((Ipv4Addr::LOCALHOST, issuing.port))) else {
        panic!("an exchange open there already");
    };
    let read = |within: Duration| {
        fetch_book_in(
            &by_name,
            (&secret(5), None),
            (Instant::now() + within, within),
            (Some(&held), None),
        )
    };
    let waited = read(WITHIN);
    assert!(
        matches!(&waited, Err(BookUnread::Unreached(Unreached::Deadline(..)))),
        "{waited:?}"
    );
    assert!(
        came.try_recv().is_err(),
        "the book was asked beside the open exchange"
    );
    let book = std::thread::scope(|scope| {
        let reading = scope.spawn(|| read(HANG));
        aterm_spec::wait::within(HANG, Duration::from_millis(5), || {
            (Arc::strong_count(&theirs.exchange) > 2).then_some(())
        })
        .expect("the book waits on the open exchange");
        drop(theirs);
        reading.join().unwrap()
    });
    assert!(
        matches!(&book, Ok(book) if book.node == issuer(0x11).node),
        "{book:?}"
    );
    came.recv_timeout(HANG)
        .expect("the book was asked once the exchange closed");
}

#[test]
fn an_entry_refused_by_the_copys_key_is_read_again_by_the_books() {
    let (old, new) = (identity(&node_of(0xda), 1), identity(&node_of(0xda), 2));
    let (dialled, came) = std::sync::mpsc::channel::<()>();
    let (dialled, came) = (std::sync::Mutex::new(dialled), std::sync::Mutex::new(came));
    let (m_state, issuer_state) = (State::new("again-m"), State::new("again-i"));
    let m = Issuing::run(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        (secret(5), Some(new.clone())),
        m_state.dir(),
        Issuer {
            node: node_of(0xda),
            ..issuer(0x11)
        },
        EVERY_OP,
        Arc::new(move |op| {
            if op == FleetOp::Survey {
                let _ = dialled.lock().unwrap().send(());
            }
            None
        }),
    );
    write_book(
        issuer_state.dir(),
        &[Peer {
            host: "127.0.0.1".to_owned(),
            ..keyed(0xda, new.key(), [127, 0, 0, 1], m.port)
        }],
    );
    issue(issuer_state.dir());
    let issuing_as = identity(&issuer(0x11).node, 9);
    let issuing = Issuing::run(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        (secret(5), Some(issuing_as.clone())),
        issuer_state.dir(),
        issuer(0x11),
        EVERY_OP,
        // The book once N's place was dialled under the copy's key.
        Arc::new(move |op| {
            if op == FleetOp::Book {
                let _ = came.lock().unwrap().recv_timeout(HANG);
            }
            None
        }),
    );
    let peers = [
        Peer {
            key: Some(issuing_as.key()),
            ..issuer_entry(0x11, issuing.port)
        },
        manual("127.0.0.1", m.port),
        copy(Peer {
            host: "127.0.0.1".to_owned(),
            ..keyed(0xda, old.key(), [127, 0, 0, 1], m.port)
        }),
    ];
    let (reads, book) = read_whole(
        &peers,
        &Keyring::of(&peers),
        None,
        (&secret(5), None),
        HANG,
        "survey",
    );
    assert!(
        matches!(&book, BookRead::Fetched(book) if book.proven),
        "{book:?}"
    );
    let at_n: Vec<_> = reads
        .into_iter()
        .filter(|read| read.peer.port == m.port)
        .map(|read| (read.peer.via, read.read))
        .collect();
    assert!(
        matches!(
            at_n.as_slice(),
            [(Via::Manual, Ok(by_hand)), (Via::IssuerBook, Ok(booked))]
                if by_hand.proven && booked.proven
                    && by_hand.node.as_deref() == Some(new.node())
        ),
        "{at_n:?}"
    );
}

#[test]
fn a_peer_past_the_first_at_once_dials_waits_for_a_place_of_its_own() {
    let peers: Vec<Peer> = (0..AT_ONCE + 5)
        .map(|n| joined(0, "127.0.0.1", [127, 0, 0, 1], 7000 + n as u16))
        .collect();
    let mut queued = Queue::seated(peers, 3, Keyring::default());
    assert_eq!(queued.hands(), AT_ONCE - 1, "the seat holds one place");
    assert_eq!(queued.hands(), 0, "every place is held");
    let deadline = (Instant::now() + HANG, Duration::from_secs(1));
    let mut waited = Vec::new();
    while let Some((at, _, by, _)) = queued.take(deadline) {
        assert_ne!(at, 3, "a skipped place is never taken");
        waited.push(by.0 != deadline.0);
    }
    assert_eq!(waited.len(), AT_ONCE + 4);
    let first = waited.iter().position(|past| *past);
    assert_eq!(first, Some(AT_ONCE - 1), "{waited:?}");
    assert!(waited[AT_ONCE - 1..].iter().all(|past| *past));
}

#[test]
fn a_dark_issuer_costs_its_member_one_deadline() {
    const WITHIN: Duration = Duration::from_secs(1);
    let dark = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let other_state = State::new("dark-other");
    let other = Issuing::start(secret(5), other_state.dir(), issuer(0x22));
    let kept = copy(joined(0x22, "127.0.0.1", [127, 0, 0, 1], other.port));
    let peers = vec![
        issuer_entry(0x11, dark.local_addr().unwrap().port()),
        kept.clone(),
    ];
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_whole(
            &peers,
            &Keyring::of(&peers),
            None,
            (&secret(5), None),
            WITHIN,
            "survey",
        ));
    });
    let (reads, book) = rx
        .recv_timeout(HANG)
        .unwrap_or_else(|_| panic!("read_whole was still reading after {HANG:?}"));
    assert!(matches!(book, BookRead::Unread(_)), "{book:?}");
    assert_eq!(reads.len(), 2, "{reads:?}");
    assert!(
        matches!(
            reads[0].read,
            Err(Unreached::Deadline(Stage::Handshake, WITHIN))
        ),
        "the book's dial is the issuer's reason: {:?}",
        reads[0].read
    );
    assert_eq!(
        reads[1].peer, kept,
        "a book that did not come leaves the copy, and it is asked"
    );
    dark.set_nonblocking(true).unwrap();
    let mut dials = 0;
    while dark.accept().is_ok() {
        dials += 1;
    }
    assert_eq!(dials, 1, "the issuer was dialled once");
}

struct Closing {
    taken: Arc<std::sync::atomic::AtomicUsize>,
    running: Arc<AtomicBool>,
    taking: Option<std::thread::JoinHandle<()>>,
}

impl Closing {
    fn on(listener: TcpListener) -> Self {
        listener.set_nonblocking(true).unwrap();
        let taken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let running = Arc::new(AtomicBool::new(true));
        let taking = std::thread::spawn({
            let (taken, running) = (Arc::clone(&taken), Arc::clone(&running));
            move || {
                while running.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            taken.fetch_add(1, Ordering::SeqCst);
                            drop(stream);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            }
        });
        Self {
            taken,
            running,
            taking: Some(taking),
        }
    }

    fn taken(&self) -> usize {
        self.taken.load(Ordering::SeqCst)
    }
}

impl Drop for Closing {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(taking) = self.taking.take() {
            let _ = taking.join();
        }
    }
}

#[test]
fn a_moved_issuer_is_surveyed_where_its_book_answered() {
    let (v6, v4, port) = twin_listeners();
    let state = State::new("moved");
    issue(state.dir());
    let _issuer = Issuing::on(v6, secret(5), state.dir(), issuer(0x11), EVERY_OP);
    let recorded = Closing::on(v4);
    let peers = [Peer {
        host: "::1".to_owned(),
        ..issuer_entry(0x11, port)
    }];
    let (reads, book) = read_whole(
        &peers,
        &Keyring::of(&peers),
        None,
        (&secret(5), None),
        HANG,
        "survey",
    );
    assert!(
        matches!(&book, BookRead::Fetched(book) if book.at.is_ipv6()),
        "{book:?}"
    );
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert!(
        matches!(&reads[0].read, Ok(s) if s.addr.is_ipv6() && s.node == Some(issuer(0x11).node)),
        "{:?}",
        reads[0].read
    );
    assert_eq!(
        recorded.taken(),
        1,
        "the recorded address is dialled once, for the book"
    );
}

#[test]
fn a_moved_issuer_whose_book_fails_is_surveyed_where_it_proved_itself() {
    let (v6, v4, port) = twin_listeners();
    let state = State::new("moved-no-book");
    issue(state.dir());
    let _issuer = Issuing::hooked(
        v6,
        secret(5),
        state.dir(),
        issuer(0x11),
        EVERY_OP,
        Arc::new(|op| (op == FleetOp::Book).then_some("OK 99999\n")),
    );
    let recorded = Closing::on(v4);
    let peers = [Peer {
        host: "::1".to_owned(),
        ..issuer_entry(0x11, port)
    }];
    let (reads, book) = read_whole(
        &peers,
        &Keyring::of(&peers),
        None,
        (&secret(5), None),
        HANG,
        "survey",
    );
    assert!(matches!(book, BookRead::Unshared(_)), "{book:?}");
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert!(
        matches!(&reads[0].read, Ok(s) if s.addr.is_ipv6() && s.node == Some(issuer(0x11).node)),
        "{:?}",
        reads[0].read
    );
    assert_eq!(
        recorded.taken(),
        1,
        "the recorded address is dialled once, for the book"
    );
}

#[test]
fn a_failed_book_ends_the_issuers_seat_or_names_where_to_survey_it() {
    let at: SocketAddr = (Ipv4Addr::new(10, 0, 0, 7), 7300).into();
    let deadline = Instant::now() + HANG;
    let late = deadline + Duration::from_millis(1);
    let early = deadline - Duration::from_secs(1);
    let cut = Unreached::Deadline(Stage::Book, HANG);
    let reply = || Unreached::Reply(at, "promised 99999 entries".to_owned());
    let busy = || Unreached::Handshake(at, FleetError::AddressBusy);
    let refused = || Unreached::Connect(at, io::ErrorKind::ConnectionRefused.into());
    assert!(
        ends_the_seat(&cut, deadline, deadline),
        "cut at the deadline"
    );
    assert!(ends_the_seat(&cut, deadline, late), "cut past it");
    assert!(ends_the_seat(&reply(), deadline, late), "no time left");
    assert!(ends_the_seat(&refused(), deadline, early), "never proved");
    for proved in [reply(), busy()] {
        assert!(!ends_the_seat(&proved, deadline, early), "{proved:?}");
    }
    let unread = |why| -> Result<Book, BookUnread> { Err(BookUnread::Unreached(why)) };
    assert_eq!(answered_at(&unread(reply())), Some(at.ip()));
    assert_eq!(answered_at(&unread(busy())), Some(at.ip()));
    assert_eq!(answered_at(&unread(refused())), None);
    assert_eq!(answered_at(&unread(cut)), None);
}

#[test]
fn a_connect_deadline_keeps_a_tried_address_failure() {
    let first: SocketAddr = ([10, 0, 0, 7], 7300).into();
    let next: SocketAddr = ([10, 0, 0, 8], 7300).into();
    let by_name: Resolver = |_, port| {
        vec![
            SocketAddr::from(([10, 0, 0, 8], port)),
            SocketAddr::from(([10, 0, 0, 9], port)),
        ]
    };
    for busy in [false, true] {
        let mut tried = Vec::new();
        let walked = walk(
            ("m7.local", Some(first.ip()), first.port()),
            (Instant::now() + HANG, HANG),
            (&[], by_name),
            |to| -> AtAddr<()> {
                tried.push(to);
                if to == first {
                    AtAddr::Passed(if busy {
                        Unreached::Handshake(to, FleetError::AddressBusy)
                    } else {
                        Unreached::Connect(to, io::ErrorKind::ConnectionRefused.into())
                    })
                } else {
                    assert_eq!(to, next, "the expired dial must end before another address");
                    AtAddr::Ended(Unreached::Deadline(Stage::Connect, HANG))
                }
            },
        );
        assert_eq!(tried, vec![first, next]);
        let why = walked.unwrap_err();
        assert_eq!(why.at_capacity(), busy);
        if busy {
            assert!(
                matches!(&why, Unreached::Handshake(at, FleetError::AddressBusy) if *at == first),
                "{why:?}"
            );
        } else {
            assert!(
                matches!(&why, Unreached::Connect(at, error)
                    if *at == first && error.kind() == io::ErrorKind::ConnectionRefused),
                "{why:?}"
            );
        }
    }
}

#[test]
fn a_terminal_handshake_result_keeps_its_own_evidence() {
    let first: SocketAddr = ([10, 0, 0, 7], 7300).into();
    let next: SocketAddr = ([10, 0, 0, 8], 7300).into();
    let by_name: Resolver = |_, port| vec![SocketAddr::from(([10, 0, 0, 8], port))];
    for handshake_deadline in [false, true] {
        let walked = walk(
            ("m7.local", Some(first.ip()), first.port()),
            (Instant::now() + HANG, HANG),
            (&[], by_name),
            |to| -> AtAddr<()> {
                if to == first {
                    AtAddr::Passed(Unreached::Connect(
                        to,
                        io::ErrorKind::ConnectionRefused.into(),
                    ))
                } else {
                    assert_eq!(to, next);
                    AtAddr::Ended(if handshake_deadline {
                        Unreached::Deadline(Stage::Handshake, HANG)
                    } else {
                        Unreached::Handshake(
                            to,
                            FleetError::UnknownOp(FleetOp::Drive.word().into()),
                        )
                    })
                }
            },
        );
        let why = walked.unwrap_err();
        if handshake_deadline {
            assert!(matches!(why, Unreached::Deadline(Stage::Handshake, _)));
        } else {
            assert!(
                matches!(why, Unreached::Handshake(at, FleetError::UnknownOp(op))
                if at == next && op == FleetOp::Drive.word())
            );
        }
    }
}

#[test]
fn a_dial_refused_as_busy_past_a_dead_address_names_the_busy_one() {
    let (dead, busy): (SocketAddr, SocketAddr) =
        (([10, 0, 0, 7], 7300).into(), ([10, 0, 0, 8], 7300).into());
    let deadline = Instant::now() + HANG;
    let by_name: Resolver = |_, port| vec![SocketAddr::from(([10, 0, 0, 8], port))];
    let walked = walk(
        ("m7.local", Some(dead.ip()), dead.port()),
        (deadline, HANG),
        (&[], by_name),
        |to| -> AtAddr<()> {
            AtAddr::Passed(if to == dead {
                Unreached::Connect(to, io::ErrorKind::ConnectionRefused.into())
            } else {
                Unreached::Handshake(to, FleetError::AddressBusy)
            })
        },
    );
    let Err(why) = walked else {
        panic!("no address answered");
    };
    assert!(
        matches!(&why, Unreached::Handshake(at, FleetError::AddressBusy) if *at == busy),
        "{why:?}"
    );
    assert!(!ends_the_seat(&why, deadline, Instant::now()));
}

static GUARD_ASKED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

#[test]
fn the_loopback_guard_skips_what_leaves_the_loopback_before_its_connect_or_lookup() {
    let asked: Resolver = |name, _| {
        GUARD_ASKED.lock().unwrap().push(name.to_owned());
        Vec::new()
    };
    // The stub `attempt` is the only dial: no address here reaches a socket.
    let walk_as = |on: bool, host: &str, addr: Option<IpAddr>, answers: bool| {
        GUARD_ASKED.lock().unwrap().clear();
        let mut dialled = Vec::new();
        let walked = crate::loopback::forced(on, || {
            walk(
                (host, addr, 7101),
                (Instant::now() + HANG, HANG),
                (&[], asked),
                |to| {
                    dialled.push(to);
                    if answers {
                        AtAddr::Reached(())
                    } else {
                        AtAddr::Passed(Unreached::Connect(
                            to,
                            io::ErrorKind::ConnectionRefused.into(),
                        ))
                    }
                },
            )
        });
        let mut names = std::mem::take(&mut *GUARD_ASKED.lock().unwrap());
        names.sort();
        (walked, dialled, names)
    };
    let guard = |walked: &Result<(), Unreached>, place: &str| {
        matches!(walked, Err(Unreached::Local(said))
            if said.starts_with("ATERM_DEBUG_LOOPBACK_ONLY=1 refuses ") && said.contains(place))
    };
    let documentation = IpAddr::from([192, 0, 2, 1]);
    for on in [true, false] {
        let (walked, dialled, names) = walk_as(on, "192.0.2.1", Some(documentation), true);
        assert!(
            matches!(walked, Err(Unreached::NotFleet(_))),
            "past the fleet's kind, guard {on}: {walked:?}"
        );
        assert!(
            dialled.is_empty() && names.is_empty(),
            "{dialled:?} {names:?}"
        );
    }
    let private = IpAddr::from([10, 0, 0, 1]);
    let (walked, dialled, names) = walk_as(true, "10.0.0.1", Some(private), true);
    assert!(guard(&walked, "a dial of 10.0.0.1:7101"), "{walked:?}");
    assert!(
        dialled.is_empty() && names.is_empty(),
        "{dialled:?} {names:?}"
    );
    let (walked, dialled, _) = walk_as(false, "10.0.0.1", Some(private), true);
    assert!(walked.is_ok(), "the control: {walked:?}");
    assert_eq!(dialled, [SocketAddr::new(private, 7101)]);
    for (host, unguarded) in [
        ("guard-peer.invalid", vec!["guard-peer.invalid"]),
        ("guard-mac", vec!["guard-mac", "guard-mac.local"]),
    ] {
        let (walked, dialled, names) = walk_as(true, host, None, true);
        assert!(
            guard(&walked, &format!("a lookup of {host:?}")),
            "{walked:?}"
        );
        assert!(dialled.is_empty() && names.is_empty(), "{host}: {names:?}");
        let (walked, _, names) = walk_as(false, host, None, true);
        assert!(matches!(walked, Err(Unreached::Resolve(_))), "{walked:?}");
        assert_eq!(names, unguarded, "the control: {host}");
    }
    let loopback = IpAddr::from([127, 0, 0, 1]);
    let (walked, dialled, names) = walk_as(true, "guard-mac", Some(loopback), false);
    assert!(
        matches!(walked, Err(Unreached::Connect(at, _)) if at == SocketAddr::new(loopback, 7101)),
        "the loopback's answer stands over the lookup the guard skipped: {walked:?}"
    );
    assert!(
        dialled.len() == 1 && names.is_empty(),
        "{dialled:?} {names:?}"
    );
    let (_, _, names) = walk_as(false, "guard-mac", Some(loopback), false);
    assert_eq!(names, ["guard-mac", "guard-mac.local"], "the control");
    let (walked, _, names) = walk_as(true, "localhost", None, true);
    assert!(matches!(walked, Err(Unreached::Resolve(_))), "{walked:?}");
    assert_eq!(
        names,
        ["localhost"],
        "no multicast-DNS twin under the guard"
    );
    let (_, _, names) = walk_as(false, "localhost", None, true);
    assert_eq!(names, ["localhost", "localhost.local"], "the control");
    let (walked, dialled, _) = walk_as(true, "127.0.0.1", Some(loopback), true);
    assert!(walked.is_ok(), "{walked:?}");
    assert_eq!(dialled, [SocketAddr::new(loopback, 7101)]);
}

#[test]
fn a_book_the_deadline_cut_short_is_the_issuers_row() {
    const WITHIN: Duration = Duration::from_secs(1);
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = std::sync::Mutex::new(held);
    let state = State::new("book-late");
    issue(state.dir());
    let issuing = Issuing::hooked(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        secret(5),
        state.dir(),
        issuer(0x11),
        EVERY_OP,
        Arc::new(move |op| {
            if op == FleetOp::Book {
                let _ = held.lock().unwrap().recv_timeout(HANG);
            }
            None
        }),
    );
    let kept = copy(joined(0x55, "127.0.0.1", [127, 0, 0, 1], closed_port()));
    let peers = vec![issuer_entry(0x11, issuing.port), kept.clone()];
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_whole(
            &peers,
            &Keyring::of(&peers),
            None,
            (&secret(5), None),
            WITHIN,
            "survey",
        ));
    });
    let (reads, book) = rx
        .recv_timeout(HANG)
        .unwrap_or_else(|_| panic!("read_whole was still reading after {HANG:?}"));
    drop(release);
    assert!(matches!(book, BookRead::Unread(_)), "{book:?}");
    assert_eq!(reads.len(), 2, "{reads:?}");
    assert!(
        matches!(
            reads[0].read,
            Err(Unreached::Deadline(Stage::Book | Stage::Handshake, WITHIN))
        ),
        "the book's reason is the issuer's row: {:?}",
        reads[0].read
    );
    assert_eq!(
        reads[1].peer, kept,
        "a book that did not come leaves the copy"
    );
    drop(issuing);
}

#[test]
fn a_member_never_reads_itself_through_its_issuers_book() {
    let own = node_of(0xaa);
    let me = identity(&own, 1);
    let read = |proving: Option<NodeIdentity>, own: Option<&str>, me: Option<&NodeIdentity>| {
        let place_state = State::new("self-member");
        let place = match proving {
            Some(key) => Issuing::proving(secret(5), place_state.dir(), issuer(0xaa), key),
            None => Issuing::start(secret(5), place_state.dir(), issuer(0xaa)),
        };
        let other_state = State::new("self-other");
        let other = Issuing::start(secret(5), other_state.dir(), issuer(0x22));
        let issuer_state = State::new("self-issuer");
        write_book(
            issuer_state.dir(),
            &[
                manual("127.0.0.1", place.port),
                joined(0x22, "127.0.0.1", [127, 0, 0, 1], other.port),
            ],
        );
        issue(issuer_state.dir());
        let issuing = Issuing::start(secret(5), issuer_state.dir(), issuer(0x11));
        let peers = [issuer_entry(0x11, issuing.port)];
        let (reads, book) = read_whole(
            &peers,
            &Keyring::of(&peers),
            own,
            (&secret(5), me),
            HANG,
            "survey",
        );
        assert!(matches!(book, BookRead::Fetched(_)), "{book:?}");
        reads
            .into_iter()
            .map(|read| match read.read {
                Ok(surveyed) => (read.peer.via, surveyed.node, surveyed.proven),
                Err(e) => panic!("{} {e}", read.peer.target()),
            })
            .collect::<Vec<_>>()
    };
    let the_issuer = (Via::Issuer, Some(issuer(0x11).node), false);
    let the_other = (Via::IssuerBook, Some(issuer(0x22).node), false);
    assert_eq!(
        read(Some(me.clone()), Some(&own), Some(&me)),
        [the_issuer.clone(), the_other.clone()],
        "this machine is no peer of its own read"
    );
    let claim = (Via::IssuerBook, Some(own.clone()), false);
    for (proving, me, why) in [
        (
            Some(identity(&own, 2)),
            Some(&me),
            "this machine's node id under another key",
        ),
        (None, Some(&me), "this machine's node id with no key"),
        (
            Some(me.clone()),
            None,
            "a read that proves no node key of its own",
        ),
    ] {
        assert_eq!(
            read(proving, Some(&own), me),
            [the_issuer.clone(), claim.clone(), the_other.clone()],
            "{why} is read as its claim, never left out as this machine"
        );
    }
    assert_eq!(
        read(None, None, None).len(),
        3,
        "a machine that does not know its node reads the place as a peer"
    );
}

/// What an [`Answering`] listener does with the request a proved peer sends.
#[derive(Clone)]
enum Answer {
    /// The Lines reply `OK <n>` and these lines.
    Lines(Vec<String>),
    /// Nothing: the stream is held, unanswered, until the peer goes — a
    /// stopped aterm's control server.
    Never,
    /// More than a reply may be: `OK <n>` and line after line.
    Flood,
    /// One TLS record of a reply sent a byte at a time, slower than a
    /// per-read timeout notices: only a wall-clock bound ends the read.
    Dribble,
    /// A one-line reply, held until `want` requests have come to the
    /// listeners its clones serve (`arrived` counts them) or `released` is
    /// set: only a reader that asks them all at once gets an answer from any.
    Together {
        arrived: Arc<AtomicUsize>,
        want: usize,
        released: Arc<AtomicBool>,
    },
}

impl Answer {
    fn together(want: usize) -> Self {
        Self::Together {
            arrived: Arc::new(AtomicUsize::new(0)),
            want,
            released: Arc::new(AtomicBool::new(false)),
        }
    }

    fn release(&self) {
        if let Self::Together { released, .. } = self {
            released.store(true, Ordering::Release);
        }
    }
}

const DRIBBLE_BYTES: usize = 4_000;

struct Answering {
    port: u16,
    running: Arc<AtomicBool>,
}

impl Answering {
    fn start(secret: FleetSecret, scope: FleetScope, answer: Answer) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let running = Arc::new(AtomicBool::new(true));
        let listening = Listening {
            secret,
            scope,
            node: None,
            serves: &[FleetOp::Survey],
            slots: AT_ONCE,
            per_source: AT_ONCE,
            lifetime: HANG,
        };
        std::thread::spawn({
            let running = Arc::clone(&running);
            move || {
                crate::fleet::serve_with_handshake(
                    &listener,
                    &listening,
                    &running,
                    HANG,
                    move |served, _| {
                        served.answer(|stream| {
                            stream.set_read_timeout(Some(HANG))?;
                            let mut line = Vec::new();
                            let mut byte = [0u8; 1];
                            while stream.read(&mut byte)? == 1 && byte[0] != b'\n' {
                                line.push(byte[0]);
                            }
                            assert_eq!(line, b"survey", "the request asks no level");
                            match &answer {
                                Answer::Lines(lines) => {
                                    let mut reply = format!("OK {}\n", lines.len());
                                    for line in lines {
                                        reply.push_str(line);
                                        reply.push('\n');
                                    }
                                    stream.write_all(reply.as_bytes())?;
                                    stream.flush()
                                }
                                Answer::Never => {
                                    let _ = stream.read(&mut byte);
                                    Ok(())
                                }
                                Answer::Flood => {
                                    stream.write_all(b"OK 100000\n")?;
                                    let line = format!("session {}\n", "x".repeat(1000));
                                    loop {
                                        stream.write_all(line.as_bytes())?;
                                    }
                                }
                                Answer::Dribble => {
                                    let mut sock = stream.sock();
                                    sock.write_all(&[0x17, 0x03, 0x03, 0x40, 0x00])?;
                                    for _ in 0..DRIBBLE_BYTES {
                                        sock.write_all(&[0x00]).and_then(|()| sock.flush())?;
                                        std::thread::sleep(Duration::from_millis(80));
                                    }
                                    Ok(())
                                }
                                Answer::Together {
                                    arrived,
                                    want,
                                    released,
                                } => {
                                    arrived.fetch_add(1, Ordering::SeqCst);
                                    let _ = aterm_spec::wait::within(
                                        HANG,
                                        Duration::from_millis(5),
                                        || {
                                            (arrived.load(Ordering::SeqCst) >= *want
                                                || released.load(Ordering::Acquire))
                                            .then_some(())
                                        },
                                    );
                                    stream.write_all(
                                        b"OK 1\ninstance pid=1 host=m version=0.104.0 \
                                          sessions=0 level=facts at_ms=1\n",
                                    )?;
                                    stream.flush()
                                }
                            }
                        })
                    },
                    |_: FleetEvent| {},
                );
            }
        });
        Self { port, running }
    }
}

impl Drop for Answering {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
    }
}

fn on_loopback(port: u16) -> Target {
    Target {
        host: "127.0.0.1".to_owned(),
        addr: None,
        node: None,
        key: None,
        port,
    }
}

fn surveyed(targets: Vec<Target>, within: Duration) -> Vec<Result<Surveyed, Unreached>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(survey_all(
            &targets,
            &Keyring::default(),
            (&secret(5), None),
            within,
        ));
    });
    rx.recv_timeout(HANG)
        .unwrap_or_else(|_| panic!("survey_all was still reading after {HANG:?}"))
}

#[test]
fn survey_all_answers_each_peer_within_one_deadline() {
    const WITHIN: Duration = Duration::from_secs(1);
    let facts_reply = vec![
        "instance pid=1 host=m1 version=0.104.0 sessions=1 level=facts at_ms=1".to_owned(),
        "session local=1 sid=s-1 program=zsh".to_owned(),
    ];
    let content_reply =
        vec!["instance pid=2 host=m2 version=0.104.0 sessions=0 level=content at_ms=1".to_owned()];
    let facts = Answering::start(
        secret(5),
        FleetScope::Facts,
        Answer::Lines(facts_reply.clone()),
    );
    let content = Answering::start(
        secret(5),
        FleetScope::Content,
        Answer::Lines(content_reply.clone()),
    );
    let flood = Answering::start(secret(5), FleetScope::Facts, Answer::Flood);
    let stranger = Answering::start(secret(6), FleetScope::Facts, Answer::Never);
    let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);
    let answering = vec![
        on_loopback(facts.port),
        on_loopback(content.port),
        on_loopback(closed_port),
        on_loopback(flood.port),
        on_loopback(stranger.port),
    ];
    let peers = answering.len();
    let read = surveyed(answering, HANG);
    assert_eq!(read.len(), peers, "one result per peer");
    let ok = |at: usize| match &read[at] {
        Ok(surveyed) => (surveyed.scope, surveyed.lines.clone()),
        Err(e) => panic!("peer {at}: {e}"),
    };
    assert_eq!(ok(0), (FleetScope::Facts, facts_reply));
    assert_eq!(ok(1), (FleetScope::Content, content_reply));
    assert!(
        matches!(&read[2], Err(Unreached::Connect(_, e)) if e.kind() == io::ErrorKind::ConnectionRefused),
        "{:?}",
        read[2]
    );
    assert!(
        matches!(&read[3], Err(Unreached::Reply(_, why)) if why.contains("256 KiB")),
        "{:?}",
        read[3]
    );
    assert!(
        matches!(
            read[4],
            Err(Unreached::Handshake(_, FleetError::ListenerWrongSecret))
        ),
        "{:?}",
        read[4]
    );

    let stopped = Answering::start(secret(5), FleetScope::Facts, Answer::Never);
    let dribble = Answering::start(secret(5), FleetScope::Facts, Answer::Dribble);
    let hang = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let read = surveyed(
        vec![
            on_loopback(hang.local_addr().unwrap().port()),
            on_loopback(stopped.port),
            on_loopback(dribble.port),
        ],
        WITHIN,
    );
    assert_eq!(read.len(), 3, "one result per peer");
    assert!(
        matches!(read[0], Err(Unreached::Deadline(Stage::Handshake, WITHIN))),
        "{:?}",
        read[0]
    );
    for at in [1, 2] {
        assert!(
            matches!(
                read[at],
                Err(Unreached::Deadline(
                    Stage::Handshake | Stage::Answer,
                    WITHIN
                ))
            ),
            "{:?}",
            read[at]
        );
    }
    for port in [stopped.port, dribble.port] {
        let mut proved = dial(
            "127.0.0.1",
            None,
            port,
            (&secret(5), None),
            FleetOp::Survey,
            HANG,
        )
        .unwrap_or_else(|e| panic!("the peer at :{port} proves itself: {e}"));
        let asked = ask_lines(
            &mut proved.transport,
            Some("survey"),
            Instant::now() + Duration::from_millis(300),
            reply_head,
        );
        assert!(
            matches!(asked, Err(Asked::Deadline)),
            "the answer's wait at :{port} is the deadline's"
        );
    }
    drop(hang);
}

#[test]
fn thirty_peers_are_asked_at_once() {
    const PEERS: usize = 30;
    let answer = Answer::together(PEERS);
    let together: Vec<Answering> = (0..PEERS)
        .map(|_| Answering::start(secret(5), FleetScope::Facts, answer.clone()))
        .collect();
    let read = surveyed(
        together.iter().map(|peer| on_loopback(peer.port)).collect(),
        HANG,
    );
    assert_eq!(read.len(), PEERS);
    assert!(
        read.iter()
            .all(|r| matches!(r, Ok(surveyed) if surveyed.lines.len() == 1)),
        "{read:?}"
    );
    answer.release();
    drop(together);

    let in_turn = Answer::together(PEERS);
    let alone = Answering::start(secret(5), FleetScope::Facts, in_turn.clone());
    for _ in 0..2 {
        let one = read_one(
            &on_loopback(alone.port),
            &Keyring::default(),
            (&secret(5), None),
            Duration::from_millis(300),
            "survey",
            &Arc::default(),
        );
        assert!(matches!(one, Err(Unreached::Deadline(..))), "{one:?}");
    }
    in_turn.release();
}

#[test]
fn a_name_is_looked_up_as_itself_and_a_bare_one_as_dot_local() {
    let peer = Answering::start(
        secret(5),
        FleetScope::Facts,
        Answer::Lines(vec![
            "instance pid=1 host=m version=0.104.0 at_ms=1".to_owned(),
        ]),
    );
    let dial_by = |host: &str, resolver: Resolver| {
        dial_avoiding(
            host,
            None,
            peer.port,
            (&secret(5), None),
            FleetOp::Survey,
            (Instant::now() + HANG, HANG),
            (&[], resolver, &|_, _| None),
        )
    };
    let nowhere: Resolver = |_, _| Vec::new();
    let only_dot_local: Resolver = |name, port| {
        if name == "lab-mini.local" {
            vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))]
        } else {
            Vec::new()
        }
    };
    let unresolved = dial_by("no-such-host", nowhere);
    assert!(
        matches!(&unresolved, Err(Unreached::Resolve(host)) if host == "no-such-host"),
        "{:?}",
        unresolved.err()
    );
    let reached = dial_by("lab-mini", only_dot_local);
    assert!(
        reached.as_ref().is_ok_and(|r| r.addr.ip().is_loopback()),
        "{:?}",
        reached.err()
    );
    let dotted = dial_by("lab-mini.lan", only_dot_local);
    assert!(
        matches!(&dotted, Err(Unreached::Resolve(host)) if host == "lab-mini.lan"),
        "{:?}",
        dotted.err()
    );
}

#[test]
fn at_once_holds_its_limit_and_its_order() {
    use std::sync::atomic::AtomicUsize;
    const LIMIT: usize = 4;
    let items: Vec<usize> = (0..12).collect();
    let (inflight, most, started) = (
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    );
    let answers = at_once(&items, LIMIT, |n| {
        let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
        started.fetch_add(1, Ordering::SeqCst);
        most.fetch_max(now, Ordering::SeqCst);
        let deadline = Instant::now() + HANG;
        while inflight.load(Ordering::SeqCst) < LIMIT
            && started.load(Ordering::SeqCst) < items.len()
        {
            assert!(
                Instant::now() < deadline,
                "the walk never filled its places"
            );
            std::thread::yield_now();
        }
        inflight.fetch_sub(1, Ordering::SeqCst);
        n * 10
    });
    assert_eq!(answers, (0..12).map(|n| n * 10).collect::<Vec<_>>());
    assert_eq!(most.load(Ordering::SeqCst), LIMIT);
}

struct Naming {
    running: Arc<AtomicBool>,
    /// How many surveys it was asked.
    asked: Arc<AtomicUsize>,
}

impl Naming {
    fn on(listener: TcpListener, secret: FleetSecret, node: String) -> Self {
        Self::proving(listener, secret, node, None)
    }

    fn proving(
        listener: TcpListener,
        secret: FleetSecret,
        node: String,
        me: Option<NodeIdentity>,
    ) -> Self {
        let header = format!(
            "instance pid=1 host=m7 version=0.95.0 sessions=0 level=content cut=0 at_ms=1 \
             node={node}"
        );
        Self::replying(listener, secret, me, vec![header])
    }

    fn replying(
        listener: TcpListener,
        secret: FleetSecret,
        me: Option<NodeIdentity>,
        lines: Vec<String>,
    ) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let asked = Arc::new(AtomicUsize::new(0));
        let listening = Listening {
            secret,
            scope: FleetScope::Facts,
            node: me,
            serves: &[FleetOp::Survey],
            slots: 8,
            per_source: 8,
            lifetime: HANG,
        };
        std::thread::spawn({
            let (running, asked) = (Arc::clone(&running), Arc::clone(&asked));
            move || {
                serve_with_handshake(
                    &listener,
                    &listening,
                    &running,
                    HANG,
                    move |served, _| {
                        served.answer(|stream| {
                            stream.set_read_timeout(Some(HANG))?;
                            let mut byte = [0u8; 1];
                            if stream.read(&mut byte)? == 1 {
                                asked.fetch_add(1, Ordering::AcqRel);
                            }
                            while stream.read(&mut byte)? == 1 && byte[0] != b'\n' {}
                            let reply = format!("OK {}\n{}\n", lines.len(), lines.join("\n"));
                            stream.write_all(reply.as_bytes())?;
                            stream.flush()
                        })
                    },
                    |_: FleetEvent| {},
                );
            }
        });
        Self { running, asked }
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::Acquire)
    }
}

impl Drop for Naming {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
    }
}

fn twin_listeners() -> (TcpListener, TcpListener, u16) {
    for _ in 0..16 {
        let Ok(v6) = TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)) else {
            aterm_spec::host::could_not_run("this host has no IPv6 loopback (::1) to bind");
        };
        let port = v6.local_addr().unwrap().port();
        if let Ok(v4) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            return (v6, v4, port);
        }
    }
    aterm_spec::host::could_not_run("no port was free on both loopbacks in 16 tries")
}

#[test]
fn a_recorded_address_that_answers_as_another_gives_way_to_the_name() {
    let (node, other) = ("aa".repeat(16), "bb".repeat(16));
    let target = |addr: [u8; 4], port: u16, node: Option<&str>| Target {
        host: "::1".to_owned(),
        addr: Some(IpAddr::from(addr)),
        port,
        node: node.map(str::to_owned),
        key: None,
    };

    let (v6, v4, port) = twin_listeners();
    let _peer = Naming::on(v6, secret(5), node.clone());
    let _stranger = Naming::on(v4, secret(6), other.clone());
    let read = survey_all(
        &[target([127, 0, 0, 1], port, Some(&node))],
        &Keyring::default(),
        (&secret(5), None),
        HANG,
    );
    match &read[0] {
        Ok(surveyed) => {
            assert_eq!(surveyed.node.as_deref(), Some(node.as_str()));
            assert!(surveyed.addr.is_ipv6(), "{:?}", surveyed.addr);
        }
        Err(e) => panic!("the name was never tried: {e}"),
    }
    drop((_peer, _stranger));

    let (v6, v4, port) = twin_listeners();
    let _peer = Naming::on(v6, secret(5), node.clone());
    let _member = Naming::on(v4, secret(5), other.clone());
    let read = survey_all(
        &[
            target([127, 0, 0, 1], port, Some(&node)),
            target([127, 0, 0, 1], port, None),
            Target {
                host: "127.0.0.1".to_owned(),
                ..target([127, 0, 0, 1], port, Some(&node))
            },
            target([192, 0, 2, 1], port, Some(&node)),
        ],
        &Keyring::default(),
        (&secret(5), None),
        HANG,
    );
    let answered = |at: usize| match &read[at] {
        Ok(surveyed) => (surveyed.node.clone(), surveyed.addr.is_ipv6()),
        Err(e) => panic!("target {at}: {e}"),
    };
    assert_eq!(
        answered(0),
        (Some(node.clone()), true),
        "the name reached it"
    );
    assert_eq!(
        answered(1),
        (Some(other.clone()), false),
        "with no node to check, the recorded address's answer"
    );
    assert!(
        matches!(&read[2], Err(Unreached::OtherNode { at, said, want })
            if at.is_ipv4() && *said == other && *want == node),
        "{:?}",
        read[2]
    );
    assert_eq!(answered(3), (Some(node.clone()), true));

    let far = dial(
        "192.0.2.1",
        None,
        7100,
        (&secret(5), None),
        FleetOp::Survey,
        HANG,
    );
    assert!(
        matches!(&far, Err(Unreached::NotFleet(addr)) if addr.ip() == IpAddr::from([192, 0, 2, 1])),
        "{:?}",
        far.err()
    );
}

#[test]
fn a_peer_that_claims_a_bound_node_without_its_key_is_named_and_never_read() {
    let real = identity(&node_of(0xaa), 1);
    let node = real.node().to_owned();
    let entry = |host: &str, port: u16, key: Option<NodeKey>| Target {
        host: host.to_owned(),
        addr: Some(IpAddr::from([127, 0, 0, 1])),
        port,
        node: Some(node.clone()),
        key,
    };
    let me = identity(&node_of(0x77), 7);
    let read = |targets: &[Target], keys: &Keyring| {
        survey_all(targets, keys, (&secret(5), Some(&me)), HANG)
    };

    for (pretender, what) in [(Some(identity(&node, 2)), "impostor"), (None, "unproved")] {
        let (v6, v4, port) = twin_listeners();
        let _real = Naming::proving(v6, secret(5), node.clone(), Some(real.clone()));
        let fake = Naming::proving(v4, secret(5), node.clone(), pretender);
        let reads = read(
            &[
                entry("::1", port, Some(real.key())),
                entry("127.0.0.1", port, Some(real.key())),
            ],
            &Keyring::default(),
        );
        match &reads[0] {
            Ok(surveyed) => assert_eq!(
                (
                    surveyed.node.as_deref(),
                    surveyed.proven,
                    surveyed.addr.is_ipv6()
                ),
                (Some(node.as_str()), true, true),
                "{what}"
            ),
            Err(e) => panic!("{what}: the name was never tried: {e}"),
        }
        let named = match &reads[1] {
            Err(Unreached::Impostor { at, node: said, .. }) => {
                what == "impostor" && at.is_ipv4() && *said == node
            }
            Err(Unreached::Unproved { at, node: bound }) => {
                what == "unproved" && at.is_ipv4() && *bound == node
            }
            _ => false,
        };
        assert!(named, "{what}: {:?}", reads[1]);
        assert_eq!(fake.asked(), 0, "{what}: its survey never asked");
    }

    let bound = Keyring::of(&[keyed(0xaa, real.key(), [127, 0, 0, 1], 7100)]);
    let by_hand = |port: u16| Target {
        node: None,
        ..entry("127.0.0.1", port, None)
    };
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let impostor = Naming::proving(listener, secret(5), node.clone(), Some(identity(&node, 2)));
    let reads = read(&[by_hand(port)], &bound);
    let said = reads[0].as_ref().map(|_| ()).unwrap_err().to_string();
    assert!(
        matches!(&reads[0], Err(Unreached::Impostor { node: said, .. }) if *said == node)
            && said.contains("claims node aaaaaaaa with another key"),
        "{said}"
    );
    assert_eq!(impostor.asked(), 0);
    drop(impostor);

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let older = Naming::proving(listener, secret(5), node.clone(), None);
    let reads = read(&[by_hand(port), entry("127.0.0.1", port, None)], &bound);
    assert_eq!(reads.len(), 2);
    for (at, read) in reads.iter().enumerate() {
        assert!(
            matches!(read, Err(Unreached::Unproved { node: bound, .. }) if *bound == node),
            "{at}: its header names a bound node it never proved: {read:?}"
        );
    }
    drop(older);

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let _real = Naming::proving(listener, secret(5), node.clone(), Some(real.clone()));
    let proven = |reads: &[Result<Surveyed, Unreached>]| {
        reads
            .iter()
            .map(|read| match read {
                Ok(surveyed) => (surveyed.node.clone(), surveyed.proven),
                Err(e) => panic!("{e}"),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        proven(&read(
            &[entry("127.0.0.1", port, None), by_hand(port)],
            &Keyring::default()
        )),
        vec![(Some(node.clone()), false), (Some(node.clone()), false)],
        "bound to no key: its claim"
    );
    assert_eq!(
        proven(&read(
            &[entry("127.0.0.1", port, None), by_hand(port)],
            &bound
        )),
        vec![(Some(node.clone()), true), (Some(node.clone()), true)],
        "the key the book binds"
    );
}

#[test]
fn this_machine_named_by_hand_is_proven_by_its_own_key() {
    let me = identity(&node_of(0xaa), 1);
    let node = me.node().to_owned();
    let read = |port: u16, me: Option<&NodeIdentity>| {
        let by_hand = Target {
            host: "127.0.0.1".to_owned(),
            addr: Some(IpAddr::from([127, 0, 0, 1])),
            port,
            node: None,
            key: None,
        };
        match survey_all(&[by_hand], &Keyring::default(), (&secret(5), me), HANG).remove(0) {
            Ok(surveyed) => (surveyed.node, surveyed.proven),
            Err(e) => panic!("{e}"),
        }
    };
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let _itself = Naming::proving(listener, secret(5), node.clone(), Some(me.clone()));
    assert_eq!(
        read(port, Some(&me)),
        (Some(node.clone()), true),
        "this machine"
    );
    assert_eq!(
        read(port, None),
        (Some(node.clone()), false),
        "a read that proves no node key of its own"
    );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let _other = Naming::proving(listener, secret(5), node.clone(), Some(identity(&node, 2)));
    assert_eq!(
        read(port, Some(&me)),
        (Some(node), false),
        "this machine's node id under another key"
    );
}

#[test]
fn a_proven_peer_is_read_only_as_the_node_it_proved() {
    let real = identity(&node_of(0xaa), 1);
    let node = real.node().to_owned();
    let me = identity(&node_of(0xbc), 7);
    let mine = me.node().to_owned();
    let head = |fields: &str| {
        format!(
            "instance pid=1 host=m7 version=0.104.0 sessions=0 level=content cut=0 at_ms=1 \
             {fields} instances=1"
        )
    };
    let entry = |port: u16, node: Option<&str>, key: Option<NodeKey>| Target {
        host: "127.0.0.1".to_owned(),
        addr: Some(IpAddr::from([127, 0, 0, 1])),
        port,
        node: node.map(str::to_owned),
        key,
    };
    let read = |proving: Option<&NodeIdentity>, lines: Vec<String>, bound: bool| {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let _peer = Naming::replying(listener, secret(5), proving.cloned(), lines);
        let target = match (proving, bound) {
            (Some(_), true) => entry(port, Some(&node), Some(real.key())),
            (Some(_), false) => entry(port, Some(&node), None),
            (None, _) => entry(port, None, None),
        };
        survey_all(
            &[target],
            &Keyring::default(),
            (&secret(5), Some(&me)),
            HANG,
        )
        .remove(0)
    };

    for honest in [format!("node={node}"), "node=-".to_owned(), String::new()] {
        match read(Some(&real), vec![head(&honest)], true) {
            Ok(surveyed) => assert_eq!(
                (surveyed.node.as_deref(), surveyed.proven),
                (Some(node.as_str()), true),
                "{honest:?}"
            ),
            Err(e) => panic!("an honest header ({honest:?}) was refused: {e}"),
        }
    }

    let encoded = format!("%{:02x}{}", mine.as_bytes()[0], &mine[1..]);
    let claims = [
        (vec![head(&format!("node={mine}"))], mine.clone()),
        (
            vec![head(&format!("node={node}")), head(&format!("node={mine}"))],
            mine.clone(),
        ),
        (
            vec![head(&format!("node={node} node={mine}"))],
            mine.clone(),
        ),
        (vec![head(&format!("node={encoded}"))], mine.clone()),
        (
            vec![head(&format!("node={}", mine.to_ascii_uppercase()))],
            mine.to_ascii_uppercase(),
        ),
    ];
    for bound in [true, false] {
        for (lines, claim) in &claims {
            let refused = read(Some(&real), lines.clone(), bound);
            assert!(
                matches!(&refused, Err(Unreached::Misnamed { at, proved, claimed })
                    if at.is_ipv4() && *proved == node && claimed == claim),
                "bound={bound} {lines:?}: {refused:?}"
            );
        }
    }
    let said = read(Some(&real), claims[0].0.clone(), true)
        .map(|_| ())
        .unwrap_err()
        .to_string();
    assert!(
        said.contains("proved node aaaaaaaa, its survey says node bcbcbcbc: not read"),
        "{said}"
    );

    match read(None, vec![head(&format!("node={mine}"))], false) {
        Ok(surveyed) => assert_eq!(
            (surveyed.node.as_deref(), surveyed.proven),
            (Some(mine.as_str()), false)
        ),
        Err(e) => panic!("a claim from a peer that proved no node was refused: {e}"),
    }

    let (v6, v4, port) = twin_listeners();
    let _real = Naming::proving(v6, secret(5), node.clone(), Some(real.clone()));
    let _misnamed = Naming::proving(v4, secret(5), mine.clone(), Some(real.clone()));
    let recorded = Target {
        host: "::1".to_owned(),
        ..entry(port, Some(&node), Some(real.key()))
    };
    match survey_all(
        &[recorded],
        &Keyring::default(),
        (&secret(5), Some(&me)),
        HANG,
    )
    .remove(0)
    {
        Ok(surveyed) => assert_eq!(
            (
                surveyed.node.as_deref(),
                surveyed.proven,
                surveyed.addr.is_ipv6()
            ),
            (Some(node.as_str()), true, true)
        ),
        Err(e) => panic!("the name was never tried: {e}"),
    }
}

#[test]
fn a_listener_names_only_the_node_its_book_binds_the_proved_key_to() {
    let state = State::new("proven");
    let (real, unbound) = (identity(&node_of(0xa1), 1), identity(&node_of(0xb2), 2));
    write_book(
        state.dir(),
        &[
            keyed(0xa1, real.key(), [127, 0, 0, 1], 7100),
            joined(0xb2, "m8", [127, 0, 0, 1], 7101),
        ],
    );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (told, judged) = std::sync::mpsc::channel();
    let running = Arc::new(AtomicBool::new(true));
    let listening = Listening {
        secret: secret(5),
        scope: FleetScope::Facts,
        node: Some(identity(&node_of(0x11), 3)),
        serves: &[FleetOp::Survey],
        slots: 8,
        per_source: 8,
        lifetime: HANG,
    };
    let serving = std::thread::spawn({
        let (running, dir) = (Arc::clone(&running), state.dir().to_path_buf());
        move || {
            serve_with_handshake(
                &listener,
                &listening,
                &running,
                HANG,
                move |served, _| {
                    let _ = told.send(proven_peer(&served, &dir));
                    served.answer(|_| Ok(()))
                },
                |_: FleetEvent| {},
            );
        }
    });
    let taker = identity(real.node(), 9);
    for (me, want) in [
        (Some(&real), Ok(real.node().to_owned())),
        (
            Some(&taker),
            Err(Unproven::OtherKey(real.node().to_owned())),
        ),
        (
            Some(&unbound),
            Err(Unproven::Unbound(unbound.node().to_owned())),
        ),
        (None, Err(Unproven::NoProof)),
    ] {
        let reached = dial(
            "127.0.0.1",
            None,
            port,
            (&secret(5), me),
            FleetOp::Survey,
            HANG,
        )
        .expect("the listener proves survey");
        assert_eq!(judged.recv_timeout(HANG), Ok(want));
        drop(reached);
    }
    running.store(false, Ordering::Release);
    serving.join().unwrap();
}

fn node_listener(
    me: NodeIdentity,
    serves: &'static [FleetOp],
) -> (u16, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    node_listener_on(listener, me, serves)
}

fn node_listener_on(
    listener: TcpListener,
    me: NodeIdentity,
    serves: &'static [FleetOp],
) -> (u16, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let port = listener.local_addr().unwrap().port();
    let running = Arc::new(AtomicBool::new(true));
    let listening = Listening {
        secret: secret(6),
        scope: FleetScope::Content,
        node: Some(me),
        serves,
        slots: 8,
        per_source: 8,
        lifetime: HANG,
    };
    let serving = std::thread::spawn({
        let running = Arc::clone(&running);
        move || {
            serve_with_handshake(
                &listener,
                &listening,
                &running,
                HANG,
                move |served, op| {
                    served.answer(|stream| {
                        stream.set_read_timeout(Some(HANG))?;
                        let mut line = Vec::new();
                        let mut byte = [0u8; 1];
                        while stream.read(&mut byte)? == 1 && byte[0] != b'\n' {
                            line.push(byte[0]);
                        }
                        let said = String::from_utf8_lossy(&line).into_owned();
                        stream.write_all(format!("OK {} {said}\n", op.word()).as_bytes())?;
                        stream.flush()
                    })
                },
                |_: FleetEvent| {},
            );
        }
    });
    (port, running, serving)
}

/// A person's drive is served by a listener that dropped its first connection, as one at its slots
/// does. NEGATIVE CONTROL: `drive` reaching its node once, with no [`passes`]: the dial hangs up and
/// this fails.
#[test]
fn a_drive_is_served_by_a_listener_that_hung_up_on_its_first_connection() {
    let host = identity(&node_of(0xd8), 4);
    let me = identity(&node_of(0x3b), 8);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let serving = std::thread::spawn({
        let host = host.clone();
        move || {
            drop(listener.accept().unwrap());
            node_listener_on(listener, host, &[FleetOp::Drive])
        }
    });
    let target = Target {
        host: "127.0.0.1".to_owned(),
        addr: None,
        port,
        node: Some(host.node().to_owned()),
        key: Some(host.key()),
    };
    let driving = drive(
        &target,
        &Keyring::default(),
        (&secret(6), &me),
        (Instant::now() + HANG, HANG),
    );
    assert!(driving.is_ok(), "{:?}", driving.err());
    drop(driving);
    let (_, running, served) = serving.join().unwrap();
    running.store(false, Ordering::Release);
    served.join().unwrap();
}

/// Any other dial takes one pass however long its budget: a heartbeat, a join's place and a vault
/// round meet a full listener once, and a join's next place keeps its time. NEGATIVE CONTROL:
/// [`dial_avoiding`] taking [`passes`]: the count is 4.
#[test]
fn a_dial_takes_one_pass_however_long_its_budget() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let (ended, end) = std::sync::mpsc::channel::<()>();
    let hanging = std::thread::spawn({
        let accepted = Arc::clone(&accepted);
        move || {
            listener.set_nonblocking(true).unwrap();
            while end.try_recv().is_err() {
                match listener.accept() {
                    Ok(connection) => {
                        accepted.fetch_add(1, Ordering::SeqCst);
                        drop(connection);
                    }
                    Err(_) => std::thread::yield_now(),
                }
            }
        }
    });
    let dialled = dial(
        "127.0.0.1",
        None,
        port,
        (&secret(6), None),
        FleetOp::Survey,
        HANG,
    );
    ended.send(()).unwrap();
    hanging.join().unwrap();
    assert!(dialled.is_err(), "a listener that only hangs up served");
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

fn busy() -> Unreached {
    Unreached::Handshake(
        SocketAddr::from(([127, 0, 0, 1], 7100)),
        FleetError::AddressBusy,
    )
}

/// A listener at a cap on every pass gets all of them, and the reason is the first's. NEGATIVE
/// CONTROL: a past-proof return ahead of `served_later` in [`passes`]: the count is 2.
#[test]
fn a_drive_keeps_dialling_a_listener_at_its_cap_until_its_passes_are_spent() {
    let now = Instant::now();
    let mut tried = 0;
    let dialled: Result<(), _> = passes((now + HANG, now + HANG), |_| {
        tried += 1;
        Err(busy())
    });
    assert_eq!(tried, AGAIN_AFTER.len() + 1);
    assert!(
        matches!(
            dialled,
            Err(Unreached::Handshake(_, FleetError::AddressBusy))
        ),
        "{dialled:?}"
    );
}

/// A later pass's definite refusal is what the dial reports; one that only could not connect, or
/// this machine's own state, says nothing of the listener, and the first reason stands. NEGATIVE CONTROL: [`passes`] returning the
/// first reason on any answer that is not worth another pass: the refusal is lost.
#[test]
fn a_later_passs_refusal_stands_and_a_later_connect_failure_does_not() {
    let at = SocketAddr::from(([127, 0, 0, 1], 7100));
    let now = Instant::now();
    let mut tried = 0;
    let refused: Result<(), _> = passes((now + HANG, now + HANG), |_| {
        tried += 1;
        Err(if tried == 1 {
            busy()
        } else {
            Unreached::Handshake(at, FleetError::Refused)
        })
    });
    assert!(
        matches!(refused, Err(Unreached::Handshake(_, FleetError::Refused))),
        "{refused:?}"
    );
    let now = Instant::now();
    let mut tried = 0;
    let gone: Result<(), _> = passes((now + HANG, now + HANG), |_| {
        tried += 1;
        Err(if tried == 1 {
            busy()
        } else {
            Unreached::Connect(at, io::ErrorKind::ConnectionRefused.into())
        })
    });
    assert!(
        matches!(gone, Err(Unreached::Handshake(_, FleetError::AddressBusy))),
        "{gone:?}"
    );
    let now = Instant::now();
    let mut tried = 0;
    let local: Result<(), _> = passes((now + HANG, now + HANG), |_| {
        tried += 1;
        Err(if tried == 1 {
            busy()
        } else {
            Unreached::Local("could not look it up".to_owned())
        })
    });
    assert!(
        matches!(local, Err(Unreached::Handshake(_, FleetError::AddressBusy))),
        "this machine's own state says nothing of the listener: {local:?}"
    );
}

/// Each further pass ends by `dial_by`, and none starts once its pause would pass it: what stays is
/// a turn's to answer in. NEGATIVE CONTROL: `pass(deadline)` for a further pass: the deadlines match
/// and the first assert fails. Dropping the `dial_by` check: three more passes run.
#[test]
fn a_further_pass_keeps_to_the_time_a_reply_is_not_owed() {
    let now = Instant::now();
    let (deadline, dial_by) = (now + HANG, now + Duration::from_millis(300));
    let mut ends = Vec::new();
    let dialled: Result<(), _> = passes((deadline, dial_by), |by| {
        ends.push(by);
        Err(busy())
    });
    assert!(dialled.is_err());
    assert_eq!(ends, [deadline, dial_by]);
}

/// A drive's further passes end the same way into any deadline: a 28 s one keeps a turn's longest
/// wait, an 8 s one that asks for no wait may still try again, and one shorter than that ends with
/// its deadline. NEGATIVE CONTROL: the end counted back from the deadline by a turn's longest wait:
/// the 8 s deadline's end is before it started.
#[test]
fn a_drives_further_passes_end_the_same_way_into_any_deadline() {
    let now = Instant::now();
    let room = Duration::from_millis(22_000);
    let long = Duration::from_secs(28);
    assert!(now + long - further_passes_end((now + long, long)) >= room);
    let short = Duration::from_secs(8);
    assert_eq!(
        further_passes_end((now + short, short)),
        now + DRIVE_DIAL_WITHIN
    );
    let shorter = Duration::from_secs(2);
    assert_eq!(further_passes_end((now + shorter, shorter)), now + shorter);
}

/// A stale recorded address does not hide a listener that could serve later: the kept reason is what
/// the listener did, which [`passes`] decides on. NEGATIVE CONTROL: `walk` keeping the first address's
/// connect failure: the listener at the name's address is not reported and this fails.
#[test]
fn a_dead_address_does_not_hide_a_listener_that_could_serve_later() {
    // Not the default port: a test binary's walk refuses that one before any attempt.
    let port = 7101;
    let at = SocketAddr::from(([127, 0, 0, 1], port));
    let walked: Result<(), _> = walk(
        ("localhost", Some(Ipv4Addr::LOCALHOST.into()), port),
        (Instant::now() + HANG, HANG),
        (&[], |_, port| {
            vec![SocketAddr::from(([127, 0, 0, 2], port))]
        }),
        |to| {
            AtAddr::Passed(if to == at {
                Unreached::Connect(to, io::ErrorKind::ConnectionRefused.into())
            } else {
                Unreached::Deadline(Stage::Handshake, HANG)
            })
        },
    );
    assert!(
        matches!(&walked, Err(why) if why.served_later()),
        "{walked:?}"
    );
}

/// A refusal on the first pass is final: only a listener that could serve later is dialled again.
#[test]
fn a_drive_a_listener_refuses_takes_one_pass() {
    let now = Instant::now();
    let mut tried = 0;
    let dialled: Result<(), _> = passes((now + HANG, now + HANG), |_| {
        tried += 1;
        Err(Unreached::Handshake(
            SocketAddr::from(([127, 0, 0, 1], 7100)),
            FleetError::Refused,
        ))
    });
    assert_eq!(tried, 1, "{dialled:?}");
}

#[test]
fn a_silent_recorded_address_gives_way_to_a_name_without_losing_the_bound_key() {
    let host = identity(&node_of(0xd7), 4);
    let me = identity(&node_of(0x3a), 8);
    for genuine in [true, false] {
        let (v6, v4, port) = twin_listeners();
        let silent = HeldSilent::at(v4.local_addr().unwrap());
        let named = if genuine {
            host.clone()
        } else {
            identity(host.node(), 5)
        };
        let (_, running, serving) = node_listener_on(v6, named, &[FleetOp::Drive]);
        let target = Target {
            host: "::1".to_owned(),
            addr: Some(Ipv4Addr::LOCALHOST.into()),
            port,
            node: Some(host.node().to_owned()),
            key: Some(host.key()),
        };
        let observed = match drive(
            &target,
            &Keyring::default(),
            (&secret(6), &me),
            (Instant::now() + HANG, HANG),
        ) {
            Ok(mut driven) => {
                driven.write_all(b"s-1 status\n").unwrap();
                let mut answer = String::new();
                io::BufRead::read_line(&mut io::BufReader::new(&mut driven), &mut answer).unwrap();
                Ok((driven.node.clone(), driven.addr, answer))
            }
            Err(why) => Err(why),
        };
        running.store(false, Ordering::Release);
        serving.join().unwrap();
        assert!(
            silent.overrode().is_some(),
            "the recorded address was tried"
        );
        if genuine {
            let (node, address, answer) = observed.expect("the name still reaches the bound node");
            assert_eq!(node, host.node());
            assert!(address.is_ipv6());
            assert_eq!(answer, "OK drive s-1 status\n");
        } else {
            assert!(
                matches!(observed, Err(Unreached::Impostor { node, .. }) if node == host.node()),
                "an alternate address must still prove the bound key"
            );
        }
    }
}

#[test]
fn a_drive_reaches_only_the_node_this_machine_binds_a_key_to() {
    let host = identity(&node_of(0xd7), 4);
    let listen = node_listener;
    let target = |port| Target {
        host: "127.0.0.1".to_owned(),
        addr: None,
        port,
        node: Some(host.node().to_owned()),
        key: None,
    };
    let me = identity(&node_of(0x3a), 8);
    let bound = Keyring::of(&[keyed(0xd7, host.key(), [127, 0, 0, 1], 7100)]);

    let (port, running, serving) = listen(host.clone(), &[FleetOp::Survey, FleetOp::Drive]);
    let mut driven = drive(
        &target(port),
        &bound,
        (&secret(6), &me),
        (Instant::now() + HANG, HANG),
    )
    .expect("the bound node is driven");
    assert_eq!(driven.node, host.node());
    driven.write_all(b"s-1 status\n").unwrap();
    let mut answer = String::new();
    io::BufRead::read_line(&mut io::BufReader::new(&mut driven), &mut answer).unwrap();
    assert_eq!(answer, "OK drive s-1 status\n", "the line after the proof");
    drop(driven);
    match drive(
        &target(port),
        &Keyring::default(),
        (&secret(6), &me),
        (Instant::now() + HANG, HANG),
    ) {
        Err(Unreached::Unbound { node, .. }) => {
            assert_eq!(
                node.as_deref(),
                Some(host.node()),
                "bound to no key: not driven"
            );
        }
        other => panic!(
            "an unbound node was driven: {:?}",
            other.map(|d| d.node.clone())
        ),
    }
    running.store(false, Ordering::Release);
    serving.join().unwrap();

    let (port, running, serving) =
        listen(identity(host.node(), 5), &[FleetOp::Survey, FleetOp::Drive]);
    match drive(
        &target(port),
        &bound,
        (&secret(6), &me),
        (Instant::now() + HANG, HANG),
    ) {
        Err(Unreached::Impostor { node, .. }) => assert_eq!(node, host.node()),
        other => panic!(
            "another key was driven: {:?}",
            other.map(|d| d.node.clone())
        ),
    }
    running.store(false, Ordering::Release);
    serving.join().unwrap();

    let (port, running, serving) = listen(host.clone(), &[FleetOp::Survey]);
    match drive(
        &target(port),
        &bound,
        (&secret(6), &me),
        (Instant::now() + HANG, HANG),
    ) {
        Err(why @ Unreached::Handshake(_, FleetError::UnknownOp(_))) => {
            assert!(
                why.to_string().contains("aterm fleet allow drive"),
                "names what its person runs: {why}"
            );
        }
        other => panic!(
            "a listener that serves no drive: {:?}",
            other.map(|d| d.node.clone())
        ),
    }
    running.store(false, Ordering::Release);
    serving.join().unwrap();
}

#[test]
fn a_drive_its_deadline_hangs_up_reads_as_that_deadline_never_as_the_peers_cut() {
    let host = identity(&node_of(0xd7), 4);
    let me = identity(&node_of(0x3a), 8);
    let bound = Keyring::of(&[keyed(0xd7, host.key(), [127, 0, 0, 1], 7100)]);
    let (port, running, serving) = node_listener(host.clone(), &[FleetOp::Survey, FleetOp::Drive]);
    let target = Target {
        host: "127.0.0.1".to_owned(),
        addr: None,
        port,
        node: Some(host.node().to_owned()),
        key: None,
    };
    let mut driven = drive(
        &target,
        &bound,
        (&secret(6), &me),
        (Instant::now() + HANG, HANG),
    )
    .expect("the bound node is driven");
    // The deadline's watchdog hangs up before the socket's own read timeout, as it may at one
    // deadline.
    driven
        .stream
        .get_ref()
        .set_read_timeout(Some(HANG))
        .unwrap();
    if let Some(watchdog) = driven.watchdog.take() {
        let _ = watchdog.finish();
    }
    driven.watchdog =
        Some(HandshakeWatchdog::arm(driven.stream.get_ref(), Duration::from_millis(50)).unwrap());
    let mut line = String::new();
    let read = io::BufRead::read_line(&mut io::BufReader::new(&mut driven), &mut line);
    assert_eq!(read.map_err(|e| e.kind()), Err(io::ErrorKind::TimedOut));
    let wrote = driven
        .write_all(b"s-1 status\n")
        .and_then(|()| driven.flush());
    assert_eq!(wrote.map_err(|e| e.kind()), Err(io::ErrorKind::TimedOut));
    drop(driven);
    running.store(false, Ordering::Release);
    serving.join().unwrap();
}

#[test]
fn a_holder_proves_itself_at_any_level_and_only_another_fleet_or_key_disowns_it() {
    let host = identity(&node_of(0xd7), 4);
    let me = identity(&node_of(0x3a), 8);
    let bound = Keyring::of(&[keyed(0xd7, host.key(), [127, 0, 0, 1], 7100)]);
    let target = |port| Target {
        host: "127.0.0.1".to_owned(),
        addr: None,
        port,
        node: Some(host.node().to_owned()),
        key: None,
    };
    let refused = |port, secret: &FleetSecret| {
        proves_itself(&target(port), &bound, (secret, &me), HANG).expect_err("it is refused")
    };

    let (port, running, serving) = node_listener(host.clone(), &[FleetOp::Survey, FleetOp::Book]);
    assert_eq!(
        proves_itself(&target(port), &bound, (&secret(6), &me), HANG).expect("it proves itself"),
        SocketAddr::from(([127, 0, 0, 1], port)),
        "a listener that lets no one drive it still proves its node"
    );
    let rotated = refused(port, &secret(7));
    assert!(
        matches!(rotated, Unreached::Handshake(..)) && rotated.disowns(),
        "another fleet's secret there: {rotated:?}"
    );
    running.store(false, Ordering::Release);
    serving.join().unwrap();
    let stopped = refused(port, &secret(6));
    assert!(
        matches!(stopped, Unreached::Connect(..)) && !stopped.disowns(),
        "nothing listening says nothing of its drives: {stopped:?}"
    );

    let (port, running, serving) =
        node_listener(identity(host.node(), 5), &[FleetOp::Survey, FleetOp::Book]);
    let rekeyed = refused(port, &secret(6));
    assert!(
        matches!(rekeyed, Unreached::Impostor { .. }) && rekeyed.disowns(),
        "{rekeyed:?}"
    );
    running.store(false, Ordering::Release);
    serving.join().unwrap();

    let (port, running, serving) = node_listener(
        identity(&node_of(0xe8), 9),
        &[FleetOp::Survey, FleetOp::Book],
    );
    let moved = refused(port, &secret(6));
    assert!(
        matches!(moved, Unreached::OtherNode { .. }) && !moved.disowns(),
        "another node at its place says nothing of where it went: {moved:?}"
    );
    running.store(false, Ordering::Release);
    serving.join().unwrap();

    let at = SocketAddr::from(([192, 168, 1, 23], 7100));
    let connect = |errno| Unreached::Connect(at, io::Error::from_raw_os_error(errno));
    assert!(Unreached::Handshake(at, FleetError::Refused).disowns());
    for undecided in [
        connect(libc::ECONNREFUSED),
        connect(libc::EHOSTUNREACH),
        connect(libc::ENETUNREACH),
        connect(libc::EPERM),
        connect(libc::ETIMEDOUT),
        Unreached::Deadline(Stage::Connect, HANG),
        Unreached::Deadline(Stage::Handshake, HANG),
        Unreached::Deadline(Stage::Book, HANG),
        Unreached::Handshake(at, FleetError::AddressBusy),
        Unreached::Handshake(at, FleetError::Io(io::ErrorKind::ConnectionReset.into())),
        Unreached::Lookup("lab-mini.local".to_owned(), HANG),
        Unreached::Local("no socket".to_owned()),
        Unreached::Unproved {
            at,
            node: node_of(0xd7),
        },
    ] {
        assert!(!undecided.disowns(), "{undecided:?}");
    }
}

#[test]
fn a_listener_that_hangs_up_in_the_handshake_is_said_to() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let hanging = std::thread::spawn(move || drop(listener.accept().unwrap()));
    let dialled = dial(
        "127.0.0.1",
        None,
        port,
        (&secret(6), None),
        FleetOp::Survey,
        HANG,
    );
    hanging.join().unwrap();
    let Err(why) = dialled else {
        panic!("a listener that hung up served");
    };
    assert!(why.hung_up(), "{why:?}");
    let said = why.to_string();
    assert!(
        said.contains("hung up mid-handshake") && said.contains("try again"),
        "{said}"
    );
    let at = SocketAddr::from(([192, 168, 1, 23], 7100));
    for not in [
        Unreached::Handshake(at, FleetError::AddressBusy),
        Unreached::Handshake(at, FleetError::Tls(io::ErrorKind::InvalidData.into())),
        Unreached::Connect(at, io::ErrorKind::ConnectionReset.into()),
    ] {
        assert!(!not.hung_up(), "{not:?}");
    }
}

#[test]
fn a_connect_failure_is_read_by_its_kind() {
    let at = SocketAddr::from(([192, 168, 1, 23], 7100));
    let connect = |errno| Unreached::Connect(at, io::Error::from_raw_os_error(errno));
    let kinds = |u: &Unreached| (u.not_permitted(), u.no_route(), u.no_network());
    for errno in [libc::EPERM, libc::EACCES] {
        let refused = connect(errno);
        assert_eq!(kinds(&refused), (true, false, false), "{refused:?}");
    }
    let host = connect(libc::EHOSTUNREACH);
    assert_eq!(kinds(&host), (false, true, false), "{host:?}");
    let net = connect(libc::ENETUNREACH);
    assert_eq!(kinds(&net), (false, false, true), "{net:?}");
    for neither in [
        connect(libc::ECONNREFUSED),
        Unreached::Handshake(at, FleetError::Refused),
    ] {
        assert_eq!(kinds(&neither), (false, false, false), "{neither:?}");
        assert!(neither.dialled(), "{neither:?}");
    }
    for never in [
        Unreached::Resolve("m7.local".to_owned()),
        Unreached::Lookup("m7.local".to_owned(), Duration::from_secs(3)),
        Unreached::NotFleet(SocketAddr::from(([192, 0, 2, 1], 7100))),
        Unreached::Local("no socket".to_owned()),
    ] {
        assert!(!never.dialled(), "{never:?}");
    }
}

/// ON macOS AN ACCESS LIST IS PART OF PRIVACY: a key file whose ACL allows
/// everyone to read it is refused like a 0644 one, however 0600 its mode
/// bits are; a deny entry (what macOS puts on `~/Library`) is no refusal;
/// and `chmod -N` makes it readable again.
#[cfg(target_os = "macos")]
#[test]
fn an_access_list_that_lets_others_in_is_not_private() {
    let state = State::new("acl");
    write_key(state.dir(), &[0x3e; SECRET_LEN]).unwrap();
    let key = state.dir().join(KEY_FILE);
    let chmod = |args: &[&std::ffi::OsStr]| {
        let ran = aterm_uds::spawnfd::command("/bin/chmod")
            .args(args)
            .status()
            .expect("run chmod");
        assert!(ran.success(), "chmod {args:?}");
    };
    let word = || read_key_file(state.dir()).err().map(|e| e.word());
    assert_eq!(word(), None);
    chmod(&[
        "+a".as_ref(),
        "everyone allow read".as_ref(),
        key.as_os_str(),
    ]);
    assert_eq!(word(), Some("key-mode"), "an allow entry opens it");
    chmod(&["-N".as_ref(), key.as_os_str()]);
    assert_eq!(word(), None);
    chmod(&[
        "+a".as_ref(),
        "everyone deny delete".as_ref(),
        key.as_os_str(),
    ]);
    assert_eq!(word(), None, "a deny entry takes nothing away");
    chmod(&["-N".as_ref(), key.as_os_str()]);

    chmod(&[
        "+a".as_ref(),
        "everyone allow list,search".as_ref(),
        state.dir().as_os_str(),
    ]);
    assert_eq!(word(), Some("key-mode"), "nor through its directory");
    assert!(ensure_private_dir(state.dir()).is_err());
    assert!(write_key(state.dir(), &[0x3f; SECRET_LEN]).is_err());
    chmod(&["-N".as_ref(), state.dir().as_os_str()]);
    assert_eq!(word(), None);
}

#[test]
fn resolver_work_keeps_its_process_budget_until_it_retires() {
    let in_flight = std::sync::atomic::AtomicUsize::new(0);
    let mut held: Vec<_> = (0..RESOLVER_THREADS_MAX)
        .map(|_| ResolverPermit::take(&in_flight).expect("a free resolver permit"))
        .collect();
    assert_eq!(in_flight.load(Ordering::Relaxed), RESOLVER_THREADS_MAX);
    let extra = ResolverPermit::take(&in_flight);
    let refused = extra.is_none();
    drop(extra);
    drop(held.pop().expect("one finished resolver"));
    let replacement = ResolverPermit::take(&in_flight).expect("the freed permit");
    drop(replacement);
    drop(held);
    assert_eq!(in_flight.load(Ordering::Relaxed), 0);

    let permit = ResolverPermit::take(&in_flight).expect("captured worker permit");
    let unstarted = move || drop(permit);
    assert_eq!(in_flight.load(Ordering::Relaxed), 1);
    drop(unstarted);
    assert_eq!(in_flight.load(Ordering::Relaxed), 0);

    let panicked = std::panic::catch_unwind(|| {
        let _permit = ResolverPermit::take(&in_flight).expect("unwinding worker permit");
        panic!("captured resolver unwind");
    });
    assert!(panicked.is_err());
    assert!(
        refused && in_flight.load(Ordering::Relaxed) == 0,
        "RESOLVER_WORK_MUST_KEEP_A_BOUNDED_PROCESS_PERMIT"
    );
}

/// Resolving `_alab._tcp` advert hints returns on its budget while a lookup hangs, and a place
/// after the hanging one keeps its address; the loopback guard skips off-loopback lookups and
/// addresses. The 120 ms budget is only a lower bound on the wait: the hanging lookup is held
/// until the test lets it go, so the round can only end on its budget.
/// NEGATIVE CONTROL: wait on each lookup with no deadline (`rx.recv()` for
/// `rx.recv_timeout(left)`): the round returns only when the hanging lookup gives up at the hang
/// detector, so the check that it was still held fails.
#[test]
fn resolving_advert_hints_returns_on_its_budget_while_a_lookup_hangs() {
    static RELEASE_HANG: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>> =
        std::sync::Mutex::new(None);
    let (tx, rx) = std::sync::mpsc::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    *RELEASE_HANG.lock().unwrap() = Some(tx);
    static RECV_HANG: std::sync::Mutex<
        Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    > = std::sync::Mutex::new(None);
    *RECV_HANG.lock().unwrap() = Some((ready_tx, rx));
    // Set once the hanging lookup is let go: still clear when resolution returns, the round ended
    // on its budget, not on that lookup.
    static HANG_ENDED: AtomicBool = AtomicBool::new(false);

    let resolver: Resolver = |name, port| match name {
        "hanging.local" => {
            let pair = RECV_HANG.lock().unwrap().take();
            if let Some((ready, rx)) = pair {
                let _ = ready.send(());
                let _ = rx.recv_timeout(HANG);
                HANG_ENDED.store(true, Ordering::Release);
            }
            vec![SocketAddr::from(([10, 0, 0, 99], port))]
        }
        "m22.local" => vec![SocketAddr::from(([10, 0, 0, 22], port))],
        _ => Vec::new(),
    };
    let places = vec![
        ("hanging.local".to_owned(), 7100),
        ("10.0.0.8".to_owned(), 7102),
    ];
    let resolved = crate::loopback::forced(false, || {
        resolve_ipv4_hints_with(&places, Duration::from_millis(120), resolver)
    });
    let lookup_still_held = !HANG_ENDED.load(Ordering::Acquire);
    ready_rx
        .recv_timeout(HANG)
        .expect("hanging resolver started");
    if let Some(tx) = RELEASE_HANG.lock().unwrap().take() {
        let _ = tx.send(());
    }
    assert!(
        lookup_still_held,
        "hint resolution returned only once the hanging lookup was let go, not on its budget"
    );
    assert_eq!(
        resolved,
        vec![SocketAddr::from(([10, 0, 0, 8], 7102))],
        "a place after a hanging lookup keeps its address"
    );

    let guarded = crate::loopback::forced(true, || {
        resolve_ipv4_hints_with(
            &[
                ("m22.local".to_owned(), 7101),
                ("10.0.0.8".to_owned(), 7102),
                ("127.0.0.1".to_owned(), 7103),
            ],
            Duration::from_millis(120),
            resolver,
        )
    });
    assert_eq!(
        guarded,
        vec![SocketAddr::from(([127, 0, 0, 1], 7103))],
        "loopback guard skips off-loopback lookups and addresses"
    );
}

/// Advert hint lookups run concurrently: a lookup that hangs does not starve a later one. The
/// hanging lookup is held until `m22.local` is looked up and fails if that never happens, so the
/// round resolves `m22.local` and the IP literal after it in `places`'s order.
/// NEGATIVE CONTROL: resolve each place on the calling thread in order (no spawn): the hanging
/// lookup's `expect` panics after the hang detector, because `m22.local` was never asked.
#[test]
fn a_hanging_advert_lookup_does_not_starve_a_later_one() {
    // `m22.local`'s lookup signals here; the hanging lookup waits on the other end.
    static M22_ASKED: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>> =
        std::sync::Mutex::new(None);
    static M22_HEARD: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>> =
        std::sync::Mutex::new(None);
    let (asked, heard) = std::sync::mpsc::channel();
    *M22_ASKED.lock().unwrap() = Some(asked);
    *M22_HEARD.lock().unwrap() = Some(heard);

    let resolver: Resolver = |name, port| match name {
        "hanging.local" => {
            let heard = M22_HEARD.lock().unwrap().take();
            if let Some(heard) = heard {
                heard
                    .recv_timeout(HANG)
                    .expect("m22 was looked up while this lookup was held");
            }
            Vec::new()
        }
        "m22.local" => {
            let asked = M22_ASKED.lock().unwrap().take();
            if let Some(asked) = asked {
                let _ = asked.send(());
            }
            vec![SocketAddr::from(([10, 0, 0, 22], port))]
        }
        _ => Vec::new(),
    };
    let places = vec![
        ("hanging.local".to_owned(), 7100),
        ("m22.local".to_owned(), 7101),
        ("10.0.0.8".to_owned(), 7102),
    ];
    let resolved =
        crate::loopback::forced(false, || resolve_ipv4_hints_with(&places, HANG, resolver));
    assert_eq!(
        resolved,
        vec![
            SocketAddr::from(([10, 0, 0, 22], 7101)),
            SocketAddr::from(([10, 0, 0, 8], 7102)),
        ],
        "a responsive advert after a hanging one still resolves"
    );
}

/// The fleet as `aterm fleet peers` lists it on m22 (2026-10-27), and the words `--host` takes.
fn measured_fleet() -> Vec<Peer> {
    vec![
        joined(
            0x10,
            "m102-macbook-M4-Pro-24GB-500GB.local",
            [192, 168, 1, 22],
            7100,
        ),
        joined(
            0x11,
            "MacbookAir-m27-M5-32GB-4TB.local",
            [192, 168, 1, 27],
            7100,
        ),
        joined(0x12, "m1-Air-M2-24GB-1TB-2.local", [192, 168, 1, 41], 7100),
        joined(0x13, "Andrews-Mac-Studio-3.local", [192, 168, 1, 50], 7100),
        joined(0x14, "m8-mini-a.local", [192, 168, 1, 8], 7100),
        joined(0x15, "m8-mini-b.local", [192, 168, 1, 9], 7100),
    ]
}

#[test]
fn a_hosts_short_names_are_its_lead_word_and_its_machine_number() {
    use ShortName::{Lead, Number};
    fn names(host: &str) -> (Vec<&str>, Vec<&str>) {
        (short_names(host, Lead), short_names(host, Number))
    }
    assert_eq!(
        names("m102-macbook-M4-Pro-24GB-500GB.local"),
        (vec!["m102"], vec![])
    );
    assert_eq!(
        names("MacbookAir-m27-M5-32GB-4TB.local"),
        (vec!["MacbookAir"], vec!["m27"])
    );
    assert_eq!(
        names("m1-Air-M2-24GB-1TB-2"),
        (vec!["m1"], vec![]),
        "never the chip's M2"
    );
    assert_eq!(
        names("Andrews-Mac-Studio-3.local"),
        (vec!["Andrews"], vec![])
    );
    assert_eq!(
        names("m9.local"),
        (vec!["m9"], vec![]),
        "up to the first `.`"
    );
    assert_eq!(
        names("lab-m3-m3.local"),
        (vec!["lab"], vec!["m3"]),
        "each once"
    );
    assert_eq!(
        names("lab-m-M3-m3x-mm3.local"),
        (vec!["lab"], vec![]),
        "an `m` with no digits, or more than digits, is no number"
    );
    for address in ["192.168.1.22", "fe80::1"] {
        assert_eq!(names(address), (vec![], vec![]), "an address has none");
    }
}

#[test]
fn a_short_name_names_one_peer_or_is_refused_naming_each() {
    let book = measured_fleet();
    let hosts = |what: &str| -> Vec<String> {
        all_short_named(&book, &[], what)
            .into_iter()
            .map(|peer| peer.host.clone())
            .collect()
    };
    assert_eq!(hosts("m102"), ["m102-macbook-M4-Pro-24GB-500GB.local"]);
    assert_eq!(hosts("M102"), ["m102-macbook-M4-Pro-24GB-500GB.local"]);
    assert_eq!(hosts("m27"), ["MacbookAir-m27-M5-32GB-4TB.local"]);
    assert_eq!(hosts("macbookair"), ["MacbookAir-m27-M5-32GB-4TB.local"]);
    assert_eq!(hosts("m1"), ["m1-Air-M2-24GB-1TB-2.local"]);
    assert!(hosts("m2").is_empty(), "a chip's M2 names no Mac");
    assert!(hosts("m5").is_empty(), "nor its M5");
    assert!(
        hosts("m10").is_empty(),
        "m102's number is no prefix of another"
    );
    assert!(hosts("zz-unknown").is_empty(), "read as typed");
    let many = all_short_named(&book, &[], "m8");
    assert_eq!(many.len(), 2, "never a pick");
    assert_eq!(
        several_short("m8", &many),
        "`m8` is a short name of 2 peers (m8-mini-a.local at 192.168.1.8:7100, m8-mini-b.local \
         at 192.168.1.9:7100): name one by its host, node id or address (`aterm fleet peers`)"
    );
    // A lead word is read before a machine number: `m27` names the Mac whose host leads with
    // it, never both.
    let numbered = [
        book.clone(),
        vec![joined(0x20, "m27-mini.local", [192, 168, 1, 60], 7100)],
    ]
    .concat();
    let named: Vec<&str> = all_short_named(&numbered, &[], "m27")
        .into_iter()
        .map(|peer| peer.host.as_str())
        .collect();
    assert_eq!(named, ["m27-mini.local"]);
    // A whole host, its first label, its address and its leading `m<N>`
    // ([`HostRank::LeadingMachine`]) name it as every verb reads a peer's word; its other short
    // names are no part of that.
    for word in [
        "m102-macbook-M4-Pro-24GB-500GB.local",
        "m102-macbook-m4-pro-24gb-500gb",
        "192.168.1.22",
        "m102",
    ] {
        let named = all_named(&book, &[], word);
        assert_eq!(named.len(), 1, "{word}");
        assert_eq!(named[0].host, "m102-macbook-M4-Pro-24GB-500GB.local");
    }
    for short in ["MacbookAir", "m27"] {
        assert!(all_named(&book, &[], short).is_empty(), "{short}");
    }
    // A node and a hand-named entry at its place are one peer.
    let hand = Peer {
        addr: Some(IpAddr::from([192, 168, 1, 22])),
        ..manual("m102-macbook-M4-Pro-24GB-500GB.local", 7100)
    };
    assert_eq!(all_short_named(&book, &[hand], "m102").len(), 1);
}

#[test]
fn host_resolution_order_ranks_tiers_resolves_m22_and_lists_ties() {
    let m22 = joined(
        0x22,
        "m22-Macbook-M5-Max-128GB-8TB.local",
        [10, 0, 0, 22],
        7100,
    );
    let mini_m4 = joined(0x44, "Mac-mini-M4.local", [10, 0, 0, 44], 7100);
    let suffixed = joined(0x3a, "m3a.local", [10, 0, 0, 58], 7100);
    let peers = vec![m22.clone(), mini_m4.clone(), suffixed];

    // Tier 3 resolves leading `m\d+` (case-insensitively), without prefix or middle-token leaks.
    assert_eq!(
        leading_machine_label("m22-Macbook-M5-Max-128GB-8TB.local").map(|n| n.to_string()),
        Some("m22".to_owned())
    );
    assert_eq!(leading_machine_label("m022-studio.local"), None, "zero-led");
    assert_eq!(
        leading_machine_label("m1234567.local"),
        None,
        "seven digits"
    );
    assert_eq!(leading_machine_label("Mac-mini-M4.local"), None);
    assert_eq!(leading_machine_label("m3a.local"), None);
    assert_eq!(named(&peers, &[], "m22"), Ok(Some(&m22)));
    assert_eq!(named(&peers, &[], "M22"), Ok(Some(&m22)));
    assert_eq!(named(&peers, &[], "m2"), Ok(None));
    assert_eq!(named(&peers, &[], "m4"), Ok(None));
    assert_eq!(named(&peers, &[], "m3"), Ok(None));

    // Full 7-tier precedence: 1 Label > 2 Alias > 3 LeadingMachine > 4 VaultName > 5 FirstDnsLabel > 6 IpAddress > 7 NodeId.
    let canonical = joined(0x01, "workstation-alpha.local", [10, 0, 1, 1], 7100);
    let aliased = joined(0x02, "workstation-beta.local", [10, 0, 1, 2], 7100);
    let host_m3 = joined(0x03, "m3-newcomer.local", [10, 0, 1, 3], 7100);
    let vault_named = joined(0x04, "studio-desk.local", [10, 0, 1, 4], 7100);
    let dns_named = joined(0x05, "andrews-3.local", [10, 0, 1, 5], 7100);
    let node_hex = Peer {
        node: Some("deadbeef000000000000000000000007".to_owned()),
        ..joined(0x07, "builder.local", [10, 0, 1, 7], 7100)
    };
    let dns_deadbeef = joined(0x06, "deadbeef.local", [10, 0, 1, 6], 7100);
    let identities = vec![
        PeerIdentity {
            node: canonical.node.clone(),
            host: None,
            label: Some("m3".to_owned()),
            aliases: vec!["m71".to_owned()],
            vault_name: None,
        },
        PeerIdentity {
            node: aliased.node.clone(),
            host: None,
            label: Some("m100".to_owned()),
            aliases: vec!["m32".to_owned(), "m3".to_owned()],
            vault_name: None,
        },
        PeerIdentity {
            node: vault_named.node.clone(),
            host: None,
            label: Some("m104".to_owned()),
            aliases: Vec::new(),
            vault_name: Some("andrews-3".to_owned()),
        },
    ];
    let fleet = vec![
        canonical.clone(),
        aliased.clone(),
        host_m3.clone(),
        vault_named.clone(),
        dns_named.clone(),
        dns_deadbeef.clone(),
        node_hex.clone(),
    ];

    // 1 Label ("m3" on canonical) beats 2 Alias ("m3" on aliased) and 3 LeadingMachine ("m3-newcomer").
    assert_eq!(
        named_with(&fleet, &[], &identities, "m3"),
        Ok(Some(&canonical))
    );
    // Without canonical in the slice, 2 Alias ("m3" on aliased) beats 3 LeadingMachine ("m3-newcomer").
    assert_eq!(
        named_with(&fleet[1..], &[], &identities, "m3"),
        Ok(Some(&aliased))
    );
    // Without canonical and aliased, 3 LeadingMachine resolves "m3-newcomer".
    assert_eq!(
        named_with(&fleet[2..], &[], &identities, "m3"),
        Ok(Some(&host_m3))
    );
    // 4 VaultName ("andrews-3" on vault_named) beats 5 FirstDnsLabel ("andrews-3.local" on dns_named).
    assert_eq!(
        named_with(&fleet, &[], &identities, "andrews-3"),
        Ok(Some(&vault_named))
    );
    // Without vault_named, 5 FirstDnsLabel resolves dns_named.
    let without_vault: Vec<Peer> = fleet
        .iter()
        .filter(|peer| peer.node != vault_named.node)
        .cloned()
        .collect();
    assert_eq!(
        named_with(&without_vault, &[], &identities, "andrews-3"),
        Ok(Some(&dns_named))
    );
    // 5 FirstDnsLabel ("deadbeef.local") beats 7 NodeId prefix ("deadbeef0000..."), while full node id resolves node_hex.
    assert_eq!(
        named_with(&fleet, &[], &identities, "deadbeef"),
        Ok(Some(&dns_deadbeef))
    );
    assert_eq!(
        named_with(&fleet, &[], &identities, "deadbeef000000000000000000000007"),
        Ok(Some(&node_hex))
    );
    // 6 IpAddress resolves exact IP and refuses partial first-octet ("10") as non-DNS.
    assert_eq!(
        named_with(&fleet, &[], &identities, "10.0.1.7"),
        Ok(Some(&node_hex))
    );
    let ip_literal_peer = joined(0x08, "10.0.1.8", [10, 0, 1, 8], 7100);
    assert_eq!(
        named(std::slice::from_ref(&ip_literal_peer), &[], "10"),
        Ok(None)
    );

    // Ambiguity at the winning tier lists both matching peers with their node ids and refuses.
    let m22_dup = joined(0x23, "m22-Studio-Ultra.local", [10, 0, 0, 23], 7100);
    let err = named(&[m22.clone(), m22_dup.clone()], &[], "m22").unwrap_err();
    assert!(
        err.contains("names 2 peers")
            && err.contains(m22.node.as_deref().unwrap())
            && err.contains(m22_dup.node.as_deref().unwrap()),
        "ambiguous --host must list both node ids: {err}"
    );
    assert_eq!(
        find(&[m22.clone(), m22_dup], "m22"),
        Err(err),
        "find and named must agree on ambiguity refusal"
    );

    // remove never deletes a lower-tier NodeId peer in `book` when `others` holds the higher-tier match.
    let mut own_book = vec![node_hex.clone()];
    let copied_dns = vec![copy(dns_deadbeef)];
    let rm_err = remove(&mut own_book, &copied_dns, "deadbeef").unwrap_err();
    assert!(
        rm_err.contains("in the fleet issuer's book"),
        "expected IssuerBook refusal, got: {rm_err}"
    );
    assert_eq!(
        own_book,
        vec![node_hex],
        "lower-tier NodeId entry in book must stay untouched"
    );
}

#[test]
fn c19_silent_fifth_member_at_one_address_evicts_oldest_silent_while_live_refuses() {
    let base_ms: u64 = 1_800_000_000_000;
    let at_ms = |n: u8, port: u16, seen_ms: u64| Peer {
        seen_ms: Some(seen_ms),
        ..joined(n, &format!("m{n}"), [10, 0, 0, 1], port)
    };

    // 4 silent entries (all > LIVE_WITHIN_MS older than newcomer_ms), with node 0x02 oldest.
    let newcomer_ms = base_ms + LIVE_WITHIN_MS + 60_000;
    let mut book = vec![
        at_ms(0x01, 7100, base_ms - 10_000),
        at_ms(0x02, 7101, base_ms - 50_000),
        at_ms(0x03, 7102, base_ms - 20_000),
        at_ms(0x04, 7103, base_ms - 5_000),
    ];
    let newcomer = at_ms(0x05, 7100, newcomer_ms);
    assert_eq!(record(&mut book, newcomer.clone()), Ok(Recorded::New));
    assert_eq!(book.len(), JOINED_PER_ADDRESS);
    assert!(
        !book.iter().any(|p| p.node == at_ms(0x02, 7101, 0).node),
        "oldest silent entry (0x02) must be replaced"
    );
    assert!(book.contains(&newcomer));

    // Negative control: when all 4 entries at 10.0.0.1 are live (within LIVE_WITHIN_MS),
    // the 5th node is refused and nothing is evicted.
    let mut all_live = vec![
        at_ms(0x11, 7100, newcomer_ms - LIVE_WITHIN_MS),
        at_ms(0x12, 7101, newcomer_ms - 3_000),
        at_ms(0x13, 7102, newcomer_ms - 2_000),
        at_ms(0x14, 7103, newcomer_ms - 1_000),
    ];
    let before = all_live.clone();
    let refused = record(&mut all_live, at_ms(0x15, 7104, newcomer_ms));
    assert!(
        refused.as_ref().is_err_and(|why| why.contains("10.0.0.1")),
        "4 live entries must refuse a 5th node: {refused:?}"
    );
    assert_eq!(all_live, before);
}

#[test]
fn c19_dormant_at_14_days_and_expiry_at_90_days() {
    let base_ms: u64 = 1_800_000_000_000;
    let peer = Peer {
        seen_ms: Some(base_ms),
        ..joined(0x21, "m21", [10, 0, 0, 21], 7100)
    };
    let manual_peer = Peer {
        seen_ms: Some(base_ms),
        ..manual("10.0.0.99", 7100)
    };

    // Negative controls just below 14d and 90d boundaries.
    assert!(!peer.is_dormant_at(base_ms + DORMANT_AFTER_MS - 1));
    assert!(peer.is_dormant_at(base_ms + DORMANT_AFTER_MS));
    assert!(
        !manual_peer.is_dormant_at(base_ms + EXPIRE_AFTER_MS * 2),
        "manual entries never become dormant or expire"
    );
    assert!(!peer.is_expired_at(base_ms + EXPIRE_AFTER_MS - 1));
    assert!(peer.is_expired_at(base_ms + EXPIRE_AFTER_MS));
    assert!(!manual_peer.is_expired_at(base_ms + EXPIRE_AFTER_MS * 2));

    let fresh = Peer {
        seen_ms: Some(base_ms + EXPIRE_AFTER_MS - 1),
        ..joined(0x22, "m22", [10, 0, 0, 22], 7100)
    };
    let mut book = vec![peer.clone(), fresh.clone(), manual_peer.clone()];
    assert!(prune_book_at(
        &mut book,
        Some(base_ms + EXPIRE_AFTER_MS),
        &[]
    ));
    assert_eq!(book, vec![fresh, manual_peer]);
}

/// The handshake's gate under a log read refuses the keys it took out; under a log this Mac
/// cannot read now it refuses every proved dialer, as with no deny list read.
#[test]
fn an_unread_log_refuses_every_proved_dialer_at_the_handshake() {
    let proof = |seed: u8| crate::fleet::NodeProof {
        node: node_of(seed),
        key: identity(&node_of(seed), seed).key(),
    };
    let (member, out) = (proof(0x51), proof(0x52));
    let read: MemberKeys = [(out.key, Standing::Out("removed".to_owned()))]
        .into_iter()
        .collect();
    let gate = crate::fleet::Denied::under(Some(&[]), Some(&read));
    assert!(!gate.unread && !gate.refuses(Some(&member)) && gate.refuses(Some(&out)));
    assert!(crate::fleet::Denied::under(Some(&[]), None).is_empty());
    let unread = MemberKeys::unreadable("this Mac's membership log cannot be read now");
    assert!(
        unread.holds_out(&member.key) && !unread.admits(&member.key) && unread.out().is_empty()
    );
    for gate in [
        crate::fleet::Denied::under(Some(&[]), Some(&unread)),
        crate::fleet::Denied::under(None, Some(&read)),
        crate::fleet::Denied::under(None, None),
    ] {
        assert!(gate.unread && gate.refuses(Some(&member)), "{gate:?}");
    }
}

/// A listener that has not read its deny list yet refuses every dialer that proved the fleet
/// secret, a node proof or none, naming [`FleetError::DenyListUnread`] in its own refusal and
/// answering `NO capacity` on the wire: the dialer reads it as busy, to dial again later, which
/// disowns no one. Once its deny list is read, the same dialer is admitted.
///
/// NEGATIVE CONTROLS (2026-11-01): with `listener_proves` not checking `unread` and
/// `Denied::refuses` and `Denied::is_empty` ignoring it (the shape before), the first dial is
/// admitted (`a listener with no deny list read must refuse a proven peer`); with the listener
/// answering `NO unread` on the wire, which a dialer from this build reads but an older one takes
/// as a refused proof, the dial fails with that (`expected a busy listener to dial again, got:
/// Handshake(127.0.0.1:<port>, DenyListUnread)`).
///
/// The listener answers the dialer before it reports the refusal, so the test waits for each
/// report (a hang detector) instead of reading whatever has been reported when the dial returns.
/// NEGATIVE CONTROL (2026-11-05): with the event callback sleeping 300 ms before it records a
/// refusal, a read of the reports when the dials return finds none (`the listener names why it
/// refused: []`, every run), and this test passes; with the unread listener refusing as
/// [`FleetError::AddressBusy`], it fails (`the listener names why it refused (report 1 of 2)`).
#[test]
fn a_listener_that_has_not_read_its_deny_list_refuses_every_proven_peer_until_it_has() {
    let secret = FleetSecret::new(&[0x43; 32]).unwrap();
    let listener_id = identity(&node_of(0x11), 0x11);
    let peer_id = identity(&node_of(0x31), 0x31);
    let read = Arc::new(AtomicBool::new(false));
    let (refused, refusals) = std::sync::mpsc::channel();
    let tcp_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = tcp_listener.local_addr().unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let listening = Listening {
        secret: secret.clone(),
        scope: FleetScope::Content,
        node: Some(listener_id),
        serves: EVERY_OP,
        slots: 8,
        per_source: 8,
        lifetime: HANG,
    };
    let server = std::thread::spawn({
        let running = Arc::clone(&running);
        let read = Arc::clone(&read);
        move || {
            crate::fleet::serve_offering_with_ask_within(
                &tcp_listener,
                (
                    &listening,
                    Arc::new(|| (FleetScope::Content, EVERY_OP)),
                    Arc::new(move || {
                        Arc::new(if read.load(Ordering::Acquire) {
                            crate::fleet::Denied::default()
                        } else {
                            crate::fleet::Denied::not_yet_read()
                        })
                    }),
                ),
                None,
                &running,
                HANG,
                |served, _| served.answer(|s| s.write_all(b"OK 0\n")),
                move |event| {
                    if let FleetEvent::Refused { reason, .. } = event {
                        let _ = refused.send(reason);
                    }
                },
            );
        }
    });
    let dial_as = |me: Option<&NodeIdentity>| {
        dial(
            "127.0.0.1",
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            addr.port(),
            (&secret, me),
            FleetOp::Survey,
            HANG,
        )
    };

    for me in [Some(&peer_id), None] {
        let Err(err) = dial_as(me) else {
            panic!("a listener with no deny list read must refuse a proven peer");
        };
        assert!(
            matches!(err, Unreached::Handshake(_, FleetError::AddressBusy)) && !err.disowns(),
            "expected a busy listener to dial again, got: {err:?}"
        );
    }
    let unread = FleetError::DenyListUnread.to_string();
    // Each dial is refused on its own thread, so the two reports may arrive in either order.
    for n in 1..=2 {
        let reason = refusals
            .recv_timeout(HANG)
            .unwrap_or_else(|e| panic!("the listener reports refusal {n} of 2: {e}"));
        assert_eq!(
            reason, unread,
            "the listener names why it refused (report {n} of 2)"
        );
    }

    read.store(true, Ordering::Release);
    if let Err(err) = dial_as(Some(&peer_id)) {
        panic!("once its deny list is read, the peer must be admitted: {err:?}");
    }
    assert!(
        refusals.try_recv().is_err(),
        "the listener refused only the two dials before its deny list was read"
    );

    running.store(false, Ordering::Release);
    let _ = TcpStream::connect(addr);
    let _ = server.join();
}

#[test]
fn c19_denied_node_and_unproved_peer_refused_with_valid_fleet_secret() {
    let secret = FleetSecret::new(&[0x42; 32]).unwrap();
    let listener_id = identity(&node_of(0x10), 0x10);
    let revoked_id = identity(&node_of(0x20), 0x20);
    let good_id = identity(&node_of(0x30), 0x30);
    let removed_id = identity(&node_of(0x40), 0x40);
    let deny = Arc::new(crate::fleet::Denied {
        nodes: vec![revoked_id.node().to_owned()],
        keys: vec![removed_id.key()],
        ..crate::fleet::Denied::default()
    });

    let tcp_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = tcp_listener.local_addr().unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let listening = Listening {
        secret: secret.clone(),
        scope: FleetScope::Content,
        node: Some(listener_id),
        serves: EVERY_OP,
        slots: 8,
        per_source: 8,
        lifetime: HANG,
    };
    let server = std::thread::spawn({
        let running = Arc::clone(&running);
        let deny = Arc::clone(&deny);
        move || {
            crate::fleet::serve_offering_with_ask_within(
                &tcp_listener,
                (
                    &listening,
                    Arc::new(|| (FleetScope::Content, EVERY_OP)),
                    Arc::new(move || Arc::clone(&deny)),
                ),
                None,
                &running,
                HANG,
                |served, _| served.answer(|s| s.write_all(b"OK 0\n")),
                |_| {},
            );
        }
    });

    // 1. Revoked node holding the valid fleet secret is refused with DeniedNode, which disowns.
    let Err(err) = dial(
        "127.0.0.1",
        Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        addr.port(),
        (&secret, Some(&revoked_id)),
        FleetOp::Survey,
        HANG,
    ) else {
        panic!("revoked node must be refused");
    };
    assert!(
        matches!(err, Unreached::Handshake(_, FleetError::DeniedNode)) && err.disowns(),
        "expected DeniedNode disowning peer, got: {err:?}"
    );

    // 1b. A node whose key the membership log took out is refused the same way, whatever node id
    //     it claims.
    for claimed in [removed_id.node(), good_id.node()] {
        let claimant = NodeIdentity::from_seed(claimed, &[0x40; 32]).unwrap();
        let Err(err) = dial(
            "127.0.0.1",
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            addr.port(),
            (&secret, Some(&claimant)),
            FleetOp::Survey,
            HANG,
        ) else {
            panic!("a removed key must be refused");
        };
        assert!(
            matches!(err, Unreached::Handshake(_, FleetError::DeniedNode)) && err.disowns(),
            "expected DeniedNode for a removed key, got: {err:?}"
        );
    }

    // 2. Unproved peer (`None`) holding the valid fleet secret is ALSO refused when deny-list is
    //    non-empty, so deleting `fleet-node-key` cannot bypass revocation.
    let Err(err_unproved) = dial(
        "127.0.0.1",
        Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        addr.port(),
        (&secret, None),
        FleetOp::Survey,
        HANG,
    ) else {
        panic!("keyless peer must not bypass non-empty deny-list");
    };
    assert!(
        matches!(
            err_unproved,
            Unreached::Handshake(_, FleetError::DeniedNode)
        ),
        "expected DeniedNode for unproved peer, got: {err_unproved:?}"
    );

    // 3. Negative control: unrevoked node holding the same fleet secret succeeds.
    if let Err(err) = dial(
        "127.0.0.1",
        Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        addr.port(),
        (&secret, Some(&good_id)),
        FleetOp::Survey,
        HANG,
    ) {
        panic!("good node must be admitted: {err:?}");
    }

    running.store(false, Ordering::Release);
    let _ = TcpStream::connect(addr);
    let _ = server.join();
}

/// The member this Mac joined through, while it serves no entry it signed (an aterm before
/// 0.147), is asked past a round's three answers though no dial of it missed: its book is its
/// members' only view of the fleet, and this Mac's entry there goes stale with no registration
/// of its own.
/// NEGATIVE CONTROL: past the three, pass over every member not marked missed, the issuer too:
/// the round answered A, C and D alone ("past them the issuer whose entry is unsigned").
#[test]
fn an_issuer_that_signs_no_entry_is_heartbeat_past_the_three() {
    let secret_bytes = [0x56; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let [a, b, c, d, e] = [0x01, 0x02, 0x03, 0x04, 0x05].map(|seed| identity(&node_of(seed), seed));
    let log: MemberKeys = [&a, &b, &c, &d, &e]
        .map(|who| (who.key(), Standing::Member))
        .into_iter()
        .collect();
    let b_state = State::new("hb3-b");
    keep_identity(b_state.dir(), &b, 0x02);
    write_key(b_state.dir(), &secret_bytes).unwrap();
    let serving: Vec<(Issuing, State)> =
        [(&a, "hb3-a"), (&c, "hb3-c"), (&d, "hb3-d"), (&e, "hb3-e")]
            .into_iter()
            .map(|(who, tag)| {
                let state = State::new(tag);
                write_key(state.dir(), &secret_bytes).unwrap();
                let at = Issuing::member(secret.clone(), state.dir(), who.clone(), log.clone());
                (at, state)
            })
            .collect();
    // The issuer's entry carries no signature, as one an aterm before 0.147 answered with.
    let entry = |who: &NodeIdentity, port: u16, via: Via| Peer {
        node: Some(who.node().to_owned()),
        host: "127.0.0.1".to_owned(),
        via,
        seen_ms: Some(1_000),
        ..keyed(0, who.key(), [127, 0, 0, 1], port)
    };
    write_book(
        b_state.dir(),
        &[
            entry(&e, serving[3].0.port, Via::Issuer),
            entry(&a, serving[0].0.port, Via::Joined),
            entry(&c, serving[1].0.port, Via::Joined),
            entry(&d, serving[2].0.port, Via::Joined),
        ],
    );
    // A answered last, and the issuer since C and D, so the round's three are A, C and D, and
    // the issuer, not marked missed, comes behind them.
    hold_book(b_state.dir())
        .unwrap()
        .note_answered(&[
            (format!("node={}", a.node()), 7_000),
            (format!("node={}", c.node()), 5_000),
            (format!("node={}", d.node()), 5_000),
            (format!("node={}", e.node()), 6_000),
        ])
        .unwrap();
    let reg = Registration {
        node: b.node().to_owned(),
        host: "m12.local".to_owned(),
        port: 7199,
        version: "0.146.0".to_owned(),
        key: None,
        signed: None,
    };

    let answered = heartbeat_members(
        b_state.dir(),
        (&secret, &b),
        &reg,
        HANG,
        (Some(&log), &Vec::new),
    )
    .expect("the book reads");
    let mut nodes: Vec<&str> = answered.iter().map(|done| done.node.as_str()).collect();
    nodes.sort_unstable();
    assert_eq!(
        nodes,
        [a.node(), c.node(), d.node(), e.node()],
        "the three members, and past them the issuer whose entry is unsigned"
    );
    assert!(
        read_peers(serving[3].1.dir())
            .unwrap()
            .iter()
            .any(|peer| peer.node.as_deref() == Some(b.node())),
        "the issuer recorded the heartbeat"
    );
}

/// Heartbeats go to the members that answer, the issuer dark or not; and this Mac's network place
/// hears each member that proved its key, where it answered, every round with how many answered,
/// and the book's admitted members (`note_place`), so the place reads the ALab LAN from proven
/// answers only. Each answer a round under the membership log heard is marked logged, and a
/// round with no log (an isolated job's, which reads no vault and denies no one) marks its
/// answers unlogged, so the vault's admission never counts them.
/// NEGATIVE CONTROLS: dropping the `note_place` call leaves the place's evidence empty, and this
/// test fails at "the place heard each member that proved its key"; marking every round logged
/// (`log.is_some()` → `true` at the `note_place` call) fails at "a round with no log notes its
/// answers unlogged".
#[test]
fn heartbeats_go_to_the_members_that_answer_with_the_issuer_dark() {
    let secret_bytes = [0x55; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let held = |state: &State| {
        write_key(state.dir(), &secret_bytes).unwrap();
    };
    let (a_state, b_state, c_state) = (State::new("hb-a"), State::new("hb-b"), State::new("hb-c"));
    for state in [&a_state, &b_state, &c_state] {
        held(state);
    }
    let a = identity(&node_of(0x01), 0x01);
    let b = identity(&node_of(0x02), 0x02);
    let c = identity(&node_of(0x03), 0x03);
    let stranger = identity(&node_of(0x04), 0x04);
    keep_identity(b_state.dir(), &b, 0x02);
    let log: MemberKeys = [&a, &b, &c]
        .map(|who| (who.key(), Standing::Member))
        .into_iter()
        .chain([(stranger.key(), Standing::Out("removed".to_owned()))])
        .collect();
    let a_at = Issuing::member(secret.clone(), a_state.dir(), a.clone(), log.clone());
    let c_at = Issuing::member(secret.clone(), c_state.dir(), c.clone(), log.clone());
    let stranger_state = State::new("hb-stranger");
    held(&stranger_state);
    let stranger_at = Issuing::member(
        secret.clone(),
        stranger_state.dir(),
        stranger.clone(),
        log.clone(),
    );
    let at = |who: &NodeIdentity, port: u16, via: Via| Peer {
        via,
        seen_ms: Some(1_000),
        ..keyed(0, who.key(), [127, 0, 0, 1], port)
    };
    let entry = |who: &NodeIdentity, port: u16, via: Via| Peer {
        node: Some(who.node().to_owned()),
        host: "127.0.0.1".to_owned(),
        ..at(who, port, via)
    };
    write_book(
        b_state.dir(),
        &[
            entry(&a, a_at.port, Via::Issuer),
            entry(&c, c_at.port, Via::Joined),
            entry(&stranger, stranger_at.port, Via::Joined),
        ],
    );
    let reg = Registration {
        node: b.node().to_owned(),
        host: "m12.local".to_owned(),
        port: 7199,
        version: "0.146.0".to_owned(),
        key: None,
        signed: None,
    };

    // A log this Mac cannot read now admits no member to dial and judges no entry of its book.
    let book_before = read_peers(b_state.dir()).unwrap();
    let unread = MemberKeys::unreadable("this Mac's membership log cannot be read now");
    assert!(
        heartbeat_members(
            b_state.dir(),
            (&secret, &b),
            &reg,
            HANG,
            (Some(&unread), &Vec::new),
        )
        .expect("the book reads")
        .is_empty(),
        "an unread log dials no member"
    );
    assert_eq!(
        read_peers(b_state.dir()).unwrap(),
        book_before,
        "an unread log takes no entry out of the book"
    );
    assert!(
        read_peers(a_state.dir()).unwrap().is_empty() && read_answered(b_state.dir()).is_empty(),
        "no member heard a heartbeat"
    );

    let answered = heartbeat_members(
        b_state.dir(),
        (&secret, &b),
        &reg,
        HANG,
        (Some(&log), &Vec::new),
    )
    .expect("the book reads");
    let mut nodes: Vec<&str> = answered.iter().map(|done| done.node.as_str()).collect();
    nodes.sort_unstable();
    assert_eq!(
        nodes,
        [a.node(), c.node()],
        "both members the log admits, never the key it holds out"
    );
    for state in [&a_state, &c_state] {
        let book = read_peers(state.dir()).unwrap();
        let held = book
            .iter()
            .find(|peer| peer.node.as_deref() == Some(b.node()))
            .expect("the member recorded the heartbeat");
        assert!(held.signature_holds(), "{held:?}");
        assert_eq!((held.host.as_str(), held.port), ("m12.local", 7199));
    }
    assert!(
        read_peers(stranger_state.dir()).unwrap().is_empty(),
        "no heartbeat to a key the log holds out"
    );
    // Each answer is noted where `aterm fleet peers` reads it (F4's follow-up), and a
    // registration a member took is no answer to it.
    let stamped = read_answered(b_state.dir());
    assert!(
        stamped.contains_key(&format!("node={}", a.node()))
            && stamped.contains_key(&format!("node={}", c.node())),
        "{stamped:?}"
    );
    assert!(
        read_answered(a_state.dir()).is_empty(),
        "{:?}",
        read_answered(a_state.dir())
    );
    let place_dir = aterm_netplace::record::dir(b_state.dir());
    let heard = aterm_netplace::heard::read(&place_dir);
    let mut heard_members: Vec<String> = heard
        .answers
        .iter()
        .map(|answer| answer.member.clone())
        .collect();
    heard_members.sort_unstable();
    heard_members.dedup();
    let mut proven =
        [&a, &c].map(|who| aterm_netplace::member_key(aterm_netplace::Lane::Fleet, who.node()));
    proven.sort_unstable();
    assert_eq!(
        heard_members, proven,
        "the place heard each member that proved its key, never the one the log holds out"
    );
    assert!(
        heard
            .answers
            .iter()
            .all(|answer| answer.addr == Some(IpAddr::from([127, 0, 0, 1]))),
        "{heard:?}"
    );
    assert!(
        heard.answers.iter().all(|answer| answer.logged),
        "a round under the log notes its answers logged: {heard:?}"
    );
    assert_eq!(
        heard
            .rounds
            .iter()
            .map(|round| round.answered)
            .collect::<Vec<_>>(),
        [2],
        "the round, with how many answered"
    );
    assert_eq!(
        aterm_netplace::heard::read_book(&place_dir).map(|book| book.members.len()),
        Some(2),
        "the book's members the log admits"
    );
    let b_book = read_peers(b_state.dir()).unwrap();
    let a_entry = b_book
        .iter()
        .find(|peer| peer.node.as_deref() == Some(a.node()))
        .unwrap();
    assert_eq!(
        (a_entry.via, a_entry.version.as_deref()),
        (Via::Issuer, Some("0.146.0")),
        "the member it joined through stays that, brought up to date"
    );
    assert!(
        b_book
            .iter()
            .any(|peer| peer.node.as_deref() == Some(c.node()) && peer.signature_holds()),
        "C's own signed entry, merged from its book"
    );

    // The same Mac's round run as an isolated job (its own state, no log): the members its book
    // binds answer, and the place hears them, never as evidence for the vault.
    let isolated = State::new("hb-b-isolated");
    held(&isolated);
    keep_identity(isolated.dir(), &b, 0x02);
    write_book(
        isolated.dir(),
        &[
            entry(&a, a_at.port, Via::Issuer),
            entry(&c, c_at.port, Via::Joined),
        ],
    );
    let answered = heartbeat_members(isolated.dir(), (&secret, &b), &reg, HANG, (None, &Vec::new))
        .expect("the book reads");
    assert!(
        !answered.is_empty(),
        "PRECONDITION: the isolated round reached a member"
    );
    let unlogged = aterm_netplace::heard::read(&aterm_netplace::record::dir(isolated.dir()));
    assert!(
        !unlogged.answers.is_empty() && unlogged.answers.iter().all(|answer| !answer.logged),
        "a round with no log notes its answers unlogged: {unlogged:?}"
    );

    // The issuer goes dark: the round goes to the member that answers.
    drop(a_at);
    let answered = heartbeat_members(
        b_state.dir(),
        (&secret, &b),
        &reg,
        HANG,
        (Some(&log), &Vec::new),
    )
    .expect("the book reads");
    assert_eq!(
        answered
            .iter()
            .map(|done| done.node.as_str())
            .collect::<Vec<_>>(),
        [c.node()]
    );
    drop(c_at);
    assert!(
        heartbeat_members(
            b_state.dir(),
            (&secret, &b),
            &reg,
            HANG,
            (Some(&log), &Vec::new)
        )
        .unwrap()
        .is_empty(),
        "every member dark: none answered"
    );
    assert!(
        read_missed(b_state.dir()).contains_key(&format!("node={}", c.node())),
        "the dial that missed is noted: {:?}",
        read_missed(b_state.dir())
    );
    assert!(
        aterm_netplace::heard::read(&place_dir)
            .rounds
            .iter()
            .any(|round| round.answered == 0),
        "a round no member answered is heard too"
    );
}

/// AWAY, A HEARTBEAT ROUND PASSES OVER THE MEMBERS BEHIND THE ALAB LAN. On a phone hotspot (the
/// place's facts scripted), the first round on the network dials every member, the one whose
/// route leaves through the hotspot toward the home LAN too; the next round dials the member it
/// can reach and passes over that one, which notes no miss of it.
/// NEGATIVE CONTROLS: dialling every member whatever the pacing says (`Pace::dials` → `true`)
/// dials the member behind the ALab LAN in the paced round, and "the paced round passes it over"
/// fails; never dialling a full round (`Round::dials` without `self.full ||`) passes it over in the
/// first round too, and "the first round on a network dials every member" fails.
#[cfg(unix)]
#[test]
fn away_a_heartbeat_round_passes_over_the_members_behind_the_alab_lan() {
    let secret_bytes = [0x5a; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let (a_state, b_state) = (State::new("hb-away-a"), State::new("hb-away-b"));
    for state in [&a_state, &b_state] {
        write_key(state.dir(), &secret_bytes).unwrap();
    }
    let a = identity(&node_of(0x21), 0x21);
    let b = identity(&node_of(0x22), 0x22);
    let behind = identity(&node_of(0x23), 0x23);
    keep_identity(b_state.dir(), &b, 0x22);
    let log: MemberKeys = [&a, &b, &behind]
        .map(|who| (who.key(), Standing::Member))
        .into_iter()
        .collect();
    let a_at = Issuing::member(secret.clone(), a_state.dir(), a.clone(), log.clone());
    // `0.0.0.1` names no host (a dial of it fails at once); the script routes it out through
    // the hotspot's own interface, the way the home LAN's addresses leave a hotspot. Each entry
    // was seen now, so no round prunes it as expired.
    let seen = unix_ms_now();
    let entry = |who: &NodeIdentity, ip: [u8; 4], port: u16| Peer {
        node: Some(who.node().to_owned()),
        host: std::net::Ipv4Addr::from(ip).to_string(),
        via: Via::Joined,
        seen_ms: seen,
        ..keyed(0, who.key(), ip, port)
    };
    write_book(
        b_state.dir(),
        &[
            entry(&a, [127, 0, 0, 1], a_at.port),
            entry(&behind, [0, 0, 0, 1], 7199),
        ],
    );
    let reg = Registration {
        node: b.node().to_owned(),
        host: "m12.local".to_owned(),
        port: 7199,
        version: "0.148.0".to_owned(),
        key: None,
        signed: None,
    };
    let script = aterm_netplace::facts::parse_script(
        "route en0 172.20.10.2/28 via 172.20.10.1 hw 02:00:00:00:00:01\n\
         path usable expensive wifi\n\
         via 0.0.0.1 172.20.10.2\n",
    );
    let behind_key = format!("node={}", behind.node());
    aterm_netplace::facts::script_here(script, || {
        let round = || {
            heartbeat_members(
                b_state.dir(),
                (&secret, &b),
                &reg,
                HANG,
                (Some(&log), &Vec::new),
            )
            .expect("the book reads")
        };
        let answered = round();
        assert_eq!(
            answered
                .iter()
                .map(|done| done.node.as_str())
                .collect::<Vec<_>>(),
            [a.node()]
        );
        assert!(
            read_missed(b_state.dir()).contains_key(&behind_key),
            "the first round on a network dials every member: {:?}",
            read_missed(b_state.dir())
        );
        assert!(
            read_peers(b_state.dir())
                .unwrap()
                .iter()
                .any(|peer| peer.node.as_deref() == Some(behind.node())),
            "the member behind the ALab LAN stays in the book"
        );
        std::fs::remove_file(b_state.dir().join(ANSWERED_FILE)).expect("the stamps");
        let answered = round();
        assert_eq!(
            answered
                .iter()
                .map(|done| done.node.as_str())
                .collect::<Vec<_>>(),
            [a.node()],
            "the member this network reaches"
        );
        assert!(
            !read_missed(b_state.dir()).contains_key(&behind_key),
            "the paced round passes it over: {:?}",
            read_missed(b_state.dir())
        );
    });
}

/// A heartbeat round reads, beside the member that answered last, the members it asked longest ago
/// (never, first), so no Mac's rounds read the same three books forever and every member in turn
/// takes its registration (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.1).
/// NEGATIVE CONTROL: order the round by when each member last answered, newest first (F17 as
/// landed): C4, which answered never, is never asked, and its book never holds B.
#[test]
fn a_heartbeat_round_asks_a_member_it_never_asked_before_ones_it_asked_lately() {
    let secret_bytes = [0x59; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let [b, c1, c2, c3, c4] =
        [0x41, 0x42, 0x43, 0x44, 0x45].map(|seed| identity(&node_of(seed), seed));
    let log: MemberKeys = [&b, &c1, &c2, &c3, &c4]
        .map(|who| (who.key(), Standing::Member))
        .into_iter()
        .collect();
    let b_state = State::new("rotate-b");
    keep_identity(b_state.dir(), &b, 0x41);
    write_key(b_state.dir(), &secret_bytes).unwrap();
    let serving: Vec<(Issuing, State)> = [
        (&c1, "rotate-c1"),
        (&c2, "rotate-c2"),
        (&c3, "rotate-c3"),
        (&c4, "rotate-c4"),
    ]
    .into_iter()
    .map(|(who, tag)| {
        let state = State::new(tag);
        write_key(state.dir(), &secret_bytes).unwrap();
        let at = Issuing::member(secret.clone(), state.dir(), who.clone(), log.clone());
        (at, state)
    })
    .collect();
    let entry = |who: &NodeIdentity, port: u16| Peer {
        node: Some(who.node().to_owned()),
        host: "127.0.0.1".to_owned(),
        via: Via::Joined,
        seen_ms: Some(1_000),
        ..keyed(0, who.key(), [127, 0, 0, 1], port)
    };
    write_book(
        b_state.dir(),
        &[
            entry(&c1, serving[0].0.port),
            entry(&c2, serving[1].0.port),
            entry(&c3, serving[2].0.port),
            entry(&c4, serving[3].0.port),
        ],
    );
    // C1, C2 and C3 answered lately; C4 was never asked.
    hold_book(b_state.dir())
        .unwrap()
        .note_answered(
            &[c1.node(), c2.node(), c3.node()].map(|node| (format!("node={node}"), 5_000)),
        )
        .unwrap();
    let reg = Registration {
        node: b.node().to_owned(),
        host: "m12.local".to_owned(),
        port: 7199,
        version: "0.147.0".to_owned(),
        key: None,
        signed: None,
    };

    let answered = heartbeat_members(
        b_state.dir(),
        (&secret, &b),
        &reg,
        HANG,
        (Some(&log), &Vec::new),
    )
    .expect("the book reads");
    assert_eq!(answered.len(), HEARTBEAT_MEMBERS, "{answered:?}");
    assert_eq!(
        answered[0].node,
        c1.node(),
        "the member that answered last leads"
    );
    assert!(
        answered.iter().any(|done| done.node == c4.node()),
        "C4, never asked, is asked before members asked lately: {answered:?}"
    );
    assert!(
        read_peers(serving[3].1.dir())
            .unwrap()
            .iter()
            .any(|peer| peer.node.as_deref() == Some(b.node())),
        "C4 holds B's registration"
    );
}

/// A member a round's dial missed since it last answered waits behind the members that answer,
/// and a dial of it takes no more than its share of the round: a member gone dark (a laptop left)
/// that answered last no longer spends every round's time and its hints, round after round. Past
/// its three answers a round dials such members, the one missed longest ago first, only until one
/// misses again: two gone dark cost a round one dial, by its share.
/// NEGATIVE CONTROL: order the round by when each member last answered, and dial each by the
/// round's deadline (F17 as landed): D's dial carries the whole round, and the next round dials D
/// first again. Go on past the three answers through every member marked missed: the second round
/// dials D after E ("the round stops at its first miss past its three answers").
#[test]
fn a_member_gone_dark_waits_behind_the_members_that_answer_and_spends_only_its_share() {
    let secret_bytes = [0x5a; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let [b, d, c1, c2, c3, e] =
        [0x51, 0x52, 0x53, 0x54, 0x55, 0x56].map(|seed| identity(&node_of(seed), seed));
    let log: MemberKeys = [&b, &d, &c1, &c2, &c3, &e]
        .map(|who| (who.key(), Standing::Member))
        .into_iter()
        .collect();
    let b_state = State::new("dark-b");
    keep_identity(b_state.dir(), &b, 0x51);
    write_key(b_state.dir(), &secret_bytes).unwrap();
    let serving: Vec<(Issuing, State)> = [(&c1, "dark-c1"), (&c2, "dark-c2"), (&c3, "dark-c3")]
        .into_iter()
        .map(|(who, tag)| {
            let state = State::new(tag);
            write_key(state.dir(), &secret_bytes).unwrap();
            let at = Issuing::member(secret.clone(), state.dir(), who.clone(), log.clone());
            (at, state)
        })
        .collect();
    // D takes connections and never answers: a Mac gone to sleep with its port held.
    let d_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let d_at = d_listener.local_addr().unwrap();
    let entry = |who: &NodeIdentity, port: u16| Peer {
        node: Some(who.node().to_owned()),
        host: "127.0.0.1".to_owned(),
        via: Via::Joined,
        seen_ms: Some(1_000),
        ..keyed(0, who.key(), [127, 0, 0, 1], port)
    };
    // D answered last, before it went dark.
    hold_book(b_state.dir())
        .unwrap()
        .note_answered(&[
            (format!("node={}", d.node()), 9_000),
            (format!("node={}", c1.node()), 5_000),
            (format!("node={}", c2.node()), 5_000),
            (format!("node={}", c3.node()), 5_000),
        ])
        .unwrap();
    let reg = Registration {
        node: b.node().to_owned(),
        host: "m12.local".to_owned(),
        port: 7199,
        version: "0.147.0".to_owned(),
        key: None,
        signed: None,
    };
    let round = || {
        heartbeat_members(
            b_state.dir(),
            (&secret, &b),
            &reg,
            HANG,
            (Some(&log), &Vec::new),
        )
        .expect("the book reads")
    };

    // A round that holds D alone dials it, by its share of the round.
    write_book(b_state.dir(), &[entry(&d, d_at.port())]);
    let held = HeldSilent::at(d_at);
    assert!(round().is_empty());
    let share = HANG / u32::try_from(HEARTBEAT_MEMBERS + 1).unwrap();
    let bound = held.overrode().expect("the round dialled D");
    assert!(
        bound <= share,
        "D's dial carried {bound:?}, past its share of the round ({share:?})"
    );
    assert!(
        read_missed(b_state.dir()).contains_key(&format!("node={}", d.node())),
        "{:?}",
        read_missed(b_state.dir())
    );
    drop(held);

    // E, gone dark too, answered after the three and missed a round before D did.
    let e_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let e_at = e_listener.local_addr().unwrap();
    let e_key = format!("node={}", e.node());
    hold_book(b_state.dir())
        .unwrap()
        .note_stamps(&[(e_key.clone(), 8_000)], (&[(e_key, 8_500)], &[]))
        .unwrap();

    // The next round, with members that answer: D and E, which answered last but missed since,
    // wait behind them, and the round fills before either; past its three answers it dials E,
    // missed longest ago, by its share, and stops at that miss.
    write_book(
        b_state.dir(),
        &[
            entry(&d, d_at.port()),
            entry(&e, e_at.port()),
            entry(&c1, serving[0].0.port),
            entry(&c2, serving[1].0.port),
            entry(&c3, serving[2].0.port),
        ],
    );
    let (held, held_e) = (HeldSilent::at(d_at), HeldSilent::at(e_at));
    let answered = round();
    let mut nodes: Vec<&str> = answered.iter().map(|done| done.node.as_str()).collect();
    nodes.sort_unstable();
    assert_eq!(nodes, [c1.node(), c2.node(), c3.node()]);
    let bound = held_e
        .overrode()
        .expect("past its three answers the round dials E, missed longest ago");
    assert!(
        bound <= share,
        "E's dial carried {bound:?}, past its share of the round ({share:?})"
    );
    assert_eq!(
        held.overrode(),
        None,
        "the round stops at its first miss past its three answers, and D, gone dark, was not \
         dialled before the members that answer"
    );
    drop((d_listener, e_listener));
}

/// After a cut heals, a heartbeat round that three members answer goes on through the members
/// marked missed (their dials missed in the cut), those heard from since their last miss first,
/// then the one missed longest ago first, until one of them misses again: each member back
/// answers within a round, and one still dark costs the round one dial past its three answers,
/// by its share, and goes behind the rest of them (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// §4.2, the heal rows of `lab swarm partition`). G's dial missed first and G is still dark; D1's,
/// D2's and D3's missed after it, and they are back, D1 registered here since. The first round
/// reaches D1 and stops at G; the second reaches D2 and D3 and stops at G again.
/// NEGATIVE CONTROLS: the round as it was (it ended at three answers, every member marked missed
/// behind them): the first round answers C1, C2 and C3 alone, and D1 is never asked ("D1, missed
/// in the cut and heard from since, answers the first round"). The members marked missed in miss
/// order alone, heard or not: the first round dials G first and stops there, the same failure.
#[test]
fn after_a_heal_each_member_missed_in_the_cut_answers_within_a_round_while_three_others_answer() {
    let secret_bytes = [0x5b; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let [b, c1, c2, c3, d1, d2, d3, g] =
        [0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68].map(|seed| identity(&node_of(seed), seed));
    let log: MemberKeys = [&b, &c1, &c2, &c3, &d1, &d2, &d3, &g]
        .map(|who| (who.key(), Standing::Member))
        .into_iter()
        .collect();
    let b_state = State::new("heal-b");
    keep_identity(b_state.dir(), &b, 0x61);
    write_key(b_state.dir(), &secret_bytes).unwrap();
    let serving: Vec<(Issuing, State)> = [
        (&c1, "heal-c1"),
        (&c2, "heal-c2"),
        (&c3, "heal-c3"),
        (&d1, "heal-d1"),
        (&d2, "heal-d2"),
        (&d3, "heal-d3"),
    ]
    .into_iter()
    .map(|(who, tag)| {
        let state = State::new(tag);
        write_key(state.dir(), &secret_bytes).unwrap();
        let at = Issuing::member(secret.clone(), state.dir(), who.clone(), log.clone());
        (at, state)
    })
    .collect();
    // G takes connections and never answers: a Mac still on the far side of the cut.
    let g_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let g_at = g_listener.local_addr().unwrap();
    // Every member answered ten minutes ago, before the cut, and was last heard from then; C1, C2
    // and C3 answered in it. The dials of G, D1, D2 and D3 missed in it, in that order, and D1
    // registered here since. Each was heard from within the last ten minutes, so no round prunes
    // an entry as expired.
    let now = unix_ms_now().expect("the clock reads");
    let ago = |ms: u64| now - ms;
    let entry = |who: &NodeIdentity, port: u16, seen: u64| Peer {
        node: Some(who.node().to_owned()),
        host: "127.0.0.1".to_owned(),
        via: Via::Joined,
        seen_ms: Some(seen),
        ..keyed(0, who.key(), [127, 0, 0, 1], port)
    };
    let heard = [
        ago(120_000),
        ago(120_000),
        ago(120_000),
        ago(60_000),
        ago(600_000),
        ago(600_000),
    ];
    let mut book: Vec<Peer> = [&c1, &c2, &c3, &d1, &d2, &d3]
        .into_iter()
        .zip(&serving)
        .zip(heard)
        .map(|((who, (at, _)), seen)| entry(who, at.port, seen))
        .collect();
    book.push(entry(&g, g_at.port(), ago(600_000)));
    write_book(b_state.dir(), &book);
    let key = |who: &NodeIdentity| format!("node={}", who.node());
    hold_book(b_state.dir())
        .unwrap()
        .note_stamps(
            &[
                (key(&d1), ago(600_000)),
                (key(&d2), ago(600_000)),
                (key(&d3), ago(600_000)),
                (key(&g), ago(600_000)),
                (key(&c1), ago(120_000)),
                (key(&c2), ago(120_000)),
                (key(&c3), ago(120_000)),
            ],
            (
                &[
                    (key(&g), ago(300_000)),
                    (key(&d1), ago(240_000)),
                    (key(&d2), ago(180_000)),
                    (key(&d3), ago(150_000)),
                ],
                &[],
            ),
        )
        .unwrap();
    let reg = Registration {
        node: b.node().to_owned(),
        host: "m12.local".to_owned(),
        port: 7199,
        version: "0.148.0".to_owned(),
        key: None,
        signed: None,
    };
    let round = || {
        let mut nodes: Vec<String> = heartbeat_members(
            b_state.dir(),
            (&secret, &b),
            &reg,
            HANG,
            (Some(&log), &Vec::new),
        )
        .expect("the book reads")
        .into_iter()
        .map(|done| done.node)
        .collect();
        nodes.sort_unstable();
        nodes
    };
    let marked = || {
        let (answered, missed) = (read_answered(b_state.dir()), read_missed(b_state.dir()));
        let mut marked: Vec<String> = [&c1, &c2, &c3, &d1, &d2, &d3, &g]
            .into_iter()
            .filter(|who| missed.get(&key(who)) > answered.get(&key(who)))
            .map(|who| who.node().to_owned())
            .collect();
        marked.sort_unstable();
        marked
    };
    let sorted = |nodes: &[&NodeIdentity]| {
        let mut nodes: Vec<String> = nodes.iter().map(|who| who.node().to_owned()).collect();
        nodes.sort_unstable();
        nodes
    };
    let share = HANG / u32::try_from(HEARTBEAT_MEMBERS + 1).unwrap();

    let held = HeldSilent::at(g_at);
    let first = round();
    assert_eq!(
        first,
        sorted(&[&c1, &c2, &c3, &d1]),
        "D1, missed in the cut and heard from since, answers the first round, and the round stops \
         at G"
    );
    let bound = held.overrode().expect("the first round dialled G");
    assert!(
        bound <= share,
        "G's dial carried {bound:?}, past its share ({share:?})"
    );
    assert_eq!(marked(), sorted(&[&d2, &d3, &g]));
    drop(held);

    let held = HeldSilent::at(g_at);
    let second = round();
    for back in [&d2, &d3] {
        assert!(
            second.iter().any(|node| node == back.node()),
            "{} answers the second round, before G, which missed again since: {second:?}",
            back.node()
        );
    }
    assert_eq!(
        second.len(),
        HEARTBEAT_MEMBERS + 2,
        "three members not marked missed, then D2 and D3: {second:?}"
    );
    let bound = held.overrode().expect("the second round dialled G");
    assert!(
        bound <= share,
        "G's dial carried {bound:?}, past its share ({share:?})"
    );
    assert_eq!(marked(), sorted(&[&g]), "G alone is still marked missed");
    for (_, state) in &serving[3..] {
        assert!(
            read_peers(state.dir())
                .unwrap()
                .iter()
                .any(|peer| peer.node.as_deref() == Some(b.node())),
            "each member back holds B's registration"
        );
    }
    drop(g_listener);
}

/// A heal that brings back more members marked missed than a round takes past its three answers
/// costs that round [`HEARTBEAT_ANSWERS_MAX`] answers, and the rest answer the round after: an
/// 86-Mac LAN that a switch split in half does not register, read and push facts with some 43
/// members per Mac in the first round after the heal. D1 to D4 missed in the cut, in that order,
/// and are back; nobody heard from them since. The first round answers C1 to C3, then D1 to D3,
/// missed longest ago, and stops at its sixth answer; the second answers D4.
/// NEGATIVE CONTROL: no cap (the round goes on through the members marked missed until one
/// misses, as `5daeab6c68` landed): the first round answers D4 too ("the first round stops at
/// its sixth answer").
#[test]
fn a_heal_that_brings_back_many_members_costs_a_round_at_most_three_answers_more() {
    let secret_bytes = [0x5c; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let [b, c1, c2, c3, d1, d2, d3, d4] =
        [0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78].map(|seed| identity(&node_of(seed), seed));
    let log: MemberKeys = [&b, &c1, &c2, &c3, &d1, &d2, &d3, &d4]
        .map(|who| (who.key(), Standing::Member))
        .into_iter()
        .collect();
    let b_state = State::new("many-b");
    keep_identity(b_state.dir(), &b, 0x71);
    write_key(b_state.dir(), &secret_bytes).unwrap();
    let members = [&c1, &c2, &c3, &d1, &d2, &d3, &d4];
    let serving: Vec<(Issuing, State)> = members
        .into_iter()
        .enumerate()
        .map(|(at, who)| {
            let state = State::new(&format!("many-{at}"));
            write_key(state.dir(), &secret_bytes).unwrap();
            let at = Issuing::member(secret.clone(), state.dir(), who.clone(), log.clone());
            (at, state)
        })
        .collect();
    // Every member answered ten minutes ago and was last heard from then; C1, C2 and C3 answered
    // in the cut, and the dials of D1, D2, D3 and D4 missed in it, in that order.
    let now = unix_ms_now().expect("the clock reads");
    let ago = |ms: u64| now - ms;
    let book: Vec<Peer> = members
        .into_iter()
        .zip(&serving)
        .map(|(who, (at, _))| Peer {
            node: Some(who.node().to_owned()),
            host: "127.0.0.1".to_owned(),
            via: Via::Joined,
            seen_ms: Some(ago(600_000)),
            ..keyed(0, who.key(), [127, 0, 0, 1], at.port)
        })
        .collect();
    write_book(b_state.dir(), &book);
    let key = |who: &NodeIdentity| format!("node={}", who.node());
    let back = [&d1, &d2, &d3, &d4];
    let answers: Vec<(String, u64)> = back
        .into_iter()
        .map(|who| (key(who), ago(600_000)))
        .chain([&c1, &c2, &c3].map(|who| (key(who), ago(120_000))))
        .collect();
    let misses: Vec<(String, u64)> = back
        .into_iter()
        .zip([300_000, 240_000, 180_000, 150_000])
        .map(|(who, ms)| (key(who), ago(ms)))
        .collect();
    hold_book(b_state.dir())
        .unwrap()
        .note_stamps(&answers, (&misses, &[]))
        .unwrap();
    let reg = Registration {
        node: b.node().to_owned(),
        host: "m12.local".to_owned(),
        port: 7199,
        version: "0.149.0".to_owned(),
        key: None,
        signed: None,
    };
    let round = || {
        let mut nodes: Vec<String> = heartbeat_members(
            b_state.dir(),
            (&secret, &b),
            &reg,
            HANG,
            (Some(&log), &Vec::new),
        )
        .expect("the book reads")
        .into_iter()
        .map(|done| done.node)
        .collect();
        nodes.sort_unstable();
        nodes
    };
    let sorted = |nodes: &[&NodeIdentity]| {
        let mut nodes: Vec<String> = nodes.iter().map(|who| who.node().to_owned()).collect();
        nodes.sort_unstable();
        nodes
    };
    let marked = || {
        let (answered, missed) = (read_answered(b_state.dir()), read_missed(b_state.dir()));
        let mut marked: Vec<String> = members
            .into_iter()
            .filter(|who| missed.get(&key(who)) > answered.get(&key(who)))
            .map(|who| who.node().to_owned())
            .collect();
        marked.sort_unstable();
        marked
    };

    assert_eq!(
        round(),
        sorted(&[&c1, &c2, &c3, &d1, &d2, &d3]),
        "the first round stops at its sixth answer, D1 to D3 missed longest ago"
    );
    assert_eq!(HEARTBEAT_ANSWERS_MAX, 2 * HEARTBEAT_MEMBERS);
    assert_eq!(marked(), sorted(&[&d4]), "D4 was not dialled");
    let second = round();
    assert!(
        second.iter().any(|node| node == d4.node()),
        "D4 answers the second round: {second:?}"
    );
    assert_eq!(marked(), Vec::<String>::new());
}

/// The member this Mac joined through, gone dark while it served no entry it signed (an aterm
/// before 0.147), is a member marked missed like any other: past a round's three answers it costs
/// the round at most the one dial that misses, never a second dial of it or of another member
/// marked missed. I, that issuer, missed first and E after it, both still dark: the first round
/// dials I once and stops, the second dials E and stops.
/// NEGATIVE CONTROL: the issuer asked past the three in a block of its own after the loop, by
/// whether it answered alone (as `b493367cc5` held it): the first round dialled I twice ("the
/// first round dials I once", left `(2, 0)`).
#[test]
fn a_dark_issuer_that_signs_no_entry_costs_a_round_one_dial_past_its_answers() {
    let secret_bytes = [0x5d; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let [b, c1, c2, c3, i, e] =
        [0x81, 0x82, 0x83, 0x84, 0x85, 0x86].map(|seed| identity(&node_of(seed), seed));
    let log: MemberKeys = [&b, &c1, &c2, &c3, &i, &e]
        .map(|who| (who.key(), Standing::Member))
        .into_iter()
        .collect();
    let b_state = State::new("dark-issuer-b");
    keep_identity(b_state.dir(), &b, 0x81);
    write_key(b_state.dir(), &secret_bytes).unwrap();
    let serving: Vec<(Issuing, State)> = [(&c1, "di-c1"), (&c2, "di-c2"), (&c3, "di-c3")]
        .into_iter()
        .map(|(who, tag)| {
            let state = State::new(tag);
            write_key(state.dir(), &secret_bytes).unwrap();
            let at = Issuing::member(secret.clone(), state.dir(), who.clone(), log.clone());
            (at, state)
        })
        .collect();
    // I and E take connections and never answer.
    let i_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let e_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let (i_at, e_at) = (
        i_listener.local_addr().unwrap(),
        e_listener.local_addr().unwrap(),
    );
    let now = unix_ms_now().expect("the clock reads");
    let ago = |ms: u64| now - ms;
    // The issuer's entry carries no signature, as one an aterm before 0.147 answered with.
    let entry = |who: &NodeIdentity, port: u16, via: Via| Peer {
        node: Some(who.node().to_owned()),
        host: "127.0.0.1".to_owned(),
        via,
        seen_ms: Some(ago(600_000)),
        ..keyed(0, who.key(), [127, 0, 0, 1], port)
    };
    let mut book: Vec<Peer> = [&c1, &c2, &c3]
        .into_iter()
        .zip(&serving)
        .map(|(who, (at, _))| entry(who, at.port, Via::Joined))
        .collect();
    book.push(entry(&i, i_at.port(), Via::Issuer));
    book.push(entry(&e, e_at.port(), Via::Joined));
    write_book(b_state.dir(), &book);
    let key = |who: &NodeIdentity| format!("node={}", who.node());
    hold_book(b_state.dir())
        .unwrap()
        .note_stamps(
            &[
                (key(&i), ago(600_000)),
                (key(&e), ago(600_000)),
                (key(&c1), ago(120_000)),
                (key(&c2), ago(120_000)),
                (key(&c3), ago(120_000)),
            ],
            (&[(key(&i), ago(300_000)), (key(&e), ago(240_000))], &[]),
        )
        .unwrap();
    let reg = Registration {
        node: b.node().to_owned(),
        host: "m12.local".to_owned(),
        port: 7199,
        version: "0.149.0".to_owned(),
        key: None,
        signed: None,
    };
    let round = || {
        let mut nodes: Vec<String> = heartbeat_members(
            b_state.dir(),
            (&secret, &b),
            &reg,
            HANG,
            (Some(&log), &Vec::new),
        )
        .expect("the book reads")
        .into_iter()
        .map(|done| done.node)
        .collect();
        nodes.sort_unstable();
        nodes
    };
    let mut three: Vec<String> = [&c1, &c2, &c3]
        .map(|who| who.node().to_owned())
        .into_iter()
        .collect();
    three.sort_unstable();

    let (held_i, held_e) = (HeldSilent::at(i_at), HeldSilent::at(e_at));
    assert_eq!(round(), three);
    assert_eq!(
        (held_i.dials(), held_e.dials()),
        (1, 0),
        "the first round dials I once, missed longest ago, and stops at that miss"
    );
    drop((held_i, held_e));

    let (held_i, held_e) = (HeldSilent::at(i_at), HeldSilent::at(e_at));
    assert_eq!(round(), three);
    assert_eq!(
        (held_i.dials(), held_e.dials()),
        (0, 1),
        "the second round dials E, missed longest ago now, and stops at that miss"
    );
    drop((held_i, held_e, i_listener, e_listener));
}

/// Past a round's three answers a dial of a member marked missed takes its share whole: its
/// registration and book read end with it, so a member that proves its node and never answers
/// the registration (a listener wedged after the handshake) holds the round no longer than one
/// gone dark. M, marked missed, proves itself and never registers.
/// NEGATIVE CONTROL: register past three answers by the round's deadline, or a second from now
/// where that comes later, as before three: M's registration carried the rest of the round's
/// minute ("M's registration carried 59.9318295s, past its share of the round (15s)").
#[test]
fn past_its_three_answers_a_member_that_proves_and_never_registers_costs_only_its_share() {
    let secret_bytes = [0x5e; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let [b, c1, c2, c3, m] =
        [0x91, 0x92, 0x93, 0x94, 0x95].map(|seed| identity(&node_of(seed), seed));
    let log: MemberKeys = [&b, &c1, &c2, &c3, &m]
        .map(|who| (who.key(), Standing::Member))
        .into_iter()
        .collect();
    let b_state = State::new("wedged-b");
    keep_identity(b_state.dir(), &b, 0x91);
    write_key(b_state.dir(), &secret_bytes).unwrap();
    let members = [&c1, &c2, &c3, &m];
    let serving: Vec<(Issuing, State)> = members
        .into_iter()
        .enumerate()
        .map(|(at, who)| {
            let state = State::new(&format!("wedged-{at}"));
            write_key(state.dir(), &secret_bytes).unwrap();
            let at = Issuing::member(secret.clone(), state.dir(), who.clone(), log.clone());
            (at, state)
        })
        .collect();
    let now = unix_ms_now().expect("the clock reads");
    let ago = |ms: u64| now - ms;
    let book: Vec<Peer> = members
        .into_iter()
        .zip(&serving)
        .map(|(who, (at, _))| Peer {
            node: Some(who.node().to_owned()),
            host: "127.0.0.1".to_owned(),
            via: Via::Joined,
            seen_ms: Some(ago(600_000)),
            ..keyed(0, who.key(), [127, 0, 0, 1], at.port)
        })
        .collect();
    write_book(b_state.dir(), &book);
    let key = |who: &NodeIdentity| format!("node={}", who.node());
    hold_book(b_state.dir())
        .unwrap()
        .note_stamps(
            &[
                (key(&m), ago(600_000)),
                (key(&c1), ago(120_000)),
                (key(&c2), ago(120_000)),
                (key(&c3), ago(120_000)),
            ],
            (&[(key(&m), ago(300_000))], &[]),
        )
        .unwrap();
    let reg = Registration {
        node: b.node().to_owned(),
        host: "m12.local".to_owned(),
        port: 7199,
        version: "0.149.0".to_owned(),
        key: None,
        signed: None,
    };
    let m_at = SocketAddr::from((Ipv4Addr::LOCALHOST, serving[3].0.port));
    let held = HeldSilent::after_proof(m_at);
    let mut nodes: Vec<String> = heartbeat_members(
        b_state.dir(),
        (&secret, &b),
        &reg,
        HANG,
        (Some(&log), &Vec::new),
    )
    .expect("the book reads")
    .into_iter()
    .map(|done| done.node)
    .collect();
    nodes.sort_unstable();
    let mut three: Vec<String> = [&c1, &c2, &c3]
        .map(|who| who.node().to_owned())
        .into_iter()
        .collect();
    three.sort_unstable();
    assert_eq!(nodes, three);
    assert_eq!(held.dials(), 1, "past its three answers the round dials M");
    let share = HANG / u32::try_from(HEARTBEAT_MEMBERS + 1).unwrap();
    let bound = held
        .overrode()
        .expect("M proved itself, and its registration was held");
    assert!(
        bound <= share,
        "M's registration carried {bound:?}, past its share of the round ({share:?})"
    );
    assert!(
        read_missed(b_state.dir()).get(&key(&m)) > read_answered(b_state.dir()).get(&key(&m)),
        "M is still marked missed"
    );
}

/// A member heard from every round that this Mac's dial never reaches (a path that works one way
/// only) takes the first place among the members marked missed once, then waits its turn by its
/// misses: it does not take every round's one dial past the three answers, and a member back
/// that nobody heard from (its own heartbeat skips its rounds) is reached within a round or two.
/// Y registers here between rounds and never answers a dial; Z is back and nobody heard from it.
/// The first round dials Y, heard from, and stops; Y registers again; the second round reaches
/// Z, and Y's miss there comes after.
/// NEGATIVE CONTROL: put first every member marked missed heard from since its miss, whether
/// that miss too came while it was heard from (no `missed-heard` check in `round_order`): the
/// second round dialled Y first again and stopped there ("Z answers the second round").
#[test]
fn a_member_heard_from_that_this_mac_never_reaches_takes_the_first_place_once() {
    let secret_bytes = [0x5f; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let [b, c1, c2, c3, y, z] =
        [0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6].map(|seed| identity(&node_of(seed), seed));
    let log: MemberKeys = [&b, &c1, &c2, &c3, &y, &z]
        .map(|who| (who.key(), Standing::Member))
        .into_iter()
        .collect();
    let b_state = State::new("oneway-b");
    keep_identity(b_state.dir(), &b, 0xa1);
    write_key(b_state.dir(), &secret_bytes).unwrap();
    let answering = [&c1, &c2, &c3, &z];
    let serving: Vec<(Issuing, State)> = answering
        .into_iter()
        .enumerate()
        .map(|(at, who)| {
            let state = State::new(&format!("oneway-{at}"));
            write_key(state.dir(), &secret_bytes).unwrap();
            let at = Issuing::member(secret.clone(), state.dir(), who.clone(), log.clone());
            (at, state)
        })
        .collect();
    // Y takes connections and never answers this Mac's dial.
    let y_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let y_at = y_listener.local_addr().unwrap();
    // Every member answered ten minutes ago; C1, C2 and C3 since. The dials of Y and Z missed
    // since, Y's first; Y was heard from a minute ago, and Z not since it answered.
    let now = unix_ms_now().expect("the clock reads");
    let ago = |ms: u64| now - ms;
    let entry = |who: &NodeIdentity, port: u16, seen: u64| Peer {
        node: Some(who.node().to_owned()),
        host: "127.0.0.1".to_owned(),
        via: Via::Joined,
        seen_ms: Some(seen),
        ..keyed(0, who.key(), [127, 0, 0, 1], port)
    };
    let mut book: Vec<Peer> = answering
        .into_iter()
        .zip(&serving)
        .map(|(who, (at, _))| entry(who, at.port, ago(600_000)))
        .collect();
    book.push(entry(&y, y_at.port(), ago(60_000)));
    write_book(b_state.dir(), &book);
    let key = |who: &NodeIdentity| format!("node={}", who.node());
    hold_book(b_state.dir())
        .unwrap()
        .note_stamps(
            &[
                (key(&y), ago(600_000)),
                (key(&z), ago(600_000)),
                (key(&c1), ago(120_000)),
                (key(&c2), ago(120_000)),
                (key(&c3), ago(120_000)),
            ],
            (&[(key(&y), ago(300_000)), (key(&z), ago(240_000))], &[]),
        )
        .unwrap();
    let reg = Registration {
        node: b.node().to_owned(),
        host: "m12.local".to_owned(),
        port: 7199,
        version: "0.149.0".to_owned(),
        key: None,
        signed: None,
    };
    let round = || {
        let mut nodes: Vec<String> = heartbeat_members(
            b_state.dir(),
            (&secret, &b),
            &reg,
            HANG,
            (Some(&log), &Vec::new),
        )
        .expect("the book reads")
        .into_iter()
        .map(|done| done.node)
        .collect();
        nodes.sort_unstable();
        nodes
    };
    let mut three: Vec<String> = [&c1, &c2, &c3]
        .map(|who| who.node().to_owned())
        .into_iter()
        .collect();
    three.sort_unstable();

    let held = HeldSilent::at(y_at);
    assert_eq!(round(), three, "the first round stops at Y's miss");
    assert_eq!(
        held.dials(),
        1,
        "the first round dials Y, heard from since its miss"
    );
    assert_eq!(
        read_missed_heard(b_state.dir()).get(&key(&y)),
        read_missed(b_state.dir()).get(&key(&y)),
        "Y's miss came while it was heard from"
    );
    drop(held);

    // Y registers here again since that miss.
    let y_missed = read_missed(b_state.dir())[&key(&y)];
    let mut book = read_peers(b_state.dir()).unwrap();
    book.iter_mut()
        .filter(|peer| peer.node.as_deref() == Some(y.node()))
        .for_each(|peer| peer.seen_ms = Some(y_missed + 1));
    write_book(b_state.dir(), &book);

    let held = HeldSilent::at(y_at);
    let second = round();
    assert!(
        second.iter().any(|node| node == z.node()),
        "Z answers the second round, before Y: {second:?}"
    );
    assert_eq!(
        held.dials(),
        1,
        "the second round dials Y after Z, and stops at its miss"
    );
    let (answered, missed) = (read_answered(b_state.dir()), read_missed(b_state.dir()));
    assert!(
        answered.get(&key(&z)) > missed.get(&key(&z)),
        "Z is marked missed no more"
    );
    drop((held, y_listener));
}

/// With no membership log yet (no genesis on this Mac), a heartbeat that reaches the member this
/// Mac joined through makes its answer this Mac's copy of that book, and takes the signed entries
/// it holds into this Mac's own book, as with a log: a member it recorded binds here. A claim of a
/// node this Mac binds to another key is passed over.
/// NEGATIVE CONTROL: keep no copy at the heartbeat and merge no node this book lacks: C is
/// unbound.
#[test]
fn with_no_log_a_heartbeat_binds_the_members_its_issuer_recorded() {
    let secret_bytes = [0x56; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let (a_state, b_state) = (State::new("nolog-a"), State::new("nolog-b"));
    for state in [&a_state, &b_state] {
        write_key(state.dir(), &secret_bytes).unwrap();
    }
    let a = identity(&node_of(0x11), 0x11);
    let b = identity(&node_of(0x12), 0x12);
    let c = identity(&node_of(0x13), 0x13);
    let x = identity(&node_of(0x14), 0x14);
    let claimant = identity(x.node(), 0x15);
    keep_identity(b_state.dir(), &b, 0x12);
    // A recorded C's join, and a claim of X's node under another key.
    write_book(
        a_state.dir(),
        &[
            Peer::own(&c, ("127.0.0.1", 7301, "0.146.0"), 1_760_000_000_001),
            Peer::own(&claimant, ("127.0.0.1", 7302, "0.146.0"), 1_760_000_000_002),
        ],
    );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let a_port = listener.local_addr().unwrap().port();
    let named = Issuer {
        node: a.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: a_port,
        version: "0.146.0".to_owned(),
    };
    let _a_at = Issuing::gated(
        listener,
        (secret.clone(), Some(a.clone())),
        a_state.dir(),
        named,
        None,
    );
    // Heard now: the heartbeat prunes an entry silent for `EXPIRE_AFTER_MS`, after which X's
    // node is one this book no longer holds, and a claim of it binds as any new node does.
    let now = unix_ms_now().unwrap();
    let entry = |who: &NodeIdentity, port: u16, via: Via| Peer {
        node: Some(who.node().to_owned()),
        host: "127.0.0.1".to_owned(),
        via,
        seen_ms: Some(now),
        ..keyed(0, who.key(), [127, 0, 0, 1], port)
    };
    write_book(
        b_state.dir(),
        &[entry(&a, a_port, Via::Issuer), entry(&x, 7303, Via::Joined)],
    );
    let reg = Registration {
        node: b.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: 7304,
        version: "0.146.0".to_owned(),
        key: None,
        signed: None,
    };

    let answered = heartbeat_members(b_state.dir(), (&secret, &b), &reg, HANG, (None, &Vec::new))
        .expect("the book reads");
    assert_eq!(
        answered
            .iter()
            .map(|done| done.node.as_str())
            .collect::<Vec<_>>(),
        [a.node()]
    );
    let keys = Keyring::read(b_state.dir());
    assert_eq!(
        keys.bound(c.node()),
        Some(c.key()),
        "C, whose join A recorded, is bound here"
    );
    assert_eq!(
        keys.bound(x.node()),
        Some(x.key()),
        "a claim of X's node under another key binds nothing here"
    );
    assert!(
        read_peers(b_state.dir())
            .unwrap()
            .iter()
            .any(|peer| peer.node.as_deref() == Some(c.node()) && peer.signature_holds()),
        "C's signed entry, merged from A's book into B's own"
    );
}

fn seed_words(state: &Path) -> Option<String> {
    fleet_seed::read(state).unwrap().map(|seed| seed.words())
}

#[test]
fn a_relayed_seed_is_adopted_only_under_the_key_this_book_binds_to_its_minter() {
    use fleet_seed::{CONFLICT_FILE, FleetSeed};
    const T: u64 = 1_793_000_000_000;
    let state = State::new("relayed-seed");
    write_key(state.dir(), &[0x56; SECRET_LEN]).unwrap();
    let mark = FleetSecret::new(&[0x56; SECRET_LEN]).unwrap().mark();
    let me = identity(&node_of(0x12), 0x12);
    let (c, d) = (
        identity(&node_of(0x13), 0x13),
        identity(&node_of(0x14), 0x14),
    );
    write_book(state.dir(), &[keyed(0x13, c.key(), [10, 0, 0, 3], 7100)]);
    let adopt = |offered: &[FleetSeed], mark, now| {
        hold_book(state.dir())
            .unwrap()
            .adopt_relayed_seed(&me, mark, offered.iter().copied(), now)
            .unwrap()
    };
    let by_c = FleetSeed::mint(&c, mark, T).unwrap();
    let by_d = FleetSeed::mint(&d, mark, T - 1_000).unwrap();
    let other_fleet = FleetSeed::mint(&c, secret(9).mark(), T).unwrap();
    assert_eq!(adopt(&[by_d, other_fleet], mark, T), None);
    assert_eq!(
        seed_words(state.dir()),
        None,
        "an unbound minter's seed waits, and another fleet's verifies under no key here"
    );
    let later_c = FleetSeed::mint(&c, mark, T + 1).unwrap();
    assert_eq!(
        adopt(&[by_d, by_c, later_c], mark, T),
        Some(by_c),
        "the first on trust, which a later mint of its minter does not replace"
    );
    assert_eq!(seed_words(state.dir()), Some(by_c.words()));
    assert_eq!(adopt(&[by_c], mark, T), None, "held already");

    write_book(
        state.dir(),
        &[
            keyed(0x13, c.key(), [10, 0, 0, 3], 7100),
            keyed(0x14, d.key(), [10, 0, 0, 4], 7100),
        ],
    );
    assert_eq!(
        adopt(&[by_d], mark, T + 5),
        None,
        "another minter's, earlier or not"
    );
    assert_eq!(seed_words(state.dir()), Some(by_c.words()));
    let recorded = || String::from_utf8(read_private(state.dir(), CONFLICT_FILE, 512).unwrap());
    assert_eq!(recorded().unwrap(), fleet_seed::conflict_line(&by_d, T + 5));
    assert!(
        !recorded()
            .unwrap()
            .contains(&aterm_codec::hex::encode_lower(by_d.seed()))
    );
    adopt(&[by_d], mark, T + 9);
    assert_eq!(
        recorded().unwrap(),
        fleet_seed::conflict_line(&by_d, T + 5),
        "recorded once"
    );

    let earlier = FleetSeed::mint(&c, mark, T - 1).unwrap();
    assert_eq!(
        adopt(&[earlier], mark, T),
        Some(earlier),
        "its own minter's earlier"
    );
    let rotated = secret(7).mark();
    let resigned = earlier.resign(&c, rotated).unwrap();
    assert_eq!(adopt(&[resigned], mark, T), None, "not under the mark held");
    assert_eq!(
        adopt(&[resigned], rotated, T),
        Some(resigned),
        "signed again"
    );

    hold_book(state.dir()).unwrap().keep_seed(None).unwrap();
    assert_eq!(seed_words(state.dir()), None);
    assert!(
        read_private(state.dir(), CONFLICT_FILE, 512).is_err(),
        "the record goes with the seed"
    );
    write_private(state.dir(), fleet_seed::FILE, b"no seed\n").unwrap();
    assert_eq!(
        adopt(&[by_c], mark, T),
        Some(by_c),
        "a file of no seed is replaced"
    );

    hold_book(state.dir()).unwrap().keep_seed(None).unwrap();
    assert_eq!(
        adopt(&[by_c, by_d], mark, T + 20),
        Some(by_c),
        "each judged against the one taken before it in the same round"
    );
    assert_eq!(
        recorded().unwrap(),
        fleet_seed::conflict_line(&by_d, T + 20)
    );
}

#[test]
fn a_root_adopts_only_a_seed_it_minted() {
    use fleet_seed::FleetSeed;
    let state = State::new("root-seed");
    issue(state.dir());
    let root = identity(&node_of(0x11), 0x11);
    let c = identity(&node_of(0x13), 0x13);
    write_book(state.dir(), &[keyed(0x13, c.key(), [10, 0, 0, 3], 7100)]);
    let adopt = |offered: FleetSeed| {
        hold_book(state.dir())
            .unwrap()
            .adopt_relayed_seed(&root, secret(5).mark(), [offered], 1)
            .unwrap()
    };
    let by_c = FleetSeed::mint(&c, secret(5).mark(), 1_000).unwrap();
    assert_eq!(adopt(by_c), None, "a member's, bound here");
    assert_eq!(seed_words(state.dir()), None);
    assert_eq!(
        String::from_utf8(read_private(state.dir(), fleet_seed::CONFLICT_FILE, 512).unwrap())
            .unwrap(),
        fleet_seed::conflict_line(&by_c, 1),
        "recorded: a second minter in a founded fleet"
    );
    let own = FleetSeed::mint(&root, secret(5).mark(), 2_000).unwrap();
    assert_eq!(adopt(own), Some(own), "the one it minted and lost");

    let member = State::new("root-seed-member");
    issue(member.dir());
    write_book(
        member.dir(),
        &[Peer {
            via: Via::Issuer,
            ..keyed(0x13, c.key(), [10, 0, 0, 3], 7100)
        }],
    );
    assert_eq!(
        hold_book(member.dir())
            .unwrap()
            .adopt_relayed_seed(&root, secret(5).mark(), [by_c], 1)
            .unwrap(),
        Some(by_c),
        "control: a code holder whose book names an issuer is a member"
    );
}

#[test]
fn a_heartbeat_adopts_the_seed_a_members_book_relays_once_it_binds_the_minter() {
    let secret_bytes = [0x57; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let (a_state, b_state) = (State::new("relay-a"), State::new("relay-b"));
    for state in [&a_state, &b_state] {
        write_key(state.dir(), &secret_bytes).unwrap();
    }
    let a = identity(&node_of(0x11), 0x11);
    let b = identity(&node_of(0x12), 0x12);
    let c = identity(&node_of(0x13), 0x13);
    let x = identity(&node_of(0x14), 0x14);
    keep_identity(b_state.dir(), &b, 0x12);
    write_book(a_state.dir(), &[]);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let a_port = listener.local_addr().unwrap().port();
    let named = Issuer {
        node: a.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: a_port,
        version: "0.146.0".to_owned(),
    };
    let _a_at = Issuing::gated(
        listener,
        (secret.clone(), Some(a.clone())),
        a_state.dir(),
        named,
        None,
    );
    let now = unix_ms_now().unwrap();
    write_book(
        b_state.dir(),
        &[Peer {
            node: Some(a.node().to_owned()),
            host: "127.0.0.1".to_owned(),
            via: Via::Issuer,
            seen_ms: Some(now),
            ..keyed(0, a.key(), [127, 0, 0, 1], a_port)
        }],
    );
    let reg = Registration {
        node: b.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: 7314,
        version: "0.146.0".to_owned(),
        key: None,
        signed: None,
    };
    let relay = |seed: &fleet_seed::FleetSeed| {
        write_private(
            a_state.dir(),
            fleet_seed::FILE,
            format!("{}\n", seed.words()).as_bytes(),
        )
        .unwrap();
        heartbeat_members(b_state.dir(), (&secret, &b), &reg, HANG, (None, &Vec::new))
            .expect("the book reads")
    };

    let unbound = fleet_seed::FleetSeed::mint(&x, secret.mark(), 1_000).unwrap();
    assert_eq!(relay(&unbound).len(), 1, "A answered");
    assert_eq!(
        seed_words(b_state.dir()),
        None,
        "no book here binds its minter"
    );
    // C's entry reaches A's book with C's seed, so B binds C only in the round that relays
    // it: the seed is judged under the keys of that round's merge.
    write_book(
        a_state.dir(),
        &[Peer::own(
            &c,
            ("127.0.0.1", 7311, "0.146.0"),
            1_760_000_000_001,
        )],
    );
    assert_eq!(Keyring::read(b_state.dir()).bound(c.node()), None);
    let by_c = fleet_seed::FleetSeed::mint(&c, secret.mark(), 2_000).unwrap();
    relay(&by_c);
    assert_eq!(
        Keyring::read(b_state.dir()).bound(c.node()),
        Some(c.key()),
        "C's entry, merged from A's book"
    );
    assert_eq!(
        seed_words(b_state.dir()),
        Some(by_c.words()),
        "C's seed, relayed by A"
    );
}

/// With no membership log yet, a Mac that joined at a member, not at the Mac that minted the
/// code, reaches every book member to member (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.1, no
/// leaders): B, whose issuer A is away, takes in D, who joined at C, from C's book, and E, who
/// joined at B, then binds D from B's. No book gathers the fleet for the rest.
/// NEGATIVE CONTROL: with no log, merge only the nodes this book holds unless it names no issuer
/// (`merge_signed`'s roster guard, F17 as landed): "B binds D" fails.
#[test]
fn with_no_log_a_mac_that_joined_at_a_member_reaches_every_book_member_to_member() {
    let secret_bytes = [0x58; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let (b_state, c_state, e_state) = (
        State::new("nolog-spread-b"),
        State::new("nolog-spread-c"),
        State::new("nolog-spread-e"),
    );
    for state in [&b_state, &c_state, &e_state] {
        write_key(state.dir(), &secret_bytes).unwrap();
    }
    let [a, b, c, d, e] = [0x31, 0x32, 0x33, 0x34, 0x35].map(|seed| identity(&node_of(seed), seed));
    keep_identity(b_state.dir(), &b, 0x32);
    keep_identity(e_state.dir(), &e, 0x35);
    let listen = |who: &NodeIdentity, state: &State| {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let named = Issuer {
            node: who.node().to_owned(),
            host: "127.0.0.1".to_owned(),
            port: listener.local_addr().unwrap().port(),
            version: "0.147.0".to_owned(),
        };
        Issuing::gated(
            listener,
            (secret.clone(), Some(who.clone())),
            state.dir(),
            named,
            None,
        )
    };
    let entry = |who: &NodeIdentity, port: u16, via: Via| Peer {
        node: Some(who.node().to_owned()),
        host: "127.0.0.1".to_owned(),
        via,
        seen_ms: Some(1_760_000_000_000),
        ..keyed(0, who.key(), [127, 0, 0, 1], port)
    };
    // D joined at C while A, the Mac that minted the code, was away.
    write_book(
        c_state.dir(),
        &[Peer::own(
            &d,
            ("127.0.0.1", 7321, "0.147.0"),
            1_760_000_000_001,
        )],
    );
    let c_at = listen(&c, &c_state);
    // B joined through A, which is away, and knows C.
    write_book(
        b_state.dir(),
        &[
            entry(&a, closed_port(), Via::Issuer),
            entry(&c, c_at.port, Via::Joined),
        ],
    );
    let b_at = listen(&b, &b_state);
    // E joined through B.
    write_book(e_state.dir(), &[entry(&b, b_at.port, Via::Issuer)]);
    let reg = |who: &NodeIdentity, port: u16| Registration {
        node: who.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port,
        version: "0.147.0".to_owned(),
        key: None,
        signed: None,
    };

    let answered = heartbeat_members(
        b_state.dir(),
        (&secret, &b),
        &reg(&b, b_at.port),
        HANG,
        (None, &Vec::new),
    )
    .expect("the book reads");
    assert_eq!(
        answered
            .iter()
            .map(|done| done.node.as_str())
            .collect::<Vec<_>>(),
        [c.node()],
        "A is away"
    );
    assert_eq!(
        Keyring::read(b_state.dir()).bound(d.node()),
        Some(d.key()),
        "B binds D, who joined at C: {:?}",
        read_peers(b_state.dir())
    );

    let answered = heartbeat_members(
        e_state.dir(),
        (&secret, &e),
        &reg(&e, 7325),
        HANG,
        (None, &Vec::new),
    )
    .expect("the book reads");
    assert_eq!(answered.len(), 1, "{answered:?}");
    assert!(
        read_peers(e_state.dir())
            .unwrap()
            .iter()
            .any(|peer| peer.node.as_deref() == Some(d.node()) && peer.key == Some(d.key())),
        "E, who joined at B, holds D's entry: {:?}",
        read_peers(e_state.dir())
    );
}

/// With no membership log yet, a Mac that joined at a member, not at the issuer, reaches the
/// issuer's book too: the issuer's heartbeat that reaches that member takes it into the issuer's
/// own book, which each member's copy of it then binds.
/// NEGATIVE CONTROL: merge only the nodes the book holds: "the issuer took in D" fails.
#[test]
fn with_no_log_the_issuer_takes_in_a_mac_that_joined_at_a_member() {
    let secret_bytes = [0x57; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let (a_state, b_state) = (State::new("nolog-root-a"), State::new("nolog-root-b"));
    for state in [&a_state, &b_state] {
        write_key(state.dir(), &secret_bytes).unwrap();
    }
    let a = identity(&node_of(0x21), 0x21);
    let b = identity(&node_of(0x22), 0x22);
    let d = identity(&node_of(0x23), 0x23);
    keep_identity(a_state.dir(), &a, 0x21);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let b_port = listener.local_addr().unwrap().port();
    // B joined through A, and recorded D's join while A was away.
    let issuer_entry = Peer {
        node: Some(a.node().to_owned()),
        host: "127.0.0.1".to_owned(),
        via: Via::Issuer,
        seen_ms: Some(1_760_000_000_000),
        ..keyed(0, a.key(), [127, 0, 0, 1], closed_port())
    };
    write_book(
        b_state.dir(),
        &[
            issuer_entry,
            Peer::own(&d, ("127.0.0.1", 7311, "0.147.0"), 1_760_000_000_001),
        ],
    );
    let named = Issuer {
        node: b.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: b_port,
        version: "0.147.0".to_owned(),
    };
    let _b_at = Issuing::gated(
        listener,
        (secret.clone(), Some(b.clone())),
        b_state.dir(),
        named,
        None,
    );
    write_book(
        a_state.dir(),
        &[Peer {
            node: Some(b.node().to_owned()),
            host: "127.0.0.1".to_owned(),
            via: Via::Joined,
            seen_ms: Some(1_760_000_000_000),
            ..keyed(0, b.key(), [127, 0, 0, 1], b_port)
        }],
    );
    let reg = Registration {
        node: a.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: 7312,
        version: "0.147.0".to_owned(),
        key: None,
        signed: None,
    };

    let answered = heartbeat_members(a_state.dir(), (&secret, &a), &reg, HANG, (None, &Vec::new))
        .expect("the book reads");
    assert_eq!(
        answered
            .iter()
            .map(|done| done.node.as_str())
            .collect::<Vec<_>>(),
        [b.node()]
    );
    let held = read_peers(a_state.dir()).unwrap();
    assert!(
        held.iter()
            .any(|peer| peer.node.as_deref() == Some(d.node()) && peer.key == Some(d.key())),
        "the issuer took in D, who joined at B: {held:?}"
    );
    assert_eq!(Keyring::read(a_state.dir()).bound(d.node()), Some(d.key()));
}

/// WHEN A MEMBER LAST ANSWERED (docs/DESIGN-fluid-swarm-2026-10-28.md, F4's follow-up): the
/// later answer stands for each member, a peer with no node id is keyed by its place, a line the
/// file cannot read is passed over, and past [`PEERS_MAX`] members the oldest answers go first.
/// NEGATIVE CONTROL: a note that keeps the time it is handed over a later one fails "the later
/// answer stands"; one that never trims fails the cap.
#[test]
fn the_answered_file_keeps_each_members_latest_answer_and_drops_the_oldest_past_the_cap() {
    let state = State::new("answered");
    let dir = state.dir();
    let (one, named) = (
        joined(0x01, "m1.local", [10, 0, 0, 1], 7100),
        manual("10.0.0.9", 7100),
    );
    assert_eq!(answer_key(&one), format!("node={}", node_of(0x01)));
    assert_eq!(answer_key(&named), "place=10.0.0.9:7100");
    assert!(read_answered(dir).is_empty(), "no file, no answer");
    let note = |answers: &[(String, u64)]| hold_book(dir).unwrap().note_answered(answers);
    note(&[(answer_key(&one), 5_000), (answer_key(&named), 7_000)]).unwrap();
    note(&[
        (answer_key(&one), 4_000),
        ("node=not-a-node".to_owned(), 9_000),
    ])
    .unwrap();
    let answered = read_answered(dir);
    assert_eq!(
        answered.get(&answer_key(&one)),
        Some(&5_000),
        "the later answer stands"
    );
    assert_eq!(answered.get(&answer_key(&named)), Some(&7_000));
    assert_eq!(answered.len(), 2, "a key that is none is passed over");

    let mut text = std::fs::read_to_string(dir.join(ANSWERED_FILE)).unwrap();
    text.push_str("answered node=zz ms=1\nsomething else\n");
    std::fs::write(dir.join(ANSWERED_FILE), text).unwrap();
    assert_eq!(read_answered(dir), answered, "a line it cannot read");

    // A heartbeat's miss is no answer, and an answer's note keeps it.
    hold_book(dir)
        .unwrap()
        .note_stamps(&[], (&[(answer_key(&one), 6_000)], &[]))
        .unwrap();
    note(&[(answer_key(&named), 8_000)]).unwrap();
    assert_eq!(read_answered(dir).get(&answer_key(&one)), Some(&5_000));
    assert_eq!(
        read_missed(dir),
        std::collections::BTreeMap::from([(answer_key(&one), 6_000)]),
        "the miss is kept"
    );

    let many: Vec<(String, u64)> = (0..=u64::try_from(PEERS_MAX).unwrap())
        .map(|n| (format!("node={n:032x}"), 10_000 + n))
        .collect();
    note(&many).unwrap();
    let kept = read_answered(dir);
    assert_eq!(kept.len(), PEERS_MAX, "the cap");
    assert!(
        !kept.contains_key(&answer_key(&one)) && !kept.contains_key(&answer_key(&named)),
        "the oldest answers went first"
    );
    assert_eq!(
        kept.values().min(),
        Some(&10_001),
        "the newest {PEERS_MAX} stay"
    );
}

/// A CLOCK THAT STEPPED BACK (F4's follow-up): an answer noted while this machine's clock ran a
/// day ahead reads as none once the clock is right again, so `aterm fleet peers` never says such
/// a member answered "0s ago" while it stays away, and the next answer, by the corrected clock,
/// stands over it. One a few seconds ahead (written moments after its time was taken) still reads.
/// NEGATIVE CONTROL: a read that keeps every time (the note reads through it, so it keeps the
/// later, day-ahead time too) fails "a time a day ahead".
#[test]
fn an_answer_past_this_machines_clock_reads_as_none_and_the_next_answer_stands() {
    let state = State::new("answered-ahead");
    let dir = state.dir();
    let (one, two) = (
        joined(0x01, "m1.local", [10, 0, 0, 1], 7100),
        joined(0x02, "m2.local", [10, 0, 0, 2], 7100),
    );
    let now = unix_ms_now().unwrap();
    let note = |answers: &[(String, u64)]| hold_book(dir).unwrap().note_answered(answers);
    note(&[
        (answer_key(&one), now + 24 * 3_600_000),
        (answer_key(&two), now + 5_000),
    ])
    .unwrap();
    let answered = read_answered(dir);
    assert_eq!(answered.get(&answer_key(&one)), None, "a time a day ahead");
    assert_eq!(
        answered.get(&answer_key(&two)),
        Some(&(now + 5_000)),
        "a time moments ahead"
    );
    note(&[(answer_key(&one), now)]).unwrap();
    assert_eq!(
        read_answered(dir).get(&answer_key(&one)),
        Some(&now),
        "the next answer stands"
    );
}

#[test]
fn unreached_at_capacity_classifies_slot_drops_and_busy_without_retrying_refused_or_wrong_secret() {
    let addr = SocketAddr::from(([10, 0, 0, 1], 7100));

    // Positive: 16-slot drop (ConnectionReset / UnexpectedEof), NO capacity (AddressBusy),
    // handshake deadline, TCP connect timeout, and resolver pool saturation.
    for cap in [
        Unreached::Handshake(
            addr,
            FleetError::Io(io::Error::from(io::ErrorKind::ConnectionReset)),
        ),
        Unreached::Handshake(
            addr,
            FleetError::Tls(io::Error::from(io::ErrorKind::UnexpectedEof)),
        ),
        Unreached::Handshake(addr, FleetError::AddressBusy),
        Unreached::Deadline(Stage::Handshake, Duration::from_secs(5)),
        Unreached::Connect(addr, io::Error::from(io::ErrorKind::TimedOut)),
        Unreached::Local("the fleet resolver is at capacity".to_owned()),
    ] {
        assert!(cap.at_capacity(), "expected at_capacity for {cap:?}");
    }

    // Negative controls: listener down (ConnectionRefused), wrong secret, wrong node, NXDOMAIN.
    for non_cap in [
        Unreached::Connect(addr, io::Error::from(io::ErrorKind::ConnectionRefused)),
        Unreached::Handshake(addr, FleetError::ListenerWrongSecret),
        Unreached::OtherNode {
            at: addr,
            said: "aa".repeat(16),
            want: "bb".repeat(16),
        },
        Unreached::Resolve("missing.local".to_owned()),
    ] {
        assert!(
            !non_cap.at_capacity(),
            "must not classify {non_cap:?} as capacity"
        );
    }
}

/// A heartbeat round keeps no copy of the issuer's book over one another read kept while the round
/// was out: that read can be newer than the book the round fetched. The next round reads the
/// issuer again and keeps its book.
#[test]
fn a_heartbeat_round_leaves_a_copy_another_read_kept_meanwhile() {
    let secret_bytes = [0x59; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let (a_state, b_state) = (State::new("hb-kept-a"), State::new("hb-kept-b"));
    for state in [&a_state, &b_state] {
        write_key(state.dir(), &secret_bytes).unwrap();
    }
    let a = identity(&node_of(0x41), 0x41);
    let b = identity(&node_of(0x42), 0x42);
    let served = identity(&node_of(0x43), 0x43);
    keep_identity(b_state.dir(), &b, 0x42);
    let kept = identity(&node_of(0x44), 0x44);
    let log: MemberKeys = [&a, &b, &served, &kept]
        .into_iter()
        .map(|id| (id.key(), Standing::Member))
        .collect();
    write_book(
        a_state.dir(),
        &[Peer::own(
            &served,
            ("127.0.0.1", 7411, "0.147.0"),
            1_760_000_000_001,
        )],
    );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let a_port = listener.local_addr().unwrap().port();
    let named = Issuer {
        node: a.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: a_port,
        version: "0.147.0".to_owned(),
    };
    let _a_at = Issuing::gated(
        listener,
        (secret.clone(), Some(a.clone())),
        a_state.dir(),
        named,
        None,
    );
    write_book(
        b_state.dir(),
        &[Peer {
            node: Some(a.node().to_owned()),
            host: "127.0.0.1".to_owned(),
            via: Via::Issuer,
            seen_ms: Some(1_760_000_000_000),
            ..keyed(0, a.key(), [127, 0, 0, 1], a_port)
        }],
    );
    let reg = Registration {
        node: b.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: 7412,
        version: "0.147.0".to_owned(),
        key: None,
        signed: None,
    };
    let kept_copy = Peer {
        via: Via::IssuerBook,
        ..Peer::own(&kept, ("127.0.0.1", 7413, "0.147.0"), 1_760_000_000_002)
    };
    let names = |copies: &[Peer]| {
        copies
            .iter()
            .filter_map(|peer| peer.node.clone())
            .collect::<Vec<_>>()
    };
    // The adverts are read after the issuer answered: a `peers` read keeps its copy then.
    let answered = heartbeat_members_with_denied(
        b_state.dir(),
        (&secret, &b),
        &reg,
        HANG,
        (Some(&log), &[], &|| {
            keep_copy(b_state.dir(), a.node(), std::slice::from_ref(&kept_copy)).unwrap();
            Vec::new()
        }),
    )
    .expect("heartbeat succeeds");
    assert_eq!(
        answered
            .iter()
            .map(|done| done.node.as_str())
            .collect::<Vec<_>>(),
        [a.node()]
    );
    assert_eq!(
        names(&read_issuer_book(b_state.dir(), a.node())),
        [kept.node()],
        "the copy kept while the round was out stays"
    );
    heartbeat_members_with_denied(
        b_state.dir(),
        (&secret, &b),
        &reg,
        HANG,
        (Some(&log), &[], &Vec::new),
    )
    .expect("heartbeat succeeds");
    assert_eq!(
        names(&read_issuer_book(b_state.dir(), a.node())),
        [served.node()],
        "a round no other read overlapped keeps the issuer's book"
    );
}

/// A member this Mac's book dropped while a heartbeat round was out (`aterm fleet peers rm`) is not
/// merged back from the book that round fetched; a round that began after the removal merges what
/// the issuer still lists.
#[test]
fn a_heartbeat_round_merges_back_no_member_dropped_meanwhile() {
    let secret_bytes = [0x5a; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let (a_state, b_state) = (State::new("hb-drop-a"), State::new("hb-drop-b"));
    for state in [&a_state, &b_state] {
        write_key(state.dir(), &secret_bytes).unwrap();
    }
    let a = identity(&node_of(0x51), 0x51);
    let b = identity(&node_of(0x52), 0x52);
    let listed = identity(&node_of(0x53), 0x53);
    keep_identity(b_state.dir(), &b, 0x52);
    let log: MemberKeys = [&a, &b, &listed]
        .into_iter()
        .map(|id| (id.key(), Standing::Member))
        .collect();
    write_book(
        a_state.dir(),
        &[Peer::own(
            &listed,
            ("127.0.0.1", 7421, "0.147.0"),
            1_760_000_000_001,
        )],
    );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let a_port = listener.local_addr().unwrap().port();
    let named = Issuer {
        node: a.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: a_port,
        version: "0.147.0".to_owned(),
    };
    let _a_at = Issuing::gated(
        listener,
        (secret.clone(), Some(a.clone())),
        a_state.dir(),
        named,
        None,
    );
    write_book(
        b_state.dir(),
        &[Peer {
            node: Some(a.node().to_owned()),
            host: "127.0.0.1".to_owned(),
            via: Via::Issuer,
            seen_ms: Some(1_760_000_000_000),
            ..keyed(0, a.key(), [127, 0, 0, 1], a_port)
        }],
    );
    let reg = Registration {
        node: b.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: 7422,
        version: "0.147.0".to_owned(),
        key: None,
        signed: None,
    };
    let holds_listed = || {
        read_peers(b_state.dir())
            .unwrap()
            .iter()
            .any(|peer| peer.node.as_deref() == Some(listed.node()))
    };
    let round = |hints: &dyn Fn() -> Vec<SocketAddr>| {
        heartbeat_members_with_denied(
            b_state.dir(),
            (&secret, &b),
            &reg,
            HANG,
            (Some(&log), &[], hints),
        )
        .expect("heartbeat succeeds")
    };
    round(&Vec::new);
    assert!(holds_listed(), "a round merges what the issuer lists");
    // The adverts are read after the issuer answered: a `peers rm` lands then.
    round(&|| {
        update_peers(b_state.dir(), |book| {
            book.retain(|peer| peer.node.as_deref() != Some(listed.node()));
            Ok(())
        })
        .unwrap();
        Vec::new()
    });
    assert!(
        !holds_listed(),
        "a member dropped while the round was out stays dropped"
    );
    round(&Vec::new);
    assert!(
        holds_listed(),
        "a round that began after the removal merges what the issuer still lists"
    );
}

/// A Mac's heartbeat (`heartbeat_members_with_denied`) and book share (`serve_book_with_log`)
/// keep out both the vault's deny-listed nodes and the node keys the membership log holds out:
/// a denied or removed Mac is not dialled on heartbeat, is pruned from the local book, is not
/// merged back from another member's book, is omitted from `ISSUER_BOOK_FILE`, and is omitted
/// when serving `FleetOp::Book`.
#[test]
fn heartbeat_and_book_sharing_omit_denied_and_log_held_out_members() {
    let secret_bytes = [0x58; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let (a_state, b_state) = (State::new("hb-denied-a"), State::new("hb-denied-b"));
    for state in [&a_state, &b_state] {
        write_key(state.dir(), &secret_bytes).unwrap();
    }
    let a = identity(&node_of(0x31), 0x31);
    let b = identity(&node_of(0x32), 0x32);
    keep_identity(b_state.dir(), &b, 0x32);
    let denied_peer = identity(&node_of(0x33), 0x33);
    let removed_peer = identity(&node_of(0x34), 0x34);
    let good_peer = identity(&node_of(0x35), 0x35);

    let log: MemberKeys = [
        (a.key(), Standing::Member),
        (b.key(), Standing::Member),
        (denied_peer.key(), Standing::Member),
        (removed_peer.key(), Standing::Out("removed".to_owned())),
        (good_peer.key(), Standing::Member),
    ]
    .into_iter()
    .collect();
    let denied = vec![denied_peer.node().to_owned()];

    // A's book holds denied_peer, removed_peer, and good_peer.
    write_book(
        a_state.dir(),
        &[
            Peer::own(
                &denied_peer,
                ("127.0.0.1", 7401, "0.147.0"),
                1_760_000_000_001,
            ),
            Peer::own(
                &removed_peer,
                ("127.0.0.1", 7402, "0.147.0"),
                1_760_000_000_002,
            ),
            Peer::own(
                &good_peer,
                ("127.0.0.1", 7403, "0.147.0"),
                1_760_000_000_003,
            ),
        ],
    );

    // 1. `serve_book_with_log` omits both `denied` and `log`-held-out entries.
    let mut served_wire = Vec::new();
    let shared_count = serve_book_with_log(
        &mut served_wire,
        a_state.dir(),
        || Ok(a.node().to_owned()),
        Some(1_760_000_000_010),
        (&denied, Some(&log), None),
    )
    .unwrap()
    .unwrap();
    assert_eq!(shared_count, 1, "only good_peer should be served");
    let served_text = String::from_utf8(served_wire).unwrap();
    assert!(served_text.contains(good_peer.node()), "{served_text}");
    assert!(!served_text.contains(denied_peer.node()), "{served_text}");
    assert!(!served_text.contains(removed_peer.node()), "{served_text}");

    // 2. Even when A serves its raw book without `denied`/`log` filtering (e.g. A has not synced
    // the vault deny list or membership log yet), B's `heartbeat_members_with_denied` prunes
    // `denied_peer` from B's book, refuses to merge `denied_peer` or `removed_peer`, and omits
    // both from B's `ISSUER_BOOK_FILE`.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let a_port = listener.local_addr().unwrap().port();
    let named = Issuer {
        node: a.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: a_port,
        version: "0.147.0".to_owned(),
    };
    let _a_at = Issuing::gated(
        listener,
        (secret.clone(), Some(a.clone())),
        a_state.dir(),
        named,
        None,
    );
    write_book(
        b_state.dir(),
        &[
            Peer {
                node: Some(a.node().to_owned()),
                host: "127.0.0.1".to_owned(),
                via: Via::Issuer,
                seen_ms: Some(1_760_000_000_000),
                ..keyed(0, a.key(), [127, 0, 0, 1], a_port)
            },
            Peer::own(
                &denied_peer,
                ("127.0.0.1", closed_port(), "0.147.0"),
                1_760_000_000_000,
            ),
        ],
    );
    let reg = Registration {
        node: b.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: 7404,
        version: "0.147.0".to_owned(),
        key: None,
        signed: None,
    };
    let answered = heartbeat_members_with_denied(
        b_state.dir(),
        (&secret, &b),
        &reg,
        HANG,
        (Some(&log), &denied, &Vec::new),
    )
    .expect("heartbeat succeeds");
    assert_eq!(
        answered
            .iter()
            .map(|done| done.node.as_str())
            .collect::<Vec<_>>(),
        [a.node()]
    );
    let b_peers = read_peers(b_state.dir()).unwrap();
    assert!(
        b_peers
            .iter()
            .any(|p| p.node.as_deref() == Some(good_peer.node())),
        "good_peer merged into B's book: {b_peers:?}"
    );
    assert!(
        !b_peers
            .iter()
            .any(|p| p.node.as_deref() == Some(denied_peer.node())),
        "denied_peer pruned and not merged back into B's book: {b_peers:?}"
    );
    assert!(
        !b_peers
            .iter()
            .any(|p| p.node.as_deref() == Some(removed_peer.node())),
        "removed_peer not merged into B's book: {b_peers:?}"
    );
    let b_copies = read_issuer_book(b_state.dir(), a.node());
    assert!(
        b_copies
            .iter()
            .any(|p| p.node.as_deref() == Some(good_peer.node())),
        "good_peer kept in B's copy of A's book: {b_copies:?}"
    );
    assert!(
        !b_copies
            .iter()
            .any(|p| p.node.as_deref() == Some(denied_peer.node())),
        "denied_peer omitted from B's copy of A's book: {b_copies:?}"
    );
    assert!(
        !b_copies
            .iter()
            .any(|p| p.node.as_deref() == Some(removed_peer.node())),
        "removed_peer omitted from B's copy of A's book: {b_copies:?}"
    );

    // 3. `store_pushed_facts` / `read_pushed_facts` enforce owner-private files and reject symlinks
    // and world-readable files; `serve_facts` rejects an unterminated final line; and peer RPCs
    // clamp `Duration::MAX` deadlines while rejecting unterminated `OK` lines.
    store_pushed_facts(b_state.dir(), good_peer.node(), "OK 1\nv=1\n").unwrap();
    assert_eq!(
        read_pushed_facts(b_state.dir()),
        vec![(good_peer.node().to_owned(), "OK 1\nv=1\n".to_owned())]
    );
    let facts_dir = b_state.dir().join(SWARM_FACTS_DIR);
    let open_path = facts_dir.join(format!("{}.txt", a.node()));
    std::fs::write(&open_path, "OK 1\nv=1\n").unwrap();
    mode(&open_path, 0o644);
    let outside = b_state.dir().join("outside-facts.txt");
    std::fs::write(&outside, "OK 1\nv=1\n").unwrap();
    mode(&outside, 0o600);
    std::os::unix::fs::symlink(&outside, facts_dir.join(format!("{}.txt", b.node()))).unwrap();
    assert_eq!(
        read_pushed_facts(b_state.dir()),
        vec![(good_peer.node().to_owned(), "OK 1\nv=1\n".to_owned())],
        "open-mode and symlinked swarm-facts entries must be ignored"
    );

    let proof = NodeProof {
        node: good_peer.node().to_owned(),
        key: good_peer.key(),
    };
    let mut unterminated_wire = io::Cursor::new(b"OK 1\nv=1 without newline".to_vec());
    let served_unterminated =
        serve_facts(&mut unterminated_wire, Some(&proof), b_state.dir(), |_| {
            Ok(Some(good_peer.node().to_owned()))
        })
        .unwrap();
    assert!(
        served_unterminated.is_err(),
        "unterminated facts line must be rejected: {served_unterminated:?}"
    );

    let trunc_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let trunc_port = trunc_listener.local_addr().unwrap().port();
    let trunc_issuer = Issuer {
        node: a.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: trunc_port,
        version: "0.147.0".to_owned(),
    };
    let _trunc_at = Issuing::hooked(
        trunc_listener,
        secret.clone(),
        a_state.dir(),
        trunc_issuer,
        &[FleetOp::Census, FleetOp::Facts],
        Arc::new(|op| match op {
            FleetOp::Census => Some("OK {\"partial\":1"),
            FleetOp::Facts => Some("OK"),
            _ => None,
        }),
    );
    let census_err = request_census_from_peer(
        "127.0.0.1",
        Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        trunc_port,
        (&secret, Some(&b)),
        Duration::MAX,
    )
    .expect_err("unterminated census reply must fail");
    assert!(census_err.contains("unterminated"), "{census_err}");
    let facts_err = push_facts_to_peer(
        "127.0.0.1",
        Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        trunc_port,
        (&secret, Some(&b)),
        "OK 1\nv=1\n",
        Duration::MAX,
    )
    .expect_err("unterminated facts push reply must fail");
    assert!(facts_err.contains("unterminated"), "{facts_err}");
}

#[test]
fn facts_mail_code_and_census_enforce_framing_bounds_and_clamp_max_deadlines() {
    struct Duplex {
        input: io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }
    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let state = State::new("framing-bounds");
    let peer_id = identity(&node_of(0x41), 0x41);
    let proof = NodeProof {
        node: peer_id.node().to_owned(),
        key: peer_id.key(),
    };

    // `serve_facts_with_denied` rejects unterminated head and unterminated body lines.
    let mut bad_head = Duplex {
        input: io::Cursor::new(b"OK 1".to_vec()),
        output: Vec::new(),
    };
    let res = serve_facts_with_denied(
        &mut bad_head,
        Some(&proof),
        state.dir(),
        (&[], None),
        |_| Ok(Some(proof.node.clone())),
    )
    .unwrap();
    assert_eq!(res.unwrap_err(), "invalid facts head");

    let mut bad_body = Duplex {
        input: io::Cursor::new(format!("OK 1\nnode={}", proof.node).into_bytes()),
        output: Vec::new(),
    };
    let res = serve_facts_with_denied(
        &mut bad_body,
        Some(&proof),
        state.dir(),
        (&[], None),
        |_| Ok(Some(proof.node.clone())),
    )
    .unwrap();
    assert_eq!(res.unwrap_err(), "truncated facts push");

    let mut good_body = Duplex {
        input: io::Cursor::new(format!("OK 1\nnode={}\n", proof.node).into_bytes()),
        output: Vec::new(),
    };
    let res = serve_facts_with_denied(
        &mut good_body,
        Some(&proof),
        state.dir(),
        (&[], None),
        |_| Ok(Some(proof.node.clone())),
    )
    .unwrap();
    assert_eq!(res.unwrap(), proof.node);

    // Peer dialers reject unframed/oversize replies and clamp `Duration::MAX` without panicking.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let running = Arc::new(AtomicBool::new(true));
    let next_reply: Arc<std::sync::Mutex<Vec<u8>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let listening = Listening {
        secret: secret(9),
        scope: FleetScope::Content,
        node: Some(identity(&node_of(0x42), 0x42)),
        serves: &[FleetOp::Facts, FleetOp::MailCode, FleetOp::Census],
        slots: 8,
        per_source: 8,
        lifetime: HANG,
    };
    let server = std::thread::spawn({
        let running = Arc::clone(&running);
        let next_reply = Arc::clone(&next_reply);
        move || {
            serve_with_handshake(
                &listener,
                &listening,
                &running,
                HANG,
                move |served, op| {
                    let bytes = next_reply.lock().unwrap().clone();
                    served.answer(|stream| {
                        stream.set_read_timeout(Some(HANG))?;
                        if matches!(op, FleetOp::Facts | FleetOp::MailCode) {
                            let mut reader = io::BufReader::new(Read::by_ref(stream).take(512));
                            let mut line = String::new();
                            let _ = reader.read_line(&mut line)?;
                        }
                        stream.write_all(&bytes)?;
                        stream.flush()
                    })
                },
                |_| {},
            );
        }
    });

    let sec = secret(9);
    let ip = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));

    *next_reply.lock().unwrap() = b"OK {\"truncated\":1".to_vec();
    assert!(request_census_from_peer("127.0.0.1", ip, port, (&sec, None), Duration::MAX).is_err());

    *next_reply.lock().unwrap() = format!("OK {}\n", "x".repeat(MAX_CENSUS_REPLY + 1)).into_bytes();
    assert!(request_census_from_peer("127.0.0.1", ip, port, (&sec, None), HANG).is_err());

    *next_reply.lock().unwrap() = b"OK {\"ok\":true}\n".to_vec();
    assert_eq!(
        request_census_from_peer("127.0.0.1", ip, port, (&sec, None), Duration::MAX).unwrap(),
        Ok("{\"ok\":true}".to_owned())
    );

    *next_reply.lock().unwrap() = b"OK".to_vec();
    assert!(
        push_facts_to_peer(
            "127.0.0.1",
            ip,
            port,
            (&sec, Some(&peer_id)),
            "OK 0\n",
            Duration::MAX,
        )
        .is_err()
    );

    *next_reply.lock().unwrap() = b"OK\n".to_vec();
    push_facts_to_peer(
        "127.0.0.1",
        ip,
        port,
        (&sec, Some(&peer_id)),
        "OK 0\n",
        Duration::MAX,
    )
    .unwrap();

    *next_reply.lock().unwrap() = b"OK sealed".to_vec();
    assert!(
        request_mail_code_from_peer(
            "127.0.0.1",
            ip,
            port,
            (&sec, Some(&peer_id)),
            "mail-code a@example.com",
            Duration::MAX,
        )
        .is_err()
    );

    let max_sealed = "s".repeat(MAX_MAIL_CODE_REPLY as usize);
    *next_reply.lock().unwrap() = format!("OK {max_sealed}\r\n").into_bytes();
    assert_eq!(
        request_mail_code_from_peer(
            "127.0.0.1",
            ip,
            port,
            (&sec, Some(&peer_id)),
            "mail-code a@example.com",
            Duration::MAX,
        )
        .unwrap(),
        Ok(max_sealed)
    );

    *next_reply.lock().unwrap() =
        format!("OK {}\n", "s".repeat(MAX_MAIL_CODE_REPLY as usize + 1)).into_bytes();
    assert!(
        request_mail_code_from_peer(
            "127.0.0.1",
            ip,
            port,
            (&sec, Some(&peer_id)),
            "mail-code a@example.com",
            HANG,
        )
        .is_err()
    );

    running.store(false, Ordering::Release);
    let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
    let _ = server.join();
}

#[test]
fn store_pushed_facts_never_acknowledges_an_unbound_publication() {
    let state = State::new("facts-write-key-refusal");
    let node = "33".repeat(16);
    let key = NodeIdentity::from_seed(&node, &[0x33; 32]).unwrap().key();
    let before = "previous survey facts\n";
    store_pushed_facts(state.dir(), &node, before).unwrap();
    let dir = state.dir().join(SWARM_FACTS_DIR);
    std::fs::create_dir(dir.join(format!("{node}.key"))).unwrap();

    let result = store_pushed_facts_with_key(state.dir(), &node, Some(key), "new survey facts\n");
    prune_pushed_facts(state.dir(), &[], &[key.hex()], None);
    let after = read_pushed_facts(state.dir());
    match result {
        Ok(()) => assert!(
            after.is_empty(),
            "acknowledged facts must carry the proved key that authorizes their eviction: {after:?}"
        ),
        Err(_) => assert_eq!(after, [(node, before.to_owned())]),
    }
}

/// Existing binaries wrote these raw files, with no versioned-record interpretation.
fn legacy_pushed_facts(state: &Path, node: &str, key: Option<NodeKey>, payload: &str) {
    let dir = state.join(SWARM_FACTS_DIR);
    ensure_private_dir(&dir).unwrap();
    write_private(&dir, &format!("{node}.txt"), payload.as_bytes()).unwrap();
    if let Some(key) = key {
        write_private(
            &dir,
            &format!("{node}.key"),
            format!("{}\n", key.hex()).as_bytes(),
        )
        .unwrap();
    }
}

#[test]
fn pushed_facts_records_keep_payload_binding_and_legacy_fallback_distinct() {
    let state = State::new("facts-record-binding");
    let node = "33".repeat(16);
    let old = NodeIdentity::from_seed(&node, &[0x11; 32]).unwrap().key();
    let new = NodeIdentity::from_seed(&node, &[0x22; 32]).unwrap().key();
    legacy_pushed_facts(state.dir(), &node, Some(old), "legacy bytes\n");
    store_pushed_facts_with_key(state.dir(), &node, Some(new), "new\nwire\0bytes\n").unwrap();
    // An old binary can still write legacy files; it cannot silently replace this record.
    legacy_pushed_facts(state.dir(), &node, Some(old), "later legacy writer\n");
    assert_eq!(
        read_pushed_facts(state.dir()),
        [(node.clone(), "new\nwire\0bytes\n".to_owned())]
    );
    prune_pushed_facts(state.dir(), &[], &[old.hex()], None);
    assert_eq!(
        read_pushed_facts(state.dir()),
        [(node.clone(), "new\nwire\0bytes\n".to_owned())]
    );
    prune_pushed_facts(state.dir(), &[], &[new.hex()], None);
    assert!(read_pushed_facts(state.dir()).is_empty());

    // A current unproved publication inherits no proved legacy sidecar. The established
    // keyring, when present, still decides its standing.
    legacy_pushed_facts(state.dir(), &node, Some(old), "legacy again\n");
    store_pushed_facts(state.dir(), &node, "unproved current\n").unwrap();
    prune_pushed_facts(state.dir(), &[], &[old.hex()], None);
    assert_eq!(
        read_pushed_facts(state.dir()),
        [(node.clone(), "unproved current\n".to_owned())]
    );
    write_book(state.dir(), &[keyed(0x33, new, [10, 0, 0, 1], 7100)]);
    assert!(read_admitted_pushed_facts(state.dir(), &[], &[new.hex()], None).is_empty());
    prune_pushed_facts(state.dir(), &[], &[new.hex()], None);
    assert!(read_pushed_facts(state.dir()).is_empty());
}

#[test]
fn pushed_facts_records_reject_unsafe_or_corrupt_current_without_legacy_fallback() {
    let node = "33".repeat(16);
    for kind in [
        "private",
        "limit",
        "symlink",
        "hardlink",
        "open",
        "open-directory",
        "oversize",
        "payload-overflow",
        "directory",
        "missing",
        "invalid-utf8",
        "magic",
        "version",
        "key",
    ] {
        let state = State::new("facts-record-kinds");
        let dir = state.dir().join(SWARM_FACTS_DIR);
        legacy_pushed_facts(state.dir(), &node, None, "legacy fallback\n");
        let payload = if kind == "limit" {
            "x".repeat(REPLY_MAX)
        } else {
            "current wire\n".to_owned()
        };
        store_pushed_facts(state.dir(), &node, &payload).unwrap();
        let name = format!("{node}.facts");
        let path = dir.join(&name);
        write_private(state.dir(), "outside", b"outside sentinel\n").unwrap();
        let outside = state.dir().join("outside");
        match kind {
            "private" | "limit" => {}
            "symlink" | "hardlink" => {
                std::fs::remove_file(&path).unwrap();
                if kind == "symlink" {
                    std::os::unix::fs::symlink(&outside, &path).unwrap();
                } else {
                    std::fs::hard_link(&outside, &path).unwrap();
                }
            }
            "open" => mode(&path, 0o644),
            "open-directory" => mode(&dir, 0o755),
            "oversize" => std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len((PUSHED_FACTS_MAX + 1) as u64)
                .unwrap(),
            "payload-overflow" => write_private(
                &dir,
                &name,
                format!("{PUSHED_FACTS_MAGIC}-\n{}", "x".repeat(REPLY_MAX + 1)).as_bytes(),
            )
            .unwrap(),
            "directory" | "missing" => {
                std::fs::remove_file(&path).unwrap();
                if kind == "directory" {
                    std::fs::create_dir(&path).unwrap();
                }
            }
            "invalid-utf8" => write_private(&dir, &name, &[0xff]).unwrap(),
            "magic" => write_private(
                &dir,
                &name,
                b"raw legacy bytes are not a versioned record\n",
            )
            .unwrap(),
            "version" => {
                write_private(&dir, &name, b"aterm-pushed-facts-v2\n-\nfuture record\n").unwrap()
            }
            "key" => write_private(
                &dir,
                &name,
                format!("{PUSHED_FACTS_MAGIC}not-a-key\nwire\n").as_bytes(),
            )
            .unwrap(),
            _ => unreachable!(),
        }
        let got = read_pushed_facts(state.dir());
        let expected = match kind {
            "private" | "limit" => vec![(node.clone(), payload)],
            "missing" => vec![(node.clone(), "legacy fallback\n".to_owned())],
            _ => Vec::new(),
        };
        assert_eq!(got, expected, "{kind}");
        assert_eq!(
            std::fs::read(&outside).unwrap(),
            b"outside sentinel\n",
            "{kind}"
        );
    }
}

#[test]
fn pushed_facts_write_failure_keeps_the_previous_record_and_a_later_writer_wins() {
    use std::sync::mpsc;
    let state = State::new("facts-record-concurrent");
    let node = "33".repeat(16);
    let old = NodeIdentity::from_seed(&node, &[0x11; 32]).unwrap().key();
    let failed = NodeIdentity::from_seed(&node, &[0x22; 32]).unwrap().key();
    let winner = NodeIdentity::from_seed(&node, &[0x33; 32]).unwrap().key();
    store_pushed_facts_with_key(state.dir(), &node, Some(old), "previous\n").unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first = std::thread::spawn({
        let state = state.dir().to_path_buf();
        let node = node.clone();
        move || {
            store_pushed_facts_using(
                &state,
                &node,
                Some(failed),
                "refused\n",
                HANG,
                || {},
                |dir, name, bytes| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(HANG).unwrap();
                    mode(dir, 0o500);
                    let result = write_private(dir, name, bytes);
                    mode(dir, 0o700);
                    assert!(result.is_err(), "actual directory write refusal");
                    assert_eq!(
                        read_pushed_facts(&state),
                        [(node.clone(), "previous\n".to_owned())]
                    );
                    result
                },
            )
        }
    });
    entered_rx.recv_timeout(HANG).unwrap();
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let second = std::thread::spawn({
        let state = state.dir().to_path_buf();
        let node = node.clone();
        move || {
            store_pushed_facts_using(
                &state,
                &node,
                Some(winner),
                "winner\n",
                HANG,
                || {
                    let _ = waiting_tx.send(());
                },
                write_private,
            )
        }
    });
    waiting_rx.recv_timeout(HANG).unwrap();
    assert_eq!(
        read_pushed_facts(state.dir()),
        [(node.clone(), "previous\n".to_owned())]
    );
    release_tx.send(()).unwrap();
    assert!(first.join().unwrap().is_err());
    second.join().unwrap().unwrap();
    prune_pushed_facts(state.dir(), &[], &[old.hex(), failed.hex()], None);
    assert_eq!(
        read_pushed_facts(state.dir()),
        [(node.clone(), "winner\n".to_owned())]
    );
    prune_pushed_facts(state.dir(), &[], &[winner.hex()], None);
    assert!(read_pushed_facts(state.dir()).is_empty());
}

#[test]
fn pushed_facts_pruning_waits_then_judges_the_winning_publication() {
    use std::sync::mpsc;
    let state = State::new("facts-record-prune-race");
    let node = "33".repeat(16);
    let old = NodeIdentity::from_seed(&node, &[0x11; 32]).unwrap().key();
    let winner = NodeIdentity::from_seed(&node, &[0x33; 32]).unwrap().key();
    store_pushed_facts_with_key(state.dir(), &node, Some(old), "previous\n").unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer = std::thread::spawn({
        let state = state.dir().to_path_buf();
        let node = node.clone();
        move || {
            store_pushed_facts_using(
                &state,
                &node,
                Some(winner),
                "winner\n",
                HANG,
                || {},
                |dir, name, bytes| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(HANG).unwrap();
                    write_private(dir, name, bytes)
                },
            )
        }
    });
    entered_rx.recv_timeout(HANG).unwrap();
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let pruning = std::thread::spawn({
        let state = state.dir().to_path_buf();
        move || {
            prune_pushed_facts_using(&state, &[], &[old.hex()], None, HANG, || {
                let _ = waiting_tx.send(());
            })
        }
    });
    waiting_rx.recv_timeout(HANG).unwrap();
    release_tx.send(()).unwrap();
    writer.join().unwrap().unwrap();
    pruning.join().unwrap();
    assert_eq!(
        read_pushed_facts(state.dir()),
        [(node, "winner\n".to_owned())]
    );
}

#[test]
fn pushed_facts_admission_refuses_denied_bindings_when_pruning_is_busy() {
    let state = State::new("facts-admission-busy");
    let node = "33".repeat(16);
    let revoked = NodeIdentity::from_seed(&node, &[0x11; 32]).unwrap().key();
    let other = "44".repeat(16);
    let allowed = NodeIdentity::from_seed(&other, &[0x22; 32]).unwrap().key();
    store_pushed_facts_with_key(state.dir(), &node, Some(revoked), "revoked\n").unwrap();
    store_pushed_facts_with_key(state.dir(), &other, Some(allowed), "allowed\n").unwrap();
    let dir = state.dir().join(SWARM_FACTS_DIR);
    let _held = pushed_facts_lock(&dir, HANG, || {}).unwrap();
    // A zero deadline still attempts the lock once. Its actual contention refuses cleanup;
    // it is no elapsed-time assertion and no wait for an event that must happen.
    prune_pushed_facts_using(
        state.dir(),
        &[],
        &[revoked.hex()],
        None,
        Duration::ZERO,
        || {},
    );
    assert_eq!(read_pushed_facts(state.dir()).len(), 2);
    for (denied, log) in [
        (vec![node.clone()], None),
        (vec![revoked.hex()], None),
        (
            Vec::new(),
            Some(MemberKeys::from_iter([(
                revoked,
                Standing::Out("removed".to_owned()),
            )])),
        ),
    ] {
        let admitted = read_admitted_pushed_facts(state.dir(), &[], &denied, log.as_ref());
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].node, other);
        assert_eq!(admitted[0].payload, "allowed\n");
    }
}

/// A log this Mac cannot read now admits no stored survey and removes none: the facts it cannot
/// judge wait, unread, for a log that reads.
#[test]
fn pushed_facts_under_an_unread_log_are_neither_admitted_nor_pruned() {
    let state = State::new("facts-unread-log");
    let node = "33".repeat(16);
    let key = NodeIdentity::from_seed(&node, &[0x11; 32]).unwrap().key();
    store_pushed_facts_with_key(state.dir(), &node, Some(key), "kept\n").unwrap();
    let unread = MemberKeys::unreadable("this Mac's membership log cannot be read now");
    prune_pushed_facts(state.dir(), &[], &[], Some(&unread));
    assert_eq!(
        read_pushed_facts(state.dir()),
        [(node.clone(), "kept\n".to_owned())]
    );
    assert!(read_admitted_pushed_facts(state.dir(), &[], &[], Some(&unread)).is_empty());
    let admitted = read_admitted_pushed_facts(state.dir(), &[], &[], None);
    assert_eq!(
        admitted.iter().map(|r| r.node.as_str()).collect::<Vec<_>>(),
        [node.as_str()],
        "with no log the stored survey is admitted"
    );
}

/// A log this Mac cannot read now admits no Mac to take its survey facts: the push names why and
/// dials no one, the Mac its book names as issuer included.
#[test]
fn an_unread_log_pushes_survey_facts_to_no_one() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let me = identity(&node_of(0x61), 0x61);
    let issuer = identity(&node_of(0x62), 0x62);
    let peer = Peer {
        node: Some(issuer.node().to_owned()),
        key: Some(issuer.key()),
        key_from: None,
        host: "127.0.0.1".to_owned(),
        addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        port: listener.local_addr().unwrap().port(),
        version: None,
        seen_ms: None,
        via: Via::Issuer,
        fleet: None,
        signed: None,
    };
    let why = "this Mac's membership log cannot be read now";
    let unread = MemberKeys::unreadable(why);
    assert_eq!(
        push_facts_to_bound_peer(
            &peer,
            (&secret(9), Some(&me)),
            (&[], Some(&unread)),
            "OK 0\n",
            HANG
        ),
        Err(why.to_owned())
    );
    assert!(
        listener
            .accept()
            .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock),
        "an unread log dialed the issuer"
    );
}

#[test]
fn pushed_facts_receive_time_belongs_to_the_authoritative_payload() {
    let state = State::new("facts-received-at");
    let node = "33".repeat(16);
    let dir = state.dir().join(SWARM_FACTS_DIR);
    let legacy_time = std::time::UNIX_EPOCH + Duration::from_secs(1234);
    let record_time = std::time::UNIX_EPOCH + Duration::from_secs(5678);
    legacy_pushed_facts(state.dir(), &node, None, "legacy\n");
    std::fs::File::options()
        .write(true)
        .open(dir.join(format!("{node}.txt")))
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(legacy_time))
        .unwrap();
    store_pushed_facts(state.dir(), &node, "current\n").unwrap();
    let path = dir.join(format!("{node}.facts"));
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(record_time))
        .unwrap();
    let current = read_admitted_pushed_facts(state.dir(), &[], &[], None);
    assert_eq!(current[0].payload, "current\n");
    assert_eq!(current[0].received_at, Some(record_time));
    std::fs::remove_file(path).unwrap();
    let legacy = read_admitted_pushed_facts(state.dir(), &[], &[], None);
    assert_eq!(legacy[0].payload, "legacy\n");
    assert_eq!(legacy[0].received_at, Some(legacy_time));
}

#[test]
fn pushed_facts_capacity_counts_nodes_once_across_formats_without_following_entries() {
    let state = State::new("facts-mixed-capacity");
    let dir = state.dir().join(SWARM_FACTS_DIR);
    let old = format!("{:032x}", 0);
    let winner = format!("{:032x}", MAX_PUSHED_FACTS_FILES);
    // Construct actual legacy records cheaply; only the duplicate node has a new record.
    for n in 0..MAX_PUSHED_FACTS_FILES {
        legacy_pushed_facts(state.dir(), &format!("{n:032x}"), None, "legacy\n");
    }
    store_pushed_facts(state.dir(), &old, "duplicate current\n").unwrap();
    assert_eq!(
        pushed_facts_nodes(&dir).unwrap().len(),
        MAX_PUSHED_FACTS_FILES
    );
    let outside = state.dir().join("outside");
    write_private(state.dir(), "outside", b"outside sentinel\n").unwrap();
    let hostile = format!("{:032x}", 1);
    std::fs::remove_file(dir.join(format!("{hostile}.txt"))).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join(format!("{hostile}.txt"))).unwrap();
    // A poisoned new-record name hides its legacy entry but is never followed for age.
    std::os::unix::fs::symlink(&outside, dir.join(format!("{hostile}.facts"))).unwrap();
    store_pushed_facts(state.dir(), &winner, "winner\n").unwrap();
    assert_eq!(
        pushed_facts_nodes(&dir).unwrap().len(),
        MAX_PUSHED_FACTS_FILES
    );
    assert!(read_pushed_facts(state.dir()).contains(&(winner, "winner\n".to_owned())));
    assert_eq!(std::fs::read(outside).unwrap(), b"outside sentinel\n");

    // Cleanup never recurses into an unexpected directory, so hostile entries can exceed
    // the retention target. A successful writer still leaves its own record readable.
    let hostile_dir = dir.join(format!("{:032x}.facts", MAX_PUSHED_FACTS_FILES + 1));
    ensure_private_dir(&hostile_dir).unwrap();
    write_private(&hostile_dir, "sentinel", b"keep directory contents\n").unwrap();
    std::fs::File::open(&hostile_dir)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .unwrap();
    let winner = format!("{:032x}", MAX_PUSHED_FACTS_FILES);
    store_pushed_facts(state.dir(), &winner, "winner again\n").unwrap();
    assert_eq!(
        pushed_facts_nodes(&dir).unwrap().len(),
        MAX_PUSHED_FACTS_FILES + 1
    );
    assert_eq!(
        read_private(&hostile_dir, "sentinel", 128).unwrap(),
        b"keep directory contents\n"
    );
    assert!(read_pushed_facts(state.dir()).contains(&(winner, "winner again\n".to_owned())));
}

#[test]
fn pushed_facts_refused_inputs_preserve_the_previous_publication() {
    let state = State::new("facts-write-input-limit");
    let node = "33".repeat(16);
    store_pushed_facts(state.dir(), &node, "previous\n").unwrap();
    assert!(store_pushed_facts(state.dir(), &node, &"x".repeat(REPLY_MAX + 1)).is_err());
    assert!(store_pushed_facts(state.dir(), "../outside", "refused\n").is_err());
    assert_eq!(
        read_pushed_facts(state.dir()),
        [(node, "previous\n".to_owned())]
    );
}

#[test]
fn pushed_facts_cleanup_never_resurrects_the_older_legacy_payload() {
    let state = State::new("facts-cleanup-snapshot");
    let node = "33".repeat(16);
    legacy_pushed_facts(state.dir(), &node, None, "older legacy\n");
    store_pushed_facts(state.dir(), &node, "current\n").unwrap();
    let dir = state.dir().join(SWARM_FACTS_DIR);
    let lock = pushed_facts_lock(&dir, HANG, || {}).unwrap();
    remove_pushed_facts_using(&dir, &node, |_| {
        let observed = read_pushed_facts(state.dir());
        assert!(
            observed.is_empty() || observed == [(node.clone(), "current\n".to_owned())],
            "cleanup exposed old legacy bytes: {observed:?}"
        );
    });
    assert!(read_pushed_facts(state.dir()).is_empty());
    drop(lock);

    let legacy_dir = dir.join(format!("{node}.txt"));
    ensure_private_dir(&legacy_dir).unwrap();
    write_private(&legacy_dir, "sentinel", b"preserve this directory\n").unwrap();
    let key = NodeIdentity::from_seed(&node, &[0x11; 32]).unwrap().key();
    store_pushed_facts_with_key(state.dir(), &node, Some(key), "current retained\n").unwrap();
    let _lock = pushed_facts_lock(&dir, HANG, || {}).unwrap();
    remove_pushed_facts(&dir, &node);
    assert_eq!(
        read_pushed_facts(state.dir()),
        [(node, "current retained\n".to_owned())]
    );
    assert!(read_admitted_pushed_facts(state.dir(), &[], &[key.hex()], None).is_empty());
    assert_eq!(
        read_private(&legacy_dir, "sentinel", 128).unwrap(),
        b"preserve this directory\n"
    );
}

#[test]
fn prune_pushed_facts_evicts_log_removed_peer_via_recorded_key_sidecar_without_peers_entry() {
    let state = State::new("facts-prune-sidecar");
    let good = NodeIdentity::from_seed(&"11".repeat(16), &[0x11; 32]).unwrap();
    let revoked = NodeIdentity::from_seed(&"22".repeat(16), &[0x22; 32]).unwrap();
    let good_proof = NodeProof {
        node: good.node().to_owned(),
        key: good.key(),
    };
    let revoked_proof = NodeProof {
        node: revoked.node().to_owned(),
        key: revoked.key(),
    };
    for proof in [&good_proof, &revoked_proof] {
        let payload = format!("OK 2\naterm 0.147.0\nsurvey facts node={}\n", proof.node);
        let mut cursor = io::Cursor::new(payload.into_bytes());
        let res =
            serve_facts_with_denied(&mut cursor, Some(proof), state.dir(), (&[], None), |_| {
                Ok(Some(proof.node.clone()))
            })
            .expect("io");
        assert_eq!(res, Ok(proof.node.clone()));
    }
    assert_eq!(read_pushed_facts(state.dir()).len(), 2);

    // Even with an empty `peers` slice, `prune_pushed_facts` uses `<node>.key` to evict the
    // log-held-out peer while keeping the admitted peer (negative control).
    let log = MemberKeys::from_iter([
        (good.key(), Standing::Member),
        (revoked.key(), Standing::Out("removed".to_owned())),
    ]);
    prune_pushed_facts(state.dir(), &[], &[], Some(&log));
    let remaining = read_pushed_facts(state.dir());
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].0, good.node());
    assert!(
        !state
            .dir()
            .join(SWARM_FACTS_DIR)
            .join(format!("{}.key", revoked.node()))
            .exists()
    );
}

#[test]
fn prune_pushed_facts_uses_only_private_bounded_key_sidecars() {
    let denied = NodeIdentity::from_seed(&"11".repeat(16), &[0x11; 32])
        .unwrap()
        .key();
    let allowed = NodeIdentity::from_seed(&"22".repeat(16), &[0x22; 32])
        .unwrap()
        .key();
    let node = "33".repeat(16);
    for kind in [
        "private",
        "allowed",
        "symlink",
        "hardlink",
        "long",
        "open",
        "directory",
        "missing",
    ] {
        let state = State::new("facts-prune-key-kinds");
        legacy_pushed_facts(state.dir(), &node, None, "scratch survey facts\n");
        let dir = state.dir().join(SWARM_FACTS_DIR);
        let name = format!("{node}.key");
        let sidecar = dir.join(&name);
        let outside = state.dir().join("outside-key");
        let denied_text = format!("{}\n", denied.hex());
        aterm_uds::private_file::write_private(state.dir(), "outside-key", denied_text.as_bytes())
            .unwrap();
        match kind {
            "private" | "open" => {
                aterm_uds::private_file::write_private(&dir, &name, denied_text.as_bytes())
                    .unwrap();
                if kind == "open" {
                    mode(&sidecar, 0o644);
                }
            }
            "allowed" => aterm_uds::private_file::write_private(
                &dir,
                &name,
                format!("{}\n", allowed.hex()).as_bytes(),
            )
            .unwrap(),
            "symlink" => std::os::unix::fs::symlink(&outside, &sidecar).unwrap(),
            "hardlink" => std::fs::hard_link(&outside, &sidecar).unwrap(),
            "long" => aterm_uds::private_file::write_private(
                &dir,
                &name,
                format!("{}{}", denied.hex(), " ".repeat(4096)).as_bytes(),
            )
            .unwrap(),
            "directory" => std::fs::create_dir(&sidecar).unwrap(),
            "missing" => {}
            _ => unreachable!(),
        }
        prune_pushed_facts(state.dir(), &[], &[denied.hex()], None);
        assert_eq!(
            dir.join(format!("{node}.txt")).exists(),
            kind != "private",
            "{kind}: only a valid private denied binding authorizes this eviction"
        );
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            denied_text,
            "{kind}"
        );
    }
}

/// A FIFO can offer syntactically valid denied-key bytes, but it is never a binding file.
/// The producer uses nonblocking open so a reader that rejects it leaves no blocked writer.
#[test]
fn prune_pushed_facts_never_reads_a_fifo_key_sidecar() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};

    let state = State::new("facts-prune-key-fifo");
    let node = "44".repeat(16);
    let denied = NodeIdentity::from_seed(&"11".repeat(16), &[0x11; 32])
        .unwrap()
        .key();
    legacy_pushed_facts(state.dir(), &node, None, "scratch survey facts\n");
    let dir = state.dir().join(SWARM_FACTS_DIR);
    let sidecar = dir.join(format!("{node}.key"));
    let path = std::ffi::CString::new(sidecar.as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated scratch path; mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);

    let reading = Arc::new(AtomicBool::new(true));
    let writer = std::thread::spawn({
        let reading = reading.clone();
        let sidecar = sidecar.clone();
        let text = format!("{}\n", denied.hex());
        move || {
            let until = Instant::now() + HANG;
            while reading.load(Ordering::Acquire) {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&sidecar)
                {
                    Ok(mut file) => {
                        // A rejecting nonblocking reader may close before this write.
                        let _ = file.write_all(text.as_bytes());
                        return;
                    }
                    Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                        assert!(Instant::now() < until, "FIFO producer hang detector");
                        std::thread::yield_now();
                    }
                    Err(error) => panic!("scratch FIFO writer: {error}"),
                }
            }
        }
    });
    prune_pushed_facts(state.dir(), &[], &[denied.hex()], None);
    reading.store(false, Ordering::Release);
    writer.join().unwrap();
    assert!(
        dir.join(format!("{node}.txt")).exists(),
        "a FIFO's offered bytes are no private regular binding"
    );
    assert!(
        std::fs::symlink_metadata(sidecar)
            .unwrap()
            .file_type()
            .is_fifo()
    );
}

#[test]
fn read_pushed_facts_accepts_only_private_bounded_payloads() {
    let node = "33".repeat(16);
    let key = NodeIdentity::from_seed(&node, &[0x33; 32]).unwrap().key();
    let mut mismatches = Vec::new();
    for kind in [
        "private",
        "keyed",
        "keyring",
        "limit",
        "symlink",
        "hardlink",
        "open",
        "open-directory",
        "oversize",
        "directory",
        "missing",
        "invalid-utf8",
    ] {
        let state = State::new("facts-payload-kinds");
        let payload = if kind == "limit" {
            "x".repeat(REPLY_MAX)
        } else {
            "aterm 0.147.0\nsurvey facts source=payload-reader\n".to_owned()
        };
        legacy_pushed_facts(
            state.dir(),
            &node,
            (kind == "keyed").then_some(key),
            &payload,
        );
        let dir = state.dir().join(SWARM_FACTS_DIR);
        let name = format!("{node}.txt");
        let target = dir.join(&name);
        let outside = state.dir().join("outside-facts");
        let sentinel = "outside facts sentinel\n";
        write_private(state.dir(), "outside-facts", sentinel.as_bytes()).unwrap();
        match kind {
            "private" | "keyed" | "limit" => {}
            "keyring" => write_book(state.dir(), &[keyed(0x33, key, [10, 0, 0, 1], 7100)]),
            "symlink" | "hardlink" => {
                std::fs::remove_file(&target).unwrap();
                if kind == "symlink" {
                    std::os::unix::fs::symlink(&outside, &target).unwrap();
                } else {
                    std::fs::hard_link(&outside, &target).unwrap();
                }
            }
            "open" => mode(&target, 0o644),
            "open-directory" => mode(&dir, 0o755),
            "oversize" => std::fs::OpenOptions::new()
                .write(true)
                .open(&target)
                .unwrap()
                .set_len((REPLY_MAX as u64) + 1)
                .unwrap(),
            "directory" | "missing" => {
                std::fs::remove_file(&target).unwrap();
                if kind == "directory" {
                    std::fs::create_dir(&target).unwrap();
                }
            }
            "invalid-utf8" => write_private(&dir, &name, &[0xff]).unwrap(),
            _ => unreachable!(),
        }
        let got = read_pushed_facts(state.dir());
        let expected = if matches!(kind, "private" | "keyed" | "keyring" | "limit") {
            vec![(node.clone(), payload)]
        } else {
            Vec::new()
        };
        if got != expected {
            mismatches.push((kind, got));
        }
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            sentinel,
            "{kind}"
        );
        if kind == "keyed" {
            assert_eq!(
                read_private(&dir, &format!("{node}.key"), 128).unwrap(),
                format!("{}\n", key.hex()).as_bytes()
            );
        } else if kind == "private" || kind == "keyring" {
            assert!(!dir.join(format!("{node}.key")).exists());
        }
        if kind == "keyring" {
            assert_eq!(Keyring::read(state.dir()).bound(&node), Some(key));
        }
    }
    assert!(
        mismatches.is_empty(),
        "unsafe payloads accepted: {mismatches:?}"
    );
}

/// A producer offers finite bytes to the actual payload API, without ever blocking its open.
/// Reading those bytes from a FIFO is a policy failure, not a wall-clock latency assertion.
#[test]
fn read_pushed_facts_never_accepts_an_offered_fifo_payload() {
    for extension in ["txt", "facts"] {
        pushed_facts_fifo_payload(extension);
    }
}

fn pushed_facts_fifo_payload(extension: &str) {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
    let state = State::new("facts-payload-fifo");
    let dir = state.dir().join(SWARM_FACTS_DIR);
    ensure_private_dir(&dir).unwrap();
    let target = dir.join(format!("{}.{extension}", "44".repeat(16)));
    let path = std::ffi::CString::new(target.as_os_str().as_bytes()).unwrap();
    // SAFETY: one valid NUL-terminated scratch path; mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let reading = Arc::new(AtomicBool::new(true));
    let writer = std::thread::spawn({
        let reading = Arc::clone(&reading);
        let target = target.clone();
        move || {
            let until = Instant::now() + HANG;
            while reading.load(Ordering::Acquire) {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&target)
                {
                    Ok(mut file) => {
                        let _ = file.write_all(b"offered FIFO facts\n");
                        return;
                    }
                    Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                        assert!(Instant::now() < until, "FIFO producer hang detector");
                        std::thread::yield_now();
                    }
                    Err(error) => panic!("scratch FIFO writer: {error}"),
                }
            }
        }
    });
    let got = read_pushed_facts(state.dir());
    reading.store(false, Ordering::Release);
    writer.join().unwrap();
    assert!(got.is_empty(), "FIFO bytes accepted as facts: {got:?}");
    assert!(
        std::fs::symlink_metadata(target)
            .unwrap()
            .file_type()
            .is_fifo()
    );
}

#[test]
fn serve_join_book_and_facts_enforce_denied_keys_and_proof_bindings() {
    let state = State::new("join-denied-proof");
    issue(state.dir());
    let from = IpAddr::from(Ipv4Addr::LOCALHOST);
    let old = identity(&node_of(0xa1), 0x61);
    let new = identity(&node_of(0xa1), 0x62);
    let other = identity(&node_of(0xa2), 0x63);

    // 1. Keyless registration cannot bypass `denied_nodes` or `log.holds_out` or spoof `reg.node`
    //    when the connection's TLS handshake proved `proof`.
    let keyless_reg = Registration {
        node: old.node().to_owned(),
        host: "m1.local".to_owned(),
        port: 7100,
        version: "0.147.0".to_owned(),
        key: None,
        signed: None,
    };
    let serve_with = |reg: &Registration,
                      proved: Option<&NodeProof>,
                      denied: &[String],
                      log: Option<&MemberKeys>| {
        let mut stream = Duplex {
            input: std::io::Cursor::new(reg.line().into_bytes()),
            output: Vec::new(),
        };
        serve_join_with_denied(
            &mut stream,
            (from, proved),
            (state.dir(), &secret(5)),
            || Ok(issuer(0x11)),
            1_760_000_000_000,
            (denied, log),
        )
        .expect("stream io")
    };

    let denied_old_key = vec![old.key().hex()];
    let err = serve_with(
        &keyless_reg,
        Some(&proof(old.node(), old.key())),
        &denied_old_key,
        None,
    )
    .expect_err("denied proved key must be refused even when reg.key is None");
    assert!(err.contains("deny-list"), "{err}");

    let log_out: MemberKeys = [(old.key(), Standing::Out("retired".to_owned()))]
        .into_iter()
        .collect();
    let err = serve_with(
        &keyless_reg,
        Some(&proof(old.node(), old.key())),
        &[],
        Some(&log_out),
    )
    .expect_err("log-held-out proved key must be refused even when reg.key is None");
    assert!(err.contains(NOT_ADMITTED), "{err}");

    let err = serve_with(
        &keyless_reg,
        Some(&proof(other.node(), other.key())),
        &[],
        None,
    )
    .expect_err("mismatched proved node must be refused even when reg.key is None");
    assert!(err.contains("did not prove"), "{err}");

    // 2. When `Via::Issuer` and `.fleet-issuer-peers` bind `old.node()` to `old.key()`, which is in
    //    `denied_nodes`, a join or `merge_signed` under `new.key()` succeeds and preserves `Via::Issuer`.
    let issuer_node = node_of(0x11);
    write_book(
        state.dir(),
        &[
            Peer {
                key: Some(old.key()),
                ..issuer_entry(0xa1, 7100)
            },
            Peer {
                node: Some(issuer_node.clone()),
                ..issuer_entry(0x11, 7100)
            },
        ],
    );
    keep_issuer_book(
        state.dir(),
        &secret(5),
        Some((
            old.node(),
            &[Peer {
                node: Some(old.node().to_owned()),
                key: Some(old.key()),
                key_from: None,
                host: "m1.local".to_owned(),
                addr: None,
                port: 7100,
                version: Some("0.146.0".to_owned()),
                seen_ms: Some(1_760_000_000_000),
                via: Via::IssuerBook,
                fleet: None,
                signed: None,
            }],
        )),
    )
    .unwrap();

    let new_reg = keyless_reg.clone().signed_by(&new, 1_760_000_000_010);
    let joined = serve_with(
        &new_reg,
        Some(&proof(new.node(), new.key())),
        &denied_old_key,
        None,
    )
    .expect("new key replaces denied old key across book and issuer-book copy");
    assert_eq!(joined.0, Recorded::Updated);
    let book = read_peers(state.dir()).unwrap();
    assert_eq!(
        book.iter()
            .find(|p| p.node.as_deref() == Some(new.node()))
            .map(|p| (p.via, p.key)),
        Some((Via::Issuer, Some(new.key()))),
    );

    // 3. `serve_book_with_log` omits `own` when `own` is denied or held out by `log`.
    let own_entry = Peer::own(&old, ("m1.local", 7100, "0.147.0"), 1_760_000_000_020);
    let mut book_out = Vec::new();
    let shared = serve_book_with_log(
        &mut book_out,
        state.dir(),
        || Ok(old.node().to_owned()),
        Some(1_760_000_000_020),
        (&denied_old_key, None, Some(&own_entry)),
    )
    .unwrap()
    .unwrap();
    let book_text = String::from_utf8(book_out).unwrap();
    assert!(
        !book_text.contains(&old.key().hex()),
        "denied own entry must not be shared in book: {book_text} (shared={shared})"
    );

    // 4. `serve_facts_with_denied` accepts `new` when `old` is in `denied_nodes` and `log` admits `new`.
    write_book(
        state.dir(),
        &[Peer {
            key: Some(old.key()),
            ..keyed(0xa1, old.key(), [10, 0, 0, 1], 7100)
        }],
    );
    let log_admits_new: MemberKeys = [(new.key(), Standing::Member)].into_iter().collect();
    let facts_payload = format!("OK 1\ninstance pid=1 node={}\n", new.node());
    let mut facts_stream = Duplex {
        input: std::io::Cursor::new(facts_payload.clone().into_bytes()),
        output: Vec::new(),
    };
    let served_facts = serve_facts_with_denied(
        &mut facts_stream,
        Some(&proof(new.node(), new.key())),
        state.dir(),
        (&denied_old_key, Some(&log_admits_new)),
        |_| Ok(Some(new.node().to_owned())),
    )
    .unwrap();
    assert_eq!(served_facts, Ok(new.node().to_owned()));
}

#[test]
fn driver_and_heartbeat_hints_refuse_impostor_claiming_bound_or_own_node_id() {
    let secret_bytes = [0x67; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let (member, impostor_state) = (
        State::new("impostor-bound-member"),
        State::new("impostor-bound-hint"),
    );
    for st in [&member, &impostor_state] {
        write_key(st.dir(), &secret_bytes).unwrap();
    }

    let me = node_identity(member.dir()).unwrap();
    let founder = identity(&issuer(0x11).node, 9);
    let impostor_for_founder = identity(founder.node(), 7);
    let impostor_for_me = identity(me.node(), 8);
    let rotated_old = identity(&node_of(0xd4), 4);
    let rotated_new = identity(rotated_old.node(), 5);

    let closed = closed_port();
    let founder_peer = Peer {
        key: Some(founder.key()),
        ..issuer_entry(0x11, closed)
    };
    let rotated_peer = keyed(0xd4, rotated_old.key(), [127, 0, 0, 1], closed);
    write_book(member.dir(), &[founder_peer.clone(), rotated_peer]);

    let all = [DriveGrant::from_word(crate::fleet::ALL_MEMBERS).expect("all parses")];
    let log: MemberKeys = [
        (impostor_for_founder.key(), Standing::Member),
        (impostor_for_me.key(), Standing::Member),
        (
            rotated_old.key(),
            Standing::Out("retired old key".to_owned()),
        ),
        (rotated_new.key(), Standing::Member),
    ]
    .into_iter()
    .collect();

    // 1a. `driver_as_of` refuses a log-admitted member claiming the founder's node id when the
    // founder's key is bound in the book and not held out by `log`.
    assert_eq!(
        driver_as_of(
            Some(&proof(
                impostor_for_founder.node(),
                impostor_for_founder.key()
            )),
            (member.dir(), &all),
            Some(&log),
        ),
        Err(Undriven::Unproven(Unproven::OtherKey(
            founder.node().to_owned()
        ))),
    );

    // 1b. `driver_as_of` refuses a log-admitted member claiming this Mac's own node id under
    // another key, while admitting this Mac's own key if it is in the log.
    assert_eq!(
        driver_as_of(
            Some(&proof(impostor_for_me.node(), impostor_for_me.key())),
            (member.dir(), &all),
            Some(&log),
        ),
        Err(Undriven::Unproven(Unproven::OtherKey(me.node().to_owned()))),
    );

    // 1c. `driver_as_of` admits `rotated_new` when `rotated_old` is explicitly held out by `log`.
    assert_eq!(
        driver_as_of(
            Some(&proof(rotated_new.node(), rotated_new.key())),
            (member.dir(), &all),
            Some(&log),
        ),
        Ok(rotated_new.node().to_owned()),
    );

    // 2. `heartbeat_members_with_denied` refuses a hint (`_alab._tcp` advert) responder whose key
    // is in `log` when it claims `founder.node()`, which `member`'s book binds to `founder.key()`.
    let poisoned_member = identity(&node_of(0xe9), 12);
    write_book(
        impostor_state.dir(),
        &[Peer::own(
            &poisoned_member,
            ("127.0.0.1", 7399, "0.147.0"),
            1_760_000_000_050,
        )],
    );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let hint_port = listener.local_addr().unwrap().port();
    let named = Issuer {
        node: impostor_for_founder.node().to_owned(),
        host: "impostor.local".to_owned(),
        port: hint_port,
        version: "0.147.0".to_owned(),
    };
    let _impostor_srv = Issuing::gated(
        listener,
        (secret.clone(), Some(impostor_for_founder.clone())),
        impostor_state.dir(),
        named,
        None,
    );
    let reg = Registration {
        node: me.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: 7390,
        version: "0.147.0".to_owned(),
        key: None,
        signed: None,
    };
    let hint_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), hint_port);
    let reached = dial(
        "127.0.0.1",
        Some(hint_addr.ip()),
        hint_port,
        (&secret, Some(&me)),
        FleetOp::Join,
        HANG,
    )
    .expect("the hint responder is reachable and proves its claimed identity");
    assert_eq!(
        reached.peer,
        Some(proof(
            impostor_for_founder.node(),
            impostor_for_founder.key()
        )),
    );
    drop(reached);
    let hints_asked = AtomicBool::new(false);
    let answered = heartbeat_members_with_denied(
        member.dir(),
        (&secret, &me),
        &reg,
        HANG,
        (Some(&log), &[], &|| {
            hints_asked.store(true, Ordering::Release);
            vec![hint_addr]
        }),
    )
    .expect("heartbeat round completes");
    assert!(
        hints_asked.load(Ordering::Acquire),
        "the round asks its hints"
    );
    assert!(
        answered.is_empty(),
        "impostor hint claiming founder's bound node id must not be admitted: {answered:?}"
    );
    let book_after = read_peers(member.dir()).unwrap();
    let founder_after = book_after
        .iter()
        .find(|p| p.node.as_deref() == Some(founder.node()))
        .expect("founder entry retained");
    assert_eq!(
        founder_after.port, closed,
        "founder port must not be hijacked"
    );
    assert_eq!(
        founder_after.host, founder_peer.host,
        "founder host must not be hijacked"
    );
    assert!(
        !book_after
            .iter()
            .any(|p| p.node.as_deref() == Some(poisoned_member.node())),
        "impostor's book must not be merged"
    );
}

#[test]
fn unsigned_wire_parsers_reject_signed_numbers_and_ports() {
    assert!(parse_host_port("lab-mini:+7100", None).is_err());
    assert!(parse_host_port("[::1]:+7100", None).is_err());

    let me = identity(&node_of(0xab), 1);
    let signed_peer = Peer::own(&me, ("m7.local", 7100, "0.146.0"), 30);
    let peer_line = signed_peer.line();
    assert_eq!(
        Peer::parse(&peer_line.replace("port=7100", "port=+7100")),
        None
    );
    assert_eq!(
        Peer::parse(&peer_line.replace("seen_ms=30", "seen_ms=+1000")),
        None
    );
    assert_eq!(Peer::parse(&peer_line.replace("seq=30", "seq=+30")), None);

    assert_eq!(
        stamp_line(
            ANSWERED,
            &format!("answered node={} ms=+1000", node_of(0xab))
        ),
        None
    );

    let reg = Registration {
        node: me.node().to_owned(),
        host: "lab-mini".to_owned(),
        port: 7100,
        version: "0.146.0".to_owned(),
        key: Some(me.key()),
        signed: None,
    }
    .signed_by(&me, 30);
    let reg_line = reg.line();
    assert!(Registration::parse(&reg_line.replace("port=7100", "port=+7100")).is_err());
    assert!(Registration::parse(&reg_line.replace("seq=30", "seq=+30")).is_err());

    let join_ok = format!(
        "OK node={} host=m7.local port=7100 version=0.146.0 peers=2",
        node_of(0xab)
    );
    assert!(joined_of(&join_ok).is_ok());
    assert!(joined_of(&join_ok.replace("port=7100", "port=+7100")).is_err());
    assert!(joined_of(&join_ok.replace("peers=2", "peers=+2")).is_err());

    assert_eq!(book_head("OK 3"), Ok(3));
    assert!(book_head("OK +3").is_err());
    assert_eq!(reply_head("OK 5"), Ok(5));
    assert!(reply_head("OK +5").is_err());

    let state = State::new("signed-facts-head");
    let proof = NodeProof {
        node: me.node().to_owned(),
        key: me.key(),
    };
    let mut signed_head = Duplex {
        input: io::Cursor::new(format!("OK +1\nnode={}\n", proof.node).into_bytes()),
        output: Vec::new(),
    };
    let res = serve_facts_with_denied(
        &mut signed_head,
        Some(&proof),
        state.dir(),
        (&[], None),
        |_| Ok(Some(proof.node.clone())),
    )
    .unwrap();
    assert_eq!(res.unwrap_err(), "invalid facts head");
}

/// The dev seam raises the per-address allowance and never lowers it: a lab whose Macs share one
/// loopback address names a larger count, and anything else (none, a word, a count under four or
/// past the book's size) leaves the four.
/// NEGATIVE CONTROL: take any whole number (drop the range filter): "assertion `left == right`
/// failed: \"2\" left: 2 right: 4".
#[test]
fn the_per_address_seam_raises_the_allowance_and_never_lowers_it() {
    use std::ffi::OsStr;
    assert_eq!(joined_per_address_from(None), JOINED_PER_ADDRESS);
    assert_eq!(joined_per_address_from(Some(OsStr::new("16"))), 16);
    assert_eq!(joined_per_address_from(Some(OsStr::new(" 16\n"))), 16);
    assert_eq!(
        joined_per_address_from(Some(OsStr::new(&PEERS_MAX.to_string()))),
        PEERS_MAX
    );
    let past = (PEERS_MAX + 1).to_string();
    for word in ["2", "0", "x", "", "-5", past.as_str()] {
        assert_eq!(
            joined_per_address_from(Some(OsStr::new(word))),
            JOINED_PER_ADDRESS,
            "{word:?}"
        );
    }
    assert_eq!(
        joined_per_address(),
        JOINED_PER_ADDRESS,
        "tests read no seam"
    );
}

/// A heartbeat dial that got no answer is silence (a timeout, a host down, no route to an
/// address off this network: the home LAN's addresses from a phone hotspot) or something else
/// (a host answered and refused, or this Mac's own Local Network privacy refused a connect to an
/// address on this network), and only silence lets the network place call a remembered home
/// network one members were once on.
/// NEGATIVE CONTROL: taking every no-route as silence (`HostUnreachable | ... =>
/// on_this_network(at.ip())` made `=> false`) reads home with Local Network turned off as
/// silent, and "this Mac's privacy refused an on-link member" fails.
#[test]
fn a_dial_this_mac_or_a_host_refused_is_no_silence() {
    use std::io::{Error, ErrorKind};
    let home: IpAddr = "192.168.4.9".parse().unwrap();
    let off: IpAddr = "10.20.30.40".parse().unwrap();
    let at = |ip: IpAddr| SocketAddr::new(ip, 7100);
    let on_link = |addr: IpAddr| addr == home;
    let off_link = |_: IpAddr| false;
    let connect = |kind: ErrorKind| Unreached::Connect(at(home), Error::from(kind));
    assert!(
        dial_blocked(&connect(ErrorKind::HostUnreachable), on_link),
        "this Mac's privacy refused an on-link member"
    );
    assert!(dial_blocked(&connect(ErrorKind::PermissionDenied), on_link));
    assert!(
        !dial_blocked(
            &Unreached::Connect(at(off), Error::from(ErrorKind::HostUnreachable)),
            off_link
        ),
        "no route to an address off this network is silence"
    );
    assert!(dial_blocked(
        &connect(ErrorKind::ConnectionRefused),
        off_link
    ));
    assert!(!dial_blocked(&connect(ErrorKind::TimedOut), on_link));
    assert!(!dial_blocked(
        &Unreached::Deadline(Stage::Connect, Duration::from_secs(1)),
        on_link
    ));
    assert!(dial_blocked(
        &Unreached::Deadline(Stage::Handshake, Duration::from_secs(1)),
        off_link
    ));
    assert!(dial_blocked(
        &Unreached::Unproved {
            at: at(off),
            node: "n".to_owned()
        },
        off_link
    ));
}

/// What a check reads of the instance that answered ([`Answerer::of`]): a window's version where
/// its first `instance` line says `window=1` or no `window=` word (an older aterm's), `Windowless`
/// for `window=0`, and nothing for a line with no version word, a version that is not one (a
/// peer's escape bytes never reach the `peers` table), or a `window=` word other than 0 or 1.
/// NEGATIVE CONTROL: an `of` that reads `version=` whatever `window=` says fails "a node job with
/// no window open"; one that keeps a version unchecked fails "an escape is no version".
#[test]
fn a_check_reads_which_instance_answered_from_its_instance_line() {
    let of =
        |lines: &[&str]| Answerer::of(&lines.iter().map(|l| (*l).to_owned()).collect::<Vec<_>>());
    let window = |v: &str| Answerer::Window(v.to_owned());
    assert_eq!(
        of(&["instance pid=7 host=m16 version=0.146.0 sessions=6 node=n window=1 window_last=9"]),
        window("0.146.0"),
        "a window"
    );
    assert_eq!(
        of(&["instance pid=7 host=m version=0.147.0 sessions=0 node=n instances=0 window=0"]),
        Answerer::Windowless,
        "a node job with no window open"
    );
    assert_eq!(
        of(&["instance pid=1 host=m version=0.95.0 sessions=0 node=n"]),
        window("0.95.0"),
        "an older aterm's window says no window= word"
    );
    assert_eq!(
        of(&["instance pid=1 host=m version=0.148.0%2Bg5bb7817df4 window=1"]),
        window("0.148.0+g5bb7817df4"),
        "a version is pct-decoded"
    );
    assert_eq!(
        of(&["instance pid=1 host=m version=%1B%5B2J window=1"]),
        Answerer::Unsaid,
        "an escape is no version"
    );
    assert_eq!(
        of(&["instance pid=1 host=m window=1"]),
        Answerer::Unsaid,
        "no version"
    );
    assert_eq!(
        of(&["instance pid=1 version=0.1.0 window=2"]),
        Answerer::Unsaid,
        "a window= word other than 0 or 1"
    );
    assert_eq!(of(&["s1 phase=idle"]), Answerer::Unsaid, "no instance line");
    assert_eq!(
        of(&[
            "instance pid=1 version=0.146.0 window=1",
            "instance pid=2 version=0.147.0 window=1"
        ]),
        window("0.146.0"),
        "the first instance line"
    );
}

/// Two nodes whose ids share their first eight characters never print alike in one message: each
/// is named by the shortest head no other node it is named beside shares, up to its whole id.
///
/// NEGATIVE CONTROL: name every node by its first eight characters again, and the two twins below
/// print as `c0ffee34` alike, in the head, the misnamed and the other-node lines, a book read's
/// and a refusal.
#[test]
fn a_node_is_named_by_the_shortest_head_no_other_node_shares() {
    let a = "c0ffee34a18b0000000000000000000a";
    let b = "c0ffee34a27c0000000000000000000b";
    let c = "26c4dc354c6726da031051adc10bc1bd";
    assert_eq!(node_head(c, &[a, b, c]), "26c4dc35");
    assert_eq!(node_head(a, &[a, b, c]), "c0ffee34a1");
    assert_eq!(node_head(b, &[a, b]), "c0ffee34a2");
    assert_eq!(node_head(a, &[] as &[&str]), "c0ffee34");
    let twin = format!("{}f", &a[..31]);
    assert_eq!(
        node_head(a, &[twin]),
        a,
        "the whole id where every shorter head is shared"
    );
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, 1));
    let misnamed = Unreached::Misnamed {
        at,
        proved: a.to_owned(),
        claimed: b.to_owned(),
    };
    assert!(
        misnamed
            .to_string()
            .contains("proved node c0ffee34a1, its survey says node c0ffee34a2"),
        "{misnamed}"
    );
    let other = Unreached::OtherNode {
        at,
        said: a.to_owned(),
        want: b.to_owned(),
    };
    assert!(
        other.to_string().starts_with(
            "127.0.0.1 is node c0ffee34a1 now: rejoin this peer, or `aterm fleet peers rm \
             c0ffee34a2`"
        ),
        "{other}"
    );
    // A book read answered at the issuer's address by a node whose id begins as the issuer's.
    let book = BookUnread::OtherNode {
        at,
        said: a.to_owned(),
        want: b.to_owned(),
    };
    assert_eq!(
        book.to_string(),
        "127.0.0.1:1 answered as node c0ffee34a1, not this Mac's issuer"
    );
    for (refusal, head) in [
        (
            Undriven::Unlisted(a.to_owned()),
            "node c0ffee34a1 is not one the dialled Mac lets drive it",
        ),
        (
            Undriven::Unproven(Unproven::Unbound(a.to_owned())),
            "node c0ffee34a1 is bound to no key",
        ),
    ] {
        let said = refusal.said(&[a, b]);
        assert!(said.starts_with(head), "{said}");
        assert!(
            refusal
                .to_string()
                .starts_with(&head.replace("c0ffee34a1", "c0ffee34")),
            "with no other node known, eight: {refusal}"
        );
    }
}

/// `addresses_within` hands back what a dial's lookup walks: an IP literal as itself, and nothing
/// for a name the loopback guard refuses or no time is left to look up.
#[test]
fn addresses_within_is_the_dials_lookup() {
    assert_eq!(
        addresses_within("192.168.1.237", Duration::ZERO),
        [IpAddr::from([192, 168, 1, 237])]
    );
    assert!(addresses_within("m22.local", Duration::ZERO).is_empty());
    let guarded = crate::loopback::forced(true, || addresses_within("m22.local", HANG));
    assert!(guarded.is_empty(), "{guarded:?}");
}

#[path = "fleet_member_tests/address_fallback.rs"]
mod address_fallback;

#[path = "fleet_member_tests/heartbeat_descriptor.rs"]
mod heartbeat_descriptor;

#[path = "fleet_member_tests/book_fallback.rs"]
mod book_fallback;
