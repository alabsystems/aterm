// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! LINEAGE — which compiler built a toolchain or a payload, decided by CONTENT and never by a name (owner order 2026-09-29: "NO STOCK RUST IN THE TOOLCHAIN FOR TRUST").

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use aterm_digest::Sha256;

/// The receipt a signed bundle carries beside its compiler (the producer's stamp writes it).
pub const LINEAGE_FILE: &str = "LINEAGE";
/// The trust repo's pack-cut receipt (scripts/promote-toolchain.sh seals it).
pub const SEED_MINT_FILE: &str = "SEED-MINT";
/// `schema=` of a [`LINEAGE_FILE`].
pub const LINEAGE_SCHEMA: &str = "atpkg-lineage-v1";
/// `schema=` of a [`SEED_MINT_FILE`].
pub const SEED_MINT_SCHEMA: &str = "trust-seed-mint-v1";

/// The double-compile relations a receipt may record (the trust repo's D12):
/// `lineage-disjoint` (a stock-lineage seed, the only diverse one), `same-lineage` (another
/// Trust compiler) and `pin-ancestor` (an earlier pin).
const ACCEPTED_RELATIONS: [&str; 3] = ["lineage-disjoint", "same-lineage", "pin-ancestor"];

/// The largest receipt read. A real SEED-MINT lists every stage2 file the double compile
/// compared — thousands of lines, well under a megabyte.
const RECEIPT_LIMIT: usize = 64 << 20;

/// How long `trustc -vV` may take before the compiler counts as not answering.
const VERSION_PROBE: std::time::Duration = std::time::Duration::from_secs(60);

/// What an absent lineage receipt means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineagePolicy {
    /// No `LINEAGE` and no `SEED-MINT`: refused.
    Require,
    /// No receipt: admitted, recorded as `unproven` and reported as not enforced.
    Dormant,
}

/// How the admitted compiler's lineage was proven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineageProof {
    /// `LINEAGE` in the toolchain root, matched against the shipped bytes.
    Lineage {
        /// sha256 of the `SEED-MINT` the record names.
        receipt_sha256: String,
        /// The double compile's `ddc_seed_relation`.
        relation: String,
    },
    /// `SEED-MINT` in the toolchain root, matched against the seal's bytes.
    SeedMint {
        /// sha256 of the `SEED-MINT` file.
        receipt_sha256: String,
        /// The double compile's `ddc_seed_relation`.
        relation: String,
    },
    /// Neither receipt — admitted only under [`LineagePolicy::Dormant`].
    Unproven,
}

impl LineageProof {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Lineage { .. } => "lineage",
            Self::SeedMint { .. } => "seed-mint",
            Self::Unproven => "unproven",
        }
    }
}

/// What `trustc -vV` said, admitted ([`parse_version_verbose`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    /// `release:` — the Rust compatibility number (`1.99.0-dev`).
    pub release: String,
    /// `commit-hash:` — 40 lowercase hex.
    pub commit: String,
    /// `host:` — the triple the compiler runs on.
    pub host: String,
    /// `trust:` — the Trust product version.
    pub trust: String,
}

/// A compiler admitted by content ([`admit_content`]): its identity and the digests of the three files that make it a compiler.
#[derive(Debug, Clone)]
pub struct CompilerContent {
    /// The canonical `bin/` directory.
    pub bin_dir: PathBuf,
    /// The canonical toolchain root (`bin/`'s parent).
    pub root: PathBuf,
    /// `trustc -vV`.
    pub version: VersionInfo,
    /// sha256 of `bin/trustc`.
    pub trustc_sha256: String,
    /// sha256 of `bin/targo`.
    pub targo_sha256: String,
    /// The driver's path relative to the root (`lib/librustc_driver-<hash>.dylib`).
    pub driver_rel: String,
    /// sha256 of the driver.
    pub driver_sha256: String,
}

/// A compiler admitted by content AND lineage ([`check_compiler`]).
#[derive(Debug, Clone)]
pub struct Compiler {
    /// The content half.
    pub content: CompilerContent,
    /// The lineage half.
    pub lineage: LineageProof,
}

impl Compiler {
    /// The provenance key/value pairs the packers embed in a manifest's `[artifact.provenance]`.
    #[must_use]
    pub fn record(&self) -> Vec<(&'static str, String)> {
        let c = &self.content;
        let (receipt, relation) = match &self.lineage {
            LineageProof::Lineage {
                receipt_sha256,
                relation,
            }
            | LineageProof::SeedMint {
                receipt_sha256,
                relation,
            } => (receipt_sha256.clone(), relation.clone()),
            LineageProof::Unproven => ("none".to_string(), "none".to_string()),
        };
        vec![
            ("compiler_release", c.version.release.clone()),
            ("compiler_commit", c.version.commit.clone()),
            ("compiler_trust", c.version.trust.clone()),
            ("compiler_host", c.version.host.clone()),
            ("trustc_sha256", c.trustc_sha256.clone()),
            ("targo_sha256", c.targo_sha256.clone()),
            ("driver_sha256", c.driver_sha256.clone()),
            ("lineage", self.lineage.kind().to_string()),
            ("lineage_receipt_sha256", receipt),
            ("ddc_seed_relation", relation),
        ]
    }
}

