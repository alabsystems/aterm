// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local ABBA of frozen verifier executables: warm test units, cold lint, ordinary admission.
//! Run from the clean, fixed checkout both drivers must judge:
//! `targo --unverified run -p aterm-verify --example tree_half -- A B OUTPUT CORES`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_uds::spawnfd::{Stdio, command};

fn head(root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let status = command("git")
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .current_dir(root)
        .output()?;
    if !status.status.success() || !status.stdout.is_empty() {
        return Err("the measured checkout must stay clean".into());
    }
    let head = command("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()?;
    if !head.status.success() {
        return Err("cannot name the measured HEAD".into());
    }
    Ok(String::from_utf8(head.stdout)?.trim().to_owned())
}

fn prime_source_checks(
    driver: &Path,
    root: &Path,
    log: &Path,
    pool: &aterm_capacity::Jobserver,
    granted: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let log = std::fs::File::create(log)?;
    let mut cmd = command(driver);
    cmd.args(["release", "--tree", "--checks", "fmt,census,forge"])
        .current_dir(root)
        .env_remove("BUILD_SLOT_HELD")
        .env_remove(aterm_capacity::IMPLICIT_POOL_ENV)
        .env("CARGO_BUILD_JOBS", granted.to_string())
        .env_remove("MAKEFLAGS")
        .env_remove("CARGO_MAKEFLAGS")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    pool.hand_to(&mut cmd);
    let status = cmd.status()?;
    if !status.success() {
        return Err(format!("source-check preparation: {status}; no timed take ran").into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("usage: tree_half BASELINE CANDIDATE OUTPUT CORES".into());
    }
    let root = std::env::current_dir()?;
    let frozen = head(&root)?;
    let before = std::fs::canonicalize(&args[0])?;
    let after = std::fs::canonicalize(&args[1])?;
    let output = PathBuf::from(&args[2]);
    let cores = args[3].to_str().ok_or("non-UTF8 cores")?.parse::<u32>()?;
    if cores == 0 {
        return Err("cores must be positive".into());
    }
    std::fs::create_dir_all(&output)?;
    // No Release rank, release line, publication, fleet operation or quiet-machine wait.
    let claim = aterm_capacity::admit(
        aterm_capacity::Need::new("local tree-half ABBA").cores(cores),
        aterm_capacity::Admit::Wait,
    )?;
    let granted = claim.cores().max(1);
    let mut rows = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output.join("takes.jsonl"))?;
    // Prime the main target on the same sources. The lint lane starts cold in every take,
    // so the preflight ordering cost is present in both arms. No verdict is carried.
    for (name, args) in [
        (
            "prepare-tests",
            vec!["test", "--all", "--as-release", "--compile-only"],
        ),
        (
            "prepare-other",
            vec!["release", "--tree", "--checks", "doc,libc,spec"],
        ),
    ] {
        let pool = aterm_capacity::Jobserver::new(&output.join(format!("{name}.jobs")), granted)?;
        let log = std::fs::File::create(output.join(format!("{name}.stdout")))?;
        let mut cmd = command(&before);
        cmd.args(args)
            .current_dir(&root)
            .env_remove("BUILD_SLOT_HELD")
            .env_remove(aterm_capacity::IMPLICIT_POOL_ENV)
            .env("CARGO_BUILD_JOBS", granted.to_string())
            .env_remove("MAKEFLAGS")
            .env_remove("CARGO_MAKEFLAGS")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        pool.hand_to(&mut cmd);
        eprintln!("{name} starts");
        let status = cmd.status()?;
        if !status.success() {
            return Err(format!("{name}: {status}; no timed take ran").into());
        }
    }
    let history = aterm_types::dirs::account_state_dir()
        .map(|root| {
            aterm_verify::test_history::load(&root.join(aterm_capacity::book::TEST_HISTORY))
        })
        .unwrap_or_default()
        .encode();
    std::fs::write(output.join("history-seed.json"), &history)?;
    let context = aterm_verify::Ctx::new(
        root.clone(),
        aterm_verify::Scope::workspace(),
        aterm_verify::EnvSnapshot::capture(),
        output.join("scratch"),
    );
    let lint_dir = aterm_verify::plan::lane_dir(&context, aterm_verify::plan::Lane::TippyTarget)
        .ok_or("no lint lane")?;
    for (take, (arm, driver)) in [("A", &before), ("B", &after), ("B", &after), ("A", &before)]
        .into_iter()
        .enumerate()
    {
        if head(&root)? != frozen {
            return Err("HEAD changed between takes".into());
        }
        let name = format!("{}-{arm}", take + 1);
        // Retire only this dedicated checkout's regenerable lint lane. The cache's own guard
        // checks Cargo locks, live users and custody; a refusal ends the experiment.
        if lint_dir.exists() && !atpkg::build_cache::remove_unbuilt(&lint_dir)? {
            return Err(format!(
                "{} is held; no matched cold take can run",
                lint_dir.display()
            )
            .into());
        }
        // Each take learns into its own copy; none can change a later take's widths or order.
        let history_path = output.join(format!("{name}.history.json"));
        std::fs::write(&history_path, &history)?;
        let pool = aterm_capacity::Jobserver::new(&output.join(format!("{name}.jobs")), granted)?;
        // Each runner's census memo binds its own code fingerprint. Warm it with the matching
        // runner before every take; no source-check verdict is supplied to the timed child.
        prime_source_checks(
            driver,
            &root,
            &output.join(format!("{name}.prime.stdout")),
            &pool,
            granted,
        )?;
        if head(&root)? != frozen {
            return Err("HEAD changed during source-check preparation".into());
        }
        let from = aterm_capacity::Reading::now(&root);
        eprintln!("{name} starts: {}", from.brief());
        let log = std::fs::File::create(output.join(format!("{name}.stdout")))?;
        let mut cmd = command("/usr/bin/time");
        cmd.args(["-l", "-o"])
            .arg(output.join(format!("{name}.time")))
            .arg(driver)
            .args(["release", "--tree", "--events"])
            .arg(output.join(format!("{name}.events")))
            .arg("--log")
            .arg(output.join(format!("{name}.log")))
            .arg("--test-history")
            .arg(&history_path)
            .current_dir(&root)
            .env_remove("BUILD_SLOT_HELD")
            .env_remove(aterm_capacity::IMPLICIT_POOL_ENV)
            .env("CARGO_BUILD_JOBS", granted.to_string())
            .env_remove("MAKEFLAGS")
            .env_remove("CARGO_MAKEFLAGS")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        pool.hand_to(&mut cmd);
        let start = Instant::now();
        let status = cmd.status()?;
        let wall = start.elapsed().as_secs_f64();
        let to = aterm_capacity::Reading::now(&root);
        let row = aterm_json::json!({
            "take": take + 1, "arm": arm, "head": frozen, "driver": driver,
            "granted_cores": granted, "wall_s": wall, "exit": status.code(),
            "cold_lint_dir": lint_dir, "build_slot_held": false,
            "load1_before": from.load1, "load1_after": to.load1,
            "load5_before": from.load5, "load5_after": to.load5,
            "available_before": from.available, "available_after": to.available,
            "physical": from.physical, "swap_before": from.swap_used,
            "swap_after": to.swap_used, "machine": aterm_capacity::between(&from, &to)
        });
        writeln!(rows, "{row}")?;
        rows.flush()?;
        eprintln!("{name} ends: {wall:.3}s, {status}; {}", to.brief());
        if head(&root)? != frozen {
            return Err("HEAD changed during a take".into());
        }
        if !status.success() {
            return Err(format!("{name}: {status}; the ABBA is incomplete").into());
        }
    }
    Ok(())
}
