// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `harness_try` — poke the harness core by hand, with your own input.
//!
//! Two PURE readings: `usage` (what do Claude Code's own status-line numbers
//! say?, `aterm_agent::harness::usage`) and `limit` (which wall does this
//! notice name?, the one wall classifier `aterm_phase::classify_wall`, which
//! the supervisor acts on). Their tests prove them
//! against fixtures; this example proves nothing at all — it is a window, so a
//! person can type a real command and read the real verdict rather than take a
//! test count on trust.
//!
//! Nothing here is wired to Claude Code: no hook is answered, no key is typed,
//! no file of yours is read, and nothing is written. (The `ring` subcommand
//! went with the ledger ring on 2026-09-23, and `rm` with the hook-era
//! `rm_policy` on 2026-09-25.)
//!
//! ```text
//! targo --unverified run -p aterm-agent --example harness_try -- usage <<'J'
//! {"rate_limits":{"five_hour":{"used_percentage":62,"resets_at":1789669500}}}
//! J
//! targo --unverified run -p aterm-agent --example harness_try -- limit "You've reached your Fable limit"
//! ```

use std::io::Read;

use aterm_agent::harness::usage::{
    AccountView, UsageView, hud_line, parse_statusline, windows_from_statusline,
};

const USAGE: &str = "\
harness_try — read the harness core's real verdicts, by hand

  usage [file]           a statusLine JSON (file, or stdin) → the HUD line
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
    let json = match args.first() {
        Some(p) => std::fs::read_to_string(p).unwrap_or_else(|e| {
            println!("could not read {p}: {e}");
            String::new()
        }),
        None => {
            let mut s = String::new();
            let _ = std::io::stdin().read_to_string(&mut s);
            s
        }
    };
    if json.trim().is_empty() {
        println!("give a statusLine JSON on stdin or as a file path");
        return;
    }
    let line = match parse_statusline(&json) {
        Ok(l) => l,
        Err(e) => {
            println!("this is not a statusLine payload the reader accepts: {e:?}");
            println!(
                "(that is the point: a shape it does not recognise is refused, not guessed at)"
            );
            return;
        }
    };
    let windows = windows_from_statusline(&line, 12);
    let mut acct = AccountView::new("work", true);
    acct.model = line.model_id().map(str::to_string);
    acct.windows = windows;
    let mut view = UsageView::new(0);
    view.accounts.push(acct);

    println!("model     {}", line.model_id().unwrap_or("(not reported)"));
    println!("windows read from this payload:");
    let acct = &view.accounts[0];
    if acct.windows.is_empty() {
        println!("  (none — this build reported no rate_limits block)");
    }
    for (name, w) in &acct.windows {
        let pct = w
            .used_pct
            .map(|p| format!("{p:.0}%"))
            .unwrap_or_else(|| "?".into());
        let resets = w
            .resets_at
            .map(|r| format!("resets_at {r}"))
            .unwrap_or_else(|| "no reset reported".into());
        println!(
            "  {name:<16} {pct:<5} {resets:<24} source={} age={}s",
            w.source.as_str(),
            w.age_s.unwrap_or(0)
        );
    }
    println!();
    println!("HUD line  {}", hud_line(&view, 0));
    println!();
    println!("Windows are per ACCOUNT. Per-MODEL figures are spend, read from the transcript,");
    println!("because no per-model window is readable here (design §5.2).");
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