/// Where a stock toolchain lives, read off a canonical path: rustup's directory (and a system `RUSTUP_HOME`'s `/usr/local/rustup/toolchains/<name>`), `~/.cargo`, Homebrew; `None` anywhere else.
#[must_use]
pub fn stock_location(path: &Path) -> Option<&'static str> {
    let mut s = path.to_string_lossy();
    if cfg!(windows) {
        s = s.replace('\\', "/").into();
    }
    if s.contains("/.rustup/") || s.contains("/rustup/toolchains/") {
        return Some("rustup's directory");
    }
    if s.contains("/.cargo/") {
        return Some("~/.cargo (rustup's proxies and cargo-installed tools)");
    }
    const HOMEBREW: [&str; 4] = [
        "/opt/homebrew/",
        "/usr/local/Cellar/",
        "/usr/local/Homebrew/",
        "/home/linuxbrew/.linuxbrew/",
    ];
    HOMEBREW
        .iter()
        .any(|p| s.starts_with(p))
        .then_some("Homebrew")
}

/// Ok when the file opens as a native object (Mach-O or ELF), else why not.
fn native_head(path: &Path) -> Result<(), String> {
    let head = crate::metadata_io::read_head(path, 8).map_err(|e| format!("cannot read: {e}"))?;
    if crate::macho::is_macho_header(&head) || head.starts_with(b"\x7fELF") {
        Ok(())
    } else if head.starts_with(b"#!") {
        Err(
            "is an interpreter script (`#!`) — a wrapper that runs some other compiler, \
             not a compiler"
                .to_string(),
        )
    } else {
        Err("is not a native executable (neither Mach-O nor ELF)".to_string())
    }
}

/// The toolchain file at `path`, admitted by location and head: its canonical path.
fn admit_file(path: &Path, what: &str, refusals: &mut Vec<String>) -> Option<PathBuf> {
    let canonical = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) => {
            refusals.push(format!("{what} {}: {e}", path.display()));
            return None;
        }
    };
    if let Some(place) = stock_location(&canonical) {
        refusals.push(format!(
            "{what} resolves into {place} ({}) — a stock toolchain's file",
            canonical.display()
        ));
        return None;
    }
    if !std::fs::metadata(&canonical).is_ok_and(|m| m.is_file()) {
        refusals.push(format!(
            "{what} {} is not a regular file",
            canonical.display()
        ));
        return None;
    }
    if let Err(why) = native_head(&canonical) {
        refusals.push(format!("{what} {} {why}", canonical.display()));
        return None;
    }
    Some(canonical)
}

/// The one `librustc_driver` under `<root>/lib`.
fn find_driver(root: &Path, refusals: &mut Vec<String>) -> Option<PathBuf> {
    let lib = root.join("lib");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&lib)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                        n.starts_with("librustc_driver-")
                            && (n.ends_with(".dylib") || n.ends_with(".so"))
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    match found.len() {
        1 => found.pop(),
        0 => {
            refusals.push(format!(
                "{} holds no librustc_driver-*.dylib/.so — a Trust toolchain carries its \
                 compiler driver there",
                lib.display()
            ));
            None
        }
        n => {
            refusals.push(format!(
                "{} holds {n} compiler drivers — a toolchain carries exactly one",
                lib.display()
            ));
            None
        }
    }
}

