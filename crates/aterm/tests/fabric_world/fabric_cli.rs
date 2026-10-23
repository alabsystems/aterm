// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric` against a REAL broker — `aterm link broker`, the binary the launchd job runs — in a temp dir of its own, driven through the real `aterm fabric` command line.
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use crate::harness;

use std::io::{BufRead, BufReader};
use std::os::unix::net::{UnixDatagram, UnixListener};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long anything here may take before it is a hang rather than a slow
/// machine (AGENTS.md: a hang detector is a minute, never a latency budget).
const DEADLINE: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The mint secret every scratch broker is guarded with.
const SECRET: &[u8] = b"fabric-cli-test-mint-secret-0032";

/// A broker client holding the fleet's unbound root grant (`/f/t1/>`), minted
/// under [`SECRET`] — the god cap a human or a bridge stand-in publishes with
/// here, on a broker that checks every request.
fn root_client(sock: &str) -> astream_broker::Client {
    let mut c = harness::broker_client(sock);
    let root = astream_cap::mint(SECRET, "/f/t1/>").expect("mint the fleet root");
    c.attach(&root.filter, &root.tag)
        .expect("the guarded broker accepts the fleet root");
    c
}

/// A private directory under `/tmp` — short, because a Unix socket path must
/// fit `sun_path` (104 bytes on macOS) and a per-user `$TMPDIR` does not.
struct Scratch {
    dir: aterm_tempfile::TempDir,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("atf-{tag}-"))
            .tempdir_in("/tmp")
            .expect("scratch dir");
        for sub in ["cfg/aterm", "run/aterm", "link", "home"] {
            std::fs::create_dir_all(dir.path().join(sub)).expect("scratch dir");
        }
        std::fs::write(dir.path().join("link/node"), "n-00000000000000aa\n").expect("node id");
        let secret = dir.path().join("mint.secret");
        std::fs::write(&secret, SECRET).expect("mint secret");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600))
                .expect("chmod 600");
        }
        let cap: String = [
            "rw,p=n-00000000000000aa:/f/t1/pub/n-00000000000000aa/>",
            "ro:/f/t1/>",
        ]
        .iter()
        .map(|g| {
            let c = astream_cap::mint(SECRET, g).expect("mint");
            astream_cap::capfile::format_line(&c.filter, &c.tag) + "\n"
        })
        .collect();
        std::fs::write(dir.path().join("node.cap"), cap).expect("cap file");
        Self { dir }
    }

    fn path(&self, rel: &str) -> String {
        self.dir.path().join(rel).to_string_lossy().into_owned()
    }

    fn sock(&self) -> String {
        self.path("b.sock")
    }

    /// The bridge command, in the spelling `aterm fabric on` writes
    /// (`aterm link serve`) or the argv0 alias's (`aterm-link serve`).
    fn command(&self, sock: &str, alias: bool) -> String {
        let program = if alias {
            "/usr/local/bin/aterm-link serve".to_string()
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
        std::fs::write(self.dir.path().join("cfg/aterm/aterm.toml"), body).expect("config");
    }

    fn config_path(&self) -> String {
        self.path("cfg/aterm/aterm.toml")
    }
}

