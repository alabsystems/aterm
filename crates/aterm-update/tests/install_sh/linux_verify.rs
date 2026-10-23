// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux release signature verifier (`linux_release_verify`) equivalence tests
//! ([`HARNESS`], [`updater_verdict`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use aterm_update_core::linux::LinuxTarget;
use aterm_update_core::roster::{self, Roster, verify_roster};
use aterm_update_core::tag::{TagKind, parse_release_tag};
use ring::signature::{Ed25519KeyPair, KeyPair as _};

use crate::support::{
    Lib, Rng, Scratch, chmod, install_sh, q, run, s, symlink, system_path, write,
};

const TAG: &str = "v0.100.0";
const SLUG: &str = "example/aterm";
const SIGNER: &str = "synthetic";
const OTHER: &str = "other";

/// Runs the verifier the installer pipes to `python3 -`. `run`: the script as
/// `__main__` with its master pin replaced, exit status and all.
const HARNESS: &str = r#"import contextlib, errno, io, json, os, sys, types
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
if os.environ.get("STUB_SIGNATURES") == "once":
    # The verifier's own check, asked once per key, bytes and signature: its answer is
    # OpenSSL's on exactly those, so a batch that repeats a proof spawns no second one.
    checked, check = {}, verifier["verify_ed25519"]
    def verify_once(key, data, signature):
        if (key, data, signature) not in checked:
            checked[key, data, signature] = check(key, data, signature)
        return checked[key, data, signature]
    verifier["verify_ed25519"] = verify_once
def stand_in(spec):
    target, _, value = spec.partition("=")
    value = json.loads(value)
    if target != "os.getxattr":
        return target, lambda *args, **kwargs: value
    def getxattr(path, attribute, *, follow_symlinks=True):
        answer = value.get(attribute, errno.ENODATA)
        if answer is None:
            return b""
        raise OSError(answer, os.strerror(answer), path)
    return "os", types.SimpleNamespace(**{**vars(os), "getxattr": getxattr})
with open(os.environ["STUB_CALLS"], encoding="utf-8") as calls:
    for line in calls:
        name, pins, *args = line.rstrip("\n").split("\t")
        name, _, spec = name.partition("|")
        if name.startswith("json:"):
            name, args = name[5:], [json.loads(arg) for arg in args]
        verifier["MASTER_PUBKEYS"] = tuple(pins.split())
        target, replacement = stand_in(spec) if spec else (None, None)
        kept = verifier.get(target)
        if target:
            verifier[target] = replacement
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
        finally:
            if target:
                verifier[target] = kept
"#;

/// The verifier's own floor: Python 3.11+ (tomllib, `hashlib.file_digest`) and
/// an OpenSSL 3 (`pkeyutl -rawin` over Ed25519; macOS's LibreSSL has none).
struct Tools {
    python: PathBuf,
    openssl: PathBuf,
}

/// Where the verifier's tools are looked for: each list in order, the first candidate
/// that meets the floor taken.
#[derive(Default)]
struct Search {
    python: Vec<PathBuf>,
    openssl: Vec<PathBuf>,
}

impl Search {
    /// This host's, from the `python3` and `openssl3` rows of `TOOLS`.
    fn host() -> Self {
        Self {
            python: candidates(
                "python3",
                &[
                    "python3.14",
                    "python3.13",
                    "python3.12",
                    "python3.11",
                    "python3",
                ],
            ),
            openssl: candidates("openssl3", &["openssl"]),
        }
    }
}

/// `row`'s candidates: `$ATERM_TEST_<ROW>` alone when it is set (an operator's choice is
/// never passed over for another), else the row's usual paths, then each of `names` on
/// `$PATH`.
fn candidates(row: &str, names: &[&str]) -> Vec<PathBuf> {
    let row = aterm_uds::host::tool(row);
    if let Some(named) = std::env::var_os(row.var()) {
        return vec![PathBuf::from(named)];
    }
    let mut found: Vec<PathBuf> = row.at.iter().map(PathBuf::from).collect();
    found.extend(names.iter().filter_map(|name| crate::support::which(name)));
    found
}

/// The first of `search`'s candidates that meets the floor, or the `TOOLS` rows of the
/// ones no candidate meets.
fn tools_in(search: &Search) -> Result<Tools, Vec<&'static str>> {
    let python = search.python.iter().find(|python| {
        python.is_file()
            && run(aterm_uds::spawnfd::command(python).args([
                "-I",
                "-c",
                "import tomllib, hashlib; hashlib.file_digest",
            ]))
            .status
                == Some(0)
    });
    let openssl = search.openssl.iter().find(|exe| {
        exe.is_file() && {
            let out = run(aterm_uds::spawnfd::command(exe).arg("version")).stdout;
            out.strip_prefix("OpenSSL ")
                .and_then(|v| v.split('.').next())
                .and_then(|major| major.parse::<u32>().ok())
                .is_some_and(|major| major >= 3)
        }
    });
    match (python, openssl) {
        (Some(python), Some(openssl)) => Ok(Tools {
            python: python.clone(),
            openssl: openssl.clone(),
        }),
        (python, openssl) => Err([("python3", python), ("openssl3", openssl)]
            .into_iter()
            .filter(|(_, found)| found.is_none())
            .map(|(row, _)| row)
            .collect()),
    }
}

/// The installer's verifier, reachable from a batch: `python3` is the harness
/// over the found Python, `openssl` the found OpenSSL 3.
struct Verifier {
    bin: PathBuf,
    python: PathBuf,
}

impl Verifier {
    /// The verifier under `scratch`, over this host's tools.
    fn new(scratch: &Scratch) -> Self {
        Self::with(scratch, &Search::host())
    }

