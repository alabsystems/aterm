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

/// Whether a recorded program `state` describes a FAULT this machine should act on.
///
/// The five fault prefixes are the ones the install/update paths actually write. Two were
/// missing until 2026-08-20 round-11, and both absences were the mirror image of the
/// stray-row bug fixed the same day: that one made doctor report a fault where none
/// existed, these made it MISS ones that did.
///
/// * `aborted: <phase>` (cli.rs, the `TxnOutcome::Aborted` arm) — a coherence-group
///   transaction killed mid-flight, precisely the state in which a tuple's members may
///   disagree with each other. Recorded against the FAILED member only, and on a group
///   disk-shortfall that member is an arbitrary one of the tuple, so the row names where
///   the transaction stopped rather than what is individually wrong.
/// * `tombstoned: pin yanked/below floor` — the worse of the two, because it is silent
///   everywhere else. A yanked pin replaces the program's working shims with failing
///   stubs that print "was yanked/revoked" ([`crate::activate::install_tombstone_shim`]);
///   the broken-shim scan skips tombstones BY DESIGN, and a tombstoned program drops out
///   of `active_builds` entirely. So doctor saw no broken shim, no active build, and no
///   recognized fault, and pronounced "healthy" on a machine whose compiler had become a
///   stub. Both clear themselves: a later successful update rewrites the row and replaces
///   the tombstone shim with a real symlink.
///
/// Deliberately NOT a fault: `active`, `dev-linked (skipped)` (a §13 hard-skip the user
/// asked for), and any future informational state. The list is allow-by-prefix so an
/// unrecognized state reads as benign rather than as a failure — a diagnostic that invents
/// faults from states it does not understand trains people to ignore it.
fn is_problem_state(state: &str) -> bool {
    state.starts_with("error:")
        || state.starts_with("unavailable:")
        || state.starts_with("blocked:")
        || state.starts_with("aborted:")
        || state.starts_with("tombstoned:")
}

/// Where the per-problem listing should START — or `None` when no verdict owns it.
///
/// The listing must hang off the branch that actually printed a verdict, which is why this
/// is a function rather than an expression at the loop. Deriving it from `store_empty`
/// alone was wrong in both directions on a DECLINED store: with an empty store it skipped
/// problem #1 that no branch had named (silently losing the only finding, since
/// `*toolset*: unavailable: …` is the normal single row on a Mac the index does not serve),
/// and with a populated store it printed every problem as an orphan line under "the ALab
/// toolset was removed on this machine" — lines describing a verdict nobody gave.
///
/// A declined store lists nothing: the decline IS the verdict, and a stale fault row
/// describes a toolset the user deliberately removed. Whether such a row should also flip
/// the exit code is a separate product question, deliberately not answered here.
fn problem_listing_start(declined: bool, store_empty: bool, problems: usize) -> Option<usize> {
    if declined || problems == 0 {
        None
    } else if store_empty {
        // That verdict names its first reason inline; listing resumes after it.
        Some(1)
    } else {
        Some(0)
    }
}

