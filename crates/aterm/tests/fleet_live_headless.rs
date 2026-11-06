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
    BookRead, BookUnread, DEFAULT_PORT, Keyring, LOCK_HELD_LINE, LOCK_TOLD_SEAM, LOCK_WAIT_SEAM,
    Registration, Target, Unregistered, Via, dial, fetch_book, read_node_identity, read_peers,
    read_whole, register_by,
};

use crate::headless_boot::{
    self, Instance, POLL_GAP, boot_session, ctl, ctl_ok, field, status, watchers,
};

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

/// The handshake a test world's real `aterm` gives one address is the bound the world's seam names,
/// not the 3 s a person's dial allows: the hang detector its other waits have, or here a tenth of a
/// second, against a port that takes the connection and never answers, so that the words show it.
#[test]
fn a_dialers_handshake_at_one_address_is_bound_by_the_worlds_seam() {
    let issuer = World(headless_boot::scratch_root("afbi"));
    let (holder, port) = hold_a_port();
    drop(holder);
    fleet_ok(
        &issuer.0,
        &["init", "--host", "127.0.0.1", "--port", &port.to_string()],
        None,
    );
    let code = std::fs::read_to_string(issuer.0.join("state/fleet-join-code")).expect("its code");
    let (silent, silent_port) = hold_a_port();
    let member = World(headless_boot::scratch_root("afbm"));
    let (holder, member_port) = hold_a_port();
    drop(holder);
    let refused = fleet_with(
        &member.0,
        &[
            "join",
            "--host",
            "127.0.0.1",
            "--port",
            &member_port.to_string(),
            "--at",
            &format!("127.0.0.1:{silent_port}"),
        ],
        Some(&code),
        &[(HANDSHAKE_SEAM, "100")],
    );
    drop(silent);
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success() && said.contains("no fleet handshake within 0.1s"),
        "{refused:?}"
    );
}

/// A drive a test world's real `aterm ctl --host` makes is bound by the world's seam, not the 28 s
/// a person's drive allows, and each leg of an `await task` over one by its wait and that seam,
/// not the 8 s past it a person's allows: the hang detector its other waits have, or here a tenth
/// of a second, against a node the book binds at a port that takes the connection and never
/// answers.
#[test]
fn a_drives_deadline_is_the_worlds_seam() {
    use aterm_net::fleet_member::{Began, Peer, node_identity, record, write_held};
    let driver = World(headless_boot::scratch_root("afdw"));
    let (holder, port) = hold_a_port();
    drop(holder);
    fleet_ok(
        &driver.0,
        &["init", "--host", "127.0.0.1", "--port", &port.to_string()],
        None,
    );
    let elsewhere = driver.0.join("another-mac");
    aterm_uds::private_file::ensure_private_dir(&elsewhere).expect("its state");
    let other = node_identity(&elsewhere).expect("another Mac's node");
    let (silent, silent_port) = hold_a_port();
    write_held(&driver.0.join("state"), Began::Here, |book| {
        book.update(|peers| {
            record(
                peers,
                Peer {
                    node: Some(other.node().to_owned()),
                    key: Some(other.key()),
                    key_from: None,
                    host: "127.0.0.1".to_owned(),
                    addr: Some(std::net::Ipv4Addr::LOCALHOST.into()),
                    port: silent_port,
                    version: Some("0.146.0".to_owned()),
                    seen_ms: Some(1_793_160_000_000),
                    via: Via::Joined,
                    fleet: None,
                    signed: None,
                },
            )
        })
        .expect("record the silent member");
    })
    .expect("its book");
    for verb in [&["@s-1", "status"][..], &["await", "task", "@s-1"]] {
        let mut args = vec!["--host", other.node()];
        args.extend_from_slice(verb);
        let refused = ctl_with(&driver.0, &args, None, &[(DRIVE_WITHIN_SEAM, "100")]);
        let said = String::from_utf8_lossy(&refused.stderr);
        assert!(
            !refused.status.success() && said.contains("within 0.1s"),
            "{verb:?}: {refused:?}"
        );
    }
    drop(silent);
}

/// A test world's aterm gives each member its heartbeat dials the world's hang detector, not its
/// share of a person's round: its dial of a member whose place takes the connection and answers
/// nothing is still open a second past that share.
/// NEGATIVE CONTROL: without the world's [`HEARTBEAT_WITHIN_SEAM`] the dial hangs up at the share.
#[test]
fn a_worlds_heartbeat_waits_the_hang_detector_at_each_member() {
    use aterm_net::fleet_member::{
        Began, HEARTBEAT_MEMBERS, Peer, node_identity, record, write_held,
    };
    let (silent, silent_port) = hold_a_port();
    let (holder, port) = hold_a_port();
    drop(holder);
    let _inst = boot_world(
        "afhb",
        |tmp, cmd| {
            let state = tmp.join("state");
            write_key(&state, &SECRET);
            node_identity(&state).expect("this Mac's node");
            let elsewhere = tmp.join("another-mac");
            aterm_uds::private_file::ensure_private_dir(&elsewhere).expect("its state");
            let other = node_identity(&elsewhere).expect("another Mac's node");
            write_held(&state, Began::Here, |book| {
                book.update(|peers| {
                    record(
                        peers,
                        Peer {
                            node: Some(other.node().to_owned()),
                            key: Some(other.key()),
                            key_from: None,
                            host: "127.0.0.1".to_owned(),
                            addr: Some(std::net::Ipv4Addr::LOCALHOST.into()),
                            port: silent_port,
                            version: Some("0.146.0".to_owned()),
                            seen_ms: Some(1_793_160_000_000),
                            via: Via::Joined,
                            fleet: None,
                            signed: None,
                        },
                    )
                })
                .expect("record the silent member");
            })
            .expect("its book");
            let config = tmp.join("cfg/aterm/aterm.toml");
            let text = std::fs::read_to_string(&config).expect("the scratch config");
            std::fs::write(&config, format!("{text}{}", fleet_table(port, "facts")))
                .expect("write the fleet table");
            cmd.env("ATERM_STATE_HOME", &state)
                .args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    );
    silent
        .set_nonblocking(true)
        .expect("poll the member's place");
    let (mut dial, _) = aterm_spec::wait::within(HANG, POLL_GAP, || silent.accept().ok())
        .expect("the heartbeat dials the member");
    dial.set_nonblocking(false).expect("block on the dial");
    let share = PERSONS_HEARTBEAT_ROUND / (HEARTBEAT_MEMBERS as u32 + 1);
    let held = Instant::now() + share + Duration::from_secs(1);
    let mut hello = [0u8; 4096];
    loop {
        let left = held.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        dial.set_read_timeout(Some(left)).expect("bound the read");
        match dial.read(&mut hello) {
            Ok(0) => panic!("the heartbeat hung up on the member within a person's {share:?}"),
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => panic!("the heartbeat cut its dial of the member: {e}"),
        }
    }
}

/// A test world's aterm serves a proved connection as long as its client keeps it, not the 30 s a
/// person's listener allows: a peer that asks past them is answered.
/// NEGATIVE CONTROL: without the world's [`LIFETIME_SEAM`] the listener cuts it at 30 s.
#[test]
fn a_worlds_aterm_serves_a_connection_past_a_persons_lifetime() {
    let (holder, port) = hold_a_port();
    drop(holder);
    let inst = boot(port);
    let (_, sid) = boot_session(&inst);
    fleet_reads(&inst, &sid, "listening");
    let tcp = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let secret = FleetSecret::new(&SECRET).expect("a fleet secret");
    let (mut peer, _) =
        client_handshake(tcp, &secret, FleetOp::Survey, HANG).expect("a proved peer");
    peer.stream()
        .get_ref()
        .set_read_timeout(Some(HANG))
        .expect("bound the reads");
    std::thread::sleep(aterm_net::fleet::SERVED_WITHIN + Duration::from_secs(2));
    let reply = ask(peer.stream(), "version");
    assert!(reply.starts_with("OK version="), "{reply:?}");
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

// The dev seams that set how long a verb's read of its issuer's book, a join's dial and
// registration, a local `hold off`'s proof of a holder, each listener's TLS and proofs and each
// dialer's handshake at one address, a drive (`aterm ctl --host`, and each leg of an `await task`
// over one past the wait it asks for), a `peers` read whose every listener must answer
// ([`peers_answered`]) and a survey whose every peer must answer ([`survey_answered`]) wait. Each
// issuer a verb here reads, each Mac a drive here reaches and each dialer of a listener is an
// aterm (or this test) on this machine that serves, each holder a `hold off` proves serves or is
// refused at once, and so do each listener a [`peers_answered`] read dials (with the aterm here
// whose status it asks, which answers or runs nowhere) and each peer a [`survey_answered`] read
// dials, so each answer comes or ends however loaded the machine is: each is given the hang
// detector (a survey's peer read twice it, [`survey_answered`]), not the 3 s a person's verb or a
// listener allows, the 28 s a drive allows (8 s past an `await task` leg's wait), nor the 10 s a
// survey's `--timeout` allows at most. The peers here that answer nothing (a held silent port, a
// stopped aterm) are read under each verb's own wait (its `--timeout`, or the one it keeps),
// which these seams do not reach but a drive's: no [`peers_answered`] or [`survey_answered`] read
// meets one, and the drives that do name a tenth of a second for theirs
// ([`a_drives_deadline_is_the_worlds_seam`]). The one `--host any` read that does, past the member
// `a_walled_codex_member_is_never_picked_by_any_host_end_to_end` stopped, dials a port that refuses
// at once, so the survey's seam costs it nothing.
// Every aterm here, and every verb, is given the hang detector for the book's lock too
// ([`LOCK_WAIT_SEAM`]), not the 2 s a person's verb waits: each holder is an aterm (or this test)
// on this machine that lets it go once its read-modify-write is made, which a loaded machine can
// keep past those 2 s. And every aterm here gives each member its heartbeat dials the hang
// detector ([`HEARTBEAT_WITHIN_SEAM`]), not its share of a person's 10 s round, and serves each
// proved connection as long as its client keeps it ([`LIFETIME_SEAM`]), not the 30 s a person's
// listener allows: each member and each client is an aterm (or this test) on this machine.
const ISSUER_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_ISSUER_WITHIN_MS";
const JOIN_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_JOIN_WITHIN_MS";
const PEER_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_PEER_WITHIN_MS";
const SURVEY_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_SURVEY_WITHIN_MS";
const HOLDER_SEAM: &str = "ATERM_DEBUG_FLEET_HOLDER_WITHIN_MS";
const DRIVE_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_DRIVE_WITHIN_MS";
const HEARTBEAT_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_HEARTBEAT_WITHIN_MS";
const LIFETIME_SEAM: &str = "ATERM_DEBUG_FLEET_LIFETIME_MS";
use aterm_net::fleet::HANDSHAKE_SEAM;

/// The longest a person's heartbeat round takes, each member it dials given a share
/// ([`aterm_net::fleet_member::HEARTBEAT_MEMBERS`] of them and the adverts): what
/// [`HEARTBEAT_WITHIN_SEAM`] lifts.
const PERSONS_HEARTBEAT_ROUND: Duration = Duration::from_secs(10);

#[expect(
    clippy::disallowed_types,
    reason = "headless_boot::boot_with's hook, on the binary under test it isolates"
)]
fn boot_world(
    prefix: &str,
    configure: impl FnOnce(&Path, &mut std::process::Command),
    ready: impl Fn(&Instance) -> bool,
) -> Instance {
    headless_boot::boot_with(
        prefix,
        |tmp, cmd| {
            world_seams(cmd);
            configure(tmp, cmd);
        },
        ready,
    )
}

/// The loopback guard and the seams every aterm of a test's fleet runs under.
#[expect(
    clippy::disallowed_types,
    reason = "headless_boot::boot_with's hook, on the binary under test it isolates"
)]
fn world_seams(cmd: &mut std::process::Command) {
    let hang = HANG.as_millis().to_string();
    let round = HANG
        .saturating_mul(aterm_net::fleet_member::HEARTBEAT_MEMBERS as u32 + 1)
        .as_millis()
        .to_string();
    cmd.env(aterm_net::loopback::SEAM, "1")
        .env(ISSUER_WITHIN_SEAM, &hang)
        .env(HOLDER_SEAM, &hang)
        .env(HANDSHAKE_SEAM, &hang)
        .env(LOCK_WAIT_SEAM, &hang)
        .env(HEARTBEAT_WITHIN_SEAM, round)
        .env(LIFETIME_SEAM, NEVER_MS);
}

fn boot(port: u16) -> Instance {
    boot_on("aflt", &fleet_table(port, "facts"), true)
}

