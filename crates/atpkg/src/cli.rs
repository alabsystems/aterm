// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg` — the aterm toolchain package manager CLI.
//!
//! The verbs compose the tested library (`crate::ops`, `crate::store`, the fetch).
//! Every network verb reads the `[packages]` table of the same `aterm.toml` the GUI
//! owns ([`crate::config`]); there is no environment alternative to any of it. The
//! verification anchor is the committed paper-master keyset
//! [`crate::PKG_TRUST_ANCHORS`] (`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`), the
//! root the app updater uses, and nothing else can supply or swap it
//! ([`effective_anchor`]); an empty keyset builds an inert manager that says so.

use std::process::ExitCode;

use crate::flow::now_unix;

/// The dispatch roster, public so the manual (a sibling crate) can be pinned
/// against the verbs that actually run.
#[must_use]
pub fn dispatch_roster() -> &'static [&'static str] {
    &VERBS
}

/// Which help tier a verb is listed under: what a reader meets first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    Daily,
    Occasional,
    Plumbing,
}

/// How a verb takes its operands — checked at the dispatch edge, before the store lock,
/// so nothing of a command runs when its argv is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arity {
    /// No argument at all: one is a usage error, never swallowed (audit D-12).
    None,
    /// At most ONE operand; `flags` are flags it also takes in any position, which do not
    /// count (`update [program] [--retry]`).
    One { flags: &'static [&'static str] },
    /// No operand, and only these flags, each at most once (`repair [--json]`): anything
    /// else is refused at the edge, before the store lock, like [`Arity::None`].
    Flags { flags: &'static [&'static str] },
    /// Its own parser owns the whole argv.
    Own,
}

/// One verb: everything the dispatch edge and every help surface know of it. The ONE
/// table: dispatch, the unknown-verb hint, `<verb> --help`, the tiers, the name-operand
/// gate, the arity refusals, the store lock and the `[machine]` edit all read it, so no
/// second list can advertise a verb that does not run or miss one that does.
struct Verb {
    name: &'static str,
    tier: Tier,
    /// What `atpkg <verb> --help` prints (docs/AUDIT-cli-per-verb-help-2026-08-31.md);
    /// it names every flag the verb accepts.
    usage: &'static str,
    arity: Arity,
    /// `Some` when the FIRST operand is a program name: the flag-shaped literals it
    /// accepts in that slot (`install --default-set`, `uninstall --all`); anything else
    /// beginning with `-` is a mistyped flag, refused before a program could be recorded
    /// under its name. `run` is absent on purpose: `aterm <tool>` arrives as
    /// `run <tool> -- …`, and `cmd_run` owns its own `--` handling.
    program_flags: Option<&'static [&'static str]>,
    /// It stages, activates or discards builds, rewrites shims or links, or edits the pin
    /// set, so it holds the store-wide single-writer lock ([`crate::lock`]) for its whole
    /// run. Everything else reads only — `run` above all, which may exec a long-lived tool.
    mutates_store: bool,
    /// A pass that applies the `[machine]` settings when their table CHANGED
    /// ([`machine_table_changed`]): the three long passes. `update <vendor program>` never
    /// does (the edge checks its operand).
    applies_machine: bool,
    /// The verb's body, handed the argv after the verb.
    run: fn(&[String]) -> ExitCode,
}

/// The verbs, in the order the help tiers list them.
const TABLE: &[Verb] = &[
    Verb {
        name: "install",
        tier: Tier::Daily,
        usage: "aterm pkg install <program>\n\
                aterm pkg install --default-set    — the whole ALab toolset",
        arity: Arity::Own,
        program_flags: Some(&["--default-set"]),
        mutates_store: true,
        applies_machine: true,
        run: cmd_install_argv,
    },
    Verb {
        name: "list",
        tier: Tier::Daily,
        usage: "aterm pkg list [--porcelain]",
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        run: cmd_list,
    },
    Verb {
        name: "update",
        tier: Tier::Daily,
        usage: "aterm pkg update [program] [--retry]  (--retry: try a held-back download again \
                now)\n\
                aterm pkg update codex --vendor | --alab  — OpenAI's codex here, or back to \
                ALab's",
        arity: Arity::One {
            flags: &["--retry", SOURCE_VENDOR_FLAG, SOURCE_ALAB_FLAG],
        },
        program_flags: Some(&["--retry", SOURCE_VENDOR_FLAG, SOURCE_ALAB_FLAG]),
        mutates_store: true,
        applies_machine: true,
        run: cmd_update_argv,
    },
    Verb {
        name: "doctor",
        tier: Tier::Daily,
        usage: "aterm pkg doctor [--verbose] [--json]  (alias: status) — what needs \
                attention; --verbose: every check; --json: every row, in the one report schema",
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        // `status` is an alias, the name other package managers use. `--verbose` (or `-v`)
        // and `--json` are what either takes ([`crate::doctor::render`],
        // [`aterm_converge::Report`]).
        run: |rest| {
            doctor_args(rest, "doctor").map_or_else(|code| code, |(d, j)| doctor("doctor", d, j))
        },
    },
    Verb {
        name: "status",
        tier: Tier::Daily,
        usage: "aterm pkg status [--verbose] [--json]  (alias of doctor) — what needs \
                attention; --verbose: every check; --json: every row, in the one report schema",
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        // The same report, prefixed with the verb the user typed.
        run: |rest| {
            doctor_args(rest, "status").map_or_else(|code| code, |(d, j)| doctor("status", d, j))
        },
    },
    Verb {
        name: "which",
        tier: Tier::Daily,
        usage: "aterm pkg which <tool>",
        arity: Arity::One { flags: &[] },
        program_flags: Some(&[]),
        mutates_store: false,
        applies_machine: false,
        run: |rest| cmd_which(rest.first()),
    },
    Verb {
        name: "run",
        tier: Tier::Daily,
        usage: "aterm pkg run <tool> [args…]",
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        run: cmd_run,
    },
    Verb {
        name: "uninstall",
        tier: Tier::Occasional,
        usage: "aterm pkg uninstall <program> | aterm pkg uninstall --all",
        arity: Arity::One { flags: &[] },
        program_flags: Some(&["--all"]),
        mutates_store: true,
        applies_machine: false,
        run: |rest| cmd_uninstall(rest.first()),
    },
    Verb {
        name: "rollback",
        tier: Tier::Occasional,
        usage: "aterm pkg rollback <program>",
        arity: Arity::One { flags: &[] },
        program_flags: Some(&[]),
        mutates_store: true,
        applies_machine: false,
        run: |rest| cmd_rollback(rest.first()),
    },
    Verb {
        name: "pin",
        tier: Tier::Occasional,
        usage: "aterm pkg pin <program>",
        arity: Arity::One { flags: &[] },
        program_flags: Some(&[]),
        mutates_store: true,
        applies_machine: false,
        run: |rest| cmd_pin(rest.first(), true),
    },
    Verb {
        name: "unpin",
        tier: Tier::Occasional,
        usage: "aterm pkg unpin <program>",
        arity: Arity::One { flags: &[] },
        program_flags: Some(&[]),
        mutates_store: true,
        applies_machine: false,
        run: |rest| cmd_pin(rest.first(), false),
    },
    Verb {
        name: "verify",
        tier: Tier::Occasional,
        usage: "aterm pkg verify [program]",
        arity: Arity::One { flags: &[] },
        program_flags: Some(&[]),
        mutates_store: false,
        applies_machine: false,
        run: |rest| cmd_verify(rest.first()),
    },
    Verb {
        name: "gc",
        tier: Tier::Occasional,
        usage: "aterm pkg gc                — reclaim superseded builds",
        arity: Arity::None,
        program_flags: None,
        mutates_store: true,
        applies_machine: false,
        run: |_| cmd_gc(),
    },
    Verb {
        name: "repair",
        tier: Tier::Occasional,
        usage: "aterm pkg repair [--json]   — restore installed commands, shell integration, \
                the reroute stubs (abrew's `brew` too), the rustup link, exec roots, an outlived \
                held update and (macOS) the ~/.local/bin links; --json: every row, in the one \
                report schema",
        // `repair` writes, so a swallowed `--dry-run` would rewrite everything at exit 0
        // (audit D-12): it takes `--json` and refuses anything else at the edge.
        arity: Arity::Flags { flags: &["--json"] },
        program_flags: None,
        mutates_store: true,
        applies_machine: false,
        run: |rest| run_repair(layout(), rest.iter().any(|a| a == "--json")),
    },
    Verb {
        name: "machine",
        tier: Tier::Occasional,
        usage: "aterm pkg machine                 — the [machine] settings, as the doctor reads \
                them\n\
                aterm pkg machine apply           — apply them now: Universal Control off for \
                this host\n\
                aterm pkg machine cache           — what the apply would take from the Rust \
                build cache, and why; removes nothing\n\
                aterm pkg machine cache clean     — reclaim unused Rust build cache entries \
                without applying host settings",
        // The [machine] settings' own door, run by the window and the session on their own
        // schedule: a per-host preference and `<prefix>/machine.applied`, no store lock.
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        run: cmd_machine,
    },
    Verb {
        name: "self-uninstall",
        tier: Tier::Occasional,
        usage: "aterm pkg self-uninstall [--dry-run] [--install-dir <dir>] [--bin-dir <dir>] \
                [--store-dir <dir>] [--man-root <dir>] [--man <dir>]  — remove what install.sh \
                placed (the toolchain store, the app, the command links, man pages, \
                completions, the PATH block), each only when it is aterm's",
        // Not a store mutator at the edge: its sweep runs the bundle's own `atpkg
        // uninstall --all`, which takes the store lock itself.
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        run: cmd_self_uninstall,
    },
    Verb {
        name: "seed",
        tier: Tier::Plumbing,
        usage: "aterm pkg seed",
        arity: Arity::None,
        program_flags: None,
        mutates_store: true,
        applies_machine: true,
        run: cmd_seed,
    },
    Verb {
        name: "link",
        tier: Tier::Plumbing,
        usage: "aterm pkg link <program> <checkout> [rel-bin…] [--if-checkout <current> --if-prefix <prefix>] [--wait-lock <secs>]",
        arity: Arity::Own,
        program_flags: Some(&[]),
        mutates_store: true,
        applies_machine: false,
        run: cmd_link,
    },
    Verb {
        name: "unlink",
        tier: Tier::Plumbing,
        usage: "aterm pkg unlink <program>",
        arity: Arity::One { flags: &[] },
        program_flags: Some(&[]),
        mutates_store: true,
        applies_machine: false,
        run: |rest| cmd_unlink(rest.first()),
    },
    Verb {
        name: "refresh",
        tier: Tier::Plumbing,
        usage: "aterm pkg refresh [program…]  — bare: every dev-linked program",
        arity: Arity::Own,
        program_flags: Some(&[]),
        mutates_store: true,
        applies_machine: false,
        run: cmd_refresh,
    },
    Verb {
        name: "shell-setup",
        tier: Tier::Plumbing,
        usage: "aterm pkg shell-setup [--path] [--completions] [--man <dir> | --own-man] \
                [--desktop [--icons <dir>]] [--toolset-coming] [--dry-run] [--bin-dir <dir>] \
                [--man-root <dir>]  — the managed PATH block, this binary's shell completions, \
                the man pages (a checkout's, or this binary's own) and the Linux desktop entry \
                (bare: --path --completions)",
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        run: cmd_shell_setup,
    },
    Verb {
        name: "tree-root",
        tier: Tier::Plumbing,
        usage: "aterm pkg tree-root <dir>",
        arity: Arity::One { flags: &[] },
        program_flags: Some(&[]),
        mutates_store: false,
        applies_machine: false,
        run: |rest| cmd_tree_root(rest.first()),
    },
    Verb {
        name: "verify-index",
        tier: Tier::Plumbing,
        usage: "aterm pkg verify-index <master-pubkey-b64> <index.toml> <index.toml.sig> \
                <aterm-machines.toml> <aterm-machines.toml.sig>",
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        run: cmd_verify_index,
    },
    Verb {
        name: "verify-pkg",
        tier: Tier::Plumbing,
        usage: "aterm pkg verify-pkg <master-pubkey-b64> <pkg-*.toml> <pkg.sig> \
                <aterm-machines.toml> <aterm-machines.toml.sig>",
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        run: cmd_verify_pkg,
    },
    Verb {
        name: "lease",
        tier: Tier::Plumbing,
        usage: "aterm pkg lease <toolchain-dir> [--who <words>] -- <command> [args…]  — hold \
                the toolchain while <command> runs: no cleanup, no unattended swap, no rustup \
                re-lay",
        // THE HOLDER VERB ([`crate::lease::hold_for`]): no store lock — a lease is laid
        // beside the store, never in it, and must be taken while a pass holds the lock.
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        run: cmd_lease,
    },
    Verb {
        name: "abrew",
        tier: Tier::Daily,
        usage: "aterm pkg abrew <brew-command> [args…]  — aterm's Homebrew (docs/DESIGN-abrew-2026-10-19.md): \
                relocation-free bottles and simple casks natively over abrew's own prefix, the rest delegated to real brew before effects; \
                abrew bundle lays the bundled Homebrew; abrew on|off toggles the `brew` reroute; \
                brew --real … runs real Homebrew unchanged",
        // abrew owns its argv and its own prefix lock; it never mutates the store.
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        run: crate::abrew::cli_entry,
    },
    Verb {
        name: "lane",
        tier: Tier::Plumbing,
        usage: "aterm pkg lane [--cwd <dir>] (-- <command…> | - | --json <field.path>)  — \
                check one command line for stock Rust: exit 2 with the Trust spelling, 0 when \
                clean or ATERM_STOCK_REASON='<why>' leads, 1 on unreadable or over-8 MiB input",
        // Read-only and store-free ([`crate::lane`]): no lock, no layout, nothing written.
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        run: cmd_lane,
    },
];

/// Every verb's name, in [`TABLE`] order: what the unknown-verb hint and the manual read.
const VERBS: [&str; TABLE.len()] = verb_names();

const fn verb_names() -> [&'static str; TABLE.len()] {
    let mut out = [""; TABLE.len()];
    let mut i = 0;
    while i < TABLE.len() {
        out[i] = TABLE[i].name;
        i += 1;
    }
    out
}

/// `aterm pkg self-uninstall` ([`crate::selfsetup`]): install.sh's placement, swept.
fn cmd_self_uninstall(rest: &[String]) -> ExitCode {
    #[cfg(unix)]
    return crate::selfsetup::self_uninstall_verb(rest);
    #[cfg(not(unix))]
    {
        let _ = rest;
        usage_error(
            "self-uninstall",
            Some("install.sh installs on macOS and Linux only"),
        )
    }
}

/// `aterm pkg shell-setup` ([`crate::selfsetup`]): the PATH block, completions, man pages,
/// the desktop entry.
fn cmd_shell_setup(rest: &[String]) -> ExitCode {
    #[cfg(unix)]
    return crate::selfsetup::shell_setup_verb(rest);
    #[cfg(not(unix))]
    {
        let _ = rest;
        usage_error(
            "shell-setup",
            Some("install.sh installs on macOS and Linux only"),
        )
    }
}

/// The [`TABLE`] row of `name`.
fn verb_row(name: &str) -> Option<&'static Verb> {
    TABLE.iter().find(|v| v.name == name)
}

/// The flag-shaped literals `verb` accepts where it takes a program name, or `None` when
/// its first operand is no program name ([`Verb::program_flags`]).
fn program_flags(verb: &str) -> Option<&'static [&'static str]> {
    verb_row(verb).and_then(|v| v.program_flags)
}

/// The usage line for `verb`, if it has one. Public so the long-form manual
/// (`aterm-cli`'s `manual.rs`) can be held to the grammar the binary parses.
#[must_use]
pub fn verb_usage(verb: &str) -> Option<&'static str> {
    verb_row(verb).map(|v| v.usage)
}

/// Whether a `-h`/`--help` in `rest` (the argv after `verb`) is a question for atpkg rather
/// than an argument for a tool. A literal `--` ends the scan; for `run`, only the first
/// operand counts, so `atpkg run rg --help` (and `aterm rg --help`, which arrives as
/// `run rg -- --help`) reaches rg.
fn help_flag_addresses_atpkg(verb: &str, rest: &[String]) -> bool {
    // `abrew` owns its whole argv (Arity::Own): its --help is abrew's (or,
    // for delegated commands, real brew's), never the atpkg usage line.
    if verb == "abrew" {
        return false;
    }
    let scanned = if verb == "run" {
        &rest[..rest.len().min(1)]
    } else {
        rest
    };
    scanned
        .iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "-h" || a == "--help")
}

/// Answer `<verb> --help` from the verb's [`Verb::usage`]: the verb's own grammar, then the one
/// pointer to the long-form manual. Exit 0 — asking a question is not an error.
fn cmd_verb_help(verb: &str) -> ExitCode {
    match verb_usage(verb) {
        Some(usage) => {
            println!("usage: {usage}");
            println!("atpkg: full manual for this verb: aterm help pkg");
            ExitCode::SUCCESS
        }
        // Not a verb: the roster at exit 0 would read as help for `<verb>`, so name the
        // topic, suggest the near miss, list the real ones, exit 2.
        None => {
            match did_you_mean(verb) {
                Some(near) => eprintln!(
                    "atpkg: unknown help topic '{verb}' \u{2014} did you mean `aterm pkg help \
                     {near}`?"
                ),
                None => eprintln!("atpkg: unknown help topic '{verb}'"),
            }
            eprintln!();
            eprintln!("available topics (aterm pkg help <verb>):");
            print_verb_tiers(&mut std::io::stderr());
            eprintln!("full manual: aterm help pkg");
            ExitCode::from(2)
        }
    }
}

/// The tier headings, in the order they print.
const TIERS: [(&str, Tier); 3] = [
    ("daily", Tier::Daily),
    ("occasional", Tier::Occasional),
    ("plumbing", Tier::Plumbing),
];

/// The tier block, rendered once for bare `atpkg`, `--help` and the unknown-verb
/// error. `status` renders as `doctor (or: status)`: the two are one report.
fn verb_tier_lines() -> Vec<String> {
    TIERS
        .iter()
        .map(|(tier, which)| {
            let shown: Vec<&str> = TABLE
                .iter()
                .filter(|v| v.tier == *which && v.name != "status")
                .map(|v| {
                    if v.name == "doctor" {
                        "doctor (or: status)"
                    } else {
                        v.name
                    }
                })
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

/// Words other package managers spell a verb with that no edit distance reaches, and the
/// verb they mean here: `remove` was no suggestion at all, and `rm` suggested `run`.
const VERB_SYNONYMS: &[(&str, &str)] = &[("remove", "uninstall"), ("rm", "uninstall")];

/// The advertised verb closest to `input` (a [`VERB_SYNONYMS`] word's verb, else the one at
/// Levenshtein distance ≤ 2), or `None` when nothing is close enough to suggest — a
/// far-fetched guess ("frobnicate → refresh"?) erodes trust in the near ones. Ties resolve
/// to the first in [`VERBS`] order.
fn did_you_mean(input: &str) -> Option<&'static str> {
    if let Some((_, verb)) = VERB_SYNONYMS.iter().find(|(word, _)| *word == input) {
        return Some(verb);
    }
    let mut best: Option<(usize, &'static str)> = None;
    for verb in VERBS {
        let d = levenshtein(input, verb);
        if best.is_none_or(|(b, _)| d < b) {
            best = Some((d, verb));
        }
    }
    best.filter(|(d, _)| *d <= 2).map(|(_, v)| v)
}

/// Plain O(len_a * len_b) edit distance over chars; the roster is short.
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

/// The whole package-manager CLI as a callable: `argv[1..]` in, exit code out. Served by
/// the one `aterm` binary (`aterm pkg …`, the `atpkg` argv0 alias) and the standalone bin.
pub fn main_entry(mut argv: Vec<std::ffi::OsString>) -> ExitCode {
    // The private window's lead ([`PRIVATE_STATE_ROOT_FLAG`]), taken off before anything
    // reads the first word as the verb: this pass carries no `[machine]` edit, whatever this
    // build's seams say.
    if argv.first().and_then(|v| v.to_str()) == Some(PRIVATE_STATE_ROOT_FLAG) {
        argv.remove(0);
        PRIVATE_STATE_ROOT_LEAD.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // The hidden warm verb ([`crate::warm`]): a detached headless first run of an agent build
    // a pass just landed. Read off the raw argv, since its operands carry the store prefix
    // byte for byte, and before the store lock, which its pass may still hold; it never
    // mutates the store and prints nothing.
    if argv.first().and_then(|v| v.to_str()) == Some(crate::warm::HIDDEN_VERB) {
        return crate::warm::run_hidden(&argv[1..]);
    }
    // THE HIDDEN DEVELOPER TOOLS PRIME ([`crate::devtools::run_prime`]): what the window
    // and `aterm doctor` start, as aterm's own responsible process, once the owner
    // switched aterm on, so macOS asks about aterm again. It never touches the store.
    if argv.first().and_then(|v| v.to_str()) == Some(crate::devtools::PRIME_VERB) {
        return crate::devtools::run_prime();
    }
    let mut args: Vec<String> = argv
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    PUBLISHED_INDEX_HINT.with(|hint| hint.set(None));
    // `--progress-file <path>`: the GUI's machine-progress opt-in, recognized only on the
    // provisioning verbs and stripped before dispatch so no verb sees it as an operand. It
    // feeds [`progress_path`].
    if matches!(
        args.first().map(String::as_str),
        Some("seed" | "update" | "install")
    ) {
        strip_progress_file_flag(&mut args);
        // `--head-watch` — which door `update <program>` takes, stripped here likewise.
        if take_flag(&mut args, HEAD_WATCH_FLAG) {
            HEAD_WATCH_PASS.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // `--wait-lock <secs>`: the machine lanes' opt-in to queue behind a sibling's pass
        // at the store lock instead of refusing, stripped before everything that would
        // otherwise refuse it (`cmd_seed`'s operand refusal, the flag gate below); read by
        // [`mutator_store_lock`].
        if let Some(bound) = take_wait_lock_flag(&mut args) {
            configure_wait_lock(&args, bound);
        }
        if args.first().is_some_and(|verb| verb == "update") {
            PUBLISHED_INDEX_HINT.with(|hint| hint.set(take_published_index_hint_flag(&mut args)));
            // `--defer-busy-flip` — the unattended lanes' opt-in to hold the toolchain's
            // flip while it is in use ([`crate::quiet`]), stripped here likewise.
            if take_flag(&mut args, DEFER_BUSY_FLIP_FLAG) {
                DEFER_BUSY_FLIP.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    let verb = args.first().map(String::as_str);
    // The HIDDEN pending-program verb (R6): what a laid stub execs. Deliberately
    // unlisted in help and the verb table (it is machinery, not vocabulary), handled
    // before the table's dispatch — and
    // BEFORE the store lock: it only reads progress/status and appends one line to
    // the reorder-only bump file, and it must answer instantly while the installer
    // HOLDS the store lock.
    if verb == Some("__pending") {
        return cmd_pending(args.get(1), args.get(2..).unwrap_or(&[]));
    }
    // The HIDDEN self-update verb ([`crate::selfupdate`]): what an agent
    // program's `agents/` twin execs when its FIRST argument is one of the vendor's own
    // self-update verbs (`claude update|upgrade|install`, `codex update`). Same
    // discipline as `__pending` — unlisted, dispatched before the table, and BEFORE the
    // help edge and the store lock: it never
    // mutates the store itself — the CHILD it spawns does, `update <program>` through
    // this very dispatch edge, lock and all — and it must answer while a pass HOLDS the
    // lock, when its answer is "that pass is doing this work".
    if verb == Some(crate::selfupdate::HIDDEN_VERB) {
        return cmd_selfupdate(&args[1..]);
    }
    // The hidden landing verb ([`crate::landing`]): what an `agents/` twin laid by an older
    // client execs while a landing marker stands — an immediate, silent `exec` of
    // `bin/<program>`. Handled like `__pending`.
    if verb == Some(crate::landing::HIDDEN_VERB) {
        return cmd_landing(&args[1..]);
    }
    // The HIDDEN reroute verb (philosophy §4, `crate::reroute`): what a reroute stub
    // execs for `cargo`/`rustfmt`/… typed inside a session. Same discipline as
    // `__pending` — unlisted, dispatched before the table, and BEFORE the store lock: it
    // must answer while the installer
    // HOLDS the lock, and it never mutates the store.
    if verb == Some(crate::reroute::HIDDEN_VERB) {
        return cmd_reroute(&args[1..]);
    }
    // The HIDDEN abrew verb (`crate::abrew`, docs/DESIGN-abrew-2026-10-19.md §6):
    // what the `brew` reroute stub execs. Same discipline as `__reroute` —
    // unlisted, before the table and BEFORE the store lock: abrew takes its own
    // prefix lock and never mutates the atpkg store.
    if verb == Some(crate::abrew::stub::HIDDEN_VERB) {
        return crate::abrew::rerouted_entry(&args[1..]);
    }
    // THE single-writer-per-store gate ([`crate::lock`]): every store-MUTATING verb
    // TRY-acquires the store-wide `store.lock` here — at the ONE dispatch edge, so
    // internal verb re-routing (`update <p>` → install) can
    // never double-acquire — and holds it for the whole verb. Contention is a loud
    // exit-75 refusal naming the lock path for a verb no person is watching (the GUI
    // Packages page surfaces the child stderr), and a bounded wait behind a spinner for
    // one typed on a terminal; the window's launch lanes pass `--wait-lock` and
    // QUEUE behind the holder instead (see `mutator_store_lock`), and their loop
    // retries a timed-out wait on a short backoff. Read-only verbs never take the lock.
    // The conventional help spellings land on the help surface with exit 0, not the
    // unknown-verb error path — `atpkg` rides PATH as an argv0 alias, so `--help` is
    // the first thing a shell (or an AI) tries. Handled before the verb table, which
    // holds only real verbs.
    if matches!(verb, Some("help" | "-h" | "--help")) {
        // `atpkg help <verb>` answers for that verb, as `aterm help <topic>` does; an
        // unknown topic is a usage error ([`cmd_verb_help`]).
        if let Some(topic) = args.get(1) {
            return cmd_verb_help(topic);
        }
        return cmd_help();
    }
    // `<verb> --help` anywhere before a literal `--` answers for that verb and does nothing
    // else ([`help_flag_addresses_atpkg`]), before the store lock: asking a mutating verb
    // how it works must never mutate (docs/AUDIT-cli-per-verb-help-2026-08-31.md).
    if let Some(v) = verb
        && verb_usage(v).is_some()
        && help_flag_addresses_atpkg(v, &args[1..])
    {
        return cmd_verb_help(v);
    }
    // A flag is never a program name. The verbs below hand `args[1]` to the signed index,
    // which would record `--x` as a failed program in `status.toml`, or uninstall nothing
    // and exit 0. Refused here, before the store lock, so a typo never contends for it.
    if let Some(v) = verb
        && let Some(operand) = args.get(1)
        && operand.starts_with('-')
        && let Some(allowed) = program_flags(v)
        && !allowed.contains(&operand.as_str())
    {
        // (`-h`/`--help` never reaches here.) The refusal names the verb's grammar.
        let problem = format!("unknown option {operand:?} for `aterm pkg {v}`");
        let refusal = usage_error(v, Some(&problem));
        if !allowed.is_empty() {
            eprintln!("atpkg:   {v} accepts: {}", allowed.join(", "));
        }
        return refusal;
    }
    // THE VERB'S ARITY ([`Verb::arity`]): a verb that takes no argument refuses one, and a
    // one-name verb refuses a second name rather than acting on the first and dropping the
    // rest — the same edge, the same reason (before the store lock, before anything is
    // done), and the answer `install` already gave in its own parser.
    if let Some(v) = verb
        && let Some(refusal) = arity_refusal(&args[1..], v)
    {
        return refusal;
    }
    // A PROGRAM BUILT INTO aterm ([`crate::builtin`]) is no store program: the store verbs
    // that name one (`install`, `update`, `uninstall`, `pin`, …) are answered here, before
    // the store lock, the `[machine]` edit and the network, none of which it needs.
    if let Some(v) = verb
        && let Some(answer) = built_in_answer(v, &args[1..])
    {
        return answer;
    }
    // THE `[machine]` SETTINGS DO NOT RIDE THE PASS (Phase 3): their `defaults
    // -currentHost` writes (the two Universal Control keys; nothing is walked or renamed)
    // run from their own verb, `aterm pkg machine apply`, which the window and the
    // session call on their own schedule — and here, at
    // a pass's edge, only when the `[machine]` table CHANGED since it was last applied, so
    // an edit takes effect at the next pass on every lane. BESIDE the pass, not in front of
    // it ([`MachineEdit`]): its thread starts before the lock is asked for — this work
    // takes no store lock, and a contended or refused pass must not hold it hostage — and
    // is joined before this function returns, so the pass never waits on `defaults` (or
    // on another apply's queue) to take the lock or do its work.
    // NOT UNDER A PRIVATE STATE ROOT (ruling 409): a pass from a development build given
    // its own `ATERM_STATE_HOME` — a window's launch seed, its loop, a session's detached
    // pass — shares the owner's prefix, and `machine.applied` there records the OWNER's
    // table, so its own `[machine]` table would read as an edit and be applied to the
    // owner's Mac. It carries none; `aterm pkg machine apply`, typed, still applies. Nor a
    // pass the private window LED with its flag (ruling 410): this build may compile no
    // seam, and the window that spawned it knows ([`under_private_state_root`]).
    let link_request = if verb == Some("link") {
        match link_args(&args[1..]) {
            Ok(request) => Some(request),
            Err(why) => return usage_error("link", Some(&why)),
        }
    } else {
        None
    };
    if let Some(bound) = link_request.as_ref().and_then(|request| request.wait_lock) {
        configure_wait_lock(&args, bound);
    }
    let machine_settings = verb.is_some_and(|v| {
        verb_applies_machine_settings(v)
            && !(v == "update"
                && args
                    .get(1)
                    .is_some_and(|p| crate::vendor_direct::is_vendor(p)))
    }) && !under_private_state_root()
        && machine_table_changed();
    // Declared ahead of the store lock, so the lock is let go before the join.
    let mut machine_edit = machine_settings.then(MachineEdit::start);
    let mut store_lock = if verb.is_some_and(verb_mutates_store) {
        match mutator_store_lock(link_request.as_ref().and_then(|r| r.if_prefix)) {
            Ok(guard) => guard,
            Err(code) => {
                // The package log hears a pass the store lock refused, too: a
                // `claude update` behind a running pass, a window lane whose wait ran out.
                if !link_request
                    .as_ref()
                    .is_some_and(|r| r.if_checkout.is_some())
                    && let Some(layout) = crate::store::resolve_configured()
                {
                    PassRecord::refused(&layout, &args, exit_code_u8(code));
                }
                // A `[machine]` edit applies beside this refusal: say so on stderr (stdout
                // stays the marker channel) once the apply has ended, only when the record
                // shows it applied and someone is there to read it.
                if let Some(edit) = machine_edit.as_mut() {
                    edit.join();
                }
                if machine_settings && !machine_table_changed() && !parent_is_gone() {
                    use std::io::Write as _;
                    let _ = writeln!(
                        std::io::stderr(),
                        "atpkg: your [machine] settings change was applied; the rest of this \
                         command did not run"
                    );
                }
                return code;
            }
        }
    } else {
        None
    };
    // A conditional refusal must precede all pass writers, including the landing sweep,
    // holder announcement and activation-notice directory. Lock-file coordination itself
    // is the same vetted kernel namespace ordinary mutations use.
    if let Some(request) = link_request.as_ref().filter(|r| r.if_checkout.is_some()) {
        let Some(guard) = store_lock.as_mut() else {
            eprintln!("atpkg: conditional link requires a configured package store");
            return ExitCode::from(1);
        };
        return cmd_conditional_link(request, guard, &args);
    }
    if let Some(guard) = store_lock.as_mut() {
        begin_store_mutation(guard);
    }
    // The pass's store, resolved once under the lock: nothing from here to the dispatch
    // writes `aterm.toml`, HOME or the prefix, so each reader below gets the same answer.
    let pass_layout = store_lock.as_ref().map(crate::lock::StoreLock::layout);
    // Snapshot the LIVE agent twins under the store lock, before interrupted
    // activations are rolled forward. The final comparison below publishes a
    // wake only if this pass really moved a live-upgrade twin's executable
    // (Claude Code's or Codex's the first marker, Muse Code's its own), even if
    // another member of a multi-program pass later failed.
    #[cfg(target_os = "macos")]
    let agents_before = pass_layout.clone().map(|layout| {
        let targets = crate::activation_notice::agent_targets(&layout);
        crate::activation_notice::prepare(&layout);
        (layout, targets)
    });
    // One `status.toml` write per verb: a lock-holding verb records into memory and writes
    // once when it returns — declared after the lock, so it drops first and the record is
    // on disk before a waiter can take the lock.
    let _status_record = pass_layout
        .clone()
        .map(|layout| crate::status::hold(&layout));
    // The package log's pass record: every verb that holds the store lock is a pass, logged
    // at start and at return while the lock is still held (so the log may rotate) and the
    // record still in memory (so the end line can quote the outcome the pass recorded).
    let pass_record = pass_layout
        .clone()
        .map(|layout| PassRecord::begin(layout, &args));
    // Finish an interrupted activation before anything decides on it: a pass killed between
    // a `current` flip and its shims left a complete build no shim runs; it is re-laid from
    // its sidecars, never re-staged or re-fetched.
    if matches!(verb, Some("seed" | "update" | "install"))
        && let Some(layout) = pass_layout.clone()
    {
        roll_forward_interrupted_activations(&layout, || cached_index(&layout));
    }
    // A typed pass's meter, started under the lock so it never draws over the wait for it.
    let _meter = pass_layout.and_then(|layout| typed_pass_meter(verb.unwrap_or_default(), &layout));
    let code = run_verb(verb, &args);
    #[cfg(target_os = "macos")]
    if let Some((layout, before)) = agents_before {
        crate::activation_notice::note_if_changed(&layout, &before);
    }
    if let Some(record) = pass_record {
        record.end(exit_code_u8(code));
    }
    code
}

/// The verb's own dispatch, below the edge ([`main_entry`]): everything the edge decided
/// (the help spellings, the refusals, the lock, the held record, the roll-forward) is done.
fn run_verb(verb: Option<&str>, args: &[String]) -> ExitCode {
    let Some(name) = verb else {
        cmd_bare();
        return ExitCode::SUCCESS;
    };
    if let Some(row) = verb_row(name) {
        return (row.run)(&args[1..]);
    }
    // The head `atpkg: unknown verb '<x>'` is byte-stable for scripts; the suggestion
    // follows it, the full roster below.
    match did_you_mean(name) {
        Some(near) => eprintln!("atpkg: unknown verb '{name}' — did you mean `{near}`?"),
        None => eprintln!("atpkg: unknown verb '{name}'"),
    }
    print_verb_tiers(&mut std::io::stderr());
    eprintln!("atpkg: full manual: aterm help pkg");
    ExitCode::from(2)
}

/// A pass, as the package log records it ([`crate::packages_log`]): begun once the verb
/// holds the store lock, ended when it returns.
struct PassRecord {
    layout: crate::store::Layout,
    lane: &'static str,
    verb: String,
    started: std::time::Instant,
    /// The record's `(updated_at, outcome)` when the pass began, and this thread's outcome
    /// count then ([`outcomes_written`]), for [`Self::outcome`].
    outcome_before: Option<(String, String)>,
    outcomes_before: u64,
}

impl PassRecord {
    fn begin(layout: crate::store::Layout, args: &[String]) -> Self {
        let lane = this_pass_lane(args);
        let verb = pass_verb_words(args);
        crate::packages_log::start_pass_clock(&layout);
        crate::packages_log::append(
            &layout,
            &crate::packages_log::Event::PassStart { lane, verb: &verb },
        );
        let outcome_before =
            crate::status::read(&layout).map(|s| (s.updated_at.clone(), s.outcome.clone()));
        Self {
            layout,
            lane,
            verb,
            started: std::time::Instant::now(),
            outcome_before,
            outcomes_before: outcomes_written(),
        }
    }

    /// The outcome sentence this pass recorded, or none: this thread wrote an outcome since
    /// the pass began ([`note_outcome_written`]) and the record moved. The record alone
    /// cannot tell — a row written keeping the outcome ([`record_row_keeping_outcome`])
    /// moves `updated_at` but carries an earlier pass's sentence.
    fn outcome(&self) -> String {
        if outcomes_written() == self.outcomes_before {
            return String::new();
        }
        crate::status::read(&self.layout)
            .filter(|s| {
                self.outcome_before.as_ref() != Some(&(s.updated_at.clone(), s.outcome.clone()))
            })
            .map(|s| s.outcome)
            .unwrap_or_default()
    }

    fn end(self, exit: u8) {
        let outcome = self.outcome();
        crate::packages_log::append(
            &self.layout,
            &crate::packages_log::Event::PassEnd {
                lane: self.lane,
                verb: &self.verb,
                exit,
                secs: self.started.elapsed().as_secs(),
                outcome: &outcome,
            },
        );
        crate::packages_log::stop_pass_clock();
    }

    /// A pass the store lock refused: one `pass-end`, no start (it never held the store).
    fn refused(layout: &crate::store::Layout, args: &[String], exit: u8) {
        let outcome = if exit == crate::lock::CONTENDED_EXIT {
            "did not run: another pass holds the store"
        } else {
            "did not run: the store lock could not be taken"
        };
        crate::packages_log::append(
            layout,
            &crate::packages_log::Event::PassEnd {
                lane: this_pass_lane(args),
                verb: &pass_verb_words(args),
                exit,
                secs: 0,
                outcome,
            },
        );
    }
}

/// The verb as the log names it: the verb and its first operand (`update`, `update claude`,
/// `install --default-set`), the edge's own flags already stripped.
fn pass_verb_words(args: &[String]) -> String {
    args.iter().take(2).cloned().collect::<Vec<_>>().join(" ")
}

/// [`pass_lane`] for this process, from what the dispatch edge recorded.
fn this_pass_lane(args: &[String]) -> &'static str {
    pass_lane(
        head_watch_pass(),
        WAIT_LOCK.get().is_some(),
        progress_path().is_some(),
        std::env::var(SPAWNER_PID_ENV).ok().as_deref() == Some(SPAWNER_DETACHED),
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    )
}

/// Which lane ran a pass, read off how it was spawned, since every spawner already says
/// enough: the head watch's door carries [`HEAD_WATCH_FLAG`]; the terminal session's
/// detached pass names no spawner ([`SPAWNER_DETACHED`]); the window's passes queue with
/// `--wait-lock` and report through `--progress-file`; the `claude update`/`codex update`
/// intercept's child queues with `--wait-lock` and no progress file
/// ([`crate::selfupdate::child_args`]); anything else was typed or scripted.
fn pass_lane(
    head_watch: bool,
    wait_lock: bool,
    progress_file: bool,
    detached: bool,
    verb: Option<&str>,
    operand: Option<&str>,
) -> &'static str {
    if head_watch {
        "head watch"
    } else if detached {
        "session"
    } else if wait_lock
        && !progress_file
        && verb == Some("update")
        && operand.is_some_and(crate::vendor_direct::is_vendor)
    {
        "agent update"
    } else if wait_lock {
        "window"
    } else {
        "typed"
    }
}

/// The exit status an [`ExitCode`] carries. `ExitCode` exposes no accessor, but it is
/// `Eq`: the one `u8` that converts to an equal code is its status. A comparison, never an
/// exit site.
fn exit_code_u8(code: ExitCode) -> u8 {
    (0..=u8::MAX)
        .find(|n| {
            let probe: ExitCode = (*n).into();
            probe == code
        })
        .unwrap_or(1)
}

/// `atpkg help` / `-h` / `--help`: usage plus the tiered roster, exit 0. Static — no
/// store reads — so it answers even with HOME unset. The full manual is `aterm help pkg`.
fn cmd_help() -> ExitCode {
    println!("aterm pkg \u{2014} installs and updates the ALab toolchain");
    println!("{}", aterm_types::identity::ORIGIN_LINE);
    println!("usage: aterm pkg <command> [args\u{2026}]");
    print_verb_tiers(&mut std::io::stdout());
    println!("new machine: aterm pkg install --default-set");
    println!("full manual: aterm help pkg");
    ExitCode::SUCCESS
}

/// The one-line answer for a program the store does not hold: the fact, then the fix.
fn not_installed_fix(name: &str) -> String {
    format!("atpkg: {name} is not installed (fix: aterm pkg install {name})")
}

/// The answer for a command nothing here runs, read from the record of the PROGRAM that
/// ships it ([`crate::stub::program_of`]: `targo` comes with `trust`): that program's
/// row when it says why no install changes it — no build for this target, or a hold
/// waiting on a person — else the install of the program, never of the command.
///
/// `aterm trust` and `aterm ay` on aarch64 Linux said "not installed (fix: aterm pkg
/// install trust)", a fix that answers "unavailable … no build is published for this
/// target" and changes nothing; `aterm pkg which targo` promised `aterm pkg install
/// targo`, which install refuses (no program by that name).
fn not_runnable_line(layout: &crate::store::Layout, tool: &str) -> String {
    let program = crate::stub::program_of_in(layout, tool);
    let program = program.as_str();
    let stated = crate::status::read(layout)
        .and_then(|s| s.programs.get(program).map(|row| row.state.clone()))
        .filter(|state| {
            state.starts_with(crate::state::UNAVAILABLE_PREFIX)
                || state.starts_with(crate::state::HELD_FAILED_PREFIX)
        });
    not_runnable_words(tool, program, stated.as_deref())
}

/// [`not_runnable_line`]'s words, pure: `state` is the program's row when it says why.
fn not_runnable_words(tool: &str, program: &str, state: Option<&str>) -> String {
    match (state, tool == program) {
        (Some(state), true) => format!("atpkg: {tool}: {state}"),
        (Some(state), false) => format!("atpkg: {tool} comes with {program}: {state}"),
        (None, true) => not_installed_fix(tool),
        (None, false) => format!(
            "atpkg: {tool} comes with {program}, which is not installed (fix: aterm pkg install \
             {program})"
        ),
    }
}

/// A LIVE build of a program one of whose shims is gone — see [`live_without_shim`] (its
/// own-name shim) and [`live_without_shim_of`] (any tool it ships).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LostShim {
    /// The build `store/<program>/current` selects.
    pub build: u64,
    /// Where the missing shim belongs: `<prefix>/bin/<tool>`.
    pub shim: std::path::PathBuf,
    /// The build's own binary, `store/<program>/<build>/bin/<tool>`, which stands.
    pub binary: std::path::PathBuf,
}

/// The `agents/` twin that answers `tool` first on `path_var` while its `bin/` shim is
/// lost: `Some` only for an agent program whose twin resolves, with `agents/` ahead of
/// every foreign copy. The twin forwards to the store build, so that shell still runs the
/// managed copy. A `reroute/` agent stub is not consulted (it decides at exec time).
fn lost_shim_twin_runs(
    layout: &crate::store::Layout,
    tool: &str,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    if !crate::stub::is_agent_program(tool) {
        return None;
    }
    let twin = layout.agent_shim(&crate::store::ToolName::new(tool)?);
    crate::platform::resolve_shim(&twin)?;
    let foreign = crate::vendor::system_binary_on_path(&layout.prefix, tool, path_var);
    for dir in std::env::split_paths(path_var?) {
        if crate::vendor::agents_dir_on_path(&layout.prefix, Some(dir.as_os_str())) {
            return Some(twin);
        }
        if foreign.as_deref().and_then(std::path::Path::parent) == Some(dir.as_path()) {
            return None;
        }
    }
    None
}

/// Whether `<prefix>/agents` comes before the managed `bin/` on `path_var` (absolute
/// entries in order; `false` with neither) — what decides whether `which` names the twin,
/// which carries the self-update hand-over, as the copy that runs.
fn agents_dir_ahead_of_bin(
    layout: &crate::store::Layout,
    path_var: Option<&std::ffi::OsStr>,
) -> bool {
    let bin = layout.bin_dir();
    for dir in std::env::split_paths(path_var.unwrap_or_default()) {
        if !dir.is_absolute() {
            continue;
        }
        if crate::vendor::agents_dir_on_path(&layout.prefix, Some(dir.as_os_str())) {
            return true;
        }
        if dir == bin || std::fs::canonicalize(&dir).ok().as_deref() == Some(bin.as_path()) {
            return false;
        }
    }
    false
}

/// Whether `tool` is an installed program whose live build holds `bin/<tool>` while the
/// managed shim `bin/<tool>` is absent, and if so which build and binary. `run`, `which` and
/// the `aterm <tool>` front door read "installed" off the shim; this asks the store, as
/// `repair` does, so the answer can name `aterm pkg repair`.
///
/// `Some` only when all hold, so a name this machine turned off never comes back:
/// * `tool` is an admissible, non-alias name and the program's own (a bundle's same-named
///   binary, `store/trust/<n>/bin/ty`, never answers for `ty`);
/// * `bin/<tool>` is proved absent (a tombstone, pending stub, dev link or foreign file
///   there is never looked past);
/// * `store/<tool>/current` names a complete build (on an older prefix, the build its
///   surviving shims point into) whose `bin/<tool>` is a regular file;
/// * the program is not removed, dev-linked or disabled here (a `tombstoned:`/`aborted:`
///   row, a tombstone among its shims, or one over any name its build ships).
#[must_use]
pub fn live_without_shim(layout: &crate::store::Layout, tool: &str) -> Option<LostShim> {
    live_without_shim_of(layout, tool, tool)
}

/// [`live_without_shim`] for `tool`, one of the binaries `program` ships (the reroute's
/// question for a branded tool such as `trustfmt`): `program`'s store and state, `tool`'s
/// shim and binary.
#[must_use]
pub fn live_without_shim_of(
    layout: &crate::store::Layout,
    program: &str,
    tool: &str,
) -> Option<LostShim> {
    let name = crate::store::ToolName::new(tool)?;
    if name.is_alias() {
        return None;
    }
    let shim = layout.shim(&name);
    if !std::fs::symlink_metadata(&shim).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return None;
    }
    let build = match std::fs::read_link(layout.program_current(program)) {
        Ok(target) => match crate::ops::store_build_of(&layout.prefix, &target) {
            Some((owner, build)) if owner == program => build,
            // A link that names something else is not ours to reinterpret.
            _ => return None,
        },
        // No per-program link (an older prefix): the build is the one the program's shims
        // point into. That walk reads every shim in `bin/`, and the `aterm <word>` front
        // door asks this for every word with no shim, so a name with no store directory
        // stops at one `stat`.
        Err(_) => {
            if !layout
                .program_current(program)
                .parent()
                .is_some_and(std::path::Path::is_dir)
            {
                return None;
            }
            crate::active_builds(layout).get(program).copied()?
        }
    };
    if !crate::list_installed(layout)
        .iter()
        .any(|(p, b)| p == program && *b == build)
    {
        return None;
    }
    let binary = layout
        .build_dir(program, build)
        .join("bin")
        .join(format!("{tool}{}", std::env::consts::EXE_SUFFIX));
    if !binary.is_file()
        || layout.removed_programs().contains(program)
        || crate::linkmode::is_linked(layout, program)
        || program_disabled_here(layout, crate::status::read(layout).as_ref(), program)
    {
        return None;
    }
    // A tombstone over any name the build ships: `program_disabled_here` sees only the
    // exposes the surviving shims spell, which with the own-name shim lost can be none. A
    // tombstone does not name its program, so a same-named one of another program blocks
    // this one too (conservative).
    let shipped = std::fs::read_dir(layout.build_dir(program, build).join("bin")).ok()?;
    for entry in shipped.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let name = name
            .strip_suffix(std::env::consts::EXE_SUFFIX)
            .unwrap_or(name);
        if is_tombstone_shim(layout, name) {
            return None;
        }
    }
    Some(LostShim {
        build,
        shim,
        binary,
    })
}

/// The one stderr line `run` prints before it execs a program whose shim is lost.
fn lost_shim_run_notice(tool: &str, lost: &LostShim) -> String {
    format!(
        "atpkg: {tool} {} is installed but {} is missing — running it from the store (fix: \
         aterm pkg repair)",
        lost.build,
        lost.shim.display()
    )
}

/// What `atpkg run <tool>` does, decided without exec'ing — see [`cmd_run`].
#[derive(Debug, PartialEq, Eq)]
enum RunTarget {
    /// Exec `path` with `env` exported, printing `notice` to stderr first when there is one.
    Exec {
        /// The file to exec.
        path: std::path::PathBuf,
        /// The environment the shim would export ([`crate::ops::Managed::env`]).
        env: crate::shim_env::ShimEnv,
        /// A line to say before exec'ing it (a lost shim), or `None` for the ordinary case.
        notice: Option<String>,
    },
    /// A laid pending stub answers for the name.
    Pending,
    /// An agent program whose shim is lost: installed, but not run from here — see
    /// [`run_target`]. The line says so and names the repair.
    LostNotRun(String),
    /// Nothing on this machine answers: the classic not-installed line.
    NotInstalled,
}

/// [`cmd_run`]'s resolution: the shim's managed copy ([`crate::ops::managed`]); else a
/// pending stub; else a live build whose own-name shim is lost ([`live_without_shim`]),
/// exec'd from the store with its declared environment and a notice naming the repair;
/// else not installed. An agent program with a lost shim is named and not run: without
/// the shim, `aterm claude update` would reach the vendor's updater
/// ([`runs_self_update_verb`]).
fn run_target(layout: &crate::store::Layout, tool: &str) -> RunTarget {
    if let Some(copy) = crate::ops::managed(layout, tool) {
        return RunTarget::Exec {
            path: copy.target,
            env: copy.env,
            notice: None,
        };
    }
    if crate::stub::pending_stub_exists(layout, tool) {
        return RunTarget::Pending;
    }
    let Some(lost) = live_without_shim(layout, tool) else {
        return RunTarget::NotInstalled;
    };
    if crate::stub::is_agent_program(tool) {
        return RunTarget::LostNotRun(format!(
            "atpkg: {tool} {} is installed but {} is missing \u{2014} not run (fix: aterm pkg \
             repair)",
            lost.build,
            lost.shim.display()
        ));
    }
    RunTarget::Exec {
        // The exec root the shim's guard would take, as `exec_path` does for a shim that
        // stands (only a trust build ever has one).
        path: crate::compat::route_for_shim(&lost.shim, &lost.binary)
            .unwrap_or_else(|| lost.binary.clone()),
        // What the build declares — `exec_env`'s first source, read from the build this
        // resolved, since there is no shim to resolve it through or copy exports from.
        env: crate::shim_env::read_sidecar(&layout.build_dir(tool, lost.build)),
        notice: Some(lost_shim_run_notice(tool, &lost)),
    }
}

/// The `--progress-file` path this invocation carries, if any — set once at the dispatch
/// edge by [`strip_progress_file_flag`]. Process-global because its consumer
/// (`install_default_set_with_path`) sits several layers below verbs whose signatures are
/// shared with paths that must never know about it.
static PROGRESS_FILE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Remove every `--progress-file <path>` pair from `args`, recording the first path. A
/// missing value means no opt-in, not an error: a broken GUI spawn degrades to a
/// progress-less install, never a refused one.
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

/// `<prefix>/progress.json` when a person typed this pass on a terminal (set with the
/// meter, [`typed_pass_meter`]): what the meter draws and a pending stub in another shell
/// reads.
static TTY_PROGRESS: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// The progress file of a pass a person typed on a terminal, if this is one.
fn tty_progress_path() -> Option<&'static std::path::Path> {
    TTY_PROGRESS.get().map(std::path::PathBuf::as_path)
}

/// Where a provisioning pass writes its live progress: the window's `--progress-file`,
/// else the typed pass's own ([`tty_progress_path`]).
fn pass_progress_path() -> Option<&'static std::path::Path> {
    progress_path().or_else(tty_progress_path)
}

/// A progress pass of a person's typed verb over work the window's lanes meter nowhere
/// (the channel apply and vendor lane, one program's install or update): begun only when
/// typed on a terminal and no pass is live, ended when dropped. No plan: each row appears
/// as its download starts.
struct TypedPass(bool);

impl TypedPass {
    fn begin(name: &str) -> Self {
        Self(tty_progress_path().is_some_and(|p| crate::progress::begin_pass(p, name)))
    }
}

impl Drop for TypedPass {
    fn drop(&mut self) {
        if self.0 {
            crate::progress::end_pass();
        }
    }
}

/// The meter under a pass verb a person typed (`install`, `update`, `seed`; stderr a
/// terminal, no progress file asked): the pass writes `<prefix>/progress.json` and the
/// meter draws it, with a spinner between passes once a second has gone by. `None`
/// everywhere else.
fn typed_pass_meter(verb: &str, layout: &crate::store::Layout) -> Option<crate::meter::TtyMeter> {
    if !matches!(verb, "install" | "update" | "seed")
        || progress_path().is_some()
        || !crate::meter::wanted()
    {
        return None;
    }
    let _ = TTY_PROGRESS.set(layout.progress_file());
    let before = if verb == "update" {
        "Checking for updates\u{2026}"
    } else {
        "Checking the latest versions\u{2026}"
    };
    let started = std::time::Instant::now();
    // Whether a pass has worked on a program yet: after one, the time with no pass live
    // is the verb tidying up, not checking.
    let mut worked = false;
    crate::meter::TtyMeter::start(move || match crate::progress::live_snapshot() {
        Some(file) => {
            worked |= !file.programs.is_empty();
            crate::meter::Show::Pass {
                file,
                focus: None,
                idle: before.to_string(),
            }
        }
        None if started.elapsed() < std::time::Duration::from_secs(1) => {
            crate::meter::Show::Nothing
        }
        None if worked => crate::meter::Show::Busy("Finishing up\u{2026}".to_string()),
        None => crate::meter::Show::Busy(before.to_string()),
    })
}

/// The flag the head watch passes with `update <program>` — the window's, or with no window
/// open a terminal session's ([`crate::vendor_direct::watch::Runner`]): the pass is the
/// unattended lane's ([`VendorDoor::Window`]), not a person's. Never user vocabulary.
pub const HEAD_WATCH_FLAG: &str = "--head-watch";

/// The window's untrusted published-index wake, carried to a whole update pass
/// only as scheduling evidence — and as a guess its discovery rounds follow
/// ([`discovery_hint`]). It cannot select an index or waive signatures.
/// The requested build is recorded with the pass end even if its listing failed.
pub const PUBLISHED_INDEX_HINT_FLAG: &str = "--published-index-hint";

thread_local! {
    static PUBLISHED_INDEX_HINT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Strip the window's hidden published-index hint before verb dispatch. Invalid
/// or missing values mean no scheduling witness; the signed resolve is unchanged.
fn take_published_index_hint_flag(args: &mut Vec<String>) -> Option<u64> {
    let mut hint = None;
    while let Some(i) = args.iter().position(|a| a == PUBLISHED_INDEX_HINT_FLAG) {
        args.remove(i);
        if i < args.len() {
            hint = args
                .remove(i)
                .parse::<u64>()
                .ok()
                .filter(|build| *build > 0);
        }
    }
    hint
}

/// Whether the dispatch edge saw [`HEAD_WATCH_FLAG`].
static HEAD_WATCH_PASS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Remove every `flag` ([`HEAD_WATCH_FLAG`], [`DEFER_BUSY_FLIP_FLAG`]) from `args`,
/// answering whether there was one.
fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|a| a != flag);
    args.len() != before
}

/// Whether the dispatch edge saw `--head-watch`.
fn head_watch_pass() -> bool {
    HEAD_WATCH_PASS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The flag the unattended lanes (the window's package loop, a terminal session's detached
/// pass) put on every whole `update`: the coherence group holding the Trust toolchain stages
/// now and flips when nothing is using the toolchain it replaces, four hours at the most
/// ([`crate::quiet`]). Without it — a person's `aterm pkg update`, typed or from Settings —
/// the flip lands at once.
pub const DEFER_BUSY_FLIP_FLAG: &str = "--defer-busy-flip";

/// The word a window under a private state root puts ahead of the verb of every pass it
/// spawns: the pass carries no `[machine]` edit, whatever its own build is. The window's
/// `atpkg` (`aterm-gui`'s `co_located_atpkg`) may be a separate dev build that compiles no
/// seam, reads the inherited `ATERM_STATE_HOME` as unset, and would carry the instance's
/// `[machine]` table to the owner's Mac. It leads the verb so an `atpkg` that predates the
/// flag refuses it as an unknown verb (exit 2, nothing done). A shipped window never passes it.
pub const PRIVATE_STATE_ROOT_FLAG: &str = "--private-state-root";

/// Whether the dispatch edge saw [`PRIVATE_STATE_ROOT_FLAG`] ahead of the verb.
static PRIVATE_STATE_ROOT_LEAD: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether this pass runs for a private state root: its own process's seam
/// ([`aterm_types::dirs::runs_under_private_state_root`]), or the lead of a window that
/// knows it runs under one ([`PRIVATE_STATE_ROOT_FLAG`]).
fn under_private_state_root() -> bool {
    PRIVATE_STATE_ROOT_LEAD.load(std::sync::atomic::Ordering::Relaxed)
        || aterm_types::dirs::runs_under_private_state_root()
}

/// Whether the dispatch edge saw [`DEFER_BUSY_FLIP_FLAG`].
static DEFER_BUSY_FLIP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// This pass's flip policy, as the dispatch edge recorded it.
fn flip_policy() -> crate::quiet::FlipPolicy {
    if DEFER_BUSY_FLIP.load(std::sync::atomic::Ordering::Relaxed) {
        crate::quiet::FlipPolicy::WhenQuiet
    } else {
        crate::quiet::FlipPolicy::Now
    }
}

/// What the dispatch edge recorded from `--wait-lock <secs>`: the bound, and the pid of
/// the parent that spawned this process — the window — so a waiter can tell "my parent is
/// gone" as a change of parent rather than as "re-parented to pid 1", which holds on macOS
/// (orphans go to launchd) but not under a Linux subreaper (`systemd --user`, most
/// container and sandbox runtimes).
#[derive(Clone, Copy, Debug)]
struct WaitLock {
    bound: std::time::Duration,
    spawner: Option<u32>,
}

/// Arm the existing bounded store wait for either a provisioning pass or an explicit link.
fn configure_wait_lock(args: &[String], bound: std::time::Duration) {
    // Capture the spawner before the wait: orphaning is a change of parent.
    let _ = WAIT_LOCK.set(WaitLock {
        bound,
        spawner: spawner_pid(),
    });
    if args.first().is_some_and(|verb| verb == "update")
        && let Some(layout) = crate::store::resolve_configured()
    {
        let _ = QUEUED_AT_PASS_SEQ.set(crate::status::pass_seq(&layout));
    }
    // Keep the existing orphan handling: a departed spawner must not strand its waiter
    // or leave a later write pointed at an unread pipe.
    let log = crate::store::resolve_configured()
        .map(|layout| layout.prefix.join(crate::progress::ORPHAN_LOG_NAME))
        .unwrap_or_else(|| std::env::temp_dir().join(crate::progress::ORPHAN_LOG_NAME));
    crate::progress::watch_for_orphaning(log);
}

/// The [`WaitLock`] this invocation carries, if any — set once by [`main_entry`], read
/// through [`wait_lock`]. Process-global like [`PROGRESS_FILE`]: the lock edge has no argv.
static WAIT_LOCK: std::sync::OnceLock<WaitLock> = std::sync::OnceLock::new();

/// Whether a `--wait-lock` wait found the store lock contended at least once before it
/// took it ([`pass_ended_while_we_waited`]).
static WAITED_FOR_LOCK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The store's full-pass count ([`crate::status::Status::pass_seq`]) when this queued
/// `update` started, read before it asked for the lock ([`pass_ended_while_we_waited`]).
static QUEUED_AT_PASS_SEQ: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

fn unix_now() -> i64 {
    aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX))
}

/// Whether the holder already did this pass. Lanes cannot see each other's children, so
/// "never back to back" is kept here: a full `update` child of a `--wait-lock` lane that
/// waited at the lock and finds the store's full-pass count moved since it was queued
/// (`queued_at` vs `seq_now`) stands down behind the pass that ended, however it ended —
/// `Some((outcome, secs ago))`. A count, not a clock: whole-second stamps cannot order an
/// end in the second the wait began. A lock taken at once, or a holder that counted
/// nothing (`pass_seq` absent), runs.
fn pass_ended_since_queued(
    waited: bool,
    queued_at: Option<u64>,
    seq_now: u64,
    stamps: &aterm_update_core::pkg_check::Stamps,
    now_unix: i64,
    requested_index: Option<u64>,
) -> Option<(aterm_update_core::pkg_check::PassOutcome, u64)> {
    use aterm_update_core::pkg_check::PassOutcome;
    if !waited || queued_at.is_none_or(|queued| seq_now <= queued) {
        return None;
    }
    // A child queued for published index N does not stand down behind a holder whose
    // finished pass attempted an older index; an unknown witness, or an attempt at N (even
    // a failed one), stands down as above.
    if requested_index.is_some_and(|requested| stamps.completed_older_index_pass(requested)) {
        return None;
    }
    let outcome = if stamps.last_failed() {
        stamps.last_outcome.unwrap_or(PassOutcome::Failed)
    } else {
        PassOutcome::Ok
    };
    let ago = stamps
        .last_attempt
        .map_or(0, |ended| now_unix.saturating_sub(ended));
    Some((outcome, u64::try_from(ago).unwrap_or(0)))
}

/// [`pass_ended_since_queued`] over this process's wait and `layout`'s record.
fn pass_ended_while_we_waited(
    layout: &crate::store::Layout,
) -> Option<(aterm_update_core::pkg_check::PassOutcome, u64)> {
    let waited = WAITED_FOR_LOCK.load(std::sync::atomic::Ordering::Relaxed);
    if !waited {
        return None;
    }
    let stamps = aterm_update_core::pkg_check::Stamps::read(&layout.status());
    let requested_index = PUBLISHED_INDEX_HINT.with(std::cell::Cell::get);
    pass_ended_since_queued(
        waited,
        QUEUED_AT_PASS_SEQ.get().copied(),
        crate::status::pass_seq(layout),
        &stamps,
        unix_now(),
        requested_index,
    )
}

/// The line a full pass that stood down behind one that ended while it waited says: that
/// pass's ending, as that pass would have said it to the window (`said` is its failure
/// sentence). A failed pass is reported, not repeated back to back: the window's failure
/// ladder retries it. No marker, and nothing recorded: the record is the holder's.
fn stood_down_after_wait(
    outcome: aterm_update_core::pkg_check::PassOutcome,
    ago: u64,
    said: &str,
) -> String {
    use aterm_update_core::pkg_check::PassOutcome;
    match outcome {
        PassOutcome::Ok => format!("atpkg: up to date (checked {ago} s ago)"),
        PassOutcome::Offline => {
            format!("atpkg: offline \u{2014} the last update reached no host {ago} s ago")
        }
        PassOutcome::Failed => format!(
            "atpkg: the last update failed {ago} s ago ({}); the next scheduled update retries it",
            if said.trim().is_empty() {
                "it recorded no reason"
            } else {
                said.trim()
            }
        ),
    }
}

/// How a full pass that stood down exits ([`stand_down_exit`]): its line, its stream, its
/// code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandDown {
    /// The one line ([`stood_down_after_wait`]).
    pub line: String,
    /// Whether it goes to stderr — the stream the window reads as a failure's words.
    pub on_stderr: bool,
    /// The exit code.
    pub code: u8,
}

/// The stand-down exits as the pass it stood behind did: `ok` 0 on stdout; `offline` the
/// offline code on stdout; `failed` 1 on stderr with that pass's sentence — except when that
/// pass failed only because its index came from the cache (`stale_index_only`,
/// [`crate::status::stale_index_only_end`]): that pass exited 1 with nothing on stderr, so
/// this one says its line on stdout and exits 1 the same way, which the window reads as a
/// stale index, not a failure. `pub` so the window's verdict test drives it.
#[must_use]
pub fn stand_down_exit(
    outcome: aterm_update_core::pkg_check::PassOutcome,
    ago: u64,
    said: &str,
    stale_index_only: bool,
) -> StandDown {
    use aterm_update_core::pkg_check::PassOutcome;
    let line = stood_down_after_wait(outcome, ago, said);
    let (on_stderr, code) = match outcome {
        PassOutcome::Ok => (false, 0),
        PassOutcome::Offline => (false, aterm_update_core::pkg_check::PASS_OFFLINE_EXIT),
        PassOutcome::Failed => (!stale_index_only, 1),
    };
    StandDown {
        line,
        on_stderr,
        code,
    }
}

#[cfg(test)]
mod stand_down_after_wait_tests {
    use super::{attempted_pass_index_build, pass_ended_since_queued, stood_down_after_wait};
    use aterm_update_core::pkg_check::{PassOutcome, Stamps};

    /// A child that waited stands down behind any full pass that ended since it was queued,
    /// with that pass's ending as its own. One that did not wait, whose count did not move,
    /// or whose holder counted nothing (an older atpkg), runs.
    #[test]
    fn a_waited_child_stands_down_behind_any_full_pass_that_ended_since_it_queued() {
        let now = 1_700_000_020;
        let ended = |outcome: Option<PassOutcome>, at: i64| Stamps {
            last_success: (outcome == Some(PassOutcome::Ok)).then_some(at),
            last_attempt: Some(at),
            last_outcome: outcome,
            ..Stamps::default()
        };
        for outcome in [PassOutcome::Ok, PassOutcome::Failed, PassOutcome::Offline] {
            assert_eq!(
                pass_ended_since_queued(
                    true,
                    Some(4),
                    5,
                    &ended(Some(outcome), now - 8),
                    now,
                    None
                ),
                Some((outcome, 8)),
                "{outcome:?}: the holder ended 8 s ago: stand down"
            );
        }
        // A same-second end: a clock could not tell it from an end before the wait; the
        // count can.
        assert_eq!(
            pass_ended_since_queued(
                true,
                Some(4),
                5,
                &ended(Some(PassOutcome::Ok), now),
                now,
                None
            ),
            Some((PassOutcome::Ok, 0)),
            "a same-second end is a moved count: stand down"
        );
        let failed = ended(Some(PassOutcome::Failed), now - 8);
        assert_eq!(
            pass_ended_since_queued(true, Some(5), 5, &failed, now, None),
            None,
            "no end since this child was queued (the record is the previous pass): run"
        );
        assert_eq!(
            pass_ended_since_queued(false, Some(4), 5, &failed, now, None),
            None,
            "a lock taken at the first try waited for nobody: run"
        );
        assert_eq!(
            pass_ended_since_queued(true, None, 5, &failed, now, None),
            None,
            "not a queued child (no count read at its start): run"
        );
        assert_eq!(
            pass_ended_since_queued(true, Some(0), 0, &failed, now, None),
            None,
            "a holder that counted nothing (an older atpkg): run"
        );
        assert_eq!(
            pass_ended_since_queued(
                true,
                Some(4),
                5,
                &ended(Some(PassOutcome::Ok), now + 30),
                now,
                None
            ),
            Some((PassOutcome::Ok, 0)),
            "a stamp ahead of the clock reads as just now"
        );
        assert!(stood_down_after_wait(PassOutcome::Ok, 8, "").contains("up to date"));
        assert!(stood_down_after_wait(PassOutcome::Offline, 8, "").contains("offline"));
        let line =
            stood_down_after_wait(PassOutcome::Failed, 8, "update failed: index unreachable");
        assert!(
            line.contains("(update failed: index unreachable)"),
            "{line}"
        );
        assert!(
            stood_down_after_wait(PassOutcome::Failed, 8, " ").contains("(it recorded no reason)")
        );
    }

    #[test]
    fn queued_new_index_runs_after_older_pass_but_same_failed_target_stands_down() {
        use aterm_update_core::pkg_check::{PassOutcome, Stamps};
        let model = aterm_models_atpkg::atpkg_published_spacing_model();
        let mut modeled = model.init_state();
        for action in ["EndOldOk", "Publish45", "DecideQueued"] {
            assert!(model.fire(action, &mut modeled), "{action}: {modeled:?}");
        }
        let first = 1_700_000_000;
        let ended = first + 12;
        let record = |outcome, target: Option<u64>| Stamps {
            last_success: (outcome == PassOutcome::Ok).then_some(ended),
            last_attempt: Some(ended),
            last_outcome: Some(outcome),
            last_pass_target: target.map(|build| (ended, build)),
            ..Stamps::default()
        };
        let actual = pass_ended_since_queued(
            true,
            Some(0),
            1,
            &record(PassOutcome::Ok, Some(44)),
            ended + 1,
            Some(45),
        );
        assert_eq!(actual.is_none(), modeled["queued"] == 1);
        assert_eq!(actual, None, "queued 45 runs after holder checked 44");
        for outcome in [PassOutcome::Ok, PassOutcome::Failed, PassOutcome::Offline] {
            assert_eq!(
                pass_ended_since_queued(
                    true,
                    Some(0),
                    1,
                    &record(outcome, Some(45)),
                    ended + 1,
                    Some(45)
                ),
                Some((outcome, 1)),
                "the holder already attempted 45: {outcome:?}"
            );
        }
        assert_eq!(
            pass_ended_since_queued(
                true,
                Some(0),
                1,
                &record(PassOutcome::Ok, None),
                ended + 1,
                Some(45)
            ),
            Some((PassOutcome::Ok, 1)),
            "an old record without a target fails closed"
        );
        assert_eq!(
            pass_ended_since_queued(
                true,
                Some(0),
                1,
                &record(PassOutcome::Failed, Some(44)),
                ended + 1,
                Some(45)
            ),
            None,
            "a newer 45 can run after an older 44 failed"
        );
    }

    #[test]
    fn pass_target_needs_a_real_index_attempt_and_never_downgrades_the_requested_build() {
        assert_eq!(attempted_pass_index_build(45, 0, false), 0);
        assert_eq!(attempted_pass_index_build(45, 44, true), 45);
        assert_eq!(attempted_pass_index_build(44, 45, true), 45);
        assert_eq!(attempted_pass_index_build(0, 45, true), 45);
        assert_eq!(attempted_pass_index_build(45, 0, true), 45);
    }
}

/// The environment variable a spawner sets beside `--wait-lock` to its own pid, so the
/// waiter's "my window is gone" compares against a fact. The fallback, `getppid` at the
/// dispatch edge, races a parent that dies in the child's first milliseconds (the edge then
/// records init or the subreaper). `aterm-gui` imports the name.
///
/// Unnamed, the edge records the parent it sees and the waiter stands down when its parent
/// changes ([`is_orphaned`]): a `nohup … &` job stands down (silently, 75) once its shell
/// exits. A parent already init at the edge (a launchd agent's job) never changes, so that
/// waiter waits out its bound: the rule is a change of parent, never `ppid 1` alone. A
/// spawner that detaches its pass on purpose sets [`SPAWNER_DETACHED`] instead.
pub const SPAWNER_PID_ENV: &str = "ATPKG_SPAWNER_PID";

/// The [`SPAWNER_PID_ENV`] value of a pass detached on purpose: no spawner is recorded, so
/// the waiter is never judged orphaned ([`is_orphaned`]) and queues for its whole bound.
pub const SPAWNER_DETACHED: &str = "detached";

/// This process's parent pid right now; `None` where the platform has no notion
/// of one that std exposes.
fn current_parent_id() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(std::os::unix::process::parent_id())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The pid of the process that spawned this one: what the spawner said
/// ([`SPAWNER_PID_ENV`]; nobody for [`SPAWNER_DETACHED`]), else the parent pid now.
fn spawner_pid() -> Option<u32> {
    spawner_from(
        std::env::var(SPAWNER_PID_ENV).ok().as_deref(),
        current_parent_id,
    )
}

/// [`spawner_pid`] over the variable's value and the parent reader. Pure for the test.
fn spawner_from(said: Option<&str>, parent: impl FnOnce() -> Option<u32>) -> Option<u32> {
    match said.map(str::trim) {
        Some(SPAWNER_DETACHED) => None,
        said => said.and_then(|v| v.parse::<u32>().ok()).or_else(parent),
    }
}

/// Whether a waiter spawned by `spawner` has been orphaned, given its parent pid `now`: its
/// parent is no longer the spawner. A change, never "pid 1" on its own (see
/// [`SPAWNER_PID_ENV`]). An unreadable parent (`now == None`, Windows) or an unnamed spawner
/// is never judged gone; both poll out the bound, still a try at the end.
fn is_orphaned(spawner: Option<u32>, now: Option<u32>) -> bool {
    matches!((spawner, now), (Some(spawner), Some(now)) if now != spawner)
}

/// Remove every `--wait-lock <secs>` pair from `args`, returning the last parsable value.
/// An unparsable value means "no wait" (like `--progress-file`, a broken spawn degrades to
/// the fail-fast try, never a refused verb). The token after the flag is consumed either
/// way, so it is never read as a program name — a valueless flag mid-argv eats what follows
/// (`install --wait-lock --default-set` is refused as a bare `install`); no spawner
/// produces that shape.
fn take_wait_lock_flag(args: &mut Vec<String>) -> Option<std::time::Duration> {
    let mut wait = None;
    while let Some(i) = args.iter().position(|a| a == "--wait-lock") {
        args.remove(i);
        if i < args.len() {
            let value = args.remove(i);
            if let Ok(secs) = value.parse::<u64>() {
                wait = Some(std::time::Duration::from_secs(secs));
            }
        }
    }
    wait
}

/// The process a seed's promise of the next pass rides on ([`cmd_seed`]): the window that
/// spawned it, which runs its update next. Only a window's seed carries both `--wait-lock`
/// (naming that window, [`SPAWNER_PID_ENV`]) and `--progress-file`; a typed `aterm pkg seed`
/// carries neither and promises nothing, and nor does a seed detached on purpose.
fn seed_promise_owner() -> Option<u32> {
    progress_path().and(wait_lock())?.spawner
}

/// The wait the edge recorded, if the caller opted in.
fn wait_lock() -> Option<WaitLock> {
    WAIT_LOCK.get().copied()
}

/// Mark `program` terminal on the live progress pass, if one is running — the one funnel
/// for `done`/`failed`/`skipped`, so the overall counter never double-counts a program.
fn note_finished(program: &str, phase: crate::progress::Phase, error: Option<String>) {
    if let Some(sink) = crate::progress::active() {
        sink.finished(program, phase, error);
    }
}

/// `atpkg __pending <tool>` — the hidden verb a pending-program stub execs.
///
/// Reads `progress.json` + `status.toml` under the untrusted-reader rules, appends the tool
/// to the reorder-only bump file, prints one honest state, and exits 127 (the stub's
/// contract: the tool did not run) — unless [`run_pending`] runs it.
fn cmd_pending(tool: Option<&String>, passthrough: &[String]) -> ExitCode {
    let Some(tool) = tool else {
        eprintln!("usage: atpkg __pending <tool> [args…]");
        return ExitCode::from(2);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    run_pending(&layout, tool, passthrough)
}

/// The copy an agent program's pending stub runs meanwhile: a foreign `claude`/`codex` on
/// `path_var` outside the managed prefix (a brew cask, the vendor's native installer);
/// `None` for every other program and when there is none. The agent stubs sit in
/// `agents/`, first on every PATH, and must not stand in for the user's working copy for
/// the minutes of an install: the stub says one line on stderr and execs that copy with the
/// arguments verbatim, until the managed twin replaces it.
#[must_use]
pub fn pending_passthrough(
    layout: &crate::store::Layout,
    tool: &str,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    if !crate::stub::is_agent_program(tool) {
        return None;
    }
    crate::vendor::system_binary_on_path(&layout.prefix, tool, path_var)
}

/// `atpkg __landing <program> [<prefix>] -- [args…]` — the hidden verb an older client's
/// `agents/` twin execs while `<prefix>/landing/<program>` stands ([`crate::landing`]): it
/// `exec`s the current `bin/<program>` shim at once, silently (on Windows the `.cmd` shim
/// runs as a child, and its exit code is this one's).
fn cmd_landing(rest: &[String]) -> ExitCode {
    // The operands ([`crate::landing::HandOver::parse`]): the program; the prefix the twin's
    // prelude passes (an older twin passes none), so the store is found with no `HOME`
    // (`env -i`, launchd, a container entrypoint); and the user's arguments with the one
    // `--` the twin inserted stripped, so a user's own leading `--` survives.
    let Some(hand_over) = crate::landing::HandOver::parse(rest) else {
        eprintln!(
            "usage: atpkg {} <program> [<prefix>] -- [args…]",
            crate::landing::HIDDEN_VERB
        );
        return ExitCode::from(2);
    };
    let program = hand_over.program;
    let Some(tool) = crate::store::ToolName::new(program) else {
        eprintln!("atpkg: {program:?} is not a tool name");
        return ExitCode::from(2);
    };
    let layout = match hand_over.prefix {
        Some(prefix) => crate::store::Layout { prefix },
        None => match layout() {
            Some(l) => l,
            None => return ExitCode::from(1),
        },
    };
    // The current `bin/<program>` shim, never the `agents/` twin, whose prelude would hand
    // over here again. `exec_or_run` returns only the error that stopped it.
    let mut command = crate::landing::shim_command(&layout, &tool, hand_over.args);
    let err = crate::platform::exec_or_run(&mut command);
    eprintln!(
        "atpkg: could not run {}: {err}",
        command.get_program().to_string_lossy()
    );
    ExitCode::from(126)
}

/// `atpkg __selfupdate <program> [<prefix>] -- [args…]` — the hidden verb an agent
/// program's `agents/` twin execs when the user's FIRST argument is one of the vendor's
/// own self-update verbs ([`crate::selfupdate`]): the package manager checks and updates
/// it instead. The operands are [`crate::landing::HandOver`]'s, with one
/// addition: a program operand that starts with `-` (`atpkg __selfupdate --help`) is the
/// usage line, exit 2 — non-mutating by design, not by the accident of a refused name.
///
/// In order, each ending pinned at the process edge by `tests/selfupdate_intercept.rs`
/// — but three, which `cmd_selfupdate_refuses_forwards_and_never_takes_the_lock` pins
/// in-process: a declined shape (5), also driven on the binary through `atpkg run`,
/// which settles the same plan (`tests/run_is_the_managed_name.rs`); the disabled line
/// (6), a build with no root key's alone; and the child's 75 (7), which needs its whole
/// bound to elapse, so its relay is pinned with a stub child, and the real binary's 75
/// by `tests/store_lock_wait.rs`:
/// 1. usage exit 2; not a tool name exit 2; the layout from the prefix operand (so no
///    `HOME` is needed — `env -i`, launchd) else [`layout`] (exit 1 with its line);
/// 2. no roster row ⇒ `exec` the `bin/<program>` shim verbatim, nothing printed — a twin
///    laid by a NEWER client against this older roster must still run the tool;
/// 3. THE PREFIX CROSS-CHECK: when [`crate::store::resolve_configured`] resolves here
///    (it falls back to the passwd home when `HOME` is unset) and, canonicalized, names a
///    DIFFERENT store than the operand, refuse with exit 2 naming both — a child `update`
///    would move THAT store, never the one this twin runs; when it cannot resolve at
///    all, the operand layout stands;
/// 4. [`crate::selfupdate::classify`]: help ⇒ one stderr note, then the shim verbatim
///    (the vendor prints its own help; never mutates); pass-through ⇒ the shim verbatim,
///    silent; otherwise on to 5 with the verdict in hand;
/// 5. a declined shape ⇒ its line, exit 2, nothing run — never handed to the vendor's
///    own updater, whose copy this name never runs, and with no bypass named, because
///    there is none; a check ⇒ on;
/// 6. the manager disabled (a build with no root key) ⇒ the disabled line, exit 1,
///    nothing run — NOT handed to the vendor either, for the same reason; `aterm pkg
///    doctor` says why the manager is off;
/// 7. the announce line, flushed, then THE CHILD: the embedded co-located `atpkg` (else
///    this executable) as `update <program> --wait-lock 1800`
///    ([`crate::selfupdate::WAIT_LOCK_SECS`], the window's own bound — a store another
///    pass holds is waited for, audibly: the child's `lock-waiting:` after 2 s and
///    `lock-acquired:` when it gets the store) with `ATPKG_SPAWNER_PID` set to this pid
///    and stdio INHERITED — the shape the window's pass uses minus its `--progress-file`
///    — waited for as its parent for the whole wait (Ctrl-C reaches both through the
///    foreground group and ends it; no handler, no epilogue for a signal death).
///    The child's status, relayed: 0 ⇒ nothing
///    more, the child's own stdout line was the verdict; 2 ⇒ 2; 75 ⇒ the contended line
///    and 75; anything else (1, a signal) ⇒ the incomplete line and 1; a spawn that
///    fails ⇒ `could not run`, 126.
///
/// WHY A CHILD and not `cmd_update_one` in-process: hidden verbs are dispatched before
/// the lock and outside [`verb_mutates_store`], so an in-process update would be an
/// unaudited lock-taker; the `--wait-lock`/orphan statics are edge-owned; a panic would
/// die holding the lock inside the twin's process; and the child gets the real dispatch
/// edge and, for its own lines, bytes identical to a typed `aterm pkg update claude` —
/// plus the `--wait-lock` lane's stdout markers a typed verb never prints (under a held
/// lock: `lock-waiting:` after the 2 s grace, `lock-acquired:`
/// when the holder lets go, `seed-busy:` at the 75 ending; [`SEED_BUSY_MARKER`] records
/// the exception, `tests/selfupdate_intercept.rs` pins the two the contended ending
/// prints). WHY NOT `exec` the child: the 75 and 1 epilogues are what turn "refusing to
/// mutate the store concurrently" into "the window's pass is doing this work".
fn cmd_selfupdate(rest: &[String]) -> ExitCode {
    let hand_over = crate::landing::HandOver::parse(rest).filter(|h| !h.program.starts_with('-'));
    let Some(hand_over) = hand_over else {
        eprintln!(
            "usage: atpkg {} <program> [<prefix>] -- [args…]",
            crate::selfupdate::HIDDEN_VERB
        );
        return ExitCode::from(2);
    };
    let program = hand_over.program;
    let Some(tool) = crate::store::ToolName::new(program) else {
        eprintln!("atpkg: {program:?} is not a tool name");
        return ExitCode::from(2);
    };
    let layout = match hand_over.prefix {
        Some(prefix) => crate::store::Layout { prefix },
        None => match layout() {
            Some(l) => l,
            None => return ExitCode::from(1),
        },
    };
    let resolved = crate::store::resolve_configured();
    let plan = selfupdate_plan(
        &layout,
        &tool,
        hand_over.args,
        resolved.as_ref(),
        manager_enabled(),
    );
    settle_selfupdate(&layout, plan)
}

/// Carry out what [`selfupdate_plan`] decided: the refusal and disabled lines with their
/// codes, the forward's `exec`, the check's child. Shared with `atpkg run` ([`cmd_run`]),
/// which runs the forward its own way.
fn settle_selfupdate(layout: &crate::store::Layout, plan: SelfUpdatePlan) -> ExitCode {
    match plan {
        SelfUpdatePlan::Refuse(line) => {
            eprintln!("{line}");
            ExitCode::from(2)
        }
        SelfUpdatePlan::Disabled(line) => {
            eprintln!("{line}");
            ExitCode::from(1)
        }
        SelfUpdatePlan::Forward { mut command, note } => {
            if let Some(note) = note {
                eprintln!("{note}");
            }
            // The `bin/<program>` shim with the arguments verbatim
            // ([`crate::platform::exec_or_run`]). An agent program's `bin/` carries the
            // self-update block too; the `exec` keeps this pid, so the block's re-entry
            // guard ([`crate::selfupdate::REENTRY_VAR`]) runs the store build instead of
            // handing over here again.
            let err = crate::platform::exec_or_run(&mut command);
            eprintln!(
                "atpkg: could not run {}: {err}",
                command.get_program().to_string_lossy()
            );
            ExitCode::from(126)
        }
        SelfUpdatePlan::Check { row } => selfupdate_check(
            layout,
            row,
            &selfupdate_child_binary(),
            crate::selfupdate::WAIT_LOCK_SECS,
        ),
    }
}

/// What [`cmd_selfupdate`] decided, before anything runs or prints — the pure half of
/// the verb, so every ending that is not the child is pinned in-process (an `exec`
/// would replace the test process; the tests run the planned command as a child instead
/// and read its argv off a log).
#[derive(Debug)]
enum SelfUpdatePlan {
    /// One stderr line, exit 2, nothing run: the prefix cross-check, or a declined shape.
    Refuse(String),
    /// One stderr line, exit 1, nothing run: the manager is disabled here.
    Disabled(String),
    /// `exec` the `bin/<program>` shim with the arguments verbatim, after `note` when there
    /// is one (the help note). `atpkg run` prints the note and runs the tool its own way.
    Forward {
        /// Boxed: a `Command` is far larger than every other variant's payload (a line,
        /// a row), and how large depends on the standard library that built it.
        command: Box<aterm_uds::spawnfd::StdCommand>,
        note: Option<String>,
    },
    /// Announce, then the standard update as a child ([`selfupdate_check`]).
    Check {
        row: &'static crate::vendor_direct::VendorSpec,
    },
}

/// Steps 2–6 of [`cmd_selfupdate`], pure: the row, the prefix cross-check against
/// `resolved` (`None` ⇒ nothing to check against), the classification, and `enabled`, the
/// one fact the process supplies.
fn selfupdate_plan(
    layout: &crate::store::Layout,
    tool: &crate::store::ToolName,
    args: &[String],
    resolved: Option<&crate::store::Layout>,
    enabled: bool,
) -> SelfUpdatePlan {
    use crate::selfupdate::{self, Verdict};
    let forward = |note: Option<String>| SelfUpdatePlan::Forward {
        command: Box::new(crate::landing::shim_command(layout, tool, args)),
        note,
    };
    let Some(row) = selfupdate::row_for(tool.as_str()) else {
        return forward(None);
    };
    if let Some(resolved) = resolved {
        let canon =
            |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        if canon(&resolved.prefix) != canon(&layout.prefix) {
            return SelfUpdatePlan::Refuse(selfupdate::prefix_mismatch_line(
                row,
                &layout.prefix,
                &resolved.prefix,
            ));
        }
    }
    // A declined shape is refused (exit 2, nothing run; [`cmd_selfupdate`] step 5). Help
    // and a pass-through forward, with their own note or none.
    match selfupdate::classify(row, args) {
        Verdict::Help => return forward(Some(selfupdate::help_line(row))),
        Verdict::PassThrough => return forward(None),
        Verdict::Declined(d) => {
            // An ALab build is updated from ALab's channel ([`crate::alab`]): the refusal
            // names that, never the vendor's latest.
            let active = crate::ops::active_builds(layout).get(row.program).copied();
            return SelfUpdatePlan::Refuse(
                match crate::alab::agent(row.program).filter(|_| {
                    active.is_some_and(|b| crate::alab::build_is_alab(layout, row.program, b))
                }) {
                    Some(agent) => selfupdate::alab_declined_line(agent, &d),
                    None => selfupdate::declined_line(row, &d),
                },
            );
        }
        // The words typed are the classification's evidence; the announce line states what
        // the person has.
        Verdict::Check { .. } => {}
    }
    if !enabled {
        let build = crate::ops::active_builds(layout).get(row.program).copied();
        return SelfUpdatePlan::Disabled(selfupdate::disabled_line(row, build));
    }
    SelfUpdatePlan::Check { row }
}

/// Step 7 of [`cmd_selfupdate`]: the announce line, then the standard update as a child
/// this process stays the parent of — `atpkg` run as `update <program> --wait-lock
/// <bound>` ([`crate::selfupdate::child_argv`]) — and the child's status relayed with
/// literal arms. `atpkg` is [`selfupdate_child_binary`] and `bound` is
/// [`crate::selfupdate::WAIT_LOCK_SECS`] in production; both are PARAMETERS so the
/// in-process test can hand it a stub child that exits 75 — the relay of the 75
/// ending, which no environment knob exists to shorten (and none may). The active build
/// the epilogues name is read AFTER the child, offline
/// ([`crate::ops::active_builds`]): it is the build that STAYS.
fn selfupdate_check(
    layout: &crate::store::Layout,
    row: &crate::vendor_direct::VendorSpec,
    atpkg: &std::path::Path,
    bound: u64,
) -> ExitCode {
    use std::io::Write as _;
    // What the person HAS, read before the child: the active build off the `bin/` shims,
    // offline; the epilogues re-read it AFTER the child, because theirs is the build
    // that stays.
    let active = crate::ops::active_builds(layout).get(row.program).copied();
    // An ALab build ([`crate::alab`]) is updated from ALab's channel, by the same verb.
    match active.filter(|b| crate::alab::build_is_alab(layout, row.program, *b)) {
        Some(build) => eprintln!(
            "{}",
            crate::selfupdate::alab_announce_line(
                row.program,
                &crate::alab::build_words(layout, row.program, build)
            )
        ),
        None => {
            let have = crate::vendor_direct::have_words(layout, row.program, active);
            eprintln!("{}", crate::selfupdate::announce_line(row, &have));
        }
    }
    let _ = std::io::stderr().flush();
    let status = aterm_uds::spawnfd::command(atpkg)
        .args(crate::selfupdate::child_argv(atpkg, row.program, bound))
        .env(SPAWNER_PID_ENV, std::process::id().to_string())
        .spawn()
        .and_then(|mut child| child.wait());
    let status = match status {
        Ok(status) => status,
        Err(e) => {
            eprintln!(
                "{}",
                crate::selfupdate::could_not_run_line(atpkg, row.program, &e)
            );
            return ExitCode::from(126);
        }
    };
    let build = || crate::ops::active_builds(layout).get(row.program).copied();
    match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(2) => ExitCode::from(2),
        Some(c) if c == i32::from(crate::lock::CONTENDED_EXIT) => {
            let person = crate::lock::holder_is_person(layout);
            eprintln!(
                "{}",
                crate::selfupdate::contended_line(row, build(), bound, person)
            );
            ExitCode::from(crate::lock::CONTENDED_EXIT)
        }
        // Killed by a signal: the child printed no verdict, so the line says what stays.
        None => {
            eprintln!("{}", crate::selfupdate::stopped_line(row, build()));
            ExitCode::from(1)
        }
        Some(_) => {
            eprintln!("{}", crate::selfupdate::incomplete_line(row, build()));
            ExitCode::from(1)
        }
    }
}

/// The binary [`selfupdate_check`] runs as the child: the co-located `atpkg`
/// ([`crate::stub::co_located_atpkg_path`]) when it is an executable regular file
/// ([`crate::stub::is_live_atpkg`]), else this process's own executable (a dev `aterm`
/// copied out of `target/`, a bundle missing the alias symlink). Under any name but `atpkg`
/// a bare `update` is aterm's app-update lane, so [`crate::selfupdate::child_argv`] puts
/// `pkg` ahead of the fallback's argv.
fn selfupdate_child_binary() -> std::path::PathBuf {
    let co_located = crate::stub::co_located_atpkg_path();
    if crate::stub::is_live_atpkg(&co_located) {
        return co_located;
    }
    std::env::current_exe().unwrap_or(co_located)
}

/// `atpkg __reroute <upstream> [args…]` — the hidden verb a reroute stub execs
/// (philosophy §4, [`crate::reroute::run`]). Unlisted for the reason `__pending` is:
/// it is machinery a stub reaches, not vocabulary a user types, so it is dispatched
/// before the verb table and is no row of it. Dispatched BEFORE the
/// store lock too: a `cargo build` typed while the installer HOLDS `store.lock` must
/// still get its signpost instantly, and the verb never mutates the store — it
/// announces, refuses (only under `[reroute] strict`), or execs a copy that is already
/// there. The arguments after the upstream name are the caller's, VERBATIM: the stub
/// inserts no separator of its own, so a leading `--` here is the user's and is kept
/// (`rustc -- x.rs`).
fn cmd_reroute(rest: &[String]) -> ExitCode {
    // Nothing unasked: this runs before every `cargo`, `rustc` and `rustfmt` a session
    // types, so a note about `aterm.toml` would be said before every build, and
    // `[reroute] announce = false` could not silence it. The doctor and Settings name the
    // file's troubles.
    crate::notice::silence();
    let Some((upstream, args)) = rest.split_first() else {
        eprintln!(
            "usage: atpkg {} <upstream> [args…]",
            crate::reroute::HIDDEN_VERB
        );
        return ExitCode::from(2);
    };
    let Some(layout) = layout() else {
        // No resolvable store (`layout` has said why): the upstream copy on PATH runs
        // after one line; `[reroute] strict = true` refuses instead, exit 2.
        return crate::reroute::run_storeless(upstream, args);
    };
    crate::reroute::run(&layout, upstream, args)
}

/// Re-assert the rustup seam: the managed `trust` toolchain linked into rustup as channel
/// `trust`, so `cargo +trust`, `targo` through rustup, and `channel = "trust"` pins work.
/// Fail-soft: no rustup home ⇒ nothing to do. `reassert` creates only names nothing else
/// owns, adopts a link already into the store, refuses anything foreign under `~/.rustup`
/// (fix printed), and replaces only a link to an older live build tree that nobody put
/// back and no build runs through (`seam::live_build_tree`), recorded with its undo
/// (`seam::replaced_from`). Every call prints the [`FOREIGN_MARKER`] line
/// ([`FOREIGN_SEAM`]): each foreign entry with [`crate::seam::DETACH_FIX`], or `none`.
fn reassert_rustup_seam(layout: &crate::store::Layout) {
    reassert_rustup_seam_at(layout, crate::seam::rustup_home().as_deref());
}

/// [`reassert_rustup_seam`] against the rustup home `home` (`None`: no rustup home), so
/// what the pass says for the window is provable from a test.
fn reassert_rustup_seam_at(layout: &crate::store::Layout, home: Option<&std::path::Path>) {
    let events = home.map_or_else(Vec::new, |home| crate::seam::reassert(layout, home));
    for event in &events {
        println!("atpkg: {event}");
    }
    let items: Vec<ForeignItem> = events
        .iter()
        .filter_map(crate::seam::SeamEvent::foreign_entry)
        .map(|(name, path)| ForeignItem {
            name: name.to_string(),
            path: path.to_path_buf(),
            fix: crate::seam::DETACH_FIX.to_string(),
        })
        .collect();
    emit_marker_line(&format!("atpkg: {}", foreign_line(FOREIGN_SEAM, &items)));
}

/// Bring the trust exec roots and every trust shim in line ([`crate::compat::reconcile`]),
/// print only what changed or failed, and answer whether nothing failed. A pass-level call,
/// not only the lay in `activate::install_tools_env`, because an up-to-date program is
/// short-circuited before any shim is laid. Called by update, install, the default set and
/// rollback; the seed, `repair` and doctor run it as the `base.pkg.exec-roots` item
/// ([`crate::converge::repair`]), at `Deep`. Failures go to
/// stderr, which the window keeps as what the pass said (aterm-gui's `read_pass_stderr`).
fn reconcile_exec_roots(layout: &crate::store::Layout, depth: crate::seam::Depth) -> bool {
    let report = crate::compat::reconcile(layout, depth);
    for n in &report.built {
        println!("{}", crate::compat::laid_line(layout, *n));
    }
    if !report.routed.is_empty() {
        let names: Vec<String> = report
            .routed
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        println!("atpkg: Trust commands updated: {}", names.join(", "));
    }
    for path in &report.swept {
        println!("atpkg: removed unused build copy {}", path.display());
    }
    for e in &report.errors {
        eprintln!("atpkg: exec root: warn — {e}");
    }
    report.errors.is_empty()
}

/// Detach every rustup seam atpkg recorded ([`crate::seam::detach_recorded`]), never
/// `--force`: an entry atpkg did not lay is refused and named. Silent with nothing to do.
fn detach_rustup_seams(layout: &crate::store::Layout) {
    let Some(home) = crate::seam::rustup_home() else {
        return;
    };
    for event in crate::seam::detach_recorded(layout, &home) {
        println!("atpkg: {event}");
    }
}

/// Lay (or refresh) the reroute stubs — at seed, after each install pass, and on `repair`
/// — and never fail the pass for them (the spawn seam re-lays them anyway): one stderr line
/// names what did not happen.
fn lay_reroute_stubs(layout: &crate::store::Layout) {
    // abrew's `brew` stub rides the same seams (docs/DESIGN-abrew-2026-10-19.md §6);
    // `reroute::lay` lays it too, so every spawn seam is covered by this one call.
    if let Err(err) = crate::reroute::lay(layout) {
        eprintln!(
            "atpkg: warn — reroute stubs not laid under {}: {err} (an aterm session lays \
             them at spawn; `aterm pkg repair` retries)",
            crate::reroute::dir(layout).display()
        );
    }
}

/// The `managed` state `which` prints for `program` at `build`: the row's own pin when the
/// row is managed; else (a faulted or absent row) the cached index's answer, so the line
/// says what the index pins and that the pass did not get there.
fn managed_state_of(
    layout: &crate::store::Layout,
    program: &str,
    build: u64,
    row: Option<&crate::ProgramStatus>,
) -> String {
    if let Some(spec) = crate::vendor_direct::spec(program) {
        return vendor_state_of(layout, spec, build, row);
    }
    if let Some((_, index)) = row.and_then(|r| crate::state::managed_pin(&r.state)) {
        return crate::state::managed(build, index);
    }
    // A managed row that claims no pin (`managed <build> — rolled back from <from>`) is this
    // build's own record, and its words lead.
    let own_words = row
        .filter(|r| {
            r.installed_build == Some(build)
                && crate::state::is_managed(&r.state)
                && crate::state::managed_pin(&r.state).is_none()
                && crate::state::shadowed_by(&r.state).is_none()
        })
        .map(|r| r.state.clone());
    let Some(index) = cached_index(layout) else {
        return match (own_words, row) {
            (Some(words), _) => words,
            (None, Some(r)) => format!("managed {build} — last pass: {}", r.state),
            (None, None) => crate::state::managed(build, build_floor(layout).index_build),
        };
    };
    let pinned = index
        .channel_for(crate::config::CHANNEL, current_triple())
        .and_then(|ch| ch.pin.get(program).copied());
    match (pinned, row) {
        (Some(pin), _) if pin == build => crate::state::managed(build, index.index_build),
        (Some(pin), Some(_)) if own_words.is_some() => format!(
            "{}; index {} pins {pin}",
            own_words.unwrap_or_default(),
            index.index_build
        ),
        (Some(pin), Some(r)) => format!(
            "managed {build} — index {} pins {pin} (last pass: {})",
            index.index_build, r.state
        ),
        (Some(pin), None) => format!(
            "managed {build} — index {} pins {pin} (not yet applied)",
            index.index_build
        ),
        (None, Some(r)) => format!("managed {build} — last pass: {}", r.state),
        (None, None) => crate::state::managed(build, index.index_build),
    }
}

/// The managed state a surface names for a vendor-direct program at `build`: the row the
/// vendor lane recorded for that build; else `… — Anthropic latest` when its stamp last saw
/// the head at this version; else `managed <version> — updates from <Vendor>`. Never an
/// index pin.
pub(crate) fn vendor_state_of(
    layout: &crate::store::Layout,
    spec: &crate::vendor_direct::VendorSpec,
    build: u64,
    row: Option<&crate::ProgramStatus>,
) -> String {
    // An ALab build ([`crate::alab`]) is no vendor version: its row as a lane wrote it
    // (pinned by the index, or kept), else what it is — never `updates from <vendor>`.
    if crate::alab::build_is_alab(layout, spec.program, build) {
        return row
            .filter(|r| r.installed_build == Some(build) && crate::state::is_managed(&r.state))
            .map_or_else(
                || {
                    crate::state::alab_kept(
                        build,
                        &crate::alab::build_words(layout, spec.program, build),
                    )
                },
                |r| r.state.clone(),
            );
    }
    let lane_row = row.filter(|r| {
        r.installed_build == Some(build)
            && crate::state::is_managed(&r.state)
            && crate::state::managed_pin(&r.state).is_none()
            && crate::state::shadowed_by(&r.state).is_none()
            && !r.state.contains(crate::state::AGENT_SHADOWED_IN_SHELL)
    });
    if let Some(r) = lane_row {
        return r.state.clone();
    }
    let label = crate::vendor_direct::build_label(build);
    let at_head = crate::vendor_direct::Version::from_build_id(build).is_some_and(|v| {
        crate::vendor_direct::ProgramStamp::read(layout, spec.program)
            .is_some_and(|s| s.last_head == Some(v))
    });
    if at_head {
        crate::state::vendor_managed(&label, spec.vendor)
    } else {
        crate::state::vendor_source(&label, spec.vendor)
    }
}

/// The pending verb's body, shared by `__pending` and `atpkg run`'s pending arm so the three
/// dead ends (the shell stub, `aterm <tool>`, `atpkg run <tool>`) share one state machine.
/// Interactive iff stdin and stdout are terminals.
///
/// Exits 127 (the command did not run), except: an agent program with a foreign copy to run
/// meanwhile ([`pending_passthrough`]) `exec`s that copy (126 if the `exec` fails; off Unix
/// the 127 stands); a caller whose program a running pass is installing waits and has it run
/// the moment it lands ([`wait_then_run`]): a person at a terminal under the meter, a script
/// or an agent's tool call with a stderr line every few seconds and a bound (a tool call
/// cannot "try again in a minute").
fn run_pending(layout: &crate::store::Layout, tool: &str, passthrough: &[String]) -> ExitCode {
    use std::io::IsTerminal as _;
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let onward = pending_passthrough(layout, tool, std::env::var_os("PATH").as_deref());
    let show = if interactive && crate::meter::wanted() {
        WaitShow::Meter
    } else {
        WaitShow::Lines
    };
    // A person asked when the stub reads from a terminal and speaks to one; a script, a pipe
    // or an agent's tool call does not, and its bump does not jump the toolchain.
    let asker = if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        crate::progress::Asker::Person
    } else {
        crate::progress::Asker::Script
    };
    let mut lines = Vec::new();
    let mut say = |line: String| lines.push(line);
    let mut io = PendingIo {
        say: &mut say,
        // A copy to run meanwhile runs instead of any wait.
        wait: onward.is_none().then_some(show),
        asker,
    };
    // Anchor freshness before deciding to wait: a terminal snapshot already here
    // is not the successor a live promise says is coming.
    let observed_at = crate::flow::now_unix().max(0).unsigned_abs();
    let initial = crate::progress::read_progress(layout);
    let initial_running = initial
        .as_ref()
        .is_some_and(|file| crate::progress::snapshot_running(file, observed_at));
    let next = pending_state(layout, tool, &mut io);
    // The lines go out once the decision is made: on STDERR for a copy that runs
    // meanwhile and for an unattended wait — the program's own stdout is the caller's (a
    // pipe, a script, an agent reading `targo metadata`), and the state is a note.
    let waits = matches!(next, PendingNext::WaitThenRun | PendingNext::WaitForPromise);
    let to_stderr = onward.is_some() || (waits && show == WaitShow::Lines);
    for line in lines {
        if to_stderr {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
    if waits {
        return wait_then_run(
            layout,
            tool,
            passthrough,
            show,
            asker,
            WaitSeen {
                snapshot: initial.as_ref(),
                running: initial_running,
                promise: next == PendingNext::WaitForPromise,
            },
        );
    }
    if let Some(exe) = onward {
        eprintln!(
            "atpkg: running {} meanwhile — the managed {tool} takes over once it is installed",
            exe.display()
        );
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            #[expect(
                clippy::disallowed_types,
                reason = "a pending stub becomes the copy it names by exec: its caller's descriptors are the copy's"
            )]
            let err = std::process::Command::new(&exe).args(passthrough).exec();
            eprintln!("atpkg: could not run {}: {err}", exe.display());
            return ExitCode::from(126);
        }
        #[cfg(not(unix))]
        {
            // No `exec(2)` to hand the process over with: the stub contract's 127
            // stands, and the line above told the user which copy to run.
            let _ = passthrough;
            return ExitCode::from(127);
        }
    }
    ExitCode::from(127)
}

/// What a waiting stub does after one look ([`wait_then_run`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitStep {
    /// The program is installed: run it.
    Run,
    /// The pass recorded its failure, or had ended with no shim laid: say so.
    Fail,
    /// The pass is still at it: look again.
    Wait,
}

/// One look's decision, over what it read IN THIS ORDER: whether the pass was `running`
/// (its `progress.json`), then whether the program's shim resolves (`installed`), with
/// whether the pass said it will not bring it (`failed`: a recorded failure, a plan that
/// left the program out, or a done row for a program that laid no shim of the name). The
/// order is the contract the derived model `AtpkgPendingWait` states: a pass lays the shim
/// before it ends, and before the program's row reads done, so a shim read after an ended
/// pass or a done row is final — read the other way round, a pass that sets the program up
/// and ends between the two reads was "stopped" over an installed program.
/// Pure, and Tier-1 bound to that model in the tests.
fn pending_wait_step(running: bool, installed: bool, failed: bool) -> WaitStep {
    if installed {
        WaitStep::Run
    } else if failed || !running {
        WaitStep::Fail
    } else {
        WaitStep::Wait
    }
}

/// The snapshot whose rows belong to a pending command's wait: a running pass's, or an
/// ended one that succeeds `initial`, the witness read before admission (advanced to each
/// pass the wait has seen live).
fn pending_wait_file<'a>(
    snapshot: Option<&'a crate::progress::ProgressFile>,
    initial: Option<&crate::progress::ProgressFile>,
    running: bool,
    initial_running: bool,
) -> Option<&'a crate::progress::ProgressFile> {
    fn generation(file: &crate::progress::ProgressFile) -> Option<&str> {
        file.generation.as_deref().filter(|id| {
            id.len() == 32
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    }
    snapshot.filter(|file| {
        if running {
            return true;
        }
        if file.ended_unix.is_none() {
            return false;
        }
        match generation(file) {
            Some(current) => {
                initial.is_none_or(|first| generation(first) != Some(current) || initial_running)
            }
            // An untagged ended file has no proof of succession. Only a pass
            // actually observed live before admission can own its terminal row.
            None => initial.is_some_and(|first| {
                generation(first).is_none()
                    && initial_running
                    && first.pass == file.pass
                    && first.started_unix == file.started_unix
            }),
        }
    })
}

/// How often a waiting stub looks for its program ([`wait_then_run`]).
const PENDING_WAIT_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// How long an unattended caller ([`WaitShow::Lines`]) waits for its program: a whole first
/// install of the toolset fits. A hang detector for a stalled pass, not a latency budget.
const PENDING_UNATTENDED_WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// How often an unattended wait says where its program is ([`WaitShow::Lines`]).
const PENDING_UNATTENDED_SAY_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// The line an unattended wait says every [`PENDING_UNATTENDED_SAY_EVERY`]: what was typed,
/// the program it waits for, and how far it is (the downloaded share while the size is
/// known, else its phase).
fn unattended_wait_line(
    typed: &str,
    program: &str,
    row: Option<&crate::progress::ProgramProgress>,
) -> String {
    use crate::progress::Phase;
    let how = match row {
        Some(r) if r.phase == Phase::Download && r.bytes_total > 0 => format!(
            " {}%",
            r.bytes_done.saturating_mul(100) / r.bytes_total.max(1)
        ),
        Some(r) if r.phase == Phase::Queued => ", queued".to_string(),
        Some(r) if matches!(r.phase, Phase::Verify | Phase::Extract | Phase::Link) => {
            ", setting up".to_string()
        }
        _ => String::new(),
    };
    format!("atpkg: {typed} is being installed ({program}{how})")
}

/// What the caller of [`wait_then_run`] read before it chose to wait.
#[derive(Clone, Copy)]
struct WaitSeen<'a> {
    /// The progress snapshot read before [`pending_state`]: a terminal one already here
    /// is not the successor a live promise says is coming.
    snapshot: Option<&'a crate::progress::ProgressFile>,
    /// Whether that snapshot's pass was running.
    running: bool,
    /// The caller saw the promise ([`PendingNext::WaitForPromise`]), so its retirement
    /// counts even when it came before the wait's first read.
    promise: bool,
}

/// The wait a caller gets when the program they typed is on its way
/// ([`PendingNext::WaitThenRun`]): the moment its shim resolves, the program is `exec`ed in
/// this process's place with the arguments they typed. A person at a terminal
/// ([`WaitShow::Meter`]) watches the meter follow the running pass for this program; Ctrl-C
/// stops the wait (the meter's line is cleared; 130 to the shell). An unattended caller
/// ([`WaitShow::Lines`]) gets a stderr line every few seconds and gives up after
/// [`PENDING_UNATTENDED_WAIT_CAP`], exit 127, saying so. A failure the pass records, or a
/// pass that ends without installing it, is said in one line and exits 1. A pass only
/// promised so far ([`crate::progress::pass_owed`]) is waited for as one in flight; when no
/// pass of the wait planned the program — the promise broke first, or its pass stopped
/// before planning — it is answered as nothing installing ([`pending_not_running`]), when
/// the promised pass planned without it, as no longer in the default set, and when its
/// program landed without the name, as a command it does not provide
/// ([`landed_without_line`]); all three exit 127, the last two without waiting the pass
/// out.
///
/// `seen` is what the caller read before it chose to wait ([`WaitSeen`]).
///
/// `tool` is what was typed; a command a program exposes (`targo`) waits on its program's
/// progress row (`trust`, [`crate::stub::program_of_in`]) and runs once its own shim lands.
fn wait_then_run(
    layout: &crate::store::Layout,
    tool: &str,
    passthrough: &[String],
    show: WaitShow,
    asker: crate::progress::Asker,
    seen: WaitSeen<'_>,
) -> ExitCode {
    let program = crate::stub::program_of_in(layout, tool);
    let program = program.as_str();
    // Register before the first progress -> shim read. A rename between that
    // read and the wait is then queued by kqueue, not lost until the fallback.
    let mut shim_wake = crate::pending_wake::PendingShimWake::new(layout);
    let waiting = format!("Waiting for {tool}\u{2026}");
    let meter = (show == WaitShow::Meter).then(|| {
        let layout = layout.clone();
        let focus = program.to_string();
        crate::meter::TtyMeter::start(move || {
            let now = crate::flow::now_unix().max(0).unsigned_abs();
            match crate::progress::read_progress(&layout)
                .filter(|f| crate::progress::snapshot_running(f, now))
            {
                Some(file) => crate::meter::Show::Pass {
                    file,
                    focus: Some(focus.clone()),
                    idle: waiting.clone(),
                },
                None => crate::meter::Show::Busy(waiting.clone()),
            }
        })
    });
    let started = std::time::Instant::now();
    let mut said = started;
    // Whether a pass of this wait ever planned the program: if none did (a promise broken
    // before any pass began, a pass that stopped before it planned), the answer is nothing
    // installing ([`pending_not_running`]), not an install that stopped.
    let mut planned = false;
    // Whether this wait (or its caller) has seen the promise: its retirement then says the
    // promised pass planned (or ended), so a live default-set pass with no row for the
    // program is a plan that left it out.
    let mut promised = seen.promise;
    let mut witness = seen.snapshot.cloned();
    let mut witness_running = seen.running;
    let outcome = loop {
        // The promise, then the pass, then the shim ([`pending_wait_step`]): an update retires
        // the promise only once its pass planned or ended, and a pass lays a program's shim
        // before it ends, so a promise read as gone was kept or broken for good, a live pass
        // read after it has planned, and a pass read as ended has laid every shim it will.
        let now = crate::flow::now_unix().max(0).unsigned_abs();
        let owed = crate::progress::pass_owed(layout, now);
        let snapshot = crate::progress::read_progress(layout);
        let running = snapshot
            .as_ref()
            .is_some_and(|f| crate::progress::snapshot_running(f, now));
        // A live pass or a successor of the pre-admission observation owns its
        // rows. Equal-second timestamps cannot turn an old failure into this one's.
        let row = pending_wait_file(
            snapshot.as_ref(),
            witness.as_ref(),
            running,
            witness_running,
        )
        .and_then(|f| f.programs.get(program));
        if running {
            witness = snapshot.clone();
            witness_running = true;
        }
        planned |= row.is_some();
        promised |= owed;
        // A plan that left it out: the promise this wait saw is gone, retired once its pass
        // planned, and that default-set pass runs with no row for the program (a name the
        // signed index does not serve here). It is not coming, so the wait ends now with
        // the answer a stub run now gets ([`pending_state`]'s honesty edge), not at the end
        // of a pass that may run for minutes.
        let left_out = promised
            && !planned
            && !owed
            && running
            && snapshot
                .as_ref()
                .is_some_and(|f| matches!(f.pass.as_str(), "net" | "seed"));
        let failed = row.filter(|r| {
            matches!(
                r.phase,
                crate::progress::Phase::Failed | crate::progress::Phase::Skipped
            )
        });
        // A PROGRAM THAT LANDED WITHOUT THE NAME: its row reads done — written only once every
        // shim its build lays stands — and the shim read after it finds none of the name typed
        // (a command this build does not expose). Nothing brings it, so the wait ends now, not
        // when the pass does.
        let landed_without = row.is_some_and(|r| r.phase == crate::progress::Phase::Done);
        let installed = crate::which(layout, tool).is_some();
        match pending_wait_step(
            owed || running,
            installed,
            failed.is_some() || left_out || landed_without,
        ) {
            WaitStep::Run => break Ok(()),
            WaitStep::Fail if left_out => {
                break Err(Some((
                    not_in_default_set_line(program),
                    ExitCode::from(127),
                )));
            }
            WaitStep::Fail if landed_without => {
                break Err(Some((
                    landed_without_line(layout, program, tool),
                    ExitCode::from(127),
                )));
            }
            WaitStep::Fail if failed.is_none() && !planned => break Err(None),
            WaitStep::Fail => {
                break Err(Some((
                    match failed {
                        Some(row) => format!(
                            "atpkg: {program} did not install: {} \u{2014} `aterm pkg install \
                             {program}` tries again",
                            crate::progress::sanitize_for_tty(
                                row.error.as_deref().unwrap_or("no reason was recorded"),
                                PENDING_ERROR_CAP
                            )
                        ),
                        None => format!(
                            "atpkg: the install stopped before {tool} was ready \u{2014} `aterm \
                             pkg install {program}` tries again"
                        ),
                    },
                    ExitCode::from(1),
                )));
            }
            WaitStep::Wait if show == WaitShow::Lines => {
                // The unattended bound: a stalled pass never holds a tool call for good.
                if started.elapsed() >= PENDING_UNATTENDED_WAIT_CAP {
                    break Err(Some((
                        format!(
                            "atpkg: stopped waiting for {program} after {} minutes \u{2014} it \
                             is still installing; run {tool} again once it lands",
                            PENDING_UNATTENDED_WAIT_CAP.as_secs() / 60
                        ),
                        ExitCode::from(127),
                    )));
                }
                if said.elapsed() >= PENDING_UNATTENDED_SAY_EVERY {
                    eprintln!("{}", unattended_wait_line(tool, program, row));
                    said = std::time::Instant::now();
                }
                let _ = shim_wake.wait(PENDING_WAIT_POLL);
            }
            WaitStep::Wait => {
                let _ = shim_wake.wait(PENDING_WAIT_POLL);
            }
        }
    };
    shim_wake.retire();
    drop(meter);
    match outcome {
        Ok(()) => {}
        Err(Some((line, exit))) => {
            eprintln!("{line}");
            return exit;
        }
        // No pass of this wait planned the program — the window went, the update ended
        // before it planned (offline, nothing to do), the deadline passed: the command did
        // not run, and nothing is installing it.
        Err(None) => {
            if let Some(program) = crate::store::ToolName::new(program) {
                pending_not_running(layout, &program, asker, &mut |line| eprintln!("{line}"));
            }
            return ExitCode::from(127);
        }
    }
    // It landed: run it through `atpkg run <tool>` ([`cmd_run`]), so it gets every rule that
    // door keeps (the exec root, `ops::exec_env`, the self-update plan). The explicit `--`
    // is the one separator `cmd_run` drops, so the tool's own arguments reach it untouched.
    // A name still laid as a pending stub is answered here: handing it on would land back
    // in this wait.
    if crate::stub::pending_stub_exists(layout, tool) {
        eprintln!("{}", not_installed_fix(program));
        return ExitCode::from(127);
    }
    let mut rest = Vec::with_capacity(passthrough.len() + 2);
    rest.push(tool.to_string());
    rest.push("--".to_string());
    rest.extend_from_slice(passthrough);
    cmd_run(&rest)
}

/// What a wait for `tool` says once its `program` landed with no command of that name
/// ([`wait_then_run`]): the program is installed, the name is not one of its commands, and
/// the commands it does provide.
fn landed_without_line(layout: &crate::store::Layout, program: &str, tool: &str) -> String {
    let mut provides = crate::installed_exposes(layout, program).unwrap_or_default();
    provides.sort();
    let mut line = format!("atpkg: {program} is installed, and it has no command named {tool}");
    if !provides.is_empty() {
        line.push_str(&format!(" \u{2014} it provides {}", provides.join(", ")));
    }
    line
}

/// How long an echoed error line may get before visible elision — generous enough
/// for every honest FlowError, finite against a hostile record.
const PENDING_ERROR_CAP: usize = 300;

/// How much of a held row's failure (`why`) the pending stub echoes: the longest digest-pair
/// sentence a memo records whole (166 characters for `asset sha256 mismatch: …`); a longer
/// signer sentence is elided.
const HELD_WHY_CAP: usize = 200;

/// …and how much of its retry (`how`). Whole for every row atpkg writes — at most about
/// 240 characters of its own words plus a signed asset name and a program name
/// (`flow::digest_refusal_note`) — and still finite against a hostile record.
const HELD_HOW_CAP: usize = 2 * PENDING_ERROR_CAP;

/// The pending stub's line for a member held after a failed stage (`held` is the row after
/// [`crate::state::HELD_FAILED_PREFIX`]: `<why>; <how>`). The halves are capped apart so
/// the retry survives a long digest sentence; the first `; ` is the seam by construction
/// ([`crate::state::held_failed`]).
fn held_stub_line(name: &str, held: &str) -> String {
    let (why, how) = held.split_once("; ").unwrap_or((held, ""));
    let mut line = format!(
        "atpkg: {name} is {}{}",
        crate::state::HELD_FAILED_PREFIX,
        crate::progress::sanitize_for_tty(why, HELD_WHY_CAP)
    );
    if !how.is_empty() {
        line.push_str("; ");
        line.push_str(&crate::progress::sanitize_for_tty(how, HELD_HOW_CAP));
    }
    line.push_str(" — Settings ▸ Packages shows details.");
    line
}

/// The `__pending` state machine's I/O seam: `say` receives each line in order (production
/// prints it; tests collect it).
struct PendingIo<'a> {
    say: &'a mut dyn FnMut(String),
    /// Whether, and how, the caller can wait for a program a running pass is installing
    /// and have it run ([`PendingNext::WaitThenRun`]). `None` when a copy of the program
    /// runs meanwhile instead ([`pending_passthrough`]).
    wait: Option<WaitShow>,
    /// Who the bump is written for: a person at a terminal, whose agent lands at once, or a
    /// script, whose agent goes first among the agents once the toolchain's bytes are in
    /// ([`RiderQueue::next`]).
    asker: crate::progress::Asker,
}

/// How a stub that waits for its program shows the wait ([`wait_then_run`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitShow {
    /// A person at a terminal: the progress meter, for as long as the pass runs.
    Meter,
    /// A script, a pipe or an agent's tool call: a line on stderr every
    /// [`PENDING_UNATTENDED_SAY_EVERY`], for at most [`PENDING_UNATTENDED_WAIT_CAP`].
    Lines,
}

/// What the caller of [`pending_state`] does after the lines: nothing more, or wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingNext {
    Done,
    /// A pass is installing the program, or is promised: wait ([`WaitShow`]), then run it
    /// with the arguments typed ([`wait_then_run`]).
    WaitThenRun,
    /// A pass is promised and has not planned yet: the same wait, which starts knowing the
    /// promise was seen, so an update that retires it before the wait's first read still
    /// ends the wait at once when its plan left the program out.
    WaitForPromise,
}

/// The pending stub's honest states, one function for the three dead ends. Side-effectful
/// on purpose: a claimed bump must really have been written, so the tests exercise message
/// and bump together.
fn pending_state(layout: &crate::store::Layout, tool: &str, io: &mut PendingIo<'_>) -> PendingNext {
    // The name gate first: only an admitted ToolName is ever echoed back to the TTY
    // (an unadmitted one could never have a stub or a shim, so nothing pends).
    let Some(tn) = crate::store::ToolName::new(tool) else {
        (io.say)("atpkg: that is not an installable program name".to_string());
        return PendingNext::Done;
    };
    let typed = tn.as_str();
    // A COMMAND A PROGRAM EXPOSES (`targo`) answers with its PROGRAM's state (`trust`,
    // [`crate::stub::program_of_in`]): the progress row, the status row and the bump are all
    // the program's. Every line below names the program; this one says how they relate.
    let program = crate::store::ToolName::new(&crate::stub::program_of_in(layout, typed))
        .unwrap_or_else(|| tn.clone());
    let name = program.as_str();
    let what = crate::stub::describe(name).unwrap_or("part of the ALab toolset");
    if typed == name {
        (io.say)(format!("atpkg: {name}: {what} — not installed yet."));
    } else {
        (io.say)(format!(
            "atpkg: {typed} comes with {name}: {what} — not installed yet."
        ));
    }
    let now = crate::flow::now_unix().max(0).unsigned_abs();
    // The promise, then the pass: the order [`wait_then_run`] reads them in.
    let owed = crate::progress::pass_owed(layout, now);
    let snapshot = crate::progress::read_progress(layout);
    // A pass installing something else (a typed `update`, another program's `install`)
    // plans no default set, so its silence about this name is no delisting; nor is a
    // promised pass's before its plan lands (the promise answers below).
    let running = snapshot.as_ref().is_some_and(|f| {
        crate::progress::snapshot_running(f, now)
            && (f.programs.contains_key(name)
                || (!owed && matches!(f.pass.as_str(), "net" | "seed")))
    });
    if let Some(file) = snapshot.as_ref().filter(|_| running) {
        if file.v != crate::progress::PROGRESS_VERSION {
            // A newer writer's file: the generic line, never a guess at fields that may
            // have changed. The bump file is versionless, so the bump stays honest.
            (io.say)(format!(
                "atpkg: aterm is installing the toolset now — try {typed} again shortly."
            ));
            let _ = crate::progress::append_bump(layout, &program, io.asker);
            return PendingNext::Done;
        }
        let overall = &file.overall;
        (io.say)(format!(
            "atpkg: aterm is installing your toolchain now ({} of {} done).",
            overall.programs_done, overall.programs_total
        ));
        let Some(row) = file.programs.get(name) else {
            // The honesty edge: a readable running-pass snapshot that does not plan this
            // program — a compiled-roster stub the signed index no longer lists. Claiming it
            // is next would be a lie the reorder-only bump silently swallows; the reconcile
            // removes this stub at pass end.
            (io.say)(not_in_default_set_line(name));
            return PendingNext::Done;
        };
        use crate::progress::Phase;
        // A caller who can wait waits for it ([`WaitShow`]), and the program runs the moment
        // it lands — bumped first when not yet started, so the wait is short.
        if let Some(wait) = io.wait
            && matches!(
                row.phase,
                Phase::Queued
                    | Phase::Download
                    | Phase::Verify
                    | Phase::Extract
                    | Phase::Link
                    | Phase::Done
            )
        {
            // Not started yet, as far as the pass goes: still queued, or its archive fetched
            // ahead and waiting for the pass to reach it ([`ProgramProgress::downloaded`]).
            if row.phase == Phase::Queued || row.downloaded() {
                let _ = crate::progress::append_bump(layout, &program, io.asker);
            }
            (io.say)(on_its_way_line(typed, name, wait));
            return PendingNext::WaitThenRun;
        }
        match row.phase {
            Phase::Download => {
                if row.bytes_total > 0 {
                    (io.say)(format!(
                        "atpkg: {name} is being installed ({}%, {:.1} of {:.1} MB) \u{2014} \
                         try again in a minute.",
                        row.bytes_done.saturating_mul(100) / row.bytes_total.max(1),
                        row.bytes_done as f64 / 1e6,
                        row.bytes_total as f64 / 1e6,
                    ));
                } else {
                    (io.say)(format!(
                        "atpkg: {name} is being installed \u{2014} try again in a minute."
                    ));
                }
            }
            Phase::Verify | Phase::Extract | Phase::Link => {
                (io.say)(format!(
                    "atpkg: {name} is almost ready \u{2014} try again in a moment."
                ));
            }
            Phase::Done => {
                (io.say)(format!(
                    "atpkg: {name} just finished installing \u{2014} run it again now."
                ));
            }
            Phase::Failed => {
                let why = row.error.as_deref().unwrap_or("no recorded reason");
                (io.say)(format!(
                    "atpkg: {name} FAILED to install this pass: {} — fix: aterm pkg \
                     update retries it; Settings ▸ Packages shows details.",
                    crate::progress::sanitize_for_tty(why, PENDING_ERROR_CAP)
                ));
            }
            Phase::Queued => {
                let position = file.queue.iter().position(|q| q == name);
                let current = file.queue.first().filter(|c| c.as_str() != name);
                let bumped = crate::progress::append_bump(layout, &program, io.asker).is_ok();
                match (position, current, bumped) {
                    (Some(p), Some(cur), true) => {
                        // `cur` came off disk: it is echoed only after its own
                        // ToolName round-trip (queue names are untrusted).
                        let cur = crate::store::ToolName::new(cur).map_or_else(
                            || "the current program".to_string(),
                            |t| t.as_str().to_string(),
                        );
                        (io.say)(format!(
                            "atpkg: {name} is next in line (it was {} of {}), after {cur} \
                             \u{2014} try again in a minute.",
                            p + 1,
                            file.queue.len()
                        ));
                    }
                    (_, _, true) => {
                        (io.say)(format!(
                            "atpkg: {name} is next in line \u{2014} try again in a minute."
                        ));
                    }
                    _ => {
                        (io.say)(format!(
                            "atpkg: {name} is waiting its turn \u{2014} try again once the \
                             install reaches it."
                        ));
                    }
                }
            }
            Phase::Skipped => {
                // Skipped is terminal, not a wait: [`crate::progress::Sink::finished`]
                // retires a skipped program from the queue, and the bump file only reorders
                // what is still queued. Say the recorded reason and claim no bump.
                match row.error.as_deref() {
                    Some(why) => (io.say)(format!(
                        "atpkg: {name} was SKIPPED by this pass: {} — `aterm pkg install \
                         {name}` tries it alone",
                        crate::progress::sanitize_for_tty(why, PENDING_ERROR_CAP)
                    )),
                    None => (io.say)(format!(
                        "atpkg: {name} was SKIPPED by this pass — already current, or not in \
                         this pass. Re-run it; if it still does not run: aterm pkg install {name}"
                    )),
                }
            }
        }
        return PendingNext::Done;
    }
    // A pass is promised and has not planned yet ([`crate::progress::pass_owed`]): the
    // window's first-run seed, or an update about to plan the set. A caller who can wait
    // waits for it as for a pass in flight, bumped first so its program goes first.
    if owed {
        let _ = crate::progress::append_bump(layout, &program, io.asker);
        (io.say)("atpkg: aterm is starting your toolchain install now.".to_string());
        match io.wait {
            Some(wait) => {
                (io.say)(on_its_way_line(typed, name, wait));
                return PendingNext::WaitForPromise;
            }
            None => {
                (io.say)(format!(
                    "atpkg: {name} is requested first \u{2014} try {typed} again in a minute."
                ));
                return PendingNext::Done;
            }
        }
    }
    pending_not_running(layout, &program, io.asker, &mut *io.say);
    PendingNext::Done
}

/// The honesty edge's line: a running default-set pass's plan leaves `name` out
/// ([`pending_state`]; [`wait_then_run`] when the promised pass planned without it).
fn not_in_default_set_line(name: &str) -> String {
    format!(
        "atpkg: {name} is no longer part of the default set — to install it: aterm pkg \
         install {name}"
    )
}

/// The line a caller who waits for its program gets ([`PendingNext::WaitThenRun`]): what
/// it waits for, and how the wait ends.
fn on_its_way_line(typed: &str, name: &str, wait: WaitShow) -> String {
    let it = if typed == name { "it" } else { typed };
    let stop = match wait {
        WaitShow::Meter => "Ctrl-C to stop".to_string(),
        WaitShow::Lines => format!(
            "waiting at most {} minutes",
            PENDING_UNATTENDED_WAIT_CAP.as_secs() / 60
        ),
    };
    format!("atpkg: {name} is on its way \u{2014} {it} starts as soon as it is installed ({stop}).")
}

/// [`pending_state`]'s answer when no pass is installing `program` or promised: the
/// recorded failure or the reason nothing is coming, else the bump and what it means.
fn pending_not_running(
    layout: &crate::store::Layout,
    program: &crate::store::ToolName,
    asker: crate::progress::Asker,
    say: &mut dyn FnMut(String),
) {
    let name = program.as_str();
    // Not running (no file, stale heartbeat, dead pid, or a terminal snapshot): only
    // not-running states may render, whatever the snapshot claims. A recorded failure
    // outranks the generic line — every failure names its next act.
    if let Some(status) = crate::status::read(layout) {
        // A member held after a failed stage: the row names the failure and the retry, so
        // say it and claim nothing — the next pass would only repeat the hold, so no bump.
        // Before the `error` arm, which would promise a retry.
        if let Some(row) = status.programs.get(name)
            && let Some(held) = row.state.strip_prefix(crate::state::HELD_FAILED_PREFIX)
        {
            say(held_stub_line(name, held));
            return;
        }
        if let Some(row) = status.programs.get(name)
            && row.state.starts_with("error")
        {
            say(format!(
                "atpkg: the last install attempt FAILED: {} — fix: aterm pkg update \
                 retries it; Settings ▸ Packages shows details.",
                crate::progress::sanitize_for_tty(&row.state, PENDING_ERROR_CAP)
            ));
            let _ = crate::progress::append_bump(layout, program, asker);
            return;
        }
        // A member the pinned build does not serve on this target (`unavailable on …`) is
        // not coming on its own: say the row's words, and do not bump.
        if let Some(row) = status.programs.get(name)
            && row.state.starts_with(crate::state::UNAVAILABLE_PREFIX)
        {
            say(format!(
                "atpkg: {name} is not coming on its own: {} — Settings ▸ Packages \
                 shows details.",
                crate::progress::sanitize_for_tty(&row.state, PENDING_ERROR_CAP)
            ));
            return;
        }
        // A blocked toolset outranks the generic promise: the `*toolset*` row lets a stub
        // that outlives the pass say the registry publishes no build for this machine.
        if let Some(row) = status.programs.get("*toolset*")
            && let Some(why) = row.state.strip_prefix("blocked: ")
        {
            say(format!(
                "atpkg: {name} is not coming on its own: {} — Settings ▸ Packages \
                 shows details.",
                crate::progress::sanitize_for_tty(why, PENDING_ERROR_CAP)
            ));
            // Still worth a bump: a triple the index has since learned to serve gets an
            // immediate pass from the GUI's bump watch.
            let _ = crate::progress::append_bump(layout, program, asker);
            return;
        }
    }
    // Bump first, then say what that means: an open aterm window checks the bump file
    // every 5 s and turns a fresh bump into a pass, so the sentence depends on whether the
    // bump was recorded.
    let bumped = crate::progress::append_bump(layout, program, asker).is_ok();
    say(if bumped {
        format!(
            "atpkg: nothing is installing right now — {name} is requested; an open aterm \
             window starts a pass within seconds (at most one every 5 minutes). \
             Otherwise: open aterm, or run: aterm pkg update"
        )
    } else {
        "atpkg: nothing is installing right now — fix: open aterm (it provisions the \
         toolset on launch), or run: aterm pkg update"
            .to_string()
    });
}

/// After an [`crate::FlowError::Unreachable`] failure line, name the one act that resumes
/// it (nothing retries a foreground verb): the caller's re-run, and when the anonymous
/// GitHub budget ran out, when it resets.
fn print_unreachable_followup(e: &crate::FlowError, rerun: &str) {
    let (crate::FlowError::Unreachable(why) | crate::FlowError::VendorUnreachable(why)) = e else {
        return;
    };
    eprintln!("atpkg:   when the connection is back, re-run: {rerun}");
    if why.contains("403") || why.to_ascii_lowercase().contains("rate limit") {
        eprintln!("atpkg:   the anonymous GitHub limit resets within the hour");
    }
}

/// Resolve the install layout through the chain-validated `[packages].prefix` override
/// ([`crate::store::resolve_configured`]; `docs/GOLDEN-INSTALL-PATH.md` §2), or print why
/// not and return `None`.
fn layout() -> Option<crate::store::Layout> {
    match crate::store::resolve_configured() {
        Some(l) => Some(l),
        None => {
            eprintln!("atpkg: HOME is unset — cannot locate the install prefix");
            None
        }
    }
}

/// The doctor's arguments, each at most once: `--verbose` (or `-v`), the detail asked for,
/// and `--json`, the rows in the one report schema instead of the text. Anything else is
/// the usage refusal (exit 2) — the swallowed-argument rule [`zero_arity`] keeps.
fn doctor_args(rest: &[String], verb: &str) -> Result<(crate::doctor::Detail, bool), ExitCode> {
    let mut detail = None;
    let mut json = false;
    for arg in rest {
        match arg.as_str() {
            "--verbose" | "-v" if detail.is_none() => {
                detail = Some(crate::doctor::Detail::Everything);
            }
            "--json" if !json => json = true,
            _ => {
                let problem = format!(
                    "unknown or repeated argument {arg:?} — it takes --verbose and --json, \
                     each once"
                );
                return Err(usage_error(verb, Some(&problem)));
            }
        }
    }
    Ok((detail.unwrap_or(crate::doctor::Detail::Problems), json))
}

/// Refuse any argument to a verb that takes none — `Some(usage error)` when one was given,
/// `None` to proceed — so a swallowed flag never reads as accepted. `-h`/`--help` never
/// reaches here.
fn zero_arity(rest: &[String], verb: &str) -> Option<ExitCode> {
    let stray = rest.first()?;
    let problem = format!("unknown argument {stray:?} — this verb takes none");
    Some(usage_error(verb, Some(&problem)))
}

/// `update <program> --vendor`: a person's choice of the vendor's build of an ALab-built
/// program on this machine ([`crate::alab::choose_vendor`]) — the one way back to the vendor
/// from ALab's channel, and never taken by anything unattended.
pub(crate) const SOURCE_VENDOR_FLAG: &str = "--vendor";

/// `update <program> --alab`: forget that choice; the program follows ALab's channel again
/// where it publishes one ([`crate::alab::clear_vendor_choice`]).
pub(crate) const SOURCE_ALAB_FLAG: &str = "--alab";

/// The refusal [`Verb::arity`] makes of `rest` (the argv after `verb`), or `None` to
/// proceed. [`Arity::None`]: any argument ([`zero_arity`]). [`Arity::One`]: a second
/// operand, the verb's flags not counted — the arm reads one name, so the rest would be
/// dropped silently. Runs at the dispatch edge, after the machine lanes' own flags are
/// stripped and before the store lock, so NOTHING of the command runs: acting on the first
/// name and refusing the rest would leave the reader to work out which half happened.
fn arity_refusal(rest: &[String], verb: &str) -> Option<ExitCode> {
    match verb_row(verb)?.arity {
        Arity::None => zero_arity(rest, verb),
        Arity::One { flags } => {
            let operands = rest.iter().filter(|a| !flags.contains(&a.as_str())).count();
            // The usage line printed under it carries the grammar.
            (operands >= 2)
                .then(|| usage_error(verb, Some("one name at a time \u{2014} nothing was done")))
        }
        Arity::Flags { flags } => rest.iter().enumerate().find_map(|(at, arg)| {
            (!flags.contains(&arg.as_str()) || rest[..at].contains(arg)).then(|| {
                usage_error(
                    verb,
                    Some(&format!(
                        "unknown or repeated argument {arg:?} \u{2014} this verb takes {}, each \
                         once, and nothing else",
                        flags.join(", ")
                    )),
                )
            })
        }),
        Arity::Own => None,
    }
}

/// What `verb` answers when `rest` (the argv after it) names one program built into aterm
/// ([`crate::builtin::verb_answer`]): its line on stdout and exit 0, or its refusal on
/// stderr and exit 1. `None` to run the verb. The verb's own flags are not names, as
/// [`arity_refusal`] counts them, so `update amacs --retry` is answered as `update amacs`.
fn built_in_answer(verb: &str, rest: &[String]) -> Option<ExitCode> {
    let flags = match verb_row(verb)?.arity {
        Arity::One { flags } | Arity::Flags { flags } => flags,
        Arity::None | Arity::Own => &[],
    };
    let answer =
        crate::builtin::verb_answer(verb, rest, flags, aterm_types::version::app_version())?;
    Some(if answer.refused {
        eprintln!("{}", answer.line);
        ExitCode::from(1)
    } else {
        println!("{}", answer.line);
        ExitCode::SUCCESS
    })
}

/// Whether `verb` MUTATES the store and must hold the store-wide single-writer lock
/// ([`Verb::mutates_store`]); an unknown word (or `help`) never does.
fn verb_mutates_store(verb: &str) -> bool {
    verb_row(verb).is_some_and(|v| v.mutates_store)
}

/// Acquire the store-wide writer lock for a mutating verb at the dispatch edge: a try by
/// default, or with `--wait-lock <secs>` a bounded wait ([`crate::lock::lock_store_waiting`])
/// that announces itself once as the [`LOCK_WAITING_MARKER`] line (its one emission site).
/// `Ok(None)` when no prefix resolves: the verb refuses with its own message. Contention,
/// fail-fast or timed out, exits [`crate::lock::CONTENDED_EXIT`] (75); an unusable lock
/// file exits 1. A waiter whose window quit ([`parent_is_gone`]) stands down silently with
/// 75 at its next contended poll, and its lines are lossy writes
/// ([`emit_marker_line_lossy`]): `SIGPIPE` is ignored and `println!` panics on a dead pipe.
///
/// A person's verb waits too: typed on a terminal with no `--wait-lock`
/// ([`a_person_typed_this`]), it waits up to [`TYPED_LOCK_WAIT`] — silent for
/// [`crate::lock::WAIT_ANNOUNCE_GRACE`], then under a spinner — and a wait that runs out
/// says so in one sentence and exits 75. Off a terminal, a typed verb refuses at once.
fn mutator_store_lock(
    expected_prefix: Option<&std::path::Path>,
) -> Result<Option<crate::lock::StoreLock>, ExitCode> {
    let Some(layout) = crate::store::resolve_configured() else {
        return Ok(None);
    };
    if let Some(expected) = expected_prefix
        && let Err(why) = crate::linkmode::check_link_prefix(&layout, expected)
    {
        eprintln!("atpkg: {why}");
        return Err(ExitCode::from(1));
    }
    // Whether the wait announced itself: only then is it answered (`lock-acquired:`).
    let announced = std::cell::Cell::new(false);
    // The spinner a person on a terminal watches while the wait lasts, cleared when it
    // ends however it ends.
    let spinner = std::cell::RefCell::new(None::<crate::meter::TtyMeter>);
    let person = wait_lock().is_none() && a_person_typed_this();
    let acquired = match wait_lock() {
        Some(WaitLock { bound, .. }) => crate::lock::lock_store_waiting(
            &layout,
            bound,
            |path| {
                announced.set(true);
                // On a terminal (`claude update`'s child inherits the person's) the
                // spinner says it; the marker keeps its bytes for every pipe.
                let shown = lock_wait_spinner(&layout);
                if shown.is_none() || !stdout_is_a_person() {
                    emit_marker_line_lossy(&format!(
                        "atpkg: {LOCK_WAITING_MARKER}another atpkg process holds the store lock \
                         at {} \u{2014} waiting up to {} s for it to finish",
                        path.display(),
                        bound.as_secs()
                    ));
                }
                *spinner.borrow_mut() = shown;
            },
            || {
                // Asked only on a contended poll: the lock was held by someone
                // when this process wanted it ([`pass_ended_while_we_waited`]).
                WAITED_FOR_LOCK.store(true, std::sync::atomic::Ordering::Relaxed);
                !parent_is_gone()
            },
        ),
        None if person => crate::lock::lock_store_waiting(
            &layout,
            TYPED_LOCK_WAIT,
            |_| {
                let shown = lock_wait_spinner(&layout);
                if shown.is_none() {
                    eprintln!("atpkg: {}", lock_wait_sentence(&layout));
                }
                *spinner.borrow_mut() = shown;
            },
            || true,
        ),
        None => crate::lock::try_lock_store(&layout),
    };
    drop(spinner.take());
    match acquired {
        Ok(guard) => {
            // The wait is over, said once and only after it was announced: the verb that
            // follows may be quiet for its whole run, and the GUI's waiting row has nothing
            // else to retire it before this child exits. Lossy, like the announcement.
            if announced.get() {
                emit_marker_line_lossy(&format!(
                    "atpkg: {LOCK_ACQUIRED_MARKER}the other process finished \u{2014} \
                     running now"
                ));
            }
            Ok(Some(guard))
        }
        Err(e) => {
            // An orphaned waiter has nobody to tell: the exit code says it all. A window can
            // quit after that check, so a waiter's lines are lossy writes (the macros panic
            // on a dead pipe, a 101 for the silent 75); a typed verb's stderr is a terminal.
            if !parent_is_gone() {
                let waited = wait_lock().map(|w| w.bound);
                let contended = matches!(e, crate::lock::StoreLockError::Contended(_));
                match waited {
                    // A `--wait-lock` caller on a person's terminal (`claude update`'s child
                    // inherits it) gets one sentence for the wait that ran out: under the
                    // intercept the parent prints it (`selfupdate::contended_line`), else
                    // the typed lane's with its own bound. Pipes keep the Display.
                    Some(bound) if contended && a_person_typed_this() => {
                        if std::env::var_os(SPAWNER_PID_ENV).is_none() {
                            eprintln!("atpkg: {}", lock_wait_timed_out(&layout, bound));
                        }
                    }
                    Some(_) => {
                        use std::io::Write as _;
                        let _ = writeln!(std::io::stderr(), "atpkg: {e}");
                    }
                    None if person && contended => {
                        eprintln!("atpkg: {}", lock_wait_timed_out(&layout, TYPED_LOCK_WAIT));
                    }
                    None => eprintln!("atpkg: {e}"),
                }
                let waiter = waited.is_some();
                // Contention gets a marker; no other refusal does: an unwritable prefix or a
                // broken lock file is a real outage and must reach the GUI's refusal card,
                // while a lock held by another `atpkg` means that pass is doing this work
                // ([`SEED_BUSY_MARKER`]). Only the `--wait-lock` lanes hear it; a typed verb
                // is told on stderr and keeps its stdout silent.
                if matches!(e, crate::lock::StoreLockError::Contended(_)) && waiter {
                    emit_marker_line_lossy(&format!(
                        "atpkg: {SEED_BUSY_MARKER}another atpkg process holds the store lock \
                         — that pass is doing this work and this one stood aside"
                    ));
                }
            }
            Err(ExitCode::from(match e {
                crate::lock::StoreLockError::Contended(_) => crate::lock::CONTENDED_EXIT,
                crate::lock::StoreLockError::Io(..) => 1,
            }))
        }
    }
}

/// Post-admission housekeeping shared by ordinary passes and an accepted conditional link.
fn begin_store_mutation(guard: &mut crate::lock::StoreLock) {
    let layout = guard.layout();
    crate::landing::sweep(&layout);
    use std::io::IsTerminal as _;
    guard.announce_holder(
        &layout,
        wait_lock().is_none() && std::io::stdin().is_terminal(),
    );
}

/// How long a verb a person typed waits for the store: long enough for a large background
/// update, short enough that a wedged holder ends in a sentence.
const TYPED_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Whether a person typed this verb and is watching it: stdin and stderr are both a
/// terminal. Never in a test binary, which a terminal often runs.
fn a_person_typed_this() -> bool {
    use std::io::IsTerminal as _;
    !cfg!(test) && std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Whether stdout is a person's terminal — where a marker line is shown as a sentence
/// ([`human_marker`]). Never in a test binary; a pipe (every window child) never is.
fn stdout_is_a_person() -> bool {
    use std::io::IsTerminal as _;
    !cfg!(test) && std::io::stdout().is_terminal()
}

/// What a waiting verb is waiting for, in the person's words: another `aterm pkg` typed
/// somewhere (the holder record says a person holds the store), else aterm's own.
fn lock_wait_sentence(layout: &crate::store::Layout) -> &'static str {
    if crate::lock::holder_is_person(layout) {
        "Waiting for another `aterm pkg` command to finish\u{2026} (Ctrl-C to stop)"
    } else {
        "Waiting for aterm's background update to finish\u{2026} (Ctrl-C to stop)"
    }
}

/// The spinner over a wait for the store, when stderr is a person's terminal.
fn lock_wait_spinner(layout: &crate::store::Layout) -> Option<crate::meter::TtyMeter> {
    let text = lock_wait_sentence(layout).to_string();
    crate::meter::TtyMeter::start(move || crate::meter::Show::Busy(text.clone()))
}

/// A wait that ran out, in the person's words, the bound as they read it (`10 min`;
/// [`crate::selfupdate::wait_words`]).
fn lock_wait_timed_out(layout: &crate::store::Layout, bound: std::time::Duration) -> String {
    let after = crate::selfupdate::wait_words(bound.as_secs());
    if crate::lock::holder_is_person(layout) {
        format!(
            "another `aterm pkg` command is still running after {after} \u{2014} try again when \
             it finishes"
        )
    } else {
        format!("aterm's background update is still running after {after} \u{2014} try again later")
    }
}

/// Whether this `--wait-lock` waiter has been orphaned: the window that spawned it (named in
/// [`SPAWNER_PID_ENV`] or read at the edge) is gone ([`is_orphaned`]); always false without
/// a recorded wait. A waiter holds nothing and would otherwise outlive its window and race
/// the successor's waiter for the freed lock; asked only on a contended poll, so that race
/// is narrowed to one poll interval, not closed. An installing child keeps running instead
/// ([`crate::progress::watch_for_orphaning`] re-points its stdio at a log).
pub(crate) fn parent_is_gone() -> bool {
    wait_lock().is_some_and(|w| is_orphaned(w.spawner, current_parent_id()))
}

/// `atpkg` (no verb) — the inert/enabled posture, observable from the shell, answering
/// a first hour's four questions in order: what is this (the head), what do I have (the
/// counts), what can I type (the tiers), and the ONE next command. Read-only and
/// lock-free — `list_installed`/`active_builds` read the store without the writer lock.
///
/// The stable head `atpkg: enabled (root key pinned)` and the byte-exact disabled line
/// are the script-facing contract; the counts ride after an em dash on the same line so
/// nothing keying on the head moves.
fn cmd_bare() {
    if !manager_enabled() {
        println!("atpkg: disabled (no root key pinned) -- inert");
        return;
    }
    // HOME unset: the head still answers (posture is a fact about the binary), with no
    // counts and no next act, which would fail in the same environment.
    let counts = crate::store::resolve_configured().map(|layout| {
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

/// The bare posture's lines for an enabled manager — pure, so the head's stability and
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
            "atpkg: next: aterm pkg install --default-set — the whole ALab toolset · \
             manual: aterm help pkg"
                .to_string(),
        ),
        Some(_) => lines.push(
            "atpkg: next: aterm pkg list — what you have · full manual: aterm help pkg".to_string(),
        ),
        None => {}
    }
    lines
}

/// `atpkg doctor` — the full health surface (§15): trust root, PATH wiring, broken-symlink
/// scan, active-build store integrity, shell.d hooks + fish-safety, disk headroom, index
/// freshness, and rustup state. No network, no mutation. Structural breakage exits
/// nonzero; advisory warnings stay exit-0.
fn doctor(prefix: &str, detail: crate::doctor::Detail, json: bool) -> ExitCode {
    // The doctor prints the `[packages]` table's spelling notes as its own `note` rows
    // (`Probes::config_notes`); the load-time copies would say each one again.
    crate::notice::spelling_reported_here();
    let Some(layout) = layout() else {
        return ExitCode::from(1); // HOME unset is itself structural
    };
    if crate::doctor::run(&layout, prefix, detail, json) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `atpkg which <tool>` — one line: the copy that runs and why, in the canonical state
/// spellings ([`crate::state`]). See [`which_line`].
fn cmd_which(tool: Option<&String>) -> ExitCode {
    let Some(tool) = tool else {
        return usage_error("which", None);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    match which_line(&layout, tool, std::env::var_os("PATH").as_deref()) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("{why}");
            ExitCode::from(1)
        }
    }
}

/// The one-line answer to "which copy of `tool` runs, and why?". Offline: it reads the
/// managed shim, the process `PATH` (`path_var`), the status record and the retirement
/// marker. The first copy on `PATH` wins, so:
///
/// * a managed shim with a foreign copy ahead of it on `PATH` ⇒
///   `<tool> → <foreign> — managed <build> — SHADOWED by <foreign>`;
/// * a managed shim otherwise ⇒
///   `<tool> → <prefix>/bin/<tool> → <store path> — managed <build> — pinned by index <N>`
///   (the index from the recorded row, else the durable floor) — or, for a vendor-direct
///   program, `… — managed 2.1.280 — Anthropic latest` ([`vendor_state_of`]); an ALab
///   build, store-installed or dev-linked, whose record names its upstream adds
///   ` — based on upstream rust-v0.163.0+g<12 hex>` after its state ([`upstream_clause`]);
/// * no shim but a system copy on `PATH` ⇒ `<tool> → <path> — system: <path> — not managed
///   by aterm`, with ` (managed copy retired <date>)` when the retirement marker (or the
///   recorded row) says so;
/// * a pending stub ⇒ named as such;
/// * a recorded `unavailable on` row ⇒ that row's words;
/// * a REROUTED upstream name (`cargo`, `rustfmt`, … — [`crate::reroute::TABLE`]) ⇒ the
///   reroute stub, its state, and the row's policy in the stub's own words
///   ([`reroute_which_line`]), answered before every other arm;
/// * a program BUILT INTO aterm (`amacs`, [`crate::builtin`]) ⇒
///   `<tool> → <dir>/<tool> — built in: aterm <version>`, `<dir>` the running aterm's own,
///   or `<tool> → <copy> — built in: aterm <version> — SHADOWED by <copy>` when another
///   copy is first on `PATH`; answered right after the reroute rows;
/// * an AGENT PROGRAM whose reroute stub answers first and takes the managed twin in this
///   shell ([`crate::reroute::agents_stub_body_sh`]) ⇒
///   `<tool> → <prefix>/reroute/<tool> → <prefix>/agents/<tool> → <store path> — …`: the
///   copy that runs is decided when it runs, whatever this shell's PATH puts ahead;
/// * nothing at all ⇒ `Err` with the one-line fix.
///
/// The agents-stub environment is the process's ([`crate::reroute::StubEnv::of_process`]);
/// [`which_line_in`] takes it injected.
///
/// # Errors
/// The tool is not an admissible name, or nothing on this machine answers to it.
fn which_line(
    layout: &crate::store::Layout,
    tool: &str,
    path_var: Option<&std::ffi::OsStr>,
) -> Result<String, String> {
    which_line_in(
        layout,
        tool,
        path_var,
        crate::reroute::StubEnv::of_process(),
    )
}

/// [`which_line`] under an explicit agents-stub environment `env`.
///
/// # Errors
/// As [`which_line`].
fn which_line_in(
    layout: &crate::store::Layout,
    tool: &str,
    path_var: Option<&std::ffi::OsStr>,
    env: crate::reroute::StubEnv,
) -> Result<String, String> {
    // A rerouted upstream name is answered by the table, ahead of the deny-list gate and
    // the store: inside an aterm session the reroute dir is first on PATH, so the stub is
    // the copy that runs, and the managed bin/ never carries these names — `cargo`/`rustc`
    // are deny-listed, and no index program exposes the other six.
    if let Some(row) = crate::reroute::row_for(tool) {
        return Ok(reroute_which_line(layout, row, path_var));
    }
    // A program BUILT INTO aterm ([`crate::builtin`]) is the argv0 alias beside the
    // running aterm, whatever the store holds: no index serves it and no shim names it,
    // so every arm below would misreport it (a `system:` copy "not managed by aterm").
    if let Some(line) = crate::builtin::which_line(tool, path_var) {
        return Ok(line);
    }
    let Some(tn) = crate::store::ToolName::new(tool) else {
        return Err(format!("atpkg: {tool:?} is not a tool name"));
    };
    let status = crate::status::read(layout);
    let row_of = |program: &str| {
        status
            .as_ref()
            .and_then(|s| s.programs.get(program).cloned())
    };
    let shim = layout.shim(&tn);
    // 1. A managed shim (a real one — a pending stub does not resolve).
    if let Some(target) = crate::which(layout, tool) {
        let Some((program, build)) = crate::ops::program_build_of_target(&target) else {
            return Ok(format!(
                "{tool} → {} → {} — dev-linked (managed from that checkout, not the index){}",
                shim.display(),
                target.display(),
                upstream_clause(tool, &target)
            ));
        };
        // An agent program's reroute stub first on this PATH decides at exec time: the
        // probe goes through it ([`shadow_through`]), and a stub that takes a current twin
        // is what the line names as running.
        let answer = agent_stub_on_path(layout, tool, path_var, env);
        let routed = match &answer {
            AgentStub::Routes { stub } if agent_twin_current(layout, &program, build) => {
                Some(stub.clone())
            }
            AgentStub::Routes { .. } | AgentStub::PassesThrough { .. } | AgentStub::Absent => None,
        };
        if let Some(shadow) = shadow_through(layout, tool, path_var, &answer) {
            // Outside aterm, a stub that passes through to the user's own copy is the
            // design, said as such ([`agent_passed_row`]).
            if let Some(row) = agent_passed_row(layout, &program, build, &shadow, path_var, env) {
                return Ok(format!("{tool} → {} — {row}", shadow.display()));
            }
            // An agent program whose twin is laid: the foreign copy runs in this shell
            // only, and the remedy is in place. Its copies line reads `on this shell's
            // PATH`, not `out-ranked`.
            if let Some(row) = agent_shell_shadow(layout, &program, build, &shadow, path_var) {
                let mut line = format!("{tool} → {} — {row}", shadow.display());
                let foreign = crate::vendor::foreign_copies_on_path(&layout.prefix, tool, path_var);
                if !foreign.is_empty() {
                    line.push('\n');
                    line.push_str(&foreign_copies_line_with(FOREIGN_ON_THIS_SHELL, &foreign));
                }
                return Ok(line);
            }
            // The canonical state, then the alias fix-line as a separate sentence when
            // `alab-<tool>` is laid: the copy that runs, and the name that runs ours.
            let state = crate::state::shadowed(build, &shadow);
            return Ok(match alias_fix(layout, &tn, &program, path_var) {
                Some(fix) => format!("{tool} → {} — {state} — {fix}", shadow.display()),
                None => format!("{tool} → {} — {state}", shadow.display()),
            });
        }
        // The canonical state, then what the managed copy runs with when its shim exports an
        // environment.
        let state = managed_state_of(layout, &program, build, row_of(&program).as_ref());
        // What runs first: a routing agents stub and the current twin it execs; else the
        // current twin where `agents/` is ahead of `bin/` (it carries the `update`
        // hand-over); else the managed shim.
        let runs = match &routed {
            Some(stub) => format!("{} → {}", stub.display(), layout.agent_shim(&tn).display()),
            None if matches!(answer, AgentStub::Absent)
                && crate::stub::is_agent_program(&program)
                && agent_twin_current(layout, &program, build)
                && agents_dir_ahead_of_bin(layout, path_var) =>
            {
                layout.agent_shim(&tn).display().to_string()
            }
            None => shim.display().to_string(),
        };
        // …and, for an ALab build whose record names it, the upstream it is based on.
        let based = upstream_clause(&program, &target);
        let mut line = match shim_env_fix(&shim, &program) {
            Some(fix) => format!(
                "{tool} → {runs} → {} — {state}{based} — {fix}",
                target.display()
            ),
            None => format!("{tool} → {runs} → {} — {state}{based}", target.display()),
        };
        // …and, for a rostered agent program ([`crate::selfupdate`]), what the vendor's own
        // self-update verb does on this name (`` `claude update` here runs `aterm pkg update
        // claude` ``). Only where "here" is true: the laid twin is current and carries the
        // hand-over ([`agent_twin_intercepts`]; the `.cmd` twin never does) and runs first in
        // this shell. Otherwise withheld: an under-claim, never a false one.
        if let Some(row) = crate::selfupdate::row_for(&program)
            && agent_twin_intercepts(layout, &program, build)
            // `agents/` on PATH decides only where no stub of ours answers first; a stub
            // that passes through runs `bin/`.
            && (routed.is_some()
                || matches!(answer, AgentStub::Absent)
                    && crate::vendor::agents_dir_on_path(&layout.prefix, path_var))
        {
            line.push_str(" — ");
            line.push_str(&crate::selfupdate::which_sentence(row));
        }
        // An agent program's second line: the foreign copies `agents/` out-ranks (a
        // vendor's native install, a brew cask), so nobody needs `which -a` to learn what a
        // `brew upgrade` or the vendor's updater still moves underneath.
        if crate::stub::is_agent_program(&program) {
            let foreign = crate::vendor::foreign_copies_on_path(&layout.prefix, tool, path_var);
            if !foreign.is_empty() {
                line.push('\n');
                line.push_str(&foreign_copies_line(&foreign));
            }
        }
        return Ok(line);
    }
    // 1a. A live build whose own-name shim is lost ([`live_without_shim`]): installed,
    //     its shim missing — not a bundle (arm 1b) and not uninstalled.
    if let Some(lost) = live_without_shim(layout, tool) {
        let state = managed_state_of(layout, tool, lost.build, row_of(tool).as_ref());
        // An agent program's `agents/` twin execs the store build itself, so where it comes
        // first the managed copy still runs.
        if let Some(twin) = lost_shim_twin_runs(layout, tool, path_var) {
            return Ok(format!(
                "{tool} → {} → {} — {state} — its shim {} is missing, so a shell without \
                 {} on its PATH finds no managed copy (fix: aterm pkg repair)",
                twin.display(),
                lost.binary.display(),
                lost.shim.display(),
                layout.agents_dir().display()
            ));
        }
        return Ok(
            match crate::vendor::system_binary_on_path(&layout.prefix, tool, path_var) {
                Some(foreign) => format!(
                    "{tool} → {} — runs instead: the managed shim {} is missing — {state} \
                     (fix: aterm pkg repair)",
                    foreign.display(),
                    lost.shim.display()
                ),
                None => format!(
                    "{tool} → (nothing runs) — its shim {} is missing — {state} \
                     (fix: aterm pkg repair)",
                    lost.shim.display()
                ),
            },
        );
    }
    // 1b. An index program whose build exposes no tool of its own name (`trust` exposes
    //     trustc/targo/…): the bundle answers with the names it puts on PATH; a same-named
    //     unrelated binary (p11-kit's `trust`) is named as that, after the truth.
    if let Some(build) = crate::active_builds(layout).get(tool).copied() {
        let state = managed_state_of(layout, tool, build, row_of(tool).as_ref());
        let mut exposes = crate::installed_exposes(layout, tool).unwrap_or_default();
        exposes.sort();
        let mut line = match exposes.first() {
            Some(first) => format!(
                "{tool} → {} (bundle; exposes {} — run `aterm pkg which {first}`) — {state}",
                layout.build_dir(tool, build).display(),
                exposes.join(", ")
            ),
            None => format!(
                "{tool} → {} (bundle; exposes no command of its own) — {state}",
                layout.build_dir(tool, build).display()
            ),
        };
        if let Some(path) = crate::vendor::system_binary_on_path(&layout.prefix, tool, path_var) {
            line.push('\n');
            line.push_str(&format!(
                "{} on PATH is an unrelated binary, not this program",
                path.display()
            ));
        }
        return Ok(line);
    }
    // 1b'. A program dev-linked from a checkout whose own name is not a tool (`trust`): the
    //      link is what runs, said before the not-installed arm.
    if crate::linkmode::is_linked(layout, tool) {
        return Ok(dev_linked_program_line(
            tool,
            crate::linkmode::linked_checkout(layout, tool).as_deref(),
            &crate::linkmode::linked_bins(layout, tool),
            crate::vendor::system_binary_on_path(&layout.prefix, tool, path_var).as_deref(),
        ));
    }
    // 2. A copy on PATH outside the prefix that runs. With a PENDING STUB in the managed
    //    bin/ the question is which comes FIRST on PATH: a foreign copy ahead of the
    //    managed bin/ runs; one behind it loses to the stub, which waits (arm 3) — the
    //    shadow walk answers that, the system walk would skip the stub.
    let stubbed = crate::stub::pending_stub_exists(layout, tool);
    let index = cached_index(layout);
    let foreign = if stubbed {
        crate::vendor::shadowing_binary_on_path(&layout.prefix, tool, path_var)
    } else {
        crate::vendor::system_binary_on_path(&layout.prefix, tool, path_var)
    };
    // 2a. …that is an UNRELATED binary sharing the name of a program of ALab's own that
    //     nothing installed here (p11-kit's `/usr/bin/trust` on every Ubuntu): the truth
    //     first — the recorded reason, the pending stub's, or plain not-installed — then
    //     the binary named for what it is, never "the system copy" of the program. ALab's
    //     own: an index program no system copy may satisfy, or, before any index is
    //     cached (a fresh machine, or one whose seed laid stubs and no pass has run), a
    //     member of the compiled default set. Not an agent program, whose foreign copies
    //     are the vendor's own installs.
    if let Some(path) = &foreign
        && !crate::stub::is_agent_program(tool)
        && match index.as_ref() {
            Some(index) => index.program(tool).is_some_and(|e| e.system.is_none()),
            None => crate::stub::describe(tool).is_some(),
        }
    {
        let row = row_of(crate::stub::program_of(tool)).map(|r| r.state);
        return Ok(index_program_shadowed_by_unrelated_line(
            tool,
            row.as_deref(),
            stubbed,
            path,
        ));
    }
    // 2b. …otherwise a system copy: it runs, and atpkg does not manage it.
    if let Some(path) = foreign {
        let retired = layout.retired_date(tool).or_else(|| {
            row_of(tool).and_then(|r| crate::state::system_retired(&r.state).map(str::to_string))
        });
        return Ok(format!(
            "{tool} → {} — {}",
            path.display(),
            crate::state::system(&path, retired.as_deref())
        ));
    }
    // 3. A pending stub in the managed bin/: typing the name waits — and a recorded row
    //    that says why it waits (unserved here) outranks the generic promise.
    if stubbed {
        // A command a program exposes (`targo`) waits on that program (`trust`): its row,
        // and the install that lays it ([`crate::stub::program_of_in`]).
        let program = crate::stub::program_of_in(layout, tool);
        let program = program.as_str();
        // …and a HELD row (`held: last attempt failed with …; <retry>`): the stub is
        // waiting on a person, not on the next pass, and the row says which act.
        let waiting = row_of(program).map(|r| r.state).filter(|st| {
            st.starts_with(crate::state::UNAVAILABLE_PREFIX)
                || st.starts_with(crate::state::HELD_FAILED_PREFIX)
        });
        return Ok(match waiting {
            Some(st) => format!("{tool} → {} (pending stub) — {st}", shim.display()),
            None => format!(
                "{tool} → {} (pending stub) — not installed yet: the next pass installs it \
                 (now: aterm pkg install {program})",
                shim.display()
            ),
        });
    }
    // 4. A recorded state that names no shim: a target the pinned build does not serve,
    //    or a member held after a failed stage (the row names the retry).
    if let Some(row) = row_of(tool)
        && (row.state.starts_with(crate::state::UNAVAILABLE_PREFIX)
            || row.state.starts_with(crate::state::HELD_FAILED_PREFIX))
    {
        return Ok(format!("{tool} → (nothing runs) — {}", row.state));
    }
    // 5. An `alab-<x>` nothing answers to: say why there is no alias rather than promise
    //    `aterm pkg install alab-x`. Aliases exist only for ALab's own tools (the cached
    //    index says which, offline); an ALab tool not installed gets the plain name's fix.
    if let Some(base) = tn.alias_base() {
        return Err(no_alias_line(layout, tool, base.as_str()));
    }
    // 6. A name the cached index does not serve: `install` refuses it, so say so in
    //    install's own words (with the roster) instead of promising an install. A
    //    command is asked about by the program that ships it (`targo` by `trust`).
    if let Some(line) =
        unserved_which_line(&crate::stub::program_of_in(layout, tool), index.as_deref())
    {
        return Err(line);
    }
    Err(not_runnable_line(layout, tool))
}

/// ` — based on upstream rust-v0.163.0+g0123456789ab` when `program` is one ALab builds and
/// `exe` is the entrypoint of an ALab package whose record names its upstream
/// ([`crate::alab::upstream_of_exe`]); empty otherwise, a package from before the field
/// included. Read off the record, never by running `exe`.
fn upstream_clause(program: &str, exe: &std::path::Path) -> String {
    crate::alab::agent(program)
        .and_then(|_| crate::alab::upstream_of_exe(exe))
        .map(|upstream| format!(" — {}", upstream.words()))
        .unwrap_or_default()
}

/// `which`'s answer for a name the cached `index` does not serve and no vendor program
/// answers to: install's own refusal, roster and all. `None` with no cached index or for a
/// name that installs.
fn unserved_which_line(tool: &str, index: Option<&crate::manifest::Index>) -> Option<String> {
    let index = index?;
    if index.is_program(tool) || crate::vendor_direct::is_vendor(tool) {
        return None;
    }
    let roster = index.programs.keys().cloned().collect();
    Some(format!(
        "atpkg: {}",
        crate::FlowError::NotReachable(tool.to_string(), roster)
    ))
}

/// The `which` answer for a program dev-linked from a checkout (arm 1b' of
/// [`which_line`]): where it is linked from, what it exposes, and — when a same-named
/// unrelated binary sits on PATH — that binary named for what it is.
fn dev_linked_program_line(
    tool: &str,
    checkout: Option<&std::path::Path>,
    bins: &[String],
    foreign: Option<&std::path::Path>,
) -> String {
    let mut sorted: Vec<&str> = bins.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let from = checkout.map_or_else(|| "a checkout".to_string(), |c| c.display().to_string());
    let mut line = if let Some(first) = sorted.first() {
        format!(
            "{tool} → {from} (dev-linked; exposes {} — run `aterm pkg which {first}`) — managed from that checkout, not the index; `aterm pkg unlink {tool}` returns it to the index",
            sorted.join(", ")
        )
    } else {
        format!(
            "{tool} → {from} (dev-linked; exposes no command of its own) — managed from that checkout, not the index; `aterm pkg unlink {tool}` returns it to the index"
        )
    };
    if let Some(path) = foreign {
        line.push('\n');
        line.push_str(&format!(
            "{} on PATH is an unrelated binary, not this program",
            path.display()
        ));
    }
    line
}

/// The `which` answer for a program of ALab's own that is NOT installed while a
/// same-named unrelated binary on PATH runs in its place (arm 2a of [`which_line`]).
/// Pure, so the wording is unit-testable without a signed index cache on disk.
///
/// The truth leads: a recorded row that says WHY nothing runs (unserved on this target,
/// or held) is quoted as-is; else a pending stub's promise (`stubbed`: it stands in the
/// managed bin/, behind the unrelated binary); otherwise plain not-installed with the
/// fix. Then the foreign binary, named for what it is — never as "the system copy" of
/// a program it has nothing to do with.
fn index_program_shadowed_by_unrelated_line(
    tool: &str,
    row_state: Option<&str>,
    stubbed: bool,
    path: &std::path::Path,
) -> String {
    let truth = match row_state {
        Some(st)
            if st.starts_with(crate::state::UNAVAILABLE_PREFIX)
                || st.starts_with(crate::state::HELD_FAILED_PREFIX) =>
        {
            format!("(nothing runs) — {st}")
        }
        _ if stubbed => format!(
            "(nothing runs) — not installed yet: the next pass installs it (now: aterm pkg \
             install {})",
            crate::stub::program_of(tool)
        ),
        _ => format!("(nothing runs) — not installed (fix: aterm pkg install {tool})"),
    };
    format!(
        "{tool} → {truth}\n{} on PATH is an unrelated binary, not this program",
        path.display()
    )
}

/// `foreign copies out-ranked: ~/.local/bin/claude (2.1.267), /opt/homebrew/bin/claude
/// (2.1.204)` — each path home-abbreviated, with the version its real location spells
/// ([`version_from_path`]). Nothing is executed to learn a version: `which` is read-only.
fn foreign_copies_line(foreign: &[std::path::PathBuf]) -> String {
    foreign_copies_line_with("foreign copies out-ranked", foreign)
}

/// The label of the agent programs' second `which` line in a shell where a foreign copy
/// runs: `foreign copies on this shell's PATH: …` — every copy, the running one included.
const FOREIGN_ON_THIS_SHELL: &str = "foreign copies on this shell's PATH";

/// `p` as the `which` lines spell it: `~/…` under the home directory, the full path elsewhere.
fn home_abbreviated(p: &std::path::Path) -> String {
    aterm_types::dirs::home_dir()
        .as_deref()
        .and_then(|h| p.strip_prefix(h).ok())
        .map_or_else(
            || p.display().to_string(),
            |rest| format!("~/{}", rest.display()),
        )
}

/// [`foreign_copies_line`] under `label`.
fn foreign_copies_line_with(label: &str, foreign: &[std::path::PathBuf]) -> String {
    let rendered: Vec<String> = foreign
        .iter()
        .map(|p| {
            let shown = home_abbreviated(p);
            match version_from_path(p) {
                Some(v) => format!("{shown} ({v})"),
                None => shown,
            }
        })
        .collect();
    format!("{label}: {}", rendered.join(", "))
}

/// The version a foreign copy's real location spells: the last dotted-digits component of
/// its canonical path (`~/.local/share/claude/versions/2.1.267`,
/// `Caskroom/claude-code/2.1.204/claude`, `Caskroom/codex/0.151.0/bin/codex`). `None` when
/// no component qualifies: the line shows the path alone, never a guess.
fn version_from_path(path: &std::path::Path) -> Option<String> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    version_from_components(&real)
}

/// The pure half of [`version_from_path`]: last dotted-digits component wins.
fn version_from_components(path: &std::path::Path) -> Option<String> {
    path.components().rev().find_map(|c| {
        let s = c.as_os_str().to_str()?;
        let dotted = s.contains('.')
            && s.split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
        dotted.then(|| s.to_string())
    })
}

/// The `which` answer for a rerouted upstream name: the stub that runs inside a session, its
/// state (`laid` / `missing` / `foreign`, [`crate::reroute::states`]), and the row's policy
/// in the stub's own words ([`crate::reroute::policy_summary`]).
fn reroute_which_line(
    layout: &crate::store::Layout,
    row: &crate::reroute::Row,
    path_var: Option<&std::ffi::OsStr>,
) -> String {
    use crate::reroute::StubState;
    let stub = crate::reroute::stub_path(layout, row.upstream);
    let state = crate::reroute::states(layout)
        .into_iter()
        .find(|(name, _)| *name == row.upstream)
        .map_or("missing", |(_, state)| match state {
            StubState::Laid => "laid",
            StubState::Missing => "missing",
            StubState::Foreign => "foreign",
        });
    format!(
        "{} → {} (reroute stub, {state}) — rerouted inside aterm sessions: {}",
        row.upstream,
        stub.display(),
        crate::reroute::policy_summary(
            row,
            crate::reroute::upstream_on_path(layout, row.upstream, path_var).is_some(),
            crate::config::cached_reroute().strict(),
        )
    )
}

/// The one-line answer for `alias` (`alab-<base>`) when no such shim is laid — the reverse
/// of [`alias_fix`]. Reads the cached index (offline; absent ⇒ the base is unknown).
fn no_alias_line(layout: &crate::store::Layout, alias: &str, base: &str) -> String {
    let index = cached_index(layout);
    no_alias_line_for(
        layout,
        alias,
        base,
        index.as_ref().and_then(|i| i.program(base)),
    )
}

/// [`no_alias_line`] over an already-looked-up index `entry` for `base` (`None` ⇒ not
/// listed, or nothing cached).
fn no_alias_line_for(
    layout: &crate::store::Layout,
    alias: &str,
    base: &str,
    entry: Option<&crate::manifest::Program>,
) -> String {
    match entry {
        Some(p) if p.system.is_some() => format!(
            "atpkg: no alias {alias} — {base} is not one of ALab's own tools (a system copy \
             may satisfy it: system = \"{}\"); type {base}",
            p.system.as_deref().unwrap_or(base)
        ),
        Some(_) if crate::stub::is_agent_program(base) => format!(
            "atpkg: no alias {alias} — {base} is not one of ALab's own tools (a vendor's); \
             type {base}"
        ),
        Some(_) if crate::which(layout, base).is_some() => format!(
            "atpkg: {alias} is not laid yet — the next pass lays it beside {base} (now: aterm \
             pkg update)"
        ),
        Some(_) => format!(
            "atpkg: {alias} names nothing yet — {}",
            not_installed_fix(base)
        ),
        None if crate::which(layout, base).is_some() => format!(
            "atpkg: no alias {alias} — {base} is not one of ALab's own tools, or the next \
             pass lays it (aterm pkg update); type {base}"
        ),
        None => format!("atpkg: no alias {alias} — {}", not_installed_fix(base)),
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
///
/// IT RUNS WHAT THE MANAGED NAME RUNS. `aterm claude` is the way to the managed copy from
/// a terminal the `agents/` twin does not lead (`crate::hooks`), so this verb does before
/// its `exec` what the shim and the twin do before theirs: it exports the environment the
/// build declares ([`crate::ops::managed`], design S7 — `DISABLE_AUTOUPDATER=1` for
/// Claude Code), and a vendor's own self-update verb as the FIRST argument (`claude
/// update|upgrade|install`, `codex update` — [`crate::selfupdate`]) is answered by the
/// package manager (the plan [`cmd_selfupdate`] carries out), never by the vendor's
/// updater, which installs a copy this name never runs. The one ending of that plan that
/// runs the tool — help (`-h` or
/// `--help` ahead of any `--`): the note, then the vendor's own help — runs it HERE, the
/// store binary with its build's environment like every other argument list, not through
/// `bin/<program>` as the twin's hand-over does: on a machine whose shim lost that
/// environment, the shim would run it without.
fn cmd_run(rest: &[String]) -> ExitCode {
    let Some((tool, args)) = rest.split_first() else {
        return usage_error("run", None);
    };
    // Drop a single leading `--` separator if the caller passed one.
    let args: &[String] = match args.split_first() {
        Some((sep, tail)) if sep == "--" => tail,
        _ => args,
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    // `exec_path`, not `which`: this execs the tool without its shim, so it takes the exec
    // root the shim's guard would (`crate::compat`). A pending stub gets the stub's own
    // answer; a live build whose shim is lost runs from the store and says so ([`run_target`]).
    let (target, env) = match run_target(&layout, tool) {
        RunTarget::Exec { path, env, notice } => {
            if let Some(notice) = notice {
                eprintln!("{notice}");
            }
            (path, env)
        }
        // With the tool's arguments: a person who waits has the tool run with them.
        RunTarget::Pending => return run_pending(&layout, tool, args),
        RunTarget::LostNotRun(line) => {
            eprintln!("{line}");
            return ExitCode::from(127);
        }
        RunTarget::NotInstalled => {
            eprintln!("{}", not_runnable_line(&layout, tool));
            return ExitCode::from(127);
        }
    };
    // A vendor self-update verb, by the twin's rule ([`runs_self_update_verb`]), goes by the
    // hidden verb's plan ([`selfupdate_plan`]). Help is the one ending that runs the tool,
    // and this verb runs it itself with the build's environment, where the hidden verb would
    // `exec` `bin/<program>`, which exports only what that shim does. Rust on every platform,
    // unlike the `.cmd` twin, whose block is empty.
    if runs_self_update_verb(&layout, tool, args)
        && let Some(name) = crate::store::ToolName::new(tool)
    {
        let resolved = crate::store::resolve_configured();
        match selfupdate_plan(&layout, &name, args, resolved.as_ref(), manager_enabled()) {
            SelfUpdatePlan::Forward { note, .. } => {
                if let Some(note) = note {
                    eprintln!("{note}");
                }
            }
            plan => return settle_selfupdate(&layout, plan),
        }
    }
    // THE TWIN'S `--yolo` BLOCK, by the same tables ([`crate::yolo`]): this verb
    // execs past the shim, so it decides the launch itself — the word expanded,
    // or the posture injected by default, or a decided launch left as typed —
    // and prints the one echo line the twin's block prints. After the
    // self-update arm, exactly as the blocks stand in the twin (an update shape
    // is decided there first). Rust on every platform, like that arm.
    let expanded: Vec<String>;
    let args: &[String] = match crate::yolo::decide(tool, args) {
        crate::yolo::Launch::Flag(args) => {
            eprintln!("{}", crate::yolo::echo_line(tool, &args));
            expanded = args;
            &expanded
        }
        crate::yolo::Launch::Default(args) => {
            eprintln!("{}", crate::yolo::default_echo_line(tool, &args));
            expanded = args;
            &expanded
        }
        crate::yolo::Launch::AsTyped => args,
    };
    let child_path =
        crate::store::append_bin_to_path(std::env::var_os("PATH").as_deref(), &layout.bin_dir());
    // `platform::exec_or_run` replaces this process on Unix, or spawns, waits and exits on
    // Windows; reaching past it means the launch failed. Raw, not `spawnfd::command`:
    // `aterm pkg run` becomes the tool, so its caller's descriptors are the tool's. Its
    // Windows console is decided here.
    #[expect(
        clippy::disallowed_types,
        reason = "aterm pkg run becomes the tool by exec: its caller's descriptors are the tool's"
    )]
    let mut command = std::process::Command::new(&target);
    aterm_uds::spawn::set_flags(&mut command, 0);
    command.args(args).env("PATH", child_path);
    // …and the environment the shim would export before its `exec` (`ops::exec_env`):
    // after `PATH`, which no admitted entry can name.
    for (name, value) in env.entries() {
        command.env(name, value);
    }
    let err = crate::platform::exec_or_run(&mut command);
    eprintln!("atpkg: failed to exec {}: {err}", target.display());
    ExitCode::from(127)
}

/// Whether [`cmd_run`] answers `tool args…` by the self-update plan ([`selfupdate_plan`]):
/// exactly when the `agents/` twin's block would hand them over — an agent program whose
/// `bin/` shim resolves into an agent program's store tree
/// ([`crate::activate::reconcile_agents`]), with the first argument one of its rostered
/// verbs ([`crate::selfupdate::verbs_of`]). The first argument only: `claude -p update` is
/// a prompt.
fn runs_self_update_verb(layout: &crate::store::Layout, tool: &str, args: &[String]) -> bool {
    args.first()
        .is_some_and(|first| crate::selfupdate::verbs_of(tool).contains(&first.as_str()))
        && crate::stub::is_agent_program(tool)
        && crate::which(layout, tool)
            .and_then(|target| crate::ops::store_build_of(&layout.prefix, &target))
            .is_some_and(|(program, _)| crate::stub::is_agent_program(&program))
}

/// `atpkg lease`'s argv, parsed ([`parse_lease_args`]).
#[derive(Debug, PartialEq, Eq)]
struct LeaseArgs {
    /// The toolchain directory the command resolved — a store build's `bin/`, the build
    /// itself, a tool in it, its exec root, or the rustup view ([`crate::lease::Subject::of_dir`]).
    dir: std::path::PathBuf,
    /// The words that name the run in its lease (`--who`), else the command's own.
    who: Option<String>,
    /// The command and its arguments, whole: everything after the first `--`.
    command: Vec<String>,
}

/// `<toolchain-dir> [--who <words>] -- <command> [args…]`: the first `--` ends atpkg's argv;
/// before it, one directory and at most one `--who`. `Err` names what is wrong.
fn parse_lease_args(rest: &[String]) -> Result<LeaseArgs, String> {
    let Some(sep) = rest.iter().position(|a| a == "--") else {
        return Err("no `--` before the command to run".to_string());
    };
    let (head, command) = (&rest[..sep], &rest[sep + 1..]);
    if command.is_empty() {
        return Err("no command after `--`".to_string());
    }
    let mut dir = None;
    let mut who = None;
    let mut it = head.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--who" if who.is_none() => match it.next() {
                Some(words) => who = Some(words.clone()),
                None => return Err("`--who` takes the words that name the run".to_string()),
            },
            s if !s.starts_with('-') && dir.is_none() => dir = Some(std::path::PathBuf::from(s)),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let Some(dir) = dir else {
        return Err("no toolchain directory to hold".to_string());
    };
    Ok(LeaseArgs {
        dir,
        who: who.filter(|w| !w.trim().is_empty()),
        command: command.to_vec(),
    })
}

/// The one line a lease carries ([`crate::lease::take`]'s `who`): the run's name, the
/// holder's pid and its directory — what `aterm pkg doctor` prints beside the build it keeps.
fn lease_who(args: &LeaseArgs, pid: u32, cwd: &std::path::Path) -> String {
    format!(
        "{} (pid {pid}) \u{2014} held by `aterm pkg lease` in {}",
        lease_run_name(args),
        cwd.display()
    )
}

/// The run's name: `--who`, else the command's file name and the arguments after it.
fn lease_run_name(args: &LeaseArgs) -> String {
    args.who.clone().unwrap_or_else(|| {
        let mut words = vec![
            std::path::Path::new(&args.command[0])
                .file_name()
                .map_or_else(
                    || args.command[0].clone(),
                    |n| n.to_string_lossy().into_owned(),
                ),
        ];
        words.extend(args.command[1..].iter().cloned());
        words.join(" ")
    })
}

/// How a holder's run ended ([`lease_and_run`]).
#[derive(Debug)]
enum LeaseRun {
    /// The command ran to its end, under the lease when one was taken.
    Ran(std::process::ExitStatus),
    /// The directory was not there once the lease was laid: nothing ran.
    Missing(crate::lease::Missing),
    /// The command could not be started (the lease was let go).
    NotStarted(std::io::Error),
    /// The command started but its status could not be collected.
    Lost(std::io::Error),
}

/// The holder verb's body: lease the toolchain `args.dir` belongs to under `prefix` (`None`:
/// no prefix resolves, so nothing can be leased — nor can anything of atpkg's reclaim a
/// build), say what is held ([`lease_hold_line`] through `say`), run the command as a
/// child, wait for it standing aside from the terminal's ^C and passing a TERM or HUP on to
/// it ([`crate::platform::wait_standing_aside`]), and let the lease go once it has ended.
/// The lease's descriptor is close-on-exec, so no process the command starts (an `sccache`
/// server) carries the lock past the command's end.
fn lease_and_run(
    prefix: Option<&std::path::Path>,
    args: &LeaseArgs,
    who: &str,
    wait: std::time::Duration,
    say: &mut dyn FnMut(&str),
) -> LeaseRun {
    let hold = match prefix {
        Some(prefix) => match crate::lease::hold_for(prefix, &args.dir, who, wait) {
            Ok(hold) => Some(hold),
            Err(missing) => return LeaseRun::Missing(missing),
        },
        None => None,
    };
    say(&lease_hold_line(
        hold.as_ref(),
        &args.dir,
        &lease_run_name(args),
    ));
    // Transparent, not `spawnfd::command`: the caller's own command keeps all its caller's
    // descriptors (a make jobserver's pipe, a `3>` redirect); its Windows console is
    // `Quiet`.
    let mut command = aterm_uds::spawnfd::transparent(&args.command[0]);
    let mut child = match command.args(&args.command[1..]).spawn() {
        Ok(child) => child,
        Err(e) => return LeaseRun::NotStarted(e),
    };
    let status = crate::platform::wait_standing_aside(&mut child);
    drop(hold);
    match status {
        Ok(status) => LeaseRun::Ran(status),
        Err(e) => LeaseRun::Lost(e),
    }
}

/// The holder's one line on stderr: what it holds and what that buys, or why it holds
/// nothing — said either way, so a run is never silently unprotected.
fn lease_hold_line(hold: Option<&crate::lease::Hold>, dir: &std::path::Path, run: &str) -> String {
    match hold {
        Some(crate::lease::Hold::Held(lease)) => {
            let keeps = match lease.subject() {
                crate::lease::Subject::Build { .. } => {
                    "gc keeps it, and an unattended update waits while it is the live build"
                }
                crate::lease::Subject::View { .. } => {
                    "it is not re-laid, and an unattended trust update waits for it"
                }
            };
            format!(
                "atpkg lease: holding {} while {run} runs \u{2014} {keeps}",
                lease.subject().describe()
            )
        }
        Some(crate::lease::Hold::Unmanaged) => format!(
            "atpkg lease: {} is no build atpkg manages \u{2014} nothing of atpkg's reclaims \
             it, so nothing is held",
            dir.display()
        ),
        Some(crate::lease::Hold::NotTaken(subject, e)) => format!(
            "atpkg lease: warn \u{2014} the lease on {} was NOT taken ({e}); {run} runs \
             unprotected: an update landing meanwhile can reclaim or re-lay it",
            subject.describe()
        ),
        None => format!(
            "atpkg lease: warn \u{2014} no package prefix resolves, so nothing was leased; \
             {run} runs unprotected"
        ),
    }
}

/// `atpkg lane` ([`crate::lane`]): the Rust-lane reader over one shell command line. Exit 0
/// when no stock Rust tool stands at command position (one stderr line when an
/// `ATERM_STOCK_REASON` cleared one), [`crate::reroute::REFUSAL_EXIT`] with the refusal
/// when one does, 1 for unreadable input or arguments or input over
/// [`crate::lane::MAX_INPUT_BYTES`].
fn cmd_lane(rest: &[String]) -> ExitCode {
    let args = match crate::lane::parse_args(rest) {
        Ok(args) => args,
        // Exit 1, not [`usage_error`]'s 2: `lane`'s 2 is its refusal, and a guard must
        // never read a usage error as one.
        Err(why) => {
            print_usage_error("lane", Some(&why));
            return ExitCode::from(1);
        }
    };
    let input = if matches!(args.source, crate::lane::Source::Words(_)) {
        Vec::new()
    } else {
        match crate::lane::read_input(std::io::stdin()) {
            Ok(input) => input,
            Err(why) => {
                eprintln!("atpkg lane: {why} — no verdict");
                return ExitCode::from(1);
            }
        }
    };
    let command = match crate::lane::command_line(&args.source, &input) {
        Ok(Some(command)) => command,
        // No command line at that field: nothing to read, nothing to refuse.
        Ok(None) => return ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("atpkg lane: {why} — no verdict");
            return ExitCode::from(1);
        }
    };
    let cwd = args.cwd.or_else(|| std::env::current_dir().ok());
    let home = aterm_types::dirs::home_dir();
    let env_toolchain = std::env::var("RUSTUP_TOOLCHAIN").ok();
    match crate::lane::answer(
        &command,
        cwd.as_deref(),
        home.as_deref(),
        env_toolchain.as_deref(),
    ) {
        crate::lane::Answer::Clean => ExitCode::SUCCESS,
        crate::lane::Answer::Escaped(line) => {
            eprintln!("{line}");
            ExitCode::SUCCESS
        }
        crate::lane::Answer::Refused(text) => {
            eprintln!("{text}");
            ExitCode::from(crate::reroute::REFUSAL_EXIT)
        }
    }
}

/// `atpkg lease <toolchain-dir> [--who <words>] -- <command> [args…]` — the holder verb
/// ([`crate::lease::hold_for`]): hold the store build (or rustup view) a multi-command run
/// resolved for exactly as long as the command lives, so gc keeps it, the seam does not
/// re-lay it, and an unattended trust update waits for it. macOS has no `flock(1)`, and a
/// lock on a descriptor the script keeps would be inherited by every daemon its build
/// starts. `atpkg-publish`'s packers take the lease in-process (`atpkg::lease`).
///
/// Exit: the command's own status (its code, or its signal re-raised,
/// [`crate::platform::exit_code_like`]); 2 for a usage error; 1 when the directory is gone
/// (nothing ran); 127 when the command could not be started. A lease that cannot be taken
/// never stops the command: it runs, and the line says it runs unprotected.
fn cmd_lease(rest: &[String]) -> ExitCode {
    let args = match parse_lease_args(rest) {
        Ok(args) => args,
        Err(why) => return usage_error("lease", Some(&why)),
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let who = lease_who(&args, std::process::id(), &cwd);
    let layout = layout();
    let run = lease_and_run(
        layout.as_ref().map(|l| l.prefix.as_path()),
        &args,
        &who,
        crate::lease::DEFAULT_WAIT,
        &mut |line| eprintln!("{line}"),
    );
    match run {
        LeaseRun::Ran(status) => crate::platform::exit_code_like(status),
        LeaseRun::Missing(crate::lease::Missing { dir, managed }) => {
            if managed {
                eprintln!(
                    "atpkg lease: {} is gone (cleaned up, or never installed) \u{2014} nothing \
                     ran; look up the toolchain path again and rerun",
                    dir.display()
                );
            } else {
                eprintln!(
                    "atpkg lease: {} does not exist \u{2014} nothing ran",
                    dir.display()
                );
            }
            ExitCode::from(1)
        }
        LeaseRun::NotStarted(e) => {
            eprintln!("atpkg lease: could not start {}: {e}", args.command[0]);
            ExitCode::from(127)
        }
        LeaseRun::Lost(e) => {
            eprintln!(
                "atpkg lease: lost {} ({e}) \u{2014} the lease is let go",
                args.command[0]
            );
            ExitCode::from(1)
        }
    }
}

/// One usage refusal: the specific complaint (`atpkg <verb>: <problem>`) when there is
/// one, then the verb's grammar, exit 2.
///
/// The grammar comes from the verb's [`Verb::usage`] rather than a literal at the call site — a
/// second copy is the thing that rots. A verb with no entry prints no usage line, never a
/// panic on the error path.
pub(crate) fn usage_error(verb: &str, problem: Option<&str>) -> ExitCode {
    print_usage_error(verb, problem);
    ExitCode::from(2)
}

/// [`usage_error`]'s lines without its exit code, for the one verb whose usage error is
/// not 2: `lane`, where 2 is the refusal ([`crate::reroute::REFUSAL_EXIT`]).
fn print_usage_error(verb: &str, problem: Option<&str>) {
    if let Some(problem) = problem {
        eprintln!("atpkg {verb}: {problem}");
    }
    if let Some(usage) = verb_usage(verb) {
        eprintln!("usage: {usage}");
    }
}

/// `atpkg list` — the installed `(program, build)` pairs.
///
/// TWO AUDIENCES, TWO SHAPES, ONE SET OF FACTS. A terminal gets the human report:
/// verdict first (`atpkg: N program(s) — L live`), then one row per PROGRAM with its
/// live build. A pipe (or `--porcelain`, for a script that runs on a TTY) gets the
/// headerless `program\tbuild\tnotes` rows every existing parser keys on: ascending
/// builds, superseded before live, `atpkg: no programs installed` when empty.
///
/// The only accepted flag is `--porcelain`; anything else is a usage error.
fn cmd_list(args: &[String]) -> ExitCode {
    use std::io::IsTerminal as _;
    let mut porcelain = false;
    for a in args {
        match a.as_str() {
            "--porcelain" => porcelain = true,
            _ => return usage_error("list", None),
        }
    }
    let human = !porcelain && std::io::stdout().is_terminal();
    run_list(layout(), human)
}

/// `atpkg repair [--json]` — put a half-installed store and the shell integration back
/// into a state the rest of the CLI can describe honestly (docs/AUDIT-nux-first-open-
/// toolchain-2026-08-31.md, B6), as converge items (docs/DESIGN-init-system-2026-10-20.md
/// S5, [`crate::converge::repair`]).
///
/// Repair restores installed commands, shell integration, the reroute stubs (abrew's
/// `brew` too), the rustup link, exec roots, an outlived held update and (macOS) the
/// `~/.local/bin` links: every `relay` and `asked` item, each probed, applied where it
/// differs and probed again. It is the asked-for pass, so it alone lays atpkg's rc block
/// again over an rc the person deleted it from, and says which ([`Pass::Repair`]); and it
/// re-runs what it always re-ran — every program's shims, the exec roots proven at
/// `Deep`, the recorded rustup seams — whatever their probe reads.
///
/// Exits 1 when something remains that repair cannot do locally — an item it could not
/// apply, or one only an install fixes (a build gone, one ALab withdrew) — so a script can
/// tell "fixed" from "needs the network". `--json` prints the rows in the one report
/// schema ([`aterm_converge::Report`], mode `apply`) instead of the lines.
///
/// [`Pass::Repair`]: crate::converge::repair::Pass::Repair
fn run_repair(layout: Option<crate::store::Layout>, json: bool) -> ExitCode {
    let Some(layout) = layout else {
        return ExitCode::from(1);
    };
    let ground = std::sync::Arc::new(crate::converge::repair::Ground::of_process(
        layout,
        crate::converge::repair::Pass::Repair,
    ));
    // Repair changes this account's files only from the account's own shell (design §3.2,
    // `Requires::Account`): under `sudo`, another `$HOME` or a private state root (the
    // process's own, or the one a window's lead names) each change is named, never made.
    let scope = aterm_converge::Scope {
        not_own_account: aterm_types::dirs::not_own_account().or_else(|| {
            under_private_state_root()
                .then_some("it runs for an isolated aterm instance's private state root")
        }),
        ..aterm_converge::Scope::REPAIR
    };
    let rows = match aterm_types::dirs::account_state_dir() {
        Some(state) => repair_rows(
            &ground,
            &mut aterm_converge::FileJournal::under(&state),
            None,
            scope,
        ),
        // No state root to journal into: nothing is changed unrecorded, so every change
        // fails on the journal's refusal and says why.
        None => repair_rows(&ground, &mut aterm_converge::ReadOnly, None, scope),
    };
    let code = repair_exit(&rows);
    if json {
        let report = aterm_converge::Report::new(
            aterm_converge::ReportMode::Apply,
            code == ExitCode::SUCCESS,
            rows,
        );
        return match aterm_json::to_string(&report) {
            Ok(text) => {
                println!("{text}");
                code
            }
            Err(why) => {
                eprintln!("atpkg repair: cannot encode the report: {why}");
                ExitCode::from(1)
            }
        };
    }
    for line in repair_lines(&rows) {
        println!("{line}");
    }
    code
}

/// The repair items over `ground` through the engine, applied within `scope`: `relay` and
/// `asked` ([`aterm_converge::Scope::REPAIR`]), the install-class store row probed and
/// never applied, and nothing changed outside the account's own shell. `only` narrows the
/// run to those ids. The store lock is the caller's: the verb takes it at the dispatch
/// edge.
fn repair_rows(
    ground: &std::sync::Arc<crate::converge::repair::Ground>,
    journal: &mut dyn aterm_converge::Journal,
    only: Option<&[&str]>,
    scope: aterm_converge::Scope,
) -> Vec<aterm_converge::ItemReport> {
    let items: Vec<Box<dyn aterm_converge::Item>> = crate::converge::repair::items(ground)
        .into_iter()
        .filter(|item| only.is_none_or(|ids| ids.contains(&item.id().as_str())))
        .collect();
    aterm_converge::run_scoped(
        &items,
        aterm_converge::Mode::Apply,
        scope,
        journal,
        &mut |_| {},
    )
    // The one edge (the rc block after the hooks) names an item of every full run; a
    // narrowed run that drops the hooks keeps the edge only in the full composition.
    .unwrap_or_else(|refusal| {
        vec![aterm_converge::ItemReport {
            id: crate::converge::repair::STORE.to_owned(),
            family: "pkg".to_owned(),
            title: "repair's own items".to_owned(),
            outcome: aterm_converge::Outcome::Failed,
            from: None,
            to: None,
            why: Some(format!("repair could not order its items: {refusal}")),
            revert: None,
            effect: aterm_converge::Effect::Now,
            detail: None,
            layer: Some(aterm_converge::Layer::Base),
            class: Some(aterm_converge::Class::Relay),
            explain: None,
            stood_aside: None,
            essential: false,
        }]
    })
}

/// `repair`'s exit rule over its rows: 1 when an item is left undone — failed, blocked
/// (only an install, the network or a person can do it), or unread (its state could not
/// be read, so nothing vouches for it) — else 0.
fn repair_exit(rows: &[aterm_converge::ItemReport]) -> ExitCode {
    if rows.iter().any(repair_left_undone) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Whether `repair` left `row` undone ([`repair_exit`]): anything but done, already right,
/// or not this machine's to do.
fn repair_left_undone(row: &aterm_converge::ItemReport) -> bool {
    use aterm_converge::Outcome;
    !matches!(
        row.outcome,
        Outcome::Ok | Outcome::Changed | Outcome::NotApplicable | Outcome::Deferred
    )
}

/// What `repair` prints of its rows, one `repair:` line each: what each item did (its
/// detail), what it could not do and why, and the verdict. An item already as wanted says
/// nothing unless its refresh did something; a re-laid opt-out is named with the way back
/// out, never implied.
fn repair_lines(rows: &[aterm_converge::ItemReport]) -> Vec<String> {
    use aterm_converge::Outcome;
    let mut lines = Vec::new();
    let remedy = |row: &aterm_converge::ItemReport| match row.explain.as_ref().map(|e| &e.remedy) {
        Some(aterm_converge::Remedy::Command(command)) => format!(" — `{command}`"),
        _ => String::new(),
    };
    for row in rows {
        let said: Vec<String> = row
            .detail
            .iter()
            .flat_map(|detail| detail.lines())
            .map(|line| format!("repair: {line}"))
            .collect();
        match row.outcome {
            // An rc left alone (opted out, behind a consent fence, not a file atpkg edits)
            // is said with why, and how to wire it by hand.
            Outcome::Ok
                if row.id == crate::converge::repair::RC
                    && row
                        .to
                        .as_deref()
                        .is_some_and(|now| now != crate::converge::repair::RC_ALL_WIRED) =>
            {
                lines.extend(said);
                lines.extend(
                    row.to
                        .iter()
                        .flat_map(|now| now.split("; "))
                        .map(|left| format!("repair: {left}")),
                );
            }
            Outcome::Ok => lines.extend(said),
            Outcome::Changed if said.is_empty() => lines.push(format!(
                "repair: {}: {}",
                row.title,
                row.to.as_deref().unwrap_or("restored")
            )),
            Outcome::Changed => lines.extend(said),
            Outcome::Failed => {
                lines.extend(said);
                lines.push(format!(
                    "repair: {}: NOT done — {}{}",
                    row.title,
                    row.why.as_deref().unwrap_or("it failed"),
                    remedy(row)
                ));
            }
            Outcome::NotApplicable
                if [crate::converge::repair::RC, crate::converge::repair::STORE]
                    .contains(&row.id.as_str()) =>
            {
                lines.push(format!(
                    "repair: {}",
                    row.why.as_deref().unwrap_or_default()
                ));
            }
            Outcome::NotApplicable | Outcome::Deferred => {}
            Outcome::Blocked | Outcome::Planned | Outcome::Person | Outcome::Unknown => {
                lines.push(format!(
                    "repair: {}: {}{}",
                    row.title,
                    row.why.as_deref().unwrap_or("not done"),
                    remedy(row)
                ))
            }
        }
    }
    lines.push(if repair_exit(rows) == ExitCode::SUCCESS {
        "repair: done — `aterm pkg doctor` confirms".to_owned()
    } else {
        let left = rows.iter().filter(|row| repair_left_undone(row)).count();
        format!("repair: not done — {left} item(s) above still need attention")
    });
    lines
}

/// `repair`'s command-link step's report ([`crate::hooks::move_aside_copied_commands`]):
/// one line per copied command still in `~/.local/bin` after `outcome`, for a test that
/// drives both arms — a copy the move could not make, and every copy an atpkg that does not
/// lay the links leaves (each is the `base.shell.copied-commands` item's `Blocked` row).
#[cfg(all(test, unix))]
fn repair_copied_commands_with(
    outcome: Result<crate::hooks::AsideOutcome, &str>,
    home: Option<&std::path::Path>,
) -> Vec<String> {
    match outcome {
        Ok(outcome) => outcome
            .failed
            .iter()
            .map(|(path, _)| format!("{} is still a file", path.display()))
            .collect(),
        Err(_) => home
            .map(crate::hooks::copied_command_links)
            .unwrap_or_default()
            .iter()
            .map(|path| format!("{} is still a file", path.display()))
            .collect(),
    }
}

/// The store half of [`run_repair`] — the held update, the shims, the exec roots and the
/// builds, as items over `layout` with no home, so its exit code is testable against a
/// temp prefix without rewriting the rc wiring of the account running the tests.
/// `leftovers` stand for a copied command the shell half could not move.
#[cfg(all(test, unix))]
fn repair_store(layout: &crate::store::Layout, leftovers: &[String]) -> ExitCode {
    let rows = repair_store_rows(layout, &REPAIR_STORE_STEPS);
    repair_store_verdict(&rows, leftovers)
}

/// The store items [`repair_store`] runs, in order.
#[cfg(all(test, unix))]
const REPAIR_STORE_STEPS: [&str; 4] = [
    crate::converge::repair::HELD_UPDATE,
    crate::converge::repair::SHIMS,
    crate::converge::repair::EXEC_ROOTS,
    crate::converge::repair::STORE,
];

#[cfg(all(test, unix))]
fn repair_store_rows(
    layout: &crate::store::Layout,
    only: &[&str],
) -> Vec<aterm_converge::ItemReport> {
    let ground = std::sync::Arc::new(crate::converge::repair::Ground {
        layout: layout.clone(),
        home: None,
        exe: None,
        rustup_home: None,
        login_rc: None,
        pass: crate::converge::repair::Pass::Repair,
    });
    repair_rows(
        &ground,
        &mut aterm_converge::NoJournal,
        Some(only),
        aterm_converge::Scope::REPAIR,
    )
}

#[cfg(all(test, unix))]
fn repair_store_verdict(rows: &[aterm_converge::ItemReport], leftovers: &[String]) -> ExitCode {
    for line in repair_lines(rows) {
        println!("{line}");
    }
    if leftovers.is_empty() {
        repair_exit(rows)
    } else {
        println!("repair: not done — {} (see above)", leftovers.join("; "));
        ExitCode::from(1)
    }
}

/// [`repair_store`] with the exec-root operation's verdict supplied in place of the
/// `base.pkg.exec-roots` item, so the completion verdict is exercised even when an empty
/// store's stale roots cannot be reconciled.
#[cfg(all(test, unix))]
fn repair_store_reconciling(
    layout: &crate::store::Layout,
    leftovers: &[String],
    reconcile: impl FnOnce(&crate::store::Layout, crate::seam::Depth) -> bool,
) -> ExitCode {
    let mut rows = repair_store_rows(
        layout,
        &[
            crate::converge::repair::HELD_UPDATE,
            crate::converge::repair::SHIMS,
            crate::converge::repair::STORE,
        ],
    );
    if !reconcile(layout, crate::seam::Depth::Deep) {
        rows.push(aterm_converge::ItemReport {
            id: crate::converge::repair::EXEC_ROOTS.to_owned(),
            family: "pkg".to_owned(),
            title: "the trust build's exec root".to_owned(),
            outcome: aterm_converge::Outcome::Failed,
            from: None,
            to: None,
            why: Some("the exec root could not be put in order".to_owned()),
            revert: None,
            effect: aterm_converge::Effect::Now,
            detail: None,
            layer: Some(aterm_converge::Layer::Base),
            class: Some(aterm_converge::Class::Relay),
            explain: None,
            stood_aside: None,
            essential: false,
        });
    }
    repair_store_verdict(&rows, leftovers)
}

/// Re-lay every installed program's shims ([`crate::converge::repair::relay`]), printing
/// what it said. Returns how many programs were re-laid, the `program@build` of each whose
/// build is gone, of each left tombstoned, and of each whose shims could not be re-laid.
#[cfg(all(test, unix))]
fn relay_shims(layout: &crate::store::Layout) -> (usize, Vec<String>, Vec<String>, Vec<String>) {
    let plan = crate::converge::repair::shim_plan(layout);
    let relaid = crate::converge::repair::relay(layout);
    for note in &relaid.notes {
        println!("repair: {note}");
    }
    (relaid.relaid, plan.missing, plan.revoked, relaid.failed)
}

/// What [`relay_shims`] re-lays for `program` ([`crate::converge::repair::exposes`]).
#[cfg(all(test, unix))]
fn repair_exposes(
    layout: &crate::store::Layout,
    program: &str,
    build_dir: &std::path::Path,
) -> Vec<String> {
    crate::converge::repair::exposes(layout, program, build_dir)
}

/// Whether this machine has disabled `program`: its recorded row says `tombstoned:` or
/// `aborted:` (the update lane's rows for a build it revoked, or a group it aborted at),
/// or one of the tools it exposes is a tombstone shim on disk — the measurements `repair`
/// makes before it would re-lay a shim, read, never inferred from the pin.
fn program_disabled_here(
    layout: &crate::store::Layout,
    recorded: Option<&crate::Status>,
    program: &str,
) -> bool {
    // A tombstoned build is not counted as installed, so its exposes are unreadable; the
    // program's own name is then the tool that matters, as in `repair`.
    recorded
        .and_then(|s| s.programs.get(program))
        .is_some_and(|r| r.state.starts_with("tombstoned:") || r.state.starts_with("aborted:"))
        || crate::installed_exposes(layout, program)
            .unwrap_or_else(|| vec![program.to_string()])
            .iter()
            .any(|tool| is_tombstone_shim(layout, tool))
}

/// Whether `bin/<tool>` is a tombstone: a shim that forwards nowhere and carries the
/// notice `activate::install_tombstone_shim` writes.
pub(crate) fn is_tombstone_shim(layout: &crate::store::Layout, tool: &str) -> bool {
    let Some(tool) = crate::store::ToolName::new(tool) else {
        return false;
    };
    let shim = layout.shim(&tool);
    crate::platform::resolve_shim(&shim).is_none()
        && crate::metadata_io::read_bounded_regular_utf8(&shim, crate::platform::MAX_SHIM_BYTES)
            .is_ok_and(|body| body.contains(" was yanked/revoked"))
}

/// [`cmd_list`] under an already-resolved layout — split so the exit codes and both
/// renderings are exercisable against a temp store without a process (and without a TTY).
fn run_list(layout: Option<crate::store::Layout>, human: bool) -> ExitCode {
    // Exit 1, like every sibling read verb (`which`, `doctor`, `verify`): `layout()`
    // already printed why.
    let Some(layout) = layout else {
        return ExitCode::from(1);
    };
    match list_lines(&layout, human) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("{why}");
            ExitCode::from(1)
        }
    }
}

/// Every line [`run_list`] prints for `layout`, in either rendering. The human form ends
/// with the programs built into aterm ([`with_built_in_section`]); the piped form is the
/// store's alone, byte-identical to every earlier release.
///
/// # Errors
/// The development links cannot be listed (the line to print).
fn list_lines(layout: &crate::store::Layout, human: bool) -> Result<Vec<String>, String> {
    let installed = crate::list_installed(layout);
    // A program DEV-LINKED FROM THE START — never installed from the index, so it has no
    // store build for `list_installed` to find — is still a program this machine runs:
    // its shims are in the managed bin/ and `which` names them. Such programs get a row
    // with no build number.
    let mut linked = std::collections::BTreeMap::new();
    let programs = crate::linkmode::linked_programs_checked(layout)
        .map_err(|e| format!("atpkg: cannot list development links: {e}"))?;
    for program in programs {
        linked.insert(
            program.clone(),
            crate::linkmode::linked_checkout(layout, &program),
        );
    }
    if installed.is_empty() && linked.is_empty() {
        return Ok(if human {
            with_built_in_section(vec![EMPTY_LIST_HUMAN.to_string()])
        } else {
            // The stable piped form — byte-identical to every earlier release.
            vec!["atpkg: no programs installed".to_string()]
        });
    }
    let live = crate::active_builds(layout);
    let mut pinned: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for program in installed
        .iter()
        .map(|(program, _)| program)
        .chain(linked.keys())
    {
        if crate::pin::is_pinned(layout, program) {
            pinned.insert(program.clone());
        }
    }
    let linked_only: Vec<String> = linked
        .keys()
        .filter(|p| !installed.iter().any(|(q, _)| q == *p))
        .cloned()
        .collect();
    Ok(if human {
        with_built_in_section(list_human_lines(
            &installed,
            &linked_only,
            &live,
            &linked,
            &pinned,
        ))
    } else {
        list_porcelain_lines(&installed, &live, &linked, &pinned)
    })
}

/// `lines` (the human form) with the "built in" section ([`crate::builtin::list_human_lines`])
/// after the store's rows and ahead of a closing [`LIST_NEXT_DOCTOR`], which stays the last
/// line: the one next act names the rows above it, never these.
fn with_built_in_section(mut lines: Vec<String>) -> Vec<String> {
    let next = if lines.last().is_some_and(|l| l == LIST_NEXT_DOCTOR) {
        lines.pop()
    } else {
        None
    };
    lines.extend(crate::builtin::list_human_lines(
        aterm_types::version::app_version(),
    ));
    lines.extend(next);
    lines
}

/// The human `list`'s closing act, printed only when a row needs explaining.
const LIST_NEXT_DOCTOR: &str = "atpkg: next — aterm pkg doctor";

/// The empty store's human answer — the fact and, on the same line, the one command
/// that changes it (the no-dead-ends rule).
const EMPTY_LIST_HUMAN: &str =
    "atpkg: no programs installed — aterm pkg install --default-set installs the ALab toolset";

/// `update` on an empty, unadopted store — the fact keeps its stable head
/// (`atpkg: nothing installed to update`), the remedy rides after the em dash.
const EMPTY_UPDATE: &str = "atpkg: nothing installed to update — aterm pkg install \
                            --default-set installs the ALab toolset";

/// `verify` over an empty store — nothing was attested because nothing is installed,
/// and saying only that is a dead end; the order of acts matters (install, then verify).
const EMPTY_VERIFY: &str = "atpkg: nothing installed to verify (aterm pkg install \
                            --default-set installs the toolset)";

/// The stable script form: one row per installed build, `program\tbuild\tnotes`,
/// headerless, ascending builds (superseded rows before the live one) — pipes key on it. A
/// development link with no store build gets `-` in the build column, never an invented
/// build number.
///
/// The store can hold more than one build per program (live + 1 rollback); the notes carry
/// the three facts that change what runs — which build is live (`active_builds` reads the
/// shims), whether a dev link took the program over, and whether a pin holds it back.
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
            // A dev link supersedes every store build, so the note belongs on the program:
            // print it once, and never call a store build "live" while it is shadowed.
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
        lines.push(format!(
            "{program}\t{}\t{}",
            crate::vendor_direct::build_label(*build),
            notes.join("  ")
        ));
    }
    for (program, checkout) in linked {
        if shown_link.contains(program) {
            continue;
        }
        let mut note = match checkout {
            Some(path) => format!("DEV-LINKED -> {}", path.display()),
            None => "DEV-LINKED".to_string(),
        };
        if pinned.contains(program) {
            note.push_str("  pinned");
        }
        let row = format!("{program}\t-\t{note}");
        let insert = lines.partition_point(|line| {
            line.split('\t').next().unwrap_or_default() <= program.as_str()
        });
        lines.insert(insert, row);
    }
    lines
}

/// The human report: a count summary first, then one aligned row per program in its live
/// state, superseded builds folded into "(N older build(s) kept for rollback)". No health
/// adjective: counts are what the shims prove; "healthy" is doctor's word.
fn list_human_lines(
    installed: &[(String, u64)],
    linked_only: &[String],
    live: &std::collections::BTreeMap<String, u64>,
    linked: &std::collections::BTreeMap<String, Option<std::path::PathBuf>>,
    pinned: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    // Group the per-build rows per program, preserving list order (ascending builds).
    let mut builds: std::collections::BTreeMap<&str, Vec<u64>> = std::collections::BTreeMap::new();
    for (program, build) in installed {
        builds.entry(program).or_default().push(*build);
    }
    // A dev-linked program with no store build still takes a row: an empty build list.
    for program in linked.keys() {
        builds.entry(program).or_default();
    }
    let name_width = builds.keys().map(|p| p.len()).max().unwrap_or(0);
    let build_width = installed
        .iter()
        .map(|(_, b)| crate::vendor_direct::build_label(*b).len())
        .max()
        .unwrap_or(0)
        .max(usize::from(!linked_only.is_empty()));
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
            if program_builds.is_empty() {
                s.push_str("  (no store build — linked from the start)");
            } else {
                s.push_str(&format!(
                    "  ({} store build(s) shadowed)",
                    program_builds.len()
                ));
            }
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
                .map(|b| crate::vendor_direct::build_label(*b))
                .unwrap_or_default()
        };
        rows.push(format!(
            "  {program:<name_width$}  {shown_build:>build_width$}  {state}"
        ));
    }
    // The summary, zero-count categories omitted, so the clause that matters on an
    // unhealthy machine is not diluted.
    let mut clauses = vec![if linked.is_empty() {
        format!("{live_count} live")
    } else {
        format!("{live_count} store builds live")
    }];
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
        lines.push(LIST_NEXT_DOCTOR.to_string());
    }
    lines
}

/// Remove `program` from the store and retire what this machine recorded about it: the
/// status row (Settings ▸ Packages would list a deleted program forever; the stale-row
/// reconciler declines while the signed index names it) and the removed marker (else the
/// resumable seed lane reinstalls it on the next launch).
///
/// The intent is written before the delete: `ops::uninstall` takes the shims off first and
/// then spends seconds in `remove_dir_all`, and a kill inside it must not leave the program
/// gone with no `removed` entry. The name gates, the only failure that deletes nothing,
/// run first.
fn uninstall_and_retire(layout: &crate::store::Layout, program: &str) -> std::io::Result<()> {
    retirement_name_gate(program)?;
    record_removed(layout, program);
    // A removal on purpose is not a retirement in favour of a system copy: forget the
    // date so a later `system:` row never reports a retirement this act superseded.
    layout.clear_retired(program);
    // The rustup seam goes with the toolchain it presents: the view is a clone of the live
    // trust build, so `uninstall trust` would otherwise leave `~/.rustup/toolchains/trust`
    // running a ghost of the deleted compiler. Before the build trees go, as `--all` does.
    if program == "trust" {
        detach_rustup_seams(layout);
    }
    crate::uninstall(layout, program)?;
    // A held toolchain update whose every build-to-move-off went with this program waits
    // for nothing now; left, doctor would name it until a pass came ([`crate::quiet`]).
    let _ = crate::quiet::end_outlived(layout);
    // The row stays on a failed delete: the machine is in a half-state worth reporting
    // (the same choice `uninstall --all` makes).
    clear_status_row(layout, program);
    Ok(())
}

/// The names [`uninstall_and_retire`] refuses before it records or deletes anything.
/// `ops::uninstall`'s shape rule rejects only empty/`.`/`..`/separators/NUL, so without this
/// `uninstall '*toolset*'` would delete a bookkeeping row with its own owner and reaper,
/// and `uninstall --help` would append a flag to the removed-markers file. The shape rule
/// is asked here too: the removal intent is recorded before `ops::uninstall` could refuse.
fn retirement_name_gate(program: &str) -> std::io::Result<()> {
    if is_sentinel_row(program) || program.starts_with('-') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{program:?} is not a program name"),
        ));
    }
    crate::ops::uninstall_name_shape(program)
}

fn cmd_uninstall(program: Option<&String>) -> ExitCode {
    let Some(program) = program else {
        return usage_error("uninstall", None);
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
/// partial one) or a bin shim resolving into it — what `ops::uninstall` cleans. The record
/// row is not consulted: it is a claim about the store, not the store.
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
    // Refuse to "uninstall" what was never there: a success would mint a durable removed
    // marker that suppresses set-completion for a program the user never had. A
    // sentinel/flag name reaches the retirement gate's own refusal instead.
    if !(is_sentinel_row(program) || program.starts_with('-')) && !program_present(layout, program)
    {
        eprintln!("atpkg: {program} is not installed — nothing to uninstall");
        return ExitCode::from(1);
    }
    // Removing a managed program is an explicit act, so this machine stops keeping the whole
    // set complete, else the next unattended pass reinstalls it (`[packages].exclude` drops
    // one program while staying adopted). Before the delete a kill can interrupt, and only
    // for a name the retirement gate accepts.
    let opted_out = retirement_name_gate(program).is_ok() && adopted(layout);
    if opted_out {
        clear_adoption(layout);
    }
    match uninstall_and_retire(layout, program) {
        Ok(()) => {
            // A removed program's pending stub goes with it: `removed` suppresses the
            // reinstall, so a stub promising "installing" would be a standing lie.
            crate::stub::remove_stub(layout, program);
            println!(
                "atpkg: uninstalled {program} \u{2014} update passes leave it out; `aterm pkg \
                 install {program}` puts it back"
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: uninstall {program} failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// The parenthetical for a status row that names a copy this manager does not own (a
/// binary the user already had, `system: <path>`); `None` for every other row.
/// [`cmd_uninstall_all`] uses it to say what it left alone: system copies are never touched,
/// and the output says so (`docs/DESIGN-which-copy-runs-2026-08-27.md` §S11).
fn foreign_copy_note(state: &str) -> Option<String> {
    crate::state::system_path(state).map(|path| format!("(system copy: {path})"))
}

/// What one [`sweep_uninstall_targets`] pass did: the programs it actually removed, the
/// foreign copies it deliberately did not touch, and how many removals failed.
#[derive(Default)]
struct UninstallSweep {
    /// Names whose store tree or shims this pass really took off the machine.
    removed: Vec<String>,
    /// `<name> (system copy: <path>)` for a copy that is still exactly where it was.
    left_alone: Vec<String>,
    /// Removals that errored; each has already printed its own stderr line.
    failures: u32,
}

/// The programs [`cmd_uninstall_all`] sweeps: the union of what is live and what the record
/// still claims. A tombstoned program is invisible to `active_builds` (its shims are
/// failing stubs), so the live set alone would leave its tree and row behind. Sentinel
/// bookkeeping rows and stray flag rows are excluded.
fn uninstall_all_targets(layout: &crate::store::Layout) -> std::collections::BTreeSet<String> {
    let mut targets: std::collections::BTreeSet<String> =
        crate::active_builds(layout).keys().cloned().collect();
    if let Some(s) = crate::status::read(layout) {
        targets.extend(
            s.programs
                .keys()
                .filter(|k| !is_sentinel_row(k) && !k.starts_with('-'))
                .cloned(),
        );
    }
    targets
}

/// Remove every target this manager actually installed, and report the rest. Rows exist
/// for names atpkg never installed (`system: … — not managed by aterm`,
/// `unavailable on <target>`, `agent program — installing`), for which `ops::uninstall`
/// deletes nothing and returns `Ok(())`; counted as removals they would be reported removed
/// and their rows wiped. Presence on disk ([`program_present`]) is what makes a name this
/// verb's to claim.
fn sweep_uninstall_targets(
    layout: &crate::store::Layout,
    targets: &std::collections::BTreeSet<String>,
) -> UninstallSweep {
    // The rows as they stood before the sweep: a row is what says whether this manager
    // ever owned a copy, and `clear_status_row` rewrites the record as we go.
    let rows = crate::status::read(layout)
        .map(|s| s.programs)
        .unwrap_or_default();
    let mut sweep = UninstallSweep::default();
    for program in targets {
        let state = rows.get(program).map(|r| r.state.as_str()).unwrap_or("");
        if !program_present(layout, program) {
            match foreign_copy_note(state) {
                // A copy that is still there and was never ours: name it and keep its row,
                // the true answer about this machine (`which` and the next pass read it).
                Some(note) => sweep.left_alone.push(format!("{program} {note}")),
                // A row with nothing behind it (a member with no build for this target, a
                // managed row that outlived its program) still goes, but is not reported
                // as a removal.
                None => clear_status_row(layout, program),
            }
            continue;
        }
        match crate::uninstall(layout, program) {
            Ok(()) => {
                // Same retirement as the single-program verb. Only on success: `uninstall`
                // removes the shims first, so an `Err` (from `remove_dir_all`) leaves the
                // program off PATH with its tree behind — a half-state the row keeps
                // reporting.
                clear_status_row(layout, program);
                sweep.removed.push(program.clone());
            }
            Err(e) => {
                sweep.failures += 1;
                eprintln!("atpkg: uninstall {program} failed: {e}");
            }
        }
    }
    sweep
}

/// Name the copies the sweep did not touch, or print nothing when there were none.
fn print_left_alone(left_alone: &[String]) {
    if left_alone.is_empty() {
        return;
    }
    println!(
        "atpkg: left alone — not installed by aterm, so nothing was removed: {}",
        left_alone.join(", ")
    );
}

/// `atpkg uninstall --all` — remove the whole managed toolset and reclaim its disk. The
/// default install lays down gigabytes as part of installing the app, so the way out is as
/// single-step as the way in.
///
/// Composed from the per-program primitive plus a GC sweep rather than an `rm -rf` of the
/// prefix: `[packages].prefix` is user-configurable, and a recursive delete of a path from
/// config must never exist. Adoption is cleared last, so the next pass does not put it all
/// back.
fn cmd_uninstall_all() -> ExitCode {
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let targets = uninstall_all_targets(&layout);
    // The seam goes first, while every recorded rustup entry still resolves into the
    // prefix — before the branch, so an empty store drops its record too.
    detach_rustup_seams(&layout);
    if targets.is_empty() {
        // Record the decline anyway: an Intel/lean Mac's store is always empty, and a launch
        // may come before the seed pass ran; without it the next launch re-adopts and
        // installs the whole set.
        clear_adoption(&layout);
        record_decline(&layout);
        layout.clear_all_retired();
        // A held toolchain update recorded over a store that is empty now waits for
        // nothing, and a declined machine runs no pass that would end it.
        let _ = crate::quiet::end_outlived(&layout);
        // The decline extends to the pending stubs: a declined machine keeps no
        // default-set names on PATH promising an install that will never come.
        crate::stub::remove_all_stubs(&layout);
        crate::reroute::remove_all(&layout);
        remove_exec_roots(&layout);
        println!(
            "atpkg: nothing to remove — updates no longer install the ALab toolset; `aterm \
             pkg install --default-set` puts it back"
        );
        return ExitCode::SUCCESS;
    }
    let UninstallSweep {
        mut removed,
        left_alone,
        failures,
    } = sweep_uninstall_targets(&layout, &targets);
    // Reclaim the now-unreferenced build trees — this is where the gigabytes
    // actually go back, and reporting it is the point of the verb.
    let report = crate::gc::run(&layout, last_verified_channel(&layout).as_ref());
    print_gc_sweeps("uninstall --all", &report);
    print_gc_abstentions("uninstall --all", &report);
    // Provisions nothing, so it gives no shell an rc it lacks.
    crate::hooks::refresh_creating_no_rc(&layout);
    // Last, after the removals: neither set-completion nor the seed lane may reinstate the
    // set. The seed installs whatever the store lacks (what makes an interrupted first run
    // resumable), so only a durable decline stops it.
    clear_adoption(&layout);
    record_decline(&layout);
    layout.clear_all_retired();
    // The held toolchain update's record goes with the builds it would have moved off.
    let _ = crate::quiet::end_outlived(&layout);
    // The pending stubs go with the declined toolset, as on the empty-store branch.
    crate::stub::remove_all_stubs(&layout);
    crate::reroute::remove_all(&layout);
    remove_exec_roots(&layout);
    if removed.is_empty() {
        // Nothing of ours was here (every row names a system copy): the decline just
        // recorded is what was asked for, so this is a success with an honest report
        // (`tools/install.sh` reads the exit code).
        if failures == 0 {
            println!(
                "atpkg: nothing to remove (nothing here was installed by aterm) — updates no \
                 longer install the ALab toolset; `aterm pkg install --default-set` puts it \
                 back"
            );
            print_left_alone(&left_alone);
            return ExitCode::SUCCESS;
        }
        // The decline above still landed: say so beside the failures already printed.
        eprintln!(
            "atpkg: removed nothing (each removal failed, above) — updates no longer install \
             the ALab toolset"
        );
        return ExitCode::from(1);
    }
    removed.sort();
    println!(
        "atpkg: removed {} — updates no longer install the ALab toolset; `aterm pkg install \
         --default-set` puts it back (the aterm app is untouched)",
        removed.join(", ")
    );
    print_left_alone(&left_alone);
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `uninstall --all`'s exec-root half, on both branches: remove `<prefix>/compat` whole
/// ([`crate::compat::remove_all`]) — the roots a failed removal or an older client left,
/// since a declined machine keeps no clone of builds it no longer has. A failure is one
/// stderr line.
fn remove_exec_roots(layout: &crate::store::Layout) {
    if let Err(e) = crate::compat::remove_all(layout) {
        eprintln!(
            "atpkg: warn — {} was not removed ({e}); `aterm pkg gc` sweeps any trust exec \
             root left in it",
            crate::compat::compat_dir(layout).display()
        );
    }
}

/// Name the programs a GC pass abstained on, or print nothing when it abstained on none: a
/// program whose two views disagree is skipped and its superseded builds stay on disk, and
/// "nothing to reclaim" alone would hide that `doctor` knows why. Shared: every verb that
/// runs a GC pass owes the same disclosure.
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
/// rollback target), and sweep what an interrupted install left behind. No network.
///
/// The outcomes are printed apart because they are different claims about where the disk
/// went: a reclaim retired a build that really was installed; a sweep removed a tree (or
/// its stage scratch) that never finished installing. The build kept below each live one
/// is the one `aterm pkg rollback` would land on under the channel this machine last
/// verified ([`last_verified_channel`]: the cache, offline).
fn cmd_gc() -> ExitCode {
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let report = crate::gc::run(&layout, last_verified_channel(&layout).as_ref());
    if gc_freed_nothing(&report) {
        println!("atpkg gc: nothing to reclaim");
    } else {
        for (p, builds) in &report.reclaimed {
            println!("atpkg gc: reclaimed {}", gc_builds_words(p, builds));
        }
    }
    print_gc_sweeps("gc", &report);
    print_gc_abstentions("gc", &report);
    ExitCode::SUCCESS
}

/// Whether a GC pass removed nothing at all — the one case `gc` says "nothing to reclaim".
fn gc_freed_nothing(report: &crate::gc::GcReport) -> bool {
    report.reclaimed.is_empty()
        && report.swept_partial.is_empty()
        && report.swept_scratch.is_empty()
        && report.swept_staging.is_empty()
        && report.swept_exec_roots.is_empty()
}

/// Disclose what a GC pass swept — the trees and stage scratch an interrupted install left
/// behind. Shared because `gc::run` also runs after an install, an update and a grouped
/// update, which delete as much as the explicit verb does.
fn print_gc_sweeps(verb: &str, report: &crate::gc::GcReport) {
    for line in gc_sweep_lines(verb, report) {
        println!("{line}");
    }
    for e in &report.exec_root_errors {
        eprintln!("atpkg {verb}: warn — {e}");
    }
}

/// [`print_gc_sweeps`]'s stdout lines.
fn gc_sweep_lines(verb: &str, report: &crate::gc::GcReport) -> Vec<String> {
    let mut lines = Vec::new();
    for (p, builds) in &report.swept_partial {
        lines.push(format!(
            "atpkg {verb}: removed unfinished install(s) of {}",
            gc_builds_words(p, builds)
        ));
    }
    for (p, names) in &report.swept_scratch {
        let mut named: Vec<String> = names.iter().map(|n| scratch_words(n)).collect();
        named.dedup();
        lines.push(format!(
            "atpkg {verb}: removed leftovers of an interrupted {p} install ({})",
            named.join(", ")
        ));
    }
    // Say it: a killed download strands a multi-hundred-MB archive in `staging/`.
    for (p, names) in &report.swept_staging {
        lines.push(format!(
            "atpkg {verb}: removed leftover download(s) of {p}: {}",
            names.join(", ")
        ));
    }
    // An exec root is a clone of its build's files, outside `store/`: one left behind
    // keeps a reclaimed build's blocks allocated, so where they went back is said too.
    for path in &report.swept_exec_roots {
        lines.push(format!(
            "atpkg {verb}: removed unused build copy {}",
            path.display()
        ));
    }
    // A build kept for a run's lease is disk the owner may wonder about: whose it is, and
    // that it goes on its own once the run ends ([`crate::lease`]).
    for (p, build, holders) in &report.leased {
        lines.push(format!(
            "atpkg {verb}: kept {} \u{2014} {}; the first pass after that run reclaims it",
            gc_builds_words(p, &[*build]),
            holders.clause()
        ));
    }
    // An archive kept for a held toolchain update is gigabytes with one use left: say
    // which, and the one command that spends it now ([`crate::quiet::held_archives`]).
    for (p, asset) in &report.held_staging {
        lines.push(format!(
            "atpkg {verb}: kept staged {p} archive {asset} \u{2014} the held Trust toolchain \
             update installs from it once nothing is using the toolchain; `aterm pkg update` \
             installs it now"
        ));
    }
    lines
}

/// A stage-scratch directory as the sweep line names it (`2.1.281`, `build 18`), any other
/// name verbatim.
fn scratch_words(name: &str) -> String {
    crate::store::stage_scratch_of(name).map_or_else(
        || name.to_string(),
        |(build, _)| crate::vendor_direct::build_words(build),
    )
}

/// `program`'s builds as the gc lines name them: `claude 2.1.279, 2.1.278` when all are
/// vendor builds, else `trust build(s) 8595, 8594`.
fn gc_builds_words(program: &str, builds: &[u64]) -> String {
    let labels: Vec<String> = builds
        .iter()
        .map(|b| crate::vendor_direct::build_label(*b))
        .collect();
    if !builds.is_empty()
        && builds
            .iter()
            .all(|b| crate::vendor_direct::is_vendor_build(*b))
    {
        format!("{program} {}", labels.join(", "))
    } else {
        format!("{program} build(s) {}", labels.join(", "))
    }
}

/// `atpkg tree-root <dir>` — print the SHA-256 tree_root of an extracted directory, the
/// value the publish pipeline writes into a per-build manifest's `tree_root`: the same
/// hashing the client re-verifies with.
fn cmd_tree_root(dir: Option<&String>) -> ExitCode {
    let Some(dir) = dir else {
        return usage_error("tree-root", None);
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

/// The target triple of this build, for artifact selection.
pub(crate) fn current_triple() -> &'static str {
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
    // Without the Windows arms a Windows build reports "unknown" and selects no artifact.
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

/// RFC3339 UTC timestamp for the status record, now ([`rfc3339_at`]).
fn now_rfc3339() -> String {
    rfc3339_at(now_unix())
}

/// RFC3339 UTC timestamp of the epoch second `secs`, in pure Rust (no `/bin/date`, which
/// Windows lacks), via `aterm_types::rfc3339`, the inverse of `flow::rfc3339_to_unix`.
/// Empty on a pre-epoch (or exactly-epoch) clock.
fn rfc3339_at(secs: i64) -> String {
    // Both ends mean an unusable clock: `now_unix` returns `i64::MAX` when it cannot be read
    // (the freshness gates' fail-closed sentinel), and that would stamp a far-future year
    // doctor's "publishing looks frozen" check reads as fresh.
    if secs <= 0 || secs == i64::MAX {
        return String::new();
    }
    // secs > 0, so the cast is lossless.
    aterm_types::rfc3339::format_rfc3339(secs as u64)
}

/// A caller's already-verified index, offered to a second lane of the same pass while its
/// signed freshness window is still open at `now`. `atpkg update` runs several lanes over
/// one store, and each resolve would re-run every signature verify and roster admit for an
/// answer that cannot differ inside one pass, except freshness, a function of the clock:
/// an index whose window lapsed during a long pass is refused and meets the same `Stale`
/// refusal in that lane's own resolve.
fn reusable_index(index: Option<&crate::TrustedIndex>, now: i64) -> Option<&crate::TrustedIndex> {
    index.filter(|index| crate::flow::index_is_fresh(index, now))
}

/// Re-record the signed `tree_root` for any program that is live but unattested, and prune
/// stale rows. Silent and best-effort: a root that cannot be re-derived is left as it was
/// (fail-closed), and nothing here fails an update. `trusted` is the caller's verified
/// index when it has one ([`reusable_index`]); `None` resolves one here.
fn recover_missing_roots(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    trusted: Option<&crate::TrustedIndex>,
) {
    let live = crate::active_builds(layout);
    if live.is_empty() {
        return;
    }
    let recorded = crate::status::read(layout)
        .map(|s| s.programs)
        .unwrap_or_default();
    // A vendor-direct build is attested by its `.vendor` record (or, rolled back to its
    // legacy build, the root its stamp kept), never by an index manifest.
    let unattested: Vec<(String, u64)> = live
        .iter()
        .filter(|(program, _)| !crate::vendor_direct::is_vendor(program))
        // …and one whose recorded root is for another build: a build that went live without
        // its row (a roll-forward, a row a killed pass never wrote) has no attestation yet.
        .filter(|(program, build)| {
            recorded
                .get(program.as_str())
                .is_none_or(|row| row.tree_root.is_empty() || row.installed_build != Some(**build))
        })
        .map(|(program, build)| (program.clone(), *build))
        .collect();
    // Cheap pre-filter for the second job below (stale rows), before the index resolve, so
    // a healthy store with nothing to do stays free.
    let maybe_stale = recorded
        .keys()
        .any(|k| !is_sentinel_row(k) && !live.contains_key(k.as_str()));
    if unattested.is_empty() && !maybe_stale {
        return;
    }
    // The pass's own index when it has one and its window is still open, else one
    // resolved here — never a second verify-select over the same cache in one pass.
    let resolved;
    let index = match reusable_index(trusted, now_unix()) {
        Some(index) => index,
        None => {
            let Ok(index) = crate::resolve_verified_index(
                fetcher,
                layout,
                &effective_anchor(layout),
                build_floor(layout),
                now_unix(),
            ) else {
                return;
            };
            resolved = index;
            &resolved
        }
    };
    // Reconcile the memo against its authority: a row naming something the verified index
    // does not name and the store does not carry is a stray that doctor would report as an
    // incomplete toolset for good. Announced: a manager never quietly edits its record.
    let named = |n: &str| index.is_program(n) || crate::vendor_direct::is_vendor(n);
    for stale in prunable_status_rows(&recorded, &live, &named) {
        println!("atpkg: cleared a stale record for {stale:?} — the signed index does not name it");
        clear_status_row(layout, &stale);
    }
    for (program, build) in unattested {
        let Some(root) = crate::flow::signed_root_for_installed(
            fetcher,
            index,
            &program,
            build,
            current_triple(),
        ) else {
            continue;
        };
        println!("atpkg: {program} was installed without its signed attestation — recorded");
        // A prior managed row for this very build keeps its words (`rolled back from <N>`
        // above all), so re-deriving a root never re-mints a pin the index does not hold.
        let state = recorded
            .get(program.as_str())
            .filter(|r| r.installed_build == Some(build) && crate::state::is_managed(&r.state))
            .map_or_else(
                || crate::state::managed(build, index.index_build),
                |r| r.state.clone(),
            );
        record_status(
            layout,
            &program,
            crate::ProgramStatus {
                installed_build: Some(build),
                state,
                tree_root: root,
            },
            format!("recovered the signed attestation for {program}"),
        );
    }
}

/// The durable `state` to record for a failed install, or `None` when the failure is about
/// the request or the network rather than this machine's store. `NotReachable` means the
/// index does not name the token, so a row would invent a missing member. The offline
/// family (`NoIndex`, `Unreachable`, `Stale`) is raised before any program lookup: an
/// `error:` row would blame the program for the machine being offline, and nothing would
/// sweep it. The failure still reaches the outcome sentence, and `cmd_install` exits 1.
fn failed_install_state(e: &crate::FlowError) -> Option<String> {
    if let Some(state) = canonical_non_error_state(e) {
        return Some(state);
    }
    match e {
        // A dev-linked program is a benign hard skip, not an error state.
        crate::FlowError::Linked(_) => Some(String::from("dev-linked (skipped)")),
        // Verdicts about the request or the network — never about this program.
        crate::FlowError::NotReachable(..)
        | crate::FlowError::NoIndex
        | crate::FlowError::Unreachable(_)
        | crate::FlowError::Stale => None,
        // The vendor lane's own stamp keeps its verdict; the program's row stays as it is.
        crate::FlowError::Vendor(_) | crate::FlowError::VendorUnreachable(_) => None,
        _ => Some(format!("error: {e}")),
    }
}

/// The canonical, non-error state some install refusals really are: a target the pinned
/// build carries no row for is `unavailable on <target>: <hint>` (the generic hint; the
/// pass's missing-triple prescan records the program's own `unavailable_hint` ahead of
/// this). `None` for every genuine fault.
fn canonical_non_error_state(e: &crate::FlowError) -> Option<String> {
    match e {
        crate::FlowError::NoArtifact(t) => Some(crate::state::unavailable(t, "")),
        // …and the vendor lane's, with its vendor's own hint.
        crate::FlowError::VendorUnavailable(state) => Some(state.clone()),
        // A member the digest-refusal memo held: the flow already rendered the canonical
        // `held: …` row ([`crate::state::held_failed`]); an `error:` head would claim this
        // pass staged something.
        crate::FlowError::StageRefused(row) => Some(row.clone()),
        _ => None,
    }
}

/// The recorded rows naming nothing the signed index knows — the ones safe to drop.
/// `status.toml` is a derived memo and the signed index the authority on what programs
/// exist. All four required:
///
/// * the caller holds a verified index — never prune on a network error;
/// * the key is not a `*sentinel*` bookkeeping row (`*index*`, `*toolset*`, `*seed*` have
///   their own owners and reapers);
/// * the index does not name it;
/// * it is not live: [`crate::verify`] fails closed on an empty recorded root, so deleting
///   a live program's row would erase the attestation `atpkg verify` needs.
fn prunable_status_rows(
    recorded: &std::collections::BTreeMap<String, crate::ProgramStatus>,
    live: &std::collections::BTreeMap<String, u64>,
    known: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    recorded
        .keys()
        .filter(|k| !is_sentinel_row(k))
        .filter(|k| !live.contains_key(k.as_str()))
        .filter(|k| !known(k))
        .cloned()
        .collect()
}

/// The seed for a writer that rebuilds `status.toml` — `None` means the record on disk
/// exists and could not be read (and, when corrupt, could not be kept aside): do not write.
///
/// A corrupt record is healed by [`crate::status::seed_for_rewrite`]: kept as
/// `status.toml.corrupt-<unix>` and rebuilt from the store. A read that failed otherwise (a
/// permission, an I/O error) leaves a possibly healthy record alone. The line is printed at
/// most once per process: a pass calls these writers once per program.
fn status_seed_for_rewrite(layout: &crate::store::Layout) -> Option<crate::Status> {
    match crate::status::seed_for_rewrite(layout) {
        Ok(seed) => Some(seed),
        Err(e) => {
            use std::sync::atomic::{AtomicBool, Ordering};
            static SAID: AtomicBool = AtomicBool::new(false);
            if !SAID.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "atpkg: {} could not be read ({e}) — left as it is: this pass records \
                     nothing there, and the next one reads it again",
                    layout.status().display()
                );
            }
            None
        }
    }
}

/// Replace the aggregate `status.outcome` sentence without touching per-program rows — the
/// line Settings ▸ Packages shows, which must not outlive a retired failure row.
fn record_outcome(layout: &crate::store::Layout, outcome: String) {
    // Create the record if it does not exist yet: environmental failures route here alone,
    // and a first-run offline `atpkg install` must leave this witness for the doctor
    // `tools/install.sh` runs on a failed seed.
    let Some(existing) = status_seed_for_rewrite(layout) else {
        return;
    };
    note_outcome_written();
    let _ = crate::status::write(
        layout,
        &crate::Status {
            schema: 1,
            enabled: manager_enabled(),
            index_source: crate::resolve_account(crate::config::cached().account()).slug(),
            outcome,
            // The timestamp moves with the sentence: Settings and `atpkg doctor` show the
            // two side by side.
            updated_at: now_rfc3339(),
            ..existing
        },
    );
}

/// Remove one row from `status.toml` — the counterpart to [`record_status`] for a failure
/// a later pass disproved (`*index*`, `*toolset*` would otherwise stand). Best-effort and
/// silent, like the writer.
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

/// The signed `tree_root` to persist for `program`'s row naming `build`: the freshly-applied
/// one when non-empty, else the recorded root, but only when the recorded row is for that
/// same build. An `already_current` install or an untouched coherence-group sibling reports
/// an empty root, and persisting it would erase a good attestation. A root recorded for
/// another build (one that went live without its row) is no attestation of this one: empty
/// then, and the pass's root recovery re-derives it. A row with no build keeps its root.
fn effective_tree_root(
    layout: &crate::store::Layout,
    program: &str,
    build: Option<u64>,
    new_root: &str,
) -> String {
    let recorded = new_root
        .is_empty()
        .then(|| crate::status::read(layout))
        .flatten();
    tree_root_for_row(
        recorded
            .as_ref()
            .and_then(|status| status.programs.get(program)),
        build,
        new_root,
    )
}

/// A fresh signed root wins; otherwise retain only this build's attestation.
/// A row naming no active build keeps its recorded root, as tombstones do.
fn tree_root_for_row(
    recorded: Option<&crate::ProgramStatus>,
    build: Option<u64>,
    new_root: &str,
) -> String {
    if !new_root.is_empty() {
        return new_root.to_string();
    }
    recorded
        .filter(|row| build.is_none() || row.installed_build == build)
        .map(|row| row.tree_root.clone())
        .unwrap_or_default()
}

/// The recorded row's `tree_root` when that row is for `build`, else empty: an
/// attestation belongs to the build it was captured for ([`failure_row`]).
fn recorded_root_of(layout: &crate::store::Layout, program: &str, build: u64) -> String {
    effective_tree_root(layout, program, Some(build), "")
}

/// A vendor program's legacy index `build`'s signed root: the recorded row's when it is for
/// that build, else the one its stamp kept ([`keep_replaced_legacy_root`]) — never an index.
fn legacy_root_of(layout: &crate::store::Layout, program: &str, build: u64) -> String {
    let recorded = recorded_root_of(layout, program, build);
    if !recorded.is_empty() {
        return recorded;
    }
    crate::vendor_direct::ProgramStamp::read(layout, program)
        .and_then(|s| s.legacy_root_of(build).map(str::to_string))
        .unwrap_or_default()
}

/// Keep, once, in the program's stamp, the signed root of a legacy index build whose row a
/// vendor install is about to replace (gc keeps that build for a rollback).
fn keep_replaced_legacy_root(
    layout: &crate::store::Layout,
    program: &str,
    row: Option<&crate::ProgramStatus>,
) {
    let Some((build, root)) = row.and_then(|r| {
        r.installed_build
            .filter(|b| !crate::vendor_direct::is_vendor_build(*b) && !r.tree_root.is_empty())
            .map(|b| (b, r.tree_root.as_str()))
    }) else {
        return;
    };
    let kept = crate::vendor_direct::ProgramStamp::read(layout, program);
    if kept.is_some_and(|s| {
        s.legacy_root_of(build)
            .is_some_and(|k| k.eq_ignore_ascii_case(root))
    }) {
        return;
    }
    let _ = crate::vendor_direct::ProgramStamp::record_legacy_root(layout, program, build, root);
}

/// The row to record when an install fails for a program that may already be live. A
/// failed install uninstalls nothing, so the row keeps the live build (`active_builds`, the
/// shims, not the record).
///
/// The root is kept only when record and store name the same build, or the store names
/// none (a tombstoned program, its bytes untouched): the live build beside another build's
/// root would disarm `crate::verify`'s `BuildMismatch` guard (a fail-open on authenticity).
/// Otherwise empty, and `verify` fails closed until the pass's root recovery re-derives it.
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

// Per thread, like the resolve's provenance ([`crate::flow::last_resolve`]) — a pass is one
// thread: how many outcome sentences it has written. Every writer of a new `outcome` counts
// one ([`note_outcome_written`]); a writer that carries the sentence it read does not. The
// package log's `pass-end` reads it ([`PassRecord::outcome`]).
thread_local! {
    static OUTCOMES_WRITTEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Count one outcome sentence this pass wrote ([`OUTCOMES_WRITTEN`]).
fn note_outcome_written() {
    OUTCOMES_WRITTEN.with(|n| n.set(n.get().wrapping_add(1)));
}

/// How many outcome sentences this thread has written so far.
fn outcomes_written() -> u64 {
    OUTCOMES_WRITTEN.with(std::cell::Cell::get)
}

// Per thread, like OUTCOMES_WRITTEN: how many successes this thread has stamped
// ([`record_pass_end`]) — whether a full pass stamped one is its recorded outcome.
thread_local! {
    static SUCCESSES_STAMPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Whether this full pass called a signed index resolve: without one, the window's hint
    /// is never recorded as an attempted index.
    static PASS_INDEX_RESOLVE_ATTEMPTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The highest index this pass actually resolved. Reset at each full pass
    /// start; the window's requested build is folded in only at pass end.
    static RESOLVED_PASS_INDEX_BUILD: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many successes this thread has stamped so far.
fn successes_stamped() -> u64 {
    SUCCESSES_STAMPED.with(std::cell::Cell::get)
}

/// Record the install outcome to `status.toml`, the silent manager's observability surface.
/// Best-effort: diagnostics, never load-bearing. The sentence is this pass's verdict
/// ([`note_outcome_written`]).
fn record_status(
    layout: &crate::store::Layout,
    program: &str,
    state: crate::ProgramStatus,
    outcome: String,
) {
    write_program_row(layout, program, state, Some(outcome));
}

/// [`record_status`] for a row with no installed build and no root: a sentinel row
/// (`*index*`, `*toolset*`, `*links*`) or a program this pass could not build here.
fn record_unbuilt(
    layout: &crate::store::Layout,
    program: &str,
    state: impl Into<String>,
    outcome: String,
) {
    record_status(
        layout,
        program,
        crate::ProgramStatus {
            installed_build: None,
            state: state.into(),
            tree_root: String::new(),
        },
        outcome,
    );
}

/// The durable `*index*` row of an update pass whose signed index did not resolve.
fn record_index_error(layout: &crate::store::Layout, e: impl std::fmt::Display) {
    record_unbuilt(
        layout,
        "*index*",
        format!("error: {e}"),
        format!("update failed: {e}"),
    );
}

/// Whether `name` is a bookkeeping row (`*index*`, `*toolset*`, …) rather than a program.
fn is_sentinel_row(name: &str) -> bool {
    name.starts_with('*') && name.ends_with('*')
}

/// [`record_status`]'s writer: `outcome` `None` carries the record's own sentence
/// ([`record_row_keeping_outcome`]), and is no outcome of this pass.
fn write_program_row(
    layout: &crate::store::Layout,
    program: &str,
    state: crate::ProgramStatus,
    outcome: Option<String>,
) {
    write_program_rows(layout, [(program.to_string(), state)], outcome);
}

/// [`write_program_row`] for several rows in one write: one fsync, rename and directory sync
/// for all of them.
fn write_program_rows(
    layout: &crate::store::Layout,
    rows: impl IntoIterator<Item = (String, crate::ProgramStatus)>,
    outcome: Option<String>,
) {
    // Seed the per-program map (and the seam record) from the EXISTING record so
    // updating ONE program does not clobber every OTHER program's last-known state.
    // The silent update loop calls this once per program, and status.toml is the
    // per-program observability surface (§5/§9) — a fresh single-entry map would
    // erase the rest each pass, and dropping `seams` would silently disown the
    // rustup seam ([`crate::seam`]) on every install.
    // `last_success_at` and the pass record are carried, never stamped: they say what a
    // whole pass did ([`record_pass_end`] is their one writer), and a row says what one
    // program did. The index build rides the same way
    // ([`crate::status::stamp_index_build`]). A record that EXISTS but cannot be read is
    // kept aside and rebuilt from the store first ([`crate::status::seed_for_rewrite`]) —
    // never replaced blind.
    let Some(mut status) = status_seed_for_rewrite(layout) else {
        return;
    };
    status.programs.extend(rows);
    // Every other field — the seams, the stamps, and the keys another build wrote
    // (`Status::extra`) — rides through as read.
    status.schema = 1;
    status.updated_at = now_rfc3339();
    status.enabled = manager_enabled();
    status.index_source = crate::resolve_account(crate::config::cached().account()).slug();
    if let Some(outcome) = outcome {
        note_outcome_written();
        status.outcome = outcome;
    }
    let _ = crate::status::write(layout, &status);
}

/// The success stamp's one writer (the derived model `AtpkgPassStamps`; its readers are
/// `aterm_update_core::pkg_check`'s): a pass that verified an index the channel served this
/// pass (`reached`: [`crate::flow::live_verifications`] moved) stamps `last_success_at`,
/// counted in [`successes_stamped`], which [`run_recorded_update_pass`] reads as the pass's
/// end. Called once, at the end, by `update` ([`finish_update_pass`]) and `install
/// --default-set`, never by a per-program writer. `now` is the pass's clock (`PassSeams::now`).
fn record_pass_end(layout: &crate::store::Layout, now: i64, reached: bool) {
    if !reached {
        return;
    }
    SUCCESSES_STAMPED.with(|n| n.set(n.get().wrapping_add(1)));
    let _ = crate::status::stamp_success(layout, &rfc3339_at(now));
}

/// A full `update` pass's end: its one stamp ([`record_pass_end`]) and the exit code that
/// agrees with it. A pass that reached the index exits `code` (0 clean, 1 when a member
/// failed, 2 when nothing is installable here). One that did not exits non-zero whatever
/// its members did — [`aterm_update_core::pkg_check::PASS_OFFLINE_EXIT`] when nothing
/// answered, else 1 — so the window never reads a pass on the cache as a check that ran.
///
/// The one ending that fails only for its index (not reached, not offline, `code` 0, so
/// nothing on stderr) is noted in [`PASS_FAILED_ONLY_FOR_ITS_INDEX`], which
/// [`run_recorded_update_pass`] records ([`crate::status::Status::stale_index_only_pass_seq`]).
fn finish_update_pass(
    layout: &crate::store::Layout,
    now: i64,
    reached: bool,
    offline: bool,
    code: u8,
) -> u8 {
    record_pass_end(layout, now, reached);
    match (reached, offline) {
        (true, _) => code,
        (false, true) => aterm_update_core::pkg_check::PASS_OFFLINE_EXIT,
        (false, false) => {
            PASS_FAILED_ONLY_FOR_ITS_INDEX.with(|only| only.set(code == 0));
            1
        }
    }
}

thread_local! {
    /// Whether this thread's last full pass ended in [`finish_update_pass`]'s "failed only
    /// for its index" arm (cached index, a host answered, no member failed: exit 1, nothing
    /// on stderr); reset and recorded by [`run_recorded_update_pass`]. The doc sits inside
    /// the macro: on the invocation it would be `unused_doc_comments`.
    static PASS_FAILED_ONLY_FOR_ITS_INDEX: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Stamp the index build this pass resolved (the publisher's pulse `doctor` reads), and —
/// when its resolve did not reach the channel — say so in the aggregate outcome, so a pass
/// served from the cache never records a bare green "up to date". `index_build` is the
/// build the pass resolved, when it has one in hand.
fn record_index_freshness(layout: &crate::store::Layout, now: i64, index_build: Option<u64>) {
    let Some(resolve) = crate::flow::last_resolve() else {
        return;
    };
    if let Some(build) = index_build {
        RESOLVED_PASS_INDEX_BUILD.with(|seen| seen.set(seen.get().max(build)));
    }
    let _ = crate::status::stamp_index_build(layout, &rfc3339_at(now), index_build);
    if !resolve.reached {
        let why = resolve
            .why
            .as_deref()
            .map(clip_cause)
            .unwrap_or_else(|| String::from("the index was not reached"));
        let Some(mut status) = status_seed_for_rewrite(layout) else {
            return;
        };
        if !status.outcome.contains("index from cache") {
            note_outcome_written();
            status.outcome = format!(
                "{} {}{why})",
                status.outcome,
                crate::status::INDEX_FROM_CACHE_CLAUSE
            );
            let _ = crate::status::write(layout, &status);
        }
    }
}

/// The production GitHub fetcher: the `[packages].account`-aware owner (a development
/// setting, [`crate::resolve_account`]), the `[packages.links]` per-program `owner/repo`
/// overrides ([`crate::repo_overrides`]), and the store's verified index floor, which its
/// discovery walk counts from. It holds no credential: every byte comes from the release
/// download host, so an overridden destination must be publicly readable.
fn github_fetcher(layout: &crate::store::Layout) -> crate::GithubFetcher {
    let cfg = crate::config::cached();
    let owner = crate::resolve_account(cfg.account()).owner;
    let floor = crate::index_probe::verified_floor(layout);
    let hint = discovery_hint(
        floor,
        || crate::index_probe::cached_index_builds(layout, &crate::net::github_source_id(&owner)),
        (owner == aterm_update_core::ATPKG_INDEX_OWNER)
            .then(crate::net::compiled_index_hint)
            .flatten(),
        PUBLISHED_INDEX_HINT.with(std::cell::Cell::get),
    );
    crate::GithubFetcher::new(owner)
        .with_overrides(crate::repo_overrides(cfg))
        .with_index_floor(floor)
        .with_index_hint(hint)
}

/// What a discovery from the verified `floor` may guess the channel publishes
/// ([`crate::net::IndexHint`]), from every build something here saw published: the window's
/// probe (`published`, [`PUBLISHED_INDEX_HINT_FLAG`]) on any store; and on one that verified
/// nothing yet, the highest build its §14 cache holds (`cached`, a resolve that landed
/// nothing) and the run this binary's release cut compiled in for the shipped channel
/// (`compiled`), which also says where the run starts. A store with a floor walks from it,
/// and is not asked to read its cache for this. Guesses all: the walk asks every build.
fn discovery_hint(
    floor: u64,
    cached: impl FnOnce() -> Option<Vec<u64>>,
    compiled: Option<crate::net::IndexHint>,
    published: Option<u64>,
) -> Option<crate::net::IndexHint> {
    let fresh = floor == 0;
    let cached = fresh
        .then(cached)
        .flatten()
        .and_then(|b| b.into_iter().max());
    let compiled = compiled.filter(|_| fresh);
    let newest = [cached, compiled.map(|c| c.newest), published]
        .into_iter()
        .flatten()
        .max()?;
    Some(crate::net::IndexHint {
        low: compiled.and_then(|c| c.low),
        newest,
    })
}

/// `ATPKG_REGISTRY` — a development seam (`aterm_types::dev_seam!`) swapping the fetch
/// source for an on-disk registry, for the integration tests and the publisher's
/// self-tests. A shipped binary does not read it.
pub(crate) const REGISTRY_SEAM: &str = "ATPKG_REGISTRY";

/// The [`REGISTRY_SEAM`] value this process runs under — `None` in every shipped binary.
pub(crate) fn registry_seam() -> Option<std::ffi::OsString> {
    aterm_types::dev_seam!(REGISTRY_SEAM)
}

/// An index program's no-op verdict, naming the index it is current with, so it can be
/// set beside a newer index on the release host (`aterm pkg doctor` says whether one is).
fn already_current_line(r: &crate::flow::InstallReport) -> String {
    format!(
        "atpkg: {} already current (build {}, as index {} pins it)",
        r.program, r.build, r.index_build
    )
}

/// The directory an `ATPKG_REGISTRY` value names with `dir:`, which [`resolve_fetcher`]
/// serves through [`crate::DirFetcher`]; `None` is the network fetcher.
pub(crate) fn dir_registry(spec: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    spec?
        .to_string_lossy()
        .strip_prefix("dir:")
        .map(std::path::PathBuf::from)
}

/// Pick the fetcher for the network verbs: a `dir:<path>` `ATPKG_REGISTRY` (a development
/// seam, [`REGISTRY_SEAM`]) selects the offline / publisher-test [`crate::DirFetcher`];
/// otherwise the production GitHub fetcher. A `dir:` registry's bytes still pass the same
/// verify + floor + freshness gates: the source is not an authenticity input.
fn resolve_fetcher(layout: &crate::store::Layout) -> Box<dyn crate::flow::Fetcher> {
    if let Some(dir) = dir_registry(registry_seam().as_deref()) {
        return Box::new(crate::DirFetcher::new(dir));
    }
    Box::new(github_fetcher(layout))
}

/// The trust anchor the network verbs verify under: the committed paper-master keyset
/// ([`crate::PKG_TRUST_ANCHORS`]) plus this store's durable `roster_seq` ratchet, and
/// nothing else. No environment variable can swap it; trusting a mirror or a second
/// account is a committed change to `aterm_update_core::pins`.
///
/// The roster floor is read per call, not snapshotted at process start: a verb that
/// ratchets it mid-pass (the default-set bootstrap) must not verify a later step under a
/// lower ratchet than the one it just advanced.
fn effective_anchor(layout: &crate::store::Layout) -> crate::Anchor {
    crate::Anchor::pinned(crate::sig::Floor::new(layout.roster_floor()).current())
}

/// This store's durable `index_build` high-water and the roster generation that recorded
/// it — one value, because a floor a machine set must not outlive the generation that
/// revoked that machine. See [`crate::sig::BuildFloor`].
fn build_floor(layout: &crate::store::Layout) -> crate::sig::BuildFloor {
    crate::sig::BuildFloor {
        index_build: crate::sig::Floor::new(layout.floor()).current(),
        roster_seq: crate::sig::Floor::new(layout.floor_generation()).current(),
    }
}

/// Advance the durable ratchets after a successful resolve. Best-effort: a failed write
/// never turns a completed install into an error. The `roster_seq` high-water's
/// authoritative write is inside `flow::verify_select_fresh`, the instant a generation is
/// admitted; repeating it here is idempotent. The `index_build` floor is this function's
/// own, and generation-aware: a strictly newer master-signed generation re-bases it
/// (possibly downward), or one rostered machine publishing `index_build = u64::MAX` would
/// raise the floor above everything, its own revocation included.
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
/// verify under ([`crate::manager_enabled`]).
fn manager_enabled() -> bool {
    crate::manager_enabled()
}

/// Whether this machine has adopted the ALab toolset ([`crate::store::Layout::adopted`]):
/// one way into set completion, the other a written `auto_install = true`
/// ([`should_complete_set`]); with [`adopted_by_seed`], the consent an announcement states
/// ([`set_completion_consent`]).
fn adopted(layout: &crate::store::Layout) -> bool {
    layout.adopted().is_file()
}

/// Record adoption after a deliberate whole-set install. Idempotent and best-effort: a
/// marker that could not be written means the question is asked again, the safe direction.
fn record_adoption(layout: &crate::store::Layout) {
    let path = layout.adopted();
    if path.is_file() {
        return;
    }
    // The payload is documentation, never read back: adoption is the file's existence.
    if let Err(e) = write_marker(
        &path,
        "# This machine runs the ALab toolset as a SET.\n\
         # atpkg therefore keeps that set COMPLETE — a program published to the\n\
         # signed index later is installed on a later pass, not silently skipped.\n\
         # Written by the batteries-included seed bootstrap or `install --default-set`.\n\
         # Removed by `uninstall`. To drop one program while staying adopted, use\n\
         # [packages].exclude in aterm.toml.",
    ) {
        eprintln!(
            "atpkg: could not record toolset adoption at {}: {e} — the set will not be \
             auto-completed until a later pass records it",
            path.display()
        );
    }
}

/// Create the `0600` marker at `path` with `body` (documentation; the marker's existence is
/// the fact). `Err` only when it cannot be created.
fn write_marker(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut f = crate::platform::open_create_write(path, 0o600)?;
    let _ = writeln!(f, "{body}");
    Ok(())
}

/// Remove the marker at `path`, if one is there — best-effort, like its writer.
fn remove_marker(path: &std::path::Path) {
    if path.is_file() {
        let _ = std::fs::remove_file(path);
    }
}

/// Whether an explicit `install --default-set` pass records adoption: it installed at least
/// one member, or found the set already complete (builds before it ran, nothing failed,
/// skipped or owed). The second arm matters: `uninstall` clears adoption and points at this
/// verb, and a complete set must then be re-adopted.
fn default_set_adopts(
    activated: usize,
    failures: u32,
    had_builds_before: bool,
    skipped: &[String],
    remaining_missing: &[String],
) -> bool {
    activated > 0
        || (failures == 0
            && had_builds_before
            && skipped.is_empty()
            && remaining_missing.is_empty())
}

/// Forget adoption, and who adopted ([`clear_seed_adoption`]). Called by `uninstall`:
/// set-completion must never undo an explicit removal.
fn clear_adoption(layout: &crate::store::Layout) {
    remove_marker(&layout.adopted());
    clear_seed_adoption(layout);
}

/// Whether the seed pass adopted this machine ([`crate::store::Layout::adopted_by_seed`]),
/// so installing aterm is the consent behind the set and `auto_install = false` the switch
/// that would have stopped it. Both files must exist: a record without the marker vouches
/// for nothing.
fn adopted_by_seed(layout: &crate::store::Layout) -> bool {
    adopted(layout) && layout.adopted_by_seed().is_file()
}

/// `cmd_seed`'s adoption: [`record_adoption`] plus the record of who adopted, written only
/// when this call creates the marker — the seed pass runs every launch, and stamping a
/// marker it found would relabel a machine the explicit verb adopted. A missing record
/// claims less, never more.
fn record_seed_adoption(layout: &crate::store::Layout) {
    if adopted(layout) {
        return;
    }
    record_adoption(layout);
    if !adopted(layout) {
        return;
    }
    let path = layout.adopted_by_seed();
    if let Err(e) = write_marker(
        &path,
        "# `atpkg seed`, the first launch's bootstrap, adopted this machine.\n\
         # Its EXISTENCE beside `adopted` is the whole fact: an update pass that\n\
         # completes the set says installing aterm is the consent, and names\n\
         # [packages].auto_install = false as the switch that would have stopped it.\n\
         # Removed with `adopted`, and by `install --default-set`, which is its own\n\
         # consent.",
    ) {
        eprintln!(
            "atpkg: could not record that the seed pass adopted the toolset at {}: {e} — \
             update passes will name only `aterm pkg uninstall --all` as the way out",
            path.display()
        );
    }
}

/// Forget that the seed pass adopted: the explicit toolset verb's call (running it is the
/// consent), after which "installing aterm is the consent" no longer describes the passes.
fn clear_seed_adoption(layout: &crate::store::Layout) {
    remove_marker(&layout.adopted_by_seed());
}

/// The programs this user removed individually ([`crate::store::Layout::removed`]).
fn removed_programs(layout: &crate::store::Layout) -> std::collections::BTreeSet<String> {
    // One reader, on the layout: the unattended update lanes need the same answer.
    layout.removed_programs()
}

/// The programs this machine keeps complete: every program the signed index names, narrowed
/// by `[packages].exclude`. The one reader for every lane that plans a set.
pub(crate) fn wanted_programs(
    index: &crate::manifest::Index,
    cfg: &crate::config::PackagesConfig,
) -> std::collections::BTreeSet<String> {
    index.installable(cfg.exclude())
}

/// Reconcile system satisfaction over `wanted` against the given `PATH`. Returns the
/// members satisfied this pass (each recorded and announced by [`satisfy_by_system`], and
/// dropped from `installed` when a managed copy was retired), which the caller removes from
/// its wanted set. `PATH` is injected: a pass reads it once, at its entry
/// ([`install_default_set_with_path`]), so the system-satisfaction and shadow questions
/// see the same value.
fn reconcile_system_satisfied_with(
    layout: &crate::store::Layout,
    index: &crate::manifest::Index,
    wanted: &std::collections::BTreeSet<String>,
    installed: &mut std::collections::BTreeMap<String, u64>,
    path_var: Option<&std::ffi::OsStr>,
) -> Vec<String> {
    let mut satisfied = Vec::new();
    // A vendor-direct program's `system` key decides nothing: retiring its managed copy
    // here would undo its own lane every pass.
    for name in wanted
        .iter()
        .filter(|n| !crate::vendor_direct::is_vendor(n))
    {
        let Some(program) = index.program(name) else {
            continue;
        };
        let Some(bin) = program.system.as_deref() else {
            continue;
        };
        let Some(path) = crate::vendor::system_binary_on_path(&layout.prefix, bin, path_var) else {
            continue;
        };
        satisfy_by_system(layout, name, &path, installed);
        satisfied.push(name.clone());
    }
    satisfied
}

/// What the last verified index pins for this machine — `(index_build, program → build)` on
/// the one channel ([`crate::config::CHANNEL`]) for this target — from the cache: offline,
/// nothing written. Settings ▸ Packages' "latest known" for an ALab program, read on the
/// window's worker thread; `cfg` is passed because a window lives for days and re-reads
/// `aterm.toml` rather than trusting this process's first read. `None` when nothing cached
/// verifies. Vendor programs are never in it — no index pins them — except one the source
/// rule hands to the index lane here ([`crate::alab`]: ALab's build, pinned for this target).
#[must_use]
pub fn latest_known_pins(
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
) -> Option<(u64, std::collections::BTreeMap<String, u64>)> {
    let index = cached_index_for(layout, cfg)?;
    let pins = index
        .channel_for(crate::config::CHANNEL, current_triple())?
        .pin
        .into_iter()
        .filter(|(program, _)| {
            !crate::vendor_direct::is_vendor(program)
                || crate::alab::index_owns(layout, program, current_triple(), Some(&index))
        })
        .collect();
    Some((index.index_build, pins))
}

/// The last index this machine verified, from the cache — offline, no floor or ratchet
/// written. `None` when nothing cached verifies: the advisory surfaces that read it
/// (`which`) then say nothing.
pub(crate) fn cached_index(layout: &crate::store::Layout) -> Option<crate::TrustedIndex> {
    cached_index_for(layout, crate::config::cached())
}

/// The channel as the last verified index publishes it ([`cached_index`]): the revocations
/// that pick the build gc keeps below each live one ([`crate::gc::run`]) at a door holding
/// no index of its own. `None` when nothing cached verifies.
fn last_verified_channel(layout: &crate::store::Layout) -> Option<crate::manifest::Channel> {
    cached_index(layout)?.channel_for(crate::config::CHANNEL, current_triple())
}

/// [`cached_index`] under the account `cfg` names.
fn cached_index_for(
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
) -> Option<crate::TrustedIndex> {
    let slug = crate::resolve_account(cfg.account()).slug();
    let cache = crate::IndexCache::for_layout(layout);
    let candidates = cache.load(&format!("github:{slug}"))?;
    crate::select_index(
        &effective_anchor(layout),
        candidates,
        build_floor(layout),
        now_unix(),
    )
    .selected
    .map(|s| s.index)
}

/// `program` is satisfied by the system install at `path`: say so, record the `system:
/// <path> — not managed by aterm` row, and retire any managed copy (through the fail-closed
/// `ops::uninstall`), remembering the day (`<prefix>/retired/<program>`) for
/// `(managed copy retired <date>)`. Not a removal on purpose: no `removed` marker, so the
/// member installs again once the system binary goes. `installed` loses a retired program.
fn satisfy_by_system(
    layout: &crate::store::Layout,
    program: &str,
    path: &std::path::Path,
    installed: &mut std::collections::BTreeMap<String, u64>,
) {
    let mut event = String::new();
    if let Some(build) = installed.remove(program) {
        match crate::uninstall(layout, program) {
            Ok(()) => {
                let today = today_ymd();
                if !today.is_empty()
                    && let Err(e) = layout.record_retired(program, &today)
                {
                    eprintln!(
                        "atpkg: {program}: could not record the retirement date at {}: {e}",
                        layout.retired_marker(program).display()
                    );
                }
                event = format!(" — retired the managed copy (build {build})");
            }
            Err(e) => {
                event = format!(" — could not retire the managed copy (build {build}): {e}");
            }
        }
    }
    let retired = layout.retired_date(program);
    let state = crate::state::system(path, retired.as_deref());
    // The pass log prints the words the row records, plus the event.
    if event.contains("could not") {
        eprintln!("atpkg: {program}: {state}{event}");
    } else {
        println!("atpkg: {program}: {state}{event}");
    }
    // The row carries the fact; the aggregate sentence stays the pass's own verdict (an
    // "up to date" must not be displaced by a per-member note). Only a record with no
    // sentence yet takes this one.
    let outcome = crate::status::read(layout)
        .map(|s| s.outcome)
        .filter(|o| !o.is_empty())
        .unwrap_or_else(|| format!("{program} satisfied by system {}", path.display()));
    record_unbuilt(layout, program, state, outcome);
}

/// Record `state` as `program`'s row (no build, no root) and print it, unless the record
/// already says exactly that: a pass that changes nothing writes and says nothing. The
/// aggregate outcome is carried over. Returns whether a row was written.
fn record_state_if_changed(layout: &crate::store::Layout, program: &str, state: String) -> bool {
    record_states_if_changed(layout, [(program, state)])
}

/// [`record_state_if_changed`] for several rows, in one write: a first run's four agent rows
/// took a write each — its fsync, rename and directory sync, 4 ms apiece — on the way to the
/// set's announcement. Returns whether any row was written.
fn record_states_if_changed<'p>(
    layout: &crate::store::Layout,
    rows: impl IntoIterator<Item = (&'p str, String)>,
) -> bool {
    let recorded = crate::status::read(layout);
    let changed: Vec<(String, crate::ProgramStatus)> = rows
        .into_iter()
        .filter(|(program, state)| {
            !recorded
                .as_ref()
                .and_then(|s| s.programs.get(*program))
                .is_some_and(|r| r.state == *state)
        })
        .map(|(program, state)| {
            println!("atpkg: {program}: {state}");
            let row = crate::ProgramStatus {
                installed_build: None,
                state,
                tree_root: String::new(),
            };
            (program.to_string(), row)
        })
        .collect();
    if changed.is_empty() {
        return false;
    }
    write_program_rows(
        layout,
        changed,
        Some(recorded.map(|s| s.outcome).unwrap_or_default()),
    );
    true
}

/// Whether an UpToDate member's row must be rewritten to `managed <build> — pinned by
/// index <index_build>`: absent, the legacy `active` spelling, a managed row whose build or
/// index differs from this pass's, or a machine-local fault this pass disproved. `aborted:`
/// and `error:` rows are written by a failed pass and every other writer rewrites a row
/// only when bytes move, so a settled program would keep its failure row forever; reaching
/// here (UpToDate, a live shim-derived build at the pin) is the disproof. Every other
/// spelling is another arm's (SHADOWED, `system:`); `tombstoned:` never reaches here.
fn up_to_date_row_needs_rewrite(prior: Option<&str>, build: u64, index_build: u64) -> bool {
    match prior {
        None | Some("active") => true,
        // The machine-local fault prefixes, spelled as `doctor::is_problem_state` spells
        // them, a flip-deferred row, and an older client's `blocked by …` row (its
        // requirement gate is retired), which nothing else lifts.
        Some(s)
            if s.starts_with("aborted:")
                || s.starts_with("error:")
                || s.starts_with("blocked by ")
                || s.starts_with(crate::state::FLIP_DEFERRED_PREFIX) =>
        {
            true
        }
        Some(s) => {
            crate::state::managed_pin(s).is_some_and(|(b, i)| b != build || i != index_build)
        }
    }
}

/// Write one program's row and nothing else: the aggregate `outcome` is carried from the
/// existing record. A row migration is not the pass's verdict, and must not replace it.
fn record_row_keeping_outcome(
    layout: &crate::store::Layout,
    program: &str,
    row: crate::ProgramStatus,
) {
    write_program_row(layout, program, row, None);
}

/// Today as `YYYY-MM-DD` (UTC), or empty when the clock is unusable — the retirement
/// marker's payload. Derived from the same RFC3339 stamp `status.toml` carries.
fn today_ymd() -> String {
    let stamp = now_rfc3339();
    stamp.get(..10).map(str::to_string).unwrap_or_default()
}

/// The fix-line for `program`'s managed shim when it exports an environment before it
/// execs ([`crate::shim_env::ShimEnv::fix_line`]): `Claude Code's own updater is off here
/// (DISABLE_AUTOUPDATER=1)`, or `runs with NAME=VALUE`. Read off the shim as laid, never a
/// manifest or a record; `None` for a plain shim. Never part of the recorded state: `which`
/// and `doctor` append it after the canonical row.
pub(crate) fn shim_env_fix(shim: &std::path::Path, program: &str) -> Option<String> {
    crate::platform::shim_env_of(shim).fix_line(program)
}

/// The fix-line for a shadowed `tool` of `program`: `type alab-<tool> for the managed one`
/// ([`crate::state::alias_hint`]), printed only when typing the alias really runs the
/// managed copy: `alab-<tool>` is laid, resolves into this prefix's `store/<program>/` (the
/// anchored containment), and is not itself shadowed by a foreign `alab-<tool>` earlier on
/// `path_var` than the managed `bin/`. `None` otherwise — a vendor tool or a
/// system-satisfiable member (no alias by policy), a tool that is its own alias, an install
/// not yet reconciled. Never part of the recorded state.
pub(crate) fn alias_fix(
    layout: &crate::store::Layout,
    tool: &crate::store::ToolName,
    program: &str,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<String> {
    let alias = tool.alias()?;
    let target = crate::which(layout, alias.as_str())?;
    let (owner, _) = crate::ops::store_build_of(&layout.prefix, &target)?;
    if owner != program {
        return None;
    }
    if crate::vendor::shadowing_binary_on_path(&layout.prefix, alias.as_str(), path_var).is_some() {
        return None;
    }
    Some(crate::state::alias_hint(alias.as_str()))
}

/// The fix-line for `program`'s SHADOWED row where the shadow is the foreign copy at
/// `shadow` — [`alias_fix`] for the tool that copy stands in for, so the Packages page says
/// what `which` and `doctor` say. Never part of the canonical state.
#[must_use]
pub fn shadowed_fix(
    layout: &crate::store::Layout,
    program: &str,
    shadow: &std::path::Path,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<String> {
    let name = shadow.file_name()?.to_str()?;
    // A Windows copy is `<tool>.exe`; the tool is the name without it.
    let name = if cfg!(windows) {
        name.strip_suffix(".exe").unwrap_or(name)
    } else {
        name
    };
    alias_fix(
        layout,
        &crate::store::ToolName::new(name)?,
        program,
        path_var,
    )
}

/// Alias reconcile: for every installed, non-dev-linked program, bring its `alab-<tool>`
/// aliases in line with the signed index's verdict on whether it is ALab's own
/// ([`crate::activate::Aliases::for_program`]) — laid when missing (an install a pre-alias
/// client made, which the pipeline short-circuits as up to date forever after), swept when
/// the entry says the program is not ALab's. A primary is rewritten only when it exports
/// other than its build declares ([`crate::activate::reassert_shim_env`]), never to
/// re-point it, so the silent tick touches nothing once primaries and aliases stand.
/// Best-effort per program: an unwritable `bin/` is the install lane's to report.
fn reconcile_aliases(layout: &crate::store::Layout, index: &crate::manifest::Index) {
    // One listing of `bin/` for the whole pass, exact for it: laying aliases, pruning and
    // re-rendering a primary in place move no primary off its build. `prune_stale_shims`
    // lists the directory itself when it deletes.
    let scan = crate::ops::BinScan::capture(layout);
    let live: Vec<(String, std::path::PathBuf, Vec<crate::store::ToolName>)> =
        crate::ops::active_builds_in(&scan)
            .into_iter()
            .filter(|(program, _)| !crate::linkmode::is_linked(layout, program))
            .map(|(program, build)| {
                let tools = crate::ops::active_tools_in(&scan, &program, build);
                let build_dir = layout.build_dir(&program, build);
                (program, build_dir, tools)
            })
            .collect();
    // The primaries' environment first, for every program in one lay: the alias and agents
    // reconciles copy what the primary exports, so a primary healed here carries its alias
    // and twin with it this pass. An up-to-date program is never laid again, so without this
    // a shim laid plain would keep Claude Code's own updater on until the next claude build.
    let builds: Vec<(&std::path::Path, &[crate::store::ToolName])> = live
        .iter()
        .map(|(_, build_dir, tools)| (build_dir.as_path(), tools.as_slice()))
        .collect();
    crate::activate::reassert_shim_env(layout, &builds);
    for (program, build_dir, tools) in &live {
        let aliases = crate::activate::Aliases::for_program(program, index.program(program));
        let _ = crate::activate::reconcile_aliases(layout, build_dir, tools, aliases);
    }
    // Once, after the loop: the front-of-PATH `agents/` twins. `reconcile_agents` checks
    // both agent programs and sweeps `agents/` whatever it is handed, so it runs once per
    // pass, not per program — and unconditionally: an empty store's sweep must still run.
    crate::activate::reconcile_agents(layout);
}

/// Shadow reconcile: for every managed member and every tool it exposes, a foreign
/// executable of that name earlier on this pass's `PATH` than the managed `bin/` is what
/// runs in a shell with that `PATH`. Printed (with the alias fix-line when laid,
/// [`alias_fix`]) and never fixed: a warning the user owns. Returns the shadowed programs,
/// for the `shadowed:` line.
///
/// Never recorded: a shadow is one shell's truth, computed at read time
/// ([`crate::status::with_shadows`]); a legacy SHADOWED row is put back to the managed row.
/// An agent program whose `agents/` twin is current ([`agent_twin_current`]) is what every
/// tab runs, so a foreign copy ahead is no shadow (the managed row plus [`pass_path_tail`]).
/// The [`FOREIGN_MARKER`] line ([`FOREIGN_PATH`]) names each shadowed member that has an
/// alias fix.
fn reconcile_shadowed(
    layout: &crate::store::Layout,
    index_build: u64,
    path_var: Option<&std::ffi::OsStr>,
) -> Vec<String> {
    let mut shadowed = Vec::new();
    let mut foreign: Vec<ForeignItem> = Vec::new();
    let recorded = crate::status::read(layout);
    let outcome = recorded
        .as_ref()
        .map(|s| s.outcome.clone())
        .unwrap_or_default();
    // One listing for this pass too: this lane only reads, prints and lifts legacy rows —
    // nothing here writes to `bin/` or deletes from it.
    let scan = crate::ops::BinScan::capture(layout);
    for (program, build) in crate::ops::active_builds_in(&scan) {
        if crate::linkmode::is_linked(layout, &program) {
            continue;
        }
        let shadow = crate::ops::active_tools_in(&scan, &program, build)
            .into_iter()
            .find_map(|tool| {
                crate::vendor::shadowing_binary_on_path(&layout.prefix, tool.as_str(), path_var)
                    .map(|path| (tool, path))
            });
        let prior = recorded.as_ref().and_then(|s| s.programs.get(&program));
        let was = prior.map(|r| r.state.clone()).unwrap_or_default();
        // The managed row this member's record carries: its index pin, or — a vendor-direct
        // program, which no index pins — its vendor row.
        let managed_row = || match crate::vendor_direct::spec(&program) {
            Some(spec) => vendor_state_of(layout, spec, build, prior),
            None => crate::state::managed(build, index_build),
        };
        let agent_tail = shadow.as_ref().and_then(|(_, path)| {
            agent_shell_shadow(layout, &program, build, path, path_var)
                .map(|_| pass_path_tail(path))
        });
        match (shadow, agent_tail) {
            (Some(_), Some(tail)) => println!("atpkg: {program}: {} — {tail}", managed_row()),
            (Some((tool, path)), None) => {
                let state = crate::state::shadowed(build, &path);
                match alias_fix(layout, &tool, &program, path_var) {
                    Some(fix) => {
                        println!("atpkg: {program}: {state} on this pass's PATH — {fix}");
                        foreign.push(ForeignItem {
                            name: program.clone(),
                            path,
                            fix,
                        });
                    }
                    None => println!("atpkg: {program}: {state} on this pass's PATH"),
                }
                shadowed.push(program.clone());
            }
            (None, _) => {}
        }
        // The one write this lane makes: a legacy SHADOWED row lifted to the managed row.
        if crate::state::shadowed_by(&was).is_some() {
            record_status(
                layout,
                &program,
                crate::ProgramStatus {
                    installed_build: Some(build),
                    state: managed_row(),
                    tree_root: effective_tree_root(layout, &program, Some(build), ""),
                },
                outcome.clone(),
            );
        }
    }
    if !shadowed.is_empty() {
        emit_marker_line(&format!("atpkg: {SHADOWED_MARKER}{}", shadowed.join(", ")));
    }
    emit_marker_line(&format!("atpkg: {}", foreign_line(FOREIGN_PATH, &foreign)));
    shadowed
}

/// The explicit door (`atpkg install <program>`) for the rule that needs the verified
/// index before the flow runs: a system-satisfied member ([`crate::manifest::Program::system`])
/// is not installed — `Some(path)` names the system binary, the row is recorded and any
/// managed copy retired ([`satisfy_by_system`]), and the caller stops there with success.
///
/// `None` in every other case, an unresolvable index or an unlisted name included: the flow
/// that follows raises the real error with its real remedy. The resolve is memoized in the
/// fetcher, so the flow's own resolve costs no second round-trip.
fn explicit_install_door(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    program: &str,
) -> Option<std::path::PathBuf> {
    // No index rule decides a vendor-direct program: no system satisfaction, and no index
    // resolve ahead of its own lane.
    if crate::vendor_direct::is_vendor(program) {
        return None;
    }
    let index = crate::resolve_verified_index(
        fetcher,
        layout,
        &effective_anchor(layout),
        build_floor(layout),
        now_unix(),
    )
    .ok()?;
    let prog = index.program(program)?;
    let path = crate::vendor::system_satisfied(&layout.prefix, prog)?;
    let mut installed = crate::active_builds(layout);
    satisfy_by_system(layout, program, &path, &mut installed);
    Some(path)
}

/// Record that `program` was removed on purpose, so no unattended pass puts it back.
///
/// A write that did not land is printed: this record alone keeps the program out of the
/// unattended lanes ([`seed_serviceable`] and `install_default_set_inner` filter on it, and
/// `cmd_seed` re-adopts on every launch), while the caller prints `uninstalled` and exits 0
/// whatever happened here.
fn record_removed(layout: &crate::store::Layout, program: &str) {
    let mut all = removed_programs(layout);
    if !all.insert(program.to_string()) {
        return;
    }
    if let Err(e) = write_removed(layout, &all) {
        eprintln!(
            "atpkg: could not record the removal at {}: {e} — the next unattended pass may \
             reinstall {program}",
            layout.removed().display()
        );
    }
}

/// Forget removals for `programs` — an explicit install is a change of mind. A failure is
/// printed: the toolset verb clears the markers before a pass that narrows `wanted` by
/// them, so a failed clear would skip the very member it was asked to restore.
fn clear_removed(layout: &crate::store::Layout, programs: &[String]) {
    let mut all = removed_programs(layout);
    let mut lifted: Vec<&str> = Vec::new();
    for p in programs {
        if all.remove(p) {
            lifted.push(p.as_str());
        }
    }
    if lifted.is_empty() {
        return;
    }
    if let Err(e) = write_removed(layout, &all) {
        eprintln!(
            "atpkg: could not lift the removal record at {}: {e} — set completion may keep \
             skipping {}",
            layout.removed().display(),
            lifted.join(", ")
        );
    }
}

/// Write the whole record, atomically, and hand back any failure.
///
/// # Errors
/// The open/write/rename error, or the delete's when the last entry is lifted (an
/// already-absent file is not one). Both callers report it: this record is load-bearing.
fn write_removed(
    layout: &crate::store::Layout,
    all: &std::collections::BTreeSet<String>,
) -> std::io::Result<()> {
    let path = layout.removed();
    if all.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let mut body = String::from(
        "# Programs removed on purpose. No unattended pass reinstalls these;\n\
         # `aterm pkg install <program>` brings one back.\n",
    );
    for p in all {
        body.push_str(p);
        body.push('\n');
    }
    // Temp + rename, like `status::write`: a crash mid-write of the live file would leave a
    // shorter list, every program past the cut un-removed.
    let mut tmp_name = String::from("removed.tmp-");
    tmp_name.push_str(&std::process::id().to_string());
    let tmp = path.with_file_name(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    let wrote = crate::platform::open_create_write(&tmp, 0o600).and_then(|mut f| {
        use std::io::Write as _;
        f.write_all(body.as_bytes())?;
        crate::platform::sync_durable(&f)
    });
    let published = wrote.and_then(|()| std::fs::rename(&tmp, &path));
    if published.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    published
}

/// `group-aborted: <label> update aborted at <member> during <phase>: <cause>` — a
/// coherence group's transaction aborted. The window renders a Warn row on the toolchain
/// lane; it answers no announcement. Printed by [`report_channel_apply`].
pub const GROUP_ABORTED_MARKER: &str = "group-aborted: ";
/// The first line of a cause, bounded for a status row and a marker line: a stage error can
/// carry a multi-line transport message (doctor prints the full row on request).
#[must_use]
pub fn clip_cause(why: &str) -> String {
    const CAUSE_CAP: usize = 240;
    let first = why.lines().next().unwrap_or("").trim();
    if first.chars().count() <= CAUSE_CAP {
        return first.to_string();
    }
    let mut clipped: String = first.chars().take(CAUSE_CAP).collect();
    clipped.push('…');
    clipped
}

/// The stable stdout markers the GUI parses (`crates/aterm-gui`, `parse_seed_line`).
/// Constants `aterm-gui` imports, since a reworded literal would fail no build or test and
/// the notice would just never appear again. The `seed-` spellings stay: the window parses
/// them.
///
/// Every marker answers an announcement except `NET_STARTING`, which opens one, and those
/// that do neither: `LOCK_WAITING`, `LOCK_ACQUIRED`, `GROUP_ABORTED`, `SHADOWED`,
/// `MANAGED_CURRENT` and `MACHINE_SETTINGS` ([`announcement::MARKERS`]; [`emit_marker_line`]
/// keeps the ledger). An announcement with no answer leaves "Installing…" on screen forever.
pub const SEED_UNUSABLE_MARKER: &str = "seed-unusable: ";
pub const SEED_FAILED_MARKER: &str = "seed-failed: ";
/// The network completion lane's answer — programs that arrived over the wire on an
/// adopted machine.
pub const NET_INSTALLED_MARKER: &str = "net-installed: ";
/// The network lane's announcement, printed before any bytes move — the only announcement
/// a pass prints, emitted only through [`net_announcement`].
pub const NET_STARTING_MARKER: &str = "net-starting: ";
/// The network lane's failure terminal: an announced provisioning that then
/// installs nothing must retire its own status bar with the truth.
pub const NET_FAILED_MARKER: &str = "net-failed: ";
/// The dispatch edge's "queued behind a sibling" announcement: printed once, before any
/// verb output, only under `--wait-lock` with the store lock held past
/// [`crate::lock::WAIT_ANNOUNCE_GRACE`]. Neither a pass start nor a terminal answer: the GUI
/// logs it (`Wake::PkgLockWaiting`) and paints no row. Emitted by [`mutator_store_lock`]
/// alone.
pub const LOCK_WAITING_MARKER: &str = "lock-waiting: ";
/// The dispatch edge's "the wait is over" line: printed once, right after a wait that
/// announced itself ([`LOCK_WAITING_MARKER`]) acquires the lock. Neither a pass start nor a
/// terminal answer: the GUI logs it (`Wake::PkgLockAcquired`); there is no row to retire.
/// Emitted by [`mutator_store_lock`] alone.
pub const LOCK_ACQUIRED_MARKER: &str = "lock-acquired: ";
/// The shadow reconcile's answer ([`reconcile_shadowed`]): the managed programs a foreign
/// copy out-ranks on this pass's `PATH`, comma-separated, so the GUI can tell a pass that
/// found a shadow from one that found none.
pub const SHADOWED_MARKER: &str = "shadowed: ";
/// The managed-current answer, printed at the end of every successful update/seed pass:
/// each agent program ([`crate::stub::AGENT_PROGRAMS`]) that is installed and is what every
/// aterm tab runs, `; `-separated, as `<name> <version> (<Vendor> latest)` while it is its
/// vendor's head, else `<name> <version> (<why not>)` ([`ManagedAgent::standing`]), so a
/// pass that could not check one program never reads as all of them current. Omitted when
/// no agent program is installed.
pub const MANAGED_CURRENT_MARKER: &str = "managed-current: ";

/// `machine-settings: universal-control disabled; rosetta installed` — what this pass changed
/// about the machine per the doctor's remedies, entries `; `-separated, omitted when nothing
/// changed.
/// The window records it as `appstatus` ledger entries (never a status-bar row) that carry
/// the revert ([`crate::machine::UNIVERSAL_CONTROL_REVERT`]); Settings ▸ Security's "Last
/// change" names it.
pub const MACHINE_SETTINGS_MARKER: &str = "machine-settings: ";

/// `foreign: <scope> <items>` — what one of a pass's looks found that aterm leaves alone
/// because it did not lay it, with the fix that is the owner's. One line per look, every
/// time it looks, `none` when it found nothing, so the window can end what a look no longer
/// sees ([`foreign_line`]): [`FOREIGN_SEAM`] (the rustup seam, [`reassert_rustup_seam`]) and
/// [`FOREIGN_PATH`] (the shadow reconcile, [`reconcile_shadowed`]). Each item is
/// `<name> <path> — <fix>`, `; `-separated; an item with no fix is not on it.
/// Informational: neither opens nor answers an announcement.
pub const FOREIGN_MARKER: &str = "foreign: ";
/// The rustup seam's scope on the [`FOREIGN_MARKER`] line: an entry under
/// `~/.rustup/toolchains` that is not atpkg's ([`crate::seam::Refusal::Foreign`]).
pub const FOREIGN_SEAM: &str = "seam";
/// The shadow reconcile's scope on the [`FOREIGN_MARKER`] line: a foreign copy ahead of a
/// managed program's tool on the pass's `PATH`, named by its program.
pub const FOREIGN_PATH: &str = "path";
/// What a scope that found nothing foreign says.
pub const FOREIGN_NONE: &str = "none";

/// `c-toolchain: missing`, `c-toolchain: broken` or `c-toolchain: ready` — whether
/// installing a C toolchain is what this machine needs ([`crate::prereq`]): `missing` where
/// none is installed ([`crate::prereq::CcVerdict::NoDriver`], `NoDeveloperDir`: on macOS no
/// Command Line Tools), `broken` where one is installed, cannot build, and an install fixes
/// that (off macOS, the libc headers, [`crate::prereq::CcVerdict::fix`]), `ready` where
/// installing one would fix nothing (it builds, it refuses for a reason of its own, it did
/// not answer). The window files one exceptions entry naming the fix
/// ([`crate::prereq::act`], on macOS `aterm init --only base.clt`) from `missing` or
/// `broken` and ends it at `ready` (docs/DESIGN-init-system-2026-10-20.md §4.2,
/// `base.clt`). A pass that laid programs down says it from the full probe
/// ([`say_c_toolchain`]), and off macOS so does every update pass over a toolset (only the
/// full probe tells a `cc` without headers from one that builds); on macOS the seed says it
/// from the read-only half at every launch ([`c_toolchain_seed_marker`]), which there is
/// the whole verdict, so a toolset that arrived with no window watching is filed at the
/// next launch and the entry ends at the first launch after the tools arrive.
/// Informational, and silent on a terminal, where doctor's sentence beside it says the
/// same.
pub const C_TOOLCHAIN_MARKER: &str = "c-toolchain: ";
/// The [`C_TOOLCHAIN_MARKER`] body of a toolchain that is not installed.
pub const C_TOOLCHAIN_MISSING: &str = "missing";
/// The [`C_TOOLCHAIN_MARKER`] body of an installed compiler that cannot build, which an
/// install fixes (off macOS only).
pub const C_TOOLCHAIN_BROKEN: &str = "broken";
/// The [`C_TOOLCHAIN_MARKER`] body of a toolchain an install would not change.
pub const C_TOOLCHAIN_READY: &str = "ready";

/// The [`C_TOOLCHAIN_MARKER`] line for `verdict`, or `None` where the probe asked nothing
/// (Windows).
fn c_toolchain_marker(verdict: &crate::prereq::CcVerdict) -> Option<String> {
    use crate::prereq::CcVerdict;
    let body = match verdict {
        CcVerdict::NotProbed => return None,
        CcVerdict::NoDriver | CcVerdict::NoDeveloperDir => C_TOOLCHAIN_MISSING,
        broken if broken.fix().is_some() => C_TOOLCHAIN_BROKEN,
        _ => C_TOOLCHAIN_READY,
    };
    Some(format!("atpkg: {C_TOOLCHAIN_MARKER}{body}"))
}

/// The seed's [`C_TOOLCHAIN_MARKER`] line, from the read-only half of the probe
/// ([`crate::prereq::installed_with`]), on macOS only: there a compiler that is found but
/// refuses is installed already, so whether one is found is the whole verdict
/// ([`crate::prereq::CcVerdict::fix`]) and the seed never contradicts a pass's full
/// probe. `ready` where the tools are found; `missing` where they are not and the toolset
/// is here to need them (`toolset_present`), so a first launch leaves the filing to the
/// pass that lays the set down. Off macOS a found `cc` may still lack the headers an
/// install supplies, so only a pass's full probe says.
fn c_toolchain_seed_marker(
    path: Option<&std::ffi::OsStr>,
    host: &crate::prereq::CcHost,
    toolset_present: bool,
) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let body = match crate::prereq::installed_with(path, host)? {
        true => C_TOOLCHAIN_READY,
        false if toolset_present => C_TOOLCHAIN_MISSING,
        false => return None,
    };
    Some(format!("atpkg: {C_TOOLCHAIN_MARKER}{body}"))
}

/// Say [`c_toolchain_seed_marker`]'s line, when there is one.
fn say_c_toolchain_at_seed(
    path: Option<&std::ffi::OsStr>,
    host: &crate::prereq::CcHost,
    toolset_present: bool,
) {
    if let Some(line) = c_toolchain_seed_marker(path, host, toolset_present) {
        emit_marker_line(&line);
    }
}

/// Say the C toolchain's verdict after a pass laid programs down: doctor's sentence when
/// it blocks builds ([`prereq_line`]), then the [`C_TOOLCHAIN_MARKER`] line the window
/// files or ends its entry from. One probe for both; `host` is
/// [`crate::prereq::CcHost::this_machine`] in a pass.
fn say_c_toolchain(path: Option<&std::ffi::OsStr>, host: &crate::prereq::CcHost) {
    let verdict = crate::prereq::probe_with(path, host);
    if let Some(line) = prereq_line(&verdict) {
        println!("{line}");
    }
    if let Some(line) = c_toolchain_marker(&verdict) {
        emit_marker_line(&line);
    }
}

/// One thing a look found that aterm leaves alone ([`FOREIGN_MARKER`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForeignItem {
    /// The seam (`trust`) or the program (`trust`) it stands before.
    pub(crate) name: String,
    /// The entry or copy that is not aterm's.
    pub(crate) path: std::path::PathBuf,
    /// atpkg's fix sentence: an item with none is not on the line.
    pub(crate) fix: String,
}

/// The [`FOREIGN_MARKER`] line for `scope`'s look: `foreign: seam none`, or
/// `foreign: path trust /u/.local/bin/trustdoc — type alab-trustdoc for the managed one`.
/// A `; ` inside a field is written `, ` so the items split back whole.
pub(crate) fn foreign_line(scope: &str, items: &[ForeignItem]) -> String {
    let field = |s: &str| s.replace("; ", ", ");
    let body = if items.is_empty() {
        FOREIGN_NONE.to_string()
    } else {
        items
            .iter()
            .map(|item| {
                format!(
                    "{} {} — {}",
                    field(&item.name),
                    field(&item.path.display().to_string()),
                    field(&item.fix)
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    format!("{FOREIGN_MARKER}{scope} {body}")
}

/// The `machine-state:` marker bare `atpkg machine` prints (the read counterpart of
/// `machine-settings:`): one byte-stable line of `key=value` pairs joined by `; `, built by
/// [`crate::machine::machine_state_line`] and parsed by
/// [`crate::machine::parse_machine_state`].
pub const MACHINE_STATE_MARKER: &str = "machine-state: ";

/// The clause a `seed-unusable:` line carries when the signed index serves no build for
/// this machine's architecture — permanent, nothing the user can fix. One spelling for the
/// emitter ([`report_seedless_posture`]) and the reader ([`seed_unusable_is_informational`]).
pub const UNSERVED_ARCHITECTURE_CLAUSE: &str = "for this machine's architecture";
/// The clause of the `seed-unusable:` line that says every program the index serves here
/// was removed on purpose (`Narrowing::verdict`): a decision, not a failure.
pub const REMOVED_ON_PURPOSE_CLAUSE: &str = "was removed on this machine";

/// Whether a `seed-unusable:` body states a fact rather than a failure: the architecture is
/// unserved ([`UNSERVED_ARCHITECTURE_CLAUSE`]) or everything served here was removed on
/// purpose ([`REMOVED_ON_PURPOSE_CLAUSE`]). The Packages badge lights for the other cause,
/// an `exclude` a person can shorten.
#[must_use]
pub fn seed_unusable_is_informational(body: &str) -> bool {
    body.contains(UNSERVED_ARCHITECTURE_CLAUSE) || body.contains(REMOVED_ON_PURPOSE_CLAUSE)
}

#[cfg(test)]
mod seed_unusable_tests {
    use super::{Narrowing, seed_unusable_is_informational};

    /// Every `seed-unusable:` cause in [`super::report_seedless_posture`]'s words: the
    /// unserved architecture and a removal read as facts, an exclusion as a failure. The
    /// exclusion's sentence is pinned verbatim because aterm-gui's
    /// `an_informational_unusable_marker_is_no_failure` quotes it.
    #[test]
    fn an_unserved_architecture_and_a_removal_are_facts_an_exclusion_is_not() {
        let unserved =
            super::unserved_sentence("x86_64-apple-darwin", &["aarch64-apple-darwin".into()]);
        assert_eq!(
            unserved,
            "ALab publishes no build for this machine's architecture (x86_64-apple-darwin) \
             — it builds for aarch64-apple-darwin; nothing installs here until one is published"
        );
        assert!(seed_unusable_is_informational(&unserved), "{unserved}");
        let names = |n: &[&str]| n.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let (removed, _) = Narrowing {
            excluded: Vec::new(),
            removed: names(&["ay", "trust"]),
        }
        .verdict("aarch64-apple-darwin");
        assert_eq!(
            removed,
            "every ALab program available here was removed on this machine (ay, trust) — \
             nothing to install. `aterm pkg install <program>` brings one back"
        );
        assert!(seed_unusable_is_informational(&removed), "{removed}");
        let (excluded, _) = Narrowing {
            excluded: names(&["ay", "clean"]),
            removed: Vec::new(),
        }
        .verdict("aarch64-apple-darwin");
        assert_eq!(
            excluded,
            "[packages].exclude narrows the toolset to nothing installable on this machine \
             (aarch64-apple-darwin; excluded: ay, clean) — shorten it to receive the toolset"
        );
        assert!(!seed_unusable_is_informational(&excluded), "{excluded}");
        // An exclusion that shares the set with removals is still the exclusion's line,
        // and never borrows the removal's clause (which would read as a fact).
        let (both, _) = Narrowing {
            excluded: names(&["ay"]),
            removed: names(&["trust"]),
        }
        .verdict("aarch64-apple-darwin");
        assert!(both.contains("excluded: ay; uninstalled: trust"), "{both}");
        assert!(!seed_unusable_is_informational(&both), "{both}");
    }
}

/// The prefix of every `atpkg machine` / `atpkg machine apply` verdict sentence, which a
/// window worker keys on to quote the verdict.
pub const MACHINE_VERDICT_PREFIX: &str = "atpkg machine: ";

/// The line [`apply_machine_settings`] prints when it refuses to touch the machine (a home
/// that is not this account's). The verb still exits 0, so a window worker keys on this
/// prefix to report the refusal.
pub const MACHINE_NOT_APPLIED_PREFIX: &str = "atpkg: machine settings not applied — ";

/// A machine action ran but failed. The verb can still exit 0, so the GUI reads this apart
/// from the change markers and the verdict; other changes from the pass stand.
pub const MACHINE_APPLY_FAILED_PREFIX: &str = "atpkg: machine settings failed — ";

/// The `machine-settings:` line for `entries` (already non-empty).
#[must_use]
pub fn machine_settings_line(entries: &[String]) -> String {
    format!("{MACHINE_SETTINGS_MARKER}{}", entries.join("; "))
}

/// Report the actual Universal Control outcome without treating a failed preference
/// write as an applied one.
fn record_universal_control_outcome(
    outcome: crate::machine::UniversalControlOutcome,
    entries: &mut Vec<String>,
) -> Option<String> {
    use crate::machine::UniversalControlOutcome;
    match outcome {
        UniversalControlOutcome::Disabled => {
            entries.push(crate::machine::UNIVERSAL_CONTROL_ENTRY.to_string());
            Some(format!(
                "atpkg: Universal Control disabled for this host (revert: {})",
                crate::machine::UNIVERSAL_CONTROL_REVERT
            ))
        }
        UniversalControlOutcome::WriteFailed => Some(format!(
            "{MACHINE_APPLY_FAILED_PREFIX}Universal Control could not be disabled for this host — \
             the preference write did not land (`aterm pkg doctor` reads the keys)"
        )),
        UniversalControlOutcome::AlreadyDisabled | UniversalControlOutcome::NotApplicable => None,
    }
}

/// Report the actual Rosetta 2 outcome: an install that did not land, or could not be
/// tried on an unproven read, is a failure line, never a change.
fn record_rosetta_outcome(
    outcome: crate::machine::RosettaOutcome,
    entries: &mut Vec<String>,
) -> Option<String> {
    use crate::machine::RosettaOutcome;
    match outcome {
        RosettaOutcome::Installed => {
            entries.push(crate::machine::ROSETTA_ENTRY.to_string());
            Some(
                "atpkg: Rosetta 2 installed on this Mac (softwareupdate --install-rosetta, as \
                 you); macOS keeps it, and [machine] rosetta = false stops aterm installing it \
                 again"
                    .to_string(),
            )
        }
        RosettaOutcome::Failed(why) => Some(format!(
            "{MACHINE_APPLY_FAILED_PREFIX}Rosetta 2 could not be installed on this Mac — {why} \
             (`aterm pkg machine` reads it)"
        )),
        RosettaOutcome::AlreadyPresent | RosettaOutcome::NotApplicable => None,
    }
}

/// The `machine-state:` record of this machine as it reads now — Universal Control's keys,
/// Rosetta 2's runtime, the synthetic-home rule — and the home refusal, if any: one builder
/// for bare `atpkg machine` and the apply's own record, so the window has one parser.
fn machine_state_now(
    cfg: &crate::config::MachineConfig,
    policy: crate::config::UniversalControlPolicy,
) -> (crate::machine::MachineState, Option<String>) {
    let state = crate::machine::universal_control_state(&crate::machine::SystemDefaults);
    let (home_posture, mismatch) = machine_home();
    let st = crate::machine::MachineState {
        universal_control: state.posture(),
        policy,
        home: home_posture,
        config_unreadable: cfg.unreadable,
        rosetta: crate::machine::rosetta_posture(&crate::machine::SystemRosetta),
        rosetta_wanted: cfg.rosetta(),
    };
    (st, mismatch)
}

/// Print the pass's own `machine-state:` record, the machine re-read (not the apply's
/// outcome), from the builder bare `atpkg machine` uses, so the window has one parser.
fn print_machine_state_after_apply(
    cfg: &crate::config::MachineConfig,
    policy: crate::config::UniversalControlPolicy,
) {
    let (st, _) = machine_state_now(cfg, policy);
    emit_marker_line(&format!(
        "atpkg: {MACHINE_STATE_MARKER}{}",
        crate::machine::machine_state_line(&st)
    ));
}

/// Apply the `[machine]` settings — from `atpkg machine apply`, and at a pass's edge when
/// the table changed ([`machine_table_changed`]) — print what changed as the
/// `machine-settings:` marker (nothing when nothing changed: the window records a change,
/// not a state), and record the table applied. Returns the same entries and why nothing
/// changed ([`MachineApply`]). Best-effort: a machine setting must never fail the
/// toolchain pass that carried it.
fn apply_machine_settings() -> MachineApply {
    let cfg = crate::config::cached_machine();
    let mut out = MachineApply::default();
    let entries = &mut out.entries;
    // An orphaned pass leaves the machine to the successor: its `machine-settings:` line
    // would reach no window, and the setting is idempotent, so the successor's pass applies
    // and reports it.
    if parent_is_gone() {
        return out;
    }
    // Off macOS there is nothing to apply and nothing to say: the home guard below would
    // otherwise print on every Linux pass whose HOME is not the passwd home (CI).
    if !cfg!(target_os = "macos") {
        return out;
    }
    // A synthetic home is a synthetic machine: `defaults` writes the account's per-host
    // domain and ignores `$HOME`, so a pass pointed at another home (this crate's
    // integration tests) must not touch the real machine. One line says why.
    if let Some(reason) = machine_refusal(cfg) {
        println!("{MACHINE_NOT_APPLIED_PREFIX}{reason}");
        return out;
    }
    // Once: `universal_control()` warns on stderr about a spelling it does not know, and the
    // record below asks for the policy too.
    let policy = cfg.universal_control();
    let outcome = crate::machine::apply_universal_control(policy, &crate::machine::SystemDefaults);
    let uc_failed = outcome == crate::machine::UniversalControlOutcome::WriteFailed;
    out.failed |= uc_failed;
    if policy == crate::config::UniversalControlPolicy::Leave {
        out.left_alone.push("universal_control = \"leave\"");
    } else {
        out.acts = true;
    }
    if let Some(line) = record_universal_control_outcome(outcome, entries) {
        println!("{line}");
    }
    // Rosetta 2, where it applies (macOS on Apple silicon): installed when missing, as
    // this user — the one `softwareupdate` verb atpkg runs (owner ruling R1).
    if crate::machine::ROSETTA_APPLIES {
        if cfg.rosetta() {
            out.acts = true;
        } else {
            out.left_alone.push("rosetta = false");
        }
    }
    let rosetta = crate::machine::apply_rosetta(cfg.rosetta(), &crate::machine::SystemRosetta);
    let rosetta_failed = matches!(rosetta, crate::machine::RosettaOutcome::Failed(_));
    out.failed |= rosetta_failed;
    if let Some(line) = record_rosetta_outcome(rosetta, entries) {
        println!("{line}");
    }
    if !entries.is_empty() {
        emit_marker_line(&format!("atpkg: {}", machine_settings_line(entries)));
    }
    // The record follows the apply, so a window that just read the change line has the
    // state a moment later instead of spawning a read of its own.
    print_machine_state_after_apply(cfg, policy);
    // What the passes compare against ([`machine_table_changed`]): only an apply that ran
    // gets here, so a refusal is retried by the next pass; one whose `defaults` write
    // failed forgets the table, so the next pass retries it.
    if let Some(layout) = crate::store::resolve_configured() {
        if uc_failed || rosetta_failed {
            crate::machine::forget_applied(&layout);
        } else {
            crate::machine::record_applied(&layout, cfg);
        }
    }
    out
}

/// What one [`apply_machine_settings`] did: the `machine-settings:` entries it changed, and
/// — for `atpkg machine apply`'s verdict when nothing changed — why.
#[derive(Debug, Default)]
struct MachineApply {
    /// What changed (the `machine-settings:` entries); empty when nothing did.
    entries: Vec<String>,
    /// A change did not land: a `machine settings failed —` line was printed.
    failed: bool,
    /// The `[machine]` settings switched off here, as spelled (`universal_control =
    /// "leave"`, `rosetta = false`).
    left_alone: Vec<&'static str>,
    /// Some setting this apply acts on is switched on here.
    acts: bool,
}

impl MachineApply {
    /// `atpkg machine apply`'s verdict when nothing changed: the one cause this apply saw.
    /// The switches are the cause only when every setting that applies here is off.
    fn nothing_changed(&self) -> String {
        if self.failed {
            "nothing changed — a change did not land".to_string()
        } else if !self.acts && !self.left_alone.is_empty() {
            format!("nothing changed — [machine] {}", self.left_alone.join(", "))
        } else {
            "nothing changed — already applied".to_string()
        }
    }
}

/// `<prefix>/machine.lock`, held (blocking) for one apply of the `[machine]` settings: the
/// window's launch apply, a session's and a pass edge carrying an edit may start at once,
/// and must not read and write the same per-host keys together. The prefix is made first,
/// as the store lock makes it ([`crate::store::Layout::ensure_dir`], write-free over one that
/// exists): on a first launch there is none yet, and two applies would run unqueued side by
/// side. `None` (unqueued) only for a prefix that cannot be made (a link, someone else's
/// directory) or an unopenable lock file. Released by `LOCK_UN` when the guard drops, not
/// by the close alone: a child forked meanwhile (the window's shells) keeps a copy of the
/// description until it execs, which would hold the queue.
fn machine_apply_queue() -> Option<crate::lock::Flock> {
    machine_apply_queue_in(&crate::store::resolve_configured()?)
}

/// [`machine_apply_queue`] in `layout`'s prefix.
fn machine_apply_queue_in(layout: &crate::store::Layout) -> Option<crate::lock::Flock> {
    crate::lock::Flock::lock(machine_lock_file(layout)?).ok()
}

/// `<prefix>/machine.lock`, opened for [`machine_apply_queue`] — the prefix made first — or
/// `None` where it cannot be.
fn machine_lock_file(layout: &crate::store::Layout) -> Option<std::fs::File> {
    layout.ensure_dir(&layout.prefix).ok()?;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(layout.prefix.join("machine.lock"))
        .ok()
}

/// A pass edge's turn at the `[machine]` queue ([`apply_machine_edit`]), never waited for.
enum MachineTurn {
    /// Taken: the edge applies, holding it.
    Mine(crate::lock::Flock),
    /// Another apply holds it now.
    Busy,
    /// No queue to take ([`machine_apply_queue`]'s `None`): the edge applies unqueued.
    Unqueued,
}

/// The edge's [`MachineTurn`]: the queue taken at once, held by another apply, or not there.
fn machine_apply_turn() -> MachineTurn {
    crate::store::resolve_configured().map_or(MachineTurn::Unqueued, |layout| {
        machine_apply_turn_in(&layout)
    })
}

/// [`machine_apply_turn`] in `layout`'s prefix.
fn machine_apply_turn_in(layout: &crate::store::Layout) -> MachineTurn {
    let Some(file) = machine_lock_file(layout) else {
        return MachineTurn::Unqueued;
    };
    match crate::lock::Flock::try_lock(file) {
        Ok(turn) => MachineTurn::Mine(turn),
        Err(std::fs::TryLockError::WouldBlock) => MachineTurn::Busy,
        Err(std::fs::TryLockError::Error(_)) => MachineTurn::Unqueued,
    }
}

/// A pass's `[machine]` edit ([`main_entry`]), applied beside the pass: on its own thread,
/// started before the store lock is asked for and joined when this guard drops (or at
/// [`Self::join`]). The `defaults` reads and writes — tens of milliseconds — and, on an
/// Apple-silicon Mac without Rosetta 2, its install (about a second from the package macOS
/// keeps on disk, at most [`crate::machine::ROSETTA_INSTALL_WITHIN`] when it must fetch
/// it; none of it while another apply holds the queue, [`apply_machine_edit`]) then delay
/// neither the lock attempt nor the work after it, and the edit has still landed, and said
/// so on stdout, before the pass exits: the join waits for the install.
struct MachineEdit(Option<std::thread::JoinHandle<()>>);

impl MachineEdit {
    /// Start the apply ([`apply_machine_edit`]); a thread that cannot be started applies
    /// here, in line.
    fn start() -> Self {
        match std::thread::Builder::new()
            .name("atpkg-machine-edit".into())
            .spawn(apply_machine_edit)
        {
            Ok(apply) => Self(Some(apply)),
            Err(_) => {
                apply_machine_edit();
                Self(None)
            }
        }
    }

    /// Wait for the apply to end. A panic in it ends the apply only: the pass's own
    /// output keeps its contract.
    fn join(&mut self) {
        if let Some(apply) = self.0.take() {
            let _ = apply.join();
        }
    }
}

impl Drop for MachineEdit {
    fn drop(&mut self) {
        self.join();
    }
}

/// The edge's apply, only if no apply just applied this very table — and NONE WHILE ANOTHER
/// APPLY HOLDS THE QUEUE: that one (the window's or a session's launch apply, which a first
/// launch starts beside its seed) applies the table and says so on its own lane, and an edit
/// it started too early to read is carried by the next pass's edge — the edge does not wait
/// it out. Waiting held the first launch's seed, and the update that installs behind it,
/// until the window's `defaults` writes were done.
fn apply_machine_edit() {
    let _turn = match machine_apply_turn() {
        MachineTurn::Busy => return,
        MachineTurn::Mine(turn) => Some(turn),
        MachineTurn::Unqueued => None,
    };
    if machine_table_changed() {
        let _ = apply_machine_settings();
    }
}

/// Whether this verb is a pass that applies the `[machine]` settings when their table
/// changed ([`machine_table_changed`]): the three long passes and `install <name>`.
/// `update <vendor program>` never does (the edge checks its operand): it is the lane a
/// `claude update` runs, and must not write a host preference.
fn verb_applies_machine_settings(verb: &str) -> bool {
    verb_row(verb).is_some_and(|v| v.applies_machine)
}

/// Whether the `[machine]` table differs from the one last applied on this machine
/// ([`crate::machine::changed_since_applied`]); never applied counts as changed. The
/// settings' own verb keeps them applied on the window's and session's schedule; a pass
/// only carries an edit.
fn machine_table_changed() -> bool {
    crate::store::resolve_configured().is_some_and(|layout| {
        crate::machine::changed_since_applied(&layout, crate::config::cached_machine())
    })
}

/// Why the `[machine]` settings must not be applied in this process, or `None`: the config
/// refusal first (cheaper, and the owner's to fix, [`machine_config_refusal`]), then the
/// synthetic-home rule ([`machine_home`]).
fn machine_refusal(cfg: &crate::config::MachineConfig) -> Option<String> {
    machine_config_refusal(cfg).or_else(|| machine_home().1)
}

/// [`machine_refusal`]'s config half: a config file that exists and does not parse cannot
/// tell us whether either setting was switched off, and both defaults act.
fn machine_config_refusal(cfg: &crate::config::MachineConfig) -> Option<String> {
    cfg.unreadable.then(|| {
        "aterm.toml does not parse, so the [machine] opt-outs could not be read — fix the \
         file (`aterm --window --validate-config`), then `aterm pkg machine apply`"
            .to_string()
    })
}

/// The synthetic-home rule against this process: the `machine-state:` posture, and the
/// refusal sentence when `$HOME` is not the account's home.
fn machine_home() -> (crate::machine::HomePosture, Option<String>) {
    // An empty `HOME` is not a home: `home_dir()` answers `Some("")` for `HOME=`, which
    // would render as a blank in the refusal sentence.
    let home = aterm_types::dirs::home_dir().filter(|h| !h.as_os_str().is_empty());
    let account = crate::platform::account_home();
    let same = match (&home, &account) {
        (Some(h), Some(a)) => same_directory(h, a),
        _ => false,
    };
    machine_home_verdict(home.as_deref(), account.as_deref(), same)
}

/// The decision behind [`machine_home`], on its inputs. Fail-closed when `HOME` is set: an
/// account that could not be resolved is no proof that `$HOME` is it, so nothing is
/// applied. No `HOME` at all is treated as the account's home: there is no redirect to
/// guard against. `same` is the filesystem's own answer ([`same_directory`]) for the
/// spellings a string compare gets wrong — a symlinked, case-variant or firmlinked home.
fn machine_home_verdict(
    home: Option<&std::path::Path>,
    account: Option<&std::path::Path>,
    same: bool,
) -> (crate::machine::HomePosture, Option<String>) {
    use crate::machine::HomePosture;
    let Some(home) = home else {
        return (HomePosture::Account, None);
    };
    let Some(account) = account else {
        return (
            HomePosture::Unresolved,
            Some(format!(
                "HOME is {} but this account's home could not be resolved (getpwuid_r); \
                 per-host preferences follow the account, and HOME cannot be proven to be it",
                home.display()
            )),
        );
    };
    if same {
        return (HomePosture::Account, None);
    }
    match machine_home_mismatch_between(home, account) {
        None => (HomePosture::Account, None),
        Some(reason) => (HomePosture::Mismatch, Some(reason)),
    }
}

/// Whether two paths name one directory (device and inode, following symlinks), so a
/// symlinked home, a case variant and the `/System/Volumes/Data/Users//x` firmlink spelling
/// (which `canonicalize` does not unify) read as the account home. A missing path is `false`.
#[cfg(unix)]
fn same_directory(a: &std::path::Path, b: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_directory(_a: &std::path::Path, _b: &std::path::Path) -> bool {
    false
}

/// The decision behind [`machine_home`]'s mismatch sentence, on its inputs.
fn machine_home_mismatch_between(
    home: &std::path::Path,
    account: &std::path::Path,
) -> Option<String> {
    if home == account {
        return None;
    }
    Some(format!(
        "HOME is {} but this account's home is {}; a redirected home is a synthetic \
         machine, and per-host preferences follow the account, not HOME",
        home.display(),
        account.display()
    ))
}

/// Why `machine cache` says one build cache folder would go, in the words of the rule
/// (`crate::build_cache::Unused`).
fn cache_plan_words(why: crate::build_cache::Unused) -> String {
    use crate::build_cache::Unused;
    let days = |d: std::time::Duration| {
        let h = d.as_secs() / 3_600;
        if h < 48 {
            format!("{h}h")
        } else {
            format!("{}d", h / 24)
        }
    };
    match why {
        Unused::Gone { idle, after } => format!(
            "goes whole: its workspace is gone and nothing wrote it for {} (the pass takes one \
             unwritten {})",
            days(idle),
            days(after)
        ),
        Unused::Idle { idle } => format!(
            "goes whole: nothing wrote it for {}, a month or more",
            days(idle)
        ),
        Unused::Unfinished => {
            "an earlier pass moved it aside and did not finish removing it".to_owned()
        }
        Unused::Stale {
            incremental,
            units,
            after,
        } => format!(
            "stays; {incremental} incremental crate folder(s) and {units} unit(s) built by a \
             compiler that is no longer the store's current one go, each unwritten for {} or \
             more",
            days(after)
        ),
    }
}

/// Bare `atpkg machine`'s Rosetta 2 sentence (after the verdict prefix): the doctor's row
/// without its level's next step, or why the setting is inert here.
fn rosetta_state_line(posture: crate::machine::RosettaPosture, wanted: bool) -> String {
    posture.doctor_row(wanted).map_or_else(
        || {
            "note — Rosetta 2: not applicable — [machine] rosetta acts only in an atpkg built \
             for Apple silicon (arm64), and this one is built for x86_64"
                .to_string()
        },
        |row| crate::doctor::spelled(&row),
    )
}

/// Bare `atpkg machine`'s verdict (after the verdict prefix), from the record and the
/// refusal: what an apply would do, that nothing is left, or why nothing is applied here.
fn machine_read_verdict(st: &crate::machine::MachineState, refusal: Option<String>) -> String {
    if let Some(reason) = refusal {
        return format!("not applied here — {reason}");
    }
    let mut would = Vec::new();
    if st.universal_control_next() {
        would.push("Universal Control off for this host");
    }
    if st.rosetta_next() {
        would.push("Rosetta 2 installed");
    }
    if would.is_empty() {
        use crate::machine::RosettaPosture;
        // An unread runtime is not where [machine] wants it: it is unknown, and the apply
        // installs nothing on it, so the verdict says that instead of vouching for it.
        let settings = match st.rosetta {
            RosettaPosture::NotApplicable => "Universal Control is where [machine] wants it",
            RosettaPosture::Unknown => {
                "Universal Control is where [machine] wants it; Rosetta 2 could not be read, \
                 and nothing is installed on that reading"
            }
            RosettaPosture::Present | RosettaPosture::Missing => {
                "Universal Control and Rosetta 2 are where [machine] wants them"
            }
        };
        format!("nothing to apply — {settings}")
    } else {
        format!("next — aterm pkg machine apply ({})", would.join("; "))
    }
}

/// Run the existing cache reclamation and report only completed removals. No host
/// settings are read or applied here. Return how many cache folders lost entries.
fn reclaim_build_cache() -> usize {
    let removed = crate::build_cache::reclaim_home();
    let parts = removed
        .iter()
        .filter(|(_, why)| matches!(why, crate::build_cache::Unused::Stale { .. }))
        .count();
    if removed.len() > parts {
        println!(
            "removed {} unused Rust build cache folder(s)",
            removed.len() - parts
        );
    }
    if parts > 0 {
        println!(
            "removed stale incremental folders and old-compiler units from {parts} Rust \
             build cache folder(s)"
        );
    }
    removed.len()
}

/// `atpkg machine [apply|cache [clean]]` — the `[machine]` settings' own door.
///
/// Bare `atpkg machine` reads: the doctor's Universal Control and Rosetta 2 lines, the
/// byte-stable [`MACHINE_STATE_MARKER`] record, and one verdict — `next — …` naming what an
/// apply would do, `nothing to apply`, or `not applied here — <why>` on a home that is not
/// this account's. `atpkg machine apply` runs [`apply_machine_settings`] and then says what
/// changed, that nothing changed, or why nothing was applied, then runs the day's build cache
/// housekeeping (`crate::build_cache::reclaim_home`); `atpkg machine cache` is that
/// housekeeping's dry run (`crate::build_cache::plan_home`), each folder sized. Exit 0 either
/// way: this is a state, not a gate. `atpkg machine cache clean` runs only the cache
/// housekeeping, without reading or applying the host settings. Anything else is a usage error.
fn cmd_machine(rest: &[String]) -> ExitCode {
    match rest {
        [] => {
            if !cfg!(target_os = "macos") {
                println!("{MACHINE_VERDICT_PREFIX}not applicable — these are macOS host settings");
                return ExitCode::SUCCESS;
            }
            let cfg = crate::config::cached_machine();
            // Once: `universal_control()` warns on stderr about a spelling it does not know.
            let policy = cfg.universal_control();
            let state = crate::machine::universal_control_state(&crate::machine::SystemDefaults);
            println!("{MACHINE_VERDICT_PREFIX}{}", state.state_line(policy));
            let (st, mismatch) = machine_state_now(cfg, policy);
            println!(
                "{MACHINE_VERDICT_PREFIX}{}",
                rosetta_state_line(st.rosetta, st.rosetta_wanted)
            );
            // The line the window reads — after the prose, so a person sees sentences
            // first, and after the platform check, so its absence means "not macOS".
            emit_marker_line(&format!(
                "atpkg: {MACHINE_STATE_MARKER}{}",
                crate::machine::machine_state_line(&st)
            ));
            // One verdict for both surfaces: `st.next()` is what the window's Apply button
            // reads too, so the CLI cannot say "nothing to apply" while an apply would act.
            let refusal = machine_config_refusal(cfg).or(mismatch);
            println!(
                "{MACHINE_VERDICT_PREFIX}{}",
                machine_read_verdict(&st, refusal)
            );
            ExitCode::SUCCESS
        }
        [sub] if sub == "apply" => {
            if !cfg!(target_os = "macos") {
                println!("{MACHINE_VERDICT_PREFIX}not applicable — these are macOS host settings");
                return ExitCode::SUCCESS;
            }
            // Asked here as well as inside the apply, so the verdict never says "nothing
            // changed" under the refusal line.
            if let Some(reason) = machine_refusal(crate::config::cached_machine()) {
                let _ = apply_machine_settings();
                println!("{MACHINE_VERDICT_PREFIX}not applied — {reason}");
                return ExitCode::SUCCESS;
            }
            // The raw apply: `machine apply` is the explicit gesture, so running it twice
            // reads and writes the machine twice. (The edge never applies for this verb:
            // it is not in `verb_applies_machine_settings`.)
            let queue = machine_apply_queue();
            let applied = apply_machine_settings();
            // The queue guards the per-host keys, not the cache walk: let it go before the
            // housekeeping below. A pass's edge queued behind this apply ([`MachineEdit`],
            // the launch seed's on a first launch) is joined before its pass exits, and the
            // window starts the install only once that seed has exited, so a walk (and the
            // removal of gigabytes) under the queue would hold the toolchain install back.
            drop(queue);
            if applied.entries.is_empty() {
                // The window's card shows this sentence alone, so it names the one cause
                // the apply saw.
                println!("{MACHINE_VERDICT_PREFIX}{}", applied.nothing_changed());
            } else {
                println!(
                    "{MACHINE_VERDICT_PREFIX}applied — {}",
                    applied.entries.join("; ")
                );
            }
            // The day's housekeeping rides the day's apply (`launch_apply_due`), after its
            // verdict: Rust build cache folders nobody builds into any more go, and from
            // the ones that stay what no build reads again.
            reclaim_build_cache();
            ExitCode::SUCCESS
        }
        [sub, action] if sub == "cache" && action == "clean" => {
            if !cfg!(target_os = "macos") {
                println!("not applicable — the Rust build cache is kept on macOS only");
                return ExitCode::SUCCESS;
            }
            if reclaim_build_cache() == 0 {
                println!("no unused Rust build cache entries removed");
            }
            ExitCode::SUCCESS
        }
        // The housekeeping's dry run: what the day's apply would take from the build cache
        // now, and why. Nothing is moved; each build directory's cargo locks are only tried.
        [sub] if sub == "cache" => {
            if !cfg!(target_os = "macos") {
                println!("not applicable — the Rust build cache is kept on macOS only");
                return ExitCode::SUCCESS;
            }
            let planned = crate::build_cache::plan_home();
            let mut total = 0u64;
            for one in &planned {
                let bytes = crate::build_cache::allocated(&one.paths);
                total = total.saturating_add(bytes);
                println!(
                    "{:>10}  {}\n            {}{}",
                    crate::cost::human_bytes(bytes),
                    one.path.display(),
                    cache_plan_words(one.why),
                    if one.held {
                        " — a build holds its locks now, so a pass now keeps it"
                    } else {
                        ""
                    }
                );
            }
            println!(
                "{} to go from {} Rust build cache folder(s) at the next `aterm pkg machine \
                 apply` (nothing removed now)",
                crate::cost::human_bytes(total),
                planned.len()
            );
            ExitCode::SUCCESS
        }
        // Name the word that was refused: `machine help` (a reasonable guess) would
        // otherwise look identical to a typo.
        other => {
            let problem = if let [word] = other {
                format!(
                    "unknown argument {word:?} — a bare `machine` reads, `machine apply` \
                     applies, `machine cache` shows what the apply would take from the build \
                     cache, and `machine cache clean` reclaims only that cache"
                )
            } else {
                format!(
                    "too many arguments ({}) — a bare `machine` reads, `machine apply` \
                     applies, `machine cache` shows what the apply would take from the build \
                     cache, and `machine cache clean` reclaims only that cache",
                    other.len()
                )
            };
            usage_error("machine", Some(&problem))
        }
    }
}

/// One agent program on the `managed-current:` line ([`managed_current_entries`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedAgent {
    /// `claude`, `codex`.
    pub(crate) name: String,
    /// The version its active build is.
    pub(crate) version: String,
    /// The parenthesis: `<Vendor> latest` while a check that reached the vendor this run
    /// (and within a day) found it the head; [`NOT_CHECKED`] when this run could not reach
    /// the vendor or no check has for a day; else [`crate::state::not_current`]'s word for
    /// its row (`held`, `failed`, …).
    pub(crate) standing: String,
}

/// The standing of a managed agent program whose vendor this run could not reach, or no
/// check has reached for a day: its row is an older check's ([`ManagedAgent::standing`]).
pub const NOT_CHECKED: &str = "not checked";

/// The `managed-current:` line ([`MANAGED_CURRENT_MARKER`]) for `entries`, in roster order:
/// `claude 2.1.280 (Anthropic latest); codex 0.156.0 (not checked)`. `None` with no entry.
fn managed_current_line(entries: &[ManagedAgent]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let parts: Vec<String> = entries
        .iter()
        .map(|a| format!("{} {} ({})", a.name, a.version, a.standing))
        .collect();
    Some(format!("{MANAGED_CURRENT_MARKER}{}", parts.join("; ")))
}

/// Whether `agents/<name>`, the copy every aterm tab runs, is laid and resolves exactly where
/// `bin/<name>` does, into `store/<name>/<build>`: the predicate behind the
/// `managed-current:` line and the agent programs' shadow wording.
pub(crate) fn agent_twin_current(layout: &crate::store::Layout, name: &str, build: u64) -> bool {
    let Some(tool) = crate::store::ToolName::new(name) else {
        return false;
    };
    let Some(target) = crate::platform::resolve_shim(&layout.shim(&tool)) else {
        return false;
    };
    if !crate::ops::store_build_of(&layout.prefix, &target)
        .is_some_and(|(p, b)| p == name && b == build)
    {
        return false;
    }
    crate::activate::twin_target(layout, name).is_some_and(|t| t == target)
}

/// [`agent_twin_current`] and the twin's bytes carry the self-update hand-over
/// ([`twin_carries_hand_over`]) — the predicate behind the `which` sentence `` `claude
/// update` here runs `aterm pkg update claude` ``. Target equality is not enough: a twin an
/// older client laid still runs the vendor's updater until a pass re-lays it, and the
/// `.cmd` twin carries no block ([`crate::platform::twin_prelude`] renders nothing on
/// Windows). One bounded, non-link regular read; anything unreadable answers `false`.
pub(crate) fn agent_twin_intercepts(layout: &crate::store::Layout, name: &str, build: u64) -> bool {
    if !agent_twin_current(layout, name, build) {
        return false;
    }
    let Some(tool) = crate::store::ToolName::new(name) else {
        return false;
    };
    crate::metadata_io::read_bounded_regular(
        &layout.agent_shim(&tool),
        crate::platform::MAX_SHIM_BYTES,
    )
    .is_ok_and(|bytes| twin_carries_hand_over(&bytes))
}

/// Whether a laid twin's bytes carry the hidden verb's hand-over: the word
/// [`crate::selfupdate::HIDDEN_VERB`] appears in no shim this crate lays except in the `sh`
/// self-update block (an agent program's twin and its `bin/` shim; the `.cmd` twin renders
/// none). Pure, so the Windows bytes are pinned from the Unix suite.
fn twin_carries_hand_over(bytes: &[u8]) -> bool {
    bytes
        .windows(crate::selfupdate::HIDDEN_VERB.len())
        .any(|w| w == crate::selfupdate::HIDDEN_VERB.as_bytes())
}

/// The pass log's tail for an agent program with a foreign copy ahead on the pass's own
/// `PATH`: `a foreign copy is ahead on this pass's PATH (<path>); a tab opened on this build
/// puts agents/ first on its own`. A fact about this process, not about a user's shell.
fn pass_path_tail(foreign: &std::path::Path) -> String {
    let mut s = String::from("a foreign copy is ahead on this pass's PATH (");
    s.push_str(&foreign.display().to_string());
    s.push_str("); a tab opened on this build puts agents/ first on its own");
    s
}

/// The in-place remedy for a shell whose `PATH` lacks `agents/` (or has it behind a foreign
/// copy): source the hook where it stands (`. ~/.aterm/shell.d/00-atpkg.zsh`, `source …fish`,
/// `. …ps1` for pwsh) when that hook file exists for the invoking shell
/// ([`crate::hooks::hook_file`]; the shell from [`crate::caller_shell`], since aterm does
/// not re-point `$SHELL`), else a `PATH` line in the shell's own dialect. Never `exec
/// $SHELL`: inside an aterm tab a re-exec'd shell loses the tab's shell integration (the zsh
/// wrapper consumes `ATERM_ORIGINAL_ZDOTDIR`, bash rides `--rcfile`).
pub(crate) fn shell_remedy_command(layout: &crate::store::Layout) -> String {
    let shell = crate::caller_shell::invoking_shell();
    let home = aterm_types::dirs::home_dir();
    shell_remedy_for(shell.as_deref(), home.as_deref(), layout)
}

/// [`shell_remedy_command`] over an explicit `shell` name and `home`, for the tests.
pub(crate) fn shell_remedy_for(
    shell: Option<&str>,
    home: Option<&std::path::Path>,
    layout: &crate::store::Layout,
) -> String {
    let hook = match (shell, home) {
        (Some(sh), Some(home)) => crate::hooks::hook_file(home, sh).map(|(_, hook)| hook),
        _ => None,
    };
    if let Some(hook) = hook {
        let mut s = String::from(if hook.ends_with(".fish") {
            "source "
        } else {
            ". "
        });
        s.push_str("~/.aterm/shell.d/");
        s.push_str(hook);
        return s;
    }
    let agents = layout.agents_dir().display().to_string();
    let mut s = String::new();
    match shell.unwrap_or_default() {
        "fish" => {
            s.push_str("set -gx PATH \"");
            s.push_str(&agents);
            s.push_str("\" $PATH");
        }
        "pwsh" | "powershell" => {
            s.push_str("$env:PATH = \"");
            s.push_str(&agents);
            s.push_str(if cfg!(windows) { ";" } else { ":" });
            s.push_str("$env:PATH\"");
        }
        _ => {
            s.push_str("export PATH=\"");
            s.push_str(&agents);
            s.push_str(":$PATH\"");
        }
    }
    s
}

/// What an agent program's reroute stub ([`crate::reroute::agents_stub_body_sh`]) does on
/// one shell's `PATH` and environment. Asked before walking PATH for a shadow: with the
/// stub first the walk stops at it and would read every shell as running the managed copy.
pub(crate) enum AgentStub {
    /// No stub of ours answers first for `program` (or it is not an agent program): walk
    /// `PATH` as always.
    Absent,
    /// The stub answers first and execs the managed twin.
    Routes { stub: std::path::PathBuf },
    /// The stub answers first and passes through: `path` is what its walk sees.
    PassesThrough {
        stub: std::path::PathBuf,
        path: Option<std::ffi::OsString>,
    },
}

/// [`AgentStub`] for `program` on `path_var` under `env`.
pub(crate) fn agent_stub_on_path(
    layout: &crate::store::Layout,
    program: &str,
    path_var: Option<&std::ffi::OsStr>,
    env: crate::reroute::StubEnv,
) -> AgentStub {
    if !crate::stub::is_agent_program(program) {
        return AgentStub::Absent;
    }
    let Some(stub) = crate::reroute::agents_stub_first_on_path(layout, program, path_var) else {
        return AgentStub::Absent;
    };
    if crate::reroute::agents_stub_takes_twin(layout, program, env) {
        AgentStub::Routes { stub }
    } else {
        AgentStub::PassesThrough {
            stub,
            path: crate::reroute::pass_through_path(layout, path_var),
        }
    }
}

/// The foreign copy of `tool` that runs in a shell with `path_var` and `env` — the shadow
/// probe ([`crate::vendor::shadowing_binary_on_path`]) with an agent program's reroute stub
/// taken into account: none where the stub takes the managed twin, the probe over the
/// stub's own walk where it passes through, and over `path_var` where no stub of ours
/// answers first. The doctor and `which` both ask this, so they cannot disagree.
pub(crate) fn shadow_in_shell(
    layout: &crate::store::Layout,
    tool: &str,
    path_var: Option<&std::ffi::OsStr>,
    env: crate::reroute::StubEnv,
) -> Option<std::path::PathBuf> {
    shadow_through(
        layout,
        tool,
        path_var,
        &agent_stub_on_path(layout, tool, path_var, env),
    )
}

/// [`shadow_in_shell`] over an [`AgentStub`] the caller already has.
pub(crate) fn shadow_through(
    layout: &crate::store::Layout,
    tool: &str,
    path_var: Option<&std::ffi::OsStr>,
    answer: &AgentStub,
) -> Option<std::path::PathBuf> {
    match answer {
        AgentStub::Routes { .. } => None,
        AgentStub::PassesThrough { path, .. } => {
            crate::vendor::shadowing_binary_on_path(&layout.prefix, tool, path.as_deref())
        }
        AgentStub::Absent => {
            crate::vendor::shadowing_binary_on_path(&layout.prefix, tool, path_var)
        }
    }
}

/// The agent programs' row for a shell outside aterm whose reroute stub answers first and
/// passes through to `foreign`: the stub doing what it is for (an aterm-managed agent runs
/// in aterm only), said as the design ([`crate::state::agent_passed_through`]), never as
/// SHADOWED with a remedy. Likewise where no stub answers first: the shell's own copy
/// running there is the design ([`crate::state::agent_own_copy_outside_aterm`];
/// `docs/PLAN-agents-dir-aterm-only-2026-09-22.md` steps 6-8). `None` inside aterm, where a
/// pass-through means the twin is gone, and where the twin is not current.
pub(crate) fn agent_passed_row(
    layout: &crate::store::Layout,
    program: &str,
    build: u64,
    foreign: &std::path::Path,
    path_var: Option<&std::ffi::OsStr>,
    env: crate::reroute::StubEnv,
) -> Option<String> {
    if env.in_aterm || !agent_twin_current(layout, program, build) {
        return None;
    }
    match agent_stub_on_path(layout, program, path_var, env) {
        AgentStub::PassesThrough { stub, .. } => {
            Some(crate::state::agent_passed_through(build, &stub, foreign))
        }
        AgentStub::Absent => Some(crate::state::agent_own_copy_outside_aterm(build, foreign)),
        AgentStub::Routes { .. } => None,
    }
}

/// The agent programs' routed row for one shell: `Some` when `program`'s reroute stub
/// answers first on `path_var` and takes the current managed twin under `env`, and a
/// foreign copy stands behind it — what this shell would run without the stub
/// ([`crate::reroute::without_reroute_dirs`] keeps `agents/`)
/// ([`crate::state::agent_routed_in_shell`]). `None` otherwise.
pub(crate) fn agent_routed_row(
    layout: &crate::store::Layout,
    program: &str,
    build: u64,
    path_var: Option<&std::ffi::OsStr>,
    env: crate::reroute::StubEnv,
) -> Option<String> {
    let AgentStub::Routes { stub } = agent_stub_on_path(layout, program, path_var, env) else {
        return None;
    };
    if !agent_twin_current(layout, program, build) {
        return None;
    }
    let behind = crate::reroute::without_reroute_dirs(path_var);
    let foreign =
        crate::vendor::shadowing_binary_on_path(&layout.prefix, program, behind.as_deref())?;
    Some(crate::state::agent_routed_in_shell(build, &stub, &foreign))
}

/// The agent programs' shadow row for one shell: `Some` when `program`'s `agents/` twin is
/// laid and current, so `foreign` ahead of it on `path_var` is this shell's state, not the
/// machine's ([`crate::state::agent_shadowed_in_shell`]). `None` for every other program.
pub(crate) fn agent_shell_shadow(
    layout: &crate::store::Layout,
    program: &str,
    build: u64,
    foreign: &std::path::Path,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<String> {
    if !crate::stub::is_agent_program(program) || !agent_twin_current(layout, program, build) {
        return None;
    }
    Some(crate::state::agent_shadowed_in_shell(
        build,
        foreign,
        &layout.agents_dir(),
        crate::vendor::agents_dir_on_path(&layout.prefix, path_var),
        &shell_remedy_command(layout),
    ))
}

/// The entries behind [`managed_current_line`]: every [`crate::stub::AGENT_PROGRAMS`]
/// member with an active build its row names, known by a vendor version, whose `agents/`
/// twin is that build, with its standing ([`ManagedAgent::standing`]): its vendor's latest
/// when its row says so and a head check reached the vendor this run and within a day
/// before `now`; [`NOT_CHECKED`] when the vendor is `unchecked` or no check reached it for
/// a day; else the word for why its row is not the head. Offline.
fn managed_current_entries(
    layout: &crate::store::Layout,
    unchecked: &std::collections::BTreeSet<String>,
    now: i64,
) -> Vec<ManagedAgent> {
    let installed = crate::active_builds(layout);
    let status = crate::status::read(layout);
    crate::stub::AGENT_PROGRAMS
        .iter()
        .filter_map(|name| {
            let build = *installed.get(*name)?;
            let row = status.as_ref()?.programs.get(*name)?;
            if row.installed_build != Some(build) {
                return None;
            }
            // The line says the managed copies are what every tab runs — true only when
            // the front-of-PATH twin is this build.
            if !agent_twin_current(layout, name, build) {
                return None;
            }
            let checked = !unchecked.contains(*name)
                && crate::vendor_direct::ProgramStamp::read(layout, name)
                    .is_some_and(|s| s.checked_recently(now));
            // The version a person knows it by: its vendor row's, or its vendor build's.
            // A build an older index laid has neither, only a store id, and says nothing.
            let version = match crate::state::vendor_row(&row.state) {
                Some((version, _)) => version.to_string(),
                None if crate::vendor_direct::is_vendor_build(build) => {
                    crate::vendor_direct::build_label(build)
                }
                None => return None,
            };
            // A row that is not the head says why in its own word (a hold, a yank, a
            // refusal); one the vendor's channel has not judged lately is not checked.
            let standing = match (crate::state::vendor_latest(&row.state), checked) {
                (Some((_, vendor)), true) => format!("{vendor} latest"),
                (Some(_), false) => NOT_CHECKED.to_string(),
                (None, _) => match crate::state::not_current(&row.state) {
                    Some((word, _)) if word != "not reached" => word.to_string(),
                    _ => NOT_CHECKED.to_string(),
                },
            };
            Some(ManagedAgent {
                name: (*name).to_string(),
                version,
                standing,
            })
        })
        .collect()
}

/// Print the `managed-current:` marker at the end of every run of the vendor lane (the
/// window records it in its `appstatus` ledger and log), whatever the index lane did.
/// Offline: it reads the rows the vendor lane recorded ([`managed_current_entries`]).
fn print_managed_current(
    layout: &crate::store::Layout,
    unchecked: &std::collections::BTreeSet<String>,
    now: i64,
) {
    if let Some(line) = managed_current_line(&managed_current_entries(layout, unchecked, now)) {
        emit_marker_line(&format!("atpkg: {line}"));
    }
}

/// The positive terminal: an announced pass that ran to its end and has no richer marker
/// to print. Every ending has a line, so an absence means only that nobody answered.
pub const SEED_DONE_MARKER: &str = "seed-done: ";

/// ANOTHER `atpkg` IS ALREADY DOING THIS WORK. Printed when the store lock is
/// CONTENDED at the dispatch edge — never for any other refusal — and only to a
/// caller that opted into `--wait-lock` and whose parent is still there (the GUI's
/// machine lanes, exactly like the `lock-waiting:` line): a typed `aterm pkg` verb is
/// told on stderr and keeps its stdout silent, and the contention exit code
/// ([`crate::lock::CONTENDED_EXIT`], 75) says the same thing to every caller. That
/// code, not this line, is what the lanes key their "deferred, never failed" verdict
/// on; the line is the pass's own words for their log. ONE caller that is not a
/// machine lane opts in too: the self-update intercept's child
/// ([`cmd_selfupdate`], `update <program> --wait-lock 1800` with stdio inherited), so a
/// person who typed `claude update` while a pass holds the store sees the wait — a
/// spinner, or on a terminal that cannot draw one the `lock-waiting:` line as a sentence
/// ([`human_marker`]) — until the holder lets go, and, only when the whole 30 minutes
/// run out, this line, then the intercept's own contended line on stderr — pinned at
/// the edge by `tests/store_lock_wait.rs` (`a_waiting_seed_times_out_with_exit_75`) and
/// `tests/selfupdate_intercept.rs` (the wait that ends in the lock), the intercept's
/// relay of the 75 in-process by
/// `cmd_selfupdate_refuses_forwards_and_never_takes_the_lock`.
///
/// Without it the GUI's refusal branch would put "install failed" on a machine where the
/// install is RUNNING in another process. Contention is a fact about this process's
/// timing, not about the toolchain: it is
/// transient, self-healing, and the work is being done. Saying so costs one line and
/// removes a warning nobody can act on.
pub const SEED_BUSY_MARKER: &str = "seed-busy: ";

/// The announcement ledger — the marker contract's other half, kept by the process instead
/// of by each verb's exits. `net-starting:` opens a held "Installing the ALab toolchain…"
/// card in the GUI; every other marker closes it, so a verb that opened one must close it
/// on every return path. The bookkeeping lives where a marker reaches stdout
/// ([`emit_marker_line`], [`emit_marker_line_lossy`]), so a lane that grows a new early
/// return cannot forget to answer.
pub(crate) mod announcement {
    use std::sync::atomic::{AtomicBool, Ordering};

    /// What the process announced, and whether it answered.
    pub(crate) struct Ledger {
        opened: AtomicBool,
        answered: AtomicBool,
    }

    impl Ledger {
        const fn new() -> Self {
            Self {
                opened: AtomicBool::new(false),
                answered: AtomicBool::new(false),
            }
        }
    }

    /// THE process's ledger.
    static PROCESS: Ledger = Ledger::new();

    /// The ledger this thread's calls answer to: [`PROCESS`], in every build. A test
    /// thread may hold one of its own ([`own_ledger`]), so a test that reads the ledger
    /// sees no other test's pass, and the production rule (one ledger per process)
    /// stays unbent.
    fn ledger() -> &'static Ledger {
        #[cfg(test)]
        {
            if let Some(own) = TEST_LEDGER.get() {
                return own;
            }
        }
        &PROCESS
    }

    #[cfg(test)]
    thread_local! {
        /// TESTS ONLY: the ledger [`ledger`] answers on this thread, when not the process's.
        static TEST_LEDGER: std::cell::Cell<Option<&'static Ledger>> =
            const { std::cell::Cell::new(None) };
    }

    /// Every marker in the stdout contract, and whether it opens an announcement rather
    /// than answering one. The GUI's `parse_seed_line` matches the same set.
    ///
    /// `lock-waiting:` and `lock-acquired:` are deliberately absent: printed at the
    /// dispatch edge, they are neither half of the contract (were the first an answer, a
    /// child that waited and then announced would look answered before it began). [`note`]
    /// ignores an unlisted prefix.
    pub(crate) const MARKERS: &[(&str, bool)] = &[
        (super::NET_STARTING_MARKER, true),
        (super::SEED_UNUSABLE_MARKER, false),
        (super::SEED_FAILED_MARKER, false),
        (super::SEED_DONE_MARKER, false),
        (super::SEED_BUSY_MARKER, false),
        (super::NET_INSTALLED_MARKER, false),
        (super::NET_FAILED_MARKER, false),
    ];

    /// Record what a marker line just said. `line` is the whole stdout line,
    /// `atpkg: ` prefix and all — the same bytes the GUI parses.
    pub(crate) fn note(line: &str) {
        let Some(rest) = line.strip_prefix("atpkg: ") else {
            return;
        };
        for (marker, opens) in MARKERS {
            if rest.starts_with(marker) {
                if *opens {
                    ledger().opened.store(true, Ordering::Release);
                } else {
                    ledger().answered.store(true, Ordering::Release);
                }
                return;
            }
        }
    }

    /// Whether this process announced a pass and has not yet answered it.
    pub(crate) fn is_open() -> bool {
        let ledger = ledger();
        ledger.opened.load(Ordering::Acquire) && !ledger.answered.load(Ordering::Acquire)
    }

    /// TESTS ONLY: give this thread an empty ledger of its own until the guard drops.
    /// The process's ledger is shared by every test whose pass prints a marker, so a
    /// test that reads it beside them would read their announcements. The ledger is
    /// leaked (two words per test).
    #[cfg(all(test, unix))]
    pub(crate) fn own_ledger() -> OwnLedger {
        let own: &'static Ledger = Box::leak(Box::new(Ledger::new()));
        OwnLedger {
            previous: TEST_LEDGER.replace(Some(own)),
        }
    }

    /// TESTS ONLY: the guard [`own_ledger`] returns.
    #[cfg(all(test, unix))]
    pub(crate) struct OwnLedger {
        previous: Option<&'static Ledger>,
    }

    #[cfg(all(test, unix))]
    impl Drop for OwnLedger {
        fn drop(&mut self) {
            TEST_LEDGER.set(self.previous);
        }
    }
}

/// The way a marker reaches stdout: print it, and record it against the announcement
/// ledger. `println!`, not a lossy write: an installing child whose window quits has its
/// stdout re-pointed at a log by the orphan watch ([`crate::progress::watch_for_orphaning`]).
/// A waiter's lines go through [`emit_marker_line_lossy`] instead.
///
/// On a person's terminal the line is shown as a sentence ([`human_marker`]), or not at all
/// when a sentence beside it already says it; the ledger still records the marker. Every
/// pipe, and so every window child, gets the bytes.
fn emit_marker_line(line: &str) {
    if stdout_is_a_person() {
        if let Some(said) = human_marker(line) {
            println!("{said}");
        }
    } else {
        println!("{line}");
    }
    announcement::note(line);
    #[cfg(test)]
    MARKERS_SAID.with(|said| said.borrow_mut().push(line.to_string()));
}

#[cfg(test)]
thread_local! {
    /// Tests only: every marker line [`emit_marker_line`] printed on this thread (the
    /// announcement ledger is process-wide and parallel tests share it). Drained by
    /// [`take_markers_said`]. The doc sits inside the macro: on the invocation it would be
    /// `unused_doc_comments`.
    static MARKERS_SAID: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// TESTS ONLY: drain the marker lines this thread printed ([`MARKERS_SAID`]).
#[cfg(all(test, unix))]
fn take_markers_said() -> Vec<String> {
    MARKERS_SAID.with(|said| std::mem::take(&mut *said.borrow_mut()))
}

/// [`emit_marker_line`] for the `--wait-lock` waiter's lines at the lock edge
/// ([`mutator_store_lock`]: `lock-waiting:`, `lock-acquired:`, `seed-busy:`): the same
/// bytes and ledger entry, but a write whose failure is ignored — a waiter whose window
/// quits after the [`parent_is_gone`] check must stand down silently with 75, not panic.
fn emit_marker_line_lossy(line: &str) {
    use std::io::Write as _;
    if stdout_is_a_person() {
        if let Some(said) = human_marker(line) {
            crate::meter::around(|| {
                let _ = writeln!(std::io::stdout(), "{said}");
            });
        }
    } else {
        crate::meter::around(|| {
            let _ = writeln!(std::io::stdout(), "{line}");
        });
    }
    announcement::note(line);
}

/// A marker line as a person reads it: the plain sentence a terminal shows in its place
/// ([`emit_marker_line`]), or `None` when it says nothing a person needs — a record for the
/// window's ledger (`managed-current:`, `machine-state:`), or a fact a line beside it
/// already says (`group-aborted:`, `lock-acquired:`, `shadowed:`, `foreign:`, and a
/// `machine-settings:` line whose one change is Universal Control). A line that is no
/// marker is itself.
fn human_marker(line: &str) -> Option<String> {
    let Some(rest) = line.strip_prefix("atpkg: ") else {
        return Some(line.to_string());
    };
    let marked = |marker: &str| rest.strip_prefix(marker);
    // The first number in a body — the count a line leads with.
    let count = |body: &str| -> Option<u64> {
        let digits: String = body
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    };
    // The trailing size group, `(~2.1 GiB download, ~6.5 GiB on disk)` — the part the
    // window slices off these lines too.
    let size = |body: &str| -> String {
        body.strip_suffix(')')
            .and_then(|b| b.rsplit_once(" ("))
            .map(|(_, group)| format!(" ({group})"))
            .unwrap_or_default()
    };
    let programs = |n: u64| if n == 1 { "program" } else { "programs" };
    // A body that is already a sentence, as one: capitalized, and full-stopped.
    let sentence = |body: &str| -> String {
        let mut chars = body.trim().chars();
        let mut out: String = chars
            .next()
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_default();
        out.push_str(chars.as_str());
        if !out.ends_with(['.', ')']) {
            out.push('.');
        }
        out
    };
    let failed = |body: &str| -> String {
        if body.contains("see the lines above") {
            "Nothing was installed \u{2014} the lines above say why (details: aterm pkg \
             doctor)."
                .to_string()
        } else {
            sentence(body)
        }
    };
    if let Some(body) = marked(NET_STARTING_MARKER) {
        let n = count(body).unwrap_or(0);
        // The way back, which the marker's consent clause names: kept, in a few words.
        let undo = if body.contains("`aterm pkg uninstall --all`") {
            " To remove them later: aterm pkg uninstall --all"
        } else {
            ""
        };
        return Some(format!(
            "Downloading {n} ALab {}{}\u{2026}{undo}",
            programs(n),
            size(body)
        ));
    }
    if let Some(body) = marked(NET_INSTALLED_MARKER) {
        return Some(format!("Installed: {body}."));
    }
    if let Some(body) = marked(SEED_DONE_MARKER) {
        return Some(match count(body) {
            Some(n) => format!("Done \u{2014} {n} ALab {} installed.", programs(n)),
            // The vendor lane's all-deferred ending names what waits: say it whole, not a
            // bare "Done." after a line that said the landing did not happen.
            None if body.contains(SIGNATURE_WAIT_CLAUSE) => sentence(body),
            None => "Done.".to_string(),
        });
    }
    if let Some(body) = marked(SEED_FAILED_MARKER).or_else(|| marked(NET_FAILED_MARKER)) {
        return Some(failed(body));
    }
    if let Some(body) = marked(SEED_UNUSABLE_MARKER) {
        return Some(if body.contains(UNSERVED_ARCHITECTURE_CLAUSE) {
            "No ALab build is published for this machine's processor yet.".to_string()
        } else {
            sentence(body)
        });
    }
    if marked(SEED_BUSY_MARKER).is_some() {
        // A fact the sentence beside it already says (the intercept's epilogue, or the
        // typed lane's timed-out sentence), so on a terminal it shows as nothing.
        return None;
    }
    if marked(LOCK_WAITING_MARKER).is_some() {
        return Some(
            "Waiting for another atpkg process to finish\u{2026} (Ctrl-C to stop)".to_string(),
        );
    }
    if let Some(body) = marked(MACHINE_SETTINGS_MARKER) {
        // The Universal Control change and the Rosetta 2 install are each said on their own
        // line beside this one, revert and all ([`record_universal_control_outcome`],
        // [`record_rosetta_outcome`]): not twice.
        let said: Vec<&str> = body
            .split("; ")
            .filter(|entry| {
                *entry != crate::machine::UNIVERSAL_CONTROL_ENTRY
                    && *entry != crate::machine::ROSETTA_ENTRY
            })
            .collect();
        return (!said.is_empty())
            .then(|| format!("Changed this Mac's settings: {}.", said.join("; ")));
    }
    if marked(LOCK_ACQUIRED_MARKER).is_some()
        || marked(GROUP_ABORTED_MARKER).is_some()
        || marked(SHADOWED_MARKER).is_some()
        || marked(FOREIGN_MARKER).is_some()
        || marked(C_TOOLCHAIN_MARKER).is_some()
        || marked(MANAGED_CURRENT_MARKER).is_some()
        || marked(MACHINE_STATE_MARKER).is_some()
    {
        return None;
    }
    Some(line.to_string())
}

/// Answer an announcement this verb left open. Called at the tail of every verb that can
/// announce; a no-op for the lanes that already printed a terminal of their own.
///
/// `ok` is the verb's own verdict: an announced pass that failed without a marker gets the
/// failure terminal, and one that succeeded gets the positive one with the store's own
/// count — a fact about what is installed, not a claim about this pass.
fn answer_announcement(ok: bool) {
    if !announcement::is_open() {
        return;
    }
    if ok {
        let held = layout().map_or(0, |l| crate::active_builds(&l).len());
        if held == 0 {
            // Exit 0 over an empty store is not a success to report: the pass announced
            // an install and the disk shows none.
            emit_marker_line(&format!(
                "atpkg: {SEED_FAILED_MARKER}the pass finished without installing \
                 anything — see the lines above for each program's reason"
            ));
            return;
        }
        emit_marker_line(&format!(
            "atpkg: {SEED_DONE_MARKER}the pass finished; {held} program(s) are installed"
        ));
    } else {
        emit_marker_line(&format!(
            "atpkg: {SEED_FAILED_MARKER}the pass ended without installing what it announced — \
             see the lines above for the reason"
        ));
    }
}

/// The human line printed after the `net-installed:` marker, never appended to it (the GUI
/// parses the marker byte for byte): how to run what arrived.
const SEED_FOLLOW_ON: &str = "atpkg: these tools are on PATH in every aterm shell — \
                              aterm pkg list shows them; aterm <tool> runs one anywhere";

/// Whether an update pass may complete the set — the whole consent policy, pure. Every
/// explicit opt-out wins:
///
/// * `consent` — [`crate::config::PackagesConfig::installs_unattended`] (`auto_install`,
///   default true, and no ignored `[packages] prefix`);
/// * `adopted` — the seed pass or `install --default-set` adopted the machine
///   (`uninstall <program>` clears it);
/// * `asked` — `auto_install = true` is written in the table, which stands in for adoption
///   on a machine no window runs on (SSH, Linux), where nothing adopts;
/// * `enabled` — Automatic updates ([`crate::config::PackagesConfig::enabled`]): off, no
///   pass installs anything new, typed or not (a typed `update` asked for updates, not a
///   multi-GB install);
/// * `declined` — the user removed the toolset (`uninstall --all`); checked here, not only
///   in the seed lane, because this is the pass the loop runs;
/// * `config_unreadable` — an unparsable `[packages]` table has no exclusions, so
///   completing the set could reinstall an excluded program (updates to what is installed
///   still apply).
fn should_complete_set(
    consent: bool,
    adopted: bool,
    asked: bool,
    enabled: bool,
    declined: bool,
    config_unreadable: bool,
) -> bool {
    consent && (adopted || asked) && enabled && !declined && !config_unreadable
}

/// Whether this machine has declined the bundled toolset
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
    if let Err(e) = write_marker(
        &path,
        "# This machine declined the bundled ALab toolset.\n\
         # `atpkg seed` will not re-install it (without this, the next launch would\n\
         # simply put back everything `uninstall --all` just removed).\n\
         # Removed automatically by any explicit install.",
    ) {
        eprintln!(
            "atpkg: could not record the decline at {}: {e} — the next launch may adopt and \
             reinstall the toolset; set [packages].auto_install = false in aterm.toml to be sure",
            path.display()
        );
    }
}

/// Forget a decline — any explicit install is an unambiguous change of mind, and undoing
/// one must never require finding a marker file.
fn clear_decline(layout: &crate::store::Layout) {
    remove_marker(&layout.declined());
}

/// Record each freshly installed `requires` dependency of `parent` (each with its own
/// signed root). Shared by [`do_install`] and the default set's ungrouped arm.
fn record_installed_deps(
    layout: &crate::store::Layout,
    parent: &str,
    index_build: u64,
    deps: &[crate::flow::DepOutcome],
) {
    for dep in deps {
        match &dep.result {
            crate::flow::DepResult::Installed { build, tree_root } => record_status(
                layout,
                &dep.program,
                crate::ProgramStatus {
                    installed_build: Some(*build),
                    state: crate::state::managed(*build, index_build),
                    tree_root: tree_root.clone(),
                },
                format!("pulled in {} (required by {parent})", dep.program),
            ),
            // A dependency the user's own copy satisfies: the canonical `system:` row,
            // dated when a managed copy was retired for it earlier.
            crate::flow::DepResult::System(path) => {
                let retired = layout.retired_date(&dep.program);
                record_state_if_changed(
                    layout,
                    &dep.program,
                    crate::state::system(path, retired.as_deref()),
                );
            }
            crate::flow::DepResult::AlreadyPresent(_) | crate::flow::DepResult::Skipped(_) => {}
        }
    }
}

/// Install or force-upgrade one program to its `channel`-pinned build, advancing the
/// durable floor and recording status. Shared by `install` and `update`; `channel`
/// ([`crate::config::CHANNEL`]) is threaded from the verb edge for testability.
fn do_install(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    channel: &str,
    program: &str,
) -> Result<crate::InstallReport, crate::FlowError> {
    do_install_with(layout, fetcher, channel, program, &PassSeams::process())
}

/// [`do_install`] with a vendor-direct program's lane under `seams` (its anchors, clock
/// and shell hooks); an index program's install reads the process's own.
fn do_install_with(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    channel: &str,
    program: &str,
    seams: &PassSeams<'_>,
) -> Result<crate::InstallReport, crate::FlowError> {
    // The active build (what the shim points at), not the max complete build on disk: a
    // staged-but-never-activated build equal to the pin must not read as up to date.
    let installed = crate::active_builds(layout).get(program).copied();
    let floor = build_floor(layout);
    let req = crate::InstallRequest {
        channel,
        program,
        triple: current_triple(),
        installed,
    };
    // The explicit door forgets a digest refusal (`store::record_stage_refusal`): a person
    // typing this verb may mean "the publish (or the proxy) is fixed, try now". Only typed
    // verbs reach here; the loop's lanes keep the cooldown.
    crate::store::clear_stage_refusals(layout, program);
    if crate::vendor_direct::is_vendor(program) {
        let _ = crate::vendor_direct::ProgramStamp::clear_refusal(layout, program);
    }
    // `program → pinned asset name` for everything this pass resolves (`requires` pull-ins
    // included), a failed download too, so the gc below spares its `.part`.
    let mut resolved_assets = std::collections::BTreeMap::new();
    // Typed on a terminal, the install draws its download under the meter ([`TypedPass`]).
    let typed = TypedPass::begin("install");
    let result = match crate::vendor_direct::spec(program)
        .filter(|_| !index_owns_now(layout, fetcher, program))
    {
        // A vendor-direct program: its own lane, which records its row ([`install_vendor_door`]).
        // One the source rule hands to the index lane here is the index flow's below.
        Some(spec) => install_vendor_door(layout, fetcher, channel, spec, seams),
        None => crate::flow::install_collecting_assets(
            fetcher,
            layout,
            &effective_anchor(layout),
            &req,
            floor,
            now_unix(),
            &mut resolved_assets,
        ),
    };
    drop(typed);
    match &result {
        // The vendor lane took nothing from an index but its yanks: no floor, no
        // dependencies, and its row is already written.
        Ok(r) if r.vendor.is_some() => {}
        Ok(r) => {
            // Advance the durable high-water floor to the index we just trusted.
            advance_floors(layout, r.index_build, r.roster_seq);
            // Record each pulled-in dependency first, so the main program's outcome
            // remains the final aggregate.
            record_installed_deps(layout, program, r.index_build, &r.dependencies);
            let outcome = if r.already_current {
                format!("up to date ({program} build {})", r.build)
            } else {
                format!("installed {program} build {}", r.build)
            };
            record_status(
                layout,
                program,
                crate::ProgramStatus {
                    installed_build: Some(r.build),
                    state: crate::state::managed(r.build, r.index_build),
                    // Preserve the recorded signed root when this was a no-op (already
                    // current): an empty root would wipe the attestation `atpkg verify` needs.
                    tree_root: effective_tree_root(layout, program, Some(r.build), &r.tree_root),
                },
                outcome,
            );
            // Reclaim superseded builds after a real activation, at the CLI edge so flow's
            // unit tests stay hermetic; the report is dropped (`gc` and `doctor` say any
            // abstention). The sparing form keeps a failed pull-in's `.part` so it resumes,
            // under the channel this install just verified, so a program moved off a yanked
            // build keeps the build `rollback` lands on.
            if !r.already_current {
                let channel = last_verified_channel(layout);
                let _ = crate::gc::run_keeping_pinned_partials(layout, channel.as_ref(), &|p| {
                    resolved_assets.get(p).cloned()
                });
            }
            // Refresh the interactive-shell PATH hook, at the CLI edge so flow's
            // synthetic-layout tests never write the real ~/.aterm. Best-effort.
            if !r.already_current {
                refresh_shell_hooks(layout);
            }
        }
        Err(e) => match failed_install_state(e) {
            Some(state) => record_status(
                layout,
                program,
                failure_row(layout, program, state),
                format!("install {program}: {e}"),
            ),
            // The attempt is still witnessed — it just is not witnessed as a program.
            None => record_outcome(layout, format!("install {program}: {e}")),
        },
    }
    result
}

/// `install <vendor program>`: the vendor lane for that one program under `seams`, its row
/// recorded by the lane's own writer ([`record_vendor_status`] — a rollback off a yanked
/// build says so, never "latest"), and after a move the gc over that program alone and the
/// shell hooks ([`settle_vendor_pass`]), as `update <vendor>` does.
///
/// # Errors
/// The lane installed nothing it could stand behind
/// ([`crate::vendor_direct::lane::Outcome::into_install`]).
fn install_vendor_door(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    channel: &str,
    spec: &'static crate::vendor_direct::VendorSpec,
    seams: &PassSeams<'_>,
) -> Result<crate::InstallReport, crate::FlowError> {
    let now = (seams.now)();
    let req = crate::InstallRequest {
        channel,
        program: spec.program,
        triple: current_triple(),
        installed: None,
    };
    let outcome = crate::flow::vendor_install_outcome(
        fetcher,
        layout,
        &(seams.anchor)(layout),
        build_floor(layout),
        &req,
        spec,
        seams.trust,
        now,
    );
    let mut pass = VendorPass::default();
    pass.ran.insert(spec.program.to_string());
    if let Some(asset) = &outcome.asset {
        pass.resolved_assets
            .insert(spec.program.to_string(), asset.clone());
    }
    pass.changed = matches!(
        outcome.verdict,
        crate::vendor_direct::lane::Verdict::Installed { .. }
            | crate::vendor_direct::lane::Verdict::RolledBack { .. }
            | crate::vendor_direct::lane::Verdict::Tombstoned { .. }
    );
    record_vendor_status(layout, &outcome, now);
    note_landed(&mut pass, &outcome);
    warm_landed(layout, &mut pass, seams);
    let _ = settle_vendor_pass(layout, &pass, seams);
    outcome.into_install(layout)
}

/// The `install` verb's argv: one program name, or `--default-set`, decided by
/// [`parse_install_argv`] before anything dispatches.
fn cmd_install_argv(rest: &[String]) -> ExitCode {
    match parse_install_argv(rest) {
        Ok(program) => cmd_install(program, "install"),
        Err(refusal) => refusal,
    }
}

/// [`cmd_install_argv`]'s grammar: the one operand, or the usage EXIT of a refusal this
/// function has already narrated on stderr.
///
/// SPLIT FROM THE VERB so the grammar is decidable without dispatching. The
/// `--default-set` route applies this machine's `[machine]` settings (Universal Control's
/// two `defaults` keys) and then reaches for the network, so a test that called
/// the verb to learn what an argv MEANS would do all of that to find out.
fn parse_install_argv(rest: &[String]) -> Result<Option<&String>, ExitCode> {
    if rest.len() > 1 {
        eprintln!(
            "atpkg install: one program at a time (got {}); the whole toolset is \
             `aterm pkg install --default-set`",
            rest.len()
        );
        return Err(ExitCode::from(2));
    }
    Ok(rest.first())
}

/// [`cmd_install`] reached from `update <program>` on a program not installed: the same
/// path, the messages naming the verb the person typed.
fn cmd_install_for_update(program: &String) -> ExitCode {
    cmd_install(Some(program), "update")
}

/// `atpkg install <program>` — resolve and verify the signed index, then install or
/// force-upgrade the program's pinned build for this triple. Inert unless a root key is
/// pinned at build time. `--default-set` routes to [`cmd_install_default_set`].
fn cmd_install(program: Option<&String>, invoked_as: &str) -> ExitCode {
    let Some(program) = program else {
        // A bare `install` is nearly always someone asking for the toolset: name the
        // spelling that grants it.
        eprintln!(
            "atpkg install: name a program, or `aterm pkg install --default-set` for the whole \
             ALab toolset"
        );
        return ExitCode::from(2);
    };
    if program == "--default-set" {
        return cmd_install_default_set();
    }
    // A flag is not a program name: one that reached here (an option in a form the edge
    // does not strip) would be recorded as a wanted program. Refuse it and name the two
    // spellings that work.
    if program.starts_with('-') {
        eprintln!("atpkg: {program:?} is a command-line flag, not a program name");
        eprintln!("atpkg:   one program:      aterm pkg install <program>");
        eprintln!("atpkg:   the whole toolset: aterm pkg install --default-set");
        return ExitCode::from(2);
    }
    // A COMMAND IS NOT A PROGRAM: `targo` comes with `trust`, and the index names no
    // program `targo`, so its install could only fail with the roster. Name the install
    // that lays it instead of installing anything the person did not name — a command only
    // a signed manifest exposes included, while its stub stands ([`crate::stub::program_of_in`]).
    let ships = crate::store::resolve_configured().map_or_else(
        || crate::stub::program_of(program).to_string(),
        |layout| crate::stub::program_of_in(&layout, program),
    );
    if ships != *program {
        eprintln!("atpkg: {program} comes with {ships}: aterm pkg install {ships}");
        return ExitCode::from(2);
    }
    if !manager_enabled() {
        eprintln!("atpkg: disabled (no root key pinned) — refusing to install");
        return ExitCode::from(1);
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let cfg = crate::config::cached();
    // Asking for this program back lifts its removal record.
    clear_removed(&layout, std::slice::from_ref(program));
    cmd_install_with(
        program,
        &layout,
        cfg,
        &*resolve_fetcher(&layout),
        invoked_as,
    )
}

/// The body of [`cmd_install`] against an already-built fetcher: a fresh one would throw
/// away the memoized index candidates and manifests and fetch the signed index again.
fn cmd_install_with(
    program: &str,
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    fetcher: &dyn crate::flow::Fetcher,
    // The verb the user typed, for messaging only: `update <p>` on a program that is not
    // installed routes here, and a failure must name the verb they typed.
    invoked_as: &str,
) -> ExitCode {
    reconcile_links(layout, cfg);
    if let Some(path) = explicit_install_door(layout, fetcher, program) {
        println!(
            "atpkg: {program}: nothing to install — the system install at {} satisfies it \
             (remove that install and re-run to have atpkg manage {program})",
            path.display()
        );
        return ExitCode::SUCCESS;
    }
    match do_install(layout, fetcher, crate::config::CHANNEL, program) {
        Ok(r) => {
            if let Some(vendor) = &r.vendor {
                // The vendor lane's own line: versions, never a store build id.
                println!("{}", vendor.line);
            } else if r.already_current {
                println!("{}", already_current_line(&r));
            } else {
                println!("atpkg: {}", installed_line(&r.program, r.build, &r.shimmed));
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
                    crate::flow::DepResult::System(path) => {
                        println!(
                            "atpkg:   {} (required): {}",
                            dep.program,
                            crate::state::system(
                                path,
                                layout.retired_date(&dep.program).as_deref()
                            )
                        );
                    }
                    crate::flow::DepResult::AlreadyPresent(_) => {}
                }
            }
            // The seam and the exec roots, on the single-program door too: the seam is how
            // `cargo +trust` and every `rust-toolchain.toml` find the managed compiler, and
            // `aterm pkg update trust` is what a person types when they suspect it is stale.
            // Best-effort; a foreign entry is refused, printed and recorded, never re-pointed.
            reassert_rustup_seam(layout);
            let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
            ExitCode::SUCCESS
        }
        Err(crate::FlowError::Linked(p)) => {
            // A dev-linked program is managed from its checkout — a hard skip, not a failure.
            println!(
                "{}",
                dev_linked_line(&p, unlink_returns_to_alab(layout, &p))
            );
            ExitCode::SUCCESS
        }
        // A target the pinned build (or the vendor) does not serve is a state, never an
        // error: say the canonical words (the record already carries them) and exit clean.
        Err(e @ (crate::FlowError::NoArtifact(_) | crate::FlowError::VendorUnavailable(_))) => {
            let state = canonical_non_error_state(&e)
                .unwrap_or_else(|| crate::state::unavailable(current_triple(), ""));
            println!("atpkg: {program}: {state}");
            ExitCode::SUCCESS
        }
        // The vendor lane's line is the verdict (the bytes `update <p>` prints through
        // `report_vendor`) and names the program itself, so no `{verb} {program} failed:`
        // head.
        Err(crate::FlowError::Vendor(line)) => {
            eprintln!("atpkg: {line}");
            ExitCode::from(1)
        }
        // …and the vendor's channel not reached gets the index lane's offline action:
        // re-run when the connection is back, under the verb the person typed.
        Err(e @ crate::FlowError::VendorUnreachable(_)) => {
            eprintln!("atpkg: {e}");
            print_unreachable_followup(&e, &format!("aterm pkg {invoked_as} {program}"));
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("atpkg: {invoked_as} {program} failed: {e}");
            if let Some(state) = canonical_non_error_state(&e) {
                eprintln!("atpkg: {program}: {state}");
            }
            // The re-run names the verb the person typed: `update <program>` reaches this
            // same install, so the person is never sent to a verb they did not type.
            print_unreachable_followup(&e, &format!("aterm pkg {invoked_as} {program}"));
            ExitCode::from(1)
        }
    }
}

/// `installed trust 9192 — 30 commands on your PATH (trustc, targo, tippy, …)`: the
/// program, its build and a few of the commands it laid (never the `alab-` aliases), the
/// program's own name first when it is one of them.
fn installed_line(program: &str, build: u64, laid: &[String]) -> String {
    let mut commands: Vec<&str> = laid
        .iter()
        .map(String::as_str)
        .filter(|n| !n.starts_with(crate::store::ALIAS_PREFIX))
        .collect();
    if let Some(at) = commands.iter().position(|n| *n == program) {
        let own = commands.remove(at);
        commands.insert(0, own);
    }
    const SHOWN: usize = 3;
    match commands.len() {
        0 => format!("installed {program} {build}"),
        1 => format!(
            "installed {program} {build} \u{2014} `{}` is on your PATH",
            commands[0]
        ),
        n => {
            let mut named = commands[..n.min(SHOWN)].join(", ");
            if n > SHOWN {
                named.push_str(", \u{2026}");
            }
            format!("installed {program} {build} \u{2014} {n} commands on your PATH ({named})")
        }
    }
}

/// The index-program rollback verdict, pure: what moved, what the next update does, and the
/// one hold — for a coherence-group member, the whole group's, said in the same line.
/// `held_by` names the pins already holding it ([`crate::flow::RollbackReport::held_by`]):
/// then the line names the unpin. `revoked` is a group member's revoked current build
/// ([`crate::flow::RollbackReport::revoked_member`]): the next update moves the group
/// whatever is pinned, so the line names that build and no pin.
fn rollback_line(
    program: &str,
    from: u64,
    to: u64,
    group: Option<&str>,
    held_by: &[String],
    revoked: Option<(&str, u64)>,
) -> String {
    let head = format!("atpkg: rolled back {program} from build {from} to {to}");
    if let Some((member, build)) = revoked {
        return format!(
            "{head}; the next update moves it forward again ({} is revoked)",
            crate::vendor_direct::display_build(member, build)
        );
    }
    if held_by.is_empty() {
        let hold = match group {
            Some(g) => format!("holds it, and with it the whole {g} group"),
            None => String::from("holds it there"),
        };
        return format!(
            "{head}; the next update moves it forward again \u{2014} `aterm pkg pin {program}` \
             {hold}"
        );
    }
    let who = match held_by {
        [p] if p == program => String::from("it is"),
        [p] => format!("{p} is"),
        _ => format!("{} are", held_by.join(" and ")),
    };
    let what = group.map_or_else(|| String::from("it"), |g| format!("the whole {g} group"));
    let unpins = held_by
        .iter()
        .map(|h| format!("`aterm pkg unpin {h}`"))
        .collect::<Vec<_>>()
        .join(" and ");
    format!("{head}; {who} pinned, so updates leave {what} there until {unpins}")
}

/// `atpkg rollback <program>` — re-point the program (shims and `current`) to the highest
/// retained build below current that passes the floor/yank gate of the signed index, and
/// advance the durable floor. A coherence-group member's line says the pin holds the whole
/// group (a per-program rollback splits the tuple until the next update). A vendor-direct
/// program never reaches the index ([`cmd_rollback_vendor`]).
fn cmd_rollback(program: Option<&String>) -> ExitCode {
    let Some(program) = program else {
        return usage_error("rollback", None);
    };
    if !manager_enabled() {
        eprintln!("atpkg: disabled (no root key pinned) — refusing to roll back");
        return ExitCode::from(1);
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    rollback_in(
        &layout,
        crate::config::CHANNEL,
        program,
        crate::config::cached().exclude(),
        &|| resolve_fetcher(&layout),
    )
}

/// [`cmd_rollback`]'s body over the store it is handed. `fetcher` is built only on the
/// index lane: a vendor program's rollback ([`cmd_rollback_vendor`]) never makes one.
/// Each lane derives its active build and refuses an absent one before any fetch.
/// `excluded` is `[packages].exclude`, which the next update reads beside the pins.
fn rollback_in(
    layout: &crate::store::Layout,
    channel: &str,
    program: &str,
    excluded: &[String],
    fetcher: &dyn Fn() -> Box<dyn crate::flow::Fetcher>,
) -> ExitCode {
    if let Some(spec) = crate::vendor_direct::spec(program) {
        return cmd_rollback_vendor(layout, spec);
    }
    let floor = build_floor(layout);
    let fetcher = fetcher();
    match crate::flow::rollback(
        &*fetcher,
        layout,
        &effective_anchor(layout),
        channel,
        program,
        excluded,
        floor,
        now_unix(),
    ) {
        Ok(r) => {
            // Advance both durable ratchets to what we just trusted.
            advance_floors(layout, r.index_build, r.roster_seq);
            record_status(
                layout,
                program,
                crate::ProgramStatus {
                    installed_build: Some(r.to_build),
                    // What happened, not a pin the index does not hold: the index still
                    // pins the build just left, and `which` prints this row's words.
                    state: crate::state::rolled_back(r.to_build, r.from_build),
                    // Not in the rollback report: unset here and re-derived below. The
                    // previous build's root would attest bytes that are no longer there.
                    tree_root: String::new(),
                },
                format!("rolled back {program} {} -> {}", r.from_build, r.to_build),
            );
            // Re-derive the root now, with the index just trusted: otherwise `atpkg verify`
            // would fail closed on a good rollback until a whole-set `update`.
            recover_missing_roots(layout, &*fetcher, None);
            println!(
                "{}",
                rollback_line(
                    program,
                    r.from_build,
                    r.to_build,
                    r.coherence_group.as_deref(),
                    &r.held_by,
                    r.revoked_member.as_ref().map(|(m, b)| (m.as_str(), *b))
                )
            );
            if program == crate::seam::SEAM_PROGRAM {
                reassert_rustup_seam(layout);
                // The pass-level reconcile every verb that moves trust ends with: a trust
                // shim the rollback's lay did not render is routed, and a root that could
                // not be laid is said.
                let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: rollback {program} failed: {e}");
            print_unreachable_followup(&e, &format!("aterm pkg rollback {program}"));
            ExitCode::from(1)
        }
    }
}

/// `atpkg rollback <vendor program>`: back to the retained previous build no signed yank
/// names — a vendor build, else the legacy index build the first vendor install replaced
/// ([`crate::vendor_direct::lane::roll_back_by_hand`]) — with no index resolve. Its row says
/// why the vendor's head is not what runs; the next update returns it to the head unless
/// it is pinned.
fn cmd_rollback_vendor(
    layout: &crate::store::Layout,
    spec: &'static crate::vendor_direct::VendorSpec,
) -> ExitCode {
    let program = spec.program;
    let policy = crate::vendor_direct::policy::Policy::current(layout, None);
    match crate::vendor_direct::lane::roll_back_by_hand(layout, &policy, spec) {
        Ok(moved) => {
            let build = moved.to_build;
            // A vendor build's root is its `.vendor` record's; a legacy build's comes from
            // [`legacy_root_of`].
            let tree_root =
                crate::vendor_direct::complete_record(&layout.build_dir(program, build))
                    .map_or_else(|| legacy_root_of(layout, program, build), |r| r.tree_root);
            // An ALab build's row names it as ALab's ([`crate::state::alab_kept`]).
            let state = if crate::alab::build_is_alab(layout, program, build) {
                crate::state::alab_kept(
                    build,
                    &format!("{}; rolled back from {}", moved.to, moved.from),
                )
            } else {
                crate::state::vendor_kept(
                    &crate::vendor_direct::build_label(build),
                    &format!("rolled back from {}", moved.from),
                )
            };
            record_status(
                layout,
                program,
                crate::ProgramStatus {
                    installed_build: Some(build),
                    state,
                    tree_root,
                },
                format!("rolled back {program} {} -> {}", moved.from, moved.to),
            );
            println!(
                "{}",
                vendor_rolled_back_line(
                    spec,
                    &moved.from,
                    &moved.to,
                    crate::pin::is_pinned(layout, program),
                    alab_channel_of(layout, program, build)
                )
            );
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("atpkg: rollback {program} failed: {why}");
            ExitCode::from(1)
        }
    }
}

/// The ALab product whose channel moves `program` on from `build` after a rollback: `Some`
/// when `build` is an ALab build ([`crate::alab::build_is_alab`]) and no person chose the
/// vendor's build here — the next update then follows ALab's channel, never the vendor's head.
fn alab_channel_of(
    layout: &crate::store::Layout,
    program: &str,
    build: u64,
) -> Option<&'static str> {
    let agent = crate::alab::agent(program)?;
    (crate::alab::build_is_alab(layout, program, build)
        && !crate::alab::vendor_chosen(layout, program))
    .then_some(agent.product)
}

/// [`cmd_rollback_vendor`]'s line: versions, and how the rollback holds. A `pinned`
/// program stays (the vendor lane leaves a pinned program alone), and the line says so in
/// [`rollback_line`]'s words. On an ALab build (`alab`, [`alab_channel_of`]) the next update
/// follows that product's channel, and the line names it rather than the vendor's latest.
fn vendor_rolled_back_line(
    spec: &crate::vendor_direct::VendorSpec,
    from: &str,
    to: &str,
    pinned: bool,
    alab: Option<&str>,
) -> String {
    let p = spec.program;
    if pinned {
        return format!(
            "atpkg: rolled back {p} from {from} to {to}; it is pinned, so updates leave it \
             there until `aterm pkg unpin {p}`"
        );
    }
    if let Some(product) = alab {
        return format!(
            "atpkg: rolled back {p} from {from} to {to}; `aterm pkg pin {p}` holds it there \
             (otherwise the next update follows {product}'s channel)"
        );
    }
    format!(
        "atpkg: rolled back {p} from {from} to {to}; `aterm pkg pin {p}` holds it there \
         (otherwise the next update returns it to {}'s latest)",
        spec.vendor
    )
}

/// What `pin` / `unpin` knows of the hold beyond the program's own pin ([`pin_line`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinPeers<'a> {
    /// No cached index to read the program's coherence group from.
    Unknown,
    /// No other pin holds it: ungrouped, or no other member of its group is pinned.
    Alone,
    /// `pinned`, other members of coherence group `group`, are pinned: the update pass's
    /// group gate holds the whole tuple while any member is ([`crate::flow::pin_gate_members`]).
    Group {
        group: &'a str,
        pinned: &'a [String],
    },
}

/// The `pin` / `unpin` verdict, pure: the program and the build the pin holds or the unpin
/// releases (`list`'s spelling, [`crate::vendor_direct::display_build`]), the state, and the
/// one thing to know. A pin holds a build against `update`, except that a yanked or
/// below-floor build is replaced regardless — said as a fact. An unpin does not predict
/// what the next update moves (that depends on the vendor's head or the index pin); it
/// names the check.
///
/// A coherence-group member's hold is the group's: while `peers` names other pinned members
/// the line names every unpin it takes; without the group (`PinPeers::Unknown`) a verb that
/// changed nothing predicts nothing.
fn pin_line(program: &str, build: u64, pinned: bool, changed: bool, peers: PinPeers<'_>) -> String {
    let what = crate::vendor_direct::display_build(program, build);
    if let PinPeers::Group {
        group,
        pinned: others,
    } = peers
    {
        let unpin = |n: &str| format!("`aterm pkg unpin {n}`");
        let who = format!(
            "{} {}",
            others.join(" and "),
            if others.len() == 1 { "is" } else { "are" }
        );
        let names = others.iter().map(String::as_str);
        let all = std::iter::once(program)
            .chain(names.clone())
            .map(unpin)
            .collect::<Vec<_>>()
            .join(" and ");
        let theirs = names.map(unpin).collect::<Vec<_>>().join(" and ");
        let (pins_hold, release) = if others.len() == 1 {
            ("its pin holds", "releases")
        } else {
            ("their pins hold", "release")
        };
        return match (pinned, changed) {
            (true, true) => format!(
                "atpkg: {what} pinned \u{2014} {who} pinned too, so updates leave the whole \
                 {group} group there until {all} (a yanked build is still replaced)"
            ),
            (true, false) => format!(
                "atpkg: {what} is already pinned, and {who} too, so updates leave the whole \
                 {group} group there until {all}"
            ),
            (false, true) => format!(
                "atpkg: {what} unpinned, but {who} pinned, and {pins_hold} the whole {group} \
                 group \u{2014} {theirs} {release} it"
            ),
            (false, false) => format!(
                "atpkg: {what} was not pinned, but {who}, and {pins_hold} the whole {group} \
                 group \u{2014} {theirs} {release} it"
            ),
        };
    }
    if !changed {
        return match (pinned, peers) {
            (true, PinPeers::Unknown) => {
                format!(
                    "atpkg: {what} is already pinned (aterm pkg unpin {program} removes the pin)"
                )
            }
            (true, _) => {
                format!("atpkg: {what} is already pinned (aterm pkg unpin {program} releases it)")
            }
            (false, PinPeers::Unknown) => {
                format!("atpkg: {what} was not pinned \u{2014} nothing to release")
            }
            (false, _) => format!("atpkg: {what} was not pinned \u{2014} updates already move it"),
        };
    }
    if pinned {
        format!(
            "atpkg: {what} pinned \u{2014} updates leave it there until `aterm pkg unpin \
             {program}` (a yanked build is still replaced)"
        )
    } else {
        format!(
            "atpkg: {what} unpinned \u{2014} updates resume; `aterm pkg update {program}` checks \
             now"
        )
    }
}

/// `program`'s coherence group and the other members whose pins hold it, read offline from
/// the cached index as the update pass's group gate reads them
/// ([`crate::flow::pin_gate_members`]); `None` when no index is cached.
fn pin_peers_of(
    layout: &crate::store::Layout,
    program: &str,
    active: &std::collections::BTreeMap<String, u64>,
) -> Option<(Option<String>, Vec<String>)> {
    let index = cached_index(layout)?;
    let ch = index.channel_for(crate::config::CHANNEL, current_triple())?;
    let group = index
        .program(program)
        .and_then(|p| p.coherence_group.clone());
    let pins = crate::pin::pinned_set(layout);
    let others = crate::flow::pin_gate_members(
        layout,
        &index,
        &ch,
        program,
        active,
        crate::config::cached().exclude(),
    )
    .into_iter()
    .filter(|m| m != program && pins.contains(m))
    .collect();
    Some((group, others))
}

/// Whether `unlink` hands `program` back to ALab's channel on this machine: a program ALab
/// builds here ([`crate::alab`]) that no person moved to the vendor's build, whose
/// activation link names an ALab store build or whose last verified index pins one. Every
/// other answer is the program's own source, as [`dev_linked_line`] says.
fn unlink_returns_to_alab(layout: &crate::store::Layout, program: &str) -> bool {
    if crate::alab::agent(program).is_none() || crate::alab::vendor_chosen(layout, program) {
        return false;
    }
    let active = std::fs::read_link(layout.program_current(program))
        .ok()
        .and_then(|t| t.file_name()?.to_str()?.parse::<u64>().ok())
        .is_some_and(|b| crate::alab::build_is_alab(layout, program, b));
    active
        || cached_index(layout).is_some_and(|index| {
            crate::alab::index_pin(&index, program, current_triple()).is_some()
        })
}

/// The dev-linked skip, pure: what `unlink` hands the program back to is the program's
/// own source — ALab's build for an ALab-built agent program on a machine that follows
/// ALab's channel (`alab`, [`unlink_returns_to_alab`]), the vendor's latest for a
/// vendor-direct program (never the registry, which supplies no bytes for it), the
/// registry pin for the rest (audit).
fn dev_linked_line(program: &str, alab: bool) -> String {
    if alab && let Some(agent) = crate::alab::agent(program) {
        return format!(
            "atpkg: {program} is dev-linked; run `aterm pkg unlink {program}` to return to the \
             {} build",
            agent.product
        );
    }
    match crate::vendor_direct::spec(program) {
        Some(spec) => format!(
            "atpkg: {program} is dev-linked; run `aterm pkg unlink {program}` to install {}'s \
             latest",
            spec.vendor
        ),
        None => format!(
            "atpkg: {program} is dev-linked; run `aterm pkg unlink {program}` to install ALab's \
             build"
        ),
    }
}

/// `atpkg pin <program>` / `atpkg unpin <program>` — freeze (or release) a program against
/// `update`. A pin is local upgrade suppression, consulted strictly after the floor/yank
/// gate, so it never resurrects a tombstoned or below-floor build.
fn cmd_pin(program: Option<&String>, pinned: bool) -> ExitCode {
    let verb = if pinned { "pin" } else { "unpin" };
    let Some(program) = program else {
        return usage_error(verb, None);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    // Pinning something not installed is meaningless; the build the pin holds (or the
    // unpin releases) is the fact the line names.
    let builds = crate::active_builds(&layout);
    let Some(&build) = builds.get(program) else {
        eprintln!(
            "atpkg: {program} is not installed — nothing to {verb} (aterm pkg list shows \
             what is)"
        );
        return ExitCode::from(1);
    };
    match crate::pin::set_pinned(&layout, program, pinned) {
        Ok(changed) => {
            // A vendor-direct program is in no coherence group and needs no index to say so.
            let looked = if crate::vendor_direct::is_vendor(program) {
                Some((None, Vec::new()))
            } else {
                pin_peers_of(&layout, program, &builds)
            };
            let peers = match &looked {
                None => PinPeers::Unknown,
                Some((Some(group), others)) if !others.is_empty() => PinPeers::Group {
                    group,
                    pinned: others,
                },
                Some(_) => PinPeers::Alone,
            };
            println!("{}", pin_line(program, build, pinned, changed, peers));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: {verb} {program} failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// `atpkg update [program] [--retry]`: the operand and the one flag, in either order.
///
/// `--retry` forgets the digest-refusal memos first (every program's, or the named one's),
/// then runs the pass as usual. A held row ([`crate::store::StageRefusal::held`]) never
/// lapses on its own, so a person who believes the publish is fixed needs a door that is
/// not "install one program by name". `install <p>` clears the same memos
/// ([`do_install_with`]; for a vendor-direct program, [`forget_vendor_refusal`]), so the
/// two spellings agree on what "try again" means.
fn cmd_update_argv(rest: &[String]) -> ExitCode {
    let mut retry = false;
    let mut choice: Option<&str> = None;
    let mut operands: Vec<&String> = Vec::new();
    for arg in rest {
        if arg == "--retry" {
            retry = true;
        } else if arg == SOURCE_VENDOR_FLAG || arg == SOURCE_ALAB_FLAG {
            if choice.is_some_and(|c| c != arg) {
                return usage_error(
                    "update",
                    Some("--vendor and --alab are two answers to one question — pass one"),
                );
            }
            choice = Some(arg.as_str());
        } else if arg.starts_with('-') {
            // Unreachable through the dispatch edge: its unknown-flag check takes the
            // FIRST position, and its arity gate counts a flag anywhere else as an
            // operand ([`arity_refusal`]). Kept so the grammar is total for a direct
            // caller.
            let problem = format!("unknown option {arg:?} for `aterm pkg update`");
            return usage_error("update", Some(&problem));
        } else {
            operands.push(arg);
        }
    }
    // A SECOND NAME IS REFUSED, in the arity gate's own words ([`arity_refusal`]). The
    // dispatch edge already refused it before the store lock; this keeps the grammar total
    // for a direct caller — before anything is forgotten or run.
    if let Some(refusal) = arity_refusal(rest, "update") {
        return refusal;
    }
    let program = operands.first().copied();
    if let Some(choice) = choice {
        return cmd_update_source(program.map(String::as_str), choice);
    }
    // THE NAME IS ADMITTED BEFORE ANYTHING IS FORGOTTEN. `--retry` deletes `*.refused`
    // files under `store/<program>/`, and nothing between the dispatch edge (which refuses
    // only flags) and this line asks whether `<program>` is one directory name — so
    // `update --retry ..\..\x` read and emptied a directory outside the store before
    // `update` got to refuse the name (and on Windows `update --retry C:` the current
    // directory of drive C: a drive prefix replaces the base in `Path::join`). The
    // store's own shape rule ([`crate::ops::uninstall_name_shape`]: one normal path
    // component — no separator, drive prefix, `.`, `..` or NUL) is the question;
    // `clear_stage_refusals` asks it too, for every other door.
    if retry && let Some(refusal) = program.and_then(|p| refuse_retry_operand(p)) {
        return refusal;
    }
    if retry && let Some(layout) = layout() {
        let mut forgotten = match program {
            Some(p) => crate::store::clear_stage_refusals(&layout, p),
            None => crate::store::clear_all_stage_refusals(&layout),
        };
        // …and the vendor lane's own memo: a vendor-direct program's signer verdict is
        // kept in its stamp, not beside a build, and a bare `update` pass honours it.
        let named = program.map(String::as_str);
        for vendor in crate::vendor_direct::VENDORS
            .iter()
            .map(|spec| spec.program)
            .filter(|v| named.is_none_or(|p| p == *v))
        {
            if forget_vendor_refusal(&layout, vendor) {
                forgotten = forgotten.saturating_add(1);
            }
        }
        println!("{}", retry_line(forgotten));
    }
    cmd_update(program)
}

/// `update <program> --vendor | --alab` (B9 of the managed-codex plan): record or forget a
/// person's choice of the vendor's build of an ALab-built program on this machine, then
/// update that program by the source rule it now reads ([`crate::alab`]). Refused for a
/// program ALab builds nowhere, or with no program named: the choice is per program.
fn cmd_update_source(program: Option<&str>, choice: &str) -> ExitCode {
    let Some(program) = program else {
        return usage_error("update", Some("--vendor and --alab name one program"));
    };
    let Some(agent) = crate::alab::agent(program) else {
        let problem = format!(
            "{program} has no ALab build to choose between — --vendor and --alab are for {}",
            crate::alab::ALAB_AGENTS
                .iter()
                .map(|a| a.program)
                .collect::<Vec<_>>()
                .join(", ")
        );
        return usage_error("update", Some(&problem));
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let vendor = crate::vendor_direct::spec(program).map_or("the vendor", |s| s.vendor);
    if choice == SOURCE_VENDOR_FLAG {
        if let Err(e) = crate::alab::choose_vendor(&layout, program) {
            eprintln!("atpkg: update {program} --vendor failed: the choice was not recorded ({e})");
            return ExitCode::from(1);
        }
        println!(
            "atpkg: {program} follows {vendor}'s builds on this machine now — `aterm pkg update \
             {program} --alab` returns it to {}'s channel",
            agent.product
        );
    } else if crate::alab::clear_vendor_choice(&layout, program) {
        println!(
            "atpkg: {program} follows {}'s channel on this machine again, where it publishes \
             one",
            agent.product
        );
    } else {
        println!(
            "atpkg: {program} already follows {}'s channel where it publishes one",
            agent.product
        );
    }
    cmd_update_one(&program.to_string())
}

/// `update --retry <program>`'s name gate: `Some(usage exit)`, said on stderr, for an
/// operand that is not one directory name ([`crate::ops::uninstall_name_shape`]); `None`
/// to proceed. Pure, so the refusal is testable without a store or a pass.
fn refuse_retry_operand(program: &str) -> Option<ExitCode> {
    crate::ops::uninstall_name_shape(program).err()?;
    let problem = format!("{program:?} is not a program name — nothing was forgotten or run");
    Some(usage_error("update", Some(&problem)))
}

/// Forget the stage refusal a vendor-direct `program`'s stamp records, as the install door
/// does; whether one was there. `false` for a program that is not vendor-direct.
fn forget_vendor_refusal(layout: &crate::store::Layout, program: &str) -> bool {
    crate::vendor_direct::is_vendor(program)
        && crate::vendor_direct::ProgramStamp::read(layout, program)
            .is_some_and(|stamp| stamp.refused.is_some())
        && crate::vendor_direct::ProgramStamp::clear_refusal(layout, program).is_ok()
}

/// What `--retry` says about the memos it forgot: how many, and that the pass follows.
fn retry_line(forgotten: usize) -> String {
    match forgotten {
        0 => String::from("atpkg: no download was being held back \u{2014} updating"),
        1 => String::from("atpkg: trying 1 held-back download again"),
        n => format!("atpkg: trying {n} held-back downloads again"),
    }
}

/// `atpkg update [program]` — the update router. No arg ⇒ every installed program
/// ([`cmd_update_all`], the background loop's verb). With a program ⇒ [`cmd_update_one`],
/// which routes a coherence-group member through the transactional path (a locked tuple
/// never splits) and an ungrouped program through the single-program install path.
fn cmd_update(program: Option<&String>) -> ExitCode {
    match program {
        Some(p) => cmd_update_one(p),
        None => cmd_update_all(),
    }
}

/// `atpkg update` (no arg) — silently upgrade every installed program to its channel pin,
/// applying coherence groups atomically: the `rustc`-locked tuple moves all-or-nothing
/// (stage-all → flip-all → rollback-on-failure), while an ungrouped tool applies alone so
/// one failure never wedges the tuple. A local pin holds a group on its current builds.
/// Under the install consent the pass also installs missing default-set members
/// ([`install_default_set_with_path`], [`should_complete_set`]), so the loop verb fills an
/// empty store. GC runs once after the whole apply.
fn cmd_update_all() -> ExitCode {
    // Answer an announcement the set completion may have made, with the pass's verdict.
    let code = cmd_update_all_code();
    answer_announcement(code == 0);
    ExitCode::from(code)
}

/// The `*index*` row after `update`'s set-completion lane, returning whether the pass has
/// resolved the signed index. On an empty store that lane is the pass, so its unresolved
/// index is recorded as the apply lane would, and a resolved one retires the row. When the
/// apply lane already resolved, a set-completion miss neither un-resolves it nor re-opens
/// the row.
fn settle_net_lane_index(
    layout: &crate::store::Layout,
    apply_resolved: bool,
    net_index_error: Option<&str>,
) -> bool {
    match net_index_error {
        None => {
            clear_status_row(layout, "*index*");
            true
        }
        Some(_) if apply_resolved => true,
        Some(e) => {
            record_index_error(layout, e);
            false
        }
    }
}

/// The update pass's one failure exit: say it, name the next act, and record the durable
/// `*index*` row. Shared by the pass's index resolve and its channel apply, so both print
/// the same sentence and leave the same row.
fn update_pass_failed(layout: &crate::store::Layout, e: &crate::FlowError) -> u8 {
    eprintln!("atpkg: update failed: {e}");
    // The single-program lane's follow-up: a network failure always names its next act.
    print_unreachable_followup(e, "aterm pkg update");
    record_index_error(layout, e);
    1
}

/// Finish every activation an interrupted pass left half done (the `current` flip landed,
/// the shims did not), before anything decides on the store: lay the shims from that build
/// and its `.shim-env` sidecar — no stage, no fetch — and record a row with no signed root,
/// which the pass's root recovery re-derives. Returns the programs rolled forward.
///
/// Never for a program removed, declined, dev-linked, tombstoned or pinned; a build the
/// latest verified index (or, for a vendor build, the yank latch) yanked or floored; a
/// sysroot bundle whose compilers do not load; or a coherence-group member, whose group's
/// own transaction re-flips it (rolling one forward alone could split the tuple).
/// Membership comes from `cached`, read only when a candidate needs it; with none cached
/// only the vendor programs (never grouped) roll forward.
fn roll_forward_interrupted_activations(
    layout: &crate::store::Layout,
    cached: impl FnOnce() -> Option<crate::TrustedIndex>,
) -> Vec<String> {
    let mut rolled = Vec::new();
    if layout.declined().is_file() {
        return rolled;
    }
    let Ok(entries) = std::fs::read_dir(layout.prefix.join("store")) else {
        return rolled;
    };
    let shims = crate::active_builds(layout);
    let removed = layout.removed_programs();
    let mut cached = Some(cached);
    let mut index: Option<Option<crate::TrustedIndex>> = None;
    for entry in entries.flatten() {
        let Some(program) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(build) = std::fs::read_link(layout.program_current(&program))
            .ok()
            .and_then(|target| crate::ops::store_build_of(&layout.prefix, &target))
            .filter(|(owner, _)| *owner == program)
            .map(|(_, build)| build)
        else {
            continue;
        };
        let build_dir = layout.build_dir(&program, build);
        if shims.get(&program) == Some(&build)
            || removed.contains(&program)
            || !crate::store::build_is_complete(&build_dir)
            || crate::linkmode::is_linked(layout, &program)
            || crate::flow::tombstoned_in_place(layout, [program.as_str()])
            || crate::pin::is_pinned(layout, &program)
        {
            continue;
        }
        let vendor = crate::vendor_direct::spec(&program);
        let admitted = match vendor {
            Some(_) => {
                let policy = crate::vendor_direct::policy::Policy::current(layout, None);
                !crate::vendor_direct::Version::from_build_id(build)
                    .is_some_and(|v| policy.is_yanked(&program, v))
                    && !policy.is_legacy_yanked(&program, build)
            }
            None => {
                let index = index.get_or_insert_with(|| cached.take().and_then(|f| f()));
                let channel = crate::config::CHANNEL;
                index.as_ref().is_some_and(|i| {
                    i.program(&program)
                        .is_some_and(|p| p.coherence_group.is_none())
                        && i.channels
                            .iter()
                            .find(|c| c.name == channel)
                            .is_some_and(|ch| {
                                crate::gate::current_build_ok(ch, &program, Some(build))
                            })
                })
            }
        };
        if !admitted {
            continue;
        }
        let exposes: Vec<String> = match vendor {
            Some(spec) => spec.exposes.iter().map(|e| (*e).to_string()).collect(),
            None => {
                crate::installed_exposes(layout, &program).unwrap_or_else(|| vec![program.clone()])
            }
        };
        let bin = build_dir.join("bin");
        let exposes: Vec<String> = exposes
            .into_iter()
            .filter(|name| {
                bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
                    .is_file()
            })
            .collect();
        let (tools, _) = crate::store::split_exposed(&exposes);
        if tools.is_empty() {
            continue;
        }
        let label = crate::vendor_direct::display_build(&program, build);
        // A sysroot bundle is checked first, as its install checks it: compilers whose
        // libraries do not load are never put on PATH.
        if build_dir.join("lib").join("rustlib").is_dir()
            && let Err(e) = crate::flow::bundle_resolve_check(&build_dir, &tools)
        {
            eprintln!(
                "atpkg: {program}: the interrupted activation of {label} was not finished ({e}) \
                 — this pass decides it afresh"
            );
            continue;
        }
        let aliases = match vendor {
            Some(_) => crate::activate::Aliases::Off,
            None => alias_policy_offline(layout, &program),
        };
        let env = crate::shim_env::read_sidecar(&build_dir);
        match crate::activate::install_tools_env(layout, &build_dir, &tools, aliases, &env) {
            Ok(()) => {
                println!(
                    "atpkg: {program}: finished an interrupted activation — its shims now run \
                     {label}"
                );
                record_row_keeping_outcome(
                    layout,
                    &program,
                    crate::status::store_row(layout, &program, build),
                );
                rolled.push(program);
            }
            Err(e) => eprintln!(
                "atpkg: {program}: could not finish the interrupted activation of {label} ({e}) \
                 — this pass decides it afresh"
            ),
        }
    }
    rolled
}

/// The live dev links, or `Err` after recording broken ones as a `*links*` fault (never as
/// an unsupported architecture); the doctor uses the same predicate.
fn update_link_inventory(
    layout: &crate::store::Layout,
) -> Result<std::collections::BTreeSet<String>, ()> {
    let (live, problems) = crate::linkmode::live_dev_links(layout);
    if problems.is_empty() {
        clear_status_row(layout, "*links*");
        return Ok(live);
    }
    let message = problems.join("; ");
    eprintln!("atpkg: {message}");
    record_unbuilt(layout, "*links*", format!("error: {message}"), message);
    Err(())
}

fn update_toolset_is_empty(
    managed: &std::collections::BTreeMap<String, u64>,
    linked: &std::collections::BTreeSet<String>,
) -> bool {
    managed.is_empty() && linked.is_empty()
}

/// [`cmd_update_all`]'s body, returning the raw exit code so the announcement can be
/// answered with the pass's real verdict rather than a guess read off its output.
fn cmd_update_all_code() -> u8 {
    if !manager_enabled() {
        eprintln!("atpkg: disabled (no root key pinned) — nothing to update");
        return 1;
    }
    let Some(layout) = layout() else {
        return 1;
    };
    // Whatever this pass ends, its promise ends with it ([`crate::progress::pass_owed`]): the
    // update a seed promised is this one, so a stub waiting on that promise answers at once.
    // Its promise only: the one it wrote, or the seed's of the window that spawned it, never
    // another window's seed's for the update that window spawns next ([`PromiseEnds`]).
    let _promise = PromiseEnds(&layout);
    // The holder did this pass: a child that queued behind another full pass whose end was
    // recorded since it started waiting does not repeat it back to back.
    if let Some((outcome, ago)) = pass_ended_while_we_waited(&layout) {
        let record = crate::status::read(&layout);
        let said = record
            .as_ref()
            .map(|s| s.outcome.clone())
            .unwrap_or_default();
        let stale_index_only = record
            .as_ref()
            .is_some_and(crate::status::stale_index_only_end);
        let stand_down = stand_down_exit(outcome, ago, &said, stale_index_only);
        if stand_down.on_stderr {
            eprintln!("{}", stand_down.line);
        } else {
            println!("{}", stand_down.line);
        }
        // Literal returns, one per ending: `lock`'s exit-code census reads this body's
        // `return <literal>` lines to prove the contention code is shared by nothing.
        match outcome {
            aterm_update_core::pkg_check::PassOutcome::Ok => {
                debug_assert_eq!(stand_down.code, 0);
                return 0;
            }
            aterm_update_core::pkg_check::PassOutcome::Offline => {
                debug_assert_eq!(
                    stand_down.code,
                    aterm_update_core::pkg_check::PASS_OFFLINE_EXIT
                );
                return aterm_update_core::pkg_check::PASS_OFFLINE_EXIT;
            }
            aterm_update_core::pkg_check::PassOutcome::Failed => {
                debug_assert_eq!(stand_down.code, 1);
                return 1;
            }
        }
    }
    let cfg = crate::config::cached();
    // Reconcile `[packages.links]` first, so a config-declared dev-link is in place before
    // the apply decides what it manages (linked programs hard-skip).
    reconcile_links(&layout, cfg);
    let Ok(dev_linked) = update_link_inventory(&layout) else {
        return 1;
    };
    // The active build per program (the shim view), not the max complete build on disk, so
    // `decide` never treats a staged-but-unactivated build as the running one. Its silence
    // for a tombstoned program is the self-heal: `decide` returns Install for the build
    // `store/<p>/current` names, the transaction re-stages it (`Staged::was_live`), and the
    // shims come back; with the authority link it would decide UpToDate forever.
    let installed: std::collections::BTreeMap<String, u64> = crate::active_builds(&layout);
    let complete_the_set = completes_the_set(&layout, cfg);
    if !has_work(&installed, &dev_linked, complete_the_set, &layout) {
        println!("{EMPTY_UPDATE}");
        // A full pass with nothing to check still ended, and says so: the schedulers count
        // their walk from it.
        let _ = crate::status::stamp_pass_end(
            &layout,
            &now_rfc3339(),
            aterm_update_core::pkg_check::PassOutcome::Ok,
        );
        return 0;
    }
    // A pass that will plan the set promises its own plan: the index resolve, the vendor lane
    // and the manifest prefetch come first, and a stub run meanwhile waits for the plan. The
    // owner is this process, so its exit breaks the promise; it is retired once the plan
    // lands in `progress.json` ([`install_default_set_inner`]) or at the latest on return.
    if update_promises_its_plan(
        &layout,
        complete_the_set && pass_progress_path().is_some(),
        crate::flow::now_unix().max(0).unsigned_abs(),
    ) {
        crate::progress::promise_pass(
            &layout,
            std::process::id(),
            crate::progress::PASS_PROMISE_SECS,
        );
    }
    let fetcher = resolve_fetcher(&layout);
    run_recorded_update_pass(
        &layout,
        cfg,
        &*fetcher,
        &installed,
        complete_the_set,
        &PassSeams::process(),
    )
}

/// Whether a whole update that `plans` the set with a progress file promises its own plan
/// ([`cmd_update_all_code`]): only on a first install ([`crate::adoption::first_install_ahead`]),
/// or over a promise it finds at `now_unix` (the window seed's, which it takes over). A
/// routine pass's stub is a program held, unserved here or failed last time, and its
/// recorded reason answers at once ([`pending_not_running`]) rather than after a wait for a
/// plan that leaves it out.
fn update_promises_its_plan(layout: &crate::store::Layout, plans: bool, now_unix: u64) -> bool {
    plans
        && (crate::adoption::first_install_ahead(layout)
            || crate::progress::pass_owed(layout, now_unix))
}

/// Retires the promise of a pass when the whole update holding it returns, by any path
/// ([`cmd_update_all_code`]): a promise this process wrote, or one its spawner holds (the
/// seed's of the window that spawned this update, [`seed_promise_owner`]), and no other
/// ([`crate::progress::retire_promise_of`]). A typed `aterm pkg update` that runs between a
/// window's seed and that window's update leaves the seed's promise to it.
struct PromiseEnds<'a>(&'a crate::store::Layout);

impl Drop for PromiseEnds<'_> {
    fn drop(&mut self) {
        let mut holders = vec![std::process::id()];
        holders.extend(wait_lock().and_then(|w| w.spawner));
        crate::progress::retire_promise_of(self.0, &holders);
    }
}

/// Who gets set-completion ([`should_complete_set`] over this machine): an adopted toolset
/// under the install consent with automatic updates on, so a program published after a
/// user installed reaches them; `declined` outranks it all.
fn completes_the_set(layout: &crate::store::Layout, cfg: &crate::config::PackagesConfig) -> bool {
    should_complete_set(
        cfg.installs_unattended(),
        adopted(layout),
        cfg.auto_install == Some(true),
        cfg.enabled(),
        declined(layout),
        cfg.unreadable,
    )
}

/// Whether an `update` pass has anything to do: a program is managed (the shim view) or
/// dev-linked ([`update_link_inventory`]), the set is being completed, or a vendor program
/// is tombstoned in place (its lane revisits it). Else [`EMPTY_UPDATE`], with no network.
fn has_work(
    installed: &std::collections::BTreeMap<String, u64>,
    linked: &std::collections::BTreeSet<String>,
    complete_the_set: bool,
    layout: &crate::store::Layout,
) -> bool {
    !update_toolset_is_empty(installed, linked)
        || complete_the_set
        || vendor_tombstoned_in_place(layout)
}

/// The long-lived index workers need the current install consent, not
/// [`crate::config::cached`]'s one-time snapshot; a metadata stamp keeps the frequent
/// empty-store check from reparsing an unchanged file.
struct ProbePackagesConfig {
    path: Option<std::path::PathBuf>,
    seen: Option<Option<(Option<std::time::SystemTime>, u64)>>,
    cfg: crate::config::PackagesConfig,
}

impl ProbePackagesConfig {
    fn new(path: Option<std::path::PathBuf>, cfg: crate::config::PackagesConfig) -> Self {
        Self {
            path,
            seen: None,
            cfg,
        }
    }

    fn poll(&mut self) -> &crate::config::PackagesConfig {
        if let Some(path) = self.path.as_deref() {
            let stamp = match std::fs::metadata(path) {
                Ok(m) => Some((m.modified().ok(), m.len())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => {
                    self.cfg = crate::config::PackagesConfig::unreadable_table();
                    self.seen = None;
                    return &self.cfg;
                }
            };
            // A chmod or privacy-consent change need not move mtime or length: a failed
            // open must not keep an earlier config's unattended-install permission alive.
            if stamp.is_some() && std::fs::File::open(path).is_err() {
                self.cfg = crate::config::PackagesConfig::unreadable_table();
                self.seen = None;
                return &self.cfg;
            }
            if self.seen != Some(stamp) {
                match crate::config::load_live(path) {
                    Some(cfg) => {
                        self.cfg = cfg;
                        self.seen = Some(stamp);
                    }
                    None => {
                        self.cfg = crate::config::PackagesConfig::unreadable_table();
                        // A transient read failure can recover without a new
                        // mtime. Try again on the next lightweight probe.
                        self.seen = None;
                    }
                }
            }
        }
        &self.cfg
    }
}

/// [`has_work`] for the store as it stands, under the live `[packages]` table — what the
/// next-index probe asks before it spends a HEAD
/// ([`crate::index_probe::successor_reporting_near_with`]): a published index can wake
/// nothing on a store whose `update` would answer [`EMPTY_UPDATE`]. Read-only: the dev links
/// are read, never reconciled. Asked every five seconds while the store is empty, so the
/// cheap set-completion predicate answers first.
pub(crate) fn update_pass_has_work(layout: &crate::store::Layout) -> bool {
    static PROBE_CONFIG: std::sync::OnceLock<std::sync::Mutex<ProbePackagesConfig>> =
        std::sync::OnceLock::new();
    let cfg = PROBE_CONFIG.get_or_init(|| {
        // `poll` reads the live file on its first call.
        std::sync::Mutex::new(ProbePackagesConfig::new(
            crate::config::config_path(),
            crate::config::PackagesConfig::default(),
        ))
    });
    let mut cfg = cfg.lock().unwrap_or_else(|poison| poison.into_inner());
    update_pass_has_work_with(layout, cfg.poll())
}

fn update_pass_has_work_with(
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
) -> bool {
    probe_has_work_lazily(
        completes_the_set(layout, cfg),
        || !crate::active_builds(layout).is_empty(),
        || vendor_tombstoned_in_place(layout),
        || !crate::linkmode::live_dev_links(layout).0.is_empty(),
    )
}

/// [`has_work`]'s decision for the probe, skipping the costlier read-only scans once any
/// sufficient condition holds.
fn probe_has_work_lazily(
    complete_the_set: bool,
    managed: impl FnOnce() -> bool,
    tombstoned_vendor: impl FnOnce() -> bool,
    linked: impl FnOnce() -> bool,
) -> bool {
    complete_the_set || managed() || tombstoned_vendor() || linked()
}

/// [`run_update_pass`], then its record for the schedulers
/// ([`crate::status::stamp_pass_end`]): `ok` when the pass stamped a success, `offline` or
/// `failed` by its exit, bound to the index it was launched for or resolved — what the
/// window's gate and the session lane read instead of `updated_at` (the derived models
/// `AtpkgPassStamps` and `AtpkgFullPassRule`).
fn run_recorded_update_pass(
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    fetcher: &dyn crate::flow::Fetcher,
    installed: &std::collections::BTreeMap<String, u64>,
    complete_the_set: bool,
    seams: &PassSeams<'_>,
) -> u8 {
    let stamped_before = successes_stamped();
    PASS_INDEX_RESOLVE_ATTEMPTED.with(|seen| seen.set(false));
    RESOLVED_PASS_INDEX_BUILD.with(|seen| seen.set(0));
    PASS_FAILED_ONLY_FOR_ITS_INDEX.with(|only| only.set(false));
    let code = run_update_pass(layout, cfg, fetcher, installed, complete_the_set, seams);
    let outcome = aterm_update_core::pkg_check::PassOutcome::of_pass(
        successes_stamped() != stamped_before,
        code,
    );
    let requested = PUBLISHED_INDEX_HINT.with(std::cell::Cell::get).unwrap_or(0);
    let resolved = RESOLVED_PASS_INDEX_BUILD.with(std::cell::Cell::get);
    let attempted = PASS_INDEX_RESOLVE_ATTEMPTED.with(std::cell::Cell::get);
    // Only the exit this pass actually took counts: code 1 from the "failed only for its
    // index" arm, never a code the flag outlived.
    let stale_index_only = code == 1 && PASS_FAILED_ONLY_FOR_ITS_INDEX.with(std::cell::Cell::get);
    let _ = crate::status::stamp_pass_end_recorded(
        layout,
        &rfc3339_at((seams.now)()),
        outcome,
        attempted_pass_index_build(requested, resolved, attempted),
        stale_index_only,
    );
    code
}

/// The index build this pass attempted: the hinted build, or a newer signed selection —
/// never lower than the hint, so the same requested build is not repeated. `0` (unknown)
/// when no index request was made.
fn attempted_pass_index_build(requested: u64, resolved: u64, attempted: bool) -> u64 {
    if attempted {
        requested.max(resolved)
    } else {
        0
    }
}

/// What a pass takes from the process rather than from its store — the index anchor, the
/// clock, the vendor anchors, the shell hooks and the window's progress file — so a test
/// drives the whole pass over a scratch store ([`PassSeams::process`] in production).
struct PassSeams<'a> {
    /// The index anchor, read per call (a pass can ratchet the roster floor).
    anchor: fn(&crate::store::Layout) -> crate::Anchor,
    /// The clock (Unix seconds).
    now: fn() -> i64,
    /// What the vendor lane verifies under.
    trust: &'a crate::vendor_direct::lane::Trust,
    /// The interactive-shell hook refresh, which writes outside the store.
    shell_hooks: fn(&crate::store::Layout),
    /// The rustup seam's re-assertion ([`reassert_rustup_seam`]), which writes outside the
    /// store.
    rustup_seam: &'a dyn Fn(&crate::store::Layout),
    /// The live progress file: the `--progress-file` the window asked for, or a typed
    /// pass's own ([`pass_progress_path`]).
    progress: Option<&'a std::path::Path>,
    /// The `PATH` the pass judges shadowing and system installs against, read once.
    path: Option<std::ffi::OsString>,
    /// The warm of the agent builds a vendor lane landed ([`crate::warm`]): each run once
    /// by a detached process that never writes the store.
    warm_agents: fn(&crate::store::Layout, &std::collections::BTreeSet<String>),
    /// Whether this process runs under Rosetta translation ([`running_translated`]).
    translated: fn() -> bool,
    /// Whether the toolchain's flip may wait for quiet ([`flip_policy`], the edge's
    /// [`DEFER_BUSY_FLIP_FLAG`]).
    flip_policy: fn() -> crate::quiet::FlipPolicy,
    /// The process table the flip gate reads ([`crate::gc::process_table`]).
    running: &'a dyn Fn() -> Option<Vec<std::path::PathBuf>>,
    /// How long a first run's vendor plan waits for a read still out ([`VENDOR_READ_GRACE`]).
    vendor_read_grace: std::time::Duration,
    /// Told the program each vendor read ahead is in for, on the thread that read it
    /// ([`read_vendors_ahead`]).
    read_ahead_in: &'a (dyn Fn(&str) + Sync),
}

impl PassSeams<'static> {
    /// This process's own.
    fn process() -> Self {
        Self {
            anchor: effective_anchor,
            now: now_unix,
            trust: &crate::vendor_direct::lane::Trust::PRODUCTION,
            shell_hooks: refresh_shell_hooks,
            rustup_seam: &reassert_rustup_seam,
            progress: pass_progress_path(),
            path: std::env::var_os("PATH"),
            warm_agents: crate::warm::spawn_landed,
            translated: running_translated,
            flip_policy,
            running: &crate::gc::process_table,
            vendor_read_grace: VENDOR_READ_GRACE,
            read_ahead_in: &no_read_ahead_note,
        }
    }
}

/// [`PassSeams::read_ahead_in`] for a pass that needs no word of its reads.
fn no_read_ahead_note(_: &str) {}

/// The shell's base relay items ([`SHELL_RELAY`]) as an install or update pass runs them
/// at its end ([`relay_base`]).
fn refresh_shell_hooks(layout: &crate::store::Layout) {
    relay_base(layout, &SHELL_RELAY);
}

/// The shell's half of the base relay items (design init-system §4.2): the `~/.local/bin`
/// links, the `shell.d` hooks, and atpkg's block in the rc files (the login shell's own rc
/// created when absent), in the repair items' order. What an install or update pass lays
/// at its end, under the store lock it holds: none of them writes the store.
const SHELL_RELAY: [&str; 3] = [
    crate::converge::repair::LINKS,
    crate::converge::repair::HOOKS,
    crate::converge::repair::RC,
];

/// The base relay items the seed runs before the rustup seam (design init-system §5:
/// `seed` = the base relay items plus adoption, unattended): [`SHELL_RELAY`], then the
/// reroute stubs, an outlived held update and the installed programs' shims. The seam is
/// the pass's own re-assertion ([`reassert_rustup_seam`]), as the update pass's is: it is
/// the step that tells the window which foreign rustup entries stand (the `foreign:`
/// marker), which the seam item, `NotApplicable` over a foreign entry, cannot say. The
/// exec roots follow it ([`SEED_RELAY_AFTER_SEAM`]), the order every pass keeps.
const SEED_RELAY: [&str; 6] = [
    crate::converge::repair::LINKS,
    crate::converge::repair::HOOKS,
    crate::converge::repair::RC,
    crate::converge::repair::STUBS,
    crate::converge::repair::HELD_UPDATE,
    crate::converge::repair::SHIMS,
];

/// The base relay items the seed runs after the rustup seam: the trust build's exec root.
const SEED_RELAY_AFTER_SEAM: [&str; 1] = [crate::converge::repair::EXEC_ROOTS];

/// Run the base relay items `ids` ([`crate::converge::repair`]) as an unattended pass
/// runs them, under the store lock its verb holds: the doctor's pass (an rc block or a
/// login rc the person deleted stays deleted; a bundle that is not the release
/// `aterm.app` lays no shell integration), each applied where its probe differs, each
/// change journaled first, where its items' account is (design init-system §3.3's
/// [`aterm_types::dirs::account_state_dir`]: a scratch `$HOME`'s pass journals in that
/// scratch). Run as root it changes nothing of the account's (`Requires::Account`): `sudo`
/// keeps `$HOME`, and a root-owned rc would lock its owner out of it. Says what each item
/// did on stdout and what it could not do on stderr, and never fails the pass.
fn relay_base(layout: &crate::store::Layout, ids: &[&str]) {
    let ground = std::sync::Arc::new(crate::converge::repair::Ground::of_process(
        layout.clone(),
        crate::converge::repair::Pass::Doctor,
    ));
    let rows = relay_rows(
        &ground,
        ids,
        aterm_types::dirs::account_state_dir().as_deref(),
    );
    for row in &rows {
        for line in relay_said(row) {
            println!("{line}");
        }
        if let Some(warn) = relay_warning(row) {
            eprintln!("{warn}");
        }
    }
}

/// Why a relay pass changes nothing of the account's: it runs as root (`sudo` keeps
/// `$HOME`). A scratch `$HOME` or a private state root is the pass's own tree, laid as
/// before items existed, so only root holds it back.
fn relay_not_own_account() -> Option<&'static str> {
    #[cfg(unix)]
    {
        relay_not_own_account_with(aterm_uds::peer::our_uid())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The pure half of [`relay_not_own_account`], for the effective uid `euid`.
#[cfg(unix)]
fn relay_not_own_account_with(euid: u32) -> Option<&'static str> {
    (euid == 0).then_some("it runs as root")
}

/// [`relay_base`]'s rows over `ground`: `ids` through the engine, journaled under `state`
/// (`None`: no root to journal into, so every change is refused and says why).
fn relay_rows(
    ground: &std::sync::Arc<crate::converge::repair::Ground>,
    ids: &[&str],
    state: Option<&std::path::Path>,
) -> Vec<aterm_converge::ItemReport> {
    let scope = aterm_converge::Scope {
        applies: &[aterm_converge::Class::Relay],
        at_console: true,
        not_own_account: relay_not_own_account(),
    };
    match state {
        Some(state) => repair_rows(
            ground,
            &mut aterm_converge::FileJournal::under(state),
            Some(ids),
            scope,
        ),
        None => repair_rows(ground, &mut aterm_converge::ReadOnly, Some(ids), scope),
    }
}

/// What a relay row did, one `atpkg:` line each (its detail); nothing for a row that
/// changed nothing.
fn relay_said(row: &aterm_converge::ItemReport) -> Vec<String> {
    row.detail
        .iter()
        .flat_map(|detail| detail.lines())
        .map(|line| {
            if line.starts_with("atpkg: ") {
                line.to_owned()
            } else {
                format!("atpkg: {line}")
            }
        })
        .collect()
}

/// The one stderr line for a relay row left undone (failed, blocked, unread), or `None`.
/// A row that stood aside, or that is not this machine's to do, says nothing.
fn relay_warning(row: &aterm_converge::ItemReport) -> Option<String> {
    use aterm_converge::Outcome;
    match row.outcome {
        Outcome::Failed | Outcome::Blocked | Outcome::Unknown if row.stood_aside.is_none() => {
            Some(format!(
                "atpkg: warn — {}: not done — {} (`aterm pkg repair` retries)",
                row.title,
                row.why.as_deref().unwrap_or("it failed")
            ))
        }
        _ => None,
    }
}

/// Whether a vendor program is installed here but disabled by a tombstone
/// ([`crate::flow::tombstoned_in_place`]).
fn vendor_tombstoned_in_place(layout: &crate::store::Layout) -> bool {
    crate::flow::tombstoned_in_place(
        layout,
        crate::vendor_direct::VENDORS.iter().map(|s| s.program),
    )
}

/// The index lane could not run: settle what the vendor lane moved ([`settle_vendor_pass`]),
/// say which agents it left current, then record the failed pass.
fn update_pass_stopped(
    layout: &crate::store::Layout,
    vendor: &VendorPass,
    e: &crate::FlowError,
    index_answered: bool,
    seams: &PassSeams<'_>,
) -> u8 {
    if let Some(report) = settle_vendor_pass(layout, vendor, seams) {
        print_gc_sweeps("update", &report);
        print_gc_abstentions("update", &report);
    }
    print_managed_current(layout, &vendor.unchecked, (seams.now)());
    let code = update_pass_failed(layout, e);
    // Nothing reached: offline, not failed. `Stale` counts as offline too when nothing
    // answered (the cached index aged out while the machine was off the network); a stale
    // index from a host that answered is the publisher's failure.
    let offline = matches!(
        e,
        crate::FlowError::Unreachable(_) | crate::FlowError::Stale
    ) && pass_reached_nothing(index_answered, vendor);
    finish_update_pass(layout, (seams.now)(), false, offline, code)
}

/// Whether a pass reached nothing: no host answered its index resolve (a refusal is an
/// answer, [`crate::flow::ResolveProvenance::answered`]) and no vendor's channel answered.
/// Such a pass exits [`aterm_update_core::pkg_check::PASS_OFFLINE_EXIT`], which the window
/// retries quietly and soon.
fn pass_reached_nothing(index_answered: bool, vendor: &VendorPass) -> bool {
    !index_answered && !vendor.reached
}

/// [`cmd_update_all_code`]'s pass over the store it is handed: the vendor lane, the
/// index lane, set completion, the pass-end gc and the verdict. `installed` is the shim
/// view the caller read after reconciling links.
fn run_update_pass(
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    fetcher: &dyn crate::flow::Fetcher,
    installed: &std::collections::BTreeMap<String, u64>,
    complete_the_set: bool,
    seams: &PassSeams<'_>,
) -> u8 {
    // The vendor lane's reads ahead run on threads of their own beside the pass, which waits
    // for them all before it returns.
    let reads = crate::vendor_direct::lane::Reads::new();
    std::thread::scope(|scope| {
        run_update_pass_in(
            scope,
            &reads,
            layout,
            cfg,
            fetcher,
            installed,
            complete_the_set,
            seams,
        )
    })
}

/// [`run_update_pass`] inside the scope its vendor reads ahead run in ([`read_vendors_ahead`]).
#[allow(
    clippy::too_many_arguments,
    reason = "run_update_pass's inputs and the scope and reads its vendor lane runs ahead in"
)]
fn run_update_pass_in<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    reads: &'env crate::vendor_direct::lane::Reads,
    layout: &'env crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    fetcher: &'env dyn crate::flow::Fetcher,
    installed: &std::collections::BTreeMap<String, u64>,
    complete_the_set: bool,
    seams: &PassSeams<'env>,
) -> u8 {
    let mut failures = 0u32;
    // Whether either lane of this pass resolved the signed index (live or cached): the
    // pass-end build stamp's condition. On an empty store the set-completion lane is the
    // whole pass, so a failure count alone cannot say which kind of pass this was.
    let mut index_resolved = false;
    // What this pass's resolves verified from the channel itself, counted from here: the
    // one fact its success stamp may record ([`finish_update_pass`]).
    let live_before = crate::flow::live_verifications();
    // The index build the channel apply resolved, for the pass-end freshness stamp.
    let mut resolved_index_build: Option<u64> = None;
    // `program → pinned asset name` for everything either lane resolved: the pass-end gc
    // spares a failed download's `.part`.
    let mut resolved_assets: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    // The one index this pass verifies, once a lane has resolved it, handed to every lane
    // below ([`reusable_index`]).
    let mut pass_index: Option<crate::TrustedIndex> = None;
    // Whether this pass said the C toolchain's verdict ([`C_TOOLCHAIN_MARKER`]) already.
    let mut c_toolchain_said = false;
    // The pass's one verify-select, first, before the vendor lane: a fresh verification
    // refreshes the yank latch, so the vendor lane binds the yanks published since the last
    // pass. A failed resolve blocks nothing: the vendor lane runs on the latch, and only
    // then does the index lane stop. A pass with no index work (only a vendor program
    // tombstoned in place) records nothing about one.
    let index_asked = !installed.is_empty() || complete_the_set;
    // THE VENDOR LANE'S READS GO OUT WITH THE INDEX RESOLVE, not after it: its heads and the
    // documents they name need no index — only its yanks do, and the plan that binds them
    // takes what was read once the index is in ([`read_vendors_ahead`]).
    let reading = read_vendors_ahead(
        scope,
        reads,
        layout,
        fetcher,
        cfg,
        installed,
        complete_the_set,
        seams,
    );
    let mut resolved = index_asked.then(|| {
        PASS_INDEX_RESOLVE_ATTEMPTED.with(|seen| seen.set(true));
        crate::resolve_verified_index(
            fetcher,
            layout,
            &(seams.anchor)(layout),
            build_floor(layout),
            (seams.now)(),
        )
    });
    // Record the signed index as soon as selection returns, before the vendor lane or the
    // apply can fail early, so a later hint for this same build is not re-run.
    if let Some(Ok(index)) = resolved.as_ref() {
        RESOLVED_PASS_INDEX_BUILD.with(|seen| seen.set(seen.get().max(index.index_build)));
    }
    // Whether a host answered that resolve, read before another resolve overwrites it: a
    // refusal (a rate limit, a 5xx, a portal's page) is not offline, nor is no resolve.
    let provenance = resolved.as_ref().and(crate::flow::last_resolve());
    let mut index_answered = resolved.is_none() || provenance.as_ref().is_some_and(|r| r.answered);
    // The members this pass loses at a fetch ([`crate::flow::note_fetch_fault`]): an
    // unreached pass is offline only when every failure it counts is one of those.
    let faults_before = crate::flow::fetch_faults();
    // A published-index hint means the window already saw a newer signed build: once this
    // pass verifies it live, its installed members are applied before the vendor lane's
    // network work, the result (even an error) kept until after the vendor lane.
    // Otherwise the vendor lane goes first.
    //
    // The one flip gate of this pass, for the apply lane and the set completion.
    let flip = crate::quiet::FlipGate {
        policy: (seams.flip_policy)(),
        now: (seams.now)(),
        running: seams.running,
    };
    // The members of a tuple whose flip the apply lane held for quiet: the set completion
    // leaves that tuple to the held flip, which brings its absent members with it.
    let mut held_flip: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let apply_index = |index: &crate::TrustedIndex| {
        let typed = TypedPass::begin("update");
        let applied = crate::flow::apply_channel_gated(
            fetcher,
            layout,
            index,
            crate::config::CHANNEL,
            current_triple(),
            installed,
            cfg.exclude(),
            &flip,
        );
        drop(typed);
        applied
    };
    let hinted_live_index = PUBLISHED_INDEX_HINT
        .with(std::cell::Cell::get)
        .is_some_and(|hint| {
            !installed.is_empty()
                && provenance.as_ref().is_some_and(|r| r.reached)
                && matches!(resolved.as_ref(), Some(Ok(index)) if index.index_build >= hint)
        });
    let mut priority_applied = if hinted_live_index {
        resolved
            .as_ref()
            .and_then(|answer| answer.as_ref().ok())
            .map(&apply_index)
    } else {
        None
    };
    // Ordinarily the vendor lane goes first: claude and codex move with their vendors even
    // when the index is stale or unreachable. Under a person's meter too ([`TypedPass`]).
    let consent = set_completion_consent(adopted(layout), adopted_by_seed(layout));
    let typed = TypedPass::begin("update");
    // A first run's set announces once this plan is in, so a vendor whose read is still out
    // a moment past the index lands after the set instead ([`VENDOR_READ_GRACE`]); every
    // other pass lands its installed vendors first and waits for them all.
    let first_run = complete_the_set && installed.is_empty() && matches!(resolved, Some(Ok(_)));
    // A first run's set manifests go out while the plan waits ([`prefetch_set_manifests`]),
    // joined with it, on a fetcher a pass may share. Only a first run's: its set completion
    // comes straight after the plan, where any other pass's apply lane comes first, and
    // joining the manifests there would hold an update's flip behind them.
    let set_index = match &resolved {
        Some(Ok(index)) if first_run => Some(index),
        _ => None,
    };
    let (mut vendor, mut fresh) = std::thread::scope(|beside| {
        if let (Some(index), Some(shared)) = (set_index, fetcher.shared()) {
            // A thread the system refuses leaves the manifests to the prescan's own prefetch.
            let _ = std::thread::Builder::new()
                .name("atpkg-set-manifests".into())
                .spawn_scoped(beside, move || {
                    prefetch_set_manifests(layout, shared, cfg, installed, index);
                });
        }
        let plan = vendor_plan_reading(
            layout,
            fetcher,
            cfg,
            installed,
            VendorLanes {
                update_installed: true,
                complete_the_set,
            },
            match &resolved {
                Some(Ok(index)) => Some(index),
                _ => None,
            },
            seams,
            reading.then(|| ReadsAhead {
                reads,
                until: first_run.then(|| std::time::Instant::now() + seams.vendor_read_grace),
            }),
        );
        // A first run's floor advance while the manifests are still out: its three durable
        // writes (17 ms) stood between the last manifest and the announcement. The set
        // completion runs next on a first run, and its own advance then has nothing to write.
        if let Some(index) = set_index {
            advance_floors(layout, index.index_build, index.roster_seq());
        }
        plan
    });
    let late = std::mem::take(&mut fresh.late);
    // AN EMPTY STORE'S FRESH VENDORS RIDE ITS SET COMPLETION, not ahead of it. On a first
    // run every agent and the whole ALab set arrive in one pass, and landing the four agents
    // first held the toolchain's first byte back by their whole download (about 770 MiB);
    // now the toolchain's tuple goes first and the agents land beside the rest of the set
    // ([`VendorRider`]). The index this pass resolved is verified and the yank latch fresh,
    // so the vendors bind the yanks they would have bound first; with no index the set has
    // nothing to ride, and a store with programs keeps the vendor-first order.
    let mut rider = if complete_the_set
        && installed.is_empty()
        && matches!(resolved, Some(Ok(_)))
        && !fresh.pending.is_empty()
    {
        Some(VendorRider {
            fresh,
            trust: seams.trust,
            warm: seams.warm_agents,
            landed: None,
        })
    } else {
        land_fresh_alone(layout, fetcher, consent, &fresh, &mut vendor, seams, false);
        None
    };
    warm_landed(layout, &mut vendor, seams);
    drop(typed);
    failures += vendor.failures;
    resolved_assets.extend(vendor.resolved_assets.clone());
    let vendor_failed: Vec<String> = vendor.failed.iter().map(|(p, _)| p.clone()).collect();
    if !installed.is_empty()
        && let Some(resolved) = resolved.take()
    {
        let index = match resolved {
            Ok(index) => index,
            Err(e) => {
                return update_pass_stopped(layout, &vendor, &e, index_answered, seams);
            }
        };
        index_resolved = true;
        let applied = priority_applied
            .take()
            .unwrap_or_else(|| apply_index(&index));
        let report = match applied {
            Ok(r) => r,
            Err(e) => {
                return update_pass_stopped(layout, &vendor, &e, index_answered, seams);
            }
        };
        // Advance the durable anti-rollback floor to the index we just trusted.
        advance_floors(layout, report.index_build, report.roster_seq);
        // For the pass-end freshness stamp.
        resolved_index_build = Some(report.index_build);
        // What the apply resolved, for the pass-end gc's sparing closure: a member whose
        // group aborted on a failed download keeps its `.part`.
        resolved_assets.extend(report.resolved_assets.clone());
        // Clear the failure this pass disproved: the `*index*` row is written when a
        // resolve fails, and only a resolve that succeeds removes it.
        clear_status_row(layout, "*index*");
        // Repair a member that went live without its attestation (a pass killed inside the
        // flip window): `atpkg verify` fails closed on a missing root, so re-derive it from
        // the same authority the install used, never a local recomputation.
        recover_missing_roots(layout, fetcher, Some(&index));
        // …and the sentence that row left behind (Settings ▸ Packages's detail line). A
        // vendor refusal this pass stays its headline: a pass that exits 1 only for the
        // vendor lane is never "up to date".
        record_outcome(
            layout,
            vendor_folded_outcome(
                &vendor,
                format!("up to date (index build {})", report.index_build),
            ),
        );
        // Added to the vendor lane's faults, never over them.
        failures += report_channel_apply(layout, installed, &report, &vendor_failed, true);
        warm_alab_landed(layout, installed, seams.warm_agents);
        for (group, outcome) in &report.groups {
            if matches!(outcome, crate::TxnOutcome::Deferred { .. }) {
                held_flip.extend(group.members.iter().cloned());
            }
        }
        for p in &report.skipped_linked {
            println!("atpkg: {p} dev-linked — skipped");
        }
        // The set-completion lane runs its own alias and shadow reconcile; a pass that only
        // applies runs them here, from the index it just verified.
        if !complete_the_set {
            // From the index this pass verified: `cached_index` would re-run the roster
            // admit and signature checks, and skip the freshness gate this one passed.
            reconcile_aliases(layout, &index);
            reconcile_shadowed(layout, report.index_build, seams.path.as_deref());
        }
        // On to the set-completion lane and the empty-store diagnosis below.
        pass_index = Some(index);
    }
    // An empty store's set completion reads the index resolved above, never a second one.
    if pass_index.is_none()
        && let Some(Ok(index)) = resolved
    {
        pass_index = Some(index);
    }
    // Install the default-set members not yet installed (exclude-narrowed; linked/yanked
    // members skip; per-program failures are loud but never block the rest): what keeps a
    // toolset complete as the suite grows, and fills an empty store under the consent.
    if complete_the_set {
        // Announce before acting: this pass can pull gigabytes, and the window reads only
        // the marker lines off its stdout.
        let before_net = crate::active_builds(layout);
        // The Trust toolchain is usable the moment its tuple flips: the rustup view and the
        // exec roots follow it then, not when the rest of the pass is done (the pass end
        // re-lays both, idempotently).
        let toolchain_flipped = |layout: &crate::store::Layout| {
            (seams.rustup_seam)(layout);
            let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
        };
        // The consent the line states is the one that holds on this machine: installing
        // aterm, with `auto_install = false` as the switch that would have stopped it —
        // unless a person adopted the set with the explicit verb ([`set_completion_consent`]).
        let net = install_default_set_with_path(
            layout,
            fetcher,
            &(seams.anchor)(layout),
            cfg,
            consent,
            (seams.now)(),
            pass_index.as_ref(),
            seams.path.as_deref(),
            &flip,
            &held_flip,
            SetCompanions {
                vendor: rider.as_mut(),
                toolchain_flipped: &toolchain_flipped,
                vendors_after: !late.is_empty(),
            },
        );
        failures += net.failures;
        warm_alab_landed(layout, &before_net, seams.warm_agents);
        // What arrived under the set completion's announcement — the vendors that rode it
        // included, the ones landed alone below (with a terminal of their own) not.
        let mut arrived: Vec<String> = crate::active_builds(layout)
            .keys()
            .filter(|k| !before_net.contains_key(k.as_str()))
            .cloned()
            .collect();
        // What the rider's landings came to, or its landings alone when the set completion
        // stopped before it could carry them. The promise goes with that plan only when
        // nothing lands after it: a vendor whose read was still out lands next, and its plan
        // takes the promise over ([`land_late_vendors`]) — a stub waiting for it never reads
        // it gone first.
        let mut deferred: Vec<&'static str> = Vec::new();
        if let Some(rider) = rider.take() {
            let failed_before = vendor.failures;
            deferred = settle_rider(
                layout,
                fetcher,
                consent,
                rider,
                &mut vendor,
                seams,
                late.is_empty(),
            );
            failures += vendor.failures - failed_before;
            resolved_assets.extend(vendor.resolved_assets.clone());
        }
        // An empty store may retry after the first resolve failed: a retry that reached a
        // host makes the pass not offline. (Whether it reached the index is the live
        // verifications' count, [`finish_update_pass`].)
        if let Some(retry) = net.resolve_provenance.as_ref() {
            index_answered |= retry.answered;
        }
        if let Some(build) = net.resolved_index_build {
            RESOLVED_PASS_INDEX_BUILD.with(|seen| seen.set(seen.get().max(build)));
        }
        index_resolved = settle_net_lane_index(layout, index_resolved, net.index_error.as_deref());
        let announced = net.announced;
        // The net lane's resolutions join the apply lane's for the same sparing closure.
        resolved_assets.extend(net.resolved_assets);
        if !arrived.is_empty() {
            arrived.sort();
            emit_marker_line(&format!(
                "atpkg: {NET_INSTALLED_MARKER}{}",
                arrived.join(", ")
            ));
            println!("{SEED_FOLLOW_ON}");
            // What the programs that just arrived build with: the window files the missing
            // Command Line Tools as an exceptions entry, a fact for later (§4.2 `base.clt`).
            say_c_toolchain(
                seams.path.as_deref(),
                &crate::prereq::CcHost::this_machine(),
            );
            c_toolchain_said = true;
        } else if announced {
            // Always answer the announcement: a pass that said "installing over the
            // network" and installed nothing must say so — a failure, unless everything it
            // announced was a vendor landing waiting on its signature check.
            if let Some(line) = fresh_landing_terminal(&[], &deferred, net.announced_count) {
                emit_marker_line(&line);
            }
        }
        // The vendors whose reads were still out when the set announced, once it is done.
        if !late.is_empty() {
            let failed_before = vendor.failures;
            land_late_vendors(
                layout,
                fetcher,
                consent,
                reads,
                &late,
                pass_index.as_ref(),
                &mut vendor,
                seams,
            );
            warm_landed(layout, &mut vendor, seams);
            failures += vendor.failures - failed_before;
            resolved_assets.extend(vendor.resolved_assets.clone());
        }
        // A CLEAN pass retires a legacy `*seed*` offer row ([`retire_seed_row`]).
        if net.failures == 0 {
            retire_seed_row(layout);
        }
    }
    // Reclaim superseded builds once after the whole apply, reporting abstentions (this
    // sweeps the whole prefix). The sparing form keeps a failed download's `.part` so the
    // next pass resumes; under the channel this pass verified, so the build kept below each
    // live one is the one `aterm pkg rollback` lands on, never a yanked one.
    let channel = pass_index
        .as_ref()
        .and_then(|index| index.channel_for(crate::config::CHANNEL, current_triple()));
    let report = crate::gc::run_keeping_pinned_partials(layout, channel.as_ref(), &|p| {
        resolved_assets.get(p).cloned()
    });
    print_gc_sweeps("update", &report);
    print_gc_abstentions("update", &report);
    // The rustup seam, once per pass, whichever lanes ran, or a trust flip leaves
    // `~/.rustup/toolchains/trust` presenting the build it moved off.
    (seams.rustup_seam)(layout);
    // The exec roots, over the store this pass leaves (after its gc, so a reclaimed build
    // gets no root): this heals an up-to-date trust machine, which the apply short-circuits
    // before any shim is laid. Best-effort; a root that cannot be laid is said on stderr.
    let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
    // Refresh the interactive-shell PATH hook at the CLI edge, best-effort.
    (seams.shell_hooks)(layout);
    // Off macOS only the full probe tells a `cc` without headers from one that builds, and
    // the seed reads none, so every pass over a toolset says the verdict: the window's
    // entry ends at the pass after a compiler is installed, not at the next program to
    // arrive. (On macOS the seed's read is the whole verdict, at every launch.)
    if !c_toolchain_said
        && !cfg!(target_os = "macos")
        && !crate::active_builds(layout).is_empty()
        && let Some(line) = c_toolchain_marker(&crate::prereq::probe_with(
            seams.path.as_deref(),
            &crate::prereq::CcHost::this_machine(),
        ))
    {
        emit_marker_line(&line);
    }
    // Which managed agents are at their vendor's latest, for the window's ledger — a fact
    // about the vendor lane, said whatever the index lane's members did.
    print_managed_current(layout, &vendor.unchecked, (seams.now)());
    let now = (seams.now)();
    if index_resolved {
        record_index_freshness(layout, now, resolved_index_build);
    }
    // Offline when nothing answered and every member this pass lost was lost at a fetch; a
    // full disk or a refused stage is a failure, whatever the network did.
    let offline = pass_reached_nothing(index_answered, &vendor)
        && crate::flow::fetch_faults().wrapping_sub(faults_before) == failures;
    // The development links, re-read after the network work (a checkout may have been
    // rebuilt or removed meanwhile): a broken link is a fault of this pass, in its own row.
    let dev_linked = if failures == 0 {
        match update_link_inventory(layout) {
            Ok(linked) => linked,
            Err(()) => {
                let reached = crate::flow::live_verifications() > live_before;
                return finish_update_pass(layout, now, reached, offline, 1);
            }
        }
    } else {
        std::collections::BTreeSet::new()
    };
    if failures == 0 && update_toolset_is_empty(&crate::active_builds(layout), &dev_linked) {
        // A pass that installed nothing on an empty store is not a success: every group
        // clean-skips when the index publishes no artifact for this triple. Say so durably,
        // on stderr with exit 2, as `cmd_install_default_set` does (the GUI maps 2 to "not
        // a temporary error"; exit 2 only for a pass that reached the index). The cause is
        // a translated process, an unserved triple, a revoked channel, an exclude or
        // uninstalls: ask `running_translated`, then `index_serves`.
        if (seams.translated)() {
            let said = format!(
                "nothing is installed — this process is running under \
                 Rosetta translation, so {} is not this Mac's real architecture. \
                 Relaunch aterm natively and the toolset installs.",
                current_triple()
            );
            eprintln!("atpkg: {said}");
            // Its own row, over whatever an earlier pass left: the window judges an exit 2
            // by this row.
            record_unbuilt(layout, "*toolset*", crate::state::TOOLSET_TRANSLATED, said);
            let reached = crate::flow::live_verifications() > live_before;
            return finish_update_pass(layout, now, reached, offline, 2);
        }
        let serves_us = match reusable_index(pass_index.as_ref(), now) {
            // The apply lane verified an index this very pass; asking the cache and
            // the signatures again for one diagnostic sentence is work, not caution.
            Some(index) => Some(index_serves(layout, fetcher, index, cfg)),
            None => {
                PASS_INDEX_RESOLVE_ATTEMPTED.with(|seen| seen.set(true));
                let found = crate::resolve_verified_index(
                    fetcher,
                    layout,
                    &(seams.anchor)(layout),
                    build_floor(layout),
                    now,
                );
                if let Ok(index) = found.as_ref() {
                    RESOLVED_PASS_INDEX_BUILD
                        .with(|seen| seen.set(seen.get().max(index.index_build)));
                }
                found
                    .ok()
                    .map(|index| index_serves(layout, fetcher, &index, cfg))
            }
        };
        // `unusable`: the `seed-unusable:` body, for the two verdicts a typed seed's line
        // carries too ([`report_seedless_posture`]).
        let (said, state, unusable) = match &serves_us {
            Some(IndexServes::Yes) => {
                // The index publishes for this triple, so something else refused every
                // member (a revocation, a floor, a filter): not the architecture.
                (
                    format!(
                        "nothing is installed and nothing was installable — ALab \
                         publishes builds for {} but every one was refused (revoked, \
                         below the floor, or filtered out); see `aterm pkg doctor`",
                        current_triple()
                    ),
                    "unavailable: every published build was refused",
                    None,
                )
            }
            Some(IndexServes::Unserved(served)) => {
                let why = unserved_sentence(current_triple(), served);
                (
                    format!("{NOTHING_INSTALLABLE_LEAD}{why}"),
                    crate::state::TOOLSET_UNSERVED,
                    Some(why),
                )
            }
            // The index serves this triple; the user's exclusion or uninstalls are
            // what left nothing — said as that, never as the architecture.
            Some(IndexServes::Narrowed(narrowing)) => {
                let (why, state) = narrowing.verdict(current_triple());
                (format!("{NOTHING_INSTALLABLE_LEAD}{why}"), state, Some(why))
            }
            // `None`: the index never resolved (offline, a rate limit, a proxy), never a
            // machine that cannot be served.
            None => (
                "nothing is installed — the signed index could not be reached, so \
                 nothing could be checked or fetched. When the connection is back, \
                 re-run: aterm pkg install --default-set"
                    .to_string(),
                crate::state::TOOLSET_INDEX_UNREACHABLE,
                None,
            ),
        };
        eprintln!("atpkg: {said}");
        // The window's card, from this pass: the window's seed resolves no index, so this
        // arm prints the line a typed seed prints (an unserved architecture, a toolset
        // narrowed to nothing) for the reader that parses every pass's stdout. A person's
        // terminal has the sentence above already.
        if let Some(why) = unusable
            && !stdout_is_a_person()
        {
            emit_marker_line(&seed_unusable_line(&why));
        }
        record_unbuilt(layout, "*toolset*", state, said);
        // Reached (an index the channel served, this diagnosis's resolve included), the
        // verdict is final: exit 2. Not reached (a first launch before the network came up,
        // a refusal), it is retried.
        let reached = crate::flow::live_verifications() > live_before;
        return finish_update_pass(layout, now, reached, offline, 2);
    }
    let reached = crate::flow::live_verifications() > live_before;
    let code = u8::from(failures > 0);
    // A dev-linked toolset the pass kept is said whatever else it did: a dev-link-only
    // store asks no index, and its pass still ends in a sentence.
    if !dev_linked.is_empty() {
        let message = format!(
            "{} dev-linked program(s) kept, not updated",
            dev_linked.len()
        );
        println!("atpkg: {message}");
        record_outcome(layout, message);
    }
    if !index_asked {
        return code;
    }
    if failures == 0 {
        // The toolset is present: retire any "unavailable" verdict an earlier pass recorded.
        clear_status_row(layout, "*toolset*");
    }
    // A pass that resolved the index and ran to its end with member failures (each in its
    // own row) is a check that ran, stamped as one when the index was the channel's. One on
    // the cached index, or with none, stamps a failure and exits non-zero whatever its
    // members did, so a listing that refused never hides behind exit 0.
    finish_update_pass(layout, now, reached, offline, code)
}

/// What a default-set pass did, for the caller's marker bookkeeping: how many members
/// failed, and whether the `net-starting:` announcement was printed (it demands a terminal
/// answer, `net-installed:` or `net-failed:`).
struct DefaultSetOutcome {
    failures: u32,
    announced: bool,
    /// How many programs the announcement named, the vendors that rode it included.
    announced_count: usize,
    /// The network verdict of this lane's own index resolve, when it could not reuse the
    /// caller's index; `update` uses it after a failed first resolve.
    resolve_provenance: Option<crate::flow::ResolveProvenance>,
    /// `Some(reason)` when the pass never reached a signed index it could act on, so it
    /// checked nothing: on an empty store this lane is the whole `update` pass, which must
    /// then never be stamped as a completed check.
    index_error: Option<String>,
    /// The signed build this lane used, a successful retry included: the full-pass end
    /// records the highest, so a queued pass for that same build does not run again.
    resolved_index_build: Option<u64>,
    /// `program → pinned asset name` for every member resolved far enough to select an
    /// artifact, a failed download included: the caller's pass-end gc spares its `.part`.
    resolved_assets: std::collections::BTreeMap<String, String>,
    /// Members refused because their coherence tuple was narrowed (`exclude`, or a member
    /// removed on purpose), sorted: absent from the announcement and the plan, and from any
    /// "default set complete" claim.
    skipped_narrowed: Vec<String>,
    /// `wanted ∧ absent` at pass end — what a clean pass proved still awaits install
    /// (failed members included; narrowed/unserved not). The legacy `*seed*` row reconciles
    /// against it.
    remaining_missing: Vec<String>,
}

/// The progress-file pass name every default-set pass runs under (the GUI reads it back).
const NET_PASS: &str = "net";

/// Who consented to a default-set pass, named by the caller: the wire lane's announcement
/// states the consent and the switches that stop the pass, which differ per caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Consent {
    /// Nobody asked: installing aterm is the consent (`[packages].auto_install`, default
    /// true), and the line names `auto_install = false` as the switch that would stop it.
    InstallingAterm,
    /// No consent claimed beyond the one every pass carries (the line names the uninstall
    /// alone): the explicit verb's pass, and every `update` pass on a machine a person
    /// adopted with it ([`set_completion_consent`]).
    Explicit,
}

impl Consent {
    /// The clauses around [`NET_OFF_SWITCH`]: the consent ahead of it and the switch that
    /// would have prevented the pass after it, each only where true. Parenthesis-free.
    fn announced(self) -> (&'static str, &'static str) {
        match self {
            Consent::InstallingAterm => (
                "installing aterm is the consent for this; ",
                ", or with `[packages].auto_install = false` in aterm.toml",
            ),
            Consent::Explicit => ("", ""),
        }
    }
}

/// The consent `cmd_update_all`'s set-completion pass states: "installing aterm is the
/// consent" wherever the seed pass adopted the machine; [`Consent::Explicit`] where a person
/// adopted it with the explicit verb ([`adopted`] without [`adopted_by_seed`]).
fn set_completion_consent(adopted: bool, adopted_by_seed: bool) -> Consent {
    if adopted && !adopted_by_seed {
        Consent::Explicit
    } else {
        Consent::InstallingAterm
    }
}

/// The off switch every wire-lane announcement names. Free of parentheses: the GUI's
/// toolchain bar reads the sizes from the line's trailing `(…)`. The timing is stated
/// because `uninstall` needs the store lock this pass holds.
const NET_OFF_SWITCH: &str = "opt out once this pass ends with `aterm pkg uninstall --all`, which \
                              removes the set and records the decline";

/// The wire lane's announcement, if the pass owes one (only a non-empty pass speaks): the
/// signed sizes ([`seed_install_bytes`], [`size_parenthetical`]) and the off switch
/// ([`NET_OFF_SWITCH`]), with the consent clauses where `consent` makes them true
/// ([`Consent::announced`]).
fn net_announcement(
    consent: Consent,
    will_install: &[String],
    sizes: SignedSizes,
) -> Option<String> {
    if will_install.is_empty() {
        return None;
    }
    let (consent_clause, seed_clause) = consent.announced();
    Some(format!(
        "atpkg: {NET_STARTING_MARKER}installing {} program(s) over the network: {} — \
         {consent_clause}{NET_OFF_SWITCH}{seed_clause}{}",
        will_install.len(),
        will_install.join(", "),
        size_parenthetical(sizes)
    ))
}

/// The §11 default-set pass, over an INJECTED `PATH` — every caller hands in its own
/// (the update pass and the explicit toolset verb, through their [`PassSeams`]), so a pass
/// is testable without reading the developer's environment.
///
/// `index` is the caller's already-verified index when its pass resolved one: `atpkg
/// update` hands in the index its apply lane trusted, so this lane does not repeat that
/// whole verify-select ([`reusable_index`]). `None` resolves one here.
///
/// `flip` is the caller's flip gate, the one its apply lane ran under: a tuple completed
/// here can move a live `trust` (a sibling the index added, one whose shims are gone), and
/// under the unattended gate that move waits for quiet like any other. `held_flip` names the
/// members of a tuple whose flip the caller's apply lane already held: that tuple is left
/// to the held flip, which installs its absent members with it. `companions` is what the
/// pass hands it besides: the vendor landings that ride it, and what follows the toolchain's
/// flip ([`SetCompanions`]).
///
/// The injected `PATH` is not a nicety. A pass answers two questions against `PATH` — "is a
/// foreign copy of this tool ahead of ours" (`reconcile_shadowed`) and "is this
/// program already satisfied by a system install"
/// (`reconcile_system_satisfied_with`) — and read from the process environment, a
/// test's temp prefix would be judged against the REAL machine's toolchain.
#[allow(
    clippy::too_many_arguments,
    reason = "the default-set pass's whole verified context, the caller's already-resolved \
              index, the injected `PATH` the shadow and system-satisfied probes read, the \
              caller's flip gate with what it holds, and what the pass hands it besides"
)]
fn install_default_set_with_path(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    anchor: &crate::Anchor,
    cfg: &crate::config::PackagesConfig,
    consent: Consent,
    now: i64,
    index: Option<&crate::TrustedIndex>,
    path_var: Option<&std::ffi::OsStr>,
    flip: &crate::quiet::FlipGate<'_>,
    held_flip: &std::collections::BTreeSet<String>,
    companions: SetCompanions<'_, '_>,
) -> DefaultSetOutcome {
    // Live-progress pass ownership: with a progress file (the GUI's, or a typed pass's) and
    // no pass live yet, this pass is the "net" pass — begun here so its start truncates the
    // file, ended here so the terminal snapshot is written on every exit path. When a pass
    // is already live, `begin_pass` declines and this one runs under it. A vendor lane
    // riding this pass meters on it too ([`crate::progress::lend`]).
    let owned_pass = pass_progress_path().is_some_and(|p| crate::progress::begin_pass(p, NET_PASS));
    let out = install_default_set_inner(
        layout, fetcher, anchor, cfg, consent, now, index, path_var, flip, held_flip, companions,
    );
    if owned_pass {
        crate::progress::end_pass();
    }
    out
}

/// What a pass's set completion carries besides its own members
/// ([`install_default_set_with_path`]).
struct SetCompanions<'c, 'r> {
    /// The vendor lane's fresh landings, announced and landed beside the set's
    /// ([`VendorRider`]).
    vendor: Option<&'c mut VendorRider<'r>>,
    /// Run the moment a tuple holding the Trust toolchain flips: the pass's rustup view
    /// and exec roots, which the pass end re-lays as well.
    toolchain_flipped: &'c dyn Fn(&crate::store::Layout),
    /// Vendors land alone after this set ([`FreshVendors::late`]): its plan keeps the
    /// promise for theirs.
    vendors_after: bool,
}

#[cfg(all(test, unix))]
impl SetCompanions<'_, '_> {
    /// A set completion with nothing beside it.
    fn alone() -> Self {
        Self {
            vendor: None,
            toolchain_flipped: &|_| {},
            vendors_after: false,
        }
    }
}

/// The commands of a program's verified manifest (`exposes`) a pending stub may stand for
/// before it lands ([`crate::stub::reconcile`]'s `exposed`): never a name `index` names as a
/// program of its own — that program's own stub answers for it, and `install <name>` must
/// install it rather than point at the program that also ships the name — and never an
/// upstream name the reroute table answers (`z3`, `rustfmt`: [`crate::reroute::row_for`]).
fn stub_commands_of(index: &crate::TrustedIndex, exposes: &[String]) -> Vec<String> {
    exposes
        .iter()
        .filter(|name| !index.is_program(name) && crate::reroute::row_for(name).is_none())
        .cloned()
        .collect()
}

/// [`install_default_set_with_path`]'s body, split so pass ownership above cannot leak on any
/// of the early returns below.
#[allow(
    clippy::too_many_arguments,
    reason = "[`install_default_set_with_path`]'s inputs, split only so pass \
              ownership cannot leak on an early return"
)]
fn install_default_set_inner(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    anchor: &crate::Anchor,
    cfg: &crate::config::PackagesConfig,
    consent: Consent,
    now: i64,
    index: Option<&crate::TrustedIndex>,
    path_var: Option<&std::ffi::OsStr>,
    flip: &crate::quiet::FlipGate<'_>,
    held_flip: &std::collections::BTreeSet<String>,
    companions: SetCompanions<'_, '_>,
) -> DefaultSetOutcome {
    let SetCompanions {
        vendor: rider,
        toolchain_flipped,
        vendors_after,
    } = companions;
    let floor = build_floor(layout);
    // The caller's index when it has one (`atpkg update`'s apply lane), else one resolved
    // here (`install --default-set`); [`reusable_index`] still re-checks freshness.
    let resolved;
    let mut resolve_provenance = None;
    let index = match reusable_index(index, now) {
        Some(index) => index,
        None => {
            let attempt = crate::resolve_verified_index(fetcher, layout, anchor, floor, now);
            resolve_provenance = crate::flow::last_resolve();
            resolved = match attempt {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("atpkg: default-set bootstrap: cannot resolve the signed index: {e}");
                    print_unreachable_followup(&e, "aterm pkg install --default-set");
                    return DefaultSetOutcome {
                        failures: 1,
                        announced: false,
                        announced_count: 0,
                        resolve_provenance,
                        index_error: Some(e.to_string()),
                        resolved_index_build: None,
                        resolved_assets: std::collections::BTreeMap::new(),
                        skipped_narrowed: Vec::new(),
                        remaining_missing: Vec::new(),
                    };
                }
            };
            &resolved
        }
    };
    // The index is verified (here or by the caller): advance the anti-rollback floor to
    // it now, as `cmd_update` does after its resolve, not only where an index-served
    // program installs. On a target the index serves only vendor-direct agents (aarch64
    // Linux, index build 71) no index program installs, and the floor left at 0 had the
    // doctor call the cached index "downloaded but not accepted". Idempotent for a caller
    // that already advanced it.
    advance_floors(layout, index.index_build, index.roster_seq());
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
    // The channel, looked up once on the one resolved index: every group decision below
    // reads the same signed state, so a mid-pass publish cannot split a tuple.
    let Some(ch) = index.channel_for(crate::config::CHANNEL, current_triple()) else {
        let e = crate::FlowError::NoChannel(crate::config::CHANNEL.to_string());
        eprintln!("atpkg: default-set bootstrap failed: {e}");
        // Nothing of the set is coming (no channel for this machine): this pass plans
        // nothing, and the promise goes with that plan, so a stub waiting on it answers now
        // rather than when the update exits. Not when vendors this pass would have carried
        // land alone next: their plan takes the promise over ([`land_fresh_alone`]), and a
        // stub waiting for one of them must not read the promise gone before it.
        if rider.as_ref().is_none_or(|r| r.fresh.pending.is_empty())
            && !vendors_after
            && let Some(sink) = crate::progress::active()
        {
            sink.plan_promised_pass(layout, &[]);
        }
        return DefaultSetOutcome {
            failures: 1,
            announced: false,
            announced_count: 0,
            resolve_provenance,
            index_error: Some(e.to_string()),
            resolved_index_build: Some(index.index_build),
            resolved_assets: std::collections::BTreeMap::new(),
            skipped_narrowed: Vec::new(),
            remaining_missing: Vec::new(),
        };
    };
    let ch = &ch;
    let groups = crate::apply::plan_groups_here(layout, index, ch, current_triple());
    let mut installed = crate::active_builds(layout);
    let mut wanted = wanted_programs(index, cfg);
    // The vendor programs are wanted from the compiled table whatever the index names:
    // their stubs and rows stay here, and their own lane installs them. One whose vendor
    // serves this machine nothing is not: its lane recorded its `unavailable on` row, and
    // no stub may promise it.
    for spec in crate::vendor_direct::VENDORS {
        if spec.serves(current_triple())
            && vendor_wanted(cfg, &std::collections::BTreeSet::new(), spec.program)
        {
            wanted.insert(spec.program.to_string());
        }
    }
    // Programs the user removed on purpose are absent by request, not missing: this is the
    // pass the loop runs, so set-completion keeps the set the user has.
    for p in removed_programs(layout) {
        wanted.remove(&p);
    }
    // System satisfaction, every pass: a member declaring `system = "<bin>"` whose binary
    // is on PATH outside the managed prefix is satisfied — no download, no shim, a
    // `satisfied by system` row — and a managed copy is retired. If the system install
    // later goes, the name stays wanted-and-missing and installs like any other member.
    for p in reconcile_system_satisfied_with(layout, index, &wanted, &mut installed, path_var) {
        wanted.remove(&p);
    }
    // Serve this triple or say nothing: `wanted` so far is what the signed index names,
    // which may include programs this triple never gets. Filtered before the stub reconcile
    // and the announcement (as `seed_serviceable` does), so unserved programs neither
    // announce, count as failures, nor leave stubs. The probe proves missing-ness from a
    // verified manifest and defers on any fetch failure (`group_missing_triple`), so an
    // offline pass drops nothing.
    let triple = current_triple();
    // The planned members that are dev-linked, each marker read once: links are reconciled
    // before the pass, under the store lock, and nothing below changes them.
    let linked_members: std::collections::BTreeSet<&str> = groups
        .iter()
        .flat_map(|group| group.members.iter())
        .filter(|m| crate::linkmode::is_linked(layout, m))
        .map(String::as_str)
        .collect();
    let mut unserved: Vec<String> = Vec::new();
    let prescan = prescan_units(&groups, &wanted, &installed, &linked_members);
    // Every manifest the prescan reads — and the size sums and the disk check after it —
    // fetched AT ONCE: one at a time they were twenty GETs, four seconds before the
    // announcement. Bytes only: each read below still verifies what it is handed. A first
    // run's update pass asked for them beside its vendor plan ([`prefetch_set_manifests`]):
    // this finds them held and asks the network for none.
    crate::flow::prefetch_pinned(
        fetcher,
        index,
        ch,
        prescan
            .iter()
            .flat_map(|(group, missing)| prescan_probe(group, missing))
            .map(String::as_str),
    );
    // What each absent member's verified manifest exposes, read by the same probe: the
    // pending stubs below answer for every command a program brings, not only the branded
    // names the compiled table knows (`targo-tippy`, `clean-lsp` beside `targo`).
    let mut exposed: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (group, missing) in prescan {
        let probe = prescan_probe(group, &missing);
        if crate::flow::group_missing_triple_reading(
            fetcher,
            index,
            crate::config::CHANNEL,
            triple,
            probe,
            &mut |member, pkg| {
                if missing.iter().any(|m| m == member) {
                    exposed.insert(member.to_string(), stub_commands_of(index, &pkg.exposes));
                }
            },
        )
        .is_some()
        {
            unserved.extend(missing);
        }
    }
    if !unserved.is_empty() {
        unserved.sort();
        // Every unserved member gets its own canonical row — `unavailable on <target>:
        // <hint>`, with the program's `unavailable_hint` when the index carries one — in
        // the words the pass log prints. A normal state, never a fault to `doctor`.
        let recorded = crate::status::read(layout);
        for p in &unserved {
            wanted.remove(p.as_str());
            // A program this machine has disabled keeps the row that says so (an `aborted:
            // stage` from `disable_revoked_currents`): repainting it `unavailable on <target>`
            // would hide the fault from doctor. Still not announced, counted or stubbed.
            if program_disabled_here(layout, recorded.as_ref(), p) {
                println!(
                    "atpkg: {p}: no build for this machine ({triple}) — and its current \
                     build is disabled here (see `aterm pkg doctor`)"
                );
                continue;
            }
            let hint = index
                .program(p)
                .and_then(|prog| prog.unavailable_hint.clone())
                .unwrap_or_default();
            let state = crate::state::unavailable(triple, &hint);
            println!("atpkg: {p}: {state}");
            record_unbuilt(
                layout,
                p,
                state,
                format!("unserved for {triple}: {}", unserved.join(", ")),
            );
        }
        println!(
            "atpkg: {} program(s) have no build for this machine ({triple}): {} — skipped, \
             not failed; they install automatically once builds publish",
            unserved.len(),
            unserved.join(", ")
        );
        // The toolset-wide blocked verdict is reserved for a machine the index serves
        // nothing on (the pending stubs and doctor read it as "nothing is coming"); a
        // partial matrix (gh and emacs absent on Linux) must not raise it. `wanted` now
        // holds what this target can carry, so the machine is blocked only when it is empty.
        if wanted.is_empty() {
            record_unbuilt(
                layout,
                "*toolset*",
                crate::state::TOOLSET_UNSERVED_BLOCKED,
                format!("unserved for {triple}: {}", unserved.join(", ")),
            );
        }
    }
    let mut failures = 0u32;
    // THE AGENT PROGRAMS' ROWS (owner decision): a wanted agent program that
    // is not installed yet reads `agent program — installing`. A vendor program's refusal or
    // tombstone is its own lane's row (`record_vendor_status`, which ran first this pass)
    // and stands: painting it `installing` would hide the fault from doctor and Settings
    // and rewrite the row twice every pass.
    let recorded = crate::status::read(layout);
    record_states_if_changed(
        layout,
        wanted
            .iter()
            .filter(|n| crate::stub::is_agent_program(n) && !installed.contains_key(n.as_str()))
            .filter(|name| {
                let lane_fault = crate::vendor_direct::is_vendor(name)
                    && recorded
                        .as_ref()
                        .and_then(|r| r.programs.get(name.as_str()))
                        .is_some_and(|r| {
                            r.state.starts_with("error:") || r.state.starts_with("tombstoned:")
                        });
                !lane_fault
            })
            .map(|name| (name.as_str(), crate::state::agent_installing())),
    );
    // A coherence tuple with a member NARROWED OUT — a `[packages]` include/exclude,
    // or a member removed on purpose — cannot install whole, and the group arm
    // below refuses it. Deciding that only AT the arm would let one pass announce,
    // stub and claim complete a tuple it then refuses. Decide it HERE, before the stubs and the
    // announcement: name the true per-member cause once, then narrow the refused
    // members out of `wanted` so no stub, announcement, plan entry, or
    // completion claim ever speaks for them. The arm's own check stays as the
    // transaction's safety net; this prescan is what keeps it unreachable.
    let removed_markers = removed_programs(layout);
    let mut skipped_narrowed: Vec<String> = Vec::new();
    for group in &groups {
        // A tuple only: an ungrouped member installs on its own.
        if group.group.is_none() {
            continue;
        }
        let missing = absent_members(group, &wanted, &installed);
        if missing.is_empty() {
            continue;
        }
        let narrowed_out: Vec<String> = group
            .members
            .iter()
            .filter(|m| !wanted.contains(m.as_str()) && !installed.contains_key(m.as_str()))
            .map(|m| {
                if removed_markers.contains(m.as_str()) {
                    format!("{m} (removed on this machine — `aterm pkg install --default-set` restores it)")
                } else {
                    format!("{m} ([packages] exclude)")
                }
            })
            .collect();
        if narrowed_out.is_empty() {
            continue;
        }
        eprintln!(
            "atpkg: {} not installed \u{2014} they install together with {}",
            missing.join(", "),
            narrowed_out.join(", ")
        );
        skipped_narrowed.extend(missing);
    }
    for p in &skipped_narrowed {
        wanted.remove(p.as_str());
    }
    skipped_narrowed.sort();
    // Pending-stub reconcile at the index resolve (R6): the SIGNED set replaces the
    // compiled roster the adoption-time stubs were laid from — newly listed names
    // gain stubs (PATH coverage before their bytes move), de-listed/removed/
    // installed names lose theirs, and every kept stub is rewritten so its embedded
    // atpkg path survives app relocation/self-update. `wanted` is already
    // triple-filtered above, so a stub is only ever laid for a program whose
    // bytes CAN move on this machine — with one for each command its manifest exposes.
    crate::stub::reconcile(layout, &wanted, &installed, &exposed);
    // ANNOUNCE BEFORE ACTING (the block comment at the caller had argued for it
    // while nothing announced). The set named is exactly what the loop below will
    // attempt: wanted ∧ absent, minus dev-linked groups (announcing a member the loop
    // then skips for a dev link would be a lie). Printed only when non-empty, so the
    // ordinary no-op tick stays silent.
    //
    // A tuple whose flip this pass's apply lane HELD for quiet is not completed here: its
    // absent members stay wanted (their stubs stand, and the apply lane's rows say they
    // wait) and arrive with the held flip, whose transaction pulls them in.
    let flip_held = |group: &crate::Group| group.members.iter().any(|m| held_flip.contains(m));
    let mut will_install: Vec<String> = Vec::new();
    // The same set as install units — each group as its lane is handed it, with the members
    // it adds — for the up-front disk check below.
    let mut planned_units: Vec<(&crate::Group, Vec<String>)> = Vec::new();
    for group in &groups {
        if flip_held(group)
            || group
                .members
                .iter()
                .any(|m| linked_members.contains(m.as_str()))
        {
            continue;
        }
        let missing = absent_members(group, &wanted, &installed);
        if !missing.is_empty() {
            will_install.extend(missing.iter().cloned());
            planned_units.push((group, missing));
        }
    }
    will_install.sort();
    // The signed sizes, derived only when the wire lane will speak; the fetcher memoises
    // manifests the triple probe already pulled.
    let sizes = if will_install.is_empty() {
        SignedSizes::UNKNOWN
    } else {
        seed_install_bytes(fetcher, index, &will_install)
    };
    // The set, refused up front when this volume cannot hold what it adds up to, rather
    // than installing a few members and failing the rest. Computed from the bytes the
    // per-program gates check, per install unit, leaving out units this pass lands nothing
    // of ([`set_units`]). An unknown size or a failed free-space query proceeds; the
    // per-program gates stay the backstop.
    //
    // Fresh vendor landings riding this pass land at the same time, so the set must fit
    // beside everything they need at once ([`FreshVendors::peak`]); a set that does not is
    // refused as it would be once they had landed, and they land alone.
    let rider = rider.filter(|r| !r.fresh.pending.is_empty());
    // The set's units and the volume's free space, when both are known: the refusal below
    // reads them, and so does the download-ahead lane's room further down.
    let set_room = if planned_units.is_empty() {
        None
    } else {
        set_units(&planned_units, |group| {
            crate::flow::planned_landing(fetcher, layout, index, ch, group, &installed, triple)
        })
        .zip(crate::freespace::available_bytes(&layout.prefix))
    };
    let disk_refusal = set_room.as_ref().and_then(|(units, have)| {
        set_disk_refusal(
            units,
            rider.as_ref().map_or(0, |r| r.fresh.peak()),
            *have,
            &layout.prefix,
        )
    });
    if let Some(refusal) = &disk_refusal {
        eprintln!("{}", refusal.line);
        refusal.record(layout);
        failures += 1;
    }
    // One announcement for both lanes: the vendors first, then the set, with their signed
    // sizes summed. A second `net-starting:` would supersede the first's card while its
    // landings were still moving bytes. Vendors a refused set leaves behind land alone after
    // this pass, as do the ones whose reads were still out ([`FreshVendors::late`]), and the
    // promise goes with their plan, not this one's ([`land_fresh_alone`]).
    let left_behind = (rider.is_some() && disk_refusal.is_some()) || vendors_after;
    let rider = rider.filter(|_| disk_refusal.is_none());
    let mut announced_names = rider.as_ref().map_or_else(Vec::new, |r| r.fresh.names());
    announced_names.extend(will_install.iter().cloned());
    let sizes = match (&rider, will_install.is_empty()) {
        (Some(r), true) => r.fresh.sizes(),
        (Some(r), false) => sizes.and(r.fresh.sizes()),
        (None, _) => sizes,
    };
    let announcement = if disk_refusal.is_some() {
        None
    } else {
        net_announcement(consent, &announced_names, sizes)
    };
    let announced = announcement.is_some();
    let announced_count = if announced { announced_names.len() } else { 0 };
    if let Some(line) = &announcement {
        emit_marker_line(line);
    }
    // The pass's plan, in plan order, with each group's freshly-installable members —
    // materialized once so the priority queue below only permutes it (reorder-only).
    let plan: Vec<(usize, Vec<String>)> = groups
        .iter()
        .enumerate()
        .filter(|(_, group)| disk_refusal.is_none() && !flip_held(group))
        .filter_map(|(i, group)| {
            let missing = absent_members(group, &wanted, &installed);
            (!missing.is_empty()).then_some((i, missing))
        })
        .collect();
    // Every planned member: a bump line naming anything else is ignored, so the bump file
    // can only permute installs the signed index authorized.
    let plannable: std::collections::BTreeSet<String> = plan
        .iter()
        .flat_map(|(_, missing)| missing.iter().cloned())
        .collect();
    let vendor_plan = rider.as_ref().map_or_else(Vec::new, |r| r.fresh.planned());
    let vendor_names: Vec<String> = vendor_plan.iter().map(|(name, _)| name.clone()).collect();
    let shared = rider.as_ref().and_then(|_| fetcher.shared());
    // The live-progress plan: per-program signed sizes, fetched only when a sink is live,
    // fix the overall bar's denominator before any byte moves. A miss leaves that row
    // unmetered, never fails the pass.
    //
    // In the order the bytes move: the queue is what a pending stub reads its place in
    // ("next in line, after …") and what the window names when nothing is mid-flight. A
    // vendor riding this pass on a thread of its own lands once every archive of the set is
    // in (the coherence tuples' among them) or once no tuple is left ([`RiderGate`]), beside
    // the rest of the set's landing; one with no thread of its own lands after every set
    // member. Its rows stand after the last tuple, or last, never ahead of the toolchain
    // they wait for ([`set_progress_queue`]).
    if let Some(sink) = crate::progress::active() {
        let vendor_sizes: std::collections::BTreeMap<_, _> = vendor_plan.into_iter().collect();
        let planned: Vec<_> = set_progress_queue(&plan, &groups, &vendor_names, shared.is_some())
            .into_iter()
            .map(|name| {
                let size = vendor_sizes.get(&name).copied().unwrap_or_else(|| {
                    crate::flow::planned_artifact_size(fetcher, index, ch, &name, current_triple())
                        .unwrap_or(0)
                });
                (name, size)
            })
            .collect();
        // The promised plan has landed: a stub reads this pass's rows from here on, and a
        // name the plan leaves out is no longer coming ([`pending_state`]). Retired only now,
        // with the pass live and planned, so a stub never reads the promise gone before the
        // pass that kept it ([`wait_then_run`]) — and not by a plan whose vendors land alone
        // after it, which would say "left out" to a stub waiting for one of them.
        if left_behind {
            sink.plan(&planned);
        } else {
            sink.plan_promised_pass(layout, &planned);
        }
    }
    // Download ahead ([`crate::flow::ahead`]): the plan's archives are fetched on a thread
    // of their own while this one digests, extracts and flips the unit in hand, so the link
    // does not sit idle. Bytes only, in the order this loop will want them; every stage,
    // flip and row stays here. Only with a fetcher a pass may share, and only on a volume
    // known to hold the set's peak plus what the lane may fetch ahead
    // ([`crate::flow::ahead::Ahead::reserve`]); otherwise the set lands one download at a
    // time.
    //
    // Over the units the disk check sized, and only when it sized them all ([`set_units`]):
    // their signed manifests are in hand, so planning the lane asks the network nothing (a
    // manifest the prescan could not fetch would cost a connect timeout apiece on a dead
    // link before the first unit starts), and a unit this pass lands nothing of (a held
    // tuple, a refusal memo, a build not published here) moves no byte ahead.
    let ahead = fetcher
        .shared()
        .zip(set_room.as_ref())
        .and_then(|(_, (sized, have))| {
            let units: Vec<(usize, Vec<String>)> = plan
                .iter()
                .filter(|(_, missing)| sized.iter().any(|unit| unit.members == *missing))
                .cloned()
                .collect();
            let lane = crate::flow::ahead::Ahead::plan(fetcher, layout, index, ch, triple, &units)?;
            crate::cost::disk_ok(
                crate::flow::peak_need(sized.iter().map(|u| u.bytes))
                    .saturating_add(rider.as_ref().map_or(0, |r| r.fresh.peak()))
                    .saturating_add(lane.reserve()),
                *have,
                crate::cost::FREE_FLOOR,
            )
            .then_some(lane)
        });
    // `program → pinned asset name` for every member either arm resolves, a failed
    // download included, so the caller's pass-end gc can spare its `.part`.
    let mut resolved_assets: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    // Every channel-pinned program some group covers (grouped tuple or singleton);
    // wanted members left over are unpinned and fail loudly below.
    let mut pinned_members: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for group in &groups {
        pinned_members.extend(group.members.iter().cloned());
    }
    // The priority queue: between items, re-read the bump file and stably re-sort the
    // remainder — bumped items first, in first-mention order, plan order as the tiebreak.
    // The current item always finishes first (in-flight bytes are never abandoned), and
    // bumping a member bumps its whole group. The bump file is read-only here and deleted
    // only at a clean pass end.
    let mut remaining: Vec<(usize, Vec<String>)> = plan;
    // The vendors land beside the loop below, on lane threads of their own when the fetcher
    // may be shared ([`RIDER_LANES`] at once, inside this pass's store lock, metered on its
    // progress pass, writing no status row: their outcomes come back to this thread), else
    // after it on this one. Either way the set's own bytes go first: the toolchain's archives
    // take the whole link, and agents landing beside the tuple's downloads or extract delay
    // trust's flip and the last ALab program. The download-ahead lane opens the gate the
    // moment the last archive of every unit of the set is in (the link is then idle while
    // the pass extracts, flips and stages the rest, and the agents take it); this thread
    // opens it once no tuple is left to land, and when the loop ends or unwinds. An agent a
    // person asked for by name at a terminal does not wait for the gate; one a script asked
    // for goes first once it opens ([`RiderQueue::next`]).
    let gate = RiderGate::default();
    let queue = rider.as_deref().map(RiderQueue::of);
    // THE RIDERS' ROWS AS THEY LAND, not once the pass is over: a lane thread sends each
    // landing here, and this thread — the one that writes every row of the pass — reports it
    // between the set's units below, and as it comes while it waits for the lanes once its
    // own units are in. A row read once an agent's shim is live says it is installed, and a
    // pass killed after that leaves no row saying it is still to come.
    let mut landed = rider.as_deref().map(|_| RiderLanded::default());
    let rider_now = rider.as_deref().map_or(0, |r| r.fresh.now);
    let (landed_tx, landed_rx) = std::sync::mpsc::channel::<RiderLanding>();
    // EVERY UNIT THE SET LANDS, not its tuples alone: one the download-ahead lane holds no
    // archive of (an app bundle, a row of unknown or zero size, a unit the disk check could
    // not size) is fetched by this thread, which alone knows when it ends, so the lane never
    // opens the gate beside that download.
    let set_units: Vec<usize> = remaining.iter().map(|(gi, _)| *gi).collect();
    std::thread::scope(|scope| {
        let opens = RiderGateOpens(&gate);
        let mut beside = Vec::new();
        if let Some(r) = rider.as_deref()
            && let Some(queue) = &queue
            && let Some(shared) = shared
        {
            for _ in 0..r.fresh.pending.len().min(RIDER_LANES) {
                let lent = crate::progress::lend();
                let gate = &gate;
                let landed_tx = landed_tx.clone();
                if let Ok(handle) = std::thread::Builder::new()
                    .name("atpkg-vendor-lane".into())
                    .spawn_scoped(scope, move || {
                        let _adopted = lent.adopt();
                        let faults = crate::flow::fetch_faults();
                        let lane = r.fresh.lane(layout, shared, r.trust);
                        rider_land(&lane, r, queue, Some(gate), &mut |landing| {
                            // This thread's receiver goes only with the pass's unwinding.
                            let _ = landed_tx.send(landing);
                        });
                        crate::flow::fetch_faults().wrapping_sub(faults)
                    })
                {
                    beside.push(handle);
                }
            }
        }
        // The lanes hold the only senders now: the wait for their landings below ends when
        // the last lane does.
        drop(landed_tx);
        if shared.is_some()
            && beside.is_empty()
            && let Some(sink) = crate::progress::active()
        {
            // No vendor lane thread could start, so the vendors land on this thread: the
            // queue follows that fallback order before the first set member starts.
            sink.queue(&set_progress_queue(
                &remaining,
                &groups,
                &vendor_names,
                false,
            ));
        }
        // The download-ahead worker, and this thread entered into its lane: every member
        // fetch below asks the lane first. Halted when `entered` drops (after the loop, or
        // unwinding), so the scope's join waits at most for the archive in hand.
        let entered = ahead.as_ref().and_then(|lane| {
            let shared = fetcher.shared()?;
            let lent = crate::progress::lend();
            let worker = std::sync::Arc::clone(lane);
            let (gate, set_units) = (&gate, &set_units);
            std::thread::Builder::new()
                .name("atpkg-download-ahead".into())
                .spawn_scoped(scope, move || {
                    let _adopted = lent.adopt();
                    worker.work(shared, layout, &|lane| {
                        if lane.all_fetched(set_units) {
                            gate.open();
                        }
                    });
                })
                .ok()?;
            Some(lane.enter())
        });
        while !remaining.is_empty() {
            if let Some(landed) = landed.as_mut() {
                for landing in landed_rx.try_iter() {
                    landed.report(layout, landing, rider_now);
                }
            }
            if !remaining.iter().any(|(gi, _)| groups[*gi].group.is_some()) {
                gate.open();
            }
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
                    let order =
                        set_progress_queue(&remaining, &groups, &vendor_names, !beside.is_empty());
                    sink.queue(&order);
                }
            }
            let (gi, missing) = remaining.remove(0);
            let group = &groups[gi];
            // Dev-linked hard skip: a singleton skips itself; a coherence tuple with any
            // linked member skips whole (no partial install of a locked group over a dev
            // link), as in `apply_channel`.
            let linked: Vec<&str> = group
                .members
                .iter()
                .map(String::as_str)
                .filter(|m| linked_members.contains(m))
                .collect();
            if !linked.is_empty() {
                match &group.group {
                    Some(_) => println!(
                        "atpkg: {} not installed \u{2014} they install together, and {} {} \
                     dev-linked",
                        missing.join(", "),
                        linked.join(", "),
                        if linked.len() == 1 { "is" } else { "are" }
                    ),
                    None => println!("atpkg: {} dev-linked — skipped", group.members[0]),
                }
                for m in &missing {
                    note_finished(m, crate::progress::Phase::Skipped, None);
                }
                continue;
            }
            match &group.group {
                // A coherence tuple fresh-installs all-or-nothing against the one index
                // resolved above — see [`bootstrap_group`].
                Some(g) => {
                    failures += bootstrap_group(
                        layout,
                        fetcher,
                        index,
                        g,
                        group,
                        &wanted,
                        &installed,
                        &missing,
                        &mut resolved_assets,
                        flip,
                        toolchain_flipped,
                    );
                }
                // An ungrouped member can move alone — see [`bootstrap_singleton`].
                None => {
                    failures += bootstrap_singleton(
                        layout,
                        fetcher,
                        anchor,
                        &group.members[0],
                        now,
                        &mut resolved_assets,
                    );
                }
            }
            if let Some(lane) = &ahead {
                lane.passed(gi);
            }
        }
        drop(entered);
        drop(opens);
        // Each landing still to come, reported as it lands, until the last lane ends. Then
        // the joins, before anything below reads the store as the pass leaves it: the second
        // stub reconcile must see the vendor shims this pass laid, never re-lay a stub
        // over one.
        if let Some(landed) = landed.as_mut() {
            for landing in &landed_rx {
                landed.report(layout, landing, rider_now);
            }
        }
        let lanes = beside.len();
        for handle in beside {
            let faults = handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            crate::flow::note_fetch_faults(faults);
        }
        if lanes == 0
            && let Some(r) = rider.as_deref()
            && let Some(queue) = &queue
            && let Some(landed) = landed.as_mut()
        {
            rider_land(
                &r.fresh.lane(layout, fetcher, r.trust),
                r,
                queue,
                None,
                &mut |landing| landed.report(layout, landing, rider_now),
            );
        }
    });
    let rider_failed = landed
        .as_ref()
        .is_some_and(|landed| landed.pass.failures > 0);
    if let Some(r) = rider {
        r.landed = landed;
    }
    // What the lane fetched for a unit that never took it (one this loop aborted before it
    // reached the member) stays for the next pass: the pass-end gc spares the resolved
    // assets, and the next attempt stages the archive without a download.
    for (program, asset) in ahead.iter().flat_map(|lane| lane.untaken()) {
        resolved_assets.entry(program).or_insert(asset);
    }
    // Second stub reconcile, against what actually landed, so a program whose shims do not
    // expose its own name keeps no stale "installing" stub.
    let now_active = crate::active_builds(layout);
    crate::stub::reconcile(layout, &wanted, &now_active, &exposed);
    // The reroute stubs ride the same reconcile: the atpkg path they name is refreshed
    // ([`crate::stub::Embedder`]) and a row that left the table is swept.
    lay_reroute_stubs(layout);
    // (The rustup seam is the caller's, once per pass, through its [`PassSeams`].)
    // Alias reconcile over what is live now: every ALab program's `alab-<tool>` names stand
    // beside its shims, and a program the signed index no longer calls ALab's own loses them.
    reconcile_aliases(layout, index);
    // Exec-root reconcile over what is live now, after the aliases so the ones just laid
    // are rendered through a root that stands ([`reconcile_exec_roots`]).
    let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
    // Shadow reconcile over what is live now: a managed member whose exposed name a foreign
    // copy precedes on PATH is said as SHADOWED, never fixed, with `type alab-<tool> for
    // the managed one` when the alias just reconciled answers.
    reconcile_shadowed(layout, index.index_build, path_var);
    // Parity with the per-member install path: a wanted member the channel does not pin
    // fails loudly as NotPinned; silence would hide a half-published index.
    for program in &wanted {
        if pinned_members.contains(program)
            || installed.contains_key(program)
            || crate::linkmode::is_linked(layout, program)
            || crate::vendor_direct::is_vendor(program)
        {
            continue;
        }
        let e = crate::FlowError::NotPinned(program.clone());
        failures += 1;
        eprintln!("atpkg: bootstrap install {program} failed: {e} (continuing)");
        record_bootstrap_error(layout, program, &e);
    }
    // What a clean pass proved still awaits install (narrowed/unserved members were removed
    // from `wanted` above): the `*seed*` row reconcile's input.
    let remaining_missing: Vec<String> = wanted
        .iter()
        .filter(|p| !now_active.contains_key(p.as_str()))
        .cloned()
        .collect();
    // The bump file is consumed only once both lanes and the final missing-pin checks
    // settle, and only by a clean pass with nothing wanted left: a failed rider keeps it
    // like a failed set member, and a deferred signature check is no failure but leaves its
    // program missing, so a request for it may still arrive. Keeping it while wanted work
    // remains avoids a read-then-unlink race with a pending stub appending its request.
    if failures == 0 && !rider_failed && remaining_missing.is_empty() {
        crate::progress::clear_bump(layout);
    }
    DefaultSetOutcome {
        failures,
        announced,
        announced_count,
        resolve_provenance,
        index_error: None,
        resolved_index_build: Some(index.index_build),
        resolved_assets,
        skipped_narrowed,
        remaining_missing,
    }
}

/// The queue of both lanes in execution order. An ALab bump keeps its order even when it
/// moves a singleton ahead of a tuple; parallel vendor lanes (`beside`) stand after the last
/// remaining tuple (their gate opens once every archive of the set is in, the tuples' among
/// them, or once no tuple is left: [`RiderGate`]), and a serial vendor lane follows every
/// set member.
fn set_progress_queue(
    remaining: &[(usize, Vec<String>)],
    groups: &[crate::Group],
    vendors: &[String],
    beside: bool,
) -> Vec<String> {
    let after = if beside {
        remaining
            .iter()
            .rposition(|(gi, _)| groups[*gi].group.is_some())
            .map_or(0, |i| i + 1)
    } else {
        remaining.len()
    };
    let mut queue = Vec::new();
    for (i, (_, missing)) in remaining.iter().enumerate() {
        if i == after {
            queue.extend_from_slice(vendors);
        }
        queue.extend_from_slice(missing);
    }
    if after == remaining.len() {
        queue.extend_from_slice(vendors);
    }
    queue
}

/// Stably re-sort the remaining plan by the bump file's admitted names: a group with any
/// bumped member moves to the front (bumping a member bumps its whole group), ordered by
/// its earliest bump mention, plan order the tiebreak. The output is a permutation of the
/// input, never new work.
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

/// The units the set completion's prescan reads: every group with no dev-linked member, and
/// its members wanted here and not installed — a group with none left out.
fn prescan_units<'g>(
    groups: &'g [crate::Group],
    wanted: &std::collections::BTreeSet<String>,
    installed: &std::collections::BTreeMap<String, u64>,
    linked: &std::collections::BTreeSet<&str>,
) -> Vec<(&'g crate::Group, Vec<String>)> {
    groups
        .iter()
        .filter(|group| !group.members.iter().any(|m| linked.contains(m.as_str())))
        .map(|group| (group, absent_members(group, wanted, installed)))
        .filter(|(_, missing)| !missing.is_empty())
        .collect()
}

/// A first run's set-completion manifest prefetch, asked the moment the update pass's index
/// is in rather than once its vendor plan is: a first run's plan waits for a vendor's read
/// still out past the index ([`VENDOR_READ_GRACE`]), and a held vendor host held the set's
/// manifests, its announcement and its first byte behind that wait. The units are the
/// prescan's ([`prescan_units`]) over the wanted set as the pass first knows it, before
/// system satisfaction narrows it (a member satisfied by the system costs its pair in the
/// same wave); a manifest the prescan wants that this did not ask for, or that did not arrive
/// whole, its own prefetch fetches. Bytes into the fetcher's memo only; every read still
/// verifies them.
fn prefetch_set_manifests(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    cfg: &crate::config::PackagesConfig,
    installed: &std::collections::BTreeMap<String, u64>,
    index: &crate::TrustedIndex,
) {
    let triple = current_triple();
    let Some(ch) = index.channel_for(crate::config::CHANNEL, triple) else {
        return;
    };
    let groups = crate::apply::plan_groups_here(layout, index, &ch, triple);
    let mut wanted = wanted_programs(index, cfg);
    for spec in crate::vendor_direct::VENDORS {
        if spec.serves(triple)
            && vendor_wanted(cfg, &std::collections::BTreeSet::new(), spec.program)
        {
            wanted.insert(spec.program.to_string());
        }
    }
    for p in removed_programs(layout) {
        wanted.remove(&p);
    }
    let linked: std::collections::BTreeSet<&str> = groups
        .iter()
        .flat_map(|group| group.members.iter())
        .filter(|m| crate::linkmode::is_linked(layout, m))
        .map(String::as_str)
        .collect();
    let units = prescan_units(&groups, &wanted, installed, &linked);
    crate::flow::prefetch_pinned(
        fetcher,
        index,
        &ch,
        units
            .iter()
            .flat_map(|(group, missing)| prescan_probe(group, missing))
            .map(String::as_str),
    );
}

/// `group`'s members that are wanted here and not installed — what a pass would fetch
/// for it, in member order.
fn absent_members(
    group: &crate::Group,
    wanted: &std::collections::BTreeSet<String>,
    installed: &std::collections::BTreeMap<String, u64>,
) -> Vec<String> {
    group
        .members
        .iter()
        .filter(|m| wanted.contains(m.as_str()) && !installed.contains_key(m.as_str()))
        .cloned()
        .collect()
}

/// The members whose manifests the missing-triple prescan reads for `group`
/// ([`crate::flow::group_missing_triple`]): every member of a coherence tuple, since it
/// installs whole, else the `missing` ones alone.
fn prescan_probe<'a>(group: &'a crate::Group, missing: &'a [String]) -> &'a [String] {
    match &group.group {
        Some(_) => &group.members,
        None => missing,
    }
}

/// The grouped (coherence-tuple) arm of [`install_default_set_with_path`]: refuse a
/// config-narrowed partial tuple, clean-skip a tuple with no artifact for this triple, else
/// fresh-install the whole group all-or-nothing against the caller's one resolved index via
/// [`crate::flow::bootstrap_group`] — the update path's stage-all → flip-all → rollback
/// transaction, so no failure can activate a partial or version-split trust toolchain.
/// Returns the hard-failure count (0 or 1); skips are never failures.
#[allow(
    clippy::too_many_arguments,
    reason = "the group arm consumes install_default_set_with_path's whole resolved \
              context: layout, fetcher, the ONE verified index, config, the group + its \
              name, the wanted/installed/missing member sets the narrowing check and \
              prescan read, the pass's resolved-asset collector the pass-end gc \
              sparing reads, the pass's flip gate, and what follows a Trust flip"
)]
fn bootstrap_group(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    g: &str,
    group: &crate::Group,
    wanted: &std::collections::BTreeSet<String>,
    installed: &std::collections::BTreeMap<String, u64>,
    missing: &[String],
    resolved_assets: &mut std::collections::BTreeMap<String, String>,
    flip: &crate::quiet::FlipGate<'_>,
    toolchain_flipped: &dyn Fn(&crate::store::Layout),
) -> u32 {
    // If `exclude` leaves part of the tuple absent, refuse the whole group: a partial tuple
    // is the split state the transaction exists to prevent. Loud, but not a failure (the
    // config's doing).
    let narrowed_out: Vec<&String> = group
        .members
        .iter()
        .filter(|m| !wanted.contains(m.as_str()) && !installed.contains_key(m.as_str()))
        .collect();
    if !narrowed_out.is_empty() {
        eprintln!(
            "atpkg: {} not installed \u{2014} they install together, and [packages] exclude \
             leaves out {}",
            missing.join(", "),
            narrowed_out
                .iter()
                .map(|m| m.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        for m in missing {
            note_finished(m, crate::progress::Phase::Skipped, None);
        }
        return 0;
    }
    // Missing-triple prescan: the singleton NoArtifact clean skip lifted to the tuple — a
    // group that cannot fully exist on this host is a correct state, not a failure.
    if let Some(m) = crate::flow::group_missing_triple(
        fetcher,
        index,
        crate::config::CHANNEL,
        current_triple(),
        missing,
    ) {
        println!(
            "atpkg: {} not installed \u{2014} they install together, and no {m} build is \
             published for this machine ({}) yet",
            missing.join(", "),
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
        crate::config::CHANNEL,
        current_triple(),
        group,
        installed,
        resolved_assets,
        flip,
    ) {
        Ok((crate::TxnOutcome::Applied(_), applied)) => {
            // Advance the durable floor to the one index the whole group trusted.
            advance_floors(layout, index.index_build, index.roster_seq());
            for (m, a) in &applied {
                record_status(
                    layout,
                    m,
                    crate::ProgramStatus {
                        installed_build: Some(a.build),
                        state: crate::state::managed(a.build, index.index_build),
                        tree_root: effective_tree_root(layout, m, Some(a.build), &a.tree_root),
                    },
                    format!(
                        "bootstrap installed {m} build {} (coherence group '{g}')",
                        a.build
                    ),
                );
                println!("atpkg: installed {m} {}", a.build);
                note_finished(m, crate::progress::Phase::Done, None);
            }
            if applied.iter().any(|(m, _)| m == crate::seam::SEAM_PROGRAM) {
                toolchain_flipped(layout);
            }
            0
        }
        Ok((crate::TxnOutcome::UpToDate, _)) => {
            for m in missing {
                note_finished(m, crate::progress::Phase::Skipped, None);
            }
            0
        }
        // A tuple completed with `trust` live moves the toolchain, and under the unattended
        // gate a busy one waits: nothing moved, and the members install with that flip.
        Ok((crate::TxnOutcome::Deferred { why, .. }, _)) => {
            println!(
                "atpkg: {} not installed yet \u{2014} they install with the Trust toolchain \
                 update, when nothing is using the toolchain ({why})",
                missing.join(", ")
            );
            for m in missing {
                note_finished(m, crate::progress::Phase::Skipped, None);
            }
            0
        }
        Ok((crate::TxnOutcome::Pinned(held), _)) => {
            println!(
                "atpkg: {} not installed \u{2014} they install together, and you pinned {} ({} \
                 lets them install)",
                missing.join(", "),
                held.join(", "),
                held.iter()
                    .map(|h| format!("`aterm pkg unpin {h}`"))
                    .collect::<Vec<_>>()
                    .join(" and ")
            );
            for m in missing {
                note_finished(m, crate::progress::Phase::Skipped, None);
            }
            0
        }
        Ok((crate::TxnOutcome::Unpublished { member, triple, .. }, _)) => {
            // The transaction's own missing-triple hold (a tuple with a member installed),
            // reached when a manifest a prescan could not fetch has no artifact for this
            // triple: the same clean skip, in the same words.
            println!(
                "atpkg: {} not installed \u{2014} they install together, and no {member} build \
                 is published for this machine ({triple}) yet",
                missing.join(", ")
            );
            for m in missing {
                note_finished(m, crate::progress::Phase::Skipped, None);
            }
            0
        }
        Ok((crate::TxnOutcome::Tombstoned(members), _)) => {
            let builds = if members.len() == 1 {
                "build"
            } else {
                "builds"
            };
            eprintln!(
                "atpkg: {} not installed \u{2014} ALab withdrew the {} {builds}",
                missing.join(", "),
                members.join(", ")
            );
            // A canonical row, not silence, for a program the index names: the update
            // lane's words (`TOMBSTONED_PIN`), which doctor counts as the fault they are.
            for m in missing {
                let state = crate::state::TOMBSTONED_PIN.to_string();
                record_state_if_changed(layout, m, state.clone());
                note_finished(m, crate::progress::Phase::Skipped, Some(state));
            }
            0
        }
        // Whichever phase it aborted in, the transaction left every member as it was
        // (a flip that began is rolled back), so the line needs no phase.
        Ok((crate::TxnOutcome::Aborted { failed, why, .. }, _)) => {
            let cause = clip_cause(&why);
            eprintln!(
                "atpkg: could not install {} ({failed}: {cause}) \u{2014} nothing changed",
                missing.join(", ")
            );
            record_status(
                layout,
                &failed,
                // An already-installed member keeps its build and attestation (the abort
                // changed no bytes); a fresh member records neither. The store, not the
                // caller's snapshot, is asked what survived.
                failure_row(
                    layout,
                    &failed,
                    format!("error: coherence group '{g}' bootstrap aborted — {cause}"),
                ),
                format!("bootstrap group '{g}' aborted at {failed} — {cause}"),
            );
            // Terminal rows: the failed member carries the reason; its siblings did not
            // install either (all-or-nothing).
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
            eprintln!("atpkg: could not install {}: {e}", missing.join(", "));
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

/// The ungrouped arm of [`install_default_set_with_path`]: the per-member install path, with
/// its signed `requires` resolution. Re-reads the durable floor, so a floor advance earlier
/// in the pass is never undercut. Returns the hard-failure count (0 or 1); dev-link,
/// missing triple, app-bundle, tombstoned pin and a held digest verdict are skips.
fn bootstrap_singleton(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    anchor: &crate::Anchor,
    program: &str,
    now: i64,
    resolved_assets: &mut std::collections::BTreeMap<String, String>,
) -> u32 {
    let floor = build_floor(layout);
    let req = crate::InstallRequest {
        channel: crate::config::CHANNEL,
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
            // Advance the durable floor to the index this install trusted.
            advance_floors(layout, r.index_build, r.roster_seq);
            record_installed_deps(layout, program, r.index_build, &r.dependencies);
            let state = crate::state::managed(r.build, r.index_build);
            record_status(
                layout,
                program,
                crate::ProgramStatus {
                    installed_build: Some(r.build),
                    state: state.clone(),
                    tree_root: effective_tree_root(layout, program, Some(r.build), &r.tree_root),
                },
                format!("bootstrap installed {program} build {}", r.build),
            );
            println!("atpkg: installed {program} {}", r.build);
            note_finished(program, crate::progress::Phase::Done, None);
            0
        }
        // Correct non-failure states — skipped quietly-but-visibly:
        Err(crate::FlowError::Linked(p)) => {
            println!("atpkg: {p} dev-linked — skipped");
            note_finished(program, crate::progress::Phase::Skipped, None);
            0
        }
        // A target the pinned build does not serve: a canonical state, not a failure. The
        // pass log says the words the row records.
        Err(e @ crate::FlowError::NoArtifact(_)) => {
            let state = canonical_non_error_state(&e)
                .unwrap_or_else(|| crate::state::unavailable(current_triple(), ""));
            println!("atpkg: {program}: {state} — skipped, not failed");
            record_status(
                layout,
                program,
                failure_row(layout, program, state),
                format!("bootstrap install {program}: {e}"),
            );
            note_finished(program, crate::progress::Phase::Skipped, None);
            0
        }
        Err(crate::FlowError::AppBundleRefused(_)) => {
            println!("atpkg: {program} updates itself \u{2014} skipped");
            note_finished(program, crate::progress::Phase::Skipped, None);
            0
        }
        Err(crate::FlowError::Tombstoned(_)) => {
            eprintln!("atpkg: {program} not installed \u{2014} ALab withdrew the {program} build");
            // The same canonical row the group arm and the update lane write.
            let state = crate::state::TOMBSTONED_PIN.to_string();
            record_state_if_changed(layout, program, state.clone());
            note_finished(program, crate::progress::Phase::Skipped, Some(state));
            0
        }
        // A member its digest-refusal memo held: a skip, not a failure
        // ([`skip_held_member`]).
        Err(crate::FlowError::StageRefused(row)) => skip_held_member(layout, program, row),
        Err(e) => {
            eprintln!("atpkg: bootstrap install {program} failed: {e} (continuing)");
            record_bootstrap_error(layout, program, &e);
            note_finished(program, crate::progress::Phase::Failed, Some(e.to_string()));
            1
        }
    }
}

/// The bootstrap lane's answer for a member its digest-refusal memo held off this pass
/// ([`crate::FlowError::StageRefused`]): the canonical `held: last attempt failed with
/// <why>; <retry>` row, recorded, and a skip — never a failure (returns 0). The pass staged
/// nothing and no unattended pass will until the pin's signed digests change or a person
/// asks; as a failure the window would re-run the pass on its failure backoff for days.
/// The row is still a problem to `doctor`, where the retry is named.
fn skip_held_member(layout: &crate::store::Layout, program: &str, row: String) -> u32 {
    println!("atpkg: {program}: {row} — skipped, not staged");
    record_status(
        layout,
        program,
        failure_row(layout, program, row.clone()),
        format!("bootstrap install {program}: {row}"),
    );
    note_finished(program, crate::progress::Phase::Skipped, Some(row));
    0
}

/// Record one hard bootstrap failure for `program` into `status.toml` (the ungrouped arm
/// and the unpinned-member sweep).
fn record_bootstrap_error(layout: &crate::store::Layout, program: &str, e: &crate::FlowError) {
    match canonical_non_error_state(e) {
        Some(state) => record_status(
            layout,
            program,
            failure_row(layout, program, state),
            format!("bootstrap install {program}: {e}"),
        ),
        None => record_bootstrap_failure(layout, program, &e.to_string()),
    }
}

/// Record `error: <why>` for `program` — [`record_bootstrap_error`]'s write for a failure
/// that is no single program's [`crate::FlowError`], such as the default set's up-front
/// disk refusal ([`SetDiskRefusal`]).
fn record_bootstrap_failure(layout: &crate::store::Layout, program: &str, why: &str) {
    record_status(
        layout,
        program,
        failure_row(layout, program, format!("error: {why}")),
        format!("bootstrap install {program}: {why}"),
    );
}

/// `atpkg install --default-set` — the explicit one-shot bootstrap: install every missing
/// default-set member now (Settings' "Install ALab toolset"). Running it is the consent,
/// whatever `[packages].auto_install` says. Exit 1 iff any member hard-failed.
fn cmd_install_default_set() -> ExitCode {
    // Answer the announcement: `install_default_set_with_path` may print `net-starting:`,
    // and this verb ends through several returns that print no marker.
    let code = cmd_install_default_set_code();
    answer_announcement(code == 0);
    ExitCode::from(code)
}

/// [`cmd_install_default_set`]'s body, returning the raw exit code: 2 ("ran fine, installed
/// nothing, and never will here") is neither success nor a retryable failure.
fn cmd_install_default_set_code() -> u8 {
    if !manager_enabled() {
        eprintln!("atpkg: disabled (no root key pinned) — refusing to install");
        return 1;
    }
    let Some(layout) = layout() else {
        return 1;
    };
    let fetcher = resolve_fetcher(&layout);
    install_toolset(
        &layout,
        crate::config::cached(),
        &*fetcher,
        &PassSeams::process(),
    )
}

/// [`cmd_install_default_set_code`]'s pass over the store it is handed.
fn install_toolset(
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    fetcher: &dyn crate::flow::Fetcher,
    seams: &PassSeams<'_>,
) -> u8 {
    reconcile_links(layout, cfg);
    // Asking for the toolset lifts any earlier decline and per-program removal — before the
    // pass, which narrows `wanted` by those markers (a removed group member would narrow
    // its whole tuple out of the install asked to restore it).
    clear_decline(layout);
    let all_removed: Vec<String> = removed_programs(layout).into_iter().collect();
    clear_removed(layout, &all_removed);
    // The seed pass's claim on the set goes too: the owner's own act stands behind it now,
    // so no later pass says "installing aterm is the consent" ([`clear_seed_adoption`]).
    clear_seed_adoption(layout);
    let before = crate::active_builds(layout);
    // What this pass verifies from the channel itself, counted from here — its one stamp
    // says whether it reached the index ([`record_pass_end`]).
    let live_before = crate::flow::live_verifications();
    // The index resolve first, blocking nothing, as in the update pass: a fresh
    // verification refreshes the yank latch the vendor lane binds.
    let resolved = crate::resolve_verified_index(
        fetcher,
        layout,
        &(seams.anchor)(layout),
        build_floor(layout),
        (seams.now)(),
    )
    .ok();
    // The vendor programs' plans, from their vendors (design §1.5), under the yanks of the
    // index resolved above: never behind it.
    let (mut vendor, fresh) = vendor_plan(
        layout,
        fetcher,
        cfg,
        &before,
        VendorLanes {
            update_installed: false,
            complete_the_set: true,
        },
        resolved.as_ref(),
        seams,
    );
    // AND THEIR LANDINGS RIDE THE SET, as an empty store's update pass lands them: the
    // toolchain's archives take the link first, the agents land beside the rest of the set
    // under its one announcement ([`VendorRider`]). Landed first, the four agents held the
    // toolchain's first byte back by their whole download (about 770 MiB). With no index the
    // set has nothing to ride, and they land alone, first, as they always did.
    let mut rider = None;
    if resolved.is_some() && !fresh.pending.is_empty() {
        rider = Some(VendorRider {
            fresh,
            trust: seams.trust,
            warm: seams.warm_agents,
            landed: None,
        });
    } else {
        land_fresh_alone(
            layout,
            fetcher,
            Consent::Explicit,
            &fresh,
            &mut vendor,
            seams,
            false,
        );
        warm_landed(layout, &mut vendor, seams);
    }
    // The Trust toolchain is usable the moment its tuple flips, as on the update pass: the
    // rustup view and the exec roots follow it there (the verb's end re-lays both).
    let toolchain_flipped = |layout: &crate::store::Layout| {
        (seams.rustup_seam)(layout);
        let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
    };
    // Running this verb is the consent, and it never reads `auto_install`: its announcement
    // names neither the first launch's consent nor that key, only the decline that stops
    // every pass ([`Consent::Explicit`]).
    let mut failures_outcome = install_default_set_with_path(
        layout,
        fetcher,
        &(seams.anchor)(layout),
        cfg,
        Consent::Explicit,
        (seams.now)(),
        resolved.as_ref(),
        seams.path.as_deref(),
        // A person's verb: what it installs lands now.
        &crate::quiet::FlipGate::NOW,
        &std::collections::BTreeSet::new(),
        SetCompanions {
            vendor: rider.as_mut(),
            toolchain_flipped: &toolchain_flipped,
            vendors_after: false,
        },
    );
    // What the rider's landings came to, or its landings alone when the set stopped before
    // it could carry them — their plan then the promised one. The verb's own terminal
    // answers the one announcement ([`answer_announcement`]).
    if let Some(rider) = rider.take() {
        settle_rider(
            layout,
            fetcher,
            Consent::Explicit,
            rider,
            &mut vendor,
            seams,
            true,
        );
    }
    // Adoption is decided on the index lane alone: a vendor program still to come is what
    // the set completion adoption grants will finish.
    let index_failures = failures_outcome.failures;
    failures_outcome.failures += vendor.failures;
    failures_outcome
        .resolved_assets
        .extend(vendor.resolved_assets.clone());
    let failures = failures_outcome.failures;
    let activated = crate::active_builds(layout)
        .keys()
        .filter(|k| !before.contains_key(k.as_str()))
        .count();
    // The explicit act is adoption — when it laid something down, or found the whole set
    // already on disk ([`default_set_adopts`]) — but not after a pass that installed nothing
    // on a machine with no toolset, nor over members it skipped or still owes.
    let (vendor_missing, index_missing): (Vec<String>, Vec<String>) = failures_outcome
        .remaining_missing
        .iter()
        .cloned()
        .partition(|p| crate::vendor_direct::is_vendor(p));
    if default_set_adopts(
        activated,
        index_failures,
        !before.is_empty(),
        &failures_outcome.skipped_narrowed,
        &index_missing,
    ) {
        record_adoption(layout);
    }
    // GC and the shell hooks once at the CLI edge, abstentions disclosed (this verb walks the
    // whole prefix). The sparing form keeps a failed download's `.part`, so a retry resumes;
    // the build kept below each live one is read under the channel this verb verified.
    let channel = resolved
        .as_ref()
        .and_then(|index| index.channel_for(crate::config::CHANNEL, current_triple()));
    let report = crate::gc::run_keeping_pinned_partials(layout, channel.as_ref(), &|p| {
        failures_outcome.resolved_assets.get(p).cloned()
    });
    print_gc_sweeps("install-default-set", &report);
    print_gc_abstentions("install-default-set", &report);
    // The rustup seam, as the update pass re-asserts it after its gc.
    (seams.rustup_seam)(layout);
    (seams.shell_hooks)(layout);
    // The pass record says whether this pass reached the index, whatever it installed: the
    // verb's exit below reports its members, the stamp what the schedulers decide on.
    let now = (seams.now)();
    record_pass_end(layout, now, crate::flow::live_verifications() > live_before);
    if failures > 0 {
        return 1;
    }
    // A clean pass retires a legacy `*seed*` offer row ([`retire_seed_row`]).
    retire_seed_row(layout);
    // Not complete while a vendor program is still to come: its vendor did not serve it
    // this pass, so nothing was checked — retryable, never "already complete".
    if !vendor_missing.is_empty() {
        eprintln!(
            "atpkg: default set incomplete — {} not installed yet (the line above says why); \
             `aterm pkg update` retries",
            vendor_missing.join(", ")
        );
        return 1;
    }
    let skipped = &failures_outcome.skipped_narrowed;
    // Zero failures is not "it worked": every member can clean-skip (no artifact for this
    // triple, all dev-linked, all tombstoned), so the no-op case gets its own words and
    // exit code, and a summary with skips names them.
    if activated == 0 {
        let already = !before.is_empty();
        if already {
            if skipped.is_empty() {
                println!("atpkg: default set already complete — nothing to install");
            } else {
                println!(
                    "atpkg: default set: nothing installed — {} program(s) skipped: {} \
                     (reasons above)",
                    skipped.len(),
                    skipped.join(", ")
                );
            }
            say_c_toolchain(
                seams.path.as_deref(),
                &crate::prereq::CcHost::this_machine(),
            );
            print_managed_current(layout, &vendor.unchecked, now);
            record_index_freshness(layout, now, None);
            return 0;
        }
        println!(
            "atpkg: nothing was installed — the signed index pins no program with a build \
             for this machine ({}). This is not a failure to retry; nothing lands here \
             until an artifact for this architecture is published.",
            current_triple()
        );
        // Exit 2, not 0 and not 1: the GUI maps 0 to "Succeeded" (which would be the
        // lie) and 1 to a retryable failure (which would be false hope).
        return 2;
    }
    if skipped.is_empty() {
        println!("atpkg: default set complete ({activated} program(s) installed)");
    } else {
        println!(
            "atpkg: default set: {activated} program(s) installed, {} skipped: {} \
             (reasons above)",
            skipped.len(),
            skipped.join(", ")
        );
    }
    say_c_toolchain(
        seams.path.as_deref(),
        &crate::prereq::CcHost::this_machine(),
    );
    print_managed_current(layout, &vendor.unchecked, now);
    record_index_freshness(layout, now, None);
    0
}

/// The sentence a finished default set adds when the machine cannot BUILD with it: the
/// C toolchain's verdict ([`crate::prereq`]) against the `PATH` the pass ran with, in
/// `doctor`'s own words, when it is an answer that blocks builds. The install did what
/// was asked, so the exit code stays; the set simply stops claiming a capability it did
/// not prove. `None` when a toolchain answers, or the probe cannot tell.
fn prereq_line(verdict: &crate::prereq::CcVerdict) -> Option<String> {
    verdict
        .blocks_builds()
        .then(|| format!("atpkg: {}", verdict.line()))
}

/// `atpkg seed` — the first-launch bootstrap, spawned by the GUI once per launch. It
/// installs nothing: it records adoption, lays a pending stub for every default-set name so
/// each answers on PATH before its bytes land, lays the reroute stubs, refreshes the shell
/// hooks, re-asserts the rustup seam and the exec roots, and says whether the signed index
/// serves this machine ([`report_seedless_posture`]). The update pass right behind it
/// installs, announced before a byte moves.
///
/// `[packages].auto_install = false` adopts and lays nothing; the one write every launch
/// makes regardless is retiring a legacy `*seed*` row ([`retire_seed_row`]). Every early
/// return is loud and exit-0.
fn cmd_seed(rest: &[String]) -> ExitCode {
    if let Some(refusal) = zero_arity(rest, "seed") {
        return refusal;
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    // A legacy `*seed*` offer row goes first, whatever the consent below decides: it was
    // written exactly on the machines whose update pass never completes the set, so the
    // set-completing lanes alone would never sweep it ([`retire_seed_row`]).
    retire_seed_row(&layout);
    // Installing aterm is wanting the toolset, so this lane adopts the machine, but only
    // once the one consent predicate says yes ([`crate::adoption`], which the window asks
    // too, so the two cannot drift). A deliberate `uninstall --all` and `auto_install =
    // false` (which folds in the retired `seed_install = false`) are checked before
    // adoption: once adopted, the update pass installs the whole set over the network
    // seconds later.
    let cfg = crate::config::cached();
    match crate::adoption::adoption(&layout, cfg) {
        crate::Adoption::Consented => {}
        crate::Adoption::Declined => {
            // The user removed this toolset deliberately (`uninstall --all`). This lane
            // adopts the toolset, so without this check it would undo that on the very
            // next launch.
            println!(
                "atpkg: the ALab toolset was removed on this machine — updates no longer \
                 install it; `aterm pkg install --default-set` puts it back"
            );
            return ExitCode::SUCCESS;
        }
        // A `[packages] prefix` this build ignores counts as "not here" until its line is
        // removed: the toolset lives where it points. Loading the config already said so
        // ([`crate::config::ignored_prefix_note`]).
        crate::Adoption::IgnoredPrefix => return ExitCode::SUCCESS,
        crate::Adoption::AutoInstallOff => {
            // Declining the lay-down is not adoption: leave the store as it was (the
            // legacy-row sweep above is the one write, and it only removes).
            println!("atpkg: [packages].auto_install = false — not installing the ALab toolset");
            return ExitCode::SUCCESS;
        }
        // A disabled manager adopts nothing: stubs would promise a pass that answers
        // "disabled", and a durable adoption would install the whole set the moment a root
        // key is pinned, a consent this lane never obtained.
        crate::Adoption::ManagerDisabled => {
            println!("atpkg: disabled (no root key pinned) — not installing the ALab toolset");
            return ExitCode::SUCCESS;
        }
    }
    // Who adopted rides beside the marker, and only when this call creates it: an update
    // pass says "installing aterm is the consent" only on a machine the seed pass adopted.
    record_seed_adoption(&layout);
    // The update this lane owes, promised before a stub can run: on a first run the window
    // spawns its update next, and that update writes `progress.json` only once it has
    // resolved the index and planned. A stub run meanwhile waits for it ([`pending_state`])
    // instead of finding nothing installing. Only the window's seed knows an update follows
    // ([`seed_promise_owner`]); it promises for as long as the window lives, at most
    // [`crate::progress::SEED_PROMISE_SECS`].
    if let Some(owner) = seed_promise_owner()
        && crate::adoption::first_install_ahead(&layout)
    {
        crate::progress::promise_pass(&layout, owner, crate::progress::SEED_PROMISE_SECS);
    }
    // Pending stubs at adoption: every default-set name resolves on PATH before a network
    // byte moves, and running one prints the live install state (`atpkg __pending`) instead
    // of "command not found". Compiled roster only; the first index resolve reconciles it.
    crate::stub::lay_adoption_stubs(&layout, cfg);
    // Then the base layer's relay items (design init-system §5), the doctor's own: the
    // shell hooks now, not at the end of the first update pass, since they are what append
    // `bin/` to a shell's PATH (the window's integration re-sources them at the next
    // prompt), so the stubs just laid answer at once and the real shims replace them in
    // place as they land; the rc block, in the login shell's own rc too when it is absent;
    // the reroute stubs, so the upstream Rust names answer inside a session from the first
    // seed on. Every gate is the items': the rc opt-out, the release-bundle rule, the
    // consent-fenced folders.
    relay_base(&layout, &SEED_RELAY);
    reassert_rustup_seam(&layout);
    // The exec roots beside the seam: this lane runs at every launch, so a trust machine is
    // healed at the next launch, not the next networked pass.
    relay_base(&layout, &SEED_RELAY_AFTER_SEAM);
    // The Command Line Tools, read without running anything: a toolset that arrived with no
    // window watching (an install over SSH, `--no-app`) files the window's entry at this
    // launch, and tools installed since end it.
    say_c_toolchain_at_seed(
        std::env::var_os("PATH").as_deref(),
        &crate::prereq::CcHost::this_machine(),
        !crate::active_builds(&layout).is_empty(),
    );
    // Promise nothing the index cannot deliver: say whether the signed index serves this
    // machine, and leave a durable trace of the answer.
    report_seedless_posture(&layout);
    ExitCode::SUCCESS
}

/// The channel-pinned members `index` can still install on this machine: wanted by config,
/// not removed on purpose, not already active, and carrying an artifact for this triple.
/// Coverage is per program (the rustc coherence tuple may serve fewer triples than the
/// standalone programs), so this may be nothing, and [`index_serves`] says which sentence
/// is then true. A coherence tuple with any member lacking this triple contributes nothing,
/// as in [`install_default_set_with_path`].
fn seed_serviceable(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    cfg: &crate::config::PackagesConfig,
) -> Vec<String> {
    // Programs the user removed on purpose are absent by request, not missing.
    serviceable_among(
        layout,
        fetcher,
        index,
        &wanted_programs(index, cfg),
        &removed_programs(layout),
    )
}

/// [`seed_serviceable`] over an explicit `wanted` set and `removed` set, so
/// [`index_serves`] can ask the same question without the user's narrowing.
fn serviceable_among(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    wanted: &std::collections::BTreeSet<String>,
    removed: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let Some(ch) = index.channel_for(crate::config::CHANNEL, current_triple()) else {
        return Vec::new();
    };
    let ch = &ch;
    let installed = crate::active_builds(layout);
    let triple = current_triple();
    let mut out: Vec<String> = Vec::new();
    let mut prescan: Vec<(crate::Group, Vec<String>)> = Vec::new();
    for group in crate::apply::plan_groups_here(layout, index, ch, triple) {
        let mut missing = absent_members(&group, wanted, &installed);
        missing.retain(|m| !removed.contains(m.as_str()));
        if missing.is_empty()
            || group
                .members
                .iter()
                .any(|m| crate::linkmode::is_linked(layout, m))
        {
            continue;
        }
        // A coherence tuple applies all-or-nothing, and `bootstrap_group` refuses the whole
        // group when any member is neither wanted nor installed (an `exclude`, a prior
        // uninstall): the prescan applies the installer's own rule, so nothing is
        // advertised that the lane would then refuse.
        if group.group.is_some()
            && !group
                .members
                .iter()
                .all(|m| wanted.contains(m.as_str()) || installed.contains_key(m.as_str()))
        {
            continue;
        }
        prescan.push((group, missing));
    }
    // Every manifest the prescan reads, fetched at once ([`install_default_set_inner`]'s).
    crate::flow::prefetch_pinned(
        fetcher,
        index,
        ch,
        prescan
            .iter()
            .flat_map(|(group, missing)| prescan_probe(group, missing))
            .map(String::as_str),
    );
    for (group, missing) in prescan {
        let probe = prescan_probe(&group, &missing);
        if crate::flow::group_missing_triple(fetcher, index, crate::config::CHANNEL, triple, probe)
            .is_none()
        {
            out.extend(missing);
        }
    }
    out.sort();
    out
}

/// What the signed index says about this machine, for an empty store's one verdict
/// ([`report_seedless_posture`], the update pass's empty-store arm).
#[derive(Debug, Clone, PartialEq, Eq)]
enum IndexServes {
    /// Something here is still installable — or the store already holds builds.
    Yes,
    /// Nothing the index publishes carries an artifact for this triple; the triples its
    /// signed manifests do carry, sorted ([`served_triples`]) — what machine would do it.
    Unserved(Vec<String>),
    /// The index serves this triple, and the user's own narrowing alone emptied the set.
    Narrowed(Narrowing),
}

/// Which members the index serves on this machine the user's own narrowing took away,
/// when that narrowing alone left nothing to install ([`IndexServes::Narrowed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Narrowing {
    /// Named by `[packages].exclude`.
    excluded: Vec<String>,
    /// Uninstalled one at a time ([`removed_programs`]).
    removed: Vec<String>,
}

impl Narrowing {
    /// The sentence — a `seed-unusable:` body, the `*toolset*` row's reason — and the
    /// row's state. An exclusion is a line a person can shorten, so it is a failure
    /// ([`crate::state::TOOLSET_EXCLUDED`], an `unavailable:` fault the window badges);
    /// programs removed on purpose, and only those, are a decision
    /// ([`REMOVED_ON_PURPOSE_CLAUSE`], [`crate::state::TOOLSET_REMOVED`], no badge).
    fn verdict(&self, triple: &str) -> (String, &'static str) {
        if self.excluded.is_empty() {
            return (
                format!(
                    "every ALab program available here {REMOVED_ON_PURPOSE_CLAUSE} ({}) — \
                     nothing to install. `aterm pkg install <program>` brings one back",
                    self.removed.join(", ")
                ),
                crate::state::TOOLSET_REMOVED,
            );
        }
        let uninstalled = if self.removed.is_empty() {
            String::new()
        } else {
            format!("; uninstalled: {}", self.removed.join(", "))
        };
        (
            format!(
                "[packages].exclude narrows the toolset to nothing installable on this \
                 machine ({triple}; excluded: {}{uninstalled}) — shorten it to receive the \
                 toolset",
                self.excluded.join(", ")
            ),
            crate::state::TOOLSET_EXCLUDED,
        )
    }
}

/// Whether `index` serves this machine, and — when nothing is left to install — why.
/// [`seed_serviceable`] is empty for three causes, and only one is the CPU's: nothing is
/// published for this triple, `[packages].exclude` names what is, or the user uninstalled
/// it. Asking the same question without the user's narrowing tells them apart.
fn index_serves(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    cfg: &crate::config::PackagesConfig,
) -> IndexServes {
    if !seed_serviceable(layout, fetcher, index, cfg).is_empty() {
        return IndexServes::Yes;
    }
    let unnarrowed = index.installable(&[]);
    let everything = serviceable_among(
        layout,
        fetcher,
        index,
        &unnarrowed,
        &std::collections::BTreeSet::new(),
    );
    if everything.is_empty() {
        return IndexServes::Unserved(served_triples(fetcher, index));
    }
    let removed = removed_programs(layout);
    let narrowing = Narrowing {
        excluded: everything
            .iter()
            .filter(|m| cfg.exclude().contains(m))
            .cloned()
            .collect(),
        removed: everything
            .iter()
            .filter(|m| removed.contains(m.as_str()))
            .cloned()
            .collect(),
    };
    // Unreachable while the two sets are the only difference between the questions; if
    // they ever are not, the index still serves this machine, which is the true half.
    if narrowing.excluded.is_empty() && narrowing.removed.is_empty() {
        return IndexServes::Yes;
    }
    IndexServes::Narrowed(narrowing)
}

/// The triples other than this machine's that the signed manifests the index pins carry
/// an artifact for, sorted — [`IndexServes::Unserved`]'s "what machine would do it". Every
/// target's view of the channel is asked, so a per-target pin counts; each pinned manifest
/// is fetched once. Asked only on an empty store that nothing here serves.
fn served_triples(fetcher: &dyn crate::flow::Fetcher, index: &crate::TrustedIndex) -> Vec<String> {
    let here = current_triple();
    let Some(base) = index
        .channels
        .iter()
        .find(|c| c.name == crate::config::CHANNEL)
    else {
        return Vec::new();
    };
    let mut views: Vec<&str> = vec![here];
    views.extend(base.pin_by_target.keys().map(String::as_str));
    let mut asked: std::collections::BTreeSet<(String, u64)> = std::collections::BTreeSet::new();
    let mut served: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for view in views {
        let ch = base.for_target(view);
        for (program, build) in &ch.pin {
            if !asked.insert((program.clone(), *build)) {
                continue;
            }
            if let Some((_, _, pkg)) = crate::flow::verified_pkg(fetcher, index, &ch, program) {
                served.extend(pkg.artifacts.into_iter().map(|a| a.target));
            }
        }
    }
    served.remove(here);
    served.into_iter().collect()
}

/// The clause naming the triples ALab does build for (`it builds for …; `), or nothing
/// when no signed manifest names one.
fn builds_for_clause(served: &[String]) -> String {
    if served.is_empty() {
        String::new()
    } else {
        format!("it builds for {}; ", served.join(", "))
    }
}

/// The signed sizes of a set — [`seed_install_bytes`]' answer, and the input of
/// [`size_parenthetical`]. The two figures are independent: a registry row that
/// declares only `disk_installed` still discloses that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SignedSizes {
    /// What the wire lane moves (`[cost].download_bytes`, summed); `None` when
    /// unknown for any member.
    download: Option<u64>,
    /// What stays when the install is finished (`[cost].disk_installed`, summed);
    /// `None` when unknown for any member.
    disk: Option<u64>,
}

impl SignedSizes {
    /// Neither figure known — the line prints no size at all.
    const UNKNOWN: Self = Self {
        download: None,
        disk: None,
    };

    /// Both sets together: each figure known only where both know it.
    fn and(self, other: Self) -> Self {
        let sum = |a: Option<u64>, b: Option<u64>| a.zip(b).map(|(a, b)| a.saturating_add(b));
        Self {
            download: sum(self.download, other.download),
            disk: sum(self.disk, other.disk),
        }
    }
}

/// The signed sizes of `members` for this triple, summed from the pinned manifests' `[cost]`
/// rows ([`crate::manifest::Cost`]): `download_bytes` and `disk_installed`. A figure is
/// `None` when any member's manifest is unavailable or its entry is zero: a partial sum
/// would understate the commitment.
fn seed_install_bytes(
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    members: &[String],
) -> SignedSizes {
    let triple = current_triple();
    // The per-target channel view, so the figures announced are what this triple's pins
    // will fetch.
    let Some(ch) = index.channel_for(crate::config::CHANNEL, triple) else {
        return SignedSizes::UNKNOWN;
    };
    let ch = &ch;
    let (mut download, mut disk) = (Some(0u64), Some(0u64));
    for m in members {
        let Some((_, _, pkg)) = crate::flow::verified_pkg(fetcher, index, ch, m) else {
            return SignedSizes::UNKNOWN;
        };
        let Some(art) = pkg.artifact_for(triple) else {
            return SignedSizes::UNKNOWN;
        };
        download = sum_nonzero(download, art.cost.download_bytes);
        disk = sum_nonzero(disk, art.cost.disk_installed);
    }
    SignedSizes {
        download: download.filter(|t| *t > 0),
        disk: disk.filter(|t| *t > 0),
    }
}

/// One install unit of the default set, as its up-front disk check reads it
/// ([`set_units`]): an ungrouped program, or one coherence tuple, which the group lane
/// lands all-or-nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SetUnit {
    /// The tuple's name; `None` for an ungrouped program.
    group: Option<String>,
    /// The members this pass adds, in member order: the ones the refusal writes rows for.
    members: Vec<String>,
    /// What the unit needs on disk while it lands — what its own gate checks, which for a
    /// tuple can count an installed member beside the added ones
    /// ([`crate::flow::planned_landing`]).
    bytes: crate::flow::LandingBytes,
}

/// The planned units (each group with the members it adds, in plan order), each read
/// through `landing` ([`crate::flow::planned_landing`]). A unit its lane lands nothing of
/// ([`crate::flow::PlannedLanding::Nothing`]: a revoked pin, a pinned tuple, a held
/// member, …) is left out. `None` when any other unit's bytes are unknown: a partial sum
/// would understate the need.
fn set_units(
    planned: &[(&crate::Group, Vec<String>)],
    landing: impl Fn(&crate::Group) -> crate::flow::PlannedLanding,
) -> Option<Vec<SetUnit>> {
    let mut units = Vec::new();
    for (group, members) in planned {
        let bytes = match landing(group) {
            crate::flow::PlannedLanding::Nothing => continue,
            crate::flow::PlannedLanding::Bytes(bytes) => bytes,
            crate::flow::PlannedLanding::Unknown => return None,
        };
        units.push(SetUnit {
            group: group.group.clone(),
            members: members.clone(),
            bytes,
        });
    }
    Some(units)
}

/// The default set's up-front disk refusal: the line, and the status row each member keeps.
#[derive(Debug)]
struct SetDiskRefusal {
    line: String,
    /// `(member, why)` for every member of every unit judged, recorded as `error: <why>`
    /// ([`record_bootstrap_failure`]): a unit that cannot fit by itself gives its members
    /// its own figure, every other member the set's sentence, so no row says a member
    /// cannot fit when its unit would.
    rows: Vec<(String, String)>,
}

impl SetDiskRefusal {
    /// Write every row ([`SetDiskRefusal::rows`]).
    fn record(&self, layout: &crate::store::Layout) {
        for (m, why) in &self.rows {
            record_bootstrap_failure(layout, m, why);
        }
    }
}

/// The default set's up-front disk refusal, pure: `Some` when the peak the per-unit gates
/// enforce ([`crate::flow::peak_need`] over `units`), plus `beside` — what landings running
/// at the same time need at once (the vendor lane's, [`FreshVendors::peak`]) — plus the
/// reserve every disk gate holds back ([`crate::cost::FREE_FLOOR`]), exceeds `have`, the
/// free space on the volume holding `prefix`. The line offers the smallest ungrouped
/// program that fits by itself — never a tuple member, which `aterm pkg install` would
/// land outside its tuple.
fn set_disk_refusal(
    units: &[SetUnit],
    beside: u64,
    have: u64,
    prefix: &std::path::Path,
) -> Option<SetDiskRefusal> {
    let floor = crate::cost::FREE_FLOOR;
    let peak = crate::flow::peak_need(units.iter().map(|u| u.bytes)).saturating_add(beside);
    if crate::cost::disk_ok(peak, have, floor) {
        return None;
    }
    let need = peak.saturating_add(floor);
    let sentence = format!(
        "the ALab toolset needs {}{} free on the volume holding {}; {} free \u{2014} free {}",
        crate::cost::human_bytes(need),
        if beside > 0 {
            " with the agents installing beside it"
        } else {
            ""
        },
        prefix.display(),
        crate::cost::human_bytes(have),
        crate::cost::human_bytes(need.saturating_sub(have))
    );
    let fits = |u: &SetUnit| crate::cost::disk_ok(u.bytes.need(), have, floor);
    let mut line = format!("atpkg: {sentence}");
    if let Some(m) = units
        .iter()
        .filter(|u| u.group.is_none() && fits(u))
        .min_by_key(|u| u.bytes.need())
        .and_then(|u| u.members.first())
    {
        line.push_str(&format!(", or install {m} alone: aterm pkg install {m}"));
    }
    let rows = units
        .iter()
        .flat_map(|u| {
            let why = if fits(u) {
                sentence.clone()
            } else {
                crate::FlowError::InsufficientDisk {
                    required: u.bytes.need().saturating_add(floor),
                    available: have,
                }
                .to_string()
            };
            u.members.iter().map(move |m| (m.clone(), why.clone()))
        })
        .collect();
    Some(SetDiskRefusal { line, rows })
}

/// One step of a None-on-zero sum: a zero (undeclared) entry poisons the total.
fn sum_nonzero(total: Option<u64>, n: u64) -> Option<u64> {
    total.filter(|_| n > 0).map(|t| t.saturating_add(n))
}

/// The trailing size parenthetical both announcing lanes end with — one renderer
/// ([`crate::cost::human_bytes`]) so the figures cannot drift across surfaces. The GUI's
/// toolchain bar reads the line's last `(…)` and clips at 40 characters: two 9-character
/// figures (up to `999.9 GiB`) render in 39; only both in the `1000.0–1023.9 MiB` band
/// render 41, clipping "on disk" with both numbers intact. An unknown figure prints
/// nothing: no number beats a wrong one.
fn size_parenthetical(sizes: SignedSizes) -> String {
    use crate::cost::human_bytes;
    match (sizes.download, sizes.disk) {
        (Some(d), Some(k)) => format!(
            " (~{} download, ~{} on disk)",
            human_bytes(d),
            human_bytes(k)
        ),
        (None, Some(k)) => format!(" (~{} on disk when finished)", human_bytes(k)),
        (Some(d), None) => format!(" (~{} download)", human_bytes(d)),
        (None, None) => String::new(),
    }
}

/// Is this process running under Rosetta translation? (`sysctl sysctl.proc_translated`,
/// 1 = translated.) [`current_triple`] is compile-time, so the x86_64 slice of the
/// universal binary reports `x86_64-apple-darwin` on an Apple Silicon Mac, and "no build
/// for this architecture" is then about the slice, not the machine: anything irreversible
/// keyed on that verdict asks this first. An unreadable sysctl answers `false`.
#[cfg(target_os = "macos")]
fn running_translated() -> bool {
    aterm_uds::spawnfd::command("/usr/sbin/sysctl")
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

/// Say something true about whether the signed index serves this machine, and leave a
/// durable trace of it — the seed verb's last word. Three situations get three sentences:
/// the index publishes for this architecture (the toolset arrives over the network), it
/// does not (nothing arrives until someone publishes), or the user's own
/// `[packages].exclude` or uninstalls took everything it serves ([`index_serves`]). It
/// records `status.toml` too: Settings reads its absence as "atpkg has not run yet".
fn report_seedless_posture(layout: &crate::store::Layout) {
    let cfg = crate::config::cached();
    report_seedless_posture_as(layout, cfg, wait_lock().is_some(), now_unix(), || {
        cached_index_for(layout, cfg)
    });
}

/// [`report_seedless_posture`] under `cfg`, as of `now`, from the window's own seed
/// (`window_pass`: a `--wait-lock` caller, [`wait_lock`]) or not.
fn report_seedless_posture_as(
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    window_pass: bool,
    now: i64,
    read_cached_index: impl FnOnce() -> Option<crate::TrustedIndex>,
) {
    let triple = current_triple();
    // A resolve failure is not the question asked (offline is not "unsupported"), so it
    // degrades to the generic sentence. `seed_serviceable` is empty both when nothing is
    // published for this CPU and when everything is already installed — the happy path —
    // so the store question is asked first, before the network ([`seedless_serves_us`]).
    // A vendor-direct program still wanted here answers it as a build does: its lane
    // installs it with no index.
    //
    // `seed_serviceable` answers "what is left to install HERE", so it returns empty
    // for two opposite states: nothing is published for this CPU, and everything is
    // already installed. The second one is the product's happy path, and reading empty as
    // "unsupported architecture" told every fully provisioned Apple Silicon Mac that
    // its own architecture was unserved, once per launch, forever: a stdout marker, a
    // GUI warning pill, a `*toolset*` row reading "unavailable", and a Settings detail
    // line saying no build exists. Ask the other question
    // first — and BEFORE the network, because its answer decides whether the resolve
    // can matter at all (see [`seedless_serves_us`]). A vendor-direct program still
    // wanted here answers it as a build does: its lane installs it with no index.
    //
    // AND THE WINDOW'S OWN SEED ASKS NO INDEX (a `--wait-lock` caller, [`wait_lock`]): the
    // update the window runs right behind it resolves and verifies the same index, and its
    // empty-store arm says the same verdict — the same `seed-unusable:` line, the same
    // `*toolset*` row ([`run_update_pass`]). Resolving it here too was the whole network
    // twice per launch: a discovery walk, the index quads and every manifest — 77 requests,
    // about a second (9-16 s from a sequential fetcher) under the store lock before the
    // install could start, on a store whose config wants no vendor program. A typed
    // `aterm pkg seed` still resolves, and still says it.
    let served_here = !crate::active_builds(layout).is_empty()
        || vendor_set_wanted_on(triple, cfg, &removed_programs(layout));
    // …AND ON A LAUNCH NO UPDATE FOLLOWS — none is due, the last pass's success younger than
    // the walk — it says the verdict that pass recorded, from the record: an Intel Mac or a
    // toolset excluded to nothing gets its card at every launch, not only at those an
    // update runs on ([`recorded_unusable_body`]).
    if window_pass
        && !served_here
        && aterm_update_core::pkg_check::launch_due_in(
            &crate::status::pass_stamps(layout, now),
            now,
        ) > 0
        && let Some(body) = recorded_unusable_body(layout, cfg, read_cached_index)
    {
        emit_marker_line(&seed_unusable_line(&body));
    }
    let served_without_index = served_here || window_pass;
    let serves_us = seedless_serves_us(served_without_index, || {
        let fetcher = resolve_fetcher(layout);
        crate::resolve_verified_index(
            &*fetcher,
            layout,
            &effective_anchor(layout),
            build_floor(layout),
            now_unix(),
        )
        .ok()
        .map(|index| index_serves(layout, &*fetcher, &index, cfg))
    });
    // The durable trace, in the same words the marker carries.
    match serves_us {
        Some(IndexServes::Unserved(served)) => {
            let why = unserved_sentence(triple, &served);
            emit_marker_line(&seed_unusable_line(&why));
            record_unusable_toolset(layout, crate::state::TOOLSET_UNSERVED, &why);
        }
        // Not the CPU: the user's own exclusion or removals emptied a set the index serves.
        Some(IndexServes::Narrowed(narrowing)) => {
            let (why, state) = narrowing.verdict(triple);
            emit_marker_line(&seed_unusable_line(&why));
            record_unusable_toolset(layout, state, &why);
        }
        // Said only while the store is empty, and only to whoever ran the verb
        // ([`says_not_installed_yet`]).
        Some(IndexServes::Yes) | None
            if says_not_installed_yet(crate::active_builds(layout).is_empty(), window_pass) =>
        {
            println!(
                "atpkg: the ALab toolset is not installed yet \u{2014} it downloads by itself; \
                 to start now: aterm pkg install --default-set"
            );
        }
        Some(IndexServes::Yes) | None => {}
    }
}

/// The `seed-unusable:` marker line over `body` — one spelling for the seed and the
/// update's empty-store arm, so the window's card reads the same from either.
fn seed_unusable_line(body: &str) -> String {
    format!("atpkg: {SEED_UNUSABLE_MARKER}{body}")
}

/// What an empty store's pass outcome says before the verdict that nothing here can install
/// ([`record_unusable_toolset`]).
const NOTHING_INSTALLABLE_LEAD: &str = "nothing is installed and nothing was installable \u{2014} ";

/// Record an empty store's `seed-unusable:` verdict — an unserved architecture, a toolset
/// narrowed to nothing — as its `*toolset*` row (`state`) and the pass outcome, which says the
/// verdict (`why`, the marker's body) after [`NOTHING_INSTALLABLE_LEAD`]: the seed and the
/// update's empty-store arm write it alike, and the window's seed says it again from here on
/// a launch no update follows ([`recorded_unusable_body`]).
fn record_unusable_toolset(layout: &crate::store::Layout, state: &str, why: &str) {
    record_unbuilt(
        layout,
        "*toolset*",
        state,
        format!("{NOTHING_INSTALLABLE_LEAD}{why}"),
    );
}

/// The `seed-unusable:` body the record still carries ([`record_unusable_toolset`]): the
/// `*toolset*` row an unserved architecture or a toolset narrowed to nothing, and the pass
/// outcome its verdict — `None` once a later pass's outcome moved on, or for any other row.
/// An exclusion/removal verdict also needs the local verified index to remain empty under
/// the current exclusions and removals; this replay never fetches package manifests.
fn recorded_unusable_body(
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    read_cached_index: impl FnOnce() -> Option<crate::TrustedIndex>,
) -> Option<String> {
    let status = crate::status::read(layout)?;
    let state = status.programs.get("*toolset*")?.state.as_str();
    if ![
        crate::state::TOOLSET_UNSERVED,
        crate::state::TOOLSET_EXCLUDED,
        crate::state::TOOLSET_REMOVED,
    ]
    .contains(&state)
    {
        return None;
    }
    let body = status.outcome.strip_prefix(NOTHING_INSTALLABLE_LEAD)?;
    if state != crate::state::TOOLSET_UNSERVED {
        // An old exclusion/removal verdict does not stand for today's policy. Read only
        // the locally verified index; a missing/refused cache leaves this launch quiet.
        let index = read_cached_index()?;
        let removed = removed_programs(layout);
        if wanted_programs(&index, cfg)
            .iter()
            .any(|program| !removed.contains(program))
        {
            return None;
        }
    }
    Some(body.to_string())
}

/// The unserved-architecture `seed-unusable:` body: the need, this machine's triple, and
/// the triples ALab does build for.
fn unserved_sentence(triple: &str, served: &[String]) -> String {
    format!(
        "ALab publishes no build {UNSERVED_ARCHITECTURE_CLAUSE} ({triple}) \u{2014} {}nothing \
         installs here until one is published",
        builds_for_clause(served)
    )
}

/// Whether the seed verb tells its reader the ALab toolset "is not installed yet": only over
/// an empty store, and never under the window's own pass (`window_pass`: a `--wait-lock`
/// caller, [`wait_lock`]), which downloads the set itself right behind this verb.
fn says_not_installed_yet(store_empty: bool, window_pass: bool) -> bool {
    store_empty && !window_pass
}

/// Whether the signed index serves this machine, for the seedless lane's one sentence: the
/// index's answer ([`IndexServes`]), or `None` when it never resolved. `served_without_index`
/// (a store that holds any build, one a vendor-direct program is still to land in
/// ([`vendor_set_wanted_on`]), or the window's own seed, whose update resolves next) answers
/// yes, so `resolve` (a discovery walk, the cache parse, the verification, under the store
/// lock `seed` holds — offline, up to three 30 s attempts) runs only otherwise.
fn seedless_serves_us(
    served_without_index: bool,
    resolve: impl FnOnce() -> Option<IndexServes>,
) -> Option<IndexServes> {
    if served_without_index {
        Some(IndexServes::Yes)
    } else {
        resolve()
    }
}

/// Retire a legacy `*seed*` row and nothing else — the row, not the aggregate `outcome`
/// sentence, which belongs to the pass that just ran. The row was an older build's
/// `auto_install = false` offer ("pending-consent: <programs> — …"), and `record_status`
/// merges the program map, so it would otherwise stand in Settings ▸ Packages forever.
/// Swept by the seed verb at every launch (the one lane that reaches an `auto_install =
/// false` machine) and by a clean set-completing pass or explicit toolset install.
fn retire_seed_row(layout: &crate::store::Layout) {
    let Some(mut status) = crate::status::read(layout) else {
        return;
    };
    if status.programs.remove("*seed*").is_none() {
        return;
    }
    status.updated_at = now_rfc3339();
    let _ = crate::status::write(layout, &status);
}

/// Reconcile the `[packages.links]` config at the CLI edge of every network verb: a
/// local-checkout value becomes a managed dev-link ([`crate::linkmode`], idempotent), while
/// a hand-made link pointing at a different checkout is refused loudly and never touched. A
/// missing checkout is loud; `owner/repo` values are the fetcher's ([`github_fetcher`]).
/// Never fatal to the update pass.
///
/// The foreign-link refusal is check-then-act (`linked_checkout` read → `crate::link`
/// write), sound only because every caller is a store-mutating verb holding the store lock
/// from the dispatch edge. Never call this from a lock-free verb.
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
                        match crate::link(layout, program, &want, &link_bins_for(program, &want)) {
                            Ok(out) => println!(
                                "atpkg: dev-linked {program} from [packages.links] ({})",
                                out.linked.join(", ")
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

/// The bins a link of `checkout` with no explicit bin list names: a PACKAGE DIRECTORY of an
/// ALab-built program (`alab/package.sh`'s output, [`crate::alab::package_link_bin`]) by its
/// entrypoint (`bin/codex`), so `[packages.links] codex = "<pkgdir>"` links it as
/// `aterm pkg link codex <pkgdir> bin/codex` does; anything else by [`default_link_bins`].
fn link_bins_for(program: &str, checkout: &std::path::Path) -> Vec<std::path::PathBuf> {
    crate::alab::package_link_bin(program, checkout)
        .map_or_else(|| default_link_bins(program), |entry| vec![entry])
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

/// The checkout-relative bins to dev-link when `checkout` is a sysroot rather than a cargo
/// project: every name the installed (signed) manifest exposes, under `bin/`. Each Trust
/// command finds its `lib/rustlib` by walking up from the binary it was invoked as, so the
/// unit is the root, not `target/release/<program>`; the manifest, not `bin/`'s listing,
/// decides the set, so an extra binary gains no shim and a missing one fails. `None` when
/// this is not a sysroot link.
fn sysroot_link_bins(
    layout: &crate::store::Layout,
    program: &str,
    checkout: &std::path::Path,
) -> Option<Result<Vec<std::path::PathBuf>, Vec<String>>> {
    // A sysroot is bin/ + lib/rustlib; requiring both keeps an ordinary cargo checkout that
    // happens to have a `bin/` directory on the single-binary path.
    if !checkout.join("bin").is_dir() || !checkout.join("lib").join("rustlib").is_dir() {
        return None;
    }
    let exposes = crate::installed_exposes(layout, program)?;
    if exposes.is_empty() {
        return None;
    }
    // Completeness is the point: linking only the names a dev build carries leaves the rest
    // on the registry build — two toolchains with different sysroots mixed on one PATH. So
    // a missing exposed name aborts the link; the user builds the missing tools or names an
    // explicit rel-bin list for a partial link.
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

/// `atpkg update <program>` — update one program. A coherence-group member goes through the
/// transactional [`crate::flow::apply_program`], so its whole tuple stages, flips and rolls
/// back atomically; an ungrouped program keeps the single-program install path. A
/// read-only [`crate::flow::plan_update`] picks the path and yields the authoritative
/// `decide()` result, so the ungrouped local-pin gate applies strictly after it (never
/// hiding a Tombstone).
fn cmd_update_one(program: &String) -> ExitCode {
    if !manager_enabled() {
        eprintln!("atpkg: disabled (no root key pinned) — nothing to update");
        return ExitCode::from(1);
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let cfg = crate::config::cached();
    reconcile_links(&layout, cfg);
    // One program's update under a person's meter ([`TypedPass`]); the install it may
    // route to runs inside it.
    let _typed = TypedPass::begin("update");
    // The head watch (a window's, or a terminal session's) names a vendor program it saw
    // move; nobody typed it.
    if let Some(spec) = crate::vendor_direct::spec(program)
        && head_watch_pass()
    {
        // The head watch names a vendor's head; a program the index lane owns here
        // (an ALab build pinned for this target, [`crate::alab`]) does not follow it.
        if crate::alab::index_owns(
            &layout,
            program,
            current_triple(),
            cached_index(&layout).as_deref(),
        ) {
            println!("atpkg: {program} follows the ALab channel here — nothing to update");
            return ExitCode::SUCCESS;
        }
        return cmd_update_vendor(&layout, spec, VendorDoor::Window);
    }
    let installed = crate::active_builds(&layout);
    if !installed.contains_key(program) {
        // Update of a not-installed program = a fresh explicit install (single path).
        return cmd_install_for_update(program);
    }
    // A vendor-direct program moves with its vendor, never behind the index resolve —
    // unless the source rule hands it to the index lane here ([`index_owns_now`], which asks
    // an index only for an ALab-built program on a target ALab builds it for).
    if let Some(spec) = crate::vendor_direct::spec(program)
        && !(crate::alab::agent(program).is_some_and(|a| a.serves(current_triple()))
            && index_owns_now(&layout, &*resolve_fetcher(&layout), program))
    {
        return cmd_update_vendor(&layout, spec, VendorDoor::Person);
    }
    let cur = installed.get(program).copied();
    let floor = build_floor(&layout);
    let fetcher = resolve_fetcher(&layout);
    let plan = match crate::flow::plan_update(
        &*fetcher,
        &layout,
        &effective_anchor(&layout),
        crate::config::CHANNEL,
        current_triple(),
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
        // Coherence-group member: the transactional path. The pin is consulted inside
        // apply_program's per-group gate.
        let report = match crate::flow::apply_program(
            &*fetcher,
            &layout,
            &effective_anchor(&layout),
            crate::config::CHANNEL,
            current_triple(),
            program,
            &installed,
            // Same wire as cmd_update_all: an excluded absent sibling must not ride back
            // in on this program's coherence tuple.
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
        let failures = report_channel_apply(&layout, &installed, &report, &[], false);
        for p in &report.skipped_linked {
            println!("atpkg: {p} dev-linked — skipped");
        }
        // GC after every successful activate, then the shell hooks, as `cmd_update_all` and
        // `do_install` do. The sparing form keeps a failed member's `.part`; under the
        // channel the plan just verified (this verb never refreshes the cache), so a member
        // moved off a yanked build keeps the build `aterm pkg rollback` lands on.
        let gc = crate::gc::run_keeping_pinned_partials(&layout, Some(&plan.channel), &|p| {
            report.resolved_assets.get(p).cloned()
        });
        print_gc_sweeps("update", &gc);
        print_gc_abstentions("update", &gc);
        // The seam and the exec roots, as `cmd_update_all_code` does after its gc: this is
        // the branch `aterm pkg update trust` takes (trust is in the `rustc` group). Order
        // matches the pass: gc, the roots, the shell hook.
        reassert_rustup_seam(&layout);
        let _ = reconcile_exec_roots(&layout, crate::seam::Depth::Shallow);
        refresh_shell_hooks(&layout);
        return if failures == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        };
    }
    // Ungrouped: the local pin gate, strictly after the authoritative `decide()`, and
    // suppression-only — only an Install (upgrade) is held. The pin holds only while the
    // active build is itself still gate-valid (`plan.current_build_ok`): if it was
    // yanked/floored, `decide()` returned Install to force the upgrade off it, and honoring
    // the pin would keep a revoked build running.
    if decision == crate::ApplyDecision::Install
        && plan.current_build_ok
        && let Some(c) = cur
        && crate::pin::is_pinned(&layout, program)
    {
        println!(
            "atpkg: {program} not updated — you pinned it at build {c} (`aterm pkg unpin \
             {program}` lets it update)"
        );
        return ExitCode::SUCCESS;
    }
    if decision == crate::ApplyDecision::Install
        && cur.is_some()
        && crate::pin::is_pinned(&layout, program)
    {
        // Pinned but the current build is no longer gate-valid: force the upgrade and say so.
        eprintln!(
            "atpkg: {program} is pinned, but ALab withdrew its current build — updating off \
             it anyway"
        );
    }
    // Re-enter the single-program install path with the fetcher THIS verb already built
    // and already warmed (token chain + index candidates), instead of `cmd_install`'s
    // second one. Everything `cmd_install` would re-do first — `manager_enabled`,
    // `layout()`, `cfg` — ran above with the same answers, and `reconcile_links` still
    // runs inside `cmd_install_with` exactly as before, so the output is unchanged.
    let before = crate::active_builds(&layout);
    let code = cmd_install_with(program, &layout, cfg, &*fetcher, "update");
    warm_alab_landed(&layout, &before, crate::warm::spawn_landed);
    code
}

/// Whether the source rule hands ALab-built `program` to the index lane here
/// ([`crate::alab::source`]), under the index a person's door verifies now (the fetcher
/// memoizes the resolve, so the flow that follows asks no second time). `false` without a
/// verified index, for a program ALab does not build, and off the targets it builds for —
/// no index is asked then.
fn index_owns_now(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    program: &str,
) -> bool {
    crate::alab::agent(program).is_some_and(|agent| agent.serves(current_triple()))
        && crate::resolve_verified_index(
            fetcher,
            layout,
            &effective_anchor(layout),
            build_floor(layout),
            now_unix(),
        )
        .ok()
        .is_some_and(|index| {
            crate::alab::index_owns(layout, program, current_triple(), Some(&index))
        })
}

/// Who asked for `atpkg update <vendor program>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VendorDoor {
    /// A person: the door forgets the program's refusals, installs a program that is not
    /// installed, and an unreached vendor fails the verb.
    Person,
    /// The head watch (`--head-watch`), a window's or a terminal session's: the update
    /// pass's own unattended lane, for this one installed program.
    Window,
}

/// `atpkg update <vendor program>`: the vendor lane alone, under the local pin, its one line
/// on stdout — or stderr, exit 1, when it refused, or when a person asked and it could not
/// check (unreachable).
fn cmd_update_vendor(
    layout: &crate::store::Layout,
    spec: &'static crate::vendor_direct::VendorSpec,
    door: VendorDoor,
) -> ExitCode {
    let fetcher = resolve_fetcher(layout);
    if update_vendor(layout, &*fetcher, spec, door, &PassSeams::process()) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// [`cmd_update_vendor`]'s body, over the fetcher and seams it is handed: whether the
/// verb succeeds.
fn update_vendor(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    spec: &'static crate::vendor_direct::VendorSpec,
    door: VendorDoor,
    seams: &PassSeams<'_>,
) -> bool {
    let program = spec.program;
    let person = door == VendorDoor::Person;
    let installed = crate::active_builds(layout);
    if !person
        && !installed.contains_key(program)
        && !crate::flow::tombstoned_in_place(layout, [program])
    {
        println!("atpkg: {program} is not installed — nothing to update");
        return true;
    }
    if person {
        // A person asking is the event that may mean "fixed, try now": the door forgets
        // refusals, as the index lane's does.
        crate::store::clear_stage_refusals(layout, program);
        let _ = crate::vendor_direct::ProgramStamp::clear_refusal(layout, program);
    }
    let now = (seams.now)();
    // Both doors refresh a latch over an hour old first, so a yank published since the
    // last pass binds them; a resolve that fails leaves them the latch, never a failure.
    let policy = crate::flow::door_vendor_policy(
        fetcher,
        layout,
        &(seams.anchor)(layout),
        build_floor(layout),
        now,
    );
    let legacy = legacy_index_for(layout, &installed);
    let lane = crate::vendor_direct::lane::Lane {
        layout,
        fetcher,
        triple: current_triple(),
        policy: &policy,
        trust: seams.trust,
        legacy_index: legacy.as_ref(),
        asked: person,
        now,
        download_turn: None,
    };
    let held = crate::pin::is_pinned(layout, program);
    let outcome = crate::vendor_direct::lane::install_one(&lane, spec, held);
    let mut pass = VendorPass::default();
    report_vendor(layout, &outcome, &mut pass, person, now);
    warm_landed(layout, &mut pass, seams);
    let _ = settle_vendor_pass(layout, &pass, seams);
    print_managed_current(layout, &pass.unchecked, now);
    // A deferred signature check is no failure for the window's door, but a person who
    // asked for the move did not get it, exactly as for an unreached vendor.
    let unchecked = matches!(
        outcome.verdict,
        crate::vendor_direct::lane::Verdict::Unreachable { .. }
            | crate::vendor_direct::lane::Verdict::Deferred { .. }
    );
    !(outcome.is_failure() || (unchecked && person))
}

/// What the vendor lane of a pass did: its faults (each failed program with its line),
/// the staging names it resolved (the pass-end gc spares their `.part`), the programs it
/// ran, whether any build moved, the programs whose vendor it could not reach, whether any
/// vendor's channel answered ([`pass_reached_nothing`]), and the programs it landed a new
/// build of, not yet warmed ([`warm_landed`]).
#[derive(Default)]
struct VendorPass {
    failures: u32,
    failed: Vec<(String, String)>,
    resolved_assets: std::collections::BTreeMap<String, String>,
    ran: std::collections::BTreeSet<String>,
    changed: bool,
    unchecked: std::collections::BTreeSet<String>,
    reached: bool,
    landed: std::collections::BTreeSet<String>,
}

impl VendorPass {
    /// Fold in what another part of the same lane did ([`RiderLanded::pass`]).
    fn absorb(&mut self, other: Self) {
        self.failures += other.failures;
        self.failed.extend(other.failed);
        self.resolved_assets.extend(other.resolved_assets);
        self.ran.extend(other.ran);
        self.changed |= other.changed;
        self.unchecked.extend(other.unchecked);
        self.reached |= other.reached;
        self.landed.extend(other.landed);
    }
}

/// Note `outcome`'s program as landed when the lane installed a new build of it — never a
/// rollback onto a build that already ran here, a tombstone or anything that kept one.
fn note_landed(pass: &mut VendorPass, outcome: &crate::vendor_direct::lane::Outcome) {
    if matches!(
        outcome.verdict,
        crate::vendor_direct::lane::Verdict::Installed { .. }
    ) {
        pass.landed.insert(outcome.spec.program.to_string());
    }
}

/// Warm the builds `pass` landed since it last warmed ([`crate::warm`], through `seams`)
/// and forget them, so no later exit of the same lane warms them again.
fn warm_landed(layout: &crate::store::Layout, pass: &mut VendorPass, seams: &PassSeams<'_>) {
    let landed = std::mem::take(&mut pass.landed);
    if !landed.is_empty() {
        (seams.warm_agents)(layout, &landed);
    }
}

/// Warm what the INDEX lane landed of an ALab-built agent program ([`crate::alab`]) since
/// `before` was read: a new active ALab build is warmed as the vendor lane warms its own
/// landing ([`crate::warm`]), so codex-alab's first launch reads a fresh model catalogue
/// exactly as a vendor codex's does.
fn warm_alab_landed(
    layout: &crate::store::Layout,
    before: &std::collections::BTreeMap<String, u64>,
    warm: fn(&crate::store::Layout, &std::collections::BTreeSet<String>),
) {
    let now = crate::active_builds(layout);
    let landed: std::collections::BTreeSet<String> = crate::alab::ALAB_AGENTS
        .iter()
        .map(|agent| agent.program)
        .filter(|program| {
            now.get(*program).is_some_and(|build| {
                before.get(*program) != Some(build)
                    && crate::alab::build_is_alab(layout, program, *build)
            })
        })
        .map(str::to_string)
        .collect();
    if !landed.is_empty() {
        warm(layout, &landed);
    }
}

/// After a vendor lane moved a build with no index lane to follow it (a vendor door, or a
/// pass whose index lane could not run): the gc that keeps live + 1, confined to the
/// programs the lane ran — it knows no other program's pins, so it never sweeps another
/// program's `.part` or carried archive, nor reclaims its builds — then the shell hooks.
/// The gc report; `None` when nothing moved.
fn settle_vendor_pass(
    layout: &crate::store::Layout,
    pass: &VendorPass,
    seams: &PassSeams<'_>,
) -> Option<crate::gc::GcReport> {
    if !pass.changed {
        return None;
    }
    let report =
        crate::gc::run_for_programs(layout, &pass.ran, &|p| pass.resolved_assets.get(p).cloned());
    (seams.shell_hooks)(layout);
    Some(report)
}

/// The pass's outcome sentence once its index lane applied clean: `clean`, unless the
/// vendor lane failed a program — then that program's own line, or for several the count
/// and names. A vendor refusal is never headlined as an index verdict.
fn vendor_folded_outcome(vendor: &VendorPass, clean: String) -> String {
    match vendor.failed.as_slice() {
        [] => clean,
        [(_, said)] => said.clone(),
        many => {
            let names: Vec<String> = many.iter().map(|(p, _)| p.clone()).collect();
            failed_programs_outcome(&names)
        }
    }
}

/// `N program(s) failed: a, b — see their rows`: the outcome of a pass that failed more
/// than one program.
fn failed_programs_outcome(names: &[String]) -> String {
    format!(
        "{} program(s) failed: {} — see their rows",
        names.len(),
        names.join(", ")
    )
}

/// Which halves of the vendor lane a pass runs.
#[derive(Clone, Copy)]
struct VendorLanes {
    /// Move every installed vendor program with its vendor (the update pass).
    update_installed: bool,
    /// Install every wanted vendor program not installed yet.
    complete_the_set: bool,
}

/// Whether `[packages].exclude` and this machine's removals leave `program` wanted.
pub(crate) fn vendor_wanted(
    cfg: &crate::config::PackagesConfig,
    removed: &std::collections::BTreeSet<String>,
    program: &str,
) -> bool {
    !cfg.exclude().iter().any(|p| p == program) && !removed.contains(program)
}

/// Whether a vendor-direct program is still wanted on `triple`: its vendor serves it there
/// and neither `[packages].exclude` nor an uninstall took it out ([`vendor_wanted`]). Its
/// lane needs no index, so the seed's sentence never calls such a machine unserved, or
/// narrowed to nothing, when the index part is empty.
fn vendor_set_wanted_on(
    triple: &str,
    cfg: &crate::config::PackagesConfig,
    removed: &std::collections::BTreeSet<String>,
) -> bool {
    crate::vendor_direct::VENDORS
        .iter()
        .any(|spec| spec.serves(triple) && vendor_wanted(cfg, removed, spec.program))
}

/// The cached verified index, read only when an active vendor program is a legacy index
/// build whose version no pass has kept yet, or one whose rollback target has no signed
/// root held ([`crate::vendor_direct::lane::legacy_root_owed`]); the lane keeps what it
/// reads in the program's stamp.
fn legacy_index_for(
    layout: &crate::store::Layout,
    installed: &std::collections::BTreeMap<String, u64>,
) -> Option<crate::TrustedIndex> {
    installed
        .iter()
        .any(|(p, b)| {
            crate::vendor_direct::is_vendor(p)
                && ((!crate::vendor_direct::is_vendor_build(*b)
                    && crate::vendor_direct::ProgramStamp::read(layout, p)
                        .and_then(|s| s.legacy_version_of(*b))
                        .is_none())
                    || crate::vendor_direct::lane::legacy_root_owed(layout, p, Some(*b)).is_some())
        })
        .then(|| cached_index(layout))
        .flatten()
}

/// THE VENDOR LANE OF A PASS (design §1.5), run after the index resolve and normally
/// before its apply; a verified published-index hint gives the apply priority. Every
/// installed vendor program moves with its vendor (the local pin holds it), one
/// tombstoned in place included, and with `complete_the_set` every wanted one not
/// installed arrives — announced first with `net-starting:` and the sizes its verified
/// documents state, metered on the window's progress file, answered with
/// `net-installed:` or `net-failed:`. Its bytes are only the vendors'. ([`vendor_plan`],
/// then [`land_fresh_alone`].) The tests' door to the lane alone: the passes compose those
/// two themselves, since a pass that installs the set on an empty store lands the fresh ones
/// beside its set completion instead ([`VendorRider`]).
#[cfg(all(test, unix))]
#[allow(
    clippy::too_many_arguments,
    reason = "the pass's own inputs plus the index it verified first, which only narrows"
)]
fn vendor_pass(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    cfg: &crate::config::PackagesConfig,
    installed: &std::collections::BTreeMap<String, u64>,
    lanes: VendorLanes,
    consent: Consent,
    fresh: Option<&crate::TrustedIndex>,
    seams: &PassSeams<'_>,
) -> VendorPass {
    let (mut pass, fresh) = vendor_plan(layout, fetcher, cfg, installed, lanes, fresh, seams);
    land_fresh_alone(layout, fetcher, consent, &fresh, &mut pass, seams, false);
    warm_landed(layout, &mut pass, seams);
    pass
}

/// The vendor lane short of its fresh landings: every vendor program it runs is planned
/// (side by side when the fetcher may be shared, since their heads and documents live on
/// different hosts), each installed one is then landed in turn as its vendor moved it, and
/// the wanted ones not installed come back as verified candidates whose bytes have not
/// moved, for one announcement before any of them does.
fn vendor_plan(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    cfg: &crate::config::PackagesConfig,
    installed: &std::collections::BTreeMap<String, u64>,
    lanes: VendorLanes,
    fresh: Option<&crate::TrustedIndex>,
    seams: &PassSeams<'_>,
) -> (VendorPass, FreshVendors) {
    vendor_plan_reading(layout, fetcher, cfg, installed, lanes, fresh, seams, None)
}

/// What an update pass's vendor lane read before its index landed ([`read_vendors_ahead`]),
/// and until when its plan waits for a read still out: for as long as it takes (`None`), or —
/// on a first run, whose set's announcement waits for the plan — [`VENDOR_READ_GRACE`] past
/// the index.
#[derive(Clone, Copy)]
struct ReadsAhead<'r> {
    reads: &'r crate::vendor_direct::lane::Reads,
    until: Option<std::time::Instant>,
}

/// How long a first run's vendor plan waits, once its index is in, for a vendor's read still
/// out ([`ReadsAhead`]). The reads go out with the index resolve and are normally in before
/// it (four hosts, a head and two or three documents each, beside three discovery rounds and
/// the quads); one still out past this is a host that does not answer — a name that does not
/// resolve, a connect that never finishes, each bounded at five seconds an attempt — and its
/// program lands after the set, alone ([`FreshVendors::late`]), instead of holding the set's
/// announcement and first byte behind it.
const VENDOR_READ_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// [`vendor_plan`] over what `ahead` read before the index landed: each program's plan takes
/// its read instead of asking the hosts again, and one whose read is still out at
/// `ahead.until` is left to plan after the set ([`FreshVendors::late`]).
#[allow(
    clippy::too_many_arguments,
    reason = "vendor_plan's inputs and the reads ahead of it"
)]
fn vendor_plan_reading(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    cfg: &crate::config::PackagesConfig,
    installed: &std::collections::BTreeMap<String, u64>,
    lanes: VendorLanes,
    fresh: Option<&crate::TrustedIndex>,
    seams: &PassSeams<'_>,
    ahead: Option<ReadsAhead<'_>>,
) -> (VendorPass, FreshVendors) {
    use crate::vendor_direct::lane::{self, Step};
    // The yanks of the index this pass verified fresh (the verification also refreshed the
    // latch), else the latch: a yank published since the last pass binds this one.
    let mut vendors = FreshVendors {
        pending: Vec::new(),
        policy: crate::vendor_direct::policy::Policy::current(layout, fresh),
        index: match fresh {
            Some(index) => Some(index.clone()),
            None => legacy_index_for(layout, installed),
        },
        now: (seams.now)(),
        late: Vec::new(),
    };
    let removed = removed_programs(layout);
    // `(program, installed, held)` for each program this lane runs. A tombstone is silent
    // in the shim view but still this machine's install: the update lane revisits it, or it
    // would wait on a person forever.
    let runs: Vec<(&'static crate::vendor_direct::VendorSpec, bool, bool)> =
        crate::vendor_direct::VENDORS
            .iter()
            .filter_map(|spec| {
                let program = spec.program;
                // THE SOURCE RULE ([`crate::alab::source`]): a program the index pins from
                // ALab's repo for this target is the index lane's this pass, never this one's.
                if crate::alab::source(layout, program, current_triple(), fresh.map(|i| &**i))
                    .index_lane()
                {
                    return None;
                }
                if lanes.update_installed
                    && (installed.contains_key(program)
                        || crate::flow::tombstoned_in_place(layout, [program]))
                {
                    Some((spec, true, crate::pin::is_pinned(layout, program)))
                } else if !installed.contains_key(program)
                    && lanes.complete_the_set
                    && vendor_wanted(cfg, &removed, program)
                {
                    Some((spec, false, false))
                } else {
                    None
                }
            })
            .collect();
    // Each program's plan over its read ahead, or `None` when that read is still out.
    let plan = |lane: &lane::Lane<'_>, spec: &'static crate::vendor_direct::VendorSpec, held| {
        match ahead {
            Some(ReadsAhead { reads, until }) => reads
                .take_read(spec.program, until)
                .ok()
                .map(|read| lane::plan_one_with(lane, spec, held, read)),
            None => Some(lane::plan_one(lane, spec, held)),
        }
    };
    let steps: Vec<Option<Step>> = match fetcher.shared() {
        Some(shared) => crate::net::fan_out(&runs, &|(spec, _, held)| {
            plan(&vendors.lane(layout, shared, seams.trust), spec, *held)
        }),
        None => {
            let lane = vendors.lane(layout, fetcher, seams.trust);
            runs.iter()
                .map(|(spec, _, held)| plan(&lane, spec, *held))
                .collect()
        }
    };
    let mut pass = VendorPass::default();
    let mut pending = Vec::new();
    let mut late = Vec::new();
    {
        let lane = vendors.lane(layout, fetcher, seams.trust);
        for ((spec, is_installed, held), step) in runs.iter().zip(steps) {
            let Some(step) = step else {
                late.push((*spec, *held));
                continue;
            };
            match step {
                Step::Done(outcome) => {
                    report_vendor(layout, &outcome, &mut pass, false, vendors.now);
                }
                Step::Land(landing) if *is_installed => {
                    let outcome = lane::land_one(&lane, &landing);
                    report_vendor(layout, &outcome, &mut pass, false, vendors.now);
                }
                Step::Land(landing) => pending.push(*landing),
            }
        }
    }
    vendors.pending = pending;
    vendors.late = late;
    (pass, vendors)
}

/// Send the vendor lane's network reads out beside the pass's index resolve: each program the
/// lane may plan this pass ([`vendor_plan`]'s, short of the one test that needs the index —
/// whether the index pins it from ALab's repo) read ahead on a thread of its own in `scope`
/// ([`crate::vendor_direct::lane::read_ahead`]), into `reads`, where its plan takes it. Every
/// other program's slot reads nothing, so its plan reads for itself. Whether any read went
/// out: none does with a fetcher the pass may not share, and the lane then plans as it
/// always did.
#[allow(
    clippy::too_many_arguments,
    reason = "the pass's own inputs and the scope and slots its reads run in"
)]
fn read_vendors_ahead<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    reads: &'env crate::vendor_direct::lane::Reads,
    layout: &'env crate::store::Layout,
    fetcher: &'env dyn crate::flow::Fetcher,
    cfg: &crate::config::PackagesConfig,
    installed: &std::collections::BTreeMap<String, u64>,
    complete_the_set: bool,
    seams: &PassSeams<'env>,
) -> bool {
    let Some(shared) = fetcher.shared() else {
        return false;
    };
    let removed = removed_programs(layout);
    let (trust, now, read_in) = (seams.trust, (seams.now)(), seams.read_ahead_in);
    for spec in crate::vendor_direct::VENDORS {
        let program = spec.program;
        let planned = installed.contains_key(program)
            || crate::flow::tombstoned_in_place(layout, [program])
            || (complete_the_set && vendor_wanted(cfg, &removed, program));
        let spawned = planned
            && std::thread::Builder::new()
                .name("atpkg-vendor-ahead".into())
                .spawn_scoped(scope, move || {
                    reads.read(program, || {
                        crate::vendor_direct::lane::read_ahead(
                            layout,
                            spec,
                            shared,
                            current_triple(),
                            trust,
                            now,
                        )
                    });
                    read_in(program);
                })
                .is_ok();
        if !spawned {
            reads.read(program, || None);
            read_in(program);
        }
    }
    true
}

/// The programs a first run's vendor plan left behind, their reads still out when its set
/// announced ([`FreshVendors::late`]): each read waited for now, planned under the index the
/// pass verified, and landed alone after the set — announced, metered and answered on their
/// own, the promise going with their plan ([`land_fresh_alone`]).
#[allow(
    clippy::too_many_arguments,
    reason = "the pass's inputs the late plans and their landings read"
)]
fn land_late_vendors(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    consent: Consent,
    reads: &crate::vendor_direct::lane::Reads,
    late: &[(&'static crate::vendor_direct::VendorSpec, bool)],
    index: Option<&crate::TrustedIndex>,
    pass: &mut VendorPass,
    seams: &PassSeams<'_>,
) {
    use crate::vendor_direct::lane::{self, Step};
    let mut vendors = FreshVendors {
        pending: Vec::new(),
        policy: crate::vendor_direct::policy::Policy::current(layout, index),
        index: index.cloned(),
        now: (seams.now)(),
        late: Vec::new(),
    };
    let steps: Vec<Step> = late
        .iter()
        .map(|(spec, held)| {
            let read = reads.take_read(spec.program, None).ok().flatten();
            lane::plan_one_with(
                &vendors.lane(layout, fetcher, seams.trust),
                spec,
                *held,
                read,
            )
        })
        .collect();
    for step in steps {
        match step {
            Step::Done(outcome) => report_vendor(layout, &outcome, pass, false, vendors.now),
            Step::Land(landing) => vendors.pending.push(*landing),
        }
    }
    land_fresh_alone(layout, fetcher, consent, &vendors, pass, seams, true);
}

/// The vendor lane's fresh landings once planned ([`vendor_plan`]): verified candidates
/// whose bytes have not moved, in table order, with what landing them reads.
struct FreshVendors {
    pending: Vec<crate::vendor_direct::lane::Pending>,
    /// The yanks the lane binds ([`crate::vendor_direct::policy::Policy::current`]).
    policy: crate::vendor_direct::policy::Policy,
    /// The verified index a legacy build's version is read from: the one this pass verified
    /// fresh, else the cached one, read only when a legacy build needs it
    /// ([`legacy_index_for`]).
    index: Option<crate::TrustedIndex>,
    /// The pass's clock.
    now: i64,
    /// The programs whose read ahead was still out when the plan stopped waiting
    /// ([`ReadsAhead`]), with their local pin: planned, and landed alone, once the set
    /// completion is done ([`land_late_vendors`]).
    late: Vec<(&'static crate::vendor_direct::VendorSpec, bool)>,
}

impl FreshVendors {
    /// The lane over `fetcher`: the same store, yanks, anchors and clock whichever thread
    /// works it.
    fn lane<'a>(
        &'a self,
        layout: &'a crate::store::Layout,
        fetcher: &'a dyn crate::flow::Fetcher,
        trust: &'a crate::vendor_direct::lane::Trust,
    ) -> crate::vendor_direct::lane::Lane<'a> {
        crate::vendor_direct::lane::Lane {
            layout,
            fetcher,
            triple: current_triple(),
            policy: &self.policy,
            trust,
            legacy_index: self.index.as_ref(),
            asked: false,
            now: self.now,
            download_turn: None,
        }
    }

    /// The programs, in table order.
    fn names(&self) -> Vec<String> {
        self.pending
            .iter()
            .map(|p| p.program().to_string())
            .collect()
    }

    /// Each program with its exact download size: its rows of a progress plan.
    fn planned(&self) -> Vec<(String, u64)> {
        self.pending
            .iter()
            .map(|p| (p.program().to_string(), p.download_bytes()))
            .collect()
    }

    /// The signed sizes the announcement states.
    fn sizes(&self) -> SignedSizes {
        let download = self.pending.iter().map(|p| p.download_bytes()).sum::<u64>();
        let disk = self
            .pending
            .iter()
            .map(|p| p.disk_bytes())
            .sum::<Option<u64>>();
        SignedSizes {
            download: Some(download).filter(|d| *d > 0),
            disk: disk.filter(|d| *d > 0),
        }
    }

    /// What the landings need free at once, landed [`RIDER_LANES`] at a time
    /// ([`crate::flow::peak_need_at_once`]): what a set completion landing beside them
    /// reserves.
    fn peak(&self) -> u64 {
        crate::flow::peak_need_at_once(
            self.pending
                .iter()
                .map(crate::vendor_direct::lane::Pending::landing_bytes),
            RIDER_LANES,
        )
    }
}

/// Land `pending` in turn on this thread, noting each on the live pass and handing its
/// outcome to `landed`. It writes no status row, so a lane thread can run it.
fn land_fresh(
    lane: &crate::vendor_direct::lane::Lane<'_>,
    pending: &[crate::vendor_direct::lane::Pending],
    landed: &mut dyn FnMut(crate::vendor_direct::lane::Outcome),
) {
    use crate::vendor_direct::lane::Verdict;
    for pending in pending {
        let outcome = crate::vendor_direct::lane::land_one(lane, pending);
        match outcome.verdict {
            Verdict::Installed { .. } => {
                note_finished(pending.program(), crate::progress::Phase::Done, None);
            }
            // A first landing whose signature check did not finish is a wait, not a
            // failure (as the update lane reads `Deferred`): no `net-failed:` card. Likeliest
            // on a first install, with the disk busy and codesign clamped to the Background
            // role. Skipped with no error; the kept archive lands next pass without a download.
            Verdict::Deferred { .. } => {
                note_finished(pending.program(), crate::progress::Phase::Skipped, None);
            }
            _ => note_finished(
                pending.program(),
                crate::progress::Phase::Failed,
                Some(outcome.line()),
            ),
        }
        landed(outcome);
    }
}

/// The fresh landings with nothing beside them: announced with `net-starting:` and their
/// signed sizes, a "net" progress pass of their own when a progress file is wanted and no
/// pass is live, each outcome reported as it lands, answered with their terminal marker
/// ([`fresh_landing_terminal`]). `promised` when they are what is left of a promised
/// default-set pass whose set completion stopped before it could carry them: their plan is
/// then the promised plan, and the promise goes with it
/// ([`crate::progress::ProgressSink::plan_promised_pass`]). Ahead of a set completion still
/// to come, a plan of the vendors alone keeps it.
fn land_fresh_alone(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    consent: Consent,
    fresh: &FreshVendors,
    pass: &mut VendorPass,
    seams: &PassSeams<'_>,
    promised: bool,
) {
    use crate::vendor_direct::lane::Verdict;
    if fresh.pending.is_empty() {
        return;
    }
    let owned_pass = seams
        .progress
        .is_some_and(|p| crate::progress::begin_pass(p, NET_PASS));
    if let Some(sink) = crate::progress::active() {
        if promised {
            sink.plan_promised_pass(layout, &fresh.planned());
        } else {
            sink.plan(&fresh.planned());
        }
    }
    if let Some(line) = net_announcement(consent, &fresh.names(), fresh.sizes()) {
        emit_marker_line(&line);
    }
    let mut arrived: Vec<&str> = Vec::new();
    let mut deferred: Vec<&str> = Vec::new();
    land_fresh(
        &fresh.lane(layout, fetcher, seams.trust),
        &fresh.pending,
        &mut |outcome| {
            match outcome.verdict {
                Verdict::Installed { .. } => arrived.push(outcome.spec.program),
                Verdict::Deferred { .. } => deferred.push(outcome.spec.program),
                _ => {}
            }
            report_vendor(layout, &outcome, pass, false, fresh.now);
        },
    );
    if owned_pass {
        crate::progress::end_pass();
    }
    if let Some(line) = fresh_landing_terminal(&arrived, &deferred, fresh.pending.len()) {
        emit_marker_line(&line);
    }
    if !arrived.is_empty() {
        println!("{SEED_FOLLOW_ON}");
    }
}

/// What a [`VendorRider`] came to, folded into its pass's vendor lane: its landings, reported
/// as each landed beside the set completion ([`RiderLanded`]), or — when that set completion
/// stopped before it could carry them — its landings alone now ([`land_fresh_alone`], their
/// plan the promised one when `promised`); then the warm of whatever landed and is not warm
/// yet. The programs whose landing waited on a signature check that did not finish: the
/// terminal of a set completion that announced them names them ([`fresh_landing_terminal`]).
fn settle_rider(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    consent: Consent,
    rider: VendorRider<'_>,
    vendor: &mut VendorPass,
    seams: &PassSeams<'_>,
    promised: bool,
) -> Vec<&'static str> {
    let deferred = match rider.landed {
        Some(landed) => {
            vendor.absorb(landed.pass);
            landed.deferred
        }
        None => {
            land_fresh_alone(
                layout,
                fetcher,
                consent,
                &rider.fresh,
                vendor,
                seams,
                promised,
            );
            Vec::new()
        }
    };
    warm_landed(layout, vendor, seams);
    deferred
}

/// The vendor lane's fresh landings handed to the set completion of a pass that installs
/// the set on an empty store — the update pass's, and `install --default-set`'s
/// ([`install_default_set_inner`]): ONE `net-starting:` names both lanes'
/// programs with their signed sizes, and once the set's archives are in ([`RiderGate`]), the
/// vendors land while the pass installs them — on lane threads of their own when the fetcher
/// may be shared — inside the one store lock, the one progress pass and the one terminal
/// marker. Each outcome is reported on the pass's thread, which writes every status row, as
/// it lands: between the set's units, and while the pass waits for the lanes once its own
/// units are in (`landed`). A set completion that
/// stops before it announces (its index stale, no channel for this machine, or a volume
/// that cannot hold both lanes at once) leaves `landed` empty, and the caller lands them
/// alone ([`land_fresh_alone`]; both through [`settle_rider`]).
struct VendorRider<'r> {
    fresh: FreshVendors,
    trust: &'r crate::vendor_direct::lane::Trust,
    /// The warm of an agent build the moment it lands ([`PassSeams::warm_agents`]).
    warm: fn(&crate::store::Layout, &std::collections::BTreeSet<String>),
    landed: Option<RiderLanded>,
}

/// One of a [`VendorRider`]'s landings, handed from the lane that landed it to the pass's
/// thread: its outcome, and whether the lane warmed its build as it landed.
struct RiderLanding {
    outcome: crate::vendor_direct::lane::Outcome,
    warmed: bool,
}

/// What a [`VendorRider`]'s landings came to, reported on the pass's thread as each landed
/// ([`RiderLanded::report`]).
#[derive(Default)]
struct RiderLanded {
    /// The vendor lane's tally of them ([`report_vendor`]), less the builds their lanes
    /// warmed as they landed: no later exit warms those again.
    pass: VendorPass,
    /// The programs whose landing waited on a signature check that did not finish.
    deferred: Vec<&'static str>,
}

impl RiderLanded {
    /// Report `landing` — its line, its count and its status row, as of `now` — on this
    /// thread, the one that writes every row of the pass.
    fn report(&mut self, layout: &crate::store::Layout, landing: RiderLanding, now: i64) {
        let program = landing.outcome.spec.program;
        if matches!(
            landing.outcome.verdict,
            crate::vendor_direct::lane::Verdict::Deferred { .. }
        ) {
            self.deferred.push(program);
        }
        report_vendor(layout, &landing.outcome, &mut self.pass, false, now);
        if landing.warmed {
            self.pass.landed.remove(program);
        }
    }
}

/// How often a vendor lane waiting on its gate reads the bump file ([`rider_land`]): a
/// person who typed an agent's name waits at most this long for its bytes to start.
const RIDER_BUMP_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// How many of a rider's programs land at once once its gate opens, each on a lane thread of
/// its own ([`rider_land`]), in table order: claude and codex first, then agy and muse as a
/// lane frees. Measured on a first run, two lanes land codex and end the pass well sooner
/// than one, with claude as early; four end it slightly sooner still but land claude later,
/// splitting the link four ways. The lanes take turns at the download itself, in the order
/// their programs were taken ([`RiderQueue::next`], [`crate::flow::DownloadTurns`]), so the
/// overlap is one lane's download beside the other's verify, unpack and activate.
const RIDER_LANES: usize = 2;

/// The gate a riding vendor lane waits on ([`rider_land`]): open for good once every archive
/// the set completion lands is in, its coherence tuples' among them (the download-ahead lane
/// says so the moment the last one lands, [`crate::flow::ahead::Ahead::all_fetched`]), or
/// once no tuple is left to land, whichever comes first. The pass thread's
/// [`RiderGateOpens`] opens it when the loop ends or unwinds, so no lane waits on it forever.
#[derive(Default)]
struct RiderGate {
    open: std::sync::Mutex<bool>,
    changed: std::sync::Condvar,
}

impl RiderGate {
    fn open(&self) {
        if let Ok(mut open) = self.open.lock() {
            *open = true;
        }
        self.changed.notify_all();
    }

    /// Open (a poisoned gate is: no lane may wait on it forever).
    fn is_open(&self) -> bool {
        self.open.lock().map_or(true, |open| *open)
    }

    /// Wait at most `tick` for the gate to open.
    fn wait(&self, tick: std::time::Duration) {
        if let Ok(open) = self.open.lock() {
            let _ = self.changed.wait_timeout_while(open, tick, |open| !*open);
        }
    }
}

/// Opens a [`RiderGate`] when dropped: the set completion's loop over, or unwinding.
struct RiderGateOpens<'g>(&'g RiderGate);

impl Drop for RiderGateOpens<'_> {
    fn drop(&mut self) {
        self.0.open();
    }
}

/// A rider's landings not yet taken, worked by every lane that lands them ([`rider_land`]),
/// and the order their payloads take the link in.
struct RiderQueue<'q> {
    left: std::sync::Mutex<Vec<&'q crate::vendor_direct::lane::Pending>>,
    /// One agent download at a time, in the order the programs are taken
    /// ([`crate::flow::DownloadTurns`]): the first agent has the whole link (on a slow one,
    /// two downloads at once would land claude later), while the other lane's verify, unpack
    /// and activate overlap the next download.
    turns: crate::flow::DownloadTurns,
}

impl<'q> RiderQueue<'q> {
    fn of(rider: &'q VendorRider<'_>) -> Self {
        Self {
            left: std::sync::Mutex::new(rider.fresh.pending.iter().collect()),
            turns: crate::flow::DownloadTurns::default(),
        }
    }

    /// The next landing for a lane: first, at once and gate or no gate, a program a person
    /// asked for by name at a terminal (its pending stub's bump,
    /// [`crate::progress::Asker::Person`]), as the set completion's own queue honours a bump
    /// between its items; else, once `gate` opens (`None` is open), a program a script asked
    /// for (first among the agents, never ahead of the toolchain's bytes: `claude --version`
    /// in a script is not a person waiting), then the first left in table order. `None` once
    /// every one is taken. Each comes with its place in line for the link, so the payloads
    /// move in the order the programs were taken.
    fn next(
        &self,
        layout: &crate::store::Layout,
        gate: Option<&RiderGate>,
    ) -> Option<(
        &'q crate::vendor_direct::lane::Pending,
        crate::flow::DownloadTicket<'_>,
    )> {
        loop {
            let bump = crate::progress::read_bumps(layout);
            let open = gate.is_none_or(RiderGate::is_open);
            {
                let mut left = self
                    .left
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if left.is_empty() {
                    return None;
                }
                if let Some(i) = bump
                    .iter()
                    .filter(|b| open || b.person)
                    .find_map(|b| left.iter().position(|p| p.program() == b.program))
                {
                    if let Some(sink) = crate::progress::active() {
                        sink.bumped(left[i].program());
                    }
                    return Some((left.remove(i), self.turns.ticket()));
                }
                if open {
                    return Some((left.remove(0), self.turns.ticket()));
                }
            }
            if let Some(gate) = gate {
                gate.wait(RIDER_BUMP_TICK);
            }
        }
    }
}

/// One lane of a rider's landings, on this thread ([`land_fresh`]): take the queue's next
/// program ([`RiderQueue::next`]) until none is left, warming each agent build the moment it
/// lands rather than when the pass is over. Every landing goes to `landed` the moment it is
/// done — a lane thread sends it to the pass's thread, which writes its row — in the order
/// they land.
fn rider_land(
    lane: &crate::vendor_direct::lane::Lane<'_>,
    rider: &VendorRider<'_>,
    queue: &RiderQueue<'_>,
    gate: Option<&RiderGate>,
    landed: &mut dyn FnMut(RiderLanding),
) {
    while let Some((pending, ticket)) = queue.next(lane.layout, gate) {
        let lane = &crate::vendor_direct::lane::Lane {
            download_turn: Some(&ticket),
            ..*lane
        };
        land_fresh(lane, std::slice::from_ref(pending), &mut |outcome| {
            let warmed = matches!(
                outcome.verdict,
                crate::vendor_direct::lane::Verdict::Installed { .. }
            );
            if warmed {
                (rider.warm)(
                    lane.layout,
                    &std::collections::BTreeSet::from([outcome.spec.program.to_string()]),
                );
            }
            landed(RiderLanding { outcome, warmed });
        });
    }
}

/// The terminal marker that answers a `net-starting:` for `planned` landings (the vendor
/// lane's alone, or the set completion's with the vendors riding it), of which `arrived`
/// installed and `deferred` waited on a signature check
/// ([`crate::vendor_direct::lane::Verdict::Deferred`]): something arrived → `net-installed:`;
/// nothing arrived and some landing failed → `net-failed:`; every landing deferred →
/// `seed-done:` naming what waits — never `net-failed:` (nothing failed) and never silence
/// (indistinguishable from a child that died after announcing).
fn fresh_landing_terminal(arrived: &[&str], deferred: &[&str], planned: usize) -> Option<String> {
    if planned == 0 {
        return None;
    }
    if !arrived.is_empty() {
        return Some(format!(
            "atpkg: {NET_INSTALLED_MARKER}{}",
            arrived.join(", ")
        ));
    }
    if deferred.len() < planned {
        return Some(format!(
            "atpkg: {NET_FAILED_MARKER}network provisioning installed nothing — see the lines \
             above for each program's reason"
        ));
    }
    let one = deferred.len() == 1;
    Some(format!(
        "atpkg: {SEED_DONE_MARKER}the pass finished; {} {} for {} {SIGNATURE_WAIT_CLAUSE} — the \
         next pass installs {}",
        deferred.join(", "),
        if one { "waits" } else { "wait" },
        if one { "its" } else { "their" },
        if one { "it" } else { "them" },
    ))
}

/// The clause a `seed-done:` line carries when every fresh vendor landing waited on a
/// signature check that did not finish ([`fresh_landing_terminal`]), and what
/// [`human_marker`] reads to say that line whole to a person.
const SIGNATURE_WAIT_CLAUSE: &str = "signature check to finish";

/// Say one vendor-lane outcome (a fault on stderr, and an unreachable or deferred one too
/// when `unchecked_fails`), count it, and record its row as of `now`.
fn report_vendor(
    layout: &crate::store::Layout,
    outcome: &crate::vendor_direct::lane::Outcome,
    pass: &mut VendorPass,
    unchecked_fails: bool,
    now: i64,
) {
    use crate::vendor_direct::lane::Verdict;
    let line = outcome.line();
    let unchecked = matches!(outcome.verdict, Verdict::Unreachable { .. });
    // A deferred signature check is said on stdout like any wait — a warning
    // only to a person whose typed verb it left undone.
    let deferred = matches!(outcome.verdict, Verdict::Deferred { .. });
    pass.ran.insert(outcome.spec.program.to_string());
    if outcome.is_failure() {
        pass.failures += 1;
        pass.failed.push((
            outcome.spec.program.to_string(),
            line.strip_prefix("atpkg: ").unwrap_or(&line).to_string(),
        ));
    }
    if outcome.is_failure() || ((unchecked || deferred) && unchecked_fails) {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
    if unchecked {
        pass.unchecked.insert(outcome.spec.program.to_string());
    }
    // Reached: a verdict the vendor's channel had to answer — never an unreached head, a
    // dev link, or a keep with no head in hand; an unreachable that carries a status
    // (`why`) is a host that answered.
    pass.reached |= matches!(
        outcome.verdict,
        Verdict::Installed { .. }
            | Verdict::Current(_)
            | Verdict::Refused { .. }
            | Verdict::Failed { .. }
            | Verdict::Deferred { .. }
            | Verdict::Kept { head: Some(_), .. }
            | Verdict::Unreachable { why: Some(_), .. }
    );
    if let Some(asset) = &outcome.asset {
        pass.resolved_assets
            .insert(outcome.spec.program.to_string(), asset.clone());
    }
    if matches!(
        outcome.verdict,
        Verdict::Installed { .. } | Verdict::RolledBack { .. } | Verdict::Tombstoned { .. }
    ) {
        pass.changed = true;
    }
    note_landed(pass, outcome);
    record_vendor_status(layout, outcome, now);
}

/// A vendor program's `status.toml` row after its lane, for the active build:
/// `managed <version> — <Vendor> latest` only when it is the vendor's head (installed or
/// current); `managed <version> — <why>` when the head was not taken for a reason that is
/// no fault ([`crate::vendor_direct::lane::Outcome::kept_clause`]); `error: <line>` for a
/// refusal or a failed landing; `tombstoned: <line>`. An unreachable vendor judged nothing:
/// its row stands while it names the active build and a head check reached the vendor
/// within the last day (as of `now`); past that it says `<Vendor>'s release channel not
/// reached since <date>`. A row is written only when it changes.
fn record_vendor_status(
    layout: &crate::store::Layout,
    outcome: &crate::vendor_direct::lane::Outcome,
    now: i64,
) {
    use crate::vendor_direct::lane::Verdict;
    let (program, vendor) = (outcome.spec.program, outcome.spec.vendor);
    let line = outcome.line();
    let said = line.strip_prefix("atpkg: ").unwrap_or(&line).to_string();
    let active = crate::active_builds(layout).get(program).copied();
    // (A SHADOWED row an older client recorded is simply replaced: shadows are read per
    // shell, never recorded, `crate::status::with_shadows`.)
    let prior = crate::status::read(layout).and_then(|s| s.programs.get(program).cloned());
    keep_replaced_legacy_root(layout, program, prior.as_ref());
    // The active build as a row names it: a vendor build by its recorded version; a legacy
    // index build by the version the lane read, else its number — never an index pin.
    let recorded = active.and_then(|b| {
        if crate::vendor_direct::is_vendor_build(b) {
            crate::vendor_direct::complete_record(&layout.build_dir(program, b))
                .map(|r| (b, r.version.to_string(), r.tree_root))
        } else {
            let label = outcome
                .active_version()
                .map_or_else(|| crate::vendor_direct::build_label(b), |v| v.to_string());
            Some((b, label, legacy_root_of(layout, program, b)))
        }
    });
    let managed = |state: &dyn Fn(&str) -> String| {
        recorded
            .as_ref()
            .map(|(b, label, root)| crate::ProgramStatus {
                installed_build: Some(*b),
                state: state(label),
                tree_root: root.clone(),
            })
    };
    let (row, moved) = match &outcome.verdict {
        Verdict::Installed { .. } => (managed(&|v| crate::state::vendor_managed(v, vendor)), true),
        Verdict::Current(_) => (managed(&|v| crate::state::vendor_managed(v, vendor)), false),
        Verdict::RolledBack { .. } | Verdict::Kept { .. } | Verdict::Deferred { .. } => {
            let why = outcome.kept_clause().unwrap_or_default();
            (
                managed(&|v| crate::state::vendor_kept(v, &why)),
                matches!(outcome.verdict, Verdict::RolledBack { .. }),
            )
        }
        Verdict::Unreachable { .. } => {
            let stamp = crate::vendor_direct::ProgramStamp::read(layout, program);
            let vouched = stamp.as_ref().is_some_and(|s| s.checked_recently(now));
            if vouched
                && prior
                    .as_ref()
                    .is_some_and(|r| active.is_some() && r.installed_build == active)
            {
                return;
            }
            let why = match stamp.filter(|s| !vouched && s.last_checked_at > 0) {
                Some(s) => unreached_since(vendor, s.last_checked_at),
                None => outcome.kept_clause().unwrap_or_default(),
            };
            (managed(&|v| crate::state::vendor_kept(v, &why)), false)
        }
        Verdict::Refused { .. } | Verdict::Failed { .. } | Verdict::AlabRestoreFailed { .. } => (
            Some(failure_row(layout, program, format!("error: {said}"))),
            true,
        ),
        Verdict::Tombstoned { .. } | Verdict::AlabTombstoned { .. } => (
            Some(failure_row(layout, program, format!("tombstoned: {said}"))),
            true,
        ),
        Verdict::Linked => (None, false),
        Verdict::Unavailable { state } => {
            (Some(failure_row(layout, program, state.clone())), false)
        }
        // An ALab build, kept or rolled back onto: its row names its store build and why,
        // never a vendor version or a claim to be the vendor's latest.
        Verdict::AlabKept { .. } | Verdict::AlabRolledBack { .. } => {
            let why = outcome.kept_clause().unwrap_or_default();
            (
                active.map(|b| crate::ProgramStatus {
                    installed_build: Some(b),
                    state: crate::state::alab_kept(b, &why),
                    tree_root: legacy_root_of(layout, program, b),
                }),
                matches!(outcome.verdict, Verdict::AlabRolledBack { .. }),
            )
        }
    };
    let Some(row) = row else {
        return;
    };
    // A row missing the root it can now carry (a rollback recorded before its build's root
    // was kept) is a change too.
    if prior.as_ref().is_some_and(|r| {
        r.state == row.state
            && r.installed_build == row.installed_build
            && r.tree_root == row.tree_root
    }) {
        return;
    }
    if moved {
        record_status(layout, program, row, said);
    } else {
        record_row_keeping_outcome(layout, program, row);
    }
}

/// `<Vendor>'s release channel not reached since <date>`: why a row stops calling a build
/// the vendor's latest, `checked_at` being the last head check that reached the vendor.
fn unreached_since(vendor: &str, checked_at: i64) -> String {
    let stamp = u64::try_from(checked_at)
        .map(aterm_types::rfc3339::format_rfc3339)
        .unwrap_or_default();
    let date = stamp.get(..10).unwrap_or(&stamp);
    format!("{vendor}'s release channel not reached since {date}")
}

/// Report a channel-apply outcome per coherence group — one summary line for the groups
/// that were current or moved ([`channel_apply_summary`]), a line of its own for every
/// other outcome — record per-program status, and return the number of aborted groups.
fn report_channel_apply(
    layout: &crate::store::Layout,
    installed: &std::collections::BTreeMap<String, u64>,
    report: &crate::ChannelApplyReport,
    also_failed: &[String],
    whole_set: bool,
) -> u32 {
    // Post-apply active builds (shim-derived), for accurate per-program status.
    let post: std::collections::BTreeMap<String, u64> = crate::active_builds(layout);
    // The rows as they stood BEFORE this pass wrote any — what the UpToDate arm reads to
    // lift the missing-triple hold's `held: …` row (the Unpublished arm below) once the
    // tuple is whole on a pin this host is served.
    // Activation and root recovery already finished under the store lock. Keep the
    // complete rows: their build-qualified roots serve this pass's members too.
    let before = crate::status::read(layout)
        .map(|status| status.programs)
        .unwrap_or_default();
    let was_held = |prog: &str| {
        before
            .get(prog)
            .is_some_and(|row| row.state.starts_with(crate::state::HELD_PREFIX))
    };
    let mut failures = 0u32;
    // The members that failed, by name: `record_status` overwrites the aggregate `outcome`,
    // so Settings' one line must name them all.
    let mut failed_names: Vec<String> = Vec::new();
    // One summary per pass: the members already current and the ones this pass moved, said
    // together at the end ([`channel_apply_summary`]), not a line per group.
    let mut current: Vec<String> = Vec::new();
    let mut moved: Vec<(String, Option<u64>, u64)> = Vec::new();
    let mut other_outcomes = false;
    for (group, outcome) in &report.groups {
        let label = group
            .group
            .clone()
            .unwrap_or_else(|| group.members.join("+"));
        other_outcomes |= !matches!(
            outcome,
            crate::TxnOutcome::UpToDate | crate::TxnOutcome::Applied(_)
        );
        match outcome {
            crate::TxnOutcome::UpToDate => {
                current.extend(
                    group
                        .members
                        .iter()
                        .filter(|p| post.contains_key(*p))
                        .cloned(),
                );
                // The same arm lifts and re-stamps: an older client's `blocked by <dep>: …`
                // row or a pre-`managed` `active` row is rewritten by nothing else while the
                // pin stands still, and a `managed N — pinned by index 15` row would keep
                // naming 15 through later index builds. The pass's outcome sentence stands
                // (only the lifted-hold case narrates).
                for prog in &group.members {
                    let Some(build) = post.get(prog).copied() else {
                        continue;
                    };
                    let state = crate::state::managed(build, report.index_build);
                    let row = crate::ProgramStatus {
                        installed_build: Some(build),
                        state: state.clone(),
                        tree_root: tree_root_for_row(before.get(prog), Some(build), ""),
                    };
                    if was_held(prog) {
                        println!("atpkg: {prog} {build} is current again (no longer held)");
                        record_status(layout, prog, row, format!("group {label}: up to date"));
                        continue;
                    }
                    let prior = before.get(prog.as_str()).map(|row| row.state.as_str());
                    if up_to_date_row_needs_rewrite(prior, build, report.index_build) {
                        record_row_keeping_outcome(layout, prog, row);
                    }
                }
            }
            crate::TxnOutcome::Applied(members) => {
                for prog in members {
                    if let Some(build) = report.applied.get(prog).map(|a| a.build) {
                        moved.push((prog.clone(), installed.get(prog).copied(), build));
                    }
                }
                current.extend(
                    group
                        .members
                        .iter()
                        .filter(|p| !members.contains(*p) && post.contains_key(*p))
                        .cloned(),
                );
                for prog in &group.members {
                    // Only flipped members carry a fresh signed root; an untouched sibling
                    // keeps its recorded one.
                    let new_root = report
                        .applied
                        .get(prog)
                        .map(|a| a.tree_root.clone())
                        .unwrap_or_default();
                    let build = report
                        .applied
                        .get(prog)
                        .map(|a| a.build)
                        .or_else(|| post.get(prog).copied())
                        .or_else(|| installed.get(prog).copied())
                        .unwrap_or(0);
                    record_status(
                        layout,
                        prog,
                        crate::ProgramStatus {
                            installed_build: post.get(prog).copied(),
                            state: crate::state::managed(build, report.index_build),
                            tree_root: tree_root_for_row(
                                before.get(prog),
                                post.get(prog).copied(),
                                &new_root,
                            ),
                        },
                        format!("group {label}: updated"),
                    );
                }
            }
            crate::TxnOutcome::Tombstoned(members) => {
                let (builds, their) = if members.len() == 1 {
                    ("build", "its")
                } else {
                    ("builds", "their")
                };
                eprintln!(
                    "atpkg: ALab withdrew the {} {builds} this Mac was set to use \u{2014} \
                     {their} commands are off until a fixed version is published, which \
                     `aterm pkg update` installs",
                    members.join(", ")
                );
                for prog in members {
                    record_status(
                        layout,
                        prog,
                        crate::ProgramStatus {
                            installed_build: installed.get(prog).copied(),
                            // Keep the recorded root: these arms (a held or tombstoned
                            // pin, an aborted transaction's post-state) describe an
                            // installed tree that did not change, and `atpkg verify`
                            // fails closed on an empty root.
                            state: crate::state::TOMBSTONED_PIN.into(),
                            tree_root: tree_root_for_row(
                                before.get(prog),
                                installed.get(prog).copied(),
                                "",
                            ),
                        },
                        format!("group {label}: tombstoned"),
                    );
                }
            }
            crate::TxnOutcome::Pinned(members) => {
                // The whole tuple holds for one pinned member, so the line names both.
                let unpin: Vec<String> = members
                    .iter()
                    .map(|m| format!("`aterm pkg unpin {m}`"))
                    .collect();
                if group.members == *members {
                    println!(
                        "atpkg: {} not updated \u{2014} you pinned it ({} lets it update)",
                        members.join(", "),
                        unpin.join(" and ")
                    );
                } else {
                    println!(
                        "atpkg: {} not updated \u{2014} you pinned {}, and they move together \
                         ({} lets them update)",
                        group.members.join(", "),
                        members.join(", "),
                        unpin.join(" and ")
                    );
                }
                for prog in members {
                    record_status(
                        layout,
                        prog,
                        crate::ProgramStatus {
                            installed_build: installed.get(prog).copied(),
                            state: format!(
                                "held \u{2014} you pinned it (`aterm pkg unpin {prog}` lets it \
                                 update)"
                            ),
                            tree_root: tree_root_for_row(
                                before.get(prog),
                                installed.get(prog).copied(),
                                "",
                            ),
                        },
                        format!("group {label}: held by pin"),
                    );
                }
                // A sibling's `deferred:` row is over too: the pin ended the wait (its
                // record is cleared), so a sibling the held flip would have moved is held by
                // the pin now, and says so. Any other sibling row stands.
                for prog in group.members.iter().filter(|p| !members.contains(*p)) {
                    let deferred = before.get(prog.as_str()).is_some_and(|row| {
                        row.state.starts_with(crate::state::FLIP_DEFERRED_PREFIX)
                    });
                    if !deferred {
                        continue;
                    }
                    let current = installed.get(prog).copied();
                    record_status(
                        layout,
                        prog,
                        crate::ProgramStatus {
                            installed_build: current,
                            state: format!(
                                "held \u{2014} it moves with {}, which you pinned ({} lets it \
                                 update)",
                                members.join(" and "),
                                unpin.join(" and ")
                            ),
                            tree_root: tree_root_for_row(before.get(prog), current, ""),
                        },
                        format!("group {label}: held by pin"),
                    );
                }
            }
            crate::TxnOutcome::Deferred {
                members,
                why,
                since,
            } => {
                // The toolchain's flip waits for quiet ([`crate::quiet`]): staged, nothing
                // flipped, never a failure; one stdout line and one `deferred:` row per member
                // it would move. "At the latest while an aterm window or terminal session is
                // open" holds because a window's park, and any interactive session, looks.
                let record = crate::quiet::read(layout);
                let until = since.saturating_add(crate::quiet::CEILING_SECS);
                println!(
                    "atpkg: {label} update staged ({}) \u{2014} it installs when nothing is using \
                     the toolchain ({why}), by {} at the latest while an aterm window or \
                     terminal session is open; `aterm pkg update` installs it now",
                    members.join(", "),
                    crate::quiet::clock(until)
                );
                for prog in members {
                    let current = installed.get(prog).copied();
                    let to = record.as_ref().and_then(|r| r.to.get(prog).copied());
                    let state =
                        crate::state::flip_deferred(to, current, &crate::quiet::clock(until));
                    record_status(
                        layout,
                        prog,
                        crate::ProgramStatus {
                            installed_build: current,
                            state,
                            tree_root: tree_root_for_row(before.get(prog), current, ""),
                        },
                        format!("group {label}: staged, flip deferred \u{2014} {why}"),
                    );
                }
            }
            crate::TxnOutcome::Unpublished {
                member,
                build,
                triple,
            } => {
                // The missing-triple hold: the new pin cannot fully exist on this host, so the
                // tuple stays on its current builds — a correct state, never a failure. One
                // row per installed member (the UpToDate arm lifts them), named by member,
                // never by group key.
                println!(
                    "atpkg: {} not updated — the new {member} build {build} is not published \
                     for {triple}; staying on the current builds",
                    group
                        .members
                        .iter()
                        .filter(|m| installed.contains_key(*m))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                for prog in &group.members {
                    let Some(current) = installed.get(prog).copied() else {
                        continue;
                    };
                    let owner = (prog != member).then_some(member.as_str());
                    let state = crate::state::held_unpublished(owner, *build, triple, current);
                    record_status(
                        layout,
                        prog,
                        crate::ProgramStatus {
                            installed_build: Some(current),
                            state: state.clone(),
                            tree_root: tree_root_for_row(before.get(prog), Some(current), ""),
                        },
                        format!("group {label}: {state}"),
                    );
                }
            }
            crate::TxnOutcome::Aborted {
                failed,
                during_flip,
                why,
            } => {
                failures += 1;
                failed_names.push(failed.clone());
                let phase = if *during_flip {
                    "flip (rolled back)"
                } else {
                    "stage"
                };
                // The cause rides everywhere the abort does: the stderr line, the member's
                // row (`aborted: <phase> — <cause>`, the prefix readers key on), the pass
                // outcome, and a stdout marker the window turns into a toolchain-lane row.
                let cause = clip_cause(why);
                eprintln!(
                    "atpkg: could not update {} ({failed}: {cause}) \u{2014} still using the \
                     previous version; the next update tries again",
                    group.members.join(", ")
                );
                emit_marker_line(&format!(
                    "atpkg: {GROUP_ABORTED_MARKER}{label} update aborted at {failed} during \
                     {phase}: {cause}"
                ));
                record_status(
                    layout,
                    failed,
                    crate::ProgramStatus {
                        installed_build: post.get(failed).copied(),
                        state: format!("aborted: {phase} — {cause}"),
                        tree_root: tree_root_for_row(
                            before.get(failed),
                            post.get(failed).copied(),
                            "",
                        ),
                    },
                    format!("group {label}: aborted at {failed} during {phase} — {cause}"),
                );
            }
        }
    }
    // The vendor lane's failures (`also_failed`) count toward the aggregate too: one
    // failure in each lane is two.
    if !failed_names.is_empty() && failed_names.len() + also_failed.len() >= 2 {
        let mut names = also_failed.to_vec();
        names.extend(failed_names);
        record_outcome(layout, failed_programs_outcome(&names));
    }
    if let Some(line) = channel_apply_summary(&current, &moved, other_outcomes, whole_set) {
        println!("atpkg: {line}");
    }
    failures
}

/// The one line a channel apply ends with: which programs it moved, `old → new`, and that
/// the rest are current. `moved` has `None` for a sibling that was not installed;
/// `other_outcomes` says some group ended another way (said on its own line), so "all"
/// would be false; `whole_set` is false for `update <program>`, whose members are named
/// rather than counted. `None` when the pass touched nothing.
fn channel_apply_summary(
    current: &[String],
    moved: &[(String, Option<u64>, u64)],
    other_outcomes: bool,
    whole_set: bool,
) -> Option<String> {
    if moved.is_empty() {
        return match (current.len(), other_outcomes, whole_set) {
            (0, _, _) => None,
            (1, _, _) => Some(format!("{} is up to date", current[0])),
            (_, _, false) => Some(format!("{} are up to date", current.join(", "))),
            (n, false, true) => Some(format!("all {n} ALab tools are up to date")),
            (n, true, true) => Some(format!("{n} ALab tools are up to date")),
        };
    }
    // Programs that moved between the same two builds are said together: a toolchain's
    // members move as one.
    let mut runs: Vec<(Vec<&str>, Option<u64>, u64)> = Vec::new();
    for (prog, from, to) in moved {
        match runs.iter_mut().find(|(_, f, t)| f == from && t == to) {
            Some((names, _, _)) => names.push(prog),
            None => runs.push((vec![prog], *from, *to)),
        }
    }
    let said: Vec<String> = runs
        .iter()
        .map(|(names, from, to)| match from {
            Some(from) => format!("{} {from} \u{2192} {to}", names.join(", ")),
            None => format!("{} {to} (new)", names.join(", ")),
        })
        .collect();
    let mut line = format!("updated {}", said.join("; "));
    if !current.is_empty() {
        line.push_str("; the rest are up to date");
    }
    Some(line)
}

/// The half [`cmd_verify_index`] and [`cmd_verify_pkg`] share: the five operands, the
/// document, its signature, and the roster admitted under the given paper master at floor
/// 0 on the real clock. `Err` has already said why: the usage (exit 2), an unreadable file
/// (1), or the opaque roster refusal (1).
fn admit_for_verify(
    args: &[String],
    verb: &str,
) -> Result<(Vec<u8>, Vec<u8>, crate::TrustedRoster), ExitCode> {
    let [master, doc, sig, roster, roster_sig] = args else {
        return Err(usage_error(verb, None));
    };
    // Naming a file that cannot be read is no verification oracle.
    let read = |path: &String| {
        std::fs::read(path).map_err(|e| {
            eprintln!("atpkg {verb}: cannot read {path}: {e}");
            ExitCode::from(1)
        })
    };
    let raw = read(doc)?;
    let sig_bytes = read(sig)?;
    let roster_bytes = read(roster)?;
    let roster_sig_bytes = read(roster_sig)?;
    let anchor = crate::Anchor::of(vec![master.clone()], 0);
    let Ok(admitted) = crate::admit_roster(&anchor, roster_bytes, &roster_sig_bytes, now_unix())
    else {
        // Opaque on purpose: no verification oracle.
        eprintln!("FAIL: the roster did not verify under that master, or is stale");
        return Err(ExitCode::from(1));
    };
    Ok((raw, sig_bytes, admitted))
}

/// `atpkg verify-index <master-pubkey-b64> <index.toml> <index.toml.sig> <roster.toml>
/// <roster.toml.sig>` — run the whole client chain over files on disk, for operators and
/// the publish pipeline's self-check. Exit 0 iff a machine the given paper master's roster
/// authorizes signed that index and the index's own attribution agrees. It takes the
/// master key because that is the only key a client pins. The roster floor is 0 (no store
/// to ratchet) and the clock is real, so a lapsed roster fails here as on a user's machine.
fn cmd_verify_index(args: &[String]) -> ExitCode {
    let (raw, sig_bytes, admitted) = match admit_for_verify(args, "verify-index") {
        Ok(admitted) => admitted,
        Err(code) => return code,
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
        // Post-verify, not a crypto oracle: the signature held, and the index's own bytes
        // cannot be honoured. Named, so the publisher fixes the row, not a machine's key.
        Err(why) => {
            match index_post_verify_refusal(&why) {
                Some(reason) => eprintln!("FAIL: {reason}"),
                // Every crypto verdict stays opaque: no verification oracle.
                None => eprintln!("FAIL: no machine on that roster signed this index"),
            }
            ExitCode::from(1)
        }
    }
}

/// What an [`crate::sig::TrustedRoster::authorize_index`] refusal means to a publisher:
/// `Some(reason)` for the post-verify variants, `None` for every crypto verdict, which
/// stays opaque. `authorize_index` checks the signature before it parses and binds the
/// attribution, so [`crate::Reject::Malformed`], [`crate::Reject::Schema`],
/// [`crate::Reject::Unattributed`] and [`crate::Reject::SeqMismatch`] are reached only with
/// a valid signature: the fix is a line of the index, not a re-keyed machine.
/// [`crate::Reject::NotAuthorized`] is not named: it is reachable both before verification
/// (a roster with no live machine) and after (a `machine_id` naming another machine).
fn index_post_verify_refusal(why: &crate::Reject) -> Option<String> {
    match why {
        crate::Reject::Malformed => Some(
            "the index verified, but no client can read it — malformed: not UTF-8, not \
             TOML, or a required field missing, duplicated or wrong-typed"
                .to_string(),
        ),
        crate::Reject::Schema => Some(format!(
            "the index verified, but its `schema` is newer than this build reads (this \
             build understands schema {})",
            crate::manifest::SUPPORTED_SCHEMA
        )),
        crate::Reject::Unattributed => Some(
            "the index verified, but carries no `machine_id`, so no client can attribute \
             it — add the signing machine's id to the index; its key is not the problem"
                .to_string(),
        ),
        crate::Reject::SeqMismatch => Some(
            "the index verified, but its `roster_seq` is absent or NEWER than the roster \
             shipped beside it — fix the index's `roster_seq` (or ship the roster it was \
             cut under); the machine's key is not the problem"
                .to_string(),
        ),
        // Disabled/BadKey/BadSig/Verify/Revoked/NotAuthorized/Expired/Stale/Rollback, and
        // the two pkg-only post-verify variants no index parse can produce.
        _ => None,
    }
}

/// `atpkg verify-pkg <master-pubkey-b64> <pkg-*.toml> <pkg.sig> <aterm-machines.toml>
/// <aterm-machines.toml.sig>` — prove a package manifest was signed by a machine the given
/// paper master's roster authorizes: the pkg sibling of [`cmd_verify_index`], with which the
/// mirror re-verifies every document it republishes with the client's own chain.
///
/// Same authority rule as the index (`TrustedRoster::authorize_bytes`): any listed,
/// unrevoked, unexpired machine on the admitted generation. No attribution bind (a pkg
/// manifest carries none); the ID printed is the verifying signature's. `OK` also means a
/// client can read the manifest ([`pkg_post_verify_refusal`]), not the signature alone.
fn cmd_verify_pkg(args: &[String]) -> ExitCode {
    let (raw, sig_bytes, admitted) = match admit_for_verify(args, "verify-pkg") {
        Ok(admitted) => admitted,
        Err(code) => return code,
    };
    match admitted.authorize_bytes(raw, &sig_bytes) {
        Ok((verified, who)) => {
            // The signature is half the chain: a client parses these bytes the moment they
            // verify and drops the program on any refusal ([`pkg_post_verify_refusal`]).
            if let Some(why) = pkg_post_verify_refusal(&verified) {
                eprintln!(
                    "FAIL: machine {} signed this manifest, but no client can read it — \
                     {why}",
                    who.machine_id
                );
                return ExitCode::from(1);
            }
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

/// The client's post-verify gates over an already-authenticated `pkg-*.toml`
/// ([`crate::manifest::parse_pkg`]): `None` when the manifest parses exactly as a client
/// reads it, `Some(reason)` naming the fix when every client would refuse it. `crate::flow`
/// drops a program whose manifest refuses, and this verb is the client's own rule on the
/// command line, so a signature check alone would pass manifests the whole fleet refuses.
/// Naming the reason is no verification oracle: every variant here is a parse failure over
/// bytes whose signature already verified.
fn pkg_post_verify_refusal(verified: &crate::VerifiedBytes) -> Option<String> {
    match crate::manifest::parse_pkg(verified) {
        Ok(_) => None,
        Err(crate::Reject::ShimEnv(why)) => Some(format!("shim_env: {why}")),
        Err(crate::Reject::RetiredKind(why)) => Some(format!("retired row: {why}")),
        Err(crate::Reject::Schema) => Some(format!(
            "its `schema` is newer than this build reads (this build understands \
             schema {})",
            crate::manifest::SUPPORTED_SCHEMA
        )),
        Err(crate::Reject::Malformed) => Some(
            "malformed: not UTF-8, not TOML, a required field missing or duplicated, or a \
             `github-release`/`https` row with no `asset`/`sha256` (a `pkg` row, no \
             `url`/`sha256`)"
                .to_string(),
        ),
        // Unreachable from a parse over authenticated bytes — total rather than silent,
        // so a new post-verify gate can never fall through this verb as an OK.
        Err(other) => Some(format!("refused post-verify: {other:?}")),
    }
}

/// `atpkg verify [program]` (§12) — re-attest the installed store against the last-trusted
/// SIGNED `tree_root` recorded in `status.toml` at install/update. A drift/integrity audit;
/// compares the recomputed `tree_root` against the signed value, never an unsigned hash. No
/// network. Exit 0 iff every audited program passes ([`crate::verify::VerifyOutcome::passes`]).
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
    for (name, o) in &outcomes {
        use crate::verify::VerifyOutcome::{
            BuildMismatch, Drift, Match, NoSignedRoot, NotInstalled, Unreadable, VendorRoot,
            WiredSysroot,
        };
        match o {
            VendorRoot {
                build,
                vendor,
                drift: None,
            } => println!(
                "atpkg: {} OK (matches the tree staged from {vendor}'s authenticated digest)",
                crate::vendor_direct::display_build(name, *build)
            ),
            VendorRoot { build, vendor, .. } => {
                eprintln!(
                    "atpkg: {} DRIFT — the store tree no longer matches the tree staged from \
                     {vendor}'s authenticated digest",
                    crate::vendor_direct::display_build(name, *build)
                );
            }
            Match { build } => {
                println!("atpkg: {name} build {build} OK (matches signed tree_root)")
            }
            WiredSysroot { build } => println!(
                "atpkg: {name} build {build} not checked \u{2014} installed by an older aterm \
                 that changed its files; a newer build is checked"
            ),
            Drift { build, .. } => {
                eprintln!(
                    "atpkg: {name} build {build} DRIFT — store tree does not match the signed tree_root"
                );
            }
            NoSignedRoot { .. } => {
                eprintln!(
                    "atpkg: {name} — no signed tree_root recorded; aterm pkg install {name} \
                     reinstalls it and records one"
                );
            }
            NotInstalled => {
                eprintln!("{}", not_installed_fix(name));
            }
            Unreadable { build, error } => {
                eprintln!(
                    "atpkg: {} unreadable: {error}",
                    crate::vendor_direct::display_build(name, *build)
                );
            }
            BuildMismatch { active, recorded } => {
                let recorded = recorded
                    .map_or_else(|| String::from("none"), crate::vendor_direct::build_words);
                eprintln!(
                    "atpkg: {name} active {} differs from recorded {recorded} — cannot attest; \
                     update/reinstall",
                    crate::vendor_direct::build_words(*active)
                );
            }
        }
    }
    if outcomes.iter().all(|(_, o)| o.passes()) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `atpkg link <program> <checkout> [rel-bin…]` — dev-link curated bins from a sibling
/// checkout into `bin/` (through the sensitive-name deny-list), marking the program so
/// `update`/`apply` hard-skip it until `atpkg unlink`. Default bin when none is given:
/// `target/release/<program>`.
struct LinkArgs<'a> {
    program: &'a str,
    checkout: &'a std::path::Path,
    bins: Vec<std::path::PathBuf>,
    if_checkout: Option<&'a std::path::Path>,
    if_prefix: Option<&'a std::path::Path>,
    wait_lock: Option<std::time::Duration>,
}

fn link_args(rest: &[String]) -> Result<LinkArgs<'_>, String> {
    let (Some(program), Some(checkout)) = (rest.first(), rest.get(1)) else {
        return Err("a program and checkout are required".into());
    };
    if program.starts_with('-') || checkout.starts_with('-') {
        return Err("a program and checkout must precede link options".into());
    }
    let mut request = LinkArgs {
        program,
        checkout: std::path::Path::new(checkout),
        bins: Vec::new(),
        if_checkout: None,
        if_prefix: None,
        wait_lock: None,
    };
    let mut args = rest[2..].iter();
    while let Some(arg) = args.next() {
        if arg == "--wait-lock" {
            if request.wait_lock.is_some() {
                return Err(format!("duplicate link option {arg}"));
            }
            let secs = args
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| format!("{arg} requires a number of seconds"))?;
            request.wait_lock = Some(std::time::Duration::from_secs(secs));
            continue;
        }
        let slot = match arg.as_str() {
            "--if-checkout" => Some(&mut request.if_checkout),
            "--if-prefix" => Some(&mut request.if_prefix),
            _ if arg.starts_with('-') => return Err(format!("unknown link option {arg:?}")),
            _ => None,
        };
        if let Some(slot) = slot {
            if slot.is_some() {
                return Err(format!("duplicate link option {arg}"));
            }
            let value = args
                .next()
                .filter(|value| !value.starts_with('-') && !value.is_empty())
                .ok_or_else(|| format!("{arg} requires a path"))?;
            *slot = Some(std::path::Path::new(value));
        } else {
            request.bins.push(std::path::PathBuf::from(arg));
        }
    }
    if request.if_checkout.is_some() != request.if_prefix.is_some() {
        return Err("--if-checkout and --if-prefix must be supplied together".into());
    }
    Ok(request)
}

fn cmd_link(rest: &[String]) -> ExitCode {
    let request = match link_args(rest) {
        Ok(request) => request,
        Err(why) => return usage_error("link", Some(&why)),
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    // Conditional requests are admitted by main_entry before any pass writer starts.
    if request.if_checkout.is_some() {
        eprintln!("atpkg: conditional link requires its store-lock admission");
        return ExitCode::from(1);
    }
    let bins = match requested_link_bins(&layout, &request) {
        Ok(bins) => bins,
        Err(code) => return code,
    };
    finish_link(
        &layout,
        request.program,
        crate::link(&layout, request.program, request.checkout, &bins),
    )
}

fn requested_link_bins(
    layout: &crate::store::Layout,
    request: &LinkArgs<'_>,
) -> Result<Vec<std::path::PathBuf>, ExitCode> {
    let program = request.program;
    let checkout_path = request.checkout;
    let checkout = checkout_path.display();
    let bins = if !request.bins.is_empty() {
        request.bins.clone()
    } else if let Some(sysroot) = sysroot_link_bins(layout, program, checkout_path) {
        // A toolchain, not a cargo project: shim every exposed name out of `<root>/bin`
        // (`sysroot_link_bins`). An explicit rel-bin list still wins.
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
                    "atpkg: {program} not linked — {checkout}/bin lacks {} of its commands \
                     ({}); build them, then re-run",
                    missing.len(),
                    missing.join(", ")
                );
                return Err(ExitCode::from(1));
            }
        }
    } else {
        // Default to the conventional cargo release bin, WITH the platform executable
        // extension (`target/release/<program>.exe` on Windows) — else `src.is_file()`
        // never matches the real exe and the link yields nothing to link. Shared with
        // the `[packages.links]` reconciliation — which a package directory of an
        // ALab-built program answers with its entrypoint instead ([`link_bins_for`]).
        link_bins_for(program, checkout_path)
    };
    Ok(bins)
}

fn cmd_conditional_link(
    request: &LinkArgs<'_>,
    guard: &mut crate::lock::StoreLock,
    args: &[String],
) -> ExitCode {
    let Some(configured) = layout() else {
        return ExitCode::from(1);
    };
    let bins = match requested_link_bins(&configured, request) {
        Ok(bins) => bins,
        Err(code) => return code,
    };
    let (Some(expected), Some(expected_prefix)) = (request.if_checkout, request.if_prefix) else {
        return usage_error("link", Some("both conditional guards are required"));
    };
    let mut admitted = match crate::linkmode::link_if_checkout(
        guard,
        &configured,
        request.program,
        request.checkout,
        &bins,
        expected,
        expected_prefix,
    ) {
        Ok(admitted) => admitted,
        Err(error) => return finish_link(&configured, request.program, Err(error)),
    };
    let layout = admitted.guard().layout();
    begin_store_mutation(admitted.guard());
    #[cfg(target_os = "macos")]
    let before = {
        let before = crate::activation_notice::agent_targets(&layout);
        crate::activation_notice::prepare(&layout);
        before
    };
    let _status_record = crate::status::hold(&layout);
    let record = PassRecord::begin(layout.clone(), args);
    let _meter = typed_pass_meter("link", &layout);
    let code = finish_link(&layout, request.program, admitted.apply());
    #[cfg(target_os = "macos")]
    crate::activation_notice::note_if_changed(&layout, &before);
    record.end(exit_code_u8(code));
    code
}

fn finish_link(
    layout: &crate::store::Layout,
    program: &str,
    result: Result<crate::linkmode::LinkOutcome, crate::linkmode::LinkError>,
) -> ExitCode {
    match result {
        Ok(out) => {
            println!(
                "atpkg: dev-linked {program} ({}); `aterm pkg unlink {program}` to release it",
                out.linked.join(", ")
            );
            for r in &out.refused {
                eprintln!("atpkg:   refused sensitive bin name {r} (deny-list)");
            }
            // The seam follows the link from this moment, not from the next pass: otherwise
            // `targo` on PATH and `cargo +trust` are two compilers under one name.
            if program == crate::seam::SEAM_PROGRAM {
                reassert_rustup_seam(layout);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: {program} not linked \u{2014} {e}");
            ExitCode::from(1)
        }
    }
}

/// `atpkg unlink <program>` — remove a program's dev links and its marker, then put the
/// installed build back on PATH (a pin is preserved): the dev shims alone would leave every
/// command the link covered absent, a split toolchain like the one `link` refuses to create.
/// The store build is still there (gc counts the per-program `current` link), so restoring
/// re-asserts shims that already have a target, never a download. A program dev-linked from
/// the start has nothing to restore, which is reported, not a failure.
fn cmd_unlink(program: Option<&String>) -> ExitCode {
    let Some(program) = program else {
        return usage_error("unlink", None);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    // Read the marker before `unlink` removes it: it is the only record of which names this
    // link took over, and restoring anything else would touch programs that were never linked.
    let owned = crate::linked_tool_names(&layout, program);
    match crate::unlink(&layout, program) {
        Ok(()) => {
            println!("atpkg: unlinked {program} (any pin is preserved)");
            match restore_installed_shims(&layout, program, owned.as_deref()) {
                Ok(Restored::NoBuild) => println!(
                    "atpkg:   no installed build to restore — {program} is off PATH until \
                     `aterm pkg install {program}`"
                ),
                // The link took over none of the installed build's commands: they still run.
                Ok(Restored::Nothing) => {}
                Ok(Restored::Laid(n)) => {
                    println!("atpkg:   restored {n} command(s) from the installed build");
                }
                Err(e) => eprintln!(
                    "atpkg:   WARNING: could not restore the installed build's shims ({e}); \
                     run `aterm pkg install {program}` to put it back on PATH"
                ),
            }
            // …and rustup's `trust` returns to the installed build with the shims: rebuilt
            // from the store, or — with no build to restore — detached, never left dangling.
            if program == crate::seam::SEAM_PROGRAM {
                if std::fs::symlink_metadata(crate::seam::store_current(&layout)).is_ok() {
                    reassert_rustup_seam(&layout);
                } else {
                    detach_rustup_seams(&layout);
                }
            }
            ExitCode::SUCCESS
        }
        Err(crate::LinkError::NotLinked(_)) => {
            eprintln!("atpkg: {program} is not dev-linked \u{2014} nothing to unlink");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("atpkg: {e}");
            ExitCode::from(1)
        }
    }
}

/// What [`restore_installed_shims`] put back.
#[derive(Debug, PartialEq, Eq)]
enum Restored {
    /// No installed build (no `current`, or one with no `bin/`): the program is off PATH.
    NoBuild,
    /// An installed build, but the link took over none of its commands (or its marker
    /// could not be read): nothing to lay back.
    Nothing,
    /// This many commands laid back from the installed build.
    Laid(usize),
}

/// Re-assert `program`'s shims from its current store build, confined to `owned` (the set
/// the link took over, from its marker) and to names the installed build has. Each passes
/// the install path's admission (`ToolName::new`, the sensitive-name deny-list), so a
/// restore can no more shadow `git`/`sudo` than an install can.
fn restore_installed_shims(
    layout: &crate::store::Layout,
    program: &str,
    owned: Option<&[String]>,
) -> std::io::Result<Restored> {
    let current = layout.program_current(program);
    if !current.exists() {
        return Ok(Restored::NoBuild);
    }
    let build_dir = std::fs::canonicalize(&current)?;
    let bin = build_dir.join("bin");
    if !bin.is_dir() {
        return Ok(Restored::NoBuild);
    }
    // Only the names the link owned: a sysroot bundle's `bin/` carries backends of other
    // atpkg programs (`trust`'s carries `ay`, `clean`, `ty`), so restoring from the listing
    // would repoint their shims at this build. The marker's set makes the restore symmetric
    // with the takeover.
    let Some(owned) = owned else {
        return Ok(Restored::Nothing);
    };
    let mut tools = Vec::new();
    for name in owned {
        // The binary must exist in the installed build: a shim with nothing behind it is
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
        return Ok(Restored::Nothing);
    }
    // The alias policy, offline: a dev link takes over the plain names only and the aliases
    // kept naming the store copy, so the restore neither sweeps them nor invents any for a
    // vendor tool.
    let aliases = alias_policy_offline(layout, program);
    // The store build's own `shim_env` sidecar: the dev link laid plain shims, and the
    // restore lays them back as the signed manifest declared them.
    let env = crate::shim_env::read_sidecar(&build_dir);
    crate::activate::install_tools_env(layout, &build_dir, &tools, aliases, &env)?;
    Ok(Restored::Laid(tools.len()))
}

/// The `alab-<tool>` alias policy for `program` on a verb that fetches no index: the cached
/// index's signed entry decides ([`crate::activate::Aliases::for_program`]) when one is
/// cached; else the aliases already laid on disk are the policy
/// ([`crate::activate::Aliases::laid_for`]), so a vendor tool never grows an alias here
/// unless a signed entry said it was ALab's.
fn alias_policy_offline(layout: &crate::store::Layout, program: &str) -> crate::activate::Aliases {
    match cached_index(layout) {
        Some(index) => crate::activate::Aliases::for_program(program, index.program(program)),
        None => crate::activate::Aliases::laid_for(layout, program),
    }
}

/// `atpkg refresh [program…]` — re-assert dev links from their markers (picking up
/// newly-built bins). No arg ⇒ every dev-linked program. One failure never blocks the rest.
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
            Ok(out) => println!("atpkg: refreshed {program} ({})", out.linked.join(", ")),
            Err(e @ (crate::LinkError::NotLinked(_) | crate::LinkError::BadName(_))) => {
                failed += 1;
                eprintln!("atpkg: {e}");
            }
            Err(e) => {
                failed += 1;
                eprintln!("atpkg: {program} not refreshed \u{2014} {e}");
            }
        }
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// The held-row surfaces, on EVERY platform: the main `tests` module below is Unix-only
/// (its shell fixtures are), and the row these pin was first met on
/// Windows — a test that could not run there would pin nothing about it.
#[cfg(test)]
#[path = "cli_held_row_tests.rs"]
mod held_row_tests;

#[cfg(all(test, unix))]
#[path = "cli_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "cli_alab_tests.rs"]
mod alab_tests;
