// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric on|off|doctor` end to end — the real `aterm fabric on` command line against a real broker, a real headless `aterm` and the real bridge it launches, every one of them in a scratch world of its own.
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use crate::harness;
use harness::{mode_of, prepare_gui_environment};

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use aterm_link::ctl::Ctl;
use aterm_link::fabric::kv;

/// The scratch world, short and under `/tmp`: the broker's socket path has to fit
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

    /// `aterm fabric <args>` in the sealed environment.
    fn fabric_cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(harness::aterm_to_launch());
        prepare_gui_environment(&mut cmd);
        cmd.arg("fabric")
            .args(args)
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("cfg"))
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("ATERM_FABRIC_HOME", self.path("root"))
            .env("ATERM_BIN", harness::aterm())
            .stdin(Stdio::null());
        cmd
    }

    fn fabric(&self, args: &[&str]) -> (i32, String, String) {
        run(&mut self.fabric_cmd(args))
    }

    /// `aterm link <verb> <args>` in the same environment (the read-side verbs,
    /// whose flags the rendezvous file defaults).
    fn link(&self, args: &[&str]) -> (i32, String, String) {
        let mut cmd = Command::new(harness::aterm_to_launch());
        prepare_gui_environment(&mut cmd);
        cmd.arg("link")
            .args(args)
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
    } = harness::output_within(cmd, "aterm");
    (
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// `<root>/mint.secret` holding `bytes` at `mode`.
fn write_secret(s: &Scratch, bytes: &[u8], mode: u32) {
    let path = s.root().join("mint.secret");
    std::fs::write(&path, bytes).expect("secret");
    chmod(&path, mode);
}

/// `aterm link broker <sock> <log> --secret-file <root>/mint.secret`, the guarded bus `on`
/// deploys under launchd, started here through [`harness::spawn_broker`] instead; killed on drop.
struct Broker {
    child: aterm_uds::spawnfd::Child,
    /// Held while it serves ([`harness::BrokerChild`]).
    _lifeline: aterm_uds::lifeline::Lifeline,
}

impl Broker {
    fn spawn(s: &Scratch) -> Self {
        Self::spawn_with(s, &[])
    }

    /// [`Broker::spawn`] with `extra` words after the secret (a
    /// `--durability`, as the manual's by-hand step 1 adds it).
    fn spawn_with(s: &Scratch, extra: &[&str]) -> Self {
        chmod(&s.root(), 0o700);
        let secret = s.root().join("mint.secret");
        if !secret.exists() {
            write_secret(s, &[0x6b; 32], 0o600);
        }
        let (sock, log) = (s.sock(), s.path("root/bus.log"));
        let secret = secret.to_string_lossy().into_owned();
        let mut args = vec![
            sock.as_str(),
            log.as_str(),
            "--secret-file",
            secret.as_str(),
        ];
        args.extend_from_slice(extra);
        let mut broker = harness::spawn_broker(&s.dir, &args, |cmd| {
            cmd.stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::null());
        });
        let first = harness::first_line_within(&mut broker.child, "its readiness line");
        assert_eq!(first.trim(), format!("listening {sock}"));
        Self {
            child: broker.child,
            _lifeline: broker.lifeline,
        }
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The caps `on` minted, checked against the broker it deploys (the root's socket, guarded
/// by the root's secret): every grant attaches and a `Last` walk under the ring answers,
/// while a cap minted under another secret and a halt published with no cap are refused,
/// which tells a guarded broker from one that checks nothing.
fn deployed_broker_checks_the_minted_caps(s: &Scratch, node: &str) {
    let caps = aterm_link::bridge::read_cap_file(&s.path("root/node.cap")).expect("cap file");
    assert_eq!(caps.len(), 8);
    let mut c = harness::broker_client(s.sock());
    c.attach_all(
        caps.iter()
            .map(|cap| (cap.grant.as_str(), cap.tag.as_slice())),
    )
    .unwrap_or_else(|e| panic!("the deployed broker refused the ring: {e}"));
    c.last("/f/local/pub/*/*/presence", "", 8)
        .expect("a Last walk under the node ring's `ro:/f/local/pub/>`");
    let forged = astream_cap::mint(
        b"not-the-secret-not-the-secret-00",
        &format!("ro:/f/local/in/{node}/>"),
    )
    .expect("mint under another secret");
    let mut d = harness::broker_client(s.sock());
    assert!(
        d.attach(&forged.filter, &forged.tag).is_err(),
        "a cap minted under another secret must be refused, or this check proves nothing"
    );
    let mut e = harness::broker_client(s.sock());
    assert!(
        e.publish(
            astream_cap::producer_id_of("h-anyone"),
            1,
            "/f/local/fleet/h-anyone/halt",
            b"v=1 t=1 state=on",
        )
        .is_err(),
        "a halt from a connection with no cap must be refused: the deployed broker is guarded"
    );
}

/// A headless aterm with no fabric command: the instance `on` has to arm.
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
        let mut cmd = Command::new(harness::aterm_to_launch());
        prepare_gui_environment(&mut cmd);
        cmd.arg("--headless")
            .env("HOME", s.path("home"))
            .env("TMPDIR", &s.dir)
            .env("XDG_RUNTIME_DIR", s.path("run"))
            .env("XDG_CONFIG_HOME", s.path("cfg"))
            .env("SHELL", "/bin/sh")
            .args(["--lines", "40", "--columns", "120"])
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err);
        harness::arm_lifeline(&mut cmd, &s.dir);
        let mut child = cmd.spawn().expect("launch aterm --headless");
        let ctl_sock = s.ctl_sock();
        let token =
            harness::World::wait_for_token_of(&ctl_sock, &mut child, &s.dir.join("gui.log"));
        Self {
            child,
            ctl_sock,
            token,
            state: s.root().join("link-state"),
        }
    }

    fn ctl(&self) -> Ctl {
        harness::connect(&self.ctl_sock, &self.token).expect("connect to aterm")
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
        let launcher = harness::identity_of(self.child.id());
        let _ = self.child.kill();
        let _ = self.child.wait();
        harness::end_bridge(&self.state, launcher);
    }
}

/// The step lines named `<name>` (e.g. `node already n-…`), from the STEPS block
/// only: the status report that follows a real run has a `node` row of its own.
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

