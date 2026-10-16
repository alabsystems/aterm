// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux lane's signature verifier: `linux_release_verify`, the Python and
//! OpenSSL the installer embeds so `curl … | bash` fetches no helper, held to
//! the updater's Rust chain.
//!
//! Every proof is built by the Rust signer. The installer decides through its
//! own function, run from the script: a `python3` stand-in hands the heredoc
//! to [`HARNESS`], which swaps only the trust anchor (no synthetic proof can
//! be signed by the paper master) and runs the embedded code. The reference is
//! the decision `aterm update install` takes on the same bytes before it places
//! a binary ([`updater_verdict`]). The installer runs the candidate as soon as
//! its own check passes, so a laxer installer runs a binary the updater
//! refuses, and a stricter one strands a fresh install.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use aterm_update_core::linux::LinuxTarget;
use aterm_update_core::roster::{self, Roster, verify_roster};
use aterm_update_core::tag::{TagKind, parse_release_tag};
use ring::signature::{Ed25519KeyPair, KeyPair as _};

use crate::support::{
    Lib, Rng, SYSTEM_PATH, Scratch, chmod, install_sh, q, run, s, symlink, write,
};

const TAG: &str = "v0.100.0";
const SLUG: &str = "example/aterm";
const SIGNER: &str = "synthetic";
const OTHER: &str = "other";

/// Runs the verifier the installer pipes to `python3 -`. `run`: the script as
/// `__main__` with its master pin replaced, exit status and all. `pins`: the
/// master keys it trusts. `calls`: each line of `$STUB_CALLS` (`function`,
/// space-separated pins, arguments; tab-separated) answered on one line.
const HARNESS: &str = r#"import contextlib, io, json, os, sys
source = sys.stdin.read()
marker = "\nMASTER_PUBKEYS = "
if source.count(marker) != 1:
    sys.exit("harness: the verifier must define MASTER_PUBKEYS exactly once")
mode = os.environ["STUB_MODE"]
if mode == "run":
    head, _, tail = source.partition(marker)
    pins = tuple(os.environ["STUB_MASTER_PUBKEYS"].split())
    sys.argv = sys.argv[1:]
    code = compile(head + marker + repr(pins) + "\n" + tail.partition("\n")[2], "linux_release_verify", "exec")
    exec(code, {"__name__": "__main__"})
    sys.exit(0)
verifier = {"__name__": "linux_release_verify"}
exec(compile(source, "linux_release_verify", "exec"), verifier)
if mode == "pins":
    print("\n".join(verifier["MASTER_PUBKEYS"]))
    sys.exit(0)
refusals = (ValueError, OSError, verifier["subprocess"].SubprocessError)
with open(os.environ["STUB_CALLS"], encoding="utf-8") as calls:
    for line in calls:
        name, pins, *args = line.rstrip("\n").split("\t")
        verifier["MASTER_PUBKEYS"] = tuple(pins.split())
        out = io.StringIO()
        try:
            with contextlib.redirect_stdout(out):
                result = verifier[name](args) if name == "main" else verifier[name](*args)
            answer = out.getvalue().strip() if name == "main" else json.dumps(result, sort_keys=True)
            print("ok\t" + answer, flush=True)
        except refusals as error:
            print("refused\t" + str(error).replace("\n", " "), flush=True)
        except Exception as error:
            print("crashed\t" + type(error).__name__ + ": " + str(error).replace("\n", " "), flush=True)
"#;

/// The verifier's own floor: Python 3.11+ (tomllib, `hashlib.file_digest`) and
/// an OpenSSL 3 (`pkeyutl -rawin` over Ed25519; macOS's LibreSSL has none).
struct Tools {
    python: PathBuf,
    openssl: PathBuf,
}

fn tools() -> Option<Tools> {
    let homebrew = ["/opt/homebrew/bin", "/usr/local/bin"];
    let python = [
        "python3.14",
        "python3.13",
        "python3.12",
        "python3.11",
        "python3",
    ]
    .iter()
    .flat_map(|name| {
        let mut found: Vec<PathBuf> = homebrew.iter().map(|d| Path::new(d).join(name)).collect();
        found.extend(crate::support::which(name));
        found
    })
    .find(|python| {
        python.is_file()
            && run(aterm_uds::spawnfd::command(python).args([
                "-I",
                "-c",
                "import tomllib, hashlib; hashlib.file_digest",
            ]))
            .status
                == Some(0)
    })?;
    let mut openssl: Vec<PathBuf> = [
        "/opt/homebrew/opt/openssl@3/bin/openssl",
        "/usr/local/opt/openssl@3/bin/openssl",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    openssl.extend(crate::support::which("openssl"));
    let openssl = openssl.into_iter().find(|exe| {
        exe.is_file() && {
            let out = run(aterm_uds::spawnfd::command(exe).arg("version")).stdout;
            out.strip_prefix("OpenSSL ")
                .and_then(|v| v.split('.').next())
                .and_then(|major| major.parse::<u32>().ok())
                .is_some_and(|major| major >= 3)
        }
    })?;
    Some(Tools { python, openssl })
}

/// The installer's verifier, reachable from a batch: `python3` is the harness
/// over the found Python, `openssl` the found OpenSSL 3.
struct Verifier {
    bin: PathBuf,
    python: PathBuf,
}

impl Verifier {
    /// The verifier under `scratch`. Its tools are a machine prerequisite: a
    /// machine without them fails here, naming the remedy, never passes.
    fn new(scratch: &Scratch) -> Self {
        let tools = tools().unwrap_or_else(|| {
            panic!(
                "the installer's Linux verifier needs Python 3.11+ and OpenSSL 3 \
                 (brew install python@3.13 openssl@3)"
            )
        });
        let bin = scratch.mkdir("verifier-bin");
        scratch.stand_in(
            "verifier-bin/python3",
            "#!/bin/sh\nexec \"$STUB_PYTHON\" -I \"$STUB_HARNESS\" \"$@\"\n",
        );
        symlink(&tools.openssl, &bin.join("openssl"));
        scratch.write("verifier-bin/harness.py", HARNESS);
        Self {
            bin,
            python: tools.python,
        }
    }

    /// A batch over `script` in which `linux_release_verify` reaches the harness.
    fn lib(&self, script: &Path) -> Lib {
        let mut lib = Lib::of(script);
        lib.env("PATH", &format!("{}:{SYSTEM_PATH}", s(&self.bin)))
            .env("STUB_PYTHON", s(&self.python))
            .env("STUB_HARNESS", s(&self.bin.join("harness.py")));
        lib
    }
}

fn keypair(seed: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).expect("a synthetic seed")
}

fn b64(bytes: &[u8]) -> String {
    aterm_codec::base64::encode(bytes).expect("base64")
}

