// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The candidate gate (`ship trust candidate`): a candidate Trust toolchain is published only
//! when, on this aterm commit and machine, it proves every obligation the published seal
//! proves (a gain offsets nothing; an audited retraction excepted, [`crate::trust_oblig`])
//! and gives every library a result within the wall budget. The lane alone would not notice
//! a prover regression, since every candidate inherits floors another prover recorded.
//!
//! The gate runs the whole-tree lane ([`crate::trust_lane`]) twice on this clean checkout,
//! the published seal's then the candidate's, each check under the memory guard with its
//! per-obligation record, and compares them ([`crate::trust_compare`]). The receipt
//! (`<out>/RECEIPT`, [`RECEIPT_MAGIC`]) is what the trust pack publisher admits. What admits
//! is atpkg-publish's pure rule (`atpkg_publish::channel::candidate`), which the `rustc`
//! group's publish answers in-process; `--admits` asks the same rule here.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use atpkg::manifest::TRUST_EXPOSES;
use atpkg_publish::channel::candidate::{
    self as admission, RECEIPT_MAGIC, Receipt, toolchain_dir, tree_root,
};

use crate::trust_lane::{self, Guard, Outcome, Scope};

/// The candidate's run waits this long for the base's trustd to retire (300 s after
/// its last grant), before it starts its own.
const CANDIDATE_TRUSTD_WAIT: Duration = Duration::from_secs(660);

/// A `trust candidate` invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    /// The staged toolchain to gate (`--candidate`), or to ask about (`--admits`).
    pub dir: PathBuf,
    pub admits: bool,
    /// `--base <bin>`: another base than the published seal (the receipt says so,
    /// and the publisher admits no such PASS).
    pub base: Option<PathBuf>,
    /// The published trust build, when the public channel cannot be read here.
    pub base_build: Option<u64>,
    pub budget: Duration,
    pub jobs: Option<usize>,
    pub out: Option<PathBuf>,
    pub min_free_gib: u64,
    /// Run each check under the memory guard ([`trust_lane::Guard`]), whose bounds
    /// follow: a footprint (six tenths of the RAM when unnamed), the swap in use, the
    /// pressure level, the free space (two GiB under `--min-free-gib` when unnamed).
    pub guard: bool,
    pub guard_footprint_gib: Option<u64>,
    pub guard_swap_gib: u64,
    pub guard_level: u32,
    pub guard_min_free_gib: Option<u64>,
    /// Run a host that refuses a library for memory anyway, for the measurement.
    pub measure_narrowed: bool,
    pub public_repo: String,
    pub retractions: Option<PathBuf>,
    pub trust_repo: Option<PathBuf>,
    pub receipts: Option<PathBuf>,
    pub dry_run: bool,
}

/// Parse `trust candidate …`.
///
/// # Errors
/// A flag this does not know, one without its value, or no directory named.
pub fn parse<'a>(it: &mut impl Iterator<Item = &'a str>) -> Result<Args, String> {
    let mut a = Args {
        dir: PathBuf::new(),
        admits: false,
        base: None,
        base_build: None,
        budget: trust_lane::DEFAULT_BUDGET,
        jobs: None,
        out: None,
        min_free_gib: 30,
        guard: true,
        guard_footprint_gib: None,
        guard_swap_gib: 8,
        guard_level: 10,
        guard_min_free_gib: None,
        measure_narrowed: false,
        public_repo: "alabsystems/aterm".into(),
        retractions: None,
        trust_repo: None,
        receipts: None,
        dry_run: false,
    };
    let (mut candidate, mut admits) = (None, None);
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .filter(|v| !v.is_empty() && !v.starts_with('-'))
                .ok_or_else(|| format!("trust candidate: {flag} needs a value"))
        };
        let count = |v: &str, what: &str| {
            v.parse::<u64>()
                .map_err(|_| format!("trust candidate: {what} takes a whole number, got {v:?}"))
        };
        match flag {
            "--candidate" => candidate = Some(PathBuf::from(value()?)),
            "--admits" => admits = Some(PathBuf::from(value()?)),
            "--base" => a.base = Some(PathBuf::from(value()?)),
            "--base-build" => a.base_build = Some(count(value()?, "--base-build")?),
            "--crate-budget" => {
                let s = count(value()?, "--crate-budget")?;
                if s == 0 {
                    return Err("trust candidate: --crate-budget takes 1 second or more".into());
                }
                a.budget = Duration::from_secs(s);
            }
            "--jobs" => {
                a.jobs = match value()? {
                    "auto" => None,
                    n => Some(
                        usize::try_from(count(n, "--jobs")?)
                            .ok()
                            .filter(|n| *n > 0)
                            .ok_or("trust candidate: --jobs takes a count or auto")?,
                    ),
                }
            }
            "--out" => a.out = Some(PathBuf::from(value()?)),
            "--min-free-gib" => a.min_free_gib = count(value()?, "--min-free-gib")?,
            "--no-guard" => a.guard = false,
            "--guard-footprint-gib" => {
                a.guard_footprint_gib = Some(count(value()?, "--guard-footprint-gib")?);
            }
            "--guard-swap-gib" => a.guard_swap_gib = count(value()?, "--guard-swap-gib")?,
            "--guard-level" => {
                a.guard_level = u32::try_from(count(value()?, "--guard-level")?)
                    .map_err(|_| "trust candidate: --guard-level takes a percentage")?;
            }
            "--guard-min-free-gib" => {
                a.guard_min_free_gib = Some(count(value()?, "--guard-min-free-gib")?);
            }
            "--measure-narrowed" => a.measure_narrowed = true,
            "--public-repo" => a.public_repo = value()?.to_string(),
            "--retractions" => a.retractions = Some(PathBuf::from(value()?)),
            "--trust-repo" => a.trust_repo = Some(PathBuf::from(value()?)),
            "--receipts" => a.receipts = Some(PathBuf::from(value()?)),
            "--dry-run" => a.dry_run = true,
            other => return Err(format!("trust candidate: unknown flag {other:?}")),
        }
    }
    if a.base.is_some() && a.base_build.is_some() {
        return Err(
            "trust candidate: --base names the base itself; --base-build names the published build in the store — give one".into(),
        );
    }
    match (candidate, admits) {
        (Some(c), None) => a.dir = c,
        (None, Some(d)) => {
            a.dir = d;
            a.admits = true;
        }
        _ => {
            return Err(
                "trust candidate: name --candidate <the staged toolchain> to gate it, or --admits <it> to ask about it".into(),
            );
        }
    }
    Ok(a)
}

