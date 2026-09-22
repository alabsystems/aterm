// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `harness_try` — poke the harness core by hand, with your own input.
//!
//! The harness core (`aterm_agent::harness`) is four PURE judgments:
//! `rm_policy` (would this `rm` be approved?), `usage` (what do Claude Code's
//! own status-line numbers say?), `limits` (what kind of failure is this, and
//! what happens next?) and `ring` (the bounded ledger). Their tests prove them
//! against fixtures; this example proves nothing at all — it is a window, so a
//! person can type a real command and read the real verdict rather than take a
//! test count on trust.
//!
//! Nothing here is wired to Claude Code: no hook is answered, no key is typed,
//! no file of yours is read, and `rm` never runs. The `ring` subcommand is the
//! one that writes, and only under a temporary directory it names.
//!
//! ```text
//! targo --unverified run -p aterm-agent --example harness_try -- rm 'rm -rf target/debug'
//! targo --unverified run -p aterm-agent --example harness_try -- rm 'rm -rf /' /Users//you/proj
//! targo --unverified run -p aterm-agent --example harness_try -- usage <<'J'
//! {"rate_limits":{"five_hour":{"used_percentage":62,"resets_at":1789669500}}}
//! J
//! targo --unverified run -p aterm-agent --example harness_try -- limit "You've reached your Fable limit"
//! targo --unverified run -p aterm-agent --example harness_try -- ring
//! ```

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use aterm_agent::harness::limits::{Class, Evidence, WindowKind, classify};
use aterm_agent::harness::ring::{Ring, RingConfig};
use aterm_agent::harness::rm_policy::{HookEvent, RmPolicy, evaluate};
use aterm_agent::harness::source::Source;
use aterm_agent::harness::usage::{
    AccountView, UsageView, hud_line, parse_statusline, windows_from_statusline,
};

const USAGE: &str = "\
harness_try — read the harness core's real verdicts, by hand

  rm <command> [cwd]     would this rm be auto-approved? (default cwd: this repo)
  usage [file]           a statusLine JSON (file, or stdin) → the HUD line
  limit <banner text>    a vendor banner → the failure class and what follows
  ring [dir]             append rows to a bounded ledger and read them back

Nothing is wired to Claude Code. No hook is answered, no key is typed, no rm runs.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = args.first().map(String::as_str) else {
        print!("{USAGE}");
        return;
    };
    match verb {
        "rm" => cmd_rm(&args[1..]),
        "usage" => cmd_usage(&args[1..]),
        "limit" => cmd_limit(&args[1..]),
        "ring" => cmd_ring(&args[1..]),
        _ => print!("{USAGE}"),
    }
}

fn cmd_rm(args: &[String]) {
    let Some(cmd) = args.first() else {
        println!("give a command, e.g.  rm 'rm -rf target/debug'");
        return;
    };
    let cwd: PathBuf = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    let policy = RmPolicy::default();

    println!("command   {cmd}");
    println!("cwd       {}", cwd.display());
    println!("policy    mode={:?}  (the shipped default)", policy.mode);
    println!();
    for event in [HookEvent::PermissionRequest, HookEvent::PreToolUse] {
        let v = evaluate(cmd, &cwd, event, &policy);
        let headline = match v.decision.as_str() {
            "allow" => "ALLOW    the harness answers yes on the vendor's channel",
            _ => "ABSTAIN  the harness says nothing; Claude Code's own prompt shows",
        };
        println!("{event:?}");
        println!("  {headline}");
        println!("  reason   {}", v.reason);
        if v.targets.is_empty() {
            println!("  targets  (none resolved before the rule decided)");
        } else {
            for t in &v.targets {
                println!("  target   {t}");
            }
        }
        println!();
    }
    println!("The harness never DENIES. Abstaining is how it declines to answer.");
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
        println!("give a banner, e.g.  limit \"You've reached your Fable limit\"");
        return;
    }
    let now = 1_789_669_000_i64;

    println!("banner    {text}");
    println!();

    // The banner alone.
    let only_banner = vec![Evidence::Banner {
        text: text.clone(),
        resets_at: None,
    }];
    report("banner alone", &only_banner, now);

    // The banner corroborated by an exhausted five-hour window.
    let paired = vec![
        Evidence::Banner {
            text,
            resets_at: None,
        },
        Evidence::Window {
            which: WindowKind::FiveHour,
            used_pct: Some(100.0),
            resets_at: Some(now + 2_700),
            source: Source::StatusLine,
            age_s: Some(8),
        },
    ];
    report("banner + an exhausted 5h window (statusline)", &paired, now);

    println!("A banner is untrusted screen text: on its own it may only DISPLAY.");
    println!("Two independent sources are required before anything acts (design §5.8.2).");
}

