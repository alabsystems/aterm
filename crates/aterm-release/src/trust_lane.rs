// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The trust lane: `targo --unverified ship trust run`.
//!
//! Runs `targo trust check -p <library> --lib --allow-l0-gaps` over the workspace
//! libraries and the vendored forks, each check in its own slot target directory, and
//! hands what each left to the judge ([`crate::trust_facts`]); a whole-tree run on a
//! clean checkout can file facts for the explicit Trust verdict commands.
//!
//! - Every check's environment is built from a list ([`lane_env`]): a caller's
//!   `TRUSTFLAGS` or `RUSTFLAGS` would move a proof the facts key does not cover.
//! - The prover is resolved once ([`Driver`]): a store update mid-run would split the run
//!   across two provers.
//! - A check runs in its own session and is ended with its whole process tree, trustd's
//!   left alone ([`proc_tree::end_tree`]).
//! - Each check has a wall budget ([`Config::budget`]): a verifier stuck before its
//!   solvers spins for hours, and a check that does not finish is not verified.
//! - A second check starts only while this build's trustd serves the socket
//!   ([`trustd::Keeper`]): two checks that each start a trustd race and the loser is
//!   refused.
//! - Nothing is retried: a check targo would not start is an undecided
//!   NOT-VERIFIED(trustd) row that `--remeasure` measures again.

use aterm_uds::spawnfd::{Stdio, command};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::ledger::GitCli;
use crate::trust_facts::{self, Decision, Plan, Update, one_line};

/// Forks the resolved graph carries that the lane does not verify yet, each with why:
/// a QUEUED row, never gated and never a pass. Any other fork is gated like a member,
/// so a new fork enters as NO-FLOOR until its floor is written; one listed here that
/// leaves the graph is FORK-VANISHED ([`vanished`]). Empty since indexmap, the last
/// vendored fork, went back to the registry (docs/AUDIT-third-party-2026-10-15.md, "The
/// last fork").
pub const QUEUED_FORKS: &[(&str, &str)] = &[];

/// The lane's cargo profile: dev at opt-level 0 with every dev speed row zeroed
/// ([`lane_profile_args`]); optimized, the MIR inliner pastes std internals into the
/// verified library and proofs fall with no source changed.
const LANE_PROFILE: &str = "trust-lane";

/// A check still running this long after it started is ended and its library is
/// OVER-BUDGET: far above the slowest honest check, so it tells stuck from slow.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(6 * 3600);

/// How long the lane waits for a trustd of another build, or one that does not answer,
/// to leave the socket before any check: a trustd retires 300 s after its last grant.
pub const TRUSTD_WAIT: Duration = Duration::from_secs(330);

/// A check whose largest process reached this is HEAVY: at most one per 64 GiB of RAM
/// runs at once, so slots never stack several near-30 GiB checks on a shared machine.
const HEAVY_BYTES: u64 = 16 << 30;

/// How often the slots are polled.
const POLL: Duration = Duration::from_millis(250);

/// Variables every compile of the lane gets as constants of this file (so of the tree
/// the facts key covers).
const LANE_PINS: [(&str, &str); 2] = [("TRUST_NO_MIGRATE_WARN", "1"), ("CARGO_INCREMENTAL", "0")];

/// Variables the lane passes from the host, by name: their values are no input the key
/// covers, so the judge refuses a compile that read one (`dep_info_check`).
const LANE_PASSED: [&str; 9] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "LANG",
    "CARGO_BUILD_JOBS",
    "RUSTDOC",
    "CARGO_TARGET_DIR",
];

// What a run is asked, and what it answers

/// Which libraries a run verifies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every library and fork: a run eligible to file whole-tree facts.
    WholeTree,
    /// These libraries only (`-p`): files nothing.
    Packages(Vec<String>),
    /// Only what the stored facts of this tree and prover left undecided, folded into
    /// that record.
    Remeasure,
}

/// A run of the lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub repo: PathBuf,
    pub scope: Scope,
    /// Slots at once; `None` is one per six cores and per 16 GiB, at least one.
    pub jobs: Option<usize>,
    /// Where the slots' target directories and the logs go, never moved when named. `None`
    /// is the first of `<repo>/target-trust.noindex` and this checkout's directory of the user's
    /// cache ([`choose_root`]) whose every component `targo trust` admits
    /// ([`refused_component`]): a checkout made under a login's umask `002` is `0775`, so
    /// there the run says it falls back, and when neither is admitted it runs nothing. A
    /// default run retires the checkout's cache directory once `<repo>/target-trust.noindex` is
    /// admitted again, and prunes those whose checkout is gone ([`prune_orphans`]).
    pub target_root: Option<PathBuf>,
    pub budget: Duration,
    /// Write floors this run meets (never lowering one) and keep the memory record
    /// current.
    pub update_ratchet: bool,
    /// Verify what this host would refuse for memory up front.
    pub try_refused: bool,
    /// Reduce each check's proof report to its per-obligation record, kept beside its
    /// log ([`crate::trust_oblig`]). The report needs `--report-dir`, whose proof artifact
    /// store a prover without trust a40ddc8e9c (the published seal 321aaeda75) caps at
    /// 512 MiB by ending the check with exit 2 before its Summary, as aterm-agent's: such
    /// a library's record needs a prover ([`Config::prover`]) that holds that commit.
    pub obligations: bool,
    /// Stop before any check when this host refuses a library for memory.
    pub stop_if_narrowed: bool,
    /// The prover's `bin/`; `None` is the toolchain the cut builds with.
    pub prover: Option<PathBuf>,
    /// Where the cost history and this host's memory observations live; `None` is
    /// `<git common dir>/aterm-release`.
    pub state: Option<PathBuf>,
    pub trustd_wait: Duration,
}

impl Config {
    /// A run of `scope` in `repo` with every default.
    #[must_use]
    pub fn new(repo: &Path, scope: Scope) -> Config {
        Config {
            repo: repo.to_path_buf(),
            scope,
            jobs: None,
            target_root: None,
            budget: DEFAULT_BUDGET,
            update_ratchet: false,
            try_refused: false,
            obligations: false,
            stop_if_narrowed: false,
            prover: None,
            state: None,
            trustd_wait: TRUSTD_WAIT,
        }
    }
}

/// What a run decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every library it verified held its floor.
    Pass,
    /// A library fell below its floor, had none, or did not finish: each
    /// `<crate> <STATUS> <proved>/<total>`.
    Fail(Vec<String>),
    /// Nothing was decided about these (trustd or the clock kept them from a result):
    /// `--remeasure` measures them again.
    Undecided(Vec<String>),
    /// This host cannot verify every library: what it refused, and why.
    Narrowed(Vec<String>),
    /// Nothing was decided: why.
    CouldNotRun(String),
}

impl Outcome {
    /// The exit status: 0 pass, 1 fail, 2 nothing decided, 4 narrowed by memory.
    #[must_use]
    pub fn exit(&self) -> i32 {
        match self {
            Outcome::Pass => 0,
            Outcome::Fail(_) => 1,
            Outcome::Undecided(_) | Outcome::CouldNotRun(_) => 2,
            Outcome::Narrowed(_) => 4,
        }
    }

    fn of(v: &trust_facts::Verdict) -> Outcome {
        match v.exit {
            0 => Outcome::Pass,
            1 => Outcome::Fail(v.failed.clone()),
            4 => Outcome::Narrowed(
                v.memory
                    .iter()
                    .map(|(name, need, ram)| match ram {
                        Some(r) => format!("{name}: needs {need} GiB; {r} GiB of RAM verifies it"),
                        None => format!("{name}: needs {need} GiB"),
                    })
                    .collect(),
            ),
            _ if !v.unverified.is_empty() => Outcome::Undecided(v.unverified.clone()),
            _ => Outcome::CouldNotRun(v.tail.clone()),
        }
    }
}

/// Parse `trust run …`.
///
/// # Errors
/// A flag this does not know, one without its value, or no scope named.
pub fn parse<'a>(it: &mut impl Iterator<Item = &'a str>) -> Result<Config, String> {
    let mut cfg = Config::new(Path::new(""), Scope::WholeTree);
    let (mut packages, mut whole, mut remeasure) = (Vec::new(), false, false);
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .filter(|v| !v.is_empty() && !v.starts_with('-'))
                .ok_or_else(|| format!("trust run: {flag} needs a value"))
        };
        match flag {
            "-p" | "--package" => packages.push(value()?.to_string()),
            "--whole-tree" => whole = true,
            "--remeasure" => remeasure = true,
            "--jobs" => {
                let v = value()?;
                cfg.jobs = match v {
                    "auto" => None,
                    n => Some(n.parse().ok().filter(|n| *n > 0).ok_or(format!(
                        "trust run: --jobs takes a count or auto, got {n:?}"
                    ))?),
                };
            }
            "--target-root" => cfg.target_root = Some(PathBuf::from(value()?)),
            "--crate-budget" => {
                let v = value()?;
                let secs: u64 = v.parse().ok().filter(|s| *s > 0).ok_or(format!(
                    "trust run: --crate-budget takes whole seconds, got {v:?}"
                ))?;
                cfg.budget = Duration::from_secs(secs);
            }
            "--prover" => cfg.prover = Some(PathBuf::from(value()?)),
            "--update-ratchet" => cfg.update_ratchet = true,
            "--try-refused" => cfg.try_refused = true,
            "--obligations" => cfg.obligations = true,
            "--stop-if-narrowed" => cfg.stop_if_narrowed = true,
            other => return Err(format!("trust run: unknown flag {other:?}")),
        }
    }
    cfg.scope = match (whole, remeasure, packages.is_empty()) {
        (true, false, true) => Scope::WholeTree,
        (false, true, true) => Scope::Remeasure,
        (false, false, false) => Scope::Packages(packages),
        (false, false, true) => {
            return Err(
                "trust run: name what to verify: -p <crate>…, --whole-tree, or --remeasure".into(),
            );
        }
        _ => {
            return Err(
                "trust run: -p, --whole-tree and --remeasure are three scopes; give one".into(),
            );
        }
    };
    Ok(cfg)
}

// The prover

/// The toolchain every compile of a run uses, resolved once.
#[derive(Clone, Debug)]
pub struct Driver {
    /// Its `bin/`, physical.
    pub bin: PathBuf,
    pub targo: PathBuf,
    /// `targo --version`, empty when it said nothing.
    pub id: String,
}

impl Driver {
    /// The toolchain at `bin`: its `targo` a regular executable file (`targo trust`
    /// refuses a symlinked driver, and a shim's body moves with the next update), with
    /// `trustc` and `trustd` beside it, and nothing of it `targo trust` would refuse to run
    /// or load from ([`toolchain_refusal`]): a refusal there fails every check before it
    /// verifies anything, its cause buried in each log.
    ///
    /// # Errors
    /// What is missing, and the fix.
    pub fn at(bin: &Path) -> Result<Driver, String> {
        use std::os::unix::fs::PermissionsExt as _;
        let bin =
            fs::canonicalize(bin).map_err(|e| format!("the prover {}: {e}", bin.display()))?;
        let regular_exe = |name: &str| {
            fs::symlink_metadata(bin.join(name))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        };
        for tool in ["targo", "trustc", "trustd"] {
            if !regular_exe(tool) {
                return Err(format!(
                    "the prover {} holds no {tool} (a regular executable file): no check runs \
                     without its own build's targo, trustc and trustd (fix: aterm pkg doctor, \
                     then aterm pkg repair, or --prover <a sealed toolchain's bin>)",
                    bin.display()
                ));
            }
        }
        let targo = bin.join("targo");
        if fs::read(&targo).is_ok_and(|b| b.starts_with(b"#!")) {
            return Err(format!(
                "{} is a shim, not a toolchain: name the toolchain's own bin/ (aterm pkg which targo)",
                targo.display()
            ));
        }
        if let Some(r) = toolchain_refusal(&bin) {
            return Err(format!(
                "the prover {}: `targo trust` refuses {} ({}), so every check would fail before \
                 verifying anything: {}",
                bin.display(),
                r.dir.display(),
                r.why,
                r.fix(
                    "install the toolchain under a directory you own, or name another with --prover"
                )
            ));
        }
        let id = command(&targo)
            .arg("--version")
            .env_clear()
            .envs(lane_env(&bin, &host_var))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .map(str::to_string)
            })
            .unwrap_or_default();
        Ok(Driver { bin, targo, id })
    }
}

fn host_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The lane's environment: the pins, then what it passes from the host (`host`), the
/// driver's directory first on PATH so a nested `trustc`, `trustd` or `trustdoc` is the
/// pinned build's, and the system's after it so a build script's `cc` is the system's. The
/// caller adds each compile's `CARGO_TARGET_DIR`.
#[must_use]
pub fn lane_env(bin: &Path, host: &dyn Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = LANE_PINS
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
    ));
    for k in ["HOME", "USER", "LOGNAME", "TMPDIR", "LANG"] {
        if let Some(v) = host(k) {
            env.push((k.into(), v));
        }
    }
    if let Some(j) = host("CARGO_BUILD_JOBS").filter(|j| j.parse::<u32>().is_ok_and(|n| n > 0)) {
        env.push(("CARGO_BUILD_JOBS".into(), j));
    }
    // `targo trust` refuses a doc driver that is a symlink; the pinned build's sibling
    // is a plain file of the same build.
    let doc = bin.join("trustdoc");
    if fs::symlink_metadata(&doc).is_ok_and(|m| m.is_file()) {
        env.push(("RUSTDOC".into(), doc.display().to_string()));
    }
    env
}

// The gate list, from the resolved graph

/// What a package of the graph is to the lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A workspace member with a library: `-p name@version --lib`, and once more per fork
    /// feature ([`fork_features`]) as `<name>+<feature>`.
    Member,
    /// A workspace member with no library: nothing the library lane verifies.
    MemberBin,
    /// A path package outside the workspace (a `[patch]` fork under vendor/): checked
    /// by its own manifest with the feature set the workspace resolve turns on.
    Fork,
}

/// One row of the gate list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lib {
    /// The package's name, or `<name>+<feature>` for a member's fork-feature row
    /// ([`FEATURE_ROW`]). A twin (one of two packages of one name in the graph:
    /// `windows-sys@0.52.0` and `windows-sys@0.61.2`) is named by its [`Self::spec`]
    /// instead, so each has its own floor row.
    pub name: String,
    pub kind: Kind,
    /// `name@version` (`name+feature@version` for a fork-feature row): never the
    /// bare name, which two resolved versions make ambiguous.
    pub spec: String,
    /// Everything `targo trust check` takes before `--allow-l0-gaps`; empty for a
    /// [`Kind::MemberBin`].
    pub argv: Vec<String>,
}

/// What joins a member to its feature in a fork-feature row's name and spec
/// (`<member>+<feature>`). A package name holds only alphanumerics, `-` and `_`, so
/// a name with it is always such a row, and the part before it is the package.
pub const FEATURE_ROW: char = '+';

/// The Cargo package a lane row names, its feature or twin version suffix ([`Lib::name`])
/// dropped: Cargo's reports name the package.
#[must_use]
pub fn package_identity(row: &str) -> &str {
    row.split_once([FEATURE_ROW, '@'])
        .map_or(row, |(package, _)| package)
}

/// The features of `pkg`'s own that its default build leaves off and that name a path fork
/// directly (`fork/feat`, `fork?/feat` or `dep:fork`, by the manifest's key for the
/// dependency). The fork's lane verifies the fork code such a feature turns on (the graph is
/// every feature's: [`gate_list`]), but the member's arms behind it, that code's call sites,
/// compile only with it, so each is gated as a row of its own beside the default row, whose
/// `cfg(not(..))` arms are the ones that ship.
fn fork_features(pkg: &aterm_json::Value, forks: &BTreeSet<&str>) -> Vec<String> {
    use aterm_json::Value;
    let Some(features) = pkg.get("features").and_then(Value::as_object) else {
        return Vec::new();
    };
    let values = |f: &str| -> Vec<&str> {
        features
            .get(f)
            .and_then(Value::as_array)
            .map(|v| v.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    };
    let deps: BTreeMap<&str, &str> = pkg
        .get("dependencies")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let name = d.get("name")?.as_str()?;
            let key = d.get("rename").and_then(Value::as_str).unwrap_or(name);
            Some((key, name))
        })
        .collect();
    // What the default build turns on: `default` and the features it names, transitively.
    let mut on: BTreeSet<&str> = BTreeSet::new();
    let mut next = vec!["default"];
    while let Some(f) = next.pop() {
        if on.insert(f) {
            next.extend(
                values(f)
                    .into_iter()
                    .filter(|v| !v.contains('/') && !v.starts_with("dep:")),
            );
        }
    }
    let names_a_fork = |v: &str| {
        let dep = v
            .strip_prefix("dep:")
            .unwrap_or_else(|| v.split('/').next().unwrap_or(v))
            .trim_end_matches('?');
        (v.starts_with("dep:") || v.contains('/'))
            && deps.get(dep).is_some_and(|name| forks.contains(name))
    };
    features
        .keys()
        .filter(|f| !on.contains(f.as_str()))
        .filter(|f| values(f).into_iter().any(names_a_fork))
        .cloned()
        .collect()
}

/// The `--manifest-path` a check's argv names: a fork's, checked by its own manifest.
fn manifest_of(argv: &[String]) -> Option<&str> {
    let i = argv.iter().position(|a| a == "--manifest-path")?;
    argv.get(i + 1).map(String::as_str)
}

/// `--profile trust-lane` and its definition: dev at opt-level 0, each named dev
/// package row (`rows`) zeroed by name, since a named row outranks `package."*"`.
#[must_use]
pub fn lane_profile_args(rows: &BTreeSet<String>) -> Vec<String> {
    let p = format!("profile.{LANE_PROFILE}");
    let mut argv = vec![
        "--profile".to_string(),
        LANE_PROFILE.to_string(),
        "--config".to_string(),
        format!("{p}.inherits=\"dev\""),
        "--config".to_string(),
        format!("{p}.opt-level=0"),
    ];
    for spec in rows {
        argv.push("--config".into());
        argv.push(format!("{p}.package.\"{spec}\".opt-level=0"));
    }
    argv
}

/// The `[profile.dev.package.<spec>]` keys of a manifest or cargo config, none when it
/// is not there.
///
/// # Errors
/// A file that is there and does not parse: unread, it would verify optimized.
fn dev_package_rows(path: &Path) -> Result<BTreeSet<String>, String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let doc: aterm_toml::Value = text
        .parse()
        .map_err(|e| format!("{} does not parse: {e}", path.display()))?;
    Ok(doc
        .get("profile")
        .and_then(|p| p.get("dev"))
        .and_then(|d| d.get("package"))
        .and_then(aterm_toml::Value::as_table)
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default())
}

/// The dev package rows a check of `manifest` sees: the manifest's own and the
/// `.cargo/config.toml` beside it or at the workspace root.
fn profile_rows_for(manifest: &Path, root: &Path) -> Result<BTreeSet<String>, String> {
    let mut rows = dev_package_rows(manifest)?;
    let mut dirs = BTreeSet::new();
    dirs.insert(root.to_path_buf());
    if let Some(d) = manifest.parent() {
        dirs.insert(d.to_path_buf());
    }
    for d in dirs {
        rows.extend(dev_package_rows(&d.join(".cargo/config.toml"))?);
    }
    Ok(rows)
}

/// The gate list, from `targo metadata --format-version 1 --all-features` with dependencies
/// (a fork is found only in the resolve): members first, then forks, by name; a fork with
/// the features that resolve turns on for it, a member once with its default features and
/// once more per fork feature ([`fork_features`]). `rows` answers the dev package rows a
/// manifest's check sees.
///
/// # Errors
/// Metadata the lane cannot read a gate list from, or a name or spec that cannot go in an
/// argv.
pub fn gate_list(
    meta: &aterm_json::Value,
    rows: &dyn Fn(&Path) -> Result<BTreeSet<String>, String>,
) -> Result<Vec<Lib>, String> {
    const LIB_KINDS: [&str; 6] = ["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"];
    let packages = meta
        .get("packages")
        .and_then(aterm_json::Value::as_array)
        .filter(|p| !p.is_empty())
        .ok_or("targo metadata listed no packages")?;
    let members: BTreeSet<&str> = meta
        .get("workspace_members")
        .and_then(aterm_json::Value::as_array)
        .map(|m| m.iter().filter_map(aterm_json::Value::as_str).collect())
        .unwrap_or_default();
    if members.is_empty() {
        return Err("targo metadata listed no workspace members".into());
    }
    let root = PathBuf::from(
        meta.get("workspace_root")
            .and_then(aterm_json::Value::as_str)
            .ok_or("targo metadata names no workspace_root")?,
    );
    let features: BTreeMap<&str, Vec<&str>> = meta
        .get("resolve")
        .and_then(|r| r.get("nodes"))
        .and_then(aterm_json::Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| {
                    let id = n.get("id")?.as_str()?;
                    let f = n
                        .get("features")
                        .and_then(aterm_json::Value::as_array)
                        .map(|f| f.iter().filter_map(aterm_json::Value::as_str).collect())
                        .unwrap_or_default();
                    Some((id, f))
                })
                .collect()
        })
        .unwrap_or_default();
    let member_lane = lane_profile_args(&rows(&root.join("Cargo.toml"))?);
    let is_fork = |pkg: &aterm_json::Value| {
        pkg.get("id")
            .and_then(aterm_json::Value::as_str)
            .is_none_or(|id| !members.contains(id))
            && pkg.get("source").is_none_or(aterm_json::Value::is_null)
    };
    let forks: BTreeSet<&str> = packages
        .iter()
        .filter(|p| is_fork(p))
        .filter_map(|p| p.get("name").and_then(aterm_json::Value::as_str))
        .collect();
    let mut out = Vec::new();
    for pkg in packages {
        let s = |k: &str| pkg.get(k).and_then(aterm_json::Value::as_str).unwrap_or("");
        let (id, name, version) = (s("id"), s("name"), s("version"));
        if name.is_empty() || version.is_empty() {
            return Err(format!(
                "targo metadata lists a package with no name or version ({id})"
            ));
        }
        if name.contains(char::is_whitespace) || version.contains(char::is_whitespace) {
            return Err(format!(
                "the package {name:?} {version:?} cannot go in an argv"
            ));
        }
        let spec = format!("{name}@{version}");
        if members.contains(id) {
            // No `targets` array reads as a library: the safe direction gates more.
            let has_lib = pkg
                .get("targets")
                .and_then(aterm_json::Value::as_array)
                .is_none_or(|targets| {
                    targets.iter().any(|t| {
                        t.get("kind")
                            .and_then(aterm_json::Value::as_array)
                            .is_some_and(|k| {
                                k.iter()
                                    .filter_map(aterm_json::Value::as_str)
                                    .any(|k| LIB_KINDS.contains(&k))
                            })
                    })
                });
            let (kind, argv) = if has_lib {
                let mut argv = member_lane.clone();
                argv.extend(["-p".to_string(), spec.clone(), "--lib".to_string()]);
                (Kind::Member, argv)
            } else {
                (Kind::MemberBin, Vec::new())
            };
            if has_lib {
                for feature in fork_features(pkg, &forks) {
                    if feature.contains(char::is_whitespace) || feature.contains(',') {
                        return Err(format!(
                            "the feature {feature:?} of {spec} cannot go in an argv"
                        ));
                    }
                    let row = format!("{name}{FEATURE_ROW}{feature}");
                    let mut argv = argv.clone();
                    argv.extend(["--features".to_string(), feature]);
                    out.push(Lib {
                        spec: format!("{row}@{version}"),
                        name: row,
                        kind: Kind::Member,
                        argv,
                    });
                }
            }
            out.push(Lib {
                name: name.into(),
                kind,
                spec,
                argv,
            });
            continue;
        }
        if !is_fork(pkg) {
            continue; // a registry or git package: not ours
        }
        let manifest = PathBuf::from(s("manifest_path"));
        if manifest.as_os_str().is_empty() {
            return Err(format!("the path package {spec} has no manifest_path"));
        }
        let rel = manifest.strip_prefix(&root).unwrap_or(&manifest);
        let mut argv = lane_profile_args(&rows(&manifest)?);
        argv.extend([
            "--manifest-path".to_string(),
            rel.display().to_string(),
            "-p".to_string(),
            spec.clone(),
            "--no-default-features".to_string(),
        ]);
        let mut feats: Vec<&str> = features.get(id).cloned().unwrap_or_default();
        feats.sort_unstable();
        if !feats.is_empty() {
            argv.extend(["--features".to_string(), feats.join(",")]);
        }
        argv.push("--lib".into());
        out.push(Lib {
            name: name.into(),
            kind: Kind::Fork,
            spec,
            argv,
        });
    }
    // Twins: two packages of one name (a member and a path fork at another version) would
    // share a floor row and be AMBIGUOUS, so each is named by its spec.
    let mut versions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for lib in &out {
        let (_, version) = lib.spec.rsplit_once('@').unwrap_or((&lib.spec, ""));
        versions
            .entry(package_identity(&lib.name).to_string())
            .or_default()
            .insert(version.to_string());
    }
    for lib in &mut out {
        if versions
            .get(package_identity(&lib.name))
            .is_some_and(|v| v.len() > 1)
        {
            lib.name.clone_from(&lib.spec);
        }
    }
    out.sort_by(|a, b| {
        (a.kind == Kind::Fork, &a.name, &a.spec).cmp(&(b.kind == Kind::Fork, &b.name, &b.spec))
    });
    Ok(out)
}

/// The libraries that left: each name a floor row (`floors`, the ratchet's text) or
/// [`QUEUED_FORKS`] holds that `libs` does not, sorted. The judge fails each as
/// FORK-VANISHED, so a deleted `[patch]` row never stops a fork being counted silently.
#[must_use]
pub fn vanished(libs: &[Lib], floors: &str) -> Vec<String> {
    let present: BTreeSet<&str> = libs.iter().map(|l| l.name.as_str()).collect();
    let named = floors
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split('\t').next())
        .filter(|n| !n.trim().is_empty())
        .chain(QUEUED_FORKS.iter().map(|(n, _)| *n));
    let gone: BTreeSet<&str> = named.filter(|n| !present.contains(n)).collect();
    gone.into_iter().map(str::to_string).collect()
}

