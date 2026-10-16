// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TRUST LANE: `targo --unverified ship trust run`.
//!
//! Runs `targo trust check -p <library> --lib --allow-l0-gaps` over the workspace
//! libraries and the vendored forks, each check in a slot target directory of its
//! own, and hands what each one left to the judge ([`crate::trust_facts`]): a
//! whole-tree run on a clean checkout files the facts a release reads.
//!
//! What the lane guarantees, and why:
//! - every check runs in an environment built from a list ([`lane_env`]): a
//!   caller's `TRUSTFLAGS` or `RUSTFLAGS` would move a proof the facts key does not
//!   cover;
//! - the prover is resolved once ([`Driver`]): a store update mid-run would split the
//!   run across two provers;
//! - a check runs in a session of its own and is ended with its whole process tree,
//!   trustd's left alone ([`proc_tree::end_tree`]): targo-trust starts its build in a
//!   group of its own, which a group signal orphans, and a signalled trustd records
//!   DIRTY and blocks every later check;
//! - each check has a wall budget ([`Config::budget`]): a verifier stuck before its
//!   solvers spins for hours, and a check that does not finish is not verified;
//! - a second check starts only while this build's trustd serves the socket
//!   ([`trustd::Keeper`]): two checks that each start a trustd race, and the loser is
//!   refused; the keeper's grant also keeps trustd from its idle exit while a long
//!   compile runs before the first solver call;
//! - nothing is retried: a check targo would not start is a NOT-VERIFIED(trustd) row,
//!   undecided, and `--remeasure` measures it again.

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
/// leaves the graph is FORK-VANISHED ([`vanished`]).
pub const QUEUED_FORKS: &[(&str, &str)] = &[(
    "indexmap",
    "its dev-dependency quickcheck closes a cycle onto the patched indexmap",
)];

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

// ---------------------------------------------------------------------------
// What a run is asked, and what it answers
// ---------------------------------------------------------------------------

/// Which libraries a run verifies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every library and fork: the run that files facts for a release.
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
    /// Where the slots' target directories and the logs go; `None` is
    /// `<repo>/target-trust`.
    pub target_root: Option<PathBuf>,
    pub budget: Duration,
    /// Write floors this run meets (never lowering one) and keep the memory record
    /// current.
    pub update_ratchet: bool,
    /// Verify what this host would refuse for memory up front.
    pub try_refused: bool,
    /// Reduce each check's proof report to its per-obligation record (the candidate
    /// gate's comparison reads it).
    pub obligations: bool,
    /// Stop before any check when this host refuses a library for memory.
    pub stop_if_narrowed: bool,
    /// The prover's `bin/`; `None` is the toolchain the cut builds with.
    pub prover: Option<PathBuf>,
    /// Where the cost history and this host's memory observations live; `None` is
    /// `<git common dir>/aterm-release`.
    pub state: Option<PathBuf>,
    pub trustd_wait: Duration,
    /// End a check that crosses one of these bounds (a toolchain with no verifier
    /// memory ceiling can take the machine down).
    pub guard: Option<Guard>,
}

/// THE MEMORY GUARD ([`guard`]): a check is ended when its process tree's memory, the
/// swap in use, the memory pressure level or the free space under the target root
/// crosses its bound. A check it ends ended by a signal, so its library has no result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guard {
    pub tree_bytes: u64,
    pub swap_bytes: u64,
    /// The `kern.memorystatus_level` under which a check is ended (macOS; 0 never).
    pub level_min: u32,
    pub free_bytes: u64,
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
            guard: None,
        }
    }

    fn root(&self) -> PathBuf {
        self.target_root
            .clone()
            .unwrap_or_else(|| self.repo.join("target-trust"))
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

    /// The lines that say why.
    #[must_use]
    pub fn detail(&self) -> Vec<String> {
        match self {
            Outcome::Pass => Vec::new(),
            Outcome::Fail(v) | Outcome::Undecided(v) | Outcome::Narrowed(v) => v.clone(),
            Outcome::CouldNotRun(why) => vec![why.clone()],
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

// ---------------------------------------------------------------------------
// The prover
// ---------------------------------------------------------------------------

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
    /// `trustc` and `trustd` beside it.
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

/// THE LANE'S ENVIRONMENT: the pins, then what it passes from the host (`host`), the
/// driver's directory first on PATH so a nested `trustc`, `trustd` or `trustdoc` is the
/// pinned build's, and the system's after it so a build script's `cc` is the system's.
/// The caller adds each compile's `CARGO_TARGET_DIR`.
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

// ---------------------------------------------------------------------------
// The gate list, from the resolved graph
// ---------------------------------------------------------------------------

/// What a package of the graph is to the lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A workspace member with a library: `-p name@version --lib`, and once more
    /// per fork feature of its own ([`fork_features`]) as `<name>+<feature>`.
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
    /// ([`FEATURE_ROW`]). A row of a package whose name another package of the
    /// graph also carries is named by its [`Self::spec`] instead (a TWIN:
    /// `windows-sys@0.52.0` and `windows-sys@0.61.2`, the two facades), so each
    /// has a floor row of its own and neither is judged against the other's.
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
/// (`aterm-link+sealed`). A package name holds only alphanumerics, `-` and `_`, so
/// a name with it is always such a row, and the part before it is the package.
pub const FEATURE_ROW: char = '+';

/// The Cargo package named by a lane row. A feature suffix identifies the
/// lane's build and a version suffix a twin ([`Lib::name`]), while Cargo reports
/// still name the underlying package.
#[must_use]
pub fn package_identity(row: &str) -> &str {
    row.split_once([FEATURE_ROW, '@'])
        .map_or(row, |(package, _)| package)
}

/// THE MEMBER CODE BEHIND A FORK'S FEATURE: each feature of `pkg`'s own that the
/// default build leaves off and that names a path fork directly (`fork/feat`,
/// `fork?/feat` or `dep:fork`, by the name the manifest gives the dependency).
/// The fork's lane verifies the fork code such a feature turns on (the graph is
/// every feature's: [`gate_list`]); the member's arms that compile only with it
/// are that code's call sites, which `-p <member> --lib` never compiles. Each one
/// is gated as a row of its own, the default row unchanged: `cfg(not(..))` arms
/// ship, and the feature's do not.
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
    // What the default build turns on: `default` and every feature it names, on down.
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

/// THE GATE LIST, from `targo metadata --format-version 1 --all-features` with
/// dependencies (a fork is found only in the resolve): members first, then forks, each
/// by name, a fork with the features that resolve turns on for it, a member once with
/// its default features and once more per fork feature ([`fork_features`]). `rows`
/// answers the dev package rows a manifest's check sees.
///
/// # Errors
/// Metadata the lane cannot read a gate list from, or a name or spec that cannot go in
/// an argv.
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
    // TWINS: two packages of one name (a member and a path fork at another
    // version) would share a floor row, which the selection refuses as
    // AMBIGUOUS; each is named by its spec instead, so each has its own.
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

/// THE LIBRARIES THAT LEFT: each name a floor row (`floors`, the ratchet's text) or
/// [`QUEUED_FORKS`] holds that `libs` does not, sorted. A deleted `[patch]` row would
/// otherwise stop a fork being counted with nothing said; the judge fails each as
/// FORK-VANISHED, and its floor row goes with it.
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

/// THE SELECTION: `names` (all of `libs` when `None`), each decided — two selected
/// packages of one name are AMBIGUOUS (one floor cannot judge both), a member with no
/// library NO-LIB, a fork [`QUEUED_FORKS`] names QUEUED, the rest gated. A member's
/// name selects its fork-feature rows too: they are its library, in another build.
///
/// # Errors
/// A name that is in neither the workspace nor the graph's forks: a run over nothing it
/// asked for must not print a verdict.
pub fn select(libs: &[Lib], names: Option<&[String]>) -> Result<Vec<Selected>, String> {
    let chosen: Vec<&Lib> = match names {
        None => libs.iter().collect(),
        Some(names) => {
            let mut out = Vec::new();
            for want in names {
                // A package's name selects its fork-feature rows and, for a
                // twin, both versions; a twin's spec selects that one.
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

// ---------------------------------------------------------------------------
// Slots, order and the budget
// ---------------------------------------------------------------------------

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
}

const COSTS_HEADER: &str = "# trust lane cost history: <name> <CPU s> <wall s> <largest process's peak RSS bytes> <when>; the slots take the longest first, a library with no row first of all";

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
                },
            ))
        })
        .collect()
}