    /// The verifier under `scratch`, over the tools `search` finds. A host without them
    /// ends the test COULD NOT RUN saying what is missing (their `TOOLS` rows' refusals):
    /// never a pass, and never a FAIL, which would charge the installer with the host's gap.
    fn with(scratch: &Scratch, search: &Search) -> Self {
        let tools = tools_in(search).unwrap_or_else(|absent| {
            aterm_uds::host::could_not_run_without(&absent);
            unreachable!("an absent tool ends COULD NOT RUN")
        });
        let bin = scratch.mkdir("verifier-bin");
        scratch.stand_in(
            "verifier-bin/python3",
            "#!/bin/sh\n[ \"$1\" = -I ] || { echo 'install.sh ran python3 without -I' >&2; exit 97; }\nshift\nexec \"$STUB_PYTHON\" -I \"$STUB_HARNESS\" \"$@\"\n",
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
        lib.env("PATH", &format!("{}:{}", s(&self.bin), system_path()))
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
    /// The `[min_build_sources]` tables, which [`updater_verdict`] does not
    /// model: only direct cases set them, never the corpus.
    sources: String,
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
    slug: String,
    arch: String,
    pins: Vec<String>,
    schema: String,
    roster_seq: String,
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

/// `c` released as `version` throughout: its tag, the signed version, the dmg,
/// its url and the target's name all agree, so only the version's own spelling
/// can refuse it.
fn retag(c: &mut Case, version: &str) {
    c.tag = format!("v{version}");
    c.set("version", &format!("{version:?}"));
    c.set("dmg", &format!("\"aterm-{version}.dmg\""));
    c.set(
        "url",
        &format!(
            "\"https://github.com/{}/releases/download/v{version}/aterm-{version}.dmg\"",
            c.slug
        ),
    );
    let arch = c.arch.clone();
    c.set(
        &format!("linux_{arch}"),
        &format!("\"aterm-{version}-linux-{arch}\""),
    );
}

/// The other target's triple beside `c`'s, with `digest` and `size`: a check on
/// it is the triple's own rule, never the candidate's digest or size.
fn sibling(c: &mut Case, digest: &str, size: &str) {
    let other = other_arch(&c.arch);
    let version = c.tag.trim_start_matches('v').to_string();
    c.set(
        &format!("linux_{other}"),
        &format!("\"aterm-{version}-linux-{other}\""),
    );
    c.set(&format!("linux_{other}_sha256"), &format!("{digest:?}"));
    c.set(&format!("linux_{other}_size"), size);
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
            slug: SLUG.into(),
            arch: "aarch64".into(),
            pins: vec![Keys::public(&keys.master)],
            schema: "1".into(),
            roster_seq: "7".into(),
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
                     [installed]\nbuild = 0\nsha256 = \"\"\n{}",
                    s(&store.join("aterm")),
                    self.policy.high_water,
                    self.policy.min_build,
                    self.policy.roster_floor,
                    toml_list(&self.policy.revoked),
                    self.policy.rejected,
                    self.policy.sources,
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
    assert!(
        case.policy.sources.is_empty(),
        "floor sources are not modelled"
    );
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
    ask_verifier_with(scratch, verifier, script, calls, &[])
}

/// [`ask_verifier`] with `env` set in each batch's environment.
fn ask_verifier_with(
    scratch: &Scratch,
    verifier: &Verifier,
    script: &Path,
    calls: &[String],
    env: &[(&str, &str)],
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
                for (key, value) in env {
                    lib.env(key, value);
                }
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
        case.slug.clone(),
        case.arch.clone(),
        s(&laid.store).into(),
        s(&laid.candidate).into(),
    ]
    .join("\t")
}

/// The installer's answer as an [`Accepted`], or why it refused. An acceptance
/// whose size, build or roster generation is no u64 (`7.0`, `-1`) reads as a
/// crash: no Rust verdict can name it.
fn installer_verdict(answer: &str) -> Result<Accepted, String> {
    let (kind, rest) = answer.split_once('\t').unwrap_or((answer, ""));
    if kind != "ok" {
        return Err(format!("{kind}: {rest}"));
    }
    let f: Vec<&str> = rest.split('\t').collect();
    let n = |i: usize| {
        f.get(i)
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| format!("crashed: accepted with an unreadable number: {rest}"))
    };
    Ok((f[0].into(), f[1].into(), n(2)?, n(3)?, n(4)?))
}

/// Whose rule a mutation exercises.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Rule {
    /// The updater's chain decides; the installer must decide the same.
    Shared,
    /// The installer's own, stricter than the updater by design: the signed
    /// origin bind, the source commit's spelling, an explicit schema, and a
    /// whole-roster refusal when any member is malformed.
    Installer,
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
    ("roster schema as a boolean", Rule::Shared, |c, _| {
        c.schema = "true".into()
    }),
    (
        "a roster generation that is a float",
        Rule::Shared,
        |c, _| c.roster_seq = "7.0".into(),
    ),
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
    (
        "roster deadline with single-digit fields",
        Rule::Shared,
        |c, _| {
            c.valid_until = "9999-1-1T0:0:0Z".into();
        },
    ),
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
    (
        "an empty machine id ahead of the signer",
        Rule::Shared,
        |c, k| {
            c.machines.insert(
                0,
                Row {
                    id: String::new(),
                    pubkey: Keys::public(&k.other),
                    extra: String::new(),
                },
            );
        },
    ),
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
        "a signer deadline that is a number",
        Rule::Shared,
        |c, _| {
            c.machines[0].extra = "not_after = 5\n".into();
        },
    ),
    ("a machine roster that is a number", Rule::Shared, |c, _| {
        c.machines.clear();
        c.roster_head = "machine = 5\n".into();
    }),
    ("a machine that is a number", Rule::Shared, |c, _| {
        c.machines.clear();
        c.roster_head = "machine = [1]\n".into();
    }),
    (
        "a lapsed machine listed ahead of the signer",
        Rule::Shared,
        |c, k| {
            c.machines.insert(
                0,
                Row {
                    id: OTHER.into(),
                    pubkey: Keys::public(&k.other),
                    extra: "not_after = \"2001-01-01T00:00:00Z\"\n".into(),
                },
            );
        },
    ),
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
    (
        "a noncanonical version under its own tag",
        Rule::Shared,
        |c, _| retag(c, "0.100.00"),
    ),
    (
        "a noncanonical major version under its own tag",
        Rule::Shared,
        |c, _| retag(c, "00.100.0"),
    ),
    (
        "a noncanonical minor version under its own tag",
        Rule::Shared,
        |c, _| retag(c, "0.0100.0"),
    ),
    (
        "a version past u64 under its own tag",
        Rule::Shared,
        |c, _| retag(c, "18446744073709551616.0.0"),
    ),
    (
        "a version at u64's bound under its own tag",
        Rule::Shared,
        |c, _| retag(c, "18446744073709551615.0.0"),
    ),
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
    ("a negative minimum build", Rule::Shared, |c, _| {
        c.set("min_build", "-1")
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
    (
        "an uppercase digest beside the target",
        Rule::Shared,
        |c, _| sibling(c, &"E".repeat(64), "4096"),
    ),
    ("a size of 63 beside the target", Rule::Shared, |c, _| {
        sibling(c, &"e".repeat(64), "63")
    }),
    ("a size of 64 beside the target", Rule::Shared, |c, _| {
        sibling(c, &"e".repeat(64), "64")
    }),
    (
        "a size over the bound beside the target",
        Rule::Shared,
        |c, _| sibling(c, &"e".repeat(64), "536870913"),
    ),
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
        "a repository slug with a third segment",
        Rule::Installer,
        |c, _| {
            c.slug = "example/aterm/x".into();
            c.set(
            "url",
            "\"https://github.com/example/aterm/x/releases/download/v0.100.0/aterm-0.100.0.dmg\"",
        );
        },
    ),
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
        Rule::Shared,
        |c, _| c.drop_key("sha256"),
    ),
    (
        "a machine added_at that is not a string",
        Rule::Shared,
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
    let combinable: Vec<&Mutation> = MUTATIONS.iter().collect();
    let neutral: Vec<&Mutation> = combinable
        .iter()
        .copied()
        .filter(|m| {
            let case = apply(&[m]);
            case.rule == Rule::Shared && updater_verdict(&case, &case.proofs(keys)).is_ok()
        })
        .collect();
    let mut rng = Rng::new(0x6c_696e_7578);
    for round in 0..100 {
        let mut picks: Vec<&Mutation> =
            (0..2 + rng.below(2)).map(|_| *rng.pick(&neutral)).collect();
        if round % 2 == 1 {
            picks.insert(rng.below(picks.len() + 1), *rng.pick(&combinable));
        }
        cases.push(apply(&picks));
    }
    cases
}