fn boot_on(prefix: &str, table: &str, guarded: bool) -> Instance {
    boot_world(
        prefix,
        |tmp, cmd| {
            let state = tmp.join("state");
            write_key(&state, &SECRET);
            let config = tmp.join("cfg/aterm/aterm.toml");
            let text = std::fs::read_to_string(&config).expect("the scratch config");
            std::fs::write(&config, format!("{text}{table}")).expect("write the fleet table");
            cmd.env("ATERM_STATE_HOME", &state)
                .env("ATERM_DEBUG_FLEET_RETRY_MS", RETRY_MS)
                .args(["--lines", "24", "--columns", "80"]);
            if !guarded {
                cmd.env_remove(aterm_net::loopback::SEAM);
            }
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
        "accounts-renew start=0123456789abcdef0123456789abcdef".to_owned(),
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
    for off_loopback in ["0.0.0.0", "192.0.2.1"] {
        let elsewhere =
            format!("[fleet]\nlisten = \"{off_loopback}:{moved}\"\nallow = \"content\"\n");
        std::fs::write(
            &config,
            text.replace(&fleet_table(moved, "content"), &elsewhere),
        )
        .expect("edit the config");
        fleet_reads(&inst, &sid, "refused:loopback");
        assert!(
            TcpStream::connect(("127.0.0.1", moved)).is_err(),
            "{off_loopback}: the guard binds nothing"
        );
        std::fs::write(&config, &text).expect("put the loopback back");
        fleet_reads(&inst, &sid, "listening");
    }
}

#[test]
fn without_the_guard_a_listen_on_an_address_this_machine_lacks_is_retried() {
    // Not the default port: a private aterm refuses that one at any address before its bind.
    let inst = boot_on(
        "aflu",
        "[fleet]\nlisten = \"192.0.2.1:7101\"\nallow = \"facts\"\n",
        false,
    );
    let (_, sid) = boot_session(&inst);
    fleet_reads(&inst, &sid, "retrying");
}

struct World(PathBuf);

impl Drop for World {
    fn drop(&mut self) {
        headless_boot::remove_world(&self.0);
    }
}

fn fleet(root: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    fleet_with(root, args, stdin, &[])
}

fn fleet_with(root: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
    fleet_ended(args, spawn_fleet(root, args, stdin, env))
}

/// `aterm fleet` (`args`) started under `root`, its stdin written, with the seams every call
/// takes and `env` over them.
fn spawn_fleet(
    root: &Path,
    args: &[&str],
    stdin: Option<&str>,
    env: &[(&str, &str)],
) -> std::process::Child {
    spawn_fleet_unset(root, args, stdin, env, &[])
}

/// [`spawn_fleet`], with the variables `unset` taken out after its seams and `env`.
fn spawn_fleet_unset(
    root: &Path,
    args: &[&str],
    stdin: Option<&str>,
    env: &[(&str, &str)],
    unset: &[&str],
) -> std::process::Child {
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
    cmd.env("ATERM_STATE_HOME", root.join("state"))
        .env(aterm_net::loopback::SEAM, "1")
        .env(ISSUER_WITHIN_SEAM, HANG.as_millis().to_string())
        .env(HANDSHAKE_SEAM, HANG.as_millis().to_string())
        .env(JOIN_WITHIN_SEAM, HANG.as_millis().to_string())
        .env(LOCK_WAIT_SEAM, HANG.as_millis().to_string())
        .envs(env.iter().copied());
    for name in unset {
        cmd.env_remove(name);
    }
    let mut child = cmd.spawn().expect("spawn aterm fleet");
    if let Some(text) = stdin {
        let mut pipe = child.stdin.take().expect("its stdin");
        pipe.write_all(text.as_bytes()).expect("write its stdin");
    }
    child
}

/// What `aterm fleet` (`args`), started by [`spawn_fleet`], wrote and how it ended.
fn fleet_ended(args: &[&str], child: std::process::Child) -> Output {
    crate::child_output::wait_and_read(child, Instant::now() + HANG)
        .unwrap_or_else(|| panic!("aterm fleet {args:?} did not exit"))
}

fn ctl_in(root: &Path, args: &[&str]) -> Output {
    ctl_fed(root, args, None)
}

fn ctl_fed(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> Output {
    ctl_with(root, args, stdin, &[])
}

/// `aterm ctl survey` (`args`) where every peer the survey dials serves or refuses at once, so
/// each answers or ends: each peer's read waits twice the hang detector
/// ([`SURVEY_WITHIN_SEAM`]), so a `--host` read's copy of the issuer's book, which takes half the
/// survey's bound, waits the detector, and the command's own wait ([`HANG`]) is the one bound.
fn survey_answered(root: &Path, args: &[&str]) -> Output {
    let twice = (2 * HANG).as_millis().to_string();
    ctl_with(root, args, None, &[(SURVEY_WITHIN_SEAM, &twice)])
}

fn ctl_with(root: &Path, args: &[&str], stdin: Option<&[u8]>, env: &[(&str, &str)]) -> Output {
    ctl_as(root, Typed::Words(args), stdin, env)
}

/// How a test's `aterm ctl` gets its words.
#[derive(Clone, Copy, Debug)]
enum Typed<'a> {
    /// Each word as the test has it, no shell between.
    Words(&'a [&'a str]),
    /// One line bash reads as a person's line after `aterm ctl `, its words bash's own (an
    /// unquoted `name=~/…` is the home of the Mac bash runs on by then).
    Bash(&'a str),
}

/// `aterm ctl` with its words `typed`, under the isolation every test's client has.
fn ctl_as(root: &Path, typed: Typed<'_>, stdin: Option<&[u8]>, env: &[(&str, &str)]) -> Output {
    headless_boot::until_served(|| {
        let aterm = crate::launch_isolation::aterm_executable();
        let mut cmd = match typed {
            Typed::Words(args) => {
                #[expect(
                    clippy::disallowed_types,
                    reason = "the binary under test, isolated by launch_isolation::apply (it inherits nothing)"
                )]
                let mut cmd = std::process::Command::new(&aterm);
                cmd.arg("ctl").args(args);
                cmd
            }
            Typed::Bash(line) => {
                #[expect(
                    clippy::disallowed_types,
                    reason = "macOS's own bash running the binary under test, isolated by launch_isolation::apply (it inherits nothing)"
                )]
                let mut cmd = std::process::Command::new("/bin/bash");
                // The binary is bash's `$0`: the isolation strips every `ATERM_*` variable.
                cmd.args(["-c", &format!("exec \"$0\" ctl {line}")])
                    .arg(&aterm);
                cmd
            }
        };
        cmd.stdin(stdin.map_or_else(Stdio::null, |_| Stdio::piped()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::launch_isolation::apply(&mut cmd, root);
        cmd.env("ATERM_STATE_HOME", root.join("state"))
            .env(aterm_net::loopback::SEAM, "1")
            .env(ISSUER_WITHIN_SEAM, HANG.as_millis().to_string())
            .env(HANDSHAKE_SEAM, HANG.as_millis().to_string())
            .env(DRIVE_WITHIN_SEAM, HANG.as_millis().to_string())
            .env(LOCK_WAIT_SEAM, HANG.as_millis().to_string())
            .envs(env.iter().copied());
        let mut child = cmd.spawn().expect("spawn aterm ctl");
        if let Some(bytes) = stdin {
            let mut pipe = child.stdin.take().expect("its stdin");
            pipe.write_all(bytes).expect("write its stdin");
        }
        crate::child_output::wait_and_read(child, Instant::now() + HANG)
            .unwrap_or_else(|| panic!("aterm ctl {typed:?} did not exit"))
    })
}

