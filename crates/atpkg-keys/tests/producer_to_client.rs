// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE PRODUCER SCRIPT AND THE CLIENT VERIFIER, IN ONE PROCESS TREE.
//!
//! `crates/atpkg/src/sig.rs` proves what the client accepts. `owner_to_client.rs` proves
//! the same chain end to end through the shipping *types*. Neither of them runs
//! `tools/atpkg-index.sh`, and that gap is not academic: after the single-root move landed
//! on the client, the producer went on emitting `schema = 1` with a `[keys]` delegation
//! table signed by a retired root key — an index every new client refuses at the
//! attribution bind — and its own self-check stayed green throughout, because the check
//! called the OLD verifier arity. A pipeline that verifies with a different chain than the
//! client is a pipeline with no self-check at all.
//!
//! So this file drives the REAL producer:
//!
//!   1. `atpkg-keys setup` on a real pseudo-terminal, inside a scratch `$HOME` — a genuine
//!      paper master, a genuine machine key, a genuine master-signed roster. Nothing is
//!      hand-written and no key literal appears anywhere in this file (which is also what
//!      keeps it off `tools/grep_guard.sh`'s B7 exemption list).
//!   2. `bash tools/atpkg-index.sh` against that prefix — the actual shipping script, with
//!      the actual `atpkg-keys` binary doing the signing.
//!   3. `atpkg verify-index` over the emitted quad — the actual shipping client verifier,
//!      invoked the way `docs/ATPKG-KEY-MANAGEMENT.md` tells an operator to invoke it.
//!
//! Step 3 is deliberately run by the TEST as well as by the script. If the two ever
//! disagree about what the chain is, this file is what goes red.
//!
//! Everything after the happy path is a MUTATION: the whole retired document shape, the
//! attribution field, the signing key, and — for the mirror — the roster pair itself. Each
//! one is a change a well-meaning edit could make, and each must be caught by the
//! producer's own self-check rather than by a user discovering it. One of them also
//! records where the gate ISN'T: the schema number alone stops nothing, because
//! `SUPPORTED_SCHEMA` rejects only NEWER.
//!
//! No network: the mirror's GitHub calls are served by a stub `gh` on `PATH`.

#![cfg(unix)]

use std::io::Read as _;
use std::os::unix::io::FromRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

/// Far enough out that no fixture here can go stale, and unambiguously synthetic.
const FAR_FUTURE: &str = "2099-01-01T00:00:00Z";

/// The index build every fixture publishes at.
const INDEX_BUILD: &str = "7";

// ---------------------------------------------------------------------------
// Paths: the two real binaries, and the real scripts.
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/atpkg-keys sits two levels under the workspace root")
        .to_path_buf()
}

fn keys_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_atpkg-keys"))
}

/// The CLIENT binary. `CARGO_BIN_EXE_*` only covers the current package's own bins, and
/// `atpkg` is a path dependency rather than a member of this one — so it is found beside
/// `atpkg-keys` in the same profile directory, which is where a workspace-wide
/// `cargo test` puts it (atpkg's own integration tests force its bin to be built).
///
/// A missing binary is a hard failure naming the one command that fixes it, never a skip:
/// a self-check that quietly does not run is exactly the failure mode this file exists to
/// catch, and reproducing it here would be a joke at its own expense.
fn atpkg_bin() -> PathBuf {
    let p = keys_bin().with_file_name("atpkg");
    assert!(
        p.is_file(),
        "the client verifier is not built at {}.\n\
         This test drives the REAL producer against the REAL client, so it needs both:\n\
             cargo build -p atpkg --bin atpkg\n\
         (a workspace-wide `cargo test` builds it as a matter of course).",
        p.display()
    );
    p
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("atpkg-producer").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn s(p: &Path) -> String {
    p.to_str().expect("utf-8 path").to_string()
}

/// A `pins.rs` in the exact two shapes the real anchor file uses. `setup` arms the empty
/// one; `atpkg_master_pubkeys` in the producer library reads what it wrote back out.
fn unarmed_pins() -> &'static str {
    "// Copyright 2026 Andrew Yates\n\
     // SPDX-License-Identifier: Apache-2.0\n\
     \n\
     /// The paper master. Empty here means INERT.\n\
     pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\n\
     \n\
     /// The channel keyset. ORDER IS A CONTRACT: index 0 is the head.\n\
     pub const UPDATE_CHANNEL_PUBKEYS: &[&str] = &[\n\
     \x20   // K1 — HEAD.\n\
     \x20   \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\n\
     ];\n"
}

// ---------------------------------------------------------------------------
// A real controlling terminal, built rather than inherited.
//
// `atpkg-keys setup` refuses to run without one — it has a master to deliver and nowhere
// to deliver it (see provision_cli.rs, which proves that refusal). A test harness's own
// terminal is not a fixture, so this builds a pty pair and makes it the child's
// controlling terminal while stdout stays a pipe.
// ---------------------------------------------------------------------------

static PTY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Pty {
    master: std::fs::File,
    slave: String,
}

impl Pty {
    fn open() -> Self {
        // `ptsname(3)` writes into a static buffer, so the open-and-name sequence is
        // serialised rather than left to `cargo test`'s thread scheduling.
        let _guard = PTY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the POSIX pty-opening sequence, each call checked. The only pointer we
        // take is `ptsname`'s, and it is copied out before the lock is released.
        unsafe {
            let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(fd >= 0, "posix_openpt: {}", std::io::Error::last_os_error());
            assert_eq!(libc::grantpt(fd), 0, "grantpt");
            assert_eq!(libc::unlockpt(fd), 0, "unlockpt");
            let name = libc::ptsname(fd);
            assert!(!name.is_null(), "ptsname");
            let slave = std::ffi::CStr::from_ptr(name)
                .to_str()
                .expect("a pty path is ASCII")
                .to_string();
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            Self {
                master: std::fs::File::from_raw_fd(fd),
                slave,
            }
        }
    }