/// Admit `trustc -vV`'s output: `binary: trustc`, a `trust:` line, a 40-hex `commit-hash`, `release:` and `host:`, and a first line that is a Trust self-report whose short hash is the commit's prefix.
/// # Errors
/// One line per missing or inconsistent field.
pub fn parse_version_verbose(text: &str) -> Result<VersionInfo, Vec<String>> {
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()))
            .unwrap_or_default()
    };
    let first = text.lines().next().unwrap_or_default().trim();
    let (binary, trust, commit, release, host) = (
        field("binary:"),
        field("trust:"),
        field("commit-hash:"),
        field("release:"),
        field("host:"),
    );
    let mut refusals = Vec::new();
    if binary != "trustc" {
        refusals.push(format!("-vV says `binary: {binary}`, not `binary: trustc`"));
    }
    if trust.is_empty() {
        refusals.push(
            "-vV carries no `trust:` line — a stock rustc's report, whatever the file is \
             called"
                .to_string(),
        );
    }
    if !aterm_codec::hex::is_lower(&commit, 40) {
        refusals.push(format!(
            "-vV's commit-hash `{commit}` is not 40 lowercase hex"
        ));
    }
    if release.is_empty() || host.is_empty() {
        refusals.push("-vV carries no `release:` or no `host:` line".to_string());
    }
    let self_report = first
        .strip_prefix("rustc ")
        .and_then(|rest| parse_version(rest.as_bytes()))
        .filter(|(len, _)| first[6 + len..].starts_with(" (trustc "));
    match self_report {
        Some((_, short)) if commit.as_bytes().starts_with(short) => {}
        Some((_, short)) => refusals.push(format!(
            "-vV's first line names commit {} but its commit-hash is {commit}",
            String::from_utf8_lossy(short)
        )),
        None => refusals.push(format!(
            "-vV's first line `{first}` is not a Trust self-report (`rustc <release> (<commit> \
             <date>) (trustc <version>)`)"
        )),
    }
    if refusals.is_empty() {
        Ok(VersionInfo {
            release,
            commit,
            host,
            trust,
        })
    } else {
        Err(refusals)
    }
}