/// Where the aterm under `root` listens (its `[fleet] listen`): the member a join names with
/// `--at`, since a join code names none (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17).
fn listen_of(root: &Path) -> String {
    let cfg = std::fs::read_to_string(root.join("cfg/aterm/aterm.toml")).expect("its config");
    cfg.lines()
        .find_map(|line| line.trim().strip_prefix("listen = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("a [fleet] listen")
        .to_owned()
}

fn fleet_ok(root: &Path, args: &[&str], stdin: Option<&str>) -> String {
    succeeded(args, fleet(root, args, stdin))
}

/// `aterm fleet peers` where every listener the read dials serves or refuses at once, and the
/// aterm here (whose `status` `peers` asks within the same bound) answers or runs nowhere, so each
/// answers or ends: the read waits the hang detector ([`PEER_WITHIN_SEAM`]), not the 3 s it keeps.
fn peers_answered(root: &Path) -> String {
    let hang = HANG.as_millis().to_string();
    succeeded(
        &["peers"],
        fleet_with(root, &["peers"], None, &[(PEER_WITHIN_SEAM, &hang)]),
    )
}

fn succeeded(args: &[&str], out: Output) -> String {
    assert!(
        out.status.success(),
        "aterm fleet {args:?} failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

/// The binary is no Cargo test binary, so an `init` with no `--port` writes [`DEFAULT_PORT`], where
/// an aterm of the person's may answer on this machine's loopback. Under a private state root, and
/// under a scratch `HOME` with none (a debug build that is no shell of its user's own account), the
/// verb's proof of its own listener reaches no such aterm: it stops before its connect, and its
/// report says why. The cut answers a dial past that, so this reaches no aterm with the guard gone.
#[test]
fn a_private_binarys_init_writes_the_default_port_and_never_dials_it() {
    for (prefix, state_root) in [("afpi", true), ("afph", false)] {
        let world = World(headless_boot::scratch_root(prefix));
        let cut = world.0.join("cut");
        std::fs::write(
            &cut,
            format!("127.0.0.1:{DEFAULT_PORT}\n[::1]:{DEFAULT_PORT}\n"),
        )
        .expect("write the cut");
        let cut = cut.to_str().expect("a UTF-8 world");
        let unset: &[&str] = if state_root {
            &[]
        } else {
            &["ATERM_STATE_HOME"]
        };
        let init = spawn_fleet_unset(
            &world.0,
            &["init"],
            None,
            &[(aterm_uds::partition::SEAM, cut)],
            unset,
        );
        let out = fleet_ended(&["init"], init);
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "state root {state_root}: {said}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            said.contains(&format!("listen = \"0.0.0.0:{DEFAULT_PORT}\"")),
            "state root {state_root}: {said}"
        );
        assert!(
            said.contains(&format!(
                "  listener   this private aterm refuses a dial of 127.0.0.1:{DEFAULT_PORT}: "
            )),
            "state root {state_root}: {said}"
        );
    }
}

/// The binary is no Cargo test binary, so a host named with no port is at [`DEFAULT_PORT`], where
/// another Mac's fleet listens unless told otherwise: only a test binary holds a port in its place.
#[test]
fn the_binary_places_a_host_named_with_no_port_at_the_default_port() {
    let world = World(headless_boot::scratch_root("afdp"));
    let added = fleet_ok(&world.0, &["peers", "add", "lab-maxi"], None);
    let want = format!("aterm fleet peers: added lab-maxi:{DEFAULT_PORT} to ");
    assert!(added.starts_with(&want), "{added}");
}

/// A verb that meets the book's lock held past the 2 s a shipped aterm waits for it: under
/// [`LOCK_WAIT_SEAM`], which every [`fleet`] call passes, the debug binary waits for the holder
/// to let it go, and without it the binary refuses as a shipped one does. The verb without the
/// seam is the hold's clock: it starts once the waiting verb has said it found the lock held
/// ([`LOCK_TOLD_SEAM`]), is refused only once it has found the lock held for its whole wait, and
/// the lock is let go only after that, by when the waiting verb has waited past those 2 s too.
#[test]
fn a_verb_under_the_lock_seam_waits_out_a_holder_the_shipped_wait_refuses() {
    let world = World(headless_boot::scratch_root("afbl"));
    // The first verb makes the private state directory the lock lives in.
    fleet_ok(&world.0, &["peers", "add", "lab-one"], None);
    let state = world.0.join("state");
    let held = aterm_net::fleet_member::hold_book(&state).expect("the book's lock");
    let mut waiting = spawn_fleet(
        &world.0,
        &["peers", "add", "lab-two"],
        None,
        &[(LOCK_TOLD_SEAM, "1")],
    );
    let told = crate::child_output::await_stderr_line(
        &mut waiting,
        |line| line == LOCK_HELD_LINE,
        Instant::now() + HANG,
    );
    match told {
        Ok(()) => {}
        Err(crate::child_output::NoLine::Exited(out)) => {
            panic!("the waiting verb ended before it found the lock held: {out:?}")
        }
        Err(crate::child_output::NoLine::Hung(said)) => {
            panic!("the waiting verb never said it found the lock held; its stderr: {said}")
        }
        Err(crate::child_output::NoLine::OutputLimit { stream, captured }) => {
            panic!("the waiting verb filled its {stream} first: {captured}")
        }
    }
    let shipped = fleet_with(
        &world.0,
        &["peers", "add", "lab-three"],
        None,
        &[(LOCK_WAIT_SEAM, "")],
    );
    let said = String::from_utf8_lossy(&shipped.stderr);
    assert!(
        shipped.status.code() == Some(1) && said.contains("another aterm is writing the book"),
        "a verb with the shipped wait refuses a lock held past it: {shipped:?}"
    );
    drop(held);
    succeeded(
        &["peers", "add", "lab-two"],
        fleet_ended(&["peers", "add", "lab-two"], waiting),
    );
    let hosts: Vec<String> = read_peers(&state)
        .expect("the book")
        .into_iter()
        .map(|peer| peer.host)
        .collect();
    assert!(
        hosts.iter().any(|host| host == "lab-one")
            && hosts.iter().any(|host| host == "lab-two")
            && !hosts.iter().any(|host| host == "lab-three"),
        "the waiting verb wrote its peer once the lock was let go, the refused one none: {hosts:?}"
    );
}

#[test]
fn fleet_init_and_join_through_the_binary_register_a_member() {
    let (holder, port) = hold_a_port();
    drop(holder);
    let port = port.to_string();
    let mut init = String::new();
    let inst = boot_world(
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
        &[
            "join",
            "--host",
            "127.0.0.1",
            "--port",
            &member_port,
            "--at",
            &listen_of(&inst.tmp),
        ],
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
    let member_view = peers_answered(&member.0);
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
    let other = boot_world(
        "aflo",
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &[
                    "join",
                    "--port",
                    &other_port_word,
                    "--host",
                    "127.0.0.1",
                    "--at",
                    &listen_of(&inst.tmp),
                ],
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
            signed: None,
        },
        Instant::now() + HANG,
    );
    assert!(
        matches!(&refused, Err(Unregistered::Answered(why))
            if why.contains(&format!("`aterm fleet peers rm {node}` on the issuer"))),
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
        matches!(&members, Ok(book) if book.node == other_node && book.proven
            && book.peers.iter().any(|peer| peer.node.as_deref() == Some(other_node)
                && peer.key == Some(other_key) && peer.signature_holds())
            && book.peers.iter().any(|peer| peer.node.as_deref() == Some(issuer_node)
                && peer.key == Some(issuer_key))),
        "a member serves its book as the issuer does (F17): its own signed entry, and the \
         issuer it joined through: {members:?}"
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
    let read = survey_answered(&member.0, &["survey", "--fleet"]);
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
                && line
                    .split_whitespace()
                    .any(|word| word == "via=issuer-book")),
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

    // The issuer's book says which members the word names, and the issuer serves: it must answer.
    let ambiguous = survey_answered(&member.0, &["survey", "--host", "127.0.0.1"]);
    assert_eq!(ambiguous.status.code(), Some(1), "{ambiguous:?}");
    assert!(ambiguous.stdout.is_empty(), "{ambiguous:?}");
    let named = String::from_utf8_lossy(&ambiguous.stderr);
    for each in [
        port.as_str(),
        &other_port.to_string(),
        &closed_port.to_string(),
        &silent_port.to_string(),
    ] {
        assert!(
            named.contains(&format!(":{each}")),
            "each peer the word names, a hand entry among them: {ambiguous:?}"
        );
    }
    let one = survey_answered(
        &member.0,
        &["survey", "--host", &format!("127.0.0.1:{port}")],
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

#[test]
fn a_driver_owns_a_plain_shell_without_ai_or_the_hosts_direct_command() {
    let (held, issuer_port) = hold_a_port();
    drop(held);
    let issuer = boot_world(
        "afs-issuer",
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &[
                    "init",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &issuer_port.to_string(),
                ],
                None,
            );
            cmd.env("ATERM_STATE_HOME", tmp.join("state"));
        },
        |_| true,
    );
    let (_, original_issuer) = boot_session(&issuer);
    fleet_reads(&issuer, &original_issuer, "listening");
    let code = std::fs::read_to_string(issuer.tmp.join("state/fleet-join-code")).unwrap();
    let (held, member_port) = hold_a_port();
    drop(held);
    let member = boot_world(
        "afs-member",
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &[
                    "join",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &member_port.to_string(),
                    "--at",
                    &listen_of(&issuer.tmp),
                ],
                Some(&code),
            );
            fleet_ok(tmp, &["allow", "drive", "--all"], None);
            std::fs::create_dir_all(tmp.join("work")).unwrap();
            std::fs::create_dir_all(tmp.join("home/work")).unwrap();
            cmd.env("SHELL", "/bin/zsh")
                .env("ATERM_STATE_HOME", tmp.join("state"))
                .args(["-e", "/bin/cat"]);
        },
        |_| true,
    );
    let (_, original) = boot_session(&member);
    fleet_reads(&member, &original, "listening");
    let target = format!("127.0.0.1:{member_port}");
    let cwd = format!("cwd={}", member.tmp.join("work").display());
    let (reply, _) = drive_ok(&issuer.tmp, &target, &["spawn", &cwd]);
    let sid = reply.trim().strip_prefix("OK ").expect("plain shell sid");
    let at = format!("@{sid}");
    assert_eq!(
        ctl_ok(&member, &[&at, "task"]).trim(),
        "OK task -",
        "no fake task"
    );
    aterm_spec::wait::until_within(HANG, "the plain shell's integrated prompt", || {
        let (blocks, _) = drive_ok(&issuer.tmp, &target, &[&at, "blocks"]);
        blocks.contains(" entering ").then_some(())
    });
    let command = "pwd; printf 'probe-before\\n'; i=0; while [ $i -lt 64 ]; do printf 'middle\\n'; i=$((i+1)); done; printf 'probe-after\\n'; (exit 7)";
    // A drive's turn is capped at 22 s, below the hang detector, so the turn waits only for its
    // submit (its whole timeout, not the default 2 s window) and the exit below is the detector's.
    let (_, turn) = drive_ok(
        &issuer.tmp,
        &target,
        &[
            &at,
            "turn",
            "settle=submitted",
            "submit_window=20000",
            "timeout=20000",
            command,
        ],
    );
    assert!(turn.contains("submitted=1"), "{turn}");
    let block = aterm_spec::wait::until_within(HANG, "the exact command's exit", || {
        let (blocks, _) = drive_ok(&issuer.tmp, &target, &[&at, "blocks"]);
        blocks
            .lines()
            .find(|line| line.contains(" complete ") && line.contains(" exit=7 "))
            .and_then(|line| line.split_whitespace().nth(1))
            .map(str::to_owned)
    });
    let (output, _) = drive_ok(&issuer.tmp, &target, &[&at, "blocktext", &block, "trim"]);
    let other_session = format!("@{original}");
    assert!(
        drive_refused(&issuer.tmp, &target, &[&other_session, "blocktext", &block])
            .contains("ERR denied"),
        "a block id grants no read of another session"
    );
    let expected_cwd = member.tmp.join("work").canonicalize().unwrap();
    let actual_cwd = output
        .lines()
        .find(|line| line.starts_with('/'))
        .and_then(|line| Path::new(line).canonicalize().ok());
    assert_eq!(
        actual_cwd.as_ref(),
        Some(&expected_cwd),
        "the requested physical cwd: {output}"
    );
    assert!(
        output.contains("probe-before") && output.contains("probe-after"),
        "retained output beyond the visible grid: {output}"
    );
    assert_eq!(output.lines().filter(|line| *line == "middle").count(), 64);
    // The prompt's return is the detector's; the drive's `await idle` then reads a quiet that
    // is already there.
    aterm_spec::wait::until_within(HANG, "the shell back at its prompt", || {
        (field(&status(&member, sid), "phase") == Some("idle")).then_some(())
    });
    let (idle, _) = drive_ok(
        &issuer.tmp,
        &target,
        &[&at, "await", "idle", "1", "timeout=20000"],
    );
    assert!(idle.starts_with("OK idle "), "{idle}");
    assert_eq!(
        field(&status(&member, &original), "program"),
        Some("cat"),
        "global direct command remains in its original session"
    );
    drive_ok(
        &issuer.tmp,
        &target,
        &[
            &at,
            "turn",
            "settle=submitted",
            "submit_window=20000",
            "timeout=20000",
            "/bin/cat",
        ],
    );
    aterm_spec::wait::until_within(HANG, "the owned shell's live foreground job", || {
        (field(&status(&member, sid), "program") == Some("cat")).then_some(())
    });
    assert!(drive_refused(&issuer.tmp, &target, &[&at, "close"]).contains("running job"));
    drive_ok(&issuer.tmp, &target, &[&at, "key", "ctrl+c"]);
    aterm_spec::wait::until_within(HANG, "the shell after ordinary interrupt", || {
        (field(&status(&member, sid), "phase") == Some("idle")).then_some(())
    });

    let other = World(headless_boot::scratch_root("afs-other"));
    let (held, other_port) = hold_a_port();
    drop(held);
    fleet_ok(
        &other.0,
        &[
            "join",
            "--host",
            "127.0.0.1",
            "--port",
            &other_port.to_string(),
            "--at",
            &listen_of(&issuer.tmp),
        ],
        Some(&code),
    );
    for args in [
        vec![at.as_str(), "close"],
        vec![at.as_str(), "blocks"],
        vec![at.as_str(), "blocktext", block.as_str()],
    ] {
        let refused = drive_refused(&other.0, &target, &args);
        assert!(refused.contains("ERR denied"), "{refused}");
    }
    let missing = format!("cwd={}", member.tmp.join("missing").display());
    assert!(drive_refused(&issuer.tmp, &target, &["spawn", &missing]).contains("no such folder"));
    // A folder named in the driven Mac's home resolves there: the shell starts in the member's
    // `~/work`, which the issuer's home does not hold. Typed as the help writes it, quoted, the
    // word reaches the client as `cwd=~/work` through bash. Negative control: unquoted, bash makes
    // it the issuer's own `~/work` first, an absolute path the member has no folder at.
    assert!(!issuer.tmp.join("home/work").exists());
    let unquoted = ctl_as(
        &issuer.tmp,
        Typed::Bash(&format!("--host {target} spawn cwd=~/work")),
        None,
        &[],
    );
    assert_eq!(unquoted.status.code(), Some(1), "{unquoted:?}");
    let refused = String::from_utf8_lossy(&unquoted.stderr);
    assert!(
        refused.contains("no such folder")
            && refused.contains(&issuer.tmp.join("home/work").display().to_string()),
        "{refused}"
    );
    let quoted = ctl_as(
        &issuer.tmp,
        Typed::Bash(&format!("--host {target} spawn 'cwd=~/work'")),
        None,
        &[],
    );
    assert!(quoted.status.success(), "{quoted:?}");
    let reply = String::from_utf8_lossy(&quoted.stdout).into_owned();
    let homed = reply.trim().strip_prefix("OK ").expect("plain shell sid");
    let at_homed = format!("@{homed}");
    aterm_spec::wait::until_within(HANG, "the home-relative shell's prompt", || {
        let (blocks, _) = drive_ok(&issuer.tmp, &target, &[&at_homed, "blocks"]);
        blocks.contains(" entering ").then_some(())
    });
    drive_ok(
        &issuer.tmp,
        &target,
        &[
            &at_homed,
            "turn",
            "settle=submitted",
            "submit_window=20000",
            "timeout=20000",
            "pwd; (exit 5)",
        ],
    );
    let pwd_block = aterm_spec::wait::until_within(HANG, "the pwd's exit", || {
        let (blocks, _) = drive_ok(&issuer.tmp, &target, &[&at_homed, "blocks"]);
        blocks
            .lines()
            .find(|line| line.contains(" complete ") && line.contains(" exit=5 "))
            .and_then(|line| line.split_whitespace().nth(1))
            .map(str::to_owned)
    });
    let (output, _) = drive_ok(
        &issuer.tmp,
        &target,
        &[&at_homed, "blocktext", &pwd_block, "trim"],
    );
    assert_eq!(
        output
            .lines()
            .find(|line| line.starts_with('/'))
            .and_then(|line| Path::new(line).canonicalize().ok()),
        Some(member.tmp.join("home/work").canonicalize().unwrap()),
        "the member's ~/work: {output}"
    );
    for (cwd, said) in [
        ("cwd=~/missing", "no such folder ~/missing"),
        ("cwd=~/Documents", "macOS guards"),
    ] {
        let refused = drive_refused(&issuer.tmp, &target, &["spawn", cwd]);
        assert!(refused.contains(said), "{cwd}: {refused}");
    }
    let leaving = drive_in(&issuer.tmp, &target, &["spawn", "cwd=~/../x"]);
    assert!(!leaving.status.success(), "{leaving:?}");
    assert!(
        String::from_utf8_lossy(&leaving.stderr).contains("may leave the home"),
        "refused here, never sent: {leaving:?}"
    );
    drive_ok(&issuer.tmp, &target, &[&at_homed, "close"]);
    drive_ok(&issuer.tmp, &target, &[&at, "close"]);
    let left = ctl_ok(&member, &["sessions"]);
    assert!(
        !left
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some(sid)),
        "only the owned shell closed: {left}"
    );
    assert!(left.contains(&original));
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
    let issuer = boot_world(
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
            "--at",
            &listen_of(&issuer.tmp),
        ],
        Some(&code),
    );

    let (holder, member_port) = hold_a_port();
    drop(holder);
    let member_port_word = member_port.to_string();
    let mut allowed = String::new();
    let member = boot_world(
        "afdm",
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &[
                    "join",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &member_port_word,
                    "--at",
                    &listen_of(&issuer.tmp),
                ],
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

    let (inbox, stamp) = drive_ok(&issuer.tmp, &target, &[&at, "inbox"]);
    assert!(inbox.is_empty(), "{inbox}");
    assert!(stamp.contains("OK 0 fleet=1 head=f0"), "{stamp}");
    let refused = drive_refused(&issuer.tmp, &target, &[&at, "close"]);
    assert!(
        refused.starts_with("aterm-ctl: ERR denied") || refused.starts_with("ERR denied"),
        "{refused}"
    );
    assert_ne!(
        field(&status(&member, &worker), "phase"),
        Some("exited"),
        "the unowned session survives"
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
    let grok = drive_fed(
        &issuer.tmp,
        &target,
        &["spawn", "agent=grok", &format!("cwd={work}"), "-"],
        b"a task from the issuer",
    );
    let refused = String::from_utf8_lossy(&grok.stderr);
    assert!(
        grok.status.code() == Some(1)
            && refused.contains(
                "spawn agent=grok: Grok Build is read, not supervised, by the aterm of the Mac \
                 asked to start it"
            )
            && !refused.contains(" here")
            && !refused.contains("this Mac"),
        "the driving Mac reads the driven Mac's refusal: {grok:?}"
    );

    peers_answered(&stranger.0);
    peers_answered(&member.tmp);
    let refused = drive_refused(&stranger.0, &target, &[&at, "status"]);
    assert!(
        refused.contains(&format!(
            "{}node ",
            aterm_types::control_verbs::DRIVE_UNGRANTED
        )) && refused.contains("is not one the dialled Mac lets drive it"),
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
    // The worker's `held_from` word in the issuer's survey: the member's row must answer and list
    // the worker, so a row that did not answer fails here rather than reading as no hold.
    let held_from = || {
        let read = survey_answered(&issuer.tmp, &["survey", "--fleet", "--json"]);
        assert!(read.status.success(), "{read:?}");
        let text = String::from_utf8_lossy(&read.stdout);
        let hosts: aterm_json::Value =
            aterm_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"));
        hosts
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|host| host.get("sessions")?.as_array())
            .flatten()
            .find(|row| row.get("sid").and_then(aterm_json::Value::as_str) == Some(worker.as_str()))
            .and_then(|row| row.get("held_from")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("the member's row lists the worker: {text}"))
    };
    assert_eq!(
        held_from(),
        issuer_node,
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
    assert_eq!(
        held_from(),
        "-",
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
        refused.contains("lets no one drive it"),
        "a grant taken back refuses the next drive at once: {refused}"
    );
    unloadable(&cfg, false);
    hold_ends(&member, &worker);
    ctl_ok(&member, &[&at, "send", "z"]);
}

/// A stand-in Claude Code a worker runs until its tab closes, so the worker stays open on the
/// board until the test closes it.
const STANDIN_CLAUDE_STAYS: &str = "#!/bin/sh\n\
    case \"$1\" in --version) echo '2.1.0 (Claude Code)'; exit 0 ;; esac\n\
    exec /bin/sleep 600\n";

/// The workers a `board --json` read (`out`, which must have succeeded) lists, each a map of its
/// row's keys.
fn board_workers(out: &Output) -> Vec<std::collections::BTreeMap<String, String>> {
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "a board read: {out:?}");
    let read: aterm_json::Value =
        aterm_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"));
    read.get("workers")
        .and_then(aterm_json::Value::as_array)
        .unwrap_or_else(|| panic!("a workers array: {text}"))
        .iter()
        .map(|row| {
            row.as_object()
                .unwrap_or_else(|| panic!("a worker object: {text}"))
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                .collect()
        })
        .collect()
}

/// The fleet task board (`docs/DESIGN-fleet-task-board-2026-11-04.md`): a dispatcher spawns two
/// labelled workers onto a member over the fleet and its session closes; its Mac's board keeps
/// each one's member, label and brief digest, never the brief; a dispatcher started on a third
/// Mac, knowing no sid, reads that board over a drive and, before either worker moves, waits on
/// every open row at the member, where the workers run, until each one moves; a Mac the
/// dispatching one does not let drive it reads nothing; and the board's keeper closes each row
/// once its worker's tab is gone.
#[test]
fn a_dispatcher_on_another_mac_resumes_every_worker_from_the_boards_open_rows() {
    let (holder, port) = hold_a_port();
    drop(holder);
    let port_word = port.to_string();
    let issuer = boot_world(
        "abdi",
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
    let (_, person) = boot_session(&issuer);
    fleet_reads(&issuer, &person, "listening");
    let code = std::fs::read_to_string(issuer.tmp.join("state/fleet-join-code")).expect("a code");
    let node_of = |root: &Path| {
        std::fs::read_to_string(root.join("state/fleet-node"))
            .expect("its node id")
            .trim()
            .to_owned()
    };
    let issuer_node = node_of(&issuer.tmp);
    let issuer_at = listen_of(&issuer.tmp);
    let joined = |tag: &str| {
        let world = World(headless_boot::scratch_root(tag));
        let (holder, port) = hold_a_port();
        drop(holder);
        fleet_ok(
            &world.0,
            &[
                "join",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--at",
                &issuer_at,
            ],
            Some(&code),
        );
        world
    };
    // The Mac a dispatcher restarts on, and one the dispatching Mac never lets drive it, both
    // joined before the member, whose join then binds them.
    let resumer = joined("abdr");
    let resumer_node = node_of(&resumer.0);
    let stranger = joined("abds");
    fleet_ok(
        &issuer.tmp,
        &["allow", "drive", "--from", &resumer_node],
        None,
    );

    let (holder, member_port) = hold_a_port();
    drop(holder);
    let member_port_word = member_port.to_string();
    let member = boot_world(
        "abdm",
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &[
                    "join",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &member_port_word,
                    "--at",
                    &issuer_at,
                ],
                Some(&code),
            );
            fleet_ok(
                tmp,
                &[
                    "allow",
                    "drive",
                    "--from",
                    &issuer_node,
                    "--from",
                    &resumer_node,
                ],
                None,
            );
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
            headless_boot::standin(&bin.join("claude"), STANDIN_CLAUDE_STAYS);
            std::fs::create_dir_all(tmp.join("work")).expect("the workers' folder");
            cmd.env("SHELL", "/bin/zsh")
                .env("ATERM_STATE_HOME", tmp.join("state"))
                .env("ATERM_DEBUG_FLEET_RETRY_MS", NEVER_MS)
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
                )
                .args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    );
    let (_, member_sid) = boot_session(&member);
    fleet_reads(&member, &member_sid, "listening");
    let member_node = node_of(&member.tmp);
    let target = format!("127.0.0.1:{member_port}");
    let work = member.tmp.join("work");
    let work = work.to_str().expect("a UTF-8 scratch path");

    // The dispatcher: a tab of its own on the issuer, one `aterm ctl` per worker in it, each
    // ended once it spawned.
    let opened = ctl_ok(&issuer, &["spawn"]);
    let dispatcher = opened
        .split_whitespace()
        .find(|w| w.starts_with("s-"))
        .unwrap_or_else(|| panic!("the dispatcher's tab: {opened}"))
        .to_owned();
    let briefs: [(&str, &[u8]); 2] = [
        ("dx1-a", b"brief one: land the board"),
        ("dx1-b", b"brief two: read it back"),
    ];
    let mut sids = Vec::new();
    for (label, brief) in briefs {
        let spawned = ctl_with(
            &issuer.tmp,
            &[
                "--host",
                &target,
                "spawn",
                "agent=claude",
                &format!("label={label}"),
                &format!("cwd={work}"),
                "-",
            ],
            Some(brief),
            &[("ATERM_PARENT_SESSION_ID", &dispatcher)],
        );
        let stdout = String::from_utf8_lossy(&spawned.stdout);
        assert!(
            spawned.status.success(),
            "spawn over the fleet: {spawned:?}"
        );
        sids.push(
            stdout
                .split_whitespace()
                .find(|w| w.starts_with("s-"))
                .unwrap_or_else(|| panic!("a sid: {stdout}"))
                .to_owned(),
        );
    }

    let here = board_workers(&ctl_in(&issuer.tmp, &["board", "--json"]));
    assert_eq!(here.len(), 2, "one row per worker: {here:?}");
    for ((label, brief), sid) in briefs.iter().zip(&sids) {
        let row = here
            .iter()
            .find(|row| row.get("sid") == Some(sid))
            .unwrap_or_else(|| panic!("{sid} on the board: {here:?}"));
        let digest: String = aterm_digest::Sha256::digest(brief)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        for (key, want) in [
            ("label", (*label).to_owned()),
            ("node", member_node.clone()),
            ("program", "claude".to_owned()),
            ("cwd", work.to_owned()),
            ("from", dispatcher.clone()),
            ("brief", format!("sha256:{digest}")),
            ("brief_bytes", brief.len().to_string()),
        ] {
            assert_eq!(
                row.get(key).map(String::as_str),
                Some(want.as_str()),
                "{key}: {row:?}"
            );
        }
        assert_ne!(row.get("state").map(String::as_str), Some("closed"));
    }
    for file in std::fs::read_dir(issuer.tmp.join("state/board")).expect("the board") {
        let bytes = std::fs::read(file.expect("a board file").path()).unwrap_or_default();
        for (_, brief) in briefs {
            assert!(
                !bytes.windows(brief.len()).any(|w| w == brief),
                "the board keeps a brief's digest, never its text"
            );
        }
    }

    // The dispatcher's session ends: what it started is on the board alone.
    let closed = ctl_ok(&issuer, &[&format!("@{dispatcher}"), "close"]);
    assert!(closed.starts_with("OK closed"), "{closed}");

    // A dispatcher restarted on the resumer's Mac knows no sid: the board names every open row.
    let there = board_workers(&drive_in(&resumer.0, &issuer_at, &["board", "--json"]));
    let open: Vec<&str> = there
        .iter()
        .filter(|row| row.get("state").map(String::as_str) != Some("closed"))
        .filter_map(|row| row.get("sid").map(String::as_str))
        .collect();
    assert_eq!(open.len(), 2, "the resumer reads both open rows: {there:?}");
    assert!(
        sids.iter().all(|sid| open.contains(&sid.as_str())),
        "{there:?}"
    );
    let (mine, _) = drive_ok(
        &resumer.0,
        &issuer_at,
        &["board", &format!("from=@{dispatcher}"), "label=dx1-a"],
    );
    let rows: Vec<&str> = mine.lines().filter(|l| l.starts_with("worker ")).collect();
    assert!(
        rows.len() == 1 && rows[0].contains(&format!("sid={}", sids[0])),
        "a filter takes one dispatcher's rows of one label: {mine}"
    );

    // The resumed wait starts while both workers work, past the revs the resumer read: a wait
    // that armed after a worker moved still answers with that move, whatever the keeper has
    // written on the board since.
    let since = there
        .iter()
        .filter_map(|row| row.get("rev")?.parse::<u64>().ok())
        .min()
        .unwrap_or_else(|| panic!("each row's rev: {there:?}"));
    // Each worker's launch line has run (its stand-in is the tab's foreground program), so its
    // launcher, which watches the tab until the shell's prompt, watches it no longer.
    for sid in &sids {
        let mut last = String::new();
        let launched = aterm_spec::wait::within(HANG, POLL_GAP, || {
            last = status(&member, sid);
            (field(&last, "program") == Some("sleep")).then_some(())
        });
        assert!(launched.is_some(), "{sid}'s launch line never ran: {last}");
    }
    let resumed = std::thread::spawn({
        let (root, at) = (resumer.0.clone(), issuer_at.clone());
        let since = format!("since={since}");
        move || {
            drive_in(
                &root,
                &at,
                &["await", "task", "board", "all", &since, "timeout=60000"],
            )
        }
    });
    // It waits at the member, beside the board's keeper, now the one other waiter on each
    // worker, with at most one leg parked at a time.
    let ended = std::cell::Cell::new(false);
    let parked = aterm_spec::wait::within(HANG, POLL_GAP, || {
        if resumed.is_finished() {
            ended.set(true);
            return Some(());
        }
        sids.iter()
            .all(|sid| watchers(&member, sid) >= 2)
            .then_some(())
    });
    if ended.get() {
        let out = resumed.join().expect("the resumed wait");
        panic!(
            "the resumed wait answered before any worker moved: {}",
            headless_boot::said(&out)
        );
    }
    assert!(
        parked.is_some(),
        "the resumed wait never waited at the member"
    );

    // The workers move at the member: one landed, one failed.
    let (a, b) = (format!("@{}", sids[0]), format!("@{}", sids[1]));
    ctl_ok(&member, &[&a, "task", "mark", "done", "why=landed"]);
    ctl_ok(
        &member,
        &[&b, "task", "mark", "failed", "why=could-not-finish"],
    );

    let out = resumed.join().expect("the resumed wait");
    assert!(
        out.status.success(),
        "the resumed wait: {}",
        headless_boot::said(&out)
    );
    let waited = String::from_utf8_lossy(&out.stdout);
    for (sid, state, label) in [(&sids[0], "done", "dx1-a"), (&sids[1], "failed", "dx1-b")] {
        let row = waited
            .lines()
            .find(|row| row.starts_with(&format!("@{sid} ")))
            .unwrap_or_else(|| {
                panic!(
                    "the resumed wait answers {sid}: {}",
                    headless_boot::said(&out)
                )
            });
        assert_eq!(field(row, "state"), Some(state), "{row}");
        assert_eq!(field(row, "label"), Some(label), "{row}");
        assert_eq!(
            field(row, "task_news"),
            Some("1"),
            "it woke on the worker's move: {row}"
        );
        assert!(
            !row.contains("read=board"),
            "the row is the member's answer, not the board's copy: {row}"
        );
    }

    let refused = drive_refused(&stranger.0, &issuer_at, &["board"]);
    assert!(
        refused.contains(aterm_types::control_verbs::DRIVE_UNGRANTED),
        "a Mac the dispatching one does not let drive it reads no board: {refused}"
    );

    // The dispatching Mac's keeper follows the workers with no dispatcher running.
    let states = |rows: &[std::collections::BTreeMap<String, String>]| -> Vec<Option<String>> {
        sids.iter()
            .map(|sid| {
                rows.iter()
                    .find(|row| row.get("sid") == Some(sid))
                    .and_then(|row| row.get("state").cloned())
            })
            .collect()
    };
    let mut last = Vec::new();
    let moved = aterm_spec::wait::within(HANG, POLL_GAP, || {
        last = board_workers(&ctl_in(&issuer.tmp, &["board", "--json"]));
        (states(&last) == [Some("done".to_owned()), Some("failed".to_owned())]).then_some(())
    });
    assert!(
        moved.is_some(),
        "the keeper records each worker's move: {last:?}"
    );
    for sid in &sids {
        let (closed, _) = drive_ok(&issuer.tmp, &target, &[&format!("@{sid}"), "close"]);
        assert!(closed.starts_with("OK closed"), "{closed}");
    }
    let kept = aterm_spec::wait::within(HANG, POLL_GAP, || {
        last = board_workers(&ctl_in(&issuer.tmp, &["board", "--json"]));
        (states(&last) == [Some("closed".to_owned()), Some("closed".to_owned())]).then_some(())
    });
    assert!(
        kept.is_some(),
        "the keeper closes each row once its tab is gone: {last:?}"
    );
    let concluded: Vec<Option<&str>> = sids
        .iter()
        .map(|sid| {
            last.iter()
                .find(|row| row.get("sid") == Some(sid))
                .and_then(|row| row.get("conclusion").map(String::as_str))
        })
        .collect();
    assert_eq!(
        concluded,
        [Some("finished"), Some("failed")],
        "a closed row keeps its conclusion: {last:?}"
    );
    let none = drive_in(
        &resumer.0,
        &issuer_at,
        &["await", "task", "board", "since=0"],
    );
    assert!(
        none.status.success()
            && none.stdout.is_empty()
            && String::from_utf8_lossy(&none.stderr).contains("OK board: no open worker"),
        "{none:?}"
    );
}