/// Run `trust candidate` in the checkout `repo`: the exit status.
pub fn run(repo: &Path, args: &Args, out: &mut dyn Write) -> i32 {
    if args.admits {
        admits(repo, args, out)
    } else {
        gate(repo, args, out)
    }
}

// ---------------------------------------------------------------------------
// What the gate reads of the world
// ---------------------------------------------------------------------------

/// The `commit-hash:` its `trustc -vV` names, `None` when it names none.
fn trust_commit(bin: &Path) -> Option<String> {
    let c = crate::trust_facts::trustc_commit(bin);
    (c.len() == 40 && c.bytes().all(|b| b.is_ascii_hexdigit())).then_some(c)
}

/// The lanes' state root, found as the publisher's reader finds it
/// ([`admission::state_dir_from_env`]); `.local/state/aterm-auto` under the current
/// directory when `HOME` is unset, where that reader refuses to run.
fn state_dir() -> PathBuf {
    admission::state_dir_from_env().unwrap_or_else(|| PathBuf::from(".local/state/aterm-auto"))
}

/// The trust build the public channel's newest `atpkg-index-<n>` release pins:
/// `(n, build)`.
fn public_trust_build(slug: &str) -> Result<(u64, u64), String> {
    let gh = |args: &[&str]| {
        std::process::Command::new("gh")
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("gh: {e}"))
            .and_then(|o| {
                if o.status.success() {
                    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
                } else {
                    Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
                }
            })
    };
    let tags = gh(&[
        "release",
        "list",
        "-R",
        slug,
        "--exclude-drafts",
        "--limit",
        "200",
        "--json",
        "tagName",
        "-q",
        ".[].tagName",
    ])?;
    let n = tags
        .lines()
        .filter_map(|t| t.strip_prefix("atpkg-index-")?.parse::<u64>().ok())
        .max()
        .ok_or_else(|| format!("{slug} lists no published atpkg-index-<n> release"))?;
    let d = std::env::temp_dir().join(format!("trust-candidate-index-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    let dir = d.display().to_string();
    let got = gh(&[
        "release",
        "download",
        &format!("atpkg-index-{n}"),
        "-R",
        slug,
        "-p",
        "index.toml",
        "-D",
        &dir,
        "--clobber",
    ])
    .and_then(|_| fs::read_to_string(d.join("index.toml")).map_err(|e| e.to_string()));
    let _ = fs::remove_dir_all(&d);
    let build = index_trust_pin(&got?)
        .ok_or_else(|| format!("{slug} atpkg-index-{n}'s index.toml pins no trust build"))?;
    Ok((n, build))
}

/// The first `trust` build any channel of an index pins.
#[must_use]
pub fn index_trust_pin(index: &str) -> Option<u64> {
    let v: aterm_toml::Value = index.parse().ok()?;
    v.get("channels")?.as_array()?.iter().find_map(|c| {
        c.get("pin")?
            .get("trust")?
            .as_integer()
            .and_then(|b| u64::try_from(b).ok())
    })
}

/// `[programs.trust]` of the package client's `status.toml`: the build installed and
/// the tree_root the client verified.
#[must_use]
pub fn installed_trust(status: &str) -> (Option<u64>, Option<String>) {
    let v: Option<aterm_toml::Value> = status.parse().ok();
    let t = v.as_ref().and_then(|v| v.get("programs")?.get("trust"));
    (
        t.and_then(|t| t.get("installed_build")?.as_integer())
            .and_then(|b| u64::try_from(b).ok()),
        t.and_then(|t| t.get("tree_root")?.as_str())
            .map(str::to_string),
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    crate::trust_facts::hex(&aterm_digest::Sha256::digest(bytes))
}

/// `--admits`: what the publisher asks, asked here.
fn admits(repo: &Path, args: &Args, out: &mut dyn Write) -> i32 {
    let checked = (|| {
        let dir = toolchain_dir(&args.dir)?;
        let root = tree_root(&dir)?;
        let (pin, from) = match args.base_build {
            Some(b) => (b, format!("--base-build {b}, named by the operator")),
            None => {
                let (n, b) = public_trust_build(&args.public_repo).map_err(|e| {
                    format!(
                        "cannot read the published trust build ({e}); name it with --base-build N"
                    )
                })?;
                (b, format!("public index {n} on {}", args.public_repo))
            }
        };
        Ok::<_, String>((dir, root, pin, from))
    })();
    let (dir, root, pin, from) = match checked {
        Ok(c) => c,
        Err(why) => {
            let _ = writeln!(out, "trust candidate: COULD NOT CHECK — {why}");
            return 2;
        }
    };
    let receipts = args
        .receipts
        .clone()
        .unwrap_or_else(|| admission::receipts_dir(&state_dir()));
    let found = admission::newest_receipt(&receipts, &root);
    let now = i64::try_from(crate::publish::unix_now()).unwrap_or(i64::MAX);
    let refused = admission::refusals(
        found.as_ref().map(|(_, r)| r),
        &root,
        pin,
        now,
        &admission::Checkout(repo),
    );
    let short = root.get(..12).unwrap_or(&root);
    if refused.is_empty() {
        let path = found
            .map(|(p, _)| p.display().to_string())
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "ADMITS: YES — {} (tree_root {short}) passed the candidate gate against the published trust {pin} ({from}): {path}",
            dir.display()
        );
        return 0;
    }
    for r in &refused {
        let _ = writeln!(out, "trust candidate: refused — {r}");
    }
    let _ = writeln!(
        out,
        "ADMITS: NO — {} (tree_root {short}) is not admitted by the candidate gate against the published trust {pin} ({from}); nothing is published",
        dir.display()
    );
    1
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// One side of the gate: a toolchain, and what the receipt says of it.
struct Side {
    bin: PathBuf,
    dir: PathBuf,
    root: String,
    commit: Option<String>,
}

/// What the preflight read of one side's toolchain, each part as found.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SideSeen {
    /// The physical toolchain directory, or why the one named is none.
    dir: Result<PathBuf, String>,
    /// Why its bin/ runs no lane ([`trust_lane::Driver::at`]), if it does not.
    driver: Option<String>,
    root: Result<String, String>,
    commit: Option<String>,
    /// The tools a publish packs ([`TRUST_EXPOSES`]: the directory gated is the one
    /// packed, byte for byte) its bin/ lacks.
    missing: Vec<&'static str>,
}

impl SideSeen {
    fn read(dir: &Path) -> SideSeen {
        match toolchain_dir(dir) {
            Err(e) => SideSeen {
                dir: Err(e),
                driver: None,
                root: Err(String::new()),
                commit: None,
                missing: Vec::new(),
            },
            Ok(d) => {
                let bin = d.join("bin");
                SideSeen {
                    driver: trust_lane::Driver::at(&bin).err(),
                    root: tree_root(&d),
                    commit: trust_commit(&bin),
                    missing: TRUST_EXPOSES
                        .iter()
                        .copied()
                        .filter(|t| !bin.join(t).is_file())
                        .collect(),
                    dir: Ok(d),
                }
            }
        }
    }

    /// Its trustc's commit; `None` when the side itself could not be read (that is
    /// said already).
    fn read_commit(&self) -> Option<Option<&str>> {
        self.dir.is_ok().then_some(self.commit.as_deref())
    }

    fn side(&self) -> Option<Side> {
        let dir = self.dir.as_ref().ok()?;
        Some(Side {
            bin: dir.join("bin"),
            dir: dir.clone(),
            root: self.root.clone().unwrap_or_default(),
            commit: self.commit.clone(),
        })
    }

    /// What keeps it from being gated, `what` naming it.
    fn problems(&self, what: &str, out: &mut Vec<String>) {
        match &self.dir {
            Err(e) => out.push(format!("the {what} is not a directory: {e}")),
            Ok(_) => {
                if let Some(e) = &self.driver {
                    out.push(format!("the {what}: {e}"));
                }
                if let Err(e) = &self.root {
                    out.push(e.clone());
                }
            }
        }
    }
}

/// Where the base came from.
#[derive(Clone, Debug, PartialEq, Eq)]
enum BaseSeen {
    /// `--base`: named by hand, so checked against nothing published.
    Explicit(SideSeen),
    /// The build the public channel pins (`pin`: it and where it was read, or why it
    /// could not be), as this host's package client holds it.
    Published {
        pin: Result<(u64, String), String>,
        store: Option<StoreSeen>,
    },
}

/// The package client's hold on the published build.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StoreSeen {
    installed: Option<u64>,
    /// The tree_root the client verified when it installed it.
    signed: Option<String>,
    /// The store holds that build, ready.
    ready: bool,
    side: SideSeen,
}