/// How a corpus run came out: the cases both accepted, both refused, and every
/// case decided otherwise than expected (or on which the installer crashed).
struct Tally {
    accepted: usize,
    refused: usize,
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
            "corpus: {} accepted by both, {} refused by both",
            self.accepted, self.refused
        );
        assert!(
            self.accepted >= 50 && self.refused >= 50,
            "accepted {}, refused {}",
            self.accepted,
            self.refused
        );
    }
}

/// The verifier in `script` over the corpus: each case must get the updater's verdict,
/// or a refusal where an installer-only rule is broken.
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
    let once = [("STUB_SIGNATURES", "once")];
    let answers = ask_verifier_with(scratch, verifier, script, &calls, &once);
    let mut tally = Tally {
        accepted: 0,
        refused: 0,
        wrong: Vec::new(),
    };
    for ((case, updater), answer) in cases.iter().zip(expected).zip(answers) {
        let installer = installer_verdict(&answer);
        let agree = match (case.rule, &installer) {
            (_, Err(why)) if why.starts_with("crashed") => false,
            (Rule::Installer, verdict) => verdict.is_err(),
            (Rule::Shared, Ok(i)) => updater.as_ref().is_ok_and(|u| u == i),
            (Rule::Shared, Err(_)) => updater.is_err(),
        };
        let why = match case.rule {
            Rule::Shared => "",
            Rule::Installer => " (an installer-only rule is broken)",
        };
        match (agree, &installer) {
            (true, Ok(_)) => tally.accepted += 1,
            (true, Err(_)) => tally.refused += 1,
            (false, _) => tally.wrong.push(format!(
                "{}: install.sh {installer:?}, updater {updater:?}{why}",
                case.label
            )),
        }
    }
    tally
}