/// One selected library and what the lane decided about it before anything ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selected {
    pub lib: Lib,
    pub decision: Decision,
}

/// Selects `names` (all of `libs` when `None`), each decided: two selected rows of one name
/// are AMBIGUOUS (one floor cannot judge both), a member with no library NO-LIB, a fork
/// [`QUEUED_FORKS`] names QUEUED, the rest gated. A package's name also selects its
/// fork-feature rows and both of a twin's rows; a twin's spec selects that one.
///
/// # Errors
/// A name in neither the workspace nor the graph's forks: a run over nothing it asked for
/// must not print a verdict.
pub fn select(libs: &[Lib], names: Option<&[String]>) -> Result<Vec<Selected>, String> {
    let chosen: Vec<&Lib> = match names {
        None => libs.iter().collect(),
        Some(names) => {
            let mut out = Vec::new();
            for want in names {
                let hits: Vec<&Lib> = libs
                    .iter()
                    .filter(|l| &l.name == want || package_identity(&l.name) == want)
                    .collect();
                if hits.is_empty() {
                    return Err(format!(
                        "{want} is in neither the workspace nor the resolved graph's forks"
                    ));
                }
                for h in hits {
                    if !out.iter().any(|o: &&Lib| o.spec == h.spec) {
                        out.push(h);
                    }
                }
            }
            out
        }
    };
    Ok(chosen
        .iter()
        .map(|lib| {
            let twins = chosen.iter().filter(|o| o.name == lib.name).count();
            let decision = if twins > 1 {
                Decision::Ambiguous
            } else if lib.kind == Kind::MemberBin {
                Decision::NoLib
            } else if lib.kind == Kind::Fork && QUEUED_FORKS.iter().any(|(n, _)| *n == lib.name) {
                Decision::Queued("queued".into())
            } else {
                Decision::Gate(trust_facts::Measured::default())
            };
            Selected {
                lib: (*lib).clone(),
                decision,
            }
        })
        .collect())
}

// Slots, order and the budget

/// `--jobs auto`: one slot per six cores and per 16 GiB, at least one; memory that
/// cannot be read allows one.
#[must_use]
pub fn auto_slots(cores: usize, mem: u64) -> usize {
    let by_mem = if mem == 0 {
        1
    } else {
        usize::try_from(mem / (16 << 30)).unwrap_or(usize::MAX)
    };
    (cores / 6).min(by_mem).max(1)
}

/// Heavy checks at once: one per 64 GiB, at least one.
#[must_use]
pub fn heavy_slots(mem: u64) -> usize {
    usize::try_from(mem / (64 << 30))
        .unwrap_or(usize::MAX)
        .max(1)
}

/// What a library's last finished check cost on this machine (the cost history).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cost {
    pub cpu: u64,
    pub wall: u64,
    /// Its largest process's resident peak, bytes.
    pub maxrss: u64,
    /// When it was measured (RFC 3339; empty when its row named no time): a row
    /// carried over by a rewrite keeps its own.
    pub when: String,
}

const COSTS_HEADER: &str = "# trust lane cost history: <name> <CPU s> <wall s> <largest process's peak RSS bytes> <when>; the slots take the longest first, a library with no row first of all";

/// The committed seed's first line ([`trust_facts::COSTS`]), above [`COSTS_HEADER`].
const SEED_HEADER: &str = "# The trust lane's cost-history SEED: the fallback a clone with no history of its own reads (a library its git directory's aterm-release/trust-costs.tsv has no row for takes its row here). `targo --unverified ship trust run --update-ratchet` rewrites it with floors.tsv and memory.tsv. A library in neither file runs first, as heavy.";

/// What orders a run: this clone's history over the committed seed, a row of the
/// history winning. Either may be absent.
#[must_use]
pub fn read_costs(history: Option<&Path>, seed: &Path) -> BTreeMap<String, Cost> {
    let mut all = cost_file(seed);
    all.extend(history.map(cost_file).unwrap_or_default());
    all
}

/// The rows of a cost file; one that cannot be read has none.
fn cost_file(path: &Path) -> BTreeMap<String, Cost> {
    fs::read_to_string(path)
        .map(|t| parse_costs(&t))
        .unwrap_or_default()
}

/// What `--update-ratchet` writes back to the committed seed, before this run's own
/// measurements go over it: for each library the graph still holds, the LATER of its
/// committed row and this clone's history row (another host's newer measurement is not
/// undone by an older one here; a row that names no time is the oldest), and no row for
/// a library that left the graph (a deleted crate is not carried back in from a history).
#[must_use]
pub fn seed_rows(
    committed: BTreeMap<String, Cost>,
    history: BTreeMap<String, Cost>,
    libraries: &BTreeSet<String>,
) -> BTreeMap<String, Cost> {
    let mut all = committed;
    for (name, row) in history {
        if all.get(&name).is_none_or(|c| row.when >= c.when) {
            all.insert(name, row);
        }
    }
    all.retain(|name, _| libraries.contains(name));
    all
}

/// Read the cost history; rows that do not parse are skipped (it only orders a run).
#[must_use]
pub fn parse_costs(text: &str) -> BTreeMap<String, Cost> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            Some((
                (*f.first()?).to_string(),
                Cost {
                    cpu: f.get(1)?.parse().ok()?,
                    wall: f.get(2)?.parse().ok()?,
                    maxrss: f.get(3)?.parse().ok()?,
                    when: f.get(4).map_or_else(String::new, |w| (*w).to_string()),
                },
            ))
        })
        .collect()
}

/// The run order: libraries with no cost first (one could be the longest of all), in the
/// given order, then the rest by CPU seconds, longest first. A library is heavy when its
/// largest process reached [`HEAVY_BYTES`], or nothing says it did not.
#[must_use]
pub fn order(names: &[&str], costs: &BTreeMap<String, Cost>) -> Vec<(usize, bool)> {
    let mut idx: Vec<usize> = (0..names.len()).collect();
    idx.sort_by_key(|&i| match costs.get(names[i]) {
        None => (0, 0, i),
        Some(c) => (1, u64::MAX - c.cpu, i),
    });
    idx.into_iter()
        .map(|i| {
            (
                i,
                costs.get(names[i]).is_none_or(|c| c.maxrss >= HEAVY_BYTES),
            )
        })
        .collect()
}

/// One check a slot runs.
#[derive(Clone, Debug)]
pub struct Job {
    pub slot: usize,
    pub name: String,
    pub argv: Vec<String>,
    pub log: PathBuf,
    /// `--report-dir`, under `--obligations`.
    pub report: Option<PathBuf>,
    /// The slot's `CARGO_TARGET_DIR`, named when the check starts.
    pub target: PathBuf,
    pub heavy: bool,
    /// A fork's lockfile, named when the check starts ([`fork_lock`]).
    pub lock: Option<ForkLock>,
}

/// The lockfile cargo writes beside a fork's manifest when the fork is checked by its
/// own path: a fork resolves on its own, never in the root lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkLock {
    pub path: PathBuf,
    /// It was not there when the check started: this run's check created it, and a
    /// lockfile left in vendor/ fails the forge's provenance check.
    pub created: bool,
}

/// The lockfile the check of `argv` (one with `--manifest-path`) uses in `repo`, and
/// whether it is there now; `None` for a check by `-p`.
#[must_use]
pub fn fork_lock(repo: &Path, argv: &[String]) -> Option<ForkLock> {
    let path = repo.join(manifest_of(argv)?).parent()?.join("Cargo.lock");
    Some(ForkLock {
        created: !path.exists(),
        path,
    })
}

/// How a check ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ended {
    /// Its exit status, 128 + N for a signal.
    pub code: i32,
    /// User + system CPU of it and every process it reaped.
    pub cpu: Duration,
    /// Its largest process's resident peak, bytes.
    pub maxrss: u64,
}

/// A running check.
pub trait Check {
    /// `Some` once it has ended (and is reaped).
    ///
    /// # Errors
    /// It cannot be waited for.
    fn poll(&mut self) -> std::io::Result<Option<Ended>>;
    /// End it and everything it started, trustd left alone, and reap it.
    fn end(&mut self) -> Ended;
}

/// What the slots run checks with: the real prover, or a test's.
pub trait Checks {
    /// Start `job` in its slot, naming the slot's target directory in it.
    ///
    /// # Errors
    /// The driver could not be started: nothing this run decides.
    fn start(&mut self, job: &mut Job) -> std::io::Result<Box<dyn Check>>;
    /// This build's trustd serves the socket, held for the run: a second check may start.
    fn trustd_held(&self) -> bool;
    /// What the socket holds now (`<none|ours|foreign|silent>\t<what>`), for a check
    /// targo would not start.
    fn probe_trustd(&mut self) -> String;
    /// The run was interrupted.
    fn interrupted(&self) -> bool;
    /// Time since the slots started.
    fn now(&self) -> Duration;
    /// Wait a poll's length.
    fn pause(&mut self);
}

/// A check that ended, as the slots saw it.
#[derive(Clone, Debug)]
pub struct Finished {
    pub job: Job,
    pub ended: Ended,
    pub wall: Duration,
    /// The budget ended it.
    pub over_budget: bool,
}

/// Why the slots stopped before their queue was done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stopped {
    Interrupted,
    /// A check could not be started at all.
    Unstartable(String),
}

/// Runs `jobs` (already in order, each with its slot unset) on `slots` slots, at most
/// `heavy` heavy ones at once, each ended at `budget`; a second check starts only while
/// [`Checks::trustd_held`]. `done` sees each check as it ends, before its slot is reused
/// (its dep-info is the slot's newest).
///
/// # Errors
/// An interrupt, or a check that could not be started: every running check is ended.
pub fn run_slots(
    jobs: Vec<Job>,
    slots: usize,
    heavy: usize,
    budget: Duration,
    checks: &mut dyn Checks,
    done: &mut dyn FnMut(Finished, &mut dyn Checks),
) -> Result<(), Stopped> {
    struct Running {
        job: Job,
        check: Box<dyn Check>,
        since: Duration,
    }
    let mut queue: VecDeque<Job> = jobs.into();
    let mut free: Vec<usize> = (0..slots.max(1)).rev().collect();
    let mut running: Vec<Running> = Vec::new();
    let stop_all = |running: &mut Vec<Running>| {
        for r in running.iter_mut() {
            r.check.end();
        }
        running.clear();
    };
    loop {
        if checks.interrupted() {
            stop_all(&mut running);
            return Err(Stopped::Interrupted);
        }
        while !free.is_empty() && (running.is_empty() || checks.trustd_held()) {
            let heavy_now = running.iter().filter(|r| r.job.heavy).count();
            let Some(i) = queue.iter().position(|j| !j.heavy || heavy_now < heavy) else {
                break;
            };
            let Some(mut job) = queue.remove(i) else {
                break;
            };
            job.slot = free.pop().unwrap_or_default();
            match checks.start(&mut job) {
                Ok(check) => running.push(Running {
                    job,
                    check,
                    since: checks.now(),
                }),
                Err(e) => {
                    stop_all(&mut running);
                    return Err(Stopped::Unstartable(format!(
                        "the check of {} could not be started: {e}",
                        job.name
                    )));
                }
            }
        }
        if running.is_empty() && queue.is_empty() {
            return Ok(());
        }
        let mut i = 0;
        while i < running.len() {
            let now = checks.now();
            let over = now.saturating_sub(running[i].since) >= budget;
            let ended = match running[i].check.poll() {
                Ok(Some(e)) => Some((e, false)),
                Ok(None) if over => Some((running[i].check.end(), true)),
                Ok(None) => None,
                Err(_) => Some((running[i].check.end(), false)),
            };
            if let Some((ended, over_budget)) = ended {
                let r = running.swap_remove(i);
                free.push(r.job.slot);
                done(
                    Finished {
                        wall: checks.now().saturating_sub(r.since),
                        job: r.job,
                        ended,
                        over_budget,
                    },
                    checks,
                );
            } else {
                i += 1;
            }
        }
        checks.pause();
    }
}

/// What a check leaves beside its log once it ends, for the judge: its exit status, the
/// budget that ended it, and what the trustd socket held when targo would not start it.
/// A fork's lockfile ([`ForkLock`]) the check created goes.
///
/// # Errors
/// A mark the judge needs could not be written.
pub fn record(
    f: &Finished,
    budget: Duration,
    probe: &mut dyn FnMut() -> String,
) -> std::io::Result<()> {
    let log = &f.job.log;
    let beside = |ext: &str| PathBuf::from(format!("{}.{ext}", log.display()));
    fs::write(beside("rc"), format!("{}\n", f.ended.code))?;
    if f.over_budget {
        fs::write(beside("budget"), format!("{}\n", budget.as_secs()))?;
    }
    let text = fs::read(log).unwrap_or_default();
    let refused = String::from_utf8_lossy(&text).contains(trust_facts::TRUSTD_REFUSED);
    if refused {
        fs::write(beside("trustd"), format!("{}\n", probe()))?;
    }
    if let Some(lock) = &f.job.lock
        && lock.created
    {
        let _ = fs::remove_file(&lock.path);
    }
    Ok(())
}

// A run

/// What a run needs of the machine: its prover, the graph that prover resolves, and
/// checks run beside that prover's trustd. [`run`] uses the machine's ([`Machine`]); a
/// test brings its own.
trait Bench {
    /// The toolchain every compile of the run uses: [`Config::prover`], else the one
    /// the cut builds with.
    ///
    /// # Errors
    /// Why there is none.
    fn driver(&mut self, cfg: &Config) -> Result<Driver, String>;
    /// `targo metadata --format-version 1 --all-features` of `repo` by `driver`, as it
    /// printed it.
    ///
    /// # Errors
    /// Why it printed none.
    fn metadata(&mut self, driver: &Driver, repo: &Path) -> Result<Vec<u8>, String>;
    /// The cargo home the lane's compiles read: `$HOME/.cargo` (the lane passes HOME,
    /// and neither `CARGO_HOME` nor `TARGO_HOME`).
    fn cargo_home(&self) -> String;
    /// The directory of the user's cache a fallback target root goes under
    /// ([`cache_base`]); `None` with no home to hold one.
    fn cache(&self) -> Option<PathBuf>;
    /// Run `jobs` on `slots` slots ([`run_slots`]) beside this prover's trustd, `done`
    /// seeing each check as it ends.
    ///
    /// # Errors
    /// Why nothing was decided: every check it started is ended.
    fn checks(
        &mut self,
        cfg: &Config,
        prep: &Prepared,
        jobs: Vec<Job>,
        slots: usize,
        done: &mut dyn FnMut(Finished, &mut dyn Checks),
        out: &mut dyn Write,
    ) -> Result<(), String>;
}

/// The machine's prover, graph and trustd.
struct Machine;

impl Bench for Machine {
    fn driver(&mut self, cfg: &Config) -> Result<Driver, String> {
        let bin = match &cfg.prover {
            Some(b) if b.is_absolute() => b.clone(),
            Some(b) => return Err(format!("--prover {} is not an absolute path", b.display())),
            None => crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?,
        };
        Driver::at(&bin)
    }

