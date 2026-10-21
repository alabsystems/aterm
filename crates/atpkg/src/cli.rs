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
        usage: "aterm pkg repair            — restore installed commands, shell integration \
                and (macOS) the ~/.local/bin links",
        // `repair` writes, so a swallowed `--dry-run` would rewrite everything at exit 0
        // (audit D-12): it takes no arguments, and refuses any.
        arity: Arity::None,
        program_flags: None,
        mutates_store: true,
        applies_machine: false,
        run: |_| run_repair(layout()),
    },
    Verb {
        name: "machine",
        tier: Tier::Occasional,
        usage: "aterm pkg machine                 — the [machine] settings, as the doctor reads \
                them\n\
                aterm pkg machine apply           — apply them now: Universal Control off for \
                this host",
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
        usage: "aterm pkg link <program> <checkout> [rel-bin…]",
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

/// Whether a `-h`/`--help` in `rest` (the argv after `verb`) is a question for atpkg
/// rather than an argument travelling to a tool. A literal `--` ends the scan for every
/// verb; for `run`, only the FIRST operand counts — everything after `<tool>` belongs to
/// the tool, so `atpkg run rg --help` reaches rg, and so does `aterm rg --help`, which
/// arrives here as `run rg -- --help`.
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
        // Not a verb: the roster at exit 0 would read as help for `<verb>` (audit
        // D-C), so name the topic, suggest the near miss, list the real ones, exit 2.
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

/// The whole package-manager CLI as a callable: `argv[1..]` in, exit code
/// out. Served in-process by the ONE `aterm` binary (`aterm pkg …` / the
/// `atpkg` argv0 alias) and by the thin standalone bin.
pub fn main_entry(mut argv: Vec<std::ffi::OsString>) -> ExitCode {
    // THE PRIVATE WINDOW'S LEAD ([`PRIVATE_STATE_ROOT_FLAG`], ruling 410), taken off
    // before anything reads the first word as the verb: this pass carries no `[machine]`
    // edit, whatever this build's seams say.
    if argv.first().and_then(|v| v.to_str()) == Some(PRIVATE_STATE_ROOT_FLAG) {
        argv.remove(0);
        PRIVATE_STATE_ROOT_LEAD.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // THE HIDDEN WARM VERB, ON THE RAW ARGV ([`crate::warm`]): what a pass that landed an
    // agent build starts, detached, to run that build once headless. Its operands carry the
    // store prefix, which must reach it byte for byte (the lossy conversion below would
    // name a store that does not exist), so it is answered here — before the store lock,
    // which the pass that started it may still hold; it never mutates the store, and says
    // nothing.
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
        // `--head-watch` — which door `update <program>` takes, stripped here likewise.
        if take_flag(&mut args, HEAD_WATCH_FLAG) {
            HEAD_WATCH_PASS.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // `--wait-lock <secs>` — the machine lanes' opt-in to QUEUE behind a sibling's
        // pass at the store lock instead of refusing, stripped here for
        // the same reason and BEFORE everything that would otherwise refuse it:
        // `cmd_seed`'s operand refusal, the flag gate below, and the lock itself,
        // which is what reads it ([`mutator_store_lock`]).
        if let Some(bound) = take_wait_lock_flag(&mut args) {
            // The spawner's pid is captured HERE, at the edge, because "my parent
            // is gone" is a CHANGE of parent (see `parent_is_gone`), and this is
            // the last moment the parent is known to be the spawner.
            let _ = WAIT_LOCK.set(WaitLock {
                bound,
                spawner: spawner_pid(),
            });
            // Before any lock attempt: how many full passes had ended when this one was
            // queued ([`pass_ended_while_we_waited`]).
            if args.first().is_some_and(|verb| verb == "update")
                && let Some(layout) = crate::store::resolve_configured()
            {
                let _ = QUEUED_AT_PASS_SEQ.set(crate::status::pass_seq(&layout));
            }
            // THE ORPHAN WATCH, armed HERE for the whole process — not inside a
            // progress pass, which the channel-apply lane never opens: a window that exits
            // under this child (a self-update's successor, a quit-and-reopen) leaves
            // its pipes unread, and the next print would kill the pass with EPIPE.
            // The watch notices the parent CHANGE and re-points stdout/stderr at a
            // log beside the progress file ([`crate::progress::watch_for_orphaning`]).
            let log = crate::store::resolve_configured()
                .map(|l| l.prefix.join(crate::progress::ORPHAN_LOG_NAME))
                .unwrap_or_else(|| std::env::temp_dir().join(crate::progress::ORPHAN_LOG_NAME));
            crate::progress::watch_for_orphaning(log);
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
    // The HIDDEN landing verb ([`crate::landing`]): what an agent program's `agents/`
    // twin laid by an older client execs while a landing marker stands — now an
    // immediate, silent `exec` of `bin/<program>`. Same discipline as `__pending` —
    // unlisted, dispatched before the match, and BEFORE the store lock, which a pass may
    // hold at that moment.
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
        // `atpkg help <verb>` answers for THAT verb, matching `aterm help <topic>`;
        // bare `help` keeps the roster. An UNKNOWN topic is a usage error, not a
        // roster at exit 0 — see `cmd_verb_help`.
        if let Some(topic) = args.get(1) {
            return cmd_verb_help(topic);
        }
        return cmd_help();
    }
    // `<verb> --help` anywhere before a literal `--` answers for that verb and does
    // nothing else — for `run`, only as the first operand, since everything after
    // `<tool>` belongs to the tool ([`help_flag_addresses_atpkg`]).
    //
    // Before the store lock and every verb body: asking a mutating verb how it works
    // must never mutate (docs/AUDIT-cli-per-verb-help-2026-08-31.md).
    if let Some(v) = verb
        && verb_usage(v).is_some()
        && help_flag_addresses_atpkg(v, &args[1..])
    {
        return cmd_verb_help(v);
    }
    // A FLAG IS NEVER A PROGRAM NAME. The verbs below read `args[1]` as a program and hand
    // it to the signed index, which would record `--x` as a failed program in
    // `status.toml`, or uninstall nothing and exit 0. Gated at the one dispatch edge,
    // before the store lock, so a typo never contends for `store.lock`.
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
    let _store_lock = if verb.is_some_and(verb_mutates_store) {
        match mutator_store_lock() {
            Ok(guard) => guard,
            Err(code) => {
                // The package log hears a pass the store lock refused, too (Phase 4): a
                // `claude update` behind a running pass, a window lane whose wait ran out.
                if let Some(layout) = crate::store::resolve_configured() {
                    PassRecord::refused(&layout, &args, exit_code_u8(code));
                }
                // A pass that carried a `[machine]` edit applied it beside this refusal —
                // the settings need no lock. Say so, on stderr (stdout stays the marker
                // channel the waiter tests pin), only when the record shows the table
                // applied, so only once the apply has ENDED, and only while someone is
                // there to read it.
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
    // The pass's store, resolved ONCE under the lock: nothing from here to the dispatch
    // writes `aterm.toml`, HOME or the prefix, so each reader below gets the same answer.
    let pass_layout = _store_lock
        .as_ref()
        .and_then(|_| crate::store::resolve_configured());
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
    // ONE `status.toml` WRITE PER VERB (Phase 3): a verb that holds the store lock records
    // into memory and writes the record once when it returns — declared after the lock,
    // so it is dropped first and the record is on disk before a waiter can take the lock.
    let _status_record = pass_layout
        .clone()
        .map(|layout| crate::status::hold(&layout));
    // THE PACKAGE LOG'S PASS RECORD (Phase 4): every verb that holds the store lock is a
    // pass — whichever lane ran it — logged when it starts and when it returns, while the
    // lock is still held (so the log may rotate) and the record still in memory (so the
    // end line can quote the outcome the pass recorded).
    let pass_record = pass_layout
        .clone()
        .map(|layout| PassRecord::begin(layout, &args));
    // AN INTERRUPTED ACTIVATION IS FINISHED BEFORE ANYTHING DECIDES ON IT (Phase 3): a
    // pass killed between a `current` flip and its shims left a complete build that no
    // shim runs; it is re-laid from its sidecars here, never re-staged or re-fetched.
    if matches!(verb, Some("seed" | "update" | "install"))
        && let Some(layout) = pass_layout.clone()
    {
        roll_forward_interrupted_activations(&layout, || cached_index(&layout));
    }
    // A PERSON'S PASS IS NEVER MINUTES OF SILENCE: under the lock, so the meter never
    // draws over the wait for it.
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
    /// The record's `(updated_at, outcome)` when the pass began, and how many outcome
    /// sentences this thread had written ([`outcomes_written`]): the end line quotes the
    /// outcome only if THIS pass wrote one ([`Self::outcome`]).
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

    /// The outcome sentence THIS pass recorded, or none. Two facts, both required: this
    /// thread wrote an outcome since the pass began ([`note_outcome_written`]), and the
    /// record moved (the write landed). The record alone cannot say it — `updated_at` moves
    /// on every write, and a row written keeping the outcome ([`record_row_keeping_outcome`],
    /// a head-watch pass whose vendor was not reached) carries an EARLIER pass's sentence.
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

/// WHICH LANE RAN A PASS, read off how it was spawned — no flag of its own, because every
/// spawner already says enough: the head watch's door carries [`HEAD_WATCH_FLAG`]; the
/// terminal session's detached pass names no spawner ([`SPAWNER_DETACHED`]); the window's
/// passes (its loop and Settings' Update now) queue with `--wait-lock` and report through
/// `--progress-file`; the `claude update`/`codex update` intercept's child queues with
/// `--wait-lock` and no progress file ([`crate::selfupdate::child_args`]); anything else
/// was typed (or scripted) — it did not queue. Pure for the test.
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
/// lost — `Some` only for an AGENT PROGRAM whose twin is laid and resolves, with the
/// `agents/` directory ahead of every foreign copy on `path_var` (or with no foreign copy
/// at all). The twin forwards to the store build, not to `bin/`, so that shell still runs
/// the managed copy. Not consulted: a `reroute/` agent stub, which decides at exec time
/// ([`agent_stub_on_path`]) — the lost-shim arm reports the PATH order as it stands.
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

/// Whether `<prefix>/agents` comes before the managed `bin/` on `path_var` — the PATH
/// shape every aterm tab has (the rc hook puts `agents/` first) — walking absolute entries
/// in order and answering at the first of the two it meets; `false` with no PATH, or with
/// neither on it. What decides which of the two files `which` names as the copy that
/// runs ([`which_line`]): the twin carries the self-update hand-over, the shim does not.
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

/// Whether `tool` is an installed program whose build is live and holds `bin/<tool>`
/// while the managed shim `bin/<tool>` is ABSENT — and if so, which build and binary.
///
/// `run`, `which` and the `aterm <tool>` front door all read "installed" off the shim,
/// so a lost shim would read as never installed — with a remedy (`install`) that answers
/// up to date. This asks the store instead, the way `repair` does (`relay_shims`), so the
/// answer can name `aterm pkg repair`, which re-lays the shim (`repair_exposes`).
///
/// `Some` only when every one of these holds, so a name this machine turned off never
/// comes back to life through it:
/// * `tool` is an admissible, non-alias tool name, and it is the PROGRAM's own name — a
///   bundle's same-named binary (`store/trust/<n>/bin/ty`) never answers for `ty`;
/// * `bin/<tool>` is proved absent — a tombstone, a pending stub, a dev link or any
///   foreign file there is never looked past;
/// * `store/<tool>/current` names a COMPLETE build of this program (on a prefix older
///   than that link, the build its surviving shims point into), and that build's
///   `bin/<tool>` is a regular file;
/// * the program is not removed on purpose, not dev-linked, and not disabled here (a
///   `tombstoned:`/`aborted:` row, or a tombstone among its shims — `program_disabled_here`
///   — or a tombstone over any name its build ships).
#[must_use]
pub fn live_without_shim(layout: &crate::store::Layout, tool: &str) -> Option<LostShim> {
    live_without_shim_of(layout, tool, tool)
}

/// [`live_without_shim`] for `tool`, one of the binaries `program` ships: the reroute's
/// question for a branded tool (`trustfmt`, shipped by `trust`), whose table names the
/// owning program. The conditions above read `store/<program>/current` and `program`'s
/// state; the shim and the binary are `tool`'s.
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
        // No per-program link: a prefix from before it, where the build is the one the
        // program's shims point into. That walk reads every shim in `bin/`, and the
        // `aterm <word>` front door asks this for every word with no shim — so a name
        // with no store directory at all, which cannot be an installed program, stops
        // at one `stat` instead.
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
    // A tombstone over any name the BUILD ships. `program_disabled_here` finds one only
    // among the exposes the surviving shims spell, and with the own-name shim lost those
    // can be none at all (a tombstone resolves nowhere), so a revoked build whose `bin/tla`
    // is a tombstone and whose `bin/ty` is gone would pass it. A tombstone does not name
    // its program, so this is conservative: a same-named tombstone of another program
    // keeps the old answer here.
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
/// exec'd from the store with the environment its build declares and a notice naming the
/// repair; else not installed.
///
/// An AGENT PROGRAM with a lost shim is named and NOT run: its run hands a vendor's
/// self-update verb to the package manager only when the shim resolves
/// ([`runs_self_update_verb`]), so exec'ing the store copy here would let `aterm claude
/// update` reach the vendor's updater. The repair re-lays the shim, and then it runs.
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

/// The `--progress-file` path this invocation carries, if any — set once at the
/// dispatch edge by [`strip_progress_file_flag`], read by the provisioning passes.
/// Process-global because the pass that consumes it (`install_default_set_with_path`) sits
/// several call layers below verbs whose signatures are shared with paths that
/// must never know about it.
static PROGRESS_FILE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Remove every `--progress-file <path>` pair from `args`, recording the FIRST path
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

/// `<prefix>/progress.json` when a PERSON typed this pass on a terminal — set at the
/// dispatch edge with the meter ([`typed_pass_meter`]), never beside `--progress-file`.
/// Its pass is what the meter draws, and what a pending stub in another shell reads.
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

/// A progress pass of a person's typed verb, over work the window's lanes meter nowhere
/// (an update's channel apply and vendor lane, one program's install or update): begun
/// only when this verb was typed on a terminal ([`tty_progress_path`]) and no pass is
/// live, and ended when dropped. No plan: each program's row appears as its download
/// starts, which is all the meter needs.
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

/// The meter under a pass verb a PERSON typed (`install`, `update`, `seed`): stderr is a
/// terminal ([`crate::meter::wanted`]) and the window did not ask for a progress file. The
/// pass then writes `<prefix>/progress.json` as the window's does, and the meter draws it;
/// between passes — the index check before, the tidying after — it says so with a spinner
/// once a second has gone by with nothing to show. `None` everywhere else.
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

/// The flag the head watch passes with `update <program>` (design §1.6) — the window's, or
/// with no window open a terminal session's ([`crate::vendor_direct::watch::Runner`]): the
/// pass is the unattended lane's ([`VendorDoor::Window`]), not a person's. Machinery for
/// those spawns, never user vocabulary.
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

/// The flag the UNATTENDED lanes pass with a whole `update` — the window's package loop
/// and a terminal session's detached pass: the coherence group holding the Trust toolchain
/// stages now and flips when nothing is using the toolchain it replaces, four hours at the
/// most ([`crate::quiet`], gap #12(b)). Without it — a person's `aterm pkg update`, typed
/// or from Settings — the flip lands at once. Machinery for the lanes' spawns, never user
/// vocabulary; ON for every pass those lanes run.
pub const DEFER_BUSY_FLIP_FLAG: &str = "--defer-busy-flip";

/// The word a window under a private state root puts AHEAD OF THE VERB of every pass it
/// spawns (ruling 410): the pass carries no `[machine]` edit, whatever its own build is.
///
/// A pass decides the edge in its own process (`under_private_state_root`), and a
/// window's passes run the `atpkg` beside its executable (`aterm-gui`'s
/// `co_located_atpkg`), which in a development tree is a separate build: one that compiles no
/// seam reads the inherited `ATERM_STATE_HOME` as unset and would carry the instance's
/// `[machine]` table to the owner's Mac. So the window, which knows it is private, says
/// so. First, not after the verb, on purpose: every `atpkg` reads its first word as the
/// verb and asks the `[machine]` edge only for a pass verb, so one built before this flag
/// answers the lead as an unknown verb — exit 2, nothing done — instead of applying.
/// Machinery for the window's spawns, never user vocabulary; a shipped window, which reads
/// no seam, never passes it.
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

/// What the dispatch edge recorded from `--wait-lock <secs>`: the bound, and the pid
/// of the parent that spawned this process at that moment — the window — so a
/// waiter can tell "my parent is gone" as a CHANGE of parent rather than as
/// "re-parented to pid 1", which is true on macOS (orphans go to launchd) and false
/// on a Linux box with a subreaper (`systemd --user`, most container and sandbox
/// runtimes), where an orphan re-parents to the subreaper's pid instead.
#[derive(Clone, Copy, Debug)]
struct WaitLock {
    bound: std::time::Duration,
    spawner: Option<u32>,
}

/// The [`WaitLock`] this invocation carries, if any — set once at the dispatch edge
/// by [`main_entry`] from the pure [`take_wait_lock_flag`]'s return value, read
/// through [`wait_lock`] by [`mutator_store_lock`] and by [`parent_is_gone`].
/// Process-global for the same reason as [`PROGRESS_FILE`]: the lock edge has no
/// argv of its own.
static WAIT_LOCK: std::sync::OnceLock<WaitLock> = std::sync::OnceLock::new();

/// Whether this process found the store lock CONTENDED at least once before it
/// took it ([`mutator_store_lock`]'s poll): set on every contended poll of a
/// `--wait-lock` wait, read by [`pass_ended_while_we_waited`].
static WAITED_FOR_LOCK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The store's full-pass count ([`crate::status::Status::pass_seq`]) when this queued
/// `update` started, read before it ever asked for the lock: a count that moved by the
/// time it holds the lock is a full pass that ended while it was queued
/// ([`pass_ended_while_we_waited`]).
static QUEUED_AT_PASS_SEQ: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

fn unix_now() -> i64 {
    aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX))
}

/// THE HOLDER DID THIS PASS. Lanes cannot see each other's children, so the machine-wide
/// "never back to back" is kept here: a full `update` child of a `--wait-lock` lane that
/// WAITED at the lock and finds the store's full-pass count moved since it was queued
/// (`queued_at`, read before its first lock attempt, against `seq_now`) stands down behind
/// the pass that ended, whatever it ended — `Some((how it ended, how long ago))`, so a
/// failed holder is reported as failed (audit PK-3). A lock taken at the first try waited
/// for nobody, and runs.
///
/// A COUNT, NOT A CLOCK: whole-second stamps cannot tell an end in the second the wait
/// began from one before it; the count moves once per recorded end. A holder too old to
/// count (`pass_seq` absent) is not stood behind. Pure.
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
    // A queued Published(45) child must not stand down behind a holder that
    // successfully checked 44. The pass-end witness is atomic with this end
    // time; an unknown witness or an attempt at the same target keeps the old
    // dedup rule, including when that same-target attempt failed.
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

/// What a full pass that stood down behind the one that ended while it waited says: that
/// pass's ending, as that pass would have said it to the window. Its caller exits as that
/// pass did — `ok` a quiet 0 on stdout, `offline` the offline code (retried soon, nothing
/// shown), `failed` 1 on stderr with that pass's own sentence (`said`), so the window's
/// failure ladder retries it rather than this child repeating it back to back — except a
/// pass that failed only for its cached index, which said nothing on stderr and so is
/// said on stdout ([`stand_down_exit`] decides the stream and code). No
/// marker, and nothing recorded: the record is the holder's. Pure.
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

/// How a full pass that stood down behind the one that ended while it waited EXITS
/// ([`stand_down_exit`]): its line, which stream carries it, and the exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandDown {
    /// The one line ([`stood_down_after_wait`]).
    pub line: String,
    /// Whether it goes to stderr — the stream the window reads as a failure's words.
    pub on_stderr: bool,
    /// The exit code.
    pub code: u8,
}

/// THE STAND-DOWN EXITS AS THE PASS IT STOOD BEHIND DID ([`stood_down_after_wait`]'s
/// contract): `ok` 0 on stdout; `offline` the offline code on stdout; `failed` 1 — on
/// stderr with that pass's sentence, EXCEPT when that pass failed only because its index
/// came from the cache (`stale_index_only`, [`crate::status::stale_index_only_end`]): that
/// pass exited 1 with NOTHING on stderr, so this one says its line on stdout and exits 1
/// the same way — the window reads a markerless exit 1 with empty stderr as a stale
/// index, not a failure. Pure; `pub` so the window's verdict test drives this decision.
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

    /// A child that waited stands down behind a full pass that ENDED since it was queued —
    /// the store's pass count moved — a success, a failed or an offline pass, with that
    /// pass's ending as its own. One that did not wait, whose count did
    /// not move, or whose holder counted nothing (an older atpkg), runs.
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
        // THE GAP THIS CLOSED: the holder's end stamped in the very second the child first
        // found the lock held. A clock could not tell it from an end before the wait; the
        // count can, whatever the stamp says.
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

/// The environment variable a spawner sets beside `--wait-lock` to say WHO it is —
/// its own pid — so the waiter's "my window is gone" is a comparison against a
/// fact, not against a guess. The guess (`getppid` at the dispatch edge) races a
/// parent that dies inside the child's few milliseconds of startup: by the time
/// the edge looks, the child is already re-parented, and the "spawner" it records
/// is init or the subreaper — the very pid it would later be compared against.
/// Hidden machinery like
/// `--wait-lock` itself; `aterm-gui` imports the name so a rename is a compile
/// error on both sides.
///
/// Unnamed, the edge records the parent it sees, and the waiter stands down when
/// its parent CHANGES from that ([`is_orphaned`]) — whoever that parent was. A
/// `nohup … &` job is no exception: its parent at the edge is the shell that
/// launched it, so when that shell exits the job is re-parented and its next
/// contended poll stands down, silently, with 75. `nohup` guards against SIGHUP,
/// not against this rule: a caller that wants its waiter to outlive it must keep
/// the job's parent alive, or the waiter stands down. Only a parent that is ALREADY init or launchd at the edge — a launchd
/// agent's job — never changes, and that waiter waits out its bound like a typed
/// one. A cron job run in the foreground waits too, though not for that reason:
/// its parent is cron's own child or the `sh` it runs, not init, and that parent
/// outlives the job. The same `ppid 1` that means "gone" for a window's child
/// means "spawned by launchd" for an agent's job, which is why the rule is a
/// change and never the pid alone. A caller that wants its waiter to stand down
/// when IT exits names itself here — which also covers the one shape the fallback
/// misses: an unnamed parent that dies inside the waiter's startup, before the
/// edge looks, so the edge records init or the subreaper and the waiter polls out
/// its bound (the documented degradation; no window is unnamed).
///
/// A spawner that DETACHES its pass on purpose — the terminal session's one-shot pass,
/// whose detaching `sh` exits at once — sets [`SPAWNER_DETACHED`] instead: that waiter
/// has no parent to outlive, so it names none and waits out its bound.
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

/// The pid of the process that spawned this one, as recorded at the dispatch edge:
/// what the spawner SAID ([`SPAWNER_PID_ENV`]) when it said anything — nobody, for a
/// pass detached on purpose ([`SPAWNER_DETACHED`]) — else the parent pid as seen right
/// now (a typed `--wait-lock` from a shell).
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

/// Whether a waiter spawned by `spawner` has been orphaned, given its parent pid
/// `now`: its parent is no longer the spawner — init on macOS and a subreaper-less
/// Linux, the subreaper's pid where there is one — and both count because the rule
/// is a CHANGE, never "pid 1" on its own. A parent nobody can read (`now == None`:
/// std has no notion of one on Windows) is never judged gone, and neither is a
/// spawner nobody could name; both poll out the bound instead — still a TRY at
/// the end, still store-safe. Init as the parent is NOT gone by itself: an
/// unnamed waiter whose parent was already init at the edge (a launchd agent's
/// job) recorded init as its spawner, and that spawner never changes. Any other
/// recorded parent can: a `nohup … &` job recorded the shell that launched it, and
/// is orphaned once that shell exits (see [`SPAWNER_PID_ENV`]). Pure for the test.
fn is_orphaned(spawner: Option<u32>, now: Option<u32>) -> bool {
    matches!((spawner, now), (Some(spawner), Some(now)) if now != spawner)
}

/// Remove every `--wait-lock <secs>` pair from `args`, returning the LAST parsable
/// value. Pure (no static) so the edge grammar is unit-testable. An unparsable VALUE
/// degrades to "no wait" — the `--progress-file` doctrine: the flag is machinery for
/// the window's spawn, never user vocabulary, and a broken spawn must degrade to the
/// fail-fast try, never to a refused verb. The token after the flag is consumed
/// either way, so it can never be read as a program name — which means a VALUELESS
/// flag eats whatever follows it: `atpkg install --wait-lock --default-set` loses
/// `--default-set` and is refused as a bare `install` (the usage, exit 2). Only a
/// valueless flag at the END of argv degrades to "no wait"; no spawner produces the
/// valueless shape (the window always passes a number).
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
/// (the stub's contract: the tool did not run) — unless a person at a terminal can
/// wait for a program a pass is installing, which then runs ([`run_pending`]). It never
/// claims progress it cannot read, nor a bump the installer would silently drop.
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

/// The copy of an AGENT program's stub that runs MEANWHILE: a foreign `claude`/`codex`
/// on `path_var` outside the managed prefix (a brew cask, Anthropic's native installer),
/// `None` for every other program and when no such copy exists. The agent stubs are laid
/// in `agents/`, first on every PATH, and a stub that ran INSTEAD of the user's working
/// copy for the minutes of an install would be the manager fighting its owner; so the
/// stub says its one line on stderr and execs the copy the user had, with the arguments
/// verbatim. The managed twin replaces the stub the moment the program installs.
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
/// `agents/` twin execs while `<prefix>/landing/<program>` stands ([`crate::landing`]).
/// It `exec`s the current `bin/<program>` shim at once and prints nothing: the pass
/// flips `bin/` atomically, so the next invocation already runs the new build. On Windows
/// the `.cmd` shim runs as a child instead, and its real exit code is this one's.
fn cmd_landing(rest: &[String]) -> ExitCode {
    // The operands, parsed by the pure half ([`crate::landing::HandOver::parse`]): the
    // program; the PREFIX operand the twin's prelude passes (absolute, laid from the
    // layout that owns the twin) so the store is found with no `HOME` — `env -i`,
    // launchd, a container entrypoint — where `layout()` would fail with the tool never
    // run (an older twin passes none); and the user's arguments with the ONE `--` the
    // twin inserted stripped, so a user's own leading `--` survives.
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
    // The CURRENT `bin/<program>` shim — never the `agents/` twin, whose prelude would
    // hand over here again. `platform::exec_or_run` is the `exec(2)` on Unix; on Windows
    // it is a child with inherited stdio whose REAL exit code this process exits with.
    // Either way it returns only the error that stopped it.
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

/// Carry out what [`selfupdate_plan`] decided — every ending of [`cmd_selfupdate`] after
/// its operands: the refusal and disabled lines with their codes, the forward's `exec`,
/// the check's child. Its own function because `atpkg run` ([`cmd_run`]) settles the
/// same plan for every ending but the forward, which it runs its own way.
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
            // The `bin/<program>` shim with the arguments verbatim: `exec(2)` on Unix, a
            // child with the real exit code on Windows ([`crate::platform::exec_or_run`]).
            // An agent program's `bin/` carries the self-update block too, and the
            // `exec` keeps this pid, so the block's re-entry guard
            // ([`crate::selfupdate::REENTRY_VAR`]) runs the store build instead of handing
            // over here again.
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
    /// `exec` the `bin/<program>` shim with the arguments verbatim — after `note` when
    /// there is one (the help note); nothing printed for a pass-through or a program
    /// with no row. `atpkg run` prints the note and runs the tool as it runs every other
    /// argument list instead ([`cmd_run`]).
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

/// Steps 2–6 of [`cmd_selfupdate`], pure: the row, the prefix cross-check against what
/// this environment `resolved` (`None` ⇒ nothing to check against), the
/// classification, the manager's enablement. No environment is read here: `enabled` is
/// the one fact the process supplies (`manager_enabled()`), and there is no other.
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
    // A declined shape is simply declined (exit 2, nothing run): the vendor's own
    // updater is never run through the managed name — the copy it installs is one this
    // name never runs — and no bypass is named, because there is none. Help and a
    // pass-through forward, with their own note or none.
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
        // The words typed are the classification's evidence, not the line's: the
        // announce line states what the person HAS, not what they typed —
        // that is the line above it in every transcript.
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
/// ([`crate::stub::co_located_atpkg_path`] — the binary this process IS when it came
/// through the twin's hand-over) when that is an executable regular file (the twin's own
/// `[ -x ]`, [`crate::stub::is_live_atpkg`]), else this
/// process's own executable. The same code either way — but NOT the same dispatch: the
/// one binary routes on its name, and under any name but `atpkg` a bare `update` is
/// aterm's app-update lane, so [`crate::selfupdate::child_argv`] puts `pkg` ahead of the
/// argv for the fallback's `aterm`. The fallback is reached only off the twin's path —
/// the twin's own `[ -x "$__atpkg" ]` guarantees the embedded path from a hand-over; it covers a
/// hand-typed hidden verb from a dev `aterm` copied out of `target/` or a bundle
/// missing the alias symlink.
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
    // NOTHING UNASKED: this runs before every `cargo`, `rustc` and `rustfmt` a session
    // types, so a note about `aterm.toml` (a retired key, a development setting this build
    // drops) would be said before every build — and `[reroute] announce = false` could not
    // silence it. The doctor and Settings name the file's troubles.
    crate::notice::silence();
    let Some((upstream, args)) = rest.split_first() else {
        eprintln!(
            "usage: atpkg {} <upstream> [args…]",
            crate::reroute::HIDDEN_VERB
        );
        return ExitCode::from(2);
    };
    let Some(layout) = layout() else {
        // No resolvable store (no `HOME`, an `env -i` wrapper): `layout` has said why.
        // The upstream copy on PATH runs after one line (warn, never stop);
        // `[reroute] strict = true` refuses instead — nothing ran, the escape named,
        // exit 2, never an unexplained 1.
        return crate::reroute::run_storeless(upstream, args);
    };
    crate::reroute::run(&layout, upstream, args)
}

/// Re-assert the rustup SEAM: the managed `trust` toolchain linked into rustup as
/// channel `trust`, so `cargo +trust`, `targo` resolved through rustup, and every
/// repo pinning `channel = "trust"` all work on THIS machine. Without it a project
/// pinning the channel sees `'rustc' is not installed for the custom toolchain 'trust'`.
///
/// Fail-soft by construction: no rustup home ⇒ nothing to do; `reassert` only
/// ever creates a name nothing else owns, adopts a link already pointing into
/// the store, and REFUSES (with the fix printed) anything foreign under
/// `~/.rustup` — it never follows an entry aterm did not lay, and replaces exactly
/// one: a link to a live build tree older than the store's, nobody put back, with
/// no build running through it (`seam::live_build_tree`), recorded with the one
/// command that puts it back (`seam::replaced_from`).
///
/// Every call says what it left alone on the [`FOREIGN_MARKER`] line ([`FOREIGN_SEAM`]):
/// each foreign entry with [`crate::seam::DETACH_FIX`], or `none` — also with no rustup
/// home, where nothing can stand there.
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

/// Bring the trust EXEC ROOTS and every trust shim in line ([`crate::compat::reconcile`]),
/// print `atpkg: exec root: …` only for what changed or failed, and answer whether nothing
/// failed.
///
/// Why a pass-level call and not only the lay in `activate::install_tools_env`: an
/// up-to-date program is short-circuited before any shim is laid, and the alias and agents
/// reconciles write only their own names, never a primary, so a machine already on the
/// current trust build — whose `bin/rustc` is a separate copy of `trustc` that its tippy
/// refuses — would keep plain primaries until something re-laid them. Called from the
/// update pass (after its gc), the default-set lane (after its alias reconcile), `repair` (at `Deep`, where a failure is exit 1),
/// `seed` (beside the rustup seam) and a trust `rollback`. It prints nothing when nothing
/// changed and nothing failed, so it adds no line to the six-hourly tick.
///
/// Changes go to stdout, failures to stderr: the window that runs a pass reads only the
/// marker lines off its stdout and keeps its whole stderr as what the pass said
/// (aterm-gui's `read_seed_markers` and `read_pass_stderr`), and a root that could not be
/// laid is the line someone has to see.
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

/// The whole-set removal's companion ([`crate::seam::detach_recorded`]): detach every
/// rustup seam atpkg recorded, never `--force` — an entry atpkg did not lay is refused
/// and named, not removed. Nothing recorded, or no rustup home: silent.
fn detach_rustup_seams(layout: &crate::store::Layout) {
    let Some(home) = crate::seam::rustup_home() else {
        return;
    };
    for event in crate::seam::detach_recorded(layout, &home) {
        println!("atpkg: {event}");
    }
}

/// Lay (or refresh) the reroute stubs beside the pending-stub reconcile — at seed,
/// after each install pass, and on `repair` — and never let a failure fail the pass:
/// the stubs are a session convenience the spawn seam re-lays anyway, while the pass
/// that called this just installed a toolchain the user is waiting for. One stderr
/// line names what did not happen.
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

/// The `managed` state `which` prints for `program` at `build`: the row's own pin when
/// the row is a managed one; otherwise — the row a fault (`aborted: stage`, `error: …`)
/// or absent — the CACHED index's answer, so the line says what the index pins and that
/// the pass did not get there — never `pinned by index <floor>` over a faulted row.
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
    // A managed row that claims no pin — `managed <build> — rolled back from <from>` — is
    // this build's own record, and its words lead: never `pinned by index N` for a build
    // the index does not pin.
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
/// vendor lane recorded for that build (`managed 2.1.280 — Anthropic latest`, or a kept
/// reason); else `… — Anthropic latest` when the program's stamp last saw the head at this
/// version; else `managed <version> — updates from <Vendor>`. Never an index pin: no index
/// pins it, whatever an older client's row says.
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

/// The pending verb's body, shared by `__pending` and `atpkg run`'s pending arm so
/// the three dead ends (the shell stub, `aterm <tool>`, `atpkg run <tool>`) converge
/// on ONE state machine. Interactive iff stdin AND stdout are terminals.
///
/// Exits 127 — whatever was printed, the command the user typed did not run — with TWO
/// exceptions. An AGENT program with a foreign copy to run meanwhile
/// ([`pending_passthrough`]) has that copy `exec`ed in this process's place,
/// so the user's command really does run and this never returns; 126 if that `exec`
/// itself fails, and off Unix, where there is no `exec`, the 127 stands. And a caller whose
/// program a running pass is installing waits for it and has it run the moment it lands
/// ([`wait_then_run`]): a person at a terminal under the meter, a script or an agent's
/// tool call with a line on stderr every few seconds and a bound (2026-10-15 — the agent
/// primer sends every agent to `targo` as soon as the shell hook sets `$ATPKG_BIN`, and
/// "try again in a minute" was a dead end for a tool call that cannot).
fn run_pending(layout: &crate::store::Layout, tool: &str, passthrough: &[String]) -> ExitCode {
    use std::io::IsTerminal as _;
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    // An agent program with a foreign copy to run meanwhile says its lines on STDERR:
    // the copy's stdout is the user's (a pipe, a script), and the state is a note.
    let onward = pending_passthrough(layout, tool, std::env::var_os("PATH").as_deref());
    let show = if interactive && crate::meter::wanted() {
        WaitShow::Meter
    } else {
        WaitShow::Lines
    };
    // A PERSON asked when the stub reads from a terminal and speaks to one; a script, a pipe
    // or an agent's tool call has neither, and its bump does not jump the toolchain.
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

/// Select the snapshot whose rows belong to a pending command's wait. The witness
/// starts before admission and can advance to a pass actually observed live.
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

/// How long an UNATTENDED caller waits for its program ([`WaitShow::Lines`]): a whole
/// first install of the toolset fits, and a tool call is never held past it. The bound
/// is a hang detector for a pass that stalls, never a latency budget.
const PENDING_UNATTENDED_WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// How often an unattended wait says where its program is ([`WaitShow::Lines`]).
const PENDING_UNATTENDED_SAY_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// The line an unattended wait says every [`PENDING_UNATTENDED_SAY_EVERY`]: what was typed,
/// the program it waits for, and how far that program is — the downloaded share while its
/// size is known, else its phase.
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
/// ([`PendingNext::WaitThenRun`]), and the moment its shim resolves the program runs —
/// `exec`ed in this process's place with the arguments they typed, so their command really
/// does run. A person at a terminal ([`WaitShow::Meter`]) watches the meter follow the
/// running pass's `progress.json` for this one program; Ctrl-C stops the wait (the meter's
/// line is cleared and the process dies of the signal, 130 to the shell). An unattended
/// caller ([`WaitShow::Lines`]) gets one stderr line every few seconds and gives up after
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
    // Whether a pass of this wait ever planned the program: one that never did — a promise
    // broken before any pass began, a pass that stopped before it planned (offline) — is
    // answered as nothing installing ([`pending_not_running`]), not as an install that
    // stopped.
    let mut planned = false;
    // Whether this wait (or its caller) has seen the promise: its retirement then says the
    // promised pass planned (or ended), so a live default-set pass with no row for the
    // program is a plan that left it out.
    let mut promised = seen.promise;
    let mut witness = seen.snapshot.cloned();
    let mut witness_running = seen.running;
    let outcome = loop {
        // THE PROMISE, THEN THE PASS, THEN THE SHIM ([`pending_wait_step`]): an update retires
        // the promise only once its pass planned or ended, and a pass lays a program's shim
        // before it ends — so a promise read as gone was kept or broken for good, a live pass
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
        // A PLAN THAT LEFT IT OUT: the promise this wait saw is gone, retired once its pass
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
    // It landed: run it through `atpkg run <tool>` itself ([`cmd_run`]), so it gets every
    // rule that door keeps — the exec root, the build's declared environment
    // (`ops::exec_env`), the self-update plan — rather than a second copy of the exec.
    // The explicit `--` is the one separator `cmd_run` drops, so the tool's own
    // arguments reach it untouched. A name still laid as a pending stub is answered here:
    // handing it on would land back in this wait.
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

/// How much of a HELD row's failure (`why`) the pending stub echoes: the longest
/// digest-pair sentence a memo records whole (`asset sha256 mismatch: expected <64 hex>,
/// got <64 hex>` is 166 characters, the `tree_root` one 163), so the reader can hold it
/// against the ledger; a longer signer sentence is elided.
const HELD_WHY_CAP: usize = 200;

/// …and how much of its retry (`how`). Whole for every row atpkg writes — at most about
/// 240 characters of its own words plus a signed asset name and a program name
/// (`flow::digest_refusal_note`) — and still finite against a hostile record.
const HELD_HOW_CAP: usize = 2 * PENDING_ERROR_CAP;

/// The pending stub's line for a member HELD after a failed stage — `held` is the row
/// after [`crate::state::HELD_FAILED_PREFIX`]: `<why>; <how>`. The two halves are capped
/// APART so the retry always survives a long digest sentence and asset name. The first
/// `; ` is the seam by construction ([`crate::state::held_failed`] keeps it out of the
/// `why`).
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

/// The `__pending` state machine's I/O seam, so every state is exercisable without a
/// process, a TTY or an environment variable: `say` receives each honest line in order
/// (production prints it; tests collect it).
struct PendingIo<'a> {
    say: &'a mut dyn FnMut(String),
    /// Whether the caller can wait for a program a running pass is installing, and how
    /// the wait shows — then the answer is to wait and run it
    /// ([`PendingNext::WaitThenRun`]), never "run it again later". `None` when a copy of
    /// the program runs meanwhile instead ([`pending_passthrough`]).
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
    /// A running pass is installing the program: wait under the meter, then run it with
    /// the arguments the person typed ([`wait_then_run`]).
    WaitThenRun,
    /// A pass is promised and has not planned yet: the same wait, which starts knowing the
    /// promise was seen, so an update that retires it before the wait's first read still
    /// ends the wait at once when its plan left the program out.
    WaitForPromise,
}

/// The five honest states (+ the honesty edge), ONE function for the three dead ends.
/// SIDE-EFFECTFUL on purpose: the bump append is part of the state machine — a claimed
/// bump must really have been written — so the tests exercise message and channel
/// together.
fn pending_state(layout: &crate::store::Layout, tool: &str, io: &mut PendingIo<'_>) -> PendingNext {
    // The name gate first: only an admitted ToolName is ever echoed back to the TTY
    // (an unadmitted one could never have a stub OR a shim, so nothing pends).
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
    // THE PROMISE, THEN THE PASS — the order [`wait_then_run`] reads them in.
    let owed = crate::progress::pass_owed(layout, now);
    let snapshot = crate::progress::read_progress(layout);
    // A pass that is installing something ELSE — a person's typed `update`, or a single
    // `install` of another program — says nothing about this one: its snapshot never
    // plans the default set, so its silence about this name is no delisting. Nor is a
    // promised pass's before its plan lands: the promise answers below.
    let running = snapshot.as_ref().is_some_and(|f| {
        crate::progress::snapshot_running(f, now)
            && (f.programs.contains_key(name)
                || (!owed && matches!(f.pass.as_str(), "net" | "seed")))
    });
    if let Some(file) = snapshot.as_ref().filter(|_| running) {
        if file.v != crate::progress::PROGRESS_VERSION {
            // A NEWER writer's file: render the generic line, never a guess at
            // fields whose meaning may have changed. The bump file's contract is
            // versionless (names only), so the bump is still honest.
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
            // THE HONESTY EDGE: a resolved-pass snapshot is readable and does not
            // plan this program — a compiled-roster stub the signed index no longer
            // lists. Claiming it is next would be a lie the reorder-only intersection
            // silently swallows; the reconcile removes this stub at pass end.
            (io.say)(not_in_default_set_line(name));
            return PendingNext::Done;
        };
        use crate::progress::Phase;
        // A PERSON AT A TERMINAL WAITS FOR IT under the meter, and the program runs the
        // moment it lands — bumped first when still queued, so the wait is short.
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
                // SKIPPED IS TERMINAL, NOT A WAIT: [`crate::progress::Sink::finished`]
                // retires a skipped program from the queue, and the bump file only
                // reorders what is still queued, so no bump could put it back. Say the
                // recorded reason and claim no bump.
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
    // A PASS IS PROMISED and has not planned yet ([`crate::progress::pass_owed`]): the
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

/// The honesty edge's line: a default-set pass is running and its plan leaves `name` out
/// ([`pending_state`], and a wait whose promised pass planned without it,
/// [`wait_then_run`]).
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
    // NOT RUNNING (no file, stale heartbeat, dead pid, or a terminal snapshot):
    // only not-running states may render, whatever the snapshot claims. A recorded
    // failure outranks the generic line — every failure names its next act.
    if let Some(status) = crate::status::read(layout) {
        // A member HELD after a failed stage (`held: last attempt failed with …`): the
        // row already names the failure and the retry, so say it and claim nothing —
        // no "the next pass retries it" (it does not: the hold is the point) and no
        // bump (a pass can only repeat the hold). Before the `error` arm, which would
        // promise exactly that.
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
        // A member the pinned build does not serve on THIS target — the canonical
        // `unavailable on <target>: <hint>` row the pass records per program — is not
        // coming on its own either: say the row's own words (the hint names why), and
        // do not bump: nothing can move.
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
        // A BLOCKED toolset outranks the generic promise: the reconcile pass records
        // the `*toolset*` row so this stub, which may outlive the pass by months, can
        // say the registry publishes no build for this machine.
        if let Some(row) = status.programs.get("*toolset*")
            && let Some(why) = row.state.strip_prefix("blocked: ")
        {
            say(format!(
                "atpkg: {name} is not coming on its own: {} — Settings ▸ Packages \
                 shows details.",
                crate::progress::sanitize_for_tty(why, PENDING_ERROR_CAP)
            ));
            // Still worth a bump: a triple the index has since learned to serve
            // gets an immediate pass from the GUI's bump watch; an unserved-triple
            // pass re-skips quietly.
            let _ = crate::progress::append_bump(layout, program, asker);
            return;
        }
    }
    // BUMP FIRST, THEN SAY WHAT THAT MEANS: an open aterm window stats the bump file
    // every 5 s and turns a fresh bump into a pass, so the sentence depends on whether
    // the bump was recorded.
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

/// After an [`crate::FlowError::Unreachable`] failure line, name the ONE act that
/// resumes the flow: nothing retries a foreground verb. The re-run is per-verb, so the
/// edge that knows the verb appends it; when the reason is the anonymous GitHub budget,
/// it says when that resets.
fn print_unreachable_followup(e: &crate::FlowError, rerun: &str) {
    let (crate::FlowError::Unreachable(why) | crate::FlowError::VendorUnreachable(why)) = e else {
        return;
    };
    eprintln!("atpkg:   when the connection is back, re-run: {rerun}");
    if why.contains("403") || why.to_ascii_lowercase().contains("rate limit") {
        eprintln!("atpkg:   the anonymous GitHub limit resets within the hour");
    }
}

/// Resolve the install layout, or print why we can't and return `None`.
///
/// Through the chain-validated `[packages].prefix` override
/// ([`crate::store::resolve_configured`]), which matters for a shared multi-user store
/// (`docs/GOLDEN-INSTALL-PATH.md` §2).
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

/// Refuse any argument to a verb that takes none — `Some(usage error)` when one
/// was given, `None` to proceed — so a swallowed flag never reads as accepted (audit
/// D-12). `-h`/`--help` never reaches here.
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
        Arity::Own => None,
    }
}

/// What `verb` answers when `rest` (the argv after it) names one program built into aterm
/// ([`crate::builtin::verb_answer`]): its line on stdout and exit 0, or its refusal on
/// stderr and exit 1. `None` to run the verb. The verb's own flags are not names, as
/// [`arity_refusal`] counts them, so `update amacs --retry` is answered as `update amacs`.
fn built_in_answer(verb: &str, rest: &[String]) -> Option<ExitCode> {
    let flags = match verb_row(verb)?.arity {
        Arity::One { flags } => flags,
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

/// Acquire the store-wide writer lock for a mutating verb at the dispatch edge: a
/// TRY by default, or — when the edge recorded `--wait-lock <secs>` — a BOUNDED WAIT
/// ([`crate::lock::lock_store_waiting`]) that queues behind a sibling's pass and
/// announces itself ONCE on stdout as the [`LOCK_WAITING_MARKER`] line (this is that
/// marker's one production emission site). `Ok(None)` when no prefix resolves (HOME
/// unset) — nothing to lock, and the verb itself refuses with its own message
/// moments later. Every refusal stays fail-closed and prints the loud one-line
/// sentence: contention — fail-fast or timed out — exits
/// [`crate::lock::CONTENDED_EXIT`] (75) so a caller classifies it by code; an
/// unusable lock file (`Io`) exits 1. A waiter whose parent has died (the window
/// quit — its pipes are gone) stands down silently with the same code at its next
/// CONTENDED poll: the wait consults [`parent_is_gone`] only when a try finds the
/// lock still held (before the announcement, before the deadline), so an orphan
/// whose holder lets go within that poll interval (≤ 500 ms) takes the lock and runs
/// its verb — the race with the successor's own waiter is narrowed, not closed. And
/// every line a waiter prints — the `lock-waiting:` announcement, the refusal
/// sentence on stderr and the `seed-busy:` terminal ([`emit_marker_line_lossy`]) —
/// is a write whose failure is ignored, so a window that quits between the check and
/// the print costs an `EPIPE` nobody sees rather than a panic (Rust leaves `SIGPIPE`
/// ignored and `println!` would panic on the dead pipe).
///
/// A PERSON'S VERB WAITS TOO. A verb typed on a terminal — stdin and stderr both one, no
/// `--wait-lock` ([`a_person_typed_this`]) — waits up to [`TYPED_LOCK_WAIT`] behind a
/// pass it did not start, silent for the first [`crate::lock::WAIT_ANNOUNCE_GRACE`], then
/// with a spinner ([`lock_wait_spinner`]), and runs when the holder lets go. Its stdout
/// stays silent; a wait that runs out says so in one sentence and exits 75. Not on a
/// terminal, a typed verb refuses at once.
fn mutator_store_lock() -> Result<Option<crate::lock::StoreLock>, ExitCode> {
    let Some(layout) = crate::store::resolve_configured() else {
        return Ok(None);
    };
    // Whether the wait ANNOUNCED itself: only an announced wait is answered below
    // (`lock-acquired:`) — a wait that ended inside the grace put nothing on the
    // glass, so there is nothing to retire.
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
                // Asked only on a CONTENDED poll: the lock was held by someone
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
            // THE WAIT IS OVER, said once and only after it was announced: the verb
            // that follows may be quiet for the whole length of its own work under
            // the lock, and the GUI's waiting row has nothing else to retire it
            // before this child exits. A lossy write, like the
            // announcement: the window may have quit meanwhile.
            if announced.get() {
                emit_marker_line_lossy(&format!(
                    "atpkg: {LOCK_ACQUIRED_MARKER}the other process finished \u{2014} \
                     running now"
                ));
            }
            // EVERY PASS SWEEPS `landing/` and `optin/` (§5.3(c)): holding the lock, this
            // pass is the only one, and no pass of this
            // client writes either, so what stands there is an older client's leftover
            // ([`crate::landing::sweep`]).
            crate::landing::sweep(&layout);
            // Who holds it, for a Settings Check deciding whether to wait (a person's
            // typed verb it answers at once; every lane it queues behind). A person is a
            // terminal on stdin and no `--wait-lock`: that flag is the lanes' machinery,
            // never a person's vocabulary, and a window started from a terminal hands its
            // tty to children that do not null their stdin.
            let mut guard = guard;
            use std::io::IsTerminal as _;
            guard.announce_holder(
                &layout,
                wait_lock().is_none() && std::io::stdin().is_terminal(),
            );
            Ok(Some(guard))
        }
        Err(e) => {
            // An orphaned waiter has nobody to tell — its stderr and stdout are the
            // dead window's pipes — and the exit code says it all. That guard is a
            // snapshot, and a window can quit between it and the prints below, so a
            // WAITER's two lines are writes whose failure is ignored — an `EPIPE`
            // nobody sees — never the `eprintln!`/`println!` pair, which panics on a
            // dead pipe and would trade the promised silent 75 for a 101. A typed
            // verb keeps the macros: its stderr is a person's terminal.
            if !parent_is_gone() {
                let waited = wait_lock().map(|w| w.bound);
                let contended = matches!(e, crate::lock::StoreLockError::Contended(_));
                match waited {
                    // A `--wait-lock` caller on a PERSON's terminal — `claude update`'s
                    // child inherits it — gets ONE sentence for the wait that ran out,
                    // not the lock module's refusal ahead of it. Under the intercept that
                    // sentence is the epilogue the parent prints
                    // (`selfupdate::contended_line`), so the child says nothing here; a
                    // hand-typed `--wait-lock` gets the typed lane's sentence with its
                    // own bound. Every pipe keeps the Display, byte for byte.
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
                // CONTENTION GETS A MARKER; every other refusal does not. An unwritable
                // prefix or a broken lock file is a real outage and must keep reaching
                // the GUI's refusal card. A lock held by another `atpkg` is the
                // opposite: that process is doing this work right now, and the only
                // honest thing to say about THIS pass is that it stood aside (see
                // [`SEED_BUSY_MARKER`]). Said to the `--wait-lock` lane only — the
                // GUI's machine lanes, and the self-update intercept's child, which opts
                // in on behalf of a person who typed `claude update`
                // — like the `lock-waiting:` line above: a typed `aterm pkg` verb is told
                // on stderr and keeps its stdout silent (the two-process proof pins
                // both), and the exit code below says the same thing to every caller.
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

/// How long a verb a person typed waits for the store ([`mutator_store_lock`]): long
/// enough for the window's background pass to finish a large update, short enough that
/// a wedged holder ends in a sentence rather than an endless spinner.
const TYPED_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Whether a PERSON typed this verb and is watching it: stdin and stderr are both a
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

/// A wait that ran out — a person's typed verb ([`TYPED_LOCK_WAIT`]) or a `--wait-lock`
/// caller on a person's terminal with its own `bound` — in the person's words, the bound
/// rendered as they read it (`10 min`, `30 min`; [`crate::selfupdate::wait_words`]).
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

/// Whether this process has been orphaned: its parent — the aterm window that
/// spawned it, which named itself in [`SPAWNER_PID_ENV`] or was read at the edge
/// with the `--wait-lock` flag — is gone, which shows as a different parent pid
/// now: init on macOS, a subreaper on Linux ([`is_orphaned`]). Only a `--wait-lock`
/// waiter asks (no recorded wait ⇒ always false). A waiter holds nothing and prints
/// at most one line while it polls — the `lock-waiting:` announcement, once the
/// grace has passed — so it is the one atpkg child that would otherwise outlive its
/// window for the whole bound and then race the successor's own waiter for the freed
/// lock. It is asked only on a CONTENDED poll, so that race is narrowed to one poll
/// interval (≤ 500 ms: an orphan whose holder lets go inside it still takes the
/// lock), not closed. An INSTALLING child is a different case — it KEEPS RUNNING:
/// the orphan watch ([`crate::progress::watch_for_orphaning`]) re-points its stdio at
/// `orphan-pass.log` when its window goes, so it finishes its pass and holds the lock
/// for the whole of it (the store is crash-consistent under a death too, `crate::lock`).
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
    // HOME unset: the head still answers (posture is a fact about the BINARY), with no
    // counts to claim and no next act to name — a next command would fail in the same
    // broken environment that emptied the tail.
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

/// `atpkg which <tool>` — ONE line: the copy that runs and why, in the canonical state
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

/// The one-line answer to "which copy of `tool` runs, and why?" — design S6. Offline: it
/// reads the managed shim, the process `PATH` (`path_var`), the status record and the
/// retirement marker; it never fetches. In `PATH` order the first copy wins, and the
/// managed `bin/` is appended LAST inside an aterm session, so:
///
/// * a managed shim with a foreign copy ahead of it on `PATH` ⇒
///   `<tool> → <foreign> — managed <build> — SHADOWED by <foreign>`;
/// * a managed shim otherwise ⇒
///   `<tool> → <prefix>/bin/<tool> → <store path> — managed <build> — pinned by index <N>`
///   (the index from the recorded row, else the durable floor) — or, for a vendor-direct
///   program, `… — managed 2.1.280 — Anthropic latest` ([`vendor_state_of`]);
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
/// Reads the shell's own environment for what an agents stub decides on
/// ([`crate::reroute::StubEnv::of_process`]); [`which_line_in`] takes it injected.
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
    // A REROUTED upstream name (philosophy §4) is answered by the table, ahead of the
    // deny-list gate and the store: inside an aterm session the reroute dir is FIRST on
    // PATH, so the stub is the copy that runs whatever else PATH holds, and the managed
    // bin/ never carries these names — `cargo`/`rustc` are deny-listed, and no index
    // program exposes the other six.
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
                "{tool} → {} → {} — dev-linked (managed from that checkout, not the index)",
                shim.display(),
                target.display()
            ));
        };
        // An agent program's reroute stub first on this PATH decides at exec time: the
        // probe goes through it ([`shadow_through`]), and a stub that
        // takes the twin is what the line names as running — only over a CURRENT twin:
        // one mid-reconcile resolves elsewhere, and the line below prints `bin/`'s target.
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
            // An AGENT PROGRAM whose twin is laid: the foreign copy runs in THIS shell
            // only, and the remedy is in place. The foreign copies line follows, labelled
            // `on this shell's PATH`, not `out-ranked`: here nothing out-ranks the copy
            // just named as running.
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
        // The canonical state, then — as a separate trailing sentence, never inside it —
        // what the managed copy runs with when its shim exports an environment (design
        // S7: `Claude Code's own updater is off here (…)`). Read off the shim that runs.
        let state = managed_state_of(layout, &program, build, row_of(&program).as_ref());
        // What runs first: where an agents stub routes, the stub and the (current) twin it
        // execs; where `agents/` is simply ahead of `bin/` on this PATH and the twin is
        // current, the twin — the file that carries the `update` hand-over the trailing
        // sentence describes; else the managed shim. Either way it resolves where `bin/`
        // does.
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
        let mut line = match shim_env_fix(&shim, &program) {
            Some(fix) => format!("{tool} → {runs} → {} — {state} — {fix}", target.display()),
            None => format!("{tool} → {runs} → {} — {state}", target.display()),
        };
        // …and, for a ROSTERED agent program ([`crate::selfupdate`]), one
        // more trailing sentence: what the vendor's own self-update verb does on this
        // name — `` `claude update` here runs `aterm pkg update claude` `` — for every
        // rostered member whether or not its shim exports an env (codex exports none, so
        // the fix-line alone could not carry it). Never for a shadowed row: the twin is
        // not what runs in that shell, and the sentence would be false there. And ONLY
        // where "here" is true: the laid twin must be current and
        // carry the hand-over ([`agent_twin_intercepts`] — the `.cmd` twin never does, a
        // twin from before the block does not until a pass re-lays it), and `agents/`
        // must be on THIS shell's `PATH` — the same two predicates the `managed-current:`
        // line and the shadow row already answer with. A shell whose `PATH` holds `bin/`
        // and not `agents/` (any shell outside aterm; the manual names it) runs
        // `bin/claude`, which carries the block too — but an older `bin/` does not, so
        // the sentence is withheld there: an under-claim, never a false one.
        if let Some(row) = crate::selfupdate::row_for(&program)
            && agent_twin_intercepts(layout, &program, build)
            // `agents/` on PATH decides only where no stub of ours answers first: one that
            // passes through skips `agents/` for `bin/`, whose hand-over is not what this
            // predicate reads — withheld there, an under-claim.
            && (routed.is_some()
                || matches!(answer, AgentStub::Absent)
                    && crate::vendor::agents_dir_on_path(&layout.prefix, path_var))
        {
            line.push_str(" — ");
            line.push_str(&crate::selfupdate::which_sentence(row));
        }
        // An AGENT PROGRAM's second line: the managed copy
        // runs because `agents/` is first on PATH, and the copies it beat — a vendor's
        // native install, a brew cask — are named so nobody has to `which -a` to learn
        // what a `brew upgrade` or Anthropic's own updater is still moving underneath.
        if crate::stub::is_agent_program(&program) {
            let foreign = crate::vendor::foreign_copies_on_path(&layout.prefix, tool, path_var);
            if !foreign.is_empty() {
                line.push('\n');
                line.push_str(&foreign_copies_line(&foreign));
            }
        }
        return Ok(line);
    }
    // 1a. A LIVE build whose own-name shim is LOST ([`live_without_shim`]): installed,
    //     its shim missing — not a bundle (arm 1b) and not uninstalled.
    if let Some(lost) = live_without_shim(layout, tool) {
        let state = managed_state_of(layout, tool, lost.build, row_of(tool).as_ref());
        // An AGENT PROGRAM's `agents/` twin execs the store build itself, not `bin/`, so
        // where it comes first the managed copy still runs — "nothing runs" and "a
        // foreign copy runs instead" would both be false there.
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
    // 1b. An index PROGRAM whose installed build exposes no tool of its own name
    //     (`trust` exposes trustc/targo/…, `trust-mc` exposes trust-mc-driver): the
    //     managed bundle answers, with the names it does put on PATH. A same-named
    //     unrelated binary (p11-kit's `trust`) is named as exactly that, after the truth.
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
    // 1b'. A program DEV-LINKED from a checkout whose own name is not a tool (`trust`
    //      exposes trustc/targo/…, never a `trust` binary): the link is what runs, and
    //      `which trust` must say so, before the not-installed arm below.
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
    //    that says WHY it waits (unserved here) outranks the generic promise.
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
    //    or a member HELD after a failed stage (the row names the retry).
    if let Some(row) = row_of(tool)
        && (row.state.starts_with(crate::state::UNAVAILABLE_PREFIX)
            || row.state.starts_with(crate::state::HELD_FAILED_PREFIX))
    {
        return Ok(format!("{tool} → (nothing runs) — {}", row.state));
    }
    // 5. An `alab-<x>` that nothing answers to: say honestly WHY there is no alias rather
    //    than promising `aterm pkg install alab-x` (no such program). The alias exists only
    //    for ALab's own tools, so an agent program or a system-satisfiable member never has
    //    one — the cached index says which, offline; an ALab tool that is simply not
    //    installed (or whose alias the next pass lays) gets the plain name's fix.
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

/// `which`'s answer for a name the cached `index` does not serve and no vendor program
/// answers to: `install` refuses it, so the line is install's own refusal, roster and
/// all. `None` with no cached index (it cannot tell) or for a name that installs.
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
/// (2.1.204)` — each path home-abbreviated, each with the version its real location
/// spells when it does ([`version_from_path`]); no parenthetical otherwise. Nothing is
/// executed to learn a version: `which` is a read-only verb and a foreign binary is
/// not ours to run.
fn foreign_copies_line(foreign: &[std::path::PathBuf]) -> String {
    foreign_copies_line_with("foreign copies out-ranked", foreign)
}

/// The label of the agent programs' second `which` line in a shell where a foreign copy
/// RUNS: `foreign copies on this shell's PATH: …` — every copy, the running one included.
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

/// The version a foreign copy's REAL location spells, read off the path alone: the
/// LAST path component of the canonical path that is dotted digits (`2.1.267` for
/// Anthropic's `~/.local/share/claude/versions/2.1.267`, `2.1.204` for Homebrew's
/// `Caskroom/claude-code/2.1.204/claude`, `0.151.0` for `Caskroom/codex/0.151.0/bin/codex`).
/// `None` when no component qualifies — the line then shows the path alone rather than a
/// guess. Symlinks are followed (`canonicalize`), which is where those spellings live.
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

/// The `which` answer for a REROUTED upstream name (philosophy §4): the stub that runs
/// inside a session, its state (`laid` / `missing` / `foreign`, [`crate::reroute::states`]),
/// and the row's policy in the stub's own words ([`crate::reroute::policy_summary`]) —
/// `<args>` standing for the caller's arguments, since `which` has none to fill in.
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
/// of [`alias_fix`]. Reads the §14 cached index (offline; absent ⇒ the base is unknown).
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
    // `exec_path`, not `which`: this execs the tool without its shim, so it has to take
    // the exec root the shim's guard would (`crate::compat` — `aterm tippy` on a trust
    // build whose tippy refuses the store's own `bin/rustc` copy lints only from there).
    // A laid pending stub gets the same live state + bump the stub itself prints (R6).
    // A live build whose shim is lost runs from the store and says so
    // ([`run_target`]); only a name nothing answers keeps the classic line.
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
    // THE TWIN'S SELF-UPDATE BLOCK, by the twin's rule: the first token exactly, only for
    // an agent program whose shim resolves into its own store tree (the one the twin is
    // laid for), and decided by the plan the twin's hand-over (`__selfupdate`) carries out
    // ([`selfupdate_plan`]): a declined shape exits 2, a disabled manager exits 1, and a
    // check runs the standard update as a child. Help is the one ending that runs the
    // tool, and this verb runs it itself — the note, then the `exec` below with the
    // build's environment. The hidden verb would `exec` `bin/<program>`, which exports
    // only what that shim does (a shim laid plain would run with the updater on). Unlike
    // the `.cmd` twin,
    // whose block is rendered empty (a batch line cannot hold user text inertly), this
    // arm is Rust on every platform.
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
    // `platform::exec_or_run` replaces this process on Unix (`execve`, never returns on
    // success) or spawns+waits+exits on Windows (no `execve`); reaching past it means the
    // launch itself failed, and the returned value is that error.
    // Raw, not `spawnfd::command`: `aterm pkg run` becomes the tool, so its caller's
    // descriptors are the tool's. Its Windows console is still decided here.
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

/// Whether [`cmd_run`] answers `tool args…` by the self-update plan ([`selfupdate_plan`])
/// — declined, disabled or checked instead of run; only help still runs the tool:
/// exactly when the `agents/` twin's block would hand them over
/// ([`crate::activate::reconcile_agents`] lays that twin only for an agent program whose
/// `bin/` shim resolves into an agent program's store tree, and its block matches `$1`
/// against the program's rostered verbs, [`crate::selfupdate::verbs_of`]). The FIRST
/// argument only, for the reason the block gives: `claude -p update` is a prompt.
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

/// `<toolchain-dir> [--who <words>] -- <command> [args…]`. The FIRST `--` ends atpkg's
/// argv, so every flag after it is the command's; before it, one directory and at most one
/// `--who`. `Err` names what is wrong, for the usage refusal.
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

/// The one line a lease carries ([`crate::lease::take`]'s `who`): the run's name — `--who`,
/// else the command's file name and the arguments after it — then the holder's pid and the
/// directory it runs in, which is what `aterm pkg doctor` prints beside the build it keeps.
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

/// THE HOLDER VERB's body: lease the toolchain `args.dir` belongs to under `prefix` (`None`:
/// no prefix resolves, so nothing can be leased — and nothing of atpkg's can reclaim a
/// build, either), say in one line what is held ([`lease_hold_line`] through `say`), run the
/// command as a child, wait for it standing aside from the terminal's ^C and passing a
/// TERM or HUP sent to the holder on to it ([`crate::platform::wait_standing_aside`]), and
/// let the lease go only once it has ended.
/// The lease's descriptor is close-on-exec, so no process the command starts — an `sccache`
/// server a build daemonizes — carries the lock past the command's own end.
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
    // TRANSPARENT, not `spawnfd::command`: the caller's own command, with ALL of its
    // caller's descriptors (a make jobserver's pipe, a `3>` redirect), whatever std this
    // binary links (`spawnfd::transparent`'s docs); its Windows console is `Quiet`.
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

/// `atpkg lane` ([`crate::lane`]): the Rust-lane reader over one shell command line.
/// Exit 0 when no stock Rust tool stands at command position (one stderr line when an
/// `ATERM_STOCK_REASON` cleared one), [`crate::reroute::REFUSAL_EXIT`] with the refusal
/// when one does, 1 when the input or the arguments cannot be read or the input is over
/// [`crate::lane::MAX_INPUT_BYTES`] — so a guard never reads a usage error as a refusal.
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

/// `atpkg lease <toolchain-dir> [--who <words>] -- <command> [args…]` — THE HOLDER VERB
/// ([`crate::lease::hold_for`]): hold the store build (or rustup view) a
/// multi-command run resolved for exactly as long as the command lives, so gc keeps it, the
/// seam does not re-lay it, and an unattended trust update waits for it. What a script
/// holds its toolchain with: macOS has no `flock(1)`, and a lock on a descriptor the script
/// keeps would be inherited by every daemon its build starts. (`atpkg-publish`'s packers
/// take the lease in-process, `atpkg::lease`.)
///
/// Exit: the command's own status — its code, or its signal re-raised
/// ([`crate::platform::exit_code_like`]) — 2 for a usage error, 1 when the directory is
/// gone (nothing ran), 127 when the command could not be started. A lease that cannot be
/// taken never stops the command: it runs, and the line says it runs unprotected.
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

/// What `atpkg repair`'s shell-integration step prints: the hooks headline, then one line per
/// rc file the pass edited, failed to edit, or will never edit; an rc that already carried the
/// block gets none. Repair is the one pass that re-lays a block the user deleted
/// ([`crate::hooks::refresh_rewiring_rc`]), so a re-laid opt-out is named with the way back
/// out rather than folded into a wholesale "rc wiring" claim.
fn repair_hook_lines(pass: &crate::hooks::HookPass) -> Vec<String> {
    use crate::hooks::{HookPass, RcOutcome};
    let mut lines = match pass {
        HookPass::Rewritten(_) => vec!["repair: ~/.aterm/shell.d hooks in place".to_string()],
        HookPass::HooksNotWritten => {
            return vec![
                "repair: a ~/.aterm/shell.d hook could NOT be written, so no rc was wired — \
                 atpkg's block may only name a hook that is on disk; the next pass retries \
                 both"
                    .to_string(),
            ];
        }
        HookPass::NotHardened => {
            return vec![
                "repair: shell integration NOT rewritten — $HOME is unset, or ~/.aterm or \
                 ~/.aterm/shell.d could not be made your own private (0700) directory (a \
                 symlink, a foreign owner or a group/other-writable mode there is refused); \
                 no rc was read or written"
                    .to_string(),
            ];
        }
        // Only the UNATTENDED pass stands aside for a non-release bundle; repair is the
        // asked-for pass and never produces this. Worded anyway, so no report is silent.
        HookPass::NotReleaseBundle => {
            return vec![
                "repair: shell integration NOT rewritten — this atpkg runs from an app bundle \
                 that is not the release aterm.app, and an unattended pass from there writes \
                 nothing"
                    .to_string(),
            ];
        }
    };
    for (rc, outcome) in pass.rc() {
        lines.push(match outcome {
            RcOutcome::AlreadyWired => continue,
            RcOutcome::Appended => format!(
                "repair: appended atpkg's block to ~/{rc} (it sources ~/.aterm/shell.d; \
                 delete the block to opt out)"
            ),
            RcOutcome::RelaidOverOptOut => format!(
                "repair: re-laid atpkg's block in ~/{rc} — you had deleted it; delete it \
                 again to opt out (install and update passes then leave it deleted)"
            ),
            RcOutcome::OptOutKept => {
                format!("repair: ~/{rc} left alone — you deleted atpkg's block")
            }
            RcOutcome::ConsentFenced => format!(
                "repair: ~/{rc} left alone — it resolves under a folder macOS guards with a \
                 consent dialog (Documents, Desktop, Downloads, Pictures, Movies, Music, \
                 iCloud Drive, a sync provider's folder, a mounted volume), which no atpkg \
                 pass opens; source ~/.aterm/shell.d from it yourself, or use the export \
                 line `aterm pkg doctor` prints"
            ),
            RcOutcome::Unreadable => format!(
                "repair: ~/{rc} left alone — atpkg edits only a regular file it can read as \
                 UTF-8 (a dangling link, a directory, a FIFO or device, or other bytes is \
                 never touched)"
            ),
            RcOutcome::WriteFailed => format!(
                "repair: ~/{rc} could NOT be rewritten (the file or its directory refused \
                 the write); atpkg's block is not in it"
            ),
        });
    }
    #[cfg(unix)]
    if pass.rc().is_empty() {
        let names: Vec<String> = crate::hooks::RC_FILES
            .iter()
            .map(|(rc, _)| format!("~/{rc}"))
            .collect();
        lines.push(format!(
            "repair: none of {} exists, so no rc was wired (atpkg never creates one)",
            names.join(", ")
        ));
    }
    lines
}

/// `atpkg repair` — put a half-installed store back into a state the rest of the CLI
/// can describe honestly.
///
/// (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md, B6.) Repair is made of the same
/// steps an install already runs, so there is one code path for laying shims and one for
/// shell integration:
///
/// 1. on macOS, from the installed aterm.app's atpkg only: move a copied `aterm`, `atpkg`
///    or `aterm-ctl` out of `~/.local/bin` (renamed beside itself) and link the app in
///    place of the first two — a copy it cannot move is reported and keeps the exit at 1;
///    then rewrite the shell hooks and wire them into the user's rc (PATH in every shell);
/// 2. re-lay each installed program's shims from its live `current` build (the newest
///    complete build only where no `current` link selects one) — never over a revoked
///    build's tombstone, which only an update pass may lift;
/// 3. lay — at `Deep` — the exec root of a trust build whose `bin/rustc` is a separate copy
///    of `trustc`, and route every trust shim through it ([`reconcile_exec_roots`]);
/// 4. name — and do not silently paper over — programs whose build is GONE, which no
///    local operation can fix and only a reinstall can.
///
/// Exits 1 when something remains that repair cannot do locally, so a script can tell
/// "fixed" from "needs the network" — exec roots the reconcile could not put in order
/// included.
fn run_repair(layout: Option<crate::store::Layout>) -> ExitCode {
    let Some(layout) = layout else {
        return ExitCode::from(1);
    };
    // A file at `~/.local/bin/aterm`, `atpkg` or `aterm-ctl` runs instead of the app, and a
    // copy never updates; only this asked-for pass moves it, and it says where to — or why
    // it could not, which keeps `repair` from saying "done" over it.
    let leftovers = repair_copied_commands();
    // Shell integration next: it is the half that needs no store at all, so a machine
    // with nothing installed still leaves repair better wired than it arrived. It REWIRES
    // an rc whose block the user deleted, which no unattended pass does (that deletion is
    // the opt-out the block documents, honoured through `hooks::RC_LEDGER`): repair is the
    // pass the user asks for. Say what happened, not what was attempted — one line per rc
    // it edited, left alone or skipped (`repair_hook_lines`).
    for line in repair_hook_lines(&crate::hooks::refresh_rewiring_rc(&layout)) {
        println!("{line}");
    }
    // The reroute stubs are the other store-less half: they name an atpkg, so a relocated
    // or self-updated app is exactly what repair re-lays for — this atpkg's path when it
    // stands and lasts at least as long as the one a stub names now
    // ([`crate::stub::Embedder`]).
    lay_reroute_stubs(&layout);
    // The rustup seam as every pass re-asserts it — and, this being the pass a person asked
    // for, a link to a toolchain older than the store's re-pointed at it too.
    if let Some(home) = crate::seam::rustup_home() {
        for event in crate::seam::repair(&layout, &home) {
            println!("repair: {event}");
        }
    }
    repair_store(&layout, &leftovers)
}

/// `repair`'s command-link step ([`crate::hooks::move_aside_copied_commands`]): prints what
/// it moved and why anything stayed, and returns one line per copied command still in
/// `~/.local/bin`, for [`repair_store`]'s verdict.
#[cfg(unix)]
fn repair_copied_commands() -> Vec<String> {
    let home = aterm_types::dirs::home_dir();
    repair_copied_commands_with(crate::hooks::move_aside_copied_commands(), home.as_deref())
}

/// [`repair_copied_commands`] over the move step's outcome and `$HOME`, so a test can drive
/// both arms. Prints as it goes and returns the leftovers.
#[cfg(unix)]
fn repair_copied_commands_with(
    outcome: Result<crate::hooks::AsideOutcome, &str>,
    home: Option<&std::path::Path>,
) -> Vec<String> {
    let mut leftovers = Vec::new();
    match outcome {
        Ok(outcome) => {
            for moved in &outcome.moved {
                println!(
                    "repair: moved {} aside to {} — a file, not a link to the app{}",
                    moved.from.display(),
                    moved.to.display(),
                    if moved.linked {
                        " — and linked the app in its place"
                    } else {
                        "; nothing takes its place, `aterm ctl` replaces it"
                    }
                );
            }
            for (path, why) in &outcome.failed {
                println!(
                    "repair: could not move {} aside: {why}; it is still there",
                    path.display()
                );
                leftovers.push(format!("{} is still a file", path.display()));
            }
        }
        // Not an atpkg that lays these links: name each copy, why this one does not
        // replace it, and what does.
        Err(why) => {
            // On macOS the reason names what keeps this copy from replacing it
            // (and, for a download or a volume, what to do); the remedy after it
            // says what does. Elsewhere no atpkg lays these links, so there is no
            // copy to change and the remedy alone is printed.
            let why = if cfg!(target_os = "macos") {
                format!(" ({why})")
            } else {
                String::new()
            };
            for path in home
                .map(crate::hooks::copied_command_links)
                .unwrap_or_default()
            {
                println!(
                    "repair: {} is a file, not a link to the installed aterm, and this atpkg \
                     does not replace it{why}: {}",
                    path.display(),
                    crate::hooks::copied_command_remedy(&path)
                );
                leftovers.push(format!("{} is still a file", path.display()));
            }
        }
    }
    leftovers
}

/// No command links off unix.
#[cfg(not(unix))]
fn repair_copied_commands() -> Vec<String> {
    Vec::new()
}

/// The store half of [`run_repair`] — everything after the shell hooks, the reroute stubs
/// and the rustup seam — split out so its exit code is testable against a temp prefix
/// without rewriting the rc wiring of the account running the tests.
fn repair_store(layout: &crate::store::Layout, leftovers: &[String]) -> ExitCode {
    repair_store_reconciling(layout, leftovers, reconcile_exec_roots)
}

/// The store pass with its exec-root operation supplied, so its completion verdict is
/// exercised even when an empty store's stale roots cannot be reconciled.
fn repair_store_reconciling(
    layout: &crate::store::Layout,
    leftovers: &[String],
    reconcile: impl FnOnce(&crate::store::Layout, crate::seam::Depth) -> bool,
) -> ExitCode {
    // A HELD-UPDATE RECORD THAT OUTLIVED EVERY BUILD IT MOVES OFF waits for nothing, and
    // doctor names this door for it ([`crate::quiet::Held::Outlived`]): with Automatic
    // updates off no update pass may come to end it. First, so an empty store ends it too.
    if crate::quiet::end_outlived(layout) {
        println!(
            "repair: ended the record of a held Trust toolchain update that outlived every \
             build it moves off"
        );
    }
    let installed = crate::list_installed(layout);
    let (relaid, missing, revoked, failed) = relay_shims(layout);
    if installed.is_empty() {
        println!(
            "repair: no programs installed — `aterm pkg install --default-set` installs the toolset"
        );
    } else {
        println!("repair: restored the commands of {relaid} program(s)");
    }
    for r in &revoked {
        println!("repair: ALab withdrew {r} — left disabled; run `aterm pkg update`");
    }
    // The exec roots, at `Deep`, over the shims just re-laid: every file of every root proven
    // a clone of its store file, a root rebuilt when it is not, and every trust shim rendered
    // through it. Repair is
    // the fix doctor names for a PATH tippy that cannot run, so a root it could not lay is
    // something left undone: exit 1, like a shim that could not be re-laid.
    // An empty store can still have roots left by a partially completed removal.
    // Their reconciliation contributes to the same verdict as an installed build's.
    let exec_roots_ok = reconcile(layout, crate::seam::Depth::Deep);
    if !exec_roots_ok {
        println!("repair: not done \u{2014} the exec root line above still needs attention");
    }
    if !leftovers.is_empty() {
        println!("repair: not done — {} (see above)", leftovers.join("; "));
    }
    if !missing.is_empty() {
        println!(
            "repair: {} program(s) have no build on disk and need a reinstall: {}",
            missing.len(),
            missing.join(", ")
        );
        println!("repair: `aterm pkg install --default-set` refetches them");
    }
    if !failed.is_empty() {
        println!(
            "repair: {} program(s) still have shims that could not be re-laid: {}",
            failed.len(),
            failed.join("; ")
        );
    }
    if missing.is_empty()
        && revoked.is_empty()
        && failed.is_empty()
        && exec_roots_ok
        && leftovers.is_empty()
    {
        if !installed.is_empty() {
            println!("repair: done — `aterm pkg doctor` confirms");
        }
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Step 2 of [`run_repair`], split out so it can be tested without the rc wiring step 1
/// writes: re-lay every installed program's shims. Returns how many programs were re-laid,
/// the `program@build` of each whose build is gone, of each left tombstoned, and of each
/// whose shims could not be re-laid — which must reach the exit code.
fn relay_shims(layout: &crate::store::Layout) -> (usize, Vec<String>, Vec<String>, Vec<String>) {
    let mut builds: std::collections::BTreeMap<String, std::collections::BTreeSet<u64>> =
        std::collections::BTreeMap::new();
    for (program, build) in crate::list_installed(layout) {
        builds.entry(program).or_default().insert(build);
    }
    let recorded = crate::status::read(layout)
        .map(|s| s.programs)
        .unwrap_or_default();
    let (mut relaid, mut missing, mut revoked, mut failed) =
        (0usize, Vec::new(), Vec::new(), Vec::new());
    // A linked checkout owns its shims even when an older store build remains.
    // Validate the bounded directory as well as each marker: malformed intent
    // is not permission to replace a developer's active tools with store copies.
    if let Err(error) = crate::linkmode::linked_programs_checked(layout) {
        failed.push(format!("development links could not be read: {error}"));
        return (relaid, missing, revoked, failed);
    }
    for (program, complete) in &builds {
        match crate::linkmode::linked_checkout_checked(layout, program) {
            Ok(Some(checkout)) => {
                println!(
                    "repair: {program}: dev-linked from {} — store shims left alone",
                    checkout.display()
                );
                continue;
            }
            Ok(None) => {}
            Err(error) => {
                failed.push(format!(
                    "{program}: development link could not be read ({error}); shims left alone"
                ));
                continue;
            }
        }
        let Some(&newest) = complete.last() else {
            continue;
        };
        // The build ACTIVATION selects (`store/<program>/current`), not the highest on disk:
        // `rollback` keeps the build it rolled off until gc, and re-laying that one silently
        // undid the rollback and minted a LinkShimMismatch (audit K2). With no
        // usable link — a prefix older than it, or one naming a build that is gone — the
        // newest complete build, as before.
        let build = std::fs::read_link(layout.program_current(program))
            .ok()
            .and_then(|t| crate::ops::store_build_of(&layout.prefix, &t))
            .filter(|(owner, b)| owner == program && complete.contains(b))
            .map_or(newest, |(_, b)| b);
        let build_dir = layout.build_dir(program, build);
        if !build_dir.exists() {
            missing.push(format!("{program}@{build}"));
            continue;
        }
        let exposes = repair_exposes(layout, program, &build_dir);
        // Never over a tombstone (audit K2). A revoked build keeps its tree,
        // marker and `current` links; only its shims turn into failing notices, which
        // resolve to nothing — so the fallback above read "nothing recorded" and laid a
        // live shim that made the yanked/below-floor build runnable again. Only an update
        // pass, which holds the index that revoked it, may lift that.
        let tombstoned = recorded
            .get(program)
            .is_some_and(|r| r.state.starts_with("tombstoned:"))
            || exposes.iter().any(|tool| is_tombstone_shim(layout, tool));
        if tombstoned {
            revoked.push(format!("{program}@{build}"));
            continue;
        }
        // The build's own `shim_env` (its sidecar, design S7): a re-lay exports what the
        // signed manifest declared, never less — `claude`'s DISABLE_AUTOUPDATER=1 above
        // all, whose loss turns the vendor's updater back on under a managed name.
        match crate::activate::install_shims_env(
            layout,
            &build_dir,
            &exposes,
            crate::activate::Aliases::laid_for(layout, program),
            &crate::shim_env::read_sidecar(&build_dir),
        ) {
            Ok(refused) => {
                relaid = relaid.saturating_add(1);
                for name in refused {
                    println!("repair: {program}: refused shim {name:?} (not an allowed tool name)");
                }
            }
            Err(e) => {
                println!("repair: {program}: could not re-lay shims: {e}");
                failed.push(format!("{program}@{build} — {e}"));
            }
        }
    }
    (relaid, missing, revoked, failed)
}

/// What [`relay_shims`] re-lays for `program` from `build_dir`: the names its own shims
/// still carry (they resolve even when their TARGET is gone, which is the audited shape),
/// falling back to the program's own name — the overwhelmingly common single-tool case —
/// when none survive.
///
/// PLUS THE PROGRAM'S OWN NAME WHEN ONLY ITS SHIM IS GONE: the build ships
/// `bin/<program>` and `bin/<program>` in the managed `bin/` is absent. The survivors are
/// not a record of the signed exposes — nothing local is — so reading them alone would
/// make a lost shim permanent (the end state a trust rollback leaves once the next trust
/// flip prunes what it re-pointed, see `flow::rollback_may_claim`). An absent shim is the
/// only case taken: a
/// present one — a dev link, a tombstone, another program's shim — is never overwritten.
/// What it does not know: a future package that bundles a same-named binary WITHOUT
/// exposing it would get a shim from repair; recording the signed exposes beside the build
/// at stage time is the principled replacement.
fn repair_exposes(
    layout: &crate::store::Layout,
    program: &str,
    build_dir: &std::path::Path,
) -> Vec<String> {
    let Some(mut exposes) = crate::installed_exposes(layout, program) else {
        return vec![program.to_string()];
    };
    let own_binary = build_dir
        .join("bin")
        .join(format!("{program}{}", std::env::consts::EXE_SUFFIX))
        .is_file();
    let own_shim_absent = crate::store::ToolName::new(program).is_some_and(|tool| {
        std::fs::symlink_metadata(layout.shim(&tool))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    });
    if own_binary && own_shim_absent && !exposes.iter().any(|name| name == program) {
        exposes.push(program.to_string());
    }
    exposes
}

/// Whether this machine has DISABLED `program`: its recorded row says `tombstoned:` or
/// `aborted:` (the update lane's rows for a build it revoked, or a group it aborted at),
/// or one of the tools the program exposes is a tombstone shim on disk. The same two
/// measurements `repair` makes before it would re-lay a shim (audit K2) — read, never
/// inferred from the pin — so a lane that repaints rows for members absent on this
/// target leaves a fault row where it found one.
fn program_disabled_here(
    layout: &crate::store::Layout,
    recorded: Option<&crate::Status>,
    program: &str,
) -> bool {
    // A tombstoned build is no longer counted as installed, so its exposes are not
    // readable from the store; the program's own name is the tool that matters then
    // (the same fallback `repair` uses).
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
/// and saying only that is a dead end; the order of acts matters (install, THEN verify).
const EMPTY_VERIFY: &str = "atpkg: nothing installed to verify (aterm pkg install \
                            --default-set installs the toolset)";

/// The STABLE script form: one row per installed BUILD, `program\tbuild\tnotes`,
/// headerless, ascending builds (superseded rows before the live one) — byte-identical
/// to what `atpkg list` always printed, because pipes elsewhere key on it.
/// A development link with no store build gets `-` in the build column;
/// it must not disappear or acquire an invented registry build number.
///
/// WHAT AM I ACTUALLY RUNNING. The store legitimately holds MORE THAN ONE build per
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

/// The human report: a count summary FIRST (verdict before detail — the grid rule),
/// then one aligned row per PROGRAM in its live state, with superseded builds folded
/// into a trailing "(N older build(s) kept for rollback)" — the fact survives, the
/// twenty-row scan does not. No health adjective anywhere: counts are what the shims
/// prove; "healthy" is doctor's word, earned by running checks.
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
    // The summary, zero-count categories omitted — "10 program(s) — 10 live" is the
    // whole story on a healthy machine, and every extra clause would dilute the one
    // that matters on an unhealthy one.
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
///
/// THE INTENT IS WRITTEN BEFORE THE DELETE. `ops::uninstall` takes the shims off first and
/// then spends seconds in `remove_dir_all` (the 3.4 GB trust tree): a kill inside the
/// delete must not leave the program absent with no `removed` entry for the next pass to
/// pull straight back. A half-deleted program is
/// still one the user asked to remove; the name gates are the only failure that deletes
/// nothing, so they run first and the marker is never minted for a refused name.
fn uninstall_and_retire(layout: &crate::store::Layout, program: &str) -> std::io::Result<()> {
    retirement_name_gate(program)?;
    record_removed(layout, program);
    // A removal on purpose is not a retirement in favour of a system copy: forget the
    // date so a later `system:` row never reports a retirement this act superseded.
    layout.clear_retired(program);
    // The rustup seam goes WITH the toolchain it presents (audit): the view
    // is a clone of the live trust build, so `uninstall trust` would otherwise leave
    // `~/.rustup/toolchains/trust` running a ghost of the deleted compiler. The single
    // program now does too — before the build trees go, as the whole-set path orders it.
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
///
/// ROW DELETION IS A NEW CAPABILITY, so it gets the name gate the old code never needed.
/// `ops::uninstall`'s shape rule rejects only empty/`.`/`..`/separators/NUL, so without
/// this `atpkg uninstall '*toolset*'` returned Ok and DELETED a bookkeeping row that has
/// its own owner and its own reaper, and `atpkg uninstall --help` appended a flag to the
/// removed-markers file. Neither is a program; neither may be retired through here. The
/// shape rule is asked here too, because the removal intent is recorded before
/// `ops::uninstall` would get to refuse the name.
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
    // REFUSE TO "UNINSTALL" WHAT WAS NEVER THERE: a success here would mint a durable
    // removed-marker that suppresses set-completion for a program the user never had.
    // Only program-shaped names take this path: a sentinel/flag name still
    // reaches the retirement gate's own refusal, which owns those words.
    if !(is_sentinel_row(program) || program.starts_with('-')) && !program_present(layout, program)
    {
        eprintln!("atpkg: {program} is not installed — nothing to uninstall");
        return ExitCode::from(1);
    }
    // Removing a managed program is an EXPLICIT act, so this machine stops being one
    // that keeps the whole set complete — otherwise the next unattended pass would
    // reinstall what the user just removed, which is the manager fighting its owner.
    // Adoption is re-established by the deliberate whole-set act (Settings ▸ Packages ▸
    // Install ALab toolset), and the way to drop ONE program while staying adopted is
    // `[packages].exclude`. Like the removed marker, this is INTENT, so it lands before
    // the multi-second delete a kill can interrupt — but only for a name the retirement
    // gate accepts, which is the one failure that deletes nothing.
    let opted_out = retirement_name_gate(program).is_ok() && adopted(layout);
    if opted_out {
        clear_adoption(layout);
    }
    match uninstall_and_retire(layout, program) {
        Ok(()) => {
            // A removed program's pending stub goes with it: `removed` suppresses the
            // reinstall, so a stub promising "installing" would be a standing lie.
            crate::stub::remove_stub(layout, program);
            // The durable fact and the one remedy: the removed marker, which every pass
            // honours until the program is put back (adoption lasts only until the next
            // app launch re-adopts, `should_complete_set`).
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

/// The parenthetical for a status row that names a copy of the program this manager does
/// NOT own: a binary the user already had (`system: <path>`). `None` for every other row
/// — those name nothing that is installed on this machine.
///
/// [`cmd_uninstall_all`] uses it to say what it LEFT ALONE, which is the half of §S11 the
/// code never had: *"system copies are never touched **and the output says so**"*
/// (`docs/DESIGN-which-copy-runs-2026-08-27.md`).
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

/// The programs [`cmd_uninstall_all`] sweeps.
///
/// THE SWEEP IS THE UNION OF WHAT IS LIVE AND WHAT THE RECORD STILL CLAIMS. A TOMBSTONED
/// program is invisible to `active_builds` — its shims were replaced by failing stubs —
/// so walking the live set alone stepped straight past it, leaving its build tree on disk
/// and its row in the record after an explicit whole-set removal. That is the exact class
/// this verb exists to clear. Sentinel bookkeeping rows and stray flag rows are excluded:
/// neither is a program, and neither is this verb's to retire.
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

/// Remove every target this manager actually installed, and report the rest.
///
/// NOTHING IS "REMOVED" THAT WAS NEVER OURS. The target list above is the union of the
/// live builds and every status row, and a row is written for plenty of names atpkg never
/// installed: `system: /opt/homebrew/bin/gh — not managed by aterm` for a foreign copy
/// that satisfies a member, `unavailable on <target>`, `agent program — installing`. For
/// all of those `ops::uninstall` finds no store tree under the prefix, deletes nothing,
/// and returns `Ok(())` — which this loop read as a removal. So `aterm pkg uninstall
/// --all` printed `atpkg: removed ay, gh, trust` on a machine where `gh` was untouched
/// and still ran, exited 0, and
/// wiped the very rows that recorded where those copies live (the next pass then had to
/// re-discover them). Presence on disk — [`program_present`], the same two places
/// `ops::uninstall` cleans — is the only thing that makes a name this verb's to claim.
fn sweep_uninstall_targets(
    layout: &crate::store::Layout,
    targets: &std::collections::BTreeSet<String>,
) -> UninstallSweep {
    // The rows as they stood BEFORE the sweep: a row is what says whether this manager
    // ever owned a copy, and `clear_status_row` rewrites the record as we go.
    let rows = crate::status::read(layout)
        .map(|s| s.programs)
        .unwrap_or_default();
    let mut sweep = UninstallSweep::default();
    for program in targets {
        let state = rows.get(program).map(|r| r.state.as_str()).unwrap_or("");
        if !program_present(layout, program) {
            match foreign_copy_note(state) {
                // A copy that is STILL THERE and was never ours. Name it, and KEEP its
                // row: the row is the true answer about this machine (`which` and the
                // next pass both read it), not a claim about a store tree this verb
                // emptied.
                Some(note) => sweep.left_alone.push(format!("{program} {note}")),
                // A row with nothing behind it — a member with no build for this
                // target, a managed row that outlived its program.
                // It still goes, because a record that outlives its program is a claim
                // about a machine state that no longer exists; but it was not a removal,
                // so it is not reported as one.
                None => clear_status_row(layout, program),
            }
            continue;
        }
        match crate::uninstall(layout, program) {
            Ok(()) => {
                // Same retirement as the single-program verb: a row that outlives its
                // program is a claim about a machine state that no longer exists.
                //
                // Only on success — but note what an `Err` here actually means. `uninstall`
                // removes the shims FIRST and unconditionally; its only fallible step is the
                // final `remove_dir_all`. So a failure leaves the program already OFF PATH
                // with its build tree behind, and keeping the row is the conservative choice
                // precisely because the machine is now in a half-state worth reporting.
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
///
/// Silence here is what let those names ride in the `removed` list instead: a user whose
/// `gh` comes from Homebrew was told aterm had removed it.
fn print_left_alone(left_alone: &[String]) {
    if left_alone.is_empty() {
        return;
    }
    println!(
        "atpkg: left alone — not installed by aterm, so nothing was removed: {}",
        left_alone.join(", ")
    );
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
    let targets = uninstall_all_targets(&layout);
    // The seam goes with the set, and goes FIRST: every recorded rustup entry is
    // detached while it still resolves into the prefix. Before the branch, so a record
    // left on an already-empty store is dropped too.
    detach_rustup_seams(&layout);
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
        layout.clear_all_retired();
        // A held toolchain update recorded over a store that is empty now waits for
        // nothing, and a declined machine runs no pass that would end it.
        let _ = crate::quiet::end_outlived(&layout);
        // The decline extends to the pending stubs: a declined machine keeps NO
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
    crate::hooks::refresh(&layout);
    // Last, and only after the removals: this machine no longer runs the set, so
    // set-completion must not reinstate it on the next unattended pass — and the
    // SEED lane must not either. The seed installs whatever the store lacks (that
    // is what makes an interrupted first run resumable), so without a durable
    // decline the next launch would cheerfully restore all 3.2 GB the user just
    // removed.
    clear_adoption(&layout);
    record_decline(&layout);
    layout.clear_all_retired();
    // The held toolchain update's record goes with the builds it would have moved off,
    // as on the empty-store branch.
    let _ = crate::quiet::end_outlived(&layout);
    // Same stub discipline as the empty-store branch: `uninstall --all` removes
    // every pending stub with the toolset it declines.
    crate::stub::remove_all_stubs(&layout);
    crate::reroute::remove_all(&layout);
    remove_exec_roots(&layout);
    if removed.is_empty() {
        // NOTHING OF OURS WAS HERE — a machine whose every row names a system copy. The
        // decline recorded just above is the act that was asked for
        // and it landed, so this is a success with an honest report, not a failure: the
        // old code reached this exit only when every removal had ERRORED, because a
        // no-op `uninstall` counted as one (`tools/install.sh` reads the exit code and
        // would otherwise tell the user to run the sweep by hand).
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

/// `uninstall --all`'s exec-root half, on both of its branches: remove `<prefix>/compat`
/// whole ([`crate::compat::remove_all`]). The per-program uninstall already took each
/// program's roots with its tree; this takes what an empty store can still carry — a root a
/// failed removal or an older client left — because a declined machine keeps no clone of
/// builds it no longer has. A failure is one stderr line: the removed
/// programs' shims are already gone, and `aterm pkg gc` sweeps any trust root left in it
/// whose build is gone.
fn remove_exec_roots(layout: &crate::store::Layout) {
    if let Err(e) = crate::compat::remove_all(layout) {
        eprintln!(
            "atpkg: warn — {} was not removed ({e}); `aterm pkg gc` sweeps any trust exec \
             root left in it",
            crate::compat::compat_dir(layout).display()
        );
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
///
/// The build kept below each live one is the one `aterm pkg rollback` would land on under the
/// channel this machine last verified ([`last_verified_channel`]: the cache, offline).
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
    // A build KEPT for a run's lease is disk the owner may wonder about: whose it is, and
    // that it goes on its own once the run ends ([`crate::lease`]).
    for (p, build, holders) in &report.leased {
        lines.push(format!(
            "atpkg {verb}: kept {} \u{2014} {}; the first pass after that run reclaims it",
            gc_builds_words(p, &[*build]),
            holders.clause()
        ));
    }
    // An archive KEPT for a held toolchain update is gigabytes with one use left: say
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

/// A stage-scratch directory as the sweep line names it: `2.1.281`, `build 18` — the
/// build as [`crate::vendor_direct::build_words`] says it, never a vendor store id; a name
/// of any other shape verbatim.
fn scratch_words(name: &str) -> String {
    crate::store::stage_scratch_of(name).map_or_else(
        || name.to_string(),
        |(build, _)| crate::vendor_direct::build_words(build),
    )
}

/// `program`'s store builds as the gc lines name them: `claude 2.1.279, 2.1.278` when every
/// one is a vendor-direct build (versions, never store ids), else `trust build(s) 8595,
/// 8594` — a legacy build among vendor ones keeps its number.
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
/// value the publish pipeline (§4.2/§12) writes into a per-build manifest's `tree_root`.
/// A producer helper exposing the same hashing the client re-verifies with (§8).
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

/// The target triple of this build, for artifact selection (§4.2).
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

/// RFC3339 UTC timestamp for the status record, now ([`rfc3339_at`]).
fn now_rfc3339() -> String {
    rfc3339_at(now_unix())
}

/// RFC3339 UTC timestamp of the epoch second `secs`. Pure Rust — NO `/bin/date` shell-out,
/// which could never spawn on Windows (no such binary; the absolute path also bypasses
/// PATHEXT), so `updated_at` was left permanently empty there, silently disabling doctor's
/// index-freshness / "publishing looks frozen" warning. Formats via the shared
/// `aterm_types::rfc3339` civil-calendar math, the exact inverse of `flow::rfc3339_to_unix`,
/// so the two round-trip. Empty string on a pre-epoch (or exactly-epoch) clock.
fn rfc3339_at(secs: i64) -> String {
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

/// A caller's ALREADY-VERIFIED index, offered to a second lane of the SAME pass — but
/// only while its signed freshness window is still open at `now` (§8 gate 2).
///
/// `atpkg update` runs several lanes over one store: the channel apply, the attestation
/// repair below, the set completion, the empty-store diagnosis. Each resolved the signed
/// index for itself, and one resolve re-reads and TOML-parses `index-cache.toml`, re-runs
/// the master-signature verify + roster admit + machine-signature verify over every
/// candidate, and re-observes the roster generation — two to four times per six-hourly
/// tick, for an answer that cannot differ inside one pass. Threading the pass's first
/// index through the rest is the shape [`crate::flow::bootstrap_group`] and
/// [`crate::flow::apply_channel_gated`] already have.
///
/// What it must never become is a way around a gate. Everything a resolve proves — the
/// anti-rollback floor, the roster admission, the machine signature over the index — is a
/// property of BYTES this pass has already checked, so it still holds. Freshness is the
/// one property that is a function of the CLOCK rather than the bytes, so it is re-checked
/// here: an index whose window lapsed during a long pass is refused re-use, that lane
/// resolves for itself, and it meets the identical `Stale` refusal it always did.
fn reusable_index(index: Option<&crate::TrustedIndex>, now: i64) -> Option<&crate::TrustedIndex> {
    index.filter(|index| crate::flow::index_is_fresh(index, now))
}

/// Re-record the signed `tree_root` for any program that is LIVE but unattested.
///
/// See the call site for the window that produces one. Silent and best-effort in
/// both directions: a program whose root cannot be re-derived is left exactly as it
/// was — fail-closed — and nothing here can fail an update.
///
/// `trusted` is the caller's own verified index when its pass already resolved one (the
/// update lane's), re-used through [`reusable_index`]; `None` (the rollback verb, which
/// holds no index) resolves one here.
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
    // A vendor-direct build is attested by its `.vendor` record — and a vendor program
    // rolled back to its legacy build by the root its stamp kept ([`legacy_root_of`]) —
    // never by an index manifest: asking the index for one is a metered miss every pass.
    let unattested: Vec<(String, u64)> = live
        .iter()
        .filter(|(program, _)| !crate::vendor_direct::is_vendor(program))
        // …and one whose recorded root is for ANOTHER build: a build that went live without
        // its row (a roll-forward, a row a killed pass never wrote) has no attestation yet.
        .filter(|(program, build)| {
            recorded
                .get(program.as_str())
                .is_none_or(|row| row.tree_root.is_empty() || row.installed_build != Some(**build))
        })
        .map(|(program, build)| (program.clone(), *build))
        .collect();
    // Cheap pre-filter for the SECOND job below: is there any row that could turn out to
    // be stale? Deciding this before the index resolve keeps the common case — a healthy
    // store with nothing to do — exactly as free as it was.
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
    // RECONCILE THE MEMO AGAINST ITS AUTHORITY. A row naming something the verified index
    // does not name and the store does not carry is a stray, and doctor would report a
    // healthy toolset as incomplete for good. Announced rather than silent: a manager
    // never quietly edits its own record.
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
        // A prior managed row for this very build keeps its words — `rolled back from
        // <N>` above all — so re-deriving a root never re-mints a pin the index does not
        // hold (audit).
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
    if let Some(state) = canonical_non_error_state(e) {
        return Some(state);
    }
    match e {
        // A dev-linked program is a benign HARD-SKIP (§13), not an error state.
        crate::FlowError::Linked(_) => Some(String::from("dev-linked (skipped)")),
        // Verdicts about the REQUEST or the NETWORK — never about this program.
        crate::FlowError::NotReachable(..)
        | crate::FlowError::NoIndex
        | crate::FlowError::Unreachable(_)
        | crate::FlowError::Stale => None,
        // The vendor lane's own stamp keeps its verdict; the program's row stays as it is.
        crate::FlowError::Vendor(_) | crate::FlowError::VendorUnreachable(_) => None,
        _ => Some(format!("error: {e}")),
    }
}

/// The CANONICAL, non-error state some install refusals really are (design §2): a
/// target the pinned build carries no row for is `unavailable on <target>: <hint>`.
/// `None` for every genuine fault.
///
/// The hint here is the generic one; the pass's missing-triple prescan, which holds the
/// verified index, records the program's own `unavailable_hint` ahead of this.
fn canonical_non_error_state(e: &crate::FlowError) -> Option<String> {
    match e {
        crate::FlowError::NoArtifact(t) => Some(crate::state::unavailable(t, "")),
        // …and the vendor lane's, with its vendor's own hint.
        crate::FlowError::VendorUnavailable(state) => Some(state.clone()),
        // A member the digest-refusal memo HELD off a stage that would fail again: the
        // flow already rendered the canonical `held: last attempt failed with …; <retry>`
        // row ([`crate::state::held_failed`]), and an `error:` head on it would say this
        // pass staged something — it staged nothing, and the row names the retry.
        crate::FlowError::StageRefused(row) => Some(row.clone()),
        _ => None,
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
        .filter(|k| !is_sentinel_row(k))
        .filter(|k| !live.contains_key(k.as_str()))
        .filter(|k| !known(k))
        .cloned()
        .collect()
}

/// The seed for a writer that REBUILDS `status.toml` — `None` means the record on disk
/// EXISTS and could not be read (and, when corrupt, could not be kept aside), i.e. do not
/// write.
///
/// A corrupt record (a symlinked path, a hand edit that broke the TOML, a file a power
/// loss truncated) is healed by [`crate::status::seed_for_rewrite`]: kept as
/// `status.toml.corrupt-<unix>` and rebuilt from the store. A read that failed for
/// another reason (a
/// permission, an I/O error) leaves a possibly healthy record alone, never overwritten
/// blind. The line is printed at most once per process because a pass calls these
/// writers once per program.
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

/// Replace the aggregate `status.outcome` sentence without touching per-program rows.
///
/// The counterpart to [`clear_status_row`] for the line Settings ▸ Packages actually
/// shows: retiring a failure row while leaving its sentence behind just makes the
/// staleness quieter.
fn record_outcome(layout: &crate::store::Layout, outcome: String) {
    // CREATE the record if it does not exist yet: environmental failures route HERE and
    // nowhere else, so a first-run offline `atpkg install` must leave this witness for
    // the doctor `tools/install.sh` hands a failed seed to. Fields are stamped exactly as
    // `record_status` stamps them: `enabled` and `index_source` are CURRENT facts, so recomputing beats carrying a stale pair.
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
            // The TIMESTAMP moves with the sentence: Settings and `atpkg doctor` show
            // the two side by side.
            updated_at: now_rfc3339(),
            ..existing
        },
    );
}

/// Remove one bookkeeping row from `status.toml` — the counterpart to
/// [`record_status`] for a failure a later pass has disproved.
///
/// Rows like `*index*` and `*toolset*` are otherwise write-only, so a transient
/// failure would read as a standing one. Best-effort and silent, exactly like the
/// writer: observability must never fail an update.
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
/// one when non-empty, otherwise the ALREADY-RECORDED root — but only when the recorded row
/// is for that same build. An `already_current` install, or an untouched coherence-group
/// sibling, reports an empty tree_root (nothing was flipped) — persisting that empty value
/// would erase a perfectly good attestation and break `atpkg verify` for an untampered
/// program. A root recorded for ANOTHER build is no attestation of this one: a build that
/// went live without its row (a roll-forward, a row a killed pass never wrote) kept the old
/// build's root under the new build, and `verify` reported drift on untouched bytes. Empty
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
/// that build, else the one its stamp kept ([`keep_replaced_legacy_root`], or its signed
/// manifest's, read with its version) — never an index asked, so no metered fallback.
fn legacy_root_of(layout: &crate::store::Layout, program: &str, build: u64) -> String {
    let recorded = recorded_root_of(layout, program, build);
    if !recorded.is_empty() {
        return recorded;
    }
    crate::vendor_direct::ProgramStamp::read(layout, program)
        .and_then(|s| s.legacy_root_of(build).map(str::to_string))
        .unwrap_or_default()
}

/// A vendor program's row about to be replaced may be the last record of its legacy index
/// build's signed root (the first vendor install replaces it, and gc keeps that build for a
/// rollback): keep the root in the program's stamp, once.
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

// PER THREAD, like the resolve's provenance ([`crate::flow::last_resolve`]) — a pass is one
// thread: how many outcome sentences it has written. Every writer of a NEW `outcome` counts
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

// PER THREAD, like OUTCOMES_WRITTEN: how many successes this thread has stamped
// ([`record_pass_end`]) — whether a full pass stamped one is its recorded outcome.
thread_local! {
    static SUCCESSES_STAMPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// A signed index resolve was actually called during this full pass. An
    /// empty-store vendor-only pass cannot promote the window's hint into a
    /// durable attempted-index witness without attempting that resolve.
    static PASS_INDEX_RESOLVE_ATTEMPTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The highest index this pass actually resolved. Reset at each full pass
    /// start; the window's requested build is folded in only at pass end.
    static RESOLVED_PASS_INDEX_BUILD: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many successes this thread has stamped so far.
fn successes_stamped() -> u64 {
    SUCCESSES_STAMPED.with(std::cell::Cell::get)
}

/// Record the install outcome to `status.toml` (the silent manager's observability
/// surface, §5/§9). Best-effort — diagnostics, never load-bearing. The sentence is this
/// pass's verdict ([`note_outcome_written`]).
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

/// THE SUCCESS STAMP'S ONE WRITER — the writer the derived model `AtpkgPassStamps` states,
/// whose readers are `aterm_update_core::pkg_check`'s: a pass that REACHED the signed index
/// (`reached` — it verified an index the channel served this pass, not the §14 cache's:
/// [`crate::flow::live_verifications`] moved) stamps `last_success_at`, counted in
/// [`successes_stamped`], which is how the pass's recorded end reads it
/// ([`run_recorded_update_pass`]: `ok`, else `failed` or `offline` by its exit). Called once,
/// at the end, by `update` ([`finish_update_pass`]) and by `install --default-set` — never
/// by a per-program writer, the seed offer or a shadow reconcile, which move `updated_at`
/// alone. A pass on the cached index stamps no success (audit PK-3). `now` is the pass's
/// own clock (`PassSeams::now`).
fn record_pass_end(layout: &crate::store::Layout, now: i64, reached: bool) {
    if !reached {
        return;
    }
    SUCCESSES_STAMPED.with(|n| n.set(n.get().wrapping_add(1)));
    let _ = crate::status::stamp_success(layout, &rfc3339_at(now));
}

/// A full `update` pass's end: its one stamp ([`record_pass_end`]) and the exit code that
/// agrees with it. A pass that reached the index exits `code` — 0 clean, 1 when a member
/// failed (recorded in its own row), 2 when nothing is installable here. One that did not
/// exits non-zero whatever its members did: [`aterm_update_core::pkg_check::PASS_OFFLINE_EXIT`]
/// when nothing answered (`offline`), else 1 — so the window never reads a pass on the §14
/// cache as a check that ran (the special case it kept for exit 0 on the cache,
/// `update_pass_served_from_cache`, is gone with the stamp that needed it).
///
/// The one ending that fails ONLY for its index — not reached, not offline, and `code` 0
/// (no member failed, so nothing went to stderr) — is noted in
/// [`PASS_FAILED_ONLY_FOR_ITS_INDEX`], which [`run_recorded_update_pass`] records with the
/// pass's end ([`crate::status::Status::stale_index_only_pass_seq`]).
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
    /// for its index" arm: the index came from the cache, some host answered, and no member
    /// failed — exit 1 with nothing on stderr. Reset and read by
    /// [`run_recorded_update_pass`], which records it with the pass's end. The doc sits
    /// inside the macro: on the invocation it would be `unused_doc_comments`.
    static PASS_FAILED_ONLY_FOR_ITS_INDEX: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Stamp the index build this pass resolved (the publisher's pulse `doctor` reads), and —
/// when its resolve did NOT reach the channel — say so in the aggregate outcome, so a pass
/// served from the §14 cache never records a bare green "up to date" (audit).
/// `index_build` is the build the pass resolved, when it has one in hand.
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
/// setting — [`crate::resolve_account`]), the `[packages.links]` per-program
/// `owner/repo` fetch overrides ([`crate::repo_overrides`]), and the store's verified
/// index floor, which its discovery walk counts from. It holds no credential: every byte
/// comes from the release download host (R3), so a repointed or overridden destination
/// must be publicly readable.
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

/// `ATPKG_REGISTRY` — a DEVELOPMENT SEAM (`aterm_types::dev_seam!`): swaps the whole fetch
/// source for an on-disk registry, for the atpkg integration tests and the publisher's
/// self-tests. A shipped binary does not read it; its one source is the signed index.
pub(crate) const REGISTRY_SEAM: &str = "ATPKG_REGISTRY";

/// The [`REGISTRY_SEAM`] value this process runs under — `None` in every shipped binary.
pub(crate) fn registry_seam() -> Option<std::ffi::OsString> {
    aterm_types::dev_seam!(REGISTRY_SEAM)
}

/// An index program's no-op verdict, naming the index it is current WITH, so it can be
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
/// seam, [`REGISTRY_SEAM`]) selects the offline / publisher-test [`crate::DirFetcher`]
/// (§14); otherwise the production GitHub fetcher. A `dir:` registry's bytes STILL pass
/// the identical verify + floor + freshness gates — the source is not an authenticity
/// input. (A `dir:` registry serves everything locally, so `[packages.links]` fetch
/// overrides are moot there by construction.)
fn resolve_fetcher(layout: &crate::store::Layout) -> Box<dyn crate::flow::Fetcher> {
    if let Some(dir) = dir_registry(registry_seam().as_deref()) {
        return Box::new(crate::DirFetcher::new(dir));
    }
    Box::new(github_fetcher(layout))
}

/// The trust anchor the network verbs verify under: the committed paper-master keyset
/// ([`crate::PKG_TRUST_ANCHORS`]) plus this store's durable `roster_seq` ratchet, and
/// nothing else.
///
/// No environment variable can swap the anchor: ambient process state never decides what
/// the package manager trusts. Trusting a mirror or a second owner account is a committed
/// change to `aterm_update_core::pins`, visible in a diff like any other trust decision.
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
/// verify under ([`crate::manager_enabled`]).
fn manager_enabled() -> bool {
    crate::manager_enabled()
}

/// Whether this machine has ADOPTED the ALab toolset — see [`crate::store::Layout::adopted`].
/// The install consent is one key (`[packages].auto_install`, default TRUE), so adoption
/// does not gate set completion ([`should_complete_set`]); it
/// records WHO adopted, which is what an announcement's consent clause states
/// ([`set_completion_consent`]).
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

/// Create the `0600` marker at `path` with `body` (documentation for whoever finds the
/// file; the marker's EXISTENCE is the fact). `Err` only when the file cannot be created —
/// a short write still leaves the marker standing.
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

/// Whether an explicit `install --default-set` pass RECORDS adoption: it installed at
/// least one member (`activated > 0`), or it found the set already complete — there were
/// builds before it ran, it failed nothing, skipped nothing and owes nothing. The
/// already-complete arm matters: `uninstall` CLEARS adoption and points at this very
/// verb, and with the set complete again a pass that recorded nothing would leave
/// unattended set-completion off for good. Pure for the table test.
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

/// Forget adoption. Called by `uninstall`: removing a managed program is an explicit act,
/// and set-completion must never undo it on the next unattended pass. The record of who
/// adopted goes with it ([`clear_seed_adoption`]), so no record outlives the marker it
/// describes.
fn clear_adoption(layout: &crate::store::Layout) {
    remove_marker(&layout.adopted());
    clear_seed_adoption(layout);
}

/// Whether the seed pass adopted this machine ([`crate::store::Layout::adopted_by_seed`]):
/// `cmd_seed` created the [`adopted`] marker itself, so installing aterm is the consent behind
/// the set, and `[packages].auto_install = false`, read before it adopts, is the switch that
/// would have stopped it. Both files must exist: a record without the marker vouches for
/// nothing, and a marker without the record claims no more than the uninstall
/// ([`set_completion_consent`]).
fn adopted_by_seed(layout: &crate::store::Layout) -> bool {
    adopted(layout) && layout.adopted_by_seed().is_file()
}

/// `cmd_seed`'s adoption: [`record_adoption`] plus the record of who adopted, written only
/// when this call creates the marker. The seed pass runs on every launch, so if it stamped a
/// marker it found, a machine the explicit verb adopted would become the first launch's on its
/// very next launch and its update passes would again say "installing aterm is the consent".
/// Best-effort like the marker: a missing record claims less, never more.
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

/// Forget that the seed pass adopted. This is the explicit toolset verb's call: running it
/// is the consent, and it never reads `auto_install`. From then on "installing aterm is the
/// consent" no longer describes the passes that keep the set complete.
fn clear_seed_adoption(layout: &crate::store::Layout) {
    remove_marker(&layout.adopted_by_seed());
}

/// The programs this user removed individually ([`crate::store::Layout::removed`]).
fn removed_programs(layout: &crate::store::Layout) -> std::collections::BTreeSet<String> {
    // ONE reader, on the layout: the unattended update lanes need the same answer,
    // and a second copy here is how they came to disagree.
    layout.removed_programs()
}

/// The programs THIS machine keeps complete: every program the signed index names,
/// narrowed by the user's `[packages].exclude`. ONE reader for every lane that plans a set
/// (the network pass, `doctor`, and [`seed_serviceable`]'s prescan of what the index can
/// still serve here).
pub(crate) fn wanted_programs(
    index: &crate::manifest::Index,
    cfg: &crate::config::PackagesConfig,
) -> std::collections::BTreeSet<String> {
    index.installable(cfg.exclude())
}

/// Reconcile SYSTEM SATISFACTION over `wanted` against the given `PATH`. Returns the
/// members satisfied this pass (each also recorded + announced by [`satisfy_by_system`],
/// and dropped from `installed` when a managed copy was retired). The caller removes the
/// returned names from its wanted set.
///
/// The `PATH` is INJECTED, never read here. A pass reads `PATH` exactly once, at its own
/// entry
/// ([`install_default_set_with_path`]), and threads the one value to both consumers, so the
/// answer to "what was on `PATH` during this pass" cannot differ between the
/// system-satisfaction question and the shadow question.
fn reconcile_system_satisfied_with(
    layout: &crate::store::Layout,
    index: &crate::manifest::Index,
    wanted: &std::collections::BTreeSet<String>,
    installed: &mut std::collections::BTreeMap<String, u64>,
    path_var: Option<&std::ffi::OsStr>,
) -> Vec<String> {
    let mut satisfied = Vec::new();
    // A vendor-direct program's `system` key is an index rule, and none decides one
    // (design §1.4): retiring its managed copy here would undo its own lane every pass.
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

/// What the last VERIFIED index pins for this machine — `(index_build, program → build)` on
/// the one channel ([`crate::config::CHANNEL`]) for this target — from the §14 cache:
/// offline, no network, nothing written. Settings ▸ Packages' "latest known" for an ALab program (Phase 4), read on the
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

/// The last index this machine VERIFIED, from the §14 cache — offline, no network, no
/// floor or ratchet written. `None` when nothing is cached (or nothing cached verifies):
/// the advisory surfaces that read it (`which`) then say nothing.
pub(crate) fn cached_index(layout: &crate::store::Layout) -> Option<crate::TrustedIndex> {
    cached_index_for(layout, crate::config::cached())
}

/// The channel `aterm pkg rollback` reads ([`crate::config::CHANNEL`]), as the last index
/// this machine verified ([`cached_index`]) publishes it: the revocations that pick the build
/// gc keeps below each live one ([`crate::gc::run`]) at a door that holds no index of its
/// own. An install verb's resolve has just refreshed that cache; `None` when nothing cached
/// verifies.
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

/// `program` is satisfied by the system install at `path`: say so in the canonical
/// words, record the `system: <path> — not managed by aterm` row, and — when atpkg
/// carries its own copy — retire that copy (shims, channel links, store tree, through the
/// fail-closed `ops::uninstall`) so the system binary is the only `<program>` the user
/// can reach, and remember the DAY it happened (`<prefix>/retired/<program>`) so the row,
/// `doctor` and `which` can say `(managed copy retired <date>)`. NOT a removal on
/// purpose: no `removed` marker is written, so the member installs again the moment the
/// system binary goes away. `installed` loses the program when a copy was retired, so the
/// caller's plan reads the store as it now is.
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
    // The pass log prints the SAME words the row records (design §2), plus the event.
    if event.contains("could not") {
        eprintln!("atpkg: {program}: {state}{event}");
    } else {
        println!("atpkg: {program}: {state}{event}");
    }
    // The ROW carries the fact; the aggregate sentence stays the pass's own verdict
    // (Settings ▸ Packages prints it as its detail line, and "up to date" must not be
    // displaced by a per-member note every six hours). Only a record with no sentence
    // yet takes this one.
    let outcome = crate::status::read(layout)
        .map(|s| s.outcome)
        .filter(|o| !o.is_empty())
        .unwrap_or_else(|| format!("{program} satisfied by system {}", path.display()));
    record_unbuilt(layout, program, state, outcome);
}

/// Record `state` as `program`'s row — no installed build, no root — and print it in the
/// pass log, UNLESS the record already says exactly that: a pass that changes nothing
/// writes nothing and says nothing. The aggregate outcome is carried over untouched (the
/// row is a per-program fact, not the pass's verdict). Returns whether a row was written.
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
/// index <index_build>`: a row that is absent, the legacy `active` spelling (no current
/// writer emits it — `state.rs` lists it among the "other" strings), or a managed row
/// whose build or index differs from this pass's — or a FAULT this very pass has
/// disproved.
///
/// THE FAULT LIFT. `aborted: <phase>` (the `Aborted` arm of
/// [`report_channel_apply`]) and `error: …` ([`failed_install_state`]) are written by a
/// pass that FAILED, and nothing was ever charged with taking them back: every other
/// writer rewrites a row only when bytes move. So once the program settles at its pin
/// with no bytes left to move — the bad build yanked and the pin rolled back, a sibling
/// upgraded through the explicit door (which rewrites only its own row), or a group-abort
/// charged to an arbitrary member that itself needed no move ([`crate::doctor`]) — the
/// row outlives the failure it describes, and it is still a fault to
/// `doctor::is_problem_state`, and only an explicit `aterm pkg install <program>` would
/// clear it — not a thing an unattended manager may require. Reaching this predicate IS the
/// disproof: the group came back UpToDate and the caller has already found a live,
/// shim-derived build for this program, i.e. it is installed at the pin: a row is lifted
/// when the fact it stated is gone — and an older client's `blocked by …` row, whose
/// requirement gate is gone, is lifted the same way.
///
/// Every OTHER spelling is still another arm's to change — SHADOWED (the shadow
/// reconcile), `system:` (a retirement) — so it is left alone, and `tombstoned:` never reaches here at all: a tombstoned program has no real
/// shim, so it is absent from `active_builds` and the caller skips it.
fn up_to_date_row_needs_rewrite(prior: Option<&str>, build: u64, index_build: u64) -> bool {
    match prior {
        None | Some("active") => true,
        // The two MACHINE-LOCAL fault prefixes, spelled with the same literals as
        // `doctor::is_problem_state`, and an older client's `blocked by …` row (the
        // requirement gate retired, design §5.3(b)), which nothing else lifts.
        // An unserved triple's row belongs to a program with no live build to find.
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

/// Write one program's row and NOTHING ELSE: the aggregate `outcome` is carried from the
/// existing record. [`record_status`] takes the sentence as a parameter because its
/// callers ARE the pass's verdict; a row migration is not, and must not swap `up to date
/// (index build N)` for a sentence about itself.
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

/// The fix-line for `program`'s managed shim when it EXPORTS an environment before it
/// execs (design S7, [`crate::shim_env::ShimEnv::fix_line`]): `Claude Code's own updater
/// is off here (DISABLE_AUTOUPDATER=1)`, or `runs with NAME=VALUE` for an env naming no
/// self-update switch. Read off the shim AS LAID —
/// the thing that actually runs — never off a manifest or a record; `None` for a plain
/// shim. Never part of the recorded state: the surfaces that speak to a person (`which`,
/// `doctor`) append it after the canonical row as a separate sentence.
pub(crate) fn shim_env_fix(shim: &std::path::Path, program: &str) -> Option<String> {
    crate::platform::shim_env_of(shim).fix_line(program)
}

/// The fix-line for a SHADOWED `tool` of `program`: `type alab-<tool> for the managed one`
/// ([`crate::state::alias_hint`]) — printed ONLY when typing the alias really runs the
/// managed copy: `alab-<tool>` is laid, resolves into THIS prefix's `store/<program>/`
/// (the anchored containment, never the unanchored parse a hand-made link into a checkout
/// could satisfy), and is not itself shadowed by a foreign `alab-<tool>` earlier on
/// `path_var` than the managed `bin/`. `None` for a vendor tool or a system-satisfiable
/// member (no alias by policy), for a tool that is its own alias, for an install a
/// pre-alias client laid that the pass has not reconciled yet, and for an alias that
/// would itself run someone else's copy. Never part of the recorded state.
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
/// `shadow` — [`alias_fix`] for the tool that copy stands in for, so the Packages page
/// says what `which`, `doctor` and the pass log say after the same row: `type alab-<tool>
/// for the managed one`, and nothing when no alias is laid or it would run someone
/// else's copy too. Never part of the canonical state (`state::alias_hint`).
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

/// ALIAS reconcile (owner decision): for every installed, non-dev-linked
/// program, bring its `alab-<tool>` aliases in line with the signed index's verdict on
/// whether it is ALab's own ([`crate::activate::Aliases::for_program`]) — laid when
/// missing (an install a pre-alias client made, and which the pipeline short-circuits as
/// up to date forever after), swept when the entry says the program is not ALab's. A
/// primary is rewritten only when it exports other than its build declares
/// ([`crate::activate::reassert_shim_env`], design S7) — never to re-point it — so the
/// silent tick touches nothing once the primaries and aliases stand. Best-effort per
/// program: an unwritable `bin/` is the install lane's problem to report, not this pass's
/// to fail on.
fn reconcile_aliases(layout: &crate::store::Layout, index: &crate::manifest::Index) {
    // One listing of `bin/` for the whole pass, and exact for it: this pass lays `alab-`
    // aliases, prunes, and re-renders a primary IN PLACE with the target it already
    // forwards to — none of which moves a primary shim off the build `active_tools_in`
    // reports. The deleting half is not on the snapshot — `prune_stale_shims` lists the
    // directory itself, after this pass's writes, at the moment it decides.
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
    // THE PRIMARIES' ENVIRONMENT FIRST, for every program in one lay (design S7): the
    // alias reconcile below and the agents reconcile after the loop both copy what the
    // primary exports, so a primary healed here carries its alias and its twin with it
    // in this same pass. An up-to-date program is never laid again, so without this a
    // shim laid plain would keep Claude Code's own updater on until the next claude build.
    let builds: Vec<(&std::path::Path, &[crate::store::ToolName])> = live
        .iter()
        .map(|(_, build_dir, tools)| (build_dir.as_path(), tools.as_slice()))
        .collect();
    crate::activate::reassert_shim_env(layout, &builds);
    for (program, build_dir, tools) in &live {
        let aliases = crate::activate::Aliases::for_program(program, index.program(program));
        let _ = crate::activate::reconcile_aliases(layout, build_dir, tools, aliases);
    }
    // ONCE, AFTER THE LOOP: the front-of-PATH `agents/` twins. Nothing in
    // `activate::reconcile_agents` is a function of the program being reconciled — it
    // checks both agent programs and sweeps `agents/` whatever it is handed — and it used
    // to hang off the end of `activate::reconcile_aliases`, which this loop calls once per
    // ACTIVE PROGRAM. A twelve-program machine therefore did the whole agents reconcile
    // twelve times per pass (audit). Unconditional, outside the loop: an empty
    // store has no program to hang it on and its sweep is exactly what must still run.
    crate::activate::reconcile_agents(layout);
}

/// SHADOW reconcile (design S5): for every MANAGED member and every tool it exposes, a
/// foreign executable of that name EARLIER on THIS PASS's `PATH` than the managed `bin/`
/// is what runs in a shell with that `PATH`. Printed for the pass log — the canonical
/// words, `on this pass's PATH`, and the alias fix-line when the alias is laid
/// ([`alias_fix`]) — and never fixed: it is a warning the user owns. Returns the shadowed
/// programs, for the `shadowed:` line.
///
/// NEVER RECORDED (Phase 3). SHADOWED is one shell's truth — every lane runs
/// the pass with its own `PATH` (the window's login shell, the session's launch shell, a
/// person's terminal) — and recording it as machine state made rows flip with whichever
/// lane ran last. `which`, `doctor` and the Settings page compute it at read time, each
/// against the `PATH` it answers for ([`crate::status::with_shadows`]). A row an older
/// client recorded as SHADOWED is put back to the managed row: `managed <build> — pinned
/// by index <N>`, or a vendor program's version and source ([`vendor_state_of`]).
///
/// THE AGENT PROGRAMS: a `claude`/`codex` whose `agents/` twin is laid and
/// current ([`agent_twin_current`]) is what every aterm tab runs, so a foreign copy ahead
/// on this pass's `PATH` is no shadow at all: printed as the managed row plus
/// [`pass_path_tail`], and named on no `shadowed:` line — nor on the [`FOREIGN_MARKER`]
/// line, which ends every reconcile ([`FOREIGN_PATH`]) naming each SHADOWED member that
/// has an alias fix ([`alias_fix`]), with it: there is nothing for the owner to fix
/// there (owner, 2026-09-16: the remedy is never "remove or reorder that copy"), and a
/// shadow with no fix is a fact the pass's own line says, never the owner's to act on
/// (review of 2026-10-09: an exception with no action is not one).
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

/// THE EXPLICIT DOOR (`atpkg install <program>`) for the owner rule that needs the
/// verified index before the flow runs: a SYSTEM-SATISFIED member
/// ([`crate::manifest::Program::system`]) is not installed — `Some(path)` names the system
/// binary, the row is recorded and any managed copy retired ([`satisfy_by_system`]), and
/// the caller stops there with success.
///
/// `None` in every other case, INCLUDING an unresolvable index or an unlisted name — the
/// flow that follows raises the real error with its real remedy; this door never invents
/// a second diagnosis. The resolve is memoized in the fetcher, so the flow's own resolve
/// costs no second round-trip.
fn explicit_install_door(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    program: &str,
) -> Option<std::path::PathBuf> {
    // No index rule decides a vendor-direct program (design §1.4): no system
    // satisfaction, and no index resolve ahead of its own lane.
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
/// A write that did NOT land is printed. This record is the only thing keeping the
/// program out of the unattended lanes ([`seed_serviceable`] and
/// `install_default_set_inner` both filter on it, and `cmd_seed` records adoption again
/// on every launch), while the caller prints `atpkg: uninstalled {program}` and exits 0
/// whatever happened here — so an unwritable prefix would let the next launch quietly
/// reinstall it. Best-effort like every marker write, and like
/// `record_adoption`/`record_decline` the failure is said out loud.
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

/// Forget removals for `programs` — an explicit install is an unambiguous change of mind.
///
/// A failure is printed for the same reason the other direction's is: the toolset verb
/// clears the markers BEFORE a pass that narrows `wanted` by them, so a clear that did
/// not land makes that pass skip the very member it was asked to restore and then report
/// the set complete.
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
/// The open/write/rename error, or the delete's when the last entry is being lifted
/// (an already-absent file is not one). Both callers report it: nothing else in the
/// process will, and this record is load-bearing state, not diagnostics.
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
    // TEMP + RENAME, like `status::write`: truncating the live file in place meant a
    // crash between the truncate and the last line left a shorter list — every program
    // past the cut silently un-removed, and back on the next unattended pass.
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
/// coherence group's transaction aborted. The window renders it as a Warn
/// row on the toolchain lane, a terminal row THERE (`toolchain_failed`) though not an
/// answer in the announcement ledger below, which lists it nowhere. Printed by
/// [`report_channel_apply`], once per aborted group.
pub const GROUP_ABORTED_MARKER: &str = "group-aborted: ";
/// The first line of a cause, bounded for a status row and a marker line: a stage
/// error can carry a multi-line transport message, and the record and the glass want
/// one sentence (the doctor prints the full row on request).
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

/// The STABLE stdout markers the GUI parses (`crates/aterm-gui`, `parse_seed_line`).
///
/// Constants rather than inline literals because this is a CROSS-CRATE CONTRACT with a
/// silent failure mode: a reworded prefix does not break a build or a test, it just
/// means the notice never appears again — and this contract has already broken
/// undetected once. `aterm-gui` imports these, so a rename is a compile error on both
/// sides instead of a string that quietly stops matching.
///
/// Every marker is a TERMINAL answer to an announcement except the ONE that OPENS one,
/// `NET_STARTING` (its local twin `seed-starting:` went with the sealed-payload lane,
/// Phase 5 of docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md) — and the ones that
/// are neither, which the ledger deliberately leaves unclassified: `LOCK_WAITING` (a
/// non-terminal "queued behind a sibling" row; [`LOCK_WAITING_MARKER`] says what
/// retires it), `LOCK_ACQUIRED` (what retires that row when the wait ends in the
/// lock, [`LOCK_ACQUIRED_MARKER`]), `GROUP_ABORTED` (a row of its own on the toolchain
/// lane, which answers no pass's announcement) and the informational `SHADOWED`,
/// `MANAGED_CURRENT` and `MACHINE_SETTINGS` rows. [`announcement::MARKERS`] is where that
/// split is recorded and [`emit_marker_line`] is what keeps the ledger, so the classification lives in
/// one table rather than in this sentence. An announcement with no answer leaves
/// "Installing…" on screen forever.
///
/// The `seed-` spelling of the markers still printed is kept because the window parses it.
pub const SEED_UNUSABLE_MARKER: &str = "seed-unusable: ";
pub const SEED_FAILED_MARKER: &str = "seed-failed: ";
/// The NETWORK completion lane's answer — programs that arrived over the wire on an
/// adopted machine.
pub const NET_INSTALLED_MARKER: &str = "net-installed: ";
/// The network lane's ANNOUNCEMENT, printed BEFORE any bytes move — the only announcement
/// a pass prints. Emitted only through [`net_announcement`] (the default-set pass and the
/// vendor lane's fresh landings both call it).
pub const NET_STARTING_MARKER: &str = "net-starting: ";
/// The network lane's failure TERMINAL: an announced provisioning that then
/// installs nothing must retire its own status bar with the truth.
pub const NET_FAILED_MARKER: &str = "net-failed: ";
/// The dispatch edge's "queued behind a sibling" ANNOUNCEMENT: printed
/// ONCE, before any verb output, only when the caller opted into `--wait-lock`, the
/// store lock is held, and the wait has outlasted [`crate::lock::WAIT_ANNOUNCE_GRACE`].
/// It is neither a pass start nor a terminal answer — the GUI logs it (one INFO
/// line, `Wake::PkgLockWaiting`) and paints NO row (after a self-update the holder is
/// the app's own predecessor's pass): a holder with a plan shows through
/// the sibling's progress file, a no-op holder is nothing to show, and a wait that
/// ran out (exit 75) is the lane's deferred notice. Emitted by exactly one site
/// ([`mutator_store_lock`]).
pub const LOCK_WAITING_MARKER: &str = "lock-waiting: ";
/// The dispatch edge's "the wait is over" line: printed ONCE, right
/// after a wait that ANNOUNCED itself ([`LOCK_WAITING_MARKER`]) acquires the lock —
/// never after a silent wait inside the grace, never after an uncontended try, and
/// never by a typed verb. Neither a pass start nor a terminal answer, like the line
/// it answers: the GUI logs it (`Wake::PkgLockAcquired`), so the interval the child
/// spent queued is on record — there is no row to retire. The verb
/// that follows may be QUIET, or stand down at once when a full pass on this store
/// ended meanwhile ([`pass_ended_while_we_waited`]). Emitted by the
/// same one site.
pub const LOCK_ACQUIRED_MARKER: &str = "lock-acquired: ";
/// The SHADOW reconcile's answer ([`reconcile_shadowed`]): the managed programs a
/// foreign copy out-ranks on this pass's `PATH`, comma-separated — the pass printed
/// one `<program>: managed <build> — SHADOWED by <path>` line per member, so this is
/// what lets a GUI tell a pass that found a shadow from one that found none.
pub const SHADOWED_MARKER: &str = "shadowed: ";
/// The MANAGED-CURRENT answer, printed at the end of every successful update/seed
/// pass: each agent program ([`crate::stub::AGENT_PROGRAMS`]) that is installed and is
/// what every aterm tab runs, `; `-separated — the R6 row "the latest aterm-managed
/// claude/codex are in use" — as `<name> <version> (<Vendor> latest)` while it is its
/// vendor's head, else `<name> <version> (<why not>)` ([`ManagedAgent::standing`]), so a
/// pass that could not check one program never reads as all of them current (audit
/// 2026-10-09: an unreached Anthropic left `Codex 0.160.0 is current` alone on the
/// line). Omitted when no agent program is installed.
pub const MANAGED_CURRENT_MARKER: &str = "managed-current: ";

/// `machine-settings: universal-control disabled`
/// (R5/R6): what THIS pass changed about the machine per the
/// doctor's remedies — entries `; `-separated, the line omitted when nothing changed.
/// The window records it as `appstatus` ledger entries (never a status-bar row) that
/// must carry the revert ([`crate::machine::UNIVERSAL_CONTROL_REVERT`]),
/// and Settings ▸ Security's "Last change" names it.
pub const MACHINE_SETTINGS_MARKER: &str = "machine-settings: ";

/// `foreign: <scope> <items>` — what one of a pass's looks found that aterm LEAVES ALONE
/// because it did not lay it, with the fix that is the owner's (audit 2026-10-09: 32
/// passes said `~/.rustup/toolchains/trust … not aterm's link; left untouched (fix: …)`
/// and 31 said `trust: managed 9192 — SHADOWED by ~/.local/bin/trustdoc` as prose the
/// window only logged, so none reached the owner). One line per look, every time it
/// looks, `none` when it found nothing, so the window can end what a look no longer
/// sees ([`foreign_line`]): [`FOREIGN_SEAM`] (the rustup seam, [`reassert_rustup_seam`])
/// and [`FOREIGN_PATH`] (the shadow reconcile, [`reconcile_shadowed`]). Each item is
/// `<name> <path> — <fix>`, items `; `-separated: only what atpkg names a fix for is on
/// it, since an item with nothing to do is no exception.
/// Informational: neither opens nor answers an announcement.
pub const FOREIGN_MARKER: &str = "foreign: ";
/// The rustup seam's scope on the [`FOREIGN_MARKER`] line: an entry under
/// `~/.rustup/toolchains` that is not atpkg's ([`crate::seam::Refusal::Foreign`]), named
/// by its seam, fixed by [`crate::seam::DETACH_FIX`].
pub const FOREIGN_SEAM: &str = "seam";
/// The shadow reconcile's scope on the [`FOREIGN_MARKER`] line: a foreign copy ahead of a
/// managed program's tool on the pass's `PATH`, named by its program.
pub const FOREIGN_PATH: &str = "path";
/// What a scope that found nothing foreign says.
pub const FOREIGN_NONE: &str = "none";

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

/// The `machine-state:` marker bare `atpkg machine` prints — the READ counterpart of
/// `machine-settings:` (a change). One byte-stable line, `key=value` pairs joined by
/// `; `, built by [`crate::machine::machine_state_line`] and parsed back by
/// [`crate::machine::parse_machine_state`], so a window reader confirms what the machine
/// measured rather than re-deriving it from prose.
pub const MACHINE_STATE_MARKER: &str = "machine-state: ";

/// The clause a `seed-unusable:` line carries when its cause is this machine's
/// ARCHITECTURE (the signed index says it): permanent, and nothing the user can fix. One
/// spelling for the emitter ([`report_seedless_posture`]) and the window's reader
/// ([`seed_unusable_is_informational`]).
pub const UNSERVED_ARCHITECTURE_CLAUSE: &str = "for this machine's architecture";
/// The clause of the `seed-unusable:` line that says every program the index serves here
/// was removed on purpose (`Narrowing::verdict`): a decision, not a failure.
pub const REMOVED_ON_PURPOSE_CLAUSE: &str = "was removed on this machine";

/// Whether a `seed-unusable:` line's body states a FACT rather than a failure: the
/// machine's architecture is unserved ([`UNSERVED_ARCHITECTURE_CLAUSE`]) or every program
/// the index serves here was removed on purpose ([`REMOVED_ON_PURPOSE_CLAUSE`]). The
/// window's Packages badge lights for the other cause, a `[packages].exclude` that leaves
/// nothing to install — a line a person can shorten.
#[must_use]
pub fn seed_unusable_is_informational(body: &str) -> bool {
    body.contains(UNSERVED_ARCHITECTURE_CLAUSE) || body.contains(REMOVED_ON_PURPOSE_CLAUSE)
}

#[cfg(test)]
mod seed_unusable_tests {
    use super::{Narrowing, seed_unusable_is_informational};

    /// Every `seed-unusable:` cause, in the words [`super::report_seedless_posture`]
    /// prints: the unserved architecture and a removal on purpose read as facts, an
    /// exclusion as a failure a person can act on. The exclusion's sentence is pinned
    /// verbatim because aterm-gui's `an_informational_unusable_marker_is_no_failure`
    /// quotes it as atpkg prints it.
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

/// The prefix of every `atpkg machine` / `atpkg machine apply` verdict sentence
/// (`applied — …`, `nothing changed — …`, `not applied — …`, `next — …`, …). A
/// window worker keys on it to quote the verdict; spelled once here so a reword is
/// a compile error on both sides rather than a card that silently stops quoting.
pub const MACHINE_VERDICT_PREFIX: &str = "atpkg machine: ";

/// The line [`apply_machine_settings`] prints when it refuses to touch the machine
/// (a home that is not this account's): `atpkg: machine settings not applied — <why>`.
/// The verb still exits 0, so a window worker keys on THIS prefix to report the
/// refusal as the outcome rather than a success.
pub const MACHINE_NOT_APPLIED_PREFIX: &str = "atpkg: machine settings not applied — ";

/// A machine action ran but failed. The shell verb remains best-effort and can
/// exit 0, so the GUI must read this independently of its change markers and
/// final verdict. Successful changes from the same pass remain valid.
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

/// Print the pass's own `machine-state:` record: the machine re-read for the Universal
/// Control half (the keys themselves, not the apply's outcome). The same line, from the same
/// builder, that bare `atpkg machine` prints — [`MACHINE_STATE_MARKER`] — so the window's one
/// parser serves both.
fn print_machine_state_after_apply(
    cfg: &crate::config::MachineConfig,
    policy: crate::config::UniversalControlPolicy,
) {
    let state = crate::machine::universal_control_state(&crate::machine::SystemDefaults);
    let (home_posture, _) = machine_home();
    let st = crate::machine::MachineState {
        universal_control: state.posture(),
        policy,
        home: home_posture,
        config_unreadable: cfg.unreadable,
    };
    emit_marker_line(&format!(
        "atpkg: {MACHINE_STATE_MARKER}{}",
        crate::machine::machine_state_line(&st)
    ));
}

/// Apply the `[machine]` settings — from `atpkg machine apply`, and at a pass's edge when
/// the table changed ([`machine_table_changed`]) — and print what CHANGED, as the
/// `machine-settings:` marker (nothing printed when nothing changed — the window's record
/// is for a change, not a state), and record the table applied. Returns the
/// same entries, and why nothing changed ([`MachineApply`]), so `atpkg machine apply` can
/// say so without re-reading its own stdout. Universal Control:
/// [`crate::machine::apply_universal_control`] through the real `defaults`. Best-effort —
/// a machine setting must never fail the toolchain pass that carried it.
fn apply_machine_settings() -> MachineApply {
    let cfg = crate::config::cached_machine();
    let mut out = MachineApply::default();
    let entries = &mut out.entries;
    // AN ORPHANED PASS LEAVES THE MACHINE TO THE SUCCESSOR. A pass whose
    // spawning window is gone ([`crate::progress::watch_for_orphaning`]) finishes its
    // store work, but the `machine-settings:` line it would print reaches no window —
    // and disabling Universal Control is the loudest thing a pass does to a Mac; it must
    // be done by a pass that can say so. The setting is idempotent per doctor's own
    // findings, so the successor's pass, minutes later at most, applies and reports it.
    // SILENTLY: an orphaned waiter passes through here before it blocks on the lock, and
    // has by construction no reader
    // (`an_orphaned_waiter_stands_down_silently_before_its_bound`).
    if parent_is_gone() {
        return out;
    }
    // Off macOS there is nothing to apply — no Universal Control — and nothing to say
    // about it either: the home guard below would otherwise print a line on every Linux
    // pass whose HOME is not the passwd home (a CI container, this crate's own
    // integration tests), about settings that do not exist there.
    if !cfg!(target_os = "macos") {
        return out;
    }
    // A SYNTHETIC HOME IS A SYNTHETIC MACHINE. `defaults` writes the ACCOUNT's per-host
    // domain and ignores `$HOME`, so a pass pointed at another home — this crate's own
    // integration tests spawn passes against a temp HOME — must not touch the real
    // machine at all. One line says why, so a `sudo -H` or a CI job is never a silent
    // no-op.
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
    out.left_alone = policy == crate::config::UniversalControlPolicy::Leave;
    if let Some(line) = record_universal_control_outcome(outcome, entries) {
        println!("{line}");
    }
    if !entries.is_empty() {
        emit_marker_line(&format!("atpkg: {}", machine_settings_line(entries)));
    }
    // The record follows the apply, so a window that just read the change line has the
    // state a moment later instead of spawning a read of its own.
    print_machine_state_after_apply(cfg, policy);
    // What the passes compare against ([`machine_table_changed`]): the table this apply
    // acted on. Only an apply that ran gets here — an orphaned, off-platform or refused
    // one returned above — so a refusal is retried by the next pass that sees it. One whose
    // `defaults` write failed forgets the table instead, so the next pass retries it.
    if let Some(layout) = crate::store::resolve_configured() {
        if uc_failed {
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
    /// `[machine] universal_control = "leave"`: nothing is the apply's to change.
    left_alone: bool,
}

impl MachineApply {
    /// `atpkg machine apply`'s verdict when nothing changed: the one cause this apply saw.
    fn nothing_changed(&self) -> String {
        if self.failed {
            "nothing changed — a change did not land".to_string()
        } else if self.left_alone {
            "nothing changed — [machine] universal_control = \"leave\"".to_string()
        } else {
            "nothing changed — already applied".to_string()
        }
    }
}

/// `<prefix>/machine.lock`, held (blocking) for one apply of the `[machine]` settings: the
/// window's launch apply, a session's and a pass edge carrying an edit may start at once,
/// and two of them reading and writing the same per-host keys together is the one thing
/// the queue exists to stop. THE PREFIX IS MADE FIRST, as the store lock makes it
/// ([`crate::store::Layout::ensure_dir`], write-free over one that exists): on a first
/// launch there is none yet, and the window's launch apply and the launch seed's edge both
/// found "no prefix" and applied unqueued, side by side. `None` — unqueued — only for a
/// prefix that cannot be made (a link, someone else's directory) or a lock file that
/// cannot be opened. Released when the returned guard drops — by
/// `LOCK_UN`, not by the close alone: a child any thread forks while an apply holds the
/// queue (the window's shells) keeps a copy of its description until it execs, and a
/// queue let go by the close stayed taken there, holding the next apply in line behind a
/// shell's exec rather than an apply.
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

/// A pass's `[machine]` edit ([`main_entry`]), applied BESIDE the pass: on its own thread,
/// started before the store lock is asked for and joined when this guard drops (or at
/// [`Self::join`]). The `defaults` reads and writes — tens of milliseconds; none while
/// another apply holds the queue ([`apply_machine_edit`]) — then delay neither the lock
/// attempt nor the work after it, and the edit has still landed, and said so on stdout,
/// before the pass exits.
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

/// Why the `[machine]` settings must NOT touch this process's machine, or `None` when
/// they may: `$HOME` (what `home_dir()` answers) is not the account's home, or the
/// account could not be resolved. The decision ([`machine_home_verdict`]) is pure over
/// its inputs so the rule is pinned by a test rather than by prose.
/// Whether this verb is a pass that applies the `[machine]` settings when their table
/// CHANGED ([`machine_table_changed`]): the three long passes and `install <name>`.
/// `update <vendor program>` never does (the edge checks its operand) — it is the lane a
/// `claude update` runs, and must not write a host preference. The quick mutators never do.
fn verb_applies_machine_settings(verb: &str) -> bool {
    verb_row(verb).is_some_and(|v| v.applies_machine)
}

/// Whether the `[machine]` table differs from the one last applied on this machine
/// ([`crate::machine::changed_since_applied`]) — never applied counts as changed. A pass
/// asks this instead of applying them every time (Phase 3): the settings' own verb,
/// which the window and the session run on their schedule, keeps them applied; a pass
/// only carries an EDIT to them.
fn machine_table_changed() -> bool {
    crate::store::resolve_configured().is_some_and(|layout| {
        crate::machine::changed_since_applied(&layout, crate::config::cached_machine())
    })
}

/// Why the `[machine]` settings must NOT be applied in this process, or `None`.
///
/// Two refusals, the config one first because it is the cheaper question and the one
/// the owner can fix ([`machine_config_refusal`]). Then the synthetic-home rule
/// ([`machine_home`]).
fn machine_refusal(cfg: &crate::config::MachineConfig) -> Option<String> {
    machine_config_refusal(cfg).or_else(|| machine_home().1)
}

/// [`machine_refusal`]'s config half: a config file that EXISTS and does not parse cannot
/// tell us whether either setting was switched off, and both defaults ACT.
fn machine_config_refusal(cfg: &crate::config::MachineConfig) -> Option<String> {
    cfg.unreadable.then(|| {
        "aterm.toml does not parse, so the [machine] opt-outs could not be read — fix the \
         file (`aterm --window --validate-config`), then `aterm pkg machine apply`"
            .to_string()
    })
}

/// The synthetic-home rule, resolved against this process: the posture the
/// `machine-state:` line reports, and — when it is not the account's home — the one
/// sentence that says why nothing is applied.
fn machine_home() -> (crate::machine::HomePosture, Option<String>) {
    // AN EMPTY `HOME` IS NOT A HOME. `home_dir()` answers `Some("")` for `HOME=`
    // (set but empty), which renders as a blank in the refusal sentence — "HOME is
    // but this account's home is …" — and reads as a bug in the message rather than
    // the misconfiguration it is reporting.
    let home = aterm_types::dirs::home_dir().filter(|h| !h.as_os_str().is_empty());
    let account = crate::platform::account_home();
    let same = match (&home, &account) {
        (Some(h), Some(a)) => same_directory(h, a),
        _ => false,
    };
    machine_home_verdict(home.as_deref(), account.as_deref(), same)
}

/// The decision behind [`machine_home`], on its inputs. FAIL-CLOSED when `HOME` is
/// set: an account that could not be resolved is not proof that `$HOME` is it, so
/// nothing is applied — the direction the `account_home` doc promises. No `HOME` at all
/// (`home_dir()` is `None`: unset, and `/etc/passwd` — where macOS local accounts do not
/// appear — has no entry) is treated as the account's home, resolved or not: there is no
/// redirect to guard against. `same` is the
/// filesystem's own answer ([`same_directory`]) for the spellings a string compare gets
/// wrong — a symlinked, case-variant or firmlinked home.
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

/// Whether two paths name ONE directory — by device and inode, following symlinks —
/// so `/users/x`, a symlinked home, and the `/System/Volumes/Data/Users//x` firmlink
/// spelling (which `canonicalize` does not unify; firmlinks are not symlinks) all read
/// as the account home they are. A missing path is `false`: nothing is proven by it.
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

/// `atpkg machine [apply]` — the `[machine]` settings' own door.
///
/// Bare `atpkg machine` READS: the doctor's Universal Control line, the byte-stable
/// [`MACHINE_STATE_MARKER`] record a window reader parses, and one verdict — `next — …`
/// naming what an apply would do, `nothing to apply`, or — on a home that is not this
/// account's — `not applied here — <why>`. `atpkg machine apply` runs
/// [`apply_machine_settings`] on the spot — what the window and the session run to keep
/// them applied — and then says what changed, that nothing changed, or that nothing was
/// applied and why, because a verb that applies and prints nothing looks broken. Exit 0
/// either way: this is a state, not a gate. Anything else is a usage error.
fn cmd_machine(rest: &[String]) -> ExitCode {
    match rest {
        [] => {
            if !cfg!(target_os = "macos") {
                println!("{MACHINE_VERDICT_PREFIX}not applicable — these are macOS host settings");
                return ExitCode::SUCCESS;
            }
            let cfg = crate::config::cached_machine();
            // ONCE: `universal_control()` warns on stderr about a spelling it does not
            // know, so asking twice printed the same warning twice per read.
            let policy = cfg.universal_control();
            let state = crate::machine::universal_control_state(&crate::machine::SystemDefaults);
            println!("{MACHINE_VERDICT_PREFIX}{}", state.state_line(policy));
            let (home_posture, mismatch) = machine_home();
            let st = crate::machine::MachineState {
                universal_control: state.posture(),
                policy,
                home: home_posture,
                config_unreadable: cfg.unreadable,
            };
            // THE LINE THE WINDOW READS — after the prose, so a person still sees
            // sentences first, and after the platform check, so its absence means "not
            // macOS" and never "nothing to apply".
            emit_marker_line(&format!(
                "atpkg: {MACHINE_STATE_MARKER}{}",
                crate::machine::machine_state_line(&st)
            ));
            // ONE verdict for both surfaces: `st.next()` is what the window's Apply button
            // reads too, so the CLI cannot say "nothing to apply" while an apply would act.
            let refusal = machine_config_refusal(cfg).or(mismatch);
            match (refusal, st.next()) {
                (Some(reason), _) => {
                    println!("{MACHINE_VERDICT_PREFIX}not applied here — {reason}")
                }
                (None, false) => println!(
                    "{MACHINE_VERDICT_PREFIX}nothing to apply — Universal Control is where \
                     [machine] wants it"
                ),
                (None, true) => println!(
                    "{MACHINE_VERDICT_PREFIX}next — aterm pkg machine apply (Universal Control \
                     off for this host)"
                ),
            }
            ExitCode::SUCCESS
        }
        [sub] if sub == "apply" => {
            if !cfg!(target_os = "macos") {
                println!("{MACHINE_VERDICT_PREFIX}not applicable — these are macOS host settings");
                return ExitCode::SUCCESS;
            }
            // The guard is asked here as well as inside the apply: the apply prints the
            // refusal and returns nothing, and a verb that then said "nothing changed"
            // would contradict the line above it.
            if let Some(reason) = machine_refusal(crate::config::cached_machine()) {
                let _ = apply_machine_settings();
                println!("{MACHINE_VERDICT_PREFIX}not applied — {reason}");
                return ExitCode::SUCCESS;
            }
            // THE RAW APPLY, never the once-wrapper: `machine apply` is the explicit
            // gesture, and a person who runs it twice is asking the machine to be read
            // and written twice — not to be told what some earlier call in this process
            // found. (The edge never applies for this verb anyway: it takes no store
            // lock, so it is not in `verb_applies_machine_settings`.)
            let queue = machine_apply_queue();
            let applied = apply_machine_settings();
            // THE QUEUE GUARDS THE PER-HOST KEYS, NOT THE CACHE WALK: let it go before the
            // housekeeping below. A pass's edge queued behind this apply ([`MachineEdit`],
            // the launch seed's on a first launch) is joined before its pass exits, and the
            // window starts the install only when that seed has exited, so a walk (and the
            // removal of gigabytes) under the queue held the toolchain install behind it.
            drop(queue);
            if applied.entries.is_empty() {
                // THE CARD SHOWS THIS SENTENCE ALONE (the window renders the verdict
                // without the narration), so it names the one cause the apply saw.
                println!("{MACHINE_VERDICT_PREFIX}{}", applied.nothing_changed());
            } else {
                println!(
                    "{MACHINE_VERDICT_PREFIX}applied — {}",
                    applied.entries.join("; ")
                );
            }
            // The day's housekeeping rides the day's apply (`launch_apply_due`), after its
            // verdict: Rust build cache folders nobody builds into any more go.
            let removed = crate::build_cache::reclaim_home();
            if !removed.is_empty() {
                println!(
                    "removed {} unused Rust build cache folder(s)",
                    removed.len()
                );
            }
            ExitCode::SUCCESS
        }
        // NAME THE WORD THAT WAS REFUSED: a bare usage block leaves
        // the reader to diff their command against it, and `machine help` (a reasonable
        // guess) looks identical to a typo.
        other => {
            let problem = if let [word] = other {
                format!(
                    "unknown argument {word:?} — a bare `machine` reads, `machine apply` \
                     applies"
                )
            } else {
                format!(
                    "too many arguments ({}) — a bare `machine` reads, `machine apply` \
                     applies",
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
    /// What the parenthesis says: `<Vendor> latest` while a check that reached the vendor
    /// this run (and within a day) found it the head; [`NOT_CHECKED`] when this run could
    /// not reach the vendor or no check has for a day; else
    /// [`crate::state::not_current`]'s word for its row (`held`, `failed`, …).
    pub(crate) standing: String,
}

/// The standing of a managed agent program whose vendor this run could not reach, or no
/// check has reached for a day: its row is an older check's ([`ManagedAgent::standing`]).
pub const NOT_CHECKED: &str = "not checked";

/// The `managed-current:` payload ([`MANAGED_CURRENT_MARKER`]) for `entries`, in roster
/// order — `claude 2.1.280 (Anthropic latest); codex 0.156.0 (OpenAI latest)`, or
/// `claude 2.1.280 (not checked); …` for a program the pass could not check. `None` when
/// there is no entry: the line is then omitted.
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

/// Whether `agents/<name>` — the copy every aterm tab runs (owner decision) —
/// is laid and resolves exactly where `bin/<name>` does, into `store/<name>/<build>`.
/// The predicate behind the `managed-current:` line's honesty and the agent programs'
/// shadow wording.
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

/// [`agent_twin_current`] AND the twin's bytes carry the self-update hand-over
/// ([`twin_carries_hand_over`]) — the predicate behind the `which` sentence `` `claude
/// update` here runs `aterm pkg update claude` ``. Target equality is not
/// enough: a twin laid by an older client resolves exactly where `bin/` does and still
/// runs the vendor's updater on `claude update` until a pass re-lays it (the dev box's
/// own `agents/claude`, laid by the installed app, carried only the landing block when
/// this was reviewed), and the `.cmd` twin resolves the same and carries no block at
/// all ([`crate::platform::twin_prelude`] renders nothing on Windows, pinned).
/// One bounded, non-link regular read; anything unreadable answers `false`, never a
/// sentence.
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
/// [`crate::selfupdate::HIDDEN_VERB`] appears nowhere in a shim this crate lays EXCEPT
/// the `sh` self-update block (an agent program's twin and its `bin/` shim; the plain
/// render carries none, and the `.cmd` twin renders none). Pure, so the Windows bytes are pinned from the Unix suite like every `.cmd` test.
fn twin_carries_hand_over(bytes: &[u8]) -> bool {
    bytes
        .windows(crate::selfupdate::HIDDEN_VERB.len())
        .any(|w| w == crate::selfupdate::HIDDEN_VERB.as_bytes())
}

/// The pass log's tail for an agent program with a foreign copy ahead on the PASS's own
/// `PATH`: `a foreign copy is ahead on this pass's PATH (<path>); a tab opened on this
/// build puts agents/ first on its own`. A fact about this process, not about any shell a user is in.
fn pass_path_tail(foreign: &std::path::Path) -> String {
    let mut s = String::from("a foreign copy is ahead on this pass's PATH (");
    s.push_str(&foreign.display().to_string());
    s.push_str("); a tab opened on this build puts agents/ first on its own");
    s
}

/// The in-place remedy for a shell whose `PATH` lacks `agents/` (or has it behind a
/// foreign copy): SOURCE THE HOOK WHERE IT STANDS — `. ~/.aterm/shell.d/00-atpkg.zsh`
/// (`. …bash`; `source …fish` for fish; the dot-source `. …ps1` for pwsh) — whenever
/// that hook file exists for the INVOKING shell's family ([`crate::hooks::hook_file`]);
/// the same sentence the window records for a tab from before the update (a ledger
/// entry `aterm ctl appstatus` reads, never a row on the glass). Only
/// where no hook file exists (a shell atpkg writes no hook for, an install whose hook
/// write failed, no shell/`HOME` known at all) is a `PATH` line printed instead — in the
/// shell's own dialect (`export PATH="<agents>:$PATH"`, fish's `set -gx PATH …`, pwsh's
/// `$env:PATH = …`), so what is printed runs where it is typed.
///
/// THE SHELL IS THE ONE THAT TYPED THIS, not `$SHELL` ([`crate::caller_shell`]): aterm
/// spawns the configured shell without re-pointing `$SHELL`, so a fish tab on a
/// zsh-login machine would be told a line fish cannot source. The parent process is the
/// shell that typed
/// the command (an `exec`'d shim keeps it); `$SHELL` answers only when the parent is not
/// a shell (`sudo`, a script driver).
///
/// NEVER `exec $SHELL`: inside an aterm tab a re-exec'd shell LOSES the tab's shell
/// integration — `aterm-shell-integration`'s zsh wrapper consumes
/// `ATERM_ORIGINAL_ZDOTDIR` before sourcing, so the new zsh never loads
/// `aterm_shell_integration.zsh` (bash rides `--rcfile`, same loss): no OSC 133/633
/// marks, no cwd/title tracking, no live `PATH` re-assert. Sourcing the hook in the live
/// shell is the whole remedy and keeps everything the tab has. Outside a session the same
/// source works — it is what the rc block runs — so it is the ONE remedy everywhere: the
/// same tab, never a new one.
pub(crate) fn shell_remedy_command(layout: &crate::store::Layout) -> String {
    let shell = crate::caller_shell::invoking_shell();
    let home = aterm_types::dirs::home_dir();
    shell_remedy_for(shell.as_deref(), home.as_deref(), layout)
}

/// [`shell_remedy_command`] over an explicit `shell` (the invoking shell's name —
/// `zsh`, `bash`, `fish`, `pwsh`) and `home`, so a test can lay the hook under a
/// throwaway home and read the exact line.
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
/// one shell's `PATH` under that shell's environment: the surfaces that say
/// which copy runs ask this BEFORE they walk PATH for a shadow, because with the stub
/// first the walk stops at it (`reroute/` holds the name) and would read every shell as
/// running the managed copy — including one where the stub passes through.
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

/// The foreign copy of `tool` that RUNS in a shell with `path_var` and `env` — the shadow
/// probe ([`crate::vendor::shadowing_binary_on_path`]) with an agent program's reroute stub
/// taken into account: none where the stub answers first and takes the
/// managed twin, the probe over the stub's own walk where it passes through, and the
/// probe over `path_var` where no stub of ours answers first. What (10d)/(10f) of the
/// doctor and `which` ask, so the three cannot disagree.
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

/// The AGENT PROGRAMS' row for a shell OUTSIDE aterm whose reroute stub answers first and
/// passes through to `foreign`: that is the stub doing what the owner's law
/// says — *"claude managed via aterm should be in aterm only"* — so it is said as the
/// design ([`crate::state::agent_passed_through`]), never as SHADOWED with a remedy: the
/// hook the shadow row names demotes `agents/` outside aterm, and the stub skips it anyway.
/// The same holds where no stub answers first: an iTerm shell has no reroute directory on
/// its PATH, and its own copy running there is the design too
/// ([`crate::state::agent_own_copy_outside_aterm`]) — whether the stub is LAID or not
/// (`docs/PLAN-agents-dir-aterm-only-2026-09-22.md` steps 6-8). `None` inside aterm, where
/// a pass-through means the twin is gone and the machine-wide wording is the truth, and
/// where the twin is not current. So [`agent_shell_shadow`] is reached inside aterm only.
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

/// The AGENT PROGRAMS' routed row for ONE shell: `Some` when `program`'s
/// reroute stub answers first on `path_var` and takes the managed twin under `env`, the
/// twin is current, and a foreign copy stands behind the stub — the copy this shell's
/// `PATH` would run were the stub not there ([`crate::reroute::without_reroute_dirs`]:
/// `agents/` KEPT, so a shell that already puts it first names nothing)
/// ([`crate::state::agent_routed_in_shell`]). `None` otherwise, including a routed shell
/// with no foreign copy to name.
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

/// The AGENT PROGRAMS' shadow row for ONE shell: `Some` when `program` is an
/// agent program whose `agents/` twin is laid and current — so aterm's copy is what
/// every tab runs, and `foreign` ahead of it on `path_var` is this shell's state, not
/// the machine's ([`crate::state::agent_shadowed_in_shell`]). `None` for every other
/// program: their `managed <build> — SHADOWED by <path>` row is unchanged.
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
/// member with an active build its row names, known by a vendor version, and whose
/// `agents/` twin is that build (what every aterm tab runs), with its version and
/// standing ([`ManagedAgent::standing`]): its
/// vendor's latest when its row says so and a head check reached the vendor this run and
/// within a day before `now`; [`NOT_CHECKED`] when the vendor is among the `unchecked`
/// (this run could not reach it) or no check has reached it for a day — the row is an
/// older check's; else the word for why its row is not the head. Offline: no index names
/// these programs.
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
            // HONEST: the row this line becomes says the managed copies are
            // what every tab runs — true only when the front-of-PATH twin really IS this
            // build. A pass whose twin was not laid (agents/ unmakeable, a lane that
            // failed) or is still on the previous build says nothing here.
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
            // refusal) whatever this run reached; one the vendor's channel has not
            // judged lately — or that no lane has judged — is not checked.
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

/// Print the `managed-current:` marker at the end of every run of the vendor lane (R6:
/// "the latest aterm-managed claude/codex are in use", which the window records in its
/// `appstatus` ledger and log), whatever the index lane did. Offline: it reads the rows
/// the vendor lane recorded, less the `unchecked` programs and those no check has
/// reached for a day before `now` ([`managed_current_entries`]).
fn print_managed_current(
    layout: &crate::store::Layout,
    unchecked: &std::collections::BTreeSet<String>,
    now: i64,
) {
    if let Some(line) = managed_current_line(&managed_current_entries(layout, unchecked, now)) {
        emit_marker_line(&format!("atpkg: {line}"));
    }
}

/// The POSITIVE terminal: an announced pass that ran to its end and has no richer
/// marker of its own to print.
///
/// Every ending has a line of its own, so an absence means one thing only — nobody
/// answered (a child that DIES after announcing is also silent).
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

/// THE ANNOUNCEMENT LEDGER — the marker contract's other half, kept by the process
/// instead of by each verb's exits.
///
/// `net-starting:` OPENS a held "Installing the ALab toolchain…" card in the GUI;
/// every other marker CLOSES it, so a verb that opened one must close it on every
/// return path.
///
/// The bookkeeping lives where a marker reaches stdout ([`emit_marker_line`], and
/// its one lossy twin [`emit_marker_line_lossy`] for the waiter's `seed-busy:` line)
/// rather than at each print site, so a lane that grows a new early return cannot
/// forget to answer — answering is not its job.
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

    /// Every marker in the stdout contract, and whether it OPENS an announcement
    /// rather than answering one. The GUI's `parse_seed_line` matches the same set;
    /// this table is what keeps the two halves from drifting.
    ///
    /// `lock-waiting:` ([`super::LOCK_WAITING_MARKER`]) is deliberately ABSENT: it is
    /// printed at the dispatch edge, before any verb, and is neither half of the
    /// contract — not an announcement (nothing to retire) and not an answer (were it
    /// one, a child that waited and then announced would look answered before it
    /// began). [`note`] ignores an unlisted prefix, which is the right reading of it.
    /// So is `lock-acquired:` ([`super::LOCK_ACQUIRED_MARKER`]), printed at the same
    /// edge when that wait ends in the lock: it answers the wait, not an announcement.
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

/// THE way a marker reaches stdout: print it, and record it against the
/// announcement ledger. The bytes are unchanged — `line` is already the whole
/// `atpkg: <marker><body>` line the GUI parses — so this adds bookkeeping and
/// nothing else. `println!` on purpose: an INSTALLING child whose window has quit
/// is meant to die at its next line of output (aterm-gui's final-exit path in its
/// `main_entry`), and a marker is one of those lines. A WAITER's three
/// lines — `lock-waiting:`, `lock-acquired:` and `seed-busy:` — go through
/// [`emit_marker_line_lossy`], the same printer with the panic taken out.
///
/// ON A PERSON'S TERMINAL the line is shown as a sentence instead ([`human_marker`]) — or
/// not at all, when a sentence beside it already says it — and the ledger still records
/// the marker. Every pipe, and so every window child, keeps the bytes above.
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
    /// TESTS ONLY: every marker line [`emit_marker_line`] printed on THIS thread, so a
    /// test can read which terminal a pass chose (the announcement ledger is
    /// process-wide and says only open/answered, and parallel tests share it). Drained
    /// by [`take_markers_said`]. The doc sits inside the macro: on the invocation it would be
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
/// bytes and the same ledger entry, but a write whose failure is ignored. A waiter's
/// contract is a SILENT stand-down with exit 75 once its window has gone, and the
/// [`parent_is_gone`] guard at the lock edge is a snapshot — a window that quits
/// between it and this print leaves a dead pipe for a stdout, where `println!`
/// would panic (exit 101) and an `EPIPE` nobody sees is the promised outcome.
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

/// A marker line as a PERSON reads it: the plain sentence a terminal shows in its place
/// ([`emit_marker_line`]), or `None` when it says nothing a person needs — a record for the
/// window's ledger (`managed-current:`, `machine-state:`), or a fact a line beside it
/// already says (`group-aborted:`, `lock-acquired:`, `shadowed:` and `foreign:`, whose
/// items each had a line naming the copy or entry and the fix, and a `machine-settings:`
/// line whose one change is Universal Control, said beside it with its revert). A line
/// that is no marker is itself. Pure, and pinned per marker.
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
        // A fact the sentence beside it already says — the intercept's epilogue, or
        // the typed lane's timed-out sentence — so on a terminal it is shown as nothing,
        // the rule `lock-acquired:` already follows. The marker's bytes still reach every
        // pipe and the ledger.
        return None;
    }
    if marked(LOCK_WAITING_MARKER).is_some() {
        return Some(
            "Waiting for another atpkg process to finish\u{2026} (Ctrl-C to stop)".to_string(),
        );
    }
    if let Some(body) = marked(MACHINE_SETTINGS_MARKER) {
        // The Universal Control change is said on its own line beside this one, revert
        // and all ([`record_universal_control_outcome`]): not twice.
        let said: Vec<&str> = body
            .split("; ")
            .filter(|entry| *entry != crate::machine::UNIVERSAL_CONTROL_ENTRY)
            .collect();
        return (!said.is_empty())
            .then(|| format!("Changed this Mac's settings: {}.", said.join("; ")));
    }
    if marked(LOCK_ACQUIRED_MARKER).is_some()
        || marked(GROUP_ABORTED_MARKER).is_some()
        || marked(SHADOWED_MARKER).is_some()
        || marked(FOREIGN_MARKER).is_some()
        || marked(MANAGED_CURRENT_MARKER).is_some()
        || marked(MACHINE_STATE_MARKER).is_some()
    {
        return None;
    }
    Some(line.to_string())
}

/// ANSWER AN ANNOUNCEMENT THIS VERB LEFT OPEN. Called at the tail of every verb that
/// can announce; a no-op for the lanes that already printed a terminal of their own,
/// which is why their stdout is byte-unchanged.
///
/// `ok` is the verb's OWN verdict, not a guess from what was printed. That matters:
/// the reader needs a failed pass to say so, and an absence cannot distinguish
/// "finished quietly" from "died". A verb that announced and then failed without a
/// marker gets the failure terminal; one that announced and succeeded gets the
/// positive one, carrying the STORE's own count — a fact about what is installed
/// rather than a claim about what this pass did.
fn answer_announcement(ok: bool) {
    if !announcement::is_open() {
        return;
    }
    if ok {
        let held = layout().map_or(0, |l| crate::active_builds(&l).len());
        if held == 0 {
            // EXIT 0 OVER AN EMPTY STORE IS NOT A SUCCESS TO REPORT. The pass
            // announced an install and the disk shows none — a positive fact, and a
            // better witness than the exit code, which says only that no step
            // errored. Reporting the tick here is how a green "toolchain installed"
            // lands over an empty prefix.
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

/// The human line printed on its own row AFTER the install marker (`net-installed:`) —
/// never appended to the marker itself, which the GUI parses byte-for-byte. Answers the
/// first question a fresh install raises: how do I run one?
const SEED_FOLLOW_ON: &str = "atpkg: these tools are on PATH in every aterm shell — \
                              aterm pkg list shows them; aterm <tool> runs one anywhere";

/// WHETHER AN UPDATE PASS MAY COMPLETE THE SET — the whole consent policy, as one pure
/// function so it can be tested. Batteries included: the ONE install consent
/// (`[packages].auto_install`, default TRUE) is the "yes", adoption is how it was
/// exercised, and every explicit opt-out wins over both:
///
/// * `consent` — [`crate::config::PackagesConfig::auto_install`]: "may atpkg install
///   toolset members nobody named?", so switching it off stops the completion on a
///   machine that adopted long ago too.
///   It is [`crate::config::PackagesConfig::installs_unattended`] here: the consent, and no
///   `[packages] prefix` this build ignores (the toolset lives where that line points).
/// * `adopted` — this machine runs the toolset: the first-run seed pass adopted it under
///   the consent, or a person ran `install --default-set`. Adoption stays an input because
///   it is what the opt-outs and the guards take back: `uninstall <program>` clears it
///   (the next launch's seed pass re-adopts, and the program's removed marker keeps it
///   out).
/// * `asked` — `auto_install = true` is WRITTEN in the table: the one way a machine no
///   window ever runs on — an SSH or Linux terminal, where
///   only the session lane's `update` pass runs and nothing adopts — gets the toolset
///   unattended, and gets it back after an `uninstall <program>` cleared adoption. So a
///   written `true` stands in for adoption; the default `true` does not (the seed adopts).
/// * `enabled` — [`crate::config::PackagesConfig::enabled`], Automatic updates: off, no
///   pass installs anything new, typed or not (a person who switched automatic updates off
///   and types `aterm pkg update` asked for updates, not a multi-GB install).
/// * `declined` — the user removed the toolset on purpose (`uninstall --all`). Checked
///   HERE and not only in the seed lane: the network pass is what the loop runs, so a
///   decline honoured locally while this pass reinstalled the set worked for exactly
///   one launch.
/// * `config_unreadable` — the `[packages]` table did not parse
///   ([`crate::config::PackagesConfig::unreadable_table`]; `consent` already reads FALSE
///   for it, and this input keeps the rule explicit): this lane is narrowed by
///   `[packages].exclude`, and an unreadable table has no exclusions to narrow it WITH.
///   Completing the set on those terms reinstalls, over the network, precisely the
///   program the owner excluded. The pass still applies updates to what is installed;
///   only the lane that installs something NEW waits for the file to be fixed.
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

/// Forget a decline — any EXPLICIT install is an unambiguous change of mind, and undoing
/// one must never require finding a marker file.
fn clear_decline(layout: &crate::store::Layout) {
    remove_marker(&layout.declined());
}

/// Record each freshly-installed `requires` dependency of `parent` into `status.toml`
/// (each carries its own SIGNED tree_root for `verify`). Shared by [`do_install`] and the
/// default-set bootstrap's ungrouped arm — identical writes at both.
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

/// Install/force-upgrade one program to its `channel`-pinned build, advancing the durable
/// anti-rollback floor and recording status. Shared by `install` and `update`; `channel`
/// is the one channel ([`crate::config::CHANNEL`], `stable` — `[packages].channel` is
/// retired) threaded from the verb edge so this stays testable without process state.
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
    // THE EXPLICIT DOOR FORGETS A DIGEST REFUSAL. An unattended pass that met a pinned
    // build whose signed `sha256` the published asset does not match records
    // that verdict and stops re-downloading the asset on every six-hourly tick
    // (`store::record_stage_refusal`). A person typing this verb is the one event that can
    // mean "the publish — or the proxy that mangled the transfer — is fixed, try now", so
    // this door drops the memo and pays for the download again. Only the verbs a person
    // types reach here (`install <p>`, and `update <p>` on a program that is not yet
    // installed); the loop's own lanes go through `flow::install`/`apply_channel` and keep
    // the cooldown.
    crate::store::clear_stage_refusals(layout, program);
    if crate::vendor_direct::is_vendor(program) {
        let _ = crate::vendor_direct::ProgramStamp::clear_refusal(layout, program);
    }
    // `program → pinned asset name` for everything this pass resolves (the program AND its
    // `requires` pull-ins) — filled even for a member whose download then fails, which is
    // what lets the pass-end gc below spare that member's `.part` resume state.
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
        // The vendor lane took nothing from an index but its yanks (a latch refresh at
        // most): no floor to advance, no dependencies, and its row is already the lane's
        // own.
        Ok(r) if r.vendor.is_some() => {}
        Ok(r) => {
            // Advance the durable high-water floor to the index we just trusted (§8 gate 3).
            advance_floors(layout, r.index_build, r.roster_seq);
            // Record each pulled-in dependency FIRST (§17), so the main program's outcome
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
            //
            // Under the channel this install just verified (its resolve refreshed the cache),
            // so an install that moved a program off a yanked build keeps the build
            // `aterm pkg rollback` lands on, never the yanked one.
            if !r.already_current {
                let channel = last_verified_channel(layout);
                let _ = crate::gc::run_keeping_pinned_partials(layout, channel.as_ref(), &|p| {
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

/// `install <vendor program>`: the vendor lane for that one program under `seams`, its row
/// recorded by the lane's own writer ([`record_vendor_status`] — a rollback off a yanked
/// build says so, never "latest"), and after a move the gc over that program alone and the
/// shell hooks ([`settle_vendor_pass`]), as `update <vendor>` does.
///
/// # Errors
/// The lane installed nothing it could stand behind ([`crate::vendor_direct::lane::Outcome::into_install`]).
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

/// The `install` verb's argv: ONE program name, or `--default-set` (`--elevate=…` went
/// with the OS-installer protocols, design §5.3(b), and is refused as a flag). The
/// grammar is decided by [`parse_install_argv`] before anything dispatches.
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

/// [`cmd_install`] reached from `update <program>` on a program that is NOT
/// installed. Same path, same behaviour -- only the verb the messages name
/// changes, so a reader is never told that a command they did not type failed.
fn cmd_install_for_update(program: &String) -> ExitCode {
    cmd_install(Some(program), "update")
}

/// `atpkg install <program>` — resolve+verify the signed index from the configured
/// account, then install/force-upgrade the program's channel-pinned build for this
/// triple (download → verify_and_stage → activate → shim). Inert unless a root key is
/// pinned at build time. `atpkg install --default-set` routes to the explicit whole-set
/// bootstrap ([`cmd_install_default_set`]).
fn cmd_install(program: Option<&String>, invoked_as: &str) -> ExitCode {
    let Some(program) = program else {
        // A bare `install` is nearly always someone asking for the toolset, and the product
        // default IS the whole set — so say the spelling that grants it rather than a usage
        // line that leaves them to guess which of the two forms they wanted.
        eprintln!(
            "atpkg install: name a program, or `aterm pkg install --default-set` for the whole \
             ALab toolset"
        );
        return ExitCode::from(2);
    };
    if program == "--default-set" {
        return cmd_install_default_set();
    }
    // A FLAG IS NOT A PROGRAM NAME. A mistyped or unsupported option — the
    // measured case was `--progress-file`, whose value the arg scanner strips
    // only in the forms it knows — otherwise lands here as the program to
    // install, gets RECORDED as a wanted program, and then sits in the local
    // record as a row `doctor` has to explain away. Refuse it at the door and
    // name the two spellings that work.
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
    // Asking for this program back lifts its removal record — otherwise the user
    // would install it, and then have to know about a marker file to stop the next
    // unattended pass treating it as "absent by request" again.
    clear_removed(&layout, std::slice::from_ref(program));
    cmd_install_with(
        program,
        &layout,
        cfg,
        &*resolve_fetcher(&layout),
        invoked_as,
    )
}

/// The body of [`cmd_install`] against an ALREADY-BUILT fetcher, so a caller that has one
/// does not construct a second.
///
/// A fresh fetcher throws away the first one's memoized index candidates and manifests, so
/// the signed index (a discovery walk plus four asset downloads per candidate) would be
/// fetched again. `atpkg update <ungrouped-program>` paid that twice when it re-entered
/// through `cmd_install`.
fn cmd_install_with(
    program: &str,
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    fetcher: &dyn crate::flow::Fetcher,
    // THE VERB THE USER TYPED, for messaging only: `update <p>` on a program that is
    // not installed routes here (one install path), and a failure must name the verb
    // they typed (audit D-4).
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
            // THE SEAM AND THE EXEC ROOTS, on the single-program door too.
            //
            // `cmd_update_all_code`, `install_default_set_inner`, `cmd_seed` and
            // `cmd_rollback` have all re-asserted the rustup seam and reconciled the exec
            // roots at the end of their pass since those calls were written; the lane a
            // PERSON types — `aterm pkg install <p>`, `aterm pkg update <p>`, which both
            // end here — never did. That asymmetry is not a detail: the seam is how
            // `cargo +trust` and every `rust-toolchain.toml` find the managed compiler, and
            // the verb a user reaches for when they suspect their compiler is stale is
            // `aterm pkg update trust`. On m3 that verb answered "up to date" for two weeks
            // and did not once look at, or heal, the `~/.rustup/toolchains/trust` entry
            // that was answering for it.
            //
            // Both are best-effort and both print only what CHANGED or was refused, so an
            // install on a healthy machine gains no output. A FOREIGN entry is refused
            // rather than re-pointed — `reassert` never follows or replaces a link atpkg did
            // not lay — and the refusal is both printed here and recorded, which is what
            // turns it into a `doctor` fault instead of a line in a scrollback.
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
        // A target the pinned build — or the vendor — does not serve is a STATE, never an
        // error (design §2): say the canonical words (the record already carries them) and
        // exit clean.
        Err(e @ (crate::FlowError::NoArtifact(_) | crate::FlowError::VendorUnavailable(_))) => {
            let state = canonical_non_error_state(&e)
                .unwrap_or_else(|| crate::state::unavailable(current_triple(), ""));
            println!("atpkg: {program}: {state}");
            ExitCode::SUCCESS
        }
        // The vendor lane's line IS the verdict — the same bytes `update <p>` prints
        // through `report_vendor` — and it names the program itself, so a `{verb}
        // {program} failed:` head would name it twice (audit).
        Err(crate::FlowError::Vendor(line)) => {
            eprintln!("atpkg: {line}");
            ExitCode::from(1)
        }
        // …and the vendor's channel not reached gets the one action the index lane's
        // offline ending has always had: re-run when the connection is back — under the
        // verb the person typed (audit).
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

/// `installed trust 9192 — 30 commands on your PATH (trustc, targo, tippy, …)`: what a
/// person needs from an install — the program, its build, and what they can now type —
/// never the full list of names laid (thirty for trust), and never the `alab-` aliases,
/// which are the same commands again. The program's own name leads when it is one of
/// them. Pure for the test.
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

/// The index-program rollback verdict, pure: what moved, what the next update does, and
/// the one hold — which for a coherence-group member holds the whole group, said as a
/// fact in the same line rather than as a stderr warning with a second, contradictory
/// remedy. `held_by` names the pins already
/// holding it ([`crate::flow::RollbackReport::held_by`]): then no update moves it, and the
/// line names the unpin instead. `revoked` is a group member's revoked current build
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

/// `atpkg rollback <program>` — re-point the program (and its shims + channel `current`) to
/// the highest RETAINED build strictly below current that still passes the floor/yank gate.
/// Consults the SIGNED index (so the gate is authoritative); advances the durable floor to
/// the index it trusted. A coherence-group member's line says the pin holds the whole
/// group (a per-program rollback splits the locked tuple until the next update). A
/// vendor-direct program never reaches the index: see [`cmd_rollback_vendor`].
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
            // Advance both durable ratchets to what we just trusted (§8 gate 3).
            advance_floors(layout, r.index_build, r.roster_seq);
            record_status(
                layout,
                program,
                crate::ProgramStatus {
                    installed_build: Some(r.to_build),
                    // What happened, not a pin the index does not hold: the index still
                    // pins the build just left, and `which` prints this row's words.
                    state: crate::state::rolled_back(r.to_build, r.from_build),
                    // The rolled-back build's signed root is not carried in the rollback
                    // report, so it is written unset here and re-derived immediately below.
                    // Carrying the PREVIOUS build's root instead would be far worse than
                    // leaving it empty: it would attest bytes that are no longer there.
                    tree_root: String::new(),
                },
                format!("rolled back {program} {} -> {}", r.from_build, r.to_build),
            );
            // CLOSE THE WINDOW WHERE IT OPENS: otherwise only a whole-set `update` would
            // recover the root, and `atpkg verify` would fail closed on a good rollback
            // until then. The fetcher is in hand and the index it just trusted is the
            // right authority to ask.
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
                // The rollback's own shim lay already laid the target build's root and
                // routed what it rendered; this is the pass-level reconcile every verb that
                // moves trust ends with — a trust shim that lay did not render is routed,
                // and a root that could not be laid is said. The rolled-off build stays on
                // disk, and keeps its root, until gc takes both.
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

/// `atpkg rollback <vendor program>` (design §1.3): back to the retained previous build
/// that no signed yank names — a vendor build, else the legacy index build the first
/// vendor install replaced ([`crate::vendor_direct::lane::roll_back_by_hand`]) — with no
/// index resolve: the index pins nothing here. Its row says why the vendor's head is not
/// what runs, and the next update returns it to the head unless it is pinned.
fn cmd_rollback_vendor(
    layout: &crate::store::Layout,
    spec: &'static crate::vendor_direct::VendorSpec,
) -> ExitCode {
    let program = spec.program;
    let policy = crate::vendor_direct::policy::Policy::current(layout, None);
    match crate::vendor_direct::lane::roll_back_by_hand(layout, &policy, spec) {
        Ok(moved) => {
            let build = moved.to_build;
            // A vendor build's root is its `.vendor` record's; a legacy build's is the
            // recorded row's when that row is for the same build, else the one its stamp
            // kept when a vendor install replaced that row.
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

/// The `pin` / `unpin` verdict, pure: the program and the build the pin holds or the
/// unpin releases (`ay build 8256`, `claude 2.1.283` — `list`'s own spelling,
/// [`crate::vendor_direct::display_build`]), the state, and the one thing to know. A pin
/// holds a build against `update`; the one exception every lane implements — a yanked
/// or below-floor build is replaced regardless — is stated as a fact, not a second
/// remedy. An unpin is the end of the hold and nothing more: whether the next update
/// moves anything depends on the vendor's head or the index pin, which this verb does
/// not read, so it names the check instead of predicting its answer.
///
/// A coherence-group member's hold is the GROUP's: one pinned member freezes the whole
/// tuple, so while `peers` names other pinned members the line names every unpin it takes
/// — and without the group (`PinPeers::Unknown`) a verb that changed nothing predicts
/// nothing.
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

/// `program`'s coherence group and the OTHER members whose pins hold it, read offline from
/// the cached index as the update pass's group gate reads them
/// ([`crate::flow::pin_gate_members`]); `None` when no index is cached. `active` is the
/// store's active builds.
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
/// `update`. A pin is purely LOCAL upgrade-suppression state (no index/network); it is
/// consulted strictly AFTER the floor/yank gate, so it can never resurrect a tombstoned or
/// below-floor build.
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
/// `--retry` FORGETS THE DIGEST-REFUSAL MEMOS FIRST — every program's, or the named
/// program's — and then runs the pass exactly as it always did. A held row (a `tree_root`
/// mismatch, [`crate::store::StageRefusal::held`]) never lapses on its own, so a person
/// who believes the publish is fixed, or who simply wants the pass to try again now,
/// needs a door that is not "install one program by name"; this is it. The explicit
/// `install <p>` door clears the same memos ([`do_install_with`]) — the store's
/// `<build>.refused` memo and, for a vendor-direct program, the refusal its stamp keeps
/// ([`forget_vendor_refusal`]) — so the two spellings agree on what "try again" means.
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

/// Forget the stage refusal a vendor-direct `program`'s stamp records
/// ([`crate::vendor_direct::ProgramStamp`]), as the install door does: whether one was
/// recorded and is now gone. `false` for a program that is not vendor-direct.
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
/// current builds (surfaced as a pin-hold). Under the install consent
/// (`[packages].auto_install`, default TRUE) the pass ALSO installs missing index default-set
/// members (§11 batteries-included, [`install_default_set_with_path`],
/// [`should_complete_set`]) — the loop verb fills an empty store instead of no-opping
/// forever. GC runs once after the whole apply.
fn cmd_update_all() -> ExitCode {
    // The pass's own verdict, then the contract's other half: `install_default_set_with_path`
    // may have announced a network install from inside this verb, and only the verb
    // knows how it ended. A lane that already printed its terminal is unaffected.
    let code = cmd_update_all_code();
    answer_announcement(code == 0);
    ExitCode::from(code)
}

/// The `*index*` row after `update`'s set-completion lane, returning whether the pass
/// has now resolved the signed index. On an EMPTY store that lane IS the pass — the
/// apply lane, the row's other writer, never runs — so its unresolved index is recorded
/// exactly as the apply lane records one (and is not a check: see [`record_pass_end`]),
/// and its resolved index retires the row the same way. When the apply lane already
/// resolved this pass, a set-completion miss neither un-resolves it nor re-opens the row
/// the apply lane just cleared.
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

/// The update pass's ONE failure exit: say it, name the next act, and record the durable
/// `*index*` row. Shared by the pass's index resolve and its channel apply — which used
/// to be a single call, the resolve living inside `apply_channel` — so both halves still
/// print the same sentence and leave the same row.
fn update_pass_failed(layout: &crate::store::Layout, e: &crate::FlowError) -> u8 {
    eprintln!("atpkg: update failed: {e}");
    // Same follow-up the single-program lane prints: the old Display carried a false
    // "retries automatically" tail, and dropping it without a replacement left this lane
    // stating a network failure with no next act.
    print_unreachable_followup(e, "aterm pkg update");
    record_index_error(layout, e);
    1
}

/// Finish every ACTIVATION an interrupted pass left half done, before anything decides on
/// the store: activation is the `current` flip, then the shims, and a kill between the two
/// left a complete build no shim runs — the next pass read the shims, decided Install and
/// fetched it again (gc abstained on the divergence meanwhile). Such a program gets its
/// shims laid from that build and its `.shim-env` sidecar — no stage, no fetch — and its
/// row names that build with no signed root, which the pass's root recovery re-derives
/// (the row it had named the old build's root, which `verify` would compare the new tree
/// against).
///
/// Never for a program the machine removed, declined, dev-linked, tombstoned or pinned, for
/// a build the latest verified index yanked or floored (the cached index's channel; a
/// vendor build, the yank latch), for a sysroot bundle whose compilers do not load (the
/// install's resolve check, run before a shim is laid), and never for a COHERENCE-GROUP
/// member: a group flips member by member, so a member's `current` ahead of its shims may
/// be the one flip of a tuple whose siblings never flipped, and rolling it forward alone
/// would split the tuple the shims still hold whole — the group's own transaction re-flips
/// it from the staged builds. Membership comes from `cached` (the §14 cached index), read
/// only when a candidate needs it; with none cached only the vendor programs, which are
/// never grouped, roll forward. Returns the programs rolled forward.
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
        // A SYSROOT BUNDLE IS CHECKED FIRST, as its install checks it: compilers whose
        // libraries do not load are never put on PATH — the pass decides it afresh.
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

/// A protected local toolset is provisioned without registry build numbers.
/// Record broken links as faults, never as an empty/unsupported architecture.
/// The updater and doctor deliberately use the same executable/shim predicate.
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
    // (The `[machine]` settings are not this pass's: they are applied by their own verb,
    // and at the dispatch edge only when the table changed — `machine_table_changed`.)
    if !manager_enabled() {
        eprintln!("atpkg: disabled (no root key pinned) — nothing to update");
        return 1;
    }
    let Some(layout) = layout() else {
        return 1;
    };
    // WHATEVER THIS PASS ENDS, ITS PROMISE ENDS WITH IT ([`crate::progress::pass_owed`]): the
    // update a seed promised is this one, and once it returns nothing more is coming — a
    // stub waiting on that promise answers at once. Its promise only: the one it wrote, or the
    // seed's of the window that spawned it, never another window's seed's for the update that
    // window spawns next ([`PromiseEnds`]).
    let _promise = PromiseEnds(&layout);
    // THE HOLDER DID THIS PASS: a child that queued behind another full pass and finds its
    // end recorded since it started waiting does not repeat it back to back, whatever
    // that pass ended.
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
    // Reconcile `[packages.links]` first, so a config-declared dev-link is in place
    // BEFORE the apply decides what it manages (linked programs hard-skip, §13).
    reconcile_links(&layout, cfg);
    let Ok(dev_linked) = update_link_inventory(&layout) else {
        return 1;
    };
    // The ACTIVE build per program (what the shims point at), NOT the max complete build
    // on disk — so `decide` never treats a staged-but-unactivated build as the running one
    // (which would silently skip a needed re-flip, #19).
    // THE SHIM VIEW, deliberately. Its SILENCE for a tombstoned program is the
    // self-heal: `decide` then returns Install for the build `store/<p>/current`
    // already names, the transaction re-stages it (`Staged::was_live` exists for
    // exactly that member), and the shims come back. Widening this to include the
    // authority link would remove that silence: a tombstoned program whose pin had not
    // moved would decide UpToDate forever.
    let installed: std::collections::BTreeMap<String, u64> = crate::active_builds(&layout);
    let complete_the_set = completes_the_set(&layout, cfg);
    if !has_work(&installed, &dev_linked, complete_the_set, &layout) {
        println!("{EMPTY_UPDATE}");
        // A full pass with nothing to check still ENDED, and says so: the schedulers count
        // their walk from it. Unrecorded, every tab's session lane owed it again five
        // minutes on.
        let _ = crate::status::stamp_pass_end(
            &layout,
            &now_rfc3339(),
            aterm_update_core::pkg_check::PassOutcome::Ok,
        );
        return 0;
    }
    // A pass that will plan the set PROMISES ITS OWN PLAN: the index resolve, the vendor lane
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
/// ([`cmd_update_all_code`]): only on a FIRST install ([`crate::adoption::first_install_ahead`]),
/// or over a promise it finds at `now_unix` — the window seed's, which it takes over. A
/// routine pass's stub is a program held, unserved here or failed last time, and its
/// recorded reason answers at once ([`pending_not_running`]) rather than after a wait for a
/// plan that leaves it out.
fn update_promises_its_plan(layout: &crate::store::Layout, plans: bool, now_unix: u64) -> bool {
    plans
        && (crate::adoption::first_install_ahead(layout)
            || crate::progress::pass_owed(layout, now_unix))
}

/// Retires the promise of a pass when the whole update holding it returns, by any path
/// ([`cmd_update_all_code`]): a promise this process wrote, or one its spawner holds — the
/// seed's of the window that spawned this update ([`seed_promise_owner`]) — and no other
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

/// WHO gets set-completion: a machine that has ADOPTED the toolset (the seed bootstrap or an
/// explicit "Install ALab toolset" already laid the whole set down) under the one install
/// consent (`[packages].auto_install`, default TRUE) with automatic updates on — so a
/// program published AFTER a user installed reaches them, instead of their "entire ALab
/// toolchain" quietly decaying into "whatever was published the day they installed".
/// `declined` outranks it all. It is checked in the pass the loop runs, not only in the seed
/// lane: honouring a removal on the local lane while the network lane quietly reinstalled
/// the whole set on the next tick made the decline look like it worked for exactly one
/// launch.
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

/// Whether an `update` pass has anything to do: a program is managed (`installed`, the shim
/// view) or dev-linked (`linked`, [`update_link_inventory`] — a protected local toolset is
/// the pass's to keep), the set is being completed, or a vendor program is tombstoned in
/// place — silent in the shim view, and its lane still revisits it, since a later head may
/// be admissible. Anything else is [`EMPTY_UPDATE`], with no network request.
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

/// The long-lived window/session index workers need the CURRENT install consent, not
/// [`crate::config::cached`]'s one-time CLI snapshot. An empty store may become eligible
/// after the user enables Automatic updates or adopts the set without restarting aterm.
/// A metadata stamp keeps the frequent empty-store check from reparsing an unchanged file.
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
            // A chmod or privacy-consent change need not move mtime or length.
            // Opening is cheap; parsing remains cached. A failed read must not
            // keep an earlier config's unattended-install permission alive.
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

/// [`has_work`] for the store as it stands, under the LIVE `[packages]` table — what
/// the next-index probe asks before it spends a HEAD (`index_probe::successor`, audit PK-7):
/// a published index can wake nothing on a store whose `update` would answer
/// [`EMPTY_UPDATE`]. Read-only: the dev links are read, never reconciled. A probe
/// asks this every five seconds while the store is empty, so do not enumerate
/// the shim and dev-link trees once the cheap set-completion predicate answers yes.
pub(crate) fn update_pass_has_work(layout: &crate::store::Layout) -> bool {
    static PROBE_CONFIG: std::sync::OnceLock<std::sync::Mutex<ProbePackagesConfig>> =
        std::sync::OnceLock::new();
    let cfg = PROBE_CONFIG.get_or_init(|| {
        // `poll` reads the live file on its first call. Seeding from the
        // short-lived CLI cache only adds a second lazy initializer here.
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

/// The probe only needs a yes/no answer, unlike an update pass, which needs the
/// complete inventories and link diagnostics. Preserve the pass's [`has_work`]
/// decision while skipping progressively more expensive read-only scans as soon
/// as any sufficient condition holds.
fn probe_has_work_lazily(
    complete_the_set: bool,
    managed: impl FnOnce() -> bool,
    tombstoned_vendor: impl FnOnce() -> bool,
    linked: impl FnOnce() -> bool,
) -> bool {
    complete_the_set || managed() || tombstoned_vendor() || linked()
}

/// [`run_update_pass`], then its RECORD for the schedulers
/// ([`crate::status::stamp_pass_end`]): `last_pass` is `ok` when the pass stamped a success
/// (it reached the signed index), `offline` or `failed` by its exit, bound to the index this
/// pass was launched for or resolved. What the window's gate and the session lane read of a
/// pass, instead of `updated_at` (every write's) — the derived models `AtpkgPassStamps` and
/// `AtpkgFullPassRule` state the pair.
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

/// A hint names the build this pass was launched to check; a signed selection
/// may be newer. A stale selected index must not lower the hint and permit a
/// repeat of the same requested build. No actual index request means unknown.
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

/// [`crate::hooks::refresh`] as a pass runs it: best-effort, its report unsaid.
fn refresh_shell_hooks(layout: &crate::store::Layout) {
    let _ = crate::hooks::refresh(layout);
}

/// Whether a vendor program is installed here but disabled by a tombstone
/// ([`crate::flow::tombstoned_in_place`]).
fn vendor_tombstoned_in_place(layout: &crate::store::Layout) -> bool {
    crate::flow::tombstoned_in_place(
        layout,
        crate::vendor_direct::VENDORS.iter().map(|s| s.program),
    )
}

/// The index lane could not run: settle what the vendor lane already moved this pass —
/// the gc that keeps live + 1 over the vendor programs alone (their partials spared) and
/// the shell hooks, as `update <vendor>` does ([`settle_vendor_pass`]) — say which agents
/// it left current, then record the failed pass.
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
    // The index source and every vendor channel unreached: offline, not failed. `Stale`
    // counts as the network's too when nothing answered — the cached index aged out while
    // the machine was off it; a stale index a reached host served is the publisher's, and a
    // failure. Either way a pass that could not apply an index is not a check that ran.
    let offline = matches!(
        e,
        crate::FlowError::Unreachable(_) | crate::FlowError::Stale
    ) && pass_reached_nothing(index_answered, vendor);
    finish_update_pass(layout, (seams.now)(), false, offline, code)
}

/// Whether a pass reached NOTHING: no host answered its index resolve (`index_answered`
/// false — the link itself failed, [`crate::flow::ResolveProvenance::answered`]; a host that
/// answered with a refusal is a host that answered) and no vendor's channel answered either.
/// Such a pass stamps a failure and exits
/// [`aterm_update_core::pkg_check::PASS_OFFLINE_EXIT`], which the window retries quietly
/// and soon; one that reached either is an ordinary pass.
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
    // Whether EITHER lane of this pass resolved the signed index (from the channel or the
    // §14 cache): the pass-end build stamp's condition. The apply lane exits early when it
    // cannot resolve; the set-completion lane does not, and on an empty store it is the
    // whole pass, so a failure count alone cannot say which kind of pass this was.
    let mut index_resolved = false;
    // What this pass's resolves VERIFIED from the channel itself, counted from here: the
    // one fact its success stamp may record ([`finish_update_pass`]).
    let live_before = crate::flow::live_verifications();
    // The index build the channel apply resolved, for the pass-end freshness stamp.
    let mut resolved_index_build: Option<u64> = None;
    // `program → pinned asset name` for everything EITHER lane of this pass resolved —
    // the pass-end gc's sparing input, so a member whose download failed mid-pass keeps
    // its `.part` resume state instead of losing it to this pass's own closing sweep.
    let mut resolved_assets: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    // The ONE index this pass verifies, once a lane has resolved it — handed to every
    // lane below instead of being re-resolved by each ([`reusable_index`]).
    let mut pass_index: Option<crate::TrustedIndex> = None;
    // THE PASS'S ONE VERIFY-SELECT, FIRST — and nothing waits on it. Hoisted out of
    // `apply_channel` so the lanes below read the index it trusted rather than resolve it
    // again: the attestation repair, the alias reconcile, the set completion and the
    // empty-store diagnosis would each re-run the cache read, roster admit and signature
    // checks for the same answer. Asked
    // BEFORE the vendor lane because a fresh verification refreshes the yank latch (design
    // §1.4): the vendor lane then binds the yanks published since the last pass, not one
    // or two passes late. A failed resolve blocks nothing: the vendor lane runs on the
    // latch, and only then does the index lane stop.
    // Whether this pass has index work at all: a pass that asked no index (a store holding
    // only a vendor program tombstoned in place) records nothing about one.
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
    // Record the signed index as soon as selection returns, before the vendor
    // lane or apply can fail early. A later Published hint for this SAME build
    // must not mistake an apply failure for an attempt at only the older target.
    if let Some(Ok(index)) = resolved.as_ref() {
        RESOLVED_PASS_INDEX_BUILD.with(|seen| seen.set(seen.get().max(index.index_build)));
    }
    // Whether a host ANSWERED that resolve, read before any other resolve in this pass can
    // overwrite the measurement: a refusal (a rate limit, a 5xx, a portal's page) is not
    // offline. No resolve (nothing to do) is not an offline pass.
    let provenance = resolved.as_ref().and(crate::flow::last_resolve());
    let mut index_answered = resolved.is_none() || provenance.as_ref().is_some_and(|r| r.answered);
    // The members this pass loses AT A FETCH ([`crate::flow::note_fetch_fault`]): an
    // unreached pass is offline only when every failure it counts is one of those.
    let faults_before = crate::flow::fetch_faults();
    // A published-index hint means the window has already seen a newer signed build.
    // Once THIS pass verifies that build live and refreshes the yank latch, activate its
    // installed members before unrelated vendor network work. A cached fallback, stale
    // build, or ordinary pass retains design §1.5's vendor-first order. Keep an early
    // apply's result until after the vendor lane so even its error cannot skip vendors,
    // and the pass-end reporting and retry semantics remain in one place below.
    //
    // The ONE flip gate of this pass, for both lanes that can move the toolchain: the apply
    // lane and the set completion below.
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
    // Ordinarily the vendor lane goes first (design §1.5): claude and codex move with
    // their vendors even when the index is stale or unreachable. The hinted path above
    // only gives already-published index members priority; vendors still run every pass.
    // The vendor lane's downloads under a person's meter too ([`TypedPass`]).
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
        // Advance the durable anti-rollback floor to the index we just trusted (§8 gate 3).
        advance_floors(layout, report.index_build, report.roster_seq);
        // The build this pass resolved, for the freshness stamp at the pass end (the
        // `report` name is taken by the gc report there).
        resolved_index_build = Some(report.index_build);
        // What the apply resolved, kept for the pass-end gc's sparing closure — a member
        // whose group aborted on a failed download is in here, and is the reason this
        // exists (its `.part` is the resume state the next pass continues from).
        resolved_assets.extend(report.resolved_assets.clone());
        // AND CLEAR THE FAILURE THIS PASS JUST DISPROVED. The `*index*` row is
        // written when a resolve fails and was never removed by anything, so a
        // single transient network error — a hotel wifi captive portal, a laptop
        // waking mid-poll — left `atpkg doctor` and Settings ▸ Packages reporting a
        // broken toolset forever, on a machine that had been updating cleanly every
        // six hours since.
        clear_status_row(layout, "*index*");
        // REPAIR A MEMBER THAT WENT LIVE WITHOUT ITS ATTESTATION. A pass killed
        // inside the flip window leaves the program installed and working but with
        // no recorded signed root, and `atpkg verify` fails closed on that forever
        // while `seed` says "fully installed" and `update` says "up to date" — so
        // nothing repaired it. Re-derive the root from the same authority the
        // install used, never a local recomputation.
        recover_missing_roots(layout, fetcher, Some(&index));
        // …and the SENTENCE that row left behind. `status.outcome` is what Settings
        // ▸ Packages prints as its detail line, so clearing the row while leaving
        // "update failed: …" in place swaps one stale verdict for a quieter one.
        // …and a vendor refusal this pass stays its headline: a pass that exits 1 only for
        // the vendor lane is never "up to date".
        record_outcome(
            layout,
            vendor_folded_outcome(
                &vendor,
                format!("up to date (index build {})", report.index_build),
            ),
        );
        // ADDED to the vendor lane's faults, never over them: an overwrite here turned a
        // vendor refusal into a green pass whenever the index resolved.
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
        // The set-completion lane below runs its own alias + shadow reconcile at its
        // end; a pass that only applies runs them here, so every pass lays the aliases
        // of what it keeps (offline, from the index this pass just verified and cached)
        // and says who is SHADOWED.
        if !complete_the_set {
            // From the index THIS pass verified. `cached_index` re-read
            // `index-cache.toml` and re-ran the whole roster admit + signature check to
            // answer a question the pass already had the answer to — and it skips the
            // freshness gate this index has passed.
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
    // §11 batteries-included: install the index default-set members not yet installed
    // (exclude-narrowed; linked/yanked members skip; per-program failures are loud
    // but never block the rest). This is what keeps a machine's toolset COMPLETE as the
    // suite grows, and what fills an empty store under the install consent.
    if complete_the_set {
        // ANNOUNCE BEFORE ACTING. This pass pulls GIGABYTES over the network — it is how
        // every machine provisions — and it once ran completely silently: the GUI spawns
        // it with stdout discarded, so a user could watch multiple GB arrive with nothing
        // on screen ever mentioning it. The marker contract exists to prevent that.
        let before_net = crate::active_builds(layout);
        // The Trust toolchain is usable the moment its tuple flips: the rustup view and the
        // exec roots follow it there, not when the rest of the pass is done (the pass end
        // re-lays both, idempotently, as it always has).
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
        // An empty store may retry after the first resolve failed. Judge the pass by
        // that retry's actual network answer: a pass whose retry reached a host is not an
        // offline pass. (Whether it REACHED the index is the live verifications' count,
        // which the retry moves like any resolve — [`finish_update_pass`].)
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
        } else if announced {
            // ALWAYS ANSWER THE ANNOUNCEMENT (the seed lane's law): a pass that
            // said "installing over the network" and then installed nothing
            // must say so, or the held notice outlives its own truth — a failure, unless
            // everything it announced was a vendor landing waiting on its signature check.
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
    //
    // Under the channel this pass VERIFIED: the build it keeps below each live one is the
    // build `aterm pkg rollback` lands on. Kept by its number alone, a yanked build just
    // below the live one was the "rollback" every pass preserved while the build the verb
    // lands on was reclaimed, and the verb then refused on a store that had one.
    let channel = pass_index
        .as_ref()
        .and_then(|index| index.channel_for(crate::config::CHANNEL, current_triple()));
    let report = crate::gc::run_keeping_pinned_partials(layout, channel.as_ref(), &|p| {
        resolved_assets.get(p).cloned()
    });
    print_gc_sweeps("update", &report);
    print_gc_abstentions("update", &report);
    // THE RUSTUP SEAM, once per pass, whichever lanes ran — or a trust flip either landed
    // leaves `~/.rustup/toolchains/trust` presenting the build it moved off.
    (seams.rustup_seam)(layout);
    // THE EXEC ROOTS, over the store this pass leaves (after its gc, so a build it just
    // reclaimed is not given a root first). This is what heals a machine that is ALREADY
    // up to date on a trust build whose tippy refuses its own `bin/rustc` copy: the apply
    // short-circuits that program before any shim is laid, so without this no pass would
    // ever route its shims. Best-effort like the gc above; a root that cannot be laid is
    // said on stderr and `aterm pkg repair` names it as undone.
    let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
    // Refresh the interactive-shell PATH hook at the CLI edge (§16), best-effort.
    (seams.shell_hooks)(layout);
    // R6: which managed agents are at their vendor's latest, for the window's ledger — a
    // fact about the vendor lane, so said whatever the index lane's members did.
    print_managed_current(layout, &vendor.unchecked, (seams.now)());
    let now = (seams.now)();
    if index_resolved {
        record_index_freshness(layout, now, resolved_index_build);
    }
    // OFFLINE when nothing answered and every member this pass lost was lost AT A FETCH; a
    // full disk or a refused stage is a failure, whatever the network did.
    let offline = pass_reached_nothing(index_answered, &vendor)
        && crate::flow::fetch_faults().wrapping_sub(faults_before) == failures;
    // The development links, re-read after the network work: a checkout may have been
    // rebuilt or removed while the pass ran, and a marker-only toolset is not a healthy one.
    // A broken link is a fault of this pass, recorded in its own row.
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
        // A PASS THAT INSTALLED NOTHING ON AN EMPTY STORE IS NOT A SUCCESS TO SHOW.
        // Every group clean-skips when the index publishes no artifact for this
        // triple, so `failures` stays zero and the verb exited 0 with nothing
        // installed and no status written — and the Settings button that runs it
        // reported a green "toolchain up to date" on a machine where the toolchain
        // can never arrive. Say the true thing instead, durably.
        //
        // STDERR AND EXIT 2, matching `cmd_install_default_set`. The GUI keeps
        // stderr and discards stdout, and it maps 2 to the honest "not a
        // temporary error" sentence while 1 is a retryable failure — so printing the
        // reason on stdout and exiting 1 produced exactly the opaque message these
        // fixes set out to kill. Exit 2 only for a pass
        // that REACHED the index: one that did not is a failure to retry
        // ([`finish_update_pass`]).
        //
        // DO NOT BLAME THE CPU FOR EVERY EMPTY STORE. "Nothing installed and
        // nothing installed this pass" has more than one cause: a triple the
        // index does not serve, a translated process reporting a triple that is
        // not this Mac's, a channel that revoked everything, and the user's own
        // `[packages].exclude` or uninstalls. This arm once asserted the
        // architecture answer for all of them; it asks
        // `running_translated` first, then `index_serves`, which tells the rest apart.
        if (seams.translated)() {
            let said = format!(
                "nothing is installed — this process is running under \
                 Rosetta translation, so {} is not this Mac's real architecture. \
                 Relaunch aterm natively and the toolset installs.",
                current_triple()
            );
            eprintln!("atpkg: {said}");
            // ITS OWN ROW, over whatever an earlier pass left: the window judges an
            // exit 2 by this row, and an older pass's "index unreachable" read as this
            // one's put a Rosetta refusal on the failure ladder.
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
        // `unusable`: the `seed-unusable:` body, for the two verdicts the seed's own line
        // used to carry ([`report_seedless_posture`]).
        let (said, state, unusable) = match &serves_us {
            Some(IndexServes::Yes) => {
                // The index DOES publish for this triple, so something else refused
                // every member — a revocation, a floor, a filter. Saying "no build
                // for your architecture" here sends the user to fix the wrong thing.
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
            // `None` = the index never RESOLVED — an offline machine, a rate
            // limit, a proxy — never a machine that cannot be served (audit-2 item 6).
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
        // THE WINDOW'S CARD, from this pass: the window's seed no longer resolves the
        // index, so this arm prints the very line it printed — an unserved architecture,
        // a toolset narrowed to nothing — for the reader that parses every pass's stdout.
        // A person's terminal has the sentence above already.
        if let Some(why) = unusable
            && !stdout_is_a_person()
        {
            emit_marker_line(&seed_unusable_line(&why));
        }
        record_unbuilt(layout, "*toolset*", state, said);
        // Reached — this pass verified an index the channel served, the diagnosis's own
        // resolve included — is a verdict no retry changes (exit 2). Not reached, it is a
        // first launch before the network came up, or a refusal: retried, never "not a
        // temporary error".
        let reached = crate::flow::live_verifications() > live_before;
        return finish_update_pass(layout, now, reached, offline, 2);
    }
    let reached = crate::flow::live_verifications() > live_before;
    let code = u8::from(failures > 0);
    // A development-linked toolset the pass kept is said whatever else it did — a
    // dev-link-only store asks no index at all, and its pass still ends in this sentence
    // rather than in silence (`dev_linked` is empty unless the pass is clean).
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
        // The toolset is present: retire any "unavailable" verdict an earlier pass
        // recorded — a machine whose triple the index did not serve yet reaches the
        // toolset through THIS lane once artifacts are published.
        clear_status_row(layout, "*toolset*");
    }
    // A pass that resolved the index and RAN TO ITS END with member failures — each
    // recorded in its own row — is a check that ran, and is stamped as one when the index
    // it resolved was the channel's. A pass on the cached index, or with no index at all,
    // is not: it stamps a failure and exits non-zero whatever its members did (audit
    // PK-3), so a listing that refused (a rate limit, a renamed repo) never hides behind
    // exit 0.
    finish_update_pass(layout, now, reached, offline, code)
}

/// §11 batteries-included bootstrap: install every SIGNED-index default-set member
/// (narrowed by the narrowing-only `[packages].exclude`) that is not yet
/// installed. Shared by `update`'s set-completion arm ([`should_complete_set`]) and the explicit
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
    /// How many programs the announcement named, the vendors that rode it included.
    announced_count: usize,
    /// The network verdict of this lane's own index resolve, when it could not
    /// reuse the caller's index. Captured before member installs can replace the
    /// thread-local provenance; `update` uses it after a failed first resolve.
    resolve_provenance: Option<crate::flow::ResolveProvenance>,
    /// `Some(reason)` when the pass never reached a signed index it could act on — the
    /// index did not resolve (offline, a rate limit, a proxy) or names no channel for
    /// this machine, the two early exits the apply lane reports as its own `*index*`
    /// failure — so it checked NOTHING; `None` once it did. `update` reads this: on an
    /// empty store this lane is the whole pass, and an unresolved index must never be
    /// stamped as a completed check (`last_success_at`).
    index_error: Option<String>,
    /// The signed build this lane used, including a successful retry after the
    /// caller's first resolve failed. The full-pass end records the highest
    /// resolved build so a queued pass for that same build cannot run again.
    resolved_index_build: Option<u64>,
    /// `program → pinned asset name` for every member the pass resolved far enough to
    /// select an artifact — a member whose download then FAILED included. GC runs at the
    /// CALLER's edge (the `cmd_update_all` precedent), so this rides the outcome out to
    /// where the pass-end `gc::run_keeping_pinned_partials` builds its sparing closure.
    resolved_assets: std::collections::BTreeMap<String, String>,
    /// Members the pass REFUSED because their coherence tuple was narrowed (a
    /// config `exclude`, or a member removed on purpose) — sorted. They are
    /// deliberately absent from the announcement and the plan; the caller's
    /// summary needs them so "default set complete" cannot claim members the
    /// pass just declined to install.
    skipped_narrowed: Vec<String>,
    /// `wanted ∧ absent` at pass end — what a CLEAN pass proved still awaits
    /// install here (failed members included; narrowed/unserved members not).
    /// The `*seed*` pending-consent row reconciles against this, so a row
    /// written in one era cannot keep advertising another era's toolset.
    remaining_missing: Vec<String>,
}

/// The R5 progress-file pass name every default-set pass runs under — the ONE naming
/// authority, so the writer sites cannot drift from each other (the GUI reads it back from
/// the progress file). There is one lane: the network.
const NET_PASS: &str = "net";

/// Who consented to a default-set pass — named by the caller, because the wire lane's
/// announcement states the consent and the switches that stop the pass, and those differ
/// per caller. [`NET_OFF_SWITCH`] is true of every pass; the two clauses
/// [`Consent::announced`] adds around it only of a pass nobody asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Consent {
    /// Nobody asked: the pass runs because installing aterm is wanting the toolset (the one
    /// install consent, `[packages].auto_install`, default TRUE). The line names both that
    /// consent and `[packages].auto_install = false`, the switch that would have stopped it.
    InstallingAterm,
    /// No consent claimed beyond the one every pass carries: the line names the uninstall
    /// alone. The explicit verb's own pass (running it is the consent, and it never reads
    /// `auto_install`), and every `update` pass on a machine a person adopted with that verb
    /// ([`set_completion_consent`]).
    Explicit,
}

impl Consent {
    /// The clauses the wire lane's announcement wraps around [`NET_OFF_SWITCH`]: the consent
    /// ahead of it and the switch that would have prevented the pass after it, each only where
    /// it is true. Parenthesis-free, as that phrase is, for the toolchain bar's sake.
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

/// The consent `cmd_update_all`'s set-completion pass states — pure, so the choice is pinned
/// by a test. The pass runs only under the one install consent on an adopted machine
/// ([`should_complete_set`]), so "installing aterm is the consent" holds wherever the seed
/// pass adopted it; a person who adopted the set with the explicit verb ([`adopted`] without
/// [`adopted_by_seed`]) gave their own consent, and its passes get [`Consent::Explicit`],
/// naming the decline alone.
fn set_completion_consent(adopted: bool, adopted_by_seed: bool) -> Consent {
    if adopted && !adopted_by_seed {
        Consent::Explicit
    } else {
        Consent::InstallingAterm
    }
}

/// The off switch every wire-lane announcement names, whoever consented ([`Consent`]). Kept
/// free of parentheses, as the clauses around it are: the GUI's toolchain bar reads the sizes
/// out of the line's trailing `(…)`, which must stay the last one on the line.
///
/// The timing is stated because `uninstall` is a store mutator like the pass itself and the
/// dispatch edge holds the writer lock throughout (`mutator_store_lock`), so a bare "opt out
/// with" would send the reader into "another atpkg process holds the store lock" mid-download.
const NET_OFF_SWITCH: &str = "opt out once this pass ends with `aterm pkg uninstall --all`, which \
                              removes the set and records the decline";

/// The wire lane's announcement, if the pass owes one — pure, so its words stay pinned
/// by a test: only a non-empty pass speaks (the ordinary no-op tick stays silent).
///
/// Names the signed sizes — what comes over the wire and what stays on disk
/// ([`seed_install_bytes`], rendered by [`size_parenthetical`]) — and the off switch
/// ([`NET_OFF_SWITCH`]), with the consent ahead of it and the `auto_install` switch after it
/// only where `consent` makes them true ([`Consent::announced`]).
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
    // Live-progress pass ownership (R5): when the GUI opted in via `--progress-file`
    // (or a person typed the pass on a terminal) and no pass is live yet, this pass IS
    // the "net" pass — begun here so its start truncates the file, ended here so the
    // terminal snapshot (pid cleared, `ended_unix` stamped) is written on every exit
    // path. When a pass is already live, `begin_pass` declines and this one runs under
    // it. A vendor lane riding this pass meters on it too ([`crate::progress::lend`]).
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
    // THE CALLER'S INDEX WHEN IT HAS ONE (`atpkg update`'s pass resolved one for its
    // apply lane), else one resolved here (`install --default-set`, the seed lane).
    // Re-using it skips a repeat of the whole verify-select — the cache read + TOML
    // parse, the roster admit, the master and machine signature checks, the
    // roster-generation observation — for an answer that cannot differ inside one pass.
    // It skips no GATE: [`reusable_index`] re-checks the signed freshness window, and an
    // index that lapsed mid-pass falls through to the resolve below and meets `Stale`
    // there, exactly as it always did.
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
    // The channel, looked up ONCE on the one resolved index — every group decision
    // below reads the SAME signed state, so a mid-pass publish can't split a tuple.
    let Some(ch) = index.channel_for(crate::config::CHANNEL, current_triple()) else {
        let e = crate::FlowError::NoChannel(crate::config::CHANNEL.to_string());
        eprintln!("atpkg: default-set bootstrap failed: {e}");
        // NOTHING OF THE SET IS COMING (an Intel Mac before x86_64 lands): this pass plans
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
    // their stubs and rows stay here, and their own lane installs them (design §1.5). One
    // whose vendor serves this machine nothing is not: its lane recorded its `unavailable
    // on` row, and no stub may promise it.
    for spec in crate::vendor_direct::VENDORS {
        if spec.serves(current_triple())
            && vendor_wanted(cfg, &std::collections::BTreeSet::new(), spec.program)
        {
            wanted.insert(spec.program.to_string());
        }
    }
    // Programs the user removed on purpose are not "missing" — they are absent by
    // request. Filtering them ONLY in the seed lane's prescan was not enough: THIS
    // function is what the 6-hour pass runs, so an adopted machine reinstalled an
    // uninstalled program on the next tick regardless. Set-completion means keeping
    // the set the user has, not overruling their removals.
    for p in removed_programs(layout) {
        wanted.remove(&p);
    }
    // SYSTEM SATISFACTION, reconciled EVERY pass (owner decision for `gh`/`emacs`): a
    // member declaring `system = "<bin>"` whose binary is already on PATH outside the
    // managed prefix is satisfied — no download, no shim, a `satisfied by system`
    // status row — and a managed copy it may have had is retired so the system binary
    // is the only one. If the system install later disappears the name simply stays
    // wanted-and-missing here and installs through its artifact like any other member.
    for p in reconcile_system_satisfied_with(layout, index, &wanted, &mut installed, path_var) {
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
    // The planned members that are dev-linked, each marker read ONCE for the whole pass:
    // links are reconciled before the pass, under the store lock, and nothing below links
    // or unlinks a program.
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
        // Every unserved member gets ITS OWN canonical row — `unavailable on <target>:
        // <hint>`, with the program's `unavailable_hint` when the index carries one —
        // in the words the pass log prints (design §2). A member absent on this target
        // is a normal state, never a fault: `doctor`'s problem scan does not match it.
        let recorded = crate::status::read(layout);
        for p in &unserved {
            wanted.remove(p.as_str());
            // A program this machine has DISABLED keeps the row that says so. The
            // update lane records `aborted: stage` on a program whose current build was
            // recalled while its coherence group's replacement pin is not published
            // here (`disable_revoked_currents`), so that doctor counts it; repainting it
            // `unavailable on <target>: …` — a benign state doctor does not match — would
            // hide the fault. The measurement is the one
            // `repair` makes: the recorded row, or a tombstone shim among the program's
            // exposed tools. It is still unserved here: not announced, not counted, no
            // stub.
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
        // The toolset-wide BLOCKED verdict is reserved for a machine the index serves
        // NOTHING on (an Intel Mac before x86_64 lands): the pending stubs and doctor read
        // it as "nothing is coming". A partial matrix — gh and emacs absent on Linux while
        // the ALab set installs fine — must not raise it: `wanted` now holds exactly the
        // members this target CAN carry (served, already installed, or dev-linked), so
        // the machine is blocked as a whole only when nothing at all is left in it.
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
    // The signed sizes, derived only when the wire lane is about to speak. No extra fetch for
    // a manifest the triple probe above already pulled — the network fetcher memoises per
    // `(repo, program, build)` — and a member whose probe fetch failed is retried once here,
    // the first miss ending the sums.
    let sizes = if will_install.is_empty() {
        SignedSizes::UNKNOWN
    } else {
        seed_install_bytes(fetcher, index, &will_install)
    };
    // THE SET, REFUSED UP FRONT when this volume cannot hold what it adds up to: the
    // per-program gates below would otherwise install a few members and then fail each
    // one after. Computed from the bytes those gates check, per install unit, leaving out
    // the units this pass lands nothing of ([`set_units`]). An unknown size or a failed
    // free-space query lets the pass proceed, as the per-program gate does; those gates
    // stay the backstop for what the sum cannot see.
    //
    // The vendor lane's fresh landings, when they ride this pass, land at the same time:
    // the set must fit beside everything they need at once ([`FreshVendors::peak`]). A set
    // that does not is refused as it would be once they had landed, and they land alone.
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
    // ONE ANNOUNCEMENT FOR BOTH LANES: the vendors first, then the set, with their signed
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
    // The pass's plan, in PLAN ORDER, with each group's freshly-installable members —
    // materialized once so the priority queue below can permute what remains between
    // items without ever re-deciding WHAT is planned (reorder-only, §4).
    let plan: Vec<(usize, Vec<String>)> = groups
        .iter()
        .enumerate()
        .filter(|(_, group)| disk_refusal.is_none() && !flip_held(group))
        .filter_map(|(i, group)| {
            let missing = absent_members(group, &wanted, &installed);
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
    let vendor_plan = rider.as_ref().map_or_else(Vec::new, |r| r.fresh.planned());
    let vendor_names: Vec<String> = vendor_plan.iter().map(|(name, _)| name.clone()).collect();
    let shared = rider.as_ref().and_then(|_| fetcher.shared());
    // The live-progress plan (R5): per-program signed sizes fetched only when a sink
    // is live (the GUI lane) — N tiny verified manifest reads fix the overall bar's
    // denominator honestly before any byte moves. Best-effort: a miss degrades that
    // row to unmetered, never fails the pass.
    //
    // IN THE ORDER THE BYTES MOVE: the queue is what a pending stub reads its place in
    // ("next in line, after …") and what the window names when nothing is mid-flight. A
    // vendor riding this pass on a thread of its own lands once every archive of the set
    // is in, the coherence tuples' among them, or once no tuple is left ([`RiderGate`]) —
    // beside the rest of the set's landing — and after every set member when it has no
    // thread of its own, so its rows stand after the last tuple, or last, never ahead of
    // the toolchain they wait for ([`set_progress_queue`]).
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
        // THE PROMISED PLAN HAS LANDED: a stub reads this pass's rows from here on, and a
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
    // DOWNLOAD AHEAD ([`crate::flow::ahead`]): the plan's archives fetched on a thread of
    // their own while this one digests, extracts and flips the unit in hand — the link sat
    // idle through all of that, about ten seconds of the toolchain's alone. Bytes only, in
    // the order this loop will want them; every stage, flip and row stays here. Only with a
    // fetcher a pass may share, and only on a volume known to hold the set's peak plus what
    // the lane may fetch ahead ([`crate::flow::ahead::Ahead::reserve`]): where it does not,
    // the set lands as it always did, one download at a time.
    //
    // Over the units the disk check sized, and only when it sized them all ([`set_units`]):
    // their signed manifests are in hand, so planning the lane asks the network nothing — a
    // manifest the prescan could not fetch would otherwise be asked again here, a connect
    // timeout apiece on a dead link, before the first unit starts — and a unit this pass
    // lands nothing of (a held tuple, a refusal memo, a build not published here) moves no
    // byte ahead.
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
    // (in-flight bytes are never abandoned); and bumping any member bumps its whole
    // group (the transactional activation is untouched). The bump file is consumed
    // read-only here and deleted only at a clean pass end.
    let mut remaining: Vec<(usize, Vec<String>)> = plan;
    // THE VENDORS LAND BESIDE THE LOOP BELOW, on lane threads of their own when the fetcher
    // may be shared — [`RIDER_LANES`] at once, inside this pass's store lock, metered on its
    // progress pass, writing no status row (their outcomes come back to this thread) — else
    // after it on this one. Either way the set's own bytes go first: the toolchain's archives
    // take the whole link (measured: trust's 744 MB took 8 s alone, and 11 to 28 s beside
    // claude, agy and muse, whose hosts outpace GitHub's), and agents landing beside the
    // tuple's extract, or beside the set's other downloads, delayed trust's flip by up to six
    // seconds (two first runs of six) and the last ALab program by two to four (median). The
    // download-ahead lane opens the gate the moment the last archive of the set is in,
    // every unit's — it holds archives of each, or the gate waits for this thread — the link
    // is then idle while the pass extracts, flips and stages the rest, and the agents take
    // it; this thread opens it once no tuple is left to land (the lane did not fetch them
    // all), and when the loop ends or unwinds. An agent a PERSON asked for by name at a
    // terminal — its pending stub's bump, "bumped first when still queued, so the wait is
    // short" — does not wait for the gate; one a script asked for goes first once it opens
    // ([`RiderQueue::next`]).
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
    // not size) is fetched by this thread, which only it knows the end of, so the lane never
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
            // A refused worker narrows execution to this thread; its queue follows the
            // actual fallback order before the first set member starts.
            sink.queue(&set_progress_queue(
                &remaining,
                &groups,
                &vendor_names,
                false,
            ));
        }
        // The download-ahead worker, and this thread entered into its lane: every member
        // fetch below asks the lane first. Halted when `entered` drops — after the loop, or
        // unwinding — so the scope's join waits at most for the archive in hand.
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
            // Dev-linked HARD-SKIP (§13): a singleton skips itself; a coherence tuple with
            // ANY linked member skips WHOLE (can't partial-install a locked group over a
            // dev link) — the same rule as `apply_channel`.
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
                // A coherence tuple fresh-installs ALL-OR-NOTHING against the one index
                // resolved above (§7) — see [`bootstrap_group`].
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
                // An ungrouped member can move alone (§7) — see [`bootstrap_singleton`].
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
    // Second stub reconcile, against what ACTUALLY landed: a program whose real
    // shims do not expose its own name would otherwise keep a stale "installing"
    // stub until the next pass.
    let now_active = crate::active_builds(layout);
    crate::stub::reconcile(layout, &wanted, &now_active, &exposed);
    // The reroute stubs ride the same reconcile: the atpkg path they name is refreshed
    // ([`crate::stub::Embedder`]) and a row that left the table is swept (philosophy §4).
    lay_reroute_stubs(layout);
    // (The rustup seam is the caller's, once per pass through its [`PassSeams`]: read from
    // here, it wrote a test's scratch view into the machine's own `~/.rustup`.)
    // ALIAS reconcile over what is live NOW: every ALab program's `alab-<tool>` names
    // stand beside its shims (an install a pre-alias client made gets them here), and a
    // program the signed index no longer calls ALab's own loses them.
    reconcile_aliases(layout, index);
    // EXEC-ROOT reconcile over what is live NOW, after the aliases so the ones just laid
    // are rendered through a root that stands ([`reconcile_exec_roots`]): a trust member
    // this lane found up to date gets its shims routed here, one it just installed had its
    // root laid by the install itself.
    let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
    // SHADOW reconcile (design S5), over what is live NOW: a managed member whose
    // exposed name a foreign copy precedes on PATH is recorded as SHADOWED — said, never
    // fixed — with `type alab-<tool> for the managed one` on the log line when the alias
    // just reconciled is what answers.
    reconcile_shadowed(layout, index.index_build, path_var);
    // Parity with the per-member install path: a wanted member the channel does not
    // PIN fails loudly as NotPinned — silence would hide a half-published index.
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
    // What a CLEAN pass proved still awaits install (the narrowed/unserved were
    // removed from `wanted` above, so this is pending work, not refused work) —
    // the `*seed*` row reconcile's input at both callers' edges.
    let remaining_missing: Vec<String> = wanted
        .iter()
        .filter(|p| !now_active.contains_key(p.as_str()))
        .cloned()
        .collect();
    // Consume requests only after both lanes and the final missing-pin checks settle.
    // A failed rider keeps the queue like a failed set member. A deferred signature
    // check is no failure, but its program is still missing: a request for it may
    // arrive even now. Keep the bounded queue while any wanted work remains, avoiding
    // a read-then-unlink race with a pending stub appending its request.
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

/// The queue of both lanes in execution order. An ALab bump keeps its order even when
/// it moves a singleton ahead of a tuple; parallel vendor lanes stand after the last
/// remaining tuple — their gate opens once every archive of the set is in, the tuples'
/// among them, or once no tuple is left ([`RiderGate`]) — and a serial vendor lane follows
/// every set member.
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
/// ([`crate::flow::group_missing_triple`]): a coherence tuple's every member, since it
/// installs whole, else the `missing` ones alone.
fn prescan_probe<'a>(group: &'a crate::Group, missing: &'a [String]) -> &'a [String] {
    match &group.group {
        Some(_) => &group.members,
        None => missing,
    }
}

/// The grouped (coherence-tuple) arm of [`install_default_set_with_path`]: refuse a config-narrowed
/// partial tuple, clean-skip a tuple with no artifact for this triple, else fresh-install
/// the WHOLE group all-or-nothing against the caller's ONE resolved index (§7) via
/// [`crate::flow::bootstrap_group`] — stage-all → flip-all → rollback, exactly the update
/// path's transaction, so no failure mode can activate a partial or version-split trust
/// toolchain. Returns the hard-failure count (0 or 1); skips are never failures.
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
    // If the narrowing `exclude` leaves part of the tuple absent,
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
    // Missing-triple prescan: the singleton NoArtifact clean-skip doctrine
    // lifted to the tuple — a group that cannot fully exist on this host
    // is a correct state, not a 6h scream.
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
            // Advance the durable floor to the ONE index the whole group
            // trusted (§8 gate 3).
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
            // The transaction's own missing-triple hold, which fires only for a tuple with a
            // member already installed. A failed fetch proves nothing to either prescan, so
            // this is reached when a manifest a prescan could not fetch was fetched by the
            // walk and has no artifact for this triple: the same clean skip, in the same
            // words.
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
            // A CANONICAL ROW, not silence, for a program the index names. The words are the
            // update lane's (`TOMBSTONED_PIN`), and doctor counts them as the fault they are.
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
                // An already-installed member keeps its honest build AND its honest
                // attestation; a fresh member records neither. An all-or-nothing
                // transaction that aborted changed no member's bytes, so erasing the signed
                // root here would make `atpkg verify` fail closed on a program the abort
                // explicitly protected.
                // Same primitive as every other failure arm: the store, not the caller's
                // snapshot, is asked what survived.
                failure_row(
                    layout,
                    &failed,
                    format!("error: coherence group '{g}' bootstrap aborted — {cause}"),
                ),
                format!("bootstrap group '{g}' aborted at {failed} — {cause}"),
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

/// The ungrouped arm of [`install_default_set_with_path`]: the per-member install path, which keeps
/// its signed `requires` dependency resolution (§17). Re-reads the durable floor so a
/// floor advance recorded earlier in the pass is never undercut by the entry-time
/// snapshot. Returns the hard-failure count (0 or 1); the correct non-failure states
/// (dev-link, missing triple, app-bundle, tombstoned pin, a held digest verdict) are
/// skips.
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
            // Advance the durable floor to the index this install trusted
            // (§8 gate 3).
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
        // A target the pinned build does not serve: a CANONICAL STATE, not a failure
        // (design §2). The pass log says the same words the row records.
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
        // A member its digest-refusal memo HELD off a stage that would fail again
        // (`held: last attempt failed with …; <retry>`, [`crate::store::StageRefusal`]):
        // a skip, not a failure — see [`skip_held_member`].
        Err(crate::FlowError::StageRefused(row)) => skip_held_member(layout, program, row),
        Err(e) => {
            eprintln!("atpkg: bootstrap install {program} failed: {e} (continuing)");
            record_bootstrap_error(layout, program, &e);
            note_finished(program, crate::progress::Phase::Failed, Some(e.to_string()));
            1
        }
    }
}

/// The bootstrap lane's answer for a member whose digest-refusal memo HELD it off this
/// pass ([`crate::FlowError::StageRefused`]): the canonical `held: last attempt failed
/// with <why>; <retry>` row, recorded and carried as the member's SKIP — never a
/// failure. Returns the hard-failure count this contributes, which is 0.
///
/// Why a skip. This pass moved no bytes and staged nothing, and no unattended pass will
/// — the hold binds until the pin's signed digests change or a person asks — so the row
/// already says everything the pass could add, with the door. As a hard failure the
/// whole pass would exit FAILED, and the window re-runs a failed pass on a backoff
/// (aterm-gui's `FAILURE_BACKOFF_FIRST`/`_CAP`), not on the 6-hour cadence — one
/// deterministic verdict re-run every couple of hours for days.
/// The row still counts as a PROBLEM in `doctor` ([`crate::doctor`]'s `is_problem_state`),
/// where the retry is named, and `which` and the pending stub quote it.
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

/// Record one hard bootstrap failure for `program` into `status.toml` — shared by the
/// ungrouped install arm and [`install_default_set_with_path`]'s unpinned-member sweep, which
/// perform identical writes.
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

/// `atpkg install --default-set` — the explicit one-shot §11 bootstrap: install every
/// missing default-set member NOW (the Settings "Install ALab toolset" button's verb;
/// the config-consent-free twin of the loop's set-completion arm — running it IS the
/// consent, whatever `[packages].auto_install` says). Exit 1 iff any member hard-failed; skips are honest and free.
fn cmd_install_default_set() -> ExitCode {
    // THE LANE THIS GUARD EXISTS FOR. This verb announces `net-starting:` from inside
    // `install_default_set_with_path` and then ends through four separate returns, none of which
    // printed a marker — so the held "Installing the ALab toolchain…" card had no
    // answer at all, and its only reader had to guess one from an absence.
    let code = cmd_install_default_set_code();
    answer_announcement(code == 0);
    ExitCode::from(code)
}

/// [`cmd_install_default_set`]'s body, returning the raw exit code: exit 2 ("ran fine,
/// installed nothing, and never will here") is neither success nor a retryable failure,
/// and the announcement must be answered with which of the three actually happened.
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
    // Asking for the toolset is an unambiguous change of mind about any earlier
    // removal; undoing a decline must never mean hunting for a marker file. The
    // per-program removals go too — "install the whole set" plainly includes them.
    // CLEARED BEFORE THE PASS, not after: the pass narrows `wanted` by these very
    // markers, so a removed coherence-group member would narrow its whole tuple out of
    // the very install that was asked to restore it.
    clear_decline(layout);
    let all_removed: Vec<String> = removed_programs(layout).into_iter().collect();
    clear_removed(layout, &all_removed);
    // The seed pass's claim on the set goes too. From here on the owner's own act stands
    // behind it, and this verb never reads `auto_install`, so no later update pass may say
    // "installing aterm is the consent" or name that key ([`clear_seed_adoption`]).
    clear_seed_adoption(layout);
    let before = crate::active_builds(layout);
    // What this pass verifies from the channel itself, counted from here — its one stamp
    // says whether it reached the index ([`record_pass_end`]).
    let live_before = crate::flow::live_verifications();
    // The index resolve first, blocking nothing — the update pass's order: a fresh
    // verification refreshes the yank latch, so the vendor lane binds today's yanks; a
    // failed one leaves it the latch, and the set pass below says why it could not run.
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
    // The explicit "Install ALab toolset" act IS adoption — when it laid something
    // down, and when it found the whole set already on disk ([`default_set_adopts`]).
    // Adoption grants unattended network set-completion later (`cmd_update_all`), so
    // it is NOT recorded after a pass that installed nothing on a machine that received
    // no toolset at all (the "nothing lands here" exit below), nor over members the
    // pass skipped or still owes.
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
    // GC + shell-hook refresh once at the CLI edge (the cmd_update_all precedent) — including
    // its disclosure of what the pass abstained on, for the same reason: this verb walks the
    // whole prefix, so a skip here is not about a program the user never mentioned.
    //
    // The SPARING form, not plain `run`: a member whose multi-GB download failed mid-pass
    // keeps its `.part`, so retrying the bootstrap resumes rather than refetching from
    // byte 0. Programs the pass resolved nothing for are
    // swept exactly as before; the standalone `atpkg gc` still reclaims everything. The
    // build kept below each live one is read under the channel this verb verified, as the
    // update pass reads it.
    let channel = resolved
        .as_ref()
        .and_then(|index| index.channel_for(crate::config::CHANNEL, current_triple()));
    let report = crate::gc::run_keeping_pinned_partials(layout, channel.as_ref(), &|p| {
        failures_outcome.resolved_assets.get(p).cloned()
    });
    print_gc_sweeps("install-default-set", &report);
    print_gc_abstentions("install-default-set", &report);
    // THE RUSTUP SEAM, as the update pass re-asserts it after its gc.
    (seams.rustup_seam)(layout);
    (seams.shell_hooks)(layout);
    // The pass record says whether this pass reached the index, whatever it installed: the
    // verb's exit below reports its members, the stamp what the schedulers decide on.
    let now = (seams.now)();
    record_pass_end(layout, now, crate::flow::live_verifications() > live_before);
    if failures > 0 {
        return 1;
    }
    // A CLEAN pass retires a legacy `*seed*` offer row ([`retire_seed_row`]).
    retire_seed_row(layout);
    // NOT COMPLETE while a vendor program is still to come: its vendor did not serve it
    // this pass (a refusal already failed above), so nothing was checked — retryable, and
    // never "already complete".
    if !vendor_missing.is_empty() {
        eprintln!(
            "atpkg: default set incomplete — {} not installed yet (the line above says why); \
             `aterm pkg update` retries",
            vendor_missing.join(", ")
        );
        return 1;
    }
    let skipped = &failures_outcome.skipped_narrowed;
    // "Zero failures" is NOT "it worked". Every member can clean-skip — no artifact
    // for this triple (§6), all dev-linked, all tombstoned — so the no-op case gets its
    // own words and its own exit code, and a summary with skips names them instead of
    // claiming completeness.
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
            if let Some(line) = default_set_prereq_line(
                seams.path.as_deref(),
                &crate::prereq::CcHost::this_machine(),
            ) {
                println!("{line}");
            }
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
    if let Some(line) = default_set_prereq_line(
        seams.path.as_deref(),
        &crate::prereq::CcHost::this_machine(),
    ) {
        println!("{line}");
    }
    print_managed_current(layout, &vendor.unchecked, now);
    record_index_freshness(layout, now, None);
    0
}

/// The sentence a finished default set adds when the machine cannot BUILD with it: the
/// C toolchain's verdict ([`crate::prereq`]) against the `PATH` the pass ran with, in
/// `doctor`'s own words, when it is an answer that blocks builds. The install did what
/// was asked, so the exit code stays; the set simply stops claiming a capability it did
/// not prove. `None` when a toolchain answers, or the probe cannot tell. `host` is
/// [`crate::prereq::CcHost::this_machine`] in the verb.
fn default_set_prereq_line(
    path: Option<&std::ffi::OsStr>,
    host: &crate::prereq::CcHost,
) -> Option<String> {
    let verdict = crate::prereq::probe_with(path, host);
    verdict
        .blocks_builds()
        .then(|| format!("atpkg: {}", verdict.line()))
}

/// `atpkg seed` — the first-launch bootstrap (§9.1/§11), the verb the GUI spawns once per
/// launch and a person may run by hand. It INSTALLS NOTHING: it records adoption
/// (installing aterm is wanting the toolset), lays a pending stub for every default-set
/// name so each answers on PATH before its bytes land, lays the reroute stubs, re-asserts
/// the rustup seam and the exec roots, and then says truthfully whether the signed index
/// serves this machine ([`report_seedless_posture`]). The update pass that runs right behind
/// it is what installs — over the network, announced before a byte moves.
///
/// Consent: `[packages].auto_install`, the ONE install consent (default TRUE). `false`
/// adopts nothing and lays nothing; the one write every launch makes, whatever the
/// consent, is retiring a legacy `*seed*` row ([`retire_seed_row`]). Store mutation is
/// serialized by the store-wide lock held at the dispatch edge (`seed` is in
/// [`verb_mutates_store`]). Every early return is loud and exit-0.
fn cmd_seed(rest: &[String]) -> ExitCode {
    if let Some(refusal) = zero_arity(rest, "seed") {
        return refusal;
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    // A LEGACY `*seed*` OFFER ROW goes first, whatever the consent below decides. Its one
    // writer — the sealed-seed lane's `auto_install = false` offer, deleted in Phase 5 —
    // wrote it exactly on the machines whose update pass never completes the set, so the
    // sweeps in the set-completing lanes alone would never reach it ([`retire_seed_row`]).
    retire_seed_row(&layout);
    // INSTALLING ATERM IS WANTING THE TOOLSET, so adoption is recorded here, on every
    // Mac, before anything else is asked — recording who adopted; the one install consent,
    // not adoption, gates set completion (`should_complete_set`).
    //
    // But only once the ONE consent predicate says yes ([`crate::adoption`], which the
    // window asks too, so the two cannot drift). A deliberate `uninstall --all` wins — and
    // so does `auto_install = false` (or the retired `seed_install = false` it folds in),
    // checked BEFORE adoption for the same reason: adopting first once made that flag a
    // lie, because the update pass then installed the entire ~3.2 GB set over the NETWORK
    // seconds later. The one documented way to say "not on my disk" has to be honoured
    // before anything records a wish for the toolset.
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
        // A `[packages] prefix` this build ignores counts as the same "not here" until its
        // line is removed: the toolset lives where it points, and this lane would lay a
        // second one down. Loading the config has already said so
        // ([`crate::config::ignored_prefix_note`]), so nothing more is printed.
        crate::Adoption::IgnoredPrefix => return ExitCode::SUCCESS,
        crate::Adoption::AutoInstallOff => {
            // Declining the lay-down is not adoption: leave the store as it was (the
            // legacy-row sweep above is the one write, and it only removes).
            println!("atpkg: [packages].auto_install = false — not installing the ALab toolset");
            return ExitCode::SUCCESS;
        }
        // A DISABLED MANAGER ADOPTS NOTHING: stubs laid now would promise a pass that
        // answers "disabled", and a durable adoption would install the whole set the
        // moment the switch lifts — a consent this lane never obtained.
        crate::Adoption::ManagerDisabled => {
            println!("atpkg: disabled (no root key pinned) — not installing the ALab toolset");
            return ExitCode::SUCCESS;
        }
    }
    // Who adopted rides beside the marker, and only when this call creates it: an update
    // pass says "installing aterm is the consent" only on a machine the seed pass adopted.
    record_seed_adoption(&layout);
    // THE UPDATE THIS LANE OWES, PROMISED BEFORE A STUB CAN RUN: on a first run the window
    // spawns its update next, and that update writes `progress.json` only seconds later,
    // once it has resolved the index and planned. A stub run meanwhile waits for it
    // ([`pending_state`]) instead of finding nothing installing. Only the window's seed
    // knows an update follows ([`seed_promise_owner`]), and it promises for as long as the
    // window lives, at most [`crate::progress::SEED_PROMISE_SECS`].
    if let Some(owner) = seed_promise_owner()
        && crate::adoption::first_install_ahead(&layout)
    {
        crate::progress::promise_pass(&layout, owner, crate::progress::SEED_PROMISE_SECS);
    }
    // PENDING STUBS AT ADOPTION (R6): the instant this machine wants the toolset,
    // every default-set name resolves on PATH — before a single network byte moves.
    // Running one prints the live install state (`atpkg __pending`) instead of "command
    // not found". Compiled roster only; the first index resolve reconciles it against
    // the signed set.
    crate::stub::lay_adoption_stubs(&layout, cfg);
    // REROUTE STUBS beside them (philosophy §4): the upstream Rust names answer inside a
    // session from the first seed on, not only once the spawn seam has laid them.
    lay_reroute_stubs(&layout);
    // THE SHELL HOOKS NOW, not at the end of the first update pass a minute later: they
    // are what append `bin/` to a shell's PATH (the window's integration re-sources them at
    // the next prompt), so the stubs just laid answer at once and the real shims replace
    // them in place as they land. Every gate stays [`crate::hooks::refresh`]'s own: the rc
    // opt-out, the release-bundle rule, the consent-fenced folders.
    refresh_shell_hooks(&layout);
    reassert_rustup_seam(&layout);
    // The EXEC ROOTS beside the seam, and for the same reason the seam is re-asserted
    // here: this lane runs at every launch. A machine already on a trust build whose
    // tippy refuses its own `bin/rustc` copy is healed at the next launch rather than at
    // the next networked pass.
    let _ = reconcile_exec_roots(&layout, crate::seam::Depth::Shallow);
    // DO NOT PROMISE WHAT THE INDEX CANNOT DELIVER: say whether the signed index serves
    // this machine, and leave a durable trace of the answer.
    report_seedless_posture(&layout);
    ExitCode::SUCCESS
}

/// The channel-pinned members `index` can still install on THIS machine: wanted by
/// config, not removed on purpose, not already active, and carrying an artifact for this
/// triple.
///
/// The triple test is the one that matters for honesty. Artifact coverage is per
/// program, not per catalogue: the standalone programs publish `x86_64-apple-darwin`
/// beside `aarch64-apple-darwin`, while the rustc coherence tuple (`trust`, `trust-ir`,
/// `trust-cg`, `trust-vc`) is `aarch64-apple-darwin` only. So this returns the part of
/// the index that can be served here — possibly nothing — and [`index_serves`] (for
/// [`report_seedless_posture`] and the update pass's empty-store verdict) says which
/// sentence is true when it is nothing.
///
/// Group granularity matches the installer's: a coherence tuple is all-or-nothing, so a
/// tuple with any member lacking this triple contributes nothing (the same rule
/// [`install_default_set_with_path`] acts on).
fn seed_serviceable(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    cfg: &crate::config::PackagesConfig,
) -> Vec<String> {
    // Programs the user removed on purpose are NOT missing — they are absent by
    // request, and the lane's "install whatever the store lacks" rule would
    // otherwise reinstate them on the next launch.
    serviceable_among(
        layout,
        fetcher,
        index,
        &wanted_programs(index, cfg),
        &removed_programs(layout),
    )
}

/// [`seed_serviceable`] over an explicit `wanted` set and `removed` set, so
/// [`index_serves`] can ask the same question WITHOUT the user's narrowing.
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

/// What the signed index says about THIS machine, for an EMPTY store's one verdict
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

/// Whether `index` serves this machine, and — when nothing is left to install — WHY.
///
/// [`seed_serviceable`] is empty for three causes, and only one is the CPU's: nothing is
/// published for this triple, `[packages].exclude` names what is, or the user uninstalled
/// it. Asking the same question without the user's narrowing tells them apart: when the
/// unnarrowed set is not empty, the narrowing alone emptied it, and blaming the
/// architecture tells a user their Mac is unserved when their own config is the cause.
/// (The sealed-seed lane's ladder drew these lines until Phase 5 deleted it; this is the
/// signed index's form of the same distinction.)
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
/// row ([`crate::manifest::Cost`]): `download_bytes` for the wire lane, `disk_installed` for
/// what stays.
///
/// A figure is `None` when any member's manifest is unavailable (the first miss ends both
/// sums) or its entry is zero: a partial sum would understate the commitment.
fn seed_install_bytes(
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    members: &[String],
) -> SignedSizes {
    let triple = current_triple();
    // [`crate::TrustedIndex::channel_for`], not a `channels` scan: the channel a host sees
    // is the per-target view, so the figures announced are the ones this triple's pins will
    // actually fetch.
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
/// ([`crate::flow::PlannedLanding::Nothing`]: a revoked pin, a local pin holding the tuple,
/// a member held by its digest-refusal memo, …) is left out, so it does not decide whether
/// the rest fits. `None` when any other unit's bytes are unknown — a partial sum would
/// understate the need.
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
    /// `(member, why)` for every member of every unit the check judged, recorded as
    /// `error: <why>` ([`record_bootstrap_failure`]). A unit that cannot fit by itself gives
    /// each of its members the unit's own figure (the `not enough disk space` sentence its
    /// gate would write); every other member records the set's sentence. So no row says a
    /// member cannot fit when its unit would, and none keeps an earlier pass's figure.
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
/// ([`crate::cost::human_bytes`]) so the figures a user is told cannot drift across surfaces.
/// The GUI's toolchain bar reads it out of the line's last `(…)` and clips at 40 characters,
/// so the arms are built to fit: two 9-character figures (up to `999.9 GiB`) render in 39.
/// The one overflow is the two-figure arm with BOTH in the `1000.0–1023.9 MiB` band, which
/// renders 41 and clips "on disk" to "on dis…" with both numbers intact — a set whose
/// download packs several-fold never lands there. An unknown figure prints nothing: no
/// number beats a wrong one.
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

/// Say something TRUE about whether the signed index serves this machine, and leave a
/// durable trace of it — the seed verb's last word.
///
/// Three very different situations reach here and they deserve different sentences: a
/// machine whose architecture the index publishes for (the toolset really will arrive
/// over the network), one it does not (nothing arrives until someone publishes an
/// artifact), and one whose own `[packages].exclude` or uninstalls took everything the
/// index serves ([`index_serves`]). Assuming the optimistic one made the product promise
/// every Intel user a toolchain at first launch and then never mention it again.
///
/// It also records `status.toml`, because the ABSENCE of that file is what makes
/// Settings — the one surface a puzzled user is pointed at — say "atpkg has not run
/// yet" on a machine where it has run correctly on every single launch.
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
    // The only thing to consult is the network index. A resolve failure is not the
    // question being asked (offline is not "unsupported"), so it degrades to the
    // generic sentence rather than an alarming one.
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
        // NOT THE CPU: the user's own exclusion or removals emptied a set the index serves.
        Some(IndexServes::Narrowed(narrowing)) => {
            let (why, state) = narrowing.verdict(triple);
            emit_marker_line(&seed_unusable_line(&why));
            record_unusable_toolset(layout, state, &why);
        }
        // Said only while the store is EMPTY, and only to whoever ran the verb
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

/// Whether the seed verb tells its reader the ALab toolset "is not installed yet": only
/// over an EMPTY store, and never under the window's
/// own pass (`window_pass`: a `--wait-lock` caller, [`wait_lock`]). That lane downloads
/// the set itself right behind this verb and shows it on its own row, so the sentence —
/// which names the command to run — told nobody anything, from inside the window's log.
fn says_not_installed_yet(store_empty: bool, window_pass: bool) -> bool {
    store_empty && !window_pass
}

/// Whether the signed index serves this machine, for the seedless lane's one sentence:
/// the index's answer ([`IndexServes`]), or `None` when the index never resolved.
///
/// THE CHEAP QUESTION FIRST. A store that holds any build is served by definition, so
/// `resolve` — a fetcher, the index discovery walk on the download host, the index-cache
/// parse, the roster/index verification and a `roster.floor` record, all under the store
/// lock `seed` holds — is never run for it: its answer could not change the output, and
/// offline it costs up to three 30 s network attempts holding `store.lock`. Only an
/// EMPTY store — where
/// the unserved-architecture verdict is the whole point — pays for the resolve. A store a
/// vendor-direct program is still to land in is served the same way ([`vendor_set_wanted_on`]).
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

/// Retire a LEGACY `*seed*` row and NOTHING ELSE — the row, not the aggregate `outcome`
/// sentence, which belongs to the pass that just ran.
///
/// The row was the sealed-payload lane's `[packages].auto_install = false` offer
/// ("pending-consent: <programs> — Install ALab toolset below"), written only by that
/// lane, which Phase 5 deleted. `record_status` MERGES the program map, so a row an older
/// build wrote would otherwise advertise a seal nothing reads, in Settings ▸ Packages,
/// forever. The seed verb sweeps it at every launch before it reads any consent — the
/// one lane that reaches an `auto_install = false` machine, the only kind that holds the
/// row — and a clean set-completing pass or explicit toolset install sweeps it too.
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
        // Coherence-group member (§11): the transactional path. Pin is consulted inside
        // apply_program's per-group gate.
        let report = match crate::flow::apply_program(
            &*fetcher,
            &layout,
            &effective_anchor(&layout),
            crate::config::CHANNEL,
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
        let failures = report_channel_apply(&layout, &installed, &report, &[], false);
        for p in &report.skipped_linked {
            println!("atpkg: {p} dev-linked — skipped");
        }
        // GC after every successful activate — the same policy (and order: GC, then the
        // shell-hook refresh) as `cmd_update_all` and `do_install`, which the ungrouped
        // path below reaches through `cmd_install`. Best-effort; never fails the update.
        // The SPARING form, for the same reason as those verbs: a tuple member whose
        // download failed aborted its group but resolved its pin first, and its `.part`
        // is the resume state the next attempt continues from. Under the channel the plan
        // just verified (this verb never refreshes the cache, so a yank published since the
        // last pass is in `plan.channel` alone): moving a member off a yanked build keeps
        // the build `aterm pkg rollback` lands on, never the yanked one.
        let gc = crate::gc::run_keeping_pinned_partials(&layout, Some(&plan.channel), &|p| {
            report.resolved_assets.get(p).cloned()
        });
        print_gc_sweeps("update", &gc);
        print_gc_abstentions("update", &gc);
        // THE SEAM AND THE EXEC ROOTS — the same two acts `cmd_update_all_code` performs
        // after its gc, and for the same reason. This is the branch `aterm pkg update trust`
        // takes (trust is in the `rustc` coherence group), so it is literally the lane that
        // printed `atpkg: rustc up to date` on m3 for two weeks without ever inspecting the
        // `~/.rustup/toolchains/trust` entry that decided what the machine compiled with.
        // Order matches the pass: gc, then the roots, then the shell hook.
        reassert_rustup_seam(&layout);
        let _ = reconcile_exec_roots(&layout, crate::seam::Depth::Shallow);
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

/// `atpkg update <vendor program>` (design §1.5, §1.6): the vendor lane alone, under the
/// local pin, its one line on stdout — or stderr, exit 1, when it refused, or when a
/// person asked and it could not check (unreachable).
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

/// What the vendor lane of a pass did: its faults (and each failed program with its
/// line, for the pass's outcome), the per-version staging names it resolved (the pass-end
/// gc spares their `.part`), the programs it ran (all a vendor door's gc may touch),
/// whether any build moved, the programs whose vendor it could not reach (the
/// `managed-current:` marker leaves them out), and whether any vendor's channel ANSWERED
/// (a pass that reached nothing is offline, not a success — [`pass_reached_nothing`]),
/// and the programs it LANDED a new build of, not yet warmed ([`warm_landed`]).
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

/// The pass's outcome sentence once its index lane applied clean (`clean`): `clean`,
/// unless the vendor lane failed a program — then that program's own line, or for several
/// the count and names every failed pass states. A vendor refusal is never headlined as
/// an index verdict.
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
/// lane needs no index — agy and muse are in none — so the seed's sentence never calls
/// such a machine unserved, or narrowed to nothing, when the index part is empty.
fn vendor_set_wanted_on(
    triple: &str,
    cfg: &crate::config::PackagesConfig,
    removed: &std::collections::BTreeSet<String>,
) -> bool {
    crate::vendor_direct::VENDORS
        .iter()
        .any(|spec| spec.serves(triple) && vendor_wanted(cfg, removed, spec.program))
}

/// The cached verified index, read only when an active vendor program is still a legacy
/// index build whose version no pass has kept yet, or a legacy build it may roll back to
/// has no signed root held ([`crate::vendor_direct::lane::legacy_root_owed`]) — the one
/// reader of it (the version, for the no-downgrade rule; the root, for a rollback; the
/// lane keeps what it reads in the program's stamp).
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

/// The vendor lane short of its fresh landings: every vendor program it runs is planned —
/// side by side when the fetcher may be shared, the four heads and their documents on four
/// different hosts — each installed one is then landed in turn as its vendor moved it, and
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
    // in the shim view, and it is still this machine's install: the update lane revisits
    // it, as the index lane revisits one, or it waits on a person forever.
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
            // A FIRST LANDING WHOSE SIGNATURE CHECK DID NOT FINISH IS A WAIT, NOT A FAILURE
            // (review of 4329ea590). The update lane already read
            // `Verdict::Deferred` that way for an installed program, but this fresh path
            // put every non-Installed verdict on the Failed row and, with nothing
            // arrived, printed `net-failed:` — which the window reads as
            // `PkgSeedFailed`, the "Package update failed" card and badge. That is
            // exactly the moment a codesign timeout is likeliest: a machine laying
            // codex's 238 MB binary for the first time, disk busiest, codesign clamped
            // to the Background role (453 s measured at load ~90 against a 2 s
            // unclamped run). The row is Skipped with no error; the deferral's own
            // line goes to stdout through `report_vendor`; the archive is kept and the
            // next pass lands it without a download.
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
/// signed sizes, a "net" progress pass of their own when the window asked for one (or a
/// person typed the pass) and none is live, each outcome reported as it lands, answered
/// with their terminal marker ([`fresh_landing_terminal`]). `promised` when they are what
/// is left of a promised default-set pass whose set completion stopped before it could
/// carry them: their plan is then the promised plan, and the promise goes with it
/// ([`crate::progress::ProgressSink::plan_promised_pass`]). Ahead of a set completion still to
/// come, a plan of the vendors alone keeps it.
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
/// lane frees. Measured on a first run: two lanes land codex seven seconds sooner and end
/// the pass eight sooner than one, with claude as early; four end it two seconds sooner
/// still, but claude, splitting the link four ways, a second and a half later. The lanes
/// take turns at the download itself, in the order their programs were taken
/// ([`RiderQueue::next`], [`crate::flow::DownloadTurns`]), so the overlap is one lane's
/// download beside the other's verify, unpack and activate.
const RIDER_LANES: usize = 2;

/// The gate a riding vendor lane waits on ([`rider_land`]): open for good once every archive
/// the set completion lands is in, its coherence tuples' among them — the
/// download-ahead lane says so the moment the last one lands
/// ([`crate::flow::ahead::Ahead::all_fetched`]) — or once no tuple is left to land, whichever
/// comes first. The pass thread's [`RiderGateOpens`] opens it when the loop ends or unwinds,
/// so no lane waits on it forever.
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
    /// ONE AGENT DOWNLOAD AT A TIME, in the order the programs are taken
    /// ([`crate::flow::DownloadTurns`]): the first agent has the whole link — on a slow one,
    /// two downloads at once would land claude later than one — while the other lane's
    /// verify, unpack and activate overlap the next download.
    turns: crate::flow::DownloadTurns,
}

impl<'q> RiderQueue<'q> {
    fn of(rider: &'q VendorRider<'_>) -> Self {
        Self {
            left: std::sync::Mutex::new(rider.fresh.pending.iter().collect()),
            turns: crate::flow::DownloadTurns::default(),
        }
    }

    /// The next landing for a lane: a program a PERSON asked for by name, the bump its
    /// pending stub wrote at a terminal ([`crate::progress::Asker::Person`]), first and at
    /// once, gate or no gate — the set completion's own queue honours a bump between its
    /// items, and an agent queued behind the toolchain is the same promise; else, once
    /// `gate` opens (`None` is open), a program a script asked for (its bump goes first
    /// among the agents, never ahead of the toolchain's bytes: `claude --version` in a
    /// script is not a person waiting), then the first left in table order. `None` once
    /// every one is taken. Each comes with its place in line for the link, taken with it, so
    /// the payloads move in the order the programs were taken.
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

/// The terminal marker that answers a `net-starting:` announcement of `planned` landings —
/// the vendor lane's alone, or the set completion's with the vendors riding it — of which
/// `arrived` installed and `deferred` waited on a signature check that did not finish
/// ([`crate::vendor_direct::lane::Verdict::Deferred`]).
///
/// * something arrived → `net-installed:` naming it (a deferral beside it was said on its
///   own stdout line);
/// * nothing arrived and some landing FAILED (neither installed nor deferred) →
///   `net-failed:`, the failure the window shows;
/// * nothing arrived and every landing deferred → `seed-done:`, the positive terminal
///   ([`SEED_DONE_MARKER`]: "an announced pass that ran to its end and has no richer
///   marker of its own"), naming what waits. Never `net-failed:` — nothing failed — and
///   never silence, which the window cannot tell from a child that died after announcing.
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

/// Say one vendor-lane outcome (a fault on stderr, and an unreachable vendor too when
/// `unchecked_fails`: the verb a person typed did not check), count it, and record its row
/// as of `now`.
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
    // A verdict the vendor's channel had to answer — never an unreached head, a dev link,
    // or a keep decided with no head in hand.
    // An unreachable channel that answered with a status (`why`) is a host that answered.
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

/// A vendor program's `status.toml` row after its lane (design §1.7), for the active
/// build: `managed <version> — <Vendor> latest` only when it IS the vendor's head
/// (installed or current); `managed <version> — <why>` when the head was not taken for a
/// reason that is no fault ([`crate::vendor_direct::lane::Outcome::kept_clause`]);
/// `error: <line>` for a refusal or a failed landing (a doctor problem); `tombstoned:
/// <line>`. An unreachable vendor judged nothing: its row stands while it names the
/// active build and a head check reached the vendor within the last day (as of `now`);
/// past that it becomes `managed <version> — <Vendor>'s release channel not reached since
/// <date>`, so no surface keeps calling a build the latest with no check behind it. A row
/// is written only when it changes.
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
    // (A SHADOWED row an older client recorded is simply replaced: SHADOWED is read per
    // shell, never recorded — Phase 3, `crate::status::with_shadows`.)
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
/// other outcome — and record per-program status, returning the number of ABORTED
/// groups (the caller's exit-code input). Shared by [`cmd_update_all`] and
/// [`cmd_update_one`].
fn report_channel_apply(
    layout: &crate::store::Layout,
    installed: &std::collections::BTreeMap<String, u64>,
    report: &crate::ChannelApplyReport,
    also_failed: &[String],
    whole_set: bool,
) -> u32 {
    // Post-apply ACTIVE builds (shim-derived), for accurate per-program status.
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
    // The members that failed, by name: `record_status` overwrites the aggregate
    // `outcome`, so Settings' one line must name them all (audit).
    let mut failed_names: Vec<String> = Vec::new();
    // ONE SUMMARY PER PASS: the members that were already current and the
    // ones this pass moved, said together at the end ([`channel_apply_summary`]) — a
    // healthy pass printed one `<group> up to date` line per group, ten lines saying
    // nothing happened, on every pass and into the window's log.
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
                // A RETIRED GATE'S ROW IS LIFTED. A member an older client's requirement
                // gate recorded `blocked by <dep>: …` (that gate is gone, design §5.3(b))
                // whose pin has not moved is UpToDate: nothing flips, so no
                // other arm would rewrite the row, and the block would outlive the gate
                // that stated it. Restore the canonical managed row, build and attestation
                // kept.
                //
                // THE SAME ARM MIGRATES AND RE-STAMPS. A row spelled `active` was
                // written by a pre-`managed` client and no pass ever rewrote it; a
                // `managed N — pinned by index 15` row stays at 15 through six index
                // builds because only an install rewrites it. One healthy store then
                // read as three spellings — `pinned by index 15`, `active`, `pinned
                // by index 21` — and the owner took the 15s for stale tools
                // (audit). An UpToDate member's row now always names THIS
                // pass's index; the outcome sentence the pass already recorded is kept
                // (only the lifted-hold case narrates, as before).
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
                    // Only the members actually flipped carry a fresh signed tree_root; an
                    // untouched (UpToDate) sibling reports none, so preserve its recorded root
                    // rather than wiping the attestation `atpkg verify` needs.
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
                            // KEEP THE RECORDED ROOT. These arms describe a program
                            // that is installed and whose tree did NOT change — a
                            // held pin, a tombstoned pin, the post-state of an
                            // aborted transaction. Writing an empty root there
                            // DELETED the signed attestation of bytes nothing had
                            // touched, and `atpkg verify` treats an empty root as
                            // fail-closed, so a single `atpkg pin trust` turned into
                            // "reinstall 3.2 GB to enable verification".
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
                // A SIBLING'S `deferred:` ROW IS OVER TOO. The pin ended the
                // wait (its record is cleared), but only the pinned members' rows were
                // rewritten: a sibling the held flip would have moved kept saying its build
                // "is staged and installs when nothing is using the toolchain" until an
                // UpToDate pass — a flip no pass would make. It is held by the pin now, and
                // says so. A sibling whose row is anything else is left as it stands.
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
                // THE TOOLCHAIN'S FLIP WAITS FOR QUIET ([`crate::quiet`]): staged, nothing
                // flipped, never a failure. One line on stdout — the pass log and the
                // window's log — and one `deferred:` row per member it would move, which
                // doctor and Settings read; doctor prints the whole record. "While an aterm
                // window or terminal session is open" is what makes "at the latest" true: a
                // window's park looks, and so does any interactive session, which takes the
                // watch's seat at its held-move look whether or not a vendor program is
                // watched.
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
                // The missing-triple hold's rows — the update-lane twin of the bootstrap's
                // clean skip: the new pin cannot fully exist on this host, so the tuple stays
                // whole on its current builds and nothing moved. A correct state, never a
                // failure; one row per installed member, and the UpToDate arm lifts them.
                // The members by name, never the group key (no program the reader has).
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
                // THE CAUSE RIDES EVERYWHERE THE ABORT DOES: the stderr line,
                // the member's own row (`aborted: <phase> — <cause>`, the prefix kept for
                // every reader that keys on it), the pass outcome, and a MARKER on stdout
                // the window turns into a row on the toolchain lane.
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
    // The programs another lane of the same pass failed (`also_failed`: the vendor lane's)
    // count toward the aggregate too: one failure of each is two, never the last one's
    // sentence alone.
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
/// the rest are current. `current` are the programs already on their pinned builds,
/// `moved` the ones this pass flipped (`None` for a sibling that was not installed), and
/// `other_outcomes` whether some group ended another way (held, blocked, withdrawn,
/// failed) — those say so on lines of their own, so "all" would not be true. `whole_set`
/// is false when the pass checked ONE program's group (`update <program>`): its members are
/// named, since a count would read as the whole toolset. `None` when the pass touched
/// nothing. Pure for the test.
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
/// 0 on the real clock. `Err` has ALREADY said why: the usage (exit 2), the first file
/// that cannot be read, with its path and error (1), or the opaque roster refusal (1).
fn admit_for_verify(
    args: &[String],
    verb: &str,
) -> Result<(Vec<u8>, Vec<u8>, crate::TrustedRoster), ExitCode> {
    let [master, doc, sig, roster, roster_sig] = args else {
        return Err(usage_error(verb, None));
    };
    // Opening a file is no verification oracle (§8 covers the signature checks below).
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
        // Opaque on purpose — no verification oracle (§8).
        eprintln!("FAIL: the roster did not verify under that master, or is stale");
        return Err(ExitCode::from(1));
    };
    Ok((raw, sig_bytes, admitted))
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
        // Post-verify, not a crypto oracle: the signature held, and it is the index's own
        // already-authenticated bytes that cannot be honoured. Named, so the publisher
        // fixes the row instead of re-keying a machine.
        Err(why) => {
            match index_post_verify_refusal(&why) {
                Some(reason) => eprintln!("FAIL: {reason}"),
                // Every CRYPTO verdict stays opaque (§8: no verification oracle).
                None => eprintln!("FAIL: no machine on that roster signed this index"),
            }
            ExitCode::from(1)
        }
    }
}

/// What an [`crate::sig::TrustedRoster::authorize_index`] refusal means to a PUBLISHER,
/// when it means anything nameable: `Some(reason)` for the POST-VERIFY variants — the
/// signature already held, and the index's own authenticated bytes are the problem — and
/// `None` for every crypto verdict, which stays deliberately opaque (§8).
///
/// WHY A CATCH-ALL WAS WRONG HERE. `authorize_index` runs the Ed25519 check FIRST
/// (`authorize_bytes`), then [`crate::manifest::parse_index`], then the attribution bind —
/// so [`crate::Reject::Malformed`], [`crate::Reject::Schema`],
/// [`crate::Reject::Unattributed`] and [`crate::Reject::SeqMismatch`] are only ever
/// reached with a valid signature already in hand. Reporting them as "no machine on that
/// roster signed this index" sent the publisher to the PAPER MASTER to re-join or re-key a
/// machine when the fix is one line of the index they just authored: a duplicate key or a
/// wrong-typed field from a hand edit reads as `Malformed`, and a misread `roster_seq`
/// reads as `SeqMismatch` — a name this verb never printed.
///
/// [`crate::Reject::NotAuthorized`] is deliberately NOT named. It is the one verdict
/// reachable from BOTH sides — `Roster::authorize_appcast` returns it when the roster has
/// no live machine at all (pre-verify), and the bind returns it for a `machine_id` naming
/// a different machine than the key that verified (post-verify) — so the opaque line is
/// both true of it and the only safe thing to say.
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
///
/// The signature is only HALF the chain a client runs, so this verb runs the other half
/// too ([`pkg_post_verify_refusal`]): `OK` means a client can both authenticate AND read
/// this manifest, never the signature alone.
fn cmd_verify_pkg(args: &[String]) -> ExitCode {
    let (raw, sig_bytes, admitted) = match admit_for_verify(args, "verify-pkg") {
        Ok(admitted) => admitted,
        Err(code) => return code,
    };
    match admitted.authorize_bytes(raw, &sig_bytes) {
        Ok((verified, who)) => {
            // THE SIGNATURE IS HALF THE CHAIN. A client parses these bytes the moment
            // they verify and drops the program on any refusal, so a self-check that
            // stopped at the signature reported OK on manifests the whole fleet refuses.
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

/// The CLIENT's post-verify gates over an already-authenticated `pkg-*.toml`
/// ([`crate::manifest::parse_pkg`]): `None` when the manifest parses exactly as a client
/// reads it, `Some(reason)` naming the fix when every client would refuse it.
///
/// WHY A SIGNATURE CHECK ALONE WAS A FALSE OK. `crate::flow` reaches a manifest's bytes
/// through `verify_pkg(raw, &sig)` and then IMMEDIATELY through `parse_pkg(&verified)`,
/// discarding the candidate on any refusal — no message, no diagnosis, the program simply
/// reads as not installable. This verb is the client's own rule on the command line —
/// so before it parsed as well as verified, a manifest
/// with a `shim_env` entry no shim can lay, an `https` row missing `asset`/`sha256`, the
/// retired `kind = "vendor-fetch"`, or a `schema` this build cannot read passed the
/// self-check, was mirrored, and was then refused fleet-wide — discovered by users, after
/// the tag and the build number had been spent. [`crate::Reject::ShimEnv`] promises in its
/// own doc that THIS verb names the fix, and [`crate::Reject::RetiredKind`] that the
/// authoring machine's own `atpkg install` does; each carries its reason, and this is where
/// the publish pipeline reads both.
///
/// Naming them is not a verification oracle (§8), exactly as [`cmd_verify_index`] naming
/// [`crate::Reject::Malformed`] is not: every variant reachable here is a PARSE failure
/// over bytes whose signature already verified.
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

/// `atpkg link <program> <checkout> [rel-bin…]` (§13) — dev-link curated bins from a sibling
/// checkout into `bin/` (all through the sensitive-name deny-list), marking the program so
/// `update`/`apply` HARD-SKIP it until `atpkg unlink`. Default bin when none given:
/// `target/release/<program>`.
fn cmd_link(rest: &[String]) -> ExitCode {
    let (Some(program), Some(checkout)) = (rest.first(), rest.get(1)) else {
        return usage_error("link", None);
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
                    "atpkg: {program} not linked — {checkout}/bin lacks {} of its commands \
                     ({}); build them, then re-run",
                    missing.len(),
                    missing.join(", ")
                );
                return ExitCode::from(1);
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
    match crate::link(&layout, program, checkout_path, &bins) {
        Ok(out) => {
            println!(
                "atpkg: dev-linked {program} ({}); `aterm pkg unlink {program}` to release it",
                out.linked.join(", ")
            );
            for r in &out.refused {
                eprintln!("atpkg:   refused sensitive bin name {r} (deny-list)");
            }
            // The seam follows the link from this moment, not from the next six-hourly
            // pass: otherwise `targo` on PATH and `cargo +trust` are two compilers under
            // one name.
            if program == crate::seam::SEAM_PROGRAM {
                reassert_rustup_seam(&layout);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atpkg: {program} not linked \u{2014} {e}");
            ExitCode::from(1)
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
        return usage_error("unlink", None);
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

/// Re-assert `program`'s shims from its CURRENT store build.
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
    // ONLY the names the link owned. A sysroot bundle's `bin/` carries backends belonging to
    // OTHER atpkg programs — `trust`'s carries `ay`, `clean`, `ty` — so restoring from the
    // directory listing repoints those programs' shims at this build. Measured: it clobbered
    // all three onto `store/trust/6459`. The marker's set is scoped to this link, which makes
    // the restore symmetric with the takeover.
    let Some(owned) = owned else {
        return Ok(Restored::Nothing);
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
        return Ok(Restored::Nothing);
    }
    // The alias policy, offline: the §14 cached index's verdict when one is cached, else
    // what is on disk — a dev link takes over the PLAIN names only and the aliases kept
    // naming the store copy throughout, so the restore must neither sweep those aliases
    // nor invent any for a vendor tool.
    let aliases = alias_policy_offline(layout, program);
    // The store build's own `shim_env` (its sidecar, design S7): the dev link took the
    // plain names over with plain shims, and the restore lays them back as the signed
    // manifest declared them.
    let env = crate::shim_env::read_sidecar(&build_dir);
    crate::activate::install_tools_env(layout, &build_dir, &tools, aliases, &env)?;
    Ok(Restored::Laid(tools.len()))
}

/// The `alab-<tool>` alias policy for `program` on a verb that fetches no index: the §14
/// cached index's signed entry decides ([`crate::activate::Aliases::for_program`]) when
/// one is cached; with nothing cached, the aliases already laid on disk are the policy
/// ([`crate::activate::Aliases::laid_for`]) — so a vendor tool never grows an alias on
/// this path unless a signed entry said it was ALab's, and an ALab tool a pre-alias client
/// installed keeps whatever the pass has laid so far.
fn alias_policy_offline(layout: &crate::store::Layout, program: &str) -> crate::activate::Aliases {
    match cached_index(layout) {
        Some(index) => crate::activate::Aliases::for_program(program, index.program(program)),
        None => crate::activate::Aliases::laid_for(layout, program),
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
