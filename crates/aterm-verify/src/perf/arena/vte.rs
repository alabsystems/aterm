// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `vte` arena benchmark lane comparing `aterm` against pinned `ghostty` on `vtebench`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_uds::spawnfd::command;

use super::cat::{Term, inner_argv, order, run_in};
use super::{
    COOLDOWN, Halt, Inbox, Ledger, arena_dir, build_aterm, ghostty_tip, macos_version, pins,
    preflight, run_ok, sha256_file, spawn_halt,
};
use crate::perf::stats::median;
use crate::perf::trend::utc_date;
use crate::perf::{LaneRun, arena::Args};

/// The stock benchmarks' tty lookup, and the trimmed one.
const STOCK_TTY: &str = "tty=\"/dev/$(ps -o tty= -p $$)\"";
const TRIMMED_TTY: &str = "tty=\"/dev/$(ps -o tty= -p $$ | tr -d ' ')\"";

/// Trim the tty lookup in one benchmark script.
pub(crate) fn trim_tty(text: &str) -> Result<(String, bool), String> {
    let changed = text.contains(STOCK_TTY);
    let out = text.replace(STOCK_TTY, TRIMMED_TTY);
    if out.replace(TRIMMED_TTY, "").contains("ps -o tty=") {
        return Err("looks its tty up another way; re-read the pin".into());
    }
    Ok((out, changed))
}

/// Copy the pinned benchmarks and trim their tty lookup.
fn trimmed_benchmarks(stock: &Path, copy: &Path) -> Result<usize, String> {
    let _ = std::fs::remove_dir_all(copy);
    run_ok(command("cp").arg("-R").arg(stock).arg(copy), "cp")?;
    let mut trimmed = 0;
    for bench in benchmark_dirs(copy)? {
        for name in ["benchmark", "setup"] {
            let script = bench.join(name);
            let Ok(text) = std::fs::read_to_string(&script) else {
                continue;
            };
            let (out, changed) =
                trim_tty(&text).map_err(|e| format!("{}: {e}", script.display()))?;
            if changed {
                std::fs::write(&script, out).map_err(|e| format!("{}: {e}", script.display()))?;
                trimmed += 1;
            }
        }
    }
    if trimmed == 0 {
        return Err("no benchmark carries the stock tty lookup any more; re-read the pin".into());
    }
    Ok(trimmed)
}

fn benchmark_dirs(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    Ok(v)
}

/// One vtebench `--dat` file.
pub(crate) fn parse_dat(text: &str) -> BTreeMap<String, Vec<f64>> {
    let mut lines = text.lines();
    let names: Vec<&str> = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    let mut cols: BTreeMap<String, Vec<f64>> = names
        .iter()
        .map(|n| ((*n).to_string(), Vec::new()))
        .collect();
    for line in lines {
        for (name, v) in names.iter().zip(line.split_whitespace()) {
            if let Ok(x) = v.parse::<f64>() {
                cols.entry((*name).to_string()).or_default().push(x);
            }
        }
    }
    cols
}

/// Every counted trial's samples pooled per `(benchmark, terminal)` pair.
pub(crate) fn rows(trials: &[(Term, BTreeMap<String, Vec<f64>>)]) -> Vec<String> {
    let mut pooled: BTreeMap<(String, &str), Vec<f64>> = BTreeMap::new();
    let mut per_trial: BTreeMap<(String, &str), Vec<f64>> = BTreeMap::new();
    for (term, dat) in trials {
        for (name, samples) in dat {
            if samples.is_empty() {
                continue;
            }
            pooled
                .entry((name.clone(), term.name()))
                .or_default()
                .extend(samples);
            per_trial
                .entry((name.clone(), term.name()))
                .or_default()
                .push(median(samples));
        }
    }
    let names: std::collections::BTreeSet<&String> = pooled.keys().map(|(n, _)| n).collect();
    names
        .into_iter()
        .map(|name| {
            let cell = |t: &str| {
                pooled
                    .get(&(name.clone(), t))
                    .map_or_else(|| "no samples".into(), |s| format!("{} ms (n={})", median(s), s.len()))
            };
            let raw = |t: &str| {
                per_trial
                    .get(&(name.clone(), t))
                    .map(|v| v.iter().map(|m| format!("{m}")).collect::<Vec<_>>().join(","))
                    .unwrap_or_default()
            };
            format!(
                "| vtebench {name} | aterm {} | ghostty-tip {} | per-trial medians: aterm {} / ghostty {} |",
                cell("aterm"),
                cell("ghostty"),
                raw("aterm"),
                raw("ghostty")
            )
        })
        .collect()
}

/// The `vte` lane.
pub(crate) fn run(args: &Args) -> LaneRun {
    super::outcome(run_inner(args))
}

