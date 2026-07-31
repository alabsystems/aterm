// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `atpkg doctor` health surface (§15) — a no-network, no-mutation diagnostic over
//! atpkg's own store/ops/sig/status primitives.
//!
//! Structural breakage (a broken bin shim, an active build whose store tree vanished, a
//! fish-breaking stray `.sh`, a world-writable login-sourced dir) is a PROBLEM → nonzero
//! exit. Everything advisory (bin not yet on PATH, a frozen-looking index, a foreign
//! sysroot wiring) stays a WARNING → exit 0. It reads no unverified index/manifest
//! (verify-before-parse): its freshness surface reads atpkg's OWN `status.toml` +
//! the durable [`crate::sig::Floor`].

use std::ffi::OsStr;
use std::path::Path;

use crate::store::Layout;

const GIB: u64 = 1 << 30;

/// Run the health surface, printing the report. Returns `true` iff there were NO structural
/// problems (`main` maps `false` → exit 1). Reads the real environment (home + PATH + clock
/// + the `[packages]` config account + the token chain's SOURCE label — never the token).
#[must_use]
pub fn run(layout: &Layout) -> bool {
    let home = aterm_types::dirs::home_dir();
    let path = std::env::var_os("PATH");
    let cfg_account = crate::config::cached().account().map(str::to_string);
    // Which source supplies a GitHub token (§5.1 private-repo aid): `$ATPKG_TOKEN`,
    // else aterm-update-core's chain. Only the LABEL is surfaced.
    let (_token, token_source) = crate::cli::resolve_pkg_token(layout);
    run_with(
        layout,
        home.as_deref(),
        path.as_deref(),
        crate::flow::now_unix(),
        cfg_account.as_deref(),
        token_source.as_deref(),
    )
}

