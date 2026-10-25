// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end fleet listener and remote drive tests against `aterm --headless`.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use aterm_net::fleet::{
    FleetError, FleetOp, FleetScope, FleetSecret, KEY_FILE, NodeIdentity, NodeKey, client_handshake,
};
use aterm_net::fleet_member::{
    BookRead, BookUnread, Keyring, Registration, Target, Via, dial, fetch_book, read_node_identity,
    read_peers, read_whole, register_by,
};

use crate::headless_boot::{self, Instance, POLL_GAP, boot_session, ctl, ctl_ok, field, status};

const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

const STANDIN_CLAUDE: &str = "#!/bin/sh\n\
    case \"$1\" in --version) echo '2.1.0 (Claude Code)'; exit 0 ;; esac\n\
    exit 0\n";

const SECRET: [u8; 20] = [0x5a; 20];

const ROTATED: [u8; 20] = [0xa5; 20];

fn hold_a_port() -> (TcpListener, u16) {
    let holder = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let port = holder.local_addr().expect("its address").port();
    (holder, port)
}

fn write_key(state: &Path, secret: &[u8]) {
    use std::os::unix::fs::PermissionsExt as _;
    let key = state.join(KEY_FILE);
    let hex: String = secret.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(&key, format!("{hex}\n")).expect("write the key");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o700)).expect("chmod");
}

fn fleet_table(port: u16, allow: &str) -> String {
    format!("[fleet]\nlisten = \"127.0.0.1:{port}\"\nallow = \"{allow}\"\n")
}

const RETRY_MS: &str = "100";

fn boot(port: u16) -> Instance {
    headless_boot::boot_with(
        "aflt",
        |tmp, cmd| {
            let state = tmp.join("state");
            write_key(&state, &SECRET);
            let config = tmp.join("cfg/aterm/aterm.toml");
            let text = std::fs::read_to_string(&config).expect("the scratch config");
            std::fs::write(&config, format!("{text}{}", fleet_table(port, "facts")))
                .expect("write the fleet table");
            cmd.env("ATERM_STATE_HOME", &state)
                .env("ATERM_DEBUG_FLEET_RETRY_MS", RETRY_MS)
                .args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    )
}

fn fleet_reads(inst: &Instance, sid: &str, want: &str) {
    let mut last = String::new();
    let seen = aterm_spec::wait::within(HANG, POLL_GAP, || {
        last = status(inst, sid);
        (field(&last, "fleet") == Some(want)).then_some(())
    });
    assert!(
        seen.is_some(),
        "fleet={want} never came; last status: {last}"
    );
}

fn ask<S: Read + Write>(stream: &mut S, line: &str) -> String {
    stream
        .write_all(format!("{line}\n").as_bytes())
        .and_then(|()| stream.flush())
        .expect("send the request");
    read_line(stream)
}

fn read_line<S: Read>(stream: &mut S) -> String {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => line.push(byte[0]),
            Err(e) => panic!("reading a reply line: {e} (after {line:?})"),
        }
    }
    String::from_utf8(line).expect("a utf-8 reply")
}

fn ask_lines<S: Read + Write>(stream: &mut S, line: &str) -> Vec<String> {
    let head = ask(stream, line);
    let n: usize = head
        .strip_prefix("OK ")
        .and_then(|rest| rest.split(' ').next()?.parse().ok())
        .unwrap_or_else(|| panic!("{line}: a Lines reply, not {head:?}"));
    (0..n).map(|_| read_line(stream)).collect()
}

fn closed<S: Read>(stream: &mut S) -> bool {
    let mut rest = Vec::new();
    match stream.read_to_end(&mut rest) {
        Ok(_) => rest.is_empty(),
        Err(e) => {
            rest.is_empty()
                && !matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
        }
    }
}

