// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 16 — a second host joins the fleet over the sealed transport**, end
//! to end, with the REAL commands an operator types: `aterm fabric on --tcp`
//! on host A, `aterm fabric mint-for` there, the two files copied, `aterm
//! fabric join` on host B — and then the work the join exists for: a `task`
//! from A's session lands in B's session's inbox AS A TASK, B marks it
//! handled, and the RECEIPT (round 15) comes back to A through the same sealed
//! broker; `aterm fabric` on each host lists both nodes and marks which one it
//! is.
//!
//! TWO HOSTS ON ONE MACHINE, and the substitution is the loopback argument
//! `two_nodes_sealed.rs` makes: the sealed record layer is a property of the
//! CONNECTION, not of the route. Each "host" is a scratch world of its own —
//! its own `$HOME`, `$XDG_CONFIG_HOME` (so its own aterm.toml),
//! `$XDG_RUNTIME_DIR` (so its own control sockets and rendezvous file),
//! `$ATERM_FABRIC_HOME` (so its own root, node id, cap and key), its own
//! headless `aterm-gui` and the bridge that gui launches, and its own
//! `$HOSTNAME` (so the presence rows' `host=` differ as two machines' would).
//! `--service none` everywhere: nothing here can reach launchd, the live
//! broker or a real session. The broker is the one `on --service none` names —
//! `aterm-link broker --tcp 127.0.0.1:<port> … --secret-file <A's secret>` —
//! run by the test, and GUARDED.
//!
//! The default build's half is at the bottom: `broker --tcp`, `on --tcp` and
//! `join` each refuse by naming the `sealed` feature.

#![cfg(unix)]
// The scaffolding below (the two hosts, their guis, the broker) serves the
// `sealed` tests; a default build compiles only the refusal test at the end.
#![cfg_attr(not(feature = "sealed"), allow(dead_code, unused_imports))]

mod harness;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use aterm_link::ctl::Ctl;
use aterm_link::fabric::kv;

const BIN: &str = env!("CARGO_BIN_EXE_aterm-link");

/// One "host": a scratch world under `/tmp` (short, for `sun_path`).
struct Host {
    dir: PathBuf,
    name: &'static str,
}

impl Host {
    fn new(tag: &str, name: &'static str) -> Self {
        let dir = PathBuf::from(format!("/tmp/r16{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["home", "cfg/aterm", "run", "inbox"] {
            std::fs::create_dir_all(dir.join(sub)).expect("scratch dir");
        }
        Self { dir, name }
    }

    fn path(&self, rel: &str) -> String {
        self.dir.join(rel).to_string_lossy().into_owned()
    }

    fn root(&self) -> PathBuf {
        self.dir.join("root")
    }

    /// `aterm-link <args>` in this host's environment — every inherited
    /// `ATERM_*` stripped first (the harness's rule: a run from inside an aterm
    /// would otherwise hand the caller's session to the proof).
    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("ATERM_") {
                cmd.env_remove(&name);
            }
        }
        cmd.args(args)
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("cfg"))
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("ATERM_FABRIC_HOME", self.path("root"))
            .env("ATERM_BIN", BIN)
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let Output {
            status,
            stdout,
            stderr,
        } = self.cmd(args).output().expect("run aterm-link");
        (
            status.code().unwrap_or(-1),
            String::from_utf8_lossy(&stdout).into_owned(),
            String::from_utf8_lossy(&stderr).into_owned(),
        )
    }

    fn node(&self) -> String {
        std::fs::read_to_string(self.root().join("link-state/node"))
            .expect("a provisioned node id")
            .trim()
            .to_string()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if std::env::var_os("ATERM_LINK_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// A headless aterm on a [`Host`], with NO fabric command: `on`/`join` arm it.
struct Gui {
    child: Child,
    ctl_sock: String,
    token: String,
    state: PathBuf,
}

impl Gui {
    fn boot(h: &Host) -> Self {
        let _permit = harness::boot_permit();
        harness::prepare_fixture_config(&h.dir);
        let log = std::fs::File::create(h.dir.join("gui.log")).expect("gui log");
        let err = log.try_clone().expect("gui log clone");
        let mut cmd = Command::new(harness::gui_binary());
        harness::prepare_gui_environment(&mut cmd);
        cmd.arg("--headless")
            .env("HOME", h.path("home"))
            .env("XDG_RUNTIME_DIR", h.path("run"))
            .env("XDG_CONFIG_HOME", h.path("cfg"))
            .env("SHELL", "/bin/sh")
            .env("ATERM_LINES", "40")
            .env("ATERM_COLUMNS", "120")
            // The bridge child inherits it, and its node presence row's
            // `host=` reads it first — two "hosts" on one machine say so.
            .env("HOSTNAME", h.name)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err);
        let child = cmd.spawn().expect("launch aterm-gui --headless");
        let ctl_sock = h.path("run/aterm/aterm.sock");
        let token = harness::World::wait_for_token_at(&ctl_sock);
        Self {
            child,
            ctl_sock,
            token,
            state: h.root().join("link-state"),
        }
    }

    fn ctl(&self) -> Ctl {
        let c = Ctl::connect(&self.ctl_sock, &self.token).expect("connect to aterm");
        let _ = c.get_ref().set_read_timeout(Some(Duration::from_secs(60)));
        c
    }

    fn verb(&self, line: &str) -> aterm_link::ctl::Reply {
        self.ctl()
            .request(line)
            .unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    fn session(&self) -> String {
        harness::until("aterm's boot session", || {
            self.verb("sessions")
                .rows()
                .first()
                .and_then(|r| r.split_whitespace().nth(1).map(str::to_string))
        })
    }
}

impl Drop for Gui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(pid) = std::fs::read_to_string(self.state.join("pid"))
            .ok()
            .and_then(|p| p.trim().parse::<i32>().ok())
        {
            harness::kill(pid, 9);
        }
    }
}

/// A process killed on drop — the test's own broker.
struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A loopback port nothing listens on right now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("an ephemeral loopback port")
}

/// Start the broker with EXACTLY the argv `on --service none` printed, and
/// wait for its readiness line.
fn spawn_printed_broker(argv_line: &str) -> Proc {
    let words: Vec<&str> = argv_line.split_whitespace().collect();
    let mut child = Command::new(words[0])
        .args(&words[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the broker `on` named");
    let mut first = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut first)
        .expect("the broker prints its readiness line");
    assert!(first.starts_with("listening 127.0.0.1:"), "{first}");
    Proc(child)
}

/// The step lines named `name` from an `on`/`join` run's STEPS block.
fn step<'a>(out: &'a str, name: &str) -> Vec<&'a str> {
    let steps = out
        .find("\naterm fabric — fleet")
        .map_or(out, |at| &out[..at]);
    steps
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with(name)
                && t[name.len()..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace)
        })
        .collect()
}

