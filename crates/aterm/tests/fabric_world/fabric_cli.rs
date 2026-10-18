// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric` against a REAL broker — `aterm link broker`, the binary the
//! launchd job runs — in a temp dir of its own, driven through the real
//! `aterm fabric` command line.
//!
//! EVERY RUN IS SEALED OFF FROM THE MACHINE IT RUNS ON: its own
//! `$XDG_CONFIG_HOME` (so the operator's aterm.toml is never read), its own
//! `$XDG_RUNTIME_DIR` holding an EMPTY control-socket directory (so no live aterm
//! instance is ever dialed), its own `$HOME` with `ATERM_FABRIC_HOME` removed
//! (so the operator's installed broker job is never read), and
//! `ATERM_FABRIC_COMMAND` removed unless a test sets it. No bridge and no aterm are started: what is under test is the
//! report's own reading of the config, the broker and the bus.
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
/// Every wait ends the moment its event lands. It was 20 s, and a loaded gate's
/// broker took longer than that to print its readiness line.
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
        // The broker is GUARDED, as every broker `aterm fabric on` runs is: a
        // mint secret beside its socket, and two grants minted under it.
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

/// A broker process, killed on drop. Started by `spawnfd`: it holds its stdio
/// and nothing else of this process, not another test thread's pipe for as long
/// as it serves.
struct Broker {
    child: aterm_uds::spawnfd::Child,
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
        let mut child = aterm_uds::spawnfd::command(harness::aterm_to_launch())
            .args(["link", "broker", sock, log])
            .args(extra)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the broker");
        let lines = line_reader(child.stdout.take().expect("stdout"));
        let first = lines
            .recv_timeout(DEADLINE)
            .expect("the broker prints its readiness line");
        assert_eq!(first, format!("listening {sock}"));
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

/// **A READINESS LINE NOBODY READS DOES NOT STOP THE BROKER.** A supervisor
/// may read the first `listening` line and close the pipe; `println!` on a
/// closed stdout panics, which would kill the broker right after binding and
/// take its socket with it. Here the read end
/// is closed BEFORE the broker starts, so every stdout write fails: the broker
/// must still reach its stderr banner and keep serving.
#[test]
fn a_broker_whose_stdout_reader_left_keeps_serving() {
    let s = Scratch::new("epipe");
    let (gone, stdout) = std::io::pipe().expect("a pipe");
    drop(gone);
    let mut child = aterm_uds::spawnfd::command(harness::aterm_to_launch())
        .args(["link", "broker", &s.sock(), &s.path("b.log")])
        .args(["--secret-file", &s.path("mint.secret")])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(stdout)
        .stderr(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn the broker");
    let err = child.stderr.take().expect("stderr");
    let _broker = Broker { child };
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
/// A broker started with NO `--secret-file` — the argv of a supervisor job
/// written before `aterm fabric on` passed one, run again after an update —
/// beside a `mint.secret` is guarded by it: the report's attach of a cap it
/// cannot verify is refused, `guarded: true`, no warning. The same argv on a
/// socket with no secret beside it checks nothing (the negative control), and
/// the report says so.
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
/// it as a fact. `--durability ordered` is accepted and THIS build serves it
/// strict: the JSON says `ordered`, served `strict`, and the `acks` row says
/// "served strict" — read from the broker's own `Hello` (tag 0x01), never
/// inferred from the argv. The same broker without the flag reads `strict`. No
/// tier writes the legacy relaxed mark (`<log>.aterm-relaxed`): astream records
/// how the run stood in its own sidecar. A tier the broker does not know is a
/// usage error, exit 2, before any log is opened.
#[test]
fn the_report_reads_the_brokers_durability_off_its_process() {
    let s = Scratch::new("dur");
    let secret = s.path("mint.secret");
    for (tier, served, extra) in [
        ("relaxed", "relaxed", &["--durability", "relaxed"][..]),
        ("ordered", "strict", &["--durability", "ordered"][..]),
        ("strict", "strict", &[][..]),
    ] {
        let sock = s.path(&format!("{tier}.sock"));
        let log = format!("{sock}.log");
        let mut args = vec!["--secret-file", secret.as_str()];
        args.extend_from_slice(extra);
        let _broker = Broker::serve(&sock, &log, &args);
        let cmd = s.command(&sock, true);
        let (_, out, err) = fabric(&s, Some(&cmd), &["--json"]);
        assert_eq!(
            json(&out)["broker"]["durability"].as_str(),
            Some(tier),
            "{out}\n{err}"
        );
        assert_eq!(
            json(&out)["broker"]["served"].as_str(),
            Some(served),
            "{tier}: the tier served, as the broker says it:\n{out}"
        );
        // The row's KEY and its TIER WORD, never the promise's prose.
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
            tier == "ordered",
            "{tier}: only ordered is served as another tier here:\n{text}"
        );
        let (_, doctor, _) = fabric(&s, Some(&cmd), &["doctor"]);
        assert_eq!(row(&doctor, "durability:"), [tier], "{doctor}");
        assert!(
            !Path::new(&format!("{log}.aterm-relaxed")).exists(),
            "{tier}: no legacy mark is written"
        );
    }
    let log = s.path("never.log");
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

/// **A RELAXED BROKER'S CLEAN STOP CLOSES ITS LOG, AND A REBOOT AFTER IT KEEPS
/// THE IDENTITY.** A real `aterm link broker --durability relaxed`, one post
/// acked, then SIGTERM — what launchd sends at logout and shutdown and `aterm
/// fabric off`'s bootout sends: it exits 0 with its socket unlinked, having
/// dropped its listeners, which is astream's clean close (a full flush, then
/// `Closed` in `<log>.id`). The log, opened next as the first open in ANOTHER
/// boot (`Boot::Known`, standing in a reboot), keeps the identity the broker's
/// `Hello` told, read through `BrokerLog::identity()`.
///
/// NEGATIVE CONTROL: SIGKILL, which no process outlives, leaves the run `Open`.
/// Reopened in the SAME boot (a copy of the log and its sidecar, so no file is
/// opened twice here) it moves nothing (the page cache outlived the broker);
/// reopened in another boot the generation moves by exactly one, with
/// the lineage step `(g + 1, 1)` — the one acked record is below the fork, so a
/// reader resumes there instead of forgetting.
///
/// A strict broker keeps SIGTERM's default action, and dies of it as it always
/// did. SIGINT (^C) and SIGHUP take the same path as SIGTERM, but are not sent
/// here: one this test inherited as ignored (a background job's SIGINT,
/// `nohup`'s SIGHUP) stays ignored in the broker, by design.
#[test]
fn a_relaxed_brokers_clean_stop_closes_its_log_and_a_reboot_keeps_its_identity() {
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
    // No real kernel answers this id: the open it is passed to is in another boot.
    let another_boot = Boot::Known([0x5a; 16]);
    for (what, sig) in [("SIGTERM", 15), ("SIGKILL", 9)] {
        let sock = s.path(&format!("{}.sock", what.to_lowercase()));
        let log = format!("{sock}.log");
        let mut broker = Broker::serve(
            &sock,
            &log,
            &["--secret-file", &secret, "--durability", "relaxed"],
        );
        let mut client = root_client(&sock);
        let before = client.seen_log_identity().expect("the Hello names the log");
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
            assert_eq!(status.signal(), Some(9), "{status:?}");
            // The same-boot reopen runs on a COPY of the log and its sidecar, so this
            // process opens each file once. A first full run of this file failed on the
            // second open of the original with `… is locked by another open BrokerLog`;
            // it was not captured or reproduced, so its cause is NOT established. The one
            // mechanism known to give it: a sibling test forks a child while this
            // process holds the first open's descriptor, and the child's copy keeps the
            // file's lock until its exec closes it. Opening each file once removes that
            // mechanism; if the failure comes back, it was something else.
            let copy = format!("{log}.copy");
            std::fs::copy(&log, &copy).expect("copy the log");
            std::fs::copy(format!("{log}.id"), format!("{copy}.id")).expect("and its sidecar");
            assert_eq!(
                reopen(&copy, Boot::Detect).identity(),
                before,
                "a kill in the same boot moves nothing"
            );
            let rebooted = reopen(&log, another_boot);
            let after = rebooted.identity();
            assert_eq!(
                (after.id, after.generation),
                (before.id, before.generation + 1),
                "SIGKILL, then a reboot: the generation moves by one"
            );
            assert_eq!(
                rebooted.lineage().steps().last(),
                Some(&(after.generation, 1)),
                "with the lineage step that continues the log below the acked record"
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
    let mut broker = Broker::serve(&sock, &format!("{sock}.log"), &["--secret-file", &secret]);
    harness::kill(broker.pid() as i32, 15);
    let status = wait(&mut broker, "SIGTERM");
    assert_eq!(status.signal(), Some(15), "strict: as before: {status:?}");
}

/// **DOCTOR READS THE ROLE OF THE FABRIC ITS REPORT IS ABOUT.** The report
/// follows `$ATERM_FABRIC_COMMAND`; the role its remedies are spelled for must
/// come from the same root, not from the environment's default one. A job no
/// broker runs is planted where the ENVIRONMENT's root keeps its plist (and
/// unit): with the seam naming a live broker elsewhere doctor reports on that
/// broker, and — the negative control — with no seam, config or rendezvous it
/// reads the planted job and refuses, so the file really sits on the fallback
/// path and the seam is what moved the root.
#[test]
fn doctor_reads_the_role_of_the_fabric_its_report_is_about() {
    let s = Scratch::new("role");
    // `$ATERM_FABRIC_HOME` is removed, so the environment's root is the
    // scratch `$HOME`'s; it is not the login user's, so its label carries the
    // root's cksum and its plist lives inside it.
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
    // Exit 2 is doctor's refusal (an unknown role, or the fabric off); a
    // refused role prints no report at all.
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

/// **`doctor --retire-ghosts` IS DRY UNTIL IT IS TOLD OTHERWISE.**
///
/// A presence row under THIS state dir's node says `state=live` and no aterm
/// instance here hosts it — the ghost the bridge's own sweep
/// refuses to touch, because it cannot prove a death it did not witness. The
/// bare run NAMES it and publishes nothing; `--yes` rewrites the row to
/// `exited`, carrying the row's own `inc=`/`epoch=` through so the next bridge
/// can still reason about it.
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

    // DRY: it names the row, publishes nothing, and exits 0.
    let (code, out, err) = fabric(&s, Some(&env_cmd), &["doctor", "--retire-ghosts"]);
    assert_eq!(code, 0, "out={out} err={err}");
    assert!(out.contains("@s-ghost"), "{out}");
    assert_eq!(
        last_body(&sock, &subject).as_deref(),
        Some(live),
        "a dry run must not touch the bus"
    );

    // --yes: the row is retired, and only `state=`/`t=` moved.
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
/// A live socket with no token is discovered, kept in the report with `error`
/// set and NO sessions — so every live row on this node reads unhosted. `status`
/// refuses to NAME such a row; the doctor must refuse to TOUCH any of them,
/// because the rows it would publish `exited` for may be hosted perfectly well
/// by the instance it could not reach.
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

    // An instance socket nobody can authenticate against: alive, unanswerable.
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
/// to say yes and it is `--yes`, which says so in the argv. Reading whatever
/// stdin happens to be turned `echo y | …` into a publish, and an inherited
/// idle pipe parked `read_line` for ever.
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
/// A blank command is off too, as it is for the app; a config that is not
/// TOML is UNREADABLE, also 2.
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

    // A command that is not a bridge is unreadable, not a broker to dial.
    s.write_config("[fabric]\ncommand = \"/usr/bin/false\"\n");
    let (code, out, _) = fabric(&s, None, &[]);
    assert_eq!(code, 2, "{out}");

    // `tail` refuses the same way.
    let (code, _, err) = fabric(&s, None, &["tail"]);
    assert_eq!(code, 2, "{err}");
}

/// A SOCKET FILE IS NOT A BROKER. A socket that was bound and abandoned — the
/// leftover of a broker that died — refuses the connect: not reachable, a
/// warning, exit 1. The config is read in the spelling `aterm fabric on`
/// writes.
#[test]
fn a_dead_socket_file_is_unreachable_and_a_warning() {
    let s = Scratch::new("dead");
    let sock = s.sock();
    // A DATAGRAM socket, never a dropped stream listener: macOS marks a socket
    // close-on-exec only after `socket()` returns, so a `posix_spawn` elsewhere in
    // this binary can inherit a listener and keep it accepting after the drop (the
    // `--tests` run of 2026-09-17 saw `the socket accepted but no broker answered`
    // instead of the refusal; the fixture then polled until the spawn let go). A
    // stream connect to a datagram socket's path is refused whoever holds it.
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
    // The next offset after the records published here, plus ONE: the guarded
    // broker binds the node's producer id the first time its `rw,p=` grant
    // attaches — the report's own probe — as one hidden `/a/bind` record.
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

    // The TEXT: the same exit, and still no body.
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
/// the report must not call that healthy, and must exit 1.
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
        // `fabric_cmd`'s std `Command` (it feeds `harness::output_within` too),
        // with the strip: the tail holds its stdio and nothing else of this
        // process for as long as it follows.
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

/// `aterm link broker <args>`, run to its exit: code, stdout, stderr.
fn broker_run(args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(harness::aterm_to_launch());
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

/// **A LOG THE BROKER REFUSES AS CORRUPT HAS A REPAIR THIS BINARY SHIPS, AND
/// DOCTOR PRESCRIBES IT** (audit 51). Two records are written, then a byte
/// inside the FIRST is flipped — not a torn tail, which astream truncates on
/// its own, but a corruption with decodable records behind it. The broker
/// refuses the log and its last stderr line names `aterm link broker --repair
/// <log>` (astream's own words name `asb repair`, which aterm does not
/// ship); with that stderr in `<root>/broker.err`, where the supervisor
/// appends it, `aterm fabric doctor` prescribes the repair
/// rather than a restart into the same refusal. The repair reports the bytes
/// it dropped, a second one finds nothing to repair, and the broker serves
/// the log again. A missing log is refused, not created.
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
    } // dropped: the log's exclusive lock is released
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
    // What the supervisor does with that stderr (the plist's
    // StandardErrorPath, the unit's StandardError=append:).
    std::fs::write(s.path("broker.err"), &err).expect("broker.err");
    let (_, doctor, _) = fabric(&s, Some(&s.command(&sock, true)), &["doctor"]);
    assert!(
        doctor
            .lines()
            .any(|l| l.trim_start().starts_with("fix:") && l.contains("broker --repair")),
        "doctor prescribes the repair:\n{doctor}"
    );

    // Execute the offered command with this build's front door. The owned
    // path contains a space and an apostrophe: the diagnostic must preserve
    // one shell operand, rather than merely contain the raw path's bytes.
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