/// EVERY recorded fault, formatted `"<program>: <state>"`, in program order — `BTreeMap`
/// order, i.e. alphabetical by program name.
///
/// Pulled out of [`run_with`] so it can be tested for what it actually promises. This used
/// to be a `.find()` inline, which returned at most ONE finding: a second failing program
/// stayed invisible until the first was repaired, and since `status.toml` is a `BTreeMap`,
/// which one you were shown was alphabetical accident. A diagnostic that reveals its
/// findings one per repair cycle is a guessing game.
///
/// Stray rows (keys beginning with `-`) are excluded — see the caller: they cannot be
/// programs at all, and counting them condemned healthy toolsets.
fn recorded_problems(status: Option<&crate::Status>) -> Vec<String> {
    status
        .map(|s| {
            s.programs
                .iter()
                .filter(|(name, _)| !name.starts_with('-'))
                .filter(|(_, p)| is_problem_state(&p.state))
                .map(|(name, p)| format!("{name}: {}", p.state))
                .collect()
        })
        .unwrap_or_default()
}

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

    // (0) WHICH atpkg IS SPEAKING. Every line below is only as good as the binary printing
    // it, and that binary is NOT self-evident: `atpkg` ships inside the aterm app, so a
    // machine can easily have several — an installed /Applications copy, a dev `dist/`
    // build, a `target/release` one — and `which atpkg` picks whichever PATH happens to
    // reach first.
    //
    // This is not hypothetical. Measured 2026-08-20: a stale in-bundle atpkg answered
    // "trust is not installed" for a store that a current atpkg verified as
    // "build 6459 OK (matches signed tree_root)". A wrong answer from a manager about its
    // own store is the most expensive kind of wrong, because the natural next step is to
    // reinstall something that was never broken. Naming the speaker makes that verifiable
    // in one line instead of an afternoon.
    println!(
        "doctor: this atpkg is {} at {}",
        env!("CARGO_PKG_VERSION"),
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown path>".to_string())
    );
    report_aterm_posture(layout);

    // (1) TRUST ROOT + INDEX SOURCE + TOKEN SOURCE.
    println!(
        "doctor: index source github.com/{}",
        crate::resolve_account(cfg_account).slug()
    );
    if crate::manager_enabled() {
        // The root is the PAPER MASTER — the same anchor the app updater uses, not a
        // package-specific key. Naming it here is what lets an operator answer "which
        // trust root is this build on?" without reading source.
        println!(
            "doctor: ok — paper master pinned (fingerprint {}, {} key(s))",
            crate::root_key_fingerprint(),
            crate::PKG_TRUST_ANCHORS.len()
        );
    } else {
        println!(
            "doctor: warn — disabled/inert (no paper master compiled in \
             (pins::PAPER_MASTER_PUBKEYS is empty), or ATPKG_DISABLE set) — this build \
             installs nothing"
        );
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

    // (5c) SOLVERS THE `trust` BUNDLE PINS PRIVATELY.
    //
    // trustc resolves its SMT solver as a SIBLING of its own executable — see
    // `sibling_solver_candidate` / `resolve_ay_solver_identity_from_candidates`
    // in trust's `compiler/rustc_mir_transform/src/trust_verify.rs`: an explicit
    // `AY_PATH` wins, otherwise the copy inside the bundle's own `bin/`, and
    // there is NO PATH fallback at all.
    //
    // The pin itself is deliberate and should not be "fixed" away: trust
    // snapshots and fingerprints the solver binary so a proof records which
    // solver produced it, and a solver that floated with whatever atpkg last
    // installed would make proof identity float with it.
    //
    // What was wrong is that NOTHING said so. atpkg dutifully kept `ay` current
    // while every Trust build went on using a copy three minor versions behind,
    // and the only way to find out was to compare two `--version` strings
    // nobody had a reason to compare. Measured 2026-08-20 on a clean v0.44.0
    // install: bundle `ay 0.10.0`, `ty 0.12.0`, `clean 0.1.0` against managed
    // `0.13.0`, `0.13.0`, `0.2.0`.
    //
    // So: reported, never failed. A pin by design is not a problem; a pin
    // nobody can see is.
    if let Some(trust_build) = active.iter().find(|(p, _)| p.as_str() == "trust").map(|(_, b)| *b) {
        let bundle_bin = layout.build_dir("trust", trust_build).join("bin");
        for (program, managed_build) in &active {
            if program.as_str() == "trust" {
                continue;
            }
            let pinned = bundle_bin.join(program);
            if !pinned.is_file() {
                continue;
            }
            let pinned_version = probe_version(&pinned);
            let managed_version =
                probe_version(&layout.build_dir(program, *managed_build).join("bin").join(program));
            // Equal versions are the healthy case. An unanswered probe on
            // either side is NOT evidence of divergence, so it stays silent
            // rather than reporting "pinned unknown vs managed 0.13.0" — a
            // line that reads like a finding and carries none.
            if pinned_version == managed_version
                || pinned_version == "unknown"
                || managed_version == "unknown"
            {
                continue;
            }
            let override_hint = if program.as_str() == "ay" { " (override: AY_PATH)" } else { "" };
            println!(
                "doctor: note — Trust builds use the {program} pinned inside the trust bundle \
                 ({pinned_version}), not the managed {program} {managed_version} \
                 (build {managed_build}){override_hint}"
            );
        }
    }

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
    // The build floor is printed WITH the generation that recorded it, because that pair
    // is the actual gate: a floor stamped with an older generation is re-based by the next
    // master-signed one rather than obeyed (`sig::BuildFloor`), so a reader who saw only
    // the number could not tell a binding floor from an inherited one.
    let build_floor = crate::sig::BuildFloor {
        index_build: crate::sig::Floor::new(layout.floor()).current(),
        roster_seq: crate::sig::Floor::new(layout.floor_generation()).current(),
    };
    println!(
        "doctor: last-trusted index_build {} (recorded under roster_seq {})",
        build_floor.index_build, build_floor.roster_seq
    );
    // The SECOND durable ratchet, shown beside the first because they answer different
    // questions and move independently: `index_build` is how far the toolchain index has
    // advanced, `roster_seq` is which generation of the machine roster this store has
    // accepted. A roster floor that is stuck while machines have been minted or revoked
    // means this store has not seen a publish since, which is worth being able to see.
    println!(
        "doctor: last-trusted roster_seq {}",
        crate::sig::Floor::new(layout.roster_floor()).current()
    );

    // (9) RUSTUP + RELOCATABILITY.
    if !rustup_present() {
        println!("doctor: warn — rustup not found (self-contained bundles are portable)");
    }

    // (10) THE QUESTION A USER ACTUALLY CAME HERE WITH: do I have the toolchain?
    //
    // Everything above audits the STRUCTURE of the store — shims, floors, PATH, disk.
    // All of it passes vacuously on a machine that received nothing at all, so
    // `doctor` cheerfully printed "healthy" to the one person most in need of an
    // answer: someone whose toolchain never arrived, looking at the command the docs
    // point them to. A diagnostic that reports health over an empty store closes off
    // the only self-service path to understanding.
    let installed = crate::ops::active_builds(layout);
    let status = crate::status::read(layout);
    // A recorded key beginning with `-` cannot ever be a program: no shim can be created
    // under one and the signed index cannot name one. Such a row is a STRAY — the residue
    // of a mistyped `atpkg install --help`, which used to resolve the flag as a name and
    // persist the failure forever. Counting it as a missing member let one typo report a
    // fully healthy ten-program toolset as incomplete, and — because `tools/install.sh`
    // hands `pkg doctor` to every failed-seed installer as THE diagnostic — took the exit
    // code down with it, outside this repo (2026-08-20 round-10 audit).
    let strays: Vec<&String> = status
        .as_ref()
        .map(|s| s.programs.keys().filter(|k| k.starts_with('-')).collect())
        .unwrap_or_default();
    for stray in &strays {
        println!(
            "doctor: warn — the record holds a stray row for {stray:?}, which cannot be a \
             program name (it is a command-line flag, left by a mistyped `atpkg install`). \
             No program is missing because of it; the next successful `aterm pkg update` \
             clears it"
        );
    }
    // EVERY problem, not the first one. This scan used to `.find()`, so a second failing
    // program was invisible until the first was fixed — a diagnostic that reveals its
    // findings one per repair cycle is not triage, it is a guessing game, and `status.toml`
    // is a `BTreeMap` so which one won was alphabetical accident.
    let recorded_problems = recorded_problems(status.as_ref());
    let declined = layout.declined().is_file();
    let mut toolset_problem = false;
    if declined {
        // Intended emptiness. Say so, so it does not read as a fault.
        println!(
            "doctor: the ALab toolset was removed on this machine (`aterm pkg install \
             --default-set` reinstalls it)"
        );
    } else if installed.is_empty() {
        toolset_problem = true;
        match recorded_problems.first() {
            Some(why) => println!("doctor: PROBLEM — no ALab programs are installed ({why})"),
            // No per-program row survives an ENVIRONMENTAL failure any more — an
            // unreachable index says nothing about any particular program — so the
            // aggregate sentence is now the only place the reason lives. Preferring it to
            // the generic hint is what keeps "why did nothing arrive?" answerable offline.
            None => match status.as_ref().map(|s| s.outcome.as_str()).filter(|o| !o.is_empty()) {
                Some(outcome) => println!(
                    "doctor: PROBLEM — no ALab programs are installed (last attempt: {outcome})"
                ),
                None => println!(
                    "doctor: PROBLEM — no ALab programs are installed. Run \
                     `aterm pkg install --default-set` to see why (it names the reason and \
                     exits 2 when no build is published for this Mac)"
                ),
            },
        }
    } else if !recorded_problems.is_empty() {
        // Something IS installed, but the record carries live failures — a partial first
        // run, a blocked disk, a member with no build for this triple, an aborted
        // coherence-group transaction.
        toolset_problem = true;
        // No COUNT here. The tail already prints "found N problem(s)" on its own arithmetic
        // (structural failures + the toolset condition as one), and a verdict line carrying
        // a different N would contradict it in the same report — the tail is the line a
        // human reads last and a script would grep. The problems are listed immediately
        // below, so the number is there to be read.
        println!(
            "doctor: PROBLEM — the toolset is incomplete; {} program(s) active",
            installed.len()
        );
    } else {
        println!("doctor: {} ALab program(s) active", installed.len());
    }
    if let Some(start) = problem_listing_start(declined, installed.is_empty(), recorded_problems.len())
    {
        for why in recorded_problems.iter().skip(start) {
            println!("doctor:   {why}");
        }
    }

    if fails == 0 && !toolset_problem {
        println!("doctor: healthy");
        true
    } else {
        let total = fails + usize::from(toolset_problem);
        println!("doctor: found {total} problem(s)");
        false
    }
}

