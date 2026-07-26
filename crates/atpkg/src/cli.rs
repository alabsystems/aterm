// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg` — the aterm toolchain package manager CLI.
//!
//! The local read/maintenance verbs (`which`/`list`/`uninstall`) and `doctor` are wired
//! here over the tested library (`crate::ops`, `crate::store`); the network-driven verbs
//! (`install`/`update`/`sync`/`rollback`, plus the `install --default-set` bootstrap)
//! compose the same tested primitives with the GitHub/dir fetch. Every network verb reads
//! the `[packages]` table of the SAME `aterm.toml` the GUI owns ([`crate::config`]) —
//! account/channel/include/exclude/links — with env always winning over config. With no
//! `ATERM_PKG_ROOTKEY` pinned at build time the manager is INERT and says so.

use std::process::ExitCode;

/// The whole package-manager CLI as a callable: `argv[1..]` in, exit code
/// out. Served in-process by the ONE `aterm` binary (`aterm pkg …` / the
/// `atpkg` argv0 alias) and by the thin standalone bin. Everything below is
/// unchanged from the binary era.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    let args: Vec<String> = argv
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let verb = args.first().map(String::as_str);
    // THE single-writer-per-store gate ([`crate::lock`]): every store-MUTATING verb
    // TRY-acquires the store-wide `store.lock` here — at the ONE dispatch edge, so
    // internal verb re-routing (`sync` → update-all, `update <p>` → install) can
    // never double-acquire — and holds it for the whole verb. Contention is a loud
    // exit-1 refusal naming the lock path (the GUI Packages page surfaces the child
    // stderr; the 6-hour loop just retries next pass). Read-only verbs skip this
    // entirely and never need the lock.
    let _store_lock = if verb.is_some_and(verb_mutates_store) {
        match mutator_store_lock() {
            Ok(guard) => guard,
            Err(code) => return code,
        }
    } else {
        None
    };
    match verb {
        Some("doctor") => return doctor(),
        Some("which") => return cmd_which(args.get(1)),
        Some("list") => cmd_list(),
        Some("uninstall") => return cmd_uninstall(args.get(1)),
        Some("tree-root") => return cmd_tree_root(args.get(1)),
        Some("verify-index") => return cmd_verify_index(args.get(1), args.get(2), args.get(3)),
        Some("install") => return cmd_install(args.get(1)),
        Some("seed") => return cmd_seed(&args[1..]),
        Some("update") => return cmd_update(args.get(1)),
        Some("sync") => return cmd_sync(),
        Some("rollback") => return cmd_rollback(args.get(1)),
        Some("pin") => return cmd_pin(args.get(1), true),
        Some("unpin") => return cmd_pin(args.get(1), false),
        Some("gc") => return cmd_gc(),
        Some("verify") => return cmd_verify(args.get(1)),
        Some("link") => return cmd_link(&args[1..]),
        Some("unlink") => return cmd_unlink(args.get(1)),
        Some("refresh") => return cmd_refresh(&args[1..]),
        Some("run") => return cmd_run(&args[1..]),
        Some("relocate") => return cmd_relocate(&args[1..]),
        Some(other) => {
            eprintln!(
                "atpkg: unknown verb '{other}' (try: doctor, which, list, run, uninstall, \
                 install, seed, update, sync, rollback, pin, unpin, gc, verify, link, unlink, \
                 refresh, tree-root, verify-index, relocate)"
            );
            return ExitCode::from(2);
        }
        None => status(),
    }
    ExitCode::SUCCESS
}

/// Resolve the install layout, or print why we can't and return `None`.
fn layout() -> Option<crate::store::Layout> {
    match crate::store::resolve(None) {
        Some(l) => Some(l),
        None => {
            eprintln!("atpkg: HOME is unset — cannot locate the install prefix");
            None
        }
    }
}

/// Whether `verb` MUTATES the store and must therefore hold the store-wide
/// single-writer lock ([`crate::lock`]) for its whole run. The mutators are every
/// verb that stages/activates/discards builds or rewrites shims (`install` — incl.
/// `--default-set` —, `update`, `sync`, `rollback`, `uninstall`, `gc`), every
/// link-mutating verb (`link`, `unlink`, `refresh` — which also covers the
/// `[packages.links]` reconciliation the network verbs run), and `pin`/`unpin`:
/// pins are LOCAL state files, but they gate the coherence-group transaction and
/// their read-modify-write of the pin set is itself check-then-act, so they take
/// the same lock. Everything else (`doctor`, `which`, `list`, `run`, `verify`,
/// `tree-root`, `verify-index`, `relocate`, bare status) reads only — lock-free,
/// and `run` in particular may exec a long-lived tool that must never hold it.
fn verb_mutates_store(verb: &str) -> bool {
    matches!(
        verb,
        "install"
            | "seed"
            | "update"
            | "sync"
            | "rollback"
            | "uninstall"
            | "gc"
            | "link"
            | "unlink"
            | "refresh"
            | "pin"
            | "unpin"
    )
}

/// TRY-acquire the store-wide writer lock for a mutating verb at the dispatch edge.
/// `Ok(None)` when no prefix resolves (HOME unset) — nothing to lock, and the verb
/// itself refuses with its own message moments later. Contention or an unusable
/// lock file is fail-closed: the loud one-line refusal and exit 1.
fn mutator_store_lock() -> Result<Option<crate::lock::StoreLock>, ExitCode> {
    let Some(layout) = crate::store::resolve(None) else {
        return Ok(None);
    };
    match crate::lock::try_lock_store(&layout) {
        Ok(guard) => Ok(Some(guard)),
        Err(e) => {
            eprintln!("atpkg: {e}");
            Err(ExitCode::from(1))
        }
    }
}

/// `atpkg` (no verb) — the inert/enabled posture, observable from the shell.
fn status() {
    if manager_enabled() {
        let anchor = if root_override().is_some() {
            "root key via ATPKG_ROOTKEY_OVERRIDE"
        } else {
            "root key pinned"
        };
        println!(
            "atpkg: enabled ({anchor}). Verbs: doctor, which, list, run, uninstall, \
             install, seed, update, sync, rollback, pin, unpin, gc, verify, link, unlink, refresh."
        );
    } else {
        println!("atpkg: disabled (no root key pinned or overridden) -- inert");
    }
}