/// §10 S2: with its tools looked up nowhere, the verifier ends COULD NOT RUN saying what
/// is missing and that nothing provides it before S11, never a FAIL: a host without
/// Python 3.11+ and OpenSSL 3 did not run these tests, which is no verdict on the
/// installer.
#[test]
fn a_host_without_the_verifiers_tools_ends_could_not_run() {
    let scratch = Scratch::new();
    assert_eq!(
        tools_in(&Search::default()).err(),
        Some(vec!["python3", "openssl3"])
    );
    let old = Search {
        python: vec![scratch.stand_in("old/python3", "#!/bin/sh\nexit 1\n")],
        openssl: vec![scratch.stand_in("old/openssl", "#!/bin/sh\necho 'LibreSSL 3.3.6'\n")],
    };
    assert_eq!(
        tools_in(&old).err(),
        Some(vec!["python3", "openssl3"]),
        "a copy below the floor is none"
    );
    let words = aterm_uds::host::refusals_for(&["python3", "openssl3"]).expect("two absent");
    assert!(
        words.contains("no Python 3.11+ here: aterm provides none: S11's Rust bootstrap verifier")
            && words.contains("no OpenSSL 3 here: aterm provides none")
            && words.contains("ATERM_TEST_PYTHON3=<path>")
            && !words.contains("verify linux")
            && !words.contains("brew"),
        "{words}"
    );
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Verifier::with(&scratch, &Search::default())
    }));
    let carried = refused.as_ref().err().and_then(|payload| {
        payload
            .downcast_ref::<String>()
            .map(|words| words.contains(aterm_uds::host::COULD_NOT_RUN))
    });
    assert_eq!(
        carried,
        Some(true),
        "the verifier ends a host without them COULD NOT RUN"
    );
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
        case.slug.clone(),
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
/// names, a trial is read by its phase and its identities, the record must be
/// the updater's own private file of the schema it writes, every number in it a
/// u64 and the target a file no other account can write.
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
    let rows: [(&str, String, Vec<u8>, bool); 8] = [
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
        (
            "rolling-back-before-the-rename",
            format!(
                "\n[trial]\nphase = \"RollbackPrepared\"\nhealthy = false\n[trial.old]\nbuild = 99\n\
                 sha256 = {fresh:?}\n[trial.new]\nbuild = 100\nsha256 = {digest:?}\n"
            ),
            raw.clone(),
            true,
        ),
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
    let linked = scratch.mkdir("policy/linked");
    symlink(
        scratch.mkdir("policy/elsewhere-dir"),
        &linked.join(".aterm-update"),
    );
    stores.push(linked);
    let installed = format!("sha256 = {digest:?}\n");
    let tampered = [raw.as_slice(), b"tampered"].concat();
    let private = [0o700, 0o600, 0o755];
    type Refusal<'a> = (
        &'a str,
        (&'a str, &'a str),
        String,
        &'a [u8],
        [u32; 3],
        &'a str,
    );
    let refusals: [Refusal<'_>; 17] = [
        (
            "newer-schema",
            ("schema = 1", "schema = 2"),
            String::new(),
            &raw,
            private,
            "schema",
        ),
        (
            "revocations-unlisted",
            ("[\"old-machine\"]", "\"old-machine\""),
            String::new(),
            &raw,
            private,
            "revoked",
        ),
        (
            "negative-floor",
            ("min_build = 98", "min_build = -1"),
            String::new(),
            &raw,
            private,
            "min_build",
        ),
        (
            "negative-rejected",
            ("rejected_build = 97", "rejected_build = -1"),
            String::new(),
            &raw,
            private,
            "rejected",
        ),
        (
            "negative-installed",
            ("[installed]\nbuild = 99", "[installed]\nbuild = -1"),
            String::new(),
            &raw,
            private,
            "installed build",
        ),
        (
            "rollback-unreadable-installed",
            (&installed, "sha256 = \"not-hex\"\n"),
            trial("RollbackPrepared", false, &fresh),
            &raw,
            private,
            "installed identity",
        ),
        (
            "prepared-from-another-identity",
            (&installed, &format!("sha256 = {fresh:?}\n")),
            trial("Prepared", false, &fresh),
            &raw,
            private,
            "unresolved",
        ),
        (
            "prepared-then-replaced",
            ("", ""),
            trial("Prepared", false, &fresh),
            &tampered,
            private,
            "unresolved",
        ),
        (
            "rollback-over-a-third-binary",
            ("", ""),
            trial("RollbackPrepared", false, &fresh),
            &tampered,
            private,
            "unresolved",
        ),
        (
            "healthy-then-replaced",
            ("", ""),
            trial("Installed", true, &digest),
            &tampered,
            private,
            "unresolved",
        ),
        (
            "healthy-under-another-identity",
            (&installed, &format!("sha256 = {fresh:?}\n")),
            trial("Installed", true, &digest),
            &raw,
            private,
            "unresolved",
        ),
        (
            "installed-digest-not-a-string",
            (&installed, "sha256 = 5\n"),
            String::new(),
            &raw,
            private,
            "installed identity",
        ),
        (
            "unreadable-trial-identity",
            ("", ""),
            trial("RollbackPrepared", false, "not-hex"),
            &raw,
            private,
            "trial identity",
        ),
        (
            "readable-directory",
            ("", ""),
            String::new(),
            &raw,
            [0o750, 0o600, 0o755],
            "directory",
        ),
        (
            "readable-file",
            ("", ""),
            String::new(),
            &raw,
            [0o700, 0o640, 0o755],
            "file",
        ),
        (
            "group-writable-target",
            ("", ""),
            String::new(),
            &raw,
            [0o700, 0o600, 0o775],
            "executable",
        ),
        (
            "setuid-target",
            ("", ""),
            String::new(),
            &raw,
            [0o700, 0o600, 0o4755],
            "executable",
        ),
    ];
    for (name, (from, to), tail, bytes, [dir, file, exe], _) in &refusals {
        let store = scratch.mkdir(&format!("policy/{name}"));
        let target = store.join("aterm");
        write(&target, bytes);
        chmod(&target, *exe);
        let mut text = record(&target);
        if !from.is_empty() {
            assert_eq!(
                text.matches(from).count(),
                1,
                "{name}: the edit applies once"
            );
            text = text.replace(from, to);
        }
        state(&store, &(text + tail));
        chmod(&store.join(".aterm-update/state.toml"), *file);
        chmod(&store.join(".aterm-update"), *dir);
        stores.push(store);
    }
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
    assert_eq!(
        ok(7)["rejected_build"].as_u64(),
        Some(100),
        "a rollback before its rename rejects the new build"
    );
    for (i, why) in [
        (4, "unresolved"),
        (5, "identity"),
        (6, "target mismatch"),
        (8, "directory"),
    ] {
        let refused = answers[i].as_ref().expect_err("refused");
        assert!(
            refused.contains(why),
            "{}: {refused}",
            rows.get(i).map_or("linked", |r| r.0)
        );
    }
    for ((name, .., why), answer) in refusals.iter().zip(&answers[9..]) {
        let refused = answer.as_ref().expect_err(name);
        assert!(
            refused.starts_with("refused\t") && refused.contains(why),
            "{name}: {refused}"
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
    assert!(
        !tmp_refusal.contains("chmod"),
        "only the owner can take a write bit back: {tmp_refusal}"
    );
}

/// The user-private-group rule is the updater's, fact for fact: over every
/// combination of an account, a group and the accounts that have it as their
/// primary group (each fact known or not), the embedded verifier's
/// `user_private_group` and the updater's answer alike.
#[cfg(target_os = "linux")]
#[test]
fn the_user_private_group_rule_is_the_updaters() {
    use aterm_update::linux::install_path::{Account, Group, user_private_group};
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let accounts = [
        None,
        Some(("ann", 1000)),
        Some(("ann", 1001)),
        Some(("root", 0)),
    ];
    let groups: [Option<(&str, &[&str])>; 6] = [
        None,
        Some(("ann", &[])),
        Some(("ann", &["ann"])),
        Some(("ann", &["ann", "bob"])),
        Some(("bob", &[])),
        Some(("root", &[])),
    ];
    let primaries: [&[u32]; 4] = [&[], &[1000], &[1000, 1001], &[0]];
    let mut cases = Vec::new();
    for uid in [0, 1000] {
        for gid in [0, 1000, 1001] {
            for account in &accounts {
                for group in &groups {
                    for others in &primaries {
                        cases.push((uid, gid, *account, *group, *others));
                    }
                }
            }
        }
    }
    let json = |v: &aterm_json::Value| aterm_json::to_string(v).expect("JSON");
    let calls: Vec<String> = cases
        .iter()
        .map(|(uid, gid, account, group, others)| {
            let account = account.map_or(aterm_json::Value::Null, |(name, gid)| {
                aterm_json::json!([name, gid])
            });
            let group = group.map_or(aterm_json::Value::Null, |(name, members)| {
                aterm_json::json!([name, members])
            });
            [
                "json:user_private_group".to_string(),
                "-".into(),
                uid.to_string(),
                gid.to_string(),
                json(&account),
                json(&group),
                json(&aterm_json::json!(others)),
            ]
            .join("\t")
        })
        .collect();
    let answers = ask_verifier(&scratch, &verifier, &install_sh(), &calls);
    let mut admitted = 0;
    for ((uid, gid, account, group, others), answer) in cases.iter().zip(&answers) {
        let account = account.map(|(name, gid)| Account {
            name: name.into(),
            gid,
        });
        let group = group.map(|(name, members)| Group {
            name: name.into(),
            members: members.iter().map(|m| m.as_bytes().to_vec()).collect(),
        });
        let rust = user_private_group(*uid, *gid, account.as_ref(), group.as_ref(), others);
        admitted += usize::from(rust);
        assert_eq!(
            answer,
            &format!("ok\t{}", if rust { "true" } else { "false" }),
            "uid {uid}, gid {gid}, {account:?}, {group:?}, primaries {others:?}"
        );
    }
    assert!(admitted > 0, "the corpus holds a private group");
}

/// The install-path walk is the updater's on this machine's own accounts and
/// directories: a directory of each mode, on this user's primary group and on
/// every other group it belongs to, laid where nothing above it is private and
/// again below a private directory, is admitted or refused alike by the
/// embedded verifier's `check_install_path` and the updater's
/// `install_path::refusal`, as is one on the primary group whose access ACL
/// gives another account its group class's bits (the mode unchanged); and each
/// of those groups is private, or not, to both.
#[cfg(target_os = "linux")]
#[test]
fn the_install_path_walk_is_the_updaters_on_this_machine() {
    use aterm_update::linux::install_path::{
        grant_group_class_by_acl, is_user_private_group, refusal,
    };
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    // SAFETY: Getgid has no preconditions and cannot fail.
    let primary = unsafe { libc::getgid() };
    let mut gids = vec![primary];
    // SAFETY: A zero count asks only for the number of supplementary groups.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let mut more = vec![0; usize::try_from(count).expect("a group count")];
    // SAFETY: `more` has room for exactly `count` gids.
    let got = unsafe { libc::getgroups(count, more.as_mut_ptr()) };
    more.truncate(usize::try_from(got).expect("the groups"));
    for gid in more {
        if !gids.contains(&gid) {
            gids.push(gid);
        }
    }
    let open = aterm_tempfile::Builder::new()
        .prefix("install-sh-open.")
        .tempdir_in(crate::support::scratch_root())
        .expect("an unshielded dir");
    let open_root = open.path().canonicalize().expect("its physical path");
    chmod(&open_root, 0o755);
    let shield = scratch.mkdir("shielded");
    chmod(&shield, 0o700);
    let mut dirs = Vec::new();
    for base in [&open_root, &shield] {
        for gid in &gids {
            for mode in [0o755, 0o775, 0o770, 0o757, 0o777, 0o700] {
                let dir = base.join(format!("g{gid}-{mode:o}"));
                std::fs::create_dir(&dir).expect("mkdir");
                std::os::unix::fs::chown(&dir, None, Some(*gid)).expect("a group of ours");
                chmod(&dir, mode);
                dirs.push(dir.join("store"));
                std::fs::create_dir(dirs.last().expect("laid")).expect("mkdir");
                chmod(dirs.last().expect("laid"), 0o755);
            }
        }
        for mode in [0o775, 0o755] {
            let dir = base.join(format!("acl-{mode:o}"));
            std::fs::create_dir(&dir).expect("mkdir");
            chmod(&dir, mode);
            grant_group_class_by_acl(&dir, 65534).expect("an access ACL");
            dirs.push(dir.join("store"));
            std::fs::create_dir(dirs.last().expect("laid")).expect("mkdir");
            chmod(dirs.last().expect("laid"), 0o755);
        }
        let alias = base.join("alias");
        symlink(base.join(format!("g{primary}-755")), &alias);
        dirs.push(alias.join("store"));
    }
    let mut calls: Vec<String> = dirs
        .iter()
        .map(|dir| format!("check_install_path\t-\t{}", s(dir)))
        .collect();
    let probed: Vec<u32> = gids.iter().copied().chain([0, 65534]).collect();
    calls.extend(
        probed
            .iter()
            .map(|gid| format!("json:private_group\t-\t{gid}")),
    );
    let answers = ask_verifier(&scratch, &verifier, &install_sh(), &calls);
    let (walks, facts) = answers.split_at(dirs.len());
    let mut refused = 0;
    for (dir, answer) in dirs.iter().zip(walks) {
        let updater = refusal(dir);
        refused += usize::from(updater.is_err());
        assert_eq!(
            answer.starts_with("ok\t"),
            updater.is_ok(),
            "{}: install.sh {answer:?}, updater {updater:?}",
            dir.display()
        );
    }
    assert!(refused > 0, "the corpus holds a refused path");
    for (gid, answer) in probed.iter().zip(facts) {
        let private = is_user_private_group(*gid);
        assert_eq!(
            answer,
            &format!("ok\t{}", if private { "true" } else { "false" }),
            "gid {gid}"
        );
    }
    if is_user_private_group(primary) {
        let own = open_root.join(format!("g{primary}-775/store"));
        assert!(refusal(&own).is_ok(), "{}", own.display());
    }
    let shared = open_root.join("acl-775/store");
    let why = refusal(&shared).expect_err("an ACL that grants write");
    assert!(why.contains("writable by other users"), "{why}");
    let read_only = open_root.join("acl-755/store");
    assert!(refusal(&read_only).is_ok(), "{}", read_only.display());
}

/// The access-ACL rule is the updater's, attribute for attribute, on a
/// stand-in filesystem: over every pair of answers the two ACL attributes'
/// length queries can give (it exists, `ENODATA`, `ENOTSUP`, another errno),
/// the embedded verifier's `access_acl`, its `os.getxattr` standing in for
/// the file, and the updater's `acl_carried` alike.
#[cfg(target_os = "linux")]
#[test]
fn the_acl_rule_is_the_updaters_on_a_stand_in_nfs4_file() {
    use aterm_update::linux::install_path::acl_carried;
    const POSIX: &str = "system.posix_acl_access";
    const NFS4: &str = "system.nfs4_acl";
    let (enodata, enotsup, eio) = (61, 95, 5);
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let outcomes = [None, Some(enodata), Some(enotsup), Some(eio)];
    let pairs: Vec<(Option<i32>, Option<i32>)> = outcomes
        .iter()
        .flat_map(|posix| outcomes.iter().map(move |nfs4| (*posix, *nfs4)))
        .collect();
    let json = |v: &aterm_json::Value| aterm_json::to_string(v).expect("JSON");
    let calls: Vec<String> = pairs
        .iter()
        .map(|(posix, nfs4)| {
            let table = json(&aterm_json::json!({ POSIX: posix, NFS4: nfs4 }));
            format!("json:access_acl|os.getxattr={table}\t-\t\"/stand-in\"")
        })
        .collect();
    let answers = ask_verifier(&scratch, &verifier, &install_sh(), &calls);
    for ((posix, nfs4), answer) in pairs.iter().zip(&answers) {
        let rust = acl_carried(|name| match name.to_bytes() {
            b"system.posix_acl_access" => *posix,
            b"system.nfs4_acl" => *nfs4,
            other => panic!("asked {other:?}"),
        });
        assert_eq!(
            answer,
            &format!("ok\t{rust}"),
            "posix {posix:?}, nfs4 {nfs4:?}"
        );
    }
    let carried = |posix, nfs4| {
        let at = pairs.iter().position(|pair| *pair == (posix, nfs4));
        answers[at.expect("in the corpus")] == "ok\ttrue"
    };
    assert!(carried(Some(enotsup), None), "an NFSv4 ACL");
    assert!(carried(Some(enodata), Some(eio)), "an unreadable NFSv4 ACL");
    assert!(!carried(Some(enodata), Some(enotsup)), "no ACL on ext4");
}

/// The filesystem rule is the updater's: each remote type, and the local ones
/// beside them, is remote or not to the embedded verifier's
/// `remote_filesystem_type` as to the updater's `remote_filesystem`; the
/// verifier's statfs (ctypes) reads the type the updater reads on each of
/// this machine's paths, `/proc` and the scratch root among them, and none
/// for a path that does not exist; with no C library to read the type, the
/// verifier counts the filesystem remote; and its install-path walk takes the
/// private group's exemption back on a stand-in NFS type, as on an
/// unreadable one.
#[cfg(target_os = "linux")]
#[test]
fn the_filesystem_rule_is_the_updaters() {
    use aterm_update::linux::install_path::{
        REMOTE_FILESYSTEMS, filesystem_type_at, is_user_private_group, remote_filesystem,
    };
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let magics: Vec<u32> = REMOTE_FILESYSTEMS
        .iter()
        .copied()
        .chain([
            0xEF53,
            0x0102_1994,
            0x9123_683E,
            0x5846_5342,
            0x794C_7630,
            0x9FA0,
            0,
            u32::MAX,
        ])
        .collect();
    let root = crate::support::scratch_root();
    let paths = [
        PathBuf::from("/"),
        PathBuf::from("/proc"),
        root.to_path_buf(),
        scratch.mkdir("fs"),
        root.join("no-such-entry"),
    ];
    let json = |v: &aterm_json::Value| aterm_json::to_string(v).expect("JSON");
    let mut calls: Vec<String> = magics
        .iter()
        .map(|magic| format!("json:remote_filesystem_type\t-\t{magic}"))
        .collect();
    calls.extend(paths.iter().map(|path| {
        format!(
            "json:filesystem_type\t-\t{}",
            json(&aterm_json::json!(s(path)))
        )
    }));
    calls.push("json:remote_filesystem|libc=null\t-\t\"/\"".into());
    let open = aterm_tempfile::Builder::new()
        .prefix("install-sh-fs.")
        .tempdir_in(root)
        .expect("an unshielded dir");
    let open_root = open.path().canonicalize().expect("its physical path");
    chmod(&open_root, 0o755);
    let own = open_root.join("own");
    std::fs::create_dir(&own).expect("mkdir");
    chmod(&own, 0o775);
    let store = own.join("store");
    std::fs::create_dir(&store).expect("mkdir");
    chmod(&store, 0o755);
    for stand_in in ["", "|filesystem_type=26985", "|filesystem_type=null"] {
        calls.push(format!("check_install_path{stand_in}\t-\t{}", s(&store)));
    }
    let answers = ask_verifier(&scratch, &verifier, &install_sh(), &calls);
    let (types, rest) = answers.split_at(magics.len());
    let (reads, rest) = rest.split_at(paths.len());
    let (unloaded, walks) = rest.split_at(1);
    for (magic, answer) in magics.iter().zip(types) {
        assert_eq!(
            answer,
            &format!("ok\t{}", remote_filesystem(*magic)),
            "{magic:#x}"
        );
    }
    for (path, answer) in paths.iter().zip(reads) {
        let rust = filesystem_type_at(path);
        assert_eq!(
            answer,
            &format!("ok\t{}", json(&aterm_json::json!(rust))),
            "{}",
            path.display()
        );
    }
    assert_eq!(filesystem_type_at(Path::new("/proc")), Some(0x9FA0));
    assert_eq!(unloaded, ["ok\ttrue"], "no C library: remote");
    // SAFETY: Getgid has no preconditions and cannot fail.
    let admitted = is_user_private_group(unsafe { libc::getgid() });
    assert_eq!(
        walks[0].starts_with("ok\t"),
        admitted,
        "on this filesystem: {}",
        walks[0]
    );
    for refused in &walks[1..] {
        assert!(refused.starts_with("refused\t"), "{refused}");
    }
}

/// The passwd enumeration is the updater's on this machine's accounts: for
/// root's group, this user's primary group and `nogroup`, the embedded
/// verifier's `primary_members` (`getpwent_r` through ctypes) lists the uids
/// the updater's does, in order.
#[cfg(target_os = "linux")]
#[test]
fn the_passwd_enumeration_is_the_updaters_on_this_machine() {
    use aterm_update::linux::install_path::{is_user_private_group, primary_members};
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    // SAFETY: Getuid and getgid have no preconditions and cannot fail.
    let (uid, primary) = unsafe { (libc::getuid(), libc::getgid()) };
    let gids = [0, primary, 65534];
    let mut calls: Vec<String> = gids
        .iter()
        .map(|gid| format!("json:primary_members\t-\t{gid}"))
        .collect();
    calls.push("json:primary_members|libc=null\t-\t0".into());
    for stand_in in [
        "null".to_string(),
        format!("[{uid}, {}]", uid + 1),
        format!("[{uid}]"),
    ] {
        calls.push(format!(
            "json:private_group|primary_members={stand_in}\t-\t{primary}"
        ));
    }
    let answers = ask_verifier(&scratch, &verifier, &install_sh(), &calls);
    let (lists, rest) = answers.split_at(gids.len());
    for (gid, answer) in gids.iter().zip(lists) {
        let python: aterm_json::Value = aterm_json::from_str(
            answer
                .strip_prefix("ok\t")
                .unwrap_or_else(|| panic!("gid {gid}: {answer}")),
        )
        .expect("JSON");
        assert_eq!(
            python,
            aterm_json::json!(primary_members(*gid)),
            "gid {gid}"
        );
    }
    assert_eq!(rest[0], "ok\tnull", "no C library: no enumeration");
    assert_eq!(rest[1], "ok\tfalse", "an enumeration that failed");
    assert_eq!(rest[2], "ok\tfalse", "another account's primary group");
    assert_eq!(
        rest[3],
        format!("ok\t{}", is_user_private_group(primary)),
        "this user's own enumeration"
    );
}

/// What the installer makes, its own verifier admits, whatever the caller's
/// umask: `ensure_dirs_writable` adds group and other write to the umask (and
/// keeps its read bits), so a missing `~/.local/lib/aterm/bin` is never made
/// writable by others under Ubuntu's `002`; the pre-flight (`--store-path`)
/// judges the existing part of a store before anything is downloaded, and
/// names the remedy.
#[test]
fn what_the_installer_makes_its_verifier_admits_under_any_umask() {
    use std::os::unix::fs::MetadataExt as _;
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let open = aterm_tempfile::Builder::new()
        .prefix("install-sh-umask.")
        .tempdir_in(crate::support::scratch_root())
        .expect("an unshielded dir");
    let open_root = open.path().canonicalize().expect("its physical path");
    chmod(&open_root, 0o755);
    let rows = [
        ("002", 0o755),
        ("022", 0o755),
        ("077", 0o700),
        ("000", 0o755),
    ];
    let mut lib = Lib::new();
    let made: Vec<(usize, PathBuf)> = rows
        .iter()
        .map(|(umask, _)| {
            let home = open_root.join(format!("u{umask}"));
            let store = home.join(".local/lib/aterm/bin");
            let call = lib.call(
                format!(
                    "umask {umask}; ensure_dirs_writable {} {} && umask",
                    q(s(&store)),
                    q(s(&home.join(".local/bin")))
                ),
                &[],
            );
            (call, store)
        })
        .collect();
    let recs = lib.run(&scratch);
    let mut calls = Vec::new();
    for ((umask, mode), (call, store)) in rows.iter().zip(&made) {
        assert_eq!(recs[*call].rc, 0, "umask {umask}: {}", recs[*call].err);
        assert_eq!(
            recs[*call].out.trim(),
            format!("0{umask}"),
            "the caller's umask stands"
        );
        for dir in store
            .ancestors()
            .take_while(|d| d.starts_with(&open_root) && *d != open_root.as_path())
        {
            let got = std::fs::metadata(dir).expect("made").mode() & 0o7777;
            assert_eq!(
                format!("{got:o}"),
                format!("{mode:o}"),
                "umask {umask}: {}",
                dir.display()
            );
        }
        calls.push(format!("read_install_policy\t-\t{}", s(store)));
        calls.push(format!(
            "main\t-\t--store-path\t{}",
            s(&store.join("not/yet"))
        ));
    }
    let shared = open_root.join("shared");
    std::fs::create_dir(&shared).expect("mkdir");
    chmod(&shared, 0o757);
    calls.push(format!(
        "main\t-\t--store-path\t{}",
        s(&shared.join("lib/aterm/bin"))
    ));
    let answers = ask_verifier(&scratch, &verifier, &install_sh(), &calls);
    let (last, admitted) = answers.split_last().expect("answers");
    for answer in admitted {
        assert!(answer.starts_with("ok\t"), "{answer}");
    }
    let mut lib = verifier.lib(&install_sh());
    lib.env("STUB_MODE", "run").env(
        "STUB_MASTER_PUBKEYS",
        &aterm_update_core::pins::PAPER_MASTER_PUBKEYS.join(" "),
    );
    let ours = lib.call(
        format!("linux_store_preflight {}", q(s(&made[0].1))),
        &["LINUX_STORE_SKIP"],
    );
    let theirs_store = shared.join("lib/aterm/bin");
    let theirs = lib.call(
        format!("linux_store_preflight {}", q(s(&theirs_store))),
        &["LINUX_STORE_SKIP"],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[ours].rc, 0, "{}", recs[ours].err);
    assert_eq!(recs[ours].var("LINUX_STORE_SKIP"), "");
    let skip = recs[theirs].var("LINUX_STORE_SKIP");
    if shielded(&open_root) {
        assert!(last.starts_with("ok\t"), "{last}");
        assert_eq!(recs[theirs].rc, 0, "{}", recs[theirs].err);
        assert_eq!(skip, "");
    } else {
        assert!(last.starts_with("refused\t"), "{last}");
        assert!(
            last.contains(&format!("chmod go-w {}", s(&shared))),
            "{last}"
        );
        assert_eq!(recs[theirs].rc, 1, "{}", recs[theirs].err);
        let lead = format!("another account could replace {}/aterm: ", s(&theirs_store));
        assert!(skip.starts_with(&lead), "{skip}");
        assert!(!skip.contains("install.sh:"), "{skip}");
        assert!(
            skip.contains(&format!("chmod go-w {}", s(&shared))),
            "{skip}"
        );
    }
}

/// Whether a directory of this user's that no other account can enter (mode
/// `0700`, as `$XDG_RUNTIME_DIR` is) contains `dir`.
fn shielded(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    // SAFETY: Getuid has no preconditions and cannot fail.
    let me = unsafe { libc::getuid() };
    dir.ancestors()
        .any(|a| std::fs::symlink_metadata(a).is_ok_and(|m| m.uid() == me && m.mode() & 0o077 == 0))
}

/// A first install that recorded its policy before placing the binary keeps
/// every floor on the retry; an enrolled target that vanished, or a disabled
/// one whose trial awaits recovery, is refused.
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
    let unbuilt = scratch.mkdir("fresh/unbuilt");
    state(
        &unbuilt,
        &(header(&unbuilt, false) + "[installed]\nbuild = 5\nsha256 = \"\"\n"),
    );
    stores.push(unbuilt);
    let trial = scratch.mkdir("fresh/trial");
    state(
        &trial,
        &(header(&trial, false) + &identities[0] + "[trial]\nphase = \"Prepared\"\n"),
    );
    stores.push(trial);
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
    let refused = answers[3].as_ref().expect_err("a built empty identity");
    assert!(refused.contains("fresh installed identity"), "{refused}");
    let refused = answers[4].as_ref().expect_err("a trial to recover");
    assert!(refused.contains("absent"), "{refused}");
}