/// THE ORDER: libraries with no cost first (one could be the longest of all), in the
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
/// own path: a fork resolves on its own, so its closure key reads its registry
/// versions there, never in the root lock.
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
    /// Why `job`'s check must end now though it is inside its budget (the memory
    /// guard), if it must.
    fn guard(&mut self, job: &Job) -> Option<String>;
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
    /// The memory guard ended it, and why.
    pub guarded: Option<String>,
}

/// Why the slots stopped before their queue was done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stopped {
    Interrupted,
    /// A check could not be started at all.
    Unstartable(String),
}

/// THE SLOTS: run `jobs` (already in their order, each with its slot unset) on `slots`
/// slots, at most `heavy` heavy ones at once, each ended at `budget`; a second check
/// starts only while [`Checks::trustd_held`]. `done` sees each check as it ends, before
/// its slot is reused (its dep-info is the slot's newest).
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
                Ok(Some(e)) => Some((e, false, None)),
                Ok(None) if over => Some((running[i].check.end(), true, None)),
                Ok(None) => checks
                    .guard(&running[i].job)
                    .map(|why| (running[i].check.end(), false, Some(why))),
                Err(_) => Some((running[i].check.end(), false, None)),
            };
            if let Some((ended, over_budget, guarded)) = ended {
                let r = running.swap_remove(i);
                free.push(r.job.slot);
                done(
                    Finished {
                        wall: checks.now().saturating_sub(r.since),
                        job: r.job,
                        ended,
                        over_budget,
                        guarded,
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
/// budget that ended it, what the trustd socket held when targo would not start it, the
/// dep-info its compiles wrote (each `.d` file in its slot at least as new as its
/// start), and a fork's lockfile (`<log>.lock`, [`ForkLock`]), which then goes if the
/// check created it.
///
/// # Errors
/// A mark the judge needs could not be written.
pub fn record(
    f: &Finished,
    budget: Duration,
    started: SystemTime,
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
    let mut d = Vec::new();
    deps_newer(&f.job.target, started, &mut d);
    d.sort();
    let listing: String = d.iter().map(|p| format!("{}\n", p.display())).collect();
    fs::write(beside("depinfo"), listing)?;
    if let Some(lock) = &f.job.lock {
        let kept = if lock.path.is_file() {
            fs::copy(&lock.path, beside("lock")).map(drop)
        } else {
            Ok(())
        };
        if lock.created {
            let _ = fs::remove_file(&lock.path);
        }
        kept?;
    }
    Ok(())
}

fn deps_newer(dir: &Path, since: SystemTime, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            deps_newer(&p, since, out);
        } else if p.extension().is_some_and(|x| x == "d")
            && p.parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == "deps")
            && e.metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|m| m >= since)
        {
            out.push(p);
        }
    }
}