/// Run `<trustc> -vV` (bounded) and return its stdout.
fn probe_version(trustc: &Path) -> Result<String, String> {
    let mut cmd = aterm_uds::spawnfd::command(trustc);
    cmd.arg("-vV");
    let out =
        crate::doctor::output_bounded_capturing(&mut cmd, VERSION_PROBE).ok_or_else(|| {
            format!(
                "{} -vV did not run to completion within {}s",
                trustc.display(),
                VERSION_PROBE.as_secs()
            )
        })?;
    if !out.status.success() {
        return Err(format!(
            "{} -vV exited {}: {}",
            trustc.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The content half of [`check_compiler`]: location, head, identity and a clean scan of the compiler's own bytes.
/// # Errors
/// Every refusal found, one line each. A script or a stock location is refused before
/// anything is executed.
pub fn admit_content(bin_dir: &Path) -> Result<CompilerContent, Vec<String>> {
    admit_content_with(bin_dir, &probe_version)
}

/// [`admit_content`] with the `-vV` probe injected (the seam unit tests drive).
pub fn admit_content_with(
    bin_dir: &Path,
    probe: &dyn Fn(&Path) -> Result<String, String>,
) -> Result<CompilerContent, Vec<String>> {
    let mut refusals = Vec::new();
    let bin =
        std::fs::canonicalize(bin_dir).map_err(|e| vec![format!("{}: {e}", bin_dir.display())])?;
    if let Some(place) = stock_location(&bin) {
        return Err(vec![format!(
            "{} resolves into {place} — a stock toolchain, not a Trust one",
            bin.display()
        )]);
    }
    let Some(root) = bin.parent().map(Path::to_path_buf) else {
        return Err(vec![format!("{} has no parent directory", bin.display())]);
    };
    let trustc = admit_file(&bin.join("trustc"), "bin/trustc", &mut refusals);
    let targo = admit_file(&bin.join("targo"), "bin/targo", &mut refusals);
    let driver = find_driver(&root, &mut refusals)
        .and_then(|d| admit_file(&d, "the compiler driver", &mut refusals));
    let (Some(trustc), Some(targo), Some(driver)) = (trustc, targo, driver) else {
        return Err(refusals);
    };
    if !refusals.is_empty() {
        return Err(refusals);
    }
    let version = probe(&trustc)
        .map_err(|e| vec![e])
        .and_then(|text| parse_version_verbose(&text))
        .map_err(|lines| {
            lines
                .into_iter()
                .map(|l| format!("bin/trustc: {l}"))
                .collect::<Vec<_>>()
        })?;
    let mut digests = Vec::new();
    for (what, path) in [
        ("bin/trustc", &trustc),
        ("bin/targo", &targo),
        ("the compiler driver", &driver),
    ] {
        match scan_file(path, &version.commit, true) {
            Ok((hits, digest)) => {
                for (evidence, count) in hits {
                    refusals.push(format!(
                        "{what}'s own bytes carry {}",
                        Finding {
                            file: path.display().to_string(),
                            evidence,
                            count
                        }
                        .describe()
                    ));
                }
                digests.push(digest.unwrap_or_default());
            }
            Err(e) => refusals.push(format!("{what} {}: {e}", path.display())),
        }
    }
    if !refusals.is_empty() {
        return Err(refusals);
    }
    let driver_rel = driver
        .strip_prefix(&root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| driver.to_string_lossy().into_owned());
    let mut digests = digests.into_iter();
    Ok(CompilerContent {
        bin_dir: bin,
        root,
        version,
        trustc_sha256: digests.next().unwrap_or_default(),
        targo_sha256: digests.next().unwrap_or_default(),
        driver_rel,
        driver_sha256: digests.next().unwrap_or_default(),
    })
}

/// Admit the compiler at `bin_dir` by content ([`admit_content`]) and lineage ([`admit_lineage`]).
/// # Errors
/// Every refusal found, one line each.
pub fn check_compiler(bin_dir: &Path, policy: LineagePolicy) -> Result<Compiler, Vec<String>> {
    check_compiler_with(bin_dir, policy, &probe_version)
}

/// [`check_compiler`] with the `-vV` probe injected.
pub fn check_compiler_with(
    bin_dir: &Path,
    policy: LineagePolicy,
    probe: &dyn Fn(&Path) -> Result<String, String>,
) -> Result<Compiler, Vec<String>> {
    let content = admit_content_with(bin_dir, probe)?;
    let lineage = admit_lineage(&content, policy)?;
    Ok(Compiler { content, lineage })
}

/// The lineage half: `LINEAGE`, else `SEED-MINT`, else [`LineageProof::Unproven`] under [`LineagePolicy::Dormant`] only.
/// # Errors
/// Every refusal found, one line each.
pub fn admit_lineage(
    c: &CompilerContent,
    policy: LineagePolicy,
) -> Result<LineageProof, Vec<String>> {
    let lineage = c.root.join(LINEAGE_FILE);
    let seed_mint = c.root.join(SEED_MINT_FILE);
    if std::fs::symlink_metadata(&lineage).is_ok() {
        return lineage_record_refusals(c).map(|(receipt_sha256, relation)| {
            LineageProof::Lineage {
                receipt_sha256,
                relation,
            }
        });
    }
    if std::fs::symlink_metadata(&seed_mint).is_ok() {
        let mut refusals = seed_mint_refusals_inner(&c.root, &c.version.commit, true);
        let receipt = read_receipt(&seed_mint);
        let receipt_sha256 = receipt
            .as_ref()
            .map(|r| r.sha256.clone())
            .unwrap_or_default();
        let relation = receipt
            .as_ref()
            .map(|r| r.field("ddc_seed_relation"))
            .unwrap_or_default();
        if let Some(r) = &receipt {
            let compared: BTreeSet<&str> = r.files.iter().map(|(sha, _)| sha.as_str()).collect();
            for (what, sha) in [
                ("bin/trustc", &c.trustc_sha256),
                (c.driver_rel.as_str(), &c.driver_sha256),
            ] {
                if !compared.contains(sha.as_str()) {
                    refusals.push(format!(
                        "SEED-MINT does not cover this compiler: none of its compared files has \
                         {what}'s bytes (sha256 {sha})"
                    ));
                }
            }
        }
        return if refusals.is_empty() {
            Ok(LineageProof::SeedMint {
                receipt_sha256,
                relation,
            })
        } else {
            Err(refusals)
        };
    }
    match policy {
        LineagePolicy::Dormant => Ok(LineageProof::Unproven),
        LineagePolicy::Require => Err(vec![format!(
            "{} carries neither {LINEAGE_FILE} nor {SEED_MINT_FILE}: nothing proves this \
             compiler was built from the trust repo's stage0 pin and double-compiled \
             against another compiler, so its lineage is unproven (a seal bootstrapped from \
             a stock stage0 has clean bytes and no receipt — this is where it is caught)",
            c.root.display()
        )]),
    }
}

/// A parsed receipt: its `key=value` fields (first wins), its `compiler_file` and
/// `compiler_link` rows, and its own digest.
struct Receipt {
    fields: BTreeMap<String, String>,
    files: Vec<(String, String)>,
    links: Vec<(String, String)>,
    sha256: String,
}

impl Receipt {
    fn field(&self, key: &str) -> String {
        self.fields.get(key).cloned().unwrap_or_default()
    }
}

fn read_receipt(path: &Path) -> Option<Receipt> {
    let bytes = crate::metadata_io::read_bounded_regular(path, RECEIPT_LIMIT).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut r = Receipt {
        fields: BTreeMap::new(),
        files: Vec::new(),
        links: Vec::new(),
        sha256: aterm_codec::hex::encode_lower(&Sha256::digest(&bytes)),
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let pair = || {
            let v = value.trim_start();
            let (a, b) = v.split_once(char::is_whitespace).unwrap_or((v, ""));
            (a.to_string(), b.trim().to_string())
        };
        match key {
            "compiler_file" => r.files.push(pair()),
            "compiler_link" => r.links.push(pair()),
            _ => {
                r.fields
                    .entry(key.to_string())
                    .or_insert_with(|| value.to_string());
            }
        }
    }
    Some(r)
}

/// `rel` as a path strictly inside `root`: relative, with no `.`/`..`/empty component.
fn inside(root: &Path, rel: &str) -> Option<PathBuf> {
    if rel.is_empty() || rel.starts_with('/') {
        return None;
    }
    rel.split('/')
        .all(|c| !c.is_empty() && c != "." && c != "..")
        .then(|| root.join(rel))
}

/// The seal's SEED-MINT refusals, one line each; empty when `<seal>/SEED-MINT` is a double-compile-backed re-pin receipt of `commit` whose every compared file and link is in `<seal>` as compared — so it describes THIS seal.
#[must_use]
pub fn seed_mint_refusals(seal: &Path, commit: &str) -> Vec<String> {
    seed_mint_refusals_inner(seal, commit, true)
}

fn seed_mint_refusals_inner(seal: &Path, commit: &str, compare_files: bool) -> Vec<String> {
    let path = seal.join(SEED_MINT_FILE);
    if std::fs::symlink_metadata(&path).is_err() {
        return vec![
            "the seal carries no SEED-MINT, so it was not cut by the trust repo's \
             scripts/mint_and_prove_seed.sh (built from the src/stage0 pin, double-compiled \
             against another compiler, re-pinned). Cut it there, seal its \
             build/pack-mint-*/<host>/stage2 with scripts/promote-toolchain.sh, and point \
             SEAL at that seal"
                .to_string(),
        ];
    }
    let Some(r) = read_receipt(&path) else {
        return vec![format!(
            "SEED-MINT is not a readable regular file of at most {RECEIPT_LIMIT} bytes"
        )];
    };
    let mut out = Vec::new();
    if r.field("schema") != SEED_MINT_SCHEMA {
        out.push(format!(
            "SEED-MINT is not a {SEED_MINT_SCHEMA} receipt (schema '{}')",
            r.field("schema")
        ));
    }
    if r.field("source_commit") != commit {
        out.push(format!(
            "SEED-MINT was minted for {}, not for this seal's compiler {commit}",
            r.field("source_commit")
        ));
    }
    let relation = r.field("ddc_seed_relation");
    if relation == "reproducibility-control" {
        out.push(
            "SEED-MINT records a reproducibility control, not a double compile: its DDC seed \
             was the pinned seed's own commit. Re-cut with --ddc-seed naming another compiler \
             (a stock-lineage sysroot makes it diverse)"
                .to_string(),
        );
    } else if !ACCEPTED_RELATIONS.contains(&relation.as_str()) {
        out.push(format!(
            "SEED-MINT records no double compile against another compiler (ddc_seed_relation \
             '{relation}')"
        ));
    }
    let pinned = r.field("stage2_tree_sha256_pinned_seed");
    let ddc = r.field("stage2_tree_sha256_ddc_seed");
    if !(r.field("ddc_verdict") == "identical"
        && aterm_codec::hex::is_lower(&pinned, 64)
        && pinned == ddc)
    {
        out.push(format!(
            "SEED-MINT does not record byte-identical stage2 trees from the two seeds (verdict \
             '{}', pinned seed {pinned}, DDC seed {ddc})",
            r.field("ddc_verdict")
        ));
    }
    if !(r.field("repin_compiler_git_commit_hash") == commit && !r.field("repin_commit").is_empty())
    {
        out.push(format!(
            "SEED-MINT records no re-pin of src/stage0 to this compiler \
             (repin_compiler_git_commit_hash '{}')",
            r.field("repin_compiler_git_commit_hash")
        ));
    }
    if !compare_files {
        return out;
    }
    let mut bad: Vec<&str> = Vec::new();
    for (want, rel) in &r.files {
        let got = inside(seal, rel)
            .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file()))
            .and_then(|p| crate::tree::sha256_file(&p).ok());
        if got.as_deref() != Some(want.as_str()) {
            bad.push(rel);
        }
    }
    for (want, rel) in &r.links {
        let got = inside(seal, rel)
            .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
            .and_then(|p| std::fs::read_link(p).ok());
        if got.as_deref() != Some(Path::new(want)) {
            bad.push(rel);
        }
    }
    let n = r.files.len() + r.links.len();
    if n == 0 {
        out.push("SEED-MINT lists no compared file, so nothing ties it to this seal".to_string());
    }
    if !bad.is_empty() {
        let shown: String = bad.iter().take(3).map(|b| format!(" {b}")).collect();
        out.push(format!(
            "SEED-MINT does not describe this seal: {} of its {n} compared entries are missing \
             or differ here —{shown}",
            bad.len()
        ));
    }
    out
}

/// A `LINEAGE` record, checked against the compiler it sits beside: the record's commit
/// is the compiler's, the SEED-MINT it names is the one in the tree (and a double-compile
/// re-pin receipt of this commit), and every digest it names is the shipped bytes'.
fn lineage_record_refusals(c: &CompilerContent) -> Result<(String, String), Vec<String>> {
    let Some(rec) = read_receipt(&c.root.join(LINEAGE_FILE)) else {
        return Err(vec![format!(
            "{LINEAGE_FILE} is not a readable regular file"
        )]);
    };
    let mut out = Vec::new();
    if rec.field("schema") != LINEAGE_SCHEMA {
        out.push(format!(
            "{LINEAGE_FILE} is not an {LINEAGE_SCHEMA} record (schema '{}')",
            rec.field("schema")
        ));
    }
    if rec.field("compiler_commit") != c.version.commit {
        out.push(format!(
            "{LINEAGE_FILE} names compiler {}, but bin/trustc reports {}",
            rec.field("compiler_commit"),
            c.version.commit
        ));
    }
    for (key, what, actual) in [
        ("trustc_sha256", "bin/trustc", &c.trustc_sha256),
        ("targo_sha256", "bin/targo", &c.targo_sha256),
    ] {
        if rec.field(key) != *actual {
            out.push(format!(
                "{LINEAGE_FILE} records {what} as {} but the shipped bytes are {actual}",
                rec.field(key)
            ));
        }
    }
    let driver = rec.field("driver");
    let expect_driver = format!("{} {}", c.driver_sha256, c.driver_rel);
    if driver != expect_driver {
        out.push(format!(
            "{LINEAGE_FILE} records the driver as `{driver}` but the shipped one is \
             `{expect_driver}`"
        ));
    }
    let receipt_sha256 = rec.field("lineage_receipt_sha256");
    let relation = rec.field("ddc_seed_relation");
    match read_receipt(&c.root.join(SEED_MINT_FILE)) {
        Some(mint) if mint.sha256 == receipt_sha256 => {
            out.extend(seed_mint_refusals_inner(&c.root, &c.version.commit, false));
            if mint.field("ddc_seed_relation") != relation {
                out.push(format!(
                    "{LINEAGE_FILE} records ddc_seed_relation '{relation}' but its SEED-MINT \
                     says '{}'",
                    mint.field("ddc_seed_relation")
                ));
            }
        }
        Some(mint) => out.push(format!(
            "{LINEAGE_FILE} names SEED-MINT {receipt_sha256}, but the tree's SEED-MINT is {}",
            mint.sha256
        )),
        None => out.push(format!(
            "{LINEAGE_FILE} names SEED-MINT {receipt_sha256}, but the tree carries no SEED-MINT"
        )),
    }
    if out.is_empty() {
        Ok((receipt_sha256, relation))
    } else {
        Err(out)
    }
}

/// One kind of compiler evidence found in a payload.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Evidence {
    /// `/rustc/<40-hex>/` naming another compiler's commit.
    StdPath(String),
    /// A crate-metadata header naming another compiler.
    MetadataVersion(String),
    /// `rustc version …` (an LLVM producer or ELF `.comment`) of another compiler.
    ProducerVersion(String),
    /// A wasm `producers` section's `rustc` entry naming another compiler.
    WasmProducer(String),
    /// A path into a rustup toolchain directory (`<name>` is the toolchain).
    RustupPath(String),
}

impl fmt::Display for Evidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StdPath(h) => write!(
                f,
                "/rustc/{h}/ — a std built by another compiler (stock Rust's std embeds its \
                 commit there; Trust's embeds none)"
            ),
            Self::MetadataVersion(v) => write!(f, "crate metadata written by `{v}`"),
            Self::ProducerVersion(v) => write!(f, "an LLVM producer / .comment `{v}`"),
            Self::WasmProducer(v) => write!(f, "a wasm producers entry `rustc {v}`"),
            Self::RustupPath(t) => write!(
                f,
                "a path into rustup's toolchain `{t}` — bytes built from a stock toolchain's \
                 tree"
            ),
        }
    }
}