    fn metadata(&mut self, driver: &Driver, repo: &Path) -> Result<Vec<u8>, String> {
        // From the pinned driver: another toolchain's resolve could gate members this targo
        // resolves otherwise. With every feature: a path package reachable only through an
        // off-by-default feature is still forked code this tree builds, so it is decided
        // like any fork and verified with the feature union a full build can turn on.
        // Members are gated with their default features, plus a row per feature that turns
        // on fork code (`fork_features`). aterm-forge's direct_vendor reads the same graph.
        let o = command(&driver.targo)
            .args(["metadata", "--format-version", "1", "--all-features"])
            .env_clear()
            .envs(lane_env(&driver.bin, &host_var))
            .current_dir(repo)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("targo metadata: {e}"))?;
        if !o.status.success() {
            return Err(format!(
                "targo metadata exited {}: {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
        Ok(o.stdout)
    }

    fn cargo_home(&self) -> String {
        host_var("HOME")
            .map(|h| format!("{h}/.cargo"))
            .unwrap_or_default()
    }

    fn cache(&self) -> Option<PathBuf> {
        let xdg = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from);
        cache_base(
            aterm_types::dirs::home_dir().as_deref(),
            xdg.as_deref(),
            cfg!(target_os = "macos"),
        )
    }

    fn checks(
        &mut self,
        cfg: &Config,
        prep: &Prepared,
        jobs: Vec<Job>,
        slots: usize,
        done: &mut dyn FnMut(Finished, &mut dyn Checks),
        out: &mut dyn Write,
    ) -> Result<(), String> {
        let sock = trustd::socket();
        let want = trustd::build_digest(&prep.driver.bin.join("trustd"));
        trustd::wait_usable(&sock, &want, cfg.trustd_wait, out)?;
        let keeper = trustd::Keeper::start(
            sock.clone(),
            want.clone(),
            prep.logdir.join(".trustd-grants"),
            prep.logdir.join(".trustd-seen"),
        );
        let mut real = RealChecks {
            driver: &prep.driver,
            repo: &cfg.repo,
            root: &prep.root,
            env: lane_env(&prep.driver.bin, &host_var),
            keeper: &keeper,
            sock: &sock,
            want: &want,
            t0: Instant::now(),
            locks: BTreeMap::new(),
        };
        let ran = run_slots(
            jobs,
            slots,
            heavy_slots(prep.mem),
            cfg.budget,
            &mut real,
            done,
        );
        let locks = std::mem::take(&mut real.locks);
        drop(real);
        drop(keeper);
        for (lock, created) in locks {
            if created {
                let _ = fs::remove_file(lock);
            }
        }
        if ran == Err(Stopped::Interrupted) {
            reap_stale_checks(&prep.root);
        }
        ran.map_err(stopped)
    }
}

/// Why slots that stopped decided nothing.
fn stopped(s: Stopped) -> String {
    match s {
        Stopped::Interrupted => "interrupted; nothing was decided".into(),
        Stopped::Unstartable(why) => why,
    }
}

/// Everything a run writes before its checks start.
struct Prepared {
    driver: Driver,
    root: PathBuf,
    logdir: PathBuf,
    plan_path: PathBuf,
    plan: String,
    selected: Vec<Selected>,
    refused: BTreeSet<String>,
    files: Option<Filing>,
    jobs: usize,
    mem: u64,
    cores: usize,
    costs_path: Option<PathBuf>,
    /// Every library the graph holds, by its row name: the committed cost seed keeps
    /// rows for these alone ([`seed_rows`]).
    libraries: BTreeSet<String>,
    _lock: RunLock,
}

/// What a filing run read before its checks: the tree and why it may not file.
struct Filing {
    why_not: String,
    start: Option<(String, String)>,
    prover: String,
}

/// Why a run stopped before the judge spoke.
enum Stop {
    /// This host refuses libraries for memory, and the run was asked to stop then.
    Narrowed(Vec<String>),
    /// Nothing was decided: why.
    CouldNotRun(String),
}

impl From<String> for Stop {
    fn from(why: String) -> Stop {
        Stop::CouldNotRun(why)
    }
}

/// Runs the lane (`ship trust run`): progress and the judge's report go to `out`, and a run
/// that decides nothing says why (`trust: COULD NOT RUN — …`).
pub fn run(cfg: &Config, out: &mut dyn Write) -> Outcome {
    crate::interrupt::install();
    run_on(cfg, &mut Machine, out)
}

fn run_on(cfg: &Config, bench: &mut dyn Bench, out: &mut dyn Write) -> Outcome {
    let ran = match prepare(cfg, bench, out) {
        Ok(prep) => {
            let judged = check_and_conclude(cfg, &prep, bench, out).map_err(Stop::CouldNotRun);
            // the next run empties what this could not
            let _ = clear_slots(&prep.root);
            judged
        }
        Err(stop) => Err(stop),
    };
    match ran {
        Ok(outcome) => outcome,
        Err(Stop::Narrowed(lines)) => Outcome::Narrowed(lines),
        Err(Stop::CouldNotRun(why)) => {
            let _ = writeln!(out, "trust: COULD NOT RUN — {why}");
            Outcome::CouldNotRun(why)
        }
    }
}

fn prepare(cfg: &Config, bench: &mut dyn Bench, out: &mut dyn Write) -> Result<Prepared, Stop> {
    let driver = bench.driver(cfg)?;
    let _ = writeln!(
        out,
        "trust: prover {} ({})",
        driver.bin.display(),
        if driver.id.is_empty() {
            "no --version answer"
        } else {
            &driver.id
        }
    );
    let Root {
        path: root,
        mine,
        note,
    } = target_root(cfg, bench.cache().as_deref())?;
    if let Some(note) = note {
        let _ = writeln!(out, "trust: {note}");
    }
    if let Some(note) = legacy_root_note(cfg).or_else(|| indexed_root_note(cfg, &root)) {
        let _ = writeln!(out, "trust: {note}");
    }
    let logdir = root.join("logs");
    owner_dirs(&logdir).map_err(|e| format!("{}: {e}", logdir.display()))?;
    if mine.as_ref() == Some(&root) {
        prepare_cache_lock(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    }
    let lock = RunLock::take(
        &root.join(".run.lock"),
        "give this one its own --target-root",
    )?;
    if let Some(mine) = &mine {
        if *mine == root {
            // This selected fallback root names its checkout without following an
            // unknown marker into another file. Only the directory entry is replaced.
            write_checkout_record(&root, &canonical_repo(cfg))
                .map_err(|e| format!("{}: {e}", root.join(CHECKOUT_RECORD).display()))?;
            // The later reaper acts on this selected fallback too. Recheck every
            // authority file and the held lock before handing the root to the bench.
            use std::os::unix::fs::MetadataExt as _;
            let stable_lock = lock
                .0
                .metadata()
                .ok()
                .zip(owned_cache_file(&root.join(".run.lock")))
                .is_some_and(|(held, named)| {
                    (held.dev(), held.ino()) == (named.dev(), named.ino())
                });
            if !stable_lock
                || cleanup_checkout(&root).as_deref() != Some(canonical_repo(cfg).as_path())
            {
                return Err(format!("{}: fallback cache custody changed", root.display()).into());
            }
        } else if cleanup_checkout(mine).as_deref() == Some(canonical_repo(cfg).as_path())
            && retire(mine)
        {
            // The checkout's own root is admitted again (a `chmod go-w` the fallback's
            // note gave): what a killed fallback run left would otherwise stay for good.
            let _ = writeln!(
                out,
                "trust: removed {}, this checkout's fallback target root, now that {} is admitted",
                mine.display(),
                root.display()
            );
        }
        let orphans = mine
            .parent()
            .map(|cache| prune_orphans(cache, mine))
            .unwrap_or_default();
        for (dir, checkout) in orphans {
            let _ = writeln!(
                out,
                "trust: removed {}, the target root of {}, which is gone",
                dir.display(),
                checkout.display()
            );
        }
    }
    // Only a run that files reads its slots' dep-info, and needs the prover's identity
    // (a hash of the whole toolchain).
    let files = matches!(cfg.scope, Scope::WholeTree | Scope::Remeasure);
    // THE SLOTS START EMPTY: a check a killed run left is ended and every slot goes, so
    // each dep-info file the judge reads at the end was written by this run's compiles.
    // Where a file is says whose it is; its stamp could not (it is its file system's
    // clock, which can lag the wall clock). A run that files stops on a slot it cannot
    // empty; a -p run reads none, and its checks name their units by a fresh session
    // nonce, so it says so and goes on.
    reap_stale_checks(&root);
    if let Err(e) = clear_slots(&root) {
        let why = format!("{}: a slot could not be emptied ({e})", root.display());
        if files {
            return Err(why.into());
        }
        let _ = writeln!(out, "trust: {why}; a -p run reads none of it");
    }
    let git = GitCli::new(&cfg.repo);
    let common = crate::ledger::git_ok(
        &git,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map(|o| PathBuf::from(o.stdout_utf8().trim()))
    .ok();
    let state = cfg
        .state
        .clone()
        .or_else(|| common.as_ref().map(|c| c.join("aterm-release")));
    let meta_out = bench.metadata(&driver, &cfg.repo)?;
    let meta: aterm_json::Value = aterm_json::from_str(&String::from_utf8_lossy(&meta_out))
        .map_err(|e| format!("targo metadata printed no JSON: {e}"))?;
    let repo = cfg.repo.clone();
    let libs = gate_list(&meta, &|m| profile_rows_for(m, &repo))?;
    // A run over the whole tree answers for every library the floors name.
    let gone = if files {
        let floors = cfg.repo.join(trust_facts::RATCHET);
        let text = fs::read_to_string(&floors)
            .map_err(|e| format!("the floors {}: {e}", floors.display()))?;
        vanished(&libs, &text)
    } else {
        Vec::new()
    };
    let filing = files.then(|| {
        let start = tree_state(&cfg.repo);
        let prover_id = trust_facts::prover_identity(&driver.bin);
        Filing {
            why_not: match &start {
                Some(_) => String::new(),
                None => "the checkout is not HEAD's bytes (an edit git's config hides included), \
                         or not a git checkout, so no tree is what it verified"
                    .into(),
            },
            start,
            prover: prover_id
                .as_ref()
                .map(|(id, _)| id.clone())
                .unwrap_or_default(),
        }
    });
    let (names, only): (Option<Vec<String>>, String) = match &cfg.scope {
        Scope::WholeTree => (None, String::new()),
        Scope::Packages(p) => (Some(p.clone()), p.join(" ")),
        Scope::Remeasure => {
            let why = filing
                .as_ref()
                .map(|f| f.why_not.clone())
                .unwrap_or_default();
            if !why.is_empty() {
                return Err(Stop::CouldNotRun(format!(
                    "--remeasure completes the stored facts of this tree, and this run would file none: {why}"
                )));
            }
            let prover = trust_facts::Prover::at(&driver.bin);
            let pending = trust_facts::pending_for(&git, &prover, "HEAD")
                .map_err(|e| format!("--remeasure: {e}"))?
                .map_err(|why| format!("--remeasure: no stored facts to complete: {why}"))?;
            let only = if pending.is_empty() {
                "--remeasure: nothing left undecided".into()
            } else {
                format!("--remeasure: {}", pending.join(" "))
            };
            (Some(pending), only)
        }
    };
    let selected = select(&libs, names.as_deref())?;
    let (mem, memtotal) = host_memory();
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let jobs = cfg.jobs.unwrap_or_else(|| auto_slots(cores, mem));
    let host_obs = state.as_ref().map(|s| s.join("trust-memory-host.tsv"));
    let mut plan = String::new();
    plan.push_str(trust_facts::PLAN_MAGIC);
    plan.push('\n');
    let flag = |b: bool| if b { "1" } else { "0" };
    let remeasure = matches!(cfg.scope, Scope::Remeasure);
    for (k, v) in [
        ("driver", driver.targo.display().to_string()),
        ("driver-id", driver.id.clone()),
        ("only", only),
        ("logdir", logdir.display().to_string()),
        (
            "ratchet",
            cfg.repo.join(trust_facts::RATCHET).display().to_string(),
        ),
        (
            "memory-file",
            cfg.repo.join(trust_facts::MEMORY).display().to_string(),
        ),
        (
            "host-obs",
            host_obs
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ),
        ("try-refused", flag(cfg.try_refused).into()),
        ("remeasure", flag(remeasure).into()),
        ("host-mem", mem.to_string()),
        (
            "host-mem-os",
            if memtotal { "MemTotal" } else { "-" }.into(),
        ),
        ("now", utc_now()),
    ] {
        push_kv(&mut plan, k, &v);
    }
    for v in &gone {
        push_kv(&mut plan, "vanished", v);
    }
    for s in &selected {
        if s.decision == Decision::Ambiguous
            && let Some(other) = selected
                .iter()
                .find(|o| o.lib.name == s.lib.name && o.lib.spec != s.lib.spec)
            && s.lib.spec < other.lib.spec
        {
            plan.push_str(&format!(
                "ambiguous {}\t{}\t{}\n",
                s.lib.name, s.lib.spec, other.lib.spec
            ));
        }
    }
    for s in &selected {
        plan.push_str(&format!(
            "crate {}\t{}\t{}\n",
            s.lib.name,
            s.lib.spec,
            s.decision.word()
        ));
    }
    // A library whose floor this host's verifier ceiling cannot reproduce is refused up
    // front, never run (the judge's `admit`).
    let parsed = Plan::parse(&plan)?;
    let (admit_lines, refusals) = parsed.admit()?;
    plan.push_str(&admit_lines);
    let plan_path = logdir.join(".plan");
    fs::write(&plan_path, &plan).map_err(|e| format!("{}: {e}", plan_path.display()))?;
    let refused: BTreeSet<String> = refusals.iter().map(|(n, _)| n.clone()).collect();
    if !refusals.is_empty() {
        for (_, l) in &refusals {
            let _ = writeln!(out, "trust: {l}");
        }
        if cfg.stop_if_narrowed {
            return Err(Stop::Narrowed(
                refusals.into_iter().map(|(_, l)| l).collect(),
            ));
        }
    }
    Ok(Prepared {
        driver,
        logdir,
        plan_path,
        plan,
        selected,
        refused,
        files: filing,
        jobs,
        mem,
        cores,
        costs_path: state.as_ref().map(|s| s.join("trust-costs.tsv")),
        libraries: libs.iter().map(|l| l.name.clone()).collect(),
        root,
        _lock: lock,
    })
}

/// The checks, then the judge: `Err` is why nothing was decided.
fn check_and_conclude(
    cfg: &Config,
    prep: &Prepared,
    bench: &mut dyn Bench,
    out: &mut dyn Write,
) -> Result<Outcome, String> {
    let work: Vec<&Selected> = prep
        .selected
        .iter()
        .filter(|s| matches!(s.decision, Decision::Gate(_)) && !prep.refused.contains(&s.lib.name))
        .collect();
    // A log is this run's or nothing: one an earlier run left would read as this one's.
    for stale in [".trustd-grants", ".trustd-seen"] {
        let _ = fs::remove_file(prep.logdir.join(stale));
    }
    for s in &work {
        let log = prep.logdir.join(format!("{}.log", s.lib.spec));
        for ext in ["", ".rc", ".trustd", ".budget", ".oblig.tsv", ".oblig.err"] {
            let _ = fs::remove_file(format!("{}{ext}", log.display()));
        }
        let _ = fs::remove_dir_all(format!("{}.report", log.display()));
    }
    let seed = cfg.repo.join(trust_facts::COSTS);
    let costs = read_costs(prep.costs_path.as_deref(), &seed);
    let names: Vec<&str> = work.iter().map(|s| s.lib.name.as_str()).collect();
    let jobs: Vec<Job> = order(&names, &costs)
        .into_iter()
        .map(|(i, heavy)| {
            let lib = &work[i].lib;
            let log = prep.logdir.join(format!("{}.log", lib.spec));
            Job {
                slot: 0,
                name: lib.name.clone(),
                argv: lib.argv.clone(),
                report: cfg
                    .obligations
                    .then(|| PathBuf::from(format!("{}.report", log.display()))),
                log,
                target: PathBuf::new(),
                heavy,
                lock: None,
            }
        })
        .collect();
    let mut new_costs: BTreeMap<String, Cost> = BTreeMap::new();
    if !jobs.is_empty() {
        let slots = prep.jobs.min(jobs.len());
        let _ = writeln!(
            out,
            "trust: checking {} librar{} in {slots} slot(s) under {}, longest first; each ended at {} s of wall time (OVER-BUDGET)",
            jobs.len(),
            if jobs.len() == 1 { "y" } else { "ies" },
            prep.root.display(),
            cfg.budget.as_secs()
        );
        let reducer = cfg.obligations.then(|| Reducer::start(cfg.repo.clone()));
        let budget = cfg.budget;
        let mut done = |f: Finished, checks: &mut dyn Checks| {
            let mut probe = || checks.probe_trustd();
            let _ = record(&f, budget, &mut probe);
            if let Some(r) = &reducer {
                r.send(&f.job);
            }
            if let Some(c) = finished_cost(&f) {
                new_costs.insert(f.job.name.clone(), c);
            }
        };
        let ran = bench.checks(cfg, prep, jobs, slots, &mut done, out);
        // Every record is written before the judge reads one; a run that decided
        // nothing reduces no more.
        if let Some(r) = reducer {
            r.finish(ran.is_err());
        }
        // A finished check's cost stands however the run ended.
        write_costs(prep.costs_path.as_deref(), &new_costs, out);
        ran?;
    }
    // The plan's filing part, then the judge.
    let mut plan = prep.plan.clone();
    plan.push_str(&format!("trustd {}\n", trustd::socket().display()));
    if let Some(f) = &prep.files {
        let mut why = f.why_not.clone();
        if why.is_empty() && tree_state(&cfg.repo) != f.start {
            why = "the checkout changed during the run (an edit or a commit)".into();
        }
        let git = GitCli::new(&cfg.repo);
        let store = crate::gates::receipt_store(&git)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let (head, tree) = f.start.clone().unwrap_or_else(|| ("-".into(), "-".into()));
        for (k, v) in [
            ("file", why),
            ("store", store),
            ("commit", head),
            ("tree", tree),
            ("prover", f.prover.clone()),
            ("prover-bin", prep.driver.bin.display().to_string()),
        ] {
            push_kv(&mut plan, k, &v);
        }
        for (k, v) in LANE_PINS {
            push_kv(&mut plan, "lane-pin", &format!("{k}={v}"));
        }
        for k in LANE_PASSED {
            push_kv(&mut plan, "lane-passed", k);
        }
        push_kv(&mut plan, "cargo-home", &bench.cargo_home());
        push_kv(&mut plan, "target-root", &prep.root.display().to_string());
        for (k, v) in [
            (
                "host",
                format!("{} cores, {} GiB", prep.cores, prep.mem >> 30),
            ),
            ("load", "-".into()),
            ("jobs", prep.jobs.to_string()),
            ("date", utc_now()),
        ] {
            push_kv(&mut plan, k, &v);
        }
    }
    fs::write(&prep.plan_path, &plan).map_err(|e| format!("{}: {e}", prep.plan_path.display()))?;
    let mut parsed = Plan::parse(&plan)?;
    let update = if cfg.update_ratchet {
        Update::RAISE
    } else {
        Update::NONE
    };
    let git = GitCli::new(&cfg.repo);
    let done = trust_facts::conclude(&git, &cfg.repo, &mut parsed, update)
        .map_err(|e| format!("the judge could not conclude: {e}"))?;
    if cfg.update_ratchet && !new_costs.is_empty() {
        // The seed follows the floors, written after the filing as they are: the later
        // of the seed's and this clone's row for every library still in the graph,
        // this run's measurements over them.
        let history = prep
            .costs_path
            .as_deref()
            .map(cost_file)
            .unwrap_or_default();
        let mut all = seed_rows(cost_file(&seed), history, &prep.libraries);
        all.extend(new_costs);
        write_cost_file(
            &seed,
            &format!("{SEED_HEADER}\n{COSTS_HEADER}\n"),
            &all,
            out,
        );
    }
    for l in &done.said {
        let _ = writeln!(out, "{l}");
    }
    let _ = write!(out, "{}", done.verdict.report);
    if let Some(l) = &done.facts_line {
        let _ = writeln!(out, "{l}");
    }
    if !done.verdict.tail.is_empty() {
        let _ = writeln!(out, "{}", done.verdict.tail);
    }
    Ok(Outcome::of(&done.verdict))
}

/// `--obligations`: each check's proof report reduced on a thread of its own, so a
/// report of gigabytes never holds the slots (no check polled or held to its budget
/// meanwhile).
struct Reducer {
    tx: Option<std::sync::mpsc::Sender<Job>>,
    skip: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Reducer {
    fn start(repo: PathBuf) -> Reducer {
        use std::sync::atomic::Ordering;
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let skip = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let skipping = std::sync::Arc::clone(&skip);
        let thread = std::thread::spawn(move || {
            for job in rx {
                if skipping.load(Ordering::SeqCst) {
                    if let Some(r) = &job.report {
                        let _ = fs::remove_dir_all(r);
                    }
                } else {
                    reduce_report(&job, &repo);
                }
            }
        });
        Reducer {
            tx: Some(tx),
            skip,
            thread: Some(thread),
        }
    }

    fn send(&self, job: &Job) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(job.clone());
        }
    }

    /// Wait for every reduction sent; with `skip`, those not begun only lose their
    /// report.
    fn finish(mut self, skip: bool) {
        self.skip.store(skip, std::sync::atomic::Ordering::SeqCst);
        self.join();
    }

    fn join(&mut self) {
        drop(self.tx.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Reducer {
    fn drop(&mut self) {
        self.join();
    }
}

/// The cost of a check that finished (a Summary, nothing cutting it short): what the
/// order reads next time. A check that did not finish measured only part of its library.
fn finished_cost(f: &Finished) -> Option<Cost> {
    if f.over_budget {
        return None;
    }
    let file = fs::File::open(&f.job.log).ok()?;
    let m = trust_facts::Measured::of_log(BufReader::new(file), &f.ended.code.to_string());
    (!m.summary.is_empty() && m.ended.is_empty() && !m.transport).then(|| Cost {
        cpu: f.ended.cpu.as_secs(),
        wall: f.wall.as_secs(),
        maxrss: f.ended.maxrss,
        when: utc_now(),
    })
}

/// Fold `new` into the cost history, one write: it only orders runs, so a write that
/// fails is said and decides nothing.
fn write_costs(path: Option<&Path>, new: &BTreeMap<String, Cost>, out: &mut dyn Write) {
    let Some(path) = path.filter(|_| !new.is_empty()) else {
        return;
    };
    let mut all = fs::read_to_string(path)
        .map(|t| parse_costs(&t))
        .unwrap_or_default();
    all.extend(new.iter().map(|(k, v)| (k.clone(), v.clone())));
    write_cost_file(path, &format!("{COSTS_HEADER}\n"), &all, out);
}

/// Write `all` under `header` to `path`, atomically, each row stamped with when it was
/// measured (now, for a row that named no time); a write that fails is said.
fn write_cost_file(path: &Path, header: &str, all: &BTreeMap<String, Cost>, out: &mut dyn Write) {
    let now = utc_now();
    let mut text = header.to_string();
    for (name, c) in all {
        let when = if c.when.is_empty() { &now } else { &c.when };
        text.push_str(&format!(
            "{name}\t{}\t{}\t{}\t{when}\n",
            c.cpu, c.wall, c.maxrss
        ));
    }
    let tmp = path.with_extension(format!("tsv.{}", std::process::id()));
    let written = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(&tmp, text))
        .and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        let _ = writeln!(
            out,
            "trust: the cost history was not written ({}: {e}); the next run orders by what it holds",
            path.display()
        );
    }
}

/// `--obligations`: reduce the check's proof report beside its log. Retire
/// the raw directory only after success; a refusal keeps its input for diagnosis.
fn reduce_report(job: &Job, repo: &Path) {
    let Some(report) = &job.report else {
        return;
    };
    let manifest = manifest_of(&job.argv).map(str::to_string);
    let args = crate::trust_oblig::ReduceArgs {
        report: report.join("report.ndjson"),
        package: package_identity(&job.name).to_string(),
        repo: repo.to_path_buf(),
        manifest,
        out: PathBuf::from(format!(
            "{}{}",
            job.log.display(),
            crate::trust_oblig::RECORD_SUFFIX
        )),
    };
    if let Err(why) = crate::trust_oblig::write_record(&args) {
        let _ = fs::write(
            format!("{}.oblig.err", job.log.display()),
            format!(
                "{why}\nraw report cleanup skipped for {}\n",
                report.display()
            ),
        );
        return;
    }
    let _ = fs::remove_dir_all(report);
}

/// `<head> <tree>` when the checkout is HEAD's bytes ([`crate::clean::strictly_clean`]),
/// else `None`: a run files facts under HEAD's tree for what its checks compiled.
fn tree_state(repo: &Path) -> Option<(String, String)> {
    if !crate::clean::strictly_clean(repo)? {
        return None;
    }
    let git = GitCli::new(repo);
    let rev = |r: &str| crate::ledger::rev_parse(&git, r).ok();
    Some((rev("HEAD")?, rev("HEAD^{tree}")?))
}

/// The slots are scratch: every check names its units by a fresh session nonce, so
/// nothing one check builds is reused, and a kept slot only grows.
///
/// # Errors
/// A slot that could not be removed (the first such error).
fn clear_slots(root: &Path) -> std::io::Result<()> {
    let rd = match fs::read_dir(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        rd => rd?,
    };
    let mut first = Ok(());
    for e in rd.flatten() {
        let n = e.file_name();
        let n = n.to_string_lossy();
        if n.strip_prefix('j')
            .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            && let Err(e) = fs::remove_dir_all(e.path())
            && first.is_ok()
        {
            first = Err(e);
        }
    }
    first
}

/// End the checks a killed run left (`.check.<slot>` names each by pid and start
/// time), so no orphaned verifier runs on for hours.
fn reap_stale_checks(root: &Path) {
    let Ok(rd) = fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        if !e.file_name().to_string_lossy().starts_with(".check.") {
            continue;
        }
        let text = fs::read_to_string(e.path()).unwrap_or_default();
        let _ = fs::remove_file(e.path());
        let Some((pid, when)) = text.trim().split_once(' ') else {
            continue;
        };
        if let Ok(pid) = pid.parse::<u32>()
            && proc_tree::started(pid).as_deref() == Some(when)
        {
            proc_tree::end_tree(pid, &mut || proc_tree::started(pid).is_none());
        }
    }
}

/// This host's RAM in bytes and whether that is Linux's MemTotal (a few percent under
/// what the machine was sold with); 0 when it cannot be read.
fn host_memory() -> (u64, bool) {
    if let Some(b) = crate::artifact::host_memory_bytes() {
        return (b, false);
    }
    let kib = fs::read_to_string("/proc/meminfo").ok().and_then(|t| {
        t.lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
    });
    kib.map_or((0, false), |k| (k * 1024, true))
}

fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    aterm_types::rfc3339::format_rfc3339(secs)
}

fn push_kv(plan: &mut String, k: &str, v: &str) {
    plan.push_str(k);
    plan.push(' ');
    plan.push_str(&one_line(v));
    plan.push('\n');
}

// Where a run's slots go, and the toolchain they are checked with

/// The checkout's own target root, `<repo>/target-trust.noindex`: a folder named
/// `*.noindex` is skipped by Spotlight with all it holds, so no check's intermediates
/// feed `mds_stores`.
pub(crate) const OWN_ROOT: &str = "target-trust.noindex";

/// The checkout's own target root before it was named for Spotlight to skip: a run
/// with no `--target-root` says when one is left, since nothing builds there now.
const LEGACY_ROOT: &str = "target-trust";

/// The fallback target roots' directory in the user's cache, beside the Linux worker's
/// `aterm-linux-worker`.
const CACHE_DIR: &str = "aterm-trust-lane";

/// macOS's: `~/Library/Caches` is indexed, and a `.noindex` name is the Spotlight exclusion
/// measured to work (the release cache's, `crate::snapshot::CacheRoot`).
const MAC_CACHE_DIR: &str = "Library/Caches/aterm-trust-lane.noindex";

/// The file in a fallback root that names its checkout ([`prune_orphans`]).
const CHECKOUT_RECORD: &str = ".checkout";

/// A component `targo trust` refuses, why, and the `chmod` that clears it when that is
/// the caller's to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refused {
    pub dir: PathBuf,
    pub why: String,
    /// The `chmod` mode that clears the refusal (`go-w`, or `a-st` for a launcher's
    /// special bits) when the caller owns it and only its mode is refused.
    pub chmod: Option<&'static str>,
}

/// [`Refused::chmod`] for a group- or world-writable object of the caller's.
const GO_W: Option<&str> = Some("go-w");

impl Refused {
    /// The command that clears it, when the caller owns it and only its mode keeps it out.
    fn chmod_command(&self) -> Option<String> {
        self.chmod.map(|mode| {
            format!(
                "chmod {mode} {}",
                crate::release::shell_word(&self.dir.to_string_lossy())
            )
        })
    }

    /// What to do: run [`Refused::chmod`] on an object of the caller's that only its mode
    /// keeps out, else `elsewhere` (another owner's object is not the caller's to change).
    pub(crate) fn fix(&self, elsewhere: &str) -> String {
        self.chmod_command()
            .map_or_else(|| elsewhere.to_string(), |c| format!("run: {c}"))
    }
}

/// The fix for a target root a component the caller cannot `chmod` keeps out.
const ELSEWHERE_ROOT: &str = "give --target-root a directory under one you own";

/// A run's target root: where; for a default root, this checkout's directory of the
/// user's cache (the root itself when it falls back, else one an earlier fallback run may
/// have left, which the run retires), beside the others whose orphans it prunes; and the
/// line saying why it is not `<repo>/target-trust.noindex`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Root {
    path: PathBuf,
    mine: Option<PathBuf>,
    note: Option<String>,
}

/// The line a default run prints when the checkout still holds the root it used before
/// [`OWN_ROOT`] (a directory, never followed through a link): left for its owner to
/// remove, because an older run may still hold it or its logs may still be read.
fn legacy_root_note(cfg: &Config) -> Option<String> {
    if cfg.target_root.is_some() {
        return None;
    }
    let legacy = cfg.repo.join(LEGACY_ROOT);
    fs::symlink_metadata(&legacy)
        .is_ok_and(|m| m.is_dir())
        .then(|| {
            format!(
                "{} is this lane's target root from before it was named {OWN_ROOT} (out of \
                 Spotlight); nothing builds there now: remove it once no older run holds it \
                 and its logs are read",
                legacy.display()
            )
        })
}

/// The line a run with a named `--target-root` prints on macOS when no component of it
/// is named `*.noindex`: Spotlight indexes every file the run writes there.
fn indexed_root_note(cfg: &Config, root: &Path) -> Option<String> {
    let skipped = |p: &Path| {
        p.components().any(|c| {
            c.as_os_str()
                .to_string_lossy()
                .ends_with(atpkg::build_cache::NOINDEX)
        })
    };
    (cfg!(target_os = "macos") && cfg.target_root.is_some() && !skipped(root)).then(|| {
        format!(
            "the target root {} is in no folder named *.noindex, so Spotlight indexes every \
             file this run writes there: name it {}",
            root.display(),
            atpkg::build_cache::noindex(&root.display().to_string())
        )
    })
}

/// The checkout as the file system names it: what keys its fallback root.
fn canonical_repo(cfg: &Config) -> PathBuf {
    fs::canonicalize(&cfg.repo).unwrap_or_else(|_| cfg.repo.clone())
}

/// This run's target root: `--target-root` (relative to the checkout) as named, refused
/// up front when `targo trust` would refuse a component of it, which would read as every
/// check failing; else [`choose_root`]'s, the fallback under `cache`.
fn target_root(cfg: &Config, cache: Option<&Path>) -> Result<Root, String> {
    if let Some(named) = &cfg.target_root {
        let path = if named.is_absolute() {
            named.clone()
        } else {
            cfg.repo.join(named)
        };
        if let Some(r) = refused_component(&path) {
            return Err(format!(
                "the target root {} has a path component, {}, that `targo trust` refuses ({}): {}",
                path.display(),
                r.dir.display(),
                r.why,
                r.fix(ELSEWHERE_ROOT)
            ));
        }
        return Ok(Root {
            path,
            mine: None,
            note: None,
        });
    }
    choose_root(
        &cfg.repo,
        &checkout_key(&canonical_repo(cfg)),
        cache,
        &refused_component,
    )
}

/// The directory the fallback roots go under: macOS's `~/Library/Caches/aterm-trust-lane.noindex`;
/// elsewhere `aterm-trust-lane` in `$XDG_CACHE_HOME` when that is absolute (the XDG rule:
/// a relative one is ignored), else in `~/.cache`. `None` with no absolute home to put it in.
fn cache_base(home: Option<&Path>, xdg_cache: Option<&Path>, macos: bool) -> Option<PathBuf> {
    let home = home.filter(|h| h.is_absolute());
    if macos {
        return home.map(|h| h.join(MAC_CACHE_DIR));
    }
    xdg_cache
        .filter(|x| x.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| home.map(|h| h.join(".cache")))
        .map(|c| c.join(CACHE_DIR))
}

/// A checkout's fallback root's name: its directory's name, then the first 12 hex of the
/// SHA-256 of its canonical path. Per worktree, never per clone: two worktrees of one
/// clone each hold their own run lock.
fn checkout_key(repo: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let name: String = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let digest = aterm_digest::Sha256::digest(repo.as_os_str().as_bytes());
    let hex = trust_facts::hex(&digest);
    let name = if name.is_empty() || name.starts_with('.') {
        format!("checkout{name}")
    } else {
        name
    };
    format!("{name}-{}", hex.get(..12).unwrap_or(&hex))
}

/// The default target root: `<repo>/target-trust.noindex` wherever `targo trust` admits it (no
/// change on a checkout writable by its owner alone), else `<cache>/<key>` (`cache` from
/// [`cache_base`], `key` from [`checkout_key`]), said on one line; `refused` answers for a
/// path as [`refused_component`] does.
///
/// # Errors
/// Neither is admitted: each refusal and its fix.
fn choose_root(
    repo: &Path,
    key: &str,
    cache: Option<&Path>,
    refused: &dyn Fn(&Path) -> Option<Refused>,
) -> Result<Root, String> {
    let own = repo.join(OWN_ROOT);
    let mine = cache.map(|c| c.join(key));
    let Some(first) = refused(&own) else {
        return Ok(Root {
            path: own,
            mine,
            note: None,
        });
    };
    let said = |path: &Path, r: &Refused| {
        format!(
            "{} has a path component, {}, that `targo trust` refuses ({})",
            path.display(),
            r.dir.display(),
            r.why
        )
    };
    let Some(path) = mine else {
        return Err(format!(
            "no target root `targo trust` admits: {}: {}; and no home directory holds a \
             per-user one",
            said(&own, &first),
            first.fix(ELSEWHERE_ROOT)
        ));
    };
    if let Some(second) = refused(&path) {
        return Err(format!(
            "no target root `targo trust` admits: {}: {}; {}: {}",
            said(&own, &first),
            first.fix(ELSEWHERE_ROOT),
            said(&path, &second),
            second.fix(ELSEWHERE_ROOT)
        ));
    }
    let to_use = first
        .chmod_command()
        .map(|c| format!("; to use it, run: {c}"))
        .unwrap_or_default();
    Ok(Root {
        note: Some(format!(
            "target root {} ({} is refused: {} {}{to_use})",
            path.display(),
            own.display(),
            first.dir.display(),
            first.why
        )),
        mine: Some(path.clone()),
        path,
    })
}

/// What a fallback root's [`CHECKOUT_RECORD`] holds: the checkout's path, byte for byte.
fn checkout_record(repo: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut b = repo.as_os_str().as_bytes().to_vec();
    b.push(b'\n');
    b
}

/// A real caller-owned cache directory under the existing target-root admission.
/// Unknown metadata and another principal's writable path never admit automatic cleanup.
fn owned_cache_dir(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let me = proc_tree::euid();
    let Ok(md) = fs::symlink_metadata(path) else {
        return false;
    };
    if !md.is_dir() || md.uid() != me || md.mode() & 0o022 != 0 {
        return false;
    }
    // The existing component rule, but a cleanup cannot skip an unreadable chain.
    for logical in path.ancestors() {
        let Ok(physical) = fs::canonicalize(logical) else {
            return false;
        };
        for dir in physical.ancestors() {
            let Ok(md) = fs::metadata(dir) else {
                return false;
            };
            if !md.is_dir()
                || component_refusal(md.uid(), md.mode(), me, || proc_tree::writable(dir)).is_some()
            {
                return false;
            }
        }
    }
    true
}

/// A non-symlink, singly linked authority file writable by its caller owner alone.
fn owned_cache_file(path: &Path) -> Option<fs::Metadata> {
    use std::os::unix::fs::MetadataExt as _;
    let md = fs::symlink_metadata(path).ok()?;
    (md.is_file() && md.uid() == proc_tree::euid() && md.mode() & 0o022 == 0 && md.nlink() == 1)
        .then_some(md)
}