/// A stolen key's floor, recorded against its machine, goes with the roster
/// that revokes it, or with this client's durable revocation of it, as the
/// updater's `FloorSources::step` withdraws it; an unattributed floor, one a
/// machine still trusted raised, or one its sources do not explain, stands; a
/// source above the floor (a hand edit) is capped at it; and sources that are
/// not a table are refused.
#[test]
fn a_revoked_machines_floor_is_withdrawn() {
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let keys = Keys::new();
    let mut case = Case::base(&keys);
    case.revoked = vec!["thief".into()];
    const BIG: u64 = 9_999_999_999;
    type Sources<'a> = (&'a str, u64, &'a str, &'a str, Result<u64, &'a str>);
    let sources: [Sources<'_>; 8] = [
        (
            "thief",
            BIG,
            "",
            "[min_build_sources.machines]\nthief = 9999999999\n",
            Ok(100),
        ),
        (
            "durably revoked",
            BIG,
            "\"stolen\"",
            "[min_build_sources.machines]\nstolen = 9999999999\n",
            Ok(100),
        ),
        ("none", BIG, "", "", Err("below apply floor")),
        (
            "base",
            BIG,
            "",
            "[min_build_sources]\nbase = 9999999999\n",
            Err("below apply floor"),
        ),
        (
            "trusted",
            BIG,
            "",
            "[min_build_sources.machines]\nother = 9999999999\n",
            Err("below apply floor"),
        ),
        (
            "unexplained",
            BIG,
            "",
            "[min_build_sources.machines]\nthief = 5\n",
            Err("below apply floor"),
        ),
        (
            "capped",
            100,
            "",
            "[min_build_sources]\nbase = 9999999999\n[min_build_sources.machines]\nother = 9999999999\n",
            Ok(100),
        ),
        (
            "malformed",
            BIG,
            "",
            "[[min_build_sources]]\nbase = 1\n",
            Err("floor sources"),
        ),
    ];
    let mut calls = Vec::new();
    for (name, min_build, durable, table, _) in &sources {
        let laid = case.lay(&keys, &scratch.join(&format!("floor/{name}")));
        state(
            &laid.store,
            &format!(
                "schema = 1\ntarget = {:?}\nenabled = false\nhigh_water = 0\nmin_build = {min_build}\n\
                 roster_floor = 0\nrevoked_machines = [{durable}]\nrejected_build = 0\n\
                 [installed]\nbuild = 0\nsha256 = \"\"\n{table}",
                s(&laid.store.join("aterm"))
            ),
        );
        calls.push(main_call(&case, &laid));
    }
    let answers = ask_verifier(&scratch, &verifier, &install_sh(), &calls);
    for ((name, .., want), answer) in sources.iter().zip(answers) {
        match (installer_verdict(&answer), want) {
            (Ok(accepted), Ok(build)) => assert_eq!(accepted.3, *build, "{name}"),
            (Err(why), Err(word)) => {
                assert!(
                    why.starts_with("refused: ") && why.contains(word),
                    "{name}: {why}"
                );
            }
            (got, _) => panic!("{name}: {got:?}"),
        }
    }
}

