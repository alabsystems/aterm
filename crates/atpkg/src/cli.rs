// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg` toolchain package manager CLI verb dispatch and formatting.

use std::process::ExitCode;

use crate::flow::now_unix;

/// The dispatch roster, public so the manual (a sibling crate) can be pinned against the verbs that actually run.
#[must_use]
pub const fn dispatch_roster() -> &'static [&'static str] {
    &VERBS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    Daily,
    Occasional,
    Plumbing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arity {
    None,
    One { flags: &'static [&'static str] },
    Flags { flags: &'static [&'static str] },
    Own,
}

struct Verb {
    name: &'static str,
    tier: Tier,
    usage: &'static str,
    arity: Arity,
    program_flags: Option<&'static [&'static str]>,
    mutates_store: bool,
    applies_machine: bool,
    run: fn(&[String]) -> ExitCode,
}

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
        usage: "aterm pkg update [program] [--retry]  (a program not installed is installed; \
                --retry: try a held-back download again now)\n\
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
        usage: "aterm pkg run <tool> [--] [args…]  — one leading `--` is dropped",
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
        usage: "aterm pkg gc                — reclaim superseded builds (the live build, one \
                rollback and any build a run holds stay) and what interrupted installs left: \
                unfinished builds, leftover downloads, unused exec-root copies",
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
        arity: Arity::Flags { flags: &["--json"] },
        program_flags: None,
        mutates_store: true,
        applies_machine: false,
        run: |rest| run_repair(layout(), rest.iter().any(|a| a == "--json")),
    },
    Verb {
        name: "machine",
        tier: Tier::Occasional,
        usage: "aterm pkg machine                 — (macOS) the [machine] settings, as the \
                doctor reads them\n\
                aterm pkg machine apply           — apply them now (by default Universal \
                Control off for this host, Rosetta 2 on Apple silicon), then reclaim what \
                `machine cache` lists\n\
                aterm pkg machine cache           — what the apply would take from the Rust \
                build cache and the tooling cache, and why; removes nothing\n\
                aterm pkg machine cache clean     — reclaim only that, without applying host \
                settings",
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
                completions, the PATH block, the Linux desktop entry and its icons), each only \
                when it is aterm's",
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
        usage: "aterm pkg verify-pkg <master-pubkey-b64> <pkg-*.toml> <pkg-*.toml.sig> \
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
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        run: cmd_lease,
    },
    Verb {
        name: "abrew",
        tier: Tier::Daily,
        usage: "aterm pkg abrew <brew-command> [args…]  — aterm's Homebrew (`aterm help abrew`): \
                relocation-free bottles and simple casks natively over abrew's own prefix, the rest delegated to real brew before effects; \
                abrew bundle lays the bundled Homebrew; abrew on|off toggles the `brew` reroute; \
                brew --real … runs real Homebrew unchanged",
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
                check one command line for stock Rust: exit 2 with the Trust spelling; 0 when \
                clean, when ATERM_STOCK_REASON='<why>' leads, or when the --json field is absent \
                or not a string; 1 on a bad argument, or on stdin that is unreadable, not UTF-8 \
                (not JSON for --json) or over 8 MiB",
        arity: Arity::Own,
        program_flags: None,
        mutates_store: false,
        applies_machine: false,
        run: cmd_lane,
    },
];

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

fn verb_row(name: &str) -> Option<&'static Verb> {
    TABLE.iter().find(|v| v.name == name)
}

fn program_flags(verb: &str) -> Option<&'static [&'static str]> {
    verb_row(verb).and_then(|v| v.program_flags)
}

/// The usage line for `verb`, if it has one.
#[must_use]
pub fn verb_usage(verb: &str) -> Option<&'static str> {
    verb_row(verb).map(|v| v.usage)
}

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

fn cmd_verb_help(verb: &str) -> ExitCode {
    match verb_usage(verb) {
        Some(usage) => {
            println!("usage: {usage}");
            println!("atpkg: full manual: aterm help pkg");
            ExitCode::SUCCESS
        }
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

const TIERS: [(&str, Tier); 3] = [
    ("daily", Tier::Daily),
    ("occasional", Tier::Occasional),
    ("plumbing", Tier::Plumbing),
];

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

fn print_verb_tiers(out: &mut impl std::io::Write) {
    for line in verb_tier_lines() {
        let _ = writeln!(out, "{line}");
    }
}

const VERB_SYNONYMS: &[(&str, &str)] = &[("remove", "uninstall"), ("rm", "uninstall")];

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

/// The whole package-manager CLI as a callable.
pub fn main_entry(mut argv: Vec<std::ffi::OsString>) -> ExitCode {
    // The private window's lead ([`PRIVATE_STATE_ROOT_FLAG`]), taken off before anything
    // reads the first word as the verb: this pass carries no `[machine]` edit, whatever this
    // build's seams say.
    if argv.first().and_then(|v| v.to_str()) == Some(PRIVATE_STATE_ROOT_FLAG) {
        argv.remove(0);
        PRIVATE_STATE_ROOT_LEAD.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if argv.first().and_then(|v| v.to_str()) == Some(crate::warm::HIDDEN_VERB) {
        return crate::warm::run_hidden(&argv[1..]);
    }
    if argv.first().and_then(|v| v.to_str()) == Some(crate::devtools::PRIME_VERB) {
        return crate::devtools::run_prime();
    }
    let mut args: Vec<String> = argv
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    PUBLISHED_INDEX_HINT.with(|hint| hint.set(None));
    if matches!(
        args.first().map(String::as_str),
        Some("seed" | "update" | "install")
    ) {
        strip_progress_file_flag(&mut args);
        if take_flag(&mut args, HEAD_WATCH_FLAG) {
            HEAD_WATCH_PASS.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(bound) = take_wait_lock_flag(&mut args) {
            configure_wait_lock(&args, bound);
        }
        if args.first().is_some_and(|verb| verb == "update") {
            PUBLISHED_INDEX_HINT.with(|hint| hint.set(take_published_index_hint_flag(&mut args)));
            if take_flag(&mut args, DEFER_BUSY_FLIP_FLAG) {
                DEFER_BUSY_FLIP.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    let verb = args.first().map(String::as_str);
    if verb == Some("__pending") {
        return cmd_pending(args.get(1), args.get(2..).unwrap_or(&[]));
    }
    if verb == Some(crate::selfupdate::HIDDEN_VERB) {
        return cmd_selfupdate(&args[1..]);
    }
    if verb == Some(crate::landing::HIDDEN_VERB) {
        return cmd_landing(&args[1..]);
    }
    if verb == Some(crate::reroute::HIDDEN_VERB) {
        return cmd_reroute(&args[1..]);
    }
    if verb == Some(crate::abrew::stub::HIDDEN_VERB) {
        return crate::abrew::rerouted_entry(&args[1..]);
    }
    if matches!(verb, Some("help" | "-h" | "--help")) {
        // `atpkg help <verb>` answers for that verb, as `aterm help <topic>` does; an
        // unknown topic is a usage error ([`cmd_verb_help`]).
        if let Some(topic) = args.get(1) {
            return cmd_verb_help(topic);
        }
        return cmd_help();
    }
    if let Some(v) = verb
        && verb_usage(v).is_some()
        && help_flag_addresses_atpkg(v, &args[1..])
    {
        return cmd_verb_help(v);
    }
    if let Some(v) = verb
        && let Some(operand) = args.get(1)
        && operand.starts_with('-')
        && let Some(allowed) = program_flags(v)
        && !allowed.contains(&operand.as_str())
    {
        let problem = format!("unknown option {operand:?} for `aterm pkg {v}`");
        let refusal = usage_error(v, Some(&problem));
        if !allowed.is_empty() {
            eprintln!("atpkg:   {v} accepts: {}", allowed.join(", "));
        }
        return refusal;
    }
    if let Some(v) = verb
        && let Some(refusal) = arity_refusal(&args[1..], v)
    {
        return refusal;
    }
    if let Some(v) = verb
        && let Some(answer) = built_in_answer(v, &args[1..])
    {
        return answer;
    }
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
    let pass_layout = store_lock.as_ref().map(crate::lock::StoreLock::layout);
    #[cfg(target_os = "macos")]
    let agents_before = pass_layout.clone().map(|layout| {
        let targets = crate::activation_notice::agent_targets(&layout);
        crate::activation_notice::prepare(&layout);
        (layout, targets)
    });
    let _status_record = pass_layout
        .clone()
        .map(|layout| crate::status::hold(&layout));
    let pass_record = pass_layout
        .clone()
        .map(|layout| PassRecord::begin(layout, &args));
    if matches!(verb, Some("seed" | "update" | "install"))
        && let Some(layout) = pass_layout.clone()
    {
        roll_forward_interrupted_activations(&layout, || cached_index(&layout));
    }
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

fn run_verb(verb: Option<&str>, args: &[String]) -> ExitCode {
    let Some(name) = verb else {
        cmd_bare();
        return ExitCode::SUCCESS;
    };
    if let Some(row) = verb_row(name) {
        return (row.run)(&args[1..]);
    }
    match did_you_mean(name) {
        Some(near) => eprintln!("atpkg: unknown verb '{name}' — did you mean `{near}`?"),
        None => eprintln!("atpkg: unknown verb '{name}'"),
    }
    print_verb_tiers(&mut std::io::stderr());
    eprintln!("atpkg: full manual: aterm help pkg");
    ExitCode::from(2)
}

struct PassRecord {
    layout: crate::store::Layout,
    lane: &'static str,
    verb: String,
    started: std::time::Instant,
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

fn pass_verb_words(args: &[String]) -> String {
    args.iter().take(2).cloned().collect::<Vec<_>>().join(" ")
}

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

fn exit_code_u8(code: ExitCode) -> u8 {
    (0..=u8::MAX)
        .find(|n| {
            let probe: ExitCode = (*n).into();
            probe == code
        })
        .unwrap_or(1)
}

fn cmd_help() -> ExitCode {
    println!(
        "aterm pkg \u{2014} installs and updates the ALab toolchain and the managed agent CLIs"
    );
    println!("{}", aterm_types::identity::ORIGIN_LINE);
    println!("usage: aterm pkg <command> [args\u{2026}]");
    print_verb_tiers(&mut std::io::stdout());
    println!("new machine: aterm pkg install --default-set");
    println!("one command: aterm pkg help <command>");
    println!("full manual: aterm help pkg");
    ExitCode::SUCCESS
}

fn not_installed_fix(name: &str) -> String {
    format!("atpkg: {name} is not installed (fix: aterm pkg install {name})")
}

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

/// A live build of a program one of whose shims is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LostShim {
    /// The build `store/<program>/current` selects.
    pub build: u64,
    /// Where the missing shim belongs.
    pub shim: std::path::PathBuf,
    /// The build's own binary, `store/<program>/<build>/bin/<tool>`, which stands.
    pub binary: std::path::PathBuf,
}

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

/// Whether `tool` is an installed program whose live build holds `bin/<tool>` while the managed shim `bin/<tool>` is absent, and if so which build and binary.
#[must_use]
pub fn live_without_shim(layout: &crate::store::Layout, tool: &str) -> Option<LostShim> {
    live_without_shim_of(layout, tool, tool)
}

/// [`live_without_shim`] for `tool`, one of the binaries `program` ships (the reroute's question for a branded tool such as `trustfmt`).
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
            _ => return None,
        },
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

fn lost_shim_run_notice(tool: &str, lost: &LostShim) -> String {
    format!(
        "atpkg: {tool} {} is installed but {} is missing — running it from the store (fix: \
         aterm pkg repair)",
        lost.build,
        lost.shim.display()
    )
}

#[derive(Debug, PartialEq, Eq)]
enum RunTarget {
    Exec {
        path: std::path::PathBuf,
        env: crate::shim_env::ShimEnv,
        notice: Option<String>,
    },
    Pending,
    LostNotRun(String),
    NotInstalled,
}

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
        path: crate::compat::route_for_shim(&lost.shim, &lost.binary)
            .unwrap_or_else(|| lost.binary.clone()),
        env: crate::shim_env::read_sidecar(&layout.build_dir(tool, lost.build)),
        notice: Some(lost_shim_run_notice(tool, &lost)),
    }
}

static PROGRESS_FILE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

fn strip_progress_file_flag(args: &mut Vec<String>) {
    while let Some(i) = args.iter().position(|a| a == "--progress-file") {
        args.remove(i);
        if i < args.len() {
            let path = args.remove(i);
            let _ = PROGRESS_FILE.set(std::path::PathBuf::from(path));
        }
    }
}

fn progress_path() -> Option<&'static std::path::Path> {
    PROGRESS_FILE.get().map(std::path::PathBuf::as_path)
}

static TTY_PROGRESS: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

fn tty_progress_path() -> Option<&'static std::path::Path> {
    TTY_PROGRESS.get().map(std::path::PathBuf::as_path)
}

fn pass_progress_path() -> Option<&'static std::path::Path> {
    progress_path().or_else(tty_progress_path)
}

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

/// The flag the head watch passes with `update <program>`.
pub const HEAD_WATCH_FLAG: &str = "--head-watch";

/// The window's untrusted published-index wake, carried to a whole update pass only as scheduling evidence.
pub const PUBLISHED_INDEX_HINT_FLAG: &str = "--published-index-hint";

thread_local! {
    static PUBLISHED_INDEX_HINT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

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

static HEAD_WATCH_PASS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|a| a != flag);
    args.len() != before
}

fn head_watch_pass() -> bool {
    HEAD_WATCH_PASS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The flag the unattended lanes (the window's package loop, a terminal session's detached pass) put on every whole `update`.
pub const DEFER_BUSY_FLIP_FLAG: &str = "--defer-busy-flip";

/// The word a window under a private state root puts ahead of the verb of every pass it spawns.
pub const PRIVATE_STATE_ROOT_FLAG: &str = "--private-state-root";

static PRIVATE_STATE_ROOT_LEAD: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn under_private_state_root() -> bool {
    PRIVATE_STATE_ROOT_LEAD.load(std::sync::atomic::Ordering::Relaxed)
        || aterm_types::dirs::runs_under_private_state_root()
}

static DEFER_BUSY_FLIP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn flip_policy() -> crate::quiet::FlipPolicy {
    if DEFER_BUSY_FLIP.load(std::sync::atomic::Ordering::Relaxed) {
        crate::quiet::FlipPolicy::WhenQuiet
    } else {
        crate::quiet::FlipPolicy::Now
    }
}

#[derive(Clone, Copy, Debug)]
struct WaitLock {
    bound: std::time::Duration,
    spawner: Option<u32>,
}

fn configure_wait_lock(args: &[String], bound: std::time::Duration) {
    let _ = WAIT_LOCK.set(WaitLock {
        bound,
        spawner: spawner_pid(),
    });
    if args.first().is_some_and(|verb| verb == "update")
        && let Some(layout) = crate::store::resolve_configured()
    {
        let _ = QUEUED_AT_PASS_SEQ.set(crate::status::pass_seq(&layout));
    }
    let log = crate::store::resolve_configured()
        .map(|layout| layout.prefix.join(crate::progress::ORPHAN_LOG_NAME))
        .unwrap_or_else(|| std::env::temp_dir().join(crate::progress::ORPHAN_LOG_NAME));
    crate::progress::watch_for_orphaning(log);
}

static WAIT_LOCK: std::sync::OnceLock<WaitLock> = std::sync::OnceLock::new();

static WAITED_FOR_LOCK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

static QUEUED_AT_PASS_SEQ: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

fn unix_now() -> i64 {
    aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX))
}

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

