// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric` against a REAL broker — `aterm-link broker`, the binary the
//! launchd job runs — in a temp dir of its own, driven through the real
//! `aterm-link fabric` command line.
//!
//! EVERY RUN IS SEALED OFF FROM THE MACHINE IT RUNS ON: its own
//! `$XDG_CONFIG_HOME` (so the operator's aterm.toml is never read), its own
//! `$XDG_RUNTIME_DIR` holding an EMPTY control-socket directory (so no live aterm
//! instance is ever dialed), and `ATERM_FABRIC_COMMAND` removed unless a test
//! sets it. No bridge and no aterm are started: what is under test is the
//! report's own reading of the config, the broker and the bus.

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use astream_broker::Client;

const BIN: &str = env!("CARGO_BIN_EXE_aterm-link");

/// How long anything here may take before it is a failure rather than a slow
/// machine.
const DEADLINE: Duration = Duration::from_secs(20);

/// A private directory under `/tmp` — short, because a Unix socket path must
/// fit `sun_path` (104 bytes on macOS) and a per-user `$TMPDIR` does not.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = PathBuf::from(format!("/tmp/atf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["cfg/aterm", "run/aterm", "link"] {
            std::fs::create_dir_all(dir.join(sub)).expect("scratch dir");
        }
        std::fs::write(dir.join("link/node"), "n-00000000000000aa\n").expect("node id");
        // Two grants. An unguarded broker acknowledges an attach without
        // checking it, so the tags only have to be well-formed 32-byte ones.
        let tag_hex = "ab".repeat(32);
        std::fs::write(
            dir.join("node.cap"),
            format!(
                "rw,p=n-00000000000000aa:/f/t1/pub/n-00000000000000aa/> {tag_hex}\n\
                 ro:/f/t1/> {tag_hex}\n"
            ),
        )
        .expect("cap file");
        Self { dir }
    }

    fn path(&self, rel: &str) -> String {
        self.dir.join(rel).to_string_lossy().into_owned()
    }

    fn sock(&self) -> String {
        self.path("b.sock")
    }

    /// The bridge command, in the spelling `tools/fabric-enable.sh` writes
    /// (`aterm link serve`) or the argv0 alias's (`aterm-link serve`).
    fn command(&self, sock: &str, alias: bool) -> String {
        let program = if alias {
            format!("{BIN} serve")
        } else {
            "/usr/local/bin/aterm link serve".to_string()
        };
        format!(
            "{program} --fleet t1 --broker {sock} --cap-file {} --state {}",
            self.path("node.cap"),
            self.path("link")
        )
    }

    fn write_config(&self, body: &str) {
        std::fs::write(self.dir.join("cfg/aterm/aterm.toml"), body).expect("config");
    }

    fn config_path(&self) -> String {
        self.path("cfg/aterm/aterm.toml")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `aterm-link fabric <args>` in the sealed environment, with `env_cmd` as
/// `ATERM_FABRIC_COMMAND` when given.
fn fabric_cmd(s: &Scratch, env_cmd: Option<&str>, args: &[&str]) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.arg("fabric")
        .args(args)
        .env_remove("ATERM_FABRIC_COMMAND")
        .env_remove("ATERM_CONTROL_SOCK")
        .env("XDG_CONFIG_HOME", s.path("cfg"))
        .env("XDG_RUNTIME_DIR", s.path("run"))
        .stdin(Stdio::null());
    if let Some(c) = env_cmd {
        cmd.env("ATERM_FABRIC_COMMAND", c);
    }
    cmd
}