/// Everything the preflight reads of the world, read once.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    cand: SideSeen,
    base: BaseSeen,
    /// The checkout is HEAD's bytes ([`crate::clean::strictly_clean`]).
    clean: bool,
    head: String,
    tree: String,
    ledger: PathBuf,
    ledger_sha: Option<String>,
    /// The ledger's sha256 as committed at HEAD.
    head_ledger_sha: Option<String>,
    rows: usize,
    /// Each row's retracting trust commit.
    row_commits: BTreeSet<String>,
    trust_repo: PathBuf,
    /// Why the trust repository cannot say which commit holds which (read only when
    /// the ledger has rows).
    trust_unfit: Option<String>,
    /// The trust commits the repository holds, of those the comparison will ask.
    trust_has: BTreeSet<String>,
    out_dir: PathBuf,
    /// --out already holds something.
    out_used: bool,
    out_refused: Option<trust_lane::Refused>,
    /// The nearest existing directory of --out, and its free bytes.
    volume: PathBuf,
    free: Option<u64>,
}

impl Seen {
    /// Read the world: `repo` the aterm checkout, `args` the invocation.
    fn read(repo: &Path, args: &Args) -> Seen {
        let cand = SideSeen::read(&args.dir);
        let base = match &args.base {
            Some(b) => BaseSeen::Explicit(SideSeen::read(b)),
            None => {
                let pin = match args.base_build {
                    Some(b) => Ok((
                        b,
                        format!("published (--base-build {b}, named by the operator)"),
                    )),
                    None => public_trust_build(&args.public_repo).map(|(n, b)| {
                        (
                            b,
                            format!("published (public index {n} on {})", args.public_repo),
                        )
                    }),
                };
                let store = match (&pin, atpkg::store::resolve_configured()) {
                    (Ok((b, _)), Some(layout)) => {
                        let status = fs::read_to_string(layout.prefix.join("status.toml"))
                            .unwrap_or_default();
                        let (installed, signed) = installed_trust(&status);
                        Some(StoreSeen {
                            installed,
                            signed,
                            ready: layout
                                .prefix
                                .join(format!("store/trust/{b}.ready"))
                                .exists(),
                            side: SideSeen::read(&layout.prefix.join(format!("store/trust/{b}"))),
                        })
                    }
                    _ => None,
                };
                BaseSeen::Published { pin, store }
            }
        };
        let git = crate::ledger::GitCli::new(repo);
        let ledger = args
            .retractions
            .clone()
            .unwrap_or_else(|| repo.join(crate::trust_oblig::LEDGER_PATH));
        let ledger_bytes = fs::read(&ledger).ok();
        let parsed = ledger_bytes.as_deref().map(|b| {
            crate::trust_oblig::parse_ledger(
                &String::from_utf8_lossy(b),
                crate::trust_oblig::LEDGER_PATH,
                &BTreeSet::new(),
            )
        });
        let rows = parsed.as_ref().map_or(0, |l| l.rows.len());
        let row_commits: BTreeSet<String> = parsed
            .iter()
            .flat_map(|l| l.rows.iter().map(|r| r.commit.clone()))
            .collect();
        let trust_repo = args
            .trust_repo
            .clone()
            .or_else(|| aterm_types::dirs::home_dir().map(|h| h.join("trust")))
            .unwrap_or_default();
        let (mut trust_unfit, mut trust_has) = (None, BTreeSet::new());
        if rows > 0 {
            let tg = crate::trust_oblig::TrustGit::new(&trust_repo);
            let tr = crate::trust_oblig::TrustRepo {
                git: &tg,
                shown: trust_repo.display().to_string(),
            };
            trust_unfit = tr.unfit();
            let base_commit = match &base {
                BaseSeen::Explicit(s) => s.commit.clone(),
                BaseSeen::Published { store, .. } => {
                    store.as_ref().and_then(|s| s.side.commit.clone())
                }
            };
            let asked = row_commits
                .iter()
                .cloned()
                .chain(cand.commit.clone())
                .chain(base_commit);
            trust_has = asked.filter(|c| tr.is_commit(c).unwrap_or(false)).collect();
        }
        let out_dir = args.out.clone().unwrap_or_else(|| {
            let stamp = aterm_types::rfc3339::format_rfc3339(crate::publish::unix_now())
                .replace([':', '-'], "");
            let c = cand.commit.clone().unwrap_or_default();
            admission::receipts_dir(&state_dir())
                .join(format!("{stamp}-{}", c.get(..10).unwrap_or(&c)))
        });
        // The volume the runs will fill: the nearest directory of --out that exists
        // (a dry run creates nothing).
        let volume = out_dir
            .ancestors()
            .find(|a| a.is_dir())
            .unwrap_or(Path::new("/"))
            .to_path_buf();
        Seen {
            cand,
            base,
            clean: crate::clean::strictly_clean(repo) == Some(true),
            head: crate::ledger::rev_parse(&git, "HEAD").unwrap_or_default(),
            tree: crate::ledger::rev_parse(&git, "HEAD^{tree}").unwrap_or_default(),
            ledger_sha: ledger_bytes.as_deref().map(sha256_hex),
            head_ledger_sha: admission::Aterm::ledger_sha(&admission::Checkout(repo), "HEAD"),
            ledger,
            rows,
            row_commits,
            trust_repo,
            trust_unfit,
            trust_has,
            out_used: fs::read_dir(&out_dir).is_ok_and(|mut d| d.next().is_some()),
            out_refused: trust_lane::refused_component(&out_dir),
            free: trust_lane::guard::free_bytes(&volume),
            volume,
            out_dir,
        }
    }