/// One file's evidence of one kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The file, relative to the scanned root.
    pub file: String,
    /// What was found.
    pub evidence: Evidence,
    /// How many times.
    pub count: usize,
}

impl Finding {
    /// `<file>: <count>× <evidence>`.
    #[must_use]
    pub fn describe(&self) -> String {
        format!("{}: {}× {}", self.file, self.count, self.evidence)
    }
}

/// The evidence of any compiler but the one whose commit-hash is `admitted` in the object at `path`, read streamed: the client's payload scan of one file, which the producer's tree scan (`atpkg-publish`'s `artifact::lineage::scan_tree`) runs over every compiled object it packs, and [`admit_content`] over a compiler's own bytes.
/// # Errors
/// The file cannot be read.
pub fn scan_object(path: &Path, admitted: &str) -> std::io::Result<BTreeMap<Evidence, usize>> {
    scan_file(path, admitted, false).map(|(hits, _)| hits)
}

/// The sha256 of the receipt at `path` (a `SEED-MINT`), as a `LINEAGE` record names it: `None` when it is not a readable regular file of at most the receipt limit.
#[must_use]
pub fn receipt_sha256(path: &Path) -> Option<String> {
    read_receipt(path).map(|r| r.sha256)
}

/// The window read past an anchor: the longest structure examined is a crate-metadata
/// version string with a distribution's description, read to at most this length
/// ([`is_metadata_version`]), so every anchor sees the same bytes wherever a read step
/// ends.
const TAIL: usize = 1024;
/// Bytes read per step.
const CHUNK: usize = 8 << 20;

