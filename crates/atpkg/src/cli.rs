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
/// reality twice: `sync` (deleted in `a43193f4`) and the source-build-era
/// `seed` (deleted in `ba832933`; the verb returned 2026-08-17 as the signed
/// bundled-seed bootstrap only) were both still advertised here long after
/// they stopped dispatching — so a user who followed the suggestion got the
/// very error that printed it. A hand-maintained help string cannot be
/// trusted to track a match arm; a test can.
const VERBS: &[&str] = &[
    "doctor",
    "status",
    "which",
    "list",
    "uninstall",
    "tree-root",
    "verify-index",
    "verify-pkg",
    "install",
    "seed",
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

/// The advertised roster, grouped the way a person meets it. PRESENTATION ONLY:
/// dispatch order stays [`VERBS`] (pinned by [`tests::verb_hint_matches_dispatch`]);
/// these tiers are pinned as an EXACT partition of it by
/// [`tests::verb_tiers_partition_the_roster`], so a verb added to dispatch without
/// a tier fails loudly instead of silently vanishing from every human surface —
/// the drift that killed `sync` and `seed`.
///
/// Three tiers, because that is the question a first hour actually has: what do I
/// type today (daily), what do I type when something needs undoing or holding
/// (occasional), and what exists but is not for me yet (plumbing). A flat 21-name
/// comma list buried `install` between `verify-pkg` and `seed`, which is how a new
/// user ends up reading the whole roster to find the one verb the docs call "the
/// usual first command".
const VERB_TIERS: &[(&str, &[&str])] = &[
    (
        "daily",
        &["install", "list", "update", "doctor", "status", "which", "run"],
    ),
    (
        "occasional",
        &["uninstall", "rollback", "pin", "unpin", "verify", "gc"],
    ),
    (
        "plumbing",
        &[
            "seed",
            "link",
            "unlink",
            "refresh",
            "tree-root",
            "verify-index",
            "verify-pkg",
            "relocate",
        ],
    ),
];

/// The tier block, rendered — ONE function so bare `atpkg`, `--help`, and the
/// unknown-verb error can never fork the roster the way the old two hand-written
/// copies (bare vs `--help`) already had.
///
/// `status` is real in [`VERB_TIERS`] (the partition test needs every dispatched
/// verb placed) but renders attached to `doctor` as `doctor (or: status)`: the two
/// are one report, and listing them as siblings would imply a difference for the
/// reader to go hunting for.
fn verb_tier_lines() -> Vec<String> {
    VERB_TIERS
        .iter()
        .map(|(tier, verbs)| {
            let shown: Vec<&str> = verbs
                .iter()
                .filter(|v| **v != "status")
                .map(|v| if *v == "doctor" { "doctor (or: status)" } else { *v })
                .collect();
            format!("atpkg:   {tier:<12} {}", shown.join(", "))
        })
        .collect()
}

/// Print [`verb_tier_lines`] to `out` — the one shared renderer (stdout for the
/// posture/help surfaces, stderr beside the unknown-verb error).
fn print_verb_tiers(out: &mut impl std::io::Write) {
    for line in verb_tier_lines() {
        let _ = writeln!(out, "{line}");
    }
}

/// The advertised verb closest to `input` (Levenshtein distance ≤ 2), or `None` when
/// nothing is close enough to suggest — a far-fetched guess ("frobnicate → refresh"?)
/// erodes trust in the near ones. Ties resolve to the first in [`VERBS`] order.
fn did_you_mean(input: &str) -> Option<&'static str> {
    let mut best: Option<(usize, &'static str)> = None;
    for verb in VERBS {
        let d = levenshtein(input, verb);
        if best.is_none_or(|(b, _)| d < b) {
            best = Some((d, verb));
        }
    }
    best.filter(|(d, _)| *d <= 2).map(|(_, v)| v)
}

/// Plain O(len_a * len_b) edit distance over chars — the roster is 21 short names,
/// so clarity beats cleverness here.
fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut row = vec![i + 1];
        for (j, cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            row.push((prev[j] + cost).min(prev[j + 1] + 1).min(row[j] + 1));
        }
        prev = row;
    }
    prev[b_chars.len()]
}

/// The whole package-manager CLI as a callable: `argv[1..]` in, exit code
/// out. Served in-process by the ONE `aterm` binary (`aterm pkg …` / the
/// `atpkg` argv0 alias) and by the thin standalone bin. Everything below is
/// unchanged from the binary era.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    let mut args: Vec<String> = argv
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // `--progress-file <path>` — the GUI-only machine-progress opt-in (R5), stripped
    // BEFORE dispatch so no verb ever sees it as an operand. Recognized only on the
    // provisioning verbs; a plain terminal `aterm pkg update` never passes it, so the
    // terminal lanes' stdout is byte-unchanged. The stripped path feeds
    // [`progress_path`], which the provisioning passes consult.
    if matches!(
        args.first().map(String::as_str),
        Some("seed" | "update" | "install")
    ) {
        strip_progress_file_flag(&mut args);
    }
    let verb = args.first().map(String::as_str);
    // The HIDDEN pending-program verb (R6): what a laid stub execs. Deliberately
    // unlisted in help/VERBS (it is machinery, not vocabulary), handled before the
    // dispatch match so the roster-coherence scraper sees only real verbs — and
    // BEFORE the store lock: it only reads progress/status and appends one line to
    // the reorder-only bump file, and it must answer instantly while the installer
    // HOLDS the store lock.
    if verb == Some("__pending") {
        return cmd_pending(args.get(1));
    }
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
    // A FLAG IS NEVER A PROGRAM NAME. The verbs below read `args[1]` as a program and hand
    // it to the signed index, so `atpkg install --help` used to RESOLVE `"--help"`, fail,
    // and persist `[programs.--help] state = "error: --help is not named in the signed
    // index"` into `status.toml` — where nothing ever removed it, so `atpkg doctor` called
    // the toolset incomplete for the life of the machine while all ten real programs sat
    // there `active`. `atpkg uninstall --version` was worse than noisy: it matched nothing,
    // took the clean-success path, cleared the program's adoption, printed "uninstalled
    // --version" and exited 0. Gated at the ONE dispatch edge, and BEFORE the store lock, so
    // a typo never contends for `store.lock` (2026-08-20 round-10 audit).
    if let Some(v) = verb
        && let Some(operand) = args.get(1)
        && operand.starts_with('-')
        && let Some((_, allowed)) = NAME_TAKING_VERBS.iter().find(|(name, _)| *name == v)
        && !allowed.contains(&operand.as_str())
    {
        // A flag straight after a verb is nearly always someone asking how the verb works.
        // Answering is strictly better than refusing the one spelling they guessed.
        if matches!(operand.as_str(), "-h" | "--help") {
            return cmd_help();
        }
        eprintln!("atpkg {v}: {operand:?} is not a program name — it is a flag");
        eprintln!("atpkg:   `aterm pkg {v} --help` shows what {v} accepts");
        return ExitCode::from(2);
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
        // `status` is the name people reach for first — it is what every other package
        // manager calls this — and it answered "unknown verb" while `doctor` sat two lines
        // away. An alias costs nothing and removes a guess from the first minute of use.
        Some("doctor") => return doctor("doctor"),
        // Same report, and the report SIGNS ITSELF with the verb the user typed: every
        // line answered "doctor:" regardless, so a `status` user could not tell which
        // verb they had run and a script keying on the prefix keyed on the wrong verb.
        Some("status") => return doctor("status"),
        Some("which") => return cmd_which(args.get(1)),
        Some("list") => return cmd_list(args.get(1..).unwrap_or(&[])),
        Some("uninstall") => return cmd_uninstall(args.get(1)),
        Some("tree-root") => return cmd_tree_root(args.get(1)),
        Some("verify-index") => return cmd_verify_index(args.get(1..).unwrap_or(&[])),
        Some("verify-pkg") => return cmd_verify_pkg(args.get(1..).unwrap_or(&[])),
        Some("install") => return cmd_install(args.get(1)),
        Some("seed") => return cmd_seed(&args[1..]),
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
            // The head `atpkg: unknown verb '<x>'` is byte-stable for scripts; the
            // suggestion rides after an em dash so a typo's fix is the FIRST thing on
            // screen, and the full tiered roster still prints below — arrangement, not
            // deletion: hiding plumbing verbs from the error would strand the operator
            // who misspells one.
            match did_you_mean(other) {
                Some(verb) => {
                    eprintln!("atpkg: unknown verb '{other}' — did you mean `{verb}`?");
                }
                None => eprintln!("atpkg: unknown verb '{other}'"),
            }
            print_verb_tiers(&mut std::io::stderr());
            eprintln!("atpkg: full manual: aterm help pkg");
            return ExitCode::from(2);
        }
        None => cmd_bare(),
    }
    ExitCode::SUCCESS
}

/// `atpkg help` / `-h` / `--help` — the conventional help surface: usage plus the ONE
/// advertised verb roster (tiered — the same [`print_verb_tiers`] block every other
/// surface prints, so they can never fork), exit 0. The full manual is `aterm help pkg`;
/// this stays a summary so the two never fork. Static — no store reads — so it answers
/// even with HOME unset.
fn cmd_help() -> ExitCode {
    println!("atpkg — the aterm toolchain package manager");
    // The usage line keeps its `atpkg` spelling (that IS this program's name, and the
    // argv0 alias makes it runnable), but a first-hour user arrived via `aterm pkg` —
    // name the spelling they can actually paste.
    println!("usage: atpkg <verb> [args…]   (you type: aterm pkg <verb>)");
    print_verb_tiers(&mut std::io::stdout());
    println!("atpkg: new machine: aterm pkg install --default-set installs the whole ALab toolset");
    println!("full manual: aterm help pkg");
    ExitCode::SUCCESS
}

/// The one-line answer for a program/tool the store does not hold — fact first, then
/// the fix, because a bare "ty is not installed" is a dead end: the user's very next
/// question is ALWAYS "then how do I get it", and the first hour should never need a
/// second guess. `fix:` is the house word for the attached remedy (one spelling
/// everywhere, greppable).
fn not_installed_fix(name: &str) -> String {
    format!("atpkg: {name} is not installed (fix: aterm pkg install {name})")
}

/// The `--progress-file` path this invocation carries, if any — set once at the
/// dispatch edge by [`strip_progress_file_flag`], read by the provisioning passes.
/// Process-global because the pass that consumes it (`install_default_set`) sits
/// several call layers below verbs whose signatures are shared with paths that
/// must never know about it.
static PROGRESS_FILE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Remove every `--progress-file <path>` pair from `args`, recording the LAST path
/// given. Value-missing is tolerated as "no opt-in" rather than an error: the flag
/// is machinery for the GUI spawn, not user vocabulary, and a broken spawn must
/// degrade to a progress-less install, never a refused one.
fn strip_progress_file_flag(args: &mut Vec<String>) {
    while let Some(i) = args.iter().position(|a| a == "--progress-file") {
        args.remove(i);
        if i < args.len() {
            let path = args.remove(i);
            let _ = PROGRESS_FILE.set(std::path::PathBuf::from(path));
        }
    }
}

/// Where this invocation's live progress should land, if the GUI opted in.
fn progress_path() -> Option<&'static std::path::Path> {
    PROGRESS_FILE.get().map(std::path::PathBuf::as_path)
}

/// Mark `program` terminal on the live progress pass, if one is running. The ONE
/// funnel for `done`/`failed`/`skipped` so the overall counter can never double-count
/// a program (flow marks only non-terminal phases).
fn note_finished(program: &str, phase: crate::progress::Phase, error: Option<String>) {
    if let Some(sink) = crate::progress::active() {
        sink.finished(program, phase, error);
    }
}