/// The testable core: `home`, the `PATH` value, `now`, the `[packages].account`
/// config override, and the resolved token-source LABEL are injected so the surface
/// can be exercised against a synthetic environment without mutating the process env
/// (or spawning the keychain/`gh` probes).
#[must_use]
pub fn run_with(
    layout: &Layout,
    home: Option<&Path>,
    path_var: Option<&OsStr>,
    now: i64,
    cfg_account: Option<&str>,
    token_source: Option<&str>,
) -> bool {
    let mut fails = 0usize;

    // (1) TRUST ROOT + INDEX SOURCE + TOKEN SOURCE.
    println!(
        "doctor: index source github.com/{}",
        crate::resolve_account(cfg_account).slug()
    );
    if crate::manager_enabled() {
        if std::env::var("ATPKG_ROOTKEY_OVERRIDE").is_ok_and(|key| !key.is_empty()) {
            println!("doctor: ok — root key via ATPKG_ROOTKEY_OVERRIDE");
        } else {
            println!(
                "doctor: ok — root key pinned (fingerprint {})",
                crate::root_key_fingerprint()
            );
        }
    } else {
        println!("doctor: warn — disabled/inert (no root key available or ATPKG_DISABLE set)");
    }
    // Loud token provenance (never the token itself): which source of the
    // `$ATPKG_TOKEN` → aterm-update-core chain (env → keychain → 0600 file →
    // `$GITHUB_TOKEN`/`$GH_TOKEN` → `gh auth token`) supplied a credential.
    match token_source {
        Some(src) => println!(
            "doctor: ok — GitHub token from {src} (used for index/pkg fetches; never printed)"
        ),
        None => println!(
            "doctor: ok — no GitHub token provisioned (anonymous API: fine for public \
             repos, rate-limited; private fetch overrides need one)"
        ),
    }
    // Bundled seed (§9.1): whether this executable ships a sealed offline
    // registry. Presence is an offer, not trust — its bytes still pass the
    // identical signed gates, so absence is informational, never a failure.
    match crate::bundled_seed_dir() {
        Some(dir) => println!("doctor: ok — bundled seed at {}", dir.display()),
        None => println!("doctor: ok — no bundled seed (network registry only)"),
    }

    // (2) PREFIX / STORE.
    if layout.prefix.is_dir() {
        println!("doctor: ok — prefix {}", layout.prefix.display());
    } else {
        println!(
            "doctor: warn — prefix {} does not exist yet (nothing installed)",
            layout.prefix.display()
        );
    }

    // (3) PATH WIRING.
    let bin_dir = layout.bin_dir();
    let on_path = path_var
        .map(|p| std::env::split_paths(p).any(|d| d == bin_dir))
        .unwrap_or(false);
    if on_path {
        println!("doctor: ok — managed bin/ is on PATH");
    } else {
        println!(
            "doctor: warn — {} is not on PATH; an aterm shell auto-sources ~/.aterm/shell.d \
             (which APPENDS it), or add: {}",
            bin_dir.display(),
            manual_path_hint(&bin_dir)
        );
    }

    // (4) BROKEN SHIM SCAN of bin/ — a shim whose forward target is GONE (a dangling
    // symlink on Unix; on Windows a `.cmd` forwarding to a missing exe, which no symlink
    // scan could ever catch). `resolve_shim` reads the target cross-platform; a tombstone
    // (deliberately target-less) yields `None` and is never flagged.
    if let Ok(entries) = std::fs::read_dir(&bin_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if let Some(target) = crate::platform::resolve_shim(&p)
                && !target.exists()
            {
                fails += 1;
                eprintln!("doctor: FAIL — broken bin shim {}", p.display());
            }
        }
    }

    // (5) ACTIVE-BUILD STORE INTEGRITY.
    let active = crate::ops::active_builds(layout);
    for (program, build) in &active {
        let bd = layout.build_dir(program, *build);
        if !bd.is_dir() || !crate::store::build_is_complete(&bd) {
            fails += 1;
            eprintln!("doctor: FAIL — active {program} build {build} store missing/incomplete");
        }
    }
    println!("doctor: ok — {} program(s) active", active.len());

    // (5b) LIVE-BUILD WITNESS. `gc` reclaims a program's superseded builds only when the
    // authoritative `store/<program>/current` symlink and the derived `bin/` shim view name
    // the SAME build; where they don't it abstains rather than guess (guessing is what
    // deleted live trees). Abstention is silent by nature — the program simply accumulates
    // builds forever — so this is the surface that makes it visible. A genuine disagreement
    // is STRUCTURAL: whichever view is stale, some tool on PATH is running a build activation
    // does not select. A merely-absent witness is not breakage, so it warns.
    let live = crate::gc::live_builds(layout);
    for d in live.diverged() {
        match &d.reason {
            crate::gc::Diverged::ChannelShimMismatch {
                channel_says,
                shims_say,
            } => {
                fails += 1;
                eprintln!(
                    "doctor: FAIL — {}: the channel selects build {channel_says} but its bin/ \
                     shims run build {shims_say} (re-run `atpkg update {}`)",
                    d.program, d.program
                );
            }
            crate::gc::Diverged::ShimsDisagree { builds } => {
                fails += 1;
                eprintln!(
                    "doctor: FAIL — {}: its bin/ shims are split across builds {} — one \
                     program's tools must all point into one build (re-run `atpkg update {}`)",
                    d.program,
                    build_list(builds),
                    d.program
                );
            }
            crate::gc::Diverged::ChannelsDisagree { builds } => {
                fails += 1;
                eprintln!(
                    "doctor: FAIL — {}: two channel `current` links select different builds \
                     {} and it has no `store/{}/current` of its own to break the tie — run \
                     `atpkg update {}` to write one",
                    d.program,
                    build_list(builds),
                    d.program,
                    d.program
                );
            }
            crate::gc::Diverged::NoLiveWitness { shims_say } => {
                println!(
                    "doctor: warn — {}: build {shims_say} is on PATH but no `current` link \
                     selects it, so gc keeps every superseded {} build. Run \
                     `atpkg update {}` to re-activate it and clear this.",
                    d.program, d.program, d.program
                );
            }
        }
    }
    println!(
        "doctor: ok — {} program(s) with a proven live build",
        live.len()
    );

    // (6) SHELL HOOKS + FISH-SAFETY.
    if let Some(home) = home {
        let aterm = home.join(".aterm");
        let shell_d = aterm.join("shell.d");
        // Probe the dialect the interactive shell on THIS platform actually sources: `.ps1`
        // (PowerShell) on Windows, `.zsh` elsewhere. An install writes the whole set, so a
        // present platform-native hook means PATH wiring is in place.
        let native_hook = format!("{}.{}", crate::hooks::HOOK_BASENAME, native_hook_ext());
        if shell_d.join(&native_hook).is_file() {
            println!("doctor: ok — shell.d hooks present");
        } else {
            println!("doctor: warn — shell.d hooks not generated yet (an install writes them)");
        }
        if let Ok(entries) = std::fs::read_dir(&shell_d) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().ends_with(".sh") {
                    fails += 1;
                    eprintln!(
                        "doctor: FAIL — shell.d/{}: a POSIX .sh breaks fish — remove it",
                        e.file_name().to_string_lossy()
                    );
                }
            }
        }
        // Privacy of the login-sourced dirs (READ-ONLY — doctor never chmods).
        for dir in [&aterm, &shell_d] {
            if let Ok(m) = std::fs::symlink_metadata(dir)
                && m.file_type().is_dir()
                && !crate::platform::dir_meta_is_private(&m)
            {
                fails += 1;
                eprintln!(
                    "doctor: FAIL — {} is group/other-writable (login shells source it)",
                    dir.display()
                );
            }
        }
    }

    // (7) DISK HEADROOM.
    match crate::freespace::available_bytes(&layout.prefix) {
        Some(free) if free < 5 * GIB => println!(
            "doctor: warn — only {} free (a toolchain update needs ~2.5x its artifact size)",
            crate::cost::human_bytes(free)
        ),
        Some(free) => println!("doctor: ok — {} free", crate::cost::human_bytes(free)),
        None => println!("doctor: warn — could not query free space"),
    }

    // (8) INDEX FREEZE / AGE (no unverified parse — atpkg's OWN diagnostics only).
    if let Some(status) = crate::status::read(layout) {
        match index_age_days(&status.updated_at, now) {
            Some(days) if days > 30 => println!(
                "doctor: warn — {days} day(s) since the last successful update ({}) — publishing \
                 looks frozen or this machine has been offline",
                status.updated_at
            ),
            Some(days) => println!("doctor: ok — {days} day(s) since the last successful update"),
            None => println!("doctor: warn — could not parse the last-update time"),
        }
    } else {
        println!("doctor: warn — no status.toml yet (no update has run)");
    }
    println!(
        "doctor: last-trusted index_build {}",
        crate::sig::Floor::new(layout.floor()).current()
    );

    // (9) RUSTUP + RELOCATABILITY.
    if !rustup_present() {
        println!("doctor: warn — rustup not found (self-contained bundles are portable)");
    }

    if fails == 0 {
        println!("doctor: healthy");
        true
    } else {
        println!("doctor: found {fails} problem(s)");
        false
    }
}

