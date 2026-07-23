// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `atpkg doctor` health surface (§15) — a no-network, no-mutation diagnostic over
//! atpkg's own store/ops/sig/status primitives.
//!
//! Structural breakage (a broken bin shim, an active build whose store tree vanished, a
//! fish-breaking stray `.sh`, a world-writable login-sourced dir) is a PROBLEM → nonzero
//! exit. Everything advisory (bin not yet on PATH, a frozen-looking index, a foreign
//! `~/.kani` link) stays a WARNING → exit 0. It reads no unverified index/manifest
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
        now_unix(),
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
    if crate::enabled() {
        println!(
            "doctor: ok — root key pinned (fingerprint {})",
            crate::root_key_fingerprint()
        );
    } else {
        println!("doctor: warn — disabled/inert (no root key pinned at build time)");
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

    // (9) ~/.kani + RUSTUP + RELOCATABILITY.
    if let Some(home) = home {
        let kani = home.join(".kani");
        if kani.is_dir() {
            if let Ok(m) = std::fs::symlink_metadata(&kani)
                && m.file_type().is_dir()
                && !crate::platform::dir_meta_is_private(&m)
            {
                println!("doctor: warn — ~/.kani is group/other-writable");
            }
            let store = layout.prefix.join("store");
            if let Ok(entries) = std::fs::read_dir(&kani) {
                for e in entries.flatten() {
                    let name = e.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if !name.starts_with("kani-") {
                        continue;
                    }
                    if let Ok(target) = std::fs::read_link(e.path())
                        && !target.starts_with(&store)
                    {
                        println!(
                            "doctor: warn — foreign ~/.kani entry {name} (atpkg did not create it)"
                        );
                    }
                }
            }
            if !rustup_present() {
                println!(
                    "doctor: warn — rustup not found; a rustup-linked toolchain bundle needs it \
                     (self-contained bundles are portable)"
                );
            }
        }
    }

    if fails == 0 {
        println!("doctor: healthy");
        true
    } else {
        println!("doctor: found {fails} problem(s)");
        false
    }
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

/// Current Unix epoch second (for the age math); 0 if the clock is before the epoch.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
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

    fn synthetic_home(label: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("atpkg-dhome-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&h, std::fs::Permissions::from_mode(0o700)).unwrap();
        h
    }

    fn install(layout: &Layout, program: &str, build: u64) {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        // The concrete executable the shim will forward to (`<program>.exe` on Windows) —
        // it must EXIST for the broken-shim scan (check 4) to see a healthy layout.
        let exe = format!("{program}{}", crate::platform::EXE_SUFFIX);
        std::fs::write(dir.join("bin").join(exe), b"#!/bin/true\n").unwrap();
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
        crate::platform::install_shim(
            &l.build_dir("ay", 99).join("bin"),
            "ghost",
            &l.shim("ghost"),
        )
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
                .join(format!("ay{}", crate::platform::EXE_SUFFIX)),
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