/// vtebench at the pin, cloned into the arena cache once.
fn clone_pin(repo: &str, dir: &Path) -> Result<(), Halt> {
    if dir.is_dir() {
        return Ok(());
    }
    let out = command("git")
        .args(["clone", "-q", repo])
        .arg(dir)
        .output()
        .map_err(|e| spawn_halt("git", &e, "install git (xcode-select --install)"))?;
    if out.status.success() {
        Ok(())
    } else {
        let _ = std::fs::remove_dir_all(dir);
        Err(Halt::skip(
            format!(
                "vtebench could not be cloned from {repo} ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            "check the network",
        ))
    }
}

fn run_inner(args: &Args) -> Result<String, Halt> {
    let trials = args.trials.unwrap_or(3);
    let (cols, grid_rows) = (args.columns.unwrap_or(120), args.rows.unwrap_or(40));
    let max_secs = args.max_secs.unwrap_or(10);
    let (machine, ledgers) = preflight(args.dev_smoke)?;
    let pins = pins()?;
    let ghostty = ghostty_tip(&pins, args.dmg.as_deref())?;
    let aterm = build_aterm()?;

    let arena = arena_dir()?;
    let pin = &pins.harness.vtebench;
    let vte_dir = arena.join(format!("vtebench-{}", pin.commit));
    clone_pin(&pin.repo, &vte_dir)?;
    run_ok(
        command("git")
            .arg("-C")
            .arg(&vte_dir)
            .args(["checkout", "-q", "--detach", &pin.commit]),
        "git checkout",
    )?;
    let dirty = super::stdout_of(
        command("git")
            .arg("-C")
            .arg(&vte_dir)
            .args(["status", "--porcelain"]),
    )
    .unwrap_or_default();
    if !dirty.is_empty() {
        return Err(Halt::Fail(format!(
            "{} has local changes — the pin is the stock harness",
            vte_dir.display()
        )));
    }
    let driver = crate::perf::driver::cargo_driver()?;
    let build_args = ["--release", "--locked", "--target-dir", "target"];
    let built = driver
        .command("build")
        .args(build_args)
        .current_dir(&vte_dir)
        .status()
        .map_err(|e| format!("could not build vtebench: {e}"))?;
    if !built.success() {
        return Err(Halt::Fail(format!("the vtebench build exited {built}")));
    }
    let vtebench = vte_dir.join("target/release/vtebench");
    let vte_sha = sha256_file(&vtebench)?;
    let bench_dir = arena.join(format!("vtebench-{}-benchmarks", pin.commit));
    let trimmed = trimmed_benchmarks(&vte_dir.join("benchmarks"), &bench_dir)?;
    let n_bench = benchmark_dirs(&bench_dir)?.len();
    let ceiling = Duration::from_secs(n_bench as u64 * (2 * u64::from(max_secs) + 30));
    eprintln!(
        "vtebench {}: {n_bench} benchmarks ({trimmed} tty lookups trimmed), grid {cols}x{grid_rows}, \
         --max-secs {max_secs}",
        pin.commit
    );
    let run_dir = arena.join(format!("vte-{}-{}", utc_date(), std::process::id()));
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("{}: {e}", run_dir.display()))?;
    let aterm_grid = vec![
        "--columns".to_string(),
        cols.to_string(),
        "--lines".to_string(),
        grid_rows.to_string(),
    ];
    let ghostty_grid = vec![
        format!("--window-width={cols}"),
        format!("--window-height={grid_rows}"),
    ];
    let max_secs_s = max_secs.to_string();
    let mut counted: Vec<(Term, BTreeMap<String, Vec<f64>>)> = Vec::new();
    for i in 1..=trials {
        for term in order(i) {
            let prefix = run_dir.join(format!("{}-{i}", term.name()));
            let inbox = Inbox::bind()?;
            let inner = inner_argv(
                "vte",
                &[
                    &vtebench,
                    &bench_dir,
                    &prefix,
                    Path::new(&max_secs_s),
                    inbox.path(),
                ],
            )?;
            let outcome = run_in(
                term,
                &aterm,
                &ghostty,
                (&aterm_grid, &ghostty_grid),
                &inner,
                &inbox,
                ceiling,
            )
            .and_then(|r| judge_trial(&r, &prefix, grid_rows, cols, n_bench));
            match outcome {
                Ok(dat) => {
                    eprintln!(
                        "  trial {i} {}: {}",
                        term.name(),
                        prefix.with_extension("dat").display()
                    );
                    counted.push((term, dat));
                }
                Err(e) => eprintln!("  trial {i} {}: REFUSED — {e}", term.name()),
            }
            std::thread::sleep(COOLDOWN);
        }
    }
    if counted.is_empty() {
        return Err("no trial produced a counted .dat".into());
    }
    let table = rows(&counted);
    eprintln!("== ARENA-VTE (median ms per sample; lower is better) ==");
    for r in &table {
        eprintln!("{r}");
    }
    let date = utc_date();
    let entry = ledgers.write(
        "vte",
        &Ledger {
            date: &date,
            dim: &format!("ARENA-VTE (vtebench default suite, {cols}x{grid_rows})"),
            machine: &machine.label(),
            macos: &macos_version(),
            aterm: &aterm
                .provenance("targo --unverified build --release -p aterm, shipping default"),
            competitors: &format!(
                "ghostty tip {} (official dmg, sha256={})",
                ghostty.commit, ghostty.sha256
            ),
        },
        &format!(
            "{}\n\nHarness: vtebench {}, built `targo --unverified build --release --locked`, \
             sha256={vte_sha}; its benchmarks stock but for the tty lookup, trimmed in {trimmed} \
             scripts for macOS `ps` padding. `--max-secs {max_secs}`, {trials} interleaved trials. \
             Both terminals ran at a {cols}x{grid_rows} grid (aterm `--columns/--lines`, ghostty \
             `--window-width/--window-height`) — a config deviation from shipping defaults.\n\
             Raw .dat files: {}\n",
            table.join("\n"),
            pin.commit,
            run_dir.display()
        ),
    )?;
    Ok(format!("ledger entry {}", entry.display()))
}