fn verdict<'a>(out: &'a str, name: &str) -> Vec<&'a str> {
    step(out, name)
        .into_iter()
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect()
}

fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777
}

fn chmod(p: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// **THE WHOLE m7 RECIPE, ON LOOPBACK, AND THE WORK IT EXISTS FOR.**
#[cfg(feature = "sealed")]
#[test]
fn a_second_host_joins_over_the_sealed_wire_and_a_task_round_trips_with_its_receipt() {
    let a = Host::new("a", "host-a");
    let b = Host::new("b", "host-b");
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let key = a.path("fleet.key");

    // ---- host A: an instance, then `on --tcp` with a FRESH key.
    let gui_a = Gui::boot(&a);
    let sid_a = gui_a.session();
    let (code, out, err) = a.run(&[
        "fabric",
        "on",
        "--tcp",
        &bind,
        "--key-file",
        &key,
        "--service",
        "none",
    ]);
    // No broker answers yet, and `--service none` starts none: exit 1 at the
    // broker step, AFTER the identity steps, naming the argv to run.
    assert_eq!(code, 1, "stdout:\n{out}\nstderr:\n{err}");
    for (name, v) in [
        ("root", "done"),
        ("node", "done"),
        ("secret", "done"),
        ("key", "done"),
        ("cap", "done"),
        ("broker", "FAILED"),
    ] {
        assert!(
            verdict(&out, name).contains(&v),
            "`{name}` should read {v}:\n{out}"
        );
    }
    assert_eq!(mode_of(Path::new(&key)), 0o600);
    assert_eq!(
        std::fs::read_to_string(&key).expect("key").trim().len(),
        64,
        "64 hex characters"
    );
    let secret = a.root().join("mint.secret");
    let printed = format!(
        "{BIN} broker --tcp {bind} --key-file {key} --secret-file {} --unix {} {}",
        secret.display(),
        a.root().join("bus.sock").display(),
        a.root().join("bus.log").display()
    );
    assert!(
        out.contains(&printed),
        "the broker argv `on` wants run:\n{out}"
    );
    let _broker = spawn_printed_broker(&printed);

    // The second `on`: identity already there, the broker answers, the rest
    // done — the instance armed and the proof through the SEALED broker.
    let (code, out, err) = a.run(&[
        "fabric",
        "on",
        "--tcp",
        &bind,
        "--key-file",
        &key,
        "--service",
        "none",
    ]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    for (name, v) in [
        ("root", "already"),
        ("node", "already"),
        ("secret", "already"),
        ("key", "already"),
        ("cap", "already"),
        ("broker", "already"),
        ("wire", "ok"),
        ("config", "done"),
        ("rendezvous", "done"),
        ("instance", "done"),
        ("proof", "ok"),
    ] {
        assert!(
            verdict(&out, name).contains(&v),
            "`{name}` should read {v}:\n{out}"
        );
    }
    let node_a = a.node();
    let config_a = std::fs::read_to_string(a.path("cfg/aterm/aterm.toml")).expect("config");
    let command_a = aterm_link::fabric::command_in_toml(&config_a)
        .expect("toml")
        .expect("a command");
    // Host A's OWN bridges dial the broker's socket — the one-host command —
    // and the port is recorded for the hosts that join (the review: a peer
    // that can reach the port can hold its handshake slots).
    assert_eq!(
        command_a,
        format!(
            "{BIN} serve --fleet local --broker {} --cap-file {} --state {} --accept-from \
             {node_a}",
            a.root().join("bus.sock").display(),
            a.root().join("node.cap").display(),
            a.root().join("link-state").display()
        )
    );
    let rv = std::fs::read_to_string(a.path("run/aterm/fabric.toml")).expect("rendezvous");
    assert!(
        rv.contains(&format!("serves_tcp = \"{bind}\""))
            && rv.contains(&format!("serves_key_file = \"{key}\""))
            && !rv.contains("transport = \"tcp+sealed\""),
        "{rv}"
    );
    // IDEMPOTENT: a third `on` changes nothing and says so.
    let (code, out, _) = a.run(&[
        "fabric",
        "on",
        "--tcp",
        &bind,
        "--key-file",
        &key,
        "--service",
        "none",
    ]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("nothing changed: the fabric was already on"),
        "{out}"
    );

    // ---- host A: `mint-for` a NEW node, into the file host B will get.
    let cap_b = b.path("inbox/b.cap");
    let (code, out, err) = a.run(&["fabric", "mint-for", "new", "--out", &cap_b]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert_eq!(mode_of(Path::new(&cap_b)), 0o600);
    let secret_bytes = std::fs::read(&secret).expect("secret");
    let secret_hex: String = secret_bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert!(
        !out.contains(&secret_hex) && !err.contains(&secret_hex),
        "the mint secret was printed"
    );
    assert!(
        out.contains(&format!("--accept-from {node_a}"))
            && out.contains(&format!("aterm fabric join --broker {bind} --tcp")),
        "mint-for names the join to run:\n{out}"
    );
    // Refusals: this host's own id, a malformed one, and a different cap
    // already at --out.
    let (code, _, err) = a.run(&["fabric", "mint-for", &node_a]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("THIS host's own node id"), "{err}");
    let (code, _, err) = a.run(&["fabric", "mint-for", "s-notanode"]);
    assert_eq!(code, 2, "{err}");
    let (code, _, err) = a.run(&["fabric", "mint-for", "n-other", "--out", &cap_b]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("holds a different cap"), "{err}");

    // ---- the COPY: the key and the cap, nothing else, 0600 on host B.
    let key_b = b.path("inbox/fleet.key");
    std::fs::copy(&key, &key_b).expect("copy the key");
    chmod(&key_b, 0o600);

    // ---- host B: an instance, a dry run that touches nothing, the join.
    let gui_b = Gui::boot(&b);
    let sid_b = gui_b.session();
    let join = |extra: &[&str]| {
        let mut args = vec![
            "fabric",
            "join",
            "--broker",
            &bind,
            "--tcp",
            "--key-file",
            &key_b,
            "--cap-file",
            &cap_b,
            "--accept-from",
            &node_a,
            "--service",
            "none",
        ];
        args.extend_from_slice(extra);
        b.run(&args)
    };
    let config_b_before = std::fs::read(b.path("cfg/aterm/aterm.toml")).expect("config");
    let (code, out, err) = join(&["--dry-run"]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert!(
        verdict(&out, "broker").contains(&"ok"),
        "the probe runs dry:\n{out}"
    );
    assert!(!b.root().exists(), "a dry run created the root");
    assert_eq!(
        std::fs::read(b.path("cfg/aterm/aterm.toml")).expect("config"),
        config_b_before
    );

    let (code, out, err) = join(&[]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    for (name, v) in [
        ("root", "done"),
        ("node", "done"),
        ("key", "done"),
        ("cap", "done"),
        ("broker", "ok"),
        ("config", "done"),
        ("rendezvous", "done"),
        ("instance", "done"),
        ("proof", "ok"),
    ] {
        assert!(
            verdict(&out, name).contains(&v),
            "`{name}` should read {v}:\n{out}"
        );
    }
    let node_b = b.node();
    assert_ne!(node_a, node_b, "two hosts, two nodes");
    assert_eq!(mode_of(&b.root().join("fleet.key")), 0o600);
    assert_eq!(mode_of(&b.root().join("node.cap")), 0o600);
    assert!(
        !b.root().join("mint.secret").exists(),
        "the mint secret never leaves host A"
    );
    let command_b = aterm_link::fabric::command_in_toml(
        &std::fs::read_to_string(b.path("cfg/aterm/aterm.toml")).expect("config"),
    )
    .expect("toml")
    .expect("a command");
    assert!(
        command_b.contains(&format!("--broker {bind} --tcp --key-file "))
            && command_b.ends_with(&format!("--accept-from {node_b},{node_a}")),
        "{command_b}"
    );
    // A second join changes nothing and says so.
    let (code, out, _) = join(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("nothing changed: this host had already joined"),
        "{out}"
    );

    // ---- THE WORK: a task from A's session to B's, the receipt back. B's
    // handler reads its high-water row id and PARKS on `await inbox` before
    // A posts — push, not a poll, so the measured round trip is the fabric's.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let handler = {
        let sock = gui_b.ctl_sock.clone();
        let token = gui_b.token.clone();
        let sid = sid_b.clone();
        std::thread::spawn(move || -> String {
            let mut ctl = Ctl::connect(&sock, &token).expect("connect B");
            let _ = ctl
                .get_ref()
                .set_read_timeout(Some(Duration::from_secs(90)));
            let before = ctl
                .request(&format!("@{sid} inbox 1 --peek --meta"))
                .expect("inbox");
            let since: u64 = before
                .rows()
                .iter()
                .filter(|r| r.starts_with("msg "))
                .filter_map(|r| r.split_whitespace().nth(1)?.parse().ok())
                .max()
                .unwrap_or(0);
            let _ = ready_tx.send(());
            let woke = ctl
                .request(&format!(
                    "@{sid} await inbox since={since} kinds=task timeout=60000"
                ))
                .expect("await");
            assert!(woke.ok(), "B's await: {}", woke.header());
            let rows = ctl.request(&format!("@{sid} inbox --peek")).expect("inbox");
            let row = rows
                .rows()
                .iter()
                .find(|r| r.starts_with("msg ") && kv(r, "kind") == Some("task"))
                .cloned()
                .unwrap_or_else(|| panic!("no task row on B: {:#?}", rows.rows()));
            let id = row.split_whitespace().nth(1).expect("id").to_string();
            let seen = ctl
                .request(&format!("@{sid} inbox seen {id} handled"))
                .expect("seen");
            assert!(seen.ok(), "inbox seen: {}", seen.header());
            row
        })
    };
    ready_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("B's handler is parked");
    let started = Instant::now();
    let posted = gui_a.verb(&format!(
        "@{sid_a} post to=@{sid_b} kind=task dl=30000 --wait-ack=60000 r16 loopback task"
    ));
    let round_trip = started.elapsed();
    let row_b = handler.join().expect("B's handler");
    assert!(
        posted.ok() && kv(posted.header(), "ack") == Some("handled"),
        "A's post must come back with B's verdict: {}",
        posted.header()
    );
    eprintln!("r16: task -> ack round trip over the sealed loopback broker: {round_trip:?}");
    // It arrived AS A TASK (B accepts A's node), from A's session attested by
    // A's node, over the broker the join named.
    assert!(
        row_b.contains(&format!("from={sid_a}@{node_a}")) && !row_b.contains("demoted="),
        "{row_b}"
    );
    let off = kv(posted.header(), "off").expect("off=").to_string();
    let inbox_a = gui_a.verb(&format!("@{sid_a} inbox --peek"));
    assert!(
        inbox_a.rows().iter().any(|r| kv(r, "kind") == Some("ack")
            && kv(r, "re") == Some(off.as_str())
            && kv(r, "verdict") == Some("handled")),
        "the receipt is in A's inbox: {:#?}",
        inbox_a.rows()
    );
    // And the other way, by the EXPLICIT cross-host address `@<sid>@<node>`:
    // B's report to A's session, correlated to A's task.
    let report = gui_b.verb(&format!(
        "@{sid_b} post to=@{sid_a}@{node_a} kind=report re={off} --wait r16 done"
    ));
    assert!(report.ok(), "B's report: {}", report.header());
    let back = harness::until("B's report in A's inbox", || {
        gui_a
            .verb(&format!("@{sid_a} inbox --peek"))
            .rows()
            .iter()
            .find(|r| kv(r, "kind") == Some("report"))
            .cloned()
    });
    assert!(
        back.contains(&format!("from={sid_b}@{node_b}")) && kv(&back, "re") == Some(off.as_str()),
        "{back}"
    );

    // ---- `aterm fabric` on each host lists BOTH nodes, marking which it is.
    for (host, me, other, other_sid) in [
        (&a, &node_a, &node_b, &sid_b),
        (&b, &node_b, &node_a, &sid_a),
    ] {
        let (code, text, err) = harness::until("both nodes on the roster", || {
            let r = host.run(&["fabric"]);
            (r.1.contains(other.as_str()) && r.1.contains(me.as_str())).then_some(r)
        });
        let nodes = text
            .split("\nNODES")
            .nth(1)
            .and_then(|t| t.split("\nBRIDGES").next())
            .unwrap_or_else(|| panic!("no NODES section:\n{text}"));
        let row = |n: &str| {
            nodes
                .lines()
                .find(|l| l.trim_start().starts_with(n))
                .unwrap_or_else(|| panic!("{n} is not in NODES:\n{text}"))
                .to_string()
        };
        assert!(
            row(me).contains(" this ") && row(me).contains(&format!("host={}", host.name)),
            "{}",
            row(me)
        );
        assert!(
            row(other).contains(" remote ") && row(other).contains("state=live fabric=connected"),
            "{}",
            row(other)
        );
        // The other node's session, from the bus's presence, under its node.
        assert!(
            text.lines()
                .any(|l| l.contains(other_sid.as_str()) && l.contains(other.as_str())),
            "the other host's session is not in SESSIONS:\n{text}"
        );
        assert!(
            text.contains("the faces the cap files grant"),
            "a guarded broker is read through the cap's faces:\n{text}"
        );
        assert_eq!(
            code, 0,
            "no warning on a healthy two-host fleet:\n{text}\n{err}"
        );
    }
    let (_, json, _) = a.run(&["fabric", "--json"]);
    let v: aterm_json::Value =
        aterm_json::from_str(&json).unwrap_or_else(|e| panic!("not JSON ({e}): {json}"));
    let nodes = v["nodes"].as_array().expect("nodes");
    let of = |n: &str| {
        nodes
            .iter()
            .find(|x| x["node"].as_str() == Some(n))
            .unwrap_or_else(|| panic!("{n} missing: {json}"))
    };
    assert_eq!(of(&node_a)["this"].as_bool(), Some(true), "{json}");
    assert_eq!(of(&node_b)["where"].as_str(), Some("remote"), "{json}");
    assert_eq!(of(&node_b)["host"].as_str(), Some("host-b"), "{json}");
    assert_eq!(of(&node_b)["sessions"].as_u64(), Some(1), "{json}");
    assert!(v["broker"]["faces"]
        .as_array()
        .is_some_and(|f| !f.is_empty()));
}

/// **THE GUARD AND THE KEY ARE REAL**: a cap minted under ANOTHER secret, and
/// the right cap under a WRONG key, are both refused by the probe — exit 1,
/// the reason named, and nothing written to host B's config.
#[cfg(feature = "sealed")]
#[test]
fn join_refuses_a_foreign_cap_and_a_wrong_key_before_writing_anything() {
    let a = Host::new("fa", "host-a");
    let b = Host::new("fb", "host-b");
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    // Host A's broker, guarded by a secret the test writes (0600).
    std::fs::create_dir_all(a.root()).expect("root");
    let secret = a.root().join("mint.secret");
    std::fs::write(&secret, [7u8; 32]).expect("secret");
    chmod(&secret.to_string_lossy(), 0o600);
    let key = a.path("fleet.key");
    std::fs::write(&key, format!("{}\n", "5a".repeat(32))).expect("key");
    chmod(&key, 0o600);
    let _broker = spawn_printed_broker(&format!(
        "{BIN} broker --tcp {bind} --key-file {key} --secret-file {} {}",
        secret.display(),
        a.path("bus.log")
    ));
    // A cap for a node, minted under a DIFFERENT secret.
    let node = "n-00000000000000bb";
    let forged: String = aterm_link::enable::node_grants("local", node)
        .iter()
        .map(|g| {
            let c = astream_cap::mint(&[9u8; 32], g).expect("mint");
            let tag: String = c.tag.iter().map(|b| format!("{b:02x}")).collect();
            format!("{} {tag}\n", c.filter)
        })
        .collect();
    let cap = b.path("inbox/forged.cap");
    std::fs::write(&cap, forged).expect("cap");
    chmod(&cap, 0o600);
    let key_b = b.path("inbox/fleet.key");
    std::fs::copy(&key, &key_b).expect("copy");
    chmod(&key_b, 0o600);
    let before = std::fs::read(b.path("cfg/aterm/aterm.toml")).ok();
    let args = |k: &str, c: &str| -> (i32, String, String) {
        b.run(&[
            "fabric",
            "join",
            "--broker",
            &bind,
            "--tcp",
            "--key-file",
            k,
            "--cap-file",
            c,
            "--service",
            "none",
        ])
    };
    let (code, out, err) = args(&key_b, &cap);
    assert_eq!(code, 1, "stdout:\n{out}\nstderr:\n{err}");
    assert!(
        out.contains("the broker refused the cap") && out.contains("unauthorized"),
        "{out}"
    );
    // A WRONG key: the handshake fails.
    let wrong = b.path("inbox/wrong.key");
    std::fs::write(&wrong, format!("{}\n", "11".repeat(32))).expect("key");
    chmod(&wrong, 0o600);
    let (code, out, _) = args(&wrong, &cap);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("this key is not the broker's"), "{out}");
    assert_eq!(
        std::fs::read(b.path("cfg/aterm/aterm.toml")).ok(),
        before,
        "a refused join wrote the config"
    );
    assert!(!b.root().exists(), "a refused join wrote into the root");
    // A key file anyone can read is refused before anything else.
    chmod(&key_b, 0o644);
    let (code, _, err) = args(&key_b, &cap);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("chmod 600"), "{err}");
}

/// **`aterm link broker --tcp` REFUSES WHAT IT MUST, BY NAME**: a
/// non-loopback bind without `--allow-remote` (and says why), no
/// `--secret-file` (a TCP broker is always guarded), plaintext TCP, `--unix`
/// without `--tcp`, and a key file other users can read.
#[cfg(feature = "sealed")]
#[test]
fn the_tcp_broker_refuses_a_remote_bind_an_unguarded_start_and_a_readable_key() {
    let h = Host::new("br", "host-a");
    let key = h.path("k");
    std::fs::write(&key, format!("{}\n", "ab".repeat(32))).expect("key");
    chmod(&key, 0o600);
    let secret = h.path("s");
    std::fs::write(&secret, [3u8; 32]).expect("secret");
    chmod(&secret, 0o600);
    let log = h.path("b.log");
    let run = |args: &[&str]| h.run(&[&["broker"][..], args].concat());

    let (code, _, err) = run(&[
        "--tcp",
        "0.0.0.0:1",
        "--key-file",
        &key,
        "--secret-file",
        &secret,
        &log,
    ]);
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("not a loopback address")
            && err.contains("--allow-remote")
            && err.contains("not a per-host identity"),
        "{err}"
    );
    let (code, _, err) = run(&["--tcp", "127.0.0.1:0", "--key-file", &key, &log]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("always GUARDED"), "{err}");
    let (code, _, err) = run(&["--tcp", "127.0.0.1:0", "--secret-file", &secret, &log]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("only SEALED"), "{err}");
    // `--unix` is the socket served BESIDE a port, never the listener itself.
    let (code, _, err) = run(&["--unix", &h.path("b.sock"), &log]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("--unix needs --tcp"), "{err}");
    chmod(&key, 0o644);
    let (code, _, err) = run(&[
        "--tcp",
        "127.0.0.1:0",
        "--key-file",
        &key,
        "--secret-file",
        &secret,
        &log,
    ]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("chmod 600"), "{err}");
    assert!(
        !Path::new(&log).exists(),
        "a refused start left a log behind"
    );
}

/// **A DEFAULT BUILD NAMES THE FEATURE** for every round-16 surface: `broker
/// --tcp`, `on --tcp`, `mint-for` and `join` — before anything is written.
/// (`mint-for` is the review's defect 7: it minted, into `--out`, while the
/// help and the changelog said a default build refuses every command.)
#[cfg(not(feature = "sealed"))]
#[test]
fn a_default_build_answers_every_sealed_surface_by_naming_the_feature() {
    let h = Host::new("df", "host-a");
    let key = h.path("k");
    for args in [
        vec![
            "broker",
            "--tcp",
            "127.0.0.1:7000",
            "--key-file",
            &key,
            "--secret-file",
            &key,
            "l",
        ],
        vec![
            "fabric",
            "on",
            "--tcp",
            "127.0.0.1:7000",
            "--key-file",
            &key,
        ],
        vec![
            "fabric",
            "join",
            "--broker",
            "127.0.0.1:7000",
            "--tcp",
            "--key-file",
            &key,
            "--cap-file",
            &key,
        ],
        vec!["fabric", "mint-for", "new", "--out", &key],
    ] {
        let (code, out, err) = h.run(&args);
        assert_eq!(code, 2, "{args:?}: {out}{err}");
        assert!(
            err.contains("the sealed TCP transport is not in this build")
                && err.contains("`sealed` cargo feature"),
            "{args:?}: {err}"
        );
    }
    assert!(!h.root().exists() && !Path::new(&key).exists());
    // With a mint secret there to mint under, too.
    std::fs::create_dir_all(h.root()).expect("root");
    let secret = h.root().join("mint.secret");
    std::fs::write(&secret, [7u8; 32]).expect("secret");
    chmod(&secret.to_string_lossy(), 0o600);
    let (code, _, err) = h.run(&["fabric", "mint-for", "new", "--out", &key]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("`sealed` cargo feature"), "{err}");
    assert!(!Path::new(&key).exists(), "a default build minted a cap");
    let (_, _, usage) = h.run(&[]);
    assert!(
        usage.contains("this build: no sealed TCP transport"),
        "{usage}"
    );
}

// ---------------------------------------------------------------------------
// The round-16 review: every defect it measured, as the test that failed.
// ---------------------------------------------------------------------------

/// Host `a`'s first host: an instance, `on --tcp` (exit 1: no broker yet),
/// the broker `on` printed, then `on` again (exit 0). Returns the instance,
/// its session, the broker and the argv it was started with.
#[cfg(feature = "sealed")]
fn first_host(a: &Host, bind: &str, key: &str) -> (Gui, String, Proc, String) {
    let gui = Gui::boot(a);
    let sid = gui.session();
    let on = || {
        a.run(&[
            "fabric",
            "on",
            "--tcp",
            bind,
            "--key-file",
            key,
            "--service",
            "none",
        ])
    };
    let (code, out, err) = on();
    assert_eq!(code, 1, "{out}{err}");
    let printed = format!(
        "{BIN} broker --tcp {bind} --key-file {key} --secret-file {} --unix {} {}",
        a.root().join("mint.secret").display(),
        a.root().join("bus.sock").display(),
        a.root().join("bus.log").display()
    );
    assert!(out.contains(&printed), "{out}");
    let broker = spawn_printed_broker(&printed);
    let (code, out, err) = on();
    assert_eq!(code, 0, "{out}{err}");
    (gui, sid, broker, printed)
}

/// `join` on `b` with copies of `key` and `cap` in its inbox (0600), accepting
/// tasks from `accept`, `--service none`, plus `extra`.
#[cfg(feature = "sealed")]
fn join_with(
    b: &Host,
    bind: &str,
    key: &str,
    cap: &str,
    accept: &str,
    extra: &[&str],
) -> (i32, String, String) {
    let key_b = b.path("inbox/fleet.key");
    let cap_b = b.path("inbox/node.cap");
    let _ = std::fs::remove_file(&key_b);
    let _ = std::fs::remove_file(&cap_b);
    std::fs::copy(key, &key_b).expect("copy key");
    std::fs::copy(cap, &cap_b).expect("copy cap");
    chmod(&key_b, 0o600);
    chmod(&cap_b, 0o600);
    let mut args = vec![
        "fabric",
        "join",
        "--broker",
        bind,
        "--tcp",
        "--key-file",
        &key_b,
        "--cap-file",
        &cap_b,
        "--accept-from",
        accept,
        "--service",
        "none",
    ];
    args.extend_from_slice(extra);
    b.run(&args)
}

/// A sealed connection to the broker at `bind` with `cap`'s grants attached.
#[cfg(feature = "sealed")]
fn sealed_conn(key: &str, bind: &str, cap: &str) -> aterm_link::transport::Conn {
    let k = aterm_link::transport::read_key_file(key).expect("key");
    let (mut conn, closer) = aterm_link::transport::connect(
        &aterm_link::transport::Transport::Sealed(Box::new(k)),
        bind,
    )
    .expect("sealed connect");
    std::mem::forget(closer);
    for c in aterm_link::bridge::read_cap_file(cap).expect("cap") {
        conn.attach(&c.grant, &c.tag).expect("attach");
    }
    conn
}

/// The line of `nodes` (a report's NODES section) for node `n`.
#[cfg(feature = "sealed")]
fn nodes_of(report: &str) -> String {
    report
        .split("\nNODES")
        .nth(1)
        .and_then(|t| t.split("\nBRIDGES").next())
        .unwrap_or("")
        .to_string()
}

/// **REVIEW 1: A NODE THAT IS LIVE IS NOBODY ELSE'S.** The same cap joined
/// from a SECOND root used to be accepted (`proof ok`): the two bridges then
/// shared the node's lane — the second one published `undeliverable
/// reason=not-hosted` for mail the first delivered, `host=` flipped between
/// them, and when the second quit its will marked the node `gone` under the
/// first, still running. `join` now reads the node's presence row in its
/// probe and refuses a live node this root has never been — exit 2, nothing
/// written. And the root that IS that node joins again as `already`, an
/// identical key at the wrong mode made 0600 on the way (the `already` line
/// used to say `(0600)` of whatever mode it found).
#[cfg(feature = "sealed")]
#[test]
fn review_a_second_root_cannot_join_as_a_node_that_is_live() {
    let a = Host::new("xa", "host-a");
    let b1 = Host::new("xb1", "host-b");
    let b2 = Host::new("xb2", "host-c");
    let bind = format!("127.0.0.1:{}", free_port());
    let key = a.path("fleet.key");
    let (_gui_a, _sid_a, _broker, _) = first_host(&a, &bind, &key);
    let node_a = a.node();
    let cap = a.path("m7.cap");
    let (code, out, err) = a.run(&["fabric", "mint-for", "new", "--out", &cap]);
    assert_eq!(code, 0, "{out}{err}");
    let gui_b1 = Gui::boot(&b1);
    let _sid_b1 = gui_b1.session();
    let (code, out, err) = join_with(&b1, &bind, &key, &cap, &node_a, &[]);
    assert_eq!(code, 0, "the first join:\n{out}{err}");
    let node_b = b1.node();
    harness::until("the joined node live on the bus", || {
        let (_, text, _) = a.run(&["fabric"]);
        nodes_of(&text)
            .lines()
            .any(|l| l.contains(&node_b) && l.contains("state=live"))
            .then_some(())
    });

    // The second root, with the SAME cap.
    let gui_b2 = Gui::boot(&b2);
    let _sid_b2 = gui_b2.session();
    let config_before = std::fs::read(b2.path("cfg/aterm/aterm.toml")).ok();
    let (code, out, err) = join_with(&b2, &bind, &key, &cap, &node_a, &[]);
    assert_eq!(code, 2, "a second root joined as a live node:\n{out}{err}");
    assert!(
        step(&out, "node")
            .iter()
            .any(|l| l.contains("FAILED") && l.contains(&format!("{node_b} is LIVE"))),
        "{out}"
    );
    assert!(out.contains("host=host-b"), "names who has it: {out}");
    assert!(
        !b2.root().join("link-state/node").exists() && !b2.root().join("node.cap").exists(),
        "a refused join wrote into the root"
    );
    assert_eq!(
        std::fs::read(b2.path("cfg/aterm/aterm.toml")).ok(),
        config_before,
        "a refused join wrote the config"
    );
    let st = gui_b1.verb("fabric status");
    assert_eq!(
        kv(st.header(), "state"),
        Some("connected"),
        "{}",
        st.header()
    );
    let (_, text, _) = a.run(&["fabric"]);
    assert!(
        nodes_of(&text)
            .lines()
            .any(|l| l.contains(&node_b) && l.contains("host=host-b state=live")),
        "the node is still the first root's:\n{text}"
    );

    // The root that IS the node joins again: `already`, and an identical key
    // at 0644 is made 0600 and said so.
    chmod(&b1.root().join("fleet.key").to_string_lossy(), 0o644);
    let (code, out, err) = join_with(&b1, &bind, &key, &cap, &node_a, &[]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        step(&out, "key")
            .iter()
            .any(|l| l.contains("made 0600") && l.contains("mode 644")),
        "{out}"
    );
    assert_eq!(mode_of(&b1.root().join("fleet.key")), 0o600);
    assert!(verdict(&out, "node").contains(&"already"), "{out}");
}

/// **REVIEW 2: A RECEIPT IS THE RECIPIENT'S, AND NO OTHER NODE'S.** A third
/// node's cap, minted on host A like any joining host's, grants it every
/// fleet lane AS ITSELF — so it could publish `ack re=<off>
/// verdict=handled` onto A's session lane for a task A sent to B, and A's
/// `post --wait-ack` answered `ack=handled` while B's inbox still held the
/// task (`OK 2 off=13 ack=handled msg=2`, measured). Now A's bridge knows
/// where the task went: the stray ack is delivered as what it is, `kind=note
/// demoted=ack`, the wait stays parked, and B's real receipt settles it. And
/// a stray ack settles no DEADLINE either — delivery and the bus sweep agree
/// — so a task nobody answers still expires.
#[cfg(feature = "sealed")]
#[test]
fn review_a_receipt_counts_only_from_the_node_the_task_went_to() {
    let a = Host::new("ya", "host-a");
    let b = Host::new("yb", "host-b");
    let bind = format!("127.0.0.1:{}", free_port());
    let key = a.path("fleet.key");
    let (gui_a, sid_a, _broker, _) = first_host(&a, &bind, &key);
    let node_a = a.node();
    let cap_b = a.path("b.cap");
    let (code, out, err) = a.run(&["fabric", "mint-for", "new", "--out", &cap_b]);
    assert_eq!(code, 0, "{out}{err}");
    let gui_b = Gui::boot(&b);
    let sid_b = gui_b.session();
    let (code, out, err) = join_with(&b, &bind, &key, &cap_b, &node_a, &[]);
    assert_eq!(code, 0, "{out}{err}");
    let node_b = b.node();
    // A THIRD node's cap, and a connection holding it: the forger.
    let cap_c = a.path("c.cap");
    let (code, out, err) = a.run(&["fabric", "mint-for", "new", "--out", &cap_c]);
    assert_eq!(code, 0, "{out}{err}");
    let grants: Vec<String> = aterm_link::bridge::read_cap_file(&cap_c)
        .expect("cap")
        .into_iter()
        .map(|c| c.grant)
        .collect();
    let (_, node_c) = aterm_link::join::node_ring_of(&grants).expect("ring");
    let mut forger = sealed_conn(&key, &bind, &cap_c);
    let pid = astream_cap::producer_id_of(&node_c);
    let mut seq = 1_000_000u64;
    let mut forge = |re: u64| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let mut body = aterm_link::body::Body::new(now);
        body.re = Some(re);
        body.from = Some("s-forged".to_string());
        body.verdict = Some("handled".to_string());
        body.text = format!("ack re={re} verdict=handled");
        seq += 1;
        forger
            .publish(
                pid,
                seq,
                &format!("/f/local/in/{node_a}/{sid_a}/{node_c}/ack"),
                &body.encode(None),
            )
            .expect("the forged ack is on the bus: the grant allows it");
    };
    let task_off = |text: &str| -> u64 {
        harness::until("the task in B's inbox", || {
            gui_b
                .verb(&format!("@{sid_b} inbox --peek"))
                .rows()
                .iter()
                .find(|r| kv(r, "kind") == Some("task") && r.contains(text))
                .and_then(|r| kv(r, "off")?.parse().ok())
        })
    };
    let post = |line: String| {
        let sock = gui_a.ctl_sock.clone();
        let token = gui_a.token.clone();
        std::thread::spawn(move || {
            let mut ctl = Ctl::connect(&sock, &token).expect("ctl");
            let _ = ctl
                .get_ref()
                .set_read_timeout(Some(Duration::from_secs(90)));
            ctl.request(&line).expect("post").header().to_string()
        })
    };

    // 1. The stray receipt settles nothing; the recipient's does.
    let waiting = post(format!(
        "@{sid_a} post to=@{sid_b}@{node_b} kind=task dl=60000 --wait-ack=60000 first"
    ));
    let off = task_off("first");
    forge(off);
    let stray = harness::until("the stray ack in A's inbox", || {
        gui_a
            .verb(&format!("@{sid_a} inbox --peek"))
            .rows()
            .iter()
            .find(|r| kv(r, "re") == Some(off.to_string().as_str()))
            .cloned()
    });
    assert!(
        kv(&stray, "kind") == Some("note")
            && kv(&stray, "demoted") == Some("ack")
            && kv(&stray, "from").is_some_and(|f| f.ends_with(&format!("@{node_c}"))),
        "{stray}"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !waiting.is_finished(),
        "{node_c}'s ack settled A's wait for B's receipt"
    );
    let row = harness::until("B's task row id", || {
        gui_b
            .verb(&format!("@{sid_b} inbox --peek"))
            .rows()
            .iter()
            .find(|r| kv(r, "off") == Some(off.to_string().as_str()))
            .and_then(|r| r.split_whitespace().nth(1).map(str::to_string))
    });
    let seen = gui_b.verb(&format!("@{sid_b} inbox seen {row} handled"));
    assert!(seen.ok(), "{}", seen.header());
    let header = waiting.join().expect("the post");
    assert_eq!(kv(&header, "ack"), Some("handled"), "{header}");
    let msg = kv(&header, "msg").expect("msg=").to_string();
    let receipt = gui_a
        .verb(&format!("@{sid_a} inbox --peek"))
        .rows()
        .iter()
        .find(|r| r.split_whitespace().nth(1) == Some(msg.as_str()))
        .cloned()
        .expect("the receipt row");
    assert!(
        kv(&receipt, "kind") == Some("ack")
            && kv(&receipt, "from") == Some(format!("{sid_b}@{node_b}").as_str()),
        "the wait was settled by B's receipt: {receipt}"
    );

    // 2. A stray ack is no answer to a DEADLINE: the task B never answers
    // expires, the stray on the bus notwithstanding.
    let waiting = post(format!(
        "@{sid_a} post to=@{sid_b}@{node_b} kind=task dl=1500 --wait-ack second"
    ));
    let off = task_off("second");
    forge(off);
    let header = waiting.join().expect("the post");
    assert!(
        header.starts_with("ERR expired "),
        "a stray ack settled the deadline: {header}"
    );
}

