// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for fleet membership join codes, peer book persistence, and loopback join/reach flows.

use super::*;
use crate::fleet::{FleetEvent, Listening, read_key_file, serve};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const HANG: Duration = Duration::from_secs(60);

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
        assert_eq!(
            refused_at_once(state.dir()).kind(),
            io::ErrorKind::WouldBlock
        );
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

#[test]
fn a_leave_goes_on_unlocked_only_where_no_edit_could_lock() {
    let state = State::new("leave-lock");
    let dir = state.dir();
    let lock = dir.join(PEERS_LOCK);
    std::fs::create_dir(&lock).unwrap();
    assert_eq!(leave_held(dir, |_| 7), Ok(7), "a lock that is no file");
    std::fs::remove_dir(&lock).unwrap();
    let linked = dir.parent().unwrap().join("linked-state");
    std::os::unix::fs::symlink(dir, &linked).unwrap();
    assert_eq!(leave_held(&linked, |_| 7), Ok(7), "a symlink");
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
        Some(&grants[0])
    );
    let rekeyed = identity(&node_of(0xa2), 4).key();
    assert_eq!(
        DriveGrant::admitting(std::slice::from_ref(&named), member.node(), rekeyed),
        None
    );
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
                serve(
                    &listener,
                    &listening,
                    &running,
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
                serve(
                    &listener,
                    &listening,
                    &running,
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
            peers: 1
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
            Err(BookUnread::OtherNode(_, node)) if node == issuer(0x11).node
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
                (host, addr, 7100),
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
    assert!(guard(&walked, "a dial of 10.0.0.1:7100"), "{walked:?}");
    assert!(
        dialled.is_empty() && names.is_empty(),
        "{dialled:?} {names:?}"
    );
    let (walked, dialled, _) = walk_as(false, "10.0.0.1", Some(private), true);
    assert!(walked.is_ok(), "the control: {walked:?}");
    assert_eq!(dialled, [SocketAddr::new(private, 7100)]);
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
        matches!(walked, Err(Unreached::Connect(at, _)) if at == SocketAddr::new(loopback, 7100)),
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
    assert_eq!(dialled, [SocketAddr::new(loopback, 7100)]);
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
                serve(
                    &listener,
                    &listening,
                    &running,
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
            serve(
                &listener,
                &listening,
                &running,
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
            serve(
                &listener,
                &listening,
                &running,
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

/// Resolving `_alab._tcp` advert hints stays within `within` and looks places up concurrently: a
/// hanging lookup on the first advert neither holds up the round past `within` nor starves a
/// responsive advert after it, and the loopback guard skips off-loopback lookups and addresses.
/// NEGATIVE CONTROL: resolve sequentially with no timeout (`to_socket_addrs` in a loop): the first
/// advert blocks for the full hang duration and starves the second when the budget expires.
#[test]
fn resolving_advert_hints_stays_within_its_budget_and_keeps_places_after_a_hanging_lookup() {
    static RELEASE_HANG: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>> =
        std::sync::Mutex::new(None);
    let (tx, rx) = std::sync::mpsc::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    *RELEASE_HANG.lock().unwrap() = Some(tx);
    static RECV_HANG: std::sync::Mutex<
        Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    > = std::sync::Mutex::new(None);
    *RECV_HANG.lock().unwrap() = Some((ready_tx, rx));

    let resolver: Resolver = |name, port| match name {
        "hanging.local" => {
            let pair = RECV_HANG.lock().unwrap().take();
            if let Some((ready, rx)) = pair {
                let _ = ready.send(());
                let _ = rx.recv_timeout(Duration::from_secs(5));
            }
            vec![SocketAddr::from(([10, 0, 0, 99], port))]
        }
        "m22.local" => vec![SocketAddr::from(([10, 0, 0, 22], port))],
        _ => Vec::new(),
    };
    let places = vec![
        ("hanging.local".to_owned(), 7100),
        ("m22.local".to_owned(), 7101),
        ("10.0.0.8".to_owned(), 7102),
    ];
    let started = Instant::now();
    let resolved = crate::loopback::forced(false, || {
        resolve_ipv4_hints_with(&places, Duration::from_millis(120), resolver)
    });
    let elapsed = started.elapsed();
    ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("hanging resolver started");
    if let Some(tx) = RELEASE_HANG.lock().unwrap().take() {
        let _ = tx.send(());
    }
    assert_eq!(
        resolved,
        vec![
            SocketAddr::from(([10, 0, 0, 22], 7101)),
            SocketAddr::from(([10, 0, 0, 8], 7102)),
        ],
        "responsive advert after a hanging one still resolves within the budget"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "hint resolution must stay bounded by its budget, took {elapsed:?}"
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
        leading_machine_label("m22-Macbook-M5-Max-128GB-8TB.local"),
        Some("m22")
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
            crate::fleet::serve_offering_with_denied(
                &tcp_listener,
                &listening,
                &|| (FleetScope::Content, EVERY_OP),
                &move || Arc::clone(&deny),
                &running,
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
    // The three joined members answered last, and the round's last dial of the issuer missed, so
    // the joined members are the round's three.
    hold_book(b_state.dir())
        .unwrap()
        .note_stamps(
            &[a.node(), c.node(), d.node()].map(|node| (format!("node={node}"), 5_000)),
            &[(format!("node={}", e.node()), 6_000)],
        )
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
/// that answered last no longer spends every round's time and its hints, round after round.
/// NEGATIVE CONTROL: order the round by when each member last answered, and dial each by the
/// round's deadline (F17 as landed): D's dial carries the whole round, and the next round dials D
/// first again.
#[test]
fn a_member_gone_dark_waits_behind_the_members_that_answer_and_spends_only_its_share() {
    let secret_bytes = [0x5a; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let [b, d, c1, c2, c3] =
        [0x51, 0x52, 0x53, 0x54, 0x55].map(|seed| identity(&node_of(seed), seed));
    let log: MemberKeys = [&b, &d, &c1, &c2, &c3]
        .map(|who| (who.key(), Standing::Member))
        .into_iter()
        .collect();
    let b_state = State::new("dark-b");
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

    // The next round, with members that answer: D, which answered last but missed since, waits
    // behind them, and the round fills before it.
    write_book(
        b_state.dir(),
        &[
            entry(&d, d_at.port()),
            entry(&c1, serving[0].0.port),
            entry(&c2, serving[1].0.port),
            entry(&c3, serving[2].0.port),
        ],
    );
    let held = HeldSilent::at(d_at);
    let answered = round();
    let mut nodes: Vec<&str> = answered.iter().map(|done| done.node.as_str()).collect();
    nodes.sort_unstable();
    assert_eq!(nodes, [c1.node(), c2.node(), c3.node()]);
    assert_eq!(
        held.overrode(),
        None,
        "D, gone dark, was dialled before the members that answer"
    );
    drop(d_listener);
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
        .note_stamps(&[], &[(answer_key(&one), 6_000)])
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
}
