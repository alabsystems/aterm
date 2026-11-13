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
/// unclaimed (its ledger unusable, its claim file gone, its timing unqualified, or a look taken at
/// once on a cohort-aware ledger short of room, which that ledger refuses), never one a look.
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
                "capacity: {why}: it can never be admitted here, so it starts at once, holding its claim where its ledger admits one short of room"
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
                // A look its ledger let decide no claim: one short of room on a cohort-aware
                // ledger, which admits only what fits (`outcome=refused`), or a ledger that
                // could not be used. Either way it starts as `--admit now` asked, unclaimed.
                Err(e) => {
                    say(&format!("capacity: {what} starts unclaimed: {e}"));
                    head.owed.decided(Err(&e));
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
    let cores = aterm_cores::available_parallelism().map_or(1, std::num::NonZero::get);
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

#[path = "trust_lane/proc_tree.rs"]
pub mod proc_tree;

#[path = "trust_lane/trustd.rs"]
pub mod trustd;

#[cfg(test)]
#[path = "trust_lane_tests.rs"]
mod tests;