/// Scan one file, streamed; with `hash`, its sha256 from the same read.
fn scan_file(
    path: &Path,
    admitted: &str,
    hash: bool,
) -> std::io::Result<(BTreeMap<Evidence, usize>, Option<String>)> {
    let f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let chunk = usize::try_from(len).map_or(CHUNK, |n| n.saturating_add(1).clamp(TAIL * 2, CHUNK));
    scan_stream(f, admitted, hash, chunk)
}

/// [`scan_file`]'s body over any reader, `chunk` bytes per step (more than [`TAIL`]).
fn scan_stream(
    mut f: impl std::io::Read,
    admitted: &str,
    hash: bool,
    chunk: usize,
) -> std::io::Result<(BTreeMap<Evidence, usize>, Option<String>)> {
    let chunk = chunk.max(TAIL * 2);
    let mut hasher = hash.then(Sha256::new);
    let mut m = Matcher {
        admitted: admitted.as_bytes(),
        hits: BTreeMap::new(),
    };
    let mut buf: Vec<u8> = Vec::with_capacity(chunk + TAIL + 2);
    let mut from = 0usize;
    loop {
        let old = buf.len();
        buf.resize(old + chunk, 0);
        let mut filled = old;
        while filled < buf.len() {
            match f.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        let eof = filled < buf.len();
        buf.truncate(filled);
        if let Some(h) = hasher.as_mut() {
            h.update(&buf[old..]);
        }
        let end = if eof {
            buf.len()
        } else {
            buf.len().saturating_sub(TAIL)
        };
        m.scan(&buf, from, end);
        if eof {
            break;
        }
        let keep = end.saturating_sub(2);
        buf.drain(..keep);
        from = end - keep;
    }
    Ok((
        m.hits,
        hasher.map(|h| aterm_codec::hex::encode_lower(&h.finalize())),
    ))
}

struct Matcher<'a> {
    admitted: &'a [u8],
    hits: BTreeMap<Evidence, usize>,
}