const NEVER_MS: &str = "600000";

/// A member's heartbeat period where a test waits on what one brings.
const BEAT_MS: &str = "1000";

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

fn driven_pair(tags: (&str, &str)) -> (Instance, Instance, String, String) {
    driven_pair_with(tags, &[])
}

#[test]
fn account_renewal_runs_over_native_fleet_with_no_ssh_or_interactive_terminal() {
    let (issuer, member, _worker, target) = driven_pair(("arni", "arnm"));
    let mut command = aterm_uds::spawnfd::command(crate::launch_isolation::aterm_executable());
    command.args(["harness", "accounts", "renew", "--due", "--on", &target]);
    command
        .env_clear()
        .env("HOME", issuer.tmp.join("home"))
        .env("XDG_CONFIG_HOME", issuer.tmp.join("cfg"))
        .env("XDG_RUNTIME_DIR", issuer.tmp.join("run"))
        .env("XDG_CACHE_HOME", issuer.tmp.join("cache"))
        .env("XDG_DATA_HOME", issuer.tmp.join("data"))
        .env("XDG_STATE_HOME", issuer.tmp.join("state"))
        .env("TMPDIR", issuer.tmp.join("tmp"))
        .env("ATERM_LOG", "off")
        .env("ATERM_STATE_HOME", issuer.tmp.join("state"))
        .env(aterm_net::loopback::SEAM, "1")
        .env(ISSUER_WITHIN_SEAM, HANG.as_millis().to_string())
        .env(HANDSHAKE_SEAM, HANG.as_millis().to_string())
        .env(LOCK_WAIT_SEAM, HANG.as_millis().to_string())
        .env("PATH", issuer.tmp.join("no-programs"));
    let output = aterm_uds::deadline::output_with_input_until(
        &mut command,
        aterm_uds::secret::Secret::new(&[]),
        Instant::now() + HANG,
        64 * 1024,
    )
    .expect("native renewal completed");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert!(text.contains(&format!("renewing on {target}:")), "{text}");
    assert!(
        text.contains("nothing is due on this machine"),
        "{output:?}"
    );

    // Losing the grant is a refusal, never an SSH fallback or an empty success.
    fleet_ok(&member.tmp, &["allow", "facts"], None);
    let _ = drive_refused(
        &issuer.tmp,
        &target,
        &["accounts-renew", "start=1123456789abcdef0123456789abcdef"],
    );
}

/// [`driven_pair`], the member's aterm run with `member_env` besides (a heartbeat's seam, a lag
/// for its typed keys).
fn driven_pair_with(
    tags: (&str, &str),
    member_env: &[(&str, &str)],
) -> (Instance, Instance, String, String) {
    let (holder, port) = hold_a_port();
    drop(holder);
    let port_word = port.to_string();
    let issuer = boot_world(
        tags.0,
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
    let member = boot_world(
        tags.1,
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &[
                    "join",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &member_port_word,
                    "--at",
                    &listen_of(&issuer.tmp),
                ],
                Some(&code),
            );
            fleet_ok(tmp, &["allow", "drive", "--from", &issuer_node], None);
            cmd.env("ATERM_STATE_HOME", tmp.join("state"))
                .env("ATERM_DEBUG_FLEET_RETRY_MS", RETRY_MS)
                .args(["--lines", "24", "--columns", "80"]);
            cmd.envs(member_env.iter().copied());
        },
        |_| true,
    );
    let (_, worker) = boot_session(&member);
    fleet_reads(&member, &worker, "listening");
    (issuer, member, worker, format!("127.0.0.1:{member_port}"))
}