/// Render a divergence's contested build numbers for the report line.
fn build_list(builds: &[u64]) -> String {
    builds
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whole days since `updated_at` (RFC3339), or `None` if it cannot be parsed.
fn index_age_days(updated_at: &str, now: i64) -> Option<i64> {
    let then = crate::flow::rfc3339_to_unix(updated_at)?;
    Some((now - then) / 86_400)
}

/// Whether `rustup` is on PATH and answers `--version`.
fn rustup_present() -> bool {
    std::process::Command::new("rustup")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Copy-pasteable manual PATH-append for the shell of THIS platform: PowerShell on Windows
/// (';' separator, `$env:PATH`), POSIX `export` elsewhere. Only a fallback — an aterm shell
/// auto-sources the `shell.d` hook that does this already.
#[cfg(windows)]
fn manual_path_hint(bin: &Path) -> String {
    format!(
        "$env:PATH += \";{}\"  (PowerShell; or add it to your User PATH via System Settings)",
        bin.display()
    )
}
#[cfg(not(windows))]
fn manual_path_hint(bin: &Path) -> String {
    format!("export PATH=\"$PATH:{}\"", bin.display())
}

/// The `shell.d` hook extension the interactive shell on this platform actually sources.
#[cfg(windows)]
fn native_hook_ext() -> &'static str {
    "ps1"
}
#[cfg(not(windows))]
fn native_hook_ext() -> &'static str {
    "zsh"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activate::{activate_channel, install_shims};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-doctor-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    fn tool(name: &str) -> crate::store::ToolName {
        crate::store::ToolName::new(name).unwrap()
    }

    fn synthetic_home(label: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("atpkg-dhome-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&h, std::fs::Permissions::from_mode(0o700)).unwrap();
        h
    }

    /// The build tree alone — no shims, no channel. Split out so a test can construct the
    /// half-wired states the witness checks are about.
    fn install_build_tree(layout: &Layout, program: &str, build: u64) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        // The concrete executable the shim will forward to (`<program>.exe` on Windows) —
        // it must EXIST for the broken-shim scan (check 4) to see a healthy layout.
        std::fs::write(
            dir.join("bin").join(tool(program).exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        dir
    }

    fn install(layout: &Layout, program: &str, build: u64) {
        let dir = install_build_tree(layout, program, build);
        install_shims(layout, &dir, &[program.to_string()]).unwrap();
        activate_channel(layout, "stable", &dir).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
    }

    #[test]
    fn index_age_math() {
        // 0 days.
        let then = crate::flow::rfc3339_to_unix("2026-07-01T00:00:00Z").unwrap();
        assert_eq!(index_age_days("2026-07-01T00:00:00Z", then), Some(0));
        // 31 days > 30.
        assert_eq!(
            index_age_days("2026-07-01T00:00:00Z", then + 31 * 86_400),
            Some(31)
        );
        // Garbage → None.
        assert_eq!(index_age_days("not-a-date", then), None);
    }

    #[test]
    fn healthy_layout_returns_true() {
        let l = layout("healthy");
        install(&l, "ay", 18);
        let home = synthetic_home("healthy");
        // PATH contains the managed bin/ so even the advisory check is clean.
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        assert!(
            run_with(&l, Some(&home), Some(&path), 0, None, None),
            "a clean install is healthy"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn broken_bin_symlink_fails() {
        let l = layout("broken");
        install(&l, "ay", 18);
        // Add a shim pointing at a nonexistent target (a dangling symlink on Unix, a `.cmd`
        // forwarding to a missing exe on Windows) via the same primitive a real install uses.
        let ghost = tool("ghost");
        crate::platform::install_shim(&l.build_dir("ay", 99).join("bin"), &ghost, &l.shim(&ghost))
            .unwrap();
        let home = synthetic_home("broken");
        assert!(
            !run_with(&l, Some(&home), None, 0, None, None),
            "a dangling bin symlink is structural"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn active_build_with_missing_store_fails() {
        let l = layout("missing-store");
        install(&l, "ay", 18);
        // The shim resolves, but the completeness marker is gone (check 5 vs check 4).
        crate::store::discard_build(&l.build_dir("ay", 18));
        // Re-create just the bin so the shim isn't dangling (isolate check 5 from check 4).
        std::fs::create_dir_all(l.build_dir("ay", 18).join("bin")).unwrap();
        std::fs::write(
            l.build_dir("ay", 18)
                .join("bin")
                .join(tool("ay").exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        assert!(!crate::store::build_is_complete(&l.build_dir("ay", 18)));
        let home = synthetic_home("missing-store");
        assert!(
            !run_with(&l, Some(&home), None, 0, None, None),
            "an incomplete active build is structural"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn stray_posix_sh_in_shell_d_fails() {
        let l = layout("stray-sh");
        install(&l, "ay", 18);
        let home = synthetic_home("stray-sh");
        let shell_d = home.join(".aterm/shell.d");
        std::fs::create_dir_all(&shell_d).unwrap();
        std::fs::write(shell_d.join("00-atpkg.sh"), b"echo stray\n").unwrap();
        assert!(
            !run_with(&l, Some(&home), None, 0, None, None),
            "a fish-breaking stray .sh is structural"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A shim and the channel naming different builds is STRUCTURAL: something on PATH is
    /// running a build the channel does not select, and `gc` has stopped reclaiming that
    /// program entirely. Doctor is the only place that says so.
    #[test]
    fn a_shim_disagreeing_with_the_channel_is_structural() {
        let l = layout("witness-mismatch");
        install(&l, "ay", 19); // channel + shim both at 19
        // Stage 18 COMPLETE on disk (so check 5 stays quiet) and re-point ONLY the shim.
        let older = install_build_tree(&l, "ay", 18);
        crate::store::mark_build_ready(&older).unwrap();
        let ay = tool("ay");
        crate::platform::install_shim(&older.join("bin"), &ay, &l.shim(&ay)).unwrap();
        let home = synthetic_home("witness-mismatch");
        assert!(
            !run_with(&l, Some(&home), None, 0, None, None),
            "channel says 19, shims say 18 — structural"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A program with no channel witness is not breakage — nothing is broken, gc just
    /// abstains — so it warns and stays exit-0. It must still be SAID: the whole cost of
    /// abstaining is that it is otherwise invisible.
    #[test]
    fn a_program_with_no_channel_witness_warns_but_exit_zero() {
        let l = layout("witness-absent");
        install_build_tree(&l, "ay", 18);
        let dir = l.build_dir("ay", 18);
        install_shims(&l, &dir, &["ay".to_string()]).unwrap(); // shimmed, never activated
        crate::store::mark_build_ready(&dir).unwrap();
        let home = synthetic_home("witness-absent");
        assert!(
            run_with(&l, Some(&home), None, 0, None, None),
            "an un-witnessed program is advisory, not structural"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn bin_not_on_path_warns_but_exit_zero() {
        let l = layout("notonpath");
        install(&l, "ay", 18);
        let home = synthetic_home("notonpath");
        // PATH without the managed bin/ → a warning, not a structural fail.
        let path = std::ffi::OsString::from("/usr/bin:/bin");
        assert!(
            run_with(&l, Some(&home), Some(&path), 0, None, None),
            "a PATH warning stays exit-0"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }
}
