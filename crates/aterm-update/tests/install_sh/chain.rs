// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signed release test corpus and differential harness for install.sh Linux verifiers.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use aterm_update_core::linux::LinuxTarget;
use aterm_update_core::roster::{self, Roster, verify_roster};
use aterm_update_core::tag::{TagKind, parse_release_tag};
use ring::signature::{Ed25519KeyPair, KeyPair as _};

use crate::support::{Lib, Rec, Rng, Scratch, chmod, q, run, s, symlink, write};

pub const TAG: &str = "v0.100.0";
pub const SLUG: &str = "example/aterm";
pub const SIGNER: &str = "synthetic";
pub const OTHER: &str = "other";

/// A verifier's answer to one command line: `ok\t<line>`, `refused\t<why>` or
/// `crashed\t<what>`.
pub type Answer = String;

/// What a verifier read as a store's durable policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyView {
    pub floors: [u64; 3],
    pub revoked_machines: Vec<String>,
    pub rejected_build: u64,
}

/// A policy read, or the verifier's answer when it read none (`refused\t<why>`).
pub type PolicyAnswer = Result<PolicyView, Answer>;

/// One of install.sh's Linux verifiers, as a suite reaches it.
pub trait Verifier: Sync {
    fn mains(&self, scratch: &Scratch, runs: &[(&Case, &Laid)]) -> Vec<Answer>;
    fn policies(&self, scratch: &Scratch, stores: &[&Path]) -> Vec<PolicyAnswer>;
    fn preflights(&self, scratch: &Scratch, stores: &[&Path]) -> Vec<Answer>;
    fn manifests_at(&self, scratch: &Scratch, asks: &[(&Case, &Laid, i64)]) -> Vec<Answer>;
    fn walks(&self, scratch: &Scratch, dirs: &[&Path]) -> Vec<Answer>;
    fn private_groups(&self, scratch: &Scratch, gids: &[u32]) -> Vec<bool>;
    fn lib(&self, pins: &[String]) -> Lib;
}

pub fn keypair(seed: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).expect("a synthetic seed")
}

pub fn b64(bytes: &[u8]) -> String {
    aterm_codec::base64::encode(bytes).expect("base64")
}