fn sha256(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

/// The same 32 bytes in a second base64 spelling: the last character's unused
/// low bits set.
fn alias(key: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let at = key.len() - 2;
    let index = ALPHABET
        .iter()
        .position(|c| *c == key.as_bytes()[at])
        .expect("a base64 character");
    format!("{}{}=", &key[..at], char::from(ALPHABET[index | 1]))
}

/// The synthetic keys: fixed seeds, NOT production keys.
struct Keys {
    master: Ed25519KeyPair,
    second_master: Ed25519KeyPair,
    machine: Ed25519KeyPair,
    other: Ed25519KeyPair,
    thief: Ed25519KeyPair,
}

impl Keys {
    fn new() -> Self {
        Self {
            master: keypair(0x17),
            second_master: keypair(0x18),
            machine: keypair(0x29),
            other: keypair(0x3b),
            thief: keypair(0x4d),
        }
    }

    fn public(pair: &Ed25519KeyPair) -> String {
        b64(pair.public_key().as_ref())
    }
}

/// Who signs a document; `Short` is a 63-byte signature.
#[derive(Clone, Copy, Debug)]
enum By {
    Master,
    Machine,
    Other,
    Thief,
    Short,
}

impl By {
    fn sign(self, keys: &Keys, bytes: &[u8]) -> Vec<u8> {
        let pair = match self {
            Self::Master => &keys.master,
            Self::Machine => &keys.machine,
            Self::Other => &keys.other,
            Self::Thief => &keys.thief,
            Self::Short => return vec![7; 63],
        };
        pair.sign(bytes).as_ref().to_vec()
    }
}

#[derive(Clone)]
struct Row {
    id: String,
    pubkey: String,
    extra: String,
}

/// The durable policy `.aterm-update/state.toml` records beside the target.
#[derive(Clone, Default, PartialEq)]
struct Policy {
    high_water: u64,
    min_build: u64,
    roster_floor: u64,
    revoked: Vec<String>,
    rejected: u64,
}

fn toml_list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|i| format!("{i:?}")).collect();
    format!("[{}]", quoted.join(", "))
}

/// One release as published, with the verifier's inputs.
#[derive(Clone)]
struct Case {
    label: String,
    tag: String,
    arch: String,
    pins: Vec<String>,
    schema: String,
    roster_seq: u64,
    valid_until: String,
    revoked: Vec<String>,
    roster_head: String,
    machines: Vec<Row>,
    roster_tail: String,
    roster_by: By,
    roster_tamper: bool,
    appcast: Vec<(String, String)>,
    appcast_by: By,
    appcast_tamper: bool,
    binary: Vec<u8>,
    policy: Policy,
    /// Whose rule decides the case: [`Rule::Shared`] unless a mutation says
    /// otherwise.
    rule: Rule,
}