    fn spawn(&self, home: &Path, args: &[&str]) -> Child {
        let slave = std::ffi::CString::new(self.slave.clone()).expect("no NUL in a pty path");
        let mut cmd = Command::new(keys_bin());
        cmd.args(args)
            .env("HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: everything in the hook is async-signal-safe (`setsid`, `open`, `ioctl`)
        // and touches no state shared with the parent; `slave` lives in the closure.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let fd = libc::open(slave.as_ptr(), libc::O_RDWR);
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(fd, libc::c_ulong::from(libc::TIOCSCTTY), 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn().expect("the atpkg-keys binary runs")
    }

    fn drain(&mut self, into: &mut String) {
        let mut buf = [0u8; 4096];
        loop {
            match self.master.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => into.push_str(&String::from_utf8_lossy(&buf[..n])),
                // EAGAIN (nothing yet) and EIO (the child is gone) both mean stop.
                Err(_) => return,
            }
        }
    }

    /// Type a line at the terminal, the way a human would.
    fn type_line(&mut self, line: &str) {
        use std::io::Write as _;
        self.master
            .write_all(line.as_bytes())
            .and_then(|()| self.master.write_all(b"\n"))
            .and_then(|()| self.master.flush())
            .expect("typing at the terminal");
    }
}

/// `atpkg-keys setup --id <id>` inside `home`, on a terminal it does not have to inherit.
/// Returns the run's stdout, which carries the master's PUBLIC key (the phrase itself goes
/// to the terminal and nowhere else — proved in `provision_cli.rs`, relied on here).
fn run_setup(home: &Path, id: &str, pins: &Path, roster: &Path) -> String {
    let mut pty = Pty::open();
    let mut child = pty.spawn(
        home,
        &["setup", "--id", id, "--pins", &s(pins), "--roster", &s(roster)],
    );
    let mut terminal = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut retyped = false;
    loop {
        pty.drain(&mut terminal);
        // Answer the transcription gate: retype the phrase already on the terminal, as an
        // operator would from their paper.
        if !retyped && terminal.contains("retype the phrase FROM YOUR PAPER") {
            const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
            let phrase = terminal
                .lines()
                .map(str::trim)
                .find(|l| {
                    !l.is_empty()
                        && l.bytes().all(|b| b == b' ' || ALPHABET.contains(&b))
                        && l.bytes().filter(|b| *b != b' ').count() == 52
                })
                .expect("the phrase precedes the retype prompt")
                .to_string();
            pty.type_line(&phrase);
            retyped = true;
        }
        match child.try_wait().expect("wait on the child") {
            Some(_) => break,
            None => {
                assert!(
                    std::time::Instant::now() <= deadline,
                    "`atpkg-keys setup` never exited; terminal so far:\n{terminal}"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    let out = child.wait_with_output().expect("collect the child's output");
    assert!(
        out.status.success(),
        "setup must succeed on a terminal.\nstderr: {}\nterminal: {terminal}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---------------------------------------------------------------------------
// A provisioned publishing prefix: HOME, anchor, roster, machine key, spec.
// ---------------------------------------------------------------------------

struct Prefix {
    home: PathBuf,
    pins: PathBuf,
    roster: PathBuf,
    spec: PathBuf,
    out: PathBuf,
    counter: PathBuf,
    /// The paper master's public key — the ONE key a client pins, and therefore the only
    /// argument `verify-index` will accept.
    master_pub: String,
}

impl Prefix {
    /// Everything `atpkg-keys setup` writes, plus a two-program spec for the indexer.
    fn provision(label: &str) -> Self {
        let home = scratch(label);
        let pins = home.join("pins.rs");
        std::fs::write(&pins, unarmed_pins()).expect("unarmed anchor fixture");
        let roster = home.join("dist/aterm-machines.toml");

        let stdout = run_setup(&home, "m3", &pins, &roster);
        // `setup` announces the master's PUBLIC identity on stdout, right after arming the
        // anchor. Reading it from there (rather than re-deriving it) means the test uses
        // the same value an operator would copy.
        let master_pub = stdout
            .lines()
            .find_map(|l| l.trim().strip_prefix("master public key:"))
            .map(|k| k.trim().to_string())
            .unwrap_or_else(|| panic!("setup must announce the master public key:\n{stdout}"));

        assert!(roster.is_file() && roster.with_extension("toml.sig").is_file());
        assert!(home.join(".aterm/machine.key").is_file());
        assert!(home.join(".aterm/machine.toml").is_file());

        let spec = home.join("programs.spec");
        std::fs::write(&spec, "ay ay prebuilt-only 6255\nny ny prebuilt-or-build 12 rustc\n")
            .expect("program spec");

        Self {
            out: home.join("out"),
            counter: home.join("index_counter"),
            home,
            pins,
            roster,
            spec,
            master_pub,
        }
    }

    /// Run a producer script against this prefix. `script` is an absolute path, so a
    /// MUTATED copy can be substituted for the tracked one.
    fn run_indexer(&self, script: &Path, extra: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new("bash");
        cmd.arg(script)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("ATPKG", atpkg_bin())
            .env("ATPKG_KEYS", keys_bin())
            .env("PROGRAMS", &self.spec)
            .env("CHANNEL", "stable")
            .env("VALID_UNTIL", FAR_FUTURE)
            .env("INDEX_BUILD", INDEX_BUILD)
            .env("ALLOW_NO_BASELINE", "1")
            .env("OUT", &self.out)
            .env("ROSTER", &self.roster)
            .env("PINS_FILE", &self.pins)
            .env("INDEX_COUNTER_FILE", &self.counter)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().expect("bash runs the producer script")
    }

    /// THE DOCUMENTED INVOCATION, in the DEFAULT LAYOUT: a repo root holding
    /// `tools/atpkg-index.sh` and a `dist/` with the roster in it, run from that root with
    /// neither `OUT` nor `ROSTER` set — so `OUT` defaults to the relative `dist` and
    /// `ROSTER` to the absolute `<repo>/dist/aterm-machines.toml`.
    ///
    /// That combination is one file under two spellings, and it is the shape
    /// `docs/ATPKG-KEY-MANAGEMENT.md` tells an operator to use. `run_indexer` cannot reach
    /// it: it always passes an explicit `OUT` in a directory of its own, which is precisely
    /// why the default configuration was the one shape no test covered.
    ///
    /// Returns `(output, dist_dir)`.
    fn run_indexer_in_default_layout(&self, extra: &[(&str, &str)]) -> (Output, PathBuf) {
        let root = self.home.join("default-layout");
        let tools = root.join("tools");
        std::fs::create_dir_all(&tools).expect("fake repo tools dir");
        let dist = root.join("dist");
        std::fs::create_dir_all(&dist).expect("fake repo dist dir");
        for f in ["atpkg-index.sh", "atpkg-publish-lib.sh"] {
            std::fs::copy(repo_root().join("tools").join(f), tools.join(f)).expect("copy script");
        }
        // The roster lives in dist/, exactly as the key-management doc instructs.
        std::fs::copy(&self.roster, dist.join("aterm-machines.toml")).expect("roster");
        std::fs::copy(
            self.roster.with_extension("toml.sig"),
            dist.join("aterm-machines.toml.sig"),
        )
        .expect("roster sig");

        let mut cmd = Command::new("bash");
        cmd.arg("tools/atpkg-index.sh")
            .current_dir(&root)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("ATPKG", atpkg_bin())
            .env("ATPKG_KEYS", keys_bin())
            .env("PROGRAMS", &self.spec)
            .env("CHANNEL", "stable")
            .env("VALID_UNTIL", FAR_FUTURE)
            .env("INDEX_BUILD", INDEX_BUILD)
            .env("ALLOW_NO_BASELINE", "1")
            .env("PINS_FILE", &self.pins)
            .env("INDEX_COUNTER_FILE", &self.counter)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in extra {
            cmd.env(k, v);
        }
        (cmd.output().expect("bash runs the producer script"), dist)
    }

    /// THE CLIENT'S OWN VERIFIER, over four assets in an arbitrary directory.
    fn client_verify_dir(&self, dir: &Path) -> Output {
        let roster = dir.join("aterm-machines.toml");
        Command::new(atpkg_bin())
            .args([
                "verify-index",
                &self.master_pub,
                &s(&dir.join("index.toml")),
                &s(&dir.join("index.toml.sig")),
                &s(&roster),
                &s(&roster.with_extension("toml.sig")),
            ])
            .output()
            .expect("the atpkg binary runs")
    }

    /// A roster with `id` withdrawn EXACTLY the way `roster_ops::revoke` does it: the
    /// `[[machine]]` block removed *and* the id named in `revoked`. The signature is left
    /// as-is and that is deliberate — the gate under test is a producer-side pre-check that
    /// runs on the file as given, before any crypto, so re-signing would test nothing extra
    /// and would need the paper master.
    ///
    /// Written as a transform of the REAL roster rather than as a fixture, so it cannot
    /// drift from the shape the tool actually emits.
    fn roster_with_revoked(&self, id: &str) -> PathBuf {
        let src = std::fs::read_to_string(&self.roster).expect("read the roster");
        let (head, blocks) = src
            .split_once("[[machine]]")
            .expect("the roster lists at least one machine");
        let kept: String = blocks
            .split("[[machine]]")
            .filter(|b| !b.contains(&format!("id = \"{id}\"")))
            .map(|b| format!("[[machine]]{b}"))
            .collect();
        assert!(
            !kept.contains(&format!("id = \"{id}\"")),
            "revoke must REMOVE the machine block, not just deny it"
        );
        assert!(
            head.contains("revoked = []"),
            "fixture assumes the provisioned roster starts with an empty deny-list"
        );
        let out = self.home.join(format!("roster-revoked-{id}.toml"));
        std::fs::write(
            &out,
            format!("{}{kept}", head.replace("revoked = []", &format!("revoked = [\"{id}\"]"))),
        )
        .expect("write the revoked roster");
        std::fs::copy(
            self.roster.with_extension("toml.sig"),
            out.with_extension("toml.sig"),
        )
        .expect("carry the signature across");
        out
    }

    /// THE CLIENT'S OWN VERIFIER, over the four assets the producer staged for publication.
    /// Exactly the invocation `docs/ATPKG-KEY-MANAGEMENT.md` gives an operator.
    fn client_verify(&self) -> Output {
        let roster = self.out.join("aterm-machines.toml");
        Command::new(atpkg_bin())
            .args([
                "verify-index",
                &self.master_pub,
                &s(&self.out.join("index.toml")),
                &s(&self.out.join("index.toml.sig")),
                &s(&roster),
                &s(&roster.with_extension("toml.sig")),
            ])
            .output()
            .expect("the atpkg binary runs")
    }
}

fn text(o: &Output) -> String {
    let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
    t.push_str(&String::from_utf8_lossy(&o.stderr));
    t
}

/// A copy of the producer (and the library it sources) with substitutions applied — the
/// mutation harness. The copy keeps the `tools/` layout because the script derives its repo
/// root from its own location and sources the library from there.
///
/// Every substitution target is asserted present before it is applied. A mutation that
/// silently matched nothing would leave a test that passes because it tested the unmutated
/// script, which is the most expensive kind of green.
fn mutated_indexer(prefix: &Prefix, label: &str, subs: &[(&str, &str)]) -> PathBuf {
    let tools = prefix.home.join("mutants").join(label).join("tools");
    std::fs::create_dir_all(&tools).expect("mutant tools dir");
    let real = repo_root().join("tools");
    std::fs::copy(
        real.join("atpkg-publish-lib.sh"),
        tools.join("atpkg-publish-lib.sh"),
    )
    .expect("copy the publish library");
    let mut src = std::fs::read_to_string(real.join("atpkg-index.sh")).expect("read the producer");
    for (from, to) in subs {
        assert!(
            src.contains(from),
            "the mutation target {from:?} is not in tools/atpkg-index.sh any more — this \
             mutation would be a no-op, which is worse than a failing test"
        );
        src = src.replace(from, to);
    }
    let dst = tools.join("atpkg-index.sh");
    std::fs::write(&dst, src).expect("write the mutant");
    dst
}

fn tracked_indexer() -> PathBuf {
    repo_root().join("tools/atpkg-index.sh")
}

// ===========================================================================
// THE HAPPY PATH: the real script, the real client, one quad.
// ===========================================================================

/// The proof this file exists for. A `setup`-minted identity, the tracked
/// `tools/atpkg-index.sh`, and the shipping `atpkg verify-index` over the exact four
/// assets the script staged for the release.
///
/// It kills the whole retired producer at once: emit `schema = 1`, drop `machine_id`, keep
/// a `[keys]` table, sign with anything other than the rostered machine key, or publish
/// without the roster pair, and one of the assertions below fails.
#[test]
fn the_real_producer_emits_a_quad_the_real_client_accepts() {
    let prefix = Prefix::provision("happy-path");

    let out = prefix.run_indexer(&tracked_indexer(), &[]);
    assert!(
        out.status.success(),
        "the tracked producer must run clean against a provisioned prefix:\n{}",
        text(&out)
    );
    // The script ran the client chain itself, and said which machine it published as.
    let log = text(&out);
    assert!(
        log.contains("self-check — OK: index build 7 signed by machine m3 under roster seq"),
        "the producer must self-verify with the CLIENT's verdict, verbatim:\n{log}"
    );

    // ALL FOUR ASSETS ARE STAGED. Three of them would be a release that authorizes
    // nothing: a client builds one candidate out of a release and will not fetch a roster
    // from anywhere else.
    for asset in [
        "index.toml",
        "index.toml.sig",
        "aterm-machines.toml",
        "aterm-machines.toml.sig",
    ] {
        assert!(
            prefix.out.join(asset).is_file(),
            "the publish set must carry {asset}; it has {:?}",
            std::fs::read_dir(&prefix.out)
                .map(|d| d.filter_map(Result::ok).map(|e| e.file_name()).collect::<Vec<_>>())
        );
    }
    // The staged roster is the roster, byte for byte — a re-serialization would be a
    // document the master did not sign.
    assert_eq!(
        std::fs::read(prefix.out.join("aterm-machines.toml")).unwrap(),
        std::fs::read(&prefix.roster).unwrap(),
        "the roster must travel unmodified"
    );

    // THE EMITTED BYTES, checked against what the client's parser and bind require.
    let index = std::fs::read_to_string(prefix.out.join("index.toml")).unwrap();
    assert!(index.contains("\nschema = 2\n"), "{index}");
    assert!(index.contains("\nmachine_id = \"m3\"\n"), "{index}");
    assert!(index.contains("\nroster_seq = "), "{index}");
    assert!(
        !index.contains("[keys]") && !index.contains("release_key_pubkey"),
        "the delegation tier is retired; a producer must not still emit it:\n{index}"
    );
    // The `roster_seq` stamped into the signed bytes is the one the roster actually
    // carries — the pairing the client's bind enforces from both directions.
    let roster = std::fs::read_to_string(&prefix.roster).unwrap();
    let seq = roster
        .lines()
        .find_map(|l| l.trim().strip_prefix("roster_seq = "))
        .expect("the roster states its sequence");
    assert!(index.contains(&format!("\nroster_seq = {seq}\n")), "{index}");

    // AND THE CLIENT AGREES — run independently of the script, so the two cannot pass by
    // agreeing with each other.
    let verified = prefix.client_verify();
    assert!(
        verified.status.success(),
        "the shipping client verifier must accept the published quad:\n{}",
        text(&verified)
    );
    assert!(
        text(&verified).contains("signed by machine m3"),
        "{}",
        text(&verified)
    );
}

// ===========================================================================
// MUTATIONS. Each is a plausible edit; each must be caught by the producer.
// ===========================================================================

/// (a) THE RETIRED PRODUCER, RESTORED. A merge that brings back the old emitter is the
/// regression this whole file exists for: `schema = 1`, a `[keys]` delegation table, and no
/// attribution pair. Every client refuses it, so the producer must refuse to finish.
///
/// WHERE THE GATE ACTUALLY IS, because it is not where one would guess. The schema number
/// alone does NOT stop this: `SUPPORTED_SCHEMA` is a reject-NEWER gate, so a schema-1
/// document parses perfectly well in a schema-2 client (1 ≤ 2) — an index that said
/// `schema = 1` but still carried `machine_id`/`roster_seq` would verify. What kills the
/// retired shape is the ATTRIBUTION BIND: schema 1 had nowhere to put a machine id, and an
/// index nobody can be held to is `Reject::Unattributed`. So this test mutates the whole
/// shape rather than the version number, and the `schema = 2` byte-level assertion lives in
/// the happy path, where a byte-level claim belongs.
#[test]
fn the_retired_index_shape_is_refused_by_the_self_check() {
    let prefix = Prefix::provision("mutant-retired-shape");
    let mutant = mutated_indexer(
        &prefix,
        "retired-shape",
        &[
            ("echo \"schema = 2\"", "echo \"schema = 1\""),
            // The attribution pair out, the delegation table back in — the exact document
            // the pre-fold script emitted (minus the release pubkey, which is a key literal
            // this tree does not fabricate and which changes no verdict).
            (
                "\techo \"machine_id = \\\"$MACHINE_ID\\\"\"\n\techo \"roster_seq = $ROSTER_SEQ\"\n",
                "\techo \"\"\n\techo \"[keys]\"\n\techo \"release_key_id = \\\"rk-$INDEX_BUILD\\\"\"\n",
            ),
        ],
    );

    let out = prefix.run_indexer(&mutant, &[]);
    assert!(
        !out.status.success(),
        "the retired index shape must not survive the self-check:\n{}",
        text(&out)
    );
    let log = text(&out);
    assert!(log.contains("SELF-CHECK FAILED"), "{log}");
    assert!(
        log.contains("refusing to publish an index no client would accept"),
        "{log}"
    );
    // The bytes written really are the retired shape, so the refusal is about the document
    // and not about a script that died before emitting one.
    let index = std::fs::read_to_string(prefix.out.join("index.toml")).unwrap();
    assert!(index.contains("\nschema = 1\n") && index.contains("[keys]"), "{index}");
    assert!(!index.contains("machine_id"), "{index}");
    // And the client refuses those same bytes for itself.
    assert!(!prefix.client_verify().status.success());
}

/// (b) NO `machine_id`. An index nobody can be held to. The field lives inside the signed
/// bytes precisely so a genuine signature cannot be relabelled, and its absence is
/// `Reject::Unattributed` — never a pass.
#[test]
fn a_producer_that_drops_machine_id_fails_its_own_self_check() {
    let prefix = Prefix::provision("mutant-no-machine-id");
    let mutant = mutated_indexer(
        &prefix,
        "no-machine-id",
        &[("\techo \"machine_id = \\\"$MACHINE_ID\\\"\"\n", "")],
    );

    let out = prefix.run_indexer(&mutant, &[]);
    assert!(
        !out.status.success(),
        "an unattributed index must not survive the self-check:\n{}",
        text(&out)
    );
    assert!(text(&out).contains("SELF-CHECK FAILED"), "{}", text(&out));
    let index = std::fs::read_to_string(prefix.out.join("index.toml")).unwrap();
    assert!(!index.contains("machine_id"), "{index}");
    assert!(index.contains("\nschema = 2\n"), "the ONLY change is the missing id: {index}");
    assert!(!prefix.client_verify().status.success());
}

/// (d) SIGNED BY A KEY NOBODY ROSTERED. The signature is mathematically perfect and the
/// document is otherwise flawless — and no machine on the roster holds that key, so the
/// client authorizes nothing.
///
/// The mutation goes at the SIGNING LINE rather than at the identity, deliberately: the
/// script's pre-checks (below) catch a wrong identity long before any bytes are written,
/// so mutating the identity would prove only that the pre-check works. Cutting the key out
/// from under an otherwise-correct run is what proves the self-check is a real backstop.
#[test]
fn an_index_signed_by_an_unrostered_key_fails_the_self_check() {
    let prefix = Prefix::provision("mutant-outsider-key");

    // A real Ed25519 key that no roster has ever heard of. Generated in-process:
    // the standalone `keygen` verb is deleted (a generator for keys the roster
    // cannot account for was cruft), and this test needs the KEY, not the verb.
    let outsider = prefix.home.join("outsider.key");
    let (outsider_pkcs8, _) = atpkg_keys::generate().expect("keypair");
    std::fs::write(&outsider, &outsider_pkcs8).unwrap();

    let mutant = mutated_indexer(
        &prefix,
        "outsider-key",
        &[(
            "\"$ATPKG_KEYS\" sign \"$MACHINE_KEY\" \"$INDEX\" \"$INDEX.sig\"",
            "\"$ATPKG_KEYS\" sign \"$OUTSIDER_KEY\" \"$INDEX\" \"$INDEX.sig\"",
        )],
    );
    let out = prefix.run_indexer(&mutant, &[("OUTSIDER_KEY", &s(&outsider))]);

    assert!(
        !out.status.success(),
        "an index signed off-roster must not survive the self-check:\n{}",
        text(&out)
    );
    assert!(text(&out).contains("SELF-CHECK FAILED"), "{}", text(&out));
    // The index BODY is the correct one — schema 2, attributed to m3. Only the signature
    // is wrong, which is exactly the case a byte-level check of the emitted file misses
    // and only the real verifier catches.
    let index = std::fs::read_to_string(prefix.out.join("index.toml")).unwrap();
    assert!(index.contains("\nschema = 2\n") && index.contains("machine_id = \"m3\""), "{index}");
    assert!(!prefix.client_verify().status.success());
}

/// THE PRE-CHECKS, which exist so the common mistakes do not have to be diagnosed from an
/// opaque end-of-run refusal. A verifier must not be a verification oracle (§8), so
/// `verify-index`'s failure says only "no machine on that roster signed this index" — true,
/// and useless to an operator whose real problem is that their machine was never added.
///
/// Each refusal must name the exact command that fixes it, and none may write an index.
#[test]
fn a_machine_the_roster_does_not_name_is_refused_before_anything_is_signed() {
    let prefix = Prefix::provision("unrostered-identity");

    // A second, internally CONSISTENT identity: its own key, its own record naming that
    // key. Nothing is wrong with it except that no roster lists it.
    let stranger_key = prefix.home.join("stranger.key");
    let (stranger_pkcs8, stranger_pub) = atpkg_keys::generate().expect("keypair");
    std::fs::write(&stranger_key, &stranger_pkcs8).unwrap();
    let stranger_rec = prefix.home.join("stranger.toml");
    std::fs::write(
        &stranger_rec,
        format!("id = \"m99\"\npubkey = \"{stranger_pub}\"\nminted_at = \"2026-08-04T00:00:00Z\"\n"),
    )
    .expect("stranger record");

    let out = prefix.run_indexer(
        &tracked_indexer(),
        &[
            ("MACHINE_KEY", &s(&stranger_key)),
            ("MACHINE_PUB", &s(&stranger_rec)),
        ],
    );
    assert!(!out.status.success(), "{}", text(&out));
    let log = text(&out);
    assert!(log.contains("is NOT on the roster"), "{log}");
    assert!(
        log.contains("atpkg-keys join --id m99"),
        "the refusal must name the command that fixes it: {log}"
    );
    assert!(
        !prefix.out.join("index.toml").exists(),
        "a refused run must not leave a signed index behind"
    );

    // A MISSING key, and a MISSING roster, refuse the same way: with the mint command.
    let out = prefix.run_indexer(
        &tracked_indexer(),
        &[("MACHINE_KEY", &s(&prefix.home.join("nope.key")))],
    );
    assert!(!out.status.success());
    assert!(text(&out).contains("atpkg-keys setup --id"), "{}", text(&out));
    assert!(text(&out).contains("atpkg-keys join"), "{}", text(&out));

    let out = prefix.run_indexer(
        &tracked_indexer(),
        &[("ROSTER", &s(&prefix.home.join("no-such-roster.toml")))],
    );
    assert!(!out.status.success());
    assert!(
        text(&out).contains("no master-signed roster pair"),
        "{}",
        text(&out)
    );
    assert!(
        text(&out).contains("Never start a second roster"),
        "{}",
        text(&out)
    );

    // NON-VACUITY: the same prefix, with its OWN identity, publishes fine.
    assert!(prefix.run_indexer(&tracked_indexer(), &[]).status.success());
}

/// AN UNARMED TREE PUBLISHES NOTHING. Exercised with a synthetic empty anchor (the
/// shipped tree has been armed since 2026-08-15): an empty anchor means the producer
/// cannot self-check against anything — so it must refuse rather than sign into the void.
///
/// This is the same fail-closed direction the client asserts from the other end
/// (`sig.rs::an_unarmed_anchor_is_inert_and_authorizes_nothing`), stated where an operator
/// would otherwise meet it: at publish time.
#[test]
fn an_unarmed_anchor_stops_the_publish_rather_than_signing_into_the_void() {
    let prefix = Prefix::provision("unarmed-anchor");
    let unarmed = prefix.home.join("unarmed-pins.rs");
    std::fs::write(&unarmed, unarmed_pins()).expect("an empty anchor");

    let out = prefix.run_indexer(&tracked_indexer(), &[("PINS_FILE", &s(&unarmed))]);
    assert!(!out.status.success(), "{}", text(&out));
    let log = text(&out);
    assert!(log.contains("PAPER_MASTER_PUBKEYS is EMPTY"), "{log}");
    assert!(log.contains("atpkg-keys setup --id"), "{log}");
}

// ===========================================================================
// (c) THE MIRROR. A public release without the roster authorizes nothing.
// ===========================================================================

/// A stub `gh` that serves releases out of a fixture directory, so the mirror's real
/// control flow runs with no network and no credentials. It implements exactly the two
/// call shapes the mirror makes before it would need either.
fn stub_gh(dir: &Path, fixtures: &Path) -> PathBuf {
    let bin = dir.join("stub-bin");
    std::fs::create_dir_all(&bin).expect("stub bin dir");
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/usr/bin/env bash\n\
             # Test double for `gh`: serves release assets from a fixture tree.\n\
             set -uo pipefail\n\
             FIX=\"{fixtures}\"\n\
             if [[ \"${{1:-}}\" == release && \"${{2:-}}\" == download ]]; then\n\
             \ttag=\"$3\"; shift 3; dir=\"\"; pats=()\n\
             \twhile [[ $# -gt 0 ]]; do\n\
             \t\tcase \"$1\" in\n\
             \t\t\t-D) dir=\"$2\"; shift 2 ;;\n\
             \t\t\t-R) shift 2 ;;\n\
             \t\t\t-p) pats+=(\"$2\"); shift 2 ;;\n\
             \t\t\t*) shift ;;\n\
             \t\tesac\n\
             \tdone\n\
             \t[[ -d \"$FIX/$tag\" ]] || {{ echo \"stub gh: no release $tag\" >&2; exit 1; }}\n\
             \tmkdir -p \"$dir\"\n\
             \tif [[ ${{#pats[@]}} -eq 0 ]]; then\n\
             \t\tcp \"$FIX/$tag\"/* \"$dir\"/ 2>/dev/null || true\n\
             \telse\n\
             \t\tfor p in \"${{pats[@]}}\"; do [[ -e \"$FIX/$tag/$p\" ]] && cp \"$FIX/$tag/$p\" \"$dir\"/; done\n\
             \tfi\n\
             \texit 0\n\
             fi\n\
             exit 1\n",
            fixtures = fixtures.display()
        ),
    )
    .expect("write the stub");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
    bin
}

fn run_mirror(prefix: &Prefix, stub_bin: &Path) -> Output {
    let path = format!(
        "{}:{}",
        stub_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new("bash")
        .arg(repo_root().join("tools/atpkg-mirror-public.sh"))
        .env_clear()
        .env("PATH", path)
        .env("HOME", &prefix.home)
        .env("ATPKG", atpkg_bin())
        .env("PINS_FILE", &prefix.pins)
        .env("INDEX_BUILD", INDEX_BUILD)
        .env("SRC_ACCOUNT", "synthetic-src")
        .env("DST_ACCOUNT", "synthetic-dst")
        .env("INDEX_REPO", "aterm")
        .env("DRY_RUN", "1")
        .env("DST_TOKEN_FILE", prefix.home.join("no-token"))
        .output()
        .expect("bash runs the mirror")
}

/// THE MIRROR MUST CARRY THE ROSTER, AND MUST SAY SO WHEN IT CANNOT.
///
/// A mirror that dropped the pair would publish a public release that yields the client no
/// candidate at all — every install and every update resolving `NoIndex`, with the mirror
/// reporting success. So: with the quad present the mirror verifies it through the real
/// client chain and moves on to the packages; with the pair removed it stops dead, names
/// the missing asset, and says what to re-run.
#[test]
fn the_mirror_refuses_a_source_release_that_lost_its_roster() {
    let prefix = Prefix::provision("mirror-roster");
    assert!(prefix.run_indexer(&tracked_indexer(), &[]).status.success());

    // The staged publish set IS the source release.
    let fixtures = prefix.home.join("releases");
    let tag_dir = fixtures.join(format!("atpkg-index-{INDEX_BUILD}"));
    std::fs::create_dir_all(&tag_dir).expect("fixture release dir");
    for asset in [
        "index.toml",
        "index.toml.sig",
        "aterm-machines.toml",
        "aterm-machines.toml.sig",
    ] {
        std::fs::copy(prefix.out.join(asset), tag_dir.join(asset)).expect("stage the asset");
    }
    let stub = stub_gh(&prefix.home, &fixtures);

    // POSITIVE CONTROL FIRST, so the refusal below is about the roster and not about the
    // harness. With all four assets the mirror verifies the quad through the client chain
    // and proceeds to the packages, where the stub has no fixture — a LATER, different
    // failure.
    let ok = run_mirror(&prefix, &stub);
    let log = text(&ok);
    assert!(
        log.contains("quad verified through the client chain")
            && log.contains("signed by machine m3"),
        "the mirror must re-verify with the client's own chain:\n{log}"
    );
    assert!(
        log.contains("stub gh: no release atpkg-ay-6255"),
        "the positive control must get past the roster gate and fail at the packages:\n{log}"
    );

    // NOW DROP THE PAIR. Nothing else changes.
    std::fs::remove_file(tag_dir.join("aterm-machines.toml")).unwrap();
    std::fs::remove_file(tag_dir.join("aterm-machines.toml.sig")).unwrap();

    let out = run_mirror(&prefix, &stub);
    assert!(!out.status.success(), "{}", text(&out));
    let log = text(&out);
    assert!(log.contains("missing aterm-machines.toml"), "{log}");
    assert!(log.contains("REFUSING to mirror"), "{log}");
    assert!(
        log.contains("NO candidate at all"),
        "the refusal must say what the damage would be: {log}"
    );
    assert!(
        log.contains("tools/atpkg-index.sh"),
        "and how to fix it: {log}"
    );
    assert!(
        !log.contains("quad verified through the client chain"),
        "it must stop BEFORE claiming anything was verified: {log}"
    );
}

// ===========================================================================
// THE GATES THE SELF-CHECK CANNOT PROVIDE.
//
// `atpkg verify-index` is not the client's whole gate set, and the two places it stops
// short are both places a producer can publish something the fleet refuses while printing
// "self-check — OK":
//
//   * `cmd_verify_index` runs `admit_roster` + `authorize_index` and returns. The index's
//     own freshness gate (`sig::check_freshness`) is applied by the client AFTER
//     verification, so the verb never reaches it.
//   * it builds `Anchor::of(vec![master], 0)` — roster floor ZERO — whereas a real client
//     uses `Anchor::pinned(Floor::new(layout.roster_floor()).current())` and ratchets that
//     floor after every use.
//
// Each test below drives the tracked script and asserts the refusal happens BEFORE
// anything is signed, because "refused after the tag was consumed" is a different and much
// more expensive outcome than "refused".
// ===========================================================================

/// THE DEFAULT LAYOUT PUBLISHES. Highest-ranked defect in this file's history: with the
/// script's own defaults the roster and the staged roster are ONE FILE under two spellings
/// (`OUT=dist` relative, `ROSTER=<repo>/dist/…` absolute). The staging step compared the
/// two by STRING, so it tried to `cp` a file onto itself, `cp` exited 1, and `set -e` killed
/// the run — AFTER the index was written and Ed25519-signed, and BEFORE the self-check.
///
/// The operator-visible shape of that bug is what makes it worth a dedicated test: a `dist/`
/// holding a complete-looking, freshly signed, NEVER self-verified quad, under a diagnostic
/// (`cp: … are identical (not copied).`) that reads like a benign notice — next to the
/// `gh release create` fallback the script itself prints. The hand-upload it invites skips
/// the client verifier entirely.
#[test]
fn the_documented_default_layout_publishes_and_self_verifies() {
    let prefix = Prefix::provision("default-layout");
    let (out, dist) = prefix.run_indexer_in_default_layout(&[]);
    let log = text(&out);

    assert!(
        out.status.success(),
        "the DEFAULT invocation — the one the docs give — must complete:\n{log}"
    );
    // It must have gone all the way through the self-check, not merely not-crashed.
    assert!(
        log.contains("self-check — OK: index build 7 signed by machine m3 under roster seq"),
        "the run must reach the client-chain self-check:\n{log}"
    );
    assert!(
        log.contains("staged in place, no copy needed"),
        "same-file staging must be recognised as such, not attempted:\n{log}"
    );
    assert!(
        !log.contains("not copied"),
        "the self-copy must never be attempted:\n{log}"
    );

    // All four assets, and the client agrees about them — run independently of the script.
    for asset in [
        "index.toml",
        "index.toml.sig",
        "aterm-machines.toml",
        "aterm-machines.toml.sig",
    ] {
        assert!(dist.join(asset).is_file(), "{asset} missing from {dist:?}");
    }
    assert!(
        prefix.client_verify_dir(&dist).status.success(),
        "{}",
        text(&prefix.client_verify_dir(&dist))
    );
    // And the roster is still the roster: staging in place must not have rewritten it.
    assert_eq!(
        std::fs::read(dist.join("aterm-machines.toml")).unwrap(),
        std::fs::read(&prefix.roster).unwrap()
    );
}

/// A LAPSED OR UNREADABLE HORIZON IS REFUSED BEFORE ANYTHING IS SIGNED.
///
/// `atpkg verify-index` does not check the index's `valid_until` at all, so this cannot be
/// delegated to the self-check: an index that expired in 2020 self-checked GREEN and
/// uploaded. An unreadable stamp is not the milder case — `sig.rs` treats what it cannot
/// parse as LAPSED, not as absent, so `valid_until = "next tuesday"` is equally dead.
///
/// The refusal must land before the signature, because the upload consumes a tag and bumps
/// the monotonic counter: discovering this afterwards costs a build number.
#[test]
fn a_lapsed_or_malformed_horizon_never_reaches_a_signature() {
    let prefix = Prefix::provision("horizon");

    for (label, stamp) in [
        ("already lapsed", "2020-01-01T00:00:00Z"),
        ("not a date at all", "not-a-date"),
        // Right date, wrong SHAPE — no time, no Z. The client cannot parse it, so it is
        // lapsed; a producer comparing strings would have called it "far future".
        ("date only, no time or Z", "2099-12-31"),
    ] {
        let out = prefix.run_indexer(&tracked_indexer(), &[("VALID_UNTIL", stamp)]);
        let log = text(&out);
        assert!(
            !out.status.success(),
            "{label} ({stamp}) must be refused:\n{log}"
        );
        assert!(
            log.contains("LAPSED") || log.contains("not UTC RFC3339"),
            "{label}: the refusal must name the freshness gate:\n{log}"
        );
        assert!(
            !prefix.out.join("index.toml").exists(),
            "{label}: refused BEFORE signing — nothing may be left on disk"
        );
        assert!(
            !log.contains("self-check — OK"),
            "{label}: it must never claim a green self-check:\n{log}"
        );
    }

    // NON-VACUITY: a well-formed future horizon still publishes.
    let ok = prefix.run_indexer(&tracked_indexer(), &[]);
    assert!(ok.status.success(), "{}", text(&ok));
    assert!(text(&ok).contains("self-check — OK"), "{}", text(&ok));
}

/// THE ROSTER GENERATION IS A NO-DOWNGRADE FLOOR TOO.
///
/// The baseline already floored `index_build`; the roster's own `roster_seq` is the second
/// monotonic counter clients ratchet, and nothing was comparing it. The self-check cannot:
/// the verb's anchor has roster floor 0, so it admits ANY generation, and the index<->roster
/// bind only proves the two agree with each other — a genuinely stale pair agrees perfectly.
///
/// The damage is a publish that is a silent no-op (`select.rs` PASS 2 admits only the newest
/// generation on offer, so the index reaches nobody while producer and mirror both report
/// success), and later an outage plus the resurrection of anyone revoked in the meantime.
#[test]
fn a_roster_older_than_the_published_generation_is_refused() {
    let prefix = Prefix::provision("roster-floor");

    let seq = std::fs::read_to_string(&prefix.roster)
        .unwrap()
        .lines()
        .find_map(|l| l.trim().strip_prefix("roster_seq = "))
        .expect("the roster states its sequence")
        .parse::<u32>()
        .expect("a numeric sequence");

    // A baseline claiming the fleet already ratcheted well past this machine's roster.
    let baseline = |s: &str, n: u32| {
        let p = prefix.home.join(format!("baseline-{s}.toml"));
        std::fs::write(
            &p,
            format!(
                "schema = 2\nindex_build = 6\nvalid_until = \"{FAR_FUTURE}\"\n\
                 machine_id = \"m3\"\nroster_seq = {n}\n\n\
                 [programs.ay]\nrepo = \"ay\"\npolicy = \"prebuilt-only\"\n\n\
                 [[channels]]\nname = \"stable\"\nchannel_build = 6\nmin_build = 0\n\
                 pin = {{ ay = 6255, ny = 12 }}\n"
            ),
        )
        .expect("baseline fixture");
        p
    };

    let ahead = baseline("ahead", seq + 7);
    let out = prefix.run_indexer(&tracked_indexer(), &[("BASELINE", &s(&ahead))]);
    let log = text(&out);
    assert!(!out.status.success(), "a stale roster must be refused:\n{log}");
    assert!(
        log.contains(&format!("REFUSING roster_seq {seq} < baseline's {}", seq + 7)),
        "the refusal must name both generations:\n{log}"
    );
    assert!(
        log.contains("silent no-op"),
        "and say why it matters — the failure mode is success-shaped:\n{log}"
    );
    assert!(
        log.contains("Copy the CURRENT"),
        "and name the remedy; the roster is gitignored, so a pull does not fix it:\n{log}"
    );
    assert!(
        !prefix.out.join("index.toml").exists(),
        "refused before signing"
    );

    // NON-VACUITY, and the case that must NOT be refused: reusing the SAME generation is
    // what almost every publish does. Only a strict decrease is a rollback.
    let level = baseline("level", seq);
    let ok = prefix.run_indexer(&tracked_indexer(), &[("BASELINE", &s(&level))]);
    assert!(
        ok.status.success(),
        "republishing under the same roster generation is normal, not a rollback:\n{}",
        text(&ok)
    );
    assert!(text(&ok).contains("self-check — OK"), "{}", text(&ok));
}

/// A BASELINE FROM BEFORE THE ROSTER SCHEME CANNOT BE FLOORED, AND MUST SAY SO.
///
/// The first index published under the roster scheme has a schema-1 baseline with no
/// `roster_seq` — a real transition state, so it cannot be fatal. But it is also exactly
/// what a wrong-file baseline looks like, and this is the gate that would otherwise have
/// caught that, so the run must not pass in silence.
#[test]
fn a_baseline_with_no_roster_generation_warns_rather_than_flooring_silently() {
    let prefix = Prefix::provision("roster-floor-absent");
    let baseline = prefix.home.join("baseline-schema1.toml");
    std::fs::write(
        &baseline,
        format!(
            "schema = 1\nindex_build = 6\nvalid_until = \"{FAR_FUTURE}\"\n\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 6\nmin_build = 0\n\
             pin = {{ ay = 6255, ny = 12 }}\n"
        ),
    )
    .expect("pre-roster baseline");

    let out = prefix.run_indexer(&tracked_indexer(), &[("BASELINE", &s(&baseline))]);
    let log = text(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("BASELINE has no 'roster_seq' line"),
        "the missing floor must be announced, not assumed benign:\n{log}"
    );
    assert!(log.contains("self-check — OK"), "{log}");
}

/// A REVOKED MACHINE IS TOLD IT IS REVOKED — not told to re-join.
///
/// `roster_ops::revoke` REMOVES the `[[machine]]` block as well as naming the id in
/// `revoked`, so a revoked machine has no key on the roster to find. A producer that tested
/// "listed AND denied" therefore never fired on a roster this toolchain produces, and fell
/// through to the never-added branch — whose remedy is `atpkg-keys join --id <id>`: a
/// command that needs the offline paper master and that `roster_ops` then refuses outright,
/// because a revoked id never returns. Wrong diagnosis, and a remedy that costs a
/// paper-master session to discover is impossible.
#[test]
fn a_revoked_machine_is_diagnosed_as_revoked_and_not_sent_to_rejoin() {
    let prefix = Prefix::provision("revoked");
    let revoked = prefix.roster_with_revoked("m3");

    let out = prefix.run_indexer(&tracked_indexer(), &[("ROSTER", &s(&revoked))]);
    let log = text(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains("is REVOKED on this roster"),
        "the revoked shape the tool actually produces must reach the revoked branch:\n{log}"
    );
    assert!(
        !log.contains("is NOT on the roster"),
        "it must NOT be diagnosed as a machine that was never added:\n{log}"
    );
    assert!(
        log.contains("NEVER returns"),
        "and must say that re-joining is not the fix:\n{log}"
    );
    assert!(
        log.contains("join --id <new-machine-id>"),
        "the remedy is a NEW id, not the revoked one:\n{log}"
    );
    assert!(!prefix.out.join("index.toml").exists());

    // NON-VACUITY: the same machine against the unrevoked roster publishes.
    assert!(prefix.run_indexer(&tracked_indexer(), &[]).status.success());
}

/// NO `$HOME` IN ANYTHING THE OPERATOR WILL PASTE.
///
/// These scripts publish to a PUBLIC repo, and their output is routinely pasted into
/// release notes and issues. A machine id or a public key in a log line is already in the
/// signed bytes and is fine; an absolute path under `$HOME` carries the operator's account
/// name and is in no signed document. The counter line was the leak that mattered most —
/// it is the LAST line of every successful upload.
#[test]
fn a_successful_publish_prints_no_absolute_home_path() {
    let prefix = Prefix::provision("no-home-leak");
    // Put the counter and the output under $HOME, so there is something to leak; and drive
    // the upload path, since the counter line only prints after a successful release.
    let bin = prefix.home.join("gh-stub");
    std::fs::create_dir_all(&bin).expect("stub dir");
    std::fs::write(bin.join("gh"), "#!/usr/bin/env bash\nexit 0\n").expect("stub gh");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755)).unwrap();

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let counter = prefix.home.join(".config/atpkg/index_build");
    std::fs::create_dir_all(counter.parent().unwrap()).expect("counter dir");
    std::fs::write(&counter, "20\n").expect("counter");

    let mut cmd = Command::new("bash");
    cmd.arg(tracked_indexer())
        .env_clear()
        .env("PATH", path)
        .env("HOME", &prefix.home)
        .env("ATPKG", atpkg_bin())
        .env("ATPKG_KEYS", keys_bin())
        .env("PROGRAMS", &prefix.spec)
        .env("CHANNEL", "stable")
        .env("VALID_UNTIL", FAR_FUTURE)
        .env("ALLOW_NO_BASELINE", "1")
        .env("OUT", prefix.home.join("leak-out"))
        .env("ROSTER", &prefix.roster)
        .env("PINS_FILE", &prefix.pins)
        .env("INDEX_COUNTER_FILE", &counter)
        .env("UPLOAD", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = cmd.output().expect("bash runs the producer");
    let log = text(&out);
    assert!(out.status.success(), "{log}");
    // The counter line is the one that prints only on success — prove it ran.
    assert!(
        log.contains("bumped 20 -> 21 (post-success)"),
        "the post-success counter line must have printed:\n{log}"
    );
    let home = prefix.home.to_str().expect("utf-8 scratch path");

    // Scoped to the lines the SHELL PRODUCER emits, which is what this task owns and what
    // `atpkg_home_rel` governs. The `atpkg-keys` BINARY separately prints the signature path
    // raw (`crates/atpkg-keys/src/main.rs:421`, "atpkg-keys: wrote <path> (64 bytes)"), which
    // is the same leak one layer down and in a file this change does not own. Excluding it
    // by prefix rather than by a substring match keeps this assertion honest: if that line
    // is ever fixed, this test does not need to change, and no NEW leak can hide behind the
    // exclusion.
    let leaked: Vec<&str> = log
        .lines()
        .filter(|l| l.starts_with("atpkg-index:") || l.starts_with("atpkg-publish-lib:"))
        .filter(|l| l.contains(home))
        .collect();
    assert!(
        leaked.is_empty(),
        "these producer lines carry an absolute $HOME path into output meant for a public \
         repo:\n{leaked:#?}"
    );
    // This used to pin ONE known out-of-scope leak (atpkg-keys' own `wrote <path>` line)
    // so a second would surface here rather than in a pasted release note. That line is
    // fixed — `sign` now collapses `$HOME` to `~` like every producer line — so the
    // assertion is what it always wanted to be: NOTHING in the whole transcript carries
    // an absolute home path.
    let others: Vec<&str> = log
        .lines()
        .filter(|l| !l.starts_with("atpkg-index:") && !l.starts_with("atpkg-publish-lib:"))
        .filter(|l| l.contains(home))
        .collect();
    assert!(
        others.is_empty(),
        "no line of a publish transcript may carry an absolute $HOME path; this run had:\n\
         {others:#?}"
    );
}