/// `aterm fabric <args>` in the sealed environment, with `env_cmd` as
/// `ATERM_FABRIC_COMMAND` when given.
fn fabric_cmd(s: &Scratch, env_cmd: Option<&str>, args: &[&str]) -> Command {
    let mut cmd = Command::new(harness::aterm_to_launch());
    cmd.args(["link", "fabric"])
        .args(args)
        .env_remove("ATERM_FABRIC_COMMAND")
        .env_remove("ATERM_FABRIC_HOME")
        .env("HOME", s.path("home"))
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
    } = harness::output_within(&mut fabric_cmd(s, env_cmd, args), "aterm fabric");
    (
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

fn json(text: &str) -> aterm_json::Value {
    aterm_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// A broker process, killed on drop. Started by [`harness::spawn_broker`]: it holds its stdio
/// and nothing else of this process, and ends itself with its lifeline if this process dies
/// first.
struct Broker {
    child: aterm_uds::spawnfd::Child,
    /// Held while it serves ([`harness::BrokerChild`]).
    _lifeline: aterm_uds::lifeline::Lifeline,
}

impl Broker {
    /// `aterm link broker <sock> <log> --secret-file <secret>`, returned once
    /// it prints `listening`.
    fn spawn(s: &Scratch) -> Self {
        Self::serve(
            &s.sock(),
            &s.path("b.log"),
            &["--secret-file", &s.path("mint.secret")],
        )
    }

    /// `aterm link broker <sock> <log> <extra>…`, returned once it prints
    /// `listening`.
    fn serve(sock: &str, log: &str, extra: &[&str]) -> Self {
        Self::serve_env(sock, log, extra, &[])
    }

    /// [`Broker::serve`] with extra environment: a dev seam a test arms. The
    /// lifeline's FIFO is made, and unlinked at once, in the socket's own
    /// scratch directory.
    fn serve_env(sock: &str, log: &str, extra: &[&str], env: &[(&str, &str)]) -> Self {
        let dir = Path::new(sock).parent().expect("the socket's directory");
        let mut args = vec![sock, log];
        args.extend_from_slice(extra);
        let mut broker = harness::spawn_broker(dir, &args, |cmd| {
            cmd.stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::null());
            for (k, v) in env {
                cmd.env(k, v);
            }
        });
        let lines = line_reader(broker.child.stdout.take().expect("stdout"));
        let first = lines
            .recv_timeout(DEADLINE)
            .expect("the broker prints its readiness line");
        assert_eq!(first, format!("listening {sock}"));
        Self {
            child: broker.child,
            _lifeline: broker.lifeline,
        }
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

/// **A READINESS LINE NOBODY READS DOES NOT STOP THE BROKER.** A supervisor
/// may read the first `listening` line and close the pipe; `println!` on a
/// closed stdout panics, which would kill the broker right after binding and
/// take its socket with it.
#[test]
fn a_broker_whose_stdout_reader_left_keeps_serving() {
    let s = Scratch::new("epipe");
    let (gone, stdout) = std::io::pipe().expect("a pipe");
    drop(gone);
    let (sock, log, secret) = (s.sock(), s.path("b.log"), s.path("mint.secret"));
    let mut broker = harness::spawn_broker(
        s.dir.path(),
        &[&sock, &log, "--secret-file", &secret],
        |cmd| {
            cmd.stdout(stdout)
                .stderr(aterm_uds::spawnfd::Stdio::piped());
        },
    );
    let err = broker.child.stderr.take().expect("stderr");
    let _broker = Broker {
        child: broker.child,
        _lifeline: broker.lifeline,
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(err).lines() {
            let Ok(line) = line else { break };
            let _ = tx.send(line);
        }
    });
    let mut seen = Vec::new();
    loop {
        match rx.recv_timeout(DEADLINE) {
            Ok(line) if line.starts_with("aterm link broker: unix socket on") => break,
            Ok(line) => seen.push(line),
            Err(e) => panic!("no broker banner ({e:?}); stderr: {seen:?}"),
        }
    }
    let offsets = publish(
        &s.sock(),
        &[(
            "/f/t1/pub/n-00000000000000aa/node/presence".to_string(),
            "alive".to_string(),
        )],
    );
    assert_eq!(
        offsets.len(),
        1,
        "the broker answers after its stdout went away"
    );
}

/// **THE ROOT'S SECRET GUARDS ITS SOCKET, AND THE REPORT MEASURES THE GUARD.**
#[test]
fn the_secret_beside_the_socket_guards_it_and_the_report_measures_the_guard() {
    let s = Scratch::new("guard");
    std::fs::create_dir_all(s.path("bare")).expect("a dir with no secret");
    for (sock, guarded) in [(s.sock(), true), (s.path("bare/b.sock"), false)] {
        let _broker = Broker::serve(&sock, &format!("{sock}.log"), &[]);
        let cmd = s.command(&sock, true);
        let (_, out, err) = fabric(&s, Some(&cmd), &["--json"]);
        let v = json(&out);
        assert_eq!(
            v["broker"]["guarded"].as_bool(),
            Some(guarded),
            "{out}\n{err}"
        );
        assert_eq!(arr(&v, "warnings").is_empty(), guarded, "{out}");
    }
}

/// **THE TIER NAMED IS READ OFF THE BROKER'S PROCESS, THE TIER SERVED OFF ITS
/// `Hello`.** A real `aterm link broker --durability relaxed`: the report's
/// BROKER section and its JSON say `relaxed`, served `relaxed`, and doctor names
/// it as a fact.
#[test]
fn the_report_reads_the_brokers_durability_off_its_process() {
    const ORDERED_SERVED: &str = if cfg!(target_vendor = "apple") {
        "ordered"
    } else {
        "strict"
    };
    let s = Scratch::new("dur");
    let secret = s.path("mint.secret");
    for (name, tier, served, guarded, extra) in [
        (
            "relaxed",
            "relaxed",
            "relaxed",
            true,
            &["--durability", "relaxed"][..],
        ),
        (
            "ordered",
            "ordered",
            ORDERED_SERVED,
            true,
            &["--durability", "ordered"][..],
        ),
        (
            "strict",
            "strict",
            "strict",
            true,
            &["--durability", "strict"][..],
        ),
        ("bare", "ordered", ORDERED_SERVED, true, &[][..]),
        ("unguarded", "strict", "strict", false, &[][..]),
    ] {
        let sock = s.path(&format!("{name}.sock"));
        let log = format!("{sock}.log");
        let mut args = Vec::new();
        if guarded {
            args.extend(["--secret-file", secret.as_str()]);
        }
        args.extend_from_slice(extra);
        let _broker = Broker::serve(&sock, &log, &args);
        let cmd = s.command(&sock, true);
        let (_, out, err) = fabric(&s, Some(&cmd), &["--json"]);
        assert_eq!(
            json(&out)["broker"]["durability"].as_str(),
            Some(tier),
            "{name}: {out}\n{err}"
        );
        assert_eq!(
            json(&out)["broker"]["served"].as_str(),
            Some(served),
            "{name}: the tier served, as the broker says it:\n{out}"
        );
        let row = |text: &str, key: &str| {
            text.lines()
                .map(str::split_whitespace)
                .filter_map(|mut words| (words.next() == Some(key)).then(|| words.next()))
                .flatten()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let (_, text, _) = fabric(&s, Some(&cmd), &[]);
        assert_eq!(row(&text, "acks"), [tier], "{text}");
        assert_eq!(
            text.contains("served strict"),
            tier == "ordered" && ORDERED_SERVED == "strict",
            "{name}: only ordered off macOS is served as another tier here:\n{text}"
        );
        let (_, doctor, _) = fabric(&s, Some(&cmd), &["doctor"]);
        assert_eq!(row(&doctor, "durability:"), [tier], "{doctor}");
        assert!(
            !Path::new(&format!("{log}.aterm-relaxed")).exists(),
            "{name}: no legacy mark is written"
        );
    }
    let log = s.path("never.log");
    // NO LIFELINE on purpose: a refused tier, so the broker exits at its usage check
    // and never serves.
    let refused = harness::output_within(
        Command::new(harness::aterm_to_launch()).args([
            "link",
            "broker",
            &s.path("never.sock"),
            &log,
            "--durability",
            "fast",
        ]),
        "aterm link broker",
    );
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert!(!Path::new(&log).exists(), "a refused tier opens no log");
}

/// **THE REPORT AND ITS DOCTOR SAY WHAT THE BROKER SAYS OF ITS LOG**, end to
/// end: a real `aterm link broker --durability relaxed`, guarded, whose
/// `Hello` entry 0x04 the report's probe reads and whose astream
/// `<log>.health` the report reads with astream's reader.
#[test]
fn the_report_and_its_doctor_say_what_the_broker_says_of_its_log() {
    let s = Scratch::new("health");
    let sock = s.sock();
    let log = s.path("b.log");
    let secret = s.path("mint.secret");
    let args = ["--secret-file", secret.as_str(), "--durability", "relaxed"];
    let seam = aterm_link::broker_health::FAULT_SEAM;
    let broker = Broker::serve_env(
        &sock,
        &log,
        &args,
        &[(seam, "sync escalate=/f/t1/x/> stay")],
    );
    let cmd = s.command(&sock, true);
    let ours = |v: &aterm_json::Value| -> Vec<String> {
        arr(v, "warnings")
            .iter()
            .filter_map(|w| w.as_str().map(str::to_string))
            .filter(|w| w.contains("POISONED") || w.contains("flush of its log failed"))
            .collect()
    };
    publish(
        &sock,
        &[("/f/t1/o/a".to_string(), "an ordinary record".to_string())],
    );

    let (_, out, err) = fabric(&s, Some(&cmd), &["--json"]);
    let v = json(&out);
    let health = &v["broker"]["health"];
    assert!(health["in_band"]["poisoned"].is_null(), "{out}\n{err}");
    assert_eq!(
        health["in_band"]["flushes"]["failed"].as_u64(),
        Some(0),
        "{out}"
    );
    assert_eq!(health["log"].as_str(), Some(log.as_str()), "{out}");
    assert_eq!(
        health["snapshot"]["writer"].as_str(),
        Some("serving"),
        "{out}"
    );
    assert_eq!(health["snapshot"]["current"].as_bool(), Some(true), "{out}");
    assert_eq!(
        health["snapshot"]["pid"].as_u64(),
        Some(u64::from(broker.pid())),
        "{out}"
    );
    assert!(ours(&v).is_empty(), "{out}");
    let (_, doctor, _) = fabric(&s, Some(&cmd), &["doctor"]);
    assert!(
        doctor.contains("\n  health: ok, not poisoned — flushes since it opened: "),
        "{doctor}"
    );
    assert!(doctor.contains("\n  holds: "), "{doctor}");
    assert!(!doctor.contains("POISONED"), "{doctor}");

    let mut c = root_client(&sock);
    c.publish(
        0xfab,
        SEQ.fetch_add(1, Ordering::Relaxed),
        "/f/t1/x/a",
        b"escalated",
    )
    .expect_err("its full flush failed");
    let (code, out) = harness::until("the report to say the log is poisoned", || {
        let (code, out, _) = fabric(&s, Some(&cmd), &["--json"]);
        let v = json(&out);
        let snapshot_says = v["broker"]["health"]["snapshot"]["poisoned"]
            .as_str()
            .is_some();
        (v["broker"]["health"]["in_band"]["poisoned"]
            .as_str()
            .is_some()
            && snapshot_says)
            .then_some((code, out))
    });
    let v = json(&out);
    let health = &v["broker"]["health"];
    let why = health["in_band"]["poisoned"].as_str().unwrap_or_default();
    assert!(why.contains("a flush failed"), "{out}");
    assert_eq!(
        health["in_band"]["flushes"]["failed"].as_u64(),
        Some(1),
        "{out}"
    );
    assert_eq!(
        v["broker"]["served"].as_str(),
        Some("strict"),
        "latched: {out}"
    );
    assert_eq!(health["snapshot"]["current"].as_bool(), Some(true), "{out}");
    let warned = ours(&v);
    assert_eq!(warned.len(), 1, "the poison's warning alone: {out}");
    assert!(
        warned[0].contains("says its log is POISONED (a flush failed"),
        "{out}"
    );
    assert_eq!(code, 1, "a warning: {out}");
    let (_, text, _) = fabric(&s, Some(&cmd), &[]);
    assert!(
        text.lines()
            .any(|l| l.trim_start().starts_with("health ") && l.contains("POISONED (a flush")),
        "{text}"
    );
    let (_, doctor, _) = fabric(&s, Some(&cmd), &["doctor"]);
    assert!(
        doctor.contains("\n  health: POISONED (a flush failed"),
        "{doctor}"
    );
    let fix = doctor
        .lines()
        .skip_while(|l| !l.contains("says its log is POISONED"))
        .nth(1)
        .unwrap_or_default();
    assert!(
        fix.trim_start()
            .starts_with("fix: the broker's log is poisoned and refuses every write"),
        "{doctor}"
    );

    drop(c);
    drop(broker);
    let broker = Broker::serve_env(&sock, &log, &args, &[(seam, "sync escalate=/f/t1/x/>")]);
    let (_, out, _) = fabric(&s, Some(&cmd), &["--json"]);
    let v = json(&out);
    let health = &v["broker"]["health"];
    assert!(health["in_band"]["poisoned"].is_null(), "{out}");
    assert_eq!(
        health["in_band"]["flushes"]["failed"].as_u64(),
        Some(0),
        "{out}"
    );
    assert_eq!(health["snapshot"]["current"].as_bool(), Some(true), "{out}");
    assert!(health["snapshot"]["poisoned"].is_null(), "{out}");
    assert_eq!(
        health["snapshot"]["pid"].as_u64(),
        Some(u64::from(broker.pid())),
        "{out}"
    );
    assert!(ours(&v).is_empty(), "{out}");
}

/// **THE BUILD'S DEFAULT OFF THE BOOT VOLUME IS SERVED STRICT, AND SAYS WHY.**
#[test]
fn the_default_off_the_boot_volume_is_served_strict_and_says_why() {
    let s = Scratch::new("vol");
    let secret = s.path("mint.secret");
    let sock = s.path("vol.sock");
    let _broker = Broker::serve_env(
        &sock,
        &format!("{sock}.log"),
        &["--secret-file", &secret],
        &[(aterm_link::durability::VOLUME_SEAM, "other")],
    );
    let cmd = s.command(&sock, true);
    let (_, out, err) = fabric(&s, Some(&cmd), &["--json"]);
    assert_eq!(
        json(&out)["broker"]["durability"].as_str(),
        Some("ordered"),
        "{out}\n{err}"
    );
    assert_eq!(
        json(&out)["broker"]["served"].as_str(),
        Some("strict"),
        "{out}"
    );
    let (_, text, _) = fabric(&s, Some(&cmd), &[]);
    let acks = text
        .lines()
        .find(|l| l.split_whitespace().next() == Some("acks"))
        .unwrap_or_else(|| panic!("no acks row:\n{text}"));
    let why = if cfg!(target_vendor = "apple") {
        "served strict (its log is not on the boot volume)"
    } else {
        "served strict (the ordering barrier exists only on Apple)"
    };
    assert!(acks.contains(why), "{acks}");
    assert!(
        acks.contains("an ack is on the disk: a power loss loses nothing acked"),
        "strict's words, as served: {acks}"
    );
}

/// **A RELAXED BROKER'S CLEAN STOP CLOSES ITS LOG, AND A REBOOT AFTER IT KEEPS
/// THE IDENTITY.** A real `aterm link broker --durability relaxed`, one post
/// acked, then SIGTERM — what launchd sends at logout and shutdown and `aterm
/// fabric off`'s bootout sends: it exits 0 with its socket unlinked, having
/// dropped its listeners, which is astream's clean close (a full flush, then
/// `Closed` in `<log>.id`).
#[test]
fn a_weak_brokers_clean_stop_closes_its_log_and_a_reboot_keeps_its_identity() {
    use astream_broker::{Boot, BrokerLog, Durability, OpenOptions, Prepared};
    use std::os::unix::process::ExitStatusExt as _;
    let s = Scratch::new("stop");
    let secret = s.path("mint.secret");
    let wait = |b: &mut Broker, what: &str| {
        harness::until(&format!("the broker to exit on {what}"), || {
            b.child.try_wait().ok().flatten()
        })
    };
    let reopen = |log: &str, boot: Boot| {
        let mut opts = OpenOptions::new(Durability::Relaxed);
        opts.boot = boot;
        BrokerLog::open_opts(log, opts, || Ok(Prepared::Nothing)).expect("the log reopens")
    };
    let another_boot = Boot::Known([0x5a; 16]);
    for ((what, sig), tier) in [("SIGTERM", 15), ("SIGKILL", 9)]
        .into_iter()
        .flat_map(|stop| ["relaxed", "ordered"].map(|tier| (stop, tier)))
    {
        let sock = s.path(&format!("{}-{tier}.sock", what.to_lowercase()));
        let log = format!("{sock}.log");
        let mut broker = Broker::serve(
            &sock,
            &log,
            &["--secret-file", &secret, "--durability", tier],
        );
        let mut client = root_client(&sock);
        let before = client.seen_log_identity().expect("the Hello names the log");
        let weak = client
            .served_durability()
            .is_some_and(|s| s.served != astream_broker::Tier::Strict);
        let what = &format!(
            "{what} ({tier}, served {})",
            if weak { tier } else { "strict" }
        );
        client
            .publish(1, 1, "/f/t1/pub/n-00000000000000aa/s-x/history", b"v=1")
            .expect("acked");
        drop(client);
        assert!(
            !Path::new(&format!("{log}.aterm-relaxed")).exists(),
            "{what}: no legacy mark is written"
        );
        harness::kill(broker.pid() as i32, sig);
        let status = wait(&mut broker, what);
        if sig == 9 {
            assert_eq!(status.signal(), Some(9), "{what}: {status:?}");
            let copy = format!("{log}.copy");
            std::fs::copy(&log, &copy).expect("copy the log");
            std::fs::copy(format!("{log}.id"), format!("{copy}.id")).expect("and its sidecar");
            assert_eq!(
                reopen(&copy, Boot::Detect).identity(),
                before,
                "{what}: a kill in the same boot moves nothing"
            );
            let rebooted = reopen(&log, another_boot);
            let after = rebooted.identity();
            if !weak {
                assert_eq!(
                    after, before,
                    "{what}: a strict run's acks were on the disk"
                );
                continue;
            }
            assert_eq!(
                (after.id, after.generation),
                (before.id, before.generation + 1),
                "{what}: SIGKILL, then a reboot: the generation moves by one"
            );
            assert_eq!(
                rebooted.lineage().steps().last(),
                Some(&(after.generation, 1)),
                "{what}: with the lineage step that continues the log below the acked record"
            );
        } else {
            assert!(status.success(), "{what}: a clean stop exits 0: {status:?}");
            assert!(!Path::new(&sock).exists(), "{what}: the socket is unlinked");
            assert_eq!(
                reopen(&log, another_boot).identity(),
                before,
                "{what}: a reboot after a clean stop keeps the identity"
            );
        }
    }
    let sock = s.path("strict.sock");
    let mut broker = Broker::serve(
        &sock,
        &format!("{sock}.log"),
        &["--secret-file", &secret, "--durability", "strict"],
    );
    harness::kill(broker.pid() as i32, 15);
    let status = wait(&mut broker, "SIGTERM");
    assert_eq!(status.signal(), Some(15), "strict: as before: {status:?}");
}

/// **DOCTOR READS THE ROLE OF THE FABRIC ITS REPORT IS ABOUT.** The report
/// follows `$ATERM_FABRIC_COMMAND`; the role its remedies are spelled for must
/// come from the same root, not from the environment's default one.
#[test]
fn doctor_reads_the_role_of_the_fabric_its_report_is_about() {
    let s = Scratch::new("role");
    let root = s.dir.path().join("home/.local/share/aterm-fabric");
    std::fs::create_dir_all(&root).expect("the environment's root");
    let label = format!(
        "systems.alab.astream-broker.{}",
        aterm_link::enable::cksum(root.to_string_lossy().as_bytes())
    );
    std::fs::write(
        root.join(format!("{label}.plist")),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
             <key>Label</key><string>{label}</string>\n\
             <key>ProgramArguments</key><array><string>/usr/bin/true</string></array>\n\
             <key>KeepAlive</key><true/>\n</dict>\n</plist>\n"
        ),
    )
    .expect("the foreign plist");
    let units = s.dir.path().join("cfg/systemd/user");
    std::fs::create_dir_all(&units).expect("the unit dir");
    std::fs::write(
        units.join(format!("{label}.service")),
        "[Service]\nExecStart=/bin/sh -c \"exec /usr/bin/true\"\n",
    )
    .expect("the foreign unit");

    let _broker = Broker::spawn(&s);
    let cmd = s.command(&s.sock(), true);
    let (code, out, err) = fabric(&s, Some(&cmd), &["doctor"]);
    assert!(
        code != 2 && !out.is_empty(),
        "doctor reports on the seam's fabric (exit {code}):\n{out}\n{err}"
    );

    let (code, out, err) = fabric(&s, None, &["doctor"]);
    assert!(
        code == 2 && out.is_empty(),
        "the planted job on the fallback path refuses the role (exit {code}):\n{out}\n{err}"
    );
}

/// The producer sequence every publish here takes the next of. ONE counter for
/// the process: the broker dedups on `(producer_id, producer_seq)`, so a
/// sequence that restarted per call would have a second batch acknowledged at
/// the FIRST batch's offsets and never appended.
static SEQ: AtomicU64 = AtomicU64::new(1);

/// Publish `(subject, body)` records straight to the broker, as a bridge or a
/// human would; answers each one's offset.
fn publish(sock: &str, records: &[(String, String)]) -> Vec<u64> {
    let mut c = root_client(sock);
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

/// **`aterm link broker` OPENS WITH THE ONE CONSTRUCTOR**:
/// the production broker binary retains its log by
/// `aterm_link::broker_rules::default_rules` and its `Hello` advertises
/// exactly them, as the world's in-process broker does
/// (`broker_retention`'s test); a restart, on the same log, sets them again
/// (astream advertises only the rules set since its open).
#[test]
fn the_broker_binary_advertises_the_default_rules() {
    let s = Scratch::new("rules");
    for start in ["first", "restart"] {
        let broker = Broker::spawn(&s);
        let mut c = root_client(&s.sock());
        c.hello().expect("hello");
        assert_eq!(
            c.retention_rules(),
            Some(aterm_link::broker_rules::default_rules()),
            "{start}"
        );
        drop(c);
        drop(broker);
    }
}

/// **`doctor --retire-ghosts` IS DRY UNTIL IT IS TOLD OTHERWISE.**
#[test]
fn doctor_retire_ghosts_lists_dry_and_publishes_exited_with_yes() {
    let s = Scratch::new("ghosts");
    let sock = s.sock();
    let _broker = Broker::spawn(&s);
    let node = "n-00000000000000aa";
    let subject = format!("/f/t1/pub/{node}/s-ghost/presence");
    let live = "v=1 t=1789400000000 inc=7 epoch=e1 gen=g1 state=live hold=0";
    publish(&sock, &[(subject.clone(), live.to_string())]);
    let env_cmd = s.command(&sock, true);

    let (code, out, err) = fabric(&s, Some(&env_cmd), &["doctor", "--retire-ghosts"]);
    assert_eq!(code, 0, "out={out} err={err}");
    assert!(out.contains("@s-ghost"), "{out}");
    assert_eq!(
        last_body(&sock, &subject).as_deref(),
        Some(live),
        "a dry run must not touch the bus"
    );

    let (code, out, err) = fabric(&s, Some(&env_cmd), &["doctor", "--retire-ghosts", "--yes"]);
    assert_eq!(code, 0, "out={out} err={err}");
    let after = last_body(&sock, &subject).expect("the row is still there");
    assert!(after.contains("state=exited"), "{after}");
    assert!(!after.contains("state=live"), "{after}");
    assert!(
        after.contains("inc=7"),
        "the bridge's facts are carried: {after}"
    );
    assert!(
        after.contains("epoch=e1") && after.contains("gen=g1"),
        "{after}"
    );
    assert!(
        !after.contains("t=1789400000000"),
        "t= is refreshed: {after}"
    );
}

/// **AN INSTANCE THAT DID NOT ANSWER KEEPS ITS ROWS OUT OF THE RETIRE SET.**
#[test]
fn doctor_retire_ghosts_refuses_while_an_instance_has_not_answered() {
    let s = Scratch::new("ghostmute");
    let sock = s.sock();
    let _broker = Broker::spawn(&s);
    let node = "n-00000000000000aa";
    let subject = format!("/f/t1/pub/{node}/s-ghost/presence");
    let live = "v=1 t=1789400000000 inc=7 epoch=e1 gen=g1 state=live hold=0";
    publish(&sock, &[(subject.clone(), live.to_string())]);
    let env_cmd = s.command(&sock, true);

    let dir = std::path::PathBuf::from(s.path("run")).join("aterm");
    std::fs::create_dir_all(&dir).expect("run/aterm");
    let mute = dir.join(format!("aterm-{}.sock", std::process::id()));
    let _listener = UnixListener::bind(&mute).expect("bind a mute instance socket");

    let (code, out, err) = fabric(&s, Some(&env_cmd), &["doctor", "--retire-ghosts", "--yes"]);
    assert_ne!(code, 0, "out={out} err={err}");
    assert_eq!(
        last_body(&sock, &subject).as_deref(),
        Some(live),
        "a row an unanswered instance may host must not be retired"
    );
}

/// **A PIPED `y` IS NOT CONSENT.** Without a terminal there is exactly one way
/// to say yes and it is `--yes`, which says so in the argv.
#[test]
fn doctor_retire_ghosts_does_not_take_a_piped_yes() {
    let s = Scratch::new("ghostpipe");
    let sock = s.sock();
    let _broker = Broker::spawn(&s);
    let node = "n-00000000000000aa";
    let subject = format!("/f/t1/pub/{node}/s-ghost/presence");
    let live = "v=1 t=1789400000000 inc=7 epoch=e1 gen=g1 state=live hold=0";
    publish(&sock, &[(subject.clone(), live.to_string())]);
    let env_cmd = s.command(&sock, true);

    let mut child = fabric_cmd(&s, Some(&env_cmd), &["doctor", "--retire-ghosts"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the doctor");
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("stdin");
        let _ = stdin.write_all(b"y\n");
    }
    let out = harness::wait_output_within(child, "the doctor");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert_eq!(
        last_body(&sock, &subject).as_deref(),
        Some(live),
        "a piped `y` must publish nothing"
    );
}

/// The LAST body on `subject`, read back through a fresh client.
fn last_body(sock: &str, subject: &str) -> Option<String> {
    let mut c = root_client(sock);
    let (rows, _) = c.last(subject, "", 8).expect("a Last walk");
    rows.into_iter()
        .rev()
        .find(|(_, s, _)| s == subject)
        .map(|(_, _, raw)| String::from_utf8_lossy(&raw).into_owned())
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

/// NO COMMAND ANYWHERE IS "OFF", exit 2 — and the line says where it looked.
#[test]
fn with_no_command_the_fabric_is_off_says_where_it_looked_and_exits_2() {
    let s = Scratch::new("off");
    let (code, out, _) = fabric(&s, None, &[]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains(&s.config_path()), "where it looked: {out}");
    let (code, out, _) = fabric(&s, None, &["--json"]);
    assert_eq!(code, 2);
    let v = json(&out);
    assert_eq!(v["exit"].as_u64(), Some(2));
    assert!(v["off"].as_str().is_some(), "{out}");

    s.write_config("font_px = 13.0\n\n[fabric]\ncommand = \"  \"\n");
    let (code, out, _) = fabric(&s, None, &["status"]);
    assert_eq!(code, 2, "a blank command is off: {out}");

    s.write_config("[fabric\ncommand = \"x\"\n");
    let (code, out, _) = fabric(&s, None, &[]);
    assert_eq!(code, 2, "{out}");

    s.write_config("[fabric]\ncommand = \"/usr/bin/false\"\n");
    let (code, out, _) = fabric(&s, None, &[]);
    assert_eq!(code, 2, "{out}");

    let (code, _, err) = fabric(&s, None, &["tail"]);
    assert_eq!(code, 2, "{err}");
}

/// A SOCKET FILE IS NOT A BROKER. A socket that was bound and abandoned — the
/// leftover of a broker that died — refuses the connect: not reachable, a
/// warning, exit 1.
#[test]
fn a_dead_socket_file_is_unreachable_and_a_warning() {
    let s = Scratch::new("dead");
    let sock = s.sock();
    drop(UnixDatagram::bind(&sock).expect("bind"));
    assert!(Path::new(&sock).exists(), "the socket FILE is still there");
    s.write_config(&format!(
        "# the operator's file\n[fabric]\ncommand = \"{}\"\n",
        s.command(&sock, false)
    ));

    let (code, out, _) = fabric(&s, None, &[]);
    assert_eq!(code, 1, "{out}");

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
    let head = offs.last().copied().expect("offsets") + 2;

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
    assert_eq!(b["guarded"].as_bool(), Some(true), "the guard, measured");
    assert!(
        b["replaced_build"].is_null(),
        "it runs the file its argv names: {out}"
    );
    assert!(arr(&v, "bridges").is_empty(), "no aterm instance is dialed");
    assert!(arr(&v, "warnings").is_empty(), "{out}");

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

    let sessions = arr(&v, "sessions");
    assert_eq!(sessions.len(), 1, "{out}");
    assert_eq!(sessions[0]["sid"].as_str(), Some("s-far"));
    assert!(sessions[0]["pid"].is_null(), "no instance here hosts it");
    assert_eq!(v["exited_sessions"].as_u64(), Some(1));
    assert_eq!(arr(&v, "nodes")[0]["host"].as_str(), Some("far"));

    let (code, text, _) = fabric(&s, Some(&env_cmd), &["status"]);
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("secret"), "{text}");
}

/// A broker that ANSWERS `Hello` and `Attach` and REFUSES the read, on `sock`,
/// until the test process exits — what `Broker::open_guarded` does when the cap
/// file's grants do not cover `/f/<fleet>/>` (a `--fleet` typo in the bridge
/// command), and what a broker that dies between the attach and the head query
/// does too.
fn refusing_broker(sock: &str) {
    use astream_broker::proto::{
        Request, Response, decode_request, encode_response, read_frame, write_frame,
    };
    let listener = UnixListener::bind(sock).expect("bind the refusing broker");
    std::thread::spawn(move || {
        while let Ok((mut c, _)) = listener.accept() {
            std::thread::spawn(move || {
                while let Ok(Some(p)) = read_frame(&mut c) {
                    let answer = match decode_request(&p) {
                        Some(Request::Hello) => Response::Nonce {
                            nonce: vec![0x11; 32],
                            log: None,
                            ext: astream_broker::NonceExt::default(),
                        },
                        Some(Request::Attach { .. }) => Response::Mark {
                            next: 0,
                            head: 7,
                            resume: String::new(),
                            durable: None,
                            oldest: None,
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
/// `Hello` and every `Attach` succeed; the head query is refused.
#[test]
fn a_broker_that_refuses_the_read_is_not_reachable_yes() {
    let s = Scratch::new("refuse");
    let sock = s.path("refuse.sock");
    refusing_broker(&sock);
    let cmd = s.command(&sock, true);

    let (code, out, _) = fabric(&s, Some(&cmd), &["--json"]);
    let v = json(&out);
    assert!(!arr(&v, "warnings").is_empty(), "{out}");
    assert!(v["broker"]["head"].is_null(), "{out}");
    assert_eq!(code, 1, "a bus that cannot be read is not healthy:\n{out}");
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
    assert!(out.contains("h-andrew"), "{out}");
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
        use aterm_uds::spawnfd::InheritNothing as _;
        let mut child = fabric_cmd(s, Some(&s.command(&s.sock(), false)), args)
            .inherit_nothing()
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

    let from = offs[1].to_string();
    let bodies = Tail::spawn(&s, &["tail", "--bodies", "--from", &from]);
    let rows = tail_rows(&bodies.lines, 2);
    assert!(rows[0].starts_with(&format!("@{}", offs[1])), "{rows:#?}");
    assert!(rows[0].ends_with("text=beta secret"), "{rows:#?}");
    assert!(rows[1].ends_with("text=gamma secret"), "{rows:#?}");

    let live = Tail::spawn(&s, &["tail"]);
    let header = live
        .lines
        .recv_timeout(DEADLINE)
        .expect("tail prints its column header once it is subscribed");
    assert!(header.starts_with("OFFSET"), "{header}");
    let new = publish(
        &s.sock(),
        &[msg(node, "s-a", "h-andrew", "report", "delta")],
    );
    let rows = tail_rows(&live.lines, 1);
    assert!(rows[0].starts_with(&format!("@{}", new[0])), "{rows:#?}");
    assert!(rows[0].contains(" report "), "{rows:#?}");
}

/// THE COMMAND LINE: a usage error is exit 2; `help` is exit 0.
#[test]
fn usage_errors_exit_2_and_help_exits_0() {
    let s = Scratch::new("usage");
    let (code, _, err) = fabric(&s, None, &["--aterm-front-door-routing-probe"]);
    assert_eq!(code, 2, "{err}");
    let (code, out, _) = fabric(&s, None, &["help"]);
    assert_eq!(code, 0, "{out}");
    let (code, _, err) = fabric(&s, None, &["tail", "--json"]);
    assert_eq!(code, 2, "{err}");
}

/// A FILTER THE BROKER WOULD REFUSE NEVER REACHES IT. `tail --filter` passes
/// the bus's own grammar at the parse (astream's `validate_filter_str`, the
/// check the broker parses a subscription with), so with a broker up and the
/// config pointing at it, a `>` that is not the last segment is a usage error
/// naming the filter (exit 2), never the broker's refusal after a dial (exit
/// 1, `aterm fabric tail: …`, which is what it was before the parse asked
/// astream).
#[test]
fn a_filter_the_broker_would_refuse_is_a_usage_error_before_the_dial() {
    let s = Scratch::new("filter");
    let _broker = Broker::spawn(&s);
    let (code, _, err) = fabric(
        &s,
        Some(&s.command(&s.sock(), false)),
        &["tail", "--filter", "/f/t1/>/x"],
    );
    assert_eq!(code, 2, "a usage error, not a broker refusal: {err}");
    assert!(
        err.contains("--filter /f/t1/>/x is not a subject filter"),
        "{err}"
    );
    for dialled in ["aterm fabric tail —", "aterm fabric tail:"] {
        assert!(!err.contains(dialled), "it dialled: {err}");
    }

    let whole = Tail::spawn(&s, &["tail", "--filter", "/f/t1/>"]);
    let header = whole
        .lines
        .recv_timeout(DEADLINE)
        .expect("a filter with `>` last is followed");
    assert!(header.starts_with("OFFSET"), "{header}");
}

/// `aterm link broker <args>`, run to its exit: code, stdout, stderr.
fn broker_run(args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(harness::aterm_to_launch());
    // NO LIFELINE on purpose: every caller's argv ends the broker before it serves (a
    // repair, a refused log, a usage error), and `output_within` bounds the run.
    cmd.args(["link", "broker"]).args(args);
    let Output {
        status,
        stdout,
        stderr,
    } = harness::output_within(&mut cmd, "aterm link broker");
    (
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

/// **A LIFELINE THE BROKER CANNOT ADOPT IS A USAGE ERROR, AND NOTHING SERVES**: a
/// launcher that asked for a lifeline and silently got none would leak the broker it
/// started. A word that is no number, a descriptor that is not open, and stdin that is
/// `/dev/null` (a character device, no pipe) each exit 2 naming the flag, before the
/// socket or the log is made.
#[test]
fn a_lifeline_the_broker_cannot_adopt_is_refused_before_it_serves() {
    let s = Scratch::new("lifeline");
    let (sock, log) = (s.sock(), s.path("b.log"));
    let flag = aterm_uds::lifeline::FLAG;
    // `broker_run`'s stdin is `/dev/null`.
    for fd in ["nope", "1000000", "0"] {
        let (code, out, err) = broker_run(&[&sock, &log, flag, fd]);
        assert_eq!(code, 2, "{flag} {fd}: stdout {out:?}, stderr:\n{err}");
        assert!(err.contains(flag), "{flag} {fd}: it names the flag:\n{err}");
        assert!(!out.contains("listening"), "{flag} {fd}: it served: {out}");
        assert!(
            !Path::new(&sock).exists() && !Path::new(&log).exists(),
            "{flag} {fd}: a refused broker makes no socket and no log"
        );
    }
}

/// **A LOG THE BROKER REFUSES AS CORRUPT HAS A REPAIR THIS BINARY SHIPS, AND
/// DOCTOR PRESCRIBES IT** (audit 51).
#[test]
fn a_corrupt_log_is_prescribed_and_repaired_by_the_shipped_verb() {
    let s = Scratch::new("repair");
    let sock = s.sock();
    let log = s.path("b log's.log");
    {
        let mut l = astream_broker::BrokerLog::open(&log).expect("a fresh log");
        l.publish(0xfab, 1, "/f/t1/pub/x".to_string(), b"one".to_vec())
            .expect("record 0");
        l.publish(0xfab, 2, "/f/t1/pub/x".to_string(), b"two".to_vec())
            .expect("record 1");
    }
    let mut bytes = std::fs::read(&log).expect("the log");
    assert!(bytes.len() > 24, "two records were written");
    bytes[20] ^= 0xFF;
    std::fs::write(&log, &bytes).expect("damage record 0");

    let (code, _, err) = broker_run(&[&sock, &log]);
    assert_eq!(code, 1, "a corrupt log is refused: {err}");
    let last = err.lines().last().unwrap_or_default();
    let repair = last
        .split_once('`')
        .and_then(|(_, tail)| tail.rsplit_once('`'))
        .map(|(command, _)| command)
        .expect("the refusal offers a shell command");
    let repair_args = repair
        .strip_prefix("aterm ")
        .expect("the repair uses the shipped executable");
    std::fs::write(s.path("broker.err"), &err).expect("broker.err");
    let (_, doctor, _) = fabric(&s, Some(&s.command(&sock, true)), &["doctor"]);
    assert!(
        doctor
            .lines()
            .any(|l| l.trim_start().starts_with("fix:") && l.contains("broker --repair")),
        "doctor prescribes the repair:\n{doctor}"
    );

    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command
        .args([
            "-c",
            &format!(r#"exec "$ATERM_TEST_REPAIR_EXE" {repair_args}"#),
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", s.dir.path())
        .env("XDG_CONFIG_HOME", s.path("cfg"))
        .env("XDG_RUNTIME_DIR", s.path("run"))
        .env("ATERM_STATE_HOME", s.path("link"))
        .env("ATERM_TEST_REPAIR_EXE", harness::aterm_to_launch())
        .current_dir(s.dir.path());
    let repaired = aterm_uds::deadline::output_until(&mut command, Instant::now() + DEADLINE)
        .expect("the offered repair finishes; its owned group is reaped on timeout");
    assert!(
        repaired.status.success(),
        "the offered repair failed: {}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    let out = String::from_utf8_lossy(&repaired.stdout);
    let dropped: u64 = out
        .trim()
        .strip_prefix(&format!("{log}: repaired, dropped "))
        .and_then(|r| r.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("the repair reports the bytes it dropped: {out:?}"));
    assert!(dropped > 0, "{out}");
    let (code, out, err) = broker_run(&["--repair", &log]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        format!("{log}: nothing to repair"),
        "idempotent"
    );
    drop(Broker::serve(&sock, &log, &[]));

    let missing = s.path("nope.log");
    let (code, _, _) = broker_run(&["--repair", &missing]);
    assert_eq!(code, 1);
    assert!(!Path::new(&missing).exists(), "a repair creates no log");
    for usage in [
        &["--repair"][..],
        &["--repair", &log, "extra"],
        &["--repair", &log, "--secret-file", "x"],
    ] {
        assert_eq!(broker_run(usage).0, 2, "{usage:?}");
    }
}
