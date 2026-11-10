// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cut-shaped profile experiments. No path here executes the release product.

use crate::{buildplan, snapshot};
use std::path::Path;

#[path = "profile_pilot_summary.rs"]
mod summary;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arm {
    Fat1,
    Fat1Lines,
    Fat16,
    Fat16Lines,
    Thin16,
    Thin16Lines,
    Thin1,
    FatGui16,
}

#[derive(Clone, Copy)]
pub(crate) struct Trial {
    pub arm: Arm,
    pub anatomy: bool,
    pub jobs: Option<u32>,
}

impl Arm {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Fat1 => "fat1",
            Self::Fat1Lines => "fat1-lines",
            Self::Fat16 => "fat16",
            Self::Fat16Lines => "fat16-lines",
            Self::Thin16 => "thin16",
            Self::Thin16Lines => "thin16-lines",
            Self::Thin1 => "thin1",
            Self::FatGui16 => "fat-gui16",
        }
    }
    pub(crate) fn environment(self) -> [(&'static str, &'static str); 3] {
        [
            (
                "CARGO_PROFILE_RELEASE_LTO",
                if matches!(self, Self::Thin1 | Self::Thin16 | Self::Thin16Lines) {
                    "thin"
                } else {
                    "fat"
                },
            ),
            (
                "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
                if matches!(
                    self,
                    Self::Fat1 | Self::Fat1Lines | Self::Thin1 | Self::FatGui16
                ) {
                    "1"
                } else {
                    "16"
                },
            ),
            (
                "CARGO_PROFILE_RELEASE_DEBUG",
                if !matches!(self, Self::Fat1Lines | Self::Fat16Lines | Self::Thin16Lines) {
                    "1"
                } else {
                    "line-tables-only"
                },
            ),
        ]
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Options {
    arms: Vec<Arm>,
    takes: usize,
    inspect: Option<std::path::PathBuf>,
    resume: Option<std::path::PathBuf>,
    jobs: Option<u32>,
    compat_jobs: Option<u32>,
    summary: Option<std::path::PathBuf>,
    anatomy: bool,
}

impl Options {
    /// One arm is a diagnostic take; comparing arms uses the full two-take experiment.
    fn diagnostic_anatomy(&self) -> bool {
        self.anatomy && self.takes == 1
    }
}

pub(crate) fn parse(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut args = args;
    let mut takes_named = false;
    let mut opts = Options {
        arms: Vec::new(),
        takes: 2,
        inspect: None,
        resume: None,
        jobs: None,
        compat_jobs: None,
        summary: None,
        anatomy: false,
    };
    while let Some(flag) = args.next() {
        if flag == "--anatomy" {
            opts.anatomy = true;
            continue;
        }
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--arm" => {
                let arm = match value.as_str() {
                    "fat1" => Arm::Fat1,
                    "fat1-lines" => Arm::Fat1Lines,
                    "fat16" => Arm::Fat16,
                    "fat16-lines" => Arm::Fat16Lines,
                    "thin16" => Arm::Thin16,
                    "thin16-lines" => Arm::Thin16Lines,
                    "thin1" => Arm::Thin1,
                    "fat-gui16" => Arm::FatGui16,
                    _ => return Err(format!("unknown pilot arm {value}")),
                };
                if opts.arms.contains(&arm) {
                    return Err(format!("duplicate arm {value}"));
                }
                opts.arms.push(arm);
            }
            "--takes" => {
                takes_named = true;
                opts.takes = value.parse().map_err(|_| "invalid --takes")?;
            }
            "--inspect" => opts.inspect = Some(value.into()),
            "--resume" => opts.resume = Some(value.into()),
            "--summary" => opts.summary = Some(value.into()),
            "--jobs" | "--compat-jobs" => {
                let jobs = value
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or("pilot jobs must be a positive integer")?;
                if flag == "--jobs" {
                    opts.jobs = Some(jobs);
                } else {
                    opts.compat_jobs = Some(jobs);
                }
            }
            _ => return Err(format!("unknown profile-pilot flag {flag}")),
        }
    }
    if opts.summary.is_some() {
        if opts.inspect.is_some()
            || opts.resume.is_some()
            || !opts.arms.is_empty()
            || opts.anatomy
            || takes_named
            || opts.jobs.is_some()
            || opts.compat_jobs.is_some()
        {
            return Err("--summary takes only a retained pilot directory".into());
        }
        return Ok(opts);
    }
    if opts.inspect.is_some() && opts.resume.is_some() {
        return Err("--inspect and --resume are exclusive".into());
    }
    if opts.anatomy {
        if opts.inspect.is_some() || takes_named {
            return Err("--anatomy takes no --inspect or --takes override".into());
        }
        if opts.arms.is_empty() {
            opts.arms.push(Arm::Fat1);
        }
        opts.takes = if opts.arms.len() == 1 { 1 } else { 2 };
        return Ok(opts);
    }
    if opts.inspect.is_some() && !opts.arms.is_empty() {
        return Err("--inspect takes no --arm".into());
    }
    if (opts.inspect.is_none() && opts.arms.is_empty()) || opts.takes != 2 {
        return Err(
            "profile-pilot requires --arm and exactly two takes for reproducibility".into(),
        );
    }
    Ok(opts)
}

fn order(opts: &Options) -> Vec<(usize, Arm)> {
    (0..opts.takes)
        .flat_map(|take| {
            let mut arms = opts.arms.clone();
            if take % 2 == 1 {
                arms.reverse();
            }
            arms.into_iter().map(move |arm| (take, arm))
        })
        .collect()
}