/// **THE DEV APP'S PATH HOLDS A SPACE** (`~/Applications/aterm (dev).app/…`,
/// what `targo --unverified ship dev-app` installs), and `on` used to refuse
/// it at the argv step: `[fabric] command` is a whitespace-split line.
#[test]
fn on_with_an_aterm_path_holding_a_space_runs_it_through_a_link() {
    let s = Scratch::new("sp");
    let spaced_dir = s.dir.join("bin/aterm (dev).app");
    std::fs::create_dir_all(&spaced_dir).expect("a spaced dir");
    let spaced = spaced_dir.join("aterm");
    std::os::unix::fs::symlink(harness::aterm(), &spaced).expect("link aterm under a spaced path");
    let spaced = spaced.to_string_lossy().into_owned();
    let gui = Gui::boot(&s);
    let session = gui.boot_session();
    let _broker = Broker::spawn(&s);

    let mut on = s.fabric_cmd(&["on", "--service", "none"]);
    on.env("ATERM_BIN", &spaced);
    let (code, out, err) = run(&mut on);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    for (name, verdict) in [
        ("binary", "ok"),
        ("program", "done"),
        ("config", "done"),
        ("rendezvous", "done"),
        ("instance", "done"),
        ("proof", "ok"),
    ] {
        assert!(
            step(&out, name)
                .iter()
                .any(|l| l.split_whitespace().nth(1) == Some(verdict)),
            "step `{name}` should read `{verdict}`:\n{out}"
        );
    }
    let link = s.root().join("bin/aterm");
    assert_eq!(
        std::fs::read_link(&link).expect("the program link"),
        std::path::Path::new(&spaced),
        "the link names the spaced path"
    );
    let link = link.to_string_lossy().into_owned();
    let node = std::fs::read_to_string(s.root().join("link-state/node")).expect("node");
    let node = node.trim();
    let line = format!(
        "{link} link serve --fleet local --broker {} --cap-file {} --state {} --accept-from {node}",
        s.sock(),
        s.path("root/node.cap"),
        s.path("root/link-state")
    );
    let want = aterm_types::fabric_command::split_line(&line);
    let config = std::fs::read_to_string(s.config()).expect("config");
    assert!(
        config.contains(&format!("command = \"{line}\"\n")),
        "the LINE form, never an array:\n{config}"
    );
    assert!(!config.contains(&spaced), "{config}");
    assert_eq!(
        aterm_link::fabric::command_in_toml(&config),
        Ok(Some(want.clone()))
    );
    let rendezvous_text = std::fs::read_to_string(s.rendezvous()).expect("fabric.toml");
    assert!(
        rendezvous_text.contains(&format!("command = \"{line}\"\n")),
        "{rendezvous_text}"
    );
    let rendezvous = aterm_link::enable::Rendezvous::parse(&rendezvous_text).expect("parses");
    assert_eq!(rendezvous.command, want);
    let h = gui.verb("fabric status").header().to_string();
    assert_eq!(kv(&h, "state"), Some("connected"), "{h}");
    assert_eq!(
        aterm_link::fabric::status_argv(&h),
        Some(want.clone()),
        "{h}"
    );
    let bridge: i32 = std::fs::read_to_string(s.root().join("link-state/pid"))
        .expect("the bridge's pid file")
        .trim()
        .parse()
        .expect("a pid");
    let argv0 = aterm_uds::process::read_process_argv(bridge)
        .and_then(|argv| argv.first().map(|w| w.to_string_lossy().into_owned()));
    assert_eq!(
        argv0.as_deref(),
        Some(link.as_str()),
        "the bridge's argv[0]"
    );
    let inbox = gui.verb(&format!("@{session} inbox --peek"));
    assert!(
        inbox
            .rows()
            .iter()
            .any(|r| r.starts_with("msg ") && kv(r, "kind") == Some("note")),
        "the proof's note was delivered: {:#?}",
        inbox.rows()
    );
    let split = aterm_types::fabric_command::split_line(&format!("{spaced} link serve"));
    assert_ne!(split[0], spaced);
    assert!(!std::path::Path::new(&split[0]).exists(), "{}", split[0]);
    let mut again = s.fabric_cmd(&["on", "--service", "none"]);
    again.env("ATERM_BIN", &spaced);
    let (code, out, err) = run(&mut again);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    for name in ["program", "config"] {
        assert!(
            step(&out, name)
                .iter()
                .any(|l| l.split_whitespace().nth(1) == Some("already")),
            "step `{name}` should read `already`:\n{out}"
        );
    }
}