impl Matcher<'_> {
    fn hit(&mut self, e: Evidence) {
        *self.hits.entry(e).or_insert(0) += 1;
    }

    fn foreign(&self, short: &[u8]) -> bool {
        !self.admitted.starts_with(short)
    }

    /// Examine every anchor (`rust…`) starting in `[from, to)`. Bytes past `to` (up to
    /// [`TAIL`]) and two before `from` are readable.
    fn scan(&mut self, buf: &[u8], from: usize, to: usize) {
        let mut i = from;
        while i < to {
            let Some(p) = find_byte(&buf[i..to], b'r') else {
                return;
            };
            let at = i + p;
            self.examine(buf, at);
            i = at + 1;
        }
    }

    fn examine(&mut self, buf: &[u8], at: usize) {
        let s = &buf[at..];
        if !s.starts_with(b"rust") {
            return;
        }
        let before = |n: usize| at.checked_sub(n).map(|k| buf[k]);
        if before(1) == Some(b'/') && s.starts_with(b"rustc/") {
            if let Some(h) = s.get(6..46).filter(|h| is_lower_hex(h, 40))
                && s.get(46) == Some(&b'/')
                && h != self.admitted
            {
                self.hit(Evidence::StdPath(String::from_utf8_lossy(h).into_owned()));
            }
            return;
        }
        if let Some(rest) = s.strip_prefix(b"rustup/toolchains/") {
            let rooted =
                before(1) == Some(b'/') || (before(1) == Some(b'.') && before(2) == Some(b'/'));
            let name_len = rest
                .iter()
                .take(121)
                .position(|&b| b == b'/' || b.is_ascii_whitespace() || b < 0x20 || b == 0x7f)
                .unwrap_or(121);
            if rooted && (1..=120).contains(&name_len) && rest.get(name_len) == Some(&b'/') {
                self.hit(Evidence::RustupPath(
                    String::from_utf8_lossy(&rest[..name_len]).into_owned(),
                ));
            }
            return;
        }
        if before(1) == Some(5) && s.starts_with(b"rustc") && !s.starts_with(b"rustc ") {
            if let Some((len, n)) = leb128_forward(s.get(5..).unwrap_or_default())
                && let Some(v) = s.get(5 + n..5 + n + len)
                && let Some((used, short)) = parse_version(v)
                && used <= v.len()
                && self.foreign(short)
            {
                self.hit(Evidence::WasmProducer(
                    String::from_utf8_lossy(v).into_owned(),
                ));
            }
            return;
        }
        let Some(rest) = s.strip_prefix(b"rustc ") else {
            return;
        };
        if let Some(v) = rest.strip_prefix(b"version ") {
            if let Some((used, short)) = parse_version(v)
                && self.foreign(short)
            {
                let text = &s[..6 + 8 + used];
                self.hit(Evidence::ProducerVersion(
                    String::from_utf8_lossy(text).into_owned(),
                ));
            }
            return;
        }
        if let Some((used, short)) = parse_version(rest)
            && self.foreign(short)
        {
            let version = 6 + used;
            let one = before(1).filter(|&b| b < 0x80).map(usize::from);
            let two = before(1)
                .zip(before(2))
                .filter(|&(hi, lo)| hi != 0 && hi < 0x80 && lo >= 0x80)
                .map(|(hi, lo)| (usize::from(hi) << 7) | usize::from(lo & 0x7f));
            if let Some(len) = [one, two]
                .into_iter()
                .flatten()
                .find(|&len| is_metadata_version(s, version, len))
            {
                self.hit(Evidence::MetadataVersion(
                    String::from_utf8_lossy(&s[..len]).into_owned(),
                ));
            }
        }
    }
}