fn report(label: &str, evidence: &[Evidence], now: i64) {
    println!("{label}");
    match classify(evidence, now) {
        None => println!("  (no class: the evidence names nothing this classifier acts on)"),
        Some(c) => {
            println!("  class        {}", c.class.as_str());
            println!(
                "  may act?     {}",
                if c.unpaired {
                    "the reversible actions only — one source, so nothing that spends"
                } else {
                    "yes, including the spending actions — an independent pair"
                }
            );
            if let Some(r) = c.resets_at {
                println!("  resets_at    {r}");
            }
            for r in &c.reasons {
                println!("  because      {r}");
            }
            let note = match c.class {
                Class::ModelBucketLimit => {
                    "the vendor owns this one: its own dialog and switch; the harness escalates"
                }
                Class::Auth => {
                    "re-login: aterm opens the VENDOR's browser flow, a human finishes it"
                }
                Class::SpendBilling => "never automated: money is the owner's call",
                Class::Session5hLimit | Class::Weekly7dLimit => {
                    "switch account if enabled, else switch model, else wait for the reset"
                }
                Class::TransientCapacity => {
                    "let the vendor retry first; act only if the storm lasts"
                }
                Class::NetworkOffline => "no switch helps: wait, and freeze the stall watchdog",
                Class::Unknown => "fail closed: escalate, never retry",
            };
            println!("  what follows {note}");
        }
    }
    println!();
}

fn cmd_ring(args: &[String]) {
    let dir: PathBuf = args
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("aterm-harness-try"));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        println!("could not make {}: {e}", dir.display());
        return;
    }
    let dir = match dir.canonicalize() {
        Ok(d) => d,
        Err(e) => {
            println!("could not resolve {}: {e}", dir.display());
            return;
        }
    };
    let cfg = RingConfig::default();
    println!("dir       {}", dir.display());
    println!(
        "bound     {} bytes over {} segments",
        cfg.max_bytes, cfg.segments
    );
    println!();
    let mut ring = match Ring::open(Path::new(&dir), "try", cfg) {
        Ok(r) => r,
        Err(e) => {
            println!("open refused: {e}");
            println!("(a second writer on one ledger is refused on purpose)");
            return;
        }
    };
    for n in 1..=3 {
        match ring.append(&format!("{{\"demo\":true,\"n\":{n}}}")) {
            Ok(id) => println!("appended  id={id}"),
            Err(e) => println!("refused   {e}"),
        }
    }
    match ring.append("a line with\na newline in it") {
        Ok(id) => println!("appended  id={id}  (unexpected)"),
        Err(e) => println!("refused   a row containing a newline: {e}"),
    }
    println!();
    match ring.read_since(0, 10) {
        Ok(rows) => {
            println!("read back {} row(s):", rows.len());
            for r in rows {
                println!("  {:>4}  {}", r.id, r.line);
            }
        }
        Err(e) => println!("read failed: {e}"),
    }
    println!();
    println!(
        "bytes={} next_id={} floor_id={}",
        ring.bytes(),
        ring.next_id(),
        ring.floor_id()
    );
    println!(
        "A cursor below floor_id means rows were dropped — the ledger says so rather than lying."
    );
    let _ = BTreeMap::<u8, u8>::new();
}