/// `atpkg doctor` — the full health surface (§15): trust root, PATH wiring, broken-symlink
/// scan, active-build store integrity, shell.d hooks + fish-safety, disk headroom, index
/// freshness, and `~/.kani`/rustup state. No network, no mutation. Structural breakage exits
/// nonzero; advisory warnings stay exit-0.
fn doctor() -> ExitCode {
    let Some(layout) = layout() else {
        return ExitCode::from(1); // HOME unset is itself structural
    };
    if crate::doctor::run(&layout) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `atpkg which <tool>` — print the store path the tool's shim resolves to.
fn cmd_which(tool: Option<&String>) -> ExitCode {
    let Some(tool) = tool else {
        eprintln!("usage: atpkg which <tool>");
        return ExitCode::from(2);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    match crate::which(&layout, tool) {
        Some(target) => {
            println!("{}", target.display());
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("atpkg: {tool} is not installed");
            ExitCode::from(1)
        }
    }
}

/// `atpkg run <tool> [--] [args…]` — run a pinned, installed tool from the store, replacing
/// this process. Resolves the tool through the store shim (NEVER ambient `$PATH`); appends the
/// managed `bin/` to the child `PATH` (append-not-prepend, so a pinned tool finds its pinned
/// siblings while system `sudo`/`ssh`/… are never shadowed); then `exec`s it. This is the
/// engine behind the `aterm <tool>` dispatcher (docs/ATERM-DISTRIBUTION-WEDGE.md §4).
///
/// An optional literal `--` after the tool name separates it from the tool's own args, so
/// `atpkg run ay -- --version` and `atpkg run ay --version` are equivalent.
fn cmd_run(rest: &[String]) -> ExitCode {
    let Some((tool, args)) = rest.split_first() else {
        eprintln!("usage: atpkg run <tool> [args…]");
        return ExitCode::from(2);
    };
    // Drop a single leading `--` separator if the caller passed one.
    let args: &[String] = match args.split_first() {
        Some((sep, tail)) if sep == "--" => tail,
        _ => args,
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let Some(target) = crate::which(&layout, tool) else {
        // A batteries-included companion mid-build reports PROGRESS, not "not installed" —
        // the eager-early-typing user hits this exactly when the promise matters most.
        if let Some(entry) = companion_installing(&layout, tool) {
            eprintln!(
                "atpkg: {tool} is still installing (attempt {}) — run `aterm pkg status` to watch",
                entry.attempts
            );
            return ExitCode::from(75); // EX_TEMPFAIL — try again shortly
        }
        eprintln!("atpkg: {tool} is not installed (try: atpkg install {tool})");
        return ExitCode::from(127);
    };
    let child_path =
        crate::store::append_bin_to_path(std::env::var_os("PATH").as_deref(), &layout.bin_dir());
    // `platform::exec_or_run` replaces this process on Unix (`execve`, never returns on
    // success) or spawns+waits+exits on Windows (no `execve`); reaching past it means the
    // launch itself failed, and the returned value is that error.
    let mut command = std::process::Command::new(&target);
    command.args(args).env("PATH", child_path);
    let err = crate::platform::exec_or_run(&mut command);
    eprintln!("atpkg: failed to exec {}: {err}", target.display());
    ExitCode::from(127)
}

/// The ledger entry for the companion that exposes `tool`, IF that companion is currently
/// building — so `atpkg run <tool>` (thus `aterm <tool>`) can report progress mid-build.
fn companion_installing(
    layout: &crate::store::Layout,
    tool: &str,
) -> Option<crate::seed::LedgerEntry> {
    let manifest = crate::companions::load().ok()?;
    let comp = manifest
        .companions
        .iter()
        .find(|c| c.expose.iter().any(|e| e == tool))?;
    let ledger = crate::seed::Ledger::read(layout);
    ledger
        .companions
        .get(&comp.name)
        .filter(|e| e.state == "building")
        .cloned()
}

/// `atpkg seed [--force]` — the SOURCE-BUILD (keyless) batteries-included lane: reconcile the
/// compiled-in companions manifest (`docs/COMPANION-TOOLS.md`, starting with `ay`) by building
/// each missing/repinned companion from its pinned public source into the store and shimming
/// it. Complementary to the SIGNED `atpkg install --default-set` bootstrap (§11): a companion
/// already installed by either lane is skipped (idempotent — running both is safe). Source-
/// build is DEFAULT-OFF (opt-in `ATPKG_SOURCE_BUILD=1`) and runs even when the manager is inert
/// (its trust basis is the owner manifest + the pinned commit, not the signed index). Store
/// mutation is serialized by the store-wide lock held at the dispatch edge (`seed` is in
/// `verb_mutates_store`). `--force` ignores the per-commit retry cap.
fn cmd_seed(rest: &[String]) -> ExitCode {
    let force = rest.iter().any(|a| a == "--force" || a == "-f");
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let manifest = match crate::companions::load() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("atpkg: companions manifest invalid: {e}");
            return ExitCode::from(1);
        }
    };
    let mut log = |line: &str| println!("atpkg: {line}");
    let results = crate::seed::reconcile_source(&layout, &manifest, force, &mut log);

    let mut failed = 0u32;
    let mut ready = 0u32;
    for r in &results {
        match r.state.as_str() {
            "ready" | "reused" => {
                ready += 1;
                println!("atpkg: {} {} — {}", r.name, r.state, r.detail);
            }
            "failed" => {
                failed += 1;
                eprintln!("atpkg: {} FAILED — {}", r.name, r.detail);
            }
            _ => println!("atpkg: {} skipped — {}", r.name, r.detail),
        }
    }
    println!(
        "atpkg: seed complete ({ready} ready, {failed} failed, {} companion(s) considered)",
        results.len()
    );
    if failed > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `atpkg relocate <stage-root> [--sign <id>] [--advisory]` — PRODUCER-side
/// pack-time relocation (§10.1). Vendors machine-local shared-library deps into
/// the staged sysroot and rewrites/deletes machine-local load commands so the
/// eventual tarball is self-contained. Run BETWEEN staging and tar so the signed
/// `tree_root` is computed over the RELOCATED payload. Fail-closed on any
/// unresolved machine-local reference unless `--advisory`.
fn cmd_relocate(rest: &[String]) -> ExitCode {
    let mut stage: Option<&str> = None;
    let mut sign_id: Option<&str> = None;
    let mut advisory = false;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--sign" => sign_id = it.next().map(String::as_str),
            "--advisory" => advisory = true,
            s if !s.starts_with('-') && stage.is_none() => stage = Some(s),
            other => {
                eprintln!("atpkg relocate: unexpected argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(stage) = stage else {
        eprintln!("usage: atpkg relocate <stage-root> [--sign <identity>] [--advisory]");
        return ExitCode::from(2);
    };
    let stage_path = std::path::Path::new(stage);
    if !stage_path.is_dir() {
        eprintln!("atpkg relocate: {stage} is not a directory");
        return ExitCode::from(1);
    }
    match crate::relocate::relocate_stage(stage_path, sign_id) {
        Ok(report) => {
            if !report.vendored.is_empty() {
                println!(
                    "atpkg relocate: vendored {} lib(s) into {}, rewrote {} object(s): {}",
                    report.vendored.len(),
                    crate::relocate::VENDOR_REL,
                    report.rewritten,
                    report.vendored.join(", ")
                );
            } else {
                println!("atpkg relocate: no machine-local references found (already portable)");
            }
            if !advisory {
                if let Err(e) = report.require_self_contained() {
                    eprintln!("atpkg relocate: {e}");
                    return ExitCode::from(1);
                }
            } else {
                for u in &report.unresolved {
                    eprintln!("atpkg relocate: advisory: {u}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg relocate: {e}");
            ExitCode::from(1)
        }
    }
}

/// `atpkg list` — the installed `(program, build)` pairs.
fn cmd_list() {
    let Some(layout) = layout() else {
        return;
    };
    let installed = crate::list_installed(&layout);
    if installed.is_empty() {
        println!("atpkg: no programs installed");
        return;
    }
    for (program, build) in installed {
        println!("{program}\t{build}");
    }
}

/// `atpkg uninstall <program>` — remove its shims + store builds (fail-closed inside the prefix).
fn cmd_uninstall(program: Option<&String>) -> ExitCode {
    let Some(program) = program else {
        eprintln!("usage: atpkg uninstall <program>");
        return ExitCode::from(2);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    match crate::uninstall(&layout, program) {
        Ok(()) => {
            println!("atpkg: uninstalled {program}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: uninstall {program} failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// The user's `~/.kani` home, for GC pinned-set discovery. `None` if the home directory
/// can't be resolved. Platform-aware (`%USERPROFILE%` on Windows), not a raw `$HOME` read.
fn kani_home() -> Option<std::path::PathBuf> {
    aterm_types::dirs::home_dir().map(|h| h.join(".kani"))
}

/// `atpkg gc` — reclaim superseded store builds (keep current + one rollback + every
/// `~/.kani`-pinned build). No network.
fn cmd_gc() -> ExitCode {
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let kani = kani_home().unwrap_or_else(|| std::path::PathBuf::from("/nonexistent"));
    let report = crate::gc::run(&layout, &kani);
    if report.reclaimed.is_empty() {
        println!("atpkg gc: nothing to reclaim");
    } else {
        for (p, builds) in &report.reclaimed {
            println!(
                "atpkg gc: reclaimed {p} build(s) {}",
                builds
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    ExitCode::SUCCESS
}

/// `atpkg tree-root <dir>` — print the SHA-256 tree_root of an extracted directory, the
/// value the publish pipeline (§4.2/§12) writes into a per-build manifest's `tree_root`.
/// A producer helper exposing the same hashing the client re-verifies with (§8).
fn cmd_tree_root(dir: Option<&String>) -> ExitCode {
    let Some(dir) = dir else {
        eprintln!("usage: atpkg tree-root <dir>");
        return ExitCode::from(2);
    };
    match crate::tree_root(std::path::Path::new(dir)) {
        Ok(root) => {
            println!("{root}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: tree-root {dir} failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// The target triple of this build, for artifact selection (§4.2).
fn current_triple() -> &'static str {
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        "aarch64-unknown-linux-gnu"
    }
    #[cfg(not(any(
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "linux"),
    )))]
    {
        "unknown"
    }
}

/// Current Unix epoch second (for the freshness gate); 0 if the clock is before the epoch.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// RFC3339 UTC timestamp for the status record. Pure Rust — NO `/bin/date` shell-out, which
/// could never spawn on Windows (no such binary; the absolute path also bypasses PATHEXT), so
/// `updated_at` was left permanently empty there, silently disabling doctor's index-freshness /
/// "publishing looks frozen" warning. Formats the current epoch second via `civil_from_days`
/// (Howard Hinnant), the exact inverse of `flow::rfc3339_to_unix`, so the two round-trip.
fn now_rfc3339() -> String {
    let secs = now_unix();
    if secs <= 0 {
        return String::new();
    }
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (h, mi, se) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // civil_from_days: days since 1970-01-01 → (year, month, day).
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{se:02}Z")
}

/// Record the install outcome to `status.toml` (the silent manager's observability surface,
/// §5/§9). Best-effort — diagnostics, never load-bearing.
/// The signed `tree_root` to persist for `program`: the freshly-applied one when non-empty,
/// otherwise the ALREADY-RECORDED root. An `already_current` install / an untouched
/// coherence-group sibling reports an empty tree_root (nothing was flipped) — persisting that
/// empty value would erase a perfectly good attestation and break `atpkg verify` for an
/// untampered program. So an empty new root means "keep what we had", never "forget it".
fn effective_tree_root(layout: &crate::store::Layout, program: &str, new_root: &str) -> String {
    if !new_root.is_empty() {
        return new_root.to_string();
    }
    crate::status::read(layout)
        .and_then(|s| s.programs.get(program).map(|p| p.tree_root.clone()))
        .unwrap_or_default()
}

fn record_status(
    layout: &crate::store::Layout,
    program: &str,
    state: crate::ProgramStatus,
    outcome: String,
) {
    // Seed the per-program map from the EXISTING record so updating ONE program does
    // not clobber every OTHER program's last-known state. The silent update loop
    // calls this once per program, and status.toml is the per-program observability
    // surface (§5/§9) — a fresh single-entry map would erase the rest each pass.
    let mut programs = crate::status::read(layout)
        .map(|s| s.programs)
        .unwrap_or_default();
    programs.insert(program.to_string(), state);
    let status = crate::Status {
        schema: 1,
        updated_at: now_rfc3339(),
        enabled: manager_enabled(),
        index_source: crate::resolve_account(crate::config::cached().account()).slug(),
        outcome,
        programs,
    };
    let _ = crate::status::write(layout, &status);
}

/// Pure precedence core of [`resolve_pkg_token`]: a non-empty `ATPKG_TOKEN` env
/// value wins outright (the dedicated, atpkg-specific override — unchanged
/// behavior); otherwise the shared `aterm-update-core` chain (`chain`, invoked
/// LAZILY so no keychain/`gh` subprocess runs when the env var suffices)
/// supplies both the token and its source label. No token at all ⇒ the empty
/// anonymous token (fine for public repos, rate-limited). The label names the
/// SOURCE only — never the token itself.
fn pick_pkg_token(
    atpkg_env: Option<String>,
    chain: impl FnOnce() -> Option<(String, String)>,
) -> (String, Option<String>) {
    if let Some(t) = atpkg_env.filter(|t| !t.is_empty()) {
        return (t, Some("$ATPKG_TOKEN".to_string()));
    }
    match chain() {
        Some((t, src)) => (t, Some(src)),
        None => (String::new(), None),
    }
}

/// Resolve the GitHub token for the network verbs: `$ATPKG_TOKEN` first (the
/// dedicated override), then `aterm-update-core`'s full per-machine chain
/// (`$ATERM_UPDATE_TOKEN` → keychain → 0600 file → `$GITHUB_TOKEN` → `$GH_TOKEN`
/// → `gh auth token`) against the shared support dir (the pkg prefix's parent —
/// the same `…/aterm` dir the app updater resolves against, so ONE provisioned
/// credential serves both). Returns `(token, source_label)`; the label feeds the
/// loud `atpkg doctor` line and NEVER carries the token.
pub(crate) fn resolve_pkg_token(layout: &crate::store::Layout) -> (String, Option<String>) {
    pick_pkg_token(std::env::var("ATPKG_TOKEN").ok(), || {
        let support = layout
            .prefix
            .parent()
            .unwrap_or(&layout.prefix)
            .to_path_buf();
        aterm_update_core::token::resolve_with_source(&support).map(|(t, src)| (t, src.to_string()))
    })
}

/// The production GitHub fetcher: the `[packages].account`-aware owner (env
/// `ATPKG_ACCOUNT` still beats config — [`crate::resolve_account`]), the token
/// chain ([`resolve_pkg_token`]), and the `[packages.links]` per-program
/// `owner/repo` fetch overrides ([`crate::repo_overrides`]).
fn github_fetcher(layout: &crate::store::Layout) -> crate::GithubFetcher {
    let cfg = crate::config::cached();
    let owner = crate::resolve_account(cfg.account()).owner;
    // Public program repos need no token; a token is the optional rate-limit / private aid.
    let (token, _source) = resolve_pkg_token(layout);
    crate::GithubFetcher::new(owner, token).with_overrides(crate::repo_overrides(cfg))
}

/// Pick the fetcher for the network verbs: a `dir:<path>` `ATPKG_REGISTRY` selects the
/// offline / publisher-test [`crate::DirFetcher`] (§14); otherwise the production GitHub
/// fetcher. A `dir:` registry's bytes STILL pass the identical verify + floor + freshness
/// gates — the source is not an authenticity input. (Env wins over config: a `dir:`
/// registry serves everything locally, so `[packages.links]` fetch overrides are moot
/// there by construction.)
fn resolve_fetcher(layout: &crate::store::Layout) -> Box<dyn crate::flow::Fetcher> {
    if let Some(spec) = std::env::var_os("ATPKG_REGISTRY") {
        let spec = spec.to_string_lossy();
        if let Some(dir) = spec.strip_prefix("dir:") {
            return Box::new(crate::DirFetcher::new(std::path::PathBuf::from(dir)));
        }
    }
    Box::new(github_fetcher(layout))
}

/// The out-of-band root-key OVERRIDE (§8): a caller/config-supplied Ed25519 root public key
/// (base64) via `ATPKG_ROOTKEY_OVERRIDE`, routed through the SAME `sig::verify_index_with` seam
/// as the compile-time [`crate::PINNED_PKG_ROOTKEY`] — so a mirror, or a second owner account,
/// can be trusted WITHOUT a rebuild. It only swaps WHICH root key anchors verification; it
/// NEVER disables it. The selected index must still carry a valid Ed25519 signature over its
/// exact bytes under this key or nothing verifies (fail-closed at [`crate::select_index`]).
/// `None` when unset/empty — the pinned key stands.
fn root_override() -> Option<String> {
    std::env::var("ATPKG_ROOTKEY_OVERRIDE")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Pure override→pin precedence: the override key when present (non-empty), else the pinned
/// key. Split out from [`effective_root_key`] so the precedence is unit-testable without
/// mutating the process environment. NEVER returns anything that disables verify — an empty
/// resolved key simply means "no anchor", which fails closed at [`manager_enabled`].
fn resolve_root_key(override_key: Option<&str>, pinned: &str) -> String {
    match override_key {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => pinned.to_string(),
    }
}

/// The root public key the network verbs verify under: the out-of-band [`root_override`] when
/// set, else the compile-time [`crate::PINNED_PKG_ROOTKEY`]. Every flow entry point takes THIS
/// in place of a bare pin, so trusting a mirror/second account needs no rebuild.
fn effective_root_key() -> String {
    resolve_root_key(root_override().as_deref(), crate::PINNED_PKG_ROOTKEY)
}

/// Whether the manager may act on the network verbs: SOME root key to verify under (pinned OR
/// overridden) AND the user has not opted out via `ATPKG_DISABLE`.
fn manager_enabled() -> bool {
    crate::manager_enabled()
}

/// Install/force-upgrade one program to its `channel`-pinned build, advancing the durable
/// anti-rollback floor and recording status. Shared by `install` and `update`; `channel`
/// is the config-resolved `[packages].channel` (default `stable`) threaded from the verb
/// edge so this stays testable without process state.
fn do_install(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    channel: &str,
    program: &str,
) -> Result<crate::InstallReport, crate::FlowError> {
    // The ACTIVE build (what the shim points at), not the max COMPLETE build on disk — a
    // staged-but-never-activated build equal to the pin must not make this report
    // up-to-date while the user keeps running the older active build (#19).
    let installed = crate::active_builds(layout).get(program).copied();
    let floor = crate::sig::Floor::new(layout.floor()).current();
    let req = crate::InstallRequest {
        channel,
        program,
        triple: current_triple(),
        installed,
    };
    let result = crate::install(
        fetcher,
        layout,
        &effective_root_key(),
        &req,
        floor,
        now_unix(),
    );
    match &result {
        Ok(r) => {
            // Advance the durable high-water floor to the index we just trusted (§8 gate 3).
            let _ = crate::sig::Floor::new(layout.floor()).check_and_record(r.index_build);
            let outcome = if r.already_current {
                format!("up to date ({program} build {})", r.build)
            } else {
                format!("installed {program} build {}", r.build)
            };
            // Record each pulled-in dependency FIRST (§17), so the main program's outcome
            // remains the final aggregate. Each carries its own SIGNED tree_root for `verify`.
            for dep in &r.dependencies {
                if let crate::flow::DepResult::Installed { build, tree_root } = &dep.result {
                    record_status(
                        layout,
                        &dep.program,
                        crate::ProgramStatus {
                            installed_build: Some(*build),
                            state: "active".into(),
                            tree_root: tree_root.clone(),
                        },
                        format!("pulled in {} (required by {program})", dep.program),
                    );
                }
            }
            record_status(
                layout,
                program,
                crate::ProgramStatus {
                    installed_build: Some(r.build),
                    state: "active".into(),
                    // Preserve the recorded signed root when this was a no-op (already
                    // current): an empty root would wipe the attestation `atpkg verify` needs.
                    tree_root: effective_tree_root(layout, program, &r.tree_root),
                },
                outcome,
            );
            // Reclaim superseded builds after a real activation (best-effort; never fails the
            // install). Runs at the CLI edge — not inside flow.rs — so flow's unit tests stay
            // hermetic w.r.t. the developer's real ~/.kani. Satisfies "GC after every
            // successful activate".
            if !r.already_current
                && let Some(k) = kani_home()
            {
                let _ = crate::gc::run(layout, &k);
            }
            // Refresh the interactive-shell PATH hook (append-not-prepend, §16). At the CLI
            // edge — not inside flow.rs — so flow's synthetic-layout tests never write the
            // real ~/.aterm. Best-effort; writes OUTSIDE the hashed store tree.
            if !r.already_current {
                crate::hooks::refresh(layout);
            }
        }
        Err(e) => {
            // A dev-linked program is a benign HARD-SKIP (§13), not an error state.
            let state = match e {
                crate::FlowError::Linked(_) => "dev-linked (skipped)".to_string(),
                _ => format!("error: {e}"),
            };
            record_status(
                layout,
                program,
                crate::ProgramStatus {
                    installed_build: None,
                    state,
                    tree_root: String::new(),
                },
                format!("install {program}: {e}"),
            );
        }
    }
    result
}

/// `atpkg install <program>` — resolve+verify the signed index from the configured account,
/// then install/force-upgrade the program's channel-pinned build for this triple (download →
/// verify_and_stage → activate → shim). Inert unless a root key is pinned at build time.
/// `atpkg install --default-set` routes to the explicit whole-set bootstrap instead
/// ([`cmd_install_default_set`]).
fn cmd_install(program: Option<&String>) -> ExitCode {
    let Some(program) = program else {
        eprintln!("usage: atpkg install <program> | atpkg install --default-set");
        return ExitCode::from(2);
    };
    if program == "--default-set" {
        return cmd_install_default_set();
    }
    if !manager_enabled() {
        eprintln!("atpkg: disabled (no root key pinned or overridden) — refusing to install");
        return ExitCode::from(1);
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let cfg = crate::config::cached();
    reconcile_links(&layout, cfg);
    match do_install(&layout, &*resolve_fetcher(&layout), cfg.channel(), program) {
        Ok(r) => {
            if r.already_current {
                println!("atpkg: {} already current (build {})", r.program, r.build);
            } else {
                println!(
                    "atpkg: installed {} build {} (shims: {})",
                    r.program,
                    r.build,
                    if r.shimmed.is_empty() {
                        "none".into()
                    } else {
                        r.shimmed.join(", ")
                    }
                );
            }
            for dep in &r.dependencies {
                match &dep.result {
                    crate::flow::DepResult::Installed { build, .. } => {
                        println!(
                            "atpkg:   pulled in {} build {build} (required)",
                            dep.program
                        );
                    }
                    crate::flow::DepResult::Skipped(why) => {
                        eprintln!("atpkg:   skipped required dep {} — {why}", dep.program);
                    }
                    crate::flow::DepResult::AlreadyPresent(_) => {}
                }
            }
            ExitCode::SUCCESS
        }
        Err(crate::FlowError::Linked(p)) => {
            // A dev-linked program is managed from its checkout — a hard skip, not a failure.
            println!(
                "atpkg: {p} is dev-linked; run `atpkg unlink {p}` to install from the registry"
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: install {program} failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// `atpkg rollback <program>` — re-point the program (and its shims + channel `current`) to
/// the highest RETAINED build strictly below current that still passes the floor/yank gate.
/// Consults the SIGNED index (so the gate is authoritative); advances the durable floor to
/// the index it trusted. Warns if the program is in a coherence group (a per-program rollback
/// splits the locked tuple).
fn cmd_rollback(program: Option<&String>) -> ExitCode {
    let Some(program) = program else {
        eprintln!("usage: atpkg rollback <program>");
        return ExitCode::from(2);
    };
    if !manager_enabled() {
        eprintln!("atpkg: disabled (no root key pinned or overridden) — cannot roll back");
        return ExitCode::from(1);
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let floor = crate::sig::Floor::new(layout.floor()).current();
    let fetcher = resolve_fetcher(&layout);
    match crate::flow::rollback(
        &*fetcher,
        &layout,
        &effective_root_key(),
        crate::config::cached().channel(),
        program,
        floor,
        now_unix(),
    ) {
        Ok(r) => {
            // Advance the durable floor to the index we just trusted (§8 gate 3).
            let _ = crate::sig::Floor::new(layout.floor()).check_and_record(r.index_build);
            record_status(
                &layout,
                program,
                crate::ProgramStatus {
                    installed_build: Some(r.to_build),
                    state: "active".into(),
                    // The rolled-back build's signed tree_root is not carried in the rollback
                    // report; `atpkg update`/reinstall re-records it (verify reports it unset).
                    tree_root: String::new(),
                },
                format!("rolled back {program} {} -> {}", r.from_build, r.to_build),
            );
            if let Some(g) = &r.coherence_group {
                eprintln!(
                    "atpkg: warning — {program} is in coherence group '{g}'; rolling back one \
                     member alone splits the locked tuple. Consider `atpkg update` to re-cohere."
                );
            }
            println!(
                "atpkg: rolled back {program} from build {} to {}; `atpkg pin {program}` to hold it there",
                r.from_build, r.to_build
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: rollback {program} failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// `atpkg pin <program>` / `atpkg unpin <program>` — freeze (or release) a program against
/// `update`/`sync`. A pin is purely LOCAL upgrade-suppression state (no index/network); it is
/// consulted strictly AFTER the floor/yank gate, so it can never resurrect a tombstoned or
/// below-floor build.
fn cmd_pin(program: Option<&String>, pinned: bool) -> ExitCode {
    let verb = if pinned { "pin" } else { "unpin" };
    let Some(program) = program else {
        eprintln!("usage: atpkg {verb} <program>");
        return ExitCode::from(2);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    // Pinning something not installed is meaningless.
    if !crate::active_builds(&layout).contains_key(program) {
        eprintln!("atpkg: {program} is not installed — nothing to {verb}");
        return ExitCode::from(1);
    }
    match crate::pin::set_pinned(&layout, program, pinned) {
        Ok(_) => {
            println!(
                "atpkg: {program} {}",
                if pinned {
                    "pinned (held against update/sync)"
                } else {
                    "unpinned"
                }
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: {verb} {program} failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// `atpkg update [program]` — the update router. No arg ⇒ update EVERY installed program
/// ([`cmd_update_all`], the background loop's verb). With a program ⇒ [`cmd_update_one`],
/// which routes a coherence-GROUP member through the transactional path (so a locked tuple
/// can never split, §11) and an ungrouped program through the single-program install path.
fn cmd_update(program: Option<&String>) -> ExitCode {
    match program {
        Some(p) => cmd_update_one(p),
        None => cmd_update_all(),
    }
}

/// `atpkg update` (no arg) — silently upgrade every installed program to its channel pin,
/// applying **coherence groups atomically** (§7): the `rustc`-locked tuple moves all-or-
/// nothing (stage-all → flip-all → rollback-on-failure), while an ungrouped tool applies
/// independently so one failure never wedges the tuple. A local pin holds a group on its
/// current builds (surfaced as a pin-hold). With `[packages].auto_install = true` the pass
/// ALSO bootstraps missing index default-set members (§11 batteries-included,
/// [`install_default_set`]) — the loop verb the GUI runs every 6h finally fills an empty
/// store instead of no-opping forever. GC runs once after the whole apply.
fn cmd_update_all() -> ExitCode {
    if !manager_enabled() {
        eprintln!("atpkg: disabled (no root key pinned or overridden) — nothing to update");
        return ExitCode::from(1);
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let cfg = crate::config::cached();
    // Reconcile `[packages.links]` first, so a config-declared dev-link is in place
    // BEFORE the apply decides what it manages (linked programs hard-skip, §13).
    reconcile_links(&layout, cfg);
    // The ACTIVE build per program (what the shims point at), NOT the max complete build
    // on disk — so `decide` never treats a staged-but-unactivated build as the running one
    // (which would silently skip a needed re-flip, #19).
    let installed: std::collections::BTreeMap<String, u64> = crate::active_builds(&layout);
    if installed.is_empty() && !cfg.auto_install() {
        println!("atpkg: nothing installed to update");
        return ExitCode::SUCCESS;
    }
    let fetcher = resolve_fetcher(&layout);
    let mut failures = 0u32;
    if !installed.is_empty() {
        let floor = crate::sig::Floor::new(layout.floor()).current();
        let report = match crate::apply_channel(
            &*fetcher,
            &layout,
            &effective_root_key(),
            cfg.channel(),
            current_triple(),
            &installed,
            floor,
            now_unix(),
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("atpkg: update failed: {e}");
                record_status(
                    &layout,
                    "*index*",
                    crate::ProgramStatus {
                        installed_build: None,
                        state: format!("error: {e}"),
                        tree_root: String::new(),
                    },
                    format!("update failed: {e}"),
                );
                return ExitCode::from(1);
            }
        };
        // Advance the durable anti-rollback floor to the index we just trusted (§8 gate 3).
        let _ = crate::sig::Floor::new(layout.floor()).check_and_record(report.index_build);
        failures = report_channel_apply(&layout, &installed, &report);
        for p in &report.skipped_linked {
            println!("atpkg: {p} dev-linked — skipped");
        }
    }
    // §11 batteries-included: with explicit config consent, ALSO install the index
    // default-set members not yet installed (include/exclude-narrowed; linked/yanked
    // members skip; per-program failures are loud but never block the rest).
    if cfg.auto_install() {
        failures += install_default_set(&layout, &*fetcher, &effective_root_key(), cfg, now_unix());
    }
    // Reclaim superseded builds once after the whole channel apply (all group activations
    // done). Best-effort; never fails the update.
    if let Some(k) = kani_home() {
        let _ = crate::gc::run(&layout, &k);
    }
    // Refresh the interactive-shell PATH hook at the CLI edge (§16), best-effort.
    crate::hooks::refresh(&layout);
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// §11 batteries-included bootstrap: install every SIGNED-index default-set member
/// (narrowed by the narrowing-only `[packages].include`/`exclude`) that is not yet
/// installed. Shared by `update`'s `auto_install = true` arm and the explicit
/// `atpkg install --default-set` one-shot (what the Settings "Install ALab toolset"
/// button calls). Every gate is the install flow's own (reachability, freshness,
/// floor/yank via `decide`, dev-link hard-skip, sig/sha256/tree_root) — this only
/// picks WHICH programs to ask for. The pass resolves + verifies the index ONCE and
/// partitions the missing members by coherence group (§7): a grouped tuple (the
/// `rustc`-locked trust set) fresh-installs all-or-nothing against that ONE index
/// state via [`crate::flow::bootstrap_group`] — a mid-pass index publish or a
/// per-member failure can never activate a version-split or partial tuple — while
/// an ungrouped member keeps the per-member install path (with its signed
/// `requires` resolution, §17). Per-program/group failures are LOUD but never
/// fatal to the rest; returns the hard-failure count. A missing-triple artifact
/// (member or whole tuple), an app-bundle member (the app updates itself), a
/// dev-link, and a tombstoned pin are all SKIPS, not failures — the loop must not
/// scream every 6h about states that are correct. GC + the shell-hook refresh run
/// once at the caller's CLI edge (the `cmd_update_all` precedent), keeping this
/// hermetically testable.
fn install_default_set(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    root_key: &str,
    cfg: &crate::config::PackagesConfig,
    now: i64,
) -> u32 {
    let floor = crate::sig::Floor::new(layout.floor()).current();
    let index = match crate::resolve_verified_index(fetcher, layout, root_key, floor, now) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("atpkg: default-set bootstrap: cannot resolve the signed index: {e}");
            return 1;
        }
    };
    // Fail-closed diagnostics for `[packages.links]` fetch overrides: a program the
    // signed index does not name is unreachable BY CONSTRUCTION (§5) — the override
    // can never fetch it, so say so plainly instead of silently never acting.
    // (`home: None` is fine — only Repo-shaped values matter here.)
    for (program, value) in &cfg.links {
        if matches!(
            crate::classify_link(value, None),
            crate::LinkTarget::Repo(_)
        ) && !index.is_program(program)
        {
            eprintln!(
                "atpkg: [packages.links] {program} = {value:?} — the signed index does not \
                 name {program}; refusing the fetch override (no unsigned installs from slugs)"
            );
        }
    }
    // The channel, looked up ONCE on the one resolved index — every group decision
    // below reads the SAME signed state, so a mid-pass publish can't split a tuple.
    let Some(ch) = index.channels.iter().find(|c| c.name == cfg.channel()) else {
        let e = crate::FlowError::NoChannel(cfg.channel().to_string());
        eprintln!("atpkg: default-set bootstrap failed: {e}");
        return 1;
    };
    let installed = crate::active_builds(layout);
    let wanted = index.installable(cfg.include(), cfg.exclude());
    let mut failures = 0u32;
    // Every channel-pinned program some group covers (grouped tuple or singleton);
    // wanted members left over are unpinned and fail loudly below.
    let mut pinned_members: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for group in crate::plan_groups(&index, ch) {
        pinned_members.extend(group.members.iter().cloned());
        // The members THIS pass would freshly install: wanted (include/exclude-
        // narrowed) and absent. Installed members are the update apply's job.
        let missing: Vec<String> = group
            .members
            .iter()
            .filter(|m| wanted.contains(m.as_str()) && !installed.contains_key(m.as_str()))
            .cloned()
            .collect();
        if missing.is_empty() {
            continue;
        }
        // Dev-linked HARD-SKIP (§13): a singleton skips itself; a coherence tuple with
        // ANY linked member skips WHOLE (can't partial-install a locked group over a
        // dev link) — the same rule as `apply_channel`.
        if group
            .members
            .iter()
            .any(|m| crate::linkmode::is_linked(layout, m))
        {
            match &group.group {
                Some(g) => {
                    println!("atpkg: coherence group '{g}' has a dev-linked member — skipped whole")
                }
                None => println!("atpkg: {} dev-linked — skipped", group.members[0]),
            }
            continue;
        }
        match &group.group {
            // A coherence tuple fresh-installs ALL-OR-NOTHING against the one index
            // resolved above (§7): stage-all → flip-all → rollback, exactly the
            // update path's transaction, so no failure mode can activate a partial
            // or version-split trust toolchain.
            Some(g) => {
                // If the narrowing include/exclude leaves part of the tuple absent,
                // refuse the WHOLE group — a deliberately partial tuple is exactly
                // the split state the transaction exists to prevent. Loud diagnostic,
                // not a loop failure (config-induced; correct every 6h pass).
                let narrowed_out: Vec<&String> = group
                    .members
                    .iter()
                    .filter(|m| !wanted.contains(m.as_str()) && !installed.contains_key(m.as_str()))
                    .collect();
                if !narrowed_out.is_empty() {
                    eprintln!(
                        "atpkg: [packages] include/exclude leaves {narrowed_out:?} out of \
                         coherence group '{g}' — a locked tuple installs whole; skipping the group"
                    );
                    continue;
                }
                // Missing-triple prescan: the singleton NoArtifact clean-skip doctrine
                // lifted to the tuple — a group that cannot fully exist on this host
                // is a correct state, not a 6h scream.
                if let Some(m) = crate::flow::group_missing_triple(
                    fetcher,
                    &index,
                    cfg.channel(),
                    current_triple(),
                    &missing,
                ) {
                    println!(
                        "atpkg: {m}: no artifact for {} — coherence group '{g}' skipped whole \
                         (§6 clean skip)",
                        current_triple()
                    );
                    continue;
                }
                match crate::flow::bootstrap_group(
                    fetcher,
                    layout,
                    &index,
                    cfg.channel(),
                    current_triple(),
                    &group,
                    &installed,
                ) {
                    Ok((crate::TxnOutcome::Applied(_), applied)) => {
                        // Advance the durable floor to the ONE index the whole group
                        // trusted (§8 gate 3).
                        let _ = crate::sig::Floor::new(layout.floor())
                            .check_and_record(index.index_build);
                        for (m, a) in &applied {
                            record_status(
                                layout,
                                m,
                                crate::ProgramStatus {
                                    installed_build: Some(a.build),
                                    state: "active".into(),
                                    tree_root: effective_tree_root(layout, m, &a.tree_root),
                                },
                                format!(
                                    "bootstrap installed {m} build {} (coherence group '{g}')",
                                    a.build
                                ),
                            );
                            println!(
                                "atpkg: installed {m} build {} (default set, coherence group '{g}')",
                                a.build
                            );
                        }
                    }
                    Ok((crate::TxnOutcome::UpToDate, _)) => {}
                    Ok((crate::TxnOutcome::Pinned(held), _)) => {
                        println!(
                            "atpkg: coherence group '{g}' held by local pin {held:?} — skipped"
                        );
                    }
                    Ok((crate::TxnOutcome::Tombstoned(members), _)) => {
                        eprintln!(
                            "atpkg: coherence group '{g}': pins tombstoned for {members:?} — \
                             nothing installed"
                        );
                    }
                    Ok((
                        crate::TxnOutcome::Aborted {
                            failed,
                            during_flip,
                        },
                        _,
                    )) => {
                        failures += 1;
                        let phase = if during_flip {
                            "flip (already-flipped members rolled back)"
                        } else {
                            "stage (nothing flipped)"
                        };
                        eprintln!(
                            "atpkg: bootstrap of coherence group '{g}' aborted at {failed} \
                             during {phase} — no member left changed (all-or-nothing, continuing)"
                        );
                        record_status(
                            layout,
                            &failed,
                            crate::ProgramStatus {
                                // An already-installed member keeps its honest build; a
                                // fresh member records none.
                                installed_build: installed.get(&failed).copied(),
                                state: format!("error: coherence group '{g}' bootstrap aborted"),
                                tree_root: String::new(),
                            },
                            format!("bootstrap group '{g}' aborted at {failed}"),
                        );
                    }
                    Err(e) => {
                        failures += 1;
                        eprintln!(
                            "atpkg: bootstrap of coherence group '{g}' failed: {e} (continuing)"
                        );
                    }
                }
            }
            // An ungrouped member can move alone (§7) — the per-member install path
            // keeps its signed `requires` dependency resolution (§17). Re-read the
            // durable floor each member so a floor advance recorded earlier in this
            // pass is never undercut by the entry-time snapshot.
            None => {
                let program = &group.members[0];
                let floor = crate::sig::Floor::new(layout.floor()).current();
                let req = crate::InstallRequest {
                    channel: cfg.channel(),
                    program,
                    triple: current_triple(),
                    installed: None,
                };
                match crate::install(fetcher, layout, root_key, &req, floor, now) {
                    Ok(r) => {
                        // Advance the durable floor to the index this install trusted
                        // (§8 gate 3).
                        let _ =
                            crate::sig::Floor::new(layout.floor()).check_and_record(r.index_build);
                        for dep in &r.dependencies {
                            if let crate::flow::DepResult::Installed { build, tree_root } =
                                &dep.result
                            {
                                record_status(
                                    layout,
                                    &dep.program,
                                    crate::ProgramStatus {
                                        installed_build: Some(*build),
                                        state: "active".into(),
                                        tree_root: tree_root.clone(),
                                    },
                                    format!("pulled in {} (required by {program})", dep.program),
                                );
                            }
                        }
                        record_status(
                            layout,
                            program,
                            crate::ProgramStatus {
                                installed_build: Some(r.build),
                                state: "active".into(),
                                tree_root: effective_tree_root(layout, program, &r.tree_root),
                            },
                            format!("bootstrap installed {program} build {}", r.build),
                        );
                        println!("atpkg: installed {program} build {} (default set)", r.build);
                    }
                    // Correct non-failure states — skipped quietly-but-visibly:
                    Err(crate::FlowError::Linked(p)) => {
                        println!("atpkg: {p} dev-linked — skipped");
                    }
                    Err(crate::FlowError::NoArtifact(t)) => {
                        println!("atpkg: {program}: no artifact for {t} — skipped (§6 clean skip)");
                    }
                    Err(crate::FlowError::AppBundleRefused(_)) => {
                        println!(
                            "atpkg: {program}: app-bundle member — managed by the app's own updater, skipped"
                        );
                    }
                    Err(e @ crate::FlowError::Tombstoned(_)) => {
                        eprintln!("atpkg: {program}: {e} — nothing installed");
                    }
                    Err(e) => {
                        failures += 1;
                        eprintln!("atpkg: bootstrap install {program} failed: {e} (continuing)");
                        record_status(
                            layout,
                            program,
                            crate::ProgramStatus {
                                installed_build: None,
                                state: format!("error: {e}"),
                                tree_root: String::new(),
                            },
                            format!("bootstrap install {program}: {e}"),
                        );
                    }
                }
            }
        }
    }
    // Parity with the per-member install path: a wanted member the channel does not
    // PIN fails loudly as NotPinned — silence would hide a half-published index.
    for program in &wanted {
        if pinned_members.contains(program)
            || installed.contains_key(program)
            || crate::linkmode::is_linked(layout, program)
        {
            continue;
        }
        let e = crate::FlowError::NotPinned(program.clone());
        failures += 1;
        eprintln!("atpkg: bootstrap install {program} failed: {e} (continuing)");
        record_status(
            layout,
            program,
            crate::ProgramStatus {
                installed_build: None,
                state: format!("error: {e}"),
                tree_root: String::new(),
            },
            format!("bootstrap install {program}: {e}"),
        );
    }
    failures
}

/// `atpkg install --default-set` — the explicit one-shot §11 bootstrap: install every
/// missing default-set member NOW (the Settings "Install ALab toolset" button's verb;
/// the config-consent-free twin of the `auto_install = true` loop arm — running it IS
/// the consent). Exit 1 iff any member hard-failed; skips are honest and free.
fn cmd_install_default_set() -> ExitCode {
    if !manager_enabled() {
        eprintln!("atpkg: disabled (no root key pinned or overridden) — refusing to install");
        return ExitCode::from(1);
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let cfg = crate::config::cached();
    reconcile_links(&layout, cfg);
    let fetcher = resolve_fetcher(&layout);
    let failures = install_default_set(&layout, &*fetcher, &effective_root_key(), cfg, now_unix());
    // GC + shell-hook refresh once at the CLI edge (the cmd_update_all precedent).
    if let Some(k) = kani_home() {
        let _ = crate::gc::run(&layout, &k);
    }
    crate::hooks::refresh(&layout);
    if failures == 0 {
        println!("atpkg: default set complete");
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Reconcile the `[packages.links]` config at the CLI edge of every network verb:
/// a local-checkout value becomes a MANAGED dev-link ([`crate::linkmode`] —
/// idempotent; registry management hard-skips it until unlinked/removed from
/// config + `atpkg unlink`), while a HAND-MADE link pointing at a DIFFERENT
/// checkout is REFUSED loudly and never touched (a developer's live dev loop is
/// not ours to re-point). A missing checkout is loud. `owner/repo` values are the
/// FETCHER's business ([`github_fetcher`] overrides), not link state. Never fatal:
/// link problems must not block the update pass itself.
///
/// The foreign-link refusal is check-then-act (`linked_checkout` read →
/// `crate::link` overwrite) — sound only because every caller is a store-MUTATING
/// verb (`install`, `install --default-set`, `update`, `sync`) already holding the
/// store-wide single-writer lock ([`crate::lock`]) from the [`main_entry`] dispatch
/// edge, so no second process can slip a link in between the read and the write.
/// Keep it that way: never call this from a lock-free (read-only) verb.
fn reconcile_links(layout: &crate::store::Layout, cfg: &crate::config::PackagesConfig) {
    let home = aterm_types::dirs::home_dir();
    for (program, value) in &cfg.links {
        match crate::classify_link(value, home.as_deref()) {
            crate::LinkTarget::Checkout(want) => {
                // Absolutize exactly as `link` records, so equality below compares
                // like with like (the marker stores an absolute checkout).
                let want = std::path::absolute(&want).unwrap_or(want);
                match crate::linked_checkout(layout, program) {
                    None => {
                        if !want.is_dir() {
                            eprintln!(
                                "atpkg: [packages.links] {program}: checkout {} does not \
                                 exist — not linking",
                                want.display()
                            );
                            continue;
                        }
                        match crate::link(layout, program, &want, &default_link_bins(program)) {
                            Ok(out) => println!(
                                "atpkg: dev-linked {program} from [packages.links] ({})",
                                if out.linked.is_empty() {
                                    "no bins".into()
                                } else {
                                    out.linked.join(", ")
                                }
                            ),
                            Err(e) => {
                                eprintln!("atpkg: [packages.links] {program}: link failed: {e}")
                            }
                        }
                    }
                    Some(existing) if existing == want => {
                        // Already ours at the right target: idempotent re-assert
                        // (picks up newly-built bins), quiet on the happy path.
                        if let Err(e) = crate::refresh(layout, program) {
                            eprintln!("atpkg: [packages.links] {program}: refresh failed: {e}");
                        }
                    }
                    Some(existing) => {
                        eprintln!(
                            "atpkg: [packages.links] {program}: REFUSING to touch the \
                             existing dev-link at {} (config wants {}); run `atpkg unlink \
                             {program}` first if the config target is right",
                            existing.display(),
                            want.display()
                        );
                    }
                }
            }
            crate::LinkTarget::Repo(_) => {} // fetch override — applied in the fetcher
            crate::LinkTarget::Invalid => {
                eprintln!(
                    "atpkg: [packages.links] {program} = {value:?} is neither an absolute/~ \
                     checkout path nor a valid owner/repo — ignored (fail-closed)"
                );
            }
        }
    }
}

/// The conventional default bin for a link with no explicit bin list —
/// `target/release/<program>` WITH the platform executable extension (shared by
/// `cmd_link` and the `[packages.links]` reconciliation so the two can never drift).
fn default_link_bins(program: &str) -> Vec<std::path::PathBuf> {
    vec![std::path::PathBuf::from(format!(
        "target/release/{program}{}",
        std::env::consts::EXE_SUFFIX
    ))]
}

/// `atpkg sync` — the whole-CHANNEL coherence-group update (§7/§11). Identical wiring to
/// `atpkg update` (no arg): [`crate::apply_channel`] over the config-resolved
/// `[packages].channel` (default `stable`) with the SAME
/// fetcher selection ([`resolve_fetcher`], honoring `ATPKG_REGISTRY=dir:`), the SAME durable
/// floor read + record, the SAME [`report_channel_apply`] surfacing, and the SAME
/// GC-after-activate + shell-hook refresh at the CLI edge. Exposed as its own verb so
/// `aterm pkg sync` reads as "make my whole toolchain coherent with the channel"; the
/// out-of-band root override applies transparently (both route through [`effective_root_key`]).
fn cmd_sync() -> ExitCode {
    cmd_update_all()
}

/// `atpkg update <program>` — update ONE program (§11 tuple-split fix). A coherence-GROUP
/// member is routed through the transactional [`crate::flow::apply_program`] so its whole
/// tuple stages/flips/rolls-back atomically and can never move alone; an ungrouped program
/// keeps the single-program install path. A read-only [`crate::flow::plan_update`] picks the
/// path AND yields the authoritative `decide()` result, so the ungrouped local-pin gate is
/// applied strictly AFTER it (never hiding a Tombstone).
fn cmd_update_one(program: &String) -> ExitCode {
    if !manager_enabled() {
        eprintln!("atpkg: disabled — nothing to update");
        return ExitCode::from(1);
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let cfg = crate::config::cached();
    reconcile_links(&layout, cfg);
    let installed = crate::active_builds(&layout);
    if !installed.contains_key(program) {
        // Update of a not-installed program = a fresh explicit install (single path).
        return cmd_install(Some(program));
    }
    let cur = installed.get(program).copied();
    let floor = crate::sig::Floor::new(layout.floor()).current();
    let fetcher = resolve_fetcher(&layout);
    let plan = match crate::flow::plan_update(
        &*fetcher,
        &effective_root_key(),
        cfg.channel(),
        program,
        cur,
        floor,
        now_unix(),
    ) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("atpkg: update {program} failed: {e}");
            return ExitCode::from(1);
        }
    };
    let decision = plan.decision;
    if plan.group.is_some() {
        // Coherence-group member (§11): the transactional path. Pin is consulted inside
        // apply_program's per-group gate.
        let report = match crate::flow::apply_program(
            &*fetcher,
            &layout,
            &effective_root_key(),
            cfg.channel(),
            current_triple(),
            program,
            &installed,
            floor,
            now_unix(),
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("atpkg: update {program} failed: {e}");
                return ExitCode::from(1);
            }
        };
        let _ = crate::sig::Floor::new(layout.floor()).check_and_record(report.index_build);
        let failures = report_channel_apply(&layout, &installed, &report);
        for p in &report.skipped_linked {
            println!("atpkg: {p} dev-linked — skipped");
        }
        crate::hooks::refresh(&layout);
        return if failures == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        };
    }
    // Ungrouped: LOCAL PIN GATE, strictly AFTER the authoritative decide() in `decision`,
    // suppression-only — only an Install (upgrade) is held; Tombstone/UpToDate/NotPinned fall
    // through to the honest single path untouched. CRITICAL: the pin may hold only while the
    // CURRENTLY-active build is itself still gate-valid (`plan.current_build_ok`). If the
    // current build was yanked/floored, decide() returned Install to force-upgrade OFF it —
    // honoring the pin would keep a revoked build running, so the pin is IGNORED there.
    if decision == crate::ApplyDecision::Install
        && plan.current_build_ok
        && let Some(c) = cur
        && crate::pin::is_pinned(&layout, program)
    {
        println!(
            "atpkg: {program} held by local pin (build {c}); `atpkg unpin {program}` to allow updates"
        );
        return ExitCode::SUCCESS;
    }
    if decision == crate::ApplyDecision::Install
        && cur.is_some()
        && crate::pin::is_pinned(&layout, program)
    {
        // Pinned but the current build is no longer gate-valid: force the upgrade and say so.
        eprintln!(
            "atpkg: {program} is pinned, but its current build is yanked/below floor — \
             force-upgrading off it (a pin never keeps a revoked build running)"
        );
    }
    cmd_install(Some(program))
}

/// Report a channel-apply outcome per coherence group and record per-program status,
/// returning the number of ABORTED groups (the caller's exit-code input). Shared by
/// [`cmd_update_all`] and [`cmd_update_one`].
fn report_channel_apply(
    layout: &crate::store::Layout,
    installed: &std::collections::BTreeMap<String, u64>,
    report: &crate::ChannelApplyReport,
) -> u32 {
    // Post-apply ACTIVE builds (shim-derived), for accurate per-program status.
    let post: std::collections::BTreeMap<String, u64> = crate::active_builds(layout);
    let mut failures = 0u32;
    for (group, outcome) in &report.groups {
        let label = group
            .group
            .clone()
            .unwrap_or_else(|| group.members.join("+"));
        match outcome {
            crate::TxnOutcome::UpToDate => println!("atpkg: {label} up to date"),
            crate::TxnOutcome::Applied(members) => {
                println!("atpkg: {label} updated ({})", members.join(", "));
                for prog in &group.members {
                    // Only the members actually flipped carry a fresh signed tree_root; an
                    // untouched (UpToDate) sibling reports none, so preserve its recorded root
                    // rather than wiping the attestation `atpkg verify` needs.
                    let new_root = report
                        .applied
                        .get(prog)
                        .map(|a| a.tree_root.clone())
                        .unwrap_or_default();
                    record_status(
                        layout,
                        prog,
                        crate::ProgramStatus {
                            installed_build: post.get(prog).copied(),
                            state: "active".into(),
                            tree_root: effective_tree_root(layout, prog, &new_root),
                        },
                        format!("group {label}: updated"),
                    );
                }
            }
            crate::TxnOutcome::Tombstoned(members) => {
                eprintln!(
                    "atpkg: {label} NOT updated — pinned build yanked/below floor: {}",
                    members.join(", ")
                );
                for prog in members {
                    record_status(
                        layout,
                        prog,
                        crate::ProgramStatus {
                            installed_build: installed.get(prog).copied(),
                            state: "tombstoned: pin yanked/below floor".into(),
                            tree_root: String::new(),
                        },
                        format!("group {label}: tombstoned"),
                    );
                }
            }
            crate::TxnOutcome::Pinned(members) => {
                println!("atpkg: {label} held by pin ({})", members.join(", "));
                for prog in members {
                    record_status(
                        layout,
                        prog,
                        crate::ProgramStatus {
                            installed_build: installed.get(prog).copied(),
                            state: "pinned: held against update".into(),
                            tree_root: String::new(),
                        },
                        format!("group {label}: held by pin"),
                    );
                }
            }
            crate::TxnOutcome::Aborted {
                failed,
                during_flip,
            } => {
                failures += 1;
                let phase = if *during_flip {
                    "flip (rolled back)"
                } else {
                    "stage"
                };
                eprintln!(
                    "atpkg: {label} update ABORTED at {failed} during {phase} — the group \
                     stays coherent on its previous builds"
                );
                record_status(
                    layout,
                    failed,
                    crate::ProgramStatus {
                        installed_build: post.get(failed).copied(),
                        state: format!("aborted: {phase}"),
                        tree_root: String::new(),
                    },
                    format!("group {label}: aborted at {failed} during {phase}"),
                );
            }
        }
    }
    failures
}

/// `atpkg verify-index <pubkey-b64> <index.toml> <index.toml.sig>` — verify a signed index
/// over its exact bytes with the SAME verifier the client uses (§8). For operators and the
/// publish pipeline's self-check; exit 0 iff the signature is valid under the given root key.
fn cmd_verify_index(
    pubkey: Option<&String>,
    index: Option<&String>,
    sig: Option<&String>,
) -> ExitCode {
    let (Some(pubkey), Some(index), Some(sig)) = (pubkey, index, sig) else {
        eprintln!("usage: atpkg verify-index <pubkey-b64> <index.toml> <index.toml.sig>");
        return ExitCode::from(2);
    };
    let (Ok(raw), Ok(sig_bytes)) = (std::fs::read(index), std::fs::read(sig)) else {
        eprintln!("atpkg: cannot read the index or signature file");
        return ExitCode::from(1);
    };
    match crate::verify_index_with(pubkey, raw, &sig_bytes) {
        Ok(_) => {
            println!("OK: index signature valid under the given root key");
            ExitCode::SUCCESS
        }
        Err(_) => {
            // Opaque on purpose — no verification oracle (§8).
            eprintln!("FAIL: index signature did not verify");
            ExitCode::from(1)
        }
    }
}

/// `atpkg verify [program]` (§12) — re-attest the installed store against the last-trusted
/// SIGNED `tree_root` recorded in `status.toml` at install/update. A drift/integrity audit;
/// compares the recomputed `tree_root` against the signed value, never an unsigned hash. No
/// network. Exit 0 iff every audited program matches.
fn cmd_verify(program: Option<&String>) -> ExitCode {
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let outcomes = match program {
        Some(p) => vec![(p.clone(), crate::verify::verify_program(&layout, p))],
        None => crate::verify::verify_all(&layout),
    };
    if outcomes.is_empty() {
        println!("atpkg: nothing installed to verify");
        return ExitCode::SUCCESS;
    }
    let mut bad = 0u32;
    for (name, o) in &outcomes {
        use crate::verify::VerifyOutcome::{
            BuildMismatch, Drift, Match, NoSignedRoot, NotInstalled, SourceBuilt, Unreadable,
            WiredSysroot,
        };
        match o {
            Match { build } => {
                println!("atpkg: {name} build {build} OK (matches signed tree_root)")
            }
            SourceBuilt { build, commit, intact } => {
                if *intact {
                    println!(
                        "atpkg: {name} build {build} OK (SOURCE-BUILT from {}, lower-assurance: \
                         trust basis = manifest + pinned commit, NOT owner-signed)",
                        commit.get(..12).unwrap_or(commit)
                    );
                } else {
                    bad += 1;
                    eprintln!(
                        "atpkg: {name} build {build} DRIFT — source-built tree no longer matches its \
                         recorded self-tree-root"
                    );
                }
            }
            WiredSysroot { build } => println!(
                "atpkg: {name} build {build} OK (rustup-linked sysroot bundle; verified at install, \
                 not tree-attestable after ~/.kani wiring — use the self-contained bundle for full \
                 attestation)"
            ),
            Drift { build, .. } => {
                bad += 1;
                eprintln!(
                    "atpkg: {name} build {build} DRIFT — store tree does not match the signed tree_root"
                );
            }
            NoSignedRoot { .. } => {
                bad += 1;
                eprintln!(
                    "atpkg: {name} — no signed tree_root recorded; reinstall to enable verification"
                );
            }
            NotInstalled => {
                bad += 1;
                eprintln!("atpkg: {name} is not installed");
            }
            Unreadable { build, error } => {
                bad += 1;
                eprintln!("atpkg: {name} build {build} unreadable: {error}");
            }
            BuildMismatch { active, recorded } => {
                bad += 1;
                eprintln!(
                    "atpkg: {name} active build {active} differs from recorded {recorded:?} — cannot attest; update/reinstall"
                );
            }
        }
    }
    if bad == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `atpkg link <program> <checkout> [rel-bin…]` (§13) — dev-link curated bins from a sibling
/// checkout into `bin/` (all through the sensitive-name deny-list), marking the program so
/// `update`/`apply` HARD-SKIP it until `atpkg unlink`. Default bin when none given:
/// `target/release/<program>`.
fn cmd_link(rest: &[String]) -> ExitCode {
    let (Some(program), Some(checkout)) = (rest.first(), rest.get(1)) else {
        eprintln!("usage: atpkg link <program> <checkout> [rel-bin…]");
        return ExitCode::from(2);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let bins: Vec<std::path::PathBuf> = if rest.len() > 2 {
        rest[2..].iter().map(std::path::PathBuf::from).collect()
    } else {
        // Default to the conventional cargo release bin, WITH the platform executable
        // extension (`target/release/<program>.exe` on Windows) — else `src.is_file()`
        // never matches the real exe and the link silently yields NoBins. Shared with
        // the `[packages.links]` reconciliation.
        default_link_bins(program)
    };
    match crate::link(&layout, program, std::path::Path::new(checkout), &bins) {
        Ok(out) => {
            println!(
                "atpkg: dev-linked {program} ({}); `atpkg unlink {program}` to release it",
                if out.linked.is_empty() {
                    "no bins".into()
                } else {
                    out.linked.join(", ")
                }
            );
            for r in &out.refused {
                eprintln!("atpkg:   refused sensitive bin name {r} (deny-list)");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: link {program} failed: {e}");
            ExitCode::from(2)
        }
    }
}

/// `atpkg unlink <program>` (§13) — remove a program's dev links (leaving any re-installed
/// store shim intact) and its marker. A pin is preserved across the cycle.
fn cmd_unlink(program: Option<&String>) -> ExitCode {
    let Some(program) = program else {
        eprintln!("usage: atpkg unlink <program>");
        return ExitCode::from(2);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    match crate::unlink(&layout, program) {
        Ok(()) => {
            println!("atpkg: unlinked {program} (any pin is preserved)");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: unlink {program} failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// `atpkg refresh [program…]` (§13) — re-assert dev links from their markers (picking up
/// newly-built bins). No arg ⇒ refresh every dev-linked program. Per-name isolation: one
/// failure never blocks the rest.
fn cmd_refresh(rest: &[String]) -> ExitCode {
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let names: Vec<String> = if rest.is_empty() {
        crate::linked_programs(&layout)
    } else {
        rest.to_vec()
    };
    if names.is_empty() {
        println!("atpkg: nothing dev-linked to refresh");
        return ExitCode::SUCCESS;
    }
    let mut failed = 0u32;
    for program in &names {
        match crate::refresh(&layout, program) {
            Ok(out) => println!(
                "atpkg: refreshed {program} ({})",
                if out.linked.is_empty() {
                    "no bins".into()
                } else {
                    out.linked.join(", ")
                }
            ),
            Err(e) => {
                failed += 1;
                eprintln!("atpkg: refresh {program} failed: {e}");
            }
        }
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_layout(label: &str) -> crate::store::Layout {
        let p = std::env::temp_dir().join(format!("atpkg-main-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        crate::store::Layout { prefix: p }
    }

    /// The silent update loop calls `record_status` once PER program; recording one
    /// must not wipe the others' last-known state (regression for the clobber bug).
    #[test]
    fn record_status_merges_programs_rather_than_clobbering() {
        let layout = temp_layout("merge");
        record_status(
            &layout,
            "ay",
            crate::ProgramStatus {
                installed_build: Some(18),
                state: "active".into(),
                tree_root: String::new(),
            },
            "up to date".into(),
        );
        record_status(
            &layout,
            "trust",
            crate::ProgramStatus {
                installed_build: Some(4821),
                state: "active".into(),
                tree_root: String::new(),
            },
            "staged 4821".into(),
        );
        let s = crate::status::read(&layout).expect("status present");
        assert!(
            s.programs.contains_key("ay"),
            "first program preserved across the second write"
        );
        assert!(s.programs.contains_key("trust"), "second program recorded");
        assert_eq!(s.programs["ay"].installed_build, Some(18));
        assert_eq!(s.programs["trust"].installed_build, Some(4821));
        assert_eq!(
            s.outcome, "staged 4821",
            "aggregate outcome reflects the latest pass"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// THE single-writer verb roster ([`crate::lock`]): every verb that mutates the
    /// store (stage/activate/discard, shims, links, pins, gc) takes the store-wide
    /// lock at the dispatch edge; every read-only verb stays lock-free. Exhaustive
    /// over the dispatch table so a new verb must be classified deliberately.
    #[test]
    fn store_lock_verb_roster_is_exact() {
        for mutator in [
            "install",
            "seed",
            "update",
            "sync",
            "rollback",
            "uninstall",
            "gc",
            "link",
            "unlink",
            "refresh",
            "pin",
            "unpin",
        ] {
            assert!(
                verb_mutates_store(mutator),
                "{mutator} mutates the store and must take the lock"
            );
        }
        for read_only in [
            "doctor",
            "which",
            "list",
            "run",
            "verify",
            "tree-root",
            "verify-index",
            "relocate",
        ] {
            assert!(
                !verb_mutates_store(read_only),
                "{read_only} is read-only and must stay lock-free"
            );
        }
    }

    /// Read-only surfaces keep working WHILE a mutator holds the store lock — the
    /// lock serializes writers only, never observation (`list`/`which`/status/verify
    /// must stay live during a long install).
    #[test]
    fn read_only_paths_need_no_store_lock() {
        let layout = temp_layout("readonly-lock");
        // A concurrent mutator: a second Layout over the SAME prefix holds the lock.
        let holder = crate::store::Layout {
            prefix: layout.prefix.clone(),
        };
        let _held = crate::lock::try_lock_store(&holder).expect("mutator takes the lock");
        // Every read-only surface the lock-free verbs are built on completes normally.
        assert!(crate::list_installed(&layout).is_empty());
        assert!(crate::which(&layout, "ay").is_none());
        assert!(crate::status::read(&layout).is_none());
        assert!(crate::verify::verify_all(&layout).is_empty());
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// The out-of-band root-key override (step 22a) SWAPS which root key anchors verification,
    /// never disables it: an override wins over the pin, an empty/absent override falls back to
    /// the pin, and the resolved key is exactly what the flow verifies under.
    #[test]
    fn root_key_override_takes_precedence_over_the_pin() {
        // Override present ⇒ it wins (trust a mirror/second account without a rebuild).
        assert_eq!(
            resolve_root_key(Some("OVERRIDE_KEY"), "PINNED_KEY"),
            "OVERRIDE_KEY"
        );
        // Absent or empty override ⇒ the pin stands (never silently blanks the anchor).
        assert_eq!(resolve_root_key(None, "PINNED_KEY"), "PINNED_KEY");
        assert_eq!(resolve_root_key(Some(""), "PINNED_KEY"), "PINNED_KEY");
        // With no pin at all, an override still supplies the anchor.
        assert_eq!(resolve_root_key(Some("OVERRIDE_KEY"), ""), "OVERRIDE_KEY");
        assert_eq!(
            resolve_root_key(None, ""),
            "",
            "no override, no pin ⇒ no anchor"
        );
    }

    /// Enablement follows the resolved key AND the opt-out. The override can ENABLE the manager
    /// without a rebuild (pin empty), but `ATPKG_DISABLE` still wins, and it can never bypass
    /// verify — enablement only decides whether a verified install is attempted.
    #[test]
    fn override_can_enable_without_a_rebuild_but_never_bypasses_disable() {
        // No pin, no override ⇒ inert.
        assert!(!crate::manager_enabled_with("", None, false));
        // Override with no compile-time pin ⇒ enabled (the without-a-rebuild path).
        assert!(crate::manager_enabled_with("", Some("OVERRIDE_KEY"), false));
        // A pin alone ⇒ enabled.
        assert!(crate::manager_enabled_with("PINNED_KEY", None, false));
        // ATPKG_DISABLE opt-out wins even with a valid key present.
        assert!(!crate::manager_enabled_with(
            "PINNED_KEY",
            Some("OVERRIDE_KEY"),
            true
        ));
        // An empty override never enables on its own.
        assert!(!crate::manager_enabled_with("", Some(""), false));
    }

    // ---- token chain precedence (pure split — no env mutation, no subprocess) ----

    /// `$ATPKG_TOKEN` wins outright and the fallback chain is NOT invoked (laziness:
    /// no keychain/`gh` probe runs when the dedicated env var suffices); an empty
    /// env value falls through; the chain's label passes through; nothing at all is
    /// the anonymous empty token.
    #[test]
    fn token_precedence_atpkg_env_then_chain_then_anonymous() {
        // env wins AND the chain closure is provably never called.
        let (t, src) = pick_pkg_token(Some("ghp_dedicated".into()), || {
            panic!("chain must not run when ATPKG_TOKEN is set")
        });
        assert_eq!(t, "ghp_dedicated");
        assert_eq!(src.as_deref(), Some("$ATPKG_TOKEN"));
        // Empty env value is treated as absent → the chain supplies token + source.
        let (t, src) = pick_pkg_token(Some(String::new()), || {
            Some(("gho_ambient".into(), "gh auth token".into()))
        });
        assert_eq!(t, "gho_ambient");
        assert_eq!(src.as_deref(), Some("gh auth token"));
        // No env → the chain.
        let (t, src) = pick_pkg_token(None, || {
            Some((
                "ghp_keychain".into(),
                "keychain item aterm-update-token".into(),
            ))
        });
        assert_eq!(t, "ghp_keychain");
        assert_eq!(src.as_deref(), Some("keychain item aterm-update-token"));
        // Nothing anywhere → the anonymous empty token, no source to report.
        let (t, src) = pick_pkg_token(None, || None);
        assert!(t.is_empty());
        assert_eq!(src, None);
    }

    // ---- [packages.links] reconciliation + the auto-install bootstrap ----
    // Signed-fixture helpers mirroring flow.rs's (a real USTAR+zstd archive, a
    // root-signed index, release-signed pkg manifests) but laid out as a DIR
    // REGISTRY so the production `DirFetcher` serves them (§14).

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    const ROOT_SEED: [u8; 32] = [7u8; 32];
    const RELEASE_SEED: [u8; 32] = [1u8; 32];

    fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
    }
    fn pk(seed: &[u8; 32]) -> String {
        STANDARD.encode(kp(seed).public_key().as_ref())
    }
    fn sign(seed: &[u8; 32], msg: &[u8]) -> Vec<u8> {
        kp(seed).sign(msg).as_ref().to_vec()
    }

    fn scratch(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-cli-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        d
    }

    /// A raw USTAR + zstd archive shipping `bin/<prog>`.
    fn make_archive(dir: &Path, prog: &str, build: u64) -> PathBuf {
        fn entry(name: &str, content: &[u8]) -> Vec<u8> {
            let mut h = [0u8; 512];
            let nb = name.as_bytes();
            h[..nb.len()].copy_from_slice(nb);
            h[100..108].copy_from_slice(b"0000755\0");
            h[108..116].copy_from_slice(b"0000000\0");
            h[116..124].copy_from_slice(b"0000000\0");
            h[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
            h[136..148].copy_from_slice(b"00000000000\0");
            h[148..156].copy_from_slice(b"        ");
            h[156] = b'0';
            h[257..263].copy_from_slice(b"ustar\0");
            h[263..265].copy_from_slice(b"00");
            let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
            h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            let mut out = h.to_vec();
            out.extend_from_slice(content);
            out.resize(out.len() + (512 - content.len() % 512) % 512, 0);
            out
        }
        let mut tar = Vec::new();
        tar.extend(entry(&format!("bin/{prog}"), b"#!/bin/true\n"));
        tar.resize(tar.len() + 1024, 0);
        let path = dir.join(format!("{prog}-{build}.tar.zst"));
        let f = std::fs::File::create(&path).unwrap();
        let mut enc = zstd::Encoder::new(f, 0).unwrap();
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap();
        path
    }

    /// Write one program's release-signed `pkg-<prog>-<build>.toml` (+ `.sig`) beside its
    /// real archive, with the archive's genuine sha256 + tree_root baked in.
    fn write_pkg(dir: &Path, prog: &str, build: u64) {
        let archive = make_archive(dir, prog, build);
        let sha = crate::tree::file_sha256(&archive).unwrap();
        let probe = dir.join(format!("probe-{prog}"));
        crate::extract::extract_tar_zst(&archive, &probe, 10_000_000, 10_000).unwrap();
        let root = crate::tree::tree_root(&probe).unwrap();
        let _ = std::fs::remove_dir_all(&probe);
        let body = format!(
            "schema = 1\nprogram = \"{prog}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
             exposes = [\"{prog}\"]\n\
             [[artifact]]\ntarget = \"{triple}\"\nkind = \"binary\"\n\
             asset = \"{prog}-{build}.tar.zst\"\nsha256 = \"{sha}\"\ntree_root = \"{root}\"\n\
             size = 100\n[artifact.cost]\ndisk_installed = 1048576\n",
            triple = current_triple()
        );
        let name = dir.join(format!("pkg-{prog}-{build}.toml"));
        std::fs::write(&name, body.as_bytes()).unwrap();
        std::fs::write(
            dir.join(format!("pkg-{prog}-{build}.toml.sig")),
            sign(&RELEASE_SEED, body.as_bytes()),
        )
        .unwrap();
    }

    /// Write the root-signed index for a 4-program default set on `channel`:
    /// `ay` (installable), `ny` (a second installable, for exclude), `zz`
    /// (pin YANKED — must never install), `lk` (installable, dev-linked in
    /// the tests) — plus real signed pkgs/archives for the non-yanked members.
    fn write_registry(dir: &Path, channel: &str) {
        let index_body = format!(
            "schema = 1\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{rk}\"\n\
             [programs.ay]\nrepo = \"ay\"\n[programs.ny]\nrepo = \"ny\"\n\
             [programs.zz]\nrepo = \"zz\"\n[programs.lk]\nrepo = \"lk\"\n\
             [[channels]]\nname = \"{channel}\"\nchannel_build = 1\nmin_build = 0\n\
             yanked = [\"zz@5\"]\npin = {{ ay = 18, ny = 7, zz = 5, lk = 3 }}\n",
            rk = pk(&RELEASE_SEED)
        );
        std::fs::write(dir.join("index.toml"), index_body.as_bytes()).unwrap();
        std::fs::write(
            dir.join("index.toml.sig"),
            sign(&ROOT_SEED, index_body.as_bytes()),
        )
        .unwrap();
        write_pkg(dir, "ay", 18);
        write_pkg(dir, "ny", 7);
        write_pkg(dir, "lk", 3);
    }

    /// A fake checkout with a built `target/release/<prog>` bin.
    fn checkout(label: &str, prog: &str) -> PathBuf {
        let d = scratch(&format!("co-{label}"));
        std::fs::create_dir_all(d.join("target/release")).unwrap();
        std::fs::write(d.join("target/release").join(prog), b"#!/bin/true\n").unwrap();
        d
    }

    // THE auto-install decision matrix (§11), against the production DirFetcher over a
    // real signed dir registry: a missing default-set member INSTALLS; an `exclude`d
    // member is narrowed out; a dev-linked member hard-skips; a yanked pin never
    // installs; an already-installed member is left to the update pass. None of the
    // skips count as failures (the 6h loop must not scream about correct states).
    #[test]
    fn default_set_bootstrap_installs_missing_and_skips_excluded_linked_yanked() {
        let dir = scratch("bootstrap");
        write_registry(&dir, "stable");
        let layout = temp_layout("bootstrap");
        // Dev-link `lk` (the hard-skip member).
        let co = checkout("bootstrap", "lk");
        crate::link(&layout, "lk", &co, &[PathBuf::from("target/release/lk")]).unwrap();
        let cfg = crate::config::PackagesConfig {
            exclude: Some(vec!["ny".into()]),
            ..Default::default()
        };
        let fetcher = crate::DirFetcher::new(dir.clone());
        let failures = install_default_set(&layout, &fetcher, &pk(&ROOT_SEED), &cfg, 0);
        assert_eq!(failures, 0, "skips are never failures");
        let active = crate::active_builds(&layout);
        assert_eq!(
            active.get("ay").copied(),
            Some(18),
            "missing member installed"
        );
        assert!(!active.contains_key("ny"), "excluded member narrowed out");
        assert!(!active.contains_key("zz"), "yanked pin never installs");
        assert!(!active.contains_key("lk"), "dev-linked member hard-skipped");
        assert!(
            crate::is_linked(&layout, "lk"),
            "the dev link survives untouched"
        );
        // Idempotence: a second pass finds ay installed and re-installs nothing.
        let failures = install_default_set(&layout, &fetcher, &pk(&ROOT_SEED), &cfg, 0);
        assert_eq!(failures, 0);
        assert_eq!(crate::active_builds(&layout).get("ay").copied(), Some(18));
        // The durable floor advanced to the trusted index (§8 gate 3).
        assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 41);
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&co);
    }

    // Channel THREADING: the bootstrap resolves the CONFIG channel, not a hardcoded
    // "stable" — a registry publishing only `nightly` installs iff the config says so.
    #[test]
    fn default_set_bootstrap_uses_the_config_channel() {
        let dir = scratch("channel");
        write_registry(&dir, "nightly");
        let fetcher = crate::DirFetcher::new(dir.clone());
        // Default config (channel "stable") against a nightly-only index: the missing
        // channel is one loud pass-level failure, nothing installs.
        let layout = temp_layout("channel-default");
        let cfg = crate::config::PackagesConfig::default();
        let failures = install_default_set(&layout, &fetcher, &pk(&ROOT_SEED), &cfg, 0);
        assert!(failures > 0, "a missing channel is loud, not silent");
        assert!(crate::active_builds(&layout).is_empty());
        let _ = std::fs::remove_dir_all(&layout.prefix);
        // channel = "nightly" threads through and installs.
        let layout = temp_layout("channel-nightly");
        let cfg = crate::config::parse_packages("[packages]\nchannel = \"nightly\"\n");
        let failures = install_default_set(&layout, &fetcher, &pk(&ROOT_SEED), &cfg, 0);
        assert_eq!(failures, 0);
        assert_eq!(
            crate::active_builds(&layout).get("ay").copied(),
            Some(18),
            "the config channel reached the install request"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Write a root-signed index whose default set is the `rustc`-locked coherence
    /// tuple `ta`+`tb` plus the ungrouped singleton `ay` — the §7 bootstrap fixture.
    /// `write_tb_pkg = false` leaves tb's pkg manifest unpublished, so its stage
    /// fails and the group transaction must abort WHOLE (never a split tuple).
    fn write_group_registry(dir: &Path, channel: &str, write_tb_pkg: bool) {
        let index_body = format!(
            "schema = 1\nindex_build = 51\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{rk}\"\n\
             [programs.ay]\nrepo = \"ay\"\n\
             [programs.ta]\nrepo = \"ta\"\ncoherence_group = \"rustc\"\n\
             [programs.tb]\nrepo = \"tb\"\ncoherence_group = \"rustc\"\n\
             [[channels]]\nname = \"{channel}\"\nchannel_build = 1\nmin_build = 0\n\
             pin = {{ ay = 18, ta = 4, tb = 6 }}\n",
            rk = pk(&RELEASE_SEED)
        );
        std::fs::write(dir.join("index.toml"), index_body.as_bytes()).unwrap();
        std::fs::write(
            dir.join("index.toml.sig"),
            sign(&ROOT_SEED, index_body.as_bytes()),
        )
        .unwrap();
        write_pkg(dir, "ay", 18);
        write_pkg(dir, "ta", 4);
        if write_tb_pkg {
            write_pkg(dir, "tb", 6);
        }
    }

    // §7 bootstrap happy path: a missing coherence tuple fresh-installs WHOLE through
    // the group transaction (one resolved index for the whole pass), alongside the
    // ungrouped singleton; the durable floor advances to the one trusted index.
    #[test]
    fn default_set_bootstrap_installs_a_coherence_group_atomically() {
        let dir = scratch("group-ok");
        write_group_registry(&dir, "stable", true);
        let layout = temp_layout("group-ok");
        let cfg = crate::config::PackagesConfig::default();
        let fetcher = crate::DirFetcher::new(dir.clone());
        let failures = install_default_set(&layout, &fetcher, &pk(&ROOT_SEED), &cfg, 0);
        assert_eq!(failures, 0);
        let active = crate::active_builds(&layout);
        assert_eq!(active.get("ta").copied(), Some(4), "tuple member installed");
        assert_eq!(active.get("tb").copied(), Some(6), "tuple member installed");
        assert_eq!(active.get("ay").copied(), Some(18), "singleton installed");
        assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 51);
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // §7 all-or-nothing: a tuple member whose pkg manifest is unpublished aborts the
    // WHOLE group — the sibling that staged fine is neither activated nor left on
    // disk (a complete-but-inactive build would mis-read as active next run), while
    // the ungrouped singleton still installs. Exactly ONE loud failure for the group.
    #[test]
    fn default_set_bootstrap_never_splits_a_coherence_group() {
        let dir = scratch("group-abort");
        write_group_registry(&dir, "stable", false);
        let layout = temp_layout("group-abort");
        let cfg = crate::config::PackagesConfig::default();
        let fetcher = crate::DirFetcher::new(dir.clone());
        let failures = install_default_set(&layout, &fetcher, &pk(&ROOT_SEED), &cfg, 0);
        assert_eq!(failures, 1, "one failure for the aborted group");
        let active = crate::active_builds(&layout);
        assert!(
            !active.contains_key("ta"),
            "no partial tuple: ta not active"
        );
        assert!(
            !active.contains_key("tb"),
            "no partial tuple: tb not active"
        );
        assert!(
            !layout.build_dir("ta", 4).exists(),
            "the staged sibling was discarded on abort"
        );
        assert_eq!(
            active.get("ay").copied(),
            Some(18),
            "the singleton still installs"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // §7 narrowing: an `exclude` that leaves part of a coherence tuple out refuses
    // the WHOLE group (loud diagnostic, not a failure) — a deliberately partial
    // tuple is exactly the split the transaction exists to prevent.
    #[test]
    fn default_set_bootstrap_refuses_a_narrowed_partial_tuple() {
        let dir = scratch("group-narrowed");
        write_group_registry(&dir, "stable", true);
        let layout = temp_layout("group-narrowed");
        let cfg = crate::config::PackagesConfig {
            exclude: Some(vec!["tb".into()]),
            ..Default::default()
        };
        let fetcher = crate::DirFetcher::new(dir.clone());
        let failures = install_default_set(&layout, &fetcher, &pk(&ROOT_SEED), &cfg, 0);
        assert_eq!(
            failures, 0,
            "a config-narrowed tuple is a diagnostic, not a failure"
        );
        let active = crate::active_builds(&layout);
        assert!(
            !active.contains_key("ta"),
            "no partial tuple over a narrowing"
        );
        assert!(!active.contains_key("tb"));
        assert_eq!(
            active.get("ay").copied(),
            Some(18),
            "the singleton still installs"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A `[packages.links]` owner/repo override for a program the signed index does NOT
    // name can never install anything (reachability §5 is structural); the bootstrap
    // only says so loudly. Fail-closed: no unsigned installs from slugs.
    #[test]
    fn unindexed_repo_override_never_installs() {
        let dir = scratch("unindexed");
        write_registry(&dir, "stable");
        let layout = temp_layout("unindexed");
        let cfg =
            crate::config::parse_packages("[packages.links]\nghost = \"alabsystems/ghost\"\n");
        let fetcher = crate::DirFetcher::new(dir.clone());
        let failures = install_default_set(&layout, &fetcher, &pk(&ROOT_SEED), &cfg, 0);
        assert_eq!(
            failures, 0,
            "the refusal is a loud diagnostic, not a loop failure"
        );
        assert!(
            !crate::active_builds(&layout).contains_key("ghost"),
            "an unindexed program is unreachable by construction"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // [packages.links] reconciliation: a config checkout path becomes a managed
    // dev-link (idempotent across passes); a missing checkout links nothing; an
    // invalid value acts on nothing.
    #[test]
    fn reconcile_links_creates_and_reasserts_config_links() {
        let layout = temp_layout("reconcile");
        let co = checkout("reconcile", "ay");
        let cfg = crate::config::parse_packages(&format!(
            "[packages.links]\nay = {:?}\nmissing = \"/nonexistent/checkout\"\nbad = \"a b\"\n",
            co.display().to_string()
        ));
        reconcile_links(&layout, &cfg);
        assert!(
            crate::is_linked(&layout, "ay"),
            "config path became a dev-link"
        );
        assert_eq!(
            crate::linked_checkout(&layout, "ay").unwrap(),
            std::path::absolute(&co).unwrap()
        );
        assert!(
            !crate::is_linked(&layout, "missing"),
            "missing checkout links nothing"
        );
        assert!(
            !crate::is_linked(&layout, "bad"),
            "invalid value acts on nothing"
        );
        // Idempotent second pass: still linked, same checkout, and a newly-built bin
        // is picked up by the quiet refresh.
        std::fs::write(co.join("target/release").join("ay2"), b"#!/bin/true\n").unwrap();
        let cfg2 = crate::config::parse_packages(&format!(
            "[packages.links]\nay = {:?}\n",
            co.display().to_string()
        ));
        reconcile_links(&layout, &cfg2);
        assert!(crate::is_linked(&layout, "ay"));
        assert_eq!(
            crate::linked_checkout(&layout, "ay").unwrap(),
            std::path::absolute(&co).unwrap()
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&co);
    }

    // The FOREIGN-link refusal: a hand-made dev-link pointing at a DIFFERENT checkout
    // is never re-pointed by config — the link (and its shim target) survive untouched.
    #[test]
    fn reconcile_links_refuses_to_touch_a_foreign_link() {
        let layout = temp_layout("foreign");
        let hand = checkout("foreign-hand", "ay");
        let config_wants = checkout("foreign-cfg", "ay");
        crate::link(&layout, "ay", &hand, &[PathBuf::from("target/release/ay")]).unwrap();
        let cfg = crate::config::parse_packages(&format!(
            "[packages.links]\nay = {:?}\n",
            config_wants.display().to_string()
        ));
        reconcile_links(&layout, &cfg);
        assert_eq!(
            crate::linked_checkout(&layout, "ay").unwrap(),
            std::path::absolute(&hand).unwrap(),
            "the hand-made link's checkout is untouched"
        );
        assert_eq!(
            crate::platform::resolve_shim(&layout.shim("ay")).unwrap(),
            std::path::absolute(&hand)
                .unwrap()
                .join("target/release/ay"),
            "the shim still points into the hand-made checkout"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&hand);
        let _ = std::fs::remove_dir_all(&config_wants);
    }
}
