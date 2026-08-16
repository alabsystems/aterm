// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg` — the aterm toolchain package manager CLI.
//!
//! The local read/maintenance verbs (`which`/`list`/`uninstall`) and `doctor` are wired
//! here over the tested library (`crate::ops`, `crate::store`); the network-driven verbs
//! (`install`/`update`/`rollback`, plus the `install --default-set` bootstrap)
//! compose the same tested primitives with the GitHub/dir fetch. Every network verb reads
//! the `[packages]` table of the SAME `aterm.toml` the GUI owns ([`crate::config`]) —
//! account/channel/include/exclude/links — with env always winning over config. The
//! verification anchor is the COMMITTED paper-master keyset [`crate::PKG_TRUST_ANCHORS`]
//! (`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`) — the SAME root the app updater uses
//! — and nothing else: no env var or build-time
//! variable can supply or swap it (see [`effective_anchor`]); a tree whose master keyset
//! is empty — which is this tree — builds a manager that is INERT and says so.

use std::process::ExitCode;

use crate::flow::now_unix;

/// Every verb the dispatch below accepts, in the order the unknown-verb hint
/// prints them.
///
/// Kept as DATA, and checked against the `match` by
/// [`tests::verb_hint_matches_dispatch`], because the hint has drifted from
/// reality twice: `sync` (deleted in `a43193f4`) and `seed` (deleted in
/// `ba832933`) were both still advertised here long after they stopped
/// dispatching — so a user who followed the suggestion got the very error
/// that printed it. A hand-maintained help string cannot be trusted to track
/// a match arm; a test can.
const VERBS: &[&str] = &[
    "doctor",
    "which",
    "list",
    "uninstall",
    "tree-root",
    "verify-index",
    "verify-pkg",
    "install",
    "update",
    "rollback",
    "pin",
    "unpin",
    "gc",
    "verify",
    "link",
    "unlink",
    "refresh",
    "run",
    "relocate",
];

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
    // internal verb re-routing (`update <p>` → install) can
    // never double-acquire — and holds it for the whole verb. Contention is a loud
    // exit-1 refusal naming the lock path (the GUI Packages page surfaces the child
    // stderr; the 6-hour loop just retries next pass). Read-only verbs skip this
    // entirely and never need the lock.
    // The conventional help spellings land on the help surface with exit 0, not the
    // unknown-verb error path — `atpkg` rides PATH as an argv0 alias, so `--help` is
    // the first thing a shell (or an AI) tries. Handled BEFORE the verb match so the
    // dispatch-coherence test's arm extraction sees only real verbs.
    if matches!(verb, Some("help" | "-h" | "--help")) {
        return cmd_help();
    }
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
        Some("verify-index") => return cmd_verify_index(args.get(1..).unwrap_or(&[])),
        Some("verify-pkg") => return cmd_verify_pkg(args.get(1..).unwrap_or(&[])),
        Some("install") => return cmd_install(args.get(1)),
        Some("update") => return cmd_update(args.get(1)),
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
            eprintln!("atpkg: unknown verb '{other}' (try: {})", VERBS.join(", "));
            return ExitCode::from(2);
        }
        None => status(),
    }
    ExitCode::SUCCESS
}

/// `atpkg help` / `-h` / `--help` — the conventional help surface: usage plus the ONE
/// advertised verb roster ([`VERBS`]), exit 0. The full manual is `aterm help pkg`;
/// this stays a summary so the two never fork.
fn cmd_help() -> ExitCode {
    println!("atpkg — the aterm toolchain package manager");
    println!("usage: atpkg <verb> [args…]");
    println!("verbs: {}", VERBS.join(", "));
    println!("full manual: aterm help pkg");
    ExitCode::SUCCESS
}

/// The chain-validated `[packages].prefix` override, or `None` for the default.
/// Threaded through `store::resolve` so the override is REACHABLE: both call sites
/// previously passed a hardcoded `None`, which made `vet_prefix`'s whole
/// configured-prefix branch dead code and pinned every install to `$HOME` — and
/// therefore to the unverified lane, since a user-owned toolchain path cannot carry
/// pathname execution authority.
fn configured_prefix() -> Option<std::path::PathBuf> {
    crate::config::load().prefix_path(aterm_types::dirs::home_dir().as_deref())
}

/// Resolve the install layout, or print why we can't and return `None`.
fn layout() -> Option<crate::store::Layout> {
    match crate::store::resolve(configured_prefix().as_deref()) {
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
/// `--default-set` —, `update`, `rollback`, `uninstall`, `gc`), every
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
            | "update"
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
    let Some(layout) = crate::store::resolve(configured_prefix().as_deref()) else {
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
        let anchor = "root key pinned";
        println!("atpkg: enabled ({anchor}). Verbs: {}.", VERBS.join(", "));
    } else {
        println!("atpkg: disabled (no root key pinned) -- inert");
    }
}

/// `atpkg doctor` — the full health surface (§15): trust root, PATH wiring, broken-symlink
/// scan, active-build store integrity, shell.d hooks + fish-safety, disk headroom, index
/// freshness, and rustup state. No network, no mutation. Structural breakage exits
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

/// Name the programs a GC pass ABSTAINED on, or print nothing when it abstained on none.
///
/// Abstention must not read as "there was nothing to do". A program whose two views disagree
/// is skipped and its superseded builds stay on disk *forever* — a pass that skipped every
/// program otherwise reports the reassuring "nothing to reclaim", with no hint that `doctor`
/// knows exactly why. Naming them is the difference between a silent unbounded-growth bug and
/// a diagnosable one, which is why it is a shared helper rather than a line inside `cmd_gc`:
/// every verb that runs a GC pass owes the same disclosure.
fn print_gc_abstentions(verb: &str, report: &crate::gc::GcReport) {
    if report.diverged.is_empty() {
        return;
    }
    let names: Vec<&str> = report.diverged.iter().map(|d| d.program.as_str()).collect();
    println!(
        "atpkg {verb}: skipped {} program(s) with no proven live build ({}) — \
         run `atpkg doctor` for the reason",
        names.len(),
        names.join(", ")
    );
}

/// `atpkg gc` — reclaim superseded store builds (per program: keep the live build plus one
/// rollback target, discard the rest), and sweep what an interrupted install left behind. No
/// network.
///
/// The three outcomes are printed on separate lines because they are different claims about
/// where the disk went: a RECLAIM retired a build that really was installed; a SWEEP removed a
/// tree (or its stage scratch) that never finished installing and that, until it was swept, no
/// verb in the manager could even name. Folding the sweep into "reclaimed" would report a leak
/// as routine housekeeping — and the leak being unreportable is the whole reason it grew.
fn cmd_gc() -> ExitCode {
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let report = crate::gc::run(&layout);
    if report.reclaimed.is_empty()
        && report.swept_partial.is_empty()
        && report.swept_scratch.is_empty()
    {
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
    print_gc_sweeps("gc", &report);
    print_gc_abstentions("gc", &report);
    ExitCode::SUCCESS
}

/// Disclose what a GC pass SWEPT — the trees and stage scratch an interrupted install left
/// behind.
///
/// A shared helper for the same reason [`print_gc_abstentions`] is one: `gc::run` is not
/// only reached through `atpkg gc`. It also runs after an install, after an update, and
/// after a grouped update, and those three passes delete exactly as much as the explicit
/// verb does. A sweep that prints only under `atpkg gc` means the common case — the pass
/// that happens automatically — removes gigabytes and says nothing, which is the same
/// silence that let the leak grow in the first place.
fn print_gc_sweeps(verb: &str, report: &crate::gc::GcReport) {
    for (p, builds) in &report.swept_partial {
        println!(
            "atpkg {verb}: swept interrupted {p} install(s), build(s) {} — never installed, \
             so until now nothing in the manager could name them",
            builds
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for (p, names) in &report.swept_scratch {
        println!("atpkg {verb}: swept {p} stage scratch {}", names.join(", "));
    }
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
    // Windows arms: without these a Windows build reports "unknown" and can
    // never select ANY artifact — the one-binary Windows packaging lane
    // (apps/aterm-win) ships a client that clean-skips every install.
    #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(all(target_arch = "aarch64", target_os = "windows"))]
    {
        "aarch64-pc-windows-msvc"
    }
    #[cfg(not(any(
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "linux"),
        all(target_arch = "x86_64", target_os = "windows"),
        all(target_arch = "aarch64", target_os = "windows"),
    )))]
    {
        "unknown"
    }
}