/// `git` in `dir`, hermetic, for a member's checkout.
fn member_git(dir: &Path, args: &[&str]) -> String {
    let mut git = aterm_uds::fixture_git::git();
    git.args([
        "-c",
        "user.name=Andrew Yates",
        "-c",
        "user.email=a@example.invalid",
        "-c",
        "init.defaultBranch=main",
        "-c",
        "commit.gpgsign=false",
    ])
    .args(args)
    .current_dir(dir);
    let out = aterm_uds::deadline::output_until(&mut git, Instant::now() + HANG)
        .unwrap_or_else(|e| panic!("git {args:?} within the hang detector: {e}"));
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A FLEET SURVEY CARRIES EACH MEMBER'S HOME AND CHECKOUTS, AND `--host any` PLACES BY THEM: the
/// member's home holds a clone of github.com/acme/trust, which its survey reports with its HEAD
/// once its read beside the survey ends (the issuer's own home, none); a `need=` it meets passes
/// placement (and stops at its accounts, of which this fixture has none), one it does not is
/// refused with its count, before any spawn. Negative control: a need for a repository its home
/// does not hold.
#[test]
fn a_fleet_survey_carries_each_members_checkouts_and_any_host_places_by_them() {
    let (issuer, member, _worker, _target) = driven_pair(("ahri", "ahrm"));
    let home = member.tmp.join("home");
    let trust = home.join("trust");
    std::fs::create_dir_all(&trust).expect("a checkout");
    member_git(&trust, &["init", "-q", "."]);
    std::fs::write(trust.join("a.txt"), "one\n").expect("a.txt");
    member_git(&trust, &["add", "a.txt"]);
    member_git(&trust, &["commit", "-q", "-m", "one"]);
    member_git(
        &trust,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/trust.git",
        ],
    );
    let head = member_git(&trust, &["rev-parse", "HEAD"]);
    let home_word = home.display().to_string();

    let mut last = String::new();
    let hosts =
        aterm_spec::wait::until_within(HANG, "the member's checkouts in its survey", || {
            let read = survey_answered(&issuer.tmp, &["survey", "--fleet", "--json"]);
            assert!(read.status.success(), "{read:?}");
            last = String::from_utf8_lossy(&read.stdout).into_owned();
            let hosts: aterm_json::Value =
                aterm_json::from_str(&last).unwrap_or_else(|e| panic!("not JSON ({e}): {last}"));
            let read_it = hosts.as_array().into_iter().flatten().any(|host| {
                host.get("repos")
                    .and_then(aterm_json::Value::as_array)
                    .is_some_and(|repos| !repos.is_empty())
            });
            read_it.then_some(hosts)
        });
    let hosts = hosts.as_array().expect("an array");
    let homed: Vec<&aterm_json::Value> = hosts
        .iter()
        .filter(|host| {
            host.get("instance")
                .and_then(|i| i.get("home"))
                .and_then(aterm_json::Value::as_str)
                .is_some()
        })
        .collect();
    assert!(homed.len() >= 2, "both members say their home: {last}");
    let of_member = hosts
        .iter()
        .find(|host| {
            host.get("instance")
                .and_then(|i| i.get("home"))
                .and_then(aterm_json::Value::as_str)
                == Some(home_word.as_str())
        })
        .unwrap_or_else(|| panic!("the member's home {home_word}: {last}"));
    let instance = of_member.get("instance").expect("its instance");
    for key in ["home_free_mb", "repos_at"] {
        assert!(
            instance
                .get(key)
                .and_then(aterm_json::Value::as_str)
                .is_some_and(|v| !v.is_empty() && v != "-"),
            "{key}: {last}"
        );
    }
    let repos = of_member.get("repos").and_then(aterm_json::Value::as_array);
    let trust_row = repos
        .into_iter()
        .flatten()
        .find(|row| row.get("name").and_then(aterm_json::Value::as_str) == Some("trust"))
        .unwrap_or_else(|| panic!("the member's trust checkout: {last}"));
    for (key, want) in [
        ("head", head.as_str()),
        ("branch", "main"),
        ("dirty", "0"),
        ("path", &trust.display().to_string()),
    ] {
        assert_eq!(
            trust_row.get(key).and_then(aterm_json::Value::as_str),
            Some(want),
            "{key}: {last}"
        );
    }
    let text = survey_answered(&issuer.tmp, &["survey", "--fleet"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains(&format!("\n    trust main@{} no origin main", &head[..12])),
        "{text}"
    );

    // `--host any` reads every member as `survey --fleet` does, within the same seam.
    let twice = (2 * HANG).as_millis().to_string();
    let any = |args: &[&str]| {
        let mut all = vec!["--host", "any", "spawn", "agent=claude"];
        all.extend_from_slice(args);
        ctl_with(&issuer.tmp, &all, None, &[(SURVEY_WITHIN_SEAM, &twice)])
    };
    let refused = any(&["need=repo:nope", "-"]);
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        said.contains("need=repo:nope")
            && said.contains("1 lack a checkout it needs (repo:nope)")
            && said.contains("so none meets them"),
        "{said}"
    );
    let met = any(&["need=repo:trust,free:1m", "cwd=$HOME/trust", "-"]);
    let said = String::from_utf8_lossy(&met.stderr);
    assert!(!met.status.success(), "no account to run it on: {met:?}");
    assert!(
        said.contains("none of the 1 that meet them has an enrolled, unwalled claude account"),
        "{said}"
    );
}

/// `--host any` end to end, on macOS alone: its fixture is one fake login of Codex's, whose launch
/// reads its usage over the network, so every aterm of it runs in the Seatbelt jail that kills the
/// process that asks for anything but loopback, which only macOS has.
#[cfg(target_os = "macos")]
mod any_host_walled {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::accounts_upkeep_live_headless::alive;

    /// The stand-in `codex`: `--version` answers at once, and any other call writes its words to
    /// `$HOME/codex-ran` (written aside, then renamed), which only a launch does.
    const STANDIN_CODEX: &str = "#!/bin/sh\n\
        case \"$1\" in --version) echo 'codex-cli 0.163.0'; exit 0 ;; esac\n\
        printf '%s\\n' \"$*\" > \"$HOME/codex-ran.part\" && \
        mv \"$HOME/codex-ran.part\" \"$HOME/codex-ran\"\n";

    /// The file [`STANDIN_CODEX`] leaves in a member's home once a launch reached it.
    const CODEX_RAN: &str = "home/codex-ran";

    /// A window's use, in percent, and its reset, in Unix seconds.
    type Window = (f64, i64);

    /// A loopback stand-in for ChatGPT's usage endpoint that keeps a launch's read off the real
    /// provider: every request line it was asked, and to each the same answer, `body`. Dropping it
    /// joins its thread.
    struct UsageStub {
        port: u16,
        asked: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl UsageStub {
        fn start(body: String) -> Self {
            let (listener, port) = hold_a_port();
            let asked = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let (asked, stop) = (Arc::clone(&asked), Arc::clone(&stop));
                std::thread::spawn(move || {
                    for stream in listener.incoming() {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                        if let Ok(stream) = stream {
                            Self::serve(stream, &asked, &body);
                        }
                    }
                })
            };
            Self {
                port,
                asked,
                stop,
                thread: Some(thread),
            }
        }

        /// One request: its first line kept, `body` answered, the connection closed.
        fn serve(mut stream: TcpStream, asked: &Mutex<Vec<String>>, body: &str) {
            let _ = stream.set_read_timeout(Some(HANG));
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && head.len() < 64 * 1024 {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => return,
                }
            }
            let line = String::from_utf8_lossy(&head)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            asked.lock().unwrap_or_else(|e| e.into_inner()).push(line);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    impl Drop for UsageStub {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Its accept returns for this one connection, and the loop sees `stop`.
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok()
                && let Some(thread) = self.thread.take()
            {
                let _ = thread.join();
            }
        }
    }