// ---------------------------------------------------------------------------
// A run
// ---------------------------------------------------------------------------

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
        // THE GRAPH, from the pinned driver: a graph another toolchain resolved could
        // gate members this targo resolves otherwise. EVERY FEATURE'S graph: a path
        // package reachable only through an off-by-default feature (astream-aead, under
        // aterm's `sealed`) is still forked code this tree carries and builds, so it is
        // discovered and decided like any fork, and a fork's lane verifies the feature
        // union a full build can turn on. Members are gated by `-p` with their default
        // features, and again per feature of their own that turns on fork code
        // (`fork_features`: aterm-link+sealed). It is also the graph the provenance
        // attest reads (aterm-forge direct_vendor).
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

    fn checks(
        &mut self,
        cfg: &Config,
        prep: &Prepared,
        jobs: Vec<Job>,
        slots: usize,
        done: &mut dyn FnMut(Finished, &mut dyn Checks),
        out: &mut dyn Write,
    ) -> Result<(), String> {
        reap_stale_checks(&prep.root);
        clear_slots(&prep.root);
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
            guard: cfg.guard,
            pids: BTreeMap::new(),
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

/// RUN THE LANE (`ship trust run`): progress and the judge's report go to `out`, and a
/// run that decides nothing says why (`trust: COULD NOT RUN — …`).
pub fn run(cfg: &Config, out: &mut dyn Write) -> Outcome {
    crate::release_steps::interrupt::install();
    run_on(cfg, &mut Machine, out)
}

fn run_on(cfg: &Config, bench: &mut dyn Bench, out: &mut dyn Write) -> Outcome {
    let ran = match prepare(cfg, bench, out) {
        Ok(prep) => {
            let judged = check_and_conclude(cfg, &prep, bench, out).map_err(Stop::CouldNotRun);
            clear_slots(&prep.root);
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
    let root = cfg.root();
    let root = if root.is_absolute() {
        root
    } else {
        cfg.repo.join(root)
    };
    if let Some((dir, why)) = refused_component(&root) {
        return Err(Stop::CouldNotRun(format!(
            "the target root {} has a path component, {}, that `targo trust` refuses ({why}): \
             give --target-root a directory whose every component is owned by root or you and \
             writable by its owner alone",
            root.display(),
            dir.display()
        )));
    }
    let logdir = root.join("logs");
    fs::create_dir_all(&logdir).map_err(|e| format!("{}: {e}", logdir.display()))?;
    let lock = RunLock::take(
        &root.join(".run.lock"),
        "give this one its own --target-root",
    )?;
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
    let _ = fs::write(logdir.join("metadata-targo.json"), &meta_out);
    let meta: aterm_json::Value = aterm_json::from_str(&String::from_utf8_lossy(&meta_out))
        .map_err(|e| format!("targo metadata printed no JSON: {e}"))?;
    let repo = cfg.repo.clone();
    let libs = gate_list(&meta, &|m| profile_rows_for(m, &repo))?;
    // Only a run that files needs the prover's identity: a hash of the whole toolchain.
    let files = matches!(cfg.scope, Scope::WholeTree | Scope::Remeasure);
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
        ("per-commit", "0".into()),
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
        ("crate-budget", cfg.budget.as_secs().to_string()),
        ("obligations", flag(cfg.obligations).into()),
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
    // MEMORY: a library whose floor this host's verifier ceiling cannot reproduce is
    // refused up front, never run (the judge's `admit`).
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
        for ext in [
            "",
            ".rc",
            ".trustd",
            ".depinfo",
            ".lock",
            ".budget",
            ".oblig.tsv",
            ".oblig.err",
        ] {
            let _ = fs::remove_file(format!("{}{ext}", log.display()));
        }
        let _ = fs::remove_dir_all(format!("{}.report", log.display()));
    }
    let costs = prep
        .costs_path
        .as_ref()
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|t| parse_costs(&t))
        .unwrap_or_default();
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
    let since = SystemTime::now();
    let since_secs = since
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
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
        let mut ended_by_guard: Vec<String> = Vec::new();
        let mut done = |f: Finished, checks: &mut dyn Checks| {
            if let Some(why) = &f.guarded {
                ended_by_guard.push(format!(
                    "trust: the memory guard ended {}: {why}",
                    f.job.name
                ));
            }
            let started = SystemTime::now()
                .checked_sub(f.wall)
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let mut probe = || checks.probe_trustd();
            let _ = record(&f, budget, started, &mut probe);
            if let Some(r) = &reducer {
                r.send(&f.job);
            }
            if let Some(c) = finished_cost(&f) {
                new_costs.insert(f.job.name.clone(), c);
            }
        };
        let ran = bench.checks(cfg, prep, jobs, slots, &mut done, out);
        for l in &ended_by_guard {
            let _ = writeln!(out, "{l}");
        }
        // Every record is written before the judge reads one; a run that decided
        // nothing reduces no more.
        if let Some(r) = reducer {
            r.finish(ran.is_err());
        }
        // What a finished check cost is so however the run ended.
        write_costs(prep.costs_path.as_deref(), &new_costs, out);
        ran?;
    }
    // PASS 3: the filing's part, then the judge.
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
        let meta = prep.logdir.join("metadata-targo.json");
        push_kv(&mut plan, "metadata", &meta.display().to_string());
        for s in &work {
            plan.push_str(&format!("argv {}\t{}\n", s.lib.name, s.lib.argv.join(" ")));
        }
        for (k, v) in [
            ("since", since_secs.to_string()),
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
/// report of gigabytes never holds the slots (no check polled, held to its budget or
/// guarded meanwhile).
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
    (!m.summary.is_empty() && m.ended.is_empty() && !m.transport).then_some(Cost {
        cpu: f.ended.cpu.as_secs(),
        wall: f.wall.as_secs(),
        maxrss: f.ended.maxrss,
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
    let when = utc_now();
    let mut text = format!("{COSTS_HEADER}\n");
    for (name, c) in &all {
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

/// `--obligations`: the check's proof report reduced to its per-obligation record
/// beside its log, or why not; then the report goes (it is large).
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
            format!("{why}\n"),
        );
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
fn clear_slots(root: &Path) {
    if let Ok(rd) = fs::read_dir(root) {
        for e in rd.flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if n.strip_prefix('j')
                .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
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

/// A path component `targo trust` refuses for a verified runtime-library directory
/// under `root` (one a principal other than the caller could swap): owned by neither
/// root nor the caller, or group- or world-writable. Each logical ancestor of the
/// deepest existing one is checked along its whole physical chain, since targo holds
/// the directory that holds each symlink to the same rule.
pub(crate) fn refused_component(root: &Path) -> Option<(PathBuf, String)> {
    use std::os::unix::fs::MetadataExt as _;
    let me = proc_tree::euid();
    let mut p = root.to_path_buf();
    while !p.is_dir() {
        if !p.pop() {
            return None;
        }
    }
    for logical in p.ancestors() {
        let Ok(phys) = fs::canonicalize(logical) else {
            continue;
        };
        for dir in phys.ancestors() {
            let Ok(md) = fs::metadata(dir) else { continue };
            if let Some(why) =
                component_refusal(md.uid(), md.mode(), me, || proc_tree::writable(dir))
            {
                return Some((dir.to_path_buf(), why));
            }
        }
    }
    None
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

/// ONE RUN PER TARGET ROOT: a run empties the root's slots and ends what a killed run
/// left there, so a second run on the same root would wreck the first.
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

// ---------------------------------------------------------------------------
// The real checks
// ---------------------------------------------------------------------------

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
    guard: Option<Guard>,
    /// Each slot's running check, and when the guard last measured it.
    pids: BTreeMap<usize, (u32, Instant)>,
}

impl Checks for RealChecks<'_> {
    fn start(&mut self, job: &mut Job) -> std::io::Result<Box<dyn Check>> {
        let target = self.root.join(format!("j{}", job.slot));
        fs::create_dir_all(&target)?;
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
        self.pids.insert(job.slot, (pid, Instant::now()));
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

    fn guard(&mut self, job: &Job) -> Option<String> {
        let g = self.guard?;
        let (pid, last) = self.pids.get_mut(&job.slot)?;
        if last.elapsed() < guard::EVERY {
            return None;
        }
        *last = Instant::now();
        guard::crossed(&g, *pid, self.root)
    }

    fn probe_trustd(&mut self) -> String {
        trustd::probe(self.sock, self.want, Duration::from_secs(10)).line()
    }

    fn interrupted(&self) -> bool {
        crate::release_steps::interrupt::caught().is_some()
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

// ---------------------------------------------------------------------------
// Process trees
// ---------------------------------------------------------------------------

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

    /// END `root`'s WHOLE TREE, trustd's left out: stopped (listed again once stopped,
    /// so nothing forked meanwhile is missed), sent SIGTERM and SIGCONT, given a second
    /// to go (`gone` may reap `root`), then SIGKILLed. Once reaped, the root is never
    /// signalled again: its pid may already name another process.
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

pub mod guard {
    //! THE MEMORY GUARD's measurements: a check's process tree's memory (macOS's
    //! physical footprint, which counts the compressed and swapped pages a resident set
    //! leaves out exactly when the machine is under pressure; Linux's resident set),
    //! the swap in use, macOS's memory pressure level, and the free space under the
    //! target root.

    use std::path::Path;
    use std::time::Duration;

    use super::Guard;

    /// How often a running check is measured.
    pub const EVERY: Duration = Duration::from_secs(2);

    /// What crossed `g` for the check at `pid`, if anything did.
    #[must_use]
    pub fn crossed(g: &Guard, pid: u32, root: &Path) -> Option<String> {
        let tree = i32::try_from(pid).ok().and_then(tree_bytes);
        verdict(
            g,
            Reading {
                tree,
                swap: swap_used(),
                level: pressure_level(),
                free: free_bytes(root),
            },
        )
    }

    /// One measurement; `None` where it could not be read.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Reading {
        pub tree: Option<u64>,
        pub swap: Option<u64>,
        /// `kern.memorystatus_level`: the percentage of memory the kernel counts free.
        pub level: Option<u32>,
        pub free: Option<u64>,
    }

    /// The bound a reading crossed, said; a measurement that could not be read
    /// crosses nothing.
    #[must_use]
    pub fn verdict(g: &Guard, r: Reading) -> Option<String> {
        let gib = |b: u64| b >> 30;
        if let Some(t) = r.tree.filter(|t| *t > g.tree_bytes) {
            return Some(format!(
                "its process tree held {} GiB, past {} GiB",
                gib(t),
                gib(g.tree_bytes)
            ));
        }
        if let Some(s) = r.swap.filter(|s| *s > g.swap_bytes) {
            return Some(format!(
                "{} GiB of swap in use, past {} GiB",
                gib(s),
                gib(g.swap_bytes)
            ));
        }
        if let Some(l) = r.level.filter(|l| *l < g.level_min) {
            return Some(format!("memorystatus_level {l}, under {}", g.level_min));
        }
        r.free.filter(|f| *f < g.free_bytes).map(|f| {
            format!(
                "{} GiB free under the target root, under {} GiB",
                gib(f),
                gib(g.free_bytes)
            )
        })
    }

    fn run(program: &str, args: &[&str]) -> Option<String> {
        aterm_uds::spawnfd::command(program)
            .args(args)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    }

    /// The memory of `root`'s whole tree (trustd's left out), bytes.
    fn tree_bytes(root: i32) -> Option<u64> {
        let listing = run("/bin/ps", &["-A", "-o", "pid=,ppid=,comm="])?;
        Some(
            super::proc_tree::descendants(&listing, root)
                .into_iter()
                .filter_map(process_bytes)
                .sum(),
        )
    }

    /// A process's physical footprint (`proc_pid_rusage`'s `ri_phys_footprint`).
    #[cfg(target_os = "macos")]
    fn process_bytes(pid: i32) -> Option<u64> {
        /// `RUSAGE_INFO_V4`.
        const FLAVOR: i32 = 4;
        /// `ri_phys_footprint`'s place in `struct rusage_info_v4`, in u64s: after the
        /// 16-byte uuid and seven counters.
        const PHYS_FOOTPRINT: usize = 9;
        unsafe extern "C" {
            fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u64) -> i32;
        }
        // Larger than `struct rusage_info_v4`.
        let mut buf = [0u64; 64];
        // SAFETY: `buf` is live, writable and larger than the struct the flavor names.
        let rc = unsafe { proc_pid_rusage(pid, FLAVOR, buf.as_mut_ptr()) };
        (rc == 0).then(|| buf[PHYS_FOOTPRINT])
    }

    /// A process's resident set (`VmRSS`).
    #[cfg(not(target_os = "macos"))]
    fn process_bytes(pid: i32) -> Option<u64> {
        let t = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let kib: u64 = t
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()?;
        Some(kib * 1024)
    }

    /// macOS's `kern.memorystatus_level`; `None` elsewhere.
    fn pressure_level() -> Option<u32> {
        run("/usr/sbin/sysctl", &["-n", "kern.memorystatus_level"])?
            .trim()
            .parse()
            .ok()
    }

    /// Swap in use, bytes: macOS's `vm.swapusage`, else Linux's meminfo.
    fn swap_used() -> Option<u64> {
        if let Some(t) = run("/usr/sbin/sysctl", &["-n", "vm.swapusage"]) {
            return parse_swapusage(&t);
        }
        let t = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kib = |key: &str| {
            t.lines()
                .find_map(|l| l.strip_prefix(key))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        };
        Some(kib("SwapTotal:")?.saturating_sub(kib("SwapFree:")?) * 1024)
    }

    /// `total = 2048.00M  used = 1035.25M  free = 1012.75M  (encrypted)` -> the used
    /// bytes (to the hundredth of the unit).
    #[must_use]
    pub fn parse_swapusage(text: &str) -> Option<u64> {
        let used = text.split("used = ").nth(1)?.split_whitespace().next()?;
        let (n, unit) = used.split_at(used.find(|c: char| c.is_ascii_alphabetic())?);
        let unit: u64 = match unit {
            "K" => 1 << 10,
            "M" => 1 << 20,
            "G" => 1 << 30,
            _ => return None,
        };
        let (whole, frac) = n.split_once('.').unwrap_or((n, ""));
        let frac = format!("{frac:0<2}");
        let hundredths = whole.parse::<u64>().ok()? * 100 + frac.get(..2)?.parse::<u64>().ok()?;
        Some(hundredths * unit / 100)
    }

    /// Free bytes on the volume holding `dir` (`df -Pk`).
    #[must_use]
    pub fn free_bytes(dir: &Path) -> Option<u64> {
        parse_df(&run("/bin/df", &["-Pk", &dir.display().to_string()])?)
    }

    /// The available KiB of `df -Pk`'s one row, as bytes.
    #[must_use]
    pub fn parse_df(text: &str) -> Option<u64> {
        let kib: u64 = text
            .lines()
            .nth(1)?
            .split_whitespace()
            .nth(3)?
            .parse()
            .ok()?;
        Some(kib * 1024)
    }

    /// This process's own footprint: a measurement of a process that is surely there.
    #[cfg(test)]
    #[must_use]
    pub fn own_bytes() -> Option<u64> {
        i32::try_from(std::process::id())
            .ok()
            .and_then(process_bytes)
    }
}

// ---------------------------------------------------------------------------
// trustd, Trust's per-user memory jobserver
// ---------------------------------------------------------------------------

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
    /// or nothing (a trustd retires 300 s after its last grant).
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
            if Instant::now() >= until || crate::release_steps::interrupt::caught().is_some() {
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

    /// THE KEEPER: while it lives, a one-byte grant on this build's trustd, so trustd
    /// does not go idle while a check compiles; [`Keeper::held`] says the grant is live.
    /// It never starts or signals a trustd; it writes how many grants it took to
    /// `grants`, and what held the socket instead to `seen`. Dropping it ends at once:
    /// it shuts the connection a slow trustd keeps it waiting on.
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
        /// Name `stream` as the one to shut; false once the keeper is stopping (its
        /// drop has shut whatever was named, so this one is never waited on).
        fn name(&self, stream: &UnixStream) -> bool {
            let mut c = self
                .conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *c = stream.try_clone().ok();
            !self.stop.load(Ordering::SeqCst)
        }
    }

    fn keep(sock: &Path, want: &str, grants_out: &Path, seen_out: &Path, k: &Kept) {
        let (stop, held) = (&*k.stop, &*k.held);
        let mut grants = 0u32;
        let tick = Duration::from_secs(2);
        while !stop.load(Ordering::SeqCst) {
            if let Ok(stream) = UnixStream::connect(sock) {
                if !k.name(&stream) {
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

    /// THE DERIVATION: a member by `-p name@version --lib`, a bin-only member with
    /// nothing to check, a fork by its own manifest with the workspace's feature
    /// union, registry packages never; members before forks; the lane profile first.
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

    /// TWINS: a member and a path fork of one name at two versions are each
    /// named by their spec, so each is judged against its own floor row; the
    /// package's name selects both, a spec selects one, and neither is AMBIGUOUS.
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

    /// THE LANE COMPILES ITS SUBJECT UNOPTIMIZED: every dev speed row the manifest
    /// names is zeroed by name (a named row outranks `package."*"`); with none, only
    /// the profile itself.
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

    /// A MEMBER FEATURE THAT TURNS ON FORK CODE is gated as a row of its own, beside
    /// the default row: `sealed` names the fork `bus` and `framed` the fork `wire`
    /// through its renamed, optional dependency key. A feature the default build
    /// turns on (`std`), one that names no package (`dev-seams`) and one that
    /// names a registry package (`derive`) get none. `-p link` selects every row
    /// of link's library; `-p link+sealed` that row alone. A floor row whose
    /// feature left the graph is FORK-VANISHED, like any library that left.
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
            lib("indexmap", "indexmap@2.0.0", Kind::Fork),
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
                ("indexmap", &Decision::Queued("queued".into())),
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

    /// A FLOOR OR A QUEUED FORK THE GRAPH NO LONGER CARRIES is named, never dropped
    /// with nothing said; one the graph carries, and a comment, are not.
    #[test]
    fn a_floor_the_graph_no_longer_carries_is_vanished() {
        let libs = vec![
            lib("alpha", "alpha@1.0.0", Kind::Member),
            lib("smol_str", "smol_str@0.3.2", Kind::Fork),
            lib("indexmap", "indexmap@2.0.0", Kind::Fork),
        ];
        let floors = "# <crate>\t<proved>\nalpha\t1\t2\tp\nsmol_str\t3\t4\tp\n";
        assert_eq!(vanished(&libs, floors), Vec::<String>::new());
        let floors = format!("{floors}pkg-config\t5\t6\tp\n");
        assert_eq!(vanished(&libs, &floors), ["pkg-config"]);
        assert_eq!(
            vanished(&libs[..2], &floors),
            ["indexmap", "pkg-config"],
            "a queued fork that left is named too"
        );
    }

    /// AMBIGUITY is never resolved by guessing: two selected packages of one name
    /// would be judged against one floor.
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
    fn slots_follow_the_machine() {
        let gib = |n: u64| n << 30;
        assert_eq!(
            auto_slots(18, gib(128)),
            3,
            "cores decide on the 128 GiB machine"
        );
        assert_eq!(auto_slots(14, gib(24)), 1);
        assert_eq!(auto_slots(64, gib(32)), 2, "memory decides here");
        assert_eq!(auto_slots(18, 0), 1, "unread memory allows one");
        assert_eq!(auto_slots(2, gib(8)), 1, "at least one");
        assert_eq!(heavy_slots(gib(128)), 2);
        assert_eq!(heavy_slots(gib(24)), 1);
    }

    /// THE ORDER: a library with no cost first (it could be the longest of all), then
    /// the longest by CPU; unmeasured or measured past 16 GiB is heavy.
    #[test]
    fn the_longest_go_first_and_the_unmeasured_before_them() {
        let costs = parse_costs(&format!(
            "{COSTS_HEADER}\nalpha\t100\t200\t{}\t2026-10-15T00:00:00Z\nbeta\t900\t950\t{}\t2026-10-15T00:00:00Z\nbad row\n",
            1 << 30,
            20_u64 << 30
        ));
        assert_eq!(costs.len(), 2, "a row that does not parse is skipped");
        let order = order(&["alpha", "beta", "gamma", "delta"], &costs);
        assert_eq!(order, [(2, true), (3, true), (1, true), (0, false)]);
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
        guard_ends: String,
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
        fn guard(&mut self, job: &Job) -> Option<String> {
            (self.w.borrow().now >= Duration::from_secs(4) && self.guard_ends == job.name)
                .then(|| "its tree held 99 GiB".to_string())
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
                let rec = crate::trust_oblig::Record::parse(&text).unwrap();
                assert_eq!(rec.package, "alpha");
                assert_eq!(rec.unit, unit);
                assert_eq!((rec.functions, rec.total(), rec.proved()), (1, 1, 1));
                assert!(!error.exists());
            } else {
                assert!(!record.exists());
                assert!(
                    fs::read_to_string(&error)
                        .unwrap()
                        .contains("package foreign, not alpha")
                );
            }
            assert!(!report.exists(), "the reduced report is retired");
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
            guard_ends: String::new(),
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

    /// Every library is checked once, in queue order, never more at once than the
    /// slots, each slot reused only after its check ended.
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

    /// HEAVY SLOTS: three heavy libraries on three slots run one at a time on a
    /// machine with one heavy slot; a light one fills a slot beside them.
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

    /// THE BUDGET: a check still running at its budget is ended, its whole tree, and
    /// marked; one that finishes inside it is not.
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

    /// TWO CHECKS, ONE TRUSTD: until this build's trustd serves the socket, one check
    /// at a time (two that each start a trustd race, and the loser is refused); once
    /// it does, the slots fill.
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

    /// THE MEMORY GUARD ends a check inside its budget, and says why; the others run on.
    #[test]
    fn the_memory_guard_ends_a_check_and_says_why() {
        let mut f = fake(&[("small", 6)]);
        f.guard_ends = "huge".into();
        let jobs = vec![job("huge", false), job("small", false)];
        let (r, done) = slots(&mut f, jobs, 2, 2, 100);
        assert_eq!(r, Ok(()));
        let huge = done.iter().find(|d| d.job.name == "huge").unwrap();
        assert_eq!(huge.guarded.as_deref(), Some("its tree held 99 GiB"));
        assert!(!huge.over_budget);
        let small = done.iter().find(|d| d.job.name == "small").unwrap();
        assert_eq!((small.guarded.as_deref(), small.ended.code), (None, 0));
    }

    /// AN INTERRUPT ends every running check and decides nothing.
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
            guarded: None,
        }
    }

    const DONE: &str = "Summary: 5 proved, 0 failed (20 total); reproducible\nCoverage: 3/3 eligible function bodies verified (complete)\n";

    #[test]
    fn a_check_leaves_its_status_and_only_its_own_marks() {
        let d = tmp("trust-lane-record");
        let f = finished(d.path(), DONE, 0, false);
        let mut probed = 0;
        record(&f, Duration::from_secs(60), SystemTime::now(), &mut || {
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
        record(&f, Duration::from_secs(60), SystemTime::now(), &mut || {
            "foreign\tanother build".into()
        })
        .unwrap();
        assert_eq!(fs::read_to_string(beside("budget")).unwrap(), "60\n");
        assert_eq!(
            fs::read_to_string(beside("trustd")).unwrap(),
            "foreign\tanother build\n"
        );
    }

    /// A check's dep-info is what its slot's compiles wrote since it started: never
    /// an earlier check's in the same slot.
    #[test]
    fn a_checks_dep_info_is_only_what_it_wrote() {
        let d = tmp("trust-lane-depinfo");
        let f = finished(d.path(), DONE, 0, false);
        let deps = f.job.target.join("debug/deps");
        fs::create_dir_all(&deps).unwrap();
        let old = deps.join("old.d");
        fs::write(&old, "x").unwrap();
        let start = SystemTime::now();
        fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(start - Duration::from_secs(60))
            .unwrap();
        let new = deps.join("new.d");
        fs::write(&new, "x").unwrap();
        fs::write(deps.join("new.rmeta"), "x").unwrap();
        fs::write(f.job.target.join("debug/stray.d"), "x").unwrap();
        record(&f, Duration::from_secs(60), start, &mut String::new).unwrap();
        let listed = fs::read_to_string(d.path().join("alpha@1.0.0.log.depinfo")).unwrap();
        assert_eq!(listed, format!("{}\n", new.display()));
    }

    /// A FORK'S LOCKFILE is kept beside its log; one the check created goes, one that
    /// was there before stays.
    #[test]
    fn a_forks_lockfile_is_kept_beside_its_log() {
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
        let kept = d.path().join("alpha@1.0.0.log.lock");
        for created in [true, false] {
            fs::write(&path, "# the fork's own resolve\n").unwrap();
            let _ = fs::remove_file(&kept);
            let mut f = finished(d.path(), DONE, 0, false);
            f.job.lock = Some(ForkLock {
                path: path.clone(),
                created,
            });
            record(
                &f,
                Duration::from_secs(60),
                SystemTime::now(),
                &mut String::new,
            )
            .unwrap();
            assert_eq!(
                fs::read_to_string(&kept).unwrap(),
                "# the fork's own resolve\n"
            );
            assert_eq!(path.exists(), !created, "created {created}");
        }
        assert!(!fork_lock(d.path(), &argv).unwrap().created);
    }

    /// Only a check that finished costs anything: one cut short measured part of its
    /// library, and goes first again next time.
    #[test]
    fn only_a_finished_check_is_costed() {
        let d = tmp("trust-lane-cost");
        let f = finished(d.path(), DONE, 0, false);
        assert_eq!(
            finished_cost(&f),
            Some(Cost {
                cpu: 4,
                wall: 5,
                maxrss: 9
            })
        );
        assert_eq!(finished_cost(&finished(d.path(), DONE, 0, true)), None);
        let cut = "Summary: 5 proved, 0 failed (20 total); reproducible\nCoverage: unknown\n";
        assert_eq!(finished_cost(&finished(d.path(), cut, 0, false)), None);
        assert_eq!(finished_cost(&finished(d.path(), DONE, 143, false)), None);
        assert_eq!(finished_cost(&finished(d.path(), "", 0, false)), None);
    }

    // -- a whole run, over a bench the test drives -------------------------------

    /// Checks that end at once, each having printed `log` and written one fresh
    /// dep-info file in its slot.
    struct Quick<'a> {
        root: PathBuf,
        log: &'static str,
        interrupt: bool,
        now: Duration,
        started: &'a mut Vec<String>,
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
            fs::write(deps.join(format!("{}.d", job.name)), "lib.rs: src/lib.rs\n")?;
            fs::write(&job.log, self.log)?;
            Ok(Box::new(Over))
        }
        fn trustd_held(&self) -> bool {
            true
        }
        fn probe_trustd(&mut self) -> String {
            "ours\tthis build's".into()
        }
        fn guard(&mut self, _: &Job) -> Option<String> {
            None
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
        libraries: Vec<&'static str>,
        started: Vec<String>,
        log: &'static str,
        interrupt: bool,
        meanwhile: Option<fn(&Path)>,
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
                .chain([pkg("indexmap", "vendor/indexmap", false)])
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

    fn site(scope: Scope) -> Site {
        let dir = tmp("trust-lane-run");
        // `targo trust` refuses a target root under a group- or world-writable
        // directory (Linux's /tmp), so the slots go in the build's own scratch.
        let slots = aterm_tempfile::Builder::new()
            .prefix("trust-lane-slots")
            .tempdir_in(
                option_env!("CARGO_TARGET_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from),
            )
            .unwrap();
        let repo = dir.path().join("repo");
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
        cfg.target_root = Some(slots.path().join("target-trust"));
        cfg.state = Some(dir.path().join("state"));
        Site {
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
        }
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

    /// A WHOLE-TREE RUN ON A CLEAN CHECKOUT FILES its facts under the tree's key; the
    /// plan names the tree, the prover and the lane's environment.
    #[test]
    fn a_clean_whole_tree_run_files_its_facts() {
        let mut s = site(Scope::WholeTree);
        let (outcome, said, filed) = run_site(&mut s);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        assert_eq!(filed.len(), 1, "{said}");
        assert!(said.contains("\nFACTS: "), "{said}");
        let plan = fs::read_to_string(s.cfg.root().join("logs/.plan")).unwrap();
        let head = crate::ledger::rev_parse(&GitCli::new(&s.repo), "HEAD").unwrap();
        assert!(plan.contains(&format!("\ncommit {head}\n")), "{plan}");
        assert!(
            plan.contains("\nfile \n"),
            "nothing kept it from filing: {plan}"
        );
        assert!(plan.contains("\nlane-passed PATH\n") && plan.contains("\nsince "));
        assert!(!s.cfg.root().join("j0").exists(), "the slots are scratch");
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

    /// Re-measure only the undecided library, preserve the completed library's facts,
    /// then run nothing once the same record has no undecided libraries left.
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
        let git = GitCli::new(&s.repo);
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
        let git = GitCli::new(&s.repo);
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

    /// NOTHING IS FILED for bytes that are not HEAD's: a checkout dirty at the start,
    /// an edit or a commit during the run; and a `-p` run files nothing at all.
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
        assert!(
            said.contains("FACTS: none — the checkout is not HEAD's bytes"),
            "{said}"
        );

        let edits: [fn(&Path); 2] = [
            |repo| fs::write(repo.join("crates/alpha/src/lib.rs"), "pub fn h() {}\n").unwrap(),
            |repo| git_in(repo, &["commit", "-q", "--allow-empty", "-m", "meanwhile"]),
        ];
        for edit in edits {
            let mut s = site(Scope::WholeTree);
            s.meanwhile = Some(edit);
            let (outcome, said, filed) = run_site(&mut s);
            assert_eq!((outcome, filed.len()), (Outcome::Pass, 0), "{said}");
            assert!(
                said.contains("FACTS: none — the checkout changed during the run"),
                "{said}"
            );
        }

        let mut narrow = site(Scope::Packages(vec!["alpha".into()]));
        let (outcome, said, filed) = run_site(&mut narrow);
        assert_eq!((outcome, filed.len()), (Outcome::Pass, 0), "{said}");
        assert!(
            !said.contains("FACTS:"),
            "a -p run is no filing run: {said}"
        );
    }

    /// A RUN THAT DECIDES NOTHING SAYS WHY, whether its checks were interrupted or
    /// could not start, and files nothing.
    #[test]
    fn a_run_that_decides_nothing_says_why() {
        let mut s = site(Scope::WholeTree);
        s.interrupt = true;
        let (outcome, said, filed) = run_site(&mut s);
        assert_eq!(
            outcome,
            Outcome::CouldNotRun("interrupted; nothing was decided".into())
        );
        assert!(
            said.contains("trust: COULD NOT RUN — interrupted; nothing was decided"),
            "{said}"
        );
        assert!(filed.is_empty());
        assert_eq!(
            stopped(Stopped::Unstartable(
                "the check of alpha could not be started: x".into()
            )),
            "the check of alpha could not be started: x"
        );
    }

    /// A HOST THAT CANNOT REPRODUCE A FLOOR refuses that library before any check:
    /// asked to stop then, nothing runs and the run is NARROWED; else the rest runs
    /// and the judge's verdict is NARROWED too.
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
                !s.cfg.root().join("logs/alpha@1.0.0.log").exists(),
                "a refused library is never run: {said}"
            );
            if stop {
                assert!(filed.is_empty(), "a run that stopped files nothing: {said}");
            }
        }
    }

    /// A FLOOR THE GRAPH NO LONGER CARRIES fails the whole-tree run (FORK-VANISHED);
    /// a `-p` run answers only for what it names.
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

    /// A WRITABLE PATH COMPONENT IS COULD NOT RUN: `targo trust` refuses a verified
    /// runtime-library directory under it, which would read as a finding.
    #[test]
    fn a_world_writable_component_of_the_target_root_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("trust-lane-root");
        let open = d.path().join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
        let (dir, why) = refused_component(&open.join("target-trust")).unwrap();
        assert_eq!(dir, fs::canonicalize(&open).unwrap());
        assert!(why.contains("world-writable"), "{why}");
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

    #[test]
    fn one_run_holds_a_target_root() {
        let d = tmp("trust-lane-lock");
        let path = d.path().join(".run.lock");
        let first = RunLock::take(&path, "wait").unwrap();
        let second = RunLock::take(&path, "wait").err().unwrap();
        assert!(second.ends_with(": wait"), "{second}");
        drop(first);
        assert!(
            RunLock::take(&path, "wait").is_ok(),
            "released when the run ends"
        );
    }

    #[test]
    fn the_run_flags_parse_strictly() {
        let p = |args: &[&str]| parse(&mut args.iter().copied());
        let c = p(&[
            "-p",
            "aterm-hash",
            "-p",
            "smol_str",
            "--jobs",
            "2",
            "--crate-budget",
            "60",
        ])
        .unwrap();
        assert_eq!(
            c.scope,
            Scope::Packages(vec!["aterm-hash".into(), "smol_str".into()])
        );
        assert_eq!((c.jobs, c.budget), (Some(2), Duration::from_secs(60)));
        assert_eq!(p(&["--whole-tree"]).unwrap().scope, Scope::WholeTree);
        assert_eq!(p(&["--remeasure"]).unwrap().scope, Scope::Remeasure);
        assert!(p(&[]).is_err(), "a whole-tree run is asked for by name");
        assert!(p(&["--whole-tree", "-p", "x"]).is_err());
        assert!(p(&["-p"]).is_err());
        assert!(p(&["--whole-tree", "--jobs", "0"]).is_err());
        assert!(p(&["--whole-tree", "--crate-budget", "-1"]).is_err());
        assert!(p(&["--whole-tree", "--bogus"]).is_err());
    }

    #[test]
    fn the_exit_status_says_what_was_decided() {
        assert_eq!(Outcome::Pass.exit(), 0);
        assert_eq!(Outcome::Fail(vec!["a".into()]).exit(), 1);
        assert_eq!(Outcome::Undecided(vec!["a".into()]).exit(), 2);
        assert_eq!(Outcome::CouldNotRun("x".into()).exit(), 2);
        assert_eq!(Outcome::Narrowed(vec!["a".into()]).exit(), 4);
    }

    /// The guard crosses a bound only on a measurement it read, and says which.
    #[test]
    fn the_guard_says_which_bound_was_crossed() {
        use guard::Reading;
        let g = Guard {
            tree_bytes: 10 << 30,
            swap_bytes: 8 << 30,
            level_min: 10,
            free_bytes: 28 << 30,
        };
        let calm = Reading {
            tree: Some(1 << 30),
            swap: Some(0),
            level: Some(40),
            free: Some(100 << 30),
        };
        assert_eq!(guard::verdict(&g, calm), None);
        assert_eq!(
            guard::verdict(&g, Reading::default()),
            None,
            "unread is no crossing"
        );
        let crossed = |r: Reading, said: &str| {
            assert!(
                guard::verdict(&g, r).is_some_and(|w| w.contains(said)),
                "{r:?}"
            );
        };
        crossed(
            Reading {
                tree: Some(11 << 30),
                ..calm
            },
            "process tree",
        );
        crossed(
            Reading {
                swap: Some(9 << 30),
                ..calm
            },
            "swap",
        );
        crossed(
            Reading {
                level: Some(9),
                ..calm
            },
            "memorystatus_level 9",
        );
        crossed(
            Reading {
                free: Some(1 << 30),
                ..calm
            },
            "free",
        );
        assert_eq!(
            guard::parse_swapusage(
                "total = 2048.00M  used = 1024.00M  free = 1024.00M  (encrypted)"
            ),
            Some(1 << 30)
        );
        assert_eq!(
            guard::parse_swapusage("used = 1035.25M"),
            Some(1_085_538_304)
        );
        assert_eq!(guard::parse_swapusage("used = 2.00G"), Some(2 << 30));
        assert_eq!(guard::parse_swapusage("used = 3G"), Some(3 << 30));
        assert_eq!(guard::parse_swapusage("nothing"), None);
        assert_eq!(
            guard::parse_df(
                "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/disk3s5 100 50 1024 50% /\n"
            ),
            Some(1 << 20)
        );
        // The footprint of a process that is surely there reads, and is no zero.
        assert!(guard::own_bytes().is_some_and(|b| b > 0));
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

    fn short_dir(tag: &str) -> PathBuf {
        // A unix socket's path must fit in 104 bytes.
        let d = PathBuf::from(format!("/tmp/tl{tag}{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_probe_tells_this_builds_trustd_from_another() {
        let d = short_dir("p");
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
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_keeper_holds_a_grant_on_this_builds_trustd_only() {
        let d = short_dir("k");
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
        let _ = fs::remove_dir_all(&d);
    }

    /// The keeper ends when dropped, even while a trustd that never answers keeps it
    /// waiting (its own read waits 130 s).
    #[test]
    fn the_keeper_ends_while_a_trustd_keeps_it_waiting() {
        let d = short_dir("m");
        let sock = d.join("m.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let _mute = std::thread::spawn(move || {
            let conn = listener.incoming().next();
            let _ = tx.send(());
            std::thread::park_timeout(Duration::from_secs(120));
            drop(conn);
        });
        let keeper = trustd::Keeper::start(sock, "aa".into(), d.join("grants"), d.join("seen"));
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the keeper never connected");
        let t0 = Instant::now();
        drop(keeper);
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "the drop waited out the trustd"
        );
        let _ = fs::remove_dir_all(&d);
    }
}