/// Lay out a guest-only input from the measured native image and frozen resources.
/// Assembly copies bytes and installs the frozen release identity without changing
/// normalized product content. It neither rebuilds nor executes the product.
pub(crate) fn runtime_app(
    repo: &Path,
    root: &Path,
    binary: &Path,
    build: u64,
    identity: &aterm_stamp::Stamp,
) -> Result<std::path::PathBuf, String> {
    if !identity.release_build {
        return Err("Q4 needs a release identity, not an unstamped pilot image".into());
    }
    let app = root.join("aterm.app");
    if crate::bundle::exists_or_symlink(&app) {
        return Err("pilot runtime app already exists".into());
    }
    std::fs::create_dir_all(root).map_err(|e| format!("pilot runtime input: {e}"))?;
    let copied = root.join("runtime-image.noindex");
    crate::durable::copy_regular_fresh(binary, &copied).map_err(|e| e.to_string())?;
    crate::postlink::resign(&copied)?;
    let content = crate::postlink::normalized(
        &crate::durable::read_regular_bytes(&copied, u64::MAX).map_err(|e| e.to_string())?,
    )?;
    crate::postlink::stamp(&copied, &aterm_stamp::encode(identity)?)?;
    crate::postlink::resign(&copied)?;
    if crate::postlink::normalized(
        &crate::durable::read_regular_bytes(&copied, u64::MAX).map_err(|e| e.to_string())?,
    )? != content
    {
        return Err("pilot runtime stamping changed product content".into());
    }
    crate::bundle::assemble_at(
        &crate::bundle::BundleSpec {
            repo_root: repo.to_path_buf(),
            out_dir: root.to_path_buf(),
            short_version: identity.app_version.to_owned(),
            build_number: build,
            bundle_id: crate::bundle::BUNDLE_ID.to_owned(),
            git_commit: identity.git_commit.to_owned(),
            aterm_bin: copied.clone(),
        },
        &app,
    )?;
    // This staging copy was never executed; the readonly bundle is the guest input.
    std::fs::remove_file(copied).map_err(|e| format!("retire pilot runtime staging: {e}"))?;
    Ok(app)
}

/// Cargo's artifact path is authoritative across both traditional deps folders
/// and Trust's per-unit output layout.
pub(crate) fn gui_rlib(log: &str) -> Option<std::path::PathBuf> {
    let mut found = std::collections::BTreeSet::new();
    for line in log.lines() {
        if !line.contains("aterm_gui") || !line.contains("compiler-artifact") {
            continue;
        }
        let Ok(row) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        if row["reason"].as_str() != Some("compiler-artifact")
            || row["target"]["name"].as_str() != Some("aterm_gui")
        {
            continue;
        }
        if let Some(files) = row["filenames"].as_array() {
            for file in files {
                if let Some(path) = file.as_str().map(std::path::PathBuf::from)
                    && path.is_absolute()
                    && path.extension().is_some_and(|ext| ext == "rlib")
                {
                    found.insert(path);
                }
            }
        }
    }
    if found.len() == 1 {
        found.pop_first()
    } else {
        None
    }
}

/// The interval from the artifact Cargo actually emitted to the final image.
pub(crate) fn gui_link_interval(log: &Path, binary: &Path) -> Result<Option<f64>, String> {
    let messages = crate::durable::read_text(&log.with_extension("messages.jsonl"))
        .map_err(|e| e.to_string())?;
    let Some(rlib) = gui_rlib(&messages) else {
        return Ok(None);
    };
    let from = std::fs::metadata(&rlib)
        .and_then(|m| m.modified())
        .map_err(|e| format!("pilot GUI artifact {}: {e}", rlib.display()))?;
    let to = std::fs::metadata(binary)
        .and_then(|m| m.modified())
        .map_err(|e| format!("pilot executable {}: {e}", binary.display()))?;
    Ok(to.duration_since(from).ok().map(|span| span.as_secs_f64()))
}