/// The roster's `valid_until` and its signer's `not_after` are exclusive
/// deadlines, read as the updater's `Roster::admit` and `Roster::machine` read
/// them: the second before admits the release, the deadline's own second
/// refuses it.
#[test]
fn a_deadline_refuses_from_its_own_second() {
    let scratch = Scratch::new();
    let verifier = Verifier::new(&scratch);
    let keys = Keys::new();
    const STAMP: &str = "2030-01-01T00:00:00Z";
    let at = aterm_types::rfc3339::parse_utc(STAMP).expect("a deadline");
    let mut roster_lapses = Case::base(&keys);
    roster_lapses.valid_until = STAMP.into();
    let mut signer_lapses = Case::base(&keys);
    signer_lapses.machines[0].extra = format!("not_after = {STAMP:?}\n");
    let json = |v: aterm_json::Value| aterm_json::to_string(&v).expect("JSON");
    let (mut calls, mut updater) = (Vec::new(), Vec::new());
    for (name, case) in [("roster", &roster_lapses), ("signer", &signer_lapses)] {
        let laid = case.lay(&keys, &scratch.join(&format!("deadline/{name}")));
        let pins: Vec<&str> = case.pins.iter().map(String::as_str).collect();
        let verified = verify_roster(&pins, laid.proofs.roster.clone(), &laid.proofs.roster_sig)
            .expect("the master signed it");
        let roster = Roster::parse(&verified).expect("a roster");
        for now in [at - 1, at] {
            let admitted = roster.admit(0, now).and_then(|()| {
                roster.authorize_appcast(&laid.proofs.appcast, &laid.proofs.appcast_sig, now)
            });
            updater.push((name, now, admitted.is_ok()));
            calls.push(
                [
                    "json:verify_manifest".to_string(),
                    case.pins.join(" "),
                    json(aterm_json::json!(s(&laid.release))),
                    json(aterm_json::json!(case.tag)),
                    json(aterm_json::json!(case.slug)),
                    json(aterm_json::json!(case.arch)),
                    "[0, 0, 0]".into(),
                    now.to_string(),
                ]
                .join("\t"),
            );
        }
    }
    let answers = ask_verifier(&scratch, &verifier, &install_sh(), &calls);
    for ((name, now, admitted), answer) in updater.iter().zip(&answers) {
        assert_eq!(*admitted, *now < at, "the updater's {name} deadline");
        assert_eq!(
            answer.starts_with("ok\t"),
            *admitted,
            "{name} at {now}: {answer}"
        );
    }
}