    fn base_side(&self) -> Option<&SideSeen> {
        match &self.base {
            BaseSeen::Explicit(s) => Some(s),
            BaseSeen::Published { store, .. } => store.as_ref().map(|s| &s.side),
        }
    }

    /// The tree_root this host's package client verified for the published base.
    fn signed(&self) -> Option<String> {
        match &self.base {
            BaseSeen::Published { store: Some(s), .. } => s.signed.clone(),
            _ => None,
        }
    }

    fn base_from(&self) -> String {
        match &self.base {
            BaseSeen::Explicit(_) => {
                "explicit (--base): not checked against the public channel".into()
            }
            BaseSeen::Published { pin, .. } => {
                pin.as_ref().map(|(_, f)| f.clone()).unwrap_or_default()
            }
        }
    }

    fn base_build(&self) -> Option<u64> {
        match &self.base {
            BaseSeen::Published {
                pin: Ok((b, _)), ..
            } => Some(*b),
            _ => None,
        }
    }
}

/// The preflight: every reason the gate must not start, each one the comparison or the
/// publisher would give hours later. Empty: run.
fn preflight(seen: &Seen, args: &Args) -> Vec<String> {
    let mut problems = Vec::new();
    seen.cand.problems("candidate", &mut problems);
    if seen.cand.dir.is_ok() && !seen.cand.missing.is_empty() {
        problems.push(format!(
            "the candidate {} is not the directory a publish packs: its bin/ lacks {} — \
             stage the trust-wp drivers into a copy of the seal and gate THAT copy",
            args.dir.display(),
            seen.cand.missing.join(" ")
        ));
    }
    // The base: the published seal as this host's package client verified it, or one
    // named by hand (which the publisher admits no PASS against).
    match &seen.base {
        BaseSeen::Explicit(s) => s.problems("base", &mut problems),
        BaseSeen::Published { pin: Err(e), .. } => problems.push(format!(
            "cannot read the published trust build ({e}): name it with --base-build N, or a base with --base"
        )),
        BaseSeen::Published { store: None, .. } => {
            problems.push("no package client store resolves here (aterm pkg doctor)".into());
        }
        BaseSeen::Published {
            pin: Ok((b, _)),
            store: Some(s),
        } => {
            if s.installed != Some(*b) {
                problems.push(format!(
                    "the published trust build is {b}, and this host's package client has {} installed: the base must be the published seal as the client verified it (aterm pkg update trust)",
                    s.installed.map_or_else(|| "none".into(), |i| i.to_string())
                ));
            }
            if !s.ready {
                problems.push(format!(
                    "the store holds no ready trust {b}: aterm pkg repair"
                ));
            }
            if let (Some(signed), Ok(root)) = (&s.signed, &s.side.root)
                && admission::is_tree_root(signed)
                && root != signed
            {
                problems.push(format!(
                    "the store's trust (tree_root {}) is not what the client verified ({}): aterm pkg repair",
                    root.get(..12).unwrap_or(root),
                    signed.get(..12).unwrap_or(signed)
                ));
            }
            if !s.signed.as_deref().is_some_and(admission::is_tree_root) {
                problems.push(format!(
                    "the package client's status.toml names no verified tree_root for trust {b}, so nothing binds the base to the published seal (aterm pkg update trust)"
                ));
            }
            s.side.problems("base", &mut problems);
        }
    }
    if let (Some(b), Ok(c)) = (seen.base_side(), &seen.cand.root)
        && b.root.as_ref().is_ok_and(|r| r == c)
    {
        problems.push(
            "the candidate's tree_root IS the base's: the same toolchain twice compares nothing"
                .into(),
        );
    }
    if !seen.clean {
        problems.push(
            "the checkout is not HEAD's bytes: its runs would file nothing, and the comparison refuses runs that measured no tree a release reads"
                .into(),
        );
    }
    match (&seen.ledger_sha, &seen.head_ledger_sha) {
        (None, _) => problems.push(format!(
            "the retraction ledger {} is not there",
            seen.ledger.display()
        )),
        (_, None) => problems.push(format!(
            "this checkout has no {} committed at HEAD to hold the ledger to: the publisher admits a PASS under that ledger alone",
            crate::trust_oblig::LEDGER_PATH
        )),
        (Some(have), Some(want)) if have != want => problems.push(format!(
            "the retraction ledger {} is not the one committed at HEAD: the publisher admits no PASS read under a ledger named by hand",
            seen.ledger.display()
        )),
        _ => {}
    }
    if seen.rows > 0 {
        let rows = seen.rows;
        let shown = seen.trust_repo.display();
        if let Some(why) = &seen.trust_unfit {
            problems.push(format!(
                "the retraction ledger has {rows} row(s), and {why}"
            ));
        }
        for (what, c) in [
            (
                "the published seal's",
                seen.base_side().and_then(SideSeen::read_commit),
            ),
            ("the candidate's", seen.cand.read_commit()),
        ] {
            match c {
                None => {}
                Some(None) => problems.push(format!(
                    "the retraction ledger has {rows} row(s), and {what} trustc names no commit"
                )),
                Some(Some(c)) if !seen.trust_has.contains(c) => problems.push(format!(
                    "the retraction ledger has {rows} row(s), and {what} trust commit {} is not a commit of {shown} (git fetch there)",
                    c.get(..12).unwrap_or(c)
                )),
                Some(Some(_)) => {}
            }
        }
        for c in seen.row_commits.difference(&seen.trust_has) {
            problems.push(format!(
                "the retraction ledger names trust {} (a row's commit column), which is not a commit of {shown} (git fetch there)",
                c.get(..12).unwrap_or(c)
            ));
        }
    }
    if seen.out_used {
        problems.push(format!(
            "--out {} already holds a gate's runs: its plans would read as this run's — give a fresh directory",
            seen.out_dir.display()
        ));
    }
    if let Some(r) = &seen.out_refused {
        problems.push(format!(
            "--out {} has a path component, {}, that `targo trust` refuses ({}): {}",
            seen.out_dir.display(),
            r.dir.display(),
            r.why,
            r.fix("give --out a directory under one you own")
        ));
    }
    match seen.free {
        None => problems.push(format!(
            "cannot read the free space under {}",
            seen.volume.display()
        )),
        Some(f) if f >> 30 < args.min_free_gib => problems.push(format!(
            "{} GiB free under {}, below --min-free-gib {}: two whole-tree runs fill target roots and logs for hours",
            f >> 30,
            seen.volume.display(),
            args.min_free_gib
        )),
        Some(_) => {}
    }
    problems
}