/// End to end, twice.
#[test]
fn on_provisions_arms_proves_and_a_second_on_changes_nothing() {
    let bin = harness::aterm();
    let s = Scratch::new("on");
    let gui = Gui::boot(&s);
    let sid = gui.boot_session();
    let status = gui.verb("fabric status");
    assert!(
        kv(status.header(), "supervised") == Some("0"),
        "the instance starts unarmed: {}",
        status.header()
    );

    let (code, out, err) = s.fabric(&["on", "--service", "none"]);
    assert_eq!(code, 1, "stdout:\n{out}\nstderr:\n{err}");
    for (name, verdict) in [
        ("binary", "ok"),
        ("root", "done"),
        ("node", "done"),
        ("secret", "done"),
        ("cap", "done"),
        ("broker", "FAILED"),
    ] {
        let lines = step(&out, name);
        assert!(
            lines
                .iter()
                .any(|l| l.split_whitespace().nth(1) == Some(verdict)),
            "step `{name}` should read `{verdict}`:\n{out}"
        );
    }
    assert!(
        step(&out, "broker").iter().any(|l| l.contains(&format!(
            "{bin} link broker {} {} --secret-file {}",
            s.sock(),
            s.path("root/bus.log"),
            s.path("root/mint.secret")
        ))),
        "the broker to run is the GUARDED one:\n{out}"
    );
    assert!(step(&out, "config").is_empty(), "{out}");
    assert_eq!(mode_of(&s.root().join("mint.secret")), 0o600);
    let secret = std::fs::read(s.root().join("mint.secret")).expect("the minted secret");
    assert_eq!(secret.len(), 32);
    let broker = Broker::spawn(&s);
    assert_eq!(
        std::fs::read(s.root().join("mint.secret")).expect("secret"),
        secret,
        "the broker is guarded by the secret `on` minted"
    );

    let (code, out, err) = s.fabric(&["on", "--service", "none"]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert!(err.is_empty(), "nothing on stderr: {err}");
    for (name, verdict) in [
        ("binary", "ok"),
        ("root", "already"),
        ("node", "already"),
        ("secret", "already"),
        ("cap", "already"),
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
    assert!(
        step(&out, "broker").iter().all(|l| !l.contains("WARNING")),
        "a guarded broker is no warning:\n{out}"
    );
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
    assert_eq!(mode_of(&s.root()), 0o700);
    deployed_broker_checks_the_minted_caps(&s, &node);
    let config = std::fs::read_to_string(s.config()).expect("config");
    let command = aterm_link::fabric::command_in_toml(&config)
        .expect("valid TOML")
        .expect("a [fabric] command");
    assert_eq!(
        command,
        aterm_types::fabric_command::split_line(&format!(
            "{bin} link serve --fleet local --broker {} --cap-file {} --state {} --accept-from {node}",
            s.sock(),
            s.path("root/node.cap"),
            s.path("root/link-state")
        ))
    );
    let baseline_backup = std::fs::read_to_string(s.dir.join("cfg/aterm/aterm.toml.bak"))
        .expect("the initial isolation config was backed up");
    assert_eq!(baseline_backup, harness::FIXTURE_GUI_CONFIG);
    let r = std::fs::read_to_string(s.rendezvous()).expect("fabric.toml");
    assert_eq!(mode_of(&s.rendezvous()), 0o600);
    let r = aterm_link::enable::Rendezvous::parse(&r).expect("parses");
    assert_eq!(r.fleet, "local");
    assert_eq!(r.broker, s.sock());
    assert_eq!(r.node, node);
    assert_eq!(r.cap_file, s.path("root/node.cap"));
    assert_eq!(r.state, s.path("root/link-state"));
    assert_eq!(r.command, command);
    let status = gui.verb("fabric status");
    let h = status.header();
    assert_eq!(kv(h, "supervised"), Some("1"), "{h}");
    assert_eq!(kv(h, "state"), Some("connected"), "{h}");
    assert_eq!(
        aterm_link::fabric::status_argv(h),
        Some(command.clone()),
        "{h}"
    );
    let inbox = gui.verb(&format!("@{sid} inbox --peek"));
    let notes: Vec<&String> = inbox
        .rows()
        .iter()
        .filter(|r| r.starts_with("msg ") && kv(r, "kind") == Some("note"))
        .collect();
    assert_eq!(notes.len(), 1, "{:#?}", inbox.rows());
    assert!(kv(notes[0], "off").is_some_and(|o| o.parse::<u64>().is_ok()));

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
    let (code, _, err) = s.link(&["ls", "--broker", "/tmp/atfo-nowhere.sock"]);
    assert_ne!(code, 0, "{err}");
    std::fs::write(
        s.config(),
        format!("font_px = 12.0\n{}", harness::FIXTURE_GUI_CONFIG),
    )
    .expect("drop only the Fabric table, retaining fixture opt-outs");
    let (code, status, _) = s.fabric(&["status"]);
    assert!(
        status.contains("the rendezvous file"),
        "exit {code}:\n{status}"
    );

    let (code, out, err) = s.fabric(&["off", "--service", "none", "--dry-run"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(s.rendezvous().exists(), "a dry run removes nothing");
    let (code, out, err) = s.fabric(&["off", "--service", "none"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(!s.rendezvous().exists(), "off removes the rendezvous file");
    for kept in ["link-state/node", "mint.secret", "node.cap"] {
        assert!(s.root().join(kept).exists(), "off keeps {kept}");
    }
    std::fs::write(
        s.config(),
        format!(
            "font_px = 12.0\n{}\n[fabric]\ncommand = \"{}\"\n\n[keys]\nx = 1\n",
            harness::FIXTURE_GUI_CONFIG,
            command.join(" ")
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
    let (code, doctor, _) = s.fabric(&["doctor"]);
    assert_eq!(code, 1, "{doctor}");

    drop(gui);
    drop(broker);
}

/// `--dry-run` on a bare world with no broker prints every step as `would` and leaves no
/// root, config or rendezvous file.
#[test]
fn dry_run_names_every_step_and_creates_nothing() {
    let s = Scratch::new("dry");
    std::fs::remove_dir_all(s.root()).expect("no root yet");
    let (code, out, err) = s.fabric(&["on", "--dry-run", "--service", "none"]);
    assert_eq!(
        code, 1,
        "a dry run that would fail says so with its exit:\n{out}\n{err}"
    );
    for name in ["root", "node", "secret", "cap", "config", "rendezvous"] {
        assert!(
            step(&out, name)
                .iter()
                .any(|l| l.split_whitespace().nth(1) == Some("would")),
            "step `{name}` should read `would`:\n{out}"
        );
    }
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

/// `--dry-run` over a 0755 root reads a 0600 secret in it as the narrowed root will (`already`)
/// and changes nothing; the control: a 0644 secret there is still refused for its own mode.
#[test]
fn dry_run_reads_the_secret_as_the_narrowed_root_will() {
    for (tag, mode) in [("kept", 0o600), ("wide", 0o644)] {
        let s = Scratch::new(&format!("dry-open-{tag}"));
        let bytes: Vec<u8> = (0..32u8).map(|i| i.wrapping_mul(11)).collect();
        write_secret(&s, &bytes, mode);
        chmod(&s.root(), 0o755);
        let (code, out, err) = s.fabric(&["on", "--dry-run", "--service", "none"]);
        let secret = step(&out, "secret");
        assert!(
            step(&out, "root")
                .iter()
                .any(|l| l.split_whitespace().nth(1) == Some("would")),
            "{tag}: the root step would narrow the root:\n{out}"
        );
        if mode == 0o600 {
            assert!(
                secret
                    .iter()
                    .any(|l| l.split_whitespace().nth(1) == Some("already")),
                "{tag}: the secret the real run keeps reads `already`:\n{out}\n{err}"
            );
            assert!(secret.iter().all(|l| !l.contains("FAILED")), "{tag}: {out}");
            // No broker answers: the dry run says so with its exit, past every step.
            assert_eq!(code, 1, "{tag}:\n{out}\n{err}");
        } else {
            assert!(
                secret
                    .iter()
                    .any(|l| l.contains("FAILED") && l.contains("chmod 600")),
                "{tag}: the secret's own mode still refuses:\n{out}\n{err}"
            );
            assert_eq!(code, 2, "{tag}:\n{out}\n{err}");
        }
        assert_eq!(
            mode_of(&s.root()),
            0o755,
            "{tag}: a dry run narrows nothing"
        );
        assert_eq!(
            std::fs::read(s.root().join("mint.secret")).expect("secret"),
            bytes,
            "{tag}: a dry run replaces no secret"
        );
        assert_eq!(mode_of(&s.root().join("mint.secret")), mode, "{tag}");
        assert!(!s.root().join("link-state").exists(), "{tag}: made state");
        assert!(!s.root().join("node.cap").exists(), "{tag}: minted a cap");
        assert!(!s.config().exists(), "{tag}: wrote the config");
    }
}

/// A rejected mint publishes nothing and supervises nothing; the effect trace runs through
/// `fabric_capability_publication_model` with eight grants.
#[test]
fn a_rejected_mint_publishes_nothing_and_the_trace_satisfies_the_model() {
    let mut model = aterm_models_session::fabric_capability_publication_model();
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
    let foreign = |s: &Scratch, node: &str| {
        let lines: String = aterm_link::enable::node_grants("local", node)
            .iter()
            .map(|g| {
                let c = astream_cap::mint(&[0x42; 32], g).expect("mint under another secret");
                astream_cap::capfile::format_line(&c.filter, &c.tag) + "\n"
            })
            .collect();
        std::fs::write(s.root().join("node.cap"), lines).expect("foreign cap");
    };
    let node_of = |s: &Scratch| -> String {
        std::fs::create_dir_all(s.root().join("link-state")).expect("state");
        std::fs::write(s.root().join("link-state/node"), "n-00000000000000aa\n").expect("node");
        "n-00000000000000aa".to_string()
    };
    let seed_secret = |s: &Scratch| {
        write_secret(
            s,
            &(0..32u8).map(|i| i.wrapping_mul(7)).collect::<Vec<u8>>(),
            0o600,
        );
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

    for existing in ["absent", "stale", "foreign"] {
        for fail_at in ["1", "3", "8"] {
            let s = Scratch::new(&format!("mint-{existing}-{fail_at}"));
            let node = node_of(&s);
            seed_secret(&s);
            if existing == "stale" {
                stale(&s, "n-previous");
            }
            if existing == "foreign" {
                foreign(&s, &node);
            }
            let secret_before = secret_bytes(&s);
            let cap_before = cap_bytes(&s);
            let (code, out, _) = run(s
                .fabric_cmd(&["on", "--service", "none"])
                .env("ATERM_FABRIC_TRACE", s.path("effects.trace"))
                .env("ATERM_FABRIC_FAIL_MINT_AT", fail_at));
            assert_eq!(code, 1, "{existing}/{fail_at}:\n{out}");
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

    for (tag, bytes, mode) in [
        ("short", b"short\n".to_vec(), 0o600),
        ("wide", vec![9u8; 32], 0o644),
    ] {
        let s = Scratch::new(&format!("mint-{tag}"));
        node_of(&s);
        write_secret(&s, &bytes, mode);
        stale(&s, "n-previous");
        let cap_before = cap_bytes(&s);
        let (code, out, _) = run(s
            .fabric_cmd(&["on", "--service", "none"])
            .env("ATERM_FABRIC_TRACE", s.path("effects.trace")));
        assert_eq!(code, 2, "{tag}: {out}");
        assert!(
            step(&out, "secret").iter().any(|l| l.contains("FAILED")),
            "{tag}: {out}"
        );
        assert!(
            step(&out, "secret").iter().all(|l| !l.contains("already")),
            "{tag}: {out}"
        );
        assert_eq!(
            std::fs::read(s.root().join("mint.secret")).expect("secret"),
            bytes
        );
        assert_eq!(mode_of(&s.root().join("mint.secret")), mode, "{tag}");
        assert_eq!(cap_bytes(&s), cap_before);
        assert!(trace_of(&s).is_empty(), "{:?}", trace_of(&s));
    }

    for existing in ["absent", "stale", "foreign"] {
        let s = Scratch::new(&format!("mint-good-{existing}"));
        let node = node_of(&s);
        seed_secret(&s);
        if existing == "stale" {
            stale(&s, "n-previous");
        }
        if existing == "foreign" {
            foreign(&s, &node);
        }
        let secret_before = secret_bytes(&s);
        let broker = Broker::spawn(&s);
        for d in [s.root(), s.root().join("link-state")] {
            chmod(&d, 0o755);
        }
        let (code, out, err) = run(s
            .fabric_cmd(&["on", "--service", "none"])
            .env("ATERM_FABRIC_TRACE", s.path("effects.trace")));
        assert_eq!(code, 0, "{out}\n{err}");
        assert_eq!(secret_bytes(&s), secret_before, "the secret is preserved");
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
        assert_eq!(mode_of(&s.root()), 0o700);
        assert_eq!(mode_of(&s.root().join("link-state")), 0o700);
        assert!(!s.dir.join("cfg/aterm/aterm.toml.bak").exists());
        std::fs::remove_file(s.dir.join("effects.trace")).expect("reset");
        let cap_before = cap_bytes(&s);
        let (code, out, _) = run(s
            .fabric_cmd(&["on", "--service", "none"])
            .env("ATERM_FABRIC_TRACE", s.path("effects.trace")));
        assert_eq!(code, 0, "{out}");
        assert_eq!(trace_of(&s), vec!["Supervise".to_string()]);
        assert_eq!(cap_bytes(&s), cap_before);
        assert!(
            step(&out, "cap").iter().any(|l| l.contains("already")),
            "{out}"
        );
        deployed_broker_checks_the_minted_caps(&s, &node);
        drop(broker);
    }
}

/// A FIFO made at `path` and held open for writing (never written) for as long as the
/// handle lives: the lifeline of a broker a script starts with `--lifeline-fd 3
/// 3<path`, which reads end-of-file once this process lets go of it or dies.
fn held_fifo(path: &Path) -> std::fs::File {
    use std::os::unix::ffi::OsStrExt as _;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("a FIFO path");
    // SAFETY: a NUL-terminated path, live for the call.
    let made = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
    assert_eq!(
        made,
        0,
        "mkfifo {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|e| panic!("hold {}: {e}", path.display()))
}

/// **LAUNCHD IS ASKED THROUGH A DERIVED LABEL, THE PLIST IS THE SCRIPT'S, AND
/// A BROKER LAUNCHD DOES NOT MANAGE IS NEVER BOOTSTRAPPED OVER.** A FAKE
/// `launchctl` on `$PATH` that behaves like the real one where it matters:
/// `bootstrap` starts the broker the plist names and `print gui/<uid>/<label>`
/// shows it with its pid until `bootout` kills it, while the legacy `list`
/// shows nothing — the CALLER's domain, as an SSH session's reads, so a run
/// that still asked `list` would miss launchd's own job (and a domain that
/// does not exist, no GUI login, is `print`'s exit 112).
#[test]
fn launchd_is_asked_through_a_derived_label_and_the_plist_is_the_scripts() {
    let bin = harness::aterm();
    let s = Scratch::new("launchd");
    let calls = s.path("launchctl.calls");
    let pidfile = s.path("launchd.pid");
    aterm_tempfile::stand_in(
        &s.dir.join("bin/launchctl"),
        &format!(
            "#!/bin/sh\n\
             d=${{0%/bin/launchctl}}\n\
             printf '%s\\n' \"$*\" >> \"$d/launchctl.calls\"\n\
             case \"$1\" in\n\
               print)\n\
                 [ -f \"$d/no-gui\" ] && exit 112\n\
                 if [ -f \"$d/launchd.pid\" ] && kill -0 \"$(cat \"$d/launchd.pid\")\" 2>/dev/null \
                    && [ \"${{2##*/}}\" = \"$(cat \"$d/launchd.label\")\" ]; then\n\
                   printf '\\tstate = running\\n\\tpid = %s\\n' \"$(cat \"$d/launchd.pid\")\"\n\
                   exit 0\n\
                 fi\n\
                 exit 113 ;;\n\
               bootstrap)\n\
                 [ -f \"$d/no-gui\" ] && exit 125\n\
                 basename \"$3\" .plist > \"$d/launchd.label\"\n\
                 rm -f \"$d/root/bus.sock\"\n\
                 nohup '{bin}' link broker \"$d/root/bus.sock\" \"$d/root/bus.log\" --secret-file \"$d/root/mint.secret\" --lifeline-fd 3 3<\"$d/lifeline\" >/dev/null 2>&1 &\n\
                 printf '%s' \"$!\" > \"$d/launchd.pid\"\n\
                 exit 0 ;;\n\
               bootout)\n\
                 if [ -f \"$d/launchd.pid\" ]; then kill \"$(cat \"$d/launchd.pid\")\" 2>/dev/null; rm -f \"$d/launchd.pid\"; exit 0; fi\n\
                 exit 3 ;;\n\
             esac\n\
             exit 0\n"
        ),
    )
    .expect("fake launchctl");
    let path = format!("{}:/usr/bin:/bin", s.path("bin"));
    // The broker the fake `bootstrap` starts serves until `off` boots it out, and ends
    // itself with this test however the test ends: its lifeline is this FIFO, which the
    // test holds open for writing and never writes (`aterm_uds::lifeline`).
    let _lifeline = held_fifo(&s.dir.join("lifeline"));
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

    let unmanaged = Broker::spawn(&s);
    let unmanaged_pid = unmanaged.child.id();
    let (code, out, err) = run(s
        .fabric_cmd(&["on", "--service", "launchd"])
        .env("PATH", &path));
    assert_eq!(code, 1, "{out}\n{err}");
    let broker_lines = step(&out, "broker");
    assert!(
        broker_lines
            .iter()
            .any(|l| l.contains("FAILED") && l.contains(&format!("pid {unmanaged_pid}"))),
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
    let (code, dry, _) = run(s
        .fabric_cmd(&["on", "--service", "launchd", "--dry-run"])
        .env("PATH", &path));
    assert_eq!(code, 1, "{dry}");
    assert!(
        step(&dry, "broker").iter().any(|l| l.contains("FAILED")),
        "{dry}"
    );
    assert!(!plist.exists());
    aterm_tempfile::stand_in(
        &s.dir.join("bin/systemctl"),
        "#!/bin/sh\n\
         d=${0%/bin/systemctl}\n\
         printf '%s\\n' \"$*\" >> \"$d/systemctl.calls\"\n\
         case \"$*\" in\n\
           '--user show-environment')\n\
             if [ -f \"$d/no-manager\" ]; then\n\
               echo 'Failed to connect to bus: No medium found' >&2\n\
               exit 1\n\
             fi ;;\n\
           *' show '*) echo 0 ;;\n\
         esac\n\
         exit 0\n",
    )
    .expect("fake systemctl");
    let systemd_calls = s.path("systemctl.calls");
    let asked_only = |asked: &str| {
        !asked.is_empty()
            && asked
                .lines()
                .all(|l| l.starts_with("--user show ") || l == "--user show-environment")
    };
    let (code, out, err) = run(s
        .fabric_cmd(&["on", "--service", "systemd"])
        .env("PATH", &path));
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(
        step(&out, "broker")
            .iter()
            .any(|l| l.contains("FAILED") && l.contains(&format!("pid {unmanaged_pid}"))),
        "systemd must refuse the unmanaged broker as launchd does:\n{out}"
    );
    let asked = std::fs::read_to_string(&systemd_calls).unwrap_or_default();
    assert!(
        asked_only(&asked),
        "systemd must not be asked to start anything over an unmanaged broker: {asked}"
    );
    let no_manager = s.dir.join("no-manager");
    std::fs::write(&no_manager, "").expect("no-manager");
    let _ = std::fs::remove_file(&systemd_calls);
    let (code, out, err) = run(s
        .fabric_cmd(&["on", "--service", "systemd"])
        .env("PATH", &path));
    assert_eq!(code, 1, "{out}\n{err}");
    let broker = step(&out, "broker");
    assert!(
        broker
            .iter()
            .any(|l| l.contains("FAILED") && l.contains("no systemd user manager"))
            && !broker.iter().any(|l| l.contains("an unmanaged broker")),
        "a login with no user manager is refused by naming it:\n{out}"
    );
    let asked = std::fs::read_to_string(&systemd_calls).unwrap_or_default();
    assert!(asked_only(&asked), "{asked}");
    std::fs::remove_file(&no_manager).expect("no-manager");
    assert!(
        !s.dir
            .join(format!("cfg/systemd/user/{label}.service"))
            .exists(),
        "no unit is written for a broker that is not started"
    );
    drop(unmanaged);
    let _ = std::fs::remove_file(&calls);

    let no_manager = s.dir.join("no-manager");
    std::fs::write(&no_manager, "").expect("no-manager");
    for dry in [false, true] {
        let _ = std::fs::remove_file(s.path("systemctl.calls"));
        let mut args = vec!["on", "--service", "systemd"];
        if dry {
            args.push("--dry-run");
        }
        let (code, out, err) = run(s.fabric_cmd(&args).env("PATH", &path));
        assert_eq!(code, 1, "{out}\n{err}");
        assert!(
            step(&out, "broker")
                .iter()
                .any(|l| l.contains("FAILED") && l.contains("no systemd user manager")),
            "{out}"
        );
        assert!(
            !s.dir
                .join(format!("cfg/systemd/user/{label}.service"))
                .exists(),
            "a refused run wrote the unit"
        );
        assert!(!s.config().exists(), "a refused run wrote the config");
        assert!(!s.rendezvous().exists(), "a refused run wrote rendezvous");
        let asked = std::fs::read_to_string(s.path("systemctl.calls")).expect("systemctl calls");
        assert!(
            asked.lines().any(|l| l == "--user show-environment")
                && asked
                    .lines()
                    .all(|l| l == "--user show-environment" || l.starts_with("--user show ")),
            "systemd must only be probed, never reloaded, enabled or started: {asked}"
        );
    }
    std::fs::remove_file(&no_manager).expect("no-manager");

    let no_gui = s.dir.join("no-gui");
    std::fs::write(&no_gui, "").expect("no-gui");
    for dry in [false, true] {
        let mut args = vec!["on", "--service", "launchd"];
        if dry {
            args.push("--dry-run");
        }
        let (code, out, err) = run(s.fabric_cmd(&args).env("PATH", &path));
        assert_eq!(code, 1, "{out}\n{err}");
        assert!(
            step(&out, "broker")
                .iter()
                .any(|l| l.contains("FAILED") && l.contains(&format!("gui/{uid}"))),
            "{out}"
        );
        assert!(!plist.exists(), "a refused run wrote the plist");
        let recorded = std::fs::read_to_string(&calls).unwrap_or_default();
        assert!(
            !recorded.contains("bootstrap") && !recorded.contains("bootout"),
            "{recorded}"
        );
    }
    std::fs::remove_file(&no_gui).expect("no-gui");
    let _ = std::fs::remove_file(&calls);

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
    assert!(
        text.contains(&format!(
            "<string>{bin}</string>\n    <string>link</string>\n    <string>broker</string>\n    <string>{sock}</string>\n    \
             <string>{root}/bus.log</string>\n    <string>--secret-file</string>\n    \
             <string>{root}/mint.secret</string>",
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
            .any(|l| l.contains(&format!("pid {managed_pid}"))),
        "the pid printed is launchd's own, never 0:\n{out}"
    );

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
        step(&out, "broker")
            .iter()
            .any(|l| l.contains("already") && l.contains(&format!("pid {managed_pid}"))),
        "{out}"
    );
    let recorded = std::fs::read_to_string(&calls).expect("launchctl was called");
    assert!(
        !recorded.contains("bootstrap") && !recorded.contains("bootout"),
        "a job that answers is left alone: {recorded}"
    );

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
    let gone_by = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let gone = loop {
        if !harness::alive(pid) {
            break true;
        }
        if std::time::Instant::now() >= gone_by {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    assert!(gone, "the managed broker {pid} must be dead after off");
}

/// **REFUSALS**: a flag `on` does not take (`--tcp`, the removed sealed wire's)
/// and a bad `--service` are usage errors, refused BEFORE anything is written.
#[test]
fn a_usage_error_is_refused_before_anything_is_written() {
    let s = Scratch::new("usage");
    let (code, _, err) = s.fabric(&["on", "--tcp", "127.0.0.1:7000"]);
    assert_eq!(code, 2, "{err}");
    assert!(
        !s.root().join("link-state").exists(),
        "a refused flag provisioned something"
    );
    let (code, _, err) = s.fabric(&["on", "--service", "cron"]);
    assert_eq!(code, 2, "{err}");
    let (code, out, _) = s.fabric(&["doctor"]);
    assert_eq!(code, 2, "{out}");
}

/// An unreadable or invalid role is not a new one-host fabric.
#[test]
fn role_identity_unreadable_state_never_writes() {
    let bin = harness::aterm();
    let mut outcomes = Vec::new();
    for bad in [
        "config-toml",
        "config-directory",
        "bridge-flags",
        "rendezvous",
        "job",
    ] {
        let s = Scratch::new(&format!("role-bad-{bad}"));
        std::fs::create_dir_all(s.root().join("link-state")).expect("state");
        let node = b"n-00000000000000aa\n";
        let cap = b"the cap the root already holds\n";
        std::fs::write(s.root().join("link-state/node"), node).expect("node");
        std::fs::write(s.root().join("node.cap"), cap).expect("cap");
        match bad {
            "config-toml" => std::fs::write(s.config(), "[fabric]\ncommand = [\n").unwrap(),
            "config-directory" => std::fs::create_dir(s.config()).unwrap(),
            "bridge-flags" => std::fs::write(
                s.config(),
                format!("[fabric]\ncommand = \"{bin} link serve --fleet work --no-such-flag\"\n"),
            )
            .unwrap(),
            "rendezvous" => {
                std::fs::create_dir_all(s.rendezvous().parent().unwrap()).unwrap();
                std::fs::write(s.rendezvous(), "fleet = [\n").unwrap();
            }
            "job" => {
                let label = format!(
                    "systems.alab.astream-broker.{}",
                    aterm_link::enable::cksum(s.path("root").as_bytes())
                );
                std::fs::write(
                    s.root().join(format!("{label}.plist")),
                    "<plist><dict></dict></plist>",
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let (dry, dry_out, dry_err) = s.fabric(&["on", "--service", "none", "--dry-run"]);
        let (real, out, err) = s.fabric(&["on", "--service", "none"]);
        let unchanged = std::fs::read(s.root().join("node.cap")).unwrap() == cap
            && std::fs::read(s.root().join("link-state/node")).unwrap() == node
            && !s.root().join("mint.secret").exists();
        outcomes.push((
            bad,
            dry,
            real,
            unchanged,
            format!("{dry_out}{dry_err}{out}{err}"),
        ));
    }
    for (bad, dry, real, unchanged, output) in outcomes {
        assert!(
            unchanged,
            "{bad}: uncertain role changed the identity:\n{output}"
        );
        assert_eq!(
            (dry, real),
            (2, 2),
            "{bad}: uncertain role was not refused before writes:\n{output}"
        );
    }
}

/// Repairing a missing cap must keep the fleet named by the existing bridge
/// command.
#[test]
fn role_identity_reapplication_preserves_the_nondefault_fleet() {
    let s = Scratch::new("role-fleet");
    let _broker = Broker::spawn(&s);
    let (code, out, err) = s.fabric(&["on", "--fleet", "work", "--service", "none"]);
    assert_eq!(code, 0, "initial fleet:\n{out}{err}");
    let node = std::fs::read(s.root().join("link-state/node")).unwrap();
    let secret = std::fs::read(s.root().join("mint.secret")).unwrap();
    std::fs::remove_file(s.root().join("node.cap")).expect("simulate a missing cap");
    let (_, report, _) = s.fabric(&["doctor"]);
    let remedies: Vec<_> = report
        .split('`')
        .filter(|c| c.starts_with("aterm fabric on"))
        .collect();
    let (code, out, err) = s.fabric(&["on", "--service", "none"]);
    assert_eq!(code, 0, "reapply fleet:\n{out}{err}");
    let command =
        aterm_link::fabric::command_in_toml(&std::fs::read_to_string(s.config()).unwrap())
            .unwrap()
            .unwrap();
    let flags = aterm_link::fabric::serve_flags(&command).unwrap();
    assert!(
        flags.windows(2).any(|w| w == ["--fleet", "work"]),
        "{command:?}"
    );
    let rendezvous =
        aterm_link::enable::Rendezvous::parse(&std::fs::read_to_string(s.rendezvous()).unwrap())
            .unwrap();
    assert_eq!(rendezvous.fleet, "work");
    let caps = aterm_link::bridge::read_cap_file(&s.path("root/node.cap")).unwrap();
    assert_eq!(caps.len(), 8);
    assert!(
        caps.iter().all(|c| c.grant.contains(":/f/work/")),
        "{caps:?}"
    );
    assert_eq!(
        std::fs::read(s.root().join("link-state/node")).unwrap(),
        node
    );
    assert_eq!(std::fs::read(s.root().join("mint.secret")).unwrap(), secret);
    assert!(!remedies.is_empty(), "missing-cap remedy:\n{report}");
    for remedy in remedies {
        assert!(
            remedy
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .any(|w| w == ["--fleet", "work"]),
            "the repair changes fleet: {remedy}"
        );
    }
}

/// Naming a different fleet is a deliberate departure, so `off` must precede
/// it.
#[test]
fn role_identity_conflicting_fleet_is_refused_before_writes() {
    let s = Scratch::new("role-conflict");
    let _broker = Broker::spawn(&s);
    let (code, out, err) = s.fabric(&["on", "--fleet", "work", "--service", "none"]);
    assert_eq!(code, 0, "initial fleet:\n{out}{err}");
    let paths = [
        s.config(),
        s.rendezvous(),
        s.root().join("node.cap"),
        s.root().join("link-state/node"),
        s.root().join("mint.secret"),
    ];
    let before: Vec<_> = paths
        .iter()
        .map(std::fs::read)
        .collect::<Result<_, _>>()
        .unwrap();
    let (code, out, err) = s.fabric(&["on", "--fleet", "other", "--service", "none"]);
    let after: Vec<_> = paths
        .iter()
        .map(std::fs::read)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        after, before,
        "a conflicting fleet changed the root:\n{out}{err}"
    );
    assert_eq!(
        code, 2,
        "the conflicting fleet was not refused up front:\n{out}{err}"
    );
}

/// **A BARE `on` RE-APPLIES THE ROOT AS IT IS** — doctor's remedy for a
/// broker that does not answer.
#[test]
fn a_bare_on_runs_the_unix_broker_and_refuses_a_job_it_cannot_read() {
    let bin = harness::aterm();
    let s = Scratch::new("role");
    let root = s.path("root");
    let label = format!(
        "systems.alab.astream-broker.{}",
        aterm_link::enable::cksum(root.as_bytes())
    );
    let plist = s.root().join(format!("{label}.plist"));
    let job = |argv: &[&str]| {
        let words: String = argv
            .iter()
            .map(|w| format!("<string>{w}</string>"))
            .collect();
        std::fs::write(
            &plist,
            format!(
                "<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string>\
                 <key>ProgramArguments</key><array>{words}</array></dict></plist>\n"
            ),
        )
        .expect("plist");
    };
    let secret = s.path("root/mint.secret");
    let log = s.path("root/bus.log");
    job(&[
        bin,
        "link",
        "broker",
        &s.sock(),
        &log,
        "--secret-file",
        &secret,
    ]);
    let (code, out, err) = s.fabric(&["on", "--service", "none", "--dry-run"]);
    assert_eq!(code, 1, "no broker answers:\n{out}{err}");
    let unix = format!("{bin} link broker {} {log}", s.sock());
    assert!(
        step(&out, "broker").iter().any(|l| l.contains(&unix)) && !out.contains("--tcp"),
        "{out}"
    );
    job(&[bin, "link", "serve", "--fleet", "local"]);
    let (code, out, err) = s.fabric(&["on", "--service", "none"]);
    assert_eq!(code, 2, "a job naming no broker ran `on`:\n{out}{err}");
    assert!(
        err.contains("does not name a recognized broker command"),
        "{err}"
    );
    for absent in ["link-state", "mint.secret", "node.cap"] {
        assert!(
            !s.root().join(absent).exists(),
            "a refused `on` wrote {absent}"
        );
    }
    assert!(
        !s.rendezvous().exists(),
        "a refused `on` wrote the rendezvous file"
    );
}

/// **A BARE `on` KEEPS THE TIER; ONLY A NAMED ONE RESTARTS FOR IT.**
#[test]
fn a_bare_on_keeps_the_tier_and_only_a_named_one_restarts_for_it() {
    let bin = harness::aterm();
    let s = Scratch::new("tier");
    let root = s.path("root");
    let label = format!(
        "systems.alab.astream-broker.{}",
        aterm_link::enable::cksum(root.as_bytes())
    );
    let plist = s.root().join(format!("{label}.plist"));
    let secret = s.path("root/mint.secret");
    let log = s.path("root/bus.log");
    let words: String = [
        bin,
        "link",
        "broker",
        &s.sock(),
        &log,
        "--secret-file",
        &secret,
        "--durability",
        "relaxed",
    ]
    .iter()
    .map(|w| format!("<string>{w}</string>"))
    .collect();
    std::fs::write(
        &plist,
        format!(
            "<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string>\
             <key>ProgramArguments</key><array>{words}</array></dict></plist>\n"
        ),
    )
    .expect("plist");
    let relaxed_argv = format!(
        "{bin} link broker {} {log} --secret-file {secret} --durability relaxed",
        s.sock()
    );
    let (code, out, err) = s.fabric(&["on", "--service", "none", "--dry-run"]);
    assert_eq!(code, 1, "no broker answers:\n{out}{err}");
    assert!(
        step(&out, "broker")
            .iter()
            .any(|l| l.contains(&relaxed_argv)),
        "a bare `on` dropped the job's tier:\n{out}"
    );
    assert!(
        step(&out, "durability")
            .iter()
            .any(|l| l.contains("relaxed") && l.contains(&plist.display().to_string())),
        "the tier is said, and where it was kept from:\n{out}"
    );
    let (code, out, err) = s.fabric(&[
        "on",
        "--service",
        "none",
        "--dry-run",
        "--durability",
        "strict",
    ]);
    assert_eq!(code, 1, "no broker answers:\n{out}{err}");
    assert!(
        step(&out, "broker").iter().any(|l| l.contains(&format!(
            "{bin} link broker {} {log} --secret-file {secret} --durability strict",
            s.sock()
        ))),
        "a named strict is WRITTEN, so a later build whose default differs keeps it:\n{out}"
    );
    assert!(
        step(&out, "durability")
            .iter()
            .any(|l| l.contains("strict, was relaxed")),
        "{out}"
    );
    std::fs::remove_file(&plist).expect("plist");

    let restart = |out: &str| -> Option<String> {
        step(out, "broker")
            .iter()
            .find(|l| l.contains("WARNING"))
            .map(|l| (*l).to_string())
    };
    for (running, other) in [("relaxed", "strict"), ("strict", "relaxed")] {
        let broker = Broker::spawn_with(&s, &["--durability", running]);
        let (_, out, err) = s.fabric(&["on", "--service", "none"]);
        assert!(
            step(&out, "broker")
                .iter()
                .any(|l| l.split_whitespace().nth(1) == Some("already")),
            "{running}: the hand-run broker answers:\n{out}{err}"
        );
        assert_eq!(
            restart(&out),
            None,
            "{running}: a bare `on` keeps the tier it runs:\n{out}"
        );
        let (_, out, err) = s.fabric(&["on", "--service", "none", "--durability", other]);
        let warning = restart(&out).unwrap_or_else(|| {
            panic!("{running}: `--durability {other}` restarts nothing silently:\n{out}{err}")
        });
        assert!(
            warning.contains(&format!("it runs at durability {running}")),
            "{warning}"
        );
        assert!(
            warning.contains(&format!("--durability {other}")),
            "the restart line carries the named tier: {warning}"
        );
        assert!(
            !warning.contains(&format!("--durability {running}")),
            "and not the one it replaces: {warning}"
        );
        drop(broker);
    }
}

/// Wait for `@<sid> status` to read `hold=<want>`.
fn until_hold(gui: &Gui, sid: &str, want: u8) -> std::time::Duration {
    let started = std::time::Instant::now();
    loop {
        let status = gui.verb(&format!("@{sid} status"));
        if status.header().contains(&format!(" hold={want} ")) {
            return started.elapsed();
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the bridge never made the session hold={want}: {}",
            status.header()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// **A PERSON HALTS THE FLEET AND WITHDRAWS IT, AND `off` NAMES EVERY SESSION
/// THE FLEET HOLDS.** On the guarded broker `on` deploys, with the commands a
/// person runs: `mint-for h-review`, `halt`, and `halt off`.
#[test]
fn review_off_names_a_session_the_fleet_holds() {
    let s = Scratch::new("held");
    let broker = Broker::spawn(&s);
    let gui = Gui::boot(&s);
    let sid = gui.boot_session();
    let (code, out, err) = s.fabric(&["on", "--service", "none"]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");

    let cap = s.path("h-review.cap");
    let (code, out, err) = s.fabric(&["mint-for", "h-review", "--out", &cap]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert_eq!(harness::mode_of(Path::new(&cap)), 0o600);
    assert!(
        out.contains(&format!("aterm fabric halt --cap-file {cap} off")),
        "mint-for names the commands the person runs:\n{out}"
    );
    let (code, _, err) = s.fabric(&["halt", "--cap-file", &s.path("root/node.cap"), "nope"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("holds no person's grant"), "{err}");
    let svc = s.path("a-svc.cap");
    let (code, out, err) = s.fabric(&["mint-for", "a-svc", "--out", &svc]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert!(!out.contains("aterm fabric halt"), "{out}");
    let (code, out, err) = s.fabric(&["halt", "--cap-file", &svc, "nope"]);
    assert_eq!(code, 2, "stdout:\n{out}\nstderr:\n{err}");
    assert!(!out.contains("halt is ON"), "{out}");

    let (code, out, err) = s.fabric(&["halt", "--cap-file", &cap, "review", "halt"]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert!(out.contains("h-review's halt is ON at @"), "{out}");
    eprintln!(
        "MEASURED fleet halt -> status hold=1: {} ms",
        until_hold(&gui, &sid, 1).as_millis()
    );
    let denied = gui.verb(&format!("hold {sid} off"));
    assert!(
        denied.header().starts_with("ERR denied"),
        "a fleet hold is not the Owner's to lift: {}",
        denied.header()
    );
    let named_held = |args: &[&str]| {
        let (code, out, err) = s.fabric(args);
        assert_eq!(code, 0, "{args:?}:\n{out}\n{err}");
        let held = step(&out, "held");
        assert!(
            held.iter().any(|l| l.contains(&format!("@{sid}"))
                && l.contains("origin=fleet")
                && l.contains("aterm fabric halt --cap-file")),
            "{args:?} must name the held session and the command that lifts it:\n{out}"
        );
    };
    named_held(&["off", "--service", "none", "--dry-run"]);

    let (code, out, err) = s.fabric(&["halt", "--cap-file", &cap, "off"]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert!(out.contains("h-review's halt is OFF at @"), "{out}");
    until_hold(&gui, &sid, 0);

    let (code, out, err) = s.fabric(&["halt", "--cap-file", &cap, "review", "halt"]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    until_hold(&gui, &sid, 1);
    named_held(&["off", "--service", "none"]);
    let status = gui.verb(&format!("@{sid} status"));
    assert!(status.header().contains(" hold=1 "), "{}", status.header());
    drop(gui);
    drop(broker);
}

/// **`(0600)` IS TRUE WHEN IT IS SAID.** `mint-for --out` over an IDENTICAL
/// cap another user could read makes it 0600 before it says so, and stderr
/// says it was readable and what voids a copy; over a DIFFERENT file it is
/// refused, the file untouched; and a symlink at `--out` is refused, its
/// target's bytes and mode untouched: a cap is never written — or chmodded —
/// through one.
#[test]
fn mint_for_never_claims_0600_of_a_file_that_is_not() {
    let s = Scratch::new("mint-out");
    let _broker = Broker::spawn(&s);
    let (code, out, err) = s.fabric(&["on", "--service", "none"]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    let cap = s.path("h-review.cap");
    let (code, out, err) = s.fabric(&["mint-for", "h-review", "--out", &cap]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    chmod(Path::new(&cap), 0o644);
    let (code, out, err) = s.fabric(&["mint-for", "h-review", "--out", &cap]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert!(out.contains(&format!("into {cap} (0600)")), "{out}");
    assert_eq!(
        harness::mode_of(Path::new(&cap)),
        0o600,
        "said 0600 of a 0644 cap"
    );
    assert!(
        err.contains("held this cap at mode 644, readable by other users; now 0600"),
        "{err}"
    );
    let secret = s.root().join("mint.secret");
    assert!(
        err.contains(&format!(
            "move {} aside and run `aterm fabric on` again",
            secret.display()
        )),
        "the remedy names what replaces the secret:\n{err}"
    );

    let other = s.path("h-other.cap");
    let (code, out, err) = s.fabric(&["mint-for", "h-other", "--out", &other]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    let theirs = std::fs::read(&other).expect("h-other's cap");
    let (code, _, err) = s.fabric(&["mint-for", "h-review", "--out", &other]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("holds a different cap"), "{err}");
    assert_eq!(std::fs::read(&other).expect("h-other's cap"), theirs);

    let victim = s.path("victim.txt");
    std::fs::write(&victim, "the operator's own file\n").expect("victim");
    chmod(Path::new(&victim), 0o644);
    let linked = s.path("linked.cap");
    std::os::unix::fs::symlink(&victim, &linked).expect("link");
    let (code, _, err) = s.fabric(&["mint-for", "h-review", "--out", &linked]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("is not a regular file"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&victim).expect("victim"),
        "the operator's own file\n"
    );
    assert_eq!(harness::mode_of(Path::new(&victim)), 0o644);
}

/// **A PERSON READS AN ASK AND ANSWERS IT, AS A HUMAN.** An agent's `post
/// to=h-review kind=ask` lands on the person's mail lane; `inbox` with the
/// person's cap prints it with its answer spelled out and marks it read (a
/// second `inbox` has nothing new); the `post … kind=answer re=<off>` it names
/// reaches the asking session as an ANSWER from `h-review`, `trust=human` —
/// the reply is from the principal the ask went to.
#[test]
fn a_person_reads_an_ask_and_answers_it_as_a_human() {
    let s = Scratch::new("person");
    let broker = Broker::spawn(&s);
    let gui = Gui::boot(&s);
    let sid = gui.boot_session();
    let (code, out, err) = s.fabric(&["on", "--service", "none"]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    let node = std::fs::read_to_string(s.root().join("link-state/node"))
        .expect("node")
        .trim()
        .to_string();
    let cap = s.path("h-review.cap");
    let (code, out, err) = s.fabric(&["mint-for", "h-review", "--out", &cap]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");

    let asked = gui.verb(&format!(
        "@{sid} post to=h-review kind=ask dl=600000 is main green?"
    ));
    assert!(asked.ok(), "{}", asked.header());
    let off = kv(asked.header(), "off")
        .expect("an ask answers its offset")
        .to_string();

    let (code, out, err) = s.fabric(&["inbox", "--cap-file", &cap]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    let row = out
        .lines()
        .find(|l| l.starts_with(&format!("@{off} ")))
        .unwrap_or_else(|| panic!("the ask @{off} is not in the person's inbox:\n{out}\n{err}"));
    assert!(
        row.contains(" ask ") && row.contains(&format!("from={sid}@{node}")),
        "{row}"
    );
    let answer =
        format!("aterm fabric post --cap-file {cap} to=@{sid}@{node} kind=answer re={off} <text>");
    assert!(out.contains(&answer), "inbox spells the answer:\n{out}");
    let (code, out, err) = s.fabric(&["inbox", "--cap-file", &cap]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !out.contains(&format!("@{off} ")) && err.contains("no new mail"),
        "the first inbox marked it read:\n{out}\n{err}"
    );

    let to = format!("to=@{sid}@{node}");
    let re = format!("re={off}");
    let (code, out, err) = s.fabric(&[
        "post",
        "--cap-file",
        &cap,
        &to,
        "kind=answer",
        &re,
        "yes,",
        "green",
    ]);
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    let got = harness::until("the person's answer in the asker's inbox", || {
        gui.verb(&format!("@{sid} inbox --peek"))
            .rows()
            .iter()
            .find(|r| kv(r, "kind") == Some("answer") && kv(r, "re") == Some(off.as_str()))
            .cloned()
    });
    assert!(
        kv(&got, "trust") == Some("human") && got.contains("from=h-review"),
        "{got}"
    );
    drop(gui);
    drop(broker);
}

/// `off` acts on the root the fabric is on.
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
            out.lines().next().is_some_and(|l| l.contains(&root_a))
                && out.contains(&format!("{root_a}/bus.log"))
                && out.contains(&node),
            "{env:?}: the config's root, A:\n{out}"
        );
        assert!(!out.contains(&root_b), "{env:?}: B is never named:\n{out}");
    }
    std::fs::write(s.config(), "font_px = 12.0\n").expect("drop the table");
    let (code, out, _) = run(s
        .fabric_cmd(&["off", "--service", "none", "--dry-run"])
        .env("ATERM_FABRIC_HOME", &root_b));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.lines().next().is_some_and(|l| l.contains(&root_a)) && !out.contains(&root_b),
        "the rendezvous file's root, A:\n{out}"
    );
    std::fs::remove_file(s.rendezvous()).expect("drop the rendezvous file");
    let (code, out, _) = run(s
        .fabric_cmd(&["off", "--service", "none", "--dry-run"])
        .env("ATERM_FABRIC_HOME", &root_b));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.lines().next().is_some_and(|l| l.contains(&root_b)),
        "the environment's root, B:\n{out}"
    );
    drop(broker);
}