/// How a row publishes the head's proof pair.
#[derive(Clone, Copy)]
enum Pair {
    /// Both files, the appcast signed by this signer.
    Signed(By),
    /// Both files, the signature 64 zero bytes.
    Forged,
    AppcastOnly,
    SignatureOnly,
}

/// A row of [`an_older_release_is_judged_under_the_current_signed_policy`]: its
/// edit of the candidate's case and of the head's, how the head is published,
/// and the build installed or a word of the refusal.
type HeadRow = (
    &'static str,
    fn(&mut Case, &mut Case, &Keys),
    Pair,
    Result<u64, &'static str>,
);

/// A stolen machine's floor in the durable policy, which the roster revokes.
fn stolen_floor(c: &mut Case) {
    c.revoked = vec!["thief".into()];
    c.policy.min_build = 9_999_999_999;
    c.policy.sources = "[min_build_sources.machines]\nthief = 9999999999\n".into();
}

/// An older release is installed only beneath the channel's current signed
/// policy: the head's own floor applies to it, beside the durable floor less
/// what revoked machines raised; the head is admitted under the durable policy,
/// the candidate under the durable revocations too, and the head must be at
/// least the candidate (a head at its version stands); and a head that does not
/// verify, or is half published, refuses the install rather than falling back
/// to the older proof alone.
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
    let signed = Pair::Signed(By::Machine);
    let rows: [HeadRow; 13] = [
        ("under", |_, h, _| h.set("min_build", "99"), signed, Ok(100)),
        (
            "floor",
            |_, h, _| h.set("min_build", "101"),
            signed,
            Err("floor"),
        ),
        ("forged", |_, _, _| {}, Pair::Forged, Err("signature")),
        (
            "appcast alone",
            |_, _, _| {},
            Pair::AppcastOnly,
            Err("pair"),
        ),
        (
            "signature alone",
            |_, _, _| {},
            Pair::SignatureOnly,
            Err("pair"),
        ),
        (
            "older than the candidate",
            |_, h, _| {
                h.set("version", "\"0.99.0\"");
                h.set("build_number", "99");
                h.set(
                    "url",
                    "\"https://github.com/example/aterm/releases/download/v0.99.0/aterm-0.99.0.dmg\"",
                );
                h.set("dmg", "\"aterm-0.99.0.dmg\"");
            },
            signed,
            Err("head"),
        ),
        (
            "below the high water",
            |c, _, _| c.policy.high_water = 101,
            signed,
            Err("floor"),
        ),
        (
            "a revoked machine's floor withdrawn",
            |c, h, _| {
                stolen_floor(c);
                h.set("min_build", "99");
            },
            signed,
            Ok(100),
        ),
        (
            "the head's floor after the withdrawal",
            |c, h, _| {
                stolen_floor(c);
                h.set("min_build", "101");
            },
            signed,
            Err("floor"),
        ),
        (
            "a head signed by a durably revoked machine",
            |c, h, k| {
                c.machines.push(Row {
                    id: OTHER.into(),
                    pubkey: Keys::public(&k.other),
                    extra: String::new(),
                });
                c.policy.revoked = vec![OTHER.into()];
                h.set("machine_id", "\"other\"");
            },
            Pair::Signed(By::Other),
            Err("signature"),
        ),
        (
            "a candidate signed by a durably revoked machine",
            |c, _, k| {
                c.machines.push(Row {
                    id: OTHER.into(),
                    pubkey: Keys::public(&k.other),
                    extra: String::new(),
                });
                c.policy.revoked = vec![OTHER.into()];
                c.appcast_by = By::Other;
                c.set("machine_id", "\"other\"");
            },
            signed,
            Err("signature"),
        ),
        (
            "below the durable minimum",
            |c, h, _| {
                c.policy.min_build = 101;
                h.set("min_build", "99");
            },
            signed,
            Err("floor"),
        ),
        (
            "a head at the candidate's version",
            |_, h, _| {
                h.set("version", "\"0.100.0\"");
                h.set("build_number", "100");
                h.set(
                    "url",
                    "\"https://github.com/example/aterm/releases/download/v0.100.0/aterm-0.100.0.dmg\"",
                );
                h.set("dmg", "\"aterm-0.100.0.dmg\"");
            },
            signed,
            Ok(100),
        ),
    ];
    let mut calls = Vec::new();
    for (name, edit, pair, _) in &rows {
        let (mut c, mut h) = (case.clone(), head.clone());
        edit(&mut c, &mut h, &keys);
        let laid = c.lay(&keys, &scratch.join(&format!("head/{name}")));
        let bytes = h.appcast_text().into_bytes();
        let sig = match pair {
            Pair::Signed(by) => by.sign(&keys, &bytes),
            _ => vec![0; 64],
        };
        if !matches!(pair, Pair::SignatureOnly) {
            write(&laid.release.join("aterm-policy-appcast.toml"), &bytes);
        }
        if !matches!(pair, Pair::AppcastOnly) {
            write(&laid.release.join("aterm-policy-appcast.toml.sig"), &sig);
        }
        calls.push(main_call(&c, &laid));
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

/// The verifier the installer pipes to `python3` imports nothing from the
/// directory the install runs in.
#[cfg(target_os = "linux")]
#[test]
fn the_verifier_imports_nothing_from_the_directory_the_install_runs_in() {
    let scratch = Scratch::new();
    let planted = scratch.mkdir("downloads");
    let mark = planted.join("planted-ran");
    scratch.write(
        "downloads/base64.py",
        format!("open({:?}, 'w').close()\n", s(&mark)),
    );
    let shipped = install_sh();
    let text = std::fs::read_to_string(&shipped).expect("read install.sh");
    let open = text.replace(
        "python3 -I - \"$@\" <<'ATERM_LINUX_VERIFY_PY'",
        "python3 - \"$@\" <<'ATERM_LINUX_VERIFY_PY'",
    );
    assert_ne!(open, text, "the fixture edit applies to install.sh");
    let unisolated = scratch.write("install-unisolated.sh", open);
    for (script, imports) in [(&shipped, false), (&unisolated, true)] {
        let _ = std::fs::remove_file(&mark);
        let mut lib = Lib::of(script);
        let call = lib.call(
            format!("cd {} && linux_release_verify", q(s(&planted))),
            &[],
        );
        let recs = lib.run(&scratch);
        assert_ne!(
            recs[call].rc, 0,
            "an argument-less call is refused: {}",
            recs[call].err
        );
        assert_eq!(
            mark.exists(),
            imports,
            "{}: the planted module {} imported: {}",
            script.display(),
            if imports { "must be" } else { "was" },
            recs[call].err
        );
    }
}