/// Whether the `len` bytes of `s` are a metadata header's version string whose parsed
/// version spans the first `version`: exactly that, or that and one ` (<description>)` of
/// printable bytes running to the end.
fn is_metadata_version(s: &[u8], version: usize, len: usize) -> bool {
    if len == version {
        return true;
    }
    if len > TAIL {
        return false;
    }
    let Some(desc) = s.get(version..len) else {
        return false;
    };
    desc.len() >= 4
        && desc.starts_with(b" (")
        && desc.ends_with(b")")
        && desc.iter().all(|&b| b >= 0x20 && b != 0x7f)
}

/// An unsigned LEB128 length of at most two bytes at the start of `s`: the value and the
/// bytes it took.
fn leb128_forward(s: &[u8]) -> Option<(usize, usize)> {
    let lo = *s.first()?;
    if lo < 0x80 {
        return Some((usize::from(lo), 1));
    }
    let hi = *s.get(1)?;
    (hi != 0 && hi < 0x80).then(|| ((usize::from(hi) << 7) | usize::from(lo & 0x7f), 2))
}

/// `<release> (<9-hex> <yyyy-mm-dd>)` at the start of `s`: the bytes it spans and the
/// short hash.
fn parse_version(s: &[u8]) -> Option<(usize, &[u8])> {
    let mut i = 0;
    for part in 0..3 {
        let start = i;
        while i < s.len() && s[i].is_ascii_digit() && i - start < 5 {
            i += 1;
        }
        if i == start {
            return None;
        }
        if part < 2 {
            if s.get(i) != Some(&b'.') {
                return None;
            }
            i += 1;
        }
    }
    if s.get(i) == Some(&b'-') {
        i += 1;
        let start = i;
        while i < s.len()
            && (s[i].is_ascii_lowercase() || s[i].is_ascii_digit() || s[i] == b'.')
            && i - start < 24
        {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    if s.get(i..i + 2)? != b" (" {
        return None;
    }
    i += 2;
    let short = s.get(i..i + 9).filter(|h| is_lower_hex(h, 9))?;
    i += 9;
    if s.get(i) != Some(&b' ') {
        return None;
    }
    i += 1;
    let date = s.get(i..i + 10)?;
    let date_ok = date.iter().enumerate().all(|(k, b)| match k {
        4 | 7 => *b == b'-',
        _ => b.is_ascii_digit(),
    });
    if !date_ok {
        return None;
    }
    i += 10;
    if s.get(i) != Some(&b')') {
        return None;
    }
    Some((i + 1, short))
}

/// Scanned bytes that are exactly `len` lowercase hex digits ([`aterm_codec::hex::is_lower`]).
fn is_lower_hex(s: &[u8], len: usize) -> bool {
    std::str::from_utf8(s).is_ok_and(|s| aterm_codec::hex::is_lower(s, len))
}

/// The first `needle` in `hay`: eight bytes at a time (a zero-byte test over the XOR),
/// then the tail.
fn find_byte(hay: &[u8], needle: u8) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    let rep = LO.wrapping_mul(u64::from(needle));
    let mut i = 0;
    while i + 8 <= hay.len() {
        let mut w = [0u8; 8];
        w.copy_from_slice(&hay[i..i + 8]);
        let x = u64::from_le_bytes(w) ^ rep;
        if x.wrapping_sub(LO) & !x & HI != 0 {
            break;
        }
        i += 8;
    }
    hay[i..].iter().position(|&b| b == needle).map(|p| p + i)
}

#[cfg(test)]
#[path = "lineage_tests.rs"]
mod tests;
