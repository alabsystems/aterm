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
pub(crate) fn recorded_problems(status: Option<&crate::Status>) -> Vec<String> {
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
pub fn run(layout: &Layout, prefix: &str) -> bool {
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
        prefix,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

/// The testable core: `home`, the `PATH` value, `now`, the `[packages].account`
/// config override, and the resolved token-source LABEL are injected so the surface
/// can be exercised against a synthetic environment without mutating the process env
/// (or spawning the keychain/`gh` probes).
#[must_use]
#[allow(clippy::too_many_arguments)] // the injected environment, plus the speaker and its streams
pub fn run_with(
    layout: &Layout,
    home: Option<&Path>,
    path_var: Option<&OsStr>,
    now: i64,
    cfg_account: Option<&str>,
    token_source: Option<&str>,
    prefix: &str,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> bool {
    // THE SPEAKER IS THE VERB THE USER TYPED. `status` is an alias for this same report,
    // and it used to answer every line as "doctor:" — so a user could not tell which verb
    // they had run, and a script keying on the prefix keyed on the wrong verb. Same
    // checks, same exit codes; only the signature matches the invocation.
    let p = prefix;
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
    let _ = writeln!(
        out,
        "{p}: this atpkg is {} at {}",
        env!("CARGO_PKG_VERSION"),
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown path>".to_string())
    );
    report_aterm_posture(layout, p, out);

    // (1) TRUST ROOT + INDEX SOURCE + TOKEN SOURCE.
    let _ = writeln!(
        out,
        "{p}: index source github.com/{}",
        crate::resolve_account(cfg_account).slug()
    );
    if crate::manager_enabled() {
        // The root is the PAPER MASTER — the same anchor the app updater uses, not a
        // package-specific key. Naming it here is what lets an operator answer "which
        // trust root is this build on?" without reading source.
        let _ = writeln!(
            out,
            "{p}: ok — paper master pinned (fingerprint {}, {} key(s))",
            crate::root_key_fingerprint(),
            crate::PKG_TRUST_ANCHORS.len()
        );
    } else {
        let _ = writeln!(
            out,
            "{p}: warn — disabled/inert (no paper master compiled in \
             (pins::PAPER_MASTER_PUBKEYS is empty), or ATPKG_DISABLE set) — this build \
             installs nothing"
        );
    }
    // Loud token provenance (never the token itself): which source of the
    // `$ATPKG_TOKEN` → aterm-update-core chain (env → keychain → 0600 file →
    // `$GITHUB_TOKEN`/`$GH_TOKEN` → `gh auth token`) supplied a credential.
    match token_source {
        Some(src) => {
            let _ = writeln!(
                out,
                "{p}: ok — GitHub token from {src} (used for index/pkg fetches; never printed)"
            );
        }
        None => {
            let _ = writeln!(
                out,
                "{p}: ok — no GitHub token provisioned (anonymous API: fine for public \
             repos, rate-limited; `gh auth login` provisions one; private fetch overrides \
             need one)"
            );
        }
    }

    // (2) PREFIX / STORE.
    if layout.prefix.is_dir() {
        let _ = writeln!(out, "{p}: ok — prefix {}", layout.prefix.display());
    } else {
        let _ = writeln!(
            out,
            "{p}: warn — prefix {} does not exist yet (nothing installed)",
            layout.prefix.display()
        );
    }

    // (3) PATH WIRING.
    let bin_dir = layout.bin_dir();
    let on_path = path_var
        .map(|p| std::env::split_paths(p).any(|d| d == bin_dir))
        .unwrap_or(false);
    if on_path {
        let _ = writeln!(out, "{p}: ok — managed bin/ is on PATH");
    } else {
        // The bin path used to appear TWICE on this line — once as the subject, once
        // inside the export — doubling the longest token in the whole report. The
        // copy-pasteable export is the copy that earns its bytes; "managed bin/" matches
        // the ok-branch's name for the same thing.
        let _ = writeln!(
            out,
            "{p}: warn — managed bin/ is not on PATH; an aterm shell auto-sources \
             ~/.aterm/shell.d (which APPENDS it), or add: {}",
            manual_path_hint(&bin_dir)
        );
    }

    // (4) BROKEN SHIM SCAN of bin/ — a shim whose forward target is GONE (a dangling
    // symlink on Unix; on Windows a `.cmd` forwarding to a missing exe, which no symlink
    // scan could ever catch). `resolve_shim` reads the target cross-platform; a tombstone
    // (deliberately target-less) yields `None` and is never flagged.
    if let Ok(entries) = std::fs::read_dir(&bin_dir) {
        for e in entries.flatten() {
            let shim = e.path();
            if let Some(target) = crate::platform::resolve_shim(&shim)
                && !target.exists()
            {
                fails += 1;
                let _ = writeln!(err, "{p}: FAIL — broken bin shim {}", shim.display());
            }
        }
    }

    // (5) ACTIVE-BUILD STORE INTEGRITY.
    let active = crate::ops::active_builds(layout);
    // The first program whose store tree is missing/incomplete — remembered so the
    // verdict tail can name ONE structural repair (`install <program>`) instead of a menu.
    let mut next_install_program: Option<String> = None;
    for (program, build) in &active {
        let bd = layout.build_dir(program, *build);
        if !bd.is_dir() || !crate::store::build_is_complete(&bd) {
            fails += 1;
            let _ = writeln!(
                err,
                "{p}: FAIL — active {program} build {build} store missing/incomplete"
            );
            if next_install_program.is_none() {
                next_install_program = Some(program.clone());
            }
        }
    }
    let _ = writeln!(out, "{p}: ok — {} program(s) active", active.len());

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
    if let Some(trust_build) = active
        .iter()
        .find(|(p, _)| p.as_str() == "trust")
        .map(|(_, b)| *b)
    {
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
            let managed_version = probe_version(
                &layout
                    .build_dir(program, *managed_build)
                    .join("bin")
                    .join(program),
            );
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
            let override_hint = if program.as_str() == "ay" {
                " (override: AY_PATH)"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "{p}: note — Trust builds use the {program} pinned inside the trust bundle \
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
    // A shim/channel divergence's repair is a re-run of `update` — remembered for the
    // verdict tail's single `next` act.
    let mut next_update_divergence = false;
    for d in live.diverged() {
        match &d.reason {
            crate::gc::Diverged::ChannelShimMismatch {
                channel_says,
                shims_say,
            } => {
                fails += 1;
                next_update_divergence = true;
                let _ = writeln!(
                    err,
                    "{p}: FAIL — {}: the channel selects build {channel_says} but its bin/ \
                     shims run build {shims_say} (re-run `aterm pkg update {}`)",
                    d.program, d.program
                );
            }
            crate::gc::Diverged::ShimsDisagree { builds } => {
                fails += 1;
                next_update_divergence = true;
                let _ = writeln!(
                    err,
                    "{p}: FAIL — {}: its bin/ shims are split across builds {} — one \
                     program's tools must all point into one build (re-run `aterm pkg update {}`)",
                    d.program,
                    build_list(builds),
                    d.program
                );
            }
            crate::gc::Diverged::ChannelsDisagree { builds } => {
                fails += 1;
                next_update_divergence = true;
                let _ = writeln!(
                    err,
                    "{p}: FAIL — {}: two channel `current` links select different builds \
                     {} and it has no `store/{}/current` of its own to break the tie — run \
                     `aterm pkg update {}` to write one",
                    d.program,
                    build_list(builds),
                    d.program,
                    d.program
                );
            }
            crate::gc::Diverged::NoLiveWitness { shims_say } => {
                let _ = writeln!(
                    out,
                    "{p}: warn — {}: build {shims_say} is on PATH but no `current` link \
                     selects it, so gc keeps every superseded {} build. Run \
                     `aterm pkg update {}` to re-activate it and clear this.",
                    d.program, d.program, d.program
                );
            }
        }
    }
    let _ = writeln!(
        out,
        "{p}: ok — {} program(s) with a proven live build",
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
            let _ = writeln!(out, "{p}: ok — shell.d hooks present");
        } else {
            let _ = writeln!(
                out,
                "{p}: warn — shell.d hooks not generated yet (an install writes them)"
            );
        }
        if let Ok(entries) = std::fs::read_dir(&shell_d) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().ends_with(".sh") {
                    fails += 1;
                    let _ = writeln!(
                        err,
                        "{p}: FAIL — shell.d/{}: a POSIX .sh breaks fish — remove it",
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
                let _ = writeln!(
                    err,
                    "{p}: FAIL — {} is group/other-writable (login shells source it)",
                    dir.display()
                );
            }
        }
    }

    // (7) DISK HEADROOM.
    match crate::freespace::available_bytes(&layout.prefix) {
        Some(free) if free < 5 * GIB => {
            let _ = writeln!(
                out,
                "{p}: warn — only {} free (a toolchain update needs ~2.5x its artifact size)",
                crate::cost::human_bytes(free)
            );
        }
        Some(free) => {
            let _ = writeln!(out, "{p}: ok — {} free", crate::cost::human_bytes(free));
        }
        None => {
            let _ = writeln!(out, "{p}: warn — could not query free space");
        }
    }

    // (8) INDEX FREEZE / AGE (no unverified parse — atpkg's OWN diagnostics only).
    if let Some(status) = crate::status::read(layout) {
        match index_age_days(&status.updated_at, now) {
            Some(days) if days > 30 => {
                let _ = writeln!(
                    out,
                    "{p}: warn — {days} day(s) since the last successful update ({}) — publishing \
                 looks frozen or this machine has been offline",
                    status.updated_at
                );
            }
            Some(days) => {
                let _ = writeln!(
                    out,
                    "{p}: ok — {days} day(s) since the last successful update"
                );
            }
            None => {
                let _ = writeln!(out, "{p}: warn — could not parse the last-update time");
            }
        }
    } else {
        let _ = writeln!(out, "{p}: warn — no status.toml yet (no update has run)");
    }
    // The build floor is printed WITH the generation that recorded it, because that pair
    // is the actual gate: a floor stamped with an older generation is re-based by the next
    // master-signed one rather than obeyed (`sig::BuildFloor`), so a reader who saw only
    // the number could not tell a binding floor from an inherited one.
    let build_floor = crate::sig::BuildFloor {
        index_build: crate::sig::Floor::new(layout.floor()).current(),
        roster_seq: crate::sig::Floor::new(layout.floor_generation()).current(),
    };
    let _ = writeln!(
        out,
        "{p}: last-trusted index_build {} (recorded under roster_seq {})",
        build_floor.index_build, build_floor.roster_seq
    );
    // The SECOND durable ratchet, shown beside the first because they answer different
    // questions and move independently: `index_build` is how far the toolchain index has
    // advanced, `roster_seq` is which generation of the machine roster this store has
    // accepted. A roster floor that is stuck while machines have been minted or revoked
    // means this store has not seen a publish since, which is worth being able to see.
    let _ = writeln!(
        out,
        "{p}: last-trusted roster_seq {}",
        crate::sig::Floor::new(layout.roster_floor()).current()
    );

    // (9) RUSTUP + RELOCATABILITY.
    if !rustup_present() {
        let _ = writeln!(
            out,
            "{p}: warn — rustup not found (self-contained bundles are portable)"
        );
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
        let _ = writeln!(
            out,
            "{p}: warn — the record holds a stray row for {stray:?}, which cannot be a \
             program name (it is a command-line flag, left by a mistyped `atpkg install`). \
             No program is missing because of it; the next successful `aterm pkg update` \
             clears it"
        );
    }
    // (10a) SYSTEM-SATISFIED MEMBERS. A member the signed index marks `system = "<bin>"`
    // that the pass found on PATH outside the prefix is deliberately NOT managed here —
    // its row says so in the canonical words (`system: <path> — not managed by aterm`),
    // and it is neither missing nor a fault. Re-check the recorded path: a binary that
    // has since gone away is the one state worth a line, because the next pass will
    // install the member through its artifact and a user may wonder why.
    if let Some(s) = status.as_ref() {
        for (program, row) in &s.programs {
            let Some(path) = crate::state::system_path(&row.state) else {
                continue;
            };
            if std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                let _ = writeln!(
                    out,
                    "{p}: ok — {program}: {} (remove that copy to have atpkg manage {program})",
                    row.state
                );
            } else {
                let _ = writeln!(
                    out,
                    "{p}: warn — {program}: recorded as `{}`, but that copy is gone — system \
                     copy gone: the next `aterm pkg update` reinstalls the managed copy",
                    row.state
                );
            }
        }
        // (10b) MEMBERS WAITING ON ELEVATION (`needs admin — run: aterm pkg install
        // <name>`): not a fault — the unattended pass cannot elevate — but the one line
        // that tells the user which act is theirs.
        for (program, row) in &s.programs {
            if row.state.starts_with(crate::state::NEEDS_ADMIN_PREFIX) {
                let _ = writeln!(
                    out,
                    "{p}: warn — {program}: {} (in a terminal; sudo asks there)",
                    row.state
                );
            }
        }
        // (10c) MEMBERS OBTAINED THROUGH ANOTHER PROTOCOL (`installed via <protocol>:
        // <path>` — Homebrew's pkg, Apple's Command Line Tools): proven by the recorded
        // `provides` path, re-checked here the way a system copy is. Gone ⇒ the next
        // pass records `needs admin` and the explicit door reinstalls it.
        for (program, row) in &s.programs {
            let Some((protocol, path)) = crate::state::installed_via_path(&row.state) else {
                continue;
            };
            if std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                let _ = writeln!(
                    out,
                    "{p}: ok — {program}: {} (kept current by {protocol}, not by aterm)",
                    row.state
                );
            } else {
                let _ = writeln!(
                    out,
                    "{p}: warn — {program}: recorded as `{}`, but that path is gone — the \
                     next pass records it as needing admin; reinstall: aterm pkg install \
                     {program}",
                    row.state
                );
            }
        }
    }
    // (10e) MEMBERS BLOCKED BY A REQUIREMENT (`blocked by <dep>: <dep state>`, §17.10):
    // a DEFERRED state, never a fault — the pass retries every six hours, and the
    // dependency's own row (quoted in the state) says whose act unblocks it. The
    // explicit door resolves both, in order.
    if let Some(s) = status.as_ref() {
        for (program, row) in &s.programs {
            if let Some((dep, _)) = crate::state::blocked_by(&row.state) {
                let _ = writeln!(
                    out,
                    "{p}: warn — {program}: {} (installs once {dep} is; `aterm pkg install \
                     {program}` does both, in order)",
                    row.state
                );
            }
        }
    }
    // (10d) SHADOWED MANAGED MEMBERS (design S5). For every managed member and every tool
    // it exposes, a foreign executable of that name EARLIER on PATH than the managed
    // bin/ is what actually runs — silently ahead of the build the index pins. Probed
    // LIVE against this process's PATH, never trusted from the record: a warning, never a
    // fault, and never "fixed" here — the user owns PATH.
    for (program, build) in &installed {
        if crate::linkmode::is_linked(layout, program) {
            continue;
        }
        let shadow = crate::ops::active_tools(layout, program, *build)
            .into_iter()
            .find_map(|tool| {
                crate::vendor::shadowing_binary_on_path(&layout.prefix, tool.as_str(), path_var)
                    .map(|path| (tool, path))
            });
        if let Some((tool, path)) = shadow {
            // The canonical state in the canonical words, then — when `alab-<tool>` is
            // laid — the one trailing sentence that names the way to the managed copy
            // without touching PATH (`cli::alias_fix`; never part of the state).
            let fix = crate::cli::alias_fix(layout, &tool, program, path_var)
                .map(|f| format!(" — {f}"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "{p}: warn — {program}: {} (not the pinned build; atpkg never edits PATH — \
                 remove or reorder that copy if you want the managed one){fix}",
                crate::state::shadowed(*build, &path)
            );
        }
    }
    // (10f) MANAGED MEMBERS WHOSE SHIM EXPORTS AN ENVIRONMENT (design S7): a vendor tool
    // whose signed manifest declared `shim_env` runs, through the managed shim, with its
    // own updater off — `self-update off (DISABLE_AUTOUPDATER=1)`. Read off the shim as
    // laid (the thing that runs), printed as ONE trailing sentence after the canonical
    // row, never inside it; withheld when a foreign copy shadows the shim (the env never
    // reaches a system copy — that member's line is (10d)'s warn). An `ok`, never a fault.
    for (program, build) in &installed {
        if crate::linkmode::is_linked(layout, program) {
            continue;
        }
        let tools = crate::ops::active_tools(layout, program, *build);
        let Some(fix) = tools
            .iter()
            .find_map(|t| crate::cli::shim_env_fix(&layout.shim(t)))
        else {
            continue;
        };
        if tools.iter().any(|t| {
            crate::vendor::shadowing_binary_on_path(&layout.prefix, t.as_str(), path_var).is_some()
        }) {
            continue;
        }
        let state = status
            .as_ref()
            .and_then(|s| s.programs.get(program))
            .filter(|r| crate::state::managed_pin(&r.state).is_some())
            .map_or_else(
                || crate::state::managed(*build, build_floor.index_build),
                |r| r.state.clone(),
            );
        let _ = writeln!(
            out,
            "{p}: ok — {program}: {state} — {fix} (updates arrive with the ALab index)"
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
        let _ = writeln!(
            out,
            "{p}: the ALab toolset was removed on this machine (`aterm pkg install \
             --default-set` reinstalls it)"
        );
    } else if installed.is_empty() {
        toolset_problem = true;
        match recorded_problems.first() {
            Some(why) => {
                let _ = writeln!(out, "{p}: PROBLEM — no ALab programs are installed ({why})");
            }
            // No per-program row survives an ENVIRONMENTAL failure any more — an
            // unreachable index says nothing about any particular program — so the
            // aggregate sentence is now the only place the reason lives. Preferring it to
            // the generic hint is what keeps "why did nothing arrive?" answerable offline.
            None => match status
                .as_ref()
                .map(|s| s.outcome.as_str())
                .filter(|o| !o.is_empty())
            {
                Some(outcome) => {
                    let _ = writeln!(
                        out,
                        "{p}: PROBLEM — no ALab programs are installed (last attempt: {outcome})"
                    );
                }
                None => {
                    let _ = writeln!(
                        out,
                        "{p}: PROBLEM — no ALab programs are installed. Fix: aterm pkg install \
                     --default-set (installs the whole ALab toolset; if no build is \
                     published for this machine it names the reason and exits 2)"
                    );
                }
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
        let _ = writeln!(
            out,
            "{p}: PROBLEM — the toolset is incomplete; {} program(s) active",
            installed.len()
        );
    } else {
        let _ = writeln!(out, "{p}: {} ALab program(s) active", installed.len());
    }
    if let Some(start) =
        problem_listing_start(declined, installed.is_empty(), recorded_problems.len())
    {
        for why in recorded_problems.iter().skip(start) {
            let _ = writeln!(out, "{p}:   {why}");
        }
    }

    if fails == 0 && !toolset_problem {
        let _ = writeln!(out, "{p}: healthy");
        true
    } else {
        let total = fails + usize::from(toolset_problem);
        let _ = writeln!(out, "{p}: found {total} problem(s)");
        // THE ONE NEXT ACT — a single command, never a menu: a report that ends in a pile
        // of problems and three suggestions teaches a first-hour user to close the
        // terminal. Priority: install the missing SET (an empty store has exactly one
        // fix), then `update` (a successful pass rewrites every recorded fault row and
        // re-flips diverged shims), then the structural repair of one named program.
        // Failures with their remedy already inline (a stray .sh — "remove it") add no
        // line here rather than a second, vaguer act.
        let next = if toolset_problem && installed.is_empty() {
            Some(String::from("aterm pkg install --default-set"))
        } else if (!recorded_problems.is_empty() && !declined) || next_update_divergence {
            Some(String::from("aterm pkg update"))
        } else {
            next_install_program.map(|program| format!("aterm pkg install {program}"))
        };
        if let Some(act) = next {
            let _ = writeln!(out, "{p}: next — {act}");
        }
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
    let Some(out) = output_bounded(std::process::Command::new(bin).arg("--version")) else {
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
    output_bounded(std::process::Command::new("rustup").arg("--version"))
        .is_some_and(|o| o.status.success())
}

/// How long ONE `--version` probe may take before `doctor` gives up on it.
///
/// These are local binaries printing one line; a healthy one answers in
/// milliseconds. The ceiling exists for the unhealthy case, which is not
/// hypothetical here: `doctor` and `status` probe EVERY installed program plus
/// `rustup`, so one wedged binary — a stale NFS mount, a `rustup` shim waiting
/// on a network toolchain fetch, a program stopped on a debugger — hung the
/// whole report with no output and no way to tell what it was waiting for.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Poll interval while waiting for a probe to exit.
const PROBE_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// Run a version probe with a bounded wall clock, killing and reaping it on
/// timeout. `None` when it could not be spawned, did not finish in time, or
/// could not be waited for.
///
/// Fails to `None`, not to an error, and that is the right direction HERE (the
/// opposite of the updater's fail-closed helpers this mirrors): the probe is a
/// convenience that renders one column of a report, and `doctor`'s contract is
/// that it never fails because a probe failed — the store integrity checks are
/// the verdict. A timed-out probe reads `unknown`, exactly like a binary that
/// will not run.
fn output_bounded(cmd: &mut std::process::Command) -> Option<std::process::Output> {
    use std::io::Read as _;
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                std::thread::sleep(PROBE_POLL.min(remaining));
            }
            Err(_) => return None,
        }
    };
    // The child has exited, so its stdout is closed and this read cannot block —
    // and a version line is one line, far inside any pipe buffer, so nothing
    // could have wedged the child on a full pipe before it got here either.
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    Some(std::process::Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
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
fn report_aterm_posture(layout: &crate::store::Layout, p: &str, out: &mut dyn std::io::Write) {
    let Some(support) = layout.prefix.parent() else {
        return;
    };
    let updates = support.join("Updates");
    let field = |file: &str, key: &str| -> Option<String> {
        let text = std::fs::read_to_string(updates.join(file)).ok()?;
        text.lines()
            .find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key))
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
    };
    let Some(installed) = field("installed.toml", "build_number") else {
        return;
    };
    match (
        field("status.toml", "current_build"),
        field("status.toml", "staged_build"),
    ) {
        (Some(current), Some(staged)) if current != staged => {
            let _ = writeln!(
                out,
                "{p}: note — aterm is RUNNING build {current}, installed {installed}, with \
             build {staged} staged and waiting for a restart"
            );
        }
        (Some(current), _) if current != installed => {
            let _ = writeln!(
                out,
                "{p}: note — aterm is RUNNING build {current} but build {installed} is \
             installed; the running process predates it"
            );
        }
        _ => {
            let _ = writeln!(out, "{p}: ok — aterm build {installed} installed");
        }
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
        install_shims(
            layout,
            &dir,
            &[program.to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
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
            run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
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
            !run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
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
            crate::status::write(
                &l,
                &crate::Status {
                    programs,
                    ..existing
                },
            )
            .unwrap();
            l
        };

        let l = stray_row("--help");
        let home = synthetic_home("stray-help");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        assert!(
            run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
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
            !run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
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
        crate::status::write(
            &l,
            &crate::Status {
                programs,
                ..existing
            },
        )
        .unwrap();

        let home = synthetic_home("multi-problem");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        assert!(
            !run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
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
            !recorded_problems(Some(&status))
                .iter()
                .any(|p| p.starts_with("ay:")),
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
            run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
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
            !run_with(
                &l,
                Some(&home),
                None,
                0,
                None,
                None,
                "doctor",
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
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
            !run_with(
                &l,
                Some(&home),
                None,
                0,
                None,
                None,
                "doctor",
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
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
            !run_with(
                &l,
                Some(&home),
                None,
                0,
                None,
                None,
                "doctor",
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
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
            !run_with(
                &l,
                Some(&home),
                None,
                0,
                None,
                None,
                "doctor",
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
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
        install_shims(&l, &dir, &["ay".to_string()], crate::activate::Aliases::Off).unwrap(); // shimmed, never activated
        crate::store::mark_build_ready(&dir).unwrap();
        let home = synthetic_home("witness-absent");
        assert!(
            run_with(
                &l,
                Some(&home),
                None,
                0,
                None,
                None,
                "doctor",
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
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
            run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "a PATH warning stays exit-0"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A `satisfied by system` row is an OK line while the system binary is there, a WARN
    /// (never a PROBLEM) once it is gone — and it is never counted as a recorded fault.
    #[test]
    fn a_system_satisfied_member_is_reported_and_never_a_fault() {
        let l = layout("system-satisfied");
        install(&l, "ay", 19);
        let exe = l.prefix.join("fake-system-gh");
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        let existing = crate::status::read(&l).unwrap_or_default();
        let mut programs = existing.programs.clone();
        programs.insert(
            "gh".into(),
            crate::ProgramStatus {
                installed_build: None,
                state: crate::state::system(&exe, Some("2026-08-27")),
                tree_root: String::new(),
            },
        );
        crate::status::write(
            &l,
            &crate::Status {
                schema: 1,
                updated_at: "2026-08-27T00:00:00Z".into(),
                enabled: true,
                index_source: "alabsystems/aterm".into(),
                outcome: "up to date".into(),
                programs,
            },
        )
        .unwrap();
        let home = synthetic_home("system-satisfied");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let run = |l: &Layout| {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let ok = run_with(
                l,
                Some(&home),
                Some(&path),
                now,
                None,
                None,
                "doctor",
                &mut out,
                &mut err,
            );
            (ok, String::from_utf8_lossy(&out).into_owned())
        };
        let (ok, out) = run(&l);
        assert!(ok, "a satisfied member is not a problem:\n{out}");
        // The SAME words the pass wrote — the canonical state, retirement note included.
        assert!(
            out.contains(&format!(
                "doctor: ok — gh: system: {} — not managed by aterm (managed copy retired \
                 2026-08-27)",
                exe.display()
            )),
            "{out}"
        );
        assert!(recorded_problems(crate::status::read(&l).as_ref()).is_empty());
        // The system binary goes away: a warning naming the remedy, still not a problem.
        std::fs::remove_file(&exe).unwrap();
        let (ok, out) = run(&l);
        assert!(ok, "{out}");
        assert!(
            out.contains("doctor: warn — gh: recorded as `system: ")
                && out.contains("system copy gone: the next `aterm pkg update` reinstalls"),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// An `installed via <protocol>: <path>` row (Homebrew's pkg, the Command Line
    /// Tools) is an OK line while its `provides` path is there — the words the pass
    /// wrote — and a WARN naming the door once it is gone; `needs admin` is a WARN with
    /// the door's spelling. Neither is ever a recorded fault.
    #[test]
    fn an_os_installed_member_is_reported_by_its_provides_path_and_never_a_fault() {
        let l = layout("installed-via");
        install(&l, "ay", 19);
        let brew = l.prefix.join("fake-opt-homebrew-bin-brew");
        std::fs::write(&brew, b"#!/bin/sh\nexit 0\n").unwrap();
        let existing = crate::status::read(&l).unwrap_or_default();
        let mut programs = existing.programs.clone();
        programs.insert(
            "brew".into(),
            crate::ProgramStatus {
                installed_build: None,
                state: crate::state::installed_via("pkg", &brew),
                tree_root: String::new(),
            },
        );
        programs.insert(
            "clt".into(),
            crate::ProgramStatus {
                installed_build: None,
                state: crate::state::needs_admin("clt"),
                tree_root: String::new(),
            },
        );
        crate::status::write(
            &l,
            &crate::Status {
                schema: 1,
                updated_at: "2026-08-27T00:00:00Z".into(),
                enabled: true,
                index_source: "alabsystems/aterm".into(),
                outcome: "up to date".into(),
                programs,
            },
        )
        .unwrap();
        let home = synthetic_home("installed-via");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let run = |l: &Layout| {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let ok = run_with(
                l,
                Some(&home),
                Some(&path),
                now,
                None,
                None,
                "doctor",
                &mut out,
                &mut err,
            );
            (ok, String::from_utf8_lossy(&out).into_owned())
        };
        let (ok, out) = run(&l);
        assert!(ok, "an OS-installed member is not a problem:\n{out}");
        assert!(
            out.contains(&format!(
                "doctor: ok — brew: installed via pkg: {} (kept current by pkg, not by aterm)",
                brew.display()
            )),
            "{out}"
        );
        assert!(
            out.contains(
                "doctor: warn — clt: needs admin — run: aterm pkg install clt (in a terminal; \
                 sudo asks there)"
            ),
            "{out}"
        );
        assert!(recorded_problems(crate::status::read(&l).as_ref()).is_empty());
        // The provides path goes away: a warning naming the door, still not a problem.
        std::fs::remove_file(&brew).unwrap();
        let (ok, out) = run(&l);
        assert!(ok, "{out}");
        assert!(
            out.contains("doctor: warn — brew: recorded as `installed via pkg: ")
                && out.contains("reinstall: aterm pkg install brew"),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// SHADOWED (design S5): a foreign copy of a managed tool AHEAD of the managed bin/ on
    /// PATH is a WARN line in the canonical words — never a fault, never touched — and a
    /// copy BEHIND the managed bin/ is not mentioned at all.
    #[cfg(unix)]
    /// A member BLOCKED by a requirement (§17.10) is reported in its own words as a
    /// `warn` naming the dependency and the door that installs both — and never counted
    /// as a fault: `doctor` stays healthy over it.
    #[test]
    fn a_blocked_member_is_a_warning_naming_its_dependency_never_a_fault() {
        let layout = layout("doctor-blocked");
        install(&layout, "ay", 18);
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "ay".to_string(),
            crate::ProgramStatus {
                installed_build: Some(18),
                state: crate::state::managed(18, 41),
                tree_root: String::new(),
            },
        );
        let blocked = crate::state::blocked("clt", &crate::state::needs_admin("clt"));
        programs.insert(
            "brew".to_string(),
            crate::ProgramStatus {
                installed_build: None,
                state: blocked.clone(),
                tree_root: String::new(),
            },
        );
        let status = crate::Status {
            schema: 1,
            updated_at: "2026-08-27T00:00:00Z".into(),
            enabled: true,
            index_source: "x/y".into(),
            outcome: "up to date".into(),
            programs,
        };
        crate::status::write(&layout, &status).unwrap();
        assert!(
            recorded_problems(Some(&status)).is_empty(),
            "a blocked row is deferred, not a fault"
        );
        let home = synthetic_home("doctor-blocked");
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let path = std::env::join_paths([layout.bin_dir()]).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let ok = run_with(
            &layout,
            Some(&home),
            Some(&path),
            now,
            None,
            None,
            "doctor",
            &mut out,
            &mut err,
        );
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(
            ok,
            "a blocked row is a warning, not a structural fault:
{text}"
        );
        assert!(
            text.contains(&format!("warn — brew: {blocked} (installs once clt is;")),
            "{text}"
        );
        assert!(text.contains("aterm pkg install brew"), "{text}");
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn a_shadowed_managed_member_is_a_warning_never_a_fault() {
        use std::os::unix::fs::PermissionsExt as _;
        let l = layout("shadowed");
        install(&l, "ay", 19);
        let foreign = l
            .prefix
            .parent()
            .unwrap()
            .join(format!("atpkg-doctor-shadow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&foreign);
        std::fs::create_dir_all(&foreign).unwrap();
        let exe = foreign.join("ay");
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let home = synthetic_home("shadowed");
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let run = |path: &std::ffi::OsStr| {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let ok = run_with(
                &l,
                Some(&home),
                Some(path),
                now,
                None,
                None,
                "doctor",
                &mut out,
                &mut err,
            );
            (ok, String::from_utf8_lossy(&out).into_owned())
        };
        let ahead = std::env::join_paths([foreign.clone(), l.bin_dir()]).unwrap();
        let (ok, out) = run(&ahead);
        assert!(ok, "a shadow is a warning, not a structural fault:\n{out}");
        assert!(
            out.contains(&format!(
                "doctor: warn — ay: {}",
                crate::state::shadowed(19, &exe)
            )),
            "{out}"
        );
        assert!(
            exe.exists() && crate::which(&l, "ay").is_some(),
            "never fixed"
        );
        let behind = std::env::join_paths([l.bin_dir(), foreign.clone()]).unwrap();
        let (ok, out) = run(&behind);
        assert!(ok, "{out}");
        assert!(!out.contains("SHADOWED"), "{out}");
        // No alias laid (a vendor-shaped install): no fix-line either.
        let (_, out) = run(&ahead);
        assert!(!out.contains("for the managed one"), "{out}");
        let _ = std::fs::remove_dir_all(&foreign);
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// DESIGN S7 on the `doctor` surface: a managed member whose shim exports its
    /// manifest's `shim_env` gets ONE `ok` line — the canonical row (the recorded one when
    /// it is managed, else derived) and the trailing `self-update off (…)` sentence —
    /// never a fault, never inside the state; a plain shim gets no such line, and a
    /// shadowed one keeps (10d)'s warn alone (the env never reaches the system copy).
    #[cfg(unix)]
    #[test]
    fn a_managed_member_with_a_shim_env_says_self_update_off_as_a_trailing_line() {
        use std::os::unix::fs::PermissionsExt as _;
        let l = layout("shim-env");
        let dir = install_build_tree(&l, "claude", 2026082701);
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        crate::activate::install_tools_env(
            &l,
            &dir,
            &[tool("claude")],
            crate::activate::Aliases::Off,
            &env,
        )
        .unwrap();
        activate_channel(&l, "stable", &dir).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "claude".to_string(),
            crate::ProgramStatus {
                installed_build: Some(2026082701),
                state: crate::state::managed(2026082701, 41),
                tree_root: String::new(),
            },
        );
        crate::status::write(
            &l,
            &crate::Status {
                schema: 1,
                updated_at: "2026-08-28T00:00:00Z".into(),
                enabled: true,
                index_source: "alabsystems/aterm".into(),
                outcome: "up to date".into(),
                programs,
            },
        )
        .unwrap();
        let home = synthetic_home("shim-env");
        let now = crate::flow::rfc3339_to_unix("2026-08-28T00:00:00Z").unwrap();
        let run = |path: &std::ffi::OsStr| {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let ok = run_with(
                &l,
                Some(&home),
                Some(path),
                now,
                None,
                None,
                "doctor",
                &mut out,
                &mut err,
            );
            (ok, String::from_utf8_lossy(&out).into_owned())
        };
        let managed_only = std::env::join_paths([l.bin_dir()]).unwrap();
        let (ok, out) = run(&managed_only);
        assert!(ok, "{out}");
        let line = out
            .lines()
            .find(|l| l.contains("ok — claude:"))
            .unwrap_or_else(|| panic!("an ok line for claude:\n{out}"));
        assert_eq!(
            line,
            "doctor: ok — claude: managed 2026082701 — pinned by index 41 — self-update off \
             (DISABLE_AUTOUPDATER=1) (updates arrive with the ALab index)"
        );
        // Shadowed: the warn alone — the foreign copy runs, and runs without the env.
        let foreign = l
            .prefix
            .parent()
            .unwrap()
            .join(format!("atpkg-doctor-env-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&foreign);
        std::fs::create_dir_all(&foreign).unwrap();
        let exe = foreign.join("claude");
        std::fs::write(&exe, b"#!/bin/sh\necho vendor\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ahead = std::env::join_paths([foreign.clone(), l.bin_dir()]).unwrap();
        let (ok, out) = run(&ahead);
        assert!(ok, "{out}");
        assert!(out.contains("warn — claude:"), "{out}");
        assert!(!out.contains("self-update"), "{out}");
        // A plain shim: no line about it at all.
        crate::activate::install_tools(&l, &dir, &[tool("claude")], crate::activate::Aliases::Off)
            .unwrap();
        let (ok, out) = run(&managed_only);
        assert!(ok, "{out}");
        assert!(!out.contains("self-update"), "{out}");
        let _ = std::fs::remove_dir_all(&foreign);
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// THE p11-kit STORY (§17.11): a Homebrew-style `trust` ahead of the managed bin/
    /// shadows ALab's `trust`; the warn line keeps the canonical state and gains ONE
    /// trailing sentence naming the alias that runs the managed copy — because the alias
    /// is laid, and only because of that.
    #[cfg(unix)]
    #[test]
    fn a_shadowed_alab_tool_names_its_alias_as_the_way_to_the_managed_copy() {
        use std::os::unix::fs::PermissionsExt as _;
        let l = layout("shadowed-alias");
        let dir = install_build_tree(&l, "trust", 6808);
        install_shims(
            &l,
            &dir,
            &["trust".to_string()],
            crate::activate::Aliases::Alab,
        )
        .unwrap();
        activate_channel(&l, "stable", &dir).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        let homebrew = l
            .prefix
            .parent()
            .unwrap()
            .join(format!("atpkg-doctor-homebrew-bin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&homebrew);
        std::fs::create_dir_all(&homebrew).unwrap();
        let exe = homebrew.join("trust");
        std::fs::write(&exe, b"#!/bin/sh\necho p11-kit\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let home = synthetic_home("shadowed-alias");
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let ahead = std::env::join_paths([homebrew.clone(), l.bin_dir()]).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let ok = run_with(
            &l,
            Some(&home),
            Some(&ahead),
            now,
            None,
            None,
            "doctor",
            &mut out,
            &mut err,
        );
        let out = String::from_utf8_lossy(&out).into_owned();
        assert!(ok, "a shadow is a warning, not a structural fault:\n{out}");
        let state = crate::state::shadowed(6808, &exe);
        let line = out
            .lines()
            .find(|l| l.contains("warn — trust:"))
            .unwrap_or_else(|| panic!("a warn line for trust:\n{out}"));
        assert!(
            line.contains(&format!("doctor: warn — trust: {state} (")),
            "{line}"
        );
        assert!(
            line.ends_with(" — type alab-trust for the managed one"),
            "the fix-line is the trailing sentence: {line}"
        );
        // And the alias really is what runs the managed copy.
        assert!(
            crate::which(&l, "alab-trust").is_some_and(|t| t.starts_with(&dir)),
            "alab-trust resolves into the managed build"
        );
        let _ = std::fs::remove_dir_all(&homebrew);
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// TWO NAMES, ONE REPORT, AND THE SPEAKER IS THE NAME YOU TYPED. `status` used to
    /// answer every line as "doctor:" — a user could not tell which verb they had run,
    /// and a script keying on the prefix keyed on the wrong verb.
    #[test]
    fn status_never_speaks_as_doctor() {
        let l = layout("speaker");
        install(&l, "ay", 18);
        let home = synthetic_home("speaker");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        for (invoked, other) in [("status", "doctor:"), ("doctor", "status:")] {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let _ = run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                invoked,
                &mut out,
                &mut err,
            );
            for stream in [&out, &err] {
                let text = String::from_utf8_lossy(stream);
                assert!(
                    !text.lines().any(|line| line.starts_with(other)),
                    "invoked as {invoked}, no line may speak as {other}:\n{text}"
                );
                assert!(
                    text.lines()
                        .all(|line| line.is_empty() || line.starts_with(invoked)),
                    "every line is signed by the verb the user typed ({invoked}):\n{text}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// AN UNHEALTHY REPORT ENDS IN ONE ACT. A fresh machine's only story is "nothing
    /// installed yet"; the tail must name the one command that fixes it — and exactly
    /// one, after the byte-stable "found N problem(s)" line, never instead of it.
    #[test]
    fn an_unhealthy_report_names_one_next_act() {
        let l = layout("next-act");
        let home = synthetic_home("next-act");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        assert!(!run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            None,
            "doctor",
            &mut out,
            &mut err
        ));
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("doctor: found 1 problem(s)"),
            "the verdict line keeps its bytes:\n{text}"
        );
        let next: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("doctor: next — "))
            .collect();
        assert_eq!(
            next,
            vec!["doctor: next — aterm pkg install --default-set"],
            "exactly one next act, and it is the whole-set install:\n{text}"
        );
        assert!(
            text.contains("Fix: aterm pkg install"),
            "the PROBLEM line states the remedy as what it is:\n{text}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }
}
