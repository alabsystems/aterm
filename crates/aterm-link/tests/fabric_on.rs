// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric on|off|doctor` end to end — the real `aterm-link fabric on`
//! command line against a real broker, a real headless `aterm-gui` and the real
//! bridge it launches, every one of them in a scratch world of its own.
//!
//! SEALED OFF FROM THE MACHINE: its own `$HOME` (so the default root, the
//! LaunchAgents dir and the config path all land in scratch), its own
//! `$XDG_CONFIG_HOME` and `$XDG_RUNTIME_DIR` (so no live instance is dialed and
//! the rendezvous file is scratch), its own `$ATERM_FABRIC_HOME` (so the derived
//! launchd label can never be the live one — `enable::Paths::label`), and
//! `--service none` everywhere except the one test that drives a FAKE
//! `launchctl` on `$PATH`. No test here can reach the real launchd, the real
//! broker, or a real session. Full-App fixtures explicitly opt out of reroute,
//! automatic primer installation, updates, and machine-setting writes; scratch
//! HOME alone does not isolate macOS preferences.

mod harness;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use aterm_link::ctl::Ctl;
use aterm_link::fabric::kv;

const BIN: &str = env!("CARGO_BIN_EXE_aterm-link");

/// The scratch world. Short, under `/tmp`: the broker's socket path has to fit
/// `sun_path`, and a per-user `$TMPDIR` does not.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = PathBuf::from(format!("/tmp/atfo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["home", "cfg/aterm", "run", "root", "bin"] {
            std::fs::create_dir_all(dir.join(sub)).expect("scratch dir");
        }
        Self { dir }
    }

    fn path(&self, rel: &str) -> String {
        self.dir.join(rel).to_string_lossy().into_owned()
    }

    fn root(&self) -> PathBuf {
        self.dir.join("root")
    }

    fn sock(&self) -> String {
        self.path("root/bus.sock")
    }

    fn config(&self) -> PathBuf {
        self.dir.join("cfg/aterm/aterm.toml")
    }

    fn rendezvous(&self) -> PathBuf {
        self.dir.join("run/aterm/fabric.toml")
    }

    fn ctl_sock(&self) -> String {
        self.path("run/aterm/aterm.sock")
    }

    /// `aterm-link fabric <args>` in the sealed environment.
    fn fabric_cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        strip_aterm_env(&mut cmd);
        cmd.arg("fabric")
            .args(args)
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("cfg"))
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("ATERM_FABRIC_HOME", self.path("root"))
            .env("ATERM_BIN", BIN)
            .stdin(Stdio::null());
        cmd
    }

    fn fabric(&self, args: &[&str]) -> (i32, String, String) {
        run(&mut self.fabric_cmd(args))
    }

    /// `aterm-link <verb> <args>` in the same environment (the read-side verbs,
    /// whose flags the rendezvous file defaults).
    fn link(&self, args: &[&str]) -> (i32, String, String) {
        let mut cmd = Command::new(BIN);
        strip_aterm_env(&mut cmd);
        cmd.args(args)
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("cfg"))
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .stdin(Stdio::null());
        run(&mut cmd)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::env::var_os("ATERM_LINK_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn run(cmd: &mut Command) -> (i32, String, String) {
    let Output {
        status,
        stdout,
        stderr,
    } = cmd.output().expect("run aterm-link");
    (
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

/// `aterm-link broker <sock> <log>` — the UNGUARDED bus `on` deploys under
/// launchd, started here instead; killed on drop.
struct Broker {
    child: Child,
}

impl Broker {
    fn spawn(s: &Scratch) -> Self {
        use std::io::{BufRead, BufReader};
        let mut child = Command::new(BIN)
            .args(["broker", &s.sock(), &s.path("root/bus.log")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the broker");
        let mut first = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut first)
            .expect("the broker prints its readiness line");
        assert_eq!(first.trim(), format!("listening {}", s.sock()));
        Self { child }
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The caps `on` minted, checked by a GUARDED broker opened with the secret
/// `on` minted: every grant attaches (the tags are real HMACs over the real
/// secret), a `Last` walk of the fleet's presence answers, and a cap forged
/// under another secret is refused — so the check can tell a good cap from a
/// bad one.
fn guarded_broker_accepts_the_minted_caps(s: &Scratch, node: &str) {
    let secret = std::fs::read(s.root().join("mint.secret")).expect("the minted secret");
    assert_eq!(secret.len(), 32);
    let sock = s.path("root/guard.sock");
    let broker = astream_broker::Broker::open_guarded(s.path("root/guard.log"), secret)
        .expect("guarded broker");
    let handle = broker.serve(&sock).expect("serve");
    let caps = aterm_link::bridge::read_cap_file(&s.path("root/node.cap")).expect("cap file");
    assert_eq!(caps.len(), 8);
    let mut c = astream_broker::Client::connect(&sock).expect("connect");
    for cap in &caps {
        c.attach(&cap.grant, &cap.tag)
            .unwrap_or_else(|e| panic!("the guarded broker refused `{}`: {e}", cap.grant));
    }
    c.last("/f/local/pub/*/*/presence", "", 8)
        .expect("a Last walk under the node ring's `ro:/f/local/pub/>`");
    let forged = astream_cap::mint(
        b"not-the-secret-not-the-secret-00",
        &format!("ro:/f/local/in/{node}/>"),
    )
    .expect("mint under another secret");
    let mut d = astream_broker::Client::connect(&sock).expect("connect");
    assert!(
        d.attach(&forged.filter, &forged.tag).is_err(),
        "a cap minted under another secret must be refused, or this check proves nothing"
    );
    drop(handle);
    drop(broker);
}

/// Every inherited `ATERM_*` goes — the harness's rule, for the harness's
/// reason: a test run from inside an aterm would otherwise hand its own session
/// id to the gui it spawns, and `on`'s proof would pick the CALLER's session.
fn strip_aterm_env(cmd: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("ATERM_") {
            cmd.env_remove(&name);
        }
    }
}

/// A headless aterm with NO fabric command — the instance `on` has to arm.
struct Gui {
    child: Child,
    ctl_sock: String,
    token: String,
    state: PathBuf,
}

impl Gui {
    fn boot(s: &Scratch) -> Self {
        let _permit = harness::boot_permit();
        harness::prepare_fixture_config(&s.dir);
        let log = std::fs::File::create(s.dir.join("gui.log")).expect("gui log");
        let err = log.try_clone().expect("gui log clone");
        let mut cmd = Command::new(harness::gui_binary());
        harness::prepare_gui_environment(&mut cmd);
        cmd.arg("--headless")
            .env("HOME", s.path("home"))
            .env("XDG_RUNTIME_DIR", s.path("run"))
            .env("XDG_CONFIG_HOME", s.path("cfg"))
            .env("SHELL", "/bin/sh")
            .env("ATERM_LINES", "40")
            .env("ATERM_COLUMNS", "120")
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err);
        let child = cmd.spawn().expect("launch aterm-gui --headless");
        let ctl_sock = s.ctl_sock();
        let token = harness::World::wait_for_token_at(&ctl_sock);
        Self {
            child,
            ctl_sock,
            token,
            state: s.root().join("link-state"),
        }
    }

    fn ctl(&self) -> Ctl {
        Ctl::connect(&self.ctl_sock, &self.token).expect("connect to aterm")
    }

    fn verb(&self, line: &str) -> aterm_link::ctl::Reply {
        self.ctl()
            .request(line)
            .unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    fn boot_session(&self) -> String {
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
        // The bridge aterm launched: its supervisor is gone with the gui, but
        // the child sees its fds close a moment later — kill it so no test
        // leaves a `serve` behind.
        if let Some(pid) = std::fs::read_to_string(self.state.join("pid"))
            .ok()
            .and_then(|p| p.trim().parse::<i32>().ok())
        {
            harness::kill(pid, 9);
        }
    }
}

/// The step lines named `<name>`, e.g. `node       already   n-…` — from the
/// STEPS block only: the `aterm fabric` status that follows a real run has a
/// `node` row of its own.
fn step<'a>(out: &'a str, name: &str) -> Vec<&'a str> {
    let steps = out
        .find("\naterm fabric — fleet")
        .map_or(out, |at| &out[..at]);
    steps
        .lines()
        .filter(|l| l.trim_start().starts_with(name) && l.len() > name.len() + 2)
        .filter(|l| {
            l.trim_start()[name.len()..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
        })
        .collect()
}

fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777
}

/// **THE WHOLE THING, END TO END, TWICE.** A guarded broker with the test's
/// secret, a headless aterm with no fabric command; `on` provisions, mints,
/// writes the config and the rendezvous file, ARMS the instance, and PROVES the
/// round trip; a second `on` changes nothing and says so; the read-side verbs
/// then need no flags; `off` removes what changes behaviour and keeps the
/// identity.
#[test]
fn on_provisions_arms_proves_and_a_second_on_changes_nothing() {
    let s = Scratch::new("on");
    let broker = Broker::spawn(&s);
    let gui = Gui::boot(&s);
    let sid = gui.boot_session();
    let status = gui.verb("fabric status");
    assert!(
        kv(status.header(), "supervised") == Some("0"),
        "the instance starts unarmed: {}",
        status.header()
    );

    // ---- the first `on`
    let (code, out, err) = s.fabric(&["on", "--service", "none"]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert!(err.is_empty(), "nothing on stderr: {err}");
    for (name, verdict) in [
        ("binary", "ok"),
        ("root", "done"),
        ("node", "done"),
        ("secret", "done"),
        ("cap", "done"),
        ("broker", "already"),
        ("config", "done"),
        ("rendezvous", "done"),
        ("instance", "done"),
        ("proof", "ok"),
    ] {
        let lines = step(&out, name);
        assert!(
            lines
                .iter()
                .any(|l| l.split_whitespace().nth(1) == Some(verdict)),
            "step `{name}` should read `{verdict}`:\n{out}"
        );
    }
    // The node id was provisioned into the state dir, and the cap minted FOR IT
    // with the test's secret — which the guarded broker verified when the
    // bridge attached (the proof could not have landed otherwise).
    let node = std::fs::read_to_string(s.root().join("link-state/node")).expect("node");
    let node = node.trim().to_string();
    assert!(node.starts_with("n-") && node.len() == 18, "{node}");
    let cap = std::fs::read_to_string(s.root().join("node.cap")).expect("cap");
    assert_eq!(cap.lines().count(), 8, "{cap}");
    assert_eq!(
        cap.lines()
            .filter(|l| l.contains(&format!("p={node}:")))
            .count(),
        4,
        "{cap}"
    );
    assert_eq!(mode_of(&s.root().join("node.cap")), 0o600);
    assert_eq!(mode_of(&s.root().join("mint.secret")), 0o600);
    assert_eq!(mode_of(&s.root()), 0o700);
    guarded_broker_accepts_the_minted_caps(&s, &node);
    // The config carries the bridge command, in the shim's spelling, with
    // --accept-from.
    let config = std::fs::read_to_string(s.config()).expect("config");
    let command = aterm_link::fabric::command_in_toml(&config)
        .expect("valid TOML")
        .expect("a [fabric] command");
    assert_eq!(
        command,
        format!(
            "{BIN} serve --fleet local --broker {} --cap-file {} --state {} --accept-from {node}",
            s.sock(),
            s.path("root/node.cap"),
            s.path("root/link-state")
        )
    );
    assert!(
        config.contains("# Written by `aterm fabric on`"),
        "{config}"
    );
    let baseline_backup = std::fs::read_to_string(s.dir.join("cfg/aterm/aterm.toml.bak"))
        .expect("the initial isolation config was backed up");
    assert_eq!(baseline_backup, harness::FIXTURE_GUI_CONFIG);
    // The rendezvous file, 0600, beside the control socket.
    let r = std::fs::read_to_string(s.rendezvous()).expect("fabric.toml");
    assert_eq!(mode_of(&s.rendezvous()), 0o600);
    let r = aterm_link::enable::Rendezvous::parse(&r).expect("parses");
    assert_eq!(r.fleet, "local");
    assert_eq!(r.broker, s.sock());
    assert_eq!(r.node, node);
    assert_eq!(r.cap_file, s.path("root/node.cap"));
    assert_eq!(r.state, s.path("root/link-state"));
    assert_eq!(r.command, command);
    // The instance is ARMED with exactly that command, and connected.
    let status = gui.verb("fabric status");
    let h = status.header();
    assert_eq!(kv(h, "supervised"), Some("1"), "{h}");
    assert_eq!(kv(h, "state"), Some("connected"), "{h}");
    assert_eq!(
        kv(h, "command").map(aterm_link::pct::decode).as_deref(),
        Some(command.as_str()),
        "{h}"
    );
    // The proof's note is in the session's inbox, delivered THROUGH the broker
    // (it carries a bus offset) — and the report line names the round trip.
    let proof = step(&out, "proof")
        .into_iter()
        .next()
        .expect("a proof line");
    assert!(
        proof.contains(&format!("a note from @{sid} to itself landed at @"))
            && proof.contains("came back through the broker in")
            && proof.contains(" ms"),
        "{proof}"
    );
    let inbox = gui.verb(&format!("@{sid} inbox --peek"));
    let notes: Vec<&String> = inbox
        .rows()
        .iter()
        .filter(|r| r.starts_with("msg ") && kv(r, "kind") == Some("note"))
        .collect();
    assert_eq!(notes.len(), 1, "{:#?}", inbox.rows());
    assert!(
        notes[0].contains("text=aterm%20fabric%20on:%20proof%20"),
        "{}",
        notes[0]
    );
    assert!(kv(notes[0], "off").is_some_and(|o| o.parse::<u64>().is_ok()));
    // The undo is spelled WITH the environment that scoped this run — the
    // scratch root and config — so running it as printed reaches this
    // scratch install and never the machine's.
    let undo = step(&out, "undo");
    assert_eq!(undo.len(), 1, "{out}");
    assert!(
        undo[0].contains(&format!(
            "ATERM_FABRIC_HOME={} XDG_CONFIG_HOME={} XDG_RUNTIME_DIR={} ATERM_BIN={BIN} HOME={} \
             aterm-link fabric off",
            s.path("root"),
            s.path("cfg"),
            s.path("run"),
            s.path("home")
        )),
        "{}",
        undo[0]
    );
    // And the status report followed, with the instance on it.
    assert!(
        out.contains("\nCONFIG\n") && out.contains("\nBRIDGES"),
        "{out}"
    );
    // The FABRIC cell carries the bridge's own link report since round 13:
    // the last acked round trip and its age, then the SUPERVISED column.
    assert!(
        out.contains(&format!("{}  connected (rtt ", gui.child.id())) && out.contains(" ago)  yes"),
        "{out}"
    );

    // ---- the second `on`: idempotent, says so, proves again
    let before = std::fs::read(s.config()).expect("config bytes");
    let rendezvous_before = std::fs::read(s.rendezvous()).expect("fabric.toml bytes");
    let (code, out2, err) = s.fabric(&["on", "--service", "none"]);
    assert_eq!(code, 0, "stdout:\n{out2}\nstderr:\n{err}");
    for name in [
        "root",
        "node",
        "secret",
        "cap",
        "broker",
        "config",
        "rendezvous",
        "instance",
    ] {
        let lines = step(&out2, name);
        assert!(
            lines
                .iter()
                .all(|l| l.split_whitespace().nth(1) == Some("already")),
            "step `{name}` must read `already` on the second run:\n{out2}"
        );
    }
    assert!(
        out2.contains("nothing changed: the fabric was already on, and the proof ran again."),
        "{out2}"
    );
    assert!(
        step(&out2, "proof")[0].contains("came back through the broker"),
        "{out2}"
    );
    assert_eq!(
        std::fs::read(s.config()).expect("config"),
        before,
        "the config was rewritten"
    );
    assert_eq!(
        std::fs::read(s.rendezvous()).expect("fabric.toml"),
        rendezvous_before
    );
    assert_eq!(
        std::fs::read_to_string(s.dir.join("cfg/aterm/aterm.toml.bak")).expect("baseline backup"),
        baseline_backup,
        "an idempotent on must not overwrite the previous backup"
    );
    assert_eq!(
        std::fs::read_to_string(s.root().join("node.cap")).expect("cap"),
        cap,
        "the cap was re-minted"
    );

    // ---- the read-side verbs need no flags now
    let (code, ls, err) = s.link(&["ls"]);
    assert_eq!(code, 0, "{ls}\n{err}");
    assert!(
        ls.lines()
            .any(|l| l.starts_with(&format!("{node} ")) && l.contains("state=live")),
        "`ls` with no flags reads the rendezvous file and finds this node:\n{ls}"
    );
    assert!(
        ls.lines().any(|l| l.contains(&format!(" {sid} "))),
        "and the session's presence row:\n{ls}"
    );
    let (code, glance, err) = s.link(&["glance"]);
    assert_eq!(code, 0, "{glance}\n{err}");
    assert_eq!(
        glance.trim(),
        s.path("root/link-state/fabric/glance.json"),
        "glance writes into the rendezvous file's state dir"
    );
    assert!(Path::new(glance.trim()).exists());
    // A flag on the command line still wins: a wrong broker is a wrong broker.
    let (code, _, err) = s.link(&["ls", "--broker", "/tmp/atfo-nowhere.sock"]);
    assert_ne!(code, 0, "{err}");
    // `aterm fabric` (status) without [fabric] reads the rendezvous file.
    std::fs::write(
        s.config(),
        format!("font_px = 12.0\n{}", harness::FIXTURE_GUI_CONFIG),
    )
    .expect("drop only the Fabric table, retaining fixture opt-outs");
    let (code, status, _) = s.fabric(&["status"]);
    assert!(
        status.contains("source     the rendezvous file") && status.contains("\nCONFIG\n"),
        "exit {code}:\n{status}"
    );
    let (code, doctor, _) = s.fabric(&["doctor"]);
    assert!(
        doctor.contains("aterm fabric doctor — fleet local"),
        "exit {code}:\n{doctor}"
    );

    // ---- off
    let (code, out, err) = s.fabric(&["off", "--service", "none", "--dry-run"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(s.rendezvous().exists(), "a dry run removes nothing");
    let (code, out, err) = s.fabric(&["off", "--service", "none"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(!s.rendezvous().exists(), "off removes the rendezvous file");
    assert!(
        step(&out, "config")
            .iter()
            .any(|l| l.contains("no [fabric] table")),
        "{out}"
    );
    assert!(
        out.contains(&format!("{} stays", s.path("root/bus.log"))),
        "{out}"
    );
    assert!(out.contains(&format!("node id {node}")), "{out}");
    for kept in ["link-state/node", "mint.secret", "node.cap"] {
        assert!(s.root().join(kept).exists(), "off keeps {kept}");
    }
    // `off` on a config that HAS the table: the table goes, the rest stays, a
    // .bak holds the previous file.
    std::fs::write(
        s.config(),
        format!(
            "font_px = 12.0\n{}\n[fabric]\ncommand = \"{command}\"\n\n[keys]\nx = 1\n",
            harness::FIXTURE_GUI_CONFIG
        ),
    )
    .expect("config");
    let (code, out, _) = s.fabric(&["off", "--service", "none"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        std::fs::read_to_string(s.config()).expect("config"),
        format!(
            "font_px = 12.0\n{}\n[keys]\nx = 1\n",
            harness::FIXTURE_GUI_CONFIG
        )
    );
    assert!(s.dir.join("cfg/aterm/aterm.toml.bak").exists());
    // `doctor` after `off`, with the instance still armed (a supervisor has no
    // stop handle): the report reads that running bridge, and warns that the
    // Fabric config and the rendezvous file are gone — each with `on` as the fix.
    let (code, doctor, _) = s.fabric(&["doctor"]);
    assert_eq!(code, 1, "{doctor}");
    assert!(
        doctor.contains("no [fabric] command in")
            && doctor.contains("fix: `aterm fabric on` writes [fabric] command")
            && doctor.contains("no rendezvous file at")
            && doctor.contains("fix: `aterm fabric on` writes it"),
        "{doctor}"
    );

    drop(gui);
    drop(broker);
}

/// **`--dry-run` PRINTS EVERY STEP AND TOUCHES NOTHING**: on a bare world with
/// no broker, every step is `would`, and afterwards there is no root, no
/// config, no rendezvous file.
#[test]
fn dry_run_names_every_step_and_creates_nothing() {
    let s = Scratch::new("dry");
    std::fs::remove_dir_all(s.root()).expect("no root yet");
    let (code, out, err) = s.fabric(&["on", "--dry-run", "--service", "none"]);
    assert_eq!(
        code, 1,
        "a dry run that would fail says so with its exit:\n{out}\n{err}"
    );
    assert!(
        out.contains("A step FAILED above, and would fail for real."),
        "{out}"
    );
    assert!(
        out.contains("DRY RUN: every step printed, nothing touched"),
        "{out}"
    );
    for name in ["root", "node", "secret", "cap", "config", "rendezvous"] {
        assert!(
            step(&out, name)
                .iter()
                .any(|l| l.split_whitespace().nth(1) == Some("would")),
            "step `{name}` should read `would`:\n{out}"
        );
    }
    // With --service none the broker is not started by anybody, so the dry run
    // reports the socket nobody answers on — and still creates nothing.
    assert!(
        step(&out, "broker")
            .iter()
            .any(|l| l.contains("no broker answers on")),
        "{out}"
    );
    assert!(!s.root().exists(), "a dry run created the root");
    assert!(!s.config().exists(), "a dry run wrote the config");
    assert!(
        !s.rendezvous().exists(),
        "a dry run wrote the rendezvous file"
    );
    assert!(
        !s.dir.join("home/Library").exists(),
        "a dry run wrote under $HOME"
    );
}

/// **A REJECTED MINT PUBLISHES NOTHING AND SUPERVISES NOTHING** — the shell
/// fixture's containment check, on the Rust command, with its effect trace run
/// through `aterm-spec`'s capability-publication model (Tier-1 of
/// `fabric_capability_publication_model`, eight grants).
///
/// A refusal at grant 1, 3 and 8, over an absent cap and over a STALE one (a
/// cap for another node id): the previous cap is byte-identical or still
/// absent, no staging file is left, the config and the rendezvous file are not
/// written, exit 1. A short secret is refused before any mint and never
/// replaced. A cap already minted for this node is reused without a mint. And
/// the good case: eight `Mint`s, one `Publish`, then `Supervise`.
#[test]
fn a_rejected_mint_publishes_nothing_and_the_trace_satisfies_the_model() {
    let mut model = aterm_spec::derive::fabric_capability_publication_model();
    model
        .consts
        .iter_mut()
        .find(|(key, _)| *key == "Grants")
        .expect("the model has Grants")
        .1 = 8;
    let stale = |s: &Scratch, node: &str| {
        std::fs::write(
            s.root().join("node.cap"),
            format!("rw,p={node}:/f/local/pub/{node}/> {}\n", "00".repeat(32)),
        )
        .expect("stale cap");
    };
    let node_of = |s: &Scratch| -> String {
        std::fs::create_dir_all(s.root().join("link-state")).expect("state");
        std::fs::write(s.root().join("link-state/node"), "n-00000000000000aa\n").expect("node");
        "n-00000000000000aa".to_string()
    };
    let seed_secret = |s: &Scratch| {
        std::fs::write(
            s.root().join("mint.secret"),
            (0..32u8).map(|i| i.wrapping_mul(7)).collect::<Vec<u8>>(),
        )
        .expect("secret");
    };
    let secret_bytes = |s: &Scratch| std::fs::read(s.root().join("mint.secret")).ok();
    let cap_bytes = |s: &Scratch| std::fs::read(s.root().join("node.cap")).ok();
    let staging = |s: &Scratch| -> Vec<PathBuf> {
        std::fs::read_dir(s.root())
            .expect("root")
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(".node.cap."))
            })
            .collect()
    };
    let trace_of = |s: &Scratch| -> Vec<String> {
        std::fs::read_to_string(s.dir.join("effects.trace"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    };
    // Run the trace through the model; answers (rejected by the model, state).
    let check = |trace: &[String]| {
        let mut state = model.init_state();
        for event in trace {
            assert!(
                model.fire(event, &mut state),
                "the real command performed `{event}` from {state:?}, which the model disables"
            );
        }
        state
    };

    for existing in ["absent", "stale"] {
        for fail_at in ["1", "3", "8"] {
            let s = Scratch::new(&format!("mint-{existing}-{fail_at}"));
            node_of(&s);
            seed_secret(&s);
            if existing == "stale" {
                stale(&s, "n-previous");
            }
            let secret_before = secret_bytes(&s);
            let cap_before = cap_bytes(&s);
            let (code, out, _) = run(s
                .fabric_cmd(&["on", "--service", "none"])
                .env("ATERM_FABRIC_TRACE", s.path("effects.trace"))
                .env("ATERM_FABRIC_FAIL_MINT_AT", fail_at));
            assert_eq!(code, 1, "{existing}/{fail_at}:\n{out}");
            assert!(
                out.contains("injected mint refusal") && out.contains("was left unchanged"),
                "{out}"
            );
            assert_eq!(secret_bytes(&s), secret_before, "the secret was replaced");
            assert_eq!(
                cap_bytes(&s),
                cap_before,
                "the cap was touched ({existing})"
            );
            assert!(staging(&s).is_empty(), "staging debris: {:?}", staging(&s));
            assert!(
                !s.config().exists(),
                "the config was written after a refused mint"
            );
            assert!(!s.rendezvous().exists());
            assert!(
                step(&out, "broker").is_empty(),
                "supervision was reached:\n{out}"
            );
            let trace = trace_of(&s);
            let n: usize = fail_at.parse().expect("n");
            let mut expect = vec!["Mint".to_string(); n - 1];
            expect.push("Reject".to_string());
            assert_eq!(trace, expect, "{existing}/{fail_at}");
            let state = check(&trace);
            assert_eq!(state["refused"], 1);
            assert_eq!(state["published"], 0);
            assert_eq!(state["supervised"], 0);
        }
    }

    // A short secret: refused before the first mint, never replaced.
    {
        let s = Scratch::new("mint-short");
        node_of(&s);
        std::fs::write(s.root().join("mint.secret"), "short\n").expect("secret");
        stale(&s, "n-previous");
        let cap_before = cap_bytes(&s);
        let (code, out, _) = run(s
            .fabric_cmd(&["on", "--service", "none"])
            .env("ATERM_FABRIC_TRACE", s.path("effects.trace")));
        assert_eq!(code, 2, "{out}");
        assert!(
            out.contains("holds 6 bytes; a mint secret is at least 32"),
            "{out}"
        );
        assert_eq!(
            std::fs::read(s.root().join("mint.secret")).expect("secret"),
            b"short\n"
        );
        assert_eq!(cap_bytes(&s), cap_before);
        assert!(trace_of(&s).is_empty(), "{:?}", trace_of(&s));
    }

    // The good case, over an absent and over a stale cap: eight mints, one
    // publish, then supervision — with a real broker answering, and no
    // instance to arm, exit 0 and the proof skipped by name.
    for existing in ["absent", "stale"] {
        let s = Scratch::new(&format!("mint-good-{existing}"));
        let node = node_of(&s);
        seed_secret(&s);
        if existing == "stale" {
            stale(&s, "n-previous");
        }
        let broker = Broker::spawn(&s);
        let (code, out, err) = run(s
            .fabric_cmd(&["on", "--service", "none"])
            .env("ATERM_FABRIC_TRACE", s.path("effects.trace")));
        assert_eq!(code, 0, "{out}\n{err}");
        let trace = trace_of(&s);
        assert_eq!(
            trace,
            ["Mint"; 8]
                .iter()
                .map(|s| (*s).to_string())
                .chain(["Publish".to_string(), "Supervise".to_string()])
                .collect::<Vec<_>>(),
            "{existing}"
        );
        let state = check(&trace);
        assert_eq!(state["completed"], 8);
        assert_eq!(state["published"], 1);
        assert_eq!(state["supervised"], 1);
        let cap = std::fs::read_to_string(s.root().join("node.cap")).expect("cap");
        assert_eq!(cap.lines().count(), 8);
        assert!(cap.lines().all(|l| l.contains("/f/local/")), "{cap}");
        assert!(cap.contains(&format!("p={node}:")));
        assert!(!cap.contains("n-previous"), "the stale cap survived: {cap}");
        assert!(staging(&s).is_empty());
        assert_eq!(mode_of(&s.root().join("node.cap")), 0o600);
        assert!(
            step(&out, "proof")
                .iter()
                .any(|l| l.contains("skipped: no armed instance")),
            "{out}"
        );
        assert!(
            step(&out, "instances")
                .iter()
                .any(|l| l.contains("none running")),
            "{out}"
        );
        // A cap already minted for this node is reused: no mint at all.
        std::fs::remove_file(s.dir.join("effects.trace")).expect("reset");
        let cap_before = cap_bytes(&s);
        let (code, out, _) = run(s
            .fabric_cmd(&["on", "--service", "none"])
            .env("ATERM_FABRIC_TRACE", s.path("effects.trace")));
        assert_eq!(code, 0, "{out}");
        assert_eq!(trace_of(&s), vec!["Supervise".to_string()]);
        assert_eq!(cap_bytes(&s), cap_before);
        assert!(
            step(&out, "cap")
                .iter()
                .any(|l| l.contains("already") && l.contains("8 grants for")),
            "{out}"
        );
        guarded_broker_accepts_the_minted_caps(&s, &node);
        drop(broker);
    }
}
/// **LAUNCHD IS ASKED THROUGH A DERIVED LABEL, THE PLIST IS THE SCRIPT'S, AND
/// A BROKER LAUNCHD DOES NOT MANAGE IS NEVER BOOTSTRAPPED OVER.** A FAKE
/// `launchctl` on `$PATH` that behaves like the real one where it matters:
/// `bootstrap` starts the broker the plist names and `list` shows it with its
/// pid until `bootout` kills it. First, the round-13 review's finding 4: with
/// a hand-run broker answering on the socket and no job by the label, `on
/// --service launchd` used to boot out nothing, bootstrap a second broker on
/// the same socket and log, and claim `launchd … pid 0` — now it names the
/// unmanaged broker's pid and stops. Then the install proper: the label is
/// derived from the root (never the live one), the plist lives INSIDE that
/// root (not in `$HOME/Library/LaunchAgents`: the root is not the default
/// one) and runs the broker directly, a second `on` reads `already` from
/// launchd's own pid, and `off` boots the job out.
#[test]
fn launchd_is_asked_through_a_derived_label_and_the_plist_is_the_scripts() {
    let s = Scratch::new("launchd");
    let calls = s.path("launchctl.calls");
    let pidfile = s.path("launchd.pid");
    let labelfile = s.path("launchd.label");
    std::fs::write(
        s.dir.join("bin/launchctl"),
        format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> '{calls}'\n\
             case \"$1\" in\n\
               list)\n\
                 if [ -f '{pidfile}' ] && kill -0 \"$(cat '{pidfile}')\" 2>/dev/null; then\n\
                   printf '%s\\t0\\t%s\\n' \"$(cat '{pidfile}')\" \"$(cat '{labelfile}')\"\n\
                 fi\n\
                 exit 0 ;;\n\
               bootstrap)\n\
                 basename \"$3\" .plist > '{labelfile}'\n\
                 rm -f '{sock}'\n\
                 nohup '{BIN}' broker '{sock}' '{log}' >/dev/null 2>&1 &\n\
                 printf '%s' \"$!\" > '{pidfile}'\n\
                 exit 0 ;;\n\
               bootout)\n\
                 if [ -f '{pidfile}' ]; then kill \"$(cat '{pidfile}')\" 2>/dev/null; rm -f '{pidfile}'; exit 0; fi\n\
                 exit 3 ;;\n\
             esac\n\
             exit 0\n",
            sock = s.sock(),
            log = s.path("root/bus.log"),
        ),
    )
    .expect("fake launchctl");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        s.dir.join("bin/launchctl"),
        PermissionsExt::from_mode(0o755),
    )
    .expect("chmod");
    let path = format!("{}:/usr/bin:/bin", s.path("bin"));
    // Whatever happens below, the broker the fake launchd started dies with
    // the test.
    struct Reap(String);
    impl Drop for Reap {
        fn drop(&mut self) {
            if let Some(pid) = std::fs::read_to_string(&self.0)
                .ok()
                .and_then(|p| p.trim().parse::<i32>().ok())
            {
                harness::kill(pid, 9);
            }
        }
    }
    let _reap = Reap(pidfile.clone());
    let root = s.path("root");
    let label = format!(
        "systems.alab.astream-broker.{}",
        aterm_link::enable::cksum(root.as_bytes())
    );
    let plist = s.root().join(format!("{label}.plist"));
    let launch_agents = s.dir.join("home/Library/LaunchAgents");
    let uid = String::from_utf8_lossy(&Command::new("id").arg("-u").output().expect("id").stdout)
        .trim()
        .to_string();

    // ---- an UNMANAGED broker answers on the socket: refused, not bootstrapped over
    let unmanaged = Broker::spawn(&s);
    let unmanaged_pid = unmanaged.child.id();
    let (code, out, err) = run(s
        .fabric_cmd(&["on", "--service", "launchd"])
        .env("PATH", &path));
    assert_eq!(code, 1, "{out}\n{err}");
    let broker_lines = step(&out, "broker");
    assert!(
        broker_lines.iter().any(|l| l.contains("FAILED")
            && l.contains("does not manage")
            && l.contains(&format!("pid {unmanaged_pid}"))
            && l.contains(&format!("launchd {label} is not loaded"))
            && l.contains("second broker")),
        "the unmanaged broker must be named by pid, and the step must fail:\n{out}"
    );
    assert!(
        !out.contains("pid 0"),
        "no broker is ever claimed as pid 0:\n{out}"
    );
    let recorded = std::fs::read_to_string(&calls).unwrap_or_default();
    assert!(
        !recorded.contains("bootstrap") && !recorded.contains("bootout"),
        "launchd must not be asked to start or stop anything over an unmanaged broker: {recorded}"
    );
    assert!(
        !plist.exists(),
        "no plist is written for a job that is not started"
    );
    assert!(!s.config().exists(), "the run stopped before the config");
    // The same run, dry: the step is named as a failure and nothing is touched.
    let (code, dry, _) = run(s
        .fabric_cmd(&["on", "--service", "launchd", "--dry-run"])
        .env("PATH", &path));
    assert_eq!(code, 1, "{dry}");
    assert!(
        step(&dry, "broker")
            .iter()
            .any(|l| l.contains("FAILED") && l.contains("does not manage")),
        "{dry}"
    );
    assert!(!plist.exists());
    drop(unmanaged);
    let _ = std::fs::remove_file(&calls);

    // ---- the install: bootstrap starts the broker, and `on` reads its pid
    let (code, out, err) = run(s
        .fabric_cmd(&["on", "--service", "launchd"])
        .env("PATH", &path));
    assert_eq!(code, 0, "{out}\n{err}");
    let text = std::fs::read_to_string(&plist).expect("the plist was written inside the root");
    assert!(
        !launch_agents.exists(),
        "a redirected root leaves nothing in $HOME/Library/LaunchAgents: {}",
        launch_agents.display()
    );
    assert!(
        text.contains(&format!("<key>Label</key><string>{label}</string>")),
        "{text}"
    );
    // The broker's words, one <string> each — no shell, no quoting.
    assert!(
        text.contains(&format!(
            "<string>{BIN}</string>\n    <string>broker</string>\n    <string>{sock}</string>\n    \
             <string>{root}/bus.log</string>",
            sock = s.sock()
        )),
        "{text}"
    );
    assert!(
        !text.contains("/bin/sh") && !text.contains("rm -f"),
        "{text}"
    );
    assert!(text.contains("<key>KeepAlive</key><true/>"), "{text}");
    let recorded = std::fs::read_to_string(&calls).expect("launchctl was called");
    assert!(
        recorded.contains(&format!("bootout gui/{uid}/{label}\n"))
            && recorded.contains(&format!("bootstrap gui/{uid} {}\n", plist.display())),
        "{recorded}"
    );
    assert!(
        !recorded.contains("systems.alab.astream-broker\n")
            && !recorded.contains("systems.alab.astream-broker.plist"),
        "the live label must never be named: {recorded}"
    );
    let managed_pid = std::fs::read_to_string(&pidfile)
        .expect("the fake launchd started the broker")
        .trim()
        .to_string();
    assert!(
        step(&out, "broker")
            .iter()
            .any(|l| l.contains("(not loaded)")),
        "{out}"
    );
    assert!(
        step(&out, "broker")
            .iter()
            .any(|l| l.contains(&format!("launchd {label} pid {managed_pid} answers on"))),
        "the pid printed is launchd's own, never 0:\n{out}"
    );

    // ---- a second `on`: the plist is `already`, and so is the job, by its pid
    std::fs::remove_file(&calls).expect("reset");
    let (code, out, _) = run(s
        .fabric_cmd(&["on", "--service", "launchd"])
        .env("PATH", &path));
    assert_eq!(code, 0, "{out}");
    assert!(
        step(&out, "plist").iter().any(|l| l.contains("already")),
        "{out}"
    );
    assert!(
        step(&out, "broker").iter().any(|l| l.contains("already")
            && l.contains(&format!("launchd {label} pid {managed_pid} answers on"))),
        "{out}"
    );
    let recorded = std::fs::read_to_string(&calls).expect("launchctl was called");
    assert!(
        !recorded.contains("bootstrap") && !recorded.contains("bootout"),
        "a job that answers is left alone: {recorded}"
    );

    // ---- `off`: bootout and remove the plist; the broker is gone
    std::fs::remove_file(&calls).expect("reset");
    let (code, out, _) = run(s
        .fabric_cmd(&["off", "--service", "launchd"])
        .env("PATH", &path));
    assert_eq!(code, 0, "{out}");
    assert!(!plist.exists(), "off removes the plist");
    assert!(
        step(&out, "broker").iter().any(|l| l.contains("removed")),
        "{out}"
    );
    let recorded = std::fs::read_to_string(&calls).expect("launchctl was called");
    assert!(
        recorded.contains(&format!("bootout gui/{uid}/{label}\n")),
        "{recorded}"
    );
    assert!(
        !Path::new(&pidfile).exists(),
        "the fake launchd reaped the broker"
    );
    let pid: i32 = managed_pid.parse().expect("pid");
    let gone = (0..200).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(25));
        !harness::alive(pid)
    });
    assert!(gone, "the managed broker {pid} must be dead after off");
}

/// **REFUSALS AND HELP**: `--tcp --key-file` is refused by name BEFORE
/// anything is written — in a default build naming the `sealed` feature, in a
/// sealed one for a non-loopback bind without `--allow-remote` and for an
/// ephemeral port — a bad `--service` is a usage error, and `help` names every
/// subcommand.
#[test]
fn tcp_is_refused_by_name_and_help_names_every_verb() {
    let s = Scratch::new("usage");
    let key = s.path("fleet.key");
    if cfg!(feature = "sealed") {
        let (code, _, err) = s.fabric(&["on", "--tcp", "0.0.0.0:7000", "--key-file", &key]);
        assert_eq!(code, 2, "{err}");
        assert!(
            err.contains("not a loopback address") && err.contains("--allow-remote"),
            "{err}"
        );
        let (code, _, err) = s.fabric(&["on", "--tcp", "127.0.0.1:0", "--key-file", &key]);
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("FIXED port"), "{err}");
    } else {
        let (code, _, err) = s.fabric(&["on", "--tcp", "127.0.0.1:7000", "--key-file", &key]);
        assert_eq!(code, 2, "{err}");
        assert!(
            err.contains("the sealed TCP transport is not in this build")
                && err.contains("`sealed` cargo feature"),
            "{err}"
        );
    }
    let (code, _, err) = s.fabric(&["on", "--tcp", "127.0.0.1:7000"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("--tcp needs --key-file"), "{err}");
    assert!(
        !s.root().join("link-state").exists() && !Path::new(&key).exists(),
        "a refused flag provisioned something"
    );
    let (code, _, err) = s.fabric(&["on", "--service", "cron"]);
    assert_eq!(code, 2);
    assert!(
        err.contains("--service cron: launchd, systemd or none"),
        "{err}"
    );
    let (code, out, _) = s.fabric(&["help"]);
    assert_eq!(code, 0);
    for needle in [
        "aterm fabric on  [--dry-run]",
        "aterm fabric off [--dry-run]",
        "aterm fabric doctor",
        "--service <s>",
        "aterm fabric mint-for <node-id>|new",
        "aterm fabric join --broker <host:port> --tcp",
        "--allow-remote",
    ] {
        assert!(out.contains(needle), "help must name `{needle}`:\n{out}");
    }
    // `doctor` on a world with no fabric: exit 2 and the fix.
    let (code, out, _) = s.fabric(&["doctor"]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("fabric is off") && out.contains("fix: `aterm fabric on`"),
        "{out}"
    );
}

/// **`doctor` NAMES THE FIX** for each warning: a config pointing at a socket
/// nobody serves is a warning with the broker fix under it, and the missing
/// rendezvous file is the one check `status` does not make.
#[test]
fn doctor_puts_a_fix_under_every_warning() {
    let s = Scratch::new("doctor");
    std::fs::create_dir_all(s.root().join("link-state")).expect("state");
    std::fs::write(s.root().join("link-state/node"), "n-00000000000000aa\n").expect("node");
    std::fs::write(
        s.root().join("node.cap"),
        format!("ro:/f/local/> {}\n", "ab".repeat(32)),
    )
    .expect("cap");
    std::fs::write(
        s.config(),
        format!(
            "[fabric]\ncommand = \"{BIN} serve --fleet local --broker {} --cap-file {} --state {}\"\n",
            s.sock(),
            s.path("root/node.cap"),
            s.path("root/link-state")
        ),
    )
    .expect("config");
    let (code, out, _) = s.fabric(&["doctor"]);
    assert_eq!(code, 1, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let broker = lines
        .iter()
        .position(|l| l.contains("does not answer"))
        .expect("the dead broker is a warning");
    assert!(
        lines[broker + 1]
            .trim_start()
            .starts_with("fix: start the broker: `aterm fabric on`"),
        "{out}"
    );
    let rendezvous = lines
        .iter()
        .position(|l| l.contains("no rendezvous file"))
        .expect("the missing rendezvous file is a warning");
    assert!(
        lines[rendezvous + 1]
            .trim_start()
            .starts_with("fix: `aterm fabric on` writes it"),
        "{out}"
    );
    // Every `!` line has a `fix:` under it.
    for (i, l) in lines.iter().enumerate() {
        if l.trim_start().starts_with("! ") {
            assert!(
                lines
                    .get(i + 1)
                    .is_some_and(|f| f.trim_start().starts_with("fix: ")),
                "no fix under: {l}\n{out}"
            );
        }
    }
}

/// **`off` NAMES EVERY SESSION THE FLEET HOLDS** — the round-13 review's
/// finding 3. A fleet halt is published on the bus; the bridge holds the
/// session; the Owner's `hold <sid> off` is `ERR denied` (a fleet hold is the
/// bridge's alone); and `off` — dry and real — used to say nothing about it
/// while stopping the one thing that could ever lift it. Now it names the
/// session, says it is held, and says what lifts it.
#[test]
fn review_off_names_a_session_the_fleet_holds() {
    let s = Scratch::new("held");
    let broker = Broker::spawn(&s);
    let gui = Gui::boot(&s);
    let sid = gui.boot_session();
    let (code, out, err) = s.fabric(&["on", "--service", "none"]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");

    // A HUMAN HALTS THE FLEET. The broker `on` deploys is unguarded, so the
    // halt is published straight onto its fleet face.
    let mut human = astream_broker::Client::connect(s.sock()).expect("connect to the broker");
    let subject = "/f/local/fleet/h-review/halt";
    human
        .publish(
            astream_cap::producer_id_of("h-review"),
            1,
            subject,
            b"v=1 t=1 state=on reason=review%20halt",
        )
        .expect("publish the halt");
    let held_in = {
        let started = std::time::Instant::now();
        loop {
            let status = gui.verb(&format!("@{sid} status"));
            if status.header().contains(" hold=1 ") {
                break started.elapsed();
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(20),
                "the bridge never held the session: {}",
                status.header()
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    eprintln!(
        "MEASURED fleet halt -> status hold=1: {} ms",
        held_in.as_millis()
    );
    let denied = gui.verb(&format!("hold {sid} off"));
    assert_eq!(
        denied.header().trim(),
        "ERR denied",
        "a fleet hold is not the Owner's to lift"
    );

    // `off --dry-run` and `off` both name it, as HELD, and say what lifts it.
    for args in [
        &["off", "--service", "none", "--dry-run"][..],
        &["off", "--service", "none"][..],
    ] {
        let (code, out, err) = s.fabric(args);
        assert_eq!(code, 0, "{args:?}:\n{out}\n{err}");
        let held = step(&out, "held");
        assert!(
            held.iter().any(|l| l.contains("WARNING")
                && l.contains(&format!("@{sid}"))
                && l.contains("HELD")
                && l.contains("hold=1")
                && l.contains("off does not lift it")
                && l.contains("relaunch instance")),
            "{args:?} must name the held session and what lifts it:\n{out}"
        );
    }
    // And the hold is untouched: `off` lifted nothing.
    let status = gui.verb(&format!("@{sid} status"));
    assert!(status.header().contains(" hold=1 "), "{}", status.header());
    drop(gui);
    drop(broker);
}

/// **`off` ACTS ON THE ROOT THE FABRIC IS ON** — the round-13 review's
/// finding 6. `on` under root A writes `[fabric] command` dialing A's socket;
/// `off` under a different `$ATERM_FABRIC_HOME` (or none) used to resolve
/// root B from the environment, report B's job "not installed", and remove
/// the config — the operator told off, A's `KeepAlive` broker running on.
/// Now `off` follows the config's `--broker` to A, says so in its header,
/// and every line names A; with the config gone, the rendezvous file; with
/// both gone, the environment.
#[test]
fn review_off_acts_on_the_root_the_config_names() {
    let s = Scratch::new("offroot");
    let broker = Broker::spawn(&s);
    let (code, out, err) = s.fabric(&["on", "--service", "none"]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    let root_a = s.path("root");
    let root_b = s.path("rootB");
    std::fs::create_dir_all(&root_b).expect("root B");
    let node = std::fs::read_to_string(s.root().join("link-state/node"))
        .expect("node id")
        .trim()
        .to_string();

    // Under B's environment, and under none: the config's root, A.
    for env in [Some(root_b.as_str()), None] {
        let mut cmd = s.fabric_cmd(&["off", "--service", "none", "--dry-run"]);
        match env {
            Some(b) => {
                cmd.env("ATERM_FABRIC_HOME", b);
            }
            None => {
                cmd.env_remove("ATERM_FABRIC_HOME");
            }
        }
        let (code, out, err) = run(&mut cmd);
        assert_eq!(code, 0, "{env:?}:\n{out}\n{err}");
        assert!(
            out.starts_with(&format!(
                "aterm fabric off — root {root_a} (from [fabric] command)"
            )),
            "{env:?}: the header names the config's root:\n{out}"
        );
        assert!(
            out.contains(&format!("{root_a}/bus.log stays"))
                && out.contains(&format!("{root_a}: node id {node}")),
            "{env:?}: every line names A:\n{out}"
        );
        assert!(!out.contains(&root_b), "{env:?}: B is never named:\n{out}");
    }
    // The config's table gone: the rendezvous file's root, still A.
    std::fs::write(s.config(), "font_px = 12.0\n").expect("drop the table");
    let (code, out, _) = run(s
        .fabric_cmd(&["off", "--service", "none", "--dry-run"])
        .env("ATERM_FABRIC_HOME", &root_b));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.starts_with(&format!(
            "aterm fabric off — root {root_a} (from the rendezvous file)"
        )),
        "{out}"
    );
    // Both gone: the environment decides, and the header says so.
    std::fs::remove_file(s.rendezvous()).expect("drop the rendezvous file");
    let (code, out, _) = run(s
        .fabric_cmd(&["off", "--service", "none", "--dry-run"])
        .env("ATERM_FABRIC_HOME", &root_b));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.starts_with(&format!(
            "aterm fabric off — root {root_b} (from the environment)"
        )),
        "{out}"
    );
    drop(broker);
}