/// How one side's lane run ended.
enum Ran {
    /// The judge concluded it: its plan is the comparison's input.
    Concluded,
    /// This host refuses libraries for memory: no whole-tree verdict is possible here.
    Narrowed(Vec<String>),
    /// It did not conclude, and why.
    NotConcluded(String),
}

#[allow(clippy::too_many_lines)]
fn gate(repo: &Path, args: &Args, out: &mut dyn Write) -> i32 {
    let seen = Seen::read(repo, args);
    let problems = preflight(&seen, args);
    let base = seen.base_side().and_then(SideSeen::side);
    let cand = seen.cand.side();
    let (base_from, base_build, signed) = (seen.base_from(), seen.base_build(), seen.signed());
    let Seen {
        head,
        tree,
        ledger,
        ledger_sha,
        rows,
        trust_repo,
        out_dir,
        ..
    } = seen;
    let gate_root = admission::receipts_dir(&state_dir());
    let guard = args
        .guard
        .then(|| guard_of(args, crate::artifact::host_memory_bytes().unwrap_or(0)));
    let say = |out: &mut dyn Write, l: &str| {
        let _ = writeln!(out, "trust candidate: {l}");
    };
    if let (Some(b), Some(c)) = (&base, &cand) {
        say(
            out,
            &format!("base:      {} — {base_from}", b.bin.display()),
        );
        say(
            out,
            &format!(
                "candidate: {} (trustc {}, tree_root {})",
                c.bin.display(),
                c.commit.as_deref().unwrap_or("?"),
                c.root.get(..12).unwrap_or(&c.root)
            ),
        );
    }
    if let Some(g) = &guard {
        say(
            out,
            &format!(
                "guard:     a check ends at a {} GiB footprint, {} GiB of swap, memorystatus_level under {}, or under {} GiB free",
                g.tree_bytes >> 30,
                g.swap_bytes >> 30,
                g.level_min,
                g.free_bytes >> 30
            ),
        );
    }
    say(
        out,
        &format!(
            "ledger:    {} ({rows} retraction(s)); trust repository {}",
            ledger.display(),
            trust_repo.display()
        ),
    );
    say(
        out,
        &format!(
            "aterm:     {} at {}; out {}; budget {} s per crate",
            repo.display(),
            head.get(..12).unwrap_or(&head),
            out_dir.display(),
            args.budget.as_secs()
        ),
    );
    if !problems.is_empty() {
        for p in &problems {
            say(out, &format!("refused — {p}"));
        }
        let _ = writeln!(
            out,
            "VERDICT: PUBLISH-REFUSED — COULD NOT RUN: {} preflight refusal(s); nothing is published",
            problems.len()
        );
        return 2;
    }
    let (Some(base), Some(cand)) = (base, cand) else {
        return 2;
    };
    let side_cfg = |side: &str, s: &Side, wait: Duration| {
        let mut cfg = trust_lane::Config::new(repo, Scope::WholeTree);
        cfg.prover = Some(s.bin.clone());
        cfg.target_root = Some(out_dir.join(side).join("target"));
        cfg.state = Some(out_dir.join(side).join("state"));
        cfg.jobs = args.jobs;
        cfg.budget = args.budget;
        cfg.obligations = true;
        cfg.stop_if_narrowed = !args.measure_narrowed;
        cfg.trustd_wait = wait;
        cfg.guard = guard;
        cfg
    };
    let plan_of = |side: &str| out_dir.join(side).join("target/logs/.plan");
    let compare_args = crate::trust_compare::Args {
        base: plan_of("base"),
        candidate: plan_of("candidate"),
        tsv: Some(out_dir.join("compare.tsv")),
        base_prover: signed.clone(),
        ratchet: Some(repo.join(crate::trust_facts::RATCHET)),
        retractions: Some(ledger.clone()),
        trust_repo: Some(trust_repo.clone()),
        candidate_trust_commit: cand.commit.clone(),
        base_trust_commit: base.commit.clone(),
        obligations_tsv: Some(out_dir.join("obligations.tsv")),
        ..crate::trust_compare::Args::default()
    };
    if args.dry_run {
        say(out, "DRY RUN — would run, in order:");
        for (n, side, s) in [(1, "base", &base), (2, "candidate", &cand)] {
            let _ = writeln!(
                out,
                "  {n}. targo --unverified ship trust run --whole-tree --prover {} --target-root {} --jobs {} --crate-budget {} --obligations{}",
                s.bin.display(),
                out_dir.join(side).join("target").display(),
                args.jobs.map_or_else(|| "auto".into(), |j| j.to_string()),
                args.budget.as_secs(),
                if args.measure_narrowed {
                    ""
                } else {
                    " --stop-if-narrowed"
                }
            );
        }
        let _ = writeln!(
            out,
            "  3. targo --unverified ship trust compare --base {} --candidate {}",
            plan_of("base").display(),
            plan_of("candidate").display()
        );
        let _ = writeln!(out, "NO VERDICT: a dry run measures nothing (exit 3)");
        return 3;
    }
    let _lock = match gate_dirs(
        &gate_root,
        &state_dir().join("candidate-gate.lock"),
        &out_dir,
    ) {
        Ok(l) => l,
        Err(why) => {
            let _ = writeln!(
                out,
                "VERDICT: PUBLISH-REFUSED — COULD NOT RUN: {why}; nothing is published"
            );
            return 2;
        }
    };
    // One snapshot of the cost history for both runs: the same order.
    let costs = fs::read(state_dir_of_clone(repo).join("trust-costs.tsv")).ok();
    let mut receipt = Receipt {
        magic: RECEIPT_MAGIC.into(),
        fields: BTreeMap::new(),
    };
    let mut set = |k: &str, v: String| {
        receipt.fields.insert(k.into(), v);
    };
    set("mode", "run".into());
    set("candidate-bin", cand.bin.display().to_string());
    set("candidate-tree-root", cand.root.clone());
    set(
        "candidate-trustc",
        cand.commit.clone().unwrap_or_else(|| "-".into()),
    );
    set(
        "candidate-trust-commit",
        cand.commit.clone().unwrap_or_else(|| "-".into()),
    );
    set("base-bin", base.bin.display().to_string());
    set("base-from", base_from.clone());
    set(
        "base-build",
        base_build.map_or_else(|| "-".into(), |b| b.to_string()),
    );
    set("base-tree-root", base.root.clone());
    set(
        "base-signed-tree-root",
        signed.clone().unwrap_or_else(|| "-".into()),
    );
    set(
        "base-trust-commit",
        base.commit.clone().unwrap_or_else(|| "-".into()),
    );
    set("aterm-commit", head.clone());
    set("aterm-tree", tree.clone());
    set("budget", args.budget.as_secs().to_string());
    set(
        "jobs",
        args.jobs.map_or_else(|| "auto".into(), |j| j.to_string()),
    );
    set("only", String::new());
    set(
        "retractions",
        format!(
            "{} {}",
            ledger_sha.clone().unwrap_or_else(|| "-".into()),
            ledger.display()
        ),
    );
    set("trust-repo", trust_repo.display().to_string());
    set(
        "obligations",
        out_dir.join("obligations.tsv").display().to_string(),
    );
    let finish =
        |receipt: &mut Receipt, verdict: &str, exit: i32, compare: String, out: &mut dyn Write| {
            let plan = |side: &str| {
                fs::read_to_string(plan_of(side))
                    .ok()
                    .and_then(|t| crate::trust_facts::Plan::parse(&t).ok())
            };
            let (bp, cp) = (plan("base"), plan("candidate"));
            let field = |p: &Option<crate::trust_facts::Plan>,
                         f: &dyn Fn(&crate::trust_facts::Plan) -> String| {
                p.as_ref().map(f).unwrap_or_default()
            };
            let prover = |p: &crate::trust_facts::Plan| {
                p.file
                    .as_ref()
                    .map(|f| f.prover.clone())
                    .unwrap_or_default()
            };
            let host = |p: &crate::trust_facts::Plan| {
                p.file.as_ref().map(|f| f.host.clone()).unwrap_or_default()
            };
            receipt.fields.insert("verdict".into(), verdict.into());
            receipt.fields.insert("exit".into(), exit.to_string());
            receipt
                .fields
                .insert("candidate-prover".into(), field(&cp, &prover));
            receipt.fields.insert(
                "candidate-driver".into(),
                field(&cp, &|p| p.facts.driver_id.clone()),
            );
            receipt
                .fields
                .insert("base-prover".into(), field(&bp, &prover));
            receipt.fields.insert(
                "base-driver".into(),
                field(&bp, &|p| p.facts.driver_id.clone()),
            );
            receipt.fields.insert(
                "host".into(),
                format!("{} {}", hostname(), field(&cp, &host)),
            );
            receipt.fields.insert(
                "date".into(),
                aterm_types::rfc3339::format_rfc3339(crate::publish::unix_now()),
            );
            receipt.fields.insert("compare".into(), compare);
            let path = out_dir.join("RECEIPT");
            let tmp = out_dir.join("RECEIPT.tmp");
            let written = fs::write(&tmp, receipt.render()).and_then(|()| {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
                fs::rename(&tmp, &path)
            });
            match written {
                Ok(()) => say(out, &format!("receipt: {}", path.display())),
                Err(e) => say(
                    out,
                    &format!("the receipt was NOT written ({}: {e})", path.display()),
                ),
            }
            exit
        };
    let run_side = |side: &str, s: &Side, wait: Duration, out: &mut dyn Write| -> Ran {
        let cfg = side_cfg(side, s, wait);
        if let Err(e) = side_dirs(&out_dir, side) {
            return Ran::NotConcluded(format!("{}: {e}", out_dir.join(side).display()));
        }
        if let (Some(c), Some(state)) = (&costs, &cfg.state) {
            let _ = fs::write(state.join("trust-costs.tsv"), c);
        }
        let log = out_dir.join(side).join("lane.out");
        say(
            out,
            &format!(
                "{side}: the trust lane with {} — {}",
                s.bin.display(),
                log.display()
            ),
        );
        let mut file = match fs::File::create(&log) {
            Ok(f) => f,
            Err(e) => return Ran::NotConcluded(format!("{}: {e}", log.display())),
        };
        let outcome = trust_lane::run(&cfg, &mut file);
        let concluded = fs::read_to_string(plan_of(side))
            .ok()
            .and_then(|t| crate::trust_facts::Plan::parse(&t).ok())
            .is_some_and(|p| !p.facts.trustd.is_empty());
        say(
            out,
            &format!(
                "{side}: the lane answered {} — {}",
                outcome.exit(),
                outcome.detail().first().map_or("", String::as_str)
            ),
        );
        match outcome {
            Outcome::Narrowed(lines) if !concluded => Ran::Narrowed(lines),
            _ if concluded => Ran::Concluded,
            other => Ran::NotConcluded(other.detail().join("; ")),
        }
    };
    for (side, s, wait, what) in [
        (
            "base",
            &base,
            trust_lane::TRUSTD_WAIT,
            "the published seal's",
        ),
        ("candidate", &cand, CANDIDATE_TRUSTD_WAIT, "the candidate's"),
    ] {
        match run_side(side, s, wait, out) {
            Ran::Concluded => {}
            Ran::Narrowed(lines) => {
                for l in &lines {
                    say(out, l);
                }
                let _ = writeln!(
                    out,
                    "VERDICT: PUBLISH-REFUSED — COULD NOT RUN: this host cannot verify every library ({}); run the gate on a machine with the RAM named; nothing is published",
                    lines.len()
                );
                return finish(&mut receipt, "NARROWED", 2, "-".into(), out);
            }
            Ran::NotConcluded(why) => {
                let _ = writeln!(
                    out,
                    "VERDICT: PUBLISH-REFUSED — COULD NOT COMPARE: {what} run did not conclude ({why}); nothing is published"
                );
                return finish(&mut receipt, "COULD-NOT-COMPARE", 2, "-".into(), out);
            }
        }
    }
    let (report, mut exit) = crate::trust_compare::answer(&compare_args);
    let compare_txt = out_dir.join("compare.txt");
    let _ = fs::write(&compare_txt, &report);
    let _ = write!(out, "{report}");
    // Each run measured the directory whose tree_root the receipt names, and neither
    // changed while the gate ran.
    let mut drift = Vec::new();
    for (what, s) in [("candidate", &cand), ("base", &base)] {
        if tree_root(&s.dir).ok().as_deref() != Some(s.root.as_str()) {
            drift.push(format!(
                "the {what} {} changed during the gate",
                s.dir.display()
            ));
        }
        let ran = fs::read_to_string(plan_of(what))
            .ok()
            .and_then(|t| crate::trust_facts::Plan::parse(&t).ok())
            .and_then(|p| p.file.map(|f| f.prover));
        if ran.as_deref() != Some(s.root.as_str()) {
            drift.push(format!(
                "the {what}'s run identified prover {}, not its tree_root {}",
                ran.unwrap_or_default(),
                s.root.get(..12).unwrap_or(&s.root)
            ));
        }
    }
    if !drift.is_empty() {
        for d in &drift {
            let _ = writeln!(out, "COULD NOT COMPARE: {d}");
        }
        let _ = writeln!(
            out,
            "VERDICT: PUBLISH-REFUSED — COULD NOT COMPARE: the runs did not measure the toolchains the receipt names; nothing is published"
        );
        exit = 2;
    }
    let verdict = match exit {
        0 => "PASS",
        1 => "REFUSED",
        _ => "COULD-NOT-COMPARE",
    };
    finish(
        &mut receipt,
        verdict,
        exit,
        compare_txt.display().to_string(),
        out,
    )
}