/// How a full pass that stood down exits ([`stand_down_exit`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandDown {
    /// The one line ([`stood_down_after_wait`]).
    pub line: String,
    /// Whether it goes to stderr.
    pub on_stderr: bool,
    /// The exit code.
    pub code: u8,
}

/// The stand-down exits as the pass it stood behind did.
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

/// The environment variable a spawner sets beside `--wait-lock` to its own pid, so the waiter's "my window is gone" compares against a fact.
pub const SPAWNER_PID_ENV: &str = "ATPKG_SPAWNER_PID";

/// The [`SPAWNER_PID_ENV`] value of a pass detached on purpose.
pub const SPAWNER_DETACHED: &str = "detached";

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

fn spawner_pid() -> Option<u32> {
    spawner_from(
        std::env::var(SPAWNER_PID_ENV).ok().as_deref(),
        current_parent_id,
    )
}

fn spawner_from(said: Option<&str>, parent: impl FnOnce() -> Option<u32>) -> Option<u32> {
    match said.map(str::trim) {
        Some(SPAWNER_DETACHED) => None,
        said => said.and_then(|v| v.parse::<u32>().ok()).or_else(parent),
    }
}

const fn is_orphaned(spawner: Option<u32>, now: Option<u32>) -> bool {
    matches!((spawner, now), (Some(spawner), Some(now)) if now != spawner)
}

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

fn seed_promise_owner() -> Option<u32> {
    progress_path().and(wait_lock())?.spawner
}

fn wait_lock() -> Option<WaitLock> {
    WAIT_LOCK.get().copied()
}

fn note_finished(program: &str, phase: crate::progress::Phase, error: Option<String>) {
    if let Some(sink) = crate::progress::active() {
        sink.finished(program, phase, error);
    }
}

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

/// The copy an agent program's pending stub runs meanwhile.
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

fn cmd_landing(rest: &[String]) -> ExitCode {
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
    let mut command = crate::landing::shim_command(&layout, &tool, hand_over.args);
    let err = crate::platform::exec_or_run(&mut command);
    eprintln!(
        "atpkg: could not run {}: {err}",
        command.get_program().to_string_lossy()
    );
    ExitCode::from(126)
}

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

#[derive(Debug)]
enum SelfUpdatePlan {
    Refuse(String),
    Disabled(String),
    Forward {
        command: Box<aterm_uds::spawnfd::StdCommand>,
        note: Option<String>,
    },
    Check {
        row: &'static crate::vendor_direct::VendorSpec,
    },
}

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
    match selfupdate::classify(row, args) {
        Verdict::Help => return forward(Some(selfupdate::help_line(row))),
        Verdict::PassThrough => return forward(None),
        Verdict::Declined(d) => {
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
        Verdict::Check { .. } => {}
    }
    if !enabled {
        let build = crate::ops::active_builds(layout).get(row.program).copied();
        return SelfUpdatePlan::Disabled(selfupdate::disabled_line(row, build));
    }
    SelfUpdatePlan::Check { row }
}

fn selfupdate_check(
    layout: &crate::store::Layout,
    row: &crate::vendor_direct::VendorSpec,
    atpkg: &std::path::Path,
    bound: u64,
) -> ExitCode {
    use std::io::Write as _;
    let active = crate::ops::active_builds(layout).get(row.program).copied();
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

fn selfupdate_child_binary() -> std::path::PathBuf {
    let co_located = crate::stub::co_located_atpkg_path();
    if crate::stub::is_live_atpkg(&co_located) {
        return co_located;
    }
    std::env::current_exe().unwrap_or(co_located)
}

fn cmd_reroute(rest: &[String]) -> ExitCode {
    crate::notice::silence();
    let Some((upstream, args)) = rest.split_first() else {
        eprintln!(
            "usage: atpkg {} <upstream> [args…]",
            crate::reroute::HIDDEN_VERB
        );
        return ExitCode::from(2);
    };
    let Some(layout) = layout() else {
        return crate::reroute::run_storeless(upstream, args);
    };
    crate::reroute::run(&layout, upstream, args)
}

fn reassert_rustup_seam(layout: &crate::store::Layout) {
    reassert_rustup_seam_at(layout, crate::seam::rustup_home().as_deref());
}

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

fn detach_rustup_seams(layout: &crate::store::Layout) {
    let Some(home) = crate::seam::rustup_home() else {
        return;
    };
    for event in crate::seam::detach_recorded(layout, &home) {
        println!("atpkg: {event}");
    }
}

fn lay_reroute_stubs(layout: &crate::store::Layout) {
    // abrew's `brew` stub rides the same seams (docs/DESIGN-abrew-2026-10-19.md §1);
    // `reroute::lay` lays it too, so every spawn seam is covered by this one call.
    if let Err(err) = crate::reroute::lay(layout) {
        eprintln!(
            "atpkg: warn — reroute stubs not laid under {}: {err} (an aterm session lays \
             them at spawn; `aterm pkg repair` retries)",
            crate::reroute::dir(layout).display()
        );
    }
}

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

/// The managed state a surface names for a vendor-direct program at `build`.
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

fn run_pending(layout: &crate::store::Layout, tool: &str, passthrough: &[String]) -> ExitCode {
    use std::io::IsTerminal as _;
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let onward = pending_passthrough(layout, tool, std::env::var_os("PATH").as_deref());
    let show = if interactive && crate::meter::wanted() {
        WaitShow::Meter
    } else {
        WaitShow::Lines
    };
    let asker = if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        crate::progress::Asker::Person
    } else {
        crate::progress::Asker::Script
    };
    let mut lines = Vec::new();
    let mut say = |line: String| lines.push(line);
    let mut io = PendingIo {
        say: &mut say,
        wait: onward.is_none().then_some(show),
        asker,
    };
    let observed_at = crate::flow::now_unix().max(0).unsigned_abs();
    let initial = crate::progress::read_progress(layout);
    let initial_running = initial
        .as_ref()
        .is_some_and(|file| crate::progress::snapshot_running(file, observed_at));
    let next = pending_state(layout, tool, &mut io);
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
            let _ = passthrough;
            return ExitCode::from(127);
        }
    }
    ExitCode::from(127)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitStep {
    Run,
    Fail,
    Wait,
}

const fn pending_wait_step(running: bool, installed: bool, failed: bool) -> WaitStep {
    if installed {
        WaitStep::Run
    } else if failed || !running {
        WaitStep::Fail
    } else {
        WaitStep::Wait
    }
}

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
            None => initial.is_some_and(|first| {
                generation(first).is_none()
                    && initial_running
                    && first.pass == file.pass
                    && first.started_unix == file.started_unix
            }),
        }
    })
}

const PENDING_WAIT_POLL: std::time::Duration = std::time::Duration::from_millis(250);

const PENDING_UNATTENDED_WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(15 * 60);

const PENDING_UNATTENDED_SAY_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

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

#[derive(Clone, Copy)]
struct WaitSeen<'a> {
    snapshot: Option<&'a crate::progress::ProgressFile>,
    running: bool,
    promise: bool,
}

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
    let mut planned = false;
    let mut promised = seen.promise;
    let mut witness = seen.snapshot.cloned();
    let mut witness_running = seen.running;
    let outcome = loop {
        let now = crate::flow::now_unix().max(0).unsigned_abs();
        let owed = crate::progress::pass_owed(layout, now);
        let snapshot = crate::progress::read_progress(layout);
        let running = snapshot
            .as_ref()
            .is_some_and(|f| crate::progress::snapshot_running(f, now));
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
        Err(None) => {
            if let Some(program) = crate::store::ToolName::new(program) {
                pending_not_running(layout, &program, asker, &mut |line| eprintln!("{line}"));
            }
            return ExitCode::from(127);
        }
    }
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

fn landed_without_line(layout: &crate::store::Layout, program: &str, tool: &str) -> String {
    let mut provides = crate::installed_exposes(layout, program).unwrap_or_default();
    provides.sort();
    let mut line = format!("atpkg: {program} is installed, and it has no command named {tool}");
    if !provides.is_empty() {
        line.push_str(&format!(" \u{2014} it provides {}", provides.join(", ")));
    }
    line
}

const PENDING_ERROR_CAP: usize = 300;

const HELD_WHY_CAP: usize = 200;

const HELD_HOW_CAP: usize = 2 * PENDING_ERROR_CAP;

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