    /// Unsigned as any fake login's: nothing here checks a signature.
    fn fake_jwt(claims: &aterm_json::Value) -> String {
        let segment = |bytes: &[u8]| {
            aterm_codec::base64::encode(bytes)
                .expect("base64")
                .trim_end_matches('=')
                .replace('+', "-")
                .replace('/', "_")
        };
        format!(
            "{}.{}.sig",
            segment(br#"{"alg":"RS256","typ":"JWT"}"#),
            segment(&aterm_json::to_vec(claims).expect("its claims"))
        )
    }

    /// `label`, the one enrolled Codex row of the scratch world `root`, on a fake login with no
    /// refresh token, so nothing here can ask the provider for one; its usage read at `base`, the
    /// test's [`UsageStub`]; and a rollout of its own sessions, opened by that login half a minute
    /// ago, that read `five_hour` and `week`: with no record of the row yet, those are the figures
    /// a survey of it has (`codex_limits::login_sample`).
    fn lay_codex_row(root: &Path, label: &str, base: &str, five_hour: Window, week: Window) {
        const ACCOUNT: &str = "acct-any-1";
        const USER: &str = "user-any";
        const THREAD: &str = "01900000-0000-7000-8000-000000000001";
        std::fs::write(
            root.join("cfg/aterm/accounts-codex.toml"),
            format!("[[account]]\nlabel='{label}'\nclass='dedicated'\n"),
        )
        .expect("write the roster");
        let codex = root.join("home/.codex");
        std::fs::create_dir_all(&codex).expect("the Codex home");
        std::fs::write(
            codex.join("config.toml"),
            format!("chatgpt_base_url = \"{base}\"\n"),
        )
        .expect("write Codex's config");
        let token = fake_jwt(&aterm_json::json!({
            "email": "a@example.test",
            "https://api.openai.com/auth": {
                "chatgpt_user_id": USER,
                "chatgpt_account_id": ACCOUNT,
            }
        }));
        std::fs::write(
            codex.join("auth.json"),
            aterm_json::to_vec(&aterm_json::json!({
                "auth_mode": "chatgpt",
                "tokens": {"id_token": token, "access_token": token, "account_id": ACCOUNT}
            }))
            .expect("JSON"),
        )
        .expect("write the login");

        let at_s = i64::try_from(aterm_time::unix_secs().expect("a clock")).expect("a time") - 30;
        let at = aterm_agent::harness::usage::rfc3339_utc(at_s);
        let stamp = format!("{}.000Z", &at[..19]);
        let day = codex.join(format!(
            "sessions/{}/{}/{}",
            &at[..4],
            &at[5..7],
            &at[8..10]
        ));
        std::fs::create_dir_all(&day).expect("the rollout's day");
        let meta = format!(
            "{{\"timestamp\":\"{stamp}\",\"type\":\"session_meta\",\"payload\":{{\"session_id\":\
             \"{THREAD}\",\"id\":\"{THREAD}\",\"history_mode\":\"paginated\",\
             \"creator_account_id\":\"{ACCOUNT}\",\"creator_user_id\":\"{USER}\",\"timestamp\":\
             \"{stamp}\",\"cwd\":\"/stand-in/project\",\"originator\":\"codex-tui\",\
             \"cli_version\":\"0.163.0\",\"source\":\"cli\",\"model_provider\":\"openai\",\
             \"base_instructions\":{{\"text\":\"You are Codex (stand-in)\"}}}}}}\n"
        );
        let slot = |(used, reset): Window, minutes: u64| {
            format!(
                "{{\"used_percent\":{used:.1},\"window_minutes\":{minutes},\"resets_at\":{reset}}}"
            )
        };
        let rate = format!(
            "{{\"timestamp\":\"{stamp}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\
             \"token_count\",\"info\":null,\"rate_limits\":{{\"limit_id\":\"codex\",\"limit_name\"\
             :null,\"primary\":{},\"secondary\":{},\"credits\":{{\"has_credits\":true,\
             \"unlimited\":false,\"balance\":\"12\"}},\"individual_limit\":null,\
             \"spend_control_reached\":null,\"plan_type\":null,\"rate_limit_reached_type\":null}}}}\
             }}\n",
            slot(five_hour, 300),
            slot(week, 10_080),
        );
        std::fs::write(
            day.join(format!(
                "rollout-{}-{THREAD}.jsonl",
                at[..19].replace(':', "-")
            )),
            format!("{meta}{rate}"),
        )
        .expect("write the rollout");
    }

    /// An aterm of the fleet in the network jail
    /// ([`crate::accounts_upkeep_live_headless::NETWORK_JAIL`]), so that what asks for anything but
    /// loopback dies at once and this test sees it gone. Its session is listening.
    #[expect(
        clippy::disallowed_types,
        reason = "headless_boot::boot_jailed's hook, on the binary under test it isolates"
    )]
    fn jailed_world(
        prefix: &str,
        configure: impl FnOnce(&Path, &mut std::process::Command),
    ) -> Instance {
        let inst = headless_boot::boot_jailed(
            prefix,
            crate::accounts_upkeep_live_headless::NETWORK_JAIL,
            |tmp, cmd| {
                world_seams(cmd);
                cmd.env("ATERM_STATE_HOME", tmp.join("state"))
                    .env("ATERM_LOG", "info")
                    .env("ATERM_DEBUG_FLEET_RETRY_MS", RETRY_MS)
                    .args(["--lines", "24", "--columns", "80"]);
                configure(tmp, cmd);
            },
            |_| true,
        );
        let (_, sid) = boot_session(&inst);
        fleet_reads(&inst, &sid, "listening");
        inst
    }

    /// A member of `issuer`'s fleet that lets it drive it, supervises its workers, runs
    /// [`STANDIN_CODEX`] for `codex`, and holds `label` ([`lay_codex_row`]), written before it
    /// boots, since a listener keeps its facts a while.
    fn codex_member(
        prefix: &str,
        issuer: &Instance,
        issuer_node: &str,
        code: &str,
        row: (&str, &str, Window, Window),
    ) -> Instance {
        let (holder, port) = hold_a_port();
        drop(holder);
        let port = port.to_string();
        let (label, base, five_hour, week) = row;
        jailed_world(prefix, |tmp, cmd| {
            fleet_ok(
                tmp,
                &[
                    "join",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port,
                    "--at",
                    &listen_of(&issuer.tmp),
                ],
                Some(code),
            );
            fleet_ok(tmp, &["allow", "drive", "--from", issuer_node], None);
            let cfg = tmp.join("cfg/aterm/aterm.toml");
            let text = std::fs::read_to_string(&cfg).expect("the isolation config");
            assert!(
                text.contains(crate::launch_isolation::HARNESS_OFF),
                "{text}"
            );
            std::fs::write(
                &cfg,
                format!(
                    "{}[harness]\nrelaunch = false\nupgrade = false\n",
                    text.replace(crate::launch_isolation::HARNESS_OFF, "")
                ),
            )
            .expect("write the config");
            lay_codex_row(tmp, label, base, five_hour, week);
            let bin = tmp.join("bin");
            headless_boot::standin(&bin.join("codex"), STANDIN_CODEX);
            cmd.env("SHELL", headless_boot::login_shell().path).env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
            );
        })
    }

    /// The Codex row `label` of a `survey --fleet --json` read, by its field names.
    fn codex_row<'a>(hosts: &'a aterm_json::Value, label: &str) -> Option<&'a aterm_json::Value> {
        let says = |row: &aterm_json::Value, key: &str, want: &str| {
            row.get(key).and_then(aterm_json::Value::as_str) == Some(want)
        };
        hosts
            .as_array()?
            .iter()
            .filter_map(|host| host.get("accounts")?.as_array())
            .flatten()
            .find(|row| says(row, "pool", "codex") && says(row, "id", label))
    }

    /// A MEMBER WHOSE CODEX ROW STANDS AT A WALL IS NEVER PICKED, by real aterms over real TCP on
    /// loopback, though its week is the lower figure, so ranking alone would choose it: only
    /// `Row::takes` keeps it out. The issuer asks: it is never a candidate itself, since a fleet
    /// read leaves out this machine and a drive refuses it. The worker lands on the free member,
    /// where it runs the stand-in `codex`, and on no other: the walled member has no such session,
    /// ran no `codex`, and its usage was never asked for. NEGATIVE CONTROL: with the free member
    /// stopped, the same spawn is refused, naming what the walled member lacks, and starts nothing.
    ///
    /// This holds where a launch's read of the row may go, not that one is made. Whatever reaches
    /// the stub must have asked for the free row, and a read that leaves loopback kills the aterm
    /// that asked, which [`alive`] sees of each. It does not require a read to arrive, since that
    /// read's own bound ([`aterm_agent::harness::accounts_pool::USAGE_READ_WAIT`]) has no seam and
    /// the launch goes on without it, so a loaded machine could miss it:
    /// `a_lone_row_is_read_as_the_row_a_launch_starts_on` holds that a launch reads.
    #[test]
    fn a_walled_codex_member_is_never_picked_by_any_host_end_to_end() {
        const WALLED: &str = "walled";
        const FREE: &str = "free";
        const TASK: &str = "a task from the issuer, for whichever Mac can take it";

        let now = i64::try_from(aterm_time::unix_secs().expect("a clock")).expect("a time");
        let (five_hour_reset, week_reset) = (now + 4 * 3600, now + 5 * 86_400);
        let stub = UsageStub::start(
            aterm_json::json!({
                "plan_type": "pro",
                "rate_limit": {
                    "allowed": true,
                    "limit_reached": false,
                    "primary_window": {
                        "used_percent": 10.0,
                        "limit_window_seconds": 18_000,
                        "reset_at": five_hour_reset,
                    },
                    "secondary_window": {
                        "used_percent": 60.0,
                        "limit_window_seconds": 604_800,
                        "reset_at": week_reset,
                    },
                }
            })
            .to_string(),
        );
        let base = |member: &str| format!("http://127.0.0.1:{}/{member}/backend-api", stub.port);

        let (holder, port) = hold_a_port();
        drop(holder);
        let port = port.to_string();
        let mut issuer = jailed_world("awci", |tmp, _| {
            fleet_ok(tmp, &["init", "--host", "127.0.0.1", "--port", &port], None);
        });
        let code =
            std::fs::read_to_string(issuer.tmp.join("state/fleet-join-code")).expect("a code");
        let issuer_node =
            std::fs::read_to_string(issuer.tmp.join("state/fleet-node")).expect("its node id");
        let issuer_node = issuer_node.trim();
        let mut walled = codex_member(
            "awcw",
            &issuer,
            issuer_node,
            &code,
            (
                WALLED,
                &base(WALLED),
                (100.0, five_hour_reset),
                (5.0, week_reset),
            ),
        );
        let mut free = codex_member(
            "awcf",
            &issuer,
            issuer_node,
            &code,
            (
                FREE,
                &base(FREE),
                (10.0, five_hour_reset),
                (60.0, week_reset),
            ),
        );

        let hosts = aterm_spec::wait::until_within(HANG, "both members' rows in a survey", || {
            let read = survey_answered(&issuer.tmp, &["survey", "--fleet", "--json"]);
            assert!(read.status.success(), "{read:?}");
            let text = String::from_utf8_lossy(&read.stdout);
            let hosts: aterm_json::Value =
                aterm_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"));
            (codex_row(&hosts, WALLED).is_some() && codex_row(&hosts, FREE).is_some())
                .then_some(hosts)
        });
        let word = |label: &str, key: &str| {
            codex_row(&hosts, label)
                .and_then(|row| row.get(key))
                .and_then(aterm_json::Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| panic!("{label}'s {key}: {hosts:?}"))
        };
        for (label, condition, five_hour, week) in [
            (WALLED, "limited", "100", "5"),
            (FREE, "usable", "10", "60"),
        ] {
            assert_eq!(word(label, "condition"), condition, "{hosts:?}");
            assert_eq!(word(label, "enrolled"), "1", "{hosts:?}");
            assert_eq!(word(label, "pct5"), five_hour, "{hosts:?}");
            assert_eq!(word(label, "pct7"), week, "{hosts:?}");
            assert_ne!(word(label, "read_at"), "-", "{hosts:?}");
        }

        let twice = (2 * HANG).as_millis().to_string();
        let any = || {
            ctl_with(
                &issuer.tmp,
                &["--host", "any", "spawn", "agent=codex", "-"],
                Some(TASK.as_bytes()),
                &[(SURVEY_WITHIN_SEAM, &twice)],
            )
        };
        let placed = any();
        let said = String::from_utf8_lossy(&placed.stderr);
        assert!(placed.status.success(), "{placed:?}");
        let chose = said
            .lines()
            .find(|line| line.starts_with("--host any: chose "))
            .unwrap_or_else(|| panic!("no choice said: {said}"));
        println!("{chose}");
        assert!(
            chose.contains(&format!("codex account {FREE} 7d 60% 5h 10%,"))
                && chose.ends_with("the best of 1 that can take it"),
            "{chose}"
        );
        let stdout = String::from_utf8_lossy(&placed.stdout);
        let sid = stdout
            .split_whitespace()
            .find(|word| word.starts_with("s-"))
            .unwrap_or_else(|| panic!("a sid: {stdout}"));
        let at = format!("@{sid}");
        let record = ctl_ok(&free, &[&at, "task"]);
        assert!(record.starts_with("OK task rev="), "{record}");
        let elsewhere = ctl(&walled, &[&at, "status"]);
        assert!(
            !elsewhere.status.success(),
            "the walled member has the worker: {elsewhere:?}"
        );

        let ran = aterm_spec::wait::within(HANG, POLL_GAP, || {
            std::fs::read_to_string(free.tmp.join(CODEX_RAN)).ok()
        })
        .unwrap_or_else(|| {
            let ask = |args: &[&str]| headless_boot::said(&ctl(&free, args));
            panic!(
                "the worker's codex never ran.\ntask: {}\nscreen: {}\nasked: {:?}\nlog:\n{}",
                ask(&[&at, "task"]),
                ask(&[&at, "text", "trim"]),
                stub.asked(),
                headless_boot::log_tail(&free.log)
            )
        });
        assert!(ran.contains(TASK), "{ran}");
        let nothing_on_the_wall = || {
            assert!(
                !walled.tmp.join(CODEX_RAN).exists(),
                "a codex ran on the walled member"
            );
            // The jail keeps a read off the provider; this says who asked, of whatever came.
            let asked = stub.asked();
            assert!(
                asked
                    .iter()
                    .all(|line| line.starts_with(&format!("GET /{FREE}/backend-api/wham/usage "))),
                "something but the free member's launch asked for usage: {asked:?}"
            );
        };
        nothing_on_the_wall();

        alive(&mut free);
        drop(free);
        let refused = any();
        let said = String::from_utf8_lossy(&refused.stderr);
        assert!(!refused.status.success(), "{refused:?}");
        assert!(
            said.contains(
                "--host any: no Mac can take agent=codex: 1 members answered, 1 of them let this \
                 Mac drive them"
            ) && said.contains("none of those has an enrolled, unwalled codex account"),
            "{said}"
        );
        assert!(
            !said.contains("--host any: chose"),
            "the refusal chose: {said}"
        );
        nothing_on_the_wall();
        let sessions = ctl_ok(&walled, &["sessions"]);
        let open = sessions
            .lines()
            .filter(|line| line.starts_with(|c: char| c.is_ascii_digit()))
            .count();
        assert_eq!(open, 1, "only its boot session: {sessions}");
        // The jail killed neither aterm that is left: what reached past loopback would be gone.
        alive(&mut issuer);
        alive(&mut walled);
    }
}

#[test]
fn a_grant_taken_back_while_aterm_toml_does_not_load_still_ends_the_drive() {
    let (issuer, member, worker, target) = driven_pair(("afri", "afrm"));
    let (took, _) = drive_ok(&issuer.tmp, &target, &["hold", &worker, "on"]);
    assert_eq!(took.trim(), "OK hold=1", "{took}");
    unloadable(&member.tmp.join("cfg/aterm/aterm.toml"), true);
    fleet_ok(&member.tmp, &["allow", "content"], None);
    hold_ends(&member, &worker);
    drive_refused(&issuer.tmp, &target, &[&format!("@{worker}"), "status"]);
}

fn open_drive(root: &Path, target: &str) -> aterm_net::fleet_member::Driven {
    let state = root.join("state");
    let secret = aterm_net::fleet::read_key_file(&state).expect("the fleet's key");
    let me = read_node_identity(&state)
        .expect("its node key")
        .expect("a node key");
    let book = read_peers(&state).expect("its book");
    let peer = book
        .iter()
        .find(|peer| format!("127.0.0.1:{}", peer.port) == target)
        .unwrap_or_else(|| panic!("no entry at {target}: {book:?}"));
    aterm_net::fleet_member::drive(
        &aterm_net::fleet_member::target_of(peer),
        &Keyring::of(&book),
        (&secret, &me),
        (Instant::now() + HANG, HANG),
    )
    .unwrap_or_else(|why| panic!("{why}"))
}

/// Another process's handle on a session's direct mailbox, as a seamless update's predecessor
/// holds one: its writes ring only its own waiters.
struct HandoffPeer {
    sid: aterm_session::SessionId,
    fabric: aterm_gui_fabric::SessionFabric,
    path: PathBuf,
}

impl HandoffPeer {
    fn of(session: &str, path: &Path) -> Self {
        Self {
            sid: aterm_session::SessionId::new(session),
            fabric: aterm_gui_fabric::SessionFabric::default(),
            path: path.to_owned(),
        }
    }
}

impl aterm_gui_fabric::FabricSession for HandoffPeer {
    fn self_id(&self) -> &aterm_session::SessionId {
        &self.sid
    }
    fn fabric(&self) -> &aterm_gui_fabric::SessionFabric {
        &self.fabric
    }
    fn record_fact(&self, _: &'static str, _: aterm_gui_fabric::FabricFact<'_>) -> bool {
        false
    }
    fn fabric_changed(&self) {}
    fn holder_token(&self) -> Option<String> {
        None
    }
    fn fleet_mail_path(&self) -> Option<PathBuf> {
        Some(self.path.clone())
    }
    fn fleet_mail_ready(&self) -> bool {
        true
    }
}

#[test]
fn startup_migrates_only_the_current_owners_legacy_mailbox() {
    const OTHER: &str = "s-0e0d0e0d";
    const TEXT: &str = "PRIVATE_EARLIER_SESSION_TEXT";
    let legacy = format!(
        "next = 1\n\n[[rows]]\nid = 1\nnode = \"{}\"\nkey = \"k\"\nkind = \"ask\"\n\
         text = \"{TEXT}\"\nlisted = false\nreply_seen = false\n",
        "a".repeat(32)
    );
    let foreign_temp = format!(".{OTHER}.toml.tmp.{}.0badcafe", u32::MAX);
    let instance = headless_boot::boot_with(
        "afms",
        |tmp, cmd| {
            // An idle older reader may still own this file. No transaction lock means neither
            // that its owner ended nor that it can read the sealed format.
            let folder = tmp.join("state/fleet-mail");
            for dir in [tmp.join("state"), folder.clone()] {
                aterm_uds::private_file::create_private_dir(&dir).expect("a private folder");
            }
            aterm_uds::private_file::write_private(
                &folder,
                &format!("{OTHER}.toml"),
                legacy.as_bytes(),
            )
            .expect("the earlier build's mailbox");
            aterm_uds::private_file::write_private(&folder, &foreign_temp, legacy.as_bytes())
                .expect("a foreign writer's retained temporary");
            cmd.env("ATERM_STATE_HOME", tmp.join("state"));
        },
        |_| true,
    );
    let folder = instance.tmp.join("state/fleet-mail");
    let (_, own_sid) = boot_session(&instance);
    let own_name = format!("{own_sid}.toml");
    let own_path = folder.join(&own_name);
    aterm_uds::private_file::write_private(&folder, &own_name, legacy.as_bytes())
        .expect("the active owner's old mailbox");
    // This is also a progress witness: the one watcher has finished startup and refreshed
    // its own session. An immediate unchanged-file assertion could race that first pass.
    aterm_spec::wait::within(HANG, POLL_GAP, || {
        std::fs::read(&own_path)
            .ok()
            .filter(|bytes| bytes.starts_with(b"\0aterm fleet mail, sealed\n"))
    })
    .expect("the active owner's watcher seals its own mailbox");
    let page = ctl_ok(&instance, &[&format!("@{own_sid}"), "inbox", "fleet"]);
    assert!(page.contains(&format!("text={TEXT}")), "{page}");

    let other_name = format!("{OTHER}.toml");
    let expected: aterm_toml::Value = aterm_toml::from_str(&legacy).unwrap();
    for _ in 0..2 {
        // The old reader's exact codec boundary: it still opens UTF-8 TOML, not AEAD.
        let bytes = aterm_uds::private_file::read_private(&folder, &other_name, 65536)
            .expect("the older reader's file remains accessible");
        assert_eq!(bytes, legacy.as_bytes());
        let text = std::str::from_utf8(&bytes).expect("the older reader's UTF-8");
        let read: aterm_toml::Value = aterm_toml::from_str(text).expect("the older reader's TOML");
        assert_eq!(read, expected);
    }
    assert_eq!(
        aterm_uds::private_file::read_private(&folder, &foreign_temp, 65536).unwrap(),
        legacy.as_bytes(),
        "startup has no ownership of a foreign temporary"
    );
}