#[test]
fn a_fleet_peer_reads_version_and_is_refused_everything_else() {
    let dial = |port: u16, secret: &[u8], op: FleetOp| {
        let tcp = TcpStream::connect(("127.0.0.1", port)).map_err(FleetError::Io)?;
        let secret = FleetSecret::new(secret).expect("a fleet secret");
        client_handshake(tcp, &secret, op, HANG).map(|(mut transport, scope)| {
            transport
                .stream()
                .get_ref()
                .set_read_timeout(Some(HANG))
                .expect("bound the reads");
            (transport, scope)
        })
    };
    let (holder, port) = hold_a_port();
    let inst = boot(port);
    let (_, sid) = boot_session(&inst);

    fleet_reads(&inst, &sid, "retrying");
    drop(holder);
    fleet_reads(&inst, &sid, "listening");

    let (mut peer, scope) = dial(port, &SECRET, FleetOp::Survey).expect("a proved peer");
    assert_eq!(scope, FleetScope::Facts, "the level [fleet] allow names");
    for _ in 0..2 {
        let reply = ask(peer.stream(), "version");
        assert!(reply.starts_with("OK version="), "{reply}");
    }
    for asked in ["survey", "survey content"] {
        let lines = ask_lines(peer.stream(), asked);
        assert!(
            lines[0].starts_with("instance ") && lines[0].contains(" level=facts "),
            "{lines:?}"
        );
        let row = lines
            .iter()
            .find(|l| l.contains(&format!(" sid={sid} ")))
            .unwrap_or_else(|| panic!("the boot session's row: {lines:?}"));
        for content in [" cwd=", " title=", " prompt=", " said=", " rows="] {
            assert!(!row.contains(content), "{content} at facts: {row}");
        }
    }
    assert!(
        closed(peer.stream()),
        "a fleet connection closes after its fourth request"
    );
    let (mut peer, _) = dial(port, &SECRET, FleetOp::Survey).expect("a proved peer");
    let reply = ask(peer.stream(), "version");
    assert!(reply.starts_with("OK version="), "{reply}");
    let reply = ask(peer.stream(), "sessions");
    assert!(reply.starts_with("ERR denied: "), "{reply}");
    assert!(closed(peer.stream()), "a refusal closes the connection");

    let refused = [
        format!("@{sid} text"),
        format!("@{sid} version"),
        "spawn".to_owned(),
        "turn echo fleet".to_owned(),
        "settings".to_owned(),
        "hold on".to_owned(),
        "dial lab version".to_owned(),
        "subscribe @* events".to_owned(),
        "inbox get 1".to_owned(),
        "sessions".to_owned(),
        "help".to_owned(),
    ];
    for line in &refused {
        let (mut peer, _) = dial(port, &SECRET, FleetOp::Survey).expect("a proved peer");
        let reply = ask(peer.stream(), line);
        assert!(
            reply.starts_with("ERR denied: the fleet serves "),
            "{line}: {reply}"
        );
        assert!(closed(peer.stream()), "{line}: the connection must close");
    }
    let (mut peer, _) = dial(port, &SECRET, FleetOp::Survey).expect("a proved peer");
    peer.stream()
        .write_all(b"feed-bin 5\nhello")
        .expect("send the frame");
    peer.stream().flush().expect("send the frame");
    let reply = read_line(peer.stream());
    assert!(
        reply.starts_with("ERR denied: the fleet serves no frame"),
        "{reply}"
    );
    assert!(closed(peer.stream()), "a frame closes the connection");
    let screen = headless_boot::ctl_ok(&inst, &[&format!("@{sid}"), "text"]);
    assert!(
        !screen.contains("echo fleet") && !screen.contains("hello"),
        "a refused line reached the session: {screen}"
    );

    let wrong = dial(port, &[0x11; 20], FleetOp::Survey).err();
    assert!(
        matches!(wrong, Some(FleetError::ListenerWrongSecret)),
        "{wrong:?}"
    );
    let (mut joining, _) = dial(port, &SECRET, FleetOp::Join).expect("a proved join");
    let node = "ab".repeat(16);
    let reply = ask(
        joining.stream(),
        &format!("register node={node} host=lab-mini port=7101 version=0.95.0"),
    );
    assert!(reply.starts_with("ERR "), "{reply}");
    assert!(
        !inst.tmp.join("state/fleet-peers").exists(),
        "a machine that issued no code records no member"
    );

    let (holder, moved) = hold_a_port();
    let config = inst.tmp.join("cfg/aterm/aterm.toml");
    let text = std::fs::read_to_string(&config).expect("the config");
    let edited = text.replace(&fleet_table(port, "facts"), &fleet_table(moved, "content"));
    assert_ne!(edited, text, "the fleet table was rewritten");
    std::fs::write(&config, edited).expect("edit the config");
    fleet_reads(&inst, &sid, "retrying");
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the old fleet port is closed"
    );
    drop(holder);
    fleet_reads(&inst, &sid, "listening");
    let (mut peer, scope) = dial(moved, &SECRET, FleetOp::Survey).expect("a proved peer");
    assert_eq!(
        scope,
        FleetScope::Content,
        "the level follows [fleet] allow"
    );
    let reply = ask(peer.stream(), "version");
    assert!(reply.starts_with("OK version="), "{reply}");
    let lines = ask_lines(peer.stream(), "survey");
    assert!(lines[0].contains(" level=content "), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!(" sid={sid} ")) && l.contains(" cwd=")),
        "content carries the excerpts: {lines:?}"
    );

    let state = inst.tmp.join("state");
    write_key(&state, &ROTATED);
    assert!(
        closed(peer.stream()),
        "a rotation cuts the fleet's connections"
    );
    let rotated = aterm_spec::wait::within(HANG, POLL_GAP, || {
        dial(moved, &ROTATED, FleetOp::Survey).ok()
    });
    let (mut peer, _) = rotated.expect("the listener serves the rotated secret");
    let reply = ask(peer.stream(), "version");
    assert!(reply.starts_with("OK version="), "{reply}");
    let old = dial(moved, &SECRET, FleetOp::Survey).err();
    assert!(
        matches!(old, Some(FleetError::ListenerWrongSecret)),
        "the old secret is refused: {old:?}"
    );

    std::fs::remove_file(state.join(KEY_FILE)).expect("remove the key");
    fleet_reads(&inst, &sid, "refused:no-key");
    assert!(
        closed(peer.stream()),
        "a removed key cuts the fleet's connections"
    );
    assert!(
        TcpStream::connect(("127.0.0.1", moved)).is_err(),
        "with no key there is no fleet port"
    );
    write_key(&state, &SECRET);
    fleet_reads(&inst, &sid, "listening");
    let (_, scope) = dial(moved, &SECRET, FleetOp::Survey).expect("a proved peer");
    assert_eq!(scope, FleetScope::Content);

    let text = std::fs::read_to_string(&config).expect("the config");
    let elsewhere = format!("[fleet]\nlisten = \"192.0.2.1:{moved}\"\nallow = \"content\"\n");
    std::fs::write(
        &config,
        text.replace(&fleet_table(moved, "content"), &elsewhere),
    )
    .expect("edit the config");
    fleet_reads(&inst, &sid, "retrying");
}