fn fabric(s: &Scratch, env_cmd: Option<&str>, args: &[&str]) -> (i32, String, String) {
    let Output {
        status,
        stdout,
        stderr,
    } = fabric_cmd(s, env_cmd, args)
        .output()
        .expect("run aterm-link fabric");
    (
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

fn json(text: &str) -> aterm_json::Value {
    aterm_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// A broker process, killed on drop.
struct Broker {
    child: Child,
}

impl Broker {
    /// `aterm-link broker <sock> <log>`, returned once it prints `listening`.
    fn spawn(s: &Scratch) -> Self {
        let mut child = Command::new(BIN)
            .args(["broker", &s.sock(), &s.path("b.log")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the broker");
        let lines = line_reader(child.stdout.take().expect("stdout"));
        let first = lines
            .recv_timeout(DEADLINE)
            .expect("the broker prints its readiness line");
        assert_eq!(first, format!("listening {}", s.sock()));
        Self { child }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A child's stdout, one line at a time, on a channel — so a read can time out.
fn line_reader(out: ChildStdout) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// The producer sequence every publish here takes the next of. ONE counter for
/// the process: the broker dedups on `(producer_id, producer_seq)`, so a
/// sequence that restarted per call would have a second batch acknowledged at
/// the FIRST batch's offsets and never appended.
static SEQ: AtomicU64 = AtomicU64::new(1);

/// Publish `(subject, body)` records straight to the broker, as a bridge or a
/// human would; answers each one's offset.
fn publish(sock: &str, records: &[(String, String)]) -> Vec<u64> {
    let mut c = Client::connect(sock).expect("connect to the test broker");
    records
        .iter()
        .map(|(subject, body)| {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let (off, deduped) = c
                .publish(0xfab, seq, subject, body.as_bytes())
                .expect("publish");
            assert!(!deduped, "the broker deduped a fresh record at @{off}");
            off
        })
        .collect()
}

/// An `in` record: `<src>` tells `<sid>@<node>` something of `kind`.
fn msg(node: &str, sid: &str, src: &str, kind: &str, text: &str) -> (String, String) {
    (
        format!("/f/t1/in/{node}/{sid}/{src}/{kind}"),
        format!("v=1 t=1789400000000 text={}", text.replace(' ', "%20")),
    )
}

fn arr(v: &aterm_json::Value, key: &str) -> Vec<aterm_json::Value> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("`{key}` is an array: {v:?}"))
        .clone()
}

/// NO COMMAND ANYWHERE IS "OFF", exit 2 — and the line says where it looked and
/// what turns it on. A blank command is off too, as it is for the app; a config
/// that is not TOML is UNREADABLE, also 2.
#[test]
fn with_no_command_the_fabric_is_off_says_where_it_looked_and_exits_2() {
    let s = Scratch::new("off");
    let (code, out, _) = fabric(&s, None, &[]);
    assert_eq!(code, 2, "{out}");
    assert_eq!(
        out.trim(),
        format!(
            "fabric is off: no [fabric] command in {} (and no $ATERM_FABRIC_COMMAND, no \
             rendezvous file); `aterm fabric on` turns it on",
            s.config_path()
        )
    );
    let (code, out, _) = fabric(&s, None, &["--json"]);
    assert_eq!(code, 2);
    let v = json(&out);
    assert_eq!(v["exit"].as_u64(), Some(2));
    assert!(
        v["off"].as_str().unwrap_or("").starts_with("fabric is off"),
        "{out}"
    );

    s.write_config("font_px = 13.0\n\n[fabric]\ncommand = \"  \"\n");
    let (code, out, _) = fabric(&s, None, &["status"]);
    assert_eq!(code, 2, "a blank command is off: {out}");

    s.write_config("[fabric\ncommand = \"x\"\n");
    let (code, out, _) = fabric(&s, None, &[]);
    assert_eq!(code, 2);
    assert!(out.starts_with("fabric config is unreadable:"), "{out}");

    // A command that is not a bridge is unreadable, not a broker to dial.
    s.write_config("[fabric]\ncommand = \"/usr/bin/false\"\n");
    let (code, out, _) = fabric(&s, None, &[]);
    assert_eq!(code, 2);
    assert!(out.contains("the command is not a bridge"), "{out}");

    // `tail` refuses the same way.
    let (code, _, err) = fabric(&s, None, &["tail"]);
    assert_eq!(code, 2);
    assert!(err.contains("the command is not a bridge"), "{err}");
}

/// A SOCKET FILE IS NOT A BROKER. A socket that was bound and abandoned — the
/// leftover of a broker that died — refuses the connect: `reachable NO`, a
/// warning naming the socket, exit 1. The config is read in the spelling
/// `tools/fabric-enable.sh` writes.
#[test]
fn a_dead_socket_file_is_unreachable_and_a_warning() {
    let s = Scratch::new("dead");
    let sock = s.sock();
    drop(UnixListener::bind(&sock).expect("bind"));
    assert!(Path::new(&sock).exists(), "the socket FILE is still there");
    s.write_config(&format!(
        "# the operator's file\n[fabric]\ncommand = \"{}\"\n",
        s.command(&sock, false)
    ));

    let (code, out, _) = fabric(&s, None, &[]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains(&format!(
            "source     [fabric] command in {}",
            s.config_path()
        )),
        "{out}"
    );
    assert!(out.contains("reachable  NO — connect:"), "{out}");
    assert!(
        out.contains(&format!("! the broker at {sock} does not answer (connect:")),
        "{out}"
    );
    assert!(out.contains("cap file   "), "{out}");
    assert!(out.contains("(2 grants)"), "{out}");

    let (code, out, _) = fabric(&s, None, &["--json"]);
    assert_eq!(code, 1);
    let v = json(&out);
    assert_eq!(v["exit"].as_u64(), Some(1));
    assert_eq!(v["config"]["source"].as_str(), Some("config"));
    assert_eq!(v["config"]["fleet"].as_str(), Some("t1"));
    assert_eq!(v["config"]["node"].as_str(), Some("n-00000000000000aa"));
    assert_eq!(v["broker"]["reachable"].as_bool(), Some(false));
    assert!(v["broker"]["head"].is_null());
    assert!(!arr(&v, "warnings").is_empty());
    assert!(arr(&v, "traffic").is_empty());
}

/// A LIVE BROKER, READ FOR REAL — and the env override beats the file. The
/// config names a dead broker; `ATERM_FABRIC_COMMAND` names the live one in the
/// argv0 alias's spelling, and the live one is what gets read: its head, its
/// pid, the presence roster and the last ten records — metadata only.
#[test]
fn a_live_broker_is_read_for_real_and_the_env_overrides_the_file() {
    let s = Scratch::new("live");
    let broker = Broker::spawn(&s);
    s.write_config(&format!(
        "[fabric]\ncommand = \"{}\"\n",
        s.command("/tmp/atf-nowhere.sock", false)
    ));
    let env_cmd = s.command(&s.sock(), true);

    // A REMOTE node's presence (not this machine's node, so no instance here
    // is expected to host it), then twelve messages: TRAFFIC must keep the
    // last ten.
    let far = "n-00000000000000ff";
    let mut records = vec![
        (
            format!("/f/t1/pub/{far}/node/presence"),
            "v=1 t=1789400000000 inc=1 state=live fabric=connected host=far".to_string(),
        ),
        (
            format!("/f/t1/pub/{far}/s-far/presence"),
            "v=1 t=1789400000000 inc=1 state=live hold=0 holder=-".to_string(),
        ),
        (
            format!("/f/t1/pub/{far}/s-old/presence"),
            "v=1 t=1789400000000 inc=1 state=exited hold=0".to_string(),
        ),
    ];
    for i in 0..12 {
        records.push(msg(
            far,
            "s-far",
            "h-andrew",
            if i % 2 == 0 { "task" } else { "note" },
            &format!("the secret plan number {i}"),
        ));
    }
    let offs = publish(&s.sock(), &records);
    let head = offs.last().copied().expect("offsets") + 1;

    let (code, out, err) = fabric(&s, Some(&env_cmd), &["--json"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(!out.contains("secret"), "a body reached the report: {out}");
    let v = json(&out);
    assert_eq!(v["schema"].as_u64(), Some(1));
    assert_eq!(v["exit"].as_u64(), Some(0));
    for key in [
        "config", "broker", "bridges", "sessions", "traffic", "warnings", "nodes",
    ] {
        assert!(!v[key].is_null(), "`{key}` missing: {out}");
    }
    assert_eq!(v["config"]["source"].as_str(), Some("env"));
    assert_eq!(v["config"]["command"].as_str(), Some(env_cmd.as_str()));
    assert_eq!(
        arr(&v["config"], "cap_files")[0]["grants"].as_u64(),
        Some(2)
    );
    let b = &v["broker"];
    assert_eq!(b["reachable"].as_bool(), Some(true));
    assert_eq!(b["attached"].as_bool(), Some(true));
    assert_eq!(b["endpoint"].as_str(), Some(s.sock().as_str()));
    assert_eq!(b["head"].as_u64(), Some(head), "the bus head offset");
    assert_eq!(
        b["pid"].as_u64(),
        Some(u64::from(broker.pid())),
        "the pid serving `link broker <socket>`"
    );
    assert!(arr(&v, "bridges").is_empty(), "no aterm instance is dialed");
    assert!(arr(&v, "warnings").is_empty(), "{out}");

    // TRAFFIC: exactly the last ten, oldest first, with every column.
    let traffic = arr(&v, "traffic");
    assert_eq!(traffic.len(), 10, "{out}");
    let got: Vec<u64> = traffic.iter().filter_map(|t| t["off"].as_u64()).collect();
    assert_eq!(got, offs[offs.len() - 10..].to_vec());
    let last = traffic.last().expect("a row");
    assert_eq!(last["from"].as_str(), Some("h-andrew"));
    assert_eq!(last["to"].as_str(), Some(format!("s-far@{far}").as_str()));
    assert_eq!(last["kind"].as_str(), Some("note"));
    assert_eq!(last["trust"].as_str(), Some("human"));
    assert_eq!(
        last["len"].as_u64(),
        Some("the secret plan number 11".len() as u64)
    );
    assert_eq!(last["t_ms"].as_u64(), Some(1_789_400_000_000));
    assert_eq!(last["time"].as_str(), Some("2026-09-14T15:33:20Z"));

    // SESSIONS: the live remote session is listed; the exited one is counted.
    let sessions = arr(&v, "sessions");
    assert_eq!(sessions.len(), 1, "{out}");
    assert_eq!(sessions[0]["sid"].as_str(), Some("s-far"));
    assert!(sessions[0]["pid"].is_null(), "no instance here hosts it");
    assert_eq!(v["exited_sessions"].as_u64(), Some(1));
    assert_eq!(arr(&v, "nodes")[0]["host"].as_str(), Some("far"));

    // The TEXT: six sections, in order, the same facts, and still no body.
    let (code, text, _) = fabric(&s, Some(&env_cmd), &["status"]);
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("secret"), "{text}");
    let at = |h: &str| {
        text.find(&format!("\n{h}"))
            .unwrap_or_else(|| panic!("no {h} section: {text}"))
    };
    let order = [
        at("CONFIG"),
        at("BROKER"),
        at("BRIDGES"),
        at("SESSIONS"),
        at("TRAFFIC"),
        at("WARNINGS"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{text}");
    assert!(text.contains("source     $ATERM_FABRIC_COMMAND"), "{text}");
    assert!(text.contains("reachable  yes"), "{text}");
    assert!(
        text.contains(&format!("pid        {}", broker.pid())),
        "{text}"
    );
    assert!(text.contains(&format!("bus head   @{head}")), "{text}");
    assert!(
        text.contains("TRAFFIC  the last 10 records under /f/t1/>"),
        "{text}"
    );
    assert!(
        text.contains("(+1 exited session on the bus not shown"),
        "{text}"
    );
    assert!(text.contains("WARNINGS  none"), "{text}");
}

/// A broker that ANSWERS `Hello` and `Attach` and REFUSES the read, on `sock`,
/// until the test process exits — what `Broker::open_guarded` does when the cap
/// file's grants do not cover `/f/<fleet>/>` (a `--fleet` typo in the bridge
/// command), and what a broker that dies between the attach and the head query
/// does too.
fn refusing_broker(sock: &str) {
    use astream_broker::proto::{
        decode_request, encode_response, read_frame, write_frame, Request, Response,
    };
    let listener = UnixListener::bind(sock).expect("bind the refusing broker");
    std::thread::spawn(move || {
        while let Ok((mut c, _)) = listener.accept() {
            std::thread::spawn(move || {
                while let Ok(Some(p)) = read_frame(&mut c) {
                    let answer = match decode_request(&p) {
                        Some(Request::Hello) => Response::Nonce {
                            nonce: vec![0x11; 32],
                        },
                        Some(Request::Attach { .. }) => Response::Mark {
                            next: 0,
                            head: 7,
                            resume: String::new(),
                        },
                        _ => Response::Error {
                            code: 5,
                            msg: "unauthorized: capability does not grant this subject/filter"
                                .to_string(),
                        },
                    };
                    if write_frame(&mut c, &encode_response(&answer)).is_err() {
                        return;
                    }
                }
            });
        }
    });
}

/// **A BROKER THAT REFUSES THE READ IS NOT `reachable yes`.** The connect, the
/// `Hello` and every `Attach` succeed; the head query is refused. Nothing of
/// the bus can be read — no head, no traffic, no roster, no standing halt — and
/// the report must not call that healthy, must print the broker's own reason,
/// and must exit 1.
#[test]
fn a_broker_that_refuses_the_read_is_not_reachable_yes() {
    let s = Scratch::new("refuse");
    let sock = s.path("refuse.sock");
    refusing_broker(&sock);
    let cmd = s.command(&sock, true);

    let (code, out, _) = fabric(&s, Some(&cmd), &[]);
    assert!(
        !out.contains("head query answered"),
        "the head query did NOT answer:\n{out}"
    );
    assert!(
        out.contains("unauthorized"),
        "the broker\u{27}s refusal is printed nowhere:\n{out}"
    );
    assert!(out.contains("bus head   -"), "{out}");
    assert_eq!(code, 1, "a bus that cannot be read is not healthy:\n{out}");

    let (code, out, _) = fabric(&s, Some(&cmd), &["--json"]);
    let v = json(&out);
    assert!(!arr(&v, "warnings").is_empty(), "{out}");
    assert_eq!(code, 1, "{out}");
}

/// A STANDING FLEET HALT is a warning — the bus says every session is held.
#[test]
fn a_fleet_halt_on_the_bus_is_a_warning() {
    let s = Scratch::new("halt");
    let _broker = Broker::spawn(&s);
    publish(
        &s.sock(),
        &[(
            "/f/t1/fleet/h-andrew/halt".to_string(),
            "v=1 t=1789400000000 state=on reason=main%20broken".to_string(),
        )],
    );
    let (code, out, _) = fabric(&s, Some(&s.command(&s.sock(), true)), &[]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("! h-andrew has a fleet halt standing (reason=main broken)"),
        "{out}"
    );
}

/// Read `tail` lines until `n` record rows (they start `@`) have arrived.
fn tail_rows(lines: &mpsc::Receiver<String>, n: usize) -> Vec<String> {
    let start = Instant::now();
    let mut rows = Vec::new();
    while rows.len() < n {
        let left = DEADLINE.saturating_sub(start.elapsed());
        match lines.recv_timeout(left) {
            Ok(line) if line.starts_with('@') => rows.push(line),
            Ok(_) => {}
            Err(_) => panic!("only {} of {n} tail rows arrived: {rows:#?}", rows.len()),
        }
    }
    rows
}

struct Tail {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Tail {
    fn spawn(s: &Scratch, args: &[&str]) -> Self {
        let mut child = fabric_cmd(s, Some(&s.command(&s.sock(), false)), args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn tail");
        let lines = line_reader(child.stdout.take().expect("stdout"));
        Self { child, lines }
    }
}

impl Drop for Tail {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `tail --from` REPLAYS exactly the records from that offset, one line each,
/// in the TRAFFIC columns and with bodies hidden; `--bodies` adds the text
/// after the trust label; a bare `tail` shows only what arrives after it starts.
#[test]
fn tail_replays_from_an_offset_hides_bodies_and_follows_live() {
    let s = Scratch::new("tail");
    let _broker = Broker::spawn(&s);
    let node = "n-00000000000000aa";
    let offs = publish(
        &s.sock(),
        &[
            msg(node, "s-a", "h-andrew", "task", "alpha secret"),
            msg(node, "s-a", "s-b", "note", "beta secret"),
            msg(node, "s-b", "h-andrew", "ask", "gamma secret"),
        ],
    );

    let all = Tail::spawn(&s, &["tail", "--from", "0"]);
    let rows = tail_rows(&all.lines, 3);
    for (row, (off, kind)) in rows.iter().zip(offs.iter().zip(["task", "note", "ask"])) {
        let cols: Vec<&str> = row.split_whitespace().collect();
        assert_eq!(cols[0], format!("@{off}"), "{row}");
        assert_eq!(cols[2], kind, "{row}");
        assert!(
            !row.contains("secret"),
            "bodies are hidden by default: {row}"
        );
    }
    assert!(rows[0].contains("h-andrew  ") && rows[0].ends_with(&format!("s-a@{node}")));
    assert!(rows[0].contains(" human "), "{}", rows[0]);
    assert!(rows[1].contains(" agent "), "{}", rows[1]);

    // From the SECOND record, with bodies: two rows, text last.
    let from = offs[1].to_string();
    let bodies = Tail::spawn(&s, &["tail", "--bodies", "--from", &from]);
    let rows = tail_rows(&bodies.lines, 2);
    assert!(rows[0].starts_with(&format!("@{}", offs[1])), "{rows:#?}");
    assert!(rows[0].ends_with("text=beta secret"), "{rows:#?}");
    assert!(rows[1].ends_with("text=gamma secret"), "{rows:#?}");

    // A bare tail starts at the head: nothing old, then what arrives.
    let live = Tail::spawn(&s, &["tail"]);
    let header = live
        .lines
        .recv_timeout(DEADLINE)
        .expect("tail prints its column header once it is subscribed");
    assert!(header.starts_with("OFFSET"), "{header}");
    // No race: the head was read BEFORE the header was printed, and the
    // subscription starts there, so anything published now is delivered.
    let new = publish(
        &s.sock(),
        &[msg(node, "s-a", "h-andrew", "report", "delta")],
    );
    let rows = tail_rows(&live.lines, 1);
    assert!(rows[0].starts_with(&format!("@{}", new[0])), "{rows:#?}");
    assert!(rows[0].contains(" report "), "{rows:#?}");
}

/// THE COMMAND LINE: a usage error is exit 2 and names the word; `help` is
/// exit 0 and prints the usage.
#[test]
fn usage_errors_exit_2_and_help_exits_0() {
    let s = Scratch::new("usage");
    let (code, _, err) = fabric(&s, None, &["--aterm-front-door-routing-probe"]);
    assert_eq!(code, 2);
    assert!(
        err.starts_with("aterm fabric: unknown flag --aterm-front-door-routing-probe"),
        "{err}"
    );
    let (code, out, _) = fabric(&s, None, &["help"]);
    assert_eq!(code, 0);
    assert!(
        out.starts_with("usage: aterm fabric [status] [--json]"),
        "{out}"
    );
    let (code, _, err) = fabric(&s, None, &["tail", "--json"]);
    assert_eq!(code, 2);
    assert!(err.contains("--json is a `status` flag"), "{err}");
}
