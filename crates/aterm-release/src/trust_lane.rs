// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trust verification lane (`targo --unverified ship trust run`).

use aterm_toolchain::named_lock;
use aterm_uds::spawnfd::{Stdio, command};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::ledger::GitCli;
use crate::trust_facts::{self, Decision, Plan, Update, one_line};

/// Forks in the resolved graph not yet verified by the lane, with their reasons.
pub const QUEUED_FORKS: &[(&str, &str)] = &[];

/// Cargo profile name for Trust lane checks (`opt-level = 0`, [`lane_profile_args`]).
const LANE_PROFILE: &str = "trust-lane";

/// Per-check wall-clock budget (`6h`) before a check is terminated as `OVER-BUDGET`.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(6 * 3600);

/// How often the slots are polled.
const POLL: Duration = Duration::from_millis(250);

/// What a run claims of its target root's volume where none was measured there: 6 GiB, the
/// lane's budget in the gate's preflight (`TRUST_LANE_BYTES`, 42e8a9a273^,
/// `crates/aterm-verify/src/disk.rs`). MEASURED there 2026-09-27: one slot peaked at 1.7 GiB
/// (`du -sh`) checking aterm-gui's whole graph, three slots on that machine, read up to 6. A
/// whole run's peak was not measured then. A first run claims it less what the root holds
/// already; a run measured here claims what it wrote instead ([`DISK_RECORD`]).
const COLD_DISK: u64 = 6 << 30;

/// The file under the target root's `logs` keeping what the last runs wrote there
/// (`docs/DESIGN-adaptive-build-2026-10-24.md`, DISK-ADMIT).
const DISK_RECORD: &str = "footprint.json";

/// The variable whose random value marks every process of one check ([`proc_tree::Mark`]).
pub const CHECK_VAR: &str = aterm_uds::process::TRUST_CHECK_VAR;

/// Fixed environment variables passed to every compile in the lane.
const LANE_PINS: [(&str, &str); 2] = [("TRUST_NO_MIGRATE_WARN", "1"), ("CARGO_INCREMENTAL", "0")];

/// Host environment variable names forwarded to lane compiles.
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

/// Package scope verified by a lane run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every workspace library and fork (eligible to file whole-tree facts).
    WholeTree,
    /// Explicit package list (`-p`); files no whole-tree facts.
    Packages(Vec<String>),
    /// Only libraries left undecided in stored facts for this tree and prover.
    Remeasure,
}

/// Configuration for a trust lane run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub repo: PathBuf,
    pub scope: Scope,
    /// Concurrent slot count (`None` selects automatic sizing from cores and memory).
    pub jobs: Option<usize>,
    /// Root directory for slot target dirs and logs (`None` selects the default root).
    pub target_root: Option<PathBuf>,
    pub budget: Duration,
    /// Whether to ratchet up floor counts and update the memory record.
    pub update_ratchet: bool,
    /// Why `--update-ratchet` lowers onto this prover a floor another prover recorded that the
    /// run falls below (`--reason`), written in the row's fifth column ([`Update::reason`]).
    pub reason: Option<String>,
    /// Whether to attempt libraries this host would otherwise refuse for memory.
    pub try_refused: bool,
    /// Whether to write per-obligation records beside each check log ([`crate::trust_oblig`]):
    /// on by default, since the judge names what a backstop stopped from them.
    pub obligations: bool,
    /// Whether to stop before running any check if memory guard narrows the run.
    pub stop_if_narrowed: bool,
    /// Explicit prover `bin/` directory (`None` uses the active toolchain).
    pub prover: Option<PathBuf>,
    /// Directory for cost history and host memory records.
    pub state: Option<PathBuf>,
    /// Whether a target root another run holds, or a socket a trustd of another build serves,
    /// is COULD NOT RUN at once (`--no-wait`) instead of waited for.
    pub no_wait: bool,
    /// How the run's disk claim and each check's memory claim are admitted (`--admit wait`, the
    /// default, or `--admit now`, which takes each at once and says what it would have waited
    /// for).
    pub admit: aterm_capacity::Admit,
}

impl Config {
    /// Creates a default [`Config`] for `scope` in `repo`.
    #[must_use]
    pub fn new(repo: &Path, scope: Scope) -> Config {
        Config {
            repo: repo.to_path_buf(),
            scope,
            jobs: None,
            target_root: None,
            budget: DEFAULT_BUDGET,
            update_ratchet: false,
            reason: None,
            try_refused: false,
            obligations: true,
            stop_if_narrowed: false,
            prover: None,
            state: None,
            no_wait: false,
            admit: aterm_capacity::Admit::Wait,
        }
    }
}

/// Final verdict of a trust lane run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every verified library met its floor.
    Pass,
    /// One or more libraries fell below floor, lacked a floor, or failed.
    Fail(Vec<String>),
    /// Libraries left undecided due to `trustd` or budget.
    Undecided(Vec<String>),
    /// Libraries refused on this host due to memory constraints.
    Narrowed(Vec<String>),
    /// Run could not start or complete.
    CouldNotRun(String),
}

impl Outcome {
    /// Exit code (`0` pass, `1` fail, `2` undecided/could-not-run, `4` narrowed).
    #[must_use]
    pub const fn exit(&self) -> i32 {
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

fn parse_uint<T: std::str::FromStr>(s: &str) -> Option<T> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Parses CLI arguments after `trust run`.
///
/// # Errors
/// An unknown flag, a missing or malformed flag value, or no scope or more than one.
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
                    n => Some(parse_uint(n).filter(|n| *n > 0).ok_or(format!(
                        "trust run: --jobs takes a count or auto, got {n:?}"
                    ))?),
                };
            }
            "--target-root" => cfg.target_root = Some(PathBuf::from(value()?)),
            "--crate-budget" => {
                let v = value()?;
                let secs: u64 = parse_uint(v).filter(|s| *s > 0).ok_or(format!(
                    "trust run: --crate-budget takes whole seconds, got {v:?}"
                ))?;
                cfg.budget = Duration::from_secs(secs);
            }
            "--prover" => cfg.prover = Some(PathBuf::from(value()?)),
            "--update-ratchet" => cfg.update_ratchet = true,
            "--reason" => cfg.reason = Some(trust_facts::reason_arg("run", value()?)?),
            "--try-refused" => cfg.try_refused = true,
            "--obligations" => cfg.obligations = true,
            "--no-obligations" => cfg.obligations = false,
            "--stop-if-narrowed" => cfg.stop_if_narrowed = true,
            "--no-wait" => cfg.no_wait = true,
            aterm_capacity::Admit::FLAG => {
                let v = value()?;
                cfg.admit = aterm_capacity::Admit::parse(v)
                    .ok_or(format!("trust run: --admit takes wait or now, got {v:?}"))?;
            }
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
    if cfg.reason.is_some() && !cfg.update_ratchet {
        return Err(
            "trust run: --reason says why --update-ratchet lowers a floor; give both".into(),
        );
    }
    Ok(cfg)
}

/// Resolved toolchain driver used for every compile in a run.
#[derive(Clone, Debug)]
pub struct Driver {
    /// Physical `bin/` directory of the toolchain.
    pub bin: PathBuf,
    pub targo: PathBuf,
    /// Output of `targo --version`.
    pub id: String,
}

impl Driver {
    /// Resolves and validates the toolchain driver at `bin` ([`toolchain_refusal`]).
    ///
    /// # Errors
    /// Returns an error describing what is missing or refused and how to fix it.
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
        if crate::durable::open_regular_file(&targo).is_ok_and(|f| {
            use std::io::Read as _;
            let mut head = [0u8; 2];
            f.take(2)
                .read(&mut head)
                .is_ok_and(|n| n == 2 && head == *b"#!")
        }) {
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

/// Builds the environment variable list for a lane check subprocess.
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
    if let Some(j) =
        host("CARGO_BUILD_JOBS").filter(|j| parse_uint::<u32>(j).is_some_and(|n| n > 0))
    {
        env.push(("CARGO_BUILD_JOBS".into(), j));
    }
    let doc = bin.join("trustdoc");
    if fs::symlink_metadata(&doc).is_ok_and(|m| m.is_file()) {
        env.push(("RUSTDOC".into(), doc.display().to_string()));
    }
    env
}

/// Classification of a package in the resolved Cargo graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Workspace member with a library target.
    Member,
    /// Workspace member with binary targets only.
    MemberBin,
    /// Path dependency outside the workspace (`vendor/` fork).
    Fork,
}

/// Single verification row in the gate list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lib {
    /// Row name (`<package>` or `<package>+<feature>` via [`FEATURE_ROW`]).
    pub name: String,
    pub kind: Kind,
    /// Versioned package spec (`name@version` or `name+feature@version`).
    pub spec: String,
    /// Arguments passed to `targo trust check` before `--allow-l0-gaps`.
    pub argv: Vec<String>,
}

/// Separator joining a workspace member and feature in a fork-feature row (`+`).
pub const FEATURE_ROW: char = '+';

/// Strips any feature or twin-version suffix from `row` to return the Cargo package name.
#[must_use]
pub fn package_identity(row: &str) -> &str {
    row.split_once([FEATURE_ROW, '@'])
        .map_or(row, |(package, _)| package)
}

/// Non-default features of `pkg` that directly enable a path fork in `forks`.
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

/// Builds `--profile trust-lane` `--config` arguments clearing `opt-level` on `rows`.
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

/// Reads `[profile.dev.package.<spec>]` keys from `path`, or an empty set if missing.
///
/// # Errors
/// A file that is there and does not parse: unread, it would verify optimized.
fn dev_package_rows(path: &Path) -> Result<BTreeSet<String>, String> {
    let text = match crate::durable::read_text(path) {
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

/// Collects dev package profile rows for `manifest` and `.cargo/config.toml` under `root`.
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

/// Builds the gate list from `targo metadata --format-version 1 --all-features`.
///
/// # Errors
/// Returns an error if `meta` is malformed or contains an invalid package name or spec.
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
            continue;
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
    let mut versions: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for lib in &out {
        let (_, version) = lib.spec.rsplit_once('@').unwrap_or((&lib.spec, ""));
        versions
            .entry(package_identity(&lib.name))
            .or_default()
            .insert(version);
    }
    let multi: BTreeSet<String> = versions
        .into_iter()
        .filter_map(|(k, v)| (v.len() > 1).then(|| k.to_string()))
        .collect();
    if !multi.is_empty() {
        for lib in &mut out {
            if multi.contains(package_identity(&lib.name)) {
                lib.name.clone_from(&lib.spec);
            }
        }
    }
    out.sort_unstable_by(|a, b| {
        (a.kind == Kind::Fork, &a.name, &a.spec).cmp(&(b.kind == Kind::Fork, &b.name, &b.spec))
    });
    Ok(out)
}

/// Returns floor or [`QUEUED_FORKS`] package names that no longer exist in `libs`.
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

/// Library row selected for a run along with its pre-check disposition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selected {
    pub lib: Lib,
    pub decision: Decision,
}

/// Selects `names` (or all of `libs` when `None`) and assigns each row's pre-check status.
///
/// # Errors
/// Returns an error if any requested name matches neither a workspace member nor a fork.
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

/// Automatic slot count (`--jobs auto`) from logical cores: one slot per 6, at least one.
/// Memory sets no slot count: each check's claim is admitted at the launch gate against the
/// memory available then ([`Admission`]).
#[must_use]
pub fn auto_slots(cores: usize) -> usize {
    (cores / 6).max(1)
}

/// The memory a check of a library claims: its last recorded peak (`cost`'s largest process,
/// the row read under the prover that runs: [`Costs`]) plus a margin
/// ([`aterm_capacity::peak_claim`]), or with no peak recorded 16 GiB or half of
/// `ram` (this host's RAM, when read; 0 unread), whichever is less
/// ([`aterm_capacity::cold_claim`]), so a host of any size can admit a library it has not
/// measured. A peak a check cut short reached ([`Cost::cut`]) is the least it needs; where no
/// check of the library under its prover has finished ([`Cost::finished`]) that says nothing of
/// its scale, so it claims the larger of the two. A run takes the larger of this and what
/// another prover's row claims where no check under its own has finished ([`Costs::claim`]).
/// A claim is never cut to fit the RAM: one that is not below what can ever be available (the
/// RAM less what is wired) is refused up front ([`trust_facts::peak_seen`]), and under
/// `--try-refused` it starts at once ([`Admission::admit`]).
#[must_use]
pub fn claim_need(cost: Option<&Cost>, ram: u64) -> u64 {
    let cold = aterm_capacity::cold_claim(Some(ram).filter(|r| *r > 0));
    match cost.filter(|c| c.maxrss > 0) {
        Some(c) if !c.finished() => aterm_capacity::peak_claim(c.maxrss).max(cold),
        Some(c) => aterm_capacity::peak_claim(c.maxrss),
        None => cold,
    }
}

/// Historical resource cost of a library's last check under one prover.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cost {
    pub cpu: u64,
    pub wall: u64,
    /// Its peak memory in bytes ([`Ended::maxrss`]; a row recorded before 2026-10-31 has the
    /// largest process's resident high-water mark alone).
    pub maxrss: u64,
    /// RFC 3339 timestamp when the cost was recorded.
    pub when: String,
    /// The prover that ran the check: the first line of its `targo --version` ([`Driver::id`]),
    /// as floors.tsv names it. Empty for a row recorded before rows named their prover, or
    /// under one whose version could not be read.
    pub prover: String,
    /// How the check was cut short, where it was: then its CPU, wall time and peak are what
    /// it took before it ended, each the least it would have taken. `None` for a check that
    /// finished, whose figures are what it took.
    pub cut: Option<Cut>,
    /// Whether this row of a cut check raised the row of a check under its prover that
    /// finished ([`keep`]), as its eighth column says it (`raised`): its prover has then
    /// measured the library ([`Cost::finished`]). Always `false` where [`Cost::cut`] is `None`.
    pub raised: bool,
}

impl Cost {
    /// Whether a check of its library under its prover finished: its own, or one whose row a
    /// cut check's raised ([`Cost::raised`]). Such a row's length and scale are measured, so it
    /// claims its peak and margin alone ([`claim_need`]), takes its place by its CPU
    /// ([`order`]), and no other prover's row stands in for it ([`Costs::of`]).
    #[must_use]
    pub const fn finished(&self) -> bool {
        self.cut.is_none() || self.raised
    }
}

/// The word a [`Cost::raised`] row's eighth column holds.
const RAISED: &str = "raised";

/// How a check that did not finish ended ([`Cost::cut`]), as its cost row's seventh column
/// says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    /// Ended at its wall-clock budget (OVER-BUDGET).
    OverBudget,
    /// Ended by the run's stop: an interrupt, or another check that could not start
    /// ([`Finished::interrupted`]).
    Interrupted,
    /// Ended by itself without a whole summary, or with its transport to trustd failed: what
    /// the judge does not take as a measurement of its library.
    Failed,
}

impl Cut {
    /// The word its row's seventh column holds.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Cut::OverBudget => "over-budget",
            Cut::Interrupted => "interrupted",
            Cut::Failed => "failed",
        }
    }

    /// Reads [`Cut::word`].
    #[must_use]
    pub fn parse(word: &str) -> Option<Cut> {
        [Cut::OverBudget, Cut::Interrupted, Cut::Failed]
            .into_iter()
            .find(|c| c.word() == word)
    }
}

/// A cost file's rows by library: one per prover that ran it ([`Cost::prover`]), oldest first,
/// so a reader that keeps a library's last row keeps its newest.
pub type CostRows = BTreeMap<String, Vec<Cost>>;

/// A clone's cost history read for the prover that runs ([`read_costs`]). Two provers ask
/// different obligations of one library and peak differently on it (aterm-gui's 62.45 GB was
/// a locally built 1.101 prover's, 5176e9d23d, where 20565c325's check reached at least
/// 47.20 GB), so a library is measured under a prover only by that prover's rows. Its memory is
/// another matter: where no check under the prover that runs has finished, another prover's
/// peak of the same library is the best estimate there is, so it sizes the claim and refuses up
/// front ([`Costs::claim`], [`Costs::peak`]) until this prover's own measurement decides: a
/// finished check of it, or a cut one whose peak is by itself refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Costs {
    /// Each library's row under that prover: its own, else its newest row where that names no
    /// prover. The order is read from these alone, and they are what a run's own rows raise.
    pub rows: BTreeMap<String, Cost>,
    /// Each library whose row here is missing, or holds no check of it under that prover that
    /// finished ([`Cost::finished`]), and that another prover has a row of: that prover's
    /// newest. The library still runs first, as one with no row does, and its claim is at least
    /// that row's ([`Costs::claim`]).
    pub elsewhere: BTreeMap<String, Cost>,
}

impl Costs {
    /// `rows` read for `prover` (its one-line `targo --version`).
    #[must_use]
    pub fn of(rows: &CostRows, prover: &str) -> Costs {
        let mut costs = Costs::default();
        for (name, all) in rows {
            let read = all
                .iter()
                .rev()
                .find(|c| c.prover == prover)
                .or_else(|| all.last().filter(|c| c.prover.is_empty()));
            if let Some(c) = read {
                costs.rows.insert(name.clone(), c.clone());
            }
            if read.is_none_or(|c| !c.finished())
                && let Some(c) = all
                    .iter()
                    .rev()
                    .find(|c| !c.prover.is_empty() && c.prover != prover)
            {
                costs.elsewhere.insert(name.clone(), c.clone());
            }
        }
        costs
    }

    /// Another prover's row of `name` that stands in for this prover's ([`Costs::elsewhere`]),
    /// unless the peak of `name`'s row here, the least its check needs, already claims what can
    /// never be available below `bound` (the plan's [`trust_facts::Host::admits_below`]; 0
    /// unread): this prover's own measurement then refuses it up front by itself, and another
    /// prover's, which peaks differently, could only misname that peak and size the claim
    /// `--try-refused` holds on it.
    fn other(&self, name: &str, bound: u64) -> Option<&Cost> {
        let decided = self
            .rows
            .get(name)
            .is_some_and(|c| bound > 0 && aterm_capacity::peak_claim(c.maxrss) >= bound);
        self.elsewhere.get(name).filter(|_| !decided)
    }

    /// The memory a check of `name` claims on `ram` ([`claim_need`] of its row here), and where
    /// no check of it under this prover has finished, at least what another prover's row
    /// claims ([`Costs::other`], under `bound`).
    #[must_use]
    pub fn claim(&self, name: &str, ram: u64, bound: u64) -> u64 {
        let there = self
            .other(name, bound)
            .map_or(0, |c| claim_need(Some(c), ram));
        claim_need(self.rows.get(name), ram).max(there)
    }

    /// Another prover's row of `name` where it raises the claim above what `name`'s row here
    /// claims on `ram` ([`Costs::claim`]): the estimate a run that checks it names.
    #[must_use]
    pub fn estimate(&self, name: &str, ram: u64, bound: u64) -> Option<&Cost> {
        self.other(name, bound)
            .filter(|_| self.claim(name, ram, bound) > claim_need(self.rows.get(name), ram))
    }

    /// The row whose peak sizes `name`'s claim, as the up-front refusal reads it
    /// ([`trust_facts::peak_seen`]): its row here, or another prover's ([`Costs::other`],
    /// under `bound`) where that one's peak is larger.
    #[must_use]
    pub fn peak(&self, name: &str, bound: u64) -> Option<trust_facts::Peak> {
        let peak = |c: &Cost, of: &str| trust_facts::Peak {
            rss: c.maxrss,
            when: c.when.clone(),
            own: of.is_empty() && !c.prover.is_empty(),
            least: c.cut.is_some(),
            of: of.to_string(),
        };
        let here = self.rows.get(name);
        match self.other(name, bound) {
            Some(o) if here.is_none_or(|c| o.maxrss > c.maxrss) => Some(peak(o, &o.prover)),
            _ => here.map(|c| peak(c, "")),
        }
    }
}

const COSTS_HEADER: &str = "# trust lane cost history: <name> <CPU s> <wall s> <peak memory bytes: the larger of the largest process's resident high-water mark and the largest footprint its processes were read with> <when> <prover> <cut> <raised>, one row per library and prover (its `targo --version`; a row with none predates rows that name one); <cut>, where a check did not finish, says how it ended (over-budget, interrupted or failed), and its figures are then each the least it would have taken, which its row raises and never lowers, and <raised> is `raised` where it raised the row of a check that finished; a run reads its prover's row, else a library's newest row where that names no prover, and a library with neither is unmeasured; the slots take the longest first, a library with no row or none of a check that finished first of all, and each check claims its peak and a margin (at least what a library with no row claims, where no check of it finished) before it starts, and where no check under its prover has finished, at least what another prover's newest row of it claims, unless its own row's peak is refused by itself (a claim not below what can ever be available, the RAM less what is wired, is refused up front)";

/// The committed seed's first line ([`trust_facts::COSTS`]), above [`COSTS_HEADER`].
const SEED_HEADER: &str = "# The trust lane's cost-history SEED: the fallback a clone with no history of its own reads (of a library's rows under one prover in this file and its git directory's aterm-release/trust-costs.tsv, the newer is the one read). `targo --unverified ship trust run -p <crate>… --update-ratchet` rewrites it with floors.tsv and memory.tsv. A library with no row a run can read, or one with no check under its prover that finished, runs first, claiming at least 16 GiB or half the RAM, whichever is less, and at least what another prover's newest row of it claims, unless its own row's peak is refused by itself.";

/// Loads cost history for `prover` ([`Costs::of`]): of a library's rows under one prover, the
/// newer of the committed `seed`'s and the local `history`'s (by its stamp; the history's on a
/// tie), so a peak measured on another host after this clone's last run of it is the one read.
#[must_use]
pub fn read_costs(history: Option<&Path>, seed: &Path, prover: &str) -> Costs {
    let mut all = cost_file(seed);
    for (name, rows) in history.map(cost_file).unwrap_or_default() {
        for row in rows {
            keep(&mut all, &name, row);
        }
    }
    Costs::of(&all, prover)
}

/// The rows of a cost file; one that cannot be read has none.
fn cost_file(path: &Path) -> CostRows {
    crate::durable::read_text(path)
        .map(|t| parse_costs(&t))
        .unwrap_or_default()
}

/// Adds `row` to `name`'s rows unless its row under the same prover is newer (by stamp; `row`
/// wins a tie). A row that names no prover and is no newer than one that names a prover is
/// never read again ([`Costs::of`]), so it is dropped, or not added. A cut check's row
/// ([`Cost::cut`]) holds the least it would have taken, so it never lowers what its prover
/// reads (its own row, else the newest that names none): where that row's peak is at least its
/// own it is not added, and otherwise it keeps that row's CPU and wall time where they are
/// larger, and is [`Cost::raised`] where that row's check, or one whose row that raised,
/// finished, so the measurement stays its prover's ([`Cost::finished`]).
fn keep(rows: &mut CostRows, name: &str, row: Cost) {
    let all = rows.entry(name.to_string()).or_default();
    if all.iter().any(|c| {
        (c.prover == row.prover && c.when > row.when)
            || (row.prover.is_empty() && !c.prover.is_empty() && c.when >= row.when)
    }) {
        return;
    }
    let read = all
        .iter()
        .rev()
        .find(|c| c.prover == row.prover)
        .or_else(|| all.iter().rev().find(|c| c.prover.is_empty()));
    let row = match read {
        Some(c) if row.cut.is_some() && c.maxrss >= row.maxrss => return,
        Some(c) if row.cut.is_some() => Cost {
            cpu: row.cpu.max(c.cpu),
            wall: row.wall.max(c.wall),
            raised: c.finished(),
            ..row
        },
        _ => row,
    };
    all.retain(|c| c.prover != row.prover && !(c.prover.is_empty() && c.when <= row.when));
    all.push(row);
    all.sort_by(|a, b| a.when.cmp(&b.when));
}

/// Merges existing seed rows with `history` for `--update-ratchet`, row by row ([`keep`]).
#[must_use]
pub fn seed_rows(committed: CostRows, history: CostRows, libraries: &BTreeSet<String>) -> CostRows {
    let mut all = committed;
    for (name, rows) in history {
        for row in rows {
            keep(&mut all, &name, row);
        }
    }
    all.retain(|name, _| libraries.contains(name));
    all
}

/// Parses cost history text into each library's rows ([`keep`]: a later row of one library
/// and prover overrides on a tie). A row whose seventh column names no [`Cut`], or whose eighth
/// is other than [`RAISED`] on a cut row's, is not read.
#[must_use]
pub fn parse_costs(text: &str) -> CostRows {
    let mut all = CostRows::new();
    for l in text.lines().filter(|l| !l.starts_with('#')) {
        let mut f = l.split('\t');
        let (Some(name), Some(cpu), Some(wall), Some(maxrss)) = (
            f.next(),
            f.next().and_then(parse_uint),
            f.next().and_then(parse_uint),
            f.next().and_then(parse_uint),
        ) else {
            continue;
        };
        let (when, prover) = (f.next(), f.next());
        let cut = match f.next().filter(|w| !w.is_empty()) {
            None => None,
            Some(w) => match Cut::parse(w) {
                Some(cut) => Some(cut),
                None => continue,
            },
        };
        let raised = match f.next().filter(|w| !w.is_empty()) {
            None => false,
            Some(RAISED) if cut.is_some() => true,
            Some(_) => continue,
        };
        let field = |v: Option<&str>| v.map_or_else(String::new, str::to_string);
        keep(
            &mut all,
            name,
            Cost {
                cpu,
                wall,
                maxrss,
                when: field(when),
                prover: field(prover),
                cut,
                raised,
            },
        );
    }
    all
}

/// Orders `names` for execution, unknown cost first (no row, or one of no check under its
/// prover that finished: [`Cost::finished`]), then by descending CPU time.
#[must_use]
pub fn order(names: &[&str], costs: &BTreeMap<String, Cost>) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..names.len()).collect();
    idx.sort_unstable_by_key(|&i| match costs.get(names[i]) {
        Some(c) if c.finished() => (1, u64::MAX - c.cpu, i),
        _ => (0, 0, i),
    });
    idx
}

/// Single verification check scheduled on a slot.
#[derive(Clone, Debug)]
pub struct Job {
    pub slot: usize,
    pub name: String,
    pub argv: Vec<String>,
    pub log: PathBuf,
    /// Per-check `--report-dir` when `--obligations` is enabled.
    pub report: Option<PathBuf>,
    /// Slot `CARGO_TARGET_DIR` assigned when the check starts.
    pub target: PathBuf,
    /// The memory it claims before it starts, in bytes ([`claim_need`]); 0 claims none.
    pub need: u64,
    /// Fork `Cargo.lock` state tracked when checking a fork by manifest path ([`fork_lock`]).
    pub lock: Option<ForkLock>,
    /// The cover marker of the memory claim it was admitted under ([`Admitted::cover`]), which
    /// its check is handed ([`aterm_capacity::COVER_ENV`]); `None` for none.
    pub cover: Option<String>,
}

/// Lockfile path and pre-existence flag for a fork check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkLock {
    pub path: PathBuf,
    /// Whether this check created the lockfile (and must remove it afterward).
    pub created: bool,
}

/// Determines the [`ForkLock`] for `argv` in `repo`, if `--manifest-path` is used.
#[must_use]
pub fn fork_lock(repo: &Path, argv: &[String]) -> Option<ForkLock> {
    let path = repo.join(manifest_of(argv)?).parent()?.join("Cargo.lock");
    Some(ForkLock {
        created: !path.exists(),
        path,
    })
}

/// Exit status and resource usage of a finished check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ended {
    /// Exit status (`128 + signal` if signaled).
    pub code: i32,
    /// Total user and system CPU time across the check and reaped children.
    pub cpu: Duration,
    /// Its peak memory in bytes: the larger of the resident high-water mark the kernel reports
    /// at the reap and the largest footprint a read of its processes showed ([`TreeTimes::peak`]).
    /// Under memory pressure the kernel compresses a process's pages out of its resident set, so
    /// the first alone can be far below what the check needed (ATERM-GUI-MEM: aterm-gui's trustc
    /// under 20565c325 had peaked at a 47.2 GB footprint when no read of its resident pages, every
    /// 30 s, had found more than 29.5 GB).
    pub maxrss: u64,
    /// How long its processes' threads ran and were runnable ([`TreeTimes`]); `None` when no
    /// read of the process table found them.
    pub threads: Option<trust_facts::Threads>,
}

/// Handle to a running verification check.
pub trait Check {
    /// Polls whether the check has finished and been reaped.
    ///
    /// # Errors
    /// Returns an error if waiting on the child process fails.
    fn poll(&mut self) -> std::io::Result<Option<Ended>>;
    /// Terminates the check's processes (leaving `trustd` alone) and reaps its root.
    fn end(&mut self) -> Ended;
}

/// Execution backend that spawns and monitors slot checks.
pub trait Checks {
    /// Starts `job` in its assigned slot target directory.
    ///
    /// # Errors
    /// Returns an error if the driver process cannot be spawned or identified, or the slot
    /// already holds a record.
    fn start(&mut self, job: &mut Job) -> std::io::Result<Box<dyn Check>>;
    /// Whether this build's `trustd` currently holds the socket.
    fn trustd_held(&self) -> bool;
    /// How many times so far the `trustd` this run held went away under it
    /// ([`trustd::Keeper::lost`]): a check across which it rose lost its prover.
    fn trustd_lost(&self) -> u32 {
        0
    }
    /// Probes the current `trustd` socket state (`<none|ours|foreign|silent>\t<detail>`).
    fn probe_trustd(&mut self) -> String;
    /// Whether the run was interrupted by a signal.
    fn interrupted(&self) -> bool;
    /// Elapsed time since slot execution began.
    fn now(&self) -> Duration;
    /// Sleeps for one poll interval.
    fn pause(&mut self);
    /// Whether slot `slot` is held, so no check may start in it ([`slot_held`]).
    fn held(&self, slot: usize) -> bool;
    /// The host's one-minute load average now; `None` when it cannot be read.
    fn load(&self) -> Option<f64>;
    /// Admits `job`'s memory claim ([`Job::need`]) at the launch gate: the claim, held until
    /// the check ends, or `None` while it waits for room or for its turn at the ledger's lock
    /// ([`Admission::admit`]).
    fn admit(&mut self, job: &Job) -> Option<Admitted>;
    /// What the check at the launch gate waits for, when one waits ([`Admission::waiting`]);
    /// [`run_slots`] asks only when its last pass found the gate's head waiting.
    fn waiting(&self) -> Option<String> {
        None
    }
    /// The run was interrupted ([`Checks::interrupted`]): the check that waits at the launch
    /// gate, if one does, is given up, its admission row naming the signal that interrupted the
    /// run ([`Admission::stopped`]). [`run_slots`] calls it once, as it stops.
    fn given_up(&mut self);
}

/// A check's admitted memory claim in the capacity ledger ([`aterm_capacity::Claim`]), held for
/// as long as the check runs and let go when it ends; none when nothing was claimed.
#[derive(Debug, Default)]
pub struct Admitted {
    /// Held only to be let go when the check ends (its drop).
    _claim: Option<aterm_capacity::Claim>,
}

impl Admitted {
    /// A check admitted with no claim.
    #[must_use]
    pub const fn unclaimed() -> Admitted {
        Admitted { _claim: None }
    }

    /// A check admitted holding `claim`.
    const fn holding(claim: aterm_capacity::Claim) -> Admitted {
        Admitted {
            _claim: Some(claim),
        }
    }

    /// The cover marker of the claim it holds ([`aterm_capacity::Record::cover_marker`]), which
    /// its check's builds are covered by alone; `None` with no claim.
    #[must_use]
    pub fn cover(&self) -> Option<String> {
        self._claim.as_ref().map(|c| c.record().cover_marker())
    }
}

/// THE LAUNCH GATE'S MEMORY ADMISSION (`docs/DESIGN-adaptive-build-2026-10-24.md`, TRUST-MEM):
/// one claim per check in this user's capacity ledger ([`aterm_capacity`]), its need the
/// check's [`Job::need`]. The check at the head of the queue enters the ledger as a
/// [`aterm_capacity::Waiter`] and is looked at once a [`aterm_capacity::Pace::poll`] until its
/// need fits beside every claim ahead of it, this run's own running checks' included: the
/// blocking `aterm_capacity::admit` would hold the slots still, so no running check could end
/// and let its claim go. While it waits it says, once a minute, what it is short of and who
/// holds it. Under `--admit now` each claim is taken at once, saying what it would have waited
/// for. A claim the machine can never admit ([`aterm_capacity::never`]: all the memory it can
/// ever have available or more, its RAM less what is wired) and that is not below the plan's
/// bound either ([`Admission::held_to`]), which the plan refuses up front unless
/// `--try-refused`, waits for nothing: it is taken at once, saying so. One below the bound
/// was admitted by the plan while less was wired, and waits for that to fall like any other.
/// A claim taken at once is still taken only in its turn ([`aterm_capacity::Waiter::force`]):
/// a look another admission's hold on the ledger's lock kept from deciding is tried again at
/// the next pass, as a waiting head's is, so the gate never holds the slots still and an
/// interrupt ends the run there ([`Checks::interrupted`]).
/// With no ledger (no home) or one that cannot be written, a check starts unclaimed and says
/// so.
///
/// Each claim asked writes one `kind=admission` row (Continuum TM-1, [`aterm_capacity::telemetry`])
/// where its ledger is this user's own: at the look that admits it, or at the end that leaves it
/// unclaimed (its ledger unusable, its claim file gone, its timing unqualified), never one a look.
/// A check given up while it waits (the run interrupted, [`Self::stopped`], or the gate dropped)
/// writes one too, `outcome=fail`.
pub struct Admission<'m> {
    ledger: Option<aterm_capacity::Ledger>,
    machine: &'m dyn aterm_capacity::Machine,
    mode: aterm_capacity::Admit,
    pace: aterm_capacity::Pace,
    /// The bound the run's plan held each claim below (`trust_facts::Host::admits_below`); 0
    /// where none was read.
    bound: u64,
    head: Option<Head>,
}

/// The check at the head of the queue, waiting in the ledger.
struct Head {
    name: String,
    waiter: aterm_capacity::Waiter,
    /// Its admission's row, owed from its entry to its decision; given up undecided, it writes
    /// that of a wait stopped.
    owed: aterm_capacity::telemetry::Owed,
    /// How it is taken, decided as it entered.
    taken: Taken,
    /// When it entered, on the slots' clock ([`Checks::now`]).
    entered: Duration,
    /// When it was last looked at; `None` before the first look.
    looked: Option<Duration>,
    /// When it last said it waits.
    said: Option<Duration>,
    /// What it waited for at its last look, its holders named.
    short: Option<String>,
}

/// How the check at the head of the queue is taken.
#[derive(Debug, PartialEq, Eq)]
enum Taken {
    /// Once its need fits beside every claim ahead of it.
    Fits,
    /// Room or not (`--admit now`), at its first look that gets the ledger's lock, saying what it
    /// would have waited for, or that it can never fit (why: a need below the plan's bound, said
    /// as it entered).
    Now(Option<String>),
    /// Room or not, at its first look that gets the ledger's lock: it can never be admitted
    /// here, as it said when it entered.
    Never,
}

impl<'m> Admission<'m> {
    /// Admission in `ledger` (none: every check starts unclaimed) on `machine`.
    #[must_use]
    pub fn new(
        ledger: Option<aterm_capacity::Ledger>,
        machine: &'m dyn aterm_capacity::Machine,
        mode: aterm_capacity::Admit,
        pace: aterm_capacity::Pace,
    ) -> Admission<'m> {
        Admission {
            ledger,
            machine,
            mode,
            pace,
            bound: 0,
            head: None,
        }
    }

    /// This admission held to the plan's `bound` (`trust_facts::Host::admits_below`, as the
    /// plan read it): a claim below it that can never fit now waits rather than starts at once,
    /// since only more being wired than when the plan read it keeps it out.
    #[must_use]
    pub const fn held_to(mut self, bound: u64) -> Admission<'m> {
        self.bound = bound;
        self
    }

    /// Admits `job` at `now` (the slots' clock), saying through `say` what it waits for: its
    /// claim, or `None` while it waits. A job that claims nothing is admitted at once.
    pub fn admit(
        &mut self,
        job: &Job,
        now: Duration,
        say: &mut dyn FnMut(&str),
    ) -> Option<Admitted> {
        use aterm_capacity::{Admit, Error, Polled, Short, span};
        if job.need == 0 {
            return Some(Admitted::unclaimed());
        }
        let what = format!("trust check {}", job.name);
        let Some(ledger) = &self.ledger else {
            say(&format!(
                "capacity: {what} starts unclaimed: there is no home to keep the capacity ledger under"
            ));
            return Some(Admitted::unclaimed());
        };
        // Another check at the gate gives up the one that waited there, whose row says so as it
        // goes ([`aterm_capacity::telemetry::Owed`]).
        if self.head.as_ref().is_some_and(|h| h.name != job.name) {
            self.head = None;
        }
        let need = aterm_capacity::Need::new(what.clone())
            .mem(job.need)
            .covers();
        // A need no amount of waiting could meet would hold its place at the head of this
        // run and of every claimant's queue on the machine for good. The plan refused up front
        // every claim not below its bound, so one below it that can never fit now fit when the
        // plan read the machine: more is wired since, and it waits for that to fall.
        let mut never = self
            .head
            .is_none()
            .then(|| aterm_capacity::never(&need, &self.machine.read(None)))
            .flatten();
        let below = never.take_if(|_| job.need < self.bound);
        if let Some(why) = &below {
            say(&format!(
                "capacity: {why}; {} could be available when this run was planned, so it waits for what is wired to fall",
                aterm_capacity::gib(self.bound)
            ));
        }
        if let Some(why) = &never {
            say(&format!(
                "capacity: {why}: it can never be admitted here, so it starts at once and holds its claim"
            ));
        }
        let taken = if never.is_some() {
            Taken::Never
        } else if self.mode == Admit::Now {
            Taken::Now(below)
        } else {
            Taken::Fits
        };
        let mut head = match self.head.take() {
            Some(head) => head,
            None => {
                let owed = aterm_capacity::telemetry::Owed::of(ledger, &need);
                match aterm_capacity::Waiter::enter(ledger, self.machine, need) {
                    Ok(waiter) => Head {
                        name: job.name.clone(),
                        waiter,
                        owed,
                        taken,
                        entered: now,
                        looked: None,
                        said: None,
                        short: None,
                    },
                    Err(e) => {
                        say(&format!(
                            "capacity: {what} starts unclaimed: the capacity ledger could not be used: {e}"
                        ));
                        owed.decided(Err(&Error::Ledger(e)));
                        return Some(Admitted::unclaimed());
                    }
                }
            }
        };
        if head
            .looked
            .is_some_and(|t| now.saturating_sub(t) < self.pace.poll)
        {
            self.head = Some(head);
            return None;
        }
        let waited = now.saturating_sub(head.entered);
        if head.taken != Taken::Fits {
            return match head.waiter.force(self.machine) {
                Ok(aterm_capacity::Forced::Admitted(claim, would)) => {
                    head.owed.decided(Ok(&claim));
                    if head.said.is_some() {
                        say(&format!("capacity: {what} admitted after {}", span(waited)));
                    }
                    match (&head.taken, would) {
                        (Taken::Now(Some(why)), _) => say(&format!(
                            "capacity: {what} admitted by --admit now, though it can never fit: {why}"
                        )),
                        (Taken::Now(None), Some(short)) => say(&format!(
                            "capacity: {what} admitted by --admit now; it would wait: {short}"
                        )),
                        _ => {}
                    }
                    Some(Admitted::holding(claim))
                }
                // Another admission held the ledger's lock while it decided, so it decided
                // nothing: look again at the next pass, as a waiting head does, and say so
                // only once it has lasted a minute.
                Ok(aterm_capacity::Forced::Turn(waiter, short)) => {
                    head.waiter = waiter;
                    if waited >= self.pace.line_every
                        && head
                            .said
                            .is_none_or(|t| now.saturating_sub(t) >= self.pace.line_every)
                    {
                        say(&format!(
                            "capacity: {what} waits ({}): {short}",
                            span(waited)
                        ));
                        head.said = Some(now);
                    }
                    self.head = Some(head);
                    None
                }
                Err(e) => {
                    say(&format!("capacity: {what} starts unclaimed: {e}"));
                    head.owed.decided(Err(&Error::Ledger(e)));
                    Some(Admitted::unclaimed())
                }
            };
        }
        match head.waiter.poll(self.machine) {
            // Its claim file was removed (the ledger's directory cleaned): no look could ever
            // admit it, so it starts unclaimed, saying why, as a check whose ledger cannot be
            // used does.
            Polled::Gone(e) => {
                say(&format!(
                    "capacity: {what} starts unclaimed after waiting {} at the launch gate: {e}",
                    span(waited)
                ));
                head.owed.decided(Err(&Error::Ledger(e)));
                Some(Admitted::unclaimed())
            }
            // It has left the queue: the next pass enters it again, a new admission.
            Polled::Unqualified(why) => {
                say(&format!("capacity: {what} timing admission refused: {why}"));
                head.owed.decided(Err(&Error::Timing(why)));
                None
            }
            Polled::Admitted(claim) => {
                head.owed.decided(Ok(&claim));
                if head.said.is_some() {
                    say(&format!("capacity: {what} admitted after {}", span(waited)));
                }
                Some(Admitted::holding(claim))
            }
            Polled::Waiting(waiter, short) => {
                head.waiter = waiter;
                // Another waiter held the ledger's lock for its own look: look again at the
                // next pass, and say so only once it has lasted a minute.
                let turn = matches!(short.shorts[..], [Short::Turn { .. }]);
                head.looked = (!turn).then_some(now);
                if !turn {
                    head.short = Some(format!(
                        "{what} waited {} at the launch gate: {short}",
                        span(waited)
                    ));
                }
                let due = head
                    .said
                    .is_none_or(|t| now.saturating_sub(t) >= self.pace.line_every);
                if due && (!turn || waited >= self.pace.line_every) {
                    say(&format!(
                        "capacity: {what} waits ({}): {short}",
                        span(waited)
                    ));
                    head.said = Some(now);
                }
                self.head = Some(head);
                None
            }
        }
    }
}

impl Admission<'_> {
    /// What the check at the head of the queue waited for at its last look, its holders named:
    /// what a run that must end while it waits says it ended on.
    #[must_use]
    pub fn waiting(&self) -> Option<String> {
        self.head.as_ref().and_then(|h| h.short.clone())
    }

    /// The run stopped, by `signal` where one interrupted it, while the check at the head of the
    /// queue may wait: that wait ends, its claim leaving the queue, and its admission row says
    /// so ([`aterm_capacity::telemetry::Owed::stopped`]). With no check waiting, nothing.
    pub fn stopped(&mut self, signal: Option<i32>) {
        if let Some(Head { waiter, owed, .. }) = self.head.take() {
            drop(waiter);
            owed.stopped(signal);
        }
    }
}

/// Completed check result reported by [`run_slots`].
#[derive(Clone, Debug)]
pub struct Finished {
    pub job: Job,
    pub ended: Ended,
    pub wall: Duration,
    /// Whether the check was terminated for exceeding its wall-clock budget.
    pub over_budget: bool,
    /// Whether the run's stop ended it ([`Stopped`]: an interrupt, or a check that could not
    /// start): it decides nothing, and is handed on only for what it measured.
    pub interrupted: bool,
    /// The host's mean one-minute load over the polls the check ran through ([`Checks::load`]);
    /// `None` when none could be read.
    pub load: Option<f64>,
    /// Whether the `trustd` the run held went away while the check ran ([`Checks::trustd_lost`]).
    pub trustd_lost: bool,
}

/// Reason [`run_slots`] stopped before draining its queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// Interrupted; while a check waited at the launch gate, what it waited for, its holders named.
    Interrupted(Option<String>),
    /// A check process could not be spawned.
    Unstartable(String),
}

/// Runs `jobs` across `slots` concurrent slots, in their order, each check in the
/// lowest-numbered slot no check runs in and none holds ([`Checks::held`]): a slot a check was
/// killed or failed in, or whose record another run left, is held, so the next check starts in
/// another. A check starts only once its memory claim is admitted ([`Checks::admit`]), and
/// holds it until it ends; the checks behind it wait with it. Every check that started is
/// handed to `done` once it ends, those a stop ends marked [`Finished::interrupted`].
///
/// # Errors
/// Returns [`Stopped`] if interrupted or if a check cannot be started.
pub fn run_slots(
    jobs: Vec<Job>,
    slots: usize,
    budget: Duration,
    checks: &mut dyn Checks,
    done: &mut dyn FnMut(Finished, &mut dyn Checks),
) -> Result<(), Stopped> {
    struct Running {
        job: Job,
        check: Box<dyn Check>,
        since: Duration,
        /// The load readings taken while it ran: their sum and count.
        load: (f64, u32),
        /// Its memory claim, let go when it ends.
        _claim: Admitted,
        /// [`Checks::trustd_lost`] as it started.
        lost: u32,
    }
    /// `r`, ended as `ended`, as `done` is handed it, and its claim, held until `done` has it.
    fn finished(
        r: Running,
        ended: Ended,
        (over_budget, interrupted): (bool, bool),
        checks: &dyn Checks,
    ) -> (Finished, Admitted) {
        let f = Finished {
            wall: checks.now().saturating_sub(r.since),
            job: r.job,
            ended,
            over_budget,
            interrupted,
            load: (r.load.1 > 0).then(|| r.load.0 / f64::from(r.load.1)),
            trustd_lost: checks.trustd_lost() > r.lost,
        };
        (f, r._claim)
    }
    /// Ends every running check, each handed to `done` as interrupted.
    fn stop_all(
        running: &mut Vec<Running>,
        checks: &mut dyn Checks,
        done: &mut dyn FnMut(Finished, &mut dyn Checks),
    ) {
        for mut r in running.drain(..) {
            let ended = r.check.end();
            let (f, _claim) = finished(r, ended, (false, true), checks);
            done(f, checks);
        }
    }
    let mut queue: VecDeque<Job> = jobs.into();
    let slots = slots.max(1);
    let mut running: Vec<Running> = Vec::new();
    // Whether the last pass asked the launch gate and its head still waits: only then does what
    // the gate says it waits for stand, not while the head waits for a slot or for trustd.
    let mut gated = false;
    loop {
        if checks.interrupted() {
            stop_all(&mut running, checks, done);
            let waiting = gated.then(|| checks.waiting()).flatten();
            checks.given_up();
            return Err(Stopped::Interrupted(waiting));
        }
        gated = false;
        while running.len() < slots && (running.is_empty() || checks.trustd_held()) {
            let Some(slot) = (0..usize::MAX)
                .find(|n| !running.iter().any(|r| r.job.slot == *n) && !checks.held(*n))
            else {
                break;
            };
            let Some(claim) = queue.front().and_then(|job| checks.admit(job)) else {
                gated = !queue.is_empty();
                break;
            };
            let Some(mut job) = queue.pop_front() else {
                break;
            };
            job.slot = slot;
            job.cover = claim.cover();
            match checks.start(&mut job) {
                Ok(check) => running.push(Running {
                    job,
                    check,
                    since: checks.now(),
                    load: (0.0, 0),
                    _claim: claim,
                    lost: checks.trustd_lost(),
                }),
                Err(e) => {
                    stop_all(&mut running, checks, done);
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
        if let Some(l) = checks.load() {
            for r in &mut running {
                r.load = (r.load.0 + l, r.load.1 + 1);
            }
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
                let (f, _claim) = finished(r, ended, (over_budget, false), checks);
                done(f, checks);
            } else {
                i += 1;
            }
        }
        checks.pause();
    }
}

/// How `f` ran on a host of `cores` cores ([`trust_facts::Pace`]).
#[must_use]
pub fn pace_of(f: &Finished, cores: usize) -> trust_facts::Pace {
    let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    trust_facts::Pace {
        cpu_ms: ms(f.ended.cpu),
        wall_ms: ms(f.wall),
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a load average is finite and non-negative, and hundredths of it fit"
        )]
        load: f.load.map(|l| (l * 100.0).round() as u64),
        cores: cores as u64,
        threads: f.ended.threads,
    }
}

/// How often a running check's processes are read for their threads' times ([`TreeTimes`]).
const THREADS_EVERY: Duration = Duration::from_secs(5);

/// One process's latest read: its CPU and runnable time in nanoseconds, and where it ran.
type ProcRead = (u64, u64, Option<aterm_sysprobe::CoreSplit>);

/// A check's processes' CPU and runnable time ([`trust_facts::Threads`]), and on a host with two
/// kinds of core where it ran, from reads of the process table while it runs: each process's
/// latest read, by pid and start, summed at the end, so one that ended between reads keeps what
/// it last showed. Beside them, the largest footprint any of them was read with ([`Self::peak`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeTimes {
    seen: BTreeMap<(u32, u64), ProcRead>,
    peak: u64,
}

impl TreeTimes {
    /// Takes from `procs` `root` and every process under it, by parent, that a read showed both
    /// times of, and the largest footprint any of them has had.
    pub fn take(&mut self, root: u32, procs: &[aterm_sysprobe::procs::ProcessReading]) {
        let mut under: BTreeMap<u32, Vec<&aterm_sysprobe::procs::ProcessReading>> = BTreeMap::new();
        for p in procs.iter().filter(|p| p.pid != p.ppid) {
            under.entry(p.ppid).or_default().push(p);
        }
        let mut next: Vec<_> = procs.iter().filter(|p| p.pid == root).collect();
        let mut been = BTreeSet::new();
        while let Some(p) = next.pop() {
            if !been.insert(p.pid) {
                continue;
            }
            if let (Some(start), Some(cpu), Some(runnable)) = (p.start, p.cpu_ns, p.runnable_ns) {
                self.seen.insert((p.pid, start), (cpu, runnable, p.cores));
            }
            self.peak = self.peak.max(p.peak_footprint.unwrap_or(0));
            next.extend(under.get(&p.pid).into_iter().flatten().copied());
        }
    }

    /// The sums, times in milliseconds; `None` while nothing was taken. Where they ran is
    /// summed only where every process taken was read with it.
    #[must_use]
    pub fn threads(&self) -> Option<trust_facts::Threads> {
        let sum = |pick: &dyn Fn(&ProcRead) -> u64| {
            self.seen.values().map(pick).fold(0u64, u64::saturating_add)
        };
        let split =
            |pick: fn(&aterm_sysprobe::CoreSplit) -> u64| sum(&|t| t.2.as_ref().map_or(0, pick));
        let cores = self
            .seen
            .values()
            .all(|t| t.2.is_some())
            .then(|| trust_facts::Cores {
                p_ms: split(|c| c.p_cpu_ns) / 1_000_000,
                instructions: split(|c| c.instructions),
                p_instructions: split(|c| c.p_instructions),
            });
        (!self.seen.is_empty()).then(|| trust_facts::Threads {
            ran_ms: sum(&|t| t.0) / 1_000_000,
            runnable_ms: sum(&|t| t.1) / 1_000_000,
            cores,
        })
    }

    /// The largest footprint, in bytes, any process of the tree was read with (macOS
    /// `ri_lifetime_max_phys_footprint`, which counts what the kernel compressed or swapped of
    /// it, as CAP-1 meters a claim's holder); 0 where no read had one (off macOS). Up to
    /// [`THREADS_EVERY`] of a process's growth before it ends goes unread.
    #[must_use]
    pub fn peak(&self) -> u64 {
        self.peak
    }
}

/// Writes sidecar status, pace, budget, and `trustd` marks for the judge after a check ends: a
/// `.trustd` mark when targo would not start it, and a `.trustd-lost` mark, what serves the
/// socket now, when the `trustd` it ran beside went away while it ran.
///
/// # Errors
/// Returns an error if any sidecar mark file cannot be written.
pub fn record(
    f: &Finished,
    budget: Duration,
    cores: usize,
    probe: &mut dyn FnMut() -> String,
) -> std::io::Result<()> {
    let log = &f.job.log;
    let beside = |ext: &str| PathBuf::from(format!("{}.{ext}", log.display()));
    crate::durable::write_fresh(&beside("rc"), format!("{}\n", f.ended.code))?;
    crate::durable::write_fresh(&beside("pace"), format!("{}\n", pace_of(f, cores).text()))?;
    if f.over_budget {
        crate::durable::write_fresh(&beside("budget"), format!("{}\n", budget.as_secs()))?;
    }
    let text = crate::durable::read_file(log).unwrap_or_default();
    let refused = text
        .windows(trust_facts::TRUSTD_REFUSED.len())
        .any(|w| w == trust_facts::TRUSTD_REFUSED.as_bytes());
    if refused {
        crate::durable::write_fresh(&beside("trustd"), format!("{}\n", probe()))?;
    }
    if f.trustd_lost {
        // What serves the socket now that the trustd the check ran beside went away.
        let now = probe();
        let what = now.split_once('\t').map_or(now.as_str(), |(_, what)| what);
        crate::durable::write_fresh(&beside("trustd-lost"), format!("{what}\n"))?;
    }
    if let Some(lock) = &f.job.lock
        && lock.created
    {
        let _ = fs::remove_file(&lock.path);
    }
    Ok(())
}

/// Host prover, metadata resolver, and slot runner abstraction.
trait Bench {
    /// Resolves the toolchain driver for `cfg`.
    ///
    /// # Errors
    /// Why there is none.
    fn driver(&mut self, cfg: &Config) -> Result<Driver, String>;
    /// Runs `targo metadata --format-version 1 --all-features` in `repo` using `driver`.
    ///
    /// # Errors
    /// Why it printed none.
    fn metadata(&mut self, driver: &Driver, repo: &Path) -> Result<Vec<u8>, String>;
    /// Cargo home directory (`$HOME/.cargo`) read by lane compiles.
    fn cargo_home(&self) -> String;
    /// Base cache directory for fallback target roots ([`cache_base`]).
    fn cache(&self) -> Option<PathBuf>;
    /// This host's RAM in bytes (0 unread) and whether it was read as Linux's `MemTotal`.
    fn memory(&self) -> (u64, bool);
    /// The most memory that can ever be available here, bytes: the RAM less what is wired
    /// ([`aterm_capacity::Reading::reachable`]); 0 unread.
    fn reach(&self) -> u64;
    /// The host's one-minute load average now; `None` when it cannot be read.
    fn load(&self) -> Option<f64>;
    /// Whether `driver`'s `trustc -Z help` lists the verifier's memory ceiling flag
    /// ([`trust_facts::CEILING_FLAG`]): `Err` names why it could not be read.
    ///
    /// # Errors
    /// trustc could not be run, or printed nothing.
    fn takes_ceiling(&mut self, driver: &Driver) -> Result<bool, String>;
    /// Waits for trustd's socket to serve `prep`'s prover's `trustd` or nothing
    /// ([`trustd::wait_usable`]): a run with checks to make waits here, before it claims its
    /// disk, so a wait with no deadline holds no claim other builds wait behind.
    ///
    /// # Errors
    /// What held the socket when `--no-wait` would not wait or an interrupt ended the wait.
    fn trustd(&mut self, cfg: &Config, prep: &Prepared, out: &mut dyn Write) -> Result<(), String>;
    /// Runs `jobs` across `slots` beside this prover's `trustd`.
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
    /// Claims `run`'s disk in the capacity ledger before the checks start, as `mode` admits
    /// (waiting for room, or at once), saying on `out` what it waits for. `None`: no claim was
    /// made.
    ///
    /// # Errors
    /// The need can never fit on the volume ([`atpkg::admit_disk::Error::Never`]), is more than
    /// it has free above its floor once the relief ran with no scratch claim held there that
    /// could meet it
    /// ([`atpkg::admit_disk::Error::Unmet`]), an interrupt ended the wait
    /// ([`atpkg::admit_disk::Error::Stopped`]), or the ledger could not be used.
    fn disk(
        &mut self,
        run: atpkg::admit_disk::Run,
        mode: atpkg::admit_disk::Admit,
        out: &mut dyn Write,
    ) -> Result<Option<atpkg::admit_disk::Admitted>, atpkg::admit_disk::Error>;
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

    fn memory(&self) -> (u64, bool) {
        host_memory()
    }

    fn reach(&self) -> u64 {
        use aterm_capacity::Machine as _;
        aterm_capacity::Live.read(None).reachable().unwrap_or(0)
    }

    fn load(&self) -> Option<f64> {
        aterm_sysprobe::load_average().map(|l| l[0])
    }

    fn takes_ceiling(&mut self, driver: &Driver) -> Result<bool, String> {
        let trustc = driver.bin.join("trustc");
        let o = command(&trustc)
            .args(["-Z", "help"])
            .env_clear()
            .envs(lane_env(&driver.bin, &host_var))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| format!("{} -Z help: {e}", trustc.display()))?;
        let help = String::from_utf8_lossy(&o.stdout);
        if !o.status.success() || help.trim().is_empty() {
            return Err(format!(
                "{} -Z help exited {} and printed {} bytes",
                trustc.display(),
                o.status,
                o.stdout.len()
            ));
        }
        Ok(lists_ceiling(&help))
    }

    fn trustd(&mut self, cfg: &Config, prep: &Prepared, out: &mut dyn Write) -> Result<(), String> {
        let want = trustd::build_digest(&prep.driver.bin.join("trustd"));
        let wait = if cfg.no_wait {
            named_lock::Wait::No
        } else {
            named_lock::Wait::Block
        };
        trustd::wait_usable(&trustd::socket(), &want, wait, out, &|| {
            crate::interrupt::caught().is_some()
        })
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
        // Again: another build's trustd can have come while the run waited for its disk.
        self.trustd(cfg, prep, out)?;
        let keeper = trustd::Keeper::start(
            sock.clone(),
            want.clone(),
            prep.logdir.join(".trustd-grants"),
            prep.logdir.join(".trustd-seen"),
        );
        let mut real = RealChecks::new(
            &prep.driver,
            &cfg.repo,
            &prep.root,
            &keeper,
            &sock,
            &want,
            Admission::new(
                aterm_capacity::Ledger::user(),
                &aterm_capacity::Live,
                cfg.admit,
                aterm_capacity::Pace::default(),
            )
            .held_to(prep.bound),
            Box::new(&mut *out),
        );
        let ran = run_slots(jobs, slots, cfg.budget, &mut real, done);
        let locks = std::mem::take(&mut real.locks);
        drop(real);
        drop(keeper);
        for (lock, created) in locks {
            if created {
                let _ = fs::remove_file(lock);
            }
        }
        if matches!(ran, Err(Stopped::Interrupted(_))) {
            reap_stale_checks(&prep.root);
        }
        ran.map_err(stopped)
    }

    fn disk(
        &mut self,
        run: atpkg::admit_disk::Run,
        mode: atpkg::admit_disk::Admit,
        out: &mut dyn Write,
    ) -> Result<Option<atpkg::admit_disk::Admitted>, atpkg::admit_disk::Error> {
        atpkg::admit_disk::admit(run, mode, &mut |line| {
            let _ = writeln!(out, "trust: {line}");
        })
        .map(Some)
    }
}

/// Why slots that stopped decided nothing.
fn stopped(s: Stopped) -> String {
    match s {
        Stopped::Interrupted(None) => "interrupted; nothing was decided".into(),
        Stopped::Interrupted(Some(waiting)) => {
            format!("interrupted while {waiting}; nothing was decided")
        }
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
    /// The bound the plan held each claim below ([`trust_facts::Host::admits_below`]), which
    /// the launch gate is held to ([`Admission::held_to`]).
    bound: u64,
    cores: usize,
    costs_path: Option<PathBuf>,
    /// Each library's last recorded cost under this run's prover ([`read_costs`]): the order,
    /// the claims and the up-front refusals for a claim not below what can ever be available
    /// are all read from its rows.
    costs: Costs,
    /// The host's one-minute load as the run began ([`load_word`]).
    load_start: String,
    /// All library row names in the resolved graph ([`seed_rows`]).
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

/// Executes the trust lane (`ship trust run`), writing progress and verdict output to `out`.
pub fn run(cfg: &Config, out: &mut dyn Write) -> Outcome {
    crate::interrupt::install();
    run_on(cfg, &mut Machine, out)
}

fn run_on(cfg: &Config, bench: &mut dyn Bench, out: &mut dyn Write) -> Outcome {
    let ran = match prepare(cfg, bench, out) {
        Ok(prep) => {
            let usable = if work(&prep).is_empty() {
                Ok(())
            } else {
                bench.trustd(cfg, &prep, out).map_err(Stop::CouldNotRun)
            };
            let judged = match usable.and_then(|()| claim_disk(cfg, &prep.root, bench, out)) {
                Ok(disk) => {
                    let judged =
                        check_and_conclude(cfg, &prep, bench, out).map_err(Stop::CouldNotRun);
                    if let Some(disk) = disk {
                        let claimed = disk.estimate;
                        let took = disk.finish();
                        let _ = writeln!(
                            out,
                            "trust: disk: claimed {claimed}; this run's target root held {} at \
                             most and grew by {} before its slots were emptied",
                            atpkg::admit_disk::size(took.peak),
                            atpkg::admit_disk::size(took.grew)
                        );
                    }
                    judged
                }
                Err(stop) => Err(stop),
            };
            let kept = clear_slots(&prep.root).unwrap_or_default();
            if !kept.is_empty() {
                let _ = writeln!(out, "trust: {}", held_words(&kept));
            }
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

/// `trust: prover <bin> (<its --version>)`.
fn say_prover(out: &mut dyn Write, driver: &Driver) {
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
}

fn prepare(cfg: &Config, bench: &mut dyn Bench, out: &mut dyn Write) -> Result<Prepared, Stop> {
    let driver = bench.driver(cfg)?;
    say_prover(out, &driver);
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
    let canonical_repo = canonical_repo(cfg);
    let me = named_lock::Holder::me(&run_verb(&cfg.scope), &canonical_repo);
    let wait = if cfg.no_wait {
        named_lock::Wait::No
    } else {
        named_lock::Wait::Block
    };
    let lock = RunLock::wait(&root.join(".run.lock"), &me, wait, out)?;
    // A wait can outlast the prover resolved before it (a toolchain update, then a store GC
    // that takes the old build): a run that waited resolves it again with the root its own.
    let driver = if lock.0.waited().is_zero() {
        driver
    } else {
        let now = bench.driver(cfg)?;
        if (&now.bin, &now.id) != (&driver.bin, &driver.id) {
            say_prover(out, &now);
        }
        now
    };
    if let Some(mine) = &mine {
        if *mine == root {
            write_checkout_record(&root, &canonical_repo)
                .map_err(|e| format!("{}: {e}", root.join(CHECKOUT_RECORD).display()))?;
            use std::os::unix::fs::MetadataExt as _;
            let stable_lock = lock
                .0
                .metadata()
                .ok()
                .zip(owned_cache_file(&root.join(".run.lock")))
                .is_some_and(|(held, named)| {
                    (held.dev(), held.ino()) == (named.dev(), named.ino())
                });
            if !stable_lock || cleanup_checkout(&root).as_deref() != Some(canonical_repo.as_path())
            {
                return Err(format!("{}: fallback cache custody changed", root.display()).into());
            }
        } else if cleanup_checkout(mine).as_deref() == Some(canonical_repo.as_path())
            && retire(mine, &me)
        {
            let _ = writeln!(
                out,
                "trust: removed {}, this checkout's fallback target root, now that {} is admitted",
                mine.display(),
                root.display()
            );
        }
        let orphans = mine
            .parent()
            .map(|cache| prune_orphans(cache, mine, &me))
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
    let files = matches!(cfg.scope, Scope::WholeTree | Scope::Remeasure);
    reap_stale_checks(&root);
    let unemptied = match clear_slots(&root) {
        Ok(kept) if kept.is_empty() => None,
        Ok(kept) => Some(held_words(&kept)),
        Err(e) => Some(format!(
            "{}: a slot could not be emptied ({e})",
            root.display()
        )),
    };
    if let Some(why) = unemptied {
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
    let gone = if files {
        let floors = cfg.repo.join(trust_facts::RATCHET);
        let text = crate::durable::read_text(&floors)
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
    let (mem, memtotal) = bench.memory();
    let reach = bench.reach();
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let jobs = cfg.jobs.unwrap_or_else(|| auto_slots(cores));
    let ceiling = driver_ceiling(bench, &driver, state.as_deref(), out);
    let load_start = load_word(bench.load());
    let costs_path = state.as_ref().map(|s| s.join("trust-costs.tsv"));
    let costs = read_costs(
        costs_path.as_deref(),
        &cfg.repo.join(trust_facts::COSTS),
        &one_line(&driver.id),
    );
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
        ("host-reach", reach.to_string()),
        ("driver-ceiling", if ceiling { "yes" } else { "no" }.into()),
        ("now", utc_now()),
        ("load-start", load_start.clone()),
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
    let parsed = Plan::parse(&plan)?;
    let bound = parsed.facts.host.admits_below();
    let peaks: BTreeMap<String, trust_facts::Peak> = costs
        .rows
        .keys()
        .chain(costs.elsewhere.keys())
        .filter_map(|name| Some((name.clone(), costs.peak(name, bound)?)))
        .collect();
    let (admit_lines, refusals) = parsed.admit(&peaks)?;
    plan.push_str(&admit_lines);
    let plan_path = logdir.join(".plan");
    crate::durable::write_fresh(&plan_path, &plan)
        .map_err(|e| format!("{}: {e}", plan_path.display()))?;
    let refused: BTreeSet<String> = refusals.iter().map(|(n, _)| n.clone()).collect();
    for s in &selected {
        if let (Decision::Gate(_), Some(c)) = (&s.decision, costs.estimate(&s.lib.name, mem, bound))
            && !refused.contains(&s.lib.name)
        {
            let _ = writeln!(
                out,
                "trust: {}",
                estimated(&s.lib.name, c, costs.claim(&s.lib.name, mem, bound))
            );
        }
    }
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
        bound,
        cores,
        costs_path,
        costs,
        load_start,
        libraries: libs.iter().map(|l| l.name.clone()).collect(),
        root,
        _lock: lock,
    })
}

/// What a run says of `name`, no check of which under this prover has finished, whose claim
/// (`claim`, [`Costs::claim`]) another prover's row (`c`, [`Costs::estimate`]) raised: it runs
/// first, as an unmeasured library does, and names whose peak it claims on.
#[allow(clippy::cast_precision_loss)]
fn estimated(name: &str, c: &Cost, claim: u64) -> String {
    let on = c
        .when
        .get(..10)
        .map_or_else(String::new, |d| format!(" on {d}"));
    let reached = if c.cut.is_some() {
        "was cut short after reaching"
    } else {
        "peaked at"
    };
    format!(
        "{name} has no finished check under this prover: its last check under {} {reached} {:.2} GB{on}, the best estimate there is; it runs first, claiming {}",
        c.prover,
        c.maxrss as f64 / 1e9,
        aterm_capacity::gib(claim)
    )
}

/// The libraries `prep` checks: those gated that the memory guard did not refuse.
fn work(prep: &Prepared) -> Vec<&Selected> {
    prep.selected
        .iter()
        .filter(|s| matches!(s.decision, Decision::Gate(_)) && !prep.refused.contains(&s.lib.name))
        .collect()
}

/// The checks, then the judge: `Err` is why nothing was decided.
fn check_and_conclude(
    cfg: &Config,
    prep: &Prepared,
    bench: &mut dyn Bench,
    out: &mut dyn Write,
) -> Result<Outcome, String> {
    let work = work(prep);
    for stale in [".trustd-grants", ".trustd-seen"] {
        let _ = fs::remove_file(prep.logdir.join(stale));
    }
    for s in &work {
        let log = prep.logdir.join(format!("{}.log", s.lib.spec));
        for ext in [
            "",
            ".rc",
            ".pace",
            ".trustd",
            ".trustd-lost",
            ".budget",
            ".oblig.tsv",
            ".oblig.err",
        ] {
            let _ = fs::remove_file(format!("{}{ext}", log.display()));
        }
        let _ = fs::remove_dir_all(format!("{}.report", log.display()));
    }
    let seed = cfg.repo.join(trust_facts::COSTS);
    let costs = &prep.costs;
    let names: Vec<&str> = work.iter().map(|s| s.lib.name.as_str()).collect();
    let jobs: Vec<Job> = order(&names, &costs.rows)
        .into_iter()
        .map(|i| {
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
                need: costs.claim(&lib.name, prep.mem, prep.bound),
                lock: None,
                cover: None,
            }
        })
        .collect();
    let mut new_costs: BTreeMap<String, Cost> = BTreeMap::new();
    let mut credit = crate::trust_credit::Credit::default();
    // Each check's mean load, for the run's provenance.
    let mut loads: Vec<f64> = Vec::new();
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
        let cores = prep.cores;
        let mut done = |f: Finished, checks: &mut dyn Checks| {
            if f.interrupted {
                // Ended by the run's stop, it decides nothing: only what it measured is kept.
                if let Some(c) = cost_of(&f, &prep.driver) {
                    new_costs.insert(f.job.name.clone(), c);
                }
                return;
            }
            let mut probe = || checks.probe_trustd();
            if let Some(l) = f.load {
                loads.push(l);
            }
            let _ = record(&f, budget, cores, &mut probe);
            // Read before the slot runs another check: the package's graded record.
            credit.add(
                &f.job.name,
                crate::trust_credit::read_record(&f.job.target, package_identity(&f.job.name)),
            );
            if let Some(r) = &reducer {
                r.send(&f.job);
            }
            if let Some(c) = cost_of(&f, &prep.driver) {
                new_costs.insert(f.job.name.clone(), c);
            }
        };
        let ran = bench.checks(cfg, prep, jobs, slots, &mut done, out);
        if let Some(r) = reducer {
            r.finish(ran.is_err());
        }
        write_costs(prep.costs_path.as_deref(), &new_costs, out);
        ran?;
    }
    let mut plan = prep.plan.clone();
    plan.push_str(&format!("trustd {}\n", trustd::socket().display()));
    let load_end = load_word(bench.load());
    push_kv(&mut plan, "load-end", &load_end);
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
            (
                "load",
                if loads.is_empty() {
                    format!("{} at the start, {load_end} at the end", prep.load_start)
                } else {
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "a count of checks is far below 2^52"
                    )]
                    let n = loads.len() as f64;
                    format!(
                        "{:.2} over its checks, {} at the start, {load_end} at the end",
                        loads.iter().sum::<f64>() / n,
                        prep.load_start
                    )
                },
            ),
            ("jobs", prep.jobs.to_string()),
            ("date", utc_now()),
        ] {
            push_kv(&mut plan, k, &v);
        }
    }
    crate::durable::write_fresh(&prep.plan_path, &plan)
        .map_err(|e| format!("{}: {e}", prep.plan_path.display()))?;
    let mut parsed = Plan::parse(&plan)?;
    let update = if cfg.update_ratchet {
        Update {
            reason: cfg.reason.as_deref(),
            ..Update::RAISE
        }
    } else {
        Update::NONE
    };
    let git = GitCli::new(&cfg.repo);
    let done = trust_facts::conclude(&git, &cfg.repo, &mut parsed, update)
        .map_err(|e| format!("the judge could not conclude: {e}"))?;
    if cfg.update_ratchet && !new_costs.is_empty() {
        let history = prep
            .costs_path
            .as_deref()
            .map(cost_file)
            .unwrap_or_default();
        let mut all = seed_rows(cost_file(&seed), history, &prep.libraries);
        for (name, row) in new_costs {
            keep(&mut all, &name, row);
        }
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
    Ok(with_credit(
        Outcome::of(&done.verdict),
        &cfg.repo,
        &credit,
        out,
    ))
}

/// The "proved (calculus)" counter over this run's records, judged against the current credit
/// epoch ([`crate::trust_credit`]): a regression or an unreadable record or ledger fails a run
/// that would pass, and joins a failing run's list.
fn with_credit(
    outcome: Outcome,
    repo: &Path,
    credit: &crate::trust_credit::Credit,
    out: &mut dyn Write,
) -> Outcome {
    let (line, failed) = match crate::trust_credit::current_epoch(repo) {
        Ok(Some(e)) => (
            credit.line(Some(&e)),
            crate::trust_credit::judge(&e, credit),
        ),
        Ok(None) => (credit.line(None), Vec::new()),
        Err(why) => (
            credit.line(None),
            vec![format!("credit epoch CREDIT-UNREADABLE {why}")],
        ),
    };
    let _ = writeln!(out, "trust: {line}");
    for f in &failed {
        let _ = writeln!(out, "trust: {f}");
    }
    if failed.is_empty() {
        return outcome;
    }
    match outcome {
        Outcome::Pass => Outcome::Fail(failed),
        Outcome::Fail(mut v) => {
            v.extend(failed);
            Outcome::Fail(v)
        }
        other => other,
    }
}

/// Background worker that reduces `--obligations` proof reports off the slot-polling thread.
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

    /// Waits for queued reductions to finish, or removes unstarted reports when `skip` is set.
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

/// The resource [`Cost`] of `f` under the prover `driver` names: what a check that finished
/// took (its log has a summary, a whole one, and no transport failure), or what one cut short
/// took before it ended ([`Cut`]: at its budget, by the run's stop, or by itself), each figure
/// then the least it would have taken. `None` for a cut check whose peak was not read.
fn cost_of(f: &Finished, driver: &Driver) -> Option<Cost> {
    let finished = || {
        crate::durable::open_regular_file(&f.job.log).is_ok_and(|file| {
            let m = trust_facts::Measured::of_log(BufReader::new(file), &f.ended.code.to_string());
            !m.summary.is_empty() && m.ended.is_empty() && !m.transport
        })
    };
    let cut = if f.interrupted {
        Some(Cut::Interrupted)
    } else if f.over_budget {
        Some(Cut::OverBudget)
    } else {
        (!finished()).then_some(Cut::Failed)
    };
    (cut.is_none() || f.ended.maxrss > 0).then(|| Cost {
        cpu: f.ended.cpu.as_secs(),
        wall: f.wall.as_secs(),
        maxrss: f.ended.maxrss,
        when: utc_now(),
        prover: one_line(&driver.id),
        cut,
        raised: false,
    })
}

/// Merges `new` into the cost history file at `path` ([`keep`]).
fn write_costs(path: Option<&Path>, new: &BTreeMap<String, Cost>, out: &mut dyn Write) {
    let Some(path) = path.filter(|_| !new.is_empty()) else {
        return;
    };
    let mut all = cost_file(path);
    for (name, row) in new {
        keep(&mut all, name, row.clone());
    }
    write_cost_file(path, &format!("{COSTS_HEADER}\n"), &all, out);
}

/// Atomically writes cost entries `all` under `header` to `path`, a row's prover in a sixth
/// column where it names one, how its check was cut short in a seventh where it was, and
/// [`RAISED`] in an eighth where it is [`Cost::raised`].
fn write_cost_file(path: &Path, header: &str, all: &CostRows, out: &mut dyn Write) {
    let now = utc_now();
    let mut text = header.to_string();
    for (name, c) in all
        .iter()
        .flat_map(|(n, rows)| rows.iter().map(move |c| (n, c)))
    {
        let when = if c.when.is_empty() { &now } else { &c.when };
        text.push_str(&format!(
            "{name}\t{}\t{}\t{}\t{when}",
            c.cpu, c.wall, c.maxrss
        ));
        if !c.prover.is_empty() || c.cut.is_some() {
            text.push('\t');
            text.push_str(&one_line(&c.prover));
        }
        if let Some(cut) = c.cut {
            text.push('\t');
            text.push_str(cut.word());
            if c.raised {
                text.push('\t');
                text.push_str(RAISED);
            }
        }
        text.push('\n');
    }
    let tmp = path.with_extension(format!("tsv.{}", std::process::id()));
    let written = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| write_new_temp(&tmp, text.as_bytes()))
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

/// Reduces `job`'s `--obligations` proof report beside its log and removes the raw directory
/// on success.
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
        let _ = crate::durable::write_fresh(
            Path::new(&format!("{}.oblig.err", job.log.display())),
            format!(
                "{why}\nraw report cleanup skipped for {}\n",
                report.display()
            ),
        );
        return;
    }
    let _ = fs::remove_dir_all(report);
}

/// Returns `(HEAD, HEAD^{tree})` when `repo` is strictly clean, or `None` otherwise.
fn tree_state(repo: &Path) -> Option<(String, String)> {
    if !crate::clean::strictly_clean(repo)? {
        return None;
    }
    let git = GitCli::new(repo);
    let rev = |r: &str| crate::ledger::rev_parse(&git, r).ok();
    Some((rev("HEAD")?, rev("HEAD^{tree}")?))
}

/// The disk a run claims (`docs/DESIGN-adaptive-build-2026-10-24.md`, DISK-ADMIT): its target
/// root, where its slots build and its logs go, measured, with what the last runs of its scope
/// there wrote kept under `logs` (the whole tree, its `-p` set sorted, or a remeasure: a `-p`
/// run's growth is not what a whole-tree run claims, nor the reverse), or where none of its scope
/// was measured [`COLD_DISK`] less what the root holds.
fn disk_run(cfg: &Config, root: &Path) -> atpkg::admit_disk::Run {
    let scope = match &cfg.scope {
        Scope::WholeTree => String::new(),
        Scope::Packages(p) => {
            let mut s = String::new();
            for n in p {
                s.push_str(" -p ");
                s.push_str(n);
            }
            s
        }
        Scope::Remeasure => " --remeasure".to_owned(),
    };
    let kept_by = match &cfg.scope {
        Scope::WholeTree => "whole-tree".to_owned(),
        Scope::Packages(p) => {
            let mut p = p.clone();
            p.sort_unstable();
            p.dedup();
            let mut s = String::new();
            for (i, n) in p.iter().enumerate() {
                if i > 0 {
                    s.push(' ');
                }
                s.push_str("-p ");
                s.push_str(n);
            }
            s
        }
        Scope::Remeasure => "--remeasure".to_owned(),
    };
    atpkg::admit_disk::Run {
        what: format!("ship trust run{scope}"),
        scope: kept_by,
        dirs: vec![root.to_path_buf()],
        record: root.join("logs").join(DISK_RECORD),
        cold: COLD_DISK,
        on: root.to_path_buf(),
        stuck_ends: None,
        scratch: false,
        machine: None,
    }
}

/// Claims the run's disk ([`disk_run`]) once it holds its target root and before a check
/// starts. A ledger that cannot be used is said, and the run goes on without a claim.
///
/// # Errors
/// The volume holding the root can never hold the need above its floor, has less free above it
/// than the need once the relief ran (which no wait admits), or an interrupt ended the wait:
/// COULD NOT RUN.
fn claim_disk(
    cfg: &Config,
    root: &Path,
    bench: &mut dyn Bench,
    out: &mut dyn Write,
) -> Result<Option<atpkg::admit_disk::Admitted>, Stop> {
    match bench.disk(disk_run(cfg, root), cfg.admit, out) {
        Ok(held) => Ok(held),
        Err(atpkg::admit_disk::Error::Never(why)) => Err(Stop::CouldNotRun(format!(
            "{why}; it can never be admitted there. Fix: give it a --target-root on a larger \
             volume"
        ))),
        Err(e @ atpkg::admit_disk::Error::Stopped { .. }) => Err(Stop::CouldNotRun(format!(
            "the run's disk claim was {e}; no check started, so nothing was decided"
        ))),
        Err(e @ atpkg::admit_disk::Error::Unmet { .. }) => Err(Stop::CouldNotRun(format!(
            "the run's disk claim was refused, no check started: {e}, or start it at once with \
             `--admit now`"
        ))),
        Err(e) => {
            let _ = writeln!(
                out,
                "trust: the run's disk is not claimed ({e}); it runs without a claim"
            );
            Ok(None)
        }
    }
}

/// Empties `root`'s slots, which are scratch: every check names its units by a fresh session
/// nonce, so nothing one check builds is reused and a kept slot only grows. A held slot
/// ([`slot_held`]) stays; those are returned.
///
/// # Errors
/// `root` exists and could not be read, or a slot could not be removed (the first such error).
fn clear_slots(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let rd = match fs::read_dir(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        rd => rd?,
    };
    let mut kept = Vec::new();
    let mut first = Ok(());
    for e in rd.flatten() {
        let file_name = e.file_name();
        let Some(n) = file_name.to_str().and_then(slot_number) else {
            continue;
        };
        let p = e.path();
        if slot_held(root, n) {
            kept.push(p);
        } else if let Err(e) = fs::remove_dir_all(&p)
            && first.is_ok()
        {
            first = Err(e);
        }
    }
    first.map(|()| kept)
}

/// The number `name` gives its slot, `j<digits>`.
fn slot_number(name: &str) -> Option<&str> {
    name.strip_prefix('j')
        .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether slot `j<n>` under `root` is held, so no run empties it or starts a check in it: a
/// `.check.<n>` record names a check the lane never saw end with nothing of it left, until a
/// reap drops it ([`reap_stale_checks`]), or a program a killed or failed check left there may
/// still be in its first exec ([`crate::gates::first_exec_hold`]). A run's own live checks hold
/// theirs by their records.
fn slot_held(root: &Path, n: impl std::fmt::Display) -> bool {
    fs::symlink_metadata(root.join(format!(".check.{n}"))).is_ok()
        || crate::gates::first_exec_hold(&root.join(format!("j{n}"))).is_some()
}

/// The slots under `root` that are held ([`slot_held`]).
fn held_slots(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(root) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .and_then(slot_number)
                .is_some_and(|n| slot_held(root, n))
        })
        .map(|e| e.path())
        .collect()
}

/// What each slot [`clear_slots`] kept waits on.
fn held_words(kept: &[PathBuf]) -> String {
    let hang = aterm_uds::deadline::FIRST_EXEC_HANG.as_secs() / 60;
    let words: Vec<String> = kept
        .iter()
        .map(|slot| {
            let when = crate::gates::first_exec_hold(slot).map_or_else(
                || "the next run".to_owned(),
                |left| {
                    format!(
                        "a run {} min of awake time from now",
                        left.as_secs().div_ceil(60)
                    )
                },
            );
            let record = slot
                .file_name()
                .and_then(|name| slot_number(&name.to_string_lossy()).map(str::to_owned))
                .zip(slot.parent())
                .and_then(|(n, root)| {
                    crate::durable::read_text(&root.join(format!(".check.{n}"))).ok()
                });
            let Some(rec) = record.map(|text| CheckRecord::parse(&text)) else {
                return format!(
                    "kept {}: a check that was ended mid-run or failed may have left a program \
                     there that macOS is still assessing at its first exec, and {when} empties \
                     it",
                    slot.display()
                );
            };
            if rec.dir.is_some() && rec.dir != slot.parent().and_then(dir_id) {
                format!(
                    "kept {}: its record was written under another target root, which this one \
                     copies, so no run signals what it names, and {when} empties the slot",
                    slot.display()
                )
            } else if let (true, Some((pid, _))) = (rec.lane_runs(), rec.lane) {
                format!(
                    "kept {}: run {pid}, which started the check there, still runs",
                    slot.display()
                )
            } else if let (Some(nonce), Some(_), Some(_)) = (&rec.nonce, rec.lane, rec.dir) {
                format!(
                    "kept {}: the check a stopped run left there was never seen to end, and its \
                     record holds the slot while a process carrying {CHECK_VAR}={nonce} or named \
                     by the record runs, which each run ends, or one in its root's session or \
                     using the slot is left; once a run finds none, a run {hang} min of awake \
                     time later empties the slot",
                    slot.display()
                )
            } else if let Some(nonce) = &rec.nonce {
                format!(
                    "kept {}: a record that names no run or root of its own holds the slot \
                     while a process carrying {CHECK_VAR}={nonce} runs, or one in its root's \
                     session or using the slot is left; no run signals them, and once a run \
                     finds none, a run {hang} min of awake time later empties the slot",
                    slot.display()
                )
            } else {
                format!(
                    "kept {}: an earlier build's record of the check a stopped run left there \
                     names no mark its processes carry, only its root, so no run signals \
                     anything for it; it holds the slot while a process in its root's session \
                     or using the slot is left, and once a run finds none, a run {hang} min of \
                     awake time later empties the slot",
                    slot.display()
                )
            }
        })
        .collect();
    words.join("; ")
}

/// Bytes in a check's nonce, which its record spells as twice as many hex digits.
const NONCE_BYTES: usize = 16;

/// What `.check.<n>` says of the check a run started in slot `j<n>` ([`RealChecks::start`]),
/// one line of words: `nonce=<hex> boot=<id> lane=<pid>:<serial> dir=<dev>:<ino>`, then
/// `pid=<pid> serial=<serial>` once its root is spawned, and a `kept=<pid>:<serial>` for each
/// process a sweep kept ([`keep_in_record`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CheckRecord {
    /// The mark its processes carry ([`CHECK_VAR`]); `None` in an earlier build's record,
    /// which names only its root's pid ([`earlier_pid`]).
    nonce: Option<String>,
    /// The boot it started in ([`aterm_uds::process::boot_id`]), when read.
    boot: Option<String>,
    /// The lane process that started the check, by pid and serial ([`this_lane`]): while it
    /// runs, the check is that run's.
    lane: Option<(i32, u64)>,
    /// The target root it was written in, by device and inode ([`dir_id`]): read under
    /// another root, it is a copy's.
    dir: Option<(u64, u64)>,
    /// Its root's pid and serial ([`aterm_uds::process::process_serial`]).
    root: Option<(i32, u64)>,
    /// The processes a sweep kept as the check's before signalling them
    /// ([`proc_tree::Mark::kept`]).
    kept: BTreeSet<(i32, u64)>,
}

impl CheckRecord {
    /// Reads a record; a word it does not know, or a nonce of another shape, reads as absent.
    fn parse(text: &str) -> CheckRecord {
        fn two<A: std::str::FromStr, B: std::str::FromStr>(v: &str) -> Option<(A, B)> {
            let (a, b) = v.split_once(':')?;
            Some((parse_uint(a)?, parse_uint(b)?))
        }
        let mut rec = CheckRecord::default();
        let (mut pid, mut serial) = (None, None);
        for word in text.split_whitespace() {
            match word.split_once('=') {
                Some(("nonce", v))
                    if v.len() == 2 * NONCE_BYTES
                        && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
                {
                    rec.nonce = Some(v.to_owned());
                }
                Some(("boot", v)) if !v.is_empty() && v != "-" => rec.boot = Some(v.to_owned()),
                Some(("lane", v)) => rec.lane = two(v).filter(|(p, _): &(i32, u64)| *p > 1),
                Some(("dir", v)) => rec.dir = two(v),
                Some(("pid", v)) => pid = parse_uint::<i32>(v).filter(|p| *p > 1),
                Some(("serial", v)) => serial = parse_uint::<u64>(v),
                Some(("kept", v)) => rec.kept.extend(two(v).filter(|(p, _): &(i32, u64)| *p > 1)),
                _ => {}
            }
        }
        rec.root = pid.zip(serial);
        rec
    }

    fn text(&self) -> String {
        use std::fmt::Write as _;
        let mut text = format!(
            "nonce={} boot={}",
            self.nonce.as_deref().unwrap_or("-"),
            self.boot.as_deref().unwrap_or("-")
        );
        if let Some((pid, serial)) = self.lane {
            let _ = write!(text, " lane={pid}:{serial}");
        }
        if let Some((dev, ino)) = self.dir {
            let _ = write!(text, " dir={dev}:{ino}");
        }
        if let Some((pid, serial)) = self.root {
            let _ = write!(text, " pid={pid} serial={serial}");
        }
        for (pid, serial) in &self.kept {
            let _ = write!(text, " kept={pid}:{serial}");
        }
        text.push('\n');
        text
    }

    /// Whether the run that started the check, another than this one, still runs: the check
    /// is that run's to end. One that exited unreaped (a zombie, whose serial Linux still
    /// reads) runs nothing. Its presence is read before its serial: a pid holds its process
    /// until that is reaped, so a serial that matches after a presence read not `Exited` shows
    /// the run had not exited at that read.
    fn lane_runs(&self) -> bool {
        self.lane.is_some_and(|(pid, serial)| {
            self.lane != this_lane()
                && aterm_uds::process::process_presence(pid)
                    != aterm_uds::process::ProcessPresence::Exited
                && aterm_uds::process::process_serial(pid) == Some(serial)
        })
    }
}

/// This process, by pid and serial, as a record names the lane that wrote it.
fn this_lane() -> Option<(i32, u64)> {
    let pid = i32::try_from(std::process::id()).ok()?;
    aterm_uds::process::process_serial(pid).map(|serial| (pid, serial))
}

/// `dir`'s device and inode, by which a record names the target root it was written in: a
/// copy of the root, its records with it, has another.
fn dir_id(dir: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    fs::metadata(dir).ok().map(|m| (m.dev(), m.ino()))
}

/// The pid an earlier build's record (`<pid> <ps start>`) names its root by.
fn earlier_pid(text: &str) -> Option<i32> {
    parse_uint::<i32>(text.split_whitespace().next()?).filter(|p| *p > 1)
}

/// A serial no process holds: what it names (an earlier build's root, named by its pid alone;
/// a parent whose serial was not read, as a child's [`proc_tree::Proc::tie`]) is never taken
/// for the process at that pid, which reads as another.
const NO_SERIAL: u64 = u64::MAX;

/// End the checks a killed run left, so no orphaned verifier runs on for hours, and hold each
/// one's slot until nothing of it can still be in a first exec there ([`slot_held`]). A record
/// here names a check the lane never saw end with nothing of it left.
///
/// A record from another boot goes at once, since its processes and any first exec's hold went
/// with that boot; one whose lane still runs is that run's and is left alone. Only a record
/// that names its mark, its lane and this root signals, and only its check's processes
/// ([`proc_tree::marked`]). Each reap ends them and, unless it could not tell what is left
/// ([`proc_tree::Left::Unread`]), holds the slot [`aterm_uds::deadline::FIRST_EXEC_HANG`] of
/// awake time from then. The record stays while one is still listed, the listing is not whole,
/// or one tied to the check may be its ([`proc_tree::unread`]); the reap that finds nothing
/// takes it.
///
/// Any other record signals nothing. One written under another root, which this one copies,
/// holds the slot from the first reap that found it until that stamp is spent. One that lacks
/// its mark, its lane or its root is watched ([`proc_tree::Act::Watch`]): each reap holds the
/// slot from then, and the one that finds none of what it names takes the record. No record
/// goes on a stamp that could not be written.
fn reap_stale_checks(root: &Path) {
    let boot = aterm_uds::process::boot_id();
    reap_stale_checks_with(root, boot.as_deref(), &mut |mark, slot, record, act| {
        proc_tree::end_marked(mark, None, slot, record, act)
    });
}

/// [`reap_stale_checks`] in the boot `boot` names, sweeping each check's processes with
/// `sweep` (its mark, slot and record).
fn reap_stale_checks_with(
    root: &Path,
    boot: Option<&str>,
    sweep: &mut dyn FnMut(&proc_tree::Mark, &Path, &Path, proc_tree::Act) -> proc_tree::Left,
) {
    use proc_tree::{Act, Left, Mark};
    let Ok(rd) = fs::read_dir(root) else {
        return;
    };
    let here = dir_id(root);
    for e in rd.flatten() {
        let file_name = e.file_name();
        let Some(n) = file_name
            .to_str()
            .and_then(|name| name.strip_prefix(".check."))
        else {
            continue;
        };
        let slot = root.join(format!("j{n}"));
        let text = crate::durable::read_text(&e.path()).unwrap_or_default();
        let rec = CheckRecord::parse(&text);
        let hold_spent = |slot: &Path| crate::gates::first_exec_hold(slot).is_none();
        let spent = if rec
            .boot
            .as_deref()
            .zip(boot)
            .is_some_and(|(was, now)| was != now)
        {
            true
        } else if rec.dir.is_some() && rec.dir != here {
            first_stamp(&slot) && hold_spent(&slot)
        } else if rec.lane_runs() {
            false
        } else if let (Some(nonce), Some(_), Some(_)) = (&rec.nonce, rec.lane, rec.dir) {
            let mark = Mark {
                nonce: Some(nonce.clone()),
                root: rec.root,
                kept: rec.kept,
            };
            match sweep(&mark, &slot, &e.path(), Act::End) {
                Left::Unread => {
                    first_stamp(&slot);
                    false
                }
                left => stamp_slot(&slot) && left.spent(),
            }
        } else {
            let mark = match rec.nonce {
                Some(nonce) => Some(Mark {
                    nonce: Some(nonce),
                    root: rec.root,
                    kept: rec.kept,
                }),
                None => earlier_pid(&text).map(|pid| Mark {
                    nonce: None,
                    root: Some((pid, NO_SERIAL)),
                    kept: BTreeSet::new(),
                }),
            };
            let left = mark.map_or(Left::Nothing, |mark| {
                sweep(&mark, &slot, &e.path(), Act::Watch)
            });
            stamp_slot(&slot) && left == Left::Nothing
        };
        if spent {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Stamps `slot` from now ([`aterm_uds::deadline::try_stamp_exec`]): whether a stamp holds it,
/// a slot that is gone needing none.
fn stamp_slot(slot: &Path) -> bool {
    if fs::symlink_metadata(slot).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return true;
    }
    aterm_uds::deadline::try_stamp_exec(slot)
        .inspect_err(|e| eprintln!("cannot stamp {}: {e}; its record holds it", slot.display()))
        .is_ok()
}

/// Stamps `slot` unless a stamp holds it already, so a record's hold runs from the first reap
/// that found it: whether a stamp holds it ([`stamp_slot`]).
fn first_stamp(slot: &Path) -> bool {
    aterm_uds::deadline::exec_stamp(slot).is_some() || stamp_slot(slot)
}

/// Adds `reached` to the processes `record` keeps as its check's ([`CheckRecord::kept`]),
/// written whole beside it and renamed over it, so a lane that dies mid-write leaves it as it
/// was: whether it keeps them. A record that names no mark is never rewritten.
fn keep_in_record(record: &Path, reached: &[(i32, u64)]) -> bool {
    let kept = crate::durable::read_text(record).and_then(|text| {
        let mut rec = CheckRecord::parse(&text);
        if rec.nonce.is_none() {
            return Err(std::io::Error::other("it names no mark"));
        }
        let before = rec.kept.len();
        rec.kept.extend(reached.iter().copied());
        if rec.kept.len() == before {
            return Ok(());
        }
        let mut name = std::ffi::OsString::from(".new");
        name.push(record.file_name().unwrap_or_default());
        let new = record.with_file_name(name);
        write_new_temp(&new, rec.text().as_bytes())
            .and_then(|()| fs::rename(&new, record))
            .inspect_err(|_| {
                let _ = fs::remove_file(&new);
            })
    });
    kept.inspect_err(|e| {
        eprintln!(
            "cannot keep what a sweep signals in {}: {e}",
            record.display()
        )
    })
    .is_ok()
}

/// Whether a `trustc -Z help` listing names the verifier's memory ceiling flag
/// ([`trust_facts::CEILING_FLAG`]) as one of its `-Z` options.
#[must_use]
pub fn lists_ceiling(help: &str) -> bool {
    help.lines().any(|l| {
        l.trim_start()
            .strip_prefix("-Z")
            .and_then(|rest| rest.trim_start().strip_prefix(trust_facts::CEILING_FLAG))
            .is_some_and(|after| after.is_empty() || after.starts_with(['=', ' ']))
    })
}

/// Where a clone keeps each prover build's [`lists_ceiling`] answer.
const CEILING_CACHE: &str = "trust-driver-ceiling.tsv";

const CEILING_CACHE_HEADER: &str = "# trust lane: whether each prover's trustc takes a verifier memory ceiling: <trustc> <bytes> <modified ns> <yes|no>, probed once per build (`trustc -Z help`). Local to this clone, never committed.";

/// Whether `driver` takes a verifier memory ceiling ([`Bench::takes_ceiling`]), probed once per
/// prover build: the answer is kept in `state`'s [`CEILING_CACHE`] under its trustc's path, size
/// and modification time, which a store build never changes. A probe that cannot read the
/// answer is said on `out` and taken as no ceiling: the RAM / 8 model then refuses nothing,
/// and a library is still refused for a recorded peak whose claim is not below what can ever
/// be available.
fn driver_ceiling(
    bench: &mut dyn Bench,
    driver: &Driver,
    state: Option<&Path>,
    out: &mut dyn Write,
) -> bool {
    let trustc = driver.bin.join("trustc");
    let key = fs::metadata(&trustc).ok().and_then(|md| {
        let ns = md
            .modified()
            .ok()?
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        Some(format!("{}\t{}\t{ns}\t", trustc.display(), md.len()))
    });
    let cache = state.map(|s| s.join(CEILING_CACHE));
    let text = cache
        .as_deref()
        .and_then(|c| crate::durable::read_text(c).ok())
        .unwrap_or_default();
    let kept = key.as_deref().and_then(|k| {
        text.lines()
            .find_map(|l| l.strip_prefix(k))
            .and_then(|word| match word {
                "yes" => Some(true),
                "no" => Some(false),
                _ => None,
            })
    });
    if let Some(takes) = kept {
        return takes;
    }
    let takes = match bench.takes_ceiling(driver) {
        Ok(takes) => takes,
        Err(why) => {
            let _ = writeln!(
                out,
                "trust: {why}; this prover is taken to have no verifier memory ceiling"
            );
            return false;
        }
    };
    if let (Some(key), Some(cache)) = (key, cache) {
        let mine = format!("{}\t", trustc.display());
        let mut rows: Vec<&str> = text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with(&mine))
            .collect();
        let row = format!("{key}{}", if takes { "yes" } else { "no" });
        rows.push(&row);
        let body = format!("{CEILING_CACHE_HEADER}\n{}\n", rows.join("\n"));
        let tmp = cache.with_extension(format!("tsv.{}", std::process::id()));
        let written = cache
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| write_new_temp(&tmp, body.as_bytes()))
            .and_then(|()| fs::rename(&tmp, &cache));
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
    takes
}

fn write_new_temp(tmp: &Path, bytes: &[u8]) -> std::io::Result<()> {
    crate::durable::write_fresh(tmp, bytes)
}

/// Returns this host's RAM in bytes and whether it was read from Linux `MemTotal`.
fn host_memory() -> (u64, bool) {
    if let Some(b) = crate::artifact::host_memory_bytes() {
        return (b, false);
    }
    let kib = crate::durable::read_regular_text(Path::new("/proc/meminfo"), 64 * 1024)
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("MemTotal:"))
                .and_then(|v| parse_uint::<u64>(v.split_whitespace().next()?))
        });
    kib.map_or((0, false), |k| (k * 1024, true))
}

fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    aterm_types::rfc3339::format_rfc3339(secs)
}

/// A one-minute load as a plan writes it: to a hundredth, `unread` when none was read.
fn load_word(load: Option<f64>) -> String {
    load.map_or_else(|| "unread".into(), |l| format!("{l:.2}"))
}

fn push_kv(plan: &mut String, k: &str, v: &str) {
    plan.push_str(k);
    plan.push(' ');
    plan.push_str(&one_line(v));
    plan.push('\n');
}

/// Default checkout-local target directory name (`target-trust.noindex`).
pub(crate) const OWN_ROOT: &str = "target-trust.noindex";

/// Legacy checkout-local target directory name (`target-trust`) before `.noindex`.
const LEGACY_ROOT: &str = "target-trust";

/// Fallback target-root subdirectory name under the user's cache directory.
const CACHE_DIR: &str = "aterm-trust-lane";

/// macOS fallback target-root path under `$HOME` with `.noindex` Spotlight exclusion.
const MAC_CACHE_DIR: &str = "Library/Caches/aterm-trust-lane.noindex";

/// The file in a fallback root that names its checkout ([`prune_orphans`]).
const CHECKOUT_RECORD: &str = ".checkout";

/// Path component refused by `targo trust` along with its reason and optional `chmod` fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refused {
    pub dir: PathBuf,
    pub why: String,
    /// `chmod` mode (`go-w` or `a-st`) that clears the refusal when owned by the current user.
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

    /// Formats the remediation advice for this refusal.
    pub(crate) fn fix(&self, elsewhere: &str) -> String {
        self.chmod_command()
            .map_or_else(|| elsewhere.to_string(), |c| format!("run: {c}"))
    }
}

/// The fix for a target root a component the caller cannot `chmod` keeps out.
const ELSEWHERE_ROOT: &str = "give --target-root a directory under one you own";

/// Resolved target root for a run, optional fallback cache directory, and fallback note.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Root {
    path: PathBuf,
    mine: Option<PathBuf>,
    note: Option<String>,
}

/// Returns a note when a default run finds a leftover legacy `<repo>/target-trust` directory.
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

/// Returns a warning on macOS when an explicit `--target-root` has no `*.noindex` component.
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

/// Resolves this run's target root from `--target-root` or [`choose_root`].
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

/// Returns the base cache directory for fallback target roots.
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

/// Computes the fallback root directory name for `repo` from its basename and SHA-256 prefix.
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

/// Selects `<repo>/target-trust.noindex` when admitted by `targo trust`, or `<cache>/<key>`.
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
fn owned_cache_dir(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let me = proc_tree::euid();
    let Ok(md) = fs::symlink_metadata(path) else {
        return false;
    };
    if !md.is_dir() || md.uid() != me || md.mode() & 0o022 != 0 {
        return false;
    }
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

/// Admits an existing fallback lock without following links, or creates a new owner-only lock.
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
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(".check."))
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
                    .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
                    .open(path)?,
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Replaces `root`'s checkout marker atomically via an exclusive temporary file.
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
    let scratch = aterm_tempfile::Builder::new()
        .prefix(".checkout-")
        .suffix(".noindex")
        .tempdir_in(root)?;
    let at = scratch.path().join("record");
    let mut file = crate::durable::open_create_new(&at)?;
    if file
        .metadata()
        .map_or(true, |m| m.permissions().mode() & 0o7777 != 0o644)
    {
        file.set_permissions(fs::Permissions::from_mode(0o644))?;
    }
    file.write_all(&checkout_record(repo))?;
    drop(file);
    fs::rename(at, marker)
}

/// Returns the recorded checkout path for `dir` after verifying ownership of its authority files.
fn cleanup_checkout(dir: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    if !owned_cache_dir(dir.parent()?) || !owned_cache_dir(dir) {
        return None;
    }
    owned_cache_file(&dir.join(CHECKOUT_RECORD))?;
    owned_cache_file(&dir.join(".run.lock"))?;
    for entry in fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(".check."))
        {
            owned_cache_file(&entry.path())?;
        }
    }
    let bytes = crate::durable::read_regular_bytes(&dir.join(CHECKOUT_RECORD), 64 * 1024).ok()?;
    let path = bytes.strip_suffix(b"\n")?;
    let checkout = PathBuf::from(std::ffi::OsStr::from_bytes(path));
    (checkout.is_absolute()
        && dir.file_name() == Some(std::ffi::OsStr::new(&checkout_key(&checkout))))
    .then_some(checkout)
}

/// Retire an admitted fallback root under its own run lock (taken as `me`, never waited for:
/// a held one is in use), once its stale checks are reaped,
/// unless a slot in it is still held ([`held_slots`]): a later run retires it then. Returns
/// whether it is gone.
fn retire(dir: &Path, me: &named_lock::Holder) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Some(checkout) = cleanup_checkout(dir) else {
        return false;
    };
    let lock_path = dir.join(".run.lock");
    let Ok(lock) = RunLock::take(&lock_path, me, "a run holds it") else {
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
    if !held_slots(dir).is_empty() {
        return false;
    }
    let gone = fs::remove_dir_all(dir).is_ok();
    drop(lock);
    gone
}

/// Retires admitted cache subdirectories under `cache` whose recorded checkouts no longer exist.
fn prune_orphans(cache: &Path, own: &Path, me: &named_lock::Holder) -> Vec<(PathBuf, PathBuf)> {
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
            && retire(&dir, me)
        {
            gone.push((dir, checkout));
        }
    }
    gone
}

/// Checks whether any path component of `root` would be refused by `targo trust`.
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

/// Returns the first component in `path`'s physical ancestor chain refused by `targo trust`.
fn physical_chain_refusal(path: &Path, me: u32) -> Option<Refused> {
    let phys = fs::canonicalize(path).ok()?;
    phys.ancestors().find_map(|dir| {
        let md = fs::metadata(dir).ok()?;
        refused_object(dir, &md, me, true)
    })
}

/// Checks [`component_refusal`] on the filesystem object at `path`.
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

/// Creates `dir` and missing parents with owner-only write permissions (`0o755`).
pub(crate) fn owner_dirs(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir)
}

/// Returns why `targo trust` refuses a directory with `(uid, mode)` for caller `me`.
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

/// Checks whether `targo trust` would refuse any binary or runtime library dir under `bin`.
pub(crate) fn toolchain_refusal(bin: &Path) -> Option<Refused> {
    let me = proc_tree::euid();
    for tool in ["targo", "trustc", "trustd"] {
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
                    let md = e.metadata().ok()?;
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

/// Checks whether `path` has setuid, setgid, or sticky mode bits refused on a launcher.
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

/// Exclusive file lock guarding a target root across concurrent lane runs: a [`named_lock`]
/// whose file names the run that holds it.
pub(crate) struct RunLock(named_lock::NamedLock);

impl RunLock {
    /// Takes the lock at `path` as `me` without waiting, or returns an error naming the run that
    /// holds it and `busy`, what to do about that.
    pub(crate) fn take(
        path: &Path,
        me: &named_lock::Holder,
        busy: &str,
    ) -> Result<RunLock, String> {
        RunLock::taken(
            path,
            named_lock::take(path, me, named_lock::Wait::No, &mut |_, _| {}, &|| false),
            busy,
        )
    }

    /// Takes the lock at `path` as `me`. Held by another run: under [`named_lock::Wait::Block`]
    /// the wait lasts while that run does, its holder named on `out` as it starts and once a
    /// minute, and ends at once on an interrupt; under [`named_lock::Wait::No`] (`--no-wait`)
    /// the error names the holder at once.
    pub(crate) fn wait(
        path: &Path,
        me: &named_lock::Holder,
        wait: named_lock::Wait,
        out: &mut dyn Write,
    ) -> Result<RunLock, String> {
        let taken = named_lock::take(
            path,
            me,
            wait,
            &mut |holder, waited| {
                let _ = if waited < named_lock::SAY_EVERY {
                    writeln!(
                        out,
                        "trust: waiting for {}, which holds {}",
                        holder_name(holder),
                        path.display()
                    )
                } else {
                    writeln!(
                        out,
                        "trust: still waiting for {} ({} min)",
                        holder_name(holder),
                        waited.as_secs() / 60
                    )
                };
            },
            &|| crate::interrupt::caught().is_some(),
        );
        RunLock::taken(
            path,
            taken,
            "give this one its own --target-root, or leave out --no-wait to wait for it",
        )
    }

    fn taken(
        path: &Path,
        taken: Result<named_lock::NamedLock, named_lock::Refused>,
        busy: &str,
    ) -> Result<RunLock, String> {
        match taken {
            Ok(held) => Ok(RunLock(held)),
            Err(named_lock::Refused::Held(holder)) => Err(format!(
                "{} holds {}: {busy}",
                holder_name(holder.as_ref()),
                path.display()
            )),
            Err(named_lock::Refused::Stopped { holder, signal }) => Err(format!(
                "interrupted{} while waiting for {}, which holds {}",
                signal.map_or_else(String::new, |sig| format!(
                    " ({})",
                    aterm_toolchain::interrupt::name(sig)
                )),
                holder_name(holder.as_ref()),
                path.display()
            )),
            Err(named_lock::Refused::Io(e)) => Err(format!("cannot lock {}: {e}", path.display())),
        }
    }
}

/// Whom a held run lock names: its record's run while that pid lives, its clock this Mac's.
fn holder_name(holder: Option<&named_lock::Holder>) -> String {
    named_lock::name(
        holder,
        |since| {
            i64::try_from(since)
                .ok()
                .and_then(aterm_uds::zone::local_offset_at)
                .unwrap_or(0)
        },
        aterm_uds::process::pid_alive,
    )
}

/// This run as the run lock's record names it: `ship trust run` and its scope.
fn run_verb(scope: &Scope) -> String {
    match scope {
        Scope::WholeTree => "ship trust run --whole-tree".to_string(),
        Scope::Remeasure => "ship trust run --remeasure".to_string(),
        Scope::Packages(packages) => packages
            .iter()
            .fold("ship trust run".to_string(), |verb, p| {
                format!("{verb} -p {p}")
            }),
    }
}

struct RealChecks<'a> {
    driver: &'a Driver,
    repo: &'a Path,
    root: &'a Path,
    env: Vec<(String, String)>,
    keeper: &'a trustd::Keeper,
    sock: &'a Path,
    want: &'a str,
    t0: Instant,
    /// Fork lockfiles used during the run and whether this run created them.
    locks: BTreeMap<PathBuf, bool>,
    /// Reads a spawned root's serial ([`aterm_uds::process::process_serial`]).
    serial: fn(i32) -> Option<u64>,
    /// The signal that interrupted the run, once one has ([`crate::interrupt::caught`]).
    caught: fn() -> Option<i32>,
    /// Reads the process table, for a running check's times ([`TreeTimes`]).
    sweep: fn() -> Option<Vec<aterm_sysprobe::procs::ProcessReading>>,
    /// Each check's memory claim, admitted at the launch gate.
    admission: Admission<'a>,
    /// Where the gate says what a check waits for.
    out: Box<dyn Write + 'a>,
}

impl<'a> RealChecks<'a> {
    /// The lane's backend for a run in this process, the one way a run builds it: what it reads
    /// of the process is this process's own (its roots' serials, its process table, and the
    /// signal that interrupted it, [`crate::interrupt::caught`]), and its clock starts now. A
    /// test replaces what it injects once this has built it.
    #[expect(
        clippy::too_many_arguments,
        reason = "the run's places the backend borrows, each a field it keeps"
    )]
    fn new(
        driver: &'a Driver,
        repo: &'a Path,
        root: &'a Path,
        keeper: &'a trustd::Keeper,
        sock: &'a Path,
        want: &'a str,
        admission: Admission<'a>,
        out: Box<dyn Write + 'a>,
    ) -> Self {
        Self {
            driver,
            repo,
            root,
            env: lane_env(&driver.bin, &host_var),
            keeper,
            sock,
            want,
            t0: Instant::now(),
            locks: BTreeMap::new(),
            serial: aterm_uds::process::process_serial,
            caught: crate::interrupt::caught,
            sweep: || aterm_sysprobe::procs::processes().map(|s| s.procs),
            admission,
            out,
        }
    }
}

impl Checks for RealChecks<'_> {
    fn admit(&mut self, job: &Job) -> Option<Admitted> {
        let now = self.t0.elapsed();
        let out = &mut self.out;
        self.admission.admit(job, now, &mut |line| {
            let _ = writeln!(out, "trust: {line}");
        })
    }

    fn waiting(&self) -> Option<String> {
        self.admission.waiting()
    }

    fn given_up(&mut self) {
        self.admission.stopped((self.caught)());
    }

    fn start(&mut self, job: &mut Job) -> std::io::Result<Box<dyn Check>> {
        let target = self.root.join(format!("j{}", job.slot));
        owner_dirs(&target)?;
        // The slot is not held, so a stamp left in it is spent; the first stamp a reap of
        // this check's record finds must be that reap's own ([`reap_stale_checks`]).
        if let Err(e) = fs::remove_file(target.join(aterm_uds::deadline::EXEC_STAMP))
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(e);
        }
        job.target.clone_from(&target);
        let log = crate::durable::create_fresh(&job.log)?;
        job.lock = fork_lock(self.repo, &job.argv);
        if let Some(l) = &job.lock {
            self.locks.entry(l.path.clone()).or_insert(l.created);
        }
        let nonce = aterm_uds::rand::hex_token::<NONCE_BYTES>()?;
        let mut rec = CheckRecord {
            nonce: Some(nonce.clone()),
            boot: aterm_uds::process::boot_id(),
            lane: this_lane(),
            dir: dir_id(self.root),
            root: None,
            kept: BTreeSet::new(),
        };
        if self
            .driver
            .targo
            .as_os_str()
            .as_encoded_bytes()
            .contains(&b'=')
        {
            return Err(std::io::Error::other(format!(
                "{}: env(1), which the gate execs it through, would read the '=' as an assignment",
                self.driver.targo.display()
            )));
        }
        // The gate's env(1) execs targo only once the lane lets it go, so an exec error there
        // would read as the check's own failure.
        exec_ready(&self.driver.targo)?;
        // The root is identified, and its record names it, before it runs targo ([`GATE`]).
        let mut cmd = command("/bin/sh");
        cmd.arg("-c")
            .arg(GATE)
            .arg(&self.driver.targo)
            .args(["trust", "check"])
            .args(&job.argv)
            .arg("--allow-l0-gaps");
        if let Some(r) = &job.report {
            cmd.arg("--report-dir").arg(r);
        }
        cmd.env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .env("CARGO_TARGET_DIR", &target)
            .env(CHECK_VAR, &nonce)
            // Its check's memory claim, held by this process, covers the builds it starts, and
            // no other check's claim does.
            .env(
                aterm_capacity::COVER_ENV,
                job.cover.as_deref().unwrap_or(aterm_capacity::NO_COVER),
            )
            .current_dir(self.repo)
            .stdin(Stdio::piped())
            .stdout(log.try_clone()?)
            .stderr(log);
        proc_tree::new_session(&mut cmd);
        // The record holds the slot before the root can run, and never replaces another
        // run's: a slot whose record is left is held ([`slot_held`]).
        let record = self.root.join(format!(".check.{}", job.slot));
        let written = crate::durable::open_create_new(&record)?.write_all(rec.text().as_bytes());
        let mut child = match written.and_then(|()| cmd.spawn()) {
            Ok(child) => child,
            Err(e) => {
                let _ = fs::remove_file(&record);
                return Err(e);
            }
        };
        let pid = child.id();
        rec.root = i32::try_from(pid)
            .ok()
            .and_then(|p| (self.serial)(p).map(|s| (p, s)));
        let mark = proc_tree::Mark {
            nonce: Some(nonce),
            root: rec.root,
            kept: BTreeSet::new(),
        };
        let ready = if rec.root.is_none() {
            Err(std::io::Error::other(format!(
                "its root, pid {pid}, could not be identified, so it was ended"
            )))
        } else {
            crate::durable::write_fresh(&record, rec.text()).and_then(|()| open(&mut child))
        };
        if let Err(e) = ready {
            let _ = child.kill();
            // Swept before the wait reaps the root, so its session is proven the check's.
            let left = proc_tree::end_marked(
                &mark,
                i32::try_from(pid).ok(),
                &target,
                &record,
                proc_tree::Act::End,
            );
            let _ = child.wait();
            let held = left == proc_tree::Left::Nothing || stamp_slot(&target);
            if left.spent() && held {
                let _ = fs::remove_file(&record);
            }
            return Err(e);
        }
        Ok(Box::new(RealCheck {
            child,
            record,
            target,
            mark,
            ended: None,
            times: TreeTimes::default(),
            read_at: None,
            sweep: self.sweep,
        }))
    }

    fn trustd_held(&self) -> bool {
        self.keeper.held()
    }

    fn trustd_lost(&self) -> u32 {
        self.keeper.lost()
    }

    fn probe_trustd(&mut self) -> String {
        trustd::probe(self.sock, self.want, Duration::from_secs(10)).line()
    }

    fn interrupted(&self) -> bool {
        (self.caught)().is_some()
    }

    fn now(&self) -> Duration {
        self.t0.elapsed()
    }

    fn pause(&mut self) {
        std::thread::sleep(POLL);
    }

    fn held(&self, slot: usize) -> bool {
        slot_held(self.root, slot)
    }

    fn load(&self) -> Option<f64> {
        aterm_sysprobe::load_average().map(|l| l[0])
    }
}

/// What a check's root runs first, `/bin/sh -c GATE <targo> <args>…`, the lane holding its
/// stdin's only writer: it waits for a line, which [`RealChecks::start`] writes once the
/// record names the root, then execs targo through env(1), which drops what the shell adds to
/// the environment (`PWD`, bash's `SHLVL`). A lane that dies first closes the pipe and the
/// gate exits without exec'ing, so nothing runs that no record names.
const GATE: &str = "read -r go && exec /usr/bin/env -u PWD -u SHLVL \"$0\" \"$@\" </dev/null";

/// Whether `path` is a file this process may exec, as execve(2) judges it but for a file some
/// process holds open for writing (Linux's `ETXTBSY`): the error naming why not.
fn exec_ready(path: &Path) -> std::io::Result<()> {
    let named =
        |e: std::io::Error| std::io::Error::new(e.kind(), format!("{}: {e}", path.display()));
    if !fs::metadata(path).map_err(named)?.is_file() {
        return Err(named(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "not a file",
        )));
    }
    proc_tree::executable(path).map_err(named)
}

/// Lets a root [`RealChecks::start`] spawned through its [`GATE`] run targo.
fn open(child: &mut aterm_uds::spawnfd::Child) -> std::io::Result<()> {
    child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("the gate's pipe is gone"))?
        .write_all(b"\n")
}

struct RealCheck {
    child: aterm_uds::spawnfd::Child,
    record: PathBuf,
    /// The slot's `CARGO_TARGET_DIR`, stamped when what ended there never showed it ran.
    target: PathBuf,
    /// What its processes carry.
    mark: proc_tree::Mark,
    ended: Option<Ended>,
    /// Its processes' times, from reads of the process table ([`RealChecks::sweep`]).
    times: TreeTimes,
    /// When it last read them.
    read_at: Option<Instant>,
    sweep: fn() -> Option<Vec<aterm_sysprobe::procs::ProcessReading>>,
}

impl RealCheck {
    /// Reads its processes' times: when `now`, or once [`THREADS_EVERY`] has passed since the
    /// last read.
    fn read_times(&mut self, now: bool) {
        if !now && self.read_at.is_some_and(|t| t.elapsed() < THREADS_EVERY) {
            return;
        }
        self.read_at = Some(Instant::now());
        if let (Some(root), Some(procs)) = (self.held(), (self.sweep)()) {
            self.times.take(root.unsigned_abs(), &procs);
        }
    }

    /// The root's pid, which this process holds until it reaps the root.
    fn held(&self) -> Option<i32> {
        i32::try_from(self.child.id()).ok()
    }

    /// The end `usage` says. The record goes only when `left` found nothing of the check
    /// ([`proc_tree::Left::spent`]) and the slot is `held` as that needs; otherwise it stays,
    /// holding the slot for a later reap.
    fn reaped(
        &mut self,
        usage: aterm_uds::spawnfd::Rusage,
        left: proc_tree::Left,
        held: bool,
    ) -> Ended {
        use std::os::unix::process::ExitStatusExt as _;
        let e = Ended {
            code: usage
                .status
                .code()
                .unwrap_or_else(|| 128 + usage.status.signal().unwrap_or(0)),
            cpu: usage.cpu,
            maxrss: usage.maxrss.max(self.times.peak()),
            threads: self.times.threads(),
        };
        if left.spent() && held {
            let _ = fs::remove_file(&self.record);
        }
        self.ended = Some(e);
        e
    }
}

impl Check for RealCheck {
    fn poll(&mut self) -> std::io::Result<Option<Ended>> {
        if let Some(e) = self.ended {
            return Ok(Some(e));
        }
        if !self.child.exited()? {
            self.read_times(false);
            return Ok(None);
        }
        self.read_times(true);
        // What the root started may run on without it, in its session or adopted by launchd;
        // it is ended while the root is still unreaped, so its session is proven the check's.
        // Only a root that exited 0 with nothing left showed that what ran in the slot ran: a
        // failed build may be one whose build script was killed at its first exec, which macOS
        // goes on assessing. Otherwise the slot is held from now ([`clear_slots`]), and the
        // record stays unless that stamp was written.
        let left = proc_tree::end_marked(
            &self.mark,
            self.held(),
            &self.target,
            &self.record,
            proc_tree::Act::End,
        );
        let usage = self.child.wait_with_rusage()?;
        let held = (usage.status.success() && left == proc_tree::Left::Nothing)
            || stamp_slot(&self.target);
        Ok(Some(self.reaped(usage, left, held)))
    }

    fn end(&mut self) -> Ended {
        if let Some(e) = self.ended {
            return e;
        }
        self.read_times(true);
        let left = proc_tree::end_marked(
            &self.mark,
            self.held(),
            &self.target,
            &self.record,
            proc_tree::Act::End,
        );
        // The root is this process's own child, not yet reaped, so it is killed whatever the
        // sweep could read.
        let _ = self.child.kill();
        // What the kill ended never showed it ran: the slot stays ([`clear_slots`]), by its
        // stamp, or by its record when the stamp could not be written.
        let held = stamp_slot(&self.target);
        if let Ok(usage) = self.child.wait_with_rusage() {
            self.reaped(usage, left, held)
        } else {
            let e = Ended {
                code: 128 + proc_tree::SIGKILL,
                cpu: Duration::ZERO,
                maxrss: self.times.peak(),
                threads: self.times.threads(),
            };
            self.ended = Some(e);
            if left.spent() && held {
                let _ = fs::remove_file(&self.record);
            }
            e
        }
    }
}

pub mod proc_tree {

    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    pub const SIGKILL: i32 = 9;
    pub const SIGTERM: i32 = 15;

    unsafe extern "C" {
        fn access(path: *const std::ffi::c_char, mode: i32) -> i32;
        fn kill(pid: i32, sig: i32) -> i32;
        fn geteuid() -> u32;
        #[cfg(target_vendor = "apple")]
        fn getsid(pid: i32) -> i32;
    }

    /// Effective user ID of the current process.
    #[must_use]
    pub fn euid() -> u32 {
        // SAFETY: `geteuid` takes no arguments and cannot fail.
        unsafe { geteuid() }
    }

    /// Whether the current process has write permission on `dir` (`access(2)`).
    #[must_use]
    pub fn writable(dir: &std::path::Path) -> bool {
        const W_OK: i32 = 2;
        may(dir, W_OK).is_ok()
    }

    /// Whether the current process may execute `path` (`access(2)`): the error when not.
    pub fn executable(path: &std::path::Path) -> std::io::Result<()> {
        const X_OK: i32 = 1;
        may(path, X_OK)
    }

    /// `access(path, mode)`.
    fn may(path: &std::path::Path, mode: i32) -> std::io::Result<()> {
        use std::os::unix::ffi::OsStrExt as _;
        let c =
            std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
        // SAFETY: `c` is a live NUL-terminated string that `access` only reads.
        if unsafe { access(c.as_ptr(), mode) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Configures `cmd` to start in a new session (`setsid`).
    pub const fn new_session(cmd: &mut aterm_uds::spawnfd::Command) {
        cmd.setsid(true);
    }

    /// What every process of one check carries: [`super::CHECK_VAR`]`=<nonce>` in the
    /// environment it was exec'd with, and the root the lane spawned, by pid and serial
    /// ([`aterm_uds::process::process_serial`]), once the record names it. The root leads a
    /// session of its own, which its pid names ([`new_session`]).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Mark {
        /// The random nonce the lane gave the check; `None` for an earlier build's check,
        /// whose processes carry none.
        pub nonce: Option<String>,
        /// The root's pid and serial.
        pub root: Option<(i32, u64)>,
        /// The processes an earlier sweep kept as the check's ([`Procs::keep`]), by pid and
        /// serial, each the check's while its pid holds that serial.
        pub kept: BTreeSet<(i32, u64)>,
    }

    /// One of this user's processes as a sweep reads it ([`table`]).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Proc {
        pub pid: i32,
        /// Its parent; 0 when unread.
        pub ppid: i32,
        /// Its serial, which each signal re-reads first ([`may_signal`]); `None` when unread,
        /// and then it is never signalled.
        pub serial: Option<u64>,
        /// Its session; `None` when unread.
        pub sid: Option<i32>,
        /// A `trustd`: the lane never ends one, nor what it runs.
        pub trustd: bool,
        /// Whether it carries the mark; `None` when that could not be read.
        pub marked: Option<bool>,
        /// Its executable, read only when `marked` could not be.
        pub exe: Option<PathBuf>,
        /// In the session the root's pid names, and working in, running from or holding a file
        /// in the slot, or unreadable as to that: read only while the lane does not hold the
        /// root ([`table`]).
        pub at_slot: bool,
        /// In the session the root's pid names while no other process holds that pid, and
        /// nothing shows it is a stranger's: its environment, when read, carries the mark's
        /// nonce (or the mark names none), and it never had a controlling terminal, which the
        /// root's session, made by `setsid` with stdin null, never gains. Once that session
        /// empties, the pid may come to lead a stranger's, which its leader can leave behind.
        /// Read only while the lane does not hold the root ([`table`]).
        pub in_session: bool,
        /// Caught mid-spawn (macOS): a `posix_spawn` child whose new image is not in yet reads
        /// as its spawner (name, path, session), and a signal sent to it lands on that image,
        /// maybe a `trustd`. It has neither run a thread nor exec'd, where a fork runs its
        /// parent's thread and a program held at its first exec has exec'd. It is never
        /// signalled; the next listing reads it as what it became.
        pub spawning: bool,
        /// The parents through which alone it is the check's ([`marked`]), each by pid and
        /// serial, from its own up to the first that is the check's by itself: it is signalled
        /// only while each still holds that serial, each under the next ([`may_signal`]). Empty
        /// in a listing's own rows.
        pub tie: Vec<(i32, u64)>,
    }

    /// One listing of this user's processes ([`table`]).
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Listing {
        pub procs: Vec<Proc>,
        /// Whether a list taken after its reads named no process identity it had not read,
        /// so none of the check's processes escaped it. A reused pid is a new identity.
        pub whole: bool,
    }

    /// What a sweep does with the check's processes it finds ([`end_marked`]).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Act {
        /// Sends them `SIGTERM`, then `SIGKILL`.
        End,
        /// Signals nothing: while any is listed, the check is left.
        Watch,
    }

    /// What a sweep found of a check ([`end_marked`]).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Left {
        /// None of its processes runs, and none tied to it may be ([`unread`]), by a whole
        /// listing ([`Listing::whole`]).
        Nothing,
        /// Its processes were signalled, and a whole listing found none left.
        Ended,
        /// Its processes were signalled, or only watched ([`Act::Watch`]), and one is still
        /// listed, or one tied to it may be, or no whole listing followed.
        Lives,
        /// No whole listing was read, or one tied to the check may be its: nothing was
        /// signalled.
        Unread,
    }

    impl Left {
        /// Whether nothing of the check is left, so its record may go.
        #[must_use]
        pub const fn spent(self) -> bool {
            matches!(self, Left::Nothing | Left::Ended)
        }
    }

    /// The reads and signals a sweep takes ([`Live`]).
    pub trait Procs {
        /// This user's processes, each judged against the mark; `None` when they could not
        /// be listed.
        fn table(&mut self) -> Option<Listing>;
        /// The check's session in `listed`, when it is proven ([`proven_session`]).
        fn session(&mut self, listed: &[Proc]) -> Option<i32>;
        /// Whether `p` still runs: its pid holds the process listed, and not as a zombie.
        fn alive(&mut self, p: &Proc) -> bool;
        /// Reads each of `procs` again and keeps in the check's record each that read shows
        /// may be signalled ([`may_signal`]), by pid and serial, so a later sweep finds it as
        /// the check's ([`Mark::kept`]) though this one dies after a signal that ended what
        /// tied it to the check. Returns whether the record keeps them.
        fn keep(&mut self, procs: &[Proc]) -> bool;
        /// Reads each of `procs` again, in order, and sends `sig` to each that read shows may be
        /// signalled ([`may_signal`]), but one caught mid-spawn ([`Proc::spawning`]). Returns
        /// each such read showed, signalled or mid-spawn, by pid and serial: the check's for as
        /// long as that serial runs.
        fn send(&mut self, procs: &[Proc], sig: i32) -> Vec<(i32, u64)>;
        /// How long a sweep waits for what it signalled to end before it lists again
        /// ([`settle`]): a second for real processes.
        fn settle_within(&self) -> Duration {
            Duration::from_secs(1)
        }
    }

    /// The session `root`, the root's pid, names, when every process in it is the check's: a
    /// session holds only what its leader, the root, started, and no process or session is
    /// given its number while the root's pid is held or a member is left. So it is proven while
    /// this process holds the root as its unreaped child (`held`), or while one of the check's
    /// own processes listed in it (the root, or one that carries the mark) `still` runs in it
    /// after the listing, which covers every read the listing made.
    pub fn proven_session(
        listed: &[Proc],
        root: Option<i32>,
        held: bool,
        still: &mut dyn FnMut(&Proc) -> bool,
    ) -> Option<i32> {
        let sid = root?;
        (held
            || listed
                .iter()
                .any(|p| p.marked == Some(true) && p.sid == Some(sid) && still(p)))
        .then_some(sid)
    }

    /// The kernel's [`Procs`] for one check's mark.
    pub struct Live<'a> {
        pub mark: &'a Mark,
        /// The root's pid while this process holds it as its own unreaped child.
        pub held: Option<i32>,
        /// The check's slot, which a member of its session may be using ([`Proc::at_slot`]).
        pub slot: &'a Path,
        /// The check's record ([`super::keep_in_record`]).
        pub record: &'a Path,
    }

    impl Procs for Live<'_> {
        fn table(&mut self) -> Option<Listing> {
            table(self.mark, self.held, self.slot)
        }

        fn session(&mut self, listed: &[Proc]) -> Option<i32> {
            let root = self.held.or(self.mark.root.map(|(pid, _)| pid));
            proven_session(listed, root, self.held.is_some(), &mut |p| {
                kin(p.pid).is_some_and(|k| {
                    !k.zombie && Some(k.serial) == p.serial && k.sid.is_some() && k.sid == p.sid
                })
            })
        }

        fn alive(&mut self, p: &Proc) -> bool {
            p.serial.is_some()
                && kin(p.pid).is_some_and(|k| !k.zombie && Some(k.serial) == p.serial)
        }

        fn keep(&mut self, procs: &[Proc]) -> bool {
            // A send's own reads, signalling nothing.
            super::keep_in_record(self.record, &send_with(procs, 0, &mut kin, &mut |_, _| {}))
        }

        // The pid can still pass to another process between the read and `kill`, kill(2)'s own
        // race; no sweep stops anything, so that leaves nothing stuck.
        fn send(&mut self, procs: &[Proc], sig: i32) -> Vec<(i32, u64)> {
            send_with(procs, sig, &mut kin, &mut |pid, sig| {
                // SAFETY: `kill` takes two integers and touches no memory of ours.
                unsafe {
                    kill(pid, sig);
                }
            })
        }
    }

    /// [`Procs::send`] over the reads `kin` takes and the signals `signal` sends.
    pub(super) fn send_with(
        procs: &[Proc],
        sig: i32,
        kin: &mut dyn FnMut(i32) -> Option<Kin>,
        signal: &mut dyn FnMut(i32, i32),
    ) -> Vec<(i32, u64)> {
        let mut reached = Vec::new();
        for p in procs {
            let Some(now) = kin(p.pid) else {
                continue;
            };
            if !may_signal(p, &now, kin) {
                continue;
            }
            reached.push((p.pid, now.serial));
            if !p.spawning {
                signal(p.pid, sig);
            }
        }
        reached
    }

    /// Whether `listed` may be signalled, its pid read again right before the signal as `now`,
    /// and its parents through `kin`: it is still the process listed (no zombie, the listed
    /// serial) under the parent listed, each parent that tied it ([`Proc::tie`]) still holds
    /// its serial under the next, and it is neither a `trustd` nor run by one. So a child whose
    /// parent died since (it reads its reaper as its parent), one tied through a pid that passed
    /// to another process, at any depth, and one that exec'd a `trustd` since it was listed are
    /// never signalled.
    pub(super) fn may_signal(
        listed: &Proc,
        now: &Kin,
        kin: &mut dyn FnMut(i32) -> Option<Kin>,
    ) -> bool {
        !now.zombie
            && Some(now.serial) == listed.serial
            && now.ppid == listed.ppid
            && !now.trustd
            && !kin(now.ppid).is_some_and(|k| k.trustd)
            && listed.tie.iter().enumerate().all(|(i, &(pid, serial))| {
                kin(pid).is_some_and(|k| {
                    k.serial == serial && listed.tie.get(i + 1).is_none_or(|&(up, _)| k.ppid == up)
                })
            })
    }

    /// What the kernel holds of one of this user's processes.
    #[derive(Clone, Copy)]
    pub(super) struct Kin {
        pub(super) ppid: i32,
        pub(super) serial: u64,
        /// Its session; `None` when unread.
        pub(super) sid: Option<i32>,
        pub(super) zombie: bool,
        /// It has, or had, a controlling terminal: macOS keeps the mark on a process once its
        /// session leader is gone; Linux clears it then.
        pub(super) ctty: bool,
        pub(super) trustd: bool,
        /// macOS: it runs the image it was forked with (no `PROC_FLAG_EXEC`), as a
        /// `posix_spawn` child does until its new image is in ([`Proc::spawning`]). Never set
        /// elsewhere.
        pub(super) forked: bool,
        /// Linux: a zombie thread-group leader whose other threads still run, which reads as
        /// a zombie (no image, no environment) while the process runs. Never set elsewhere.
        pub(super) leader_only: bool,
    }

    /// `pid`'s [`Kin`], or `None` when unread: gone, another user's, or no reader here.
    #[cfg(target_vendor = "apple")]
    fn kin(pid: i32) -> Option<Kin> {
        const SZOMB: u32 = 5;
        const PROC_FLAG_CONTROLT: u32 = 0x80;
        const PROC_FLAG_EXEC: u32 = 0x4000;
        const NODEV: u32 = u32::MAX;
        let info = aterm_uds::process::bsdinfo_uniq(pid)?;
        if info.pbsd.pbi_pid != pid.unsigned_abs() || info.pbsd.pbi_uid != euid() {
            return None;
        }
        let trustd = info
            .pbsd
            .pbi_comm
            .iter()
            .map(|c| c.to_ne_bytes()[0])
            .take_while(|&b| b != 0)
            .eq(b"trustd".iter().copied());
        // SAFETY: `getsid` takes an integer and touches no memory of ours.
        let sid = unsafe { getsid(pid) };
        // The session is the process read above only while its pid still holds it.
        let sid = (sid > 0 && aterm_uds::process::process_serial(pid) == Some(info.p_uniqueid))
            .then_some(sid);
        Some(Kin {
            ppid: i32::try_from(info.pbsd.pbi_ppid).unwrap_or(0),
            serial: info.p_uniqueid,
            sid,
            zombie: info.pbsd.pbi_status == SZOMB,
            ctty: info.pbsd.pbi_flags & PROC_FLAG_CONTROLT != 0 || info.pbsd.e_tdev != NODEV,
            trustd,
            forked: info.pbsd.pbi_flags & PROC_FLAG_EXEC == 0,
            leader_only: false,
        })
    }

    /// See the Apple twin: `/proc/<pid>/stat` ([`stat_kin`]).
    #[cfg(target_os = "linux")]
    fn kin(pid: i32) -> Option<Kin> {
        stat_kin(
            &aterm_uds::process::read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat")))
                .ok()?,
            pid,
        )
    }

    /// See the Apple twin: nothing is read here.
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    fn kin(_pid: i32) -> Option<Kin> {
        None
    }

    /// [`Kin`] from `pid`'s `/proc/<pid>/stat` line, `pid (comm) state ppid pgrp session
    /// tty_nr …`, its serial the start in clock ticks ([`aterm_uds::process::process_serial`]).
    /// A zombie counts itself among its threads until it is reaped, so one that counts more
    /// is a leader whose other threads run (as `aterm_uds`' procfs reader tells them apart).
    #[cfg(any(target_os = "linux", test))]
    fn stat_kin(stat: &str, pid: i32) -> Option<Kin> {
        let open = stat.find(" (")?;
        if super::parse_uint::<i32>(stat[..open].trim())? != pid {
            return None;
        }
        let close = stat.rfind(')')?;
        let comm = stat.get(open + 2..close)?;
        let mut fields = stat[close + 1..].split_ascii_whitespace();
        let state = fields.next()?;
        let ppid = super::parse_uint(fields.next()?)?;
        let sid = super::parse_uint(fields.nth(1)?).filter(|s| *s > 0);
        let tty: i64 = fields
            .next()
            .filter(|s| !s.starts_with('+'))?
            .parse()
            .ok()?;
        let threads: u64 = super::parse_uint(fields.nth(12)?)?;
        let serial = super::parse_uint(fields.nth(1)?)?;
        let exited = matches!(state, "Z" | "X" | "x");
        Some(Kin {
            ppid,
            serial,
            sid,
            zombie: exited && threads <= 1,
            ctty: tty != 0,
            trustd: comm == "trustd",
            forked: false,
            leader_only: exited && threads > 1,
        })
    }

    /// How long a listing lists before it is not whole ([`table`]). Each list is cheap (a
    /// serial per pid, a row only for a new one), but on a busy machine every list may name a
    /// process not yet read for seconds on end, where an idle machine's listing is whole within
    /// milliseconds; so a listing gives up after a time, not a count of lists, and too short a
    /// time holds slots whose checks left nothing.
    pub(super) const LISTING: Duration = Duration::from_secs(10);

    /// Rounds of `SIGKILL` a sweep sends, each settling up to a second, before it says one of
    /// the check's processes lives ([`end_marked_with`]).
    const KILLS: usize = 16;

    /// This user's processes but this one and its zombies, each judged against `mark`: the
    /// root and those it keeps by their pid and serial, any other by the environment it was
    /// exec'd with ([`aterm_uds::process::read_process_markers`]), read only once its thread
    /// ran (a read of a program held mid-start can hang). While this process does not hold
    /// the root (`held`), a member of the session the root's pid names is read for its use of
    /// `slot` ([`Proc::at_slot`]) and for whether the session is the root's
    /// ([`Proc::in_session`]). A process gone mid-read is left out. `None` when they could not
    /// be listed.
    ///
    /// No list is atomic: a process that exits before its row is read may have forked one
    /// after the list was taken. So the listing then reads each process a new list names that
    /// it did not read yet, by pid and serial, and is whole once a list names none: whatever
    /// runs then was read while it ran, or was started by what was. A serial unread or changed
    /// across a row, or a process caught mid-spawn, is not cached; an exited, unreaped process
    /// is read as [`EXITED`]. Once it has listed for [`LISTING`], a list after the first that
    /// still names one ends it, not whole.
    #[must_use]
    pub fn table(mark: &Mark, held: Option<i32>, slot: &Path) -> Option<Listing> {
        #[cfg(test)]
        if let Some(pids) = LISTED.with(|l| l.borrow().clone()) {
            return table_from(mark, held, slot, LISTING, &mut || Ok(pids.clone()));
        }
        #[cfg(test)]
        let mut pids = || aterm_uds::holders::uid_pids().map(this_binarys);
        #[cfg(not(test))]
        let mut pids = aterm_uds::holders::uid_pids;
        table_from(mark, held, slot, LISTING, &mut pids)
    }

    #[cfg(test)]
    thread_local! {
        /// The pids [`table`] lists on this thread in place of all of this user's, so a test
        /// can judge a sweep by a listing whose wholeness no other process's churn decides.
        pub(super) static LISTED: std::cell::RefCell<Option<Vec<u32>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Of `pids`, those this test binary started and those its tree left to an adopter: each
    /// whose parents, read up from it while they are younger than this process, lead to this
    /// process, or to an adopter of orphans that did not start it itself. The adopters are pid 1
    /// and this process's ancestors, since a Linux subreaper (`systemd --user`) adopts only its
    /// descendants' orphans. Apple keeps a process's launcher's pid version through adoption
    /// ([`launched_by`]), so there what an adopter started itself, launchd's jobs and the
    /// installed app's new tabs, is left out; Linux keeps none, so there every process an
    /// ancestor started since this one is kept. On both, what another program daemonized since
    /// this one started is kept, with all it starts. [`table`] lists only these under test, so
    /// the rest of the machine's churn does not decide the wholeness a test's sweep needs: on a
    /// loaded machine a list after the first nearly always names a process not read yet, and a
    /// sweep then keeps the record it watched for as long as the load lasts. A listed process
    /// gone by its read is left out; one whose parent went mid-read is kept, since an adopter
    /// then holds it, and so is one whose parents' pids were reused into a loop.
    #[cfg(test)]
    fn this_binarys(pids: Vec<u32>) -> Vec<u32> {
        let me = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
        let Some(born) = aterm_uds::process::process_serial(me) else {
            return pids;
        };
        let mut adopters = BTreeSet::from([1]);
        let mut up = me;
        while let Some(parent) = aterm_uds::process::read_process_parent(up)
            .and_then(|p| i32::try_from(p).ok())
            .filter(|&p| adopters.insert(p))
        {
            up = parent;
        }
        let mut known: BTreeMap<i32, bool> = BTreeMap::new();
        pids.into_iter()
            .filter(|&pid| {
                let Ok(pid) = i32::try_from(pid) else {
                    return false;
                };
                let mut chain = Vec::new();
                let mut at = pid;
                let ours = loop {
                    if at == me || chain.contains(&at) {
                        break true;
                    }
                    if let Some(&ours) = known.get(&at) {
                        break ours;
                    }
                    let Some(k) = kin(at) else {
                        break at != pid;
                    };
                    if k.serial < born {
                        break false;
                    }
                    chain.push(at);
                    if adopters.contains(&k.ppid) {
                        break !launched_by(at, k.ppid);
                    }
                    at = k.ppid;
                };
                for p in chain {
                    known.insert(p, ours);
                }
                ours
            })
            .collect()
    }

    /// Whether `parent`, `pid`'s parent now, started it rather than adopted it: its launcher's
    /// pid version, which Apple keeps through adoption, is `parent`'s.
    #[cfg(all(test, target_vendor = "apple"))]
    fn launched_by(pid: i32, parent: i32) -> bool {
        let launcher = aterm_uds::process::bsdinfo_uniq(pid).map(|i| i.p_orig_ppidversion);
        launcher.is_some()
            && launcher == aterm_uds::process::uniq_identifier(parent).map(|i| i.p_idversion)
    }

    /// See the Apple twin: no launcher is kept here, so no process reads as its parent's own.
    #[cfg(all(test, not(target_vendor = "apple")))]
    fn launched_by(_pid: i32, _parent: i32) -> bool {
        false
    }

    /// The serial [`table_from`] reads for a process that exited and is not yet reaped, whose
    /// own serial macOS no longer reads: it runs nothing again and its pid stays its own until
    /// it is reaped, after which a process given that pid reads its real serial, never this.
    const EXITED: u64 = u64::MAX;

    /// [`table`] over the lists `pids` takes, listing for up to `within`.
    pub(super) fn table_from(
        mark: &Mark,
        held: Option<i32>,
        slot: &Path,
        within: Duration,
        pids: &mut dyn FnMut() -> std::io::Result<Vec<u32>>,
    ) -> Option<Listing> {
        let session = mark.root.map(|(pid, _)| pid).filter(|_| held.is_none());
        // Another process at the root's pid leads any session that bears it.
        let taken = session.is_some_and(|pid| {
            aterm_uds::process::process_serial(pid)
                .is_some_and(|serial| mark.root.map(|(_, root)| root) != Some(serial))
        });
        let dirs: Vec<PathBuf> = std::iter::once(slot.to_path_buf())
            .chain(std::fs::canonicalize(slot).ok())
            .collect();
        let since = Instant::now();
        table_read(
            within,
            &mut || since.elapsed(),
            pids,
            &mut |pid| {
                let pid = i32::try_from(pid).ok()?;
                aterm_uds::process::process_serial(pid).or_else(|| {
                    (aterm_uds::process::process_presence(pid)
                        == aterm_uds::process::ProcessPresence::Exited)
                        .then_some(EXITED)
                })
            },
            &mut |pid| row(mark, session, taken, &dirs, pid),
        )
    }

    /// [`table_from`]'s repeated lists and identity-checked row cache ([`table`]); `now` is its
    /// clock.
    pub(super) fn table_read(
        within: Duration,
        now: &mut dyn FnMut() -> Duration,
        pids: &mut dyn FnMut() -> std::io::Result<Vec<u32>>,
        serial: &mut dyn FnMut(u32) -> Option<u64>,
        row: &mut dyn FnMut(u32) -> Option<Proc>,
    ) -> Option<Listing> {
        let since = now();
        let mut read = BTreeMap::new();
        let mut procs = BTreeMap::new();
        let mut first = true;
        loop {
            let Ok(list) = pids() else {
                return (!first).then_some(Listing {
                    procs: procs.into_values().collect(),
                    whole: false,
                });
            };
            let mut new = false;
            for pid in list {
                let before = serial(pid);
                if before.is_some_and(|s| read.get(&pid) == Some(&s)) {
                    continue;
                }
                new = true;
                let process = row(pid);
                if let Some(s) = before
                    && serial(pid) == Some(s)
                    && process
                        .as_ref()
                        .is_none_or(|p| p.serial == Some(s) && !p.spawning)
                {
                    read.insert(pid, s);
                } else {
                    read.remove(&pid);
                }
                // A pid that now holds another process must not leave its old parent,
                // mark or trustd status in the listing.
                procs.remove(&pid);
                if let Some(process) = process {
                    procs.insert(pid, process);
                }
            }
            if !first && (!new || now().saturating_sub(since) >= within) {
                return Some(Listing {
                    procs: procs.into_values().collect(),
                    whole: !new,
                });
            }
            first = false;
        }
    }

    /// `pid`'s row ([`table`]), `None` for this process, a zombie, or one gone mid-read.
    fn row(
        mark: &Mark,
        session: Option<i32>,
        taken: bool,
        dirs: &[PathBuf],
        pid: u32,
    ) -> Option<Proc> {
        use aterm_uds::holders::{Exe, exe_of};
        let p = i32::try_from(pid).ok()?;
        if pid == std::process::id() || p <= 1 {
            return None;
        }
        let kin = kin(p);
        if kin.as_ref().is_some_and(|k| k.zombie) {
            return None;
        }
        // A leader whose other threads still run has no image of its own to read: it is the
        // check's by its pid and serial, its session or its parent ([`marked`]), and a stranger's
        // tied to none of those counts for nothing ([`unread`]).
        let leader_only = kin.as_ref().is_some_and(|k| k.leader_only);
        let ran = aterm_uds::process::thread_ran(p);
        // An environment the kernel withheld (a platform binary's on macOS) reads as unread.
        let marked = match &kin {
            Some(k) if mark.root == Some((p, k.serial)) || mark.kept.contains(&(p, k.serial)) => {
                Some(true)
            }
            Some(_) if leader_only => None,
            Some(_) if ran == Some(true) => aterm_uds::process::read_process_markers(p)
                .filter(|m| !m.withheld)
                .map(|m| m.trust_check.is_some() && m.trust_check == mark.nonce),
            _ => None,
        };
        let exe = match marked {
            Some(_) => None,
            None if leader_only => None,
            None => match exe_of(pid) {
                Exe::Exited => return None,
                Exe::At(path) => Some(path),
                Exe::Unnamed => None,
            },
        };
        let sid = kin.as_ref().and_then(|k| k.sid);
        let member = session.is_some() && sid == session;
        let at_slot = member
            && !aterm_uds::holders::scan_path_users_of(&[pid], dirs, &[])
                .is_ok_and(|s| s.users.is_empty() && s.unreadable.is_empty());
        let another =
            kin.as_ref().is_some_and(|k| k.ctty) || (mark.nonce.is_some() && marked == Some(false));
        Some(Proc {
            pid: p,
            ppid: kin.as_ref().map_or(0, |k| k.ppid),
            serial: kin.as_ref().map(|k| k.serial),
            sid,
            trustd: kin.as_ref().is_some_and(|k| k.trustd),
            marked,
            exe,
            at_slot,
            in_session: member && !taken && !another,
            spawning: kin.as_ref().is_some_and(|k| k.forked) && ran != Some(true),
            tie: Vec::new(),
        })
    }

    /// Each `trustd` in `procs` and all it started, by parent: the lane never ends one.
    fn spared(procs: &[Proc]) -> BTreeSet<i32> {
        let mut spared: BTreeSet<i32> = procs.iter().filter(|p| p.trustd).map(|p| p.pid).collect();
        loop {
            let before = spared.len();
            for p in procs {
                if spared.contains(&p.ppid) {
                    spared.insert(p.pid);
                }
            }
            if spared.len() == before {
                return spared;
            }
        }
    }

    /// The check's processes in `procs`: each that carries its mark, each in its proven
    /// `session` ([`proven_session`]), each a signal of this sweep reached (`reached`, by pid
    /// and serial: [`Procs::send`]), and all they started (by parent, [`Proc::tie`]), never a
    /// `trustd` or what it runs. Nothing else is the check's: not a process in its root's
    /// group, nor one that works in or holds a file in its slot. macOS withholds a platform
    /// binary's environment (`/bin/sh`, `/bin/sleep`, the `/usr/bin` tools), so once its
    /// parent is gone such a process is found only by its session, while that is proven, or
    /// as one a signal reached or a sweep kept ([`Mark::kept`]).
    #[must_use]
    pub fn marked(
        procs: &[Proc],
        session: Option<i32>,
        reached: &BTreeSet<(i32, u64)>,
    ) -> Vec<Proc> {
        let spared = spared(procs);
        marked_with_spared(procs, session, reached, &spared)
    }

    fn marked_with_spared(
        procs: &[Proc],
        session: Option<i32>,
        reached: &BTreeSet<(i32, u64)>,
        spared: &BTreeSet<i32>,
    ) -> Vec<Proc> {
        let mut out: Vec<Proc> = procs
            .iter()
            .filter(|p| {
                (p.marked == Some(true)
                    || (session.is_some() && p.sid == session)
                    || p.serial.is_some_and(|s| reached.contains(&(p.pid, s))))
                    && !spared.contains(&p.pid)
            })
            .cloned()
            .collect();
        let mut seen: BTreeSet<i32> = out.iter().map(|p| p.pid).collect();
        let mut i = 0;
        while i < out.len() {
            let parent = out[i].pid;
            let tie: Vec<(i32, u64)> =
                std::iter::once((parent, out[i].serial.unwrap_or(super::NO_SERIAL)))
                    .chain(out[i].tie.iter().copied())
                    .collect();
            for p in procs {
                if p.ppid == parent && !spared.contains(&p.pid) && seen.insert(p.pid) {
                    out.push(Proc {
                        tie: tie.clone(),
                        ..p.clone()
                    });
                }
            }
            i += 1;
        }
        out
    }

    /// Whether one of `procs` tied to the check, though not shown to be one of its processes,
    /// may be: a member of its root's session ([`Proc::in_session`], [`Proc::at_slot`]); one
    /// whose mark could not be read that runs from `slot`, where a program of the check's may
    /// be in its first exec; or, on macOS, one whose mark and executable both could not be
    /// read and that one carrying the mark started. A `trustd` and what it runs never count,
    /// nor does a process tied to nothing, however unreadable: another user's is never
    /// listed, and a stranger's would hold the slot for as long as it lives. Linux holds no
    /// first exec, so there an executable it could not name counts for nothing.
    #[must_use]
    pub fn unread(procs: &[Proc], slot: &Path) -> bool {
        let canonical = std::fs::canonicalize(slot).ok();
        let spared = spared(procs);
        let started: BTreeSet<i32> = marked_with_spared(procs, None, &BTreeSet::new(), &spared)
            .iter()
            .map(|p| p.pid)
            .collect();
        procs.iter().any(|p| {
            !p.trustd
                && (p.at_slot
                    || (p.in_session && !spared.contains(&p.pid))
                    || (p.marked.is_none()
                        && p.exe.as_deref().map_or_else(
                            || cfg!(not(target_os = "linux")) && started.contains(&p.pid),
                            |exe| {
                                exe.starts_with(slot)
                                    || canonical.as_deref().is_some_and(|c| exe.starts_with(c))
                            },
                        )))
        })
    }

    /// Sweeps what is left of a check ([`marked`]), sparing every `trustd`; `act` says whether
    /// its processes are ended or only watched. `held` is the root's pid while this process
    /// holds it unreaped, `slot` is what a process not shown to be the check's must use to
    /// count ([`unread`]), and `record` keeps what the sweep signals ([`Procs::keep`]).
    #[must_use]
    pub fn end_marked(
        mark: &Mark,
        held: Option<i32>,
        slot: &Path,
        record: &Path,
        act: Act,
    ) -> Left {
        end_marked_with(
            &mut Live {
                mark,
                held,
                slot,
                record,
            },
            slot,
            act,
        )
    }

    /// [`end_marked`] over `procs`. No process is ever stopped, so a sweep cut short (its
    /// lane killed) leaves each running or ended. The check's processes are sent `SIGTERM`;
    /// after up to a second, those each fresh listing names are sent `SIGKILL`, until one
    /// names none or [`KILLS`] listings have, and only a whole one that names none, with
    /// nothing tied to the check left ([`unread`]), says it ended. A signal ends what tied a
    /// survivor to the check (its parent, the carriers that proved its session), so each
    /// listing counts what a signal already reached as the check's ([`marked`]), and the
    /// record keeps each before it is signalled ([`Procs::keep`]); when it cannot, they get
    /// `SIGKILL` at once, not a `SIGTERM` a survivor could outlive. Signals go to children
    /// before their parents ([`targets`]) and never to a row caught mid-spawn; a watch
    /// signals nothing.
    pub(super) fn end_marked_with(procs: &mut dyn Procs, slot: &Path, act: Act) -> Left {
        let mut reached = BTreeSet::new();
        let Some((first, found)) = list(procs, &reached) else {
            return Left::Unread;
        };
        if found.is_empty() {
            return if first.whole && !unread(&first.procs, slot) {
                Left::Nothing
            } else {
                Left::Unread
            };
        }
        if act == Act::Watch {
            return Left::Lives;
        }
        let term = targets(&found);
        let sig = if procs.keep(&term) { SIGTERM } else { SIGKILL };
        reached.extend(procs.send(&term, sig));
        settle(procs, &term);
        for _ in 0..KILLS {
            let Some((listed, mine)) = list(procs, &reached) else {
                return Left::Lives;
            };
            if mine.is_empty() {
                return if listed.whole && !unread(&listed.procs, slot) {
                    Left::Ended
                } else {
                    Left::Lives
                };
            }
            let kill = targets(&mine);
            procs.keep(&kill);
            reached.extend(procs.send(&kill, SIGKILL));
            settle(procs, &kill);
        }
        Left::Lives
    }

    /// The check's processes `mine` in the order a signal goes to them, children before their
    /// parents, so a child's listed parent still holds when it is signalled ([`may_signal`]).
    fn targets(mine: &[Proc]) -> Vec<Proc> {
        let mut ppids = std::collections::BTreeMap::new();
        for q in mine {
            ppids.entry(q.pid).or_insert(q.ppid);
        }
        let depth = |p: &Proc| {
            let mut depth = 0;
            let mut at = p.ppid;
            while let Some(&parent_ppid) = ppids.get(&at) {
                depth += 1;
                if depth > mine.len() {
                    break;
                }
                at = parent_ppid;
            }
            depth
        };
        let mut out = mine.to_vec();
        out.sort_by_cached_key(|p| std::cmp::Reverse(depth(p)));
        out
    }

    /// One listing, and the check's processes in it, with each `reached` names ([`marked`]).
    fn list(procs: &mut dyn Procs, reached: &BTreeSet<(i32, u64)>) -> Option<(Listing, Vec<Proc>)> {
        let listed = procs.table()?;
        let session = procs.session(&listed.procs);
        let mine = marked(&listed.procs, session, reached);
        Some((listed, mine))
    }

    /// Waits until none of `set` runs but those caught mid-spawn, which no signal went to, for
    /// at most [`Procs::settle_within`].
    fn settle(procs: &mut dyn Procs, set: &[Proc]) {
        let deadline = crate::durable::deadline_after(procs.settle_within());
        while set.iter().any(|p| !p.spawning && procs.alive(p)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_stat_line_reads_its_parent_session_serial_and_name() {
            let line = "4242 (trust d) x) S 4200 4242 4100 0 -1 4194560 1 0 0 0 0 0 0 0 20 0 1 0 987654 0 0";
            let k = stat_kin(line, 4242).unwrap();
            assert_eq!(
                (
                    k.ppid, k.sid, k.serial, k.zombie, k.ctty, k.trustd, k.forked
                ),
                (4200, Some(4100), 987_654, false, false, false, false)
            );
            assert!(!k.leader_only);
            let k = stat_kin(&line.replace("(trust d) x) S", "(trustd) Z"), 4242).unwrap();
            assert!(k.zombie && k.trustd && !k.leader_only);
            let leader = line
                .replace(") S", ") Z")
                .replace(" 20 0 1 0 ", " 20 0 3 0 ");
            let k = stat_kin(&leader, 4242).unwrap();
            assert!(k.leader_only && !k.zombie, "two threads still run");
            assert_eq!(k.serial, 987_654);
            let k = stat_kin(&line.replace(" 4100 0 ", " 4100 34816 "), 4242).unwrap();
            assert!(k.ctty && !k.zombie, "on /dev/pts/0");
            assert_eq!(k.serial, 987_654);
            assert!(stat_kin(line, 4243).is_none(), "another pid's line");
            assert!(
                stat_kin(&line.replacen("4242 ", "+4242 ", 1), 4242).is_none(),
                "plus-prefixed pid"
            );
            assert!(
                stat_kin(&line.replace(") S 4200 ", ") S +4200 "), 4242).is_none(),
                "plus-prefixed ppid"
            );
        }

        /// The re-read before a signal must show the listed serial, no zombie, under the listed
        /// parent, each tie still holding, and no `trustd` in it or its parent.
        #[test]
        fn a_signal_goes_only_to_the_process_listed_under_its_parent_and_no_trustd() {
            let listed = Proc {
                pid: 301,
                ppid: 300,
                serial: Some(3010),
                sid: Some(200),
                trustd: false,
                marked: None,
                exe: None,
                at_slot: false,
                in_session: false,
                spawning: false,
                tie: Vec::new(),
            };
            let now = |ppid, serial, zombie, trustd| Kin {
                ppid,
                serial,
                sid: Some(200),
                zombie,
                ctty: false,
                trustd,
                forked: false,
                leader_only: false,
            };
            // The kin `table` holds, by pid.
            let read = |table: &[(i32, Kin)]| {
                let table = table.to_vec();
                move |pid| table.iter().find(|(p, _)| *p == pid).map(|(_, k)| *k)
            };
            let parent = now(200, 3000, false, false);
            let seed = now(1, 2000, false, false);
            let trustd = now(1, 3000, false, true);
            assert!(may_signal(
                &listed,
                &now(300, 3010, false, false),
                &mut read(&[(300, parent)])
            ));
            assert!(
                may_signal(&listed, &now(300, 3010, false, false), &mut read(&[])),
                "a parent that cannot be read is no trustd"
            );
            for (why, now, table) in [
                ("a zombie", now(300, 3010, true, false), vec![(300, parent)]),
                (
                    "another serial",
                    now(300, 3011, false, false),
                    vec![(300, parent)],
                ),
                ("reparented", now(1, 3010, false, false), vec![]),
                ("a trustd", now(300, 3010, false, true), vec![(300, parent)]),
                (
                    "run by a trustd",
                    now(300, 3010, false, false),
                    vec![(300, trustd)],
                ),
            ] {
                assert!(!may_signal(&listed, &now, &mut read(&table)), "{why}");
            }
            let tied = Proc {
                tie: vec![(300, 3000), (200, 2000)],
                ..listed.clone()
            };
            assert!(may_signal(
                &tied,
                &now(300, 3010, false, false),
                &mut read(&[(300, parent), (200, seed)])
            ));
            for (why, table) in [
                (
                    "another process at its parent's pid",
                    vec![(300, now(200, 3001, false, false)), (200, seed)],
                ),
                ("its parent unread", vec![(200, seed)]),
                (
                    "its parent under another",
                    vec![(300, now(1, 3000, false, false)), (200, seed)],
                ),
                (
                    "another process at its parent's parent's pid",
                    vec![(300, parent), (200, now(1, 2001, false, false))],
                ),
                ("its parent's parent unread", vec![(300, parent)]),
            ] {
                assert!(
                    !may_signal(&tied, &now(300, 3010, false, false), &mut read(&table)),
                    "tied by its parents alone, {why}"
                );
            }
            let unread = Proc {
                serial: None,
                ..listed
            };
            assert!(
                !may_signal(
                    &unread,
                    &now(300, 3010, false, false),
                    &mut read(&[(300, parent)])
                ),
                "a row whose serial was not read"
            );
        }
    }
}

pub mod trustd {

    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// targo's own deadline over connect, IDENTITY and STATUS.
    #[cfg(not(test))]
    const TARGO_GIVES: Duration = Duration::from_millis(500);

    /// Under test, the one-minute hang detector: a unit test's stand-in answers from a thread
    /// of the test binary, which a loaded machine can keep from running for longer than targo's
    /// 0.5 s, and no test's property is how fast a stand-in answers.
    #[cfg(test)]
    const TARGO_GIVES: Duration = aterm_spec::wait::MUST_HAPPEN;

    /// Path to `trustd`'s per-user Unix domain socket.
    #[must_use]
    pub fn socket() -> PathBuf {
        let tmp = std::fs::canonicalize("/tmp").unwrap_or_else(|_| PathBuf::from("/tmp"));
        tmp.join(format!("trustd-runtime-locks-{}", super::proc_tree::euid()))
            .join("trust-memory-jobserver.sock")
    }

    /// SHA-256 hex digest of `exe` used by `trustd` for `IDENTITY`.
    #[must_use]
    pub fn build_digest(exe: &Path) -> String {
        crate::durable::sha256_file_with_meta(exe)
            .map(|(h, _)| h)
            .unwrap_or_default()
    }

    /// What currently serves the `trustd` socket.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Held {
        /// Socket is absent or unconnected.
        None,
        /// Served by this build's `trustd`.
        Ours,
        /// Served by another build's `trustd` or an unrecognized server.
        Foreign,
        /// Connected but did not answer within the timeout.
        Silent,
    }

    /// Result of probing the `trustd` socket.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Probe {
        pub held: Held,
        pub what: String,
    }

    impl Probe {
        /// Formats `<none|ours|foreign|silent>\t<detail>` for a check's `.trustd` mark file.
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
        const MAX_REPLY: usize = 64 * 1024;
        s.get_mut().write_all(format!("{req}\n").as_bytes())?;
        let mut line = String::new();
        if std::io::Read::by_ref(s)
            .take((MAX_REPLY as u64) + 1)
            .read_line(&mut line)?
            == 0
        {
            return Err(std::io::Error::other("it closed the connection"));
        }
        if line.len() > MAX_REPLY {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "trustd reply exceeds 64 KiB",
            ));
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
        // A Unix-domain connect never waits on Apple, so it may hold the spawn lock.
        let stream = match aterm_uds::kept_out(|| UnixStream::connect(sock)) {
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

    /// Probes `sock` against expected identity `want` with timeout `wait`.
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

    /// How often a wait for the socket looks at it again ([`wait_usable`]).
    pub const LOOK_EVERY: Duration = Duration::from_secs(5);

    /// What a look at the socket gives each answer it waits for: a trustd that takes longer
    /// reads silent, and the wait looks again.
    #[cfg(not(test))]
    const LOOK_GIVES: Duration = Duration::from_secs(10);

    /// Under test, the one-minute hang detector, as [`TARGO_GIVES`]: a unit test's stand-in
    /// trustd answers every look from a thread of the test binary, and no test holds one silent
    /// for a look.
    #[cfg(test)]
    const LOOK_GIVES: Duration = aterm_spec::wait::MUST_HAPPEN;

    /// The most leases a wait line names.
    const LEASES_SAID: usize = 8;

    const NEVER: &str = "never signal it: a killed trustd records DIRTY and blocks every check \
                         until `--recover-after-crash` attests no solver survives";

    /// A lease a trustd granted, as its STATUS names it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Lease {
        pub pid: u32,
        /// What its holder said it holds trustd for: the lane's keeper says `trust lane holds
        /// trustd for its run`.
        pub label: String,
    }

    /// The leases the trustd at `sock` says it has granted (its STATUS `active` list), or
    /// `None` when it would not say.
    fn leases(sock: &Path, wait: Duration) -> Option<Vec<Lease>> {
        let stream = aterm_uds::kept_out(|| UnixStream::connect(sock)).ok()?;
        let _ = stream.set_read_timeout(Some(wait));
        let _ = stream.set_write_timeout(Some(wait));
        let mut s = BufReader::new(stream);
        ask(&mut s, "IDENTITY").ok()?;
        let status: aterm_json::Value = aterm_json::from_str(&ask(&mut s, "STATUS").ok()?).ok()?;
        status
            .get("active")?
            .as_array()?
            .iter()
            .map(|lease| {
                Some(Lease {
                    pid: u32::try_from(lease.get("pid")?.as_u64()?).ok()?,
                    label: crate::trust_facts::one_line(
                        lease
                            .get("label")
                            .and_then(aterm_json::Value::as_str)
                            .unwrap_or(""),
                    ),
                })
            })
            .collect()
    }

    /// Whom `leases` names, for a wait line: what keeps a trustd of another build serving,
    /// since one retires 300 s after its last grant ends.
    #[must_use]
    pub fn leased_by(leases: Option<&[Lease]>) -> String {
        let Some(leases) = leases else {
            return String::new();
        };
        if leases.is_empty() {
            return "; it holds no lease, so it retires 300 s after its last grant".into();
        }
        let mut named: Vec<String> = leases
            .iter()
            .take(LEASES_SAID)
            .map(|l| {
                let label = if l.label.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", l.label)
                };
                let gone = if aterm_uds::process::pid_alive(l.pid) {
                    ""
                } else {
                    ", which has exited"
                };
                format!("pid {}{label}{gone}", l.pid)
            })
            .collect();
        if leases.len() > LEASES_SAID {
            named.push(format!("and {} more", leases.len() - LEASES_SAID));
        }
        format!("; leased by {}", named.join(", "))
    }

    /// The clock a wait for the socket keeps ([`wait_on`]): the machine's, or a test's.
    pub trait Clock {
        /// How long the wait has lasted.
        fn waited(&self) -> Duration;
        /// Nap `d`: `true` when `stop` ended it early.
        fn nap(&mut self, d: Duration, stop: &dyn Fn() -> bool) -> bool;
    }

    /// The machine's clock, napping in steps an interrupt ends ([`aterm_toolchain::interrupt::NAP`]).
    struct Wall(Instant);

    impl Clock for Wall {
        fn waited(&self) -> Duration {
            self.0.elapsed()
        }

        fn nap(&mut self, d: Duration, stop: &dyn Fn() -> bool) -> bool {
            let end = Instant::now() + d;
            loop {
                if stop() {
                    return true;
                }
                let now = Instant::now();
                if now >= end {
                    return false;
                }
                std::thread::sleep((end - now).min(aterm_toolchain::interrupt::NAP));
            }
        }
    }

    /// Why a wait for the socket ended without it ([`wait_on`]).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Unusable {
        /// `--no-wait`: what holds it.
        Held(String),
        /// An interrupt, or the caller's `stop`: what held it then.
        Stopped(String),
    }

    /// One look at `sock` against `want`, with the leases of a trustd of another build, made on
    /// a thread of its own so that `stop` is heard every [`aterm_toolchain::interrupt::NAP`]
    /// while a trustd takes its time to answer; `None` once `stop` says so first.
    fn look(
        sock: &Path,
        want: &str,
        stop: &dyn Fn() -> bool,
    ) -> Option<(Probe, Option<Vec<Lease>>)> {
        let seen = |sock: &Path, want: &str| {
            let p = probe(sock, want, LOOK_GIVES);
            let leases = (p.held == Held::Foreign)
                .then(|| leases(sock, LOOK_GIVES))
                .flatten();
            (p, leases)
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let (owned_sock, owned_want) = (sock.to_path_buf(), want.to_string());
        let spawned = std::thread::Builder::new()
            .name("trustd look".into())
            .spawn(move || {
                let _ = tx.send(seen(&owned_sock, &owned_want));
            });
        if spawned.is_err() {
            return Some(seen(sock, want));
        }
        loop {
            match rx.recv_timeout(aterm_toolchain::interrupt::NAP) {
                Ok(found) => return Some(found),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if stop() => return None,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Some(seen(sock, want));
                }
            }
        }
    }

    /// Waits for `sock` to serve this build's `trustd` or nothing: admission behind whatever
    /// holds it, with no deadline (D-TRUSTD-330S). A trustd of another build serves while any
    /// run of that build holds a lease on it, which a lane run does for hours, and retires 300 s
    /// after its last grant; a fixed wait ended a busy socket COULD NOT RUN. While it waits it
    /// says what holds the socket and who holds its leases as it starts and once a minute, and
    /// marks the wait where a ceiling reads it ([`aterm_toolchain::waits`]); it looks every
    /// [`LOOK_EVERY`]. An interrupt (SIGINT, SIGTERM or SIGHUP, [`aterm_toolchain::interrupt`])
    /// or `stop` ends it within a nap, and `--no-wait` ([`aterm_toolchain::named_lock::Wait::No`]) answers at once.
    ///
    /// # Errors
    /// What held the socket when `--no-wait` would not wait or the wait was interrupted, and
    /// that nothing was verified.
    pub fn wait_usable(
        sock: &Path,
        want: &str,
        wait: aterm_toolchain::named_lock::Wait,
        out: &mut dyn std::io::Write,
        stop: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let watch = aterm_toolchain::interrupt::Watch::enter();
        let stopped = || watch.caught().is_some() || stop();
        let ended = wait_on(sock, want, wait, out, &stopped, &mut Wall(Instant::now()));
        ended.map_err(|e| match e {
            Unusable::Held(what) => format!(
                "{what}: --no-wait does not wait for it, so nothing was verified; leave out \
                 --no-wait to wait until it serves this build or retires; {NEVER}"
            ),
            Unusable::Stopped(what) => format!(
                "interrupted{} while waiting for this build's trustd or none: {what}; nothing \
                 was verified",
                watch.take().map_or_else(String::new, |sig| format!(
                    " ({})",
                    aterm_toolchain::interrupt::name(sig)
                ))
            ),
        })
    }

    /// [`wait_usable`]'s wait on `clock`.
    ///
    /// # Errors
    /// [`Unusable`]: what held the socket when it would not wait, or when `stop` ended the wait.
    pub fn wait_on(
        sock: &Path,
        want: &str,
        wait: aterm_toolchain::named_lock::Wait,
        out: &mut dyn std::io::Write,
        stop: &dyn Fn() -> bool,
        clock: &mut dyn Clock,
    ) -> Result<(), Unusable> {
        let mut said: Option<Duration> = None;
        let mut mark: Option<aterm_toolchain::waits::Mark> = None;
        let mut held = format!("what serves {} was not read yet", sock.display());
        loop {
            let Some((p, leases)) = look(sock, want, stop) else {
                return Err(Unusable::Stopped(held));
            };
            let waited = clock.waited();
            if matches!(p.held, Held::None | Held::Ours) {
                if said.is_some() {
                    let _ = writeln!(
                        out,
                        "trust: the socket is usable after {} min: {}",
                        waited.as_secs() / 60,
                        p.what
                    );
                }
                return Ok(());
            }
            held = format!("{}{}", p.what, leased_by(leases.as_deref()));
            if wait == aterm_toolchain::named_lock::Wait::No {
                return Err(Unusable::Held(held));
            }
            let line = match said {
                None => Some(format!(
                    "trust: waiting for this build's trustd or none: {held}"
                )),
                Some(t) if waited >= t + aterm_toolchain::named_lock::SAY_EVERY => Some(format!(
                    "trust: still waiting for this build's trustd or none ({} min): {held}",
                    waited.as_secs() / 60
                )),
                Some(_) => None,
            };
            if let Some(line) = line {
                let _ = writeln!(out, "{line}");
                match &mut mark {
                    Some(m) => m.say(&line),
                    None => mark = aterm_toolchain::waits::Mark::begin(&line),
                }
                said = Some(waited);
            }
            if clock.nap(LOOK_EVERY, stop) {
                return Err(Unusable::Stopped(held));
            }
        }
    }

    /// Holds a one-byte lease on this build's `trustd` so it does not idle out mid-run, and
    /// counts the leases that ended under it ([`Keeper::lost`]).
    pub struct Keeper {
        stop: Arc<AtomicBool>,
        held: Arc<AtomicBool>,
        lost: Arc<AtomicU32>,
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
            let lost = Arc::new(AtomicU32::new(0));
            let conn = Arc::new(Mutex::new(None));
            let k = Kept {
                stop: Arc::clone(&stop),
                held: Arc::clone(&held),
                lost: Arc::clone(&lost),
                conn: Arc::clone(&conn),
            };
            let thread = std::thread::spawn(move || keep(&sock, &want, &grants, &seen, &k));
            Keeper {
                stop,
                held,
                lost,
                conn,
                thread: Some(thread),
            }
        }

        #[must_use]
        pub fn held(&self) -> bool {
            self.held.load(Ordering::SeqCst)
        }

        /// How many of its grants trustd ended under it (it closed or failed the lease's
        /// connection: it went away): a check that ran across one lost its prover. A grant
        /// whose connection trustd has already closed counts at once, before the keeper's
        /// thread reads the end, so a check that ended of that loss counts it.
        #[must_use]
        pub fn lost(&self) -> u32 {
            use std::os::fd::AsRawFd as _;
            // Read before the count: the thread counts an end before it lets `held` go, so an
            // end it has not counted yet is the held grant's, and none is missed (one counted
            // twice still counts as lost).
            let ending = self.held()
                && self
                    .conn
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .is_some_and(|c| {
                        matches!(
                            aterm_uds::hangup::fd_peer_state(c.as_raw_fd()),
                            aterm_uds::hangup::PeerState::Gone
                                | aterm_uds::hangup::PeerState::HalfClosed
                        )
                    });
            self.lost
                .load(Ordering::SeqCst)
                .saturating_add(u32::from(ending))
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
        /// The grants trustd ended under it ([`Keeper::lost`]).
        lost: Arc<AtomicU32>,
        /// The connection it waits on, for the drop to shut.
        conn: Arc<Mutex<Option<UnixStream>>>,
    }

    impl Kept {
        /// Registers the cloned connection before waiting on it.
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
            if let Ok(stream) = aterm_uds::kept_out(|| UnixStream::connect(sock)) {
                if !k.name(aterm_uds::kept_out(|| stream.try_clone())) {
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
                        let _ = crate::durable::write_fresh(grants_out, format!("{grants}\n"));
                        held.store(true, Ordering::SeqCst);
                        let _ = s.get_ref().set_read_timeout(Some(tick));
                        let mut buf = [0u8; 256];
                        let mut ended = false;
                        while !stop.load(Ordering::SeqCst) {
                            match s.get_mut().read(&mut buf) {
                                Ok(0) => {
                                    ended = true;
                                    break;
                                }
                                Ok(_) => {}
                                Err(e)
                                    if matches!(
                                        e.kind(),
                                        std::io::ErrorKind::WouldBlock
                                            | std::io::ErrorKind::TimedOut
                                    ) => {}
                                Err(_) => {
                                    ended = true;
                                    break;
                                }
                            }
                        }
                        // trustd ended the lease, not this keeper's drop: counted before
                        // `held` goes, as [`Keeper::lost`] reads them.
                        if ended && !stop.load(Ordering::SeqCst) {
                            k.lost.fetch_add(1, Ordering::SeqCst);
                        }
                        held.store(false, Ordering::SeqCst);
                    }
                } else {
                    let _ = crate::durable::write_fresh(seen_out, format!("{}\n", p.line()));
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

        /// The trustd the keeper holds a grant on goes away (TP-4): the keeper counts the grant lost
        /// once trustd's end of the lease is closed. NEGATIVE CONTROL: while trustd keeps the lease
        /// nothing is lost, and the keeper's own drop of a grant it holds is no loss.
        #[test]
        fn the_keeper_counts_a_grant_trustd_ended_and_not_its_own_drop() {
            use std::io::BufRead as _;

            for trustd_ends in [true, false] {
                let private = aterm_tempfile::Builder::new()
                    .prefix("tl-g-")
                    .tempdir_in("/tmp")
                    .unwrap();
                let d = private.path();
                let sock = d.join("g.sock");
                let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
                let (end_tx, end_rx) = std::sync::mpsc::channel::<()>();
                let (ended_tx, ended_rx) = std::sync::mpsc::channel::<()>();
                let trustd = std::thread::spawn(move || {
                    let (conn, _) = aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Forever,
                        aterm_uds::AcceptedMode::Blocking,
                    )
                    .unwrap();
                    let mut w = conn.try_clone().unwrap();
                    let mut lines = BufReader::new(conn).lines();
                    for reply in [
                        "{\"executable_sha256\":\"aa\",\"release\":\"1.99\",\"commit\":\"abc\"}",
                        "GRANTED 1",
                    ] {
                        lines.next().expect("a request").unwrap();
                        writeln!(w, "{reply}").unwrap();
                    }
                    // Gone at the test's word: the lease's connection closes, both ends of it.
                    let _ = end_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
                    drop((lines, w, listener));
                    let _ = ended_tx.send(());
                });
                let keeper = Keeper::start(sock, "aa".into(), d.join("grants"), d.join("seen"));
                let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                while !keeper.held() {
                    assert!(Instant::now() < deadline, "the keeper took no grant");
                    aterm_uds::deadline::poll_yield();
                }
                assert_eq!(keeper.lost(), 0, "a held grant is not lost");
                if trustd_ends {
                    end_tx.send(()).unwrap();
                    ended_rx
                        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                        .expect("the stand-in closed the lease");
                    // Once the lease's last copy closes: a child another test forks may hold
                    // one until it execs.
                    while keeper.lost() == 0 {
                        assert!(
                            Instant::now() < deadline,
                            "the grant trustd ended is not lost"
                        );
                        aterm_uds::deadline::poll_yield();
                    }
                    trustd.join().unwrap();
                    drop(keeper);
                } else {
                    let lost = Arc::clone(&keeper.lost);
                    drop(keeper);
                    assert_eq!(lost.load(std::sync::atomic::Ordering::SeqCst), 0);
                    drop(end_tx);
                    trustd.join().unwrap();
                }
            }
        }

        #[test]
        fn a_failed_connection_clone_refuses_an_uncancellable_wait() {
            let k = Kept {
                stop: Arc::new(AtomicBool::new(false)),
                held: Arc::new(AtomicBool::new(false)),
                lost: Arc::new(AtomicU32::new(0)),
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
            let k = Kept {
                stop: Arc::new(AtomicBool::new(false)),
                held: Arc::new(AtomicBool::new(false)),
                lost: Arc::new(AtomicU32::new(0)),
                conn: Arc::new(Mutex::new(None)),
            };
            let stop = Arc::clone(&k.stop);
            let (start_tx, start_rx) = std::sync::mpsc::channel();
            let mut keeper = Keeper {
                stop: Arc::clone(&k.stop),
                held: Arc::clone(&k.held),
                lost: Arc::clone(&k.lost),
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
                aterm_uds::deadline::poll_yield();
            }
            start_tx.send(()).unwrap();
            ended_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("the stopped worker never ended");
            ending.join().unwrap();
            assert_eq!(
                aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Ready,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .unwrap_err()
                .kind(),
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

    /// Each library's rows of one name, as a cost file reads under `prover` ([`Costs::of`]).
    fn read(rows: &CostRows, prover: &str) -> BTreeMap<String, Cost> {
        Costs::of(rows, prover).rows
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
            prover: P.into(),
            cut: None,
            raised: false,
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
            BTreeMap::from([("a".into(), vec![one(3)]), ("b".into(), vec![one(2)])])
        );
        assert!(out.is_empty());

        let seed = d.path().join("costs.tsv");
        write_cost_file(
            &seed,
            &format!("{SEED_HEADER}\n{COSTS_HEADER}\n"),
            &BTreeMap::from([
                ("a".to_string(), vec![one(9)]),
                ("c".to_string(), vec![one(4)]),
            ]),
            &mut out,
        );
        assert_eq!(
            read_costs(Some(&path), &seed, P).rows,
            BTreeMap::from([
                ("a".into(), one(9)),
                ("b".into(), one(2)),
                ("c".into(), one(4))
            ]),
            "a's seed row is newer than this clone's own"
        );
        let tied = Cost { cpu: 19, ..one(9) };
        write_costs(
            Some(&path),
            &BTreeMap::from([("a".to_string(), tied), ("c".to_string(), one(5))]),
            &mut out,
        );
        assert_eq!(
            read_costs(Some(&path), &seed, P)
                .rows
                .into_iter()
                .map(|(n, c)| (n, c.cpu))
                .collect::<Vec<_>>(),
            [("a".into(), 19), ("b".into(), 2), ("c".into(), 5)],
            "the history's row on a tie, and wherever it is newer"
        );
        let none = d.path().join("no-history.tsv");
        assert_eq!(
            read_costs(Some(&none), &seed, P).rows.len(),
            2,
            "a fresh clone"
        );
        assert_eq!(read_costs(None, &seed, P).rows.len(), 2, "no state at all");
        assert_eq!(
            read_costs(None, &none, P),
            Costs::default(),
            "every library heavy"
        );
        let committed = aterm_tempfile::manifest_dir()
            .join("../..")
            .join(trust_facts::COSTS);
        assert!(
            !read_costs(None, &committed, P).rows.is_empty(),
            "{}",
            committed.display()
        );
    }

    /// The prover the cost tests run under, and another.
    const P: &str = "targo 1.99.0-dev (20565c325 2026-10-19) (targo 0.11.0+g20565c325)";
    const OTHER: &str = "targo 1.101.0-dev (targo 0.10.0)";

    /// MEM-COSTS-NOT-PER-PROVER: cost rows are kept per prover, and a run reads its own
    /// prover's row, else a library's newest row where that names no prover (one recorded
    /// before rows named theirs); a library whose newest row another prover recorded, with
    /// none of this one's, is unmeasured under it, that row set aside for its claim alone
    /// ([`Costs::elsewhere`]). aterm-gui's 62.45 GB row of the locally
    /// built 1.101 prover (5176e9d23d) is not 20565c325's, nor is its older row that names no
    /// prover read once a named row is newer. NEGATIVE CONTROLS: under the 1.101 prover the
    /// same file reads that peak; a row that names no prover is read under any prover, as every
    /// row was before rows named theirs; and this prover's own row wins over a newer one of
    /// another's.
    #[test]
    fn a_run_reads_only_its_own_provers_cost_rows() {
        let peaks = |rows: &[Cost]| {
            rows.iter()
                .map(|c| (c.maxrss, c.prover.clone()))
                .collect::<Vec<_>>()
        };
        let text = format!(
            "{COSTS_HEADER}\n\
             aterm-gui\t1222\t1159\t14015578112\t2026-10-20T03:58:10Z\n\
             aterm-gui\t16972\t17245\t62450089984\t2026-10-20T17:45:18Z\t{OTHER}\n\
             aterm-effects\t8281\t8304\t26018627584\t2026-10-27T07:27:24Z\n\
             aterm-font\t1\t1\t15125987328\t2026-10-25T00:00:00Z\t{P}\n\
             aterm-font\t1\t1\t9000000000\t2026-10-27T00:00:00Z\t{OTHER}\n"
        );
        let rows = parse_costs(&text);
        assert_eq!(
            peaks(&rows["aterm-gui"]),
            [(62_450_089_984, OTHER.to_string())],
            "the older row that names no prover is dropped: no prover reads it again"
        );
        let here = Costs::of(&rows, P);
        assert!(!here.rows.contains_key("aterm-gui"), "{here:?}");
        assert_eq!(here.elsewhere["aterm-gui"].prover, OTHER);
        assert_eq!(here.rows["aterm-effects"].maxrss, 26_018_627_584);
        assert_eq!(here.rows["aterm-font"].maxrss, 15_125_987_328);
        let there = Costs::of(&rows, OTHER);
        assert_eq!(there.rows["aterm-gui"].maxrss, 62_450_089_984);
        assert_eq!(there.rows["aterm-effects"].maxrss, 26_018_627_584);
        assert_eq!(there.rows["aterm-font"].maxrss, 9_000_000_000);
        assert!(there.elsewhere.is_empty(), "{there:?}");

        let d = tmp("trust-lane-provers");
        let path = d.path().join("trust-costs.tsv");
        fs::write(&path, &text).unwrap();
        let mut out = Vec::new();
        write_costs(
            Some(&path),
            &BTreeMap::from([(
                "aterm-gui".to_string(),
                Cost {
                    maxrss: 30 * GIB,
                    when: "2026-10-30T00:00:00Z".into(),
                    prover: P.into(),
                    ..Cost::default()
                },
            )]),
            &mut out,
        );
        let kept = cost_file(&path);
        assert_eq!(
            peaks(&kept["aterm-gui"]),
            [
                (62_450_089_984, OTHER.to_string()),
                (30 * GIB, P.to_string())
            ],
            "a row per prover, oldest first: a reader that keeps a library's last row keeps its newest"
        );
        assert_eq!(read(&kept, P)["aterm-gui"].maxrss, 30 * GIB);
        assert_eq!(read(&kept, OTHER)["aterm-gui"].maxrss, 62_450_089_984);
        let written = fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("\naterm-effects\t8281\t8304\t26018627584\t2026-10-27T07:27:24Z\n"),
            "a row that names no prover is written as it was read: {written}"
        );
        assert!(out.is_empty());
    }

    /// F9: a check cut short (over budget, interrupted, or ended without a whole summary) keeps
    /// what it measured in its prover's row, each figure the least it would have taken: the row
    /// raises the peak its prover reads (its own row, else the newest that names none) and never
    /// lowers it, keeps the larger times, is marked `raised` where the row it raised was a
    /// finished check's, is written with how it was cut and read back so, and, where no check of
    /// the library under its prover finished, claims at least what an unmeasured library claims
    /// and runs first, as one with no row does. One that raised a finished check's row claims
    /// its peak and margin alone and takes its place by its CPU: its prover has measured the
    /// library. NEGATIVE CONTROLS: a finished check's row replaces a higher one, claims its peak
    /// and its margin alone, and is ordered by its CPU; a seventh column naming no cut, and an
    /// eighth naming anything but `raised` or on a finished row, are not read.
    #[test]
    fn a_cut_checks_peak_is_a_lower_bound_its_prover_reads() {
        let row = |maxrss: u64, day: u64, cut: Option<Cut>| Cost {
            cpu: 100 * day,
            wall: 100 * day,
            maxrss,
            when: format!("2026-11-0{day}T00:00:00Z"),
            prover: P.into(),
            cut,
            raised: false,
        };
        let (over, interrupted) = (Some(Cut::OverBudget), Some(Cut::Interrupted));
        let mut rows = CostRows::new();
        keep(&mut rows, "a", row(40 * GIB, 1, None));
        keep(&mut rows, "a", row(20 * GIB, 2, interrupted));
        assert_eq!(
            read(&rows, P)["a"],
            row(40 * GIB, 1, None),
            "a cut check that reached less lowers nothing"
        );
        keep(
            &mut rows,
            "a",
            Cost {
                cpu: 50,
                wall: 60,
                ..row(47 * GIB, 3, over)
            },
        );
        let raised = Cost {
            cpu: 100,
            wall: 100,
            raised: true,
            ..row(47 * GIB, 3, over)
        };
        assert_eq!(
            rows["a"],
            [raised],
            "one that reached more raises the peak, keeping the larger times and that a check finished"
        );
        let mut again = rows.clone();
        keep(&mut again, "a", row(48 * GIB, 4, interrupted));
        assert!(
            again["a"][0].raised,
            "a row that raised a finished one's stays measured: {again:?}"
        );
        let mut finished = rows.clone();
        keep(&mut finished, "a", row(20 * GIB, 4, None));
        assert_eq!(
            finished["a"],
            [row(20 * GIB, 4, None)],
            "NEGATIVE CONTROL: a finished check's row is what it took, lower or not"
        );
        let mut unnamed = CostRows::new();
        keep(
            &mut unnamed,
            "b",
            Cost {
                prover: String::new(),
                ..row(30 * GIB, 1, None)
            },
        );
        keep(&mut unnamed, "b", row(10 * GIB, 2, interrupted));
        assert_eq!(
            (read(&unnamed, P)["b"].maxrss, read(&unnamed, P)["b"].cut),
            (30 * GIB, None),
            "a prover with no row of its own reads the one that names none, which a lower cut row leaves"
        );
        let mut unmeasured = CostRows::new();
        keep(&mut unmeasured, "e", row(10 * GIB, 1, interrupted));
        keep(&mut unmeasured, "e", row(20 * GIB, 2, over));
        assert!(
            !unmeasured["e"][0].raised,
            "a cut row that raised only a cut one's is no measurement: {unmeasured:?}"
        );

        let d = tmp("trust-lane-cut");
        let path = d.path().join("trust-costs.tsv");
        let mut out = Vec::new();
        rows.insert(
            "c".into(),
            vec![Cost {
                prover: String::new(),
                ..row(GIB, 1, Some(Cut::Failed))
            }],
        );
        write_cost_file(
            &path,
            &format!(
                "{COSTS_HEADER}
"
            ),
            &rows,
            &mut out,
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(&format!(
                "
a	100	100	{}	2026-11-03T00:00:00Z	{P}	over-budget	raised
",
                47 * GIB
            )) && text.contains(&format!(
                "
c	100	100	{GIB}	2026-11-01T00:00:00Z		failed
"
            )),
            "{text}"
        );
        assert_eq!(parse_costs(&text), rows, "read back as written");
        assert!(out.is_empty());
        assert!(
            parse_costs(&format!(
                "d	1	1	1	2026-11-01T00:00:00Z	{P}	later
"
            ))
            .is_empty(),
            "a seventh column naming no cut is not read"
        );
        for eighth in ["over-budget\tlater", "\traised"] {
            assert!(
                parse_costs(&format!(
                    "d\t1\t1\t1\t2026-11-01T00:00:00Z\t{P}\t{eighth}\n"
                ))
                .is_empty(),
                "{eighth:?}"
            );
        }

        let ram = 48 * GIB;
        let low = row(GIB, 1, Some(Cut::Failed));
        assert_eq!(
            claim_need(Some(&low), ram),
            16 * GIB,
            "a cut check's small peak claims what an unmeasured library does"
        );
        assert_eq!(
            claim_need(
                Some(&Cost {
                    cut: None,
                    ..low.clone()
                }),
                ram
            ),
            2 * GIB,
            "NEGATIVE CONTROL: a finished check's claims its peak and its margin"
        );
        assert_eq!(
            claim_need(Some(&row(47 * GIB, 1, interrupted)), ram),
            aterm_capacity::peak_claim(47 * GIB),
            "a high one claims its peak and its margin"
        );
        let measured = Cost {
            raised: true,
            ..low.clone()
        };
        assert_eq!(
            claim_need(Some(&measured), ram),
            2 * GIB,
            "one that raised a finished check's row claims its peak and its margin alone"
        );
        let costs = BTreeMap::from([
            ("long".to_string(), row(GIB, 9, None)),
            ("cut".to_string(), low),
            ("raised".to_string(), measured),
        ]);
        assert_eq!(
            order(&["long", "cut", "new", "raised"], &costs),
            [1, 2, 0, 3],
            "a cut row's length is unknown: it runs first, as one with no row does, unless a check under its prover finished"
        );
    }

    /// The store's default prover when TRUST-MEM-2 ended, under which aterm-gui has no row.
    const NEW: &str = "targo 1.101.0-dev (29ebb403d 2026-10-28) (targo 0.12.0+g29ebb403d)";

    /// TRUST-MEM-2: where no check of a library under the prover that runs has finished,
    /// another prover's newest row of it is the best estimate of what its check needs, so its
    /// claim is at least that row's, and the up-front refusal reads that row where its peak is
    /// the larger, naming its prover. aterm-gui under 29ebb403d, with 20565c325's 47.20 GB
    /// interrupted row and the older 1.101 62.45 GB one, claims on the newer. A small peak
    /// another prover recorded still claims what an unmeasured library claims, since a prover
    /// that took a ceiling held its peaks down. A row of this prover's own that finished
    /// replaces the other's; one cut short below it does not. NEGATIVE CONTROL: the rows this
    /// prover reads, and so the order, hold none of another prover's.
    #[test]
    fn another_provers_peak_sizes_the_claim_until_this_provers_own_replaces_it() {
        let ram = 48 * GIB;
        let bound = REACH;
        let row = |maxrss: u64, day: u64, prover: &str, cut: Option<Cut>| Cost {
            cpu: 100,
            wall: 100,
            maxrss,
            when: format!("2026-10-{day}T00:00:00Z"),
            prover: prover.into(),
            cut,
            raised: false,
        };
        let gui = row(47_198_031_152, 31, P, Some(Cut::Interrupted));
        let mut rows = CostRows::new();
        keep(&mut rows, "aterm-gui", row(62_450_089_984, 20, OTHER, None));
        keep(&mut rows, "aterm-gui", gui.clone());
        keep(&mut rows, "tiny", row(2 * GIB, 20, OTHER, None));
        let costs = Costs::of(&rows, NEW);
        assert!(costs.rows.is_empty(), "{costs:?}");
        assert_eq!(costs.elsewhere["aterm-gui"], gui, "the newest other row");
        assert_eq!(
            costs.claim("aterm-gui", ram, bound),
            aterm_capacity::peak_claim(47_198_031_152)
        );
        assert_eq!(costs.estimate("aterm-gui", ram, bound), Some(&gui));
        assert_eq!(
            costs.peak("aterm-gui", bound),
            Some(trust_facts::Peak {
                rss: 47_198_031_152,
                when: gui.when.clone(),
                own: false,
                least: true,
                of: P.into(),
            })
        );
        assert_eq!(costs.claim("tiny", ram, bound), 16 * GIB);
        assert_eq!(
            costs.estimate("tiny", ram, bound),
            None,
            "a row that raises nothing is no estimate a run names"
        );
        assert_eq!(costs.claim("unknown", ram, bound), 16 * GIB);
        assert_eq!(order(&["tiny", "aterm-gui"], &costs.rows), [0, 1]);

        let mut own = rows.clone();
        keep(
            &mut own,
            "aterm-gui",
            row(20 * GIB, 30, NEW, Some(Cut::OverBudget)),
        );
        let costs = Costs::of(&own, NEW);
        assert_eq!(
            costs.claim("aterm-gui", ram, bound),
            aterm_capacity::peak_claim(47_198_031_152),
            "a cut row of its own below the other's peak leaves the other's claim"
        );
        assert_eq!(
            costs.peak("aterm-gui", bound).map(|p| p.of),
            Some(P.to_string())
        );
        keep(&mut own, "aterm-gui", row(10 * GIB, 30, NEW, None));
        let costs = Costs::of(&own, NEW);
        assert!(!costs.elsewhere.contains_key("aterm-gui"), "{costs:?}");
        assert_eq!(costs.claim("aterm-gui", ram, bound), 12 * GIB);
        assert_eq!(
            costs.peak("aterm-gui", bound),
            Some(trust_facts::Peak {
                rss: 10 * GIB,
                when: "2026-10-30T00:00:00Z".into(),
                own: true,
                least: false,
                of: String::new(),
            }),
            "a finished row of its own replaces the other's"
        );
    }

    /// What this 48 GiB Mac can ever make available, the RAM less 3.5 GiB wired.
    const REACH: u64 = 48 * GIB - 7 * GIB / 2;

    /// TRUST-MEM-2's review: this prover's own measurement stands against another prover's
    /// peak wherever it decides by itself. costs.tsv's two aterm-gui rows, on this 48 GiB Mac:
    /// under 20565c325 its own interrupted 47.20 GB row claims 56.64 GB, which is refused by
    /// itself, so it is refused as `own-least` on that peak and claims that, though the older
    /// 1.101 row peaked at 62.45 GB; under 29ebb403d, with no row of its own, it is refused as
    /// `other-least` naming 20565c325, on the same claim. A check that finished under the
    /// prover that runs, then one cut short above it, leaves its row measured: it claims its own
    /// peak and margin and is refused or admitted on that, and no other prover's row stands in,
    /// older or newer. NEGATIVE CONTROLS: a cut row of its own whose claim is admissible, though
    /// newer than another prover's row, leaves that row's larger claim, since a check cut short
    /// early says nothing of the library's scale; and where 20565c325's own peak decides
    /// nothing (the memory unread, or a host where its claim fits), its run claims on the 1.101
    /// peak, the larger.
    #[test]
    fn this_provers_own_measurement_stands_where_it_decides() {
        let host = trust_facts::Host {
            mem: 48 * GIB,
            reach: REACH,
            uncapped: true,
            ..trust_facts::Host::default()
        };
        let (ram, bound) = (host.mem, host.admits_below());
        let gui = |rows: &CostRows, prover: &str, bound: u64| {
            let costs = Costs::of(rows, prover);
            let peak = costs.peak("aterm-gui", bound).unwrap();
            let seen = trust_facts::peak_seen(&host, &peak).map(|s| (s.outcome, s.prover));
            (seen, peak.rss, costs.claim("aterm-gui", ram, bound))
        };
        let rows = parse_costs(&format!(
            "{COSTS_HEADER}\n\
             aterm-gui\t16972\t17245\t62450089984\t2026-10-20T17:45:18Z\t{OTHER}\n\
             aterm-gui\t5862\t7547\t47198031152\t2026-10-31T01:00:39Z\t{P}\tinterrupted\n"
        ));
        let least = aterm_capacity::peak_claim(47_198_031_152);
        assert_eq!(least, 56_637_637_382);
        assert_eq!(
            gui(&rows, P, bound),
            (
                Some((trust_facts::OWN_LEAST.into(), String::new())),
                47_198_031_152,
                least
            ),
            "under 20565c325, its own peak"
        );
        assert_eq!(
            gui(&rows, NEW, bound),
            (
                Some((trust_facts::OTHER_LEAST.into(), P.into())),
                47_198_031_152,
                least
            ),
            "under 29ebb403d, 20565c325's"
        );
        let bigger = aterm_capacity::peak_claim(62_450_089_984);
        for undecided in [0, 128 * GIB] {
            let (_, rss, claim) = gui(&rows, P, undecided);
            assert_eq!(
                (rss, claim),
                (62_450_089_984, bigger),
                "NEGATIVE CONTROL: a bound of {undecided} that its own peak does not reach"
            );
        }

        let row = |maxrss: u64, when: &str, prover: &str, cut: Option<Cut>| Cost {
            cpu: 100,
            wall: 100,
            maxrss,
            when: format!("2026-11-{when}T00:00:00Z"),
            prover: prover.into(),
            cut,
            raised: false,
        };
        for other in ["02", "04"] {
            let mut rows = CostRows::new();
            keep(&mut rows, "x", row(10 * GIB, "01", P, None));
            keep(&mut rows, "x", row(62_450_089_984, other, OTHER, None));
            keep(
                &mut rows,
                "x",
                row(11 * GIB, "03", P, Some(Cut::Interrupted)),
            );
            let costs = Costs::of(&rows, P);
            assert!(costs.rows["x"].raised, "{costs:?}");
            assert!(costs.elsewhere.is_empty(), "{other}: {costs:?}");
            assert_eq!(
                costs.claim("x", ram, bound),
                aterm_capacity::peak_claim(11 * GIB),
                "{other}: its own peak and margin, not 16 GiB nor the other's"
            );
            let peak = costs.peak("x", bound).unwrap();
            assert_eq!((peak.rss, peak.own, peak.least), (11 * GIB, true, true));
            assert_eq!(trust_facts::peak_seen(&host, &peak), None, "{other}");
            assert_eq!(costs.estimate("x", ram, bound), None, "{other}");
        }

        let mut early = CostRows::new();
        keep(
            &mut early,
            "aterm-gui",
            row(47_198_031_152, "01", P, Some(Cut::Interrupted)),
        );
        keep(
            &mut early,
            "aterm-gui",
            row(10 * GIB, "02", NEW, Some(Cut::Interrupted)),
        );
        assert_eq!(
            gui(&early, NEW, bound),
            (
                Some((trust_facts::OTHER_LEAST.into(), P.into())),
                47_198_031_152,
                least
            ),
            "NEGATIVE CONTROL: its own newer 10 GiB lower bound decides nothing"
        );
    }

    #[derive(Default)]
    struct World {
        now: Duration,
        live: usize,
        peak: usize,
        starts: Vec<String>,
        /// When each check started, in order.
        started_at: Vec<Duration>,
        /// What the launch gate said, and when.
        said: Vec<(Duration, String)>,
        ended_by_lane: Vec<String>,
        /// The slots each check started in, in order.
        slots: Vec<usize>,
        /// The held slots: a test's, and each one the lane ended a check in, as a stamp does.
        held: BTreeSet<usize>,
        /// Each check's cover marker as it started, with the gate ledger's claim that marker
        /// names then (its `what`), in order.
        covers: Vec<(String, Option<String>, Option<String>)>,
    }

    struct Fake<'m> {
        w: Rc<RefCell<World>>,
        /// How long each library's check runs; absent runs forever.
        takes: BTreeMap<String, Duration>,
        /// trustd is held once this many checks have started.
        held_after: usize,
        /// trustd lets go at this moment.
        trustd_until: Option<Duration>,
        interrupt_at: Option<Duration>,
        unstartable: Option<String>,
        /// The host's load at a moment of the fake clock; `None` reads none.
        load: Option<fn(Duration) -> f64>,
        /// The launch gate's memory admission; none admits every check unclaimed.
        gate: Option<Admission<'m>>,
        /// A claim another holder keeps in the gate's ledger, let go at that moment.
        holder: Option<(Duration, aterm_capacity::Claim)>,
        /// The gate ledger's lock, held as another admission deciding holds it, let go at that
        /// moment.
        turn: Option<(Duration, aterm_capacity::Turn)>,
    }

    struct FakeCheck {
        w: Rc<RefCell<World>>,
        name: String,
        slot: usize,
        until: Option<Duration>,
        done: bool,
    }

    impl FakeCheck {
        fn finish(&mut self, code: i32) -> Ended {
            if !self.done {
                self.done = true;
                self.w.borrow_mut().live -= 1;
            }
            Ended {
                code,
                cpu: Duration::from_secs(1),
                maxrss: 1,
                threads: None,
            }
        }
    }

    impl Check for FakeCheck {
        fn poll(&mut self) -> std::io::Result<Option<Ended>> {
            let now = self.w.borrow().now;
            Ok(self.until.filter(|u| now >= *u).map(|_| self.finish(0)))
        }
        fn end(&mut self) -> Ended {
            let mut w = self.w.borrow_mut();
            w.ended_by_lane.push(self.name.clone());
            w.held.insert(self.slot);
            drop(w);
            self.finish(137)
        }
    }

    impl Checks for Fake<'_> {
        fn start(&mut self, job: &mut Job) -> std::io::Result<Box<dyn Check>> {
            if self.unstartable.as_deref() == Some(job.name.as_str()) {
                return Err(std::io::Error::other("no targo"));
            }
            job.target = PathBuf::from(format!("/slots/j{}", job.slot));
            let named = job.cover.as_ref().and_then(|marker| {
                let ledger = self.gate.as_ref()?.ledger.as_ref()?;
                ledger
                    .list()
                    .ok()?
                    .into_iter()
                    .find(|r| r.held.is_some() && r.cover_marker() == *marker)
                    .map(|r| r.need.what)
            });
            let mut w = self.w.borrow_mut();
            w.covers.push((job.name.clone(), job.cover.clone(), named));
            w.starts.push(job.name.clone());
            let now = w.now;
            w.started_at.push(now);
            w.slots.push(job.slot);
            w.live += 1;
            w.peak = w.peak.max(w.live);
            Ok(Box::new(FakeCheck {
                w: Rc::clone(&self.w),
                name: job.name.clone(),
                slot: job.slot,
                until: self.takes.get(&job.name).map(|d| w.now + *d),
                done: false,
            }))
        }
        fn admit(&mut self, job: &Job) -> Option<Admitted> {
            let Some(gate) = &mut self.gate else {
                return Some(Admitted::unclaimed());
            };
            let now = self.w.borrow().now;
            let w = Rc::clone(&self.w);
            gate.admit(job, now, &mut |line| {
                w.borrow_mut().said.push((now, line.to_string()));
            })
        }
        fn waiting(&self) -> Option<String> {
            self.gate.as_ref().and_then(Admission::waiting)
        }
        fn given_up(&mut self) {
            if let Some(gate) = &mut self.gate {
                gate.stopped(Some(INTERRUPTED_BY));
            }
        }
        fn trustd_held(&self) -> bool {
            let w = self.w.borrow();
            w.starts.len() >= self.held_after && self.trustd_until.is_none_or(|t| w.now < t)
        }
        fn trustd_lost(&self) -> u32 {
            u32::from(self.trustd_until.is_some_and(|t| self.w.borrow().now >= t))
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
            let now = {
                let mut w = self.w.borrow_mut();
                w.now += Duration::from_secs(1);
                w.now
            };
            if self.holder.as_ref().is_some_and(|(at, _)| now >= *at) {
                self.holder = None;
            }
            if self.turn.as_ref().is_some_and(|(at, _)| now >= *at) {
                self.turn = None;
            }
        }
        fn held(&self, slot: usize) -> bool {
            self.w.borrow().held.contains(&slot)
        }
        fn load(&self) -> Option<f64> {
            self.load.map(|l| l(self.w.borrow().now))
        }
    }

    /// A check of `name` that claims no memory.
    fn job(name: &str) -> Job {
        Job {
            slot: 0,
            name: name.into(),
            argv: Vec::new(),
            log: PathBuf::from(format!("/nowhere/{name}.log")),
            report: None,
            target: PathBuf::new(),
            need: 0,
            lock: None,
            cover: None,
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
            let mut check = job(row);
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

    fn fake(takes: &[(&str, u64)]) -> Fake<'static> {
        Fake {
            w: Rc::new(RefCell::new(World::default())),
            takes: takes
                .iter()
                .map(|(n, s)| ((*n).to_string(), Duration::from_secs(*s)))
                .collect(),
            held_after: 0,
            trustd_until: None,
            interrupt_at: None,
            unstartable: None,
            load: None,
            gate: None,
            holder: None,
            turn: None,
        }
    }

    /// A check across which the run's trustd went away is marked so (TP-4: its trustc then dies
    /// of SIGPIPE and measures nothing, which says nothing of the code), and only that one:
    /// NEGATIVE CONTROL, a check that ended before trustd went away, and one that started after,
    /// are not.
    #[test]
    fn a_check_the_trustd_went_away_under_is_marked_and_no_other() {
        let mut f = fake(&[("before", 3), ("across", 20), ("after", 2)]);
        f.trustd_until = Some(Duration::from_secs(10));
        let jobs = ["before", "across"].map(job).to_vec();
        let (r, done) = slots(&mut f, jobs, 2, 100);
        assert_eq!(r, Ok(()));
        let lost: Vec<(&str, bool)> = done
            .iter()
            .map(|d| (d.job.name.as_str(), d.trustd_lost))
            .collect();
        assert_eq!(lost, [("before", false), ("across", true)]);
        let (r, done) = slots(&mut f, vec![job("after")], 1, 100);
        assert_eq!(r, Ok(()));
        assert!(!done[0].trustd_lost, "it started after the loss");
    }

    /// A check's load is the mean of the readings taken while it ran, not the last one; a
    /// host whose load cannot be read leaves none.
    #[test]
    fn a_check_carries_the_mean_load_it_ran_under() {
        let mut f = fake(&[("a", 4)]);
        f.load = Some(|now| now.as_secs_f64());
        let (r, done) = slots(&mut f, vec![job("a")], 1, 100);
        assert_eq!(r, Ok(()));
        assert_eq!(done[0].load, Some(2.0), "read at 0, 1, 2, 3 and 4 s");
        let mut f = fake(&[("a", 4)]);
        let (_, done) = slots(&mut f, vec![job("a")], 1, 100);
        assert_eq!(done[0].load, None);
    }

    #[test]
    fn a_run_keeps_its_obligation_records_unless_told_not_to() {
        let kept = |args: &[&str]| parse(&mut args.iter().copied()).map(|c| c.obligations);
        assert_eq!(kept(&["-p", "alpha"]), Ok(true));
        assert_eq!(kept(&["-p", "alpha", "--no-obligations"]), Ok(false));
        assert_eq!(
            kept(&["--no-obligations", "--obligations", "-p", "alpha"]),
            Ok(true)
        );
    }

    /// `--reason` says why `--update-ratchet` lowers a floor, so it is refused without it, and
    /// without words: a blank one would leave the row's fifth column blank.
    #[test]
    fn a_reason_is_given_with_update_ratchet() {
        let reason = |args: &[&str]| parse(&mut args.iter().copied()).map(|c| c.reason);
        assert_eq!(
            reason(&["-p", "alpha", "--update-ratchet", "--reason", "re-based"]),
            Ok(Some("re-based".into()))
        );
        assert_eq!(
            reason(&["-p", "alpha", "--update-ratchet", "--reason", " re-based\t"]),
            Ok(Some(" re-based\t".into()))
        );
        assert_eq!(reason(&["-p", "alpha", "--update-ratchet"]), Ok(None));
        for args in [
            &["-p", "alpha", "--reason", "re-based"][..],
            &["-p", "alpha", "--update-ratchet", "--reason"],
            &["-p", "alpha", "--update-ratchet", "--reason", ""],
            &["-p", "alpha", "--update-ratchet", "--reason", " "],
            &["-p", "alpha", "--update-ratchet", "--reason", "\t\n"],
        ] {
            assert!(reason(args).is_err(), "{args:?}");
        }
    }

    /// A run waits for room for its disk unless `--admit now` says to take it at once; another
    /// word is refused, naming it.
    #[test]
    fn a_run_waits_for_its_disk_unless_admitted_now() {
        let admit = |args: &[&str]| parse(&mut args.iter().copied()).map(|c| c.admit);
        assert_eq!(admit(&["-p", "alpha"]), Ok(atpkg::admit_disk::Admit::Wait));
        assert_eq!(
            admit(&["-p", "alpha", "--admit", "now"]),
            Ok(atpkg::admit_disk::Admit::Now)
        );
        assert!(
            admit(&["--admit", "later", "-p", "alpha"]).is_err_and(|e| e.contains("\"later\""))
        );
    }

    fn slots(
        f: &mut Fake<'_>,
        jobs: Vec<Job>,
        n: usize,
        budget: u64,
    ) -> (Result<(), Stopped>, Vec<Finished>) {
        let mut done = Vec::new();
        let r = run_slots(jobs, n, Duration::from_secs(budget), f, &mut |fin, _| {
            done.push(fin)
        });
        (r, done)
    }

    #[test]
    fn each_queued_check_runs_once_within_the_slots() {
        let mut f = fake(&[("a", 5), ("b", 1), ("c", 2), ("d", 1)]);
        let jobs = ["a", "b", "c", "d"].map(job).to_vec();
        let (r, done) = slots(&mut f, jobs, 2, 100);
        assert_eq!(r, Ok(()));
        let w = f.w.borrow();
        assert_eq!(w.starts, ["a", "b", "c", "d"]);
        assert_eq!(w.peak, 2);
        assert_eq!(done.len(), 4);
        assert!(
            done.iter()
                .all(|d| !d.over_budget && !d.interrupted && d.ended.code == 0)
        );
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

    const GIB: u64 = 1 << 30;

    /// The signal a fake run's interrupt is (`SIGTERM`), as its gate's row names it.
    const INTERRUPTED_BY: i32 = 15;

    /// A scratch capacity ledger, removed when the test ends.
    fn ledger() -> (aterm_tempfile::TempDir, aterm_capacity::Ledger) {
        let d = tmp("trust-gate");
        let l = aterm_capacity::Ledger::at(d.path().join("ledger"));
        (d, l)
    }

    /// A check of `name` claiming `gib` GiB.
    fn needing(name: &str, gib: u64) -> Job {
        Job {
            need: gib * GIB,
            ..job(name)
        }
    }

    /// The gate on `machine`'s memory in `ledger`: one look every 2 s, a line a minute.
    fn gate<'m>(
        ledger: &aterm_capacity::Ledger,
        machine: &'m aterm_capacity::Fixed,
        mode: aterm_capacity::Admit,
    ) -> Admission<'m> {
        Admission::new(
            Some(ledger.clone()),
            machine,
            mode,
            aterm_capacity::Pace {
                poll: Duration::from_secs(2),
                line_every: Duration::from_secs(60),
            },
        )
    }

    /// The slot count follows memory: on 40 GiB available, two checks claiming 25 GiB each run
    /// one after the other in three slots, and two claiming 15 GiB run side by side. Each
    /// claim is held while its check runs and let go when it ends.
    #[test]
    fn checks_whose_claims_do_not_fit_together_run_one_at_a_time() {
        let (_d, l) = ledger();
        let m = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
        let mut f = fake(&[("a", 5), ("b", 5)]);
        f.gate = Some(gate(&l, &m, aterm_capacity::Admit::Wait));
        let jobs = vec![needing("a", 25), needing("b", 25)];
        let mut held = Vec::new();
        let r = run_slots(jobs, 3, Duration::from_secs(100), &mut f, &mut |fin, _| {
            let claims = l.list().unwrap();
            held.push((fin.job.name.clone(), claims.len()));
        });
        assert_eq!(r, Ok(()));
        let w = f.w.borrow();
        assert_eq!(w.peak, 1, "25 + 25 GiB never fit in 40");
        assert!(w.started_at[1] >= w.started_at[0] + Duration::from_secs(5));
        assert_eq!(
            held,
            [("a".to_string(), 2), ("b".to_string(), 1)],
            "a's claim and b's waiter, then b's claim, as each ended"
        );
        drop(w);
        assert!(l.list().unwrap().is_empty(), "every claim let go");

        let mut g = fake(&[("a", 5), ("b", 5)]);
        g.gate = Some(gate(&l, &m, aterm_capacity::Admit::Wait));
        slots(&mut g, vec![needing("a", 15), needing("b", 15)], 3, 100)
            .0
            .unwrap();
        assert_eq!(g.w.borrow().peak, 2, "15 + 15 GiB fit in 40");
    }

    /// Each check is handed the cover marker of its own claim (BUILD-ADMIT), so its builds are
    /// covered by that claim alone, never by another check's this process holds. NEGATIVE
    /// CONTROL: a check that claims nothing is handed none (its child gets `NO_COVER`).
    #[test]
    fn each_check_is_handed_its_own_claims_cover_marker() {
        let (_d, l) = ledger();
        let m = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
        let mut g = fake(&[("a", 5), ("b", 5), ("free", 5)]);
        g.gate = Some(gate(&l, &m, aterm_capacity::Admit::Wait));
        let jobs = vec![needing("a", 4), needing("b", 4), job("free")];
        assert_eq!(slots(&mut g, jobs, 3, 100).0, Ok(()));
        let covers = g.w.borrow().covers.clone();
        let named: Vec<(String, Option<String>)> = covers
            .iter()
            .map(|(n, _, w)| (n.clone(), w.clone()))
            .collect();
        assert_eq!(
            named,
            [
                ("a".to_string(), Some("trust check a".to_string())),
                ("b".to_string(), Some("trust check b".to_string())),
                ("free".to_string(), None),
            ]
        );
        assert_eq!(covers[2].1, None, "no claim, no marker");
        assert_eq!(
            aterm_capacity::Record::marker_holder(covers[0].1.as_deref().expect("a's")),
            Some(std::process::id())
        );
    }

    /// A check's memory claim asks to cover the builds its check starts (BUILD-ADMIT): its record
    /// says so (`Need::covers`, which `aterm_capacity::covering` reads; its coverage tests hold
    /// what a covering claim covers). NEGATIVE CONTROL: a plain claim beside it does not.
    #[test]
    fn a_checks_memory_claim_covers_the_builds_it_starts() {
        let (_d, l) = ledger();
        let m = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
        let mut g = gate(&l, &m, aterm_capacity::Admit::Wait);
        let admitted = g
            .admit(&needing("a", 2), Duration::ZERO, &mut |_| {})
            .expect("admitted at its first look");
        let plain = match aterm_capacity::Waiter::enter(
            &l,
            &m,
            aterm_capacity::Need::new("hold").mem_gib(1),
        )
        .unwrap()
        .poll(&m)
        {
            aterm_capacity::Polled::Admitted(c) => c,
            aterm_capacity::Polled::Waiting(_, short) => panic!("the holder waits: {short}"),
            aterm_capacity::Polled::Gone(e) => panic!("the holder is gone: {e}"),
            aterm_capacity::Polled::Unqualified(why) => {
                panic!("load-only holder unqualified: {why}")
            }
        };
        let covers: Vec<(String, bool)> = l
            .list()
            .unwrap()
            .iter()
            .map(|r| (r.need.what.clone(), r.need.covers))
            .collect();
        assert_eq!(
            covers,
            [
                ("trust check a".to_string(), true),
                ("hold".to_string(), false)
            ]
        );
        drop((admitted, plain));
    }

    /// A check waiting at the launch gate whose claim file is removed (the ledger's directory
    /// cleaned) can never be admitted, since its grant could not be written: at its next look it
    /// starts unclaimed, saying so and naming the file, as a check whose ledger cannot be used
    /// does (DA-TRUST-GATE-FILE-GONE; before, it waited for ever, a line a minute). NEGATIVE
    /// CONTROL: with its file in place it is still waiting at every look.
    #[cfg(unix)]
    #[test]
    fn a_check_whose_claim_file_is_removed_at_the_gate_starts_unclaimed() {
        for remove in [true, false] {
            let (_d, l) = ledger();
            let m = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
            let hold = match aterm_capacity::Waiter::enter(
                &l,
                &m,
                aterm_capacity::Need::new("hold").mem_gib(30),
            )
            .unwrap()
            .poll(&m)
            {
                aterm_capacity::Polled::Admitted(c) => c,
                other => panic!("the holder is not admitted: {other:?}"),
            };
            let mut g = gate(&l, &m, aterm_capacity::Admit::Wait);
            let check = needing("a", 20);
            let mut said = Vec::new();
            assert!(
                g.admit(&check, Duration::ZERO, &mut |s| said.push(s.to_string()))
                    .is_none(),
                "20 GiB beside 30 waits"
            );
            if remove {
                for e in l.dir().read_dir().expect("the ledger").flatten() {
                    if e.file_name().to_string_lossy().ends_with(".wait") {
                        fs::remove_file(e.path()).expect("a's file removed");
                    }
                }
            }
            let mut look = |n: u64, said: &mut Vec<String>| {
                g.admit(&check, Duration::from_secs(2 * n), &mut |s| {
                    said.push(s.to_string());
                })
                .is_some()
            };
            if remove {
                assert!(look(1, &mut said), "started at the next look: {said:?}");
                let line = said.last().expect("it says why");
                assert!(
                    line.contains("starts unclaimed")
                        && line.contains(".wait")
                        && line.contains("was removed while it waited"),
                    "{line}"
                );
                assert!(
                    l.list().unwrap().iter().all(|r| r.need.what == "hold"),
                    "it holds no claim"
                );
            } else {
                for n in 1..=3 {
                    assert!(!look(n, &mut said), "still waiting at look {n}: {said:?}");
                }
            }
            drop(hold);
        }
    }

    /// TRUST-MEM's acceptance on an injected machine: another holder's claim leaves
    /// aterm-render's recorded 15 GB peak (and its margin) no room, so its check waits at the
    /// gate, saying once a minute what it is short of and who holds it, and starts at the
    /// first look after that claim is let go. Negative control: while the claim is held it
    /// never starts.
    #[test]
    fn a_check_waits_at_the_gate_until_its_claim_fits_and_starts_at_the_next_look() {
        let render = Cost {
            cpu: 8318,
            wall: 7256,
            maxrss: 15_018_508_288,
            when: "2026-10-21T14:40:38Z".into(),
            prover: String::new(),
            cut: None,
            raised: false,
        };
        let need = claim_need(Some(&render), 48 * GIB);
        let mut check = job("aterm-render");
        check.need = need;
        let (_d, l) = ledger();
        let m = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
        let hold = |l: &aterm_capacity::Ledger| match aterm_capacity::Waiter::enter(
            l,
            &m,
            aterm_capacity::Need::new("hold").mem_gib(30),
        )
        .unwrap()
        .poll(&m)
        {
            aterm_capacity::Polled::Admitted(c) => c,
            aterm_capacity::Polled::Waiting(_, short) => panic!("the holder waits: {short}"),
            aterm_capacity::Polled::Gone(e) => panic!("the holder is gone: {e}"),
            aterm_capacity::Polled::Unqualified(why) => {
                panic!("load-only holder unqualified: {why}")
            }
        };
        let mut f = fake(&[("aterm-render", 5)]);
        f.gate = Some(gate(&l, &m, aterm_capacity::Admit::Wait));
        f.holder = Some((Duration::from_secs(150), hold(&l)));
        let (r, _) = slots(&mut f, vec![check.clone()], 1, 1000);
        assert_eq!(r, Ok(()));
        let w = f.w.borrow();
        let at = w.started_at[0];
        assert!(
            (Duration::from_secs(150)..=Duration::from_secs(152)).contains(&at),
            "started {at:?}, the claim let go at 150 s and the gate looks every 2 s"
        );
        let waits: Vec<&(Duration, String)> = w
            .said
            .iter()
            .filter(|(_, l)| l.contains(" waits "))
            .collect();
        assert_eq!(
            waits.iter().map(|(t, _)| t.as_secs()).collect::<Vec<_>>(),
            [0, 60, 120],
            "{:?}",
            w.said
        );
        let me = std::process::id().to_string();
        for (_, line) in &waits {
            for named in [
                "trust check aterm-render",
                "memory short by 6.8 GiB",
                "needs 16.8 GiB",
                "40 GiB available",
                me.as_str(),
                "\"hold\"",
            ] {
                assert!(line.contains(named), "{line:?} does not name {named:?}");
            }
        }
        assert!(
            w.said
                .last()
                .is_some_and(|(_, l)| l.contains("admitted after 2m30s")),
            "{:?}",
            w.said
        );
        drop(w);

        // Once the gate is no longer asked (the head waits for trustd, beside a check that runs),
        // an interrupt names no shortfall its last look found long before.
        let mut beside = fake(&[("aterm-render", 5)]);
        beside.gate = Some(gate(&l, &m, aterm_capacity::Admit::Wait));
        beside.holder = Some((Duration::MAX, hold(&l)));
        beside.held_after = 1;
        beside.trustd_until = Some(Duration::from_secs(100));
        beside.interrupt_at = Some(Duration::from_secs(400));
        let (r, _) = slots(&mut beside, vec![job("a"), check.clone()], 2, 1000);
        assert_eq!(r, Err(Stopped::Interrupted(None)));
        assert_eq!(beside.w.borrow().starts, ["a"]);
        assert!(
            beside
                .w
                .borrow()
                .said
                .iter()
                .any(|(_, l)| l.contains("trust check aterm-render waits")),
            "it waited at the gate before trustd let go"
        );
        drop(beside);

        let mut never = fake(&[("aterm-render", 5)]);
        never.gate = Some(gate(&l, &m, aterm_capacity::Admit::Wait));
        never.holder = Some((Duration::MAX, hold(&l)));
        never.interrupt_at = Some(Duration::from_secs(400));
        let (r, _) = slots(&mut never, vec![check], 1, 1000);
        // The run ended while the check waited, so it says so, naming the holder.
        let Err(Stopped::Interrupted(Some(waiting))) = r else {
            panic!("an interrupt at the gate names the wait: {r:?}");
        };
        for named in [
            "trust check aterm-render waited 6m",
            "at the launch gate",
            "memory short by",
            me.as_str(),
            "\"hold\"",
        ] {
            assert!(
                waiting.contains(named),
                "{waiting:?} does not name {named:?}"
            );
        }
        assert!(
            stopped(Stopped::Interrupted(Some(waiting)))
                .starts_with("interrupted while trust check")
        );
        let w = never.w.borrow();
        assert!(
            w.starts.is_empty(),
            "it started beside a claim with no room"
        );
        assert_eq!(
            w.said.iter().filter(|(_, l)| l.contains(" waits ")).count(),
            7,
            "one line a minute for 400 s"
        );
        drop(w);
        // The interrupted head's place leaves the queue with the gate, so only the holder's
        // claim is left.
        never.gate = None;
        assert_eq!(
            l.list()
                .unwrap()
                .iter()
                .map(|r| r.need.what.as_str())
                .collect::<Vec<_>>(),
            ["hold"]
        );
    }

    /// `--admit now` starts a check whose claim has no room beside a held claim at once, and says
    /// what it would have waited for.
    #[test]
    fn admit_now_starts_a_check_at_once_and_says_what_it_would_wait_for() {
        let (_d, l) = ledger();
        let m = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 20, 14));
        let _held = aterm_capacity::admit_with(
            &l,
            &m,
            aterm_capacity::Need::new("held").mem_gib(10),
            aterm_capacity::Admit::Wait,
            aterm_capacity::Pace::default(),
            &mut |_| {},
        )
        .expect("holder admitted");
        let mut f = fake(&[("big", 3)]);
        f.gate = Some(gate(&l, &m, aterm_capacity::Admit::Now));
        let (r, _) = slots(&mut f, vec![needing("big", 30)], 1, 100);
        assert_eq!(r, Ok(()));
        let w = f.w.borrow();
        assert_eq!(w.started_at, [Duration::ZERO]);
        assert!(
            w.said
                .iter()
                .any(|(_, l)| l.contains("--admit now") && l.contains("memory short by 20 GiB")),
            "{:?}",
            w.said
        );
    }

    /// `--admit now` takes a check room or not, but only in its turn: a look that meets another
    /// admission holding the ledger's lock decides nothing, and the gate looks again at the next
    /// pass instead of holding the slots still, so the check beside it runs on and ends while it
    /// waits. It starts holding its claim at the first pass after the lock is let go, having said
    /// once it waited a minute who held the lock. An interrupt while it waits for the lock ends
    /// the run there with nothing started, and its place leaves the queue with the gate.
    /// NEGATIVE CONTROL: the refusal at a held lock this replaced starts the check unclaimed at
    /// once, and a gate that waits for the lock inside its look never reaches the fake clock's
    /// next pass, which would let the lock go, and fails the one-minute hang detector.
    #[test]
    fn admit_now_takes_its_turn_at_the_gate_without_holding_the_slots() {
        /// What a gated run did: its end, each check's start, the gate's lines, the checks
        /// that ended and when, each check's cover, and the claims left once the gate is gone.
        type Ran = (
            Result<(), Stopped>,
            Vec<(String, Duration)>,
            Vec<(Duration, String)>,
            Vec<(String, Duration)>,
            Vec<Option<String>>,
            usize,
        );
        // The run on a thread of its own, within the one-minute hang detector.
        let within_a_minute = |release: Duration, interrupt: Option<Duration>| -> Ran {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let (_d, l) = ledger();
                let m = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
                let mut f = fake(&[("beside", 5), ("forced", 3)]);
                f.gate = Some(gate(&l, &m, aterm_capacity::Admit::Now));
                f.turn = Some((
                    release,
                    l.hold_turn().expect("the ledger's lock").expect("free"),
                ));
                f.interrupt_at = interrupt;
                let (r, done) = slots(&mut f, vec![job("beside"), needing("forced", 4)], 2, 100);
                f.gate = None;
                let w = f.w.borrow();
                let ran: Ran = (
                    r,
                    w.starts.iter().cloned().zip(w.started_at.clone()).collect(),
                    w.said.clone(),
                    done.iter().map(|d| (d.job.name.clone(), d.wall)).collect(),
                    w.covers.iter().map(|(_, c, _)| c.clone()).collect(),
                    l.list().expect("the ledger").len(),
                );
                let _ = tx.send(ran);
            });
            rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the gated run ended within the hang detector")
        };
        let lock = aterm_capacity::Short::Turn {
            holder: Some(std::process::id()),
        }
        .to_string();

        let (r, starts, said, done, covers, left) = within_a_minute(Duration::from_secs(90), None);
        assert_eq!(r, Ok(()));
        assert_eq!(
            starts,
            [
                ("beside".to_owned(), Duration::ZERO),
                ("forced".to_owned(), Duration::from_secs(90))
            ],
            "{said:?}"
        );
        assert!(
            done.contains(&("beside".to_owned(), Duration::from_secs(5))),
            "the check beside it ran on and ended while it waited: {done:?}"
        );
        assert!(covers[1].is_some(), "it holds its claim: {covers:?}");
        let waits: Vec<&String> = said
            .iter()
            .filter(|(_, l)| l.contains("trust check forced waits"))
            .map(|(_, l)| l)
            .collect();
        assert_eq!(
            waits.len(),
            1,
            "one line, once it waited a minute: {said:?}"
        );
        assert!(waits[0].contains(&lock), "{waits:?}");
        assert!(
            !said.iter().any(|(_, l)| l.contains("unclaimed")),
            "{said:?}"
        );
        assert_eq!(left, 0);

        let (r, starts, _, _, _, left) =
            within_a_minute(Duration::MAX, Some(Duration::from_secs(30)));
        assert_eq!(r, Err(Stopped::Interrupted(None)));
        assert_eq!(starts, [("beside".to_owned(), Duration::ZERO)]);
        assert_eq!(left, 0, "its place left the queue with the gate");
    }

    /// A claim of `gib` GiB another holder keeps in `l`, admitted at its one look.
    fn held_by_another(
        l: &aterm_capacity::Ledger,
        m: &aterm_capacity::Fixed,
        gib: u64,
    ) -> aterm_capacity::Claim {
        match aterm_capacity::Waiter::enter(l, m, aterm_capacity::Need::new("hold").mem_gib(gib))
            .expect("the holder entered")
            .poll(m)
        {
            aterm_capacity::Polled::Admitted(c) => c,
            other => panic!("the holder is not admitted: {other:?}"),
        }
    }

    /// The `kind`, `outcome`, `stop`, `sig` and `meter` of `row`.
    fn admission_words(
        row: &aterm_telemetry::Row,
    ) -> (
        aterm_telemetry::Kind,
        aterm_telemetry::Outcome,
        aterm_telemetry::Stop,
        Option<i32>,
        aterm_telemetry::Meter,
    ) {
        (row.kind.clone(), row.outcome, row.stop, row.sig, row.meter)
    }

    /// A CHECK'S ADMISSION AT THE GATE WRITES ONE ROW, WHERE IT IS DECIDED (TM-1 review): the
    /// gate drives a waiter of its own across passes of the slots, not `admit_with`, so its
    /// `kind=admission` row is written at the look that decides, however many looks waited.
    /// Three looks short of memory beside a held claim write none, and the look after that claim
    /// is let go writes one, admitted, with the wait its record's stamps say (the one its check's
    /// build rows read). Under `--admit now`, three looks that met another admission holding the
    /// ledger's lock write none, and the look in its turn writes one. NEGATIVE CONTROLS (by hand):
    /// the gate writing no row, as before: none at either decision; a row each look: three for
    /// the looks that waited.
    #[test]
    fn a_checks_admission_at_the_gate_writes_one_row_where_it_is_decided() {
        use aterm_capacity::telemetry::{capture, wait};
        use aterm_telemetry::{Kind, Meter, Outcome, Stop};
        let (_d, l) = ledger();
        let m = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
        let at = |n: u64| Duration::from_secs(2 * n);
        let admitted_row = (Kind::Admission, Outcome::Ok, Stop::Exit, None, Meter::Exact);

        let hold = held_by_another(&l, &m, 30);
        let mut g = gate(&l, &m, aterm_capacity::Admit::Wait);
        let check = needing("a", 20);
        let ((), rows) = capture(l.clone(), || {
            for n in 0..3 {
                assert!(
                    g.admit(&check, at(n), &mut |_| {}).is_none(),
                    "20 GiB beside 30 waits at look {n}"
                );
            }
        });
        assert!(rows.is_empty(), "no row a look: {rows:?}");
        drop(hold);
        let (admitted, rows) = capture(l.clone(), || g.admit(&check, at(3), &mut |_| {}));
        let admitted = admitted.expect("admitted at the look after the holder let go");
        let claim = admitted._claim.as_ref().expect("its claim");
        assert_eq!(rows.len(), 1, "one row however many looks: {rows:?}");
        assert_eq!(admission_words(&rows[0]), admitted_row);
        assert_eq!(
            rows[0].wait_ms,
            wait(claim.asked_ms(), claim.record().held.map(|h| h.admitted_ms)),
            "the wait its record's stamps say"
        );
        drop(admitted);
        let ((), rows) = capture(l.clone(), || drop(g));
        assert!(rows.is_empty(), "a gate with none waiting: {rows:?}");

        let mut g = gate(&l, &m, aterm_capacity::Admit::Now);
        let check = needing("b", 4);
        let turn = l.hold_turn().expect("the ledger's lock").expect("free");
        let ((), rows) = capture(l.clone(), || {
            for n in 0..3 {
                assert!(
                    g.admit(&check, at(n), &mut |_| {}).is_none(),
                    "not in its turn at look {n}"
                );
            }
        });
        assert!(rows.is_empty(), "no row a look: {rows:?}");
        drop(turn);
        let (admitted, rows) = capture(l.clone(), || g.admit(&check, at(3), &mut |_| {}));
        assert!(admitted.is_some(), "admitted in its turn");
        assert_eq!(rows.len(), 1, "one row however many looks: {rows:?}");
        assert_eq!(admission_words(&rows[0]), admitted_row);
        drop((admitted, g));
        assert!(l.list().expect("the ledger").is_empty());
    }

    /// A CHECK GIVEN UP WHILE IT WAITS AT THE GATE WRITES ONE ROW (TM-1 review): its admission
    /// was asked, so its row is owed however its wait ends. A run stopped by a signal while the
    /// check waits ends that wait, its row `outcome=fail` with the signal ([`Admission::stopped`]),
    /// and neither a second stop nor the gate's drop after writes another. A run interrupted
    /// through the slots writes one as they stop, with the backend's signal ([`Checks::given_up`]),
    /// and the gate's drop after none. A check the gate gave up for another writes one with no
    /// signal, and so does that other as the gate drops. A check whose
    /// claim file was removed while it waited (it starts unclaimed) writes one, its ledger's
    /// failure. NEGATIVE CONTROLS (by hand): a wait given up writing no row, as before: none for
    /// each but the last; the slots not giving the gate up as they stop: none until the gate's
    /// drop, and that one with no signal.
    #[cfg(unix)]
    #[test]
    fn a_check_given_up_at_the_gate_writes_one_row() {
        use aterm_capacity::telemetry::capture;
        use aterm_telemetry::{Kind, Meter, Outcome, Stop};
        let (_d, l) = ledger();
        let m = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
        let given_up = (
            Kind::Admission,
            Outcome::Fail,
            Stop::Exit,
            None,
            Meter::Stamped,
        );
        let waits = |g: &mut Admission<'_>, name: &str, n: u64| {
            assert!(
                g.admit(&needing(name, 20), Duration::from_secs(2 * n), &mut |_| {})
                    .is_none(),
                "{name}'s 20 GiB beside 30 waits"
            );
        };
        let hold = held_by_another(&l, &m, 30);

        let mut g = gate(&l, &m, aterm_capacity::Admit::Wait);
        let ((), rows) = capture(l.clone(), || {
            waits(&mut g, "a", 0);
            g.stopped(Some(15));
            g.stopped(Some(15));
            drop(g);
        });
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            admission_words(&rows[0]),
            (
                Kind::Admission,
                Outcome::Fail,
                Stop::Signal,
                Some(15),
                Meter::Stamped
            )
        );
        assert_eq!(l.list().expect("the ledger").len(), 1, "its wait left");

        let mut f = fake(&[("a", 5)]);
        f.gate = Some(gate(&l, &m, aterm_capacity::Admit::Wait));
        f.interrupt_at = Some(Duration::from_secs(30));
        let ((), rows) = capture(l.clone(), || {
            let (r, _) = slots(&mut f, vec![needing("a", 20)], 1, 100);
            assert!(matches!(r, Err(Stopped::Interrupted(Some(_)))), "{r:?}");
        });
        assert_eq!(
            rows.len(),
            1,
            "the slots gave it up as they stopped: {rows:?}"
        );
        assert_eq!(
            admission_words(&rows[0]),
            (
                Kind::Admission,
                Outcome::Fail,
                Stop::Signal,
                Some(INTERRUPTED_BY),
                Meter::Stamped
            )
        );
        let ((), rows) = capture(l.clone(), || f.gate = None);
        assert!(rows.is_empty(), "the gate's drop after: {rows:?}");

        let mut g = gate(&l, &m, aterm_capacity::Admit::Wait);
        let ((), rows) = capture(l.clone(), || {
            waits(&mut g, "a", 0);
            waits(&mut g, "b", 1);
        });
        assert_eq!(rows.len(), 1, "a, given up for b: {rows:?}");
        assert_eq!(admission_words(&rows[0]), given_up);
        let ((), rows) = capture(l.clone(), || drop(g));
        assert_eq!(rows.len(), 1, "b, as the gate drops: {rows:?}");
        assert_eq!(admission_words(&rows[0]), given_up);

        let mut g = gate(&l, &m, aterm_capacity::Admit::Wait);
        let ((), rows) = capture(l.clone(), || {
            waits(&mut g, "a", 0);
            for e in l.dir().read_dir().expect("the ledger").flatten() {
                if e.file_name().to_string_lossy().ends_with(".wait") {
                    fs::remove_file(e.path()).expect("a's file removed");
                }
            }
            assert!(
                g.admit(&needing("a", 20), Duration::from_secs(2), &mut |_| {})
                    .is_some_and(|a| a.cover().is_none()),
                "it starts unclaimed"
            );
            drop(g);
        });
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            admission_words(&rows[0]),
            (
                Kind::Admission,
                Outcome::Fail,
                Stop::Unknown,
                None,
                Meter::Stamped
            )
        );
        drop(hold);
        assert!(l.list().expect("the ledger").is_empty());
    }

    /// A check claims its recorded peak and a fifth more, at least 1 GiB more; with no peak
    /// recorded 16 GiB, or half the RAM where that is less. A claim is never cut to fit the
    /// RAM: one that is not below it is refused or, under `--try-refused`, starts at once.
    #[test]
    fn a_claim_is_the_recorded_peak_and_its_margin() {
        let cost = |maxrss| Cost {
            cpu: 1,
            wall: 1,
            maxrss,
            ..Cost::default()
        };
        assert_eq!(claim_need(None, 48 * GIB), 16 * GIB);
        assert_eq!(claim_need(Some(&cost(0)), 48 * GIB), 16 * GIB);
        assert_eq!(claim_need(None, 16 * GIB), 8 * GIB);
        assert_eq!(claim_need(None, 8 * GIB), 4 * GIB);
        assert_eq!(claim_need(None, 0), 16 * GIB);
        assert_eq!(claim_need(Some(&cost(10 * GIB)), 48 * GIB), 12 * GIB);
        assert_eq!(claim_need(Some(&cost(GIB)), 48 * GIB), 2 * GIB);
        assert_eq!(claim_need(Some(&cost(45 * GIB)), 48 * GIB), 54 * GIB);
        assert_eq!(claim_need(Some(&cost(45 * GIB)), 0), 54 * GIB);
    }

    /// On a 16 GiB host whose available memory is below its RAM, as every host's is: a
    /// library with no recorded peak claims half the RAM and starts at the first look, and a
    /// claim of all the RAM or more (one `--try-refused` lets past the plan) starts at once
    /// beside it, saying it can never be admitted here, and holds its claim while it runs.
    /// Negative control: a claim of all the RAM, the most a claim once took, waited for
    /// memory that is never available, and every check behind it waited too.
    #[test]
    fn a_claim_this_host_can_never_admit_starts_at_once_and_an_unmeasured_one_fits() {
        let (_d, l) = ledger();
        let m = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(16, 12, 8));
        let peaked = Cost {
            cpu: 1,
            wall: 1,
            maxrss: 15 * GIB,
            ..Cost::default()
        };
        for big in [claim_need(Some(&peaked), 16 * GIB), 16 * GIB] {
            let mut f = fake(&[("new", 3), ("huge", 3)]);
            f.gate = Some(gate(&l, &m, aterm_capacity::Admit::Wait));
            f.interrupt_at = Some(Duration::from_secs(300));
            let mut jobs = vec![job("new"), job("huge")];
            jobs[0].need = claim_need(None, 16 * GIB);
            jobs[1].need = big;
            let mut held = Vec::new();
            let r = run_slots(jobs, 2, Duration::from_secs(100), &mut f, &mut |fin, _| {
                held.push((fin.job.name.clone(), l.list().unwrap().len()));
            });
            assert_eq!(r, Ok(()), "{big}");
            let w = f.w.borrow();
            assert_eq!(w.starts, ["new", "huge"]);
            assert_eq!(w.started_at, [Duration::ZERO; 2], "{:?}", w.said);
            let never: Vec<&String> = w
                .said
                .iter()
                .map(|(_, l)| l)
                .filter(|l| l.contains("can never be admitted here"))
                .collect();
            assert_eq!(never.len(), 1, "{:?}", w.said);
            for named in ["trust check huge", "16 GiB of memory", "starts at once"] {
                assert!(
                    never[0].contains(named),
                    "{never:?} does not name {named:?}"
                );
            }
            assert!(
                !w.said.iter().any(|(_, l)| l.contains(" waits ")),
                "{:?}",
                w.said
            );
            assert_eq!(
                held.iter().map(|(_, n)| *n).max(),
                Some(2),
                "both claims held while both ran: {held:?}"
            );
            drop(w);
            assert!(l.list().unwrap().is_empty(), "every claim let go");
        }
    }

    /// NEVER-REACHABLE (`docs/DESIGN-adaptive-build-2026-10-24.md`): the gate is held to the
    /// plan's bound. A 44.2 GiB claim, which a plan that read 44.5 GiB could ever be available
    /// let through, meets a reading of 4 GiB wired (44 GiB at most available): it waits,
    /// saying so, and is admitted once wired falls back to 3.5 GiB and 44.4 GiB is available.
    /// A claim not below the bound, which only `--try-refused` lets past the plan, still starts
    /// at once. Negative control: judged by its own reading alone, the gate took the 44.2 GiB
    /// claim at once.
    #[test]
    fn a_claim_the_plan_admitted_waits_while_more_is_wired_than_the_plan_read() {
        let (_d, l) = ledger();
        let at = |wired: u64, available: u64| aterm_capacity::Reading {
            wired: Some(wired),
            available: Some(available),
            ..aterm_capacity::Reading::of_gib(48, 0, 14)
        };
        let m = aterm_capacity::Fixed::new(at(4 * GIB, 21 * GIB));
        let mut g = gate(&l, &m, aterm_capacity::Admit::Wait).held_to(48 * GIB - 7 * GIB / 2);
        let planned = Job {
            need: 442 * GIB / 10,
            ..job("alpha")
        };
        let mut said = Vec::new();
        let waits = g.admit(&planned, Duration::ZERO, &mut |s| said.push(s.to_string()));
        assert!(waits.is_none(), "{said:?}");
        assert_eq!(said.len(), 2, "the bound's line, then the wait's: {said:?}");
        for (line, named) in [
            (0, "capacity: trust check alpha: needs 44.2 GiB, this "),
            (0, "44 GiB of its 48 GiB"),
            (0, "4 GiB of it is wired"),
            (0, "44.5 GiB"),
            (0, "waits"),
            (1, "capacity: trust check alpha waits (0s): memory short by"),
        ] {
            assert!(
                said[line].contains(named),
                "{said:?} does not name {named:?}"
            );
        }
        m.set(at(7 * GIB / 2, 444 * GIB / 10));
        let admitted = g.admit(&planned, Duration::from_secs(2), &mut |s| {
            said.push(s.to_string())
        });
        assert!(admitted.is_some(), "admitted once wired fell: {said:?}");
        assert_eq!(l.list().unwrap().len(), 1, "its claim held");
        drop(admitted);
        assert!(l.list().unwrap().is_empty(), "its claim let go");
        let forced = Job {
            need: 45 * GIB,
            ..job("beta")
        };
        said.clear();
        let at_once = g.admit(&forced, Duration::from_secs(4), &mut |s| {
            said.push(s.to_string())
        });
        assert!(at_once.is_some(), "{said:?}");
        assert!(
            said.iter()
                .any(|s| s.contains("it can never be admitted here, so it starts at once")),
            "{said:?}"
        );
    }

    #[test]
    fn a_check_over_its_budget_is_ended_and_marked() {
        let mut f = fake(&[("quick", 2)]);
        let jobs = vec![job("stuck"), job("quick")];
        let (r, done) = slots(&mut f, jobs, 2, 10);
        assert_eq!(r, Ok(()));
        let stuck = done.iter().find(|d| d.job.name == "stuck").unwrap();
        assert!(stuck.over_budget);
        assert_eq!(stuck.ended.code, 137);
        assert!(stuck.wall >= Duration::from_secs(10));
        let quick = done.iter().find(|d| d.job.name == "quick").unwrap();
        assert!(!quick.over_budget);
        assert_eq!(f.w.borrow().ended_by_lane, ["stuck"]);
    }

    #[test]
    fn a_second_check_waits_for_this_builds_trustd() {
        let mut f = fake(&[("a", 3), ("b", 3), ("c", 3)]);
        f.held_after = usize::MAX;
        let jobs = ["a", "b", "c"].map(job).to_vec();
        slots(&mut f, jobs.clone(), 3, 100).0.unwrap();
        assert_eq!(f.w.borrow().peak, 1, "never two while trustd is not held");
        let mut g = fake(&[("a", 3), ("b", 3), ("c", 3)]);
        g.held_after = 1;
        slots(&mut g, jobs, 3, 100).0.unwrap();
        assert_eq!(g.w.borrow().peak, 3);
    }

    #[test]
    fn an_interrupt_ends_every_running_check() {
        let mut f = fake(&[]);
        f.interrupt_at = Some(Duration::from_secs(3));
        let jobs = ["a", "b", "c"].map(job).to_vec();
        let (r, done) = slots(&mut f, jobs, 2, 100);
        assert_eq!(r, Err(Stopped::Interrupted(None)));
        let mut ended: Vec<_> = done
            .iter()
            .map(|d| {
                (
                    d.job.name.as_str(),
                    d.interrupted,
                    d.over_budget,
                    d.ended.code,
                )
            })
            .collect();
        ended.sort_unstable();
        assert_eq!(
            ended,
            [("a", true, false, 137), ("b", true, false, 137)],
            "each check the interrupt ended is handed on once, marked, for what it measured"
        );
        let w = f.w.borrow();
        assert_eq!(w.ended_by_lane, ["a", "b"]);
        assert_eq!(w.live, 0);
        assert_eq!(w.starts, ["a", "b"], "nothing starts after it");
    }

    #[test]
    fn an_unstartable_check_stops_the_run() {
        let mut f = fake(&[]);
        f.unstartable = Some("b".into());
        let jobs = ["a", "b"].map(job).to_vec();
        let (r, done) = slots(&mut f, jobs, 2, 100);
        assert!(matches!(r, Err(Stopped::Unstartable(why)) if why.contains("no targo")));
        assert_eq!(f.w.borrow().ended_by_lane, ["a"]);
        assert_eq!(
            done.iter()
                .map(|d| (d.job.name.as_str(), d.interrupted))
                .collect::<Vec<_>>(),
            [("a", true)]
        );
    }

    /// A slot held before the run, or one the lane just ended a check in, takes no check; one
    /// a check left by its own exit is used again.
    #[test]
    fn no_check_starts_in_a_held_slot() {
        let mut f = fake(&[("a", 2), ("b", 2)]);
        f.w.borrow_mut().held.insert(0);
        let jobs = vec![job("stuck"), job("a"), job("b")];
        let (r, done) = slots(&mut f, jobs, 1, 3);
        assert_eq!(r, Ok(()));
        assert_eq!(done.len(), 3);
        let w = f.w.borrow();
        assert_eq!(w.starts, ["stuck", "a", "b"]);
        assert_eq!(w.ended_by_lane, ["stuck"]);
        assert_eq!(w.slots, [1, 2, 2]);
    }

    fn finished(dir: &Path, log_text: &str, code: i32, over: bool) -> Finished {
        let log = dir.join("alpha@1.0.0.log");
        fs::write(&log, log_text).unwrap();
        let mut j = job("alpha");
        j.log = log;
        j.target = dir.join("j0");
        Finished {
            job: j,
            ended: Ended {
                code,
                cpu: Duration::from_secs(4),
                maxrss: 9,
                threads: None,
            },
            wall: Duration::from_secs(5),
            over_budget: over,
            interrupted: false,
            load: Some(30.0),
            trustd_lost: false,
        }
    }

    /// F9: a check's cost row keeps what it measured whether or not it finished, under the
    /// prover that ran it: a finished check's is what it took, and one ended at its budget, by
    /// the run's stop, or by itself without a whole summary or with its transport to trustd
    /// failed says how it was cut short. NEGATIVE CONTROL: a cut check whose peak was not read
    /// keeps nothing, where a finished one with none still keeps its times.
    #[test]
    fn a_check_cut_short_keeps_what_it_measured() {
        let d = tmp("trust-lane-cost-of");
        let driver = Driver {
            bin: PathBuf::new(),
            targo: PathBuf::new(),
            id: P.into(),
        };
        let cut = |f: &Finished| cost_of(f, &driver).map(|c| c.cut);
        let done = finished(d.path(), DONE, 0, false);
        let c = cost_of(&done, &driver).unwrap();
        assert_eq!((c.cpu, c.wall, c.maxrss, c.cut), (4, 5, 9, None));
        assert_eq!(c.prover, P);
        assert_eq!(
            cut(&finished(d.path(), DONE, 137, true)),
            Some(Some(Cut::OverBudget))
        );
        let mut stopped = finished(d.path(), "", 137, false);
        stopped.interrupted = true;
        assert_eq!(cut(&stopped), Some(Some(Cut::Interrupted)));
        for log in [
            "error: could not compile `alpha`\n",
            "Summary: 5 proved, 0 failed (20 total); reproducible\n",
        ] {
            assert_eq!(
                cut(&finished(d.path(), log, 101, false)),
                Some(Some(Cut::Failed)),
                "{log}"
            );
        }
        let mut unread = finished(d.path(), "", 137, true);
        unread.ended.maxrss = 0;
        assert_eq!(cut(&unread), None, "nothing of its memory was read");
        let mut quiet = finished(d.path(), DONE, 0, false);
        quiet.ended.maxrss = 0;
        assert_eq!(cut(&quiet), Some(None));
    }

    const DONE: &str = "Summary: 5 proved, 0 failed (20 total); reproducible\nCoverage: 3/3 eligible function bodies verified (complete)\n";

    /// A check the trustd went away under leaves a `.trustd-lost` mark naming what serves the
    /// socket now, and the judge reads it into the check's facts. NEGATIVE CONTROL: a check it
    /// stayed under leaves none, and probes nothing for one.
    #[test]
    fn a_check_the_trustd_went_away_under_leaves_what_serves_the_socket() {
        for lost in [true, false] {
            let d = tmp("trust-lane-lost");
            let mut f = finished(
                d.path(),
                "error: Broken pipe (os error 32)\nerror: could not compile `alpha`\n",
                101,
                false,
            );
            f.trustd_lost = lost;
            let mut probed = 0;
            record(&f, Duration::from_secs(60), 14, &mut || {
                probed += 1;
                "none\tnothing serves /tmp/t.sock".into()
            })
            .unwrap();
            let mark = d.path().join("alpha@1.0.0.log.trustd-lost");
            if lost {
                assert_eq!(probed, 1);
                assert_eq!(
                    fs::read_to_string(&mark).unwrap(),
                    "nothing serves /tmp/t.sock\n"
                );
            } else {
                assert_eq!(probed, 0);
                assert!(!mark.exists());
            }
        }
    }

    #[test]
    fn a_check_leaves_its_status_and_only_its_own_marks() {
        let d = tmp("trust-lane-record");
        let f = finished(d.path(), DONE, 0, false);
        let mut probed = 0;
        record(&f, Duration::from_secs(60), 14, &mut || {
            probed += 1;
            "ours\tx".into()
        })
        .unwrap();
        let beside = |ext: &str| d.path().join(format!("alpha@1.0.0.log.{ext}"));
        assert_eq!(fs::read_to_string(beside("rc")).unwrap(), "0\n");
        let pace = fs::read_to_string(beside("pace")).unwrap();
        assert_eq!(pace, "4000 5000 3000 14\n");
        let pace = trust_facts::Pace::parse(pace.trim()).unwrap();
        assert!(pace.starved(), "load 30 on 14 cores: {}", pace.said());
        assert!(!beside("budget").exists());
        assert!(!beside("trustd").exists());
        assert_eq!(
            probed, 0,
            "the socket is asked only about a check targo refused"
        );
        let refused = format!("error: {}\n", trust_facts::TRUSTD_REFUSED);
        let f = finished(d.path(), &refused, 137, true);
        record(&f, Duration::from_secs(60), 14, &mut || {
            "foreign\tanother build".into()
        })
        .unwrap();
        assert_eq!(fs::read_to_string(beside("budget")).unwrap(), "60\n");
        assert_eq!(
            fs::read_to_string(beside("trustd")).unwrap(),
            "foreign\tanother build\n"
        );
        let mut f = finished(d.path(), DONE, 0, false);
        f.ended.threads = Some(trust_facts::Threads {
            ran_ms: 3600,
            runnable_ms: 4000,
            cores: None,
        });
        record(&f, Duration::from_secs(60), 14, &mut || String::new()).unwrap();
        let pace = fs::read_to_string(beside("pace")).unwrap();
        assert_eq!(pace, "4000 5000 3000 14 3600 4000\n");
        let pace = trust_facts::Pace::parse(pace.trim()).unwrap();
        assert!(!pace.starved(), "its threads ran: {}", pace.said());
        // The same threads, a third of their time on efficiency cores at half the speed.
        f.ended.threads = Some(trust_facts::Threads {
            ran_ms: 3600,
            runnable_ms: 4000,
            cores: Some(trust_facts::Cores {
                p_ms: 2400,
                instructions: 3_000_000,
                p_instructions: 2_400_000,
            }),
        });
        record(&f, Duration::from_secs(60), 14, &mut || String::new()).unwrap();
        let pace = fs::read_to_string(beside("pace")).unwrap();
        assert_eq!(pace, "4000 5000 3000 14 3600 4000 2400 3000000 2400000\n");
        let pace = trust_facts::Pace::parse(pace.trim()).unwrap();
        assert!(
            pace.starved(),
            "0.75 at a performance core's speed: {}",
            pace.said()
        );
    }

    /// A check's times are its own processes', each at its latest read, and its peak the largest
    /// footprint one of them had.
    #[test]
    fn a_checks_times_sum_its_own_processes_latest_reads() {
        use aterm_sysprobe::procs::ProcessReading;
        let p = |pid, ppid, start, ran_ms: u64, runnable_ms: Option<u64>| ProcessReading {
            pid,
            ppid,
            start: Some(start),
            cpu_ns: Some(ran_ms * 1_000_000),
            runnable_ns: runnable_ms.map(|r| r * 1_000_000),
            ..ProcessReading::default()
        };
        let sums = |t: &TreeTimes| t.threads().map(|t| (t.ran_ms, t.runnable_ms));
        let mut t = TreeTimes::default();
        assert_eq!(sums(&t), None);
        assert_eq!(t.threads().and_then(|t| t.cores), None);
        // The root 10, its child 11 and grandchild 12; a stranger 20 and its child 21; pid 0,
        // its own parent.
        let stranger = [
            p(20, 1, 200, 7000, Some(7000)),
            p(21, 20, 210, 7000, Some(7000)),
        ];
        let first = [
            p(0, 0, 1, 9, Some(9)),
            p(10, 1, 100, 1, Some(1)),
            p(11, 10, 110, 50, Some(60)),
            p(12, 11, 120, 400, Some(900)),
        ];
        t.take(10, &[&first[..], &stranger[..]].concat());
        assert_eq!(
            sums(&t),
            Some((451, 961)),
            "the stranger's are not the check's"
        );
        // 12 ended and keeps what it last showed; 11 counts once, at its latest read.
        t.take(
            10,
            &[p(10, 1, 100, 2, Some(2)), p(11, 10, 110, 80, Some(100))],
        );
        assert_eq!(sums(&t), Some((482, 1002)));
        // A pid used again under the root by another process is that other process; one read
        // without its runnable time is left out.
        t.take(
            10,
            &[
                p(10, 1, 100, 2, Some(2)),
                p(12, 10, 130, 5, Some(5)),
                p(13, 10, 140, 900, None),
            ],
        );
        assert_eq!(sums(&t), Some((487, 1007)));
        let mut none = TreeTimes::default();
        none.take(10, &stranger);
        assert_eq!(sums(&none), None, "no root read, nothing taken");
        assert_eq!(none.peak(), 0);

        // ATERM-GUI-MEM: the largest footprint any of the tree's processes had, read with or
        // without its times, and never the stranger's.
        let peaked = |mut r: ProcessReading, peak| {
            r.peak_footprint = Some(peak);
            r
        };
        let mut m = TreeTimes::default();
        m.take(
            10,
            &[
                peaked(p(10, 1, 100, 1, Some(1)), 3),
                peaked(p(11, 10, 110, 1, None), 40),
                peaked(p(20, 1, 200, 1, Some(1)), 900),
            ],
        );
        assert_eq!(m.peak(), 40);
        m.take(10, &[peaked(p(10, 1, 100, 1, Some(1)), 7)]);
        assert_eq!(m.peak(), 40, "one that ended keeps the largest it showed");
    }

    /// FONT-ECORE-SHARE: where a check's processes ran is summed with their times, its
    /// performance cores' time in milliseconds; a process read without it (a host with one kind
    /// of core, or a read that had none) leaves the sum without it, so no share is weighed on
    /// part of the tree.
    #[test]
    fn a_checks_core_split_is_summed_only_where_every_process_has_one() {
        use aterm_sysprobe::{CoreSplit, procs::ProcessReading};
        let p = |pid, ppid, ran_ms: u64, cores: Option<(u64, u64, u64)>| ProcessReading {
            pid,
            ppid,
            start: Some(u64::from(pid)),
            cpu_ns: Some(ran_ms * 1_000_000),
            runnable_ns: Some(ran_ms * 2_000_000),
            cores: cores.map(|(p_ms, instructions, p_instructions)| CoreSplit {
                p_cpu_ns: p_ms * 1_000_000,
                instructions,
                p_instructions,
            }),
            ..ProcessReading::default()
        };
        let mut t = TreeTimes::default();
        t.take(
            10,
            &[
                p(10, 1, 100, Some((100, 500, 500))),
                p(11, 10, 300, Some((200, 900, 700))),
            ],
        );
        let both = t.threads().unwrap();
        assert_eq!((both.ran_ms, both.runnable_ms), (400, 800));
        assert_eq!(
            both.cores,
            Some(trust_facts::Cores {
                p_ms: 300,
                instructions: 1400,
                p_instructions: 1200,
            })
        );
        assert_eq!(
            both.worked_ms(),
            350,
            "300 ms on P, 100 ms on E at half the rate"
        );
        t.take(
            10,
            &[p(10, 1, 100, Some((100, 500, 500))), p(12, 10, 50, None)],
        );
        let mixed = t.threads().unwrap();
        assert_eq!(mixed.ran_ms, 450);
        assert_eq!(mixed.cores, None, "one process read without a split");
        assert_eq!(mixed.worked_ms(), 450);
    }

    /// A running check's times come off the live process table: its root's and what the root
    /// started, no other process's. Negative control: a check that reads no table has none.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_running_checks_times_are_its_own_tree_s_in_the_live_table() {
        let d = tmp("trust-lane-times");
        let pidfile = d.path().join("pid");
        let mut cmd = command("/bin/sh");
        cmd.arg("-c")
            .arg("\"$2\" 600 & echo $! > \"$1\"; wait")
            .arg("sh")
            .arg(&pidfile)
            .arg(readable("/bin/sleep"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let child = cmd.spawn().unwrap();
        let root = child.id();
        let deadline = Instant::now() + Duration::from_secs(60);
        let under = loop {
            if let Some(p) = fs::read_to_string(&pidfile)
                .ok()
                .and_then(|t| t.trim().parse().ok())
            {
                break Leftover::new(p);
            }
            assert!(
                Instant::now() < deadline,
                "the root never started its child"
            );
            aterm_uds::deadline::poll_yield();
        };
        assert!(
            runs_from(under.pid, &readable("/bin/sleep")),
            "the root's child never ran its program"
        );
        let mut check = RealCheck {
            child,
            record: d.path().join(".check.0"),
            target: d.path().join("j0"),
            mark: proc_tree::Mark {
                nonce: None,
                root: None,
                kept: BTreeSet::new(),
            },
            ended: None,
            times: TreeTimes::default(),
            read_at: None,
            sweep: || None,
        };
        check.read_times(true);
        assert_eq!(check.times.threads(), None, "no table read, no times");
        check.sweep = || aterm_sysprobe::procs::processes().map(|s| s.procs);
        check.read_times(true);
        let read: BTreeSet<u32> = check.times.seen.keys().map(|(pid, _)| *pid).collect();
        assert_eq!(read, BTreeSet::from([root, under.pid]), "{:?}", check.times);
        check.child.kill().unwrap();
        let _ = check.child.wait();
    }

    /// ATERM-GUI-MEM: a check's recorded peak ([`Ended::maxrss`]) is the larger of the resident
    /// high-water mark its reap reports and the largest footprint a read of its tree showed
    /// ([`TreeTimes::peak`]), whether it ends by itself (`poll`) or the lane ends it (`end`):
    /// under memory pressure the kernel compresses pages out of the resident set, so the first
    /// alone can be far below what the check needed (aterm-gui's 29.5 GB resident against a
    /// 47.2 GB footprint). NEGATIVE CONTROL: with no footprint read, its peak is the resident
    /// mark alone, far below the footprint the other arm was read with.
    #[test]
    fn a_checks_peak_counts_its_trees_largest_footprint() {
        const FOOTPRINT: u64 = 1 << 40;
        for by_itself in [true, false] {
            for read in [true, false] {
                let d = tmp("trust-lane-peak");
                let nonce = nonce();
                let mut cmd = command("/bin/sh");
                cmd.arg("-c")
                    .arg("read -r go")
                    .env(CHECK_VAR, &nonce)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                proc_tree::new_session(&mut cmd);
                let child = cmd.spawn().unwrap();
                let root = child.id();
                let target = d.path().join("j0");
                owner_dirs(&target).unwrap();
                let record = d.path().join(".check.0");
                fs::write(&record, "the lane's own\n").unwrap();
                let mut check = RealCheck {
                    child,
                    record,
                    target,
                    mark: proc_tree::Mark {
                        nonce: Some(nonce),
                        root: Some((i32::try_from(root).unwrap(), serial_of(root))),
                        kept: BTreeSet::new(),
                    },
                    ended: None,
                    times: TreeTimes::default(),
                    read_at: None,
                    sweep: || None,
                };
                if read {
                    check.times.take(
                        root,
                        &[aterm_sysprobe::procs::ProcessReading {
                            pid: root,
                            ppid: 1,
                            peak_footprint: Some(FOOTPRINT),
                            ..aterm_sysprobe::procs::ProcessReading::default()
                        }],
                    );
                }
                let ended = if by_itself {
                    open(&mut check.child).unwrap();
                    let deadline = Instant::now() + Duration::from_secs(60);
                    loop {
                        if let Some(ended) = check.poll().unwrap() {
                            break ended;
                        }
                        assert!(Instant::now() < deadline, "the root never ended");
                        aterm_uds::deadline::poll_yield();
                    }
                } else {
                    check.end()
                };
                let how = if by_itself { "poll" } else { "end" };
                if read {
                    assert_eq!(ended.maxrss, FOOTPRINT, "{how}");
                } else {
                    assert!(
                        ended.maxrss > 0 && ended.maxrss < FOOTPRINT,
                        "{how}: {}",
                        ended.maxrss
                    );
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    type ModeT = u16;
    #[cfg(not(target_os = "macos"))]
    type ModeT = u32;

    unsafe extern "C" {
        fn umask(mask: ModeT) -> ModeT;
    }

    const UMASK_PROBE_ENV: &str = "ATERM_RELEASE_TRUST_LANE_UMASK_DIR";
    const UMASK_PROBE: &str =
        "trust_lane::tests::the_lanes_directories_are_admitted_under_umask_002";

    fn probe_the_lanes_directories_under_umask_002_if_asked() -> bool {
        let Some(dir) = std::env::var_os(UMASK_PROBE_ENV) else {
            return false;
        };
        // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail in this
        // single-test process.
        unsafe { umask(0o002) };
        let slot = PathBuf::from(dir).join("target-trust.noindex/j0");
        owner_dirs(&slot).unwrap();
        assert_eq!(refused_component(&slot), None, "{}", slot.display());
        true
    }

    #[test]
    fn the_lanes_directories_are_admitted_under_umask_002() {
        if probe_the_lanes_directories_under_umask_002_if_asked() {
            return;
        }
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
            record(&f, Duration::from_secs(60), 14, &mut String::new).unwrap();
            assert_eq!(path.exists(), !created, "created {created}");
        }
        assert!(!fork_lock(d.path(), &argv).unwrap().created);
    }

    struct Quick<'a> {
        root: PathBuf,
        log: &'static str,
        interrupt: bool,
        /// Each check runs until the run is interrupted at this moment ([`Hung`]).
        stop_at: Option<Duration>,
        now: Duration,
        started: &'a mut Vec<String>,
        stamp: Option<SystemTime>,
        load: Option<f64>,
        /// The launch gate's memory admission; none admits every check unclaimed.
        gate: Option<Admission<'a>>,
        /// What the gate said.
        said: Vec<String>,
    }

    /// How long a gated run may go on the fake clock before it counts as interrupted: a
    /// check the gate must admit at once is admitted well inside it, so one that waits ends
    /// the run instead of hanging the test.
    const GATE_HANG: Duration = Duration::from_secs(60);

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
                threads: None,
            }
        }
    }

    /// A check that runs until the lane ends it, having reached a 5 GiB peak.
    struct Hung;

    impl Check for Hung {
        fn poll(&mut self) -> std::io::Result<Option<Ended>> {
            Ok(None)
        }
        fn end(&mut self) -> Ended {
            Ended {
                code: 137,
                cpu: Duration::from_secs(2),
                maxrss: 5 * GIB,
                threads: None,
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
            if self.stop_at.is_some() {
                return Ok(Box::new(Hung));
            }
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
                || self.stop_at.is_some_and(|t| self.now >= t)
                || (self.gate.is_some() && self.now >= GATE_HANG)
        }
        fn now(&self) -> Duration {
            self.now
        }
        fn pause(&mut self) {
            self.now += Duration::from_secs(1);
        }
        fn held(&self, slot: usize) -> bool {
            slot_held(&self.root, slot)
        }
        fn load(&self) -> Option<f64> {
            self.load
        }
        fn admit(&mut self, job: &Job) -> Option<Admitted> {
            let Some(gate) = &mut self.gate else {
                return Some(Admitted::unclaimed());
            };
            let said = &mut self.said;
            gate.admit(job, self.now, &mut |line| said.push(line.to_string()))
        }
        fn given_up(&mut self) {
            if let Some(gate) = &mut self.gate {
                gate.stopped(Some(INTERRUPTED_BY));
            }
        }
    }

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
        /// Each check runs until the run is interrupted at this moment ([`Quick::stop_at`]).
        stop_at: Option<Duration>,
        meanwhile: Option<fn(&Path)>,
        /// The stamp each check's dep-info gets ([`Quick`]).
        stamp: Option<SystemTime>,
        /// The host's load while the checks run ([`Checks::load`]).
        load: Option<f64>,
        /// The prover a second resolution finds (a toolchain updated meanwhile); `driver` when none.
        updated: Option<Driver>,
        /// How often the run resolved its prover.
        drivers_asked: usize,
        /// The host's RAM, and whether it was read as `MemTotal`.
        mem: (u64, bool),
        /// The most memory that can ever be available on the host ([`Bench::reach`]).
        reach: u64,
        /// What the prover's `trustc -Z help` says of the memory ceiling flag.
        ceiling: Result<bool, String>,
        /// How often the run read `trustc -Z help`.
        ceiling_probes: usize,
        /// The launch gate's machine and ledger; none admits every check unclaimed.
        gate: Option<(aterm_capacity::Fixed, aterm_capacity::Ledger)>,
    }

    impl Bench for Site {
        fn driver(&mut self, _: &Config) -> Result<Driver, String> {
            self.drivers_asked += 1;
            Ok(match &self.updated {
                Some(now) if self.drivers_asked > 1 => now.clone(),
                _ => self.driver.clone(),
            })
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
        fn memory(&self) -> (u64, bool) {
            self.mem
        }
        fn reach(&self) -> u64 {
            self.reach
        }
        fn load(&self) -> Option<f64> {
            self.load
        }
        fn takes_ceiling(&mut self, _: &Driver) -> Result<bool, String> {
            self.ceiling_probes += 1;
            self.ceiling.clone()
        }
        /// The socket is usable: a test run never reads the user's trustd ([`Claims`] stands in
        /// for a wait that ends without it).
        fn trustd(&mut self, _: &Config, _: &Prepared, _: &mut dyn Write) -> Result<(), String> {
            Ok(())
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
            let gate = self.gate.as_ref().map(|(machine, ledger)| {
                Admission::new(
                    Some(ledger.clone()),
                    machine,
                    cfg.admit,
                    aterm_capacity::Pace {
                        poll: Duration::from_secs(2),
                        line_every: Duration::from_secs(60),
                    },
                )
                .held_to(prep.bound)
            });
            let mut quick = Quick {
                root: prep.root.clone(),
                log: self.log,
                interrupt: self.interrupt,
                stop_at: self.stop_at,
                now: Duration::ZERO,
                started: &mut self.started,
                stamp: self.stamp,
                load: self.load,
                gate,
                said: Vec::new(),
            };
            let ran = run_slots(jobs, slots, cfg.budget, &mut quick, done);
            for line in std::mem::take(&mut quick.said) {
                let _ = writeln!(out, "trust: {line}");
            }
            if let Some(edit) = self.meanwhile.take() {
                edit(&self.repo);
            }
            ran.map_err(stopped)
        }
        /// No claim: a test run never reads the live volume or writes the user's ledger
        /// ([`Claims`] stands in for the ledger's answers).
        fn disk(
            &mut self,
            _: atpkg::admit_disk::Run,
            _: atpkg::admit_disk::Admit,
            _: &mut dyn Write,
        ) -> Result<Option<atpkg::admit_disk::Admitted>, atpkg::admit_disk::Error> {
            Ok(None)
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

    fn slots_base() -> PathBuf {
        let mut refused = Vec::new();
        for base in [
            Some(aterm_tempfile::target_tmpdir()),
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
            stop_at: None,
            meanwhile: None,
            stamp: None,
            load: None,
            updated: None,
            drivers_asked: 0,
            mem: (48 * GIB, false),
            reach: 0,
            ceiling: Ok(false),
            ceiling_probes: 0,
            gate: None,
        }
    }

    fn root_of(s: &Site) -> PathBuf {
        s.cfg.target_root.clone().unwrap()
    }

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

    /// A [`Site`] whose disk claim answers as the capacity ledger would, refused (`never`),
    /// ended by an interrupt (`interrupt`) or admitted with no claim, keeping each run it was
    /// asked to claim with how many checks had started by then; and whose wait for trustd's
    /// socket ends without it when `trustd` says why, keeping how many disk claims had been
    /// asked for each time the run waited (`trustd_at`).
    struct Claims<'s> {
        site: &'s mut Site,
        never: Option<String>,
        interrupt: Option<i32>,
        asked: Vec<(atpkg::admit_disk::Run, atpkg::admit_disk::Admit, usize)>,
        trustd: Option<String>,
        trustd_at: Vec<usize>,
    }

    impl Bench for Claims<'_> {
        fn driver(&mut self, cfg: &Config) -> Result<Driver, String> {
            self.site.driver(cfg)
        }
        fn metadata(&mut self, driver: &Driver, repo: &Path) -> Result<Vec<u8>, String> {
            self.site.metadata(driver, repo)
        }
        fn cargo_home(&self) -> String {
            self.site.cargo_home()
        }
        fn cache(&self) -> Option<PathBuf> {
            self.site.cache()
        }
        fn memory(&self) -> (u64, bool) {
            self.site.memory()
        }
        fn reach(&self) -> u64 {
            self.site.reach()
        }
        fn load(&self) -> Option<f64> {
            self.site.load()
        }
        fn takes_ceiling(&mut self, driver: &Driver) -> Result<bool, String> {
            self.site.takes_ceiling(driver)
        }
        fn trustd(
            &mut self,
            cfg: &Config,
            prep: &Prepared,
            out: &mut dyn Write,
        ) -> Result<(), String> {
            self.trustd_at.push(self.asked.len());
            match &self.trustd {
                Some(why) => Err(why.clone()),
                None => self.site.trustd(cfg, prep, out),
            }
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
            self.site.checks(cfg, prep, jobs, slots, done, out)
        }
        fn disk(
            &mut self,
            run: atpkg::admit_disk::Run,
            mode: atpkg::admit_disk::Admit,
            _: &mut dyn Write,
        ) -> Result<Option<atpkg::admit_disk::Admitted>, atpkg::admit_disk::Error> {
            self.asked.push((run, mode, self.site.started.len()));
            if let Some(sig) = self.interrupt {
                return Err(atpkg::admit_disk::Error::Stopped {
                    signal: Some(sig),
                    short: None,
                });
            }
            match &self.never {
                Some(why) => Err(atpkg::admit_disk::Error::Never(why.clone())),
                None => Ok(None),
            }
        }
    }

    /// A run claims its target root's disk once it holds the root and before any check starts:
    /// the cold budget on the root's volume, kept under its `logs`. Negative control: a need the
    /// volume can never hold is COULD NOT RUN naming the fix, and no check starts; so is a wait
    /// an interrupt ended, naming the signal.
    #[test]
    fn a_run_claims_its_roots_disk_before_any_check() {
        let mut s = site(Scope::Packages(vec!["alpha".into()]));
        let cfg = s.cfg.clone();
        let mut claims = Claims {
            site: &mut s,
            never: None,
            interrupt: None,
            asked: Vec::new(),
            trustd: None,
            trustd_at: Vec::new(),
        };
        let mut out = Vec::new();
        let outcome = run_on(&cfg, &mut claims, &mut out);
        let said = String::from_utf8(out).unwrap();
        assert_eq!(outcome, Outcome::Pass, "{said}");
        let [(run, mode, started)] = &claims.asked[..] else {
            panic!("one claim: {:?}", claims.asked);
        };
        assert_eq!(*started, 0, "the claim comes before the checks");
        assert_eq!(
            *mode,
            atpkg::admit_disk::Admit::Wait,
            "it waits unless told"
        );
        let root = &run.dirs[0];
        assert_eq!(
            run,
            &atpkg::admit_disk::Run {
                what: "ship trust run -p alpha".into(),
                scope: "-p alpha".into(),
                dirs: vec![root.clone()],
                record: root.join("logs").join(DISK_RECORD),
                cold: COLD_DISK,
                on: root.clone(),
                stuck_ends: None,
                scratch: false,
                machine: None,
            }
        );
        assert!(root.join("logs").is_dir(), "{}", root.display());
        assert!(!claims.site.started.is_empty(), "{said}");

        let mut s = site(Scope::Packages(vec!["alpha".into()]));
        let cfg = s.cfg.clone();
        let mut claims = Claims {
            site: &mut s,
            never: Some(
                "ship trust run -p alpha: needs 6 GiB on /Volumes/s, which holds 2 GiB above its \
                 floor"
                    .into(),
            ),
            interrupt: None,
            asked: Vec::new(),
            trustd: None,
            trustd_at: Vec::new(),
        };
        let mut out = Vec::new();
        let outcome = run_on(&cfg, &mut claims, &mut out);
        let said = String::from_utf8(out).unwrap();
        assert!(
            matches!(&outcome, Outcome::CouldNotRun(why)
                if why.contains("needs 6 GiB on /Volumes/s") && why.contains("--target-root")),
            "{outcome:?}: {said}"
        );
        assert!(claims.site.started.is_empty(), "{said}");

        let mut s = site(Scope::Packages(vec!["alpha".into()]));
        let cfg = s.cfg.clone();
        let mut claims = Claims {
            site: &mut s,
            never: None,
            interrupt: Some(15),
            asked: Vec::new(),
            trustd: None,
            trustd_at: Vec::new(),
        };
        let mut out = Vec::new();
        let outcome = run_on(&cfg, &mut claims, &mut out);
        let said = String::from_utf8(out).unwrap();
        assert!(
            matches!(&outcome, Outcome::CouldNotRun(why)
                if why.contains("disk claim was interrupted (SIGTERM)")
                    && why.contains("nothing was decided")),
            "{outcome:?}: {said}"
        );
        assert!(claims.site.started.is_empty(), "{said}");
    }

    /// A run with checks to make waits for trustd's socket before it claims its disk, so a
    /// wait with no deadline (another build's trustd, for hours) holds no claim other builds
    /// wait behind; a run with none does not wait. NEGATIVE CONTROL: a wait that ends without
    /// the socket is COULD NOT RUN with no disk claimed and no check started.
    #[test]
    fn a_run_waits_for_trustd_before_it_claims_its_disk() {
        let claims = |s: &mut Site, trustd: Option<&str>| {
            let cfg = s.cfg.clone();
            let mut c = Claims {
                site: s,
                never: None,
                interrupt: None,
                asked: Vec::new(),
                trustd: trustd.map(str::to_string),
                trustd_at: Vec::new(),
            };
            let mut out = Vec::new();
            let outcome = run_on(&cfg, &mut c, &mut out);
            let said = String::from_utf8(out).unwrap();
            let (asked, at) = (c.asked.len(), c.trustd_at);
            (outcome, said, asked, at)
        };
        let mut s = site(Scope::Packages(vec!["alpha".into()]));
        let (outcome, said, asked, at) = claims(&mut s, None);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        assert_eq!(at, [0], "the wait comes before the disk claim: {said}");
        assert_eq!(asked, 1, "{said}");
        assert!(!s.started.is_empty(), "{said}");

        const HELD: &str = "interrupted (SIGINT) while waiting for this build's trustd or none: \
                            a trustd of another build serves /tmp/t.sock; leased by pid 7 \
                            (trust lane holds trustd for its run); nothing was verified";
        let mut s = site(Scope::Packages(vec!["alpha".into()]));
        let (outcome, said, asked, at) = claims(&mut s, Some(HELD));
        assert_eq!(outcome, Outcome::CouldNotRun(HELD.into()), "{said}");
        assert_eq!((asked, at), (0, vec![0]), "no disk claimed: {said}");
        assert!(s.started.is_empty(), "{said}");

        // Its one library refused for memory and the run going on: nothing to check, no wait.
        let mut s = site(Scope::WholeTree);
        s.ceiling = Ok(true);
        fs::write(
            s.repo.join(trust_facts::MEMORY),
            "# no host here verifies alpha's floor\nalpha\t4096.0\t4096\t512.0\t1000.0\t=\ta test\n",
        )
        .unwrap();
        git_in(&s.repo, &["commit", "-q", "-am", "a floor set at 4 TiB"]);
        s.cfg.stop_if_narrowed = false;
        let (outcome, said, _, at) = claims(&mut s, Some(HELD));
        assert!(
            matches!(&outcome, Outcome::Narrowed(l) if l.len() == 1),
            "{outcome:?}: {said}"
        );
        assert!(
            at.is_empty(),
            "nothing to check waits for no trustd: {said}"
        );
    }

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

    /// A slot a killed check left stays while macOS may still be assessing a first exec in it:
    /// a filing run is refused and a `-p` run keeps it. Once its hold is spent a run empties it.
    #[test]
    fn a_slot_a_killed_check_left_stays_until_its_hold_is_spent() {
        let held = |scope| {
            let s = site(scope);
            let slot = root_of(&s).join("j7");
            owner_dirs(&slot.join("debug/build/x")).unwrap();
            fs::write(slot.join("debug/build/x/build-script-build"), "").unwrap();
            aterm_uds::deadline::stamp_exec(&slot);
            (s, slot)
        };
        let started = |slot: &Path| slot.join("debug/build/x/build-script-build").exists();

        let (mut whole, slot) = held(Scope::WholeTree);
        let (outcome, said, filed) = run_site(&mut whole);
        assert!(
            matches!(&outcome, Outcome::CouldNotRun(why) if why.contains("first exec")),
            "{outcome:?}: {said}"
        );
        assert!(filed.is_empty() && whole.started.is_empty(), "{said}");
        assert!(started(&slot), "{said}");

        let (mut named, slot) = held(Scope::Packages(vec!["alpha".into()]));
        let (outcome, said, _) = run_site(&mut named);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        assert!(said.contains(&format!("kept {}", slot.display())), "{said}");
        assert!(started(&slot), "{said}");

        fs::write(
            slot.join(aterm_uds::deadline::EXEC_STAMP),
            u64::MAX.to_string(),
        )
        .unwrap();
        let (outcome, said, _) = run_site(&mut named);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        assert!(!slot.exists(), "{said}");
    }

    /// A check a killed run left is ended and its slot held by a stamp; so is the slot of an
    /// earlier build's record, which names no mark and ends nothing.
    #[test]
    fn a_reaped_check_holds_its_slot() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("trust-lane-reaped");
        let root = d.path().join("root");
        for slot in ["j1", "j2", "j3"] {
            owner_dirs(&root.join(slot)).unwrap();
        }
        let nonce = nonce();
        let mut check = command("/bin/sleep")
            .arg("600")
            .env(CHECK_VAR, &nonce)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let pid = check.id();
        for (record, text) in [
            (".check.1", marked_record(&nonce, Some(pid), &root)),
            (".check.2", "999999999 never\n".to_string()),
            (".check.3", "unreadable\n".to_string()),
        ] {
            fs::write(root.join(record), text).unwrap();
            fs::set_permissions(root.join(record), fs::Permissions::from_mode(0o600)).unwrap();
        }
        reap_stale_checks(&root);
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
            aterm_uds::deadline::poll_yield();
        };
        assert!(ended.is_some(), "the stale check outlived its reap");
        for n in 1..=3 {
            reap_until_gone(
                &root,
                &root.join(format!(".check.{n}")),
                "the record of a check ended, or of one nothing is left of, goes",
                || {},
            );
        }
        let slots = ["j1", "j2", "j3"].map(|slot| root.join(slot));
        let mut held = held_slots(&root);
        held.sort();
        assert_eq!(held, slots);
        let mut kept = clear_slots(&root).unwrap();
        kept.sort();
        assert_eq!(kept, slots);
        assert!(slots.iter().all(|slot| slot.is_dir()));
    }

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
        let rows = cost_file(&seed);
        let alpha = &rows["alpha"];
        assert_eq!(
            alpha.len(),
            1,
            "this run's row stands in for the row that named no prover"
        );
        assert_eq!(
            (alpha[0].cpu, alpha[0].maxrss),
            (1, 1),
            "this run's measurement"
        );
        assert_ne!(alpha[0].when, "2026-10-01T00:00:00Z", "stamped by this run");
        assert_eq!(alpha[0].prover, "targo 0.0.0-test", "under its prover");
        assert_eq!(
            rows["zeta"],
            [Cost {
                cpu: 50,
                wall: 60,
                maxrss: 70,
                when: "2026-10-02T00:00:00Z".into(),
                prover: String::new(),
                cut: None,
                raised: false,
            }],
            "a row it did not measure keeps its own, over an older row of the history"
        );
        assert!(
            !rows.contains_key("gone"),
            "a library outside the graph is not written back: {rows:?}"
        );
    }

    /// A ratcheting run given `--reason` lowers onto its prover a floor another prover recorded
    /// that it falls below, the reason in the row; without one the row stands.
    #[test]
    fn a_ratcheting_run_lowers_another_provers_floor_only_with_its_reason() {
        let theirs = "alpha\t9\t20\ttargo 0.0.0-other\n";
        let lowered = |reason: Option<&str>| {
            let mut s = site(Scope::Packages(vec!["alpha".into()]));
            let floors = s.repo.join(trust_facts::RATCHET);
            let text = fs::read_to_string(&floors).unwrap();
            fs::write(
                &floors,
                text.replace("alpha\t5\t20\ttargo 0.0.0-test\n", theirs),
            )
            .unwrap();
            git_in(&s.repo, &["commit", "-q", "-am", "another prover's floor"]);
            s.cfg.update_ratchet = true;
            s.cfg.reason = reason.map(str::to_string);
            let (outcome, said, _) = run_site(&mut s);
            assert_eq!(
                outcome,
                Outcome::Fail(vec!["alpha RATCHET-REGRESSION(<9) 5/20".into()]),
                "judged against the floor as it stood: {said}"
            );
            let text = fs::read_to_string(&floors).unwrap();
            text.lines()
                .find(|l| l.starts_with("alpha\t"))
                .unwrap()
                .to_string()
        };
        assert_eq!(
            lowered(Some("the new prover asks fewer")),
            "alpha\t5\t20\ttargo 0.0.0-test\tthe new prover asks fewer"
        );
        assert_eq!(lowered(None), theirs.trim_end());
    }

    #[test]
    fn the_seed_keeps_the_later_row_of_each_library_still_in_the_graph() {
        let row = |cpu: u64, when: &str| {
            vec![Cost {
                cpu,
                wall: cpu,
                maxrss: cpu,
                when: when.into(),
                prover: P.into(),
                cut: None,
                raised: false,
            }]
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
        let cpu: BTreeMap<String, u64> = read(&seed_rows(committed, history, &graph), P)
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
        // Starved: a lapse is the clock's only then.
        s.load = Some(1000.0);
        s
    }

    fn filed_facts(s: &Site, name: &str) -> trust_facts::Facts {
        let record =
            fs::read_to_string(s.repo.join(".git/aterm-verify/receipts").join(name)).unwrap();
        trust_facts::Facts::parse(&record).unwrap()
    }

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

    #[test]
    fn an_interrupted_run_decides_nothing() {
        let mut s = site(Scope::WholeTree);
        s.interrupt = true;
        let (outcome, said, filed) = run_site(&mut s);
        assert!(matches!(outcome, Outcome::CouldNotRun(_)), "{said}");
        assert!(filed.is_empty());
    }

    /// F9: a run interrupted while a check runs decides nothing, and keeps what the check
    /// measured in its prover's row of the clone's cost history, marked interrupted: its 5 GiB
    /// peak is the least it needs. NEGATIVE CONTROL: a run interrupted before any check started
    /// keeps no row.
    #[test]
    fn an_interrupted_check_keeps_its_peak_and_decides_nothing() {
        for stop_at in [Some(Duration::from_secs(3)), None] {
            let mut s = site(Scope::WholeTree);
            s.stop_at = stop_at;
            s.interrupt = stop_at.is_none();
            let (outcome, said, filed) = run_site(&mut s);
            assert!(
                matches!(&outcome, Outcome::CouldNotRun(why) if why.starts_with("interrupted")),
                "{outcome:?}: {said}"
            );
            assert!(filed.is_empty(), "{filed:?}");
            let rows = cost_file(&s.cfg.state.clone().unwrap().join("trust-costs.tsv"));
            if stop_at.is_some() {
                assert_eq!(s.started, ["alpha"], "{said}");
                let alpha = &rows["alpha"];
                assert_eq!(
                    (alpha.len(), alpha[0].maxrss, alpha[0].cut),
                    (1, 5 * GIB, Some(Cut::Interrupted)),
                    "{rows:?}"
                );
                assert_eq!(alpha[0].prover, "targo 0.0.0-test");
            } else {
                assert!(s.started.is_empty(), "{said}");
                assert!(rows.is_empty(), "{rows:?}");
            }
        }
    }

    #[test]
    fn a_floor_this_host_cannot_reproduce_narrows_the_run() {
        for stop in [true, false] {
            let mut s = site(Scope::WholeTree);
            s.ceiling = Ok(true);
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

    /// A memory record row of aterm-gpu's shape on this 48 GiB host: measured under the
    /// RAM / 8 ceiling, it ran out at 6 GiB, so it needs more than 19.9 GiB.
    const GPU_ROW: &str =
        "# alpha ran out at its 6 GiB ceiling\nalpha\t-\t48\t6.0\t19.9\t>\t2026-10-20\n";

    /// Under a prover whose trustc takes no memory ceiling, the RAM / 8 model refuses nothing:
    /// a library whose row says it ran out at 6 GiB runs, and the HOST line says there is no
    /// ceiling. Negative control: under a prover that takes one, the same row refuses it.
    #[test]
    fn a_prover_with_no_ceiling_runs_what_the_ram_eighth_model_refuses() {
        for takes in [false, true] {
            let mut s = site(Scope::Packages(vec!["alpha".into()]));
            s.ceiling = Ok(takes);
            fs::write(s.repo.join(trust_facts::MEMORY), GPU_ROW).unwrap();
            let (outcome, said, _) = run_site(&mut s);
            let plan = fs::read_to_string(root_of(&s).join("logs/.plan")).unwrap();
            if takes {
                assert!(
                    matches!(&outcome, Outcome::Narrowed(l) if l == &["alpha: needs >19.9 GiB"]),
                    "{outcome:?}: {said}"
                );
                assert!(s.started.is_empty(), "{said}");
                assert!(plan.contains("\ndriver-ceiling yes\n"), "{plan}");
                assert!(said.contains("needs more than 19.9 GiB, this host gives 6.0 GiB"));
            } else {
                assert_eq!(outcome, Outcome::Pass, "{said}");
                assert_eq!(s.started, ["alpha"]);
                assert!(plan.contains("\ndriver-ceiling no\n"), "{plan}");
                assert!(!plan.contains("\nrefused "), "{plan}");
                assert!(
                    said.contains("HOST: 48 GiB RAM, no verifier ceiling"),
                    "{said}"
                );
            }
        }
    }

    /// A site whose cost history says alpha's last check peaked at `maxrss` bytes on
    /// 2026-10-20, on a row that names no prover, under a prover that does (`takes`) or does
    /// not take a memory ceiling.
    fn with_peak(maxrss: u64, takes: bool) -> Site {
        let mut s = with_peak_of(maxrss, "");
        s.ceiling = Ok(takes);
        s
    }

    /// A site whose cost history says alpha's last check peaked at `maxrss` bytes on
    /// 2026-10-20 under `prover` (none where empty).
    fn with_peak_of(maxrss: u64, prover: &str) -> Site {
        let mut s = site(Scope::Packages(vec!["alpha".into()]));
        s.ceiling = Ok(false);
        let state = s.cfg.state.clone().unwrap();
        fs::create_dir_all(&state).unwrap();
        let named = if prover.is_empty() {
            String::new()
        } else {
            format!("\t{prover}")
        };
        fs::write(
            state.join("trust-costs.tsv"),
            format!("{COSTS_HEADER}\nalpha\t16972\t17245\t{maxrss}\t2026-10-20T17:45:18Z{named}\n"),
        )
        .unwrap();
        s
    }

    /// TRUST-MEM-2 through the whole run, on a 48 GiB host with the launch gate on: aterm-gui
    /// under a prover with no row of its own (29ebb403d's place), whose only row is
    /// 20565c325's 47.20 GB interrupted one, is refused up front on that peak with its margin,
    /// its plan row `other-least` naming that prover, its line saying no check under this
    /// prover has finished, and no check starts; a finished 62.45 GB row of the 1.101 prover is
    /// refused as `other-peak`. The rule this replaced admitted the first on the 16 GiB claim of
    /// an unmeasured library, which let one reach a 33 GB footprint on 2026-10-30 while the
    /// other steps' claims waited behind it. A 30 GiB peak another prover recorded runs first,
    /// claiming 36 GiB on it, and the run names whose peak that is. NEGATIVE CONTROLS: the same
    /// 47.20 GB beside a finished 10 GiB row of this prover's own runs on its own, claiming
    /// 12 GiB, and names no other prover; so does a 1.101 62.45 GB row beside this prover's
    /// finished 10 GiB check and the 11 GiB one cut short after it, older or newer; and
    /// costs.tsv's two aterm-gui rows, this prover in 20565c325's place, refuse it as
    /// `own-least` on its own 47.20 GB, not the 1.101 prover's larger peak.
    #[test]
    fn another_provers_peak_refuses_up_front_until_this_provers_own_replaces_it() {
        const P15698: &str = "targo 1.99.0-dev (20565c325 2026-10-19) (targo 0.11.0+g20565c325)";
        let gate = |s: &mut Site, tag: &str, available: u64| {
            let dir = tmp(tag);
            let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
            s.gate = Some((
                aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, available, 14)),
                ledger.clone(),
            ));
            (dir, ledger)
        };
        for (maxrss, prover, word, peaked, token) in [
            (
                47_198_031_152,
                format!("{P15698}\tinterrupted"),
                "other-least",
                "was cut short after reaching 47.20 GB on 2026-10-20, the least it needs",
                "~52.7 GiB",
            ),
            (
                62_450_089_984,
                "targo 1.101.0-dev (targo 0.10.0)".to_string(),
                "other-peak",
                "peaked at 62.45 GB on 2026-10-20",
                "~69.8 GiB",
            ),
        ] {
            let mut s = with_peak_of(maxrss, &prover);
            let (_dir, ledger) = gate(&mut s, "trust-other-refused", 28);
            let (outcome, said, _) = run_site(&mut s);
            assert!(
                matches!(&outcome, Outcome::Narrowed(l) if l == &[format!("alpha: needs {token}")]),
                "{outcome:?}: {said}"
            );
            assert!(s.started.is_empty(), "no check starts: {said}");
            let named = prover.split('\t').next().unwrap();
            a_line_names(
                &said,
                &format!(
                    "trust: alpha not verified here: no check of it under this prover has finished, and its last check under {named} {peaked}"
                ),
                &["51.5 GB (48 GiB)"],
            );
            let plan = fs::read_to_string(root_of(&s).join("logs/.plan")).unwrap();
            assert!(
                plan.contains(&format!(
                    "\nrefused alpha\t{word}\t-\t{maxrss}\t2026-10-20T17:45:18Z\t{named}\n"
                )),
                "{plan}"
            );
            assert!(ledger.list().unwrap().is_empty(), "nothing claimed");
        }

        let mut fits = with_peak_of(30 * GIB, P15698);
        let (_dir, ledger) = gate(&mut fits, "trust-other-fits", 40);
        let (outcome, said, _) = run_site(&mut fits);
        assert_eq!((outcome, fits.started.len()), (Outcome::Pass, 1), "{said}");
        a_line_names(
            &said,
            "trust: alpha has no finished check under this prover",
            &[
                &format!("its last check under {P15698} peaked at 32.21 GB on 2026-10-20"),
                "runs first, claiming 36 GiB",
            ],
        );
        assert!(ledger.list().unwrap().is_empty(), "its claim let go");

        let mut own = with_peak_of(47_198_031_152, &format!("{P15698}\tinterrupted"));
        let state = own.cfg.state.clone().unwrap().join("trust-costs.tsv");
        let text = format!(
            "{}alpha\t1\t1\t{}\t2026-10-30T00:00:00Z\ttargo 0.0.0-test\n",
            fs::read_to_string(&state).unwrap(),
            10 * GIB
        );
        fs::write(&state, text).unwrap();
        let (_dir, _ledger) = gate(&mut own, "trust-other-own", 28);
        let (outcome, said, _) = run_site(&mut own);
        assert_eq!((outcome, own.started.len()), (Outcome::Pass, 1), "{said}");
        assert!(
            !said.contains("no finished check") && !said.contains(P15698),
            "its own row stands alone: {said}"
        );
        let plan = fs::read_to_string(root_of(&own).join("logs/.plan")).unwrap();
        assert!(!plan.contains("\nrefused "), "{plan}");

        const P1101: &str = "targo 1.101.0-dev (targo 0.10.0)";
        let with_rows = |rows: &[String]| {
            let s = with_peak_of(1, "");
            let state = s.cfg.state.clone().unwrap().join("trust-costs.tsv");
            fs::write(&state, format!("{COSTS_HEADER}\n{}", rows.concat())).unwrap();
            s
        };
        for other in ["2026-10-30", "2026-11-01"] {
            let mut measured = with_rows(&[
                format!(
                    "alpha\t1\t1\t{}\t2026-10-29T00:00:00Z\ttargo 0.0.0-test\n",
                    10 * GIB
                ),
                format!("alpha\t16972\t17245\t62450089984\t{other}T00:00:00Z\t{P1101}\n"),
                format!(
                    "alpha\t2\t2\t{}\t2026-10-31T00:00:00Z\ttargo 0.0.0-test\tinterrupted\n",
                    11 * GIB
                ),
            ]);
            let (_dir, _ledger) = gate(&mut measured, "trust-own-measured", 28);
            let (outcome, said, _) = run_site(&mut measured);
            assert_eq!(
                (outcome, measured.started.len()),
                (Outcome::Pass, 1),
                "{other}: {said}"
            );
            assert!(
                !said.contains("no finished check") && !said.contains(P1101),
                "{other}: a check under this prover finished: {said}"
            );
        }

        let mut cut = with_rows(&[
            format!("alpha\t16972\t17245\t62450089984\t2026-10-20T17:45:18Z\t{P1101}\n"),
            "alpha\t5862\t7547\t47198031152\t2026-10-31T01:00:39Z\ttargo 0.0.0-test\tinterrupted\n"
                .to_string(),
        ]);
        let (_dir, ledger) = gate(&mut cut, "trust-own-cut", 28);
        let (outcome, said, _) = run_site(&mut cut);
        assert!(
            matches!(&outcome, Outcome::Narrowed(l) if l == &["alpha: needs ~52.7 GiB"]),
            "{outcome:?}: {said}"
        );
        assert!(cut.started.is_empty(), "no check starts: {said}");
        a_line_names(
            &said,
            "trust: alpha not verified here: its last check under this prover",
            &[
                "was cut short after reaching 47.20 GB on 2026-10-31, the least it needs",
                "it claims 56.64 GB",
            ],
        );
        assert!(!said.contains(P1101), "{said}");
        let plan = fs::read_to_string(root_of(&cut).join("logs/.plan")).unwrap();
        assert!(
            plan.contains("\nrefused alpha\town-least\t-\t47198031152\t2026-10-31T01:00:39Z\t-\n"),
            "{plan}"
        );
        assert!(ledger.list().unwrap().is_empty(), "nothing claimed");
    }

    /// F9 through the whole run: a peak this prover's last check reached before it was cut short
    /// (aterm-gui's 47.20 GB under 20565c325, interrupted) is the least it needs, so a claim
    /// sized from it that this 48 GiB host can never make available is refused up front, its
    /// plan row `own-least` and its line saying the check was cut short. NEGATIVE CONTROL: the
    /// same row from a check that finished is refused as its peak.
    #[test]
    fn a_cut_checks_peak_refuses_up_front_as_the_least_it_needs() {
        for cut in ["\tinterrupted", ""] {
            let mut s = with_peak_of(47_198_031_152, &format!("targo 0.0.0-test{cut}"));
            let (outcome, said, _) = run_site(&mut s);
            assert!(
                matches!(&outcome, Outcome::Narrowed(l) if l == &["alpha: needs ~52.7 GiB"]),
                "{outcome:?}: {said}"
            );
            assert!(s.started.is_empty(), "no check starts: {said}");
            let plan = fs::read_to_string(root_of(&s).join("logs/.plan")).unwrap();
            let (word, peaked) = if cut.is_empty() {
                ("own-peak", "peaked at 47.20 GB on 2026-10-20, and")
            } else {
                (
                    "own-least",
                    "was cut short after reaching 47.20 GB on 2026-10-20, the least it needs, and",
                )
            };
            a_line_names(
                &said,
                "trust: alpha not verified here: its last check under this prover",
                &[peaked, "it claims 56.64 GB", "51.5 GB (48 GiB)"],
            );
            assert!(
                plan.contains(&format!(
                    "\nrefused alpha\t{word}\t-\t47198031152\t2026-10-20T17:45:18Z\t-\n"
                )),
                "{plan}"
            );
        }
    }

    /// F9 through the whole run: a check that ends by itself without a whole summary keeps its
    /// peak in its prover's row of the clone's cost history, marked failed, where it used to
    /// keep none. NEGATIVE CONTROL: a check that finished keeps its row unmarked.
    #[test]
    fn a_check_that_fails_keeps_its_peak_in_its_provers_row() {
        for (log, cut) in [
            ("error: could not compile `alpha`\n", Some(Cut::Failed)),
            (DONE, None),
        ] {
            let mut s = site(Scope::Packages(vec!["alpha".into()]));
            s.log = log;
            let (outcome, said, _) = run_site(&mut s);
            assert_eq!(s.started, ["alpha"], "{outcome:?}: {said}");
            let rows = cost_file(&s.cfg.state.clone().unwrap().join("trust-costs.tsv"));
            let alpha = &rows["alpha"];
            assert_eq!(alpha.len(), 1, "{rows:?}");
            assert_eq!(
                (alpha[0].maxrss, alpha[0].prover.as_str(), alpha[0].cut),
                (1, "targo 0.0.0-test", cut),
                "{log}: {said}"
            );
        }
    }

    /// Asserts that one line of `said` starts with `head` and names each of `facts`.
    fn a_line_names(said: &str, head: &str, facts: &[&str]) {
        assert!(
            said.lines()
                .any(|l| l.starts_with(head) && facts.iter().all(|f| l.contains(f))),
            "no line starting {head:?} names each of {facts:?}: {said}"
        );
    }

    /// A library whose last recorded check peaked above this host's RAM is refused up front
    /// under any prover, in one line naming the peak, its date and the RAM, and no check
    /// starts. So is one whose peak is below the RAM but whose claim, with its margin, is
    /// not, since the launch gate never has all of the RAM available; its line names the
    /// claim too. Negative controls: a claim below the RAM runs, and `--try-refused` runs it.
    #[test]
    fn a_library_whose_recorded_peak_is_above_the_ram_is_refused_up_front() {
        for takes in [false, true] {
            let mut s = with_peak(62_450_089_984, takes);
            let (outcome, said, _) = run_site(&mut s);
            assert!(
                matches!(&outcome, Outcome::Narrowed(l) if l == &["alpha: needs ~69.8 GiB"]),
                "{outcome:?}: {said}"
            );
            assert!(s.started.is_empty(), "no check starts: {said}");
            a_line_names(
                &said,
                "trust: alpha not verified here",
                &["62.45 GB", "2026-10-20", "51.5 GB (48 GiB)"],
            );
            assert!(said.contains("more RAM than its recorded peak"), "{said}");
            let plan = fs::read_to_string(root_of(&s).join("logs/.plan")).unwrap();
            assert!(
                plan.contains("\nrefused alpha\tpeak\t-\t62450089984\t2026-10-20T17:45:18Z\t-\n"),
                "{plan}"
            );
        }
        let mut tight = with_peak(40 * GIB, false);
        let (outcome, said, _) = run_site(&mut tight);
        assert!(
            matches!(&outcome, Outcome::Narrowed(l) if l == &["alpha: needs ~48.0 GiB"]),
            "{outcome:?}: {said}"
        );
        assert!(tight.started.is_empty(), "no check starts: {said}");
        a_line_names(
            &said,
            "trust: alpha not verified here",
            &["42.95 GB", "51.54 GB", "51.5 GB (48 GiB)"],
        );
        let mut fits = with_peak(39 * GIB, false);
        let (outcome, said, _) = run_site(&mut fits);
        assert_eq!((outcome, fits.started.len()), (Outcome::Pass, 1), "{said}");
        let mut forced = with_peak(62_450_089_984, false);
        forced.cfg.try_refused = true;
        let (outcome, said, _) = run_site(&mut forced);
        assert_eq!(
            (outcome, forced.started.len()),
            (Outcome::Pass, 1),
            "{said}"
        );
    }

    /// NEVER-REACHABLE (`docs/DESIGN-adaptive-build-2026-10-24.md`): on a 48 GiB host of which
    /// 44.5 GiB can ever be available (3.5 GiB wired), a library whose claim falls between the
    /// two (a 38 GiB peak claims 45.6 GiB) is refused up front in one line naming the peak, the
    /// claim and that ceiling, its plan row keeping the ceiling, and no check starts. A 37 GiB
    /// peak (44.4 GiB) runs; under `--try-refused` the refused one starts at once at the gate,
    /// which says it can never be admitted, naming the ceiling. Negative control: with nothing
    /// read of what can be available (the old rule, the RAM), the 38 GiB peak's check starts,
    /// though no wait could ever admit its claim.
    #[test]
    fn a_claim_above_what_can_ever_be_available_is_refused_up_front() {
        let reach = 48 * GIB - 7 * GIB / 2;
        let mut s = with_peak(38 * GIB, false);
        s.reach = reach;
        let (outcome, said, _) = run_site(&mut s);
        assert!(
            matches!(&outcome, Outcome::Narrowed(l) if l == &["alpha: needs ~45.6 GiB"]),
            "{outcome:?}: {said}"
        );
        assert!(s.started.is_empty(), "no check starts: {said}");
        a_line_names(
            &said,
            "trust: alpha not verified here",
            &["40.80 GB", "48.96 GB", "47.8 GB", "51.5 GB (48 GiB)"],
        );
        let plan = fs::read_to_string(root_of(&s).join("logs/.plan")).unwrap();
        for row in [
            format!("\nhost-reach {reach}\n"),
            format!(
                "\nrefused alpha\tpeak\t{reach}\t{}\t2026-10-20T17:45:18Z\t-\n",
                38 * GIB
            ),
        ] {
            assert!(plan.contains(&row), "{row:?} in {plan}");
        }
        let mut fits = with_peak(37 * GIB, false);
        fits.reach = reach;
        let (outcome, said, _) = run_site(&mut fits);
        assert_eq!((outcome, fits.started.len()), (Outcome::Pass, 1), "{said}");
        let mut forced = with_peak(38 * GIB, false);
        (forced.reach, forced.cfg.try_refused) = (reach, true);
        let dir = tmp("trust-reach-site");
        let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
        let mac = aterm_capacity::Reading {
            wired: Some(7 * GIB / 2),
            ..aterm_capacity::Reading::of_gib(48, 28, 14)
        };
        forced.gate = Some((aterm_capacity::Fixed::new(mac), ledger.clone()));
        let (outcome, said, _) = run_site(&mut forced);
        assert_eq!(
            (outcome, forced.started.len()),
            (Outcome::Pass, 1),
            "{said}"
        );
        a_line_names(
            &said,
            "trust: capacity: trust check alpha",
            &["45.6 GiB", "44.5 GiB", "3.5 GiB", "starts at once"],
        );
        assert!(ledger.list().unwrap().is_empty(), "its claim let go");
        let mut old = with_peak(38 * GIB, false);
        let (outcome, said, _) = run_site(&mut old);
        assert_eq!((outcome, old.started.len()), (Outcome::Pass, 1), "{said}");
    }

    /// NEVER-REACHABLE: the plan's bound reaches the launch gate. On a 48 GiB host whose plan
    /// read 44.5 GiB that can ever be available, a library whose claim is below that (a 36.8
    /// GiB peak claims 44.2 GiB) runs past the plan and meets a gate that reads 4 GiB wired, 44
    /// GiB at most available: without `--try-refused` it waits for what is wired to fall, and
    /// no check starts before the run ends at [`GATE_HANG`]. Negative control: a gate judging by
    /// its own reading alone started it at once.
    #[test]
    fn a_claim_the_plan_admitted_does_not_start_at_once_when_more_is_wired_at_the_gate() {
        let mut s = with_peak(368 * GIB / 10, false);
        s.reach = 48 * GIB - 7 * GIB / 2;
        let dir = tmp("trust-wired-site");
        let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
        let mac = aterm_capacity::Reading {
            wired: Some(4 * GIB),
            ..aterm_capacity::Reading::of_gib(48, 21, 14)
        };
        s.gate = Some((aterm_capacity::Fixed::new(mac), ledger));
        let (outcome, said, _) = run_site(&mut s);
        assert!(s.started.is_empty(), "{outcome:?}: {said}");
        assert!(
            matches!(&outcome, Outcome::CouldNotRun(why) if why.starts_with("interrupted")),
            "{outcome:?}: {said}"
        );
        a_line_names(
            &said,
            "trust: capacity: trust check alpha",
            &["44.5 GiB", " waits "],
        );
        assert!(!said.contains("starts at once"), "{said}");
    }

    /// `--try-refused` on a library whose recorded peak is above the RAM, with the launch gate
    /// on: on a machine whose available memory is below its RAM, its check starts at once,
    /// saying it can never be admitted here, and lets its claim go when it ends. Negative
    /// control: the claim once took all of the RAM and waited at the gate for memory that is
    /// never available (here the run would end interrupted at [`GATE_HANG`]).
    #[test]
    fn try_refused_starts_a_claim_this_host_can_never_admit_at_once() {
        let mut s = with_peak(62_450_089_984, false);
        s.cfg.try_refused = true;
        let dir = tmp("trust-gate-site");
        let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
        s.gate = Some((
            aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 28, 14)),
            ledger.clone(),
        ));
        let (outcome, said, _) = run_site(&mut s);
        assert_eq!((outcome, s.started.len()), (Outcome::Pass, 1), "{said}");
        a_line_names(
            &said,
            "trust: capacity: trust check alpha",
            &["69.8 GiB", "48 GiB of memory", "starts at once"],
        );
        assert!(!said.contains(" waits "), "{said}");
        assert!(ledger.list().unwrap().is_empty(), "its claim let go");
    }

    /// The probe reads `trustc -Z help`'s option rows: a row for the ceiling flag (321aaeda7's,
    /// as its trustc prints it) lists it, and a row for another flag that mentions it, or a
    /// longer flag that starts with its name, does not.
    #[test]
    fn the_ceiling_probe_reads_the_flag_s_own_row() {
        let row = "    -Z             trust-verify-memory-ceiling-bytes=val -- set the resident-memory ceiling for this verifier process in bytes (0 = disabled).";
        assert!(lists_ceiling(&format!(
            "    -Z  trust-ir-lower=val -- off|on\n{row}\n"
        )));
        assert!(lists_ceiling("-Z trust-verify-memory-ceiling-bytes"));
        for other in [
            "    -Z     trust-proc-macro-boundary=val -- requires -Ztrust-verify-session and verification",
            "    -Z     trust-limits=val -- replaces -Ztrust-verify-memory-ceiling-bytes",
            "    -Z     trust-verify-memory-ceiling-bytes-soft=val -- a longer flag",
            "",
        ] {
            assert!(!lists_ceiling(other), "{other}");
        }
    }

    /// Whether the prover takes a ceiling is probed once per build: a second run reads the
    /// kept answer, a rebuilt trustc is probed again, and a probe that cannot be read is said,
    /// taken as no ceiling and kept for nothing.
    #[test]
    fn the_ceiling_probe_is_kept_per_prover_build() {
        let mut s = site(Scope::Packages(vec!["alpha".into()]));
        s.ceiling = Ok(true);
        run_site(&mut s);
        run_site(&mut s);
        assert_eq!(s.ceiling_probes, 1);
        fs::write(s.driver.bin.join("trustc"), "trustc, rebuilt").unwrap();
        s.ceiling = Ok(false);
        let (outcome, said, _) = run_site(&mut s);
        assert_eq!((s.ceiling_probes, outcome), (2, Outcome::Pass), "{said}");
        let kept = fs::read_to_string(s.cfg.state.as_ref().unwrap().join(CEILING_CACHE)).unwrap();
        assert_eq!(
            kept.lines().filter(|l| !l.starts_with('#')).count(),
            1,
            "{kept}"
        );
        assert!(kept.ends_with("\tno\n"), "{kept}");

        let mut unread = site(Scope::Packages(vec!["alpha".into()]));
        unread.ceiling = Err("trustc -Z help exited 1".into());
        let (_, said, _) = run_site(&mut unread);
        a_line_names(
            &said,
            "trust: trustc -Z help exited 1",
            &["no verifier memory ceiling"],
        );
        run_site(&mut unread);
        assert_eq!(unread.ceiling_probes, 2, "an unread answer is not kept");
        let plan = fs::read_to_string(root_of(&unread).join("logs/.plan")).unwrap();
        assert!(plan.contains("\ndriver-ceiling no\n"), "{plan}");
    }

    /// The plan says the host's load as the run began and as it ended, and a filing run's
    /// provenance says them beside the mean over its checks.
    #[test]
    fn the_plan_says_the_load_at_the_start_and_the_end() {
        let mut s = site(Scope::WholeTree);
        s.load = Some(3.5);
        let (outcome, said, filed) = run_site(&mut s);
        assert_eq!((outcome, filed.len()), (Outcome::Pass, 1), "{said}");
        let plan = fs::read_to_string(root_of(&s).join("logs/.plan")).unwrap();
        for line in [
            "\nload-start 3.50\n",
            "\nload-end 3.50\n",
            "\nload 3.50 over its checks, 3.50 at the start, 3.50 at the end\n",
        ] {
            assert!(plan.contains(line), "{line:?}: {plan}");
        }
        let mut unread = site(Scope::Packages(vec!["alpha".into()]));
        run_site(&mut unread);
        let plan = fs::read_to_string(root_of(&unread).join("logs/.plan")).unwrap();
        assert!(plan.contains("\nload-start unread\n"), "{plan}");
    }

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
        assert!(
            !second_orphan.exists(),
            "orphans are pruned on any default run"
        );
        assert!(
            root.join("j0/debug").is_dir(),
            "the slot of a check record a killed run left is held, so the root stays: {said}"
        );
        assert!(!said.lines().any(|l| l.contains(&root_s)), "{said}");

        fs::write(
            root.join("j0").join(aterm_uds::deadline::EXEC_STAMP),
            u64::MAX.to_string(),
        )
        .unwrap();
        let (outcome, said, _) = run_site(&mut s);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        let naming: Vec<&str> = said.lines().filter(|l| l.contains(&root_s)).collect();
        assert!(naming.len() == 1 && naming[0].contains("removed"), "{said}");
        assert!(
            !root.exists(),
            "the fallback root is retired once the hold is spent: {said}"
        );
    }

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

    fn says_in_order(said: &str, parts: &[&str]) -> bool {
        let mut rest = said;
        parts.iter().all(|p| {
            rest.find(p).is_some_and(|at| {
                rest = &rest[at + p.len()..];
                true
            })
        })
    }

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
        let me = named_lock::Holder::me("a test", d.path());
        let lock = RunLock::take(&held.join(".run.lock"), &me, "a test holds it").unwrap();
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
        let slotted = make("slotted", Some(&d.path().join("slotted-gone")));
        owner_dirs(&slotted.join("j0")).unwrap();
        aterm_uds::deadline::stamp_exec(&slotted.join("j0"));
        let weak = make("weak", Some(&d.path().join("weak-gone")));
        fs::set_permissions(&weak, fs::Permissions::from_mode(0o775)).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o775)).unwrap();
        assert!(prune_orphans(&cache, &own, &named_lock::Holder::me("a test", &own)).is_empty());
        assert!(orphan.exists());
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
        let mark = nonce();
        let mut check = command("/bin/sleep")
            .arg("600")
            .env(CHECK_VAR, &mark)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        fs::write(
            orphan.join(".check.0"),
            marked_record(&mark, Some(check.id()), &orphan),
        )
        .unwrap();
        fs::set_permissions(orphan.join(".check.0"), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            prune_orphans(&cache, &own, &named_lock::Holder::me("a test", &own)),
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
            aterm_uds::deadline::poll_yield();
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
            &slotted,
            &weak,
        ] {
            assert!(kept.join("logs").is_dir(), "{}", kept.display());
        }
        drop(lock);
        assert_eq!(
            prune_orphans(&cache, &own, &named_lock::Holder::me("a test", &own)),
            [(held, d.path().join("also-gone"))],
            "once its run lets go"
        );
        fs::write(
            slotted.join("j0").join(aterm_uds::deadline::EXEC_STAMP),
            u64::MAX.to_string(),
        )
        .unwrap();
        assert_eq!(
            prune_orphans(&cache, &own, &named_lock::Holder::me("a test", &own)),
            [(slotted, d.path().join("slotted-gone"))],
            "once its slot's hold is spent"
        );
    }

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
        let me = named_lock::Holder::me("ship trust run -p a", Path::new("/checkouts/a"));
        let first = RunLock::take(&path, &me, "wait").unwrap();
        let held = RunLock::take(&path, &me, "wait").err().expect("held");
        assert!(
            held.contains(&format!(
                "pid {} (ship trust run -p a, /checkouts/a, since ",
                me.pid
            )),
            "{held}"
        );
        drop(first);
        assert!(
            RunLock::take(&path, &me, "wait").is_ok(),
            "released when the run ends"
        );
    }

    /// [`a_held_root_is_waited_for_until_its_holder_dies_by_its_own_hand`]'s holder.
    const ROOT_HOLDER: &str =
        "trust_lane::tests::a_held_root_is_waited_for_until_its_holder_dies_by_its_own_hand";
    /// The run lock the stand-in takes.
    const ROOT_HOLDER_ENV: &str = "ATERM_RELEASE_TEST_ROOT_HOLDER";
    /// What the stand-in says once it holds the root (libtest's own words share its line).
    const ROOT_HOLDER_HOLDS: &str = "the stand-in holds the root";

    /// Not a test alone: as [`ROOT_HOLDER`] it takes the run lock at [`ROOT_HOLDER_ENV`]'s path,
    /// says [`ROOT_HOLDER_HOLDS`], and at its first line on stdin dies by its own `SIGKILL`.
    fn a_root_holder_stand_in_if_asked() {
        let Some(path) = std::env::var_os(ROOT_HOLDER_ENV) else {
            return;
        };
        let me = named_lock::Holder::me("ship trust run -p a", Path::new("/checkouts/holder"));
        let _held = RunLock::take(Path::new(&path), &me, "the root is free").unwrap();
        println!("{ROOT_HOLDER_HOLDS}");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().read_line(&mut String::new());
        aterm_uds::process::kill_self_now()
    }

    /// The waiter's `out`: the first line it writes tells the stand-in to die.
    struct DieOnFirstWrite {
        heard: Vec<u8>,
        die: Option<std::process::ChildStdin>,
    }

    impl Write for DieOnFirstWrite {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.heard.extend_from_slice(buf);
            if let Some(mut stdin) = self.die.take() {
                stdin.write_all(b"die\n")?;
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A second run on a held target root waits with its holder named, and has the root once
    /// the holder is gone, though the holder died by its own `SIGKILL` and nobody signalled
    /// anyone. Negative control: `--no-wait` refuses at once, naming the same holder.
    #[test]
    fn a_held_root_is_waited_for_until_its_holder_dies_by_its_own_hand() {
        a_root_holder_stand_in_if_asked();
        use std::io::BufRead as _;
        use std::os::unix::process::ExitStatusExt as _;
        let d = tmp("trust-lane-lock-wait");
        let path = d.path().join(".run.lock");
        let mut holder = command(std::env::current_exe().unwrap())
            .args(["--exact", ROOT_HOLDER, "--nocapture", "--test-threads=1"])
            .env(ROOT_HOLDER_ENV, &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = holder.id();
        let said = BufReader::new(holder.stdout.take().unwrap());
        let (holds, held_now) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in said.lines().map_while(Result::ok) {
                if line.contains(ROOT_HOLDER_HOLDS) {
                    let _ = holds.send(());
                }
            }
        });
        // The one-minute hang detector: the stand-in takes a free root at once.
        held_now
            .recv_timeout(Duration::from_secs(60))
            .expect("the stand-in holds the root");
        let named = format!("pid {pid} (ship trust run -p a, /checkouts/holder, since ");
        let me = named_lock::Holder::me("ship trust run -p a", Path::new("/checkouts/waiter"));
        let mut quiet = Vec::new();
        let refused = RunLock::wait(&path, &me, named_lock::Wait::No, &mut quiet)
            .err()
            .expect("--no-wait does not wait");
        assert!(refused.contains(&named), "{refused}");
        assert!(quiet.is_empty(), "--no-wait says nothing of a wait");

        let die = holder.stdin.take();
        let (died, death) = std::sync::mpsc::channel();
        let reaper = std::thread::spawn(move || {
            let status = holder.wait().unwrap();
            let _ = died.send(Instant::now());
            status
        });
        let (took, taken) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let mut out = DieOnFirstWrite {
                heard: Vec::new(),
                die,
            };
            let held = RunLock::wait(&path, &me, named_lock::Wait::Block, &mut out).map(drop);
            let _ = took.send(Instant::now());
            (held, String::from_utf8_lossy(&out.heard).into_owned())
        });
        // The one-minute hang detector: the root frees itself as its holder dies.
        let had = taken
            .recv_timeout(Duration::from_secs(60))
            .expect("the waiter has the root once its holder is gone");
        let (held, heard) = waiter.join().unwrap();
        held.unwrap();
        let status = reaper.join().unwrap();
        assert_eq!(status.signal(), Some(9), "{status}");
        let gone = death.recv().unwrap();
        assert!(
            heard.starts_with(&format!("trust: waiting for {named}")),
            "{heard}"
        );
        eprintln!(
            "root free {} ms after its holder died",
            had.saturating_duration_since(gone).as_millis()
        );
    }

    /// A run that waited for its root resolves its prover again, since a wait can outlast the
    /// build it resolved first: it names the one it now has and files the run against it.
    /// Negative control: a run that found the root free resolves it once.
    #[test]
    fn a_run_that_waited_resolves_its_prover_again() {
        let mut s = site(Scope::WholeTree);
        let (outcome, said, _) = run_site(&mut s);
        assert_eq!((outcome, s.drivers_asked), (Outcome::Pass, 1), "{said}");

        let mut s = site(Scope::WholeTree);
        let bin = s.driver.bin.with_file_name("updated");
        fs::create_dir_all(&bin).unwrap();
        for tool in ["targo", "trustc", "trustd"] {
            fs::write(bin.join(tool), tool).unwrap();
        }
        s.updated = Some(Driver {
            targo: bin.join("targo"),
            bin: bin.clone(),
            id: s.driver.id.clone(),
        });
        let root = root_of(&s);
        owner_dirs(&root).unwrap();
        let holder = named_lock::Holder::me("ship trust run -p alpha", Path::new("/checkouts/h"));
        let held = RunLock::take(&root.join(".run.lock"), &holder, "a test holds it").unwrap();
        let (waits, waiting) = std::sync::mpsc::channel::<()>();
        let letting_go = std::thread::spawn(move || {
            // The one-minute hang detector: the run names its holder a tick into its wait.
            waiting
                .recv_timeout(Duration::from_secs(60))
                .expect("the run waits for the root");
            drop(held);
        });

        /// The run's `out`: its first wait line lets the holder go.
        struct Out {
            said: Vec<u8>,
            waits: Option<std::sync::mpsc::Sender<()>>,
        }
        impl Write for Out {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.said.extend_from_slice(buf);
                if String::from_utf8_lossy(&self.said).contains("trust: waiting for ")
                    && let Some(waits) = self.waits.take()
                {
                    let _ = waits.send(());
                }
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let cfg = s.cfg.clone();
        let mut out = Out {
            said: Vec::new(),
            waits: Some(waits),
        };
        let outcome = run_on(&cfg, &mut s, &mut out);
        letting_go.join().unwrap();
        let said = String::from_utf8(out.said).unwrap();
        assert_eq!((outcome, s.drivers_asked), (Outcome::Pass, 2), "{said}");
        assert!(
            said.contains(&format!("trust: prover {} (", bin.display())),
            "{said}"
        );
        let plan = fs::read_to_string(root.join("logs/.plan")).unwrap();
        assert!(
            plan.contains(&format!("prover-bin {}\n", bin.display())),
            "{plan}"
        );
    }

    #[test]
    fn a_run_names_its_scope() {
        assert_eq!(
            run_verb(&Scope::Packages(vec!["a".into(), "b".into()])),
            "ship trust run -p a -p b"
        );
        assert_eq!(run_verb(&Scope::WholeTree), "ship trust run --whole-tree");
    }

    #[test]
    fn the_exit_status_says_what_was_decided() {
        assert_eq!(Outcome::Pass.exit(), 0);
        assert_eq!(Outcome::Fail(vec!["a".into()]).exit(), 1);
        assert_eq!(Outcome::Undecided(vec!["a".into()]).exit(), 2);
        assert_eq!(Outcome::CouldNotRun("x".into()).exit(), 2);
        assert_eq!(Outcome::Narrowed(vec!["a".into()]).exit(), 4);
    }

    /// A fresh nonce, as [`RealChecks::start`] gives a check.
    fn nonce() -> String {
        aterm_uds::rand::hex_token::<NONCE_BYTES>().unwrap()
    }

    /// `pid`'s serial ([`aterm_uds::process::process_serial`]).
    fn serial_of(pid: u32) -> u64 {
        aterm_uds::process::process_serial(i32::try_from(pid).unwrap()).unwrap()
    }

    /// A lane no process is: no pid this high is given out.
    const GONE_LANE: (i32, u64) = (i32::MAX, 1);

    /// The record [`RealChecks::start`] writes under the target root `at` for a check marked
    /// `nonce` in this boot by a lane now gone, its root `root` when given.
    fn record_at(nonce: &str, root: Option<(i32, u64)>, at: &Path) -> String {
        CheckRecord {
            nonce: Some(nonce.to_owned()),
            boot: aterm_uds::process::boot_id(),
            lane: Some(GONE_LANE),
            dir: Some(dir_id(at).unwrap()),
            root,
            kept: BTreeSet::new(),
        }
        .text()
    }

    /// [`record_at`], its root the live process `root` when given.
    fn marked_record(nonce: &str, root: Option<u32>, at: &Path) -> String {
        let root = root.map(|pid| (i32::try_from(pid).unwrap(), serial_of(pid)));
        record_at(nonce, root, at)
    }

    /// `pid`'s `ps -o lstart=` start, which an earlier build recorded beside the pid.
    fn lstart(pid: u32) -> String {
        let o = command("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// What the kernel holds at a pid ([`state`]).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum State {
        /// No process holds it.
        Gone,
        /// A process that exited, or is exiting, and is not yet reaped.
        Zombie,
        /// A stopped process.
        Stopped,
        /// A process that runs.
        Runs,
    }

    impl State {
        /// It runs nothing again: gone, or a zombie.
        fn ended(self) -> bool {
            matches!(self, State::Gone | State::Zombie)
        }
    }

    /// `pid`'s [`State`], read in this process: a test's poll of it spawns nothing, so it adds
    /// no process to the listing of another test's sweep running beside it
    /// ([`proc_tree::table`]), which a `ps` per poll kept from ever being whole on a loaded
    /// machine.
    #[cfg(target_vendor = "apple")]
    fn state(pid: u32) -> State {
        use aterm_uds::process::{BsdShortInfo, ProcessPresence};
        let pid = i32::try_from(pid).unwrap();
        // A zombie, and a process whose exit has begun, read no record here.
        match aterm_uds::process::read_short_info(pid) {
            Some(info) if info.pbsi_status == BsdShortInfo::SSTOP => State::Stopped,
            Some(_) => State::Runs,
            None => match aterm_uds::process::process_presence(pid) {
                ProcessPresence::Absent => State::Gone,
                _ => State::Zombie,
            },
        }
    }

    /// See the Apple twin: the state letter of `/proc/<pid>/stat`.
    #[cfg(not(target_vendor = "apple"))]
    fn state(pid: u32) -> State {
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return State::Gone;
        };
        match aterm_uds::process::procfs::stat_start(&stat, i32::try_from(pid).unwrap()) {
            Some(('Z' | 'X', _)) => State::Zombie,
            Some(('T' | 't', _)) => State::Stopped,
            Some(_) => State::Runs,
            None => State::Gone,
        }
    }

    /// The program at `path` as a process whose environment another can read: macOS withholds
    /// a platform binary's, so on macOS a test runs its signed stand-in
    /// ([`aterm_uds::deadline::stand_in`]).
    fn readable(path: &str) -> PathBuf {
        aterm_uds::deadline::stand_in(Path::new(path))
    }

    /// Whether `pid` runs `exe` within a minute: it exec'd it and its thread ran, so its
    /// environment reads.
    fn runs_from(pid: u32, exe: &Path) -> bool {
        let exe = fs::canonicalize(exe).unwrap();
        let p = i32::try_from(pid).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if matches!(aterm_uds::holders::exe_of(pid), aterm_uds::holders::Exe::At(at) if at == exe)
                && aterm_uds::process::thread_ran(p) == Some(true)
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            aterm_uds::deadline::poll_yield();
        }
    }

    /// Whether `pid` has left `root`'s tree: its parent is neither `root` nor a child of
    /// `root`, the subshell that started it having exited. launchd adopts such an orphan, or
    /// on Linux the nearest subreaper (`systemd --user`, a container's init).
    fn outside_tree(pid: u32, root: u32) -> bool {
        let parent = |p: u32| aterm_uds::process::read_process_parent(i32::try_from(p).ok()?);
        parent(pid).is_some_and(|q| q != root && parent(q) != Some(root))
    }

    /// The process `file` names once it has left `root`'s tree ([`outside_tree`]), within a
    /// minute.
    fn orphan_named(file: &Path, root: u32) -> Leftover {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(p) = fs::read_to_string(file)
                .ok()
                .and_then(|t| t.trim().parse().ok())
                && outside_tree(p, root)
            {
                return Leftover::new(p);
            }
            assert!(
                Instant::now() < deadline,
                "the root never left an orphan outside its tree ({})",
                file.display()
            );
            aterm_uds::deadline::poll_yield();
        }
    }

    /// Whether `pid` still holds the process `serial` names, as more than a zombie.
    fn alive(pid: u32, serial: u64) -> bool {
        aterm_uds::process::process_serial(i32::try_from(pid).unwrap()) == Some(serial)
            && !state(pid).ended()
    }

    /// Whether `pid`'s process `serial` ends within a minute; it is killed if not.
    fn ends(pid: u32, serial: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(60);
        while alive(pid, serial) {
            if Instant::now() >= deadline {
                let _ = command("/bin/kill")
                    .args(["-KILL", &pid.to_string()])
                    .status();
                return false;
            }
            aterm_uds::deadline::poll_yield();
        }
        true
    }

    /// Reaps `root` until `record` is gone, as the runs after this one would: a reap whose
    /// listing is not whole, as a loaded machine's may not be within `proc_tree::LISTING`,
    /// keeps the record it watched. `between` checks, after each reap, what no reap may do. A
    /// one-minute hang detector.
    fn reap_until_gone(root: &Path, record: &Path, what: &str, mut between: impl FnMut()) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while record.exists() {
            assert!(Instant::now() < deadline, "{what}");
            reap_stale_checks(root);
            between();
        }
    }

    /// Kills a process a test left running when dropped, if its pid still holds it.
    struct Leftover {
        pid: u32,
        serial: u64,
    }

    impl Leftover {
        fn new(pid: u32) -> Leftover {
            Leftover {
                pid,
                serial: serial_of(pid),
            }
        }

        /// Whether it still runs, neither stopped nor killed.
        fn running(&self) -> bool {
            aterm_uds::process::process_serial(i32::try_from(self.pid).unwrap())
                == Some(self.serial)
                && state(self.pid) == State::Runs
        }
    }

    impl Drop for Leftover {
        fn drop(&mut self) {
            if alive(self.pid, self.serial) {
                let _ = command("/bin/kill")
                    .args(["-KILL", &self.pid.to_string()])
                    .status();
            }
        }
    }

    /// What a root leading its own session and group left when it was killed and reaped: a
    /// subshell looping in its group, beating `beat` each turn. A run killed mid-check leaves
    /// this once someone else ends its root; a stranger's pipeline whose leader exited has the
    /// same shape.
    struct Group {
        /// The root's pid, serial and `ps` start, read while it ran.
        root: u32,
        root_serial: u64,
        lstart: String,
        /// The subshell, killed when dropped.
        left: Leftover,
        /// The pipe the subshell waits on, closed only once it is killed.
        _quiet: std::process::ChildStdin,
        beat: PathBuf,
    }

    impl Group {
        /// Kills all the group holds, and whether none of it is left within a minute.
        fn end(&self) -> bool {
            let group = self.root.to_string();
            let _ = command("/bin/kill")
                .args(["-KILL", "--", &format!("-{group}")])
                .status();
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                // Read in this process, as `state` is: a `ps` per turn adds to the listing of
                // every sweep running beside it.
                if aterm_uds::process::group_members(self.root)
                    .is_some_and(|m| m.complete && m.pids.is_empty())
                {
                    return true;
                }
                if Instant::now() >= deadline {
                    return false;
                }
                aterm_uds::deadline::poll_yield();
            }
        }

        /// Whether the subshell beats again within a minute, so it runs on.
        fn beats_on(&self) -> bool {
            let len = || fs::metadata(&self.beat).map_or(0, |m| m.len());
            let was = len();
            let deadline = Instant::now() + Duration::from_secs(60);
            while len() == was {
                if Instant::now() >= deadline {
                    return false;
                }
                aterm_uds::deadline::poll_yield();
            }
            true
        }
    }

    /// What a test's beating loop runs after each turn's `read -t 1`: macOS's bash 3.2 returns 1
    /// both when the second runs out and when the pipe closes, so a loop that turns nine times
    /// within one second of `$SECONDS` is reading a closed pipe (its test is gone, killed before
    /// it ended the loop) and ends, where one that waits out each second beats on. A loop that
    /// read on would append to its beat file and spin a core for good.
    const SURVIVOR_ENDS: &str =
        "[ \"$SECONDS\" != \"$t\" ] && t=$SECONDS && n=0; n=$((n+1)); [ $n -lt 9 ] || exit;";

    /// Starts a [`Group`] whose processes carry `mark` when given, its root starting in `cwd`
    /// (the test's own when `None`) and running the shell words `prelude` before the subshell,
    /// which inherits all three. The subshell forks nothing: it waits out each turn reading
    /// a pipe no one writes, so every look reads it, environment and all, as it is. Once that
    /// pipe closes (the test is gone, killed before it ended the group) each read returns at
    /// once, and the subshell ends at the ninth turn within one second ([`SURVIVOR_ENDS`]).
    fn rootless_group(dir: &Path, mark: Option<&str>, cwd: Option<&Path>, prelude: &str) -> Group {
        let pidfile = dir.join("left.pid");
        let beat = dir.join(format!("beat.{}", nonce()));
        let _ = fs::remove_file(&pidfile);
        let mut cmd = command(readable("/bin/bash"));
        cmd.arg("-c")
            .arg(format!(
                "{prelude}exec 3<&0; t=; n=0; while :; do echo >> \"$2\"; read -t 1 -u 3; \
                 {SURVIVOR_ENDS} done & echo $! > \"$1\"; wait"
            ))
            .arg("sh")
            .arg(&pidfile)
            .arg(&beat)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(mark) = mark {
            cmd.env(CHECK_VAR, mark);
        }
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        proc_tree::new_session(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let root = child.id();
        let root_serial = serial_of(root);
        let lstart = lstart(root);
        let deadline = Instant::now() + Duration::from_secs(60);
        let left: u32 = loop {
            if let Some(p) = fs::read_to_string(&pidfile)
                .ok()
                .and_then(|t| t.trim().parse().ok())
            {
                break p;
            }
            assert!(
                Instant::now() < deadline,
                "the root never started its subshell"
            );
            aterm_uds::deadline::poll_yield();
        };
        let left = Leftover::new(left);
        let quiet = child.stdin.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        Group {
            root,
            root_serial,
            lstart,
            left,
            _quiet: quiet,
            beat,
        }
    }

    /// A clock that reads the wall time since it was made, as [`proc_tree::table_from`]'s does.
    fn wall() -> impl FnMut() -> Duration {
        let since = Instant::now();
        move || since.elapsed()
    }

    fn proc(pid: i32, ppid: i32, marked: Option<bool>) -> proc_tree::Proc {
        proc_tree::Proc {
            pid,
            ppid,
            serial: Some(u64::try_from(pid).unwrap() * 10),
            sid: None,
            trustd: false,
            marked,
            exe: None,
            at_slot: false,
            in_session: false,
            spawning: false,
            tie: Vec::new(),
        }
    }

    /// `proc` in session `sid`.
    fn in_sid(sid: i32, p: proc_tree::Proc) -> proc_tree::Proc {
        proc_tree::Proc {
            sid: Some(sid),
            ..p
        }
    }

    /// A process table a sweep reads and signals in a test. A process is listed while it
    /// lives; `SIGTERM` or `SIGKILL` ends one that is not `stuck` (`SIGTERM` none that
    /// `ignores_term`), each signal sent as the lane sends it, over the process's current row
    /// ([`proc_tree::send_with`]), and launchd (pid 1) adopts the children of one it ends.
    /// From listing `unlisted_from` on the table cannot be read, and each listing numbered in
    /// `partial` is not whole. Once the listing `becomes` numbers is read, a process reads as
    /// the row it names (one that exec'd, or was reparented). The check's root has pid `root`,
    /// which the sweep holds when `held`. What a sweep keeps goes in `kept`, unless the record
    /// is `unkept`.
    struct Table {
        procs: Vec<(proc_tree::Proc, bool)>,
        stuck: BTreeSet<i32>,
        ignores_term: BTreeSet<i32>,
        becomes: BTreeMap<i32, (usize, proc_tree::Proc)>,
        unlisted_from: Option<usize>,
        partial: BTreeSet<usize>,
        listings: usize,
        sent: Vec<(i32, i32)>,
        root: Option<i32>,
        held: bool,
        kept: BTreeSet<(i32, u64)>,
        unkept: bool,
    }

    impl Table {
        fn of(procs: Vec<proc_tree::Proc>) -> Table {
            Table {
                procs: procs.into_iter().map(|p| (p, true)).collect(),
                stuck: BTreeSet::new(),
                ignores_term: BTreeSet::new(),
                becomes: BTreeMap::new(),
                unlisted_from: None,
                partial: BTreeSet::new(),
                listings: 0,
                sent: Vec::new(),
                root: None,
                held: false,
                kept: BTreeSet::new(),
                unkept: false,
            }
        }

        /// The signals `pid` was sent, in order.
        fn sent_to(&self, pid: i32) -> Vec<i32> {
            self.sent
                .iter()
                .filter(|(p, _)| *p == pid)
                .map(|(_, sig)| *sig)
                .collect()
        }

        fn signalled(&self) -> BTreeSet<i32> {
            self.sent.iter().map(|(pid, _)| *pid).collect()
        }

        /// What `pid` reads as now, as a signal re-reads it.
        fn kin(&self, pid: i32) -> Option<proc_tree::Kin> {
            let (q, _) = self
                .procs
                .iter()
                .find(|(q, alive)| *alive && q.pid == pid)?;
            Some(proc_tree::Kin {
                ppid: q.ppid,
                serial: q.serial?,
                sid: q.sid,
                zombie: false,
                ctty: false,
                trustd: q.trustd,
                forked: q.spawning,
                leader_only: false,
            })
        }

        /// `sig` reaching `pid`.
        fn signal(&mut self, pid: i32, sig: i32) {
            self.sent.push((pid, sig));
            if self.stuck.contains(&pid)
                || (sig == proc_tree::SIGTERM && self.ignores_term.contains(&pid))
            {
                return;
            }
            for (q, alive) in &mut self.procs {
                if q.pid == pid {
                    *alive = false;
                } else if *alive && q.ppid == pid {
                    q.ppid = 1;
                }
            }
        }
    }

    impl proc_tree::Procs for Table {
        fn session(&mut self, listed: &[proc_tree::Proc]) -> Option<i32> {
            let (root, held) = (self.root, self.held);
            proc_tree::proven_session(listed, root, held, &mut |p| self.alive(p))
        }

        fn table(&mut self) -> Option<proc_tree::Listing> {
            let n = self.listings;
            self.listings += 1;
            if self.unlisted_from.is_some_and(|from| n >= from) {
                return None;
            }
            let listing = proc_tree::Listing {
                procs: self
                    .procs
                    .iter()
                    .filter(|(_, alive)| *alive)
                    .map(|(p, _)| p.clone())
                    .collect(),
                whole: !self.partial.contains(&n),
            };
            for (pid, (from, row)) in &self.becomes {
                if n >= *from {
                    for (q, _) in &mut self.procs {
                        if q.pid == *pid {
                            *q = row.clone();
                        }
                    }
                }
            }
            Some(listing)
        }

        fn alive(&mut self, p: &proc_tree::Proc) -> bool {
            p.serial.is_some()
                && self
                    .procs
                    .iter()
                    .any(|(q, alive)| *alive && q.pid == p.pid && q.serial == p.serial)
        }

        fn keep(&mut self, procs: &[proc_tree::Proc]) -> bool {
            if self.unkept {
                return false;
            }
            let reached = proc_tree::send_with(procs, 0, &mut |pid| self.kin(pid), &mut |_, _| {});
            self.kept.extend(reached);
            true
        }

        fn send(&mut self, procs: &[proc_tree::Proc], sig: i32) -> Vec<(i32, u64)> {
            let table = RefCell::new(self);
            proc_tree::send_with(
                procs,
                sig,
                &mut |pid| table.borrow().kin(pid),
                &mut |pid, sig| table.borrow_mut().signal(pid, sig),
            )
        }

        /// A listed process ends only as [`Table::signal`] ends it, never with time, so a
        /// wait would only outlast one the test holds ([`Table::stuck`]).
        fn settle_within(&self) -> Duration {
            Duration::ZERO
        }
    }

    /// A check's processes are those that carry its mark and all they started, by parent,
    /// whatever their group or mark; never a `trustd` or what it runs, though they carry it,
    /// and never a stranger.
    #[test]
    fn a_checks_processes_are_what_carries_its_mark_and_all_it_started() {
        let mut trustd = proc(300, 200, Some(true));
        trustd.trustd = true;
        let procs = [
            proc(200, 1, Some(true)),
            proc(201, 200, Some(false)),
            proc(202, 201, None),
            trustd,
            proc(301, 300, Some(true)),
            proc(302, 301, None),
            proc(400, 1, Some(true)),
            proc(500, 1, Some(false)),
            proc(501, 500, None),
            proc(502, 0, Some(false)),
        ];
        let mut got: Vec<i32> = proc_tree::marked(&procs, None, &BTreeSet::new())
            .iter()
            .map(|p| p.pid)
            .collect();
        got.sort_unstable();
        assert_eq!(got, [200, 201, 202, 400]);
    }

    /// What an unreadable process left in the root's session is the check's, with all it
    /// started and never a `trustd` or what it runs, while the session is proven: by the held
    /// root, or by a carrier of the mark still in it after the listing.
    #[test]
    fn a_checks_session_is_its_own_while_that_is_proven() {
        let withheld = |pid, ppid| proc_tree::Proc {
            exe: Some("/bin/sh".into()),
            ..proc(pid, ppid, None)
        };
        let mut trustd = in_sid(200, withheld(300, 1));
        trustd.trustd = true;
        let carrier = in_sid(200, proc(400, 1, Some(true)));
        let rest = [
            in_sid(200, withheld(500, 1)),
            withheld(501, 500),
            trustd,
            withheld(301, 300),
            in_sid(900, proc(600, 1, Some(false))),
        ];
        let session: Vec<proc_tree::Proc> = std::iter::once(carrier.clone())
            .chain(rest.iter().cloned())
            .collect();
        let mut got: Vec<i32> = proc_tree::marked(&session, Some(200), &BTreeSet::new())
            .iter()
            .map(|p| p.pid)
            .collect();
        got.sort_unstable();
        assert_eq!(
            got,
            [400, 500, 501],
            "the proven session and all it started"
        );
        let mut got: Vec<i32> = proc_tree::marked(&session, None, &BTreeSet::new())
            .iter()
            .map(|p| p.pid)
            .collect();
        got.sort_unstable();
        assert_eq!(got, [400], "an unproven session adds nothing");

        let root = in_sid(200, proc(200, 1, Some(true)));
        let stranger = in_sid(200, proc(700, 1, Some(false)));
        let elsewhere = in_sid(900, proc(800, 1, Some(true)));
        let proven = |listed: &[proc_tree::Proc], held, still: bool| {
            proc_tree::proven_session(listed, Some(200), held, &mut |_| still)
        };
        assert_eq!(proven(&[], true, false), Some(200), "held");
        assert_eq!(proven(std::slice::from_ref(&root), false, true), Some(200));
        assert_eq!(
            proven(std::slice::from_ref(&carrier), false, true),
            Some(200)
        );
        assert_eq!(
            proven(std::slice::from_ref(&carrier), false, false),
            None,
            "a witness gone or out of the session after the listing proves nothing"
        );
        assert_eq!(proven(&[stranger, elsewhere], false, true), None);
        assert_eq!(
            proc_tree::proven_session(&[root], None, true, &mut |_| true),
            None,
            "no root, no session"
        );

        let mut table = Table::of(session.clone());
        table.root = Some(200);
        assert_eq!(
            proc_tree::end_marked_with(
                &mut table,
                Path::new("/nonexistent/root/j0"),
                proc_tree::Act::End
            ),
            proc_tree::Left::Ended
        );
        assert_eq!(table.signalled(), BTreeSet::from([400, 500, 501]));
        let mut unproven = Table::of(session[1..].to_vec());
        unproven.root = Some(200);
        assert_eq!(
            proc_tree::end_marked_with(
                &mut unproven,
                Path::new("/nonexistent/root/j0"),
                proc_tree::Act::End
            ),
            proc_tree::Left::Nothing
        );
        assert!(unproven.sent.is_empty(), "{:?}", unproven.sent);
        let mut held = Table::of(session[1..].to_vec());
        (held.root, held.held) = (Some(200), true);
        assert_eq!(
            proc_tree::end_marked_with(
                &mut held,
                Path::new("/nonexistent/root/j0"),
                proc_tree::Act::End
            ),
            proc_tree::Left::Ended
        );
        assert_eq!(held.signalled(), BTreeSet::from([500, 501]));
    }

    /// A process whose mark could not be read holds the slot only while it runs from the slot
    /// or, on macOS, its executable is unread too and a carrier of the mark started it; a
    /// stranger's (Linux's `ssh-agent`) never does. A member of the root's session counts
    /// unless a `trustd` runs it.
    #[test]
    fn an_unread_process_counts_only_while_it_is_tied_to_the_check() {
        let slot = Path::new("/nonexistent/root/j0");
        let unread = |exe: Option<&str>, ppid| proc_tree::Proc {
            exe: exe.map(PathBuf::from),
            ..proc(700, ppid, None)
        };
        let member = |at_slot, in_session| proc_tree::Proc {
            at_slot,
            in_session,
            ..proc(701, 1, Some(false))
        };
        assert!(
            proc_tree::unread(&[member(true, false)], slot),
            "a member of the root's session using the slot counts, though it reads"
        );
        assert!(
            proc_tree::unread(&[member(false, true)], slot),
            "a member of the root's own session counts wherever it works"
        );
        let mut trustd = proc(600, 1, Some(true));
        trustd.trustd = true;
        let spared = proc_tree::Proc {
            in_session: true,
            ..proc(601, 600, Some(false))
        };
        assert!(
            !proc_tree::unread(&[trustd.clone(), spared], slot),
            "what a trustd runs never counts"
        );
        let in_slot = "/nonexistent/root/j0/debug/build/x/build-script-build";
        let first_exec = cfg!(not(target_os = "linux"));
        for (exe, parent, counts) in [
            (Some("/usr/bin/sudo"), None, false),
            (Some("/nonexistent/root/j01/gen"), None, false),
            (Some(in_slot), None, true),
            (None, None, false),
            (None, Some(proc(600, 1, Some(true))), first_exec),
            (None, Some(trustd.clone()), false),
            (Some("/usr/bin/sudo"), Some(proc(600, 1, Some(true))), false),
        ] {
            let ppid = parent.as_ref().map_or(1, |p| p.pid);
            let procs: Vec<proc_tree::Proc> = [proc(500, 1, Some(false)), unread(exe, ppid)]
                .into_iter()
                .chain(parent.clone())
                .collect();
            assert_eq!(
                proc_tree::unread(&procs, slot),
                counts,
                "{exe:?} started by {parent:?}"
            );
            if parent.is_none() {
                let mut table = Table::of(procs.clone());
                let want = if counts {
                    proc_tree::Left::Unread
                } else {
                    proc_tree::Left::Nothing
                };
                assert_eq!(
                    proc_tree::end_marked_with(&mut table, slot, proc_tree::Act::End),
                    want,
                    "{exe:?}"
                );
                assert!(table.sent.is_empty(), "{:?}", table.sent);
            }
        }
    }

    /// A sweep signals only the check's processes, and only with `SIGTERM`, then `SIGKILL` for
    /// survivors. It says what is left: ended only by a whole listing after the signals, and
    /// unread, with nothing signalled, when it cannot list at all. A watch signals nothing.
    #[test]
    fn a_sweep_ends_the_checks_processes_and_says_what_is_left() {
        let slot = Path::new("/nonexistent/root/j0");
        let procs = vec![
            proc(200, 1, Some(true)),
            proc(201, 200, Some(false)),
            proc(400, 1, Some(true)),
            proc(500, 1, Some(false)),
            proc_tree::Proc {
                exe: Some("/usr/bin/less".into()),
                ..proc(501, 500, None)
            },
        ];
        let check = BTreeSet::from([200, 201, 400]);
        let ends_only = |sent: &[(i32, i32)]| {
            sent.iter()
                .all(|(_, sig)| [proc_tree::SIGTERM, proc_tree::SIGKILL].contains(sig))
        };

        let mut table = Table::of(procs.clone());
        assert_eq!(
            proc_tree::end_marked_with(&mut table, slot, proc_tree::Act::End),
            proc_tree::Left::Ended
        );
        assert_eq!(table.signalled(), check);
        for pid in check.iter().copied() {
            assert_eq!(table.sent_to(pid), [proc_tree::SIGTERM], "{pid}");
        }
        assert!(
            table.sent.iter().position(|(pid, _)| *pid == 201)
                < table.sent.iter().position(|(pid, _)| *pid == 200),
            "a child is signalled before its parent: {:?}",
            table.sent
        );

        let mut watched = Table::of(procs.clone());
        assert_eq!(
            proc_tree::end_marked_with(&mut watched, slot, proc_tree::Act::Watch),
            proc_tree::Left::Lives
        );
        assert!(watched.sent.is_empty(), "{:?}", watched.sent);

        let mut stuck = Table::of(procs.clone());
        stuck.stuck.insert(400);
        assert_eq!(
            proc_tree::end_marked_with(&mut stuck, slot, proc_tree::Act::End),
            proc_tree::Left::Lives
        );
        assert!(
            stuck.sent.contains(&(400, proc_tree::SIGKILL)),
            "{:?}",
            stuck.sent
        );
        assert_eq!(stuck.signalled(), check);
        assert!(ends_only(&stuck.sent), "{:?}", stuck.sent);

        let mut blind = Table::of(procs.clone());
        blind.unlisted_from = Some(1);
        assert_eq!(
            proc_tree::end_marked_with(&mut blind, slot, proc_tree::Act::End),
            proc_tree::Left::Lives,
            "a table that cannot be read after the signals is no proof that nothing is left"
        );
        assert_eq!(blind.signalled(), check);

        let mut partial = Table::of(procs.clone());
        partial.partial.insert(1);
        assert_eq!(
            proc_tree::end_marked_with(&mut partial, slot, proc_tree::Act::End),
            proc_tree::Left::Lives,
            "a last listing that is not whole is no proof that nothing is left"
        );
        assert_eq!(partial.signalled(), check);

        let mut unread = Table::of(procs);
        unread.unlisted_from = Some(0);
        assert_eq!(
            proc_tree::end_marked_with(&mut unread, slot, proc_tree::Act::End),
            proc_tree::Left::Unread
        );
        assert!(unread.sent.is_empty());

        for act in [proc_tree::Act::End, proc_tree::Act::Watch] {
            let mut none = Table::of(vec![proc(500, 1, Some(false))]);
            none.partial.insert(0);
            assert_eq!(
                proc_tree::end_marked_with(&mut none, slot, act),
                proc_tree::Left::Unread,
                "a listing that is not whole is no proof that nothing of the check runs"
            );
            assert!(none.sent.is_empty());
        }
    }

    /// A process listed before it exec'd a `trustd` reads as one at its signal, so it and what
    /// it runs are spared while the rest of the check is ended.
    #[test]
    fn a_process_that_became_a_trustd_since_it_was_listed_is_never_signalled() {
        let slot = Path::new("/nonexistent/root/j0");
        let listed = in_sid(200, proc(300, 200, Some(true)));
        let mut table = Table::of(vec![
            in_sid(200, proc(200, 1, Some(true))),
            listed.clone(),
            in_sid(200, proc(301, 300, None)),
        ]);
        table.root = Some(200);
        let trustd = proc_tree::Proc {
            trustd: true,
            ..listed
        };
        table.becomes.insert(300, (0, trustd));
        assert_eq!(
            proc_tree::end_marked_with(&mut table, slot, proc_tree::Act::End),
            proc_tree::Left::Ended
        );
        assert_eq!(table.signalled(), BTreeSet::from([200]), "{:?}", table.sent);
    }

    /// A `posix_spawn` child whose image is not in yet is left to a later listing: read as a
    /// `trustd` it is spared, read as the check's it is killed, and while still mid-spawn the
    /// check lives.
    #[test]
    fn a_row_caught_mid_spawn_is_never_signalled() {
        let slot = Path::new("/nonexistent/root/j0");
        let spawning = proc_tree::Proc {
            exe: Some("/nonexistent/targo".into()),
            spawning: true,
            ..in_sid(200, proc(300, 200, None))
        };
        let became = |trustd| proc_tree::Proc {
            exe: None,
            spawning: false,
            trustd,
            marked: Some(!trustd),
            ..spawning.clone()
        };
        for (what, becomes, left, sent) in [
            (
                "a trustd",
                Some(became(true)),
                proc_tree::Left::Ended,
                vec![],
            ),
            (
                "the check's",
                Some(became(false)),
                proc_tree::Left::Ended,
                vec![proc_tree::SIGKILL],
            ),
            ("still mid-spawn", None, proc_tree::Left::Lives, vec![]),
        ] {
            let mut table = Table::of(vec![
                in_sid(200, proc(200, 1, Some(true))),
                spawning.clone(),
            ]);
            (table.root, table.held) = (Some(200), true);
            if let Some(row) = becomes {
                table.becomes.insert(300, (1, row));
            }
            assert_eq!(
                proc_tree::end_marked_with(&mut table, slot, proc_tree::Act::End),
                left,
                "{what}"
            );
            assert_eq!(table.sent_to(300), sent, "{what}");
            assert_eq!(table.sent_to(200), [proc_tree::SIGTERM], "{what}");
        }
    }

    /// A stranger's child listed under a parent pid since given to one of the check's reads
    /// launchd as its parent at its signal, so it is sent nothing.
    #[test]
    fn a_child_tied_to_the_check_by_a_stale_parent_is_never_signalled() {
        let slot = Path::new("/nonexistent/root/j0");
        let child = in_sid(500, proc(301, 300, Some(false)));
        let mut table = Table::of(vec![in_sid(900, proc(300, 1, Some(true))), child.clone()]);
        table
            .becomes
            .insert(301, (0, proc_tree::Proc { ppid: 1, ..child }));
        assert_eq!(
            proc_tree::end_marked_with(&mut table, slot, proc_tree::Act::End),
            proc_tree::Left::Ended
        );
        assert_eq!(table.signalled(), BTreeSet::from([300]), "{:?}", table.sent);
    }

    /// A child tied through a parent whose pid passed to a stranger's process finds another
    /// serial at its signal, so it and what it started, at any depth, are sent nothing; nor are
    /// a child read under a stranger's pid that then passed to one of the check's, which reads
    /// its reaper as its parent by then, and what it started.
    #[test]
    fn a_child_tied_by_a_parent_whose_pid_passed_to_another_is_never_signalled() {
        let slot = Path::new("/nonexistent/root/j0");
        let stranger = proc_tree::Proc {
            serial: Some(9999),
            ..proc(300, 1, Some(false))
        };
        let tree = |depth: i32| -> Vec<proc_tree::Proc> {
            std::iter::once(proc(300, 1, Some(true)))
                .chain((301..=300 + depth).map(|pid| proc(pid, pid - 1, Some(false))))
                .collect()
        };
        for (depth, ignores_term) in [(1, false), (2, false), (3, true)] {
            let mut table = Table::of(tree(depth));
            table.becomes.insert(300, (0, stranger.clone()));
            if ignores_term {
                table.ignores_term.extend([302, 303]);
            }
            assert_eq!(
                proc_tree::end_marked_with(&mut table, slot, proc_tree::Act::End),
                proc_tree::Left::Ended,
                "depth {depth}"
            );
            assert!(table.sent.is_empty(), "depth {depth}: {:?}", table.sent);
            assert!(table.kept.is_empty(), "depth {depth}: {:?}", table.kept);
        }
        let mut table = Table::of(tree(2));
        let reparented = proc_tree::Proc {
            ppid: 1,
            ..proc(301, 300, Some(false))
        };
        table.becomes.insert(301, (0, reparented));
        assert_eq!(
            proc_tree::end_marked_with(&mut table, slot, proc_tree::Act::End),
            proc_tree::Left::Ended
        );
        assert_eq!(table.signalled(), BTreeSet::from([300]), "{:?}", table.sent);
    }

    /// A process tied only by a session or parent the `SIGTERM` ended is still the check's, the
    /// `SIGTERM` having reached it, and is killed; so is a row caught mid-spawn that the read
    /// before the `SIGTERM` showed the check's.
    #[test]
    fn what_outlives_the_sigterm_is_killed_though_the_sigterm_ended_its_ties() {
        let slot = Path::new("/nonexistent/root/j0");
        let root = in_sid(200, proc(200, 1, Some(true)));
        let carrier = in_sid(200, proc(400, 200, Some(true)));
        let spawning = proc_tree::Proc {
            exe: Some("/nonexistent/targo".into()),
            in_session: true,
            spawning: true,
            ..in_sid(200, proc(500, 400, None))
        };
        let mut table = Table::of(vec![root.clone(), carrier.clone(), spawning.clone()]);
        table.root = Some(200);
        let became = proc_tree::Proc {
            ppid: 1,
            exe: Some("/bin/sh".into()),
            spawning: false,
            ..spawning
        };
        table.becomes.insert(500, (1, became));
        assert_eq!(
            proc_tree::end_marked_with(&mut table, slot, proc_tree::Act::End),
            proc_tree::Left::Ended,
            "caught mid-spawn"
        );
        assert_eq!(table.sent_to(500), [proc_tree::SIGKILL], "caught mid-spawn");
        for (how, sid, in_session, held) in [
            ("its session's member", 200, true, false),
            ("in its session, read as a stranger's", 200, false, false),
            ("its parent's child, in another session", 900, false, true),
        ] {
            let survivor = proc_tree::Proc {
                exe: Some("/bin/sh".into()),
                in_session,
                ..in_sid(sid, proc(500, 400, None))
            };
            let mut table = Table::of(vec![root.clone(), carrier.clone(), survivor]);
            (table.root, table.held) = (Some(200), held);
            table.ignores_term.insert(500);
            assert_eq!(
                proc_tree::end_marked_with(&mut table, slot, proc_tree::Act::End),
                proc_tree::Left::Ended,
                "{how}"
            );
            assert_eq!(
                table.sent_to(500),
                [proc_tree::SIGTERM, proc_tree::SIGKILL],
                "{how}"
            );
        }
    }

    /// Each process a sweep signals is kept in the record first; when the record cannot keep
    /// them, the first signal is `SIGKILL`.
    #[test]
    fn a_sweep_keeps_what_it_signals_and_sends_no_sigterm_it_could_not_keep() {
        let slot = Path::new("/nonexistent/root/j0");
        let root = in_sid(200, proc(200, 1, Some(true)));
        let survivor = proc_tree::Proc {
            exe: Some("/bin/sh".into()),
            in_session: true,
            ..in_sid(200, proc(500, 200, None))
        };
        for (unkept, sent) in [
            (false, vec![proc_tree::SIGTERM, proc_tree::SIGKILL]),
            (true, vec![proc_tree::SIGKILL]),
        ] {
            let mut table = Table::of(vec![root.clone(), survivor.clone()]);
            table.root = Some(200);
            table.ignores_term.insert(500);
            table.unkept = unkept;
            assert_eq!(
                proc_tree::end_marked_with(&mut table, slot, proc_tree::Act::End),
                proc_tree::Left::Ended,
                "unkept: {unkept}"
            );
            assert_eq!(table.sent_to(500), sent, "unkept: {unkept}");
            assert_eq!(table.sent_to(200), sent[..1], "unkept: {unkept}");
            if !unkept {
                assert_eq!(table.kept, BTreeSet::from([(200, 2000), (500, 5000)]));
            }
        }
    }

    /// A fork that ran (its parent's thread) and a program held at its first exec (it exec'd)
    /// are not mid-spawn; only a `posix_spawn` child whose image is not in yet is.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn neither_a_fork_nor_a_program_held_at_its_exec_reads_as_mid_spawn() {
        let d = tmp("trust-lane-spawning");
        let g = rootless_group(d.path(), None, None, "");
        let mut cmd = command("/bin/sleep");
        cmd.arg("600").stdin(Stdio::null()).start_suspended(true);
        let mut held = cmd.spawn().unwrap();
        let mark = proc_tree::Mark {
            nonce: Some(nonce()),
            root: None,
            kept: BTreeSet::new(),
        };
        let rows = proc_tree::table(&mark, None, &d.path().join("j0")).map(|t| t.procs);
        let _ = held.kill();
        let _ = held.wait();
        let row = |pid: u32| {
            rows.iter()
                .flatten()
                .find(|p| p.pid == i32::try_from(pid).unwrap())
                .cloned()
        };
        let fork = row(g.left.pid);
        assert!(
            fork.as_ref().is_some_and(|p| !p.spawning),
            "a fork that ran: {fork:?}"
        );
        let suspended = row(held.id());
        assert!(
            suspended.as_ref().is_some_and(|p| !p.spawning),
            "a program held at its exec: {suspended:?}"
        );
    }

    /// No sweep stops a process, so a lane killed mid-sweep leaves each of the check's
    /// processes running or ended, never stopped (a stopped trust worker would keep its
    /// `trustd` memory grant).
    #[test]
    fn a_sweep_cut_short_leaves_nothing_stopped() {
        /// The kernel's reads and signals, but the lane dies after its first send.
        struct CutShort<'a>(proc_tree::Live<'a>);
        impl proc_tree::Procs for CutShort<'_> {
            fn table(&mut self) -> Option<proc_tree::Listing> {
                self.0.table()
            }
            fn session(&mut self, listed: &[proc_tree::Proc]) -> Option<i32> {
                self.0.session(listed)
            }
            fn alive(&mut self, p: &proc_tree::Proc) -> bool {
                self.0.alive(p)
            }
            fn keep(&mut self, procs: &[proc_tree::Proc]) -> bool {
                self.0.keep(procs)
            }
            fn send(&mut self, procs: &[proc_tree::Proc], sig: i32) -> Vec<(i32, u64)> {
                self.0.send(procs, sig);
                panic!("the lane died after its first signal");
            }
        }
        let d = tmp("trust-lane-cut-short");
        let slot = d.path().join("j0");
        let nonce = nonce();
        let g = rootless_group(d.path(), Some(&nonce), None, "trap '' TERM; ");
        let root = Some((i32::try_from(g.root).unwrap(), g.root_serial));
        let record = d.path().join(".check.0");
        fs::write(&record, record_at(&nonce, root, d.path())).unwrap();
        let mark = proc_tree::Mark {
            nonce: Some(nonce),
            root,
            kept: BTreeSet::new(),
        };
        let live = proc_tree::Live {
            mark: &mark,
            held: None,
            slot: &slot,
            record: &record,
        };
        let cut = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            proc_tree::end_marked_with(&mut CutShort(live), &slot, proc_tree::Act::End)
        }));
        assert!(cut.is_err(), "the sweep never signalled the check: {cut:?}");
        assert!(
            g.beats_on() && g.left.running(),
            "a sweep cut short left the check's process stopped"
        );
    }

    /// A pid holding another process than the one listed, a row with no serial, and one under
    /// another parent than it has now are never signalled; the child's echo of a line written
    /// after those sends proves it was spared.
    #[test]
    fn a_pid_holding_another_process_than_the_one_listed_is_never_signalled() {
        use proc_tree::Procs as _;
        let mut cat = command(readable("/bin/cat"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = cat.id();
        let serial = serial_of(pid);
        let parent = i32::try_from(std::process::id()).unwrap();
        let listed = |serial, ppid| proc_tree::Proc {
            serial,
            ..proc(i32::try_from(pid).unwrap(), ppid, Some(true))
        };
        let mark = proc_tree::Mark {
            nonce: Some(nonce()),
            root: None,
            kept: BTreeSet::new(),
        };
        let mut live = proc_tree::Live {
            mark: &mark,
            held: None,
            slot: Path::new("/nonexistent/root/j0"),
            record: Path::new("/nonexistent/root/.check.0"),
        };
        live.send(
            &[
                listed(Some(serial + 1), parent),
                listed(None, parent),
                listed(Some(serial), 1),
            ],
            proc_tree::SIGKILL,
        );
        let input = aterm_uds::deadline::cat_echoes(&mut cat);
        live.send(&[listed(Some(serial), parent)], proc_tree::SIGKILL);
        let deadline = Instant::now() + Duration::from_secs(60);
        let killed = loop {
            if let Some(status) = cat.try_wait().unwrap() {
                break status.code().is_none();
            }
            if Instant::now() >= deadline {
                let _ = cat.kill();
                let _ = cat.wait();
                break false;
            }
            aterm_uds::deadline::poll_yield();
        };
        drop(input);
        assert!(killed, "the process listed was not signalled");
    }

    /// A process that reads as a `trustd` at the send is sent nothing; its echo of a line
    /// written after the sends proves it was spared.
    #[test]
    fn a_process_that_reads_as_a_trustd_at_the_send_is_never_killed() {
        use proc_tree::Procs as _;
        let mut d = aterm_uds::deadline::KeptUntilRan::new(tmp("trust-lane-trustd").keep());
        let exe = d.path().join("trustd");
        aterm_uds::deadline::ad_hoc_copy(Path::new("/bin/cat"), &exe);
        let mut trustd = command(&exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = trustd.id();
        let parent = i32::try_from(std::process::id()).unwrap();
        let listed = proc_tree::Proc {
            serial: Some(serial_of(pid)),
            ..in_sid(1, proc(i32::try_from(pid).unwrap(), parent, Some(true)))
        };
        let mark = proc_tree::Mark {
            nonce: Some(nonce()),
            root: None,
            kept: BTreeSet::new(),
        };
        let mut live = proc_tree::Live {
            mark: &mark,
            held: None,
            slot: Path::new("/nonexistent/root/j0"),
            record: Path::new("/nonexistent/root/.check.0"),
        };
        for sig in [proc_tree::SIGTERM, proc_tree::SIGKILL] {
            live.send(std::slice::from_ref(&listed), sig);
        }
        let input = aterm_uds::deadline::cat_echoes(&mut trustd);
        d.ran();
        drop(input);
        let status = trustd.wait().unwrap();
        assert_eq!(status.code(), Some(0), "the trustd ended by a signal");
    }

    /// A record reads back as written, its lane and root directory with it, the root's words
    /// once it is spawned; an earlier build's (`<pid> <ps start>`), or one with a nonce of
    /// another shape, names no mark, and a lane or directory of another shape is none.
    #[test]
    fn a_check_record_reads_back_as_written() {
        let rec = CheckRecord {
            nonce: Some(nonce()),
            boot: Some("b007".into()),
            lane: Some((4141, 66)),
            dir: Some((16_777_234, 123_456_789)),
            root: Some((4242, 77)),
            kept: BTreeSet::from([(4343, 88), (4444, 99)]),
        };
        assert_eq!(CheckRecord::parse(&rec.text()), rec);
        let bare = CheckRecord {
            root: None,
            boot: None,
            lane: None,
            dir: None,
            kept: BTreeSet::new(),
            ..rec.clone()
        };
        assert_eq!(CheckRecord::parse(&bare.text()), bare);
        for earlier in [
            "4242 Fri Oct 23 09:30:33 2026\n",
            "4242 \n",
            "unreadable\n",
            "",
            "nonce=0f1e boot=b007 pid=4242 serial=77\n",
            "nonce=- boot=-\n",
        ] {
            assert_eq!(CheckRecord::parse(earlier).nonce, None, "{earlier:?}");
        }
        assert_eq!(earlier_pid("4242 Fri Oct 23 09:30:33 2026\n"), Some(4242));
        assert_eq!(earlier_pid(&rec.text()), None);
        for odd in [
            "lane=4141 dir=1 kept=4343",
            "lane=1:66 dir=a:2 kept=1:88",
            "lane=x:1 dir=1:2:3 kept=x:88",
        ] {
            let rec = CheckRecord::parse(odd);
            assert_eq!((rec.lane, rec.dir), (None, None), "{odd}");
            assert!(rec.kept.is_empty(), "{odd}");
        }
    }

    /// Another boot's record goes at once, stamping nothing; one whose lane runs is left alone,
    /// unstamped; a copy's is never swept and goes once its first stamp is spent; one with no
    /// mark or lane only watches. One naming its mark, a gone lane or this run, and this root is
    /// judged by its sweep: an unread sweep keeps it whatever the stamp, and any other stamps
    /// the slot from then and takes the record once nothing is left.
    #[test]
    fn each_record_is_judged_by_its_boot_its_lane_its_root_and_its_mark() {
        use proc_tree::{Act, Left};
        /// What a record is, its text, how its sweep acts and what it finds, and whether its
        /// slot starts with a spent stamp.
        type Case = (&'static str, String, Option<(Act, Left)>, bool);
        let d = tmp("trust-lane-records");
        let root = d.path().join("root");
        owner_dirs(&root).unwrap();
        let stamp = |n: usize| {
            root.join(format!("j{n}"))
                .join(aterm_uds::deadline::EXEC_STAMP)
        };
        let spent = u64::MAX.to_string();
        let lane = Leftover::new(
            command("/bin/sleep")
                .arg("600")
                .stdin(Stdio::null())
                .spawn()
                .unwrap()
                .id(),
        );
        let record = |boot: Option<&str>, lane, dir| {
            CheckRecord {
                nonce: Some(nonce()),
                boot: boot.map(str::to_owned),
                lane,
                dir,
                root: Some((4242, 77)),
                kept: BTreeSet::new(),
            }
            .text()
        };
        let here = dir_id(&root);
        let gone = Some(GONE_LANE);
        let cases: [Case; 10] = [
            (
                "another boot",
                record(Some("then"), gone, here),
                None,
                false,
            ),
            (
                "no mark, its root gone",
                "2147483647 Fri Oct 23 09:30:33 2026\n".into(),
                Some((Act::Watch, Left::Nothing)),
                false,
            ),
            (
                "unread",
                record(Some("now"), gone, here),
                Some((Act::End, Left::Unread)),
                true,
            ),
            (
                "no boot read",
                record(None, gone, here),
                Some((Act::End, Left::Lives)),
                true,
            ),
            (
                "nothing left",
                record(Some("now"), gone, here),
                Some((Act::End, Left::Nothing)),
                true,
            ),
            (
                "ended",
                record(Some("now"), gone, here),
                Some((Act::End, Left::Ended)),
                true,
            ),
            (
                "a copy's",
                record(Some("now"), gone, dir_id(d.path())),
                None,
                false,
            ),
            (
                "its lane runs",
                record(
                    Some("now"),
                    Some((i32::try_from(lane.pid).unwrap(), lane.serial)),
                    here,
                ),
                None,
                true,
            ),
            (
                "no lane named",
                record(Some("now"), None, here),
                Some((Act::Watch, Left::Lives)),
                false,
            ),
            (
                "this run's own",
                record(Some("now"), this_lane(), here),
                Some((Act::End, Left::Nothing)),
                true,
            ),
        ];
        for (n, (_, text, _, stamped)) in cases.iter().enumerate() {
            owner_dirs(&root.join(format!("j{n}"))).unwrap();
            fs::write(root.join(format!(".check.{n}")), text).unwrap();
            if *stamped {
                fs::write(stamp(n), &spent).unwrap();
            }
        }
        let mut swept = Vec::new();
        let mut first = |mark: &proc_tree::Mark, slot: &Path, _: &Path, act: Act| {
            let n: usize = slot_number(&slot.file_name().unwrap().to_string_lossy())
                .unwrap()
                .parse()
                .unwrap();
            let root = if n == 1 {
                (i32::MAX, NO_SERIAL)
            } else {
                (4242, 77)
            };
            assert_eq!(mark.root, Some(root), "{}", cases[n].0);
            assert_eq!(mark.nonce.is_some(), n != 1, "{}", cases[n].0);
            swept.push((n, act));
            let (want, left) = cases[n].2.unwrap();
            assert_eq!(act, want, "{}", cases[n].0);
            left
        };
        reap_stale_checks_with(&root, Some("now"), &mut first);
        swept.sort_unstable_by_key(|(n, _)| *n);
        assert_eq!(
            swept.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 8, 9],
            "a record of this boot, this root and no live run is swept"
        );
        let kept = |n: usize| root.join(format!(".check.{n}")).exists();
        let stamp_of = |n: usize| fs::read_to_string(stamp(n)).ok();
        assert!(
            !kept(0) && stamp_of(0).is_none(),
            "another boot's goes, stamping nothing"
        );
        for (n, record, fresh) in [
            (1, false, true),
            (2, true, false),
            (3, true, true),
            (4, false, true),
            (5, false, true),
            (6, true, true),
            (7, true, false),
            (8, true, true),
            (9, false, true),
        ] {
            assert_eq!(kept(n), record, "{}", cases[n].0);
            assert_eq!(
                stamp_of(n).is_some_and(|s| s != spent),
                fresh,
                "{}: the slot held from now",
                cases[n].0
            );
        }

        let first_stamps: Vec<Option<String>> = (0..cases.len()).map(stamp_of).collect();
        fs::write(stamp(8), &spent).unwrap();
        reap_stale_checks_with(&root, Some("now"), &mut |_, _, _, _| Left::Unread);
        for n in [6, 7] {
            assert!(kept(n), "{} stays while its first stamp holds", cases[n].0);
            assert_eq!(stamp_of(n), first_stamps[n], "{}: unrenewed", cases[n].0);
        }
        assert!(
            kept(8) && stamp_of(8).is_some_and(|s| s != spent),
            "a watch that could not read keeps the record, the slot held from that reap"
        );
        for n in [6, 8] {
            fs::write(stamp(n), &spent).unwrap();
        }
        reap_stale_checks_with(&root, Some("now"), &mut |_, _, _, _| Left::Nothing);
        assert!(
            !kept(6),
            "a copy's record goes once its first stamp is spent"
        );
        assert!(
            !kept(8) && stamp_of(8).is_some_and(|s| s != spent),
            "a watch that finds nothing takes its record, the slot held from that reap"
        );
        assert!(kept(7), "a live run's record stays");
        assert_eq!(stamp_of(7).as_deref(), Some(spent.as_str()), "unstamped");
        let _ = command("/bin/kill")
            .args(["-KILL", &lane.pid.to_string()])
            .status();
        assert!(ends(lane.pid, lane.serial));
        reap_stale_checks_with(&root, Some("now"), &mut |_, _, _, _| Left::Nothing);
        assert!(!kept(7), "once its run is gone, a reap judges it");
    }

    /// A check whose root a signal ended, not the lane, never showed what ran in its slot
    /// ran: what carries its mark is ended and the slot is held.
    #[test]
    fn a_check_a_signal_ended_ends_what_it_left_and_holds_its_slot() {
        let d = tmp("trust-lane-signalled");
        let pidfile = d.path().join("left.pid");
        let nonce = nonce();
        let mut cmd = command("/bin/sh");
        cmd.arg("-c")
            .arg("\"$2\" 600 & echo $! > \"$1\"; wait")
            .arg("sh")
            .arg(&pidfile)
            .arg(readable("/bin/sleep"))
            .env(CHECK_VAR, &nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let root = child.id();
        let mark = proc_tree::Mark {
            nonce: Some(nonce),
            root: Some((i32::try_from(root).unwrap(), serial_of(root))),
            kept: BTreeSet::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        let left: u32 = loop {
            if let Some(p) = fs::read_to_string(&pidfile)
                .ok()
                .and_then(|t| t.trim().parse().ok())
            {
                break p;
            }
            assert!(
                Instant::now() < deadline,
                "the root never started its child"
            );
            aterm_uds::deadline::poll_yield();
        };
        let left = Leftover::new(left);
        assert!(
            runs_from(left.pid, &readable("/bin/sleep")),
            "the root's child never ran its program"
        );
        child.kill().unwrap();
        let target = d.path().join("j0");
        owner_dirs(&target).unwrap();
        let record = d.path().join(".check.0");
        fs::write(&record, "the lane's own\n").unwrap();
        let mut check = RealCheck {
            child,
            record: record.clone(),
            target: target.clone(),
            mark,
            ended: None,
            times: TreeTimes::default(),
            read_at: None,
            sweep: || None,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        let ended = loop {
            if let Some(ended) = check.poll().unwrap() {
                break ended;
            }
            assert!(
                Instant::now() < deadline,
                "the killed root was never reaped"
            );
            aterm_uds::deadline::poll_yield();
        };
        assert_eq!(ended.code, 128 + proc_tree::SIGKILL);
        let held = held_slots(d.path());
        assert!(
            ends(left.pid, left.serial),
            "what the killed root started outlived it"
        );
        assert_eq!(held, [target]);
        reap_until_gone(
            d.path(),
            &record,
            "the record of what was ended stayed",
            || {},
        );
    }

    /// What a gone root left carries its mark wherever it works and whatever adopted it, so
    /// the reap ends it and holds the slot.
    #[test]
    fn what_a_gone_root_left_carrying_its_mark_is_ended_by_the_reap() {
        let d = tmp("trust-lane-rootless");
        let root = d.path().join("root");
        let slot = root.join("j0");
        owner_dirs(&slot).unwrap();
        let nonce = nonce();
        let g = rootless_group(d.path(), Some(&nonce), None, "");
        let record = root.join(".check.0");
        let named = Some((i32::try_from(g.root).unwrap(), g.root_serial));
        fs::write(&record, record_at(&nonce, named, &root)).unwrap();
        reap_stale_checks(&root);
        assert!(
            ends(g.left.pid, g.left.serial),
            "what the gone root left outlived the reap"
        );
        reap_until_gone(&root, &record, "the record of what was ended stayed", || {});
        assert_eq!(held_slots(&root), [slot]);
    }

    /// A check's process that ignores `SIGTERM`, unmarked and tied only by the root and its
    /// session, which the `SIGTERM` ends, is still killed by the reap, which then takes the
    /// record. It ends by itself once the test's pipe closes ([`SURVIVOR_ENDS`]), so a test
    /// killed before its reap leaves no loop behind.
    #[test]
    fn what_outlives_the_reaps_sigterm_is_ended_by_it() {
        let d = tmp("trust-lane-survivor");
        let root = d.path().join("root");
        let slot = root.join("j0");
        owner_dirs(&slot).unwrap();
        let (pidfile, beat) = (d.path().join("member.pid"), d.path().join("beat"));
        let nonce = nonce();
        let mut cmd = command("/bin/bash");
        cmd.arg("-c")
            .arg(format!(
                "exec 3<&0; /usr/bin/env -i /bin/bash -c \
                 'trap \"\" TERM; t=; n=0; while :; do echo >> \"$0\"; read -t 1 -u 3; \
                 {SURVIVOR_ENDS} done' \"$2\" & echo $! > \"$1\"; wait",
            ))
            .arg("sh")
            .arg(&pidfile)
            .arg(&beat)
            .env(CHECK_VAR, &nonce)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let mut check = cmd.spawn().unwrap();
        let quiet = check.stdin.take().unwrap();
        let record = root.join(".check.0");
        fs::write(&record, marked_record(&nonce, Some(check.id()), &root)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let member = loop {
            if let Some(p) = fs::read_to_string(&pidfile)
                .ok()
                .and_then(|t| t.trim().parse().ok())
                && fs::metadata(&beat).is_ok_and(|m| m.len() > 0)
            {
                break Leftover::new(p);
            }
            assert!(
                Instant::now() < deadline,
                "the root never started its member"
            );
            aterm_uds::deadline::poll_yield();
        };
        reap_stale_checks(&root);
        assert!(
            ends(member.pid, member.serial),
            "what outlived the SIGTERM outlived the reap"
        );
        let _ = check.wait();
        reap_until_gone(
            &root,
            &record,
            "the reap kept the record of a check it ended",
            || {},
        );
        drop(quiet);
    }

    /// An unmarked member of a dead root's session is never signalled, since a reused pid can
    /// make it a stranger's. Working in or holding a file in the slot, it holds the record
    /// whatever the stamp says; working elsewhere, only under an earlier build's record, which
    /// names no nonce. Once the session is empty a reap takes the record, and its stamp holds
    /// the slot.
    #[test]
    fn what_a_dead_roots_session_holds_keeps_its_record_unsignalled() {
        let d = tmp("trust-lane-stranger");
        let root = d.path().join("root");
        let slot = root.join("j0");
        let record = root.join(".check.0");
        for (how, prelude, at_slot) in [
            ("working in the slot", "", true),
            (
                "holding a file in the slot open",
                "exec 4>lock; cd /; ",
                true,
            ),
            ("working elsewhere", "cd /; ", false),
        ] {
            for earlier in [false, true] {
                owner_dirs(&slot).unwrap();
                fs::write(
                    slot.join(aterm_uds::deadline::EXEC_STAMP),
                    u64::MAX.to_string(),
                )
                .unwrap();
                let g = rootless_group(d.path(), None, Some(&slot), prelude);
                let text = if earlier {
                    format!("{} {}\n", g.root, g.lstart)
                } else {
                    let named = Some((i32::try_from(g.root).unwrap(), g.root_serial));
                    record_at(&nonce(), named, &root)
                };
                fs::write(&record, text).unwrap();
                let unsignalled = || {
                    assert!(
                        g.beats_on() && g.left.running(),
                        "a member {how} was signalled (earlier build's record: {earlier})"
                    );
                };
                reap_stale_checks(&root);
                unsignalled();
                let held = at_slot || earlier;
                if !held {
                    reap_until_gone(
                        &root,
                        &record,
                        &format!("{how}: a stranger's member held the record"),
                        unsignalled,
                    );
                }
                assert_eq!(
                    record.exists(),
                    held,
                    "{how}: a member of the dead root's session holds the record past its \
                     stamp only while it may be the check's (earlier build's record: {earlier})"
                );
                assert_eq!(
                    held_slots(&root),
                    std::slice::from_ref(&slot),
                    "{how}: held by its record, or by the stamp the reap that took it wrote"
                );
                assert!(g.end(), "the group outlived its kill");
                let stamp = slot.join(aterm_uds::deadline::EXEC_STAMP);
                fs::write(&stamp, u64::MAX.to_string()).unwrap();
                reap_until_gone(
                    &root,
                    &record,
                    &format!(
                        "{how}: once the session is empty the record goes (earlier build's \
                         record: {earlier})"
                    ),
                    || {},
                );
                let kept: &[PathBuf] = if held {
                    std::slice::from_ref(&slot)
                } else {
                    &[]
                };
                assert_eq!(
                    clear_slots(&root).unwrap(),
                    kept,
                    "{how}: the reap that took the record holds the slot from then (earlier \
                     build's record: {earlier})"
                );
                let _ = fs::remove_dir_all(&slot);
            }
        }
    }

    /// A member of a dead root's session that had a controlling terminal is a stranger's (the
    /// check's session, made by `setsid` with stdin null, never has one; macOS keeps the mark on
    /// what a tab's shell left when it exited), so it holds no record, even an earlier build's,
    /// and is never signalled. The shell exits only once the member has exec'd, since macOS
    /// drops the mark from a process that execs after its session's leader is gone.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_member_that_had_a_terminal_holds_no_record() {
        let d = tmp("trust-lane-tty");
        let root = d.path().join("root");
        let slot = root.join("j0");
        owner_dirs(&slot).unwrap();
        fs::write(
            slot.join(aterm_uds::deadline::EXEC_STAMP),
            u64::MAX.to_string(),
        )
        .unwrap();
        let (left, leader) = (d.path().join("left.pid"), d.path().join("leader.pid"));
        let mut tab = command("/usr/bin/script")
            .args(["-q", "/dev/null", "/bin/sh", "-c"])
            .arg(
                "trap '' HUP; (cd /; exec /bin/sleep 600) & p=$!; n=0; \
                 until [ \"$(ps -o comm= -p $p)\" = /bin/sleep ] || [ $n -ge 6000 ]; do \
                 n=$((n + 1)); sleep 0.01; done; echo $p > \"$1\"; echo $$ > \"$2\"",
            )
            .arg("sh")
            .arg(&left)
            .arg(&leader)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while tab.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = tab.kill();
                let _ = tab.wait();
                panic!("the terminal's shell never exited");
            }
            aterm_uds::deadline::poll_yield();
        }
        let pid =
            |file: &Path| -> u32 { fs::read_to_string(file).unwrap().trim().parse().unwrap() };
        let member = Leftover::new(pid(&left));
        let leader = i32::try_from(pid(&leader)).unwrap();
        assert!(
            runs_from(member.pid, Path::new("/bin/sleep")),
            "the member never ran its program"
        );
        let mark = proc_tree::Mark {
            nonce: None,
            root: Some((leader, NO_SERIAL)),
            kept: BTreeSet::new(),
        };
        let row = proc_tree::table(&mark, None, &slot)
            .unwrap()
            .procs
            .into_iter()
            .find(|p| p.pid == i32::try_from(member.pid).unwrap())
            .unwrap();
        assert_eq!(
            row.sid,
            Some(leader),
            "the case: it is left in the session its gone leader's pid names"
        );
        assert!(!row.in_session, "{row:?}");
        let record = root.join(".check.0");
        fs::write(&record, format!("{leader} whenever\n")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while record.exists() {
            assert!(
                Instant::now() < deadline,
                "a member that had a terminal holds no record"
            );
            reap_stale_checks(&root);
            assert!(member.running(), "the reap signalled a member");
        }
    }

    /// A live check is ended by its mark, never by a start's text (`ps` prints one in the
    /// reader's zone and locale): the running root and what it left outside its tree, an orphan
    /// whose environment macOS withholds among them, are ended and the slot held. A member the
    /// root's loop starts after the last proof of its session holds the record until it exits.
    #[test]
    fn a_live_check_and_what_it_left_outside_its_tree_are_ended_by_the_reap() {
        let d = tmp("trust-lane-live");
        let root = d.path().join("root");
        let slot = root.join("j0");
        owner_dirs(&slot).unwrap();
        let pidfile = d.path().join("orphan.pid");
        let withheld = d.path().join("withheld.pid");
        let nonce = nonce();
        let mut cmd = command("/bin/sh");
        cmd.arg("-c")
            .arg(
                "(\"$2\" 600 & echo $! > \"$1\"; /bin/sleep 600 & echo $! > \"$3\"); \
                 while :; do /bin/sleep 1; done",
            )
            .arg("sh")
            .arg(&pidfile)
            .arg(readable("/bin/sleep"))
            .arg(&withheld)
            .env(CHECK_VAR, &nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let mut check = cmd.spawn().unwrap();
        let orphan = orphan_named(&pidfile, check.id());
        let withheld = orphan_named(&withheld, check.id());
        assert!(
            runs_from(orphan.pid, &readable("/bin/sleep"))
                && runs_from(withheld.pid, Path::new("/bin/sleep")),
            "an orphan never ran its program"
        );
        let record = root.join(".check.0");
        fs::write(&record, marked_record(&nonce, Some(check.id()), &root)).unwrap();
        reap_stale_checks(&root);
        let deadline = Instant::now() + Duration::from_secs(60);
        let root_ended = loop {
            if check.try_wait().unwrap().is_some() {
                break true;
            }
            if Instant::now() >= deadline {
                let _ = check.kill();
                let _ = check.wait();
                break false;
            }
            aterm_uds::deadline::poll_yield();
        };
        assert!(root_ended, "the live root outlived the reap");
        assert!(
            ends(orphan.pid, orphan.serial),
            "what it left outside its tree outlived the reap"
        );
        assert!(
            ends(withheld.pid, withheld.serial),
            "what it left in its session, its environment withheld, outlived the reap"
        );
        reap_until_gone(&root, &record, "the record of what was ended stayed", || {});
        assert_eq!(held_slots(&root), [slot]);
    }

    /// The lane sweeps before it reaps an exited root, so the root's session is proven the
    /// check's and an orphan there is ended though nothing carrying the mark is left; the slot
    /// is held, since the check left something running.
    #[test]
    fn what_a_check_left_in_its_session_is_ended_when_its_root_exits() {
        use std::io::Write as _;
        let d = tmp("trust-lane-session");
        let target = d.path().join("j0");
        owner_dirs(&target).unwrap();
        let pidfile = d.path().join("orphan.pid");
        let nonce = nonce();
        let mut cmd = command("/bin/sh");
        cmd.arg("-c")
            .arg("(/bin/sleep 600 & echo $! > \"$1\"); read go")
            .arg("sh")
            .arg(&pidfile)
            .current_dir(&target)
            .env(CHECK_VAR, &nonce)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let root = child.id();
        let orphan = orphan_named(&pidfile, root);
        assert!(
            runs_from(orphan.pid, Path::new("/bin/sleep")),
            "the orphan never ran its program"
        );
        let record = d.path().join(".check.0");
        fs::write(&record, marked_record(&nonce, Some(root), d.path())).unwrap();
        let mark = proc_tree::Mark {
            nonce: Some(nonce),
            root: Some((i32::try_from(root).unwrap(), serial_of(root))),
            kept: BTreeSet::new(),
        };
        child.stdin.take().unwrap().write_all(b"go\n").unwrap();
        let mut check = RealCheck {
            mark,
            child,
            record: record.clone(),
            target: target.clone(),
            ended: None,
            times: TreeTimes::default(),
            read_at: None,
            sweep: || None,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        let ended = loop {
            if let Some(ended) = check.poll().unwrap() {
                break ended;
            }
            assert!(Instant::now() < deadline, "the root never exited");
            aterm_uds::deadline::poll_yield();
        };
        assert_eq!(ended.code, 0);
        assert!(
            ends(orphan.pid, orphan.serial),
            "what the root left in its session outlived it"
        );
        reap_until_gone(
            d.path(),
            &record,
            "the record stayed once a sweep ended all",
            || {},
        );
        assert_eq!(held_slots(d.path()), [target]);
    }

    /// With the root gone and nothing carrying the mark, a member working in the slot may still
    /// be the check's and exec a fresh build there: it holds the record, unsignalled and
    /// whatever the stamp says, until it is gone. The member drops the mark before it starts:
    /// one that carried it would be the check's by it, which Linux reads (macOS withholds a
    /// platform binary's environment), and a reap would end it.
    #[test]
    fn what_a_gone_root_left_working_in_its_slot_holds_it_unsignalled() {
        let d = tmp("trust-lane-working");
        let root = d.path().join("root");
        let slot = root.join("j0");
        owner_dirs(&slot).unwrap();
        let stamp = slot.join(aterm_uds::deadline::EXEC_STAMP);
        fs::write(&stamp, u64::MAX.to_string()).unwrap();
        let pidfile = d.path().join("orphan.pid");
        let nonce = nonce();
        let mut cmd = command("/bin/sh");
        cmd.arg("-c")
            .arg("(unset \"$2\"; /bin/sleep 600 & echo $! > \"$1\"); exec /bin/sleep 600")
            .arg("sh")
            .arg(&pidfile)
            .arg(CHECK_VAR)
            .current_dir(&slot)
            .env(CHECK_VAR, &nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();
        let serial = serial_of(pid);
        let orphan = orphan_named(&pidfile, pid);
        assert!(
            runs_from(orphan.pid, Path::new("/bin/sleep")),
            "the orphan never ran its program"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        let record = root.join(".check.0");
        let named = Some((i32::try_from(pid).unwrap(), serial));
        fs::write(&record, record_at(&nonce, named, &root)).unwrap();
        reap_stale_checks(&root);
        assert!(
            orphan.running(),
            "a process nothing ties to the check was signalled"
        );
        assert!(
            record.exists(),
            "a member of its session working in the slot holds it"
        );
        assert_eq!(fs::read_to_string(&stamp).unwrap(), u64::MAX.to_string());
        assert_eq!(held_slots(&root), std::slice::from_ref(&slot));
        let _ = command("/bin/kill")
            .args(["-KILL", &orphan.pid.to_string()])
            .status();
        assert!(ends(orphan.pid, orphan.serial));
        reap_until_gone(&root, &record, "once it is gone the record goes", || {});
        assert_ne!(fs::read_to_string(&stamp).unwrap(), u64::MAX.to_string());
        assert_eq!(held_slots(&root), [slot]);
    }

    /// An earlier build's record names its root by pid alone, and the process at that pid may
    /// be any of this user's (a copied tree resets the record's time, and the clock can be set
    /// back), so the reap signals nothing there or in its session; the first reap takes the
    /// record, and its stamp holds the slot.
    #[test]
    fn an_earlier_builds_record_never_signals_the_process_at_its_pid() {
        let d = tmp("trust-lane-earlier-live");
        let root = d.path().join("root");
        let slot = root.join("j0");
        owner_dirs(&slot).unwrap();
        let pidfile = d.path().join("orphan.pid");
        let mut cmd = command("/bin/sh");
        cmd.arg("-c")
            .arg("(/bin/sleep 600 & echo $! > \"$1\"); while :; do /bin/sleep 1; done")
            .arg("sh")
            .arg(&pidfile)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let mut live = cmd.spawn().unwrap();
        let leader = Leftover::new(live.id());
        let orphan = orphan_named(&pidfile, live.id());
        let record = root.join(".check.0");
        fs::write(&record, format!("{} {}\n", live.id(), lstart(live.id()))).unwrap();
        reap_until_gone(
            &root,
            &record,
            "a session another process leads at its pid holds nothing",
            || {
                assert!(
                    leader.running() && orphan.running(),
                    "the reap signalled what an earlier build's record names"
                );
            },
        );
        assert_eq!(held_slots(&root), std::slice::from_ref(&slot));
        let _ = live.kill();
        let _ = live.wait();
    }

    /// A reap that could not read keeps the record and leaves the check running, the slot held
    /// past a spent stamp; one that reads ends it, and a reap that finds nothing left takes the
    /// record.
    #[test]
    fn a_reap_that_could_not_read_keeps_the_record_past_a_spent_stamp() {
        let d = tmp("trust-lane-unread");
        let root = d.path().join("root");
        let slot = root.join("j0");
        owner_dirs(&slot).unwrap();
        let stamp = slot.join(aterm_uds::deadline::EXEC_STAMP);
        fs::write(&stamp, u64::MAX.to_string()).unwrap();
        let record = root.join(".check.0");
        let nonce = nonce();
        let mut live = command("/bin/sleep")
            .arg("600")
            .env(CHECK_VAR, &nonce)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        fs::write(&record, marked_record(&nonce, Some(live.id()), &root)).unwrap();
        let boot = aterm_uds::process::boot_id();
        reap_stale_checks_with(&root, boot.as_deref(), &mut |_, _, _, _| {
            proc_tree::Left::Unread
        });
        let running = live.try_wait().unwrap().is_none();
        assert!(running, "a check a reap could not read is left alone");
        assert!(record.exists(), "an unread reap keeps the record");
        assert_eq!(fs::read_to_string(&stamp).unwrap(), u64::MAX.to_string());
        assert_eq!(clear_slots(&root).unwrap(), std::slice::from_ref(&slot));

        reap_stale_checks(&root);
        let deadline = Instant::now() + Duration::from_secs(60);
        let ended = loop {
            if live.try_wait().unwrap().is_some() {
                break true;
            }
            if Instant::now() >= deadline {
                let _ = live.kill();
                let _ = live.wait();
                break false;
            }
            aterm_uds::deadline::poll_yield();
        };
        assert!(ended, "a reap that reads ends the check");
        reap_until_gone(
            &root,
            &record,
            "a reap that finds nothing of the check takes its record",
            || {},
        );
        assert_ne!(fs::read_to_string(&stamp).unwrap(), u64::MAX.to_string());
        assert_eq!(held_slots(&root), [slot]);
    }

    /// An earlier build's record ends nothing: it holds the slot while a member of its gone
    /// root's session is left, each reap renewing the stamp, and the reap that finds none takes
    /// the record, its stamp holding the slot.
    #[test]
    fn an_earlier_builds_record_holds_its_slot_while_its_roots_session_is_left() {
        let d = tmp("trust-lane-earlier-session");
        let root = d.path().join("root");
        let slot = root.join("j0");
        owner_dirs(&slot).unwrap();
        let g = rootless_group(d.path(), None, None, "");
        let record = root.join(".check.0");
        fs::write(&record, format!("{} {}\n", g.root, g.lstart)).unwrap();
        let stamp = slot.join(aterm_uds::deadline::EXEC_STAMP);
        let spent = u64::MAX.to_string();
        fs::write(&stamp, &spent).unwrap();
        reap_stale_checks(&root);
        assert!(
            g.beats_on() && g.left.running(),
            "an earlier build's record ended its group"
        );
        assert!(
            record.exists(),
            "a member of its gone root's session holds it past the stamp"
        );
        assert_ne!(
            fs::read_to_string(&stamp).unwrap(),
            spent,
            "the reap holds the slot from then"
        );
        assert!(g.end(), "the group outlived its kill");
        fs::write(&stamp, &spent).unwrap();
        reap_until_gone(
            &root,
            &record,
            "the record goes once the session is empty",
            || {},
        );
        assert_eq!(
            clear_slots(&root).unwrap(),
            std::slice::from_ref(&slot),
            "the stamp the reap that took the record wrote holds the slot"
        );
        fs::write(&stamp, &spent).unwrap();
        assert!(clear_slots(&root).unwrap().is_empty() && !slot.exists());
    }

    /// A slot whose record a reap kept stays held whatever its stamp says, until a reap
    /// removes the record: a run's last clear, which reaps nothing, keeps it, and says what
    /// holds it.
    #[test]
    fn a_kept_record_holds_its_slot_past_its_stamp() {
        let d = tmp("trust-lane-kept-record");
        let root = d.path().join("root");
        for slot in ["j0", "j1", "j2"] {
            owner_dirs(&root.join(slot)).unwrap();
            fs::write(
                root.join(slot).join(aterm_uds::deadline::EXEC_STAMP),
                u64::MAX.to_string(),
            )
            .unwrap();
        }
        fs::write(root.join(".check.0"), "1 whenever\n").unwrap();
        let nonce = nonce();
        fs::write(root.join(".check.2"), marked_record(&nonce, None, &root)).unwrap();
        let kept = [root.join("j0"), root.join("j2")];
        let mut held = held_slots(&root);
        held.sort();
        assert_eq!(held, kept);
        let mut cleared = clear_slots(&root).unwrap();
        cleared.sort();
        assert_eq!(cleared, kept);
        assert!(kept.iter().all(|slot| slot.is_dir()) && !root.join("j1").exists());
        let said = held_words(&kept);
        assert!(said.contains("names no mark"), "{said}");
        assert!(said.contains(&format!("{CHECK_VAR}={nonce}")), "{said}");
    }

    /// A live check of this boot, marked `nonce` and leading its own session, its root's
    /// environment readable: the process and its child, to be waited on.
    fn live_check(nonce: &str) -> (aterm_uds::spawnfd::Child, Leftover) {
        let mut cmd = command(readable("/bin/sleep"));
        cmd.arg("600")
            .env(CHECK_VAR, nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let child = cmd.spawn().unwrap();
        let left = Leftover::new(child.id());
        (child, left)
    }

    /// A copy's record (another root's) and a live run's record signal nothing: the copy's
    /// holds its slot by the first stamp a reap gave it and goes once that is spent; the live
    /// run's stays, unstamped, until that run is gone.
    #[test]
    fn a_copied_record_or_a_live_runs_record_signals_nothing() {
        let d = tmp("trust-lane-copy");
        let root = d.path().join("root");
        for slot in ["j0", "j1"] {
            owner_dirs(&root.join(slot)).unwrap();
        }
        let stamp = |slot: &str| root.join(slot).join(aterm_uds::deadline::EXEC_STAMP);
        let nonce = nonce();
        let (mut child, check) = live_check(&nonce);
        assert!(
            runs_from(check.pid, &readable("/bin/sleep")),
            "the check never ran its program"
        );
        let lane = Leftover::new(
            command("/bin/sleep")
                .arg("600")
                .stdin(Stdio::null())
                .spawn()
                .unwrap()
                .id(),
        );
        let named = Some((i32::try_from(check.pid).unwrap(), check.serial));
        let record = |lane, dir| {
            CheckRecord {
                nonce: Some(nonce.clone()),
                boot: aterm_uds::process::boot_id(),
                lane: Some(lane),
                dir,
                root: named,
                kept: BTreeSet::new(),
            }
            .text()
        };
        fs::write(root.join(".check.0"), record(GONE_LANE, dir_id(d.path()))).unwrap();
        let live_run = (i32::try_from(lane.pid).unwrap(), lane.serial);
        fs::write(root.join(".check.1"), record(live_run, dir_id(&root))).unwrap();
        reap_stale_checks(&root);
        assert!(check.running(), "a reap signalled a check it does not own");
        assert!(root.join(".check.0").exists() && root.join(".check.1").exists());
        assert!(
            aterm_uds::deadline::exec_stamp(&root.join("j0")).is_some(),
            "a copy's record holds its slot by a first stamp"
        );
        assert!(!stamp("j1").exists(), "a live run's slot is its own");
        let said = held_words(&[root.join("j0"), root.join("j1")]);
        assert!(said.contains("another target root"), "{said}");
        assert!(said.contains(&format!("run {}", lane.pid)), "{said}");

        fs::write(stamp("j0"), u64::MAX.to_string()).unwrap();
        reap_until_gone(
            &root,
            &root.join(".check.0"),
            "a copy's record goes once its first stamp is spent",
            || {
                assert!(check.running(), "a reap signalled a check it does not own");
                assert!(root.join(".check.1").exists() && !stamp("j1").exists());
            },
        );

        let _ = command("/bin/kill")
            .args(["-KILL", &lane.pid.to_string()])
            .status();
        assert!(ends(lane.pid, lane.serial));
        reap_stale_checks(&root);
        let deadline = Instant::now() + Duration::from_secs(60);
        let ended = loop {
            if child.try_wait().unwrap().is_some() {
                break true;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
            aterm_uds::deadline::poll_yield();
        };
        assert!(ended, "once its run is gone, the reap ends the check");
        reap_until_gone(
            &root,
            &root.join(".check.1"),
            "the record of the check its run's reap ended stayed",
            || {},
        );
        assert_eq!(held_slots(&root), [root.join("j1")]);
    }

    /// In a slot whose stamp cannot be written, a check the lane ended or a reap found ended
    /// keeps its record, which holds the slot until a reap that writes the stamp takes it.
    #[test]
    fn a_record_whose_stamp_cannot_be_written_holds_its_slot() {
        let d = tmp("trust-lane-unstamped");
        let root = d.path().join("root");
        let target = root.join("j0");
        owner_dirs(&target).unwrap();
        struct BlockedStamp(PathBuf);
        impl Drop for BlockedStamp {
            fn drop(&mut self) {
                let _ = fs::remove_dir(&self.0);
            }
        }
        let blocked = BlockedStamp(target.join(aterm_uds::deadline::EXEC_STAMP));
        fs::create_dir(&blocked.0).unwrap();
        assert!(aterm_uds::deadline::try_stamp_exec(&target).is_err());
        let retained = target.join("retained");
        fs::write(&retained, b"the slot stays").unwrap();
        let record = root.join(".check.0");
        let nonce = nonce();
        // The unreaped child's PID and serial identify it without reading its environment.
        let mut cmd = command("/bin/sleep");
        cmd.arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let child = cmd.spawn().unwrap();
        let check = Leftover::new(child.id());
        assert!(
            runs_from(check.pid, Path::new("/bin/sleep")),
            "the check never ran its program"
        );
        fs::write(&record, marked_record(&nonce, Some(check.pid), &root)).unwrap();
        let mut real = RealCheck {
            child,
            record: record.clone(),
            target: target.clone(),
            mark: proc_tree::Mark {
                nonce: Some(nonce),
                root: Some((i32::try_from(check.pid).unwrap(), check.serial)),
                kept: BTreeSet::new(),
            },
            ended: None,
            times: TreeTimes::default(),
            read_at: None,
            sweep: || None,
        };
        real.end();
        let kept = record.exists();
        let stamped = aterm_uds::deadline::exec_stamp(&target).is_some();
        let held = held_slots(&root);
        reap_stale_checks(&root);
        let kept_by_reap = record.exists();
        let held_by_clear = clear_slots(&root).unwrap();
        let retained_bytes = fs::read(&retained);
        drop(blocked);
        assert!(
            kept && !stamped,
            "a check whose slot could not be stamped keeps its record"
        );
        assert_eq!(held, std::slice::from_ref(&target));
        assert!(
            kept_by_reap,
            "a reap whose stamp could not be written keeps the record"
        );
        assert_eq!(held_by_clear, std::slice::from_ref(&target));
        assert_eq!(retained_bytes.unwrap(), b"the slot stays");
        reap_until_gone(
            &root,
            &record,
            "a reap that stamps the slot takes it",
            || {},
        );
        assert_eq!(held_slots(&root), [target]);
    }

    /// A reused pid must be read as the new process before a listing can establish absence.
    #[test]
    fn a_reused_pid_is_read_again_before_a_listing_is_whole() {
        use std::cell::Cell;

        let pass = Cell::new(0);
        let reads = Cell::new(0);
        let listing = proc_tree::table_read(
            Duration::from_secs(60),
            &mut wall(),
            &mut || {
                pass.set(pass.get() + 1);
                Ok(vec![51])
            },
            &mut |_| Some(if pass.get() == 1 { 100 } else { 101 }),
            &mut |_| {
                reads.set(reads.get() + 1);
                Some(proc_tree::Proc {
                    serial: Some(if pass.get() == 1 { 100 } else { 101 }),
                    ..proc(51, 1, Some(pass.get() > 1))
                })
            },
        )
        .unwrap();
        assert!(listing.whole);
        assert_eq!((pass.get(), reads.get()), (3, 2));
        let owned = proc_tree::marked(&listing.procs, None, &BTreeSet::new());
        assert_eq!(owned.len(), 1);
        assert_eq!((owned[0].pid, owned[0].serial), (51, Some(101)));
        assert_eq!(listing.procs, owned, "the old stranger's row was replaced");
    }

    /// A listing outlasts churn for up to [`proc_tree::LISTING`]: churn that stops before it
    /// ends whole at the first list that names nothing new, and churn past it ends not whole.
    #[test]
    fn a_listing_outlasts_a_busy_machines_churn() {
        use std::cell::Cell;

        const LIST: Duration = Duration::from_millis(100);
        let churn = |churning: u32| {
            let lists = Cell::new(0);
            let clock = Cell::new(Duration::from_secs(3600));
            let listing = proc_tree::table_read(
                proc_tree::LISTING,
                &mut || clock.get(),
                &mut || {
                    lists.set(lists.get() + 1);
                    clock.set(clock.get() + LIST);
                    Ok(if lists.get() <= churning {
                        vec![51, 1000 + lists.get()]
                    } else {
                        vec![51]
                    })
                },
                &mut |pid| Some(u64::from(pid) * 10),
                &mut |pid| Some(proc(i32::try_from(pid).unwrap(), 1, Some(false))),
            )
            .unwrap();
            (listing.whole, lists.get())
        };
        assert_eq!(
            churn(99),
            (true, 100),
            "9.9 s of churn ends whole at the first list that names none"
        );
        let past = u32::try_from(proc_tree::LISTING.as_nanos().div_ceil(LIST.as_nanos())).unwrap();
        assert_eq!(
            churn(u32::MAX),
            (false, past),
            "churn past LISTING ends not whole at the first list once it passed"
        );
    }

    /// An exited, unreaped process, of which macOS reads no serial, leaves a listing whole; a
    /// check's root is one whenever its sweep runs. Each list is cut to the child, since a busy
    /// machine's other processes may keep a listing from being whole.
    #[test]
    fn an_unreaped_child_leaves_the_listing_whole() {
        let d = tmp("trust-lane-unreaped");
        let mut child = command("/usr/bin/true")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while aterm_uds::process::process_presence(pid)
            != aterm_uds::process::ProcessPresence::Exited
        {
            assert!(Instant::now() < deadline, "the child never exited");
            aterm_uds::deadline::poll_yield();
        }
        let mark = proc_tree::Mark {
            nonce: Some(nonce()),
            root: None,
            kept: BTreeSet::new(),
        };
        let id = child.id();
        let mut lists = 0;
        let listing =
            proc_tree::table_from(&mark, None, d.path(), Duration::from_secs(60), &mut || {
                lists += 1;
                let all = aterm_uds::holders::uid_pids()?;
                assert!(all.contains(&id), "this user's processes name the child");
                Ok(vec![id])
            })
            .unwrap();
        assert!(
            listing.whole,
            "a listing that names an unreaped child is whole"
        );
        assert_eq!(lists, 2, "the child is read once");
        child.wait().unwrap();
    }

    /// A spawn placeholder may look like trustd, then become a check's process at the same
    /// pid and serial. Its placeholder image must not suppress the eventual marked row.
    #[test]
    fn a_spawn_placeholder_is_read_again_at_the_same_serial() {
        use std::cell::Cell;

        let reads = Cell::new(0);
        let listing = proc_tree::table_read(
            Duration::from_secs(60),
            &mut wall(),
            &mut || Ok(vec![51]),
            &mut |_| Some(100),
            &mut |_| {
                reads.set(reads.get() + 1);
                let spawning = reads.get() == 1;
                Some(proc_tree::Proc {
                    serial: Some(100),
                    trustd: spawning,
                    spawning,
                    ..proc(51, 1, Some(!spawning))
                })
            },
        )
        .unwrap();
        assert!(listing.whole);
        assert_eq!(reads.get(), 2);
        assert_eq!(
            proc_tree::marked(&listing.procs, None, &BTreeSet::new()),
            listing.procs
        );
        assert_eq!(listing.procs.len(), 1);
        assert!(!listing.procs[0].trustd && !listing.procs[0].spawning);
    }

    /// An unread serial or a replacement during a row cannot become a cached identity.
    #[test]
    fn an_unread_or_changing_identity_cannot_certify_a_whole_listing() {
        use std::cell::Cell;

        for unread in [true, false] {
            let identity = Cell::new(100);
            let reads = Cell::new(0);
            let listing = proc_tree::table_read(
                Duration::from_millis(100),
                &mut wall(),
                &mut || Ok(vec![51]),
                &mut |_| (!unread).then_some(identity.get()),
                &mut |_| {
                    let serial = identity.get();
                    identity.set(serial + 1);
                    reads.set(reads.get() + 1);
                    Some(proc_tree::Proc {
                        serial: Some(serial),
                        ..proc(51, 1, Some(true))
                    })
                },
            )
            .unwrap();
            assert!(!listing.whole, "unread={unread}");
            assert!(reads.get() > 1);
            assert_eq!(listing.procs.len(), 1);
            assert_eq!(listing.procs[0].serial, Some(identity.get() - 1));
        }

        let reads = Cell::new(0);
        let listing = proc_tree::table_read(
            Duration::from_secs(60),
            &mut wall(),
            &mut || Ok(vec![51]),
            &mut |_| Some(100),
            &mut |_| {
                reads.set(reads.get() + 1);
                Some(proc_tree::Proc {
                    serial: Some(100),
                    ..proc(51, 1, Some(false))
                })
            },
        )
        .unwrap();
        assert!(listing.whole, "a stable identity still completes");
        assert_eq!(reads.get(), 1, "a stable row is read once");
    }

    /// A listing is whole only once a later list names nothing it did not read: here a later
    /// list names what the first missed in a gone root's session, which then holds the slot;
    /// lists that keep naming new processes make no whole listing.
    #[test]
    fn a_process_the_first_list_missed_is_read_from_a_later_one() {
        let d = tmp("trust-lane-whole");
        let slot = d.path().join("j0");
        owner_dirs(&slot).unwrap();
        let nonce = nonce();
        let g = rootless_group(d.path(), Some(&nonce), None, "");
        let mark = proc_tree::Mark {
            nonce: Some(nonce),
            root: Some((i32::try_from(g.root).unwrap(), g.root_serial)),
            kept: BTreeSet::new(),
        };
        let missed = g.left.pid;
        let mut lists = 0;
        let listing =
            proc_tree::table_from(&mark, None, &slot, Duration::from_secs(60), &mut || {
                lists += 1;
                Ok(if lists > 1 { vec![missed] } else { Vec::new() })
            })
            .unwrap();
        assert!(listing.whole);
        assert!(
            listing
                .procs
                .iter()
                .any(|p| p.pid == i32::try_from(missed).unwrap() && p.in_session),
            "the process the first list missed was never read"
        );
        assert!(proc_tree::unread(&listing.procs, &slot));
        assert!(g.left.running());

        let mut n = 0;
        let churning =
            proc_tree::table_from(&mark, None, &slot, Duration::from_millis(100), &mut || {
                n += 1;
                Ok(vec![u32::MAX - n])
            })
            .unwrap();
        assert!(!churning.whole, "lists that keep naming new processes");
    }

    /// The lane's own `RealChecks` over a script `targo` in `d`, its root's serial read with
    /// `serial`, with no gate, no environment, no process table and no interrupt caught.
    fn real_checks<'a>(
        d: &'a Path,
        root: &'a Path,
        driver: &'a Driver,
        keeper: &'a trustd::Keeper,
        sock: &'a Path,
        serial: fn(i32) -> Option<u64>,
    ) -> RealChecks<'a> {
        let mut real = RealChecks::new(
            driver,
            d,
            root,
            keeper,
            sock,
            "aa",
            Admission::new(
                None,
                &aterm_capacity::Live,
                aterm_capacity::Admit::Wait,
                aterm_capacity::Pace::default(),
            ),
            Box::new(std::io::sink()),
        );
        real.env = Vec::new();
        real.serial = serial;
        real.sweep = || None;
        real.caught = || None;
        real
    }

    /// A script `targo` under `d` that runs `body`.
    fn script_driver(d: &Path, body: &str) -> Driver {
        use std::os::unix::fs::PermissionsExt as _;
        let bin = d.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let targo = bin.join("targo");
        fs::write(&targo, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&targo, fs::Permissions::from_mode(0o755)).unwrap();
        Driver {
            targo,
            bin,
            id: String::new(),
        }
    }

    fn quiet_keeper(d: &Path) -> (PathBuf, trustd::Keeper) {
        let sock = d.join("none.sock");
        let keeper =
            trustd::Keeper::start(sock.clone(), "aa".into(), d.join("grants"), d.join("seen"));
        (sock, keeper)
    }

    /// A REAL RUN INTERRUPTED GIVES ITS GATE UP WITH THE SIGNAL IT CAUGHT (TM-1 review): the
    /// lane's own backend reads the run's interrupt from its `caught` (the process's latch,
    /// [`crate::interrupt::caught`], in a run), both for whether the run was interrupted and for
    /// the signal its waiting check's admission row names. NEGATIVE CONTROLS (by hand):
    /// `given_up` doing nothing: no row until the backend drops, and that one with no signal;
    /// `interrupted` reading the process's latch instead: not interrupted.
    #[test]
    fn a_real_run_interrupted_gives_its_gate_up_with_the_signal_it_caught() {
        use aterm_capacity::telemetry::capture;
        use aterm_telemetry::{Kind, Meter, Outcome, Stop};
        let d = tmp("trust-lane-given-up");
        let (_l, l) = ledger();
        let m = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
        let hold = held_by_another(&l, &m, 30);
        let root = d.path().join("root");
        let driver = script_driver(d.path(), "exit 0");
        let (sock, keeper) = quiet_keeper(d.path());
        let mut real = real_checks(
            d.path(),
            &root,
            &driver,
            &keeper,
            &sock,
            aterm_uds::process::process_serial,
        );
        assert!(!real.interrupted(), "no signal caught");
        real.caught = || Some(15);
        assert!(real.interrupted(), "a signal caught");
        real.admission = gate(&l, &m, aterm_capacity::Admit::Wait);
        let ((), rows) = capture(l.clone(), || {
            assert!(
                real.admit(&needing("a", 20)).is_none(),
                "20 GiB beside 30 waits"
            );
            real.given_up();
            drop(real);
        });
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            admission_words(&rows[0]),
            (
                Kind::Admission,
                Outcome::Fail,
                Stop::Signal,
                Some(15),
                Meter::Stamped
            )
        );
        drop(hold);
        assert!(l.list().expect("the ledger").is_empty());
    }

    /// A REAL BACKEND READS THE INTERRUPT THIS PROCESS CAUGHT (TM-1 review): the backend a run
    /// builds ([`RealChecks::new`]) reads the process's own latch ([`crate::interrupt::caught`]),
    /// both for whether the run was interrupted and for the signal its waiting check's admission
    /// row names. The latch is the process's, so this runs in a process of its own (its
    /// directory named by `ATERM_RELEASE_TRUST_LANE_LATCH`), which catches a `SIGTERM` it sends
    /// itself and reaches no other test. NEGATIVE CONTROL (by hand): `new` binding no latch
    /// (`|| None`): not interrupted once the signal is caught.
    #[cfg(unix)]
    #[test]
    fn a_real_backend_reads_the_interrupt_this_process_caught() {
        const LATCH_ENV: &str = "ATERM_RELEASE_TRUST_LANE_LATCH";
        const LATCH: &str =
            "trust_lane::tests::a_real_backend_reads_the_interrupt_this_process_caught";
        if let Some(dir) = std::env::var_os(LATCH_ENV) {
            the_interrupt_this_process_caught(Path::new(&dir));
            return;
        }
        let d = tmp("trust-lane-latch");
        let out = command(std::env::current_exe().unwrap())
            .args(["--exact", "--nocapture", "--test-threads=1", LATCH])
            .env(LATCH_ENV, d.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let said = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "the probe: {said}");
        assert!(
            d.path().join("caught").exists(),
            "the probe never ran: {said}"
        );
    }

    /// Run by [`a_real_backend_reads_the_interrupt_this_process_caught`] in a process of its
    /// own: a backend built as a run builds it, its gate in a scratch ledger under `d`, reads no
    /// interrupt; once this process, its handler installed from the default dispositions, has
    /// caught the `SIGTERM` it sends itself, the backend is
    /// interrupted, and its waiting check's row names that signal. Writes `caught` in `d` once
    /// all of it held.
    #[cfg(unix)]
    fn the_interrupt_this_process_caught(d: &Path) {
        use aterm_capacity::telemetry::capture;
        use aterm_telemetry::{Kind, Meter, Outcome, Stop};
        let l = aterm_capacity::Ledger::at(d.join("ledger"));
        let m = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
        let hold = held_by_another(&l, &m, 30);
        let root = d.join("root");
        let driver = script_driver(d, "exit 0");
        let (sock, keeper) = quiet_keeper(d);
        let mut real = RealChecks::new(
            &driver,
            d,
            &root,
            &keeper,
            &sock,
            "aa",
            gate(&l, &m, aterm_capacity::Admit::Wait),
            Box::new(std::io::sink()),
        );
        assert!(!real.interrupted(), "no signal caught yet");
        unsafe extern "C" {
            fn signal(sig: i32, handler: usize) -> usize;
        }
        // The handler is installed as a run installs it, from the default dispositions, whatever
        // the test inherited: a `SIGTERM` the binary was started ignoring is caught here too.
        for sig in [
            crate::interrupt::SIGHUP,
            crate::interrupt::SIGINT,
            crate::interrupt::SIGTERM,
        ] {
            // SAFETY: SIG_DFL (0) for a signal no handler of this process holds yet.
            unsafe {
                signal(sig, 0);
            }
        }
        crate::interrupt::install();
        crate::interrupt::send(
            i32::try_from(std::process::id()).expect("a pid"),
            crate::interrupt::SIGTERM,
        );
        let deadline = Instant::now() + Duration::from_secs(60);
        while crate::interrupt::caught().is_none() {
            assert!(Instant::now() < deadline, "the SIGTERM was never caught");
            aterm_uds::deadline::poll_yield();
        }
        assert!(real.interrupted(), "this process caught a signal");
        let ((), rows) = capture(l.clone(), || {
            assert!(
                real.admit(&needing("a", 20)).is_none(),
                "20 GiB beside 30 waits"
            );
            real.given_up();
            drop(real);
        });
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            admission_words(&rows[0]),
            (
                Kind::Admission,
                Outcome::Fail,
                Stop::Signal,
                Some(crate::interrupt::SIGTERM),
                Meter::Stamped
            )
        );
        drop(hold);
        fs::write(d.join("caught"), "").expect("the probe's mark");
    }

    /// Starting a check removes its slot's spent stamp, so the first stamp a reap of its record
    /// finds is that reap's own; the record names the check's mark, boot, lane, target root and
    /// root.
    #[test]
    fn a_check_clears_the_spent_stamp_of_its_slot() {
        let d = tmp("trust-lane-start");
        let root = d.path().join("root");
        let slot = root.join("j0");
        owner_dirs(&slot).unwrap();
        let stamp = slot.join(aterm_uds::deadline::EXEC_STAMP);
        fs::write(&stamp, u64::MAX.to_string()).unwrap();
        assert!(!slot_held(&root, 0));
        let seen = d.path().join("seen.env");
        let driver = script_driver(
            d.path(),
            &format!("echo \"${CHECK_VAR}\" > '{}'", seen.display()),
        );
        let (sock, keeper) = quiet_keeper(d.path());
        let mut real = real_checks(
            d.path(),
            &root,
            &driver,
            &keeper,
            &sock,
            aterm_uds::process::process_serial,
        );
        let mut j = job("alpha");
        j.log = d.path().join("alpha.log");
        let mut check = real.start(&mut j).unwrap();
        assert!(!stamp.exists(), "the spent stamp is gone");
        let rec = CheckRecord::parse(&fs::read_to_string(root.join(".check.0")).unwrap());
        let deadline = Instant::now() + Duration::from_secs(60);
        let ended = loop {
            if let Some(ended) = check.poll().unwrap() {
                break ended;
            }
            assert!(Instant::now() < deadline, "the check never ended");
            aterm_uds::deadline::poll_yield();
        };
        assert_eq!(ended.code, 0);
        reap_until_gone(
            &root,
            &root.join(".check.0"),
            "the record of a check that ended stayed",
            || {},
        );
        assert!(rec.root.is_some() && rec.boot == aterm_uds::process::boot_id());
        assert_eq!(
            (rec.lane, rec.dir),
            (this_lane(), dir_id(&root)),
            "the record names the lane that started the check and the root it is under"
        );
        assert_eq!(
            fs::read_to_string(&seen).unwrap().trim(),
            rec.nonce.unwrap(),
            "the check carries the mark its record names"
        );
    }

    /// A check whose root cannot be identified is ended at its gate ([`GATE`]), never started,
    /// and its record goes.
    #[test]
    fn a_check_whose_root_cannot_be_identified_is_ended_and_not_started() {
        use std::sync::atomic::{AtomicI32, Ordering};
        static ASKED: AtomicI32 = AtomicI32::new(0);
        let d = tmp("trust-lane-unidentified");
        let root = d.path().join("root");
        owner_dirs(&root.join("j0")).unwrap();
        let driver = script_driver(d.path(), "exec /bin/sleep 600");
        let (sock, keeper) = quiet_keeper(d.path());
        let mut real = real_checks(d.path(), &root, &driver, &keeper, &sock, |pid| {
            ASKED.store(pid, Ordering::SeqCst);
            None
        });
        let mut j = job("alpha");
        j.log = d.path().join("alpha.log");
        let Err(e) = real.start(&mut j) else {
            panic!("a check whose root was not identified started");
        };
        assert!(e.to_string().contains("could not be identified"), "{e}");
        let pid = ASKED.load(Ordering::SeqCst);
        assert!(pid > 1, "the root's serial was never asked for");
        let state = state(pid.unsigned_abs());
        if !state.ended() {
            let _ = command("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
        assert!(
            state.ended(),
            "the root is left running or stopped: {state:?}"
        );
        reap_until_gone(
            &root,
            &root.join(".check.0"),
            "the record of a check that never started stayed",
            || {},
        );
    }

    const DIES_STARTING_ENV: &str = "ATERM_RELEASE_TRUST_LANE_DIES_STARTING";
    const DIES_STARTING: &str =
        "trust_lane::tests::a_lane_killed_starting_a_check_leaves_no_root_waiting";

    /// Run by [`a_lane_killed_starting_a_check_leaves_no_root_waiting`] in a process of its
    /// own: starts a check under the directory [`DIES_STARTING_ENV`] names, and as it reads the
    /// root's serial writes the root's pid and serial to `root.pid` there and dies by
    /// `SIGKILL`, before the record names the root.
    fn probe_a_lane_that_dies_starting_a_check_if_asked() {
        let Some(dir) = std::env::var_os(DIES_STARTING_ENV) else {
            return;
        };
        let d = PathBuf::from(dir);
        let root = d.join("root");
        owner_dirs(&root.join("j0")).unwrap();
        let ran = d.join("ran");
        let driver = script_driver(
            &d,
            &format!("echo ran > '{}'; exec /bin/sleep 600", ran.display()),
        );
        let (sock, keeper) = quiet_keeper(&d);
        let mut real = real_checks(&d, &root, &driver, &keeper, &sock, |pid| {
            let dir = PathBuf::from(std::env::var_os(DIES_STARTING_ENV).unwrap());
            let serial = aterm_uds::process::process_serial(pid).unwrap();
            fs::write(dir.join("root.pid"), format!("{pid} {serial}")).unwrap();
            aterm_uds::process::kill_self_now()
        });
        let mut j = job("alpha");
        j.log = d.join("alpha.log");
        let started = real.start(&mut j).map(|_| ());
        panic!("the lane outlived reading its root's serial: {started:?}");
    }

    /// The root waits at its gate ([`GATE`]) for the lane's line, so a lane killed before the
    /// record names the root leaves it neither stopped (as a root started suspended would stay,
    /// with nothing to continue it) nor running targo: it exits, and the next reap takes the
    /// record.
    #[test]
    fn a_lane_killed_starting_a_check_leaves_no_root_waiting() {
        probe_a_lane_that_dies_starting_a_check_if_asked();
        use std::os::unix::process::ExitStatusExt as _;
        let d = tmp("trust-lane-dies-starting");
        let out = command(std::env::current_exe().unwrap())
            .args(["--exact", "--nocapture", "--test-threads=1", DIES_STARTING])
            .env(DIES_STARTING_ENV, d.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(
            out.status.signal(),
            Some(proc_tree::SIGKILL),
            "the probe lane: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let named = fs::read_to_string(d.path().join("root.pid")).unwrap();
        let (pid, serial) = named.split_once(' ').unwrap();
        let root = Leftover {
            pid: pid.parse().unwrap(),
            serial: serial.parse().unwrap(),
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        while alive(root.pid, root.serial) {
            let state = state(root.pid);
            assert_ne!(
                state,
                State::Stopped,
                "the lane's death left its check's root stopped"
            );
            assert!(
                Instant::now() < deadline,
                "the root outlived its lane by a minute: {state:?}"
            );
            aterm_uds::deadline::poll_yield();
        }
        assert!(
            !d.path().join("ran").exists(),
            "the root ran targo though no record named it"
        );
        let record = d.path().join("root").join(".check.0");
        assert!(record.exists(), "the record was written before the root");
        reap_until_gone(
            &d.path().join("root"),
            &record,
            "the next runs' reaps kept the record",
            || {},
        );
    }

    const DIES_SWEEPING_ENV: &str = "ATERM_RELEASE_TRUST_LANE_DIES_SWEEPING";
    const DIES_SWEEPING: &str =
        "trust_lane::tests::what_a_dead_lanes_sigterm_reached_is_ended_by_the_next_reap";

    /// The kernel's reads and signals, but the lane dies once it has sent `SIGTERM`.
    struct DiesAfterSigterm<'a>(proc_tree::Live<'a>, bool);

    impl proc_tree::Procs for DiesAfterSigterm<'_> {
        fn table(&mut self) -> Option<proc_tree::Listing> {
            self.0.table()
        }
        fn session(&mut self, listed: &[proc_tree::Proc]) -> Option<i32> {
            self.0.session(listed)
        }
        fn alive(&mut self, p: &proc_tree::Proc) -> bool {
            if self.1 {
                aterm_uds::process::kill_self_now()
            }
            self.0.alive(p)
        }
        fn keep(&mut self, procs: &[proc_tree::Proc]) -> bool {
            self.0.keep(procs)
        }
        fn send(&mut self, procs: &[proc_tree::Proc], sig: i32) -> Vec<(i32, u64)> {
            self.1 |= sig == proc_tree::SIGTERM;
            self.0.send(procs, sig)
        }
    }

    /// Run by [`what_a_dead_lanes_sigterm_reached_is_ended_by_the_next_reap`] in a process of
    /// its own: starts a check under the directory [`DIES_SWEEPING_ENV`] names, its root leading
    /// a session with a member that ignores `SIGTERM` and reads without the mark, writes the
    /// member's pid and serial to `member.pid` there, and ends the check as `RealCheck::end`
    /// does, dying by `SIGKILL` as the sweep first waits after its `SIGTERM`.
    fn probe_a_lane_that_dies_after_its_sigterm_if_asked() {
        let Some(dir) = std::env::var_os(DIES_SWEEPING_ENV) else {
            return;
        };
        let d = PathBuf::from(dir);
        let root = d.join("root");
        let slot = root.join("j0");
        owner_dirs(&slot).unwrap();
        let pidfile = d.join("left.pid");
        let nonce = nonce();
        let mut cmd = command("/bin/sh");
        cmd.arg("-c")
            .arg(
                "/usr/bin/env -i /bin/sh -c 'trap \"\" TERM; exec /bin/sleep 600' & \
                 echo $! > \"$1\"; wait",
            )
            .arg("sh")
            .arg(&pidfile)
            .env(CHECK_VAR, &nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let check = cmd.spawn().unwrap();
        let pid = i32::try_from(check.id()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let member: u32 = loop {
            if let Some(p) = fs::read_to_string(&pidfile)
                .ok()
                .and_then(|t| t.trim().parse().ok())
            {
                break p;
            }
            assert!(
                Instant::now() < deadline,
                "the root never started its member"
            );
            aterm_uds::deadline::poll_yield();
        };
        assert!(
            runs_from(member, Path::new("/bin/sleep")),
            "the member never ran its program"
        );
        fs::write(
            d.join("member.pid"),
            format!("{member} {}", serial_of(member)),
        )
        .unwrap();
        let mark = proc_tree::Mark {
            nonce: Some(nonce.clone()),
            root: Some((pid, serial_of(check.id()))),
            kept: BTreeSet::new(),
        };
        let record = root.join(".check.0");
        let rec = CheckRecord {
            nonce: Some(nonce),
            boot: aterm_uds::process::boot_id(),
            lane: this_lane(),
            dir: dir_id(&root),
            root: mark.root,
            kept: BTreeSet::new(),
        };
        fs::write(&record, rec.text()).unwrap();
        let live = proc_tree::Live {
            mark: &mark,
            held: Some(pid),
            slot: &slot,
            record: &record,
        };
        let left = proc_tree::end_marked_with(
            &mut DiesAfterSigterm(live, false),
            &slot,
            proc_tree::Act::End,
        );
        panic!("the lane outlived its SIGTERM: {left:?}");
    }

    /// A lane killed between its `SIGTERM` and its `SIGKILL` leaves the record keeping what the
    /// `SIGTERM` reached, so the next reap ends a survivor whose ties the `SIGTERM` ended (its
    /// parent, the root, which alone proved the session) and takes the record.
    #[test]
    fn what_a_dead_lanes_sigterm_reached_is_ended_by_the_next_reap() {
        probe_a_lane_that_dies_after_its_sigterm_if_asked();
        use std::os::unix::process::ExitStatusExt as _;
        let d = tmp("trust-lane-dies-sweeping");
        let out = command(std::env::current_exe().unwrap())
            .args(["--exact", "--nocapture", "--test-threads=1", DIES_SWEEPING])
            .env(DIES_SWEEPING_ENV, d.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let named = fs::read_to_string(d.path().join("member.pid")).unwrap_or_default();
        let member = named.split_once(' ').map(|(pid, serial)| Leftover {
            pid: pid.parse().unwrap(),
            serial: serial.parse().unwrap(),
        });
        assert_eq!(
            out.status.signal(),
            Some(proc_tree::SIGKILL),
            "the probe lane: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let member = member.unwrap();
        assert!(
            member.running(),
            "the case: the member outlived the SIGTERM"
        );
        let root = d.path().join("root");
        let record = root.join(".check.0");
        reap_stale_checks(&root);
        assert!(
            ends(member.pid, member.serial),
            "what the dead lane's SIGTERM reached outlived the next reap"
        );
        reap_until_gone(
            &root,
            &record,
            "the reap kept the record of a check it ended",
            || {},
        );
    }

    /// The gate execs its program only on the lane's line, with exactly the variables the lane
    /// set; at its pipe's end it execs nothing.
    #[test]
    fn the_gate_runs_its_program_only_on_the_lanes_line_in_the_lanes_environment() {
        let d = tmp("trust-lane-gate");
        let gate = |program: &str, arg: &Path| {
            let mut cmd = command("/bin/sh");
            cmd.arg("-c")
                .arg(GATE)
                .arg(program)
                .arg(arg)
                .env_clear()
                .env("LANG", "C")
                .env(CHECK_VAR, "0f1e")
                .current_dir(d.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            cmd.spawn().unwrap()
        };
        let mut env = gate("/usr/bin/env", Path::new("-0"));
        open(&mut env).unwrap();
        let out = env.wait_with_output().unwrap();
        let mut seen: Vec<String> = out
            .stdout
            .split(|b| *b == 0)
            .filter(|w| !w.is_empty())
            .map(|w| String::from_utf8_lossy(w).into_owned())
            .collect();
        seen.sort();
        assert_eq!(seen, [format!("{CHECK_VAR}=0f1e"), "LANG=C".to_owned()]);
        let ran = d.path().join("ran");
        let mut touch = gate("/usr/bin/touch", &ran);
        drop(touch.stdin.take());
        assert!(!touch.wait().unwrap().success());
        assert!(!ran.exists(), "the gate ran its program at its pipe's end");
    }

    /// A targo whose path holds '=' starts no check and writes no record: env(1), which the
    /// gate execs it through, would read that path as an assignment.
    #[test]
    fn a_targo_env_would_read_as_an_assignment_starts_nothing() {
        let d = tmp("trust-lane-assignment");
        let root = d.path().join("root");
        owner_dirs(&root.join("j0")).unwrap();
        let driver = script_driver(&d.path().join("a=b"), "exit 0");
        let (sock, keeper) = quiet_keeper(d.path());
        let mut real = real_checks(
            d.path(),
            &root,
            &driver,
            &keeper,
            &sock,
            aterm_uds::process::process_serial,
        );
        let mut j = job("alpha");
        j.log = d.path().join("alpha.log");
        assert!(real.start(&mut j).is_err());
        assert!(!root.join(".check.0").exists());
    }

    /// A targo that cannot be exec'd fails the start, naming the exec error, and leaves no
    /// record or stamp, so the run stops as unstartable.
    #[test]
    fn a_targo_that_cannot_be_execd_starts_nothing() {
        use std::os::unix::fs::PermissionsExt as _;
        for (how, kind) in [
            ("gone", std::io::ErrorKind::NotFound),
            ("not executable", std::io::ErrorKind::PermissionDenied),
            ("a directory", std::io::ErrorKind::PermissionDenied),
        ] {
            let d = tmp("trust-lane-unexecutable");
            let root = d.path().join("root");
            owner_dirs(&root.join("j0")).unwrap();
            let driver = script_driver(d.path(), "exit 0");
            match how {
                "gone" => fs::remove_file(&driver.targo).unwrap(),
                "not executable" => {
                    fs::set_permissions(&driver.targo, fs::Permissions::from_mode(0o644)).unwrap();
                }
                _ => {
                    fs::remove_file(&driver.targo).unwrap();
                    fs::create_dir(&driver.targo).unwrap();
                }
            }
            let (sock, keeper) = quiet_keeper(d.path());
            let mut real = real_checks(
                d.path(),
                &root,
                &driver,
                &keeper,
                &sock,
                aterm_uds::process::process_serial,
            );
            let mut j = job("alpha");
            j.log = d.path().join("alpha.log");
            let Err(e) = real.start(&mut j) else {
                panic!("{how}: a check started");
            };
            assert_eq!(e.kind(), kind, "{how}: {e}");
            assert!(
                e.to_string().contains(&*driver.targo.to_string_lossy()),
                "{how}: {e}"
            );
            assert!(!root.join(".check.0").exists(), "{how}: a record is left");
            assert_eq!(
                aterm_uds::deadline::exec_stamp(&root.join("j0")),
                None,
                "{how}: the slot is held"
            );
        }
    }

    /// A check never starts over another run's record: its slot is held, and the record
    /// stays as it was.
    #[test]
    fn a_check_never_starts_over_another_runs_record() {
        let d = tmp("trust-lane-foreign");
        let root = d.path().join("root");
        owner_dirs(&root.join("j0")).unwrap();
        let foreign = marked_record(&nonce(), None, &root);
        fs::write(root.join(".check.0"), &foreign).unwrap();
        let driver = script_driver(d.path(), "exit 0");
        let (sock, keeper) = quiet_keeper(d.path());
        let mut real = real_checks(
            d.path(),
            &root,
            &driver,
            &keeper,
            &sock,
            aterm_uds::process::process_serial,
        );
        assert!(real.held(0));
        let mut j = job("alpha");
        j.log = d.path().join("alpha.log");
        let Err(e) = real.start(&mut j) else {
            panic!("a check started over another run's record");
        };
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
        assert_eq!(fs::read_to_string(root.join(".check.0")).unwrap(), foreign);
    }

    /// What a gone root left running a program from the slot carries its mark: the reap ends
    /// it, and the copy stays until the program showed it ran.
    #[test]
    fn what_a_gone_root_left_running_from_its_slot_is_ended_by_the_reap() {
        let mut d = aterm_uds::deadline::KeptUntilRan::new(tmp("trust-lane-exe").keep());
        let root = d.path().join("root");
        let slot = root.join("j0");
        owner_dirs(&slot).unwrap();
        aterm_uds::deadline::ad_hoc_copy(Path::new("/bin/cat"), &slot.join("gen"));
        let nonce = nonce();
        let mut cmd = command("/bin/sh");
        cmd.arg("-c")
            .arg("exec 3<&0; ./gen <&3 & wait")
            .current_dir(&slot)
            .env(CHECK_VAR, &nonce)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();
        let serial = serial_of(pid);
        let input = aterm_uds::deadline::cat_echoes(&mut child);
        d.ran();
        let gen_pid = command("/bin/ps")
            .args(["-A", "-o", "pid=,ppid="])
            .output()
            .unwrap();
        let gen_pid: u32 = String::from_utf8_lossy(&gen_pid.stdout)
            .lines()
            .find_map(|l| {
                let mut f = l.split_whitespace();
                let p = f.next()?.parse().ok()?;
                (f.next()?.parse::<u32>().ok()? == pid).then_some(p)
            })
            .unwrap();
        let gen_serial = serial_of(gen_pid);
        child.kill().unwrap();
        child.wait().unwrap();
        let record = root.join(".check.0");
        let named = Some((i32::try_from(pid).unwrap(), serial));
        fs::write(&record, record_at(&nonce, named, &root)).unwrap();
        reap_stale_checks(&root);
        let gone = ends(gen_pid, gen_serial);
        drop(input);
        assert!(gone, "what ran from the slot outlived the reap");
        reap_until_gone(&root, &record, "the record of what was ended stayed", || {});
        assert_eq!(held_slots(&root), [slot]);
    }

    /// An earlier build's record holds its slot through runs, unsignalled, while its gone root's
    /// session is left: a `-p` run uses another slot and a filing run is refused. The reap that
    /// finds the session empty takes the record, and its stamp refuses filing until it is spent.
    #[test]
    fn an_earlier_builds_record_holds_its_slot_through_runs() {
        let mut s = site(Scope::Packages(vec!["alpha".into()]));
        let root = root_of(&s);
        let slot = root.join("j0");
        owner_dirs(&slot).unwrap();
        let g = rootless_group(s._dir.path(), None, None, "");
        let record = root.join(".check.0");
        fs::write(&record, format!("{} {}\n", g.root, g.lstart)).unwrap();

        let (outcome, said, _) = run_site(&mut s);
        assert_eq!(outcome, Outcome::Pass, "{said}");
        assert_eq!(s.started, ["alpha"], "{said}");
        assert!(
            !slot.join("debug/deps/alpha.d").exists(),
            "no check starts in a held slot: {said}"
        );
        assert!(record.exists() && slot.is_dir(), "{said}");
        let kept = format!(
            "kept {}: an earlier build's record of the check a stopped run left there",
            slot.display()
        );
        assert!(said.contains(&kept), "{said}");
        let stamp = slot.join(aterm_uds::deadline::EXEC_STAMP);
        let spent = u64::MAX.to_string();

        s.cfg.scope = Scope::WholeTree;
        fs::write(&stamp, &spent).unwrap();
        let (outcome, said, filed) = run_site(&mut s);
        assert!(
            g.left.running(),
            "no run signals what an earlier build's record names: {said}"
        );
        assert!(
            matches!(&outcome, Outcome::CouldNotRun(why) if why.contains("names no mark")),
            "a member of its gone root's session holds it past the stamp: {outcome:?}: {said}"
        );
        assert!(filed.is_empty() && record.exists(), "{said}");

        assert!(g.end(), "the group outlived its kill");
        fs::write(&stamp, &spent).unwrap();
        reap_until_gone(
            &root,
            &record,
            "the record goes once the session is empty",
            || {},
        );
        let (outcome, said, filed) = run_site(&mut s);
        assert!(
            matches!(&outcome, Outcome::CouldNotRun(why) if why.contains("may have left a program")),
            "the stamp the reap that took the record wrote holds the slot: {outcome:?}: {said}"
        );
        assert!(filed.is_empty() && !record.exists(), "{said}");

        fs::write(&stamp, &spent).unwrap();
        let (outcome, said, filed) = run_site(&mut s);
        assert_eq!((outcome, filed.len()), (Outcome::Pass, 1), "{said}");
        assert!(!slot.exists(), "{said}");
    }

    /// Ending a check ends all that carries its mark: its tree, a grandchild that left the
    /// root's group and session, and an orphan launchd (or a Linux subreaper) adopted. The root
    /// execs `/bin/sleep`, so only the end ends it.
    #[test]
    fn ending_a_check_ends_its_whole_tree() {
        let pgid =
            |pid: u32| aterm_uds::process::read_lineage(i32::try_from(pid).ok()?).map(|l| l.pgid);
        let d = tmp("trust-lane-tree");
        let pidfile = d.path().join("grandchild.pid");
        let orphanfile = d.path().join("orphan.pid");
        let nonce = nonce();
        let mut cmd = command("/bin/sh");
        cmd.arg("-c")
            .arg(
                "/usr/bin/perl -MPOSIX -e 'POSIX::setsid() or die; exec @ARGV' /bin/sleep 600 & \
                 echo $! > \"$1\"; (\"$3\" 600 & echo $! > \"$2\"); exec /bin/sleep 600",
            )
            .arg("sh")
            .arg(&pidfile)
            .arg(&orphanfile)
            .arg(readable("/bin/sleep"))
            .env(CHECK_VAR, &nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        proc_tree::new_session(&mut cmd);
        let child = cmd.spawn().unwrap();
        let root = child.id();
        let target = d.path().join("j0");
        owner_dirs(&target).unwrap();
        let mut check = RealCheck {
            child,
            record: d.path().join(".check.0"),
            target: target.clone(),
            mark: proc_tree::Mark {
                nonce: Some(nonce.clone()),
                root: Some((i32::try_from(root).unwrap(), serial_of(root))),
                kept: BTreeSet::new(),
            },
            ended: None,
            times: TreeTimes::default(),
            read_at: None,
            sweep: || None,
        };
        let read = |file: &Path| {
            fs::read_to_string(file)
                .ok()
                .and_then(|t| t.trim().parse::<u32>().ok())
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        let (grandchild, orphan) = loop {
            if let (Some(g), Some(o)) = (read(&pidfile), read(&orphanfile))
                && pgid(g) == Some(g)
                && outside_tree(o, root)
            {
                break (Leftover::new(g), Leftover::new(o));
            }
            assert!(
                Instant::now() < deadline,
                "the grandchild never led a group of its own, or the orphan never left the tree"
            );
            aterm_uds::deadline::poll_yield();
        };
        assert!(
            runs_from(orphan.pid, &readable("/bin/sleep")),
            "the orphan never ran its program"
        );
        assert_eq!(pgid(root), Some(root));
        assert_ne!(
            pgid(grandchild.pid),
            pgid(root),
            "the case: a group signal would miss it"
        );
        assert_eq!(
            pgid(orphan.pid),
            Some(root),
            "the case: a walk of the root's tree would miss it"
        );
        assert_eq!(check.poll().unwrap(), None);
        fs::write(
            d.path().join(".check.0"),
            marked_record(&nonce, Some(root), d.path()),
        )
        .unwrap();
        let ended = check.end();
        assert!(ended.code > 128, "{ended:?}");
        assert!(ended.maxrss > 0 && ended.cpu > Duration::ZERO, "{ended:?}");
        assert_eq!(check.poll().unwrap(), Some(ended));
        assert_eq!(check.end(), ended);
        assert_eq!(held_slots(d.path()), [target]);
        reap_until_gone(
            d.path(),
            &d.path().join(".check.0"),
            "the record stayed once the lane saw the end it made, the slot stamped",
            || {},
        );
        assert!(
            ends(grandchild.pid, grandchild.serial),
            "the grandchild outlived its check"
        );
        assert!(
            ends(orphan.pid, orphan.serial),
            "the orphan outlived its check"
        );

        let target = d.path().join("j1");
        owner_dirs(&target).unwrap();
        let child = command("/usr/bin/true")
            .env(CHECK_VAR, &nonce)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        // Its sweep lists the root alone: whether a clean exit stamps the slot is judged on a
        // whole listing, which a loaded machine's churn may not give.
        let only_root = child.id();
        proc_tree::LISTED.with(|l| *l.borrow_mut() = Some(vec![only_root]));
        fs::write(d.path().join(".check.1"), "the lane's own\n").unwrap();
        let mut exited = RealCheck {
            mark: proc_tree::Mark {
                nonce: Some(nonce),
                root: None,
                kept: BTreeSet::new(),
            },
            child,
            record: d.path().join(".check.1"),
            target: target.clone(),
            ended: None,
            times: TreeTimes::default(),
            read_at: None,
            sweep: || None,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        let ended = loop {
            if let Some(ended) = exited.poll().unwrap() {
                break ended;
            }
            assert!(Instant::now() < deadline, "true never exited");
            aterm_uds::deadline::poll_yield();
        };
        proc_tree::LISTED.with(|l| *l.borrow_mut() = None);
        assert_eq!((ended.code, exited.end()), (0, ended));
        assert!(!target.join(aterm_uds::deadline::EXEC_STAMP).exists());
        assert!(
            !d.path().join(".check.1").exists(),
            "the record goes once the lane saw the check's own exit, the slot not stamped"
        );
    }

    /// A root that exits other than 0, though nothing of the check is left, stamps its slot:
    /// cargo fails a build whose build script was killed at its first exec from the slot, which
    /// macOS goes on assessing.
    #[test]
    fn a_check_that_failed_holds_its_slot() {
        let d = tmp("trust-lane-failed");
        let target = d.path().join("j0");
        owner_dirs(&target).unwrap();
        let record = d.path().join(".check.0");
        let nonce = nonce();
        let child = command("/bin/sh")
            .arg("-c")
            .arg("/bin/sleep 600 & kill -KILL $!; wait $!; exit 101")
            .env(CHECK_VAR, &nonce)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        fs::write(&record, "the lane's own\n").unwrap();
        let mut check = RealCheck {
            mark: proc_tree::Mark {
                nonce: Some(nonce),
                root: None,
                kept: BTreeSet::new(),
            },
            child,
            record: record.clone(),
            target: target.clone(),
            ended: None,
            times: TreeTimes::default(),
            read_at: None,
            sweep: || None,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        let ended = loop {
            if let Some(ended) = check.poll().unwrap() {
                break ended;
            }
            assert!(Instant::now() < deadline, "the root never exited");
            aterm_uds::deadline::poll_yield();
        };
        assert_eq!(ended.code, 101);
        assert!(
            target.join(aterm_uds::deadline::EXEC_STAMP).is_file(),
            "a failed check's slot is stamped"
        );
        reap_until_gone(
            d.path(),
            &record,
            "the record goes once the stamp holds the slot",
            || {},
        );
        assert_eq!(
            clear_slots(d.path()).unwrap(),
            std::slice::from_ref(&target)
        );
        assert!(target.is_dir());
    }

    fn stand_in(sock: &Path, sha: &'static str) -> std::thread::JoinHandle<()> {
        stand_in_saying(sock, sha, "{\"active\":[]}".into())
    }

    /// A stand-in trustd whose IDENTITY names `sha` and whose STATUS answers `status`.
    fn stand_in_saying(
        sock: &Path,
        sha: &'static str,
        status: String,
    ) -> std::thread::JoinHandle<()> {
        use std::io::{BufRead as _, BufReader as Reader};
        let listener = std::os::unix::net::UnixListener::bind(sock).unwrap();
        std::thread::spawn(move || {
            loop {
                let Ok((conn, _)) = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                ) else {
                    return;
                };
                let status = status.clone();
                std::thread::spawn(move || {
                    let mut w = conn.try_clone().unwrap();
                    for line in Reader::new(conn).lines() {
                        let Ok(line) = line else { return };
                        let reply = match line.split_whitespace().next() {
                            Some("IDENTITY") => format!(
                                "{{\"executable_sha256\":\"{sha}\",\"release\":\"1.99\",\"commit\":\"abc\"}}"
                            ),
                            Some("STATUS") => status.clone(),
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
        aterm_tempfile::Builder::new()
            .prefix(&format!("tl{tag}-"))
            .tempdir_in("/tmp")
            .unwrap()
    }

    /// A test's clock for a wait on the socket: each nap moves it on at once, and `at` hears
    /// each moment it reaches.
    struct Sim<'a> {
        now: Duration,
        at: &'a mut dyn FnMut(Duration),
    }

    impl trustd::Clock for Sim<'_> {
        fn waited(&self) -> Duration {
            self.now
        }
        fn nap(&mut self, d: Duration, stop: &dyn Fn() -> bool) -> bool {
            self.now += d;
            (self.at)(self.now);
            stop()
        }
    }

    /// STATUS naming one lease, this process's, as the lane's keeper takes it.
    fn leased_status() -> String {
        format!(
            "{{\"version\":1,\"active\":[{{\"pid\":{},\"bytes\":1,\"label\":\"trust lane holds trustd for its run\",\"since_secs\":5,\"token\":\"t\"}}]}}",
            std::process::id()
        )
    }

    /// D-TRUSTD-330S: a trustd of another build, which another run's lease keeps serving, is
    /// waited for past the 330 s the lane used to give it, naming what serves the socket and who
    /// holds its lease as the wait starts and once a minute, and the run goes on at the first
    /// look after it retires. NEGATIVE CONTROL: a fixed limit put back ends this wait before the
    /// trustd retires at 400 s, and the test fails.
    #[test]
    fn a_trustd_of_another_build_is_waited_for_naming_its_leases_until_it_retires() {
        let private = short_dir("w");
        let sock = private.path().join("w.sock");
        let _t = stand_in_saying(&sock, "bb", leased_status());
        let mut out = Vec::new();
        let gone = sock.clone();
        let mut retire = |now: Duration| {
            if now >= Duration::from_secs(400) {
                let _ = fs::remove_file(&gone);
            }
        };
        let mut clock = Sim {
            now: Duration::ZERO,
            at: &mut retire,
        };
        let r = trustd::wait_on(
            &sock,
            "aa",
            named_lock::Wait::Block,
            &mut out,
            &|| false,
            &mut clock,
        );
        assert_eq!(r, Ok(()));
        assert_eq!(clock.now, Duration::from_secs(400), "past 330 s");
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        let me = format!(
            "leased by pid {} (trust lane holds trustd for its run)",
            std::process::id()
        );
        assert!(
            lines[0].starts_with(
                "trust: waiting for this build's trustd or none: a trustd of another build serves"
            ) && lines[0].contains(&me),
            "{out}"
        );
        let minutes: Vec<&str> = lines
            .iter()
            .filter(|l| l.starts_with("trust: still waiting"))
            .copied()
            .collect();
        assert_eq!(minutes.len(), 6, "a line a minute: {out}");
        assert!(minutes.iter().all(|l| l.contains(&me)), "{out}");
        assert!(minutes[5].contains("(6 min)"), "{out}");
        assert_eq!(
            lines.last().copied(),
            Some(
                format!(
                    "trust: the socket is usable after 6 min: nothing serves {}",
                    sock.display()
                )
                .as_str()
            ),
            "{out}"
        );
    }

    /// The wait for a trustd of another build ends when the run is interrupted (`stop`, which
    /// the lane's interrupt and WAIT-INTERRUPT's watch answer), naming what held the socket, and
    /// `--no-wait` answers at once with no wait line. NEGATIVE CONTROL: this build's trustd, or
    /// none, is used at once, with nothing said.
    #[test]
    fn an_interrupt_or_no_wait_ends_the_wait_for_another_builds_trustd() {
        let private = short_dir("i");
        let d = private.path();
        let foreign = d.join("f.sock");
        let _f = stand_in_saying(&foreign, "bb", leased_status());
        let naps = std::cell::Cell::new(0_u32);
        let mut count = |_: Duration| naps.set(naps.get() + 1);
        let mut clock = Sim {
            now: Duration::ZERO,
            at: &mut count,
        };
        let mut out = Vec::new();
        let r = trustd::wait_on(
            &foreign,
            "aa",
            named_lock::Wait::Block,
            &mut out,
            &|| naps.get() >= 3,
            &mut clock,
        );
        let Err(trustd::Unusable::Stopped(held)) = r else {
            panic!("{r:?}");
        };
        assert!(held.contains("a trustd of another build serves"), "{held}");
        assert!(
            held.contains(&format!("pid {}", std::process::id())),
            "{held}"
        );
        assert_eq!(
            clock.now,
            Duration::from_secs(15),
            "ended at the nap it was stopped in"
        );

        let mut out = Vec::new();
        let refused =
            trustd::wait_usable(&foreign, "aa", named_lock::Wait::No, &mut out, &|| false)
                .expect_err("--no-wait does not wait");
        assert!(
            refused.contains("a trustd of another build serves")
                && refused.contains("--no-wait does not wait for it")
                && refused.contains("trust lane holds trustd for its run"),
            "{refused}"
        );
        assert!(out.is_empty(), "--no-wait says nothing of a wait");

        let ours = d.join("o.sock");
        let _o = stand_in(&ours, "aa");
        for sock in [ours, d.join("absent.sock")] {
            let mut out = Vec::new();
            let mut never = |_: Duration| panic!("no nap");
            let mut clock = Sim {
                now: Duration::ZERO,
                at: &mut never,
            };
            let r = trustd::wait_on(
                &sock,
                "aa",
                named_lock::Wait::No,
                &mut out,
                &|| false,
                &mut clock,
            );
            assert_eq!(r, Ok(()), "{}", sock.display());
            assert!(out.is_empty());
        }
    }

    #[test]
    fn the_probe_tells_this_builds_trustd_from_another() {
        let private = short_dir("p");
        let d = private.path();
        let wait = aterm_spec::wait::MUST_HAPPEN;
        let none = trustd::probe(&d.join("absent.sock"), "aa", wait);
        assert_eq!(none.held, trustd::Held::None);
        let ours = d.join("ours.sock");
        let _a = stand_in(&ours, "aa");
        assert_eq!(trustd::probe(&ours, "aa", wait).held, trustd::Held::Ours);
        let other = trustd::probe(&ours, "bb", wait);
        assert_eq!(other.held, trustd::Held::Foreign);
        assert!(other.line().starts_with("foreign\t"), "{}", other.line());
        let mute = d.join("mute.sock");
        let listener = std::os::unix::net::UnixListener::bind(&mute).unwrap();
        let _held = std::thread::spawn(move || {
            let conn = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("the silent probe connects");
            std::thread::park_timeout(Duration::from_secs(60));
            drop(conn);
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
            aterm_uds::deadline::poll_yield();
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
            aterm_uds::deadline::poll_yield();
        }
        assert!(!other.held(), "no grant on another build's trustd");
        drop(other);
    }

    #[test]
    fn the_keeper_ends_while_a_trustd_keeps_it_waiting() {
        use std::io::{BufRead as _, Read as _};

        let private = short_dir("m");
        let d = private.path();
        let sock = d.join("m.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mute = std::thread::spawn(move || {
            let (conn, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
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