fn elf(arch: &str) -> Vec<u8> {
    let machine: u16 = match arch {
        "x86_64" => 62,
        _ => 183,
    };
    let mut header = vec![0u8; 80];
    header[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    header[16..18].copy_from_slice(&3u16.to_le_bytes());
    header[18..20].copy_from_slice(&machine.to_le_bytes());
    header[20..24].copy_from_slice(&1u32.to_le_bytes());
    header[52..54].copy_from_slice(&64u16.to_le_bytes());
    header
}

/// The other supported architecture than `arch`.
fn other_arch(arch: &str) -> &'static str {
    if arch == "x86_64" {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// `c`'s candidate replaced by `bytes`, its target's signed digest and size
/// (where the appcast has them) moved to match: only the check under test can
/// refuse it, never the digest.
fn resign(c: &mut Case, bytes: Vec<u8>) {
    let digest = format!("{:?}", sha256(&bytes));
    let size = bytes.len().to_string();
    for (suffix, value) in [("sha256", digest), ("size", size)] {
        let key = format!("linux_{}_{suffix}", c.arch);
        if c.get(&key).is_some() {
            c.set(&key, &value);
        }
    }
    c.binary = bytes;
}

/// `c`'s candidate with `bytes` written at `at` of its ELF header, re-signed.
fn resign_header(c: &mut Case, at: usize, bytes: &[u8]) {
    let mut patched = c.binary.clone();
    patched[at..at + bytes.len()].copy_from_slice(bytes);
    resign(c, patched);
}

impl Case {
    /// A valid aarch64 release, as the cutter publishes it.
    fn base(keys: &Keys) -> Self {
        let binary = elf("aarch64");
        let s = |v: &str| format!("{v:?}");
        let appcast = vec![
            ("schema".into(), "1".into()),
            ("version".into(), s("0.100.0")),
            ("build_number".into(), "100".into()),
            ("commit".into(), s(&"c".repeat(40))),
            ("machine_id".into(), s(SIGNER)),
            ("roster_seq".into(), "7".into()),
            ("dmg".into(), s("aterm-0.100.0.dmg")),
            ("sha256".into(), s(&"d".repeat(64))),
            (
                "url".into(),
                s("https://github.com/example/aterm/releases/download/v0.100.0/aterm-0.100.0.dmg"),
            ),
            ("linux_aarch64".into(), s("aterm-0.100.0-linux-aarch64")),
            ("linux_aarch64_sha256".into(), s(&sha256(&binary))),
            ("linux_aarch64_size".into(), "80".into()),
        ];
        Self {
            label: "a valid release".into(),
            tag: TAG.into(),
            arch: "aarch64".into(),
            pins: vec![Keys::public(&keys.master)],
            schema: "1".into(),
            roster_seq: 7,
            valid_until: "9999-12-31T00:00:00Z".into(),
            revoked: Vec::new(),
            roster_head: String::new(),
            machines: vec![Row {
                id: SIGNER.into(),
                pubkey: Keys::public(&keys.machine),
                extra: String::new(),
            }],
            roster_tail: String::new(),
            roster_by: By::Master,
            roster_tamper: false,
            appcast,
            appcast_by: By::Machine,
            appcast_tamper: false,
            binary,
            policy: Policy::default(),
            rule: Rule::Shared,
        }
    }

    fn set(&mut self, key: &str, value: &str) {
        match self.appcast.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value.into(),
            None => self.appcast.push((key.into(), value.into())),
        }
    }

    fn drop_key(&mut self, key: &str) {
        self.appcast.retain(|(k, _)| k != key);
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.appcast
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn roster_text(&self) -> String {
        let mut text = format!(
            "schema = {}\nroster_seq = {}\nvalid_until = {:?}\nrevoked = {}\n{}",
            self.schema,
            self.roster_seq,
            self.valid_until,
            toml_list(&self.revoked),
            self.roster_head
        );
        for m in &self.machines {
            text.push_str(&format!(
                "[[machine]]\nid = {:?}\npubkey = {:?}\n{}",
                m.id, m.pubkey, m.extra
            ));
        }
        text.push_str(&self.roster_tail);
        text
    }

    fn appcast_text(&self) -> String {
        self.appcast
            .iter()
            .map(|(k, v)| format!("{k} = {v}\n"))
            .collect()
    }

    /// The four proof files as published, the signatures over the bytes
    /// before any tamper.
    fn proofs(&self, keys: &Keys) -> Proofs {
        let roster = self.roster_text().into_bytes();
        let appcast = self.appcast_text().into_bytes();
        let roster_sig = self.roster_by.sign(keys, &roster);
        let appcast_sig = self.appcast_by.sign(keys, &appcast);
        let tamper = |mut bytes: Vec<u8>, on: bool| {
            if on {
                bytes.extend_from_slice(b"# changed\n");
            }
            bytes
        };
        Proofs {
            roster: tamper(roster, self.roster_tamper),
            roster_sig,
            appcast: tamper(appcast, self.appcast_tamper),
            appcast_sig,
        }
    }

    /// The release directory, the store (with the durable policy when there is
    /// one) and the candidate under `dir`.
    fn lay(&self, keys: &Keys, dir: &Path) -> Laid {
        let proofs = self.proofs(keys);
        let release = dir.join("release");
        write(&release.join("aterm-machines.toml"), &proofs.roster);
        write(&release.join("aterm-machines.toml.sig"), &proofs.roster_sig);
        write(&release.join("aterm-appcast.toml"), &proofs.appcast);
        write(&release.join("aterm-appcast.toml.sig"), &proofs.appcast_sig);
        let candidate = dir.join("candidate");
        write(&candidate, &self.binary);
        let store = dir.join("store");
        std::fs::create_dir_all(&store).expect("mkdir");
        if self.policy != Policy::default() {
            state(
                &store,
                &format!(
                    "schema = 1\ntarget = {:?}\nenabled = false\nhigh_water = {}\nmin_build = {}\n\
                     roster_floor = {}\nrevoked_machines = {}\nrejected_build = {}\n\
                     [installed]\nbuild = 0\nsha256 = \"\"\n",
                    s(&store.join("aterm")),
                    self.policy.high_water,
                    self.policy.min_build,
                    self.policy.roster_floor,
                    toml_list(&self.policy.revoked),
                    self.policy.rejected,
                ),
            );
        }
        Laid {
            release,
            store,
            candidate,
            proofs,
        }
    }
}

/// `state.toml` under `store`, private as the updater writes it.
fn state(store: &Path, body: &str) {
    let dir = store.join(".aterm-update");
    std::fs::create_dir_all(&dir).expect("mkdir");
    chmod(&dir, 0o700);
    let file = dir.join("state.toml");
    write(&file, body);
    chmod(&file, 0o600);
}

struct Proofs {
    roster: Vec<u8>,
    roster_sig: Vec<u8>,
    appcast: Vec<u8>,
    appcast_sig: Vec<u8>,
}

struct Laid {
    release: PathBuf,
    store: PathBuf,
    candidate: PathBuf,
    proofs: Proofs,
}

/// What a verifier accepted: the target's name, digest and size, the build
/// and the roster generation.
type Accepted = (String, String, u64, u64, u64);

/// The updater's decision on `case`, as `aterm update install` takes it before
/// placing the candidate: the roster under the pinned masters, admitted at the
/// durable floor; the appcast under its live machines, attributed, outside the
/// durable revocations; the tag; the target's signed name, size, ELF header and
/// digest; the source commit; the build against the durable floors.
fn updater_verdict(case: &Case, proofs: &Proofs) -> Result<Accepted, String> {
    let target = match case.arch.as_str() {
        "x86_64" => LinuxTarget::X86_64,
        "aarch64" => LinuxTarget::Aarch64,
        other => return Err(format!("unsupported architecture {other}")),
    };
    let pins: Vec<&str> = case.pins.iter().map(String::as_str).collect();
    let verified = verify_roster(&pins, proofs.roster.clone(), &proofs.roster_sig)
        .map_err(|e| format!("roster {e:?}"))?;
    let roster = Roster::parse(&verified).map_err(|e| format!("roster {e:?}"))?;
    let now = roster::now_unix();
    roster
        .admit(case.policy.roster_floor, now)
        .map_err(|e| format!("roster {e:?}"))?;
    let mut revoked = case.policy.revoked.clone();
    revoked.extend(roster.revoked.iter().cloned());
    let attribution = roster
        .authorize_appcast(&proofs.appcast, &proofs.appcast_sig, now)
        .map_err(|e| format!("appcast {e:?}"))?;
    let text = std::str::from_utf8(&proofs.appcast).map_err(|e| e.to_string())?;
    let manifest = aterm_update_core::Manifest::parse(text)?;
    if revoked.contains(&attribution.machine_id) {
        return Err("signer durably revoked".into());
    }
    attribution
        .bind(manifest.machine_id.as_deref(), manifest.roster_seq)
        .map_err(|e| format!("attribution {e:?}"))?;
    let min_build = case.policy.min_build.max(manifest.min_build.unwrap_or(0));
    if !matches!(parse_release_tag(&case.tag), Ok(TagKind::Candidate(_)))
        || case.tag != format!("v{}", manifest.version)
    {
        return Err("tag".into());
    }
    let artifact = manifest
        .linux_artifact(target)?
        .ok_or("no artifact for this target")?;
    if u64::try_from(case.binary.len()).ok() != Some(artifact.size) {
        return Err("size".into());
    }
    target.validate_elf_header(case.binary.get(..64).unwrap_or(&case.binary))?;
    if sha256(&case.binary) != artifact.sha256 {
        return Err("digest".into());
    }
    let commit = manifest.commit.as_deref().ok_or("no commit")?;
    let build = manifest.build_number;
    if !aterm_codec::hex::is_hex(commit, 40) || build == 0 {
        return Err("source identity".into());
    }
    if build < case.policy.high_water.max(min_build) || build <= case.policy.rejected {
        return Err("below the durable floor".into());
    }
    Ok((
        artifact.name.to_string(),
        artifact.sha256.to_string(),
        artifact.size,
        build,
        roster.roster_seq,
    ))
}

static CALLS: AtomicU32 = AtomicU32::new(0);

/// One verifier answer per call line, the batch split across threads: the heredoc in
/// `script` (install.sh, or a broken copy of it) through its own `linux_release_verify`.
fn ask_verifier(
    scratch: &Scratch,
    verifier: &Verifier,
    script: &Path,
    calls: &[String],
) -> Vec<String> {
    let code = "linux_release_verify";
    let chunks: Vec<&[String]> = calls.chunks(32).collect();
    let answers = std::sync::Mutex::new(vec![Vec::new(); chunks.len()]);
    std::thread::scope(|scope| {
        for (n, chunk) in chunks.iter().enumerate() {
            let answers = &answers;
            scope.spawn(move || {
                let file = scratch.write(
                    &format!("calls/{}", CALLS.fetch_add(1, Ordering::Relaxed)),
                    chunk.join("\n") + "\n",
                );
                let mut lib = verifier.lib(script);
                lib.env("STUB_MODE", "calls").env("STUB_CALLS", s(&file));
                let call = lib.call(code, &[]);
                let recs = lib.run(scratch);
                let rec = &recs[call];
                assert_eq!(rec.rc, 0, "the harness ran: {}", rec.err);
                let lines: Vec<String> = rec.out.lines().map(str::to_string).collect();
                assert_eq!(lines.len(), chunk.len(), "one answer per call: {}", rec.err);
                answers.lock().expect("answers")[n] = lines;
            });
        }
    });
    answers.into_inner().expect("answers").concat()
}

fn main_call(case: &Case, laid: &Laid) -> String {
    [
        "main".to_string(),
        case.pins.join(" "),
        s(&laid.release).into(),
        case.tag.clone(),
        SLUG.into(),
        case.arch.clone(),
        s(&laid.store).into(),
        s(&laid.candidate).into(),
    ]
    .join("\t")
}

/// The installer's answer as an [`Accepted`], or why it refused.
fn installer_verdict(answer: &str) -> Result<Accepted, String> {
    let (kind, rest) = answer.split_once('\t').unwrap_or((answer, ""));
    if kind != "ok" {
        return Err(format!("{kind}: {rest}"));
    }
    let f: Vec<&str> = rest.split('\t').collect();
    let n = |i: usize| {
        f.get(i)
            .and_then(|v| v.parse::<u64>().ok())
            .expect("a number")
    };
    Ok((f[0].into(), f[1].into(), n(2), n(3), n(4)))
}

/// Whose rule a mutation exercises.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Rule {
    /// The updater's chain decides; the installer must decide the same.
    Shared,
    /// The installer's own, stricter than the updater by design: the signed
    /// origin bind, the source commit's spelling, an explicit schema, and a
    /// whole-roster refusal when any member is malformed. It must refuse.
    Installer,
    /// A known gap: the installer accepts what the updater refuses, so a fresh
    /// install runs a binary the updater would not place. Applied alone and
    /// kept as a row, so tightening install.sh fails it until it moves to
    /// [`Rule::Shared`].
    KnownLooser,
}

type Mutation = (&'static str, Rule, fn(&mut Case, &Keys));

/// One rule each, across the roster, the appcast, the durable policy, the
/// candidate and the pinned masters; each changes the verdict or pins down
/// that it does not.
const MUTATIONS: &[Mutation] = &[
    ("roster signed by a machine", Rule::Shared, |c, _| {
        c.roster_by = By::Machine
    }),
    ("roster tampered after signing", Rule::Shared, |c, _| {
        c.roster_tamper = true
    }),
    ("roster signature of 63 bytes", Rule::Shared, |c, _| {
        c.roster_by = By::Short
    }),
    ("roster schema 2", Rule::Shared, |c, _| {
        c.schema = "2".into()
    }),
    ("roster schema 0", Rule::Shared, |c, _| {
        c.schema = "0".into()
    }),
    ("roster schema as a string", Rule::Shared, |c, _| {
        c.schema = "\"1\"".into()
    }),
    ("roster below the durable floor", Rule::Shared, |c, _| {
        c.policy.roster_floor = 8
    }),
    ("roster at the durable floor", Rule::Shared, |c, _| {
        c.policy.roster_floor = 7
    }),
    ("roster lapsed", Rule::Shared, |c, _| {
        c.valid_until = "2001-01-01T00:00:00Z".into()
    }),
    ("roster deadline without a time", Rule::Shared, |c, _| {
        c.valid_until = "9999-12-31".into()
    }),
    ("roster deadline with an offset", Rule::Shared, |c, _| {
        c.valid_until = "9999-12-31T00:00:00+01:00".into();
    }),
    ("signer revoked", Rule::Shared, |c, _| {
        c.revoked = vec![SIGNER.into()]
    }),
    ("another machine revoked", Rule::Shared, |c, _| {
        c.revoked = vec!["retired".into()]
    }),
    ("an empty revocation", Rule::Shared, |c, _| {
        c.revoked = vec![String::new()]
    }),
    ("a misspelt revoke key", Rule::Shared, |c, _| {
        c.roster_head = "revoke = [\"synthetic\"]\n".into()
    }),
    ("revoked written after the tables", Rule::Shared, |c, _| {
        c.roster_tail = "revoked = [\"synthetic\"]\n".into();
    }),
    ("an unknown machine key", Rule::Shared, |c, _| {
        c.machines[0].extra = "scope = \"all\"\n".into()
    }),
    ("a second machine listed", Rule::Shared, |c, k| {
        c.machines.push(Row {
            id: OTHER.into(),
            pubkey: Keys::public(&k.other),
            extra: String::new(),
        });
    }),
    ("a second machine listed first", Rule::Shared, |c, k| {
        c.machines.insert(
            0,
            Row {
                id: OTHER.into(),
                pubkey: Keys::public(&k.other),
                extra: String::new(),
            },
        );
    }),
    ("a duplicate machine id", Rule::Shared, |c, k| {
        c.machines.push(Row {
            id: SIGNER.into(),
            pubkey: Keys::public(&k.other),
            extra: String::new(),
        });
    }),
    ("a duplicate machine key", Rule::Shared, |c, k| {
        c.machines.push(Row {
            id: OTHER.into(),
            pubkey: Keys::public(&k.machine),
            extra: String::new(),
        });
    }),
    ("seventeen machines", Rule::Shared, |c, _| {
        for seed in 0x60..0x70u8 {
            c.machines.push(Row {
                id: format!("m{seed}"),
                pubkey: Keys::public(&keypair(seed)),
                extra: String::new(),
            });
        }
    }),
    ("signer lapsed", Rule::Shared, |c, _| {
        c.machines[0].extra = "not_after = \"2001-01-01T00:00:00Z\"\n".into();
    }),
    ("signer lapses later", Rule::Shared, |c, _| {
        c.machines[0].extra = "not_after = \"9999-01-01T00:00:00Z\"\n".into();
    }),
    (
        "signer key in a noncanonical spelling",
        Rule::Shared,
        |c, _| {
            c.machines[0].pubkey = alias(&c.machines[0].pubkey);
        },
    ),
    ("appcast signed by an unlisted key", Rule::Shared, |c, _| {
        c.appcast_by = By::Thief
    }),
    ("appcast signed by the master", Rule::Shared, |c, _| {
        c.appcast_by = By::Master
    }),
    (
        "appcast signed by the other machine",
        Rule::Shared,
        |c, _| c.appcast_by = By::Other,
    ),
    ("appcast tampered after signing", Rule::Shared, |c, _| {
        c.appcast_tamper = true
    }),
    ("appcast signature of 63 bytes", Rule::Shared, |c, _| {
        c.appcast_by = By::Short
    }),
    ("attributed to the other machine", Rule::Shared, |c, _| {
        c.set("machine_id", "\"other\"")
    }),
    ("unattributed", Rule::Shared, |c, _| {
        c.drop_key("machine_id")
    }),
    ("attributed under a newer roster", Rule::Shared, |c, _| {
        c.set("roster_seq", "8")
    }),
    ("attributed under an older roster", Rule::Shared, |c, _| {
        c.set("roster_seq", "6")
    }),
    ("no roster generation", Rule::Shared, |c, _| {
        c.drop_key("roster_seq")
    }),
    ("version differs from the tag", Rule::Shared, |c, _| {
        c.set("version", "\"0.99.0\"")
    }),
    ("a noncanonical tag", Rule::Shared, |c, _| {
        c.tag = "v00.100.0".into()
    }),
    ("build zero", Rule::Shared, |c, _| {
        c.set("build_number", "0")
    }),
    ("build rejected before", Rule::Shared, |c, _| {
        c.policy.rejected = 100
    }),
    ("build below the high water", Rule::Shared, |c, _| {
        c.policy.high_water = 101
    }),
    ("build at the high water", Rule::Shared, |c, _| {
        c.policy.high_water = 100
    }),
    ("build below the durable minimum", Rule::Shared, |c, _| {
        c.policy.min_build = 101
    }),
    ("build below its own minimum", Rule::Shared, |c, _| {
        c.set("min_build", "101")
    }),
    ("build at its own minimum", Rule::Shared, |c, _| {
        c.set("min_build", "100")
    }),
    ("signer durably revoked", Rule::Shared, |c, _| {
        c.policy.revoked = vec![SIGNER.into()]
    }),
    ("another machine durably revoked", Rule::Shared, |c, _| {
        c.policy.revoked = vec![OTHER.into()]
    }),
    ("a partial Linux triple", Rule::Shared, |c, _| {
        c.drop_key("linux_aarch64_size")
    }),
    ("a traversal in the Linux name", Rule::Shared, |c, _| {
        c.set("linux_aarch64", "\"../../aterm\"")
    }),
    ("an uppercase Linux digest", Rule::Shared, |c, _| {
        let upper = c
            .get("linux_aarch64_sha256")
            .expect("digest")
            .to_uppercase();
        c.set("linux_aarch64_sha256", &upper);
    }),
    ("a Linux size of 63", Rule::Shared, |c, _| {
        c.set("linux_aarch64_size", "63")
    }),
    ("a Linux size over the bound", Rule::Shared, |c, _| {
        c.set("linux_aarch64_size", "536870913")
    }),
    ("a Linux size that is a boolean", Rule::Shared, |c, _| {
        c.set("linux_aarch64_size", "true")
    }),
    ("an x86_64 triple beside it", Rule::Shared, |c, _| {
        c.set("linux_x86_64", "\"aterm-0.100.0-linux-x86_64\"");
        c.set("linux_x86_64_sha256", &format!("{:?}", "e".repeat(64)));
        c.set("linux_x86_64_size", "4096");
    }),
    ("an x86_64 name alone", Rule::Shared, |c, _| {
        c.set("linux_x86_64", "\"aterm-0.100.0-linux-x86_64\"")
    }),
    ("an x86_64 release", Rule::Shared, |c, _| {
        c.binary = elf("x86_64");
        let digest = format!("{:?}", sha256(&c.binary));
        c.drop_key("linux_aarch64");
        c.drop_key("linux_aarch64_sha256");
        c.drop_key("linux_aarch64_size");
        c.set("linux_x86_64", "\"aterm-0.100.0-linux-x86_64\"");
        c.set("linux_x86_64_sha256", &digest);
        c.set("linux_x86_64_size", "80");
        c.arch = "x86_64".into();
    }),
    ("asked for the other architecture", Rule::Shared, |c, _| {
        c.arch = if c.arch == "x86_64" {
            "aarch64"
        } else {
            "x86_64"
        }
        .into();
    }),
    ("asked for riscv64", Rule::Shared, |c, _| {
        c.arch = "riscv64".into()
    }),
    ("appcast schema 2", Rule::Shared, |c, _| {
        c.set("schema", "2")
    }),
    ("a duplicate key in the appcast", Rule::Shared, |c, _| {
        c.appcast.push(("build_number".into(), "999".into()))
    }),
    ("no source commit", Rule::Shared, |c, _| {
        c.drop_key("commit")
    }),
    ("a short source commit", Rule::Shared, |c, _| {
        c.set("commit", &format!("{:?}", "c".repeat(39)))
    }),
    ("no dmg", Rule::Shared, |c, _| c.drop_key("dmg")),
    ("candidate tampered", Rule::Shared, |c, _| {
        if let Some(last) = c.binary.last_mut() {
            *last ^= 1;
        }
    }),
    (
        "candidate of the other architecture",
        Rule::Shared,
        |c, _| {
            c.binary = elf(if c.binary[18] == 62 {
                "aarch64"
            } else {
                "x86_64"
            });
        },
    ),
    ("candidate one byte longer", Rule::Shared, |c, _| {
        c.binary.push(0)
    }),
    (
        "a signed candidate of the other architecture",
        Rule::Shared,
        |c, _| {
            let other = elf(other_arch(&c.arch));
            resign(c, other);
        },
    ),
    (
        "a signed size one byte over the candidate's",
        Rule::Shared,
        |c, _| {
            let key = format!("linux_{}_size", c.arch);
            if c.get(&key).is_some() {
                c.set(&key, &(c.binary.len() + 1).to_string());
            }
        },
    ),
    ("a signed relocatable ELF", Rule::Shared, |c, _| {
        resign_header(c, 16, &1u16.to_le_bytes())
    }),
    ("a signed non-PIE executable", Rule::Shared, |c, _| {
        resign_header(c, 16, &2u16.to_le_bytes())
    }),
    ("a signed 32-bit ELF", Rule::Shared, |c, _| {
        resign_header(c, 4, &[1])
    }),
    ("a signed big-endian ELF", Rule::Shared, |c, _| {
        resign_header(c, 5, &[2])
    }),
    ("a signed ELF of version 0", Rule::Shared, |c, _| {
        resign_header(c, 20, &0u32.to_le_bytes())
    }),
    ("a signed ELF header of 52 bytes", Rule::Shared, |c, _| {
        resign_header(c, 52, &52u16.to_le_bytes())
    }),
    (
        "master pinned in a noncanonical spelling",
        Rule::Shared,
        |c, _| {
            c.pins = c.pins.iter().map(|p| alias(p)).collect();
        },
    ),
    ("a second master pinned first", Rule::Shared, |c, k| {
        c.pins.insert(0, Keys::public(&k.second_master))
    }),
    ("an appcast without a schema", Rule::Installer, |c, _| {
        c.drop_key("schema")
    }),
    ("an uppercase source commit", Rule::Installer, |c, _| {
        c.set("commit", &format!("{:?}", "C".repeat(40)));
    }),
    ("another release's dmg", Rule::Installer, |c, _| {
        c.set("dmg", "\"aterm-0.99.0.dmg\"")
    }),
    ("another repository's url", Rule::Installer, |c, _| {
        c.set(
            "url",
            "\"https://github.com/other/aterm/releases/download/v0.100.0/aterm-0.100.0.dmg\"",
        );
    }),
    (
        "an unreadable deadline on a machine ahead of the signer",
        Rule::Installer,
        |c, k| {
            c.machines.insert(
                0,
                Row {
                    id: OTHER.into(),
                    pubkey: Keys::public(&k.other),
                    extra: "not_after = \"soon\"\n".into(),
                },
            );
        },
    ),
    (
        "another machine's key in a noncanonical spelling",
        Rule::Installer,
        |c, k| {
            c.machines.push(Row {
                id: OTHER.into(),
                pubkey: alias(&Keys::public(&k.other)),
                extra: String::new(),
            });
        },
    ),
    ("an appcast key from the future", Rule::Shared, |c, _| {
        c.set("future_key", "1")
    }),
    ("a negative appcast schema", Rule::Shared, |c, _| {
        c.set("schema", "-1")
    }),
    ("a build number that is a string", Rule::Shared, |c, _| {
        c.set("build_number", "\"100\"")
    }),
    (
        "an appcast without the dmg's digest",
        Rule::KnownLooser,
        |c, _| c.drop_key("sha256"),
    ),
    (
        "a machine added_at that is not a string",
        Rule::KnownLooser,
        |c, _| c.machines[0].extra = "added_at = 1\n".into(),
    ),
];

/// Every mutation alone, then seeded combinations of two and three.
fn corpus(keys: &Keys) -> Vec<Case> {
    let apply = |picks: &[&Mutation]| {
        let mut case = Case::base(keys);
        case.label = picks
            .iter()
            .map(|(name, ..)| *name)
            .collect::<Vec<_>>()
            .join(" + ");
        for (_, rule, mutate) in picks {
            if *rule != Rule::Shared {
                case.rule = *rule;
            }
            mutate(&mut case, keys);
        }
        case
    };
    let mut cases = vec![Case::base(keys)];
    cases.extend(MUTATIONS.iter().map(|m| apply(&[m])));
    // A known gap is a row of its own: combined, its verdict is no one's.
    let combinable: Vec<&Mutation> = MUTATIONS
        .iter()
        .filter(|(_, rule, _)| *rule != Rule::KnownLooser)
        .collect();
    // The mutations a release survives alone, combined, reach the accepting
    // side's interactions; any mutation on top of them reaches the refusals'.
    let neutral: Vec<&Mutation> = combinable
        .iter()
        .copied()
        .filter(|m| {
            let case = apply(&[m]);
            case.rule == Rule::Shared && updater_verdict(&case, &case.proofs(keys)).is_ok()
        })
        .collect();
    let mut rng = Rng::new(0x6c_696e_7578);
    for round in 0..300 {
        let mut picks: Vec<&Mutation> =
            (0..2 + rng.below(2)).map(|_| *rng.pick(&neutral)).collect();
        if round % 2 == 1 {
            picks.insert(rng.below(picks.len() + 1), *rng.pick(&combinable));
        }
        cases.push(apply(&picks));
    }
    cases
}

/// How a corpus run came out: the cases both accepted, both refused, the known
/// gaps still open, and every case decided otherwise than expected (or on
/// which the installer crashed).
struct Tally {
    accepted: usize,
    refused: usize,
    looser: usize,
    wrong: Vec<String>,
}

impl Tally {
    /// No disagreement, and both verdicts reached many times over.
    fn assert_agrees(&self) {
        assert!(
            self.wrong.is_empty(),
            "{} disagreements:\n{}",
            self.wrong.len(),
            self.wrong.join("\n")
        );
        eprintln!(
            "corpus: {} accepted by both, {} refused by both, {} known gaps",
            self.accepted, self.refused, self.looser
        );
        assert!(
            self.accepted >= 150 && self.refused >= 150,
            "accepted {}, refused {}",
            self.accepted,
            self.refused
        );
    }
}

/// The verifier in `script` over the corpus: each case must get the updater's verdict,
/// a refusal where an installer-only rule is broken, or the installer's acceptance where
/// a known gap is the case.
fn differential(scratch: &Scratch, verifier: &Verifier, script: &Path) -> Tally {
    let keys = Keys::new();
    let cases = corpus(&keys);
    let root = scratch.mkdir(&format!("corpus.{}", CALLS.fetch_add(1, Ordering::Relaxed)));
    let mut calls = Vec::new();
    let mut expected = Vec::new();
    for (n, case) in cases.iter().enumerate() {
        let laid = case.lay(&keys, &root.join(n.to_string()));
        calls.push(main_call(case, &laid));
        expected.push(updater_verdict(case, &laid.proofs));
    }
    let answers = ask_verifier(scratch, verifier, script, &calls);
    let mut tally = Tally {
        accepted: 0,
        refused: 0,
        looser: 0,
        wrong: Vec::new(),
    };
    for ((case, updater), answer) in cases.iter().zip(expected).zip(answers) {
        let installer = installer_verdict(&answer);
        let agree = match (case.rule, &installer) {
            (_, Err(why)) if why.starts_with("crashed") => false,
            (Rule::Installer, verdict) => verdict.is_err(),
            (Rule::KnownLooser, verdict) => verdict.is_ok() && updater.is_err(),
            (Rule::Shared, Ok(i)) => updater.as_ref().is_ok_and(|u| u == i),
            (Rule::Shared, Err(_)) => updater.is_err(),
        };
        let why = match case.rule {
            Rule::Shared => "",
            Rule::Installer => " (an installer-only rule is broken)",
            Rule::KnownLooser => " (a known gap: if both now refuse, make the row Rule::Shared)",
        };
        match (agree, case.rule, &installer) {
            (true, Rule::KnownLooser, _) => tally.looser += 1,
            (true, _, Ok(_)) => tally.accepted += 1,
            (true, _, Err(_)) => tally.refused += 1,
            (false, ..) => tally.wrong.push(format!(
                "{}: install.sh {installer:?}, updater {updater:?}{why}",
                case.label
            )),
        }
    }
    tally
}

/// The installer accepts exactly what the updater would install, naming the
/// same target, digest, size, build and roster generation, and refuses
/// wherever one of its own stricter rules is broken.
#[test]
fn the_embedded_verifier_agrees_with_the_updater_on_a_generated_corpus() {
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    differential(&scratch, &verifier, &install_sh()).assert_agrees();
}

/// Negative control: a copy of install.sh whose verifier forgets the durable
/// revocation set is caught.
#[test]
fn the_verifier_differential_catches_a_dropped_durable_revocation() {
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let text = std::fs::read_to_string(install_sh()).expect("install.sh");
    let rule = r#"if machine["id"] in revoked or machine["id"] in revoked_machines:"#;
    assert_eq!(text.matches(rule).count(), 1, "the rule the control breaks");
    let broken = scratch.write(
        "install.sh",
        text.replace(rule, r#"if machine["id"] in revoked:"#),
    );
    let wrong = differential(&scratch, &verifier, &broken).wrong;
    assert!(
        wrong
            .iter()
            .any(|w| w.starts_with("signer durably revoked:")),
        "{wrong:?}"
    );
}

/// The installer trusts exactly the paper master the updater pins.
#[test]
fn the_embedded_verifier_trusts_the_updaters_paper_master() {
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let mut lib = verifier.lib(&install_sh());
    lib.env("STUB_MODE", "pins");
    let call = lib.call("linux_release_verify", &[]);
    let recs = lib.run(&scratch);
    let pinned: Vec<&str> = recs[call].out.lines().collect();
    assert_eq!(
        pinned,
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        "{}",
        recs[call].err
    );
}

/// One run of `linux_release_verify` as the installer runs it: its exit status,
/// what it printed and why it refused.
fn verify_once(
    scratch: &Scratch,
    verifier: &Verifier,
    case: &Case,
    laid: &Laid,
) -> crate::support::Rec {
    let mut lib = verifier.lib(&install_sh());
    lib.env("STUB_MODE", "run")
        .env("STUB_MASTER_PUBKEYS", &case.pins.join(" "));
    let args = [
        q(s(&laid.release)),
        case.tag.clone(),
        SLUG.into(),
        case.arch.clone(),
        q(s(&laid.store)),
        q(s(&laid.candidate)),
    ];
    let call = lib.call(format!("linux_release_verify {}", args.join(" ")), &[]);
    lib.run(scratch).swap_remove(call)
}

/// The script's own exits: a proof it accepts prints the signed target for the
/// installer to read, and a refusal exits 1 saying why.
#[test]
fn a_verdict_is_the_scripts_exit_status_and_its_one_line() {
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let keys = Keys::new();
    let good = Case::base(&keys);
    let laid = good.lay(&keys, &scratch.join("good"));
    let accepted = verify_once(&scratch, &verifier, &good, &laid);
    assert_eq!(accepted.rc, 0, "{}", accepted.err);
    let want = updater_verdict(&good, &laid.proofs).expect("the updater accepts it");
    assert_eq!(
        accepted.out,
        format!("{}\t{}\t{}\t{}\t{}", want.0, want.1, want.2, want.3, want.4)
    );
    let mut bad = good.clone();
    bad.appcast_tamper = true;
    let laid = bad.lay(&keys, &scratch.join("bad"));
    let refused = verify_once(&scratch, &verifier, &bad, &laid);
    assert_eq!(refused.rc, 1);
    assert_eq!(refused.out, "");
    assert!(
        refused
            .err
            .starts_with("install.sh: Linux verification refused: "),
        "{}",
        refused.err
    );
}

/// The candidate is read as itself: a symlink to the very bytes is refused.
#[test]
fn a_symlinked_candidate_is_refused() {
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let keys = Keys::new();
    let case = Case::base(&keys);
    let mut laid = case.lay(&keys, &scratch.join("link"));
    let alias = scratch.join("link/alias");
    symlink(&laid.candidate, &alias);
    laid.candidate = alias;
    let run = verify_once(&scratch, &verifier, &case, &laid);
    assert_eq!(run.rc, 1, "{}", run.out);
    assert!(run.err.contains("type"), "{}", run.err);
}

/// Each call is `read_install_policy` on a store prepared by the case; the
/// answers are the policy as JSON, or the refusal.
fn read_policies(
    scratch: &Scratch,
    verifier: &Verifier,
    stores: &[&Path],
) -> Vec<Result<aterm_json::Value, String>> {
    let calls: Vec<String> = stores
        .iter()
        .map(|store| format!("read_install_policy\t-\t{}", s(store)))
        .collect();
    ask_verifier(scratch, verifier, &install_sh(), &calls)
        .into_iter()
        .map(|answer| match answer.split_once('\t') {
            Some(("ok", json)) => Ok(aterm_json::from_str(json).expect("the policy as JSON")),
            _ => Err(answer),
        })
        .collect()
}

fn floors(policy: &aterm_json::Value) -> Vec<u64> {
    policy["floors"]
        .as_array()
        .expect("floors")
        .iter()
        .map(|f| f.as_u64().expect("a floor"))
        .collect()
}

/// The durable policy binds the installed file: the floors, revocations and
/// rejected build are read only while the target is the identity the record
/// names, a trial is read by its phase, and the record must be the updater's
/// own private file.
#[test]
fn the_durable_policy_binds_the_installed_file() {
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let raw = elf("aarch64");
    let digest = sha256(&raw);
    let record = |target: &Path| {
        format!(
            "schema = 1\ntarget = {:?}\nhigh_water = 99\nmin_build = 98\nroster_floor = 7\n\
             revoked_machines = [\"old-machine\"]\nrejected_build = 97\n\
             [installed]\nbuild = 99\nsha256 = {digest:?}\n",
            s(target)
        )
    };
    let trial = |phase: &str, healthy: bool, new_digest: &str| {
        format!(
            "\n[trial]\nphase = {phase:?}\nhealthy = {healthy}\n[trial.old]\nbuild = 99\nsha256 = {digest:?}\n\
             [trial.new]\nbuild = 100\nsha256 = {new_digest:?}\n"
        )
    };
    let fresh = "f".repeat(64);
    // (name, the record's tail, the target's bytes, the record's target)
    let rows: [(&str, String, Vec<u8>, bool); 7] = [
        ("plain", String::new(), raw.clone(), true),
        (
            "prepared",
            trial("Prepared", false, &fresh),
            raw.clone(),
            true,
        ),
        (
            "rolling-back",
            trial("RollbackPrepared", false, &fresh),
            raw.clone(),
            true,
        ),
        (
            "healthy",
            trial("Installed", true, &digest),
            raw.clone(),
            true,
        ),
        (
            "unresolved",
            trial("Installed", false, &digest),
            raw.clone(),
            true,
        ),
        (
            "tampered",
            String::new(),
            [raw.as_slice(), b"tampered"].concat(),
            true,
        ),
        ("elsewhere", String::new(), raw.clone(), false),
    ];
    let mut stores = Vec::new();
    for (name, tail, bytes, here) in &rows {
        let store = scratch.mkdir(&format!("policy/{name}"));
        let target = store.join("aterm");
        write(&target, bytes);
        chmod(&target, 0o755);
        let named = if *here {
            target.clone()
        } else {
            store.join("different")
        };
        state(&store, &(record(&named) + tail));
        stores.push(store);
    }
    // A state directory that is a symlink is not the updater's own.
    let linked = scratch.mkdir("policy/linked");
    symlink(
        scratch.mkdir("policy/elsewhere-dir"),
        &linked.join(".aterm-update"),
    );
    stores.push(linked);
    let refs: Vec<&Path> = stores.iter().map(PathBuf::as_path).collect();
    let answers = read_policies(&scratch, &verifier, &refs);
    let ok = |i: usize| {
        answers[i]
            .as_ref()
            .unwrap_or_else(|e| panic!("{}: {e}", rows[i].0))
    };
    assert_eq!(floors(ok(0)), [99, 98, 7]);
    assert_eq!(ok(0)["revoked_machines"][0].as_str(), Some("old-machine"));
    assert_eq!(ok(0)["rejected_build"].as_u64(), Some(97));
    assert_eq!(
        floors(ok(1)),
        [99, 98, 7],
        "a prepared trial keeps the floors"
    );
    assert_eq!(
        ok(2)["rejected_build"].as_u64(),
        Some(100),
        "a rollback rejects the new build"
    );
    assert_eq!(
        floors(ok(3)),
        [99, 98, 7],
        "a healthy trial reads as installed"
    );
    for (i, why) in [
        (4, "unresolved"),
        (5, "identity"),
        (6, "target mismatch"),
        (7, "directory"),
    ] {
        let refused = answers[i].as_ref().expect_err("refused");
        assert!(
            refused.contains(why),
            "{}: {refused}",
            rows.get(i).map_or("linked", |r| r.0)
        );
    }
}

/// A user-owned private ancestor shields a group-writable child, a symlinked
/// store is refused, and a world-writable ancestor before any private one
/// (the system's /tmp) is refused.
#[test]
fn a_private_ancestor_shields_writable_children_but_not_earlier_ancestors() {
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let private = scratch.mkdir("shield");
    chmod(&private, 0o700);
    let writable = scratch.mkdir("shield/lib");
    chmod(&writable, 0o775);
    let alias = scratch.join("shield/alias");
    symlink(&writable, &alias);
    let shared = aterm_tempfile::Builder::new()
        .prefix("install-sh-unshielded.")
        .tempdir_in("/tmp")
        .expect("a dir under /tmp");
    let unshielded = shared.path().canonicalize().expect("its physical path");
    let answers = read_policies(&scratch, &verifier, &[&writable, &alias, &unshielded]);
    assert_eq!(floors(answers[0].as_ref().expect("shielded")), [0, 0, 0]);
    let alias_refusal = answers[1].as_ref().expect_err("a symlinked store");
    assert!(
        alias_refusal.contains("unsafe install path"),
        "{alias_refusal}"
    );
    let tmp_refusal = answers[2].as_ref().expect_err("under /tmp");
    assert!(tmp_refusal.contains("unshielded"), "{tmp_refusal}");
}

/// A first install that recorded its policy before placing the binary keeps
/// every floor on the retry; an enrolled target that vanished is refused.
#[test]
fn a_disabled_fresh_install_keeps_its_policy_on_retry() {
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let digest = sha256(&elf("aarch64"));
    let header = |store: &Path, enabled: bool| {
        format!(
            "schema = 1\ntarget = {:?}\nenabled = {enabled}\nhigh_water = 101\nmin_build = 99\n\
             roster_floor = 7\nrevoked_machines = [\"revoked-machine\"]\nrejected_build = 98\n",
            s(&store.join("aterm"))
        )
    };
    let identities = [
        "[installed]\nbuild = 0\nsha256 = \"\"\n".to_string(),
        format!("[installed]\nbuild = 101\nsha256 = {digest:?}\n"),
    ];
    let mut stores = Vec::new();
    for (n, identity) in identities.iter().enumerate() {
        let store = scratch.mkdir(&format!("fresh/{n}"));
        state(&store, &(header(&store, false) + identity));
        stores.push(store);
    }
    let enrolled = scratch.mkdir("fresh/enrolled");
    state(&enrolled, &(header(&enrolled, true) + &identities[1]));
    stores.push(enrolled);
    let refs: Vec<&Path> = stores.iter().map(PathBuf::as_path).collect();
    let answers = read_policies(&scratch, &verifier, &refs);
    for answer in &answers[..2] {
        let policy = answer.as_ref().expect("a disabled fresh install's policy");
        assert_eq!(floors(policy), [101, 99, 7]);
        assert_eq!(
            policy["revoked_machines"][0].as_str(),
            Some("revoked-machine")
        );
        assert_eq!(policy["rejected_build"].as_u64(), Some(98));
    }
    let refused = answers[2]
        .as_ref()
        .expect_err("an enrolled target vanished");
    assert!(refused.contains("absent"), "{refused}");
}

/// A stolen key's floor, recorded against its machine, goes with the roster
/// that revokes it, as the updater's `FloorSources::step` withdraws it; an
/// unattributed floor, or one a machine still trusted raised, stands.
#[test]
fn a_revoked_machines_floor_is_withdrawn() {
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let keys = Keys::new();
    let mut case = Case::base(&keys);
    case.revoked = vec!["thief".into()];
    let sources = [
        (
            "thief",
            "[min_build_sources.machines]\nthief = 9999999999\n",
            true,
        ),
        ("none", "", false),
        ("base", "[min_build_sources]\nbase = 9999999999\n", false),
        (
            "trusted",
            "[min_build_sources.machines]\nother = 9999999999\n",
            false,
        ),
    ];
    let mut calls = Vec::new();
    for (name, table, _) in &sources {
        let laid = case.lay(&keys, &scratch.join(&format!("floor/{name}")));
        state(
            &laid.store,
            &format!(
                "schema = 1\ntarget = {:?}\nenabled = false\nhigh_water = 0\nmin_build = 9999999999\n\
                 roster_floor = 0\nrevoked_machines = []\nrejected_build = 0\n\
                 [installed]\nbuild = 0\nsha256 = \"\"\n{table}",
                s(&laid.store.join("aterm"))
            ),
        );
        calls.push(main_call(&case, &laid));
    }
    let answers = ask_verifier(&scratch, &verifier, &install_sh(), &calls);
    for ((name, _, installs), answer) in sources.iter().zip(answers) {
        let verdict = installer_verdict(&answer);
        if *installs {
            assert_eq!(verdict.map(|a| a.3), Ok(100), "{name}");
        } else {
            let why = verdict.expect_err(name);
            assert!(why.contains("below apply floor"), "{name}: {why}");
        }
    }
}

/// An older release is installed only beneath the channel's current signed
/// policy: the head's own floor applies to it, and a head that does not verify
/// refuses the install rather than falling back to the older proof alone.
#[test]
fn an_older_release_is_judged_under_the_current_signed_policy() {
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let keys = Keys::new();
    let case = Case::base(&keys);
    let mut head = case.clone();
    head.set("version", "\"0.101.0\"");
    head.set("build_number", "101");
    head.set(
        "url",
        "\"https://github.com/example/aterm/releases/download/v0.101.0/aterm-0.101.0.dmg\"",
    );
    head.set("dmg", "\"aterm-0.101.0.dmg\"");
    head.appcast.retain(|(k, _)| !k.starts_with("linux_"));
    let rows: [(&str, &str, bool, Result<u64, &str>); 3] = [
        ("under", "99", true, Ok(100)),
        ("floor", "101", true, Err("floor")),
        ("forged", "99", false, Err("signature")),
    ];
    let mut calls = Vec::new();
    for (name, min_build, signed, _) in &rows {
        let laid = case.lay(&keys, &scratch.join(&format!("head/{name}")));
        let mut policy = head.clone();
        policy.set("min_build", min_build);
        let bytes = policy.appcast_text().into_bytes();
        let sig = if *signed {
            By::Machine.sign(&keys, &bytes)
        } else {
            vec![0; 64]
        };
        write(&laid.release.join("aterm-policy-appcast.toml"), &bytes);
        write(&laid.release.join("aterm-policy-appcast.toml.sig"), &sig);
        calls.push(main_call(&case, &laid));
    }
    let answers = ask_verifier(&scratch, &verifier, &install_sh(), &calls);
    for ((name, _, _, want), answer) in rows.iter().zip(answers) {
        match (installer_verdict(&answer), want) {
            (Ok(accepted), Ok(build)) => assert_eq!(accepted.3, *build, "{name}"),
            (Err(why), Err(key)) => assert!(why.contains(key), "{name}: {why}"),
            (got, _) => panic!("{name}: {got:?}"),
        }
    }
}