struct PendingIo<'a> {
    say: &'a mut dyn FnMut(String),
    wait: Option<WaitShow>,
    asker: crate::progress::Asker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitShow {
    Meter,
    Lines,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingNext {
    Done,
    WaitThenRun,
    WaitForPromise,
}

fn pending_state(layout: &crate::store::Layout, tool: &str, io: &mut PendingIo<'_>) -> PendingNext {
    let Some(tn) = crate::store::ToolName::new(tool) else {
        (io.say)("atpkg: that is not an installable program name".to_string());
        return PendingNext::Done;
    };
    let typed = tn.as_str();
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
    let owed = crate::progress::pass_owed(layout, now);
    let snapshot = crate::progress::read_progress(layout);
    let running = snapshot.as_ref().is_some_and(|f| {
        crate::progress::snapshot_running(f, now)
            && (f.programs.contains_key(name)
                || (!owed && matches!(f.pass.as_str(), "net" | "seed")))
    });
    if let Some(file) = snapshot.as_ref().filter(|_| running) {
        if file.v != crate::progress::PROGRESS_VERSION {
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
            (io.say)(not_in_default_set_line(name));
            return PendingNext::Done;
        };
        use crate::progress::Phase;
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
            Phase::Skipped => match row.error.as_deref() {
                Some(why) => (io.say)(format!(
                    "atpkg: {name} was SKIPPED by this pass: {} — `aterm pkg install \
                         {name}` tries it alone",
                    crate::progress::sanitize_for_tty(why, PENDING_ERROR_CAP)
                )),
                None => (io.say)(format!(
                    "atpkg: {name} was SKIPPED by this pass — already current, or not in \
                         this pass. Re-run it; if it still does not run: aterm pkg install {name}"
                )),
            },
        }
        return PendingNext::Done;
    }
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

fn not_in_default_set_line(name: &str) -> String {
    format!(
        "atpkg: {name} is no longer part of the default set — to install it: aterm pkg \
         install {name}"
    )
}

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

fn pending_not_running(
    layout: &crate::store::Layout,
    program: &crate::store::ToolName,
    asker: crate::progress::Asker,
    say: &mut dyn FnMut(String),
) {
    let name = program.as_str();
    if let Some(status) = crate::status::read(layout) {
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
        if let Some(row) = status.programs.get("*toolset*")
            && let Some(why) = row.state.strip_prefix("blocked: ")
        {
            say(format!(
                "atpkg: {name} is not coming on its own: {} — Settings ▸ Packages \
                 shows details.",
                crate::progress::sanitize_for_tty(why, PENDING_ERROR_CAP)
            ));
            let _ = crate::progress::append_bump(layout, program, asker);
            return;
        }
    }
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

fn print_unreachable_followup(e: &crate::FlowError, rerun: &str) {
    let (crate::FlowError::Unreachable(why) | crate::FlowError::VendorUnreachable(why)) = e else {
        return;
    };
    eprintln!("atpkg:   when the connection is back, re-run: {rerun}");
    if why.contains("403") || why.to_ascii_lowercase().contains("rate limit") {
        eprintln!("atpkg:   the anonymous GitHub limit resets within the hour");
    }
}

fn layout() -> Option<crate::store::Layout> {
    match crate::store::resolve_configured() {
        Some(l) => Some(l),
        None => {
            eprintln!("atpkg: HOME is unset — cannot locate the install prefix");
            None
        }
    }
}

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

fn zero_arity(rest: &[String], verb: &str) -> Option<ExitCode> {
    let stray = rest.first()?;
    let problem = format!("unknown argument {stray:?} — this verb takes none");
    Some(usage_error(verb, Some(&problem)))
}

/// `update <program> --vendor`.
pub(crate) const SOURCE_VENDOR_FLAG: &str = "--vendor";

/// `update <program> --alab`.
pub(crate) const SOURCE_ALAB_FLAG: &str = "--alab";

fn arity_refusal(rest: &[String], verb: &str) -> Option<ExitCode> {
    match verb_row(verb)?.arity {
        Arity::None => zero_arity(rest, verb),
        Arity::One { flags } => {
            let operands = rest.iter().filter(|a| !flags.contains(&a.as_str())).count();
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

fn verb_mutates_store(verb: &str) -> bool {
    verb_row(verb).is_some_and(|v| v.mutates_store)
}

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
    let announced = std::cell::Cell::new(false);
    let spinner = std::cell::RefCell::new(None::<crate::meter::TtyMeter>);
    let person = wait_lock().is_none() && a_person_typed_this();
    let acquired = match wait_lock() {
        Some(WaitLock { bound, .. }) => crate::lock::lock_store_waiting(
            &layout,
            bound,
            |path| {
                announced.set(true);
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

fn begin_store_mutation(guard: &mut crate::lock::StoreLock) {
    let layout = guard.layout();
    crate::landing::sweep(&layout);
    use std::io::IsTerminal as _;
    guard.announce_holder(
        &layout,
        wait_lock().is_none() && std::io::stdin().is_terminal(),
    );
}

const TYPED_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

fn a_person_typed_this() -> bool {
    use std::io::IsTerminal as _;
    !cfg!(test) && std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn stdout_is_a_person() -> bool {
    use std::io::IsTerminal as _;
    !cfg!(test) && std::io::stdout().is_terminal()
}

fn lock_wait_sentence(layout: &crate::store::Layout) -> &'static str {
    if crate::lock::holder_is_person(layout) {
        "Waiting for another `aterm pkg` command to finish\u{2026} (Ctrl-C to stop)"
    } else {
        "Waiting for aterm's background update to finish\u{2026} (Ctrl-C to stop)"
    }
}

fn lock_wait_spinner(layout: &crate::store::Layout) -> Option<crate::meter::TtyMeter> {
    let text = lock_wait_sentence(layout).to_string();
    crate::meter::TtyMeter::start(move || crate::meter::Show::Busy(text.clone()))
}

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

/// Whether this `--wait-lock` waiter has been orphaned.
pub(crate) fn parent_is_gone() -> bool {
    wait_lock().is_some_and(|w| is_orphaned(w.spawner, current_parent_id()))
}

fn cmd_bare() {
    if !manager_enabled() {
        println!("atpkg: disabled (no root key pinned) -- inert");
        return;
    }
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
        Some((0, _)) => lines.push(
            "atpkg: next: aterm pkg install --default-set — the whole ALab toolset · \
             full manual: aterm help pkg"
                .to_string(),
        ),
        Some(_) => lines.push(
            "atpkg: next: aterm pkg list — what you have · full manual: aterm help pkg".to_string(),
        ),
        None => {}
    }
    lines
}

fn doctor(prefix: &str, detail: crate::doctor::Detail, json: bool) -> ExitCode {
    crate::notice::spelling_reported_here();
    let Some(layout) = layout() else {
        // Distinct branch.
        return ExitCode::from(1);
    };
    if crate::doctor::run(&layout, prefix, detail, json) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

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
    if let Some(target) = crate::which(layout, tool) {
        let Some((program, build)) = crate::ops::program_build_of_target(&target) else {
            return Ok(format!(
                "{tool} → {} → {} — dev-linked (managed from that checkout, not the index){}",
                shim.display(),
                target.display(),
                upstream_clause(tool, &target)
            ));
        };
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
            if let Some(row) = agent_shell_shadow(layout, &program, build, &shadow, path_var) {
                let mut line = format!("{tool} → {} — {row}", shadow.display());
                let foreign = crate::vendor::foreign_copies_on_path(&layout.prefix, tool, path_var);
                if !foreign.is_empty() {
                    line.push('\n');
                    line.push_str(&foreign_copies_line_with(FOREIGN_ON_THIS_SHELL, &foreign));
                }
                return Ok(line);
            }
            let state = crate::state::shadowed(build, &shadow);
            return Ok(match alias_fix(layout, &tn, &program, path_var) {
                Some(fix) => format!("{tool} → {} — {state} — {fix}", shadow.display()),
                None => format!("{tool} → {} — {state}", shadow.display()),
            });
        }
        let state = managed_state_of(layout, &program, build, row_of(&program).as_ref());
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
        let based = upstream_clause(&program, &target);
        let mut line = match shim_env_fix(&shim, &program) {
            Some(fix) => format!(
                "{tool} → {runs} → {} — {state}{based} — {fix}",
                target.display()
            ),
            None => format!("{tool} → {runs} → {} — {state}{based}", target.display()),
        };
        if let Some(row) = crate::selfupdate::row_for(&program)
            && agent_twin_intercepts(layout, &program, build)
            && (routed.is_some()
                || matches!(answer, AgentStub::Absent)
                    && crate::vendor::agents_dir_on_path(&layout.prefix, path_var))
        {
            line.push_str(" — ");
            line.push_str(&crate::selfupdate::which_sentence(row));
        }
        if crate::stub::is_agent_program(&program) {
            let foreign = crate::vendor::foreign_copies_on_path(&layout.prefix, tool, path_var);
            if !foreign.is_empty() {
                line.push('\n');
                line.push_str(&foreign_copies_line(&foreign));
            }
        }
        return Ok(line);
    }
    if let Some(lost) = live_without_shim(layout, tool) {
        let state = managed_state_of(layout, tool, lost.build, row_of(tool).as_ref());
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
    if crate::linkmode::is_linked(layout, tool) {
        return Ok(dev_linked_program_line(
            tool,
            crate::linkmode::linked_checkout(layout, tool).as_deref(),
            &crate::linkmode::linked_bins(layout, tool),
            crate::vendor::system_binary_on_path(&layout.prefix, tool, path_var).as_deref(),
        ));
    }
    let stubbed = crate::stub::pending_stub_exists(layout, tool);
    let index = cached_index(layout);
    let foreign = if stubbed {
        crate::vendor::shadowing_binary_on_path(&layout.prefix, tool, path_var)
    } else {
        crate::vendor::system_binary_on_path(&layout.prefix, tool, path_var)
    };
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
    if stubbed {
        let program = crate::stub::program_of_in(layout, tool);
        let program = program.as_str();
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
    if let Some(row) = row_of(tool)
        && (row.state.starts_with(crate::state::UNAVAILABLE_PREFIX)
            || row.state.starts_with(crate::state::HELD_FAILED_PREFIX))
    {
        return Ok(format!("{tool} → (nothing runs) — {}", row.state));
    }
    if let Some(base) = tn.alias_base() {
        return Err(no_alias_line(layout, tool, base.as_str()));
    }
    if let Some(line) =
        unserved_which_line(&crate::stub::program_of_in(layout, tool), index.as_deref())
    {
        return Err(line);
    }
    Err(not_runnable_line(layout, tool))
}

fn upstream_clause(program: &str, exe: &std::path::Path) -> String {
    crate::alab::agent(program)
        .and_then(|_| crate::alab::upstream_of_exe(exe))
        .map(|upstream| format!(" — {}", upstream.words()))
        .unwrap_or_default()
}

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

fn foreign_copies_line(foreign: &[std::path::PathBuf]) -> String {
    foreign_copies_line_with("foreign copies out-ranked", foreign)
}

const FOREIGN_ON_THIS_SHELL: &str = "foreign copies on this shell's PATH";

fn home_abbreviated(p: &std::path::Path) -> String {
    aterm_types::dirs::home_dir()
        .as_deref()
        .and_then(|h| p.strip_prefix(h).ok())
        .map_or_else(
            || p.display().to_string(),
            |rest| format!("~/{}", rest.display()),
        )
}

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

fn version_from_path(path: &std::path::Path) -> Option<String> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    version_from_components(&real)
}

fn version_from_components(path: &std::path::Path) -> Option<String> {
    path.components().rev().find_map(|c| {
        let s = c.as_os_str().to_str()?;
        let dotted = s.contains('.')
            && s.split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
        dotted.then(|| s.to_string())
    })
}

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

fn no_alias_line(layout: &crate::store::Layout, alias: &str, base: &str) -> String {
    let index = cached_index(layout);
    no_alias_line_for(
        layout,
        alias,
        base,
        index.as_ref().and_then(|i| i.program(base)),
    )
}

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

fn cmd_run(rest: &[String]) -> ExitCode {
    let Some((tool, args)) = rest.split_first() else {
        return usage_error("run", None);
    };
    let args: &[String] = match args.split_first() {
        Some((sep, tail)) if sep == "--" => tail,
        _ => args,
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let (target, env) = match run_target(&layout, tool) {
        RunTarget::Exec { path, env, notice } => {
            if let Some(notice) = notice {
                eprintln!("{notice}");
            }
            (path, env)
        }
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
    let stripped_nh = crate::yolo::strip_no_harness(tool, args);
    let no_harness = stripped_nh.is_some();
    let args = stripped_nh.as_deref().unwrap_or(args);
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
    #[expect(
        clippy::disallowed_types,
        reason = "aterm pkg run becomes the tool by exec: its caller's descriptors are the tool's"
    )]
    let mut command = std::process::Command::new(&target);
    aterm_uds::spawn::set_flags(&mut command, 0);
    command.args(args).env("PATH", child_path);
    if no_harness {
        command.env(crate::yolo::NO_HARNESS_ENV, "1");
    }
    for (name, value) in env.entries() {
        command.env(name, value);
    }
    let err = crate::platform::exec_or_run(&mut command);
    eprintln!("atpkg: failed to exec {}: {err}", target.display());
    ExitCode::from(127)
}

fn runs_self_update_verb(layout: &crate::store::Layout, tool: &str, args: &[String]) -> bool {
    args.first()
        .is_some_and(|first| crate::selfupdate::verbs_of(tool).contains(&first.as_str()))
        && crate::stub::is_agent_program(tool)
        && crate::which(layout, tool)
            .and_then(|target| crate::ops::store_build_of(&layout.prefix, &target))
            .is_some_and(|(program, _)| crate::stub::is_agent_program(&program))
}

#[derive(Debug, PartialEq, Eq)]
struct LeaseArgs {
    dir: std::path::PathBuf,
    who: Option<String>,
    command: Vec<String>,
}

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

fn lease_who(args: &LeaseArgs, pid: u32, cwd: &std::path::Path) -> String {
    format!(
        "{} (pid {pid}) \u{2014} held by `aterm pkg lease` in {}",
        lease_run_name(args),
        cwd.display()
    )
}

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

#[derive(Debug)]
enum LeaseRun {
    Ran(std::process::ExitStatus),
    Missing(crate::lease::Missing),
    NotStarted(std::io::Error),
    Lost(std::io::Error),
}

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
    let forwarding = crate::platform::arm_forwarding();
    let mut command = aterm_uds::spawnfd::transparent(&args.command[0]);
    let mut child = match command.args(&args.command[1..]).spawn() {
        Ok(child) => child,
        Err(e) => return LeaseRun::NotStarted(e),
    };
    let status = crate::platform::wait_standing_aside(&mut child, forwarding);
    drop(hold);
    match status {
        Ok(status) => LeaseRun::Ran(status),
        Err(e) => LeaseRun::Lost(e),
    }
}

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

fn cmd_lane(rest: &[String]) -> ExitCode {
    let args = match crate::lane::parse_args(rest) {
        Ok(args) => args,
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

/// One usage refusal: the specific complaint (`atpkg <verb>: <problem>`) when there is one, then the verb's grammar, exit 2.
pub(crate) fn usage_error(verb: &str, problem: Option<&str>) -> ExitCode {
    print_usage_error(verb, problem);
    ExitCode::from(2)
}

fn print_usage_error(verb: &str, problem: Option<&str>) {
    if let Some(problem) = problem {
        eprintln!("atpkg {verb}: {problem}");
    }
    if let Some(usage) = verb_usage(verb) {
        eprintln!("usage: {usage}");
    }
}

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

fn run_repair(layout: Option<crate::store::Layout>, json: bool) -> ExitCode {
    let Some(layout) = layout else {
        return ExitCode::from(1);
    };
    let ground = std::sync::Arc::new(crate::converge::repair::Ground::of_process(
        layout,
        crate::converge::repair::Pass::Repair,
    ));
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

fn repair_exit(rows: &[aterm_converge::ItemReport]) -> ExitCode {
    if rows.iter().any(repair_left_undone) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

const fn repair_left_undone(row: &aterm_converge::ItemReport) -> bool {
    use aterm_converge::Outcome;
    !matches!(
        row.outcome,
        Outcome::Ok | Outcome::Changed | Outcome::NotApplicable | Outcome::Deferred
    )
}

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

#[cfg(all(test, unix))]
fn repair_store(layout: &crate::store::Layout, leftovers: &[String]) -> ExitCode {
    let rows = repair_store_rows(layout, &REPAIR_STORE_STEPS);
    repair_store_verdict(&rows, leftovers)
}

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

#[cfg(all(test, unix))]
fn relay_shims(layout: &crate::store::Layout) -> (usize, Vec<String>, Vec<String>, Vec<String>) {
    let plan = crate::converge::repair::shim_plan(layout);
    let relaid = crate::converge::repair::relay(layout);
    for note in &relaid.notes {
        println!("repair: {note}");
    }
    (relaid.relaid, plan.missing, plan.revoked, relaid.failed)
}

#[cfg(all(test, unix))]
fn repair_exposes(
    layout: &crate::store::Layout,
    program: &str,
    build_dir: &std::path::Path,
) -> Vec<String> {
    crate::converge::repair::exposes(layout, program, build_dir)
}

fn program_disabled_here(
    layout: &crate::store::Layout,
    recorded: Option<&crate::Status>,
    program: &str,
) -> bool {
    recorded
        .and_then(|s| s.programs.get(program))
        .is_some_and(|r| r.state.starts_with("tombstoned:") || r.state.starts_with("aborted:"))
        || crate::installed_exposes(layout, program)
            .unwrap_or_else(|| vec![program.to_string()])
            .iter()
            .any(|tool| is_tombstone_shim(layout, tool))
}

/// Whether `bin/<tool>` is a tombstone.
pub(crate) fn is_tombstone_shim(layout: &crate::store::Layout, tool: &str) -> bool {
    let Some(tool) = crate::store::ToolName::new(tool) else {
        return false;
    };
    let shim = layout.shim(&tool);
    crate::platform::resolve_shim(&shim).is_none()
        && crate::metadata_io::read_bounded_regular_utf8(&shim, crate::platform::MAX_SHIM_BYTES)
            .is_ok_and(|body| body.contains(" was yanked/revoked"))
}

fn run_list(layout: Option<crate::store::Layout>, human: bool) -> ExitCode {
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

fn list_lines(layout: &crate::store::Layout, human: bool) -> Result<Vec<String>, String> {
    let installed = crate::list_installed(layout);
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

const LIST_NEXT_DOCTOR: &str = "atpkg: next — aterm pkg doctor";

const EMPTY_LIST_HUMAN: &str =
    "atpkg: no programs installed — aterm pkg install --default-set installs the ALab toolset";

const EMPTY_UPDATE: &str = "atpkg: nothing installed to update — aterm pkg install \
                            --default-set installs the ALab toolset";

const EMPTY_VERIFY: &str = "atpkg: nothing installed to verify (aterm pkg install \
                            --default-set installs the toolset)";

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

fn list_human_lines(
    installed: &[(String, u64)],
    linked_only: &[String],
    live: &std::collections::BTreeMap<String, u64>,
    linked: &std::collections::BTreeMap<String, Option<std::path::PathBuf>>,
    pinned: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let mut builds: std::collections::BTreeMap<&str, Vec<u64>> = std::collections::BTreeMap::new();
    for (program, build) in installed {
        builds.entry(program).or_default().push(*build);
    }
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

fn uninstall_and_retire(layout: &crate::store::Layout, program: &str) -> std::io::Result<()> {
    retirement_name_gate(program)?;
    record_removed(layout, program);
    layout.clear_retired(program);
    if program == "trust" {
        detach_rustup_seams(layout);
    }
    crate::uninstall(layout, program)?;
    let _ = crate::quiet::end_outlived(layout);
    clear_status_row(layout, program);
    Ok(())
}

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

fn uninstall_one(layout: &crate::store::Layout, program: &str) -> ExitCode {
    // Refuse to "uninstall" what was never there: a success would mint a durable removed
    // marker that suppresses set-completion for a program the user never had. A
    // sentinel/flag name reaches the retirement gate's own refusal instead.
    if !(is_sentinel_row(program) || program.starts_with('-')) && !program_present(layout, program)
    {
        eprintln!("atpkg: {program} is not installed — nothing to uninstall");
        return ExitCode::from(1);
    }
    let opted_out = retirement_name_gate(program).is_ok() && adopted(layout);
    if opted_out {
        clear_adoption(layout);
    }
    match uninstall_and_retire(layout, program) {
        Ok(()) => {
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

fn foreign_copy_note(state: &str) -> Option<String> {
    crate::state::system_path(state).map(|path| format!("(system copy: {path})"))
}

#[derive(Default)]
struct UninstallSweep {
    removed: Vec<String>,
    left_alone: Vec<String>,
    failures: u32,
}

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

fn sweep_uninstall_targets(
    layout: &crate::store::Layout,
    targets: &std::collections::BTreeSet<String>,
) -> UninstallSweep {
    let rows = crate::status::read(layout)
        .map(|s| s.programs)
        .unwrap_or_default();
    let mut sweep = UninstallSweep::default();
    for program in targets {
        let state = rows.get(program).map(|r| r.state.as_str()).unwrap_or("");
        if !program_present(layout, program) {
            match foreign_copy_note(state) {
                Some(note) => sweep.left_alone.push(format!("{program} {note}")),
                None => clear_status_row(layout, program),
            }
            continue;
        }
        match crate::uninstall(layout, program) {
            Ok(()) => {
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

fn print_left_alone(left_alone: &[String]) {
    if left_alone.is_empty() {
        return;
    }
    println!(
        "atpkg: left alone — not installed by aterm, so nothing was removed: {}",
        left_alone.join(", ")
    );
}

fn cmd_uninstall_all() -> ExitCode {
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let targets = uninstall_all_targets(&layout);
    detach_rustup_seams(&layout);
    if targets.is_empty() {
        clear_adoption(&layout);
        record_decline(&layout);
        layout.clear_all_retired();
        let _ = crate::quiet::end_outlived(&layout);
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
    let report = crate::gc::run(&layout, last_verified_channel(&layout).as_ref());
    print_gc_sweeps("uninstall --all", &report);
    print_gc_abstentions("uninstall --all", &report);
    crate::hooks::refresh_creating_no_rc(&layout);
    clear_adoption(&layout);
    record_decline(&layout);
    layout.clear_all_retired();
    let _ = crate::quiet::end_outlived(&layout);
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

fn remove_exec_roots(layout: &crate::store::Layout) {
    if let Err(e) = crate::compat::remove_all(layout) {
        eprintln!(
            "atpkg: warn — {} was not removed ({e}); `aterm pkg gc` sweeps any trust exec \
             root left in it",
            crate::compat::compat_dir(layout).display()
        );
    }
}

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

const fn gc_freed_nothing(report: &crate::gc::GcReport) -> bool {
    report.reclaimed.is_empty()
        && report.swept_partial.is_empty()
        && report.swept_scratch.is_empty()
        && report.swept_staging.is_empty()
        && report.swept_exec_roots.is_empty()
}

fn print_gc_sweeps(verb: &str, report: &crate::gc::GcReport) {
    for line in gc_sweep_lines(verb, report) {
        println!("{line}");
    }
    for e in &report.exec_root_errors {
        eprintln!("atpkg {verb}: warn — {e}");
    }
}

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
    for (p, names) in &report.swept_staging {
        lines.push(format!(
            "atpkg {verb}: removed leftover download(s) of {p}: {}",
            names.join(", ")
        ));
    }
    for path in &report.swept_exec_roots {
        lines.push(format!(
            "atpkg {verb}: removed unused build copy {}",
            path.display()
        ));
    }
    for (p, build, holders) in &report.leased {
        lines.push(format!(
            "atpkg {verb}: kept {} \u{2014} {}; the first pass after that run reclaims it",
            gc_builds_words(p, &[*build]),
            holders.clause()
        ));
    }
    for (p, asset) in &report.held_staging {
        lines.push(format!(
            "atpkg {verb}: kept staged {p} archive {asset} \u{2014} the held Trust toolchain \
             update installs from it once nothing is using the toolchain; `aterm pkg update` \
             installs it now"
        ));
    }
    lines
}

fn scratch_words(name: &str) -> String {
    crate::store::stage_scratch_of(name).map_or_else(
        || name.to_string(),
        |(build, _)| crate::vendor_direct::build_words(build),
    )
}

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
pub(crate) const fn current_triple() -> &'static str {
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

fn now_rfc3339() -> String {
    rfc3339_at(now_unix())
}

fn rfc3339_at(secs: i64) -> String {
    // Both ends mean an unusable clock: `now_unix` returns `i64::MAX` when it cannot be read
    // (the freshness gates' fail-closed sentinel), and that would stamp a far-future year
    // doctor's "publishing looks frozen" check reads as fresh.
    if secs <= 0 || secs == i64::MAX {
        return String::new();
    }
    aterm_types::rfc3339::format_rfc3339(secs as u64)
}

fn reusable_index(index: Option<&crate::TrustedIndex>, now: i64) -> Option<&crate::TrustedIndex> {
    index.filter(|index| crate::flow::index_is_fresh(index, now))
}

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
    let unattested: Vec<(String, u64)> = live
        .iter()
        .filter(|(program, _)| !crate::vendor_direct::is_vendor(program))
        .filter(|(program, build)| {
            recorded
                .get(program.as_str())
                .is_none_or(|row| row.tree_root.is_empty() || row.installed_build != Some(**build))
        })
        .map(|(program, build)| (program.clone(), *build))
        .collect();
    let maybe_stale = recorded
        .keys()
        .any(|k| !is_sentinel_row(k) && !live.contains_key(k.as_str()));
    if unattested.is_empty() && !maybe_stale {
        return;
    }
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

fn failed_install_state(e: &crate::FlowError) -> Option<String> {
    if let Some(state) = canonical_non_error_state(e) {
        return Some(state);
    }
    match e {
        crate::FlowError::Linked(_) => Some(String::from("dev-linked (skipped)")),
        crate::FlowError::NotReachable(..)
        | crate::FlowError::NoIndex
        | crate::FlowError::Unreachable(_)
        | crate::FlowError::Stale => None,
        crate::FlowError::Vendor(_) | crate::FlowError::VendorUnreachable(_) => None,
        _ => Some(format!("error: {e}")),
    }
}

fn canonical_non_error_state(e: &crate::FlowError) -> Option<String> {
    match e {
        crate::FlowError::NoArtifact(t) => Some(crate::state::unavailable(t, "")),
        crate::FlowError::VendorUnavailable(state) => Some(state.clone()),
        crate::FlowError::StageRefused(row) => Some(row.clone()),
        _ => None,
    }
}

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

fn record_outcome(layout: &crate::store::Layout, outcome: String) {
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
            updated_at: now_rfc3339(),
            ..existing
        },
    );
}

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

fn recorded_root_of(layout: &crate::store::Layout, program: &str, build: u64) -> String {
    effective_tree_root(layout, program, Some(build), "")
}

fn legacy_root_of(layout: &crate::store::Layout, program: &str, build: u64) -> String {
    let recorded = recorded_root_of(layout, program, build);
    if !recorded.is_empty() {
        return recorded;
    }
    crate::vendor_direct::ProgramStamp::read(layout, program)
        .and_then(|s| s.legacy_root_of(build).map(str::to_string))
        .unwrap_or_default()
}

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

thread_local! {
    static OUTCOMES_WRITTEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn note_outcome_written() {
    OUTCOMES_WRITTEN.with(|n| n.set(n.get().wrapping_add(1)));
}

fn outcomes_written() -> u64 {
    OUTCOMES_WRITTEN.with(std::cell::Cell::get)
}

thread_local! {
    static SUCCESSES_STAMPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static PASS_INDEX_RESOLVE_ATTEMPTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static RESOLVED_PASS_INDEX_BUILD: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn successes_stamped() -> u64 {
    SUCCESSES_STAMPED.with(std::cell::Cell::get)
}

fn record_status(
    layout: &crate::store::Layout,
    program: &str,
    state: crate::ProgramStatus,
    outcome: String,
) {
    write_program_row(layout, program, state, Some(outcome));
}

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

fn record_index_error(layout: &crate::store::Layout, e: impl std::fmt::Display) {
    record_unbuilt(
        layout,
        "*index*",
        format!("error: {e}"),
        format!("update failed: {e}"),
    );
}

fn is_sentinel_row(name: &str) -> bool {
    name.starts_with('*') && name.ends_with('*')
}

fn write_program_row(
    layout: &crate::store::Layout,
    program: &str,
    state: crate::ProgramStatus,
    outcome: Option<String>,
) {
    write_program_rows(layout, [(program.to_string(), state)], outcome);
}

fn write_program_rows(
    layout: &crate::store::Layout,
    rows: impl IntoIterator<Item = (String, crate::ProgramStatus)>,
    outcome: Option<String>,
) {
    let Some(mut status) = status_seed_for_rewrite(layout) else {
        return;
    };
    status.programs.extend(rows);
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

fn record_pass_end(layout: &crate::store::Layout, now: i64, reached: bool) {
    if !reached {
        return;
    }
    SUCCESSES_STAMPED.with(|n| n.set(n.get().wrapping_add(1)));
    let _ = crate::status::stamp_success(layout, &rfc3339_at(now));
}

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
    static PASS_FAILED_ONLY_FOR_ITS_INDEX: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

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

/// `ATPKG_REGISTRY`.
pub(crate) const REGISTRY_SEAM: &str = "ATPKG_REGISTRY";

/// The [`REGISTRY_SEAM`] value this process runs under.
pub(crate) fn registry_seam() -> Option<std::ffi::OsString> {
    aterm_types::dev_seam!(REGISTRY_SEAM)
}

fn already_current_line(r: &crate::flow::InstallReport) -> String {
    format!(
        "atpkg: {} already current (build {}, as index {} pins it)",
        r.program, r.build, r.index_build
    )
}

/// The directory an `ATPKG_REGISTRY` value names with `dir:`, which [`resolve_fetcher`] serves through [`crate::DirFetcher`].
pub(crate) fn dir_registry(spec: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    spec?
        .to_string_lossy()
        .strip_prefix("dir:")
        .map(std::path::PathBuf::from)
}

fn resolve_fetcher(layout: &crate::store::Layout) -> Box<dyn crate::flow::Fetcher> {
    if let Some(dir) = dir_registry(registry_seam().as_deref()) {
        return Box::new(crate::DirFetcher::new(dir));
    }
    Box::new(github_fetcher(layout))
}

fn effective_anchor(layout: &crate::store::Layout) -> crate::Anchor {
    crate::Anchor::pinned(crate::sig::Floor::new(layout.roster_floor()).current())
}

fn build_floor(layout: &crate::store::Layout) -> crate::sig::BuildFloor {
    crate::sig::BuildFloor {
        index_build: crate::sig::Floor::new(layout.floor()).current(),
        roster_seq: crate::sig::Floor::new(layout.floor_generation()).current(),
    }
}

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

const fn manager_enabled() -> bool {
    crate::manager_enabled()
}

fn adopted(layout: &crate::store::Layout) -> bool {
    layout.adopted().is_file()
}

fn record_adoption(layout: &crate::store::Layout) {
    let path = layout.adopted();
    if path.is_file() {
        return;
    }
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

fn write_marker(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut f = crate::platform::open_create_write(path, 0o600)?;
    let _ = writeln!(f, "{body}");
    Ok(())
}

fn remove_marker(path: &std::path::Path) {
    if path.is_file() {
        let _ = std::fs::remove_file(path);
    }
}

const fn default_set_adopts(
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

fn clear_adoption(layout: &crate::store::Layout) {
    remove_marker(&layout.adopted());
    clear_seed_adoption(layout);
}

fn adopted_by_seed(layout: &crate::store::Layout) -> bool {
    adopted(layout) && layout.adopted_by_seed().is_file()
}

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

fn clear_seed_adoption(layout: &crate::store::Layout) {
    remove_marker(&layout.adopted_by_seed());
}

fn removed_programs(layout: &crate::store::Layout) -> std::collections::BTreeSet<String> {
    // One reader, on the layout: the unattended update lanes need the same answer.
    layout.removed_programs()
}

/// The programs this machine keeps complete.
pub(crate) fn wanted_programs(
    index: &crate::manifest::Index,
    cfg: &crate::config::PackagesConfig,
) -> std::collections::BTreeSet<String> {
    index.installable(cfg.exclude())
}

fn reconcile_system_satisfied_with(
    layout: &crate::store::Layout,
    index: &crate::manifest::Index,
    wanted: &std::collections::BTreeSet<String>,
    installed: &mut std::collections::BTreeMap<String, u64>,
    path_var: Option<&std::ffi::OsStr>,
) -> Vec<String> {
    let mut satisfied = Vec::new();
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

/// What the last verified index pins for this machine.
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

/// The last index this machine verified, from the cache.
pub(crate) fn cached_index(layout: &crate::store::Layout) -> Option<crate::TrustedIndex> {
    cached_index_for(layout, crate::config::cached())
}

fn last_verified_channel(layout: &crate::store::Layout) -> Option<crate::manifest::Channel> {
    cached_index(layout)?.channel_for(crate::config::CHANNEL, current_triple())
}

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
    if event.contains("could not") {
        eprintln!("atpkg: {program}: {state}{event}");
    } else {
        println!("atpkg: {program}: {state}{event}");
    }
    let outcome = crate::status::read(layout)
        .map(|s| s.outcome)
        .filter(|o| !o.is_empty())
        .unwrap_or_else(|| format!("{program} satisfied by system {}", path.display()));
    record_unbuilt(layout, program, state, outcome);
}

fn record_state_if_changed(layout: &crate::store::Layout, program: &str, state: String) -> bool {
    record_states_if_changed(layout, [(program, state)])
}

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

fn up_to_date_row_needs_rewrite(prior: Option<&str>, build: u64, index_build: u64) -> bool {
    match prior {
        None | Some("active") => true,
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

fn record_row_keeping_outcome(
    layout: &crate::store::Layout,
    program: &str,
    row: crate::ProgramStatus,
) {
    write_program_row(layout, program, row, None);
}

fn today_ymd() -> String {
    let stamp = now_rfc3339();
    stamp.get(..10).map(str::to_string).unwrap_or_default()
}

/// The fix-line for `program`'s managed shim when it exports an environment before it execs ([`crate::shim_env::ShimEnv::fix_line`]).
pub(crate) fn shim_env_fix(shim: &std::path::Path, program: &str) -> Option<String> {
    crate::platform::shim_env_of(shim).fix_line(program)
}

/// The fix-line for a shadowed `tool` of `program`.
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

/// The fix-line for `program`'s shadowed row where the shadow is the foreign copy at `shadow`.
#[must_use]
pub fn shadowed_fix(
    layout: &crate::store::Layout,
    program: &str,
    shadow: &std::path::Path,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<String> {
    let name = shadow.file_name()?.to_str()?;
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

fn reconcile_aliases(layout: &crate::store::Layout, index: &crate::manifest::Index) {
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
    let builds: Vec<(&std::path::Path, &[crate::store::ToolName])> = live
        .iter()
        .map(|(_, build_dir, tools)| (build_dir.as_path(), tools.as_slice()))
        .collect();
    crate::activate::reassert_shim_env(layout, &builds);
    for (program, build_dir, tools) in &live {
        let aliases = crate::activate::Aliases::for_program(program, index.program(program));
        let _ = crate::activate::reconcile_aliases(layout, build_dir, tools, aliases);
    }
    crate::activate::reconcile_agents(layout);
}

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

/// `group-aborted: <label> update aborted at <member> during <phase>: <cause>`.
pub const GROUP_ABORTED_MARKER: &str = "group-aborted: ";
/// The first line of a cause, bounded for a status row and a marker line.
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
pub const SEED_UNUSABLE_MARKER: &str = "seed-unusable: ";
pub const SEED_FAILED_MARKER: &str = "seed-failed: ";
/// The network completion lane's answer.
pub const NET_INSTALLED_MARKER: &str = "net-installed: ";
/// The network lane's announcement, printed before any bytes move.
pub const NET_STARTING_MARKER: &str = "net-starting: ";
/// The network lane's failure terminal.
pub const NET_FAILED_MARKER: &str = "net-failed: ";
/// The dispatch edge's "queued behind a sibling" announcement.
pub const LOCK_WAITING_MARKER: &str = "lock-waiting: ";
/// The dispatch edge's "the wait is over" line.
pub const LOCK_ACQUIRED_MARKER: &str = "lock-acquired: ";
/// The shadow reconcile's answer ([`reconcile_shadowed`]).
pub const SHADOWED_MARKER: &str = "shadowed: ";
/// The managed-current answer, printed late in a whole `update` pass (one an error stopped too), `update <agent>` and `install --default-set` when a managed agent is current; never by `seed`.
pub const MANAGED_CURRENT_MARKER: &str = "managed-current: ";

/// `machine-settings: universal-control disabled; rosetta installed`.
pub const MACHINE_SETTINGS_MARKER: &str = "machine-settings: ";

/// `foreign: <scope> <items>`.
pub const FOREIGN_MARKER: &str = "foreign: ";
/// The rustup seam's scope on the [`FOREIGN_MARKER`] line.
pub const FOREIGN_SEAM: &str = "seam";
/// The shadow reconcile's scope on the [`FOREIGN_MARKER`] line.
pub const FOREIGN_PATH: &str = "path";
/// What a scope that found nothing foreign says.
pub const FOREIGN_NONE: &str = "none";

/// `c-toolchain: missing`, `c-toolchain: broken` or `c-toolchain: ready`.
pub const C_TOOLCHAIN_MARKER: &str = "c-toolchain: ";
/// The [`C_TOOLCHAIN_MARKER`] body of a toolchain that is not installed.
pub const C_TOOLCHAIN_MISSING: &str = "missing";
/// The [`C_TOOLCHAIN_MARKER`] body of an installed compiler that cannot build, which an install fixes (off macOS only).
pub const C_TOOLCHAIN_BROKEN: &str = "broken";
/// The [`C_TOOLCHAIN_MARKER`] body of a toolchain an install would not change.
pub const C_TOOLCHAIN_READY: &str = "ready";

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

fn say_c_toolchain_at_seed(
    path: Option<&std::ffi::OsStr>,
    host: &crate::prereq::CcHost,
    toolset_present: bool,
) {
    if let Some(line) = c_toolchain_seed_marker(path, host, toolset_present) {
        emit_marker_line(&line);
    }
}

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
    /// Atpkg's fix sentence.
    pub(crate) fix: String,
}

/// The [`FOREIGN_MARKER`] line for `scope`'s look.
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

/// The `machine-state:` marker bare `atpkg machine` prints (the read counterpart of `machine-settings:`).
pub const MACHINE_STATE_MARKER: &str = "machine-state: ";

/// The clause a `seed-unusable:` line carries when the signed index serves no build for this machine's architecture.
pub const UNSERVED_ARCHITECTURE_CLAUSE: &str = "for this machine's architecture";
/// The clause of the `seed-unusable:` line that says every program the index serves here was removed on purpose (`Narrowing::verdict`).
pub const REMOVED_ON_PURPOSE_CLAUSE: &str = "was removed on this machine";

/// Whether a `seed-unusable:` body states a fact rather than a failure.
#[must_use]
pub fn seed_unusable_is_informational(body: &str) -> bool {
    body.contains(UNSERVED_ARCHITECTURE_CLAUSE) || body.contains(REMOVED_ON_PURPOSE_CLAUSE)
}

#[cfg(test)]
mod seed_unusable_tests {
    use super::{Narrowing, seed_unusable_is_informational};

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
        let (both, _) = Narrowing {
            excluded: names(&["ay"]),
            removed: names(&["trust"]),
        }
        .verdict("aarch64-apple-darwin");
        assert!(both.contains("excluded: ay; uninstalled: trust"), "{both}");
        assert!(!seed_unusable_is_informational(&both), "{both}");
    }
}

/// The prefix of every `atpkg machine` / `atpkg machine apply` verdict sentence, which a window worker keys on to quote the verdict.
pub const MACHINE_VERDICT_PREFIX: &str = "atpkg machine: ";

/// The line [`apply_machine_settings`] prints when it refuses to touch the machine (a home that is not this account's).
pub const MACHINE_NOT_APPLIED_PREFIX: &str = "atpkg: machine settings not applied — ";

/// A machine action ran but failed.
pub const MACHINE_APPLY_FAILED_PREFIX: &str = "atpkg: machine settings failed — ";

/// The `machine-settings:` line for `entries` (already non-empty).
#[must_use]
pub fn machine_settings_line(entries: &[String]) -> String {
    format!("{MACHINE_SETTINGS_MARKER}{}", entries.join("; "))
}

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

fn apply_machine_settings() -> MachineApply {
    let cfg = crate::config::cached_machine();
    let mut out = MachineApply::default();
    let entries = &mut out.entries;
    if parent_is_gone() {
        return out;
    }
    if !cfg!(target_os = "macos") {
        return out;
    }
    if let Some(reason) = machine_refusal(cfg) {
        println!("{MACHINE_NOT_APPLIED_PREFIX}{reason}");
        return out;
    }
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
    print_machine_state_after_apply(cfg, policy);
    if let Some(layout) = crate::store::resolve_configured() {
        if uc_failed || rosetta_failed {
            crate::machine::forget_applied(&layout);
        } else {
            crate::machine::record_applied(&layout, cfg);
        }
    }
    out
}

#[derive(Debug, Default)]
struct MachineApply {
    entries: Vec<String>,
    failed: bool,
    left_alone: Vec<&'static str>,
    acts: bool,
}

impl MachineApply {
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

fn machine_apply_queue() -> Option<crate::lock::Flock> {
    machine_apply_queue_in(&crate::store::resolve_configured()?)
}

fn machine_apply_queue_in(layout: &crate::store::Layout) -> Option<crate::lock::Flock> {
    crate::lock::Flock::lock(machine_lock_file(layout)?).ok()
}

fn machine_lock_file(layout: &crate::store::Layout) -> Option<std::fs::File> {
    layout.ensure_dir(&layout.prefix).ok()?;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(layout.prefix.join("machine.lock"))
        .ok()
}

enum MachineTurn {
    Mine(crate::lock::Flock),
    Busy,
    Unqueued,
}

fn machine_apply_turn() -> MachineTurn {
    crate::store::resolve_configured().map_or(MachineTurn::Unqueued, |layout| {
        machine_apply_turn_in(&layout)
    })
}

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

struct MachineEdit(Option<std::thread::JoinHandle<()>>);

impl MachineEdit {
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

fn verb_applies_machine_settings(verb: &str) -> bool {
    verb_row(verb).is_some_and(|v| v.applies_machine)
}

fn machine_table_changed() -> bool {
    crate::store::resolve_configured().is_some_and(|layout| {
        crate::machine::changed_since_applied(&layout, crate::config::cached_machine())
    })
}

fn machine_refusal(cfg: &crate::config::MachineConfig) -> Option<String> {
    machine_config_refusal(cfg).or_else(|| machine_home().1)
}

fn machine_config_refusal(cfg: &crate::config::MachineConfig) -> Option<String> {
    cfg.unreadable.then(|| {
        "aterm.toml does not parse, so the [machine] opt-outs could not be read — fix the \
         file (`aterm --window --validate-config`), then `aterm pkg machine apply`"
            .to_string()
    })
}

fn machine_home() -> (crate::machine::HomePosture, Option<String>) {
    let home = aterm_types::dirs::home_dir().filter(|h| !h.as_os_str().is_empty());
    let account = crate::platform::account_home();
    let same = match (&home, &account) {
        (Some(h), Some(a)) => same_directory(h, a),
        _ => false,
    };
    machine_home_verdict(home.as_deref(), account.as_deref(), same)
}

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
        Unused::Tooling { idle, after } => format!(
            "goes whole: a pinned tool nothing ran for {} (the pass takes one unused \
             {}; `targo --unverified verify provision` lays it again)",
            days(idle),
            days(after)
        ),
        Unused::GivenUp => "goes whole: a scratch a failed provisioning run left, past \
                            macOS's first-exec assessment of what ran from it"
            .to_owned(),
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

fn cmd_machine(rest: &[String]) -> ExitCode {
    match rest {
        [] => {
            if !cfg!(target_os = "macos") {
                println!("{MACHINE_VERDICT_PREFIX}not applicable — these are macOS host settings");
                return ExitCode::SUCCESS;
            }
            let cfg = crate::config::cached_machine();
            let policy = cfg.universal_control();
            let state = crate::machine::universal_control_state(&crate::machine::SystemDefaults);
            println!("{MACHINE_VERDICT_PREFIX}{}", state.state_line(policy));
            let (st, mismatch) = machine_state_now(cfg, policy);
            println!(
                "{MACHINE_VERDICT_PREFIX}{}",
                rosetta_state_line(st.rosetta, st.rosetta_wanted)
            );
            emit_marker_line(&format!(
                "atpkg: {MACHINE_STATE_MARKER}{}",
                crate::machine::machine_state_line(&st)
            ));
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
            if let Some(reason) = machine_refusal(crate::config::cached_machine()) {
                let _ = apply_machine_settings();
                println!("{MACHINE_VERDICT_PREFIX}not applied — {reason}");
                return ExitCode::SUCCESS;
            }
            let queue = machine_apply_queue();
            let applied = apply_machine_settings();
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
    /// The parenthesis: `<Vendor> latest` while a check that reached the vendor this run (and within a day) found it the head.
    pub(crate) standing: String,
}

/// The standing of a managed agent program whose vendor this run could not reach, or no check has reached for a day.
pub const NOT_CHECKED: &str = "not checked";

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

/// Whether `agents/<name>`, the copy every aterm tab runs, is laid and resolves exactly where `bin/<name>` does, into `store/<name>/<build>`.
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

/// [`agent_twin_current`] and the twin's bytes carry the self-update hand-over ([`twin_carries_hand_over`]).
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

fn twin_carries_hand_over(bytes: &[u8]) -> bool {
    bytes
        .windows(crate::selfupdate::HIDDEN_VERB.len())
        .any(|w| w == crate::selfupdate::HIDDEN_VERB.as_bytes())
}

fn pass_path_tail(foreign: &std::path::Path) -> String {
    let mut s = String::from("a foreign copy is ahead on this pass's PATH (");
    s.push_str(&foreign.display().to_string());
    s.push_str("); a tab opened on this build puts agents/ first on its own");
    s
}

/// The in-place remedy for a shell whose `PATH` lacks `agents/` (or has it behind a foreign copy).
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

/// What an agent program's reroute stub ([`crate::reroute::agents_stub_body_sh`]) does on one shell's `PATH` and environment.
pub(crate) enum AgentStub {
    /// No stub of ours answers first for `program` (or it is not an agent program).
    Absent,
    /// The stub answers first and execs the managed twin.
    Routes { stub: std::path::PathBuf },
    /// The stub answers first and passes through.
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

/// The foreign copy of `tool` that runs in a shell with `path_var` and `env`.
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

/// The agent programs' row for a shell outside aterm whose reroute stub answers first and passes through to `foreign`.
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

/// The agent programs' routed row for one shell.
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

/// The agent programs' shadow row for one shell.
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
            if !agent_twin_current(layout, name, build) {
                return None;
            }
            let checked = !unchecked.contains(*name)
                && crate::vendor_direct::ProgramStamp::read(layout, name)
                    .is_some_and(|s| s.checked_recently(now));
            let version = match crate::state::vendor_row(&row.state) {
                Some((version, _)) => version.to_string(),
                None if crate::vendor_direct::is_vendor_build(build) => {
                    crate::vendor_direct::build_label(build)
                }
                None => return None,
            };
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

fn print_managed_current(
    layout: &crate::store::Layout,
    unchecked: &std::collections::BTreeSet<String>,
    now: i64,
) {
    if let Some(line) = managed_current_line(&managed_current_entries(layout, unchecked, now)) {
        emit_marker_line(&format!("atpkg: {line}"));
    }
}

/// The positive terminal.
pub const SEED_DONE_MARKER: &str = "seed-done: ";

/// Another `atpkg` is already doing this work.
pub const SEED_BUSY_MARKER: &str = "seed-busy: ";

/// The announcement ledger.
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

    static PROCESS: Ledger = Ledger::new();

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
        static TEST_LEDGER: std::cell::Cell<Option<&'static Ledger>> =
            const { std::cell::Cell::new(None) };
    }

    /// Every marker in the stdout contract, and whether it opens an announcement rather than answering one.
    pub(crate) const MARKERS: &[(&str, bool)] = &[
        (super::NET_STARTING_MARKER, true),
        (super::SEED_UNUSABLE_MARKER, false),
        (super::SEED_FAILED_MARKER, false),
        (super::SEED_DONE_MARKER, false),
        (super::SEED_BUSY_MARKER, false),
        (super::NET_INSTALLED_MARKER, false),
        (super::NET_FAILED_MARKER, false),
    ];

    /// Record what a marker line just said.
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

    /// Tests only: give this thread an empty ledger of its own until the guard drops.
    #[cfg(all(test, unix))]
    pub(crate) fn own_ledger() -> OwnLedger {
        let own: &'static Ledger = Box::leak(Box::new(Ledger::new()));
        OwnLedger {
            previous: TEST_LEDGER.replace(Some(own)),
        }
    }

    /// Tests only: the guard [`own_ledger`] returns.
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
    static MARKERS_SAID: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(all(test, unix))]
fn take_markers_said() -> Vec<String> {
    MARKERS_SAID.with(|said| std::mem::take(&mut *said.borrow_mut()))
}

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

fn human_marker(line: &str) -> Option<String> {
    let Some(rest) = line.strip_prefix("atpkg: ") else {
        return Some(line.to_string());
    };
    let marked = |marker: &str| rest.strip_prefix(marker);
    let count = |body: &str| -> Option<u64> {
        let digits: String = body
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    };
    let size = |body: &str| -> String {
        body.strip_suffix(')')
            .and_then(|b| b.rsplit_once(" ("))
            .map(|(_, group)| format!(" ({group})"))
            .unwrap_or_default()
    };
    let programs = |n: u64| if n == 1 { "program" } else { "programs" };
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

fn answer_announcement(ok: bool) {
    if !announcement::is_open() {
        return;
    }
    if ok {
        let held = layout().map_or(0, |l| crate::active_builds(&l).len());
        if held == 0 {
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

const SEED_FOLLOW_ON: &str = "atpkg: these tools are on PATH in every aterm shell — \
                              aterm pkg list shows them; aterm <tool> runs one anywhere";

const fn should_complete_set(
    consent: bool,
    adopted: bool,
    asked: bool,
    enabled: bool,
    declined: bool,
    config_unreadable: bool,
) -> bool {
    consent && (adopted || asked) && enabled && !declined && !config_unreadable
}

fn declined(layout: &crate::store::Layout) -> bool {
    layout.declined().is_file()
}

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

fn clear_decline(layout: &crate::store::Layout) {
    remove_marker(&layout.declined());
}

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

fn do_install(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    channel: &str,
    program: &str,
) -> Result<crate::InstallReport, crate::FlowError> {
    do_install_with(layout, fetcher, channel, program, &PassSeams::process())
}

fn do_install_with(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    channel: &str,
    program: &str,
    seams: &PassSeams<'_>,
) -> Result<crate::InstallReport, crate::FlowError> {
    let installed = crate::active_builds(layout).get(program).copied();
    let floor = build_floor(layout);
    let req = crate::InstallRequest {
        channel,
        program,
        triple: current_triple(),
        installed,
    };
    crate::store::clear_stage_refusals(layout, program);
    if crate::vendor_direct::is_vendor(program) {
        let _ = crate::vendor_direct::ProgramStamp::clear_refusal(layout, program);
    }
    let mut resolved_assets = std::collections::BTreeMap::new();
    let typed = TypedPass::begin("install");
    let result = match crate::vendor_direct::spec(program)
        .filter(|_| !index_owns_now(layout, fetcher, program))
    {
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
        Ok(r) if r.vendor.is_some() => {}
        Ok(r) => {
            advance_floors(layout, r.index_build, r.roster_seq);
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
                    tree_root: effective_tree_root(layout, program, Some(r.build), &r.tree_root),
                },
                outcome,
            );
            if !r.already_current {
                let channel = last_verified_channel(layout);
                let _ = crate::gc::run_keeping_pinned_partials(layout, channel.as_ref(), &|p| {
                    resolved_assets.get(p).cloned()
                });
            }
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
            None => record_outcome(layout, format!("install {program}: {e}")),
        },
    }
    result
}

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

fn cmd_install_argv(rest: &[String]) -> ExitCode {
    match parse_install_argv(rest) {
        Ok(program) => cmd_install(program, "install"),
        Err(refusal) => refusal,
    }
}

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

fn cmd_install_for_update(program: &String) -> ExitCode {
    cmd_install(Some(program), "update")
}

fn cmd_install(program: Option<&String>, invoked_as: &str) -> ExitCode {
    let Some(program) = program else {
        eprintln!(
            "atpkg install: name a program, or `aterm pkg install --default-set` for the whole \
             ALab toolset"
        );
        return ExitCode::from(2);
    };
    if program == "--default-set" {
        return cmd_install_default_set();
    }
    if program.starts_with('-') {
        eprintln!("atpkg: {program:?} is a command-line flag, not a program name");
        eprintln!("atpkg:   one program:      aterm pkg install <program>");
        eprintln!("atpkg:   the whole toolset: aterm pkg install --default-set");
        return ExitCode::from(2);
    }
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
    clear_removed(&layout, std::slice::from_ref(program));
    cmd_install_with(
        program,
        &layout,
        cfg,
        &*resolve_fetcher(&layout),
        invoked_as,
    )
}

fn cmd_install_with(
    program: &str,
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    fetcher: &dyn crate::flow::Fetcher,
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
            reassert_rustup_seam(layout);
            let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
            ExitCode::SUCCESS
        }
        Err(crate::FlowError::Linked(p)) => {
            println!(
                "{}",
                dev_linked_line(&p, unlink_returns_to_alab(layout, &p))
            );
            ExitCode::SUCCESS
        }
        Err(e @ (crate::FlowError::NoArtifact(_) | crate::FlowError::VendorUnavailable(_))) => {
            let state = canonical_non_error_state(&e)
                .unwrap_or_else(|| crate::state::unavailable(current_triple(), ""));
            println!("atpkg: {program}: {state}");
            ExitCode::SUCCESS
        }
        Err(crate::FlowError::Vendor(line)) => {
            eprintln!("atpkg: {line}");
            ExitCode::from(1)
        }
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
            print_unreachable_followup(&e, &format!("aterm pkg {invoked_as} {program}"));
            ExitCode::from(1)
        }
    }
}

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
            advance_floors(layout, r.index_build, r.roster_seq);
            record_status(
                layout,
                program,
                crate::ProgramStatus {
                    installed_build: Some(r.to_build),
                    state: crate::state::rolled_back(r.to_build, r.from_build),
                    tree_root: String::new(),
                },
                format!("rolled back {program} {} -> {}", r.from_build, r.to_build),
            );
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

fn cmd_rollback_vendor(
    layout: &crate::store::Layout,
    spec: &'static crate::vendor_direct::VendorSpec,
) -> ExitCode {
    let program = spec.program;
    let policy = crate::vendor_direct::policy::Policy::current(layout, None);
    match crate::vendor_direct::lane::roll_back_by_hand(layout, &policy, spec) {
        Ok(moved) => {
            let build = moved.to_build;
            let tree_root =
                crate::vendor_direct::complete_record(&layout.build_dir(program, build))
                    .map_or_else(|| legacy_root_of(layout, program, build), |r| r.tree_root);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinPeers<'a> {
    Unknown,
    Alone,
    Group {
        group: &'a str,
        pinned: &'a [String],
    },
}

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

fn cmd_pin(program: Option<&String>, pinned: bool) -> ExitCode {
    let verb = if pinned { "pin" } else { "unpin" };
    let Some(program) = program else {
        return usage_error(verb, None);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
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
            let problem = format!("unknown option {arg:?} for `aterm pkg update`");
            return usage_error("update", Some(&problem));
        } else {
            operands.push(arg);
        }
    }
    if let Some(refusal) = arity_refusal(rest, "update") {
        return refusal;
    }
    let program = operands.first().copied();
    if let Some(choice) = choice {
        return cmd_update_source(program.map(String::as_str), choice);
    }
    if retry && let Some(refusal) = program.and_then(|p| refuse_retry_operand(p)) {
        return refusal;
    }
    if retry && let Some(layout) = layout() {
        let mut forgotten = match program {
            Some(p) => crate::store::clear_stage_refusals(&layout, p),
            None => crate::store::clear_all_stage_refusals(&layout),
        };
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

fn refuse_retry_operand(program: &str) -> Option<ExitCode> {
    crate::ops::uninstall_name_shape(program).err()?;
    let problem = format!("{program:?} is not a program name — nothing was forgotten or run");
    Some(usage_error("update", Some(&problem)))
}

fn forget_vendor_refusal(layout: &crate::store::Layout, program: &str) -> bool {
    crate::vendor_direct::is_vendor(program)
        && crate::vendor_direct::ProgramStamp::read(layout, program)
            .is_some_and(|stamp| stamp.refused.is_some())
        && crate::vendor_direct::ProgramStamp::clear_refusal(layout, program).is_ok()
}

fn retry_line(forgotten: usize) -> String {
    match forgotten {
        0 => String::from("atpkg: no download was being held back \u{2014} updating"),
        1 => String::from("atpkg: trying 1 held-back download again"),
        n => format!("atpkg: trying {n} held-back downloads again"),
    }
}

fn cmd_update(program: Option<&String>) -> ExitCode {
    match program {
        Some(p) => cmd_update_one(p),
        None => cmd_update_all(),
    }
}

fn cmd_update_all() -> ExitCode {
    let code = cmd_update_all_code();
    answer_announcement(code == 0);
    ExitCode::from(code)
}

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

fn update_pass_failed(layout: &crate::store::Layout, e: &crate::FlowError) -> u8 {
    eprintln!("atpkg: update failed: {e}");
    print_unreachable_followup(e, "aterm pkg update");
    record_index_error(layout, e);
    1
}

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

fn cmd_update_all_code() -> u8 {
    if !manager_enabled() {
        eprintln!("atpkg: disabled (no root key pinned) — nothing to update");
        return 1;
    }
    let Some(layout) = layout() else {
        return 1;
    };
    let _promise = PromiseEnds(&layout);
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
    reconcile_links(&layout, cfg);
    let Ok(dev_linked) = update_link_inventory(&layout) else {
        return 1;
    };
    let installed: std::collections::BTreeMap<String, u64> = crate::active_builds(&layout);
    let complete_the_set = completes_the_set(&layout, cfg);
    if !has_work(&installed, &dev_linked, complete_the_set, &layout) {
        println!("{EMPTY_UPDATE}");
        let _ = crate::status::stamp_pass_end(
            &layout,
            &now_rfc3339(),
            aterm_update_core::pkg_check::PassOutcome::Ok,
        );
        return 0;
    }
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

fn update_promises_its_plan(layout: &crate::store::Layout, plans: bool, now_unix: u64) -> bool {
    plans
        && (crate::adoption::first_install_ahead(layout)
            || crate::progress::pass_owed(layout, now_unix))
}

struct PromiseEnds<'a>(&'a crate::store::Layout);

impl Drop for PromiseEnds<'_> {
    fn drop(&mut self) {
        let mut holders = vec![std::process::id()];
        holders.extend(wait_lock().and_then(|w| w.spawner));
        crate::progress::retire_promise_of(self.0, &holders);
    }
}

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

struct ProbePackagesConfig {
    path: Option<std::path::PathBuf>,
    seen: Option<Option<(Option<std::time::SystemTime>, u64)>>,
    cfg: crate::config::PackagesConfig,
}

impl ProbePackagesConfig {
    const fn new(path: Option<std::path::PathBuf>, cfg: crate::config::PackagesConfig) -> Self {
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
                        self.seen = None;
                    }
                }
            }
        }
        &self.cfg
    }
}

/// [`has_work`] for the store as it stands, under the live `[packages]` table.
pub(crate) fn update_pass_has_work(layout: &crate::store::Layout) -> bool {
    static PROBE_CONFIG: std::sync::OnceLock<std::sync::Mutex<ProbePackagesConfig>> =
        std::sync::OnceLock::new();
    let cfg = PROBE_CONFIG.get_or_init(|| {
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

fn probe_has_work_lazily(
    complete_the_set: bool,
    managed: impl FnOnce() -> bool,
    tombstoned_vendor: impl FnOnce() -> bool,
    linked: impl FnOnce() -> bool,
) -> bool {
    complete_the_set || managed() || tombstoned_vendor() || linked()
}

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

fn attempted_pass_index_build(requested: u64, resolved: u64, attempted: bool) -> u64 {
    if attempted {
        requested.max(resolved)
    } else {
        0
    }
}

struct PassSeams<'a> {
    anchor: fn(&crate::store::Layout) -> crate::Anchor,
    now: fn() -> i64,
    trust: &'a crate::vendor_direct::lane::Trust,
    shell_hooks: fn(&crate::store::Layout),
    rustup_seam: &'a dyn Fn(&crate::store::Layout),
    progress: Option<&'a std::path::Path>,
    path: Option<std::ffi::OsString>,
    warm_agents: fn(&crate::store::Layout, &std::collections::BTreeSet<String>),
    translated: fn() -> bool,
    flip_policy: fn() -> crate::quiet::FlipPolicy,
    running: &'a dyn Fn() -> Option<Vec<std::path::PathBuf>>,
    vendor_read_grace: std::time::Duration,
    read_ahead_in: &'a (dyn Fn(&str) + Sync),
}

impl PassSeams<'static> {
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

const fn no_read_ahead_note(_: &str) {}

fn refresh_shell_hooks(layout: &crate::store::Layout) {
    relay_base(layout, &SHELL_RELAY);
}

const SHELL_RELAY: [&str; 3] = [
    crate::converge::repair::LINKS,
    crate::converge::repair::HOOKS,
    crate::converge::repair::RC,
];

const SEED_RELAY: [&str; 6] = [
    crate::converge::repair::LINKS,
    crate::converge::repair::HOOKS,
    crate::converge::repair::RC,
    crate::converge::repair::STUBS,
    crate::converge::repair::HELD_UPDATE,
    crate::converge::repair::SHIMS,
];

const SEED_RELAY_AFTER_SEAM: [&str; 1] = [crate::converge::repair::EXEC_ROOTS];

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

#[cfg(unix)]
fn relay_not_own_account_with(euid: u32) -> Option<&'static str> {
    (euid == 0).then_some("it runs as root")
}

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

fn vendor_tombstoned_in_place(layout: &crate::store::Layout) -> bool {
    crate::flow::tombstoned_in_place(
        layout,
        crate::vendor_direct::VENDORS.iter().map(|s| s.program),
    )
}

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
    let offline = matches!(
        e,
        crate::FlowError::Unreachable(_) | crate::FlowError::Stale
    ) && pass_reached_nothing(index_answered, vendor);
    finish_update_pass(layout, (seams.now)(), false, offline, code)
}

const fn pass_reached_nothing(index_answered: bool, vendor: &VendorPass) -> bool {
    !index_answered && !vendor.reached
}

fn run_update_pass(
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    fetcher: &dyn crate::flow::Fetcher,
    installed: &std::collections::BTreeMap<String, u64>,
    complete_the_set: bool,
    seams: &PassSeams<'_>,
) -> u8 {
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
    let mut index_resolved = false;
    let live_before = crate::flow::live_verifications();
    let mut resolved_index_build: Option<u64> = None;
    let mut resolved_assets: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut pass_index: Option<crate::TrustedIndex> = None;
    let mut c_toolchain_said = false;
    let index_asked = !installed.is_empty() || complete_the_set;
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
    if let Some(Ok(index)) = resolved.as_ref() {
        RESOLVED_PASS_INDEX_BUILD.with(|seen| seen.set(seen.get().max(index.index_build)));
    }
    let provenance = resolved.as_ref().and(crate::flow::last_resolve());
    let mut index_answered = resolved.is_none() || provenance.as_ref().is_some_and(|r| r.answered);
    let faults_before = crate::flow::fetch_faults();
    let flip = crate::quiet::FlipGate {
        policy: (seams.flip_policy)(),
        now: (seams.now)(),
        running: seams.running,
    };
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
    let consent = set_completion_consent(adopted(layout), adopted_by_seed(layout));
    let typed = TypedPass::begin("update");
    let first_run = complete_the_set && installed.is_empty() && matches!(resolved, Some(Ok(_)));
    let set_index = match &resolved {
        Some(Ok(index)) if first_run => Some(index),
        _ => None,
    };
    let (mut vendor, mut fresh) = std::thread::scope(|beside| {
        if let (Some(index), Some(shared)) = (set_index, fetcher.shared()) {
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
        if let Some(index) = set_index {
            advance_floors(layout, index.index_build, index.roster_seq());
        }
        plan
    });
    let late = std::mem::take(&mut fresh.late);
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
        advance_floors(layout, report.index_build, report.roster_seq);
        resolved_index_build = Some(report.index_build);
        resolved_assets.extend(report.resolved_assets.clone());
        clear_status_row(layout, "*index*");
        recover_missing_roots(layout, fetcher, Some(&index));
        record_outcome(
            layout,
            vendor_folded_outcome(
                &vendor,
                format!("up to date (index build {})", report.index_build),
            ),
        );
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
        if !complete_the_set {
            reconcile_aliases(layout, &index);
            reconcile_shadowed(layout, report.index_build, seams.path.as_deref());
        }
        pass_index = Some(index);
    }
    if pass_index.is_none()
        && let Some(Ok(index)) = resolved
    {
        pass_index = Some(index);
    }
    if complete_the_set {
        let before_net = crate::active_builds(layout);
        let toolchain_flipped = |layout: &crate::store::Layout| {
            (seams.rustup_seam)(layout);
            let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
        };
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
        let mut arrived: Vec<String> = crate::active_builds(layout)
            .keys()
            .filter(|k| !before_net.contains_key(k.as_str()))
            .cloned()
            .collect();
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
        if let Some(retry) = net.resolve_provenance.as_ref() {
            index_answered |= retry.answered;
        }
        if let Some(build) = net.resolved_index_build {
            RESOLVED_PASS_INDEX_BUILD.with(|seen| seen.set(seen.get().max(build)));
        }
        index_resolved = settle_net_lane_index(layout, index_resolved, net.index_error.as_deref());
        let announced = net.announced;
        resolved_assets.extend(net.resolved_assets);
        if !arrived.is_empty() {
            arrived.sort();
            emit_marker_line(&format!(
                "atpkg: {NET_INSTALLED_MARKER}{}",
                arrived.join(", ")
            ));
            println!("{SEED_FOLLOW_ON}");
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
        if net.failures == 0 {
            retire_seed_row(layout);
        }
    }
    let channel = pass_index
        .as_ref()
        .and_then(|index| index.channel_for(crate::config::CHANNEL, current_triple()));
    let report = crate::gc::run_keeping_pinned_partials(layout, channel.as_ref(), &|p| {
        resolved_assets.get(p).cloned()
    });
    print_gc_sweeps("update", &report);
    print_gc_abstentions("update", &report);
    (seams.rustup_seam)(layout);
    let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
    (seams.shell_hooks)(layout);
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
    print_managed_current(layout, &vendor.unchecked, (seams.now)());
    let now = (seams.now)();
    if index_resolved {
        record_index_freshness(layout, now, resolved_index_build);
    }
    let offline = pass_reached_nothing(index_answered, &vendor)
        && crate::flow::fetch_faults().wrapping_sub(faults_before) == failures;
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
            record_unbuilt(layout, "*toolset*", crate::state::TOOLSET_TRANSLATED, said);
            let reached = crate::flow::live_verifications() > live_before;
            return finish_update_pass(layout, now, reached, offline, 2);
        }
        let serves_us = match reusable_index(pass_index.as_ref(), now) {
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
        let (said, state, unusable) = match &serves_us {
            Some(IndexServes::Yes) => (
                format!(
                    "nothing is installed and nothing was installable — ALab \
                         publishes builds for {} but every one was refused (revoked, \
                         below the floor, or filtered out); see `aterm pkg doctor`",
                    current_triple()
                ),
                "unavailable: every published build was refused",
                None,
            ),
            Some(IndexServes::Unserved(served)) => {
                let why = unserved_sentence(current_triple(), served);
                (
                    format!("{NOTHING_INSTALLABLE_LEAD}{why}"),
                    crate::state::TOOLSET_UNSERVED,
                    Some(why),
                )
            }
            Some(IndexServes::Narrowed(narrowing)) => {
                let (why, state) = narrowing.verdict(current_triple());
                (format!("{NOTHING_INSTALLABLE_LEAD}{why}"), state, Some(why))
            }
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
        if let Some(why) = unusable
            && !stdout_is_a_person()
        {
            emit_marker_line(&seed_unusable_line(&why));
        }
        record_unbuilt(layout, "*toolset*", state, said);
        let reached = crate::flow::live_verifications() > live_before;
        return finish_update_pass(layout, now, reached, offline, 2);
    }
    let reached = crate::flow::live_verifications() > live_before;
    let code = u8::from(failures > 0);
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
    finish_update_pass(layout, now, reached, offline, code)
}

struct DefaultSetOutcome {
    failures: u32,
    announced: bool,
    announced_count: usize,
    resolve_provenance: Option<crate::flow::ResolveProvenance>,
    index_error: Option<String>,
    resolved_index_build: Option<u64>,
    resolved_assets: std::collections::BTreeMap<String, String>,
    skipped_narrowed: Vec<String>,
    remaining_missing: Vec<String>,
}

const NET_PASS: &str = "net";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Consent {
    InstallingAterm,
    Explicit,
}

impl Consent {
    const fn announced(self) -> (&'static str, &'static str) {
        match self {
            Consent::InstallingAterm => (
                "installing aterm is the consent for this; ",
                ", or with `[packages].auto_install = false` in aterm.toml",
            ),
            Consent::Explicit => ("", ""),
        }
    }
}

const fn set_completion_consent(adopted: bool, adopted_by_seed: bool) -> Consent {
    if adopted && !adopted_by_seed {
        Consent::Explicit
    } else {
        Consent::InstallingAterm
    }
}

const NET_OFF_SWITCH: &str = "opt out once this pass ends with `aterm pkg uninstall --all`, which \
                              removes the set and records the decline";

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
    let owned_pass = pass_progress_path().is_some_and(|p| crate::progress::begin_pass(p, NET_PASS));
    let out = install_default_set_inner(
        layout, fetcher, anchor, cfg, consent, now, index, path_var, flip, held_flip, companions,
    );
    if owned_pass {
        crate::progress::end_pass();
    }
    out
}

struct SetCompanions<'c, 'r> {
    vendor: Option<&'c mut VendorRider<'r>>,
    toolchain_flipped: &'c dyn Fn(&crate::store::Layout),
    vendors_after: bool,
}

#[cfg(all(test, unix))]
impl SetCompanions<'_, '_> {
    fn alone() -> Self {
        Self {
            vendor: None,
            toolchain_flipped: &|_| {},
            vendors_after: false,
        }
    }
}

fn stub_commands_of(index: &crate::TrustedIndex, exposes: &[String]) -> Vec<String> {
    exposes
        .iter()
        .filter(|name| !index.is_program(name) && crate::reroute::row_for(name).is_none())
        .cloned()
        .collect()
}

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
    advance_floors(layout, index.index_build, index.roster_seq());
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
    let Some(ch) = index.channel_for(crate::config::CHANNEL, current_triple()) else {
        let e = crate::FlowError::NoChannel(crate::config::CHANNEL.to_string());
        eprintln!("atpkg: default-set bootstrap failed: {e}");
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
    for spec in crate::vendor_direct::VENDORS {
        if spec.serves(current_triple())
            && vendor_wanted(cfg, &std::collections::BTreeSet::new(), spec.program)
        {
            wanted.insert(spec.program.to_string());
        }
    }
    for p in removed_programs(layout) {
        wanted.remove(&p);
    }
    for p in reconcile_system_satisfied_with(layout, index, &wanted, &mut installed, path_var) {
        wanted.remove(&p);
    }
    let triple = current_triple();
    let linked_members: std::collections::BTreeSet<&str> = groups
        .iter()
        .flat_map(|group| group.members.iter())
        .filter(|m| crate::linkmode::is_linked(layout, m))
        .map(String::as_str)
        .collect();
    let mut unserved: Vec<String> = Vec::new();
    let prescan = prescan_units(&groups, &wanted, &installed, &linked_members);
    crate::flow::prefetch_pinned(
        fetcher,
        index,
        ch,
        prescan
            .iter()
            .flat_map(|(group, missing)| prescan_probe(group, missing))
            .map(String::as_str),
    );
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
        let recorded = crate::status::read(layout);
        for p in &unserved {
            wanted.remove(p.as_str());
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
    crate::stub::reconcile(layout, &wanted, &installed, &exposed);
    let flip_held = |group: &crate::Group| group.members.iter().any(|m| held_flip.contains(m));
    let mut will_install: Vec<String> = Vec::new();
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
    let sizes = if will_install.is_empty() {
        SignedSizes::UNKNOWN
    } else {
        seed_install_bytes(fetcher, index, &will_install)
    };
    let rider = rider.filter(|r| !r.fresh.pending.is_empty());
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
    let plan: Vec<(usize, Vec<String>)> = groups
        .iter()
        .enumerate()
        .filter(|(_, group)| disk_refusal.is_none() && !flip_held(group))
        .filter_map(|(i, group)| {
            let missing = absent_members(group, &wanted, &installed);
            (!missing.is_empty()).then_some((i, missing))
        })
        .collect();
    let plannable: std::collections::BTreeSet<String> = plan
        .iter()
        .flat_map(|(_, missing)| missing.iter().cloned())
        .collect();
    let vendor_plan = rider.as_ref().map_or_else(Vec::new, |r| r.fresh.planned());
    let vendor_names: Vec<String> = vendor_plan.iter().map(|(name, _)| name.clone()).collect();
    let shared = rider.as_ref().and_then(|_| fetcher.shared());
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
        if left_behind {
            sink.plan(&planned);
        } else {
            sink.plan_promised_pass(layout, &planned);
        }
    }
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
    let mut resolved_assets: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut pinned_members: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for group in &groups {
        pinned_members.extend(group.members.iter().cloned());
    }
    let mut remaining: Vec<(usize, Vec<String>)> = plan;
    let gate = RiderGate::default();
    let queue = rider.as_deref().map(RiderQueue::of);
    let mut landed = rider.as_deref().map(|_| RiderLanded::default());
    let rider_now = rider.as_deref().map_or(0, |r| r.fresh.now);
    let (landed_tx, landed_rx) = std::sync::mpsc::channel::<RiderLanding>();
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
        drop(landed_tx);
        if shared.is_some()
            && beside.is_empty()
            && let Some(sink) = crate::progress::active()
        {
            sink.queue(&set_progress_queue(
                &remaining,
                &groups,
                &vendor_names,
                false,
            ));
        }
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
    for (program, asset) in ahead.iter().flat_map(|lane| lane.untaken()) {
        resolved_assets.entry(program).or_insert(asset);
    }
    let now_active = crate::active_builds(layout);
    crate::stub::reconcile(layout, &wanted, &now_active, &exposed);
    lay_reroute_stubs(layout);
    reconcile_aliases(layout, index);
    let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
    reconcile_shadowed(layout, index.index_build, path_var);
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
    let remaining_missing: Vec<String> = wanted
        .iter()
        .filter(|p| !now_active.contains_key(p.as_str()))
        .cloned()
        .collect();
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

fn prescan_probe<'a>(group: &'a crate::Group, missing: &'a [String]) -> &'a [String] {
    match &group.group {
        Some(_) => &group.members,
        None => missing,
    }
}

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
            for m in missing {
                let state = crate::state::TOMBSTONED_PIN.to_string();
                record_state_if_changed(layout, m, state.clone());
                note_finished(m, crate::progress::Phase::Skipped, Some(state));
            }
            0
        }
        Ok((crate::TxnOutcome::Aborted { failed, why, .. }, _)) => {
            let cause = clip_cause(&why);
            eprintln!(
                "atpkg: could not install {} ({failed}: {cause}) \u{2014} nothing changed",
                missing.join(", ")
            );
            record_status(
                layout,
                &failed,
                failure_row(
                    layout,
                    &failed,
                    format!("error: coherence group '{g}' bootstrap aborted — {cause}"),
                ),
                format!("bootstrap group '{g}' aborted at {failed} — {cause}"),
            );
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
        Err(crate::FlowError::Linked(p)) => {
            println!("atpkg: {p} dev-linked — skipped");
            note_finished(program, crate::progress::Phase::Skipped, None);
            0
        }
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
            let state = crate::state::TOMBSTONED_PIN.to_string();
            record_state_if_changed(layout, program, state.clone());
            note_finished(program, crate::progress::Phase::Skipped, Some(state));
            0
        }
        Err(crate::FlowError::StageRefused(row)) => skip_held_member(layout, program, row),
        Err(e) => {
            eprintln!("atpkg: bootstrap install {program} failed: {e} (continuing)");
            record_bootstrap_error(layout, program, &e);
            note_finished(program, crate::progress::Phase::Failed, Some(e.to_string()));
            1
        }
    }
}

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

fn record_bootstrap_failure(layout: &crate::store::Layout, program: &str, why: &str) {
    record_status(
        layout,
        program,
        failure_row(layout, program, format!("error: {why}")),
        format!("bootstrap install {program}: {why}"),
    );
}

fn cmd_install_default_set() -> ExitCode {
    let code = cmd_install_default_set_code();
    answer_announcement(code == 0);
    ExitCode::from(code)
}

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

fn install_toolset(
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    fetcher: &dyn crate::flow::Fetcher,
    seams: &PassSeams<'_>,
) -> u8 {
    reconcile_links(layout, cfg);
    clear_decline(layout);
    let all_removed: Vec<String> = removed_programs(layout).into_iter().collect();
    clear_removed(layout, &all_removed);
    clear_seed_adoption(layout);
    let before = crate::active_builds(layout);
    let live_before = crate::flow::live_verifications();
    let resolved = crate::resolve_verified_index(
        fetcher,
        layout,
        &(seams.anchor)(layout),
        build_floor(layout),
        (seams.now)(),
    )
    .ok();
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
    let toolchain_flipped = |layout: &crate::store::Layout| {
        (seams.rustup_seam)(layout);
        let _ = reconcile_exec_roots(layout, crate::seam::Depth::Shallow);
    };
    let mut failures_outcome = install_default_set_with_path(
        layout,
        fetcher,
        &(seams.anchor)(layout),
        cfg,
        Consent::Explicit,
        (seams.now)(),
        resolved.as_ref(),
        seams.path.as_deref(),
        &crate::quiet::FlipGate::NOW,
        &std::collections::BTreeSet::new(),
        SetCompanions {
            vendor: rider.as_mut(),
            toolchain_flipped: &toolchain_flipped,
            vendors_after: false,
        },
    );
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
    let channel = resolved
        .as_ref()
        .and_then(|index| index.channel_for(crate::config::CHANNEL, current_triple()));
    let report = crate::gc::run_keeping_pinned_partials(layout, channel.as_ref(), &|p| {
        failures_outcome.resolved_assets.get(p).cloned()
    });
    print_gc_sweeps("install --default-set", &report);
    print_gc_abstentions("install --default-set", &report);
    (seams.rustup_seam)(layout);
    (seams.shell_hooks)(layout);
    let now = (seams.now)();
    record_pass_end(layout, now, crate::flow::live_verifications() > live_before);
    if failures > 0 {
        return 1;
    }
    retire_seed_row(layout);
    if !vendor_missing.is_empty() {
        eprintln!(
            "atpkg: default set incomplete — {} not installed yet (the line above says why); \
             `aterm pkg update` retries",
            vendor_missing.join(", ")
        );
        return 1;
    }
    let skipped = &failures_outcome.skipped_narrowed;
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
             for this machine ({}); retrying will not help until one is published.",
            current_triple()
        );
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

fn prereq_line(verdict: &crate::prereq::CcVerdict) -> Option<String> {
    verdict
        .blocks_builds()
        .then(|| format!("atpkg: {}", verdict.line()))
}

fn cmd_seed(rest: &[String]) -> ExitCode {
    if let Some(refusal) = zero_arity(rest, "seed") {
        return refusal;
    }
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    retire_seed_row(&layout);
    let cfg = crate::config::cached();
    match crate::adoption::adoption(&layout, cfg) {
        crate::Adoption::Consented => {}
        crate::Adoption::Declined => {
            println!(
                "atpkg: the ALab toolset was removed on this machine — updates no longer \
                 install it; `aterm pkg install --default-set` puts it back"
            );
            return ExitCode::SUCCESS;
        }
        crate::Adoption::IgnoredPrefix => return ExitCode::SUCCESS,
        crate::Adoption::AutoInstallOff => {
            println!("atpkg: [packages].auto_install = false — not installing the ALab toolset");
            return ExitCode::SUCCESS;
        }
        crate::Adoption::ManagerDisabled => {
            println!("atpkg: disabled (no root key pinned) — not installing the ALab toolset");
            return ExitCode::SUCCESS;
        }
    }
    record_seed_adoption(&layout);
    if let Some(owner) = seed_promise_owner()
        && crate::adoption::first_install_ahead(&layout)
    {
        crate::progress::promise_pass(&layout, owner, crate::progress::SEED_PROMISE_SECS);
    }
    crate::stub::lay_adoption_stubs(&layout, cfg);
    relay_base(&layout, &SEED_RELAY);
    reassert_rustup_seam(&layout);
    relay_base(&layout, &SEED_RELAY_AFTER_SEAM);
    say_c_toolchain_at_seed(
        std::env::var_os("PATH").as_deref(),
        &crate::prereq::CcHost::this_machine(),
        !crate::active_builds(&layout).is_empty(),
    );
    report_seedless_posture(&layout);
    ExitCode::SUCCESS
}

fn seed_serviceable(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    cfg: &crate::config::PackagesConfig,
) -> Vec<String> {
    serviceable_among(
        layout,
        fetcher,
        index,
        &wanted_programs(index, cfg),
        &removed_programs(layout),
    )
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum IndexServes {
    Yes,
    Unserved(Vec<String>),
    Narrowed(Narrowing),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Narrowing {
    excluded: Vec<String>,
    removed: Vec<String>,
}

impl Narrowing {
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
    if narrowing.excluded.is_empty() && narrowing.removed.is_empty() {
        return IndexServes::Yes;
    }
    IndexServes::Narrowed(narrowing)
}

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

fn builds_for_clause(served: &[String]) -> String {
    if served.is_empty() {
        String::new()
    } else {
        format!("it builds for {}; ", served.join(", "))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SignedSizes {
    download: Option<u64>,
    disk: Option<u64>,
}

impl SignedSizes {
    const UNKNOWN: Self = Self {
        download: None,
        disk: None,
    };

    fn and(self, other: Self) -> Self {
        let sum = |a: Option<u64>, b: Option<u64>| a.zip(b).map(|(a, b)| a.saturating_add(b));
        Self {
            download: sum(self.download, other.download),
            disk: sum(self.disk, other.disk),
        }
    }
}

fn seed_install_bytes(
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::TrustedIndex,
    members: &[String],
) -> SignedSizes {
    let triple = current_triple();
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SetUnit {
    group: Option<String>,
    members: Vec<String>,
    bytes: crate::flow::LandingBytes,
}

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

#[derive(Debug)]
struct SetDiskRefusal {
    line: String,
    rows: Vec<(String, String)>,
}

impl SetDiskRefusal {
    fn record(&self, layout: &crate::store::Layout) {
        for (m, why) in &self.rows {
            record_bootstrap_failure(layout, m, why);
        }
    }
}

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

fn sum_nonzero(total: Option<u64>, n: u64) -> Option<u64> {
    total.filter(|_| n > 0).map(|t| t.saturating_add(n))
}

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

fn report_seedless_posture(layout: &crate::store::Layout) {
    let cfg = crate::config::cached();
    report_seedless_posture_as(layout, cfg, wait_lock().is_some(), now_unix(), || {
        cached_index_for(layout, cfg)
    });
}

fn report_seedless_posture_as(
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    window_pass: bool,
    now: i64,
    read_cached_index: impl FnOnce() -> Option<crate::TrustedIndex>,
) {
    let triple = current_triple();
    let served_here = !crate::active_builds(layout).is_empty()
        || vendor_set_wanted_on(triple, cfg, &removed_programs(layout));
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
    match serves_us {
        Some(IndexServes::Unserved(served)) => {
            let why = unserved_sentence(triple, &served);
            emit_marker_line(&seed_unusable_line(&why));
            record_unusable_toolset(layout, crate::state::TOOLSET_UNSERVED, &why);
        }
        Some(IndexServes::Narrowed(narrowing)) => {
            let (why, state) = narrowing.verdict(triple);
            emit_marker_line(&seed_unusable_line(&why));
            record_unusable_toolset(layout, state, &why);
        }
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

fn seed_unusable_line(body: &str) -> String {
    format!("atpkg: {SEED_UNUSABLE_MARKER}{body}")
}

const NOTHING_INSTALLABLE_LEAD: &str = "nothing is installed and nothing was installable \u{2014} ";

fn record_unusable_toolset(layout: &crate::store::Layout, state: &str, why: &str) {
    record_unbuilt(
        layout,
        "*toolset*",
        state,
        format!("{NOTHING_INSTALLABLE_LEAD}{why}"),
    );
}

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

fn unserved_sentence(triple: &str, served: &[String]) -> String {
    format!(
        "ALab publishes no build {UNSERVED_ARCHITECTURE_CLAUSE} ({triple}) \u{2014} {}nothing \
         installs here until one is published",
        builds_for_clause(served)
    )
}

const fn says_not_installed_yet(store_empty: bool, window_pass: bool) -> bool {
    store_empty && !window_pass
}

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

fn reconcile_links(layout: &crate::store::Layout, cfg: &crate::config::PackagesConfig) {
    let home = aterm_types::dirs::home_dir();
    for (program, value) in &cfg.links {
        match crate::classify_link(value, home.as_deref()) {
            crate::LinkTarget::Checkout(want) => {
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
            crate::LinkTarget::Repo(_) => {}
            crate::LinkTarget::Invalid => {
                eprintln!(
                    "atpkg: [packages.links] {program} = {value:?} is neither an absolute/~ \
                     checkout path nor a valid owner/repo — ignored (fail-closed)"
                );
            }
        }
    }
}

fn link_bins_for(program: &str, checkout: &std::path::Path) -> Vec<std::path::PathBuf> {
    crate::alab::package_link_bin(program, checkout)
        .map_or_else(|| default_link_bins(program), |entry| vec![entry])
}

fn default_link_bins(program: &str) -> Vec<std::path::PathBuf> {
    vec![std::path::PathBuf::from(format!(
        "target/release/{program}{}",
        std::env::consts::EXE_SUFFIX
    ))]
}

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
    let _typed = TypedPass::begin("update");
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
        let report = match crate::flow::apply_program(
            &*fetcher,
            &layout,
            &effective_anchor(&layout),
            crate::config::CHANNEL,
            current_triple(),
            program,
            &installed,
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
        let gc = crate::gc::run_keeping_pinned_partials(&layout, Some(&plan.channel), &|p| {
            report.resolved_assets.get(p).cloned()
        });
        print_gc_sweeps("update", &gc);
        print_gc_abstentions("update", &gc);
        reassert_rustup_seam(&layout);
        let _ = reconcile_exec_roots(&layout, crate::seam::Depth::Shallow);
        refresh_shell_hooks(&layout);
        return if failures == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        };
    }
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
        eprintln!(
            "atpkg: {program} is pinned, but ALab withdrew its current build — updating off \
             it anyway"
        );
    }
    let before = crate::active_builds(&layout);
    let code = cmd_install_with(program, &layout, cfg, &*fetcher, "update");
    warm_alab_landed(&layout, &before, crate::warm::spawn_landed);
    code
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VendorDoor {
    Person,
    Window,
}

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
        crate::store::clear_stage_refusals(layout, program);
        let _ = crate::vendor_direct::ProgramStamp::clear_refusal(layout, program);
    }
    let now = (seams.now)();
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
    let unchecked = matches!(
        outcome.verdict,
        crate::vendor_direct::lane::Verdict::Unreachable { .. }
            | crate::vendor_direct::lane::Verdict::Deferred { .. }
    );
    !(outcome.is_failure() || (unchecked && person))
}

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

fn note_landed(pass: &mut VendorPass, outcome: &crate::vendor_direct::lane::Outcome) {
    if matches!(
        outcome.verdict,
        crate::vendor_direct::lane::Verdict::Installed { .. }
    ) {
        pass.landed.insert(outcome.spec.program.to_string());
    }
}

fn warm_landed(layout: &crate::store::Layout, pass: &mut VendorPass, seams: &PassSeams<'_>) {
    let landed = std::mem::take(&mut pass.landed);
    if !landed.is_empty() {
        (seams.warm_agents)(layout, &landed);
    }
}

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

fn failed_programs_outcome(names: &[String]) -> String {
    format!(
        "{} program(s) failed: {} — see their rows",
        names.len(),
        names.join(", ")
    )
}

#[derive(Clone, Copy)]
struct VendorLanes {
    update_installed: bool,
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

fn vendor_set_wanted_on(
    triple: &str,
    cfg: &crate::config::PackagesConfig,
    removed: &std::collections::BTreeSet<String>,
) -> bool {
    crate::vendor_direct::VENDORS
        .iter()
        .any(|spec| spec.serves(triple) && vendor_wanted(cfg, removed, spec.program))
}

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

#[derive(Clone, Copy)]
struct ReadsAhead<'r> {
    reads: &'r crate::vendor_direct::lane::Reads,
    until: Option<std::time::Instant>,
}

const VENDOR_READ_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

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
    let runs: Vec<(&'static crate::vendor_direct::VendorSpec, bool, bool)> =
        crate::vendor_direct::VENDORS
            .iter()
            .filter_map(|spec| {
                let program = spec.program;
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

struct FreshVendors {
    pending: Vec<crate::vendor_direct::lane::Pending>,
    policy: crate::vendor_direct::policy::Policy,
    index: Option<crate::TrustedIndex>,
    now: i64,
    late: Vec<(&'static crate::vendor_direct::VendorSpec, bool)>,
}

impl FreshVendors {
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

    fn names(&self) -> Vec<String> {
        self.pending
            .iter()
            .map(|p| p.program().to_string())
            .collect()
    }

    fn planned(&self) -> Vec<(String, u64)> {
        self.pending
            .iter()
            .map(|p| (p.program().to_string(), p.download_bytes()))
            .collect()
    }

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

    fn peak(&self) -> u64 {
        crate::flow::peak_need_at_once(
            self.pending
                .iter()
                .map(crate::vendor_direct::lane::Pending::landing_bytes),
            RIDER_LANES,
        )
    }
}

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

struct VendorRider<'r> {
    fresh: FreshVendors,
    trust: &'r crate::vendor_direct::lane::Trust,
    warm: fn(&crate::store::Layout, &std::collections::BTreeSet<String>),
    landed: Option<RiderLanded>,
}

struct RiderLanding {
    outcome: crate::vendor_direct::lane::Outcome,
    warmed: bool,
}

#[derive(Default)]
struct RiderLanded {
    pass: VendorPass,
    deferred: Vec<&'static str>,
}

impl RiderLanded {
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

const RIDER_BUMP_TICK: std::time::Duration = std::time::Duration::from_millis(100);

const RIDER_LANES: usize = 2;

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

    fn is_open(&self) -> bool {
        self.open.lock().map_or(true, |open| *open)
    }

    fn wait(&self, tick: std::time::Duration) {
        if let Ok(open) = self.open.lock() {
            let _ = self.changed.wait_timeout_while(open, tick, |open| !*open);
        }
    }
}

struct RiderGateOpens<'g>(&'g RiderGate);

impl Drop for RiderGateOpens<'_> {
    fn drop(&mut self) {
        self.0.open();
    }
}

struct RiderQueue<'q> {
    left: std::sync::Mutex<Vec<&'q crate::vendor_direct::lane::Pending>>,
    turns: crate::flow::DownloadTurns,
}

impl<'q> RiderQueue<'q> {
    fn of(rider: &'q VendorRider<'_>) -> Self {
        Self {
            left: std::sync::Mutex::new(rider.fresh.pending.iter().collect()),
            turns: crate::flow::DownloadTurns::default(),
        }
    }

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

const SIGNATURE_WAIT_CLAUSE: &str = "signature check to finish";

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
    let prior = crate::status::read(layout).and_then(|s| s.programs.get(program).cloned());
    keep_replaced_legacy_root(layout, program, prior.as_ref());
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

fn unreached_since(vendor: &str, checked_at: i64) -> String {
    let stamp = u64::try_from(checked_at)
        .map(aterm_types::rfc3339::format_rfc3339)
        .unwrap_or_default();
    let date = stamp.get(..10).unwrap_or(&stamp);
    format!("{vendor}'s release channel not reached since {date}")
}

fn report_channel_apply(
    layout: &crate::store::Layout,
    installed: &std::collections::BTreeMap<String, u64>,
    report: &crate::ChannelApplyReport,
    also_failed: &[String],
    whole_set: bool,
) -> u32 {
    let post: std::collections::BTreeMap<String, u64> = crate::active_builds(layout);
    let before = crate::status::read(layout)
        .map(|status| status.programs)
        .unwrap_or_default();
    let was_held = |prog: &str| {
        before
            .get(prog)
            .is_some_and(|row| row.state.starts_with(crate::state::HELD_PREFIX))
    };
    let mut failures = 0u32;
    let mut failed_names: Vec<String> = Vec::new();
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

fn admit_for_verify(
    args: &[String],
    verb: &str,
) -> Result<(Vec<u8>, Vec<u8>, crate::TrustedRoster), ExitCode> {
    let [master, doc, sig, roster, roster_sig] = args else {
        return Err(usage_error(verb, None));
    };
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
        eprintln!("FAIL: the roster did not verify under that master, or is stale");
        return Err(ExitCode::from(1));
    };
    Ok((raw, sig_bytes, admitted))
}

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
        Err(why) => {
            match index_post_verify_refusal(&why) {
                Some(reason) => eprintln!("FAIL: {reason}"),
                None => eprintln!("FAIL: no machine on that roster signed this index"),
            }
            ExitCode::from(1)
        }
    }
}

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
        _ => None,
    }
}

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
        Err(other) => Some(format!("refused post-verify: {other:?}")),
    }
}

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

fn cmd_unlink(program: Option<&String>) -> ExitCode {
    let Some(program) = program else {
        return usage_error("unlink", None);
    };
    let Some(layout) = layout() else {
        return ExitCode::from(1);
    };
    let owned = crate::linked_tool_names(&layout, program);
    match crate::unlink(&layout, program) {
        Ok(()) => {
            println!("atpkg: unlinked {program} (any pin is preserved)");
            match restore_installed_shims(&layout, program, owned.as_deref()) {
                Ok(Restored::NoBuild) => println!(
                    "atpkg:   no installed build to restore — {program} is off PATH until \
                     `aterm pkg install {program}`"
                ),
                Ok(Restored::Nothing) => {}
                Ok(Restored::Laid(n)) => {
                    println!("atpkg:   restored {n} command(s) from the installed build");
                }
                Err(e) => eprintln!(
                    "atpkg:   WARNING: could not restore the installed build's shims ({e}); \
                     run `aterm pkg install {program}` to put it back on PATH"
                ),
            }
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

#[derive(Debug, PartialEq, Eq)]
enum Restored {
    NoBuild,
    Nothing,
    Laid(usize),
}

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
    let Some(owned) = owned else {
        return Ok(Restored::Nothing);
    };
    let mut tools = Vec::new();
    for name in owned {
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
    let aliases = alias_policy_offline(layout, program);
    let env = crate::shim_env::read_sidecar(&build_dir);
    crate::activate::install_tools_env(layout, &build_dir, &tools, aliases, &env)?;
    Ok(Restored::Laid(tools.len()))
}

fn alias_policy_offline(layout: &crate::store::Layout, program: &str) -> crate::activate::Aliases {
    match cached_index(layout) {
        Some(index) => crate::activate::Aliases::for_program(program, index.program(program)),
        None => crate::activate::Aliases::laid_for(layout, program),
    }
}

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

#[cfg(test)]
#[path = "cli_held_row_tests.rs"]
mod held_row_tests;

#[cfg(all(test, unix))]
#[path = "cli_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "cli_alab_tests.rs"]
mod alab_tests;