#[test]
fn direct_fleet_mail_delivers_and_replies_without_terminal_input_and_requires_a_drive_grant() {
    let (sender, receiver, session, target) = driven_pair(("afmi", "afmm"));
    let at = format!("@{session}");
    let mut subscription = headless_boot::subscribed(&receiver, &[&at, "mail"]);
    let stream = subscription.stdout.take().expect("subscription stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stream)
            .lines()
            .map_while(Result::ok)
            .filter(|line| line.starts_with("MAIL "))
        {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let (posted, _) = drive_ok(
        &sender.tmp,
        &target,
        &[
            &at,
            "post",
            "key=one",
            "kind=ask",
            "--",
            "fleet-mail-only-question",
        ],
    );
    assert_eq!(posted.trim(), "OK f1");
    let notification = rx
        .recv_timeout(HANG)
        .expect("idle recipient receives a MAIL frame");
    assert!(
        notification.contains(" id=f1 off=- from=fleet:") && notification.ends_with(" kind=ask"),
        "{notification}"
    );
    assert!(!notification.contains("fleet-mail-only-question"));
    // A predecessor can finish a durable write after the successor cached its mailbox.
    // Write from this separate process, through the mailbox's own transaction (sealed under the
    // key beside it), as the sending node, without issuing a receiver-side inbox command.
    let durable = receiver
        .tmp
        .join("state/fleet-mail")
        .join(format!("{session}.toml"));
    let node = notification
        .split_once(" from=fleet:")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .expect("the sending node")
        .to_owned();
    let handoff = HandoffPeer::of(&session, &durable);
    let as_sender = || {
        aterm_gui_fabric::DrivingAs::enter(aterm_gui_fabric::Driver {
            node: node.clone(),
            host: "handoff.local".to_owned(),
        })
    };
    {
        let _sender = as_sender();
        assert_eq!(
            aterm_gui_fabric::fleet_mail::post(
                &handoff,
                "key=late-handoff kind=ask -- PRIVATE_HANDOFF_BODY"
            ),
            "OK f2\n"
        );
    }
    let late = rx
        .recv_timeout(HANG)
        .expect("subscriber discovers another process's late durable write");
    assert!(late.contains(" id=f2 off=- from=fleet:"), "{late}");
    assert!(!late.contains("PRIVATE_HANDOFF_BODY"));
    let log = ctl_ok(&receiver, &[&at, "log"]);
    assert!(log.contains("re=f1") && log.contains("kind=ask"), "{log}");
    assert!(!log.contains("fleet-mail-only-question"));
    let local = ctl_ok(&receiver, &[&at, "inbox"]);
    assert!(
        local.contains("msg f1 from=fleet:") && local.contains("fleet-mail-only-question"),
        "{local}"
    );
    let terminal = ctl_ok(&receiver, &[&at, "text"]);
    assert!(
        !terminal.contains("fleet-mail-only-question"),
        "mail reached the PTY: {terminal}"
    );
    let answered = ctl_ok(
        &receiver,
        &[
            &at,
            "post",
            "re=f1",
            "key=answer-one",
            "kind=answer",
            "--",
            "fleet-mail-only-answer",
        ],
    );
    assert_eq!(answered.trim(), "OK f1 reply=1");
    let (reply, _) = drive_ok(&sender.tmp, &target, &[&at, "inbox"]);
    assert!(
        reply.contains("reply_text=fleet-mail-only-answer"),
        "{reply}"
    );
    let (acknowledged, _) = drive_ok(&sender.tmp, &target, &[&at, "inbox", "seen", "f1"]);
    assert_eq!(acknowledged.trim(), "OK seen=f1");
    let (retry, _) = drive_ok(
        &sender.tmp,
        &target,
        &[
            &at,
            "post",
            "key=one",
            "kind=ask",
            "--",
            "fleet-mail-only-question",
        ],
    );
    assert_eq!(retry.trim(), "OK f1 dup=1");
    let log = ctl_ok(&receiver, &[&at, "log"]);
    assert_eq!(
        log.lines()
            .filter(|line| line.contains(" mark - mail ") && line.contains("re=f1"))
            .count(),
        1,
        "{log}"
    );
    // The mailbox on disk holds the answer, opened only under its key: no row's text is in it.
    let reread = {
        let _sender = as_sender();
        aterm_gui_fabric::fleet_mail::inbox(&HandoffPeer::of(&session, &durable), "", false)
    };
    assert!(
        reread.contains("reply_text=fleet-mail-only-answer"),
        "{reread}"
    );
    let on_disk = std::fs::read(&durable).unwrap();
    for text in [
        "fleet-mail-only-question",
        "fleet-mail-only-answer",
        "PRIVATE_HANDOFF_BODY",
    ] {
        assert!(
            !on_disk.windows(text.len()).any(|w| w == text.as_bytes()),
            "{text} is on disk in plaintext"
        );
    }
    assert!(
        durable
            .with_file_name(aterm_gui_fabric::fleet_mail::KEY)
            .exists()
    );
    fleet_ok(&receiver.tmp, &["allow", "content"], None);
    drive_refused(
        &sender.tmp,
        &target,
        &[
            &at,
            "post",
            "key=denied",
            "kind=note",
            "--",
            "must-not-land",
        ],
    );
    assert!(!ctl_ok(&receiver, &[&at, "inbox", "fleet"]).contains("must-not-land"));
    let _ = subscription.kill();
    let _ = subscription.wait();
    reader.join().expect("subscription reader joined");
}

#[test]
fn a_grant_written_while_a_drive_is_open_cuts_it_only_once_it_takes_its_grant_away() {
    let (issuer, member, worker, target) = driven_pair(("afgi", "afgm"));
    let code = std::fs::read_to_string(issuer.tmp.join("state/fleet-join-code")).expect("a code");
    let third = World(headless_boot::scratch_root("afgt"));
    let (holder, third_port) = hold_a_port();
    drop(holder);
    fleet_ok(
        &third.0,
        &[
            "join",
            "--host",
            "127.0.0.1",
            "--port",
            &third_port.to_string(),
            "--at",
            &listen_of(&issuer.tmp),
        ],
        Some(&code),
    );
    let third_node =
        std::fs::read_to_string(third.0.join("state/fleet-node")).expect("its node id");
    let third_node = third_node.trim().to_owned();
    let at = format!("@{worker}");
    drive_refused(&third.0, &target, &[&at, "status"]);
    let mut open = open_drive(&issuer.tmp, &target);
    let read = ask(&mut open, &format!("{at} status"));
    assert!(read.starts_with("OK schema="), "{read}");

    fleet_ok(
        &member.tmp,
        &["allow", "drive", "--from", &third_node],
        None,
    );
    let (read, _) = drive_ok(&third.0, &target, &[&at, "status"]);
    assert!(
        read.starts_with("OK schema="),
        "a grant drives once its write returns: {read}"
    );
    let serves = format!("fleet listening on {target} (serving content; 2 node(s) may drive it)");
    let mut log = String::new();
    let stood = aterm_spec::wait::within(HANG, POLL_GAP, || {
        log = std::fs::read_to_string(&member.log).unwrap_or_default();
        log.contains(&serves).then_some(())
    });
    assert!(
        stood.is_some(),
        "the listener never served the grant: {log}"
    );
    let read = ask(&mut open, &format!("{at} status"));
    assert!(
        read.starts_with("OK schema="),
        "a grant written cut no drive it still lets in: {read}"
    );

    let before = log.len();
    fleet_ok(&member.tmp, &["allow", "content"], None);
    let cut = aterm_spec::wait::within(HANG, POLL_GAP, || {
        log = std::fs::read_to_string(&member.log).unwrap_or_default();
        log.get(before..)?
            .contains("connection(s) it no longer serves are cut")
            .then_some(())
    });
    assert!(cut.is_some(), "the listener never cut the drive: {log}");
    assert!(
        closed(&mut open),
        "a grant taken back cuts the drive it let in"
    );
}

#[test]
fn an_all_member_grant_lets_every_member_drive_and_keeps_the_holds_they_take() {
    let (issuer, member, worker, target) = driven_pair_with(
        ("afai", "afam"),
        &[("ATERM_DEBUG_FLEET_HEARTBEAT_MS", BEAT_MS)],
    );
    let code = std::fs::read_to_string(issuer.tmp.join("state/fleet-join-code")).expect("a code");
    let third = World(headless_boot::scratch_root("afat"));
    let (holder, third_port) = hold_a_port();
    drop(holder);
    fleet_ok(
        &third.0,
        &[
            "join",
            "--host",
            "127.0.0.1",
            "--port",
            &third_port.to_string(),
            "--at",
            &listen_of(&issuer.tmp),
        ],
        Some(&code),
    );
    let third_node =
        std::fs::read_to_string(third.0.join("state/fleet-node")).expect("its node id");
    let third_node = third_node.trim().to_owned();
    // No drive reads the issuer's book again (F17): the member binds the third Mac once its
    // heartbeat makes the issuer's answer its copy of that book.
    let bound = aterm_spec::wait::within(HANG, POLL_GAP, || {
        Keyring::read(&member.tmp.join("state"))
            .bound(&third_node)
            .map(|_| ())
    });
    assert!(
        bound.is_some(),
        "the member's heartbeat never bound the third Mac"
    );
    fleet_ok(&member.tmp, &["allow", "content"], None);
    let all = fleet_ok(&member.tmp, &["allow", "drive", "--all"], None);
    assert!(
        all.contains("drive_from = [\"*\"]")
            && all.contains(
                "every member this Mac's membership log admits or its eligible peer books bind"
            )
            && !all.contains("each node named"),
        "{all}"
    );
    let at = format!("@{worker}");
    for driver in [&issuer.tmp, &third.0] {
        let (read, _) = drive_ok(driver, &target, &[&at, "status"]);
        assert!(read.starts_with("OK schema="), "{read}");
    }
    let (took, _) = drive_ok(&issuer.tmp, &target, &["hold", &worker, "on"]);
    assert_eq!(took.trim(), "OK hold=1", "{took}");
    let off = ctl(&member, &["hold", &worker, "off"]);
    let said = String::from_utf8_lossy(&off.stderr);
    assert!(
        off.status.code() == Some(1) && said.contains("which still drives this session's Mac"),
        "the issuer, answering under the key the member binds, keeps its hold: {off:?}"
    );
    let (off, _) = drive_ok(&issuer.tmp, &target, &["hold", &worker, "off"]);
    assert_eq!(off.trim(), "OK hold=0", "{off}");

    let (took, _) = drive_ok(&third.0, &target, &["hold", &worker, "on"]);
    assert_eq!(took.trim(), "OK hold=1", "{took}");
    let local = ctl(&member, &[&at, "send", "x"]);
    let said = String::from_utf8_lossy(&local.stderr);
    assert!(
        local.status.code() == Some(1)
            && said.contains("origin=remote")
            && said.contains(&format!(
                "held by 127.0.0.1 (node {}) over the fleet",
                &third_node[..8]
            ))
            && said.contains(&format!("`aterm ctl hold {worker} off`")),
        "the third member's remote hold refuses local send and names the holder: {said}"
    );
    fleet_ok(&issuer.tmp, &["peers", "rm", &third_node], None);
    peers_answered(&member.tmp);
    // F17 also records signed entries learned from a member's book in this Mac's own book.
    // Removing it at the issuer clears the copy, but this Mac must remove its local binding.
    fleet_ok(&member.tmp, &["peers", "rm", &third_node], None);
    let member_state = member.tmp.join("state");
    let issuer_node =
        std::fs::read_to_string(issuer.tmp.join("state/fleet-node")).expect("the issuer node id");
    let books = || {
        let own = read_peers(&member_state)
            .expect("the member's book")
            .iter()
            .any(|peer| peer.node.as_deref() == Some(third_node.as_str()));
        let copy = aterm_net::fleet_member::read_issuer_book(&member_state, issuer_node.trim())
            .iter()
            .any(|peer| peer.node.as_deref() == Some(third_node.as_str()));
        (own, copy)
    };
    let issuer_state = issuer.tmp.join("state");
    let issuer_binds_third = || {
        read_peers(&issuer_state)
            .expect("the issuer's book")
            .iter()
            .any(|peer| peer.node.as_deref() == Some(third_node.as_str()))
    };
    // The member's heartbeat brings the issuer's current book every BEAT_MS: a round that read
    // it before the removal, or a `peers` read a loaded machine kept from answering, leaves the
    // older copy until that next round. The issuer beats too, at times this test does not set,
    // and no Mac's `peers rm` takes a Mac out of another's book (`merge_signed`): an issuer round
    // that reads the member's book between the two removals takes the third Mac back in, the
    // member's next round copies it, and the removal is made again at both. While the issuer
    // never takes it back, the member's one removal must hold against its own rounds.
    let mut taken_back = 0_u32;
    let cleared = aterm_spec::wait::within(HANG, POLL_GAP, || {
        if issuer_binds_third() {
            taken_back += 1;
            fleet_ok(&issuer.tmp, &["peers", "rm", &third_node], None);
        }
        if taken_back > 0 && books().0 {
            fleet_ok(&member.tmp, &["peers", "rm", &third_node], None);
        }
        (books() == (false, false) && !issuer_binds_third()).then_some(())
    });
    let (own_has_third, copy_has_third) = books();
    assert!(
        cleared.is_some(),
        "removal from both books must leave no member binding: own={own_has_third} \
         copy={copy_has_third} (the issuer took it back {taken_back} times)"
    );
    hold_ends(&member, &worker);
}

#[test]
fn a_remote_turn_into_a_shell_with_no_integration_is_submitted_by_one_enter() {
    let (issuer, member, worker, target) = driven_pair(("afti", "aftm"));
    let at = format!("@{worker}");
    assert_eq!(
        field(&status(&member, &worker), "integration"),
        Some("off"),
        "a shell that marks no command block"
    );
    // A press window of most of the drive's 20 s turn, and a command silent past it: only the
    // cursor's move shows the press, however late the echo is read.
    let (_, verdict) = drive_ok(
        &issuer.tmp,
        &target,
        &[
            &at,
            "turn",
            "submit_window=15000",
            "sleep 18; echo slow-$((6*7))",
        ],
    );
    assert!(
        verdict.contains("submitted=1") && !verdict.contains("pressed="),
        "{verdict}"
    );
    ctl_ok(
        &member,
        &[&at, "await", "match", "^slow-42", "timeout=60000"],
    );
    let after = ctl_ok(
        &member,
        &[&at, "turn", "timeout=60000", "echo after-$((6*7))"],
    );
    let rows: Vec<&str> = after.lines().collect();
    let slow = rows
        .iter()
        .position(|row| row.starts_with("slow-42"))
        .unwrap_or_else(|| panic!("the command ran: {after}"));
    assert!(
        rows.get(slow + 1)
            .is_some_and(|row| row.ends_with("echo after-$((6*7))")),
        "one Enter went in, so no empty prompt follows the command's own: {after}"
    );
}

/// A drive's typed turn whose keys go slower than its 20 s allows (the driven node holds each key
/// 400 ms before it goes, `ATERM_DEBUG_TURN_KEY_LAG_MS`: m108 behind m14's drive, 2026-11-04)
/// lands whole: the keys that fit, then the rest as one paste and an Enter; and one whose first
/// key does not fit writes nothing. Either way the shell's line is as the turn found it, so the
/// next command runs alone. What it judges is that property, never how soon the shell answers:
/// an Enter no look verified still says `pressed=`, and the line's run is the hang detector's.
/// NEGATIVE CONTROL: cutting the typing at the deadline, as the turn did before its pace, answers
/// `skipped reason=deadline typed=<n>` and leaves those keys in the shell's line.
#[test]
fn a_drives_typed_turn_slower_than_its_deadline_lands_whole_or_writes_nothing() {
    let (issuer, member, worker, target) =
        driven_pair_with(("afki", "afkm"), &[("ATERM_DEBUG_TURN_KEY_LAG_MS", "400")]);
    let at = format!("@{worker}");
    ctl_ok(
        &member,
        &[&at, "turn", "timeout=60000", "echo ready-$((6*7))"],
    );
    ctl_ok(
        &member,
        &[&at, "await", "match", "^ready-42", "timeout=60000"],
    );
    let alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let line = format!("echo whole-$((6*7))-{alphabet}");
    let keys = line.chars().count();
    // 72 keys 70 ms apart fit the drive's 20 s; 400 ms apart (the lag, which the cadence runs
    // within) they do not. The submit's share is the most a 20 s turn keeps for it, half.
    let sent = drive_in(
        &issuer.tmp,
        &target,
        &[
            &at,
            "turn",
            "typed=1",
            "timeout=20000",
            "settle=submitted",
            "submit_window=10000",
            &line,
        ],
    );
    let verdict = String::from_utf8_lossy(&sent.stderr).into_owned();
    // A turn whose Enter no look verified in its window ends `status=timeout`, which exits 124.
    assert!(
        sent.status.success() || sent.status.code() == Some(124),
        "{sent:?}"
    );
    let count = |name: &str| {
        field(verdict.trim_end(), name)
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or_else(|| panic!("{name}= in {verdict}"))
    };
    let (typed, pasted) = (count("typed"), count("pasted"));
    assert!(
        !verdict.contains(" turn skipped ")
            && (verdict.contains("submitted=1") || verdict.contains(" pressed="))
            && typed > 0
            && pasted > 0
            && typed + pasted == keys,
        "the keys that fit, then the rest as one paste and an Enter: {verdict}"
    );
    ctl_ok(
        &member,
        &[&at, "await", "match", "^whole-42-", "timeout=60000"],
    );
    // A first key that does not fit: nothing is written.
    let (_, skipped) = drive_ok(
        &issuer.tmp,
        &target,
        &[
            &at,
            "turn",
            "typed=1",
            "cadence=2000",
            "timeout=1000",
            "submit_window=200",
            "echo none-$((6*7))",
        ],
    );
    assert!(
        skipped.contains(" turn skipped reason=deadline typed=0 pasted=0 submitted=0 "),
        "{skipped}"
    );
    ctl_ok(
        &member,
        &[&at, "turn", "timeout=60000", "echo after-$((6*7))"],
    );
    ctl_ok(
        &member,
        &[&at, "await", "match", "^after-42", "timeout=60000"],
    );
    let screen = ctl_ok(&member, &[&at, "text"]);
    let rows: Vec<&str> = screen.lines().map(str::trim_end).collect();
    assert_eq!(
        rows.iter()
            .filter(|row| **row == format!("whole-42-{alphabet}"))
            .count(),
        1,
        "the whole line ran once: {screen}"
    );
    assert!(
        rows.contains(&"after-42") && !rows.iter().any(|row| row.contains("none-42")),
        "the next command ran alone: {screen}"
    );
}

#[test]
fn a_local_hold_off_keeps_a_hold_its_node_may_still_drive_and_lifts_one_it_may_not() {
    let (holder, port) = hold_a_port();
    drop(holder);
    let port_word = port.to_string();
    let held = boot_world(
        "afli",
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &["init", "--host", "127.0.0.1", "--port", &port_word],
                None,
            );
            cmd.env("ATERM_STATE_HOME", tmp.join("state"))
                .env("ATERM_DEBUG_FLEET_RETRY_MS", RETRY_MS)
                .args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    );
    let (_, sid) = boot_session(&held);
    fleet_reads(&held, &sid, "listening");
    let code = std::fs::read_to_string(held.tmp.join("state/fleet-join-code")).expect("a code");
    let (holder, member_port) = hold_a_port();
    drop(holder);
    let member_port_word = member_port.to_string();
    let driver = boot_world(
        "aflm",
        |tmp, cmd| {
            fleet_ok(
                tmp,
                &[
                    "join",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &member_port_word,
                    "--at",
                    &listen_of(&held.tmp),
                ],
                Some(&code),
            );
            cmd.env("ATERM_STATE_HOME", tmp.join("state"))
                .env("ATERM_DEBUG_FLEET_RETRY_MS", RETRY_MS)
                .args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    );
    let (_, driver_sid) = boot_session(&driver);
    fleet_reads(&driver, &driver_sid, "listening");
    let node = std::fs::read_to_string(driver.tmp.join("state/fleet-node")).expect("its node id");
    let node = node.trim().to_owned();
    fleet_ok(&held.tmp, &["allow", "drive", "--from", &node], None);
    let target = format!("127.0.0.1:{port}");
    let mut last = None;
    let took = aterm_spec::wait::within(HANG, POLL_GAP, || {
        let out = drive_in(&driver.tmp, &target, &["hold", &sid, "on"]);
        let took = out.status.success().then_some(());
        last = Some(out);
        took
    });
    assert!(took.is_some(), "the grant never let it hold: {last:?}");
    let at = format!("@{sid}");
    let by = format!("held by 127.0.0.1 (node {}) over the fleet", &node[..8]);

    let local = ctl(&held, &[&at, "send", "x"]);
    let said = String::from_utf8_lossy(&local.stderr);
    assert_eq!(local.status.code(), Some(1), "{local:?}");
    assert!(
        said.contains("origin=remote")
            && said.contains(&by)
            && said.contains(&format!("`aterm fleet peers rm {node}`"))
            && said.contains(&format!("`aterm ctl hold {sid} off`")),
        "the refusal names who holds it and what lifts it: {said}"
    );
    let stands = |says: &str| {
        let off = ctl(&held, &["hold", &sid, "off"]);
        let said = String::from_utf8_lossy(&off.stderr);
        assert_eq!(off.status.code(), Some(1), "{off:?}");
        assert!(
            said.contains(says),
            "a node that may still drive keeps its hold: {said}"
        );
        assert_eq!(field(&status(&held, &sid), "hold_by"), Some("remote"));
    };
    stands("which still drives this session's Mac");

    let cfg = driver.tmp.join("cfg/aterm/aterm.toml");
    let text = std::fs::read_to_string(&cfg).expect("the driver's config");
    let unlistened: String = text
        .lines()
        .filter(|line| !line.starts_with("listen = "))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_ne!(
        unlistened, text,
        "the driver's [fleet] listen was taken out"
    );
    std::fs::write(&cfg, unlistened).expect("write the driver's config");
    fleet_reads(&driver, &driver_sid, "off");
    stands(&format!(
        "which may still drive this session's Mac: its place did not prove it just now (nothing \
         listens on 127.0.0.1:{member_port}"
    ));
    let (off, _) = drive_ok(&driver.tmp, &target, &["hold", &sid, "off"]);
    assert_eq!(
        off.trim(),
        "OK hold=0",
        "its own CLI drives with no listener there: {off}"
    );
    let (on, _) = drive_ok(&driver.tmp, &target, &["hold", &sid, "on"]);
    assert_eq!(on.trim(), "OK hold=1", "{on}");

    fleet_ok(&driver.tmp, &["leave"], None);
    fleet_ok(
        &driver.tmp,
        &["init", "--host", "127.0.0.1", "--port", &member_port_word],
        None,
    );
    fleet_reads(&driver, &driver_sid, "listening");
    let lifted = ctl_ok(&held, &["hold", &sid, "off"]);
    assert_eq!(lifted.trim(), "OK hold=0 lifted=remote", "{lifted}");
    assert_eq!(field(&status(&held, &sid), "hold_by"), Some("-"));
    ctl_ok(&held, &[&at, "send", "y"]);
}

#[test]
fn nothing_listening_here_is_told_as_the_aterm_here_stands() {
    let (holder, port) = hold_a_port();
    drop(holder);
    let inst = boot_world(
        "afnl",
        |tmp, cmd| {
            use std::os::unix::fs::PermissionsExt as _;
            // The aterm here reads another aterm.toml than the fleet verbs write.
            let own = tmp.join("cfg-here");
            std::fs::create_dir_all(own.join("aterm")).expect("its config folder");
            for dir in [own.clone(), own.join("aterm")] {
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                    .expect("chmod");
            }
            std::fs::copy(
                tmp.join("cfg/aterm/aterm.toml"),
                own.join("aterm/aterm.toml"),
            )
            .expect("its config");
            cmd.env("XDG_CONFIG_HOME", &own)
                .env("ATERM_STATE_HOME", tmp.join("state"))
                .env("ATERM_DEBUG_FLEET_RETRY_MS", RETRY_MS)
                .args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    );
    let (_, sid) = boot_session(&inst);
    let pid = i32::try_from(inst.child.id()).expect("a pid");
    let signal = |sig| {
        // SAFETY: `kill(2)` on the instance this test started and still holds.
        assert_eq!(unsafe { libc::kill(pid, sig) }, 0, "signal {sig}");
    };
    let state = inst.tmp.join("state");
    let stop = || {
        // Do not freeze a heartbeat while it owns the book: the next fleet verb
        // needs that lock too. Keep it here until the kernel confirms the stop,
        // so the node cannot take it between the signal and its delivery.
        let book = aterm_net::fleet_member::lock_waiting(HANG, || {
            aterm_net::fleet_member::hold_book(&state)
        })
        .expect("hold the book while stopping its node");
        signal(libc::SIGSTOP);
        aterm_spec::wait::until_within(HANG, "the owned fleet node stopped", || {
            aterm_uds::process::is_stopped(inst.child.id()).then_some(())
        });
        drop(book);
    };
    let config = inst.tmp.join("cfg/aterm/aterm.toml");
    let place = format!("127.0.0.1:{port}");
    let port_word = port.to_string();

    stop();
    let init = fleet(
        &inst.tmp,
        &["init", "--host", "127.0.0.1", "--port", &port_word],
        None,
    );
    signal(libc::SIGCONT);
    let said = String::from_utf8_lossy(&init.stdout);
    assert!(
        init.status.success()
            && said.contains(&format!(
                "  listener   nothing listens on {place}: the aterm running here (pid {pid}) is \
                 stopped, so it answers nothing and listens nowhere: `kill -CONT {pid}` resumes it"
            ))
            && !said.contains("follows [fleet]"),
        "a stopped aterm takes a connection and answers nothing: {init:?}"
    );

    fleet_reads(&inst, &sid, "off");
    let head = |listed: String| listed.lines().next().unwrap_or_default().to_owned();
    let off = head(peers_answered(&inst.tmp));
    assert!(
        off.contains(&format!(
            "([fleet] listen is {place} and allow is facts, but nothing listens on {place} just \
             now: the aterm running here reads fleet=off, so it has not read [fleet] yet, or it \
             reads another aterm.toml than {}, whose [fleet] is off",
            config.display()
        )),
        "{off}"
    );

    let bogus = std::fs::read_to_string(&config)
        .expect("the verbs' config")
        .replace("allow = \"facts\"", "allow = \"bogus\"");
    assert!(bogus.contains("allow = \"bogus\""), "{bogus}");
    std::fs::write(inst.tmp.join("cfg-here/aterm/aterm.toml"), &bogus)
        .expect("write the aterm's own config");
    fleet_reads(&inst, &sid, "refused:allow");
    let refused = head(peers_answered(&inst.tmp));
    assert!(
        refused.contains(&format!(
            "([fleet] listen is {place} and allow is facts, but nothing listens on {place} just \
             now: the aterm running here refused the listen (fleet=refused:allow): the [fleet] \
             allow it read is none of \"facts\", \"content\" and \"drive\""
        )) && !refused.contains("serving"),
        "{refused}"
    );

    let valid = std::fs::read_to_string(&config).expect("the verbs' config");
    std::fs::write(inst.tmp.join("cfg-here/aterm/aterm.toml"), &valid)
        .expect("write the aterm's own config");
    fleet_reads(&inst, &sid, "listening");
    std::fs::write(&config, &bogus).expect("write the verbs' config");
    let proved = head(peers_answered(&inst.tmp));
    std::fs::write(&config, &valid).expect("write the verbs' config");
    assert!(
        proved.contains(&format!(
            "(it listens on {place}, though [fleet] allow is \"bogus\", none of \"facts\", \
             \"content\" and \"drive\": the aterm listening there has not read it yet, or reads \
             another aterm.toml"
        )) && !proved.contains("no aterm here listens"),
        "its node's proof outweighs aterm.toml's word: {proved}"
    );

    stop();
    let rotate = fleet(&inst.tmp, &["init", "--rotate"], None);
    let stalled = head(fleet_ok(&inst.tmp, &["peers"], None));
    signal(libc::SIGCONT);
    let said = String::from_utf8_lossy(&rotate.stdout);
    let stopped = format!(
        "the aterm running here (pid {pid}) is stopped, so a listener it bound takes a connection \
         in the kernel and answers nothing: `kill -CONT {pid}` resumes it"
    );
    assert!(
        rotate.status.success()
            && said.contains(&format!(
                "  listener   {place} takes a connection but finishes no fleet handshake: {stopped}"
            ))
            && !said.contains("follows [fleet]"),
        "a stopped aterm's bound listener takes a connection and answers nothing: {rotate:?}"
    );
    assert!(
        stalled.contains(&format!(
            "but {place} takes a connection but finishes no fleet handshake just now: {stopped}"
        )),
        "{stalled}"
    );
}