pub fn sha256(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

/// The same 32 bytes in a second base64 spelling: the last character's unused
/// low bits set.
pub fn alias(key: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let at = key.len() - 2;
    let index = ALPHABET
        .iter()
        .position(|c| *c == key.as_bytes()[at])
        .expect("a base64 character");
    format!("{}{}=", &key[..at], char::from(ALPHABET[index | 1]))
}

/// The synthetic keys: fixed seeds, NOT production keys.
pub struct Keys {
    pub master: Ed25519KeyPair,
    pub second_master: Ed25519KeyPair,
    pub machine: Ed25519KeyPair,
    pub other: Ed25519KeyPair,
    pub thief: Ed25519KeyPair,
}

impl Keys {
    pub fn new() -> Self {
        Self {
            master: keypair(0x17),
            second_master: keypair(0x18),
            machine: keypair(0x29),
            other: keypair(0x3b),
            thief: keypair(0x4d),
        }
    }

    pub fn public(pair: &Ed25519KeyPair) -> String {
        b64(pair.public_key().as_ref())
    }
}

/// Who signs a document; `Short` is a 63-byte signature.
#[derive(Clone, Copy, Debug)]
pub enum By {
    Master,
    Machine,
    Other,
    Thief,
    Short,
}

impl By {
    pub fn sign(self, keys: &Keys, bytes: &[u8]) -> Vec<u8> {
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
pub struct Row {
    pub id: String,
    pub pubkey: String,
    pub extra: String,
}

/// The durable policy `.aterm-update/state.toml` records beside the target.
#[derive(Clone, Default, PartialEq)]
pub struct Policy {
    pub high_water: u64,
    pub min_build: u64,
    pub roster_floor: u64,
    pub revoked: Vec<String>,
    pub rejected: u64,
    /// The `[min_build_sources]` tables, which [`updater_verdict`] does not
    /// model: only direct cases set them, never the corpus.
    pub sources: String,
}

pub fn toml_list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|i| format!("{i:?}")).collect();
    format!("[{}]", quoted.join(", "))
}

/// One release as published, with the verifier's inputs.
#[derive(Clone)]
pub struct Case {
    pub label: String,
    pub tag: String,
    pub slug: String,
    pub arch: String,
    pub pins: Vec<String>,
    pub schema: String,
    pub roster_seq: String,
    pub valid_until: String,
    pub revoked: Vec<String>,
    /// Text before the roster's first line.
    pub roster_lead: String,
    pub roster_head: String,
    pub machines: Vec<Row>,
    pub roster_tail: String,
    pub roster_by: By,
    pub roster_tamper: bool,
    pub appcast: Vec<(String, String)>,
    pub appcast_by: By,
    pub appcast_tamper: bool,
    pub binary: Vec<u8>,
    pub policy: Policy,
    /// Whose rule decides the case: [`Rule::Shared`] unless a mutation says
    /// otherwise.
    pub rule: Rule,
}

pub fn elf(arch: &str) -> Vec<u8> {
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
pub fn other_arch(arch: &str) -> &'static str {
    if arch == "x86_64" {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// `c`'s candidate replaced by `bytes`, its target's signed digest and size
/// (where the appcast has them) moved to match: only the check under test can
/// refuse it, never the digest.
pub fn resign(c: &mut Case, bytes: Vec<u8>) {
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
pub fn resign_header(c: &mut Case, at: usize, bytes: &[u8]) {
    let mut patched = c.binary.clone();
    patched[at..at + bytes.len()].copy_from_slice(bytes);
    resign(c, patched);
}

/// `c` released as `version` throughout: its tag, the signed version, the dmg,
/// its url and the target's name all agree, so only the version's own spelling
/// can refuse it.
pub fn retag(c: &mut Case, version: &str) {
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
pub fn sibling(c: &mut Case, digest: &str, size: &str) {
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
    pub fn base(keys: &Keys) -> Self {
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
            roster_lead: String::new(),
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

    pub fn set(&mut self, key: &str, value: &str) {
        match self.appcast.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value.into(),
            None => self.appcast.push((key.into(), value.into())),
        }
    }

    pub fn drop_key(&mut self, key: &str) {
        self.appcast.retain(|(k, _)| k != key);
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.appcast
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn roster_text(&self) -> String {
        let mut text = format!(
            "{}schema = {}\nroster_seq = {}\nvalid_until = {:?}\nrevoked = {}\n{}",
            self.roster_lead,
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

    pub fn appcast_text(&self) -> String {
        self.appcast
            .iter()
            .map(|(k, v)| format!("{k} = {v}\n"))
            .collect()
    }

    /// The four proof files as published, the signatures over the bytes
    /// before any tamper.
    pub fn proofs(&self, keys: &Keys) -> Proofs {
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
    pub fn lay(&self, keys: &Keys, dir: &Path) -> Laid {
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
pub fn state(store: &Path, body: &str) {
    let dir = store.join(".aterm-update");
    std::fs::create_dir_all(&dir).expect("mkdir");
    chmod(&dir, 0o700);
    let file = dir.join("state.toml");
    write(&file, body);
    chmod(&file, 0o600);
}

pub struct Proofs {
    pub roster: Vec<u8>,
    pub roster_sig: Vec<u8>,
    pub appcast: Vec<u8>,
    pub appcast_sig: Vec<u8>,
}

pub struct Laid {
    pub release: PathBuf,
    pub store: PathBuf,
    pub candidate: PathBuf,
    pub proofs: Proofs,
}

/// What a verifier accepted: the target's name, digest and size, the build
/// and the roster generation.
pub type Accepted = (String, String, u64, u64, u64);

/// The updater's decision on `case`, as `aterm update install` takes it before
/// placing the candidate: the roster under the pinned masters, admitted at the
/// durable floor; the appcast under its live machines, attributed, outside the
/// durable revocations; the tag; the target's signed name, size, ELF header and
/// digest; the source commit; the build against the durable floors.
pub fn updater_verdict(case: &Case, proofs: &Proofs) -> Result<Accepted, String> {
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
/// The installer's answer as an [`Accepted`], or why it refused. An acceptance
/// whose size, build or roster generation is no u64 (`7.0`, `-1`) reads as a
/// crash: no Rust verdict can name it.
pub fn installer_verdict(answer: &str) -> Result<Accepted, String> {
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
pub enum Rule {
    /// The updater's chain decides; the installer must decide the same.
    Shared,
    /// The installer's own, stricter than the updater by design: the signed
    /// origin bind, the source commit's spelling, an explicit schema, and a
    /// whole-roster refusal when any member is malformed.
    Installer,
}

pub type Mutation = (&'static str, Rule, fn(&mut Case, &Keys));

/// One rule each, across the roster, the appcast, the durable policy, the
/// candidate and the pinned masters; each changes the verdict or pins down
/// that it does not.
pub const MUTATIONS: &[Mutation] = &[
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
    (
        "the static verifiers' signed receipts beside it",
        Rule::Shared,
        |c, _| {
            for arch in ["x86_64", "aarch64"] {
                c.set(
                    &format!("linux_verifier_{arch}"),
                    &format!("\"aterm-bootstrap-verify-0.100.0-linux-{arch}\""),
                );
                c.set(
                    &format!("linux_verifier_{arch}_sha256"),
                    &format!("{:?}", "e".repeat(64)),
                );
                c.set(&format!("linux_verifier_{arch}_size"), "2000000");
            }
            c.set("linux_verifier_interface", "1");
        },
    ),
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
    (
        "a roster after a byte order mark",
        Rule::Installer,
        |c, _| c.roster_lead = "\u{feff}".into(),
    ),
    (
        "an appcast after a byte order mark",
        Rule::Installer,
        |c, _| c.appcast[0].0.insert(0, '\u{feff}'),
    ),
    (
        "an appcast date-time on a leap second",
        Rule::Installer,
        |c, _| c.set("published", "2026-06-30T23:59:60Z"),
    ),
    ("an appcast date in year 0", Rule::Installer, |c, _| {
        c.set("published", "0000-01-01")
    }),
];

/// Every mutation alone, then seeded combinations of two and three.
pub fn corpus(keys: &Keys) -> Vec<Case> {
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
pub struct Tally {
    pub accepted: usize,
    pub refused: usize,
    pub wrong: Vec<String>,
}

impl Tally {
    /// No disagreement, and both verdicts reached many times over.
    pub fn assert_agrees(&self) {
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

static LAID: AtomicU32 = AtomicU32::new(0);

fn fresh(scratch: &Scratch, what: &str) -> PathBuf {
    scratch.mkdir(&format!("{what}.{}", LAID.fetch_add(1, Ordering::Relaxed)))
}

/// `v` over the corpus: each case must get the updater's verdict, or a refusal where an
/// installer-only rule is broken.
pub fn differential(scratch: &Scratch, v: &impl Verifier) -> Tally {
    let keys = Keys::new();
    let cases = corpus(&keys);
    let root = fresh(scratch, "corpus");
    let laid: Vec<Laid> = cases
        .iter()
        .enumerate()
        .map(|(n, case)| case.lay(&keys, &root.join(n.to_string())))
        .collect();
    let runs: Vec<(&Case, &Laid)> = cases.iter().zip(&laid).collect();
    let answers = v.mains(scratch, &runs);
    assert_eq!(answers.len(), cases.len(), "one answer per case");
    let mut tally = Tally {
        accepted: 0,
        refused: 0,
        wrong: Vec::new(),
    };
    for ((case, laid), answer) in runs.iter().zip(answers) {
        let updater = updater_verdict(case, &laid.proofs);
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

/// The installer accepts exactly what the updater would install, naming the same target,
/// digest, size, build and roster generation, and refuses wherever one of its own
/// stricter rules is broken.
pub fn agrees_with_the_updater_on_a_generated_corpus(scratch: &Scratch, v: &impl Verifier) {
    differential(scratch, v).assert_agrees();
}

/// One run of `linux_release_verify` as the installer runs it: its exit status, what it
/// printed and why it refused.
pub fn verify_once(scratch: &Scratch, v: &impl Verifier, case: &Case, laid: &Laid) -> Rec {
    let mut lib = v.lib(&case.pins);
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

/// The script's own exits: a proof it accepts prints the signed target for the installer
/// to read, and a refusal exits 1 saying why. A tampered roster and a tampered appcast are
/// each refused.
pub fn a_verdict_is_the_scripts_exit_status_and_its_one_line(scratch: &Scratch, v: &impl Verifier) {
    let keys = Keys::new();
    let good = Case::base(&keys);
    let laid = good.lay(&keys, &fresh(scratch, "good"));
    let accepted = verify_once(scratch, v, &good, &laid);
    assert_eq!(accepted.rc, 0, "{}", accepted.err);
    let want = updater_verdict(&good, &laid.proofs).expect("the updater accepts it");
    assert_eq!(
        accepted.out,
        format!("{}\t{}\t{}\t{}\t{}", want.0, want.1, want.2, want.3, want.4)
    );
    for (name, tamper) in [
        (
            "appcast",
            (|c: &mut Case| c.appcast_tamper = true) as fn(&mut Case),
        ),
        ("roster", |c: &mut Case| c.roster_tamper = true),
    ] {
        let mut bad = good.clone();
        tamper(&mut bad);
        let laid = bad.lay(&keys, &fresh(scratch, name));
        let refused = verify_once(scratch, v, &bad, &laid);
        assert_eq!(refused.rc, 1, "a tampered {name}: {}", refused.out);
        assert_eq!(refused.out, "");
        let want = match name {
            "roster" => "roster is not signed by the pinned paper master",
            _ => "appcast has no authorized live machine signature",
        };
        assert_eq!(
            refused.err,
            format!("install.sh: Linux verification refused: {want}"),
            "a tampered {name}"
        );
    }
}

/// A machine key Python's strict base64 cannot read (too short, or its `=` dropped) is an
/// invalid key, and a second spelling of a key's bytes is a noncanonical one, in the same
/// words from either verifier.
pub fn a_misspelt_machine_key_is_refused_in_its_own_words(scratch: &Scratch, v: &impl Verifier) {
    let keys = Keys::new();
    let key = Keys::public(&keys.machine);
    let spellings = [
        ("AAA".to_string(), "invalid roster machine key"),
        (
            key.trim_end_matches('=').to_string(),
            "invalid roster machine key",
        ),
        (alias(&key), "noncanonical roster machine key"),
    ];
    let cases: Vec<Case> = spellings
        .iter()
        .map(|(spelt, _)| {
            let mut case = Case::base(&keys);
            case.label = format!("machine key {spelt:?}");
            case.machines[0].pubkey.clone_from(spelt);
            case
        })
        .collect();
    let root = fresh(scratch, "keys");
    let laid: Vec<Laid> = cases
        .iter()
        .enumerate()
        .map(|(n, case)| case.lay(&keys, &root.join(n.to_string())))
        .collect();
    let runs: Vec<(&Case, &Laid)> = cases.iter().zip(&laid).collect();
    let answers = v.mains(scratch, &runs);
    for ((spelt, why), answer) in spellings.iter().zip(&answers) {
        assert_eq!(answer, &format!("refused\t{why}"), "{spelt:?}");
    }
}

/// The candidate is read as itself: a symlink to the very bytes is refused.
pub fn a_symlinked_candidate_is_refused(scratch: &Scratch, v: &impl Verifier) {
    let keys = Keys::new();
    let case = Case::base(&keys);
    let dir = fresh(scratch, "link");
    let mut laid = case.lay(&keys, &dir);
    let alias = dir.join("alias");
    symlink(&laid.candidate, &alias);
    laid.candidate = alias;
    let run = verify_once(scratch, v, &case, &laid);
    assert_eq!(run.rc, 1, "{}", run.out);
    assert!(run.err.contains("type"), "{}", run.err);
}

/// The durable policy binds the installed file: the floors, revocations and rejected
/// build are read only while the target is the identity the record names, a trial is read
/// by its phase and its identities, the record must be the updater's own private file of
/// the schema it writes, every number in it a u64 and the target a file no other account
/// can write.
pub fn the_durable_policy_binds_the_installed_file(scratch: &Scratch, v: &impl Verifier) {
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
    let fresh_digest = "f".repeat(64);
    let root = fresh(scratch, "policy");
    let rows: [(&str, String, Vec<u8>, bool); 8] = [
        ("plain", String::new(), raw.clone(), true),
        (
            "prepared",
            trial("Prepared", false, &fresh_digest),
            raw.clone(),
            true,
        ),
        (
            "rolling-back",
            trial("RollbackPrepared", false, &fresh_digest),
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
                 sha256 = {fresh_digest:?}\n[trial.new]\nbuild = 100\nsha256 = {digest:?}\n"
            ),
            raw.clone(),
            true,
        ),
    ];
    let mut stores = Vec::new();
    for (name, tail, bytes, here) in &rows {
        let store = root.join(name);
        std::fs::create_dir_all(&store).expect("mkdir");
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
    let linked = root.join("linked");
    std::fs::create_dir_all(&linked).expect("mkdir");
    let elsewhere = root.join("elsewhere-dir");
    std::fs::create_dir_all(&elsewhere).expect("mkdir");
    symlink(&elsewhere, &linked.join(".aterm-update"));
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
            trial("RollbackPrepared", false, &fresh_digest),
            &raw,
            private,
            "installed identity",
        ),
        (
            "prepared-from-another-identity",
            (&installed, &format!("sha256 = {fresh_digest:?}\n")),
            trial("Prepared", false, &fresh_digest),
            &raw,
            private,
            "unresolved",
        ),
        (
            "prepared-then-replaced",
            ("", ""),
            trial("Prepared", false, &fresh_digest),
            &tampered,
            private,
            "unresolved",
        ),
        (
            "rollback-over-a-third-binary",
            ("", ""),
            trial("RollbackPrepared", false, &fresh_digest),
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
            (&installed, &format!("sha256 = {fresh_digest:?}\n")),
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
        let store = root.join(name);
        std::fs::create_dir_all(&store).expect("mkdir");
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
    let answers = v.policies(scratch, &refs);
    let ok = |i: usize| {
        answers[i]
            .as_ref()
            .unwrap_or_else(|e| panic!("{}: {e}", rows[i].0))
    };
    assert_eq!(ok(0).floors, [99, 98, 7]);
    assert_eq!(ok(0).revoked_machines, ["old-machine"]);
    assert_eq!(ok(0).rejected_build, 97);
    assert_eq!(
        ok(1).floors,
        [99, 98, 7],
        "a prepared trial keeps the floors"
    );
    assert_eq!(
        ok(2).rejected_build,
        100,
        "a rollback rejects the new build"
    );
    assert_eq!(
        ok(3).floors,
        [99, 98, 7],
        "a healthy trial reads as installed"
    );
    assert_eq!(
        ok(7).rejected_build,
        100,
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

/// A user-owned private ancestor shields a group-writable child, a symlinked store is
/// refused, and a world-writable ancestor before any private one (the system's /tmp) is
/// refused.
pub fn a_private_ancestor_shields_writable_children_but_not_earlier_ancestors(
    scratch: &Scratch,
    v: &impl Verifier,
) {
    let private = fresh(scratch, "shield");
    chmod(&private, 0o700);
    let writable = private.join("lib");
    std::fs::create_dir(&writable).expect("mkdir");
    chmod(&writable, 0o775);
    let alias = private.join("alias");
    symlink(&writable, &alias);
    let shared = aterm_tempfile::Builder::new()
        .prefix("install-sh-unshielded.")
        .tempdir_in("/tmp")
        .expect("a dir under /tmp");
    let unshielded = shared.path().canonicalize().expect("its physical path");
    let answers = v.policies(scratch, &[&writable, &alias, &unshielded]);
    assert_eq!(answers[0].as_ref().expect("shielded").floors, [0, 0, 0]);
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

/// Whether a directory of this user's that no other account can enter (mode `0700`, as
/// `$XDG_RUNTIME_DIR` is) contains `dir`.
pub fn shielded(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    // SAFETY: Getuid has no preconditions and cannot fail.
    let me = unsafe { libc::getuid() };
    dir.ancestors()
        .any(|a| std::fs::symlink_metadata(a).is_ok_and(|m| m.uid() == me && m.mode() & 0o077 == 0))
}

/// What the installer makes, its own verifier admits, whatever the caller's umask:
/// `ensure_dirs_writable` adds group and other write to the umask (and keeps its read
/// bits), so a missing `~/.local/lib/aterm/bin` is never made writable by others under
/// Ubuntu's `002`; the pre-flight (`--store-path`) judges the existing part of a store
/// before anything is downloaded, and names the remedy.
pub fn what_the_installer_makes_its_verifier_admits_under_any_umask(
    scratch: &Scratch,
    v: &impl Verifier,
) {
    use std::os::unix::fs::MetadataExt as _;
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
    let recs = lib.run(scratch);
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
    }
    let stores: Vec<&Path> = made.iter().map(|(_, store)| store.as_path()).collect();
    for (store, policy) in stores.iter().zip(v.policies(scratch, &stores)) {
        assert!(policy.is_ok(), "{}: {policy:?}", store.display());
    }
    // Default ACLs, and `setfacl`, are Linux's: macOS (where aterm-bootstrap-verify runs
    // this too) has neither.
    if cfg!(target_os = "linux") {
        // Below a folder with a default ACL, Linux makes a folder with the modes that ACL
        // passes on, whatever the umask (0777 under one that lets every account write), and
        // the installer then takes group and other write back, the ACL's mask with them. The
        // pre-flight's answer while the folders are still to be made, a dry run's, stands.
        let inherited: Vec<PathBuf> = ["g::rwx,o::rwx", "u:nobody:rwx"]
            .iter()
            .enumerate()
            .map(|(n, spec)| {
                let parent = open_root.join(format!("acl{n}"));
                std::fs::create_dir(&parent).expect("mkdir");
                chmod(&parent, 0o755);
                let set = run(aterm_uds::spawnfd::command("setfacl")
                    .args(["-d", "-m", spec])
                    .arg(&parent));
                assert_eq!(set.code(), 0, "setfacl -d -m {spec}: {}", set.all());
                parent.join(".local/lib/aterm/bin")
            })
            .collect();
        let inherited: Vec<&Path> = inherited.iter().map(PathBuf::as_path).collect();
        for answer in v.preflights(scratch, &inherited) {
            assert!(answer.starts_with("ok\t"), "still to be made: {answer}");
        }
        let mut lib = Lib::new();
        let calls: Vec<usize> = inherited
            .iter()
            .map(|store| {
                lib.call(
                    format!("umask 022; ensure_dirs_writable {}", q(s(store))),
                    &[],
                )
            })
            .collect();
        let recs = lib.run(scratch);
        for (store, call) in inherited.iter().zip(calls) {
            assert_eq!(recs[call].rc, 0, "{}: {}", store.display(), recs[call].err);
            for dir in store.ancestors().take(4) {
                let got = std::fs::metadata(dir).expect("made").mode() & 0o7777;
                assert_eq!(format!("{got:o}"), "755", "{}", dir.display());
            }
        }
        for answer in v.preflights(scratch, &inherited) {
            assert!(answer.starts_with("ok\t"), "made: {answer}");
        }
        for (store, policy) in inherited.iter().zip(v.policies(scratch, &inherited)) {
            assert!(policy.is_ok(), "{}: {policy:?}", store.display());
        }
    }
    let shared = open_root.join("shared");
    std::fs::create_dir(&shared).expect("mkdir");
    chmod(&shared, 0o757);
    let theirs_store = shared.join("lib/aterm/bin");
    let mut later: Vec<PathBuf> = stores.iter().map(|store| store.join("not/yet")).collect();
    later.push(theirs_store.clone());
    let later: Vec<&Path> = later.iter().map(PathBuf::as_path).collect();
    let answers = v.preflights(scratch, &later);
    let (last, admitted) = answers.split_last().expect("answers");
    for answer in admitted {
        assert!(answer.starts_with("ok\t"), "{answer}");
    }
    let pins: Vec<String> = aterm_update_core::pins::PAPER_MASTER_PUBKEYS
        .iter()
        .map(|key| (*key).to_string())
        .collect();
    let mut lib = v.lib(&pins);
    let ours = lib.call(
        format!("linux_store_preflight {}", q(s(&made[0].1))),
        &["LINUX_STORE_SKIP"],
    );
    let theirs = lib.call(
        format!("linux_store_preflight {}", q(s(&theirs_store))),
        &["LINUX_STORE_SKIP"],
    );
    let recs = lib.run(scratch);
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

/// A first install that recorded its policy before placing the binary keeps every floor
/// on the retry; an enrolled target that vanished, or a disabled one whose trial awaits
/// recovery, is refused.
pub fn a_disabled_fresh_install_keeps_its_policy_on_retry(scratch: &Scratch, v: &impl Verifier) {
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
    let root = fresh(scratch, "fresh");
    let at = |name: &str| {
        let store = root.join(name);
        std::fs::create_dir_all(&store).expect("mkdir");
        store
    };
    let mut stores = Vec::new();
    for (n, identity) in identities.iter().enumerate() {
        let store = at(&n.to_string());
        state(&store, &(header(&store, false) + identity));
        stores.push(store);
    }
    let enrolled = at("enrolled");
    state(&enrolled, &(header(&enrolled, true) + &identities[1]));
    stores.push(enrolled);
    let unbuilt = at("unbuilt");
    state(
        &unbuilt,
        &(header(&unbuilt, false) + "[installed]\nbuild = 5\nsha256 = \"\"\n"),
    );
    stores.push(unbuilt);
    let trial = at("trial");
    state(
        &trial,
        &(header(&trial, false) + &identities[0] + "[trial]\nphase = \"Prepared\"\n"),
    );
    stores.push(trial);
    let refs: Vec<&Path> = stores.iter().map(PathBuf::as_path).collect();
    let answers = v.policies(scratch, &refs);
    for answer in &answers[..2] {
        let policy = answer.as_ref().expect("a disabled fresh install's policy");
        assert_eq!(policy.floors, [101, 99, 7]);
        assert_eq!(policy.revoked_machines, ["revoked-machine"]);
        assert_eq!(policy.rejected_build, 98);
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

/// A stolen key's floor, recorded against its machine, goes with the roster that revokes
/// it, or with this client's durable revocation of it, as the updater's
/// `FloorSources::step` withdraws it; an unattributed floor, one a machine still trusted
/// raised, or one its sources do not explain, stands; a source above the floor (a hand
/// edit) is capped at it; and sources that are not a table are refused.
pub fn a_revoked_machines_floor_is_withdrawn(scratch: &Scratch, v: &impl Verifier) {
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
    let root = fresh(scratch, "floor");
    let mut laid = Vec::new();
    for (name, min_build, durable, table, _) in &sources {
        let at = case.lay(&keys, &root.join(name));
        state(
            &at.store,
            &format!(
                "schema = 1\ntarget = {:?}\nenabled = false\nhigh_water = 0\nmin_build = {min_build}\n\
                 roster_floor = 0\nrevoked_machines = [{durable}]\nrejected_build = 0\n\
                 [installed]\nbuild = 0\nsha256 = \"\"\n{table}",
                s(&at.store.join("aterm"))
            ),
        );
        laid.push(at);
    }
    let runs: Vec<(&Case, &Laid)> = laid.iter().map(|at| (&case, at)).collect();
    let answers = v.mains(scratch, &runs);
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

/// The roster's `valid_until` and its signer's `not_after` are exclusive deadlines, read
/// as the updater's `Roster::admit` and `Roster::machine` read them: the second before
/// admits the release, the deadline's own second refuses it.
pub fn a_deadline_refuses_from_its_own_second(scratch: &Scratch, v: &impl Verifier) {
    let keys = Keys::new();
    const STAMP: &str = "2030-01-01T00:00:00Z";
    let at = aterm_types::rfc3339::parse_utc(STAMP).expect("a deadline");
    let mut roster_lapses = Case::base(&keys);
    roster_lapses.valid_until = STAMP.into();
    let mut signer_lapses = Case::base(&keys);
    signer_lapses.machines[0].extra = format!("not_after = {STAMP:?}\n");
    let root = fresh(scratch, "deadline");
    let cases = [("roster", &roster_lapses), ("signer", &signer_lapses)];
    let laid: Vec<Laid> = cases
        .iter()
        .map(|(name, case)| case.lay(&keys, &root.join(name)))
        .collect();
    let (mut asks, mut updater) = (Vec::new(), Vec::new());
    for ((name, case), laid) in cases.iter().zip(&laid) {
        let pins: Vec<&str> = case.pins.iter().map(String::as_str).collect();
        let verified = verify_roster(&pins, laid.proofs.roster.clone(), &laid.proofs.roster_sig)
            .expect("the master signed it");
        let roster = Roster::parse(&verified).expect("a roster");
        for now in [at - 1, at] {
            let admitted = roster.admit(0, now).and_then(|()| {
                roster.authorize_appcast(&laid.proofs.appcast, &laid.proofs.appcast_sig, now)
            });
            updater.push((name, now, admitted.is_ok()));
            asks.push((*case, laid, now));
        }
    }
    let answers = v.manifests_at(scratch, &asks);
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

type HeadRow = (
    &'static str,
    fn(&mut Case, &mut Case, &Keys),
    Pair,
    Result<u64, &'static str>,
);

fn stolen_floor(c: &mut Case) {
    c.revoked = vec!["thief".into()];
    c.policy.min_build = 9_999_999_999;
    c.policy.sources = "[min_build_sources.machines]\nthief = 9999999999\n".into();
}

/// An older release is installed only beneath the channel's current signed policy: the
/// head's own floor applies to it, beside the durable floor less what revoked machines
/// raised; the head is admitted under the durable policy, the candidate under the durable
/// revocations too, and the head must be at least the candidate (a head at its version
/// stands); and a head that does not verify, or is half published, refuses the install
/// rather than falling back to the older proof alone.
pub fn an_older_release_is_judged_under_the_current_signed_policy(
    scratch: &Scratch,
    v: &impl Verifier,
) {
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
    let root = fresh(scratch, "head");
    let mut laid = Vec::new();
    for (name, edit, pair, _) in &rows {
        let (mut c, mut h) = (case.clone(), head.clone());
        edit(&mut c, &mut h, &keys);
        let at = c.lay(&keys, &root.join(name));
        let bytes = h.appcast_text().into_bytes();
        let sig = match pair {
            Pair::Signed(by) => by.sign(&keys, &bytes),
            _ => vec![0; 64],
        };
        if !matches!(pair, Pair::SignatureOnly) {
            write(&at.release.join("aterm-policy-appcast.toml"), &bytes);
        }
        if !matches!(pair, Pair::AppcastOnly) {
            write(&at.release.join("aterm-policy-appcast.toml.sig"), &sig);
        }
        laid.push((c, at));
    }
    let runs: Vec<(&Case, &Laid)> = laid.iter().map(|(c, at)| (c, at)).collect();
    let answers = v.mains(scratch, &runs);
    for ((name, _, _, want), answer) in rows.iter().zip(answers) {
        match (installer_verdict(&answer), want) {
            (Ok(accepted), Ok(build)) => assert_eq!(accepted.3, *build, "{name}"),
            (Err(why), Err(key)) => assert!(why.contains(key), "{name}: {why}"),
            (got, _) => panic!("{name}: {got:?}"),
        }
    }
}

/// The install-path walk is the updater's on this machine's own accounts and directories:
/// a directory of each mode, on this user's primary group and on every other group it
/// belongs to, laid where nothing above it is private and again below a private directory,
/// is admitted or refused alike by the verifier's `check_install_path` and the updater's
/// `install_path::refusal`, as is one on the primary group whose access ACL gives another
/// account its group class's bits (the mode unchanged); and each of those groups is
/// private, or not, to both.
#[cfg(target_os = "linux")]
pub fn the_install_path_walk_is_the_updaters_on_this_machine(scratch: &Scratch, v: &impl Verifier) {
    use aterm_update::linux::install_path::{
        grant_group_class_by_acl, is_user_private_group, refusal,
    };
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
    let shield = fresh(scratch, "shielded");
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
    let refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    let walks = v.walks(scratch, &refs);
    let probed: Vec<u32> = gids.iter().copied().chain([0, 65534]).collect();
    let facts = v.private_groups(scratch, &probed);
    let mut refused = 0;
    for (dir, answer) in dirs.iter().zip(&walks) {
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
    for (gid, private) in probed.iter().zip(facts) {
        assert_eq!(private, is_user_private_group(*gid), "gid {gid}");
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
