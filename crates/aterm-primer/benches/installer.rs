// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Five fresh-home/warm installer samples, not cold process or disk-cache samples.
//! Counts run by default; elapsed time is opt-in
//! after external quiet admission: `targo --unverified bench --profile test
//! -p aterm-primer --bench installer -- --elapsed`.

#[cfg(all(unix, debug_assertions))]
mod support;

#[cfg(all(unix, debug_assertions))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use aterm_durable::{Flush, seam::Recording};
    use std::time::{Instant, SystemTime};
    use support::{Drift, Tree};

    let mut mode = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--bench" => {}
            "--counts-only" | "--elapsed" => {
                let elapsed = arg == "--elapsed";
                if mode.is_some_and(|old| old != elapsed) {
                    return Err("--counts-only and --elapsed contradict each other".into());
                }
                mode = Some(elapsed);
            }
            "--help" | "-h" => {
                println!(
                    "installer [--counts-only | --elapsed]: five cold/warm scratch-home samples; --elapsed requires external quiet admission"
                );
                return Ok(());
            }
            _ => return Err(format!("unknown installer argument: {arg}").into()),
        }
    }
    let elapsed = mode.unwrap_or(false);
    let admission = if elapsed {
        "caller_not_checked_by_driver"
    } else {
        "not_applicable_counts_only"
    };
    let sandbox = aterm_tempfile::Builder::new()
        .prefix("primer-installer-")
        .tempdir()?;
    let root = sandbox.path();
    for (key, relative) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("TMPDIR", "tmp"),
        ("CODEX_HOME", "overrides/codex"),
        ("CLAUDE_CONFIG_DIR", "overrides/claude"),
    ] {
        let path = root.join(relative);
        std::fs::create_dir_all(&path)?;
        // SAFETY: this standalone benchmark is still single-threaded. No worker
        // or installer has run; subsequent library calls inherit only scratch roots.
        unsafe { std::env::set_var(key, path) };
    }
    let overrides = root.join("overrides");
    std::fs::write(
        overrides.join("codex/AGENTS.md"),
        b"Shell-only Codex fixture.\n",
    )?;
    std::fs::write(
        overrides.join("claude/CLAUDE.md"),
        b"Shell-only Claude fixture.\n",
    )?;
    let protected = [root.join("home"), root.join("config"), overrides]
        .into_iter()
        .map(|path| Tree::read(&path).map(|tree| (path, tree)))
        .collect::<Result<Vec<_>, _>>()?;
    println!(
        "installer samples=5 passes_per_phase=1 cold=fresh_scratch_home_not_process_or_disk_cache debug_assertions=true elapsed={elapsed} admission={admission} durable_flushes_are_plain={} allocated=reachable_fixture_entry_blocks_excluding_retained_replaced_inodes",
        aterm_durable::durable_flushes_are_plain()
    );
    for sample in 0..5 {
        let home = root.join(format!("sample-{sample}"));
        support::seed(&home)?;
        let mut before = Tree::read(&home)?;
        for phase in ["cold", "warm"] {
            if phase == "warm" {
                before.age_files()?;
                before = Tree::read(&home)?;
            }
            let recording = Recording::start();
            let started = elapsed.then(Instant::now);
            let pass = aterm_primer::auto_prime(&home);
            let nanos = started.map(|started| started.elapsed().as_nanos());
            let flushes = recording.take();
            let plain = recording.take_plain();
            drop(recording);
            assert_eq!(pass.errors().count(), 0, "{}", pass.summary);
            let after = Tree::read(&home)?;
            let drift = before.drift(&after);
            let durable = flushes
                .iter()
                .filter(|flush| **flush == Flush::Durable)
                .count();
            let ordered = flushes
                .iter()
                .filter(|flush| **flush == Flush::Ordered)
                .count();
            println!(
                "sample={sample} phase={phase} elapsed_ns={nanos:?} durable_flushes={durable} ordered_flushes={ordered} plain_flushes={plain} files={} directories={} logical_bytes={} reachable_allocated_bytes={} changed={} drift={drift:?}",
                after.files.len(),
                after.directory_count(),
                after.logical_bytes(),
                after.allocated_bytes(),
                pass.changed()
            );
            for (path, file) in &after.files {
                let modified = file
                    .modified
                    .duration_since(SystemTime::UNIX_EPOCH)?
                    .as_nanos();
                println!(
                    "sample={sample} phase={phase} file={path:?} bytes={} identity={:?} modified_unix_ns={modified}",
                    file.bytes.len(),
                    file.identity
                );
            }
            if phase == "warm" {
                assert!(!pass.changed());
                assert_eq!(drift, Drift::default());
                assert!(flushes.is_empty());
                assert_eq!(plain, 0);
            } else {
                assert!(pass.changed());
                assert!(!flushes.is_empty());
            }
            before = after;
        }
    }
    for (path, before) in protected {
        assert_eq!(
            before.drift(&Tree::read(&path)?),
            Drift::default(),
            "{path:?}"
        );
    }
    Ok(())
}

#[cfg(not(all(unix, debug_assertions)))]
fn main() {
    eprintln!("installer requires Unix and --profile test (debug assertions)");
    std::process::exit(2);
}