/// `atpkg __pending <tool>` — the hidden verb a pending-program stub execs (R6).
///
/// Reads `progress.json` + `status.toml` under the UNTRUSTED-reader rules, appends
/// the tool to the reorder-only bump file, prints ONE honest state, and exits 127
/// (the stub's contract: the tool did not run). It never claims progress it cannot
/// read, and never claims a bump the installer would silently drop.
fn cmd_pending(tool: Option<&String>) -> ExitCode {
    let Some(tool) = tool else {
        eprintln!("usage: atpkg __pending <tool>");
        return ExitCode::from(2);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    print_pending_state(&layout, tool);
    // ALWAYS 127: whatever was printed, the command the user typed did not run.
    ExitCode::from(127)
}

/// How long an echoed error line may get before visible elision — generous enough
/// for every honest FlowError, finite against a hostile record.
const PENDING_ERROR_CAP: usize = 300;

/// The four honest states (+ the honesty edge), shared verbatim by `__pending` and
/// `atpkg run`'s pending arm so the three dead ends can never drift apart.
fn print_pending_state(layout: &crate::store::Layout, tool: &str) {
    for line in pending_state_lines(layout, tool) {
        println!("{line}");
    }
}

/// [`print_pending_state`]'s body, returning the lines so the four states are
/// assertable without capturing a process's stdout. SIDE-EFFECTFUL on purpose: the
/// bump append is part of the state machine (a claimed bump must really have been
/// written), so the tests exercise message and channel together.
fn pending_state_lines(layout: &crate::store::Layout, tool: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // The name gate first: only an admitted ToolName is ever echoed back to the TTY
    // (an unadmitted one could never have a stub OR a shim, so nothing pends).
    let Some(tn) = crate::store::ToolName::new(tool) else {
        out.push("atpkg: that is not an installable program name".to_string());
        return out;
    };
    let name = tn.as_str();
    match crate::stub::describe(name) {
        Some(what) => out.push(format!("atpkg: {name}: {what} — not installed yet.")),
        None => out.push(format!(
            "atpkg: {name}: part of the ALab toolset — not installed yet."
        )),
    }
    let snapshot = crate::progress::read_progress(layout);
    let now = crate::flow::now_unix().max(0).unsigned_abs();
    let running = snapshot
        .as_ref()
        .is_some_and(|f| crate::progress::snapshot_running(f, now));
    if let Some(file) = snapshot.as_ref().filter(|_| running) {
        if file.v != crate::progress::PROGRESS_VERSION {
            // A NEWER writer's file: render the generic line, never a guess at
            // fields whose meaning may have changed. The bump file's contract is
            // versionless (names only), so the bump is still honest.
            out.push(format!(
                "atpkg: aterm is installing the toolset now — re-run {name} shortly."
            ));
            let _ = crate::progress::append_bump(layout, &tn);
            return out;
        }
        let overall = &file.overall;
        out.push(format!(
            "atpkg: aterm is installing your toolchain now ({} of {} done).",
            overall.programs_done, overall.programs_total
        ));
        let Some(row) = file.programs.get(name) else {
            // THE HONESTY EDGE: a resolved-pass snapshot is readable and does not
            // plan this program — a compiled-roster stub the signed index no longer
            // lists. Claiming "bumped" would be a lie the reorder-only intersection
            // silently swallows; the reconcile removes this stub at pass end.
            out.push(format!(
                "atpkg: {name} is no longer part of the default set — nothing is \
                 scheduled to install it. `aterm pkg install {name}` tries it \
                 individually."
            ));
            return out;
        };
        use crate::progress::Phase;
        match row.phase {
            Phase::Download => {
                if row.bytes_total > 0 {
                    out.push(format!(
                        "atpkg: {name} is downloading NOW — {:.1} of {:.1} MB ({}%). \
                         Re-run {name} when it lands.",
                        row.bytes_done as f64 / 1e6,
                        row.bytes_total as f64 / 1e6,
                        row.bytes_done.saturating_mul(100) / row.bytes_total.max(1)
                    ));
                } else {
                    out.push(format!(
                        "atpkg: {name} is downloading NOW — re-run it when it lands."
                    ));
                }
            }
            Phase::Verify | Phase::Extract | Phase::Link => {
                out.push(format!(
                    "atpkg: {name} is installing NOW ({}) — re-run it in a moment.",
                    match row.phase {
                        Phase::Verify => "verifying",
                        Phase::Extract => "extracting",
                        _ => "activating",
                    }
                ));
            }
            Phase::Done => {
                out.push(format!(
                    "atpkg: {name} just finished installing — re-run it now."
                ));
            }
            Phase::Failed => {
                let why = row.error.as_deref().unwrap_or("no recorded reason");
                out.push(format!(
                    "atpkg: {name} FAILED to install this pass: {} — fix: aterm pkg \
                     update retries it; Settings ▸ Packages shows details.",
                    crate::progress::sanitize_for_tty(why, PENDING_ERROR_CAP)
                ));
            }
            Phase::Queued | Phase::Skipped => {
                let position = file.queue.iter().position(|q| q == name);
                let current = file.queue.first().filter(|c| c.as_str() != name);
                let bumped = crate::progress::append_bump(layout, &tn).is_ok();
                match (position, current, bumped) {
                    (Some(p), Some(cur), true) => {
                        // `cur` came off disk: it is echoed only after its own
                        // ToolName round-trip (queue names are untrusted).
                        let cur = crate::store::ToolName::new(cur)
                            .map_or_else(|| "the current program".to_string(), |t| {
                                t.as_str().to_string()
                            });
                        out.push(format!(
                            "atpkg: {name} was queued {} of {}; it is now BUMPED to \
                             install next, after {cur} finishes. Re-run it in a minute.",
                            p + 1,
                            file.queue.len()
                        ));
                    }
                    (_, _, true) => {
                        out.push(format!(
                            "atpkg: {name} is queued and now BUMPED to install next — \
                             re-run it in a minute."
                        ));
                    }
                    _ => {
                        out.push(format!(
                            "atpkg: {name} is queued — re-run it once the pass reaches it."
                        ));
                    }
                }
            }
        }
        return out;
    }
    // NOT RUNNING (no file, stale heartbeat, dead pid, or a terminal snapshot):
    // only not-running states may render, whatever the snapshot claims. A recorded
    // failure outranks the generic line — every failure names its next act.
    if let Some(status) = crate::status::read(layout) {
        if let Some(row) = status.programs.get(name)
            && row.state.starts_with("error")
        {
            out.push(format!(
                "atpkg: the last install attempt FAILED: {} — fix: aterm pkg update \
                 retries it; Settings ▸ Packages shows details.",
                crate::progress::sanitize_for_tty(&row.state, PENDING_ERROR_CAP)
            ));
            let _ = crate::progress::append_bump(layout, &tn);
            return out;
        }
        // A BLOCKED toolset outranks the generic promise: "open aterm, it
        // provisions" is a lie when the recorded `*toolset*` row says the
        // registry publishes no build for this machine (or the disk cannot
        // hold one) — the reconcile pass writes that row precisely so this
        // stub, which may outlive the pass by months, can tell the truth.
        if let Some(row) = status.programs.get("*toolset*")
            && let Some(why) = row.state.strip_prefix("blocked: ")
        {
            out.push(format!(
                "atpkg: {name} is not coming on its own: {} — Settings ▸ Packages \
                 shows details.",
                crate::progress::sanitize_for_tty(why, PENDING_ERROR_CAP)
            ));
            // Still worth a bump: for the disk-blocked case the user may just
            // have freed space, and the GUI's bump watch turns this wish into
            // an immediate pass; an unserved-triple pass re-skips quietly.
            let _ = crate::progress::append_bump(layout, &tn);
            return out;
        }
    }
    out.push(
        "atpkg: nothing is installing right now — fix: open aterm (it provisions the \
         toolset on launch), or run: aterm pkg update"
            .to_string(),
    );
    let _ = crate::progress::append_bump(layout, &tn);
    out
}

/// After an [`crate::FlowError::Unreachable`] failure line, name the ONE act that
/// resumes the flow — because the Display used to end "…the toolchain retries
/// automatically", which is true only inside the windowed app's 6-hour loop and a
/// plain lie at this CLI edge (nothing retries a foreground verb). The re-run is
/// per-verb, so the edge that knows the verb appends it; and when the reason is the
/// anonymous GitHub budget, waiting is not the only fix, so that one case names the
/// token that lifts it.
fn print_unreachable_followup(e: &crate::FlowError, rerun: &str) {
    let crate::FlowError::Unreachable(why) = e else {
        return;
    };
    eprintln!("atpkg:   when the connection is back, re-run: {rerun}");
    if why.contains("403") || why.to_ascii_lowercase().contains("rate limit") {
        eprintln!(
            "atpkg:   the anonymous GitHub limit resets within the hour; gh auth login \
             provisions a token that lifts it"
        );
    }
}

/// The chain-validated `[packages].prefix` override, or `None` for the default.
/// Threaded through `store::resolve` so the override is REACHABLE: both call sites
/// previously passed a hardcoded `None`, which made `vet_prefix`'s whole
/// configured-prefix branch dead code and pinned every install to `$HOME`.
///
/// (That was a real bug, but NOT for the reason once recorded here: `$HOME` does not
/// mean "the unverified lane". Trust's default `CallerOwned` authority mode admits a
/// toolchain owned by the invoking identity, so the default prefix proves fine —
/// see `docs/GOLDEN-INSTALL-PATH.md` §2. The override matters for a shared
/// multi-user store.)
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
/// `--default-set` —, `seed`, `update`, `rollback`, `uninstall`, `gc`), every
/// link-mutating verb (`link`, `unlink`, `refresh` — which also covers the
/// `[packages.links]` reconciliation the network verbs run), and `pin`/`unpin`:
/// pins are LOCAL state files, but they gate the coherence-group transaction and
/// their read-modify-write of the pin set is itself check-then-act, so they take
/// the same lock. Everything else (`doctor`, `which`, `list`, `run`, `verify`,
/// `tree-root`, `verify-index`, `relocate`, bare status) reads only — lock-free,
/// and `run` in particular may exec a long-lived tool that must never hold it.
/// The verbs whose FIRST positional is a program name, each paired with the flag-shaped
/// literals it legitimately accepts in that slot.
///
/// Exactly two such literals exist across the whole surface — `install --default-set` and
/// `uninstall --all` — and both are real operands, not flags: they name a SET where a
/// program name would go. Everything else beginning with `-` is a mistyped flag.
///
/// `run` is deliberately absent. `aterm <tool>` synthesizes `["run", <tool>, "--", …]`, so
/// `cmd_run` owns its own `--` handling, and its failure mode is already non-durable (exit
/// 127, no `status.toml` write) — there is nothing here to protect and a dispatcher to
/// avoid disturbing.
const NAME_TAKING_VERBS: &[(&str, &[&str])] = &[
    ("install", &["--default-set"]),
    ("uninstall", &["--all"]),
    ("update", &[]),
    ("rollback", &[]),
    ("which", &[]),
    ("tree-root", &[]),
    ("verify", &[]),
    ("pin", &[]),
    ("unpin", &[]),
    ("link", &[]),
    ("unlink", &[]),
    ("refresh", &[]),
];

fn verb_mutates_store(verb: &str) -> bool {
    matches!(
        verb,
        "install"
            | "seed"
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

/// `atpkg` (no verb) — the inert/enabled posture, observable from the shell, answering
/// a first hour's four questions in order: what is this (the head), what do I have (the
/// counts), what can I type (the tiers), and the ONE next command. Read-only and
/// lock-free — `list_installed`/`active_builds` read the store without the writer lock,
/// so the bare invocation's classification in `store_lock_verb_roster_is_exact` holds.
///
/// The stable head `atpkg: enabled (root key pinned)` and the byte-exact disabled line
/// are the script-facing contract; the counts ride after an em dash on the same line so
/// nothing keying on the head moves. (The old `. Verbs: …` tail is gone — the roster
/// now prints tiered below, on lines a script was never keying on.)
fn cmd_bare() {
    if !manager_enabled() {
        println!("atpkg: disabled (no root key pinned) -- inert");
        return;
    }
    // HOME unset: the head still answers (posture is a fact about the BINARY), with no
    // counts to claim and no next act to name — a next command would fail in the same
    // broken environment that emptied the tail.
    let counts = crate::store::resolve(configured_prefix().as_deref()).map(|layout| {
        let programs: std::collections::BTreeSet<String> = crate::list_installed(&layout)
            .into_iter()
            .map(|(program, _)| program)
            .collect();
        (programs.len(), crate::active_builds(&layout).len())
    });
    for line in bare_lines(counts) {
        println!("{line}");
    }
}

/// The bare posture's lines for an ENABLED manager — pure, so the head's stability and
/// the one-next-command rule are testable without a process.
fn bare_lines(counts: Option<(usize, usize)>) -> Vec<String> {
    let mut lines = vec![match counts {
        Some((0, _)) => "atpkg: enabled (root key pinned) — nothing installed yet".to_string(),
        Some((programs, live)) => format!(
            "atpkg: enabled (root key pinned) — {programs} program(s) installed, {live} live"
        ),
        None => "atpkg: enabled (root key pinned)".to_string(),
    }];
    lines.extend(verb_tier_lines());
    match counts {
        // Empty store: the one next act is the install, and it says what it does.
        Some((0, _)) => lines.push(
            "atpkg: next: aterm pkg install --default-set — installs the whole ALab \
             toolset · manual: aterm help pkg"
                .to_string(),
        ),
        Some(_) => lines.push(
            "atpkg: next: aterm pkg list — what you have · full manual: aterm help pkg"
                .to_string(),
        ),
        None => {}
    }
    lines
}

/// `atpkg doctor` — the full health surface (§15): trust root, PATH wiring, broken-symlink
/// scan, active-build store integrity, shell.d hooks + fish-safety, disk headroom, index
/// freshness, and rustup state. No network, no mutation. Structural breakage exits
/// nonzero; advisory warnings stay exit-0.
fn doctor(prefix: &str) -> ExitCode {
    let Some(layout) = layout() else {
        return ExitCode::from(1); // HOME unset is itself structural
    };
    if crate::doctor::run(&layout, prefix) {
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
            eprintln!("{}", not_installed_fix(tool));
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
        // The third dead end converges on the pending message (R6): when the name is
        // a laid pending stub, `atpkg run <tool>` gets the same live state + bump the
        // stub itself prints — not a static "not installed" over a tool that is
        // literally downloading right now. A non-pending name keeps the classic line.
        if crate::stub::pending_stub_exists(&layout, tool) {
            print_pending_state(&layout, tool);
        } else {
            eprintln!("{}", not_installed_fix(tool));
        }
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
///
/// TWO AUDIENCES, TWO SHAPES, ONE SET OF FACTS. A terminal gets the human report:
/// verdict first (`atpkg: N program(s) — L live`), then one row per PROGRAM with its
/// live build, because the most common question here — "what am I running, is it
/// healthy?" — used to take twenty rows of raw TSV printed superseded-first. A pipe
/// (or `--porcelain`, for a script that runs on a TTY) gets the original headerless
/// `program\tbuild\tnotes` rows BYTE-IDENTICAL to what every existing parser keys on:
/// ascending builds, superseded before live, `atpkg: no programs installed` when empty.
///
/// The only accepted flag is `--porcelain`; anything else is a usage error (extra args
/// used to be silently ignored, which is a meaning nobody asked for).
fn cmd_list(args: &[String]) -> ExitCode {
    use std::io::IsTerminal as _;
    let mut porcelain = false;
    for a in args {
        match a.as_str() {
            "--porcelain" => porcelain = true,
            _ => {
                eprintln!("usage: atpkg list [--porcelain]");
                return ExitCode::from(2);
            }
        }
    }
    let human = !porcelain && std::io::stdout().is_terminal();
    run_list(layout(), human)
}

/// [`cmd_list`] under an already-resolved layout — split so the exit codes and both
/// renderings are exercisable against a temp store without a process (and without a TTY).
fn run_list(layout: Option<crate::store::Layout>, human: bool) -> ExitCode {
    // Exit 1, like every sibling read verb (`which`, `doctor`, `verify`): `layout()`
    // already printed why. This used to return success with an error on stderr — a
    // silent green in exactly the environments that are already broken.
    let Some(layout) = layout else {
        return ExitCode::from(1);
    };
    let installed = crate::list_installed(&layout);
    if installed.is_empty() {
        if human {
            println!("{EMPTY_LIST_HUMAN}");
        } else {
            // The stable piped form — byte-identical to every earlier release.
            println!("atpkg: no programs installed");
        }
        return ExitCode::SUCCESS;
    }
    let live = crate::active_builds(&layout);
    let mut linked: std::collections::BTreeMap<String, Option<std::path::PathBuf>> =
        std::collections::BTreeMap::new();
    let mut pinned: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (program, _) in &installed {
        if !linked.contains_key(program) && crate::linkmode::is_linked(&layout, program) {
            linked.insert(program.clone(), crate::linkmode::linked_checkout(&layout, program));
        }
        if crate::pin::is_pinned(&layout, program) {
            pinned.insert(program.clone());
        }
    }
    let lines = if human {
        list_human_lines(&installed, &live, &linked, &pinned)
    } else {
        list_porcelain_lines(&installed, &live, &linked, &pinned)
    };
    for line in lines {
        println!("{line}");
    }
    ExitCode::SUCCESS
}

/// The empty store's human answer — the fact and, on the same line, the one command
/// that changes it (the no-dead-ends rule).
const EMPTY_LIST_HUMAN: &str =
    "atpkg: no programs installed — aterm pkg install --default-set installs the ALab toolset";

/// `update` on an empty, unadopted store — the fact keeps its stable head
/// (`atpkg: nothing installed to update`), the remedy rides after the em dash.
const EMPTY_UPDATE: &str = "atpkg: nothing installed to update — aterm pkg install \
                            --default-set installs the ALab toolset";

/// `verify` over an empty store — nothing was attested because nothing is installed,
/// and saying only that is a dead end; the order of acts matters (install, THEN verify).
const EMPTY_VERIFY: &str = "atpkg: nothing installed to verify — install first (aterm pkg \
                            install --default-set), then verify re-attests every program \
                            against the signed root";

/// The STABLE script form: one row per installed BUILD, `program\tbuild\tnotes`,
/// headerless, ascending builds (superseded rows before the live one) — byte-identical
/// to what `atpkg list` always printed, because pipes elsewhere key on it.
///
/// WHAT AM I ACTUALLY RUNNING. `list` used to print `<program>\t<build>` and nothing
/// else, which cannot answer that — the store legitimately holds MORE THAN ONE build per
/// program (retention is live + 1 rollback). The three facts that change what runs are
/// which build is LIVE, whether a dev link has taken the program over, and whether a pin
/// is holding it back; each comes from the authority that owns it (`active_builds` reads
/// the shims, the link marker, the pin state).
fn list_porcelain_lines(
    installed: &[(String, u64)],
    live: &std::collections::BTreeMap<String, u64>,
    linked: &std::collections::BTreeMap<String, Option<std::path::PathBuf>>,
    pinned: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let mut shown_link = std::collections::BTreeSet::new();
    let mut lines = Vec::new();
    for (program, build) in installed {
        let mut notes: Vec<String> = Vec::new();
        if let Some(checkout) = linked.get(program) {
            // A dev link supersedes EVERY store build, so the note belongs on the program,
            // not on one row; print it once and never call a store build "live" while it is
            // shadowed — that was the misleading part.
            if shown_link.insert(program.clone()) {
                match checkout {
                    Some(path) => notes.push(format!("DEV-LINKED -> {}", path.display())),
                    None => notes.push("DEV-LINKED".to_string()),
                }
            } else {
                notes.push("(shadowed by the dev link)".to_string());
            }
        } else if live.get(program) == Some(build) {
            notes.push("live".to_string());
        } else {
            notes.push("superseded (kept for rollback)".to_string());
        }
        if pinned.contains(program) {
            notes.push("pinned".to_string());
        }
        lines.push(format!("{program}\t{build}\t{}", notes.join("  ")));
    }
    lines
}

/// The human report: a count summary FIRST (verdict before detail — the grid rule),
/// then one aligned row per PROGRAM in its live state, with superseded builds folded
/// into a trailing "(N older build(s) kept for rollback)" — the fact survives, the
/// twenty-row scan does not. No health adjective anywhere: counts are what the shims
/// prove; "healthy" is doctor's word, earned by running checks.
fn list_human_lines(
    installed: &[(String, u64)],
    live: &std::collections::BTreeMap<String, u64>,
    linked: &std::collections::BTreeMap<String, Option<std::path::PathBuf>>,
    pinned: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    // Group the per-build rows per program, preserving list order (ascending builds).
    let mut builds: std::collections::BTreeMap<&str, Vec<u64>> = std::collections::BTreeMap::new();
    for (program, build) in installed {
        builds.entry(program).or_default().push(*build);
    }
    let name_width = builds.keys().map(|p| p.len()).max().unwrap_or(0);
    let build_width = installed
        .iter()
        .map(|(_, b)| b.to_string().len())
        .max()
        .unwrap_or(0);
    let mut live_count = 0usize;
    let mut no_live = 0usize;
    let mut rows = Vec::new();
    for (program, program_builds) in &builds {
        let is_pinned = pinned.contains(*program);
        let state = if let Some(checkout) = linked.get(*program) {
            let mut s = match checkout {
                Some(path) => format!("DEV-LINKED -> {}", path.display()),
                None => "DEV-LINKED".to_string(),
            };
            s.push_str(&format!(
                "  ({} store build(s) shadowed)",
                program_builds.len()
            ));
            if is_pinned {
                s.push_str(", pinned (updates held)");
            }
            s
        } else if let Some(live_build) = live.get(*program) {
            live_count += 1;
            let mut s = String::from("live");
            if is_pinned {
                s.push_str(", pinned (updates held)");
            }
            let older = program_builds.iter().filter(|b| *b != live_build).count();
            if older > 0 {
                s.push_str(&format!("   ({older} older build(s) kept for rollback)"));
            }
            s
        } else {
            // Loud and uppercase, like the porcelain DEV-LINKED marker: a program with
            // builds on disk and nothing on PATH is the one row that needs doctor.
            no_live += 1;
            format!("NO LIVE BUILD ({} build(s) on disk)", program_builds.len())
        };
        let shown_build = if linked.contains_key(*program) {
            // Shadowed: the store build number would imply it is what runs.
            "-".to_string()
        } else {
            live.get(*program)
                .or_else(|| program_builds.last())
                .map(u64::to_string)
                .unwrap_or_default()
        };
        rows.push(format!(
            "  {program:<name_width$}  {shown_build:>build_width$}  {state}"
        ));
    }
    // The summary, zero-count categories omitted — "10 program(s) — 10 live" is the
    // whole story on a healthy machine, and every extra clause would dilute the one
    // that matters on an unhealthy one.
    let mut clauses = vec![format!("{live_count} live")];
    if !pinned.is_empty() {
        clauses.push(format!("{} pinned", pinned.len()));
    }
    if !linked.is_empty() {
        clauses.push(format!("{} dev-linked", linked.len()));
    }
    if no_live > 0 {
        clauses.push(format!("{no_live} without a live build"));
    }
    let mut lines = vec![format!(
        "atpkg: {} program(s) — {}",
        builds.len(),
        clauses.join(", ")
    )];
    lines.extend(rows);
    if no_live > 0 {
        // The one next act, only when a row needs explaining — doctor names the why.
        lines.push("atpkg: next — aterm pkg doctor".to_string());
    }
    lines
}

/// `atpkg uninstall <program>` — remove its shims + store builds (fail-closed inside the prefix).
/// Remove `program` from the store and retire everything this machine recorded about it.
///
/// Split out of [`cmd_uninstall`] so the bookkeeping is exercisable without a process — the
/// CLI arm keeps only its printing and its exit code. Both steps are part of "uninstalled",
/// and neither happened before:
///
/// * THE STATUS ROW. `active_builds` reads shims, so `doctor` went quiet after a removal —
///   but Settings ▸ Packages reads the RECORD, and listed a deleted program as `active` at
///   its last build forever. The stale-row reconciler cannot help and correctly declines
///   to: the signed index still names this program, it is this MACHINE that no longer
///   carries it.
/// * THE REMOVED MARKER, which already worked: without it the resumable seed lane
///   reinstalls the program on the next launch — the manager undoing a deliberate act.
fn uninstall_and_retire(layout: &crate::store::Layout, program: &str) -> std::io::Result<()> {
    // ROW DELETION IS A NEW CAPABILITY, so it gets the name gate the old code never needed.
    // `ops::uninstall`'s shape rule rejects only empty/`.`/`..`/separators/NUL, so without
    // this `atpkg uninstall '*toolset*'` returned Ok and DELETED a bookkeeping row that has
    // its own owner and its own reaper, and `atpkg uninstall --help` appended a flag to the
    // removed-markers file. Neither is a program; neither may be retired through here.
    if (program.starts_with('*') && program.ends_with('*')) || program.starts_with('-') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{program:?} is not a program name"),
        ));
    }
    crate::uninstall(layout, program)?;
    clear_status_row(layout, program);
    record_removed(layout, program);
    Ok(())
}

fn cmd_uninstall(program: Option<&String>) -> ExitCode {
    let Some(program) = program else {
        eprintln!("usage: atpkg uninstall <program> | --all");
        return ExitCode::from(2);
    };
    if program == "--all" {
        return cmd_uninstall_all();
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    uninstall_one(&layout, program)
}

/// Whether anything on this machine still bears `program`'s name: a store tree (even a
/// partial one an interrupted install left) or a bin shim resolving into it — the same
/// two places `ops::uninstall` cleans. The record row is deliberately NOT consulted:
/// a row is a claim about the store, not the store.
fn program_present(layout: &crate::store::Layout, program: &str) -> bool {
    let prog_store = layout.prefix.join("store").join(program);
    if prog_store.exists() {
        return true;
    }
    if let Ok(entries) = std::fs::read_dir(layout.bin_dir()) {
        for e in entries.flatten() {
            if let Some(target) = crate::platform::resolve_shim(&e.path())
                && target.starts_with(&prog_store)
            {
                return true;
            }
        }
    }
    false
}

/// [`cmd_uninstall`]'s single-program body, split from the layout resolution so the
/// refusal below is exercisable against a temp store.
fn uninstall_one(layout: &crate::store::Layout, program: &str) -> ExitCode {
    // REFUSE TO "UNINSTALL" WHAT WAS NEVER THERE. This used to print "uninstalled ty",
    // exit 0, and mint a durable removed-marker — a fabricated success whose hidden
    // state change silently suppressed future set-completion for a program the user
    // never had. Only program-shaped names take this path: a sentinel/flag name still
    // reaches the retirement gate's own refusal, which owns those words.
    if !((program.starts_with('*') && program.ends_with('*')) || program.starts_with('-'))
        && !program_present(layout, program)
    {
        eprintln!("atpkg: {program} is not installed — nothing to uninstall");
        return ExitCode::from(1);
    }
    match uninstall_and_retire(layout, program) {
        Ok(()) => {
            // A removed program's pending stub goes with it: `removed` suppresses the
            // reinstall, so a stub promising "installing" would be a standing lie.
            crate::stub::remove_stub(layout, program);
            println!("atpkg: uninstalled {program}");
            // Removing a managed program is an EXPLICIT act, so this machine stops being
            // one that keeps the whole set complete — otherwise the next unattended pass
            // would reinstall what the user just removed, which is the manager fighting
            // its owner. Adoption is re-established by the deliberate whole-set act
            // (Settings ▸ Packages ▸ Install ALab toolset), and the way to drop ONE
            // program while staying adopted is `[packages].exclude`.
            if adopted(layout) {
                clear_adoption(layout);
                println!(
                    "atpkg: this machine no longer auto-completes the ALab toolset (removing \
                     a program opts out). Re-adopt with `aterm pkg install --default-set`, or \
                     keep the set and drop just this one with [packages].exclude in aterm.toml"
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: uninstall {program} failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// `atpkg uninstall --all` — remove the WHOLE managed toolset and reclaim its disk.
///
/// The batteries-included install lays down ~3.2 GB without asking (§9.1 — the bytes
/// shipped inside the app, so installing the app is the consent). A user who decides
/// they do not want it must have a way OUT that is as single-step as the way in;
/// making them run `uninstall` eight times, once per program they never chose
/// individually, is the kind of asymmetry that earns a product a reputation.
///
/// Composed from the tested per-program primitive plus a GC sweep rather than an
/// `rm -rf` of the prefix: `[packages].prefix` is user-configurable and chain-vetted,
/// and a recursive delete of a path from config is exactly the operation that must
/// never exist. Adoption is cleared last, so the 6h pass does not put it all back.
fn cmd_uninstall_all() -> ExitCode {
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let installed = crate::active_builds(&layout);
    // THE SWEEP IS THE UNION OF WHAT IS LIVE AND WHAT THE RECORD STILL CLAIMS. A TOMBSTONED
    // program is invisible to `active_builds` — its shims were replaced by failing stubs —
    // so walking the live set alone stepped straight past it, leaving its build tree on disk
    // and its row in the record after an explicit whole-set removal. That is the exact class
    // this verb exists to clear. Sentinel bookkeeping rows and stray flag rows are excluded:
    // neither is a program, and neither is this verb's to retire.
    let mut targets: std::collections::BTreeSet<String> = installed.keys().cloned().collect();
    if let Some(s) = crate::status::read(&layout) {
        targets.extend(
            s.programs
                .keys()
                .filter(|k| !(k.starts_with('*') && k.ends_with('*')) && !k.starts_with('-'))
                .cloned(),
        );
    }
    if targets.is_empty() {
        // RECORD THE DECLINE ANYWAY. "Remove the ALab toolset" states an INTENT —
        // "I do not want this on my machine" — and that intent is independent of
        // whether anything happens to be installed at this instant. Returning early
        // made the documented exit door a silent no-op in exactly the cases that
        // need it most: an Intel/lean Mac (where the store is ALWAYS empty, so the
        // button could never do anything), and a launch during which the seed pass
        // has not run yet. The user saw "ALab toolset removed", and the next launch
        // re-adopted the machine and installed the whole set.
        clear_adoption(&layout);
        record_decline(&layout);
        // The decline extends to the pending stubs: a declined machine keeps NO
        // default-set names on PATH promising an install that will never come.
        crate::stub::remove_all_stubs(&layout);
        println!(
            "atpkg: removed nothing (nothing installed) — noted that this machine declines \
             the ALab toolset: no later pass installs it (not the first-run seed, not the \
             unattended update, not set auto-completion); aterm pkg install --default-set \
             opts back in"
        );
        return ExitCode::SUCCESS;
    }
    let mut failures = 0u32;
    let mut removed: Vec<String> = Vec::new();
    for program in &targets {
        match crate::uninstall(&layout, program) {
            Ok(()) => {
                // Same retirement as the single-program verb: a row that outlives its
                // program is a claim about a machine state that no longer exists.
                //
                // Only on success — but note what an `Err` here actually means. `uninstall`
                // removes the shims FIRST and unconditionally; its only fallible step is the
                // final `remove_dir_all`. So a failure leaves the program already OFF PATH
                // with its build tree behind, and keeping the row is the conservative choice
                // precisely because the machine is now in a half-state worth reporting.
                clear_status_row(&layout, program);
                removed.push(program.clone());
            }
            Err(e) => {
                failures += 1;
                eprintln!("atpkg: uninstall {program} failed: {e}");
            }
        }
    }
    // Reclaim the now-unreferenced build trees — this is where the gigabytes
    // actually go back, and reporting it is the point of the verb.
    let report = crate::gc::run(&layout);
    print_gc_sweeps("uninstall --all", &report);
    print_gc_abstentions("uninstall --all", &report);
    crate::hooks::refresh(&layout);
    // Last, and only after the removals: this machine no longer runs the set, so
    // set-completion must not reinstate it on the next unattended pass — and the
    // SEED lane must not either. The seed installs whatever the store lacks (that
    // is what makes an interrupted first run resumable), so without a durable
    // decline the next launch would cheerfully restore all 3.2 GB the user just
    // removed.
    clear_adoption(&layout);
    record_decline(&layout);
    // Same stub discipline as the empty-store branch: `uninstall --all` removes
    // every pending stub with the toolset it declines.
    crate::stub::remove_all_stubs(&layout);
    if removed.is_empty() {
        eprintln!("atpkg: removed nothing");
        return ExitCode::from(1);
    }
    removed.sort();
    println!("atpkg: removed {}", removed.join(", "));
    println!(
        "atpkg: the ALab toolset is no longer managed here. `aterm pkg install --default-set` \
         puts it back; the app itself is untouched."
    );
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
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
         run `aterm pkg doctor` for the reason",
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
    // Say it, because the whole point is that this space used to vanish silently:
    // a killed download stranded a multi-hundred-MB archive in `staging/` that
    // NOTHING swept, while `gc` reported "nothing to reclaim".
    for (p, names) in &report.swept_staging {
        println!(
            "atpkg {verb}: swept {p} interrupted download(s) {} — a killed transfer left              the compressed archive behind",
            names.join(", ")
        );
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

/// Re-record the signed `tree_root` for any program that is LIVE but unattested.
///
/// See the call site for the window that produces one. Silent and best-effort in
/// both directions: a program whose root cannot be re-derived is left exactly as it
/// was — fail-closed — and nothing here can fail an update.
fn recover_missing_roots(layout: &crate::store::Layout, fetcher: &dyn crate::flow::Fetcher) {
    let live = crate::active_builds(layout);
    if live.is_empty() {
        return;
    }
    let recorded = crate::status::read(layout)
        .map(|s| s.programs)
        .unwrap_or_default();
    let unattested: Vec<(String, u64)> = live
        .iter()
        .filter(|(program, _)| {
            recorded
                .get(program.as_str())
                .is_none_or(|row| row.tree_root.is_empty())
        })
        .map(|(program, build)| (program.clone(), *build))
        .collect();
    // Cheap pre-filter for the SECOND job below: is there any row that could turn out to
    // be stale? Deciding this before the index resolve keeps the common case — a healthy
    // store with nothing to do — exactly as free as it was.
    let maybe_stale = recorded
        .keys()
        .any(|k| !(k.starts_with('*') && k.ends_with('*')) && !live.contains_key(k.as_str()));
    if unattested.is_empty() && !maybe_stale {
        return;
    }
    let Ok(index) = crate::resolve_verified_index(
        fetcher,
        layout,
        &effective_anchor(layout),
        build_floor(layout),
        now_unix(),
    ) else {
        return;
    };
    // RECONCILE THE MEMO AGAINST ITS AUTHORITY. A row naming something the verified index
    // does not name and the store does not carry is a stray: a mistyped `atpkg install`
    // used to mint one permanently, and doctor then reported a healthy toolset as
    // incomplete for the life of the machine. Announced rather than silent — a manager
    // that quietly edits its own record is the thing this crate exists not to be.
    for stale in prunable_status_rows(&recorded, &live, &|n| index.is_program(n)) {
        println!("atpkg: cleared a stale record for {stale:?} — the signed index does not name it");
        clear_status_row(layout, &stale);
    }
    for (program, build) in unattested {
        let Some(root) = crate::flow::signed_root_for_installed(
            fetcher,
            &index,
            &program,
            build,
            current_triple(),
        ) else {
            continue;
        };
        println!("atpkg: {program} was installed without its signed attestation — recorded");
        record_status(
            layout,
            &program,
            crate::ProgramStatus {
                installed_build: Some(build),
                state: String::from("active"),
                tree_root: root,
            },
            format!("recovered the signed attestation for {program}"),
        );
    }
}

/// The durable `state` to record for a failed install — or `None` when the failure is a
/// fact about the REQUEST rather than about this machine's store.
///
/// [`crate::FlowError::NotReachable`] means the signed index resolved, verified, and does
/// not name the token. Nothing was ever installed under that name and nothing ever will
/// be, so there is nothing for `status.toml` to remember: writing a row there INVENTS a
/// program, and every later `atpkg doctor` reads it back as a missing member of the
/// toolset. The attempt still reaches the aggregate outcome sentence and `cmd_install`
/// still prints it and still exits 1 — the failure is not swallowed, it is just not
/// promoted to a permanent resident of the store's record.
///
/// The OFFLINE FAMILY is suppressed for the same reason, and it is the larger leak of the
/// two. `NoIndex`, `Unreachable` and `Stale` are all raised inside `resolve_verified_index`
/// (flow.rs:1327/1330/1430) — strictly BEFORE the program lookup at flow.rs:384 — so the
/// run never reached a question about this program and learned nothing about it. Recording
/// "error: no signature-valid index" against `ay` says the program is broken; what actually
/// happened is that a laptop was on a plane.
///
/// The damage was durable and asymmetric. A default-set member heals on the next update
/// (§11 reinstalls it), but a program the user asked for BY NAME while offline is not
/// swept by anything: it is not live, so `update` skips it, and it IS named by the index,
/// so the stale-row reconciler correctly declines to prune it. Its row sat there reading
/// `error:` forever, on a machine that had been online and healthy for months.
///
/// Nothing is lost by suppressing it. The reason still reaches the aggregate outcome
/// sentence (which Settings ▸ Packages prints and `doctor` now falls back to), `cmd_install`
/// still prints it live and still exits 1, and the environmental condition itself is
/// doctor's own business — it independently reports index age and days-since-update, which
/// is where "this machine cannot reach the channel" belongs. What is removed is only the
/// false claim that a specific program is at fault.
fn failed_install_state(e: &crate::FlowError) -> Option<String> {
    match e {
        // A dev-linked program is a benign HARD-SKIP (§13), not an error state.
        crate::FlowError::Linked(_) => Some(String::from("dev-linked (skipped)")),
        // Verdicts about the REQUEST or the NETWORK — never about this program.
        crate::FlowError::NotReachable(..)
        | crate::FlowError::NoIndex
        | crate::FlowError::Unreachable(_)
        | crate::FlowError::Stale => None,
        _ => Some(format!("error: {e}")),
    }
}

/// The recorded rows naming nothing the signed index knows — the ones safe to drop.
///
/// `status.toml` is a DERIVED memo and the signed index is the authority on what programs
/// exist, so reconciling the memo against its authority at the moment that authority
/// speaks is the memo doing its job, not a destructive edit. Four conditions, all
/// required:
///
/// * the caller holds a VERIFIED index — never prune on a network error, because
///   inventing an absence is the same class of mistake as inventing a fault;
/// * the key is not a `*sentinel*` bookkeeping row (`*index*`, `*toolset*`, `*seed*` have
///   their own owners and their own reapers);
/// * the index does not name it;
/// * it is not LIVE. A program dropped upstream while still installed keeps its row:
///   [`crate::verify`] fails closed on an empty recorded root, so deleting a live
///   program's row would erase the attestation `atpkg verify` needs.
fn prunable_status_rows(
    recorded: &std::collections::BTreeMap<String, crate::ProgramStatus>,
    live: &std::collections::BTreeMap<String, u64>,
    known: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    recorded
        .keys()
        .filter(|k| !(k.starts_with('*') && k.ends_with('*')))
        .filter(|k| !live.contains_key(k.as_str()))
        .filter(|k| !known(k))
        .cloned()
        .collect()
}

/// Replace the aggregate `status.outcome` sentence without touching per-program rows.
///
/// The counterpart to [`clear_status_row`] for the line Settings ▸ Packages actually
/// shows: retiring a failure row while leaving its sentence behind just makes the
/// staleness quieter (2026-08-20 round-8 audit).
fn record_outcome(layout: &crate::store::Layout, outcome: String) {
    // CREATE the record if it does not exist yet, rather than returning silently. This
    // used to early-return on a virgin store, which was harmless while every failure also
    // wrote a per-program row — but the environmental failures now route HERE and nowhere
    // else, so a first-run offline `atpkg install` left no witness of any kind. That is
    // precisely the failed-seed machine `tools/install.sh` hands `pkg doctor` to, and it
    // made doctor's own fallback to this sentence dead code in the case that motivated it.
    // Fields are stamped exactly as `record_status` stamps them: `enabled` and
    // `index_source` are CURRENT facts, so recomputing beats carrying a stale pair.
    let existing = crate::status::read(layout).unwrap_or_default();
    let _ = crate::status::write(
        layout,
        &crate::Status {
            schema: 1,
            enabled: manager_enabled(),
            index_source: crate::resolve_account(crate::config::cached().account()).slug(),
            outcome,
            // The TIMESTAMP moves with the sentence. Refreshing one and not the other
            // paired a fresh verdict with the moment of the last failure, which is
            // what Settings and `atpkg doctor` show side by side
            // (2026-08-20 round-9 audit).
            updated_at: now_rfc3339(),
            ..existing
        },
    );
}

/// Remove one bookkeeping row from `status.toml` — the counterpart to
/// [`record_status`] for a failure a later pass has disproved.
///
/// Rows like `*index*` and `*toolset*` are WRITE-ONLY diagnostics: nothing ever
/// deleted one, so a transient failure was indistinguishable from a standing one
/// for the life of the machine. Best-effort and silent, exactly like the writer:
/// observability must never fail an update.
fn clear_status_row(layout: &crate::store::Layout, program: &str) {
    let Some(existing) = crate::status::read(layout) else {
        return;
    };
    let mut programs = existing.programs;
    if programs.remove(program).is_none() {
        return;
    }
    let _ = crate::status::write(
        layout,
        &crate::Status {
            programs,
            ..existing
        },
    );
}

/// The signed `tree_root` to persist for `program`: the freshly-applied one when
/// non-empty, otherwise the ALREADY-RECORDED root. An `already_current` install, or an
/// untouched coherence-group sibling, reports an empty tree_root (nothing was flipped) —
/// persisting that empty value would erase a perfectly good attestation and break
/// `atpkg verify` for an untampered program. So an empty new root means "keep what we
/// had", never "forget it".
fn effective_tree_root(layout: &crate::store::Layout, program: &str, new_root: &str) -> String {
    if !new_root.is_empty() {
        return new_root.to_string();
    }
    crate::status::read(layout)
        .and_then(|s| s.programs.get(program).map(|p| p.tree_root.clone()))
        .unwrap_or_default()
}

/// The row to record when an install FAILS for a program that may already be live.
///
/// A FAILED INSTALL UNINSTALLS NOTHING. An upgrade that dies at download, stage, activate
/// or manifest-fetch leaves the previous build exactly where it was — still activated,
/// still shimmed, still working — and every failure arm nonetheless recorded
/// `installed_build: None, tree_root: ""`, which says the program is gone AND unattested.
/// Both false, and the second is destructive rather than merely wrong: `crate::verify`
/// fails closed on an empty recorded root, so one failed upgrade turned `atpkg verify` into
/// a permanent "cannot verify" for a program whose bytes were never touched, recoverable
/// only by reinstalling gigabytes to re-record a root that had been correct all along.
///
/// The live build comes from `active_builds` — the shims, i.e. ground truth — rather than
/// from the record being overwritten, so a machine whose record disagrees with its store
/// gets the store's answer.
///
/// THE ATTESTATION BELONGS TO THE BUILD IT WAS CAPTURED FOR, and that is why the two fields
/// cannot simply be read from two different places. Taking the build from the store and the
/// root from the record with no agreement check stitches together a row saying "live build
/// 20, attested by a root signed for build 18" — and `crate::verify` is ordered
/// `NotInstalled → NoSignedRoot → BuildMismatch → compare`, so writing the live build into
/// `installed_build` DISARMS the `BuildMismatch` guard that exists to catch exactly this.
/// Execution then reaches the tree comparison, where differing bytes report `Drift` — a
/// tampering accusation against bytes nobody touched — and identical bytes report `Match`,
/// affirming a build no signature ever covered. The second is a fail-OPEN on authenticity,
/// which is the one direction this crate may never fail.
///
/// So the root is kept only when it still means something: when record and store name the
/// SAME build, or when the store names none at all. That second case is not an edge — it is
/// a TOMBSTONED program, whose shims were replaced by failing stubs while its bytes and
/// their attestation sit untouched in the store. Dropping the root there would erase a good
/// attestation on the strength of a yanked pin.
fn failure_row(
    layout: &crate::store::Layout,
    program: &str,
    state: String,
) -> crate::ProgramStatus {
    let live = crate::active_builds(layout).get(program).copied();
    let recorded = crate::status::read(layout).and_then(|s| s.programs.get(program).cloned());
    crate::ProgramStatus {
        installed_build: live,
        state,
        tree_root: match &recorded {
            Some(r) if live.is_none() || r.installed_build == live => r.tree_root.clone(),
            _ => String::new(),
        },
    }
}

/// Record the install outcome to `status.toml` (the silent manager's observability
/// surface, §5/§9). Best-effort — diagnostics, never load-bearing.
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
    // Both teeth are closed structurally, which is why the lane could return
    // (2026-08-17, resurrecting the shape deleted in `ba832933`):
    //   * the chain joins ONLY on an EMPTY store — the first run, where there
    //     is no floor to roll back and no installed build for a stale pin to
    //     downgrade; every subsequent pass is network-only, so the seed can
    //     never act as an update source;
    //   * `ChainFetcher` caches under the PRIMARY leg's id and persists only
    //     the primary's own candidates (`cache_source_id`,
    //     `cacheable_candidates` — net.rs), so a seed-leg success can neither
    //     satisfy nor overwrite the last-good NETWORK cache.
    // The seed is the network registry's twin, not a second trust path: the
    // DirFetcher's bytes pass the identical verify + floor + freshness gates.
    let seeded_bootstrap = seed_bootstrap_leg(
        crate::bundled_seed_dir(),
        crate::active_builds(layout).is_empty(),
    )
    .map(|seed| Box::new(crate::DirFetcher::new(seed)) as Box<dyn crate::flow::Fetcher>);
    match seeded_bootstrap {
        Some(seed) => Box::new(crate::ChainFetcher::new(github, seed)),
        None => github,
    }
}

/// THE bootstrap-only rule, as a pure function: the co-located seed joins the
/// fetcher chain **iff** it exists AND the store is empty.
///
/// Split out of [`resolve_fetcher`] because it is the entire safety argument
/// for re-admitting the seed lane (§9.1), and the version of it that lived
/// inline was untestable — it reached for `current_exe` and process env — so
/// the one claim the 2026-07-30 adversarial review turned on had no test at
/// all. Both teeth it closes are restated at the call site; what this function
/// guarantees is the premise they rest on.
///
/// `store_is_empty` is the SHIM view (`active_builds`), not "no build dirs on
/// disk", so a store whose every program has been TOMBSTONED by a yank reads
/// empty here and re-arms the chain. That is safe, but for a reason OUTSIDE
/// this function: the pass that tombstones also advances the durable
/// `index_build` floor to the yanking index, and a yanking index is by
/// construction published above the sealed one — so a seal that would
/// reinstate yanked builds is refused by the floor, not by this predicate.
/// Worth knowing before anyone "tightens" the emptiness test to count
/// directories instead.
fn seed_bootstrap_leg(
    seed: Option<std::path::PathBuf>,
    store_is_empty: bool,
) -> Option<std::path::PathBuf> {
    seed.filter(|_| store_is_empty)
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

/// Whether this machine has ADOPTED the ALab toolset — see [`crate::store::Layout::adopted`]
/// for what that means and why it is not the same question as `[packages].auto_install`.
fn adopted(layout: &crate::store::Layout) -> bool {
    layout.adopted().is_file()
}

/// Record adoption after a deliberate whole-set install. Idempotent, and best-effort in the
/// same sense the floor writes are: a machine that adopted but could not persist the marker
/// simply asks the consent question again next pass, which is the safe direction.
fn record_adoption(layout: &crate::store::Layout) {
    let path = layout.adopted();
    if path.is_file() {
        return;
    }
    // The payload is documentation for whoever finds the file, never something read back:
    // adoption is the file's EXISTENCE, so a truncated or garbled write cannot be
    // misinterpreted as a different answer.
    match crate::platform::open_create_write(&path, 0o600) {
        Ok(mut f) => {
            use std::io::Write as _;
            let _ = writeln!(
                f,
                "# This machine runs the ALab toolset as a SET.\n\
                 # atpkg therefore keeps that set COMPLETE — a program published to the\n\
                 # signed index later is installed on a later pass, not silently skipped.\n\
                 # Written by the batteries-included seed bootstrap or `install --default-set`.\n\
                 # Removed by `uninstall`. To drop one program while staying adopted, use\n\
                 # [packages].exclude in aterm.toml."
            );
        }
        Err(e) => eprintln!(
            "atpkg: could not record toolset adoption at {}: {e} — the set will not be \
             auto-completed until a later pass records it",
            path.display()
        ),
    }
}

/// Forget adoption. Called by `uninstall`: removing a managed program is an explicit act,
/// and set-completion must never undo it on the next unattended pass.
fn clear_adoption(layout: &crate::store::Layout) {
    let path = layout.adopted();
    if path.is_file() {
        let _ = std::fs::remove_file(&path);
    }
}

/// The programs this user removed individually ([`crate::store::Layout::removed`]).
fn removed_programs(layout: &crate::store::Layout) -> std::collections::BTreeSet<String> {
    // ONE reader, on the layout: the unattended update lanes need the same answer,
    // and a second copy here is how they came to disagree.
    layout.removed_programs()
}

/// Record that `program` was removed on purpose, so no unattended pass puts it back.
fn record_removed(layout: &crate::store::Layout, program: &str) {
    let mut all = removed_programs(layout);
    if !all.insert(program.to_string()) {
        return;
    }
    write_removed(layout, &all);
}

/// Forget removals for `programs` — an explicit install is an unambiguous change of mind.
fn clear_removed(layout: &crate::store::Layout, programs: &[String]) {
    let mut all = removed_programs(layout);
    let before = all.len();
    for p in programs {
        all.remove(p);
    }
    if all.len() != before {
        write_removed(layout, &all);
    }
}

fn write_removed(layout: &crate::store::Layout, all: &std::collections::BTreeSet<String>) {
    let path = layout.removed();
    if all.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Ok(mut f) = crate::platform::open_create_write(&path, 0o600) {
        use std::io::Write as _;
        let _ = writeln!(
            f,
            "# Programs removed on purpose. No unattended pass reinstalls these;\n\
             # `aterm pkg install <program>` brings one back."
        );
        for p in all {
            let _ = writeln!(f, "{p}");
        }
    }
}

/// The STABLE stdout markers the GUI parses (`crates/aterm-gui`, `parse_seed_line`).
///
/// Constants rather than inline literals because this is a CROSS-CRATE CONTRACT with a
/// silent failure mode: a reworded prefix does not break a build or a test, it just
/// means the notice never appears again — and this contract has already broken
/// undetected once. `aterm-gui` imports these, so a rename is a compile error on both
/// sides instead of a string that quietly stops matching.
///
/// Every marker is a TERMINAL answer to an announcement except `SEED_STARTING`, which
/// opens one. An announcement with no answer leaves "Installing…" on screen forever.
pub const SEED_STARTING_MARKER: &str = "seed-starting: ";
pub const SEED_INSTALLED_MARKER: &str = "seed-installed: ";
pub const SEED_PENDING_MARKER: &str = "seed-pending: ";
pub const SEED_UNUSABLE_MARKER: &str = "seed-unusable: ";
pub const SEED_FAILED_MARKER: &str = "seed-failed: ";
pub const SEED_PARTIAL_MARKER: &str = "seed-partial: ";
/// The NETWORK completion lane's answer — programs that arrived over the wire on an
/// adopted machine, which used to happen with no user-visible trace at all.
pub const NET_INSTALLED_MARKER: &str = "net-installed: ";
/// The network lane's ANNOUNCEMENT, printed BEFORE any bytes move — the
/// seed-starting twin for the wire. The comment above `complete_the_set`
/// already argued for it ("ANNOUNCE BEFORE ACTING, exactly as the local seed
/// lane does") while the code announced nothing: gigabytes could stream with
/// the first user-visible line being the completion.
pub const NET_STARTING_MARKER: &str = "net-starting: ";
/// The network lane's failure TERMINAL: an announced provisioning that then
/// installs nothing must retire its own held card with the truth — the seed
/// lane shipped exactly this bug once (a card held for its full 20 minutes),
/// and the marker contract is what prevents the rerun.
pub const NET_FAILED_MARKER: &str = "net-failed: ";

/// The human line printed on its own row AFTER an install marker (`seed-installed:` /
/// `net-installed:`) — never appended to the marker itself, which the GUI parses
/// byte-for-byte. Answers the first question a fresh install raises: how do I run one?
const SEED_FOLLOW_ON: &str = "atpkg: these tools are on PATH in every aterm shell — \
                              aterm pkg list shows them; aterm <tool> runs one anywhere";

/// WHETHER THE 6-HOUR PASS MAY COMPLETE THE SET — the whole consent policy, as one
/// pure function so it can be tested.
///
/// Three inputs, and the precedence between them is the part that keeps going wrong:
///
/// * `declined` — the user removed the toolset on purpose. It outranks everything,
///   including `auto_install`, because it is the later and more specific act. Checked
///   HERE and not only in the seed lane: the network pass is what the loop runs, so a
///   decline honoured locally while this pass reinstalled the set worked for exactly
///   one launch.
/// * `adopted` — this machine runs the toolset (installing aterm is wanting it), so
///   newly published members should arrive without asking again.
/// * `auto_install` — explicit consent to pull the set over the network onto a machine
///   that has none.
fn should_complete_set(auto_install: bool, adopted: bool, declined: bool) -> bool {
    !declined && (auto_install || adopted)
}

/// Whether this machine has DECLINED the bundled toolset
/// ([`crate::store::Layout::declined`]).
fn declined(layout: &crate::store::Layout) -> bool {
    layout.declined().is_file()
}

/// Record a decline, so the seed lane stops re-installing what the user removed.
fn record_decline(layout: &crate::store::Layout) {
    let path = layout.declined();
    if path.is_file() {
        return;
    }
    match crate::platform::open_create_write(&path, 0o600) {
        Ok(mut f) => {
            use std::io::Write as _;
            let _ = writeln!(
                f,
                "# This machine declined the bundled ALab toolset.\n\
                 # `atpkg seed` will not re-install it (without this, the next launch would\n\
                 # simply put back everything `uninstall --all` just removed).\n\
                 # Removed automatically by any explicit install."
            );
        }
        Err(e) => eprintln!(
            "atpkg: could not record the decline at {}: {e} — the bundled seed may reinstall \
             on the next launch; set [packages].seed_install = false in aterm.toml to be sure",
            path.display()
        ),
    }
}

/// Forget a decline — any EXPLICIT install is an unambiguous change of mind, and undoing
/// one must never require finding a marker file.
fn clear_decline(layout: &crate::store::Layout) {
    let path = layout.declined();
    if path.is_file() {
        let _ = std::fs::remove_file(&path);
    }
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
    // `program → pinned asset name` for everything this pass resolves (the program AND its
    // `requires` pull-ins) — filled even for a member whose download then fails, which is
    // what lets the pass-end gc below spare that member's `.part` resume state.
    let mut resolved_assets = std::collections::BTreeMap::new();
    let result = crate::flow::install_collecting_assets(
        fetcher,
        layout,
        &effective_anchor(layout),
        &req,
        floor,
        now_unix(),
        &mut resolved_assets,
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
            //
            // The SPARING form, not plain `run`: this pass holds the verified pinned asset
            // name for everything it resolved, and a `requires` pull-in whose download
            // failed (the main install still succeeds — deps are best-effort) has a `.part`
            // here that plain `run`'s staging sweep would destroy, refetching the dep from
            // byte 0 next pass. Programs outside the map are swept exactly as before.
            if !r.already_current {
                let _ = crate::gc::run_keeping_pinned_partials(layout, &|p| {
                    resolved_assets.get(p).cloned()
                });
            }
            // Refresh the interactive-shell PATH hook (append-not-prepend, §16). At the CLI
            // edge — not inside flow.rs — so flow's synthetic-layout tests never write the
            // real ~/.aterm. Best-effort; writes OUTSIDE the hashed store tree.
            if !r.already_current {
                crate::hooks::refresh(layout);
            }
        }
        Err(e) => match failed_install_state(e) {
            Some(state) => record_status(
                layout,
                program,
                failure_row(layout, program, state),
                format!("install {program}: {e}"),
            ),
            // The attempt is still WITNESSED — it just is not witnessed as a program.
            None => record_outcome(layout, format!("install {program}: {e}")),
        },
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
        // A bare `install` is nearly always someone asking for the toolset, and the product
        // default IS the whole set — so say the spelling that grants it rather than a usage
        // line that leaves them to guess which of the two forms they wanted.
        eprintln!("usage: atpkg install <program> | atpkg install --default-set");
        eprintln!(
            "atpkg:   for the whole ALab toolset (the usual answer): aterm pkg install --default-set"
        );
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
    // Asking for this program back lifts its removal record — otherwise the user
    // would install it, and then have to know about a marker file to stop the next
    // unattended pass treating it as "absent by request" again.
    clear_removed(&layout, std::slice::from_ref(program));
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
                "atpkg: {p} is dev-linked; run `aterm pkg unlink {p}` to install from the registry"
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: install {program} failed: {e}");
            print_unreachable_followup(&e, &format!("aterm pkg install {program}"));
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
                    // The rolled-back build's signed root is not carried in the rollback
                    // report, so it is written unset here and re-derived immediately below.
                    // Carrying the PREVIOUS build's root instead would be far worse than
                    // leaving it empty: it would attest bytes that are no longer there.
                    tree_root: String::new(),
                },
                format!("rolled back {program} {} -> {}", r.from_build, r.to_build),
            );
            // CLOSE THE WINDOW WHERE IT OPENS. This used to be left for "a later `atpkg
            // update`", but `recover_missing_roots` has exactly ONE call site and it is
            // `cmd_update_all` — so `atpkg update <program>`, the verb a user most naturally
            // reaches for right after rolling that program back, never repaired it, and
            // `atpkg verify` failed closed on a perfectly good rollback until an unrelated
            // whole-set pass happened to run. The fetcher is already in hand and the index
            // it just trusted is the right authority to ask.
            recover_missing_roots(&layout, &*fetcher);
            if let Some(g) = &r.coherence_group {
                eprintln!(
                    "atpkg: warning — {program} is in coherence group '{g}'; rolling back one \
                     member alone splits the locked tuple. Consider `aterm pkg update` to re-cohere."
                );
            }
            println!(
                "atpkg: rolled back {program} from build {} to {}; `aterm pkg pin {program}` to hold it there",
                r.from_build, r.to_build
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: rollback {program} failed: {e}");
            print_unreachable_followup(&e, &format!("aterm pkg rollback {program}"));
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
        eprintln!(
            "atpkg: {program} is not installed — nothing to {verb} (aterm pkg list shows \
             what is)"
        );
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
    // THE SHIM VIEW, deliberately. Its SILENCE for a tombstoned program is the
    // self-heal: `decide` then returns Install for the build `store/<p>/current`
    // already names, the transaction re-stages it (`Staged::was_live` exists for
    // exactly that member), and the shims come back. Widening this to include the
    // authority link — which I did, to close what an independent derivation called a
    // "one-way door" — REMOVED that silence and made the door real: a tombstoned
    // program whose pin had not moved decided UpToDate forever, a rollback re-pointed
    // its shims into the revoked build, and `uninstall --all` on a declined machine
    // revived it. The derivation's claim was wrong and so was my fix
    // (2026-08-20 round-13 audit).
    let installed: std::collections::BTreeMap<String, u64> = crate::active_builds(&layout);
    // WHO gets set-completion: a machine that has ADOPTED the toolset (the seed bootstrap
    // or an explicit "Install ALab toolset" already laid the whole set down), or one whose
    // owner ticked `auto_install` to authorize a from-scratch network bootstrap. The two
    // are different questions — see `store::Layout::adopted`. Reading only the config bit
    // meant a program published AFTER a user installed never reached them, so their
    // "entire ALab toolchain" quietly decayed into "whatever was published the day they
    // installed".
    // `declined` outranks BOTH. It is checked here and not only in the seed lane
    // because this is the pass the 6-hour loop runs: honouring a removal on the
    // local lane while the network lane quietly reinstalled the whole set on the
    // next tick made the decline look like it worked for exactly one launch.
    let complete_the_set =
        should_complete_set(cfg.auto_install(), adopted(&layout), declined(&layout));
    if installed.is_empty() && !complete_the_set {
        println!("{EMPTY_UPDATE}");
        return ExitCode::SUCCESS;
    }
    let fetcher = resolve_fetcher(&layout);
    let mut failures = 0u32;
    // `program → pinned asset name` for everything EITHER lane of this pass resolved —
    // the pass-end gc's sparing input, so a member whose download failed mid-pass keeps
    // its `.part` resume state instead of losing it to this pass's own closing sweep.
    let mut resolved_assets: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    if !installed.is_empty() {
        let floor = build_floor(&layout);
        let report = match crate::apply_channel(
            &*fetcher,
            &layout,
            &effective_anchor(&layout),
            cfg.channel(),
            current_triple(),
            &installed,
            // The invoking user's `[packages].exclude` — read HERE, at the CLI
            // edge that owns "whose config speaks", and handed down as data so
            // flow.rs stays hermetic (round-13 audit). This is the wire that
            // makes `uninstall`'s "keep the set, drop just this one" advice
            // true on the unattended tick instead of a promise the next pass
            // reversed with a multi-GB reinstall.
            cfg.exclude(),
            floor,
            now_unix(),
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("atpkg: update failed: {e}");
                // Same follow-up the single-program lane prints: the old Display
                // carried a false "retries automatically" tail, and dropping it
                // without a replacement left this lane stating a network failure
                // with no next act.
                print_unreachable_followup(&e, "aterm pkg update");
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
        // What the apply resolved, kept for the pass-end gc's sparing closure — a member
        // whose group aborted on a failed download is in here, and is the reason this
        // exists (its `.part` is the resume state the next pass continues from).
        resolved_assets.extend(report.resolved_assets.clone());
        // AND CLEAR THE FAILURE THIS PASS JUST DISPROVED. The `*index*` row is
        // written when a resolve fails and was never removed by anything, so a
        // single transient network error — a hotel wifi captive portal, a laptop
        // waking mid-poll — left `atpkg doctor` and Settings ▸ Packages reporting a
        // broken toolset forever, on a machine that had been updating cleanly every
        // six hours since (2026-08-20 round-8 audit).
        clear_status_row(&layout, "*index*");
        // REPAIR A MEMBER THAT WENT LIVE WITHOUT ITS ATTESTATION. A pass killed
        // inside the flip window leaves the program installed and working but with
        // no recorded signed root, and `atpkg verify` fails closed on that forever
        // while `seed` says "fully installed" and `update` says "up to date" — so
        // nothing repaired it. Re-derive the root from the same authority the
        // install used, never a local recomputation (2026-08-20 round-8 audit).
        recover_missing_roots(&layout, &*fetcher);
        // …and the SENTENCE that row left behind. `status.outcome` is what Settings
        // ▸ Packages prints as its detail line, so clearing the row while leaving
        // "update failed: …" in place swaps one stale verdict for a quieter one.
        record_outcome(
            &layout,
            format!("up to date (index build {})", report.index_build),
        );
        failures = report_channel_apply(&layout, &installed, &report);
        for p in &report.skipped_linked {
            println!("atpkg: {p} dev-linked — skipped");
        }
    }
    // §11 batteries-included: install the index default-set members not yet installed
    // (include/exclude-narrowed; linked/yanked members skip; per-program failures are loud
    // but never block the rest). This is what keeps an ADOPTED machine's toolset COMPLETE
    // as the suite grows, and what an `auto_install` machine uses to bootstrap from empty.
    if complete_the_set {
        // ANNOUNCE BEFORE ACTING, exactly as the local seed lane does. This pass can
        // pull GIGABYTES over the network — an Intel Mac taking the x86_64 set, a
        // seedless cut, a seal past its horizon, an app updated before it ever
        // provisioned — and it ran completely silently: the GUI spawns it with
        // stdout discarded, so a user could watch multiple GB arrive with nothing on
        // screen ever mentioning it. That is the same defect the seed lane's marker
        // contract exists to prevent, on the lane that is MORE surprising because
        // nothing local prompted it.
        let before_net = crate::active_builds(&layout);
        let net = install_default_set(
            &layout,
            &*fetcher,
            &effective_anchor(&layout),
            cfg,
            now_unix(),
        );
        failures += net.failures;
        let announced = net.announced;
        // The net lane's resolutions join the apply lane's for the same sparing closure.
        resolved_assets.extend(net.resolved_assets);
        let mut arrived: Vec<String> = crate::active_builds(&layout)
            .keys()
            .filter(|k| !before_net.contains_key(k.as_str()))
            .cloned()
            .collect();
        if !arrived.is_empty() {
            arrived.sort();
            println!("atpkg: {NET_INSTALLED_MARKER}{}", arrived.join(", "));
            println!("{SEED_FOLLOW_ON}");
        } else if announced {
            // ALWAYS ANSWER THE ANNOUNCEMENT (the seed lane's law): a pass that
            // said "installing over the network" and then installed nothing
            // must say so, or the held notice outlives its own truth.
            println!(
                "atpkg: {NET_FAILED_MARKER}network provisioning installed nothing —                  see the lines above for each program's reason"
            );
        }
    }
    // Reclaim superseded builds once after the whole channel apply (all group activations
    // done). Best-effort; never fails the update. This verb sweeps the WHOLE prefix, so an
    // abstention here is about a program it did try to keep current — reported, or the disk
    // grows after every update with nothing on screen ever mentioning it.
    //
    // The SPARING form, not plain `run`: this pass just resolved the current pins, so a
    // member whose download failed mid-pass keeps its `.part` — otherwise this very sweep
    // destroyed the resume state and the next 6-hour tick refetched a multi-GB archive
    // from byte 0 (resume-across-passes survived only a process kill). Programs the pass
    // resolved nothing for are swept exactly as before; `atpkg gc` still reclaims all.
    let report = crate::gc::run_keeping_pinned_partials(&layout, &|p| {
        resolved_assets.get(p).cloned()
    });
    print_gc_sweeps("update", &report);
    print_gc_abstentions("update", &report);
    // Refresh the interactive-shell PATH hook at the CLI edge (§16), best-effort.
    crate::hooks::refresh(&layout);
    if failures == 0 {
        // A PASS THAT INSTALLED NOTHING ON AN EMPTY STORE IS NOT A SUCCESS TO SHOW.
        // Every group clean-skips when the index publishes no artifact for this
        // triple, so `failures` stays zero and the verb exited 0 with nothing
        // installed and no status written — and the Settings button that runs it
        // reported a green "toolchain up to date" on a machine where the toolchain
        // can never arrive. Say the true thing instead, durably
        // (2026-08-20 round-8 audit).
        if crate::active_builds(&layout).is_empty() {
            // STDERR AND EXIT 2, matching `cmd_install_default_set`. The GUI keeps
            // stderr and discards stdout, and it maps 2 to the honest "not a
            // temporary error" sentence while 1 is a retryable failure it renders as
            // a bare exit code — so printing the reason on stdout and exiting 1
            // produced exactly the opaque message these fixes set out to kill
            // (2026-08-20 round-9 audit).
            // DO NOT BLAME THE CPU FOR EVERY EMPTY STORE. "Nothing installed and
            // nothing installed this pass" has more than one cause: a triple the
            // index does not serve, a translated process reporting a triple that is
            // not this Mac's, and a channel that revoked everything. The seed lane
            // already distinguishes these; this one asserted the architecture answer
            // for all of them (2026-08-20 round-9 audit).
            if running_translated() {
                eprintln!(
                    "atpkg: nothing is installed — this process is running under \
                     Rosetta translation, so {} is not this Mac's real architecture. \
                     Relaunch aterm natively and the toolset installs.",
                    current_triple()
                );
                return ExitCode::from(2);
            }
            let serves_us = crate::resolve_verified_index(
                &*fetcher,
                &layout,
                &effective_anchor(&layout),
                build_floor(&layout),
                now_unix(),
            )
            .ok()
            .map(|index| !seed_serviceable(&layout, &*fetcher, &index, cfg).is_empty());
            let (said, state) = if serves_us == Some(true) {
                // The index DOES publish for this triple, so something else refused
                // every member — a revocation, a floor, a filter. Saying "no build
                // for your architecture" here sends the user to fix the wrong thing.
                (
                    format!(
                        "nothing is installed and nothing was installable — the index \
                         publishes builds for {} but every one was refused (revoked, \
                         below the floor, or filtered out); see `aterm pkg doctor`",
                        current_triple()
                    ),
                    "unavailable: every published build was refused",
                )
            } else {
                (
                    format!(
                        "nothing is installed and nothing was installable — no published \
                         build was found for this machine's architecture ({})",
                        current_triple()
                    ),
                    "unavailable: no build for this architecture",
                )
            };
            eprintln!("atpkg: {said}");
            record_status(
                &layout,
                "*toolset*",
                crate::ProgramStatus {
                    installed_build: None,
                    state: String::from(state),
                    tree_root: String::new(),
                },
                said,
            );
            return ExitCode::from(2);
        }
        // The toolset is present: retire any "unavailable" verdict an earlier
        // pass recorded. Only the seed lane cleared this row, and a machine
        // whose triple the index did not serve yet reaches the toolset through
        // THIS lane once artifacts are published (2026-08-20 round-9 audit).
        clear_status_row(&layout, "*toolset*");
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
/// What a default-set pass did, for the caller's marker bookkeeping: how many
/// members failed, and whether the `net-starting:` announcement was printed
/// (an announcement DEMANDS a terminal answer — `net-installed:` or
/// `net-failed:` — so the caller must know one was opened).
struct DefaultSetOutcome {
    failures: u32,
    announced: bool,
    /// `program → pinned asset name` for every member the pass resolved far enough to
    /// select an artifact — a member whose download then FAILED included. GC runs at the
    /// CALLER's edge (the `cmd_update_all` precedent), so this rides the outcome out to
    /// where the pass-end `gc::run_keeping_pinned_partials` builds its sparing closure.
    resolved_assets: std::collections::BTreeMap<String, String>,
}

fn install_default_set(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    anchor: &crate::Anchor,
    cfg: &crate::config::PackagesConfig,
    now: i64,
) -> DefaultSetOutcome {
    // Live-progress pass ownership (R5): when the GUI opted in via `--progress-file`
    // and no pass is live yet, this pass IS the "net" pass — begun here so its start
    // truncates the file, ended here so the terminal snapshot (pid cleared,
    // `ended_unix` stamped) is written on every exit path. The seed lane begins its
    // own "seed" pass BEFORE calling in, in which case `begin_pass` declines and the
    // seed lane keeps ownership.
    let owned_pass =
        progress_path().is_some_and(|p| crate::progress::begin_pass(p, "net"));
    let out = install_default_set_inner(layout, fetcher, anchor, cfg, now);
    if owned_pass {
        crate::progress::end_pass();
    }
    out
}

/// [`install_default_set`]'s body, split so pass ownership above cannot leak on any
/// of the early returns below.
fn install_default_set_inner(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    anchor: &crate::Anchor,
    cfg: &crate::config::PackagesConfig,
    now: i64,
) -> DefaultSetOutcome {
    let floor = build_floor(layout);
    let index = match crate::resolve_verified_index(fetcher, layout, anchor, floor, now) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("atpkg: default-set bootstrap: cannot resolve the signed index: {e}");
            print_unreachable_followup(&e, "aterm pkg install --default-set");
            return DefaultSetOutcome {
                failures: 1,
                announced: false,
                resolved_assets: std::collections::BTreeMap::new(),
            };
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
        return DefaultSetOutcome {
            failures: 1,
            announced: false,
            resolved_assets: std::collections::BTreeMap::new(),
        };
    };
    let installed = crate::active_builds(layout);
    let mut wanted = index.installable(cfg.include(), cfg.exclude());
    // Programs the user removed on purpose are not "missing" — they are absent by
    // request. Filtering them ONLY in the seed lane's prescan was not enough: THIS
    // function is what the 6-hour pass runs, so an adopted machine reinstalled an
    // uninstalled program on the next tick regardless. Set-completion means keeping
    // the set the user has, not overruling their removals.
    for p in removed_programs(layout) {
        wanted.remove(&p);
    }
    // SERVE THIS TRIPLE OR SAY NOTHING. `wanted` so far is what the signed
    // index NAMES — which, on a machine whose triple the registry publishes no
    // artifacts for (an Intel Mac before x86_64 lands; Linux/Windows today),
    // is a list of programs that can never arrive here. Unfiltered, it turned
    // every 6-hour tick into theater: "installing N program(s)" announced, N
    // failures counted, a failure pill raised — forever — and N pending stubs
    // laid on PATH whose promise ("open aterm, it provisions") could never
    // come true. The seed prescan (`seed_serviceable`) already applies this
    // rule with the same probe; the network lane now applies it BEFORE the
    // stub reconcile and the announcement, so unserved programs neither
    // announce, nor count as failures, nor leave stubs. The probe proves
    // missing-ness from a verified manifest and DEFERS on any fetch failure
    // (`group_missing_triple`), so an offline tick drops nothing — the real
    // stage still fails loudly there. One `*toolset*` status row records the
    // truth for `__pending`'s not-running fallback to surface.
    let triple = current_triple();
    let mut unserved: Vec<String> = Vec::new();
    for group in crate::plan_groups(&index, ch) {
        let missing: Vec<String> = group
            .members
            .iter()
            .filter(|m| wanted.contains(m.as_str()) && !installed.contains_key(m.as_str()))
            .cloned()
            .collect();
        if missing.is_empty()
            || group
                .members
                .iter()
                .any(|m| crate::linkmode::is_linked(layout, m))
        {
            continue;
        }
        let probe: &[String] = match &group.group {
            Some(_) => &group.members,
            None => &missing,
        };
        if crate::flow::group_missing_triple(fetcher, &index, cfg.channel(), triple, probe)
            .is_some()
        {
            unserved.extend(missing);
        }
    }
    if !unserved.is_empty() {
        unserved.sort();
        for p in &unserved {
            wanted.remove(p.as_str());
        }
        println!(
            "atpkg: {} program(s) have no build for this machine ({triple}): {} — skipped, \
             not failed; they install automatically once builds publish",
            unserved.len(),
            unserved.join(", ")
        );
        record_status(
            layout,
            "*toolset*",
            crate::ProgramStatus {
                installed_build: None,
                state: String::from("blocked: no build for this architecture"),
                tree_root: String::new(),
            },
            format!("unserved for {triple}: {}", unserved.join(", ")),
        );
    }
    // Pending-stub reconcile at the index resolve (R6): the SIGNED set replaces the
    // compiled roster the adoption-time stubs were laid from — newly listed names
    // gain stubs (PATH coverage before their bytes move), de-listed/removed/
    // installed names lose theirs, and every kept stub is rewritten so its embedded
    // atpkg path survives app relocation/self-update. `wanted` is already
    // triple-filtered above, so a stub is only ever laid for a program whose
    // bytes CAN move on this machine.
    crate::stub::reconcile(layout, &wanted, &installed);
    // ANNOUNCE BEFORE ACTING — the seed lane's law, now kept on the wire lane
    // too (the block comment at the caller had argued for it while nothing
    // announced). The set named is exactly what the loop below will attempt:
    // wanted ∧ absent, minus dev-linked groups (announcing a member the loop
    // then skips for a dev link would be a lie). Printed only when non-empty,
    // so the ordinary every-6-hours no-op tick stays silent.
    let mut will_install: Vec<String> = Vec::new();
    for group in crate::plan_groups(&index, ch) {
        if group
            .members
            .iter()
            .any(|m| crate::linkmode::is_linked(layout, m))
        {
            continue;
        }
        will_install.extend(
            group
                .members
                .iter()
                .filter(|m| wanted.contains(m.as_str()) && !installed.contains_key(m.as_str()))
                .cloned(),
        );
    }
    let announced = !will_install.is_empty();
    if announced {
        will_install.sort();
        println!(
            "atpkg: {NET_STARTING_MARKER}installing {} program(s) over the network: {}",
            will_install.len(),
            will_install.join(", ")
        );
    }
    // The pass's plan, in PLAN ORDER, with each group's freshly-installable members —
    // materialized once so the priority queue below can permute what remains between
    // items without ever re-deciding WHAT is planned (reorder-only, §4).
    let groups = crate::plan_groups(&index, ch);
    let plan: Vec<(usize, Vec<String>)> = groups
        .iter()
        .enumerate()
        .filter_map(|(i, group)| {
            let missing: Vec<String> = group
                .members
                .iter()
                .filter(|m| wanted.contains(m.as_str()) && !installed.contains_key(m.as_str()))
                .cloned()
                .collect();
            (!missing.is_empty()).then_some((i, missing))
        })
        .collect();
    // Every planned member, for the reorder-only intersection: a bump line naming
    // anything OUTSIDE this set — unknown, already installed, removed, garbage — is
    // ignored. The bump file can only permute installs the signed index authorized.
    let plannable: std::collections::BTreeSet<String> = plan
        .iter()
        .flat_map(|(_, missing)| missing.iter().cloned())
        .collect();
    // The live-progress plan (R5): per-program signed sizes fetched only when a sink
    // is live (the GUI lane) — N tiny verified manifest reads fix the overall bar's
    // denominator honestly before any byte moves. Best-effort: a miss degrades that
    // row to unmetered, never fails the pass.
    if let Some(sink) = crate::progress::active() {
        let planned: Vec<(String, u64)> = plan
            .iter()
            .flat_map(|(_, missing)| missing.iter())
            .map(|m| {
                let size = crate::flow::planned_artifact_size(
                    fetcher,
                    &index,
                    ch,
                    m,
                    current_triple(),
                )
                .unwrap_or(0);
                (m.clone(), size)
            })
            .collect();
        sink.plan(&planned);
    }
    let mut failures = 0u32;
    // `program → pinned asset name` for every member either arm below resolves — filled
    // even when the member's download then fails, and carried out on the outcome so the
    // CALLER's pass-end gc can spare that member's `.part` resume state.
    let mut resolved_assets: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    // Every channel-pinned program some group covers (grouped tuple or singleton);
    // wanted members left over are unpinned and fail loudly below.
    let mut pinned_members: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for group in &groups {
        pinned_members.extend(group.members.iter().cloned());
    }
    // THE PRIORITY QUEUE (§4): between items, re-read the bump file and stably
    // re-sort the REMAINDER — bumped items first, in bump (first-mention) order, plan
    // order as the tiebreak. The current item always finishes completely first
    // (in-flight bytes are never abandoned); bumping any member bumps its whole
    // group (the transactional activation is untouched). The bump file is consumed
    // read-only here and deleted only at a clean pass end.
    let mut remaining: Vec<(usize, Vec<String>)> = plan;
    while !remaining.is_empty() {
        let bump: Vec<String> = crate::progress::read_bump(layout)
            .into_iter()
            .filter(|n| plannable.contains(n))
            .collect();
        if !bump.is_empty() {
            resort_bumped(&mut remaining, &bump);
            if let Some(sink) = crate::progress::active() {
                for name in &bump {
                    sink.bumped(name);
                }
                let order: Vec<String> = remaining
                    .iter()
                    .flat_map(|(_, missing)| missing.iter().cloned())
                    .collect();
                sink.queue(&order);
            }
        }
        let (gi, missing) = remaining.remove(0);
        let group = &groups[gi];
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
            for m in &missing {
                note_finished(m, crate::progress::Phase::Skipped, None);
            }
            continue;
        }
        match &group.group {
            // A coherence tuple fresh-installs ALL-OR-NOTHING against the one index
            // resolved above (§7) — see [`bootstrap_group`].
            Some(g) => {
                failures += bootstrap_group(
                    layout,
                    fetcher,
                    &index,
                    cfg,
                    g,
                    group,
                    &wanted,
                    &installed,
                    &missing,
                    &mut resolved_assets,
                );
            }
            // An ungrouped member can move alone (§7) — see [`bootstrap_singleton`].
            None => {
                failures += bootstrap_singleton(
                    layout,
                    fetcher,
                    anchor,
                    cfg,
                    &group.members[0],
                    now,
                    &mut resolved_assets,
                );
            }
        }
    }
    // Clean pass end consumes the bump file: every admitted line was either acted on
    // or proven outside the plan. A FAILED pass keeps it — the failed program's
    // priority survives to the retry pass, exactly like its `.part`.
    if failures == 0 {
        crate::progress::clear_bump(layout);
    }
    // Second stub reconcile, against what ACTUALLY landed: a program whose real
    // shims do not expose its own name would otherwise keep a stale "installing"
    // stub until the next pass.
    crate::stub::reconcile(layout, &wanted, &crate::active_builds(layout));
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
    DefaultSetOutcome {
        failures,
        announced,
        resolved_assets,
    }
}

/// Stably re-sort the remaining plan by the bump file's admitted names: a group
/// containing ANY bumped member moves to the front (bumping a member bumps the
/// whole group — the transactional activation stays whole), ordered by the
/// earliest bump mention among its members, with PLAN ORDER as the tiebreak
/// (`sort_by_key` is stable). Pure over its inputs, so the reorder-only property —
/// the output is a PERMUTATION of the input, never new work — is directly testable.
fn resort_bumped(remaining: &mut [(usize, Vec<String>)], bump: &[String]) {
    let rank_of = |missing: &[String]| {
        missing
            .iter()
            .filter_map(|m| bump.iter().position(|b| b == m))
            .min()
            .unwrap_or(usize::MAX)
    };
    remaining.sort_by_key(|(_, missing)| rank_of(missing));
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
              fetcher, the ONE verified index, config, the group + its name, the \
              wanted/installed/missing member sets the narrowing check and prescan read, \
              and the pass's resolved-asset collector the pass-end gc sparing reads"
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
    resolved_assets: &mut std::collections::BTreeMap<String, String>,
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
        for m in missing {
            note_finished(m, crate::progress::Phase::Skipped, None);
        }
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
        for m in missing {
            note_finished(m, crate::progress::Phase::Skipped, None);
        }
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
        resolved_assets,
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
                note_finished(m, crate::progress::Phase::Done, None);
            }
            0
        }
        Ok((crate::TxnOutcome::UpToDate, _)) => {
            for m in missing {
                note_finished(m, crate::progress::Phase::Skipped, None);
            }
            0
        }
        Ok((crate::TxnOutcome::Pinned(held), _)) => {
            println!("atpkg: coherence group '{g}' held by local pin {held:?} — skipped");
            for m in missing {
                note_finished(m, crate::progress::Phase::Skipped, None);
            }
            0
        }
        Ok((crate::TxnOutcome::Tombstoned(members), _)) => {
            eprintln!(
                "atpkg: coherence group '{g}': pins tombstoned for {members:?} — \
                 nothing installed"
            );
            for m in missing {
                note_finished(m, crate::progress::Phase::Skipped, None);
            }
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
                // An already-installed member keeps its honest build AND its honest
                // attestation; a fresh member records neither. An all-or-nothing
                // transaction that aborted changed no member's bytes, so erasing the signed
                // root here made `atpkg verify` fail closed on a program the abort had
                // explicitly protected — the one site the round-8 "keep the recorded root"
                // sweep missed, on the very line below a field already making that argument.
                // Same primitive as every other failure arm: the store, not the caller's
                // snapshot, is asked what survived.
                failure_row(
                    layout,
                    &failed,
                    format!("error: coherence group '{g}' bootstrap aborted"),
                ),
                format!("bootstrap group '{g}' aborted at {failed}"),
            );
            // Honest terminal rows: the member that failed carries the reason; its
            // siblings did not install either (all-or-nothing), and saying so beats
            // a row frozen mid-phase forever.
            for m in missing {
                let why = if *m == failed {
                    format!("coherence group '{g}' bootstrap aborted at {failed}")
                } else {
                    format!("coherence group '{g}' aborted at {failed} — nothing changed")
                };
                note_finished(m, crate::progress::Phase::Failed, Some(why));
            }
            1
        }
        Err(e) => {
            eprintln!("atpkg: bootstrap of coherence group '{g}' failed: {e} (continuing)");
            for m in missing {
                note_finished(
                    m,
                    crate::progress::Phase::Failed,
                    Some(format!("coherence group '{g}' bootstrap failed: {e}")),
                );
            }
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
    resolved_assets: &mut std::collections::BTreeMap<String, String>,
) -> u32 {
    let floor = build_floor(layout);
    let req = crate::InstallRequest {
        channel: cfg.channel(),
        program,
        triple: current_triple(),
        installed: None,
    };
    // The collecting form: on the failure arm below there is no report, and the failed
    // download is exactly the member whose `.part` the caller's pass-end gc must spare.
    match crate::flow::install_collecting_assets(
        fetcher,
        layout,
        anchor,
        &req,
        floor,
        now,
        resolved_assets,
    ) {
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
            note_finished(program, crate::progress::Phase::Done, None);
            0
        }
        // Correct non-failure states — skipped quietly-but-visibly:
        Err(crate::FlowError::Linked(p)) => {
            println!("atpkg: {p} dev-linked — skipped");
            note_finished(program, crate::progress::Phase::Skipped, None);
            0
        }
        Err(crate::FlowError::NoArtifact(t)) => {
            println!("atpkg: {program}: no artifact for {t} — skipped (§6 clean skip)");
            note_finished(program, crate::progress::Phase::Skipped, None);
            0
        }
        Err(crate::FlowError::AppBundleRefused(_)) => {
            println!(
                "atpkg: {program}: app-bundle member — managed by the app's own updater, skipped"
            );
            note_finished(program, crate::progress::Phase::Skipped, None);
            0
        }
        Err(e @ crate::FlowError::Tombstoned(_)) => {
            eprintln!("atpkg: {program}: {e} — nothing installed");
            note_finished(program, crate::progress::Phase::Skipped, None);
            0
        }
        Err(e) => {
            eprintln!("atpkg: bootstrap install {program} failed: {e} (continuing)");
            record_bootstrap_error(layout, program, &e);
            note_finished(program, crate::progress::Phase::Failed, Some(e.to_string()));
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
        failure_row(layout, program, format!("error: {e}")),
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
    let before = crate::active_builds(&layout);
    let failures_outcome = install_default_set(
        &layout,
        &*fetcher,
        &effective_anchor(&layout),
        cfg,
        now_unix(),
    );
    let failures = failures_outcome.failures;
    let activated = crate::active_builds(&layout)
        .keys()
        .filter(|k| !before.contains_key(k.as_str()))
        .count();
    // The explicit "Install ALab toolset" act IS adoption — but only if it actually
    // laid something down. Adoption grants unattended network set-completion later
    // (`cmd_update_all`), so recording it after a pass that installed nothing would
    // switch that on for a machine that received no toolset at all.
    if activated > 0 {
        record_adoption(&layout);
    }
    // Asking for the toolset is an unambiguous change of mind about any earlier
    // removal; undoing a decline must never mean hunting for a marker file. The
    // per-program removals go too — "install the whole set" plainly includes them.
    clear_decline(&layout);
    let all_removed: Vec<String> = removed_programs(&layout).into_iter().collect();
    clear_removed(&layout, &all_removed);
    // GC + shell-hook refresh once at the CLI edge (the cmd_update_all precedent) — including
    // its disclosure of what the pass abstained on, for the same reason: this verb walks the
    // whole prefix, so a skip here is not about a program the user never mentioned.
    //
    // The SPARING form, not plain `run`: a member whose multi-GB download failed mid-pass
    // keeps its `.part` — this very sweep used to destroy the resume state, so retrying
    // the bootstrap refetched from byte 0. Programs the pass resolved nothing for are
    // swept exactly as before; the standalone `atpkg gc` still reclaims everything.
    let report = crate::gc::run_keeping_pinned_partials(&layout, &|p| {
        failures_outcome.resolved_assets.get(p).cloned()
    });
    print_gc_sweeps("install-default-set", &report);
    print_gc_abstentions("install-default-set", &report);
    crate::hooks::refresh(&layout);
    if failures > 0 {
        return ExitCode::from(1);
    }
    // "Zero failures" is NOT "it worked". Every member can clean-skip — no artifact
    // for this triple (§6), all dev-linked, all tombstoned — and that path used to
    // print "default set complete" and exit 0, which the Packages page renders as
    // "ALab toolset install completed" over an empty program list. Telling a user in
    // green that a multi-GB toolchain installed when nothing happened is the exact
    // silent-and-green shape this codebase keeps having to root out, so the
    // no-op case gets its own words and its own exit code.
    if activated == 0 {
        let already = !before.is_empty();
        if already {
            println!("atpkg: default set already complete — nothing to install");
            return ExitCode::SUCCESS;
        }
        println!(
            "atpkg: nothing was installed — the signed index pins no program with a build \
             for this machine ({}). This is not a failure to retry; nothing lands here \
             until an artifact for this architecture is published.",
            current_triple()
        );
        // Exit 2, not 0 and not 1: the GUI maps 0 to "Succeeded" (which would be the
        // lie) and 1 to a retryable failure (which would be false hope).
        return ExitCode::from(2);
    }
    // The `*seed*` offer, if one was ever announced, is now TAKEN. `record_status`
    // MERGES the program map, so a row nobody removes lives forever and Settings ▸
    // Packages keeps advertising an install the user already performed.
    clear_seed_status(&layout);
    println!("atpkg: default set complete ({activated} program(s) installed)");
    ExitCode::SUCCESS
}

/// `atpkg seed` — the batteries-included first-run bootstrap (§9.1/§11),
/// resurrected 2026-08-17 as the SIGNED lane only (the keyless source-build
/// lane stays deleted, `ba832933`): if a release cut sealed a signed seed
/// registry beside this executable, fill the EMPTY store from it — zero
/// network required — through the UNCHANGED anchor + freshness + floor +
/// sha256 + `tree_root` gates. The verb the GUI spawns once per launch.
///
/// The seed is a BOOTSTRAP source (see [`resolve_fetcher`]): once the store
/// holds anything, updates belong to the network + cache path, so the lane is
/// skipped entirely rather than resolving an index it will not use. Consent:
/// `[packages].seed_install` (default TRUE — the bytes are already on disk,
/// sealed under the app's own code signature, so installing the app is the
/// consent; ~3.2 GB lands in the store on extraction). `false` announces the
/// offer instead ([`announce_pending_seed`]) and Settings ▸ Packages carries
/// the act. Store mutation is serialized by the store-wide lock held at the
/// dispatch edge (`seed` is in [`verb_mutates_store`]). Every skip is loud
/// and exit-0 — absence of a seed is a legal state, not a failure.
///
/// # This verb is LOCAL-ONLY, and that is a consent property, not an optimization
///
/// The fetcher here is a bare [`crate::DirFetcher`] over the sealed registry —
/// **never** [`resolve_fetcher`]'s chain. `seed_install` defaults TRUE on
/// exactly one argument: the bytes already shipped inside the app, so laying
/// them down costs disk and no network. Handing this path the chain would make
/// that argument false. The chain's network leg is the index AUTHORITY, so the
/// moment the published index outranks the sealed one (the registry publishes
/// independently of app cuts — §9.2), its pins name builds the seed never
/// sealed, every asset misses by build-qualified name, and the "local bytes"
/// bootstrap silently pulls gigabytes over a metered link with
/// `auto_install = false` still set. `auto_install` is the switch that exists
/// to stop precisely that, and it gates the NETWORK bootstrap
/// ([`cmd_update_all`]) — which runs moments later on the same launch, and is
/// where a machine that wants fresher-than-sealed bytes gets them, with
/// consent. Staleness here is therefore correct and self-healing: the seed
/// lays down what it carries, the consented network pass upgrades it.
fn cmd_seed(rest: &[String]) -> ExitCode {
    if !rest.is_empty() {
        eprintln!("usage: atpkg seed");
        return ExitCode::from(2);
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    if declined(&layout) {
        // The user removed this toolset deliberately (`uninstall --all`). The seed
        // lane installs whatever the store lacks, so without this check it would
        // undo that on the very next launch.
        println!(
            "atpkg: the ALab toolset was removed on this machine — not re-installing it. \
             `aterm pkg install --default-set` brings it back."
        );
        return ExitCode::SUCCESS;
    }
    // INSTALLING ATERM IS WANTING THE TOOLSET. Adoption is recorded here — on the
    // first run of this lane, before any question of whether a seal exists —
    // because the sealed registry is an optimisation for HOW the toolchain
    // arrives, never a condition on WHETHER it does.
    //
    // Recording it only after a successful seed install left a hole exactly where
    // the product could least afford one: a machine that got the LEAN container
    // (every Intel Mac today, and any deliberately seedless cut) has no seal, so it
    // was never adopted, so `cmd_update_all` short-circuited on
    // `installed.is_empty() && !complete_the_set` and printed "nothing installed to
    // update" — forever. That user could never receive the ALab toolchain, not even
    // now that x86_64 artifacts publish, unless they hand-edited
    // `[packages].auto_install = true` into a config file that does not exist until
    // the app has already run. "Installing aterm installs the packages" has to be
    // true on every Mac, not only the ones the seal happens to serve.
    //
    // `declined` is checked above, so a deliberate `uninstall --all` still wins —
    // and so does `seed_install = false`, which is checked BEFORE this line for the
    // same reason. Adopting first made that flag a lie: the machine became adopted,
    // `cmd_update_all` then saw `complete_the_set` and installed the entire ~3.2 GB
    // set over the NETWORK seconds later, bypassing `auto_install` — the exact
    // unconsented multi-GB path the bare local `DirFetcher` exists to prevent. The
    // one documented way to say "not on my disk" has to be honoured before anything
    // records a wish for the toolset.
    let cfg = crate::config::cached();
    if !cfg.seed_install() {
        // Declining the lay-down is not adoption. Announce what is on offer (if
        // anything) and leave the store exactly as it was.
        if let Some(seed_dir) = crate::bundled_seed_dir()
            && manager_enabled()
        {
            let fetcher = crate::DirFetcher::new(seed_dir.clone());
            announce_pending_seed(&layout, &fetcher, cfg, &seed_dir);
        } else {
            println!("atpkg: [packages].seed_install = false — not installing the ALab toolset");
        }
        return ExitCode::SUCCESS;
    }
    record_adoption(&layout);
    // PENDING STUBS AT ADOPTION (R6): the instant this machine wants the toolset,
    // every default-set name resolves on PATH — BEFORE any question of whether a
    // seal exists, before a single network byte moves. Running one prints the live
    // install state (`atpkg __pending`) instead of "command not found". Compiled
    // roster only; the first index resolve reconciles it against the signed set.
    crate::stub::lay_adoption_stubs(&layout);
    let Some(seed_dir) = crate::bundled_seed_dir() else {
        // DO NOT PROMISE WHAT THE INDEX CANNOT DELIVER. This used to assert the
        // toolset would be "kept current and complete from here on" without asking
        // whether the index publishes anything for THIS machine — false on every
        // Intel Mac, which is the lean container's entire audience. (The sentence
        // also carried a 14-space run from a lost line continuation.)
        report_seedless_posture(&layout);
        return ExitCode::SUCCESS;
    };
    if !manager_enabled() {
        println!(
            "atpkg: bundled seed present ({}) but the manager is disabled — lane skipped \
             (fail-closed)",
            seed_dir.display()
        );
        return ExitCode::SUCCESS;
    }
    // LOCAL-ONLY by construction (see the consent note above): the sealed
    // registry, served through the identical verify + floor + freshness gates
    // as any network source — a dir holds bytes, not trust.
    let fetcher = crate::DirFetcher::new(seed_dir.clone());
    // Resolve the sealed index FIRST, so an unusable seed is a loud SKIP rather
    // than the hard failure `install_default_set` reports for an unresolvable
    // index. Every reason a seed can be unusable here is a correct state the
    // once-per-launch pass must not scream about: freshness lapsed (the DMG
    // outlived its horizon), or the store's durable floor already sits above
    // the sealed index_build (this machine has trusted something newer). In
    // both cases the consented network lane is the answer, not an error.
    let anchor = effective_anchor(&layout);
    let index = match crate::resolve_verified_index(
        &fetcher,
        &layout,
        &anchor,
        build_floor(&layout),
        now_unix(),
    ) {
        Ok(i) => i,
        Err(e) => {
            println!(
                "atpkg: bundled seed present ({}) but its index is not usable here: {e} \
                 — leaving bootstrap to the network lane",
                seed_dir.display()
            );
            return ExitCode::SUCCESS;
        }
    };
    // WHAT THIS SEAL CAN STILL DO FOR THIS MACHINE, decided before anything is
    // announced or extracted. Two questions, and the old code answered neither:
    //
    //  * Is there work left? The gate used to be "the store is empty", which made
    //    the lane single-shot: a first launch interrupted halfway (the extraction
    //    is minutes long) left some members installed, and every later launch then
    //    returned early and never touched the seal again — stranding the rest on a
    //    network that, for the sealed builds, is a strictly worse source. Asking
    //    instead for the members this store LACKS makes the lane resumable.
    //    Bootstrap-only-ness is not weakened: it is enforced by the seed being a
    //    bare local `DirFetcher` outranked by any newer network index, not by the
    //    store happening to be empty.
    //  * Can it serve THIS triple at all? A seal carrying no artifact for this Mac's
    //    architecture answers no. Coverage is per program, not per catalogue: the
    //    standalone programs publish x86_64-apple-darwin beside aarch64-apple-darwin,
    //    while the rustc coherence tuple is aarch64-only, so a seal can serve an Intel
    //    Mac in part or not at all. Saying "installing…" first and discovering the
    //    answer afterwards is how a user ends up watching a progress notice that never
    //    resolves.
    let wanted = seed_serviceable(&layout, &fetcher, &index, cfg);
    if wanted.is_empty() {
        return finish_unusable_seed(&layout, &seed_dir, &fetcher, &index, cfg);
    }
    // Parity with the other mutating network verbs: a config-declared dev-link
    // must exist BEFORE the pass decides what it manages (linked members
    // hard-skip, §13) — a dev box's first run must not pave over a checkout.
    reconcile_links(&layout, cfg);
    // WHOLE-SET DISK GATE, before a single byte is extracted. The per-member and
    // per-group preflights inside `flow` each ask "does THIS member fit", which is
    // the wrong question for a first run: on a laptop with a few GB free the small
    // solvers install, the trust tuple then fails its own gate, and the machine is
    // left permanently partial — provers but no compiler — with the cause recorded
    // nowhere. Asking for the total first turns that into one honest refusal that
    // names the number, and leaves the seal intact for a retry after the user frees
    // space.
    if let Some(need) = seed_install_bytes(&fetcher, &index, cfg, &wanted) {
        let have = crate::freespace::available_bytes(&layout.prefix);
        if !have.is_none_or(|a| crate::cost::disk_ok(need, a, crate::cost::FREE_FLOOR)) {
            println!(
                "atpkg: {SEED_FAILED_MARKER}the ALab toolset needs {:.1} GB free (plus a \
                 {:.0} GB reserve) and this disk has {:.1} GB — nothing was installed, and \
                 the bundled copy is kept for when space is available",
                need as f64 / 1e9,
                crate::cost::FREE_FLOOR as f64 / 1e9,
                have.unwrap_or(0) as f64 / 1e9
            );
            record_status(
                &layout,
                "*toolset*",
                crate::ProgramStatus {
                    installed_build: None,
                    state: String::from("blocked: insufficient disk space"),
                    tree_root: String::new(),
                },
                format!("needs {:.1} GB free", need as f64 / 1e9),
            );
            return ExitCode::SUCCESS;
        }
    }
    // ANNOUNCE BEFORE ACTING, with the SIGNED size rather than a guess. Extracting
    // the seal is minutes of work and gigabytes of disk, and until this marker the
    // only honest description of a first launch was "the app silently starts
    // consuming disk". The GUI streams this child's stdout, so the notice lands
    // while it happens rather than after (crates/aterm-gui, `parse_seed_line`).
    println!(
        "atpkg: seed-starting: installing {} ALab program(s) from the bundled registry{}",
        wanted.len(),
        match seed_install_bytes(&fetcher, &index, cfg, &wanted) {
            Some(b) => format!(" (~{:.1} GB on disk when finished)", b as f64 / 1e9),
            None => String::new(),
        }
    );
    let before = crate::active_builds(&layout);
    // The SEED progress pass (R5): same writer, `pass: "seed"` — verify/extract/link
    // phases with no download rows (the fetcher is a local dir; the sealed-seed
    // lane's bytes are untouched). Owned here so `install_default_set` sees a live
    // pass and does not begin its own "net" one.
    let owned_pass =
        progress_path().is_some_and(|p| crate::progress::begin_pass(p, "seed"));
    let failures = install_default_set(&layout, &fetcher, &anchor, cfg, now_unix()).failures;
    if owned_pass {
        crate::progress::end_pass();
    }
    // New shims must reach interactive shells without a relaunch.
    crate::hooks::refresh(&layout);
    let after = crate::active_builds(&layout);
    let mut new: Vec<String> = after
        .keys()
        .filter(|k| !before.contains_key(k.as_str()))
        .cloned()
        .collect();
    if !new.is_empty() {
        new.sort();
        // Mark what the SEED laid down as PROVISIONAL. The seal is a cut-time
        // snapshot; the update pass that runs seconds later replaces whatever the
        // published index has moved, and under GC's ordinary live+1-rollback rule
        // each replaced seed build would be retained — a second ~3.2 GB copy of
        // `trust` preserving a state nobody ran. See `crate::provisional`.
        let provisional: Vec<(String, u64)> = new
            .iter()
            .filter_map(|p| after.get(p.as_str()).map(|b| (p.clone(), *b)))
            .collect();
        crate::provisional::record(&layout, &provisional);
        // The offer, if one was ever announced, is now TAKEN — retire the row
        // before announcing the install, or Settings ▸ Packages keeps
        // advertising a pending offer the user already has on disk
        // (`record_status` MERGES, so nobody else ever removes it).
        clear_seed_status(&layout);
        // The stable marker the GUI parses — change it and the first-run
        // notice goes blind (crates/aterm-gui, spawn_pkg_update_check).
        println!("atpkg: seed-installed: {}", new.join(", "));
        // The marker above is the GUI's (byte-stable, parsed); this line is the human's:
        // an install that lands 10 commands and never says how to run one is a dead end.
        println!("{SEED_FOLLOW_ON}");
    }
    // RECLAIM only on a CLEAN pass. The payload is dead weight once extracted —
    // ~600 MB on top of the gigabytes it expanded into — but "extracted" has to
    // mean all of it. A partial pass (a disk that filled, one corrupt artifact)
    // leaves members the seal still holds locally, and deleting it there would
    // force the exact bytes that were on this disk a second ago to come back over
    // the network. `failures == 0` is what separates "consumed" from "interrupted",
    // and the lane is resumable now, so keeping it is what lets the next launch
    // finish the job.
    //
    // Deleting is signature-SAFE only because of WHERE the payload lives: a
    // `.lproj` directory, sealed `optional = true` by codesign's built-in rules,
    // so the bundle still verifies with it entirely absent (measured — see
    // `crate::bundled::SEED_DIR_NAME`). Doing this to a normally-sealed resource
    // would break `codesign --verify` and, with it, the updater's verification of
    // the installed bundle at the next apply.
    if failures == 0 {
        // The toolset is in: retire any standing "blocked"/"unavailable" row this
        // pass just disproved, so a disk-space or architecture verdict from an
        // earlier launch stops being Settings' answer forever.
        clear_status_row(&layout, "*toolset*");
        reclaim_bundled_seed(&seed_dir);
    } else {
        println!(
            "atpkg: keeping the bundled seed — {failures} member(s) did not install, and the \
             seal still holds them locally for the next attempt"
        );
    }
    // ALWAYS ANSWER THE ANNOUNCEMENT. `seed-starting:` already put "Installing the
    // ALab toolchain…" on screen; if this pass then installs nothing and emits no
    // terminal marker, that notice is the last word the user ever gets — an install
    // that announced itself, delivered nothing, and never said why. `new` being
    // empty here means every member failed (a clean skip cannot happen: the
    // serviceable prescan ran before the announcement), so this is the failure
    // marker the GUI needs to retire the notice honestly.
    if new.is_empty() {
        println!(
            "atpkg: seed-failed: no ALab program could be installed from the bundled \
             registry ({failures} failed) — the toolset will be retried on the next launch \
             and can also come from the network"
        );
    } else if failures > 0 {
        // PARTIAL. This is the likeliest real first-launch failure — a laptop
        // without ~4 GB free installs the small tools and then the disk preflight
        // refuses the trust tuple — and it used to print ONLY `seed-installed:`,
        // so the user got "✓ ALab toolchain installed" with no compiler present.
        // A green tick over a missing compiler is worse than an error, because
        // nothing prompts the user to look. The marker carries the count so the
        // notice can say what is actually true.
        println!(
            "atpkg: seed-partial: {} installed, {failures} could not be installed — the \
             rest is retried on the next launch (Settings ▸ Packages shows which)",
            new.len()
        );
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// The channel-pinned members this SEAL can still install on THIS machine: wanted by
/// config, not already active, and carrying an artifact for this triple.
///
/// The triple test is the one that matters for honesty. Artifact coverage is per
/// program, not per catalogue: the standalone programs publish `x86_64-apple-darwin`
/// beside `aarch64-apple-darwin`, while the rustc coherence tuple (`trust`, `trust-ir`,
/// `trust-cg`, `trust-vc`) is `aarch64-apple-darwin` only. So this returns the part of
/// the seal that can be served here — possibly nothing — and the caller can say which
/// BEFORE announcing an install, rather than printing "installing…", clean-skipping
/// every member inside `install_default_set`, and leaving a progress notice that never
/// resolves.
///
/// Group granularity matches the installer's: a coherence tuple is all-or-nothing, so a
/// tuple with any member lacking this triple contributes nothing (the same rule
/// `announce_pending_seed` applies, and the same one `install_default_set` acts on).
fn seed_serviceable(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    cfg: &crate::config::PackagesConfig,
) -> Vec<String> {
    let Some(ch) = index.channels.iter().find(|c| c.name == cfg.channel()) else {
        return Vec::new();
    };
    let installed = crate::active_builds(layout);
    let wanted = index.installable(cfg.include(), cfg.exclude());
    // Programs the user removed on purpose are NOT missing — they are absent by
    // request, and the lane's "install whatever the store lacks" rule would
    // otherwise reinstate them on the next launch.
    let removed = removed_programs(layout);
    let triple = current_triple();
    let mut out: Vec<String> = Vec::new();
    for group in crate::plan_groups(index, ch) {
        let missing: Vec<String> = group
            .members
            .iter()
            .filter(|m| {
                wanted.contains(m.as_str())
                    && !installed.contains_key(m.as_str())
                    && !removed.contains(m.as_str())
            })
            .cloned()
            .collect();
        if missing.is_empty()
            || group
                .members
                .iter()
                .any(|m| crate::linkmode::is_linked(layout, m))
        {
            continue;
        }
        // A coherence tuple applies ALL-OR-NOTHING, and `bootstrap_group` refuses the
        // whole group when any member is neither wanted nor installed — exactly what
        // an `exclude` entry or a prior `uninstall <member>` produces. Announcing the
        // remaining members anyway meant the lane advertised programs it would then
        // silently refuse, reported zero failures, and walked out having installed
        // nothing it promised. The prescan has to apply the installer's own rule.
        if group.group.is_some()
            && !group
                .members
                .iter()
                .all(|m| wanted.contains(m.as_str()) || installed.contains_key(m.as_str()))
        {
            continue;
        }
        let probe: &[String] = match &group.group {
            Some(_) => &group.members,
            None => &missing,
        };
        if crate::flow::group_missing_triple(fetcher, index, cfg.channel(), triple, probe).is_none()
        {
            out.extend(missing);
        }
    }
    out.sort();
    out
}

/// The SIGNED installed size of `members` for this triple, summed from the pinned
/// manifests' `[cost].disk_installed`.
///
/// The authoritative number, not a multiplier: the one disclosure a user gets before
/// committing multiple GB of disk should come from the same signed bytes everything
/// else in this lane is verified against. `None` when any member's cost is unavailable
/// or zero — a partial sum would understate the commitment, and no number at all is
/// more honest than a confidently wrong one.
fn seed_install_bytes(
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    cfg: &crate::config::PackagesConfig,
    members: &[String],
) -> Option<u64> {
    let ch = index.channels.iter().find(|c| c.name == cfg.channel())?;
    let triple = current_triple();
    let mut total: u64 = 0;
    for m in members {
        let (_, _, pkg) = crate::flow::verified_pkg(fetcher, index, ch, m)?;
        let art = pkg.artifact_for(triple)?;
        if art.cost.disk_installed == 0 {
            return None;
        }
        total = total.saturating_add(art.cost.disk_installed);
    }
    (total > 0).then_some(total)
}

/// The seal cannot serve this machine: say so honestly, reclaim it, and DO NOT claim
/// the network will make up the difference.
///
/// That claim would usually be false. The seal is staged from the published index, so
/// a program the seal cannot serve for this triple is overwhelmingly one the registry
/// does not publish for that triple either. Telling that user "the toolchain will come
/// from the network instead" sends them to wait for something that is not coming.
///
/// Emits the `seed-unusable:` marker so the GUI can retire the in-progress notice with
/// a real answer instead of leaving "Installing…" on screen forever.
fn finish_unusable_seed(
    layout: &crate::store::Layout,
    seed_dir: &std::path::Path,
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    cfg: &crate::config::PackagesConfig,
) -> ExitCode {
    // Distinguish "already done" from "can never be done here" — they deserve
    // different words, and only one of them is a disappointment.
    let complete = !crate::active_builds(layout).is_empty()
        && index
            .channels
            .iter()
            .find(|c| c.name == cfg.channel())
            .is_some_and(|ch| {
                let installed = crate::active_builds(layout);
                crate::plan_groups(index, ch)
                    .iter()
                    .flat_map(|g| g.members.iter())
                    .all(|m| installed.contains_key(m.as_str()))
            });
    clear_seed_status(layout);
    // Does this verdict justify deleting the seal? Only `complete` and the
    // architecture branch below say yes.
    let mut permanent = complete;
    if complete {
        println!("atpkg: the bundled seed is fully installed — nothing left to lay down");
    } else if crate::active_builds(layout).is_empty() {
        // Nothing installed and nothing installable — but do NOT blame the
        // architecture reflexively. An empty serviceable set has three causes, and
        // saying "no build for your Mac" to someone who actually excluded every
        // program, or whose whole toolset is dev-linked, is a confident wrong answer.
        let _ = fetcher;
        let removed = removed_programs(layout);
        permanent = false;
        if !removed.is_empty() {
            println!(
                "atpkg: seed-unusable: every program the bundled registry offers was \
                 removed on this machine ({}) — nothing to install. \
                 `aterm pkg install <program>` brings one back",
                removed.iter().cloned().collect::<Vec<_>>().join(", ")
            );
        } else if !index.channels.iter().any(|c| c.name == cfg.channel()) {
            // A channel the seal does not carry. REVERSIBLE — one line of config —
            // and it used to fall through to the architecture arm below, which set
            // `permanent = true` and DELETED the seal while blaming the CPU. The
            // comment on the reclaim names this exact case as one that must be kept;
            // the ladder simply had no branch for it, so the code contradicted its
            // own stated rule and destroyed ~600 MB over a typo.
            println!(
                "atpkg: seed-unusable: the bundled registry carries no '{}' channel \
                 (it has: {}) — nothing was installed. Fix [packages].channel in \
                 aterm.toml; the bundled toolchain is kept.",
                cfg.channel(),
                index
                    .channels
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        } else if !cfg.include().is_empty() || !cfg.exclude().is_empty() {
            println!(
                "atpkg: seed-unusable: [packages].include/exclude narrows the bundled \
                 registry to nothing installable on this machine ({}) — widen the filters \
                 to receive the toolset",
                current_triple()
            );
        } else if running_translated() {
            // NOT PERMANENT, AND NOT THIS MACHINE'S ARCHITECTURE. `current_triple()`
            // is a compile-time cfg, so the x86_64 slice of the universal binary
            // reports x86_64 even on Apple Silicon — which is what runs when someone
            // ticks "Open using Rosetta", or under any translated parent. Treating
            // that as "the CPU will not change" deleted the seal from an M-series
            // Mac that could have installed every program natively, and unticking
            // the box does not bring it back: only re-downloading the DMG does
            // (2026-08-20 round-8 audit). tools/install.sh already refuses to decide
            // a container from the running process's arch for this exact reason.
            println!(
                "atpkg: keeping the bundled seed — this process is running under \
                 Rosetta translation, so {} is not this Mac's real architecture. \
                 Relaunch aterm natively (Finder ▸ Get Info ▸ uncheck \"Open using \
                 Rosetta\") and the sealed toolchain installs.",
                current_triple()
            );
        } else {
            // The only permanent one in this arm: the CPU will not change.
            permanent = true;
            println!(
                "atpkg: seed-unusable: the bundled toolchain has no build for this machine's \
                 architecture ({}) — no ALab programs were installed from it",
                current_triple()
            );
            // AND LEAVE A DURABLE TRACE. Without this the whole first session had no
            // status.toml at all, so Settings ▸ Packages — the surface the notice
            // sends the user to — said "atpkg has not run yet" on a machine where it
            // had run correctly and reached a definite verdict. An honest "no" is
            // still an answer, and the absence of one reads as a broken install
            // (2026-08-20 round-8 audit).
            record_status(
                layout,
                "*toolset*",
                crate::ProgramStatus {
                    installed_build: None,
                    state: String::from("unavailable: no build for this architecture"),
                    tree_root: String::new(),
                },
                format!(
                    "the bundled toolchain carries no build for {}",
                    current_triple()
                ),
            );
        }
    } else {
        println!(
            "atpkg: the bundled seed has nothing further to install on this machine ({})",
            current_triple()
        );
    }
    // RECLAIM ONLY ON A PERMANENT VERDICT. Deleting ~600 MB is irreversible, so it
    // must not hinge on something the user can change their mind about in one line
    // of config. Two verdicts are permanent enough: `complete` (the payload was
    // consumed — it is genuinely spent) and no-artifact-for-this-triple (the CPU
    // will not change). The others — every offered program removed, include/exclude
    // narrowing the set to nothing, a channel the seal does not carry — are
    // reversible: widen the filter or reinstall one program and the seal would have
    // served, except it is gone. That turns a typo into a permanent loss of the
    // offline bootstrap this whole feature exists to provide.
    if permanent {
        reclaim_bundled_seed(seed_dir);
    } else {
        println!(
            "atpkg: keeping the bundled seed — nothing is installable under the current \
             configuration, but that is reversible and the seal would serve if it changes"
        );
    }
    ExitCode::SUCCESS
}

/// Is this process running under Rosetta translation?
///
/// `sysctl sysctl.proc_translated` is the documented answer (1 = translated). It
/// matters here because [`current_triple`] is a compile-time `cfg(target_arch)`: the
/// x86_64 slice of the shipped universal binary reports `x86_64-apple-darwin` on an
/// Apple Silicon Mac, and any verdict of the form "no build for this architecture"
/// is then a statement about the SLICE, not the machine. Anything irreversible
/// keyed on that verdict must ask this first.
///
/// Fail-safe: an unreadable sysctl answers `false`, which is the pre-existing
/// behaviour on every non-translated process.
#[cfg(target_os = "macos")]
fn running_translated() -> bool {
    std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "sysctl.proc_translated"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1")
}

#[cfg(not(target_os = "macos"))]
const fn running_translated() -> bool {
    false
}

/// Say something TRUE about a machine with no sealed registry, and leave a durable
/// trace of it.
///
/// Two very different situations reach here and they deserve different sentences: a
/// machine whose architecture the index publishes for (the toolset really will arrive
/// over the network) and one it does not (nothing arrives until someone publishes an
/// artifact). Assuming the optimistic one made the product promise every Intel user a
/// toolchain at first launch and then never mention it again.
///
/// It also records `status.toml`, because the ABSENCE of that file is what makes
/// Settings — the one surface a puzzled user is pointed at — say "atpkg has not run
/// yet" on a machine where it has run correctly on every single launch.
fn report_seedless_posture(layout: &crate::store::Layout) {
    let cfg = crate::config::cached();
    let triple = current_triple();
    // No seal here, so the only thing to consult is the network index. A resolve
    // failure is not the question being asked (offline is not "unsupported"), so it
    // degrades to the generic sentence rather than an alarming one.
    let fetcher = resolve_fetcher(layout);
    let serves_us = crate::resolve_verified_index(
        &*fetcher,
        layout,
        &effective_anchor(layout),
        build_floor(layout),
        now_unix(),
    )
    .ok()
    .map(|index| {
        // `seed_serviceable` answers "what is left to install HERE", so it returns
        // empty for two opposite states: nothing is published for this CPU, and
        // everything is already installed. The second one is the product's happy
        // path — the seal is consumed and reclaimed after a successful first run —
        // and reading empty as "unsupported architecture" told every fully
        // provisioned Apple Silicon Mac that its own architecture was unserved, once
        // per launch, forever: a stdout marker, a GUI warning pill, a `*toolset*`
        // row reading "unavailable", and a Settings detail line saying no build
        // exists (2026-08-20 round-8 audit). Ask the other question first.
        !seed_serviceable(layout, &*fetcher, &index, cfg).is_empty()
            || !crate::active_builds(layout).is_empty()
    });
    if serves_us == Some(false) {
        println!(
            "atpkg: {SEED_UNUSABLE_MARKER}the signed index publishes no artifact for this \
             machine's architecture ({triple}) — nothing was installed, and nothing arrives \
             until one is published"
        );
        record_status(
            layout,
            "*toolset*",
            crate::ProgramStatus {
                installed_build: None,
                state: String::from("unavailable: no build for this architecture"),
                tree_root: String::new(),
            },
            format!("the signed index publishes no artifact for {triple}"),
        );
    } else {
        println!(
            "atpkg: no bundled toolchain in this app — the ALab toolset installs from the \
             signed index instead: aterm pkg install --default-set (the windowed app also \
             runs this pass on its 6-hour loop)"
        );
    }
}

/// Delete the consumed bundled seed, reclaiming its disk (§9.1).
///
/// Best-effort by design, and silent on the ordinary failure: the app bundle may
/// legitimately not be ours to write (installed to `/Applications` by another
/// admin, or on a read-only mount), and a machine that keeps its seed is merely
/// carrying dead weight — never broken. The disk-space win is not worth turning
/// a successful toolchain install into a failure.
///
/// Only ever called after a seed install SUCCEEDED, so the bytes are provably
/// redundant: they are now extracted in the store, and the seed lane refuses to
/// read them again on a non-empty store.
/// Is this seal inside a BUILD OUTPUT rather than an installed app?
///
/// `seed_dir` is `<app>/Contents/Resources/<SEED_DIR_NAME>`, so the directory
/// holding the bundle is four levels up. `aterm-release`'s `bundle::assemble`
/// writes `.metadata_never_index` into every directory it produces a bundle in —
/// originally to keep a build-output app out of Spotlight, which makes it exactly
/// the durable "this is not an install" marker this needs, written by the only
/// thing that knows.
///
/// Fail-safe by construction: an unreadable or absent marker reads as "this is a
/// real install", which is the pre-existing behaviour.
fn is_build_output_bundle(seed_dir: &std::path::Path) -> bool {
    seed_dir
        .ancestors()
        .nth(4)
        .is_some_and(|dir| dir.join(".metadata_never_index").exists())
}

fn reclaim_bundled_seed(seed_dir: &std::path::Path) {
    // NOT OURS TO DELETE. `/Applications/aterm.app` is shared by every account on
    // the Mac, and the store this pass just filled belongs to ONE of them. If the
    // bundle is not owned by us, deleting the payload would reclaim our disk by
    // taking the offline batteries away from every other user on the machine —
    // they would each fall back to a network bootstrap for bytes that were sitting
    // right there. Owning the bundle is the closest available proxy for "this app
    // is mine to modify", and it also covers the root-owned/managed-fleet case,
    // where the delete would fail anyway.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let ours = std::fs::symlink_metadata(seed_dir)
            .map(|m| m.uid() == crate::platform::our_uid())
            .unwrap_or(false);
        if !ours {
            println!(
                "atpkg: leaving the bundled seed in place — {} is not owned by this user, and \
                 other accounts on this machine may still need it to install offline",
                seed_dir.display()
            );
            return;
        }
    }
    // NOR IS A BUILD OUTPUT. A bundle under a `dist/` the cutter marked
    // `.metadata_never_index` is an ARTIFACT, not an install: something produced it
    // and may still be signing, packaging or verifying it. On 2026-08-19 this exact
    // delete removed a gigabyte from a release the cutter was mid-package — the app
    // WAS ours and the payload WAS spent for this machine, so every test above
    // passed, and the artifact was corrupted anyway. The same rule spares a
    // developer who builds locally and runs their own `dist/aterm.app`: their next
    // `make` would otherwise find the batteries silently gone.
    //
    // Reclaiming is a disk optimisation. Refusing to do it costs a directory; doing
    // it to something that is not an install costs someone their release.
    if is_build_output_bundle(seed_dir) {
        println!(
            "atpkg: leaving the bundled seed in place — {} sits in a build output \
             directory, not an install; the payload belongs to whatever produced it",
            seed_dir.display()
        );
        return;
    }
    /// Recursive byte count, best-effort: this only sizes a log line, so an
    /// unreadable entry contributes zero rather than derailing the reclaim.
    fn dir_bytes(dir: &std::path::Path) -> u64 {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        entries
            .filter_map(Result::ok)
            .map(|e| match e.file_type() {
                Ok(t) if t.is_dir() => dir_bytes(&e.path()),
                _ => e.metadata().map(|m| m.len()).unwrap_or(0),
            })
            .sum()
    }
    let freed = dir_bytes(seed_dir);
    match std::fs::remove_dir_all(seed_dir) {
        Ok(()) => println!(
            "atpkg: reclaimed the bundled seed ({:.0} MB) — a provisioned store never reads \
             it again",
            freed as f64 / 1_000_000.0
        ),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            // Common and harmless: say it once, at a level that does not read as
            // an install failure.
            println!(
                "atpkg: the bundled seed stays on disk (no write access to {}) — harmless, \
                 just unreclaimed space",
                seed_dir.display()
            );
        }
        Err(e) => println!(
            "atpkg: could not reclaim the bundled seed at {}: {e} (harmless — the toolchain \
             is installed either way)",
            seed_dir.display()
        ),
    }
}

/// Retire the `*seed*` pending-consent row (the offer was taken, or nothing is
/// installable here). `record_status` MERGES the program map, so a row nobody
/// removes lives forever — Settings ▸ Packages would keep advertising an offer
/// the user already accepted.
fn clear_seed_status(layout: &crate::store::Layout) {
    let Some(mut status) = crate::status::read(layout) else {
        return;
    };
    if status.programs.remove("*seed*").is_none() {
        return;
    }
    status.updated_at = now_rfc3339();
    status.outcome = "bundled seed: nothing pending".to_string();
    let _ = crate::status::write(layout, &status);
}

/// The consent-pending half of the bundled-seed lane (§11, the
/// `[packages].seed_install = false` posture): resolve the ONE verified index
/// through the chain, count the channel-pinned installable members not yet
/// installed, and say so — a stable stdout marker line (`seed-pending: …`,
/// what the GUI's launch-time seed run parses for the first-run notice) plus a
/// `status.toml` entry so Settings ▸ Packages shows the same truth.
/// Announcement only: nothing is downloaded or extracted, and a failure to
/// resolve the index here is itself only announced (the seed is an offer, not
/// an obligation).
fn announce_pending_seed(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    cfg: &crate::config::PackagesConfig,
    seed_dir: &std::path::Path,
) {
    let floor = build_floor(layout);
    let index = match crate::resolve_verified_index(
        fetcher,
        layout,
        &effective_anchor(layout),
        floor,
        now_unix(),
    ) {
        Ok(i) => i,
        Err(e) => {
            println!(
                "atpkg: bundled seed present ({}) but its index did not verify: {e}",
                seed_dir.display()
            );
            return;
        }
    };
    let Some(ch) = index.channels.iter().find(|c| c.name == cfg.channel()) else {
        println!(
            "atpkg: bundled seed present ({}) but names no '{}' channel — nothing to offer",
            seed_dir.display(),
            cfg.channel()
        );
        return;
    };
    let installed = crate::active_builds(layout);
    let wanted = index.installable(cfg.include(), cfg.exclude());
    // A program with no artifact for THIS triple can never be installed here
    // (`install_default_set` clean-skips it, §6), so offering it would be a
    // pill and a status row the user can never satisfy. Narrow to what the
    // seed can actually lay down on this machine via the §11 bootstrap prescan
    // ([`crate::flow::group_missing_triple`]).
    //
    // The prescan runs at the SAME GRANULARITY the install uses, and that is the
    // whole point of doing it per group rather than per program: a coherence
    // tuple is all-or-nothing, so `install_default_set` passes the whole member
    // slice and clean-skips the ENTIRE group when any one member lacks an
    // artifact for this triple. A per-member prescan disagreed with that — for a
    // mixed-coverage tuple it advertised the members that DO have artifacts,
    // which neither advertised route would then install, and via the GUI the
    // failure was silent-and-green (a clean skip is zero failures, so the
    // Packages page reported "Succeeded" over an empty store).
    let triple = current_triple();
    let mut missing: Vec<String> = Vec::new();
    for group in crate::plan_groups(&index, ch) {
        let group_missing: Vec<String> = group
            .members
            .iter()
            .filter(|m| wanted.contains(m.as_str()) && !installed.contains_key(m.as_str()))
            .cloned()
            .collect();
        if group_missing.is_empty() {
            continue;
        }
        // Ungrouped members carry a one-element slice, so this is the identical
        // call for both shapes — the difference is only what the slice spans.
        let probe: &[String] = match &group.group {
            Some(_) => &group.members,
            None => &group_missing,
        };
        if crate::flow::group_missing_triple(fetcher, &index, cfg.channel(), triple, probe)
            .is_none()
        {
            missing.extend(group_missing);
        }
    }
    if missing.is_empty() {
        // Nothing left to offer: retire any stale pending-consent row so the
        // Packages page cannot keep showing an offer that is already taken
        // (record_status MERGES the program map, so an untouched row lingers
        // forever — adversarial review 2026-07-30).
        clear_seed_status(layout);
        return;
    }
    missing.sort();
    let list = missing.join(", ");
    // The stable marker the GUI parses — change it and the first-run notice
    // goes blind (crates/aterm-gui, spawn_pkg_update_check).
    println!(
        "atpkg: seed-pending: {} program(s) ready to install from the bundled seed: {list} \
         (Settings ▸ Packages ▸ Install ALab toolset, or `aterm pkg install --default-set`)",
        missing.len()
    );
    // Value-first so the offer survives the Packages card's truncation width
    // (UX review 2026-07-30: "bundled seed offers: ay · 2026-…" truncated the
    // payload away); the page's own "Install ALab toolset" button is the act.
    record_status(
        layout,
        "*seed*",
        crate::ProgramStatus {
            installed_build: None,
            state: format!("pending-consent: {list}"),
            tree_root: String::new(),
        },
        format!("ALab toolchain ready: {list} — Install ALab toolset below"),
    );
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
                             existing dev-link at {} (config wants {}); run `aterm pkg \
                             unlink {program}` first if the config target is right",
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

/// The checkout-relative bins to dev-link when `checkout` is a SYSROOT rather than a cargo
/// project — every name the installed build exposes, under `bin/`.
///
/// WHY A SYSROOT NEEDS ITS OWN ANSWER. `default_link_bins` returns
/// `target/release/<program>`, which is right for a single-binary program and wrong for a
/// toolchain: `trust` publishes ONE program that exposes fifteen commands, and those commands
/// find their own `lib/rustlib` by walking up from the binary they were invoked as (`targo`
/// authenticates its frontend and derives the sysroot from `frontend.parent()`). Linking
/// `target/release/trust` would therefore shim a path that does not exist, and linking a lone
/// `bin/targo` out of a sysroot would shim a binary whose parent has no sibling `lib/` — a
/// toolchain that resolves and then cannot compile. Both failures are silent at link time.
///
/// So the unit for a sysroot is the ROOT: shim every exposed name to `<root>/bin/<name>`, so
/// each one keeps its real neighbours. The exposed set is read from the INSTALLED manifest
/// rather than from `<root>/bin`'s listing, because the manifest is the signed record of what
/// this program is supposed to put on PATH; a dev build with an extra binary lying in `bin/`
/// must not silently gain a shim, and one MISSING an exposed name must fail rather than
/// quietly shim fewer commands than the registry build did.
///
/// Returns `None` when this is not a sysroot link, so the caller falls back unchanged.
fn sysroot_link_bins(
    layout: &crate::store::Layout,
    program: &str,
    checkout: &std::path::Path,
) -> Option<Result<Vec<std::path::PathBuf>, Vec<String>>> {
    // A sysroot is bin/ + lib/rustlib. Requiring BOTH is what keeps an ordinary cargo
    // checkout that happens to have a `bin/` directory on the single-binary path.
    if !checkout.join("bin").is_dir() || !checkout.join("lib").join("rustlib").is_dir() {
        return None;
    }
    let exposes = crate::installed_exposes(layout, program)?;
    if exposes.is_empty() {
        return None;
    }
    // COMPLETENESS IS THE WHOLE POINT, and partial is worse than refused. Linking only the
    // names a dev build happens to carry leaves the REST pointing at the registry build, so
    // `trustc` would be the dev compiler while `trust-wp` stayed on the shipped one — two
    // toolchains with different sysroots, mixed on one PATH, silently. Measured: a stage2
    // built without the trust-wp tools linked 10 of 15 and left 5 shims on build 6459.
    //
    // So a missing exposed name aborts the LINK rather than shrinking it. The user then
    // either builds the missing tools or names an explicit rel-bin list to say they meant a
    // partial link.
    let mut bins = Vec::with_capacity(exposes.len());
    let mut missing = Vec::new();
    for name in &exposes {
        let rel =
            std::path::PathBuf::from("bin").join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if checkout.join(&rel).is_file() {
            bins.push(rel);
        } else {
            missing.push(name.clone());
        }
    }
    if missing.is_empty() {
        missing.sort();
        Some(Ok(bins))
    } else {
        missing.sort();
        Some(Err(missing))
    }
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
            print_unreachable_followup(&e, &format!("aterm pkg update {program}"));
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
            // Same wire as cmd_update_all: an excluded ABSENT sibling must not
            // ride back in on this program's coherence tuple.
            cfg.exclude(),
            floor,
            now_unix(),
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("atpkg: update {program} failed: {e}");
                print_unreachable_followup(&e, &format!("aterm pkg update {program}"));
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
        // The SPARING form, for the same reason as those verbs: a tuple member whose
        // download failed aborted its group but resolved its pin first, and its `.part`
        // is the resume state the next attempt continues from.
        let gc = crate::gc::run_keeping_pinned_partials(&layout, &|p| {
            report.resolved_assets.get(p).cloned()
        });
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
            "atpkg: {program} held by local pin (build {c}); `aterm pkg unpin {program}` to allow updates"
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
                            // KEEP THE RECORDED ROOT. These arms describe a program
                            // that is installed and whose tree did NOT change — a
                            // held pin, a tombstoned pin, the post-state of an
                            // aborted transaction. Writing an empty root there
                            // DELETED the signed attestation of bytes nothing had
                            // touched, and `atpkg verify` treats an empty root as
                            // fail-closed, so a single `atpkg pin trust` turned into
                            // "reinstall 3.2 GB to enable verification"
                            // (2026-08-20 round-8 audit).
                            state: "tombstoned: pin yanked/below floor".into(),
                            tree_root: effective_tree_root(layout, prog, ""),
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
                            tree_root: effective_tree_root(layout, prog, ""),
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
                        tree_root: effective_tree_root(layout, failed, ""),
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
        println!("{EMPTY_VERIFY}");
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
                    "atpkg: {name} — no signed tree_root recorded; aterm pkg install {name} \
                     reinstalls it and records one"
                );
            }
            NotInstalled => {
                bad += 1;
                eprintln!("{}", not_installed_fix(name));
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
    let checkout_path = std::path::Path::new(checkout);
    let bins: Vec<std::path::PathBuf> = if rest.len() > 2 {
        rest[2..].iter().map(std::path::PathBuf::from).collect()
    } else if let Some(sysroot) = sysroot_link_bins(&layout, program, checkout_path) {
        // A TOOLCHAIN, not a cargo project: shim every exposed name out of `<root>/bin` so
        // each keeps its sibling `lib/` (see `sysroot_link_bins`). An explicit rel-bin list
        // still wins — this only replaces the DEFAULT.
        match sysroot {
            Ok(bins) => {
                println!(
                    "atpkg: {checkout} is a sysroot — linking {} exposed command(s) from bin/",
                    bins.len()
                );
                bins
            }
            Err(missing) => {
                eprintln!(
                    "atpkg: refusing to dev-link {program}: {} of its exposed command(s) are \
                     missing from {checkout}/bin — {}",
                    missing.len(),
                    missing.join(", ")
                );
                eprintln!(
                    "atpkg:   linking the rest would leave those commands on the INSTALLED \
                     build while the others moved to your checkout — one PATH, two toolchains,"
                );
                eprintln!(
                    "atpkg:   different sysroots. Build the missing tools, or pass an explicit \
                     rel-bin list to say you meant a partial link."
                );
                return ExitCode::from(1);
            }
        }
    } else {
        // Default to the conventional cargo release bin, WITH the platform executable
        // extension (`target/release/<program>.exe` on Windows) — else `src.is_file()`
        // never matches the real exe and the link silently yields NoBins. Shared with
        // the `[packages.links]` reconciliation.
        default_link_bins(program)
    };
    match crate::link(&layout, program, checkout_path, &bins) {
        Ok(out) => {
            println!(
                "atpkg: dev-linked {program} ({}); `aterm pkg unlink {program}` to release it",
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

/// `atpkg unlink <program>` (§13) — remove a program's dev links and its marker, then PUT
/// THE INSTALLED BUILD BACK ON PATH. A pin is preserved across the cycle.
///
/// RESTORATION IS PART OF UNLINK, not a follow-up the user is expected to remember. Removing
/// the dev shims alone leaves every command the link covered simply ABSENT: measured on a
/// 15-command toolchain, a link/unlink cycle took `targo`, `trustc` and `tippy` off PATH
/// entirely while leaving the commands the dev build had not carried still pointing at the
/// installed build. "Your compiler disappeared" is not an acceptable resting state for a
/// verb whose whole job is to undo something, and a partial PATH is the same split-toolchain
/// hazard `link` now refuses to create.
///
/// The store build is guaranteed to still be there: gc's claim union counts the per-program
/// `current` link, so a dev-linked program's build is never reclaimable (measured — gc
/// reports "nothing to reclaim" while linked). Restoration is therefore just re-asserting
/// shims that already have a target, never a download.
///
/// Best-effort by design: a program that was never installed (dev-linked from the start) has
/// nothing to restore, and that is reported rather than treated as failure.
fn cmd_unlink(program: Option<&String>) -> ExitCode {
    let Some(program) = program else {
        eprintln!("usage: atpkg unlink <program>");
        return ExitCode::from(2);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    // Read the marker BEFORE `unlink` removes it: it is the only record of which names this
    // link took over, and restoring anything else would touch programs that were never linked.
    let owned = crate::linked_tool_names(&layout, program);
    match crate::unlink(&layout, program) {
        Ok(()) => {
            println!("atpkg: unlinked {program} (any pin is preserved)");
            match restore_installed_shims(&layout, program, owned.as_deref()) {
                Ok(0) => println!(
                    "atpkg:   no installed build to restore — {program} is off PATH until \
                     `aterm pkg install {program}`"
                ),
                Ok(n) => println!("atpkg:   restored {n} command(s) from the installed build"),
                Err(e) => eprintln!(
                    "atpkg:   WARNING: could not restore the installed build's shims ({e}); \
                     run `aterm pkg install {program}` to put it back on PATH"
                ),
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: unlink {program} failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// Re-assert `program`'s shims from its CURRENT store build. Returns how many were written,
/// or 0 when the program has no installed build.
///
/// `owned` is the set the link took over (from its marker), and restoration is confined to
/// it — see the note inside on why the build's `bin/` listing is NOT a safe substitute. Each
/// name must also exist in the installed build, and still passes the same admission the
/// install path uses (`ToolName::new` — the sensitive-name deny-list), so a restore can no
/// more shadow `git`/`sudo` than an install can.
fn restore_installed_shims(
    layout: &crate::store::Layout,
    program: &str,
    owned: Option<&[String]>,
) -> std::io::Result<usize> {
    let current = layout.program_current(program);
    if !current.exists() {
        return Ok(0);
    }
    let build_dir = std::fs::canonicalize(&current)?;
    let bin = build_dir.join("bin");
    if !bin.is_dir() {
        return Ok(0);
    }
    // ONLY the names the link owned. A sysroot bundle's `bin/` carries backends belonging to
    // OTHER atpkg programs — `trust`'s carries `ay`, `clean`, `ty` — so restoring from the
    // directory listing repoints those programs' shims at this build. Measured: it clobbered
    // all three onto `store/trust/6459`. The marker's set is scoped to this link, which makes
    // the restore symmetric with the takeover.
    let Some(owned) = owned else {
        return Ok(0);
    };
    let mut tools = Vec::new();
    for name in owned {
        // Still require the binary to exist in the installed build: the dev checkout and the
        // registry build need not expose the same set, and a shim with nothing behind it is
        // worse than an absent one.
        let file = bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if !file.is_file() {
            continue;
        }
        if let Some(tool) = crate::store::ToolName::new(name) {
            tools.push(tool);
        }
    }
    if tools.is_empty() {
        return Ok(0);
    }
    crate::activate::install_tools(layout, &build_dir, &tools)?;
    Ok(tools.len())
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

    /// A translated process must never reach a permanent "wrong architecture"
    /// verdict: `current_triple()` is a compile-time cfg, so the x86_64 slice of the
    /// universal binary says x86_64 on an Apple Silicon Mac, and the verdict that
    /// follows deleted the seal from a machine that could have installed every
    /// program natively (2026-08-20 round-8 audit).
    #[test]
    fn a_translated_process_is_not_evidence_about_this_mac() {
        // On the native arm64 slice this is false, which is the pre-existing path.
        // The point of the test is that the decision CONSULTS it at all: the arch
        // arm of `finish_unusable_seed` must be gated on it before setting
        // `permanent`, or the reclaim is keyed on a compile-time constant.
        let src = include_str!("cli.rs");
        let arm = src
            .find("the bundled toolchain has no build for this machine's")
            .expect("the architecture verdict");
        let guard = src[..arm]
            .rfind("running_translated()")
            .expect("a translation check before the architecture verdict");
        let permanent = src[..arm]
            .rfind("permanent = true;")
            .expect("the permanent assignment");
        assert!(
            guard < permanent,
            "the translation check must come BEFORE the permanent architecture verdict \
             it gates"
        );
        assert!(!running_translated() || cfg!(target_arch = "x86_64"));
    }

    /// An unattended update must not undo a deliberate uninstall. The coherence
    /// rule ("a group with any installed member is applied whole, missing siblings
    /// pulled in") re-downloaded ~3.2 GB of `trust` on the next tick after the user
    /// removed it (2026-08-20 round-8 audit).
    #[test]
    fn a_removed_program_is_read_from_the_layout_by_every_lane() {
        let root = std::env::temp_dir().join(format!("atpkg-removed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let layout = crate::store::resolve(Some(&root)).expect("layout");
        std::fs::write(layout.removed(), "# deliberate\ntrust\n\n").unwrap();
        let removed = layout.removed_programs();
        assert!(
            removed.contains("trust"),
            "the durable record is the answer"
        );
        assert_eq!(removed.len(), 1, "comments and blanks are not programs");
        // The CLI reader and the flow reader must be the same answer, or the
        // unattended lane disagrees with the verb the user ran.
        assert_eq!(removed, removed_programs(&layout));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A seal inside a build output belongs to whatever produced it. Deleting one
    /// took a gigabyte out of a release mid-package on 2026-08-19; every other
    /// guard passed, because the app really was ours and the payload really was
    /// spent for this machine.
    #[test]
    fn a_seal_in_a_build_output_is_not_ours_to_reclaim() {
        let root = std::env::temp_dir().join(format!("atpkg-buildout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let seed_in = |app: &std::path::Path| app.join("Contents/Resources/toolchain-seed.lproj");

        // The cutter's own marker, written by `aterm-release`'s bundle::assemble.
        let cut = root.join("dist/cut-app");
        std::fs::create_dir_all(seed_in(&cut.join("aterm.app"))).unwrap();
        std::fs::write(cut.join(".metadata_never_index"), "").unwrap();
        assert!(
            is_build_output_bundle(&seed_in(&cut.join("aterm.app"))),
            "a bundle beside .metadata_never_index is an artifact, not an install"
        );

        // A real install has no such marker beside it, and still reclaims.
        let installed = root.join("Applications");
        std::fs::create_dir_all(seed_in(&installed.join("aterm.app"))).unwrap();
        assert!(
            !is_build_output_bundle(&seed_in(&installed.join("aterm.app"))),
            "an ordinary install must still reclaim its spent payload"
        );

        // Fail-safe: an unreadable/absent marker reads as a real install.
        assert!(!is_build_output_bundle(std::path::Path::new(
            "/nonexistent/aterm.app/Contents/Resources/toolchain-seed.lproj"
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

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

    /// [`VERB_TIERS`] must be an EXACT partition of [`VERBS`]: every dispatched verb
    /// placed in exactly one tier, and no tier naming a verb that does not dispatch.
    /// Without this, a verb added to the match with no tier would still parse but
    /// silently vanish from bare `atpkg`, `--help`, and the unknown-verb hint at once —
    /// the same drift that killed `sync` and `seed` when the roster was hand-kept.
    #[test]
    fn verb_tiers_partition_the_roster() {
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for (tier, verbs) in VERB_TIERS {
            for v in *verbs {
                assert!(
                    seen.insert(v),
                    "{v} appears in more than one tier (second: {tier}) — a verb has ONE home"
                );
                assert!(
                    VERBS.contains(v),
                    "{v} is tiered ({tier}) but not dispatched — the tiers advertise it \
                     while the match rejects it"
                );
            }
        }
        for v in VERBS {
            assert!(
                seen.contains(v),
                "{v} dispatches but has no tier — it would vanish from every human surface"
            );
        }
    }

    /// The shared tier block renders each advertised verb exactly once as a word —
    /// `status` folded into `doctor (or: status)` rather than dropped.
    #[test]
    fn tier_block_renders_every_verb_once() {
        let block = verb_tier_lines().join("\n");
        for v in VERBS {
            let count = block
                .split(|c: char| !(c.is_alphanumeric() || c == '-'))
                .filter(|w| w == v)
                .count();
            assert_eq!(count, 1, "{v} must appear exactly once in the tier block:\n{block}");
        }
        assert!(
            block.contains("doctor (or: status)"),
            "status renders attached to doctor, not as a sibling:\n{block}"
        );
    }

    /// The suggestion engine: a one-slip typo gets the fix, gibberish gets silence —
    /// a far-fetched guess erodes trust in the near ones.
    #[test]
    fn did_you_mean_suggests_and_gives_up() {
        assert_eq!(did_you_mean("instal"), Some("install"));
        assert_eq!(did_you_mean("udpate"), Some("update"));
        assert_eq!(did_you_mean("frobnicate"), None);
    }

    /// The bare posture's script contract: the enabled HEAD never moves (counts ride
    /// after the em dash), the disabled line is byte-exact, and the tail names exactly
    /// one `next:` act.
    #[test]
    fn bare_posture_head_is_stable() {
        for counts in [None, Some((0, 0)), Some((10, 10))] {
            let lines = bare_lines(counts);
            assert!(
                lines[0].starts_with("atpkg: enabled (root key pinned)"),
                "the stable head leads: {:?}",
                lines[0]
            );
            let nexts = lines.iter().filter(|l| l.starts_with("atpkg: next:")).count();
            assert_eq!(
                nexts,
                usize::from(counts.is_some()),
                "one next act with a store, none without: {lines:?}"
            );
        }
        // Fresh machine: the one act is the install, and it says what it does.
        assert!(
            bare_lines(Some((0, 0)))
                .last()
                .unwrap()
                .contains("aterm pkg install --default-set"),
            "an empty store's next act is the whole-set install"
        );
    }

    /// NO DEAD ENDS: every empty/negative answer carries a `aterm pkg …` act the user
    /// can paste — the fact alone ("ty is not installed") always raises exactly one
    /// follow-up question, and the first hour should never need a second guess.
    #[test]
    fn empty_answers_name_a_next_command() {
        let family: Vec<String> = vec![
            EMPTY_LIST_HUMAN.to_string(),
            EMPTY_UPDATE.to_string(),
            EMPTY_VERIFY.to_string(),
            SEED_FOLLOW_ON.to_string(),
            not_installed_fix("ty"),
        ];
        for answer in &family {
            assert!(
                answer.contains("aterm pkg "),
                "an empty/negative answer must name its next command: {answer}"
            );
        }
    }

    /// The piped `list` form is a SCRIPT CONTRACT: `program\tbuild\tnotes`, headerless,
    /// ascending builds with superseded rows before the live one — byte-identical to
    /// every earlier release, dev-link and pin notes included.
    #[test]
    fn list_porcelain_is_stable() {
        let installed = vec![
            ("ay".to_string(), 7987),
            ("ay".to_string(), 8256),
            ("clean".to_string(), 7345),
        ];
        let live: std::collections::BTreeMap<String, u64> =
            [("ay".to_string(), 8256), ("clean".to_string(), 7345)].into();
        let linked = std::collections::BTreeMap::new();
        let mut pinned = std::collections::BTreeSet::new();
        pinned.insert("clean".to_string());
        assert_eq!(
            list_porcelain_lines(&installed, &live, &linked, &pinned),
            vec![
                "ay\t7987\tsuperseded (kept for rollback)".to_string(),
                "ay\t8256\tlive".to_string(),
                "clean\t7345\tlive  pinned".to_string(),
            ],
        );
    }

    /// The human report leads with the verdict-shaped summary, folds superseded rows
    /// into a per-program count, and appends its one `next` act only when some program
    /// has no live build (counts only, no health adjective — "healthy" is doctor's word).
    #[test]
    fn list_human_report_summarizes_and_names_doctor_only_when_needed() {
        let installed = vec![
            ("ay".to_string(), 7987),
            ("ay".to_string(), 8256),
            ("clean".to_string(), 7345),
        ];
        let live: std::collections::BTreeMap<String, u64> =
            [("ay".to_string(), 8256), ("clean".to_string(), 7345)].into();
        let none_linked = std::collections::BTreeMap::new();
        let none_pinned = std::collections::BTreeSet::new();
        let lines = list_human_lines(&installed, &live, &none_linked, &none_pinned);
        assert_eq!(lines[0], "atpkg: 2 program(s) — 2 live");
        assert!(
            lines.iter().any(|l| l.contains("(1 older build(s) kept for rollback)")),
            "the superseded build survives as a count: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("next")),
            "a healthy listing names no next act: {lines:?}"
        );
        // A program with builds on disk and nothing live is the row that needs doctor.
        let dead_live: std::collections::BTreeMap<String, u64> =
            [("ay".to_string(), 8256)].into();
        let lines = list_human_lines(&installed, &dead_live, &none_linked, &none_pinned);
        assert_eq!(lines[0], "atpkg: 2 program(s) — 1 live, 1 without a live build");
        assert!(
            lines.iter().any(|l| l.contains("NO LIVE BUILD")),
            "the dead program is loud: {lines:?}"
        );
        assert_eq!(
            lines.last().unwrap(),
            "atpkg: next — aterm pkg doctor",
            "exactly one next act, at the end: {lines:?}"
        );
    }

    /// `list` with no resolvable prefix exits 1 like every sibling read verb — it used
    /// to print the error and exit 0, a silent green in an already-broken environment.
    #[test]
    fn list_exit_codes() {
        assert_eq!(
            format!("{:?}", run_list(None, true)),
            format!("{:?}", ExitCode::from(1)),
            "no layout is a structural failure, not a success"
        );
    }

    /// UNINSTALLING WHAT IS NOT THERE IS A REFUSAL, NOT A SUCCESS. This used to print
    /// "uninstalled ty", exit 0, and mint a durable removed-marker that silently
    /// suppressed future set-completion — a fabricated success with a hidden state
    /// change. Partial-install debris (a store tree with no shims) must still clean.
    #[test]
    fn uninstall_not_installed_refuses() {
        let layout = temp_layout("uninstall-absent");
        assert_eq!(
            format!("{:?}", uninstall_one(&layout, "ty")),
            format!("{:?}", ExitCode::from(1)),
            "nothing bears the name, so nothing was uninstalled"
        );
        assert!(
            !removed_programs(&layout).contains("ty"),
            "no removed-marker is minted for a program that was never here"
        );
        // Partial debris: a store tree with no shims is still this verb's to clean.
        std::fs::create_dir_all(layout.build_dir("ty", 7)).unwrap();
        assert_eq!(
            format!("{:?}", uninstall_one(&layout, "ty")),
            format!("{:?}", ExitCode::SUCCESS),
            "debris cleans as before"
        );
        assert!(
            !layout.prefix.join("store").join("ty").exists(),
            "the debris really came off"
        );
        assert!(
            removed_programs(&layout).contains("ty"),
            "a real removal still records itself for the seed lane"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// THE SPELLING RULE CANNOT REGRESS AT THE SHAPES THAT ALREADY SLIPPED. Anything a
    /// user is invited to TYPE is spelled `aterm pkg …`; the bare `atpkg` spelling in a
    /// remedy only works because of the argv0 alias, which a first-hour user has no way
    /// to know about. Source-level, because these strings are scattered across four
    /// files and every one of them drifted at least once.
    #[test]
    fn remedies_are_spelled_as_typed() {
        for (file, source) in [
            ("cli.rs", include_str!("cli.rs")),
            ("doctor.rs", include_str!("doctor.rs")),
            ("flow.rs", include_str!("flow.rs")),
            ("activate.rs", include_str!("activate.rs")),
        ] {
            // Needles assembled at runtime so this test's own source (inside
            // cli.rs's include_str!) never contains them.
            for head in ["try: ", "run `", ": "] {
                let needle = format!("{head}atpkg {}", if head == ": " { "install --default-set" } else { "" });
                assert!(
                    !source.contains(&needle),
                    "{file} still spells a remedy as `atpkg` ({needle:?}) — remedies say \
                     `aterm pkg`, the spelling a user can paste"
                );
            }
        }
    }

    fn temp_layout(label: &str) -> crate::store::Layout {
        let p = std::env::temp_dir().join(format!("atpkg-main-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        crate::store::Layout { prefix: p }
    }

    /// A verdict the AUTHORITY never gave must not become a resident of the record.
    ///
    /// `NotReachable` means the signed index resolved, verified, and does not name the
    /// token — a fact about the request, not about this store. Recording it as a program
    /// state invents a program, and `atpkg doctor` reads every such row back as a missing
    /// member of the toolset.
    #[test]
    fn a_name_the_index_does_not_name_is_never_recorded() {
        assert_eq!(
            failed_install_state(&crate::FlowError::NotReachable("--help".into(), vec![])),
            None,
            "the index does not name it, so there is no program to remember"
        );
        assert_eq!(
            failed_install_state(&crate::FlowError::Linked("ay".into())),
            Some(String::from("dev-linked (skipped)")),
            "a dev-linked program is a benign hard-skip (§13), and still says so"
        );
        // THE OFFLINE FAMILY, all raised before the program lookup ever happens. Recording
        // any of these against a program says the program is broken; what happened is that
        // the machine could not reach the channel.
        for env in [
            crate::FlowError::NoIndex,
            crate::FlowError::Unreachable("dns".into()),
            crate::FlowError::Stale,
        ] {
            assert_eq!(
                failed_install_state(&env),
                None,
                "{env} is a fact about the network, not about any program"
            );
        }
        // …while a genuine per-program fact about THIS machine still lands on the record.
        assert_eq!(
            failed_install_state(&crate::FlowError::NoArtifact("aarch64".into())),
            Some(format!(
                "error: {}",
                crate::FlowError::NoArtifact("aarch64".into())
            )),
            "no build for this Mac's triple is a real, durable fact about the program"
        );
        assert_eq!(
            failed_install_state(&crate::FlowError::PkgVerify),
            Some(format!("error: {}", crate::FlowError::PkgVerify)),
            "a manifest that failed its signature check is a fault worth remembering"
        );
    }

    /// Put `program` genuinely LIVE in `layout` at `build` — real build tree, real shims,
    /// real channel pointer — so `active_builds` reports it the way it does on a machine.
    /// A stubbed record would not exercise the thing under test.
    fn make_live(layout: &crate::store::Layout, program: &str, build: u64) {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let t = crate::store::ToolName::new(program).unwrap();
        std::fs::write(dir.join("bin").join(t.exe_file()), b"#!/bin/true\n").unwrap();
        crate::activate::install_shims(layout, &dir, &[program.to_string()]).unwrap();
        crate::activate::activate_channel(layout, "stable", &dir).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
    }

    /// A FAILED INSTALL UNINSTALLS NOTHING — so it must not report that it did.
    ///
    /// An upgrade that dies at download/stage/activate leaves the previous build activated,
    /// shimmed and working. Every failure arm nonetheless wrote `installed_build: None` and
    /// `tree_root: ""`, which says the program is gone AND unattested. The second is
    /// destructive: `crate::verify` fails closed on an empty root, so ONE failed upgrade
    /// turned `atpkg verify` into a permanent "cannot verify" for bytes that were never
    /// touched — recoverable only by reinstalling gigabytes to re-record a root that had
    /// been correct the whole time.
    #[test]
    fn a_failed_install_keeps_the_live_build_and_its_attestation() {
        let layout = temp_layout("failure-row");
        make_live(&layout, "ay", 18);
        record_status(
            &layout,
            "ay",
            crate::ProgramStatus {
                installed_build: Some(18),
                state: "active".into(),
                tree_root: "0fda8eaefbfae96ac95ffb83afce208e".into(),
            },
            "up to date".into(),
        );

        // An upgrade to 20 dies mid-flight.
        let row = failure_row(&layout, "ay", String::from("error: download failed"));
        assert_eq!(
            row.installed_build,
            Some(18),
            "build 18 is still activated and shimmed; the record must not claim otherwise"
        );
        assert_eq!(
            row.tree_root, "0fda8eaefbfae96ac95ffb83afce208e",
            "the signed attestation for untouched bytes survives a failed upgrade"
        );
        assert_eq!(
            row.state, "error: download failed",
            "the failure is still reported"
        );

        // …while a program that was never installed still records nothing to preserve.
        let fresh = failure_row(&layout, "clean", String::from("error: download failed"));
        assert_eq!(fresh.installed_build, None);
        assert!(fresh.tree_root.is_empty());

        // THE RECORD DISAGREES WITH THE STORE — the case the two fields are read from
        // different sources, and the one that makes stapling them together dangerous. The
        // store is now live at 20 while the record still says 18/root18. Taking the build
        // from the store and the root from the record would produce "build 20, attested by
        // build 18's signature", which DISARMS verify's `BuildMismatch` guard and lands in
        // the tree comparison: `Drift` (a tampering accusation against untouched bytes) or,
        // worse, `Match` — affirming a build no signature ever covered.
        make_live(&layout, "ay", 20);
        assert_eq!(
            crate::active_builds(&layout).get("ay").copied(),
            Some(20),
            "precondition: the store moved on and the record has not caught up"
        );
        let stitched = failure_row(&layout, "ay", String::from("error: download failed"));
        assert_eq!(
            stitched.installed_build,
            Some(20),
            "the store is ground truth for what is live"
        );
        assert!(
            stitched.tree_root.is_empty(),
            "an attestation captured for build 18 must NOT be stapled onto build 20 — an \
             honest `NoSignedRoot` beats a false `Drift`, and beats a false `Match` by \
             considerably more"
        );

        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// A TOMBSTONED PROGRAM KEEPS ITS ATTESTATION. Its shims were replaced by failing stubs
    /// so `active_builds` goes silent, but its bytes and their signed root sit untouched in
    /// the store — and the build-agreement rule above must not read that silence as
    /// disagreement and throw the root away on the strength of a yanked pin.
    #[test]
    fn a_tombstoned_program_does_not_lose_its_signed_root() {
        let layout = temp_layout("tombstone-root");
        record_status(
            &layout,
            "trust",
            crate::ProgramStatus {
                installed_build: Some(6459),
                state: "active".into(),
                tree_root: "abc123".into(),
            },
            "up to date".into(),
        );
        // No shims at all: exactly what `active_builds` sees for a tombstoned program.
        assert_eq!(crate::active_builds(&layout).get("trust").copied(), None);

        let row = failure_row(&layout, "trust", String::from("tombstoned: pin yanked"));
        assert_eq!(
            row.tree_root, "abc123",
            "nothing contradicts the recorded root, so it survives"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// A ROW MUST NOT OUTLIVE ITS PROGRAM.
    ///
    /// `active_builds` reads shims, so doctor went quiet after an uninstall — but Settings ▸
    /// Packages reads the RECORD, and listed a deleted program as `active` at its last build
    /// forever. The stale-row reconciler cannot fix this and correctly declines to: the
    /// signed index still names the program, it is this MACHINE that no longer carries it.
    #[test]
    fn uninstalling_retires_the_program_row() {
        let layout = temp_layout("retire-row");
        make_live(&layout, "ay", 18);
        make_live(&layout, "clean", 7);
        record_status(
            &layout,
            "ay",
            crate::ProgramStatus {
                installed_build: Some(18),
                state: "active".into(),
                tree_root: "deadbeef".into(),
            },
            "up to date".into(),
        );
        record_status(
            &layout,
            "clean",
            crate::ProgramStatus {
                installed_build: Some(7),
                state: "active".into(),
                tree_root: "cafe".into(),
            },
            "up to date".into(),
        );
        assert!(
            crate::active_builds(&layout).contains_key("ay"),
            "precondition: ay is genuinely live"
        );

        // THE REAL PATH the CLI arm runs — not the primitive it happens to call.
        uninstall_and_retire(&layout, "ay").expect("uninstall succeeds");

        assert!(
            !crate::active_builds(&layout).contains_key("ay"),
            "the program really came off"
        );
        assert!(
            removed_programs(&layout).contains("ay"),
            "and the seed lane is told not to put it back"
        );
        let s = crate::status::read(&layout).expect("record present");
        assert!(
            !s.programs.contains_key("ay"),
            "the removed program's row is retired, not left reading `active`"
        );
        assert!(
            s.programs.contains_key("clean"),
            "and retiring one program does not disturb the others"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// ROW DELETION IS A NEW CAPABILITY, so the names it accepts are gated.
    ///
    /// `ops::uninstall`'s shape rule rejects only empty/`.`/`..`/separators/NUL, so before
    /// the gate `atpkg uninstall '*toolset*'` returned Ok and DELETED a bookkeeping row that
    /// has its own owner and its own reaper, and `atpkg uninstall --help` appended a flag to
    /// the removed-markers file — which would then suppress a program named `--help` forever,
    /// a fittingly absurd end for the bug that started this.
    #[test]
    fn retirement_refuses_names_that_are_not_programs() {
        let layout = temp_layout("retire-gate");
        record_status(
            &layout,
            "*toolset*",
            crate::ProgramStatus {
                installed_build: None,
                state: "unavailable: no build for this architecture".into(),
                tree_root: String::new(),
            },
            "no build published".into(),
        );

        assert!(
            uninstall_and_retire(&layout, "*toolset*").is_err(),
            "a sentinel bookkeeping row is not a program and is not this verb's to retire"
        );
        assert!(
            crate::status::read(&layout)
                .expect("record present")
                .programs
                .contains_key("*toolset*"),
            "…and it is still there"
        );
        assert!(
            uninstall_and_retire(&layout, "--help").is_err(),
            "a flag is not a program name here either"
        );
        assert!(
            !removed_programs(&layout).contains("--help"),
            "no removed-marker is minted for a flag"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// AN OFFLINE FIRST RUN MUST STILL LEAVE A WITNESS.
    ///
    /// Suppressing the environmental failures' per-program rows routes their reason to the
    /// outcome sentence and NOWHERE else — so the sentence has to survive a virgin store.
    /// While `record_outcome` early-returned when no record existed, a first-run offline
    /// install recorded nothing at all: exactly the failed-seed machine that gets pointed
    /// at `pkg doctor`, and the one case doctor's fallback to this sentence exists for.
    #[test]
    fn the_outcome_sentence_creates_the_record_it_needs() {
        let layout = temp_layout("virgin-outcome");
        assert!(
            crate::status::read(&layout).is_none(),
            "precondition: this store has never been written to"
        );
        record_outcome(
            &layout,
            String::from("install ay: no signature-valid index"),
        );
        let s = crate::status::read(&layout).expect("a first-run failure still leaves a witness");
        assert_eq!(s.outcome, "install ay: no signature-valid index");
        assert!(
            s.programs.is_empty(),
            "the witness records what happened without blaming a program for the network"
        );
        assert_eq!(s.schema, 1, "a created record is a well-formed one");
    }

    /// The record is a DERIVED memo; the signed index is the authority on what exists.
    ///
    /// Pins the four conditions together, because each one alone is a different bug: prune
    /// a sentinel and the bookkeeping rows lose their reapers; prune a live-but-dropped
    /// program and `atpkg verify` fails closed forever on an erased attestation.
    #[test]
    fn the_record_reconciles_against_the_signed_index() {
        let row = |state: &str| crate::ProgramStatus {
            installed_build: None,
            state: state.into(),
            tree_root: String::new(),
        };
        let recorded = std::collections::BTreeMap::from([
            (
                "--help".to_string(),
                row("error: --help is not named in the signed index"),
            ),
            (
                "ayy".to_string(),
                row("error: ayy is not named in the signed index"),
            ),
            ("ay".to_string(), row("active")),
            (
                "*toolset*".to_string(),
                row("unavailable: no build for this Mac"),
            ),
            ("retired".to_string(), row("active")),
        ]);
        let live = std::collections::BTreeMap::from([
            ("ay".to_string(), 18_u64),
            ("retired".to_string(), 5_u64),
        ]);
        let pruned = prunable_status_rows(&recorded, &live, &|n| n == "ay");

        assert_eq!(
            pruned,
            vec!["--help".to_string(), "ayy".to_string()],
            "only the rows naming nothing the index knows and nothing the store carries"
        );
        assert!(
            !pruned.contains(&"*toolset*".to_string()),
            "sentinel bookkeeping rows have their own owners and their own reapers"
        );
        assert!(
            !pruned.contains(&"retired".to_string()),
            "a program dropped upstream while still LIVE keeps its row — deleting it would \
             erase the attestation `atpkg verify` fails closed without"
        );
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
        // EXHAUSTIVE over VERBS: a verb added to the dispatch without a place in
        // one of these two lists fails here rather than silently defaulting to
        // lock-free. The `seed` verb landed while this test only enumerated the
        // verbs it already knew, so it classified nothing and proved nothing
        // about the new store mutator.
        let mut classified: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for mutator in [
            "install",
            "seed",
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
            classified.insert(mutator);
            assert!(
                verb_mutates_store(mutator),
                "{mutator} mutates the store and must take the lock"
            );
        }
        for read_only in [
            "doctor",
            // `status` is the same read-only report under the name people reach for.
            "status",
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
            classified.insert(read_only);
            assert!(
                !verb_mutates_store(read_only),
                "{read_only} is read-only and must stay lock-free"
            );
        }
        let unclassified: Vec<&&str> = VERBS.iter().filter(|v| !classified.contains(*v)).collect();
        assert!(
            unclassified.is_empty(),
            "every advertised verb must be classified as a store mutator or read-only here \
             (a new verb defaults to lock-free, which is the dangerous direction): \
             {unclassified:?}"
        );
    }

    /// The bundled seed is a BOOTSTRAP source and nothing else (§9.1) — the
    /// premise both 2026-07-30 teeth rest on. A populated store must never
    /// re-arm the chain: that is what kept sealed pins from rolling installed
    /// builds backwards on the unattended pass, and what keeps a seed-leg
    /// success from standing in for a network answer once there is real state
    /// to protect. Pinned here because the inline version of this rule shipped
    /// twice with no test.
    #[test]
    fn the_seed_leg_joins_only_on_an_empty_store() {
        let seed =
            std::path::PathBuf::from("/Applications/aterm.app/Contents/Resources/toolchain-seed");
        assert_eq!(
            seed_bootstrap_leg(Some(seed.clone()), true),
            Some(seed.clone()),
            "an empty store with a sealed seed is exactly the bootstrap case"
        );
        assert_eq!(
            seed_bootstrap_leg(Some(seed), false),
            None,
            "a populated store must NOT re-arm the seed chain — the seed is never an \
             update source (downgrade + cache-masking teeth, adversarial review 2026-07-30)"
        );
        // No seed sealed: network-only in both store states, never a chain over
        // a path that does not exist.
        assert_eq!(seed_bootstrap_leg(None, true), None);
        assert_eq!(seed_bootstrap_leg(None, false), None);
    }

    /// ADOPTION is what keeps a user's toolchain the WHOLE toolchain as the suite grows
    /// (§11): consent to a multi-GB download is asked once (`auto_install`), but a machine
    /// that already runs the set gets newly-published members without being asked again.
    /// The lifecycle is small enough to pin exactly, and getting it wrong is silent in both
    /// directions — a missing marker decays the toolset, a sticky one reinstalls what the
    /// user deliberately removed.
    #[test]
    fn adoption_is_recorded_by_the_whole_set_and_forgotten_by_uninstall() {
        let dir = std::env::temp_dir().join(format!("atpkg-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let layout = crate::store::Layout {
            prefix: dir.clone(),
        };

        // A fresh store has NOT adopted: the consent question is still open.
        assert!(!adopted(&layout), "a fresh store has adopted nothing");

        // A whole-set install records it, and is idempotent.
        record_adoption(&layout);
        assert!(adopted(&layout), "the whole-set install adopts");
        let first = std::fs::read(layout.adopted()).unwrap();
        record_adoption(&layout);
        assert_eq!(
            std::fs::read(layout.adopted()).unwrap(),
            first,
            "re-adopting rewrites nothing"
        );

        // The marker is 0600 like every other durable store file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(layout.adopted())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "adoption marker must be 0600");
        }

        // Adoption is the file's EXISTENCE — never a parsed value, so a garbled or
        // truncated write can never read as some other answer.
        std::fs::write(layout.adopted(), b"\0\0garbage").unwrap();
        assert!(adopted(&layout), "existence is the whole predicate");

        // Uninstall forgets it: set-completion must never undo an explicit removal.
        clear_adoption(&layout);
        assert!(!adopted(&layout), "uninstall opts out of set completion");
        // Clearing what is already clear is not an error.
        clear_adoption(&layout);
        assert!(!adopted(&layout));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A TARGETED removal must stick too. The resumable seed lane installs whatever
    /// the store lacks, so without a per-program record `atpkg uninstall ny` came
    /// back on the next launch — the manager undoing a deliberate act, which is the
    /// single most infuriating thing a package manager can do. Asking for it back
    /// lifts the record, so nobody has to learn a marker file exists.
    #[test]
    fn a_single_program_removal_is_not_undone_by_the_seed_lane() {
        let dir = std::env::temp_dir().join(format!("atpkg-rm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let layout = crate::store::Layout {
            prefix: dir.clone(),
        };

        assert!(removed_programs(&layout).is_empty());
        record_removed(&layout, "ny");
        record_removed(&layout, "clean");
        record_removed(&layout, "ny"); // idempotent
        let rm = removed_programs(&layout);
        assert!(rm.contains("ny") && rm.contains("clean"), "{rm:?}");
        assert_eq!(rm.len(), 2, "no duplicate from the repeat: {rm:?}");
        // The comment header must never read as a program name.
        assert!(!rm.iter().any(|p| p.starts_with('#')), "{rm:?}");

        clear_removed(&layout, &["ny".to_string()]);
        let rm = removed_programs(&layout);
        assert!(
            !rm.contains("ny"),
            "an explicit install lifts just that one"
        );
        assert!(rm.contains("clean"), "and leaves the others alone");

        clear_removed(&layout, &["clean".to_string()]);
        assert!(
            removed_programs(&layout).is_empty(),
            "emptying the set removes the file rather than leaving a header behind"
        );
        assert!(!layout.removed().exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// REMOVAL MUST STICK. The seed lane is resumable — it installs whatever the
    /// store lacks, so an interrupted first run can finish later — and that same
    /// property means it would happily reinstall everything `uninstall --all` just
    /// removed, on the very next launch. The decline marker is what stops that, and
    /// an explicit install is what lifts it. Both directions are pinned here because
    /// getting either wrong is user-hostile in a way tests are the only defence
    /// against: one way the manager fights the user, the other way a change of mind
    /// requires deleting a file nobody documented.
    #[test]
    fn declining_the_toolset_survives_until_an_explicit_install() {
        let dir = std::env::temp_dir().join(format!("atpkg-decline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let layout = crate::store::Layout {
            prefix: dir.clone(),
        };

        assert!(!declined(&layout), "a fresh store has declined nothing");
        record_decline(&layout);
        assert!(declined(&layout), "uninstall --all declines the toolset");
        // Idempotent, and 0600 like every other durable marker.
        record_decline(&layout);
        assert!(declined(&layout));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(layout.declined())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "decline marker must be 0600");
        }
        // Existence is the predicate — never a parsed value.
        std::fs::write(layout.declined(), b"\0garbage").unwrap();
        assert!(declined(&layout));

        clear_decline(&layout);
        assert!(!declined(&layout), "an explicit install lifts the decline");
        clear_decline(&layout); // clearing what is clear is not an error

        // Decline and adoption are INDEPENDENT facts: declining must not read as
        // adoption, or a removed toolset would still get set-completion.
        record_decline(&layout);
        assert!(!adopted(&layout), "declining is not adopting");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The PRODUCING half of the cross-crate seed-marker contract
    /// (crates/aterm-gui `parse_seed_markers` is the consuming half, and its
    /// own tests feed it strings it wrote itself — which is why the contract
    /// broke undetected once already). These are the exact prefixes `cmd_seed`
    /// and `announce_pending_seed` print; if either moves, the GUI's first-run
    /// notice goes blind, so the literals are pinned on this side too.
    #[test]
    fn the_seed_marker_prefixes_are_the_ones_the_gui_parses() {
        // Search PRODUCTION code only. The previous version searched the whole file,
        // including this test — whose own array contains the very literals it looks
        // for — so it matched itself and passed with every `println!` deleted. A
        // test that cannot fail is worse than no test: it is a standing claim that
        // the cross-crate contract is checked when nothing checks it.
        let src = include_str!("cli.rs");
        let production = src
            .split_once("#[cfg(test)]")
            .map_or(src, |(before, _)| before);
        assert!(
            production.len() < src.len(),
            "the test module must be excluded, or this test can match itself"
        );
        for marker in [
            "atpkg: seed-installed: ",
            "atpkg: seed-pending: ",
            "atpkg: seed-unusable: ",
            "atpkg: seed-failed: ",
            "atpkg: seed-partial: ",
        ] {
            assert!(
                production.contains(marker),
                "the {marker:?} marker must be EMITTED by production code — \
                 crates/aterm-gui's parse_seed_line strips exactly this prefix, and a \
                 marker nothing prints leaves its announcement unanswered on screen"
            );
        }
    }

    /// THE CONSENT POLICY, as a table. Every consent regression this branch shipped
    /// lived in this one boolean, and none of them had a test — reverting either
    /// round-3 fix left the whole suite green, which is precisely why they shipped.
    #[test]
    fn set_completion_honours_decline_over_adoption_and_auto_install() {
        // Nothing asked for: the default posture installs nothing over the network.
        assert!(!should_complete_set(false, false, false));
        // Adopted (installing aterm is wanting the toolset) ⇒ keep the set complete.
        assert!(should_complete_set(false, true, false));
        // The explicit network-consent switch also enables it.
        assert!(should_complete_set(true, false, false));
        // DECLINED outranks BOTH. Honouring a removal on the local lane while the
        // 6-hour network pass quietly reinstalled the set made the decline work for
        // exactly one launch.
        assert!(
            !should_complete_set(false, true, true),
            "a declined machine must not be re-completed just because it once adopted"
        );
        assert!(
            !should_complete_set(true, true, true),
            "declining outranks the auto_install switch too — it is the later, more \
             specific act"
        );
        assert!(!should_complete_set(true, false, true));
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
        write_pkg_for_triple(dir, prog, build, current_triple());
    }

    /// [`write_pkg`] with an explicit artifact `target` — a manifest whose one
    /// artifact serves a FOREIGN triple is how the registry looks from every
    /// machine the publisher does not build for.
    fn write_pkg_for_triple(dir: &Path, prog: &str, build: u64, triple: &str) {
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
             size = 100\n[artifact.cost]\ndisk_installed = 1048576\n"
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
        assert_eq!(failures, 0);
        assert_eq!(crate::active_builds(&layout).get("ay").copied(), Some(18));
        // The durable floor advanced to the trusted index (§8 gate 3).
        assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 41);
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&co);
    }

    /// AUDIT-2 ITEM 2: a program the registry publishes NO artifact for on
    /// this triple neither announces, nor counts as a failure, nor leaves a
    /// pending stub on PATH — and the `*toolset*` status row records why, so
    /// a pre-existing stub's `__pending` tells the truth instead of promising
    /// a launch will provision it. Served members in the same pass still
    /// install normally.
    #[test]
    fn unserved_triple_members_skip_quietly_with_a_recorded_reason() {
        let dir = scratch("unserved");
        write_registry(&dir, "stable");
        // ay's one artifact serves a machine this test is not running on.
        write_pkg_for_triple(&dir, "ay", 18, "riscv64gc-unknown-linux-gnu");
        let layout = temp_layout("unserved");
        let cfg = crate::config::PackagesConfig::default();
        let fetcher = crate::DirFetcher::new(dir.clone());
        let out = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
        assert_eq!(out.failures, 0, "unserved is a correct state, not a failure");
        let active = crate::active_builds(&layout);
        assert!(!active.contains_key("ay"), "no artifact, no install");
        assert_eq!(
            active.get("ny").copied(),
            Some(7),
            "served members still install in the same pass"
        );
        assert!(
            !crate::stub::pending_stub_exists(&layout, "ay"),
            "no stub for a program whose bytes can never move here"
        );
        let status = crate::status::read(&layout).expect("status recorded");
        let row = status.programs.get("*toolset*").expect("toolset row");
        assert_eq!(row.state, "blocked: no build for this architecture");
        let lines = pending_state_lines(&layout, "ay");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("no build for this architecture")),
            "__pending surfaces the recorded reason, not the launch promise: {lines:?}"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&dir);
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
        assert!(failures > 0, "a missing channel is loud, not silent");
        assert!(crate::active_builds(&layout).is_empty());
        let _ = std::fs::remove_dir_all(&layout.prefix);
        // channel = "nightly" threads through and installs.
        let layout = temp_layout("channel-nightly");
        let cfg = crate::config::parse_packages("[packages]\nchannel = \"nightly\"\n");
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
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
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
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

    // -----------------------------------------------------------------------
    // `__pending` — the four honest states, the honesty edge, escape stripping
    // (R6; design §5).
    // -----------------------------------------------------------------------

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// A live-looking progress snapshot: this test process's pid, a fresh heartbeat.
    fn live_snapshot(programs: &str, queue: &str) -> String {
        format!(
            "{{\"v\":1,\"pid\":{},\"pass\":\"net\",\"heartbeat_unix\":{},\
             \"overall\":{{\"programs_done\":2,\"programs_total\":9}},\
             \"queue\":{queue},\"programs\":{programs}}}",
            std::process::id(),
            now_secs()
        )
    }

    fn bump_contents(l: &crate::store::Layout) -> String {
        std::fs::read_to_string(l.bump_file()).unwrap_or_default()
    }

    /// State: NOT STARTED — no progress file, no status row. The honest line names
    /// both next acts, and the bump is still written so the next pass front-loads
    /// the program the user actually wanted.
    #[test]
    fn pending_not_started_names_the_next_act() {
        let l = temp_layout("pend-notstarted");
        let lines = pending_state_lines(&l, "trust");
        assert!(lines[0].contains("trust:"), "leads with what the tool IS: {lines:?}");
        assert!(
            lines.iter().any(|x| x.contains("nothing is installing right now")
                && x.contains("open aterm")
                && x.contains("aterm pkg update")),
            "the not-started state names its fixes: {lines:?}"
        );
        assert_eq!(bump_contents(&l), "trust\n", "the wish is recorded for the next pass");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// State: INSTALLING NOW — a live snapshot's download row renders live MB and
    /// the overall count.
    #[test]
    fn pending_installing_now_shows_live_bytes() {
        let l = temp_layout("pend-installing");
        std::fs::write(
            l.progress_file(),
            live_snapshot(
                "{\"trust\":{\"phase\":\"download\",\"bytes_done\":4100000,\"bytes_total\":9800000}}",
                "[\"trust\"]",
            ),
        )
        .unwrap();
        let lines = pending_state_lines(&l, "trust");
        assert!(
            lines.iter().any(|x| x.contains("2 of 9 done")),
            "overall progress renders: {lines:?}"
        );
        assert!(
            lines.iter().any(|x| x.contains("downloading NOW") && x.contains("4.1 of 9.8 MB")),
            "live MB from the snapshot: {lines:?}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// State: QUEUED — the row is queued behind a current program: position + the
    /// BUMP claim, and the bump file really carries the name (a claimed bump must
    /// have been written).
    #[test]
    fn pending_queued_bumps_and_says_so() {
        let l = temp_layout("pend-queued");
        std::fs::write(
            l.progress_file(),
            live_snapshot(
                "{\"robi\":{\"phase\":\"download\"},\"trust\":{\"phase\":\"queued\"}}",
                "[\"robi\",\"ty\",\"trust\"]",
            ),
        )
        .unwrap();
        let lines = pending_state_lines(&l, "trust");
        assert!(
            lines.iter().any(|x| x.contains("queued 3 of 3")
                && x.contains("BUMPED")
                && x.contains("after robi finishes")),
            "position + bump + current program: {lines:?}"
        );
        assert_eq!(bump_contents(&l), "trust\n");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// State: FAILED — the recorded error surfaces WITH its next act, and control
    /// characters in it are stripped before the TTY (escape-sequence injection).
    #[test]
    fn pending_failed_names_the_error_and_strips_escapes() {
        let l = temp_layout("pend-failed");
        record_status(
            &l,
            "trust",
            crate::ProgramStatus {
                installed_build: None,
                state: "error: mirror said \u{1b}[2Jno".into(),
                tree_root: String::new(),
            },
            "bootstrap install trust: failed".into(),
        );
        let lines = pending_state_lines(&l, "trust");
        let failed = lines
            .iter()
            .find(|x| x.contains("FAILED"))
            .expect("the failure renders");
        assert!(failed.contains("mirror said [2Jno"), "escapes stripped: {failed}");
        assert!(!failed.contains('\u{1b}'), "no ESC byte reaches the TTY");
        assert!(failed.contains("fix: aterm pkg update"), "every failure names its next act");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A STALE snapshot renders only not-running states: yesterday's "downloading"
    /// must never be today's claim, however live the file looks otherwise.
    #[test]
    fn pending_stale_heartbeat_renders_not_running() {
        let l = temp_layout("pend-stale");
        let stale = format!(
            "{{\"v\":1,\"pid\":{},\"heartbeat_unix\":{},\
             \"programs\":{{\"trust\":{{\"phase\":\"download\",\"bytes_done\":1}}}}}}",
            std::process::id(),
            now_secs() - crate::progress::HEARTBEAT_STALE_SECS - 5
        );
        std::fs::write(l.progress_file(), stale).unwrap();
        let lines = pending_state_lines(&l, "trust");
        assert!(
            lines.iter().any(|x| x.contains("nothing is installing right now")),
            "stale = not running: {lines:?}"
        );
        assert!(
            !lines.iter().any(|x| x.contains("downloading")),
            "a dead installer's file never claims live progress: {lines:?}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE HONESTY EDGE: a live resolved-pass snapshot that does NOT plan the
    /// program says "no longer part of the default set" — and claims NO bump,
    /// because the reorder-only intersection would silently drop it.
    #[test]
    fn pending_delisted_name_is_told_the_truth_and_not_bumped() {
        let l = temp_layout("pend-delisted");
        std::fs::write(
            l.progress_file(),
            live_snapshot("{\"ty\":{\"phase\":\"download\"}}", "[\"ty\"]"),
        )
        .unwrap();
        let lines = pending_state_lines(&l, "trust");
        assert!(
            lines.iter().any(|x| x.contains("no longer part of the default set")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|x| x.contains("BUMPED")),
            "no bump claim the installer would drop: {lines:?}"
        );
        assert_eq!(bump_contents(&l), "", "and no bump was written");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A name the shim gate refuses is never echoed back — the one universally safe
    /// answer for it.
    #[test]
    fn pending_refuses_inadmissible_names() {
        let l = temp_layout("pend-refuse");
        let lines = pending_state_lines(&l, "../sudo");
        assert_eq!(lines, vec!["atpkg: that is not an installable program name".to_string()]);
        assert_eq!(bump_contents(&l), "");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    // -----------------------------------------------------------------------
    // The priority queue — reorder-only by construction (design §4).
    // -----------------------------------------------------------------------

    fn plan_of(groups: &[&[&str]]) -> Vec<(usize, Vec<String>)> {
        groups
            .iter()
            .enumerate()
            .map(|(i, members)| (i, members.iter().map(|m| (*m).to_string()).collect()))
            .collect()
    }

    /// The permutation property: whatever the bump says, the output is a
    /// rearrangement of the input — never new work, never lost work.
    #[test]
    fn resort_bumped_is_a_permutation() {
        let original = plan_of(&[&["ay"], &["trust", "trust-cg"], &["ty"]]);
        for bump in [
            vec![],
            vec!["ty".to_string()],
            vec!["nonsense".to_string(), "trust-cg".to_string(), "ty".to_string()],
            vec!["ay".to_string(), "ay".to_string()],
        ] {
            let mut sorted = original.clone();
            resort_bumped(&mut sorted, &bump);
            let mut a = original.clone();
            let mut b = sorted.clone();
            a.sort();
            b.sort();
            assert_eq!(a, b, "bump {bump:?} must only permute the plan");
        }
    }

    /// Bumped-first in bump order, plan order as the stable tiebreak; names outside
    /// the plan have no effect at all.
    #[test]
    fn resort_bumped_orders_bumped_first_then_plan_order() {
        let mut plan = plan_of(&[&["ay"], &["clean"], &["ny"], &["ty"]]);
        resort_bumped(&mut plan, &["ty".to_string(), "clean".to_string()]);
        let order: Vec<usize> = plan.iter().map(|(i, _)| *i).collect();
        assert_eq!(order, vec![3, 1, 0, 2], "bump order first, then plan order");
        // Unknown names are inert — the reorder-only intersection already dropped
        // anything unplanned, and even raw they cannot move a thing.
        let mut plan = plan_of(&[&["ay"], &["clean"]]);
        resort_bumped(&mut plan, &["sudo".to_string(), "zzz".to_string()]);
        let order: Vec<usize> = plan.iter().map(|(i, _)| *i).collect();
        assert_eq!(order, vec![0, 1]);
    }

    /// Bumping ANY member bumps its WHOLE group — the tuple stays transactional,
    /// the queue jump is group-granular.
    #[test]
    fn bumping_a_member_bumps_the_whole_group() {
        let mut plan = plan_of(&[&["ay"], &["trust", "trust-cg", "trust-ir"], &["ty"]]);
        resort_bumped(&mut plan, &["trust-ir".to_string()]);
        let order: Vec<usize> = plan.iter().map(|(i, _)| *i).collect();
        assert_eq!(
            order,
            vec![1, 0, 2],
            "the group containing the bumped member moves whole"
        );
        assert_eq!(
            plan[0].1,
            vec!["trust".to_string(), "trust-cg".to_string(), "trust-ir".to_string()],
            "membership untouched — activation stays all-or-nothing"
        );
    }
}
