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
    BookRead, BookUnread, DEFAULT_PORT, Keyring, Registration, Target, Unregistered, Via, dial,
    fetch_book, read_node_identity, read_peers, read_whole, register_by,
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

// The dev seams that set how long a verb's read of its issuer's book, a join's dial and
// registration, a local `hold off`'s proof of a holder, each listener's TLS and proofs, a `peers`
// read whose every listener must answer
// ([`peers_answered`]) and a survey whose every peer must answer ([`survey_answered`]) wait. Each
// issuer a verb here reads and each dialer of a listener is an aterm (or this test) on this
// machine that serves, each holder a `hold off` proves serves or is refused at once, and so do
// each listener a [`peers_answered`] read dials (with the aterm here whose status it asks, which
// answers or runs nowhere) and each peer a [`survey_answered`] read dials, so each answer comes or
// ends however loaded the machine is: each is given the hang detector (a survey's peer read twice
// it, [`survey_answered`]), not the 3 s a person's verb or a listener allows nor the 10 s a
// survey's `--timeout` allows at most. The peers here that answer nothing (a held silent port, a
// stopped aterm) are read under each verb's own wait (its `--timeout`, or the one it keeps), which
// these seams do not reach: no [`peers_answered`] or [`survey_answered`] read meets one.
const ISSUER_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_ISSUER_WITHIN_MS";
const JOIN_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_JOIN_WITHIN_MS";
const PEER_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_PEER_WITHIN_MS";
const SURVEY_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_SURVEY_WITHIN_MS";
const HOLDER_SEAM: &str = "ATERM_DEBUG_FLEET_HOLDER_WITHIN_MS";
const HANDSHAKE_SEAM: &str = "ATERM_DEBUG_FLEET_HANDSHAKE_MS";

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
            let hang = HANG.as_millis().to_string();
            cmd.env(aterm_net::loopback::SEAM, "1")
                .env(ISSUER_WITHIN_SEAM, &hang)
                .env(HOLDER_SEAM, &hang)
                .env(HANDSHAKE_SEAM, &hang);
            configure(tmp, cmd);
        },
        ready,
    )
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
    let inst = boot_on(
        "aflu",
        "[fleet]\nlisten = \"192.0.2.1:7100\"\nallow = \"facts\"\n",
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
        .env(JOIN_WITHIN_SEAM, HANG.as_millis().to_string())
        .envs(env.iter().copied());
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
        cmd.env("ATERM_STATE_HOME", root.join("state"))
            .env(aterm_net::loopback::SEAM, "1")
            .env(ISSUER_WITHIN_SEAM, HANG.as_millis().to_string())
            .envs(env.iter().copied());
        let mut child = cmd.spawn().expect("spawn aterm ctl");
        if let Some(bytes) = stdin {
            let mut pipe = child.stdin.take().expect("its stdin");
            pipe.write_all(bytes).expect("write its stdin");
        }
        crate::child_output::wait_and_read(child, Instant::now() + HANG)
            .unwrap_or_else(|| panic!("aterm ctl {args:?} did not exit"))
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

/// The binary is no Cargo test binary, so a host named with no port is at [`DEFAULT_PORT`], where
/// another Mac's fleet listens unless told otherwise: only a test binary holds a port in its place.
#[test]
fn the_binary_places_a_host_named_with_no_port_at_the_default_port() {
    let world = World(headless_boot::scratch_root("afdp"));
    let added = fleet_ok(&world.0, &["peers", "add", "lab-maxi"], None);
    let want = format!("aterm fleet peers: added lab-maxi:{DEFAULT_PORT} to ");
    assert!(added.starts_with(&want), "{added}");
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
    driven_pair_beating(tags, None)
}

/// [`driven_pair`], the member's heartbeat every `beat_ms` (`ATERM_DEBUG_FLEET_HEARTBEAT_MS`)
/// where one is named.
fn driven_pair_beating(
    tags: (&str, &str),
    beat_ms: Option<&str>,
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
                .env("ATERM_DEBUG_FLEET_LIFETIME_MS", NEVER_MS)
                .args(["--lines", "24", "--columns", "80"]);
            if let Some(ms) = beat_ms {
                cmd.env("ATERM_DEBUG_FLEET_HEARTBEAT_MS", ms);
            }
        },
        |_| true,
    );
    let (_, worker) = boot_session(&member);
    fleet_reads(&member, &worker, "listening");
    (issuer, member, worker, format!("127.0.0.1:{member_port}"))
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
    // Write from this separate process, without issuing a receiver-side inbox command.
    let durable = receiver
        .tmp
        .join("state/fleet-mail")
        .join(format!("{session}.toml"));
    {
        use aterm_toml::Value;
        use aterm_uds::private_file::{PrivateLock, write_private};
        let dir = durable.parent().unwrap();
        let name = durable.file_name().unwrap().to_str().unwrap();
        let _lock =
            PrivateLock::take(dir, &format!("{name}.lock"), HANG, "test handoff writer").unwrap();
        let mut mailbox: Value =
            aterm_toml::from_str(&std::fs::read_to_string(&durable).unwrap()).unwrap();
        let mut rows = mailbox.get("rows").unwrap().as_array().unwrap().clone();
        let mut late = rows[0].clone();
        let row = late.as_table_mut().unwrap();
        row.insert("id".to_owned(), Value::Integer(2));
        row.insert("key".to_owned(), Value::String("late-handoff".to_owned()));
        row.insert(
            "text".to_owned(),
            Value::String("PRIVATE_HANDOFF_BODY".to_owned()),
        );
        rows.push(late);
        let table = mailbox.as_table_mut().unwrap();
        table.insert("next".to_owned(), Value::Integer(2));
        table.insert("rows".to_owned(), Value::Array(rows));
        write_private(
            dir,
            name,
            aterm_toml::to_string(&mailbox).unwrap().as_bytes(),
        )
        .unwrap();
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
    assert!(
        std::fs::read_to_string(durable)
            .unwrap()
            .contains("fleet-mail-only-answer")
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
    let (issuer, member, worker, target) = driven_pair_beating(("afai", "afam"), Some(BEAT_MS));
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
    let own_has_third = read_peers(&member_state)
        .expect("the member's book")
        .iter()
        .any(|peer| peer.node.as_deref() == Some(third_node.as_str()));
    let copy_has_third =
        aterm_net::fleet_member::read_issuer_book(&member_state, issuer_node.trim())
            .iter()
            .any(|peer| peer.node.as_deref() == Some(third_node.as_str()));
    assert!(
        !own_has_third && !copy_has_third,
        "removal from both books must leave no member binding: own={own_has_third} copy={copy_has_third}"
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
    let config = inst.tmp.join("cfg/aterm/aterm.toml");
    let place = format!("127.0.0.1:{port}");
    let port_word = port.to_string();

    signal(libc::SIGSTOP);
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

    signal(libc::SIGSTOP);
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