/// The bare version token from `<bin> --version`, for comparing two copies of
/// the same program at a glance.
///
/// Takes the SECOND whitespace token (`ay 0.13.0+build.8174.…` -> `0.13.0…`)
/// and drops any `+build…`/commit/date suffix, because the question this
/// answers is "are these the same release", not "which exact commit".
///
/// Best-effort by construction: a binary that will not run, will not answer, or
/// answers in some other shape reports `unknown` and the caller stays quiet
/// about it. `doctor` must never fail because a probe failed — the probe is a
/// convenience, and the store integrity checks above are the real verdict.
fn probe_version(bin: &Path) -> String {
    const UNKNOWN: &str = "unknown";
    let Ok(out) = std::process::Command::new(bin).arg("--version").output() else {
        return UNKNOWN.to_string();
    };
    if !out.status.success() {
        return UNKNOWN.to_string();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(line) = text.lines().next() else {
        return UNKNOWN.to_string();
    };
    let Some(token) = line.split_whitespace().nth(1) else {
        return UNKNOWN.to_string();
    };
    token.split('+').next().unwrap_or(token).to_string()
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

/// Report the aterm app's own update posture beside the toolchain's.
///
/// THE DESIGN OVERSIGHT THIS PARTIALLY CLOSES. atpkg manages the ALab toolchain; the aterm
/// app manages itself through a SEPARATE updater; and `atpkg` is a binary inside that app.
/// So the component a user is most likely to be running a stale copy of is the one thing
/// atpkg had nothing to say about — you could ask it about ten programs and get no hint
/// that the eleventh, the one answering, was months old.
///
/// The app cannot simply BECOME an atpkg program: swapping a running, notarized `.app`
/// needs Gatekeeper assessment, a crash-loop boot sentinel, and a live-process handoff,
/// none of which the shim-and-store model provides. What atpkg can do — and now does — is
/// stop pretending the app is not there, by reading the updater's own records rather than
/// forming a second opinion about them.
///
/// Silent when there is no updater state: a bare CLI install is a legitimate posture, not a
/// fault.
fn report_aterm_posture(layout: &crate::store::Layout) {
    let Some(support) = layout.prefix.parent() else {
        return;
    };
    let updates = support.join("Updates");
    let field = |file: &str, key: &str| -> Option<String> {
        let text = std::fs::read_to_string(updates.join(file)).ok()?;
        text.lines()
            .find_map(|l| l.split_once('='). filter(|(k, _)| k.trim() == key))
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
    };
    let Some(installed) = field("installed.toml", "build_number") else {
        return;
    };
    match (field("status.toml", "current_build"), field("status.toml", "staged_build")) {
        (Some(current), Some(staged)) if current != staged => println!(
            "doctor: note — aterm is RUNNING build {current}, installed {installed}, with \
             build {staged} staged and waiting for a restart"
        ),
        (Some(current), _) if current != installed => println!(
            "doctor: note — aterm is RUNNING build {current} but build {installed} is \
             installed; the running process predates it"
        ),
        _ => println!("doctor: ok — aterm build {installed} installed"),
    }
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

    /// THE QUESTION THE COMMAND EXISTS FOR. Every structural check passes vacuously
    /// on a store that received nothing, so `doctor` used to print "healthy" to the
    /// one user most in need of an answer — someone whose toolchain never arrived,
    /// running the command the docs point them at. An empty store is not health.
    #[test]
    fn an_empty_store_is_not_healthy() {
        let l = layout("empty-store");
        let home = synthetic_home("empty-store");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        // Structurally spotless — and still not healthy, because there is no toolchain.
        assert!(
            !run_with(&l, Some(&home), Some(&path), 0, None, None),
            "a store with no ALab programs must report a problem, not health"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// ONE TYPO MUST NOT CONDEMN A HEALTHY TOOLSET.
    ///
    /// A key beginning with `-` cannot be a program: no shim exists under one and the
    /// signed index cannot name one. Before this, a single `atpkg install --help` minted
    /// `[programs.--help]` permanently and doctor read it back as a missing member — so a
    /// machine with ten verified programs reported "the toolset is incomplete" and exited
    /// 1, forever. That exit code is a published contract: `tools/install.sh` hands
    /// `pkg doctor` to every failed-seed installer as THE diagnostic to trust.
    ///
    /// The second case is the one that gives the first its teeth — it proves the scan was
    /// NARROWED to stray flags, not disabled.
    #[test]
    fn a_stray_flag_row_is_not_a_missing_program() {
        let stray_row = |name: &str| {
            let l = layout(&format!("stray-{}", name.trim_start_matches('-')));
            install(&l, "ay", 18);
            let existing = crate::status::read(&l).unwrap_or_default();
            let mut programs = existing.programs;
            programs.insert(
                "ay".to_string(),
                crate::ProgramStatus {
                    installed_build: Some(18),
                    state: "active".into(),
                    tree_root: String::new(),
                },
            );
            programs.insert(
                name.to_string(),
                crate::ProgramStatus {
                    installed_build: None,
                    state: format!("error: {name} is not named in the signed index"),
                    tree_root: String::new(),
                },
            );
            crate::status::write(&l, &crate::Status { programs, ..existing }).unwrap();
            l
        };

        let l = stray_row("--help");
        let home = synthetic_home("stray-help");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        assert!(
            run_with(&l, Some(&home), Some(&path), 0, None, None),
            "a mistyped flag left in the record is a stray row, not a missing program"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);

        // …and a plausible PROGRAM name in the same error state still fails, so the change
        // narrowed the scan rather than blunting it.
        let l = stray_row("trust-vc");
        let home = synthetic_home("stray-real");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        assert!(
            !run_with(&l, Some(&home), Some(&path), 0, None, None),
            "a real program name in an error state is still a problem doctor must report"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// THE SCAN MUST SEE WHAT IT IS FOR. Two independent blindnesses, fixed together
    /// because they are the same failure — a diagnostic that under-reports.
    ///
    /// `aborted: <phase>` is written for every member of a coherence group whose
    /// transaction was killed mid-flight — precisely the state in which a tuple's members
    /// may disagree with each other — and the scan matched only three prefixes, so doctor
    /// pronounced such a machine healthy. That is the mirror image of the stray-row bug:
    /// one invented a fault, this one missed a real one.
    #[test]
    fn an_aborted_transaction_is_a_problem_doctor_can_see() {
        assert!(
            is_problem_state("aborted: activate"),
            "a killed coherence-group transaction is a fault, not an informational state"
        );
        for fault in ["error: x", "unavailable: y", "blocked: z"] {
            assert!(is_problem_state(fault), "{fault} stays a fault");
        }
        // Allow-by-prefix: a state the scan does not recognize reads as benign. Inventing
        // faults from unknown states is how a diagnostic teaches people to ignore it.
        for benign in ["active", "dev-linked (skipped)", "staged 4821"] {
            assert!(!is_problem_state(benign), "{benign} is not a fault");
        }
    }

    /// A YANKED PIN IS NOT HEALTH — the quietest fault in the crate.
    ///
    /// A tombstoned program's shims are replaced by stubs that print "was yanked/revoked",
    /// the broken-shim scan skips tombstones by design, and the program drops out of
    /// `active_builds` — so every other check went quiet at once and doctor said "healthy"
    /// about a machine whose compiler had become a stub.
    #[test]
    fn a_yanked_pin_is_a_problem_not_silence() {
        assert!(
            is_problem_state("tombstoned: pin yanked/below floor"),
            "a yanked pin leaves failing stubs behind; that is not health"
        );
    }

    /// THE LISTING MUST HANG OFF THE BRANCH THAT SPOKE.
    ///
    /// Derived from `store_empty` alone, it was wrong in both directions on a DECLINED
    /// store: with an empty store it skipped a problem no branch had named — losing the
    /// only finding, since `*toolset*: unavailable: …` is the normal single row on a Mac
    /// the index does not serve — and with a populated store it printed every problem as
    /// an orphan line under "the ALab toolset was removed on this machine".
    ///
    /// Exhaustive over (declined) x (store empty) x (0 / 1 / many problems), because that
    /// is the matrix the bug lived in and no single case would have exposed it.
    #[test]
    fn the_problem_listing_follows_the_verdict_that_named_it() {
        for empty in [true, false] {
            for n in [0, 1, 5] {
                assert_eq!(
                    problem_listing_start(true, empty, n),
                    None,
                    "a declined store lists nothing (empty={empty}, n={n}): the decline is \
                     the verdict, and orphan lines would describe one nobody gave"
                );
            }
        }
        // Empty store: the verdict names reason #1 inline, so the listing resumes after it
        // and each problem is printed exactly once.
        assert_eq!(problem_listing_start(false, true, 0), None);
        assert_eq!(problem_listing_start(false, true, 1), Some(1));
        assert_eq!(problem_listing_start(false, true, 5), Some(1));
        // Populated store: the verdict names only a count, so all of them list.
        assert_eq!(problem_listing_start(false, false, 0), None);
        assert_eq!(problem_listing_start(false, false, 1), Some(0));
        assert_eq!(problem_listing_start(false, false, 5), Some(0));
    }

    /// …and it must report ALL of them. The scan used to `.find()`, so a second failing
    /// program stayed invisible until the first was repaired — and because `status.toml` is
    /// a `BTreeMap`, which failure you were shown was alphabetical accident.
    #[test]
    fn every_recorded_problem_is_reported_not_just_the_first() {
        let l = layout("multi-problem");
        install(&l, "ay", 18);
        let existing = crate::status::read(&l).unwrap_or_default();
        let mut programs = existing.programs;
        for (name, state) in [
            ("ay", "active"),
            ("clean", "error: no build for this Mac"),
            ("trust", "blocked: disk full"),
            ("ty", "aborted: activate"),
        ] {
            programs.insert(
                name.to_string(),
                crate::ProgramStatus {
                    installed_build: None,
                    state: state.into(),
                    tree_root: String::new(),
                },
            );
        }
        crate::status::write(&l, &crate::Status { programs, ..existing }).unwrap();

        let home = synthetic_home("multi-problem");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        assert!(
            !run_with(&l, Some(&home), Some(&path), 0, None, None),
            "three recorded faults must fail the health verdict"
        );

        // THE ASSERTION THAT PROVES THE FIX. Against the real collector, not a
        // re-implementation of it: the old `.find()` could return at most one of these, and
        // `clean` — alphabetically first — is the one it would have returned, leaving
        // `trust` and `ty` unseen.
        let status = crate::status::read(&l).expect("status present");
        assert_eq!(
            recorded_problems(Some(&status)),
            vec![
                "clean: error: no build for this Mac".to_string(),
                "trust: blocked: disk full".to_string(),
                "ty: aborted: activate".to_string(),
            ],
            "all three faults are reported — one error, one blocked, one aborted"
        );
        // The healthy member is not swept up in the reporting.
        assert!(
            !recorded_problems(Some(&status)).iter().any(|p| p.starts_with("ay:")),
            "an active program is not a problem"
        );

        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// …unless the emptiness was ASKED FOR. A user who removed the toolset is not
    /// broken, and telling them so would train them to ignore the diagnostic.
    #[test]
    fn a_declined_store_is_healthy_while_empty() {
        let l = layout("declined-store");
        let home = synthetic_home("declined-store");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        std::fs::create_dir_all(&l.prefix).unwrap();
        std::fs::write(l.declined(), b"# removed on purpose\n").unwrap();
        assert!(
            run_with(&l, Some(&home), Some(&path), 0, None, None),
            "a deliberate removal is a healthy state, not a fault"
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