/// RFC3339 UTC timestamp for the status record. Pure Rust — NO `/bin/date` shell-out, which
/// could never spawn on Windows (no such binary; the absolute path also bypasses PATHEXT), so
/// `updated_at` was left permanently empty there, silently disabling doctor's index-freshness /
/// "publishing looks frozen" warning. Formats the current epoch second via the shared
/// `aterm_types::rfc3339` civil-calendar math, the exact inverse of `flow::rfc3339_to_unix`,
/// so the two round-trip. Empty string on a pre-epoch (or exactly-epoch) clock.
fn now_rfc3339() -> String {
    let secs = now_unix();
    // Both ends are "the clock is unusable", and both must yield an EMPTY stamp rather than
    // a formatted lie. `now_unix` returns `i64::MAX` when the clock cannot be read at all —
    // the fail-CLOSED sentinel the freshness gates need — and formatting that would stamp
    // `status.toml` with a year in the hundreds of billions, which doctor's
    // "publishing looks frozen" check would then read as a perfectly fresh record.
    if secs <= 0 || secs == i64::MAX {
        return String::new();
    }
    // secs > 0, so the cast is lossless.
    aterm_types::rfc3339::format_rfc3339(secs as u64)
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

/// Whether a fetch destination engages the FULL credential chain in
/// `aterm-update-core`'s token walk. Spelled with the SAME constants as
/// `token.rs::needs_ambient_credential` (the crate-private gate inside
/// `resolve_with_source`) so the pick below can never disagree with what the
/// walk will actually do: only the compiled-in public channel slug is anonymous
/// by construction; anything else may be private and runs every rung.
fn engages_credential_chain(owner: &str, repo: &str) -> bool {
    owner != aterm_update_core::DEFAULT_OWNER || repo != aterm_update_core::DEFAULT_REPO
}

/// The ONE `owner/repo` the credential chain is keyed to, chosen over EVERY
/// destination this process's fetcher will actually reach — the resolved index
/// slug plus every `[packages.links]` fetch-override slug — not just the index.
///
/// `GithubFetcher` presents a single token to all of its destinations
/// (`net.rs`: an override is "a possibly-private repo, reached with the same
/// token"), so keying the chain to the index alone starved the links lane: on
/// the compiled default account the index slug IS the public channel, the
/// chain's gate short-circuits to the `$ATERM_UPDATE_TOKEN` rung only, and a
/// `[packages.links] prog = "someorg/private-repo"` fetch went out anonymous —
/// 404 on a machine whose keychain/0600-file/`gh auth` credential worked the
/// round before (adversarial review 2026-08-11). Picking the FIRST
/// chain-engaging destination is equivalent to resolving per fetch: the walk's
/// rung order does not vary by slug once the gate opens, so one chain-engaging
/// slug yields the same token any of them would.
///
/// Order: the index slug when it engages the chain (a repointed account governs
/// every non-overridden fetch), else the first chain-engaging override slug
/// (BTreeMap order — deterministic), else the index slug so the compiled
/// public default stays anonymous by construction.
fn credential_destination(
    index_owner: &str,
    index_repo: &str,
    overrides: &std::collections::BTreeMap<String, String>,
) -> (String, String) {
    if !engages_credential_chain(index_owner, index_repo) {
        for slug in overrides.values() {
            if let Some((o, r)) = slug.split_once('/')
                && engages_credential_chain(o, r)
            {
                return (o.to_string(), r.to_string());
            }
        }
    }
    (index_owner.to_string(), index_repo.to_string())
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
        // The credential chain is keyed to where the fetches actually go — ALL
        // of them, via [`credential_destination`]: the resolved index slug PLUS
        // the `[packages.links]` fetch overrides, because the fetcher presents
        // this one token to every destination. On the compiled default with no
        // overrides the chain's own gate keeps the lookup anonymous by
        // construction (no ambient PAT is gathered for the public channel); a
        // repointed account (`[packages].account`/`ATPKG_ACCOUNT`) OR any
        // links override to a non-default repo consults the full chain, so a
        // private per-program override still reaches the keychain / 0600-file /
        // ambient rungs.
        let cfg = crate::config::cached();
        let index = crate::resolve_account(cfg.account());
        let (owner, repo) =
            credential_destination(&index.owner, &index.repo, &crate::repo_overrides(cfg));
        aterm_update_core::token::resolve_with_source(&support, &owner, &repo)
            .map(|(t, src)| (t, src.to_string()))
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
    let github = Box::new(github_fetcher(layout));
    // An app bundle sealing a signed seed registry (§9.1) joins the flow as a
    // FALLBACK leg — but ONLY as a BOOTSTRAP source, never an update source.
    //
    // The seal is a snapshot of the channel at CUT time; a machine that has
    // already trusted a published index holds a durable floor at or above it,
    // and `select_index` admits any signature-valid index `>= floor`. Chaining
    // the seed unconditionally therefore had two teeth (found by adversarial
    // review 2026-07-30, both traced end-to-end):
    //   * DOWNGRADE — the seed and a later publish routinely share an
    //     `index_build` (the refresh script reads the counter that only a
    //     successful UPLOAD bumps) while pinning different builds, so an
    //     unreachable network let the sealed pins re-install OVER newer
    //     installed builds on the GUI's unattended 6h pass (`gate::decide`
    //     force-installs on any `pinned != installed`, including lower).
    //   * CACHE MASKING — a seed-leg success turned a network failure into
    //     `Ok`, so `flow::resolve_candidates`' §14 last-good-index cache
    //     fallback (its `Err` arm) could never fire, and the seed-only set
    //     then OVERWROTE that cache under the chain's source id.
    // The signed network registry is the only source. A co-located bundled seed
    // used to be chained in ahead of it for a first run on an empty store; that
    // offline lane is gone, so there is one way to get bytes.
    github
}

/// The trust anchor the network verbs verify under: the committed paper-master keyset
/// ([`crate::PKG_TRUST_ANCHORS`]) plus this store's durable `roster_seq` ratchet, and
/// nothing else.
///
/// There used to be an `ATPKG_ROOTKEY_OVERRIDE` here that swapped WHICH root key anchored
/// verification "without a rebuild". It is gone. However carefully it was scoped, it let
/// ambient process state decide what the package manager trusted — an unpinned build
/// could be handed an anchor by an environment variable. Trusting a mirror or a second
/// owner account is now a committed change to `aterm_update_core::pins`, visible in a
/// diff like any other trust decision.
///
/// The roster floor is read HERE, per call, rather than snapshotted once at process
/// start: a verb that ratchets the floor mid-pass (the default-set bootstrap installs
/// several programs in a row) must not have a later step verify under a stale, lower
/// ratchet than the one it just advanced.
fn effective_anchor(layout: &crate::store::Layout) -> crate::Anchor {
    crate::Anchor::pinned(crate::sig::Floor::new(layout.roster_floor()).current())
}

/// This store's durable `index_build` high-water AND the roster generation that recorded
/// it (§8 gate 3) — one value, because a floor a MACHINE set must not outlive the
/// generation that revoked that machine. See [`crate::sig::BuildFloor`].
fn build_floor(layout: &crate::store::Layout) -> crate::sig::BuildFloor {
    crate::sig::BuildFloor {
        index_build: crate::sig::Floor::new(layout.floor()).current(),
        roster_seq: crate::sig::Floor::new(layout.floor_generation()).current(),
    }
}

/// Advance the durable ratchets after a successful resolve. Best-effort, exactly as the
/// single floor advance always was — a failed write never turns a completed install into
/// an error.
///
/// # Which ratchet turns where, and why they are not the same call
///
/// The `roster_seq` high-water is NOT advanced here as its authoritative write: it turns on
/// OBSERVATION, inside `flow::verify_select_fresh`, the instant a generation is admitted —
/// because a client that merely SAW a revocation must refuse the pre-revocation roster even
/// if it went on to install nothing. Repeating it here is belt-and-braces over an idempotent
/// monotonic write (this seq was admitted in the same pass), not the defence itself.
///
/// The `index_build` floor is the one this function really owns, and it is written
/// GENERATION-AWARE: a strictly newer master-signed generation RE-BASES it (possibly
/// downward) instead of ratcheting it. Without that, one rostered machine publishing
/// `index_build = u64::MAX` would raise a monotonic floor above everything the owner can
/// ever publish — including the index carrying that machine's revocation — and no republish
/// could recover the store. Only the paper master can mint a generation, so only the paper
/// master can pull that lever.
fn advance_floors(layout: &crate::store::Layout, index_build: u64, roster_seq: u64) {
    let generation = crate::sig::Floor::new(layout.floor_generation());
    let build = crate::sig::Floor::new(layout.floor());
    if roster_seq > generation.current() {
        build.rebase(index_build);
    } else {
        let _ = build.check_and_record(index_build);
    }
    let _ = generation.check_and_record(roster_seq);
    crate::flow::observe_roster_generation(layout, roster_seq);
}

/// Whether the manager may act on the network verbs: a committed root anchor to
/// verify under AND the user has not opted out via `ATPKG_DISABLE`.
fn manager_enabled() -> bool {
    crate::manager_enabled()
}

/// Record each freshly-installed `requires` dependency of `parent` into `status.toml`
/// (each carries its own SIGNED tree_root for `verify`). Shared by [`do_install`] and the
/// default-set bootstrap's ungrouped arm — identical writes at both.
fn record_installed_deps(
    layout: &crate::store::Layout,
    parent: &str,
    deps: &[crate::flow::DepOutcome],
) {
    for dep in deps {
        if let crate::flow::DepResult::Installed { build, tree_root } = &dep.result {
            record_status(
                layout,
                &dep.program,
                crate::ProgramStatus {
                    installed_build: Some(*build),
                    state: "active".into(),
                    tree_root: tree_root.clone(),
                },
                format!("pulled in {} (required by {parent})", dep.program),
            );
        }
    }
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
    let floor = build_floor(layout);
    let req = crate::InstallRequest {
        channel,
        program,
        triple: current_triple(),
        installed,
    };
    let result = crate::install(
        fetcher,
        layout,
        &effective_anchor(layout),
        &req,
        floor,
        now_unix(),
    );
    match &result {
        Ok(r) => {
            // Advance the durable high-water floor to the index we just trusted (§8 gate 3).
            advance_floors(layout, r.index_build, r.roster_seq);
            let outcome = if r.already_current {
                format!("up to date ({program} build {})", r.build)
            } else {
                format!("installed {program} build {}", r.build)
            };
            // Record each pulled-in dependency FIRST (§17), so the main program's outcome
            // remains the final aggregate.
            record_installed_deps(layout, program, &r.dependencies);
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
            // install). Runs at the CLI edge — not inside flow.rs — so flow's unit tests
            // stay hermetic w.r.t. the developer's real store. Satisfies "GC after every
            // successful activate".
            //
            // The report is dropped ON PURPOSE here, unlike in the prefix-wide verbs. This
            // pass sweeps the whole store, but the program THIS verb just activated is
            // freshly linked and freshly shimmed, so it is witnessed; any abstention is about
            // some other program the user did not ask about. Repeating that on every single
            // install is how a warning becomes wallpaper — `atpkg gc` and `atpkg doctor` are
            // the verbs whose job is to say it.
            if !r.already_current {
                let _ = crate::gc::run(layout);
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
        eprintln!("atpkg: disabled (no root key pinned) — refusing to install");
        return ExitCode::from(1);
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let cfg = crate::config::cached();
    cmd_install_with(program, &layout, cfg, &*resolve_fetcher(&layout))
}

/// The body of [`cmd_install`] against an ALREADY-BUILT fetcher, so a caller that has one
/// does not construct a second.
///
/// Building a `GithubFetcher` is not free: it runs the whole token chain (a keychain probe
/// plus up to three `gh auth token` spawns — hundreds of ms of blocking subprocess latency
/// for an answer that cannot differ within one process), and a fresh fetcher also throws
/// away the first one's memoized release listings + index candidates, so the entire signed
/// index (a listing plus up to 20 × 2 asset downloads) is re-fetched. `atpkg update
/// <ungrouped-program>` paid both twice because it re-entered through `cmd_install`.
fn cmd_install_with(
    program: &str,
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    fetcher: &dyn crate::flow::Fetcher,
) -> ExitCode {
    reconcile_links(layout, cfg);
    match do_install(layout, fetcher, cfg.channel(), program) {
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
        eprintln!("atpkg: disabled (no paper master pinned) — cannot roll back");
        return ExitCode::from(1);
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let floor = build_floor(&layout);
    let fetcher = resolve_fetcher(&layout);
    match crate::flow::rollback(
        &*fetcher,
        &layout,
        &effective_anchor(&layout),
        crate::config::cached().channel(),
        program,
        floor,
        now_unix(),
    ) {
        Ok(r) => {
            // Advance both durable ratchets to what we just trusted (§8 gate 3).
            advance_floors(&layout, r.index_build, r.roster_seq);
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
/// `update`. A pin is purely LOCAL upgrade-suppression state (no index/network); it is
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
                    "pinned (held against update)"
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
        eprintln!("atpkg: disabled (no root key pinned) — nothing to update");
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
        let floor = build_floor(&layout);
        let report = match crate::apply_channel(
            &*fetcher,
            &layout,
            &effective_anchor(&layout),
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
        advance_floors(&layout, report.index_build, report.roster_seq);
        failures = report_channel_apply(&layout, &installed, &report);
        for p in &report.skipped_linked {
            println!("atpkg: {p} dev-linked — skipped");
        }
    }
    // §11 batteries-included: with explicit config consent, ALSO install the index
    // default-set members not yet installed (include/exclude-narrowed; linked/yanked
    // members skip; per-program failures are loud but never block the rest).
    if cfg.auto_install() {
        failures += install_default_set(&layout, &*fetcher, &effective_anchor(&layout), cfg, now_unix());
    }
    // Reclaim superseded builds once after the whole channel apply (all group activations
    // done). Best-effort; never fails the update. This verb sweeps the WHOLE prefix, so an
    // abstention here is about a program it did try to keep current — reported, or the disk
    // grows after every update with nothing on screen ever mentioning it.
    let report = crate::gc::run(&layout);
    print_gc_sweeps("update", &report);
    print_gc_abstentions("update", &report);
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
    anchor: &crate::Anchor,
    cfg: &crate::config::PackagesConfig,
    now: i64,
) -> u32 {
    let floor = build_floor(layout);
    let index = match crate::resolve_verified_index(fetcher, layout, anchor, floor, now) {
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
            // resolved above (§7) — see [`bootstrap_group`].
            Some(g) => {
                failures += bootstrap_group(
                    layout, fetcher, &index, cfg, g, &group, &wanted, &installed, &missing,
                );
            }
            // An ungrouped member can move alone (§7) — see [`bootstrap_singleton`].
            None => {
                failures +=
                    bootstrap_singleton(layout, fetcher, anchor, cfg, &group.members[0], now);
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
        record_bootstrap_error(layout, program, &e);
    }
    failures
}

/// The grouped (coherence-tuple) arm of [`install_default_set`]: refuse a config-narrowed
/// partial tuple, clean-skip a tuple with no artifact for this triple, else fresh-install
/// the WHOLE group all-or-nothing against the caller's ONE resolved index (§7) via
/// [`crate::flow::bootstrap_group`] — stage-all → flip-all → rollback, exactly the update
/// path's transaction, so no failure mode can activate a partial or version-split trust
/// toolchain. Returns the hard-failure count (0 or 1); skips are never failures.
#[allow(
    clippy::too_many_arguments,
    reason = "the group arm consumes install_default_set's whole resolved context: layout, \
              fetcher, the ONE verified index, config, the group + its name, and the \
              wanted/installed/missing member sets the narrowing check and prescan read"
)]
fn bootstrap_group(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    cfg: &crate::config::PackagesConfig,
    g: &str,
    group: &crate::Group,
    wanted: &std::collections::BTreeSet<String>,
    installed: &std::collections::BTreeMap<String, u64>,
    missing: &[String],
) -> u32 {
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
        return 0;
    }
    // Missing-triple prescan: the singleton NoArtifact clean-skip doctrine
    // lifted to the tuple — a group that cannot fully exist on this host
    // is a correct state, not a 6h scream.
    if let Some(m) =
        crate::flow::group_missing_triple(fetcher, index, cfg.channel(), current_triple(), missing)
    {
        println!(
            "atpkg: {m}: no artifact for {} — coherence group '{g}' skipped whole \
             (§6 clean skip)",
            current_triple()
        );
        return 0;
    }
    match crate::flow::bootstrap_group(
        fetcher,
        layout,
        index,
        cfg.channel(),
        current_triple(),
        group,
        installed,
    ) {
        Ok((crate::TxnOutcome::Applied(_), applied)) => {
            // Advance the durable floor to the ONE index the whole group
            // trusted (§8 gate 3).
            advance_floors(layout, index.index_build, index.roster_seq());
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
            0
        }
        Ok((crate::TxnOutcome::UpToDate, _)) => 0,
        Ok((crate::TxnOutcome::Pinned(held), _)) => {
            println!("atpkg: coherence group '{g}' held by local pin {held:?} — skipped");
            0
        }
        Ok((crate::TxnOutcome::Tombstoned(members), _)) => {
            eprintln!(
                "atpkg: coherence group '{g}': pins tombstoned for {members:?} — \
                 nothing installed"
            );
            0
        }
        Ok((
            crate::TxnOutcome::Aborted {
                failed,
                during_flip,
            },
            _,
        )) => {
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
            1
        }
        Err(e) => {
            eprintln!("atpkg: bootstrap of coherence group '{g}' failed: {e} (continuing)");
            1
        }
    }
}

/// The ungrouped arm of [`install_default_set`]: the per-member install path, which keeps
/// its signed `requires` dependency resolution (§17). Re-reads the durable floor so a
/// floor advance recorded earlier in the pass is never undercut by the entry-time
/// snapshot. Returns the hard-failure count (0 or 1); the correct non-failure states
/// (dev-link, missing triple, app-bundle, tombstoned pin) are skips.
fn bootstrap_singleton(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    anchor: &crate::Anchor,
    cfg: &crate::config::PackagesConfig,
    program: &str,
    now: i64,
) -> u32 {
    let floor = build_floor(layout);
    let req = crate::InstallRequest {
        channel: cfg.channel(),
        program,
        triple: current_triple(),
        installed: None,
    };
    match crate::install(fetcher, layout, anchor, &req, floor, now) {
        Ok(r) => {
            // Advance the durable floor to the index this install trusted
            // (§8 gate 3).
            advance_floors(layout, r.index_build, r.roster_seq);
            record_installed_deps(layout, program, &r.dependencies);
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
            0
        }
        // Correct non-failure states — skipped quietly-but-visibly:
        Err(crate::FlowError::Linked(p)) => {
            println!("atpkg: {p} dev-linked — skipped");
            0
        }
        Err(crate::FlowError::NoArtifact(t)) => {
            println!("atpkg: {program}: no artifact for {t} — skipped (§6 clean skip)");
            0
        }
        Err(crate::FlowError::AppBundleRefused(_)) => {
            println!(
                "atpkg: {program}: app-bundle member — managed by the app's own updater, skipped"
            );
            0
        }
        Err(e @ crate::FlowError::Tombstoned(_)) => {
            eprintln!("atpkg: {program}: {e} — nothing installed");
            0
        }
        Err(e) => {
            eprintln!("atpkg: bootstrap install {program} failed: {e} (continuing)");
            record_bootstrap_error(layout, program, &e);
            1
        }
    }
}

/// Record one hard bootstrap failure for `program` into `status.toml` — shared by the
/// ungrouped install arm and [`install_default_set`]'s unpinned-member sweep, which
/// perform identical writes.
fn record_bootstrap_error(layout: &crate::store::Layout, program: &str, e: &crate::FlowError) {
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

/// `atpkg install --default-set` — the explicit one-shot §11 bootstrap: install every
/// missing default-set member NOW (the Settings "Install ALab toolset" button's verb;
/// the config-consent-free twin of the `auto_install = true` loop arm — running it IS
/// the consent). Exit 1 iff any member hard-failed; skips are honest and free.
fn cmd_install_default_set() -> ExitCode {
    if !manager_enabled() {
        eprintln!("atpkg: disabled (no root key pinned) — refusing to install");
        return ExitCode::from(1);
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let cfg = crate::config::cached();
    reconcile_links(&layout, cfg);
    let fetcher = resolve_fetcher(&layout);
    let failures = install_default_set(&layout, &*fetcher, &effective_anchor(&layout), cfg, now_unix());
    // GC + shell-hook refresh once at the CLI edge (the cmd_update_all precedent) — including
    // its disclosure of what the pass abstained on, for the same reason: this verb walks the
    // whole prefix, so a skip here is not about a program the user never mentioned.
    let report = crate::gc::run(&layout);
    print_gc_sweeps("install-default-set", &report);
    print_gc_abstentions("install-default-set", &report);
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
/// verb (`install`, `install --default-set`, `update`) already holding the
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
    let floor = build_floor(&layout);
    let fetcher = resolve_fetcher(&layout);
    let plan = match crate::flow::plan_update(
        &*fetcher,
        &layout,
        &effective_anchor(&layout),
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
            &effective_anchor(&layout),
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
        advance_floors(&layout, report.index_build, report.roster_seq);
        let failures = report_channel_apply(&layout, &installed, &report);
        for p in &report.skipped_linked {
            println!("atpkg: {p} dev-linked — skipped");
        }
        // GC after every successful activate — the same policy (and order: GC, then the
        // shell-hook refresh) as `cmd_update_all` and `do_install`, which the ungrouped
        // path below reaches through `cmd_install`. Best-effort; never fails the update.
        let gc = crate::gc::run(&layout);
        print_gc_sweeps("update", &gc);
        print_gc_abstentions("update", &gc);
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
    // Re-enter the single-program install path with the fetcher THIS verb already built
    // and already warmed (token chain + index candidates), instead of `cmd_install`'s
    // second one. Everything `cmd_install` would re-do first — `manager_enabled`,
    // `layout()`, `cfg` — ran above with the same answers, and `reconcile_links` still
    // runs inside `cmd_install_with` exactly as before, so the output is unchanged.
    cmd_install_with(program, &layout, cfg, &*fetcher)
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

/// `atpkg verify-index <master-pubkey-b64> <index.toml> <index.toml.sig> <roster.toml>
/// <roster.toml.sig>` — run the WHOLE client chain over files on disk, for operators and
/// the publish pipeline's self-check. Exit 0 iff a machine the given paper master's roster
/// authorizes signed that index and the index's own attribution agrees.
///
/// It takes the MASTER key rather than an index key because that is the only key the
/// client pins: checking `index.toml` against "some public key" would prove nothing a
/// client would act on. The roster floor is 0 (this verb has no store to ratchet) and the
/// clock is the real one, so a lapsed roster fails here exactly as it would on a user's
/// machine — which is the point of a pipeline self-check.
fn cmd_verify_index(args: &[String]) -> ExitCode {
    let [master, index, sig, roster, roster_sig] = args else {
        eprintln!(
            "usage: atpkg verify-index <master-pubkey-b64> <index.toml> <index.toml.sig> \
             <aterm-machines.toml> <aterm-machines.toml.sig>"
        );
        return ExitCode::from(2);
    };
    let (Ok(raw), Ok(sig_bytes), Ok(roster_bytes), Ok(roster_sig_bytes)) = (
        std::fs::read(index),
        std::fs::read(sig),
        std::fs::read(roster),
        std::fs::read(roster_sig),
    ) else {
        eprintln!("atpkg: cannot read the index, the roster, or one of their signatures");
        return ExitCode::from(1);
    };
    let anchor = crate::Anchor::of(vec![master.clone()], 0);
    let Ok(admitted) = crate::admit_roster(&anchor, roster_bytes, &roster_sig_bytes, now_unix())
    else {
        // Opaque on purpose — no verification oracle (§8).
        eprintln!("FAIL: the roster did not verify under that master, or is stale");
        return ExitCode::from(1);
    };
    match admitted.authorize_index(raw, &sig_bytes) {
        Ok((idx, _)) => {
            println!(
                "OK: index build {} signed by machine {} under roster seq {}",
                idx.index_build,
                idx.attribution().machine_id,
                idx.roster_seq()
            );
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("FAIL: no machine on that roster signed this index");
            ExitCode::from(1)
        }
    }
}

/// `atpkg verify-pkg <master-pubkey-b64> <pkg-*.toml> <pkg.sig> <aterm-machines.toml>
/// <aterm-machines.toml.sig>` — prove a package manifest was signed by a machine the
/// given paper master's roster authorizes. The pkg sibling of [`cmd_verify_index`], and
/// it exists for the same operator: the mirror re-verifies every document it republishes
/// with the client's own chain, and before this verb it had no way to do that for
/// `pkg-*.toml` short of hand-rolling Ed25519 in shell — so it shipped with an honest
/// comment saying pkg manifests were NOT crypto-verified. That comment collapses to one
/// call to this.
///
/// Same authority rule as the index (`TrustedRoster::authorize_bytes`): any listed,
/// unrevoked, unexpired machine on the admitted generation. No attribution bind, because
/// a pkg manifest carries none — the ID printed comes from the signature that verified.
fn cmd_verify_pkg(args: &[String]) -> ExitCode {
    let [master, pkg, sig, roster, roster_sig] = args else {
        eprintln!(
            "usage: atpkg verify-pkg <master-pubkey-b64> <pkg-*.toml> <pkg.sig> \
             <aterm-machines.toml> <aterm-machines.toml.sig>"
        );
        return ExitCode::from(2);
    };
    let (Ok(raw), Ok(sig_bytes), Ok(roster_bytes), Ok(roster_sig_bytes)) = (
        std::fs::read(pkg),
        std::fs::read(sig),
        std::fs::read(roster),
        std::fs::read(roster_sig),
    ) else {
        eprintln!("atpkg: cannot read the manifest, the roster, or one of their signatures");
        return ExitCode::from(1);
    };
    let anchor = crate::Anchor::of(vec![master.clone()], 0);
    let Ok(admitted) = crate::admit_roster(&anchor, roster_bytes, &roster_sig_bytes, now_unix())
    else {
        // Opaque on purpose — no verification oracle (§8).
        eprintln!("FAIL: the roster did not verify under that master, or is stale");
        return ExitCode::from(1);
    };
    match admitted.authorize_bytes(raw, &sig_bytes) {
        Ok((_, who)) => {
            println!(
                "OK: manifest signed by machine {} under roster seq {}",
                who.machine_id, who.roster_seq
            );
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("FAIL: no machine on that roster signed this manifest");
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
            BuildMismatch, Drift, Match, NoSignedRoot, NotInstalled, Unreadable, WiredSysroot,
        };
        match o {
            Match { build } => {
                println!("atpkg: {name} build {build} OK (matches signed tree_root)")
            }
            WiredSysroot { build } => println!(
                "atpkg: {name} build {build} OK (rustup-linked sysroot bundle; verified at install, \
                 not tree-attestable after install-time wiring — use a self-contained bundle for full \
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

    /// [`VERBS`] must name EXACTLY the verbs `main_entry`'s match dispatches —
    /// no more (advertising a verb that no longer exists) and no fewer (a
    /// working verb the hint never mentions).
    ///
    /// Derived from this file's own source rather than from a second hand-kept
    /// list, because a second list is the thing that rotted. `sync` and `seed`
    /// were each advertised for months after deletion; both would have failed
    /// this the day their arm was removed.
    #[test]
    fn verb_hint_matches_dispatch() {
        let src = include_str!("cli.rs");
        // The dispatch arms, in source order: `        Some("<verb>") =>`.
        let dispatched: Vec<&str> = src
            .lines()
            .filter_map(|line| line.trim().strip_prefix("Some(\""))
            .filter_map(|rest| rest.split_once("\")"))
            .filter(|(_, tail)| tail.trim_start().starts_with("=>"))
            .map(|(verb, _)| verb)
            .collect();

        // Non-vacuity: if the extraction ever stops matching the source shape it
        // would silently compare two empty lists and pass.
        assert!(
            dispatched.len() > 10,
            "extracted only {} arm(s) — the scraper stopped matching the match block, \
             so this test was about to pass vacuously: {dispatched:?}",
            dispatched.len()
        );

        assert_eq!(
            dispatched, VERBS,
            "the unknown-verb hint and the dispatch table disagree.\n  \
             dispatched: {dispatched:?}\n  advertised: {VERBS:?}\n\
             A verb in `advertised` but not `dispatched` tells the user to run \
             something that only reprints this error."
        );
    }

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
            "update",
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
            "verify-pkg",
            "relocate",
            "help",
            "-h",
            "--help",
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

    /// REGRESSION: no ambient state may supply the verification anchor.
    ///
    /// `ATPKG_ROOTKEY_OVERRIDE` used to swap which root key anchored verification,
    /// and could supply one to a build that had none — an environment variable
    /// deciding trust. The anchor is now the committed paper-master keyset and only
    /// that, so setting the old variable must change nothing.
    #[test]
    fn no_environment_variable_can_supply_the_verification_anchor() {
        // NO `set_var` HERE, deliberately. The claim is that the anchor is a
        // COMPILED constant and the variable is not consulted at all — and
        // `effective_anchor` reads no environment (only the store's own durable
        // roster floor), which is the property under test. Mutating the
        // process-global environment to demonstrate that would race every other
        // test in this binary (cargo runs them on threads) to prove something the
        // source already settles, and the lint that flags it is right.
        let layout = temp_layout("anchor");
        assert_eq!(
            effective_anchor(&layout).is_armed(),
            !crate::PKG_TRUST_ANCHORS.is_empty(),
            "the committed keyset is the only thing that arms the manager"
        );
        assert!(
            effective_anchor(&layout).is_armed(),
            "and in THIS tree it is armed (2026-08-15), so the CLI's anchor is live"
        );
        // NON-VACUITY: the reader must contain no env lookup at all, so this
        // cannot pass by the variable merely being unset in this process.
        assert!(
            !include_str!("cli.rs").contains("ATPKG_ROOTKEY_OVERRIDE\""),
            "no env-var lookup may reach the anchor"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// Enablement follows the COMPILED keyset and the opt-out, and nothing else.
    /// `ATPKG_DISABLE` may only ever subtract authority — a kill switch that can
    /// turn the manager off is safe; one that could turn it on was not, which is
    /// why the root-key override is gone.
    #[test]
    fn enablement_follows_the_compiled_anchor_and_the_kill_switch_only() {
        // No compiled anchor ⇒ inert. There is no longer any way to supply one
        // at runtime, so this state can only be changed by a commit. THIS is the
        // shipped state: `PKG_TRUST_ANCHORS` is empty.
        assert!(!crate::manager_enabled_with(&[], false));
        assert!(!crate::manager_enabled_with(&[], true));
        // A compiled keyset ⇒ enabled...
        assert!(crate::manager_enabled_with(&["PINNED_KEY"], false));
        // ...and the opt-out wins even with a valid keyset present.
        assert!(!crate::manager_enabled_with(&["PINNED_KEY"], true));
    }

    /// The two durable ratchets advance TOGETHER and INDEPENDENTLY: an index build and a
    /// roster generation are different counters over different documents, and folding
    /// them into one high-water would let either one's advance refuse the other's
    /// perfectly current document.
    #[test]
    fn advance_floors_ratchets_both_counters_separately() {
        let layout = temp_layout("floors");
        std::fs::create_dir_all(&layout.prefix).unwrap();
        std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o700)).unwrap();
        advance_floors(&layout, 41, 3);
        assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 41);
        assert_eq!(crate::sig::Floor::new(layout.roster_floor()).current(), 3);
        // An index republish that does NOT bump the roster leaves the roster floor put,
        // so the generation still in use is not ratcheted out from under the client.
        advance_floors(&layout, 42, 3);
        assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 42);
        assert_eq!(crate::sig::Floor::new(layout.roster_floor()).current(), 3);
        // A revocation bumps the roster without re-cutting the index — symmetrically.
        advance_floors(&layout, 42, 4);
        assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 42);
        assert_eq!(crate::sig::Floor::new(layout.roster_floor()).current(), 4);
        // And the anchor the CLI builds carries the roster floor it just recorded.
        assert_eq!(effective_anchor(&layout).roster_floor, 4);
        // NON-VACUITY: they are genuinely separate files.
        assert_ne!(layout.floor(), layout.roster_floor());
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// A BUILD FLOOR ONE MACHINE DROVE OUT OF REACH IS RE-BASED BY THE NEXT GENERATION.
    ///
    /// `index_build` sits inside MACHINE-signed bytes, so any rostered machine can set it to
    /// the u64 ceiling and — under a purely monotonic ratchet — put the store permanently
    /// above every index the owner will ever publish, including the one that revokes that
    /// machine. Bricked AND unrevocable, by a tier the roster exists to be able to revoke.
    ///
    /// So the floor is scoped to the generation that recorded it: a strictly newer
    /// master-signed generation re-bases it. Only the paper master can mint a generation, so
    /// this lever is the master's alone.
    ///
    /// MUTATION: replace the `roster_seq > generation.current()` arm in `advance_floors`
    /// with the plain `check_and_record` and the rescue assertion fails with the floor still
    /// at `u64::MAX`.
    #[test]
    fn a_newer_generation_rebases_a_floor_a_machine_drove_out_of_reach() {
        let layout = temp_layout("floor-poison");
        std::fs::create_dir_all(&layout.prefix).unwrap();
        std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o700)).unwrap();

        // A machine on generation 3 publishes the TOML integer ceiling.
        advance_floors(&layout, u64::MAX, 3);
        assert_eq!(build_floor(&layout).index_build, u64::MAX);
        assert_eq!(build_floor(&layout).roster_seq, 3);
        // PRECONDITION: within that same generation the floor is immovable — this really is
        // the trap, not a floor that would have relaxed on its own.
        advance_floors(&layout, 101, 3);
        assert_eq!(
            build_floor(&layout).index_build,
            u64::MAX,
            "same generation ⇒ monotonic, exactly as before"
        );

        // The owner revokes that machine and republishes at a sane build under generation 4.
        advance_floors(&layout, 101, 4);
        assert_eq!(
            build_floor(&layout),
            crate::sig::BuildFloor {
                index_build: 101,
                roster_seq: 4
            },
            "a newer master-signed generation re-bases the floor it inherited"
        );
        // ...and from there it ratchets again, so the rescue is one step, not a hole.
        advance_floors(&layout, 100, 4);
        assert_eq!(build_floor(&layout).index_build, 101);
        let _ = std::fs::remove_dir_all(&layout.prefix);
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

    /// The chain-engagement predicate mirrors `token.rs::needs_ambient_credential`
    /// by construction (same constants); pin the behavior at both poles so the
    /// mirror can never silently drift: ONLY the compiled public channel slug is
    /// anonymous by construction, any other owner OR repo runs the full chain.
    #[test]
    fn only_the_public_channel_slug_skips_the_credential_chain() {
        assert!(
            !engages_credential_chain(
                aterm_update_core::DEFAULT_OWNER,
                aterm_update_core::DEFAULT_REPO
            ),
            "the compiled public channel needs no ambient credential"
        );
        assert!(
            engages_credential_chain("someone-else", aterm_update_core::DEFAULT_REPO),
            "a foreign owner engages the full chain"
        );
        assert!(
            engages_credential_chain(aterm_update_core::DEFAULT_OWNER, "some-other-repo"),
            "a foreign repo engages the full chain"
        );
        // NON-VACUITY for the destination tests below: the compiled index default
        // IS the public channel slug, so the default account resolves to the
        // anonymous-by-construction destination.
        assert!(!engages_credential_chain(
            aterm_update_core::ATPKG_INDEX_OWNER,
            crate::manifest::INDEX_REPO
        ));
    }

    /// REGRESSION (adversarial review 2026-08-11): keying the credential chain to
    /// the index destination ALONE starved the `[packages.links]` lane — on the
    /// compiled default account the index slug is the public channel, so the
    /// chain's gate short-circuited and a links override to a private repo went
    /// out anonymous (404 on a machine whose keychain / 0600-file / `gh auth`
    /// credential had worked the round before). The pick must range over EVERY
    /// destination the fetcher reaches: index slug + all override slugs.
    #[test]
    fn credential_destination_covers_index_and_links_overrides() {
        use std::collections::BTreeMap;
        let (default_owner, default_repo) = (
            aterm_update_core::ATPKG_INDEX_OWNER,
            crate::manifest::INDEX_REPO,
        );
        // Default account + no overrides → the public channel slug: the chain
        // stays anonymous by construction (the LIVE registry's proven install
        // lane — no ambient PAT is ever gathered for it).
        let dest = credential_destination(default_owner, default_repo, &BTreeMap::new());
        assert_eq!(dest, (default_owner.to_string(), default_repo.to_string()));
        assert!(
            !engages_credential_chain(&dest.0, &dest.1),
            "default + no overrides must stay anonymous by construction"
        );
        // Default account + a links override to a possibly-private repo → the
        // OVERRIDE slug, which engages the full chain (the regression case).
        let mut overrides = BTreeMap::new();
        overrides.insert("myprog".to_string(), "someorg/private-repo".to_string());
        let dest = credential_destination(default_owner, default_repo, &overrides);
        assert_eq!(
            dest,
            ("someorg".to_string(), "private-repo".to_string()),
            "a links override must key the chain to the override's slug"
        );
        assert!(
            engages_credential_chain(&dest.0, &dest.1),
            "the links-override destination must engage the full chain"
        );
        // An override that IS the public channel slug engages nothing — the pick
        // falls back to the (equally public) index slug: still anonymous.
        let mut public_override = BTreeMap::new();
        public_override.insert(
            "myprog".to_string(),
            format!("{default_owner}/{default_repo}"),
        );
        let dest = credential_destination(default_owner, default_repo, &public_override);
        assert!(
            !engages_credential_chain(&dest.0, &dest.1),
            "a public-slug override must not conjure a credential lookup"
        );
        // Repointed account → the INDEX slug engages the chain and wins even with
        // overrides present: a repoint governs every non-overridden fetch, so it
        // is the destination the one shared token must serve first.
        let dest = credential_destination("my-private-org", default_repo, &overrides);
        assert_eq!(
            dest,
            ("my-private-org".to_string(), default_repo.to_string()),
            "a repointed account keys the chain to the index slug"
        );
        assert!(
            engages_credential_chain(&dest.0, &dest.1),
            "the repointed-account destination must engage the full chain"
        );
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

    use crate::sig::testkit;

    /// The synthetic paper master and the one machine it rosters — the SAME fixture the
    /// whole crate signs with, so a CLI test cannot accidentally prove something under a
    /// trust shape no other layer uses.
    const ROOT_SEED: [u8; 32] = testkit::MASTER_SEED;
    const RELEASE_SEED: [u8; 32] = testkit::MACHINE_SEED;

    /// The anchor the dir-registry tests resolve under: armed with the synthetic master,
    /// roster floor 0.
    fn test_anchor() -> crate::Anchor {
        crate::Anchor::of(vec![pk(&ROOT_SEED)], 0)
    }

    /// Publish the master-signed roster beside a `dir:` registry's index. Without it the
    /// DirFetcher yields no candidate at all — a registry is index PLUS the generation
    /// that authorized its signer, never one without the other.
    fn write_roster(dir: &Path) {
        let (bytes, sig) = testkit::published_roster();
        std::fs::write(
            dir.join(aterm_update_core::roster::ROSTER_ASSET),
            bytes.as_slice(),
        )
        .unwrap();
        std::fs::write(
            dir.join(aterm_update_core::roster::ROSTER_SIG_ASSET),
            sig.as_slice(),
        )
        .unwrap();
    }

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
            "schema = 2\nprogram = \"{prog}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
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
        write_registry_at(dir, channel, 41);
    }

    /// As [`write_registry`], with an explicit `index_build` (the seed-admission tests
    /// need a sealed index strictly above / equal to the durable floor).
    fn write_registry_at(dir: &Path, channel: &str, index_build: u64) {
        let index_body = format!(
            "schema = 2\nindex_build = {index_build}\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             machine_id = \"{id}\"\nroster_seq = {seq}\n\
             [programs.ay]\nrepo = \"ay\"\n[programs.ny]\nrepo = \"ny\"\n\
             [programs.zz]\nrepo = \"zz\"\n[programs.lk]\nrepo = \"lk\"\n\
             [[channels]]\nname = \"{channel}\"\nchannel_build = 1\nmin_build = 0\n\
             yanked = [\"zz@5\"]\npin = {{ ay = 18, ny = 7, zz = 5, lk = 3 }}\n",
            id = testkit::MACHINE_ID,
            seq = testkit::SEQ
        );
        std::fs::write(dir.join("index.toml"), index_body.as_bytes()).unwrap();
        // The index is MACHINE-signed now; only the roster is signed by the master.
        std::fs::write(
            dir.join("index.toml.sig"),
            sign(&RELEASE_SEED, index_body.as_bytes()),
        )
        .unwrap();
        write_roster(dir);
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
        assert!(failures > 0, "a missing channel is loud, not silent");
        assert!(crate::active_builds(&layout).is_empty());
        let _ = std::fs::remove_dir_all(&layout.prefix);
        // channel = "nightly" threads through and installs.
        let layout = temp_layout("channel-nightly");
        let cfg = crate::config::parse_packages("[packages]\nchannel = \"nightly\"\n");
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
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
            "schema = 2\nindex_build = 51\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             machine_id = \"{id}\"\nroster_seq = {seq}\n\
             [programs.ay]\nrepo = \"ay\"\n\
             [programs.ta]\nrepo = \"ta\"\ncoherence_group = \"rustc\"\n\
             [programs.tb]\nrepo = \"tb\"\ncoherence_group = \"rustc\"\n\
             [[channels]]\nname = \"{channel}\"\nchannel_build = 1\nmin_build = 0\n\
             pin = {{ ay = 18, ta = 4, tb = 6 }}\n",
            id = testkit::MACHINE_ID,
            seq = testkit::SEQ
        );
        std::fs::write(dir.join("index.toml"), index_body.as_bytes()).unwrap();
        // The index is MACHINE-signed now; only the roster is signed by the master.
        std::fs::write(
            dir.join("index.toml.sig"),
            sign(&RELEASE_SEED, index_body.as_bytes()),
        )
        .unwrap();
        write_roster(dir);
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
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
            crate::platform::resolve_shim(
                &layout.shim(&crate::store::ToolName::new("ay").unwrap())
            )
            .unwrap(),
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