/// Count a trial only at the asked grid and with every benchmark.
fn judge_trial(
    report: &str,
    prefix: &Path,
    rows: u32,
    cols: u32,
    n_bench: usize,
) -> Result<BTreeMap<String, Vec<f64>>, String> {
    let grid = report
        .strip_prefix("vte ")
        .ok_or_else(|| format!("the in-terminal half reported: {report:?}"))?;
    let dat_path = prefix.with_extension("dat");
    let refuse = |why: String| {
        let _ = std::fs::rename(&dat_path, prefix.with_extension("refused"));
        Err(why)
    };
    if grid != format!("{rows} {cols}") {
        return refuse(format!(
            "the benchmarks got a '{grid}' grid (rows cols), not '{rows} {cols}'"
        ));
    }
    let text =
        std::fs::read_to_string(&dat_path).map_err(|e| format!("{}: {e}", dat_path.display()))?;
    let dat = parse_dat(&text);
    if dat.len() != n_bench {
        return refuse(format!("{} of {n_bench} benchmarks ran", dat.len()));
    }
    Ok(dat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tty_lookup_is_trimmed_and_nothing_else_is_touched() {
        let script = format!("#!/bin/sh\n{STOCK_TTY}\ncols=$(tput cols < $tty)\n");
        let (out, changed) = trim_tty(&script).expect("trims");
        assert!(changed);
        assert_eq!(
            out,
            format!("#!/bin/sh\n{TRIMMED_TTY}\ncols=$(tput cols < $tty)\n")
        );
        assert_eq!(
            trim_tty("echo hi\n").expect("untouched"),
            ("echo hi\n".into(), false)
        );
        assert!(
            trim_tty("t=$(ps -o tty= -p 1)\n").is_err(),
            "another lookup is refused"
        );
    }

    #[test]
    fn a_dat_parses_with_gaps_and_pools_per_terminal() {
        let dat = parse_dat("a b\n1 2\n3 _\n5 6\n");
        assert_eq!(dat["a"], vec![1.0, 3.0, 5.0]);
        assert_eq!(dat["b"], vec![2.0, 6.0]);
        let table = rows(&[
            (Term::Aterm, dat.clone()),
            (Term::Ghostty, parse_dat("a\n10\n")),
        ]);
        assert_eq!(table.len(), 2);
        assert!(
            table[0].starts_with("| vtebench a | aterm 3 ms (n=3) | ghostty-tip 10 ms (n=1)"),
            "{}",
            table[0]
        );
        assert!(table[1].contains("ghostty-tip no samples"), "{}", table[1]);
    }

    #[test]
    fn a_trial_at_another_grid_or_short_a_benchmark_is_refused_and_kept() {
        let dir = super::super::Scratch::new("atv-vte").expect("scratch");
        let prefix = dir.path().join("aterm-1");
        std::fs::write(prefix.with_extension("dat"), "a b\n1 2\n").expect("write");
        assert!(judge_trial("vte 40 120", &prefix, 40, 120, 2).is_ok());
        let e = judge_trial("vte 40 120", &prefix, 40, 120, 3).expect_err("short");
        assert!(e.contains("2 of 3"), "{e}");
        assert!(prefix.with_extension("refused").is_file());
        std::fs::write(prefix.with_extension("dat"), "a b\n1 2\n").expect("write");
        assert!(judge_trial("vte 24 80", &prefix, 40, 120, 2).is_err());
        assert!(judge_trial("fail vtebench exited 1", &prefix, 40, 120, 2).is_err());
    }
}