/// Admit an existing fallback lock without following links, or make a new owner-only
/// one exclusively. The automatic root must not reach RunLock's writer on an unknown file.
fn prepare_cache_lock(root: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let path = root.join(".run.lock");
    let refused = || {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unknown fallback cache root, run lock or check record",
        )
    };
    if !root.parent().is_some_and(owned_cache_dir) || !owned_cache_dir(root) {
        return Err(refused());
    }
    // The selected automatic root is reaped after preparation. Refuse unknown
    // stale-check entries before RunLock can write or any PID record is read.
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(".check.")
            && owned_cache_file(&entry.path()).is_none()
        {
            return Err(refused());
        }
    }
    match fs::symlink_metadata(&path) {
        Ok(_) if owned_cache_file(&path).is_some() => Ok(()),
        Ok(_) => Err(refused()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            drop(
                fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(path)?,
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Replace the selected fallback root's checkout marker through exclusive scratch,
/// never by truncating/chmoding the target of an existing symlink or hard link.
fn write_checkout_record(root: &Path, repo: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let marker = root.join(CHECKOUT_RECORD);
    let refused = || {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unknown checkout marker or fallback root",
        )
    };
    if !root.parent().is_some_and(owned_cache_dir)
        || !owned_cache_dir(root)
        || root.file_name() != Some(std::ffi::OsStr::new(&checkout_key(repo)))
    {
        return Err(refused());
    }
    match fs::symlink_metadata(&marker) {
        Ok(md) if md.is_file() && md.uid() == proc_tree::euid() && md.nlink() == 1 => {}
        Ok(_) => return Err(refused()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    // An existing caller-owned regular marker may have a weak old umask. Replace
    // it with a fresh 0644 file; its old contents never authorize this selected run.
    let scratch = aterm_tempfile::Builder::new()
        .prefix(".checkout-")
        .tempdir_in(root)?;
    let at = scratch.path().join("record");
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&at)?;
    file.set_permissions(fs::Permissions::from_mode(0o644))?;
    file.write_all(&checkout_record(repo))?;
    drop(file);
    fs::rename(at, marker)
}

/// The checkout an automatic cleanup may act for. All authority files and the
/// cache/child directory are admitted before a run lock can be opened or a PID read.
fn cleanup_checkout(dir: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    if !owned_cache_dir(dir.parent()?) || !owned_cache_dir(dir) {
        return None;
    }
    owned_cache_file(&dir.join(CHECKOUT_RECORD))?;
    // A real run made this lock. A missing, weak, linked or unknown lock is retained.
    owned_cache_file(&dir.join(".run.lock"))?;
    for entry in fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        if entry.file_name().to_string_lossy().starts_with(".check.") {
            owned_cache_file(&entry.path())?;
        }
    }
    let bytes = fs::read(dir.join(CHECKOUT_RECORD)).ok()?;
    let path = bytes.strip_suffix(b"\n")?;
    let checkout = PathBuf::from(std::ffi::OsStr::from_bytes(path));
    (checkout.is_absolute()
        && dir.file_name() == Some(std::ffi::OsStr::new(&checkout_key(&checkout))))
    .then_some(checkout)
}

/// Retire an admitted fallback root under its own existing run lock. The selected
/// explicit run-root cleanup remains separate. Unknown cache state is left alone.
fn retire(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Some(checkout) = cleanup_checkout(dir) else {
        return false;
    };
    let lock_path = dir.join(".run.lock");
    let Ok(lock) = RunLock::take(&lock_path, "a run holds it") else {
        return false;
    };
    let (Ok(held), Some(named)) = (lock.0.metadata(), owned_cache_file(&lock_path)) else {
        return false;
    };
    if (held.dev(), held.ino()) != (named.dev(), named.ino())
        || cleanup_checkout(dir).as_ref() != Some(&checkout)
    {
        return false;
    }
    reap_stale_checks(dir);
    let gone = fs::remove_dir_all(dir).is_ok();
    drop(lock);
    gone
}

/// Retire only admitted cache children whose matching recorded checkout is known
/// missing. Read/metadata failures, unknown entries and unadmitted caches are preserved.
fn prune_orphans(cache: &Path, own: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut gone = Vec::new();
    if !owned_cache_dir(cache) {
        return gone;
    }
    let Ok(entries) = fs::read_dir(cache) else {
        return gone;
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let dir = entry.path();
        if dir == own {
            continue;
        }
        let Some(checkout) = cleanup_checkout(&dir) else {
            continue;
        };
        if fs::symlink_metadata(&checkout).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            && retire(&dir)
        {
            gone.push((dir, checkout));
        }
    }
    gone
}

/// A path component `targo trust` refuses for a verified runtime-library directory
/// under `root` (one a principal other than the caller could swap): owned by neither
/// root nor the caller, or group- or world-writable. Each logical ancestor of the
/// deepest existing one is checked along its whole physical chain, since targo holds
/// the directory that holds each symlink to the same rule.
pub(crate) fn refused_component(root: &Path) -> Option<Refused> {
    let me = proc_tree::euid();
    let mut p = root.to_path_buf();
    while !p.is_dir() {
        if !p.pop() {
            return None;
        }
    }
    p.ancestors()
        .find_map(|logical| physical_chain_refusal(logical, me))
}

/// The first of `path`'s physical chain (it, then each directory above it) `targo trust`
/// refuses ([`component_refusal`]).
fn physical_chain_refusal(path: &Path, me: u32) -> Option<Refused> {
    let phys = fs::canonicalize(path).ok()?;
    phys.ancestors().find_map(|dir| {
        let md = fs::metadata(dir).ok()?;
        refused_object(dir, &md, me, true)
    })
}

/// [`component_refusal`] of the object at `path`, `acl` asking whether an ACL lets the
/// caller write one another owns (targo asks it of a path component, not of what a
/// runtime-library directory holds).
fn refused_object(path: &Path, md: &fs::Metadata, me: u32, acl: bool) -> Option<Refused> {
    use std::os::unix::fs::MetadataExt as _;
    component_refusal(md.uid(), md.mode(), me, || acl && proc_tree::writable(path)).map(|why| {
        Refused {
            dir: path.to_path_buf(),
            chmod: GO_W.filter(|_| md.uid() == me),
            why,
        }
    })
}

/// Make `dir` and each missing parent as [`refused_component`] admits them: writable by
/// their owner alone (`0755`, less what the caller's umask takes away). The target root and
/// its slots lie above every runtime-library directory a check makes, and a login's umask
/// `002` (Ubuntu's) would make them `0775`, which `targo trust` refuses whatever the group.
pub(crate) fn owner_dirs(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir)
}

/// Why `targo trust` refuses a directory owned by `uid` with `mode`, for the caller
/// `me`: owned by neither root nor the caller, group- or world-writable (a sticky bit
/// exempts nothing), or owned by another and still `writable` by the caller (an ACL).
fn component_refusal(
    uid: u32,
    mode: u32,
    me: u32,
    writable: impl FnOnce() -> bool,
) -> Option<String> {
    let mode = mode & 0o7777;
    if uid != 0 && uid != me {
        return Some(format!(
            "owned by uid={uid}, neither root nor you (uid={me})"
        ));
    }
    if mode & 0o022 != 0 {
        return Some(format!("mode {mode:o}: group- or world-writable"));
    }
    (uid != me && writable())
        .then(|| format!("writable by you through an ACL although uid={uid} owns it"))
}

/// What `targo trust` would refuse to execute or load from the toolchain whose physical
/// `bin/` is `bin`, before it verifies anything (targo's `process_authority`, trustd's
/// executable check):
/// - each launcher (`targo`, the `trustc` targo runs as the compiler, and `trustd`): a
///   plain file held to [`component_refusal`], on a physical chain held to it too; and
///   `targo` and `trustc`, which targo opens as verified tool launchers, with no setuid,
///   setgid or sticky bit ([`special_bits_refusal`]);
/// - each runtime-library directory, as targo lists them from the sysroot (`bin/..`):
///   `lib`, every `lib/rustlib/<target>/lib`, and `<sysroot>-rustc/*/release/deps` beside
///   it; reached through plain directories only, held to the rule along its physical chain,
///   and each entry in it a plain file or directory of root's or the caller's that no group
///   or world can write.
pub(crate) fn toolchain_refusal(bin: &Path) -> Option<Refused> {
    let me = proc_tree::euid();
    for tool in ["targo", "trustc", "trustd"] {
        // `Driver::at` says what is missing.
        let Ok(path) = fs::canonicalize(bin.join(tool)) else {
            continue;
        };
        let Ok(md) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !md.is_file() {
            return Some(Refused {
                dir: path,
                why: "not a plain regular file".into(),
                chmod: None,
            });
        }
        if let Some(r) = refused_object(&path, &md, me, true)
            .or_else(|| special_bits_refusal(&path, &md, me).filter(|_| tool != "trustd"))
            .or_else(|| path.parent().and_then(|p| physical_chain_refusal(p, me)))
        {
            return Some(r);
        }
    }
    let sysroot = bin.parent()?;
    let dirs = match runtime_dirs(sysroot) {
        Ok(dirs) => dirs,
        Err(r) => return Some(r),
    };
    dirs.iter().find_map(|dir| {
        physical_chain_refusal(dir, me).or_else(|| {
            fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .find_map(|e| {
                    let path = e.path();
                    let md = fs::symlink_metadata(&path).ok()?;
                    if !md.is_file() && !md.is_dir() {
                        return Some(Refused {
                            dir: path,
                            why: "a symlink or special file in a runtime-library directory".into(),
                            chmod: None,
                        });
                    }
                    refused_object(&path, &md, me, false)
                })
        })
    })
}

/// A verified tool launcher's mode bits targo refuses besides write (its
/// `launcher_mode_is_safe`): setuid, setgid or sticky.
fn special_bits_refusal(path: &Path, md: &fs::Metadata, me: u32) -> Option<Refused> {
    use std::os::unix::fs::MetadataExt as _;
    let mode = md.mode() & 0o7777;
    (mode & 0o7000 != 0).then(|| Refused {
        dir: path.to_path_buf(),
        why: format!("mode {mode:o}: a launcher with a setuid, setgid or sticky bit"),
        chmod: (md.uid() == me).then_some("a-st"),
    })
}

/// The runtime-library directories targo lists for the toolchain at `sysroot`.
///
/// # Errors
/// A component on the way to one that is there but is no plain directory.
fn runtime_dirs(sysroot: &Path) -> Result<Vec<PathBuf>, Refused> {
    let mut dirs = Vec::new();
    if plain_dirs_to(sysroot, &sysroot.join("lib"))? {
        dirs.push(sysroot.join("lib"));
    }
    let rustlib = sysroot.join("lib/rustlib");
    if plain_dirs_to(sysroot, &rustlib)? {
        for e in fs::read_dir(&rustlib).into_iter().flatten().flatten() {
            if e.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let lib = e.path().join("lib");
            if plain_dirs_to(sysroot, &lib)? {
                dirs.push(lib);
            }
        }
    }
    if let (Some(build), Some(stage)) = (sysroot.parent(), sysroot.file_name()) {
        let mut name = stage.to_os_string();
        name.push("-rustc");
        let deps_root = build.join(name);
        if plain_dirs_to(build, &deps_root)? {
            for e in fs::read_dir(&deps_root).into_iter().flatten().flatten() {
                if !e.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let deps = e.path().join("release/deps");
                if plain_dirs_to(build, &deps)? {
                    dirs.push(deps);
                }
            }
        }
    }
    Ok(dirs)
}

/// Whether `candidate` is there, reached from `root` through plain directories only.
///
/// # Errors
/// A component that is there and is no plain directory (targo forbids a symlink inside a
/// toolchain).
fn plain_dirs_to(root: &Path, candidate: &Path) -> Result<bool, Refused> {
    let Ok(rel) = candidate.strip_prefix(root) else {
        return Ok(false);
    };
    let mut at = root.to_path_buf();
    for c in rel.components() {
        at.push(c);
        match fs::symlink_metadata(&at) {
            Ok(md) if md.is_dir() => {}
            Ok(_) => {
                return Err(Refused {
                    dir: at,
                    why: "not a plain directory (targo forbids a symlink inside a toolchain)"
                        .into(),
                    chmod: None,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => {
                return Err(Refused {
                    dir: at,
                    why: format!("cannot be inspected: {e}"),
                    chmod: None,
                });
            }
        }
    }
    Ok(true)
}

/// One run per target root: a run empties the root's slots and ends what a killed run left
/// there, so a second run on the same root would wreck the first.
pub(crate) struct RunLock(fs::File);

impl RunLock {
    /// Take the lock at `path`, or say who holds it and `busy` (what to do).
    pub(crate) fn take(path: &Path, busy: &str) -> Result<RunLock, String> {
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "released by RunLock's Drop through File::unlock before the close"
        )]
        let taken = f.try_lock();
        match taken {
            Ok(()) => {
                let _ = f.set_len(0);
                let _ = (&f).write_all(format!("{}\n", std::process::id()).as_bytes());
                Ok(RunLock(f))
            }
            Err(fs::TryLockError::WouldBlock) => Err(format!(
                "run {} holds {}: {busy}",
                fs::read_to_string(path).unwrap_or_default().trim(),
                path.display()
            )),
            Err(fs::TryLockError::Error(e)) => Err(format!("cannot lock {}: {e}", path.display())),
        }
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

// The real checks

struct RealChecks<'a> {
    driver: &'a Driver,
    repo: &'a Path,
    root: &'a Path,
    env: Vec<(String, String)>,
    keeper: &'a trustd::Keeper,
    sock: &'a Path,
    want: &'a str,
    t0: Instant,
    /// Each fork lockfile a check used, and whether this run created it: what
    /// [`record`] did not remove (a check an interrupt ended) goes after the slots.
    locks: BTreeMap<PathBuf, bool>,
}

impl Checks for RealChecks<'_> {
    fn start(&mut self, job: &mut Job) -> std::io::Result<Box<dyn Check>> {
        let target = self.root.join(format!("j{}", job.slot));
        owner_dirs(&target)?;
        job.target.clone_from(&target);
        let log = fs::File::create(&job.log)?;
        job.lock = fork_lock(self.repo, &job.argv);
        if let Some(l) = &job.lock {
            self.locks.entry(l.path.clone()).or_insert(l.created);
        }
        let mut cmd = command(&self.driver.targo);
        cmd.args(["trust", "check"])
            .args(&job.argv)
            .arg("--allow-l0-gaps");
        if let Some(r) = &job.report {
            cmd.arg("--report-dir").arg(r);
        }
        cmd.env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .env("CARGO_TARGET_DIR", &target)
            .current_dir(self.repo)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        proc_tree::new_session(&mut cmd);
        let child = cmd.spawn()?;
        let pid = child.id();
        let record = self.root.join(format!(".check.{}", job.slot));
        let _ = fs::write(
            &record,
            format!("{pid} {}\n", proc_tree::started(pid).unwrap_or_default()),
        );
        Ok(Box::new(RealCheck {
            child,
            pid,
            record,
            ended: None,
        }))
    }

    fn trustd_held(&self) -> bool {
        self.keeper.held()
    }

    fn probe_trustd(&mut self) -> String {
        trustd::probe(self.sock, self.want, Duration::from_secs(10)).line()
    }

    fn interrupted(&self) -> bool {
        crate::interrupt::caught().is_some()
    }

    fn now(&self) -> Duration {
        self.t0.elapsed()
    }

    fn pause(&mut self) {
        std::thread::sleep(POLL);
    }
}

struct RealCheck {
    child: aterm_uds::spawnfd::Child,
    pid: u32,
    record: PathBuf,
    ended: Option<Ended>,
}

impl RealCheck {
    fn reaped(&mut self, usage: aterm_uds::spawnfd::Rusage) -> Ended {
        use std::os::unix::process::ExitStatusExt as _;
        let e = Ended {
            code: usage
                .status
                .code()
                .unwrap_or_else(|| 128 + usage.status.signal().unwrap_or(0)),
            cpu: usage.cpu,
            maxrss: usage.maxrss,
        };
        let _ = fs::remove_file(&self.record);
        self.ended = Some(e);
        e
    }
}

impl Check for RealCheck {
    fn poll(&mut self) -> std::io::Result<Option<Ended>> {
        if let Some(e) = self.ended {
            return Ok(Some(e));
        }
        Ok(self
            .child
            .try_wait_with_rusage()?
            .map(|usage| self.reaped(usage)))
    }

    fn end(&mut self) -> Ended {
        if let Some(e) = self.ended {
            return e;
        }
        let pid = self.pid;
        let mut got = None;
        {
            let mut gone = || {
                if got.is_none() {
                    match self.child.try_wait_with_rusage() {
                        Ok(Some(usage)) => got = Some(usage),
                        Ok(None) => return false,
                        Err(_) => return true,
                    }
                }
                true
            };
            proc_tree::end_tree(pid, &mut gone);
        }
        if let Some(usage) = got.or_else(|| self.child.wait_with_rusage().ok()) {
            self.reaped(usage)
        } else {
            let e = Ended {
                code: 128 + proc_tree::SIGKILL,
                cpu: Duration::ZERO,
                maxrss: 0,
            };
            self.ended = Some(e);
            let _ = fs::remove_file(&self.record);
            e
        }
    }
}

// Process trees

pub mod proc_tree {
    //! A check's process tree: listed by parent pid (targo-trust starts its build in a
    //! group of its own, so a group signal misses it), trustd and what it runs left
    //! out (every run on the machine shares it, and a signalled one records DIRTY).

    use std::collections::BTreeSet;
    use std::time::Duration;

    pub const SIGKILL: i32 = 9;
    const SIGTERM: i32 = 15;
    #[cfg(not(target_os = "linux"))]
    const SIGSTOP: i32 = 17;
    #[cfg(not(target_os = "linux"))]
    const SIGCONT: i32 = 19;
    #[cfg(target_os = "linux")]
    const SIGSTOP: i32 = 19;
    #[cfg(target_os = "linux")]
    const SIGCONT: i32 = 18;

    unsafe extern "C" {
        fn access(path: *const std::ffi::c_char, mode: i32) -> i32;
        fn kill(pid: i32, sig: i32) -> i32;
        fn geteuid() -> u32;
    }

    /// This process's effective uid.
    #[must_use]
    pub fn euid() -> u32 {
        // SAFETY: `geteuid` takes nothing and cannot fail.
        unsafe { geteuid() }
    }

    /// Whether this process may write in `dir` (`access(2)`, so an ACL counts).
    #[must_use]
    pub fn writable(dir: &std::path::Path) -> bool {
        use std::os::unix::ffi::OsStrExt as _;
        const W_OK: i32 = 2;
        let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `c` is a live NUL-terminated string `access` only reads.
        unsafe { access(c.as_ptr(), W_OK) == 0 }
    }

    /// Start `cmd` as the leader of a session of its own: no controlling terminal to
    /// stop it on, and a group no signal to the lane's reaches.
    pub fn new_session(cmd: &mut aterm_uds::spawnfd::Command) {
        cmd.setsid(true);
    }

    /// A pid's start time (`ps -o lstart=`), `None` when no such process lives: a pid
    /// the kernel reused is not the process recorded.
    #[must_use]
    pub fn started(pid: u32) -> Option<String> {
        let out = aterm_uds::spawnfd::command("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        (out.status.success() && !s.is_empty()).then_some(s)
    }

    fn listing() -> Option<String> {
        aterm_uds::spawnfd::command("/bin/ps")
            .args(["-A", "-o", "pid=,ppid=,comm="])
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    }

    /// `root` and every descendant of it in a `ps -A -o pid=,ppid=,comm=` listing, but a
    /// trustd (its command's last path component) and everything beneath one.
    #[must_use]
    pub fn descendants(listing: &str, root: i32) -> Vec<i32> {
        let rows: Vec<(i32, i32, bool)> = listing
            .lines()
            .filter_map(|l| {
                let mut f = l.split_whitespace();
                let pid = f.next()?.parse().ok()?;
                let ppid = f.next()?.parse().ok()?;
                let comm = f.collect::<Vec<_>>().join(" ");
                Some((pid, ppid, comm.rsplit('/').next() == Some("trustd")))
            })
            .collect();
        let mut out = vec![root];
        let mut seen: BTreeSet<i32> = BTreeSet::from([root]);
        let mut i = 0;
        while i < out.len() {
            let parent = out[i];
            for (pid, ppid, trustd) in &rows {
                if *ppid == parent && !trustd && seen.insert(*pid) {
                    out.push(*pid);
                }
            }
            i += 1;
        }
        out
    }

    fn send(pids: &[i32], sig: i32) {
        for &p in pids {
            // SAFETY: `kill` takes two integers and touches no memory of ours.
            unsafe {
                kill(p, sig);
            }
        }
    }

    /// Ends `root`'s whole tree, trustd's left out: stopped (listed again once stopped, so
    /// nothing forked meanwhile is missed), sent SIGTERM and SIGCONT, given a second to go
    /// (`gone` may reap `root`), then SIGKILLed. Once reaped, the root is never signalled
    /// again: its pid may already name another process.
    pub fn end_tree(root: u32, gone: &mut dyn FnMut() -> bool) {
        end_tree_with(root, gone, &mut listing, &mut send);
    }

    pub(super) fn end_tree_with(
        root: u32,
        gone: &mut dyn FnMut() -> bool,
        listing: &mut dyn FnMut() -> Option<String>,
        send: &mut dyn FnMut(&[i32], i32),
    ) {
        let Ok(root) = i32::try_from(root) else {
            return;
        };
        let first = listing().map_or_else(|| vec![root], |l| descendants(&l, root));
        send(&first, SIGSTOP);
        let mut all: Vec<i32> = listing().map_or_else(Vec::new, |l| descendants(&l, root));
        for p in first {
            if !all.contains(&p) {
                all.push(p);
            }
        }
        send(&all, SIGSTOP);
        send(&all, SIGTERM);
        send(&all, SIGCONT);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let mut root_gone = false;
        loop {
            if gone() {
                root_gone = true;
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if root_gone {
            all.retain(|pid| *pid != root);
        }
        send(&all, SIGKILL);
    }
}

// trustd, Trust's per-user memory jobserver

pub mod trustd {
    //! targo starts a check only beside the trustd packaged with it: it asks the daemon
    //! on the socket for its IDENTITY (the SHA-256 of its executable), gives it half a
    //! second, and refuses the check when the answer is another build's or none came.
    //! The lane asks the same questions ([`probe`]) and holds a one-byte grant on this
    //! build's trustd for the run ([`Keeper`]). Nothing here starts or signals a trustd.

    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// targo's own deadline over connect, IDENTITY and STATUS.
    const TARGO_GIVES: Duration = Duration::from_millis(500);

    /// Trust's per-user socket.
    #[must_use]
    pub fn socket() -> PathBuf {
        let tmp = std::fs::canonicalize("/tmp").unwrap_or_else(|_| PathBuf::from("/tmp"));
        tmp.join(format!("trustd-runtime-locks-{}", super::proc_tree::euid()))
            .join("trust-memory-jobserver.sock")
    }

    /// The build identity a trustd answers IDENTITY with: its executable's SHA-256.
    #[must_use]
    pub fn build_digest(exe: &Path) -> String {
        std::fs::read(exe)
            .map(|b| crate::trust_facts::hex(&aterm_digest::Sha256::digest(&b)))
            .unwrap_or_default()
    }

    /// What serves the socket.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Held {
        /// Nothing: targo starts this build's.
        None,
        /// This build's, answering within targo's half second.
        Ours,
        /// Another build's, or one that answers no IDENTITY.
        Foreign,
        /// One that does not answer in time.
        Silent,
    }

    /// A probe of the socket.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Probe {
        pub held: Held,
        pub what: String,
    }

    impl Probe {
        /// `<none|ours|foreign|silent>\t<what>`, as a check's `.trustd` mark reads.
        #[must_use]
        pub fn line(&self) -> String {
            let word = match self.held {
                Held::None => "none",
                Held::Ours => "ours",
                Held::Foreign => "foreign",
                Held::Silent => "silent",
            };
            format!("{word}\t{}", crate::trust_facts::one_line(&self.what))
        }
    }

    fn ask(s: &mut BufReader<UnixStream>, req: &str) -> std::io::Result<String> {
        s.get_mut().write_all(format!("{req}\n").as_bytes())?;
        let mut line = String::new();
        if s.read_line(&mut line)? == 0 {
            return Err(std::io::Error::other("it closed the connection"));
        }
        Ok(line.trim().to_string())
    }

    /// Who a trustd says it is: `Ours` when its IDENTITY names `want`.
    fn identify(s: &mut BufReader<UnixStream>, sock: &Path, want: &str) -> Probe {
        let silent = |why: String| Probe {
            held: Held::Silent,
            what: format!("the trustd at {} {why}", sock.display()),
        };
        let reply = match ask(s, "IDENTITY") {
            Ok(r) => r,
            Err(e) => return silent(format!("did not answer IDENTITY ({e})")),
        };
        if reply == "ERR busy" {
            return silent("answered ERR busy, every connection taken".into());
        }
        let ident: Option<aterm_json::Value> = aterm_json::from_str(&reply).ok();
        let field = |k: &str| {
            ident
                .as_ref()
                .and_then(|v| v.get(k))
                .and_then(aterm_json::Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let sha = field("executable_sha256");
        let build = format!("trustd {} {}", field("release"), field("commit"));
        if !sha.is_empty() && sha == want {
            return Probe {
                held: Held::Ours,
                what: format!("this build's {build} serves {}", sock.display()),
            };
        }
        Probe {
            held: Held::Foreign,
            what: format!(
                "a trustd of another build serves {} ({build}, sha256 {}; this run's is {})",
                sock.display(),
                sha.get(..12).unwrap_or("none"),
                want.get(..12).unwrap_or("none")
            ),
        }
    }

    fn probe_once(sock: &Path, want: &str, wait: Duration) -> (Probe, Duration) {
        if !sock.exists() {
            let what = format!("nothing serves {}", sock.display());
            return (
                Probe {
                    held: Held::None,
                    what,
                },
                Duration::ZERO,
            );
        }
        let t0 = Instant::now();
        let stream = match UnixStream::connect(sock) {
            Ok(s) => s,
            Err(e) => {
                let what = format!("nothing serves {} ({e})", sock.display());
                return (
                    Probe {
                        held: Held::None,
                        what,
                    },
                    Duration::ZERO,
                );
            }
        };
        let _ = stream.set_read_timeout(Some(wait));
        let _ = stream.set_write_timeout(Some(wait));
        let mut s = BufReader::new(stream);
        let mut p = identify(&mut s, sock, want);
        if p.held == Held::Ours
            && let Err(e) = ask(&mut s, "STATUS")
        {
            p = Probe {
                held: Held::Silent,
                what: format!(
                    "the trustd at {} did not answer STATUS ({e})",
                    sock.display()
                ),
            };
        }
        (p, t0.elapsed())
    }

    /// What serves `sock`, each answer given `wait`. This build's trustd slower than
    /// targo's half second three times running is [`Held::Silent`]: targo would refuse
    /// every check beside it.
    #[must_use]
    pub fn probe(sock: &Path, want: &str, wait: Duration) -> Probe {
        let mut took = Duration::ZERO;
        for _ in 0..3 {
            let (p, t) = probe_once(sock, want, wait);
            if p.held != Held::Ours || t <= TARGO_GIVES {
                return p;
            }
            took = t;
        }
        Probe {
            held: Held::Silent,
            what: format!(
                "the trustd at {} took {:.1} s to answer, where targo gives it 0.5 s",
                sock.display(),
                took.as_secs_f64()
            ),
        }
    }

    /// Before any check: wait up to `wait` for the socket to hold this build's trustd
    /// or nothing.
    ///
    /// # Errors
    /// What held it through the wait, and what to do.
    pub fn wait_usable(
        sock: &Path,
        want: &str,
        wait: Duration,
        out: &mut dyn std::io::Write,
    ) -> Result<(), String> {
        let until = Instant::now() + wait;
        let mut said = false;
        loop {
            let p = probe(sock, want, Duration::from_secs(10));
            if matches!(p.held, Held::None | Held::Ours) {
                return Ok(());
            }
            if !said {
                let _ = writeln!(
                    out,
                    "trust: waiting up to {} s for this build's trustd or none: {}",
                    wait.as_secs(),
                    p.what
                );
                said = true;
            }
            let never = "never signal it: a killed trustd records DIRTY and blocks every check \
                         until `--recover-after-crash` attests no solver survives";
            if Instant::now() >= until || crate::interrupt::caught().is_some() {
                let remedy = if p.held == Held::Foreign {
                    format!("re-run once it retires (300 s after its last grant); {never}")
                } else {
                    format!("re-run when the machine is quieter; {never}")
                };
                return Err(format!(
                    "{}, and it held the socket through the {} s the run waits; nothing was \
                     verified; {remedy}",
                    p.what,
                    wait.as_secs()
                ));
            }
            std::thread::sleep(
                until
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(5)),
            );
        }
    }

    /// While it lives, a one-byte grant on this build's trustd, so trustd does not go idle
    /// while a check compiles; [`Keeper::held`] says the grant is live. It writes how many
    /// grants it took to `grants` and what held the socket instead to `seen`. It never
    /// starts or signals a trustd. Dropping it requests shutdown, closes the registered
    /// connection and joins the keeper thread.
    pub struct Keeper {
        stop: Arc<AtomicBool>,
        held: Arc<AtomicBool>,
        conn: Arc<Mutex<Option<UnixStream>>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Keeper {
        #[must_use]
        pub fn start(sock: PathBuf, want: String, grants: PathBuf, seen: PathBuf) -> Keeper {
            let _ = std::fs::remove_file(&grants);
            let _ = std::fs::remove_file(&seen);
            let stop = Arc::new(AtomicBool::new(false));
            let held = Arc::new(AtomicBool::new(false));
            let conn = Arc::new(Mutex::new(None));
            let k = Kept {
                stop: Arc::clone(&stop),
                held: Arc::clone(&held),
                conn: Arc::clone(&conn),
            };
            let thread = std::thread::spawn(move || keep(&sock, &want, &grants, &seen, &k));
            Keeper {
                stop,
                held,
                conn,
                thread: Some(thread),
            }
        }

        #[must_use]
        pub fn held(&self) -> bool {
            self.held.load(Ordering::SeqCst)
        }
    }

    impl Drop for Keeper {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(c) = self
                .conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = c.shutdown(std::net::Shutdown::Both);
            }
            if let Some(t) = self.thread.take() {
                t.thread().unpark();
                let _ = t.join();
            }
        }
    }

    /// The keeper thread's share of its [`Keeper`].
    struct Kept {
        stop: Arc<AtomicBool>,
        held: Arc<AtomicBool>,
        /// The connection it waits on, for the drop to shut.
        conn: Arc<Mutex<Option<UnixStream>>>,
    }

    impl Kept {
        /// Register the cloned connection before waiting on it. A failed clone
        /// or a stopping keeper refuses the wait: drop must be able to shut it.
        fn name(&self, stream: std::io::Result<UnixStream>) -> bool {
            let Ok(stream) = stream else { return false };
            let mut c = self
                .conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.stop.load(Ordering::SeqCst) {
                return false;
            }
            *c = Some(stream);
            true
        }
    }

    fn keep(sock: &Path, want: &str, grants_out: &Path, seen_out: &Path, k: &Kept) {
        let (stop, held) = (&*k.stop, &*k.held);
        let mut grants = 0u32;
        let tick = Duration::from_secs(2);
        while !stop.load(Ordering::SeqCst) {
            if let Ok(stream) = UnixStream::connect(sock) {
                if !k.name(stream.try_clone()) {
                    break;
                }
                let _ = stream.set_read_timeout(Some(Duration::from_secs(130)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
                let mut s = BufReader::new(stream);
                let p = identify(&mut s, sock, want);
                if p.held == Held::Ours {
                    let req = format!(
                        "RESERVE 1 {} trust lane holds trustd for its run",
                        std::process::id()
                    );
                    if ask(&mut s, &req).is_ok_and(|r| r.starts_with("GRANTED ")) {
                        grants += 1;
                        let _ = std::fs::write(grants_out, format!("{grants}\n"));
                        held.store(true, Ordering::SeqCst);
                        let _ = s.get_ref().set_read_timeout(Some(tick));
                        let mut buf = [0u8; 256];
                        while !stop.load(Ordering::SeqCst) {
                            match s.get_mut().read(&mut buf) {
                                Ok(0) => break,
                                Ok(_) => {}
                                Err(e)
                                    if matches!(
                                        e.kind(),
                                        std::io::ErrorKind::WouldBlock
                                            | std::io::ErrorKind::TimedOut
                                    ) => {}
                                Err(_) => break,
                            }
                        }
                        held.store(false, Ordering::SeqCst);
                    }
                } else {
                    let _ = std::fs::write(seen_out, format!("{}\n", p.line()));
                }
            }
            if !stop.load(Ordering::SeqCst) {
                std::thread::park_timeout(tick);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_failed_connection_clone_refuses_an_uncancellable_wait() {
            let k = Kept {
                stop: Arc::new(AtomicBool::new(false)),
                held: Arc::new(AtomicBool::new(false)),
                conn: Arc::new(Mutex::new(None)),
            };
            assert!(!k.name(Err(std::io::Error::other("injected clone failure"))));
            assert!(k.conn.lock().unwrap().is_none());

            let (stream, _peer) = UnixStream::pair().unwrap();
            assert!(k.name(stream.try_clone()));
            assert!(k.conn.lock().unwrap().is_some());

            k.stop.store(true, Ordering::SeqCst);
            k.conn.lock().unwrap().take();
            assert!(!k.name(Ok(stream)));
            assert!(k.conn.lock().unwrap().is_none());
        }

        #[test]
        fn dropping_before_the_worker_starts_sends_no_request() {
            let d = aterm_tempfile::Builder::new()
                .prefix("tl-start-")
                .tempdir_in("/tmp")
                .unwrap();
            let sock = d.path().join("k.sock");
            let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
            listener.set_nonblocking(true).unwrap();
            let k = Kept {
                stop: Arc::new(AtomicBool::new(false)),
                held: Arc::new(AtomicBool::new(false)),
                conn: Arc::new(Mutex::new(None)),
            };
            let stop = Arc::clone(&k.stop);
            let (start_tx, start_rx) = std::sync::mpsc::channel();
            let mut keeper = Keeper {
                stop: Arc::clone(&k.stop),
                held: Arc::clone(&k.held),
                conn: Arc::clone(&k.conn),
                thread: None,
            };
            keeper.thread = Some(std::thread::spawn(move || {
                start_rx
                    .recv_timeout(Duration::from_secs(60))
                    .expect("the worker was never released");
                keep(
                    &sock,
                    "aa",
                    &d.path().join("grants"),
                    &d.path().join("seen"),
                    &k,
                );
            }));
            let (ended_tx, ended_rx) = std::sync::mpsc::channel();
            let ending = std::thread::spawn(move || {
                drop(keeper);
                ended_tx.send(()).unwrap();
            });
            let deadline = Instant::now() + Duration::from_secs(60);
            while !stop.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "drop never stopped the worker");
                std::thread::yield_now();
            }
            start_tx.send(()).unwrap();
            ended_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("the stopped worker never ended");
            ending.join().unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "a worker stopped before startup must not connect"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn tmp(prefix: &str) -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .unwrap()
    }

    /// A resolved graph: two members (one bin-only), a fork with resolved features,
    /// a registry package, all under `/ws`.
    fn meta() -> aterm_json::Value {
        aterm_json::from_str(
            r#"{
              "workspace_root": "/ws",
              "workspace_members": ["alpha 1.0.0 (path+file:///ws/crates/alpha)", "tool 1.0.0 (path+file:///ws/crates/tool)"],
              "packages": [
                {"id": "tool 1.0.0 (path+file:///ws/crates/tool)", "name": "tool", "version": "1.0.0", "source": null,
                 "manifest_path": "/ws/crates/tool/Cargo.toml", "targets": [{"kind": ["bin"]}]},
                {"id": "alpha 1.0.0 (path+file:///ws/crates/alpha)", "name": "alpha", "version": "1.0.0", "source": null,
                 "manifest_path": "/ws/crates/alpha/Cargo.toml", "targets": [{"kind": ["lib"]}, {"kind": ["test"]}]},
                {"id": "forky 0.3.1 (path+file:///ws/vendor/forky)", "name": "forky", "version": "0.3.1", "source": null,
                 "manifest_path": "/ws/vendor/forky/Cargo.toml", "targets": [{"kind": ["lib"]}]},
                {"id": "serde 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)", "name": "serde",
                 "version": "1.0.0", "source": "registry+https://github.com/rust-lang/crates.io-index",
                 "manifest_path": "/reg/serde/Cargo.toml", "targets": [{"kind": ["lib"]}]}
              ],
              "resolve": {"nodes": [{"id": "forky 0.3.1 (path+file:///ws/vendor/forky)", "features": ["std", "alloc"]}]}
            }"#,
        )
        .unwrap()
    }

    fn rows_none(_: &Path) -> Result<BTreeSet<String>, String> {
        Ok(BTreeSet::new())
    }

    /// A member by `-p name@version --lib`, a bin-only member with nothing to check, a fork
    /// by its own manifest with its resolved features, registry packages never; members
    /// before forks; the lane profile first.
    #[test]
    fn the_gate_list_is_derived_from_the_resolved_graph() {
        let libs = gate_list(&meta(), &rows_none).unwrap();
        let names: Vec<(&str, Kind)> = libs.iter().map(|l| (l.name.as_str(), l.kind)).collect();
        assert_eq!(
            names,
            [
                ("alpha", Kind::Member),
                ("tool", Kind::MemberBin),
                ("forky", Kind::Fork)
            ]
        );
        let profile = lane_profile_args(&BTreeSet::new());
        let mut alpha = profile.clone();
        alpha.extend(["-p", "alpha@1.0.0", "--lib"].map(String::from));
        assert_eq!(libs[0].argv, alpha);
        assert!(libs[1].argv.is_empty());
        let mut forky = profile;
        forky.extend(
            [
                "--manifest-path",
                "vendor/forky/Cargo.toml",
                "-p",
                "forky@0.3.1",
                "--no-default-features",
                "--features",
                "alloc,std",
                "--lib",
            ]
            .map(String::from),
        );
        assert_eq!(libs[2].argv, forky);
        assert_eq!(manifest_of(&libs[2].argv), Some("vendor/forky/Cargo.toml"));
    }

    /// A member and a path fork of one name each have their own floor row; the package's name
    /// selects both, a spec selects one, and neither is AMBIGUOUS.
    #[test]
    fn two_versions_of_one_name_are_named_by_their_specs() {
        let mut meta = meta();
        let twin = aterm_json::from_str(
            r#"{"id": "alpha 2.0.0 (path+file:///ws/crates/alpha-2)", "name": "alpha", "version": "2.0.0",
                "source": null, "manifest_path": "/ws/crates/alpha-2/Cargo.toml", "targets": [{"kind": ["lib"]}]}"#,
        )
        .unwrap();
        meta.as_object_mut()
            .and_then(|m| m.get_mut("packages"))
            .and_then(aterm_json::Value::as_array_mut)
            .expect("the fixture lists packages")
            .push(twin);
        let libs = gate_list(&meta, &rows_none).unwrap();
        let names: Vec<(&str, Kind)> = libs.iter().map(|l| (l.name.as_str(), l.kind)).collect();
        assert_eq!(
            names,
            [
                ("alpha@1.0.0", Kind::Member),
                ("tool", Kind::MemberBin),
                ("alpha@2.0.0", Kind::Fork),
                ("forky", Kind::Fork)
            ]
        );
        assert_eq!(package_identity("alpha@2.0.0"), "alpha");
        let both = select(&libs, Some(&["alpha".to_string()])).unwrap();
        assert_eq!(both.len(), 2);
        assert!(both.iter().all(|s| matches!(s.decision, Decision::Gate(_))));
        let one = select(&libs, Some(&["alpha@2.0.0".to_string()])).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].lib.spec, "alpha@2.0.0");
    }

    /// A named row outranks `package."*"`; with none, only the profile itself.
    #[test]
    fn every_dev_speed_row_is_zeroed_by_name() {
        let rows: BTreeSet<String> = ["aterm-census".to_string(), "ttf-parser".to_string()].into();
        let with = gate_list(&meta(), &|_| Ok(rows.clone())).unwrap();
        let zeroed: Vec<&String> = with[0]
            .argv
            .iter()
            .filter(|a| a.contains(".package."))
            .collect();
        assert_eq!(
            zeroed,
            [
                "profile.trust-lane.package.\"aterm-census\".opt-level=0",
                "profile.trust-lane.package.\"ttf-parser\".opt-level=0"
            ]
        );
        assert!(with[0].argv.starts_with(&[
            "--profile".into(),
            "trust-lane".into(),
            "--config".into(),
            "profile.trust-lane.inherits=\"dev\"".into(),
            "--config".into(),
            "profile.trust-lane.opt-level=0".into()
        ]));
        let without = gate_list(&meta(), &rows_none).unwrap();
        assert!(!without[0].argv.iter().any(|a| a.contains(".package.")));
    }

    #[test]
    fn dev_package_rows_are_read_from_the_manifest_and_its_config() {
        let d = tmp("trust-lane-rows");
        let manifest = d.path().join("Cargo.toml");
        fs::write(
            &manifest,
            "[profile.dev.package.aterm-png]\nopt-level = 2\n[profile.release]\nlto = true\n",
        )
        .unwrap();
        fs::create_dir_all(d.path().join(".cargo")).unwrap();
        fs::write(
            d.path().join(".cargo/config.toml"),
            "[profile.dev.package.\"*\"]\nopt-level = 1\n",
        )
        .unwrap();
        let rows = profile_rows_for(&manifest, d.path()).unwrap();
        assert_eq!(
            rows,
            BTreeSet::from(["*".to_string(), "aterm-png".to_string()])
        );
        fs::write(&manifest, "[profile.dev.package.x\n").unwrap();
        assert!(
            profile_rows_for(&manifest, d.path()).is_err(),
            "unread is never no rows"
        );
    }

    /// `sealed` names the fork `bus`, and `framed` the fork `wire` through its renamed,
    /// optional dependency key; a feature the default build turns on (`std`), one that names
    /// no package (`dev-seams`) and one that names a registry package (`derive`) get no row.
    /// `-p link` selects every row of link's library, `-p link+sealed` that row alone, and a
    /// floor row whose feature left the graph is FORK-VANISHED.
    #[test]
    fn a_member_feature_that_turns_on_fork_code_is_gated_as_a_row_of_its_own() {
        let meta: aterm_json::Value = aterm_json::from_str(
            r#"{
              "workspace_root": "/ws",
              "workspace_members": ["link-id"],
              "packages": [
                {"id": "link-id", "name": "link", "version": "1.0.0", "source": null,
                 "manifest_path": "/ws/crates/link/Cargo.toml", "targets": [{"kind": ["lib"]}],
                 "dependencies": [{"name": "bus"}, {"name": "wire", "rename": "the-wire"},
                                  {"name": "serde"}],
                 "features": {"default": ["std"], "std": ["bus/std"], "sealed": ["bus/aead"],
                              "framed": ["the-wire?/framed"], "dev-seams": [],
                              "derive": ["serde/derive"]}},
                {"id": "bus-id", "name": "bus", "version": "0.1.0", "source": null,
                 "manifest_path": "/ws/vendor/bus/Cargo.toml", "targets": [{"kind": ["lib"]}]},
                {"id": "wire-id", "name": "wire", "version": "0.1.0", "source": null,
                 "manifest_path": "/ws/vendor/wire/Cargo.toml", "targets": [{"kind": ["lib"]}]},
                {"id": "serde-id", "name": "serde", "version": "1.0.0",
                 "source": "registry+https://github.com/rust-lang/crates.io-index",
                 "manifest_path": "/reg/serde/Cargo.toml", "targets": [{"kind": ["lib"]}]}
              ],
              "resolve": {"nodes": []}
            }"#,
        )
        .unwrap();
        let libs = gate_list(&meta, &rows_none).unwrap();
        let members: Vec<(&str, &str)> = libs
            .iter()
            .filter(|l| l.kind == Kind::Member)
            .map(|l| (l.name.as_str(), l.spec.as_str()))
            .collect();
        assert_eq!(
            members,
            [
                ("link", "link@1.0.0"),
                ("link+framed", "link+framed@1.0.0"),
                ("link+sealed", "link+sealed@1.0.0")
            ]
        );
        let mut sealed = lane_profile_args(&BTreeSet::new());
        sealed.extend(["-p", "link@1.0.0", "--lib", "--features", "sealed"].map(String::from));
        let row = libs.iter().find(|l| l.name == "link+sealed").unwrap();
        assert_eq!(row.argv, sealed);
        let picked = |want: &str| -> Vec<String> {
            select(&libs, Some(&[want.to_string()]))
                .unwrap()
                .into_iter()
                .map(|s| {
                    assert!(matches!(s.decision, Decision::Gate(_)), "{s:?}");
                    s.lib.name
                })
                .collect()
        };
        assert_eq!(picked("link"), ["link", "link+framed", "link+sealed"]);
        assert_eq!(picked("link+sealed"), ["link+sealed"]);
        let without: Vec<Lib> = libs
            .iter()
            .filter(|l| l.name != "link+sealed")
            .cloned()
            .collect();
        let floors = "link\t5\t9\tp\nlink+sealed\t5\t9\tp\n";
        let gone = |libs: &[Lib]| vanished(libs, floors).contains(&"link+sealed".to_string());
        assert!(!gone(&libs));
        assert!(gone(&without));
    }

    #[test]
    fn metadata_without_a_graph_is_refused() {
        let empty: aterm_json::Value = aterm_json::from_str(r#"{"packages": []}"#).unwrap();
        assert!(gate_list(&empty, &rows_none).is_err());
        let mut m = meta();
        if let aterm_json::Value::Object(o) = &mut m {
            o.remove("workspace_members");
        }
        assert!(gate_list(&m, &rows_none).is_err());
    }

    fn lib(name: &str, spec: &str, kind: Kind) -> Lib {
        Lib {
            name: name.into(),
            kind,
            spec: spec.into(),
            argv: vec!["-p".into(), spec.into()],
        }
    }

    #[test]
    fn the_selection_decides_each_library_before_anything_runs() {
        let libs = vec![
            lib("alpha", "alpha@1.0.0", Kind::Member),
            lib("tool", "tool@1.0.0", Kind::MemberBin),
            lib("smol_str", "smol_str@0.3.2", Kind::Fork),
        ];
        let all = select(&libs, None).unwrap();
        let decided: Vec<(&str, &Decision)> = all
            .iter()
            .map(|s| (s.lib.name.as_str(), &s.decision))
            .collect();
        assert_eq!(
            decided,
            [
                ("alpha", &Decision::Gate(trust_facts::Measured::default())),
                ("tool", &Decision::NoLib),
                (
                    "smol_str",
                    &Decision::Gate(trust_facts::Measured::default())
                ),
            ]
        );
        let one = select(&libs, Some(&["smol_str".to_string()])).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].lib.spec, "smol_str@0.3.2");
        assert!(
            select(&libs, Some(&["nope".to_string()])).is_err(),
            "a name that matches nothing is no run over zero libraries"
        );
    }

    /// A row the graph carries, and a comment, do not count.
    #[test]
    fn a_floor_the_graph_no_longer_carries_is_vanished() {
        let libs = vec![
            lib("alpha", "alpha@1.0.0", Kind::Member),
            lib("smol_str", "smol_str@0.3.2", Kind::Fork),
        ];
        let floors = "# <crate>\t<proved>\nalpha\t1\t2\tp\nsmol_str\t3\t4\tp\n";
        assert_eq!(vanished(&libs, floors), Vec::<String>::new());
        let floors = format!("{floors}pkg-config\t5\t6\tp\n");
        assert_eq!(vanished(&libs, &floors), ["pkg-config"]);
        assert_eq!(
            vanished(&libs[..1], &floors),
            ["pkg-config", "smol_str"],
            "a fork that left is named with its floor"
        );
    }

    #[test]
    fn two_selected_packages_of_one_name_are_both_ambiguous() {
        let libs = vec![
            lib("arrayvec", "arrayvec@0.7.8", Kind::Member),
            lib("arrayvec", "arrayvec@0.7.7", Kind::Fork),
            lib("beta", "beta@1.0.0", Kind::Member),
        ];
        let all = select(&libs, None).unwrap();
        assert_eq!(all[0].decision, Decision::Ambiguous);
        assert_eq!(all[1].decision, Decision::Ambiguous);
        assert!(matches!(all[2].decision, Decision::Gate(_)));
    }

    #[test]
    fn the_cost_history_keeps_every_row_and_adds_the_new() {
        let d = tmp("trust-lane-costs");
        let path = d.path().join("state/trust-costs.tsv");
        let mut out = Vec::new();
        let one = |cpu| Cost {
            cpu,
            wall: cpu,
            maxrss: 7,
            when: format!("2026-10-0{cpu}T00:00:00Z"),
        };
        write_costs(
            Some(&path),
            &BTreeMap::from([("a".to_string(), one(1))]),
            &mut out,
        );
        write_costs(
            Some(&path),
            &BTreeMap::from([("b".to_string(), one(2))]),
            &mut out,
        );
        write_costs(
            Some(&path),
            &BTreeMap::from([("a".to_string(), one(3))]),
            &mut out,
        );
        let got = parse_costs(&fs::read_to_string(&path).unwrap());
        assert_eq!(
            got,
            BTreeMap::from([("a".into(), one(3)), ("b".into(), one(2))])
        );
        assert!(out.is_empty());

        // THE SEED: what a clone with no history orders by; a history row wins, and a
        // library the history lacks takes the seed's.
        let seed = d.path().join("costs.tsv");
        write_cost_file(
            &seed,
            &format!("{SEED_HEADER}\n{COSTS_HEADER}\n"),
            &BTreeMap::from([("a".to_string(), one(9)), ("c".to_string(), one(4))]),
            &mut out,
        );
        assert_eq!(
            read_costs(Some(&path), &seed),
            BTreeMap::from([
                ("a".into(), one(3)),
                ("b".into(), one(2)),
                ("c".into(), one(4))
            ])
        );
        let none = d.path().join("no-history.tsv");
        assert_eq!(read_costs(Some(&none), &seed).len(), 2, "a fresh clone");
        assert_eq!(read_costs(None, &seed).len(), 2, "no state at all");
        assert!(read_costs(None, &none).is_empty(), "every library heavy");
        // the committed seed is one a fresh clone reads
        let committed = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(trust_facts::COSTS);
        assert!(
            !read_costs(None, &committed).is_empty(),
            "{}",
            committed.display()
        );
    }

    // -- the slots, over a prover a test drives ---------------------------------

    #[derive(Default)]
    struct World {
        now: Duration,
        live: usize,
        live_heavy: usize,
        peak: usize,
        peak_heavy: usize,
        starts: Vec<String>,
        ended_by_lane: Vec<String>,
    }

    struct Fake {
        w: Rc<RefCell<World>>,
        /// How long each library's check runs; absent runs forever.
        takes: BTreeMap<String, Duration>,
        /// trustd is held once this many checks have started.
        held_after: usize,
        interrupt_at: Option<Duration>,
        unstartable: Option<String>,
    }

    struct FakeCheck {
        w: Rc<RefCell<World>>,
        name: String,
        heavy: bool,
        until: Option<Duration>,
        done: bool,
    }

    impl FakeCheck {
        fn finish(&mut self, code: i32) -> Ended {
            if !self.done {
                self.done = true;
                let mut w = self.w.borrow_mut();
                w.live -= 1;
                if self.heavy {
                    w.live_heavy -= 1;
                }
            }
            Ended {
                code,
                cpu: Duration::from_secs(1),
                maxrss: 1,
            }
        }
    }

    impl Check for FakeCheck {
        fn poll(&mut self) -> std::io::Result<Option<Ended>> {
            let now = self.w.borrow().now;
            Ok(self.until.filter(|u| now >= *u).map(|_| self.finish(0)))
        }
        fn end(&mut self) -> Ended {
            self.w.borrow_mut().ended_by_lane.push(self.name.clone());
            self.finish(137)
        }
    }

    impl Checks for Fake {
        fn start(&mut self, job: &mut Job) -> std::io::Result<Box<dyn Check>> {
            if self.unstartable.as_deref() == Some(job.name.as_str()) {
                return Err(std::io::Error::other("no targo"));
            }
            job.target = PathBuf::from(format!("/slots/j{}", job.slot));
            let mut w = self.w.borrow_mut();
            w.starts.push(job.name.clone());
            w.live += 1;
            w.peak = w.peak.max(w.live);
            if job.heavy {
                w.live_heavy += 1;
                w.peak_heavy = w.peak_heavy.max(w.live_heavy);
            }
            Ok(Box::new(FakeCheck {
                w: Rc::clone(&self.w),
                name: job.name.clone(),
                heavy: job.heavy,
                until: self.takes.get(&job.name).map(|d| w.now + *d),
                done: false,
            }))
        }
        fn trustd_held(&self) -> bool {
            self.w.borrow().starts.len() >= self.held_after
        }
        fn probe_trustd(&mut self) -> String {
            "ours\tthis build's".into()
        }
        fn interrupted(&self) -> bool {
            self.interrupt_at.is_some_and(|t| self.w.borrow().now >= t)
        }
        fn now(&self) -> Duration {
            self.w.borrow().now
        }
        fn pause(&mut self) {
            self.w.borrow_mut().now += Duration::from_secs(1);
        }
    }

    fn job(name: &str, heavy: bool) -> Job {
        Job {
            slot: 0,
            name: name.into(),
            argv: Vec::new(),
            log: PathBuf::from(format!("/nowhere/{name}.log")),
            report: None,
            target: PathBuf::new(),
            heavy,
            lock: None,
        }
    }

    #[test]
    fn a_feature_rows_obligations_keep_the_cargo_package_and_the_row_filename() {
        let dir = tmp("trust-feature-report");
        for (case, (row, reported, accepted)) in [
            ("alpha", "alpha", true),
            ("alpha+sealed", "alpha", true),
            ("alpha+sealed", "foreign", false),
        ]
        .into_iter()
        .enumerate()
        {
            let slot = dir.path().join(case.to_string());
            let report = slot.join("report");
            fs::create_dir_all(&report).unwrap();
            let unit = format!(r#"cargo-target(package="{reported}",target="alpha",kind=["lib"])"#);
            let unit_json = aterm_json::to_string(&unit).unwrap();
            let function = aterm_json::to_string(&format!("{unit}::alpha::work")).unwrap();
            let schema = crate::trust_oblig::REPORT_SCHEMA;
            fs::write(
                report.join("report.ndjson"),
                format!(
                    concat!(
                        r#"{{"record_type":"header","schema":"{schema}","crate_name":{unit_json},"expected_functions":1}}"#,
                        "\n",
                        r#"{{"record_type":"function","function":{function},"summary":{{"total_obligations":1,"proved":1}},"obligations":[{{"kind":"assertion","description":"checked assertion","outcome":{{"status":"proved"}}}}]}}"#,
                        "\n",
                        r#"{{"record_type":"footer","schema":"{schema}","summary":{{"total_obligations":1,"total_proved":1}},"functions_emitted":1}}"#,
                        "\n"
                    ),
                    schema = schema,
                    unit_json = unit_json,
                    function = function,
                ),
            )
            .unwrap();
            let mut check = job(row, false);
            check.log = slot.join(format!("{row}@1.0.0.log"));
            check.report = Some(report.clone());
            let raw_path = report.join("report.ndjson");
            let raw = fs::read(&raw_path).unwrap();
            reduce_report(&check, dir.path());

            let record = PathBuf::from(format!(
                "{}{}",
                check.log.display(),
                crate::trust_oblig::RECORD_SUFFIX
            ));
            let error = PathBuf::from(format!("{}.oblig.err", check.log.display()));
            if accepted {
                let text = fs::read_to_string(&record).unwrap_or_else(|e| {
                    panic!(
                        "{row} lost its real-package report: {e}; {:?}",
                        fs::read_to_string(&error)
                    )
                });
                let head: Vec<&str> = text.lines().take(6).collect();
                let unit_line = format!("unit\t{unit}");
                assert_eq!(
                    head,
                    [
                        crate::trust_oblig::RECORD_MAGIC,
                        "package\talpha",
                        unit_line.as_str(),
                        "functions\t1",
                        "total\t1",
                        "proved\t1",
                    ]
                );
                assert!(!error.exists());
                assert!(
                    !report.exists(),
                    "the successfully reduced report is retired"
                );
            } else {
                assert!(!record.exists());
                assert!(
                    fs::read_to_string(&error)
                        .unwrap()
                        .contains("package foreign, not alpha")
                );
                assert_eq!(fs::read(&raw_path).unwrap(), raw);
            }
        }
    }

    fn fake(takes: &[(&str, u64)]) -> Fake {
        Fake {
            w: Rc::new(RefCell::new(World::default())),
            takes: takes
                .iter()
                .map(|(n, s)| ((*n).to_string(), Duration::from_secs(*s)))
                .collect(),
            held_after: 0,
            interrupt_at: None,
            unstartable: None,
        }
    }

    fn slots(
        f: &mut Fake,
        jobs: Vec<Job>,
        n: usize,
        heavy: usize,
        budget: u64,
    ) -> (Result<(), Stopped>, Vec<Finished>) {
        let mut done = Vec::new();
        let r = run_slots(
            jobs,
            n,
            heavy,
            Duration::from_secs(budget),
            f,
            &mut |fin, _| done.push(fin),
        );
        (r, done)
    }

    /// In queue order, each slot reused only after its check ended.
    #[test]
    fn each_queued_check_runs_once_within_the_slots() {
        let mut f = fake(&[("a", 5), ("b", 1), ("c", 2), ("d", 1)]);
        let jobs = ["a", "b", "c", "d"].map(|n| job(n, false)).to_vec();
        let (r, done) = slots(&mut f, jobs, 2, 1, 100);
        assert_eq!(r, Ok(()));
        let w = f.w.borrow();
        assert_eq!(w.starts, ["a", "b", "c", "d"]);
        assert_eq!(w.peak, 2);
        assert_eq!(done.len(), 4);
        assert!(done.iter().all(|d| !d.over_budget && d.ended.code == 0));
        assert!(
            done.iter()
                .all(|d| d.job.target == Path::new(&format!("/slots/j{}", d.job.slot))),
            "each check's dep-info is read from the slot it ran in"
        );
        let mut slots_used: Vec<usize> = done.iter().map(|d| d.job.slot).collect();
        slots_used.sort_unstable();
        slots_used.dedup();
        assert_eq!(slots_used, [0, 1]);
    }

    /// One heavy slot runs three heavy checks one at a time, a light one beside them.
    #[test]
    fn heavy_checks_never_stack_past_their_slots() {
        let mut f = fake(&[("h1", 3), ("h2", 3), ("h3", 3), ("l", 1)]);
        let jobs = vec![
            job("h1", true),
            job("h2", true),
            job("h3", true),
            job("l", false),
        ];
        let (r, done) = slots(&mut f, jobs, 3, 1, 100);
        assert_eq!(r, Ok(()));
        let w = f.w.borrow();
        assert_eq!(w.peak_heavy, 1);
        assert_eq!(
            w.starts[..2],
            ["h1", "l"],
            "the light one runs beside the heavy"
        );
        assert_eq!(done.len(), 4);
        // the control: two heavy slots let two run at once
        drop(w);
        let mut g = fake(&[("h1", 3), ("h2", 3), ("h3", 3)]);
        let jobs = vec![job("h1", true), job("h2", true), job("h3", true)];
        slots(&mut g, jobs, 3, 2, 100).0.unwrap();
        assert_eq!(g.w.borrow().peak_heavy, 2);
    }

    /// A check that finishes inside its budget is not.
    #[test]
    fn a_check_over_its_budget_is_ended_and_marked() {
        let mut f = fake(&[("quick", 2)]);
        let jobs = vec![job("stuck", false), job("quick", false)];
        let (r, done) = slots(&mut f, jobs, 2, 1, 10);
        assert_eq!(r, Ok(()));
        let stuck = done.iter().find(|d| d.job.name == "stuck").unwrap();
        assert!(stuck.over_budget);
        assert_eq!(stuck.ended.code, 137);
        assert!(stuck.wall >= Duration::from_secs(10));
        let quick = done.iter().find(|d| d.job.name == "quick").unwrap();
        assert!(!quick.over_budget);
        assert_eq!(f.w.borrow().ended_by_lane, ["stuck"]);
    }

    /// One check at a time until this build's trustd serves the socket; then the slots fill.
    #[test]
    fn a_second_check_waits_for_this_builds_trustd() {
        let mut f = fake(&[("a", 3), ("b", 3), ("c", 3)]);
        f.held_after = usize::MAX;
        let jobs = ["a", "b", "c"].map(|n| job(n, false)).to_vec();
        slots(&mut f, jobs.clone(), 3, 3, 100).0.unwrap();
        assert_eq!(f.w.borrow().peak, 1, "never two while trustd is not held");
        let mut g = fake(&[("a", 3), ("b", 3), ("c", 3)]);
        g.held_after = 1;
        slots(&mut g, jobs, 3, 3, 100).0.unwrap();
        assert_eq!(g.w.borrow().peak, 3);
    }

    /// An interrupt ends every running check, starts no more, and decides nothing.
    #[test]
    fn an_interrupt_ends_every_running_check() {
        let mut f = fake(&[]);
        f.interrupt_at = Some(Duration::from_secs(3));
        let jobs = ["a", "b", "c"].map(|n| job(n, false)).to_vec();
        let (r, done) = slots(&mut f, jobs, 2, 2, 100);
        assert_eq!(r, Err(Stopped::Interrupted));
        assert!(done.is_empty());
        let w = f.w.borrow();
        assert_eq!(w.ended_by_lane, ["a", "b"]);
        assert_eq!(w.live, 0);
        assert_eq!(w.starts, ["a", "b"], "nothing starts after it");
    }

    /// A driver that cannot be started is no finding about a library: the run stops,
    /// its checks ended, with why.
    #[test]
    fn an_unstartable_check_stops_the_run() {
        let mut f = fake(&[]);
        f.unstartable = Some("b".into());
        let jobs = ["a", "b"].map(|n| job(n, false)).to_vec();
        let (r, _) = slots(&mut f, jobs, 2, 2, 100);
        assert!(matches!(r, Err(Stopped::Unstartable(why)) if why.contains("no targo")));
        assert_eq!(f.w.borrow().ended_by_lane, ["a"]);
    }

    // -- what a check leaves for the judge --------------------------------------

    fn finished(dir: &Path, log_text: &str, code: i32, over: bool) -> Finished {
        let log = dir.join("alpha@1.0.0.log");
        fs::write(&log, log_text).unwrap();
        let mut j = job("alpha", false);
        j.log = log;
        j.target = dir.join("j0");
        Finished {
            job: j,
            ended: Ended {
                code,
                cpu: Duration::from_secs(4),
                maxrss: 9,
            },
            wall: Duration::from_secs(5),
            over_budget: over,
        }
    }

    const DONE: &str = "Summary: 5 proved, 0 failed (20 total); reproducible\nCoverage: 3/3 eligible function bodies verified (complete)\n";

    #[test]
    fn a_check_leaves_its_status_and_only_its_own_marks() {
        let d = tmp("trust-lane-record");
        let f = finished(d.path(), DONE, 0, false);
        let mut probed = 0;
        record(&f, Duration::from_secs(60), &mut || {
            probed += 1;
            "ours\tx".into()
        })
        .unwrap();
        let beside = |ext: &str| d.path().join(format!("alpha@1.0.0.log.{ext}"));
        assert_eq!(fs::read_to_string(beside("rc")).unwrap(), "0\n");
        assert!(!beside("budget").exists());
        assert!(!beside("trustd").exists());
        assert_eq!(
            probed, 0,
            "the socket is asked only about a check targo refused"
        );
        // over budget, and refused by targo for want of its trustd
        let refused = format!("error: {}\n", trust_facts::TRUSTD_REFUSED);
        let f = finished(d.path(), &refused, 137, true);
        record(&f, Duration::from_secs(60), &mut || {
            "foreign\tanother build".into()
        })
        .unwrap();
        assert_eq!(fs::read_to_string(beside("budget")).unwrap(), "60\n");
        assert_eq!(
            fs::read_to_string(beside("trustd")).unwrap(),
            "foreign\tanother build\n"
        );
    }

    #[cfg(target_os = "macos")]
    type ModeT = u16;
    #[cfg(not(target_os = "macos"))]
    type ModeT = u32;

    unsafe extern "C" {
        fn umask(mask: ModeT) -> ModeT;
    }

    /// The env that turns [`probe_the_lanes_directories_under_umask_002`] on: the
    /// directory it makes them under.
    const UMASK_PROBE_ENV: &str = "ATERM_RELEASE_TRUST_LANE_UMASK_DIR";
    /// That probe's name, as `--exact` wants it.
    const UMASK_PROBE: &str = "trust_lane::tests::probe_the_lanes_directories_under_umask_002";

    /// The child half of [`the_lanes_directories_are_admitted_under_umask_002`]: under umask
    /// `002`, the slot the lane makes is one `targo trust` admits. Outside that child (the
    /// env unset) it does nothing: the umask is the whole process's, so it is set only in a
    /// process running this one test.
    #[test]
    fn probe_the_lanes_directories_under_umask_002() {
        let Some(dir) = std::env::var_os(UMASK_PROBE_ENV) else {
            return;
        };
        // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail; this
        // process runs this one test (`--exact`), so no other create sees the mask.
        unsafe { umask(0o002) };
        let slot = PathBuf::from(dir).join("target-trust.noindex/j0");
        owner_dirs(&slot).unwrap();
        assert_eq!(refused_component(&slot), None, "{}", slot.display());
    }

    /// The target root and slots the lane makes are ones `targo trust` admits under a
    /// login's umask `002` (Ubuntu's), where a plain `create_dir_all` makes them `0775`. The
    /// umask is the whole process's, so they are made in a child of this test binary that
    /// runs only [`probe_the_lanes_directories_under_umask_002`].
    #[test]
    fn the_lanes_directories_are_admitted_under_umask_002() {
        let base = aterm_tempfile::Builder::new()
            .prefix("trust-lane-dirs")
            .tempdir_in(slots_base())
            .unwrap();
        let out = command(std::env::current_exe().unwrap())
            .args(["--exact", "--nocapture", "--test-threads=1", UMASK_PROBE])
            .env(UMASK_PROBE_ENV, base.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the probe under umask 002: {}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A FORK'S LOCKFILE the check created goes; one that was there before stays.
    #[test]
    fn a_forks_lockfile_the_check_created_goes() {
        let d = tmp("trust-lane-lock");
        let argv: Vec<String> = [
            "--manifest-path",
            "vendor/forky/Cargo.toml",
            "-p",
            "forky@0.3.1",
        ]
        .map(String::from)
        .to_vec();
        let fork = d.path().join("vendor/forky");
        fs::create_dir_all(&fork).unwrap();
        let path = fork.join("Cargo.lock");
        assert_eq!(
            fork_lock(d.path(), &["-p".into(), "alpha@1.0.0".into()]),
            None
        );
        let fresh = fork_lock(d.path(), &argv).unwrap();
        assert_eq!(
            (fresh.path.as_path(), fresh.created),
            (path.as_path(), true)
        );
        for created in [true, false] {
            fs::write(&path, "# the fork's own resolve\n").unwrap();
            let mut f = finished(d.path(), DONE, 0, false);
            f.job.lock = Some(ForkLock {
                path: path.clone(),
                created,
            });
            record(&f, Duration::from_secs(60), &mut String::new).unwrap();
            assert_eq!(path.exists(), !created, "created {created}");
        }
        assert!(!fork_lock(d.path(), &argv).unwrap().created);
    }

    // -- a whole run, over a bench the test drives -------------------------------

    /// Checks that end at once, each having printed `log` and written one fresh
    /// dep-info file in its slot, stamped `stamp` when that is set (as a file system
    /// whose clock lags the wall clock would stamp it).
    struct Quick<'a> {
        root: PathBuf,
        log: &'static str,
        interrupt: bool,
        now: Duration,
        started: &'a mut Vec<String>,
        stamp: Option<SystemTime>,
    }

    struct Over;

    impl Check for Over {
        fn poll(&mut self) -> std::io::Result<Option<Ended>> {
            Ok(Some(self.end()))
        }
        fn end(&mut self) -> Ended {
            Ended {
                code: 0,
                cpu: Duration::from_secs(1),
                maxrss: 1,
            }
        }
    }

    impl Checks for Quick<'_> {
        fn start(&mut self, job: &mut Job) -> std::io::Result<Box<dyn Check>> {
            self.started.push(job.name.clone());
            job.target = self.root.join(format!("j{}", job.slot));
            let deps = job.target.join("debug/deps");
            fs::create_dir_all(&deps)?;
            let dep = deps.join(format!("{}.d", job.name));
            fs::write(&dep, "lib.rs: src/lib.rs\n")?;
            if let Some(t) = self.stamp {
                fs::File::options()
                    .write(true)
                    .open(&dep)?
                    .set_modified(t)?;
            }
            fs::write(&job.log, self.log)?;
            Ok(Box::new(Over))
        }
        fn trustd_held(&self) -> bool {
            true
        }
        fn probe_trustd(&mut self) -> String {
            "ours\tthis build's".into()
        }
        fn interrupted(&self) -> bool {
            self.interrupt
        }
        fn now(&self) -> Duration {
            self.now
        }
        fn pause(&mut self) {
            self.now += Duration::from_secs(1);
        }
    }

    /// A git checkout holding the library `alpha` with its floor, beside a stand-in
    /// prover, and a bench whose checks are [`Quick`]; `meanwhile` runs while they do.
    struct Site {
        _dir: aterm_tempfile::TempDir,
        _slots: aterm_tempfile::TempDir,
        repo: PathBuf,
        cfg: Config,
        driver: Driver,
        home: PathBuf,
        /// Where a run with no `--target-root` falls back to.
        cache: PathBuf,
        libraries: Vec<&'static str>,
        started: Vec<String>,
        log: &'static str,
        interrupt: bool,
        meanwhile: Option<fn(&Path)>,
        /// The stamp each check's dep-info gets ([`Quick`]).
        stamp: Option<SystemTime>,
    }

    impl Bench for Site {
        fn driver(&mut self, _: &Config) -> Result<Driver, String> {
            Ok(self.driver.clone())
        }
        fn metadata(&mut self, _: &Driver, repo: &Path) -> Result<Vec<u8>, String> {
            let pkg = |name: &str, dir: &str, member: bool| {
                let id = format!("{name} 1.0.0 (path+file://{}/{dir})", repo.display());
                (
                    format!(
                        r#"{{"id": "{id}", "name": "{name}", "version": "1.0.0", "source": null,
                          "manifest_path": "{}/{dir}/Cargo.toml", "targets": [{{"kind": ["lib"]}}]}}"#,
                        repo.display()
                    ),
                    member.then_some(id),
                )
            };
            let all: Vec<_> = self
                .libraries
                .iter()
                .map(|name| pkg(name, &format!("crates/{name}"), true))
                .collect();
            let packages: Vec<&str> = all.iter().map(|(p, _)| p.as_str()).collect();
            let members: Vec<String> = all
                .iter()
                .filter_map(|(_, m)| m.as_ref().map(|m| format!("\"{m}\"")))
                .collect();
            Ok(format!(
                r#"{{"workspace_root": "{}", "workspace_members": [{}], "packages": [{}], "resolve": {{"nodes": []}}}}"#,
                repo.display(),
                members.join(", "),
                packages.join(", ")
            )
            .into_bytes())
        }
        fn cargo_home(&self) -> String {
            self.home.display().to_string()
        }
        fn cache(&self) -> Option<PathBuf> {
            Some(self.cache.clone())
        }
        fn checks(
            &mut self,
            cfg: &Config,
            prep: &Prepared,
            jobs: Vec<Job>,
            slots: usize,
            done: &mut dyn FnMut(Finished, &mut dyn Checks),
            _: &mut dyn Write,
        ) -> Result<(), String> {
            let mut quick = Quick {
                root: prep.root.clone(),
                log: self.log,
                interrupt: self.interrupt,
                now: Duration::ZERO,
                started: &mut self.started,
                stamp: self.stamp,
            };
            let ran = run_slots(jobs, slots, 1, cfg.budget, &mut quick, done);
            if let Some(edit) = self.meanwhile.take() {
                edit(&self.repo);
            }
            ran.map_err(stopped)
        }
    }

    fn git_in(repo: &Path, args: &[&str]) {
        let out = crate::ledger::testing::git()
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Where a run's slots go: the first of the build's own scratch, the system's temporary
    /// directory and a private `$XDG_RUNTIME_DIR` whose every component `targo trust`
    /// admits ([`refused_component`]: no group- or world-writable one, whatever its group).
    /// macOS's per-user temporary directory is private; a checkout made under Ubuntu's
    /// umask `002` is `0775`, and Linux's /tmp is world-writable, so there it is the
    /// runtime directory.
    ///
    /// Nor may a base lie under a `.cargo/config[.toml]`: the build's own scratch is
    /// inside the checkout when its target directory is (a release clone's is), and the
    /// site's repo would then read the checkout's config, so a run there files nothing
    /// ([`trust_facts::outside_config`]).
    fn slots_base() -> PathBuf {
        let mut refused = Vec::new();
        for base in [
            option_env!("CARGO_TARGET_TMPDIR").map(PathBuf::from),
            Some(std::env::temp_dir()),
            std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        ]
        .into_iter()
        .flatten()
        .filter(|base| base.is_absolute() && base.is_dir())
        {
            if let Some(c) = trust_facts::outside_config(&base.join("site"), None).first() {
                refused.push(format!("{} (under {})", base.display(), c.display()));
                continue;
            }
            match refused_component(&base) {
                None => return base,
                Some(r) => refused.push(format!("{} ({})", r.dir.display(), r.why)),
            }
        }
        panic!(
            "no directory here can hold a target root `targo trust` admits: {}",
            refused.join(", ")
        )
    }

    fn site(scope: Scope) -> Site {
        let dir = tmp("trust-lane-run");
        let slots = aterm_tempfile::Builder::new()
            .prefix("trust-lane-slots")
            .tempdir_in(slots_base())
            .unwrap();
        // Under the slots' base, which targo admits, so a test can make the checkout's own
        // `target-trust.noindex` admitted or refused by the checkout's mode alone.
        let repo = slots.path().join("repo");
        crate::ledger::testing::init(
            &repo,
            &[
                ("user.name", "Test"),
                ("user.email", "test@example.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        fs::create_dir_all(repo.join("crates/aterm-release/trust")).unwrap();
        fs::write(
            repo.join(trust_facts::RATCHET),
            "# <crate>\t<proved floor>\t<total>\t<prover --version>\nalpha\t5\t20\ttargo 0.0.0-test\n",
        )
        .unwrap();
        fs::write(repo.join(trust_facts::MEMORY), "# no library needs more\n").unwrap();
        fs::create_dir_all(repo.join("crates/alpha/src")).unwrap();
        fs::write(repo.join("crates/alpha/src/lib.rs"), "pub fn f() {}\n").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-q", "-m", "base"]);
        let bin = dir.path().join("prover/bin");
        fs::create_dir_all(&bin).unwrap();
        for tool in ["targo", "trustc", "trustd"] {
            fs::write(bin.join(tool), tool).unwrap();
        }
        let home = dir.path().join("cargo-home");
        fs::create_dir_all(&home).unwrap();
        let mut cfg = Config::new(&repo, scope);
        cfg.jobs = Some(1);
        cfg.target_root = Some(slots.path().join("target-trust.noindex"));
        cfg.state = Some(dir.path().join("state"));
        let cache = slots.path().join("cache");
        Site {
            cache,
            driver: Driver {
                targo: bin.join("targo"),
                bin,
                id: "targo 0.0.0-test".into(),
            },
            _dir: dir,
            _slots: slots,
            repo,
            cfg,
            home,
            libraries: vec!["alpha"],
            started: Vec::new(),
            log: DONE,
            interrupt: false,
            meanwhile: None,
            stamp: None,
        }
    }

    /// The target root a site's runs name.
    fn root_of(s: &Site) -> PathBuf {
        s.cfg.target_root.clone().unwrap()
    }

    /// The run's output, and what it filed in the checkout's store.
    fn run_site(s: &mut Site) -> (Outcome, String, Vec<String>) {
        let cfg = s.cfg.clone();
        let mut out = Vec::new();
        let outcome = run_on(&cfg, s, &mut out);
        let store = s.repo.join(".git/aterm-verify/receipts");
        let mut filed: Vec<String> = fs::read_dir(&store)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with(trust_facts::FACTS_PREFIX) && !n.contains('.'))
                    .collect()
            })
            .unwrap_or_default();
        filed.sort();
        (outcome, String::from_utf8(out).unwrap(), filed)
    }

    /// A whole-tree run on a clean checkout files its facts under the tree's key; the plan
    /// names the tree, the prover and the lane's environment.
    #[test]
    fn a_clean_whole_tree_run_files_its_facts() {
        let mut s = site(Scope::WholeTree);
        let (outcome, said, filed) = run_site(&mut s);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        assert_eq!(filed.len(), 1, "{said}");
        assert!(said.contains("\nFACTS: "), "{said}");
        let plan = fs::read_to_string(root_of(&s).join("logs/.plan")).unwrap();
        let head =
            crate::ledger::rev_parse(&crate::ledger::testing::Git::new(&s.repo), "HEAD").unwrap();
        assert!(plan.contains(&format!("\ncommit {head}\n")), "{plan}");
        assert!(
            plan.contains("\nfile \n"),
            "nothing kept it from filing: {plan}"
        );
        assert!(plan.contains("\nlane-passed PATH\n"));
        assert!(!root_of(&s).join("j0").exists(), "the slots are scratch");
    }

    /// THE RUN'S DEP-INFO IS WHAT ITS SLOTS HOLD, whatever the host's clocks say: every
    /// bench empties the slots before its checks start, so a dep-info file a check wrote
    /// counts however old its file system stamped it, and one a killed run left in a slot
    /// is gone before the checks start, though it names the floors. (The start-of-run
    /// stamp test this replaces read a file written within a tick of the run's start as
    /// older than the run, which only an instant bench like this one did; and only the real
    /// bench emptied its slots.) A slot that cannot be emptied, whoever runs the tests,
    /// stops a run that files before any check, so nothing it holds is read as this run's;
    /// a -p run reads no dep-info, says so and goes on.
    #[test]
    fn a_runs_dep_info_is_what_its_slots_hold_whatever_the_clock() {
        let mut lagged = site(Scope::WholeTree);
        lagged.stamp = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000));
        let (outcome, said, filed) = run_site(&mut lagged);
        assert_eq!((outcome, filed.len()), (Outcome::Pass, 1), "{said}");

        let mut left = site(Scope::WholeTree);
        let stale = root_of(&left).join("j3/debug/deps/stale.d");
        owner_dirs(stale.parent().unwrap()).unwrap();
        fs::write(&stale, format!("x: {}\n", trust_facts::RATCHET)).unwrap();
        let (outcome, said, filed) = run_site(&mut left);
        assert_eq!((outcome, filed.len()), (Outcome::Pass, 1), "{said}");
        assert!(!stale.exists(), "a killed run's slot is gone");

        // A file where a slot goes: removing a directory tree fails on it (not a
        // directory) for every user, root too, which a permission bit would not.
        let stuck = |scope| {
            let s = site(scope);
            let root = root_of(&s);
            owner_dirs(&root).unwrap();
            fs::write(root.join("j9"), "").unwrap();
            s
        };
        let mut whole = stuck(Scope::WholeTree);
        let (outcome, said, filed) = run_site(&mut whole);
        assert!(
            matches!(&outcome, Outcome::CouldNotRun(why) if why.contains("could not be emptied")),
            "{outcome:?}: {said}"
        );
        assert!(filed.is_empty() && whole.started.is_empty(), "{said}");

        let mut named = stuck(Scope::Packages(vec!["alpha".into()]));
        let (outcome, said, filed) = run_site(&mut named);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        assert!(said.contains("could not be emptied"), "it says so: {said}");
        assert!(filed.is_empty() && named.started == ["alpha"], "{said}");
    }

    /// `--update-ratchet` REWRITES THE COMMITTED COST SEED beside the floors: the row it
    /// measured takes this run's cost, a row it did not measure keeps its own, with the
    /// time it was measured — an older row of this clone's history does not undo it, and
    /// a library that left the graph is not carried back in from that history; a run
    /// without it leaves the seed alone.
    #[test]
    fn an_update_ratchet_run_rewrites_the_cost_seed() {
        let committed = format!(
            "{SEED_HEADER}\n{COSTS_HEADER}\nalpha\t900\t900\t{}\t2026-10-01T00:00:00Z\nzeta\t50\t60\t70\t2026-10-02T00:00:00Z\n",
            20_u64 << 30
        );
        let seeded = |update_ratchet: bool| {
            let mut s = site(Scope::Packages(vec!["alpha".into()]));
            s.libraries = vec!["alpha", "zeta"];
            fs::write(s.repo.join(trust_facts::COSTS), &committed).unwrap();
            git_in(&s.repo, &["add", "."]);
            git_in(&s.repo, &["commit", "-q", "-m", "the seed"]);
            let state = s.cfg.state.clone().unwrap();
            fs::create_dir_all(&state).unwrap();
            fs::write(
                state.join("trust-costs.tsv"),
                format!(
                    "{COSTS_HEADER}\nzeta\t9\t9\t9\t2026-09-01T00:00:00Z\ngone\t5\t5\t5\t2026-10-05T00:00:00Z\n"
                ),
            )
            .unwrap();
            s.cfg.update_ratchet = update_ratchet;
            s
        };

        let mut s = seeded(false);
        let (outcome, said, _) = run_site(&mut s);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        assert_eq!(
            fs::read_to_string(s.repo.join(trust_facts::COSTS)).unwrap(),
            committed,
            "a run without --update-ratchet leaves the seed alone"
        );

        let mut s = seeded(true);
        let seed = s.repo.join(trust_facts::COSTS);
        let (outcome, said, _) = run_site(&mut s);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        let costs = read_costs(None, &seed);
        let alpha = &costs["alpha"];
        assert_eq!((alpha.cpu, alpha.maxrss), (1, 1), "this run's measurement");
        assert_ne!(alpha.when, "2026-10-01T00:00:00Z", "stamped by this run");
        assert_eq!(
            costs["zeta"],
            Cost {
                cpu: 50,
                wall: 60,
                maxrss: 70,
                when: "2026-10-02T00:00:00Z".into(),
            },
            "a row it did not measure keeps its own, over an older row of the history"
        );
        assert!(
            !costs.contains_key("gone"),
            "a library outside the graph is not written back: {costs:?}"
        );
    }

    /// THE SEED'S MERGE: the later row of each library still in the graph, whichever
    /// file holds it; a row with no time is the oldest; a library the graph lost goes.
    #[test]
    fn the_seed_keeps_the_later_row_of_each_library_still_in_the_graph() {
        let row = |cpu: u64, when: &str| Cost {
            cpu,
            wall: cpu,
            maxrss: cpu,
            when: when.into(),
        };
        let committed = BTreeMap::from([
            ("newer-here".to_string(), row(1, "2026-10-01T00:00:00Z")),
            ("newer-there".to_string(), row(2, "2026-10-09T00:00:00Z")),
            ("untimed-here".to_string(), row(3, "2026-10-01T00:00:00Z")),
            ("only-there".to_string(), row(4, "2026-10-01T00:00:00Z")),
            ("gone".to_string(), row(5, "2026-10-01T00:00:00Z")),
        ]);
        let history = BTreeMap::from([
            ("newer-here".to_string(), row(11, "2026-10-05T00:00:00Z")),
            ("newer-there".to_string(), row(12, "2026-10-05T00:00:00Z")),
            ("untimed-here".to_string(), row(13, "")),
            ("only-here".to_string(), row(14, "2026-10-05T00:00:00Z")),
            ("gone-too".to_string(), row(15, "2026-10-05T00:00:00Z")),
        ]);
        let graph: BTreeSet<String> = [
            "newer-here",
            "newer-there",
            "untimed-here",
            "only-there",
            "only-here",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let cpu: BTreeMap<String, u64> = seed_rows(committed, history, &graph)
            .into_iter()
            .map(|(n, c)| (n, c.cpu))
            .collect();
        assert_eq!(
            cpu,
            BTreeMap::from([
                ("newer-here".to_string(), 11),
                ("newer-there".to_string(), 2),
                ("untimed-here".to_string(), 3),
                ("only-there".to_string(), 4),
                ("only-here".to_string(), 14),
            ])
        );
    }

    /// A whole-tree record with alpha undecided and beta already above its floor:
    /// both checks print the same report, but only alpha needs another measurement.
    fn undecided_site() -> Site {
        let mut s = site(Scope::WholeTree);
        s.libraries.push("beta");
        fs::create_dir_all(s.repo.join("crates/beta/src")).unwrap();
        fs::write(s.repo.join("crates/beta/src/lib.rs"), "pub fn g() {}\n").unwrap();
        let floors = s.repo.join(trust_facts::RATCHET);
        let mut text = fs::read_to_string(&floors).unwrap();
        text.push_str("beta\t4\t20\ttargo 0.0.0-test\n");
        fs::write(floors, text).unwrap();
        git_in(&s.repo, &["add", "."]);
        git_in(&s.repo, &["commit", "-q", "-m", "another library"]);
        s.log = "timed_out=1\nSummary: 4 proved, 0 failed (20 total); reproducible\nCoverage: 3/3 eligible function bodies verified (complete)\n";
        s
    }

    fn filed_facts(s: &Site, name: &str) -> trust_facts::Facts {
        let record =
            fs::read_to_string(s.repo.join(".git/aterm-verify/receipts").join(name)).unwrap();
        trust_facts::Facts::parse(&record).unwrap()
    }

    /// Only the undecided library is measured and the decided one's facts are kept; a
    /// second remeasure runs nothing.
    #[test]
    fn a_remeasure_completes_its_record_and_then_runs_nothing() {
        let mut s = undecided_site();
        let (outcome, said, filed) = run_site(&mut s);
        assert_eq!(outcome, Outcome::Undecided(vec!["alpha".into()]), "{said}");
        s.started.sort();
        assert_eq!(s.started, ["alpha", "beta"]);
        assert_eq!(filed.len(), 1, "{said}");
        let original = filed_facts(&s, &filed[0]);
        let prover = trust_facts::Prover::at(&s.driver.bin);
        let git = crate::ledger::testing::Git::new(&s.repo);
        assert_eq!(
            trust_facts::pending_for(&git, &prover, "HEAD")
                .unwrap()
                .unwrap(),
            ["alpha"]
        );

        s.cfg.scope = Scope::Remeasure;
        s.log = DONE;
        s.started.clear();
        let (outcome, said, completed_files) = run_site(&mut s);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        assert_eq!(s.started, ["alpha"]);
        assert_eq!(completed_files, filed, "the same record is completed");
        let completed = filed_facts(&s, &filed[0]);
        assert_eq!(
            completed.crates.iter().find(|c| c.name == "beta"),
            original.crates.iter().find(|c| c.name == "beta"),
            "a library that was decided retains its earlier measurement"
        );
        assert_eq!(
            completed.provenance.as_ref().unwrap().key,
            original.provenance.as_ref().unwrap().key
        );
        assert!(
            trust_facts::pending_for(&git, &prover, "HEAD")
                .unwrap()
                .unwrap()
                .is_empty()
        );

        s.started.clear();
        s.interrupt = true;
        let (outcome, said, noop_files) = run_site(&mut s);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        assert!(s.started.is_empty(), "a completed record starts no check");
        assert_eq!(noop_files, filed);
        assert_eq!(filed_facts(&s, &filed[0]).crates, completed.crates);
    }

    /// A source commit changes the facts key: it cannot resume the earlier record,
    /// starts no checks and preserves that record for the commit it measured.
    #[test]
    fn a_remeasure_never_reuses_facts_after_a_source_commit() {
        let mut s = undecided_site();
        let (outcome, said, filed) = run_site(&mut s);
        assert_eq!(outcome, Outcome::Undecided(vec!["alpha".into()]), "{said}");
        assert_eq!(filed.len(), 1, "{said}");
        let original = filed_facts(&s, &filed[0]);
        let git = crate::ledger::testing::Git::new(&s.repo);
        let measured = crate::ledger::rev_parse(&git, "HEAD").unwrap();
        fs::write(
            s.repo.join("crates/alpha/src/lib.rs"),
            "pub fn changed() {}\n",
        )
        .unwrap();
        git_in(&s.repo, &["commit", "-q", "-am", "change a library"]);

        s.cfg.scope = Scope::Remeasure;
        s.log = DONE;
        s.started.clear();
        let (outcome, said, after) = run_site(&mut s);
        assert!(
            matches!(outcome, Outcome::CouldNotRun(_)),
            "{outcome:?}: {said}"
        );
        assert!(s.started.is_empty(), "another key starts no check");
        assert_eq!(after, filed, "no record was filed for unmeasured source");
        assert_eq!(filed_facts(&s, &filed[0]), original);
        let prover = trust_facts::Prover::at(&s.driver.bin);
        assert!(
            trust_facts::pending_for(&git, &prover, "HEAD")
                .unwrap()
                .is_err()
        );
        assert_eq!(
            trust_facts::pending_for(&git, &prover, &measured)
                .unwrap()
                .unwrap(),
            ["alpha"],
            "the original commit still has its own undecided record"
        );
    }

    /// Nothing is filed for bytes that are not HEAD's (dirty at the start, an edit or a
    /// commit during the run), nor by a `-p` run.
    #[test]
    fn a_run_files_nothing_for_a_tree_it_did_not_verify() {
        let mut dirty = site(Scope::WholeTree);
        fs::write(
            dirty.repo.join("crates/alpha/src/lib.rs"),
            "pub fn g() {}\n",
        )
        .unwrap();
        let (outcome, said, filed) = run_site(&mut dirty);
        assert_eq!((outcome, filed.len()), (Outcome::Pass, 0), "{said}");
        assert!(said.contains("FACTS: none"), "{said}");

        let edits: [fn(&Path); 2] = [
            |repo| fs::write(repo.join("crates/alpha/src/lib.rs"), "pub fn h() {}\n").unwrap(),
            |repo| git_in(repo, &["commit", "-q", "--allow-empty", "-m", "meanwhile"]),
        ];
        for edit in edits {
            let mut s = site(Scope::WholeTree);
            s.meanwhile = Some(edit);
            let (outcome, said, filed) = run_site(&mut s);
            assert_eq!((outcome, filed.len()), (Outcome::Pass, 0), "{said}");
            assert!(said.contains("FACTS: none"), "{said}");
        }

        let mut narrow = site(Scope::Packages(vec!["alpha".into()]));
        let (outcome, said, filed) = run_site(&mut narrow);
        assert_eq!((outcome, filed.len()), (Outcome::Pass, 0), "{said}");
        assert!(
            !said.contains("FACTS:"),
            "a -p run is no filing run: {said}"
        );
    }

    /// AN INTERRUPTED RUN DECIDES NOTHING and files nothing.
    #[test]
    fn an_interrupted_run_decides_nothing() {
        let mut s = site(Scope::WholeTree);
        s.interrupt = true;
        let (outcome, said, filed) = run_site(&mut s);
        assert!(matches!(outcome, Outcome::CouldNotRun(_)), "{said}");
        assert!(filed.is_empty());
    }

    /// A host that cannot reproduce a floor refuses that library before any check: with
    /// `stop_if_narrowed` nothing runs, else the rest runs; either way the run is NARROWED.
    #[test]
    fn a_floor_this_host_cannot_reproduce_narrows_the_run() {
        for stop in [true, false] {
            let mut s = site(Scope::WholeTree);
            fs::write(
                s.repo.join(trust_facts::MEMORY),
                "# no host here verifies alpha's floor\nalpha\t4096.0\t4096\t512.0\t1000.0\t=\ta test\n",
            )
            .unwrap();
            git_in(&s.repo, &["commit", "-q", "-am", "a floor set at 4 TiB"]);
            s.cfg.stop_if_narrowed = stop;
            let (outcome, said, filed) = run_site(&mut s);
            assert!(
                matches!(&outcome, Outcome::Narrowed(l) if l.len() == 1 && l[0].contains("alpha")),
                "{outcome:?}: {said}"
            );
            assert_eq!(outcome.exit(), 4);
            assert!(
                !root_of(&s).join("logs/alpha@1.0.0.log").exists(),
                "a refused library is never run: {said}"
            );
            if stop {
                assert!(filed.is_empty(), "a run that stopped files nothing: {said}");
            }
        }
    }

    /// A floor the graph no longer carries fails the whole-tree run (FORK-VANISHED); a `-p`
    /// run answers only for what it names.
    #[test]
    fn a_vanished_fork_fails_the_whole_tree_run() {
        for scope in [Scope::WholeTree, Scope::Packages(vec!["alpha".into()])] {
            let whole = scope == Scope::WholeTree;
            let mut s = site(scope);
            let floors = s.repo.join(trust_facts::RATCHET);
            let mut text = fs::read_to_string(&floors).unwrap();
            text.push_str("pkg-config\t5\t6\ttargo 0.0.0-test\n");
            fs::write(&floors, text).unwrap();
            git_in(
                &s.repo,
                &["commit", "-q", "-am", "a fork's patch row deleted"],
            );
            let (outcome, said, _) = run_site(&mut s);
            if whole {
                assert_eq!(
                    outcome,
                    Outcome::Fail(vec!["pkg-config FORK-VANISHED".into()]),
                    "{said}"
                );
            } else {
                assert_eq!(outcome, Outcome::Pass, "{said}");
            }
        }
    }

    /// With no `--target-root`, a checkout `targo trust` refuses (`0775`, as a login's
    /// umask `002` makes it) runs under its own directory of the user's cache: said on one
    /// line, recorded with its checkout, made as targo admits it, the same directory the
    /// next run; a root there whose checkout is gone is pruned, and the checkout is left
    /// with no `target-trust.noindex`. Once the checkout is `chmod go-w`'d (the note's fix), the
    /// run is back in `target-trust.noindex`, and the fallback root goes, with what a killed run
    /// left in it, and orphans are still pruned.
    #[test]
    fn a_refused_checkout_runs_under_its_own_cache_root() {
        use std::os::unix::fs::PermissionsExt as _;
        let mut s = site(Scope::Packages(vec!["alpha".into()]));
        s.cfg.target_root = None;
        fs::set_permissions(&s.repo, fs::Permissions::from_mode(0o775)).unwrap();
        let checkout = fs::canonicalize(&s.repo).unwrap();
        let root = s.cache.join(checkout_key(&checkout));
        let cache = s.cache.clone();
        let orphan = |name: &str| {
            let checkout = cache.join(name);
            let dir = cache.join(checkout_key(&checkout));
            owner_dirs(&dir).unwrap();
            fs::write(dir.join(CHECKOUT_RECORD), checkout_record(&checkout)).unwrap();
            fs::set_permissions(dir.join(CHECKOUT_RECORD), fs::Permissions::from_mode(0o600))
                .unwrap();
            fs::write(dir.join(".run.lock"), "").unwrap();
            fs::set_permissions(dir.join(".run.lock"), fs::Permissions::from_mode(0o600)).unwrap();
            dir
        };
        let first_orphan = orphan("deleted-0123456789ab");
        // A record a group could rewrite (as a umask `002` writes one) is tightened.
        owner_dirs(&root).unwrap();
        fs::write(root.join(CHECKOUT_RECORD), "stale\n").unwrap();
        fs::set_permissions(
            root.join(CHECKOUT_RECORD),
            fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        let root_s = root.display().to_string();
        let own_s = s.repo.join("target-trust.noindex").display().to_string();
        for run in ["first", "second"] {
            let (outcome, said, _) = run_site(&mut s);
            assert_eq!(outcome, Outcome::Pass, "{run}: {said}");
            // One line: the root it uses, the one refused, the component, and its fix.
            let note: Vec<&str> = said.lines().filter(|l| l.contains(&own_s)).collect();
            assert!(
                note.len() == 1
                    && says_in_order(
                        note[0],
                        &[
                            &root_s,
                            &own_s,
                            &checkout.display().to_string(),
                            "775",
                            &format!("chmod go-w {}", checkout.display()),
                        ]
                    ),
                "{run}: {said}"
            );
            assert!(root.join("logs/.plan").is_file(), "{run}: {said}");
            assert_eq!(
                fs::read(root.join(CHECKOUT_RECORD)).unwrap(),
                checkout_record(&checkout)
            );
            assert_eq!(
                fs::metadata(root.join(CHECKOUT_RECORD))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o644,
                "{run}: only its owner can rewrite the record"
            );
            assert_eq!(refused_component(&root.join("j0")), None);
        }
        assert!(!first_orphan.exists(), "the orphan is pruned");
        assert!(!s.repo.join("target-trust.noindex").exists());

        // What a SIGKILLed fallback run leaves: a slot, and a record of its check.
        fs::create_dir_all(root.join("j0/debug")).unwrap();
        fs::write(root.join(".check.0"), "4194304999 never\n").unwrap();
        fs::set_permissions(root.join(".check.0"), fs::Permissions::from_mode(0o600)).unwrap();
        let second_orphan = orphan("deleted-ba9876543210");
        fs::set_permissions(&s.repo, fs::Permissions::from_mode(0o755)).unwrap();
        let (outcome, said, _) = run_site(&mut s);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        assert!(
            s.repo.join("target-trust.noindex/logs/.plan").is_file(),
            "back in the checkout: {said}"
        );
        // No fallback note; one line, saying the fallback root was removed.
        let naming: Vec<&str> = said.lines().filter(|l| l.contains(&root_s)).collect();
        assert!(naming.len() == 1 && naming[0].contains("removed"), "{said}");
        assert!(!root.exists(), "the fallback root is retired: {said}");
        assert!(
            !second_orphan.exists(),
            "orphans are pruned on any default run"
        );
    }

    // -- the environment, the root, the lock -------------------------------------

    #[test]
    fn the_lane_environment_is_built_from_a_list() {
        let d = tmp("trust-lane-env");
        let host = |k: &str| match k {
            "HOME" => Some("/h".to_string()),
            "CARGO_BUILD_JOBS" => Some("lots".to_string()),
            "TRUSTFLAGS" | "RUSTFLAGS" => Some("-Zx".to_string()),
            _ => None,
        };
        let env = lane_env(d.path(), &host);
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            ["TRUST_NO_MIGRATE_WARN", "CARGO_INCREMENTAL", "PATH", "HOME"]
        );
        assert!(env[2].1.starts_with(&format!("{}:", d.path().display())));
        fs::write(d.path().join("trustdoc"), "x").unwrap();
        let host = |k: &str| (k == "CARGO_BUILD_JOBS").then(|| "4".to_string());
        let names: Vec<String> = lane_env(d.path(), &host)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert!(names.contains(&"CARGO_BUILD_JOBS".into()) && names.contains(&"RUSTDOC".into()));
        assert!(names.iter().all(|n| LANE_PASSED.contains(&n.as_str())
            || LANE_PINS.iter().any(|(p, _)| p == n)));
    }

    /// A group- or world-writable component of the target root is COULD NOT RUN up front:
    /// `targo trust` would refuse the check, which would read as a finding.
    #[test]
    fn a_world_writable_component_of_the_target_root_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("trust-lane-root");
        let open = d.path().join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
        let r = refused_component(&open.join("target-trust.noindex")).unwrap();
        assert_eq!(r.dir, fs::canonicalize(&open).unwrap());
        assert!(r.why.contains("world-writable"), "{}", r.why);
    }

    /// Each component rule `targo trust` holds: another owner, a group- or
    /// world-writable mode (sticky or not), a root-owned directory an ACL lets the
    /// caller write; root's or the caller's own, writable by its owner alone, passes.
    #[test]
    fn each_component_rule_targo_holds_is_said() {
        let me = 501;
        let no = || false;
        assert_eq!(component_refusal(0, 0o40755, me, no), None);
        assert_eq!(component_refusal(me, 0o40700, me, || true), None);
        assert!(component_refusal(502, 0o40755, me, no).is_some_and(|w| w.contains("uid=502")));
        assert!(component_refusal(0, 0o41777, me, no).is_some_and(|w| w.contains("1777")));
        assert!(component_refusal(me, 0o40775, me, no).is_some_and(|w| w.contains("775")));
        assert!(component_refusal(0, 0o40755, me, || true).is_some_and(|w| w.contains("ACL")));
        assert!(proc_tree::writable(&std::env::temp_dir()));
        assert!(!proc_tree::writable(Path::new(
            "/nonexistent-trust-lane-dir"
        )));
    }

    fn refusal(dir: &str, chmod: Option<&'static str>) -> Refused {
        Refused {
            dir: dir.into(),
            why: "mode 775: group- or world-writable".into(),
            chmod,
        }
    }

    /// Whether each of `parts` appears in `said`, in that order: a message names each
    /// refused path and puts each fix after its refusal, however its prose is worded.
    fn says_in_order(said: &str, parts: &[&str]) -> bool {
        let mut rest = said;
        parts.iter().all(|p| {
            rest.find(p).is_some_and(|at| {
                rest = &rest[at + p.len()..];
                true
            })
        })
    }

    /// A checkout that still holds the root from before the `.noindex` name is told so by
    /// a default run, and nothing is removed; a named root, an absent one and a link are
    /// not mentioned.
    #[test]
    fn a_default_run_names_the_root_left_from_before_the_noindex_name() {
        let dir = tmp("trust-legacy");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let cfg = Config::new(&repo, Scope::WholeTree);
        assert_eq!(legacy_root_note(&cfg), None, "nothing left");
        assert_eq!(
            choose_root(&repo, "k", None, &|_| None).unwrap().path,
            repo.join(OWN_ROOT)
        );
        let legacy = repo.join("target-trust");
        fs::create_dir_all(legacy.join("logs")).unwrap();
        let note = legacy_root_note(&cfg).expect("the old root is named");
        assert!(note.contains(&legacy.display().to_string()), "{note}");
        assert!(legacy.join("logs").is_dir(), "the note removes nothing");
        let mut named = cfg.clone();
        named.target_root = Some(legacy.clone());
        assert_eq!(
            legacy_root_note(&named),
            None,
            "a named root is the caller's"
        );
        fs::remove_dir_all(&legacy).unwrap();
        std::os::unix::fs::symlink(dir.path(), &legacy).unwrap();
        assert_eq!(legacy_root_note(&cfg), None, "a link is never followed");

        // A named root Spotlight would index is told so on macOS; the default and one
        // under a `*.noindex` folder are not.
        let mine = Path::new("/w/target/trust-gui1");
        assert_eq!(
            indexed_root_note(&cfg, &repo.join(OWN_ROOT)),
            None,
            "the default"
        );
        let note = indexed_root_note(&named, mine);
        if cfg!(target_os = "macos") {
            let note = note.expect("an indexed root is named");
            assert!(note.contains("/w/target/trust-gui1.noindex"), "{note}");
        } else {
            assert_eq!(note, None, "Spotlight is macOS's");
        }
        assert_eq!(
            indexed_root_note(&named, Path::new("/w/target/mine.noindex/trust")),
            None
        );
    }

    /// THE DEFAULT TARGET ROOT: `<repo>/target-trust.noindex` wherever targo admits it; else this
    /// checkout's directory of the user's cache, said with the fix that would bring the
    /// first back; when neither is admitted, nothing runs and each refusal is named with
    /// its fix.
    #[test]
    fn the_default_target_root_falls_back_only_when_targo_refuses_it() {
        let repo = Path::new("/w/aterm");
        let cache = Path::new("/h/.cache/aterm-trust-lane");
        let in_repo = |p: &Path| p.starts_with(repo).then(|| refusal("/w/aterm", GO_W));
        let mine = cache.join("aterm-0123");

        let admitted = choose_root(repo, "aterm-0123", Some(cache), &|_| None).unwrap();
        assert_eq!(
            admitted,
            Root {
                path: "/w/aterm/target-trust.noindex".into(),
                mine: Some(mine.clone()),
                note: None,
            },
            "admitted, with this checkout's cache directory to retire"
        );
        assert_eq!(
            choose_root(repo, "aterm-0123", None, &|_| None).map(|r| (r.path, r.mine)),
            Ok(("/w/aterm/target-trust.noindex".into(), None))
        );

        let fallback = choose_root(repo, "aterm-0123", Some(cache), &in_repo).unwrap();
        assert_eq!(fallback.path, mine);
        assert_eq!(fallback.mine.as_ref(), Some(&mine));
        let note = fallback.note.unwrap();
        assert!(
            says_in_order(
                &note,
                &[
                    "/h/.cache/aterm-trust-lane/aterm-0123",
                    "/w/aterm/target-trust.noindex",
                    "/w/aterm",
                    "775",
                    "chmod go-w /w/aterm",
                ]
            ),
            "{note}"
        );

        let both = choose_root(repo, "aterm-0123", Some(cache), &|p| {
            in_repo(p).or_else(|| Some(refusal("/h", None)))
        })
        .unwrap_err();
        assert!(
            says_in_order(
                &both,
                &[
                    "/w/aterm/target-trust.noindex",
                    "/w/aterm",
                    "run: chmod go-w /w/aterm",
                    "/h/.cache/aterm-trust-lane/aterm-0123",
                    "/h,",
                    ELSEWHERE_ROOT,
                ]
            ) && both.matches("chmod").count() == 1,
            "{both}"
        );

        let homeless = choose_root(repo, "aterm-0123", None, &in_repo).unwrap_err();
        assert!(
            homeless.contains("run: chmod go-w /w/aterm") && homeless.contains("no home directory"),
            "{homeless}"
        );
    }

    /// The fallback lies in the user's cache (`$XDG_CACHE_HOME` only when absolute, as the
    /// XDG rule says; macOS's unindexed caches), under a name per checkout: two worktrees
    /// of one name never share a run lock.
    #[test]
    fn the_fallback_root_is_the_users_cache_keyed_by_checkout() {
        let home = Some(Path::new("/h"));
        let linux = |xdg: Option<&str>| cache_base(home, xdg.map(Path::new), false);
        assert_eq!(linux(None), Some("/h/.cache/aterm-trust-lane".into()));
        assert_eq!(
            linux(Some("/x/cache")),
            Some("/x/cache/aterm-trust-lane".into())
        );
        assert_eq!(
            linux(Some("x/cache")),
            Some("/h/.cache/aterm-trust-lane".into()),
            "a relative XDG_CACHE_HOME is ignored"
        );
        assert_eq!(
            cache_base(None, Some(Path::new("/x")), false),
            Some("/x/aterm-trust-lane".into())
        );
        assert_eq!(cache_base(Some(Path::new("h")), None, false), None);
        assert_eq!(
            cache_base(home, Some(Path::new("/x")), true),
            Some("/h/Library/Caches/aterm-trust-lane.noindex".into())
        );
        let digest = trust_facts::hex(&aterm_digest::Sha256::digest(b"/w/aterm"));
        assert_eq!(
            checkout_key(Path::new("/w/aterm")),
            format!("aterm-{}", &digest[..12])
        );
        assert_ne!(
            checkout_key(Path::new("/w/aterm")),
            checkout_key(Path::new("/v/aterm"))
        );
        assert!(checkout_key(Path::new("/w/a b")).starts_with("a_b-"));
        assert!(checkout_key(Path::new("/")).starts_with("checkout-"));
    }

    /// A refusal's fix: `chmod go-w` on a component of the caller's that only its mode
    /// keeps out (the target root's, said up front), another directory for another
    /// owner's.
    #[test]
    fn a_refusal_names_its_fix() {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(refusal("/w", GO_W).fix("elsewhere"), "run: chmod go-w /w");
        assert_eq!(
            refusal("/w", Some("a-st")).fix("elsewhere"),
            "run: chmod a-st /w"
        );
        assert_eq!(refusal("/w", None).fix("elsewhere"), "elsewhere");
        assert_eq!(
            refusal("/w/a b'c", GO_W).fix("elsewhere"),
            r"run: chmod go-w '/w/a b'\''c'"
        );
        let base = aterm_tempfile::Builder::new()
            .prefix("trust-lane-fix")
            .tempdir_in(slots_base())
            .unwrap();
        let open = base.path().join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o775)).unwrap();
        let open = fs::canonicalize(&open).unwrap();
        // A cache a fallback could use is there: a named root never falls back to it.
        let cache = base.path().join("cache");
        owner_dirs(&cache).unwrap();
        let mut cfg = Config::new(base.path(), Scope::WholeTree);
        cfg.target_root = Some(open.join("target-trust.noindex"));
        let why = target_root(&cfg, Some(&cache)).unwrap_err();
        assert!(
            says_in_order(
                &why,
                &[
                    &open.join("target-trust.noindex").display().to_string(),
                    "mode 775",
                    &format!("run: chmod go-w {}", open.display()),
                ]
            ),
            "{why}"
        );
        cfg.target_root = Some(base.path().join("target-trust.noindex"));
        assert_eq!(
            target_root(&cfg, Some(&cache)),
            Ok(Root {
                path: base.path().join("target-trust.noindex"),
                mine: None,
                note: None,
            }),
            "a named root never falls back, and leaves the cache alone"
        );
        assert_eq!(
            fs::read_dir(&cache).unwrap().count(),
            0,
            "nothing was made in the cache"
        );
    }

    /// A FALLBACK ROOT WHOSE CHECKOUT IS GONE is removed when a run starts, under its
    /// own run lock, the checks a killed run left there ended first; one a run holds, one
    /// whose checkout is there, one with no record (or none this lane wrote) and the run's
    /// own stay.
    #[test]
    fn a_fallback_root_whose_checkout_is_gone_is_pruned() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = aterm_tempfile::Builder::new()
            .prefix("trust-lane-orphans")
            .tempdir_in(slots_base())
            .unwrap();
        let cache = d.path().join("cache");
        let make = |name: &str, record: Option<&Path>| {
            let dir = cache.join(record.map(checkout_key).unwrap_or_else(|| name.into()));
            owner_dirs(&dir.join("logs")).unwrap();
            if let Some(r) = record {
                fs::write(dir.join(CHECKOUT_RECORD), checkout_record(r)).unwrap();
                fs::set_permissions(dir.join(CHECKOUT_RECORD), fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            fs::write(dir.join(".run.lock"), "").unwrap();
            fs::set_permissions(dir.join(".run.lock"), fs::Permissions::from_mode(0o600)).unwrap();
            dir
        };
        let present = d.path().join("present");
        fs::create_dir(&present).unwrap();
        let own = make("own", Some(&d.path().join("own-gone")));
        let orphan = make("orphan", Some(&d.path().join("gone")));
        let live = make("live", Some(&present));
        let unrecorded = make("unrecorded", None);
        let relative = make("relative", Some(Path::new("relative/gone")));
        let held = make("held", Some(&d.path().join("also-gone")));
        let lock = RunLock::take(&held.join(".run.lock"), "a test holds it").unwrap();
        let wrong = make("wrong", Some(&d.path().join("wrong-gone")));
        let wrong_key = cache.join("not-the-recorded-checkout");
        fs::rename(wrong, &wrong_key).unwrap();
        let linked = make("linked", Some(&d.path().join("linked-gone")));
        let outside = d.path().join("outside-record");
        fs::write(&outside, checkout_record(&d.path().join("linked-gone"))).unwrap();
        let outside_before = fs::read(&outside).unwrap();
        fs::remove_file(linked.join(CHECKOUT_RECORD)).unwrap();
        std::os::unix::fs::symlink(&outside, linked.join(CHECKOUT_RECORD)).unwrap();
        assert!(write_checkout_record(&linked, &d.path().join("linked-gone")).is_err());
        assert_eq!(
            fs::read(&outside).unwrap(),
            outside_before,
            "no marker symlink is followed"
        );
        let missing_lock = make("no-lock", Some(&d.path().join("no-lock-gone")));
        fs::remove_file(missing_lock.join(".run.lock")).unwrap();
        let linked_lock = make("linked-lock", Some(&d.path().join("linked-lock-gone")));
        fs::remove_file(linked_lock.join(".run.lock")).unwrap();
        std::os::unix::fs::symlink(&outside, linked_lock.join(".run.lock")).unwrap();
        assert!(prepare_cache_lock(&linked_lock).is_err());
        assert_eq!(
            fs::read(&outside).unwrap(),
            outside_before,
            "no run-lock symlink is opened"
        );
        let linked_check = make("linked-check", Some(&d.path().join("linked-check-gone")));
        std::os::unix::fs::symlink(orphan.join(".check.0"), linked_check.join(".check.0")).unwrap();
        assert!(prepare_cache_lock(&linked_check).is_err());
        let weak = make("weak", Some(&d.path().join("weak-gone")));
        fs::set_permissions(&weak, fs::Permissions::from_mode(0o775)).unwrap();
        // A shared cache is never scanned for authority, even with an admitted own root.
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o775)).unwrap();
        assert!(prune_orphans(&cache, &own).is_empty());
        assert!(orphan.exists());
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
        // A check the orphan's killed run left running is ended with it.
        let mut check = command("/bin/sleep")
            .arg("600")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let pid = check.id();
        let started = proc_tree::started(pid).unwrap();
        fs::write(orphan.join(".check.0"), format!("{pid} {started}\n")).unwrap();
        fs::set_permissions(orphan.join(".check.0"), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            prune_orphans(&cache, &own),
            [(orphan.clone(), d.path().join("gone"))]
        );
        assert!(!orphan.exists());
        let deadline = Instant::now() + Duration::from_secs(60);
        let ended = loop {
            if let Some(status) = check.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                let _ = check.kill();
                let _ = check.wait();
                break None;
            }
            std::thread::yield_now();
        };
        assert!(ended.is_some(), "the orphan's check outlived its root");
        for kept in [
            &own,
            &live,
            &unrecorded,
            &relative,
            &held,
            &wrong_key,
            &linked,
            &missing_lock,
            &linked_lock,
            &linked_check,
            &weak,
        ] {
            assert!(kept.join("logs").is_dir(), "{}", kept.display());
        }
        drop(lock);
        assert_eq!(
            prune_orphans(&cache, &own),
            [(held, d.path().join("also-gone"))],
            "once its run lets go"
        );
    }

    /// A stand-in toolchain under `base` as targo lists it: launchers in `bin/`, the
    /// sysroot's `lib/`, a target's `lib/rustlib/<target>/lib` (beside a metadata file
    /// and a target with no `lib`), and the compiler's build `deps` beside the sysroot;
    /// every mode set explicitly (`0755`, a library `0644`), whatever the umask.
    fn toolchain(base: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let sysroot = base.join("trust-x");
        let dirs = [
            "trust-x",
            "trust-x/bin",
            "trust-x/lib",
            "trust-x/lib/rustlib",
            "trust-x/lib/rustlib/aarch64-test",
            "trust-x/lib/rustlib/aarch64-test/lib",
            "trust-x/lib/rustlib/src",
            "trust-x-rustc",
            "trust-x-rustc/aarch64-test",
            "trust-x-rustc/aarch64-test/release",
            "trust-x-rustc/aarch64-test/release/deps",
        ];
        for dir in dirs {
            fs::create_dir_all(base.join(dir)).unwrap();
            fs::set_permissions(base.join(dir), fs::Permissions::from_mode(0o755)).unwrap();
        }
        for (file, mode) in [
            ("trust-x/bin/targo", 0o755),
            ("trust-x/bin/trustc", 0o755),
            ("trust-x/bin/trustd", 0o755),
            ("trust-x/lib/libstd.so", 0o644),
            ("trust-x/lib/rustlib/components", 0o644),
            ("trust-x/lib/rustlib/aarch64-test/lib/libcore.rlib", 0o644),
            (
                "trust-x-rustc/aarch64-test/release/deps/librustc.rlib",
                0o644,
            ),
        ] {
            fs::write(base.join(file), file).unwrap();
            fs::set_permissions(base.join(file), fs::Permissions::from_mode(mode)).unwrap();
        }
        sysroot.join("bin")
    }

    /// THE TOOLCHAIN PRE-FLIGHT holds the prover to what `targo trust` admits before any
    /// check: a toolchain writable by its owner alone passes; a launcher, a component of
    /// its chain, a runtime-library directory or what one holds that a group could write
    /// is refused with `chmod go-w`, and a symlink where targo wants a plain object is
    /// refused.
    #[test]
    fn the_toolchain_is_held_to_what_targo_admits() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = aterm_tempfile::Builder::new()
            .prefix("trust-lane-toolchain")
            .tempdir_in(slots_base())
            .unwrap();
        let base = fs::canonicalize(base.path()).unwrap();
        let bin = toolchain(&base);
        assert_eq!(toolchain_refusal(&bin), None);
        for (part, open, kept) in [
            ("trust-x/bin/targo", 0o775, 0o755),
            ("trust-x/bin/trustc", 0o775, 0o755),
            ("trust-x/bin/trustd", 0o775, 0o755),
            ("trust-x/bin", 0o775, 0o755),
            ("trust-x/lib", 0o775, 0o755),
            ("trust-x/lib/libstd.so", 0o664, 0o644),
            ("trust-x/lib/rustlib/aarch64-test/lib", 0o775, 0o755),
            (
                "trust-x/lib/rustlib/aarch64-test/lib/libcore.rlib",
                0o664,
                0o644,
            ),
            ("trust-x-rustc/aarch64-test/release/deps", 0o775, 0o755),
        ] {
            let path = base.join(part);
            fs::set_permissions(&path, fs::Permissions::from_mode(open)).unwrap();
            let r = toolchain_refusal(&bin);
            fs::set_permissions(&path, fs::Permissions::from_mode(kept)).unwrap();
            let r = r.unwrap_or_else(|| panic!("{part} at {open:o} is admitted"));
            assert_eq!((&r.dir, r.chmod), (&path, GO_W), "{part}: {}", r.why);
            assert!(r.why.contains(&format!("{open:o}")), "{part}: {}", r.why);
        }
        // A verified tool launcher targo opens (targo, trustc) with a setuid, setgid or
        // sticky bit is refused, with the chmod that clears it; trustd's own check has no
        // such rule. (macOS forbids a non-root sticky file, and a setgid one outside the
        // file's group, so there only setuid is made.)
        let mut special_modes = vec![
            ("trust-x/bin/targo", 0o4755),
            ("trust-x/bin/trustc", 0o4755),
        ];
        if cfg!(target_os = "linux") {
            special_modes.extend([
                ("trust-x/bin/targo", 0o2755),
                ("trust-x/bin/trustc", 0o1755),
            ]);
        }
        for (part, special) in special_modes {
            let path = base.join(part);
            fs::set_permissions(&path, fs::Permissions::from_mode(special)).unwrap();
            let r = toolchain_refusal(&bin);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            let r = r.unwrap_or_else(|| panic!("{part} at {special:o} is admitted"));
            assert_eq!(
                (&r.dir, r.chmod),
                (&path, Some("a-st")),
                "{part}: {}",
                r.why
            );
            assert!(r.why.contains(&format!("{special:o}")), "{part}: {}", r.why);
        }
        let trustd = base.join("trust-x/bin/trustd");
        fs::set_permissions(&trustd, fs::Permissions::from_mode(0o4755)).unwrap();
        let r = toolchain_refusal(&bin);
        fs::set_permissions(&trustd, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(r, None, "trustd's executable check admits a setuid bit");
        let link = base.join("trust-x/lib/libstd-link.so");
        std::os::unix::fs::symlink("libstd.so", &link).unwrap();
        let r = toolchain_refusal(&bin).unwrap();
        fs::remove_file(&link).unwrap();
        assert_eq!((r.dir, r.chmod), (link, None), "{}", r.why);
        let target = base.join("trust-x/lib/rustlib/linked");
        std::os::unix::fs::symlink("aarch64-test", &target).unwrap();
        let r = toolchain_refusal(&bin).unwrap();
        fs::remove_file(&target).unwrap();
        assert_eq!((r.dir, r.chmod), (target, None), "{}", r.why);
        assert_eq!(toolchain_refusal(&bin), None);
        // The prover is refused before anything of it runs, with the fix.
        let lib = base.join("trust-x/lib");
        fs::set_permissions(&lib, fs::Permissions::from_mode(0o775)).unwrap();
        let why = Driver::at(&bin).map(|d| d.id).unwrap_err();
        assert!(
            says_in_order(
                &why,
                &[
                    &format!("refuses {} (mode 775", lib.display()),
                    &format!("run: chmod go-w {}", lib.display()),
                ]
            ),
            "{why}"
        );
    }

    #[test]
    fn one_run_holds_a_target_root() {
        let d = tmp("trust-lane-lock");
        let path = d.path().join(".run.lock");
        let first = RunLock::take(&path, "wait").unwrap();
        assert!(RunLock::take(&path, "wait").is_err());
        drop(first);
        assert!(
            RunLock::take(&path, "wait").is_ok(),
            "released when the run ends"
        );
    }

    #[test]
    fn the_exit_status_says_what_was_decided() {
        assert_eq!(Outcome::Pass.exit(), 0);
        assert_eq!(Outcome::Fail(vec!["a".into()]).exit(), 1);
        assert_eq!(Outcome::Undecided(vec!["a".into()]).exit(), 2);
        assert_eq!(Outcome::CouldNotRun("x".into()).exit(), 2);
        assert_eq!(Outcome::Narrowed(vec!["a".into()]).exit(), 4);
    }

    // -- process trees --------------------------------------------------------

    /// A check's tree is its descendants by parent, whatever their group; a trustd
    /// and what it runs are never in it.
    #[test]
    fn a_trees_members_spare_trustd_and_its_solvers() {
        let listing = "\
            100 1 /usr/bin/zsh\n\
            200 100 /store/trust/9192/bin/targo\n\
            201 200 /store/trust/9192/bin/targo-trust\n\
            202 201 /store/trust/9192/bin/trustc\n\
            300 200 /store/trust/9192/bin/trustd\n\
            301 300 /store/trust/9192/bin/z3 solver\n\
            400 1 /usr/libexec/other\n";
        let mut got = proc_tree::descendants(listing, 200);
        got.sort_unstable();
        assert_eq!(got, [200, 201, 202]);
    }

    #[test]
    fn ending_a_tree_never_signals_the_root_after_its_owned_wait_reaped_it() {
        let mut signals = Vec::new();
        proc_tree::end_tree_with(
            200,
            &mut || true,
            &mut || Some("200 1 targo\n201 200 trustc\n300 200 trustd\n301 300 z3\n".into()),
            &mut |pids, sig| signals.push((pids.to_vec(), sig)),
        );
        let (last, signal) = signals.last().unwrap();
        assert_eq!(*signal, proc_tree::SIGKILL);
        assert_eq!(
            last,
            &[201],
            "the descendant still needs cleanup, the reaped root is no longer ours"
        );
        assert!(
            signals
                .iter()
                .all(|(pids, _)| !pids.contains(&300) && !pids.contains(&301))
        );
    }

    /// The tree is ended to its last process, a grandchild in a session and group of
    /// its own included (a signal to the check's group misses it).
    #[test]
    fn ending_a_check_ends_its_whole_tree() {
        let pgid = |pid: u32| {
            let o = command("/bin/ps")
                .args(["-o", "pgid=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u32>()
                .ok()
        };
        let d = tmp("trust-lane-tree");
        let pidfile = d.path().join("grandchild.pid");
        let mut cmd = command("/bin/sh");
        cmd.arg("-c")
            .arg(
                "/usr/bin/perl -MPOSIX -e 'POSIX::setsid() or die; exec @ARGV' /bin/sleep 600 & \
                 echo $! > \"$1\"; wait",
            )
            .arg("sh")
            .arg(&pidfile)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let child = cmd.spawn().unwrap();
        let root = child.id();
        let mut check = RealCheck {
            child,
            pid: root,
            record: d.path().join(".check.0"),
            ended: None,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        let grandchild: u32 = loop {
            if let Some(p) = fs::read_to_string(&pidfile)
                .ok()
                .and_then(|t| t.trim().parse().ok())
                && pgid(p) == Some(p)
            {
                break p;
            }
            assert!(
                Instant::now() < deadline,
                "the grandchild never led a group of its own"
            );
            std::thread::yield_now();
        };
        assert_eq!(pgid(root), Some(root));
        assert_ne!(
            pgid(grandchild),
            pgid(root),
            "the case: a group signal would miss it"
        );
        assert_eq!(check.poll().unwrap(), None);
        let ended = check.end();
        assert!(ended.code > 128, "{ended:?}");
        assert!(ended.maxrss > 0 && ended.cpu > Duration::ZERO, "{ended:?}");
        assert_eq!(check.poll().unwrap(), Some(ended));
        assert_eq!(check.end(), ended);
        let deadline = Instant::now() + Duration::from_secs(60);
        while proc_tree::started(grandchild).is_some() {
            assert!(
                Instant::now() < deadline,
                "the grandchild outlived its check"
            );
            std::thread::yield_now();
        }
    }

    // -- trustd ----------------------------------------------------------------

    /// A stand-in trustd on a socket of its own: answers IDENTITY with `sha`, STATUS,
    /// and RESERVE with a grant it holds until the client goes.
    fn stand_in(sock: &Path, sha: &'static str) -> std::thread::JoinHandle<()> {
        use std::io::{BufRead as _, BufReader as Reader};
        let listener = std::os::unix::net::UnixListener::bind(sock).unwrap();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { return };
                std::thread::spawn(move || {
                    let mut w = conn.try_clone().unwrap();
                    for line in Reader::new(conn).lines() {
                        let Ok(line) = line else { return };
                        let reply = match line.split_whitespace().next() {
                            Some("IDENTITY") => format!(
                                "{{\"executable_sha256\":\"{sha}\",\"release\":\"1.99\",\"commit\":\"abc\"}}"
                            ),
                            Some("STATUS") => "{\"active\":[]}".into(),
                            Some("RESERVE") => "GRANTED 1".into(),
                            _ => "ERR".into(),
                        };
                        if writeln!(w, "{reply}").is_err() {
                            return;
                        }
                    }
                });
            }
        })
    }

    fn short_dir(tag: &str) -> aterm_tempfile::TempDir {
        // A unix socket's path must fit in 104 bytes.
        aterm_tempfile::Builder::new()
            .prefix(&format!("tl{tag}-"))
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[test]
    fn the_probe_tells_this_builds_trustd_from_another() {
        let private = short_dir("p");
        let d = private.path();
        let wait = Duration::from_secs(10);
        let none = trustd::probe(&d.join("absent.sock"), "aa", wait);
        assert_eq!(none.held, trustd::Held::None);
        let ours = d.join("ours.sock");
        let _a = stand_in(&ours, "aa");
        assert_eq!(trustd::probe(&ours, "aa", wait).held, trustd::Held::Ours);
        let other = trustd::probe(&ours, "bb", wait);
        assert_eq!(other.held, trustd::Held::Foreign);
        assert!(other.line().starts_with("foreign\t"), "{}", other.line());
        // one that takes the connection and never answers
        let mute = d.join("mute.sock");
        let listener = std::os::unix::net::UnixListener::bind(&mute).unwrap();
        let _held = std::thread::spawn(move || {
            let conns: Vec<_> = listener.incoming().take(1).collect();
            std::thread::park_timeout(Duration::from_secs(60));
            drop(conns);
        });
        let silent = trustd::probe(&mute, "aa", Duration::from_millis(200));
        assert_eq!(silent.held, trustd::Held::Silent);
    }

    #[test]
    fn the_keeper_holds_a_grant_on_this_builds_trustd_only() {
        let private = short_dir("k");
        let d = private.path();
        let sock = d.join("k.sock");
        let _t = stand_in(&sock, "aa");
        let grants = d.join("grants");
        let seen = d.join("seen");
        let keeper = trustd::Keeper::start(sock.clone(), "aa".into(), grants.clone(), seen.clone());
        let deadline = Instant::now() + Duration::from_secs(60);
        while !keeper.held() {
            assert!(Instant::now() < deadline, "the keeper took no grant");
            std::thread::yield_now();
        }
        assert_eq!(fs::read_to_string(&grants).unwrap(), "1\n");
        drop(keeper);
        let other = trustd::Keeper::start(sock, "bb".into(), grants, seen.clone());
        let deadline = Instant::now() + Duration::from_secs(60);
        while !seen.exists() {
            assert!(
                Instant::now() < deadline,
                "the keeper said nothing of another build"
            );
            std::thread::yield_now();
        }
        assert!(!other.held(), "no grant on another build's trustd");
        drop(other);
    }

    /// Dropping the keeper closes its outstanding IDENTITY connection without
    /// needing a reply or a close from the peer.
    #[test]
    fn the_keeper_ends_while_a_trustd_keeps_it_waiting() {
        use std::io::{BufRead as _, Read as _};

        let private = short_dir("m");
        let d = private.path();
        let sock = d.join("m.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mute = std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            conn.set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            let mut reader = BufReader::new(conn);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert_eq!(request, "IDENTITY\n");
            tx.send(reader.into_inner()).unwrap();
        });
        let keeper = trustd::Keeper::start(sock, "aa".into(), d.join("grants"), d.join("seen"));
        let mut peer = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the keeper never requested IDENTITY");
        mute.join().unwrap();

        // Keep the silent peer open until the keeper has ended. These waits
        // detect a hang; no elapsed-time threshold is a performance assertion.
        let (ended_tx, ended_rx) = std::sync::mpsc::channel();
        let ending = std::thread::spawn(move || {
            drop(keeper);
            ended_tx.send(()).unwrap();
        });
        assert_eq!(
            peer.read(&mut [0u8; 1]).expect("the keeper never closed"),
            0,
            "the keeper sent another request instead of closing"
        );
        ended_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the keeper never ended after closing its connection");
        ending.join().unwrap();
        drop(peer);
    }
}