/// Observe load and new members of this command's process group while work runs.
/// Initial members (the diagnostic, its shell and supervisors) are excluded. Short-lived
/// processes between reads can be missed; these samples supplement, never replace, wait4.
pub(crate) fn with_load<T>(
    root: &Path,
    work: impl FnOnce() -> T,
) -> Result<(T, Option<f64>), String> {
    use std::io::Write as _;
    let file = crate::durable::create_fresh(&root.join("build-resources.jsonl"))
        .map_err(|e| format!("create pilot resource samples: {e}"))?;
    let (group, initial) = i32::try_from(std::process::id())
        .ok()
        .and_then(aterm_uds::process::read_lineage)
        .and_then(|lineage| {
            aterm_sysprobe::group_usage(lineage.pgid).map(|members| {
                (
                    Some(lineage.pgid),
                    members
                        .into_iter()
                        .map(|member| (member.pid, member.start))
                        .collect(),
                )
            })
        })
        .unwrap_or_default();
    let started = std::time::Instant::now();
    std::thread::scope(|scope| -> Result<_, String> {
        let (done, receiver) = std::sync::mpsc::channel::<()>();
        let sampler = scope.spawn(move || {
            let mut file = std::io::BufWriter::new(file);
            let mut log = None;
            let mut pending = Vec::new();
            let mut completed = false;
            let mut maximum: Option<f64> = None;
            loop {
                if let Some(load) = aterm_sysprobe::load_average() {
                    maximum = Some(maximum.map_or(load[0], |old| old.max(load[0])));
                }
                let members = group.and_then(aterm_sysprobe::group_usage);
                if log.is_none() {
                    log = crate::durable::open_regular_file(&root.join("build.log"))
                        .ok()
                        .map(std::io::BufReader::new);
                }
                let passes = match &mut log {
                    Some(log) => compiler_passes(log, &mut pending).map_err(|e| e.to_string())?,
                    None => Vec::new(),
                };
                let sample =
                    resource_sample(started.elapsed().as_secs_f64(), &initial, members, passes);
                writeln!(file, "{sample}").map_err(|e| e.to_string())?;
                if completed {
                    break;
                }
                completed = !matches!(
                    receiver.recv_timeout(std::time::Duration::from_secs(1)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                );
            }
            file.flush().map_err(|e| e.to_string())?;
            Ok::<_, String>(maximum)
        });
        let result = work();
        drop(done);
        Ok((
            result,
            sampler
                .join()
                .map_err(|_| "pilot resource sampler panicked")??,
        ))
    })
}

/// Cargo relays rustc's untracked time-pass diagnostic. Polling its regular log avoids a
/// compiler/daemon holding an inherited pipe open. The preceding sample interval bounds
/// when the diagnostic reached the log; subtracting its duration estimates the pass start.
fn compiler_passes(
    log: &mut impl std::io::BufRead,
    pending: &mut Vec<u8>,
) -> std::io::Result<Vec<aterm_json::Value>> {
    let mut passes = Vec::new();
    loop {
        if log.read_until(b'\n', pending)? == 0 || pending.last() != Some(&b'\n') {
            break;
        }
        if let Some(json) = pending.strip_prefix(b"time: ")
            && let Ok(text) = std::str::from_utf8(json)
            && let Ok(pass) = aterm_json::from_str::<aterm_json::Value>(text)
        {
            passes.push(pass);
        }
        pending.clear();
    }
    Ok(passes)
}

/// Preserve the unit timeline before the successful pilot discards its cold intermediates.
pub(crate) fn retain_timings(target: &Path, root: &Path) -> Result<(), String> {
    std::fs::rename(target.join("cargo-timings"), root.join("cargo-timings"))
        .map_err(|e| format!("retain pilot Cargo timings: {e}"))
}

fn resource_sample(
    elapsed: f64,
    initial: &std::collections::BTreeSet<(u32, u64)>,
    members: Option<Vec<aterm_sysprobe::MemberUsage>>,
    compiler_passes: Vec<aterm_json::Value>,
) -> aterm_json::Value {
    let members = members.map(|members| {
        members
            .into_iter()
            .filter(|member| !initial.contains(&(member.pid, member.start)))
            .map(|member| {
                aterm_json::json!({
                    "pid": member.pid, "start": member.start, "cpu_ns": member.cpu_ns,
                    "runnable_ns": member.runnable_ns, "footprint_bytes": member.footprint,
                    "peak_footprint_bytes": member.peak_footprint,
                })
            })
            .collect::<Vec<_>>()
    });
    aterm_json::json!({"elapsed_s": elapsed, "members": members, "compiler_passes": compiler_passes})
}

pub(crate) fn run(repo: &Path, opts: &Options) -> Result<(), String> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err("profile-pilot requires macOS or native Linux".into());
    }
    if let Some(dir) = &opts.summary {
        let cache = snapshot::CacheRoot::for_repo(repo)?;
        return summary::run(&retained_root(&cache.path().join(".ubd/pilot"), dir)?);
    }
    let git = crate::ledger::GitCli::new(repo);
    let release = crate::release::release_dir(&git)?;
    let pid = std::process::id();
    let start =
        crate::release_steps::started_of(pid).ok_or("cannot establish pilot process identity")?;
    // The same claim as a cut closes the check/start race in both directions.
    let _claim = crate::release::RunClaim::take(
        &release.join(crate::release::RUNNING_FILE),
        (pid, &start),
        &release,
        &mut crate::release_steps::started_of,
    )
    .map_err(|e| format!("profile-pilot cannot run beside a release claim: {e:?}"))?;
    if let Some(dir) = &opts.inspect {
        let cache = snapshot::CacheRoot::for_repo(repo)?;
        let root = retained_root(&cache.path().join(".ubd/pilot"), dir)?;
        let image = root.join("aterm");
        let out = root.join(format!("inspection-{}.noindex", std::process::id()));
        snapshot::private_dir(&out, "pilot retained inspection")?;
        let copied = out.join("aterm");
        crate::durable::copy_regular_fresh(&image, &copied).map_err(|e| e.to_string())?;
        let inspection = inspect_symbols(&copied, &root, false)?;
        let (sha256, bytes) = crate::dmg::sha256_and_size(&copied)?;
        let report = aterm_json::Value::Object(aterm_json::Map::from([
            ("binary".to_string(), aterm_json::json!(copied)),
            (
                "binary_bytes".to_string(),
                aterm_json::Value::Number(bytes.into()),
            ),
            ("sha256".to_string(), aterm_json::Value::String(sha256)),
            ("inspection".to_string(), inspection),
        ]));
        crate::durable::write_fresh(&out.join("inspection.json"), report.to_string())
            .map_err(|e| e.to_string())?;
        println!("profile-pilot-inspection: {report}");
        return Ok(());
    }
    let commit = snapshot::git_text(repo, &["rev-parse", "HEAD"])?;
    let take = buildplan::Take::prepare(repo, commit.trim())?;
    let inputs = if cfg!(target_os = "macos") {
        Some(crate::build_inputs::current()?)
    } else {
        None
    };
    let cache = snapshot::CacheRoot::for_repo(repo)?;
    let pilot_cache = cache.path().join(".ubd/pilot");
    let (root, mut rows) = if let Some(resume) = &opts.resume {
        let root = retained_root(&pilot_cache, resume)?;
        let report: aterm_json::Value = aterm_json::from_str(
            &crate::durable::read_text(&root.join("report.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("pilot resume report: {e}"))?;
        if report["commit"].as_str() != Some(commit.trim())
            || report["source_digest"] != aterm_json::json!(take.snapshot.digest)
            || report["build_inputs"] != aterm_json::json!(inputs)
            || report["anatomy"] != aterm_json::json!(opts.anatomy)
        {
            return Err("pilot resume inputs differ from the retained comparison".into());
        }
        let rows = report["rows"]
            .as_array()
            .ok_or("pilot resume has no rows")?
            .clone();
        for row in &rows {
            let binary = row["binary"]
                .as_str()
                .ok_or("pilot resume row has no binary")?;
            let binary = std::path::Path::new(binary);
            retained_root(&root, binary)?;
            let (sha, bytes) = crate::dmg::sha256_and_size(binary)?;
            if row["sha256"] != aterm_json::json!(sha)
                || row["binary_bytes"] != aterm_json::json!(bytes)
            {
                return Err("pilot resume artifact changed".into());
            }
            if let Some(app) = row["runtime_app"].as_str() {
                let image = Path::new(app).join("Contents/MacOS/aterm");
                retained_root(&root, &image)?;
                if row["runtime_app_sha256"] != aterm_json::json!(crate::dmg::sha256_file(&image)?)
                {
                    return Err("pilot resume runtime artifact changed".into());
                }
            }
        }
        (root, rows)
    } else {
        let root = pilot_cache.join(format!("{}-{pid}", commit.trim()));
        std::fs::create_dir_all(root.parent().ok_or("pilot root has no parent")?)
            .map_err(|e| e.to_string())?;
        std::fs::create_dir(&root)
            .map_err(|e| format!("create unique pilot run (existing evidence is retained): {e}"))?;
        (root, Vec::new())
    };
    let hint = if opts.resume.is_some() {
        let app = rows
            .iter()
            .find_map(|row| row["runtime_app"].as_str())
            .ok_or("pilot resume needs a retained native runtime image for its frozen identity")?;
        let image = crate::durable::read_file(&Path::new(app).join("Contents/MacOS/aterm"))
            .map_err(|e| e.to_string())?;
        let hint = crate::postlink::index_hint(&image)?;
        (!hint.is_empty()).then_some(hint)
    } else {
        buildplan::index_hint_now()
    };
    if opts.resume.is_some() && (opts.jobs.is_some() || opts.compat_jobs.is_some()) {
        let excluded: Vec<_> = rows
            .iter()
            .filter(|row| {
                let jobs = if row["slice"].as_str() == Some("native") {
                    opts.jobs
                } else {
                    opts.compat_jobs
                };
                jobs.is_some_and(|jobs| row["granted_cores"].as_u64() != Some(u64::from(jobs)))
            })
            .cloned()
            .collect();
        if !excluded.is_empty() {
            crate::durable::write_fresh(&root.join(format!("exploratory-job-counts-{pid}.json")),
                aterm_json::json!({"reason": "build job count differs from the fixed comparison", "rows": excluded}).to_string())
                .map_err(|e| e.to_string())?;
            rows.retain(|row| !excluded.contains(row));
        }
    }
    // Identity and network-derived metadata are frozen once, across every arm.
    let build = snapshot::git_text(repo, &["show", "-s", "--format=%ct", commit.trim()])?
        .trim()
        .parse()
        .map_err(|_| "commit has no timestamp")?;
    for (n, arm) in order(opts) {
        let targets = if cfg!(target_os = "macos") {
            vec![None, Some("x86_64-apple-darwin")]
        } else {
            vec![None]
        };
        for target in targets {
            let base = root
                .join(arm.name())
                .join(format!("take{}", n + 1))
                .join(target.unwrap_or("native"));
            let jobs = if target.is_some() {
                opts.compat_jobs
            } else {
                opts.jobs
            };
            if rows.iter().any(|row| {
                row["arm"] == aterm_json::json!(arm.name())
                    && row["slice"] == aterm_json::json!(target.unwrap_or("native"))
                    && row["binary"]
                        .as_str()
                        .is_some_and(|binary| Path::new(binary).starts_with(&base))
            }) {
                println!(
                    "profile-pilot: retained {} take {} {:?}",
                    arm.name(),
                    n + 1,
                    target
                );
                continue;
            }
            let dir = jobs.map_or_else(|| base.clone(), |jobs| base.join(format!("jobs-{jobs}")));
            if dir.exists() {
                std::fs::rename(&dir, dir.with_extension(format!("failed-{pid}")))
                    .map_err(|e| format!("retain incomplete pilot take: {e}"))?;
            }
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            println!("profile-pilot: {} take {} {:?}", arm.name(), n + 1, target);
            let row = if cfg!(target_os = "linux") {
                buildplan::linux::pilot(
                    &take,
                    Trial {
                        arm,
                        anatomy: opts.anatomy,
                        jobs,
                    },
                    &dir,
                    build,
                    hint.as_deref(),
                )?
            } else {
                buildplan::pilot_build(
                    &take,
                    Trial {
                        arm,
                        anatomy: opts.anatomy,
                        jobs,
                    },
                    &dir,
                    target,
                    build,
                    hint.as_deref(),
                )?
            };
            let inspection_error = row["inspection"]["error"].as_str().map(str::to_owned);
            println!("profile-pilot-row: {row}");
            rows.push(row);
            let report = aterm_json::json!({"schema": 1, "commit": commit.trim(), "source_digest": take.snapshot.digest, "build_inputs": inputs, "rows": rows,
                "vm_rows": "not run: Q4 profile runtime qualification is not integrated", "linux_rows": "not run",
                "anatomy": opts.anatomy,
                "reproducibility": if opts.diagnostic_anatomy() { "not judged: one take" } else { "pending" },
                "library_benches": if opts.diagnostic_anatomy() { "not run: anatomy only" } else { "pending" },
                "wasm_ceilings": if opts.diagnostic_anatomy() { "not run: anatomy only" } else { "pending" }});
            crate::durable::write_fresh(&root.join("report.json"), report.to_string())
                .map_err(|e| e.to_string())?;
            if let Some(error) = inspection_error {
                return Err(format!(
                    "pilot inspection failed (build evidence retained): {error}"
                ));
            }
        }
    }
    if let Some(before) = &inputs
        && crate::build_inputs::current()? != *before
    {
        return Err("pilot compiler or SDK inputs changed during the comparison".into());
    }
    if opts.diagnostic_anatomy() {
        println!(
            "profile-pilot: anatomy recorded; one take does not judge reproducibility; report {}",
            root.join("report.json").display()
        );
        return Ok(());
    }
    validate(&rows)?;
    // Keep build ABBA uninterrupted by instruments that compete for the CPU.
    for (n, arm) in order(opts) {
        let dir = root.join(arm.name()).join(format!("host-checks-{}", n + 1));
        std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
        buildplan::pilot_host_checks(&take, arm, &dir, build, hint.as_deref())?;
    }
    crate::durable::write_fresh(
        &root.join("host-checks-completed"),
        b"vte-engine vte-marks wasm\n",
    )
    .map_err(|e| e.to_string())?;
    let report = aterm_json::json!({"schema": 1, "commit": commit.trim(), "source_digest": take.snapshot.digest, "build_inputs": inputs, "rows": rows, "anatomy": opts.anatomy,
        "vm_rows": "not run: Q4 profile runtime qualification is not integrated",
        "linux_rows": if cfg!(target_os = "linux") { "recorded" } else { "not run: invoke on native Linux" },
        "library_benches": "commands completed with exit 0; judgments in per-arm host-checks-{1,2}/vte-engine.log and vte-marks.log",
        "wasm_ceilings": "commands completed with exit 0; judgments in per-arm host-checks-{1,2}/wasm.log"});
    crate::durable::write_fresh(&root.join("report.json"), report.to_string())
        .map_err(|e| e.to_string())?;
    println!(
        "profile-pilot: reproducible host artifacts; report {}",
        root.join("report.json").display()
    );
    Ok(())
}

fn validate(rows: &[aterm_json::Value]) -> Result<(), String> {
    for row in rows {
        let peers: Vec<_> = rows
            .iter()
            .filter(|r| r["arm"] == row["arm"] && r["slice"] == row["slice"])
            .collect();
        if peers.len() != 2
            || peers.iter().any(|r| {
                r["sha256"] != row["sha256"]
                    || r["runtime_app_sha256"] != row["runtime_app_sha256"]
                    || r["inspection"]["dsym_dwarf_sha256"]
                        != row["inspection"]["dsym_dwarf_sha256"]
            })
        {
            return Err(format!(
                "non-reproducible arm {} slice {}",
                row["arm"], row["slice"]
            ));
        }
        let twin = match row["arm"].as_str() {
            Some("fat1-lines") => "fat1",
            Some("fat16-lines") => "fat16",
            Some("thin16-lines") => "thin16",
            _ => continue,
        };
        if let Some(base) = rows
            .iter()
            .find(|r| r["arm"].as_str() == Some(twin) && r["slice"] == row["slice"])
        {
            for field in ["sections", "symbols"] {
                if row["inspection"][field] != base["inspection"][field] {
                    return Err(format!("{field} differ between {twin} and {}", row["arm"]));
                }
            }
        }
    }
    Ok(())
}

fn tool(name: &str, args: &[&std::ffi::OsStr]) -> Result<String, String> {
    run_tool(name, args, false)
}

fn retained_root(cache: &Path, requested: &Path) -> Result<std::path::PathBuf, String> {
    let cache = std::fs::canonicalize(cache).map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(requested).map_err(|e| e.to_string())?;
    if root == cache || !root.starts_with(&cache) {
        return Err("pilot evidence must be inside its private retained root".into());
    }
    Ok(root)
}

fn run_tool(name: &str, args: &[&std::ffi::OsStr], lookup: bool) -> Result<String, String> {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("pilot-tool-")
        .suffix(".noindex")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("xcrun"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TMPDIR", scratch.path())
        .current_dir("/")
        .arg(name)
        .args(args)
        .output()
        .map_err(|e| format!("{name}: {e}"))?;
    tool_answer(name, out.status.code(), out.stdout, &out.stderr, lookup)
}

fn tool_answer(
    name: &str,
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: &[u8],
    lookup: bool,
) -> Result<String, String> {
    // dwarfdump's lookup returns 1 with no diagnostic when an address has no
    // DWARF entry. A malformed image or an interrupted tool remains an error.
    if code != Some(0) && !(lookup && code == Some(1) && stderr.is_empty()) {
        return Err(format!(
            "{name} exited {code:?}: {}",
            String::from_utf8_lossy(stderr)
        ));
    }
    String::from_utf8(stdout).map_err(|e| e.to_string())
}

fn demangle_symbol(symbol: &str) -> Result<String, String> {
    if symbol.starts_with("_R") || symbol.starts_with("_Z") {
        tool("llvm-cxxfilt", &[symbol.as_ref()]).map(|text| text.trim().to_owned())
    } else {
        Ok(symbol.to_owned())
    }
}

fn canonical_symbolication(
    text: &str,
    mut demangle: impl FnMut(&str) -> Result<String, String>,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Err("symbolicator gave no function name".into());
    }
    text.lines()
        .map(|line| {
            let (function, location) = line
                .split_once(" (in ")
                .ok_or("symbolicator gave no image and location")?;
            let (symbol, offset) = function
                .split_once(" + ")
                .map_or((function, None), |(name, offset)| (name, Some(offset)));
            let name = demangle(symbol)?;
            if name.is_empty() {
                return Err("symbolicator gave no function name".into());
            }
            let offset = offset.map_or(String::new(), |offset| format!(" + {offset}"));
            Ok(format!("{name}{offset} (in {location}"))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|lines| lines.join("\n"))
}

pub(crate) fn inspect(binary: &Path, root: &Path) -> Result<aterm_json::Value, String> {
    inspect_symbols(binary, root, true)
}

fn inspect_symbols(binary: &Path, root: &Path, extract: bool) -> Result<aterm_json::Value, String> {
    if cfg!(target_os = "linux") {
        return Ok(aterm_json::Value::Object(aterm_json::Map::from([
            ("sections".to_string(), aterm_json::Value::Null),
            ("symbols".to_string(), aterm_json::Value::Null),
            (
                "dsym".to_string(),
                aterm_json::Value::String("not applicable on Linux".to_string()),
            ),
        ])));
    }
    let dsym = root.join("aterm.dSYM");
    if extract {
        tool(
            "dsymutil",
            &[binary.as_os_str(), "-o".as_ref(), dsym.as_os_str()],
        )?;
    }
    let dwarf = dsym.join("Contents/Resources/DWARF/aterm");
    let (dsym_sha256, dsym_bytes) = crate::dmg::sha256_and_size(&dwarf)?;
    let image = crate::durable::read_file(binary).map_err(|e| e.to_string())?;
    let sections = sections(&image)?;
    let cpu = image.get(4..8).ok_or("Mach-O CPU type is absent")?;
    let arch = match u32::from_le_bytes(cpu.try_into().map_err(|_| "Mach-O CPU type")?) {
        0x0100_000c => "arm64",
        0x0100_0007 => "x86_64",
        _ => return Err("unsupported pilot Mach-O architecture".into()),
    };
    let names = tool("nm", &["-n".as_ref(), binary.as_os_str()])?;
    let addresses: Vec<_> = names
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let address = fields.next()?;
            let kind = fields.next()?;
            let name = fields.next()?;
            matches!(kind, "T" | "t").then_some((address, name))
        })
        .take(128)
        .collect();
    if addresses.is_empty() {
        return Err("no text addresses to symbolicate".into());
    }
    // Capture addresses before the cut's -x strip removes local symbol names.
    // The dSYM keeps those names; code/data must survive the strip unchanged.
    tool("strip", &["-x".as_ref(), binary.as_os_str()])?;
    let stripped = crate::durable::read_file(binary).map_err(|e| e.to_string())?;
    if self::sections(&stripped)? != sections {
        return Err("release strip changed code/data sections".into());
    }
    let mut symbols = Vec::new();
    for (address, name) in addresses {
        let lookup_address = format!("0x{address}");
        let lookup = run_tool(
            "dwarfdump",
            &[
                "--lookup".as_ref(),
                lookup_address.as_ref(),
                dwarf.as_os_str(),
            ],
            true,
        )?;
        // dSYM paths and UUIDs differ; retain the resolved function and source location.
        let resolved: Vec<_> = lookup
            .lines()
            .filter(|line| line.contains("Line info:"))
            .map(str::trim)
            .collect();
        if resolved.is_empty() {
            continue;
        }
        let symbolicated = tool(
            "atos",
            &[
                "-o".as_ref(),
                dwarf.as_os_str(),
                "-arch".as_ref(),
                arch.as_ref(),
                lookup_address.as_ref(),
            ],
        )?;
        if symbolicated.trim().is_empty() || symbolicated.trim().starts_with("0x") {
            return Err(format!("dSYM did not resolve function at {lookup_address}"));
        }
        // Mach-O adds an underscore to the Rust linkage name. Compare semantic
        // names, not the compiler's per-profile crate disambiguator.
        let function = demangle_symbol(name.strip_prefix('_').unwrap_or(name))?;
        let symbolicated = canonical_symbolication(symbolicated.trim(), demangle_symbol)?;
        symbols.push(aterm_json::Value::Object(aterm_json::Map::from([
            (
                "address".to_string(),
                aterm_json::Value::String(address.to_string()),
            ),
            ("function".to_string(), aterm_json::Value::String(function)),
            (
                "resolved".to_string(),
                aterm_json::Value::Array(
                    resolved
                        .into_iter()
                        .map(|s| aterm_json::Value::String(s.to_string()))
                        .collect(),
                ),
            ),
            (
                "dsym_symbolication".to_string(),
                aterm_json::Value::String(symbolicated),
            ),
        ])));
        if symbols.len() == 16 {
            break;
        }
    }
    if symbols.len() != 16 {
        return Err("fewer than 16 symbolicated text addresses".into());
    }
    Ok(aterm_json::Value::Object(aterm_json::Map::from([
        ("sections".to_string(), sections),
        ("symbols".to_string(), aterm_json::Value::Array(symbols)),
        (
            "dsym_dwarf_bytes".to_string(),
            aterm_json::Value::Number(dsym_bytes.into()),
        ),
        (
            "dsym_dwarf_sha256".to_string(),
            aterm_json::Value::String(dsym_sha256),
        ),
    ])))
}

// Read file-backed section bytes, excluding UUIDs and link-edit metadata. Bounds
// checks reject a truncated Mach-O before any indexing can panic.
fn sections(bytes: &[u8]) -> Result<aterm_json::Value, String> {
    let u32_at = |at: usize| -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            bytes
                .get(at..at + 4)
                .ok_or("truncated Mach-O")?
                .try_into()
                .map_err(|_| "Mach-O word")?,
        ))
    };
    let u64_at = |at: usize| -> Result<u64, String> {
        Ok(u64::from_le_bytes(
            bytes
                .get(at..at + 8)
                .ok_or("truncated Mach-O")?
                .try_into()
                .map_err(|_| "Mach-O word")?,
        ))
    };
    if u32_at(0)? != 0xfeed_facf {
        return Err("expected thin little-endian Mach-O 64".into());
    }
    let mut at = 32usize;
    let mut rows = Vec::new();
    for _ in 0..u32_at(16)? {
        let size = u32_at(at + 4)? as usize;
        let end = at
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or("Mach-O load command bounds")?;
        if size < 8 {
            return Err("short Mach-O load command".into());
        }
        if u32_at(at)? == 0x19 {
            if size < 72 {
                return Err("short Mach-O segment".into());
            }
            let segment = bytes.get(at + 8..at + 24).ok_or("segment name")?;
            if segment.starts_with(b"__TEXT\0") || segment.starts_with(b"__DATA") {
                for n in 0..u32_at(at + 64)? as usize {
                    let section = at + 72 + n * 80;
                    if section + 80 > end {
                        return Err("Mach-O section bounds".into());
                    }
                    let length =
                        usize::try_from(u64_at(section + 40)?).map_err(|_| "section size")?;
                    let offset = u32_at(section + 48)? as usize;
                    let kind = u32_at(section + 64)? & 0xff;
                    let digest = if [1, 0xc, 0x12].contains(&kind) {
                        None
                    } else {
                        let data = bytes
                            .get(offset..offset.checked_add(length).ok_or("section overflow")?)
                            .ok_or("section data bounds")?;
                        Some(aterm_codec::hex::encode_lower(
                            &aterm_digest::Sha256::digest(data),
                        ))
                    };
                    rows.push(aterm_json::Value::Object(aterm_json::Map::from([
                        (
                            "name".to_string(),
                            aterm_json::Value::String(
                                String::from_utf8_lossy(&bytes[section..section + 16])
                                    .trim_end_matches('\0')
                                    .to_string(),
                            ),
                        ),
                        (
                            "segment".to_string(),
                            aterm_json::Value::String(
                                String::from_utf8_lossy(segment)
                                    .trim_end_matches('\0')
                                    .to_string(),
                            ),
                        ),
                        (
                            "size".to_string(),
                            aterm_json::Value::Number((length as u64).into()),
                        ),
                        (
                            "sha256".to_string(),
                            digest.map_or(aterm_json::Value::Null, aterm_json::Value::String),
                        ),
                    ])));
                }
            }
        }
        at = end;
    }
    if rows.is_empty() {
        return Err("no code/data sections".into());
    }
    Ok(aterm_json::Value::Array(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiler_pass_observations_join_fragmented_lines_and_ignore_other_diagnostics() {
        let mut pending = Vec::new();
        let mut first = &b"warning: time: {}\ntime: {\"pass\":\"LLVM_fat"[..];
        assert!(
            compiler_passes(&mut first, &mut pending)
                .unwrap()
                .is_empty()
        );
        let mut second = &b"lto\",\"time\":7.5}\ntime: not json\n"[..];
        assert_eq!(
            compiler_passes(&mut second, &mut pending).unwrap(),
            vec![aterm_json::json!({"pass":"LLVM_fatlto", "time":7.5})]
        );
        assert!(pending.is_empty());
        assert!(
            compiler_passes(&mut &b""[..], &mut pending)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn anatomy_comparisons_use_two_interleaved_takes_and_single_arms_are_diagnostic() {
        let opts = parse(["--anatomy"].into_iter().map(str::to_owned)).unwrap();
        assert_eq!(order(&opts), vec![(0, Arm::Fat1)]);
        assert!(opts.anatomy);
        assert!(opts.diagnostic_anatomy());
        let candidate = parse(
            ["--anatomy", "--arm", "fat16"]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(order(&candidate), vec![(0, Arm::Fat16)]);
        assert!(candidate.diagnostic_anatomy());
        let comparison = parse(
            ["--anatomy", "--arm", "fat1", "--arm", "fat16"]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap();
        assert!(!comparison.diagnostic_anatomy());
        assert_eq!(
            order(&comparison),
            vec![
                (0, Arm::Fat1),
                (0, Arm::Fat16),
                (1, Arm::Fat16),
                (1, Arm::Fat1)
            ]
        );
        for flags in [
            vec!["--takes", "1", "--arm", "fat1"],
            vec!["--anatomy", "--arm", "fat16", "--arm", "fat16"],
            vec!["--anatomy", "--inspect", "take"],
            vec!["--anatomy", "--takes", "1"],
            vec!["--anatomy", "--takes", "2"],
        ] {
            assert!(parse(flags.into_iter().map(str::to_owned)).is_err());
        }
    }

    #[test]
    fn cargo_timings_survive_retirement_of_the_cold_graph() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let target = dir.path().join("build.noindex");
        let timings = target.join("cargo-timings");
        std::fs::create_dir_all(&timings).unwrap();
        std::fs::write(timings.join("cargo-timing.html"), b"unit timeline").unwrap();
        retain_timings(&target, dir.path()).unwrap();
        std::fs::remove_dir_all(&target).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("cargo-timings/cargo-timing.html")).unwrap(),
            b"unit timeline"
        );
        // A missing report is an observation failure, rather than a successful empty report.
        assert!(retain_timings(&target, dir.path()).is_err());
    }

    #[test]
    fn build_samples_exclude_supervisors_and_distinguish_reused_pids() {
        let member = aterm_sysprobe::MemberUsage {
            pid: 42,
            start: 1,
            cpu_ns: 123,
            footprint: 456,
            ..Default::default()
        };
        let initial = [(42, 1)].into_iter().collect();
        let reused = aterm_sysprobe::MemberUsage { start: 2, ..member };
        let sample = resource_sample(3.0, &initial, Some(vec![member, reused]), vec![]);
        assert_eq!(sample["members"].as_array().unwrap().len(), 1);
        assert_eq!(sample["members"][0]["start"].as_u64(), Some(2));
        assert_eq!(sample["members"][0]["cpu_ns"].as_u64(), Some(123));
        assert_eq!(sample["members"][0]["footprint_bytes"].as_u64(), Some(456));
        // A refused read must remain distinguishable from an empty build group.
        assert!(resource_sample(0.0, &initial, None, vec![])["members"].is_null());
        assert_eq!(
            resource_sample(0.0, &initial, Some(vec![]), vec![])["members"],
            aterm_json::json!([])
        );
    }

    #[test]
    fn build_sampler_retains_observations_and_returns_the_work_result() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (result, _) = with_load(dir.path(), || 17).unwrap();
        assert_eq!(result, 17);
        let samples = std::fs::read_to_string(dir.path().join("build-resources.jsonl")).unwrap();
        assert!(!samples.is_empty());
        for line in samples.lines() {
            let row: aterm_json::Value = aterm_json::from_str(line).unwrap();
            assert!(row["elapsed_s"].as_f64().unwrap() >= 0.0);
            assert!(row.as_object().unwrap().contains_key("members"));
        }
    }

    #[test]
    fn symbol_identity_preserves_function_offsets_and_source_locations() {
        let demangle = |_: &str| Ok("module::function".to_owned());
        let a = canonical_symbolication("_Rfirst + 4 (in aterm) (source.rs:7)", demangle).unwrap();
        let b = canonical_symbolication("_Rsecond + 4 (in aterm) (source.rs:7)", demangle).unwrap();
        assert_eq!(a, b);
        assert_ne!(
            a,
            canonical_symbolication("_Rsecond + 4 (in aterm) (source.rs:8)", demangle).unwrap()
        );
        assert_ne!(
            a,
            canonical_symbolication("_Rsecond + 8 (in aterm) (source.rs:7)", demangle).unwrap()
        );
        assert_ne!(
            a,
            canonical_symbolication("_Rsecond + 4 (in aterm) (source.rs:7)", |_| Ok(
                "module::other".to_owned()
            ))
            .unwrap()
        );
        assert!(canonical_symbolication("0x1000", demangle).is_err());
        assert!(canonical_symbolication("", demangle).is_err());
    }
    #[test]
    fn retained_inspection_cannot_write_outside_the_pilot_cache() {
        let cache = aterm_tempfile::tempdir().unwrap();
        let outside = aterm_tempfile::tempdir().unwrap();
        let take = cache.path().join("take");
        std::fs::create_dir(&take).unwrap();
        assert_eq!(
            retained_root(cache.path(), &take).unwrap(),
            std::fs::canonicalize(&take).unwrap()
        );
        assert!(retained_root(cache.path(), outside.path()).is_err());
        assert!(retained_root(cache.path(), cache.path()).is_err());
        assert!(retained_root(cache.path(), &take.join("../..")).is_err());
        #[cfg(unix)]
        {
            let link = take.join("escape");
            std::os::unix::fs::symlink(outside.path(), &link).unwrap();
            assert!(retained_root(cache.path(), &link).is_err());
        }
        assert!(
            parse(
                ["--inspect", "take", "--arm", "fat1"]
                    .into_iter()
                    .map(str::to_owned)
            )
            .is_err()
        );
    }
    #[test]
    fn lookup_misses_are_skipped_but_tool_failures_are_not() {
        assert!(tool_answer("dwarfdump", Some(1), b"image header".to_vec(), b"", true).is_ok());
        assert!(tool_answer("dwarfdump", Some(1), vec![], b"malformed DWARF", true).is_err());
        assert!(tool_answer("dwarfdump", None, vec![], b"", true).is_err());
        assert!(tool_answer("dsymutil", Some(1), vec![], b"", false).is_err());
    }
    #[test]
    fn retained_artifact_messages_determine_the_link_interval() {
        use std::fs::{File, FileTimes};
        use std::time::{Duration, UNIX_EPOCH};
        let dir = aterm_tempfile::tempdir().unwrap();
        let library = dir.path().join("relocated.rlib");
        let binary = dir.path().join("image");
        for (path, seconds) in [(&library, 100), (&binary, 107)] {
            File::create(path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
                .unwrap();
        }
        let log = dir.path().join("build.log");
        std::fs::write(&log, "compiler diagnostics, not artifacts\n").unwrap();
        let row = aterm_json::json!({"reason":"compiler-artifact", "target":{"name":"aterm_gui"}, "filenames":[library]}).to_string();
        std::fs::write(log.with_extension("messages.jsonl"), &row).unwrap();
        assert_eq!(gui_link_interval(&log, &binary).unwrap(), Some(7.0));
        std::fs::write(
            log.with_extension("messages.jsonl"),
            row.replace("compiler-artifact", "compiler-message"),
        )
        .unwrap();
        assert_eq!(gui_link_interval(&log, &binary).unwrap(), None);
    }
    #[test]
    fn artifact_messages_find_relocated_rlibs_and_reject_diagnostics() {
        let path = std::env::current_dir()
            .unwrap()
            .join("fresh/release/build/gui/unit/out/libaterm_gui.rlib");
        let row = aterm_json::json!({"reason":"compiler-artifact","target":{"name":"aterm_gui"},"filenames":[path]}).to_string();
        assert_eq!(gui_rlib(&row).unwrap(), path);
        assert!(gui_rlib(&row.replace("compiler-artifact", "compiler-message")).is_none());
        assert!(gui_rlib(&row.replace("aterm_gui", "other")).is_none());
    }
    #[test]
    fn section_identity_ignores_metadata_but_detects_changed_machine_code() {
        let mut image = vec![0u8; 188];
        image[0..4].copy_from_slice(&0xfeed_facfu32.to_le_bytes());
        image[16..20].copy_from_slice(&1u32.to_le_bytes());
        image[32..36].copy_from_slice(&0x19u32.to_le_bytes());
        image[36..40].copy_from_slice(&152u32.to_le_bytes());
        image[40..46].copy_from_slice(b"__TEXT");
        image[96..100].copy_from_slice(&1u32.to_le_bytes());
        image[104..110].copy_from_slice(b"__text");
        image[144..152].copy_from_slice(&4u64.to_le_bytes());
        image[152..156].copy_from_slice(&184u32.to_le_bytes());
        image[184..188].copy_from_slice(&[1, 2, 3, 4]);
        let before = sections(&image).unwrap();
        image[8] = 3;
        assert_eq!(sections(&image).unwrap(), before);
        image[187] = 9;
        assert_ne!(sections(&image).unwrap(), before);
        image.truncate(187);
        assert!(sections(&image).is_err());
    }
    #[test]
    fn arms_and_interleaving_are_exact() {
        let opts = parse(
            ["--arm", "fat1", "--arm", "fat1-lines"]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(
            order(&opts),
            vec![
                (0, Arm::Fat1),
                (0, Arm::Fat1Lines),
                (1, Arm::Fat1Lines),
                (1, Arm::Fat1)
            ]
        );
        assert_ne!(Arm::Fat1.environment(), Arm::Fat1Lines.environment());
        assert_eq!(Arm::Fat1Lines.environment()[2].1, "line-tables-only");
        assert!(parse(["--arm", "unknown"].into_iter().map(str::to_owned)).is_err());
    }
    #[test]
    fn thin_comparison_keeps_the_shipped_debug_information() {
        let opts = parse(
            ["--arm", "fat1", "--arm", "thin16"]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(
            order(&opts),
            vec![
                (0, Arm::Fat1),
                (0, Arm::Thin16),
                (1, Arm::Thin16),
                (1, Arm::Fat1)
            ]
        );
        assert_eq!(
            Arm::Thin16.environment(),
            [
                ("CARGO_PROFILE_RELEASE_LTO", "thin"),
                ("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "16"),
                ("CARGO_PROFILE_RELEASE_DEBUG", "1"),
            ]
        );
        assert_eq!(Arm::Thin16.environment()[2], Arm::Fat1.environment()[2]);
        assert_ne!(
            Arm::Thin16.environment()[2],
            Arm::Thin16Lines.environment()[2]
        );
    }
    #[test]
    fn thin_debug_twin_rejects_changed_code() {
        let row = |arm: &str, code: &str| {
            aterm_json::json!({
                "arm": arm, "slice": "native", "sha256": arm,
                "inspection": {"sections": code, "symbols": "same"}
            })
        };
        let base = row("thin16", "same");
        let lines = row("thin16-lines", "same");
        let mut rows = vec![base.clone(), base, lines.clone(), lines];
        assert!(validate(&rows).is_ok());
        let changed = row("thin16-lines", "changed");
        rows[2] = changed.clone();
        rows[3] = changed;
        assert!(validate(&rows).is_err());
    }
    #[test]
    fn identity_rejects_mutated_code_and_nonreproducible_binary() {
        let make = |arm: &str, hash: &str, sections: &str| aterm_json::json!({"arm":arm,"slice":"native","sha256":hash,"inspection":{"sections":sections,"symbols":"same"}});
        let row = make("fat1", "a", "same");
        let mut rows = vec![row.clone(), row.clone()];
        assert!(validate(&rows).is_ok());
        rows[1] = aterm_json::json!({"arm":"fat1","slice":"native","sha256":"a","runtime_app_sha256":"different stamped bytes","inspection":{"sections":"same","symbols":"same"}});
        assert!(validate(&rows).is_err());
        rows[1] = make("fat1", "mutated", "same");
        assert!(validate(&rows).is_err());
        rows[1] = row;
        let lines = make("fat1-lines", "b", "same");
        rows.extend([lines.clone(), lines]);
        assert!(validate(&rows).is_ok());
        rows[2] = make("fat1-lines", "b", "changed");
        assert!(validate(&rows).is_err());
        rows[2] = aterm_json::json!({"arm":"fat1-lines","slice":"native","sha256":"b","inspection":{"sections":"same","symbols":"changed"}});
        assert!(validate(&rows).is_err());
        assert!(sections(&[0; 32]).is_err());
    }
}