struct World(PathBuf);

impl Drop for World {
    fn drop(&mut self) {
        headless_boot::remove_world(&self.0);
    }
}

fn fleet(root: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let aterm = crate::launch_isolation::aterm_executable();
    #[expect(
        clippy::disallowed_types,
        reason = "the binary under test, isolated by launch_isolation::apply (it inherits nothing)"
    )]
    let mut cmd = std::process::Command::new(aterm);
    cmd.arg("fleet")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::launch_isolation::apply(&mut cmd, root);
    cmd.env("ATERM_STATE_HOME", root.join("state"));
    let mut child = cmd.spawn().expect("spawn aterm fleet");
    if let Some(text) = stdin {
        let mut pipe = child.stdin.take().expect("its stdin");
        pipe.write_all(text.as_bytes()).expect("write its stdin");
    }
    crate::child_output::wait_and_read(child, Instant::now() + HANG)
        .unwrap_or_else(|| panic!("aterm fleet {args:?} did not exit"))
}

fn ctl_in(root: &Path, args: &[&str]) -> Output {
    ctl_fed(root, args, None)
}

fn ctl_fed(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> Output {
    headless_boot::until_served(|| {
        let aterm = crate::launch_isolation::aterm_executable();
        #[expect(
            clippy::disallowed_types,
            reason = "the binary under test, isolated by launch_isolation::apply (it inherits nothing)"
        )]
        let mut cmd = std::process::Command::new(aterm);
        cmd.arg("ctl")
            .args(args)
            .stdin(stdin.map_or_else(Stdio::null, |_| Stdio::piped()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::launch_isolation::apply(&mut cmd, root);
        cmd.env("ATERM_STATE_HOME", root.join("state"));
        let mut child = cmd.spawn().expect("spawn aterm ctl");
        if let Some(bytes) = stdin {
            let mut pipe = child.stdin.take().expect("its stdin");
            pipe.write_all(bytes).expect("write its stdin");
        }
        crate::child_output::wait_and_read(child, Instant::now() + HANG)
            .unwrap_or_else(|| panic!("aterm ctl {args:?} did not exit"))
    })
}

fn fleet_ok(root: &Path, args: &[&str], stdin: Option<&str>) -> String {
    let out = fleet(root, args, stdin);
    assert!(
        out.status.success(),
        "aterm fleet {args:?} failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

#[test]
fn fleet_init_and_join_through_the_binary_register_a_member() {
    let (holder, port) = hold_a_port();
    drop(holder);
    let port = port.to_string();
    let mut init = String::new();
    let inst = headless_boot::boot_with(
        "afli",
        |tmp, cmd| {
            init = fleet_ok(
                tmp,
                &[
                    "init",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port,
                    "--allow",
                    "content",
                ],
                None,
            );
            cmd.env("ATERM_STATE_HOME", tmp.join("state")).args([
                "--lines",
                "24",
                "--columns",
                "80",
            ]);
        },
        |_| true,
    );
    let (_, sid) = boot_session(&inst);
    let code_file = inst.tmp.join("state/fleet-join-code");
    let code = std::fs::read_to_string(&code_file).expect("init wrote the code");
    assert!(code.starts_with("atf1-"), "a join code");
    assert!(
        init.contains(&code_file.display().to_string()) && !init.contains(code.trim()),
        "init prints the code's path, never the code:\n{init}"
    );
    fleet_reads(&inst, &sid, "listening");

    let member = World(headless_boot::scratch_root("aflm"));
    let refused = fleet(&member.0, &["join", code.trim()], None);
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert!(
        !member.0.join("state").join(KEY_FILE).exists(),
        "an argument code writes nothing"
    );

    let (holder, member_port) = hold_a_port();
    drop(holder);
    let member_port = member_port.to_string();
    fleet_ok(
        &member.0,
        &["join", "--host", "127.0.0.1", "--port", &member_port],
        Some(&code),
    );
    let node = std::fs::read_to_string(member.0.join("state/fleet-node")).expect("a node id");
    let node = node.trim();
    let key_of = |state: &Path| -> NodeKey {
        read_node_identity(state)
            .expect("a node key it can read")
            .expect("init and join mint the node key")
            .key()
    };
    let member_key = key_of(&member.0.join("state"));
    let book = std::fs::read_to_string(inst.tmp.join("state/fleet-peers")).expect("the book");
    assert!(
        book.lines().any(
            |line| line.contains(&format!("node={node} key={}", member_key.hex()))
                && line.contains("addr=127.0.0.1")
                && line.contains("via=joined")
        ),
        "the issuer's book holds the member, bound to the key its join proved: {book}"
    );
    let issuer_key = key_of(&inst.tmp.join("state"));
    let theirs = read_peers(&member.0.join("state")).expect("the member's book");
    assert!(
        theirs
            .iter()
            .any(|peer| peer.via == Via::Issuer && peer.key == Some(issuer_key)),
        "{theirs:?}"
    );

    let check_of = |row: &str| {
        row.split("  ")
            .map(str::trim)
            .filter(|cell| !cell.is_empty())
            .last()
            .unwrap_or_default()
            .to_owned()
    };
    let issuer_view = fleet_ok(&inst.tmp, &["peers"], None);
    let row = issuer_view
        .lines()
        .find(|line| line.contains(&node[..8]))
        .unwrap_or_else(|| panic!("the member is listed:\n{issuer_view}"));
    let unreached = check_of(row);
    assert!(
        unreached.contains(&format!("127.0.0.1:{member_port}")),
        "{issuer_view}"
    );
    let member_view = fleet_ok(&member.0, &["peers", "--timeout", "10"], None);
    let row = member_view
        .lines()
        .find(|line| line.split_whitespace().nth(1) == Some("issuer"))
        .unwrap_or_else(|| panic!("the issuer is listed:\n{member_view}"));
    let reached = check_of(row);
    assert_eq!(
        reached.split_whitespace().last(),
        Some("content"),
        "the issuer serves content:\n{member_view}"
    );
    assert_ne!(reached, unreached, "{member_view}\n{issuer_view}");

    let (holder, other_port) = hold_a_port();
    drop(holder);
    let other_port_word = other_port.to_string();
    let other = headless_boot::boot_with(
        "aflo",
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &["join", "--port", &other_port_word, "--host", "127.0.0.1"],
                Some(&code),
            );
            cmd.env("ATERM_STATE_HOME", tmp.join("state")).args([
                "--lines",
                "24",
                "--columns",
                "80",
            ]);
        },
        |_| true,
    );
    let (_, other_sid) = boot_session(&other);
    fleet_reads(&other, &other_sid, "listening");
    let other_node =
        std::fs::read_to_string(other.tmp.join("state/fleet-node")).expect("its node id");
    let other_node = other_node.trim();
    assert!(
        !std::fs::read_to_string(member.0.join("state/fleet-peers"))
            .expect("the member's book")
            .contains(other_node),
        "the member never named the second member"
    );

    let secret = aterm_net::fleet::read_key_file(&inst.tmp.join("state")).expect("the fleet's key");
    let issuer_node =
        std::fs::read_to_string(inst.tmp.join("state/fleet-node")).expect("the issuer's node id");
    let issuer_node = issuer_node.trim();
    let book_of = |port: u16, node: &str, key: Option<NodeKey>| {
        let target = Target {
            host: "127.0.0.1".to_owned(),
            addr: None,
            port,
            node: Some(node.to_owned()),
            key,
        };
        fetch_book(&target, (&secret, None), (Instant::now() + HANG, HANG))
    };
    let other_key = key_of(&other.tmp.join("state"));
    let issuers = book_of(
        port.parse().expect("the issuer's port"),
        issuer_node,
        Some(issuer_key),
    );
    assert!(
        matches!(&issuers, Ok(book) if book.node == issuer_node && book.proven
            && book.peers.iter().any(|peer| peer.node.as_deref() == Some(other_node)
                && peer.key == Some(other_key))
            && book.peers.iter().any(|peer| peer.node.as_deref() == Some(node)
                && peer.key == Some(member_key))),
        "the issuer proves the key the member binds, and its book binds each member's: \
         {issuers:?}"
    );
    let claimed = book_of(
        port.parse().expect("the issuer's port"),
        issuer_node,
        Some(member_key),
    );
    assert!(
        matches!(&claimed, Err(BookUnread::Unreached(aterm_net::fleet_member::Unreached::Impostor { node, .. })) if node == issuer_node),
        "{claimed:?}"
    );

    let taker = NodeIdentity::from_seed(node, &[0x99; 32]).expect("a node key");
    let mut reached = dial(
        "127.0.0.1",
        None,
        port.parse().expect("the issuer's port"),
        (&secret, Some(&taker)),
        FleetOp::Join,
        HANG,
    )
    .expect("the issuer proves join");
    let refused = register_by(
        &mut reached,
        &Registration {
            node: node.to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 7100,
            version: "0.106.0".to_owned(),
            key: Some(taker.key()),
        },
        Instant::now() + HANG,
    );
    assert!(
        refused.as_ref().is_err_and(|why| why.contains(&format!(
            "`aterm fleet peers rm {}` on the issuer",
            &node[..8]
        ))),
        "{refused:?}"
    );
    let book = std::fs::read_to_string(inst.tmp.join("state/fleet-peers")).expect("the book");
    assert!(
        book.lines()
            .any(|line| line.contains(&format!("node={node} key={}", member_key.hex()))),
        "the member's entry stands: {book}"
    );
    let members = book_of(other_port, other_node, Some(other_key));
    assert!(
        matches!(&members, Err(BookUnread::NoBook { node: Some(said), .. }) if said == other_node),
        "{members:?}"
    );

    let own_book = read_peers(&member.0.join("state")).expect("the member's book");
    let me = read_node_identity(&member.0.join("state")).expect("the member's node key");
    let (reads, whole) = read_whole(
        &own_book,
        &Keyring::of(&own_book),
        Some(node),
        (&secret, me.as_ref()),
        HANG,
        "survey",
    );
    assert!(matches!(whole, BookRead::Fetched(_)), "{whole:?}");
    let reached: Vec<(Via, Option<&str>, bool)> = reads
        .iter()
        .map(|read| {
            let surveyed = read.read.as_ref().unwrap_or_else(|e| panic!("{e}"));
            (read.peer.via, surveyed.node.as_deref(), surveyed.proven)
        })
        .collect();
    assert_eq!(
        reached,
        [
            (Via::Issuer, Some(issuer_node), true),
            (Via::IssuerBook, Some(other_node), true)
        ],
        "the issuer, then the member its book names, each proven by the key bound to it"
    );
    assert!(
        reads[1].read.as_ref().is_ok_and(|surveyed| surveyed
            .lines
            .iter()
            .any(|line| line.contains(&format!(" sid={other_sid} ")))),
        "{:?}",
        reads[1].read
    );

    let (closed, closed_port) = hold_a_port();
    drop(closed);
    let add = |port: u16| {
        fleet_ok(
            &member.0,
            &["peers", "add", &format!("127.0.0.1:{port}")],
            None,
        );
    };
    add(closed_port);
    let read = ctl_in(&member.0, &["survey", "--fleet", "--timeout", "10"]);
    let text = String::from_utf8_lossy(&read.stdout);
    assert!(read.status.success(), "{read:?}");
    let rows: Vec<&str> = text.lines().filter(|l| l.starts_with("== ")).collect();
    assert_eq!(
        rows.len(),
        4,
        "this machine and three peers — the issuer, the member its book names, the one named \
         by hand — one row each:\n{text}"
    );
    assert!(
        rows.iter()
            .any(|r| r.starts_with("== this machine did not answer: ")),
        "no aterm runs on the member:\n{text}"
    );
    let reason = |text: &str, port: u16| {
        let head = format!("== 127.0.0.1:{port} did not answer: ");
        text.lines()
            .find_map(|r| r.strip_prefix(&head)?.strip_suffix(" trust=screen"))
            .map(str::to_string)
            .unwrap_or_else(|| panic!("an unreached row for :{port}:\n{text}"))
    };
    let refused = reason(&text, closed_port);
    assert!(
        rows.iter()
            .any(|r| r.starts_with(&format!("== fleet 127.0.0.1:{port} "))
                && r.contains(" pid ")
                && r.ends_with(" trust=screen")),
        "the issuer's instance answered:\n{text}"
    );
    assert!(
        text.lines().any(|l| l.starts_with(&sid)),
        "the issuer's session is listed:\n{text}"
    );
    assert!(
        rows.iter().any(
            |r| r.starts_with(&format!("== fleet 127.0.0.1:{other_port} "))
                && r.contains(" pid ")
                && r.ends_with(" trust=screen")
        ),
        "the issuer's book led the read to the second member:\n{text}"
    );
    assert!(
        text.lines().any(|l| l.starts_with(&other_sid)),
        "the second member's session is listed:\n{text}"
    );
    let copy = std::fs::read_to_string(member.0.join("state/fleet-issuer-book"))
        .expect("the member's copy of the issuer's book");
    assert!(
        copy.lines()
            .any(|line| line.contains(&format!("node={other_node}"))
                && line.contains(&format!("port={other_port}"))
                && line.ends_with(" via=issuer-book")),
        "the member keeps a copy of the issuer's entry: {copy}"
    );
    let book =
        std::fs::read_to_string(member.0.join("state/fleet-peers")).expect("the member's book");
    assert!(
        !book.contains("issuer-book"),
        "the member's book holds no copy: {book}"
    );
    let listed = fleet_ok(&member.0, &["peers"], None);
    assert!(
        listed.lines().any(|line| line.contains(&other_node[..8])
            && line.split_whitespace().nth(1) == Some("issuer-book")),
        "{listed}"
    );

    let (silent, silent_port) = hold_a_port();
    add(silent_port);
    let read = ctl_in(&member.0, &["survey", "--fleet", "--timeout", "0.5"]);
    let text = String::from_utf8_lossy(&read.stdout);
    assert_ne!(refused, reason(&text, silent_port), "{text}");

    let ambiguous = ctl_in(&member.0, &["survey", "--host", "127.0.0.1"]);
    assert_eq!(ambiguous.status.code(), Some(1), "{ambiguous:?}");
    assert!(ambiguous.stdout.is_empty(), "{ambiguous:?}");
    let named = String::from_utf8_lossy(&ambiguous.stderr);
    for each in [port.as_str(), &other_port.to_string()] {
        assert!(named.contains(&format!(":{each}")), "{ambiguous:?}");
    }
    for each in [closed_port, silent_port] {
        assert!(!named.contains(&format!(":{each}")), "{ambiguous:?}");
    }
    let one = ctl_in(
        &member.0,
        &[
            "survey",
            "--host",
            &format!("127.0.0.1:{port}"),
            "--timeout",
            "10",
        ],
    );
    let text = String::from_utf8_lossy(&one.stdout);
    assert!(one.status.success(), "{one:?}");
    assert_eq!(
        text.lines().filter(|l| l.starts_with("== ")).count(),
        1,
        "{text}"
    );
    assert!(text.lines().any(|l| l.starts_with(&sid)), "{text}");
    drop(silent);

    let left = fleet_ok(&member.0, &["leave"], None);
    assert!(left.contains("fleet.key"), "{left}");
    assert!(!member.0.join("state").join(KEY_FILE).exists());
    let alone = ctl_in(&member.0, &["survey", "--fleet"]);
    let text = String::from_utf8_lossy(&alone.stdout);
    assert_eq!(alone.status.code(), Some(1), "nothing answered: {alone:?}");
    let rows: Vec<&str> = text.lines().filter(|l| l.starts_with("== ")).collect();
    assert_eq!(rows.len(), 2, "{text}");
    assert!(
        rows.iter()
            .all(|r| r.contains(" did not answer: ") && r.ends_with(" trust=screen")),
        "{text}"
    );
    let refused = ctl_in(&member.0, &["survey", "--host", "lab-mini"]);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(refused.stdout.is_empty(), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("ERR "),
        "{refused:?}"
    );
}

fn drive_in(root: &Path, host: &str, args: &[&str]) -> Output {
    let mut all = vec!["--host", host];
    all.extend_from_slice(args);
    ctl_in(root, &all)
}

fn drive_fed(root: &Path, host: &str, args: &[&str], stdin: &[u8]) -> Output {
    let mut all = vec!["--host", host];
    all.extend_from_slice(args);
    ctl_fed(root, &all, Some(stdin))
}

fn drive_ok(root: &Path, host: &str, args: &[&str]) -> (String, String) {
    let out = drive_in(root, host, args);
    assert!(
        out.status.success(),
        "aterm ctl --host {host} {args:?}: {out:?}"
    );
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn drive_refused(root: &Path, host: &str, args: &[&str]) -> String {
    let out = drive_in(root, host, args);
    assert_eq!(
        out.status.code(),
        Some(1),
        "aterm ctl --host {host} {args:?} must be refused: {out:?}"
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_mac_its_member_lets_drive_it_drives_a_worker_and_no_other_does() {
    let (holder, port) = hold_a_port();
    drop(holder);
    let port_word = port.to_string();
    let issuer = headless_boot::boot_with(
        "afdi",
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &[
                    "init",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port_word,
                    "--allow",
                    "content",
                ],
                None,
            );
            cmd.env("ATERM_STATE_HOME", tmp.join("state")).args([
                "--lines",
                "24",
                "--columns",
                "80",
            ]);
        },
        |_| true,
    );
    let (_, issuer_sid) = boot_session(&issuer);
    fleet_reads(&issuer, &issuer_sid, "listening");
    let code = std::fs::read_to_string(issuer.tmp.join("state/fleet-join-code")).expect("a code");
    let issuer_node =
        std::fs::read_to_string(issuer.tmp.join("state/fleet-node")).expect("its node id");
    let issuer_node = issuer_node.trim().to_owned();

    let stranger = World(headless_boot::scratch_root("afds"));
    let (holder, stranger_port) = hold_a_port();
    drop(holder);
    fleet_ok(
        &stranger.0,
        &[
            "join",
            "--host",
            "127.0.0.1",
            "--port",
            &stranger_port.to_string(),
        ],
        Some(&code),
    );

    let (holder, member_port) = hold_a_port();
    drop(holder);
    let member_port_word = member_port.to_string();
    let mut allowed = String::new();
    let member = headless_boot::boot_with(
        "afdm",
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &["join", "--host", "127.0.0.1", "--port", &member_port_word],
                Some(&code),
            );
            allowed = fleet_ok(tmp, &["allow", "drive", "--from", &issuer_node], None);
            let cfg = tmp.join("cfg/aterm/aterm.toml");
            let base = std::fs::read_to_string(&cfg).expect("the isolation config");
            assert!(
                base.contains(crate::launch_isolation::HARNESS_OFF),
                "{base}"
            );
            std::fs::write(
                &cfg,
                format!(
                    "{}[harness]\nrelaunch = false\nupgrade = false\n",
                    base.replace(crate::launch_isolation::HARNESS_OFF, "")
                ),
            )
            .expect("write the config");
            let bin = tmp.join("bin");
            headless_boot::standin(&bin.join("claude"), STANDIN_CLAUDE);
            std::fs::create_dir_all(tmp.join("work")).expect("the worker's folder");
            cmd.env("SHELL", "/bin/zsh")
                .env("ATERM_STATE_HOME", tmp.join("state"))
                .env("ATERM_DEBUG_PERSON_INVOKE", "1")
                .env("ATERM_DEBUG_FLEET_RETRY_MS", NEVER_MS)
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
                )
                .args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    );
    assert!(
        allowed.contains(&format!("drive_from = [\"{issuer_node}:")),
        "the grant holds the node with its key: {allowed}"
    );
    let (_, worker) = boot_session(&member);
    fleet_reads(&member, &worker, "listening");
    let target = format!("127.0.0.1:{member_port}");
    let at = format!("@{worker}");

    let (read, _) = drive_ok(&issuer.tmp, &target, &[&at, "status"]);
    assert!(
        read.starts_with("OK schema=") && field(&read, "fleet") == Some("listening"),
        "the member's own status: {read}"
    );
    ctl_ok(&member, &[&at, "task", "mark", "working"]);
    ctl_ok(&member, &[&at, "task", "mark", "done", "why=landed"]);

    let (_, verdict) = drive_ok(
        &issuer.tmp,
        &target,
        &[&at, "turn", "timeout=20000", "true"],
    );
    assert!(verdict.contains("submitted=1"), "{verdict}");
    let task_rev = field(&verdict, "task_rev")
        .unwrap_or_else(|| panic!("a turn into a worker names its rev: {verdict}"))
        .to_owned();
    let record = ctl_ok(&member, &[&at, "task"]);
    assert_eq!(field(&record, "why"), Some("turn"), "{record}");
    let typed = status(&member, &worker);
    let credit = format!(
        "127.0.0.1%20(node%20{})%20over%20the%20fleet",
        &issuer_node[..8]
    );
    assert_eq!(
        field(&typed, "typed_by"),
        Some(credit.as_str()),
        "the member says who typed: {typed}"
    );
    let log = ctl_ok(&member, &["messages"]);
    assert!(
        log.lines()
            .any(|row| row.contains("over%20the%20fleet%20typed%20into")
                || row.contains("over the fleet typed into")),
        "the member's Messages records the write: {log}"
    );

    let tasks = member.tmp.join("state/tasks");
    std::fs::create_dir_all(&tasks).expect("tasks");
    let md = tasks.join(format!("{worker}-1.result.md"));
    std::fs::write(&md, "Driven from the issuer.").expect("result");
    std::fs::write(
        md.with_extension("src"),
        "source=transcript session=0b2299fa uuid=d-1\n",
    )
    .expect("sidecar");
    let path: String = md
        .to_string_lossy()
        .bytes()
        .map(|b| {
            if b.is_ascii_graphic() && b != b'%' {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    ctl_ok(
        &member,
        &[
            &at,
            "task",
            "mark",
            "done",
            "why=landed",
            "result_bytes=23",
            &format!("result_path={path}"),
        ],
    );
    let (rows, _) = drive_ok(
        &issuer.tmp,
        &target,
        &[
            "await",
            "task",
            &format!("@{worker}:{task_rev}"),
            "timeout=60000",
        ],
    );
    assert!(
        rows.starts_with(&format!("@{worker} rev=")) && field(&rows, "state") == Some("done"),
        "{rows}"
    );
    let (text, stamp) = drive_ok(&issuer.tmp, &target, &[&at, "result"]);
    assert_eq!(text, "Driven from the issuer.\n", "{stamp}");

    let refused = drive_refused(&issuer.tmp, &target, &[&at, "inbox"]);
    assert!(refused.contains("ERR denied: a drive serves"), "{refused}");
    let refused = drive_refused(&issuer.tmp, &target, &[&at, "close"]);
    assert!(
        refused.contains("ERR denied: a drive closes only a worker it spawned"),
        "{refused}"
    );
    let work = member.tmp.join("work");
    let work = work.to_str().expect("a UTF-8 scratch path");
    let spawned = drive_fed(
        &issuer.tmp,
        &target,
        &["spawn", "agent=claude", &format!("cwd={work}"), "-"],
        b"a task from the issuer",
    );
    let stdout = String::from_utf8_lossy(&spawned.stdout);
    assert!(
        spawned.status.success(),
        "spawn over the fleet: {spawned:?}"
    );
    let spawned_sid = stdout
        .split_whitespace()
        .find(|w| w.starts_with("s-"))
        .unwrap_or_else(|| panic!("a sid: {stdout}"))
        .to_owned();
    let record = ctl_ok(&member, &[&format!("@{spawned_sid}"), "task"]);
    assert!(
        record.starts_with("OK task rev="),
        "a worker's record: {record}"
    );
    let (closed, _) = drive_ok(&issuer.tmp, &target, &[&format!("@{spawned_sid}"), "close"]);
    assert!(closed.starts_with("OK closed"), "{closed}");

    fleet_ok(&stranger.0, &["peers", "--timeout", "10"], None);
    fleet_ok(&member.tmp, &["peers", "--timeout", "10"], None);
    let refused = drive_refused(&stranger.0, &target, &[&at, "status"]);
    assert!(
        refused.contains("ERR denied: node ")
            && refused.contains("is not one this Mac lets drive it"),
        "{refused}"
    );

    let (took, _) = drive_ok(&issuer.tmp, &target, &["hold", &worker, "on"]);
    assert_eq!(took.trim(), "OK hold=1", "{took}");
    let held = status(&member, &worker);
    assert_eq!(field(&held, "hold_by"), Some("remote"), "{held}");
    let local = ctl(&member, &[&at, "send", "x"]);
    assert_eq!(local.status.code(), Some(1), "{local:?}");
    assert!(
        String::from_utf8_lossy(&local.stderr).contains("origin=remote"),
        "{local:?}"
    );
    drive_ok(&issuer.tmp, &target, &[&at, "send", "x"]);
    let holds_read = || {
        let read = ctl_in(
            &issuer.tmp,
            &["survey", "--fleet", "--json", "--timeout", "10"],
        );
        let text: String = String::from_utf8_lossy(&read.stdout)
            .split_whitespace()
            .collect();
        text.contains(&format!("\"held_from\":\"{issuer_node}\""))
    };
    assert!(
        holds_read(),
        "the member's row names the node that holds it"
    );

    ctl_ok(&member, &["invoke", "TakeOver"]);
    let held = status(&member, &worker);
    assert_eq!(field(&held, "hold_by"), Some("person"), "{held}");
    let refused = drive_refused(&issuer.tmp, &target, &[&at, "send", "x"]);
    assert!(
        refused.contains("ERR halted") && refused.contains("origin=person"),
        "{refused}"
    );
    let refused = drive_refused(&issuer.tmp, &target, &["hold", &worker, "on"]);
    assert!(refused.contains("origin=person"), "{refused}");
    ctl_ok(&member, &["invoke", "HandBack"]);
    let free = status(&member, &worker);
    assert_eq!(field(&free, "hold_by"), Some("-"), "{free}");
    assert!(
        !holds_read(),
        "a hold the person took is no longer the issuer's"
    );
    ctl_ok(&member, &[&at, "send", "y"]);

    let (took, _) = drive_ok(&issuer.tmp, &target, &["hold", &worker, "on"]);
    assert_eq!(took.trim(), "OK hold=1", "{took}");
    let cfg = member.tmp.join("cfg/aterm/aterm.toml");
    unloadable(&cfg, true);
    fleet_ok(&member.tmp, &["allow", "content"], None);
    let refused = drive_refused(&issuer.tmp, &target, &[&at, "status"]);
    assert!(
        refused.contains("is not one this Mac lets drive it"),
        "a grant taken back refuses the next drive before the listener moves: {refused}"
    );
    unloadable(&cfg, false);
    hold_ends(&member, &worker);
    ctl_ok(&member, &[&at, "send", "z"]);
}

const NEVER_MS: &str = "600000";

const UNLOADABLE: &str = "font_px = \"not a number\"\n";

fn unloadable(cfg: &Path, unload: bool) {
    let text = std::fs::read_to_string(cfg).expect("the config");
    let text = if unload {
        format!("{UNLOADABLE}{text}")
    } else {
        text.replacen(UNLOADABLE, "", 1)
    };
    std::fs::write(cfg, text).expect("write the config");
}

fn hold_ends(inst: &Instance, sid: &str) {
    let mut last = String::new();
    let ended = aterm_spec::wait::within(HANG, POLL_GAP, || {
        last = status(inst, sid);
        (field(&last, "hold_by") == Some("-")).then_some(())
    });
    assert!(ended.is_some(), "the remote hold never ended: {last}");
}

#[test]
fn a_grant_taken_back_while_aterm_toml_does_not_load_still_ends_the_drive() {
    let (holder, port) = hold_a_port();
    drop(holder);
    let port_word = port.to_string();
    let issuer = headless_boot::boot_with(
        "afri",
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &["init", "--host", "127.0.0.1", "--port", &port_word],
                None,
            );
            cmd.env("ATERM_STATE_HOME", tmp.join("state")).args([
                "--lines",
                "24",
                "--columns",
                "80",
            ]);
        },
        |_| true,
    );
    let (_, issuer_sid) = boot_session(&issuer);
    fleet_reads(&issuer, &issuer_sid, "listening");
    let code = std::fs::read_to_string(issuer.tmp.join("state/fleet-join-code")).expect("a code");
    let issuer_node =
        std::fs::read_to_string(issuer.tmp.join("state/fleet-node")).expect("its node id");
    let issuer_node = issuer_node.trim().to_owned();
    let (holder, member_port) = hold_a_port();
    drop(holder);
    let member_port_word = member_port.to_string();
    let member = headless_boot::boot_with(
        "afrm",
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &["join", "--host", "127.0.0.1", "--port", &member_port_word],
                Some(&code),
            );
            fleet_ok(tmp, &["allow", "drive", "--from", &issuer_node], None);
            cmd.env("ATERM_STATE_HOME", tmp.join("state"))
                .env("ATERM_DEBUG_FLEET_RETRY_MS", RETRY_MS)
                .args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    );
    let (_, worker) = boot_session(&member);
    fleet_reads(&member, &worker, "listening");
    let target = format!("127.0.0.1:{member_port}");
    let (took, _) = drive_ok(&issuer.tmp, &target, &["hold", &worker, "on"]);
    assert_eq!(took.trim(), "OK hold=1", "{took}");
    unloadable(&member.tmp.join("cfg/aterm/aterm.toml"), true);
    fleet_ok(&member.tmp, &["allow", "content"], None);
    hold_ends(&member, &worker);
    drive_refused(&issuer.tmp, &target, &[&format!("@{worker}"), "status"]);
}
