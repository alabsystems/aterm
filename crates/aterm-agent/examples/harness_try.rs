// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `harness_try` — poke the harness core by hand, with your own input.
//!
//! Two PURE readings: `usage` (what does a Claude Code transcript say the
//! session spent, and on which model?, `aterm_agent::harness::usage`) and
//! `limit` (which wall does this notice name?, the one wall classifier
//! `aterm_phase::classify_wall`, which the supervisor acts on). Their tests
//! prove them against fixtures; this example proves nothing at all — it is a
//! window, so a person can hand it a real transcript or a real notice and read
//! the real verdict rather than take a test count on trust.
//!
//! Nothing here is wired to Claude Code: no hook is answered, no key is typed,
//! only the file you name is read, and nothing is written. (The `ring`
//! subcommand went with the ledger ring on 2026-09-23, `rm` with the hook-era
//! `rm_policy` on 2026-09-25, and the statusLine reading with its reader on
//! 2026-09-27.)
//!
//! ```text
//! targo --unverified run -p aterm-agent --example harness_try -- usage ~/.claude/projects/<slug>/<uuid>.jsonl
//! targo --unverified run -p aterm-agent --example harness_try -- limit "You've reached your Fable limit"
//! ```

use std::io::BufReader;

use aterm_agent::harness::usage::{AccountView, PriceTable, TranscriptUsage, UsageView, hud_line};

const USAGE: &str = "\
harness_try — read the harness core's real verdicts, by hand

  usage [file]           a Claude Code transcript (file, or stdin) → spend per model and the HUD line
  limit <notice text>    a vendor notice → the wall it names

Nothing is wired to Claude Code. No hook is answered and no key is typed.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = args.first().map(String::as_str) else {
        print!("{USAGE}");
        return;
    };
    match verb {
        "usage" => cmd_usage(&args[1..]),
        "limit" => cmd_limit(&args[1..]),
        _ => print!("{USAGE}"),
    }
}

fn cmd_usage(args: &[String]) {
    let mut fold = TranscriptUsage::new();
    let read = match args.first() {
        Some(p) => std::fs::File::open(p).and_then(|f| fold.fold_reader(BufReader::new(f))),
        None => fold.fold_reader(std::io::stdin().lock()),
    };
    if let Err(e) = read {
        println!("could not read the transcript: {e}");
        return;
    }
    let mut acct = AccountView::new("work", true);
    acct.add_transcript(&fold, &PriceTable::new());
    println!(
        "rows      {} ({} assistant, {} unparsed, {} without usage)",
        fold.rows, fold.assistant_rows, fold.rows_unparsed, fold.rows_without_usage
    );
    println!(
        "model     {}",
        acct.model.as_deref().unwrap_or("(none named)")
    );
    println!("spend by model:");
    if acct.spend.is_empty() {
        println!("  (none — no assistant row carried usage)");
    }
    for (model, s) in &acct.spend {
        println!(
            "  {model:<28} in={} out={} cache_write={} cache_read={}",
            s.input, s.output, s.cache_write, s.cache_read
        );
    }
    let mut view = UsageView::new(0);
    view.accounts.push(acct);
    println!();
    println!("HUD line  {}", hud_line(&view));
    println!();
    println!("Windows are per ACCOUNT, and no reader here fills them (`harness limits`");
    println!("reads the painted /usage panel's); per-MODEL figures are spend (design §5.2).");
}

fn cmd_limit(args: &[String]) {
    let text = args.join(" ");
    if text.trim().is_empty() {
        // The example notice is the anchor table's, not a second copy of the
        // vendor's words (tools/grep_guard.sh V1).
        println!(
            "give a notice, e.g.  limit \"{}\"",
            aterm_phase::anchor("wall.fable")
        );
        return;
    }
    println!("notice    {text}");
    match aterm_phase::classify_wall(&text) {
        None => println!("wall      none: not a notice the supervisor stops for"),
        Some(kind) => println!("wall      {}", kind.name()),
    }
}