/// Make the receipts root `gate_root` as `targo trust` admits it under any umask
/// ([`trust_lane::owner_dirs`]: the default `--out` lies under it, so a `0775` one would
/// refuse every side's target root), take the host's one-measurement `lock`, then make the
/// runs' directory ([`private_out`]).
///
/// # Errors
/// Another gate holds the lock, or the runs' directory cannot be made.
pub(crate) fn gate_dirs(
    gate_root: &Path,
    lock: &Path,
    out_dir: &Path,
) -> Result<trust_lane::RunLock, String> {
    // Best effort: `private_out` makes what --out needs either way.
    let _ = trust_lane::owner_dirs(gate_root);
    let held = trust_lane::RunLock::take(lock, "one measurement at a time on this host")?;
    private_out(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    Ok(held)
}

/// Make the runs' directory, the operator's alone (`0700`: its plans and receipt decide a
/// publish), and each missing directory above it as `targo trust` admits them under any
/// umask ([`trust_lane::owner_dirs`]): each side's target root lies under it.
fn private_out(out_dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    trust_lane::owner_dirs(out_dir)?;
    fs::set_permissions(out_dir, fs::Permissions::from_mode(0o700))
}

/// Make one side's directory under the runs' and its lane `state/`, as `targo trust`
/// admits them under any umask: the side's target root is `<out>/<side>/target`, and a
/// login's umask `002` would make a plain `create_dir_all`'s `0775`.
pub(crate) fn side_dirs(out_dir: &Path, side: &str) -> std::io::Result<()> {
    trust_lane::owner_dirs(&out_dir.join(side).join("state"))
}

/// The clone's own lane state (its cost history), where a plain `trust run` keeps it.
fn state_dir_of_clone(repo: &Path) -> PathBuf {
    crate::ledger::git_ok(
        &crate::ledger::GitCli::new(repo),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map(|o| PathBuf::from(o.stdout_utf8().trim()).join("aterm-release"))
    .unwrap_or_default()
}

/// The memory guard's bounds on a host with `mem` bytes of RAM (0: unread, so no
/// footprint bound; the others hold).
fn guard_of(args: &Args, mem: u64) -> Guard {
    let tree_bytes = match args.guard_footprint_gib {
        Some(g) => g << 30,
        None if mem == 0 => u64::MAX,
        None => mem / 10 * 6,
    };
    Guard {
        tree_bytes,
        swap_bytes: args.guard_swap_gib << 30,
        level_min: args.guard_level,
        free_bytes: args
            .guard_min_free_gib
            .unwrap_or_else(|| args.min_free_gib.saturating_sub(2))
            << 30,
    }
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "?".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "a519d423f47ff6fb19d505012f210f13ad39f2f6f31cb3c3af5aff508551d0b2";
    const SEAL: &str = "5ea15ea15ea15ea15ea15ea15ea15ea15ea15ea15ea15ea15ea15ea15ea15ea1";
    const AT: &str = "36b2c3df36eac4971b8189da9967ab7cc442bbf2";
    const C_COMMIT: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const B_COMMIT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const R_COMMIT: &str = "dddddddddddddddddddddddddddddddddddddddd";

    fn side_seen(dir: &str, root: &str, commit: &str) -> SideSeen {
        SideSeen {
            dir: Ok(PathBuf::from(dir)),
            driver: None,
            root: Ok(root.into()),
            commit: Some(commit.into()),
            missing: Vec::new(),
        }
    }

    /// A world the gate runs in: a full candidate, the published build installed and
    /// verified, a clean checkout under its committed ledger of one row whose commits
    /// the trust repository holds, a fresh --out on a roomy volume.
    fn seen_ok() -> Seen {
        Seen {
            cand: side_seen("/stage", ROOT, C_COMMIT),
            base: BaseSeen::Published {
                pin: Ok((
                    9192,
                    "published (public index 68 on alabsystems/aterm)".into(),
                )),
                store: Some(StoreSeen {
                    installed: Some(9192),
                    signed: Some(SEAL.into()),
                    ready: true,
                    side: side_seen("/store/trust/9192", SEAL, B_COMMIT),
                }),
            },
            clean: true,
            head: AT.into(),
            tree: AT.into(),
            ledger: PathBuf::from("/r/crates/aterm-release/trust/retractions.tsv"),
            ledger_sha: Some("abc".into()),
            head_ledger_sha: Some("abc".into()),
            rows: 1,
            row_commits: BTreeSet::from([R_COMMIT.to_string()]),
            trust_repo: PathBuf::from("/trust"),
            trust_unfit: None,
            trust_has: BTreeSet::from([
                R_COMMIT.to_string(),
                C_COMMIT.to_string(),
                B_COMMIT.to_string(),
            ]),
            out_dir: PathBuf::from("/out/run"),
            out_used: false,
            out_refused: None,
            volume: PathBuf::from("/out"),
            free: Some(500 << 30),
        }
    }

    fn store(seen: &mut Seen) -> &mut StoreSeen {
        match &mut seen.base {
            BaseSeen::Published { store: Some(s), .. } => s,
            _ => unreachable!("seen_ok has a store"),
        }
    }

    /// The preflight refuses, before hours of runs, each world the comparison or the
    /// publisher would refuse after them, one reason each, and passes the good one.
    #[test]
    fn the_preflight_refuses_each_world_it_must() {
        let args = parse(&mut ["--candidate", "/stage"].into_iter()).unwrap();
        assert_eq!(preflight(&seen_ok(), &args), Vec::<String>::new());
        type Edit = Box<dyn Fn(&mut Seen)>;
        let cases: Vec<(&str, Edit)> = vec![
            ("IS the base's", Box::new(|s| s.cand.root = Ok(SEAL.into()))),
            (
                "not what the client verified",
                Box::new(|s| store(s).side.root = Ok(ROOT.replace('a', "e"))),
            ),
            (
                "names no verified tree_root",
                Box::new(|s| store(s).signed = None),
            ),
            (
                "names no verified tree_root",
                Box::new(|s| store(s).signed = Some("-".into())),
            ),
            (
                "has none installed",
                Box::new(|s| store(s).installed = None),
            ),
            ("no ready trust 9192", Box::new(|s| store(s).ready = false)),
            (
                "cannot read the published trust build",
                Box::new(|s| {
                    s.base = BaseSeen::Published {
                        pin: Err("gh: offline".into()),
                        store: None,
                    };
                }),
            ),
            (
                "no package client store",
                Box::new(|s| {
                    s.base = BaseSeen::Published {
                        pin: Ok((9192, "published".into())),
                        store: None,
                    };
                }),
            ),
            (
                "its bin/ lacks trust-wp targo-trust-wp",
                Box::new(|s| s.cand.missing = vec!["trust-wp", "targo-trust-wp"]),
            ),
            (
                "the candidate: no targo",
                Box::new(|s| s.cand.driver = Some("no targo".into())),
            ),
            (
                "the candidate is not a directory",
                Box::new(|s| s.cand.dir = Err("/stage: not found".into())),
            ),
            ("not HEAD's bytes", Box::new(|s| s.clean = false)),
            (
                "not the one committed at HEAD",
                Box::new(|s| s.ledger_sha = Some("def".into())),
            ),
            ("is not there", Box::new(|s| s.ledger_sha = None)),
            (
                "names trust dddddddddddd (a row's commit column)",
                Box::new(|s| {
                    s.trust_has.remove(R_COMMIT);
                }),
            ),
            (
                "the candidate's trust commit cccccccccccc",
                Box::new(|s| {
                    s.trust_has.remove(C_COMMIT);
                }),
            ),
            (
                "is a shallow repository",
                Box::new(|s| s.trust_unfit = Some("/trust is a shallow repository".into())),
            ),
            (
                "already holds a gate's runs",
                Box::new(|s| s.out_used = true),
            ),
            (
                "that `targo trust` refuses",
                Box::new(|s| {
                    s.out_refused = Some(trust_lane::Refused {
                        dir: "/tmp".into(),
                        why: "mode 1777: group- or world-writable".into(),
                        chmod: None,
                    });
                }),
            ),
            (
                "below --min-free-gib 30",
                Box::new(|s| s.free = Some(10 << 30)),
            ),
            ("cannot read the free space", Box::new(|s| s.free = None)),
        ];
        for (said, edit) in cases {
            let mut seen = seen_ok();
            edit(&mut seen);
            let got = preflight(&seen, &args);
            assert_eq!(got.len(), 1, "{said}: {got:?}");
        }
        // A refused component of --out names its fix: `chmod go-w` on one of the
        // caller's that only its mode keeps out, another directory for another owner's.
        for (chmod, fix) in [
            (Some("go-w"), "run: chmod go-w /home/u"),
            (None, "give --out a directory under one you own"),
        ] {
            let mut seen = seen_ok();
            seen.out_refused = Some(trust_lane::Refused {
                dir: "/home/u".into(),
                why: "mode 775: group- or world-writable".into(),
                chmod,
            });
            let got = preflight(&seen, &args);
            assert!(
                got.len() == 1
                    && got[0].ends_with(&format!("(mode 775: group- or world-writable): {fix}")),
                "{got:?}"
            );
        }
        // A base named by hand is held to nothing published, and no rows ask nothing
        // of the trust repository.
        let mut by_hand = seen_ok();
        by_hand.base = BaseSeen::Explicit(side_seen("/elsewhere", SEAL, B_COMMIT));
        by_hand.rows = 0;
        by_hand.trust_has.clear();
        assert_eq!(preflight(&by_hand, &args), Vec::<String>::new());
    }

    #[test]
    fn the_published_pin_and_the_installed_build_are_read_as_toml() {
        let index = "schema = 2\n[[channels]]\nname = \"stable\"\npin = { trust = 9192, ty = 5 }\n";
        assert_eq!(index_trust_pin(index), Some(9192));
        assert_eq!(index_trust_pin("schema = 2\n"), None);
        let status = format!(
            "[programs.trust]\ninstalled_build = 9192\nstate = \"managed\"\ntree_root = \"{ROOT}\"\n\n[programs.ty]\ninstalled_build = 1\n"
        );
        assert_eq!(
            installed_trust(&status),
            (Some(9192), Some(ROOT.to_string()))
        );
        assert_eq!(installed_trust(""), (None, None));
    }
}