/// **REVIEW 3: A HOST THAT JOINED ANOTHER'S FLEET MINTS NOTHING.** Host B
/// ran a one-host `on` before (its own id, secret and cap), then joined host
/// A's fleet. `mint-for` on B minted under B's STALE secret and printed a
/// `join --broker <A's broker>` for a cap A's broker refuses (`capability
/// proof does not verify`). It is refused now, exit 2, naming the host to run
/// it on — and so is it on a joined host with no secret at all, where the old
/// advice (`aterm fabric on`) pointed back at a local socket.
#[cfg(feature = "sealed")]
#[test]
fn review_mint_for_refuses_on_a_host_that_joined_another_fleet() {
    let a = Host::new("za", "host-a");
    let b = Host::new("zb", "host-b");
    let c = Host::new("zc", "host-c");
    let bind = format!("127.0.0.1:{}", free_port());
    let key = a.path("fleet.key");
    let (_gui_a, _sid_a, _broker, _) = first_host(&a, &bind, &key);
    let node_a = a.node();
    // B's earlier one-host `on` (no broker runs: exit 1 after the identity).
    let (code, out, err) = b.run(&["fabric", "on", "--service", "none"]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(b.root().join("mint.secret").exists(), "{out}{err}");
    let node_b = b.node();
    let cap = a.path("b.cap");
    let (code, out, err) = a.run(&["fabric", "mint-for", &node_b, "--out", &cap]);
    assert_eq!(code, 0, "{out}{err}");
    let (code, out, err) = join_with(&b, &bind, &key, &cap, &node_a, &[]);
    assert_eq!(code, 0, "join:\n{out}{err}");
    let refused = |h: &Host| {
        let out_cap = h.path("minted.cap");
        let (code, out, err) = h.run(&["fabric", "mint-for", "new", "--out", &out_cap]);
        assert_eq!(code, 2, "a joined host minted:\n{out}{err}");
        assert!(
            err.contains("JOINED a fleet whose broker another host serves")
                && err.contains(&format!("it dials {bind}"))
                && !err.contains("`aterm fabric on`"),
            "{err}"
        );
        assert!(!Path::new(&out_cap).exists(), "a cap was written");
    };
    refused(&b);
    // C joined with no secret of its own.
    let cap_c = a.path("c.cap");
    let (code, out, err) = a.run(&["fabric", "mint-for", "new", "--out", &cap_c]);
    assert_eq!(code, 0, "{out}{err}");
    let (code, out, err) = join_with(&c, &bind, &key, &cap_c, &node_a, &[]);
    assert_eq!(code, 0, "join:\n{out}{err}");
    assert!(!c.root().join("mint.secret").exists());
    refused(&c);
    // The first host still mints.
    let (code, out, err) = a.run(&["fabric", "mint-for", "new"]);
    assert_eq!(code, 0, "{out}{err}");
}

/// **REVIEW 6: `(0600)` IS TRUE WHEN IT IS SAID.** `mint-for --out` over an
/// IDENTICAL cap another user could read printed `into … (0600)` and left it
/// 0644. It is made 0600 now, and stderr says it was readable. A symlink at
/// `--out` is refused: a cap is never written — or chmod'ed — through one.
#[cfg(feature = "sealed")]
#[test]
fn review_mint_for_never_claims_0600_of_a_file_that_is_not() {
    let a = Host::new("wa", "host-a");
    let bind = format!("127.0.0.1:{}", free_port());
    let key = a.path("fleet.key");
    let (_gui_a, _sid_a, _broker, _) = first_host(&a, &bind, &key);
    let cap = a.path("m7.cap");
    let (code, out, err) = a.run(&["fabric", "mint-for", "n-00000000000000aa", "--out", &cap]);
    assert_eq!(code, 0, "{out}{err}");
    chmod(&cap, 0o644);
    let (code, out, err) = a.run(&["fabric", "mint-for", "n-00000000000000aa", "--out", &cap]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains(&format!("into {cap} (0600)")), "{out}");
    assert_eq!(mode_of(Path::new(&cap)), 0o600, "said 0600 of a 0644 cap");
    assert!(
        err.contains("held this cap at mode 644, readable by other users"),
        "{err}"
    );
    // A symlink at --out, to a file of the operator's.
    let victim = a.path("victim.txt");
    std::fs::write(&victim, "the operator's own file\n").expect("victim");
    chmod(&victim, 0o644);
    let linked = a.path("linked.cap");
    std::os::unix::fs::symlink(&victim, &linked).expect("link");
    let (code, _, err) = a.run(&["fabric", "mint-for", "n-00000000000000bb", "--out", &linked]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("is not a regular file"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&victim).expect("victim"),
        "the operator's own file\n"
    );
    assert_eq!(mode_of(Path::new(&victim)), 0o644);
}

/// **REVIEW 5: A PEER WITHOUT THE KEY CANNOT LOCK HOST 1 OUT OF ITS OWN
/// BROKER.** astream admits 64 connections into the sealed handshake at once,
/// for up to 5 s each, and drops the rest at accept — so a peer WITHOUT the
/// key that opens 80 connections and never speaks keeps every new TCP
/// connection out while it keeps doing so (the cap is astream's; vendor code
/// is not edited here). Host 1's own bridges and `aterm fabric` were on that
/// port: its report read `reachable NO — Connection reset by peer`. They are
/// on the broker's socket now: while the port is held, host 1's report
/// reaches the broker and a fresh connection with its node cap — what a
/// relaunched bridge makes — attaches and reads. A joining host still waits
/// for the slots (the residual, documented), and `join` names the queue
/// instead of calling a right key wrong; a wrong key, which fails the key
/// confirmation rather than the accept, still says the key.
#[cfg(feature = "sealed")]
#[test]
fn review_join_tells_a_full_handshake_queue_from_a_wrong_key() {
    let a = Host::new("da", "host-a");
    let b = Host::new("db", "host-b");
    let bind = format!("127.0.0.1:{}", free_port());
    let key = a.path("fleet.key");
    let (_gui_a, _sid_a, _broker, _) = first_host(&a, &bind, &key);
    let node_a = a.node();
    let cap = a.path("b.cap");
    let (code, out, err) = a.run(&["fabric", "mint-for", "new", "--out", &cap]);
    assert_eq!(code, 0, "{out}{err}");
    let squatters: Vec<std::net::TcpStream> = (0..80)
        .filter_map(|_| std::net::TcpStream::connect(&bind).ok())
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    let held = Instant::now();
    let (code_a, report, _) = a.run(&["fabric"]);
    let mut own = aterm_link::transport::connect(
        &aterm_link::transport::Transport::Unix,
        &a.root().join("bus.sock").to_string_lossy(),
    )
    .expect("host 1's socket, while the port is held")
    .0;
    for c in aterm_link::bridge::read_cap_file(&a.root().join("node.cap").to_string_lossy())
        .expect("host 1's cap")
    {
        own.attach(&c.grant, &c.tag).expect("attach on the socket");
    }
    let read = own.fetch(0, &format!("/f/local/pub/{node_a}/>"), 1);
    let (code, out, err) = join_with(&b, &bind, &key, &cap, &node_a, &[]);
    let within = held.elapsed();
    drop(squatters);
    assert!(
        within < Duration::from_secs(4),
        "the checks ran past the slots' 5 s: {within:?}"
    );
    let broker = report
        .split("\nBROKER")
        .nth(1)
        .and_then(|t| t.split("\nNODES").next())
        .unwrap_or("");
    assert!(
        broker.contains("reachable  yes") && broker.contains(&format!("serves     {bind}")),
        "host 1's own report, while the port is held (exit {code_a}):{broker}"
    );
    assert!(read.is_ok(), "a fresh connection on the socket: {read:?}");
    assert_eq!(code, 1, "{out}{err}");
    assert!(
        out.contains("pre-authentication slots were full")
            && !out.contains("this key is not the broker's"),
        "{out}"
    );
    assert!(!b.root().exists(), "a refused join wrote into the root");
}

/// **THE REMOTE BROKER RESTARTS, AND NOTHING POSTED MEANWHILE IS LOST** (the
/// review's check that held, kept as a test). Both bridges read `stalled` at
/// once; a post made then answers `queued=1`; both reconnect and the queued
/// task is delivered; B's bridge SIGKILLed, a task posted while it is gone
/// lands and its relaunch delivers it.
#[cfg(feature = "sealed")]
#[test]
fn the_remote_broker_restarts_and_nothing_posted_meanwhile_is_lost() {
    let a = Host::new("ra", "host-a");
    let b = Host::new("rb", "host-b");
    let bind = format!("127.0.0.1:{}", free_port());
    let key = a.path("fleet.key");
    let (gui_a, sid_a, broker, printed) = first_host(&a, &bind, &key);
    let node_a = a.node();
    let cap_b = a.path("b.cap");
    let (code, out, err) = a.run(&["fabric", "mint-for", "new", "--out", &cap_b]);
    assert_eq!(code, 0, "{out}{err}");
    let gui_b = Gui::boot(&b);
    let sid_b = gui_b.session();
    let (code, out, err) = join_with(&b, &bind, &key, &cap_b, &node_a, &[]);
    assert_eq!(code, 0, "{out}{err}");
    let node_b = b.node();
    let state = |g: &Gui| {
        kv(g.verb("fabric status").header(), "state").map_or_else(String::new, str::to_string)
    };
    drop(broker);
    harness::until("A leaves connected", || {
        (state(&gui_a) != "connected").then_some(())
    });
    harness::until("B leaves connected", || {
        (state(&gui_b) != "connected").then_some(())
    });
    let queued = gui_a.verb(&format!(
        "@{sid_a} post to=@{sid_b}@{node_b} kind=task --wait=2000 posted while the broker was down"
    ));
    assert!(
        queued.header().contains("queued=1") || queued.ok(),
        "{}",
        queued.header()
    );
    let _broker2 = spawn_printed_broker(&printed);
    harness::until("A reconnects", || {
        (state(&gui_a) == "connected").then_some(())
    });
    harness::until("B reconnects", || {
        (state(&gui_b) == "connected").then_some(())
    });
    harness::until("the queued task in B's inbox", || {
        gui_b
            .verb(&format!("@{sid_b} inbox --peek"))
            .rows()
            .iter()
            .find(|r| kv(r, "kind") == Some("task"))
            .cloned()
    });
    let back = gui_b.verb(&format!(
        "@{sid_b} post to=@{sid_a}@{node_a} kind=report --wait back after the restart"
    ));
    assert!(back.ok(), "{}", back.header());
    let pid: i32 = std::fs::read_to_string(b.root().join("link-state/pid"))
        .expect("pid")
        .trim()
        .parse()
        .expect("pid");
    harness::kill(pid, 9);
    let gone = gui_a.verb(&format!(
        "@{sid_a} post to=@{sid_b} kind=task --wait=5000 while B's bridge was down"
    ));
    assert!(gone.ok(), "{}", gone.header());
    harness::until("the task in B's inbox after B's relaunch", || {
        gui_b
            .verb(&format!("@{sid_b} inbox --peek"))
            .rows()
            .iter()
            .find(|r| r.contains("while%20B"))
            .cloned()
    });
}
