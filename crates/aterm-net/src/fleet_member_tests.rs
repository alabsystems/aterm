// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet membership's tests: the join code's checks, the book's files and
//! rules, and a join and a reach over real loopback fleet listeners.

use super::*;
use crate::fleet::{FleetEvent, Listening, read_key_file, serve};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// A wait for something that must happen: a hang detector, never a budget.
const HANG: Duration = Duration::from_secs(60);

fn secret(byte: u8) -> FleetSecret {
    FleetSecret::new(&[byte; SECRET_LEN]).unwrap()
}

fn mode(path: &Path, bits: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
}

/// A private state directory of this test's own, removed on drop.
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

fn code() -> JoinCode {
    let mut secret = [0u8; SECRET_LEN];
    for (at, byte) in secret.iter_mut().enumerate() {
        *byte = (at as u8).wrapping_mul(37).wrapping_add(11);
    }
    JoinCode {
        secret,
        host: "m7.local".to_owned(),
        port: 7100,
    }
}

/// The code's own characters, with `-` between groups, and its issuer part.
fn split(text: &str) -> (Vec<char>, String) {
    let body = text.strip_prefix(CODE_PREFIX).unwrap();
    let (chars, issuer) = body.rsplit_once('@').unwrap();
    (
        chars.chars().filter(|c| *c != '-').collect(),
        issuer.to_owned(),
    )
}

fn join(chars: &[char], issuer: &str) -> String {
    format!("{CODE_PREFIX}{}@{issuer}", chars.iter().collect::<String>())
}

/// A code reads back as itself, from what a person might paste: another case,
/// no group dashes, white space around, Crockford's look-alikes; and its
/// issuer may be a name, an IPv4 or an IPv6 address.
#[test]
fn a_join_code_reads_back_as_itself() {
    let code = code();
    let text = code.text();
    assert!(text.starts_with(CODE_PREFIX), "{text}");
    let (chars, issuer) = split(&text);
    assert_eq!(chars.len(), CODE_SYMBOLS);
    assert_eq!(issuer, "m7.local:7100");
    let groups = text[CODE_PREFIX.len()..text.find('@').unwrap()].split('-');
    assert!(groups.clone().all(|group| group.len() == GROUP), "{text}");
    assert_eq!(groups.count(), CODE_SYMBOLS / GROUP);

    assert_eq!(JoinCode::parse(&text), Ok(code.clone()));
    assert_eq!(
        JoinCode::parse(&format!("  {}\n", text.to_ascii_uppercase())),
        Ok(JoinCode {
            host: "M7.LOCAL".to_owned(),
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
            host: host.to_owned(),
            ..code.clone()
        };
        assert_eq!(JoinCode::parse(&code.text()), Ok(code));
    }
    assert!(
        !format!("{code:?}").contains(&chars.iter().collect::<String>()[..8]),
        "a code's Debug shows no secret"
    );
}

/// A CODE IS FOUND WHEREVER IT SITS (what the survey's redaction replaces):
/// whole as printed, its secret's groups without the `atf1-`, in capitals,
/// with spaces for its dashes, without dashes at all, and its issuer taken
/// with it, the words around it left out of the span. NEGATIVE CONTROLS: the
/// same characters with one mistyped (no codeword), a run one character
/// short, and an ordinary sentence of code characters hold none.
#[test]
fn a_join_code_is_found_wherever_it_sits() {
    let text = code().text();
    let body = &text[CODE_PREFIX.len()..text.find('@').unwrap()];
    // The one span `t` holds, as text; `None` for none or several.
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

/// THE CHECK CHARACTERS: every one-character typo at every place — each of
/// the alphabet's other 31 characters — and every swap of two different
/// neighbours is refused as a typo, so no mistyped code becomes a wrong
/// secret written to disk.
#[test]
fn every_one_character_typo_and_neighbour_swap_is_refused() {
    let (chars, issuer) = split(&code().text());
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

/// A text that is not a whole code is refused by what is wrong with it, and
/// no refusal quotes a character of it.
#[test]
fn a_code_that_is_not_whole_is_refused_by_what_is_wrong() {
    let text = code().text();
    let (chars, issuer) = split(&text);
    let cases = [
        (text.replacen("atf1-", "atf2-", 1), CodeError::Prefix),
        (
            text.split('@').next().unwrap().to_owned(),
            CodeError::NoIssuer,
        ),
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
        (join(&chars, "m7.local"), CodeError::Issuer("no port")),
        (
            join(&chars, "m7.local:0"),
            CodeError::Issuer("a port is 1 to 65535"),
        ),
        (
            join(&chars, "m7 local:7100"),
            CodeError::Issuer(
                "a host is an address or a name of letters, digits, `.`, `-` and `_`",
            ),
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

/// The key file `init` and `join` write is the one the listener reads, and
/// the node id is minted once and kept.
#[test]
fn the_key_and_the_node_id_are_written_as_their_readers_read_them() {
    let state = State::new("key");
    write_key_file(state.dir(), &[0x3e; SECRET_LEN]).unwrap();
    assert_eq!(read_key_file(state.dir()).unwrap(), secret(0x3e));
    let meta = std::fs::metadata(state.dir().join(KEY_FILE)).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert!(
        write_key_file(state.dir(), &[1; 8]).is_err(),
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

/// THE NODE KEY is minted once, beside the node id, and kept: written 0600 as
/// its seed's 64 hex digits and a newline, read back as the same key, never
/// minted by a reader, and a broken file is refused rather than re-minted —
/// a new key would be another node to the issuer that bound the old one. A
/// held membership claim refuses a mint without changing either identity
/// file, before and after a winner exists; releasing it permits the read.
#[test]
fn a_node_key_is_minted_once_and_read_back_as_itself() {
    let state = State::new("nodekey");
    assert!(read_node_identity(state.dir()).unwrap().is_none());
    {
        let _claim = BookLock::take(state.dir(), Duration::ZERO).unwrap();
        assert_eq!(
            node_identity(state.dir()).unwrap_err().kind(),
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
            node_identity(state.dir()).unwrap_err().kind(),
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
            node_identity(state.dir()).unwrap_err().kind(),
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

/// `ensure_private_dir` makes a missing state directory 0700, narrows one
/// open to others and says what it was, and refuses a symlink; a writer
/// refuses a directory that is not private rather than repair it.
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
    assert!(write_key_file(&linked, &[2; SECRET_LEN]).is_err());
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
    }
}

/// `peer` as a member's copy of its issuer's book holds it.
fn copy(peer: Peer) -> Peer {
    Peer {
        via: Via::IssuerBook,
        ..peer
    }
}

/// A node id of one repeated byte.
fn node_of(byte: u8) -> String {
    format!("{byte:02x}").repeat(16)
}

/// The node `node` with a key from one repeated seed byte: another seed is
/// another key for the same node id.
fn identity(node: &str, seed: u8) -> NodeIdentity {
    NodeIdentity::from_seed(node, &[seed; 32]).unwrap()
}

/// What a connection's peer proved: `node` under `key`.
fn proof(node: &str, key: NodeKey) -> NodeProof {
    NodeProof {
        node: node.to_owned(),
        key,
    }
}

/// [`joined`], bound to `key`.
fn keyed(node: u8, key: NodeKey, ip: [u8; 4], port: u16) -> Peer {
    Peer {
        key: Some(key),
        ..joined(node, "m7.local", ip, port)
    }
}

/// A PEER'S KEY IS ON ITS LINE, after its node id, and reads back as
/// itself; a line an older aterm wrote, with no `key=`, binds none. A key is
/// a node id's, so one without a node id is refused, and so is one that is
/// no key.
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

    // Where an unbound node's key may be bound from is on its line, after its
    // key, and only there: a bound node's is dropped, as is one with no node.
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

/// A peer is one line and reads back as itself; a line that is not a peer
/// (a hand edit gone wrong, a host that could carry a control character) is
/// refused by its line number.
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

/// THE BOOK IS PRIVATE: read only when it and its directory are this user's
/// alone — a group or other bit on either, a second link, a symlink at
/// either is refused — and no file at all is an empty book.
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

/// The book is keyed by node id: a registration replaces its node's entry and
/// a person's entry for the same place, a person's peer is added once, and a
/// full book refuses a newcomer rather than evict a member.
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
    // Bound to no key, it binds one only from where it was recorded.
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

/// ONE ADDRESS, A FEW MEMBERS, NONE EVICTED: a registration from an address
/// and port the book holds for another node — on DHCP, an address that
/// passed from a machine that moved — is an entry beside it, never in its
/// place, and the machine that moved keeps its entry until it joins again
/// from where it is now. Every node registered from an address counts
/// toward its share, so an address that has registered its share is refused
/// a node more: a made-up one, or a member's moved there by its node id,
/// whose entry stays where it was. A member of that address re-joins, and
/// another address still registers.
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

/// A REGISTRATION STANDS FOR A PERSON'S ENTRY AT ITS OWN PLACE ONLY: the
/// address and port it registered from, never the host name it gives itself,
/// which any holder of the secret writes as it likes. The issuer a member
/// records stands for the person's entry of its name, `lab-mini` as
/// `lab-mini.local`; a person's peer by that bare name is in the book
/// already, while two dotted names are two machines.
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

/// A NODE IS BOUND TO THE KEY IT FIRST REGISTERS WITH (trust on first use):
/// a registration that proves that key brings its entry up to date from
/// wherever it is; another key, or none, is refused, naming `aterm fleet
/// peers rm <node>` on the issuer, and changes nothing — after which a new
/// key binds. An entry an older aterm recorded with no key is bound at its
/// next registration from the address it was recorded from, never from
/// another, and one that proves no key brings it up to date as before.
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
            why.contains(says) && why.contains("`aterm fleet peers rm abababab` on the issuer"),
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
        why.contains("recorded from 10.0.0.8") && why.contains("peers rm cdcdcdcd"),
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

    // A registration that proves no key moves an older entry — an older
    // member's new lease — and never where its key may be bound from: else a
    // secret holder would move it to its own address keyless, then bind its
    // own key there in a second registration.
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
    // As the book's file holds it, between one registration and the next.
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
    // NEGATIVE CONTROL: from the address it was recorded from, its key is
    // bound, wherever a keyless move left the entry.
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

/// THE PROVEN NODE is the one a connection proved under the key this
/// machine binds it to; a claim is never one — no proof, a node bound to no
/// key, another key. The keys are the machine's own book's, then its copy
/// of its issuer's book's, its own first; a book that cannot be read binds
/// nothing, so every peer reads unproven, never proven.
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
    write_issuer_book(
        state.dir(),
        i.node(),
        &[
            copy(keyed(0xa1, b.key(), [10, 0, 0, 7], 7100)),
            copy(keyed(0xb2, b.key(), [10, 0, 0, 8], 7102)),
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
    mode(&state.dir().join(PEERS_FILE), 0o644);
    assert_eq!(Keyring::read(state.dir()), Keyring::default());

    // A person's entry at a copy's place is read in the copy's stead, and the
    // copy's key still binds its node: whatever answers that entry as it is
    // held to that key.
    let n = identity(&node_of(0xc3), 6);
    let state = State::new("ringplace");
    let issuer = Peer {
        key: Some(i.key()),
        ..issuer_entry(0x11, 7100)
    };
    write_book(state.dir(), &[issuer, manual("m7", 7100)]);
    let at_m7 = copy(keyed(0xc3, n.key(), [10, 0, 0, 7], 7100));
    write_issuer_book(state.dir(), i.node(), std::slice::from_ref(&at_m7)).unwrap();
    let mut read = read_peers(state.dir()).unwrap();
    with_copies(&mut read, [at_m7.clone()]);
    assert_eq!(read.len(), 2, "the person's entry stands for the copy");
    assert_eq!(
        Keyring::of(&read).bound(n.node()),
        None,
        "a read's targets alone have lost the copy's key"
    );
    let ring = Keyring::read(state.dir());
    assert_eq!(ring.bound(n.node()), Some(n.key()));
    assert_eq!(
        ring,
        Keyring::of(read.iter().chain([&at_m7])),
        "the book and its copy, before a read leaves the copy out"
    );
    // What names a node to grant or drive it finds the node the person's
    // entry stands for in a read ([`bound_peers`]); a read's targets name
    // the person's entry, which has none.
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
    assert_eq!(find(&read, "m7"), Ok(Some(1)));
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

/// A KEY THE ISSUER NO LONGER PROVES IS TAKEN OUT ON THE MEMBER: the
/// issuer's book never holds itself, so an answer that claims the issuer's
/// node under another key names `peers rm` here and a join again; any other
/// node's, `peers rm` on the issuer. Each within a survey row's room.
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

/// A DRIVE GRANT HOLDS THE KEY THE PERSON APPROVED: the driver is the node
/// the book binds to the key it proved AND a grant names with that key. A
/// node id the book has bound afresh to another key since — the issuer's
/// `peers rm`, and a join again under that id — drives nothing, though the
/// book now calls it proven (the NEGATIVE CONTROL beside the admitted
/// driver); so does a node no grant names, and a bare node id grants no one.
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
    // Rebound: the book binds A's id to M's key now, and M proves it.
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
        approved.driver(None, &grants),
        Err(Undriven::Unproven(Unproven::NoProof))
    );
}

/// A DRIVER AN OLD COPY ALONE BINDS IS CHECKED AGAINST THE ISSUER'S BOOK
/// ([`proven_driver`]): on a member, a node only its copy of the issuer's
/// book binds is admitted against that copy while it is under
/// [`COPY_DRIVES_FOR`] old; past it the issuer's book is read again first,
/// over a real loopback issuer that proves its key: a book that still binds
/// the node admits it, and the issuer's `peers rm` of the node refuses it,
/// the copy following. An issuer that cannot be reached, or one this Mac
/// binds no key to, refuses it, saying why and what to do, and leaves the
/// copy. FAILS WITHOUT THE FIX: the old copy admits the node the issuer
/// took out. NEGATIVE CONTROLS: a young copy admits with no read, and the
/// member's own issuer — bound by its own book, recorded at its join — is
/// admitted however old the copy, with no read.
#[test]
fn a_driver_an_old_copy_alone_binds_is_checked_against_the_issuers_book() {
    let issuer_state = State::new("recheck-issuer");
    issue(issuer_state.dir());
    let i = identity(&issuer(0x11).node, 9);
    let n = identity(&node_of(0xd4), 4);
    let entry = keyed(0xd4, n.key(), [10, 0, 0, 4], 7100);
    write_book(issuer_state.dir(), std::slice::from_ref(&entry));
    let issuing = Issuing::proving(secret(5), issuer_state.dir(), issuer(0x11), i.clone());

    let member = State::new("recheck-member");
    let me = identity(&node_of(0x77), 7);
    let issuer_at = |port, key| Peer {
        key,
        ..issuer_entry(0x11, port)
    };
    write_book(member.dir(), &[issuer_at(issuing.port, Some(i.key()))]);
    write_issuer_book(member.dir(), i.node(), &[copy(entry.clone())]).unwrap();
    let grants = [
        DriveGrant {
            node: n.node().to_owned(),
            key: n.key(),
        },
        DriveGrant {
            node: i.node().to_owned(),
            key: i.key(),
        },
    ];
    let young = || std::time::SystemTime::now() + Duration::from_secs(1);
    let old = || std::time::SystemTime::now() + COPY_DRIVES_FOR + Duration::from_secs(1);
    let fetched = |issuer: &Target| {
        fetch_book(
            issuer,
            (&secret(5), Some(&me)),
            (Instant::now() + HANG, HANG),
        )
    };
    let unread = |_: &Target| -> Result<Book, BookUnread> {
        panic!("a young copy, or the book's own binding, reads no book")
    };
    let as_of = |who: &NodeIdentity| Some(proof(who.node(), who.key()));
    let copied = || read_issuer_book(member.dir(), i.node());

    assert_eq!(
        driver_as_of(as_of(&n).as_ref(), (member.dir(), &grants), young(), unread),
        Ok(n.node().to_owned()),
        "a young copy admits with no read"
    );
    assert_eq!(
        driver_as_of(as_of(&i).as_ref(), (member.dir(), &grants), old(), unread),
        Ok(i.node().to_owned()),
        "the member's own issuer, however old the copy"
    );
    assert_eq!(
        driver_as_of(as_of(&n).as_ref(), (member.dir(), &grants), old(), fetched),
        Ok(n.node().to_owned()),
        "the issuer's book still binds it"
    );

    // The issuer's `peers rm`.
    write_book(issuer_state.dir(), &[]);
    assert_eq!(
        driver_as_of(as_of(&n).as_ref(), (member.dir(), &grants), old(), fetched),
        Err(Undriven::Unproven(Unproven::Unbound(n.node().to_owned()))),
        "taken out on the issuer: refused"
    );
    assert!(copied().is_empty(), "the copy follows the issuer's book");
    assert_eq!(
        driver_as_of(as_of(&n).as_ref(), (member.dir(), &grants), young(), unread),
        Err(Undriven::Unproven(Unproven::Unbound(n.node().to_owned()))),
        "and refuses by itself from then"
    );

    // An old copy that still binds it, and no issuer answers: refused, and
    // the copy is left as it was.
    write_issuer_book(member.dir(), i.node(), &[copy(entry.clone())]).unwrap();
    write_book(member.dir(), &[issuer_at(closed_port(), Some(i.key()))]);
    let refused = driver_as_of(as_of(&n).as_ref(), (member.dir(), &grants), old(), fetched);
    assert!(
        matches!(&refused, Err(Undriven::Unchecked { node, .. }) if node == n.node()),
        "{refused:?}"
    );
    let said = refused.unwrap_err().to_string();
    assert!(
        said.contains("could not read that book again") && said.contains("aterm fleet peers"),
        "{said}"
    );
    assert_eq!(copied(), vec![copy(entry.clone())]);
    // An issuer this Mac binds no key to answers with a claim: no book binds.
    write_book(member.dir(), &[issuer_at(issuing.port, None)]);
    let claimed = driver_as_of(as_of(&n).as_ref(), (member.dir(), &grants), old(), fetched);
    assert!(
        matches!(&claimed, Err(Undriven::Unchecked { why, .. }) if why.contains("no key to its issuer")),
        "{claimed:?}"
    );
}

/// `rm` takes the one peer a node id, a host, a host's first label or an
/// address names, and names every candidate when more than one answers.
#[test]
fn rm_takes_one_peer_or_names_the_candidates() {
    let a = joined(0xaa, "m7.local", [10, 0, 0, 7], 7100);
    let b = joined(0xbb, "m7.lan", [10, 0, 0, 8], 7100);
    let mut book = vec![a.clone(), b.clone(), manual("lab-mini", 7100)];
    let ambiguous = remove(&mut book, &[], "m7").unwrap_err();
    assert!(
        ambiguous.contains("names 2 peers") && ambiguous.contains("aaaaaaaa"),
        "{ambiguous}"
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

/// Every op this aterm's listener serves at the read levels.
const EVERY_OP: &[FleetOp] = &[FleetOp::Survey, FleetOp::Book, FleetOp::Join];

/// The ops an older aterm's listener serves: no `book`.
const OLDER_OPS: &[FleetOp] = &[FleetOp::Survey, FleetOp::Join];

/// A loopback fleet listener with `secret`, its `join`s served by
/// [`serve_join`] into `state` as `issuer`, its `book` reads by
/// [`serve_book`] from `state`, and its surveys answered naming `issuer`'s
/// node, each once its hook has run; stopped on drop.
struct Issuing {
    port: u16,
    running: Arc<AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
}

impl Issuing {
    fn start(secret: FleetSecret, state: &Path, issuer: Issuer) -> Self {
        Self::serving(secret, state, issuer, EVERY_OP)
    }

    /// [`Issuing::start`] serving only `serves`: [`OLDER_OPS`] is an older
    /// aterm.
    fn serving(
        secret: FleetSecret,
        state: &Path,
        issuer: Issuer,
        serves: &'static [FleetOp],
    ) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        Self::on(listener, secret, state, issuer, serves)
    }

    /// [`Issuing::serving`] on `listener`.
    fn on(
        listener: TcpListener,
        secret: FleetSecret,
        state: &Path,
        issuer: Issuer,
        serves: &'static [FleetOp],
    ) -> Self {
        Self::hooked(listener, secret, state, issuer, serves, Arc::new(|_| None))
    }

    /// [`Issuing::start`] proving its node `me` (`issuer`'s) to each dialer.
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

    /// [`Issuing::on`], each survey or book answered once `before` has run
    /// with its op — with the answer `before` gives in place of its own,
    /// when it gives one.
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

    /// [`Issuing::hooked`], proving the node `me` when there is one.
    fn run(
        listener: TcpListener,
        (secret, me): (FleetSecret, Option<NodeIdentity>),
        state: &Path,
        issuer: Issuer,
        serves: &'static [FleetOp],
        before: Arc<dyn Fn(FleetOp) -> Option<&'static str> + Send + Sync>,
    ) -> Self {
        let port = listener.local_addr().unwrap().port();
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
                                return serve_book(stream, &state, move || Ok(node)).map(|_| ());
                            }
                            if op != FleetOp::Join {
                                // A survey: its header names this node.
                                return answer_survey(stream, &issuer.node);
                            }
                            let issuer = Issuer::clone(&issuer);
                            serve_join(
                                stream,
                                (from, proof.as_ref()),
                                &state,
                                move || Ok(issuer),
                                1_760_000_000_000,
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
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// Answer one `survey` request (any level) with a header naming `node` and no
/// session.
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

/// Make `state` an issuer's: the join code `aterm fleet init` writes there.
fn issue(state: &Path) {
    write_private(state, CODE_FILE, format!("{}\n", code().text()).as_bytes()).unwrap();
}

/// Dial `port` on loopback for a join with `secret` — the dial and the
/// proofs must succeed, so a refusal is the issuer's answer, never a failed
/// dial — and send `reg`: the issuer's answer.
fn join_over(port: u16, secret: &FleetSecret, reg: &Registration) -> Result<Joined, String> {
    join_as(port, (secret, None), reg).0
}

/// [`join_over`], proving the node `me`: the answer, and the node the issuer
/// proved.
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
    (register(reached.transport.stream(), reg), reached.peer)
}

/// A JOIN REGISTERS: over a real fleet listener on loopback, a member that
/// proves the secret for `join` is recorded in the issuer's book by node id,
/// with the address it came from, and the answer names the issuer; joining
/// again updates its one entry; the issuer's own node id is refused; a member
/// of another fleet is refused before it sends a byte of its record; and a
/// machine that holds no join code — a member, not the issuer — records no
/// one.
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

/// One registration line in, the issuer's answer out: [`serve_join`]'s
/// stream with no socket.
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

/// THE JOIN SAYS WHO IT RECORDED: [`serve_join`] answers the caller (its
/// audit line, `fleet_listen`) with the peer as the book holds it — its node,
/// the address the record came from, and the FLEET port the record names,
/// never a connection's source port — first `New`, then `Updated` with the
/// port it moved to; a refusal records and names no one.
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
            state.dir(),
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
    assert!(outcome.is_err() && answer.starts_with("ERR "), "{answer}");
    assert_eq!(read_peers(state.dir()).unwrap().len(), 1);
}

/// A REGISTRATION'S KEY IS THE ONE ITS CONNECTION PROVED: [`serve_join`]
/// refuses a `key=` the dialer did not prove — no proof, a proof of another
/// node, of another key — and records nothing; the key it proved is bound,
/// and that node's registration without one is then refused. NEGATIVE
/// CONTROL: an older aterm's registration, with no key, is recorded unbound,
/// as before keys came.
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
            state.dir(),
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
        outcome.is_err() && answer.contains("`aterm fleet peers rm cdcdcdcd` on the issuer"),
        "{answer}"
    );

    let older = Registration {
        node: node_of(0xef),
        key: None,
        ..reg
    };
    assert_eq!(serve(&older, None).0.unwrap().1.key, None);
}

/// A KEYLESS MOVE NEVER MOVES WHERE A KEY IS BOUND FROM, through the
/// issuer's own end and its book's file: an entry an older aterm recorded
/// from 10.0.0.8 is moved by a registration from 10.0.0.66 that proves no
/// key, as before keys came; a key proved from 10.0.0.66 is then refused,
/// in the words of an entry recorded before keys came; and the real node's,
/// from 10.0.0.8, is bound. NEGATIVE CONTROL: that last bind, after which
/// the one from 10.0.0.66 is another key.
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
        };
        let mut stream = Duplex {
            input: std::io::Cursor::new(reg.line().into_bytes()),
            output: Vec::new(),
        };
        let proved = key.map(|key| proof(key.node(), key.key()));
        serve_join(
            &mut stream,
            (IpAddr::from(from), proved.as_ref()),
            state.dir(),
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

/// AN ISSUER THAT CANNOT NAME ITSELF RECORDS NO ONE: one whose host is no
/// host — empty, as a host name that could not be read gives — answers the
/// join `ERR`, and its book stays empty: the member refuses an answer that
/// names no host, and a book that had recorded it would hold a member that
/// was told it never joined.
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
    };
    assert!(join_over(issuing.port, &secret(5), &reg).is_err());
    assert_eq!(read_peers(state.dir()).unwrap(), Vec::new());
}

/// A JOIN PROVES BOTH NODES: over a real fleet listener, the member's
/// registration binds the key its connection proved, and the member is
/// told the issuer's node by the issuer's proof; a machine that dials with
/// the member's node id under another key — a secret holder taking its
/// name — is refused, naming `aterm fleet peers rm <node>` on the issuer,
/// and the member's entry stands; once removed there, the node joins
/// afresh under its new key (a machine that lost its node key).
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
            && why.contains("`aterm fleet peers rm abababab` on the issuer"),
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

/// A check (`read_whole` with `survey facts`, no issuer among the peers)
/// answers every peer, in order, with what it serves or a reason a person can
/// act on: a member, a closed port and a listener of another fleet, each
/// within the hang detector; and a port that accepts and never speaks is its
/// deadline's, however short. (A name nothing resolves is
/// [`a_name_is_looked_up_as_itself_and_a_bare_one_as_dot_local`]'s, with no
/// resolver of the network's asked.)
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

    // Accepted by the kernel's backlog, never a byte answered: whatever the
    // deadline, it is the deadline that ends the wait.
    let silent = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let silent_port = silent.local_addr().unwrap().port();
    let results = check(&[at(silent_port)], Duration::from_millis(300));
    assert!(
        matches!(results[..], [Err(Unreached::Deadline(Stage::Handshake, _))]),
        "{results:?}"
    );
    drop(silent);
}

// ---------------------------------------------------------------------------
// the issuer's book, read by a member
// ---------------------------------------------------------------------------

/// The member's entry for its issuer on loopback `port`, as `aterm fleet
/// join` records it.
fn issuer_entry(node: u8, port: u16) -> Peer {
    Peer {
        via: Via::Issuer,
        ..joined(node, "127.0.0.1", [127, 0, 0, 1], port)
    }
}

/// Write `peers` as `state`'s book, as they are: a hand edit, so entries
/// [`record`] would never write (an issuer's own `via=issuer`, a copy) can
/// be in it.
fn write_book(state: &Path, peers: &[Peer]) {
    write_private(state, PEERS_FILE, book_text(peers).as_bytes()).unwrap();
}

/// A loopback port nothing listens on.
fn closed_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// THE ISSUER SERVES ITS BOOK: over a real fleet listener, a member's `book`
/// read of the issuer is answered with its first-hand entries — the members
/// that registered and the peers its person named, in order, never its own
/// issuer's entry nor a copy — headed by the issuer's node; a machine that
/// holds no join code answers `no-book`, naming its node; an issuer that
/// cannot read its book answers `not-now`; and a head naming another node
/// than the member's record of its issuer is refused.
#[test]
fn the_issuer_serves_its_book_and_any_other_machine_refuses() {
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

    let refused = fetch_book(&target, (&secret(5), None), deadline());
    assert!(
        matches!(&refused, Err(BookUnread::NoBook { node: Some(node), .. }) if *node == issuer(0x11).node),
        "no code here: no issuer, no book, and the node that says so: {refused:?}"
    );

    issue(state.dir());
    let book = fetch_book(&target, (&secret(5), None), deadline()).expect("the issuer's book");
    assert_eq!(book.node, issuer(0x11).node);
    assert_eq!(book.peers, first_hand, "first-hand entries, in order");
    assert_eq!(book.at, IpAddr::from(Ipv4Addr::LOCALHOST));

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

    // A book the issuer cannot read is no book this time, never none.
    write_private(state.dir(), PEERS_FILE, b"peer garbage\n").unwrap();
    let unshared = fetch_book(&target, (&secret(5), None), deadline());
    assert!(
        matches!(&unshared, Err(BookUnread::NotNow { node: Some(node), .. }) if *node == issuer(0x11).node),
        "{unshared:?}"
    );
}

/// AN OLDER ISSUER: a listener from before members read the book answers
/// `book` `NO op`; its member reads its issuer and the peers it knows, as it
/// did before the book, and its copy of the book goes (an answer replaces the
/// copy whole), all in the one deadline.
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

/// AN OLDER ISSUER'S `NO op` ENDS THE DIAL THERE: it proved this fleet's
/// secret, so it is this fleet's machine, and asking its name would only
/// spend the read's one deadline. Here its name (`::1`) leads to a newer
/// listener of the same node, which a dial going on to the name would read
/// the book of. NEGATIVE CONTROL: that newer listener at the recorded address
/// is read.
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

/// A MEMBER READS THE WHOLE FLEET: its book names only its issuer (and a
/// stale copy of a member the issuer no longer holds), and its read reaches
/// every member the issuer's book names — a registered member's survey, a
/// peer the issuer's person named (closed, so its reason) — never its own
/// registration there nor the stale copy.
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

/// A BOOK'S KEYS ARE COPIED ONLY FROM THE ISSUER ITSELF: a book read of an
/// issuer that proved the key its member binds it to is proven, and its
/// copies keep the keys it binds; an issuer bound to no key (a member that
/// joined before keys came) is a claim, and so is every key its book names,
/// so its copies keep none; a machine that claims the issuer's node under
/// another key gives no book at all.
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

/// THE COPY IS MERGED WHOLE: the old copy is replaced; this machine, the
/// issuer itself, a node or place the member's own book holds, a repeat, and
/// a second-hand entry are left out; an entry on the issuer's loopback is
/// placed at the address the issuer answered from; a merge twice is the
/// merge once; and the book never holds more than its cap, the member's own
/// entries kept first.
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
            relocated,
            sibling,
        ]
    );
    let once = book.clone();
    merge_book(&mut book, &fetched, Some(&node_of(0xaa)));
    assert_eq!(book, once, "a merge twice is the merge once");

    // An issuer that answered on loopback is on this machine: its loopback
    // entries stay where they are.
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

    // The cap: a book of more members than a book holds fills it, the
    // member's own entries first and whole.
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

/// A COPY IS NEVER EDITED BY HAND: `rm` refuses a peer only the copy holds,
/// and takes this machine's own entry where both answer; `record` refuses to
/// write a copy; read beside the book, a person's entry for a copy's place
/// stands for it, a node the book holds stands for its copy, and the two
/// together never pass the cap, the book's own entries first and whole.
#[test]
fn a_copy_of_the_issuers_book_yields_to_the_machines_own_entries() {
    let own = vec![issuer_entry(0x11, 7100), manual("LAB-MINI", 7101)];
    let copies = vec![
        copy(manual("lab-mini", 7101)),
        copy(joined(0x22, "m2", [10, 0, 0, 2], 7100)),
        copy(joined(0x11, "m7-again", [10, 0, 0, 77], 7100)),
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
        [own[0].clone(), own[1].clone(), copies[1].clone()],
        "a place or a node of the book's own stands for its copy"
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

/// THE COPY HAS A FILE OF ITS OWN, BOUND TO ITS ISSUER: what a read of the
/// book writes is read back only for the node it was read from — another
/// issuer, a missing file, or a line that is no copy reads as no copy — and
/// the book itself never holds one: a copy found there (an aterm built
/// before the file) is passed over, and an edit of the book drops it, so the
/// words an older aterm knows are the only ones its book holds.
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
    write_issuer_book(state.dir(), &issuer, &written).unwrap();
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

/// THE BOOK'S MEMBERS ARE ASKED WHILE THE ISSUER'S OWN SURVEY WAITS: the
/// issuer answers its book at once and its survey only once the member that
/// book names has been asked — an issuer whose control server is slow
/// behind a listener that answers the book at once — and the read reaches
/// both, the member never waiting on the issuer's survey. NEGATIVE CONTROL:
/// hands for the members started only after the issuer's read leave the
/// member undialled until that survey ends, and both reach the deadline.
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

/// A MACHINE AT THE ISSUER'S OLD ADDRESS NEVER SPEAKS FOR THE ISSUER: the
/// address the member recorded for its issuer now holds another member of
/// the fleet — a newer one that answers `no-book` naming its own node, or an
/// older one that answers `NO op` — while the issuer answers at its name.
/// Neither refusal is the issuer's: the issuer's own survey finds it at its
/// name, its book is read there, its member asked, and the member's copy
/// stands. NEGATIVE CONTROL: a refusal taken as the issuer's whoever said
/// it drops the copy and reads no book.
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

/// AN ISSUER THAT CANNOT SHARE ITS BOOK NOW (it cannot read it) answers
/// `not-now`: the member keeps its copy and reads it, its issuer's own row
/// answered, and the read says it is short of a fresh book. NEGATIVE
/// CONTROL: `not-now` taken as `no-book` drops the copy.
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

/// A RE-KEYED MEMBER IS READ UNDER ITS NEW KEY the moment the issuer's book
/// binds it: a copy of it held under the old key is skipped and queued again
/// under the book's, and the book's keys replace the copy's. NEGATIVE
/// CONTROL: a book that binds the key the copy holds changes nothing.
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
    let queue = || Queue {
        peers: peers.clone(),
        next: 0,
        skip: vec![0],
        live: 1,
        started: 1,
        keys: Arc::new(Keyring::of(&peers)),
    };
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

/// THE WHOLE READ AFTER A RE-KEY: a member's copy holds node M under its old
/// key, M's listener proves its new one, and the issuer's book binds the new
/// one — M is read, proven, never named an impostor. NEGATIVE CONTROL: with
/// the issuer's book still binding the old key, M is an impostor.
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
            &[keyed(0xd4, bound, [127, 0, 0, 1], m.port)],
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
            copy(keyed(0xd4, old.key(), [127, 0, 0, 1], m.port)),
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

/// PAST [`AT_ONCE`] DIALS A PEER HAS A DEADLINE OF ITS OWN: the issuer's
/// seat is the first dial, so the first `AT_ONCE - 1` peers taken share the
/// read's one deadline and every later one waited for a free place; a place
/// skipped is no dial; and no call of `hands` starts more than `AT_ONCE`
/// readers in all. NEGATIVE CONTROL: a take one dial early (`>=` for `>`)
/// gives the last of the first `AT_ONCE` dials a deadline of its own.
#[test]
fn a_peer_past_the_first_at_once_dials_waits_for_a_place_of_its_own() {
    let peers: Vec<Peer> = (0..AT_ONCE + 5)
        .map(|n| joined(0, "127.0.0.1", [127, 0, 0, 1], 7000 + n as u16))
        .collect();
    let mut queued = Queue {
        peers,
        next: 0,
        skip: vec![3],
        live: 1,
        started: 1,
        keys: std::sync::Arc::default(),
    };
    assert_eq!(queued.hands(), AT_ONCE - 1, "the seat holds one place");
    assert_eq!(queued.hands(), 0, "every place is held");
    let mut waited = Vec::new();
    while let Some((at, _, past)) = queued.take() {
        assert_ne!(at, 3, "a skipped place is never taken");
        waited.push(past);
    }
    assert_eq!(waited.len(), AT_ONCE + 4);
    let first = waited.iter().position(|past| *past);
    assert_eq!(first, Some(AT_ONCE - 1), "{waited:?}");
    assert!(waited[AT_ONCE - 1..].iter().all(|past| *past));
}

/// A DARK ISSUER COSTS ITS MEMBER ONE DIAL: an issuer that accepts and never
/// speaks holds its book's dial to the deadline, and its own row is that
/// dial's reason (connected, no handshake) — never a second dial — while
/// every other peer is asked meanwhile; the copy stands. The dark listener's
/// queue holds the one connection.
#[test]
fn a_dark_issuer_costs_its_member_one_deadline() {
    const WITHIN: Duration = Duration::from_secs(2);
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

/// A loopback listener that takes every connection and closes it at once,
/// counting them: what a dialer's handshake passes over for the name (an
/// address that is no fleet listener now). Stopped on drop.
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

/// A MOVED ISSUER IS SURVEYED WHERE ITS BOOK ANSWERED: the address the
/// member recorded is no fleet listener now (it takes the connection and
/// closes it), and the issuer answers at its name. The book's dial passes
/// over the recorded address once; the issuer's own survey is asked where the
/// book came from, never at the recorded address again, and both answer.
/// NEGATIVE CONTROL: the survey asked at the recorded address first dials it
/// a second time.
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

/// A MOVED ISSUER WHOSE BOOK FAILS IS SURVEYED WHERE IT PROVED ITSELF: the
/// address the member recorded is no fleet listener now, and the issuer at
/// its name proves `book` and then answers none (a head promising more
/// entries than a book holds). Its own survey is asked where that dial was
/// answered, never at the recorded address a second time in the one
/// deadline; it answers, and the read is short of the book. NEGATIVE
/// CONTROL: a failed reply read as no address dials the recorded one again.
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

/// WHERE AND WHETHER THE ISSUER'S SURVEY FOLLOWS A BOOK THAT FAILED, with no
/// clock: a dial that failed before proof, or a book the deadline cut short,
/// is the issuer's answer too (never a second dial); one that proved itself
/// and then failed, with time left, is surveyed at the address it proved
/// itself at — a reply that failed, a listener at capacity — and one that
/// never proved names no address. NEGATIVE CONTROL: a seat that does not
/// read the deadline dials a book the deadline cut short again.
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

/// A BOOK THE DEADLINE CUT SHORT IS THE ISSUER'S ROW: the issuer proves
/// `book` and sends none; the read's one deadline comes while the member
/// waits for it, and the issuer's row is that reason (proved, no book) —
/// never a second dial with no time left, read as no connection — while the
/// copy stands. On a machine too loaded to finish the handshake in `WITHIN`
/// the deadline comes at the handshake instead, the same one row: the test
/// asks no latency of the loopback, and the rule's clock is pinned by
/// `a_failed_book_ends_the_issuers_seat_or_names_where_to_survey_it`.
/// NEGATIVE CONTROL: the issuer's survey dialled after the deadline reads as
/// `Deadline(Stage::Connect)`.
#[test]
fn a_book_the_deadline_cut_short_is_the_issuers_row() {
    const WITHIN: Duration = Duration::from_secs(2);
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

/// A MEMBER NEVER READS ITSELF THROUGH ITS ISSUER'S BOOK: the issuer's person
/// named this machine's own place by hand (as the loopback fleet does for a
/// member past four an address), so the copy names no node; dialled, it
/// proves this machine's node under this machine's key, and it is left out
/// of the read — the issuer and the other member read. NEGATIVE CONTROLS:
/// only that proof leaves it out. A Mac there that names this machine's node
/// without its key — that node id proved under a key of its own, or none
/// proved and the node in its survey's header — is read as the claim it is,
/// never left out as this machine (which would hide it from this machine's
/// read); so is this machine's own answer to a read that proves no node key
/// of its own; and without `own` the place is read as a peer.
#[test]
fn a_member_never_reads_itself_through_its_issuers_book() {
    let own = node_of(0xaa);
    let me = identity(&own, 1);
    // The whole read, `proving` what answers at the place (its survey's
    // header naming `own` whatever it proves): each row's entry, its node
    // and whether that node is proven.
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
    /// A one-line reply, held until `want` requests have come to this
    /// listener (`arrived` counts them) or `released` is set: only a reader
    /// that asks them all at once gets an answer from any.
    Together {
        arrived: Arc<AtomicUsize>,
        want: usize,
        released: Arc<AtomicBool>,
    },
}

impl Answer {
    /// [`Answer::Together`] for `want` requests, none come yet.
    fn together(want: usize) -> Self {
        Self::Together {
            arrived: Arc::new(AtomicUsize::new(0)),
            want,
            released: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Answer every request a [`Answer::Together`] still holds.
    fn release(&self) {
        if let Self::Together { released, .. } = self {
            released.store(true, Ordering::Release);
        }
    }
}

/// How many bytes a [`Answer::Dribble`] peer sends, one per 80 ms: over five
/// minutes, so only the reader's deadline ends its read inside the hang
/// detector. It stops at the first byte the cut connection refuses.
const DRIBBLE_BYTES: usize = 4_000;

/// A loopback fleet listener serving `survey` at `scope`, which answers the
/// request as its [`Answer`] says; stopped on drop.
struct Answering {
    port: u16,
    running: Arc<AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
}

impl Answering {
    fn start(secret: FleetSecret, scope: FleetScope, answer: Answer) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let running = Arc::new(AtomicBool::new(true));
        // Room for every peer a read asks at once, all from loopback.
        let listening = Listening {
            secret,
            scope,
            node: None,
            serves: &[FleetOp::Survey],
            slots: AT_ONCE,
            per_source: AT_ONCE,
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
                                // Held until the reader goes: its close ends the read.
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
                                // An application-data record claiming 16 KiB,
                                // then its body a byte at a time.
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
                                    // Held at most the hang detector, as the
                                    // reader waits: past it the test has failed.
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
        Self {
            port,
            running,
            serving: Some(serving),
        }
    }
}

impl Drop for Answering {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
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

/// [`survey_all`] of `targets` on a thread of its own, waited for at most
/// the [`HANG`] detector: its results.
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

/// THE FLEET READ: two members (one allowing facts, one content), a closed
/// port, a member past the reply cap and one of another fleet — each one
/// result, its survey or its reason, asked at the hang detector, since each
/// must answer. Then a port that accepts and never speaks, a member whose
/// control server never answers (a stopped aterm) and one that dribbles its
/// answer: each ended by its one deadline, at the stage it reached — the
/// silent port's the handshake, the others' the answer once their
/// handshake came in time, which a loaded machine need not give them; so a
/// proved stream's answer is asked once more under a deadline of its own,
/// which ends it whatever the peer does. (That they are asked side by side
/// is [`thirty_peers_are_asked_at_once`]'s.)
#[test]
fn survey_all_answers_each_peer_within_one_deadline() {
    const WITHIN: Duration = Duration::from_secs(2);
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
    // The kernel completes the connect; nothing ever reads or writes.
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

/// THIRTY PEERS ARE ASKED AT ONCE, by no clock: each holds its answer until
/// all thirty have asked, so only a read that asks them side by side gets
/// thirty answers, and a walk in turn would get none before the hang
/// detector. NEGATIVE CONTROL: the same peers asked one at a time are
/// answered by none, each its own deadline's.
#[test]
fn thirty_peers_are_asked_at_once() {
    const PEERS: usize = 30;
    let answer = Answer::together(PEERS);
    let together = Answering::start(secret(5), FleetScope::Facts, answer.clone());
    let read = surveyed(vec![on_loopback(together.port); PEERS], HANG);
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
        );
        assert!(matches!(one, Err(Unreached::Deadline(..))), "{one:?}");
    }
    in_turn.release();
}

/// A NAME IS LOOKED UP AS ITSELF, AND A BARE ONE AS `<name>.local` TOO, by
/// a resolver of this test's, so no network's resolver is asked: a name
/// nothing resolves is its own reason; a bare name only multicast DNS knows
/// reaches the peer at its `.local` answer; a dotted one is asked as itself
/// alone (the control: the same resolver gives it nothing).
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
            (&[], resolver),
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

/// `at_once` asks at most its limit at a time — and that many — and answers
/// in the items' order.
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
        // Each waits for a full house (or the last items), so a walk that
        // ran more at once than its limit is seen to.
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

/// A fleet listener on `listener` that answers every `survey` with a header
/// naming `node` (or with the lines it is given, [`Naming::replying`]);
/// stopped on drop.
struct Naming {
    running: Arc<AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
    /// How many surveys it was asked.
    asked: Arc<AtomicUsize>,
}

impl Naming {
    fn on(listener: TcpListener, secret: FleetSecret, node: String) -> Self {
        Self::proving(listener, secret, node, None)
    }

    /// [`Naming::on`], proving the node `me` when there is one — its header
    /// still naming `node`, as an impostor's would.
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

    /// [`Naming::proving`], answering every `survey` with `lines` after its
    /// `OK <n>` head, whatever they say.
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
        let serving = std::thread::spawn({
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
        Self {
            running,
            serving: Some(serving),
            asked,
        }
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::Acquire)
    }
}

impl Drop for Naming {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// Two loopback listeners on one port: `[::1]` and `127.0.0.1`. Where there
/// is no IPv6 loopback, or no port free on both, the test ends COULD NOT RUN
/// (`aterm_spec::host::could_not_run`): never a pass that checked nothing.
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

/// A RECORDED ADDRESS THAT PASSED TO ANOTHER MACHINE gives way to the name:
/// another fleet's listener there (the handshake fails) and another member
/// there (its survey names another node) are each passed over for the name,
/// which reaches the peer; with no other place to ask, the other member is
/// reported, never read as this peer. An address past the LAN is never
/// dialled. NEGATIVE CONTROL: a target that names no node takes the
/// recorded address's answer.
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

    // Another fleet's listener at the recorded address.
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

    // Another member of this fleet at the recorded address.
    let (v6, v4, port) = twin_listeners();
    let _peer = Naming::on(v6, secret(5), node.clone());
    let _member = Naming::on(v4, secret(5), other.clone());
    let read = survey_all(
        &[
            target([127, 0, 0, 1], port, Some(&node)),
            target([127, 0, 0, 1], port, None),
            // The name is the recorded address itself: nowhere else to ask.
            Target {
                host: "127.0.0.1".to_owned(),
                ..target([127, 0, 0, 1], port, Some(&node))
            },
            // Past the LAN (TEST-NET-1, no machine's): never dialled, so
            // the name answers.
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

/// A PEER THAT CLAIMS A BOUND NODE WITHOUT ITS KEY IS NAMED, NEVER READ AS
/// IT: at a target's recorded address, a secret holder that proves the
/// target's node id under another key is an impostor, and one that proves
/// no node where a key is bound is unproved — neither is asked its survey —
/// and the name is asked, which reaches the real node, proven; with nowhere
/// else to ask, each is reported as what it is. A peer a person named binds
/// no key of its own and is still held to the key the book binds the node
/// it claims, by its proof or by its header. NEGATIVE CONTROLS: the real
/// node is read, proven; a node bound to no key (an older aterm's entry) is
/// read as its claim, unproven.
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
                // The name is the recorded address itself: nowhere else.
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

/// THIS MACHINE, AT A PLACE NAMED BY HAND, IS PROVEN BY ITS OWN KEY: a
/// person's `peers add` or `[[fleet.peers]]` of this machine names no node,
/// no keyring binds this machine's own, and what answers there proves this
/// machine's node under the key only it holds — this machine, never a claim
/// (so the menu bar's glance leaves it out as this Mac). NEGATIVE CONTROL:
/// this machine's node id under another key is the claim it was, and so is
/// this machine's own answer to a read that proves no node key of its own.
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

/// A PROVEN PEER IS READ ONLY AS THE NODE IT PROVED: one whose survey's
/// header names another node — this Mac's own, which would drop it from the
/// menu bar's glance as this Mac and key its notices as this Mac's — is
/// refused whole, naming both, however the claim is spelt for a reader to
/// decode (a second header, a second `node=`, percent-encoded, in capitals),
/// and whether or not the node it proved is bound here; at a recorded
/// address, it gives way to the name. NEGATIVE CONTROLS: an honest proven
/// peer's header (its node, `-` where it cannot read its node file, or no
/// `node=`) is read, proven; a peer that proves no node is still read as its
/// header's claim.
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

    // A peer that proves no node: its header is its claim, as before.
    match read(None, vec![head(&format!("node={mine}"))], false) {
        Ok(surveyed) => assert_eq!(
            (surveyed.node.as_deref(), surveyed.proven),
            (Some(mine.as_str()), false)
        ),
        Err(e) => panic!("a claim from a peer that proved no node was refused: {e}"),
    }

    // At a recorded address, the name is asked, which reaches the real node.
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

/// THE PROVEN NODE OF A SERVED CONNECTION ([`proven_peer`]) is the node the
/// dialer proved under the key the listener's book binds it to — what a
/// listener authorizes a node by. The node's own key proves it; the same
/// node id under another key, a node bound to none, and no proof are each
/// named, never that node.
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

/// A DRIVE IS HELD TO THE NODE KEY THIS MACHINE BINDS ([`drive`]): a peer
/// that proves the bound key is driven, its stream carrying the lines after
/// the proof; one that proves a node bound to no key is never driven,
/// though a read would take it as the peer's claim; one that proves the
/// node under another key is an impostor; and a listener that serves no
/// `drive` (its person allows none) says what its person runs.
#[test]
fn a_drive_reaches_only_the_node_this_machine_binds_a_key_to() {
    let host = identity(&node_of(0xd7), 4);
    let listen = |me: NodeIdentity, serves: &'static [FleetOp]| {
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
    };
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
    let mut driven =
        drive(&target(port), &bound, (&secret(6), &me), HANG).expect("the bound node is driven");
    assert_eq!(driven.node, host.node());
    driven.write_all(b"s-1 status\n").unwrap();
    let mut answer = String::new();
    io::BufRead::read_line(&mut io::BufReader::new(&mut driven), &mut answer).unwrap();
    assert_eq!(answer, "OK drive s-1 status\n", "the line after the proof");
    drop(driven);
    match drive(&target(port), &Keyring::default(), (&secret(6), &me), HANG) {
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
    match drive(&target(port), &bound, (&secret(6), &me), HANG) {
        Err(Unreached::Impostor { node, .. }) => assert_eq!(node, host.node()),
        other => panic!(
            "another key was driven: {:?}",
            other.map(|d| d.node.clone())
        ),
    }
    running.store(false, Ordering::Release);
    serving.join().unwrap();

    let (port, running, serving) = listen(host.clone(), &[FleetOp::Survey]);
    match drive(&target(port), &bound, (&secret(6), &me), HANG) {
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

/// WHO REFUSED A CONNECT is read off its kind, which the advice a survey,
/// `peers` and `join` give follows: `EPERM` and `EACCES` are this process's
/// own refusal (a sandbox, a firewall); `EHOSTUNREACH` is no route to the
/// host (on a Mac, local-network privacy, or a host off the LAN);
/// `ENETUNREACH` is no route to its network (this machine offline, or a VPN
/// holding the route), which privacy never gives. Each is one kind alone. A
/// name that did not resolve, an address past the LAN and this machine's
/// own failure were never dialled. NEGATIVE CONTROLS: a refused port, and a
/// handshake that failed, are dialled and none of the three.
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
/// and `chmod -N` makes it readable again. The same for the directory.
#[cfg(target_os = "macos")]
#[test]
fn an_access_list_that_lets_others_in_is_not_private() {
    let state = State::new("acl");
    write_key_file(state.dir(), &[0x3e; SECRET_LEN]).unwrap();
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
    assert!(write_key_file(state.dir(), &[0x3f; SECRET_LEN]).is_err());
    chmod(&["-N".as_ref(), state.dir().as_os_str()]);
    assert_eq!(word(), None);
}

/// The actual resolver permit is held through unfinished work and unwind,
/// independently of the caller's deadline. No DNS or thread is started here.
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
